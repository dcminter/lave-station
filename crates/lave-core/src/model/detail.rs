//! What the right-hand pane shows for the current selection.
//!
//! Returned as data rather than widgets: the content is decided and tested here, and
//! the GTK layer only turns rows into `AdwActionRow`s.

use crate::endpoint::Resolved;
use crate::engine::{ContainerState, ContainerSummary, EnvironmentSummary, ImageSummary};

use super::format::{
    bytes, container_label, image_label, instant, is_untagged, list_or_none, port, short_id,
    text_or_unknown,
};
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

/// The running-only / everything toggle offered above a container table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerFilter {
    /// True when stopped containers are included.
    pub showing_all: bool,
    pub running_label: String,
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
    pub table_filter: Option<ContainerFilter>,
    /// Pretty-printed inspect output, shown in a collapsed expander.
    pub raw: Option<String>,
}

/// Everything a detail page needs beyond the object it is describing. Passed as one
/// value so adding context does not ripple through every call site.
pub struct Context<'a> {
    pub images: &'a [ImageSummary],
    pub containers: &'a [ContainerSummary],
    pub layers: &'a LayerIndex,
    pub raw: Option<&'a serde_json::Value>,
    /// Seconds since the Unix epoch, injected so rendering is deterministic.
    pub now: i64,
    pub offset: chrono::FixedOffset,
    /// Whether the environment page's container table includes stopped containers.
    pub show_stopped: bool,
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
    let running = cx
        .containers
        .iter()
        .filter(|container| container.state.is_active())
        .count();

    let mut groups = vec![
        connection_group(environment, resolved),
        host_group(environment),
        runtime_group(environment),
        contents_group(environment),
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
        table_filter: Some(ContainerFilter {
            showing_all: cx.show_stopped,
            running_label: format!("Running ({running})"),
            all_label: format!("All ({})", cx.containers.len()),
            visible_rows: table::visible_rows(running),
        }),
        raw: raw_json(cx.raw),
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

/// The Images node: a summary, then every image as a table.
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

    DetailPage {
        title: "Images".to_owned(),
        subtitle: Some("Images available on the local device".to_owned()),
        groups: vec![group(
            "Summary",
            vec![
                row("Images", images.len().to_string()),
                // Layers are shared, so this is an upper bound rather than disk usage.
                row("Combined size", bytes(total_size)),
                row("Largest", largest),
                row("Untagged", untagged.to_string()),
                row("Used by containers", in_use.to_string()),
            ],
        )],
        table: Some(table::images(images, cx.containers, cx.now)),
        table_first: false,
        table_filter: None,
        raw: None,
    }
}

/// The Containers node: a summary broken down by state, then every container as a table.
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
        table: Some(table::containers(containers, cx.now)),
        table_first: false,
        table_filter: None,
        raw: None,
    }
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
        raw: raw_json(cx.raw),
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
        raw: raw_json(cx.raw),
    }
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
        raw: Option<serde_json::Value>,
        show_stopped: bool,
    }

    impl Default for World {
        fn default() -> Self {
            Self {
                images: Vec::new(),
                containers: Vec::new(),
                layers: LayerIndex::new(),
                raw: None,
                // Matches Settings::default, so tests exercise what a fresh install does.
                show_stopped: true,
            }
        }
    }

    impl World {
        fn showing_running_only(mut self) -> Self {
            self.show_stopped = false;
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

        fn with_raw(mut self, raw: serde_json::Value) -> Self {
            self.raw = Some(raw);
            self
        }

        fn context(&self) -> Context<'_> {
            Context {
                images: &self.images,
                containers: &self.containers,
                layers: &self.layers,
                raw: self.raw.as_ref(),
                now: NOW,
                offset: utc(),
                show_stopped: self.show_stopped,
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
        assert_eq!(filter.running_label, "Running (1)");
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

        assert_eq!(filter.running_label, "Running (10)");
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
    fn only_the_environment_page_carries_a_filter_toggle() {
        let world = World::default()
            .with_images(vec![sample_image()])
            .with_containers(vec![sample_container()]);
        let cx = world.context();

        assert!(images(&cx).table_filter.is_none());
        assert!(containers(&cx).table_filter.is_none());
        assert!(image(&sample_image(), &cx).table_filter.is_none());
    }

    #[test]
    fn the_overview_tables_follow_their_summaries_rather_than_leading() {
        let world = World::default().with_containers(vec![sample_container()]);
        let cx = world.context();

        assert!(!images(&cx).table_first);
        assert!(!containers(&cx).table_first);
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
}
