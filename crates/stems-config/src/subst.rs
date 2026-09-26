//! `${…}` substitution (FR-WS-10).
//!
//! Recognised references (anything else inside `${…}`, e.g. a shell `${HOME}`,
//! is left untouched; `$${` is an escape for a literal `${`):
//!
//! | reference | value |
//! |---|---|
//! | `${var.x}` | workspace variable (may itself reference others; cycles are errors) |
//! | `${env.X}` | environment at load time |
//! | `${workspace.root}` / `${workspace.name}` | integration repo root / workspace name |
//! | `${codebase}` | the enclosing stem's codebase directory |
//! | `${stem.<n>.port}` | first declared port of `<n>` |
//! | `${stem.<n>.ports.<p>}` | named port |
//! | `${stem.<n>.host}` | `localhost` |
//! | `${stem.self.…}` | the enclosing stem |
//! | `${stem.<n>.outputs.<x>}` | runtime output: kept literally, recorded as deferred |
//!
//! A reference to a `port: auto` port is kept literally (with `self` rewritten
//! to the stem name) and recorded as a [`DeferredRef`] for the daemon.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::diagnostic::{Diagnostic, codes};
use crate::model::{Port, PortRef};
use crate::path::ConfigPath;

/// Why a reference was left in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeferredKind {
    /// A `port: auto` port, allocated when the stem starts.
    AutoPort,
    /// A stem output, evaluated after the stem is healthy.
    Output,
}

/// A reference kept literally in the resolved config for the daemon to fill.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeferredRef {
    /// Where the reference occurs.
    pub path: ConfigPath,
    /// The reference, without `${` `}`, e.g. `stem.shop-web.port`.
    pub reference: String,
    /// Kind.
    pub kind: DeferredKind,
}

/// What the substitution engine knows about each stem.
#[derive(Clone, Debug, Default)]
pub struct StemFacts {
    /// Declared ports (first one is `${stem.<n>.port}`).
    pub ports: Vec<Port>,
    /// Codebase directory, if any.
    pub codebase: Option<PathBuf>,
}

/// Lexical scope of a string being substituted.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    /// The enclosing stem (for `${codebase}` and `stem.self`).
    pub stem: Option<String>,
    /// Whether `${codebase}` may be used (not inside `codebase:` itself).
    pub allow_codebase: bool,
}

type Deferred = Vec<(String, DeferredKind)>;

enum Piece {
    Value(String),
    /// A variable's value together with deferred refs it contains.
    VarValue(String, Deferred),
    Deferred(String, DeferredKind),
    /// Unresolved; `None` = already reported elsewhere (failed variable).
    Error(Option<(String, String)>),
}

/// The substitution engine. Collects diagnostics and deferred references.
pub struct Substituter<'a> {
    env: &'a HashMap<String, String>,
    root: &'a Path,
    ws_name: &'a str,
    stems: &'a IndexMap<String, StemFacts>,
    raw_vars: IndexMap<String, String>,
    memo: HashMap<String, Option<(String, Deferred)>>,
    failures: usize,
    /// Diagnostics (all `UNRESOLVED_VARIABLE`).
    pub diagnostics: Vec<Diagnostic>,
    /// Deferred references.
    pub deferred: Vec<DeferredRef>,
}

impl<'a> Substituter<'a> {
    /// New engine over raw (unsubstituted) vars.
    pub fn new(
        env: &'a HashMap<String, String>,
        root: &'a Path,
        ws_name: &'a str,
        stems: &'a IndexMap<String, StemFacts>,
        raw_vars: IndexMap<String, String>,
    ) -> Self {
        Self {
            env,
            root,
            ws_name,
            stems,
            raw_vars,
            memo: HashMap::new(),
            failures: 0,
            diagnostics: Vec::new(),
            deferred: Vec::new(),
        }
    }

    /// Resolve every variable. Failed ones keep their raw text.
    pub fn resolve_vars(&mut self) -> IndexMap<String, String> {
        let names: Vec<String> = self.raw_vars.keys().cloned().collect();
        let mut out = IndexMap::new();
        for n in names {
            let v = match self.var(&n, &mut Vec::new()) {
                Some((v, _)) => v,
                None => self.raw_vars[&n].clone(),
            };
            out.insert(n, v);
        }
        out
    }

    fn var(&mut self, name: &str, stack: &mut Vec<String>) -> Option<(String, Deferred)> {
        if let Some(m) = self.memo.get(name) {
            return m.clone();
        }
        if let Some(pos) = stack.iter().position(|s| s == name) {
            let mut cycle: Vec<&str> = stack[pos..].iter().map(String::as_str).collect();
            cycle.push(name);
            let path = ConfigPath::root().key("vars").key(&stack[pos]);
            self.diagnostics.push(
                Diagnostic::new(
                    codes::UNRESOLVED_VARIABLE,
                    format!("variable cycle: {}", cycle.join(" -> ")),
                )
                .with_path(path)
                .with_hint("break the cycle so each variable resolves to a value"),
            );
            for n in &stack[pos..] {
                self.memo.insert(n.clone(), None);
            }
            return None;
        }
        let raw = self.raw_vars.get(name)?.clone();
        stack.push(name.to_string());
        let path = ConfigPath::root().key("vars").key(name);
        let before = self.failures;
        let mut deferred = Vec::new();
        let value = self.expand_inner(&raw, &Scope::default(), &path, stack, &mut deferred);
        stack.pop();
        if self.memo.contains_key(name) {
            // Marked failed by a cycle detected deeper down.
            return None;
        }
        let ok = self.failures == before;
        let result = ok.then(|| (value, deferred.clone()));
        for (reference, kind) in deferred {
            self.deferred.push(DeferredRef {
                path: path.clone(),
                reference,
                kind,
            });
        }
        self.memo.insert(name.to_string(), result.clone());
        result
    }

    /// Substitute all references in `s`, reporting problems at `path`.
    pub fn expand(&mut self, s: &str, scope: &Scope, path: &ConfigPath) -> String {
        let mut deferred = Vec::new();
        let out = self.expand_inner(s, scope, path, &mut Vec::new(), &mut deferred);
        for (reference, kind) in deferred {
            self.deferred.push(DeferredRef {
                path: path.clone(),
                reference,
                kind,
            });
        }
        out
    }

    fn expand_inner(
        &mut self,
        s: &str,
        scope: &Scope,
        path: &ConfigPath,
        stack: &mut Vec<String>,
        deferred: &mut Deferred,
    ) -> String {
        let mut out = String::with_capacity(s.len());
        let mut rest = s;
        while let Some(i) = rest.find('$') {
            out.push_str(&rest[..i]);
            let tail = &rest[i..];
            if let Some(after) = tail.strip_prefix("$${") {
                out.push_str("${");
                rest = after;
                continue;
            }
            if !tail.starts_with("${") {
                out.push('$');
                rest = &tail[1..];
                continue;
            }
            let Some(end) = tail.find('}') else {
                out.push_str(tail);
                rest = "";
                break;
            };
            let literal = &tail[..=end];
            let expr = tail[2..end].trim();
            rest = &tail[end + 1..];
            if !is_ours(expr) {
                out.push_str(literal);
                continue;
            }
            match self.lookup(expr, scope, stack) {
                Piece::Value(v) => out.push_str(&v),
                Piece::VarValue(v, d) => {
                    out.push_str(&v);
                    for item in d {
                        if !deferred.contains(&item) {
                            deferred.push(item);
                        }
                    }
                }
                Piece::Deferred(reference, kind) => {
                    out.push_str("${");
                    out.push_str(&reference);
                    out.push('}');
                    if !deferred.iter().any(|(r, _)| *r == reference) {
                        deferred.push((reference, kind));
                    }
                }
                Piece::Error(err) => {
                    self.failures += 1;
                    out.push_str(literal);
                    if let Some((msg, hint)) = err {
                        self.diagnostics.push(
                            Diagnostic::new(
                                codes::UNRESOLVED_VARIABLE,
                                format!("unresolved reference `${{{expr}}}`: {msg}"),
                            )
                            .with_path(path.clone())
                            .with_hint(hint),
                        );
                    }
                }
            }
        }
        out.push_str(rest);
        out
    }

    fn lookup(&mut self, expr: &str, scope: &Scope, stack: &mut Vec<String>) -> Piece {
        let err = |m: String, h: &str| Piece::Error(Some((m, h.to_string())));
        if expr == "codebase" {
            if !scope.allow_codebase {
                return err(
                    "`${codebase}` can only be used inside a stem (and not in `codebase` itself)"
                        .into(),
                    "use an explicit path here",
                );
            }
            let Some(stem) = &scope.stem else {
                return err(
                    "`${codebase}` can only be used inside a stem".into(),
                    "use `${workspace.root}` or `${stem.<name>…}` outside stems",
                );
            };
            return match self.stems.get(stem).and_then(|f| f.codebase.as_ref()) {
                Some(p) => Piece::Value(p.display().to_string()),
                None => err(
                    format!("stem `{stem}` has no codebase"),
                    "declare `codebase:` on the stem",
                ),
            };
        }
        if let Some(name) = expr.strip_prefix("var.") {
            if !self.raw_vars.contains_key(name) {
                return err(
                    format!("variable `{name}` is not declared"),
                    "declare it under `vars:`",
                );
            }
            return match self.var(name, stack) {
                Some((v, d)) => Piece::VarValue(v, d),
                None => Piece::Error(None),
            };
        }
        if let Some(name) = expr.strip_prefix("env.") {
            return match self.env.get(name) {
                Some(v) => Piece::Value(v.clone()),
                None => err(
                    format!("environment variable `{name}` is not set"),
                    "export it before running stems, or use a `vars:` entry",
                ),
            };
        }
        if let Some(what) = expr.strip_prefix("workspace.") {
            return match what {
                "root" => Piece::Value(self.root.display().to_string()),
                "name" => Piece::Value(self.ws_name.to_string()),
                _ => err(
                    format!("unknown workspace property `{what}`"),
                    "available: `workspace.root`, `workspace.name`",
                ),
            };
        }
        if let Some(rest) = expr.strip_prefix("stem.") {
            return self.lookup_stem(rest, scope);
        }
        err(
            format!("unknown reference `{expr}`"),
            "see the substitution table in the docs",
        )
    }

    fn lookup_stem(&self, rest: &str, scope: &Scope) -> Piece {
        let err = |m: String, h: &str| Piece::Error(Some((m, h.to_string())));
        let (name, prop) = rest.split_once('.').unwrap_or((rest, ""));
        let name = if name == "self" {
            match &scope.stem {
                Some(s) => s.as_str(),
                None => {
                    return err(
                        "`stem.self` can only be used inside a stem".into(),
                        "name the stem explicitly",
                    );
                }
            }
        } else {
            name
        };
        let Some(facts) = self.stems.get(name) else {
            return err(
                format!("stem `{name}` does not exist"),
                "check the stem name under `stems:`",
            );
        };
        let port_piece = |port: &Port, reference: String| match port.port {
            PortRef::Fixed(n) => Piece::Value(n.to_string()),
            PortRef::Auto => Piece::Deferred(reference, DeferredKind::AutoPort),
        };
        match prop.split_once('.').unwrap_or((prop, "")) {
            ("port", "") => match facts.ports.first() {
                Some(p) => port_piece(p, format!("stem.{name}.port")),
                None => err(
                    format!("stem `{name}` declares no ports"),
                    "add a `ports:` entry to that stem",
                ),
            },
            ("ports", pname) if !pname.is_empty() => {
                match facts.ports.iter().find(|p| p.name == pname) {
                    Some(p) => port_piece(p, format!("stem.{name}.ports.{pname}")),
                    None => err(
                        format!("stem `{name}` has no port named `{pname}`"),
                        "name the port with `{ name: …, port: … }`",
                    ),
                }
            }
            ("host", "") => Piece::Value("localhost".into()),
            ("outputs", out) if !out.is_empty() => {
                Piece::Deferred(format!("stem.{name}.outputs.{out}"), DeferredKind::Output)
            }
            _ => err(
                format!("unknown stem property `{prop}`"),
                "available: `port`, `ports.<name>`, `host`, `outputs.<name>`",
            ),
        }
    }
}

fn is_ours(expr: &str) -> bool {
    expr == "codebase"
        || ["var.", "env.", "workspace.", "stem."]
            .iter()
            .any(|p| expr.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine<'a>(
        env: &'a HashMap<String, String>,
        stems: &'a IndexMap<String, StemFacts>,
        vars: &[(&str, &str)],
    ) -> Substituter<'a> {
        Substituter::new(
            env,
            Path::new("/ws"),
            "demo",
            stems,
            vars.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn facts() -> IndexMap<String, StemFacts> {
        let mut m = IndexMap::new();
        m.insert(
            "api".to_string(),
            StemFacts {
                ports: vec![
                    Port {
                        name: "http".into(),
                        port: PortRef::Fixed(8080),
                        container_port: None,
                    },
                    Port {
                        name: "debug".into(),
                        port: PortRef::Auto,
                        container_port: None,
                    },
                ],
                codebase: Some(PathBuf::from("/code/api")),
            },
        );
        m
    }

    #[test]
    fn var_chains_resolve() {
        let env = HashMap::new();
        let stems = IndexMap::new();
        let mut s = engine(
            &env,
            &stems,
            &[("a", "${var.b}!"), ("b", "${var.c}${var.c}"), ("c", "x")],
        );
        let vars = s.resolve_vars();
        assert_eq!(vars["a"], "xx!");
        assert!(s.diagnostics.is_empty());
        assert_eq!(
            s.expand("<${var.a}>", &Scope::default(), &ConfigPath::root()),
            "<xx!>"
        );
    }

    #[test]
    fn cycles_are_detected() {
        let env = HashMap::new();
        let stems = IndexMap::new();
        let mut s = engine(
            &env,
            &stems,
            &[
                ("a", "${var.b}"),
                ("b", "${var.c}"),
                ("c", "${var.a}"),
                ("me", "${var.me}"),
            ],
        );
        let vars = s.resolve_vars();
        assert_eq!(vars["a"], "${var.b}", "failed vars keep their raw text");
        let msgs: Vec<_> = s.diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            msgs,
            [
                "variable cycle: a -> b -> c -> a",
                "variable cycle: me -> me"
            ]
        );
    }

    #[test]
    fn stem_references() {
        let env = HashMap::new();
        let stems = facts();
        let mut s = engine(&env, &stems, &[]);
        let scope = Scope {
            stem: Some("api".into()),
            allow_codebase: true,
        };
        let p = ConfigPath::root().key("x");
        assert_eq!(s.expand("${stem.api.port}", &scope, &p), "8080");
        assert_eq!(s.expand("${stem.self.ports.http}", &scope, &p), "8080");
        assert_eq!(
            s.expand("${stem.self.ports.debug}", &scope, &p),
            "${stem.api.ports.debug}"
        );
        assert_eq!(s.expand("${stem.api.host}", &scope, &p), "localhost");
        assert_eq!(s.expand("${codebase}/bin", &scope, &p), "/code/api/bin");
        assert_eq!(s.expand("${ codebase }", &scope, &p), "/code/api");
        assert!(s.diagnostics.is_empty(), "{:?}", s.diagnostics);
        assert_eq!(s.deferred.len(), 1);
        assert_eq!(s.deferred[0].kind, DeferredKind::AutoPort);
    }

    #[test]
    fn literals_escapes_and_malformed_input() {
        let env = HashMap::new();
        let stems = IndexMap::new();
        let mut s = engine(&env, &stems, &[]);
        let p = ConfigPath::root();
        let sc = Scope::default();
        assert_eq!(s.expand("cost: $5", &sc, &p), "cost: $5");
        assert_eq!(s.expand("$${var.x}", &sc, &p), "${var.x}");
        assert_eq!(s.expand("${unclosed", &sc, &p), "${unclosed");
        assert_eq!(s.expand("${HOME}/x ${1}", &sc, &p), "${HOME}/x ${1}");
        assert!(s.diagnostics.is_empty());
        assert_eq!(s.expand("${codebase}", &sc, &p), "${codebase}");
        assert_eq!(s.expand("${stem.self.port}", &sc, &p), "${stem.self.port}");
        assert_eq!(s.diagnostics.len(), 2);
    }
}
