//! Golden of an overlay template rendered for a real workspace (18): the
//! hello-shop `shop-api` overlay (`${stem.postgres.port}`, `${var.pg_user}`,
//! `${stem.self.port}`, and a `$${…}`-escaped placeholder in a comment), and
//! the overlays fixture's template with an allocated `auto` port.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use stems_core::overlays::{RenderCtx, hash_bytes, render_overlay, resolve_dest};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn load(rel: &str) -> stems_config::Resolved {
    stems_config::load(stems_config::LoadOptions {
        workspace: Some(repo_root().join(rel)),
        cwd: repo_root(),
        env: HashMap::new(),
        skip_local: true,
    })
    .unwrap()
}

#[test]
fn hello_shop_local_ini() {
    let r = load("examples/workspaces/hello-shop");
    let ws = &r.workspace;
    let stem = ws.stem("shop-api").unwrap();
    let ctx = RenderCtx::for_stem(ws, stem, HashMap::new());
    let bytes = render_overlay(&stem.overlays[0], &ctx).unwrap();
    insta::assert_snapshot!(String::from_utf8(bytes.clone()).unwrap(), @r#"
    ; overlays/shop-api/local.ini.tmpl
    ;
    ; Materialised by stems into shop-api's codebase at config/local.ini
    ; (never committed there) before shop-api starts, and removed again on
    ; `stems down` unless `keep: true` is set on the overlay (FR-WS-7).
    ;
    ; Variable substitution supports ${stem.<name>.port}, and 18080
    ; as a shorthand for "this stem's own port" so a codebase-relative template
    ; doesn't need to know its own stem name.
    [database]
    host = localhost
    port = 15432
    user = shop

    [server]
    port = 18080
    "#);
    assert_eq!(hash_bytes(&bytes).len(), 64);
    let dest = resolve_dest(&ctx.codebase, &stem.overlays[0].dest).unwrap();
    assert!(
        dest.ends_with("examples/repos/shop-api/config/local.ini"),
        "{}",
        dest.display()
    );
}

#[test]
fn auto_ports_render_once_set() {
    let r = load("tests/fixtures/workspaces/overlays");
    let ws = &r.workspace;
    let stem = ws.stem("shop-api").unwrap();
    let mut ctx = RenderCtx::for_stem(ws, stem, HashMap::new());
    // A runtime port (the daemon sets allocated/overridden ports this way).
    assert!(ctx.set_port("shop-worker", "control", 40001));
    assert!(!ctx.set_port("shop-worker", "nope", 1));
    let text = String::from_utf8(render_overlay(&stem.overlays[0], &ctx).unwrap()).unwrap();
    assert!(text.contains("port = 18422\n"), "{text}");
    assert!(text.contains("control_port = 40001\n"), "{text}");
    // `file:` overlays are verbatim.
    let kept = String::from_utf8(render_overlay(&stem.overlays[1], &ctx).unwrap()).unwrap();
    assert!(kept.contains("kept = ${not.substituted}"), "{kept}");
}
