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

/// A lifecycle transition. Every one of these leaves the container recoverable, which
/// is why `model::action` offers them without a confirmation dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Kill,
}

impl Lifecycle {
    #[must_use]
    pub fn verb(self) -> &'static str {
        match self {
            Lifecycle::Start => "start",
            Lifecycle::Stop => "stop",
            Lifecycle::Restart => "restart",
            Lifecycle::Pause => "pause",
            Lifecycle::Unpause => "unpause",
            Lifecycle::Kill => "kill",
        }
    }
}

/// What a prune actually removed. The daemon reports this only after the fact; the
/// preview shown beforehand is computed in `model::action` from the current listings.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PruneOutcome {
    pub removed: Vec<String>,
    /// Bytes the daemon says it reclaimed.
    pub reclaimed: i64,
}

/// One record from an image's build history, newest first as the daemon reports it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistoryEntry {
    /// `<missing>` for every layer but the top on a modern daemon: these are history
    /// records, not images. See `docs/iteration_2_plan.md` §2.
    pub id: String,
    /// Seconds since the Unix epoch.
    pub created: i64,
    /// The build instruction as recorded, which is what a Dockerfile is reconstructed
    /// from.
    pub created_by: String,
    pub size: i64,
    pub comment: String,
    pub tags: Vec<String>,
}

impl HistoryEntry {
    /// True when the daemon has no image for this record, which is the normal case.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        self.id.is_empty() || self.id == "<missing>"
    }
}

/// One path in a container's filesystem, as reported by the archive endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathStat {
    pub name: String,
    pub size: i64,
    /// Go's `os.FileMode`, not a raw Unix mode: the type bits live above bit 15.
    pub mode: u32,
    /// Seconds since the Unix epoch.
    pub mtime: i64,
    /// Empty unless this is a symlink.
    pub link_target: String,
}

/// Which of a container's streams a log chunk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// A chunk of log output, already demultiplexed.
///
/// The 8-byte framing of `docs/container_daemon_integration.md` §5 is unpicked by
/// `bollard`, which also collapses to a single stream when the container has a TTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogChunk {
    pub stream: LogStream,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogOptions {
    /// Keep the stream open and deliver new output as it arrives.
    pub follow: bool,
    /// Start from the last `tail` lines; `None` means everything.
    pub tail: Option<usize>,
    pub timestamps: bool,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            follow: false,
            // Enough to be useful, few enough that a chatty container does not stall the
            // window while it loads.
            tail: Some(2000),
            timestamps: false,
        }
    }
}

/// Everything the application asks of a container daemon.
///
/// **This is no longer read-only.** Versions 1 and 2 only observed; version 3 starts,
/// stops, removes and prunes. Which of those a user is offered, and what they are warned
/// about first, is decided in `model::action` — not here, and not in the widget layer.
#[async_trait]
pub trait ContainerEngine: Send + Sync {
    async fn probe(&self) -> Result<EnvironmentSummary, EngineError>;

    async fn list_images(&self) -> Result<Vec<ImageSummary>, EngineError>;

    async fn list_containers(&self) -> Result<Vec<ContainerSummary>, EngineError>;

    /// Raw inspect output, rendered verbatim in the detail pane.
    async fn inspect_image(&self, id: &str) -> Result<serde_json::Value, EngineError>;

    /// An image's layer digests, base first. Not carried by the image listing, so this
    /// costs one inspect per image; the caller is expected to cache the result.
    async fn image_layers(&self, id: &str) -> Result<Vec<String>, EngineError>;

    async fn inspect_container(&self, id: &str) -> Result<serde_json::Value, EngineError>;

    /// Never-ending event stream. `since` replays the gap after a reconnect.
    fn events(&self, since: Option<i64>) -> BoxStream<'_, Result<EngineEvent, EngineError>>;

    /// A reversible state change. The daemon treats a no-op — starting something already
    /// running — as success, and so do we.
    async fn lifecycle(&self, id: &str, action: Lifecycle) -> Result<(), EngineError>;

    /// `force` kills a running container first. Never set implicitly: see
    /// `docs/iteration_3_plan.md` §4.
    async fn remove_container(&self, id: &str, force: bool) -> Result<(), EngineError>;

    async fn remove_image(&self, id: &str, force: bool) -> Result<(), EngineError>;

    /// Removes every stopped container. There is no dry run.
    async fn prune_containers(&self) -> Result<PruneOutcome, EngineError>;

    /// Removes dangling images only — never one that still carries a tag.
    async fn prune_images(&self) -> Result<PruneOutcome, EngineError>;

    fn logs(&self, id: &str, options: LogOptions) -> BoxStream<'_, Result<LogChunk, EngineError>>;

    /// A tar of `path` **and everything beneath it**: the endpoint has no non-recursive
    /// mode, which is the constraint `model::fs_tree` is built around.
    fn archive(&self, id: &str, path: &str) -> BoxStream<'_, Result<Vec<u8>, EngineError>>;

    /// One path's metadata, without transferring its contents.
    async fn stat_path(&self, id: &str, path: &str) -> Result<PathStat, EngineError>;

    async fn image_history(&self, id: &str) -> Result<Vec<HistoryEntry>, EngineError>;

    /// Create a container from an image and do **not** start it, so the image's
    /// filesystem can be read through the container archive endpoint. Returns its ID.
    /// Carries [`SCRATCH_LABEL`] so strays can be swept.
    async fn create_scratch_container(&self, image_id: &str) -> Result<String, EngineError>;
}

/// Marks the never-started containers created solely to read an image's filesystem.
/// Swept at startup so a crash cannot leak them into the user's `docker ps`.
pub const SCRATCH_LABEL: &str = "com.paperstack.lave-station.scratch";

/// Whether this container is one of ours, created only to read an image's filesystem.
///
/// The label is the whole test. Nothing is inferred from the name, which the user is
/// free to change, and nothing from the image, which is theirs.
#[must_use]
pub fn is_scratch(container: &ContainerSummary) -> bool {
    container.labels.contains_key(SCRATCH_LABEL)
}

/// Scratch containers left behind by a previous run.
///
/// Anything carrying our label is by definition abandoned: a live one is removed as soon
/// as browsing ends, so finding one at startup means the last run did not get to.
#[must_use]
pub fn scratch_strays(containers: &[ContainerSummary]) -> Vec<&ContainerSummary> {
    containers.iter().filter(|c| is_scratch(c)).collect()
}
