//! `stems run | scripts` (deliverable 17, `docs/scripts.md`).
//!
//! * **`run <stem> <script> [-- args…]`** / **`run --ws <script> [-- args…]`**
//!   calls `run_script` (starting the daemon if none runs, and stopping it
//!   again afterwards when nothing is running). Human mode streams the
//!   script's log lines (`subscribe_logs`, filtered by stem and tag) while it
//!   runs and ends with `✓ <script> finished in 1.2s` / `✗ <script> failed:
//!   exit 3`. `data = RunScriptResult { run_id, stem, script, ok, exit,
//!   signal, duration_ms, timed_out, attempts, argv, tail, queued }`; a
//!   failed script is `SCRIPT_FAILED` (exit 1). `--no-wait` returns
//!   `RunScriptAccepted { run_id, stem, script }` at once.
//! * **`scripts [stem]`** lists the script catalogue (workspace scripts and
//!   every enabled stem's, lifecycle and custom, with descriptions and
//!   argument schemas). It needs no daemon: the config is loaded locally.
//!   `data = ScriptCatalogResult { scripts: [{stem, name, description, args,
//!   requires, kind, timeout, retries, concurrent, mcp_tool, input_schema}] }`.

use std::io::Write;
use std::time::Duration;

use futures::StreamExt;
use serde::Serialize;
use serde_json::Value;
use stems_api::{
    CatalogScript, LogFilter, Method, RunScriptAccepted, RunScriptParams, RunScriptResult,
    ScriptArgsInput, ScriptCatalogResult, SubscribeLogsParams,
};
use stems_config::ArgType;
use stems_core::scriptargs::ScriptKind;
use stems_core::{Error, Errors};

use crate::cli::{RunArgs, ScriptsArgs};
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::commands::lifecycle::{connect_or_start, shutdown_if_idle};
use crate::output::{CommandOutput, Mode};

/// Scripts may run long; the RPC is bounded generously.
const LONG: Duration = Duration::from_secs(24 * 3600);
/// After the result, keep printing log lines until none arrived for this long.
const LOG_IDLE: Duration = Duration::from_millis(150);
/// … but never longer than this.
const LOG_DRAIN_MAX: Duration = Duration::from_secs(1);

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn parse_wait(raw: Option<&str>) -> Result<Option<u64>, Error> {
    raw.map(|s| {
        s.parse::<stems_config::Dur>()
            .map(|d| d.as_duration().as_millis() as u64)
            .map_err(|e| Error::usage(e, "use e.g. `--wait 90s`"))
    })
    .transpose()
}

/// `1.2s`, `85ms`, `2m03s`.
pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{:02}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// The last line of `stems run` in human mode.
pub fn summary_line(r: &RunScriptResult) -> String {
    let tries = if r.attempts > 1 {
        format!(" after {} attempts", r.attempts)
    } else {
        String::new()
    };
    if r.ok {
        return format!(
            "✓ {} finished in {}{tries}",
            r.script,
            format_duration(r.duration_ms)
        );
    }
    let how = if r.timed_out {
        "timed out".to_string()
    } else {
        match (r.exit, r.signal) {
            (Some(c), _) => format!("exit {c}"),
            (None, Some(s)) => format!("killed by signal {s}"),
            _ => "unknown status".into(),
        }
    };
    format!(
        "✗ {} failed: {how} ({}){tries}",
        r.script,
        format_duration(r.duration_ms)
    )
}

/// `stems run`.
pub fn run(ctx: &Ctx, args: &RunArgs, mode: Mode, stdout: &mut dyn Write) -> CommandOutput {
    block_on(run_async(ctx, args, mode, stdout)).unwrap_or_else(CommandOutput::failed)
}

async fn run_async(
    ctx: &Ctx,
    args: &RunArgs,
    mode: Mode,
    stdout: &mut dyn Write,
) -> Result<CommandOutput, Errors> {
    let (stem, name) = match (&args.workspace_script, &args.stem, &args.script) {
        (Some(w), _, _) => (None, w.clone()),
        (None, Some(s), Some(n)) => (Some(s.clone()), n.clone()),
        _ => {
            return Err(Error::usage(
                "`stems run` needs a stem and a script, or `--ws <script>`",
                "e.g. `stems run shop-api create-test-user -- --email a@b.c`",
            )
            .into());
        }
    };
    let params = RunScriptParams {
        stem: stem.clone(),
        name: name.clone(),
        args: ScriptArgsInput::Argv(args.args.clone()),
        wait: !args.no_wait,
        start_deps: args.start_deps,
        ready_timeout_ms: parse_wait(args.wait.as_deref())?,
    };
    let t = client::target(ctx)?;
    let (c, started) = connect_or_start(ctx, &t).await?;

    if args.no_wait {
        let r: Result<RunScriptAccepted, Error> =
            c.call_with_timeout(Method::RUN_SCRIPT, &params, LONG).await;
        let r = match r {
            Ok(r) => r,
            Err(e) => {
                if started {
                    shutdown_if_idle(&t, &c).await;
                }
                return Err(e.into());
            }
        };
        let human = format!(
            "started {} (run {}); follow it with `stems events -f`\n",
            r.script, r.run_id
        );
        return Ok(CommandOutput::data(to_value(&r)).with_human(human));
    }

    let mut logs = if mode == Mode::Human {
        let owner = stem
            .clone()
            .unwrap_or_else(|| stems_daemon::scripts::WORKSPACE_LOG_STEM.to_string());
        let p = SubscribeLogsParams {
            filter: LogFilter {
                stems: vec![owner],
                script: Some(name.clone()),
                ..LogFilter::default()
            },
        };
        c.subscribe_logs(p).await.ok()
    } else {
        None
    };
    let call = c.call_with_timeout::<RunScriptResult>(Method::RUN_SCRIPT, &params, LONG);
    tokio::pin!(call);
    let result = loop {
        let next = async {
            match logs.as_mut() {
                Some(l) => l.next().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            r = &mut call => break r,
            Some(rec) = next => {
                let _ = writeln!(stdout, "{}", rec.text);
                let _ = stdout.flush();
            }
        }
    };
    if let (Ok(_), Some(l)) = (&result, logs.as_mut()) {
        let _ = tokio::time::timeout(LOG_DRAIN_MAX, async {
            while let Ok(Some(rec)) = tokio::time::timeout(LOG_IDLE, l.next()).await {
                let _ = writeln!(stdout, "{}", rec.text);
                let _ = stdout.flush();
            }
        })
        .await;
    }
    if started {
        shutdown_if_idle(&t, &c).await;
    }
    let r = result?;
    let mut out = CommandOutput::data(to_value(&r)).with_human(format!("{}\n", summary_line(&r)));
    if let Some(e) = &r.error {
        out = out.with_errors(Errors(vec![e.clone()]));
    }
    Ok(out)
}

fn arg_label(a: &stems_config::ScriptArg) -> String {
    let ty = match a.kind {
        ArgType::Enum => a.values.join("|"),
        ArgType::String => "string".into(),
        ArgType::Int => "int".into(),
        ArgType::Float => "float".into(),
        ArgType::Bool => "bool".into(),
        ArgType::Path => "path".into(),
    };
    let mut s = format!("--{} <{ty}>", a.name);
    if let Some(d) = &a.default {
        s.push_str(&format!("={d}"));
    } else if a.required {
        s.push('!');
    }
    s
}

/// The `stems scripts` table: STEM SCRIPT KIND DESCRIPTION ARGS.
pub fn catalog_table(scripts: &[CatalogScript]) -> String {
    if scripts.is_empty() {
        return "no scripts\n".into();
    }
    let rows: Vec<[String; 5]> = scripts
        .iter()
        .map(|s| {
            let e = &s.entry;
            [
                e.stem.clone().unwrap_or_else(|| "(workspace)".into()),
                e.name.clone(),
                match e.kind {
                    ScriptKind::Lifecycle => "lifecycle".into(),
                    ScriptKind::Custom => "custom".into(),
                },
                e.description.clone().unwrap_or_else(|| "-".into()),
                if e.args.is_empty() {
                    "-".into()
                } else {
                    e.args.iter().map(arg_label).collect::<Vec<_>>().join(" ")
                },
            ]
        })
        .collect();
    let header = ["STEM", "SCRIPT", "KIND", "DESCRIPTION", "ARGS"];
    let mut w = header.map(str::len);
    for r in &rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    let line = |cells: [&str; 5]| {
        let mut s = String::new();
        for (i, c) in cells.iter().enumerate() {
            if i == 4 {
                s.push_str(c);
            } else {
                s.push_str(&format!("{c:<width$}  ", width = w[i]));
            }
        }
        s.trim_end().to_string() + "\n"
    };
    let mut out = line(header);
    for r in &rows {
        out.push_str(&line([&r[0], &r[1], &r[2], &r[3], &r[4]]));
    }
    out
}

/// `stems scripts` (no daemon needed).
pub fn scripts(ctx: &Ctx, args: &ScriptsArgs) -> CommandOutput {
    let r: Result<ScriptCatalogResult, Errors> = (|| {
        let ws = stems_config::load(ctx.load_options()).map_err(Errors::from)?;
        Ok(stems_daemon::supervisor::run::catalog(
            &ws,
            args.stem.as_deref(),
        )?)
    })();
    match r {
        Ok(r) => CommandOutput::data(to_value(&r)).with_human(catalog_table(&r.scripts)),
        Err(e) => CommandOutput::failed(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(ok: bool, exit: Option<i32>, attempts: u32, ms: u64) -> RunScriptResult {
        RunScriptResult {
            run_id: "01J".into(),
            stem: Some("api".into()),
            script: "seed".into(),
            ok,
            exit,
            signal: None,
            duration_ms: ms,
            timed_out: false,
            attempts,
            argv: Vec::new(),
            tail: Vec::new(),
            queued: false,
            error: None,
        }
    }

    fn hello_shop() -> stems_config::Resolved {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        stems_config::load(stems_config::LoadOptions {
            workspace: Some(root.join("examples/workspaces/hello-shop")),
            cwd: root.clone(),
            env: std::collections::HashMap::new(),
            skip_local: true,
        })
        .expect("hello-shop loads")
    }

    /// The catalogue JSON 30 (TUI) and 31 (MCP) consume, for hello-shop.
    #[test]
    fn hello_shop_catalog_golden() {
        let r = stems_daemon::supervisor::run::catalog(&hello_shop(), None).unwrap();
        insta::assert_snapshot!(
            "hello_shop_catalog",
            serde_json::to_string_pretty(&r).unwrap()
        );
        insta::assert_snapshot!("hello_shop_catalog_table", catalog_table(&r.scripts));
        let one = stems_daemon::supervisor::run::catalog(&hello_shop(), Some("postgres")).unwrap();
        assert!(
            one.scripts
                .iter()
                .all(|s| s.entry.stem.as_deref() == Some("postgres"))
        );
        assert_eq!(one.scripts.len(), 3);
    }

    #[test]
    fn summary_lines() {
        insta::assert_snapshot!(
            [
                summary_line(&result(true, Some(0), 1, 1234)),
                summary_line(&result(true, Some(0), 3, 85)),
                summary_line(&result(false, Some(3), 1, 61_000)),
                summary_line(&RunScriptResult {
                    timed_out: true,
                    ..result(false, None, 2, 300)
                }),
            ]
            .join("\n"),
            @r"
        ✓ seed finished in 1.2s
        ✓ seed finished in 85ms after 3 attempts
        ✗ seed failed: exit 3 (1m01s)
        ✗ seed failed: timed out (300ms) after 2 attempts
        "
        );
    }
}
