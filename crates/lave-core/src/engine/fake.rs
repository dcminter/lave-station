//! A test double for [`ContainerEngine`], so consumers can be tested without a daemon.

use std::collections::BTreeMap;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};

use super::{
    ContainerEngine, ContainerSummary, EngineError, EngineEvent, EnvironmentSummary, ImageSummary,
};

#[derive(Debug, Clone, Default)]
pub struct FakeEngine {
    environment: Option<EnvironmentSummary>,
    images: Vec<ImageSummary>,
    containers: Vec<ContainerSummary>,
    events: Vec<EngineEvent>,
    inspect: Option<serde_json::Value>,
    layers: BTreeMap<String, Vec<String>>,
    failure: Option<EngineError>,
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

    /// Make every call fail, for exercising error paths.
    #[must_use]
    pub fn failing(mut self, error: EngineError) -> Self {
        self.failure = Some(error);
        self
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
