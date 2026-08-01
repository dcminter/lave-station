//! The bridge between the daemon and the GTK main thread.
//!
//! A tokio runtime on its own thread owns the engine, the event subscription and the
//! panel indicator. Nothing here touches a widget; everything crosses as an [`Update`]
//! consumed on the main thread.

use std::time::{SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender};
use futures_util::StreamExt;
use lave_core::activity::{Activity, ActivityState, Effect, Signal};
use lave_core::endpoint::{Resolved, SystemEnv, SystemPaths, resolve};
use lave_core::engine::{
    ContainerEngine, ContainerSummary, EngineError, EnvironmentSummary, ImageSummary,
    bollard_engine::BollardEngine,
};
use lave_core::indicator::Counts;
use lave_core::model::relations::LayerIndex;
use lave_core::model::tree::NodeId;

use crate::background::{self, BackgroundOutcome};
use crate::indicator_tray::{self, TrayHandle};

/// How many images to inspect at once when filling in layer data. The socket is local,
/// but a hundred simultaneous requests would still be rude to a busy daemon.
const LAYER_FETCH_CONCURRENCY: usize = 8;

/// Everything the window knows about the daemon at one moment.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub resolved: Resolved,
    pub environment: EnvironmentSummary,
    pub images: Vec<ImageSummary>,
    pub containers: Vec<ContainerSummary>,
    /// Layer digests per image, from which derivation is reconstructed.
    pub layers: LayerIndex,
}

/// What one connection accumulates. Bundled so the session functions keep a sane
/// number of parameters as more of it appears.
struct SessionState {
    activity: Activity,
    counts: Counts,
    /// Cached across refreshes and reconnects: layer stacks do not change, so an image
    /// already inspected never needs inspecting again.
    layers: LayerIndex,
}

/// Connection state, rendered as the status page when not connected.
#[derive(Debug, Clone)]
pub struct StatusView {
    pub state: ActivityState,
    pub message: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Command {
    Refresh,
    Inspect(NodeId),
}

#[derive(Debug)]
pub enum Update {
    Snapshot(Box<Snapshot>),
    Inspected {
        id: NodeId,
        raw: Box<serde_json::Value>,
    },
    Status(StatusView),
    /// Whether the desktop actually has somewhere to put the indicator.
    IndicatorAvailable(bool),
    /// Whether the desktop permits running with no window.
    Background(BackgroundOutcome),
    /// The tray was clicked, or its Open item chosen.
    OpenRequested,
    QuitRequested,
}

pub struct RuntimeHandle {
    pub commands: Sender<Command>,
    pub updates: Receiver<Update>,
}

/// Start the runtime thread. Returns immediately.
#[must_use]
pub fn start(docker_host: Option<String>, want_indicator: bool) -> RuntimeHandle {
    let (command_tx, command_rx) = async_channel::unbounded::<Command>();
    let (update_tx, update_rx) = async_channel::unbounded::<Update>();

    std::thread::Builder::new()
        .name("lave-runtime".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!("could not start the async runtime: {error}");
                    return;
                }
            };
            runtime.block_on(serve(docker_host, want_indicator, &command_rx, &update_tx));
        })
        .map_or_else(
            |error| tracing::error!("could not start the runtime thread: {error}"),
            |_| (),
        );

    RuntimeHandle {
        commands: command_tx,
        updates: update_rx,
    }
}

#[must_use]
pub fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or_default()
}

enum SessionEnd {
    Lost(EngineError),
    Closed,
}

async fn serve(
    docker_host: Option<String>,
    want_indicator: bool,
    commands: &Receiver<Command>,
    updates: &Sender<Update>,
) {
    let mut state = SessionState {
        activity: Activity::new(),
        counts: Counts::default(),
        layers: LayerIndex::new(),
    };

    let tray = if want_indicator {
        indicator_tray::start(updates.clone(), &state.activity, state.counts).await
    } else {
        None
    };
    let _ = updates
        .send(Update::IndicatorAvailable(tray.is_some()))
        .await;

    // Only worth asking when there is an indicator to return to.
    if tray.is_some() {
        let _ = updates
            .send(Update::Background(background::request().await))
            .await;
    }

    loop {
        publish(updates, &state.activity, tray.as_ref(), state.counts).await;

        // Resolution happens per attempt, so a daemon that starts later is found.
        let resolved = match resolve(docker_host.as_deref(), &SystemEnv, &SystemPaths) {
            Ok(resolved) => resolved,
            Err(error) => {
                let error = EngineError::Unreachable {
                    explanation: error.to_string(),
                    hint: None,
                };
                if !retry_after(&mut state, error, updates, tray.as_ref()).await {
                    return;
                }
                continue;
            }
        };

        let engine = match BollardEngine::connect(resolved.endpoint.path()).await {
            Ok(engine) => engine,
            Err(error) => {
                if !retry_after(&mut state, error, updates, tray.as_ref()).await {
                    return;
                }
                continue;
            }
        };

        match session(
            &engine,
            &resolved,
            &mut state,
            commands,
            updates,
            tray.as_ref(),
        )
        .await
        {
            SessionEnd::Closed => return,
            SessionEnd::Lost(error) => {
                if !retry_after(&mut state, error, updates, tray.as_ref()).await {
                    return;
                }
            }
        }
    }
}

/// One connected session: hold the event stream open and serve commands until it drops.
async fn session(
    engine: &BollardEngine,
    resolved: &Resolved,
    state: &mut SessionState,
    commands: &Receiver<Command>,
    updates: &Sender<Update>,
    tray: Option<&TrayHandle>,
) -> SessionEnd {
    let mut events = engine.events(state.activity.since());

    let effects = state
        .activity
        .apply(Signal::Connected { at: now_seconds() });
    publish(updates, &state.activity, tray, state.counts).await;
    for effect in effects {
        if matches!(effect, Effect::Refresh) {
            refresh(engine, resolved, state, updates).await;
            publish(updates, &state.activity, tray, state.counts).await;
        }
    }

    loop {
        let effects = tokio::select! {
            message = events.next() => match message {
                Some(Ok(event)) => state.activity.apply(Signal::Observed(event)),
                Some(Err(error)) => return SessionEnd::Lost(error),
                None => return SessionEnd::Lost(EngineError::Unreachable {
                    explanation: "the daemon closed the event stream".to_owned(),
                    hint: None,
                }),
            },
            command = commands.recv() => match command {
                Ok(Command::Refresh) => vec![Effect::Refresh],
                Ok(Command::Inspect(id)) => {
                    inspect(engine, &id, updates).await;
                    Vec::new()
                }
                Err(_) => return SessionEnd::Closed,
            },
        };

        for effect in effects {
            if matches!(effect, Effect::Refresh) {
                refresh(engine, resolved, state, updates).await;
            }
        }
        publish(updates, &state.activity, tray, state.counts).await;
    }
}

/// Apply a loss, then wait out the backoff. False means stop trying.
async fn retry_after(
    state: &mut SessionState,
    error: EngineError,
    updates: &Sender<Update>,
    tray: Option<&TrayHandle>,
) -> bool {
    let effects = state.activity.apply(Signal::Lost(error));
    publish(updates, &state.activity, tray, state.counts).await;

    for effect in effects {
        match effect {
            Effect::WaitThen(delay) => {
                tokio::time::sleep(delay).await;
                state.activity.apply(Signal::RetryElapsed);
                return true;
            }
            Effect::Stop => return false,
            Effect::Connect | Effect::Refresh => {}
        }
    }
    true
}

async fn refresh(
    engine: &BollardEngine,
    resolved: &Resolved,
    state: &mut SessionState,
    updates: &Sender<Update>,
) {
    let environment = match engine.probe().await {
        Ok(environment) => environment,
        Err(error) => {
            tracing::warn!("probe failed: {error}");
            return;
        }
    };
    let images = match engine.list_images().await {
        Ok(images) => images,
        Err(error) => {
            tracing::warn!("listing images failed: {error}");
            return;
        }
    };
    let containers = match engine.list_containers().await {
        Ok(containers) => containers,
        Err(error) => {
            tracing::warn!("listing containers failed: {error}");
            return;
        }
    };

    fetch_layers(engine, &images, &mut state.layers).await;

    state.counts = Counts {
        images: images.len(),
        containers: containers.len(),
        running: containers
            .iter()
            .filter(|container| container.state.is_active())
            .count(),
    };
    tracing::debug!(
        "refreshed from {}: {} images, {} containers ({} running)",
        resolved.endpoint,
        state.counts.images,
        state.counts.containers,
        state.counts.running
    );

    let _ = updates
        .send(Update::Snapshot(Box::new(Snapshot {
            resolved: resolved.clone(),
            environment,
            images,
            containers,
            layers: state.layers.clone(),
        })))
        .await;
}

/// Fill in the layer stacks of images not seen before.
///
/// The image listing does not carry them, so this costs one inspect per *new* image.
/// A failure is not fatal: the relationships that layer data would have shown are
/// simply absent, which is better than failing the whole refresh.
async fn fetch_layers(engine: &BollardEngine, images: &[ImageSummary], layers: &mut LayerIndex) {
    layers.retain_known(images);

    let missing: Vec<String> = images
        .iter()
        .filter(|image| !layers.contains(&image.id))
        .map(|image| image.id.clone())
        .collect();
    if missing.is_empty() {
        return;
    }

    let fetched: Vec<(String, Result<Vec<String>, EngineError>)> =
        futures_util::stream::iter(missing.into_iter().map(|id| async move {
            let result = engine.image_layers(&id).await;
            (id, result)
        }))
        .buffer_unordered(LAYER_FETCH_CONCURRENCY)
        .collect()
        .await;

    let mut failures = 0;
    for (id, result) in fetched {
        match result {
            Ok(digests) => layers.insert(&id, digests),
            Err(error) => {
                failures += 1;
                tracing::debug!("could not read layers of {id}: {error}");
            }
        }
    }

    tracing::debug!(
        "layer index now covers {} images ({failures} could not be read)",
        layers.len()
    );
}

async fn inspect(engine: &BollardEngine, id: &NodeId, updates: &Sender<Update>) {
    let raw = match id {
        NodeId::Image(image) => engine.inspect_image(image).await,
        NodeId::Container(container) => engine.inspect_container(container).await,
        _ => return,
    };

    match raw {
        Ok(raw) => {
            let _ = updates
                .send(Update::Inspected {
                    id: id.clone(),
                    raw: Box::new(raw),
                })
                .await;
        }
        Err(error) => tracing::warn!("inspect failed: {error}"),
    }
}

/// Push the current state to both the window and the indicator.
async fn publish(
    updates: &Sender<Update>,
    activity: &Activity,
    tray: Option<&TrayHandle>,
    counts: Counts,
) {
    if let Some(tray) = tray {
        indicator_tray::refresh(tray, activity, counts).await;
    }

    let (message, hint) = match activity.state() {
        ActivityState::Connected => ("Connected".to_owned(), None),
        ActivityState::Connecting => ("Connecting to the Docker daemon".to_owned(), None),
        ActivityState::Reconnecting { delay, .. } => (
            format!("Lost the connection. Retrying in {}s.", delay.as_secs()),
            None,
        ),
        ActivityState::Failed { reason, hint } => (reason.clone(), hint.clone()),
    };

    let _ = updates
        .send(Update::Status(StatusView {
            state: activity.state().clone(),
            message,
            hint,
        }))
        .await;
}
