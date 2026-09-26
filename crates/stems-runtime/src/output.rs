//! Line-oriented output capture: splitting raw pipe bytes into [`OutputLine`]s
//! and fanning them out through a bounded broadcast channel.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::broadcast;

/// Capacity of each handle's output broadcast channel (lines).
pub const OUTPUT_CHANNEL_CAPACITY: usize = 4096;
/// Maximum bytes kept of a single line; the rest is discarded and a marker appended.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// Which pipe a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStreamKind {
    Out,
    Err,
}

/// One line of process output (without the trailing newline).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputLine {
    pub ts: SystemTime,
    pub stream: OutputStreamKind,
    pub text: String,
}

/// Incremental byte -> line splitter.
///
/// * splits on `\n`, strips one trailing `\r` (so `\r\n` works);
/// * decodes lossily (invalid UTF-8 becomes U+FFFD);
/// * keeps at most [`MAX_LINE_BYTES`] of a line and appends
///   `" [truncated N bytes]"` for the rest;
/// * [`LineSplitter::finish`] yields a trailing partial line at EOF.
#[derive(Debug, Default)]
pub struct LineSplitter {
    buf: Vec<u8>,
    discarded: usize,
}

impl LineSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk; returns the complete lines it terminated.
    pub fn push(&mut self, mut chunk: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        while !chunk.is_empty() {
            match chunk.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    self.append(&chunk[..i]);
                    lines.push(self.take_line());
                    chunk = &chunk[i + 1..];
                }
                None => {
                    self.append(chunk);
                    chunk = &[];
                }
            }
        }
        lines
    }

    /// Flush the partial line at EOF, if any.
    pub fn finish(&mut self) -> Option<String> {
        if self.buf.is_empty() && self.discarded == 0 {
            None
        } else {
            Some(self.take_line())
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        let room = MAX_LINE_BYTES.saturating_sub(self.buf.len());
        let keep = room.min(bytes.len());
        self.buf.extend_from_slice(&bytes[..keep]);
        self.discarded += bytes.len() - keep;
    }

    fn take_line(&mut self) -> String {
        // A '\r' that was cut off by truncation is simply part of the discarded tail.
        if self.discarded == 0 && self.buf.last() == Some(&b'\r') {
            self.buf.pop();
        }
        let mut s = String::from_utf8_lossy(&self.buf).into_owned();
        if self.discarded > 0 {
            s.push_str(&format!(" [truncated {} bytes]", self.discarded));
        }
        self.buf.clear();
        self.discarded = 0;
        s
    }
}

/// Read `reader` to EOF, broadcasting each line. Lines sent while nobody is
/// subscribed are dropped silently (there is no one to lag).
pub(crate) async fn pump<R: AsyncRead + Unpin>(
    mut reader: R,
    stream: OutputStreamKind,
    tx: broadcast::Sender<OutputLine>,
) {
    let mut splitter = LineSplitter::new();
    let mut buf = vec![0u8; 8192];
    let send = |text: String| {
        let _ = tx.send(OutputLine {
            ts: SystemTime::now(),
            stream,
            text,
        });
    };
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => splitter.push(&buf[..n]).into_iter().for_each(send),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                tracing::debug!(error = %e, ?stream, "output pipe read failed");
                break;
            }
        }
    }
    if let Some(last) = splitter.finish() {
        send(last);
    }
}

/// What a subscriber receives: a line, or notice that it fell behind and
/// `n` lines were dropped for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputEvent {
    Line(OutputLine),
    Dropped(u64),
}

/// A subscription to one handle's output. Wraps a `broadcast::Receiver` and
/// accounts receiver lag into the handle's shared dropped-lines counter, so a
/// slow consumer loses lines instead of making the daemon buffer without bound.
#[derive(Debug)]
pub struct OutputStream {
    rx: broadcast::Receiver<OutputLine>,
    dropped_total: Arc<AtomicU64>,
    dropped_here: u64,
}

impl OutputStream {
    pub(crate) fn new(rx: broadcast::Receiver<OutputLine>, dropped_total: Arc<AtomicU64>) -> Self {
        Self {
            rx,
            dropped_total,
            dropped_here: 0,
        }
    }

    /// Next line or drop notice; `None` once the process's pipes are closed
    /// and every buffered line has been delivered.
    pub async fn recv_event(&mut self) -> Option<OutputEvent> {
        match self.rx.recv().await {
            Ok(line) => Some(OutputEvent::Line(line)),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                self.dropped_here += n;
                self.dropped_total.fetch_add(n, Ordering::Relaxed);
                Some(OutputEvent::Dropped(n))
            }
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }

    /// Next line, silently skipping (but counting) drops.
    pub async fn recv(&mut self) -> Option<OutputLine> {
        loop {
            match self.recv_event().await? {
                OutputEvent::Line(l) => return Some(l),
                OutputEvent::Dropped(_) => continue,
            }
        }
    }

    /// Lines this subscriber has missed so far.
    pub fn dropped(&self) -> u64 {
        self.dropped_here
    }

    /// Unwrap into the raw receiver (lag is then no longer counted).
    pub fn into_inner(self) -> broadcast::Receiver<OutputLine> {
        self.rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lf_and_crlf() {
        let mut s = LineSplitter::new();
        assert_eq!(s.push(b"a\nb\r\nc"), vec!["a", "b"]);
        assert_eq!(s.push(b"d\r"), Vec::<String>::new());
        assert_eq!(s.push(b"\n"), vec!["cd"]);
        assert_eq!(s.finish(), None);
    }

    #[test]
    fn empty_lines_are_kept() {
        let mut s = LineSplitter::new();
        assert_eq!(s.push(b"\n\r\nx\n"), vec!["", "", "x"]);
    }

    #[test]
    fn partial_line_at_eof() {
        let mut s = LineSplitter::new();
        assert_eq!(s.push(b"one\ntw"), vec!["one"]);
        assert_eq!(s.push(b"o"), Vec::<String>::new());
        assert_eq!(s.finish().as_deref(), Some("two"));
        assert_eq!(s.finish(), None);
    }

    #[test]
    fn invalid_utf8_is_lossy() {
        let mut s = LineSplitter::new();
        let lines = s.push(b"ok \xff\xfe end\n");
        assert_eq!(lines, vec!["ok \u{FFFD}\u{FFFD} end"]);
    }

    #[test]
    fn utf8_split_across_chunks_is_preserved() {
        let mut s = LineSplitter::new();
        let bytes = "caf\u{e9}\n".as_bytes();
        assert!(s.push(&bytes[..4]).is_empty());
        assert_eq!(s.push(&bytes[4..]), vec!["caf\u{e9}"]);
    }

    #[test]
    fn long_line_is_truncated_with_marker() {
        let mut s = LineSplitter::new();
        let big = vec![b'x'; MAX_LINE_BYTES + 10];
        assert!(s.push(&big).is_empty());
        let lines = s.push(b"yy\nnext\n");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("xxx"));
        assert!(
            lines[0].ends_with(" [truncated 12 bytes]"),
            "{}",
            &lines[0][MAX_LINE_BYTES..]
        );
        assert_eq!(
            lines[0].len(),
            MAX_LINE_BYTES + " [truncated 12 bytes]".len()
        );
        assert_eq!(lines[1], "next");
    }

    #[test]
    fn truncated_partial_line_at_eof() {
        let mut s = LineSplitter::new();
        s.push(&vec![b'z'; MAX_LINE_BYTES + 1]);
        let last = s.finish().unwrap();
        assert!(last.ends_with(" [truncated 1 bytes]"));
    }

    #[tokio::test]
    async fn lag_is_counted() {
        let (tx, rx) = broadcast::channel(4);
        let total = Arc::new(AtomicU64::new(0));
        let mut out = OutputStream::new(rx, total.clone());
        for i in 0..10 {
            tx.send(OutputLine {
                ts: SystemTime::now(),
                stream: OutputStreamKind::Out,
                text: i.to_string(),
            })
            .unwrap();
        }
        drop(tx);
        let mut got = Vec::new();
        while let Some(l) = out.recv().await {
            got.push(l.text);
        }
        assert_eq!(got, vec!["6", "7", "8", "9"]);
        assert_eq!(out.dropped(), 6);
        assert_eq!(total.load(Ordering::Relaxed), 6);
    }

    #[tokio::test]
    async fn pump_reads_to_eof() {
        let (tx, mut rx) = broadcast::channel(16);
        let data: &[u8] = b"a\r\nb\nc";
        pump(data, OutputStreamKind::Err, tx).await;
        let mut got = Vec::new();
        while let Ok(l) = rx.recv().await {
            assert_eq!(l.stream, OutputStreamKind::Err);
            got.push(l.text);
        }
        assert_eq!(got, vec!["a", "b", "c"]);
    }
}
