//! Raw (merged, substituted) schema → resolved [`Workspace`], applying
//! [`crate::defaults`].

use std::path::{Path, PathBuf};

use indexmap::IndexMap;

use crate::codebase::{resolve_codebase, resolve_path};
use crate::defaults as d;
use crate::diagnostic::{Diagnostic, codes};
use crate::model::*;
use crate::path::ConfigPath;
use crate::raw::*;
use crate::subst::{DeferredKind, DeferredRef};
use crate::types::{HealthType, Scalar, StemType};

/// Inputs shared by every resolution step.
pub(crate) struct Ctx<'a> {
    pub root: &'a Path,
    pub home: Option<&'a str>,
    pub ws_name: String,
    pub repos_dir: PathBuf,
    pub diagnostics: Vec<Diagnostic>,
    pub deferred: Vec<DeferredRef>,
}

impl<'a> Ctx<'a> {
    pub fn new(raw: &RawWorkspace, root: &'a Path, home: Option<&'a str>) -> Self {
        Self {
            root,
            home,
            ws_name: workspace_name(raw, root),
            repos_dir: resolve_path(root, raw.repos_dir.as_deref().unwrap_or(d::REPOS_DIR), home),
            diagnostics: Vec::new(),
            deferred: Vec::new(),
        }
    }

    fn schema(&mut self, path: ConfigPath, msg: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::new(codes::SCHEMA_INVALID, msg).with_path(path));
    }

    fn path(&self, base: &Path, s: &str) -> PathBuf {
        resolve_path(base, s, self.home)
    }
}

/// Workspace name: `name:` or the root directory's name.
pub(crate) fn workspace_name(raw: &RawWorkspace, root: &Path) -> String {
    raw.name.clone().unwrap_or_else(|| {
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "workspace".into())
    })
}

fn scalars(m: &IndexMap<String, Scalar>) -> IndexMap<String, String> {
    m.iter().map(|(k, v)| (k.clone(), v.to_string())).collect()
}

/// Resolve a stem's port list; invalid entries become diagnostics.
pub(crate) fn resolve_ports(
    raw: Option<&[RawPort]>,
    kind: Option<StemType>,
    path: &ConfigPath,
    diags: &mut Vec<Diagnostic>,
) -> Vec<Port> {
    let containerised = matches!(kind, Some(StemType::Docker | StemType::Compose));
    let mut out = Vec::new();
    for (i, p) in raw.unwrap_or_default().iter().enumerate() {
        let (name, port, container) = match p {
            RawPort::Number(n) => (None, PortRef::Fixed(*n), None),
            RawPort::Mapping(s) => {
                let parse = |x: &str| x.trim().parse::<u16>().ok();
                let parsed = match s.split_once(':') {
                    Some((h, c)) => parse(h)
                        .zip(parse(c))
                        .map(|(h, c)| (PortRef::Fixed(h), Some(c))),
                    None if s.trim() == "auto" => Some((PortRef::Auto, None)),
                    None => parse(s).map(|h| (PortRef::Fixed(h), None)),
                };
                let Some((port, container)) = parsed else {
                    diags.push(
                        Diagnostic::new(
                            codes::SCHEMA_INVALID,
                            format!(
                                "invalid port `{s}`: expected `5432`, `\"15432:5432\"` or `auto`"
                            ),
                        )
                        .with_path(path.index(i)),
                    );
                    continue;
                };
                (None, port, container)
            }
            RawPort::Spec(spec) => {
                let port = match spec.port {
                    RawPortValue::Number(n) => PortRef::Fixed(n),
                    RawPortValue::Auto(_) => PortRef::Auto,
                };
                (spec.name.clone(), port, spec.container_port)
            }
        };
        let container_port = if containerised {
            container.or(match port {
                PortRef::Fixed(n) => Some(n),
                PortRef::Auto => None,
            })
        } else {
            if container.is_some() {
                diags.push(
                    Diagnostic::new(
                        codes::SCHEMA_INVALID,
                        "`container_port` only applies to docker and compose stems",
                    )
                    .with_path(path.index(i)),
                );
            }
            None
        };
        out.push(Port {
            name: name.unwrap_or_else(|| d::port_name(i)),
            port,
            container_port,
        });
    }
    out
}

/// Build the resolved workspace.
pub(crate) fn resolve_workspace(
    raw: RawWorkspace,
    ctx: &mut Ctx<'_>,
    config_file: &Path,
    vars: IndexMap<String, String>,
) -> Result<Workspace, Vec<Diagnostic>> {
    let root = ctx.root;
    let ws_env = scalars(&raw.env);
    let mut fatal = Vec::new();

    let scripts = raw
        .scripts
        .iter()
        .map(|(k, s)| {
            let p = ConfigPath::root().key("scripts").key(k);
            (k.clone(), resolve_script(s, root, &p, ctx))
        })
        .collect();

    let mut stems = IndexMap::new();
    for (name, rs) in &raw.stems {
        let p = ConfigPath::root().key("stems").key(name);
        match resolve_stem(name, rs, &ws_env, &p, ctx) {
            Ok(s) => {
                stems.insert(name.clone(), s);
            }
            Err(e) => fatal.push(*e),
        }
    }
    if !fatal.is_empty() {
        return Err(fatal);
    }

    let agent = raw.agent.unwrap_or_default();
    let logs = raw.logs.unwrap_or_default();
    let metrics = raw.metrics.unwrap_or_default();
    Ok(Workspace {
        schema_version: raw.schema_version.unwrap_or(d::SCHEMA_VERSION),
        name: ctx.ws_name.clone(),
        root: root.to_path_buf(),
        config_file: config_file.to_path_buf(),
        vars,
        env: ws_env,
        requires: raw.requires,
        profiles: raw
            .profiles
            .into_iter()
            .map(|(k, v)| {
                let p = match v {
                    RawProfile::Stems(s) => Profile::Stems(s),
                    RawProfile::Alias(a) => Profile::Alias(a),
                };
                (k, p)
            })
            .collect(),
        default_profile: raw.default_profile,
        strict_profiles: raw.strict_profiles.unwrap_or(d::STRICT_PROFILES),
        agent: Agent {
            allow_destructive: agent
                .allow_destructive
                .unwrap_or(d::AGENT_ALLOW_DESTRUCTIVE),
            allowed_tools: agent.allowed_tools.unwrap_or_default(),
            denied_tools: agent.denied_tools.unwrap_or_default(),
        },
        logs: Logs {
            max_size: logs.max_size.unwrap_or(d::LOGS_MAX_SIZE),
            keep: logs.keep.unwrap_or(d::LOGS_KEEP),
            ring: logs.ring.unwrap_or(d::LOGS_RING),
        },
        metrics: Metrics {
            interval: metrics.interval.unwrap_or(d::METRICS_INTERVAL),
            persist: metrics.persist.unwrap_or(d::METRICS_PERSIST),
        },
        repos_dir: ctx.repos_dir.clone(),
        scripts,
        stems,
    })
}

fn resolve_script(s: &RawScript, default_cwd: &Path, p: &ConfigPath, ctx: &mut Ctx<'_>) -> Script {
    let spec = match s {
        RawScript::Inline(c) if names_script_file(c, ctx.root) => RawScriptSpec {
            file: Some(c.trim().to_string()),
            ..Default::default()
        },
        RawScript::Inline(c) => RawScriptSpec {
            command: Some(c.clone()),
            ..Default::default()
        },
        RawScript::Spec(spec) => (**spec).clone(),
    };
    let source = match (&spec.command, &spec.file) {
        (Some(c), None) => ScriptSource::Command(c.clone()),
        (None, Some(f)) => ScriptSource::File(ctx.path(ctx.root, f)),
        (Some(c), Some(_)) => {
            ctx.schema(
                p.clone(),
                "a script takes either `command` or `file`, not both",
            );
            ScriptSource::Command(c.clone())
        }
        (None, None) => {
            ctx.schema(p.clone(), "a script needs `command` or `file`");
            ScriptSource::Command(String::new())
        }
    };
    Script {
        source,
        description: spec.description,
        args: spec
            .args
            .unwrap_or_default()
            .into_iter()
            .map(|a| ScriptArg {
                name: a.name,
                kind: a.kind.unwrap_or(d::ARG_TYPE),
                default: a.default,
                required: a.required.unwrap_or(d::ARG_REQUIRED),
                description: a.description,
                values: a.values.unwrap_or_default(),
            })
            .collect(),
        inputs: spec.inputs.unwrap_or_default(),
        requires: spec.requires.unwrap_or_default(),
        timeout: spec.timeout,
        retries: spec.retries.unwrap_or(d::SCRIPT_RETRIES),
        concurrent: spec.concurrent.unwrap_or(d::SCRIPT_CONCURRENT),
        stamp_env: spec.stamp_env.unwrap_or_default(),
        cwd: spec
            .cwd
            .as_deref()
            .map_or_else(|| default_cwd.to_path_buf(), |c| ctx.path(default_cwd, c)),
    }
}

/// A bare-string script (`seed: scripts/postgres/seed.sh`) is a file when it
/// is a single word (no whitespace) naming an existing file relative to the
/// integration repo; anything else is an inline shell command.
pub(crate) fn names_script_file(s: &str, root: &Path) -> bool {
    let t = s.trim();
    !t.is_empty() && !t.contains(char::is_whitespace) && !t.contains('$') && root.join(t).is_file()
}

/// Fields that only apply to some stem types: (field, is set, allowed types).
fn type_specific_fields(rs: &RawStem) -> Vec<(&'static str, bool, &'static [StemType])> {
    use StemType::*;
    vec![
        ("cwd", rs.cwd.is_some(), &[Process]),
        ("command", rs.command.is_some(), &[Process, Docker]),
        ("shell", rs.shell.is_some(), &[Process]),
        ("stdin", rs.stdin.is_some(), &[Process]),
        ("image", rs.image.is_some(), &[Docker]),
        ("build", rs.build.is_some(), &[Docker]),
        ("volumes", rs.volumes.is_some(), &[Docker]),
        ("entrypoint", rs.entrypoint.is_some(), &[Docker]),
        ("network", rs.network.is_some(), &[Docker]),
        ("labels", !rs.labels.is_empty(), &[Docker]),
        ("healthcheck", rs.healthcheck.is_some(), &[Docker]),
        ("file", rs.file.is_some(), &[Compose]),
        ("service", rs.service.is_some(), &[Compose]),
        ("project_name", rs.project_name.is_some(), &[Compose]),
        ("adopt", rs.adopt.is_some(), &[Compose]),
    ]
}

fn resolve_stem(
    name: &str,
    rs: &RawStem,
    ws_env: &IndexMap<String, String>,
    p: &ConfigPath,
    ctx: &mut Ctx<'_>,
) -> Result<Stem, Box<Diagnostic>> {
    let Some(kind) = rs.kind else {
        return Err(Box::new(
            Diagnostic::new(
                codes::SCHEMA_INVALID,
                format!("stem `{name}` has no `type`"),
            )
            .with_path(p.key("type"))
            .with_hint("set `type:` to one of process, docker, compose, external"),
        ));
    };
    if let Some(n) = &rs.name
        && n != name
    {
        ctx.schema(
            p.key("name"),
            format!("`name: {n}` does not match the stem key `{name}`"),
        );
    }
    for (field, set, allowed) in type_specific_fields(rs) {
        if set && !allowed.contains(&kind) {
            let list: Vec<String> = allowed.iter().map(ToString::to_string).collect();
            ctx.schema(
                p.key(field),
                format!(
                    "field `{field}` is not valid for a {kind} stem (only {})",
                    list.join(", ")
                ),
            );
        }
    }

    let root = ctx.root;
    let codebase = rs
        .codebase
        .as_ref()
        .map(|c| resolve_codebase(c, name, root, &ctx.repos_dir, ctx.home));
    let base_dir = codebase.as_ref().map_or(root, Codebase::path).to_path_buf();

    let mut env = ws_env.clone();
    env.extend(scalars(&rs.env));

    let ports = resolve_ports(
        rs.ports.as_deref(),
        Some(kind),
        &p.key("ports"),
        &mut ctx.diagnostics,
    );

    let depends_on = rs
        .depends_on
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|dep| match dep {
            RawDependency::Name(stem) => Dependency {
                stem,
                condition: d::DEP_CONDITION,
                soft: d::DEP_SOFT,
                protocol: None,
                via: None,
            },
            RawDependency::Spec(s) => Dependency {
                stem: s.stem,
                condition: s.condition.unwrap_or(d::DEP_CONDITION),
                soft: s.soft.unwrap_or(d::DEP_SOFT),
                protocol: s.protocol,
                via: s.via,
            },
        })
        .collect();

    let scripts = rs
        .scripts
        .iter()
        .map(|(k, s)| {
            (
                k.clone(),
                resolve_script(s, &base_dir, &p.key("scripts").key(k), ctx),
            )
        })
        .collect();

    let health = resolve_health(
        name,
        kind,
        rs.health.as_ref(),
        &ports,
        &p.key("health"),
        ctx,
    );

    let watch = rs
        .watch
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|w| Watch {
            paths: w.paths,
            ignore: d::WATCH_IGNORE
                .iter()
                .map(|s| (*s).to_string())
                .chain(w.ignore.unwrap_or_default())
                .collect(),
            debounce: w.debounce.unwrap_or(d::WATCH_DEBOUNCE),
            action: w.action.unwrap_or(d::WATCH_ACTION),
            settle: w.settle.unwrap_or(d::WATCH_SETTLE),
            root: w.root.unwrap_or(d::WATCH_ROOT),
        })
        .collect();

    let r = rs.restart.clone().unwrap_or_default();
    let b = r.backoff.unwrap_or_default();
    let restart = Restart {
        policy: r.policy.unwrap_or(d::RESTART_POLICY),
        max: r.max.unwrap_or(d::RESTART_MAX),
        window: r.window.unwrap_or(d::RESTART_WINDOW),
        backoff: Backoff {
            initial: b.initial.unwrap_or(d::BACKOFF_INITIAL),
            max: b.max.unwrap_or(d::BACKOFF_MAX),
            factor: b.factor.unwrap_or(d::BACKOFF_FACTOR),
        },
        on_unhealthy: r.on_unhealthy.unwrap_or(d::RESTART_ON_UNHEALTHY),
        unhealthy_grace: r.unhealthy_grace.unwrap_or(d::UNHEALTHY_GRACE),
    };

    let limits = rs
        .limits
        .clone()
        .map(|l| Limits {
            memory: l.memory,
            cpu: l.cpu,
        })
        .unwrap_or_default();

    let mut overlays = Vec::new();
    for (i, o) in rs
        .overlays
        .clone()
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        let op = p.key("overlays").index(i);
        let source = match (&o.template, &o.file) {
            (Some(t), None) => OverlaySource::Template(ctx.path(root, t)),
            (None, Some(f)) => OverlaySource::File(ctx.path(root, f)),
            _ => {
                ctx.schema(op, "an overlay needs exactly one of `template` or `file`");
                continue;
            }
        };
        overlays.push(Overlay {
            source,
            dest: PathBuf::from(&o.dest),
            keep: o.keep.unwrap_or(d::OVERLAY_KEEP),
            mode: o.mode.unwrap_or(d::OVERLAY_MODE),
        });
    }

    let runtime = match kind {
        StemType::Process => StemRuntime::Process(ProcessSpec {
            cwd: rs
                .cwd
                .as_deref()
                .map_or_else(|| base_dir.clone(), |c| ctx.path(&base_dir, c)),
            command: rs.command.as_ref().map(|c| c.to_joined()),
            shell: rs.shell.clone().unwrap_or_else(|| d::PROCESS_SHELL.into()),
            stdin: rs.stdin.unwrap_or(d::PROCESS_STDIN),
        }),
        StemType::Docker => StemRuntime::Docker(Box::new(DockerSpec {
            image: rs.image.clone(),
            build: rs.build.as_ref().map(|b| {
                let context = b
                    .context
                    .as_deref()
                    .map_or_else(|| base_dir.clone(), |c| ctx.path(&base_dir, c));
                let dockerfile =
                    ctx.path(&context, b.dockerfile.as_deref().unwrap_or(d::DOCKERFILE));
                DockerBuild {
                    context,
                    dockerfile,
                }
            }),
            volumes: rs.volumes.clone().unwrap_or_default(),
            command: rs.command.as_ref().map(|c| c.to_list()),
            entrypoint: rs.entrypoint.as_ref().map(|c| c.to_list()),
            network: rs.network.clone(),
            labels: rs.labels.clone(),
            healthcheck: rs.healthcheck.as_ref().map(|h| DockerHealthcheck {
                test: h.test.as_ref().map(|t| t.to_list()),
                interval: h.interval,
                timeout: h.timeout,
                retries: h.retries,
                start_period: h.start_period,
                disable: h.disable.unwrap_or(false),
            }),
        })),
        StemType::Compose => StemRuntime::Compose(ComposeSpec {
            file: rs.file.as_deref().map(|f| ctx.path(root, f)),
            service: rs.service.clone().unwrap_or_else(|| name.to_string()),
            project_name: rs
                .project_name
                .clone()
                .unwrap_or_else(|| ctx.ws_name.clone()),
            adopt: rs.adopt.unwrap_or(d::COMPOSE_ADOPT),
        }),
        StemType::External => StemRuntime::External,
    };

    Ok(Stem {
        name: name.to_string(),
        runtime,
        description: rs.description.clone(),
        codebase,
        enabled: rs.enabled.unwrap_or(d::STEM_ENABLED),
        env,
        local_env: Default::default(),
        env_files: rs
            .env_files
            .clone()
            .unwrap_or_default()
            .iter()
            .map(|f| ctx.path(root, f))
            .collect(),
        ports,
        depends_on,
        scripts,
        health,
        watch,
        restart,
        limits,
        outputs: rs.outputs.clone(),
        overlays,
        tags: rs.tags.clone().unwrap_or_default(),
        stop_grace: rs.stop_grace.unwrap_or(d::STOP_GRACE),
    })
}

fn resolve_health(
    stem: &str,
    kind: StemType,
    raw: Option<&RawHealth>,
    ports: &[Port],
    p: &ConfigPath,
    ctx: &mut Ctx<'_>,
) -> Option<Health> {
    let default_kind = d::health_type_for(kind);
    let h = match raw {
        Some(h) => h.clone(),
        None => RawHealth {
            kind: Some(default_kind?),
            ..Default::default()
        },
    };
    let inferred = if h.url.is_some() {
        Some(HealthType::Http)
    } else if h.command.is_some() {
        Some(HealthType::Command)
    } else if h.port.is_some() {
        Some(HealthType::Tcp)
    } else {
        default_kind
    };
    let Some(hkind) = h.kind.or(inferred) else {
        ctx.schema(
            p.clone(),
            "health check needs a `type` (or a `url`, `command` or `port`)",
        );
        return None;
    };
    let port = match h.port {
        Some(RawHealthPort::Number(n)) => Some(HealthPort::Number(n)),
        Some(RawHealthPort::Expr(s)) => match s.trim().parse::<u16>() {
            Ok(n) => Some(HealthPort::Number(n)),
            Err(_) => {
                if !s.contains("${") {
                    ctx.schema(p.key("port"), format!("invalid health port `{s}`"));
                }
                Some(HealthPort::Deferred(s))
            }
        },
        None if matches!(hkind, HealthType::Tcp | HealthType::Grpc) => {
            ports.first().map(|first| match first.port {
                PortRef::Fixed(n) => HealthPort::Number(n),
                PortRef::Auto => {
                    let reference = format!("stem.{stem}.port");
                    ctx.deferred.push(DeferredRef {
                        path: p.key("port"),
                        reference: reference.clone(),
                        kind: DeferredKind::AutoPort,
                    });
                    HealthPort::Deferred(format!("${{{reference}}}"))
                }
            })
        }
        None => None,
    };
    Some(Health {
        kind: hkind,
        host: h.host.unwrap_or_else(|| d::HEALTH_HOST.into()),
        port,
        url: h.url,
        status: h.status,
        body_contains: h.body_contains,
        command: h.command,
        interval: h.interval.unwrap_or(d::HEALTH_INTERVAL),
        timeout: h.timeout.unwrap_or(d::HEALTH_TIMEOUT),
        retries: h.retries.unwrap_or(d::HEALTH_RETRIES),
        start_period: h.start_period.unwrap_or(d::HEALTH_START_PERIOD),
        start_timeout: h.start_timeout.unwrap_or(d::HEALTH_START_TIMEOUT),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_script_names_a_file_only_when_it_exists_and_has_no_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("scripts/postgres")).unwrap();
        std::fs::write(root.join("scripts/postgres/seed.sh"), "#!/bin/sh\n").unwrap();
        assert!(names_script_file("scripts/postgres/seed.sh", root));
        assert!(names_script_file(" scripts/postgres/seed.sh ", root));
        // Missing file, a command with arguments, a directory, a bare command.
        assert!(!names_script_file("scripts/postgres/nope.sh", root));
        assert!(!names_script_file("sh scripts/postgres/seed.sh", root));
        assert!(!names_script_file("scripts/postgres", root));
        assert!(!names_script_file("true", root));
        assert!(!names_script_file("", root));
    }

    #[test]
    fn bare_script_resolves_to_file_or_command() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(root.join("scripts/seed.sh"), "#!/bin/sh\n").unwrap();
        let raw = RawWorkspace::default();
        let mut ctx = Ctx::new(&raw, &root, None);
        let p = ConfigPath::root().key("scripts").key("seed");
        let file = resolve_script(
            &RawScript::Inline("scripts/seed.sh".into()),
            &root,
            &p,
            &mut ctx,
        );
        assert_eq!(
            file.source,
            ScriptSource::File(root.join("scripts/seed.sh"))
        );
        let cmd = resolve_script(
            &RawScript::Inline("scripts/nope.sh".into()),
            &root,
            &p,
            &mut ctx,
        );
        assert_eq!(cmd.source, ScriptSource::Command("scripts/nope.sh".into()));
        let cmd = resolve_script(
            &RawScript::Inline("python3 app.py".into()),
            &root,
            &p,
            &mut ctx,
        );
        assert_eq!(cmd.source, ScriptSource::Command("python3 app.py".into()));
        assert!(ctx.diagnostics.is_empty());
    }
}
