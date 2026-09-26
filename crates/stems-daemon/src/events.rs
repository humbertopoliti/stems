//! The daemon's event bus: a bounded ring (for `events` / `--since` replay)
//! plus a broadcast channel for live subscribers.

use std::collections::VecDeque;
use std::sync::Mutex;

use chrono::Utc;
use serde_json::Value;
use stems_api::{Event, EventKind};
use tokio::sync::broadcast;

/// Events kept for replay.
pub const RING_CAPACITY: usize = 10_000;
/// Live-channel capacity per subscriber before it lags (and re-reads the ring).
pub const LIVE_CAPACITY: usize = 1024;

/// An event before the bus assigns `seq` and `ts`.
#[derive(Clone, Debug, PartialEq)]
pub struct EventDraft {
    /// Kind.
    pub kind: EventKind,
    /// Stem concerned.
    pub stem: Option<String>,
    /// Previous state.
    pub from: Option<String>,
    /// New state.
    pub to: Option<String>,
    /// Why.
    pub reason: Option<String>,
    /// Who.
    pub actor: String,
    /// Payload.
    pub data: Value,
}

impl EventDraft {
    /// A draft with no stem/transition and `data: {}`.
    pub fn new(kind: EventKind, actor: impl Into<String>) -> Self {
        Self {
            kind,
            stem: None,
            from: None,
            to: None,
            reason: None,
            actor: actor.into(),
            data: Value::Object(Default::default()),
        }
    }

    /// Set the stem.
    #[must_use]
    pub fn stem(mut self, stem: impl Into<String>) -> Self {
        self.stem = Some(stem.into());
        self
    }

    /// Set `from` -> `to`.
    #[must_use]
    pub fn transition(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.from = Some(from.into());
        self.to = Some(to.into());
        self
    }

    /// Set the reason.
    #[must_use]
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// Set the payload.
    #[must_use]
    pub fn data(mut self, data: Value) -> Self {
        self.data = data;
        self
    }
}

struct Ring {
    buf: VecDeque<Event>,
    capacity: usize,
    last_seq: u64,
}

/// Ring buffer + broadcast. Cheap to share behind an `Arc`.
pub struct EventBus {
    ring: Mutex<Ring>,
    tx: broadcast::Sender<Event>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(RING_CAPACITY)
    }
}

impl EventBus {
    /// A bus keeping the last `capacity` events.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(LIVE_CAPACITY);
        Self {
            ring: Mutex::new(Ring {
                buf: VecDeque::with_capacity(capacity.min(1024)),
                capacity: capacity.max(1),
                last_seq: 0,
            }),
            tx,
        }
    }

    fn ring(&self) -> std::sync::MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Assign the next seq and `ts = now`, buffer and broadcast.
    pub fn emit(&self, d: EventDraft) -> Event {
        let mut ring = self.ring();
        ring.last_seq += 1;
        let ev = Event {
            ts: Utc::now(),
            seq: ring.last_seq,
            kind: d.kind,
            stem: d.stem,
            from: d.from,
            to: d.to,
            reason: d.reason,
            actor: d.actor,
            data: d.data,
        };
        if ring.buf.len() == ring.capacity {
            ring.buf.pop_front();
        }
        ring.buf.push_back(ev.clone());
        // Sent under the ring lock so `subscribe_from` sees no gap and no duplicate.
        let _ = self.tx.send(ev.clone());
        ev
    }

    /// Buffered events with `seq > since_seq`, oldest first.
    pub fn replay(&self, since_seq: u64) -> Vec<Event> {
        let ring = self.ring();
        ring.buf
            .iter()
            .filter(|e| e.seq > since_seq)
            .cloned()
            .collect()
    }

    /// Highest seq emitted so far (0 before the first event).
    pub fn last_seq(&self) -> u64 {
        self.ring().last_seq
    }

    /// Live receiver (new events only).
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// Atomically: the buffered events after `since_seq` (none if `None`) and
    /// a live receiver that starts right after them.
    pub fn subscribe_from(
        &self,
        since_seq: Option<u64>,
    ) -> (Vec<Event>, broadcast::Receiver<Event>) {
        let ring = self.ring();
        let backlog = match since_seq {
            Some(s) => ring.buf.iter().filter(|e| e.seq > s).cloned().collect(),
            None => Vec::new(),
        };
        (backlog, self.tx.subscribe())
    }

    /// Open live receivers.
    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn seq_is_monotonic_from_one() {
        let bus = EventBus::new(10);
        assert_eq!(bus.last_seq(), 0);
        let a = bus.emit(EventDraft::new(EventKind::DAEMON_STARTED, "daemon"));
        let b = bus.emit(
            EventDraft::new(EventKind::STEM_STATE, "cli:x")
                .stem("api")
                .transition("stopped", "starting"),
        );
        assert_eq!((a.seq, b.seq), (1, 2));
        assert!(b.ts >= a.ts);
        assert_eq!(b.from.as_deref(), Some("stopped"));
        assert_eq!(a.data, json!({}));
    }

    #[test]
    fn replay_since() {
        let bus = EventBus::new(100);
        for _ in 0..5 {
            bus.emit(EventDraft::new(EventKind::PROCESS_OUTPUT, "daemon"));
        }
        assert_eq!(bus.replay(0).len(), 5);
        let r: Vec<u64> = bus.replay(3).iter().map(|e| e.seq).collect();
        assert_eq!(r, vec![4, 5]);
        assert!(bus.replay(5).is_empty());
        assert!(bus.replay(99).is_empty());
    }

    #[test]
    fn ring_drops_oldest() {
        let bus = EventBus::new(3);
        for _ in 0..10 {
            bus.emit(EventDraft::new(EventKind::PROCESS_OUTPUT, "daemon"));
        }
        let r: Vec<u64> = bus.replay(0).iter().map(|e| e.seq).collect();
        assert_eq!(r, vec![8, 9, 10]);
        assert_eq!(bus.last_seq(), 10);
    }

    #[test]
    fn default_ring_holds_10k() {
        let bus = EventBus::default();
        for _ in 0..(RING_CAPACITY + 5) {
            bus.emit(EventDraft::new(EventKind::PROCESS_OUTPUT, "daemon"));
        }
        let r = bus.replay(0);
        assert_eq!(r.len(), RING_CAPACITY);
        assert_eq!(r[0].seq, 6);
    }

    #[tokio::test]
    async fn subscribe_from_has_no_gap_or_duplicate() {
        let bus = EventBus::new(100);
        bus.emit(EventDraft::new(EventKind::DAEMON_STARTED, "daemon"));
        bus.emit(EventDraft::new(EventKind::WORKSPACE_LOADED, "daemon"));
        let (backlog, mut rx) = bus.subscribe_from(Some(0));
        assert_eq!(
            backlog.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
        bus.emit(EventDraft::new(EventKind::DAEMON_STOPPING, "daemon"));
        assert_eq!(rx.recv().await.unwrap().seq, 3);
        let (none, _rx2) = bus.subscribe_from(None);
        assert!(none.is_empty());
        assert_eq!(bus.subscriber_count(), 2);
    }
}
