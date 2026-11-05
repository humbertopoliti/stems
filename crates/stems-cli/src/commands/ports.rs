//! `stems ports [stems...] [--format table|json|dotenv|shell] [--no-status]`
//! (deliverable 42, FR-ST-5, `docs/ports.md`): every stem's host ports and
//! the variables / templates that address them, for consumers outside stems.
//!
//! Config only, plus the live `auto` allocations when a daemon runs (never
//! started by this command; `--no-status` skips it). Disabled stems are
//! left out unless named. JSON: `data = { ports: [{stem, name, port, auto,
//! container_port, primary, var, template, running}], vars: {NAME: port} }`;
//! `port` is `null` for an `auto` port not allocated yet and `vars` holds
//! only resolved ports. `var` is `STEMS_<STEM>_PORT` for the primary port,
//! else `STEMS_<STEM>_PORT_<NAME>` (`null` for a default `port<i>` name);
//! `vars` also holds the named form of an explicitly named primary port.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use stems_api::{Method, StatusParams, StatusResult};
use stems_config::{PortRef, Workspace};
use stems_core::{Error, ErrorCode, Errors};
use stems_daemon::supervisor::env::port_var;
use stems_daemon::supervisor::ports::{is_named, named_port_var};

use crate::cli::{PortsArgs, PortsFormat};
use crate::client::{self, block_on};
use crate::commands::Ctx;
use crate::output::{CommandOutput, Mode};

/// One row of the registry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// Stem name.
    pub stem: String,
    /// Port name.
    pub name: String,
    /// Host port, when known.
    pub port: Option<u16>,
    /// Declared `auto`.
    pub auto: bool,
    /// Container-side port.
    pub container_port: Option<u16>,
    /// First port of the stem.
    pub primary: bool,
    /// The variable stems exports for it.
    pub var: Option<String>,
    /// The `${stem...}` template.
    pub template: String,
    /// The stem is running.
    pub running: bool,
}

/// Build the registry. `live` maps `(stem, port name)` to an allocated port
/// and `running` lists the running stems.
pub fn rows(
    ws: &Workspace,
    names: &[String],
    live: &BTreeMap<(String, String), u16>,
    running: &[String],
) -> Result<Vec<Row>, Errors> {
    for n in names {
        if ws.stem(n).is_none() {
            return Err(Error::new(
                ErrorCode::UnknownStem,
                format!("stem `{n}` does not exist in workspace `{}`", ws.name),
            )
            .with_hint(format!(
                "known stems: {}",
                ws.stems.keys().cloned().collect::<Vec<_>>().join(", ")
            ))
            .with_details(json!({ "stem": n }))
            .into());
        }
    }
    let mut out = Vec::new();
    for s in ws.stems.values() {
        let wanted = if names.is_empty() {
            s.enabled
        } else {
            names.contains(&s.name)
        };
        if !wanted {
            continue;
        }
        for (i, p) in s.ports.iter().enumerate() {
            let primary = i == 0;
            let (port, auto) = match p.port {
                PortRef::Fixed(n) => (Some(n), false),
                PortRef::Auto => (live.get(&(s.name.clone(), p.name.clone())).copied(), true),
            };
            let var = if primary {
                Some(port_var(&s.name))
            } else {
                is_named(i, p).then(|| named_port_var(&s.name, &p.name))
            };
            let template = if primary {
                format!("${{stem.{}.port}}", s.name)
            } else {
                format!("${{stem.{}.ports.{}}}", s.name, p.name)
            };
            out.push(Row {
                stem: s.name.clone(),
                name: p.name.clone(),
                port,
                auto,
                container_port: p.container_port,
                primary,
                var,
                template,
                running: running.contains(&s.name),
            });
        }
    }
    Ok(out)
}

/// `vars`: every resolved port under its variable name(s).
pub fn vars(ws: &Workspace, rows: &[Row]) -> BTreeMap<String, u16> {
    let mut v = BTreeMap::new();
    for r in rows {
        let Some(port) = r.port else { continue };
        if let Some(var) = &r.var {
            v.insert(var.clone(), port);
        }
        let named = ws
            .stem(&r.stem)
            .and_then(|s| s.ports.iter().position(|p| p.name == r.name))
            .is_some_and(|i| ws.stem(&r.stem).is_some_and(|s| is_named(i, &s.ports[i])));
        if named {
            v.insert(named_port_var(&r.stem, &r.name), port);
        }
    }
    v
}

/// The aligned table.
pub fn table(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "no stem declares ports\n".into();
    }
    let mut cells: Vec<[String; 6]> =
        vec![["STEM", "NAME", "PORT", "CONTAINER", "SOURCE", "VAR"].map(str::to_string)];
    for r in rows {
        cells.push([
            r.stem.clone(),
            r.name.clone(),
            r.port.map_or("-".into(), |p| p.to_string()),
            r.container_port.map_or("-".into(), |p| p.to_string()),
            (if r.auto { "auto" } else { "fixed" }).into(),
            r.var.clone().unwrap_or_else(|| "-".into()),
        ]);
    }
    let widths: Vec<usize> = (0..6)
        .map(|i| {
            cells
                .iter()
                .map(|r| r[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for row in &cells {
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

/// `dotenv` / `shell` text: sorted `NAME=value` lines.
pub fn lines(vars: &BTreeMap<String, u16>, shell: bool) -> String {
    let prefix = if shell { "export " } else { "" };
    vars.iter()
        .map(|(k, v)| format!("{prefix}{k}={v}\n"))
        .collect()
}

/// `running` in `stems ports`: the stem holds (or is about to hold) its
/// ports — `starting`, `healthy`, `unhealthy` or `seeding`.
fn counts_as_running(state: stems_core::status::StemState) -> bool {
    use stems_core::status::StemState as S;
    matches!(state, S::Starting | S::Healthy | S::Unhealthy | S::Seeding)
}

/// `stems ports`.
pub fn run(ctx: &Ctx, args: &PortsArgs, mode: Mode) -> CommandOutput {
    let resolved = match stems_config::load(ctx.load_options()) {
        Ok(r) => r,
        Err(e) => {
            let mut errors = Errors::from(e);
            errors.sort();
            return CommandOutput::failed(errors);
        }
    };
    let ws = &resolved.workspace;
    let status = if args.no_status {
        None
    } else {
        block_on(daemon_status(ctx))
    };
    let mut live = BTreeMap::new();
    let mut running = Vec::new();
    if let Some(st) = &status {
        for s in &st.stems {
            for p in &s.ports {
                if let Some(n) = p.port {
                    live.insert((s.name.clone(), p.name.clone()), n);
                }
            }
            if counts_as_running(s.state) {
                running.push(s.name.clone());
            }
        }
    }
    let rows = match rows(ws, &args.stems, &live, &running) {
        Ok(r) => r,
        Err(e) => return CommandOutput::failed(e),
    };
    let v = vars(ws, &rows);
    let data: Value = json!({
        "ports": rows.iter().map(|r| json!({
            "stem": r.stem,
            "name": r.name,
            "port": r.port,
            "auto": r.auto,
            "container_port": r.container_port,
            "primary": r.primary,
            "var": r.var,
            "template": r.template,
            "running": r.running,
        })).collect::<Vec<_>>(),
        "vars": v,
    });
    let out = CommandOutput::data(data);
    match args.format {
        PortsFormat::Table | PortsFormat::Json => out.with_human(table(&rows)),
        PortsFormat::Dotenv | PortsFormat::Shell => {
            if mode == Mode::Human {
                let skipped: Vec<String> = rows
                    .iter()
                    .filter(|r| r.port.is_none())
                    .map(|r| format!("{}.{}", r.stem, r.name))
                    .collect();
                if !skipped.is_empty() {
                    eprintln!(
                        "stems: skipped ports not allocated yet (auto, stem not started): {}",
                        skipped.join(", ")
                    );
                }
            }
            out.with_raw(lines(&v, args.format == PortsFormat::Shell))
        }
    }
}

/// The daemon's status, `None` when no daemon runs or it does not answer.
async fn daemon_status(ctx: &Ctx) -> Option<StatusResult> {
    let c = client::connect(ctx).await.ok()?;
    c.call(Method::STATUS, StatusParams::default()).await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_means_starting_healthy_unhealthy_or_seeding() {
        use stems_core::status::StemState as S;
        for s in [S::Starting, S::Healthy, S::Unhealthy, S::Seeding] {
            assert!(counts_as_running(s), "{s}");
        }
        for s in [S::Stopped, S::Setup, S::Stopping, S::Failed, S::Unknown] {
            assert!(!counts_as_running(s), "{s}");
        }
    }

    fn row(stem: &str, name: &str, port: Option<u16>, auto: bool, primary: bool) -> Row {
        Row {
            stem: stem.into(),
            name: name.into(),
            port,
            auto,
            container_port: None,
            primary,
            var: Some(if primary {
                format!("STEMS_{}_PORT", stem.to_uppercase())
            } else {
                format!("STEMS_{}_PORT_{}", stem.to_uppercase(), name.to_uppercase())
            }),
            template: String::new(),
            running: false,
        }
    }

    #[test]
    fn table_golden() {
        let mut a = row("api", "http", None, true, true);
        a.container_port = Some(80);
        let b = row("web", "admin", Some(18502), false, false);
        insta::assert_snapshot!(
            "ports-table",
            table(&[a, row("web", "http", Some(18501), false, true), b])
        );
    }

    #[test]
    fn dotenv_and_shell() {
        let mut v = BTreeMap::new();
        v.insert("STEMS_WEB_PORT".to_string(), 18501);
        v.insert("STEMS_API_PORT".to_string(), 8080);
        insta::assert_snapshot!("ports-dotenv", lines(&v, false));
        insta::assert_snapshot!("ports-shell", lines(&v, true));
    }

    #[test]
    fn empty_table() {
        assert_eq!(table(&[]), "no stem declares ports\n");
    }
}
