//! The daemon's shutdown budget: how long its orderly exit may spend stopping
//! stems before it gives up and exits anyway.
//!
//! `daemon.shutdown_timeout` when set; otherwise [`default_budget`]:
//! [`SHUTDOWN_SLACK`] plus, for each stop layer (dependents before their
//! dependencies, the layers `down` stops one after the other), the longest
//! [`stop_cost`] among its running stems. So the deadline is never shorter
//! than the stop grace of a stem that is still running and no container is
//! abandoned by it. The `shutdown` RPC returns the budget so `stems daemon
//! stop` waits as long as the daemon may take.

use std::collections::HashSet;
use std::time::Duration;

use stems_config::{Stem, Workspace};

/// Headroom on top of the stems' stop costs: script cancellation, stop
/// hooks without a timeout, failed-container cleanup, log flush.
pub const SHUTDOWN_SLACK: Duration = Duration::from_secs(30);

/// The derived budget is never below this while a stem is running: Docker
/// stops many containers slower than one, and a budget longer than the
/// shutdown costs nothing (the wait ends when the stems do). The old fixed
/// deadline was 60 s and a 50-stem workspace was seen to take about that.
pub const SHUTDOWN_FLOOR: Duration = Duration::from_secs(120);

/// Stop grace used for a running stem the daemon has no config for (no
/// workspace loaded): the supervisor's own fallback.
pub const UNKNOWN_STEM_GRACE: Duration = Duration::from_secs(10);

/// Worst-case time to stop one stem: its `pre_stop` timeout, `stop_grace`
/// and `post_stop` timeout (a hook without a timeout counts as zero; the
/// slack covers it).
pub fn stop_cost(stem: &Stem) -> Duration {
    let hook = |name: &str| {
        stem.scripts
            .get(name)
            .and_then(|s| s.timeout)
            .map_or(Duration::ZERO, |d| d.as_duration())
    };
    hook("pre_stop") + stem.stop_grace.as_duration() + hook("post_stop")
}

/// The derived budget for stopping `running` (stem names) in `ws`:
/// [`SHUTDOWN_SLACK`] + Σ over stop layers of the longest [`stop_cost`] of
/// the layer's running stems. Without a workspace (or for a name it does not
/// know) a stem costs [`UNKNOWN_STEM_GRACE`], all of them in one layer.
/// At least [`SHUTDOWN_FLOOR`] when anything is running.
pub fn default_budget(ws: Option<&Workspace>, running: &[String]) -> Duration {
    if running.is_empty() {
        return SHUTDOWN_SLACK;
    }
    layered_budget(ws, running).max(SHUTDOWN_FLOOR)
}

/// [`SHUTDOWN_SLACK`] + Σ over stop layers of the longest [`stop_cost`],
/// before the floor.
pub fn layered_budget(ws: Option<&Workspace>, running: &[String]) -> Duration {
    let mut left: HashSet<&str> = running.iter().map(String::as_str).collect();
    let mut total = SHUTDOWN_SLACK;
    if let Some(ws) = ws {
        for layer in stems_core::graph::stop_order(ws).unwrap_or_default() {
            let longest = layer
                .iter()
                .filter(|n| left.remove(n.as_str()))
                .filter_map(|n| ws.stem(n))
                .map(stop_cost)
                .max()
                .unwrap_or(Duration::ZERO);
            total += longest;
        }
        // Running stems outside the graph (disabled since they started, or
        // unknown): stopped first, in parallel.
        let rest = left
            .iter()
            .map(|n| ws.stem(n).map_or(UNKNOWN_STEM_GRACE, stop_cost))
            .max()
            .unwrap_or(Duration::ZERO);
        total += rest;
    } else if !left.is_empty() {
        total += UNKNOWN_STEM_GRACE;
    }
    total
}

/// The shutdown budget: `daemon.shutdown_timeout` when the workspace sets
/// it, else [`default_budget`].
pub fn budget(ws: Option<&Workspace>, running: &[String]) -> Duration {
    ws.and_then(|w| w.daemon.shutdown_timeout)
        .map(|d| d.as_duration())
        .unwrap_or_else(|| default_budget(ws, running))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(yaml: &str) -> Workspace {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("stems.yaml"), yaml).unwrap();
        let r = stems_config::load(stems_config::LoadOptions {
            workspace: Some(d.path().to_path_buf()),
            cwd: d.path().to_path_buf(),
            env: Default::default(),
            skip_local: true,
        })
        .unwrap();
        r.workspace
    }

    const YAML: &str = r#"
name: t
stems:
  db:
    type: process
    command: sleep 100
    stop_grace: 20s
  cache:
    type: process
    command: sleep 100
    stop_grace: 5s
  api:
    type: process
    command: sleep 100
    depends_on: [db, cache]
    stop_grace: 10s
    scripts:
      pre_stop: { command: "true", timeout: 3s }
      post_stop: { command: "true", timeout: 2s }
  web:
    type: process
    command: sleep 100
    depends_on: [api]
"#;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn stop_cost_adds_the_stop_hook_timeouts() {
        let w = ws(YAML);
        assert_eq!(stop_cost(w.stem("api").unwrap()), Duration::from_secs(15));
        assert_eq!(stop_cost(w.stem("db").unwrap()), Duration::from_secs(20));
    }

    #[test]
    fn default_budget_sums_the_longest_stop_of_each_layer() {
        let w = ws(YAML);
        // web (default 10s) → api (15s) → max(db 20s, cache 5s) + 30s slack.
        assert_eq!(
            layered_budget(Some(&w), &names(&["web", "api", "db", "cache"])),
            Duration::from_secs(10 + 15 + 20 + 30)
        );
        // Only what runs counts.
        assert_eq!(
            layered_budget(Some(&w), &names(&["cache"])),
            Duration::from_secs(5 + 30)
        );
        assert_eq!(layered_budget(Some(&w), &[]), SHUTDOWN_SLACK);
        // A running stem the config no longer has.
        assert_eq!(
            layered_budget(Some(&w), &names(&["gone"])),
            UNKNOWN_STEM_GRACE + SHUTDOWN_SLACK
        );
        // The default floors the layered sum while anything runs.
        assert_eq!(default_budget(Some(&w), &names(&["cache"])), SHUTDOWN_FLOOR);
        assert_eq!(default_budget(Some(&w), &[]), SHUTDOWN_SLACK);
    }

    #[test]
    fn default_budget_without_a_workspace() {
        assert_eq!(default_budget(None, &[]), SHUTDOWN_SLACK);
        assert_eq!(
            layered_budget(None, &names(&["a", "b"])),
            UNKNOWN_STEM_GRACE + SHUTDOWN_SLACK
        );
        assert_eq!(default_budget(None, &names(&["a", "b"])), SHUTDOWN_FLOOR);
    }

    #[test]
    fn shutdown_timeout_overrides_the_default() {
        let w = ws(&format!("{YAML}daemon:\n  shutdown_timeout: 7s\n"));
        assert_eq!(
            budget(Some(&w), &names(&["web", "db"])),
            Duration::from_secs(7)
        );
        let w = ws(YAML);
        assert_eq!(budget(Some(&w), &names(&["db"])), SHUTDOWN_FLOOR);
    }
}
