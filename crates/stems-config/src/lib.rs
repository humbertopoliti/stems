//! `stems-config`: loads `stems.yaml` (+ `extends`, `include`s and
//! `stems.local.yaml`), merges, applies defaults, substitutes `${…}`
//! references and produces a resolved [`Workspace`]. Also generates the JSON
//! Schema for editors. Validation semantics live in `stems_core::validate`.
//!
//! Entry point: [`load`]. See the crate README for merge and substitution rules.

mod codebase;
pub mod compat;
pub mod defaults;
mod diagnostic;
mod discover;
pub mod edit;
mod loader;
mod merge;
mod model;
mod path;
pub mod raw;
mod resolve;
mod spans;
mod subst;
mod types;
pub mod variants;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_yaml_ng::{Mapping, Value};

pub use codebase::{expand_tilde, is_git_url, normalize};
pub use diagnostic::{ConfigErrors, Diagnostic, Span, codes};
pub use discover::{CONFIG_FILE, ENV_WORKSPACE, LOCAL_FILE, discover};
pub use loader::MAX_EXTENDS_DEPTH;
pub use merge::merge as merge_values;
pub use model::*;
pub use path::{ConfigPath, ParsePathError, Segment};
pub use spans::SpanIndex;
pub use subst::{
    DeferredKind, DeferredRef, StemFacts, Substituted, TemplateContext, substitute_template,
};
pub use types::*;

use crate::loader::{Loader, yaml_error};
use crate::raw::{RawCodebase, RawWorkspace};
use crate::resolve::{Ctx, resolve_ports, resolve_workspace};
use crate::subst::{Scope, Substituter};

/// Name of this crate, used to prove the workspace wiring in tests.
pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");

/// How to find and load a workspace.
#[derive(Clone, Debug, Default)]
pub struct LoadOptions {
    /// `--workspace`: a directory or a `stems.yaml` path (relative to `cwd`).
    pub workspace: Option<PathBuf>,
    /// Directory discovery walks up from.
    pub cwd: PathBuf,
    /// Environment used for `STEMS_WORKSPACE`, `${env.X}` and `~` (`HOME`).
    pub env: HashMap<String, String>,
    /// Ignore `stems.local.yaml`.
    pub skip_local: bool,
}

impl LoadOptions {
    /// Options from the current process (cwd and environment).
    pub fn from_process() -> std::io::Result<Self> {
        Ok(Self {
            workspace: None,
            cwd: std::env::current_dir()?,
            env: std::env::vars().collect(),
            skip_local: false,
        })
    }
}

/// A loaded workspace.
#[derive(Clone, Debug)]
pub struct Resolved {
    /// The resolved model.
    pub workspace: Workspace,
    /// Files read, in merge order (extends base, includes, main, local).
    pub sources: Vec<PathBuf>,
    /// Non-fatal problems (e.g. `UNRESOLVED_VARIABLE`); `stems_core::validate`
    /// reports every one of them as an error.
    pub diagnostics: Vec<Diagnostic>,
    /// Source locations of every config path.
    pub spans: SpanIndex,
    /// References left for the daemon (`port: auto`, outputs).
    pub deferred: Vec<DeferredRef>,
}

/// Load, merge, default and substitute a workspace.
pub fn load(opts: LoadOptions) -> Result<Resolved, ConfigErrors> {
    let config_file = discover(opts.workspace.as_deref(), &opts.cwd, &opts.env)?;
    let root = config_file
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"));
    let home = opts.env.get("HOME").map(String::as_str);

    let mut loader = Loader::default();
    let main = loader.load(&config_file, None);
    let local_file = root.join(LOCAL_FILE);
    let local = (!opts.skip_local && local_file.is_file())
        .then(|| loader.load(&local_file, None))
        .flatten();
    if !loader.errors.is_empty() {
        return Err(ConfigErrors {
            errors: loader.errors,
        });
    }
    let Loader {
        sources, mut spans, ..
    } = loader;
    let mut merged = main.unwrap_or_else(|| Value::Mapping(Mapping::new()));
    let local_keys = local.as_ref().map(LocalEnvKeys::of).unwrap_or_default();
    // Variants (FR-ST-8): base -> active variant -> stems.local.yaml.
    let mut variant_diags = Vec::new();
    let stem_variants = variants::apply(&mut merged, local.as_ref(), &mut variant_diags);
    for (name, v) in &stem_variants {
        if let Some(active) = &v.active {
            let stem = ConfigPath::root().key("stems").key(name);
            spans.alias(&stem.key("variants").key(active), &stem, &local_file);
        }
    }
    if let Some(l) = local {
        merge::merge(&mut merged, l, &ConfigPath::root());
    }
    variants::strip(&mut merged);

    let schema_err = |e: serde_yaml_ng::Error| ConfigErrors::one(yaml_error(&e, None, &spans));
    let prelim: RawWorkspace = serde_yaml_ng::from_value(merged.clone()).map_err(schema_err)?;

    // Facts for substitution: ports and codebase directories of every stem.
    let ctx0 = Ctx::new(&prelim, &root, home);
    let raw_vars: IndexMap<String, String> = prelim
        .vars
        .iter()
        .map(|(k, v)| (k.clone(), v.to_string()))
        .collect();
    let mut facts: IndexMap<String, StemFacts> = IndexMap::new();
    let mut scratch = Vec::new();
    for (name, s) in &prelim.stems {
        let p = ConfigPath::root().key("stems").key(name).key("ports");
        facts.insert(
            name.clone(),
            StemFacts {
                ports: resolve_ports(s.ports.as_deref(), s.kind, &p, &mut scratch),
                codebase: None,
                outputs: Default::default(),
            },
        );
    }
    let codebases: Vec<(String, PathBuf)> = {
        let mut sub = Substituter::new(&opts.env, &root, &ctx0.ws_name, &facts, raw_vars.clone());
        prelim
            .stems
            .iter()
            .filter_map(|(name, s)| {
                let raw = match s.codebase.as_ref()? {
                    RawCodebase::Path(p) => {
                        RawCodebase::Path(sub.expand(p, &Scope::default(), &ConfigPath::root()))
                    }
                    RawCodebase::Git(g) => {
                        let mut g = g.clone();
                        g.git = sub.expand(&g.git, &Scope::default(), &ConfigPath::root());
                        g.path = g
                            .path
                            .map(|p| sub.expand(&p, &Scope::default(), &ConfigPath::root()));
                        RawCodebase::Git(g)
                    }
                };
                let cb = codebase::resolve_codebase(&raw, name, &root, &ctx0.repos_dir, home);
                Some((name.clone(), cb.path().to_path_buf()))
            })
            .collect()
    };
    for (name, path) in codebases {
        if let Some(f) = facts.get_mut(&name) {
            f.codebase = Some(path);
        }
    }

    // Substitute every string in the merged tree.
    let mut sub = Substituter::new(&opts.env, &root, &ctx0.ws_name, &facts, raw_vars);
    let vars = sub.resolve_vars();
    substitute_tree(&mut merged, &mut sub, &vars);
    let Substituter {
        diagnostics: sub_diags,
        deferred: sub_deferred,
        ..
    } = sub;

    let raw: RawWorkspace = serde_yaml_ng::from_value(merged).map_err(schema_err)?;
    let mut ctx = Ctx::new(&raw, &root, home);
    let mut workspace =
        resolve_workspace(raw, &mut ctx, &config_file, vars).map_err(|errors| ConfigErrors {
            errors: errors.into_iter().map(|d| locate(d, &spans)).collect(),
        })?;
    local_keys.apply(&mut workspace);
    for (name, v) in stem_variants {
        if let Some(stem) = workspace.stems.get_mut(&name) {
            stem.variant = Some(
                v.active
                    .unwrap_or_else(|| variants::BASE_VARIANT.to_string()),
            );
            stem.variants = v.names;
        }
    }

    let diagnostics = variant_diags
        .into_iter()
        .chain(sub_diags)
        .chain(ctx.diagnostics)
        .map(|d| locate(d, &spans))
        .collect();
    let mut deferred = sub_deferred;
    deferred.extend(ctx.deferred);

    Ok(Resolved {
        workspace,
        sources,
        diagnostics,
        spans,
        deferred,
    })
}

/// The committed config tree: `stems.yaml` with its `extends` and
/// `include`s merged, without `stems.local.yaml`, before defaults and
/// substitution (`stems config set` copies lists from it). Also returns the
/// path of `stems.yaml`.
pub fn committed_tree(opts: &LoadOptions) -> Result<(PathBuf, Value), ConfigErrors> {
    let config_file = discover(opts.workspace.as_deref(), &opts.cwd, &opts.env)?;
    let mut loader = Loader::default();
    let main = loader.load(&config_file, None);
    if !loader.errors.is_empty() {
        return Err(ConfigErrors {
            errors: loader.errors,
        });
    }
    Ok((
        config_file,
        main.unwrap_or_else(|| Value::Mapping(Mapping::new())),
    ))
}

/// Env keys set by `stems.local.yaml`: workspace-level and per stem.
#[derive(Default)]
struct LocalEnvKeys {
    workspace: Vec<String>,
    stems: HashMap<String, Vec<String>>,
}

impl LocalEnvKeys {
    fn of(local: &Value) -> Self {
        let keys = |v: Option<&Value>| -> Vec<String> {
            match v {
                Some(Value::Mapping(m)) => m.keys().map(merge::key_string).collect(),
                _ => Vec::new(),
            }
        };
        let workspace = keys(local.get("env"));
        let stems = match local.get("stems") {
            Some(Value::Mapping(m)) => m
                .iter()
                .map(|(k, v)| (merge::key_string(k), keys(v.get("env"))))
                .collect(),
            _ => HashMap::new(),
        };
        Self { workspace, stems }
    }

    /// Fill `Stem::local_env` with the effective values of the local keys.
    fn apply(&self, ws: &mut Workspace) {
        for (name, stem) in &mut ws.stems {
            let own = self.stems.get(name).map(Vec::as_slice).unwrap_or_default();
            for key in self.workspace.iter().chain(own) {
                if let Some(v) = stem.env.get(key) {
                    stem.local_env.insert(key.clone(), v.clone());
                }
            }
        }
    }
}

fn locate(mut d: Diagnostic, spans: &SpanIndex) -> Diagnostic {
    if d.location.is_none()
        && let Some(p) = &d.path
    {
        d.location = spans.locate(p).cloned();
    }
    d
}

/// Walk the merged tree substituting strings; `vars` is replaced by the
/// resolved variables.
fn substitute_tree(root: &mut Value, sub: &mut Substituter<'_>, vars: &IndexMap<String, String>) {
    let Value::Mapping(m) = root else { return };
    for (k, v) in m.iter_mut() {
        let key = merge::key_string(k);
        let path = ConfigPath::root().key(&key);
        match key.as_str() {
            "vars" => {
                *v = Value::Mapping(
                    vars.iter()
                        .map(|(k, v)| (Value::String(k.clone()), Value::String(v.clone())))
                        .collect(),
                );
            }
            "stems" => {
                if let Value::Mapping(stems) = v {
                    for (sk, sv) in stems.iter_mut() {
                        let name = merge::key_string(sk);
                        let scope = Scope {
                            stem: Some(name.clone()),
                            allow_codebase: true,
                        };
                        walk(sv, &path.key(&name), sub, &scope);
                    }
                }
            }
            _ => walk(v, &path, sub, &Scope::default()),
        }
    }
}

fn walk(v: &mut Value, path: &ConfigPath, sub: &mut Substituter<'_>, scope: &Scope) {
    match v {
        Value::String(s) => {
            if s.contains('$') {
                *s = sub.expand(s, scope, path);
            }
        }
        Value::Sequence(items) => {
            for (i, item) in items.iter_mut().enumerate() {
                walk(item, &path.index(i), sub, scope);
            }
        }
        Value::Mapping(m) => {
            for (k, item) in m.iter_mut() {
                let key = merge::key_string(k);
                let child = path.key(&key);
                // `${codebase}` is meaningless inside `codebase:` itself.
                if path.segments().len() == 2 && key == "codebase" {
                    let inner = Scope {
                        stem: scope.stem.clone(),
                        allow_codebase: false,
                    };
                    walk(item, &child, sub, &inner);
                } else {
                    walk(item, &child, sub, scope);
                }
            }
        }
        Value::Tagged(t) => walk(&mut t.value, path, sub, scope),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// The JSON Schema for `stems.yaml`, as pretty-printed JSON (trailing newline).
pub fn json_schema_string() -> String {
    let schema = schemars::schema_for!(RawWorkspace);
    let mut s = serde_json::to_string_pretty(&schema).unwrap_or_default();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_wired() {
        assert_eq!(super::CRATE_NAME, "stems-config");
    }
}
