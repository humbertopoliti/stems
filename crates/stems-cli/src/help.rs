//! Generated help (36, 50). Everything here is derived from two tables so
//! the help cannot drift from what stems does:
//!
//! * `stems_config::lifecycle::HOOKS`: the hook names under `stems --help`
//!   and the hook table under the long help of `run`, `scripts`, `build`
//!   and `reset`;
//! * `stems_config::features::FEATURES` (re-exported here): the grouped
//!   command list of `stems --help` ([`GROUPS`]), its "Where to look" block,
//!   the **See also** line every subcommand's `--help` ends with, `stems
//!   features` and `docs/features.md`.
//!
//! [`decorate`] attaches all of it to the clap tree (`crate::cli::command`).
//! Golden-tested through `tests/help.rs` and `docs/cli.md`; the integrity
//! tests below check every `commands` entry is a real subcommand and every
//! `doc` anchor a real heading.

use clap::Command;
use clap::builder::PossibleValuesParser;
use stems_config::lifecycle::{HOOKS, HookScope, summary};

pub use stems_config::features::{AREAS, Area, CONFIG_KEYS, FEATURES, Feature};

/// The headings of `stems --help` and the top-level commands under each, in
/// order. Every visible top-level command is in exactly one group (tested).
pub const GROUPS: &[(&str, &[&str])] = &[
    (
        "Lifecycle",
        &[
            "up", "down", "start", "stop", "restart", "build", "pull", "watch",
        ],
    ),
    (
        "Inspect",
        &["status", "logs", "events", "metrics", "health", "graph"],
    ),
    (
        "Scripts & context",
        &[
            "run", "scripts", "exec", "shell", "compose", "reset", "stamps", "overlays",
        ],
    ),
    (
        "Config",
        &[
            "init",
            "validate",
            "show",
            "switch",
            "configuration",
            "profiles",
            "config",
            "add",
            "remove",
            "edit",
        ],
    ),
    ("Environment & ports", &["env", "ports", "outputs"]),
    ("Doctor & repos", &["doctor", "repos"]),
    ("Dashboard & agents", &["attach", "mcp", "features", "docs"]),
    ("Maintenance", &["daemon", "upgrade", "completions"]),
];

/// Commands that exist in the CLI surface but are not implemented yet
/// (`NOT_IMPLEMENTED`): they are not features, so they get no See also.
pub const NOT_IMPLEMENTED: &[&str] = &["stems add", "stems remove", "stems edit"];

/// Commands whose long help also lists the lifecycle hooks.
const HOOK_COMMANDS: &[&str] = &["stems run", "stems scripts", "stems build", "stems reset"];

/// Width the generated text is wrapped to (the clap `term_width`).
const WIDTH: usize = 100;

/// The lifecycle-hooks paragraph (part of the `stems --help` footer).
fn hooks_paragraph() -> String {
    format!(
        "Lifecycle hooks (fixed `scripts:` names stems runs itself): {}.\n\
         Any other script name is a custom script (`stems run`). `stems scripts --lifecycle`\n\
         lists every hook with when it runs and who defines it.",
        summary()
    )
}

/// The footer of `stems --help`: where to look next, then the hooks.
pub fn after_help() -> String {
    format!(
        "Where to look:\n  \
         stems features [<area>]   everything stems can do, by area, with commands, config keys\n  \
         \x20                         and docs (`--json` for agents; docs/features.md)\n  \
         stems docs [<topic>]      list the reference pages, or print one\n  \
         stems <command> --help    the details; ends with related commands and docs\n\n{}",
        hooks_paragraph()
    )
}

/// The hook table under the long help of `run`, `scripts`, `build`, `reset`.
pub fn hooks_long_help() -> String {
    let width = HOOKS.iter().map(|h| h.name.len()).max().unwrap_or(0);
    let mut s = String::from(
        "Lifecycle hooks, in lifecycle order (every other `scripts:` name is a custom script):\n",
    );
    for h in HOOKS {
        let scope = match h.scope {
            HookScope::Stem => "",
            HookScope::Workspace => "(workspace) ",
        };
        s.push_str(&format!("  {:<width$}  {scope}{}\n", h.name, h.when));
    }
    s.push_str(
        "`stems scripts --lifecycle` prints this table with the stems that define each hook.",
    );
    s
}

/// Does feature command `c` belong to command `path` (itself, or one of its
/// subcommands when `path` is a parent such as `stems repos`)?
fn covers(c: &str, path: &str) -> bool {
    c == path
        || c.strip_prefix(path)
            .is_some_and(|rest| rest.starts_with(' '))
}

/// The features that use command `path`, the most specific first: those
/// listing it earliest in their `commands` (a feature lists its main
/// command first), then in table order.
pub fn features_of(path: &str) -> Vec<&'static Feature> {
    let mut v: Vec<(usize, usize, &'static Feature)> = FEATURES
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            f.commands
                .iter()
                .position(|c| covers(c, path))
                .map(|pos| (pos, i, f))
        })
        .collect();
    v.sort_by_key(|(pos, i, _)| (*pos, *i));
    v.into_iter().map(|(_, _, f)| f).collect()
}

/// At most this many related commands / doc links in a See also line.
const MAX_RELATED: usize = 5;
const MAX_DOCS: usize = 3;

/// The See also line of `path` (`stems up`): related commands from the
/// same features, their doc pages and the feature area of the closest
/// feature. `None` when no feature names the command.
pub fn see_also(path: &str) -> Option<String> {
    let feats = features_of(path);
    let first = feats.first()?;
    let mut related: Vec<&str> = Vec::new();
    let mut docs: Vec<&str> = Vec::new();
    for f in &feats {
        for c in f.commands {
            // Neither the command itself, its parent nor its children.
            if covers(c, path) || covers(path, c) || related.contains(c) {
                continue;
            }
            related.push(c);
        }
        if !docs.contains(&f.doc) {
            docs.push(f.doc);
        }
    }
    related.truncate(MAX_RELATED);
    docs.truncate(MAX_DOCS);
    let mut parts = Vec::new();
    if !related.is_empty() {
        parts.push(
            related
                .iter()
                .map(|c| format!("`{c}`"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    parts.push(docs.join(", "));
    parts.push(format!("`stems features {}`", first.area));
    Some(format!("See also: {}.", parts.join("; ")))
}

/// Attach the generated help to the clap tree: the grouped command list and
/// footer of `stems --help`, and the long-help footer (hooks, See also) of
/// every visible subcommand. Call before `build()`.
pub fn decorate(cmd: Command) -> Command {
    let template = root_template(&cmd);
    let cmd = decorate_children(cmd, "stems");
    cmd.help_template(template).after_help(after_help())
}

fn decorate_children(mut cmd: Command, path: &str) -> Command {
    let names: Vec<String> = cmd
        .get_subcommands()
        .filter(|s| !s.is_hide_set())
        .map(|s| s.get_name().to_string())
        .collect();
    for name in names {
        let sub_path = format!("{path} {name}");
        cmd = cmd.mut_subcommand(&name, |sub| {
            let sub = decorate_children(sub, &sub_path);
            let mut footer: Vec<String> = Vec::new();
            if HOOK_COMMANDS.contains(&sub_path.as_str()) {
                footer.push(hooks_long_help());
            }
            if let Some(s) = see_also(&sub_path) {
                footer.push(s);
            }
            if footer.is_empty() {
                sub
            } else {
                sub.after_long_help(footer.join("\n\n"))
            }
        });
    }
    cmd
}

/// The `stems --help` template: clap's default layout with the
/// subcommands grouped under [`GROUPS`] headings (clap has no per-subcommand
/// headings) and every option under one `Options:` heading.
fn root_template(cmd: &Command) -> String {
    let subs: Vec<(&str, String)> = cmd
        .get_subcommands()
        .filter(|s| !s.is_hide_set())
        .map(|s| {
            let about = s.get_about().map(|a| a.to_string()).unwrap_or_default();
            (s.get_name(), about)
        })
        .collect();
    let help_about = "Print this message or the help of the given subcommand(s)";
    let width = subs
        .iter()
        .map(|(n, _)| n.len())
        .chain(std::iter::once("help".len()))
        .max()
        .unwrap_or(0);
    let mut groups = String::new();
    for (i, (heading, names)) in GROUPS.iter().enumerate() {
        if i > 0 {
            groups.push('\n');
        }
        groups.push_str(&format!("{heading}:\n"));
        let mut rows: Vec<(&str, &str)> = names
            .iter()
            .filter_map(|n| subs.iter().find(|(s, _)| s == n))
            .map(|(n, a)| (*n, a.as_str()))
            .collect();
        if i + 1 == GROUPS.len() {
            rows.push(("help", help_about));
        }
        for (name, about) in rows {
            let indent = 2 + width + 2;
            let lines = wrap(about, WIDTH - indent);
            for (j, line) in lines.iter().enumerate() {
                if j == 0 {
                    groups.push_str(&format!("  {name:<width$}  {line}\n"));
                } else {
                    groups.push_str(&format!("{:indent$}{line}\n", ""));
                }
            }
        }
    }
    // Clap's template language uses `{…}`; the abouts contain none (tested).
    format!(
        "{{before-help}}{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{groups}\n\
         Options:\n{{options}}{{after-help}}"
    )
}

/// Greedy word wrap.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() || lines.is_empty() {
        lines.push(cur);
    }
    lines
}

/// The value parser of `stems features <area>`.
pub fn area_parser() -> PossibleValuesParser {
    PossibleValuesParser::new(AREAS.iter().map(|a| a.id))
}

/// `stems features` as human text, optionally one area.
pub fn features_text(area: Option<&str>) -> String {
    let mut out = String::new();
    for a in AREAS.iter().filter(|a| area.is_none_or(|x| a.id == x)) {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("{} (stems features {})\n", a.title, a.id));
        for f in FEATURES.iter().filter(|f| f.area == a.id) {
            let first = format!("{} — {}", f.name, f.one_liner);
            for (i, line) in wrap(&first, WIDTH - 4).iter().enumerate() {
                let pad = if i == 0 { "  " } else { "    " };
                out.push_str(&format!("{pad}{line}\n"));
            }
            if !f.commands.is_empty() {
                out.push_str(&format!("    commands: {}\n", f.commands.join(", ")));
            }
            if !f.config_keys.is_empty() {
                out.push_str(&format!("    config:   {}\n", f.config_keys.join(", ")));
            }
            out.push_str(&format!("    docs:     {}\n", f.doc));
        }
    }
    out.push_str(
        "\n`stems features <area>` narrows the list, `--json` gives the table, \
         `stems docs <topic>` prints a page.\n",
    );
    out
}

/// `docs/features.md` ("What stems can do"), generated by `make docs`.
pub fn features_markdown() -> String {
    let cell = |s: &str| s.replace('|', "\\|");
    let mut out = String::from(
        "# What stems can do\n\n\
         <!-- Generated by `make docs` (`stems __docs features`) from the feature index\n\
         in crates/stems-config/src/features.rs. Do not edit by hand:\n\
         crates/stems-cli/tests/docs.rs fails when this file drifts;\n\
         `UPDATE_DOCS=1 cargo test -p stems-cli --test docs` rewrites it. -->\n\n\
         Every capability of stems, by area: what it does, the commands that use it,\n\
         the `stems.yaml` keys that configure it (`*` is any stem, list item or script\n\
         name) and the page that documents it. The same table is printed by\n\
         `stems features` (`--json` for agents, `stems features <area>` for one area)\n\
         and served to MCP clients as `stems://features`; every `stems <command> --help`\n\
         ends with a \"See also\" line drawn from it.\n\n",
    );
    let toc: Vec<String> = AREAS
        .iter()
        .map(|a| format!("[{}](#{})", a.title, crate::docs::slug(a.title)))
        .collect();
    out.push_str(&toc.join(" · "));
    out.push('\n');
    for a in AREAS {
        out.push_str(&format!(
            "\n## {}\n\n`stems features {}`\n\n| Feature | What it does | Commands | Config keys |\n|---|---|---|---|\n",
            a.title, a.id
        ));
        for f in FEATURES.iter().filter(|f| f.area == a.id) {
            let link = f.doc.trim_start_matches("docs/");
            let code = |v: &[&str]| {
                if v.is_empty() {
                    "-".to_string()
                } else {
                    v.iter()
                        .map(|c| format!("`{c}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            };
            out.push_str(&format!(
                "| [{}]({link}) | {} | {} | {} |\n",
                cell(f.name),
                cell(f.one_liner),
                code(f.commands),
                code(f.config_keys),
            ));
        }
    }
    out.push_str(&format!(
        "\n## Top-level keys of `stems.yaml`\n\n\
         The comment block of a `stems init` template lists the same keys.\n\n\
         | Key | Purpose |\n|---|---|\n{}",
        CONFIG_KEYS
            .iter()
            .map(|(k, p)| format!("| `{k}` | {} |\n", cell(p)))
            .collect::<String>()
    ));
    out.push_str(
        "\nSee also: [Documentation index](README.md) · [Getting started](getting-started.md) · [CLI reference](cli.md)\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn every_hook_is_named_in_both_texts() {
        for h in HOOKS {
            assert!(after_help().contains(h.name), "{} missing", h.name);
            assert!(hooks_long_help().contains(h.name), "{} missing", h.name);
            assert!(
                hooks_long_help().contains(h.when),
                "{} when missing",
                h.name
            );
        }
        assert!(after_help().contains("stems scripts --lifecycle"));
        assert!(after_help().contains("stems features"));
        assert!(after_help().contains("stems docs"));
        insta::assert_snapshot!("help-after-help", after_help());
        insta::assert_snapshot!("help-hooks-long-help", hooks_long_help());
    }

    fn command_paths() -> HashSet<String> {
        crate::docs::visible_commands()
            .into_iter()
            .map(|(p, _)| p)
            .collect()
    }

    #[test]
    fn every_feature_command_is_a_real_subcommand() {
        let paths = command_paths();
        for f in FEATURES {
            for c in f.commands {
                assert!(paths.contains(*c), "{}: `{c}` is not a command", f.name);
            }
        }
    }

    #[test]
    fn every_command_is_a_feature_or_not_implemented() {
        for p in command_paths() {
            if p == "stems" {
                continue;
            }
            let documented = see_also(&p).is_some();
            assert!(
                documented != NOT_IMPLEMENTED.contains(&p.as_str()),
                "`{p}`: every implemented command needs a feature (and only those)"
            );
        }
    }

    #[test]
    fn every_doc_link_points_at_a_real_heading() {
        for f in FEATURES {
            let (page, anchor) = f.doc.split_once('#').unwrap_or((f.doc, ""));
            let name = page
                .strip_prefix("docs/")
                .and_then(|p| p.strip_suffix(".md"))
                .unwrap_or_else(|| panic!("{}: bad doc path {}", f.name, f.doc));
            let text =
                crate::docs::page(name).unwrap_or_else(|| panic!("{}: no page {page}", f.name));
            if !anchor.is_empty() {
                let slugs = crate::docs::heading_slugs(text);
                assert!(
                    slugs.iter().any(|s| s == anchor),
                    "{}: no heading `#{anchor}` in {page}",
                    f.name
                );
            }
        }
    }

    #[test]
    fn groups_cover_every_top_level_command_once() {
        let cmd = crate::cli::command();
        let mut seen = HashSet::new();
        for (_, names) in GROUPS {
            for n in *names {
                assert!(seen.insert(*n), "{n} is in two groups");
                assert!(cmd.find_subcommand(n).is_some(), "{n} is not a command");
            }
        }
        for s in cmd.get_subcommands() {
            if s.is_hide_set() || s.get_name() == "help" {
                continue;
            }
            assert!(seen.contains(s.get_name()), "{} has no group", s.get_name());
            let about = s.get_about().map(|a| a.to_string()).unwrap_or_default();
            assert!(
                !about.contains('{'),
                "{}: braces break the template",
                s.get_name()
            );
        }
    }

    #[test]
    fn see_also_names_related_commands_and_docs() {
        let s = see_also("stems exec").unwrap();
        assert!(s.contains("`stems shell`"), "{s}");
        assert!(s.contains("docs/exec.md"), "{s}");
        assert!(s.contains("`stems features env`"), "{s}");
        assert!(!s.contains("`stems exec`"), "{s}");
        let r = see_also("stems repos").unwrap();
        assert!(r.contains("docs/repos.md"), "{r}");
        assert!(see_also("stems add").is_none());
    }

    #[test]
    fn features_text_golden() {
        insta::assert_snapshot!("features-text-repos", features_text(Some("repos")));
        let all = features_text(None);
        for f in FEATURES {
            assert!(all.contains(f.doc), "{}", f.name);
        }
    }

    #[test]
    fn wraps_words() {
        assert_eq!(wrap("a bb ccc", 4), ["a bb", "ccc"]);
        assert_eq!(wrap("", 4), [""]);
    }

    #[test]
    fn every_error_code_has_a_meaning() {
        for c in stems_core::ErrorCode::all() {
            assert!(
                !c.meaning().trim().is_empty(),
                "{} has no meaning",
                c.as_str()
            );
        }
    }
}
