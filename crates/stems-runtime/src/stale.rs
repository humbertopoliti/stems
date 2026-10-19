//! Is a built image older than its build inputs? (deliverable 35)
//!
//! "Build inputs" are the build context walked the way the docker runtime
//! tars it ([`crate::docker::context::context_files`]: `.dockerignore`
//! honoured, `.git` skipped) plus any extra files (the Dockerfile, which
//! may live outside the context, and the compose file). The newest
//! modification time wins; an mtime in the future counts as "now". The
//! scan stops at the first input newer than the image (`cutoff`), so the
//! common fresh case walks everything once and the stale case stops early.
//! More than [`MAX_ENTRIES`] entries is [`Verdict::Unknown`]: never a false
//! warning on a huge context.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::docker::context::context_files;

/// Entries scanned before the verdict is `Unknown`.
pub const MAX_ENTRIES: usize = 50_000;

/// What the scan found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scan {
    /// The newest input (mtime, path), if any.
    pub newest: Option<(SystemTime, PathBuf)>,
    /// Entries looked at (directories count).
    pub visited: usize,
    /// The scan stopped at the first input newer than `cutoff`.
    pub stopped_early: bool,
    /// [`MAX_ENTRIES`] reached: `newest` is not the newest.
    pub capped: bool,
}

/// The newest modification time among `context`'s build inputs and
/// `extras` (each counted when it exists). With `cutoff`, the scan
/// returns as soon as an input newer than it is found (`stopped_early`).
pub fn newest_input(
    context: &Path,
    dockerfile: &Path,
    extras: &[PathBuf],
    cutoff: Option<SystemTime>,
) -> io::Result<Scan> {
    let mut scan = Scan {
        newest: None,
        visited: 0,
        stopped_early: false,
        capped: false,
    };
    let note = |scan: &mut Scan, mtime: SystemTime, path: PathBuf| -> bool {
        if scan.newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
            scan.newest = Some((mtime, path));
        }
        if cutoff.is_some_and(|c| mtime > c) {
            scan.stopped_early = true;
            return true;
        }
        false
    };
    // The extras first: they are few, and the Dockerfile is the likeliest
    // thing to have changed after the context.
    for p in extras {
        match std::fs::metadata(p) {
            Ok(m) => {
                scan.visited += 1;
                if note(&mut scan, m.modified()?, p.clone()) {
                    return Ok(scan);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    // The context root itself: adding or removing a top-level file changes it.
    let root = std::fs::metadata(context)?;
    scan.visited += 1;
    if note(&mut scan, root.modified()?, context.to_path_buf()) {
        return Ok(scan);
    }
    for entry in context_files(context, dockerfile)? {
        let entry = entry?;
        scan.visited += 1;
        if scan.visited > MAX_ENTRIES {
            scan.capped = true;
            return Ok(scan);
        }
        if note(&mut scan, entry.meta.modified()?, entry.path) {
            return Ok(scan);
        }
    }
    Ok(scan)
}

/// The comparison's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum Verdict {
    /// No input is newer than the image.
    Fresh {
        image_created: SystemTime,
        /// The newest input, if the context has any file.
        newest: Option<SystemTime>,
    },
    /// `path` was modified after the image was built.
    Stale {
        image_created: SystemTime,
        newest: SystemTime,
        path: PathBuf,
    },
    /// The image does not exist locally (nothing to compare; the first
    /// build creates it).
    NoImage,
    /// The inputs could not be scanned (unreadable, or more than
    /// [`MAX_ENTRIES`]): no verdict, never a warning.
    Unknown { reason: String },
}

impl Verdict {
    pub fn is_stale(&self) -> bool {
        matches!(self, Verdict::Stale { .. })
    }
}

/// Compare an image's creation time with the scanned inputs (`now` clamps
/// mtimes from the future). Stale means strictly older: an input modified
/// in the same instant the image was created is fresh.
pub fn compare(
    image_created: Option<SystemTime>,
    inputs: io::Result<Scan>,
    now: SystemTime,
) -> Verdict {
    let Some(image_created) = image_created else {
        return Verdict::NoImage;
    };
    let scan = match inputs {
        Ok(s) => s,
        Err(e) => {
            return Verdict::Unknown {
                reason: format!("cannot scan the build inputs: {e}"),
            };
        }
    };
    if scan.capped {
        return Verdict::Unknown {
            reason: format!("the build context has more than {MAX_ENTRIES} entries"),
        };
    }
    match scan.newest {
        Some((mtime, path)) => {
            let newest = mtime.min(now);
            if image_created < newest {
                Verdict::Stale {
                    image_created,
                    newest,
                    path,
                }
            } else {
                Verdict::Fresh {
                    image_created,
                    newest: Some(newest),
                }
            }
        }
        None => Verdict::Fresh {
            image_created,
            newest: None,
        },
    }
}

/// [`newest_input`] with `cutoff = image_created`, then [`compare`]: the
/// one call the daemon and `doctor` make (blocking: run it off the async
/// thread).
pub fn check(
    image_created: Option<SystemTime>,
    context: &Path,
    dockerfile: &Path,
    extras: &[PathBuf],
    now: SystemTime,
) -> Verdict {
    if image_created.is_none() {
        return Verdict::NoImage;
    }
    let scan = newest_input(context, dockerfile, extras, image_created);
    compare(image_created, scan, now)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn write(root: &Path, rel: &str, body: &str) -> PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    fn set_mtime(p: &Path, t: SystemTime) {
        // Read-only works for directories too (futimens on the fd).
        std::fs::File::open(p).unwrap().set_modified(t).unwrap();
    }

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn scan(newest: Option<(u64, &str)>, capped: bool) -> io::Result<Scan> {
        Ok(Scan {
            newest: newest.map(|(s, p)| (t(s), PathBuf::from(p))),
            visited: 1,
            stopped_early: false,
            capped,
        })
    }

    #[test]
    fn compare_table() {
        let now = t(1_000);
        assert_eq!(
            compare(Some(t(500)), scan(Some((400, "app.py")), false), now),
            Verdict::Fresh {
                image_created: t(500),
                newest: Some(t(400))
            }
        );
        assert_eq!(
            compare(Some(t(500)), scan(Some((600, "app.py")), false), now),
            Verdict::Stale {
                image_created: t(500),
                newest: t(600),
                path: "app.py".into()
            }
        );
        // Equal instant: fresh (stale is strictly older).
        assert!(matches!(
            compare(Some(t(500)), scan(Some((500, "app.py")), false), now),
            Verdict::Fresh { .. }
        ));
        // An mtime in the future is clamped to now: stale, newest = now.
        assert_eq!(
            compare(Some(t(500)), scan(Some((9_999, "app.py")), false), now),
            Verdict::Stale {
                image_created: t(500),
                newest: now,
                path: "app.py".into()
            }
        );
        // ... and an image built "now" is not stale because of it.
        assert!(matches!(
            compare(Some(now), scan(Some((9_999, "app.py")), false), now),
            Verdict::Fresh { .. }
        ));
        assert_eq!(
            compare(None, scan(Some((600, "app.py")), false), now),
            Verdict::NoImage
        );
        assert!(matches!(
            compare(Some(t(500)), scan(Some((600, "app.py")), true), now),
            Verdict::Unknown { reason } if reason.contains("50000")
        ));
        assert!(matches!(
            compare(Some(t(500)), Err(io::Error::other("boom")), now),
            Verdict::Unknown { reason } if reason.contains("boom")
        ));
        // An empty context is fresh.
        assert_eq!(
            compare(Some(t(500)), scan(None, false), now),
            Verdict::Fresh {
                image_created: t(500),
                newest: None
            }
        );
    }

    #[test]
    fn newest_input_honours_dockerignore_git_and_extras() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let ctx = root.join("ctx");
        let old = t(1_000);
        let app = write(&ctx, "app.py", "x\n");
        let ignored = write(&ctx, "node_modules/x.js", "x\n");
        let git = write(&ctx, ".git/HEAD", "x\n");
        let di = write(&ctx, ".dockerignore", "node_modules\n");
        let dockerfile = write(root, "docker/api.Dockerfile", "FROM x\n");
        let compose = write(root, "compose.yml", "services: {}\n");
        for p in [&app, &ignored, &git, &di, &dockerfile, &compose] {
            set_mtime(p, old);
        }
        for d in [
            &ctx,
            &ctx.join("node_modules"),
            &ctx.join(".git"),
            &root.join("docker"),
        ] {
            set_mtime(d, old);
        }
        let extras = vec![dockerfile.clone(), compose.clone()];
        let newest = |cutoff| {
            newest_input(&ctx, &dockerfile, &extras, cutoff)
                .unwrap()
                .newest
                .unwrap()
        };
        assert_eq!(newest(None).0, old, "everything is old");

        // Ignored and `.git` files never count.
        set_mtime(&ignored, t(5_000));
        set_mtime(&git, t(5_000));
        assert_eq!(newest(None).0, old);

        // The Dockerfile outside the context and the compose file do.
        set_mtime(&dockerfile, t(2_000));
        assert_eq!(newest(None), (t(2_000), dockerfile.clone()));
        set_mtime(&compose, t(3_000));
        assert_eq!(newest(None), (t(3_000), compose.clone()));
        set_mtime(&app, t(4_000));
        assert_eq!(newest(None), (t(4_000), app.clone()));

        // A missing extra is skipped.
        let scan = newest_input(&ctx, &dockerfile, &[root.join("nope.yml")], None).unwrap();
        assert_eq!(scan.newest.unwrap().0, t(4_000));
    }

    #[test]
    fn newest_input_stops_at_the_first_newer_file() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = dir.path().join("ctx");
        let old = t(1_000);
        let dockerfile = write(&ctx, "Dockerfile", "FROM x\n");
        let a = write(&ctx, "a.txt", "a\n");
        let mut all = vec![dockerfile.clone(), a.clone()];
        for i in 0..20 {
            all.push(write(&ctx, &format!("z/{i}.txt"), "z\n"));
        }
        for p in &all {
            set_mtime(p, old);
        }
        set_mtime(&ctx, old);
        set_mtime(&ctx.join("z"), old);
        let full = newest_input(&ctx, &dockerfile, &[], None).unwrap();
        assert!(!full.stopped_early);
        assert_eq!(full.visited, 1 + all.len() + 1, "root, files, z/");

        // `a.txt` newer than the cutoff: the scan returns before `z/`.
        set_mtime(&a, t(2_000));
        let early = newest_input(&ctx, &dockerfile, &[], Some(t(1_500))).unwrap();
        assert!(early.stopped_early);
        assert_eq!(early.newest.unwrap().1, a);
        assert!(
            early.visited < full.visited / 2,
            "visited {} of {}",
            early.visited,
            full.visited
        );
        // Nothing newer: the whole context is read, no early stop.
        let fresh = newest_input(&ctx, &dockerfile, &[], Some(t(3_000))).unwrap();
        assert!(!fresh.stopped_early);
        assert_eq!(fresh.visited, full.visited);
    }

    #[test]
    fn check_is_the_two_steps() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = dir.path().join("ctx");
        let dockerfile = write(&ctx, "Dockerfile", "FROM x\n");
        set_mtime(&dockerfile, t(1_000));
        set_mtime(&ctx, t(1_000));
        assert_eq!(
            check(None, &ctx, &dockerfile, &[], t(5_000)),
            Verdict::NoImage
        );
        assert!(!check(Some(t(2_000)), &ctx, &dockerfile, &[], t(5_000)).is_stale());
        assert!(check(Some(t(500)), &ctx, &dockerfile, &[], t(5_000)).is_stale());
        assert!(matches!(
            check(
                Some(t(500)),
                &dir.path().join("missing"),
                &dockerfile,
                &[],
                t(5_000)
            ),
            Verdict::Unknown { .. }
        ));
    }
}
