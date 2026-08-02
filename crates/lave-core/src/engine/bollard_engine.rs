//! The real client: HTTP/JSON over a Unix socket, via `bollard`.
//!
//! Deliberately thin. Everything interesting happens in `convert` or above.

use std::path::{Path, PathBuf};

use std::collections::HashMap;

use async_trait::async_trait;
use bollard::models::ContainerCreateBody;
use bollard::query_parameters::{
    ContainerArchiveInfoOptions, CreateContainerOptions, DownloadFromContainerOptions,
    EventsOptions, ListContainersOptions, ListImagesOptions, LogsOptions, PruneImagesOptions,
    RemoveContainerOptions, RemoveImageOptions, StopContainerOptions,
};
use bollard::{API_DEFAULT_VERSION, Docker};
use futures_util::stream::{BoxStream, StreamExt};

use super::convert::environment_summary;
use super::{
    ContainerEngine, ContainerSummary, EngineError, EngineEvent, EnvironmentSummary, HistoryEntry,
    ImageSummary, Lifecycle, LogChunk, LogOptions, LogStream, PathStat, PruneOutcome,
    SCRATCH_LABEL,
};

/// Long enough to ride out a busy daemon, short enough to fail visibly.
const CONNECT_TIMEOUT_SECONDS: u64 = 20;

pub struct BollardEngine {
    docker: Docker,
    endpoint: PathBuf,
}

impl BollardEngine {
    /// Connect and negotiate the API version. The version is never hardcoded:
    /// `bollard` reads `API-Version` from `/_ping` and uses the lower of the two.
    ///
    /// # Errors
    ///
    /// If the socket is absent or unreadable, or the daemon does not answer the ping.
    pub async fn connect(endpoint: &Path) -> Result<Self, EngineError> {
        let path = endpoint.to_string_lossy();
        let docker = Docker::connect_with_unix(&path, CONNECT_TIMEOUT_SECONDS, API_DEFAULT_VERSION)
            .map_err(|error| EngineError::from_bollard(&error, endpoint))?;

        let docker = docker
            .negotiate_version()
            .await
            .map_err(|error| EngineError::from_bollard(&error, endpoint))?;

        Ok(Self {
            docker,
            endpoint: endpoint.to_path_buf(),
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    fn translate(&self, error: &bollard::errors::Error) -> EngineError {
        EngineError::from_bollard(error, &self.endpoint)
    }
}

#[async_trait]
impl ContainerEngine for BollardEngine {
    async fn probe(&self) -> Result<EnvironmentSummary, EngineError> {
        let version = self
            .docker
            .version()
            .await
            .map_err(|error| self.translate(&error))?;
        let info = self
            .docker
            .info()
            .await
            .map_err(|error| self.translate(&error))?;

        Ok(environment_summary(version, info))
    }

    async fn list_images(&self) -> Result<Vec<ImageSummary>, EngineError> {
        let options = ListImagesOptions {
            all: false,
            ..ListImagesOptions::default()
        };

        Ok(self
            .docker
            .list_images(Some(options))
            .await
            .map_err(|error| self.translate(&error))?
            .into_iter()
            .map(ImageSummary::from)
            .collect())
    }

    async fn list_containers(&self) -> Result<Vec<ContainerSummary>, EngineError> {
        // The README asks for stopped containers as well as running ones.
        let options = ListContainersOptions {
            all: true,
            ..ListContainersOptions::default()
        };

        Ok(self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(|error| self.translate(&error))?
            .into_iter()
            .map(ContainerSummary::from)
            .collect())
    }

    async fn inspect_image(&self, id: &str) -> Result<serde_json::Value, EngineError> {
        let inspect = self
            .docker
            .inspect_image(id)
            .await
            .map_err(|error| self.translate(&error))?;

        serde_json::to_value(inspect).map_err(|error| EngineError::Protocol(error.to_string()))
    }

    async fn image_layers(&self, id: &str) -> Result<Vec<String>, EngineError> {
        let inspect = self
            .docker
            .inspect_image(id)
            .await
            .map_err(|error| self.translate(&error))?;

        // An image with no root filesystem is legal, if unusual; report it as empty
        // rather than as a failure.
        Ok(inspect
            .root_fs
            .and_then(|root| root.layers)
            .unwrap_or_default())
    }

    async fn inspect_container(&self, id: &str) -> Result<serde_json::Value, EngineError> {
        let inspect = self
            .docker
            .inspect_container(id, None)
            .await
            .map_err(|error| self.translate(&error))?;

        serde_json::to_value(inspect).map_err(|error| EngineError::Protocol(error.to_string()))
    }

    fn events(&self, since: Option<i64>) -> BoxStream<'_, Result<EngineEvent, EngineError>> {
        let options = EventsOptions {
            since: since.map(|seconds| seconds.to_string()),
            ..EventsOptions::default()
        };

        self.docker
            .events(Some(options))
            .map(move |message| match message {
                Ok(message) => Ok(EngineEvent::from(message)),
                Err(error) => Err(self.translate(&error)),
            })
            .boxed()
    }

    async fn lifecycle(&self, id: &str, action: Lifecycle) -> Result<(), EngineError> {
        let result = match action {
            Lifecycle::Start => self.docker.start_container(id, None).await,
            // The daemon's own default grace period; overriding it is not ours to do.
            Lifecycle::Stop => {
                self.docker
                    .stop_container(id, None::<StopContainerOptions>)
                    .await
            }
            Lifecycle::Restart => self.docker.restart_container(id, None).await,
            Lifecycle::Pause => self.docker.pause_container(id).await,
            Lifecycle::Unpause => self.docker.unpause_container(id).await,
            Lifecycle::Kill => self.docker.kill_container(id, None).await,
        };

        result.map_err(|error| self.translate(&error))
    }

    async fn remove_container(&self, id: &str, force: bool) -> Result<(), EngineError> {
        let options = RemoveContainerOptions {
            force,
            ..RemoveContainerOptions::default()
        };

        self.docker
            .remove_container(id, Some(options))
            .await
            .map_err(|error| self.translate(&error))
    }

    async fn remove_image(&self, id: &str, force: bool) -> Result<(), EngineError> {
        let options = RemoveImageOptions {
            force,
            ..RemoveImageOptions::default()
        };

        self.docker
            .remove_image(id, Some(options), None)
            .await
            .map(|_| ())
            .map_err(|error| self.translate(&error))
    }

    async fn prune_containers(&self) -> Result<PruneOutcome, EngineError> {
        let response = self
            .docker
            .prune_containers(None)
            .await
            .map_err(|error| self.translate(&error))?;

        Ok(PruneOutcome {
            removed: response.containers_deleted.unwrap_or_default(),
            reclaimed: response.space_reclaimed.unwrap_or_default(),
        })
    }

    async fn prune_images(&self) -> Result<PruneOutcome, EngineError> {
        // Without this filter the daemon removes every image no container is using,
        // tagged or not, which is emphatically not what "prune" is offered as here.
        let mut filters = HashMap::new();
        filters.insert("dangling".to_owned(), vec!["true".to_owned()]);
        let options = PruneImagesOptions {
            filters: Some(filters),
        };

        let response = self
            .docker
            .prune_images(Some(options))
            .await
            .map_err(|error| self.translate(&error))?;

        let removed = response
            .images_deleted
            .unwrap_or_default()
            .into_iter()
            .filter_map(|item| item.deleted.or(item.untagged))
            .collect();

        Ok(PruneOutcome {
            removed,
            reclaimed: response.space_reclaimed.unwrap_or_default(),
        })
    }

    fn logs(&self, id: &str, options: LogOptions) -> BoxStream<'_, Result<LogChunk, EngineError>> {
        let query = LogsOptions {
            follow: options.follow,
            stdout: true,
            stderr: true,
            timestamps: options.timestamps,
            tail: options
                .tail
                .map_or_else(|| "all".to_owned(), |lines| lines.to_string()),
            ..LogsOptions::default()
        };

        self.docker
            .logs(id, Some(query))
            .map(move |message| match message {
                Ok(output) => Ok(LogChunk::from(output)),
                Err(error) => Err(self.translate(&error)),
            })
            .boxed()
    }

    fn archive(&self, id: &str, path: &str) -> BoxStream<'_, Result<Vec<u8>, EngineError>> {
        let options = DownloadFromContainerOptions {
            path: path.to_owned(),
        };

        self.docker
            .download_from_container(id, Some(options))
            .map(move |chunk| match chunk {
                Ok(bytes) => Ok(bytes.to_vec()),
                Err(error) => Err(self.translate(&error)),
            })
            .boxed()
    }

    async fn stat_path(&self, id: &str, path: &str) -> Result<PathStat, EngineError> {
        let options = ContainerArchiveInfoOptions {
            path: path.to_owned(),
        };

        let stat = self
            .docker
            .get_container_archive_info(id, Some(options))
            .await
            .map_err(|error| self.translate(&error))?;

        Ok(PathStat {
            name: stat.name,
            size: stat.size,
            mode: stat.file_mode,
            mtime: super::convert::epoch_seconds(stat.modification_time.as_deref()),
            link_target: stat.link_target,
        })
    }

    async fn image_history(&self, id: &str) -> Result<Vec<HistoryEntry>, EngineError> {
        Ok(self
            .docker
            .image_history(id)
            .await
            .map_err(|error| self.translate(&error))?
            .into_iter()
            .map(HistoryEntry::from)
            .collect())
    }

    async fn create_scratch_container(&self, image_id: &str) -> Result<String, EngineError> {
        let mut labels = HashMap::new();
        labels.insert(SCRATCH_LABEL.to_owned(), "1".to_owned());

        let config = ContainerCreateBody {
            image: Some(image_id.to_owned()),
            labels: Some(labels),
            // Overridden so an image with a failing entrypoint cannot be made to run by
            // accident. It is never started regardless; this is belt and braces.
            entrypoint: Some(vec!["/nonexistent-lave-station-placeholder".to_owned()]),
            ..ContainerCreateBody::default()
        };

        let response = self
            .docker
            .create_container(None::<CreateContainerOptions>, config)
            .await
            .map_err(|error| self.translate(&error))?;

        Ok(response.id)
    }
}

impl From<bollard::container::LogOutput> for LogChunk {
    fn from(output: bollard::container::LogOutput) -> Self {
        use bollard::container::LogOutput;

        // Console is the TTY case, where the daemon does not separate the streams at
        // all; reporting it as stdout is the closest true statement available.
        let (stream, message) = match output {
            LogOutput::StdErr { message } => (LogStream::Stderr, message),
            LogOutput::StdOut { message }
            | LogOutput::StdIn { message }
            | LogOutput::Console { message } => (LogStream::Stdout, message),
        };

        Self {
            stream,
            bytes: message.to_vec(),
        }
    }
}

impl From<bollard::models::ImageHistoryResponseItem> for HistoryEntry {
    fn from(item: bollard::models::ImageHistoryResponseItem) -> Self {
        Self {
            id: item.id,
            created: item.created,
            created_by: item.created_by,
            size: item.size,
            comment: item.comment,
            tags: item.tags,
        }
    }
}

#[cfg(all(test, feature = "live-docker"))]
mod live_tests {
    // These need a reachable daemon, so they are off unless --features live-docker.
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::endpoint::{SystemEnv, SystemPaths, resolve};

    async fn engine() -> BollardEngine {
        let resolved = resolve(None, &SystemEnv, &SystemPaths).expect("a daemon is reachable");
        BollardEngine::connect(resolved.endpoint.path())
            .await
            .expect("connects")
    }

    #[tokio::test]
    async fn the_probe_reports_a_real_daemon() {
        let environment = engine().await.probe().await.expect("probe succeeds");

        assert!(!environment.server_version.is_empty());
        assert!(!environment.api_version.is_empty());
        assert!(!environment.storage_driver.is_empty());
    }

    #[tokio::test]
    async fn listings_agree_with_the_probe_counts() {
        let engine = engine().await;
        let environment = engine.probe().await.expect("probe succeeds");
        let containers = engine.list_containers().await.expect("containers listed");

        assert_eq!(
            i64::try_from(containers.len()).expect("count fits"),
            environment.containers_total
        );
    }

    #[tokio::test]
    async fn a_container_can_be_inspected_raw() {
        let engine = engine().await;
        let containers = engine.list_containers().await.expect("containers listed");
        let Some(container) = containers.first() else {
            return;
        };

        let raw = engine
            .inspect_container(&container.id)
            .await
            .expect("inspect succeeds");

        assert!(raw.get("Id").is_some(), "raw inspect should carry an Id");
    }

    /// The image these tests build on. Tiny, and it exits immediately when started.
    const FIXTURE_IMAGE: &str = "hello-world";

    /// Create a startable container of our own.
    ///
    /// These tests only ever touch objects they created themselves. Nothing here starts,
    /// stops or removes anything that was already on the machine, and **prune is
    /// deliberately not covered**: there is no way to exercise it without destroying
    /// whatever the person running the suite happens to have.
    async fn disposable(engine: &BollardEngine) -> String {
        let mut labels = HashMap::new();
        labels.insert(SCRATCH_LABEL.to_owned(), "1".to_owned());

        let config = ContainerCreateBody {
            image: Some(FIXTURE_IMAGE.to_owned()),
            labels: Some(labels),
            ..ContainerCreateBody::default()
        };

        engine
            .docker
            .create_container(None::<CreateContainerOptions>, config)
            .await
            .expect("creates a container")
            .id
    }

    #[tokio::test]
    async fn a_container_can_be_started_read_and_removed() {
        let engine = engine().await;
        let id = disposable(&engine).await;

        engine
            .lifecycle(&id, Lifecycle::Start)
            .await
            .expect("starts");

        // hello-world exits on its own; follow until the daemon closes the stream.
        let chunks: Vec<LogChunk> = engine
            .logs(
                &id,
                LogOptions {
                    follow: true,
                    tail: None,
                    timestamps: false,
                },
            )
            .filter_map(|chunk| async move { chunk.ok() })
            .collect()
            .await;

        let output: String = chunks
            .iter()
            .map(|chunk| String::from_utf8_lossy(&chunk.bytes).into_owned())
            .collect();

        assert!(
            output.contains("Hello from Docker"),
            "the demultiplexed log should be readable text, got {output:?}"
        );

        engine
            .remove_container(&id, false)
            .await
            .expect("removes what we created");

        assert!(
            engine.inspect_container(&id).await.is_err(),
            "the container should be gone"
        );
    }

    #[tokio::test]
    async fn an_images_filesystem_is_reachable_through_a_never_started_container() {
        let engine = engine().await;

        let id = engine
            .create_scratch_container(FIXTURE_IMAGE)
            .await
            .expect("creates a scratch container");

        // The point of the exercise: metadata without ever running the image.
        let stat = engine.stat_path(&id, "/").await.expect("stats the root");
        assert_eq!(stat.name, "/");

        let bytes: usize = engine
            .archive(&id, "/")
            .filter_map(|chunk| async move { chunk.ok() })
            .fold(0, |total, chunk| async move { total + chunk.len() })
            .await;
        assert!(bytes > 0, "the archive should carry a tar");

        engine
            .remove_container(&id, false)
            .await
            .expect("cleans up after itself");
    }

    #[tokio::test]
    async fn a_scratch_container_is_labelled_so_strays_can_be_swept() {
        let engine = engine().await;
        let id = engine
            .create_scratch_container(FIXTURE_IMAGE)
            .await
            .expect("creates a scratch container");

        let raw = engine.inspect_container(&id).await.expect("inspects");
        let labelled = raw
            .pointer("/Config/Labels")
            .and_then(|labels| labels.get(SCRATCH_LABEL))
            .is_some();

        engine
            .remove_container(&id, false)
            .await
            .expect("cleans up");

        assert!(
            labelled,
            "without the label a crash would leak this container"
        );
    }

    /// Assemble a real container's output into lines.
    ///
    /// The unit tests in `model::logs` feed the transcript synthetic chunks. This checks
    /// it against output a container actually produced, where the socket splits chunks
    /// wherever it likes and the two streams genuinely interleave.
    #[tokio::test]
    async fn a_real_containers_output_assembles_into_lines() {
        use crate::model::logs::Transcript;

        let engine = engine().await;

        // Needs a shell, so not hello-world. Skipped rather than failed on a host
        // without one: the suite must not depend on images the user happens to have.
        let images = engine.list_images().await.expect("images listed");
        let Some(shell_image) = images.iter().find(|image| {
            image
                .repo_tags
                .iter()
                .any(|tag| tag.contains("alpine") || tag.starts_with("node:"))
        }) else {
            println!("no image with a shell is present; skipping");
            return;
        };

        let config = ContainerCreateBody {
            image: Some(shell_image.id.clone()),
            entrypoint: Some(vec!["/bin/sh".to_owned()]),
            cmd: Some(vec![
                "-c".to_owned(),
                "echo first; echo to-stderr >&2; echo second; printf trailing-fragment".to_owned(),
            ]),
            ..ContainerCreateBody::default()
        };

        let id = engine
            .docker
            .create_container(None::<CreateContainerOptions>, config)
            .await
            .expect("creates a container")
            .id;

        engine
            .lifecycle(&id, Lifecycle::Start)
            .await
            .expect("starts");

        let mut transcript = Transcript::default();
        let mut stream = engine.logs(
            &id,
            LogOptions {
                follow: true,
                tail: None,
                timestamps: false,
            },
        );
        while let Some(Ok(chunk)) = stream.next().await {
            transcript.push(&chunk);
        }
        drop(stream);
        transcript.finish();

        engine
            .remove_container(&id, true)
            .await
            .expect("cleans up after itself");

        let of = |want: LogStream| -> Vec<String> {
            transcript
                .lines()
                .iter()
                .filter(|line| line.stream == want)
                .map(|line| line.text.clone())
                .collect()
        };
        let stdout = of(LogStream::Stdout);
        let stderr = of(LogStream::Stderr);
        println!("stdout: {stdout:?}\nstderr: {stderr:?}");

        assert!(stdout.iter().any(|line| line == "first"), "got {stdout:?}");
        assert!(stdout.iter().any(|line| line == "second"), "got {stdout:?}");
        assert!(
            stdout.iter().any(|line| line == "trailing-fragment"),
            "a final line without a newline must survive: {stdout:?}"
        );
        assert_eq!(stderr, vec!["to-stderr"], "the streams must stay separate");
    }

    /// Index a real container's filesystem.
    ///
    /// The unit tests in `model::fs_tree` build their own tars. This runs the parser
    /// against what Docker's Go `archive/tar` actually emits, which is where the
    /// differences live.
    #[tokio::test]
    async fn a_real_filesystem_indexes_into_a_browsable_tree() {
        use crate::model::fs_tree::{DEFAULT_BUDGET, EntryKind, Indexer};

        let engine = engine().await;
        let id = engine
            .create_scratch_container(FIXTURE_IMAGE)
            .await
            .expect("creates a scratch container");

        let mut indexer = Indexer::new("/", DEFAULT_BUDGET);
        let mut stream = engine.archive(&id, "/");
        while let Some(Ok(chunk)) = stream.next().await {
            if !indexer.push(&chunk) {
                break;
            }
        }
        drop(stream);
        let index = indexer.finish();

        engine
            .remove_container(&id, false)
            .await
            .expect("cleans up");

        let children: Vec<String> = index
            .tree
            .children("/")
            .iter()
            .map(|node| format!("{} ({:?})", node.name, node.kind))
            .collect();
        println!("children of /: {children:?}");

        assert!(!index.truncated, "hello-world is not large");
        assert!(
            index.tree.get("/hello").is_some(),
            "the image's own binary should be indexed: {children:?}"
        );
        assert_eq!(
            index.tree.get("/etc").map(|node| node.kind),
            Some(EntryKind::Directory)
        );
        assert!(
            index.tree.get("/etc/mtab").is_some_and(
                |node| node.kind == EntryKind::Symlink && node.link_target == "/proc/mounts"
            ),
            "a symlink should keep its target"
        );
        assert!(
            index.tree.children("/etc").len() >= 4,
            "immediate children of /etc: {:?}",
            index.tree.children("/etc")
        );
    }

    #[tokio::test]
    async fn history_carries_the_instructions_a_dockerfile_is_rebuilt_from() {
        let history = engine()
            .await
            .image_history(FIXTURE_IMAGE)
            .await
            .expect("reads history");

        assert!(!history.is_empty());
        assert!(
            history.iter().any(|entry| !entry.created_by.is_empty()),
            "at least one record should name what built it"
        );
    }

    #[tokio::test]
    async fn inspecting_something_absent_is_a_not_found() {
        let error = engine()
            .await
            .inspect_container("definitely-not-a-container")
            .await
            .expect_err("no such container");

        assert!(
            matches!(error, EngineError::NotFound { .. }),
            "got {error:?}"
        );
    }
}
