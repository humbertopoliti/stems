//! Batcher (fake clock), message goldens, argv builders, backend selection.

use std::time::{Duration, Instant};

use super::backend::{
    Backend, find_on_path, notify_send_args, osascript_args, terminal_notifier_args,
};
use super::*;

const D: Duration = Duration::from_secs(2);
const C: Duration = Duration::from_secs(30);

fn f(stem: &str, code: &str) -> Failure {
    Failure {
        stem: stem.into(),
        code: code.into(),
        message: format!(
            "`{stem}` was restarted 5 times within 10m and failed again: giving up (restart.max = 5)"
        ),
        hint: Some(format!(
            "check `stems logs {stem}`, fix the crash and run `stems restart {stem}`"
        )),
    }
}

fn ms(t0: Instant, n: u64) -> Instant {
    t0 + Duration::from_millis(n)
}

// ---------------------------------------------------------------------------
// Batcher
// ---------------------------------------------------------------------------

#[test]
fn debounce_waits_for_quiet_time() {
    let t0 = Instant::now();
    let mut b = Batcher::new(D, C);
    assert_eq!(b.deadline(), None);
    b.fail(t0, "ws", f("a", "MAX_RESTARTS"));
    assert_eq!(b.deadline(), Some(ms(t0, 2000)));
    // A second failure 1 s later moves the deadline.
    b.fail(ms(t0, 1000), "ws", f("b", "START_TIMEOUT"));
    assert_eq!(b.deadline(), Some(ms(t0, 3000)));
    assert!(b.take_due(ms(t0, 2999)).is_empty());
    let got = b.take_due(ms(t0, 3000));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].workspace, "ws");
    assert_eq!(got[0].failures.len(), 2);
    assert_eq!(got[0].folded, 0);
    assert_eq!(b.deadline(), None);
    assert_eq!(b.pending(), 0);
}

#[test]
fn debounce_is_capped_at_five_times() {
    let t0 = Instant::now();
    let mut b = Batcher::new(D, C);
    // A failure every second: the deadline never stops moving...
    for i in 0..20 {
        b.fail(ms(t0, i * 1000), "ws", f(&format!("s{i}"), "EXITED"));
    }
    // ...but the cap (5 × 2 s after the first) holds.
    assert_eq!(b.deadline(), Some(ms(t0, 10_000)));
    let got = b.take_due(ms(t0, 10_000));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].failures.len(), 20);
}

#[test]
fn early_flush_makes_the_batch_due_now() {
    let t0 = Instant::now();
    let mut b = Batcher::new(Duration::from_secs(30), C);
    b.fail(t0, "ws", f("a", "START_FAILED"));
    assert_eq!(b.deadline(), Some(t0 + Duration::from_secs(30)));
    b.flush_now();
    assert_eq!(b.deadline(), Some(t0));
    assert_eq!(b.take_due(t0).len(), 1);
    // Nothing open: a flush is a no-op.
    b.flush_now();
    assert_eq!(b.deadline(), None);
}

#[test]
fn cooldown_holds_failures_for_the_next_batch_and_counts_them() {
    let t0 = Instant::now();
    let mut b = Batcher::new(Duration::from_millis(200), Duration::from_secs(3));
    b.fail(t0, "ws", f("api", "EXITED"));
    let first = b.take_due(ms(t0, 200));
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].folded, 0);
    // 1 s later (inside the 3 s cooldown that started at t0+200ms).
    b.fail(ms(t0, 1200), "ws", f("api", "EXITED"));
    assert_eq!(
        b.deadline(),
        Some(ms(t0, 3200)),
        "held until the cooldown ends"
    );
    // An early flush does not break the cooldown either.
    b.flush_now();
    assert_eq!(b.deadline(), Some(ms(t0, 3200)));
    assert!(b.take_due(ms(t0, 3199)).is_empty());
    let second = b.take_due(ms(t0, 3200));
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].folded, 1);
    assert_eq!(second[0].failures.len(), 1);
    // A failure after the cooldown is not folded.
    b.fail(ms(t0, 10_000), "ws", f("api", "EXITED"));
    let third = b.take_due(ms(t0, 10_200));
    assert_eq!(third[0].folded, 0);
}

#[test]
fn a_storm_inside_the_cooldown_is_one_batch() {
    let t0 = Instant::now();
    let mut b = Batcher::new(D, C);
    b.fail(t0, "ws", f("a", "EXITED"));
    assert_eq!(b.take_due(ms(t0, 2000)).len(), 1);
    for i in 0..20 {
        b.fail(ms(t0, 3000 + i * 100), "ws", f(&format!("s{i}"), "EXITED"));
    }
    assert!(b.take_due(ms(t0, 31_999)).is_empty());
    let got = b.take_due(ms(t0, 32_000));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].failures.len(), 20);
    assert_eq!(got[0].folded, 20);
}

#[test]
fn workspaces_are_independent() {
    let t0 = Instant::now();
    let mut b = Batcher::new(D, C);
    b.fail(t0, "one", f("a", "EXITED"));
    assert_eq!(b.take_due(ms(t0, 2000)).len(), 1);
    // `two` is not in `one`'s cooldown.
    b.fail(ms(t0, 2500), "two", f("b", "EXITED"));
    b.fail(ms(t0, 2500), "one", f("c", "EXITED"));
    let got = b.take_due(ms(t0, 4500));
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].workspace, "two");
    assert_eq!(got[0].folded, 0);
    assert_eq!(b.deadline(), Some(ms(t0, 32_000)));
}

#[test]
fn configure_applies_to_the_next_batch() {
    let t0 = Instant::now();
    let mut b = Batcher::new(D, C);
    b.configure(Duration::from_millis(500), Duration::from_secs(1));
    b.fail(t0, "ws", f("a", "EXITED"));
    assert_eq!(b.deadline(), Some(ms(t0, 500)));
    assert_eq!(b.take_due(ms(t0, 500)).len(), 1);
    b.fail(ms(t0, 600), "ws", f("a", "EXITED"));
    assert_eq!(
        b.deadline(),
        Some(ms(t0, 1500)),
        "cooldown 1 s from the flush"
    );
}

// ---------------------------------------------------------------------------
// Failure::from_event
// ---------------------------------------------------------------------------

fn event(kind: EventKind, from: &str, to: &str, data: Value) -> Event {
    Event {
        ts: chrono::Utc::now(),
        seq: 1,
        kind,
        stem: Some("api".into()),
        from: Some(from.into()),
        to: Some(to.into()),
        reason: Some("exited with code 3".into()),
        actor: "daemon".into(),
        data,
    }
}

#[test]
fn failure_from_event_reads_the_error_or_the_reason() {
    let e = event(
        EventKind::STEM_STATE,
        "healthy",
        "failed",
        json!({"error": {"code": "MAX_RESTARTS", "message": "m", "hint": "h"}}),
    );
    let f = Failure::from_event(&e).unwrap();
    assert_eq!(
        (f.stem.as_str(), f.code.as_str(), f.message.as_str()),
        ("api", "MAX_RESTARTS", "m")
    );
    assert_eq!(f.hint.as_deref(), Some("h"));
    let e = event(
        EventKind::STEM_STATE,
        "healthy",
        "failed",
        json!({"exit_code": 3}),
    );
    let f = Failure::from_event(&e).unwrap();
    assert_eq!(f.code, EXITED_CODE);
    assert_eq!(f.message, "exited with code 3");
    assert_eq!(f.hint, None);
    assert!(
        Failure::from_event(&event(
            EventKind::STEM_STATE,
            "healthy",
            "stopped",
            json!({})
        ))
        .is_none()
    );
    assert!(
        Failure::from_event(&event(
            EventKind::STEM_HEALTH,
            "healthy",
            "failed",
            json!({})
        ))
        .is_none()
    );
}

#[test]
fn env_kill_switch_values() {
    use std::ffi::OsStr;
    for v in ["0", "off", "false", "FALSE", " Off ", "no"] {
        assert!(env_disables(Some(OsStr::new(v))), "{v:?}");
    }
    for v in ["1", "on", "true", "", "yes"] {
        assert!(!env_disables(Some(OsStr::new(v))), "{v:?}");
    }
    assert!(!env_disables(None));
}

#[test]
fn replace_id_is_stable_and_short() {
    let a = replace_id(std::path::Path::new("/tmp/ws"));
    assert_eq!(a, replace_id(std::path::Path::new("/tmp/ws")));
    assert_ne!(a, replace_id(std::path::Path::new("/tmp/other")));
    assert!(a.starts_with("stems:") && a.len() == 14, "{a}");
}

// ---------------------------------------------------------------------------
// Rendering goldens
// ---------------------------------------------------------------------------

fn batch(failures: Vec<Failure>, folded: usize) -> Batch {
    Batch {
        workspace: "hello-shop".into(),
        failures,
        folded,
    }
}

fn show(n: &Notification) -> String {
    format!(
        "title:    {}\nbody:     {}\nsubtitle: {}\nstems:    {}\ncodes:    {}\nfolded:   {}\n",
        n.title,
        n.body,
        n.subtitle.as_deref().unwrap_or("-"),
        n.stems.join(","),
        n.codes.join(","),
        n.folded
    )
}

#[test]
fn render_one_stem() {
    let n = render(&batch(vec![f("shop-api", "MAX_RESTARTS")], 0));
    insta::assert_snapshot!(show(&n), @r"
    title:    stems · hello-shop
    body:     shop-api failed: was restarted 5 times within 10m and failed again: giving up (restart.max = 5)
    subtitle: check `stems logs shop-api`, fix the crash and run `stems restart shop-api`
    stems:    shop-api
    codes:    MAX_RESTARTS
    folded:   0
    ");
}

#[test]
fn render_two_and_three_stems_with_tally() {
    let n = render(&batch(
        vec![f("shop-api", "MAX_RESTARTS"), f("worker", "START_TIMEOUT")],
        0,
    ));
    insta::assert_snapshot!(show(&n), @r"
    title:    stems · hello-shop
    body:     shop-api, worker failed
    subtitle: 1 × MAX_RESTARTS, 1 × START_TIMEOUT
    stems:    shop-api,worker
    codes:    MAX_RESTARTS,START_TIMEOUT
    folded:   0
    ");
    let n = render(&batch(
        vec![
            f("shop-api", "MAX_RESTARTS"),
            f("worker", "MAX_RESTARTS"),
            f("redis", "START_TIMEOUT"),
        ],
        0,
    ));
    insta::assert_snapshot!(show(&n), @r"
    title:    stems · hello-shop
    body:     shop-api, worker, redis failed
    subtitle: 2 × MAX_RESTARTS, 1 × START_TIMEOUT
    stems:    shop-api,worker,redis
    codes:    MAX_RESTARTS,START_TIMEOUT
    folded:   0
    ");
}

#[test]
fn render_twelve_stems_lists_three_and_counts_the_rest() {
    let fails: Vec<Failure> = (1..=12)
        .map(|i| {
            f(
                &format!("s{i}"),
                if i % 2 == 0 { "EXITED" } else { "START_FAILED" },
            )
        })
        .collect();
    let n = render(&batch(fails, 0));
    insta::assert_snapshot!(show(&n), @r"
    title:    stems · hello-shop
    body:     12 stems failed: s1, s2, s3, +9 more
    subtitle: 6 × START_FAILED, 6 × EXITED
    stems:    s1,s2,s3,s4,s5,s6,s7,s8,s9,s10,s11,s12
    codes:    START_FAILED,EXITED
    folded:   0
    ");
}

#[test]
fn render_truncates_long_messages_and_keeps_folded_out_of_the_body() {
    let long = Failure {
        stem: "api".into(),
        code: "SETUP_FAILED".into(),
        message: format!("api setup script failed: {}", "x".repeat(300)),
        hint: None,
    };
    let n = render(&batch(vec![long], 2));
    assert_eq!(n.body.chars().count(), "api failed: ".len() + MESSAGE_LIMIT);
    assert!(n.body.ends_with('…'), "{}", n.body);
    assert_eq!(n.folded, 2);
    assert_eq!(n.subtitle, None);
    // The same stem failing twice is listed once, described by its latest failure.
    let n = render(&batch(
        vec![
            Failure {
                stem: "api".into(),
                code: "EXITED".into(),
                message: "exited with code 3".into(),
                hint: None,
            },
            Failure {
                stem: "api".into(),
                code: "EXITED".into(),
                message: "exited with code 4".into(),
                hint: None,
            },
        ],
        1,
    ));
    insta::assert_snapshot!(show(&n), @r"
    title:    stems · hello-shop
    body:     api failed: exited with code 4
    subtitle: -
    stems:    api
    codes:    EXITED
    folded:   1
    ");
}

#[test]
fn render_never_includes_commands_or_env() {
    // Only the error message, cut short; details (command lines, env) never
    // reach the body.
    let n = render(&batch(
        vec![Failure {
            stem: "api".into(),
            code: "START_FAILED".into(),
            message: "`api` exited with code 1 before it became ready".into(),
            hint: Some("check its output (`stems logs api`)".into()),
        }],
        0,
    ));
    assert_eq!(
        n.body,
        "api failed: exited with code 1 before it became ready"
    );
    assert_eq!(
        n.subtitle.as_deref(),
        Some("check its output (`stems logs api`)")
    );
}

// ---------------------------------------------------------------------------
// argv builders
// ---------------------------------------------------------------------------

fn tricky() -> Notification {
    Notification {
        title: "stems · ws".into(),
        body: "api failed: said \"quote\" and \\ backslash\nsecond line".into(),
        subtitle: Some("it's $HOME `cmd`".into()),
        stems: vec!["api".into()],
        codes: vec!["EXITED".into()],
        workspace: "ws".into(),
        folded: 0,
    }
}

#[test]
fn osascript_takes_strings_as_argv() {
    let n = tricky();
    let args = osascript_args(&n);
    let s: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        s,
        vec![
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv) subtitle (item 3 of argv)",
            "-e",
            "end run",
            "stems · ws",
            "api failed: said \"quote\" and \\ backslash\nsecond line",
            "it's $HOME `cmd`",
        ]
    );
    // The strings pass through untouched: no script text contains them.
    for script in s.iter().filter(|a| a.contains("display")) {
        assert!(!script.contains("quote"));
    }
    let mut no_sub = n.clone();
    no_sub.subtitle = None;
    let args = osascript_args(&no_sub);
    assert_eq!(args.len(), 8);
    assert_eq!(
        args[3],
        "display notification (item 2 of argv) with title (item 1 of argv)"
    );
}

#[test]
fn terminal_notifier_and_notify_send_arg_lists() {
    let n = tricky();
    let s: Vec<String> = terminal_notifier_args(&n, "stems:abcd1234")
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        s,
        vec![
            "-title",
            "stems · ws",
            "-message",
            "api failed: said \"quote\" and \\ backslash\nsecond line",
            "-subtitle",
            "it's $HOME `cmd`",
            "-group",
            "stems:abcd1234",
        ]
    );
    let s: Vec<String> = notify_send_args(&n, None, true)
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        s,
        vec![
            "--app-name",
            "stems",
            "-p",
            "--",
            "stems · ws",
            "api failed: said \"quote\" and \\ backslash\nsecond line\nit's $HOME `cmd`",
        ]
    );
    let s: Vec<String> = notify_send_args(&n, Some(42), true)
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(&s[..5], ["--app-name", "stems", "-r", "42", "-p"]);
    let s: Vec<String> = notify_send_args(&n, None, false)
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(&s[..3], ["--app-name", "stems", "--"]);
}

// ---------------------------------------------------------------------------
// Backend selection with a fake PATH
// ---------------------------------------------------------------------------

fn fake_bin(dir: &std::path::Path, names: &[&str]) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for n in names {
        let p = bin.join(n);
        std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

#[test]
fn backend_selection_table() {
    let d = tempfile::tempdir().unwrap();
    let all = fake_bin(d.path(), &["terminal-notifier", "osascript", "notify-send"]);
    let path = std::env::join_paths([all.clone()]).unwrap();
    let p = Some(path.as_os_str());
    assert_eq!(
        Backend::detect(p, "macos"),
        Some(Backend::TerminalNotifier(all.join("terminal-notifier")))
    );
    assert_eq!(
        Backend::detect(p, "linux"),
        Some(Backend::NotifySend(all.join("notify-send")))
    );
    assert_eq!(Backend::detect(p, "freebsd"), None);
    assert_eq!(Backend::detect(None, "macos"), None);

    let d2 = tempfile::tempdir().unwrap();
    let only_osa = fake_bin(d2.path(), &["osascript"]);
    let path = std::env::join_paths([only_osa.clone()]).unwrap();
    let b = Backend::detect(Some(path.as_os_str()), "macos").unwrap();
    assert_eq!(b, Backend::Osascript(only_osa.join("osascript")));
    assert_eq!(b.name(), "osascript");
    assert_eq!(Backend::detect(Some(path.as_os_str()), "linux"), None);

    // Not executable: not found. Empty PATH: nothing.
    let d3 = tempfile::tempdir().unwrap();
    let bin = fake_bin(d3.path(), &[]);
    std::fs::write(bin.join("notify-send"), "").unwrap();
    let path = std::env::join_paths([bin]).unwrap();
    assert_eq!(find_on_path(Some(path.as_os_str()), "notify-send"), None);
    assert_eq!(find_on_path(Some(std::ffi::OsStr::new("")), "sh"), None);
}

// ---------------------------------------------------------------------------
// The command notifier end to end (a shell script)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn command_notifier_gets_env_and_json_on_stdin() {
    let d = tempfile::tempdir().unwrap();
    let log = d.path().join("notify.log");
    let cmd = format!(
        "printf '%s|%s|%s|' \"$STEMS_NOTIFY_TITLE\" \"$STEMS_NOTIFY_STEMS\" \"$STEMS_NOTIFY_WORKSPACE\" >> {log}; cat >> {log}",
        log = log.display()
    );
    let notifier = CommandNotifier::new(&cmd, d.path());
    let mut st = ReplaceState::default();
    let n = render(&batch(
        vec![f("api", "MAX_RESTARTS"), f("web", "START_TIMEOUT")],
        0,
    ));
    let sent = notifier.send(&n, &mut st).await.unwrap();
    assert_eq!(sent.backend, "command");
    assert!(!sent.replaced);
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        text.starts_with("stems · hello-shop|api,web|hello-shop|{"),
        "{text}"
    );
    let json: Value = serde_json::from_str(
        text.split_once('|')
            .map(|_| &text[text.find('{').unwrap()..])
            .unwrap(),
    )
    .unwrap();
    assert_eq!(json["stems"], json!(["api", "web"]));
    assert_eq!(json["codes"], json!(["MAX_RESTARTS", "START_TIMEOUT"]));
    assert_eq!(json["folded"], 0);

    // A failing command is an error naming the status and its stderr.
    let bad = CommandNotifier::new("echo boom >&2; exit 3", d.path());
    let e = bad.send(&n, &mut st).await.unwrap_err();
    assert!(e.contains("exit status: 3") && e.contains("boom"), "{e}");
}
