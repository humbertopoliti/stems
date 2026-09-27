//! Loading behaviour on small temp workspaces and the broken examples.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use stems_config::{
    Codebase, ConfigErrors, ConfigPath, DeferredKind, HealthPort, LoadOptions, PortRef, Resolved,
    ScriptSource, StemRuntime, Workspace, codes, load,
};

struct Ws {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Ws {
    fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap().join("integration");
        for (name, text) in files {
            let p = root.join(name);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
        Self { _dir: dir, root }
    }

    fn opts(&self) -> LoadOptions {
        let mut env = HashMap::new();
        env.insert("HOME".to_string(), "/home/tester".to_string());
        env.insert("SECRET".to_string(), "s3cret".to_string());
        LoadOptions {
            workspace: Some(self.root.clone()),
            cwd: self.root.clone(),
            env,
            skip_local: false,
        }
    }

    fn load(&self) -> Resolved {
        load(self.opts()).unwrap_or_else(|e| panic!("load failed: {e}"))
    }

    fn fail(&self) -> ConfigErrors {
        load(self.opts()).expect_err("load should fail")
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn env<'a>(ws: &'a Workspace, stem: &str, key: &str) -> &'a str {
    &ws.stem(stem).unwrap().env[key]
}

const BASIC: &str = r#"
schema_version: 1
name: demo
vars:
  user: app
stems:
  db:
    type: docker
    image: postgres:16
    ports: ["15432:5432"]
  api:
    type: process
    codebase: ../api
    ports: [{ name: http, port: 8080 }, { name: admin, port: 9090 }]
    depends_on: [db]
    scripts:
      setup: { command: npm ci, inputs: [package-lock.json] }
      start: npm run dev
  web:
    type: process
    codebase: ../web
    ports: [{ name: http, port: auto }]
"#;

// ---------------------------------------------------------------------------
// stems.local.yaml overlay
// ---------------------------------------------------------------------------

#[test]
fn local_overlay_disables_a_stem_but_keeps_it_listed() {
    let ws = Ws::new(&[
        ("stems.yaml", BASIC),
        ("stems.local.yaml", "stems:\n  web:\n    enabled: false\n"),
    ]);
    let r = ws.load();
    let w = &r.workspace;
    let enabled: Vec<_> = w.stems().map(|s| s.name.as_str()).collect();
    let disabled: Vec<_> = w.disabled_stems().map(|s| s.name.as_str()).collect();
    assert_eq!(enabled, ["db", "api"]);
    assert_eq!(disabled, ["web"]);
    assert!(!w.stem("web").unwrap().enabled);
    assert_eq!(w.stem("web").unwrap().ports[0].port, PortRef::Auto);
    assert_eq!(
        r.sources,
        [ws.root.join("stems.yaml"), ws.root.join("stems.local.yaml")]
    );
    assert_eq!(w.stems_in_order().len(), 2);
}

#[test]
fn local_overlay_overrides_codebase_env_and_scripts_per_key() {
    let ws = Ws::new(&[
        ("stems.yaml", BASIC),
        (
            "stems.local.yaml",
            "stems:\n  api:\n    codebase: ~/work/api\n    env: { EXTRA: '1' }\n    scripts:\n      setup: npm install\n",
        ),
    ]);
    let w = ws.load().workspace;
    let api = w.stem("api").unwrap();
    assert_eq!(
        api.codebase,
        Some(Codebase::Local {
            path: "/home/tester/work/api".into()
        })
    );
    assert_eq!(api.env["EXTRA"], "1");
    // setup replaced whole (inputs gone), start untouched.
    assert_eq!(
        api.scripts["setup"].source,
        ScriptSource::Command("npm install".into())
    );
    assert!(api.scripts["setup"].inputs.is_empty());
    assert_eq!(
        api.scripts["start"].source,
        ScriptSource::Command("npm run dev".into())
    );
    assert_eq!(
        api.scripts["setup"].cwd,
        PathBuf::from("/home/tester/work/api")
    );
}

#[test]
fn local_env_records_keys_set_by_the_local_file() {
    let ws = Ws::new(&[
        (
            "stems.yaml",
            "env: { SHARED: base, LOG: info }\nstems:\n  api:\n    type: process\n    env: { OWN: committed, KEEP: yes }\n  db:\n    type: process\n",
        ),
        (
            "stems.local.yaml",
            "env: { LOG: debug }\nstems:\n  api:\n    env: { OWN: mine, NEW: '${var.x}' }\nvars: { x: local-var }\n",
        ),
    ]);
    let w = ws.load().workspace;
    let api = w.stem("api").unwrap();
    let local: Vec<(&str, &str)> = api
        .local_env
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        local,
        [("LOG", "debug"), ("NEW", "local-var"), ("OWN", "mine")]
    );
    assert_eq!(api.env["KEEP"], "yes");
    assert_eq!(api.env["SHARED"], "base");
    let db = w.stem("db").unwrap();
    assert_eq!(db.local_env.len(), 1);
    assert_eq!(db.local_env["LOG"], "debug");
}

#[test]
fn local_env_is_empty_without_a_local_file() {
    let w = Ws::new(&[("stems.yaml", BASIC)]).load().workspace;
    assert!(w.stems.values().all(|s| s.local_env.is_empty()));
}

#[test]
fn bare_string_scripts_naming_an_existing_file_are_files() {
    let ws = Ws::new(&[
        (
            "stems.yaml",
            "scripts:\n  bootstrap: scripts/boot.sh\nstems:\n  db:\n    type: process\n    scripts:\n      seed: scripts/db/seed.sh\n      reset: scripts/db/missing.sh\n      start: sh scripts/db/seed.sh\n",
        ),
        ("scripts/boot.sh", "#!/bin/sh\n"),
        ("scripts/db/seed.sh", "#!/bin/sh\n"),
    ]);
    let w = ws.load().workspace;
    assert_eq!(
        w.scripts["bootstrap"].source,
        ScriptSource::File(ws.root.join("scripts/boot.sh"))
    );
    let db = w.stem("db").unwrap();
    assert_eq!(
        db.scripts["seed"].source,
        ScriptSource::File(ws.root.join("scripts/db/seed.sh"))
    );
    assert_eq!(
        db.scripts["reset"].source,
        ScriptSource::Command("scripts/db/missing.sh".into())
    );
    assert_eq!(
        db.scripts["start"].source,
        ScriptSource::Command("sh scripts/db/seed.sh".into())
    );
}

#[test]
fn restart_defaults_to_on_failure_with_five_restarts() {
    let w = Ws::new(&[("stems.yaml", BASIC)]).load().workspace;
    let r = &w.stem("api").unwrap().restart;
    assert_eq!(r.policy, stems_config::RestartPolicy::OnFailure);
    assert_eq!(r.max, 5);
}

#[test]
fn skip_local_ignores_the_overlay() {
    let ws = Ws::new(&[
        ("stems.yaml", BASIC),
        ("stems.local.yaml", "stems:\n  web:\n    enabled: false\n"),
    ]);
    let mut o = ws.opts();
    o.skip_local = true;
    let r = load(o).unwrap();
    assert!(r.workspace.stem("web").unwrap().enabled);
    assert_eq!(r.sources.len(), 1);
}

// ---------------------------------------------------------------------------
// extends / include
// ---------------------------------------------------------------------------

#[test]
fn extends_and_include_merge_in_order() {
    let ws = Ws::new(&[
        (
            "base/org.yaml",
            "vars: { region: eu, user: base }\nlogs: { keep: 9 }\nstems:\n  cache: { type: docker, image: redis:7 }\n",
        ),
        (
            "stems.yaml",
            "extends: base/org.yaml\ninclude: [teams/payments.yaml]\nname: child\nvars: { user: child }\nstems:\n  api: { type: process, env: { WHO: '${var.user}-${var.region}' } }\n",
        ),
        (
            "teams/payments.yaml",
            "include: [../shared/extra.yaml]\nvars: { user: from-include }\nstems:\n  payments: { type: process, env: { R: '${var.region}' } }\n",
        ),
        (
            "shared/extra.yaml",
            "stems:\n  audit: { type: external, health: { url: 'http://x' } }\n",
        ),
    ]);
    let r = ws.load();
    let w = &r.workspace;
    assert_eq!(
        w.vars["user"], "child",
        "the including file wins over base and includes"
    );
    assert_eq!(w.vars["region"], "eu");
    assert_eq!(w.logs.keep, 9);
    let names: Vec<_> = w.stems.keys().map(String::as_str).collect();
    assert_eq!(names, ["cache", "audit", "payments", "api"]);
    assert_eq!(env(w, "api", "WHO"), "child-eu");
    assert_eq!(env(w, "payments", "R"), "eu");
    let rel: Vec<_> = r
        .sources
        .iter()
        .map(|p| p.strip_prefix(&ws.root).unwrap().display().to_string())
        .collect();
    assert_eq!(
        rel,
        [
            "base/org.yaml",
            "shared/extra.yaml",
            "teams/payments.yaml",
            "stems.yaml"
        ]
    );
    // Spans point at the file that supplied the effective value.
    let s = r.spans.get(&"stems.audit".parse().unwrap()).unwrap();
    assert_eq!(s.file, ws.root.join("shared/extra.yaml"));
}

#[test]
fn missing_include_is_an_error() {
    let ws = Ws::new(&[("stems.yaml", "include: [nope.yaml]\n")]);
    let e = ws.fail();
    assert_eq!(e.errors[0].code, codes::INCLUDE_NOT_FOUND);
    assert!(e.errors[0].message.contains("nope.yaml"));
}

#[test]
fn include_cycle_is_an_error() {
    let ws = Ws::new(&[
        ("stems.yaml", "include: [a.yaml]\n"),
        ("a.yaml", "include: [b.yaml]\n"),
        ("b.yaml", "include: [a.yaml]\n"),
    ]);
    let e = ws.fail();
    assert_eq!(e.errors[0].code, codes::INCLUDE_CYCLE);
}

#[test]
fn duplicate_stem_across_includes_is_an_error_with_both_files() {
    let ws = Ws::new(&[
        ("stems.yaml", "include: [teams/a.yaml, teams/b.yaml]\n"),
        ("teams/a.yaml", "stems:\n  pay: { type: process }\n"),
        ("teams/b.yaml", "stems:\n  pay: { type: process }\n"),
    ]);
    let e = ws.fail();
    assert_eq!(e.errors[0].code, codes::DUPLICATE_STEM);
    let d = e.errors[0].details.as_ref().unwrap();
    assert_eq!(d["stem"], "pay");
    let files: Vec<String> = d["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        files,
        [
            ws.root.join("teams/a.yaml").display().to_string(),
            ws.root.join("teams/b.yaml").display().to_string()
        ]
    );
    assert_eq!(e.errors[0].path, Some("stems.pay".parse().unwrap()));

    // An included stem redefined by the including file is a duplicate too.
    let ws = Ws::new(&[
        (
            "stems.yaml",
            "include: [a.yaml]\nstems:\n  pay: { type: process }\n",
        ),
        ("a.yaml", "stems:\n  pay: { type: process }\n"),
    ]);
    assert_eq!(ws.fail().errors[0].code, codes::DUPLICATE_STEM);
}

#[test]
fn extends_bases_and_the_local_file_may_override_stems() {
    let ws = Ws::new(&[
        (
            "base.yaml",
            "stems:\n  pay: { type: process, env: { A: base } }\n",
        ),
        (
            "stems.yaml",
            "extends: base.yaml\ninclude: [a.yaml, a.yaml]\nstems:\n  pay: { env: { A: child } }\n",
        ),
        ("a.yaml", "stems:\n  other: { type: process }\n"),
        ("stems.local.yaml", "stems:\n  pay: { enabled: false }\n"),
    ]);
    let r = ws.load();
    let pay = r.workspace.stem("pay").unwrap();
    assert!(!pay.enabled);
    assert_eq!(pay.env["A"], "child");
}

#[test]
fn extends_chain_is_limited_to_five() {
    let mut files: Vec<(String, String)> = vec![("stems.yaml".into(), "extends: b1.yaml\n".into())];
    for i in 1..=5 {
        files.push((format!("b{i}.yaml"), format!("extends: b{}.yaml\n", i + 1)));
    }
    files.push(("b6.yaml".into(), "vars: { deep: '1' }\n".into()));
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let e = Ws::new(&refs).fail();
    assert_eq!(e.errors[0].code, codes::INCLUDE_CYCLE);
    assert!(
        e.errors[0].message.contains("deeper than 5"),
        "{}",
        e.errors[0].message
    );
    assert!(e.errors[0].hint.as_deref().unwrap().contains("at most 5"));

    // Five hops are fine.
    let mut files: Vec<(String, String)> = vec![("stems.yaml".into(), "extends: b1.yaml\n".into())];
    for i in 1..=4 {
        files.push((format!("b{i}.yaml"), format!("extends: b{}.yaml\n", i + 1)));
    }
    files.push(("b5.yaml".into(), "vars: { deep: '1' }\n".into()));
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    assert_eq!(Ws::new(&refs).load().workspace.vars["deep"], "1");
}

#[test]
fn nested_extends_resolve_relative_to_their_own_file() {
    let ws = Ws::new(&[
        ("stems.yaml", "extends: org/base.yaml\n"),
        (
            "org/base.yaml",
            "extends: ../shared/root.yaml\ninclude: [parts/p.yaml]\n",
        ),
        ("org/parts/p.yaml", "vars: { part: p }\n"),
        ("shared/root.yaml", "vars: { root: r }\n"),
    ]);
    let r = ws.load();
    assert_eq!(r.workspace.vars["part"], "p");
    assert_eq!(r.workspace.vars["root"], "r");
}

// ---------------------------------------------------------------------------
// Substitution
// ---------------------------------------------------------------------------

#[test]
fn every_substitution_form() {
    let yaml = r#"
name: subst
vars:
  a: "${var.b}-a"
  b: "${var.c}-b"
  c: c
  web_url: "http://${stem.web.host}:${stem.web.port}"
stems:
  db:
    type: docker
    image: postgres:16
    ports: ["15432:5432"]
  web:
    type: process
    codebase: ../web
    ports: [{ name: http, port: auto }]
    health: { type: tcp }
    outputs: { URL: "http://localhost:${stem.self.port}" }
  api:
    type: process
    codebase: ../api
    ports: [{ name: http, port: 8080 }, { name: admin, port: 9090 }]
    env:
      CHAIN: "${var.a}"
      ENV: "${env.SECRET}"
      ROOT: "${workspace.root}"
      WSNAME: "${workspace.name}"
      CODE: "${codebase}/src"
      DB: "${stem.db.port}"
      ADMIN: "${stem.api.ports.admin}"
      HOST: "${stem.db.host}"
      SELF: "${stem.self.port}"
      WEB: "${var.web_url}"
      TOKEN: "${stem.db.outputs.TOKEN}"
      SHELL_VAR: "${HOME} and $PATH"
      ESCAPED: "$${var.a}"
    health: { type: http, url: "http://localhost:${stem.self.ports.http}/healthz" }
"#;
    let ws = Ws::new(&[("stems.yaml", yaml)]);
    let r = ws.load();
    assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
    let w = &r.workspace;
    let e = |k| env(w, "api", k);
    assert_eq!(e("CHAIN"), "c-b-a");
    assert_eq!(e("ENV"), "s3cret");
    assert_eq!(e("ROOT"), ws.root.display().to_string());
    assert_eq!(e("WSNAME"), "subst");
    assert_eq!(
        e("CODE"),
        format!("{}/src", ws.root.parent().unwrap().join("api").display())
    );
    assert_eq!(e("DB"), "15432");
    assert_eq!(e("ADMIN"), "9090");
    assert_eq!(e("HOST"), "localhost");
    assert_eq!(e("SELF"), "8080");
    assert_eq!(e("WEB"), "http://localhost:${stem.web.port}");
    assert_eq!(e("TOKEN"), "${stem.db.outputs.TOKEN}");
    assert_eq!(e("SHELL_VAR"), "${HOME} and $PATH");
    assert_eq!(e("ESCAPED"), "${var.a}");
    assert_eq!(
        w.stem("api")
            .unwrap()
            .health
            .as_ref()
            .unwrap()
            .url
            .as_deref(),
        Some("http://localhost:8080/healthz")
    );
    // Auto ports: `self` is rewritten to the stem name and recorded.
    let web = w.stem("web").unwrap();
    assert_eq!(
        web.outputs["URL"],
        stems_config::Output::Value("http://localhost:${stem.web.port}".into())
    );
    assert_eq!(
        web.health.as_ref().unwrap().port,
        Some(HealthPort::Deferred("${stem.web.port}".into()))
    );
    let deferred: Vec<(String, String, DeferredKind)> = r
        .deferred
        .iter()
        .map(|d| (d.path.to_string(), d.reference.clone(), d.kind))
        .collect();
    for want in [
        ("vars.web_url", "stem.web.port", DeferredKind::AutoPort),
        ("stems.api.env.WEB", "stem.web.port", DeferredKind::AutoPort),
        (
            "stems.api.env.TOKEN",
            "stem.db.outputs.TOKEN",
            DeferredKind::Output,
        ),
        (
            "stems.web.outputs.URL",
            "stem.web.port",
            DeferredKind::AutoPort,
        ),
        (
            "stems.web.health.port",
            "stem.web.port",
            DeferredKind::AutoPort,
        ),
    ] {
        let want = (want.0.to_string(), want.1.to_string(), want.2);
        assert!(
            deferred.contains(&want),
            "missing {want:?} in {deferred:#?}"
        );
    }
}

#[test]
fn variable_cycle_is_reported_once_and_does_not_hang() {
    let yaml = r#"
vars:
  a: "x${var.b}"
  b: "y${var.a}"
  ok: fine
stems:
  api: { type: process, env: { A: "${var.a}", OK: "${var.ok}" } }
"#;
    let ws = Ws::new(&[("stems.yaml", yaml)]);
    let r = ws.load();
    let cycles: Vec<_> = r
        .diagnostics
        .iter()
        .filter(|d| d.message.contains("cycle"))
        .collect();
    assert_eq!(cycles.len(), 1, "{:#?}", r.diagnostics);
    assert_eq!(cycles[0].code, codes::UNRESOLVED_VARIABLE);
    assert!(
        cycles[0].message.contains("a -> b -> a"),
        "{}",
        cycles[0].message
    );
    assert_eq!(cycles[0].path.as_ref().unwrap().to_string(), "vars.a");
    assert!(cycles[0].location.is_some());
    assert_eq!(env(&r.workspace, "api", "A"), "${var.a}");
    assert_eq!(env(&r.workspace, "api", "OK"), "fine");
}

#[test]
fn unresolvable_references_become_located_diagnostics() {
    let yaml = r#"
stems:
  db: { type: docker, image: x }
  api:
    type: process
    env:
      A: "${var.nope}"
      B: "${env.NOT_SET}"
      C: "${stem.ghost.port}"
      D: "${stem.db.port}"
      E: "${stem.db.flavour}"
      F: "${codebase}"
      G: "${workspace.colour}"
scripts:
  x: "echo ${stem.self.port}"
"#;
    let ws = Ws::new(&[("stems.yaml", yaml)]);
    let r = ws.load();
    let got: Vec<(String, &str)> = r
        .diagnostics
        .iter()
        .map(|d| (d.path.as_ref().unwrap().to_string(), d.code.as_str()))
        .collect();
    for k in ["A", "B", "C", "D", "E", "F", "G"] {
        assert!(
            got.contains(&(format!("stems.api.env.{k}"), codes::UNRESOLVED_VARIABLE)),
            "{k}: {got:#?}"
        );
    }
    assert!(got.contains(&("scripts.x".into(), codes::UNRESOLVED_VARIABLE)));
    let a = r
        .diagnostics
        .iter()
        .find(|d| d.message.contains("var.nope"))
        .unwrap();
    let loc = a.location.as_ref().unwrap();
    assert_eq!(
        (loc.file.clone(), loc.line),
        (ws.root.join("stems.yaml"), 7)
    );
    assert!(a.hint.is_some());
}

// ---------------------------------------------------------------------------
// Schema errors and defaults
// ---------------------------------------------------------------------------

#[test]
fn bad_schema_example_fails_with_path_and_location() {
    let dir = repo_root().join("examples/workspaces/broken/bad-schema");
    let e = load(LoadOptions {
        workspace: Some(dir.clone()),
        cwd: dir.clone(),
        ..Default::default()
    })
    .unwrap_err();
    let d = &e.errors[0];
    assert_eq!(d.code, codes::SCHEMA_INVALID);
    assert_eq!(d.path.as_ref().unwrap().to_string(), "stems.shop-api.type");
    assert!(d.message.contains("rocket"), "{}", d.message);
    let loc = d.location.as_ref().unwrap();
    assert_eq!(loc.line, 8);
    assert!(loc.file.ends_with("bad-schema/stems.yaml"));
}

#[test]
fn unresolved_variable_example_matches_expected() {
    let dir = repo_root().join("examples/workspaces/broken/unresolved-variable");
    let r = load(LoadOptions {
        workspace: Some(dir.clone()),
        cwd: dir,
        ..Default::default()
    })
    .unwrap();
    let d = &r.diagnostics[0];
    assert_eq!(d.code, codes::UNRESOLVED_VARIABLE);
    assert_eq!(
        d.path.as_ref().unwrap().to_string(),
        "stems.shop-api.env.DATABASE_URL"
    );
    assert!(d.message.contains("var.nope"));
}

#[test]
fn every_broken_example_parses_except_bad_schema() {
    for entry in fs::read_dir(repo_root().join("examples/workspaces/broken")).unwrap() {
        let dir = entry.unwrap().path();
        let res = load(LoadOptions {
            workspace: Some(dir.clone()),
            cwd: dir.clone(),
            ..Default::default()
        });
        let bad = dir.ends_with("bad-schema");
        assert_eq!(res.is_err(), bad, "{}: {:?}", dir.display(), res.err());
    }
}

#[test]
fn unknown_fields_are_rejected_with_a_path() {
    let ws = Ws::new(&[(
        "stems.yaml",
        "stems:\n  api:\n    type: process\n    colour: blue\n",
    )]);
    let d = &ws.fail().errors[0];
    assert_eq!(d.code, codes::SCHEMA_INVALID);
    assert_eq!(d.path.as_ref().unwrap().to_string(), "stems.api.colour");
    assert!(d.message.contains("colour"), "{}", d.message);
    assert_eq!(d.location.as_ref().unwrap().line, 4);

    let ws = Ws::new(&[(
        "stems.yaml",
        "stems:\n  api:\n    type: process\n    scripts:\n      seed: { file: a.sh, colour: x }\n",
    )]);
    let d = &ws.fail().errors[0];
    assert_eq!(
        d.path.as_ref().unwrap().to_string(),
        "stems.api.scripts.seed"
    );
    assert!(d.message.contains("colour"), "{}", d.message);
}

#[test]
fn yaml_syntax_errors_are_located() {
    let ws = Ws::new(&[("stems.yaml", "stems:\n  api: [unclosed\n")]);
    let d = &ws.fail().errors[0];
    assert_eq!(d.code, codes::SCHEMA_INVALID);
    assert!(d.location.is_some());
}

#[test]
fn missing_type_after_merge_is_an_error() {
    let ws = Ws::new(&[("stems.yaml", "stems:\n  api:\n    description: no type\n")]);
    let d = &ws.fail().errors[0];
    assert_eq!(d.path.as_ref().unwrap().to_string(), "stems.api.type");
    assert_eq!(d.location.as_ref().unwrap().line, 2);
}

#[test]
fn fields_of_another_stem_type_are_diagnosed() {
    let ws = Ws::new(&[(
        "stems.yaml",
        "stems:\n  api:\n    type: process\n    image: node:20\n    service: x\n",
    )]);
    let r = ws.load();
    let paths: Vec<String> = r
        .diagnostics
        .iter()
        .map(|d| d.path.as_ref().unwrap().to_string())
        .collect();
    assert_eq!(paths, ["stems.api.image", "stems.api.service"]);
    assert!(
        r.diagnostics
            .iter()
            .all(|d| d.code == codes::SCHEMA_INVALID)
    );
}

#[test]
fn defaults_fill_an_empty_stem() {
    let ws = Ws::new(&[(
        "stems.yaml",
        "stems:\n  p: { type: process, ports: [3000] }\n  d: { type: docker, image: x, ports: [80] }\n  c: { type: compose, file: dc.yml }\n  e: { type: external }\n",
    )]);
    let w = ws.load().workspace;
    assert_eq!(w.name, "integration");
    let p = w.stem("p").unwrap();
    assert_eq!(p.ports[0].name, "port0");
    assert_eq!(p.ports[0].container_port, None);
    assert_eq!(
        p.health.as_ref().unwrap().kind,
        stems_config::HealthType::Process
    );
    match &p.runtime {
        StemRuntime::Process(s) => {
            assert_eq!(s.cwd, ws.root);
            assert_eq!(s.shell, "/bin/sh");
        }
        other => panic!("{other:?}"),
    }
    let d = w.stem("d").unwrap();
    assert_eq!(d.ports[0].container_port, Some(80));
    assert_eq!(
        d.health.as_ref().unwrap().kind,
        stems_config::HealthType::Docker
    );
    match &w.stem("c").unwrap().runtime {
        StemRuntime::Compose(c) => {
            assert_eq!(c.service, "c");
            assert_eq!(c.project_name, "stems-integration");
            assert_eq!(c.file.as_deref(), Some(ws.root.join("dc.yml").as_path()));
        }
        other => panic!("{other:?}"),
    }
    assert!(w.stem("e").unwrap().health.is_none());
}

#[test]
fn git_codebases_get_a_managed_directory() {
    let ws = Ws::new(&[(
        "stems.yaml",
        "repos_dir: checkouts\nstems:\n  a: { type: process, codebase: 'git@github.com:acme/a.git' }\n  b: { type: process, codebase: { git: 'https://x/b.git', ref: main } }\n",
    )]);
    let w = ws.load().workspace;
    assert_eq!(
        w.stem("a").unwrap().codebase,
        Some(Codebase::Git {
            url: "git@github.com:acme/a.git".into(),
            git_ref: None,
            path: ws.root.join("checkouts/a"),
        })
    );
    assert_eq!(
        w.stem("b").unwrap().codebase,
        Some(Codebase::Git {
            url: "https://x/b.git".into(),
            git_ref: Some("main".into()),
            path: ws.root.join("checkouts/b"),
        })
    );
}

#[test]
fn span_index_covers_nested_paths() {
    let r = load(LoadOptions {
        workspace: Some(repo_root().join("examples/workspaces/hello-shop")),
        cwd: repo_root(),
        skip_local: true,
        ..Default::default()
    })
    .unwrap();
    let p: ConfigPath = "stems.shop-api.depends_on[1].stem".parse().unwrap();
    let s = r.spans.get(&p).expect("span for depends_on[1].stem");
    let text = fs::read_to_string(&s.file).unwrap();
    let line = text.lines().nth(s.line - 1).unwrap();
    assert!(line.contains("stem: redis"), "{line}");
}

#[test]
fn resolved_model_roundtrips_through_json() {
    let r = load(LoadOptions {
        workspace: Some(repo_root().join("examples/workspaces/hello-shop")),
        cwd: repo_root(),
        skip_local: true,
        ..Default::default()
    })
    .unwrap();
    let json = serde_json::to_string(&r.workspace).unwrap();
    let back: Workspace = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r.workspace);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["stems"]["shop-web"]["type"], "process");
    assert_eq!(v["stems"]["shop-web"]["enabled"], true);
}

#[test]
fn include_demo_fixture_resolves() {
    let dir = repo_root().join("tests/fixtures/workspaces/include-demo");
    let r = load(LoadOptions {
        workspace: Some(dir.clone()),
        cwd: dir,
        skip_local: true,
        ..Default::default()
    })
    .unwrap();
    assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
    let w = &r.workspace;
    assert_eq!(w.vars["region"], "eu");
    assert_eq!(w.vars["owner"], "child");
    assert_eq!(w.logs.keep, 3);
    assert_eq!(env(w, "payments", "GATEWAY_URL"), "http://localhost:18190");
    assert_eq!(env(w, "gateway", "OWNER"), "child");
    assert_eq!(r.sources.len(), 3);
}

#[test]
fn local_example_profile_alias_parses() {
    // hello-shop's stems.local.yaml.example uses `profiles: { default: backend }`.
    let ws = Ws::new(&[
        (
            "stems.yaml",
            "profiles: { default: [a], backend: [b] }\nstems: {}\n",
        ),
        ("stems.local.yaml", "profiles:\n  default: backend\n"),
    ]);
    let w = ws.load().workspace;
    assert_eq!(
        w.profiles["default"],
        stems_config::Profile::Alias("backend".into())
    );
    assert_eq!(
        w.profiles["backend"],
        stems_config::Profile::Stems(vec!["b".into()])
    );
}

#[test]
fn outputs_forms_and_invalid_names() {
    let ws = Ws::new(&[(
        "stems.yaml",
        "stems:\n  api:\n    type: process\n    ports: [8080]\n    outputs:\n      URL: \"http://localhost:${stem.self.port}\"\n      N: 3\n      TOKEN: { command: \"cat $STEMS_STATE_DIR/token\", secret: true }\n      PLAIN: { command: \"echo x\" }\n      bad-name: x\n      9LIVES: y\n",
    )]);
    let r = ws.load();
    let api = r.workspace.stem("api").unwrap();
    use stems_config::Output;
    assert_eq!(
        api.outputs["URL"],
        Output::Value("http://localhost:8080".into())
    );
    assert_eq!(api.outputs["N"], Output::Value("3".into()));
    assert_eq!(
        api.outputs["TOKEN"],
        Output::Command {
            command: "cat $STEMS_STATE_DIR/token".into(),
            secret: true
        }
    );
    assert!(api.outputs["TOKEN"].is_secret());
    assert!(!api.outputs["PLAIN"].is_secret());
    assert_eq!(api.outputs.len(), 4, "invalid names are dropped");
    let paths: Vec<String> = r
        .diagnostics
        .iter()
        .map(|d| d.path.as_ref().unwrap().to_string())
        .collect();
    assert_eq!(
        paths,
        ["stems.api.outputs.bad-name", "stems.api.outputs.9LIVES"]
    );
    assert!(
        r.diagnostics
            .iter()
            .all(|d| d.code == codes::SCHEMA_INVALID)
    );
    // `show` serialises the declaration as written (no value exists at config time).
    let v = serde_json::to_value(api).unwrap();
    assert_eq!(
        v["outputs"]["TOKEN"],
        serde_json::json!({ "command": "cat $STEMS_STATE_DIR/token", "secret": true })
    );

    let ws = Ws::new(&[(
        "stems.yaml",
        "stems:\n  api:\n    type: process\n    outputs:\n      T: { command: x, secret: true, extra: 1 }\n",
    )]);
    assert_eq!(ws.fail().errors[0].code, codes::SCHEMA_INVALID);
}

// ---------------------------------------------------------------------------
// A local `ports` override drives the primary port (and references to it),
// with or without a variant; `container_port` is kept by port name.
// ---------------------------------------------------------------------------

const PORTS: &str = r#"
schema_version: 1
name: ports
stems:
  a:
    type: process
    command: python3 app.py
    ports: [{ name: http, port: 18301 }]
    health: { type: http, url: "http://127.0.0.1:${stem.a.port}/healthz" }
    variants:
      docker:
        type: docker
        image: shop-api
        ports: [{ name: http, port: 18301, container_port: 8080 }]
  b:
    type: process
    command: python3 app.py
    depends_on: [a]
    env: { API_URL: "http://127.0.0.1:${stem.a.port}", API_HTTP: "${stem.a.ports.http}" }
    ports: [{ name: http, port: 18302 }]
"#;

#[test]
fn local_ports_override_is_the_resolved_primary_port() {
    for variant in ["", "    variant: docker\n"] {
        let local = format!(
            "stems:\n  a:\n{variant}    ports: [{{ name: http, port: 20020 }}]\n  b:\n    ports: [{{ name: http, port: 20021 }}]\n"
        );
        let ws = Ws::new(&[("stems.yaml", PORTS), ("stems.local.yaml", &local)]);
        let r = ws.load();
        assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
        let w = &r.workspace;
        let a = w.stem("a").unwrap();
        let p = a.primary_port().unwrap();
        assert_eq!(p.name, "http");
        assert_eq!(p.port, PortRef::Fixed(20020), "variant {variant:?}");
        let container = if variant.is_empty() { None } else { Some(8080) };
        assert_eq!(p.container_port, container, "variant {variant:?}");
        assert_eq!(a.ports.len(), 1);
        assert_eq!(
            w.stem("b").unwrap().primary_port().unwrap().port,
            PortRef::Fixed(20021)
        );
        assert_eq!(env(w, "b", "API_URL"), "http://127.0.0.1:20020");
        assert_eq!(env(w, "b", "API_HTTP"), "20020");
        let health = serde_json::to_value(&a.health).unwrap();
        assert!(
            health
                .to_string()
                .contains("http://127.0.0.1:20020/healthz"),
            "{health}"
        );
    }
}
