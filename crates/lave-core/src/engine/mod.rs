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

/// One container's resource use at a moment, as `/containers/{id}/stats` reports it.
///
/// Only the memory figures so far: they are what a developer glances at, and they are
/// the ones a single sample can answer. CPU needs two samples to mean anything.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContainerStats {
    pub id: String,
    /// Bytes in use with the page cache taken out, which is the figure `docker stats`
    /// prints. Negative when the daemon did not report one.
    pub memory_usage: i64,
    /// The cgroup limit, or the host's memory when the container is unconstrained.
    /// Negative when the daemon did not report one.
    pub memory_limit: i64,
}

impl ContainerStats {
    /// Whether the daemon actually reported a usage figure. A container that has just
    /// stopped answers with an empty sample rather than an error.
    #[must_use]
    pub fn has_memory(&self) -> bool {
        self.memory_usage >= 0
    }

    /// Usage as a fraction of the limit, or `None` when either is unknown. Left as a
    /// fraction so the caller decides how to render it.
    #[must_use]
    pub fn memory_fraction(&self) -> Option<f64> {
        if !self.has_memory() || self.memory_limit <= 0 {
            return None;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "the ratio is shown to one decimal place"
        )]
        Some(self.memory_usage as f64 / self.memory_limit as f64)
    }
}

/// One category's disk footprint, as `/system/df` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiskCategory {
    pub total_count: i64,
    pub active_count: i64,
    /// Bytes actually on disk, deduplicated: a layer shared by two images is counted
    /// once, which is what makes this smaller than the sum of the listing's sizes.
    pub size: i64,
    /// Bytes a prune of this category would give back.
    pub reclaimable: i64,
}

/// What the daemon says its storage is spent on.
///
/// Each category is `None` when the daemon did not break it down: the itemised form of
/// `/system/df` arrived in API 1.49, and an older daemon simply leaves it out rather
/// than failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiskUsage {
    pub images: Option<DiskCategory>,
    pub containers: Option<DiskCategory>,
    pub volumes: Option<DiskCategory>,
    pub build_cache: Option<DiskCategory>,
}

impl DiskUsage {
    /// Everything the daemon did account for, added up. `None` when it accounted for
    /// nothing, so a page can leave the row out rather than claiming zero.
    #[must_use]
    pub fn total_size(&self) -> Option<i64> {
        self.sum(|category| category.size)
    }

    /// What a prune of everything would give back, across the categories reported.
    #[must_use]
    pub fn total_reclaimable(&self) -> Option<i64> {
        self.sum(|category| category.reclaimable)
    }

    fn sum(&self, field: impl Fn(&DiskCategory) -> i64) -> Option<i64> {
        let categories = [self.images, self.containers, self.volumes, self.build_cache];
        let reported: Vec<i64> = categories.iter().flatten().map(&field).collect();
        (!reported.is_empty()).then(|| reported.into_iter().sum())
    }
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

/// How much history the tail view asks for: enough to see what just happened, few
/// enough that attaching to a chatty container is instant. The whole log is a
/// deliberate choice the user makes in the viewer, not the default.
pub const TAIL_LINES: usize = 500;

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
            tail: Some(TAIL_LINES),
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

    /// One sample of a container's resource use. A single sample, not a subscription:
    /// the memory figure is meaningful on its own, and holding a stream open per
    /// container would cost the daemon a goroutine each.
    async fn container_stats(&self, id: &str) -> Result<ContainerStats, EngineError>;

    /// What the daemon's storage is spent on. Deduplicated across layers, which is why
    /// this and not the sum of the image listing is the disk footprint.
    async fn disk_usage(&self) -> Result<DiskUsage, EngineError>;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn category(size: i64, reclaimable: i64) -> DiskCategory {
        DiskCategory {
            total_count: 1,
            active_count: 1,
            size,
            reclaimable,
        }
    }

    #[test]
    fn a_measured_sample_reports_its_share_of_the_limit() {
        let stats = ContainerStats {
            id: "web".to_owned(),
            memory_usage: 2_000_000_000,
            memory_limit: 8_000_000_000,
        };

        assert!(stats.has_memory());
        assert_eq!(stats.memory_fraction(), Some(0.25));
    }

    #[test]
    fn an_unmeasured_sample_has_no_share_to_report() {
        let unmeasured = ContainerStats {
            id: "web".to_owned(),
            memory_usage: -1,
            memory_limit: -1,
        };
        assert!(!unmeasured.has_memory());
        assert_eq!(unmeasured.memory_fraction(), None);

        // Unconstrained containers on some daemons report usage but no limit.
        let unlimited = ContainerStats {
            memory_usage: 2_000,
            memory_limit: 0,
            ..unmeasured
        };
        assert!(unlimited.has_memory());
        assert_eq!(unlimited.memory_fraction(), None);
    }

    #[test]
    fn disk_usage_adds_up_the_categories_the_daemon_broke_down() {
        let usage = DiskUsage {
            images: Some(category(1_000, 400)),
            containers: Some(category(30, 30)),
            volumes: None,
            build_cache: Some(category(500, 500)),
        };

        assert_eq!(usage.total_size(), Some(1_530));
        assert_eq!(usage.total_reclaimable(), Some(930));
    }

    #[test]
    fn a_daemon_that_broke_nothing_down_yields_no_total_rather_than_zero() {
        let usage = DiskUsage::default();

        assert_eq!(
            usage.total_size(),
            None,
            "zero would be a claim we cannot make"
        );
        assert_eq!(usage.total_reclaimable(), None);
    }
}
