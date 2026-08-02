//! The bridge between the daemon and the GTK main thread.
//!
//! A tokio runtime on its own thread owns the engine, the event subscription and the
//! panel indicator. Nothing here touches a widget; everything crosses as an [`Update`]
//! consumed on the main thread.

use std::time::{SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use lave_core::activity::{Activity, ActivityState, Effect, Signal};
use lave_core::endpoint::{Resolved, SystemEnv, SystemPaths, resolve};
use lave_core::engine::{
    self, ContainerEngine, ContainerSummary, EngineError, EnvironmentSummary, ImageSummary,
    Lifecycle, LogChunk, LogOptions, bollard_engine::BollardEngine,
};
use lave_core::indicator::Counts;
use lave_core::model::action::Action;
use lave_core::model::dockerfile as dockerfile_model;
use lave_core::model::format;
use lave_core::model::fs_tree::{self, Indexer, Node};
use lave_core::model::logs::{self as logs_model, LogLine};
use lave_core::model::relations::{self, LayerIndex};
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
    /// Carry out a mutating action. Confirmation, where one was required, has already
    /// happened in the window: by the time this is sent the user has agreed.
    Act(Box<ActionRequest>),
    /// Rebuild an image's Dockerfile from its history.
    Dockerfile {
        image_id: String,
    },
    /// Start streaming a container's logs, replacing any stream already running.
    Logs {
        container_id: String,
        follow: bool,
    },
    /// Stop the current log stream. Sent when the viewer closes.
    StopLogs,
    /// List a directory. For an image this creates a scratch container on first use and
    /// keeps it for the browsing session.
    Browse {
        target: BrowseTarget,
        path: String,
    },
    /// Finished browsing: remove the scratch container, if one was made.
    StopBrowsing,
}

/// What is being browsed. An image has no filesystem the daemon will serve, so it is
/// reached through a container created from it and never started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseTarget {
    Container(String),
    Image(String),
}

/// A mutating action, bound to the object it applies to.
#[derive(Debug, Clone)]
pub struct ActionRequest {
    pub action: Action,
    /// The container or image ID the action applies to. Empty for the prunes, which
    /// apply to the daemon as a whole.
    pub id: String,
    /// How the object is named to the user, for the success and failure messages.
    pub label: String,
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
    /// A mutating action finished. Reported either way: a removal that silently failed
    /// would leave the user believing something is gone when it is not.
    ActionOutcome {
        message: String,
        failed: bool,
    },
    /// A reconstructed Dockerfile, ready to display.
    Dockerfile {
        image_id: String,
        title: String,
        text: String,
    },
    /// Lines to append to a log tab, with how many to trim from its top. Carries the
    /// container so the right tab receives them when several are open.
    LogLines {
        container_id: String,
        lines: Vec<LogLine>,
        dropped: usize,
    },
    /// The log stream ended, either because the container stopped writing or because
    /// reading it failed. `error` distinguishes the two.
    LogsEnded {
        error: Option<String>,
    },
    /// A directory listing for the file browser.
    Listing {
        path: String,
        entries: Vec<Node>,
        /// Set when the index stopped at its budget; the listing is then incomplete and
        /// says so rather than looking finished.
        notice: Option<String>,
    },
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

    // The log stream lives in the select alongside the events, rather than in a task of
    // its own: dropping it is the cancellation, so there is no second lifetime to
    // manage and no way to leak a follower.
    let mut logs: Option<BoxStream<'_, Result<LogChunk, EngineError>>> = None;
    let mut transcript = logs_model::Transcript::default();
    // Which container the open stream belongs to, so its lines can be addressed.
    let mut following: String = String::new();

    // The scratch container backing an image browse, kept for as long as the browser is
    // open so that walking into a directory does not create another.
    let mut scratch: Option<Scratch> = None;
    let mut browse_cache: Option<BrowseCache> = None;

    // Anything carrying our label at this point was left by a run that did not get to
    // clean up. Removing it here is why a crash cannot leak into the user's `docker ps`.
    sweep_scratch(engine).await;

    loop {
        let effects = tokio::select! {
            // Only polled while a viewer is open; otherwise this branch parks forever.
            chunk = next_log(&mut logs) => {
                if !consume_log(chunk, &following, &mut transcript, updates).await {
                    logs = None;
                }
                Vec::new()
            },
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
                Ok(Command::Dockerfile { image_id }) => {
                    dockerfile(engine, &image_id, state, updates).await;
                    Vec::new()
                }
                Ok(Command::Logs { container_id, follow }) => {
                    transcript = logs_model::Transcript::default();
                    following.clone_from(&container_id);
                    logs = Some(engine.logs(&container_id, LogOptions {
                        follow,
                        ..LogOptions::default()
                    }));
                    Vec::new()
                }
                Ok(Command::StopLogs) => {
                    logs = None;
                    Vec::new()
                }
                Ok(Command::Browse { target, path }) => {
                    browse(engine, &target, &path, &mut scratch, &mut browse_cache, updates)
                        .await;
                    Vec::new()
                }
                Ok(Command::StopBrowsing) => {
                    browse_cache = None;
                    release_scratch(engine, &mut scratch).await;
                    Vec::new()
                }
                Ok(Command::Act(request)) => {
                    act(engine, &request, updates).await;
                    // Refresh rather than waiting for the daemon's event, so a failed
                    // action still restores the pane to what is actually true.
                    vec![Effect::Refresh]
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

/// Absorb one item from the log stream. Returns false once the stream is over, at which
/// point the caller drops it.
async fn consume_log(
    chunk: Option<Result<LogChunk, EngineError>>,
    container_id: &str,
    transcript: &mut logs_model::Transcript,
    updates: &Sender<Update>,
) -> bool {
    match chunk {
        Some(Ok(chunk)) => {
            let appended = transcript.push(&chunk);
            send_lines(container_id, appended, updates).await;
            true
        }
        Some(Err(error)) => {
            let _ = updates
                .send(Update::LogsEnded {
                    error: Some(error.to_string()),
                })
                .await;
            false
        }
        None => {
            // The container stopped writing. Flush whatever it left without a trailing
            // newline, then say the stream is over.
            let appended = transcript.finish();
            send_lines(container_id, appended, updates).await;
            let _ = updates.send(Update::LogsEnded { error: None }).await;
            false
        }
    }
}

async fn send_lines(container_id: &str, appended: logs_model::Appended, updates: &Sender<Update>) {
    if appended.lines.is_empty() {
        return;
    }

    let _ = updates
        .send(Update::LogLines {
            container_id: container_id.to_owned(),
            lines: appended.lines,
            dropped: appended.dropped,
        })
        .await;
}

/// What has been indexed for the object currently being browsed.
///
/// The archive endpoint is recursive, so indexing `/` already covers everything beneath
/// it. Keeping that index means expanding a directory in the tree costs nothing after
/// the first fetch.
struct BrowseCache {
    target: BrowseTarget,
    index: fs_tree::Index,
}

impl BrowseCache {
    /// Whether this index already knows about `path`, for this same object.
    fn covers(&self, target: &BrowseTarget, path: &str) -> bool {
        self.target == *target && self.index.covers(path)
    }
}

/// A scratch container standing in for an image being browsed.
struct Scratch {
    /// The image it was made from, so a second browse of the same image reuses it.
    image_id: String,
    container_id: String,
}

/// Remove scratch containers left behind by a previous run.
async fn sweep_scratch(engine: &BollardEngine) {
    let Ok(containers) = engine.list_containers().await else {
        return;
    };

    for stray in engine::scratch_strays(&containers) {
        match engine.remove_container(&stray.id, true).await {
            Ok(()) => tracing::info!("swept a scratch container left by a previous run"),
            // Not fatal: it may already be going, and it is not the user's problem.
            Err(error) => tracing::warn!("could not sweep {}: {error}", stray.id),
        }
    }
}

/// List one directory, creating a scratch container first if an image is being browsed.
async fn browse(
    engine: &BollardEngine,
    target: &BrowseTarget,
    path: &str,
    scratch: &mut Option<Scratch>,
    cache: &mut Option<BrowseCache>,
    updates: &Sender<Update>,
) {
    // Answer from the index already held, when it reaches this far.
    if let Some(held) = cache.as_ref()
        && held.covers(target, path)
    {
        let entries = held
            .index
            .tree
            .children(path)
            .into_iter()
            .cloned()
            .collect();
        let _ = updates
            .send(Update::Listing {
                path: fs_tree::normalise(path),
                entries,
                notice: held.index.truncation_notice(),
            })
            .await;
        return;
    }

    let container_id = match target {
        BrowseTarget::Container(id) => id.clone(),
        BrowseTarget::Image(image_id) => match ensure_scratch(engine, image_id, scratch).await {
            Ok(id) => id,
            Err(error) => {
                let _ = updates
                    .send(Update::ActionOutcome {
                        message: format!("Could not open that image's filesystem: {error}"),
                        failed: true,
                    })
                    .await;
                return;
            }
        },
    };

    let mut indexer = Indexer::new(path, fs_tree::DEFAULT_BUDGET);
    let mut stream = engine.archive(&container_id, path);
    let mut failure = None;

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(bytes) => {
                if !indexer.push(&bytes) {
                    // Budget spent. Dropping the stream is what stops the transfer.
                    break;
                }
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    drop(stream);

    if let Some(error) = failure {
        let _ = updates
            .send(Update::ActionOutcome {
                message: format!("Could not read {path}: {error}"),
                failed: true,
            })
            .await;
        return;
    }

    let index = indexer.finish();
    let entries = index
        .tree
        .children(path)
        .into_iter()
        .cloned()
        .collect::<Vec<Node>>();
    let notice = index.truncation_notice();
    let root = index.root.clone();

    *cache = Some(BrowseCache {
        target: target.clone(),
        index,
    });

    let _ = updates
        .send(Update::Listing {
            path: root,
            entries,
            notice,
        })
        .await;
}

/// The container standing in for an image, made once per browsing session.
async fn ensure_scratch(
    engine: &BollardEngine,
    image_id: &str,
    scratch: &mut Option<Scratch>,
) -> Result<String, EngineError> {
    if let Some(existing) = scratch.as_ref()
        && existing.image_id == image_id
    {
        return Ok(existing.container_id.clone());
    }

    // Browsing a different image: the previous stand-in has no further use.
    release_scratch(engine, scratch).await;

    let container_id = engine.create_scratch_container(image_id).await?;
    *scratch = Some(Scratch {
        image_id: image_id.to_owned(),
        container_id: container_id.clone(),
    });

    Ok(container_id)
}

async fn release_scratch(engine: &BollardEngine, scratch: &mut Option<Scratch>) {
    let Some(existing) = scratch.take() else {
        return;
    };

    if let Err(error) = engine.remove_container(&existing.container_id, true).await {
        // The startup sweep will catch it next time.
        tracing::warn!("could not remove the scratch container: {error}");
    }
}

/// The next log chunk, or a future that never completes when no viewer is open.
///
/// `select!` needs every branch to be a future; parking is how a branch is disabled
/// without a guard that would still evaluate the expression.
async fn next_log(
    logs: &mut Option<BoxStream<'_, Result<LogChunk, EngineError>>>,
) -> Option<Result<LogChunk, EngineError>> {
    match logs.as_mut() {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

/// Rebuild an image's Dockerfile and send it to the window.
///
/// The `FROM` line is the reason this needs more than the image's own history: only the
/// layer analysis knows which local image is the base, and only the base's history
/// length says where its records stop and this image's begin.
async fn dockerfile(
    engine: &BollardEngine,
    image_id: &str,
    state: &SessionState,
    updates: &Sender<Update>,
) {
    let history = match engine.image_history(image_id).await {
        Ok(history) => history,
        Err(error) => {
            let _ = updates
                .send(Update::ActionOutcome {
                    message: format!("Could not read the history of that image: {error}"),
                    failed: true,
                })
                .await;
            return;
        }
    };

    // Losing the listing costs the FROM line and nothing else, so it is not worth
    // failing the whole reconstruction over.
    let images: Vec<ImageSummary> = engine.list_images().await.unwrap_or_default();

    let base = images
        .iter()
        .find(|image| image.id == image_id)
        .and_then(|image| relations::base_of(image, &images, &state.layers));

    // A base we cannot read the history of is still worth naming in the FROM line; only
    // the boundary is then unknown, and an empty base history means we claim it all.
    let base_history = match base {
        Some(base) => engine.image_history(&base.id).await.unwrap_or_default(),
        None => Vec::new(),
    };

    let base_label = base.map(format::image_label);
    let reconstruction =
        dockerfile_model::reconstruct(&history, base_label.as_deref(), &base_history);

    let title = images
        .iter()
        .find(|image| image.id == image_id)
        .map_or_else(|| format::short_id(image_id), format::image_label);

    let _ = updates
        .send(Update::Dockerfile {
            image_id: image_id.to_owned(),
            title,
            text: reconstruction.render(),
        })
        .await;
}

/// Carry out a mutating action and report how it went.
///
/// Both outcomes are reported. A removal that failed silently would leave the user
/// believing something is gone when it is not, which is the worst available outcome.
async fn act(engine: &BollardEngine, request: &ActionRequest, updates: &Sender<Update>) {
    let label = &request.label;

    let outcome = match request.action {
        Action::Lifecycle(action) => engine
            .lifecycle(&request.id, action)
            .await
            .map(|()| format!("{label} {}", past_tense(action))),
        Action::RemoveContainer { force } => engine
            .remove_container(&request.id, force)
            .await
            .map(|()| format!("Removed {label}")),
        Action::RemoveImage => engine
            .remove_image(&request.id, false)
            .await
            .map(|()| format!("Removed {label}")),
        Action::PruneContainers => engine.prune_containers().await.map(|outcome| {
            format!(
                "Removed {} stopped containers, reclaiming {}",
                outcome.removed.len(),
                format::bytes(outcome.reclaimed)
            )
        }),
        Action::PruneImages => engine.prune_images().await.map(|outcome| {
            format!(
                "Removed {} untagged images, reclaiming {}",
                outcome.removed.len(),
                format::bytes(outcome.reclaimed)
            )
        }),
        // The read-only actions are handled in the window and never reach the runtime.
        Action::ViewLogs
        | Action::ViewDockerfile
        | Action::BrowseFilesystem
        | Action::OpenInFileManager => return,
    };

    let update = match outcome {
        Ok(message) => Update::ActionOutcome {
            message,
            failed: false,
        },
        Err(error) => {
            tracing::warn!("action on {label} failed: {error}");
            Update::ActionOutcome {
                message: format!("Could not {} {label}: {error}", verb(request.action)),
                failed: true,
            }
        }
    };

    let _ = updates.send(update).await;
}

fn past_tense(action: Lifecycle) -> &'static str {
    match action {
        Lifecycle::Start => "started",
        Lifecycle::Stop => "stopped",
        Lifecycle::Restart => "restarted",
        Lifecycle::Pause => "paused",
        Lifecycle::Unpause => "resumed",
        Lifecycle::Kill => "killed",
    }
}

fn verb(action: Action) -> &'static str {
    match action {
        Action::Lifecycle(lifecycle) => lifecycle.verb(),
        Action::RemoveContainer { .. } | Action::RemoveImage => "remove",
        Action::PruneContainers | Action::PruneImages => "prune",
        Action::ViewLogs => "read the logs of",
        Action::ViewDockerfile => "reconstruct the Dockerfile for",
        Action::BrowseFilesystem | Action::OpenInFileManager => "open",
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
