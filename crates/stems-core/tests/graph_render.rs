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

// --- cells for the TUI graph widget (deliverable 28) -----------------------

#[test]
fn grid_matches_the_text_rendering_and_tags_boxes() {
    use stems_core::render::render_grid;
    let l = hello_shop();
    let opts = RenderOptions {
        status: Some(&live),
        ..RenderOptions::default()
    };
    let g = render_grid(&l, &opts);
    let text = render_text(&l, &opts);
    let grid_text = g.text();
    let trimmed: Vec<&str> = grid_text.lines().collect();
    let expected: Vec<&str> = text.lines().collect();
    assert_eq!(trimmed[..expected.len()], expected[..]);
    assert_eq!(g.boxes.len(), 6);
    let api = l.node("shop-api").unwrap();
    let b = g.box_of(api).unwrap();
    // The failed box carries a reason line: 4 rows.
    assert_eq!(b.height, 4);
    assert_eq!(g.rows[b.y + 1][b.x + 2].ch, 's');
    assert!(
        g.rows[b.y + 1]
            .iter()
            .any(|c| c.ch == '✗' && c.glyph == Some(Glyph::Failed) && c.node == Some(api))
    );
    assert!(g.rows.iter().flatten().any(|c| c.edge && c.ch == '▶'));
    assert!(
        g.rows
            .iter()
            .flatten()
            .all(|c| !(c.edge && c.node.is_some()))
    );
}

#[test]
fn compact_boxes_are_one_row() {
    use stems_core::render::render_grid;
    let l = hello_shop();
    let opts = RenderOptions {
        compact: true,
        status: Some(&live),
        ..RenderOptions::default()
    };
    let out = render_text(&l, &opts);
    insta::assert_snapshot!("graph-hello-shop-compact", out);
    assert!(
        !out.contains("dep shop-api"),
        "no reason lines when compact"
    );
    let g = render_grid(&l, &opts);
    assert!(g.boxes.iter().all(|b| b.height == 1));
    let full = render_grid(&l, &RenderOptions::default());
    assert!(g.height < full.height && g.width < full.width);
}

#[test]
fn soft_edges_are_marked_in_the_grid() {
    use stems_core::render::render_grid;
    let l = spec(&["web: api, db?", "api: db", "db:"]);
    let g = render_grid(&l, &RenderOptions::default());
    assert!(g.rows.iter().flatten().any(|c| c.soft && c.ch == '╌'));
    assert!(
        g.rows
            .iter()
            .flatten()
            .any(|c| c.edge && !c.soft && c.ch == '─')
    );
}

#[test]
fn graph_reasons_are_short() {
    use stems_core::render::graph_reason;
    assert_eq!(
        graph_reason(Glyph::Degraded, Some("dependency shop-api unhealthy")).as_deref(),
        Some("dep shop-api")
    );
    assert_eq!(
        graph_reason(Glyph::Degraded, Some("dependency a failed; flapping")).as_deref(),
        Some("dep a; flapping")
    );
    assert_eq!(
        graph_reason(Glyph::Failed, Some("exited with\n code 1")).as_deref(),
        Some("exited with code 1")
    );
    assert_eq!(graph_reason(Glyph::Healthy, Some("ready (tcp)")), None);
    assert_eq!(graph_reason(Glyph::Failed, None), None);
}

#[test]
fn from_parts_equals_layout_of_the_workspace() {
    use stems_config::StemType;
    use stems_core::layout::from_parts;
    let ws = load_dir(&repo_root().join("examples/workspaces/hello-shop"));
    let stems: Vec<(String, StemType)> = ws.stems().map(|s| (s.name.clone(), s.kind())).collect();
    let deps: Vec<(String, stems_config::Dependency)> = ws
        .stems()
        .flat_map(|s| s.depends_on.iter().map(|d| (s.name.clone(), d.clone())))
        .collect();
    assert_eq!(from_parts(stems, &deps), layout(&ws).unwrap());
    // Unknown targets are dropped.
    let d: stems_config::Dependency =
        serde_json::from_value(serde_json::json!({"stem": "ghost", "condition": "started", "soft": false, "protocol": null, "via": null})).unwrap();
    let l = from_parts([("a".to_string(), StemType::Process)], &[("a".into(), d)]);
    assert_eq!(l.edges.len(), 0);
}

#[test]
fn restrict_keeps_named_stems_and_their_edges() {
    let l = hello_shop().restrict(&["shop-api", "postgres", "redis", "nope"]);
    let mut names: Vec<&str> = l.nodes.iter().map(|n| n.name.as_str()).collect();
    names.sort();
    assert_eq!(names, ["postgres", "redis", "shop-api"]);
    assert_eq!(l.edges.len(), 2);
}

#[test]
fn unrelated_parts_are_not_separated_by_tall_gaps() {
    let l = spec(&[
        "gateway: auth, catalog, cart",
        "admin: auth, catalog",
        "auth: users",
        "catalog: search, db",
        "cart: db, cache",
        "search: index",
        "users: db",
        "index:",
        "db:",
        "cache:",
        "mailer: queue",
        "queue:",
    ]);
    let out = text(&l);
    assert!(!out.contains("\n\n\n"), "{out}");
}
