//! The output layer: every command returns a [`CommandOutput`], rendered as
//! the JSON envelope `{ "ok", "data", "errors", "version" }` or as human
//! text. The exit code is `errors.exit_code()` (0 when there are none).
//!
//! **Exception: NDJSON.** `stems events --json` prints one event object per
//! line and no envelope (FR-CL-4: the stream is the hook for external
//! automation, and `-f` must be consumable line by line while it runs). A
//! command opts in with [`CommandOutput::with_ndjson`]; on failure the
//! envelope is printed as usual, so errors stay machine-readable.
//!
//! `stems up --json` (and `attach --json`) also stream NDJSON events while
//! they run and then print the envelope as the **last line**, compact
//! ([`CommandOutput::compact`]); a reader takes the last line as the result.

use std::io::Write;

use serde_json::{Value, json};
use stems_core::{Error, Errors};

/// Version of this binary (the envelope's `version`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How to render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The JSON envelope.
    Json,
    /// Human text on stdout, errors on stderr.
    Human,
}

/// Rendering options derived from the global flags and the terminal.
#[derive(Clone, Copy, Debug)]
pub struct OutputOptions {
    /// Selected mode.
    pub mode: Mode,
    /// `--json` was passed explicitly (raw outputs such as completion
    /// scripts are only wrapped in the envelope then).
    pub json_explicit: bool,
    /// `--quiet`: no human stdout on success.
    pub quiet: bool,
    /// `--no-color` / `STEMS_NO_COLOR`.
    pub no_color: bool,
}

impl OutputOptions {
    /// `--json` wins, then `--human`; otherwise JSON iff stdout is not a TTY.
    pub fn new(json: bool, human: bool, stdout_is_tty: bool) -> Self {
        let mode = if json || (!human && !stdout_is_tty) {
            Mode::Json
        } else {
            Mode::Human
        };
        Self {
            mode,
            json_explicit: json,
            quiet: false,
            no_color: false,
        }
    }
}

/// What a command produced.
#[derive(Clone, Debug, Default)]
pub struct CommandOutput {
    /// The envelope's `data` (`null` when a command failed before producing any).
    pub data: Value,
    /// Human rendering of `data`; when `None`, `data` is printed as YAML.
    pub human: Option<String>,
    /// Errors; empty means success.
    pub errors: Errors,
    /// Text printed verbatim (no envelope) unless `--json` was explicit, e.g.
    /// a completion script or `stems --version`.
    pub raw: Option<String>,
    /// JSON mode prints this verbatim (NDJSON lines) instead of the envelope
    /// when there are no errors (`stems events`).
    pub ndjson: Option<String>,
    /// Exit code overriding the one derived from `errors` (`up`: 3 when
    /// some stems failed and others are ready).
    pub exit: Option<i32>,
    /// Print the envelope on one line (it ends an NDJSON stream: `up --json`).
    pub compact: bool,
}

impl CommandOutput {
    /// Successful output with `data`.
    pub fn data(data: Value) -> Self {
        Self {
            data,
            ..Self::default()
        }
    }

    /// Failed output (`data: null`).
    pub fn failed(errors: impl Into<Errors>) -> Self {
        Self {
            errors: errors.into(),
            ..Self::default()
        }
    }

    /// Set the human text.
    #[must_use]
    pub fn with_human(mut self, text: impl Into<String>) -> Self {
        self.human = Some(text.into());
        self
    }

    /// Set the raw text.
    #[must_use]
    pub fn with_raw(mut self, text: impl Into<String>) -> Self {
        self.raw = Some(text.into());
        self
    }

    /// Set the NDJSON text printed instead of the envelope in JSON mode.
    #[must_use]
    pub fn with_ndjson(mut self, text: impl Into<String>) -> Self {
        self.ndjson = Some(text.into());
        self
    }

    /// Set the errors.
    #[must_use]
    pub fn with_errors(mut self, errors: impl Into<Errors>) -> Self {
        self.errors = errors.into();
        self
    }

    /// Set an explicit exit code.
    #[must_use]
    pub fn with_exit(mut self, code: i32) -> Self {
        self.exit = Some(code);
        self
    }

    /// Print the envelope on a single line.
    #[must_use]
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// Process exit code.
    pub fn exit_code(&self) -> i32 {
        self.exit.unwrap_or_else(|| self.errors.exit_code())
    }

    /// The JSON envelope.
    pub fn envelope(&self) -> Value {
        json!({
            "ok": self.errors.is_empty(),
            "data": self.data,
            "errors": self.errors,
            "version": VERSION,
        })
    }
}

impl From<Error> for CommandOutput {
    fn from(e: Error) -> Self {
        Self::failed(e)
    }
}

impl From<Errors> for CommandOutput {
    fn from(e: Errors) -> Self {
        Self::failed(e)
    }
}

/// Render `out` to `stdout`/`stderr`. Write errors (e.g. a closed pipe) are
/// ignored: the exit code still reports the command's outcome.
pub fn render(
    out: &CommandOutput,
    opts: &OutputOptions,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) {
    if let Some(raw) = &out.raw
        && !opts.json_explicit
        && out.errors.is_empty()
    {
        let _ = stdout.write_all(raw.as_bytes());
        let _ = stdout.flush();
        return;
    }
    match opts.mode {
        Mode::Json if out.ndjson.is_some() && out.errors.is_empty() => {
            let _ = stdout.write_all(out.ndjson.as_deref().unwrap_or_default().as_bytes());
            let _ = stdout.flush();
        }
        Mode::Json => {
            let env = out.envelope();
            let text = if out.compact {
                serde_json::to_string(&env)
            } else {
                serde_json::to_string_pretty(&env)
            }
            .unwrap_or_else(|e| format!("{{\"ok\":false,\"error\":\"{e}\"}}"));
            let _ = writeln!(stdout, "{text}");
            let _ = stdout.flush();
        }
        Mode::Human => {
            if !opts.quiet {
                let text = match &out.human {
                    Some(t) => t.clone(),
                    None if out.data.is_null() => String::new(),
                    None => human_value(&out.data),
                };
                if !text.is_empty() {
                    let _ = stdout.write_all(text.as_bytes());
                    if !text.ends_with('\n') {
                        let _ = stdout.write_all(b"\n");
                    }
                }
            }
            let _ = stdout.flush();
            let _ = stderr.write_all(human_errors(&out.errors).as_bytes());
            let _ = stderr.flush();
        }
    }
}

/// `data` as YAML (the default human rendering).
pub fn human_value(v: &Value) -> String {
    serde_yaml_ng::to_string(v).unwrap_or_else(|_| format!("{v:#}\n"))
}

/// Errors as human text, one block per error.
pub fn human_errors(errors: &Errors) -> String {
    let mut s = String::new();
    for e in errors {
        s.push_str("error: ");
        s.push_str(&e.to_string());
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use stems_core::ErrorCode;

    #[test]
    fn json_is_implied_without_a_tty_unless_human() {
        assert_eq!(OutputOptions::new(false, false, false).mode, Mode::Json);
        assert_eq!(OutputOptions::new(false, true, false).mode, Mode::Human);
        assert_eq!(OutputOptions::new(false, false, true).mode, Mode::Human);
        assert_eq!(OutputOptions::new(true, false, true).mode, Mode::Json);
    }

    #[test]
    fn envelope_shape_on_success() {
        let out = CommandOutput::data(json!({"x": 1}));
        assert_eq!(
            out.envelope(),
            json!({"ok": true, "data": {"x": 1}, "errors": [], "version": VERSION})
        );
        assert_eq!(out.exit_code(), 0);
    }

    #[test]
    fn envelope_shape_on_error() {
        let out = CommandOutput::failed(Error::usage("bad flag", "see --help"));
        let env = out.envelope();
        assert_eq!(env["ok"], json!(false));
        assert_eq!(env["data"], Value::Null);
        assert_eq!(
            env["errors"][0],
            json!({
                "code": "USAGE",
                "message": "bad flag",
                "path": null,
                "location": null,
                "hint": "see --help",
                "details": {},
            })
        );
        assert_eq!(out.exit_code(), 2);
    }

    #[test]
    fn exit_code_is_the_highest() {
        let out = CommandOutput::failed(vec![
            Error::new(ErrorCode::NotImplemented, "a"),
            Error::new(ErrorCode::Cycle, "b"),
        ]);
        assert_eq!(out.exit_code(), 2);
    }

    #[test]
    fn raw_is_verbatim_unless_json_explicit() {
        let out = CommandOutput::data(json!({"script": "x"})).with_raw("#compdef stems\n");
        let (mut o, mut e) = (Vec::new(), Vec::new());
        render(
            &out,
            &OutputOptions::new(false, false, false),
            &mut o,
            &mut e,
        );
        assert_eq!(String::from_utf8(o).unwrap(), "#compdef stems\n");
        let (mut o, mut e) = (Vec::new(), Vec::new());
        render(
            &out,
            &OutputOptions::new(true, false, false),
            &mut o,
            &mut e,
        );
        let v: Value = serde_json::from_slice(&o).unwrap();
        assert_eq!(v["data"]["script"], json!("x"));
    }

    #[test]
    fn ndjson_replaces_the_envelope_unless_there_are_errors() {
        let out = CommandOutput::data(json!([1])).with_ndjson("{\"seq\":1}\n");
        let (mut o, mut e) = (Vec::new(), Vec::new());
        render(
            &out,
            &OutputOptions::new(true, false, false),
            &mut o,
            &mut e,
        );
        assert_eq!(String::from_utf8(o).unwrap(), "{\"seq\":1}\n");
        let out = out.with_errors(Error::usage("x", "y"));
        let (mut o, mut e) = (Vec::new(), Vec::new());
        render(
            &out,
            &OutputOptions::new(true, false, false),
            &mut o,
            &mut e,
        );
        let v: Value = serde_json::from_slice(&o).unwrap();
        assert_eq!(v["ok"], json!(false));
    }

    #[test]
    fn human_errors_go_to_stderr() {
        let out = CommandOutput::failed(Error::usage("bad flag", "see --help"));
        let (mut o, mut e) = (Vec::new(), Vec::new());
        render(
            &out,
            &OutputOptions::new(false, true, false),
            &mut o,
            &mut e,
        );
        assert!(o.is_empty());
        assert_eq!(
            String::from_utf8(e).unwrap(),
            "error: USAGE: bad flag\n  hint: see --help\n"
        );
    }
}
