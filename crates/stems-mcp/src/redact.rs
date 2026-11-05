//! Redaction of config JSON before an agent sees it.
//!
//! stems' config has no typed secret values (secret *outputs* are commands
//! whose results the daemon redacts), but env vars and variables often
//! carry credentials. The names are decided by the shared
//! [`stems_core::redact::Redactor`] (47: built-in patterns, `secrets:`,
//! `redaction:`; `docs/config.md#secrets`). Anywhere in the JSON:
//! * a string value whose key is a secret name becomes `"<redacted>"`
//!   (in a stem's env maps, that stem's own `secrets:` apply too);
//! * the password of a URL (`scheme://user:pass@host`) becomes `<redacted>`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use stems_api::REDACTED;
use stems_core::redact::Redactor;

static URL_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<pre>[A-Za-z][A-Za-z0-9+.-]*://[^:/@\s]+:)[^@/\s]+@").expect("valid regex")
});

/// Whether an env / variable name looks like it holds a secret (the
/// built-in patterns only).
pub fn is_secret_key(key: &str) -> bool {
    Redactor::default().is_secret(None, key)
}

/// Redact `v` in place with the built-in patterns.
pub fn redact(v: &mut Value) {
    redact_with(v, &Redactor::default());
}

/// Redact `v` (a config document of `stem`, or the workspace's) in place
/// with the workspace's `redactor`.
pub fn redact_config(v: &mut Value, stem: Option<&str>, redactor: &Redactor) {
    redactor.redact_json(v, stem);
    redact_with(v, redactor);
}

fn redact_with(v: &mut Value, r: &Redactor) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if x.is_string() && r.is_secret(None, k) {
                    *x = Value::String(REDACTED.into());
                } else {
                    redact_with(x, r);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| redact_with(x, r)),
        Value::String(s) if URL_PASSWORD.is_match(s) => {
            *s = URL_PASSWORD
                .replace_all(s, format!("${{pre}}{REDACTED}@").as_str())
                .into_owned();
        }
        _ => {}
    }
}

/// `v` redacted.
pub fn redacted(mut v: Value) -> Value {
    redact(&mut v);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_and_urls() {
        let v = redacted(json!({
            "env": {
                "DB_PASSWORD": "hunter2",
                "STRIPE_API_KEY": "sk_live",
                "PORT": "8080",
                "DATABASE_URL": "postgres://shop:s3cret@localhost:5432/shop",
                "PLAIN_URL": "http://localhost:8080/x"
            },
            "vars": {"github-token": "ghp_x"},
            "outputs": {"token": {"command": "cat t", "secret": true}}
        }));
        assert_eq!(v["env"]["DB_PASSWORD"], "<redacted>");
        assert_eq!(v["env"]["STRIPE_API_KEY"], "<redacted>");
        assert_eq!(v["env"]["PORT"], "8080");
        assert_eq!(
            v["env"]["DATABASE_URL"],
            "postgres://shop:<redacted>@localhost:5432/shop"
        );
        assert_eq!(v["env"]["PLAIN_URL"], "http://localhost:8080/x");
        assert_eq!(v["vars"]["github-token"], "<redacted>");
        assert_eq!(v["outputs"]["token"]["command"], "cat t");
    }
}
