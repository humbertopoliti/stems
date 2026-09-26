//! Which stems a lifecycle command acts on (FR-WS-8, deliverable 26).
//!
//! Rules (see `docs/config.md`, "Profiles"):
//!
//! 1. **Names win.** When stem names are given (`up api web`), they are the
//!    requested set as-is and no profile applies (a `--profile` passed with
//!    names is ignored for selection; the CLI refuses the combination).
//! 2. Otherwise the **profile** is, first match wins: the `profile` argument
//!    (`--profile`, else `STEMS_PROFILE`, both read by the caller), the local
//!    override `profile:` (usually in `stems.local.yaml`), `default_profile`,
//!    and a profile literally named `default`. No profile at all selects
//!    every enabled stem.
//! 3. A profile is a list of stems or an **alias** of another profile
//!    (`default: backend`), followed up to [`MAX_ALIAS_DEPTH`] hops.
//!    Disabled members (`enabled: false`) are dropped silently.
//! 4. The **closure** adds hard dependencies transitively (soft edges never
//!    pull anything in; disabled targets are skipped: validation reports
//!    them as `DEPENDENCY_DISABLED`). The stems pulled in that were not
//!    requested are `expanded`. With `strict_profiles: true`, a profile that
//!    would need expansion is `PROFILE_MISSING_DEPENDENCY` instead.

use std::collections::{BTreeSet, HashSet, VecDeque};

use serde::Serialize;
use serde_json::json;
use stems_config::{ConfigPath, Profile, Workspace};

use crate::error::{Error, ErrorCode};

/// Environment variable naming the profile when `--profile` is not given.
pub const ENV_PROFILE: &str = "STEMS_PROFILE";

/// Maximum number of alias hops (`a: b`, `b: c`, ...) followed.
pub const MAX_ALIAS_DEPTH: usize = 8;

/// Name of the profile used when nothing else names one.
pub const DEFAULT_PROFILE_NAME: &str = "default";

/// Knobs for [`Selection::resolve`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectOptions {
    /// Add hard dependencies (false for `--no-deps`).
    pub include_deps: bool,
    /// Override `strict_profiles` (None = the workspace's setting).
    pub strict_profiles: Option<bool>,
}

impl Default for SelectOptions {
    fn default() -> Self {
        Self {
            include_deps: true,
            strict_profiles: None,
        }
    }
}

/// The stems a command acts on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Selection {
    /// The profile that selected the stems (`None` when names were given or
    /// no profile applies).
    pub profile: Option<String>,
    /// Names given, or the profile's enabled members, or every enabled stem.
    pub requested: Vec<String>,
    /// `requested` plus hard dependencies, in declaration order.
    pub closure: Vec<String>,
    /// Stems in `closure` that were not requested (pulled in as dependencies).
    pub expanded: Vec<String>,
    /// Profile members left out because they are disabled.
    pub disabled: Vec<String>,
}

/// Why a profile was picked when none was named on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultSource {
    /// `profile:` (local override).
    Profile,
    /// `default_profile:`.
    DefaultProfile,
    /// A profile named `default`.
    Named,
}

/// The profile `up` uses when neither `--profile` nor `STEMS_PROFILE` is set.
pub fn default_profile(ws: &Workspace) -> Option<(String, DefaultSource)> {
    if let Some(p) = &ws.profile {
        return Some((p.clone(), DefaultSource::Profile));
    }
    if let Some(p) = &ws.default_profile {
        return Some((p.clone(), DefaultSource::DefaultProfile));
    }
    ws.profiles
        .contains_key(DEFAULT_PROFILE_NAME)
        .then(|| (DEFAULT_PROFILE_NAME.to_string(), DefaultSource::Named))
}

fn unknown_profile(ws: &Workspace, name: &str) -> Error {
    let known: Vec<&String> = ws.profiles.keys().collect();
    let mut hint = String::new();
    if let Some(c) = crate::validate::closest_name(name, ws.profiles.keys()) {
        hint.push_str(&format!("did you mean `{c}`? "));
    }
    if known.is_empty() {
        hint.push_str("this workspace defines no profiles (add a `profiles:` map to stems.yaml)");
    } else {
        hint.push_str(&format!(
            "profiles: {} (see `stems profiles`)",
            known
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Error::new(
        ErrorCode::UnknownProfile,
        format!("profile `{name}` is not defined"),
    )
    .with_hint(hint)
    .with_details(json!({ "profile": name, "known": known }))
}

/// Follow aliases from `name` to a stem list. Returns the chain of names
/// visited (starting with `name`) and the list.
pub fn resolve_profile<'w>(
    ws: &'w Workspace,
    name: &str,
) -> Result<(Vec<String>, &'w [String]), Error> {
    let mut chain = vec![name.to_string()];
    let mut cur = name;
    loop {
        match ws.profiles.get(cur) {
            None => {
                let mut e = unknown_profile(ws, cur);
                if chain.len() > 1 {
                    e.message = format!(
                        "profile `{cur}` is not defined (alias chain {})",
                        chain.join(" -> ")
                    );
                    e = e.with_path(
                        ConfigPath::root()
                            .key("profiles")
                            .key(&chain[chain.len() - 2]),
                    );
                    e.details["chain"] = json!(chain);
                }
                return Err(e);
            }
            Some(Profile::Stems(list)) => return Ok((chain, list)),
            Some(Profile::Alias(target)) => {
                if chain.len() > MAX_ALIAS_DEPTH || chain.iter().any(|c| c == target) {
                    let mut shown = chain.clone();
                    shown.push(target.clone());
                    return Err(Error::new(
                        ErrorCode::UnknownProfile,
                        format!(
                            "profile aliases do not resolve to a stem list: {}",
                            shown.join(" -> ")
                        ),
                    )
                    .with_path(ConfigPath::root().key("profiles").key(name))
                    .with_hint(format!(
                        "make one of these profiles a list of stems (at most {MAX_ALIAS_DEPTH} alias hops are followed)"
                    ))
                    .with_details(json!({ "profile": name, "chain": shown })));
                }
                chain.push(target.clone());
                cur = target;
            }
        }
    }
}

fn check_stem(ws: &Workspace, r: &str) -> Result<(), Error> {
    match ws.stem(r) {
        None => Err(Error::new(
            ErrorCode::UnknownStem,
            format!("stem `{r}` does not exist in workspace `{}`", ws.name),
        )
        .with_hint(format!(
            "known stems: {}",
            ws.stems.keys().cloned().collect::<Vec<_>>().join(", ")
        ))
        .with_details(json!({ "stem": r }))),
        Some(s) if !s.enabled => Err(Error::new(
            ErrorCode::UnknownStem,
            format!("stem `{r}` is disabled (`enabled: false`)"),
        )
        .with_hint(format!(
            "enable it in stems.local.yaml: stems.{r}.enabled: true"
        ))
        .with_details(json!({ "stem": r, "disabled": true }))),
        Some(_) => Ok(()),
    }
}

/// `requested` plus enabled hard dependencies, transitively.
fn hard_closure(ws: &Workspace, requested: &[String]) -> HashSet<String> {
    let mut set: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = requested.iter().cloned().collect();
    while let Some(n) = queue.pop_front() {
        if !set.insert(n.clone()) {
            continue;
        }
        if let Some(s) = ws.stem(&n) {
            for d in s.depends_on.iter().filter(|d| !d.soft) {
                if ws.stem(&d.stem).is_some_and(|t| t.enabled) {
                    queue.push_back(d.stem.clone());
                }
            }
        }
    }
    set
}

fn in_declaration_order(ws: &Workspace, set: &HashSet<String>) -> Vec<String> {
    ws.stems
        .keys()
        .filter(|n| set.contains(*n))
        .cloned()
        .collect()
}

impl Selection {
    /// Resolve the selection. `profile` is `--profile` or `STEMS_PROFILE`
    /// (the caller reads both); it is ignored when `names` is non-empty.
    pub fn resolve(
        ws: &Workspace,
        profile: Option<&str>,
        names: &[String],
        opts: SelectOptions,
    ) -> Result<Selection, Error> {
        let mut sel = Selection::default();
        if !names.is_empty() {
            let mut seen = BTreeSet::new();
            for n in names {
                check_stem(ws, n)?;
                if seen.insert(n.clone()) {
                    sel.requested.push(n.clone());
                }
            }
        } else {
            let chosen = profile
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .or_else(|| default_profile(ws).map(|(p, _)| p));
            match chosen {
                Some(p) => {
                    let (_, list) = resolve_profile(ws, &p)?;
                    let mut seen = BTreeSet::new();
                    for (i, s) in list.iter().enumerate() {
                        match ws.stem(s) {
                            None => {
                                return Err(Error::new(
                                    ErrorCode::UnknownStem,
                                    format!("profile `{p}` lists `{s}`, which is not a stem"),
                                )
                                .with_path(ConfigPath::root().key("profiles").key(&p).index(i))
                                .with_hint(format!("remove `{s}` from profile `{p}`"))
                                .with_details(json!({ "profile": p, "stem": s })));
                            }
                            Some(st) if !st.enabled => sel.disabled.push(s.clone()),
                            Some(_) => {
                                if seen.insert(s.clone()) {
                                    sel.requested.push(s.clone());
                                }
                            }
                        }
                    }
                    sel.profile = Some(p);
                }
                None => sel.requested = ws.stems().map(|s| s.name.clone()).collect(),
            }
        }
        let full = hard_closure(ws, &sel.requested);
        let requested: HashSet<&String> = sel.requested.iter().collect();
        let missing: Vec<String> = in_declaration_order(ws, &full)
            .into_iter()
            .filter(|n| !requested.contains(n))
            .collect();
        if let Some(p) = &sel.profile
            && !missing.is_empty()
            && opts.strict_profiles.unwrap_or(ws.strict_profiles)
        {
            let needed_by: Vec<serde_json::Value> = missing
                .iter()
                .map(|m| {
                    let by: Vec<&String> = full
                        .iter()
                        .filter(|n| {
                            ws.stem(n).is_some_and(|s| {
                                s.depends_on.iter().any(|d| !d.soft && &d.stem == m)
                            })
                        })
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect();
                    json!({ "stem": m, "needed_by": by })
                })
                .collect();
            return Err(Error::new(
                ErrorCode::ProfileMissingDependency,
                format!(
                    "profile `{p}` omits {}, needed as hard {} of its stems, and `strict_profiles` is on",
                    missing
                        .iter()
                        .map(|m| format!("`{m}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    if missing.len() == 1 { "dependency" } else { "dependencies" }
                ),
            )
            .with_path(ConfigPath::root().key("profiles").key(p))
            .with_hint(format!(
                "add {} to profile `{p}`, or set `strict_profiles: false` to pull dependencies in automatically",
                missing.join(", ")
            ))
            .with_details(json!({
                "profile": p,
                "missing": missing,
                "dependencies": needed_by,
            })));
        }
        if opts.include_deps {
            sel.closure = in_declaration_order(ws, &full);
            sel.expanded = missing;
        } else {
            let set: HashSet<String> = sel.requested.iter().cloned().collect();
            sel.closure = in_declaration_order(ws, &set);
        }
        Ok(sel)
    }
}

/// One row of `stems profiles`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ProfileInfo {
    /// Profile name.
    pub name: String,
    /// For an alias, the profile it points at (first hop).
    pub alias_of: Option<String>,
    /// Enabled members (after alias resolution).
    pub stems: Vec<String>,
    /// Members plus hard dependencies, in declaration order.
    pub closure: Vec<String>,
    /// Dependencies pulled in that the profile does not list.
    pub expanded: Vec<String>,
    /// Members left out because they are disabled.
    pub disabled: Vec<String>,
    /// `up` without `--profile` / `STEMS_PROFILE` uses this profile.
    pub default: bool,
    /// Why this is the default (`profile`, `default_profile`, `named`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_source: Option<DefaultSource>,
    /// Resolving it fails (unknown alias target, strict profile missing a
    /// dependency, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

/// Every profile, in declaration order, with its resolved membership.
pub fn describe_profiles(ws: &Workspace) -> Vec<ProfileInfo> {
    let default = default_profile(ws);
    ws.profiles
        .iter()
        .map(|(name, p)| {
            let alias_of = match p {
                Profile::Alias(t) => Some(t.clone()),
                Profile::Stems(_) => None,
            };
            let is_default = default.as_ref().is_some_and(|(d, _)| d == name);
            let mut info = ProfileInfo {
                name: name.clone(),
                alias_of,
                stems: Vec::new(),
                closure: Vec::new(),
                expanded: Vec::new(),
                disabled: Vec::new(),
                default: is_default,
                default_source: default.as_ref().filter(|_| is_default).map(|(_, s)| *s),
                error: None,
            };
            match Selection::resolve(ws, Some(name), &[], SelectOptions::default()) {
                Ok(sel) => {
                    info.stems = sel.requested;
                    info.closure = sel.closure;
                    info.expanded = sel.expanded;
                    info.disabled = sel.disabled;
                }
                Err(e) => info.error = Some(e),
            }
            info
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::graph::tests::load_str;

    fn hello_shop() -> Workspace {
        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/workspaces/hello-shop");
        stems_config::load(stems_config::LoadOptions {
            workspace: Some(dir),
            cwd: Path::new("/").to_path_buf(),
            env: [("HOME".to_string(), "/home/u".to_string())].into(),
            skip_local: true,
        })
        .unwrap()
        .workspace
    }

    fn sel(ws: &Workspace, profile: Option<&str>, names: &[&str]) -> Result<Selection, Error> {
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        Selection::resolve(ws, profile, &names, SelectOptions::default())
    }

    #[test]
    fn hello_shop_profiles_golden() {
        let ws = hello_shop();
        let rows: Vec<String> = ["default", "backend", "process-only"]
            .iter()
            .map(|p| {
                let s = sel(&ws, Some(p), &[]).unwrap();
                format!(
                    "{p}: requested={:?} closure={:?} expanded={:?}",
                    s.requested, s.closure, s.expanded
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                r#"default: requested=["postgres", "redis", "shop-api", "shop-worker", "shop-web", "httpbin"] closure=["postgres", "redis", "shop-api", "shop-worker", "shop-web", "httpbin"] expanded=[]"#,
                r#"backend: requested=["postgres", "redis", "shop-api", "shop-worker"] closure=["postgres", "redis", "shop-api", "shop-worker"] expanded=[]"#,
                r#"process-only: requested=["shop-api", "shop-worker", "shop-web"] closure=["postgres", "redis", "shop-api", "shop-worker", "shop-web"] expanded=["postgres", "redis"]"#,
            ]
        );
        // No profile named: the profile called `default` applies.
        let s = sel(&ws, None, &[]).unwrap();
        assert_eq!(s.profile.as_deref(), Some("default"));
    }

    #[test]
    fn names_win_over_profiles() {
        let ws = hello_shop();
        let s = sel(&ws, Some("backend"), &["shop-web"]).unwrap();
        assert_eq!(s.profile, None);
        assert_eq!(s.requested, ["shop-web"]);
        assert!(s.closure.contains(&"shop-api".to_string()));
        let s = Selection::resolve(
            &ws,
            None,
            &["shop-web".to_string()],
            SelectOptions {
                include_deps: false,
                strict_profiles: None,
            },
        )
        .unwrap();
        assert_eq!(s.closure, ["shop-web"]);
        assert!(s.expanded.is_empty());
    }

    #[test]
    fn strict_profiles_reports_missing_dependencies() {
        let ws = hello_shop();
        let opts = SelectOptions {
            include_deps: true,
            strict_profiles: Some(true),
        };
        let e = Selection::resolve(&ws, Some("process-only"), &[], opts).unwrap_err();
        assert_eq!(e.code, ErrorCode::ProfileMissingDependency);
        assert_eq!(e.details["missing"], json!(["postgres", "redis"]));
        assert_eq!(e.exit_code(), 2);
        // Complete profiles pass in strict mode.
        assert!(Selection::resolve(&ws, Some("backend"), &[], opts).is_ok());
    }

    #[test]
    fn aliases_resolve_and_loops_fail() {
        let ws = load_str(
            "profiles:\n  base: [a]\n  mid: base\n  top: mid\n  loop1: loop2\n  loop2: loop1\n  bad: nope\ndefault_profile: top\nstems:\n  a: { type: process, depends_on: [b] }\n  b: { type: process }\n",
        );
        let s = sel(&ws, None, &[]).unwrap();
        assert_eq!(s.profile.as_deref(), Some("top"));
        assert_eq!(s.requested, ["a"]);
        assert_eq!(s.closure, ["a", "b"]);
        assert_eq!(s.expanded, ["b"]);
        let (chain, _) = resolve_profile(&ws, "top").unwrap();
        assert_eq!(chain, ["top", "mid", "base"]);
        let e = sel(&ws, Some("loop1"), &[]).unwrap_err();
        assert_eq!(e.code, ErrorCode::UnknownProfile);
        assert!(
            e.message.contains("loop1 -> loop2 -> loop1"),
            "{}",
            e.message
        );
        let e = sel(&ws, Some("bad"), &[]).unwrap_err();
        assert_eq!(e.code, ErrorCode::UnknownProfile);
        assert!(e.message.contains("`nope`"));
    }

    #[test]
    fn unknown_profile_suggests() {
        let ws = hello_shop();
        let e = sel(&ws, Some("backnd"), &[]).unwrap_err();
        assert_eq!(e.code, ErrorCode::UnknownProfile);
        assert_eq!(e.exit_code(), 2);
        assert!(e.hint.unwrap().contains("did you mean `backend`?"));
    }

    #[test]
    fn soft_edges_and_disabled_stems() {
        let ws = load_str(
            "profiles:\n  p: [a, off]\nstems:\n  a: { type: process, depends_on: [{ stem: s, soft: true }, b] }\n  b: { type: process }\n  s: { type: process }\n  off: { type: process, enabled: false }\n",
        );
        let s = sel(&ws, Some("p"), &[]).unwrap();
        assert_eq!(s.requested, ["a"]);
        assert_eq!(s.disabled, ["off"]);
        assert_eq!(s.closure, ["a", "b"]);
        // No profile at all and no `default`: every enabled stem.
        let s = sel(&ws, None, &[]).unwrap();
        assert_eq!(s.profile, None);
        assert_eq!(s.requested, ["a", "b", "s"]);
        let e = sel(&ws, None, &["off"]).unwrap_err();
        assert_eq!(e.code, ErrorCode::UnknownStem);
    }

    #[test]
    fn local_profile_override_wins() {
        let ws = load_str(
            "profiles:\n  x: [a]\n  y: [b]\ndefault_profile: x\nprofile: y\nstems:\n  a: { type: process }\n  b: { type: process }\n",
        );
        assert_eq!(sel(&ws, None, &[]).unwrap().requested, ["b"]);
        assert_eq!(sel(&ws, Some("x"), &[]).unwrap().requested, ["a"]);
        let rows = describe_profiles(&ws);
        assert!(!rows[0].default);
        assert!(rows[1].default);
        assert_eq!(rows[1].default_source, Some(DefaultSource::Profile));
    }
}
