//! docs/cli.md must match `stems __docs` (the clap tree). Regenerate with
//! `make docs` or `UPDATE_DOCS=1 cargo test -p stems-cli --test docs`.

use std::path::Path;

#[test]
fn cli_reference_is_up_to_date() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/cli.md");
    let want = stems_cli::docs::markdown();
    if std::env::var_os("UPDATE_DOCS").is_some() {
        std::fs::write(&path, &want).unwrap();
        return;
    }
    let got = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "docs/cli.md is out of date with the clap definitions; run `make docs` \
         (or UPDATE_DOCS=1 cargo test -p stems-cli --test docs) and review the diff"
    );
}

#[test]
fn docs_subcommand_prints_the_same_markdown() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_stems"))
        .arg("__docs")
        .env_remove("STEMS_NO_COLOR")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        stems_cli::docs::markdown()
    );
}
