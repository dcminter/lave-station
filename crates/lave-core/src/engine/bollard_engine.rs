//! The real client: HTTP/JSON over a Unix socket, via `bollard`.
//!
//! Deliberately thin. Everything interesting happens in `convert` or above.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bollard::query_parameters::{EventsOptions, ListContainersOptions, ListImagesOptions};
use bollard::{API_DEFAULT_VERSION, Docker};
use futures_util::stream::{BoxStream, StreamExt};

use super::convert::environment_summary;
use super::{
    ContainerEngine, ContainerSummary, EngineError, EngineEvent, EnvironmentSummary, ImageSummary,
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
