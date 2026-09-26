//! Reading files and expanding `extends` / `include`.

use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde_yaml_ng::{Mapping, Value};

use crate::codebase::normalize;
use crate::diagnostic::{Diagnostic, Span, codes};
use crate::merge::merge;
use crate::path::ConfigPath;
use crate::raw::RawWorkspace;
use crate::spans::SpanIndex;

/// Maximum number of `extends` hops followed from a file.
pub const MAX_EXTENDS_DEPTH: usize = 5;

/// Which file defined each stem.
type Origins = IndexMap<String, PathBuf>;

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
        self.load_file(file, from, 0).map(|(v, _)| v)
    }

    /// [`Self::load`], also returning which file defined each stem, with
    /// `extends_depth` = `extends` hops from the top file.
    fn load_file(
        &mut self,
        file: &Path,
        from: Option<&Path>,
        extends_depth: usize,
    ) -> Option<(Value, Origins)> {
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
        let mut origins = Origins::new();
        if let Some(base) = &raw.extends {
            if extends_depth >= MAX_EXTENDS_DEPTH {
                self.errors.push(
                    Diagnostic::new(
                        codes::INCLUDE_CYCLE,
                        format!(
                            "`extends` chain deeper than {MAX_EXTENDS_DEPTH} files: {} extends {base}",
                            file.display()
                        ),
                    )
                    .with_path(ConfigPath::root().key("extends"))
                    .with_location(local_spans.get(&ConfigPath::root().key("extends")).cloned())
                    .with_hint(format!(
                        "flatten the chain: at most {MAX_EXTENDS_DEPTH} levels of `extends` are followed (use `include:` for side-by-side files)"
                    ))
                    .with_details(serde_json::json!({
                        "file": file,
                        "extends": base,
                        "max_depth": MAX_EXTENDS_DEPTH,
                    })),
                );
            } else if let Some((v, o)) =
                self.load_file(&dir.join(base), Some(&file), extends_depth + 1)
            {
                // A base is meant to be overridden: its stems are not duplicates.
                merge(&mut result, v, &ConfigPath::root());
                origins.extend(o);
            }
        }
        // Stems contributed by each file of this level: every include, then
        // this file's own content. Two contributors of one stem = DUPLICATE_STEM.
        let mut level: IndexMap<String, PathBuf> = IndexMap::new();
        for inc in &raw.include {
            if let Some((v, o)) = self.load_file(&dir.join(inc), Some(&file), extends_depth) {
                merge(&mut result, v, &ConfigPath::root());
                for (stem, def) in o {
                    self.check_duplicate(&mut level, &stem, &def, None);
                }
            }
        }
        self.stack.pop();
        for stem in raw.stems.keys() {
            let at = local_spans
                .get(&ConfigPath::root().key("stems").key(stem))
                .cloned();
            self.check_duplicate(&mut level, stem, &file, at);
        }
        origins.extend(level);

        let mut own = value;
        if let Value::Mapping(m) = &mut own {
            m.remove("extends");
            m.remove("include");
        }
        merge(&mut result, own, &ConfigPath::root());
        self.spans_extend(local_spans);
        self.sources.push(file);
        Some((result, origins))
    }

    /// Record that `def` defines `stem` at this include level; a second
    /// file doing so is `DUPLICATE_STEM` (both paths in `details.files`).
    fn check_duplicate(
        &mut self,
        level: &mut IndexMap<String, PathBuf>,
        stem: &str,
        def: &Path,
        at: Option<Span>,
    ) {
        match level.get(stem) {
            Some(first) if first != def => {
                let path = ConfigPath::root().key("stems").key(stem);
                let location = at.or_else(|| self.spans.get(&path).cloned());
                self.errors.push(
                    Diagnostic::new(
                        codes::DUPLICATE_STEM,
                        format!(
                            "stem `{stem}` is defined in both {} and {}",
                            first.display(),
                            def.display()
                        ),
                    )
                    .with_path(path)
                    .with_location(location)
                    .with_hint(
                        "keep one definition; override fields from stems.local.yaml (or an `extends` base) instead of redefining the stem",
                    )
                    .with_details(serde_json::json!({
                        "stem": stem,
                        "files": [first, def],
                    })),
                );
            }
            _ => {
                level.insert(stem.to_string(), def.to_path_buf());
            }
        }
    }

    fn spans_extend(&mut self, other: SpanIndex) {
        self.spans.extend(other);
    }
}
