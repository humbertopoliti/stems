//! A tiny HTTP/1.0 GET client over `TcpStream`, enough for chaos endpoints.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Status code and body of a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// `GET http://127.0.0.1:<port><path>` with a bounded timeout.
pub fn get(port: u16, path: &str, timeout: Duration) -> Result<Response, String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| format!("connect 127.0.0.1:{port}: {e}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    let req = format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("read: {e}"))?;
    parse_response(&String::from_utf8_lossy(&raw))
}

/// Parses a raw HTTP/1.x response.
pub fn parse_response(raw: &str) -> Result<Response, String> {
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .or_else(|| raw.split_once("\n\n"))
        .unwrap_or((raw, ""));
    let status_line = head.lines().next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("bad status line {status_line:?}"))?;
    Ok(Response {
        status,
        body: body.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_responses() {
        let r = parse_response("HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nhello").unwrap();
        assert_eq!(
            r,
            Response {
                status: 200,
                body: "hello".into()
            }
        );
        assert!(parse_response("garbage").is_err());
    }

    #[test]
    fn get_against_a_local_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 512];
            let n = s.read(&mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            s.write_all(b"HTTP/1.0 202 Accepted\r\n\r\n{\"ok\":true}")
                .unwrap();
            req
        });
        let r = get(port, "/__chaos/crash", Duration::from_secs(5)).unwrap();
        assert_eq!(r.status, 202);
        assert_eq!(r.body, "{\"ok\":true}");
        assert!(
            server
                .join()
                .unwrap()
                .starts_with("GET /__chaos/crash HTTP/1.0")
        );
    }
}
