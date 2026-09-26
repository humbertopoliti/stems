//! Goldens for deliverables 17 (script catalogue, MCP arg schemas) and 18
//! (overlay template rendering) against the hello-shop example workspace.

use std::path::{Path, PathBuf};

use stems_config::Workspace;
use stems_core::ErrorCode;
use stems_core::overlays::{RenderCtx, hash_bytes, render_overlay};
use stems_core::scriptargs::{build_catalog, json_schema_for, parse_args};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn hello_shop() -> Workspace {
    stems_config::load(stems_config::LoadOptions {
        workspace: Some(repo_root().join("examples/workspaces/hello-shop")),
        cwd: repo_root(),
        env: Default::default(),
        skip_local: true,
    })
    .unwrap_or_else(|e| panic!("{e}"))
    .workspace
}

/// Replace the machine-specific repo path so goldens are portable.
fn portable(s: &str) -> String {
    s.replace(&repo_root().display().to_string(), "<repo>")
}

fn pretty(v: &impl serde::Serialize) -> String {
    portable(&serde_json::to_string_pretty(v).unwrap())
}

#[test]
fn catalog_hello_shop() {
    let ws = hello_shop();
    insta::assert_snapshot!("catalog-hello-shop", pretty(&build_catalog(&ws)));
}

fn args_of<'a>(ws: &'a Workspace, stem: &str, script: &str) -> &'a [stems_config::ScriptArg] {
    &ws.stem(stem).unwrap().scripts[script].args
}

#[test]
fn mcp_schema_create_test_user() {
    let ws = hello_shop();
    let schema = json_schema_for(args_of(&ws, "shop-api", "create-test-user"));
    insta::assert_snapshot!("mcp-schema-create-test-user", pretty(&schema));
}

#[test]
fn mcp_schema_seed_large() {
    let ws = hello_shop();
    let schema = json_schema_for(args_of(&ws, "postgres", "seed-large"));
    insta::assert_snapshot!("mcp-schema-seed-large", pretty(&schema));
}

#[test]
fn create_test_user_errors_name_the_arg() {
    let ws = hello_shop();
    let schema = args_of(&ws, "shop-api", "create-test-user");
    let argv = |s: &str| -> Vec<String> { s.split_whitespace().map(String::from).collect() };
    let ok = parse_args(schema, &argv("--email a@b.c")).unwrap();
    assert_eq!(ok.to_argv(), argv("--email a@b.c --role admin"));
    let missing = parse_args(schema, &[]).unwrap_err();
    let bad_enum = parse_args(schema, &argv("--email a@b.c --role superuser")).unwrap_err();
    for e in [&missing, &bad_enum] {
        assert_eq!(e.code, ErrorCode::ScriptArgsInvalid);
        assert_eq!(e.exit_code(), 2);
    }
    insta::assert_snapshot!(
        "create-test-user-errors",
        format!("{missing}\n---\n{bad_enum}")
    );
}

#[test]
fn render_local_ini_template() {
    let ws = hello_shop();
    let stem = ws.stem("shop-api").unwrap();
    let mut ctx = RenderCtx::for_stem(&ws, stem, Default::default());
    // Fake runtime ports (as if overridden/allocated by the daemon).
    assert!(ctx.set_port("postgres", "pg", 25432));
    assert!(ctx.set_port("shop-api", "http", 28080));
    assert!(!ctx.set_port("shop-api", "nope", 1));
    let bytes = render_overlay(&stem.overlays[0], &ctx).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(text.contains("port = 25432") && text.contains("port = 28080"));
    insta::assert_snapshot!("overlay-local-ini", text);
    assert_eq!(hash_bytes(&bytes).len(), 64);
}

#[test]
fn render_rejects_sources_outside_the_repo_and_unresolved_refs() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(dir.path().join("outside.tmpl"), "x").unwrap();
    std::fs::write(repo.join("bad.tmpl"), "${stem.ghost.port} ${var.nope}").unwrap();
    std::fs::write(repo.join("raw.bin"), b"${stem.self.port}\xff").unwrap();
    std::fs::write(repo.join("auto.tmpl"), "${stem.self.port}").unwrap();
    let ctx = RenderCtx {
        workspace_root: repo.clone(),
        integration_repo: repo.clone(),
        codebase: dir.path().join("code"),
        stem_name: "api".into(),
        ports: [(
            "api".to_string(),
            vec![stems_config::Port {
                name: "http".into(),
                port: stems_config::PortRef::Auto,
                container_port: None,
            }],
        )]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let overlay = |source| stems_config::Overlay {
        source,
        dest: "x".into(),
        keep: false,
        mode: stems_config::FileMode(0o644),
    };
    use stems_config::OverlaySource::{File, Template};

    let e = render_overlay(&overlay(Template(repo.join("../outside.tmpl"))), &ctx).unwrap_err();
    assert_eq!(e.code, ErrorCode::SchemaInvalid);
    assert!(e.message.contains("outside the integration repo"), "{e}");

    let e = render_overlay(&overlay(Template(repo.join("missing.tmpl"))), &ctx).unwrap_err();
    assert_eq!(e.code, ErrorCode::SchemaInvalid);

    let e = render_overlay(&overlay(Template(repo.join("bad.tmpl"))), &ctx).unwrap_err();
    assert_eq!(e.code, ErrorCode::UnresolvedVariable);
    assert_eq!(e.details["problems"].as_array().unwrap().len(), 2);

    let e = render_overlay(&overlay(Template(repo.join("auto.tmpl"))), &ctx).unwrap_err();
    assert_eq!(e.code, ErrorCode::UnresolvedVariable);
    assert_eq!(e.details["reference"], "stem.api.port");

    // `file:` is verbatim: no substitution, bytes need not be UTF-8.
    let raw = render_overlay(&overlay(File(repo.join("raw.bin"))), &ctx).unwrap();
    assert_eq!(raw, b"${stem.self.port}\xff");
    let e = render_overlay(&overlay(Template(repo.join("raw.bin"))), &ctx).unwrap_err();
    assert!(e.message.contains("UTF-8"));
}
