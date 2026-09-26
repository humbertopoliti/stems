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
    Codebase, Condition, ConfigPath, LoadOptions, PortRef, Profile, Requirement, Resolved, Script,
    ScriptSource, SpanIndex, StemRuntime, StemType, Workspace,
};

use crate::error::{Error, ErrorCode, Errors, sort_errors};
use crate::graph;
use crate::tools::{SystemTools, ToolVersion, ToolVersions, version_command};
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
    v.codebases();
    if !opts.skip_overlays {
        v.overlays(&opts.owned_overlays);
    }
    v.profiles();
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
/// materialising it would dirty the repo (18). Sorted by location.
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
    let mut out = v.out;
    sort_errors(&mut out);
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

    /// Unknown targets, hard edges to disabled stems, `condition: seeded`
    /// without a seed script.
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
                if stem.enabled && !dep.soft && !target.enabled {
                    let e = Error::new(
                        ErrorCode::DependencyDisabled,
                        format!(
                            "stem `{}` needs `{}`, which is disabled (`enabled: false`)",
                            stem.name, dep.stem
                        ),
                    )
                    .with_path(deps_path.clone())
                    .with_hint(format!(
                        "enable `{t}` (remove `enabled: false`, usually in stems.local.yaml), disable `{s}` too, or mark the edge `soft: true`",
                        t = dep.stem,
                        s = stem.name
                    ))
                    .with_details(json!({ "stem": stem.name, "dependency": dep.stem }));
                    self.push(e, Some(&at));
                }
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
            for (i, r) in script.requires.iter().enumerate() {
                if ws.stem(r).is_none() {
                    let at = path.key("requires").index(i);
                    let e = Error::new(
                        ErrorCode::UnknownStem,
                        format!("script {path} requires `{r}`, which is not a stem"),
                    )
                    .with_path(at.clone())
                    .with_hint(format!(
                        "remove `{r}` from {path}.requires or define a stem named `{r}`"
                    ));
                    self.push(e, None);
                }
            }
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

    /// `requires:` tool versions.
    fn requires(&mut self, tools: &mut dyn ToolVersions) {
        for (tool, req) in &self.ws.requires {
            if let Err(e) = check_requirement(tool, req, tools) {
                self.push(e, None);
            }
        }
    }
}

/// Check one `requires:` entry (`validate`, `doctor`): `Ok(found version)`
/// when the installed tool satisfies the range, else `TOOL_VERSION` (with
/// `details { tool, required, found }`) or `SCHEMA_INVALID` for a bad range
/// or regex. Errors carry the config path `requires.<tool>`.
pub fn check_requirement(
    tool: &str,
    req: &Requirement,
    tools: &mut dyn ToolVersions,
) -> Result<Version, Error> {
    let path = ConfigPath::root().key("requires").key(tool);
    let range: VersionReq =
        req.version()
            .parse()
            .map_err(|err: crate::version::VersionReqError| {
                Error::new(ErrorCode::SchemaInvalid, err.to_string())
                    .with_path(path.clone())
                    .with_hint("use a range such as `>=20`, `^1.2`, `~3.11` or `>=1 <2`")
            })?;
    let details =
        |found: Option<String>| json!({ "tool": tool, "required": range.as_str(), "found": found });
    let cmd = version_command(tool, req);
    let e = match tools.detect(tool, req) {
        ToolVersion::Found { version, .. } if range.matches(version) => return Ok(version),
        ToolVersion::Found { version, output } => Error::new(
            ErrorCode::ToolVersion,
            format!("`{tool}` {version} does not satisfy the required range {range}"),
        )
        .with_hint(format!(
            "install {tool} {range} (e.g. with brew, nvm, asdf or pyenv) and make sure it is first on PATH (`{cmd}` printed `{output}`), or relax requires.{tool}"
        ))
        .with_details(details(Some(version.to_string()))),
        ToolVersion::Missing { reason } => Error::new(
            ErrorCode::ToolVersion,
            format!("`{tool}` {range} is required but not available: {reason}"),
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
