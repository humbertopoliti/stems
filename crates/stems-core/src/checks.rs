//! User-defined `checks:` (FR-CL-3, deliverable 48): enumeration, execution
//! and error shaping shared by `doctor`, `stems up` and `validate`.
//!
//! A check is a shell command: exit 0 passes. It runs in its own process
//! group (killed on timeout), with the workspace root as the working
//! directory (a stem's check: its codebase) and the workspace env (a stem's
//! check: the stem's env, which already folds the workspace's) over the
//! caller's environment.

use std::path::PathBuf;

use serde_json::json;
use stems_config::{
    CHECK_MAX_TIMEOUT, CheckSeverity, CheckWhen, ConfigPath, CustomCheckSpec, Workspace,
};

use crate::error::{Error, ErrorCode};
use crate::tools::{CheckRun, run_check_command};
use crate::validate::requires_dir;

/// One `checks:` entry with where it runs.
#[derive(Clone, Debug)]
pub struct CheckEntry<'a> {
    /// The stem, for a stem-level check.
    pub stem: Option<&'a str>,
    /// The check's name.
    pub name: &'a str,
    /// Its definition.
    pub spec: &'a CustomCheckSpec,
    /// Working directory.
    pub dir: PathBuf,
    /// Environment added over the caller's.
    pub env: Vec<(String, String)>,
}

impl CheckEntry<'_> {
    /// Doctor id: `check.<name>` or `check.<stem>.<name>`.
    pub fn id(&self) -> String {
        match self.stem {
            Some(s) => format!("check.{s}.{}", self.name),
            None => format!("check.{}", self.name),
        }
    }

    /// Config path of the entry.
    pub fn path(&self) -> ConfigPath {
        checks_path(self.stem, self.name)
    }

    /// Run the check command.
    pub fn run(&self) -> CheckRun {
        run_check_command(
            &self.spec.command,
            self.spec.timeout.as_duration(),
            &self.dir,
            &self.env,
        )
    }

    /// Run the `fix` command, if any (same directory, env and timeout).
    pub fn run_fix(&self) -> Option<CheckRun> {
        self.spec
            .fix
            .as_ref()
            .map(|f| run_check_command(f, self.spec.timeout.as_duration(), &self.dir, &self.env))
    }

    /// One-line failure message of `run`.
    pub fn failure_message(&self, run: &CheckRun) -> String {
        let base = self
            .spec
            .message
            .clone()
            .unwrap_or_else(|| "check failed".to_string());
        if run.timed_out {
            format!(
                "{base} (timed out after {}s)",
                self.spec.timeout.as_duration().as_secs_f64()
            )
        } else {
            base
        }
    }

    /// The `CHECK_FAILED` error of a failed `run`.
    pub fn error(&self, run: &CheckRun) -> Error {
        let mut e = Error::new(
            ErrorCode::CheckFailed,
            format!(
                "check `{}` failed: {}",
                self.id().trim_start_matches("check."),
                self.failure_message(run)
            ),
        )
        .with_path(self.path())
        .with_details(json!({
            "check": self.id(),
            "exit": run.exit,
            "tail": run.tail,
        }));
        if let Some(h) = &self.spec.hint {
            e = e.with_hint(h.clone());
        }
        e
    }
}

/// `checks.<name>` or `stems.<stem>.checks.<name>`.
pub fn checks_path(stem: Option<&str>, name: &str) -> ConfigPath {
    let base = match stem {
        Some(s) => ConfigPath::root().key("stems").key(s),
        None => ConfigPath::root(),
    };
    base.key("checks").key(name)
}

/// Every check of `ws` (the workspace's, then each enabled stem's) that runs
/// `at`.
pub fn entries(ws: &Workspace, at: CheckWhen) -> Vec<CheckEntry<'_>> {
    let env_of = |m: &indexmap::IndexMap<String, String>| -> Vec<(String, String)> {
        m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    };
    let mut out = Vec::new();
    for (name, spec) in ws.checks.iter().filter(|(_, c)| c.runs_on(at)) {
        out.push(CheckEntry {
            stem: None,
            name,
            spec,
            dir: ws.root.clone(),
            env: env_of(&ws.env),
        });
    }
    for stem in ws.stems().filter(|s| s.enabled) {
        for (name, spec) in stem.checks.iter().filter(|(_, c)| c.runs_on(at)) {
            out.push(CheckEntry {
                stem: Some(&stem.name),
                name,
                spec,
                dir: requires_dir(stem, &ws.root).to_path_buf(),
                env: env_of(&stem.env),
            });
        }
    }
    out
}

/// Run the checks that apply `at`; returns each entry with its failed run
/// (passing checks are dropped).
pub fn failures(ws: &Workspace, at: CheckWhen) -> Vec<(CheckEntry<'_>, CheckRun)> {
    run_failures(entries(ws, at))
}

/// Run `entries` (already filtered by the caller); returns each with its
/// failed run (passing checks are dropped).
pub fn run_failures(entries: Vec<CheckEntry<'_>>) -> Vec<(CheckEntry<'_>, CheckRun)> {
    entries
        .into_iter()
        .filter_map(|e| {
            let r = e.run();
            (!r.passed()).then_some((e, r))
        })
        .collect()
}

/// Run the `validate` checks once and split the failures by severity:
/// `(errors, warnings)` — `fail` checks are errors, `warn` checks warnings.
/// Only `stems validate` calls this ([`crate::validate::validate`] never runs
/// user checks: it backs every daemon config load).
pub fn validate_results(ws: &Workspace) -> (Vec<Error>, Vec<Error>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for (e, r) in failures(ws, CheckWhen::Validate) {
        match e.spec.severity {
            CheckSeverity::Fail => errors.push(e.error(&r)),
            CheckSeverity::Warn => warnings.push(e.error(&r)),
        }
    }
    (errors, warnings)
}

/// Shape errors of `checks:` (always run): empty `command`, empty `when`,
/// `timeout` of zero or over 30s.
pub fn shape_errors(ws: &Workspace) -> Vec<Error> {
    let mut out = Vec::new();
    let mut check = |stem: Option<&str>, name: &str, spec: &CustomCheckSpec| {
        let path = checks_path(stem, name);
        let mut bad = |what: &str, hint: &str| {
            out.push(
                Error::new(ErrorCode::SchemaInvalid, format!("{path}: {what}"))
                    .with_path(path.clone())
                    .with_hint(hint.to_string()),
            );
        };
        if spec.command.trim().is_empty() {
            bad(
                "`command` is empty",
                "set the shell command to run; exit 0 means ok",
            );
        }
        if spec.when.is_empty() {
            bad(
                "`when` is empty",
                "list where it runs: `when: [doctor, up]` (add `validate` to run it in `stems validate`)",
            );
        }
        let t = spec.timeout.as_duration();
        if t.is_zero() || t > CHECK_MAX_TIMEOUT {
            bad(
                &format!("`timeout` {} is outside 0 < timeout <= 30s", spec.timeout),
                "use a timeout such as `5s` (at most `30s`)",
            );
        }
    };
    for (name, spec) in &ws.checks {
        check(None, name, spec);
    }
    for stem in ws.stems() {
        for (name, spec) in &stem.checks {
            check(Some(&stem.name), name, spec);
        }
    }
    out
}
