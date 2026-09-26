//! The e2e entry point: filters, concurrency, result bookkeeping, PENDING
//! semantics and the selftest inversion.
//!
//! Environment (all optional; the Makefile forwards them):
//! - `STEMS_E2E_FEATURE`: a feature file or directory (repo-relative or
//!   absolute) to run instead of `tests/features`.
//! - `STEMS_E2E_TAGS`: a cucumber tag expression, e.g. `@FR-LC-5 and not @slow`.
//! - `STEMS_E2E_DOCKER=1`: also run `@docker` scenarios (serially).
//! - `STEMS_E2E_SELFTEST=1`: run only `@harness-selftest` and succeed iff every
//!   such scenario failed with a `LEAK:` message.
//! - `STEMS_E2E_SCENARIO_TIMEOUT` (seconds, default 60), `STEMS_E2E_BIN`,
//!   `STEMS_E2E_TMPDIR`, `STEMS_E2E_CONCURRENCY` (default 4),
//!   `STEMS_E2E_BLESS=1` (write goldens).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;

use cucumber::event::{self, HookType};
use cucumber::gherkin::tagexpr::TagOperation;
use cucumber::runner::ScenarioType;
use cucumber::tag::Ext as _;
use cucumber::{Event, Writer, WriterExt as _, cli, gherkin, parser, writer};

use crate::hooks::{self, feature_path};
use crate::world::{E2eWorld, repo_root, stems_bin};
use crate::{pending, procs, steps};

/// What happened to one scenario.
#[derive(Debug, Default, Clone)]
pub struct Outcome {
    pub name: String,
    pub failed: bool,
    pub leak: bool,
    pub finished: bool,
    pub messages: Vec<String>,
}

/// (feature path, line, name) -> outcome.
type Key = (String, usize, String);

static RESULTS: Mutex<BTreeMap<Key, Outcome>> = Mutex::new(BTreeMap::new());
static PARSE_ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn panic_text(info: &event::Info) -> String {
    if let Some(s) = info.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = info.downcast_ref::<&str>() {
        (*s).to_owned()
    } else {
        String::from("<non-string panic>")
    }
}

fn step_error_text(e: &event::StepError) -> String {
    match e {
        event::StepError::Panic(info) => panic_text(info),
        other => other.to_string(),
    }
}

/// Writer that records per-scenario outcomes (runs alongside the default
/// console writer).
#[derive(Debug, Default, Clone, Copy)]
pub struct Recorder;

impl writer::Normalized for Recorder {}

impl Recorder {
    fn record(
        feature: &gherkin::Feature,
        scenario: &gherkin::Scenario,
        ev: &event::Scenario<E2eWorld>,
    ) {
        let key = (
            feature_path(feature),
            scenario.position.line,
            scenario.name.clone(),
        );
        let mut results = RESULTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let o = results.entry(key).or_insert_with(|| Outcome {
            name: scenario.name.clone(),
            ..Outcome::default()
        });
        let step_ev = match ev {
            event::Scenario::Step(s, e) | event::Scenario::Background(s, e) => Some((s, e)),
            _ => None,
        };
        if let Some((step, e)) = step_ev {
            match e {
                event::Step::Failed(_, _, _, err) => {
                    o.failed = true;
                    o.messages.push(format!(
                        "step `{} {}` failed: {}",
                        step.keyword.trim(),
                        step.value,
                        step_error_text(err)
                    ));
                }
                event::Step::Skipped => {
                    o.failed = true;
                    o.messages.push(format!(
                        "step `{} {}` is not defined in the step library (see tests/stems-e2e/STEPS.md)",
                        step.keyword.trim(),
                        step.value
                    ));
                }
                _ => {}
            }
        }
        match ev {
            event::Scenario::Hook(ty, event::Hook::Failed(_, info)) => {
                let text = panic_text(info);
                o.failed = true;
                if text.starts_with("LEAK:") {
                    o.leak = true;
                }
                let which = if matches!(ty, HookType::Before) {
                    "Before"
                } else {
                    "After"
                };
                o.messages.push(format!("{which} hook failed: {text}"));
            }
            event::Scenario::Finished => o.finished = true,
            _ => {}
        }
    }
}

impl Writer<E2eWorld> for Recorder {
    type Cli = cli::Empty;

    async fn handle_event(
        &mut self,
        ev: parser::Result<Event<event::Cucumber<E2eWorld>>>,
        _cli: &Self::Cli,
    ) {
        match ev {
            Err(e) => PARSE_ERRORS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(e.to_string()),
            Ok(ev) => match ev.into_inner() {
                event::Cucumber::Feature(f, event::Feature::Scenario(s, rs)) => {
                    Self::record(&f, &s, &rs.event);
                }
                event::Cucumber::Feature(
                    f,
                    event::Feature::Rule(_, event::Rule::Scenario(s, rs)),
                ) => {
                    Self::record(&f, &s, &rs.event);
                }
                _ => {}
            },
        }
    }
}

struct Config {
    input: PathBuf,
    tags: Option<TagOperation>,
    docker: bool,
    selftest: bool,
    concurrency: usize,
}

fn env_flag(k: &str) -> bool {
    std::env::var(k).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn config() -> Result<Config, String> {
    let feature = std::env::var("STEMS_E2E_FEATURE").unwrap_or_default();
    let input = if feature.trim().is_empty() {
        repo_root().join("tests/features")
    } else {
        let p = PathBuf::from(feature.trim());
        if p.is_absolute() {
            p
        } else {
            repo_root().join(p)
        }
    };
    if !input.exists() {
        return Err(format!(
            "STEMS_E2E_FEATURE path {} does not exist",
            input.display()
        ));
    }
    let tags_raw = std::env::var("STEMS_E2E_TAGS").unwrap_or_default();
    let tags = if tags_raw.trim().is_empty() {
        None
    } else {
        Some(
            tags_raw
                .trim()
                .parse::<TagOperation>()
                .map_err(|e| format!("invalid STEMS_E2E_TAGS {tags_raw:?}: {e}"))?,
        )
    };
    Ok(Config {
        input,
        tags,
        docker: env_flag("STEMS_E2E_DOCKER"),
        selftest: env_flag("STEMS_E2E_SELFTEST"),
        concurrency: std::env::var("STEMS_E2E_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(4),
    })
}

fn all_tags<'a>(
    f: &'a gherkin::Feature,
    r: Option<&'a gherkin::Rule>,
    s: &'a gherkin::Scenario,
) -> Vec<&'a str> {
    f.tags
        .iter()
        .chain(r.map(|r| r.tags.iter()).into_iter().flatten())
        .chain(s.tags.iter())
        .map(String::as_str)
        .collect()
}

/// Synchronous entry point used by `tests/e2e.rs`.
pub fn main() -> ExitCode {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(run())
}

async fn run() -> ExitCode {
    let cfg = match config() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("stems-e2e: {e}");
            return ExitCode::FAILURE;
        }
    };
    let bin = stems_bin();
    if !bin.is_file() {
        eprintln!(
            "stems-e2e: the stems binary was not found at {}.\n\
             Build it first with `cargo build -p stems-cli` (`make e2e` does this), or set STEMS_E2E_BIN.",
            bin.display()
        );
        return ExitCode::FAILURE;
    }

    let pending_path = repo_root().join("tests/features/PENDING.txt");
    let entries = pending::parse(&std::fs::read_to_string(&pending_path).unwrap_or_default());
    for e in &entries {
        let p = repo_root().join(e.path.trim_end_matches('/'));
        if !p.exists() {
            println!(
                "stems-e2e: warning: PENDING.txt:{} names {} which does not exist",
                e.source_line, e.path
            );
        }
    }
    *hooks::PENDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = entries.clone();
    if !cfg.selftest {
        let _ = std::fs::remove_dir_all(hooks::failures_dir());
    }

    let (selftest, docker, tags) = (cfg.selftest, cfg.docker, cfg.tags.clone());
    let filter = move |f: &gherkin::Feature, r: Option<&gherkin::Rule>, s: &gherkin::Scenario| {
        let t = all_tags(f, r, s);
        let is_selftest = t.contains(&"harness-selftest");
        if selftest {
            return is_selftest;
        }
        if is_selftest || (t.contains(&"docker") && !docker) {
            return false;
        }
        tags.as_ref().is_none_or(|op| op.eval(t.iter()))
    };

    println!(
        "stems-e2e: binary {} | features {} | concurrency {} | docker {} | selftest {}",
        bin.display(),
        cfg.input.display(),
        cfg.concurrency,
        cfg.docker,
        cfg.selftest
    );

    cucumber::Cucumber::<E2eWorld, _, _, _, _>::new()
        .steps(steps::collection())
        .max_concurrent_scenarios(cfg.concurrency)
        .which_scenario(|f, r, s| {
            if all_tags(f, r, s).contains(&"docker") {
                ScenarioType::Serial
            } else {
                ScenarioType::Concurrent
            }
        })
        .after(hooks::after)
        .with_writer(
            writer::Basic::stdout()
                .summarized()
                .tee::<E2eWorld, _>(Recorder),
        )
        .with_default_cli()
        .filter_run(cfg.input.clone(), filter)
        .await;

    let results = std::mem::take(
        &mut *RESULTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let parse_errors = std::mem::take(
        &mut *PARSE_ERRORS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    if cfg.selftest {
        selftest_verdict(&results)
    } else {
        verdict(&results, &parse_errors, &entries)
    }
}

fn first_line(o: &Outcome) -> String {
    o.messages
        .first()
        .map(|m| m.lines().take(2).collect::<Vec<_>>().join(" | "))
        .unwrap_or_else(|| "did not finish".into())
}

fn verdict(
    results: &BTreeMap<Key, Outcome>,
    parse_errors: &[String],
    entries: &[pending::Entry],
) -> ExitCode {
    let mut bad = 0usize;
    let (mut passed, mut pend) = (0usize, 0usize);
    println!("\nstems-e2e summary ({} scenario(s))", results.len());
    for ((path, line, _), o) in results {
        let is_pending = pending::is_pending(entries, path, *line);
        let failed = o.failed || !o.finished;
        let label = if o.leak {
            bad += 1;
            "FAILED (LEAK)"
        } else if failed && is_pending {
            pend += 1;
            "PENDING (expected)"
        } else if failed {
            bad += 1;
            "FAILED"
        } else if is_pending {
            bad += 1;
            "UNEXPECTED PASS"
        } else {
            passed += 1;
            "PASSED"
        };
        println!("  {label:<20} {path}:{line}  {}", o.name);
        if failed {
            for m in &o.messages {
                let short: Vec<&str> = m.lines().take(3).collect();
                println!("      {}", short.join("\n      "));
            }
            if o.messages.is_empty() {
                println!("      {}", first_line(o));
            }
        }
        if label == "UNEXPECTED PASS" {
            println!("      this scenario passes now: remove it from tests/features/PENDING.txt");
        }
    }
    for e in parse_errors {
        bad += 1;
        println!("  PARSE ERROR  {e}");
    }
    println!("stems-e2e: {passed} passed, {pend} pending (expected failures), {bad} failing");
    if bad == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn selftest_verdict(results: &BTreeMap<Key, Outcome>) -> ExitCode {
    let mut ok = !results.is_empty();
    println!("\nstems-e2e selftest ({} scenario(s))", results.len());
    for ((path, line, _), o) in results {
        let leaked = o.leak && o.messages.iter().any(|m| m.contains("LEAK:"));
        println!(
            "  {} {path}:{line} {}",
            if leaked {
                "LEAK REPORTED (expected)"
            } else {
                "NO LEAK REPORTED"
            },
            o.name
        );
        ok &= leaked;
    }
    // Sweep: whatever the hook found must be dead now; kill stragglers.
    let leaked = std::mem::take(
        &mut *hooks::LEAKED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    for (pgid, pid) in &leaked {
        procs::kill_group(*pgid);
        procs::kill_pid(*pid);
    }
    let still: Vec<i32> = leaked
        .iter()
        .map(|(g, _)| *g)
        .filter(|g| procs::pgid_alive(*g))
        .collect();
    if !still.is_empty() {
        println!("  stray process groups survived cleanup: {still:?}");
        ok = false;
    }
    if ok {
        println!("stems-e2e selftest: OK — the After hook reported LEAK and the stray was killed");
        ExitCode::SUCCESS
    } else {
        println!(
            "stems-e2e selftest: FAILED — expected every @harness-selftest scenario to fail with LEAK:"
        );
        ExitCode::FAILURE
    }
}
