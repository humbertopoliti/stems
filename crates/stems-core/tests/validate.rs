//! Each validation pass on small temp workspaces, with a fake tool prober.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use stems_config::{LoadOptions, Requirement, Resolved};
use stems_core::tools::{ToolVersion, ToolVersions};
use stems_core::validate::{ValidateOptions, validate_with};
use stems_core::version::Version;
use stems_core::{Error, ErrorCode};

struct Ws {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Ws {
    fn new(yaml: &str, files: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("integration");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("stems.yaml"), yaml).unwrap();
        for f in files {
            let p = root.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "x\n").unwrap();
        }
        Self { _dir: dir, root }
    }

    fn load(&self) -> Resolved {
        stems_config::load(LoadOptions {
            workspace: Some(self.root.clone()),
            cwd: self.root.clone(),
            env: HashMap::new(),
            skip_local: true,
        })
        .unwrap_or_else(|e| panic!("{e}"))
    }

    fn check_with(&self, opts: &ValidateOptions, tools: &mut Fake) -> Vec<Error> {
        validate_with(&self.load(), opts, tools)
    }

    fn check(&self) -> Vec<Error> {
        self.check_with(&ValidateOptions::default(), &mut Fake::default())
    }
}

/// Fake prober: tool name -> result; unknown tools are missing.
#[derive(Default)]
struct Fake {
    tools: HashMap<String, ToolVersion>,
    calls: Vec<String>,
}

impl Fake {
    fn with(mut self, tool: &str, v: ToolVersion) -> Self {
        self.tools.insert(tool.into(), v);
        self
    }
}

impl ToolVersions for Fake {
    fn detect(
        &mut self,
        tool: &str,
        _req: &Requirement,
        _cwd: Option<&std::path::Path>,
    ) -> ToolVersion {
        self.calls.push(tool.into());
        self.tools
            .get(tool)
            .cloned()
            .unwrap_or(ToolVersion::Missing {
                reason: format!("`{tool}` was not found on PATH"),
            })
    }
}

fn found(v: (u64, u64, u64)) -> ToolVersion {
    ToolVersion::Found {
        version: Version::new(v.0, v.1, v.2),
        output: format!("v{}.{}.{}", v.0, v.1, v.2),
    }
}

fn codes(es: &[Error]) -> Vec<&str> {
    es.iter().map(|e| e.code.as_str()).collect()
}

fn paths(es: &[Error]) -> Vec<String> {
    es.iter()
        .map(|e| e.path.as_ref().map(ToString::to_string).unwrap_or_default())
        .collect()
}

#[test]
fn a_clean_workspace_has_no_errors() {
    let ws = Ws::new(
        "stems:\n  db: { type: docker, image: postgres:16, ports: ['15432:5432'], scripts: { seed: scripts/seed.sh } }\n  api:\n    type: process\n    command: run\n    depends_on: [{ stem: db, condition: seeded }]\n    ports: [8080, auto]\n",
        &["scripts/seed.sh"],
    );
    assert!(ws.check().is_empty(), "{:?}", ws.check());
}

#[test]
fn unknown_dependency_suggests_a_close_name() {
    let ws = Ws::new(
        "stems:\n  postgres: { type: docker, image: pg }\n  api: { type: process, command: run, depends_on: [postgre] }\n",
        &[],
    );
    let es = ws.check();
    assert_eq!(codes(&es), ["UNKNOWN_DEPENDENCY"]);
    assert_eq!(paths(&es), ["stems.api.depends_on"]);
    assert!(
        es[0]
            .hint
            .as_deref()
            .unwrap()
            .contains("did you mean `postgres`")
    );
    assert_eq!(es[0].span.as_ref().unwrap().line, 3);
}

#[test]
fn soft_cycles_pass_and_hard_cycles_fail() {
    let soft = Ws::new(
        "stems:\n  a: { type: process, command: x, depends_on: [b] }\n  b: { type: process, command: x, depends_on: [{ stem: a, soft: true }] }\n",
        &[],
    );
    assert!(soft.check().is_empty());
    let hard = Ws::new(
        "stems:\n  a: { type: process, command: x, depends_on: [b] }\n  b: { type: process, command: x, depends_on: [a] }\n",
        &[],
    );
    let es = hard.check();
    assert_eq!(codes(&es), ["CYCLE"]);
    assert!(es[0].message.contains("a -> b -> a"));
    assert_eq!(es[0].details["cycle"], serde_json::json!(["a", "b"]));
}

#[test]
fn port_conflicts_across_enabled_stems_only() {
    let ws = Ws::new(
        "stems:\n  a: { type: process, command: x, ports: [8080] }\n  b: { type: process, command: x, ports: [9000, 8080] }\n  c: { type: process, command: x, ports: [9000], enabled: false }\n  d: { type: process, command: x, ports: [7000, 7000] }\n",
        &[],
    );
    let es = ws.check();
    assert_eq!(codes(&es), ["PORT_CONFLICT", "PORT_CONFLICT"]);
    assert_eq!(paths(&es), ["stems.b.ports", "stems.d.ports"]);
    assert!(es[0].message.contains("8080") && es[0].message.contains("`a`"));
    assert!(es[1].message.contains("twice"));
    assert_eq!(es[0].details["stems"], serde_json::json!(["a", "b"]));
}

#[test]
fn auto_ports_are_rejected_on_containers() {
    let ws = Ws::new(
        "stems:\n  db: { type: docker, image: pg, ports: [auto] }\n  web: { type: process, command: x, ports: [auto] }\n",
        &[],
    );
    let es = ws.check();
    assert_eq!(codes(&es), ["SCHEMA_INVALID"]);
    assert_eq!(paths(&es), ["stems.db.ports[0]"]);
    assert!(es[0].hint.as_deref().unwrap().contains("container_port"));
}

#[test]
fn seeded_without_seed_and_disabled_dependencies() {
    let ws = Ws::new(
        "stems:\n  db: { type: docker, image: pg }\n  off: { type: process, command: x, enabled: false }\n  api:\n    type: process\n    command: x\n    depends_on:\n      - { stem: db, condition: seeded }\n      - off\n      - { stem: off, soft: true }\n",
        &[],
    );
    // A hard edge to a disabled stem is no longer an error (46) ...
    let es = ws.check();
    assert_eq!(codes(&es), ["SEEDED_WITHOUT_SEED"]);
    // ... but a warning, located at the edge: api starts without it (51).
    let w = stems_core::validate::warnings(&ws.load());
    assert_eq!(codes(&w), ["DEPENDENCY_DISABLED"]);
    assert_eq!(paths(&w), ["stems.api.depends_on"]);
    assert_eq!(
        w[0].details,
        serde_json::json!({ "stem": "api", "dependency": "off", "if_disabled": "start" })
    );
    assert_eq!(
        w[0].message,
        "stem `api` depends on `off`, which is disabled; `api` starts without it"
    );
}

#[test]
fn if_disabled_skip_and_unset_enabled_variables() {
    let ws = Ws::new(
        "stems:\n  search: { type: process, command: x, enabled: \"${env.WITH_SEARCH}\" }\n  api:\n    type: process\n    command: x\n    depends_on: [{ stem: search, if_disabled: skip }]\n",
        &[],
    );
    assert!(ws.check().is_empty());
    let r = ws.load();
    let w = stems_core::validate::warnings(&r);
    assert_eq!(codes(&w), ["ENABLED_VAR_UNSET", "DEPENDENCY_DISABLED"]);
    assert_eq!(
        w[1].message,
        "stem `api` needs `search`, which is disabled: `up` skips `api` (`if_disabled: skip`)"
    );
    assert_eq!(w[1].details["if_disabled"], "skip");
    assert_eq!(paths(&w), ["stems.search.enabled", "stems.api.depends_on"]);
    assert_eq!(w[0].message, "search disabled: WITH_SEARCH is unset");
    assert_eq!(w[0].details["level"], "info");
}

#[test]
fn script_files_must_exist_inside_the_repo() {
    let outside = tempfile::tempdir().unwrap();
    let abs = outside.path().join("evil.sh");
    fs::write(&abs, "x").unwrap();
    let yaml = format!(
        "scripts:\n  boot: {{ file: scripts/missing.sh }}\nstems:\n  a:\n    type: process\n    command: x\n    scripts:\n      seed: {{ file: ../escape.sh }}\n      setup: {{ file: '{}' }}\n      ok: {{ file: scripts/ok.sh }}\n",
        abs.display()
    );
    let ws = Ws::new(&yaml, &["scripts/ok.sh"]);
    fs::write(ws.root.parent().unwrap().join("escape.sh"), "x").unwrap();
    let es = ws.check();
    assert_eq!(
        codes(&es),
        [
            "SCRIPT_NOT_FOUND",
            "SCRIPT_OUTSIDE_WORKSPACE",
            "SCRIPT_OUTSIDE_WORKSPACE"
        ]
    );
    assert_eq!(
        paths(&es),
        [
            "scripts.boot",
            "stems.a.scripts.seed",
            "stems.a.scripts.setup"
        ]
    );
    assert!(es[0].message.contains("scripts/missing.sh"));
}

#[cfg(unix)]
#[test]
fn a_symlink_out_of_the_repo_is_outside() {
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("real.sh"), "x").unwrap();
    let ws = Ws::new(
        "stems:\n  a: { type: process, command: x, scripts: { seed: { file: scripts/link.sh } } }\n",
        &[],
    );
    fs::create_dir_all(ws.root.join("scripts")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("real.sh"),
        ws.root.join("scripts/link.sh"),
    )
    .unwrap();
    assert_eq!(codes(&ws.check()), ["SCRIPT_OUTSIDE_WORKSPACE"]);
}

#[test]
fn codebases_must_exist_for_enabled_stems() {
    let ws = Ws::new(
        "stems:\n  a: { type: process, command: x, codebase: ../nope }\n  b: { type: process, command: x, codebase: ../nope-too, enabled: false }\n  c: { type: process, command: x, codebase: stems.yaml }\n  g: { type: process, command: x, codebase: 'git@example.com:x/y.git' }\n",
        &[],
    );
    let es = ws.check();
    assert_eq!(codes(&es), ["CODEBASE_NOT_FOUND", "CODEBASE_NOT_FOUND"]);
    assert_eq!(paths(&es), ["stems.a.codebase", "stems.c.codebase"]);
    assert!(es[1].message.contains("not a directory"));
}

#[test]
fn overlays_may_not_overwrite_foreign_files_unless_owned() {
    let ws = Ws::new(
        "stems:\n  a:\n    type: process\n    command: x\n    codebase: repo\n    overlays:\n      - { file: o/a.ini, dest: config/a.ini }\n      - { file: o/b.ini, dest: config/b.ini }\n",
        &["repo/config/a.ini"],
    );
    let es = ws.check();
    assert_eq!(codes(&es), ["OVERLAY_CONFLICT"]);
    assert_eq!(paths(&es), ["stems.a.overlays"]);
    let owned = ValidateOptions {
        owned_overlays: vec![ws.root.join("repo/config/a.ini")],
        ..Default::default()
    };
    assert!(ws.check_with(&owned, &mut Fake::default()).is_empty());
}

#[test]
fn unknown_stems_in_profiles_and_script_requires() {
    let ws = Ws::new(
        "profiles:\n  be: [api, apii]\nscripts:\n  nuke: { command: x, requires: [db] }\nstems:\n  api: { type: process, command: x }\n",
        &[],
    );
    let es = ws.check();
    assert_eq!(codes(&es), ["UNKNOWN_STEM", "UNKNOWN_STEM"]);
    assert_eq!(paths(&es), ["profiles.be[1]", "scripts.nuke.requires[0]"]);
}

#[test]
fn type_requirements() {
    let ws = Ws::new(
        "stems:\n  p: { type: process }\n  d: { type: docker }\n  c: { type: compose }\n  e: { type: external }\n",
        &[],
    );
    let es = ws.check();
    assert_eq!(codes(&es), ["SCHEMA_INVALID"; 3]);
    assert_eq!(paths(&es), ["stems.p", "stems.d.image", "stems.c.file"]);
}

#[test]
fn unsupported_schema_version_is_the_only_error() {
    let ws = Ws::new(
        "schema_version: 7\nstems:\n  a: { type: process, depends_on: [ghost] }\n",
        &[],
    );
    let es = ws.check();
    assert_eq!(codes(&es), ["SCHEMA_VERSION_UNSUPPORTED"]);
    assert!(es[0].message.contains("schema_version 7"));
    assert_eq!(es[0].details["supported"], serde_json::json!([1]));
}

#[test]
fn loader_diagnostics_are_forwarded() {
    let ws = Ws::new(
        "stems:\n  a: { type: process, command: x, env: { U: '${var.nope}' } }\n",
        &[],
    );
    let es = ws.check();
    assert_eq!(codes(&es), ["UNRESOLVED_VARIABLE"]);
    assert_eq!(paths(&es), ["stems.a.env.U"]);
}

#[test]
fn requires_versions() {
    let ws = Ws::new(
        "requires:\n  node: '>=20'\n  python3: '>=3.9'\n  go: '^1.21'\n  pnpm: '>=9'\n  weird: 'banana'\n  custom: { version: '>=2', command: 'custom -V', regex: 'v(\\S+)' }\nstems: {}\n",
        &[],
    );
    let mut fake = Fake::default()
        .with("node", found((18, 19, 0)))
        .with("python3", found((3, 13, 7)))
        .with("go", found((1, 22, 1)))
        .with(
            "custom",
            ToolVersion::Unparseable {
                output: "custom build".into(),
            },
        );
    let es = ws.check_with(&ValidateOptions::default(), &mut fake);
    assert_eq!(
        codes(&es),
        [
            "TOOL_VERSION",
            "TOOL_VERSION",
            "SCHEMA_INVALID",
            "TOOL_VERSION"
        ]
    );
    assert_eq!(
        paths(&es),
        [
            "requires.node",
            "requires.pnpm",
            "requires.weird",
            "requires.custom"
        ]
    );
    assert!(es[0].message.contains("18.19.0") && es[0].message.contains(">=20"));
    assert!(es[1].message.contains("not available"));
    assert_eq!(es[0].details["found"], "18.19.0");
    // The unparseable range is never probed.
    assert!(!fake.calls.contains(&"weird".to_string()));

    let skip = ValidateOptions {
        skip_requires: true,
        ..Default::default()
    };
    let mut fake = Fake::default();
    let es = ws.check_with(&skip, &mut fake);
    assert!(es.is_empty(), "{es:?}");
    assert!(fake.calls.is_empty());
}

#[test]
fn all_errors_are_collected_and_sorted_by_location() {
    let ws = Ws::new(
        "stems:\n  z: { type: process, command: x, ports: [1000], depends_on: [ghost] }\n  a: { type: process, command: x, ports: [1000], codebase: ../missing }\n",
        &[],
    );
    let es = ws.check();
    assert_eq!(
        codes(&es),
        ["UNKNOWN_DEPENDENCY", "PORT_CONFLICT", "CODEBASE_NOT_FOUND"]
    );
    let lines: Vec<usize> = es.iter().map(|e| e.span.as_ref().unwrap().line).collect();
    let mut sorted = lines.clone();
    sorted.sort();
    assert_eq!(lines, sorted);
    assert!(es.iter().all(|e| e.hint.is_some()));
    assert!(es.iter().all(|e| e.code.exit_code() == 2));
    let _ = ErrorCode::all();
}

#[test]
fn git_codebase_urls_must_look_like_urls() {
    let ws = Ws::new(
        "stems:\n  a: { type: process, command: x, codebase: { git: 'not a url', ref: main } }\n  b: { type: process, command: x, codebase: { git: 'https://example.com/b.git', ref: v2 } }\n  c: { type: process, command: x, codebase: { git: 'file:///srv/c.git' } }\n  d: { type: process, command: x, codebase: { git: 'git@example.com:x/d.git' } }\n",
        &[],
    );
    let es = ws.check();
    assert_eq!(codes(&es), ["SCHEMA_INVALID"]);
    assert_eq!(paths(&es), ["stems.a.codebase.git"]);
}

#[test]
fn checks_shape_and_validate_opt_in() {
    let ws = Ws::new(
        "schema_version: 1\nname: w\nchecks:\n  long: { command: 'true', timeout: 31s }\n  empty: { command: ' ' }\n  nowhen: { command: 'true', when: [] }\n  ok: { command: 'true', timeout: 30s }\n  v: { command: 'false', when: [validate] }\n  d: { command: 'false' }\nstems:\n  a: { type: external }\n",
        &[],
    );
    let es = ws.check();
    let at: Vec<String> = paths(&es);
    assert_eq!(
        at,
        ["checks.long", "checks.empty", "checks.nowhen"],
        "{es:?}"
    );
    assert!(es.iter().all(|e| e.code == ErrorCode::SchemaInvalid));

    // Shape fine: `validate()` backs every daemon config load, so it never
    // runs user checks (not even `when: [validate]` ones).
    let ws = Ws::new(
        "schema_version: 1\nname: w\nchecks:\n  v: { command: 'echo run >> runs.txt; echo no >&2; exit 2', message: broke, when: [validate] }\n  w: { command: 'echo run >> runs.txt; false', severity: warn, when: [validate] }\n  d: { command: 'false' }\nstems:\n  a: { type: external }\n",
        &[],
    );
    assert!(ws.check().is_empty());
    assert!(!ws.root.join("runs.txt").exists(), "validate() ran a check");

    // `stems validate` runs them through `validate_results`: each check once,
    // split by severity.
    let (errors, warnings) = stems_core::checks::validate_results(&ws.load().workspace);
    assert_eq!(codes(&errors), ["CHECK_FAILED"]);
    assert_eq!(errors[0].details["check"], "check.v");
    assert_eq!(errors[0].details["exit"], 2);
    assert_eq!(codes(&warnings), ["CHECK_FAILED"]);
    assert_eq!(warnings[0].details["check"], "check.w");
    let runs = fs::read_to_string(ws.root.join("runs.txt")).unwrap();
    assert_eq!(runs.lines().count(), 2, "each check runs exactly once");
}

#[test]
fn build_allow_read_must_be_exact_absolute_paths_and_secret_ids_valid() {
    // 44: globs, relative paths and symlinks are rejected; ids are checked.
    let ws = Ws::new(
        "stems:\n  d:\n    type: docker\n    build:\n      context: .\n      secrets: [{ id: ok, file: s.txt }, { id: ok, file: s.txt }, { id: 'a b', env: X }]\n      allow_read:\n        - \"${workspace.root}/infra\"\n        - \"${workspace.root}/infra/*\"\n        - \"${workspace.root}/link\"\n        - relative/dir\n        - \"${workspace.root}/{a,b}\"\n",
        &["Dockerfile", "s.txt", "infra/x"],
    );
    std::os::unix::fs::symlink(ws.root.join("infra"), ws.root.join("link")).unwrap();
    let es = ws.check();
    assert_eq!(
        paths(&es),
        [
            "stems.d.build.secrets[1]",
            "stems.d.build.secrets[2]",
            "stems.d.build.allow_read[1]",
            "stems.d.build.allow_read[2]",
            "stems.d.build.allow_read[3]",
            "stems.d.build.allow_read[4]",
        ],
        "{es:#?}"
    );
    assert!(codes(&es).iter().all(|c| *c == "SCHEMA_INVALID"));
    assert!(es[2].message.contains("glob"), "{}", es[2].message);
    assert!(es[3].message.contains("symlink"), "{}", es[3].message);
    assert!(es[4].message.contains("absolute"), "{}", es[4].message);
    // The compose object form is checked the same way.
    let ws = Ws::new(
        "stems:\n  c:\n    type: compose\n    file: compose.yml\n    build: { allow_read: [\"/x/**\"] }\n",
        &["compose.yml"],
    );
    assert_eq!(paths(&ws.check()), ["stems.c.build.allow_read[0]"]);
}
