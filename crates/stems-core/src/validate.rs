//! `stems validate` semantics (FR-WS-6): static checks over a loaded
//! workspace. Every pass runs; all errors are collected and returned sorted
//! by source location. Nothing is started and nothing is written.
//!
//! This lives in stems-core rather than stems-config because it produces
//! [`crate::Error`]s and stems-core already depends on stems-config
//! (DECISIONS.md, "Architectural choices").

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::json;
use stems_config::{
    Codebase, Condition, ConfigPath, IfDisabled, LoadOptions, PortRef, Profile, Requirement,
    Resolved, Script, ScriptSource, SpanIndex, Stem, StemRuntime, StemType, Workspace,
};

use crate::error::{Error, ErrorCode, Errors, sort_errors};
use crate::graph;
use crate::tools::{SystemTools, ToolVersion, ToolVersions, requirement_source, version_command};
use crate::version::{Version, VersionReq};

/// `schema_version` values this binary understands (the table with the
/// minimum stems version per schema lives in [`stems_config::compat`]).
pub use stems_config::compat::SUPPORTED_SCHEMA_VERSIONS;

/// Knobs for [`validate`].
#[derive(Clone, Debug, Default)]
pub struct ValidateOptions {
    /// Do not run `<tool> --version` for `requires:` (`--skip-requires`).
    pub skip_requires: bool,
    /// Overlay destinations (absolute) that stems wrote itself, recorded in
    /// state; these do not count as `OVERLAY_CONFLICT`. Empty until the state
    /// store (11/18) provides them.
    pub owned_overlays: Vec<PathBuf>,
    /// Skip the static `OVERLAY_CONFLICT` check: the daemon decides at start
    /// time instead, by content hash and with `--force-overlays` (18).
    pub skip_overlays: bool,
}

/// Scripts that accept `soft_fail: true` (41): the stem's `setup`, `seed`,
/// `pre_start`, `post_start`, `build` and the workspace `bootstrap`.
pub const SOFT_FAIL_SCRIPTS: [&str; 6] = [
    "setup",
    "seed",
    "pre_start",
    "post_start",
    "build",
    "bootstrap",
];

/// Validate a loaded workspace, probing real tools for `requires:`.
pub fn validate(resolved: &Resolved, opts: &ValidateOptions) -> Vec<Error> {
    validate_with(resolved, opts, &mut SystemTools::new())
}

/// [`validate`] with an injectable tool prober.
pub fn validate_with(
    resolved: &Resolved,
    opts: &ValidateOptions,
    tools: &mut dyn ToolVersions,
) -> Vec<Error> {
    let ws = &resolved.workspace;
    let mut v = Validator {
        ws,
        spans: &resolved.spans,
        out: Vec::new(),
    };
    // A config written for a newer format may mean something else entirely:
    // report only that.
    if !SUPPORTED_SCHEMA_VERSIONS.contains(&ws.schema_version) {
        v.schema_version();
        return v.out;
    }
    v.out
        .extend(resolved.diagnostics.iter().cloned().map(Error::from));
    v.dependencies();
    v.cycles();
    v.ports();
    v.type_requirements();
    v.scripts();
    v.soft_fail();
    v.build_when();
    v.codebases();
    v.env_files();
    v.build_specs();
    if !opts.skip_overlays {
        v.overlays(&opts.owned_overlays);
    }
    v.profiles();
    v.requires_shape();
    for e in crate::checks::shape_errors(ws) {
        v.push(e, None);
    }
    // `when: [validate]` checks (48) are not run here: this function backs
    // every config load (the daemon's `up`/`start`/reloads included), and a
    // check may take up to 30 s. Only `stems validate` runs them, through
    // [`crate::checks::validate_results`], once a load is otherwise valid.
    if !opts.skip_requires {
        v.requires(tools);
    }
    let mut out = v.out;
    sort_errors(&mut out);
    out
}

/// Load a workspace and validate it. `Err` holds every load or validation
/// error, sorted by location.
pub fn load_and_validate(load: LoadOptions, opts: &ValidateOptions) -> Result<Resolved, Errors> {
    let resolved = stems_config::load(load).map_err(|e| {
        let mut es = Errors::from(e);
        es.sort();
        es
    })?;
    let errors = validate(&resolved, opts);
    if errors.is_empty() {
        Ok(resolved)
    } else {
        Err(Errors(errors))
    }
}

/// Warnings (never errors; `validate` still exits 0): `OVERLAY_TRACKED_FILE`
/// for each overlay `dest` tracked in its codebase's git index, since
/// materialising it would dirty the repo (18), and `ENV_SHADOWED` for each
/// env-file key that overrides a different `env:` / earlier-file value
/// (38). Sorted by location.
pub fn warnings(resolved: &Resolved) -> Vec<Error> {
    let ws = &resolved.workspace;
    let mut v = Validator {
        ws,
        spans: &resolved.spans,
        out: Vec::new(),
    };
    for stem in ws.stems() {
        let Some(base) = stem.codebase.as_ref().map(Codebase::path) else {
            continue;
        };
        if !base.is_dir() {
            continue;
        }
        let path = stem_path(&stem.name).key("overlays");
        for (i, o) in stem.overlays.iter().enumerate() {
            if crate::overlays::is_git_tracked(base, &o.dest) != Some(true) {
                continue;
            }
            let e = Error::new(
                ErrorCode::OverlayTrackedFile,
                format!(
                    "overlay destination `{}` of `{}` is tracked in the codebase's git index",
                    o.dest.display(),
                    stem.name
                ),
            )
            .with_path(path.clone())
            .with_hint(format!(
                "materialising it would show up as a change in the repo: add it to .gitignore and `git rm --cached {}`, or choose another `dest`",
                o.dest.display()
            ))
            .with_details(json!({ "stem": stem.name, "dest": base.join(&o.dest) }));
            v.push(e, Some(&path.index(i)));
        }
    }
    // `ENV_SHADOWED` (38): an env file overriding `env:` or an earlier file.
    for stem in ws.stems() {
        for e in crate::envfiles::shadow_warnings(&ws.root, stem) {
            v.push(e, None);
        }
    }
    // `COMPOSE_INJECT_ENV_MIXED` (38): one project, both `inject_env` values.
    for e in inject_env_mixed(ws) {
        v.push(e, None);
    }
    // `REQUIRES_NOT_CHECKED` (40): `from:` in a git codebase not cloned yet.
    for (e, path) in deferred_requires_warnings(ws) {
        v.push(e.with_path(path), None);
    }
    // `DEPENDENCY_DISABLED` and `ENABLED_VAR_UNSET` (46).
    for (e, at) in disabled_warnings(ws) {
        v.push(e, Some(&at));
    }
    let mut out = v.out;
    sort_errors(&mut out);
    out
}

/// The disabled-stem notes of `validate` (46), with the precise path to
/// locate each: `DEPENDENCY_DISABLED` for a hard edge (`if_disabled: skip`,
/// the default) of an enabled stem to a disabled one, which `up`/`start`
/// skip; `ENABLED_VAR_UNSET` (info) for a stem disabled because its
/// `enabled:` names an unset environment variable.
pub fn disabled_warnings(ws: &Workspace) -> Vec<(Error, ConfigPath)> {
    let mut out = Vec::new();
    for stem in ws.stems.values() {
        if !stem.enabled {
            if let Some(t) = stem
                .enabled_template
                .as_ref()
                .filter(|t| !t.unset.is_empty())
            {
                let path = stem_path(&stem.name).key("enabled");
                let e = Error::new(
                    ErrorCode::EnabledVarUnset,
                    format!("{} disabled: {} is unset", stem.name, t.unset.join(", ")),
                )
                .with_path(path.clone())
                .with_hint(format!("export {}=1 to enable `{}`", t.unset[0], stem.name))
                .with_details(json!({
                    "stem": stem.name,
                    "variables": t.unset,
                    "enabled": t.text,
                    "level": "info",
                }));
                out.push((e, path));
            }
            continue;
        }
        let deps_path = stem_path(&stem.name).key("depends_on");
        for (i, dep) in stem.depends_on.iter().enumerate() {
            if dep.soft || dep.if_disabled == IfDisabled::Start {
                continue;
            }
            if ws.stem(&dep.stem).is_none_or(|t| t.enabled) {
                continue;
            }
            let e = Error::new(
                ErrorCode::DependencyDisabled,
                format!(
                    "stem `{}` needs `{}`, which is disabled: `up` skips `{}`",
                    stem.name, dep.stem, stem.name
                ),
            )
            .with_path(deps_path.clone())
            .with_hint(format!(
                "enable `{t}` (usually in stems.local.yaml), or mark the edge `if_disabled: start` (start `{s}` anyway) or `soft: true`",
                t = dep.stem,
                s = stem.name
            ))
            .with_details(json!({ "stem": stem.name, "dependency": dep.stem }));
            out.push((e, deps_path.index(i)));
        }
    }
    out
}

fn stem_path(name: &str) -> ConfigPath {
    ConfigPath::root().key("stems").key(name)
}

/// `p` relative to `root` when inside it, else as is.
fn shown(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

struct Validator<'a> {
    ws: &'a Workspace,
    spans: &'a SpanIndex,
    out: Vec<Error>,
}

impl Validator<'_> {
    /// Push `e` with its span taken from `at` (a more precise path than the
    /// reported one), else from its own path.
    fn push(&mut self, e: Error, at: Option<&ConfigPath>) {
        let span = at
            .and_then(|p| self.spans.locate(p))
            .or_else(|| e.path.as_ref().and_then(|p| self.spans.locate(p)))
            .cloned();
        self.out.push(e.with_span(span));
    }

    fn schema_version(&mut self) {
        let n = self.ws.schema_version;
        let supported: Vec<String> = SUPPORTED_SCHEMA_VERSIONS
            .iter()
            .map(ToString::to_string)
            .collect();
        let this = env!("CARGO_PKG_VERSION");
        let required = stems_config::compat::required_stems(n, this);
        let e = Error::new(
            ErrorCode::SchemaVersionUnsupported,
            format!(
                "schema_version {n} requires stems {required} (this is stems {this}, which supports schema_version {})",
                supported.join(", ")
            ),
        )
        .with_path(ConfigPath::root().key("schema_version"))
        .with_hint(format!(
            "upgrade stems (`stems upgrade`) to a release that supports schema_version {n}, or set `schema_version: {}`",
            stems_config::compat::LATEST_SCHEMA_VERSION
        ))
        .with_details(json!({
            "schema_version": n,
            "supported": SUPPORTED_SCHEMA_VERSIONS,
            "stems_version": this,
            "required_stems": required,
        }));
        self.push(e, None);
    }

    /// Unknown targets and `condition: seeded` without a seed script.
    fn dependencies(&mut self) {
        let ws = self.ws;
        for stem in ws.stems.values() {
            let deps_path = stem_path(&stem.name).key("depends_on");
            for (i, dep) in stem.depends_on.iter().enumerate() {
                let at = deps_path.index(i);
                let Some(target) = ws.stem(&dep.stem) else {
                    let mut hint = String::new();
                    if let Some(s) = closest(&dep.stem, ws.stems.keys()) {
                        hint.push_str(&format!("did you mean `{s}`? otherwise "));
                    }
                    hint.push_str(&format!(
                        "define a stem named `{}` under `stems:` or remove it from {deps_path}",
                        dep.stem
                    ));
                    let e = Error::new(
                        ErrorCode::UnknownDependency,
                        format!(
                            "stem `{}` depends on `{}`, which is not defined",
                            stem.name, dep.stem
                        ),
                    )
                    .with_path(deps_path.clone())
                    .with_hint(hint)
                    .with_details(json!({ "stem": stem.name, "dependency": dep.stem }));
                    self.push(e, Some(&at));
                    continue;
                };
                // A hard edge to a disabled stem is a warning (46, in
                // [`warnings`]): `up` skips the dependant.
                if dep.condition == Condition::Seeded && !target.scripts.contains_key("seed") {
                    let e = Error::new(
                        ErrorCode::SeededWithoutSeed,
                        format!(
                            "stem `{}` waits for `{}` to be seeded, but `{}` has no `seed` script",
                            stem.name, dep.stem, dep.stem
                        ),
                    )
                    .with_path(deps_path.clone())
                    .with_hint(format!(
                        "add a `seed` script to stems.{}.scripts, or use `condition: healthy` on this edge",
                        dep.stem
                    ))
                    .with_details(json!({ "stem": stem.name, "dependency": dep.stem }));
                    self.push(e, Some(&at));
                }
            }
        }
    }

    fn cycles(&mut self) {
        for c in graph::cycles(self.ws) {
            let at = stem_path(&c.stems[0]).key("depends_on").index(c.first_edge);
            self.push(c.to_error(), Some(&at));
        }
    }

    /// Duplicate fixed host ports across enabled stems, and `port: auto` on
    /// containerised stems.
    fn ports(&mut self) {
        let mut seen: HashMap<u16, (String, usize)> = HashMap::new();
        for stem in self.ws.stems() {
            let ports_path = stem_path(&stem.name).key("ports");
            let containerised = matches!(stem.kind(), StemType::Docker | StemType::Compose);
            for (i, p) in stem.ports.iter().enumerate() {
                let at = ports_path.index(i);
                let n = match p.port {
                    PortRef::Fixed(n) => n,
                    PortRef::Auto if containerised => {
                        let e = Error::new(
                            ErrorCode::SchemaInvalid,
                            format!(
                                "`port: auto` is not supported for {} stems (stem `{}`)",
                                stem.kind(),
                                stem.name
                            ),
                        )
                        .with_path(at.clone())
                        .with_hint(
                            "declare a fixed host port for the published container port, e.g. `{ port: 15432, container_port: 5432 }`",
                        );
                        self.push(e, None);
                        continue;
                    }
                    PortRef::Auto => continue,
                };
                match seen.get(&n) {
                    None => {
                        seen.insert(n, (stem.name.clone(), i));
                    }
                    Some((first, fi)) => {
                        let message = if *first == stem.name {
                            format!(
                                "port {n} is declared twice by `{first}` (ports[{fi}] and ports[{i}])"
                            )
                        } else {
                            format!(
                                "port {n} is declared by both `{first}` (stems.{first}.ports[{fi}]) and `{}` ({at})",
                                stem.name
                            )
                        };
                        let fix = if containerised {
                            format!(
                                "map a different host port for `{}` (keep `container_port`, change `port`)",
                                stem.name
                            )
                        } else {
                            format!(
                                "give `{}` a different port, or use `port: auto` to have one allocated",
                                stem.name
                            )
                        };
                        let e = Error::new(ErrorCode::PortConflict, message)
                            .with_path(ports_path.clone())
                            .with_hint(format!(
                                "{fix}; two stems cannot listen on the same host port"
                            ))
                            .with_details(json!({ "port": n, "stems": [first, stem.name] }));
                        self.push(e, Some(&at));
                    }
                }
            }
        }
    }

    /// Things a stem type cannot run without.
    fn type_requirements(&mut self) {
        for stem in self.ws.stems.values() {
            let p = stem_path(&stem.name);
            let problem = match &stem.runtime {
                StemRuntime::Process(spec)
                    if spec.command.is_none() && !stem.scripts.contains_key("start") =>
                {
                    Some((
                        p.clone(),
                        format!("process stem `{}` has nothing to run", stem.name),
                        "set `command:` or a `scripts.start` script".to_string(),
                    ))
                }
                StemRuntime::Docker(d) if d.image.is_none() && d.build.is_none() => Some((
                    p.key("image"),
                    format!(
                        "docker stem `{}` has neither `image` nor `build`",
                        stem.name
                    ),
                    "set `image:` (e.g. `postgres:16`) or `build:` with a context".to_string(),
                )),
                StemRuntime::Compose(c) if c.file.is_none() => Some((
                    p.key("file"),
                    format!("compose stem `{}` has no `file`", stem.name),
                    "set `file:` to the compose file, relative to the integration repo".to_string(),
                )),
                _ => None,
            };
            if let Some((path, message, hint)) = problem {
                let e = Error::new(ErrorCode::SchemaInvalid, message)
                    .with_path(path)
                    .with_hint(hint);
                self.push(e, None);
            }
        }
    }

    /// Script `file:`s exist and stay inside the integration repo; script
    /// `requires:` name known stems.
    fn scripts(&mut self) {
        let ws = self.ws;
        let root_canon = ws.root.canonicalize().unwrap_or_else(|_| ws.root.clone());
        let mut all: Vec<(ConfigPath, &Script)> = ws
            .scripts
            .iter()
            .map(|(k, s)| (ConfigPath::root().key("scripts").key(k), s))
            .collect();
        for stem in ws.stems.values() {
            for (k, s) in &stem.scripts {
                all.push((stem_path(&stem.name).key("scripts").key(k), s));
            }
        }
        for (path, script) in all {
            self.script_file(&path, script, &root_canon);
            let lists = [
                ("requires", &script.requires),
                ("requires_stopped", &script.requires_stopped),
            ];
            for (key, list) in lists {
                for (i, r) in list.iter().enumerate() {
                    if ws.stem(r).is_none() {
                        let at = path.key(key).index(i);
                        let e = Error::new(
                            ErrorCode::UnknownStem,
                            format!("script {path} {key} `{r}`, which is not a stem"),
                        )
                        .with_path(at.clone())
                        .with_hint(format!(
                            "remove `{r}` from {path}.{key} or define a stem named `{r}`"
                        ));
                        self.push(e, None);
                    }
                }
            }
        }
    }

    /// `soft_fail: true` only on the scripts whose failure it can soften
    /// (41): [`SOFT_FAIL_SCRIPTS`] of a stem, the workspace `bootstrap`.
    fn soft_fail(&mut self) {
        let ws = self.ws;
        let mut all: Vec<(ConfigPath, bool)> = ws
            .scripts
            .iter()
            .filter(|(_, s)| s.soft_fail)
            .map(|(k, _)| (ConfigPath::root().key("scripts").key(k), k == "bootstrap"))
            .collect();
        for stem in ws.stems.values() {
            for (k, _) in stem.scripts.iter().filter(|(_, s)| s.soft_fail) {
                let ok = SOFT_FAIL_SCRIPTS.contains(&k.as_str()) && k != "bootstrap";
                all.push((stem_path(&stem.name).key("scripts").key(k), ok));
            }
        }
        for (path, ok) in all {
            if ok {
                continue;
            }
            let at = path.key("soft_fail");
            let e = Error::new(
                ErrorCode::SchemaInvalid,
                format!(
                    "{at}: soft_fail applies to setup, seed, pre_start, post_start, build and bootstrap"
                ),
            )
            .with_path(at.clone())
            .with_hint(format!(
                "remove `soft_fail` from {path}: only start-sequence scripts and `bootstrap` can soft-fail (a failed stop hook never blocks anyway)"
            ));
            self.push(e, None);
        }
    }

    /// `when:` only on a stem's `build` script (45).
    fn build_when(&mut self) {
        let mut bad = Vec::new();
        for (k, _) in self
            .ws
            .scripts
            .iter()
            .filter(|(_, s)| !s.when.is_on_demand())
        {
            bad.push(ConfigPath::root().key("scripts").key(k));
        }
        for stem in self.ws.stems.values() {
            for (k, _) in stem
                .scripts
                .iter()
                .filter(|(k, s)| k.as_str() != "build" && !s.when.is_on_demand())
            {
                bad.push(stem_path(&stem.name).key("scripts").key(k));
            }
        }
        for path in bad {
            let at = path.key("when");
            let e = Error::new(
                ErrorCode::SchemaInvalid,
                format!("{at}: when applies only to a stem's build script"),
            )
            .with_path(at.clone())
            .with_hint(format!(
                "remove `when` from {path}: only `build` can run in the start sequence (`when: changed | always`)"
            ));
            self.push(e, None);
        }
    }

    fn script_file(&mut self, path: &ConfigPath, script: &Script, root_canon: &Path) {
        let ScriptSource::File(f) = &script.source else {
            return;
        };
        let root = &self.ws.root;
        let at = path.key("file");
        let escapes =
            !f.starts_with(root) || f.canonicalize().is_ok_and(|c| !c.starts_with(root_canon));
        if escapes {
            let e = Error::new(
                ErrorCode::ScriptOutsideWorkspace,
                format!(
                    "script file `{}` ({path}) is outside the integration repo {}",
                    f.display(),
                    root.display()
                ),
            )
            .with_path(path.clone())
            .with_hint(
                "scripts live in the integration repo (FR-WS-2): move the script under it and reference it relative to stems.yaml, e.g. `file: scripts/<stem>/<name>.sh`",
            )
            .with_details(json!({ "file": f }));
            self.push(e, Some(&at));
        } else if !f.is_file() {
            let rel = shown(root, f);
            let e = Error::new(
                ErrorCode::ScriptNotFound,
                format!("script file `{rel}` ({path}) does not exist in the integration repo"),
            )
            .with_path(path.clone())
            .with_hint(format!(
                "create `{rel}` in {} or fix the `file:` path (it is relative to the directory of stems.yaml)",
                root.display()
            ))
            .with_details(json!({ "file": f }));
            self.push(e, Some(&at));
        }
    }

    /// Local codebase directories of enabled stems exist (git codebases are
    /// cloned later, so they are not checked).
    /// `ENV_FILE_NOT_FOUND` for a required `env_files` entry that does not
    /// exist, `CONFIG_READ_FAILED` for one that cannot be read (38).
    fn env_files(&mut self) {
        for stem in self.ws.stems() {
            for e in crate::envfiles::errors(&self.ws.root, stem) {
                self.push(e, None);
            }
        }
    }

    /// `build.secrets` and `build.allow_read` (44): secret ids are
    /// non-empty, unique and `[A-Za-z0-9_.-]`; `allow_read` entries are
    /// exact absolute paths (no glob characters `* ? [ {`) and not
    /// symlinks (the final component; grant the target instead).
    fn build_specs(&mut self) {
        for stem in self.ws.stems() {
            let (secrets, allow_read) = match &stem.runtime {
                StemRuntime::Docker(d) => match &d.build {
                    Some(b) => (&b.secrets, &b.allow_read),
                    None => continue,
                },
                StemRuntime::Compose(c) => (&c.build_options.secrets, &c.build_options.allow_read),
                _ => continue,
            };
            let base = stem_path(&stem.name).key("build");
            let mut seen = std::collections::HashSet::new();
            for (i, s) in secrets.iter().enumerate() {
                let at = base.key("secrets").index(i);
                let bad_chars = s.id.is_empty()
                    || !s
                        .id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c));
                let problem = if bad_chars {
                    Some("must be non-empty and use only letters, digits, `_`, `.` and `-`")
                } else if !seen.insert(s.id.as_str()) {
                    Some("is declared twice")
                } else {
                    None
                };
                if let Some(problem) = problem {
                    let e = Error::new(
                        ErrorCode::SchemaInvalid,
                        format!("build secret id `{}` of `{}` {problem}", s.id, stem.name),
                    )
                    .with_path(at.clone())
                    .with_hint(
                        "the id is what the Dockerfile mounts: `RUN --mount=type=secret,id=<id>`",
                    )
                    .with_details(json!({ "stem": stem.name, "secret": s.id }));
                    self.push(e, Some(&at));
                }
            }
            for (i, p) in allow_read.iter().enumerate() {
                let at = base.key("allow_read").index(i);
                let text = p.to_string_lossy();
                let problem = if text.contains(['*', '?', '[', '{']) {
                    Some(
                        "contains a glob character (`*`, `?`, `[` or `{`); list exact paths, wildcards are never allowed",
                    )
                } else if !p.is_absolute() {
                    Some(
                        "is not an absolute path; list exact absolute paths (e.g. `${workspace.root}/../infra` or `${env.HOME}/.config/…`)",
                    )
                } else if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
                    Some("is a symlink; symlinks are rejected, list the path it points to")
                } else {
                    None
                };
                if let Some(problem) = problem {
                    let e = Error::new(
                        ErrorCode::SchemaInvalid,
                        format!("`allow_read` entry `{text}` of `{}` {problem}", stem.name),
                    )
                    .with_path(at.clone())
                    .with_hint("`allow_read` grants exact paths outside the build context (Bake's `fs.read`): no globs, no symlinks; the context, the Dockerfile's directory and secret files are granted already")
                    .with_details(json!({ "stem": stem.name, "path": text }));
                    self.push(e, Some(&at));
                }
            }
        }
    }

    fn codebases(&mut self) {
        for stem in self.ws.stems() {
            // Git forms: the URL must look like one (20).
            if let Some(Codebase::Git { url, .. }) = &stem.codebase
                && !stems_config::is_git_url(url)
            {
                let e = Error::new(
                    ErrorCode::SchemaInvalid,
                    format!("codebase of `{}`: `{url}` is not a git URL", stem.name),
                )
                .with_path(stem_path(&stem.name).key("codebase").key("git"))
                .with_hint("use an https://, ssh://, git@host:path or file:// URL (a local checkout is `codebase: <path>`)")
                .with_details(json!({ "stem": stem.name, "url": url }));
                self.push(e, None);
                continue;
            }
            let Some(Codebase::Local { path }) = &stem.codebase else {
                continue;
            };
            if path.is_dir() {
                continue;
            }
            let what = if path.exists() {
                "is not a directory"
            } else {
                "does not exist"
            };
            let e = Error::new(
                ErrorCode::CodebaseNotFound,
                format!(
                    "codebase of `{}` {what}: {}",
                    stem.name,
                    path.display()
                ),
            )
            .with_path(stem_path(&stem.name).key("codebase"))
            .with_hint(format!(
                "fix `codebase:` (relative paths start at {}) or point it at your checkout in stems.local.yaml: `stems: {{ {}: {{ codebase: ~/src/{} }} }}`",
                self.ws.root.display(),
                stem.name,
                stem.name
            ))
            .with_details(json!({ "stem": stem.name, "codebase": path }));
            self.push(e, None);
        }
    }

    /// Overlay destinations must not overwrite files stems did not write.
    fn overlays(&mut self, owned: &[PathBuf]) {
        for stem in self.ws.stems() {
            let Some(base) = stem.codebase.as_ref().map(Codebase::path) else {
                continue;
            };
            if !base.is_dir() {
                continue;
            }
            let path = stem_path(&stem.name).key("overlays");
            for (i, o) in stem.overlays.iter().enumerate() {
                let dest = base.join(&o.dest);
                let exists = dest.symlink_metadata().is_ok();
                if !exists || owned.iter().any(|p| p == &dest) {
                    continue;
                }
                let e = Error::new(
                    ErrorCode::OverlayConflict,
                    format!(
                        "overlay destination `{}` already exists in the codebase of `{}` and was not written by stems",
                        o.dest.display(),
                        stem.name
                    ),
                )
                .with_path(path.clone())
                .with_hint(format!(
                    "stems never overwrites files it did not create: remove or rename {} if it is a stale copy, or choose another `dest`",
                    dest.display()
                ))
                .with_details(json!({ "stem": stem.name, "dest": dest }));
                self.push(e, Some(&path.index(i)));
            }
        }
    }

    /// Profiles list known stems.
    fn profiles(&mut self) {
        for (name, profile) in &self.ws.profiles {
            let Profile::Stems(list) = profile else {
                continue;
            };
            for (i, s) in list.iter().enumerate() {
                if self.ws.stem(s).is_some() {
                    continue;
                }
                let at = ConfigPath::root().key("profiles").key(name).index(i);
                let mut hint = String::new();
                if let Some(c) = closest(s, self.ws.stems.keys()) {
                    hint.push_str(&format!("did you mean `{c}`? otherwise "));
                }
                hint.push_str(&format!("remove `{s}` from profile `{name}`"));
                let e = Error::new(
                    ErrorCode::UnknownStem,
                    format!("profile `{name}` lists `{s}`, which is not a stem"),
                )
                .with_path(at)
                .with_hint(hint);
                self.push(e, None);
            }
        }
        // Aliases, `default_profile` and the local `profile:` name defined
        // profiles and resolve to a stem list (26).
        for (name, profile) in &self.ws.profiles {
            if let Profile::Alias(_) = profile
                && let Err(e) = crate::selection::resolve_profile(self.ws, name)
            {
                let at = ConfigPath::root().key("profiles").key(name);
                self.push(e.with_path(at.clone()), Some(&at));
            }
        }
        for (key, value) in [
            ("default_profile", &self.ws.default_profile),
            ("profile", &self.ws.profile),
        ] {
            if let Some(p) = value
                && !self.ws.profiles.contains_key(p)
            {
                let at = ConfigPath::root().key(key);
                let e = crate::selection::resolve_profile(self.ws, p)
                    .err()
                    .unwrap_or_else(|| Error::internal("profile lookup"));
                self.push(e.with_path(at.clone()), Some(&at));
            }
        }
    }

    /// `requires:` shape (`version` xor `from`); always runs.
    fn requires_shape(&mut self) {
        let mut errs = Vec::new();
        for (tool, req) in &self.ws.requires {
            if let Some(e) = requirement_shape(&requires_path(None, tool), req) {
                errs.push(e);
            }
        }
        for stem in self.ws.stems() {
            for (tool, req) in &stem.requires {
                if let Some(e) = requirement_shape(&requires_path(Some(&stem.name), tool), req) {
                    errs.push(e);
                }
            }
        }
        for e in errs {
            self.push(e, None);
        }
    }

    /// `requires:` tool versions: the workspace's, then each enabled stem's.
    fn requires(&mut self, tools: &mut dyn ToolVersions) {
        let root = self.ws.root.clone();
        let mut errs = Vec::new();
        for (tool, req) in &self.ws.requires {
            let scope = RequireScope {
                dir: &root,
                stem: None,
            };
            if requirement_shape(&requires_path(None, tool), req).is_some() {
                continue; // reported by `requires_shape`
            }
            if let Err(e) = check_requirement(tool, req, &scope, tools) {
                errs.push(e);
            }
        }
        for stem in self.ws.stems().filter(|s| s.enabled) {
            let dir = requires_dir(stem, &root);
            for (tool, req) in &stem.requires {
                let scope = RequireScope {
                    dir,
                    stem: Some(&stem.name),
                };
                if requirement_shape(&requires_path(Some(&stem.name), tool), req).is_some() {
                    continue;
                }
                if requirement_deferred(stem, req) {
                    continue; // `REQUIRES_NOT_CHECKED` warning (see `warnings`)
                }
                if let Err(e) = check_requirement(tool, req, &scope, tools) {
                    errs.push(e);
                }
            }
        }
        for e in errs {
            self.push(e, None);
        }
    }
}

/// `COMPOSE_INJECT_ENV_MIXED` warnings: enabled compose stems that share a
/// compose project but disagree on `inject_env`. Starting one recreates the
/// project's services with its own setting, so the other may come back
/// without (or with) the injected environment. One warning per stem that
/// sets `inject_env: true`.
fn inject_env_mixed(ws: &Workspace) -> Vec<Error> {
    let mut projects: indexmap::IndexMap<&str, Vec<(&str, bool)>> = indexmap::IndexMap::new();
    for stem in ws.stems().filter(|s| s.enabled) {
        if let StemRuntime::Compose(c) = &stem.runtime {
            projects
                .entry(c.project_name.as_str())
                .or_default()
                .push((stem.name.as_str(), c.inject_env));
        }
    }
    let mut out = Vec::new();
    for (project, stems) in projects {
        let (with, without): (Vec<_>, Vec<_>) = stems.iter().partition(|(_, inject)| *inject);
        if with.is_empty() || without.is_empty() {
            continue;
        }
        let others: Vec<&str> = without.iter().map(|(n, _)| *n).collect();
        for (name, _) in &with {
            let e = Error::new(
                ErrorCode::ComposeInjectEnvMixed,
                format!(
                    "`{name}` and {} share the compose project `{project}` but only `{name}` sets `inject_env: true`",
                    others
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
            .with_path(stem_path(name).key("inject_env"))
            .with_hint(
                "set the same `inject_env` on every stem of the project: compose may recreate a service without the injected environment when another stem of the project starts",
            )
            .with_details(json!({ "stem": name, "project": project, "without": others }));
            out.push(e);
        }
    }
    out
}

/// `true` when `stem` has a git codebase that is not cloned yet: its
/// `from:` files do not exist until `up` clones it (40).
pub fn codebase_uncloned(stem: &Stem) -> bool {
    matches!(&stem.codebase, Some(Codebase::Git { path, .. }) if !path.is_dir())
}

/// A per-stem `requires:` entry that cannot be checked yet: it reads its
/// range `from:` a file of a git codebase that is not cloned. `validate`
/// skips it with a `REQUIRES_NOT_CHECKED` warning, `doctor` reports `warn`.
pub fn requirement_deferred(stem: &Stem, req: &Requirement) -> bool {
    !req.from().is_empty() && codebase_uncloned(stem)
}

/// The directory of `stem`'s `requires:` (`from:` lookups, probe cwd): its
/// codebase, or `root` when it has none or it is not cloned yet.
pub fn requires_dir<'a>(stem: &'a Stem, root: &'a Path) -> &'a Path {
    match &stem.codebase {
        Some(c) if !codebase_uncloned(stem) => c.path(),
        _ => root,
    }
}

/// `REQUIRES_NOT_CHECKED` warnings: per-stem `from:` entries of enabled
/// stems whose git codebase is not cloned yet.
fn deferred_requires_warnings(ws: &Workspace) -> Vec<(Error, ConfigPath)> {
    let mut out = Vec::new();
    for stem in ws.stems().filter(|s| s.enabled) {
        for (tool, req) in &stem.requires {
            if !requirement_deferred(stem, req) {
                continue;
            }
            let path = requires_path(Some(&stem.name), tool);
            let e = Error::new(
                ErrorCode::RequiresNotChecked,
                format!(
                    "`{tool}` of `{}` is not checked: its range comes from {} in a git codebase that is not cloned yet",
                    stem.name,
                    req.from().join(", ")
                ),
            )
            .with_hint(format!(
                "`stems up {}` clones it; run `stems validate` or `stems doctor` again afterwards to check the version",
                stem.name
            ))
            .with_details(json!({ "stem": stem.name, "tool": tool, "from": req.from() }));
            out.push((e, path));
        }
    }
    out
}

/// `requires.<tool>` or `stems.<stem>.requires.<tool>`.
pub fn requires_path(stem: Option<&str>, tool: &str) -> ConfigPath {
    let base = match stem {
        Some(s) => ConfigPath::root().key("stems").key(s),
        None => ConfigPath::root(),
    };
    base.key("requires").key(tool)
}

/// Where a `requires:` entry lives, for [`check_requirement`].
#[derive(Clone, Copy, Debug)]
pub struct RequireScope<'a> {
    /// Directory for `from:` lookups and the version probe's cwd: the stem's
    /// codebase, or the workspace root.
    pub dir: &'a Path,
    /// The stem the entry belongs to; `None` for a workspace-level entry.
    pub stem: Option<&'a str>,
}

/// A satisfied requirement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Satisfied {
    /// The installed version.
    pub version: Version,
    /// The range it satisfies (literal, or derived from `from`).
    pub required: String,
    /// The `from:` entry the range was read from, if any.
    pub from: Option<String>,
}

/// `SCHEMA_INVALID` unless exactly one of `version` and `from` is set.
fn requirement_shape(path: &ConfigPath, req: &Requirement) -> Option<Error> {
    let (has_v, has_f) = (req.version().is_some(), !req.from().is_empty());
    let msg = match (has_v, has_f) {
        (true, false) | (false, true) => return None,
        (true, true) => "set `version` or `from`, not both",
        (false, false) => "set `version` or `from`",
    };
    Some(
        Error::new(ErrorCode::SchemaInvalid, format!("{path}: {msg}"))
            .with_path(path.clone())
            .with_hint(
                "e.g. `node: \">=20\"`, `node: { from: .nvmrc }` or `pnpm: { from: \"package.json#packageManager\" }`",
            ),
    )
}

/// Check one `requires:` entry (`validate`, `doctor`): `Ok` when the
/// installed tool satisfies the range, else `TOOL_VERSION` (with `details
/// { tool, required, found, from?, stem? }`) or `SCHEMA_INVALID` for a bad
/// range, regex or shape. A `from:` range is read from the files under
/// `scope.dir` first. Errors carry the config path `requires.<tool>` or
/// `stems.<stem>.requires.<tool>`.
pub fn check_requirement(
    tool: &str,
    req: &Requirement,
    scope: &RequireScope<'_>,
    tools: &mut dyn ToolVersions,
) -> Result<Satisfied, Error> {
    let path = requires_path(scope.stem, tool);
    if let Some(e) = requirement_shape(&path, req) {
        return Err(e);
    }
    let with_stem = |mut d: serde_json::Value| {
        if let (Some(stem), Some(o)) = (scope.stem, d.as_object_mut()) {
            o.insert("stem".into(), json!(stem));
        }
        d
    };
    let (literal, from) = match req.version() {
        Some(v) => (v.to_string(), None),
        None => match requirement_source(tool, &req.from(), scope.dir) {
            Ok(d) => (d.range, Some(d.from)),
            Err(se) => {
                let mut e = Error::new(ErrorCode::ToolVersion, format!("`{tool}`: {}", se.message))
                    .with_path(path)
                    .with_details(with_stem(
                        json!({ "tool": tool, "required": null, "found": null, "from": req.from() }),
                    ));
                if let Some(h) = se.hint {
                    e = e.with_hint(h);
                }
                return Err(e);
            }
        },
    };
    let range: VersionReq = literal
        .parse()
        .map_err(|err: crate::version::VersionReqError| {
            Error::new(ErrorCode::SchemaInvalid, err.to_string())
                .with_path(path.clone())
                .with_hint("use a range such as `>=20`, `^1.2`, `~3.11` or `>=1 <2`")
        })?;
    let source = from
        .as_ref()
        .map(|f| format!(" (from {f})"))
        .unwrap_or_default();
    let details = |found: Option<String>| {
        let mut d = json!({ "tool": tool, "required": range.as_str(), "found": found });
        if let (Some(f), Some(o)) = (&from, d.as_object_mut()) {
            o.insert("from".into(), json!(f));
        }
        with_stem(d)
    };
    let cmd = version_command(tool, req);
    let e = match tools.detect(tool, req, Some(scope.dir)) {
        ToolVersion::Found { version, .. } if range.matches(version) => {
            return Ok(Satisfied {
                version,
                required: range.as_str().to_string(),
                from,
            });
        }
        ToolVersion::Found { version, output } => Error::new(
            ErrorCode::ToolVersion,
            format!("`{tool}` {version} does not satisfy the required range {range}{source}"),
        )
        .with_hint(format!(
            "install {tool} {range} (e.g. with brew, nvm, asdf or pyenv) and make sure it is first on PATH (`{cmd}` printed `{output}`), or relax requires.{tool}"
        ))
        .with_details(details(Some(version.to_string()))),
        ToolVersion::Missing { reason } => Error::new(
            ErrorCode::ToolVersion,
            format!("`{tool}` {range} is required but not available: {reason}{source}"),
        )
        .with_hint(format!(
            "install {tool} {range} and make sure `{cmd}` works in this shell, or pass --skip-requires to validate without checking tools"
        ))
        .with_details(details(None)),
        ToolVersion::Unparseable { output } => Error::new(
            ErrorCode::ToolVersion,
            format!(
                "could not read a version of `{tool}` (required {range}) from `{cmd}` output `{output}`"
            ),
        )
        .with_hint(format!(
            "use the long form: `requires: {{ {tool}: {{ version: \"{range}\", command: \"<prints the version>\", regex: \"<captures it>\" }} }}`"
        ))
        .with_details(details(None)),
        ToolVersion::BadRegex { reason } => {
            return Err(Error::new(
                ErrorCode::SchemaInvalid,
                format!("invalid `regex` for requires.{tool}: {reason}"),
            )
            .with_path(path.key("regex"))
            .with_hint(
                "fix the regular expression; its first capture group should match the version",
            ));
        }
    };
    Err(e.with_path(path))
}

/// The candidate within edit distance 2 of `name` (closest first), for
/// "did you mean" hints elsewhere in the crate.
pub(crate) fn closest_name<'a>(
    name: &str,
    candidates: impl Iterator<Item = &'a String>,
) -> Option<&'a str> {
    closest(name, candidates)
}

/// The candidate within edit distance 2 of `name` (closest first).
fn closest<'a>(name: &str, candidates: impl Iterator<Item = &'a String>) -> Option<&'a str> {
    candidates
        .map(|c| (edit_distance(name, c), c))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c.as_str())
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}
