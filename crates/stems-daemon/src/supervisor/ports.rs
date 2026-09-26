//! Host ports: checking fixed ports are free (`PORT_IN_USE`) and allocating
//! `port: auto` ports, sticky per stem for the daemon's lifetime (FR-ST-5).

use std::collections::HashMap;
use std::sync::Mutex;

use indexmap::IndexMap;
use serde_json::json;
use stems_config::{Port, PortRef, Workspace};
use stems_core::{Error, ErrorCode};

/// Sticky `auto` port allocations: `stem -> port name -> port`.
#[derive(Default, Debug)]
pub struct PortBook {
    auto: Mutex<HashMap<String, IndexMap<String, u16>>>,
}

/// Outcome of [`PortBook::host_port`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostPort {
    /// The port.
    pub port: u16,
    /// It was allocated by this call (emit `stem.port_allocated`).
    pub newly_allocated: bool,
}

impl PortBook {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, IndexMap<String, u16>>> {
        self.auto.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The allocated `auto` port of `stem`/`name`, if any.
    pub fn allocated(&self, stem: &str, name: &str) -> Option<u16> {
        self.lock().get(stem).and_then(|m| m.get(name)).copied()
    }

    /// Restore a sticky `auto` allocation (crash recovery, 11).
    pub fn restore(&self, stem: &str, name: &str, port: u16) {
        self.lock()
            .entry(stem.to_string())
            .or_default()
            .insert(name.to_string(), port);
    }

    /// Host port of `port` of `stem`: the fixed number, or the sticky
    /// allocation of an `auto` port (allocating a free one on first use).
    pub fn host_port(&self, stem: &str, port: &Port) -> Result<HostPort, Error> {
        match port.port {
            PortRef::Fixed(n) => Ok(HostPort {
                port: n,
                newly_allocated: false,
            }),
            PortRef::Auto => {
                let mut book = self.lock();
                let taken: Vec<u16> = book.values().flat_map(|m| m.values().copied()).collect();
                let entry = book.entry(stem.to_string()).or_default();
                if let Some(p) = entry.get(&port.name) {
                    return Ok(HostPort {
                        port: *p,
                        newly_allocated: false,
                    });
                }
                let p = free_port(&taken)?;
                entry.insert(port.name.clone(), p);
                Ok(HostPort {
                    port: p,
                    newly_allocated: true,
                })
            }
        }
    }

    /// Resolve `${stem.<n>.port}` / `${stem.<n>.ports.<p>}` of `ws` (fixed or
    /// allocated). Allocates `auto` ports on demand; returns them in
    /// `allocated` so the caller can emit events.
    pub fn resolve_ref(
        &self,
        ws: &Workspace,
        stem: &str,
        port_name: Option<&str>,
        allocated: &mut Vec<(String, String, u16)>,
    ) -> Option<u16> {
        let s = ws.stem(stem)?;
        let port = match port_name {
            None => s.primary_port()?,
            Some(n) => s.ports.iter().find(|p| p.name == n)?,
        };
        let hp = self.host_port(stem, port).ok()?;
        if hp.newly_allocated {
            allocated.push((stem.to_string(), port.name.clone(), hp.port));
        }
        Some(hp.port)
    }
}

/// An ephemeral port nobody listens on (bind `127.0.0.1:0`), not in `taken`.
fn free_port(taken: &[u16]) -> Result<u16, Error> {
    for _ in 0..20 {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| Error::internal(format!("cannot allocate a free port: {e}")))?;
        let p = l
            .local_addr()
            .map_err(|e| Error::internal(e.to_string()))?
            .port();
        if !taken.contains(&p) {
            return Ok(p);
        }
    }
    Err(Error::internal("cannot allocate a free port"))
}

/// `PORT_IN_USE` if something already listens on `port` (with its pid and
/// command when visible), for `stem`'s port `name`.
pub fn check_free(stem: &str, name: &str, port: u16) -> Result<(), Error> {
    let listeners = stems_runtime::os::listeners_on_port(port);
    if let Some(l) = listeners.first() {
        return Err(Error::new(
            ErrorCode::PortInUse,
            format!(
                "port {port} of `{stem}` is already in use by pid {} ({})",
                l.pid, l.command
            ),
        )
        .with_hint(format!(
            "stop that process (`kill {}`), or change `stems.{stem}.ports` (e.g. `port: auto`) in stems.local.yaml",
            l.pid
        ))
        .with_details(json!({
            "stem": stem,
            "name": name,
            "port": port,
            "pid": l.pid,
            "command": l.command,
            "listeners": listeners,
        })));
    }
    // Not visible as a listener (another user's process?): try to bind.
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => Err(Error::new(
            ErrorCode::PortInUse,
            format!("port {port} of `{stem}` is already in use"),
        )
        .with_hint(format!(
            "find the owner with `lsof -nP -iTCP:{port} -sTCP:LISTEN`, or change `stems.{stem}.ports`"
        ))
        .with_details(json!({
            "stem": stem, "name": name, "port": port, "pid": null, "command": null,
        }))),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto(name: &str) -> Port {
        Port {
            name: name.into(),
            port: PortRef::Auto,
            container_port: None,
        }
    }

    #[test]
    fn auto_ports_are_sticky_and_distinct() {
        let b = PortBook::default();
        let a1 = b.host_port("web", &auto("http")).unwrap();
        assert!(a1.newly_allocated && a1.port > 1024);
        let a2 = b.host_port("web", &auto("http")).unwrap();
        assert_eq!(a2.port, a1.port);
        assert!(!a2.newly_allocated);
        let other = b.host_port("api", &auto("http")).unwrap();
        assert_ne!(other.port, a1.port);
        assert_eq!(b.allocated("web", "http"), Some(a1.port));
        let fixed = Port {
            name: "x".into(),
            port: PortRef::Fixed(1234),
            container_port: None,
        };
        assert_eq!(b.host_port("web", &fixed).unwrap().port, 1234);
    }

    #[test]
    fn port_in_use_names_the_listener() {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let e = check_free("api", "http", port).unwrap_err();
        assert_eq!(e.code, ErrorCode::PortInUse);
        assert_eq!(e.details["port"], port);
        assert_eq!(e.details["pid"], std::process::id() as i64);
        drop(l);
        check_free("api", "http", port).unwrap();
    }
}
