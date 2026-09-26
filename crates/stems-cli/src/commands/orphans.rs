//! Orphan handling shared by `stems up` (automatic scan) and
//! `stems doctor --orphans` (FR-CR-4, deliverable 11; `docs/recovery.md`).
//!
//! The scan itself is [`stems_daemon::orphans::scan`]: listeners on the
//! workspace's declared ports that the state file does not know. What to do
//! with each orphan is decided here:
//!
//! | flags | orphan matching the stem's start command | any other orphan |
//! |---|---|---|
//! | none, `--json` or no terminal | reported (`ORPHANS_FOUND`, exit 3) | reported |
//! | none, human on a terminal | prompt `adopt / kill / ignore` | prompt `kill / ignore` |
//! | `--yes` / `--kill-orphans` | killed | ignored |
//! | `--adopt-orphans` (`up`) | adopted by the daemon | ignored |
//! | `--kill-foreign` | killed (or adopted with `--adopt-orphans`) | killed |
//!
//! Each orphan in `--json` output is the scan's object plus `action`
//! (`killed`, `adopted`, `ignored`, `kill_failed`, `adopt_failed`, and for
//! containers `removed` / `remove_failed`) and, on failure, `error`.
//!
//! Container orphans (14/15: `kind: container`, labelled for the workspace
//! or in a stems-owned compose project, so `matches_start_command` is
//! true) are "killed" by removing the container; they are never adopted
//! here (a daemon adopts its own recorded containers on start).

use std::io::{BufRead, IsTerminal, Write};
use std::time::Duration;

use serde_json::{Value, json};
use stems_api::Method;
use stems_api::client::Client;
use stems_core::{Error, ErrorCode, Errors};
use stems_daemon::orphans::{self, Orphan, OrphanKind};
use stems_daemon::state::StateFile;

use crate::client::Target;
use crate::commands::Ctx;

/// SIGTERM grace when killing an orphan.
pub const KILL_GRACE: Duration = Duration::from_secs(2);

/// What the user allowed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// Adopt orphans that match their stem's start command.
    pub adopt: bool,
    /// Kill orphans that match their stem's start command.
    pub kill_matching: bool,
    /// Kill everything else too.
    pub kill_foreign: bool,
    /// Ask per orphan (human mode on a terminal, no flags).
    pub interactive: bool,
}

impl Policy {
    /// Build from flags; `json` disables prompting.
    pub fn new(yes: bool, adopt: bool, kill: bool, kill_foreign: bool, json: bool) -> Self {
        let any = yes || adopt || kill || kill_foreign;
        Self {
            adopt,
            kill_matching: yes || kill || (kill_foreign && !adopt),
            kill_foreign,
            interactive: !any && !json && std::io::stdin().is_terminal(),
        }
    }

    /// No flag and no prompt: orphans are only reported.
    pub fn report_only(&self) -> bool {
        !self.adopt && !self.kill_matching && !self.kill_foreign && !self.interactive
    }
}

/// The decision for one orphan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Adopt,
    Kill,
    Ignore,
}

/// Scan `t`'s workspace (config loaded with local overrides) against its
/// state file. `ignore` are pids never reported (the daemon).
pub fn scan(ctx: &Ctx, t: &Target, ignore: &[i32]) -> Result<Vec<Orphan>, Errors> {
    let resolved =
        stems_core::load_and_validate(ctx.load_options(), &stems_core::ValidateOptions::default())?;
    let state = StateFile::peek(&t.paths.state);
    let mut ignore = ignore.to_vec();
    ignore.push(std::process::id() as i32);
    Ok(orphans::scan(&resolved.workspace, state.as_ref(), &ignore))
}

/// Container orphans (14/15) of `t`'s workspace: labelled containers and
/// containers of stems-owned compose projects not in its state file.
/// Empty when the workspace has no docker/compose stem, its config does
/// not load, or Docker is not reachable (never an error). `running_only`:
/// see [`orphans::scan_containers`].
pub async fn scan_containers(ctx: &Ctx, t: &Target, running_only: bool) -> Vec<Orphan> {
    let Ok(resolved) = stems_config::load(ctx.load_options()) else {
        return Vec::new();
    };
    let state = StateFile::peek(&t.paths.state);
    orphans::scan_containers(
        &resolved.workspace,
        state.as_ref(),
        ctx.env.get("DOCKER_HOST").cloned(),
        &t.paths.dir.join("compose"),
        running_only,
    )
    .await
}

/// Does `up` of `stems` (with hard dependencies) include a docker or
/// compose stem? Only then does `up` scan for container orphans.
pub fn selection_needs_docker(ctx: &Ctx, stems: &[String]) -> bool {
    let Ok(resolved) = stems_config::load(ctx.load_options()) else {
        return false;
    };
    let Ok(plan) = stems_daemon::supervisor::schedule::plan(&resolved, stems, false) else {
        return false;
    };
    plan.order.iter().any(|n| {
        resolved.workspace.stem(n).is_some_and(|s| {
            matches!(
                s.kind(),
                stems_config::StemType::Docker | stems_config::StemType::Compose
            )
        })
    })
}

/// Ports of `orphans`, for messages.
fn ports(orphans: &[Orphan]) -> String {
    let mut v: Vec<String> = orphans
        .iter()
        .filter_map(|o| o.port.map(|p| p.to_string()))
        .collect();
    v.dedup();
    v.join(", ")
}

/// `ORPHANS_FOUND` (exit 3) with `details.orphans`.
pub fn found_error(orphans: &[Value], hint: &str) -> Error {
    let n = orphans.len();
    let ports: Vec<String> = orphans
        .iter()
        .filter_map(|o| o.get("port").and_then(Value::as_u64).map(|p| p.to_string()))
        .collect();
    let containers = orphans
        .iter()
        .filter(|o| o.get("kind").and_then(Value::as_str) == Some("container"))
        .count();
    let message = if containers == 0 {
        format!(
            "{n} process{} not started by stems listen{} on this workspace's ports ({})",
            if n == 1 { "" } else { "es" },
            if n == 1 { "s" } else { "" },
            ports.join(", ")
        )
    } else {
        format!(
            "{n} orphan{} of this workspace not recorded by stems ({containers} container{}{})",
            if n == 1 { "" } else { "s" },
            if containers == 1 { "" } else { "s" },
            if ports.is_empty() {
                String::new()
            } else {
                format!("; processes on ports {}", ports.join(", "))
            }
        )
    };
    Error::new(ErrorCode::OrphansFound, message)
        .with_hint(hint.to_string())
        .with_details(json!({ "orphans": orphans }))
}

fn describe(o: &Orphan) -> String {
    format!(
        "pid {} on port {} (stem {}): {}{}",
        o.pid.unwrap_or(0),
        o.port.map_or("-".into(), |p| p.to_string()),
        o.stem.as_deref().unwrap_or("-"),
        o.command,
        if o.matches_start_command {
            "  [looks like the stem's start command]"
        } else {
            ""
        }
    )
}

/// Ask on stderr / stdin.
fn prompt(o: &Orphan, can_adopt: bool) -> Action {
    let mut err = std::io::stderr();
    let choices = if can_adopt {
        "adopt / kill / ignore? [a/k/I] "
    } else {
        "kill / ignore? [k/I] "
    };
    let _ = write!(err, "orphan: {}\n  {choices}", describe(o));
    let _ = err.flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return Action::Ignore;
    }
    match line.trim().to_lowercase().as_str() {
        "a" | "adopt" if can_adopt => Action::Adopt,
        "k" | "kill" => Action::Kill,
        _ => Action::Ignore,
    }
}

/// Decide what to do with `o`. Only orphans matching their stem's start
/// command can be adopted.
pub fn decide(o: &Orphan, p: &Policy, can_adopt: bool) -> Action {
    // Containers are never adopted through `adopt_orphans` (processes only).
    let adoptable =
        can_adopt && o.kind == OrphanKind::Process && o.matches_start_command && o.stem.is_some();
    if p.interactive {
        return prompt(o, adoptable);
    }
    if o.matches_start_command {
        if p.adopt && adoptable {
            Action::Adopt
        } else if p.kill_matching {
            Action::Kill
        } else {
            Action::Ignore
        }
    } else if p.kill_foreign {
        Action::Kill
    } else {
        Action::Ignore
    }
}

fn with_action(o: &Orphan, action: &str, error: Option<&Error>) -> Value {
    let mut v = serde_json::to_value(o).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut v {
        m.insert("action".into(), json!(action));
        if let Some(e) = error {
            m.insert("error".into(), json!(e));
        }
    }
    v
}

/// Apply `policy` to `orphans`. Adoption goes through `daemon` (required
/// for it; without a daemon adoptable orphans are left alone). "Kill" of a
/// container orphan removes the container (volumes kept; `docker_host` as
/// the caller's `DOCKER_HOST`). Returns each orphan with its `action`.
pub async fn resolve(
    orphans: &[Orphan],
    policy: &Policy,
    daemon: Option<&Client>,
    docker_host: Option<String>,
) -> Vec<Value> {
    let mut out = Vec::new();
    for o in orphans {
        match decide(o, policy, daemon.is_some()) {
            Action::Ignore => out.push(with_action(o, "ignored", None)),
            Action::Kill if o.kind == OrphanKind::Container => {
                match orphans::remove_container(o, docker_host.clone()).await {
                    Ok(()) => out.push(with_action(o, "removed", None)),
                    Err(e) => {
                        let e = Error::internal(format!(
                            "cannot remove container {}: {e}",
                            o.container_id.as_deref().unwrap_or("?")
                        ));
                        out.push(with_action(o, "remove_failed", Some(&e)));
                    }
                }
            }
            Action::Kill => match orphans::kill(o, KILL_GRACE).await {
                Ok(_) => out.push(with_action(o, "killed", None)),
                Err(e) => {
                    let e = Error::internal(format!("cannot kill pid {:?}: {e}", o.pid));
                    out.push(with_action(o, "kill_failed", Some(&e)));
                }
            },
            Action::Adopt => {
                let Some(c) = daemon else {
                    out.push(with_action(o, "ignored", None));
                    continue;
                };
                let r: Result<Value, Error> = c
                    .call(
                        Method::ADOPT_ORPHANS,
                        json!({ "orphans": [{ "stem": o.stem, "pid": o.pid }] }),
                    )
                    .await;
                let err = match r {
                    Ok(v) => v
                        .get("failed")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(|f| f.get("error"))
                        .and_then(|e| serde_json::from_value::<Error>(e.clone()).ok()),
                    Err(e) => Some(e),
                };
                match err {
                    None => out.push(with_action(o, "adopted", None)),
                    Some(e) => out.push(with_action(o, "adopt_failed", Some(&e))),
                }
            }
        }
    }
    out
}

/// Human lines for resolved orphans.
pub fn human(resolved: &[Value]) -> String {
    let mut s = String::new();
    for v in resolved {
        s.push_str(&format!(
            "  {:<12} pid {} port {} stem {}: {}{}\n",
            v["action"].as_str().unwrap_or("?"),
            v["pid"],
            v["port"],
            v["stem"].as_str().unwrap_or("-"),
            v["command"].as_str().unwrap_or(""),
            if v["matches_start_command"] == json!(true) {
                "  [start command]"
            } else {
                ""
            }
        ));
    }
    s
}

/// The ports summary of orphans that are still there (not killed/adopted).
pub fn remaining(resolved: &[Value]) -> Vec<Value> {
    resolved
        .iter()
        .filter(|v| !matches!(v["action"].as_str(), Some("killed" | "adopted" | "removed")))
        .cloned()
        .collect()
}

/// `ports` for the human header.
pub fn header(orphans: &[Orphan]) -> String {
    format!(
        "{} orphan{} on port{} {}\n",
        orphans.len(),
        if orphans.len() == 1 { "" } else { "s" },
        if orphans.len() == 1 { "" } else { "s" },
        ports(orphans)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_daemon::orphans::OrphanKind;

    fn orphan(matches: bool) -> Orphan {
        Orphan {
            kind: OrphanKind::Process,
            port: Some(18090),
            pid: Some(123),
            pgid: Some(123),
            command: "python3 app.py".into(),
            container_id: None,
            stem: Some("echo-svc".into()),
            matches_start_command: matches,
        }
    }

    #[test]
    fn policy_table() {
        let yes = Policy::new(true, false, false, false, true);
        assert_eq!(decide(&orphan(true), &yes, true), Action::Kill);
        assert_eq!(decide(&orphan(false), &yes, true), Action::Ignore);
        let foreign = Policy::new(true, false, false, true, true);
        assert_eq!(decide(&orphan(false), &foreign, true), Action::Kill);
        let adopt = Policy::new(false, true, false, false, true);
        assert_eq!(decide(&orphan(true), &adopt, true), Action::Adopt);
        assert_eq!(decide(&orphan(true), &adopt, false), Action::Ignore);
        assert_eq!(decide(&orphan(false), &adopt, true), Action::Ignore);
        let none = Policy::new(false, false, false, false, true);
        assert!(none.report_only());
        assert_eq!(decide(&orphan(true), &none, true), Action::Ignore);
    }

    #[test]
    fn found_error_shape() {
        let v = vec![with_action(&orphan(true), "ignored", None)];
        let e = found_error(&v, "h");
        assert_eq!(e.code, ErrorCode::OrphansFound);
        assert_eq!(e.exit_code(), 3);
        assert_eq!(e.details["orphans"][0]["matches_start_command"], true);
        assert!(e.message.contains("18090"), "{}", e.message);
        assert_eq!(remaining(&v).len(), 1);
    }
}
