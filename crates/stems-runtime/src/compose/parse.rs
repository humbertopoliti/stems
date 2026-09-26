//! Pure decisions of the compose runtime: `ps`/`version` parsing, the
//! project-in-use table, orphan selection, adoption checks and the mapping
//! of failed invocations to errors.

use std::collections::HashMap;

use bollard::models::ContainerInspectResponse;
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::runtime::{AdoptRecord, Orphan, OrphanKind, OrphanScope, RuntimeError};

/// Lines of compose output kept for [`RuntimeError::ComposeFailed`].
pub const COMPOSE_TAIL_LINES: usize = 20;
/// Compose's own container labels.
pub const LABEL_COMPOSE_PROJECT: &str = "com.docker.compose.project";
pub const LABEL_COMPOSE_SERVICE: &str = "com.docker.compose.service";
pub const LABEL_COMPOSE_CONFIG_FILES: &str = "com.docker.compose.project.config_files";

/// Hint when the `docker` CLI or its compose plugin is missing.
pub const COMPOSE_MISSING_HINT: &str = "install the Docker CLI with the Compose v2 plugin (Docker Desktop, or `docker-compose-plugin`) so that `docker compose version` works";

/// One container of `docker compose ps --format json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PsEntry {
    pub id: String,
    pub name: String,
    pub service: String,
    /// `running`, `exited`, `created`, `restarting`, ...
    pub state: String,
    pub project: String,
    /// `starting` / `healthy` / `unhealthy`; `None` without a health check.
    pub health: Option<String>,
    pub image: Option<String>,
}

impl PsEntry {
    pub fn is_running(&self) -> bool {
        self.state.eq_ignore_ascii_case("running")
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPs {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Service")]
    service: String,
    #[serde(rename = "State")]
    state: String,
    #[serde(rename = "Project")]
    project: String,
    #[serde(rename = "Health")]
    health: String,
    #[serde(rename = "Image")]
    image: String,
}

impl From<RawPs> for PsEntry {
    fn from(r: RawPs) -> Self {
        let opt = |s: String| Some(s).filter(|s| !s.is_empty());
        PsEntry {
            id: r.id,
            name: r.name,
            service: r.service,
            state: r.state,
            project: r.project,
            health: opt(r.health),
            image: opt(r.image),
        }
    }
}

/// Parse `docker compose ps --format json`: a JSON array (compose before
/// 2.21) or NDJSON, one object per line (2.21+). Blank output is no
/// containers; lines that are not JSON objects (warnings) are skipped.
pub fn parse_ps_json(s: &str) -> Vec<PsEntry> {
    let t = s.trim();
    if t.is_empty() {
        return Vec::new();
    }
    if t.starts_with('[')
        && let Ok(v) = serde_json::from_str::<Vec<RawPs>>(t)
    {
        return v.into_iter().map(PsEntry::from).collect();
    }
    t.lines()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .filter_map(|l| serde_json::from_str::<RawPs>(l).ok())
        .map(PsEntry::from)
        .collect()
}

/// Leading `\d+(\.\d+){0,2}` of `s` (after an optional `v`), padded to
/// three components, plus a semver pre-release/build suffix if present.
fn lenient_version(s: &str) -> Option<Version> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    if let Ok(v) = Version::parse(s) {
        return Some(v);
    }
    let core: String = s
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts: Vec<u64> = core
        .split('.')
        .filter(|p| !p.is_empty())
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    parts.resize(3, 0);
    Some(Version::new(parts[0], parts[1], parts[2]))
}

/// Parse `docker compose version --format json` (`{"version":"v2.24.5"}`)
/// or the plain form (`Docker Compose version v2.24.5`,
/// `docker-compose version 1.29.2, build 5becea4c`).
pub fn parse_version(s: &str) -> Option<Version> {
    let t = s.trim();
    if t.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(t).ok()?;
        return lenient_version(v.get("version")?.as_str()?);
    }
    let lower = t.to_ascii_lowercase();
    let after = lower
        .find("version")
        .map_or(t, |i| &t[i + "version".len()..]);
    let token = after
        .split(|c: char| c.is_whitespace() || c == ',')
        .find(|w| !w.is_empty())?;
    lenient_version(token)
}

/// Require Compose v2 (or later). v1 and unparseable output are
/// [`RuntimeError::ComposeUnavailable`].
pub fn require_v2(output: &str) -> Result<Version, RuntimeError> {
    match parse_version(output) {
        Some(v) if v.major >= 2 => Ok(v),
        Some(v) => Err(RuntimeError::ComposeUnavailable {
            hint: format!("Compose v1 ({v}) is not supported; {COMPOSE_MISSING_HINT}"),
        }),
        None => Err(RuntimeError::ComposeUnavailable {
            hint: format!(
                "cannot parse `docker compose version` output `{}`; {COMPOSE_MISSING_HINT}",
                output.trim()
            ),
        }),
    }
}

/// What to do with a compose project before `up`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectDecision {
    /// No container of the project exists.
    Free,
    /// It has containers and stems owns it (ownership marker present, or
    /// it is the default `stems-<ws>` project).
    Owned,
    /// It has containers stems did not create, and the stem says `adopt: true`.
    Adopt,
    /// It has containers stems did not create: `COMPOSE_PROJECT_IN_USE`.
    InUse,
}

impl ProjectDecision {
    pub fn allows_start(self) -> bool {
        !matches!(self, ProjectDecision::InUse)
    }
}

/// The project-in-use table. `entries` is `docker compose -p <project> ps
/// -a` (entries of other projects are ignored); `marker` is whether
/// `<env_dir>/<project>.owned` exists.
pub fn project_decision(
    entries: &[PsEntry],
    project: &str,
    is_default_project: bool,
    marker: bool,
    adopt: bool,
) -> ProjectDecision {
    let has_containers = entries
        .iter()
        .any(|e| e.project.is_empty() || e.project == project);
    match (has_containers, marker || is_default_project, adopt) {
        (false, _, _) => ProjectDecision::Free,
        (true, true, _) => ProjectDecision::Owned,
        (true, false, true) => ProjectDecision::Adopt,
        (true, false, false) => ProjectDecision::InUse,
    }
}

/// The container `up` produced for `service`: a running one if any, else
/// the first match.
pub fn pick_container<'a>(
    entries: &'a [PsEntry],
    project: &str,
    service: &str,
) -> Option<&'a PsEntry> {
    let all: Vec<&PsEntry> = entries
        .iter()
        .filter(|e| e.service == service && (e.project.is_empty() || e.project == project))
        .filter(|e| !e.id.is_empty())
        .collect();
    all.iter().find(|e| e.is_running()).or(all.first()).copied()
}

fn ids_match(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && (a.starts_with(b) || b.starts_with(a))
}

/// Containers of stems-owned compose projects that are not in `scope.known`.
pub fn select_compose_orphans(entries: &[PsEntry], scope: &OrphanScope) -> Vec<Orphan> {
    entries
        .iter()
        .filter(|e| !e.id.is_empty())
        .filter(|e| {
            !scope
                .known
                .iter()
                .filter_map(|r| r.container_id.as_deref())
                .any(|k| ids_match(k, &e.id))
        })
        .map(|e| Orphan {
            kind: OrphanKind::Container,
            port: None,
            pid: None,
            pgid: None,
            command: format!(
                "compose {}/{}: {} ({}{})",
                e.project,
                e.service,
                e.name,
                e.state,
                e.image
                    .as_deref()
                    .map(|i| format!(", {i}"))
                    .unwrap_or_default()
            ),
            container_id: Some(e.id.clone()),
            stem: None,
            // Only stems-owned projects are scanned.
            matches_start_command: true,
        })
        .collect()
}

/// Compose labels of an inspected container.
pub fn inspect_labels(inspect: &ContainerInspectResponse) -> Option<&HashMap<String, String>> {
    inspect.config.as_ref()?.labels.as_ref()
}

pub(crate) fn inspect_running(inspect: &ContainerInspectResponse) -> bool {
    inspect
        .state
        .as_ref()
        .and_then(|s| s.running)
        .unwrap_or(false)
}

/// Why a recorded compose container was not adopted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeAdoptRejection {
    NoContainerId,
    IdMismatch { found: String },
    WrongProject { found: Option<String> },
    WrongService { found: Option<String> },
    NotRunning,
}

/// Adoption decision: same id, `com.docker.compose.project == project`,
/// `com.docker.compose.service == service` (when given), and running.
pub fn verify_compose_adoption(
    inspect: &ContainerInspectResponse,
    record: &AdoptRecord,
    project: &str,
    service: Option<&str>,
) -> Result<(), ComposeAdoptRejection> {
    let want = record
        .container_id
        .as_deref()
        .ok_or(ComposeAdoptRejection::NoContainerId)?;
    let found = inspect.id.clone().unwrap_or_default();
    if !ids_match(&found, want) {
        return Err(ComposeAdoptRejection::IdMismatch { found });
    }
    let labels = inspect_labels(inspect);
    let get = |k: &str| labels.and_then(|l| l.get(k)).cloned();
    let p = get(LABEL_COMPOSE_PROJECT);
    if p.as_deref() != Some(project) {
        return Err(ComposeAdoptRejection::WrongProject { found: p });
    }
    if let Some(service) = service {
        let s = get(LABEL_COMPOSE_SERVICE);
        if s.as_deref() != Some(service) {
            return Err(ComposeAdoptRejection::WrongService { found: s });
        }
    }
    if !inspect_running(inspect) {
        return Err(ComposeAdoptRejection::NotRunning);
    }
    Ok(())
}

/// The last [`COMPOSE_TAIL_LINES`] non-empty lines of stdout then stderr
/// (compose reports errors on stderr, so they end the tail).
pub fn output_tail(stdout: &str, stderr: &str) -> Vec<String> {
    let lines: Vec<String> = stdout
        .lines()
        .chain(stderr.lines())
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    let skip = lines.len().saturating_sub(COMPOSE_TAIL_LINES);
    lines.into_iter().skip(skip).collect()
}

/// Output that means the Docker engine is not reachable.
fn engine_unreachable(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("cannot connect to the docker daemon")
        || l.contains("is the docker daemon running")
        || l.contains("error during connect")
}

/// Output that means `compose` is not a docker subcommand.
fn compose_missing(line: &str) -> bool {
    let l = line.to_ascii_lowercase();
    l.contains("'compose' is not a docker command")
        || l.contains("unknown command \"compose\"")
        || l.contains("unknown command: docker compose")
}

/// Map a failed invocation: engine unreachable → `DockerUnavailable`,
/// compose plugin missing → `ComposeUnavailable`, anything else →
/// `ComposeFailed` with the output tail.
pub fn failure_error(
    command: String,
    exit: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> RuntimeError {
    let tail = output_tail(stdout, stderr);
    if let Some(l) = tail.iter().find(|l| engine_unreachable(l)) {
        return RuntimeError::DockerUnavailable {
            hint: format!("{l}; {}", crate::docker::UNAVAILABLE_HINT),
        };
    }
    if tail.iter().any(|l| compose_missing(l)) {
        return RuntimeError::ComposeUnavailable {
            hint: COMPOSE_MISSING_HINT.to_string(),
        };
    }
    RuntimeError::ComposeFailed {
        command,
        exit,
        tail,
    }
}

/// `stems-<ws>`, lower-cased with characters compose rejects replaced by `-`.
pub fn default_project_name(workspace: &str) -> String {
    let ws: String = workspace
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("stems-{ws}")
}
