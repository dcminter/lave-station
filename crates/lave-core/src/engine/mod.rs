//! The container daemon, expressed as domain types this application controls.
//!
//! Nothing above this module sees a `bollard` type, so the HTTP client is replaceable
//! and every consumer is testable against [`fake::FakeEngine`].

pub mod bollard_engine;
mod convert;
mod error;
pub mod fake;

use std::collections::BTreeMap;

use async_trait::async_trait;
use futures_util::stream::BoxStream;

pub use error::EngineError;

/// An image on the local device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageSummary {
    pub id: String,
    pub repo_tags: Vec<String>,
    pub repo_digests: Vec<String>,
    /// Seconds since the Unix epoch.
    pub created: i64,
    pub size: i64,
    pub shared_size: i64,
    pub containers: i64,
    pub labels: BTreeMap<String, String>,
}

/// A container on the local device, running or not.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContainerSummary {
    pub id: String,
    /// Names with Docker's leading slash removed.
    pub names: Vec<String>,
    pub image: String,
    pub image_id: String,
    pub command: String,
    /// Seconds since the Unix epoch.
    pub created: i64,
    pub state: ContainerState,
    pub status: String,
    pub ports: Vec<PortMapping>,
    pub mounts: Vec<MountSummary>,
    pub networks: Vec<String>,
    pub labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ContainerState {
    Created,
    Running,
    Paused,
    Restarting,
    Exited,
    Removing,
    Dead,
    Stopping,
    #[default]
    Unknown,
}

impl ContainerState {
    /// True for states in which the container is executing.
    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            ContainerState::Running | ContainerState::Restarting | ContainerState::Paused
        )
    }

    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            ContainerState::Created => "created",
            ContainerState::Running => "running",
            ContainerState::Paused => "paused",
            ContainerState::Restarting => "restarting",
            ContainerState::Exited => "exited",
            ContainerState::Removing => "removing",
            ContainerState::Dead => "dead",
            ContainerState::Stopping => "stopping",
            ContainerState::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortMapping {
    pub ip: Option<String>,
    pub private_port: u16,
    pub public_port: Option<u16>,
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MountSummary {
    pub kind: String,
    pub source: String,
    pub destination: String,
    pub read_write: bool,
}

/// The startup capability probe of `docs/container_daemon_integration.md` §8.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvironmentSummary {
    pub name: String,
    pub server_version: String,
    pub api_version: String,
    pub min_api_version: Option<String>,
    pub os_type: String,
    pub architecture: String,
    pub operating_system: String,
    pub kernel_version: String,
    pub storage_driver: String,
    pub logging_driver: String,
    pub cgroup_version: String,
    pub cgroup_driver: String,
    pub rootless: bool,
    pub cpus: i64,
    pub memory_total: i64,
    pub docker_root_dir: String,
    pub containers_total: i64,
    pub containers_running: i64,
    pub containers_paused: i64,
    pub containers_stopped: i64,
    pub images: i64,
    pub security_options: Vec<String>,
    /// Warnings from the daemon, plus any this application adds.
    pub warnings: Vec<String>,
}

/// One message from the daemon's event stream.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EngineEvent {
    /// Object kind: `container`, `image`, `network`, and so on.
    pub kind: String,
    pub action: String,
    pub actor_id: String,
    pub actor_name: Option<String>,
    /// Seconds since the Unix epoch.
    pub time: i64,
}

impl EngineEvent {
    /// Whether this event invalidates the cached listings.
    #[must_use]
    pub fn affects_listing(&self) -> bool {
        matches!(self.kind.as_str(), "container" | "image")
    }
}

/// Everything iteration 1 asks of a container daemon. Read-only by design.
#[async_trait]
pub trait ContainerEngine: Send + Sync {
    async fn probe(&self) -> Result<EnvironmentSummary, EngineError>;

    async fn list_images(&self) -> Result<Vec<ImageSummary>, EngineError>;

    async fn list_containers(&self) -> Result<Vec<ContainerSummary>, EngineError>;

    /// Raw inspect output, rendered verbatim in the detail pane.
    async fn inspect_image(&self, id: &str) -> Result<serde_json::Value, EngineError>;

    async fn inspect_container(&self, id: &str) -> Result<serde_json::Value, EngineError>;

    /// Never-ending event stream. `since` replays the gap after a reconnect.
    fn events(&self, since: Option<i64>) -> BoxStream<'_, Result<EngineEvent, EngineError>>;
}
