//! Reading files and expanding `extends` / `include`.

use std::path::{Path, PathBuf};

use serde_yaml_ng::{Mapping, Value};

use crate::codebase::normalize;
use crate::diagnostic::{Diagnostic, Span, codes};
use crate::merge::merge;
use crate::path::ConfigPath;
use crate::raw::RawWorkspace;
use crate::spans::SpanIndex;

/// Loads a file and everything it pulls in, in merge order.
#[derive(Default)]
pub(crate) struct Loader {
    pub sources: Vec<PathBuf>,
    pub spans: SpanIndex,
    pub errors: Vec<Diagnostic>,
    stack: Vec<PathBuf>,
}

/// Turn a serde_yaml_ng error into a located `SCHEMA_INVALID` diagnostic.
pub(crate) fn yaml_error(
    err: &serde_yaml_ng::Error,
    file: Option<&Path>,
    spans: &SpanIndex,
) -> Diagnostic {
    let full = err.to_string();
    let loc = err.location();
    let mut msg = full.as_str();
    if let Some(l) = &loc {
        let suffix = format!(" at line {} column {}", l.line(), l.column());
        msg = msg.strip_suffix(suffix.as_str()).unwrap_or(msg);
    }
    let mut path = None;
    if let Some((prefix, rest)) = msg.split_once(": ")
        && !prefix.contains(char::is_whitespace)
        && let Ok(p) = prefix.parse::<ConfigPath>()
    {
        path = Some(p);
        msg = rest;
    }
    let location = match (file, &loc) {
        (Some(f), Some(l)) => Some(Span {
            file: f.to_path_buf(),
            line: l.line(),
            col: l.column(),
        }),
        _ => None,
    };
    // Errors raised inside string-or-mapping values surface at the parent
    // map; the location is exact, so prefer a deeper path found there.
    if let Some(s) = &location
        && let Some(at) = spans.path_at(&s.file, s.line, s.col)
        && path.as_ref().is_none_or(|p| at.starts_with(p))
    {
        path = Some(at);
    }
    let location = location.or_else(|| path.as_ref().and_then(|p| spans.locate(p).cloned()));
    let mut d = Diagnostic::new(codes::SCHEMA_INVALID, msg.to_string()).with_location(location);
    d.path = path;
    d
}

impl Loader {
    /// Load `file` with its `extends` and `include`s expanded. Errors are
    /// collected in `self.errors`; `None` means the file could not be used.
    pub fn load(&mut self, file: &Path, from: Option<&Path>) -> Option<Value> {
        let file = normalize(file);
        if self.stack.contains(&file) {
            let mut chain: Vec<String> =
                self.stack.iter().map(|p| p.display().to_string()).collect();
            chain.push(file.display().to_string());
            self.errors.push(
                Diagnostic::new(
                    codes::INCLUDE_CYCLE,
                    format!("include/extends cycle: {}", chain.join(" -> ")),
                )
                .with_hint("remove one of the include/extends entries"),
            );
            return None;
        }
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => {
                let (code, what) = if e.kind() == std::io::ErrorKind::NotFound {
                    (codes::INCLUDE_NOT_FOUND, "not found".to_string())
                } else {
                    (codes::CONFIG_READ_FAILED, format!("cannot be read: {e}"))
                };
                let origin = from
                    .map(|f| format!(" (referenced from {})", f.display()))
                    .unwrap_or_default();
                self.errors.push(Diagnostic::new(
                    code,
                    format!("{} {what}{origin}", file.display()),
                ));
                return None;
            }
        };
        let mut local_spans = SpanIndex::default();
        local_spans.index_file(&file, &text);

        let value: Value = if text.trim().is_empty() {
            Value::Mapping(Mapping::new())
        } else {
            match serde_yaml_ng::from_str(&text) {
                Ok(Value::Null) => Value::Mapping(Mapping::new()),
                Ok(v) => v,
                Err(e) => {
                    self.errors.push(yaml_error(&e, Some(&file), &local_spans));
                    return None;
                }
            }
        };
        if !value.is_mapping() {
            self.errors.push(
                Diagnostic::new(codes::SCHEMA_INVALID, "the top level must be a mapping")
                    .with_location(Some(Span {
                        file: file.clone(),
                        line: 1,
                        col: 1,
                    })),
            );
            return None;
        }
        // Typed check of this file alone, for precisely located errors.
        let raw: RawWorkspace = match serde_yaml_ng::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                self.errors.push(yaml_error(&e, Some(&file), &local_spans));
                return None;
            }
        };

        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        self.stack.push(file.clone());
        let mut result = Value::Mapping(Mapping::new());
        if let Some(base) = &raw.extends
            && let Some(v) = self.load(&dir.join(base), Some(&file))
        {
            merge(&mut result, v, &ConfigPath::root());
        }
        for inc in &raw.include {
            if let Some(v) = self.load(&dir.join(inc), Some(&file)) {
                merge(&mut result, v, &ConfigPath::root());
            }
        }
        self.stack.pop();

        let mut own = value;
        if let Value::Mapping(m) = &mut own {
            m.remove("extends");
            m.remove("include");
        }
        merge(&mut result, own, &ConfigPath::root());
        self.spans_extend(local_spans);
        self.sources.push(file);
        Some(result)
    }

    fn spans_extend(&mut self, other: SpanIndex) {
        self.spans.extend(other);
    }
}
