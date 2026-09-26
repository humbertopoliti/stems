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
use crate::{golden, hooks, http, procs, util};

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
        .arg(&m[1])
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
    golden::check(&dir, &m[1], &w.last().stdout, &ignore).unwrap_or_else(|e| panic!("{e}"));
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
    // Then
    (r#"^the exit code is (\d+)$"#, then_exit_code),
    (r#"^the command succeeds$"#, then_succeeds),
    (r#"^the command fails$"#, then_fails),
    (r#"^the JSON at "([^"]+)" equals (.+)$"#, then_json_equals),
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
        r#"^no process from the workspace's process groups is alive$"#,
        then_no_process_alive,
    ),
    (
        r#"^no container with label stems\.workspace=(\S+) exists$"#,
        then_no_container,
    ),
    (r#"^the state file contains no stems$"#, then_state_no_stems),
    (
        r#"^the lock file and socket do not exist$"#,
        then_no_lock_socket,
    ),
    (r#"^the file "([^"]+)" exists$"#, then_file_exists),
    (
        r#"^the file "([^"]+)" does not exist$"#,
        then_file_not_exists,
    ),
];

/// The step collection handed to cucumber.
pub fn collection() -> Collection<E2eWorld> {
    let mut c = Collection::new();
    for (re, f) in STEPS {
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
            r#"the state file contains no stems"#,
            r#"the lock file and socket do not exist"#,
            r#"the file "config/local.ini" exists"#,
            r#"the file "config/local.ini" does not exist"#,
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
            samples.len() - 3,
            "every step has a sample (plus optional-group variants)"
        );
    }
}
