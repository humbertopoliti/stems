# stems-config

Loads a stems workspace: finds `stems.yaml`, expands `extends` / `include`,
overlays `stems.local.yaml`, applies defaults, substitutes `${…}` references
and returns a fully resolved `Workspace`. It also generates the JSON Schema in
`schema/stems.schema.json`. Semantic validation (unknown dependencies, cycles,
port conflicts, missing files) is `stems_core::validate` (deliverable 06);
this crate only rejects what cannot be parsed.

```rust
let resolved = stems_config::load(stems_config::LoadOptions::from_process()?)?;
for stem in resolved.workspace.stems() { /* enabled stems, declaration order */ }
```

## Pipeline

1. **Discovery**: `LoadOptions::workspace` (a directory or a file, relative to
   `cwd`), else `STEMS_WORKSPACE` from `LoadOptions::env`, else walk up from
   `cwd` looking for `stems.yaml`. Failure: `WORKSPACE_NOT_FOUND`.
2. **Per-file parse**: every file is parsed with `serde_yaml_ng` into the
   typed file schema (`raw::RawWorkspace`, all fields optional,
   `deny_unknown_fields`), so schema errors (`SCHEMA_INVALID`) carry the exact
   file, line and config path. Each file is also parsed with `marked-yaml` into
   the span index (`Resolved::spans`, `ConfigPath -> file:line:col`).
3. **Expansion**, per file, recursively: `extends` base first, then each
   `include` in list order, then the file's own content. `extends`/`include`
   paths are relative to the file that names them. Missing targets are
   `INCLUDE_NOT_FOUND`, loops `INCLUDE_CYCLE`, an `extends` chain deeper
   than 5 files `INCLUDE_CYCLE` too. A stem defined by two files of one
   include level (two included files, or an included file and the file
   including it) is `DUPLICATE_STEM` with both paths in
   `Diagnostic::details.files`; `extends` bases and `stems.local.yaml` may
   redefine stems (they are merged). See `docs/config.md`.
4. **Local overlay**: `stems.local.yaml` next to `stems.yaml` (unless
   `skip_local`) is expanded the same way and merged last.
5. **Substitution** over every string of the merged tree (see below).
6. **Resolution**: typed parse of the merged tree, defaults applied from
   `defaults.rs`, paths made absolute. A stem with no `type` after merging is
   fatal (`SCHEMA_INVALID`); a field that does not apply to the stem's type
   (e.g. `image` on a process stem) is a non-fatal diagnostic.

Fatal problems come back as `Err(ConfigErrors)`; everything else is collected
in `Resolved::diagnostics` (06 decides severity and maps both onto
`stems_core::Error`).

## Merge rules

Applied for `extends`, `include` and `stems.local.yaml` alike ("base" is what
has been merged so far, "overlay" the file being merged on top):

| Base | Overlay | Result |
|---|---|---|
| mapping | mapping | merged key by key, recursively; new keys appended in overlay order |
| `scripts` (workspace level or `stems.<n>.scripts`) | mapping | **per key**: an overlay script replaces the base script of that name whole (its `inputs`, `args`, … are not merged); other scripts are kept |
| mapping with `type: A` | mapping with `type: B` (A ≠ B) | replaced whole (a stem or a health check changing kind) |
| list | anything | replaced (lists never concatenate: `ports`, `depends_on`, `env_files`, `watch`, profile lists, …) |
| anything | scalar / list / `null` | replaced (`null` clears the value, restoring the default) |

Order of precedence, lowest first: `extends` base, `include`s (in order), the
including file, `stems.local.yaml`. So `stems.local.yaml` with

```yaml
stems:
  web:
    enabled: false
```

keeps `web` fully defined but disabled: `Workspace::stem("web")` returns it,
`Workspace::stems()` skips it and `Workspace::disabled_stems()` lists it.

Workspace-level `env` is folded into each stem's `env` (stem keys win).
`env_files` are *not* read here: they are resolved to absolute paths and the
runtime applies them after `env` (FR-ST-4). Keys set by `stems.local.yaml`
(its workspace-level `env` or the stem's `env`) are also recorded, with their
effective values, in `Stem::local_env`, so the runtime can re-apply them after
`env_files`: workspace env → stem env → env_files → local → shell.

## Substitution

References are written `${…}`. Only the namespaces below are interpreted;
anything else (`${HOME}`, `${1}`, `$PATH`) is left for the shell. `$${` is an
escape for a literal `${`. Substitution applies to string values only (numbers
such as ports must be literal).

| Reference | Value |
|---|---|
| `${var.x}` | workspace variable; variables may reference other variables, `env`, `workspace` and `stem` values; cycles are reported |
| `${env.X}` | `LoadOptions::env` at load time |
| `${workspace.root}`, `${workspace.name}` | integration repo root, workspace name |
| `${codebase}` | the enclosing stem's codebase directory (not allowed in `codebase:` itself) |
| `${stem.<n>.port}` | first declared port of stem `<n>` |
| `${stem.<n>.ports.<p>}` | port named `<p>` |
| `${stem.<n>.host}` | `localhost` (v1) |
| `${stem.self.…}` | the enclosing stem |
| `${stem.<n>.outputs.<x>}` | runtime value: kept literally, recorded as deferred |

A reference to a `port: auto` port cannot be known at load time: it is kept
literally (with `self` rewritten to the stem's name, e.g.
`${stem.shop-web.port}`) and recorded in `Resolved::deferred` with its config
path, for the daemon to fill once it has allocated the port. The same applies
to outputs (the daemon fills them in at the dependant's start, see
`docs/config.md` → Outputs; `TemplateContext` callers pass evaluated values
in `StemFacts::outputs`).

Every reference that cannot be resolved (undeclared variable, unset env var,
unknown stem, stem without ports, unknown port name, `${codebase}` outside a
stem or on a stem without a codebase, variable cycle) is left in place and
reported as an `UNRESOLVED_VARIABLE` diagnostic with its config path and
source location. Loading never panics on bad references.

## Paths

- Local codebases: `~` is expanded from `HOME`; relative paths resolve against
  the integration repo root (the directory of `stems.yaml`); the result is
  lexically normalised (it need not exist).
- Git codebases: `codebase: git@host:org/repo.git` (or `ssh://`, `https://`,
  `http://`, `git://`, `file://` URLs) or `codebase: { git, ref, path }`. The
  checkout directory is `path` if given, else `<repos_dir>/<stem>` with
  `repos_dir` defaulting to `<workspace>/.stems/repos`. Nothing is cloned here.
- Script `file:`, overlay `template:`/`file:`, `env_files` and compose `file:`
  are relative to the workspace root.
- A bare-string script (`seed: scripts/postgres/seed.sh`) is a **file** when
  it is a single word (no whitespace, no `$`) naming an existing file relative
  to the workspace root; otherwise it is an inline shell command
  (`start: python3 app.py`). Use `file:` explicitly to get a validation error
  (`SCRIPT_NOT_FOUND`) when the file is missing. Process `cwd` and script `cwd` are
  relative to the stem's codebase (else the workspace root). Docker
  `build.context` is relative to the codebase (else the workspace root),
  `build.dockerfile` relative to the context.

## `requires`

`requires: { node: ">=20" }` or, for tools without a built-in probe,
`requires: { mytool: { version: ">=1.2", command: "mytool -V", regex: "v([0-9.]+)" } }`
(`Requirement::{Version, Custom}`). Checking versions is `stems_core::validate`.

## Defaults

All defaults live in `src/defaults.rs`; `defaults::defaults_table()` is
golden-tested (`tests/snapshots/golden__defaults_table.snap`), so that
snapshot is the reference table.

## Editing `stems.local.yaml`

`edit::{get, set, unset}` edit YAML text by `ConfigPath` keeping comments
and layout (line-level, DECISIONS.md); `edit::write_checked` writes, runs a
validation closure and restores the original on failure. Used by
`stems config set|unset` (docs/config.md). `committed_tree` returns the
merged committed tree (no local file, no substitution) for copying lists.

## JSON Schema

`schema/stems.schema.json` is generated from `raw::RawWorkspace`. The test
`tests/schema.rs` fails if it drifts; regenerate with

```sh
UPDATE_SCHEMA=1 cargo test -p stems-config --test schema
```

Editors can use it via a first line
`# yaml-language-server: $schema=<path or URL>/stems.schema.json`.
