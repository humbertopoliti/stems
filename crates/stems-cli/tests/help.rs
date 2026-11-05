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
        "stems compose",
        "stems shell",
        "stems reset",
        "stems build",
        "stems pull",
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
        "stems env",
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
        "stems features",
        "stems docs",
    ] {
        assert!(names.contains(&want), "missing {want}");
    }
    for (path, help) in &commands {
        assert!(!help.trim().is_empty(), "{path} has no help");
    }
}

#[test]
fn root_help_is_grouped_and_routes() {
    let mut cmd = stems_cli::cli::command();
    let help = cmd.render_long_help().to_string();
    for (heading, names) in stems_cli::help::GROUPS {
        assert!(
            help.contains(&format!("\n{heading}:\n")),
            "{heading} missing"
        );
        for n in *names {
            assert!(help.contains(&format!("\n  {n} ")), "{n} missing");
        }
    }
    assert!(!help.contains("\nCommands:\n"));
    assert!(help.contains("stems features"));
    assert!(help.contains("stems docs"));
}

#[test]
fn subcommand_long_help_ends_with_see_also() {
    for (path, help) in stems_cli::docs::visible_commands() {
        if path == "stems" || stems_cli::help::NOT_IMPLEMENTED.contains(&path.as_str()) {
            continue;
        }
        let last = help.trim_end().rsplit("\n\n").next().unwrap_or("");
        assert!(last.starts_with("See also: "), "{path}: {last}");
    }
}
