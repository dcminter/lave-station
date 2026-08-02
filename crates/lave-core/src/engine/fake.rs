//! A test double for [`ContainerEngine`], so consumers can be tested without a daemon.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};

use super::{
    ContainerEngine, ContainerSummary, EngineError, EngineEvent, EnvironmentSummary, HistoryEntry,
    ImageSummary, Lifecycle, LogChunk, LogOptions, PathStat, PruneOutcome,
};

/// A mutating call the engine was asked to make.
///
/// Recorded so that "the confirm button removed the container the dialog named" is an
/// assertion rather than something checked by hand against a real daemon. Read-only
/// calls are not recorded: they cannot do damage, and recording them would bury the
/// ones that can.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCall {
    Lifecycle { id: String, action: Lifecycle },
    RemoveContainer { id: String, force: bool },
    RemoveImage { id: String, force: bool },
    PruneContainers,
    PruneImages,
    CreateScratchContainer { image_id: String },
}

#[derive(Debug, Clone, Default)]
pub struct FakeEngine {
    environment: Option<EnvironmentSummary>,
    images: Vec<ImageSummary>,
    containers: Vec<ContainerSummary>,
    events: Vec<EngineEvent>,
    inspect: Option<serde_json::Value>,
    layers: BTreeMap<String, Vec<String>>,
    history: Vec<HistoryEntry>,
    logs: Vec<LogChunk>,
    archive: Vec<u8>,
    stat: Option<PathStat>,
    failure: Option<EngineError>,
    calls: Arc<Mutex<Vec<EngineCall>>>,
}

impl FakeEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_environment(mut self, environment: EnvironmentSummary) -> Self {
        self.environment = Some(environment);
        self
    }

    #[must_use]
    pub fn with_images(mut self, images: Vec<ImageSummary>) -> Self {
        self.images = images;
        self
    }

    #[must_use]
    pub fn with_containers(mut self, containers: Vec<ContainerSummary>) -> Self {
        self.containers = containers;
        self
    }

    #[must_use]
    pub fn with_events(mut self, events: Vec<EngineEvent>) -> Self {
        self.events = events;
        self
    }

    #[must_use]
    pub fn with_inspect(mut self, inspect: serde_json::Value) -> Self {
        self.inspect = Some(inspect);
        self
    }

    #[must_use]
    pub fn with_layers(mut self, image_id: &str, layers: Vec<String>) -> Self {
        self.layers.insert(image_id.to_owned(), layers);
        self
    }

    #[must_use]
    pub fn with_history(mut self, history: Vec<HistoryEntry>) -> Self {
        self.history = history;
        self
    }

    #[must_use]
    pub fn with_logs(mut self, logs: Vec<LogChunk>) -> Self {
        self.logs = logs;
        self
    }

    /// A tar stream, delivered as a single chunk.
    #[must_use]
    pub fn with_archive(mut self, archive: Vec<u8>) -> Self {
        self.archive = archive;
        self
    }

    #[must_use]
    pub fn with_stat(mut self, stat: PathStat) -> Self {
        self.stat = Some(stat);
        self
    }

    /// Make every call fail, for exercising error paths.
    #[must_use]
    pub fn failing(mut self, error: EngineError) -> Self {
        self.failure = Some(error);
        self
    }

    /// The mutating calls made so far, in order.
    ///
    /// # Panics
    ///
    /// If a previous caller panicked while holding the lock, which cannot happen in a
    /// passing test.
    #[must_use]
    pub fn calls(&self) -> Vec<EngineCall> {
        match self.calls.lock() {
            Ok(calls) => calls.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn record(&self, call: EngineCall) {
        match self.calls.lock() {
            Ok(mut calls) => calls.push(call),
            Err(poisoned) => poisoned.into_inner().push(call),
        }
    }

    fn check(&self) -> Result<(), EngineError> {
        match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}

#[async_trait]
impl ContainerEngine for FakeEngine {
    async fn probe(&self) -> Result<EnvironmentSummary, EngineError> {
        self.check()?;
        Ok(self.environment.clone().unwrap_or_default())
    }

    async fn list_images(&self) -> Result<Vec<ImageSummary>, EngineError> {
        self.check()?;
        Ok(self.images.clone())
    }

    async fn list_containers(&self) -> Result<Vec<ContainerSummary>, EngineError> {
        self.check()?;
        Ok(self.containers.clone())
    }

    async fn inspect_image(&self, id: &str) -> Result<serde_json::Value, EngineError> {
        self.check()?;
        self.inspect.clone().ok_or_else(|| EngineError::NotFound {
            what: format!("image {id}"),
        })
    }

    async fn image_layers(&self, id: &str) -> Result<Vec<String>, EngineError> {
        self.check()?;
        Ok(self.layers.get(id).cloned().unwrap_or_default())
    }

    async fn inspect_container(&self, id: &str) -> Result<serde_json::Value, EngineError> {
        self.check()?;
        self.inspect.clone().ok_or_else(|| EngineError::NotFound {
            what: format!("container {id}"),
        })
    }

    fn events(&self, _since: Option<i64>) -> BoxStream<'_, Result<EngineEvent, EngineError>> {
        if let Some(error) = self.failure.clone() {
            return stream::once(async move { Err(error) }).boxed();
        }
        stream::iter(self.events.clone().into_iter().map(Ok)).boxed()
    }

    // Every mutating call records before it checks for failure, so a test can assert
    // what was attempted even when the daemon refused it.

    async fn lifecycle(&self, id: &str, action: Lifecycle) -> Result<(), EngineError> {
        self.record(EngineCall::Lifecycle {
            id: id.to_owned(),
            action,
        });
        self.check()
    }

    async fn remove_container(&self, id: &str, force: bool) -> Result<(), EngineError> {
        self.record(EngineCall::RemoveContainer {
            id: id.to_owned(),
            force,
        });
        self.check()
    }

    async fn remove_image(&self, id: &str, force: bool) -> Result<(), EngineError> {
        self.record(EngineCall::RemoveImage {
            id: id.to_owned(),
            force,
        });
        self.check()
    }

    async fn prune_containers(&self) -> Result<PruneOutcome, EngineError> {
        self.record(EngineCall::PruneContainers);
        self.check()?;
        Ok(PruneOutcome::default())
    }

    async fn prune_images(&self) -> Result<PruneOutcome, EngineError> {
        self.record(EngineCall::PruneImages);
        self.check()?;
        Ok(PruneOutcome::default())
    }

    fn logs(
        &self,
        _id: &str,
        _options: LogOptions,
    ) -> BoxStream<'_, Result<LogChunk, EngineError>> {
        if let Some(error) = self.failure.clone() {
            return stream::once(async move { Err(error) }).boxed();
        }
        stream::iter(self.logs.clone().into_iter().map(Ok)).boxed()
    }

    fn archive(&self, _id: &str, _path: &str) -> BoxStream<'_, Result<Vec<u8>, EngineError>> {
        if let Some(error) = self.failure.clone() {
            return stream::once(async move { Err(error) }).boxed();
        }
        stream::once({
            let archive = self.archive.clone();
            async move { Ok(archive) }
        })
        .boxed()
    }

    async fn stat_path(&self, _id: &str, path: &str) -> Result<PathStat, EngineError> {
        self.check()?;
        self.stat.clone().ok_or_else(|| EngineError::NotFound {
            what: path.to_owned(),
        })
    }

    async fn image_history(&self, _id: &str) -> Result<Vec<HistoryEntry>, EngineError> {
        self.check()?;
        Ok(self.history.clone())
    }

    async fn create_scratch_container(&self, image_id: &str) -> Result<String, EngineError> {
        self.record(EngineCall::CreateScratchContainer {
            image_id: image_id.to_owned(),
        });
        self.check()?;
        Ok(format!("scratch-for-{image_id}"))
    }
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;
    use std::io::ErrorKind;
    use std::path::Path;

    #[tokio::test]
    async fn a_configured_engine_returns_what_it_was_given() {
        let engine = FakeEngine::new()
            .with_images(vec![ImageSummary {
                id: "sha256:abc".to_owned(),
                ..ImageSummary::default()
            }])
            .with_containers(vec![ContainerSummary::default()]);

        assert_eq!(engine.list_images().await.expect("images").len(), 1);
        assert_eq!(engine.list_containers().await.expect("containers").len(), 1);
    }

    #[tokio::test]
    async fn a_failing_engine_fails_every_call_including_the_event_stream() {
        let error =
            EngineError::unreachable(Path::new("/var/run/docker.sock"), ErrorKind::NotFound);
        let engine = FakeEngine::new().failing(error);

        assert!(engine.probe().await.is_err());
        assert!(engine.list_images().await.is_err());
        assert!(engine.list_containers().await.is_err());

        let first = engine.events(None).next().await.expect("one item");
        assert!(first.is_err());
    }

    #[test]
    fn only_our_own_labelled_containers_count_as_strays() {
        let mut labels = BTreeMap::new();
        labels.insert(super::super::SCRATCH_LABEL.to_owned(), "1".to_owned());

        let ours = ContainerSummary {
            id: "ours".to_owned(),
            labels,
            ..ContainerSummary::default()
        };
        // Same image, same shape, no label: emphatically not ours to remove.
        let theirs = ContainerSummary {
            id: "theirs".to_owned(),
            ..ContainerSummary::default()
        };

        let containers = [ours, theirs];
        let strays = super::super::scratch_strays(&containers);

        assert_eq!(strays.len(), 1);
        assert_eq!(strays[0].id, "ours");
    }

    #[tokio::test]
    async fn mutating_calls_are_recorded_in_order() {
        let engine = FakeEngine::new();

        engine
            .lifecycle("abc", Lifecycle::Stop)
            .await
            .expect("stops");
        engine.remove_container("abc", true).await.expect("removes");

        assert_eq!(
            engine.calls(),
            vec![
                EngineCall::Lifecycle {
                    id: "abc".to_owned(),
                    action: Lifecycle::Stop
                },
                EngineCall::RemoveContainer {
                    id: "abc".to_owned(),
                    force: true
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_refused_call_is_still_recorded_as_attempted() {
        let engine = FakeEngine::new().failing(EngineError::Protocol("no".to_owned()));

        assert!(engine.remove_image("sha256:abc", false).await.is_err());

        assert_eq!(
            engine.calls(),
            vec![EngineCall::RemoveImage {
                id: "sha256:abc".to_owned(),
                force: false
            }]
        );
    }

    #[tokio::test]
    async fn reading_records_nothing_so_the_log_holds_only_what_can_do_damage() {
        let engine = FakeEngine::new();

        engine.list_images().await.expect("images");
        engine.list_containers().await.expect("containers");
        engine.image_history("sha256:abc").await.expect("history");

        assert!(engine.calls().is_empty());
    }

    #[tokio::test]
    async fn inspect_reports_not_found_when_nothing_was_configured() {
        let engine = FakeEngine::new();

        let error = engine
            .inspect_container("abc")
            .await
            .expect_err("nothing configured");

        assert!(matches!(error, EngineError::NotFound { .. }));
    }
}
