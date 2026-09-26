//! The runtime-facing container spec and its *pure* mapping to the Docker
//! Engine API types (no daemon needed; golden-tested).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use bollard::models::{
    ContainerCreateBody, EndpointSettings, HealthConfig, HostConfig, NetworkingConfig, PortBinding,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

/// Label carrying the workspace name (orphan scan, adoption).
pub const LABEL_WORKSPACE: &str = "stems.workspace";
/// Label carrying the stem name (adoption).
pub const LABEL_STEM: &str = "stems.stem";
/// Label carrying the daemon run id that created the container. Adopted
/// containers keep the run id of the run that created them, so adoption
/// never matches on it.
pub const LABEL_RUN_ID: &str = "stems.run_id";
/// Label carrying [`ContainerSpec::spec_hash`] (restart: recreate or not).
pub const LABEL_SPEC_HASH: &str = "stems.spec_hash";

/// Transport protocol of a published port.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortProto {
    #[default]
    Tcp,
    Udp,
    Sctp,
}

impl std::fmt::Display for PortProto {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PortProto::Tcp => "tcp",
            PortProto::Udp => "udp",
            PortProto::Sctp => "sctp",
        })
    }
}

/// `host:container[/proto]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortMapping {
    /// Host port (already allocated/remapped by the daemon).
    pub host: u16,
    /// Port inside the container.
    pub container: u16,
    #[serde(default)]
    pub proto: PortProto,
}

impl PortMapping {
    /// The Docker port key, e.g. `5432/tcp`.
    pub fn key(&self) -> String {
        format!("{}/{}", self.container, self.proto)
    }
}

/// A volume mount.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VolumeMount {
    /// A named volume; the Docker name is prefixed `<ws>_` by the mapping.
    Named {
        name: String,
        target: String,
        #[serde(default)]
        read_only: bool,
    },
    /// A bind mount of an absolute host path.
    Bind {
        source: PathBuf,
        target: String,
        #[serde(default)]
        read_only: bool,
    },
}

impl VolumeMount {
    /// Parse a config volume string `source:target[:ro|rw]`. A source that
    /// starts with `/`, `.` or `~` is a bind mount (relative paths are
    /// resolved against `base`, `~` against `$HOME`); anything else is a
    /// named volume.
    pub fn parse(spec: &str, base: &Path) -> Result<Self, String> {
        let parts: Vec<&str> = spec.split(':').collect();
        let (source, target, mode) = match parts.as_slice() {
            [s, t] => (*s, *t, None),
            [s, t, m] => (*s, *t, Some(*m)),
            _ => return Err(format!("volume `{spec}`: expected `source:target[:ro|rw]`")),
        };
        if source.is_empty() || !target.starts_with('/') {
            return Err(format!(
                "volume `{spec}`: expected `source:target[:ro|rw]` with an absolute container path"
            ));
        }
        let read_only = match mode {
            None | Some("rw") => false,
            Some("ro") => true,
            Some(m) => return Err(format!("volume `{spec}`: unknown mode `{m}` (ro|rw)")),
        };
        let target = target.to_string();
        if source.starts_with('/') || source.starts_with('.') || source.starts_with('~') {
            let source = if let Some(rest) = source.strip_prefix('~') {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default();
                home.join(rest.trim_start_matches('/'))
            } else if source.starts_with('/') {
                PathBuf::from(source)
            } else {
                normalize(&base.join(source))
            };
            Ok(VolumeMount::Bind {
                source,
                target,
                read_only,
            })
        } else {
            Ok(VolumeMount::Named {
                name: source.to_string(),
                target,
                read_only,
            })
        }
    }
}

/// Lexically resolve `.` and `..` (no filesystem access).
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// `build: { context, dockerfile }` (both absolute).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildSpec {
    pub context: PathBuf,
    pub dockerfile: PathBuf,
}

/// Health-check override; unset fields keep the image's values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthcheckSpec {
    pub test: Option<Vec<String>>,
    pub interval: Option<Duration>,
    pub timeout: Option<Duration>,
    pub retries: Option<u32>,
    pub start_period: Option<Duration>,
}

/// One coalesced pull progress step (one per layer status change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullProgress {
    pub image: String,
    /// Layer id; empty for image-level messages ("Pulling from ...", "Digest: ...").
    pub layer: String,
    pub status: String,
}

/// Image preparation progress (`docker.pull` / `docker.build` events).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ImageProgress {
    Pull(PullProgress),
    Build { stem: String, line: String },
}

/// Where image progress goes. Ignored by equality, hashing and serde.
#[derive(Debug, Clone, Default)]
pub struct ProgressSink(pub Option<mpsc::Sender<ImageProgress>>);

impl ProgressSink {
    /// Best effort: a slow or absent receiver never blocks a pull.
    pub fn send(&self, p: ImageProgress) {
        if let Some(tx) = &self.0 {
            let _ = tx.try_send(p);
        }
    }
}

impl PartialEq for ProgressSink {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
impl Eq for ProgressSink {}

/// Everything the Docker runtime needs to create a stem's container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerSpec {
    pub workspace: String,
    pub stem: String,
    /// Run id of the daemon creating the container (label + build tag).
    pub run_id: String,
    /// Image reference (absent when `build` is set).
    pub image: Option<String>,
    pub build: Option<BuildSpec>,
    #[serde(default)]
    pub ports: Vec<PortMapping>,
    #[serde(default)]
    pub volumes: Vec<VolumeMount>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// CMD override.
    pub command: Option<Vec<String>>,
    pub entrypoint: Option<Vec<String>>,
    /// Network override; default `<ws>_net`.
    pub network: Option<String>,
    /// User labels (stems' own labels are added on top and win).
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub healthcheck: Option<HealthcheckSpec>,
    /// Docker stop timeout (rounded up to whole seconds).
    pub stop_grace: Duration,
    #[serde(skip)]
    pub progress: ProgressSink,
}

impl ContainerSpec {
    /// A spec with no image yet, default network and a 10 s stop grace.
    pub fn new(
        workspace: impl Into<String>,
        stem: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            workspace: workspace.into(),
            stem: stem.into(),
            run_id: run_id.into(),
            image: None,
            build: None,
            ports: Vec::new(),
            volumes: Vec::new(),
            env: BTreeMap::new(),
            command: None,
            entrypoint: None,
            network: None,
            labels: BTreeMap::new(),
            healthcheck: None,
            stop_grace: Duration::from_secs(10),
            progress: ProgressSink::default(),
        }
    }

    /// `<ws>-<stem>`.
    pub fn container_name(&self) -> String {
        container_name(&self.workspace, &self.stem)
    }

    /// The network override, or `<ws>_net`.
    pub fn network_name(&self) -> String {
        self.network
            .clone()
            .unwrap_or_else(|| default_network(&self.workspace))
    }

    /// The Docker name of a named volume: `<ws>_<name>` (not doubled).
    pub fn volume_name(&self, name: &str) -> String {
        volume_name(&self.workspace, name)
    }

    /// `stems/<ws>/<stem>:<run_id>` when the image is built.
    pub fn build_tag(&self) -> Option<String> {
        self.build.as_ref().map(|_| {
            format!(
                "stems/{}/{}:{}",
                self.workspace.to_lowercase(),
                self.stem.to_lowercase(),
                self.run_id
            )
        })
    }

    /// The image the container runs: the build tag, else `image`.
    pub fn image_ref(&self) -> Option<String> {
        self.build_tag().or_else(|| self.image.clone())
    }

    /// Stable hash of everything that requires recreating the container
    /// when it changes (image/build, ports, volumes, env, command,
    /// entrypoint, network, user labels, healthcheck). Excludes the run id
    /// (adopted containers keep theirs), `stop_grace` and the progress sink.
    pub fn spec_hash(&self) -> String {
        #[derive(Serialize)]
        struct Hashed<'a> {
            workspace: &'a str,
            stem: &'a str,
            image: &'a Option<String>,
            build: &'a Option<BuildSpec>,
            ports: &'a [PortMapping],
            volumes: &'a [VolumeMount],
            env: &'a BTreeMap<String, String>,
            command: &'a Option<Vec<String>>,
            entrypoint: &'a Option<Vec<String>>,
            network: String,
            labels: &'a BTreeMap<String, String>,
            healthcheck: &'a Option<HealthcheckSpec>,
        }
        let h = Hashed {
            workspace: &self.workspace,
            stem: &self.stem,
            image: &self.image,
            build: &self.build,
            ports: &self.ports,
            volumes: &self.volumes,
            env: &self.env,
            command: &self.command,
            entrypoint: &self.entrypoint,
            network: self.network_name(),
            labels: &self.labels,
            healthcheck: &self.healthcheck,
        };
        // Field order is fixed by the struct and maps are BTreeMaps, so the
        // JSON text is canonical.
        let json = serde_json::to_vec(&h).expect("spec serialises");
        hex(&Sha256::digest(&json)[..16])
    }

    /// User labels plus stems' own (which win).
    pub fn all_labels(&self) -> BTreeMap<String, String> {
        let mut l = self.labels.clone();
        l.insert(LABEL_WORKSPACE.into(), self.workspace.clone());
        l.insert(LABEL_STEM.into(), self.stem.clone());
        l.insert(LABEL_RUN_ID.into(), self.run_id.clone());
        l.insert(LABEL_SPEC_HASH.into(), self.spec_hash());
        l
    }
}

/// `<ws>-<stem>`.
pub fn container_name(workspace: &str, stem: &str) -> String {
    format!("{workspace}-{stem}")
}

/// `<ws>_net`.
pub fn default_network(workspace: &str) -> String {
    format!("{workspace}_net")
}

/// `<ws>_<name>`. A leading `<ws>_` or `<ws>-` in `name` is dropped first,
/// so the prefix is never doubled: `hello-shop-pgdata` and
/// `hello-shop_pgdata` both become `hello-shop_pgdata`, like `pgdata`.
pub fn volume_name(workspace: &str, name: &str) -> String {
    let bare = name
        .strip_prefix(&format!("{workspace}_"))
        .or_else(|| name.strip_prefix(&format!("{workspace}-")))
        .filter(|b| !b.is_empty())
        .unwrap_or(name);
    format!("{workspace}_{bare}")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Docker's stop `t`: whole seconds, rounded up.
pub fn grace_secs(grace: Duration) -> i64 {
    grace.as_millis().div_ceil(1000) as i64
}

fn nanos(d: Duration) -> i64 {
    i64::try_from(d.as_nanos()).unwrap_or(i64::MAX)
}

/// Pure mapping of a spec to the create-container body, its host config and
/// its networking config (the body returned has `host_config` and
/// `networking_config` unset; [`create_body`] combines them).
pub fn to_bollard(spec: &ContainerSpec) -> (ContainerCreateBody, HostConfig, NetworkingConfig) {
    let network = spec.network_name();

    let mut exposed = Vec::new();
    let mut bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
    for p in &spec.ports {
        let key = p.key();
        if !exposed.contains(&key) {
            exposed.push(key.clone());
        }
        bindings
            .entry(key)
            .or_insert_with(|| Some(Vec::new()))
            .get_or_insert_with(Vec::new)
            .push(PortBinding {
                host_ip: None,
                host_port: Some(p.host.to_string()),
            });
    }

    let binds: Vec<String> = spec
        .volumes
        .iter()
        .map(|v| match v {
            VolumeMount::Named {
                name,
                target,
                read_only,
            } => bind_string(&spec.volume_name(name), target, *read_only),
            VolumeMount::Bind {
                source,
                target,
                read_only,
            } => bind_string(&source.to_string_lossy(), target, *read_only),
        })
        .collect();

    let healthcheck = spec.healthcheck.as_ref().map(|h| HealthConfig {
        test: h.test.clone(),
        interval: h.interval.map(nanos),
        timeout: h.timeout.map(nanos),
        retries: h.retries.map(i64::from),
        start_period: h.start_period.map(nanos),
        start_interval: None,
    });

    let config = ContainerCreateBody {
        image: spec.image_ref(),
        env: Some(spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect()),
        cmd: spec.command.clone(),
        entrypoint: spec.entrypoint.clone(),
        labels: Some(spec.all_labels().into_iter().collect()),
        exposed_ports: (!exposed.is_empty()).then_some(exposed),
        healthcheck,
        stop_timeout: Some(grace_secs(spec.stop_grace)),
        ..Default::default()
    };

    let host = HostConfig {
        auto_remove: Some(false),
        binds: (!binds.is_empty()).then_some(binds),
        port_bindings: (!bindings.is_empty()).then_some(bindings),
        network_mode: Some(network.clone()),
        ..Default::default()
    };

    let mut endpoints = HashMap::new();
    endpoints.insert(
        network,
        EndpointSettings {
            aliases: Some(vec![spec.stem.clone()]),
            ..Default::default()
        },
    );
    let net = NetworkingConfig {
        endpoints_config: Some(endpoints),
    };
    (config, host, net)
}

/// The full create-container body.
pub fn create_body(spec: &ContainerSpec) -> ContainerCreateBody {
    let (mut config, host, net) = to_bollard(spec);
    config.host_config = Some(host);
    config.networking_config = Some(net);
    config
}

fn bind_string(source: &str, target: &str, read_only: bool) -> String {
    if read_only {
        format!("{source}:{target}:ro")
    } else {
        format!("{source}:{target}")
    }
}

/// Split an image reference into `(repository, tag-or-digest)` for a pull.
/// A missing tag means `latest`; a `:` before the last `/` is a registry port.
pub fn split_image_ref(image: &str) -> (String, String) {
    if let Some((repo, digest)) = image.split_once('@') {
        return (repo.to_string(), digest.to_string());
    }
    let last_slash = image.rfind('/').map_or(0, |i| i + 1);
    match image[last_slash..].rfind(':') {
        Some(i) => (
            image[..last_slash + i].to_string(),
            image[last_slash + i + 1..].to_string(),
        ),
        None => (image.to_string(), "latest".to_string()),
    }
}

/// Coalesces Docker's chatty pull stream into one [`PullProgress`] per
/// layer status change (byte-count updates of the same status are dropped).
#[derive(Debug, Default)]
pub struct PullCoalescer {
    last: HashMap<String, String>,
}

impl PullCoalescer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one stream item; returns an event when the layer's status changed.
    pub fn observe(
        &mut self,
        image: &str,
        layer: Option<&str>,
        status: Option<&str>,
    ) -> Option<PullProgress> {
        let status = status?.trim();
        if status.is_empty() {
            return None;
        }
        let layer = layer.unwrap_or("").to_string();
        if self.last.get(&layer).map(String::as_str) == Some(status) {
            return None;
        }
        self.last.insert(layer.clone(), status.to_string());
        Some(PullProgress {
            image: image.to_string(),
            layer,
            status: status.to_string(),
        })
    }
}
