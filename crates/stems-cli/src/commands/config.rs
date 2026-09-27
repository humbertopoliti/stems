//! `stems config get|set|unset` (FR-CL-5, deliverable 26) and `stems config
//! diff|apply` (deliverable 33, FR-WD-3, `docs/config.md#reload`).
//!
//! - `get <path>`: the **resolved** value at `path` (the model `stems show`
//!   prints: defaults applied, variables substituted) and where it came
//!   from: `data: { path, value, source, file }` with `source` one of
//!   `stems.yaml`, `stems.local.yaml`, `include:<file relative to the
//!   workspace>` (an included or `extends` file) or `default` (not written
//!   in any file: a default or a derived value); `file` is the absolute
//!   path or null.
//! - `set <path> <value>`: writes `value` (YAML: scalar or flow collection)
//!   at `path` in `stems.local.yaml`, creating the file and missing keys,
//!   preserving comments and layout (`stems_config::edit`). The workspace is
//!   then re-loaded and validated; if that introduces errors that were not
//!   there before, the file is restored and the errors are returned (exit
//!   2). `data: { path, value, file, changed, diff: ["-old", "+new"] }`.
//! - `unset <path>`: removes `path` (and parents left empty) from
//!   `stems.local.yaml`, validated the same way. `data: { path, file,
//!   changed, diff }`.
//!
//! - `diff`: the running daemon's reload plan from the applied config to
//!   the one on disk: `data = ConfigDiffResult { plan: { stems: [{name,
//!   action, changes, fields, hot, running}], workspace, catalog_changed },
//!   pending, loaded_at, detected_at, sources, last_error }`. Human: a table
//!   `STEM ACTION FIELDS HOT` of the affected stems.
//! - `apply [stems…] [--yes]`: apply it (`data = ConfigApplyResult {
//!   applied, failed, skipped, workspace, pending, ok }`); asks on a
//!   terminal, refuses (`DESTRUCTIVE_NOT_CONFIRMED`) without `--yes`
//!   otherwise; exit 3 when a stem failed.
//!
//! `get`/`set`/`unset` involve no daemon; a running daemon notices the
//! change (config watcher, 33) and waits for `stems config apply`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};
use stems_config::edit::{self, WriteError};
use stems_config::{ConfigPath, LOCAL_FILE, Resolved, Segment};
use stems_core::{Error, ErrorCode, Errors, ValidateOptions};

use crate::cli::{ConfigApplyArgs, ConfigGetArgs, ConfigSetArgs, ConfigUnsetArgs};
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::output::{CommandOutput, Mode, human_value};

fn parse_path(s: &str) -> Result<ConfigPath, Error> {
    let p: ConfigPath = s.parse().map_err(|e: stems_config::ParsePathError| {
        Error::usage(
            e.to_string(),
            "write the path with dots and [n] indices, e.g. stems.shop-api.env.PORT or stems.shop-api.ports[0].port",
        )
    })?;
    if p.is_root() {
        return Err(Error::usage(
            "the config path is empty",
            "name a key, e.g. stems.shop-api.enabled",
        ));
    }
    Ok(p)
}

fn load(ctx: &Ctx) -> Result<Resolved, Errors> {
    stems_config::load(ctx.load_options()).map_err(|e| {
        let mut es = Errors::from(e);
        es.sort();
        es
    })
}

fn navigate<'v>(v: &'v Json, path: &ConfigPath) -> Option<&'v Json> {
    path.segments().iter().try_fold(v, |cur, s| match s {
        Segment::Key(k) => cur.get(k.as_str()),
        Segment::Index(i) => cur.get(*i),
    })
}

/// Where the effective value at `path` was written.
fn source_of(resolved: &Resolved, path: &ConfigPath) -> (String, Option<PathBuf>) {
    let ws = &resolved.workspace;
    let Some(span) = resolved.spans.get(path) else {
        return ("default".into(), None);
    };
    let file = span.file.clone();
    let label = if file == ws.config_file {
        stems_config::CONFIG_FILE.to_string()
    } else if file == ws.root.join(LOCAL_FILE) {
        LOCAL_FILE.to_string()
    } else {
        let rel = file.strip_prefix(&ws.root).unwrap_or(&file);
        format!("include:{}", rel.display())
    };
    (label, Some(file))
}

/// `stems config get`.
pub fn get(ctx: &Ctx, args: &ConfigGetArgs) -> CommandOutput {
    let path = match parse_path(&args.path) {
        Ok(p) => p,
        Err(e) => return CommandOutput::failed(e),
    };
    let resolved = match load(ctx) {
        Ok(r) => r,
        Err(e) => return CommandOutput::failed(e),
    };
    let model = match serde_json::to_value(&resolved.workspace) {
        Ok(v) => v,
        Err(e) => return CommandOutput::failed(Error::internal(format!("serialising: {e}"))),
    };
    let Some(value) = navigate(&model, &path).cloned() else {
        let parent = path.parent();
        let known: Vec<String> = navigate(&model, &parent)
            .and_then(Json::as_object)
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        let hint = if known.is_empty() {
            "see `stems show --json` for the resolved config and its paths".to_string()
        } else {
            format!("keys at `{parent}`: {}", known.join(", "))
        };
        return CommandOutput::failed(
            Error::new(ErrorCode::Usage, format!("no config value at `{path}`"))
                .with_path(path.clone())
                .with_hint(hint)
                .with_details(json!({ "path": path.to_string() })),
        );
    };
    let (source, file) = source_of(&resolved, &path);
    let human = match &value {
        Json::Object(_) | Json::Array(_) => human_value(&value),
        Json::String(s) => format!("{s}\n"),
        other => format!("{other}\n"),
    };
    CommandOutput::data(json!({
        "path": path.to_string(),
        "value": value,
        "source": source,
        "file": file,
    }))
    .with_human(human)
}

/// Problems of the workspace as it is on disk now, keyed without locations
/// (lines move when the file is edited).
fn problems(ctx: &Ctx) -> Result<HashSet<String>, Errors> {
    let opts = ValidateOptions {
        skip_requires: true,
        skip_overlays: true,
        owned_overlays: Vec::new(),
    };
    let errors = match stems_config::load(ctx.load_options()) {
        Ok(r) => stems_core::validate(&r, &opts),
        Err(e) => Errors::from(e).into_vec(),
    };
    let keys: HashSet<String> = errors.iter().map(key).collect();
    Ok(keys)
}

fn key(e: &Error) -> String {
    format!(
        "{}|{}|{}",
        e.code,
        e.message,
        e.path.as_ref().map(ToString::to_string).unwrap_or_default()
    )
}

/// The errors the edited file introduces (sorted), or `Ok`.
fn new_problems(ctx: &Ctx, before: &HashSet<String>) -> Result<(), Errors> {
    let opts = ValidateOptions {
        skip_requires: true,
        skip_overlays: true,
        owned_overlays: Vec::new(),
    };
    let errors = match stems_config::load(ctx.load_options()) {
        Ok(r) => stems_core::validate(&r, &opts),
        Err(e) => Errors::from(e).into_vec(),
    };
    let mut fresh: Errors = errors
        .into_iter()
        .filter(|e| !before.contains(&key(e)))
        .collect();
    if fresh.is_empty() {
        Ok(())
    } else {
        fresh.sort();
        Err(fresh)
    }
}

/// Write `new` over `original` in `file` if it validates.
pub(crate) fn commit(
    ctx: &Ctx,
    file: &Path,
    original: &str,
    new: &str,
    mut data: serde_json::Map<String, Json>,
) -> CommandOutput {
    let diff = edit::diff_lines(original, new);
    let changed = original != new;
    data.insert("file".into(), json!(file));
    data.insert("changed".into(), json!(changed));
    data.insert("diff".into(), json!(diff));
    if changed {
        let before = match problems(ctx) {
            Ok(b) => b,
            Err(e) => return CommandOutput::failed(e),
        };
        match edit::write_checked(file, new, || new_problems(ctx, &before)) {
            Ok(()) => {}
            Err(WriteError::Io(e)) => {
                return CommandOutput::failed(
                    Error::new(
                        ErrorCode::ConfigReadFailed,
                        format!("cannot write {}: {e}", file.display()),
                    )
                    .with_hint("check the permissions of the integration repo directory")
                    .with_details(json!({ "file": file })),
                );
            }
            Err(WriteError::Rejected(mut errors)) => {
                for e in errors.0.iter_mut() {
                    if e.hint.is_none() {
                        e.hint = Some(format!(
                            "{} was left unchanged; fix the value and run the command again",
                            LOCAL_FILE
                        ));
                    }
                }
                let mut out = CommandOutput::failed(errors);
                data.insert("changed".into(), json!(false));
                data.insert("rejected".into(), json!(true));
                out.data = Json::Object(data);
                return out;
            }
        }
    }
    let human = if changed {
        let mut h = format!("{}:\n", file.display());
        for l in &diff {
            h.push_str(&format!("  {l}\n"));
        }
        h
    } else {
        format!("{} unchanged\n", file.display())
    };
    CommandOutput::data(Json::Object(data)).with_human(human)
}

/// The integration repo's `stems.local.yaml` and the committed tree.
pub(crate) fn local_file(ctx: &Ctx) -> Result<(PathBuf, serde_yaml_ng::Value), Errors> {
    let (config_file, tree) =
        stems_config::committed_tree(&ctx.load_options()).map_err(Errors::from)?;
    let root = config_file
        .parent()
        .map_or_else(PathBuf::new, Path::to_path_buf);
    Ok((root.join(LOCAL_FILE), tree))
}

pub(crate) fn read_local(file: &Path) -> Result<String, Error> {
    match std::fs::read_to_string(file) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(Error::new(
            ErrorCode::ConfigReadFailed,
            format!("{} cannot be read: {e}", file.display()),
        )
        .with_hint("check that the file is readable by your user (permissions, not a directory)")),
    }
}

fn edit_error(e: edit::EditError, path: &ConfigPath) -> Error {
    Error::new(ErrorCode::Usage, format!("cannot edit `{path}`: {}", e.0))
        .with_path(path.clone())
        .with_hint(format!(
            "pass the value as YAML (quote strings with spaces or `:`), or edit {LOCAL_FILE} by hand"
        ))
}

/// `stems config set`.
pub fn set(ctx: &Ctx, args: &ConfigSetArgs) -> CommandOutput {
    let path = match parse_path(&args.path) {
        Ok(p) => p,
        Err(e) => return CommandOutput::failed(e),
    };
    let value = match edit::parse_value(&args.value) {
        Ok(v) => v,
        Err(e) => return CommandOutput::failed(edit_error(e, &path)),
    };
    let (file, committed) = match local_file(ctx) {
        Ok(x) => x,
        Err(e) => return CommandOutput::failed(e),
    };
    let original = match read_local(&file) {
        Ok(t) => t,
        Err(e) => return CommandOutput::failed(e),
    };
    let base = |p: &ConfigPath| {
        p.segments()
            .iter()
            .try_fold(&committed, |cur, s| match s {
                Segment::Key(k) => cur.get(k.as_str()),
                Segment::Index(i) => cur.get(*i),
            })
            .cloned()
    };
    let new = match edit::set(&original, &path, &args.value, &base) {
        Ok(t) => t,
        Err(e) => return CommandOutput::failed(edit_error(e, &path)),
    };
    let mut data = serde_json::Map::new();
    data.insert("path".into(), json!(path.to_string()));
    data.insert(
        "value".into(),
        serde_json::to_value(&value).unwrap_or(Json::Null),
    );
    commit(ctx, &file, &original, &new, data)
}

/// `stems config unset`.
pub fn unset(ctx: &Ctx, args: &ConfigUnsetArgs) -> CommandOutput {
    let path = match parse_path(&args.path) {
        Ok(p) => p,
        Err(e) => return CommandOutput::failed(e),
    };
    let (file, _) = match local_file(ctx) {
        Ok(x) => x,
        Err(e) => return CommandOutput::failed(e),
    };
    let original = match read_local(&file) {
        Ok(t) => t,
        Err(e) => return CommandOutput::failed(e),
    };
    let (new, _) = match edit::unset(&original, &path) {
        Ok(x) => x,
        Err(e) => return CommandOutput::failed(edit_error(e, &path)),
    };
    let mut data = serde_json::Map::new();
    data.insert("path".into(), json!(path.to_string()));
    commit(ctx, &file, &original, &new, data)
}

// ---------------------------------------------------------------------------
// diff / apply (33)
// ---------------------------------------------------------------------------

/// How long `config apply` may take (it restarts stems).
pub(crate) const APPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3600);

fn table(rows: &[[String; 4]]) -> String {
    let widths: Vec<usize> = (0..4)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in rows {
        let line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

/// The human `config diff`: the affected stems as `STEM ACTION FIELDS HOT`,
/// then workspace-level changes.
pub fn diff_human(r: &stems_api::ConfigDiffResult) -> String {
    let mut out = String::new();
    if !r.last_error.is_empty() {
        out.push_str(&format!(
            "the config on disk is invalid ({} error{}); the applied config stays:\n",
            r.last_error.len(),
            if r.last_error.len() == 1 { "" } else { "s" }
        ));
        for e in &r.last_error {
            out.push_str(&format!("  {}: {}\n", e.code, e.message));
        }
    }
    if !r.pending {
        out.push_str("no pending config change\n");
        return out;
    }
    let mut rows: Vec<[String; 4]> = vec![["STEM", "ACTION", "FIELDS", "HOT"].map(str::to_string)];
    for e in r.plan.affected() {
        let action = if e.changes.len() > 1 {
            let rest: Vec<&str> = e
                .changes
                .iter()
                .filter(|a| **a != e.action)
                .map(|a| a.as_str())
                .collect();
            format!("{} (+{})", e.action, rest.join(", "))
        } else {
            e.action.to_string()
        };
        let fields = if e.fields.is_empty() {
            "-".to_string()
        } else {
            e.fields.join(",")
        };
        let hot = if e.hot {
            "yes"
        } else if e.running {
            "no (restart)"
        } else {
            "no (not running)"
        };
        rows.push([e.name.clone(), action, fields, hot.to_string()]);
    }
    if rows.len() > 1 {
        out.push_str(&table(&rows));
    }
    let unchanged = r.plan.stems.len() - r.plan.affected().count();
    if unchanged > 0 {
        out.push_str(&format!("{unchanged} stems unchanged\n"));
    }
    if !r.plan.workspace.is_empty() {
        out.push_str(&format!("workspace: {}\n", r.plan.workspace.join(", ")));
    }
    if r.plan.catalog_changed {
        out.push_str("script catalog (MCP tools) changed\n");
    }
    out.push_str("apply with `stems config apply`\n");
    out
}

/// `stems config diff`.
pub fn diff(ctx: &Ctx) -> CommandOutput {
    block_on(async {
        let c = client::connect(ctx).await?;
        let r: stems_api::ConfigDiffResult =
            c.call(stems_api::Method::CONFIG_DIFF, json!({})).await?;
        let data = serde_json::to_value(&r).unwrap_or(Json::Null);
        Ok::<_, Errors>(CommandOutput::data(data).with_human(diff_human(&r)))
    })
    .unwrap_or_else(CommandOutput::failed)
}

/// The human `config apply`.
pub fn apply_human(r: &stems_api::ConfigApplyResult) -> String {
    let mut out = String::new();
    for a in &r.applied {
        out.push_str(&format!("{}: {} ({})\n", a.stem, a.result, a.action));
    }
    for f in &r.failed {
        out.push_str(&format!(
            "{}: failed ({}): {}\n",
            f.stem, f.action, f.error.message
        ));
    }
    for s in &r.skipped {
        out.push_str(&format!(
            "{}: skipped ({}): {}\n",
            s.stem, s.action, s.reason
        ));
    }
    if !r.workspace.is_empty() {
        out.push_str(&format!("workspace: {}\n", r.workspace.join(", ")));
    }
    if out.is_empty() {
        out.push_str("nothing to apply\n");
    }
    if r.pending {
        out.push_str("changes are still pending: see `stems config diff`\n");
    }
    out
}

fn confirm(plan: &stems_api::ConfigDiffResult) -> bool {
    use std::io::{BufRead, Write};
    let mut err = std::io::stderr();
    let _ = write!(err, "{}", diff_human(plan));
    let _ = write!(err, "apply these changes? [y/N] ");
    let _ = err.flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

/// `stems config apply`.
pub fn apply(ctx: &Ctx, args: &ConfigApplyArgs, mode: Mode) -> CommandOutput {
    block_on(async {
        let c = client::connect(ctx).await?;
        let mut yes = args.yes;
        if !yes && mode == Mode::Human && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            let plan: stems_api::ConfigDiffResult =
                c.call(stems_api::Method::CONFIG_DIFF, json!({})).await?;
            if !plan.pending {
                return Ok(CommandOutput::data(json!({
                    "applied": [], "failed": [], "skipped": [], "workspace": [],
                    "pending": false, "ok": true,
                }))
                .with_human("no pending config change\n".to_string()));
            }
            yes = confirm(&plan);
            if !yes {
                return Ok(
                    CommandOutput::data(json!({ "applied": [], "confirmed": false }))
                        .with_human("not applied\n".to_string()),
                );
            }
        }
        let p = stems_api::ConfigApplyParams {
            stems: args.stems.clone(),
            yes,
        };
        let r: stems_api::ConfigApplyResult = c
            .call_with_timeout(stems_api::Method::CONFIG_APPLY, &p, APPLY_TIMEOUT)
            .await?;
        let errors: Vec<Error> = r.failed.iter().map(|f| f.error.clone()).collect();
        let data = serde_json::to_value(&r).unwrap_or(Json::Null);
        let mut out = CommandOutput::data(data)
            .with_human(apply_human(&r))
            .with_errors(Errors(errors));
        if !r.failed.is_empty() {
            // Partial: the other stems were applied.
            out = out.with_exit(3);
        }
        Ok::<_, Errors>(out)
    })
    .unwrap_or_else(CommandOutput::failed)
}
