//! Overlays: files the integration repo materialises into a codebase
//! (FR-WS-7, NFR-3; deliverable 18).
//!
//! Pure decisions plus the few filesystem primitives the daemon needs:
//!
//! - [`render_overlay`]: template substitution (same engine as the config,
//!   including `${stem.self.port}`) or verbatim bytes.
//! - [`plan_materialise`] / [`plan_cleanup`]: never clobber or delete a file
//!   stems did not write (hash-compared against the [`OverlayRecord`]).
//! - [`write_atomic`], [`backup_file`], [`hash_bytes`], [`ExistingFile::probe`],
//!   [`status_of`], [`is_git_tracked`].

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use stems_config::{
    FileMode, Overlay, OverlaySource, Port, PortRef, Stem, StemFacts, TemplateContext, Workspace,
    substitute_template,
};

use crate::{Error, ErrorCode};

/// State entry for one materialised overlay (`state.stems.<stem>.overlays`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlayRecord {
    /// Absolute destination path.
    pub dest: PathBuf,
    /// Lowercase hex sha256 of the bytes stems wrote.
    pub sha256: String,
    /// Run that wrote it.
    pub run_id: String,
    /// `keep: true` (survives `down`).
    pub keep: bool,
}

/// Lowercase hex sha256 of `bytes` (64 chars).
pub fn hash_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Hint of an `UNRESOLVED_VARIABLE` for a `${stem.<n>.outputs.X}` without a
/// value at start time (FR-ST-6).
pub const OUTPUTS_HINT: &str = "outputs are available only from dependencies with condition: healthy (declare `depends_on: [{stem: <name>, condition: healthy}]` and the output under that stem's `outputs:`)";

/// Everything a template can reference while rendering one stem's overlays.
#[derive(Clone, Debug, Default)]
pub struct RenderCtx {
    /// `${workspace.root}`.
    pub workspace_root: PathBuf,
    /// `${workspace.name}`.
    pub workspace_name: String,
    /// Directory overlay sources must stay inside (the integration repo).
    pub integration_repo: PathBuf,
    /// The stem's codebase (`${codebase}`).
    pub codebase: PathBuf,
    /// The enclosing stem (`${stem.self.…}`).
    pub stem_name: String,
    /// Resolved workspace variables (`${var.x}`).
    pub vars: IndexMap<String, String>,
    /// Environment (`${env.X}`).
    pub env: HashMap<String, String>,
    /// Ports per stem; allocated `auto` ports must be set as
    /// [`PortRef::Fixed`] (see [`RenderCtx::set_port`]).
    pub ports: IndexMap<String, Vec<Port>>,
    /// Codebase directory per stem.
    pub codebases: IndexMap<String, PathBuf>,
    /// Evaluated outputs per stem (`${stem.<n>.outputs.X}`, FR-ST-6): the
    /// daemon fills in those of the stem's dependencies.
    pub outputs: IndexMap<String, std::collections::BTreeMap<String, String>>,
}

impl RenderCtx {
    /// Context for `stem` from the resolved workspace: declared ports (auto
    /// ports still unset), variables and codebases.
    pub fn for_stem(ws: &Workspace, stem: &Stem, env: HashMap<String, String>) -> Self {
        Self {
            workspace_root: ws.root.clone(),
            workspace_name: ws.name.clone(),
            integration_repo: ws.root.clone(),
            codebase: stem.default_cwd(&ws.root).to_path_buf(),
            stem_name: stem.name.clone(),
            vars: ws.vars.clone(),
            env,
            ports: ws
                .stems
                .iter()
                .map(|(n, s)| (n.clone(), s.ports.clone()))
                .collect(),
            codebases: ws
                .stems
                .iter()
                .filter_map(|(n, s)| Some((n.clone(), s.codebase.as_ref()?.path().to_path_buf())))
                .collect(),
            outputs: IndexMap::new(),
        }
    }

    /// Record the runtime port of `stem`'s port `port_name` (allocated or
    /// overridden). Returns false if no such port is declared.
    pub fn set_port(&mut self, stem: &str, port_name: &str, port: u16) -> bool {
        let Some(p) = self
            .ports
            .get_mut(stem)
            .and_then(|ps| ps.iter_mut().find(|p| p.name == port_name))
        else {
            return false;
        };
        p.port = PortRef::Fixed(port);
        true
    }

    fn template_context(&self) -> TemplateContext {
        let mut stems: IndexMap<String, StemFacts> = self
            .ports
            .iter()
            .map(|(n, ports)| {
                (
                    n.clone(),
                    StemFacts {
                        ports: ports.clone(),
                        codebase: self.codebases.get(n).cloned(),
                        outputs: self.outputs.get(n).cloned().unwrap_or_default(),
                    },
                )
            })
            .collect();
        stems
            .entry(self.stem_name.clone())
            .or_default()
            .codebase
            .get_or_insert_with(|| self.codebase.clone());
        TemplateContext {
            env: self.env.clone(),
            root: self.workspace_root.clone(),
            workspace_name: self.workspace_name.clone(),
            vars: self.vars.clone(),
            stems,
            stem: Some(self.stem_name.clone()),
        }
    }
}

fn source_error(src: &Path, what: &str) -> Error {
    Error::new(
        ErrorCode::SchemaInvalid,
        format!("overlay source `{}` {what}", src.display()),
    )
    .with_hint("overlay `template:` / `file:` paths are relative to the integration repo and must stay inside it")
    .with_details(json!({ "source": src }))
}

/// Read an overlay source, refusing anything that resolves (after symlinks)
/// outside `repo`.
fn read_source(src: &Path, repo: &Path) -> Result<Vec<u8>, Error> {
    let real =
        fs::canonicalize(src).map_err(|e| source_error(src, &format!("cannot be read: {e}")))?;
    let repo_real = fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    if !real.starts_with(&repo_real) {
        return Err(source_error(src, "is outside the integration repo"));
    }
    fs::read(&real).map_err(|e| source_error(src, &format!("cannot be read: {e}")))
}

/// Escape (`$${`) every `${…}` whose content cannot be a reference, i.e.
/// contains anything besides `[A-Za-z0-9_.-]` and spaces (such as the
/// `${stem.<name>.port}` placeholder in a template's comment), so it is kept
/// literally instead of failing as an unresolved reference.
fn protect_placeholders(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("${") {
        let escaped = i > 0 && rest.as_bytes()[i - 1] == b'$';
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail.find('}');
        let plausible = end.is_none_or(|e| {
            tail[2..e]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | ' '))
        });
        if !escaped && !plausible {
            out.push('$');
        }
        out.push_str("${");
        rest = &tail[2..];
    }
    out.push_str(rest);
    out
}

/// Render an overlay: a `template` is read (it must be UTF-8 and live inside
/// the integration repo) and `${…}` references substituted; a `file` is
/// returned verbatim. Unresolvable references are `UNRESOLVED_VARIABLE`.
pub fn render_overlay(overlay: &Overlay, ctx: &RenderCtx) -> Result<Vec<u8>, Error> {
    match &overlay.source {
        OverlaySource::File(src) => read_source(src, &ctx.integration_repo),
        OverlaySource::Template(src) => {
            let bytes = read_source(src, &ctx.integration_repo)?;
            let text =
                String::from_utf8(bytes).map_err(|_| source_error(src, "is not valid UTF-8"))?;
            let out = substitute_template(&protect_placeholders(&text), &ctx.template_context());
            if let Some(d) = out.diagnostics.first() {
                let all: Vec<&str> = out.diagnostics.iter().map(|d| d.message.as_str()).collect();
                return Err(Error::new(
                    ErrorCode::UnresolvedVariable,
                    format!("overlay template `{}`: {}", src.display(), d.message),
                )
                .with_hint(
                    d.hint
                        .clone()
                        .unwrap_or_else(|| "fix the reference in the template".into()),
                )
                .with_details(json!({ "source": src, "problems": all })));
            }
            if let Some(r) = out.deferred.first() {
                if r.kind == stems_config::DeferredKind::Output {
                    return Err(Error::new(
                        ErrorCode::UnresolvedVariable,
                        format!(
                            "overlay template `{}`: output `${{{}}}` has no value",
                            src.display(),
                            r.reference
                        ),
                    )
                    .with_hint(OUTPUTS_HINT)
                    .with_details(json!({ "source": src, "reference": r.reference })));
                }
                return Err(Error::new(
                    ErrorCode::UnresolvedVariable,
                    format!(
                        "overlay template `{}`: `${{{}}}` has no value yet",
                        src.display(),
                        r.reference
                    ),
                )
                .with_hint("only ports known when the stem starts (fixed or already allocated) can be used in overlays")
                .with_details(json!({ "source": src, "reference": r.reference })));
            }
            Ok(out.text.into_bytes())
        }
    }
}

/// The absolute destination of an overlay: `dest` joined to `codebase`.
/// Absolute `dest`s and `..` components are refused (`SCHEMA_INVALID`), so
/// overlays can never write outside their codebase.
pub fn resolve_dest(codebase: &Path, dest: &Path) -> Result<PathBuf, Error> {
    let ok = !dest.as_os_str().is_empty()
        && dest
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !ok {
        return Err(Error::new(
            ErrorCode::SchemaInvalid,
            format!(
                "overlay dest `{}` must be a relative path inside the codebase",
                dest.display()
            ),
        )
        .with_hint("use a path relative to the codebase without `..`, e.g. `config/local.ini`")
        .with_details(json!({ "dest": dest })));
    }
    Ok(codebase.join(dest))
}

/// What is currently at a destination path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum ExistingFile {
    /// Nothing there.
    Absent,
    /// A file with this content hash.
    Present {
        /// Lowercase hex sha256.
        sha256: String,
    },
}

impl ExistingFile {
    /// Hash whatever is at `path` (`Absent` if it does not exist; a
    /// directory or unreadable file is an error).
    pub fn probe(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => Ok(Self::Present {
                sha256: hash_bytes(&bytes),
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::Absent),
            Err(e) => Err(e),
        }
    }
}

/// Decision for materialising one overlay.
#[derive(Clone, Debug, PartialEq)]
pub enum MaterialisePlan {
    /// Write the rendered bytes (record first, then write).
    Write,
    /// Back the existing file up, then write (`--force-overlays`).
    WriteAfterBackup,
    /// Refuse: `OVERLAY_CONFLICT`.
    Conflict(Error),
    /// The file already holds exactly these bytes and is stems-owned.
    Unchanged,
}

fn conflict(dest: &Path, reason: &str) -> Error {
    Error::new(
        ErrorCode::OverlayConflict,
        format!("overlay dest `{}` {reason}", dest.display()),
    )
    .with_hint("use keep: true and an explicit .gitignore, or choose another dest (or pass --force-overlays to back the file up and overwrite it)")
    .with_details(json!({ "dest": dest, "reason": reason }))
}

/// Decide how to materialise `dest`. `owned` is the state record for this
/// dest, if stems wrote it before. A file stems did not write — or wrote and
/// someone modified since — is never overwritten unless `force`.
pub fn plan_materialise(
    dest: &Path,
    rendered_hash: &str,
    existing: &ExistingFile,
    owned: Option<&OverlayRecord>,
    force: bool,
) -> MaterialisePlan {
    let ExistingFile::Present { sha256 } = existing else {
        return MaterialisePlan::Write;
    };
    let owned = owned.filter(|r| r.dest == dest);
    match owned {
        Some(rec) if rec.sha256 == *sha256 => {
            if sha256 == rendered_hash {
                MaterialisePlan::Unchanged
            } else {
                MaterialisePlan::Write
            }
        }
        Some(_) if sha256 == rendered_hash => MaterialisePlan::Unchanged,
        _ if force => MaterialisePlan::WriteAfterBackup,
        Some(_) => {
            MaterialisePlan::Conflict(conflict(dest, "was written by stems but modified since"))
        }
        None => MaterialisePlan::Conflict(conflict(
            dest,
            "already exists and was not written by stems",
        )),
    }
}

/// Decision for one recorded overlay on `down` / `stop`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPlan {
    /// Unmodified stems file: delete it.
    Remove,
    /// Content differs from what stems wrote: leave it, warn
    /// `overlay.modified_left_in_place`.
    LeaveModified,
    /// `keep: true`.
    LeaveKept,
    /// Nothing there any more.
    AlreadyGone,
}

/// Decide what to do with a recorded overlay at cleanup time.
pub fn plan_cleanup(record: &OverlayRecord, existing: &ExistingFile) -> CleanupPlan {
    match existing {
        ExistingFile::Absent => CleanupPlan::AlreadyGone,
        _ if record.keep => CleanupPlan::LeaveKept,
        ExistingFile::Present { sha256 } if *sha256 != record.sha256 => CleanupPlan::LeaveModified,
        ExistingFile::Present { .. } => CleanupPlan::Remove,
    }
}

/// Status of a recorded overlay (`stems overlays`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverlayStatus {
    /// On disk with the bytes stems wrote.
    Present,
    /// On disk with different content.
    Modified,
    /// Not on disk.
    Missing,
}

/// Status of `record` given what is on disk (see [`ExistingFile::probe`]).
pub fn status_of(record: &OverlayRecord, existing: &ExistingFile) -> OverlayStatus {
    match existing {
        ExistingFile::Absent => OverlayStatus::Missing,
        ExistingFile::Present { sha256 } if *sha256 == record.sha256 => OverlayStatus::Present,
        ExistingFile::Present { .. } => OverlayStatus::Modified,
    }
}

/// Write `bytes` to `dest` atomically: parent directories are created, a
/// temp file in the same directory is written, synced, chmod'ed to `mode`
/// and renamed over `dest`.
pub fn write_atomic(dest: &Path, bytes: &[u8], mode: FileMode) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = dest
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "dest has no file name"))?;
    fs::create_dir_all(dir)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let tmp = dir.join(format!(
        ".{}.stems-tmp-{}-{nanos}",
        name.to_string_lossy(),
        std::process::id()
    ));
    let result = (|| {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode.0)
            .open(&tmp)?;
        f.write_all(bytes)?;
        // The umask may have narrowed the mode given to open().
        f.set_permissions(fs::Permissions::from_mode(mode.0))?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, dest)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Copy `dest` to `<backup_dir>/<run_id>/<dest without its root>` (the
/// `--force-overlays` backup) and return the backup path.
pub fn backup_file(dest: &Path, backup_dir: &Path, run_id: &str) -> io::Result<PathBuf> {
    let rel: PathBuf = dest
        .components()
        .filter_map(|c| match c {
            Component::Normal(p) => Some(p),
            _ => None,
        })
        .collect();
    let target = backup_dir.join(run_id).join(rel);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(dest, &target)?;
    Ok(target)
}

/// Whether `rel` is tracked in `codebase`'s git index (`git ls-files
/// --error-unmatch`). `None` when git is not installed or `codebase` is not
/// inside a git work tree.
pub fn is_git_tracked(codebase: &Path, rel: &Path) -> Option<bool> {
    let out = Command::new("git")
        .arg("-C")
        .arg(codebase)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(rel)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    match out.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H1: &str = "aaaa";
    const H2: &str = "bbbb";
    const H3: &str = "cccc";

    fn rec(dest: &str, sha: &str, keep: bool) -> OverlayRecord {
        OverlayRecord {
            dest: dest.into(),
            sha256: sha.into(),
            run_id: "r1".into(),
            keep,
        }
    }

    fn present(sha: &str) -> ExistingFile {
        ExistingFile::Present { sha256: sha.into() }
    }

    fn is_conflict(p: &MaterialisePlan, reason: &str) -> bool {
        matches!(p, MaterialisePlan::Conflict(e)
            if e.code == ErrorCode::OverlayConflict
                && e.details["reason"].as_str().unwrap().contains(reason)
                && e.hint.as_deref().unwrap().contains("keep: true"))
    }

    #[test]
    fn materialise_branches() {
        let d = Path::new("/c/x.ini");
        let own = rec("/c/x.ini", H1, false);
        let other = rec("/c/other.ini", H1, false);
        use MaterialisePlan as P;
        // Absent: always write.
        assert_eq!(
            plan_materialise(d, H1, &ExistingFile::Absent, None, false),
            P::Write
        );
        assert_eq!(
            plan_materialise(d, H1, &ExistingFile::Absent, Some(&own), false),
            P::Write
        );
        // Owned and untouched.
        assert_eq!(
            plan_materialise(d, H1, &present(H1), Some(&own), false),
            P::Unchanged
        );
        assert_eq!(
            plan_materialise(d, H2, &present(H1), Some(&own), false),
            P::Write
        );
        // Owned but modified.
        assert_eq!(
            plan_materialise(d, H2, &present(H2), Some(&own), false),
            P::Unchanged
        );
        assert!(is_conflict(
            &plan_materialise(d, H3, &present(H2), Some(&own), false),
            "modified since"
        ));
        assert_eq!(
            plan_materialise(d, H3, &present(H2), Some(&own), true),
            P::WriteAfterBackup
        );
        // Not owned (no record, or a record for another dest).
        for owned in [None, Some(&other)] {
            assert!(is_conflict(
                &plan_materialise(d, H1, &present(H1), owned, false),
                "not written by stems"
            ));
            assert_eq!(
                plan_materialise(d, H1, &present(H2), owned, true),
                P::WriteAfterBackup
            );
        }
    }

    #[test]
    fn cleanup_branches() {
        use CleanupPlan as C;
        assert_eq!(plan_cleanup(&rec("/x", H1, false), &present(H1)), C::Remove);
        assert_eq!(
            plan_cleanup(&rec("/x", H1, false), &present(H2)),
            C::LeaveModified
        );
        assert_eq!(
            plan_cleanup(&rec("/x", H1, true), &present(H1)),
            C::LeaveKept
        );
        assert_eq!(
            plan_cleanup(&rec("/x", H1, true), &present(H2)),
            C::LeaveKept
        );
        assert_eq!(
            plan_cleanup(&rec("/x", H1, false), &ExistingFile::Absent),
            C::AlreadyGone
        );
        assert_eq!(
            plan_cleanup(&rec("/x", H1, true), &ExistingFile::Absent),
            C::AlreadyGone
        );
    }

    #[test]
    fn templates_see_outputs_of_running_stems() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_path_buf();
        fs::write(repo.join("t.tmpl"), "url=${stem.api.outputs.URL}\n").unwrap();
        fs::write(repo.join("missing.tmpl"), "${stem.api.outputs.NOPE}").unwrap();
        let mut ctx = RenderCtx {
            workspace_root: repo.clone(),
            integration_repo: repo.clone(),
            codebase: repo.join("code"),
            stem_name: "web".into(),
            ports: [("api".to_string(), Vec::new())].into_iter().collect(),
            ..Default::default()
        };
        ctx.outputs.insert(
            "api".into(),
            [("URL".to_string(), "http://localhost:1".to_string())].into(),
        );
        let overlay = |src: &str| Overlay {
            source: OverlaySource::Template(repo.join(src)),
            dest: "x".into(),
            keep: false,
            mode: FileMode(0o644),
        };
        assert_eq!(
            render_overlay(&overlay("t.tmpl"), &ctx).unwrap(),
            b"url=http://localhost:1\n"
        );
        let e = render_overlay(&overlay("missing.tmpl"), &ctx).unwrap_err();
        assert_eq!(e.code, ErrorCode::UnresolvedVariable);
        assert_eq!(e.hint.as_deref(), Some(OUTPUTS_HINT));
        assert_eq!(e.details["reference"], "stem.api.outputs.NOPE");
    }

    #[test]
    fn statuses() {
        let r = rec("/x", H1, false);
        assert_eq!(status_of(&r, &present(H1)), OverlayStatus::Present);
        assert_eq!(status_of(&r, &present(H2)), OverlayStatus::Modified);
        assert_eq!(status_of(&r, &ExistingFile::Absent), OverlayStatus::Missing);
    }

    #[test]
    fn placeholders_are_protected() {
        assert_eq!(
            protect_placeholders("a ${stem.<name>.port} ${stem.x.port} $${keep} ${ codebase } ${x"),
            "a $${stem.<name>.port} ${stem.x.port} $${keep} ${ codebase } ${x"
        );
    }

    #[test]
    fn hashing() {
        assert_eq!(
            hash_bytes(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn dest_resolution() {
        let c = Path::new("/code/api");
        assert_eq!(
            resolve_dest(c, Path::new("config/local.ini")).unwrap(),
            Path::new("/code/api/config/local.ini")
        );
        for bad in ["../x", "/etc/passwd", "a/../../b", ""] {
            let e = resolve_dest(c, Path::new(bad)).unwrap_err();
            assert_eq!(e.code, ErrorCode::SchemaInvalid, "{bad}");
        }
    }

    #[test]
    fn atomic_write_sets_mode_and_replaces() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("nested/dir/secret.env");
        write_atomic(&dest, b"one", FileMode(0o600)).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"one");
        let mode = fs::metadata(&dest).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o600);
        write_atomic(&dest, b"two", FileMode(0o644)).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"two");
        let mode = fs::metadata(&dest).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o644);
        let leftovers: Vec<_> = fs::read_dir(dest.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["secret.env"]);
        assert_eq!(
            ExistingFile::probe(&dest).unwrap(),
            present(&hash_bytes(b"two"))
        );
        assert_eq!(
            ExistingFile::probe(&dir.path().join("nope")).unwrap(),
            ExistingFile::Absent
        );
    }

    #[test]
    fn backup_mirrors_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("code/config/local.ini");
        write_atomic(&dest, b"user data", FileMode(0o644)).unwrap();
        let backups = dir.path().join("backups");
        let b = backup_file(&dest, &backups, "run42").unwrap();
        assert!(b.starts_with(backups.join("run42")));
        assert!(b.ends_with("code/config/local.ini"));
        assert_eq!(fs::read(b).unwrap(), b"user data");
    }

    fn git(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[test]
    fn git_tracked_check() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        if !git(root, &["--version"]) {
            eprintln!("git not installed; skipping");
            return;
        }
        // Not a repository (the temp dir is outside any work tree).
        if is_git_tracked(root, Path::new("a.txt")).is_some() {
            eprintln!("temp dir is inside a git work tree; skipping");
            return;
        }
        assert!(git(root, &["init", "-q"]));
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config/tracked.ini"), "x").unwrap();
        fs::write(root.join("untracked.ini"), "y").unwrap();
        assert!(git(root, &["add", "config/tracked.ini"]));
        assert_eq!(
            is_git_tracked(root, Path::new("config/tracked.ini")),
            Some(true)
        );
        assert_eq!(
            is_git_tracked(root, Path::new("untracked.ini")),
            Some(false)
        );
        assert_eq!(is_git_tracked(root, Path::new("missing.ini")), Some(false));
    }
}
