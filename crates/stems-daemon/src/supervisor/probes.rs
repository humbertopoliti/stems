//! Health probes (deliverable 21, `docs/health.md`).
//!
//! * [`Prober`] — one check of one stem ([`TcpProber`], [`HttpProber`],
//!   [`CommandProber`], [`DockerProber`], [`ProcessProber`]; `grpc` is
//!   refused with `NOT_IMPLEMENTED`).
//! * [`Tracker`] — the pure transition rules: probe outcomes in, actions
//!   (`healthy`, `unhealthy`, `unknown`, `START_TIMEOUT`) out.
//! * [`monitor`] — the per-stem task: probes every `interval` (never
//!   overlapping: a tick that comes while a probe runs is skipped), keeps the
//!   last [`PROBE_HISTORY`] results in the cell's [`HealthLog`], and sends
//!   the tracker's actions to the stem's actor ([`Cmd::Health`]), which is
//!   the only writer of the state ([`on_health`]).
//! * [`apply_degraded`] — the derived `degraded` flag of `status`.
//!
//! Transition rules (managed stems):
//!
//! | state | probe | → |
//! |---|---|---|
//! | `starting` | ok | `healthy` (then `post_start`/`seed`, 16) |
//! | `starting` | past `start_timeout` | `failed`, `START_TIMEOUT`, process stopped |
//! | `healthy` | `retries` failures in a row (not counting `start_period`) | `unhealthy` (`stem.health`) |
//! | `unhealthy` | ok | `healthy` (`stem.health`) |
//!
//! External stems start `unknown`; ok → `healthy`; `retries` failures in a
//! row → `unhealthy`, or `unknown` when the last probe could not run
//! ([`ProbeOutcome::Unknown`]: name resolution, invalid URL, spawn failure).

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::json;
use stems_api::{EventKind, HealthStatus, ProbeOutcome, ProbeRecord, StemStatus};
use stems_config::{
    Health, HealthType, Resolved, Script, ScriptSource, Stem, StemRuntime, StemType,
};
use stems_core::health::{DegradedInputs, FLAP_WINDOW, degraded_text, derive_degraded};
use stems_core::{Error, ErrorCode, StemState};
use stems_runtime::{Handle, Runtime};
use tokio::time::MissedTickBehavior;

use super::Core;
use super::actor::{Cmd, StemCell};
use super::waiter::WaitTarget;
use crate::events::EventDraft;
use crate::scripts::{RunContext, ScriptRef};

pub use stems_api::health::PROBE_HISTORY;

/// Shortest probe interval (a `0s` interval would spin).
const MIN_INTERVAL: Duration = Duration::from_millis(50);
/// Longest `detail` kept per probe.
const MAX_DETAIL: usize = 200;

/// The result of one probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeResult {
    /// The check passed.
    pub ok: bool,
    /// Wall time of the probe.
    pub latency_ms: u64,
    /// What happened.
    pub detail: String,
    /// The check could not run at all (see [`ProbeOutcome::Unknown`]).
    pub unknown: bool,
}

impl ProbeResult {
    fn new(ok: bool, unknown: bool, t0: Instant, detail: impl Into<String>) -> Self {
        let mut detail: String = detail.into();
        if detail.chars().count() > MAX_DETAIL {
            detail = detail.chars().take(MAX_DETAIL - 1).collect::<String>() + "…";
        }
        Self {
            ok,
            latency_ms: t0.elapsed().as_millis() as u64,
            detail,
            unknown: unknown && !ok,
        }
    }
    /// A passing check.
    pub fn pass(t0: Instant, detail: impl Into<String>) -> Self {
        Self::new(true, false, t0, detail)
    }
    /// A failing check.
    pub fn fail(t0: Instant, detail: impl Into<String>) -> Self {
        Self::new(false, false, t0, detail)
    }
    /// A check that could not run.
    pub fn unknown(t0: Instant, detail: impl Into<String>) -> Self {
        Self::new(false, true, t0, detail)
    }
    /// Its outcome class.
    pub fn outcome(&self) -> ProbeOutcome {
        match (self.ok, self.unknown) {
            (true, _) => ProbeOutcome::Ok,
            (false, true) => ProbeOutcome::Unknown,
            (false, false) => ProbeOutcome::Fail,
        }
    }
    fn record(&self) -> ProbeRecord {
        ProbeRecord {
            ts: Utc::now(),
            ok: self.ok,
            outcome: self.outcome(),
            latency_ms: self.latency_ms,
            detail: self.detail.clone(),
        }
    }
}

/// One health check of one stem. Implementations bound themselves by the
/// check's `timeout`.
#[async_trait::async_trait]
pub trait Prober: Send + Sync {
    /// Run the check once.
    async fn probe(&self) -> ProbeResult;
}

// --------------------------------------------------------------------- probers

/// `tcp`: a TCP connect to `host:port` succeeds within `timeout`. The
/// resolved addresses are cached for [`DNS_TTL`] (re-resolved after a
/// failure), and the address that answered last is tried first, so a
/// `localhost` probe does not pay a name lookup and a refused `::1` connect
/// every time.
pub struct TcpProber {
    host: String,
    port: u16,
    timeout: Duration,
    cache: Mutex<Option<(Instant, Vec<std::net::SocketAddr>)>>,
}

/// How long [`TcpProber`] trusts a name lookup.
const DNS_TTL: Duration = Duration::from_secs(30);

impl TcpProber {
    /// A prober for `host:port`.
    pub fn new(host: impl Into<String>, port: u16, timeout: Duration) -> Self {
        Self {
            host: host.into(),
            port,
            timeout,
            cache: Mutex::new(None),
        }
    }

    fn cached(&self) -> Option<Vec<std::net::SocketAddr>> {
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.as_ref()
            .filter(|(at, _)| at.elapsed() < DNS_TTL)
            .map(|(_, a)| a.clone())
    }

    fn remember(&self, addrs: Option<Vec<std::net::SocketAddr>>) {
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        *c = addrs.map(|a| {
            let at = c.as_ref().map_or_else(Instant::now, |(t, _)| *t);
            (at, a)
        });
    }
}

#[async_trait::async_trait]
impl Prober for TcpProber {
    async fn probe(&self) -> ProbeResult {
        let t0 = Instant::now();
        let target = format!("{}:{}", self.host, self.port);
        let mut addrs = match self.cached() {
            Some(a) => a,
            None => {
                let looked =
                    tokio::time::timeout(self.timeout, tokio::net::lookup_host(&target)).await;
                match looked {
                    Ok(Ok(a)) => {
                        let a: Vec<_> = a.collect();
                        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some((Instant::now(), a.clone()));
                        a
                    }
                    Ok(Err(e)) => {
                        return ProbeResult::unknown(
                            t0,
                            format!("cannot resolve {}: {e}", self.host),
                        );
                    }
                    Err(_) => {
                        return ProbeResult::fail(t0, format!("resolving {} timed out", self.host));
                    }
                }
            }
        };
        if addrs.is_empty() {
            self.remember(None);
            return ProbeResult::unknown(t0, format!("{} has no address", self.host));
        }
        let mut last = String::new();
        for i in 0..addrs.len() {
            let a = addrs[i];
            let left = self.timeout.saturating_sub(t0.elapsed());
            match tokio::time::timeout(left, tokio::net::TcpStream::connect(a)).await {
                Ok(Ok(_)) => {
                    if i > 0 {
                        addrs[..=i].rotate_right(1);
                        self.remember(Some(addrs));
                    }
                    return ProbeResult::pass(t0, format!("connected to {target}"));
                }
                Ok(Err(e)) => last = format!("{target}: {}", io_text(&e)),
                Err(_) => {
                    self.remember(None);
                    return ProbeResult::fail(
                        t0,
                        format!(
                            "connect to {target} timed out after {}ms",
                            self.timeout.as_millis()
                        ),
                    );
                }
            }
        }
        self.remember(None);
        ProbeResult::fail(t0, last)
    }
}

fn io_text(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::ConnectionRefused => "connection refused".into(),
        _ => e.to_string(),
    }
}

/// `http`: a GET of `url` answers `status` (any 2xx when unset) and, with
/// `body_contains`, a body containing it.
pub struct HttpProber {
    client: reqwest::Client,
    url: String,
    status: Option<u16>,
    body_contains: Option<String>,
    headers: BTreeMap<String, String>,
    timeout: Duration,
}

impl HttpProber {
    /// A prober for `url` (no connection pooling: every probe is a fresh
    /// connection, like a client would make).
    pub fn new(url: &str, health: &Health) -> Result<Self, Error> {
        let timeout = health.timeout.as_duration();
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout)
            .pool_max_idle_per_host(0)
            .no_proxy()
            .tls_danger_accept_invalid_certs(health.insecure)
            .user_agent(concat!("stems/", env!("CARGO_PKG_VERSION"), " health"))
            .build()
            .map_err(|e| Error::internal(format!("cannot build the HTTP client: {e}")))?;
        Ok(Self {
            client,
            url: url.to_string(),
            status: health.status,
            body_contains: health.body_contains.clone(),
            headers: health.headers.clone(),
            timeout,
        })
    }
}

/// The error chain of `e`, innermost last.
fn chain(e: &(dyn std::error::Error + 'static)) -> Vec<String> {
    let mut out = vec![e.to_string()];
    let mut cur = e.source();
    while let Some(s) = cur {
        out.push(s.to_string());
        cur = s.source();
    }
    out
}

/// Could not run (name resolution, bad URL) vs failed (refused, timeout).
fn classify_http(e: &reqwest::Error, timeout: Duration) -> (bool, String) {
    if e.is_timeout() {
        return (false, format!("timed out after {}ms", timeout.as_millis()));
    }
    if e.is_builder() {
        return (true, format!("invalid request: {e}"));
    }
    let c = chain(e);
    let all = c.join(": ").to_ascii_lowercase();
    let innermost = c.last().cloned().unwrap_or_default();
    if all.contains("dns error")
        || all.contains("failed to lookup address")
        || all.contains("nodename nor servname")
        || all.contains("name or service not known")
    {
        return (true, format!("cannot resolve host: {innermost}"));
    }
    if all.contains("connection refused") {
        return (false, "connection refused".into());
    }
    (false, innermost)
}

#[async_trait::async_trait]
impl Prober for HttpProber {
    async fn probe(&self) -> ProbeResult {
        let t0 = Instant::now();
        let mut req = self.client.get(&self.url);
        for (k, v) in &self.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                let (unknown, detail) = classify_http(&e, self.timeout);
                return if unknown {
                    ProbeResult::unknown(t0, detail)
                } else {
                    ProbeResult::fail(t0, detail)
                };
            }
        };
        let code = resp.status().as_u16();
        let status_ok = match self.status {
            Some(want) => code == want,
            None => resp.status().is_success(),
        };
        if !status_ok {
            let want = self
                .status
                .map_or_else(|| "2xx".to_string(), |s| s.to_string());
            return ProbeResult::fail(t0, format!("HTTP {code} (want {want})"));
        }
        if let Some(needle) = &self.body_contains {
            return match resp.text().await {
                Ok(body) if body.contains(needle.as_str()) => {
                    ProbeResult::pass(t0, format!("HTTP {code}, body matches"))
                }
                Ok(_) => ProbeResult::fail(t0, format!("HTTP {code}, body lacks `{needle}`")),
                Err(e) => {
                    let (_, detail) = classify_http(&e, self.timeout);
                    ProbeResult::fail(t0, format!("HTTP {code}, reading body: {detail}"))
                }
            };
        }
        ProbeResult::pass(t0, format!("HTTP {code}"))
    }
}

/// `command`: a shell command (via the [`crate::scripts::ScriptRunner`],
/// tag `health`) exits 0 within `timeout`. Its output goes to the stem's
/// log (tag `health`) only when the probe fails or its outcome changes, so a
/// passing probe every 2 s does not flood the log.
pub struct CommandProber {
    core: Arc<Core>,
    stem: String,
    ws_root: std::path::PathBuf,
    script: Script,
    env: BTreeMap<String, String>,
    shell: Option<String>,
    last_ok: Mutex<Option<bool>>,
}

#[async_trait::async_trait]
impl Prober for CommandProber {
    async fn probe(&self) -> ProbeResult {
        let t0 = Instant::now();
        let r = self
            .core
            .scripts
            .run(
                &self.ws_root,
                Some(&self.stem),
                ScriptRef {
                    name: "health",
                    script: &self.script,
                },
                RunContext {
                    env: self.env.clone(),
                    shell: self.shell.clone(),
                    actor: stems_api::DAEMON_ACTOR.into(),
                    quiet: true,
                    ..RunContext::default()
                },
            )
            .await;
        let result = match &r {
            Err(e) => ProbeResult::unknown(t0, e.message.clone()),
            Ok(r) if r.success() => ProbeResult::pass(t0, "exit 0"),
            Ok(r) if r.timed_out => ProbeResult::fail(
                t0,
                format!(
                    "timed out after {}ms",
                    r.timeout.unwrap_or_default().as_millis()
                ),
            ),
            Ok(r) => {
                let how = match (r.exit.code, r.exit.signal) {
                    (Some(c), _) => format!("exit {c}"),
                    (None, Some(s)) => format!("killed by signal {s}"),
                    _ => "failed".into(),
                };
                match r.tail.last() {
                    Some(l) => ProbeResult::fail(t0, format!("{how}: {l}")),
                    None => ProbeResult::fail(t0, how),
                }
            }
        };
        let changed = {
            let mut last = self.last_ok.lock().unwrap_or_else(|e| e.into_inner());
            let changed = *last != Some(result.ok);
            *last = Some(result.ok);
            changed
        };
        if (changed || !result.ok)
            && let Some(w) = self.core.sink.script_writer(&self.stem, "health")
        {
            if let Ok(r) = &r {
                for l in &r.tail {
                    w.line(l.clone()).await;
                }
            }
            let verdict = if result.ok { "passed" } else { "failed" };
            w.line(format!(
                "[stems] health check {verdict} ({}; {}ms)",
                result.detail, result.latency_ms
            ))
            .await;
        }
        result
    }
}

/// `docker`: the container's own healthcheck (`State.Health.Status` via
/// `describe`): `healthy` passes, `starting`/`unhealthy` fail. A container
/// without a healthcheck passes while it runs.
pub struct DockerProber {
    /// The runtime.
    pub runtime: Arc<dyn Runtime>,
    /// The container.
    pub handle: Handle,
}

#[async_trait::async_trait]
impl Prober for DockerProber {
    async fn probe(&self) -> ProbeResult {
        let t0 = Instant::now();
        match self.runtime.describe(&self.handle).await {
            Ok(f) => match f.container_health.as_deref() {
                Some("healthy") => ProbeResult::pass(t0, "container healthy"),
                Some(other) => ProbeResult::fail(t0, format!("container {other}")),
                None if self.runtime.is_alive(&self.handle).await => {
                    ProbeResult::pass(t0, "running (no container healthcheck)")
                }
                None => ProbeResult::fail(t0, "not running"),
            },
            Err(e) => ProbeResult::fail(t0, format!("cannot inspect: {e}")),
        }
    }
}

/// `process`: the process (group leader) or container is alive.
pub struct ProcessProber {
    /// The runtime.
    pub runtime: Arc<dyn Runtime>,
    /// The unit.
    pub handle: Handle,
}

#[async_trait::async_trait]
impl Prober for ProcessProber {
    async fn probe(&self) -> ProbeResult {
        let t0 = Instant::now();
        if self.runtime.is_alive(&self.handle).await {
            ProbeResult::pass(t0, "alive")
        } else {
            ProbeResult::fail(t0, "not alive")
        }
    }
}

/// `grpc` needs the `grpc` build (tonic health/v1 `Check`), not built yet.
pub fn grpc_unavailable(stem: &str) -> Error {
    Error::new(
        ErrorCode::NotImplemented,
        format!("`{stem}` uses a `grpc` health check, which this build of stems cannot run"),
    )
    .with_hint(
        "use `type: tcp` on the gRPC port, or a `command` probe running `grpc_health_probe -addr=...`",
    )
    .with_details(json!({ "stem": stem, "health_type": "grpc", "feature": "grpc" }))
}

fn no_port(stem: &str, kind: &str) -> Error {
    Error::new(
        ErrorCode::StartFailed,
        format!("`{stem}` has a `{kind}` health check but no port to probe"),
    )
    .with_hint(format!(
        "set `stems.{stem}.health.port` (or `url`), or declare a port on the stem"
    ))
    .with_details(json!({ "stem": stem, "health_type": kind }))
}

/// The shell command of a `command` probe. For a container stem it runs
/// inside the container (`docker exec <id> sh -c '<command>'`), where the
/// service's tools live (`pg_isready`, `redis-cli`).
fn probe_command(stem: &Stem, target: &WaitTarget, h: &Health) -> String {
    let command = h.command.clone().unwrap_or_default();
    match target.handle.container_id() {
        Some(id) if matches!(stem.kind(), StemType::Docker | StemType::Compose) => {
            format!(
                "exec docker exec {id} sh -c '{}'",
                command.replace('\'', "'\\''")
            )
        }
        _ => command,
    }
}

fn kind_name(k: HealthType) -> String {
    serde_json::to_value(k)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The prober for a started stem (`target` carries its rendered health
/// config and handle). Command probes run in the stem's codebase (or
/// `health.cwd` below it) with the stem's environment.
pub(crate) fn build(
    core: &Arc<Core>,
    ws: &Resolved,
    stem: &Stem,
    target: &WaitTarget,
    own_env: &BTreeMap<String, String>,
) -> Result<Option<Arc<dyn Prober>>, Error> {
    let Some(h) = &target.health else {
        return Ok(None);
    };
    let timeout = h.timeout.as_duration();
    let p: Arc<dyn Prober> = match h.kind {
        HealthType::Tcp => Arc::new(TcpProber::new(
            h.host.clone(),
            target
                .probe_port
                .ok_or_else(|| no_port(&stem.name, "tcp"))?,
            timeout,
        )),
        HealthType::Http => {
            let url = match (&h.url, target.probe_port) {
                (Some(u), _) => u.clone(),
                (None, Some(p)) => format!("http://{}:{p}/", h.host),
                (None, None) => return Err(no_port(&stem.name, "http")),
            };
            Arc::new(HttpProber::new(&url, h)?)
        }
        HealthType::Command => {
            let root = &ws.workspace.root;
            let base = stem.default_cwd(root).to_path_buf();
            let cwd = h.cwd.as_ref().map_or(base.clone(), |c| base.join(c));
            let mut env = core.base_env.clone();
            env.extend(own_env.iter().map(|(k, v)| (k.clone(), v.clone())));
            let shell = match &stem.runtime {
                StemRuntime::Process(p) => Some(p.shell.clone()),
                _ => None,
            };
            Arc::new(CommandProber {
                core: core.clone(),
                stem: stem.name.clone(),
                ws_root: root.clone(),
                script: Script {
                    source: ScriptSource::Command(probe_command(stem, target, h)),
                    description: None,
                    args: Vec::new(),
                    inputs: Vec::new(),
                    requires: Vec::new(),
                    timeout: Some(h.timeout),
                    retries: 0,
                    concurrent: true,
                    stamp_env: Vec::new(),
                    cwd,
                    cwd_set: true,
                },
                env,
                shell,
                last_ok: Mutex::new(None),
            })
        }
        HealthType::Docker => Arc::new(DockerProber {
            runtime: target.runtime.clone(),
            handle: target.handle.clone(),
        }),
        HealthType::Process => Arc::new(ProcessProber {
            runtime: target.runtime.clone(),
            handle: target.handle.clone(),
        }),
        HealthType::Grpc => return Err(grpc_unavailable(&stem.name)),
    };
    Ok(Some(p))
}

// --------------------------------------------------------------------- tracker

/// What the tracker asks the actor to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// → `healthy` (from `starting`: the start sequence continues).
    Healthy,
    /// → `unhealthy`.
    Unhealthy,
    /// → `unknown` (external stems only).
    Unknown,
    /// `starting` for longer than `start_timeout`: fail with `START_TIMEOUT`.
    StartTimeout,
}

/// The tracker's parameters (from `health`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackerConfig {
    /// Consecutive failures before `unhealthy` (0 counts as 1).
    pub retries: u32,
    /// Failures in this period after the start do not count.
    pub start_period: Duration,
    /// `starting` longer than this fails the start.
    pub start_timeout: Duration,
    /// External stem: `unknown ⇄ healthy/unhealthy`, never `starting`.
    pub external: bool,
}

impl TrackerConfig {
    /// From a stem's health config.
    pub fn of(h: &Health, external: bool) -> Self {
        Self {
            retries: h.retries,
            start_period: h.start_period.as_duration(),
            start_timeout: h.start_timeout.as_duration(),
            external,
        }
    }
}

/// The transition rules, fed one probe outcome at a time (pure; the
/// current state comes from the actor).
#[derive(Clone, Debug)]
pub struct Tracker {
    cfg: TrackerConfig,
    started: Instant,
    failures: u32,
}

impl Tracker {
    /// Rules for a unit started at `started`.
    pub fn new(cfg: TrackerConfig, started: Instant) -> Self {
        Self {
            cfg,
            started,
            failures: 0,
        }
    }

    /// When `starting` gives up.
    pub fn start_deadline(&self) -> Instant {
        self.started + self.cfg.start_timeout
    }

    /// Failures in a row that counted.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// Feed one outcome, seen while the stem is `current`.
    pub fn on_result(
        &mut self,
        current: StemState,
        outcome: ProbeOutcome,
        now: Instant,
    ) -> Option<Action> {
        use StemState::*;
        let ok = outcome == ProbeOutcome::Ok;
        let in_grace = now < self.started + self.cfg.start_period;
        if ok {
            self.failures = 0;
        } else if !in_grace {
            self.failures += 1;
        }
        let retries = self.cfg.retries.max(1);
        if self.cfg.external {
            if !matches!(current, Unknown | Healthy | Unhealthy) {
                return None;
            }
            if ok {
                return (current != Healthy).then_some(Action::Healthy);
            }
            if self.failures >= retries {
                let (to, action) = if outcome == ProbeOutcome::Unknown {
                    (Unknown, Action::Unknown)
                } else {
                    (Unhealthy, Action::Unhealthy)
                };
                return (current != to).then_some(action);
            }
            return None;
        }
        match current {
            Starting if ok => Some(Action::Healthy),
            Starting if now >= self.start_deadline() => Some(Action::StartTimeout),
            Healthy if !ok && self.failures >= retries => Some(Action::Unhealthy),
            Unhealthy if ok => Some(Action::Healthy),
            _ => None,
        }
    }
}

// ------------------------------------------------------------------ health log

/// A stem's probe history (in its cell): the last [`PROBE_HISTORY`] results,
/// the failure streak and recent health transitions (flapping).
#[derive(Clone, Debug, Default)]
pub struct HealthLog {
    kind: Option<HealthType>,
    ring: VecDeque<ProbeRecord>,
    consecutive_failures: u32,
    transitions: VecDeque<Instant>,
}

impl HealthLog {
    /// A new start with probe type `kind` (history is kept).
    pub fn begin(&mut self, kind: Option<HealthType>) {
        self.kind = kind;
        self.consecutive_failures = 0;
    }

    /// Record one result.
    pub fn push(&mut self, r: ProbeRecord) {
        if r.ok {
            self.consecutive_failures = 0;
        } else {
            self.consecutive_failures += 1;
        }
        if self.ring.len() == PROBE_HISTORY {
            self.ring.pop_front();
        }
        self.ring.push_back(r);
    }

    /// Record a health transition at `now`.
    pub fn transition(&mut self, now: Instant) {
        self.transitions.push_back(now);
        self.prune(now);
    }

    fn prune(&mut self, now: Instant) {
        while self
            .transitions
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= FLAP_WINDOW)
        {
            self.transitions.pop_front();
        }
    }

    /// Transitions in the last 60 s.
    pub fn transitions_in_window(&self, now: Instant) -> usize {
        self.transitions
            .iter()
            .filter(|t| now.saturating_duration_since(**t) < FLAP_WINDOW)
            .count()
    }

    /// The last `n` results, oldest first.
    pub fn last(&self, n: usize) -> Vec<ProbeRecord> {
        let skip = self.ring.len().saturating_sub(n);
        self.ring.iter().skip(skip).cloned().collect()
    }

    /// Probe type name.
    pub fn kind(&self) -> Option<String> {
        self.kind.map(kind_name)
    }

    /// Failed probes in a row.
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// The `health` block of `status` (`None` without a health check).
    pub fn status(&self, now: Instant) -> Option<HealthStatus> {
        Some(HealthStatus {
            kind: self.kind()?,
            last: self.ring.back().cloned(),
            consecutive_failures: self.consecutive_failures,
            transitions_60s: self.transitions_in_window(now) as u32,
            container: None,
        })
    }
}

// --------------------------------------------------------------------- monitor

/// Start probing a just-spawned managed stem (generation `generation`):
/// the task drives `starting → healthy` (or `START_TIMEOUT`) and then
/// `healthy ⇄ unhealthy` until the stem stops. A prober that cannot be
/// built (grpc, no port) fails the start.
pub(crate) fn start(
    core: &Arc<Core>,
    cell: &Arc<StemCell>,
    ws: &Resolved,
    stem: &Stem,
    target: &WaitTarget,
    generation: u64,
) {
    let own_env = cell.info().env.clone();
    let kind = target.health.as_ref().map(|h| h.kind);
    cell.info().health.begin(kind);
    let built = match core.waiter.prober(target) {
        Some(p) => Ok(Some(p)),
        None => build(core, ws, stem, target, &own_env),
    };
    let prober = match built {
        Ok(Some(p)) => p,
        Ok(None) => {
            // No health check: ready as soon as it runs.
            cell.send(Cmd::Health {
                generation,
                action: Action::Healthy,
                result: ProbeResult::pass(Instant::now(), "no health check"),
            });
            return;
        }
        Err(e) => {
            cell.send(Cmd::Ready {
                generation,
                result: Err(e),
            });
            return;
        }
    };
    let Some(h) = target.health.clone() else {
        return;
    };
    let task = tokio::spawn(monitor(
        cell.clone(),
        generation,
        prober,
        h.clone(),
        TrackerConfig::of(&h, false),
    ));
    cell.set_ready_task(task.abort_handle());
}

/// Keep probing an adopted stem (crash recovery, `adopt_orphans`): it is
/// `healthy` already, so the tracker only watches for `unhealthy`.
pub(crate) fn monitor_adopted(core: &Arc<Core>, cell: &Arc<StemCell>, ws: &Resolved) {
    if !core.waiter.probes() {
        return;
    }
    let Some(stem) = ws.workspace.stem(&cell.name) else {
        return;
    };
    let (runtime, handle, generation) = {
        let info = cell.info();
        match (&info.runtime, &info.handle) {
            (Some(r), Some(h)) => (r.clone(), h.clone(), info.generation),
            _ => return,
        }
    };
    let health = stem.health.clone().map(|mut h| {
        render_health(core, ws, stem, &mut h);
        h
    });
    let probe_port = health.as_ref().and_then(|h| probe_port(h, stem));
    let target = WaitTarget {
        stem: stem.name.clone(),
        runtime,
        handle,
        health: health.clone(),
        probe_port,
    };
    cell.info().health.begin(health.as_ref().map(|h| h.kind));
    let own_env = cell.info().env.clone();
    if let (Ok(Some(prober)), Some(h)) = (build(core, ws, stem, &target, &own_env), health) {
        let task = tokio::spawn(monitor(
            cell.clone(),
            generation,
            prober,
            h.clone(),
            TrackerConfig::of(&h, false),
        ));
        cell.set_ready_task(task.abort_handle());
    }
}

/// Start monitoring an external stem (state `unknown`); `false` when it has
/// no health check (it then stays `unknown`).
pub(crate) fn start_external(
    core: &Arc<Core>,
    cell: &Arc<StemCell>,
    ws: &Resolved,
    stem: &Stem,
    runtime: Arc<dyn Runtime>,
    handle: Handle,
) -> bool {
    if !core.waiter.probes() {
        return false;
    }
    let Some(mut h) = stem.health.clone() else {
        return false;
    };
    render_health(core, ws, stem, &mut h);
    let target = WaitTarget {
        stem: stem.name.clone(),
        runtime,
        handle,
        probe_port: probe_port(&h, stem),
        health: Some(h.clone()),
    };
    let generation = cell.bump_generation();
    cell.info().health.begin(Some(h.kind));
    match build(core, ws, stem, &target, &BTreeMap::new()) {
        Ok(Some(prober)) => {
            let task = tokio::spawn(monitor(
                cell.clone(),
                generation,
                prober,
                h.clone(),
                TrackerConfig::of(&h, true),
            ));
            cell.set_ready_task(task.abort_handle());
            true
        }
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(stem = %stem.name, error = %e.message, "external stem cannot be probed");
            cell.info()
                .health
                .push(ProbeResult::unknown(Instant::now(), e.message.clone()).record());
            false
        }
    }
}

/// Stop monitoring an external stem (`down`): back to `stopped`.
pub(crate) fn unmonitor(core: &Core, cell: &StemCell, actor: &str, reason: &str) {
    cell.bump_generation();
    if cell.state() != StemState::Stopped {
        cell.transition(core, StemState::Stopped, reason, actor, json!({}));
    }
}

/// Render deferred `${stem.x.port}` references in `url`/`port`.
fn render_health(core: &Core, ws: &Resolved, stem: &Stem, h: &mut Health) {
    let workspace = &ws.workspace;
    let mut allocated = Vec::new();
    let mut lookup =
        |n: &str, p: Option<&str>| core.ports.resolve_ref(workspace, n, p, &mut allocated);
    h.url = h
        .url
        .take()
        .map(|u| super::env::render_refs(&u, &stem.name, &mut lookup));
    if let Some(stems_config::HealthPort::Deferred(s)) = &h.port {
        let r = super::env::render_refs(s, &stem.name, &mut lookup);
        h.port = Some(match r.trim().parse() {
            Ok(n) => stems_config::HealthPort::Number(n),
            Err(_) => stems_config::HealthPort::Deferred(r),
        });
    }
}

fn port_of_url(url: &str) -> Option<u16> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next()?;
    let (_, port) = authority.rsplit_once(':')?;
    port.parse().ok()
}

fn probe_port(h: &Health, stem: &Stem) -> Option<u16> {
    match &h.port {
        Some(stems_config::HealthPort::Number(n)) => Some(*n),
        _ => h.url.as_deref().and_then(port_of_url),
    }
    .or_else(|| {
        stem.ports.first().and_then(|p| match p.port {
            stems_config::PortRef::Fixed(n) => Some(n),
            stems_config::PortRef::Auto => None,
        })
    })
}

/// The probe loop of one start (generation). Ends when the generation
/// changes (stop, exit, restart) or the task is aborted.
async fn monitor(
    cell: Arc<StemCell>,
    generation: u64,
    prober: Arc<dyn Prober>,
    health: Health,
    cfg: TrackerConfig,
) {
    let mut tracker = Tracker::new(cfg, Instant::now());
    let deadline = tokio::time::Instant::from_std(tracker.start_deadline());
    let mut tick = tokio::time::interval(health.interval.as_duration().max(MIN_INTERVAL));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let current = |cell: &StemCell| {
        let info = cell.info();
        (info.generation == generation).then_some(info.state)
    };
    loop {
        let starting = current(&cell) == Some(StemState::Starting) && !cfg.external;
        tokio::select! {
            _ = tick.tick() => {}
            () = tokio::time::sleep_until(deadline), if starting => {
                let last = cell.info().health.last(1).pop();
                let r = ProbeResult {
                    ok: false,
                    latency_ms: last.as_ref().map_or(0, |r| r.latency_ms),
                    detail: last.map_or_else(|| "no probe finished".into(), |r| r.detail),
                    unknown: false,
                };
                act(&cell, generation, &cfg, Action::StartTimeout, r);
                // Wait for the actor to act on it (it stops this task).
                tick.tick().await;
                continue;
            }
        }
        if current(&cell).is_none() {
            return;
        }
        // Sequential: a probe never overlaps the previous one; ticks missed
        // meanwhile are skipped.
        let r = prober.probe().await;
        let Some(state) = current(&cell) else {
            return;
        };
        cell.info().health.push(r.record());
        if let Some(action) = tracker.on_result(state, r.outcome(), Instant::now()) {
            act(&cell, generation, &cfg, action, r);
        }
    }
}

/// Hand an action to the actor: `START_TIMEOUT` fails the start like a
/// failed readiness wait; the others are health transitions.
fn act(cell: &StemCell, generation: u64, cfg: &TrackerConfig, action: Action, r: ProbeResult) {
    if action == Action::StartTimeout {
        let e = start_timeout_error(&cell.name, cfg.start_timeout, &r);
        cell.send(Cmd::Ready {
            generation,
            result: Err(e),
        });
    } else {
        cell.send(Cmd::Health {
            generation,
            action,
            result: r,
        });
    }
}

/// `START_TIMEOUT` of a stem that stayed `starting` too long.
fn start_timeout_error(stem: &str, timeout: Duration, last: &ProbeResult) -> Error {
    Error::new(
        ErrorCode::StartTimeout,
        format!(
            "`{stem}` did not become healthy within its start_timeout of {}s (last probe: {})",
            timeout.as_secs_f64(),
            last.detail
        ),
    )
    .with_hint(format!(
        "check `stems logs {stem}` and `stems health {stem}`; raise `health.start_timeout` if it is just slow to boot"
    ))
    .with_details(json!({
        "stem": stem,
        "start_timeout_ms": timeout.as_millis() as u64,
        "last_probe": last.detail,
    }))
}

/// Apply a tracker action (actor side; `generation` already checked
/// against stale results here too).
pub(crate) async fn on_health(
    core: &Arc<Core>,
    cell: &Arc<StemCell>,
    generation: u64,
    action: Action,
    r: ProbeResult,
) {
    let state = {
        let info = cell.info();
        if info.generation != generation {
            return;
        }
        info.state
    };
    use StemState::{Healthy, Starting, Unhealthy, Unknown};
    match (action, state) {
        (Action::Healthy, Starting) => {
            super::actor::on_ready(core, cell, generation, Ok(())).await;
        }
        (Action::Healthy, Unhealthy | Unknown) => {
            health_transition(core, cell, state, Healthy, "health check passed".into(), &r);
        }
        (Action::Unhealthy, Healthy | Unknown) => {
            health_transition(core, cell, state, Unhealthy, r.detail.clone(), &r);
        }
        (Action::Unknown, Healthy | Unhealthy) => {
            let reason = format!("probe cannot run: {}", r.detail);
            health_transition(core, cell, state, Unknown, reason, &r);
        }
        _ => {}
    }
}

fn health_transition(
    core: &Core,
    cell: &StemCell,
    from: StemState,
    to: StemState,
    reason: String,
    r: &ProbeResult,
) {
    let failures = cell.info().health.consecutive_failures();
    let data = json!({
        "detail": r.detail,
        "latency_ms": r.latency_ms,
        "outcome": r.outcome(),
        "consecutive_failures": failures,
    });
    if !cell.transition(
        core,
        to,
        reason.clone(),
        stems_api::DAEMON_ACTOR,
        data.clone(),
    ) {
        return;
    }
    {
        let mut info = cell.info();
        info.health.transition(Instant::now());
        if to == StemState::Healthy {
            // A plain healthy stem shows no reason.
            info.reason = None;
        }
    }
    let kind = cell.info().health.kind();
    core.events.emit(
        EventDraft::new(EventKind::STEM_HEALTH, stems_api::DAEMON_ACTOR)
            .stem(&cell.name)
            .transition(from.as_str(), to.as_str())
            .reason(reason)
            .data(json!({
                "probe": kind,
                "detail": r.detail,
                "latency_ms": r.latency_ms,
                "outcome": r.outcome(),
                "consecutive_failures": failures,
            })),
    );
}

// -------------------------------------------------------------------- degraded

/// Derive `degraded` for every healthy stem of `stems` (FR-GR-6):
/// `state_of` gives any stem's current state (dependencies may be outside
/// the `status` filter).
pub(crate) fn apply_degraded(
    ws: &Resolved,
    stems: &mut [StemStatus],
    state_of: impl Fn(&str) -> StemState,
    metrics_of: impl Fn(&str) -> Vec<String>,
) {
    for s in stems.iter_mut() {
        let Some(stem) = ws.workspace.stem(&s.name) else {
            continue;
        };
        let inputs = DegradedInputs {
            state: Some(s.state),
            hard_deps: stem
                .depends_on
                .iter()
                .filter(|d| !d.soft)
                .map(|d| (d.stem.clone(), state_of(&d.stem)))
                .collect(),
            transitions_in_window: s.health.as_ref().map_or(0, |h| h.transitions_60s as usize),
            // FR-HS-4: >= 3 policy restarts in `restart.window` (22).
            restarts_degraded: (s.restarts_in_window as usize
                >= stems_core::restart::DEGRADED_RESTARTS)
                .then_some(s.restarts_in_window),
            // Crossed `limits:` thresholds (25), e.g. `memory > 100MB`.
            metrics: metrics_of(&s.name),
        };
        let reasons = derive_degraded(&inputs);
        if let Some(text) = degraded_text(&reasons) {
            s.degraded = true;
            s.glyph = s.state.glyph(true);
            s.reason = Some(text);
        }
    }
}

/// Is `stem` an external stem with a health check (the scheduler then
/// waits for `healthy` when a dependant needs it)?
pub(crate) fn external_with_health(ws: &Resolved, stem: &str) -> Option<Duration> {
    let s = ws.workspace.stem(stem)?;
    if s.kind() != StemType::External {
        return None;
    }
    s.health.as_ref().map(|h| h.start_timeout.as_duration())
}

impl super::Supervisor {
    /// `health {stems?, last?}`: the last `last` (default 10, at most 50)
    /// probe results of each stem, oldest first.
    pub fn health(
        &self,
        p: &stems_api::HealthParams,
        actor: &str,
    ) -> Result<stems_api::HealthResult, Error> {
        let ws = self.workspace(false, actor)?;
        if !p.stems.is_empty() {
            super::schedule::plan(&ws, &p.stems, true)?;
        }
        let n = p
            .last
            .unwrap_or(stems_api::health::DEFAULT_LAST)
            .min(PROBE_HISTORY);
        let now = Instant::now();
        let stems = ws
            .workspace
            .stems()
            .filter(|s| p.stems.is_empty() || p.stems.contains(&s.name))
            .map(|s| {
                let cell = self.cell(&s.name);
                let info = cell.info();
                stems_api::StemHealth {
                    name: s.name.clone(),
                    kind: info
                        .health
                        .kind()
                        .or_else(|| s.health.as_ref().map(|h| kind_name(h.kind))),
                    state: info.state,
                    consecutive_failures: info.health.consecutive_failures(),
                    transitions_60s: info.health.transitions_in_window(now) as u32,
                    results: info.health.last(n),
                }
            })
            .collect();
        Ok(stems_api::HealthResult { stems })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ProbeOutcome::{Fail, Ok as Pass, Unknown as Cant};
    use StemState::*;

    fn cfg(retries: u32, start_period_ms: u64, external: bool) -> TrackerConfig {
        TrackerConfig {
            retries,
            start_period: Duration::from_millis(start_period_ms),
            start_timeout: Duration::from_secs(5),
            external,
        }
    }

    /// Feed a scripted sequence; the "actor" applies every action, and the
    /// states after each probe are returned.
    fn run(
        c: TrackerConfig,
        start: StemState,
        seq: &[(u64, ProbeOutcome)],
    ) -> Vec<(StemState, Option<Action>)> {
        let t0 = Instant::now();
        let mut t = Tracker::new(c, t0);
        let mut state = start;
        let mut out = Vec::new();
        for (ms, o) in seq {
            let a = t.on_result(state, *o, t0 + Duration::from_millis(*ms));
            state = match a {
                Some(Action::Healthy) => Healthy,
                Some(Action::Unhealthy) => Unhealthy,
                Some(Action::Unknown) => Unknown,
                Some(Action::StartTimeout) => Failed,
                None => state,
            };
            out.push((state, a));
        }
        out
    }

    fn states(v: &[(StemState, Option<Action>)]) -> Vec<StemState> {
        v.iter().map(|(s, _)| *s).collect()
    }

    #[test]
    fn starting_becomes_healthy_on_first_success() {
        let r = run(
            cfg(3, 0, false),
            Starting,
            &[(0, Fail), (200, Fail), (400, Pass)],
        );
        assert_eq!(states(&r), [Starting, Starting, Healthy]);
        assert_eq!(r[2].1, Some(Action::Healthy));
    }

    #[test]
    fn starting_past_start_timeout_fails() {
        let r = run(cfg(3, 0, false), Starting, &[(4_000, Fail), (5_000, Fail)]);
        assert_eq!(states(&r), [Starting, Failed]);
        assert_eq!(r[1].1, Some(Action::StartTimeout));
        // A late success still wins before the deadline check.
        let r = run(cfg(3, 0, false), Starting, &[(6_000, Pass)]);
        assert_eq!(states(&r), [Healthy]);
    }

    #[test]
    fn healthy_needs_retries_consecutive_failures() {
        let seq = [
            (0, Pass),
            (200, Fail),
            (400, Pass),
            (600, Fail),
            (800, Fail),
            (1000, Fail),
            (1200, Fail),
            (1400, Pass),
        ];
        let r = run(cfg(3, 0, false), Starting, &seq);
        assert_eq!(
            states(&r),
            [
                Healthy, Healthy, Healthy, Healthy, Healthy, Unhealthy, Unhealthy, Healthy
            ]
        );
        // Exactly one action per transition, none per probe.
        let actions: Vec<Action> = r.iter().filter_map(|(_, a)| *a).collect();
        assert_eq!(
            actions,
            [Action::Healthy, Action::Unhealthy, Action::Healthy]
        );
    }

    #[test]
    fn start_period_failures_do_not_count() {
        // retries 2, start_period 1s: the failures at 300/600 ms are grace.
        let seq = [
            (100, Pass),
            (300, Fail),
            (600, Fail),
            (1200, Fail),
            (1400, Fail),
        ];
        let r = run(cfg(2, 1000, false), Starting, &seq);
        assert_eq!(states(&r), [Healthy, Healthy, Healthy, Healthy, Unhealthy]);
    }

    #[test]
    fn retries_zero_counts_as_one() {
        let r = run(cfg(0, 0, false), Healthy, &[(0, Fail)]);
        assert_eq!(states(&r), [Unhealthy]);
    }

    #[test]
    fn transient_states_are_left_alone() {
        for s in [Seeding, Setup, Stopping, Stopped, Failed] {
            let r = run(cfg(1, 0, false), s, &[(0, Fail), (10, Pass)]);
            assert_eq!(states(&r), [s, s], "{s}");
        }
    }

    #[test]
    fn external_unknown_vs_unhealthy() {
        // Reachable: unknown -> healthy; refused x2 -> unhealthy; back.
        let r = run(
            cfg(2, 0, true),
            Unknown,
            &[(0, Pass), (200, Fail), (400, Fail), (600, Pass)],
        );
        assert_eq!(states(&r), [Healthy, Healthy, Unhealthy, Healthy]);
        // Unresolvable: stays unknown (no action at all).
        let r = run(
            cfg(2, 0, true),
            Unknown,
            &[(0, Cant), (200, Cant), (400, Cant)],
        );
        assert_eq!(states(&r), [Unknown, Unknown, Unknown]);
        assert!(r.iter().all(|(_, a)| a.is_none()));
        // Healthy, then the probe cannot run: unknown after retries.
        let r = run(
            cfg(2, 0, true),
            Unknown,
            &[(0, Pass), (200, Cant), (400, Cant)],
        );
        assert_eq!(states(&r), [Healthy, Healthy, Unknown]);
        // Refused while unknown: unhealthy (it answered, negatively).
        let r = run(cfg(1, 0, true), Unknown, &[(0, Fail)]);
        assert_eq!(states(&r), [Unhealthy]);
        // An external stem never times out.
        let r = run(cfg(1, 0, true), Unknown, &[(60_000, Cant)]);
        assert_eq!(states(&r), [Unknown]);
    }

    #[test]
    fn managed_stems_count_unknown_as_failure() {
        let r = run(cfg(2, 0, false), Healthy, &[(0, Cant), (200, Cant)]);
        assert_eq!(states(&r), [Healthy, Unhealthy]);
    }

    #[test]
    fn health_log_ring_and_flapping_window() {
        let mut log = HealthLog::default();
        log.begin(Some(HealthType::Http));
        for i in 0..(PROBE_HISTORY + 5) {
            let t0 = Instant::now();
            let r = if i % 2 == 0 {
                ProbeResult::pass(t0, format!("p{i}"))
            } else {
                ProbeResult::fail(t0, format!("p{i}"))
            };
            log.push(r.record());
        }
        assert_eq!(log.last(1000).len(), PROBE_HISTORY);
        assert_eq!(log.last(2)[1].detail, format!("p{}", PROBE_HISTORY + 4));
        let now = Instant::now();
        log.transition(now);
        log.transition(now);
        log.transition(now);
        assert_eq!(log.transitions_in_window(now), 3);
        assert_eq!(log.transitions_in_window(now + FLAP_WINDOW), 0);
        let st = log.status(now).unwrap();
        assert_eq!(st.kind, "http");
        assert_eq!(st.transitions_60s, 3);
        assert!(HealthLog::default().status(now).is_none());
    }

    #[tokio::test]
    async fn tcp_probe_classifies_refused_and_unresolvable() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let p = TcpProber::new("127.0.0.1", port, Duration::from_secs(1));
        assert_eq!(p.probe().await.outcome(), ProbeOutcome::Ok);
        drop(l);
        let r = p.probe().await;
        assert_eq!(r.outcome(), ProbeOutcome::Fail, "{r:?}");
        assert!(r.detail.contains("connection refused"), "{r:?}");
        let p = TcpProber::new("nonexistent.invalid", 1, Duration::from_secs(2));
        let r = p.probe().await;
        assert_eq!(r.outcome(), ProbeOutcome::Unknown, "{r:?}");
    }

    #[tokio::test]
    async fn http_probe_status_body_and_classification() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let (code, body) = if req.starts_with("GET /bad") {
                        ("503 Service Unavailable", "down")
                    } else {
                        ("200 OK", "status: ok")
                    };
                    let resp = format!(
                        "HTTP/1.1 {code}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(resp.as_bytes()).await;
                });
            }
        });
        let health = |status: Option<u16>, body: Option<&str>| {
            let mut h: Health = serde_json::from_value(json!({
                "type": "http", "host": "127.0.0.1", "port": null, "url": null,
                "status": status, "body_contains": body, "command": null,
                "interval": "1s", "timeout": "1s", "retries": 1,
                "start_period": "0s", "start_timeout": "5s"
            }))
            .unwrap();
            h.insecure = false;
            h
        };
        let url = |p: &str| format!("http://127.0.0.1:{port}{p}");
        let ok = HttpProber::new(&url("/healthz"), &health(None, None)).unwrap();
        assert_eq!(ok.probe().await.outcome(), ProbeOutcome::Ok);
        let body = HttpProber::new(&url("/healthz"), &health(Some(200), Some("ok"))).unwrap();
        assert!(body.probe().await.ok);
        let nobody = HttpProber::new(&url("/healthz"), &health(None, Some("nope"))).unwrap();
        assert!(!nobody.probe().await.ok);
        let bad = HttpProber::new(&url("/bad"), &health(None, None)).unwrap();
        let r = bad.probe().await;
        assert_eq!(r.outcome(), ProbeOutcome::Fail);
        assert!(r.detail.contains("503"), "{r:?}");
        let want503 = HttpProber::new(&url("/bad"), &health(Some(503), None)).unwrap();
        assert!(want503.probe().await.ok);
        let dns = HttpProber::new("http://nonexistent.invalid:1/", &health(None, None)).unwrap();
        let r = dns.probe().await;
        assert_eq!(r.outcome(), ProbeOutcome::Unknown, "{r:?}");
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let fport = free.local_addr().unwrap().port();
        drop(free);
        let refused =
            HttpProber::new(&format!("http://127.0.0.1:{fport}/"), &health(None, None)).unwrap();
        let r = refused.probe().await;
        assert_eq!(r.outcome(), ProbeOutcome::Fail, "{r:?}");
    }
}
