//! `env_files` (FR-ST-4): the dotenv grammar, reading an entry (required or
//! `required: false`), and the `ENV_SHADOWED` diagnostics. Shared by
//! `validate`, `doctor` and the daemon's `build_env`, so all three agree on
//! what a file contributes.

use std::path::Path;

use serde_json::json;
use stems_config::{ConfigPath, EnvFile, Stem};

use crate::error::{Error, ErrorCode};

/// Parse dotenv text: `KEY=value` lines, optional `export `, `#` comment
/// lines, blank lines; one pair of matching single/double quotes around the
/// value is stripped (no escapes, no interpolation). Lines without `=` or
/// with an empty key are ignored.
pub fn parse_dotenv(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (k, v) = line.split_once('=')?;
            let k = k.trim();
            if k.is_empty() {
                return None;
            }
            let v = v.trim();
            let v = ['"', '\'']
                .iter()
                .find_map(|q| {
                    v.strip_prefix(*q)
                        .and_then(|x| x.strip_suffix(*q))
                        .filter(|_| v.len() >= 2)
                })
                .unwrap_or(v);
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// `stems.<stem>.env_files[<i>]`.
pub fn entry_path(stem: &str, i: usize) -> ConfigPath {
    ConfigPath::root()
        .key("stems")
        .key(stem)
        .key("env_files")
        .index(i)
}

/// `p` relative to `root` when inside it, else as is.
pub fn shown(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

/// Read entry `i` of `stem`'s `env_files`. `Ok(None)`: an absent
/// `required: false` file. A required file that does not exist is
/// `ENV_FILE_NOT_FOUND`; one that exists but cannot be read (permissions, a
/// directory) is `CONFIG_READ_FAILED` whatever `required` says.
pub fn read(
    root: &Path,
    stem: &str,
    i: usize,
    f: &EnvFile,
) -> Result<Option<Vec<(String, String)>>, Error> {
    let rel = shown(root, &f.path);
    match std::fs::read_to_string(&f.path) {
        Ok(text) => Ok(Some(parse_dotenv(&text))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !f.required => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::new(
            ErrorCode::EnvFileNotFound,
            format!("env file {rel} of `{stem}` does not exist"),
        )
        .with_path(entry_path(stem, i))
        .with_hint(format!(
            "create it, or mark it optional: `- {{ path: {rel}, required: false }}`"
        ))
        .with_details(json!({ "stem": stem, "file": f.path }))),
        Err(e) => Err(Error::new(
            ErrorCode::ConfigReadFailed,
            format!("env file {rel} of `{stem}` cannot be read: {e}"),
        )
        .with_path(entry_path(stem, i))
        .with_hint("check that it is a readable file (permissions, not a directory)")
        .with_details(json!({ "stem": stem, "file": f.path }))),
    }
}

/// Every read error of `stem`'s `env_files` (see [`read`]).
pub fn errors(root: &Path, stem: &Stem) -> Vec<Error> {
    stem.env_files
        .iter()
        .enumerate()
        .filter_map(|(i, f)| read(root, &stem.name, i, f).err())
        .collect()
}

/// `ENV_SHADOWED` warnings for `stem`: an env file sets a key that the
/// stem's `env:` (workspace `env` included) or an earlier env file sets
/// to a different value. Keys set by `stems.local.yaml` are skipped (the
/// local value wins over every file anyway); unreadable files are skipped
/// (they are [`errors`]).
pub fn shadow_warnings(root: &Path, stem: &Stem) -> Vec<Error> {
    // key -> (value, where it was set: "env" or a file shown relative to root)
    let mut seen: std::collections::BTreeMap<String, (String, String)> = stem
        .env
        .iter()
        .filter(|(k, _)| !stem.local_env.contains_key(*k))
        .map(|(k, v)| (k.clone(), (v.clone(), "env".to_string())))
        .collect();
    let mut out = Vec::new();
    for (i, f) in stem.env_files.iter().enumerate() {
        let Ok(Some(pairs)) = read(root, &stem.name, i, f) else {
            continue;
        };
        let file = shown(root, &f.path);
        for (k, v) in pairs {
            if stem.local_env.contains_key(&k) {
                continue;
            }
            if let Some((old, from)) = seen.get(&k)
                && *old != v
                && *from != file
            {
                let set_in = if from == "env" {
                    format!("`stems.{}.env`", stem.name)
                } else {
                    from.clone()
                };
                out.push(
                    Error::new(
                        ErrorCode::EnvShadowed,
                        format!(
                            "`{k}` of `{}` from {file} shadows the value set in {set_in}",
                            stem.name
                        ),
                    )
                    .with_path(entry_path(&stem.name, i))
                    .with_hint(
                        "remove one, or move the value into stems.local.yaml if it is a local override",
                    )
                    .with_details(json!({
                        "stem": stem.name,
                        "key": k,
                        "file": file,
                        "shadows": from,
                    })),
                );
            }
            seen.insert(k, (v, file.clone()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv() {
        let got = parse_dotenv(
            "# c\n\nA=1\nexport B = two words \nC=\"quoted # x\"\nD='s'\nnoeq\n=x\nE=a=b\n",
        );
        let want = [
            ("A", "1"),
            ("B", "two words"),
            ("C", "quoted # x"),
            ("D", "s"),
            ("E", "a=b"),
        ];
        assert_eq!(
            got,
            want.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<Vec<_>>()
        );
    }

    fn file(path: &Path, required: bool) -> EnvFile {
        EnvFile {
            path: path.to_path_buf(),
            required,
        }
    }

    #[test]
    fn read_matrix() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let present = root.join("a.env");
        std::fs::write(&present, "A=1\n").unwrap();
        let absent = root.join("missing.env");
        let a_dir = root.join("adir");
        std::fs::create_dir(&a_dir).unwrap();

        for required in [true, false] {
            let got = read(root, "api", 0, &file(&present, required)).unwrap();
            assert_eq!(got, Some(vec![("A".into(), "1".into())]));
        }
        assert_eq!(read(root, "api", 1, &file(&absent, false)).unwrap(), None);
        let e = read(root, "api", 1, &file(&absent, true)).unwrap_err();
        assert_eq!(e.code, ErrorCode::EnvFileNotFound);
        assert_eq!(e.path.unwrap().to_string(), "stems.api.env_files[1]");
        assert!(e.message.contains("missing.env"), "{}", e.message);
        assert!(e.hint.unwrap().contains("required: false"));
        for required in [true, false] {
            let e = read(root, "api", 2, &file(&a_dir, required)).unwrap_err();
            assert_eq!(e.code, ErrorCode::ConfigReadFailed, "required={required}");
        }
    }

    #[test]
    fn shadowing_follows_the_layer_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = &dir.path().canonicalize().unwrap();
        std::fs::write(root.join("one.env"), "A=file1\nB=env\nC=file1\nL=x\n").unwrap();
        std::fs::write(root.join("two.env"), "C=file2\nD=file2\n").unwrap();
        std::fs::write(
            root.join("stems.yaml"),
            "schema_version: 1\nname: t\nenv: { A: env }\nstems:\n  api:\n    type: process\n    command: x\n    env: { B: env, L: base }\n    env_files: [one.env, two.env, { path: nope.env, required: false }]\n",
        )
        .unwrap();
        std::fs::write(
            root.join("stems.local.yaml"),
            "stems: { api: { env: { L: local } } }\n",
        )
        .unwrap();
        let r = stems_config::load(stems_config::LoadOptions {
            workspace: Some(root.to_path_buf()),
            cwd: root.to_path_buf(),
            env: Default::default(),
            skip_local: false,
        })
        .unwrap();
        let root = &r.workspace.root;
        let stem = r.workspace.stem("api").unwrap().clone();
        let w = shadow_warnings(root, &stem);
        let got: Vec<(String, String, String, String)> = w
            .iter()
            .map(|e| {
                (
                    e.details["key"].as_str().unwrap().to_string(),
                    e.details["file"].as_str().unwrap().to_string(),
                    e.details["shadows"].as_str().unwrap().to_string(),
                    e.path.as_ref().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    "A".into(),
                    "one.env".into(),
                    "env".into(),
                    "stems.api.env_files[0]".into()
                ),
                (
                    "C".into(),
                    "two.env".into(),
                    "one.env".into(),
                    "stems.api.env_files[1]".into()
                ),
            ]
        );
        assert_eq!(w[0].code, ErrorCode::EnvShadowed);
        assert_eq!(
            w[0].message,
            "`A` of `api` from one.env shadows the value set in `stems.api.env`"
        );
    }
}
