//! The Unix-socket JSON-RPC server: newline-delimited frames
//! (`LinesCodec`), one task per connection, `subscribe_events` streamed on the
//! same connection. A misbehaving client only ever loses its own connection.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use stems_api::{
    API_VERSION, JSONRPC_VERSION, Method, Notification, Request, Response, SubscribeAck,
    SubscribeEventsParams, SubscribeLogsAck, SubscribeLogsParams, VERSION, rpc_code,
    version_mismatch,
};
use stems_core::logs::LogQuery;
use stems_core::{Error, ErrorCode};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinSet;
use tokio_util::codec::{Framed, LinesCodec, LinesCodecError};

use crate::events::EventBus;
use crate::handler::{Handler, RequestCtx};
use crate::logs::LogHub;

/// Largest request frame accepted (bytes).
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

type Conn = Framed<UnixStream, LinesCodec>;

/// Bind `path` with mode 0600, removing a stale socket file first. Call only
/// while holding the workspace lock.
pub fn bind(path: &Path) -> Result<UnixListener, Error> {
    match std::fs::remove_file(path) {
        Ok(()) => tracing::info!(socket = %path.display(), "removed stale socket"),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(sock_err(path, "remove stale socket", e)),
    }
    let l = UnixListener::bind(path).map_err(|e| sock_err(path, "bind socket", e))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| sock_err(path, "chmod socket", e))?;
    Ok(l)
}

fn sock_err(path: &Path, what: &str, e: io::Error) -> Error {
    let long = path.as_os_str().len() >= 100;
    Error::new(
        ErrorCode::Internal,
        format!("cannot {what} at {}: {e}", path.display()),
    )
    .with_hint(if long {
        "the socket path is too long for a Unix socket; use a shorter STEMS_HOME"
    } else {
        "check that the stems home directory is writable (STEMS_HOME)"
    })
}

/// Shared server state.
pub(crate) struct Server {
    pub handler: Arc<dyn Handler>,
    pub events: Arc<EventBus>,
    /// Stem logs (`subscribe_logs`).
    pub logs: Arc<LogHub>,
    /// Flips to true after `daemon.stopped`: connections flush and close.
    pub closing: watch::Receiver<bool>,
}

/// Accept until `stop` flips; returns the live connection tasks. The listener
/// is dropped (closed) on return.
pub(crate) async fn accept_loop(
    listener: UnixListener,
    server: Arc<Server>,
    mut stop: watch::Receiver<bool>,
) -> JoinSet<()> {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            _ = signalled(&mut stop) => break,
            r = listener.accept() => match r {
                Ok((stream, _)) => {
                    while conns.try_join_next().is_some() {}
                    let s = server.clone();
                    conns.spawn(async move { serve_conn(stream, s).await });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    // Avoid a hot loop on persistent errors (EMFILE).
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            },
        }
    }
    conns
}

/// Resolves once the flag is true (without holding the watch guard).
async fn signalled(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}

fn usage(msg: String) -> Error {
    Error::usage(
        msg,
        "send one JSON-RPC 2.0 request object per line: {\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{},\"meta\":{\"actor\":\"cli:me\",\"client_version\":\"x.y.z\",\"api_version\":1}} (see docs/protocol.md)",
    )
}

/// Parse one frame into a request, or the error response to send back.
pub(crate) fn parse_request(line: &str) -> Result<Request, Box<Response>> {
    let v: Value = serde_json::from_str(line).map_err(|e| {
        Box::new(Response::err_with_code(
            Value::Null,
            rpc_code::PARSE_ERROR,
            usage(format!("malformed JSON: {e}")),
        ))
    })?;
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    let req: Request = serde_json::from_value(v).map_err(|e| {
        Box::new(Response::err_with_code(
            id.clone(),
            rpc_code::INVALID_REQUEST,
            usage(format!("invalid request: {e}")),
        ))
    })?;
    if req.jsonrpc != JSONRPC_VERSION {
        return Err(Box::new(Response::err_with_code(
            id,
            rpc_code::INVALID_REQUEST,
            usage(format!("unsupported jsonrpc version {:?}", req.jsonrpc)),
        )));
    }
    Ok(req)
}

async fn send(conn: &mut Conn, frame: &impl serde::Serialize) -> bool {
    match serde_json::to_string(frame) {
        Ok(s) => conn.send(s).await.is_ok(),
        Err(e) => {
            tracing::error!(error = %e, "cannot encode frame");
            false
        }
    }
}

async fn serve_conn(stream: UnixStream, server: Arc<Server>) {
    let mut conn = Framed::new(stream, LinesCodec::new_with_max_length(MAX_FRAME));
    let mut closing = server.closing.clone();
    loop {
        let next = tokio::select! {
            n = conn.next() => n,
            _ = signalled(&mut closing) => return,
        };
        let line = match next {
            None => return,
            Some(Ok(l)) => l,
            Some(Err(LinesCodecError::MaxLineLengthExceeded)) => {
                let r = Response::err_with_code(
                    Value::Null,
                    rpc_code::PARSE_ERROR,
                    usage(format!("frame longer than {MAX_FRAME} bytes")),
                );
                if !send(&mut conn, &r).await {
                    return;
                }
                continue;
            }
            Some(Err(e)) => {
                tracing::debug!(error = %e, "connection read error");
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let req = match parse_request(&line) {
            Ok(r) => r,
            Err(resp) => {
                tracing::debug!("rejected malformed frame");
                if !send(&mut conn, &*resp).await {
                    return;
                }
                continue;
            }
        };
        if req.meta.api_version != API_VERSION {
            let e = version_mismatch(
                &req.meta.client_version,
                req.meta.api_version,
                VERSION,
                API_VERSION,
            );
            if !send(&mut conn, &Response::err(req.id, e)).await {
                return;
            }
            continue;
        }
        let ctx = RequestCtx {
            actor: req.meta.actor.clone(),
            client_version: req.meta.client_version.clone(),
            api_version: req.meta.api_version,
            request_id: req.id.clone(),
        };
        if req.method == Method::SUBSCRIBE_EVENTS {
            let p: SubscribeEventsParams = match serde_json::from_value(req.params) {
                Ok(p) => p,
                Err(e) => {
                    let r = Response::err(req.id, usage(format!("invalid params: {e}")));
                    if !send(&mut conn, &r).await {
                        return;
                    }
                    continue;
                }
            };
            tracing::debug!(actor = %ctx.actor, since = ?p.since_seq, "subscribe_events");
            stream_events(conn, &server, req.id, p.since_seq).await;
            return;
        }
        if req.method == Method::SUBSCRIBE_LOGS {
            let q = serde_json::from_value::<SubscribeLogsParams>(req.params)
                .map_err(|e| usage(format!("invalid params: {e}")))
                .and_then(|p| crate::logs::to_query(&p.filter, chrono::Utc::now()));
            let q = match q {
                Ok(q) => q,
                Err(e) => {
                    if !send(&mut conn, &Response::err(req.id, e)).await {
                        return;
                    }
                    continue;
                }
            };
            tracing::debug!(actor = %ctx.actor, stems = ?q.stems, "subscribe_logs");
            stream_logs(conn, &server, req.id, q).await;
            return;
        }
        tracing::debug!(actor = %ctx.actor, method = %req.method, "request");
        let resp = match server
            .handler
            .handle(ctx, req.method.as_str(), req.params)
            .await
        {
            Ok(v) => Response::ok(req.id, v),
            Err(e) => Response::err(req.id, e),
        };
        if !send(&mut conn, &resp).await {
            return;
        }
    }
}

/// `subscribe_logs`: subscribe live first, then (with `since` or `tail`)
/// replay history from the ring/files, ack with the replay size, send the
/// replay and then live records (skipping those the replay already held)
/// until the client closes or the daemon stops.
async fn stream_logs(mut conn: Conn, server: &Server, id: Value, q: LogQuery) {
    let mut rx = server.logs.subscribe();
    let replay = if q.since.is_some() || q.tail.is_some() {
        let (hub, rq) = (server.logs.clone(), q.clone());
        let opts = crate::logs::QueryOptions::default();
        tokio::task::spawn_blocking(move || hub.query(&rq, opts))
            .await
            .unwrap_or_default()
    } else {
        crate::logs::QueryOutcome::default()
    };
    let ack = SubscribeLogsAck {
        subscribed: true,
        replay: replay.records.len(),
    };
    if !send(&mut conn, &Response::ok(id, json!(ack))).await {
        return;
    }
    for rec in &replay.records {
        if !send(&mut conn, &Notification::log(rec)).await {
            return;
        }
    }
    // Live records: every filter but the time window and `tail`.
    let live_q = LogQuery {
        since: None,
        until: None,
        tail: None,
        ..q
    };
    let seen = replay.next_seq;
    let mut closing = server.closing.clone();
    loop {
        tokio::select! {
            biased;
            r = rx.recv() => match r {
                Ok(live) => {
                    if seen.get(&live.record.stem).is_some_and(|n| live.seq < *n)
                        || !live_q.matches(&live.record)
                    {
                        continue;
                    }
                    if !send(&mut conn, &Notification::log(&live.record)).await { return; }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!(skipped = n, "subscribe_logs client lagged");
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            n = conn.next() => match n {
                Some(Ok(_)) => {}
                _ => return,
            },
            _ = signalled(&mut closing) => {
                let _ = SinkExt::<String>::close(&mut conn).await;
                return;
            }
        }
    }
}

/// Ack, replay, then live events until the client closes or the daemon stops.
async fn stream_events(mut conn: Conn, server: &Server, id: Value, since: Option<u64>) {
    let (backlog, mut rx) = server.events.subscribe_from(since);
    let ack = SubscribeAck {
        subscribed: true,
        last_seq: server.events.last_seq(),
    };
    if !send(&mut conn, &Response::ok(id, json!(ack))).await {
        return;
    }
    let mut last = since.unwrap_or(0);
    for ev in backlog {
        last = ev.seq;
        if !send(&mut conn, &Notification::event(&ev)).await {
            return;
        }
    }
    let mut closing = server.closing.clone();
    loop {
        tokio::select! {
            biased;
            r = rx.recv() => match r {
                Ok(ev) => {
                    if ev.seq <= last { continue; }
                    last = ev.seq;
                    if !send(&mut conn, &Notification::event(&ev)).await { return; }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    // Fell behind the live channel: catch up from the ring.
                    for ev in server.events.replay(last) {
                        last = ev.seq;
                        if !send(&mut conn, &Notification::event(&ev)).await { return; }
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            // Client input is ignored; EOF or error means it went away.
            n = conn.next() => match n {
                Some(Ok(_)) => {}
                _ => return,
            },
            _ = signalled(&mut closing) => {
                // Flush what was emitted before closing (daemon.stopped).
                while let Ok(ev) = rx.try_recv() {
                    if ev.seq > last {
                        last = ev.seq;
                        if !send(&mut conn, &Notification::event(&ev)).await { return; }
                    }
                }
                let _ = SinkExt::<String>::close(&mut conn).await;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_errors() {
        let r = parse_request("{not json").unwrap_err();
        let e = r.error.unwrap();
        assert_eq!(e.code, rpc_code::PARSE_ERROR);
        assert_eq!(e.data.code, ErrorCode::Usage);
        assert_eq!(r.id, Value::Null);

        let r = parse_request(r#"{"jsonrpc":"2.0","id":5,"method":"ping"}"#).unwrap_err();
        assert_eq!(r.id, json!(5));
        assert_eq!(r.error.unwrap().code, rpc_code::INVALID_REQUEST);

        let r = parse_request(
            r#"{"jsonrpc":"1.0","id":5,"method":"ping","meta":{"actor":"a","client_version":"0","api_version":1}}"#,
        )
        .unwrap_err();
        assert_eq!(r.error.unwrap().code, rpc_code::INVALID_REQUEST);

        let ok = parse_request(
            r#"{"jsonrpc":"2.0","id":5,"method":"ping","params":{},"meta":{"actor":"a","client_version":"0","api_version":1}}"#,
        )
        .unwrap();
        assert_eq!(ok.method, Method::PING);
    }

    #[tokio::test]
    async fn bind_is_0600_and_replaces_stale_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        std::fs::write(&p, "stale").unwrap();
        let l = bind(&p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        drop(l);
    }
}
