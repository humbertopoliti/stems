//! `stems graph [--format text|mermaid|dot|json] [--status|--no-status]
//! [--watch [<interval>]] [--focus <stem>] [--profile <p>] [--edges]`
//! (FR-GR-4, FR-GR-5).
//!
//! The graph comes from the workspace config (the same layered layout as the
//! TUI graph view, [`stems_core::layout`]); the glyphs come from the
//! daemon's `status` when it runs (`--status` is then the default), else
//! every stem is `·` (config only). `--status` without a daemon is
//! `DAEMON_NOT_RUNNING` (exit 4); `--no-status` never asks the daemon.
//!
//! Output: `text` is the box drawing followed by a blank line and the glyph
//! legend; `mermaid`, `dot` and `json` export. The text is printed verbatim
//! unless `--json` is explicit. JSON `data` is the graph shape
//! `{nodes: [{name, type, status, glyph, reason}], edges: [{from, to,
//! condition, soft, protocol, via}]}` ([`stems_core::render::to_json`]);
//! for the text formats it also has `format` and `output` (the rendered
//! text).
//!
//! Glyphs are Unicode unless `STEMS_ASCII` is set (on a terminal, also a
//! non-UTF-8 locale); `--no-color` only drops the ANSI colours, which are
//! used on a terminal only. The text wraps into bands at `COLUMNS` (or the
//! terminal width).
//!
//! `--watch [<interval>]` redraws until Ctrl-C (exit 0) like `status
//! --watch`: the screen is cleared on a terminal, frames are separated by a
//! blank line on a pipe, and in JSON mode each frame is one NDJSON line
//! holding `data`.

use std::io::Write;
use std::time::Duration;

use serde_json::{Value, json};
use stems_api::client::Client;
use stems_api::{Method, StatusParams, StatusResult};
use stems_core::layout::{Layout, layout};
use stems_core::render::{
    RenderOptions, graph_reason, render_dot, render_mermaid, render_text, to_json,
};
use stems_core::selection::{SelectOptions, Selection};
use stems_core::{Error, ErrorCode, Errors, Glyph};

use crate::cli::{GraphArgs, GraphFormat};
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::commands::status::{Style, parse_interval};
use crate::output::{CommandOutput, Mode};

/// Run `graph`.
pub fn run(ctx: &Ctx, args: &GraphArgs, mode: Mode, stdout: &mut dyn Write) -> CommandOutput {
    let l = match graph_layout(ctx, args) {
        Ok(l) => l,
        Err(e) => return CommandOutput::failed(e),
    };
    let look = Look::detect(ctx, args, mode);
    if let Some(raw) = &args.watch {
        let every = match parse_interval(raw) {
            Ok(d) => d,
            Err(e) => return CommandOutput::failed(e),
        };
        return block_on(watch(ctx, args, &l, &look, every, mode, stdout))
            .unwrap_or_else(CommandOutput::failed);
    }
    block_on(async {
        let mut conn = None;
        let st = live_status(ctx, args, &mut conn).await?;
        let (data, text) = frame(&l, st.as_ref(), args.format, &look);
        Ok::<_, Errors>(CommandOutput::data(data).with_raw(text))
    })
    .unwrap_or_else(CommandOutput::failed)
}

/// How frames are drawn.
struct Look {
    unicode: bool,
    color: bool,
    edges: bool,
    width: usize,
}

impl Look {
    fn detect(ctx: &Ctx, args: &GraphArgs, mode: Mode) -> Self {
        use std::io::IsTerminal;
        let style = Style::detect(ctx, mode);
        let env = |k: &str| ctx.env.get(k).cloned();
        let ascii_env = env("STEMS_ASCII")
            .is_some_and(|v| !matches!(v.trim(), "" | "0" | "false" | "no" | "off" | "n" | "f"));
        // Pipes and files get deterministic Unicode; only a terminal follows
        // the locale.
        let ascii = if std::io::stdout().is_terminal() {
            stems_core::status::prefers_ascii(env)
        } else {
            ascii_env
        };
        Self {
            unicode: !ascii,
            color: style.color,
            edges: args.edges,
            width: style.width.unwrap_or(0),
        }
    }
}

/// The layout of the workspace, restricted to `--profile` and `--focus`.
fn graph_layout(ctx: &Ctx, args: &GraphArgs) -> Result<Layout, Errors> {
    let resolved = stems_config::load(ctx.load_options()).map_err(|e| {
        let mut errors = Errors::from(e);
        errors.sort();
        errors
    })?;
    let ws = &resolved.workspace;
    let mut l = layout(ws)?;
    if let Some(p) = &args.profile {
        let sel = Selection::resolve(
            ws,
            Some(p),
            &[],
            SelectOptions {
                include_deps: true,
                strict_profiles: Some(false),
            },
        )?;
        l = l.restrict(&sel.closure);
    }
    if let Some(f) = &args.focus {
        if l.node(f).is_none() {
            let known: Vec<&str> = l.nodes.iter().map(|n| n.name.as_str()).collect();
            return Err(Error::new(
                ErrorCode::UnknownStem,
                format!("no stem named `{f}` in the graph"),
            )
            .with_hint(format!("stems in the graph: {}", known.join(", ")))
            .with_details(json!({ "stem": f, "known": known }))
            .into());
        }
        l = l.focus(f);
    }
    Ok(l)
}

/// The daemon's `status`, or `None` for a config-only graph (`--no-status`,
/// or no daemon without `--status`). Reconnects when `conn` is empty.
async fn live_status(
    ctx: &Ctx,
    args: &GraphArgs,
    conn: &mut Option<Client>,
) -> Result<Option<StatusResult>, Errors> {
    if args.no_status && !args.status {
        return Ok(None);
    }
    let c = match conn {
        Some(c) => c,
        None => match client::connect(ctx).await {
            Ok(c) => conn.insert(c),
            Err(e) if !args.status && e.0.iter().all(|x| x.code == ErrorCode::DaemonNotRunning) => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        },
    };
    match c.call(Method::STATUS, StatusParams::default()).await {
        Ok(st) => Ok(Some(st)),
        Err(e) => {
            *conn = None;
            if args.status {
                Err(e.into())
            } else {
                // The daemon went away: config only until it is back.
                Ok(None)
            }
        }
    }
}

/// One rendering: the JSON `data` and the text printed.
fn frame(
    l: &Layout,
    st: Option<&StatusResult>,
    format: GraphFormat,
    look: &Look,
) -> (Value, String) {
    let find = |name: &str| st.and_then(|st| st.stems.iter().find(|s| s.name == name));
    let short = |name: &str| match find(name) {
        Some(s) => (s.glyph, graph_reason(s.glyph, s.reason.as_deref())),
        None => (Glyph::Stopped, None),
    };
    let full = |name: &str| match find(name) {
        Some(s) => (s.glyph, s.reason.clone()),
        None => (Glyph::Stopped, None),
    };
    let data = to_json(
        l,
        st.map(|_| &full as &dyn Fn(&str) -> (Glyph, Option<String>)),
    );
    let opts = RenderOptions {
        unicode: look.unicode,
        color: look.color,
        edge_labels: look.edges,
        width: look.width,
        status: st.map(|_| &short as &dyn Fn(&str) -> (Glyph, Option<String>)),
        compact: false,
    };
    let (name, text) = match format {
        GraphFormat::Json => {
            let text = serde_json::to_string_pretty(&data).unwrap_or_default() + "\n";
            return (data, text);
        }
        GraphFormat::Mermaid => ("mermaid", render_mermaid(l, &opts)),
        GraphFormat::Dot => ("dot", render_dot(l, &opts)),
        GraphFormat::Text => {
            let mut t = render_text(l, &opts);
            if !l.nodes.is_empty() {
                t.push('\n');
            }
            t.push_str(&Glyph::legend(look.unicode));
            t.push('\n');
            ("text", t)
        }
    };
    let mut data = data;
    data["format"] = json!(name);
    data["output"] = json!(text);
    (data, text)
}

async fn watch(
    ctx: &Ctx,
    args: &GraphArgs,
    l: &Layout,
    look: &Look,
    every: Duration,
    mode: Mode,
    stdout: &mut dyn Write,
) -> Result<CommandOutput, Errors> {
    use std::io::IsTerminal;
    use tokio::signal::unix::{SignalKind, signal};
    let tty = std::io::stdout().is_terminal();
    let mut int = signal(SignalKind::interrupt()).ok();
    let mut term = signal(SignalKind::terminate()).ok();
    let mut conn = None;
    let mut first = true;
    loop {
        let res = live_status(ctx, args, &mut conn).await;
        let out = match (&res, mode) {
            (Err(e), _) if first => return Err(e.clone()),
            (Ok(st), Mode::Json) => {
                let (data, _) = frame(l, st.as_ref(), args.format, look);
                serde_json::to_string(&data).unwrap_or_default() + "\n"
            }
            (Ok(st), Mode::Human) => frame(l, st.as_ref(), args.format, look).1,
            (Err(e), Mode::Json) => {
                serde_json::to_string(&json!({ "errors": e })).unwrap_or_default() + "\n"
            }
            (Err(e), Mode::Human) => crate::output::human_errors(e),
        };
        let prefix = match mode {
            Mode::Human if tty => "\x1b[H\x1b[2J",
            Mode::Human if !first => "\n",
            _ => "",
        };
        first = false;
        if write!(stdout, "{prefix}{out}")
            .and_then(|()| stdout.flush())
            .is_err()
        {
            break;
        }
        tokio::select! {
            () = tokio::time::sleep(every) => {}
            () = recv(&mut int) => break,
            () = recv(&mut term) => break,
        }
    }
    Ok(CommandOutput::data(json!(null))
        .with_ndjson("")
        .with_human("")
        .with_raw(""))
}

async fn recv(s: &mut Option<tokio::signal::unix::Signal>) {
    match s {
        Some(s) => {
            s.recv().await;
        }
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_api::StatusSummary;

    fn chain() -> Layout {
        let yaml = "stems:\n  web: { type: process, depends_on: [{ stem: api, protocol: http }] }\n  api: { type: process }\n";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("stems.yaml"), yaml).unwrap();
        let ws = stems_config::load(stems_config::LoadOptions {
            workspace: Some(dir.path().to_path_buf()),
            cwd: dir.path().to_path_buf(),
            env: Default::default(),
            skip_local: true,
        })
        .unwrap()
        .workspace;
        layout(&ws).unwrap()
    }

    fn look(edges: bool) -> Look {
        Look {
            unicode: true,
            color: false,
            edges,
            width: 0,
        }
    }

    fn status(glyph: &str, reason: Option<&str>) -> StatusResult {
        let stem = |name: &str, state: &str, glyph: &str, reason: Option<&str>| {
            serde_json::from_value(json!({
                "name": name, "type": "process", "state": state, "glyph": glyph,
                "reason": reason, "pid": null, "pgid": null, "ports": [],
                "uptime_s": null, "started_at": null, "restarts": 0,
                "health": null, "error": null
            }))
            .unwrap()
        };
        let stems = vec![
            stem("api", "unhealthy", "failed", Some("health: 503")),
            stem("web", "healthy", glyph, reason),
        ];
        StatusResult {
            summary: StatusSummary::of(&stems),
            stems,
        }
    }

    #[test]
    fn config_only_text_has_the_legend() {
        let (data, text) = frame(&chain(), None, GraphFormat::Text, &look(false));
        assert!(text.contains("│ web · ├"), "{text}");
        assert!(
            text.ends_with(&format!("{}\n", Glyph::legend(true))),
            "{text}"
        );
        assert_eq!(data["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(data["format"], "text");
        assert_eq!(data["output"], text);
    }

    #[test]
    fn live_glyphs_and_short_reasons() {
        let st = status("degraded", Some("dependency api unhealthy"));
        let (data, text) = frame(&chain(), Some(&st), GraphFormat::Text, &look(true));
        assert!(text.contains("│ web   ! ├"), "{text}");
        assert!(text.contains("│ dep api │"), "{text}");
        assert!(text.contains("│ api       ✗ │"), "{text}");
        assert!(text.contains("│ health: 503 │"), "{text}");
        assert!(text.contains("http"), "{text}");
        let web = data["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["name"] == "web")
            .unwrap();
        assert_eq!(web["status"], "degraded");
        assert_eq!(web["reason"], "dependency api unhealthy");
    }

    #[test]
    fn exports() {
        let (data, text) = frame(&chain(), None, GraphFormat::Json, &look(false));
        assert_eq!(data["edges"][0]["protocol"], "http");
        assert!(data.get("output").is_none());
        assert!(text.starts_with('{'));
        let (_, m) = frame(&chain(), None, GraphFormat::Mermaid, &look(false));
        assert!(m.starts_with("graph LR\n"), "{m}");
        let (_, d) = frame(&chain(), None, GraphFormat::Dot, &look(false));
        assert!(d.starts_with("digraph"), "{d}");
    }
}
