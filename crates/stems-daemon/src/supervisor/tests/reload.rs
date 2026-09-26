//! Config reload through the supervisor with the fake runtime (33): apply
//! order (reverse dependency order to stop, dependency order to start),
//! per-stem transactions, removed/added stems, hot watch changes, and what
//! `up` does while a change is pending.

use super::*;
use stems_api::{ConfigApplyParams, ReloadAction};

/// A host whose config on disk (`candidate`) can differ from the applied one.
struct ReloadHost {
    dir: tempfile::TempDir,
    applied: Mutex<Arc<Resolved>>,
    candidate: Mutex<Arc<Resolved>>,
}

impl ReloadHost {
    fn new(doc: &str) -> Arc<Self> {
        let dir = tempfile::tempdir().unwrap();
        let ws = Arc::new(load(dir.path(), doc));
        Arc::new(Self {
            dir,
            applied: Mutex::new(ws.clone()),
            candidate: Mutex::new(ws),
        })
    }

    /// Edit the config on disk.
    fn write(&self, doc: &str) {
        *self.candidate.lock().unwrap() = Arc::new(load(self.dir.path(), doc));
    }

    fn applied(&self) -> Arc<Resolved> {
        self.applied.lock().unwrap().clone()
    }
}

fn load(dir: &std::path::Path, doc: &str) -> Resolved {
    std::fs::write(dir.join("stems.yaml"), doc).unwrap();
    stems_config::load(stems_config::LoadOptions {
        workspace: Some(dir.to_path_buf()),
        cwd: dir.to_path_buf(),
        env: Default::default(),
        skip_local: true,
    })
    .unwrap_or_else(|e| panic!("{e}\n{doc}"))
}

impl Host for ReloadHost {
    fn resolved(&self) -> Option<Arc<Resolved>> {
        Some(self.applied())
    }
    fn reload(&self, _actor: &str) -> Result<Arc<Resolved>, Error> {
        Ok(self.applied())
    }
    fn load_candidate(
        &self,
        _actor: &str,
        _check_requires: bool,
    ) -> Result<Arc<Resolved>, stems_core::Errors> {
        Ok(self.candidate.lock().unwrap().clone())
    }
    fn install(&self, resolved: Arc<Resolved>, _actor: &str) {
        *self.applied.lock().unwrap() = resolved;
    }
    fn request_shutdown(&self, _actor: &str, _reason: &str) {}
    fn started_by_up(&self) -> bool {
        false
    }
    fn set_started_by_up(&self) {}
    fn data_dir(&self) -> Option<std::path::PathBuf> {
        Some(self.dir.path().join("data"))
    }
}

struct RRig {
    sup: Arc<Supervisor>,
    events: Arc<EventBus>,
    host: Arc<ReloadHost>,
    rt: Arc<FakeRuntime>,
}

fn rrig(doc: &str) -> RRig {
    let host = ReloadHost::new(doc);
    let events = Arc::new(EventBus::default());
    let rt = Arc::new(FakeRuntime::default());
    let mut reg = RuntimeRegistry::default();
    reg.register(StemType::Process, rt.clone());
    let sup = Supervisor::new(
        events.clone(),
        host.clone(),
        reg,
        Arc::new(FakeWaiter::default()),
    );
    RRig {
        sup,
        events,
        host,
        rt,
    }
}

fn doc(stems: &str) -> String {
    format!("schema_version: 1\nname: t\nstems:\n{stems}")
}

fn stem(name: &str, deps: &str, env: &str) -> String {
    format!(
        "  {name}: {{ type: process, command: run, depends_on: [{deps}], env: {{ V: \"{env}\" }} }}\n"
    )
}

/// db ← api ← web, and `other` alone.
fn chain(db: &str, api: &str, web: &str, other: &str) -> String {
    doc(&[
        stem("db", "", db),
        stem("api", "db", api),
        stem("web", "api", web),
        stem("other", "", other),
    ]
    .concat())
}

async fn up_all(r: &RRig) {
    let res = r.sup.up(UpParams::default(), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");
    r.rt.actions.lock().unwrap().clear();
}

fn apply_all() -> ConfigApplyParams {
    ConfigApplyParams {
        stems: vec![],
        yes: true,
    }
}

fn pid(r: &RRig, stem: &str) -> Option<i32> {
    r.sup
        .status(&StatusParams::default(), "t")
        .unwrap()
        .stems
        .into_iter()
        .find(|s| s.name == stem)
        .and_then(|s| s.pid)
}

fn count(r: &RRig, kind: &EventKind) -> usize {
    r.events
        .replay(0)
        .iter()
        .filter(|e| &e.kind == kind)
        .count()
}

#[tokio::test]
async fn apply_restarts_in_dependency_order_and_leaves_the_rest() {
    let r = rrig(&chain("1", "1", "1", "1"));
    up_all(&r).await;
    let (web, other) = (pid(&r, "web"), pid(&r, "other"));
    r.host.write(&chain("2", "2", "1", "1"));

    let diff = r.sup.config_diff("t").unwrap();
    assert!(diff.pending);
    let actions: Vec<(String, ReloadAction)> = diff
        .plan
        .affected()
        .map(|e| (e.name.clone(), e.action))
        .collect();
    assert_eq!(
        actions,
        [
            ("db".to_string(), ReloadAction::RestartRequired),
            ("api".to_string(), ReloadAction::RestartRequired)
        ]
    );
    assert!(
        r.rt.actions.lock().unwrap().is_empty(),
        "diff applies nothing"
    );

    let res = r.sup.config_apply(apply_all(), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");
    assert!(!res.pending);
    assert_eq!(
        *r.rt.actions.lock().unwrap(),
        ["stop api", "stop db", "start db", "start api"]
    );
    // Dependants whose config did not change keep running.
    assert_eq!(pid(&r, "web"), web);
    assert_eq!(pid(&r, "other"), other);
    // The new config is the applied one and nothing is pending any more.
    assert_eq!(r.host.applied().workspace.stem("db").unwrap().env["V"], "2");
    assert!(!r.sup.config_diff("t").unwrap().pending);
    assert_eq!(count(&r, &EventKind::CONFIG_APPLIED), 1);
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn apply_is_transactional_per_stem() {
    let r = rrig(&chain("1", "1", "1", "1"));
    up_all(&r).await;
    // `other` moves to a port that is taken: its restart fails, api's works.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let mut new = chain("1", "2", "1", "2");
    new = new.replacen(
        "  other: { type: process, command: run, ",
        &format!(
            "  other: {{ type: process, command: run, ports: [{{ name: http, port: {port} }}], "
        ),
        1,
    );
    r.host.write(&new);
    let res = r.sup.config_apply(apply_all(), "cli:t").await.unwrap();
    assert!(!res.ok);
    assert_eq!(res.failed.len(), 1, "{res:?}");
    assert_eq!(res.failed[0].stem, "other");
    assert_eq!(res.failed[0].error.code, ErrorCode::PortInUse);
    assert!(
        res.applied
            .iter()
            .any(|a| a.stem == "api" && a.result == "restarted"),
        "{res:?}"
    );
    until_state(&r.sup, "api", StemState::Healthy).await;
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn removed_stems_stop_and_added_ones_start() {
    let r = rrig(&chain("1", "1", "1", "1"));
    up_all(&r).await;
    let mut new = doc(&[
        stem("db", "", "1"),
        stem("api", "db", "1"),
        stem("web", "api", "1"),
    ]
    .concat());
    new.push_str(&stem("extra", "db", "1"));
    r.host.write(&new);
    let diff = r.sup.config_diff("t").unwrap();
    let find = |n: &str| diff.plan.stems.iter().find(|e| e.name == n).unwrap().action;
    assert_eq!(find("other"), ReloadAction::Removed);
    assert_eq!(find("extra"), ReloadAction::Added);

    let res = r.sup.config_apply(apply_all(), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");
    assert_eq!(*r.rt.actions.lock().unwrap(), ["stop other", "start extra"]);
    let st = until_state(&r.sup, "extra", StemState::Healthy).await;
    assert!(st.stems.iter().all(|s| s.name != "other"));
    assert!(
        !r.sup.cells.lock().unwrap().contains_key("other"),
        "the removed stem's cell is gone"
    );
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn an_added_stem_outside_the_last_up_is_only_registered() {
    let r = rrig(&chain("1", "1", "1", "1"));
    let res = r
        .sup
        .up(
            UpParams {
                stems: vec!["other".into()],
                ..UpParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert!(res.ok);
    r.rt.actions.lock().unwrap().clear();
    r.host.write(&format!(
        "{}{}",
        chain("1", "1", "1", "1"),
        stem("extra", "", "1")
    ));
    let res = r.sup.config_apply(apply_all(), "cli:t").await.unwrap();
    assert!(res.ok);
    assert!(r.rt.actions.lock().unwrap().is_empty());
    assert_eq!(res.applied[0].result, "registered");
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn a_hot_watch_change_restarts_nothing() {
    let with = |debounce: &str| {
        doc(&format!(
            "  w:\n    type: process\n    command: run\n    watch:\n      - {{ paths: ['*.py'], action: restart, debounce: {debounce} }}\n"
        ))
    };
    let r = rrig(&with("200ms"));
    up_all(&r).await;
    let before = pid(&r, "w");
    r.host.write(&with("700ms"));
    let diff = r.sup.config_diff("t").unwrap();
    let w = diff.plan.affected().next().unwrap().clone();
    assert_eq!(
        (w.action, w.hot, w.running),
        (ReloadAction::WatchChanged, true, true)
    );
    let res = r.sup.config_apply(apply_all(), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");
    assert_eq!(res.applied[0].result, "reconfigured");
    assert!(r.rt.actions.lock().unwrap().is_empty());
    assert_eq!(pid(&r, "w"), before);
    assert_eq!(count(&r, &EventKind::WATCH_RECONFIGURED), 1);
    let ws = r.sup.watch_status(Default::default(), "t").unwrap();
    assert_eq!(ws.stems[0].rules[0].debounce_ms, 700);
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn up_uses_the_applied_config_while_a_restart_is_pending() {
    let r = rrig(&chain("1", "1", "1", "1"));
    up_all(&r).await;
    // A change that restarts a running stem: `up` keeps the applied config.
    r.host.write(&chain("2", "1", "1", "1"));
    let res = r.sup.up(UpParams::default(), "cli:t").await.unwrap();
    assert!(res.ok);
    assert!(r.rt.actions.lock().unwrap().is_empty());
    assert_eq!(r.host.applied().workspace.stem("db").unwrap().env["V"], "1");
    assert_eq!(count(&r, &EventKind::CONFIG_PENDING), 1);
    assert_eq!(count(&r, &EventKind::CONFIG_CHANGED), 1);
    // A second `up` does not announce the same change again.
    r.sup.up(UpParams::default(), "cli:t").await.unwrap();
    assert_eq!(count(&r, &EventKind::CONFIG_CHANGED), 1);

    // Only stopped stems change: applied in place by the next command.
    r.sup
        .stop(
            StopParams {
                stems: vec!["db".into()],
                cascade: true,
                ..StopParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    r.sup
        .start(
            StartParams {
                stems: vec!["other".into()],
                ..StartParams::default()
            },
            "cli:t",
        )
        .await
        .unwrap();
    assert_eq!(r.host.applied().workspace.stem("db").unwrap().env["V"], "2");
    assert!(!r.sup.config_diff("t").unwrap().pending);
    r.sup.stop_all("t", "test").await;
}

#[tokio::test]
async fn policy_restarts_use_the_latest_applied_config() {
    let r = rrig(&doc(&stem("a", "", "1")));
    up_all(&r).await;
    // A hot change (restart policy) is applied: the running stem's restart
    // workspace is rebased onto the new config.
    r.host.write(&doc(
        &stem("a", "", "1").replace("env:", "restart: { max: 9 }, env:")
    ));
    let res = r.sup.config_apply(apply_all(), "cli:t").await.unwrap();
    assert!(res.ok, "{res:?}");
    let cell = r.sup.cell("a");
    let ws = cell.info().restart.workspace().cloned().unwrap();
    assert!(Arc::ptr_eq(&ws, &r.host.applied()));
    assert_eq!(ws.workspace.stem("a").unwrap().restart.max, 9);
    r.sup.stop_all("t", "test").await;
}
