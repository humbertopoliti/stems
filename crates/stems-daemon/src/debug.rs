//! `_debug.*` RPCs (only with `STEMS_DEBUG_RPC=1`): drive
//! [`stems_runtime::ProcessRuntime`] directly so scenarios can exercise the
//! runtime before the supervisor exists.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use stems_api::{EventKind, Method};
use stems_core::{Error, ErrorCode};
use stems_runtime::{
    Handle, HandleId, OutputStreamKind, ProcessRuntime, ProcessSpec, Runtime, RuntimeError,
    StartSpec, StopOutcome,
};

use crate::events::{EventBus, EventDraft};
use crate::handler::RequestCtx;

/// Env var enabling the debug RPCs.
pub const ENV_DEBUG_RPC: &str = "STEMS_DEBUG_RPC";

/// Default stop grace for `_debug.stop_raw` and shutdown.
const DEFAULT_GRACE: Duration = Duration::from_secs(2);

/// How long shutdown waits for exit watchers (each waits up to 1 s for
/// buffered output) so `process.exited` lands before `daemon.stopped`.
const WATCHER_DEADLINE: Duration = Duration::from_secs(3);

pub(crate) struct DebugRpc {
    runtime: Arc<ProcessRuntime>,
    handles: Arc<Mutex<HashMap<HandleId, Handle>>>,
    events: Arc<EventBus>,
    /// Exit-watcher tasks, one per started process.
    watchers: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

#[derive(Deserialize)]
struct StartParams {
    spec: ProcessSpec,
}

#[derive(Deserialize)]
struct StopParams {
    handle: Value,
    #[serde(default)]
    grace_ms: Option<u64>,
}

#[derive(Deserialize)]
struct DescribeParams {
    handle: Value,
}

fn params<T: for<'de> Deserialize<'de>>(method: &str, v: Value) -> Result<T, Error> {
    serde_json::from_value(v).map_err(|e| {
        Error::usage(
            format!("invalid params for `{method}`: {e}"),
            "see docs/protocol.md for the params of the _debug methods",
        )
    })
}

fn runtime_err(e: RuntimeError) -> Error {
    Error::new(ErrorCode::Internal, e.to_string())
}

fn stream_name(k: OutputStreamKind) -> &'static str {
    match k {
        OutputStreamKind::Out => "stdout",
        OutputStreamKind::Err => "stderr",
    }
}

impl DebugRpc {
    pub(crate) fn new(events: Arc<EventBus>) -> Self {
        Self {
            runtime: Arc::new(ProcessRuntime::new()),
            handles: Arc::new(Mutex::new(HashMap::new())),
            events,
            watchers: Mutex::new(Vec::new()),
        }
    }

    fn handles(&self) -> std::sync::MutexGuard<'_, HashMap<HandleId, Handle>> {
        self.handles.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn resolve(&self, v: &Value) -> Result<Handle, Error> {
        if let Some(n) = v.as_u64() {
            return self.handles().get(&HandleId(n)).cloned().ok_or_else(|| {
                Error::usage(
                    format!("unknown debug handle {n}"),
                    "pass the handle object returned by `_debug.start_raw`",
                )
            });
        }
        serde_json::from_value(v.clone()).map_err(|e| {
            Error::usage(
                format!("invalid handle: {e}"),
                "pass the handle object returned by `_debug.start_raw` (or its numeric id)",
            )
        })
    }

    pub(crate) async fn handle(
        &self,
        ctx: &RequestCtx,
        method: &str,
        p: Value,
    ) -> Option<Result<Value, Error>> {
        let r = match method {
            m if m == Method::DEBUG_START_RAW => self.start(ctx, params(m, p)).await,
            m if m == Method::DEBUG_STOP_RAW => self.stop(params(m, p)).await,
            m if m == Method::DEBUG_DESCRIBE => self.describe(params(m, p)).await,
            _ => return None,
        };
        Some(r)
    }

    async fn start(&self, ctx: &RequestCtx, p: Result<StartParams, Error>) -> Result<Value, Error> {
        let p = p?;
        let h = self
            .runtime
            .start(&StartSpec::Process(p.spec))
            .await
            .map_err(runtime_err)?;
        self.handles().insert(h.id(), h.clone());
        let id = h.id().0;
        let pid = h.pid();

        let output = self.runtime.output_stream(&h).map(|mut out| {
            let events = self.events.clone();
            let actor = ctx.actor.clone();
            tokio::spawn(async move {
                while let Some(line) = out.recv().await {
                    events.emit(
                        EventDraft::new(EventKind::PROCESS_OUTPUT, actor.clone()).data(json!({
                            "handle": id,
                            "pid": pid,
                            "stream": stream_name(line.stream),
                            "text": line.text,
                        })),
                    );
                }
            })
        });

        let runtime = self.runtime.clone();
        let events = self.events.clone();
        let handles = self.handles.clone();
        let actor = ctx.actor.clone();
        let hh = h.clone();
        let watcher = tokio::spawn(async move {
            let status = runtime.wait(&hh).await;
            if let Some(o) = output {
                // Let buffered output land before the exit event.
                let _ = tokio::time::timeout(Duration::from_secs(1), o).await;
            }
            let (code, signal) = match &status {
                Ok(s) => (s.code, s.signal),
                Err(_) => (None, None),
            };
            let reason = match (code, signal) {
                (Some(c), _) => format!("exited with code {c}"),
                (_, Some(s)) => format!("killed by signal {s}"),
                _ => "exited".to_string(),
            };
            tracing::info!(handle = id, pid, ?code, ?signal, "debug process exited");
            events.emit(
                EventDraft::new(EventKind::PROCESS_EXITED, actor)
                    .reason(reason)
                    .data(json!({ "handle": id, "pid": pid, "code": code, "signal": signal })),
            );
            handles
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&hh.id());
            runtime.release(&hh);
        });
        {
            let mut ws = self.watchers.lock().unwrap_or_else(|e| e.into_inner());
            ws.retain(|w| !w.is_finished());
            ws.push(watcher);
        }
        serde_json::to_value(&h).map_err(|e| Error::internal(e.to_string()))
    }

    async fn stop(&self, p: Result<StopParams, Error>) -> Result<Value, Error> {
        let p = p?;
        let h = self.resolve(&p.handle)?;
        let grace = p.grace_ms.map_or(DEFAULT_GRACE, Duration::from_millis);
        let outcome = match self.runtime.stop(&h, grace).await {
            Ok(o) => o,
            Err(RuntimeError::NotFound(_)) => StopOutcome::AlreadyDead,
            Err(e) => return Err(runtime_err(e)),
        };
        Ok(serde_json::to_value(outcome).unwrap_or(Value::Null))
    }

    async fn describe(&self, p: Result<DescribeParams, Error>) -> Result<Value, Error> {
        let p = p?;
        let h = self.resolve(&p.handle)?;
        let facts = self.runtime.describe(&h).await.map_err(runtime_err)?;
        serde_json::to_value(facts).map_err(|e| Error::internal(e.to_string()))
    }

    /// Stop every debug process still tracked (shutdown path).
    pub(crate) async fn stop_all(&self) {
        let hs: Vec<Handle> = self.handles().values().cloned().collect();
        let stops = hs.iter().map(|h| self.runtime.stop(h, DEFAULT_GRACE));
        for (h, r) in hs.iter().zip(futures::future::join_all(stops).await) {
            tracing::info!(handle = h.id().0, pid = h.pid(), outcome = ?r.ok(), "debug process stopped on shutdown");
        }
        let watchers: Vec<_> =
            std::mem::take(&mut *self.watchers.lock().unwrap_or_else(|e| e.into_inner()));
        if tokio::time::timeout(WATCHER_DEADLINE, futures::future::join_all(watchers))
            .await
            .is_err()
        {
            tracing::warn!("debug exit watchers exceeded their shutdown deadline");
        }
    }
}
