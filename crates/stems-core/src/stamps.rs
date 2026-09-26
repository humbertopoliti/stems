//! Script stamps (plan 16): a content hash that decides whether an
//! idempotent script (`setup`, `seed`, …) must run again.
//!
//! A stamp is `sha256` over, in this order and length-prefixed so that no
//! two different inputs can collide by concatenation:
//!
//! 1. a version tag (`stems-stamp-v1`),
//! 2. the script text (inline command, or the file's contents),
//! 3. for every input file matched by the script's `inputs:` globs, in
//!    byte-wise sorted order of its `/`-separated path relative to the base
//!    directory: that path and the sha256 of the file's contents,
//! 4. every `(name, value)` of the `stamp_env` variables, sorted by name.
//!
//! File metadata (mtime, permissions, …) is never hashed: touching a file
//! does not re-run a script; editing it does.
//!
//! Glob semantics (via `globset`): patterns are relative to `base_dir`
//! (a leading `./` is ignored); `*`/`?` do not cross `/`, `**` does; a
//! pattern also matches everything beneath it when it names a directory
//! (`src` ≡ `src` + `src/**`). The walk never descends into `.git`,
//! `node_modules`, `target` or `.venv` directories and does not follow
//! symlinked directories (symlinked files are hashed by their target's
//! contents).

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Directory names never walked when matching inputs.
pub const IGNORED_DIRS: [&str; 4] = [".git", "node_modules", "target", ".venv"];

/// Version tag mixed into every hash; bump it to invalidate all stamps.
const STAMP_VERSION: &str = "stems-stamp-v1";

/// The recorded fingerprint of one script run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// Lowercase hex sha256 (64 chars).
    pub hash: String,
    /// When the stamp was computed.
    pub computed_at: DateTime<Utc>,
    /// Matched input files, relative to the base directory, sorted.
    pub inputs: Vec<PathBuf>,
}

impl Stamp {
    /// Whether two stamps fingerprint the same inputs (ignores `computed_at`).
    pub fn same_as(&self, other: &Stamp) -> bool {
        self.hash == other.hash
    }
}

/// Compute a script's stamp.
///
/// - `script_text`: the inline command or the script file's contents.
/// - `inputs_globs`: the script's `inputs:` (relative to `base_dir`). When
///   empty, `base_dir` is not read at all.
/// - `base_dir`: the directory globs are relative to (the codebase).
/// - `env`: the `stamp_env` variables and their values. Pass only the names
///   listed in `stamp_env`; how an unset variable is represented (omitted vs
///   empty) is up to the caller, but it must be consistent.
///
/// Errors: an invalid glob (`InvalidInput`), or any I/O error while walking
/// `base_dir` or reading a matched file.
pub fn compute_stamp(
    script_text: &str,
    inputs_globs: &[String],
    base_dir: &Path,
    env: &BTreeMap<String, String>,
) -> io::Result<Stamp> {
    let inputs = if inputs_globs.is_empty() {
        Vec::new()
    } else {
        let set = build_globset(inputs_globs)?;
        let mut found = Vec::new();
        walk(base_dir, base_dir, &set, &mut found)?;
        found.sort();
        found
    };

    let mut h = Sha256::new();
    put(&mut h, STAMP_VERSION.as_bytes());
    put(&mut h, b"script");
    put(&mut h, script_text.as_bytes());
    for rel in &inputs {
        put(&mut h, b"input");
        put(&mut h, rel.as_bytes());
        put(&mut h, hash_file(&base_dir.join(rel))?.as_bytes());
    }
    for (k, v) in env {
        put(&mut h, b"env");
        put(&mut h, k.as_bytes());
        put(&mut h, v.as_bytes());
    }
    Ok(Stamp {
        hash: hex(&h.finalize()),
        computed_at: Utc::now(),
        inputs: inputs.into_iter().map(PathBuf::from).collect(),
    })
}

/// Feed one length-prefixed field.
fn put(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// Lowercase hex sha256 of a file's contents (streamed).
pub fn hash_file(path: &Path) -> io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn build_globset(globs: &[String]) -> io::Result<GlobSet> {
    let mut b = GlobSetBuilder::new();
    for g in globs {
        let g = g.trim();
        let g = g.strip_prefix("./").unwrap_or(g).trim_end_matches('/');
        if g.is_empty() {
            continue;
        }
        for pat in [g.to_string(), format!("{g}/**")] {
            let glob = GlobBuilder::new(&pat)
                .literal_separator(true)
                .build()
                .map_err(|e| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("invalid glob `{g}`: {e}"),
                    )
                })?;
            b.add(glob);
        }
    }
    b.build()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))
}

/// Collect `/`-separated relative paths of matching files under `dir`.
fn walk(root: &Path, dir: &Path, set: &GlobSet, out: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = entry.file_type()?;
        if ft.is_dir() {
            let name = entry.file_name();
            if IGNORED_DIRS.iter().any(|d| name == *d) {
                continue;
            }
            walk(root, &path, set, out)?;
            continue;
        }
        let is_file =
            ft.is_file() || (ft.is_symlink() && path.metadata().is_ok_and(|m| m.is_file()));
        if !is_file {
            continue;
        }
        let rel = path.strip_prefix(root).unwrap_or(&path);
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if set.is_match(&rel) {
            out.push(rel);
        }
    }
    Ok(())
}

/// Recorded stamps: `stem → script → stamp`, persisted in daemon state as
/// `stamps.<stem>.<script>`. Serialises as that plain nested map.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StampStore(pub BTreeMap<String, BTreeMap<String, Stamp>>);

impl StampStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `script` of `stem` must run given its freshly computed
    /// stamp: true when nothing is recorded or the hash differs.
    pub fn needs_run(&self, stem: &str, script: &str, current: &Stamp) -> bool {
        self.get(stem, script)
            .is_none_or(|recorded| !recorded.same_as(current))
    }

    /// The recorded stamp, if any.
    pub fn get(&self, stem: &str, script: &str) -> Option<&Stamp> {
        self.0.get(stem).and_then(|m| m.get(script))
    }

    /// Record a stamp after a successful run (replacing any previous one).
    pub fn record(&mut self, stem: &str, script: &str, stamp: Stamp) {
        self.0
            .entry(stem.to_string())
            .or_default()
            .insert(script.to_string(), stamp);
    }

    /// Forget one script's stamp. Returns it if it existed.
    pub fn clear_script(&mut self, stem: &str, script: &str) -> Option<Stamp> {
        let m = self.0.get_mut(stem)?;
        let s = m.remove(script);
        if m.is_empty() {
            self.0.remove(stem);
        }
        s
    }

    /// Forget every stamp of a stem (`stems reset <stem>`).
    pub fn clear_stem(&mut self, stem: &str) {
        self.0.remove(stem);
    }

    /// Forget everything (`up --fresh` / `stems reset`).
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Stamps of one stem, by script name.
    pub fn for_stem(&self, stem: &str) -> Option<&BTreeMap<String, Stamp>> {
        self.0.get(stem)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn globs(g: &[&str]) -> Vec<String> {
        g.iter().map(|s| s.to_string()).collect()
    }

    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "VERSION", "1.0.0\n");
        write(d.path(), "package.json", "{}");
        write(d.path(), "src/main.rs", "fn main() {}");
        write(d.path(), "src/lib/util.rs", "pub fn f() {}");
        write(d.path(), "src/readme.md", "# hi");
        write(d.path(), "node_modules/dep/index.js", "x");
        write(d.path(), "target/debug/out.rs", "y");
        write(d.path(), ".git/config", "z");
        write(d.path(), ".venv/lib/site.py", "v");
        write(d.path(), "sub/node_modules/x.rs", "nested ignored");
        d
    }

    #[test]
    fn hash_is_hex_sha256_and_deterministic() {
        let d = repo();
        let g = globs(&["VERSION", "src/**/*.rs"]);
        let a = compute_stamp("echo hi", &g, d.path(), &BTreeMap::new()).unwrap();
        let b = compute_stamp("echo hi", &g, d.path(), &BTreeMap::new()).unwrap();
        assert_eq!(a.hash.len(), 64);
        assert!(
            a.hash
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(a.hash, b.hash);
        assert!(a.same_as(&b));
    }

    #[test]
    fn empty_stamp_is_stable() {
        // Golden: guards against accidental changes to the hashing scheme.
        let s = compute_stamp("", &[], Path::new("/nonexistent"), &BTreeMap::new()).unwrap();
        insta::assert_snapshot!(s.hash, @"7890cc4773c35ee3913b9c1118d640e48f2203ed6a24430c2049e32ce3f72913");
        assert!(s.inputs.is_empty());
    }

    #[test]
    fn matched_inputs_sorted_and_ignored_dirs_skipped() {
        let d = repo();
        let s = compute_stamp(
            "x",
            &globs(&["**/*.rs", "VERSION", "**/*.js", "**/*.py", "**/config"]),
            d.path(),
            &BTreeMap::new(),
        )
        .unwrap();
        let got: Vec<_> = s
            .inputs
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, ["VERSION", "src/lib/util.rs", "src/main.rs"]);
    }

    #[test]
    fn glob_semantics() {
        let d = repo();
        let names = |g: &[&str]| -> Vec<String> {
            compute_stamp("x", &globs(g), d.path(), &BTreeMap::new())
                .unwrap()
                .inputs
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        };
        // `*` does not cross `/`.
        assert_eq!(names(&["src/*.rs"]), ["src/main.rs"]);
        // A directory pattern matches everything beneath it.
        assert_eq!(
            names(&["src"]),
            ["src/lib/util.rs", "src/main.rs", "src/readme.md"]
        );
        assert_eq!(names(&["./src/lib/"]), ["src/lib/util.rs"]);
        // Top-level only.
        assert_eq!(names(&["*.json"]), ["package.json"]);
        // No match is fine.
        assert!(names(&["nothing/**"]).is_empty());
    }

    #[test]
    fn invalid_glob_is_invalid_input() {
        let d = repo();
        let e = compute_stamp("x", &globs(&["src/[a"]), d.path(), &BTreeMap::new()).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn missing_base_dir_errors_only_with_globs() {
        let missing = Path::new("/definitely/not/here");
        assert!(compute_stamp("x", &[], missing, &BTreeMap::new()).is_ok());
        assert_eq!(
            compute_stamp("x", &globs(&["*"]), missing, &BTreeMap::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn sensitive_to_script_text() {
        let d = repo();
        let g = globs(&["VERSION"]);
        let a = compute_stamp("echo a", &g, d.path(), &BTreeMap::new()).unwrap();
        let b = compute_stamp("echo b", &g, d.path(), &BTreeMap::new()).unwrap();
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn content_change_changes_hash() {
        let d = repo();
        let g = globs(&["VERSION"]);
        let a = compute_stamp("s", &g, d.path(), &BTreeMap::new()).unwrap();
        write(d.path(), "VERSION", "1.0.1\n");
        let b = compute_stamp("s", &g, d.path(), &BTreeMap::new()).unwrap();
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn mtime_only_change_does_not_change_hash() {
        let d = repo();
        let g = globs(&["VERSION", "src/**"]);
        let a = compute_stamp("s", &g, d.path(), &BTreeMap::new()).unwrap();
        let f = fs::OpenOptions::new()
            .write(true)
            .open(d.path().join("VERSION"))
            .unwrap();
        f.set_modified(SystemTime::now() + Duration::from_secs(3600))
            .unwrap();
        drop(f);
        // Rewrite identical content too.
        write(d.path(), "src/main.rs", "fn main() {}");
        let b = compute_stamp("s", &g, d.path(), &BTreeMap::new()).unwrap();
        assert_eq!(a.hash, b.hash);
    }

    #[test]
    fn adding_or_renaming_an_input_changes_hash() {
        let d = repo();
        let g = globs(&["src/**"]);
        let a = compute_stamp("s", &g, d.path(), &BTreeMap::new()).unwrap();
        write(d.path(), "src/new.rs", "");
        let b = compute_stamp("s", &g, d.path(), &BTreeMap::new()).unwrap();
        assert_ne!(a.hash, b.hash);
        fs::rename(d.path().join("src/new.rs"), d.path().join("src/renamed.rs")).unwrap();
        let c = compute_stamp("s", &g, d.path(), &BTreeMap::new()).unwrap();
        assert_ne!(b.hash, c.hash);
    }

    #[test]
    fn ignored_dir_changes_do_not_matter() {
        let d = repo();
        let g = globs(&["**"]);
        let a = compute_stamp("s", &g, d.path(), &BTreeMap::new()).unwrap();
        write(d.path(), "node_modules/dep/index.js", "changed");
        write(d.path(), "target/new", "changed");
        write(d.path(), ".git/HEAD", "changed");
        let b = compute_stamp("s", &g, d.path(), &BTreeMap::new()).unwrap();
        assert_eq!(a.hash, b.hash);
    }

    #[test]
    fn env_values_matter_but_insertion_order_does_not() {
        let d = repo();
        let g = globs(&["VERSION"]);
        let mut e1 = BTreeMap::new();
        e1.insert("B".to_string(), "2".to_string());
        e1.insert("A".to_string(), "1".to_string());
        let e2 = env(&[("A", "1"), ("B", "2")]);
        let a = compute_stamp("s", &g, d.path(), &e1).unwrap();
        let b = compute_stamp("s", &g, d.path(), &e2).unwrap();
        assert_eq!(a.hash, b.hash);
        let c = compute_stamp("s", &g, d.path(), &env(&[("A", "1"), ("B", "3")])).unwrap();
        assert_ne!(a.hash, c.hash);
        let none = compute_stamp("s", &g, d.path(), &BTreeMap::new()).unwrap();
        assert_ne!(a.hash, none.hash);
        // Length prefixes: moving a character between key and value differs.
        let x = compute_stamp("s", &[], d.path(), &env(&[("AB", "C")])).unwrap();
        let y = compute_stamp("s", &[], d.path(), &env(&[("A", "BC")])).unwrap();
        assert_ne!(x.hash, y.hash);
    }

    #[test]
    fn store_needs_run() {
        let d = repo();
        let g = globs(&["VERSION"]);
        let s1 = compute_stamp("s", &g, d.path(), &BTreeMap::new()).unwrap();
        let mut store = StampStore::new();
        assert!(store.needs_run("api", "setup", &s1));
        store.record("api", "setup", s1.clone());
        assert!(!store.needs_run("api", "setup", &s1));
        assert!(store.needs_run("api", "seed", &s1));
        assert!(store.needs_run("web", "setup", &s1));

        // Same hash computed later (different computed_at) still does not run.
        let again = Stamp {
            computed_at: s1.computed_at + chrono::Duration::seconds(60),
            ..s1.clone()
        };
        assert!(!store.needs_run("api", "setup", &again));

        write(d.path(), "VERSION", "2");
        let s2 = compute_stamp("s", &g, d.path(), &BTreeMap::new()).unwrap();
        assert!(store.needs_run("api", "setup", &s2));

        store.record("api", "seed", s2.clone());
        assert_eq!(store.for_stem("api").unwrap().len(), 2);
        assert_eq!(store.clear_script("api", "seed"), Some(s2.clone()));
        assert_eq!(store.clear_script("api", "seed"), None);
        store.clear_stem("api");
        assert!(store.needs_run("api", "setup", &s1));
        assert!(store.for_stem("api").is_none());
        store.record("x", "y", s1.clone());
        store.clear();
        assert_eq!(store, StampStore::default());
    }

    #[test]
    fn store_serialises_as_nested_map() {
        let mut store = StampStore::new();
        store.record(
            "api",
            "setup",
            Stamp {
                hash: "ab".repeat(32),
                computed_at: DateTime::parse_from_rfc3339("2026-09-26T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                inputs: vec![PathBuf::from("VERSION")],
            },
        );
        let json = serde_json::to_string(&store).unwrap();
        insta::assert_snapshot!(json, @r#"{"api":{"setup":{"hash":"abababababababababababababababababababababababababababababababab","computed_at":"2026-09-26T10:00:00Z","inputs":["VERSION"]}}}"#);
        let back: StampStore = serde_json::from_str(&json).unwrap();
        assert_eq!(back, store);
    }
}
