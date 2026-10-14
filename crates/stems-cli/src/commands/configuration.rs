//! `stems configuration list|use|save` (`docs/config.md#configurations`).
//!
//! - `list` (the default): the declared configurations, the active one
//!   marked. `data: { active, default, configurations: [{name, source,
//!   file?, active}] }`. Reads the config locally (no daemon).
//! - `use <name>|none [--no-apply]`: writes `configuration: <name>` to
//!   `stems.local.yaml` with the comment-preserving editor (`none` removes
//!   the key, or writes `configuration: none` when `stems.yaml` selects a
//!   default), re-validates the workspace and restores the file if that
//!   introduces errors (exit 2). When the daemon runs and `--no-apply` is
//!   not given, the daemon does all of this (`switch_configuration
//!   {configuration}`) and then applies the change (`config_apply {yes:
//!   true}`). `data: { from, to, path, file, changed, diff, daemon,
//!   applied }` (`applied` is the `ConfigApplyResult`, `null` when nothing
//!   was applied). An unknown name is `UNKNOWN_CONFIGURATION` (exit 2,
//!   nothing written).
//! - `save <name> [--file <path>] [--force]`: snapshots the current
//!   `stems.local.yaml` (minus its `configuration` key, comments kept) into
//!   `<path>` (default `configurations/<name>.yaml`, relative to the
//!   integration repo) and adds `configurations.<name>: <path>` to
//!   `stems.yaml` (comment-preserving; validated, restored on error). It
//!   does not select it. `data: { name, configuration_file, relative,
//!   keys, path, value, file, changed, diff }`.

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use stems_api::{Method, SwitchConfigurationParams, SwitchConfigurationResult};
use stems_config::{ConfigPath, ConfigurationSource, Workspace, configurations, edit};
use stems_core::{Error, ErrorCode, Errors};

use crate::cli::{ConfigurationCommand, ConfigurationSaveArgs, ConfigurationUseArgs};
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::commands::config;
use crate::output::CommandOutput;

/// Run `stems configuration`.
pub fn run(ctx: &Ctx, cmd: Option<&ConfigurationCommand>) -> CommandOutput {
    match cmd {
        None | Some(ConfigurationCommand::List) => list(ctx),
        Some(ConfigurationCommand::Use(a)) => use_(ctx, a),
        Some(ConfigurationCommand::Save(a)) => save(ctx, a),
    }
}

fn load(ctx: &Ctx) -> Result<stems_config::Resolved, Errors> {
    stems_config::load(ctx.load_options()).map_err(|e| {
        let mut errors = Errors::from(e);
        errors.sort();
        errors
    })
}

fn relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .display()
        .to_string()
}

fn row(ws: &Workspace, c: &stems_config::ConfigurationInfo) -> Json {
    let active = ws.configuration.as_deref() == Some(c.name.as_str());
    match &c.source {
        ConfigurationSource::Inline => {
            json!({ "name": c.name, "source": "inline", "active": active })
        }
        ConfigurationSource::File(f) => {
            json!({ "name": c.name, "source": "file", "file": f, "active": active })
        }
    }
}

/// `stems configuration list`.
fn list(ctx: &Ctx) -> CommandOutput {
    let resolved = match load(ctx) {
        Ok(r) => r,
        Err(e) => return CommandOutput::failed(e),
    };
    let ws = &resolved.workspace;
    let rows: Vec<Json> = ws.configurations.iter().map(|c| row(ws, c)).collect();
    let mut human = String::new();
    if ws.configurations.is_empty() {
        human.push_str(
            "no configurations declared (see docs/config.md#configurations; `stems configuration save <name>` creates one from stems.local.yaml)\n",
        );
    }
    let width = ws
        .configurations
        .iter()
        .map(|c| c.name.len())
        .max()
        .unwrap_or(0);
    for c in &ws.configurations {
        let active = ws.configuration.as_deref() == Some(c.name.as_str());
        let mark = if active { "*" } else { " " };
        let (kind, where_) = match &c.source {
            ConfigurationSource::Inline => ("inline", String::new()),
            ConfigurationSource::File(f) => ("file", format!("  {}", relative(&ws.root, f))),
        };
        let default = if ws.default_configuration.as_deref() == Some(c.name.as_str()) {
            "  (default)"
        } else {
            ""
        };
        human.push_str(&format!(
            "{mark} {:<width$}  {kind}{where_}{default}\n",
            c.name
        ));
    }
    if !ws.configurations.is_empty() {
        match &ws.configuration {
            Some(_) => {
                human.push_str("* = active; switch with `stems configuration use <name>|none`\n")
            }
            None => {
                human.push_str("none active; select one with `stems configuration use <name>`\n")
            }
        }
    }
    CommandOutput::data(json!({
        "active": ws.configuration,
        "default": ws.default_configuration,
        "configurations": rows,
    }))
    .with_human(human)
}

fn unknown(ws: &Workspace, name: &str) -> Error {
    let known: Vec<&str> = ws.configurations.iter().map(|c| c.name.as_str()).collect();
    let hint = if known.is_empty() {
        "no `configurations` are declared in stems.yaml (see docs/config.md#configurations, or `stems configuration save <name>`)".to_string()
    } else {
        format!(
            "configurations: {} (or `{}` for the base config)",
            known.join(", "),
            configurations::NONE
        )
    };
    Error::new(
        ErrorCode::UnknownConfiguration,
        format!("no configuration named `{name}`"),
    )
    .with_path(ConfigPath::root().key("configuration"))
    .with_hint(hint)
    .with_details(json!({ "configuration": name, "known": known }))
}

fn label(c: Option<&str>) -> &str {
    c.unwrap_or(configurations::NONE)
}

/// `switch_configuration {configuration}` on the workspace's daemon, which
/// edits `stems.local.yaml`, validates and applies the change. `None` when
/// no daemon runs (the CLI edits the file itself then).
fn via_daemon(ctx: &Ctx, to: &str) -> Result<Option<SwitchConfigurationResult>, Errors> {
    block_on(async {
        let c = match client::connect(ctx).await {
            Ok(c) => c,
            Err(e) if e.0.iter().all(|e| e.code == ErrorCode::DaemonNotRunning) => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let p = SwitchConfigurationParams {
            configuration: Some(to.to_string()),
        };
        let r: SwitchConfigurationResult = c
            .call_with_timeout(Method::SWITCH_CONFIGURATION, &p, config::APPLY_TIMEOUT)
            .await?;
        Ok(Some(r))
    })
}

/// The first lines of the human output: the switch, then the file's diff.
fn head_human(
    from: Option<&str>,
    to: Option<&str>,
    file: Option<&Path>,
    diff: Option<&[Json]>,
) -> String {
    let mut human = if from == to {
        format!("configuration: already `{}`\n", label(to))
    } else {
        format!("configuration: {} -> {}\n", label(from), label(to))
    };
    if let (Some(file), Some(diff)) = (file, diff) {
        human.push_str(&format!("{}:\n", file.display()));
        for l in diff.iter().filter_map(Json::as_str) {
            human.push_str(&format!("  {l}\n"));
        }
    }
    human
}

/// The output of a switch the daemon made (`daemon: true`).
fn daemon_output(r: SwitchConfigurationResult) -> CommandOutput {
    let diff: Vec<Json> = r.diff.iter().map(|l| json!(l)).collect();
    let mut human = head_human(
        r.from.as_deref(),
        r.to.as_deref(),
        r.file.as_deref().filter(|_| r.changed),
        Some(&diff),
    );
    let mut data = serde_json::Map::new();
    data.insert("from".into(), json!(r.from));
    data.insert("to".into(), json!(r.to));
    data.insert("path".into(), json!(r.path));
    data.insert("file".into(), json!(r.file));
    data.insert("changed".into(), json!(r.changed));
    data.insert("diff".into(), json!(r.diff));
    data.insert("daemon".into(), json!(true));
    data.insert(
        "applied".into(),
        r.applied
            .as_ref()
            .and_then(|a| serde_json::to_value(a).ok())
            .unwrap_or(Json::Null),
    );
    let mut errors = Errors::new();
    if let Some(a) = &r.applied {
        human.push_str(&config::apply_human(a));
        for f in &a.failed {
            let mut e = f.error.clone();
            if e.hint.is_none() {
                e.hint = Some(format!(
                    "fix it and run `stems config apply --yes`, or go back with `stems configuration use {}`",
                    label(r.from.as_deref())
                ));
            }
            errors.push(e);
        }
    }
    CommandOutput::data(Json::Object(data))
        .with_human(human)
        .with_errors(errors)
}

/// `stems configuration use`.
fn use_(ctx: &Ctx, args: &ConfigurationUseArgs) -> CommandOutput {
    let resolved = match load(ctx) {
        Ok(r) => r,
        Err(e) => return CommandOutput::failed(e),
    };
    let ws = &resolved.workspace;
    let from = ws.configuration.clone();
    let to = (!configurations::is_none(&args.name)).then(|| args.name.clone());
    if let Some(name) = &to
        && !ws.configurations.iter().any(|c| &c.name == name)
    {
        return CommandOutput::failed(unknown(ws, name));
    }

    // A daemon runs: it edits, validates and applies (`switch_configuration`).
    if !args.no_apply {
        match via_daemon(ctx, &args.name) {
            Err(e) => return CommandOutput::failed(e),
            Ok(Some(r)) => return daemon_output(r),
            Ok(None) => {}
        }
    }

    // No daemon (or --no-apply): edit stems.local.yaml here.
    let (file, committed) = match config::local_file(ctx) {
        Ok(x) => x,
        Err(e) => return CommandOutput::failed(e),
    };
    let original = match config::read_local(&file) {
        Ok(t) => t,
        Err(e) => return CommandOutput::failed(e),
    };
    let path = ConfigPath::root().key("configuration");
    let no_base = |_: &ConfigPath| None;
    let committed_default = configurations::committed_selection(&committed);
    let edited = match &to {
        None if committed_default.is_none() => edit::unset(&original, &path).map(|(t, _)| t),
        None => edit::set(&original, &path, configurations::NONE, &no_base),
        Some(name) => edit::set(&original, &path, name, &no_base),
    };
    let new = match edited {
        Ok(t) => t,
        Err(e) => {
            return CommandOutput::failed(
                Error::new(
                    ErrorCode::Usage,
                    format!("cannot edit `{path}` in {}: {}", file.display(), e.0),
                )
                .with_path(path.clone())
                .with_hint(format!(
                    "set `configuration: {}` at the top level of {} by hand",
                    args.name,
                    file.display()
                )),
            );
        }
    };
    let mut data = serde_json::Map::new();
    data.insert("from".into(), json!(from));
    data.insert("to".into(), json!(to));
    data.insert("path".into(), json!(path.to_string()));
    let written = config::commit(ctx, &file, &original, &new, data);
    if !written.errors.is_empty() {
        return written;
    }
    let Json::Object(mut data) = written.data else {
        return CommandOutput::failed(Error::internal("configuration: unexpected output shape"));
    };
    let changed = data.get("changed").and_then(Json::as_bool).unwrap_or(false);
    let diff = data.get("diff").and_then(Json::as_array).cloned();
    let mut human = head_human(
        from.as_deref(),
        to.as_deref(),
        Some(file.as_path()).filter(|_| changed),
        diff.as_deref(),
    );
    data.insert("applied".into(), Json::Null);
    if args.no_apply {
        data.insert("daemon".into(), Json::Null);
        human.push_str(
            "not applied (--no-apply): run `stems config apply --yes` to restart what it changes\n",
        );
    } else {
        data.insert("daemon".into(), json!(false));
        human.push_str("the daemon is not running: the next `stems up` uses it\n");
    }
    CommandOutput::data(Json::Object(data)).with_human(human)
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `stems configuration save`.
fn save(ctx: &Ctx, args: &ConfigurationSaveArgs) -> CommandOutput {
    let name = &args.name;
    if !valid_name(name) || configurations::is_none(name) {
        return CommandOutput::failed(Error::usage(
            format!("`{name}` is not a valid configuration name"),
            format!(
                "use letters, digits, `-`, `_` or `.` (and not `{}`)",
                configurations::NONE
            ),
        ));
    }
    let (local_path, committed) = match config::local_file(ctx) {
        Ok(x) => x,
        Err(e) => return CommandOutput::failed(e),
    };
    let (config_file, _) = match stems_config::committed_tree(&ctx.load_options()) {
        Ok(x) => x,
        Err(e) => return CommandOutput::failed(Errors::from(e)),
    };
    let root = config_file
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf);
    let original = match config::read_local(&local_path) {
        Ok(t) => t,
        Err(e) => return CommandOutput::failed(e),
    };
    let key = ConfigPath::root().key("configuration");
    let snapshot = match edit::unset(&original, &key) {
        Ok((t, _)) => t,
        Err(e) => {
            return CommandOutput::failed(Error::usage(
                format!("cannot read {}: {}", local_path.display(), e.0),
                "fix the file's YAML first",
            ));
        }
    };
    let keys: Vec<String> = match edit::parse_value(&snapshot) {
        Ok(serde_yaml_ng::Value::Mapping(m)) if !m.is_empty() => m
            .keys()
            .map(|k| k.as_str().map_or_else(|| format!("{k:?}"), str::to_string))
            .collect(),
        _ => {
            return CommandOutput::failed(
                Error::usage(
                    format!(
                        "{} is missing or empty: there are no local overrides to save",
                        local_path.display()
                    ),
                    "put the overrides into stems.local.yaml first (`stems config set …`, `stems switch …`), then save them",
                )
                .with_details(json!({ "file": local_path })),
            );
        }
    };
    if let Some(existing) = committed
        .get("configurations")
        .and_then(|c| c.get(name.as_str()))
    {
        let inline = !existing.is_string();
        if inline || !args.force {
            let what = if inline { "an inline" } else { "a" };
            return CommandOutput::failed(
                Error::usage(
                    format!("`{name}` is already {what} configuration in stems.yaml"),
                    if inline {
                        "choose another name, or remove the inline configuration first".to_string()
                    } else {
                        "choose another name, or pass --force to overwrite its file".to_string()
                    },
                )
                .with_path(ConfigPath::root().key("configurations").key(name)),
            );
        }
    }
    let rel: PathBuf = args
        .file
        .clone()
        .unwrap_or_else(|| PathBuf::from("configurations").join(format!("{name}.yaml")));
    let target = root.join(&rel);
    let value = relative(&root, &target);
    if target.exists() && !args.force {
        return CommandOutput::failed(
            Error::usage(
                format!("{} already exists", target.display()),
                "pass --force to overwrite it, or --file <path> for another file",
            )
            .with_details(json!({ "file": target })),
        );
    }
    let existed = target.exists();
    if let Some(dir) = target.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        return CommandOutput::failed(
            Error::new(
                ErrorCode::ConfigReadFailed,
                format!("cannot create {}: {e}", dir.display()),
            )
            .with_hint("check the permissions of the integration repo directory"),
        );
    }
    let header = format!(
        "# {value} — configuration `{name}`, saved from {} by `stems configuration save`.\n# Select it with `stems configuration use {name}`.\n",
        stems_config::LOCAL_FILE
    );
    if let Err(e) = std::fs::write(&target, format!("{header}{snapshot}")) {
        return CommandOutput::failed(
            Error::new(
                ErrorCode::ConfigReadFailed,
                format!("cannot write {}: {e}", target.display()),
            )
            .with_hint("check the permissions of the integration repo directory")
            .with_details(json!({ "file": target })),
        );
    }

    // Register it in stems.yaml (validated; restored on error).
    let path = ConfigPath::root().key("configurations").key(name);
    let config_text = match std::fs::read_to_string(&config_file) {
        Ok(t) => t,
        Err(e) => {
            return CommandOutput::failed(Error::new(
                ErrorCode::ConfigReadFailed,
                format!("{} cannot be read: {e}", config_file.display()),
            ));
        }
    };
    let value_text = serde_yaml_ng::to_string(&serde_yaml_ng::Value::String(value.clone()))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| value.clone());
    let no_base = |_: &ConfigPath| None;
    let new = match edit::set(&config_text, &path, &value_text, &no_base) {
        Ok(t) => t,
        Err(e) => {
            return CommandOutput::failed(
                Error::new(
                    ErrorCode::Usage,
                    format!("cannot edit `{path}` in {}: {}", config_file.display(), e.0),
                )
                .with_path(path)
                .with_hint(format!(
                    "add `configurations: {{ {name}: {value_text} }}` to {} by hand",
                    config_file.display()
                )),
            );
        }
    };
    let mut data = serde_json::Map::new();
    data.insert("name".into(), json!(name));
    data.insert("configuration_file".into(), json!(target));
    data.insert("relative".into(), json!(value));
    data.insert("keys".into(), json!(keys));
    data.insert("path".into(), json!(path.to_string()));
    data.insert("value".into(), json!(value));
    let written = config::commit(ctx, &config_file, &config_text, &new, data);
    if !written.errors.is_empty() {
        if !existed {
            let _ = std::fs::remove_file(&target);
        }
        return written;
    }
    let Json::Object(data) = written.data else {
        return CommandOutput::failed(Error::internal("configuration: unexpected output shape"));
    };
    let mut human = format!("wrote {} (keys: {})\n", target.display(), keys.join(", "));
    if data.get("changed").and_then(Json::as_bool).unwrap_or(false) {
        human.push_str(&format!("{}:\n", config_file.display()));
        for l in data
            .get("diff")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
        {
            human.push_str(&format!("  {l}\n"));
        }
    }
    human.push_str(&format!(
        "select it with `stems configuration use {name}` (the local overrides stay in {})\n",
        stems_config::LOCAL_FILE
    ));
    CommandOutput::data(Json::Object(data)).with_human(human)
}
