//! The feature index (deliverable 50): every capability of stems in one
//! table, so `stems features`, `docs/features.md`, the `See also` lines of
//! `--help`, the `stems init` template and the MCP `stems://features`
//! resource are all generated from the same data and cannot drift.
//!
//! It lives here (not in the CLI crate) so the MCP server can serve it too.
//! `stems_cli::help` re-exports it and owns the integrity tests that need the
//! clap tree (every `commands` entry is a real subcommand path) and the docs
//! (every `doc` anchor is a heading slug of that page); the schema test
//! (every `config_keys` path exists in `schema/stems.schema.json`) lives
//! below.

use serde::Serialize;

/// A group of features (`stems features <area>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Area {
    /// Short id, the argument of `stems features <area>`.
    pub id: &'static str,
    /// Heading.
    pub title: &'static str,
}

/// One capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Feature {
    /// [`Area::id`] this feature belongs to.
    pub area: &'static str,
    /// Short name.
    pub name: &'static str,
    /// One sentence: what it does for you.
    pub one_liner: &'static str,
    /// The commands that use it (full paths, `stems repos sync`).
    pub commands: &'static [&'static str],
    /// The config keys that configure it, as schema paths: `profiles`,
    /// `stems.*.health` (`*` is any stem name, list item or script name).
    pub config_keys: &'static [&'static str],
    /// The reference: `docs/<page>.md` or `docs/<page>.md#<heading-slug>`.
    pub doc: &'static str,
}

/// The areas, in the order `stems features` and `docs/features.md` print them.
pub const AREAS: &[Area] = &[
    Area {
        id: "lifecycle",
        title: "Running the environment",
    },
    Area {
        id: "config",
        title: "Configuring a workspace",
    },
    Area {
        id: "env",
        title: "Environment, ports and outputs",
    },
    Area {
        id: "scripts",
        title: "Scripts, hooks and overlays",
    },
    Area {
        id: "containers",
        title: "Docker and compose",
    },
    Area {
        id: "supervision",
        title: "Health and supervision",
    },
    Area {
        id: "observe",
        title: "Logs, events, metrics and the dashboard",
    },
    Area {
        id: "doctor",
        title: "Doctor, requirements and checks",
    },
    Area {
        id: "repos",
        title: "Code from git",
    },
    Area {
        id: "agents",
        title: "Agents and automation",
    },
    Area {
        id: "maintenance",
        title: "Install and maintenance",
    },
];

/// Every capability of stems.
pub const FEATURES: &[Feature] = &[
    // --- lifecycle --------------------------------------------------------
    Feature {
        area: "lifecycle",
        name: "Start and stop",
        one_liner: "Start everything, or some stems and their dependencies, in dependency order (attached or detached); stop in reverse order.",
        commands: &[
            "stems up",
            "stems down",
            "stems start",
            "stems stop",
            "stems restart",
        ],
        config_keys: &["stems", "stems.*.stop_grace"],
        doc: "docs/lifecycle.md#commands",
    },
    Feature {
        area: "lifecycle",
        name: "Stem types",
        one_liner: "Local processes, docker containers, compose services, and external endpoints stems only health-checks.",
        commands: &["stems up", "stems show"],
        config_keys: &[
            "stems.*.type",
            "stems.*.command",
            "stems.*.cwd",
            "stems.*.shell",
        ],
        doc: "docs/getting-started.md#4-add-your-services",
    },
    Feature {
        area: "lifecycle",
        name: "Dependency graph",
        one_liner: "`depends_on` with readiness conditions (started, healthy, seeded) and soft edges; `stems graph` draws it or exports Mermaid, DOT or JSON.",
        commands: &["stems graph", "stems up"],
        config_keys: &[
            "stems.*.depends_on",
            "stems.*.depends_on.*.condition",
            "stems.*.depends_on.*.soft",
        ],
        doc: "docs/lifecycle.md#readiness",
    },
    Feature {
        area: "lifecycle",
        name: "Cascading restarts",
        one_liner: "Restarting a stem can restart its dependants, edge by edge (`protocol`, `cascade`).",
        commands: &["stems restart"],
        config_keys: &["stems.*.restart.cascade", "stems.*.depends_on.*.protocol"],
        doc: "docs/restart.md#cascading-restarts-fr-lc-9",
    },
    Feature {
        area: "lifecycle",
        name: "Disabled stems",
        one_liner: "`enabled: false` or `enabled: \"${env.X}\"` keeps a stem out of `up`; it shows as disabled and its dependants start without it (`if_disabled: skip` skips them).",
        commands: &["stems status", "stems show"],
        config_keys: &["stems.*.enabled", "stems.*.depends_on.*.if_disabled"],
        doc: "docs/config.md#enabled",
    },
    Feature {
        area: "lifecycle",
        name: "Crash recovery and orphans",
        one_liner: "A restarted daemon re-adopts the stems still running; leftovers of a crashed run are found and stopped or adopted.",
        commands: &["stems up", "stems down", "stems doctor"],
        config_keys: &[],
        doc: "docs/recovery.md",
    },
    Feature {
        area: "lifecycle",
        name: "Process supervision",
        one_liner: "Every process runs in its own process group; stops signal the whole group and pids are verified against reuse.",
        commands: &["stems stop", "stems down"],
        config_keys: &["stems.*.stop_grace", "stems.*.stdin"],
        doc: "docs/process-model.md",
    },
    // --- config -----------------------------------------------------------
    Feature {
        area: "config",
        name: "Scaffold and validate",
        one_liner: "`stems init` scaffolds an integration repo (`--from` an example); `stems validate` reports every config error with its file and line.",
        commands: &["stems init", "stems validate"],
        config_keys: &["schema_version", "name"],
        doc: "docs/getting-started.md#1-create-an-integration-repo",
    },
    Feature {
        area: "config",
        name: "Resolved config",
        one_liner: "`stems show` prints the merged, substituted config (`--effective` adds the defaults applied; secrets redacted).",
        commands: &["stems show"],
        config_keys: &["vars", "stems.*.description", "stems.*.tags"],
        doc: "docs/config.md",
    },
    Feature {
        area: "config",
        name: "Profiles",
        one_liner: "Named subsets of stems (`stems up --profile backend`), a default profile and strict mode.",
        commands: &["stems profiles", "stems up"],
        config_keys: &["profiles", "default_profile", "strict_profiles", "profile"],
        doc: "docs/config.md#profiles-fr-ws-8",
    },
    Feature {
        area: "config",
        name: "Variants",
        one_liner: "Alternative forms of a stem (a local process or a container); `stems switch` picks one, `stems show --variant` shows where each field comes from.",
        commands: &["stems switch", "stems show"],
        config_keys: &["stems.*.variants", "stems.*.variant", "stems.*.inherit"],
        doc: "docs/config.md#variants-fr-st-8",
    },
    Feature {
        area: "config",
        name: "Configurations",
        one_liner: "Named overlays with the shape of stems.local.yaml (variants, disabled stems, env, profile) a team can share and select.",
        commands: &[
            "stems configuration",
            "stems configuration list",
            "stems configuration use",
            "stems configuration save",
        ],
        config_keys: &["configurations", "default_configuration", "configuration"],
        doc: "docs/config.md#configurations",
    },
    Feature {
        area: "config",
        name: "Local overrides",
        one_liner: "Per-developer changes in a gitignored stems.local.yaml, edited by hand or with `stems config set` (comments kept).",
        commands: &["stems config get", "stems config set", "stems config unset"],
        config_keys: &[],
        doc: "docs/config.md#stems-config-getsetunset-fr-cl-5",
    },
    Feature {
        area: "config",
        name: "Include and extends",
        one_liner: "Split a workspace over several files, or build on a shared base file.",
        commands: &["stems show"],
        config_keys: &["include", "extends"],
        doc: "docs/config.md#include--extends-fr-ws-9",
    },
    Feature {
        area: "config",
        name: "Reload on change",
        one_liner: "The daemon notices config edits and plans what to restart; `stems config diff` shows the plan, `stems config apply` runs it.",
        commands: &["stems config diff", "stems config apply"],
        config_keys: &["config", "config.reload.auto_apply"],
        doc: "docs/config.md#reload-on-change-fr-wd-3",
    },
    // --- env --------------------------------------------------------------
    Feature {
        area: "env",
        name: "Environment and env files",
        one_liner: "Workspace and stem `env`, `env_files` (optional with `required: false`), a documented precedence and a warning when a value is shadowed.",
        commands: &["stems env", "stems show"],
        config_keys: &[
            "env",
            "stems.*.env",
            "stems.*.env_files",
            "stems.*.env_files.*.required",
        ],
        doc: "docs/config.md#environment-fr-st-4",
    },
    Feature {
        area: "env",
        name: "Secrets and redaction",
        one_liner: "Secret env values are redacted in `show`, `env`, logs, exports and MCP; name patterns are adjustable.",
        commands: &["stems show", "stems env", "stems logs"],
        config_keys: &["secrets", "redaction", "stems.*.secrets"],
        doc: "docs/config.md#secrets",
    },
    Feature {
        area: "env",
        name: "Ports",
        one_liner: "Named host ports, `auto` allocation, conflict checks and `${stem.<name>.port.<port>}` variables; `stems ports` lists them all.",
        commands: &["stems ports"],
        config_keys: &["stems.*.ports", "stems.*.ports.*.name"],
        doc: "docs/ports.md",
    },
    Feature {
        area: "env",
        name: "Outputs",
        one_liner: "Values a stem publishes (static templates or commands, optionally secret) for other stems to consume.",
        commands: &["stems outputs"],
        config_keys: &["stems.*.outputs"],
        doc: "docs/config.md#outputs-fr-st-6",
    },
    Feature {
        area: "env",
        name: "Commands in a stem's context",
        one_liner: "`stems exec` runs a command with a stem's env and cwd (or in its container), `stems shell` opens a shell there, `stems env` prints the env.",
        commands: &["stems exec", "stems shell", "stems env"],
        config_keys: &["stems.*.scripts.*.inherit_env"],
        doc: "docs/exec.md",
    },
    // --- scripts ----------------------------------------------------------
    Feature {
        area: "scripts",
        name: "Lifecycle hooks",
        one_liner: "Fixed `scripts:` names stems runs itself (setup, seed, pre_start, post_stop, ...) in lifecycle order; `stems scripts --lifecycle` lists them.",
        commands: &["stems scripts", "stems run"],
        config_keys: &["scripts", "stems.*.scripts"],
        doc: "docs/scripts.md#the-lifecycle-order",
    },
    Feature {
        area: "scripts",
        name: "Custom scripts",
        one_liner: "Any other script name, run with `stems run` (typed arguments, preconditions such as `requires_stopped`, timeouts, retries).",
        commands: &["stems run", "stems scripts"],
        config_keys: &[
            "stems.*.scripts.*.args",
            "stems.*.scripts.*.requires",
            "stems.*.scripts.*.requires_stopped",
            "stems.*.scripts.*.timeout",
        ],
        doc: "docs/scripts.md#custom-scripts-and-stems-run",
    },
    Feature {
        area: "scripts",
        name: "Stamps",
        one_liner: "`setup` and `seed` run once until their inputs change; `stems stamps` shows or clears the stamps, `stems reset` starts over.",
        commands: &["stems stamps", "stems reset"],
        config_keys: &["stems.*.scripts.*.inputs", "stems.*.scripts.*.stamp_env"],
        doc: "docs/scripts.md#stamps-fr-sc-5",
    },
    Feature {
        area: "scripts",
        name: "Soft-failing setup",
        one_liner: "`soft_fail: true` lets a stem start degraded when an optional setup step fails.",
        commands: &["stems status"],
        config_keys: &["stems.*.scripts.*.soft_fail"],
        doc: "docs/scripts.md#partial-installs-soft_fail",
    },
    Feature {
        area: "scripts",
        name: "Builds",
        one_liner: "`stems build` runs build scripts or docker builds; `when:` puts a build into the start sequence.",
        commands: &["stems build", "stems restart"],
        config_keys: &["stems.*.scripts.*.when", "stems.*.build"],
        doc: "docs/scripts.md#build-in-the-start-sequence-when",
    },
    Feature {
        area: "scripts",
        name: "Overlays",
        one_liner: "Templated files stems writes into a codebase before start (never into tracked files); `stems overlays render` renders them on demand.",
        commands: &["stems overlays", "stems overlays render"],
        config_keys: &["stems.*.overlays"],
        doc: "docs/overlays.md",
    },
    // --- containers -------------------------------------------------------
    Feature {
        area: "containers",
        name: "Docker stems",
        one_liner: "Containers named and labelled per workspace, registry credentials from `docker login`, adopted after a daemon restart; `stems pull` refreshes images.",
        commands: &["stems pull", "stems up"],
        config_keys: &[
            "stems.*.image",
            "stems.*.pull",
            "stems.*.volumes",
            "stems.*.network",
            "stems.*.entrypoint",
            "stems.*.labels",
            "stems.*.healthcheck",
        ],
        doc: "docs/docker.md",
    },
    Feature {
        area: "containers",
        name: "Docker builds",
        one_liner: "Build images from a context and Dockerfile, with build secrets and paths outside the context.",
        commands: &["stems build"],
        config_keys: &[
            "stems.*.build.context",
            "stems.*.build.dockerfile",
            "stems.*.build.secrets",
            "stems.*.build.allow_read",
        ],
        doc: "docs/docker.md#build-secrets-and-paths-outside-the-context-buildx",
    },
    Feature {
        area: "containers",
        name: "Compose stems",
        one_liner: "One service of a compose file as a stem: built by default, stale images reported, the stem's env injected with `inject_env`.",
        commands: &["stems up", "stems doctor"],
        config_keys: &[
            "stems.*.file",
            "stems.*.service",
            "stems.*.project_name",
            "stems.*.adopt",
            "stems.*.inject_env",
            "stems.*.build.services",
        ],
        doc: "docs/compose.md",
    },
    Feature {
        area: "containers",
        name: "Compose passthrough",
        one_liner: "`stems compose <stem> -- <args>` runs `docker compose` with the stem's file, project and env (migrations, `run --rm`, `ps`).",
        commands: &["stems compose"],
        config_keys: &[],
        doc: "docs/compose.md",
    },
    // --- supervision ------------------------------------------------------
    Feature {
        area: "supervision",
        name: "Health checks",
        one_liner: "HTTP, TCP and command probes with retries, start periods and flapping detection; `stems health` shows the last results.",
        commands: &["stems health", "stems status"],
        config_keys: &["stems.*.health"],
        doc: "docs/health.md#probe-types",
    },
    Feature {
        area: "supervision",
        name: "Status",
        one_liner: "One status model for every stem (healthy, degraded, failed, ...); `stems status --expect` waits for a state in scripts.",
        commands: &["stems status"],
        config_keys: &[],
        doc: "docs/lifecycle.md#checking-states-in-scripts-status---expect",
    },
    Feature {
        area: "supervision",
        name: "Restart policies",
        one_liner: "Restart on failure with backoff, a restart window and `on_unhealthy`; the output tail of a failure is kept.",
        commands: &["stems status", "stems restart"],
        config_keys: &["stems.*.restart"],
        doc: "docs/restart.md#policies",
    },
    Feature {
        area: "supervision",
        name: "Watchdogs",
        one_liner: "File watchers that restart or rebuild a stem, run a script or signal it on change; pause and resume them.",
        commands: &[
            "stems watch pause",
            "stems watch resume",
            "stems watch status",
        ],
        config_keys: &["stems.*.watch"],
        doc: "docs/watchdogs.md",
    },
    // --- observe ----------------------------------------------------------
    Feature {
        area: "observe",
        name: "Logs",
        one_liner: "Captured stem and script logs, merged or per stem, followed live, with levels detected, rotation and retention.",
        commands: &["stems logs"],
        config_keys: &["logs"],
        doc: "docs/logs.md#stems-logs",
    },
    Feature {
        area: "observe",
        name: "Bug-report bundle",
        one_liner: "`stems logs --export` writes status, events, redacted config and log files into one .tar.gz.",
        commands: &["stems logs"],
        config_keys: &[],
        doc: "docs/logs.md#export-bundle",
    },
    Feature {
        area: "observe",
        name: "Events",
        one_liner: "A structured event log of every state change and action (with its actor); `stems events -f --json` streams it as NDJSON.",
        commands: &["stems events"],
        config_keys: &[],
        doc: "docs/protocol.md#events",
    },
    Feature {
        area: "observe",
        name: "Metrics",
        one_liner: "CPU, memory, children and disk per stem with sparklines and thresholds.",
        commands: &["stems metrics"],
        config_keys: &["metrics", "stems.*.limits"],
        doc: "docs/metrics.md",
    },
    Feature {
        area: "observe",
        name: "Dashboard",
        one_liner: "A terminal dashboard: table, graph, logs, events and scripts views, with every action one key away.",
        commands: &["stems attach", "stems up"],
        config_keys: &[],
        doc: "docs/tui.md",
    },
    Feature {
        area: "observe",
        name: "Desktop notifications",
        one_liner: "One OS notification when stems fail for good (or your own command).",
        commands: &[],
        config_keys: &["notifications"],
        doc: "docs/notifications.md",
    },
    // --- doctor -----------------------------------------------------------
    Feature {
        area: "doctor",
        name: "Doctor",
        one_liner: "Diagnose the machine and the workspace: Docker, tool versions, ports, orphans; `--fix` repairs what it can.",
        commands: &["stems doctor"],
        config_keys: &[],
        doc: "docs/doctor.md#checks",
    },
    Feature {
        area: "doctor",
        name: "Tool requirements",
        one_liner: "`requires:` tool versions per workspace or stem, read from .nvmrc, packageManager or .tool-versions files too.",
        commands: &["stems doctor", "stems validate"],
        config_keys: &["requires", "stems.*.requires"],
        doc: "docs/doctor.md#requires",
    },
    Feature {
        area: "doctor",
        name: "Custom checks",
        one_liner: "Your own `checks:` commands, run by `stems doctor` and before `stems up`.",
        commands: &["stems doctor", "stems up"],
        config_keys: &["checks", "stems.*.checks"],
        doc: "docs/doctor.md#custom-checks",
    },
    // --- repos ------------------------------------------------------------
    Feature {
        area: "repos",
        name: "Git codebases",
        one_liner: "A `codebase:` can be a git URL: stems clones and fetches it; `stems repos status` shows where each one is.",
        commands: &["stems repos sync", "stems repos status"],
        config_keys: &["repos_dir", "stems.*.codebase"],
        doc: "docs/repos.md",
    },
    Feature {
        area: "repos",
        name: "Checking out a ref",
        one_liner: "`stems repos checkout <stem> <ref>` switches a stem's codebase to a branch, tag or commit.",
        commands: &["stems repos checkout"],
        config_keys: &[],
        doc: "docs/repos.md",
    },
    // --- agents -----------------------------------------------------------
    Feature {
        area: "agents",
        name: "MCP server",
        one_liner: "Agents start, stop, inspect and script the environment over MCP; custom scripts become tools, destructive calls are gated.",
        commands: &["stems mcp"],
        config_keys: &["agent"],
        doc: "docs/mcp.md",
    },
    Feature {
        area: "agents",
        name: "JSON everywhere",
        one_liner: "Every command takes `--json` (one envelope, stable error codes and exit codes).",
        commands: &[],
        config_keys: &[],
        doc: "docs/cli.md",
    },
    Feature {
        area: "agents",
        name: "Daemon protocol",
        one_liner: "One daemon per workspace owns the stems; its local JSON-RPC socket is documented for your own tools.",
        commands: &[
            "stems daemon",
            "stems daemon start",
            "stems daemon stop",
            "stems daemon status",
        ],
        config_keys: &[],
        doc: "docs/protocol.md",
    },
    Feature {
        area: "agents",
        name: "Feature index",
        one_liner: "This table: `stems features [--json] [<area>]`; `stems docs [<topic>]` prints the reference pages.",
        commands: &["stems features", "stems docs"],
        config_keys: &[],
        doc: "docs/features.md",
    },
    // --- maintenance ------------------------------------------------------
    Feature {
        area: "maintenance",
        name: "Install and upgrade",
        one_liner: "Homebrew, an installer script or tarballs; `stems upgrade` updates either install.",
        commands: &["stems upgrade"],
        config_keys: &[],
        doc: "docs/install.md#upgrading",
    },
    Feature {
        area: "maintenance",
        name: "Shell completions",
        one_liner: "Completion scripts for bash, zsh, fish and more.",
        commands: &["stems completions"],
        config_keys: &[],
        doc: "docs/install.md",
    },
];

/// Every top-level key of `stems.yaml` with a one-line purpose (the comment
/// block of the `stems init` template). A test keeps it equal to the
/// schema's top-level properties.
pub const CONFIG_KEYS: &[(&str, &str)] = &[
    ("schema_version", "config format version (1)"),
    ("name", "workspace name (default: the directory name)"),
    ("vars", "named values for `${var.<name>}`"),
    ("env", "environment for every stem (a stem's `env` wins)"),
    (
        "requires",
        "tool versions every developer needs (`stems doctor`)",
    ),
    (
        "checks",
        "your own checks, run by `stems doctor` and before `up`",
    ),
    (
        "profiles",
        "named subsets of stems (`stems up --profile <name>`)",
    ),
    ("default_profile", "profile `up` uses when none is given"),
    (
        "strict_profiles",
        "never start stems outside the selected profile",
    ),
    ("profile", "this machine's profile (stems.local.yaml)"),
    ("agent", "guard rails for MCP clients (`stems mcp`)"),
    ("logs", "log rotation and retention"),
    ("metrics", "metrics sampling"),
    (
        "config",
        "how the daemon handles config edits (`config.reload`)",
    ),
    (
        "notifications",
        "desktop notifications when stems fail for good",
    ),
    ("secrets", "env names whose values are secret (redacted)"),
    ("redaction", "adjust the built-in secret name patterns"),
    ("repos_dir", "where git codebases are cloned"),
    (
        "scripts",
        "workspace scripts (`bootstrap`, `teardown`, custom)",
    ),
    ("include", "extra YAML files merged into this one"),
    (
        "extends",
        "a base workspace file merged underneath this one",
    ),
    (
        "configurations",
        "named, shareable overlays (`stems configuration`)",
    ),
    (
        "default_configuration",
        "configuration used when none is selected",
    ),
    (
        "configuration",
        "this machine's configuration (stems.local.yaml)",
    ),
    ("stems", "the stems, keyed by name"),
];

/// The table as the JSON `stems features --json` prints and the MCP
/// `stems://features` resource serves: `{areas, features}`, optionally
/// limited to one area.
pub fn to_json(area: Option<&str>) -> serde_json::Value {
    let areas: Vec<&Area> = AREAS
        .iter()
        .filter(|a| area.is_none_or(|x| a.id == x))
        .collect();
    let features: Vec<&Feature> = FEATURES
        .iter()
        .filter(|f| area.is_none_or(|x| f.area == x))
        .collect();
    serde_json::json!({ "areas": areas, "features": features })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn schema() -> Value {
        serde_json::from_str(include_str!("../../../schema/stems.schema.json")).unwrap()
    }

    /// Follow `$ref`, `anyOf` / `oneOf` / `allOf` to every object schema
    /// `node` can be.
    fn branches<'a>(root: &'a Value, node: &'a Value, out: &mut Vec<&'a Value>) {
        if let Some(r) = node.get("$ref").and_then(Value::as_str) {
            let name = r.trim_start_matches("#/$defs/");
            let target = if r == "#" { root } else { &root["$defs"][name] };
            // `#` (an inline configuration) is the whole schema: do not recurse
            // forever, it is already an object schema.
            if r == "#" {
                out.push(target);
            } else {
                branches(root, target, out);
            }
        }
        for k in ["anyOf", "oneOf", "allOf"] {
            if let Some(v) = node.get(k).and_then(Value::as_array) {
                for b in v {
                    branches(root, b, out);
                }
            }
        }
        out.push(node);
    }

    fn path_exists(root: &Value, path: &str) -> bool {
        let mut nodes = vec![root];
        for seg in path.split('.') {
            let mut next = Vec::new();
            for n in nodes {
                let mut bs = Vec::new();
                branches(root, n, &mut bs);
                for b in bs {
                    if seg == "*" {
                        if let Some(ap) = b.get("additionalProperties").filter(|v| v.is_object()) {
                            next.push(ap);
                        }
                        if let Some(items) = b.get("items") {
                            next.push(items);
                        }
                    } else if let Some(p) = b.get("properties").and_then(|p| p.get(seg)) {
                        next.push(p);
                    }
                }
            }
            if next.is_empty() {
                return false;
            }
            nodes = next;
        }
        true
    }

    #[test]
    fn every_config_key_is_in_the_schema() {
        let s = schema();
        for f in FEATURES {
            for k in f.config_keys {
                assert!(path_exists(&s, k), "{}: `{k}` is not in the schema", f.name);
            }
        }
        assert!(!path_exists(&s, "stems.*.nope"));
        assert!(!path_exists(&s, "nope"));
    }

    #[test]
    fn config_keys_are_the_schema_top_level_keys() {
        let s = schema();
        let want: Vec<&str> = s["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let got: Vec<&str> = CONFIG_KEYS.iter().map(|(k, _)| *k).collect();
        let mut w = want.clone();
        let mut g = got.clone();
        w.sort_unstable();
        g.sort_unstable();
        assert_eq!(g, w, "CONFIG_KEYS must list every top-level schema key");
        for (k, purpose) in CONFIG_KEYS {
            assert!(!purpose.is_empty(), "{k} has no purpose");
        }
    }

    #[test]
    fn every_top_level_key_is_covered_by_a_feature() {
        for (k, _) in CONFIG_KEYS {
            assert!(
                FEATURES.iter().any(|f| f
                    .config_keys
                    .iter()
                    .any(|c| c.split('.').next() == Some(*k))),
                "no feature names the top-level key `{k}`"
            );
        }
    }

    #[test]
    fn areas_and_features_are_consistent() {
        assert!(FEATURES.len() >= 20);
        for a in AREAS {
            assert!(
                FEATURES.iter().any(|f| f.area == a.id),
                "area {} is empty",
                a.id
            );
        }
        let mut names = std::collections::HashSet::new();
        for f in FEATURES {
            assert!(
                AREAS.iter().any(|a| a.id == f.area),
                "{}: unknown area",
                f.name
            );
            assert!(names.insert(f.name), "duplicate feature {}", f.name);
            assert!(
                f.doc.starts_with("docs/") && f.doc.contains(".md"),
                "{}",
                f.name
            );
            assert!(
                !f.one_liner.is_empty() && f.one_liner.ends_with('.'),
                "{}",
                f.name
            );
            for c in f.commands {
                assert!(c.starts_with("stems "), "{}: {c}", f.name);
            }
        }
        // Areas are listed contiguously, in AREAS order.
        let order: Vec<&str> = FEATURES.iter().map(|f| f.area).collect();
        let mut dedup = order.clone();
        dedup.dedup();
        let ids: Vec<&str> = AREAS.iter().map(|a| a.id).collect();
        assert_eq!(dedup, ids);
    }

    #[test]
    fn json_filters_by_area() {
        let all = to_json(None);
        assert_eq!(all["features"].as_array().unwrap().len(), FEATURES.len());
        let one = to_json(Some("repos"));
        assert_eq!(one["areas"].as_array().unwrap().len(), 1);
        assert!(
            one["features"]
                .as_array()
                .unwrap()
                .iter()
                .all(|f| f["area"] == "repos")
        );
    }
}
