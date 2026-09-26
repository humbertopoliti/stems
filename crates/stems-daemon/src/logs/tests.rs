use std::time::Duration;

use stems_config::ByteSize;

use super::*;

fn hub(dir: &Path) -> Arc<LogHub> {
    Arc::new(LogHub::new(dir.join(LOGS_DIR)))
}

fn all(hub: &LogHub, stems: &[&str]) -> Vec<LogRecord> {
    let q = LogQuery {
        stems: stems.iter().map(|s| s.to_string()).collect(),
        ..LogQuery::default()
    };
    hub.query(&q, QueryOptions::default()).records
}

async fn wait_for(hub: &LogHub, stem: &str, n: usize) -> Vec<LogRecord> {
    for _ in 0..200 {
        let r = all(hub, &[stem]);
        if r.len() >= n {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{stem} never reached {n} records: {:?}", all(hub, &[stem]));
}

#[tokio::test]
async fn script_writer_tags_lines_and_writes_the_file() {
    let d = tempfile::tempdir().unwrap();
    let hub = hub(d.path());
    let w = hub.script_writer("api", "seed");
    assert_eq!((w.stem(), w.script()), ("api", "seed"));
    assert!(w.line("seeding 3 users").await);
    assert!(w.try_line(r#"{"level":"error","msg":"boom","table":"users"}"#));
    let recs = wait_for(&hub, "api", 2).await;
    assert_eq!(recs[0].stream, Stream::Script);
    assert_eq!(recs[0].tag.as_deref(), Some("seed"));
    assert_eq!(recs[0].text, "seeding 3 users");
    assert_eq!(recs[1].level, Some(Level::Error));
    assert_eq!(recs[1].text, "boom");
    assert_eq!(recs[1].fields.as_ref().unwrap()["table"], "users");
    // --script filter
    let q = LogQuery {
        script: Some("seed".into()),
        ..LogQuery::default()
    };
    assert_eq!(hub.query(&q, QueryOptions::default()).records.len(), 2);
    let q = LogQuery {
        script: Some("migrate".into()),
        ..LogQuery::default()
    };
    assert!(hub.query(&q, QueryOptions::default()).records.is_empty());
    // On disk: one JSON record per line, identical to the ring.
    hub.flush_all().await;
    let mut from_file = Vec::new();
    read_records(&hub.stem_dir("api").join("current.log"), &mut |r| {
        from_file.push(r)
    });
    assert_eq!(from_file, recs);
}

#[tokio::test]
async fn files_are_flushed_when_idle_without_clients() {
    let d = tempfile::tempdir().unwrap();
    let hub = hub(d.path());
    let w = hub.script_writer("api", "x");
    w.line("hello").await;
    let path = hub.stem_dir("api").join("current.log");
    for _ in 0..100 {
        if std::fs::read_to_string(&path).is_ok_and(|s| s.contains("hello")) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("current.log was not flushed within 2 s");
}

#[tokio::test]
async fn rotation_keeps_at_most_keep_plus_one_files() {
    let d = tempfile::tempdir().unwrap();
    let hub = hub(d.path());
    hub.configure(LogSettings {
        ring: 100,
        rotation: RotationPolicy {
            max_size: ByteSize(2_000),
            keep: 2,
        },
    });
    let w = hub.script_writer("api", "spew");
    for i in 0..200 {
        w.line(format!("line {i:04} {}", "x".repeat(40))).await;
    }
    wait_for(&hub, "api", 100).await;
    hub.flush_all().await;
    let files = list_log_files(&hub.stem_dir("api"));
    let names: Vec<_> = files
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["current.2.log", "current.1.log", "current.log"]);
    for f in &files {
        assert!(
            std::fs::metadata(f).unwrap().len() <= 2_000,
            "{f:?} too big"
        );
    }
    let mut last = None;
    read_records(&files[2], &mut |r| last = Some(r.text));
    assert!(last.unwrap().starts_with("line 0199"));
}

#[tokio::test]
async fn query_reads_files_for_history_older_than_the_ring() {
    let d = tempfile::tempdir().unwrap();
    let hub = hub(d.path());
    hub.configure(LogSettings {
        ring: 5,
        ..LogSettings::default()
    });
    let w = hub.script_writer("api", "s");
    for i in 0..20 {
        w.line(format!("n{i}")).await;
    }
    let recs = wait_for(&hub, "api", 20).await;
    assert_eq!(recs.len(), 20, "5 from the ring, 15 from the file");
    assert_eq!(recs[0].text, "n0");
    assert_eq!(recs[19].text, "n19");
    // tail keeps the newest
    let q = LogQuery {
        tail: Some(3),
        ..LogQuery::default()
    };
    let t: Vec<_> = hub
        .query(&q, QueryOptions::default())
        .records
        .into_iter()
        .map(|r| r.text)
        .collect();
    assert_eq!(t, ["n17", "n18", "n19"]);
    // cap
    let out = hub.query(
        &LogQuery::default(),
        QueryOptions {
            from_files: false,
            cap: 4,
        },
    );
    assert!(out.truncated);
    assert_eq!(out.records.len(), 4);
    assert_eq!(out.records[3].text, "n19");
}

#[tokio::test]
async fn a_new_daemon_run_sees_previous_files_and_notes_adoption() {
    let d = tempfile::tempdir().unwrap();
    {
        let old = hub(d.path());
        let w = old.script_writer("api", "s");
        w.line("before crash").await;
        wait_for(&old, "api", 1).await;
        old.flush_all().await;
    }
    let new = hub(d.path());
    // No sink yet: everything comes from the files.
    assert_eq!(all(&new, &[])[0].text, "before crash");
    new.note_adopted("api");
    new.note_adopted("api"); // idempotent
    let recs = wait_for(&new, "api", 2).await;
    assert_eq!(recs.len(), 2);
    assert_eq!(recs[0].text, "before crash");
    assert_eq!(recs[1].text, ADOPTED_TEXT);
    assert_eq!(recs[1].stream, Stream::Err);
}

#[tokio::test]
async fn adoption_events_are_noted() {
    let d = tempfile::tempdir().unwrap();
    let hub = hub(d.path());
    let bus = EventBus::default();
    hub.watch_events(&bus);
    bus.emit(crate::events::EventDraft::new(EventKind::STEM_ADOPTED, "daemon").stem("web"));
    let recs = wait_for(&hub, "web", 1).await;
    assert_eq!(recs[0].text, ADOPTED_TEXT);
}

#[tokio::test]
async fn live_subscribers_get_records_with_increasing_seq() {
    let d = tempfile::tempdir().unwrap();
    let hub = hub(d.path());
    let mut rx = hub.subscribe();
    let w = hub.script_writer("api", "s");
    w.line("a").await;
    w.line("b").await;
    let a = rx.recv().await.unwrap();
    let b = rx.recv().await.unwrap();
    assert_eq!((a.record.text.as_str(), b.record.text.as_str()), ("a", "b"));
    assert!(b.seq > a.seq);
    let out = hub.query(&LogQuery::default(), QueryOptions::default());
    assert_eq!(out.next_seq["api"], b.seq + 1);
}

#[test]
fn to_query_validates() {
    let now = Utc::now();
    let f = LogFilter {
        since: Some("10m".into()),
        level: Some("warn+".into()),
        grep: Some("^GET".into()),
        ..LogFilter::default()
    };
    let q = to_query(&f, now).unwrap();
    assert_eq!(q.since, Some(now - chrono::Duration::minutes(10)));
    assert!(q.level.unwrap().and_above);
    for bad in [
        LogFilter {
            since: Some("yesterday".into()),
            ..LogFilter::default()
        },
        LogFilter {
            grep: Some("(".into()),
            ..LogFilter::default()
        },
        LogFilter {
            level: Some("loud".into()),
            ..LogFilter::default()
        },
    ] {
        assert_eq!(to_query(&bad, now).unwrap_err().code, ErrorCode::Usage);
    }
}

#[test]
fn log_files_sort_oldest_first() {
    let d = tempfile::tempdir().unwrap();
    for n in [
        "current.log",
        "current.1.log",
        "current.10.log",
        "current.2.log",
        "x.log",
        "current.0.log",
    ] {
        std::fs::write(d.path().join(n), "").unwrap();
    }
    let names: Vec<_> = list_log_files(d.path())
        .iter()
        .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        [
            "current.10.log",
            "current.2.log",
            "current.1.log",
            "current.log"
        ]
    );
}
