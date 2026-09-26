//! Async client for the daemon socket (feature `client`).
//!
//! ```no_run
//! # async fn demo() -> Result<(), stems_core::Error> {
//! use stems_api::client::{Client, ClientOptions};
//! let client = Client::connect("/path/stemsd.sock", ClientOptions::new("cli:me")).await?;
//! let pong: serde_json::Value = client.call(stems_api::Method::PING, serde_json::json!({})).await?;
//! # let _ = pong; Ok(()) }
//! ```

use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::{SinkExt, Stream, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use stems_core::{Error, ErrorCode};
use tokio::net::UnixStream;
use tokio::sync::Mutex;
use tokio_util::codec::{Framed, LinesCodec};

use crate::{
    API_VERSION, DaemonInfo, Event, Method, NOTIFY_EVENT, Notification, Request, RequestMeta,
    Response, ShutdownResult, SubscribeEventsParams, VERSION, version_mismatch,
};

/// Env var overriding the client's reported stems version (tests only).
pub const ENV_FAKE_VERSION: &str = "STEMS_FAKE_VERSION";

/// Default connect timeout.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Default per-call timeout.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Max bytes of one frame the client accepts.
const MAX_FRAME: usize = 16 * 1024 * 1024;

/// The version this client reports: `STEMS_FAKE_VERSION` or [`VERSION`].
pub fn client_version() -> String {
    std::env::var(ENV_FAKE_VERSION)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| VERSION.to_string())
}

/// Connection options.
#[derive(Clone, Debug)]
pub struct ClientOptions {
    /// `cli:<user>`, `tui:<user>`, `mcp:<client>`.
    pub actor: String,
    /// Reported stems version (default: [`client_version()`]).
    pub client_version: String,
    /// Connect timeout (default 2 s).
    pub connect_timeout: Duration,
    /// Default per-call timeout (default 30 s).
    pub call_timeout: Duration,
    /// Refuse a daemon whose stems version differs from `client_version`
    /// (default true). With `false` only the API version must match (the
    /// daemon rejects every request otherwise); `stems daemon status|stop`
    /// use this so an upgraded CLI can still inspect and stop an old daemon.
    pub check_version: bool,
}

impl ClientOptions {
    /// Defaults for `actor`.
    pub fn new(actor: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            client_version: client_version(),
            connect_timeout: CONNECT_TIMEOUT,
            call_timeout: CALL_TIMEOUT,
            check_version: true,
        }
    }

    fn meta(&self) -> RequestMeta {
        RequestMeta {
            actor: self.actor.clone(),
            client_version: self.client_version.clone(),
            api_version: API_VERSION,
        }
    }
}

type Conn = Framed<UnixStream, LinesCodec>;

/// A stream of events from `subscribe_events`; ends when the daemon closes it.
pub type EventStream = Pin<Box<dyn Stream<Item = Event> + Send>>;

/// A connection to a running daemon. Calls on one client are serialised.
pub struct Client {
    socket: PathBuf,
    opts: ClientOptions,
    conn: Mutex<Conn>,
    next_id: AtomicU64,
    info: DaemonInfo,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("socket", &self.socket)
            .field("actor", &self.opts.actor)
            .field("info", &self.info)
            .finish()
    }
}

fn not_running(socket: &Path, why: impl std::fmt::Display) -> Error {
    Error::new(
        ErrorCode::DaemonNotRunning,
        format!("the stems daemon is not running ({why})"),
    )
    .with_hint("start it with `stems daemon start` (or `stems up`)")
    .with_details(json!({ "socket": socket }))
}

async fn open(socket: &Path, timeout: Duration) -> Result<Conn, Error> {
    match tokio::time::timeout(timeout, UnixStream::connect(socket)).await {
        Err(_) => Err(not_running(
            socket,
            format!("connect timed out after {}ms", timeout.as_millis()),
        )),
        Ok(Err(e)) => Err(not_running(socket, connect_reason(&e))),
        Ok(Ok(s)) => Ok(Framed::new(s, LinesCodec::new_with_max_length(MAX_FRAME))),
    }
}

fn connect_reason(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::NotFound => "no socket".into(),
        io::ErrorKind::ConnectionRefused => "socket refuses connections".into(),
        _ => e.to_string(),
    }
}

async fn roundtrip(conn: &mut Conn, socket: &Path, req: &Request) -> Result<Value, Error> {
    let line = serde_json::to_string(req).map_err(|e| Error::internal(e.to_string()))?;
    conn.send(line)
        .await
        .map_err(|e| not_running(socket, format!("write failed: {e}")))?;
    loop {
        match conn.next().await {
            None => return Err(not_running(socket, "connection closed by the daemon")),
            Some(Err(e)) => return Err(not_running(socket, format!("read failed: {e}"))),
            Some(Ok(line)) => {
                let v: Value = serde_json::from_str(&line).map_err(|e| {
                    Error::internal(format!("daemon sent invalid JSON: {e}: {line}"))
                })?;
                // Skip stray notifications (no id).
                if v.get("id").is_none() {
                    continue;
                }
                let resp: Response = serde_json::from_value(v).map_err(|e| {
                    Error::internal(format!("daemon sent an invalid response: {e}"))
                })?;
                if resp.id != req.id {
                    continue;
                }
                return resp.into_result();
            }
        }
    }
}

impl Client {
    /// Connect, call `info` and check versions.
    ///
    /// * `DAEMON_NOT_RUNNING` if the socket is missing, refuses or times out (2 s).
    /// * `DAEMON_VERSION_MISMATCH` if the daemon's API version differs, or its
    ///   stems version does (unless [`ClientOptions::check_version`] is off).
    pub async fn connect(
        socket_path: impl AsRef<Path>,
        opts: ClientOptions,
    ) -> Result<Client, Error> {
        let socket = socket_path.as_ref().to_path_buf();
        let mut conn = open(&socket, opts.connect_timeout).await?;
        let req = Request::new(0, Method::INFO, json!({}), opts.meta());
        let v = match tokio::time::timeout(opts.call_timeout, roundtrip(&mut conn, &socket, &req))
            .await
        {
            Err(_) => return Err(not_running(&socket, "`info` timed out")),
            Ok(r) => r?,
        };
        let info: DaemonInfo = serde_json::from_value(v)
            .map_err(|e| Error::internal(format!("invalid `info` result: {e}")))?;
        if info.api_version != API_VERSION
            || (opts.check_version && info.version != opts.client_version)
        {
            return Err(version_mismatch(
                &opts.client_version,
                API_VERSION,
                &info.version,
                info.api_version,
            ));
        }
        Ok(Client {
            socket,
            opts,
            conn: Mutex::new(conn),
            next_id: AtomicU64::new(1),
            info,
        })
    }

    /// `info` as returned at connect time.
    pub fn info(&self) -> &DaemonInfo {
        &self.info
    }

    /// The socket this client is connected to.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Call `method` with the default timeout.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: impl Into<Method>,
        params: impl Serialize,
    ) -> Result<T, Error> {
        self.call_with_timeout(method, params, self.opts.call_timeout)
            .await
    }

    /// Call `method` with an explicit timeout.
    pub async fn call_with_timeout<T: DeserializeOwned>(
        &self,
        method: impl Into<Method>,
        params: impl Serialize,
        timeout: Duration,
    ) -> Result<T, Error> {
        let method = method.into();
        let params = serde_json::to_value(params)
            .map_err(|e| Error::internal(format!("cannot encode params: {e}")))?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = Request::new(id, method.clone(), params, self.opts.meta());
        let mut conn = self.conn.lock().await;
        let v = match tokio::time::timeout(timeout, roundtrip(&mut conn, &self.socket, &req)).await
        {
            Err(_) => {
                return Err(Error::new(
                    ErrorCode::DaemonNotRunning,
                    format!(
                        "the daemon did not answer `{method}` within {}ms",
                        timeout.as_millis()
                    ),
                )
                .with_hint("the daemon may be hung; check its log, then `stems daemon stop`")
                .with_details(json!({ "socket": self.socket, "method": method })));
            }
            Ok(r) => r?,
        };
        serde_json::from_value(v)
            .map_err(|e| Error::internal(format!("invalid `{method}` result: {e}")))
    }

    /// Subscribe to events on a separate connection. With `since_seq`, buffered
    /// events with a greater seq are replayed first. The stream ends when the
    /// daemon shuts down (after `daemon.stopped`, best effort).
    pub async fn subscribe_events(&self, since_seq: Option<u64>) -> Result<EventStream, Error> {
        let mut conn = open(&self.socket, self.opts.connect_timeout).await?;
        let req = Request::new(
            0,
            Method::SUBSCRIBE_EVENTS,
            serde_json::to_value(SubscribeEventsParams { since_seq }).unwrap_or_default(),
            self.opts.meta(),
        );
        match tokio::time::timeout(
            self.opts.call_timeout,
            roundtrip(&mut conn, &self.socket, &req),
        )
        .await
        {
            Err(_) => return Err(not_running(&self.socket, "`subscribe_events` timed out")),
            Ok(r) => {
                r?;
            }
        }
        let s = futures::stream::unfold(conn, |mut conn| async move {
            loop {
                let line = match conn.next().await {
                    Some(Ok(l)) => l,
                    _ => return None,
                };
                let Ok(n) = serde_json::from_str::<Notification>(&line) else {
                    continue;
                };
                if n.method != NOTIFY_EVENT {
                    continue;
                }
                if let Ok(ev) = serde_json::from_value::<Event>(n.params) {
                    return Some((ev, conn));
                }
            }
        });
        Ok(Box::pin(s))
    }

    /// Ask the daemon to shut down (returns once the daemon acknowledged; the
    /// socket and lock disappear shortly after).
    pub async fn shutdown(&self) -> Result<(), Error> {
        let _: ShutdownResult = self.call(Method::SHUTDOWN, json!({})).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_socket_is_daemon_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let err = Client::connect(dir.path().join("nope.sock"), ClientOptions::new("cli:test"))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::DaemonNotRunning);
        assert_eq!(err.exit_code(), 4);
    }

    #[tokio::test]
    async fn refusing_socket_is_daemon_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.sock");
        // Bind then drop: the file stays but nobody listens.
        drop(std::os::unix::net::UnixListener::bind(&p).unwrap());
        let err = Client::connect(&p, ClientOptions::new("cli:test"))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::DaemonNotRunning);
    }
}
