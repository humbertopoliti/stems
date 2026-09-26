//! Stem outputs (FR-ST-6, deliverable 26, `docs/config.md` → Outputs).
//!
//! A stem declares `outputs: { NAME: "template" | {command, secret} }`.
//! When its health check first passes, *before* the actor moves it to
//! `healthy`, the outputs are evaluated:
//!
//! * a template is rendered with the stem's own context: the loader already
//!   substituted vars, env and fixed ports; `auto` ports and outputs of other
//!   (running) stems are filled in here;
//! * a command runs through the [`ScriptRunner`](crate::scripts::ScriptRunner)
//!   (script name / log tag `outputs`) in the stem's environment and cwd,
//!   bounded by [`COMMAND_TIMEOUT`]; its stdout (last
//!   [`TAIL_LINES`](crate::scripts::TAIL_LINES) lines), trimmed, is the value.
//!   A secret command runs quietly: its output never reaches the logs, events
//!   or error details. A failure fails the stem (`SCRIPT_FAILED`, `details.output`).
//!
//! The values go to the supervisor's [`OutputStore`] (the actor is the only
//! writer of its stem's entry), and `stem.outputs {names}` is emitted (never
//! values). Because the stem only becomes `healthy` afterwards, a dependant
//! with `condition: healthy` (or `seeded`) always finds them when *its* env,
//! scripts and overlays are rendered at its start. They are dropped whenever
//! the stem leaves the running states (stop, exit, failure, a new start) and
//! re-evaluated on every start.
//!
//! Consumers: `${stem.<n>.outputs.X}` in env values ([`super::env`]), inline
//! stem scripts and overlay templates; `STEMS_<DEP>_OUTPUT_<X>` in the env
//! of every direct dependant. `status` / `outputs` show secrets as `<redacted>`.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use stems_api::{EventKind, OutputValue, OutputsParams, OutputsResult, REDACTED, StemOutputs};
use stems_config::{Dur, Output, Resolved, Script, ScriptSource, Stem, StemRuntime, Workspace};
use stems_core::{Error, ErrorCode, StemState};
use tokio_util::sync::CancellationToken;

use super::actor::StemCell;
use super::{Core, Supervisor};
use crate::events::EventDraft;
use crate::scripts::{RunContext, ScriptRef};

/// Script name (and log tag) of output commands.
pub const OUTPUTS_TAG: &str = "outputs";
/// Upper bound for one output command.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
pub use stems_core::overlays::OUTPUTS_HINT;

/// One evaluated output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputEntry {
    /// The value.
    pub value: String,
    /// Declared `secret: true`.
    pub secret: bool,
}

impl OutputEntry {
    /// The value as shown to users (`<redacted>` when secret).
    pub fn shown(&self) -> String {
        if self.secret {
            REDACTED.to_string()
        } else {
            self.value.clone()
        }
    }
}

/// Evaluated outputs of one stem, by name.
pub type Outputs = BTreeMap<String, OutputEntry>;

/// The outputs of every stem that is running, by stem.
#[derive(Debug, Default)]
pub struct OutputStore(Mutex<HashMap<String, Outputs>>);

impl OutputStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Outputs>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// `stem`'s outputs (empty when not evaluated).
    pub fn get(&self, stem: &str) -> Outputs {
        self.lock().get(stem).cloned().unwrap_or_default()
    }
    /// One output value.
    pub fn value(&self, stem: &str, name: &str) -> Option<String> {
        self.lock()
            .get(stem)
            .and_then(|o| o.get(name))
            .map(|e| e.value.clone())
    }
    /// Replace `stem`'s outputs.
    pub fn set(&self, stem: &str, outputs: Outputs) {
        self.lock().insert(stem.to_string(), outputs);
    }
    /// Drop `stem`'s outputs; true if it had any.
    pub fn clear(&self, stem: &str) -> bool {
        self.lock().remove(stem).is_some()
    }
    /// Plain values per stem (templates / overlays).
    pub fn values(&self) -> HashMap<String, BTreeMap<String, String>> {
        self.lock()
            .iter()
            .map(|(s, o)| {
                let vals = o
                    .iter()
                    .map(|(k, e)| (k.clone(), e.value.clone()))
                    .collect();
                (s.clone(), vals)
            })
            .collect()
    }
}

/// `STEMS_<STEM>_OUTPUT_<NAME>` (`shop-api`, `TOKEN` → `STEMS_SHOP_API_OUTPUT_TOKEN`).
pub fn output_var(stem: &str, name: &str) -> String {
    let up: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("STEMS_{up}_OUTPUT_{name}")
}

/// Replace every `${stem.<n>.outputs.<x>}` (and `stem.self`) in `s` with
/// `lookup(n, x)`; references without a value are kept literally and pushed
/// (without `${`/`}`, `self` rewritten) to `missing`.
pub fn render_output_refs(
    s: &str,
    self_name: &str,
    lookup: &dyn Fn(&str, &str) -> Option<String>,
    missing: &mut Vec<String>,
) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("${stem.") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[i..]);
            return out;
        };
        let expr = after[..end].trim();
        let literal = &rest[i..i + 2 + end + 1];
        rest = &after[end + 1..];
        let parts: Vec<&str> = expr.splitn(4, '.').collect();
        let ["stem", n, "outputs", x] = parts.as_slice() else {
            out.push_str(literal);
            continue;
        };
        let n = if *n == "self" { self_name } else { n };
        match lookup(n, x) {
            Some(v) => out.push_str(&v),
            None => {
                out.push_str(literal);
                let r = format!("stem.{n}.outputs.{x}");
                if !missing.contains(&r) {
                    missing.push(r);
                }
            }
        }
    }
    out.push_str(rest);
    out
}

/// `UNRESOLVED_VARIABLE` for an output reference without a value at `stem`'s start.
pub fn unresolved_error(stem: &str, reference: &str, key: Option<&str>) -> Error {
    let place = key.map_or_else(String::new, |k| format!(" (env `{k}`)"));
    Error::new(
        ErrorCode::UnresolvedVariable,
        format!("`${{{reference}}}` has no value when `{stem}` starts{place}"),
    )
    .with_hint(OUTPUTS_HINT)
    .with_details(json!({ "stem": stem, "reference": reference, "key": key }))
}

/// Render a static output: `auto` port references via `port`, output
/// references via the store. Unresolvable references stay literal.
pub fn render_static(
    template: &str,
    stem: &str,
    port: &mut dyn FnMut(&str, Option<&str>) -> Option<u16>,
    output: &dyn Fn(&str, &str) -> Option<String>,
) -> String {
    let s = super::env::render_refs(template, stem, port);
    render_output_refs(&s, stem, output, &mut Vec::new())
}

/// Render `${stem.…}` references (ports and outputs) inside an inline stem
/// script, so scripts can use `${stem.<dep>.outputs.X}` too. Unresolved
/// references are kept (the shell reports them).
pub(crate) fn render_script(core: &Core, ws: &Workspace, stem: &str, script: &Script) -> Script {
    let ScriptSource::Command(c) = &script.source else {
        return script.clone();
    };
    if !c.contains("${stem.") {
        return script.clone();
    }
    let mut allocated = Vec::new();
    let mut port = |n: &str, p: Option<&str>| core.ports.resolve_ref(ws, n, p, &mut allocated);
    let output = |n: &str, x: &str| core.outputs.value(n, x);
    let mut s = script.clone();
    s.source = ScriptSource::Command(render_static(c, stem, &mut port, &output));
    s
}

/// What the actor needs to evaluate a stem's outputs once it is ready (kept
/// from its spawn).
#[derive(Clone)]
pub(crate) struct OutputsCtx {
    /// The workspace the stem was started from.
    pub ws: Arc<Resolved>,
    /// The stem's full environment.
    pub env: BTreeMap<String, String>,
    /// Who started it.
    pub actor: String,
}

/// Why an evaluation did not produce values.
#[derive(Debug)]
pub(crate) enum EvalError {
    /// A stop arrived (it takes over).
    Cancelled,
    /// A command failed: `SCRIPT_FAILED`.
    Failed(Error),
}

/// Evaluate every output of `stem` in declaration order.
pub(crate) async fn evaluate(
    core: &Core,
    ws: &Workspace,
    stem: &Stem,
    env: &BTreeMap<String, String>,
    actor: &str,
    cancel: Option<&CancellationToken>,
) -> Result<Outputs, EvalError> {
    let mut out = Outputs::new();
    let mut allocated = Vec::new();
    for (name, o) in &stem.outputs {
        let entry = match o {
            Output::Value(t) => {
                let mut port =
                    |n: &str, p: Option<&str>| core.ports.resolve_ref(ws, n, p, &mut allocated);
                let output = |n: &str, x: &str| {
                    if n == stem.name {
                        out.get(x).map(|e| e.value.clone())
                    } else {
                        core.outputs.value(n, x)
                    }
                };
                OutputEntry {
                    value: render_static(t, &stem.name, &mut port, &output),
                    secret: false,
                }
            }
            Output::Command { command, secret } => {
                let value =
                    run_command(core, ws, stem, name, command, *secret, env, actor, cancel).await?;
                OutputEntry {
                    value,
                    secret: *secret,
                }
            }
        };
        out.insert(name.clone(), entry);
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn run_command(
    core: &Core,
    ws: &Workspace,
    stem: &Stem,
    name: &str,
    command: &str,
    secret: bool,
    env: &BTreeMap<String, String>,
    actor: &str,
    cancel: Option<&CancellationToken>,
) -> Result<String, EvalError> {
    let script = Script {
        source: ScriptSource::Command(command.to_string()),
        description: None,
        args: Vec::new(),
        inputs: Vec::new(),
        requires: Vec::new(),
        timeout: Some(Dur(COMMAND_TIMEOUT)),
        retries: 0,
        concurrent: true,
        stamp_env: Vec::new(),
        cwd: stem.default_cwd(&ws.root).to_path_buf(),
        cwd_set: false,
    };
    let script = render_script(core, ws, &stem.name, &script);
    let cwd_override = stem
        .codebase
        .is_none()
        .then(|| core.scripts.state_dir(Some(&stem.name)));
    let shell = match &stem.runtime {
        StemRuntime::Process(p) => Some(p.shell.clone()),
        _ => None,
    };
    let mut event_extra = serde_json::Map::new();
    event_extra.insert("output".into(), json!(name));
    let with_output = |mut e: Error| {
        if let Some(m) = e.details.as_object_mut() {
            m.insert("output".into(), json!(name));
            if secret {
                m.insert("tail".into(), json!([]));
            }
        }
        e.message = format!("{} (output `{name}`)", e.message);
        e
    };
    let r = core
        .scripts
        .run(
            &ws.root,
            Some(&stem.name),
            ScriptRef {
                name: OUTPUTS_TAG,
                script: &script,
            },
            RunContext {
                env: env.clone(),
                cwd_override,
                shell,
                actor: actor.to_string(),
                cancel: cancel.cloned(),
                event_extra,
                // A secret's value must never reach logs or events.
                quiet: secret,
                ..RunContext::default()
            },
        )
        .await
        .map_err(|e| EvalError::Failed(with_output(e)))?;
    if r.cancelled {
        return Err(EvalError::Cancelled);
    }
    if !r.success() {
        return Err(EvalError::Failed(with_output(r.error())));
    }
    Ok(r.tail.join("\n").trim().to_string())
}

/// Store `outputs` for `stem` and announce them (names only).
pub(crate) fn publish(core: &Core, stem: &str, outputs: Outputs, actor: &str) {
    let names: Vec<&String> = outputs.keys().collect();
    let secret: Vec<&String> = outputs
        .iter()
        .filter(|(_, e)| e.secret)
        .map(|(n, _)| n)
        .collect();
    core.events.emit(
        EventDraft::new(EventKind::STEM_OUTPUTS, actor)
            .stem(stem)
            .data(json!({ "names": names, "secret": secret })),
    );
    core.outputs.set(stem, outputs);
}

/// Called by every state transition: outputs only live while the stem runs.
pub(crate) fn on_transition(core: &Core, stem: &str, to: StemState) {
    if matches!(
        to,
        StemState::Stopped
            | StemState::Stopping
            | StemState::Failed
            | StemState::Setup
            | StemState::Starting
    ) {
        core.outputs.clear(stem);
    }
}

/// The actor's ready path (before `healthy`): evaluate the outputs of the
/// start of `generation`. `true`: go on to `healthy`. `false`: the stem was
/// failed here (its process stopped) or a stop takes over.
pub(crate) async fn on_ready(core: &Arc<Core>, cell: &Arc<StemCell>, generation: u64) -> bool {
    let Some(ctx) = cell.info().outputs_ctx.clone() else {
        return true;
    };
    let Some(stem) = ctx.ws.workspace.stem(&cell.name).cloned() else {
        return true;
    };
    if stem.outputs.is_empty() {
        return true;
    }
    let token = CancellationToken::new();
    cell.info().script_cancel = Some(token.clone());
    let r = evaluate(
        core,
        &ctx.ws.workspace,
        &stem,
        &ctx.env,
        &ctx.actor,
        Some(&token),
    )
    .await;
    let current = {
        let mut info = cell.info();
        info.script_cancel = None;
        info.generation == generation
    };
    match r {
        _ if !current => false,
        Ok(outputs) => {
            publish(core, &cell.name, outputs, stems_api::DAEMON_ACTOR);
            true
        }
        Err(EvalError::Cancelled) => false,
        Err(EvalError::Failed(e)) => {
            cell.bump_generation();
            if let Some((h, rt)) = cell.clear_process() {
                let grace = cell.info().grace;
                if let Err(err) = rt.stop(&h, grace).await {
                    tracing::warn!(stem = %cell.name, error = %err, "stopping a stem whose output failed");
                }
                rt.release(&h);
            }
            cell.fail(core, e, &ctx.actor);
            false
        }
    }
}

/// External stems (never `healthy` through the actor's ready path): their
/// outputs are evaluated when monitoring starts. Env references to outputs
/// are resolved leniently.
pub(crate) async fn for_external(
    core: &Core,
    ws: &Resolved,
    stem: &Stem,
    actor: &str,
) -> Result<(), Error> {
    if stem.outputs.is_empty() {
        return Ok(());
    }
    let env = super::hooks::stem_env(core, &ws.workspace, stem, &BTreeMap::new())?;
    match evaluate(core, &ws.workspace, stem, &env, actor, None).await {
        Ok(o) => {
            publish(core, &stem.name, o, actor);
            Ok(())
        }
        Err(EvalError::Failed(e)) => Err(e),
        Err(EvalError::Cancelled) => Ok(()),
    }
}

/// A stem adopted after a daemon restart (11) lost its outputs with the old
/// daemon: evaluate them again in the background (a failure is logged; the
/// adopted process is left alone).
pub(crate) fn after_adopt(core: &Arc<Core>, cell: &Arc<StemCell>, ws: &Arc<Resolved>) {
    let Some(stem) = ws.workspace.stem(&cell.name).cloned() else {
        return;
    };
    if stem.outputs.is_empty() {
        return;
    }
    let (core, cell, ws) = (core.clone(), cell.clone(), ws.clone());
    tokio::spawn(async move {
        let generation = cell.info().generation;
        let env = match super::hooks::stem_env(&core, &ws.workspace, &stem, &BTreeMap::new()) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(stem = %stem.name, error = %e, "outputs of an adopted stem");
                return;
            }
        };
        let actor = stems_api::DAEMON_ACTOR;
        match evaluate(&core, &ws.workspace, &stem, &env, actor, None).await {
            Ok(o) if cell.info().generation == generation && cell.state().is_running() => {
                publish(&core, &stem.name, o, actor);
            }
            Ok(_) | Err(EvalError::Cancelled) => {}
            Err(EvalError::Failed(e)) => {
                tracing::warn!(stem = %stem.name, error = %e, "outputs of an adopted stem");
            }
        }
    });
}

/// `status`: the stem's outputs, secrets redacted.
pub(crate) fn shown(core: &Core, stem: &str) -> BTreeMap<String, String> {
    core.outputs
        .get(stem)
        .iter()
        .map(|(k, e)| (k.clone(), e.shown()))
        .collect()
}

/// Redact, in a dependant's displayed env, the `STEMS_<DEP>_OUTPUT_<X>` of
/// secret outputs and any value containing a secret output's value.
pub(crate) fn redact_env(core: &Core, stem: &Stem, env: &mut BTreeMap<String, String>) {
    let mut secrets: Vec<(String, String)> = Vec::new();
    for dep in &stem.depends_on {
        for (name, e) in core.outputs.get(&dep.stem) {
            if e.secret {
                secrets.push((output_var(&dep.stem, &name), e.value));
            }
        }
    }
    if secrets.is_empty() {
        return;
    }
    for (k, v) in env.iter_mut() {
        let hit = secrets
            .iter()
            .any(|(var, val)| k == var || (!val.is_empty() && v.contains(val.as_str())));
        if hit {
            *v = REDACTED.to_string();
        }
    }
}

impl Supervisor {
    /// `outputs {stems?, reveal?}`: evaluated outputs of the stems that
    /// declare any (secrets redacted unless `reveal`; `value: null` until
    /// evaluated).
    pub fn outputs(&self, p: &OutputsParams, actor: &str) -> Result<OutputsResult, Error> {
        let ws = self.workspace(false, actor)?;
        if !p.stems.is_empty() {
            super::schedule::plan(&ws, &p.stems, true)?;
        }
        let stems = ws
            .workspace
            .stems()
            .filter(|s| {
                if p.stems.is_empty() {
                    !s.outputs.is_empty()
                } else {
                    p.stems.contains(&s.name)
                }
            })
            .map(|s| {
                let have = self.core.outputs.get(&s.name);
                StemOutputs {
                    name: s.name.clone(),
                    outputs: s
                        .outputs
                        .iter()
                        .map(|(n, o)| OutputValue {
                            name: n.clone(),
                            value: have
                                .get(n)
                                .map(|e| if p.reveal { e.value.clone() } else { e.shown() }),
                            secret: o.is_secret(),
                        })
                        .collect(),
                }
            })
            .collect();
        Ok(OutputsResult { stems })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vars() {
        assert_eq!(
            output_var("shop-api", "TOKEN"),
            "STEMS_SHOP_API_OUTPUT_TOKEN"
        );
        assert_eq!(output_var("api", "url"), "STEMS_API_OUTPUT_url");
    }

    #[test]
    fn output_refs() {
        let look = |n: &str, x: &str| (n == "api" && x == "URL").then(|| "http://h:1".to_string());
        let mut missing = Vec::new();
        let s = render_output_refs(
            "u=${stem.api.outputs.URL}/x ${stem.api.port} ${stem.self.outputs.T} ${stem.api.outputs.URL}",
            "web",
            &look,
            &mut missing,
        );
        assert_eq!(
            s,
            "u=http://h:1/x ${stem.api.port} ${stem.self.outputs.T} http://h:1"
        );
        assert_eq!(missing, ["stem.web.outputs.T"]);
        let mut m = Vec::new();
        assert_eq!(
            render_output_refs("${stem.api.outputs.URL", "w", &look, &mut m),
            "${stem.api.outputs.URL"
        );
    }

    #[test]
    fn static_outputs_render_ports_and_outputs() {
        let mut port = |n: &str, p: Option<&str>| match (n, p) {
            ("api", None) => Some(18080),
            _ => None,
        };
        let out = |n: &str, x: &str| (n == "db" && x == "URL").then(|| "pg://db".to_string());
        assert_eq!(
            render_static(
                "http://localhost:${stem.self.port}?db=${stem.db.outputs.URL}",
                "api",
                &mut port,
                &out
            ),
            "http://localhost:18080?db=pg://db"
        );
        assert_eq!(
            render_static("${stem.nope.port}", "api", &mut port, &out),
            "${stem.nope.port}"
        );
    }

    #[test]
    fn redaction() {
        let e = OutputEntry {
            value: "tok-1".into(),
            secret: true,
        };
        assert_eq!(e.shown(), REDACTED);
        let e = OutputEntry {
            value: "v".into(),
            secret: false,
        };
        assert_eq!(e.shown(), "v");

        let store = OutputStore::default();
        store.set(
            "api",
            [
                (
                    "TOKEN".to_string(),
                    OutputEntry {
                        value: "tok-1".into(),
                        secret: true,
                    },
                ),
                (
                    "URL".to_string(),
                    OutputEntry {
                        value: "http://x".into(),
                        secret: false,
                    },
                ),
            ]
            .into(),
        );
        assert_eq!(store.value("api", "URL").as_deref(), Some("http://x"));
        assert!(store.clear("api"));
        assert!(!store.clear("api"));
        assert!(store.get("api").is_empty());
    }
}
