//! What the right-hand pane shows for the current selection.
//!
//! Returned as data rather than widgets: the content is decided and tested here, and
//! the GTK layer only turns rows into `AdwActionRow`s.

use crate::endpoint::Resolved;
use crate::engine::{
    ContainerState, ContainerSummary, DiskCategory, DiskUsage, EnvironmentSummary, ImageSummary,
};

use super::action::{self, Offer};
use super::format::{
    bytes, container_label, image_label, instant, is_untagged, list_or_none, port, share, short_id,
    text_or_unknown,
};
use super::metrics::StatsIndex;
use super::relations::{self, LayerIndex};
use super::table::{self, Table};
use super::tree::NodeId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailRow {
    pub label: String,
    pub value: String,
    /// Where selecting this row navigates to. `None` for plain informational rows.
    pub link: Option<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailGroup {
    pub title: String,
    pub rows: Vec<DetailRow>,
}

/// Which listing a table's filter narrows, and so which preference it drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    /// Leave out containers that are not running.
    StoppedContainers,
    /// Leave out images carrying no tag.
    UntaggedImages,
}

/// The narrowed / everything toggle offered above a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableFilter {
    pub kind: FilterKind,
    /// True when the rows the filter would leave out are included.
    pub showing_all: bool,
    /// The narrowed view, with its count: "Running (3)", "Tagged (12)".
    pub narrow_label: String,
    pub all_label: String,
    /// Rows the table should be given before the user drags it. See
    /// [`super::table::visible_rows`].
    pub visible_rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailPage {
    pub title: String,
    pub subtitle: Option<String>,
    pub groups: Vec<DetailGroup>,
    /// Shown on the pages that list many objects. Rendered at full window width, since
    /// a table is exactly the thing the width is for.
    pub table: Option<Table>,
    /// Whether the table leads the page or follows the groups.
    pub table_first: bool,
    /// When set, the table is offered with a filter toggle above it.
    pub table_filter: Option<TableFilter>,
    /// A figure about the listing as a whole, shown in the strip above the table where
    /// it is read alongside the rows it sums rather than below them.
    pub table_summary: Option<String>,
    /// Pretty-printed inspect output, shown in a collapsed expander.
    pub raw: Option<String>,
    /// What the user may do to this object, decided in [`crate::model::action`].
    pub actions: Vec<Offer>,
}

/// Everything a detail page needs beyond the object it is describing. Passed as one
/// value so adding context does not ripple through every call site.
pub struct Context<'a> {
    pub images: &'a [ImageSummary],
    pub containers: &'a [ContainerSummary],
    pub layers: &'a LayerIndex,
    /// The most recent memory sample per container. Refreshed on its own timer, so it
    /// moves between listings rather than with them.
    pub stats: &'a StatsIndex,
    /// What the daemon says its storage is spent on.
    pub disk: &'a DiskUsage,
    pub raw: Option<&'a serde_json::Value>,
    /// Seconds since the Unix epoch, injected so rendering is deterministic.
    pub now: i64,
    pub offset: chrono::FixedOffset,
    /// Whether container tables include the ones that are not running.
    pub show_stopped: bool,
    /// Whether the image table includes the ones carrying no tag.
    pub show_untagged: bool,
}

impl DetailPage {
    /// Look up a row's value. Convenience for callers and tests.
    #[must_use]
    pub fn value(&self, group: &str, label: &str) -> Option<&str> {
        self.groups
            .iter()
            .find(|candidate| candidate.title == group)?
            .rows
            .iter()
            .find(|row| row.label == label)
            .map(|row| row.value.as_str())
    }

    /// Whether the page leads with a strip of buttons for its actions.
    ///
    /// Only the pages describing a single object do. A listing page acts through its
    /// table — per row from the context menu, or on the checked rows from the cog — and
    /// the daemon's own prunes are on the primary menu, where a whole-machine action
    /// belongs.
    #[must_use]
    pub fn shows_action_bar(&self) -> bool {
        self.table.is_none() && !self.actions.is_empty()
    }

    #[must_use]
    pub fn group_titles(&self) -> Vec<&str> {
        self.groups
            .iter()
            .map(|group| group.title.as_str())
            .collect()
    }

    /// Where a row navigates to, if anywhere.
    #[must_use]
    pub fn link(&self, group: &str, label: &str) -> Option<&NodeId> {
        self.groups
            .iter()
            .find(|candidate| candidate.title == group)?
            .rows
            .iter()
            .find(|row| row.label == label)?
            .link
            .as_ref()
    }
}

fn row(label: &str, value: impl Into<String>) -> DetailRow {
    DetailRow {
        label: label.to_owned(),
        value: value.into(),
        link: None,
    }
}

/// A row that selects another object in the tree when activated.
fn link_row(label: &str, value: impl Into<String>, target: NodeId) -> DetailRow {
    DetailRow {
        link: Some(target),
        ..row(label, value)
    }
}

fn group(title: &str, rows: Vec<DetailRow>) -> DetailGroup {
    DetailGroup {
        title: title.to_owned(),
        rows,
    }
}

/// Pretty-print inspect output for the raw expander.
#[must_use]
pub fn raw_json(value: Option<&serde_json::Value>) -> Option<String> {
    value.map(|value| serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()))
}

/// The root node: the local Docker environment, as probed at startup, led by the
/// container table.
#[must_use]
pub fn environment(
    environment: &EnvironmentSummary,
    resolved: &Resolved,
    cx: &Context<'_>,
) -> DetailPage {
    let running = count_running(cx.containers);

    let mut groups = vec![
        connection_group(environment, resolved),
        host_group(environment),
        runtime_group(environment),
        contents_group(environment),
        footprint_group(cx, Some(environment.memory_total)),
    ];

    if !environment.security_options.is_empty() {
        groups.push(group(
            "Security",
            environment
                .security_options
                .iter()
                .map(|option| row("Option", option.clone()))
                .collect(),
        ));
    }

    // Warnings last so they sit closest to the raw output, but always present when the
    // daemon has something to say: §8 says surface these rather than hide them.
    if !environment.warnings.is_empty() {
        groups.push(group(
            "Warnings",
            environment
                .warnings
                .iter()
                .map(|warning| row("Warning", warning.clone()))
                .collect(),
        ));
    }

    DetailPage {
        title: if environment.name.trim().is_empty() {
            "Docker".to_owned()
        } else {
            environment.name.clone()
        },
        subtitle: Some(format!(
            "{} \u{2014} {}",
            resolved.endpoint, resolved.source
        )),
        groups,
        table: Some(table::process_list(cx.containers, cx.now, cx.show_stopped)),
        // The containers are what a person opens this application to look at; the
        // daemon's own metadata is reference material below them.
        table_first: true,
        table_filter: Some(running_filter(cx, running)),
        table_summary: None,
        raw: raw_json(cx.raw),
        actions: action::for_environment(cx.containers, cx.images),
    }
}

/// The running-only / everything toggle, shared by the two pages that list containers so
/// the same question is asked the same way in both.
fn running_filter(cx: &Context<'_>, running: usize) -> TableFilter {
    TableFilter {
        kind: FilterKind::StoppedContainers,
        showing_all: cx.show_stopped,
        narrow_label: format!("Running ({running})"),
        all_label: format!("All ({})", cx.containers.len()),
        // Sized to the running ones whichever view is showing, as version 2 decided: the
        // stopped ones are reached by dragging the divider rather than by opening onto
        // a table of them.
        visible_rows: table::visible_rows(running),
    }
}

fn connection_group(environment: &EnvironmentSummary, resolved: &Resolved) -> DetailGroup {
    group(
        "Connection",
        vec![
            row("Endpoint", resolved.endpoint.to_string()),
            row("Selected by", resolved.source.to_string()),
            row(
                "Server version",
                text_or_unknown(&environment.server_version),
            ),
            row("API version", text_or_unknown(&environment.api_version)),
            row(
                "Minimum API version",
                environment
                    .min_api_version
                    .clone()
                    .unwrap_or_else(|| "unknown".to_owned()),
            ),
            row("Rootless", if environment.rootless { "yes" } else { "no" }),
        ],
    )
}

fn host_group(environment: &EnvironmentSummary) -> DetailGroup {
    group(
        "Host",
        vec![
            row("Name", text_or_unknown(&environment.name)),
            row(
                "Operating system",
                text_or_unknown(&environment.operating_system),
            ),
            row(
                "Platform",
                format!(
                    "{}/{}",
                    text_or_unknown(&environment.os_type),
                    text_or_unknown(&environment.architecture)
                ),
            ),
            row("Kernel", text_or_unknown(&environment.kernel_version)),
            row("CPUs", environment.cpus.to_string()),
            row("Memory", bytes(environment.memory_total)),
        ],
    )
}

fn runtime_group(environment: &EnvironmentSummary) -> DetailGroup {
    group(
        "Runtime",
        vec![
            row(
                "Storage driver",
                text_or_unknown(&environment.storage_driver),
            ),
            row(
                "Logging driver",
                text_or_unknown(&environment.logging_driver),
            ),
            row(
                "cgroup version",
                text_or_unknown(&environment.cgroup_version),
            ),
            row("cgroup driver", text_or_unknown(&environment.cgroup_driver)),
            row(
                "Root directory",
                text_or_unknown(&environment.docker_root_dir),
            ),
        ],
    )
}

fn contents_group(environment: &EnvironmentSummary) -> DetailGroup {
    group(
        "Contents",
        vec![
            row("Images", environment.images.to_string()),
            row("Containers", environment.containers_total.to_string()),
            row("Running", environment.containers_running.to_string()),
            row("Paused", environment.containers_paused.to_string()),
            row("Stopped", environment.containers_stopped.to_string()),
        ],
    )
}

/// What the daemon is costing the machine: memory now, disk since it was installed.
///
/// The disk rows come from the daemon's own accounting rather than from the listings,
/// because a layer shared by two images is one copy on disk and the listings cannot
/// know that. They are left out entirely on a daemon too old to itemise.
fn footprint_group(cx: &Context<'_>, host_memory: Option<i64>) -> DetailGroup {
    let mut rows = vec![row(
        "Memory in use",
        memory_in_use(cx.containers, cx.stats, host_memory),
    )];

    if cx.disk.total_size().is_some() {
        rows.push(disk_row("Images", cx.disk.images));
        rows.push(disk_row("Containers", cx.disk.containers));
        rows.push(disk_row("Volumes", cx.disk.volumes));
        rows.push(disk_row("Build cache", cx.disk.build_cache));
        if let Some(total) = cx.disk.total_size() {
            rows.push(row("Total on disk", bytes(total)));
        }
    }

    group("Footprint", rows)
}

/// One category's disk row: its size, and what a prune of it would give back.
fn disk_row(label: &str, category: Option<DiskCategory>) -> DetailRow {
    let Some(category) = category else {
        return row(label, "unknown");
    };

    if category.reclaimable > 0 {
        row(
            label,
            format!(
                "{} ({} reclaimable)",
                bytes(category.size),
                bytes(category.reclaimable)
            ),
        )
    } else {
        row(label, bytes(category.size))
    }
}

/// Memory across the containers that are executing, as the summaries phrase it.
///
/// The count of containers is part of the figure: a total that silently left one out
/// would read as the machine's, and it would be wrong.
fn memory_in_use(
    containers: &[ContainerSummary],
    stats: &StatsIndex,
    host_memory: Option<i64>,
) -> String {
    let total = stats.running_total(containers);

    if total.is_empty() {
        return if total.is_partial() {
            "not measured yet".to_owned()
        } else {
            "nothing running".to_owned()
        };
    }

    let held = match host_memory.filter(|memory| *memory > 0) {
        Some(memory) => share(total.bytes, memory),
        None => bytes(total.bytes),
    };

    let across = if total.measured == 1 {
        format!("{held} in 1 container")
    } else {
        format!("{held} across {} containers", total.measured)
    };

    if total.is_partial() {
        format!("{across}, {} not measured", total.unmeasured)
    } else {
        across
    }
}
/// The Images node: every image as a table, with a summary beneath it.
#[must_use]
pub fn images(cx: &Context<'_>) -> DetailPage {
    let images = cx.images;
    let total_size: i64 = images.iter().map(|image| image.size).sum();
    let untagged = images.iter().filter(|image| is_untagged(image)).count();
    // Counted from the container listing: the daemon reports -1 in the image listing
    // unless it was asked to compute the figure, which is expensive.
    let in_use = images
        .iter()
        .filter(|image| !relations::containers_of(image, cx.containers).is_empty())
        .count();
    let largest = images.iter().max_by_key(|image| image.size).map_or_else(
        || "none".to_owned(),
        |image| format!("{} ({})", image_label(image), bytes(image.size)),
    );

    let mut summary = vec![
        row("Images", images.len().to_string()),
        // Layers are shared, so this is an upper bound rather than what is on disk.
        row("Combined size", bytes(total_size)),
    ];

    // The daemon's own figure, which counts a shared layer once. Absent on a daemon too
    // old to itemise `/system/df`, in which case the upper bound above is all there is.
    if let Some(disk) = cx.disk.images {
        summary.push(row("On disk", bytes(disk.size)));
        summary.push(row("Reclaimable", bytes(disk.reclaimable)));
    }

    summary.push(row("Largest", largest));
    summary.push(row("Untagged", untagged.to_string()));
    summary.push(row("Used by containers", in_use.to_string()));

    DetailPage {
        title: "Images".to_owned(),
        subtitle: Some("Images available on the local device".to_owned()),
        groups: vec![group("Summary", summary)],
        table: Some(table::images(
            images,
            cx.containers,
            cx.now,
            cx.show_untagged,
        )),
        // The images are what this page is for; the summary is reference material below
        // them, as on the environment page.
        table_first: true,
        table_filter: Some(TableFilter {
            kind: FilterKind::UntaggedImages,
            showing_all: cx.show_untagged,
            narrow_label: format!("Tagged ({})", images.len() - untagged),
            all_label: format!("All ({})", images.len()),
            visible_rows: table::visible_rows(if cx.show_untagged {
                images.len()
            } else {
                images.len() - untagged
            }),
        }),
        table_summary: None,
        raw: None,
        actions: Vec::new(),
    }
}

/// The Containers node: every container as a table, with a summary beneath it.
#[must_use]
pub fn containers(cx: &Context<'_>) -> DetailPage {
    let containers = cx.containers;
    let count = |state: &ContainerState| {
        containers
            .iter()
            .filter(|container| &container.state == state)
            .count()
            .to_string()
    };
    let other = containers
        .iter()
        .filter(|container| {
            !matches!(
                container.state,
                ContainerState::Running
                    | ContainerState::Paused
                    | ContainerState::Exited
                    | ContainerState::Created
            )
        })
        .count();

    DetailPage {
        title: "Containers".to_owned(),
        subtitle: Some("Containers on the local device, running and stopped".to_owned()),
        groups: vec![group(
            "Summary",
            vec![
                row("Containers", containers.len().to_string()),
                row("Running", count(&ContainerState::Running)),
                row("Paused", count(&ContainerState::Paused)),
                row("Exited", count(&ContainerState::Exited)),
                row("Created", count(&ContainerState::Created)),
                row("Other", other.to_string()),
            ],
        )],
        table: Some(table::containers(
            containers,
            cx.stats,
            cx.now,
            cx.show_stopped,
        )),
        table_first: true,
        table_filter: Some(running_filter(cx, count_running(containers))),
        // Beside the table rather than in the summary below it: this totals the Memory
        // column, and is read against it. The host's own total is not on this page, so
        // the figure stands alone.
        table_summary: Some(format!(
            "Memory in use: {}",
            memory_in_use(containers, cx.stats, None)
        )),
        raw: None,
        actions: Vec::new(),
    }
}

fn count_running(containers: &[ContainerSummary]) -> usize {
    containers
        .iter()
        .filter(|container| container.state.is_active())
        .count()
}

/// One image, with its relationships to containers and to other images.
#[must_use]
pub fn image(image: &ImageSummary, cx: &Context<'_>) -> DetailPage {
    let mut groups = vec![
        group(
            "Identity",
            vec![
                row("Tags", list_or_none(&image.repo_tags)),
                row("ID", short_id(&image.id)),
                row("Digests", list_or_none(&image.repo_digests)),
            ],
        ),
        group("Storage", storage_rows(image, cx.now, cx.offset)),
        using_group(image, cx),
    ];

    if let Some(related) = related_images_group(image, cx) {
        groups.push(related);
    }
    if let Some(labels) = label_group(&image.labels) {
        groups.push(labels);
    }

    DetailPage {
        title: image_label(image),
        // The title is already the ID when untagged; repeating it says nothing.
        subtitle: if is_untagged(image) {
            Some("untagged".to_owned())
        } else {
            Some(short_id(&image.id))
        },
        groups,
        table: None,
        table_first: false,
        table_filter: None,
        table_summary: None,
        raw: raw_json(cx.raw),
        actions: action::for_image(image, cx.containers),
    }
}

/// The containers built from an image, each one navigable.
fn using_group(image: &ImageSummary, cx: &Context<'_>) -> DetailGroup {
    let using = relations::containers_of(image, cx.containers);
    if using.is_empty() {
        return group("Used by", vec![row("Containers", "none")]);
    }

    group(
        "Used by",
        using
            .into_iter()
            .map(|container| {
                link_row(
                    &container_label(container),
                    container.state.label(),
                    NodeId::Container(container.id.clone()),
                )
            })
            .collect(),
    )
}

/// Derivation, reconstructed from shared layer prefixes rather than from `Parent`,
/// which `BuildKit` leaves empty. Omitted entirely when nothing relates.
fn related_images_group(image: &ImageSummary, cx: &Context<'_>) -> Option<DetailGroup> {
    let own_depth = cx.layers.get(&image.id).map_or(0, <[String]>::len);
    let mut rows = Vec::new();

    if let Some(base) = relations::base_of(image, cx.images, cx.layers) {
        let shared = cx.layers.get(&base.id).map_or(0, <[String]>::len);
        rows.push(link_row(
            &image_label(base),
            format!(
                "the image this was built FROM \u{2014} {shared} of these {own_depth} layers come from it"
            ),
            NodeId::Image(base.id.clone()),
        ));
    }

    for child in relations::derived_from(image, cx.images, cx.layers) {
        let child_depth = cx.layers.get(&child.id).map_or(0, <[String]>::len);
        rows.push(link_row(
            &image_label(child),
            format!(
                "built FROM this image \u{2014} {}",
                layers_added(child_depth.saturating_sub(own_depth))
            ),
            NodeId::Image(child.id.clone()),
        ));
    }

    for twin in relations::same_layers_as(image, cx.images, cx.layers) {
        rows.push(link_row(
            &image_label(twin),
            "identical layers, different metadata",
            NodeId::Image(twin.id.clone()),
        ));
    }

    (!rows.is_empty()).then(|| group("Related images", rows))
}

/// One container, with the image it runs and its siblings.
#[must_use]
pub fn container(container: &ContainerSummary, cx: &Context<'_>) -> DetailPage {
    let ports: Vec<String> = container.ports.iter().map(port).collect();
    let mounts: Vec<String> = container
        .mounts
        .iter()
        .map(|mount| {
            format!(
                "{} \u{2192} {}{}",
                text_or_unknown(&mount.source),
                text_or_unknown(&mount.destination),
                if mount.read_write { "" } else { " (read-only)" }
            )
        })
        .collect();

    let mut groups = vec![
        group(
            "Identity",
            vec![
                row("Name", list_or_none(&container.names)),
                row("ID", short_id(&container.id)),
                row("Image", text_or_unknown(&container.image)),
                row("Image ID", short_id(&container.image_id)),
                row("Command", text_or_unknown(&container.command)),
                row("Created", instant(container.created, cx.now, cx.offset)),
            ],
        ),
        group(
            "State",
            vec![
                row("State", container.state.label()),
                row("Status", text_or_unknown(&container.status)),
            ],
        ),
        image_group(container, cx),
        group(
            "Networking",
            vec![
                row("Ports", list_or_none(&ports)),
                row("Networks", list_or_none(&container.networks)),
            ],
        ),
        group("Storage", vec![row("Mounts", list_or_none(&mounts))]),
    ];

    if let Some(memory) = memory_group(container, cx) {
        groups.push(memory);
    }
    if let Some(siblings) = siblings_group(container, cx) {
        groups.push(siblings);
    }
    if let Some(labels) = label_group(&container.labels) {
        groups.push(labels);
    }

    DetailPage {
        title: container_label(container),
        subtitle: Some(format!(
            "{} \u{2014} {}",
            container.state.label(),
            text_or_unknown(&container.image)
        )),
        groups,
        table: None,
        table_first: false,
        table_filter: None,
        table_summary: None,
        raw: raw_json(cx.raw),
        actions: action::for_container(container, cx.images),
    }
}

/// What this container is holding, for one that is executing.
///
/// Nothing for a stopped container: it holds nothing, and a row saying so on every
/// stopped page would be noise. A running one that has not been sampled yet says so,
/// rather than silently dropping the group the user has learned to look for.
fn memory_group(container: &ContainerSummary, cx: &Context<'_>) -> Option<DetailGroup> {
    if !container.state.is_active() {
        return None;
    }

    let Some(stats) = cx
        .stats
        .get(&container.id)
        .filter(|stats| stats.has_memory())
    else {
        return Some(group("Memory", vec![row("In use", "not measured yet")]));
    };

    // The limit is the host's own memory when the container is unconstrained, which is
    // how `docker stats` reports it too.
    Some(group(
        "Memory",
        vec![row("In use", share(stats.memory_usage, stats.memory_limit))],
    ))
}

/// The image a container runs, and — when they have diverged — the different image its
/// reference now names. A container relates to two images exactly when a later pull or
/// build moved the tag out from under it.
fn image_group(container: &ContainerSummary, cx: &Context<'_>) -> DetailGroup {
    let mut rows = Vec::new();

    match relations::running_image(container, cx.images) {
        Some(running) => rows.push(link_row(
            &image_label(running),
            "the image this container is running",
            NodeId::Image(running.id.clone()),
        )),
        None => rows.push(row(
            "Running image",
            format!(
                "{} \u{2014} no longer present",
                short_id(&container.image_id)
            ),
        )),
    }

    if relations::tag_has_moved(container, cx.images)
        && let Some(tagged) = relations::tagged_image(container, cx.images)
    {
        rows.push(link_row(
            &image_label(tagged),
            format!(
                "what {} refers to now \u{2014} this container predates it",
                container.image
            ),
            NodeId::Image(tagged.id.clone()),
        ));
    }

    group("Image", rows)
}

/// Other containers built from the same image.
fn siblings_group(container: &ContainerSummary, cx: &Context<'_>) -> Option<DetailGroup> {
    let siblings: Vec<&ContainerSummary> = cx
        .containers
        .iter()
        .filter(|candidate| {
            candidate.id != container.id && candidate.image_id == container.image_id
        })
        .collect();

    (!siblings.is_empty()).then(|| {
        group(
            "Others from the same image",
            siblings
                .into_iter()
                .map(|sibling| {
                    link_row(
                        &container_label(sibling),
                        sibling.state.label(),
                        NodeId::Container(sibling.id.clone()),
                    )
                })
                .collect(),
        )
    })
}

fn layers_added(count: usize) -> String {
    if count == 1 {
        "adds 1 layer".to_owned()
    } else {
        format!("adds {count} layers")
    }
}

/// The daemon reports a negative shared size unless it was asked to compute one, which
/// is expensive. Omit the row rather than showing a placeholder.
fn storage_rows(image: &ImageSummary, now: i64, offset: chrono::FixedOffset) -> Vec<DetailRow> {
    let mut rows = vec![row("Size", bytes(image.size))];
    if image.shared_size >= 0 {
        rows.push(row("Shared size", bytes(image.shared_size)));
    }
    rows.push(row("Created", instant(image.created, now, offset)));
    rows
}

fn label_group(labels: &std::collections::BTreeMap<String, String>) -> Option<DetailGroup> {
    if labels.is_empty() {
        return None;
    }
    Some(group(
        "Labels",
        labels
            .iter()
            .map(|(key, value)| row(key, value.clone()))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::endpoint::{Endpoint, EndpointSource};
    use crate::engine::{MountSummary, PortMapping};
    use chrono::FixedOffset;
    use std::path::PathBuf;

    const NOW: i64 = 1_782_231_445;

    fn utc() -> FixedOffset {
        FixedOffset::east_opt(0).expect("UTC is a valid offset")
    }

    fn resolved() -> Resolved {
        Resolved {
            endpoint: Endpoint::Unix(PathBuf::from("/var/run/docker.sock")),
            source: EndpointSource::RootfulSocket,
        }
    }

    /// Owns the slices a [`Context`] borrows, so tests can build one in a line.
    struct World {
        images: Vec<ImageSummary>,
        containers: Vec<ContainerSummary>,
        layers: LayerIndex,
        stats: StatsIndex,
        disk: DiskUsage,
        raw: Option<serde_json::Value>,
        show_stopped: bool,
        show_untagged: bool,
    }

    impl Default for World {
        fn default() -> Self {
            Self {
                images: Vec::new(),
                containers: Vec::new(),
                layers: LayerIndex::new(),
                stats: StatsIndex::new(),
                disk: DiskUsage::default(),
                raw: None,
                // Matches Settings::default, so tests exercise what a fresh install does.
                show_stopped: true,
                show_untagged: true,
            }
        }
    }

    impl World {
        fn showing_running_only(mut self) -> Self {
            self.show_stopped = false;
            self
        }

        fn showing_tagged_only(mut self) -> Self {
            self.show_untagged = false;
            self
        }

        fn with_images(mut self, images: Vec<ImageSummary>) -> Self {
            self.images = images;
            self
        }

        fn with_containers(mut self, containers: Vec<ContainerSummary>) -> Self {
            self.containers = containers;
            self
        }

        fn with_layers(mut self, image_id: &str, layers: &[&str]) -> Self {
            self.layers.insert(
                image_id,
                layers.iter().map(|layer| (*layer).to_owned()).collect(),
            );
            self
        }

        fn with_stats(mut self, id: &str, usage: i64) -> Self {
            self.stats.insert(crate::engine::ContainerStats {
                id: id.to_owned(),
                memory_usage: usage,
                memory_limit: 8_000_000_000,
            });
            self
        }

        fn with_disk(mut self, disk: DiskUsage) -> Self {
            self.disk = disk;
            self
        }

        fn with_raw(mut self, raw: serde_json::Value) -> Self {
            self.raw = Some(raw);
            self
        }

        fn context(&self) -> Context<'_> {
            Context {
                images: &self.images,
                containers: &self.containers,
                layers: &self.layers,
                stats: &self.stats,
                disk: &self.disk,
                raw: self.raw.as_ref(),
                now: NOW,
                offset: utc(),
                show_stopped: self.show_stopped,
                show_untagged: self.show_untagged,
            }
        }
    }

    fn environment_summary() -> EnvironmentSummary {
        EnvironmentSummary {
            name: "workstation".to_owned(),
            server_version: "29.6.2".to_owned(),
            api_version: "1.55".to_owned(),
            min_api_version: Some("1.24".to_owned()),
            os_type: "linux".to_owned(),
            architecture: "amd64".to_owned(),
            operating_system: "Debian GNU/Linux 13 (trixie)".to_owned(),
            kernel_version: "6.12.90".to_owned(),
            storage_driver: "overlayfs".to_owned(),
            logging_driver: "json-file".to_owned(),
            cgroup_version: "2".to_owned(),
            cgroup_driver: "systemd".to_owned(),
            rootless: false,
            cpus: 32,
            memory_total: 64_960_110_592,
            docker_root_dir: "/var/lib/docker".to_owned(),
            containers_total: 5,
            containers_running: 1,
            containers_paused: 0,
            containers_stopped: 4,
            images: 16,
            security_options: vec!["name=seccomp,profile=builtin".to_owned()],
            warnings: vec![],
        }
    }

    fn sample_image() -> ImageSummary {
        ImageSummary {
            id: "sha256:dff9997d956e5b7117ff96819a213cc4f80754c8".to_owned(),
            repo_tags: vec!["pub-sub-tui:local".to_owned()],
            repo_digests: vec!["pub-sub-tui@sha256:dff9997d".to_owned()],
            created: 1_782_058_645,
            size: 164_231_172,
            shared_size: 1000,
            containers: 2,
            labels: [("maintainer".to_owned(), "dave".to_owned())]
                .into_iter()
                .collect(),
        }
    }

    fn sample_container() -> ContainerSummary {
        ContainerSummary {
            id: "13ef39df585fa5ea8df9325dffdc7c18".to_owned(),
            names: vec!["web".to_owned()],
            image: "nginx:1.27".to_owned(),
            image_id: "sha256:abcdef0123456789".to_owned(),
            command: "nginx -g daemon off;".to_owned(),
            created: 1_782_058_645,
            state: ContainerState::Running,
            status: "Up 2 hours".to_owned(),
            ports: vec![PortMapping {
                ip: Some("0.0.0.0".to_owned()),
                private_port: 80,
                public_port: Some(8080),
                protocol: "tcp".to_owned(),
            }],
            mounts: vec![MountSummary {
                kind: "bind".to_owned(),
                source: "/srv/www".to_owned(),
                destination: "/usr/share/nginx/html".to_owned(),
                read_write: false,
            }],
            networks: vec!["bridge".to_owned()],
            labels: [("com.docker.compose.project".to_owned(), "site".to_owned())]
                .into_iter()
                .collect(),
        }
    }

    fn named_image(id: &str, tag: &str) -> ImageSummary {
        ImageSummary {
            id: format!("sha256:{id}"),
            repo_tags: if tag.is_empty() {
                vec![]
            } else {
                vec![tag.to_owned()]
            },
            ..ImageSummary::default()
        }
    }

    fn named_container(name: &str, reference: &str, image_id: &str) -> ContainerSummary {
        ContainerSummary {
            id: format!("id-{name}"),
            names: vec![name.to_owned()],
            image: reference.to_owned(),
            image_id: format!("sha256:{image_id}"),
            state: ContainerState::Running,
            ..ContainerSummary::default()
        }
    }

    #[test]
    fn the_environment_page_says_where_it_connected_and_why() {
        let page = environment(
            &environment_summary(),
            &resolved(),
            &World::default().context(),
        );

        assert_eq!(
            page.value("Connection", "Endpoint"),
            Some("unix:///var/run/docker.sock")
        );
        assert_eq!(
            page.value("Connection", "Selected by"),
            Some("rootful socket")
        );
        assert!(
            page.subtitle
                .as_deref()
                .expect("subtitle")
                .contains("/var/run/docker.sock")
        );
    }

    #[test]
    fn the_environment_page_reports_the_capability_probe() {
        let page = environment(
            &environment_summary(),
            &resolved(),
            &World::default().context(),
        );

        assert_eq!(page.value("Connection", "API version"), Some("1.55"));
        assert_eq!(page.value("Connection", "Rootless"), Some("no"));
        assert_eq!(page.value("Runtime", "Storage driver"), Some("overlayfs"));
        assert_eq!(page.value("Runtime", "cgroup version"), Some("2"));
        assert_eq!(page.value("Host", "Platform"), Some("linux/amd64"));
        assert_eq!(page.value("Host", "Memory"), Some("65.0 GB"));
        assert_eq!(page.value("Contents", "Images"), Some("16"));
    }

    #[test]
    fn a_clean_environment_has_no_warnings_group() {
        let page = environment(
            &environment_summary(),
            &resolved(),
            &World::default().context(),
        );

        assert!(!page.group_titles().contains(&"Warnings"));
    }

    #[test]
    fn warnings_get_their_own_group_when_the_daemon_reports_any() {
        let mut summary = environment_summary();
        summary.warnings = vec!["No swap limit support".to_owned()];

        let page = environment(&summary, &resolved(), &World::default().context());

        assert!(page.group_titles().contains(&"Warnings"));
        assert_eq!(
            page.value("Warnings", "Warning"),
            Some("No swap limit support")
        );
    }

    #[test]
    fn a_rootless_daemon_is_stated_plainly() {
        let mut summary = environment_summary();
        summary.rootless = true;

        assert_eq!(
            environment(&summary, &resolved(), &World::default().context())
                .value("Connection", "Rootless"),
            Some("yes")
        );
    }

    #[test]
    fn missing_environment_fields_read_as_unknown() {
        let page = environment(
            &EnvironmentSummary::default(),
            &resolved(),
            &World::default().context(),
        );

        assert_eq!(page.value("Connection", "Server version"), Some("unknown"));
        assert_eq!(
            page.value("Connection", "Minimum API version"),
            Some("unknown")
        );
        assert_eq!(page.title, "Docker");
    }

    #[test]
    fn the_images_page_summarises_the_collection() {
        let mut small = sample_image();
        small.id = "sha256:small".to_owned();
        small.size = 1000;
        small.repo_tags = vec!["small:1".to_owned()];

        let world = World::default()
            .with_images(vec![sample_image(), small])
            .with_containers(vec![named_container(
                "web",
                "pub-sub-tui:local",
                "dff9997d956e5b7117ff96819a213cc4f80754c8",
            )]);
        let page = images(&world.context());

        assert_eq!(page.value("Summary", "Images"), Some("2"));
        assert_eq!(page.value("Summary", "Combined size"), Some("164.2 MB"));
        assert_eq!(
            page.value("Summary", "Largest"),
            Some("pub-sub-tui:local (164.2 MB)")
        );
        assert_eq!(page.value("Summary", "Used by containers"), Some("1"));
    }

    #[test]
    fn the_images_page_counts_untagged_images() {
        let mut untagged = sample_image();
        untagged.id = "sha256:untagged".to_owned();
        untagged.repo_tags = vec![];
        let mut placeholder = sample_image();
        placeholder.id = "sha256:placeholder".to_owned();
        placeholder.repo_tags = vec![crate::model::format::UNTAGGED.to_owned()];

        let world = World::default().with_images(vec![sample_image(), untagged, placeholder]);

        assert_eq!(
            images(&world.context()).value("Summary", "Untagged"),
            Some("2")
        );
    }

    #[test]
    fn an_empty_images_page_is_still_coherent() {
        let page = images(&World::default().context());

        assert_eq!(page.value("Summary", "Images"), Some("0"));
        assert_eq!(page.value("Summary", "Combined size"), Some("0 B"));
        assert_eq!(page.value("Summary", "Largest"), Some("none"));
    }

    #[test]
    fn the_environment_page_leads_with_the_container_table() {
        let world = World::default().with_containers(vec![sample_container()]);

        let page = environment(&environment_summary(), &resolved(), &world.context());
        let table = page.table.expect("the environment page has a table");

        assert!(page.table_first, "the containers come before the metadata");
        assert_eq!(
            table.column_titles(),
            vec![
                "Container ID",
                "Image",
                "Command",
                "Created",
                "Status",
                "Ports",
                "Names"
            ]
        );
        assert_eq!(table.cell(0, "Names"), Some("web"));
    }

    #[test]
    fn the_environment_table_offers_a_filter_labelled_with_both_counts() {
        let mut exited = sample_container();
        exited.id = "exited".to_owned();
        exited.state = ContainerState::Exited;
        let world = World::default().with_containers(vec![sample_container(), exited]);

        let filter = environment(&environment_summary(), &resolved(), &world.context())
            .table_filter
            .expect("the filter toggle is offered");

        assert!(filter.showing_all);
        assert_eq!(filter.narrow_label, "Running (1)");
        assert_eq!(filter.all_label, "All (2)");
        // One running container, so the table takes the floor rather than one row.
        assert_eq!(filter.visible_rows, crate::model::table::MIN_VISIBLE_ROWS);
    }

    #[test]
    fn the_table_is_sized_by_the_running_containers_not_by_the_whole_list() {
        let containers: Vec<ContainerSummary> = (0..30)
            .map(|index| {
                let mut container = sample_container();
                container.id = format!("c{index}");
                container.names = vec![format!("c{index}")];
                // A third of them running, the rest stopped.
                container.state = if index % 3 == 0 {
                    ContainerState::Running
                } else {
                    ContainerState::Exited
                };
                container
            })
            .collect();

        let world = World::default().with_containers(containers);
        let filter = environment(&environment_summary(), &resolved(), &world.context())
            .table_filter
            .expect("the filter toggle is offered");

        assert_eq!(filter.narrow_label, "Running (10)");
        assert_eq!(filter.all_label, "All (30)");
        assert_eq!(filter.visible_rows, 10, "sized to the running ones only");
    }

    #[test]
    fn the_environment_table_honours_the_running_only_choice() {
        let mut exited = sample_container();
        exited.id = "exited".to_owned();
        exited.names = vec!["sleeper".to_owned()];
        exited.state = ContainerState::Exited;
        let containers = vec![sample_container(), exited];

        let all = World::default().with_containers(containers.clone());
        let all_page = environment(&environment_summary(), &resolved(), &all.context());
        assert_eq!(all_page.table.expect("table").rows.len(), 2);

        let running = World::default()
            .with_containers(containers)
            .showing_running_only();
        let running_page = environment(&environment_summary(), &resolved(), &running.context());
        let table = running_page.table.expect("table");

        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.cell(0, "Names"), Some("web"));
        assert!(
            !running_page
                .table_filter
                .expect("the toggle is still offered")
                .showing_all
        );
    }

    #[test]
    fn a_page_listing_one_object_carries_no_filter_toggle() {
        let world = World::default()
            .with_images(vec![sample_image()])
            .with_containers(vec![sample_container()]);
        let cx = world.context();

        assert!(image(&sample_image(), &cx).table_filter.is_none());
        assert!(container(&sample_container(), &cx).table_filter.is_none());
    }

    #[test]
    fn every_listing_page_leads_with_its_table() {
        // The objects are what these pages are for; the summary is reference material
        // below them, and 300 pixels of it before the first row is not a summary.
        let world = World::default().with_containers(vec![sample_container()]);
        let cx = world.context();

        assert!(images(&cx).table_first);
        assert!(containers(&cx).table_first);
        assert_eq!(images(&cx).group_titles(), vec!["Summary"]);
        assert_eq!(containers(&cx).group_titles(), vec!["Summary"]);
    }

    #[test]
    fn the_containers_page_hides_the_stopped_ones_on_request() {
        let mut exited = sample_container();
        exited.id = "exited".to_owned();
        exited.names = vec!["sleeper".to_owned()];
        exited.state = ContainerState::Exited;
        let listing = vec![sample_container(), exited];

        let all = World::default().with_containers(listing.clone());
        assert_eq!(
            containers(&all.context()).table.expect("table").rows.len(),
            2
        );

        let running = World::default()
            .with_containers(listing)
            .showing_running_only();
        let page = containers(&running.context());
        let table = page.table.expect("table");

        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.cell(0, "Container"), Some("web"));

        let filter = page.table_filter.expect("the toggle is still offered");
        assert!(!filter.showing_all);
        assert_eq!(filter.kind, FilterKind::StoppedContainers);
        assert_eq!(filter.narrow_label, "Running (1)");
        assert_eq!(filter.all_label, "All (2)");
    }

    #[test]
    fn the_containers_page_asks_the_same_question_as_the_environment_page() {
        // One preference drives both, so the toggles cannot disagree about what is showing.
        let world = World::default()
            .with_containers(vec![sample_container()])
            .showing_running_only();
        let cx = world.context();

        let overview = containers(&cx).table_filter.expect("filter");
        let environment = environment(&environment_summary(), &resolved(), &cx)
            .table_filter
            .expect("filter");

        assert_eq!(overview.kind, environment.kind);
        assert_eq!(overview.showing_all, environment.showing_all);
    }

    #[test]
    fn the_images_page_hides_the_untagged_ones_on_request() {
        let mut untagged = sample_image();
        untagged.id = "sha256:loose".to_owned();
        untagged.repo_tags = Vec::new();
        let listing = vec![sample_image(), untagged];

        let all = World::default().with_images(listing.clone());
        assert_eq!(images(&all.context()).table.expect("table").rows.len(), 2);

        let tagged = World::default().with_images(listing).showing_tagged_only();
        let page = images(&tagged.context());
        let table = page.table.expect("table");

        assert_eq!(table.rows.len(), 1);
        assert_eq!(table.cell(0, "Image"), Some("pub-sub-tui:local"));

        let filter = page.table_filter.expect("the toggle is offered");
        assert!(!filter.showing_all);
        assert_eq!(filter.kind, FilterKind::UntaggedImages);
        assert_eq!(filter.narrow_label, "Tagged (1)");
        assert_eq!(filter.all_label, "All (2)");
    }

    #[test]
    fn hiding_the_untagged_images_does_not_change_what_the_summary_counts() {
        // The summary describes the collection, not the view of it: a count that fell to
        // zero when the rows were hidden would be reporting the filter back at the user.
        let mut untagged = sample_image();
        untagged.id = "sha256:loose".to_owned();
        untagged.repo_tags = Vec::new();
        let listing = vec![sample_image(), untagged];

        let page = images(
            &World::default()
                .with_images(listing)
                .showing_tagged_only()
                .context(),
        );

        assert_eq!(page.value("Summary", "Images"), Some("2"));
        assert_eq!(page.value("Summary", "Untagged"), Some("1"));
    }

    #[test]
    fn the_overview_pages_carry_a_table_and_the_object_pages_do_not() {
        let world = World::default()
            .with_images(vec![sample_image()])
            .with_containers(vec![sample_container()]);
        let cx = world.context();

        let image_table = images(&cx).table.expect("the images page has a table");
        assert_eq!(image_table.rows.len(), 1);
        assert_eq!(image_table.cell(0, "Image"), Some("pub-sub-tui:local"));

        let container_table = containers(&cx)
            .table
            .expect("the containers page has a table");
        assert_eq!(container_table.cell(0, "Container"), Some("web"));

        assert!(image(&sample_image(), &cx).table.is_none());
        assert!(container(&sample_container(), &cx).table.is_none());
    }

    #[test]
    fn the_containers_page_breaks_down_by_state() {
        let mut exited = sample_container();
        exited.id = "exited".to_owned();
        exited.state = ContainerState::Exited;
        let mut dead = sample_container();
        dead.id = "dead".to_owned();
        dead.state = ContainerState::Dead;

        let world = World::default().with_containers(vec![sample_container(), exited, dead]);
        let page = containers(&world.context());

        assert_eq!(page.value("Summary", "Containers"), Some("3"));
        assert_eq!(page.value("Summary", "Running"), Some("1"));
        assert_eq!(page.value("Summary", "Exited"), Some("1"));
        assert_eq!(page.value("Summary", "Other"), Some("1"));
    }

    #[test]
    fn the_image_page_shows_identity_and_storage() {
        let world = World::default().with_images(vec![sample_image()]);
        let page = image(&sample_image(), &world.context());

        assert_eq!(page.title, "pub-sub-tui:local");
        assert_eq!(page.value("Identity", "ID"), Some("dff9997d956e"));
        assert_eq!(page.value("Identity", "Tags"), Some("pub-sub-tui:local"));
        assert_eq!(page.value("Storage", "Size"), Some("164.2 MB"));
        assert_eq!(
            page.value("Storage", "Created"),
            Some("2026-06-21 16:17:25 (2 days ago)")
        );
        assert_eq!(page.value("Labels", "maintainer"), Some("dave"));
    }

    #[test]
    fn an_uncomputed_shared_size_is_omitted_rather_than_shown_as_unknown() {
        let mut image_summary = sample_image();
        image_summary.shared_size = -1;
        let world = World::default();

        let page = image(&image_summary, &world.context());

        assert_eq!(page.value("Storage", "Shared size"), None);
        assert_eq!(page.value("Storage", "Size"), Some("164.2 MB"));
        assert!(page.value("Storage", "Created").is_some());
    }

    #[test]
    fn a_computed_shared_size_is_shown() {
        assert_eq!(
            image(&sample_image(), &World::default().context()).value("Storage", "Shared size"),
            Some("1.0 kB")
        );
    }

    #[test]
    fn an_untagged_image_page_identifies_the_image_by_id() {
        let mut untagged = sample_image();
        untagged.repo_tags = vec![];
        untagged.repo_digests = vec![];
        untagged.labels.clear();

        let page = image(&untagged, &World::default().context());

        assert_eq!(page.title, "dff9997d956e");
        // Repeating the ID as a subtitle would say nothing; say what it means instead.
        assert_eq!(page.subtitle.as_deref(), Some("untagged"));
        assert_eq!(page.value("Identity", "Tags"), Some("none"));
        assert_eq!(page.value("Identity", "Digests"), Some("none"));
        assert!(!page.group_titles().contains(&"Labels"));
    }

    #[test]
    fn an_image_lists_the_containers_using_it_and_each_one_is_navigable() {
        let nginx = named_image("nginx", "nginx:1.27");
        let world = World::default()
            .with_images(vec![nginx.clone()])
            .with_containers(vec![
                named_container("web", "nginx:1.27", "nginx"),
                named_container("api", "nginx:1.27", "nginx"),
                named_container("db", "postgres:16", "postgres"),
            ]);

        let page = image(&nginx, &world.context());

        assert_eq!(page.value("Used by", "web"), Some("running"));
        assert_eq!(page.value("Used by", "api"), Some("running"));
        assert_eq!(page.value("Used by", "db"), None, "db uses another image");
        assert_eq!(
            page.link("Used by", "web"),
            Some(&NodeId::Container("id-web".to_owned()))
        );
    }

    #[test]
    fn an_unused_image_says_so_rather_than_showing_an_empty_group() {
        let nginx = named_image("nginx", "nginx:1.27");
        let world = World::default().with_images(vec![nginx.clone()]);

        let page = image(&nginx, &world.context());

        assert_eq!(page.value("Used by", "Containers"), Some("none"));
        assert!(page.link("Used by", "Containers").is_none());
    }

    #[test]
    fn an_image_links_to_the_image_it_was_built_from_and_to_its_descendants() {
        let alpine = named_image("alpine", "alpine:3.24");
        let node = named_image("node", "node:22-alpine");
        let webgui = named_image("webgui", "web-gui:latest");
        let world = World::default()
            .with_images(vec![alpine.clone(), node.clone(), webgui])
            .with_layers("sha256:alpine", &["l1"])
            .with_layers("sha256:node", &["l1", "l2", "l3", "l4"])
            .with_layers("sha256:webgui", &["l1", "l2", "l3", "l4", "l5"]);

        let page = image(&node, &world.context());

        assert_eq!(
            page.value("Related images", "alpine:3.24"),
            Some("the image this was built FROM \u{2014} 1 of these 4 layers come from it")
        );
        assert_eq!(
            page.link("Related images", "alpine:3.24"),
            Some(&NodeId::Image("sha256:alpine".to_owned()))
        );
        assert_eq!(
            page.value("Related images", "web-gui:latest"),
            Some("built FROM this image \u{2014} adds 1 layer")
        );
        assert_eq!(
            page.link("Related images", "web-gui:latest"),
            Some(&NodeId::Image("sha256:webgui".to_owned()))
        );
    }

    #[test]
    fn an_image_with_no_relatives_has_no_related_group_at_all() {
        let lonely = named_image("lonely", "lonely:1");
        let world = World::default()
            .with_images(vec![lonely.clone()])
            .with_layers("sha256:lonely", &["l1"]);

        let page = image(&lonely, &world.context());

        assert!(!page.group_titles().contains(&"Related images"));
    }

    #[test]
    fn without_layer_data_the_related_group_is_absent_rather_than_wrong() {
        let alpine = named_image("alpine", "alpine:3.24");
        let node = named_image("node", "node:22-alpine");
        let world = World::default().with_images(vec![alpine, node.clone()]);

        assert!(
            !image(&node, &world.context())
                .group_titles()
                .contains(&"Related images")
        );
    }

    #[test]
    fn the_object_pages_carry_actions_and_the_listing_pages_do_not() {
        let world = World::default();

        // A listing is a way of choosing something, not a thing to act on; the object's
        // own page is where its actions belong.
        assert!(images(&world.context()).actions.is_empty());
        assert!(containers(&world.context()).actions.is_empty());

        assert!(
            !container(&sample_container(), &world.context())
                .actions
                .is_empty()
        );
        assert!(!image(&sample_image(), &world.context()).actions.is_empty());
    }

    #[test]
    fn only_an_object_page_leads_with_its_actions_as_buttons() {
        let stopped = ContainerSummary {
            id: "old".to_owned(),
            names: vec!["old".to_owned()],
            state: ContainerState::Exited,
            ..ContainerSummary::default()
        };
        let world = World::default().with_containers(vec![stopped]);
        let cx = world.context();

        assert!(container(&sample_container(), &cx).shows_action_bar());
        assert!(image(&sample_image(), &cx).shows_action_bar());

        // A listing page acts through its table, and the environment's prunes are on the
        // primary menu, so neither wants a strip of buttons over its table.
        assert!(!containers(&cx).shows_action_bar());
        assert!(!images(&cx).shows_action_bar());
        let overview = environment(&environment_summary(), &resolved(), &cx);
        assert!(!overview.actions.is_empty(), "there is a prune to be had");
        assert!(!overview.shows_action_bar());
    }

    #[test]
    fn the_environment_page_offers_a_prune_only_when_something_is_prunable() {
        let idle = World::default();
        assert!(
            environment(&environment_summary(), &resolved(), &idle.context())
                .actions
                .is_empty(),
            "nothing stopped and nothing untagged means nothing to prune"
        );

        let stopped = ContainerSummary {
            id: "old".to_owned(),
            names: vec!["old".to_owned()],
            state: ContainerState::Exited,
            ..ContainerSummary::default()
        };
        let world = World::default().with_containers(vec![stopped]);

        let actions = environment(&environment_summary(), &resolved(), &world.context()).actions;

        assert!(
            actions
                .iter()
                .any(|offer| offer.action == crate::model::action::Action::PruneContainers)
        );
    }

    #[test]
    fn the_container_page_shows_state_ports_and_mounts() {
        let page = container(&sample_container(), &World::default().context());

        assert_eq!(page.title, "web");
        assert_eq!(page.value("State", "State"), Some("running"));
        assert_eq!(page.value("State", "Status"), Some("Up 2 hours"));
        assert_eq!(
            page.value("Networking", "Ports"),
            Some("0.0.0.0:8080 \u{2192} 80/tcp")
        );
        assert_eq!(page.value("Networking", "Networks"), Some("bridge"));
        assert_eq!(
            page.value("Storage", "Mounts"),
            Some("/srv/www \u{2192} /usr/share/nginx/html (read-only)")
        );
    }

    #[test]
    fn a_container_with_nothing_published_says_none_rather_than_blank() {
        let mut bare = sample_container();
        bare.ports.clear();
        bare.mounts.clear();
        bare.networks.clear();
        bare.names.clear();
        bare.labels.clear();

        let page = container(&bare, &World::default().context());

        assert_eq!(page.value("Networking", "Ports"), Some("none"));
        assert_eq!(page.value("Networking", "Networks"), Some("none"));
        assert_eq!(page.value("Storage", "Mounts"), Some("none"));
        assert_eq!(page.value("Identity", "Name"), Some("none"));
        assert_eq!(page.title, "13ef39df585f");
        assert!(!page.group_titles().contains(&"Labels"));
    }

    #[test]
    fn a_read_write_mount_is_not_annotated() {
        let mut writable = sample_container();
        writable.mounts[0].read_write = true;

        let page = container(&writable, &World::default().context());

        assert_eq!(
            page.value("Storage", "Mounts"),
            Some("/srv/www \u{2192} /usr/share/nginx/html")
        );
    }

    #[test]
    fn a_container_links_to_the_image_it_is_running() {
        let nginx = named_image("nginx", "nginx:1.27");
        let web = named_container("web", "nginx:1.27", "nginx");
        let world = World::default().with_images(vec![nginx]);

        let page = container(&web, &world.context());

        assert_eq!(
            page.value("Image", "nginx:1.27"),
            Some("the image this container is running")
        );
        assert_eq!(
            page.link("Image", "nginx:1.27"),
            Some(&NodeId::Image("sha256:nginx".to_owned()))
        );
    }

    #[test]
    fn a_container_whose_tag_has_moved_reaches_both_images() {
        // nginx:1.27 was pulled again: the tag moved to a new image while this container
        // kept running the old one, which lost its tag in the process.
        let old = named_image("old", "");
        let new = named_image("new", "nginx:1.27");
        let web = named_container("web", "nginx:1.27", "old");
        let world = World::default().with_images(vec![old, new]);

        let page = container(&web, &world.context());

        assert_eq!(
            page.link("Image", "old"),
            Some(&NodeId::Image("sha256:old".to_owned())),
            "the untagged image it is running is reachable by short ID"
        );
        assert_eq!(
            page.value("Image", "nginx:1.27"),
            Some("what nginx:1.27 refers to now \u{2014} this container predates it")
        );
        assert_eq!(
            page.link("Image", "nginx:1.27"),
            Some(&NodeId::Image("sha256:new".to_owned()))
        );
    }

    #[test]
    fn a_container_whose_tag_still_agrees_shows_only_one_image() {
        let nginx = named_image("nginx", "nginx:1.27");
        let web = named_container("web", "nginx:1.27", "nginx");
        let world = World::default().with_images(vec![nginx]);

        let rows = container(&web, &world.context())
            .groups
            .into_iter()
            .find(|group| group.title == "Image")
            .expect("the Image group is always present")
            .rows;

        assert_eq!(rows.len(), 1, "got {rows:?}");
    }

    #[test]
    fn a_container_whose_image_was_deleted_says_so_rather_than_linking_nowhere() {
        let web = named_container("web", "nginx:1.27", "gone");

        let page = container(&web, &World::default().context());

        assert_eq!(
            page.value("Image", "Running image"),
            Some("gone \u{2014} no longer present")
        );
        assert!(page.link("Image", "Running image").is_none());
    }

    #[test]
    fn a_container_lists_its_siblings_from_the_same_image() {
        let web = named_container("web", "nginx:1.27", "nginx");
        let world = World::default()
            .with_images(vec![named_image("nginx", "nginx:1.27")])
            .with_containers(vec![
                web.clone(),
                named_container("api", "nginx:1.27", "nginx"),
                named_container("db", "postgres:16", "postgres"),
            ]);

        let page = container(&web, &world.context());

        assert_eq!(
            page.value("Others from the same image", "api"),
            Some("running")
        );
        assert_eq!(
            page.link("Others from the same image", "api"),
            Some(&NodeId::Container("id-api".to_owned()))
        );
        assert_eq!(
            page.value("Others from the same image", "web"),
            None,
            "not itself"
        );
        assert_eq!(page.value("Others from the same image", "db"), None);
    }

    #[test]
    fn an_only_container_has_no_siblings_group() {
        let web = named_container("web", "nginx:1.27", "nginx");
        let world = World::default().with_containers(vec![web.clone()]);

        assert!(
            !container(&web, &world.context())
                .group_titles()
                .contains(&"Others from the same image")
        );
    }

    #[test]
    fn raw_inspect_output_is_pretty_printed_for_the_expander() {
        let world =
            World::default().with_raw(serde_json::json!({"Id": "abc", "State": {"Running": true}}));

        let page = container(&sample_container(), &world.context());
        let rendered = page.raw.expect("raw output present");

        assert!(rendered.contains("\n  \"Id\": \"abc\""), "got: {rendered}");
    }

    #[test]
    fn pages_without_inspect_output_have_no_raw_section() {
        let world = World::default();

        assert!(image(&sample_image(), &world.context()).raw.is_none());
        assert!(images(&world.context()).raw.is_none());
        assert!(containers(&world.context()).raw.is_none());
    }

    fn disk_usage() -> DiskUsage {
        DiskUsage {
            images: Some(DiskCategory {
                total_count: 16,
                active_count: 4,
                size: 14_783_927_715,
                reclaimable: 12_556_632_188,
            }),
            containers: Some(DiskCategory {
                total_count: 6,
                active_count: 1,
                size: 17_522_688,
                reclaimable: 17_518_592,
            }),
            volumes: Some(DiskCategory::default()),
            build_cache: Some(DiskCategory {
                total_count: 103,
                active_count: 0,
                size: 8_023_805_715,
                reclaimable: 7_704_593_624,
            }),
        }
    }

    #[test]
    fn the_environment_page_reports_what_the_daemon_is_costing_the_machine() {
        let world = World::default()
            .with_containers(vec![
                named_container("web", "nginx:1.27", "abc"),
                named_container("api", "nginx:1.27", "abc"),
            ])
            .with_stats("id-web", 700_000_000)
            .with_stats("id-api", 300_000_000)
            .with_disk(disk_usage());

        let page = environment(&environment_summary(), &resolved(), &world.context());

        assert_eq!(
            page.value("Footprint", "Memory in use"),
            Some("1.0 GB of 65.0 GB (1.5%) across 2 containers")
        );
        assert_eq!(
            page.value("Footprint", "Images"),
            Some("14.8 GB (12.6 GB reclaimable)")
        );
        assert_eq!(
            page.value("Footprint", "Volumes"),
            Some("0 B"),
            "a category with nothing to reclaim says only its size"
        );
        assert_eq!(page.value("Footprint", "Total on disk"), Some("22.8 GB"));
    }

    #[test]
    fn a_running_container_that_has_not_been_sampled_makes_the_total_a_floor() {
        let world = World::default()
            .with_containers(vec![
                named_container("web", "nginx:1.27", "abc"),
                named_container("api", "nginx:1.27", "abc"),
            ])
            .with_stats("id-web", 700_000_000);

        let page = containers(&world.context());

        assert_eq!(
            page.table_summary.as_deref(),
            Some("Memory in use: 700.0 MB in 1 container, 1 not measured")
        );
    }

    #[test]
    fn the_containers_page_totals_the_memory_of_the_ones_that_are_running() {
        let world = World::default()
            .with_containers(vec![
                named_container("web", "nginx:1.27", "abc"),
                named_container("api", "nginx:1.27", "abc"),
            ])
            .with_stats("id-web", 700_000_000)
            .with_stats("id-api", 300_000_000);

        let page = containers(&world.context());

        // Above the table, where it is read against the column it sums, rather than in
        // the summary group below it.
        assert_eq!(
            page.table_summary.as_deref(),
            Some("Memory in use: 1.0 GB across 2 containers")
        );
        assert_eq!(
            page.value("Summary", "Memory in use"),
            None,
            "the figure is stated once"
        );
    }

    #[test]
    fn only_the_page_listing_containers_carries_a_table_summary() {
        let world = World::default()
            .with_containers(vec![named_container("web", "nginx:1.27", "abc")])
            .with_images(vec![sample_image()])
            .with_stats("id-web", 700_000_000);
        let cx = world.context();

        assert!(images(&cx).table_summary.is_none());
        assert!(
            environment(&environment_summary(), &resolved(), &cx)
                .table_summary
                .is_none(),
            "the environment page has the same figure in its Footprint group"
        );
        assert!(container(&sample_container(), &cx).table_summary.is_none());
    }

    #[test]
    fn a_machine_with_nothing_running_says_so_rather_than_reporting_no_memory() {
        let mut stopped = sample_container();
        stopped.state = ContainerState::Exited;
        let world = World::default().with_containers(vec![stopped]);

        let page = containers(&world.context());

        assert_eq!(
            page.table_summary.as_deref(),
            Some("Memory in use: nothing running")
        );
    }

    #[test]
    fn a_daemon_too_old_to_itemise_its_storage_leaves_the_disk_rows_out() {
        // Every category absent: the row would otherwise read "unknown" four times.
        let page = environment(
            &environment_summary(),
            &resolved(),
            &World::default().context(),
        );

        let footprint = page
            .groups
            .iter()
            .find(|group| group.title == "Footprint")
            .expect("the group is still there for the memory row");

        assert_eq!(footprint.rows.len(), 1);
        assert_eq!(footprint.rows[0].label, "Memory in use");
    }

    #[test]
    fn the_images_page_reports_the_disk_the_daemon_actually_spent() {
        let world = World::default()
            .with_images(vec![sample_image()])
            .with_disk(disk_usage());

        let page = images(&world.context());

        // The listing's sum counts a shared layer once per image that carries it; the
        // daemon's figure counts it once.
        assert_eq!(page.value("Summary", "Combined size"), Some("164.2 MB"));
        assert_eq!(page.value("Summary", "On disk"), Some("14.8 GB"));
        assert_eq!(page.value("Summary", "Reclaimable"), Some("12.6 GB"));
    }

    #[test]
    fn the_images_page_falls_back_to_the_listing_when_the_daemon_does_not_itemise() {
        let world = World::default().with_images(vec![sample_image()]);
        let page = images(&world.context());

        assert_eq!(page.value("Summary", "Combined size"), Some("164.2 MB"));
        assert_eq!(page.value("Summary", "On disk"), None);
    }

    #[test]
    fn a_running_containers_page_reports_what_it_is_holding_and_of_what() {
        let world = World::default()
            .with_containers(vec![sample_container()])
            .with_stats(&sample_container().id, 716_800);

        let page = container(&sample_container(), &world.context());

        assert_eq!(
            page.value("Memory", "In use"),
            Some("716.8 kB of 8.0 GB (<0.1%)")
        );
    }

    #[test]
    fn a_running_container_awaiting_its_first_sample_says_so() {
        let world = World::default().with_containers(vec![sample_container()]);
        let page = container(&sample_container(), &world.context());

        assert_eq!(page.value("Memory", "In use"), Some("not measured yet"));
    }

    #[test]
    fn a_stopped_container_is_not_given_a_memory_group_at_all() {
        let mut stopped = sample_container();
        stopped.state = ContainerState::Exited;
        let world = World::default().with_containers(vec![stopped.clone()]);

        let page = container(&stopped, &world.context());

        assert!(!page.group_titles().contains(&"Memory"));
    }
}
