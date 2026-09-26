//! `--help` goldens for `stems` and every visible subcommand (insta), so any
//! change to the CLI surface is reviewed deliberately.

#[test]
fn help_for_every_command() {
    let commands = stems_cli::docs::visible_commands();
    assert!(commands.len() > 40, "only {} commands", commands.len());
    for (path, help) in commands {
        let name = path.replace(' ', "_");
        insta::assert_snapshot!(name, help);
    }
}

#[test]
fn every_later_command_is_a_stub_with_help() {
    let commands = stems_cli::docs::visible_commands();
    let names: Vec<&str> = commands.iter().map(|(p, _)| p.as_str()).collect();
    for want in [
        "stems up",
        "stems down",
        "stems start",
        "stems stop",
        "stems restart",
        "stems attach",
        "stems status",
        "stems logs",
        "stems events",
        "stems metrics",
        "stems health",
        "stems graph",
        "stems run",
        "stems scripts",
        "stems exec",
        "stems shell",
        "stems reset",
        "stems build",
        "stems stamps",
        "stems overlays",
        "stems doctor",
        "stems repos sync",
        "stems repos status",
        "stems watch pause",
        "stems watch resume",
        "stems watch status",
        "stems profiles",
        "stems outputs",
        "stems config get",
        "stems config set",
        "stems config unset",
        "stems config diff",
        "stems config apply",
        "stems add",
        "stems remove",
        "stems edit",
        "stems mcp",
        "stems upgrade",
        "stems daemon start",
        "stems daemon stop",
        "stems daemon status",
    ] {
        assert!(names.contains(&want), "missing {want}");
    }
    for (path, help) in &commands {
        assert!(!help.trim().is_empty(), "{path} has no help");
    }
}
