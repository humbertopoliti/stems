//! Secret redaction (FR-AI-3, deliverable 47, `docs/config.md#secrets`).
//!
//! One [`Redactor`] decides which env names hold secrets, built from the
//! resolved workspace:
//!
//! 1. a stem's own `secrets:` list (`NAME` adds, `!NAME` excepts),
//! 2. the workspace `secrets:` list (same syntax),
//! 3. the name patterns: the built-in [`BUILTIN_PATTERNS`] (unless
//!    `redaction.builtin: false`) plus `redaction.patterns`.
//!
//! The first rule that says something about a name wins, so an exception
//! (`!PUBLIC_KEY`) beats a pattern and a stem's list beats the workspace's.
//! Every entry and pattern is a glob (`*`, `?`) matched case-insensitively
//! against the name and against the name with `-` read as `_`
//! (`github-token` is `GITHUB_TOKEN`).
//!
//! Used for display ([`Redactor::redact_json`], [`Redactor::redact_pairs`]:
//! values become [`REDACTED`]) and for log lines ([`Redactor::secret_values`]
//! then [`scrub`]: each secret *value* of at least [`MIN_SCRUB_LEN`]
//! characters becomes `<redacted:KEY>`).

use std::collections::HashMap;
use std::path::Path;

use globset::{Glob, GlobBuilder, GlobMatcher};
use serde_json::Value;
use stems_config::{Stem, Workspace};

/// What a redacted value is shown as.
pub const REDACTED: &str = "<redacted>";

/// Values shorter than this (in characters) are not scrubbed from log
/// lines: too likely to match ordinary text.
pub const MIN_SCRUB_LEN: usize = 6;

/// Built-in secret name patterns (globs, case-insensitive).
pub const BUILTIN_PATTERNS: [&str; 8] = [
    "*TOKEN*",
    "*SECRET*",
    "*PASSWORD*",
    "*PASSWD*",
    "*CREDENTIAL*",
    "*_KEY",
    "*_KEY_*",
    "*APIKEY*",
];

fn glob(pattern: &str) -> GlobMatcher {
    GlobBuilder::new(pattern)
        .case_insensitive(true)
        .literal_separator(false)
        .build()
        .or_else(|_| Glob::new(&globset::escape(pattern)))
        .unwrap_or_else(|_| Glob::new("").expect("empty glob"))
        .compile_matcher()
}

/// One `secrets:` list: names to redact and `!` exceptions.
#[derive(Clone, Debug, Default)]
struct NameList {
    secret: Vec<GlobMatcher>,
    except: Vec<GlobMatcher>,
}

impl NameList {
    fn new(entries: &[String]) -> Self {
        let mut l = Self::default();
        for e in entries {
            match e.trim().strip_prefix('!') {
                Some(x) => l.except.push(glob(x.trim())),
                None => l.secret.push(glob(e.trim())),
            }
        }
        l
    }

    /// `Some(false)` for an exception, `Some(true)` for a listed name.
    fn decide(&self, forms: &[&str]) -> Option<bool> {
        let hit = |gs: &[GlobMatcher]| gs.iter().any(|g| forms.iter().any(|f| g.is_match(f)));
        if hit(&self.except) {
            Some(false)
        } else if hit(&self.secret) {
            Some(true)
        } else {
            None
        }
    }
}

/// Decides which env names are secret (see the module docs).
#[derive(Clone, Debug)]
pub struct Redactor {
    patterns: Vec<GlobMatcher>,
    workspace: NameList,
    stems: HashMap<String, NameList>,
}

impl Default for Redactor {
    /// The built-in patterns only (nothing configured).
    fn default() -> Self {
        Self {
            patterns: BUILTIN_PATTERNS.iter().map(|p| glob(p)).collect(),
            workspace: NameList::default(),
            stems: HashMap::new(),
        }
    }
}

impl Redactor {
    /// From the resolved workspace: `redaction`, `secrets` and every
    /// stem's `secrets`.
    pub fn new(ws: &Workspace) -> Self {
        let mut patterns: Vec<GlobMatcher> = Vec::new();
        if ws.redaction.builtin {
            patterns.extend(BUILTIN_PATTERNS.iter().map(|p| glob(p)));
        }
        patterns.extend(ws.redaction.patterns.iter().map(|p| glob(p)));
        Self {
            patterns,
            workspace: NameList::new(&ws.secrets),
            stems: ws
                .stems
                .iter()
                .filter(|(_, s)| !s.secrets.is_empty())
                .map(|(n, s)| (n.clone(), NameList::new(&s.secrets)))
                .collect(),
        }
    }

    /// Whether env name `key` (of `stem`, or workspace-level) is secret.
    pub fn is_secret(&self, stem: Option<&str>, key: &str) -> bool {
        let norm = key.replace('-', "_");
        let forms = [key, norm.as_str()];
        if let Some(d) = stem
            .and_then(|s| self.stems.get(s))
            .and_then(|l| l.decide(&forms))
        {
            return d;
        }
        if let Some(d) = self.workspace.decide(&forms) {
            return d;
        }
        self.patterns
            .iter()
            .any(|g| forms.iter().any(|f| g.is_match(f)))
    }

    /// Replace the values of secret names with [`REDACTED`] in an env map
    /// (`map.iter_mut()` of a `BTreeMap` or an `IndexMap`).
    pub fn redact_pairs<'a>(
        &self,
        stem: Option<&str>,
        pairs: impl IntoIterator<Item = (&'a String, &'a mut String)>,
    ) {
        for (k, v) in pairs {
            if self.is_secret(stem, k) {
                *v = REDACTED.to_string();
            }
        }
    }

    /// Redact a JSON document shaped like the resolved config (a workspace,
    /// a bundle holding one, or a single stem when `stem` is given): in
    /// every env map (`env`, `local_env`, `*_env`, `environment`), values of
    /// secret names become [`REDACTED`]. Inside `stems.<name>` the stem's
    /// own `secrets` apply.
    pub fn redact_json(&self, v: &mut Value, stem: Option<&str>) {
        match v {
            Value::Object(m) => {
                for (k, child) in m.iter_mut() {
                    if is_env_key(k)
                        && let Value::Object(env) = child
                    {
                        for (ek, ev) in env.iter_mut() {
                            if !ev.is_null() && self.is_secret(stem, ek) {
                                *ev = Value::String(REDACTED.to_string());
                            }
                        }
                        continue;
                    }
                    if k == "stems"
                        && let Value::Object(stems) = child
                    {
                        for (name, s) in stems.iter_mut() {
                            self.redact_json(s, Some(name));
                        }
                        continue;
                    }
                    self.redact_json(child, stem);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|c| self.redact_json(c, stem)),
            _ => {}
        }
    }

    /// The secret values of `stem`'s configured env (its `env`, which holds
    /// the workspace `env` and `stems.local.yaml` values, plus every readable
    /// `env_files` entry, read now) that are at least [`MIN_SCRUB_LEN`]
    /// characters long, as `(KEY, value)`, longest value first (so a value
    /// containing another is replaced whole).
    pub fn secret_values(&self, stem: &Stem) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let files: Vec<(String, String)> = stem
            .env_files
            .iter()
            .enumerate()
            .filter_map(|(i, f)| crate::envfiles::read(Path::new("/"), &stem.name, i, f).ok())
            .flatten()
            .flatten()
            .collect();
        let pairs = stem
            .env
            .iter()
            .chain(stem.local_env.iter())
            .chain(files.iter().map(|(k, v)| (k, v)));
        for (k, v) in pairs {
            if v.chars().count() >= MIN_SCRUB_LEN
                && self.is_secret(Some(&stem.name), k)
                && !out.iter().any(|(_, x)| x == v)
            {
                out.push((k.clone(), v.clone()));
            }
        }
        out.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        out
    }
}

/// `env`, `local_env`, `base_env`, `environment`: maps of environment variables.
fn is_env_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    k == "env" || k == "environment" || k.ends_with("_env")
}

/// `text` with every secret value replaced by `<redacted:KEY>`; `None`
/// when it holds none (the common case, no allocation).
pub fn scrub(text: &str, secrets: &[(String, String)]) -> Option<String> {
    let mut out: Option<String> = None;
    for (k, v) in secrets {
        let cur = out.as_deref().unwrap_or(text);
        if cur.contains(v.as_str()) {
            out = Some(cur.replace(v.as_str(), &format!("<redacted:{k}>")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ws(yaml: &str) -> Workspace {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("stems.yaml"), yaml).unwrap();
        let opts = stems_config::LoadOptions {
            cwd: d.path().to_path_buf(),
            ..Default::default()
        };
        stems_config::load(opts).unwrap().workspace
    }

    #[test]
    fn builtin_patterns() {
        let r = Redactor::default();
        for k in [
            "API_TOKEN",
            "github-token",
            "DB_PASSWORD",
            "SMTP_PASSWD",
            "Secret_Thing",
            "AWS_CREDENTIALS",
            "STRIPE_API_KEY",
            "api_key",
            "AWS_ACCESS_KEY_ID",
            "MYAPIKEY",
            "token",
        ] {
            assert!(r.is_secret(None, k), "{k} should be secret");
        }
        for k in [
            "PORT",
            "HOST",
            "KEYCLOAK_URL",
            "MONKEY",
            "DATABASE_URL",
            "key",
        ] {
            assert!(!r.is_secret(None, k), "{k} should not be secret");
        }
    }

    #[test]
    fn lists_and_exceptions() {
        let w = ws(r#"
schema_version: 1
name: t
secrets: [PLAIN, "!PUBLIC_KEY", "*_DSN"]
redaction: { patterns: ["*_CERT"] }
env: { PUBLIC_KEY: pk }
stems:
  api:
    type: process
    secrets: ["!API_TOKEN", LOCAL_ONLY]
    scripts: { start: "sleep 1" }
  web:
    type: process
    scripts: { start: "sleep 1" }
"#);
        let r = Redactor::new(&w);
        assert!(r.is_secret(None, "PLAIN"));
        assert!(r.is_secret(None, "SENTRY_DSN"));
        assert!(r.is_secret(None, "tls_cert"));
        assert!(!r.is_secret(None, "PUBLIC_KEY"));
        assert!(!r.is_secret(Some("api"), "API_TOKEN"));
        assert!(r.is_secret(Some("web"), "API_TOKEN"));
        assert!(r.is_secret(Some("api"), "LOCAL_ONLY"));
        assert!(!r.is_secret(Some("web"), "LOCAL_ONLY"));
        assert!(r.is_secret(Some("api"), "PLAIN"));

        let off = ws(r#"
schema_version: 1
name: t
redaction: { builtin: false }
secrets: [ONLY_THIS]
"#);
        let r = Redactor::new(&off);
        assert!(!r.is_secret(None, "API_TOKEN"));
        assert!(r.is_secret(None, "ONLY_THIS"));
    }

    #[test]
    fn json_and_pairs() {
        let w = ws(r#"
schema_version: 1
name: t
stems:
  api:
    type: process
    secrets: [PLAIN]
    env: { PLAIN: hunter22, PORT: "1" }
    scripts: { start: "sleep 1" }
"#);
        let r = Redactor::new(&w);
        let mut v = json!({
            "workspace": {
                "env": {"API_TOKEN": "t", "PORT": "1", "PLAIN": "x"},
                "stems": {"api": {"env": {"PLAIN": "hunter22", "PORT": "1"},
                                  "local_env": {"DB_PASSWORD": "p"},
                                  "outputs": {"token": {"command": "cat t", "secret": true}}}}
            }
        });
        r.redact_json(&mut v, None);
        let w = &v["workspace"];
        assert_eq!(w["env"]["API_TOKEN"], REDACTED);
        assert_eq!(w["env"]["PORT"], "1");
        assert_eq!(w["env"]["PLAIN"], "x", "PLAIN is secret only in api");
        assert_eq!(w["stems"]["api"]["env"]["PLAIN"], REDACTED);
        assert_eq!(w["stems"]["api"]["env"]["PORT"], "1");
        assert_eq!(w["stems"]["api"]["local_env"]["DB_PASSWORD"], REDACTED);
        assert_eq!(w["stems"]["api"]["outputs"]["token"]["command"], "cat t");

        let mut one = json!({"env": {"PLAIN": "hunter22"}});
        r.redact_json(&mut one, Some("api"));
        assert_eq!(one["env"]["PLAIN"], REDACTED);

        let mut m: std::collections::BTreeMap<String, String> = [("A_TOKEN", "v"), ("PORT", "1")]
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .into();
        r.redact_pairs(None, m.iter_mut());
        assert_eq!(m["A_TOKEN"], REDACTED);
        assert_eq!(m["PORT"], "1");
    }

    #[test]
    fn values_and_scrubbing() {
        let w = ws(r#"
schema_version: 1
name: t
env: { SHARED_SECRET: sharedvalue }
stems:
  api:
    type: process
    secrets: [PLAIN, SHORTY]
    env: { API_TOKEN: s3cr3tvalue, PLAIN: hunter22, SHORTY: abc, PORT: "123456" }
    scripts: { start: "sleep 1" }
"#);
        let r = Redactor::new(&w);
        let api = w.stem("api").unwrap();
        let vals = r.secret_values(api);
        let keys: Vec<&str> = vals.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["API_TOKEN", "SHARED_SECRET", "PLAIN"]);
        assert_eq!(
            scrub("token=s3cr3tvalue plain=hunter22 short=abc", &vals).unwrap(),
            "token=<redacted:API_TOKEN> plain=<redacted:PLAIN> short=abc"
        );
        assert_eq!(scrub("nothing here 123456", &vals), None);
    }

    #[test]
    fn secret_values_include_env_files() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("stems.yaml"),
            "schema_version: 1\nname: t\nstems:\n  api:\n    type: process\n    env_files: [a.env, { path: missing.env, required: false }]\n    scripts: { start: 'true' }\n",
        )
        .unwrap();
        std::fs::write(
            d.path().join("a.env"),
            "FILE_SECRET=from-file-42\nPLAIN=visible-value\n",
        )
        .unwrap();
        let w = stems_config::load(stems_config::LoadOptions {
            cwd: d.path().to_path_buf(),
            ..Default::default()
        })
        .unwrap()
        .workspace;
        let vals = Redactor::new(&w).secret_values(w.stem("api").unwrap());
        assert_eq!(
            vals,
            [("FILE_SECRET".to_string(), "from-file-42".to_string())]
        );
    }
}
