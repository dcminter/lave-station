//! What the right-hand pane shows for the current selection.
//!
//! Returned as data rather than widgets: the content is decided and tested here, and
//! the GTK layer only turns rows into `AdwActionRow`s.

use crate::endpoint::Resolved;
use crate::engine::{ContainerState, ContainerSummary, EnvironmentSummary, ImageSummary};

use super::format::{UNTAGGED, bytes, instant, list_or_none, port, short_id, text_or_unknown};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailRow {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailGroup {
    pub title: String,
    pub rows: Vec<DetailRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailPage {
    pub title: String,
    pub subtitle: Option<String>,
    pub groups: Vec<DetailGroup>,
    /// Pretty-printed inspect output, shown in a collapsed expander.
    pub raw: Option<String>,
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
}

fn row(label: &str, value: impl Into<String>) -> DetailRow {
    DetailRow {
        label: label.to_owned(),
        value: value.into(),
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

/// The root node: the local Docker environment, as probed at startup.
#[must_use]
pub fn environment(
    environment: &EnvironmentSummary,
    resolved: &Resolved,
    raw: Option<&serde_json::Value>,
) -> DetailPage {
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
        raw: raw_json(raw),
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

/// The Images node: a summary of everything under it.
#[must_use]
pub fn images(images: &[ImageSummary]) -> DetailPage {
    let total_size: i64 = images.iter().map(|image| image.size).sum();
    let untagged = images
        .iter()
        .filter(|image| {
            image
                .repo_tags
                .iter()
                .all(|tag| tag.is_empty() || tag == UNTAGGED)
        })
        .count();
    let in_use = images.iter().filter(|image| image.containers > 0).count();
    let largest = images.iter().max_by_key(|image| image.size).map_or_else(
        || "none".to_owned(),
        |image| {
            format!(
                "{} ({})",
                super::format::image_label(image),
                bytes(image.size)
            )
        },
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
        raw: None,
    }
}

/// The Containers node: a summary broken down by state.
#[must_use]
pub fn containers(containers: &[ContainerSummary]) -> DetailPage {
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
        raw: None,
    }
}

/// One image.
#[must_use]
pub fn image(
    image: &ImageSummary,
    raw: Option<&serde_json::Value>,
    now: i64,
    offset: chrono::FixedOffset,
) -> DetailPage {
    let mut groups = vec![
        group(
            "Identity",
            vec![
                row("Tags", list_or_none(&image.repo_tags)),
                row("ID", short_id(&image.id)),
                row("Digests", list_or_none(&image.repo_digests)),
            ],
        ),
        group("Storage", storage_rows(image, now, offset)),
        group(
            "Usage",
            vec![row("Containers", image.containers.to_string())],
        ),
    ];

    if let Some(labels) = label_group(&image.labels) {
        groups.push(labels);
    }

    DetailPage {
        title: super::format::image_label(image),
        subtitle: Some(short_id(&image.id)),
        groups,
        raw: raw_json(raw),
    }
}

/// One container.
#[must_use]
pub fn container(
    container: &ContainerSummary,
    raw: Option<&serde_json::Value>,
    now: i64,
    offset: chrono::FixedOffset,
) -> DetailPage {
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
                row("Created", instant(container.created, now, offset)),
            ],
        ),
        group(
            "State",
            vec![
                row("State", container.state.label()),
                row("Status", text_or_unknown(&container.status)),
            ],
        ),
        group(
            "Networking",
            vec![
                row("Ports", list_or_none(&ports)),
                row("Networks", list_or_none(&container.networks)),
            ],
        ),
        group("Storage", vec![row("Mounts", list_or_none(&mounts))]),
    ];

    if let Some(labels) = label_group(&container.labels) {
        groups.push(labels);
    }

    DetailPage {
        title: super::format::container_label(container),
        subtitle: Some(format!(
            "{} \u{2014} {}",
            container.state.label(),
            text_or_unknown(&container.image)
        )),
        groups,
        raw: raw_json(raw),
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

    #[test]
    fn the_environment_page_says_where_it_connected_and_why() {
        let page = environment(&environment_summary(), &resolved(), None);

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
        let page = environment(&environment_summary(), &resolved(), None);

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
        let page = environment(&environment_summary(), &resolved(), None);

        assert!(!page.group_titles().contains(&"Warnings"));
    }

    #[test]
    fn warnings_get_their_own_group_when_the_daemon_reports_any() {
        let mut summary = environment_summary();
        summary.warnings = vec!["No swap limit support".to_owned()];

        let page = environment(&summary, &resolved(), None);

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
            environment(&summary, &resolved(), None).value("Connection", "Rootless"),
            Some("yes")
        );
    }

    #[test]
    fn missing_environment_fields_read_as_unknown() {
        let page = environment(&EnvironmentSummary::default(), &resolved(), None);

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
        small.size = 1000;
        small.repo_tags = vec!["small:1".to_owned()];
        small.containers = 0;

        let page = images(&[sample_image(), small]);

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
        untagged.repo_tags = vec![];
        let mut placeholder = sample_image();
        placeholder.repo_tags = vec![UNTAGGED.to_owned()];

        let page = images(&[sample_image(), untagged, placeholder]);

        assert_eq!(page.value("Summary", "Untagged"), Some("2"));
    }

    #[test]
    fn an_empty_images_page_is_still_coherent() {
        let page = images(&[]);

        assert_eq!(page.value("Summary", "Images"), Some("0"));
        assert_eq!(page.value("Summary", "Combined size"), Some("0 B"));
        assert_eq!(page.value("Summary", "Largest"), Some("none"));
    }

    #[test]
    fn the_containers_page_breaks_down_by_state() {
        let mut exited = sample_container();
        exited.state = ContainerState::Exited;
        let mut dead = sample_container();
        dead.state = ContainerState::Dead;

        let page = containers(&[sample_container(), exited, dead]);

        assert_eq!(page.value("Summary", "Containers"), Some("3"));
        assert_eq!(page.value("Summary", "Running"), Some("1"));
        assert_eq!(page.value("Summary", "Exited"), Some("1"));
        assert_eq!(page.value("Summary", "Other"), Some("1"));
    }

    #[test]
    fn the_image_page_shows_identity_storage_and_usage() {
        let page = image(&sample_image(), None, NOW, utc());

        assert_eq!(page.title, "pub-sub-tui:local");
        assert_eq!(page.value("Identity", "ID"), Some("dff9997d956e"));
        assert_eq!(page.value("Identity", "Tags"), Some("pub-sub-tui:local"));
        assert_eq!(page.value("Storage", "Size"), Some("164.2 MB"));
        assert_eq!(
            page.value("Storage", "Created"),
            Some("2026-06-21 16:17:25 (2 days ago)")
        );
        assert_eq!(page.value("Usage", "Containers"), Some("2"));
        assert_eq!(page.value("Labels", "maintainer"), Some("dave"));
    }

    #[test]
    fn an_uncomputed_shared_size_is_omitted_rather_than_shown_as_unknown() {
        let mut image_summary = sample_image();
        image_summary.shared_size = -1;

        let page = image(&image_summary, None, NOW, utc());

        assert_eq!(page.value("Storage", "Shared size"), None);
        assert_eq!(page.value("Storage", "Size"), Some("164.2 MB"));
        assert!(page.value("Storage", "Created").is_some());
    }

    #[test]
    fn a_computed_shared_size_is_shown() {
        assert_eq!(
            image(&sample_image(), None, NOW, utc()).value("Storage", "Shared size"),
            Some("1.0 kB")
        );
    }

    #[test]
    fn an_untagged_image_page_still_identifies_the_image() {
        let mut untagged = sample_image();
        untagged.repo_tags = vec![];
        untagged.repo_digests = vec![];
        untagged.labels.clear();

        let page = image(&untagged, None, NOW, utc());

        assert_eq!(page.title, UNTAGGED);
        assert_eq!(page.subtitle.as_deref(), Some("dff9997d956e"));
        assert_eq!(page.value("Identity", "Tags"), Some("none"));
        assert_eq!(page.value("Identity", "Digests"), Some("none"));
        assert!(!page.group_titles().contains(&"Labels"));
    }

    #[test]
    fn the_container_page_shows_state_ports_and_mounts() {
        let page = container(&sample_container(), None, NOW, utc());

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

        let page = container(&bare, None, NOW, utc());

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

        let page = container(&writable, None, NOW, utc());

        assert_eq!(
            page.value("Storage", "Mounts"),
            Some("/srv/www \u{2192} /usr/share/nginx/html")
        );
    }

    #[test]
    fn raw_inspect_output_is_pretty_printed_for_the_expander() {
        let raw = serde_json::json!({"Id": "abc", "State": {"Running": true}});

        let page = container(&sample_container(), Some(&raw), NOW, utc());
        let rendered = page.raw.expect("raw output present");

        assert!(rendered.contains("\n  \"Id\": \"abc\""), "got: {rendered}");
    }

    #[test]
    fn pages_without_inspect_output_have_no_raw_section() {
        assert!(image(&sample_image(), None, NOW, utc()).raw.is_none());
        assert!(images(&[]).raw.is_none());
        assert!(containers(&[]).raw.is_none());
    }
}
