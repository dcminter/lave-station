//! The bridge between the daemon and the GTK main thread.
//!
//! A tokio runtime on its own thread owns the engine, the event subscription and the
//! panel indicator. Nothing here touches a widget; everything crosses as an [`Update`]
//! consumed on the main thread.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use async_channel::{Receiver, Sender};
use futures_util::StreamExt;
use futures_util::stream::{AbortHandle, BoxStream, SelectAll};
use lave_core::activity::{Activity, ActivityState, Effect, Signal};
use lave_core::endpoint::{Resolved, SystemEnv, SystemPaths, resolve};
use lave_core::engine::{
    self, ContainerEngine, ContainerSummary, DiskUsage, EngineError, EnvironmentSummary,
    ImageSummary, Lifecycle, LogChunk, LogOptions, bollard_engine::BollardEngine,
};
use lave_core::indicator::Counts;
use lave_core::model::action::Action;
use lave_core::model::dockerfile as dockerfile_model;
use lave_core::model::format;
use lave_core::model::fs_tree::{self, Indexer, Node};
use lave_core::model::logs::{self as logs_model, LogLine};
use lave_core::model::metrics::StatsIndex;
use lave_core::model::relations::{self, LayerIndex};
use lave_core::model::tree::NodeId;

use crate::background::{self, BackgroundOutcome};
use crate::indicator_tray::{self, TrayHandle};

/// How many images to inspect at once when filling in layer data. The socket is local,
/// but a hundred simultaneous requests would still be rude to a busy daemon.
const LAYER_FETCH_CONCURRENCY: usize = 8;

/// How many containers to sample at once. Same reasoning as the layer fetch, and the
/// same socket.
const STATS_FETCH_CONCURRENCY: usize = 8;

/// How often the running containers are re-sampled while connected.
///
/// Memory moves between daemon events, so it cannot ride on the event stream the way
/// the listings do. Matched to what `docker stats` itself samples at: often enough to
/// be current, rare enough that a quiet machine is left alone.
const STATS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Everything the window knows about the daemon at one moment.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub resolved: Resolved,
    pub environment: EnvironmentSummary,
    pub images: Vec<ImageSummary>,
    pub containers: Vec<ContainerSummary>,
    /// Layer digests per image, from which derivation is reconstructed.
    pub layers: LayerIndex,
    /// The most recent memory sample per running container.
    pub stats: StatsIndex,
    /// What the daemon says its storage is spent on.
    pub disk: DiskUsage,
}

/// What one connection accumulates. Bundled so the session functions keep a sane
/// number of parameters as more of it appears.
struct SessionState {
    activity: Activity,
    counts: Counts,
    /// Cached across refreshes and reconnects: layer stacks do not change, so an image
    /// already inspected never needs inspecting again.
    layers: LayerIndex,
    /// The containers that were executing at the last refresh: what the stats timer
    /// samples between one listing and the next.
    running: Vec<String>,
    /// Kept between refreshes so a listing that arrives before the next sample still
    /// carries the last figures rather than blanking the column.
    stats: StatsIndex,
    disk: DiskUsage,
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
    /// Carry out the same action across several objects, reporting once at the end.
    ActMany(Vec<ActionRequest>),
    /// Rebuild an image's Dockerfile from its history.
    Dockerfile {
        image_id: String,
    },
    /// Start streaming a container's logs, replacing that container's stream if one is
    /// already running — which is how the viewer switches between the tail and the whole
    /// log. Other containers' streams are untouched.
    Logs {
        container_id: String,
        follow: bool,
        /// Start from the last this-many lines; `None` for the whole log.
        tail: Option<usize>,
    },
    /// Stop one container's log stream. Sent when its tab closes.
    StopLogs {
        container_id: String,
    },
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
    /// Fresh memory samples, without a new listing. Sent on the stats timer: the
    /// listings have not changed, so only the figures are carried.
    Stats(StatsIndex),
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
    /// A log stream ended, either because the container stopped writing or because
    /// reading it failed. `error` distinguishes the two.
    LogsEnded {
        container_id: String,
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
        running: Vec::new(),
        stats: StatsIndex::new(),
        disk: DiskUsage::default(),
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

    // The log streams live in the select alongside the events, rather than in tasks of
    // their own: there is then no second lifetime to manage and no way to leak a
    // follower. One per open viewer, since several tabs can be open at once and closing
    // one must not silence the others.
    let mut followers: SelectAll<BoxStream<'_, LogItem>> = SelectAll::new();
    let mut aborts: HashMap<String, AbortHandle> = HashMap::new();
    let mut transcripts: HashMap<String, logs_model::Transcript> = HashMap::new();

    // The scratch container backing an image browse, kept for as long as the browser is
    // open so that walking into a directory does not create another.
    let mut scratch: Option<Scratch> = None;
    let mut browse_cache: Option<BrowseCache> = None;

    // Anything carrying our label at this point was left by a run that did not get to
    // clean up. Removing it here is why a crash cannot leak into the user's `docker ps`.
    sweep_scratch(engine).await;

    // The listings ride on the daemon's events; the memory figures cannot, because they
    // move without anything happening. The connection's own refresh has just sampled, so
    // the immediate first tick is consumed here rather than repeating it.
    let mut sampler = tokio::time::interval(STATS_INTERVAL);
    sampler.tick().await;

    loop {
        let effects = tokio::select! {
            _ = sampler.tick() => {
                sample_stats(engine, state, updates).await;
                Vec::new()
            },
            // Only polled while a viewer is open; otherwise this branch parks forever.
            item = next_log(&mut followers) => {
                consume_log(item, &mut transcripts, &mut aborts, updates).await;
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
                Ok(Command::Logs { container_id, follow, tail }) => {
                    start_logs(
                        engine, &container_id, follow, tail,
                        &mut followers, &mut aborts, &mut transcripts,
                    );
                    Vec::new()
                }
                Ok(Command::StopLogs { container_id }) => {
                    stop_logs(&container_id, &mut aborts, &mut transcripts);
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
                Ok(Command::ActMany(requests)) => {
                    act_many(engine, &requests, updates).await;
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
    state.stats.retain_known(&containers);
    state.running = containers
        .iter()
        .filter(|container| container.state.is_active())
        .map(|container| container.id.clone())
        .collect();
    fetch_stats(engine, &state.running, &mut state.stats).await;
    fetch_disk_usage(engine, &mut state.disk).await;

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
            stats: state.stats.clone(),
            disk: state.disk,
        })))
        .await;
}

/// Sample the memory of every container that is executing.
///
/// One request each, so this is bounded like the layer fetch. A container that stops
/// between the listing and the sample simply fails, which costs its own figure and
/// nothing else: the listings are what the page is built from, not this.
async fn fetch_stats(engine: &BollardEngine, running: &[String], stats: &mut StatsIndex) {
    if running.is_empty() {
        return;
    }

    let sampled: Vec<Result<engine::ContainerStats, EngineError>> = futures_util::stream::iter(
        running
            .iter()
            .map(|id| async move { engine.container_stats(id).await }),
    )
    .buffer_unordered(STATS_FETCH_CONCURRENCY)
    .collect()
    .await;

    let mut failures = 0;
    for result in sampled {
        match result {
            Ok(sample) => stats.insert(sample),
            Err(error) => {
                failures += 1;
                tracing::debug!("could not sample a container: {error}");
            }
        }
    }

    if failures > 0 {
        tracing::debug!(
            "{failures} of {} containers could not be sampled",
            running.len()
        );
    }
}

/// Re-sample on the timer and send the figures on their own, without a listing.
///
/// The window merges these into the snapshot it already has: nothing about what exists
/// has changed, so rebuilding the tree for them would be work for nothing.
async fn sample_stats(engine: &BollardEngine, state: &mut SessionState, updates: &Sender<Update>) {
    if state.running.is_empty() {
        return;
    }

    fetch_stats(engine, &state.running, &mut state.stats).await;
    let _ = updates.send(Update::Stats(state.stats.clone())).await;
}

/// Ask the daemon what its storage is spent on.
///
/// Left as it was on failure rather than blanked: a stale figure is closer to the truth
/// than no figure, and `/system/df` is the one call here that walks the disk.
async fn fetch_disk_usage(engine: &BollardEngine, disk: &mut DiskUsage) {
    match engine.disk_usage().await {
        Ok(usage) => *disk = usage,
        Err(error) => tracing::debug!("could not read disk usage: {error}"),
    }
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

/// One item from one container's log stream: `None` marks that stream's end, and the ID
/// is carried because several streams are multiplexed into one.
type LogItem = (String, Option<Result<LogChunk, EngineError>>);

/// Start following a container, replacing its own stream if it already had one.
///
/// Replacing is how the viewer switches between the tail and the whole log; every other
/// container's stream is left alone, which is what makes several viewers work at once.
fn start_logs<'a>(
    engine: &'a BollardEngine,
    container_id: &str,
    follow: bool,
    tail: Option<usize>,
    followers: &mut SelectAll<BoxStream<'a, LogItem>>,
    aborts: &mut HashMap<String, AbortHandle>,
    transcripts: &mut HashMap<String, logs_model::Transcript>,
) {
    stop_logs(container_id, aborts, transcripts);

    let stream = engine.logs(
        container_id,
        LogOptions {
            follow,
            tail,
            ..LogOptions::default()
        },
    );

    // Tagged with the container, and given an explicit terminator: once the stream is
    // merged into the others there is no other way to tell which one has finished.
    let tag = container_id.to_owned();
    let ending = container_id.to_owned();
    let tagged = stream
        .map(move |item| (tag.clone(), Some(item)))
        .chain(futures_util::stream::once(async move { (ending, None) }));

    // Aborting is what stops the transfer; a stream that has ended is dropped from the
    // set by `SelectAll` itself.
    let (abortable, handle) = futures_util::stream::abortable(tagged);
    aborts.insert(container_id.to_owned(), handle);
    transcripts.insert(container_id.to_owned(), logs_model::Transcript::default());
    followers.push(abortable.boxed());
}

/// Stop following a container. Aborting cuts the stream before its terminator, so a
/// deliberate stop reports nothing to the window — there is nothing to report.
fn stop_logs(
    container_id: &str,
    aborts: &mut HashMap<String, AbortHandle>,
    transcripts: &mut HashMap<String, logs_model::Transcript>,
) {
    if let Some(handle) = aborts.remove(container_id) {
        handle.abort();
    }
    transcripts.remove(container_id);
}

/// Absorb one item from whichever stream produced it.
async fn consume_log(
    item: Option<LogItem>,
    transcripts: &mut HashMap<String, logs_model::Transcript>,
    aborts: &mut HashMap<String, AbortHandle>,
    updates: &Sender<Update>,
) {
    // The set drained between being polled and now; the next poll parks.
    let Some((container_id, message)) = item else {
        return;
    };

    match message {
        Some(Ok(chunk)) => {
            if let Some(transcript) = transcripts.get_mut(&container_id) {
                let appended = transcript.push(&chunk);
                send_lines(&container_id, appended, updates).await;
            }
        }
        Some(Err(error)) => {
            stop_logs(&container_id, aborts, transcripts);
            let _ = updates
                .send(Update::LogsEnded {
                    container_id,
                    error: Some(error.to_string()),
                })
                .await;
        }
        None => {
            // The container stopped writing. Flush whatever it left without a trailing
            // newline, then say the stream is over.
            if let Some(mut transcript) = transcripts.remove(&container_id) {
                let appended = transcript.finish();
                send_lines(&container_id, appended, updates).await;
            }
            aborts.remove(&container_id);
            let _ = updates
                .send(Update::LogsEnded {
                    container_id,
                    error: None,
                })
                .await;
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

/// The next item from any open log stream, or a future that never completes when no
/// viewer is open.
///
/// `select!` needs every branch to be a future; parking is how a branch is disabled
/// without a guard that would still evaluate the expression. An empty `SelectAll`
/// completes immediately with `None`, which would spin the loop.
async fn next_log(followers: &mut SelectAll<BoxStream<'_, LogItem>>) -> Option<LogItem> {
    if followers.is_empty() {
        return std::future::pending().await;
    }
    followers.next().await
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
    let update = match run(engine, request).await {
        // The read-only actions are handled in the window and never arrive here; if one
        // did, an empty toast would be worse than saying nothing.
        Ok(message) if message.is_empty() => return,
        Ok(message) => Update::ActionOutcome {
            message,
            failed: false,
        },
        Err(error) => {
            tracing::warn!("action on {} failed: {error}", request.label);
            Update::ActionOutcome {
                message: format!(
                    "Could not {} {}: {error}",
                    lave_core::model::action::verb(request.action),
                    request.label
                ),
                failed: true,
            }
        }
    };

    let _ = updates.send(update).await;
}

/// Carry out the same action across a checked selection, reporting once.
///
/// Sequential rather than concurrent: the objects are related often enough — a container
/// and the image it holds open — that racing them produces failures the user then has to
/// reason about. A handful of local socket calls is fast enough.
async fn act_many(engine: &BollardEngine, requests: &[ActionRequest], updates: &Sender<Update>) {
    let Some(first) = requests.first() else {
        return;
    };

    let mut succeeded = 0;
    let mut failures = Vec::new();

    for request in requests {
        match run(engine, request).await {
            Ok(_) => succeeded += 1,
            Err(error) => {
                tracing::warn!("action on {} failed: {error}", request.label);
                failures.push(format!("{}: {error}", request.label));
            }
        }
    }

    let (message, failed) =
        lave_core::model::action::bulk_outcome(first.action, succeeded, &failures);
    let _ = updates
        .send(Update::ActionOutcome { message, failed })
        .await;
}

/// One action, returning what to say about it having worked.
async fn run(engine: &BollardEngine, request: &ActionRequest) -> Result<String, EngineError> {
    let label = &request.label;

    match request.action {
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
        | Action::OpenInFileManager => Ok(String::new()),
    }
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
