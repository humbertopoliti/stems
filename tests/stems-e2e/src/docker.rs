//! Docker checks for `@docker` scenarios (deliverables 14/15), through the
//! `docker` CLI. The decisions (label/state checks on `docker inspect`
//! JSON, `docker compose ps` parsing, `docker run` arguments) are pure
//! functions unit-tested here without Docker; the step functions only run
//! the CLI and feed them.

use std::time::Duration;

use serde_json::Value;

/// Output of one `docker` invocation.
#[derive(Debug)]
pub struct Ran {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Run `docker <args>` (bounded by `timeout`).
pub async fn docker(args: &[String], timeout: Duration) -> Ran {
    let fut = tokio::process::Command::new("docker")
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(timeout, fut).await {
        Ok(Ok(o)) => Ran {
            ok: o.status.success(),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
        },
        Ok(Err(e)) => Ran {
            ok: false,
            stdout: String::new(),
            stderr: format!("cannot run docker: {e}"),
        },
        Err(_) => Ran {
            ok: false,
            stdout: String::new(),
            stderr: format!("docker {} timed out", args.join(" ")),
        },
    }
}

/// `k=v,k2=v2` → pairs (whitespace around items trimmed; empty items skipped).
pub fn parse_labels(s: &str) -> Result<Vec<(String, String)>, String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(|kv| {
            kv.split_once('=')
                .filter(|(k, _)| !k.is_empty())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .ok_or_else(|| format!("label `{kv}` is not `key=value`"))
        })
        .collect()
}

/// `docker run -d --name <name> --label k=v ... alpine:3 sleep 300`: a
/// long-running container carrying `labels` (orphan scenarios).
pub fn run_args(name: &str, labels: &[(String, String)]) -> Vec<String> {
    let mut a = vec!["run".into(), "-d".into(), "--name".into(), name.into()];
    for (k, v) in labels {
        a.push("--label".into());
        a.push(format!("{k}={v}"));
    }
    a.extend(["alpine:3".into(), "sleep".into(), "300".into()]);
    a
}

/// `docker inspect <name>` output (an array of one object) → that object.
pub fn inspect_object(stdout: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("docker inspect output is not JSON: {e}"))?;
    match v {
        Value::Array(mut a) if !a.is_empty() => Ok(a.remove(0)),
        Value::Object(_) => Ok(v),
        _ => Err("docker inspect returned nothing".into()),
    }
}

/// The container is running and carries label `k=v`.
pub fn check_running_with_label(inspect: &Value, k: &str, v: &str) -> Result<(), String> {
    if inspect["State"]["Running"] != Value::Bool(true) {
        return Err(format!(
            "container is not running (State.Status = {})",
            inspect["State"]["Status"]
        ));
    }
    match inspect["Config"]["Labels"][k].as_str() {
        Some(found) if found == v => Ok(()),
        Some(found) => Err(format!("label {k} is `{found}`, expected `{v}`")),
        None => Err(format!(
            "no label {k}; labels: {}",
            inspect["Config"]["Labels"]
        )),
    }
}

/// `docker compose ps --format json` output: a JSON array (compose <
/// 2.21) or NDJSON (2.21+); other lines are ignored.
pub fn parse_compose_ps(stdout: &str) -> Vec<Value> {
    let t = stdout.trim();
    if t.starts_with('[') {
        return serde_json::from_str::<Vec<Value>>(t).unwrap_or_default();
    }
    t.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(Value::is_object)
        .collect()
}

/// Some entry of `ps` is `service` and running.
pub fn compose_service_running(entries: &[Value], service: &str) -> bool {
    entries.iter().any(|e| {
        e["Service"].as_str() == Some(service)
            && e["State"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("running"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn labels_parse() {
        assert_eq!(
            parse_labels("stems.workspace=docker-pg, stems.stem=db").unwrap(),
            [
                ("stems.workspace".to_string(), "docker-pg".to_string()),
                ("stems.stem".to_string(), "db".to_string())
            ]
        );
        assert_eq!(parse_labels("a=").unwrap(), [("a".into(), String::new())]);
        assert!(parse_labels("nope").is_err());
        assert!(parse_labels("=v").is_err());
    }

    #[test]
    fn run_arguments() {
        let a = run_args("x", &[("k".into(), "v".into())]);
        assert_eq!(
            a,
            [
                "run", "-d", "--name", "x", "--label", "k=v", "alpine:3", "sleep", "300"
            ]
        );
    }

    #[test]
    fn inspect_checks() {
        let out = json!([{
            "Id": "abc",
            "State": { "Running": true, "Status": "running" },
            "Config": { "Labels": { "stems.workspace": "docker-pg", "stems.stem": "db" } }
        }])
        .to_string();
        let i = inspect_object(&out).unwrap();
        assert!(check_running_with_label(&i, "stems.stem", "db").is_ok());
        assert!(
            check_running_with_label(&i, "stems.stem", "api")
                .unwrap_err()
                .contains("`db`")
        );
        assert!(check_running_with_label(&i, "team", "x").is_err());
        let stopped = json!({ "State": { "Running": false, "Status": "exited" } });
        assert!(
            check_running_with_label(&stopped, "k", "v")
                .unwrap_err()
                .contains("exited")
        );
        assert!(inspect_object("[]").is_err());
        assert!(inspect_object("Error: No such object").is_err());
    }

    /// The `@docker` feature files never run without Docker, so check here
    /// that each of their steps matches exactly one step definition (and
    /// that each file has scenarios and is tagged `@docker`).
    #[test]
    fn docker_feature_steps_are_all_defined() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../features");
        let regexes: Vec<regex::Regex> = crate::steps::STEPS
            .iter()
            .map(|(r, _)| regex::Regex::new(r).unwrap())
            .collect();
        let mut files = 0;
        for dir in ["docker", "compose"] {
            let mut entries: Vec<_> = std::fs::read_dir(root.join(dir))
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_some_and(|x| x == "feature"))
                .collect();
            entries.sort();
            for f in entries {
                files += 1;
                let text = std::fs::read_to_string(&f).unwrap();
                let first = text.lines().next().unwrap_or_default();
                assert!(
                    first.split_whitespace().any(|t| t == "@docker"),
                    "{}: not tagged @docker",
                    f.display()
                );
                assert!(
                    text.contains("  Scenario: "),
                    "{}: no scenario",
                    f.display()
                );
                for (i, line) in text.lines().enumerate() {
                    let t = line.trim();
                    let Some(step) = ["Given ", "When ", "Then ", "And ", "But "]
                        .iter()
                        .find_map(|k| t.strip_prefix(k))
                    else {
                        continue;
                    };
                    let n = regexes.iter().filter(|r| r.is_match(step)).count();
                    assert_eq!(
                        n,
                        1,
                        "{}:{}: `{step}` matches {n} step definitions",
                        f.display(),
                        i + 1
                    );
                }
            }
        }
        assert!(
            files >= 12,
            "found only {files} docker/compose feature files"
        );
    }

    #[test]
    fn compose_ps_formats() {
        let array =
            r#"[{"Service":"redis","State":"running"},{"Service":"other","State":"exited"}]"#;
        let ndjson = "{\"Service\":\"redis\",\"State\":\"running\"}\nWARN something\n{\"Service\":\"other\",\"State\":\"exited\"}\n";
        for out in [array, ndjson] {
            let e = parse_compose_ps(out);
            assert_eq!(e.len(), 2, "{out}");
            assert!(compose_service_running(&e, "redis"));
            assert!(!compose_service_running(&e, "other"));
            assert!(!compose_service_running(&e, "db"));
        }
        assert!(parse_compose_ps("").is_empty());
    }
}
