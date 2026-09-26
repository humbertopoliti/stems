//! `stems init [--name n] [--from minimal|hello-shop] [--force]` (FR-WS-5):
//! scaffold an integration repo in the current (or `--workspace`) directory.
//!
//! Writes `stems.yaml` (first line: a `yaml-language-server` `$schema`
//! reference to `schema/stems.schema.json`), `.gitignore` (created, or the
//! missing lines appended), `scripts/`, `env/` and `overlays/`. With
//! `--from`, the example workspace is embedded in the binary and its
//! `codebase:` paths are rewritten to `./repos/<name>` placeholders (empty
//! directories created here, so the result passes
//! `stems validate --skip-requires`). Existing files other than `stems.yaml`
//! are never overwritten; an existing `stems.yaml` is `ALREADY_INITIALISED`
//! (exit 1) unless `--force`.
//!
//! Plain file writes are fine here: nothing is edited in place (DECISIONS.md
//! reserves line-level YAML editing for `add` / `config set`).

use std::path::{Path, PathBuf};

use serde_json::json;
use stems_config::CONFIG_FILE;
use stems_core::{Error, ErrorCode};

use crate::cli::{InitArgs, Template};
use crate::commands::Ctx;
use crate::output::CommandOutput;

/// JSON Schema referenced by generated `stems.yaml` files.
pub const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/humbertopoliti/stems/main/schema/stems.schema.json";

/// Lines every integration repo's `.gitignore` needs.
pub const GITIGNORE_LINES: &[&str] = &[".stems/", "stems.local.yaml"];

macro_rules! example {
    ($rel:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/workspaces/",
            $rel
        ))
    };
}

const MINIMAL_YAML: &str = example!("minimal/stems.yaml");
const HELLO_SHOP_YAML: &str = example!("hello-shop/stems.yaml");

/// Extra files of the hello-shop example: (path, contents, executable).
const HELLO_SHOP_FILES: &[(&str, &str, bool)] = &[
    (
        "compose/redis.yml",
        example!("hello-shop/compose/redis.yml"),
        false,
    ),
    (
        "env/shop-api.env",
        example!("hello-shop/env/shop-api.env"),
        false,
    ),
    (
        "overlays/shop-api/local.ini.tmpl",
        example!("hello-shop/overlays/shop-api/local.ini.tmpl"),
        false,
    ),
    (
        "scripts/bootstrap.sh",
        example!("hello-shop/scripts/bootstrap.sh"),
        true,
    ),
    (
        "scripts/teardown.sh",
        example!("hello-shop/scripts/teardown.sh"),
        true,
    ),
    (
        "scripts/nuke.sh",
        example!("hello-shop/scripts/nuke.sh"),
        true,
    ),
    (
        "scripts/postgres/seed.sh",
        example!("hello-shop/scripts/postgres/seed.sh"),
        true,
    ),
    (
        "scripts/postgres/seed-large.sh",
        example!("hello-shop/scripts/postgres/seed-large.sh"),
        true,
    ),
    (
        "scripts/shop-api/setup.sh",
        example!("hello-shop/scripts/shop-api/setup.sh"),
        true,
    ),
    (
        "scripts/shop-api/create-test-user.sh",
        example!("hello-shop/scripts/shop-api/create-test-user.sh"),
        true,
    ),
    (
        "stems.local.yaml.example",
        example!("hello-shop/stems.local.yaml.example"),
        false,
    ),
];

/// A file to scaffold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScaffoldFile {
    /// Path relative to the workspace root.
    pub path: String,
    /// Contents.
    pub contents: String,
    /// Mode 0755 instead of 0644.
    pub executable: bool,
}

/// Everything `init` writes, before touching the disk.
#[derive(Clone, Debug)]
pub struct Scaffold {
    /// The `stems.yaml` text.
    pub config: String,
    /// Other files (never overwritten when present).
    pub files: Vec<ScaffoldFile>,
}

/// Build the scaffold for `name` from `from` (or the empty template).
pub fn scaffold(name: &str, from: Option<Template>) -> Scaffold {
    let (config, mut files) = match from {
        None => (empty_config(name), Vec::new()),
        Some(Template::Minimal) => (from_example(MINIMAL_YAML, name, "minimal"), Vec::new()),
        Some(Template::HelloShop) => {
            let files = HELLO_SHOP_FILES
                .iter()
                .map(|(p, c, x)| ScaffoldFile {
                    path: (*p).to_string(),
                    contents: c.replace("}/../../repos/", "}/repos/"),
                    executable: *x,
                })
                .collect();
            (from_example(HELLO_SHOP_YAML, name, "hello-shop"), files)
        }
    };
    for repo in placeholder_repos(&config) {
        files.push(keep(&format!("repos/{repo}/.gitkeep")));
    }
    for dir in ["scripts", "env", "overlays"] {
        let prefix = format!("{dir}/");
        if !files.iter().any(|f| f.path.starts_with(&prefix)) {
            files.push(keep(&format!("{dir}/.gitkeep")));
        }
    }
    Scaffold { config, files }
}

fn keep(path: &str) -> ScaffoldFile {
    ScaffoldFile {
        path: path.to_string(),
        contents: String::new(),
        executable: false,
    }
}

fn header(name: &str, from: Option<&str>) -> String {
    let origin = from.map_or_else(
        || "`stems init`".to_string(),
        |f| format!("`stems init --from {f}`"),
    );
    let mut h = format!(
        "# yaml-language-server: $schema={SCHEMA_URL}\n\
         #\n\
         # stems.yaml — the {name} integration repo (scaffolded by {origin}).\n\
         # Check it with `stems validate`; per-machine overrides go in\n\
         # stems.local.yaml (gitignored).\n"
    );
    if from.is_some() {
        h.push_str(
            "#\n\
             # Codebases point at empty ./repos/<name> placeholders: replace each\n\
             # `codebase:` with the path of your checkout (or a git URL).\n",
        );
    }
    h.push('\n');
    h
}

fn empty_config(name: &str) -> String {
    format!(
        "{}\
         # Declare each stem under `stems:` (type process, docker, compose or\n\
         # external), for example:\n\
         #\n\
         #   stems:\n\
         #     api:\n\
         #       type: process\n\
         #       codebase: ../api\n\
         #       ports: [{{ name: http, port: 8080 }}]\n\
         #       scripts:\n\
         #         start: npm start\n\
         \n\
         schema_version: 1\n\
         name: {name}\n\
         \n\
         stems: {{}}\n",
        header(name, None)
    )
}

/// The example's text with its leading comment block replaced by our header,
/// `name:` set and `codebase: ../../repos/<x>` rewritten to `./repos/<x>`.
fn from_example(text: &str, name: &str, from: &str) -> String {
    let mut out = header(name, Some(from));
    let mut in_header = true;
    let mut named = false;
    for line in text.lines() {
        if in_header {
            if line.starts_with('#') || line.trim().is_empty() {
                continue;
            }
            in_header = false;
        }
        if !named && line.starts_with("name:") {
            out.push_str(&format!("name: {name}\n"));
            named = true;
            continue;
        }
        out.push_str(&line.replace("codebase: ../../repos/", "codebase: ./repos/"));
        out.push('\n');
    }
    out
}

/// Names `x` of every `codebase: ./repos/x` line.
fn placeholder_repos(config: &str) -> Vec<String> {
    let mut out: Vec<String> = config
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("codebase: ./repos/"))
        .map(|r| r.trim().trim_end_matches('/').to_string())
        .filter(|r| !r.is_empty())
        .collect();
    out.dedup();
    out
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// A workspace name from a directory name (invalid characters become `-`).
pub fn name_from_dir(dir: &Path) -> String {
    let base = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let s: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() { "workspace".into() } else { s }
}

fn io_error(what: &str, path: &Path, e: &std::io::Error) -> Error {
    Error::new(
        ErrorCode::Internal,
        format!("cannot {what} {}: {e}", path.display()),
    )
    .with_hint("check that the directory is writable by your user")
    .with_details(json!({ "path": path }))
}

/// Run `init`.
pub fn run(ctx: &Ctx, args: &InitArgs) -> CommandOutput {
    let dir: PathBuf = match &ctx.global.workspace {
        Some(p) => {
            let p = ctx.cwd.join(p);
            if p.file_name().is_some_and(|n| n == CONFIG_FILE) {
                p.parent().map(Path::to_path_buf).unwrap_or(p)
            } else {
                p
            }
        }
        None => ctx.cwd.clone(),
    };
    let name = match &args.name {
        Some(n) if !valid_name(n) => {
            return CommandOutput::failed(Error::usage(
                format!("invalid workspace name `{n}`"),
                "use letters, digits, `-`, `_` and `.` only",
            ));
        }
        Some(n) => n.clone(),
        None => name_from_dir(&dir.canonicalize().unwrap_or_else(|_| dir.clone())),
    };
    let config_path = dir.join(CONFIG_FILE);
    if config_path.exists() && !args.force {
        return CommandOutput::failed(
            Error::new(
                ErrorCode::AlreadyInitialised,
                format!("{} already exists", config_path.display()),
            )
            .with_hint("edit the existing stems.yaml, or pass --force to overwrite it")
            .with_details(json!({ "path": config_path })),
        );
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return CommandOutput::failed(io_error("create", &dir, &e));
    }

    let sc = scaffold(&name, args.from);
    let mut created: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    if let Err(e) = std::fs::write(&config_path, &sc.config) {
        return CommandOutput::failed(io_error("write", &config_path, &e));
    }
    created.push(CONFIG_FILE.to_string());

    match update_gitignore(&dir) {
        Ok(true) => created.push(".gitignore".into()),
        Ok(false) => skipped.push(".gitignore".into()),
        Err(e) => return CommandOutput::failed(io_error("write", &dir.join(".gitignore"), &e)),
    }

    for f in &sc.files {
        let path = dir.join(&f.path);
        if path.exists() {
            skipped.push(f.path.clone());
            continue;
        }
        if let Err(e) = write_file(&path, &f.contents, f.executable) {
            return CommandOutput::failed(io_error("write", &path, &e));
        }
        created.push(f.path.clone());
    }

    let from = args.from.map(|t| match t {
        Template::Minimal => "minimal",
        Template::HelloShop => "hello-shop",
    });
    let mut human = format!("initialised workspace `{name}` in {}\n", dir.display());
    for c in &created {
        human.push_str(&format!("  created {c}\n"));
    }
    for s in &skipped {
        human.push_str(&format!("  kept    {s} (already present)\n"));
    }
    human.push_str("next: edit stems.yaml, then run `stems validate`\n");
    CommandOutput::data(json!({
        "workspace": dir,
        "config_file": config_path,
        "name": name,
        "from": from,
        "schema": SCHEMA_URL,
        "created": created,
        "skipped": skipped,
    }))
    .with_human(human)
}

/// Create `.gitignore` or append the missing [`GITIGNORE_LINES`]. Returns
/// whether anything was written.
fn update_gitignore(dir: &Path) -> std::io::Result<bool> {
    let path = dir.join(".gitignore");
    let existing = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let present: Vec<&str> = existing.lines().map(str::trim).collect();
    let missing: Vec<&str> = GITIGNORE_LINES
        .iter()
        .copied()
        .filter(|l| !present.contains(l))
        .collect();
    if missing.is_empty() {
        return Ok(false);
    }
    let mut text = existing.clone();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if text.is_empty() {
        text.push_str("# stems: per-machine state and overrides\n");
    }
    for l in missing {
        text.push_str(l);
        text.push('\n');
    }
    std::fs::write(&path, text)?;
    Ok(true)
}

fn write_file(path: &Path, contents: &str, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    if executable {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_scaffold_rewrites_codebases_and_name() {
        let sc = scaffold("demo", Some(Template::Minimal));
        assert!(
            sc.config
                .starts_with("# yaml-language-server: $schema=https://")
        );
        assert!(sc.config.contains("schema/stems.schema.json"));
        assert!(sc.config.contains("\nname: demo\n"));
        assert!(sc.config.contains("codebase: ./repos/shop-api"));
        assert!(!sc.config.contains("../../repos"));
        let paths: Vec<&str> = sc.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "repos/shop-api/.gitkeep",
                "scripts/.gitkeep",
                "env/.gitkeep",
                "overlays/.gitkeep"
            ]
        );
    }

    #[test]
    fn hello_shop_scaffold_has_scripts_and_three_placeholders() {
        let sc = scaffold("shop", Some(Template::HelloShop));
        assert!(sc.config.contains("\nname: shop\n"));
        assert_eq!(
            placeholder_repos(&sc.config),
            ["shop-api", "shop-worker", "shop-web"]
        );
        assert!(
            sc.files
                .iter()
                .any(|f| f.path == "scripts/postgres/seed.sh" && f.executable)
        );
        assert!(!sc.files.iter().any(|f| f.contents.contains("../../repos")));
        assert!(!sc.files.iter().any(|f| f.path == "scripts/.gitkeep"));
    }

    #[test]
    fn empty_template_is_valid_yaml_with_no_stems() {
        let sc = scaffold("x", None);
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&sc.config).unwrap();
        assert_eq!(v["name"], serde_yaml_ng::Value::from("x"));
        assert!(v["stems"].as_mapping().is_some_and(|m| m.is_empty()));
    }

    #[test]
    fn names_from_directories() {
        assert_eq!(name_from_dir(Path::new("/a/My Project")), "My-Project");
        assert_eq!(name_from_dir(Path::new("/a/ok_name.1")), "ok_name.1");
        assert_eq!(name_from_dir(Path::new("/")), "workspace");
    }
}
