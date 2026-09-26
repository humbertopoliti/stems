//! The step library. See `tests/stems-e2e/STEPS.md` for the vocabulary.
//!
//! Steps are registered explicitly (no `inventory` magic) so the library
//! works from a separate crate. Every step is registered for Given, When and
//! Then, so `And`/`But` work after any keyword; keep the Given/When/Then
//! convention in feature files anyway. Regexes are anchored.
//!
//! To add a step: write a `step!` function and add a line to [`STEPS`], then
//! document it in `STEPS.md`. Extend, do not rename (04 "Interfaces").

use std::path::PathBuf;
use std::time::{Duration, Instant};

use cucumber::step::{Collection, Context, Step};
use futures::future::LocalBoxFuture;
use serde_json::Value;

use crate::world::{
    self, E2eWorld, find_files, find_stem, is_socket_or_lock, stem_port, stem_state,
};
use crate::{docker, golden, hooks, http, procs, util};

/// Poll interval for `within Ns ...` steps.
pub const POLL: Duration = Duration::from_millis(100);

macro_rules! step {
    ($name:ident($w:ident, $m:ident) $body:block) => {
        fn $name<'a>($w: &'a mut E2eWorld, ctx: Context) -> LocalBoxFuture<'a, ()> {
            #[allow(unused_variables)]
            let $m: Vec<String> = ctx.matches.into_iter().map(|(_, s)| s).collect();
            Box::pin(async move {
                let _ = $w.remaining();
                $body
            })
        }
    };
}

// ----------------------------------------------------------------------------
// Given: workspaces
// ----------------------------------------------------------------------------

step!(given_workspace(w, m) {
    w.use_workspace(&m[1], true);
});

step!(given_broken_workspace(w, m) {
    w.use_workspace(&format!("broken/{}", m[1]), true);
});

step!(given_fixture_workspace(w, m) {
    w.use_fixture_workspace(&m[1], true);
});

step!(given_fixture_workspace_original_ports(w, m) {
    w.use_fixture_workspace(&m[1], false);
});

step!(given_workspace_original_ports(w, m) {
    w.use_workspace(&m[1], false);
});

step!(given_workspace_with_override(w, m) {
    w.use_workspace(&m[1], true);
    w.set_override(&m[2], &m[3]);
});

step!(given_local_override(w, m) {
    w.set_override(&m[1], &m[2]);
});

step!(given_local_override_removed(w, m) {
    w.remove_override(&m[1]);
});

/// `Given the local override file is extended with:` + docstring (33): the
/// YAML mapping is deep-merged into the generated `stems.local.yaml` (port
/// remapping kept), which is rewritten.
fn given_local_override_extended<'a>(w: &'a mut E2eWorld, ctx: Context) -> LocalBoxFuture<'a, ()> {
    let text = ctx.step.docstring.clone().unwrap_or_default();
    Box::pin(async move {
        let _ = w.remaining();
        let text = text.strip_prefix('\n').unwrap_or(&text);
        let cut = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        let body: String = text
            .lines()
            .map(|l| format!("{}\n", l.get(cut..).unwrap_or("").trim_end()))
            .collect();
        w.merge_override(&body);
    })
}

step!(given_empty_dir(w, m) {
    w.use_empty_dir();
});

step!(given_private_repos(w, m) {
    w.private_repos();
});

step!(given_workspace_up(w, m) {
    w.use_workspace(&m[1], true);
    w.daemon_started = true;
    let mut line = String::from("stems up --detach --json");
    if !m[3].is_empty() {
        line.push_str(&format!(" --profile {}", m[3]));
    }
    let out = w.run(&line, None, &[]).await;
    out.guard_implemented("08");
    assert!(out.code == 0, "`{line}` failed\n{}", out.describe());
});

step!(given_stray_process(w, m) {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c")
        .arg(w.expand(&m[1]))
        .current_dir(w.default_cwd())
        .env("STEMS_HOME", &w.home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let child = cmd.spawn().unwrap_or_else(|e| panic!("spawning stray {:?}: {e}", m[1]));
    let pgid = child.id().and_then(|p| i32::try_from(p).ok()).expect("stray has a pid");
    w.pgids.insert(pgid);
    w.strays.push(child);
});

step!(then_strays_running(w, m) {
    assert!(!w.strays.is_empty(), "no stray process was started in this scenario");
    for child in &mut w.strays {
        let pid = child.id().and_then(|p| i32::try_from(p).ok()).unwrap_or(0);
        let exited = child.try_wait().ok().flatten();
        assert!(
            exited.is_none() && procs::pid_alive(pid),
            "stray process {pid} is not running any more ({exited:?})"
        );
    }
});

step!(when_strays_stopped(w, m) {
    for mut child in std::mem::take(&mut w.strays) {
        if let Some(pid) = child.id().and_then(|p| i32::try_from(p).ok()) {
            procs::kill_group(pid);
        }
        let _ = tokio::time::timeout(w.remaining(), child.wait()).await;
    }
});

// ----------------------------------------------------------------------------
// When: running stems
// ----------------------------------------------------------------------------

fn parse_env_pairs(s: &str) -> Vec<(String, String)> {
    util::split_args(s)
        .unwrap_or_else(|e| panic!("{e}"))
        .into_iter()
        .map(|kv| {
            let (k, v) = kv
                .split_once('=')
                .unwrap_or_else(|| panic!("expected KEY=value, got {kv:?}"));
            (k.to_owned(), v.to_owned())
        })
        .collect()
}

step!(when_run(w, m) {
    w.run(&m[1], None, &[]).await;
});

step!(when_run_from(w, m) {
    let base = w.default_cwd();
    let dir = base.join(w.expand(&m[2]));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("creating {}: {e}", dir.display()));
    w.run(&m[1], Some(dir), &[]).await;
});

step!(when_run_with_env(w, m) {
    let env = parse_env_pairs(&m[2]);
    w.run(&m[1], None, &env).await;
});

step!(when_run_outside(w, m) {
    let dir = w.root.join("outside");
    w.run(&m[1], Some(dir), &[]).await;
});

// ----------------------------------------------------------------------------
// Then: exit codes and output
// ----------------------------------------------------------------------------

step!(then_exit_code(w, m) {
    let want: i32 = m[1].parse().expect("exit code is a number");
    let last = w.last();
    assert!(last.code == want, "expected exit code {want}\n{}", last.describe());
});

step!(then_succeeds(w, m) {
    let last = w.last();
    assert!(last.code == 0, "expected the command to succeed\n{}", last.describe());
    if let Some(ok) = last.json.as_ref().and_then(|j| j.get("ok")) {
        assert!(ok == &Value::Bool(true), "exit code 0 but `ok` is {ok}\n{}", last.describe());
    }
});

step!(then_fails(w, m) {
    let last = w.last();
    assert!(last.code != 0, "expected the command to fail\n{}", last.describe());
});

fn nodes<'a>(w: &'a E2eWorld, path: &str) -> Vec<&'a Value> {
    util::query(w.last_json(), path).unwrap_or_else(|e| panic!("{e}"))
}

fn expected(w: &E2eWorld, raw: &str) -> Value {
    util::parse_expected(&w.expand(raw))
}

step!(then_json_equals(w, m) {
    let want = expected(w, &m[2]);
    let got = nodes(w, &m[1]);
    assert!(
        got.len() == 1 && got[0] == &want,
        "JSON at {} is {} (expected {want})\n{}",
        m[1],
        serde_json::to_string(&got).unwrap_or_default(),
        w.last().describe()
    );
});

step!(then_json_not_equals(w, m) {
    let want = expected(w, &m[2]);
    let got = nodes(w, &m[1]);
    assert!(
        got.len() == 1 && got[0] != &want,
        "JSON at {} is {} (expected exactly one node different from {want})\n{}",
        m[1],
        serde_json::to_string(&got).unwrap_or_default(),
        w.last().describe()
    );
});

step!(then_json_contains(w, m) {
    let want = expected(w, &m[2]);
    let got = nodes(w, &m[1]);
    assert!(
        got.iter().any(|v| util::contains(v, &want)),
        "JSON at {} ({}) does not contain {want}\n{}",
        m[1],
        serde_json::to_string(&got).unwrap_or_default(),
        w.last().describe()
    );
});

step!(then_json_semver(w, m) {
    let got = nodes(w, &m[1]);
    assert!(
        got.len() == 1 && got[0].as_str().is_some_and(util::is_semver),
        "JSON at {} is {} (expected a semver string)\n{}",
        m[1],
        serde_json::to_string(&got).unwrap_or_default(),
        w.last().describe()
    );
});

step!(then_json_exists(w, m) {
    assert!(!nodes(w, &m[1]).is_empty(), "nothing at JSON path {}\n{}", m[1], w.last().describe());
});

step!(then_json_not_exists(w, m) {
    let got = nodes(w, &m[1]);
    assert!(got.is_empty(), "JSON path {} unexpectedly matched {}", m[1], serde_json::to_string(&got).unwrap_or_default());
});

fn json_errors(w: &E2eWorld) -> Vec<Value> {
    let last = w.last();
    let doc = last
        .json
        .clone()
        .or_else(|| util::parse_json_output(&last.stderr))
        .unwrap_or_else(|| panic!("no JSON envelope on stdout or stderr\n{}", last.describe()));
    util::query(&doc, "$.errors[*]")
        .unwrap_or_default()
        .into_iter()
        .cloned()
        .collect()
}

step!(then_error_code(w, m) {
    let errors = json_errors(w);
    let hit = errors.iter().any(|e| {
        e.get("code").and_then(Value::as_str) == Some(m[1].as_str())
            && (m[2].is_empty() || e.get("path").and_then(Value::as_str) == Some(m[2].as_str()))
    });
    assert!(
        hit,
        "no error with code {:?}{} in $.errors: {}\n{}",
        m[1],
        if m[2].is_empty() { String::new() } else { format!(" and path {:?}", m[2]) },
        serde_json::to_string(&errors).unwrap_or_default(),
        w.last().describe()
    );
});

step!(then_error_message_contains(w, m) {
    let text = w.expand(&m[1]);
    let errors = json_errors(w);
    assert!(
        errors.iter().any(|e| e.get("message").and_then(Value::as_str).is_some_and(|s| s.contains(&text))),
        "no error message contains {text:?}: {}\n{}",
        serde_json::to_string(&errors).unwrap_or_default(),
        w.last().describe()
    );
});

step!(then_stdout_contains(w, m) {
    let text = w.expand(&m[1]);
    let last = w.last();
    assert!(last.stdout.contains(&text), "stdout does not contain {text:?}\n{}", last.describe());
});

step!(then_stderr_contains(w, m) {
    let text = w.expand(&m[1]);
    let last = w.last();
    assert!(last.stderr.contains(&text), "stderr does not contain {text:?}\n{}", last.describe());
});

step!(then_stdout_not_json(w, m) {
    let last = w.last();
    assert!(
        last.json.is_none() && !last.stdout.trim_start().starts_with('{'),
        "stdout is JSON (expected human text)\n{}",
        last.describe()
    );
});

step!(then_golden(w, m) {
    let ignore: Vec<String> = m[2].split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
    let dir = world::repo_root().join("tests/features/goldens");
    // Goldens hold the ports the workspace declares, not the remapped ones.
    let back: std::collections::HashMap<String, String> =
        w.port_map.iter().map(|(from, to)| (to.to_string(), from.to_string())).collect();
    let actual = regex::Regex::new(r"\b\d{2,5}\b")
        .expect("regex")
        .replace_all(&w.last().stdout, |c: &regex::Captures<'_>| {
            back.get(&c[0]).cloned().unwrap_or_else(|| c[0].to_owned())
        })
        .into_owned();
    golden::check(&dir, &m[1], &actual, &ignore).unwrap_or_else(|e| panic!("{e}"));
});

// ----------------------------------------------------------------------------
// TUI frames (27)
// ----------------------------------------------------------------------------

/// Map remapped ports in `text` back to the ones the workspace declares.
fn unmap_ports(w: &E2eWorld, text: &str) -> String {
    let back: std::collections::HashMap<String, String> = w
        .port_map
        .iter()
        .map(|(from, to)| (to.to_string(), from.to_string()))
        .collect();
    regex::Regex::new(r"\b\d{2,5}\b")
        .expect("regex")
        .replace_all(text, |c: &regex::Captures<'_>| {
            back.get(&c[0]).cloned().unwrap_or_else(|| c[0].to_owned())
        })
        .into_owned()
}

/// The frames of a headless `stems attach` / `stems up` in `stdout`: the
/// text after each `--- frame N ---` line up to the next one (ports mapped
/// back to the declared ones).
pub fn frames_in(stdout: &str) -> Vec<String> {
    let mut frames = Vec::new();
    let mut cur: Option<Vec<&str>> = None;
    for line in stdout.lines() {
        let t = line.trim();
        if t.starts_with("--- frame ") && t.ends_with(" ---") {
            if let Some(c) = cur.take() {
                frames.push(c.join("\n"));
            }
            cur = Some(Vec::new());
        } else if let Some(c) = cur.as_mut() {
            c.push(line);
        }
    }
    if let Some(c) = cur {
        frames.push(c.join("\n"));
    }
    frames
}

fn frames(w: &E2eWorld) -> Vec<String> {
    frames_in(&unmap_ports(w, &w.last().stdout))
}

fn frame_n(w: &E2eWorld, n: &str) -> String {
    let all = frames(w);
    let i: usize = n.parse().expect("frame number");
    all.get(i.wrapping_sub(1)).cloned().unwrap_or_else(|| {
        panic!(
            "there is no frame {i} ({} frames)\n{}",
            all.len(),
            w.last().describe()
        )
    })
}

/// Mask frame columns by the table header's positions: in the header line
/// (the first with `STEM` and `STATUS`) each named column spans from its
/// start to the next header's; in the rows below it (up to the first blank
/// line) each masked cell becomes `*`, blank ones too (a sparkline may or
/// may not have its first sample yet; 25). `daemon pid N` becomes
/// `daemon pid *`. Trailing spaces are trimmed.
pub fn mask_frame(frame: &str, cols: &[String]) -> String {
    let mut lines: Vec<Vec<char>> = frame.lines().map(|l| l.chars().collect()).collect();
    let header = lines.iter().position(|l| {
        let s: String = l.iter().collect();
        let words: Vec<&str> = s.split_whitespace().collect();
        words.contains(&"STEM") && words.contains(&"STATUS")
    });
    if let Some(h) = header {
        let hl = lines[h].clone();
        let starts: Vec<usize> = (0..hl.len())
            .filter(|&i| hl[i] != ' ' && (i == 0 || hl[i - 1] == ' '))
            .collect();
        let names: Vec<String> = starts
            .iter()
            .map(|&s| hl[s..].iter().take_while(|c| **c != ' ').collect())
            .collect();
        let ranges: Vec<(usize, Option<usize>)> = names
            .iter()
            .enumerate()
            .filter(|(_, n)| cols.iter().any(|c| c.eq_ignore_ascii_case(n)))
            .map(|(i, _)| (starts[i], starts.get(i + 1).copied()))
            .collect();
        for row in lines.iter_mut().skip(h + 1) {
            if row.iter().all(|c| *c == ' ') {
                break;
            }
            for &(a, b) in &ranges {
                if row.len() <= a {
                    row.resize(a + 1, ' ');
                }
                let b = b.unwrap_or(row.len()).min(row.len());
                for c in &mut row[a..b] {
                    *c = ' ';
                }
                row[a] = '*';
            }
        }
    }
    let pid = regex::Regex::new(r"daemon pid \d+").expect("regex");
    // `TIME` (29): clock times (`12:00:01`, `12:00:01.234`) anywhere in the
    // frame (log lines, the events table) get their digits masked.
    let time = regex::Regex::new(r"\b\d{2}:\d{2}:\d{2}(?:\.\d{3})?\b").expect("regex");
    let mask_time = cols.iter().any(|c| c.eq_ignore_ascii_case("TIME"));
    lines
        .iter()
        .map(|l| {
            let s: String = l.iter().collect();
            let mut s = pid.replace_all(s.trim_end(), "daemon pid *").into_owned();
            // The status bar's right-hand hints are right-aligned (and cut
            // with `…`) after a left part whose width depends on the pid's
            // digits: keep the line only up to the six glyph counts.
            if let Some(i) = s.find("daemon pid * · ") {
                let head = i + "daemon pid * · ".len();
                let counts: Vec<&str> = s[head..].split(' ').take(6).collect();
                s = format!("{}{}", &s[..head], counts.join(" "));
            }
            if mask_time {
                time.replace_all(&s, |c: &regex::Captures| {
                    c[0].replace(|ch: char| ch.is_ascii_digit(), "*")
                })
                .into_owned()
            } else {
                s
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end()
        .to_owned()
        + "\n"
}

step!(then_frame_golden(w, m) {
    let cols: Vec<String> = m[2].split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
    let all = frames(w);
    let last = all.last().unwrap_or_else(|| panic!("no frame in stdout\n{}", w.last().describe()));
    let actual = mask_frame(last, &cols);
    let dir = world::repo_root().join("tests/features/goldens");
    let path = dir.join(format!("{}.txt", m[1]));
    if std::env::var("STEMS_E2E_BLESS").is_ok_and(|v| v == "1") {
        std::fs::write(&path, &actual).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        return;
    }
    let Ok(expected) = std::fs::read_to_string(&path) else {
        let new = dir.join(format!("{}.txt.new", m[1]));
        std::fs::write(&new, &actual).unwrap_or_else(|e| panic!("{}: {e}", new.display()));
        panic!("golden {} does not exist; wrote {} for review (rename it, or rerun with STEMS_E2E_BLESS=1)", path.display(), new.display());
    };
    let expected = mask_frame(&expected, &cols);
    assert!(
        expected == actual,
        "the frame does not match golden {} (masking {cols:?})\n--- expected\n{expected}--- actual\n{actual}",
        path.display()
    );
});

step!(then_last_frame_contains(w, m) {
    let text = w.expand(&m[1]);
    let all = frames(w);
    let last = all.last().unwrap_or_else(|| panic!("no frame in stdout\n{}", w.last().describe()));
    assert!(last.contains(&text), "the last frame does not contain {text:?}\n{last}");
});

step!(then_last_frame_not_contains(w, m) {
    let text = w.expand(&m[1]);
    let all = frames(w);
    let last = all.last().unwrap_or_else(|| panic!("no frame in stdout\n{}", w.last().describe()));
    assert!(!last.contains(&text), "the last frame contains {text:?}\n{last}");
});

step!(then_frame_n_contains(w, m) {
    let text = w.expand(&m[2]);
    let f = frame_n(w, &m[1]);
    assert!(f.contains(&text), "frame {} does not contain {text:?}\n{f}", m[1]);
});

step!(then_frame_n_not_contains(w, m) {
    let text = w.expand(&m[2]);
    let f = frame_n(w, &m[1]);
    assert!(!f.contains(&text), "frame {} contains {text:?}\n{f}", m[1]);
});

step!(then_stdout_restore_sequence(w, m) {
    let last = w.last();
    assert!(
        last.stdout.contains("\x1b[?1049l"),
        "stdout does not contain the terminal restore sequence ESC[?1049l\n{}",
        last.describe()
    );
});

/// Standard base64 (with padding), for the OSC 52 step.
fn base64(data: &[u8]) -> String {
    const B: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            out.push(if i <= c.len() {
                B[((n >> (18 - 6 * i)) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

step!(then_stdout_osc52(w, m) {
    // `\n` in the step text is a newline (a copied range of lines).
    let text = w.expand(&m[1]).replace("\\n", "\n");
    let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let last = w.last();
    assert!(
        last.stdout.contains(&seq),
        "stdout does not contain the OSC 52 sequence for {text:?} ({seq:?})\n{}",
        last.describe()
    );
});

step!(then_stdout_not_contains(w, m) {
    let text = w.expand(&m[1]);
    let last = w.last();
    assert!(!last.stdout.contains(&text), "stdout contains {text:?}\n{}", last.describe());
});

step!(then_background_stdout_contains(w, m) {
    let until = bound(w, &m[1]);
    let want = w.expand(&m[2]);
    loop {
        let bg = w.background();
        bg.poll_exit();
        if bg.stdout().contains(&want) {
            return;
        }
        assert!(
            Instant::now() < until,
            "the background command's stdout does not contain {want:?} within {}s\n{}",
            m[1],
            bg.describe()
        );
        tokio::time::sleep(POLL).await;
    }
});

// ----------------------------------------------------------------------------
// Daemon-backed steps (need deliverables 08/10+)
// ----------------------------------------------------------------------------

fn bound(w: &E2eWorld, secs: &str) -> Instant {
    let n: f64 = secs.parse().expect("seconds");
    Instant::now() + Duration::from_secs_f64(n).min(w.remaining())
}

step!(then_within_stem_status(w, m) {
    let until = bound(w, &m[1]);
    let (name, want) = (m[2].clone(), m[3].clone());
    loop {
        let status = w.status().await;
        let state = find_stem(&status, &name).and_then(stem_state).map(str::to_owned);
        if state.as_deref() == Some(want.as_str()) {
            return;
        }
        let seen = state.unwrap_or_else(|| format!("stem missing from {status}"));
        assert!(Instant::now() < until, "stem {name:?} is {seen:?}, not {want:?}, after {}s", m[1]);
        tokio::time::sleep(POLL).await;
    }
});

fn events_of(v: &Value) -> Vec<Value> {
    // A single NDJSON line parses as one object, not an array.
    if v.get("seq").is_some() && v.get("kind").is_some() {
        return vec![v.clone()];
    }
    match v.get("data").unwrap_or(v) {
        Value::Array(a) => a
            .iter()
            .flat_map(|e| match e.get("data") {
                Some(Value::Array(inner)) => inner.clone(),
                _ => vec![e.clone()],
            })
            .collect(),
        Value::Object(o) => o
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

step!(then_within_events(w, m) {
    let until = bound(w, &m[1]);
    let want = expected(w, &m[2]);
    loop {
        let out = w.run("stems events --json --since 0", None, &[]).await;
        out.guard_implemented("08");
        let events = out.json.as_ref().map(events_of).unwrap_or_default();
        if events.iter().any(|e| util::is_subset(&want, e)) {
            return;
        }
        assert!(
            Instant::now() < until,
            "no event matching {want} within {}s; last {} event(s)\n{}",
            m[1],
            events.len(),
            out.describe()
        );
        tokio::time::sleep(POLL).await;
    }
});

step!(then_events_before(w, m) {
    let (first, second) = (expected(w, &m[1]), expected(w, &m[2]));
    let out = w.run("stems events --json --since 0", None, &[]).await;
    out.guard_implemented("08");
    let events = out.json.as_ref().map(events_of).unwrap_or_default();
    let seq = |want: &Value| {
        events
            .iter()
            .find(|e| util::is_subset(want, e))
            .and_then(|e| e.get("seq").and_then(Value::as_u64))
    };
    let (a, b) = (seq(&first), seq(&second));
    assert!(
        matches!((a, b), (Some(a), Some(b)) if a < b),
        "expected an event matching {first} (seq {a:?}) before one matching {second} (seq {b:?})\n{}",
        out.describe()
    );
});

step!(when_chaos(w, m) {
    let status = w.status().await;
    let stem = find_stem(&status, &m[2]).unwrap_or_else(|| panic!("stem {:?} not in status: {status}", m[2]));
    let port = stem_port(stem).unwrap_or_else(|| panic!("stem {:?} has no port in status: {stem}", m[2]));
    let path = format!("/__chaos/{}", m[1].trim_start_matches('/'));
    let timeout = w.remaining().min(Duration::from_secs(10));
    let resp = tokio::task::spawn_blocking(move || http::get(port, &path, timeout))
        .await
        .expect("http task");
    w.last_http = Some(match resp {
        Ok(r) => r,
        Err(e) if e.starts_with("connect") => panic!("chaos endpoint on {:?}: {e}", m[2]),
        // The endpoint may kill the process before answering (e.g. crash).
        Err(e) => http::Response { status: 0, body: e },
    });
});

step!(then_chaos_status(w, m) {
    let want: u16 = m[1].parse().expect("status code");
    let got = w.last_http.as_ref().expect("no chaos endpoint has been called");
    assert!(got.status == want, "chaos response was {} {:?}, expected {want}", got.status, got.body);
});

step!(when_daemon_killed(w, m) {
    let sig = procs::parse_signal(&m[1]).unwrap_or_else(|e| panic!("{e}"));
    w.daemon_started = true;
    let out = w.run("stems daemon status --json", None, &[]).await;
    out.guard_implemented("08");
    let pid = out
        .json
        .as_ref()
        .and_then(|j| j.pointer("/data/pid"))
        .and_then(Value::as_i64)
        .and_then(|p| i32::try_from(p).ok())
        .unwrap_or_else(|| panic!("no $.data.pid in `stems daemon status --json`\n{}", out.describe()));
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), sig)
        .unwrap_or_else(|e| panic!("kill {pid} with {sig}: {e}"));
    // SIGKILL cannot be caught: wait (bounded) until the pid is really gone
    // so the next step observes the crashed state, not a dying process.
    if sig == nix::sys::signal::Signal::SIGKILL {
        let until = Instant::now() + Duration::from_secs(2).min(w.remaining());
        while procs::pid_alive(pid) && !procs::is_zombie(pid) {
            assert!(Instant::now() < until, "daemon pid {pid} survived SIGKILL for 2s");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
});

// ----------------------------------------------------------------------------
// Daemon lifecycle, RPCs and background commands (deliverables 08/09)
// ----------------------------------------------------------------------------

step!(given_daemon_started(w, m) {
    w.daemon_started = true;
    let env = if m[1].is_empty() {
        Vec::new()
    } else {
        vec![("STEMS_DEBUG_RPC".to_owned(), "1".to_owned())]
    };
    let out = w.run("stems daemon start --json", None, &env).await;
    out.guard_implemented("08");
    assert!(out.code == 0, "`stems daemon start --json` failed\n{}", out.describe());
});

/// The daemon's socket under `STEMS_HOME` (exactly one expected).
fn daemon_socket(w: &E2eWorld) -> PathBuf {
    let socks = find_files(&w.home, &|n| n == "stemsd.sock");
    assert!(
        socks.len() == 1,
        "expected one stemsd.sock under {}, found {socks:?}",
        w.home.display()
    );
    socks[0].clone()
}

step!(when_rpc(w, m) {
    let method = m[1].clone();
    let raw = w.expand(&m[2]);
    let params: Value = serde_json::from_str(raw.trim())
        .unwrap_or_else(|e| panic!("RPC params are not JSON ({e}): {raw}"));
    let socket = daemon_socket(w);
    let mut opts = stems_api::client::ClientOptions::new("cli:e2e");
    opts.call_timeout = w.remaining();
    let started = Instant::now();
    let r: Result<Value, stems_core::Error> = async {
        let c = stems_api::client::Client::connect(&socket, opts).await?;
        c.call(stems_api::Method::new(method.clone()), params.clone()).await
    }
    .await;
    let (code, doc) = match r {
        Ok(v) => (0, serde_json::json!({"ok": true, "result": v, "errors": []})),
        Err(e) => (e.exit_code(), serde_json::json!({"ok": false, "result": null, "errors": [e]})),
    };
    world::collect_pgids(&doc, &mut w.pgids);
    w.last = Some(world::CmdOutput {
        argv: vec!["rpc".to_owned(), method, params.to_string()],
        cwd: w.default_cwd(),
        code,
        stdout: serde_json::to_string_pretty(&doc).unwrap_or_default(),
        stderr: String::new(),
        json: Some(doc),
        elapsed: started.elapsed(),
    });
});

step!(when_save_json(w, m) {
    let got = nodes(w, &m[1]);
    assert!(!got.is_empty(), "nothing at JSON path {}\n{}", m[1], w.last().describe());
    let v = if got.len() == 1 { got[0].clone() } else { Value::Array(got.into_iter().cloned().collect()) };
    let text = match v {
        Value::String(s) => s,
        other => other.to_string(),
    };
    w.vars.insert(m[2].clone(), text);
});

step!(when_run_background(w, m) {
    w.run_background(&m[1], &[]);
});

step!(when_background_stopped(w, m) {
    let remaining = w.remaining();
    let bg = w.background();
    if bg.poll_exit().is_none() {
        let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(-bg.pgid), nix::sys::signal::Signal::SIGINT);
        let bound = Duration::from_secs(5).min(remaining);
        if tokio::time::timeout(bound, bg.child.wait()).await.is_err() {
            procs::kill_group(bg.pgid);
            let _ = bg.child.wait().await;
            panic!("the background command ignored SIGINT for {}s\n{}", bound.as_secs(), bg.describe());
        }
        bg.poll_exit();
    }
    // Let the reader tasks drain the pipes.
    let _ = w.remaining();
});

fn background_events(out: &str) -> Vec<Value> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

step!(then_background_in_order(w, m) {
    let until = bound(w, &m[1]);
    let want = expected(w, &m[2]);
    let want = want.as_array().cloned().unwrap_or_else(|| panic!("expected a JSON array of subsets, got {want}"));
    loop {
        let bg = w.background();
        bg.poll_exit();
        let lines = background_events(&bg.stdout());
        let mut it = lines.iter();
        let matched = want.iter().all(|wv| it.any(|l| util::is_subset(wv, l)));
        if matched {
            return;
        }
        assert!(
            Instant::now() < until,
            "the background output does not contain {} in order within {}s\n{}",
            serde_json::to_string(&want).unwrap_or_default(),
            m[1],
            bg.describe()
        );
        tokio::time::sleep(POLL).await;
    }
});

step!(then_background_exits(w, m) {
    let until = bound(w, &m[1]);
    let want: i32 = m[2].parse().expect("exit code");
    loop {
        let bg = w.background();
        if let Some(code) = bg.poll_exit() {
            assert!(code == want, "the background command exited {code}, expected {want}\n{}", bg.describe());
            return;
        }
        assert!(Instant::now() < until, "the background command is still running after {}s\n{}", m[1], bg.describe());
        tokio::time::sleep(POLL).await;
    }
});

fn parse_port(w: &E2eWorld, raw: &str) -> u16 {
    let s = w.expand(raw);
    s.trim()
        .parse()
        .unwrap_or_else(|_| panic!("not a port: {s:?}"))
}

step!(when_chaos_port(w, m) {
    let port = parse_port(w, &m[2]);
    let path = format!("/__chaos/{}", m[1].trim_start_matches('/'));
    let timeout = w.remaining().min(Duration::from_secs(10));
    let resp = tokio::task::spawn_blocking(move || http::get(port, &path, timeout))
        .await
        .expect("http task");
    w.last_http = Some(match resp {
        Ok(r) => r,
        Err(e) if e.starts_with("connect") => panic!("chaos endpoint on port {port}: {e}"),
        Err(e) => http::Response { status: 0, body: e },
    });
});

step!(then_port_listening(w, m) {
    let until = bound(w, &m[1]);
    let port = parse_port(w, &m[2]);
    loop {
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return;
        }
        assert!(Instant::now() < until, "nothing listens on 127.0.0.1:{port} after {}s", m[1]);
        tokio::time::sleep(POLL).await;
    }
});

async fn daemon_pid(w: &mut E2eWorld) -> i32 {
    let out = w
        .run_quiet("stems daemon status --json", w.remaining())
        .await
        .expect("`stems daemon status --json` ran");
    out.json
        .as_ref()
        .and_then(|j| j.pointer("/data/pid"))
        .and_then(Value::as_i64)
        .and_then(|p| i32::try_from(p).ok())
        .unwrap_or_else(|| {
            panic!(
                "no $.data.pid in `stems daemon status --json`\n{}",
                out.describe()
            )
        })
}

step!(then_daemon_rss_below(w, m) {
    let max_mb: f64 = m[1].parse().expect("MB");
    let pid = daemon_pid(w).await;
    let out = tokio::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .await
        .expect("ps");
    let text = String::from_utf8_lossy(&out.stdout);
    let kb: f64 = text.trim().parse().unwrap_or_else(|_| panic!("ps -o rss= -p {pid} printed {text:?}"));
    let mb = kb / 1024.0;
    assert!(mb < max_mb, "daemon pid {pid} RSS is {mb:.1} MB (limit {max_mb} MB)");
});

step!(then_last_took_less(w, m) {
    let max: u128 = m[1].parse().expect("ms");
    let last = w.last();
    assert!(
        last.elapsed.as_millis() < max,
        "the last command took {} ms (limit {max} ms)\n{}",
        last.elapsed.as_millis(),
        last.describe()
    );
});

step!(then_last_took_at_least(w, m) {
    let min: u128 = m[1].parse().expect("ms");
    let last = w.last();
    assert!(
        last.elapsed.as_millis() >= min,
        "the last command took only {} ms (expected at least {min} ms)\n{}",
        last.elapsed.as_millis(),
        last.describe()
    );
});

step!(then_within_no_lock_socket(w, m) {
    let until = bound(w, &m[1]);
    loop {
        let files = find_files(&w.home, &is_socket_or_lock);
        if files.is_empty() {
            return;
        }
        assert!(Instant::now() < until, "lock/socket files still exist after {}s: {files:?}", m[1]);
        tokio::time::sleep(POLL).await;
    }
});

step!(then_socket_mode(w, m) {
    use std::os::unix::fs::PermissionsExt;
    let want = u32::from_str_radix(&m[1], 8).expect("octal mode");
    let socket = daemon_socket(w);
    let mode = std::fs::metadata(&socket)
        .unwrap_or_else(|e| panic!("{}: {e}", socket.display()))
        .permissions()
        .mode()
        & 0o777;
    assert!(mode == want, "{} has mode {mode:o}, expected {want:o}", socket.display());
});

step!(given_stale_lock(w, m) {
    let out = w
        .run_quiet("stems daemon status --json", w.remaining())
        .await
        .expect("`stems daemon status --json` ran");
    let data = out.json.as_ref().and_then(|j| j.get("data")).cloned().unwrap_or_default();
    assert!(
        data.get("running") == Some(&Value::Bool(false)),
        "expected no daemon running before writing a stale lock\n{}",
        out.describe()
    );
    let lock = PathBuf::from(data["lock"].as_str().expect("$.data.lock"));
    let socket = PathBuf::from(data["socket"].as_str().expect("$.data.socket"));
    // A pid that certainly belonged to a process that is gone: spawn and reap.
    let mut child = tokio::process::Command::new("true").spawn().expect("spawn true");
    let pid = child.id().expect("pid");
    child.wait().await.expect("reap true");
    let dir = lock.parent().expect("lock dir");
    std::fs::create_dir_all(dir).expect("create daemon dir");
    let body = serde_json::json!({
        "pid": pid,
        "start_time": 1,
        "version": "0.0.1",
        "created_at": "2026-01-01T00:00:00Z",
    });
    std::fs::write(&lock, body.to_string()).expect("write lock");
    if !m[1].is_empty() {
        // A socket file nobody listens on.
        drop(std::os::unix::net::UnixListener::bind(&socket).expect("bind socket"));
    }
    w.daemon_started = true;
});

step!(then_daemon_log_contains(w, m) {
    let until = bound(w, &m[1]);
    let text = w.expand(&m[2]);
    loop {
        let logs = find_files(&w.home, &|n| n == "stemsd.log");
        let all: String = logs.iter().map(|f| std::fs::read_to_string(f).unwrap_or_default()).collect();
        if all.contains(&text) {
            return;
        }
        assert!(Instant::now() < until, "the daemon log ({logs:?}) does not contain {text:?} after {}s:\n{}", m[1], util::clip(&all, 3000));
        tokio::time::sleep(POLL).await;
    }
});

step!(then_json_greater(w, m) {
    let min: f64 = m[2].parse().expect("number");
    let got = nodes(w, &m[1]);
    assert!(
        got.len() == 1 && got[0].as_f64().is_some_and(|n| n > min),
        "JSON at {} is {} (expected a number > {min})\n{}",
        m[1],
        serde_json::to_string(&got).unwrap_or_default(),
        w.last().describe()
    );
});

step!(then_pids_dead(w, m) {
    let until = bound(w, &m[1]);
    let raw = w.expand(&m[2]);
    let v: Value = serde_json::from_str(raw.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {raw}"));
    let pids: Vec<i32> = match &v {
        Value::Array(a) => a.iter().filter_map(Value::as_i64).filter_map(|p| i32::try_from(p).ok()).collect(),
        other => other.as_i64().and_then(|p| i32::try_from(p).ok()).into_iter().collect(),
    };
    assert!(!pids.is_empty(), "no pids in {raw}");
    loop {
        let alive: Vec<i32> = pids.iter().copied().filter(|p| procs::pid_alive(*p) && !procs::is_zombie(*p)).collect();
        if alive.is_empty() {
            return;
        }
        assert!(Instant::now() < until, "still alive after {}s: {alive:?}", m[1]);
        tokio::time::sleep(POLL).await;
    }
});

// ----------------------------------------------------------------------------
// Logs (12)
// ----------------------------------------------------------------------------

/// Entry names and contents of a `.tar.gz` (path relative to the workspace).
fn archive_entries(w: &E2eWorld, rel: &str) -> Vec<(String, Vec<u8>)> {
    use std::io::Read;
    let p = ws_file(w, rel);
    let f = std::fs::File::open(&p).unwrap_or_else(|e| panic!("cannot open {}: {e}", p.display()));
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(f));
    let mut out = Vec::new();
    for e in ar
        .entries()
        .unwrap_or_else(|e| panic!("{} is not a tar.gz: {e}", p.display()))
    {
        let mut e = e.unwrap_or_else(|e| panic!("bad entry in {}: {e}", p.display()));
        let name = e
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut data = Vec::new();
        e.read_to_end(&mut data)
            .unwrap_or_else(|e| panic!("cannot read {name}: {e}"));
        out.push((name, data));
    }
    out
}

step!(then_archive_contains(w, m) {
    let want = w.expand(&m[2]);
    let entries = archive_entries(w, &m[1]);
    let names: Vec<&str> = entries.iter().map(|(n, _)| n.as_str()).collect();
    let hit = names.iter().any(|n| *n == want || (want.ends_with('/') && n.starts_with(&want)));
    assert!(hit, "archive {} has no entry {want:?}; entries: {names:?}", m[1]);
});

step!(then_archive_entry_contains(w, m) {
    let (entry, text) = (w.expand(&m[2]), w.expand(&m[3]));
    let entries = archive_entries(w, &m[1]);
    let data = entries
        .iter()
        .find(|(n, _)| *n == entry)
        .map(|(_, d)| String::from_utf8_lossy(d).into_owned())
        .unwrap_or_else(|| panic!("archive {} has no entry {entry:?}", m[1]));
    assert!(data.contains(&text), "archive entry {entry} does not contain {text:?}:\n{}", util::clip(&data, 3000));
});

step!(then_archive_entry_not_contains(w, m) {
    let (entry, text) = (w.expand(&m[2]), w.expand(&m[3]));
    let entries = archive_entries(w, &m[1]);
    let data = entries
        .iter()
        .find(|(n, _)| *n == entry)
        .map(|(_, d)| String::from_utf8_lossy(d).into_owned())
        .unwrap_or_else(|| panic!("archive {} has no entry {entry:?}", m[1]));
    assert!(!data.contains(&text), "archive entry {entry} contains {text:?}");
});

/// Files of `<STEMS_HOME>/<hash>/logs/<stem>/`.
fn stem_log_files(w: &E2eWorld, stem: &str) -> Vec<PathBuf> {
    find_files(&w.home, &|_| true)
        .into_iter()
        .filter(|p| {
            let dir = p.parent();
            dir.and_then(|d| d.file_name()).is_some_and(|n| n == stem)
                && dir
                    .and_then(|d| d.parent())
                    .and_then(|d| d.file_name())
                    .is_some_and(|n| n == "logs")
        })
        .collect()
}

step!(then_stem_log_file_contains(w, m) {
    let until = bound(w, &m[1]);
    let (stem, file) = m[2].split_once('/').unwrap_or_else(|| panic!("expected <stem>/<file>, got {:?}", m[2]));
    let text = w.expand(&m[3]);
    loop {
        let files: Vec<PathBuf> = stem_log_files(w, stem)
            .into_iter()
            .filter(|p| p.file_name().is_some_and(|n| n == file))
            .collect();
        let all: String = files.iter().map(|f| std::fs::read_to_string(f).unwrap_or_default()).collect();
        if all.contains(&text) {
            return;
        }
        assert!(Instant::now() < until, "log file {stem}/{file} under STEMS_HOME ({files:?}) does not contain {text:?} after {}s:\n{}", m[1], util::clip(&all, 2000));
        tokio::time::sleep(POLL).await;
    }
});

step!(then_stem_log_dir_at_most(w, m) {
    let max: usize = m[2].parse().expect("count");
    let files = stem_log_files(w, &m[1]);
    assert!(!files.is_empty(), "no log files for {} under {}", m[1], w.home.display());
    assert!(files.len() <= max, "{} log files for {} (at most {max}): {files:?}", files.len(), m[1]);
});

step!(then_within_output_no_json(w, m) {
    let until = bound(w, &m[1]);
    let (line, path) = (m[2].clone(), m[3].clone());
    loop {
        let out = w.run(&line, None, &[]).await;
        // NDJSON always as an array here (a single line too); empty = [].
        let doc = if out.stdout.trim().is_empty() {
            Value::Array(Vec::new())
        } else {
            let lines: Option<Vec<Value>> = out.stdout.lines().filter(|l| !l.trim().is_empty()).map(|l| serde_json::from_str(l).ok()).collect();
            lines.map(Value::Array).unwrap_or_else(|| panic!("`{line}` did not print NDJSON\n{}", out.describe()))
        };
        let hits = util::query(&doc, &path).unwrap_or_else(|e| panic!("{e}"));
        if out.code == 0 && hits.is_empty() {
            return;
        }
        assert!(Instant::now() < until, "`{line}` still has JSON at {path} after {}s: {}\n{}", m[1], serde_json::to_string(&hits).unwrap_or_default(), out.describe());
        tokio::time::sleep(POLL).await;
    }
});

step!(then_json_nodes_equal(w, m) {
    let want = expected(w, &m[2]);
    let got = Value::Array(nodes(w, &m[1]).into_iter().cloned().collect());
    assert!(
        got == want,
        "JSON nodes at {} are {} (expected {want})\n{}",
        m[1],
        got,
        w.last().describe()
    );
});

/// Order key for `is in ascending order`: RFC 3339 strings as instants.
fn order_key(v: &Value) -> Option<(i64, u32, String)> {
    match v {
        Value::String(s) => Some(match chrono::DateTime::parse_from_rfc3339(s) {
            Ok(t) => (t.timestamp(), t.timestamp_subsec_nanos(), String::new()),
            Err(_) => (0, 0, s.clone()),
        }),
        Value::Number(n) => n.as_f64().map(|f| {
            (
                f.floor() as i64,
                ((f - f.floor()) * 1e9) as u32,
                String::new(),
            )
        }),
        _ => None,
    }
}

step!(then_json_ascending(w, m) {
    let got = nodes(w, &m[1]);
    assert!(got.len() >= 2, "JSON at {} has {} nodes (expected at least 2 to compare)\n{}", m[1], got.len(), w.last().describe());
    let keys: Vec<_> = got.iter().map(|v| order_key(v).unwrap_or_else(|| panic!("cannot order {v}"))).collect();
    for (i, pair) in keys.windows(2).enumerate() {
        assert!(pair[0] <= pair[1], "JSON at {} is not ascending at index {}: {} then {}", m[1], i, got[i], got[i + 1]);
    }
});

/// Cumulative CPU seconds of `pid` (`ps -o time=`: `[[dd-]hh:]mm:ss.cc`).
async fn cpu_seconds(pid: i32) -> f64 {
    let out = tokio::process::Command::new("ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
        .await
        .expect("ps");
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let (days, rest) = match text.split_once('-') {
        Some((d, r)) => (d.parse::<f64>().unwrap_or(0.0), r.to_string()),
        None => (0.0, text.clone()),
    };
    let secs = rest
        .split(':')
        .try_fold(0.0, |acc, part| part.parse::<f64>().map(|v| acc * 60.0 + v))
        .unwrap_or_else(|_| panic!("ps -o time= -p {pid} printed {text:?}"));
    days * 86_400.0 + secs
}

step!(then_background_cpu_below(w, m) {
    let max: f64 = m[1].parse().expect("percent");
    let window = Duration::from_secs(2).min(w.remaining());
    let pid = {
        let bg = w.background();
        assert!(bg.poll_exit().is_none(), "the background command has exited\n{}", bg.describe());
        bg.child.id().and_then(|p| i32::try_from(p).ok()).expect("background pid")
    };
    // Measured over a window (not a fixed sleep: this is the sampling period).
    let t0 = (Instant::now(), cpu_seconds(pid).await);
    tokio::time::sleep(window).await;
    let t1 = (Instant::now(), cpu_seconds(pid).await);
    let pct = 100.0 * (t1.1 - t0.1) / t1.0.duration_since(t0.0).as_secs_f64();
    assert!(pct < max, "background command (pid {pid}) used {pct:.1}% CPU over {:.1}s (limit {max}%)", window.as_secs_f64());
});

// ----------------------------------------------------------------------------
// Then: nothing left behind
// ----------------------------------------------------------------------------

step!(then_no_process_alive(w, m) {
    let mut groups = w.pgids.clone();
    groups.extend(w.state_pgids());
    let until = Instant::now() + Duration::from_secs(2).min(w.remaining());
    loop {
        let alive: Vec<i32> = groups.iter().copied().filter(|g| procs::pgid_alive(*g)).collect();
        let found = procs::scan(&w.root).await;
        if alive.is_empty() && found.is_empty() {
            return;
        }
        assert!(
            Instant::now() < until,
            "still alive: process groups {alive:?}; processes {:?}",
            found.iter().map(|f| format!("{} {} ({})", f.pid, f.command, f.why)).collect::<Vec<_>>()
        );
        tokio::time::sleep(POLL).await;
    }
});

step!(then_no_container(w, m) {
    let label = w.expand(&m[1]);
    let ids = hooks::labelled_containers(&label);
    assert!(ids.is_empty(), "containers labelled stems.workspace={label} exist: {ids:?}");
});

// ----------------------------------------------------------------------------
// Docker (14/15; `@docker` scenarios only)
// ----------------------------------------------------------------------------

fn docker_bound(w: &E2eWorld) -> Duration {
    Duration::from_secs(120).min(w.remaining())
}

step!(then_container_running_with_label(w, m) {
    let name = w.expand(&m[1]);
    let (k, v) = (w.expand(&m[2]), w.expand(&m[3]));
    let r = docker::docker(&["inspect".into(), name.clone()], docker_bound(w)).await;
    assert!(r.ok, "docker inspect {name} failed: {}", r.stderr);
    let i = docker::inspect_object(&r.stdout).unwrap_or_else(|e| panic!("{name}: {e}"));
    if let Err(e) = docker::check_running_with_label(&i, &k, &v) {
        panic!("container {name}: {e}");
    }
});

step!(given_container_with_labels(w, m) {
    let name = w.expand(&m[1]);
    let labels = docker::parse_labels(&w.expand(&m[2])).unwrap_or_else(|e| panic!("{e}"));
    let r = docker::docker(&docker::run_args(&name, &labels), docker_bound(w)).await;
    assert!(r.ok, "docker run {name} failed: {}", r.stderr);
});

step!(then_docker_volume(w, m) {
    let name = w.expand(&m[1]);
    let want = m[2] == "exists";
    let r = docker::docker(
        &["volume".into(), "inspect".into(), name.clone()],
        docker_bound(w),
    )
    .await;
    assert_eq!(r.ok, want, "docker volume {name}: expected it to {}; {}", m[2], r.stderr);
});

step!(then_compose_service_running(w, m) {
    let (project, service) = (w.expand(&m[1]), w.expand(&m[2]));
    let args: Vec<String> = ["compose", "-p", &project, "ps", "--format", "json", "-a"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let r = docker::docker(&args, docker_bound(w)).await;
    assert!(r.ok, "docker compose -p {project} ps failed: {}", r.stderr);
    let entries = docker::parse_compose_ps(&r.stdout);
    assert!(
        docker::compose_service_running(&entries, &service),
        "compose project {project} has no running service {service}: {entries:?}"
    );
});

// ----------------------------------------------------------------------------
// Crash recovery (11)
// ----------------------------------------------------------------------------

/// Every `state.json` under `STEMS_HOME` parses as JSON (a missing file passes).
fn assert_state_files_parse(w: &E2eWorld, when: &str) {
    for f in find_files(&w.home, &|n| n == "state.json") {
        let text = std::fs::read_to_string(&f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        if let Err(e) = serde_json::from_str::<Value>(&text) {
            panic!(
                "{} is not valid JSON {when}: {e}\n{}",
                f.display(),
                util::clip(&text, 2000)
            );
        }
    }
}

step!(then_state_valid_json(w, m) {
    assert_state_files_parse(w, "");
});

/// The daemon's state file path: `<dir of the lock>/state.json`, from
/// `stems daemon status --json` (works without a running daemon).
async fn state_path(w: &mut E2eWorld) -> PathBuf {
    let out = w
        .run_quiet("stems daemon status --json", w.remaining())
        .await
        .expect("`stems daemon status --json` ran");
    let lock = out
        .json
        .as_ref()
        .and_then(|j| j.pointer("/data/lock"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!(
                "no $.data.lock in `stems daemon status --json`\n{}",
                out.describe()
            )
        });
    lock.parent().expect("lock dir").join("state.json")
}

step!(given_state_records_stray(w, m) {
    let stem = m[1].clone();
    let pid = w
        .strays
        .first()
        .and_then(|c| c.id())
        .and_then(|p| i32::try_from(p).ok())
        .expect("start a stray process first");
    let path = state_path(w).await;
    std::fs::create_dir_all(path.parent().expect("state dir")).expect("create daemon dir");
    // Same pid and process group as the stray, but a start time it never had:
    // a recycled pid, as far as stems can tell.
    let body = serde_json::json!({
        "version": 1,
        "run_id": "01J00000000000000000000000",
        "daemon": { "pid": 1, "start_time": 1 },
        "stems": { stem: {
            "pid": pid, "pgid": pid, "start_time": 1, "container_id": null,
            "ports": [], "overlays": [], "state": "healthy",
            "started_at": "2026-01-01T00:00:00Z", "log_file": null,
        }},
        "stamps": {},
    });
    std::fs::write(&path, serde_json::to_string_pretty(&body).expect("json")).expect("write state.json");
    w.daemon_started = true;
});

/// Pid of the daemon from the lock file under `STEMS_HOME` (no RPC, so the
/// kill lands wherever the daemon is).
fn lock_pid(w: &E2eWorld) -> Option<i32> {
    find_files(&w.home, &|n| n == "stemsd.lock")
        .iter()
        .find_map(|f| {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(f).ok()?).ok()?;
            v.get("pid")
                .and_then(Value::as_i64)
                .and_then(|p| i32::try_from(p).ok())
        })
}

step!(when_daemon_killed_during(w, m) {
    let (line, marker) = (m[1].clone(), m[2].clone());
    let times: u32 = m[3].parse().expect("a count");
    w.daemon_started = true;
    for i in 0..times {
        if let Some(mut prev) = w.background.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5).min(w.remaining()), prev.child.wait()).await;
            procs::kill_group(prev.pgid);
        }
        w.run_background(&line, &[]);
        // Bounded poll for the marker line (or the command giving up).
        let until = Instant::now() + Duration::from_secs(20).min(w.remaining());
        let seen = loop {
            let bg = w.background();
            let hit = background_events(&bg.stdout())
                .iter()
                .any(|e| e.get("kind").and_then(Value::as_str) == Some(marker.as_str()));
            if hit {
                break true;
            }
            if bg.poll_exit().is_some() {
                break false;
            }
            assert!(Instant::now() < until, "iteration {i}: no {marker:?} within 20s\n{}", bg.describe());
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        if seen {
            // A pseudo-random extra 0-400 ms so the kills land at different
            // points of the startup (the point of the test, not a wait).
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos());
            tokio::time::sleep(Duration::from_millis(u64::from(nanos % 400))).await;
            if let Some(pid) = lock_pid(w) {
                let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL);
                let until = Instant::now() + Duration::from_secs(2).min(w.remaining());
                while procs::pid_alive(pid) && !procs::is_zombie(pid) {
                    assert!(Instant::now() < until, "daemon pid {pid} survived SIGKILL for 2s");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        }
        // The client notices the dead daemon and exits.
        let bound = Duration::from_secs(10).min(w.remaining());
        let bg = w.background();
        if tokio::time::timeout(bound, bg.child.wait()).await.is_err() {
            procs::kill_group(bg.pgid);
            panic!("iteration {i}: `{line}` did not exit after the daemon was killed\n{}", bg.describe());
        }
        bg.poll_exit();
        assert_state_files_parse(w, &format!("after kill {}", i + 1));
    }
});

step!(then_state_no_stems(w, m) {
    for f in find_files(&w.home, &|n| n == "state.json") {
        let text = std::fs::read_to_string(&f).unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", f.display()));
        let empty = match v.get("stems") {
            None | Some(Value::Null) => true,
            Some(Value::Object(o)) => o.is_empty(),
            Some(Value::Array(a)) => a.is_empty(),
            Some(_) => false,
        };
        assert!(empty, "{} still lists stems: {}", f.display(), v["stems"]);
    }
});

step!(then_no_lock_socket(w, m) {
    let files = find_files(&w.home, &is_socket_or_lock);
    assert!(files.is_empty(), "lock/socket files exist: {files:?}");
});

fn ws_file(w: &E2eWorld, rel: &str) -> PathBuf {
    w.default_cwd().join(w.expand(rel))
}

step!(then_file_exists(w, m) {
    let p = ws_file(w, &m[1]);
    assert!(p.exists(), "{} does not exist", p.display());
});

step!(then_file_not_exists(w, m) {
    let p = ws_file(w, &m[1]);
    assert!(!p.exists(), "{} exists", p.display());
});

// ----------------------------------------------------------------------------
// Scripts (16)
// ----------------------------------------------------------------------------

step!(when_file_written(w, m) {
    let p = ws_file(w, &m[1]);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("creating {}: {e}", dir.display()));
    }
    let text = w.expand(&m[2]);
    std::fs::write(&p, format!("{text}\n")).unwrap_or_else(|e| panic!("writing {}: {e}", p.display()));
});

/// `Given the local override file contains:` + docstring (26): writes
/// `stems.local.yaml` verbatim (placeholders expanded), replacing the
/// harness-generated one. Use with a workspace "with its original ports"
/// (the generated port overrides are not merged back).
fn given_local_file_docstring<'a>(w: &'a mut E2eWorld, ctx: Context) -> LocalBoxFuture<'a, ()> {
    let text = ctx.step.docstring.clone().unwrap_or_default();
    Box::pin(async move {
        let _ = w.remaining();
        let text = w.expand(&text);
        // Gherkin docstrings start with the newline after `"""`.
        let text = text.strip_prefix('\n').unwrap_or(&text);
        // Dedent (in case the parser keeps the feature file's indentation).
        let cut = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        let mut body: String = text
            .lines()
            .map(|l| format!("{}\n", l.get(cut..).unwrap_or("").trim_end()))
            .collect();
        let p = ws_file(w, "stems.local.yaml");
        if !body.ends_with('\n') {
            body.push('\n');
        }
        std::fs::write(&p, body).unwrap_or_else(|e| panic!("writing {}: {e}", p.display()));
    })
}

step!(then_file_contains(w, m) {
    let p = ws_file(w, &m[1]);
    let want = w.expand(&m[2]);
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("reading {}: {e}", p.display()));
    assert!(text.contains(&want), "{} does not contain {want:?}:\n{text}", p.display());
});

step!(then_events_not_contain(w, m) {
    let unwanted = expected(w, &m[1]);
    let out = w.run("stems events --json --since 0", None, &[]).await;
    out.guard_implemented("08");
    let events = out.json.as_ref().map(events_of).unwrap_or_default();
    assert!(out.code == 0, "cannot read the events stream\n{}", out.describe());
    if let Some(e) = events.iter().find(|e| util::is_subset(&unwanted, e)) {
        panic!("unexpected event matching {unwanted}: {e}\n{}", out.describe());
    }
});

// ----------------------------------------------------------------------------
// Overlays (18)
// ----------------------------------------------------------------------------

step!(given_repo_copy_git(w, m) {
    let rel = std::path::PathBuf::from(w.expand(&m[1]));
    let mut parts = rel.components();
    let repo_name = parts.next().expect("path starts with the repo directory");
    let inner: std::path::PathBuf = parts.collect();
    let repos = w.root.join("examples/repos");
    assert!(
        !repos.is_symlink(),
        "use `Given the workspace has a private copy of the repos` first"
    );
    let repo = repos.join(repo_name);
    let file = repo.join(&inner);
    if !file.exists() {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("creating {}: {e}", dir.display()));
        }
        std::fs::write(&file, "committed\n").unwrap_or_else(|e| panic!("writing {}: {e}", file.display()));
    }
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["-c", "user.name=stems-e2e", "-c", "user.email=e2e@stems.invalid", "-c", "commit.gpgsign=false"])
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap_or_else(|e| panic!("running git: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let inner = inner.to_string_lossy().to_string();
    git(&["init", "-q"]);
    git(&["add", "--", &inner]);
    git(&["commit", "-q", "-m", "e2e: track the overlay dest"]);
});

step!(when_run_shell(w, m) {
    w.run_shell(&m[1]).await;
});

// ----------------------------------------------------------------------------
// Git codebases (20)
// ----------------------------------------------------------------------------

/// `git` with a neutral config and a fixed identity (the developer's global
/// config, hooks and signing never apply).
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let o = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "stems-e2e")
        .env("GIT_AUTHOR_EMAIL", "e2e@stems.invalid")
        .env("GIT_COMMITTER_NAME", "stems-e2e")
        .env("GIT_COMMITTER_EMAIL", "e2e@stems.invalid")
        .output()
        .unwrap_or_else(|e| panic!("cannot run git: {e}"));
    assert!(
        o.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&o.stderr)
    );
}

step!(given_bare_git_repo(w, m) {
    let name = &m[1];
    let src = world::repo_root().join(&m[2]);
    let dir = w.root.join("git");
    let work = dir.join(format!("{name}-src"));
    let bare = dir.join(format!("{name}.git"));
    world::copy_dir(&src, &work).unwrap_or_else(|e| panic!("copying {}: {e}", src.display()));
    git_in(&work, &["init", "-q", "-b", "main"]);
    for (k, v) in [
        ("user.name", "stems-e2e"),
        ("user.email", "e2e@stems.invalid"),
        ("commit.gpgsign", "false"),
        ("tag.gpgsign", "false"),
    ] {
        git_in(&work, &["config", k, v]);
    }
    git_in(&work, &["add", "-A"]);
    git_in(&work, &["commit", "-q", "-m", "initial"]);
    git_in(&dir, &["init", "-q", "--bare", "-b", "main", &bare.display().to_string()]);
    git_in(&work, &["remote", "add", "origin", &bare.display().to_string()]);
    git_in(&work, &["push", "-q", "origin", "main"]);
    w.vars.insert(format!("{name}_url"), format!("file://{}", bare.display()));
    w.vars.insert(format!("{name}_src"), work.display().to_string());
});

// ----------------------------------------------------------------------------
// Doctor (19)
// ----------------------------------------------------------------------------

step!(given_fake_tool(w, m) {
    let bin = w.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap_or_else(|e| panic!("creating {}: {e}", bin.display()));
    let file = bin.join(&m[1]);
    std::fs::write(&file, format!("#!/bin/sh\n{}\n", w.expand(&m[2])))
        .unwrap_or_else(|e| panic!("writing {}: {e}", file.display()));
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod {}: {e}", file.display()));
});

step!(given_state_records_overlay(w, m) {
    let stem = m[1].clone();
    let dest = ws_file(w, &m[2]);
    if !dest.exists() {
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("creating {}: {e}", dir.display()));
        }
        std::fs::write(&dest, "written by stems (e2e)\n")
            .unwrap_or_else(|e| panic!("writing {}: {e}", dest.display()));
    }
    let bytes = std::fs::read(&dest).unwrap_or_else(|e| panic!("reading {}: {e}", dest.display()));
    let record = serde_json::json!({
        "dest": dest,
        "sha256": stems_core::overlays::hash_bytes(&bytes),
        "run_id": "01J00000000000000000000000",
        "keep": false,
    });
    let path = state_path(w).await;
    std::fs::create_dir_all(path.parent().expect("state dir")).expect("create daemon dir");
    let mut doc: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| {
            serde_json::json!({
                "version": 1,
                "run_id": "01J00000000000000000000000",
                "daemon": { "pid": 1, "start_time": 1 },
                "stems": {},
                "stamps": {},
            })
        });
    let ledger = doc
        .as_object_mut()
        .expect("state.json is an object")
        .entry("overlays")
        .or_insert_with(|| serde_json::json!({}));
    let list = ledger
        .as_object_mut()
        .expect("overlays is an object")
        .entry(stem)
        .or_insert_with(|| serde_json::json!([]));
    list.as_array_mut().expect("a list of records").push(record);
    std::fs::write(&path, serde_json::to_string_pretty(&doc).expect("json")).expect("write state.json");
});

// ----------------------------------------------------------------------------
// Health (21)
// ----------------------------------------------------------------------------

async fn matching_events(w: &mut E2eWorld, want: &Value) -> (usize, String) {
    let out = w.run("stems events --json --since 0", None, &[]).await;
    out.guard_implemented("08");
    let events = out.json.as_ref().map(events_of).unwrap_or_default();
    let n = events.iter().filter(|e| util::is_subset(want, e)).count();
    (n, out.describe())
}

step!(then_exactly_events(w, m) {
    let n: usize = m[1].parse().expect("count");
    let want = expected(w, &m[2]);
    let (got, describe) = matching_events(w, &want).await;
    assert!(got == n, "{got} event(s) match {want}, expected exactly {n}\n{describe}");
});

// ----------------------------------------------------------------------------
// Restart policies (22)
// ----------------------------------------------------------------------------

step!(then_during_events_never(w, m) {
    let until = bound(w, &m[1]);
    let unwanted = expected(w, &m[2]);
    loop {
        let out = w.run("stems events --json --since 0", None, &[]).await;
        out.guard_implemented("08");
        assert!(out.code == 0, "cannot read the events stream\n{}", out.describe());
        let events = out.json.as_ref().map(events_of).unwrap_or_default();
        if let Some(e) = events.iter().find(|e| util::is_subset(&unwanted, e)) {
            panic!("unexpected event matching {unwanted}: {e}\n{}", out.describe());
        }
        if Instant::now() >= until {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
});

// Watchdogs (24): a bounded "no more than k" check (coalescing proofs).
step!(then_during_at_most_events(w, m) {
    let until = bound(w, &m[1]);
    let max: usize = m[2].parse().expect("count");
    let want = expected(w, &m[3]);
    loop {
        let (got, describe) = matching_events(w, &want).await;
        assert!(got <= max, "{got} event(s) match {want}, expected at most {max}\n{describe}");
        if Instant::now() >= until {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
});

/// Milliseconds between two events' `ts`.
fn ts_ms(e: &Value) -> Option<i64> {
    let ts = e.get("ts")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|t| t.timestamp_millis())
}

step!(then_event_followed_after(w, m) {
    let (first, second) = (expected(w, &m[1]), expected(w, &m[2]));
    let (lo, hi): (i64, i64) = (m[3].parse().expect("ms"), m[4].parse().expect("ms"));
    let out = w.run("stems events --json --since 0", None, &[]).await;
    out.guard_implemented("08");
    let events = out.json.as_ref().map(events_of).unwrap_or_default();
    let seq = |e: &Value| e.get("seq").and_then(Value::as_u64).unwrap_or(0);
    let a = events
        .iter()
        .find(|e| util::is_subset(&first, e))
        .unwrap_or_else(|| panic!("no event matches {first}\n{}", out.describe()));
    let b = events
        .iter()
        .filter(|e| seq(e) > seq(a))
        .find(|e| util::is_subset(&second, e))
        .unwrap_or_else(|| panic!("no event after {a} matches {second}\n{}", out.describe()));
    let gap = ts_ms(b).expect("ts") - ts_ms(a).expect("ts");
    assert!(
        (lo..=hi).contains(&gap),
        "{gap} ms between {a} and {b}, expected {lo}..={hi} ms"
    );
});

step!(then_within_at_least_events(w, m) {
    let until = bound(w, &m[1]);
    let n: usize = m[2].parse().expect("count");
    let want = expected(w, &m[3]);
    loop {
        let (got, describe) = matching_events(w, &want).await;
        if got >= n {
            return;
        }
        assert!(
            Instant::now() < until,
            "only {got} event(s) match {want} after {}s (want at least {n})\n{describe}",
            m[1]
        );
        tokio::time::sleep(POLL).await;
    }
});

step!(then_within_json_of(w, m) {
    let until = bound(w, &m[1]);
    let (path, line) = (m[2].clone(), m[3].clone());
    let want = expected(w, &m[4]);
    loop {
        let out = w.run(&line, None, &[]).await;
        let got: Vec<Value> = out
            .json
            .as_ref()
            .and_then(|j| util::query(j, &path).ok())
            .map(|v| v.into_iter().cloned().collect())
            .unwrap_or_default();
        if got.len() == 1 && got[0] == want {
            return;
        }
        assert!(
            Instant::now() < until,
            "JSON at {path} of `{line}` is {} after {}s (expected {want})\n{}",
            serde_json::to_string(&got).unwrap_or_default(),
            m[1],
            out.describe()
        );
        tokio::time::sleep(POLL).await;
    }
});

step!(then_daemon_cpu_below(w, m) {
    let max: f64 = m[1].parse().expect("percent");
    let pid = daemon_pid(w).await;
    let window = Duration::from_secs(3).min(w.remaining());
    // Measured over a window (the sampling period, not a wait).
    let t0 = (Instant::now(), cpu_seconds(pid).await);
    tokio::time::sleep(window).await;
    let t1 = (Instant::now(), cpu_seconds(pid).await);
    let pct = 100.0 * (t1.1 - t0.1) / t1.0.duration_since(t0.0).as_secs_f64();
    assert!(pct < max, "daemon (pid {pid}) used {pct:.1}% CPU over {:.1}s (limit {max}%)", window.as_secs_f64());
});

step!(given_generated_probe_workspace(w, m) {
    let n: u16 = m[1].parse().expect("stem count");
    let every: u64 = m[2].parse().expect("interval ms");
    assert!(n <= world::PORT_BLOCK, "at most {} stems (one port block)", world::PORT_BLOCK);
    w.use_empty_dir();
    let base = world::allocate_port_block();
    w.port_base = Some(base);
    let mut yaml = String::from(
        "# generated by `a workspace with N process stems using tcp health` (21)\nschema_version: 1\nname: probes\nstems:\n",
    );
    for i in 0..n {
        let port = base + i;
        yaml.push_str(&format!(
            "  s{i:02}:\n    type: process\n    command: exec python3 -m http.server {port} --bind 127.0.0.1\n    ports: [{{ name: http, port: {port} }}]\n    health: {{ type: tcp, interval: {every}ms, timeout: 150ms, retries: 5, start_timeout: 60s }}\n    stop_grace: 2s\n"
        ));
    }
    let path = w.ws().join("stems.yaml");
    std::fs::write(&path, yaml).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
});

// ----------------------------------------------------------------------------
// Metrics (25)
// ----------------------------------------------------------------------------

/// A number bound: a number, or a sum `a+b` after placeholder expansion
/// (`${var:rss}+104857600`).
fn number_expr(w: &E2eWorld, raw: &str) -> f64 {
    w.expand(raw)
        .split('+')
        .map(|p| {
            p.trim()
                .trim_matches('"')
                .parse::<f64>()
                .unwrap_or_else(|_| panic!("not a number: {p} (in {raw})"))
        })
        .sum()
}

/// Objects selected by `path` (arrays are flattened one level).
fn objects_at(doc: &Value, path: &str) -> Vec<Value> {
    util::query(doc, path)
        .unwrap_or_else(|e| panic!("{e}"))
        .into_iter()
        .flat_map(|v| match v {
            Value::Array(a) => a.clone(),
            other => vec![other.clone()],
        })
        .collect()
}

fn some_field_above(doc: &Value, path: &str, field: &str, min: f64) -> (bool, Vec<f64>) {
    let vals: Vec<f64> = objects_at(doc, path)
        .iter()
        .filter_map(|o| o.get(field)?.as_f64())
        .collect();
    (vals.iter().any(|v| *v > min), vals)
}

step!(then_within_json_of_greater(w, m) {
    let until = bound(w, &m[1]);
    let (path, line) = (m[2].clone(), m[3].clone());
    let min = number_expr(w, &m[4]);
    loop {
        let out = w.run(&line, None, &[]).await;
        let got: Vec<Value> = out.json.as_ref().and_then(|j| util::query(j, &path).ok()).map(|v| v.into_iter().cloned().collect()).unwrap_or_default();
        if got.len() == 1 && got[0].as_f64().is_some_and(|n| n > min) {
            return;
        }
        assert!(Instant::now() < until, "JSON at {path} of `{line}` is {} after {}s (expected a number > {min})\n{}", serde_json::to_string(&got).unwrap_or_default(), m[1], out.describe());
        tokio::time::sleep(POLL).await;
    }
});

step!(then_some_sample_greater(w, m) {
    let min = number_expr(w, &m[3]);
    let (ok, vals) = some_field_above(w.last_json(), &m[1], &m[2], min);
    assert!(ok, "no object at {} has {} > {min} (values {vals:?})\n{}", m[1], m[2], w.last().describe());
});

step!(then_within_some_sample_greater(w, m) {
    let until = bound(w, &m[1]);
    let (path, line, field) = (m[2].clone(), m[3].clone(), m[4].clone());
    let min = number_expr(w, &m[5]);
    loop {
        let out = w.run(&line, None, &[]).await;
        let (ok, vals) = out.json.as_ref().map(|j| some_field_above(j, &path, &field, min)).unwrap_or_default();
        if ok {
            return;
        }
        assert!(Instant::now() < until, "no object at {path} of `{line}` has {field} > {min} after {}s (last values {vals:?})\n{}", m[1], out.describe());
        tokio::time::sleep(POLL).await;
    }
});

step!(then_json_descending_by(w, m) {
    let objs = objects_at(w.last_json(), &m[1]);
    assert!(objs.len() >= 2, "JSON at {} has {} objects (expected at least 2 to compare)\n{}", m[1], objs.len(), w.last().describe());
    let vals: Vec<f64> = objs.iter().map(|o| util::query(o, &format!("$.{}", m[2])).ok().and_then(|v| v.first().and_then(|x| x.as_f64())).unwrap_or(f64::NEG_INFINITY)).collect();
    for (i, pair) in vals.windows(2).enumerate() {
        assert!(pair[0] >= pair[1], "JSON at {} is not descending by {} at index {i}: {vals:?}\n{}", m[1], m[2], w.last().describe());
    }
});

step!(then_json_count_between(w, m) {
    let (lo, hi): (usize, usize) = (m[2].parse().expect("min"), m[3].parse().expect("max"));
    let n = objects_at(w.last_json(), &m[1]).len();
    assert!(lo <= n && n <= hi, "JSON at {} has {n} elements (expected {lo}..={hi})\n{}", m[1], w.last().describe());
});

step!(then_within_metrics_file_lines(w, m) {
    let until = bound(w, &m[1]);
    let (stem, min): (String, usize) = (m[2].clone(), m[3].parse().expect("lines"));
    let name = format!("{stem}.ndjson");
    loop {
        let files: Vec<PathBuf> = find_files(&w.home, &|_| true)
            .into_iter()
            .filter(|p| p.file_name().is_some_and(|n| n == name.as_str()) && p.parent().and_then(|d| d.file_name()).is_some_and(|n| n == "metrics"))
            .collect();
        let lines = files.first().and_then(|f| std::fs::read_to_string(f).ok()).map_or(0, |t| {
            t.lines().filter(|l| serde_json::from_str::<Value>(l).is_ok_and(|v| v.get("rss_bytes").is_some())).count()
        });
        if lines >= min {
            return;
        }
        assert!(Instant::now() < until, "metrics file {name} under {} has {lines} sample line(s) after {}s (want at least {min}; files: {files:?})", w.home.display(), m[1]);
        tokio::time::sleep(POLL).await;
    }
});

/// Every step: (regex, function). Registered for Given, When and Then.
pub const STEPS: &[(&str, Step<E2eWorld>)] = &[
    // Given
    (r#"^the "([^"]+)" workspace$"#, given_workspace),
    (
        r#"^the broken workspace "([^"]+)"$"#,
        given_broken_workspace,
    ),
    (
        r#"^the fixture workspace "([^"]+)"$"#,
        given_fixture_workspace,
    ),
    (
        r#"^the "([^"]+)" workspace with its original ports$"#,
        given_workspace_original_ports,
    ),
    (
        r#"^the fixture workspace "([^"]+)" with its original ports$"#,
        given_fixture_workspace_original_ports,
    ),
    (
        r#"^the "([^"]+)" workspace with a local override setting ([^=\s]+)=(.*)$"#,
        given_workspace_with_override,
    ),
    (
        r#"^a local override setting ([^=\s]+)=(.*)$"#,
        given_local_override,
    ),
    (
        r#"^the local override "([^"]+)" is removed$"#,
        given_local_override_removed,
    ),
    (
        r#"^the local override file is extended with:$"#,
        given_local_override_extended,
    ),
    (
        r#"^the workspace has a private copy of the repos$"#,
        given_private_repos,
    ),
    (r#"^an empty directory$"#, given_empty_dir),
    (
        r#"^the "([^"]+)" workspace is up( in detached mode)?(?: with profile "([^"]+)")?$"#,
        given_workspace_up,
    ),
    (
        r#"^a stray process "([^"]+)" is running in a new process group$"#,
        given_stray_process,
    ),
    // When
    (r#"^I run "([^"]*)"$"#, when_run),
    (r#"^I run "([^"]*)" from "([^"]*)"$"#, when_run_from),
    (r#"^I run "([^"]*)" with env (.+)$"#, when_run_with_env),
    (
        r#"^I run "([^"]*)" from outside any workspace$"#,
        when_run_outside,
    ),
    (
        r#"^the chaos endpoint "([^"]+)" is called on "([^"]+)"$"#,
        when_chaos,
    ),
    (r#"^the daemon is killed with (\S+)$"#, when_daemon_killed),
    (r#"^the stray processes are stopped$"#, when_strays_stopped),
    (
        r#"^the stray processes are still running$"#,
        then_strays_running,
    ),
    // Then
    (r#"^the exit code is (\d+)$"#, then_exit_code),
    (r#"^the command succeeds$"#, then_succeeds),
    (r#"^the command fails$"#, then_fails),
    (r#"^the JSON at "([^"]+)" equals (.+)$"#, then_json_equals),
    (
        r#"^the JSON at "([^"]+)" does not equal (.+)$"#,
        then_json_not_equals,
    ),
    (
        r#"^the JSON at "([^"]+)" contains (.+)$"#,
        then_json_contains,
    ),
    (
        r#"^the JSON at "([^"]+)" matches semver$"#,
        then_json_semver,
    ),
    (r#"^the JSON at "([^"]+)" exists$"#, then_json_exists),
    (
        r#"^the JSON at "([^"]+)" does not exist$"#,
        then_json_not_exists,
    ),
    (
        r#"^the JSON error has code "([^"]+)"(?: and path "([^"]+)")?$"#,
        then_error_code,
    ),
    (
        r#"^the error message contains "(.*)"$"#,
        then_error_message_contains,
    ),
    (r#"^stdout contains "(.*)"$"#, then_stdout_contains),
    (r#"^stderr contains "(.*)"$"#, then_stderr_contains),
    (r#"^stdout is not JSON$"#, then_stdout_not_json),
    (
        r#"^the output matches golden "([^"]+)"(?: ignoring columns (.+))?$"#,
        then_golden,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s the stem "([^"]+)" is "([^"]+)"$"#,
        then_within_stem_status,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s the events stream contains (.+)$"#,
        then_within_events,
    ),
    (r#"^the chaos response status is (\d+)$"#, then_chaos_status),
    (
        r#"^the events stream contains (\{.*?\}) before (\{.*\})$"#,
        then_events_before,
    ),
    (
        r#"^no process from the workspace's process groups is alive$"#,
        then_no_process_alive,
    ),
    (
        r#"^no container with label stems\.workspace=(\S+) exists$"#,
        then_no_container,
    ),
    (
        r#"^the container "([^"]+)" is running with label "([^"=]+)=([^"]*)"$"#,
        then_container_running_with_label,
    ),
    (
        r#"^a container "([^"]+)" is running with labels "([^"]+)"$"#,
        given_container_with_labels,
    ),
    (
        r#"^the docker volume "([^"]+)" (exists|does not exist)$"#,
        then_docker_volume,
    ),
    (
        r#"^the compose project "([^"]+)" has service "([^"]+)" running$"#,
        then_compose_service_running,
    ),
    (r#"^the state file contains no stems$"#, then_state_no_stems),
    (r#"^the state file is valid JSON$"#, then_state_valid_json),
    (
        r#"^the state file records stem "([^"]+)" at the stray process with a wrong start time$"#,
        given_state_records_stray,
    ),
    (
        r#"^the daemon is killed with SIGKILL during "([^"]*)" once it prints "([^"]+)", (\d+) times$"#,
        when_daemon_killed_during,
    ),
    (
        r#"^the lock file and socket do not exist$"#,
        then_no_lock_socket,
    ),
    (r#"^the file "([^"]+)" exists$"#, then_file_exists),
    (
        r#"^the file "([^"]+)" does not exist$"#,
        then_file_not_exists,
    ),
    // Daemon, RPCs, background commands (08/09)
    (
        r#"^the daemon is started( with debug RPCs)?$"#,
        given_daemon_started,
    ),
    (
        r#"^a stale daemon lock from a dead process( and its socket)?$"#,
        given_stale_lock,
    ),
    (r#"^I call the daemon RPC "([^"]+)" with (.+)$"#, when_rpc),
    (
        r#"^I save the JSON at "([^"]+)" as "([\w-]+)"$"#,
        when_save_json,
    ),
    (
        r#"^I run "([^"]*)" in the background$"#,
        when_run_background,
    ),
    (
        r#"^the background command is stopped$"#,
        when_background_stopped,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s the background command's output contains (\[.+\]) in order$"#,
        then_background_in_order,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s the background command exits with code (\d+)$"#,
        then_background_exits,
    ),
    (
        r#"^the chaos endpoint "([^"]+)" is called on port (\S+)$"#,
        when_chaos_port,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s port (\S+) is listening$"#,
        then_port_listening,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s the background command's stdout contains "(.*)"$"#,
        then_background_stdout_contains,
    ),
    (
        r#"^the daemon RSS is below (\d+(?:\.\d+)?) MB$"#,
        then_daemon_rss_below,
    ),
    (
        r#"^the last command took less than (\d+) ms$"#,
        then_last_took_less,
    ),
    (
        r#"^the last command took at least (\d+) ms$"#,
        then_last_took_at_least,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s the lock file and socket do not exist$"#,
        then_within_no_lock_socket,
    ),
    (r#"^the socket has mode ([0-7]{3,4})$"#, then_socket_mode),
    (
        r#"^within (\d+(?:\.\d+)?)s the daemon log contains "(.*)"$"#,
        then_daemon_log_contains,
    ),
    (
        r#"^the JSON at "([^"]+)" is greater than (-?\d+(?:\.\d+)?)$"#,
        then_json_greater,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s none of the pids (.+) is alive$"#,
        then_pids_dead,
    ),
    // Logs (12)
    (
        r#"^the archive "([^"]+)" contains "([^"]+)"$"#,
        then_archive_contains,
    ),
    (
        r#"^the archive "([^"]+)" entry "([^"]+)" contains "(.*)"$"#,
        then_archive_entry_contains,
    ),
    (
        r#"^the archive "([^"]+)" entry "([^"]+)" does not contain "(.*)"$"#,
        then_archive_entry_not_contains,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s the stem log file "([^"/]+/[^"]+)" contains "(.*)"$"#,
        then_stem_log_file_contains,
    ),
    (
        r#"^the stem log directory "([^"]+)" holds at most (\d+) files$"#,
        then_stem_log_dir_at_most,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s the output of "([^"]*)" has no JSON at "([^"]+)"$"#,
        then_within_output_no_json,
    ),
    (
        r#"^the JSON at "([^"]+)" is in ascending order$"#,
        then_json_ascending,
    ),
    (
        r#"^the JSON nodes at "([^"]+)" equal (.+)$"#,
        then_json_nodes_equal,
    ),
    (
        r#"^the background command's CPU is below (\d+(?:\.\d+)?) %$"#,
        then_background_cpu_below,
    ),
    // Scripts (16)
    (
        r#"^the file "([^"]+)" is written with "(.*)"$"#,
        when_file_written,
    ),
    (
        r#"^the file "([^"]+)" contains "(.*)"$"#,
        then_file_contains,
    ),
    (
        r#"^the local override file contains:$"#,
        given_local_file_docstring,
    ),
    (
        r#"^the events stream does not contain (.+)$"#,
        then_events_not_contain,
    ),
    (r#"^I run the shell command "([^"]*)"$"#, when_run_shell),
    (
        r#"^a bare git repository "([\w-]+)" made from "([^"]+)"$"#,
        given_bare_git_repo,
    ),
    // Overlays (18)
    (
        r#"^the repo copy is a git repository with "([^"]+)" committed$"#,
        given_repo_copy_git,
    ),
    // Doctor (19)
    (
        r#"^a fake tool "([\w.-]+)" on PATH that runs "(.*)"$"#,
        given_fake_tool,
    ),
    (
        r#"^the state file records an overlay for stem "([^"]+)" at "([^"]+)"$"#,
        given_state_records_overlay,
    ),
    // Health (21)
    (
        r#"^there are exactly (\d+) events matching (.+)$"#,
        then_exactly_events,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s there are at least (\d+) events matching (.+)$"#,
        then_within_at_least_events,
    ),
    // Restart policies (22)
    (
        r#"^during (\d+(?:\.\d+)?)s the events stream never contains (.+)$"#,
        then_during_events_never,
    ),
    // Watchdogs (24)
    (
        r#"^during (\d+(?:\.\d+)?)s there are at most (\d+) events matching (.+)$"#,
        then_during_at_most_events,
    ),
    (
        r#"^the first event matching (\{.*?\}) is followed by one matching (\{.*\}) after (\d+) to (\d+) ms$"#,
        then_event_followed_after,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s the JSON at "([^"]+)" of "(stems [^"]*)" equals (.+)$"#,
        then_within_json_of,
    ),
    (
        r#"^the daemon's CPU is below (\d+(?:\.\d+)?) %$"#,
        then_daemon_cpu_below,
    ),
    (
        r#"^a workspace with (\d+) process stems using tcp health every (\d+)ms$"#,
        given_generated_probe_workspace,
    ),
    // TUI (27)
    (
        r#"^the frame matches golden "([^"]+)"(?: masking ([\w,]+))?$"#,
        then_frame_golden,
    ),
    (
        r#"^the last frame contains "(.*)"$"#,
        then_last_frame_contains,
    ),
    (
        r#"^the last frame does not contain "(.*)"$"#,
        then_last_frame_not_contains,
    ),
    (r#"^frame (\d+) contains "(.*)"$"#, then_frame_n_contains),
    (
        r#"^frame (\d+) does not contain "(.*)"$"#,
        then_frame_n_not_contains,
    ),
    (
        r#"^stdout contains the terminal restore sequence$"#,
        then_stdout_restore_sequence,
    ),
    (
        r#"^stdout contains the OSC 52 sequence for "(.*)"$"#,
        then_stdout_osc52,
    ),
    (
        r#"^stdout does not contain "(.*)"$"#,
        then_stdout_not_contains,
    ),
    // Metrics (25)
    (
        r#"^within (\d+(?:\.\d+)?)s the JSON at "([^"]+)" of "(stems [^"]*)" is greater than (\S+)$"#,
        then_within_json_of_greater,
    ),
    (
        r#"^some sample in the JSON at "([^"]+)" has "([\w.]+)" greater than (\S+)$"#,
        then_some_sample_greater,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s some sample in the JSON at "([^"]+)" of "(stems [^"]*)" has "([\w.]+)" greater than (\S+)$"#,
        then_within_some_sample_greater,
    ),
    (
        r#"^the JSON at "([^"]+)" is in descending order by "([\w.]+)"$"#,
        then_json_descending_by,
    ),
    (
        r#"^the JSON at "([^"]+)" has between (\d+) and (\d+) elements$"#,
        then_json_count_between,
    ),
    (
        r#"^within (\d+(?:\.\d+)?)s the metrics file of "([^"]+)" has at least (\d+) lines$"#,
        then_within_metrics_file_lines,
    ),
];

/// The step collection handed to cucumber.
pub fn collection() -> Collection<E2eWorld> {
    let mut c = Collection::new();
    for (re, f) in STEPS.iter().chain(crate::mcp::STEPS) {
        let regex = regex::Regex::new(re).unwrap_or_else(|e| panic!("bad step regex {re}: {e}"));
        c = c
            .given(None, regex.clone(), *f)
            .when(None, regex.clone(), *f)
            .then(None, regex, *f);
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_time_and_base64() {
        let f = " a 12:00:01.234 x\n12:00:01 stem.state\n";
        assert_eq!(
            mask_frame(f, &[]),
            " a 12:00:01.234 x\n12:00:01 stem.state\n"
        );
        assert_eq!(
            mask_frame(f, &["TIME".to_owned()]),
            " a **:**:**.*** x\n**:**:** stem.state\n"
        );
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // Status bars differing only by the pid's width compare equal.
        let a = " ws · profile - · daemon pid 4242 · ✓1 !0 ✗0 ·0 ?0 ↻0 Space pause · ? help…\n";
        let b = " ws · profile - · daemon pid 94242 · ✓1 !0 ✗0 ·0 ?0 ↻0 Space pause · ? hel…\n";
        assert_eq!(mask_frame(a, &[]), mask_frame(b, &[]));
        assert_eq!(
            mask_frame(a, &[]),
            " ws · profile - · daemon pid * · ✓1 !0 ✗0 ·0 ?0 ↻0\n"
        );
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
    }

    #[test]
    fn events_of_accepts_ndjson_envelopes_and_single_lines() {
        use serde_json::json;
        let one = json!({"seq": 1, "kind": "daemon.started", "data": {}});
        assert_eq!(events_of(&one), vec![one.clone()]);
        let many = json!([one, {"seq": 2, "kind": "workspace.loaded", "data": {"x": 1}}]);
        assert_eq!(events_of(&many).len(), 2);
        let env = json!({"ok": true, "data": [{"seq": 1, "kind": "k", "data": {}}]});
        assert_eq!(events_of(&env).len(), 1);
        assert_eq!(
            background_events("{\"a\":1}\nnot json\n\n{\"a\":2}\n"),
            vec![json!({"a": 1}), json!({"a": 2})]
        );
    }

    #[test]
    fn frames_are_split_and_masked() {
        let out = "noise\n--- frame 1 ---\n  STEM  STATUS  PID    UPTIME\n› a     ok      123    3s\n\n daemon pid 42 · x\n--- frame 2 ---\nsecond\n";
        let f = frames_in(out);
        assert_eq!(f.len(), 2);
        assert_eq!(f[1], "second");
        let masked = mask_frame(&f[0], &["PID".to_owned(), "UPTIME".to_owned()]);
        assert_eq!(
            masked,
            "  STEM  STATUS  PID    UPTIME\n› a     ok      *      *\n\n daemon pid * · x\n"
        );
        let other =
            "  STEM  STATUS  PID    UPTIME\n› a     ok      98765  1m02s\n\n daemon pid 7 · x\n";
        assert_eq!(
            mask_frame(other, &["PID".to_owned(), "UPTIME".to_owned()]),
            masked
        );
    }

    /// No step text may match two regexes (cucumber would report ambiguity).
    #[test]
    fn step_regexes_are_unambiguous() {
        let samples = [
            r#"the "minimal" workspace"#,
            r#"the broken workspace "cycle""#,
            r#"the "minimal" workspace with its original ports"#,
            r#"the fixture workspace "include-demo""#,
            r#"the fixture workspace "two-errors" with its original ports"#,
            r#"the "minimal" workspace with a local override setting stems.echo-svc.enabled=false"#,
            r#"a local override setting profiles.default=backend"#,
            r#"the local override file contains:"#,
            r#"the local override file is extended with:"#,
            r#"the local override "stems.extra" is removed"#,
            r#"the workspace has a private copy of the repos"#,
            r#"an empty directory"#,
            r#"the "minimal" workspace is up"#,
            r#"the "hello-shop" workspace is up in detached mode with profile "backend""#,
            r#"a stray process "sleep 300" is running in a new process group"#,
            r#"I run "stems validate --json""#,
            r#"I run "stems validate --json" from "sub/dir""#,
            r#"I run "stems validate --json" with env STEMS_WORKSPACE=${ws} FOO=1"#,
            r#"I run "stems validate --json" from outside any workspace"#,
            r#"the chaos endpoint "crash" is called on "echo-svc""#,
            r#"the daemon is killed with SIGKILL"#,
            r#"the stray processes are stopped"#,
            r#"the exit code is 2"#,
            r#"the command succeeds"#,
            r#"the command fails"#,
            r#"the JSON at "$.version" equals "0.1.0""#,
            r#"the JSON at "$.errors" contains {"code": "CYCLE"}"#,
            r#"the JSON at "$.version" matches semver"#,
            r#"the JSON at "$.data" exists"#,
            r#"the JSON at "$.data.x" does not exist"#,
            r#"the JSON error has code "CYCLE""#,
            r#"the JSON error has code "CYCLE" and path "stems.a.depends_on""#,
            r#"the error message contains "a -> b -> a""#,
            r#"stdout contains "pong""#,
            r#"stderr contains "warning""#,
            r#"stdout is not JSON"#,
            r#"the output matches golden "status-table""#,
            r#"the output matches golden "status-table" ignoring columns PID,UPTIME"#,
            r#"within 5s the stem "echo-svc" is "healthy""#,
            r#"within 2.5s the events stream contains {"kind": "daemon.started"}"#,
            r#"the chaos response status is 200"#,
            r#"no process from the workspace's process groups is alive"#,
            r#"no container with label stems.workspace=hello-shop exists"#,
            r#"the container "docker-pg-db" is running with label "stems.workspace=docker-pg""#,
            r#"a container "docker-pg-stray" is running with labels "stems.workspace=docker-pg,stems.stem=db""#,
            r#"the docker volume "docker-pg_pgdata" exists"#,
            r#"the docker volume "docker-pg_pgdata" does not exist"#,
            r#"the compose project "stems-compose-redis" has service "redis" running"#,
            r#"the state file contains no stems"#,
            r#"the state file is valid JSON"#,
            r#"the state file records stem "echo-svc" at the stray process with a wrong start time"#,
            r#"the daemon is killed with SIGKILL during "stems up --detach --json" once it prints "up.started", 5 times"#,
            r#"the lock file and socket do not exist"#,
            r#"the file "config/local.ini" exists"#,
            r#"the file "config/local.ini" does not exist"#,
            r#"the daemon is started"#,
            r#"the daemon is started with debug RPCs"#,
            r#"a stale daemon lock from a dead process"#,
            r#"a stale daemon lock from a dead process and its socket"#,
            r#"I call the daemon RPC "_debug.describe" with {"handle": 1}"#,
            r#"I save the JSON at "$.result.id" as "handle""#,
            r#"I run "stems events -f --json" in the background"#,
            r#"the background command is stopped"#,
            r#"within 5s the background command's output contains [{"kind": "daemon.started"}] in order"#,
            r#"within 5s the background command exits with code 0"#,
            r#"the chaos endpoint "fork?n=3" is called on port ${port:18090}"#,
            r#"within 5s port ${port:18090} is listening"#,
            r#"within 1s the background command's stdout contains "- stopped""#,
            r#"the daemon RSS is below 20 MB"#,
            r#"the last command took less than 100 ms"#,
            r#"the socket has mode 0600"#,
            r#"the last command took at least 400 ms"#,
            r#"within 2s the lock file and socket do not exist"#,
            r#"within 2s the daemon log contains "stale lock reclaimed""#,
            r#"the JSON at "$.result.dropped_lines" is greater than 0"#,
            r#"within 3s none of the pids ${var:pids} is alive"#,
            r#"the JSON at "$.data.stems[0].pid" does not equal ${var:pid}"#,
            r#"the events stream contains {"stem": "a", "to": "healthy"} before {"stem": "b", "to": "starting"}"#,
            r#"the stray processes are still running"#,
            r#"the archive "bundle.tar.gz" contains "logs/echo-svc/""#,
            r#"the archive "bundle.tar.gz" entry "config.json" contains "<redacted>""#,
            r#"the archive "bundle.tar.gz" entry "config.json" does not contain "hunter2""#,
            r#"within 5s the stem log file "echo-svc/current.log" contains "chaos log line 49""#,
            r#"the stem log directory "echo-svc" holds at most 3 files"#,
            r#"within 5s the output of "stems logs --json --since 1s" has no JSON at "$[?@.level == 'warn']""#,
            r#"the JSON at "$[*].ts" is in ascending order"#,
            r#"the JSON nodes at "$[*].text" equal ["a", "b"]"#,
            r#"the background command's CPU is below 2 %"#,
            r#"the file "${tmp}/examples/repos/shop-api/VERSION" is written with "2""#,
            r#"the file "hooks.log" contains "pre_start""#,
            r#"the events stream does not contain {"kind": "script.started"}"#,
            r#"I run the shell command "docker ps""#,
            r#"the repo copy is a git repository with "shop-api/config/local.ini" committed"#,
            r#"a bare git repository "shop" made from "examples/repos/shop-api""#,
            r#"a fake tool "vite" on PATH that runs "echo fake vite""#,
            r#"the state file records an overlay for stem "shop-api" at "config/local.ini""#,
            r#"there are exactly 2 events matching {"kind": "stem.health"}"#,
            r#"within 3s there are at least 4 events matching {"kind": "stem.health"}"#,
            r#"during 3s the events stream never contains {"kind": "stem.restarting"}"#,
            r#"during 2s there are at most 1 events matching {"kind": "watch.triggered"}"#,
            r#"the first event matching {"kind": "stem.restarting"} is followed by one matching {"to": "healthy"} after 400 to 9000 ms"#,
            r#"within 2s the JSON at "$.data.stems[0].degraded" of "stems status --json" equals false"#,
            r#"the daemon's CPU is below 5 %"#,
            r#"a workspace with 20 process stems using tcp health every 200ms"#,
            r#"the frame matches golden "tui-table-minimal""#,
            r#"the frame matches golden "tui-table-minimal" masking PID,UPTIME"#,
            r#"the last frame contains "[Detail]""#,
            r#"the last frame does not contain "a""#,
            r#"frame 2 contains "[Detail]""#,
            r#"frame 2 does not contain "Help""#,
            r#"stdout contains the terminal restore sequence"#,
            r#"stdout contains the OSC 52 sequence for "INFO chaos log line 2""#,
            r#"stdout does not contain "--- frame 1 ---""#,
            r#"within 5s the JSON at "$.data.stems[0].latest.rss_bytes" of "stems metrics --json" is greater than ${var:rss}+104857600"#,
            r#"some sample in the JSON at "$.data.stems[0].history" has "cpu_pct" greater than 50"#,
            r#"within 5s some sample in the JSON at "$.data.stems[0].history" of "stems metrics --history 10s --json" has "cpu_pct" greater than 50"#,
            r#"the JSON at "$.data.stems" is in descending order by "latest.rss_bytes""#,
            r#"the JSON at "$.data.stems[0].history" has between 1 and 40 elements"#,
            r#"within 3s the metrics file of "echo-svc" has at least 2 lines"#,
        ];
        let regexes: Vec<regex::Regex> = STEPS
            .iter()
            .map(|(r, _)| regex::Regex::new(r).unwrap())
            .collect();
        for s in samples {
            let hits = regexes.iter().filter(|r| r.is_match(s)).count();
            assert_eq!(hits, 1, "{s:?} matched {hits} step regexes");
        }
        assert_eq!(
            STEPS.len(),
            // +1 variant: the docker volume step's `exists`/`does not exist` (14).
            samples.len() - 7,
            "every step has a sample (plus optional-group variants)"
        );
    }
}
