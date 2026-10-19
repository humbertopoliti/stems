//! Build-context tarball for `docker build` through the API.
//!
//! Minimal `.dockerignore` support: `#` comments, blank lines, `!`
//! negations (last match wins), `*`/`?`/`**` globs anchored at the context
//! root; a pattern that matches a directory excludes everything under it.
//! `.git` is always skipped. The Dockerfile and `.dockerignore` are always
//! sent (as Docker does).
//!
//! [`context_files`] is the walk itself, an iterator shared by the tarball
//! and the stale-image scan (`crate::stale`), so both agree on what a
//! build input is.

use std::fs::Metadata;
use std::io;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};

/// Name used for a Dockerfile that lives outside the context directory.
pub const EXTERNAL_DOCKERFILE: &str = ".stems.Dockerfile";

/// Parsed `.dockerignore`.
#[derive(Debug, Default)]
pub struct DockerIgnore {
    rules: Vec<(GlobMatcher, bool)>,
}

impl DockerIgnore {
    /// Parse `.dockerignore` text; invalid patterns are skipped.
    pub fn parse(text: &str) -> Self {
        let mut rules = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (negate, pat) = match line.strip_prefix('!') {
                Some(p) => (true, p.trim()),
                None => (false, line),
            };
            let pat = pat.trim_start_matches("./").trim_start_matches('/');
            let pat = pat.trim_end_matches('/');
            if pat.is_empty() {
                continue;
            }
            match GlobBuilder::new(pat).literal_separator(true).build() {
                Ok(g) => rules.push((g.compile_matcher(), negate)),
                Err(e) => tracing::debug!(pattern = pat, error = %e, "bad .dockerignore pattern"),
            }
        }
        Self { rules }
    }

    fn has_negations(&self) -> bool {
        self.rules.iter().any(|(_, n)| *n)
    }

    /// Is `rel` (context-relative, `/`-separated) excluded? A pattern also
    /// excludes everything below a directory it matches.
    pub fn is_ignored(&self, rel: &str) -> bool {
        let mut ignored = false;
        for (m, negate) in &self.rules {
            let hit = prefixes(rel).any(|p| m.is_match(p));
            if hit {
                ignored = !negate;
            }
        }
        ignored
    }
}

/// `a`, `a/b`, `a/b/c` for `a/b/c`.
fn prefixes(rel: &str) -> impl Iterator<Item = &str> {
    rel.match_indices('/')
        .map(move |(i, _)| &rel[..i])
        .chain(std::iter::once(rel))
}

/// The Dockerfile's path relative to the context (`/`-separated), or
/// `None` when it lives outside the context.
fn dockerfile_rel(context: &Path, dockerfile: &Path) -> Option<String> {
    dockerfile
        .strip_prefix(context)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
}

/// Tar the build context. Returns the archive and the Dockerfile path to
/// pass to the build API (relative to the context).
pub fn context_tar(context: &Path, dockerfile: &Path) -> io::Result<(Vec<u8>, String)> {
    let mut builder = tar::Builder::new(Vec::new());
    builder.follow_symlinks(false);
    builder.mode(tar::HeaderMode::Deterministic);
    for entry in context_files(context, dockerfile)? {
        let entry = entry?;
        if entry.meta.is_dir() {
            builder.append_dir(&entry.rel, &entry.path)?;
        } else {
            builder.append_path_with_name(&entry.path, &entry.rel)?;
        }
    }

    let name = match dockerfile_rel(context, dockerfile) {
        Some(rel) => rel,
        None => {
            builder.append_path_with_name(dockerfile, EXTERNAL_DOCKERFILE)?;
            EXTERNAL_DOCKERFILE.to_string()
        }
    };
    Ok((builder.into_inner()?, name))
}

/// One file or directory of a build context, as `docker build` would send
/// it: `meta` is the entry's own metadata (symlinks are not followed).
#[derive(Debug)]
pub struct ContextEntry {
    /// Absolute path.
    pub path: PathBuf,
    /// Context-relative, `/`-separated.
    pub rel: String,
    pub meta: Metadata,
}

/// The build inputs under `context`, in the order [`context_tar`] archives
/// them: sorted per directory, depth first, `.dockerignore` applied,
/// `.git` skipped, the Dockerfile (when inside the context) and
/// `.dockerignore` always included. A Dockerfile outside the context is
/// not yielded (the tarball adds it as [`EXTERNAL_DOCKERFILE`]). Lazy: a
/// consumer that stops early reads no further directory.
pub fn context_files(context: &Path, dockerfile: &Path) -> io::Result<ContextFiles> {
    let ignore = match std::fs::read_to_string(context.join(".dockerignore")) {
        Ok(t) => DockerIgnore::parse(&t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => DockerIgnore::default(),
        Err(e) => return Err(e),
    };
    let mut files = ContextFiles {
        ignore,
        dockerfile: dockerfile_rel(context, dockerfile),
        stack: Vec::new(),
    };
    files.push_dir(context, "")?;
    Ok(files)
}

/// Iterator of [`context_files`].
pub struct ContextFiles {
    ignore: DockerIgnore,
    dockerfile: Option<String>,
    /// Directories being walked: their remaining entries (sorted, reversed
    /// so `pop` yields the first) and their context-relative path.
    stack: Vec<(Vec<std::fs::DirEntry>, String)>,
}

impl ContextFiles {
    fn push_dir(&mut self, dir: &Path, rel_dir: &str) -> io::Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
        self.stack.push((entries, rel_dir.to_string()));
        Ok(())
    }

    fn step(&mut self) -> io::Result<Option<ContextEntry>> {
        loop {
            let Some((entries, rel_dir)) = self.stack.last_mut() else {
                return Ok(None);
            };
            let Some(entry) = entries.pop() else {
                self.stack.pop();
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let rel = if rel_dir.is_empty() {
                name
            } else {
                format!("{rel_dir}/{name}")
            };
            let dockerfile = self.dockerfile.as_deref();
            let always = rel == ".dockerignore" || Some(rel.as_str()) == dockerfile;
            let ignored = !always && self.ignore.is_ignored(&rel);
            let meta = entry.metadata()?;
            if meta.is_dir() {
                // With negations, something below an ignored dir may be re-included.
                let dockerfile_below =
                    dockerfile.is_some_and(|d| d.starts_with(&format!("{rel}/")));
                if ignored && !self.ignore.has_negations() && !dockerfile_below {
                    continue;
                }
                let path = entry.path();
                self.push_dir(&path, &rel)?;
                if !ignored {
                    return Ok(Some(ContextEntry { path, rel, meta }));
                }
            } else if !ignored {
                return Ok(Some(ContextEntry {
                    path: entry.path(),
                    rel,
                    meta,
                }));
            }
        }
    }
}

impl Iterator for ContextFiles {
    type Item = io::Result<ContextEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.step() {
            Ok(Some(e)) => Some(Ok(e)),
            Ok(None) => None,
            Err(e) => {
                // Do not retry a failing directory forever.
                self.stack.clear();
                Some(Err(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(tar_bytes: &[u8]) -> Vec<String> {
        let mut a = tar::Archive::new(tar_bytes);
        let mut v: Vec<String> = a
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn dockerignore_rules() {
        let ig =
            DockerIgnore::parse("# c\n\nnode_modules\n*.env\n!keep.env\n/target/\ndocs/**/*.md\n");
        assert!(ig.is_ignored("node_modules"));
        assert!(ig.is_ignored("node_modules/x/y.js"));
        assert!(!ig.is_ignored("src/node_modules"), "anchored at the root");
        assert!(ig.is_ignored("secret.env"));
        assert!(!ig.is_ignored("keep.env"));
        assert!(!ig.is_ignored("src/a.env"), "`*` does not cross `/`");
        assert!(ig.is_ignored("target/debug/app"));
        assert!(ig.is_ignored("docs/a/b.md"));
        assert!(!ig.is_ignored("src/main.rs"));
    }

    #[test]
    fn context_tar_honours_dockerignore_and_skips_git() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Dockerfile", "FROM alpine:3\n");
        write(root, "src/main.rs", "fn main() {}\n");
        write(root, ".git/HEAD", "ref: refs/heads/main\n");
        write(root, "node_modules/x/y.js", "x\n");
        write(root, "target/debug/app", "bin\n");
        write(root, "secret.env", "A=1\n");
        write(root, "keep.env", "B=2\n");
        write(
            root,
            ".dockerignore",
            "node_modules\n*.env\n!keep.env\ntarget/\nDockerfile\n",
        );

        let (bytes, dockerfile) = context_tar(root, &root.join("Dockerfile")).unwrap();
        assert_eq!(dockerfile, "Dockerfile");
        assert_eq!(
            entries(&bytes),
            vec![
                ".dockerignore",
                "Dockerfile",
                "keep.env",
                "src",
                "src/main.rs"
            ]
        );
    }

    #[test]
    fn context_files_yields_the_archive_order_lazily() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "Dockerfile", "FROM alpine:3\n");
        write(root, "b/2.txt", "2\n");
        write(root, "b/1.txt", "1\n");
        write(root, "a.txt", "a\n");
        write(root, ".git/HEAD", "x\n");
        write(root, "node_modules/x.js", "x\n");
        write(root, ".dockerignore", "node_modules\n");
        let rels: Vec<String> = context_files(root, &root.join("Dockerfile"))
            .unwrap()
            .map(|e| e.unwrap().rel)
            .collect();
        assert_eq!(
            rels,
            vec![
                ".dockerignore",
                "Dockerfile",
                "a.txt",
                "b",
                "b/1.txt",
                "b/2.txt"
            ]
        );
        // The same list the tarball holds (the refactor keeps it byte-identical).
        let (bytes, _) = context_tar(root, &root.join("Dockerfile")).unwrap();
        assert_eq!(entries(&bytes), rels);
        // Stopping early reads nothing more: the first entry comes without
        // walking `b/`.
        let mut it = context_files(root, &root.join("Dockerfile")).unwrap();
        assert_eq!(it.next().unwrap().unwrap().rel, ".dockerignore");
        assert_eq!(it.stack.len(), 1, "only the root directory was read");
    }

    #[test]
    fn dockerfile_outside_context_is_added() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "ctx/app.py", "print(1)\n");
        write(dir.path(), "docker/api.Dockerfile", "FROM python:3\n");
        let (bytes, dockerfile) = context_tar(
            &dir.path().join("ctx"),
            &dir.path().join("docker/api.Dockerfile"),
        )
        .unwrap();
        assert_eq!(dockerfile, EXTERNAL_DOCKERFILE);
        assert_eq!(entries(&bytes), vec![EXTERNAL_DOCKERFILE, "app.py"]);
    }
}
