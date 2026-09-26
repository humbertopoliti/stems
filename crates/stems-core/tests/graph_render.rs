//! Goldens of `stems graph` renderings (FR-GR-4, FR-GR-5). Review changes
//! visually: these are the user-facing product.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use stems_config::Workspace;
use stems_core::layout::{Layout, layout};
use stems_core::render::{RenderOptions, render_dot, render_mermaid, render_text, to_json};
use stems_core::status::Glyph;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn load_dir(dir: &Path) -> Workspace {
    stems_config::load(stems_config::LoadOptions {
        workspace: Some(dir.to_path_buf()),
        cwd: repo_root(),
        env: Default::default(),
        skip_local: true,
    })
    .unwrap_or_else(|e| panic!("{e}"))
    .workspace
}

fn load_yaml(yaml: &str) -> Workspace {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("stems.yaml"), yaml).unwrap();
    load_dir(dir.path())
}

fn hello_shop() -> Layout {
    layout(&load_dir(
        &repo_root().join("examples/workspaces/hello-shop"),
    ))
    .unwrap()
}

/// `name: [dep, dep?soft]` lines to a workspace of process stems.
fn spec(lines: &[&str]) -> Layout {
    let mut yaml = String::from("stems:\n");
    for l in lines {
        let (name, deps) = l.split_once(':').unwrap();
        let _ = writeln!(
            yaml,
            "  {}:\n    type: process\n    depends_on:",
            name.trim()
        );
        let deps: Vec<&str> = deps
            .split(',')
            .map(str::trim)
            .filter(|d| !d.is_empty())
            .collect();
        if deps.is_empty() {
            yaml.truncate(yaml.len() - "\n    depends_on:".len() - 1);
            yaml.push('\n');
        }
        for d in deps {
            let (d, soft) = d.strip_suffix('?').map_or((d, false), |d| (d, true));
            let _ = writeln!(yaml, "      - {{ stem: {d}, soft: {soft} }}");
        }
    }
    layout(&load_yaml(&yaml)).unwrap()
}

fn text(l: &Layout) -> String {
    render_text(l, &RenderOptions::default())
}

#[test]
fn hello_shop_text() {
    insta::assert_snapshot!("graph-hello-shop", text(&hello_shop()));
}

#[test]
fn hello_shop_focus_text() {
    insta::assert_snapshot!("graph-focus", text(&hello_shop().focus("shop-api")));
}

#[test]
fn hello_shop_ascii() {
    let opts = RenderOptions {
        unicode: false,
        ..RenderOptions::default()
    };
    insta::assert_snapshot!("graph-hello-shop-ascii", render_text(&hello_shop(), &opts));
}

fn live(name: &str) -> (Glyph, Option<String>) {
    match name {
        "shop-api" => (Glyph::Failed, Some("health: 503".into())),
        "shop-web" => (Glyph::Degraded, Some("dep shop-api".into())),
        "shop-worker" => (Glyph::Transitioning, None),
        "httpbin" => (Glyph::Unknown, None),
        _ => (Glyph::Healthy, None),
    }
}

#[test]
fn hello_shop_live_status() {
    let opts = RenderOptions {
        status: Some(&live),
        ..RenderOptions::default()
    };
    insta::assert_snapshot!("graph-hello-shop-live", render_text(&hello_shop(), &opts));
}

#[test]
fn diamond() {
    let l = spec(&["top: left, right", "left: base", "right: base", "base:"]);
    insta::assert_snapshot!("graph-diamond", text(&l));
}

#[test]
fn chain() {
    let l = spec(&["web: api", "api: db", "db:"]);
    insta::assert_snapshot!("graph-chain", text(&l));
}

#[test]
fn soft_and_long_edges_with_labels() {
    let yaml = "stems:
  web:
    type: process
    depends_on:
      - { stem: api, protocol: http, via: ':8080' }
      - { stem: db, soft: true }
  api:
    type: process
    depends_on:
      - { stem: db, condition: seeded, protocol: tcp }
      - { stem: queue, protocol: amqp }
  queue: { type: process, depends_on: [{ stem: db, soft: true }] }
  db: { type: process, depends_on: [{ stem: web, soft: true }] }
";
    let l = layout(&load_yaml(yaml)).unwrap();
    let opts = RenderOptions {
        edge_labels: true,
        ..RenderOptions::default()
    };
    insta::assert_snapshot!("graph-soft-labels", render_text(&l, &opts));
}

/// Deterministic pseudo-random DAG of `n` stems.
fn random_dag(n: usize, seed: u64) -> Layout {
    let mut x = seed;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut lines = Vec::new();
    for i in 0..n {
        let mut deps = std::collections::BTreeSet::new();
        if i > 0 {
            for _ in 0..(next() % 3) {
                deps.insert(next() % i as u64);
            }
        }
        let deps: Vec<String> = deps.into_iter().map(|d| format!("s{d:02}")).collect();
        lines.push(format!("s{i:02}: {}", deps.join(", ")));
    }
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    spec(&refs)
}

#[test]
fn random_dag_20() {
    let l = random_dag(20, 0x5eed);
    assert_eq!(l, random_dag(20, 0x5eed));
    insta::assert_snapshot!("graph-random-20", text(&l));
}

#[test]
fn narrow_width_wraps_into_bands() {
    let opts = RenderOptions {
        width: 40,
        ..RenderOptions::default()
    };
    let out = render_text(&hello_shop(), &opts);
    assert!(out.lines().all(|l| l.chars().count() <= 40), "{out}");
    insta::assert_snapshot!("graph-hello-shop-narrow", out);
}

#[test]
fn color_wraps_glyphs_only() {
    let opts = RenderOptions {
        color: true,
        status: Some(&live),
        ..RenderOptions::default()
    };
    let out = render_text(&hello_shop(), &opts);
    assert!(out.contains("\x1b[31m✗\x1b[0m"));
    assert!(out.contains("\x1b[33m!\x1b[0m"));
}

#[test]
fn hello_shop_mermaid() {
    insta::assert_snapshot!(
        "graph-hello-shop-mermaid",
        render_mermaid(&hello_shop(), &RenderOptions::default())
    );
}

#[test]
fn hello_shop_mermaid_live() {
    let opts = RenderOptions {
        status: Some(&live),
        edge_labels: true,
        ..RenderOptions::default()
    };
    insta::assert_snapshot!(
        "graph-hello-shop-mermaid-live",
        render_mermaid(&hello_shop(), &opts)
    );
}

#[test]
fn hello_shop_dot() {
    insta::assert_snapshot!(
        "graph-hello-shop-dot",
        render_dot(&hello_shop(), &RenderOptions::default())
    );
}

#[test]
fn hello_shop_json() {
    let v = to_json(&hello_shop(), None);
    assert_eq!(v["nodes"].as_array().unwrap().len(), 6);
    assert_eq!(v["edges"].as_array().unwrap().len(), 5);
    insta::assert_snapshot!(
        "graph-hello-shop-json",
        serde_json::to_string_pretty(&v).unwrap()
    );
}

#[test]
fn empty_workspace_renders_nothing() {
    let l = layout(&load_yaml("stems: {}\n")).unwrap();
    assert_eq!(render_text(&l, &RenderOptions::default()), "\n");
}
