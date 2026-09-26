//! Redaction of config JSON before an agent sees it.
//!
//! stems' config has no typed secret values (secret *outputs* are commands
//! whose results the daemon redacts), but env vars and variables often
//! carry credentials. So, anywhere in the JSON:
//! * a string value whose key looks like a secret (`*PASSWORD*`,
//!   `*SECRET*`, `*TOKEN*`, `*API_KEY*`, `*PRIVATE_KEY*`, `*CREDENTIAL*`,
//!   `*ACCESS_KEY*`, `*PASSWD*`) becomes `"<redacted>"`;
//! * the password of a URL (`scheme://user:pass@host`) becomes `<redacted>`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use stems_api::REDACTED;

const SECRET_WORDS: [&str; 9] = [
    "PASSWORD",
    "PASSWD",
    "SECRET",
    "TOKEN",
    "API_KEY",
    "APIKEY",
    "PRIVATE_KEY",
    "CREDENTIAL",
    "ACCESS_KEY",
];

static URL_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<pre>[A-Za-z][A-Za-z0-9+.-]*://[^:/@\s]+:)[^@/\s]+@").expect("valid regex")
});

/// Whether an env / variable name looks like it holds a secret.
pub fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_uppercase().replace('-', "_");
    SECRET_WORDS.iter().any(|w| k.contains(w))
}

/// Redact `v` in place.
pub fn redact(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if x.is_string() && is_secret_key(k) {
                    *x = Value::String(REDACTED.into());
                } else {
                    redact(x);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(redact),
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
