//! `stems __man <outdir>` (hidden, FR-DS-1): write roff man pages with
//! `clap_mangen`, one per command: `stems.1`, `stems-up.1`,
//! `stems-daemon-start.1`, … Run by `release/build-extras.sh` so release
//! tarballs and the Homebrew formula ship them (`man stems`, `man stems-up`).

use std::path::{Path, PathBuf};

use clap::Command;
use serde_json::json;
use stems_core::Error;

use crate::output::CommandOutput;

/// Write every page into `dir` (created if missing); returns the file names.
pub fn write_pages(dir: &Path) -> std::io::Result<Vec<String>> {
    std::fs::create_dir_all(dir)?;
    let mut cmd = crate::cli::command();
    cmd.build();
    let mut written = Vec::new();
    write_tree(&cmd, "stems", dir, &mut written)?;
    written.sort();
    Ok(written)
}

fn write_tree(cmd: &Command, name: &str, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    let page = cmd.clone().name(name.to_string());
    let mut buf = Vec::new();
    clap_mangen::Man::new(page).render(&mut buf)?;
    let file = format!("{name}.1");
    std::fs::write(dir.join(&file), buf)?;
    out.push(file);
    for sub in cmd.get_subcommands() {
        if sub.is_hide_set() || sub.get_name() == "help" {
            continue;
        }
        write_tree(sub, &format!("{name}-{}", sub.get_name()), dir, out)?;
    }
    Ok(())
}

/// Run `__man`.
pub fn run(cwd: &Path, outdir: &Path) -> CommandOutput {
    let dir: PathBuf = cwd.join(outdir);
    match write_pages(&dir) {
        Ok(pages) => {
            let human = format!("wrote {} man pages to {}\n", pages.len(), dir.display());
            CommandOutput::data(json!({ "dir": dir, "pages": pages })).with_human(human)
        }
        Err(e) => CommandOutput::failed(Error::internal(format!(
            "writing man pages to {}: {e}",
            dir.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn writes_one_page_per_visible_command() {
        let tmp = std::env::temp_dir().join(format!("stems-man-{}", std::process::id()));
        let pages = super::write_pages(&tmp).unwrap();
        assert!(pages.contains(&"stems.1".to_string()));
        assert!(pages.contains(&"stems-up.1".to_string()));
        assert!(pages.contains(&"stems-daemon-start.1".to_string()));
        assert!(!pages.iter().any(|p| p.contains("__")), "{pages:?}");
        let root = std::fs::read_to_string(tmp.join("stems.1")).unwrap();
        assert!(root.contains(".TH stems 1"), "{root}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
