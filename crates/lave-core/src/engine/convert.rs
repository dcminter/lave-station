//! Translation from `bollard`'s wire types into our own.
//!
//! Kept in one place so that a change of HTTP client, or a change in the daemon's
//! schema, shows up here as a failing fixture test rather than as odd values in the UI.

use bollard::models::{
    ContainerSummary as WireContainer, ContainerSummaryStateEnum, EventMessage as WireEvent,
    ImageSummary as WireImage, MountPoint as WireMount, PortSummary as WirePort,
    SystemInfo as WireInfo, SystemVersion as WireVersion,
};

use super::{
    ContainerState, ContainerSummary, EngineEvent, EnvironmentSummary, ImageSummary, MountSummary,
    PortMapping,
};

impl From<WireImage> for ImageSummary {
    fn from(wire: WireImage) -> Self {
        ImageSummary {
            id: wire.id,
            repo_tags: wire.repo_tags,
            repo_digests: wire.repo_digests,
            created: wire.created,
            size: wire.size,
            shared_size: wire.shared_size,
            containers: wire.containers,
            labels: wire.labels.into_iter().collect(),
        }
    }
}

impl From<WireContainer> for ContainerSummary {
    fn from(wire: WireContainer) -> Self {
        let networks = wire
            .network_settings
            .and_then(|settings| settings.networks)
            .map(|networks| {
                let mut names: Vec<String> = networks.into_keys().collect();
                names.sort();
                names
            })
            .unwrap_or_default();

        ContainerSummary {
            id: wire.id.unwrap_or_default(),
            // Docker prefixes container names with a slash.
            names: wire
                .names
                .unwrap_or_default()
                .into_iter()
                .map(|name| name.trim_start_matches('/').to_owned())
                .collect(),
            image: wire.image.unwrap_or_default(),
            image_id: wire.image_id.unwrap_or_default(),
            command: wire.command.unwrap_or_default(),
            created: wire.created.unwrap_or_default(),
            state: wire
                .state
                .map_or(ContainerState::Unknown, ContainerState::from),
            status: wire.status.unwrap_or_default(),
            ports: wire
                .ports
                .unwrap_or_default()
                .into_iter()
                .map(PortMapping::from)
                .collect(),
            mounts: wire
                .mounts
                .unwrap_or_default()
                .into_iter()
                .map(MountSummary::from)
                .collect(),
            networks,
            labels: wire.labels.unwrap_or_default().into_iter().collect(),
        }
    }
}

impl From<ContainerSummaryStateEnum> for ContainerState {
    fn from(wire: ContainerSummaryStateEnum) -> Self {
        match wire {
            ContainerSummaryStateEnum::CREATED => ContainerState::Created,
            ContainerSummaryStateEnum::RUNNING => ContainerState::Running,
            ContainerSummaryStateEnum::PAUSED => ContainerState::Paused,
            ContainerSummaryStateEnum::RESTARTING => ContainerState::Restarting,
            ContainerSummaryStateEnum::EXITED => ContainerState::Exited,
            ContainerSummaryStateEnum::REMOVING => ContainerState::Removing,
            ContainerSummaryStateEnum::DEAD => ContainerState::Dead,
            ContainerSummaryStateEnum::STOPPING => ContainerState::Stopping,
            ContainerSummaryStateEnum::EMPTY => ContainerState::Unknown,
        }
    }
}

impl From<WirePort> for PortMapping {
    fn from(wire: WirePort) -> Self {
        PortMapping {
            ip: wire.ip.filter(|ip| !ip.is_empty()),
            private_port: wire.private_port,
            public_port: wire.public_port,
            protocol: wire
                .typ
                .map(|typ| typ.to_string())
                .filter(|protocol| !protocol.is_empty())
                .unwrap_or_else(|| "tcp".to_owned()),
        }
    }
}

impl From<WireMount> for MountSummary {
    fn from(wire: WireMount) -> Self {
        MountSummary {
            kind: wire.typ.unwrap_or_default(),
            // A volume mount reports its name rather than a host path.
            source: wire
                .source
                .filter(|source| !source.is_empty())
                .or(wire.name)
                .unwrap_or_default(),
            destination: wire.destination.unwrap_or_default(),
            read_write: wire.rw.unwrap_or_default(),
        }
    }
}

impl From<WireEvent> for EngineEvent {
    fn from(wire: WireEvent) -> Self {
        let attributes = wire
            .actor
            .as_ref()
            .and_then(|actor| actor.attributes.clone())
            .unwrap_or_default();

        EngineEvent {
            kind: wire.typ.map(|typ| typ.to_string()).unwrap_or_default(),
            action: wire.action.unwrap_or_default(),
            actor_id: wire.actor.and_then(|actor| actor.id).unwrap_or_default(),
            actor_name: attributes
                .get("name")
                .filter(|name| !name.is_empty())
                .cloned(),
            time: wire.time.unwrap_or_default(),
        }
    }
}

/// The capability probe combines `/version` and `/info`.
#[must_use]
pub fn environment_summary(version: WireVersion, info: WireInfo) -> EnvironmentSummary {
    let security_options = info.security_options.unwrap_or_default();
    let rootless = security_options
        .iter()
        .any(|option| is_rootless_option(option));
    let storage_driver = info.driver.unwrap_or_default();

    let mut warnings = info.warnings.unwrap_or_default();
    // §8: the native snapshotter is slow and disk-hungry, and users blame the app.
    if storage_driver == "native" {
        warnings.push(
            "The native storage driver is in use. It copies every layer in full, so pulls \
             are slow and disk usage is high."
                .to_owned(),
        );
    }

    EnvironmentSummary {
        name: info.name.unwrap_or_default(),
        server_version: version.version.unwrap_or_default(),
        api_version: version.api_version.unwrap_or_default(),
        min_api_version: version.min_api_version.filter(|text| !text.is_empty()),
        os_type: version.os.unwrap_or_default(),
        architecture: version.arch.unwrap_or_default(),
        operating_system: info.operating_system.unwrap_or_default(),
        kernel_version: info.kernel_version.unwrap_or_default(),
        storage_driver,
        logging_driver: info.logging_driver.unwrap_or_default(),
        cgroup_version: enum_or_unknown(info.cgroup_version.map(|value| value.to_string())),
        cgroup_driver: enum_or_unknown(info.cgroup_driver.map(|value| value.to_string())),
        rootless,
        cpus: info.ncpu.unwrap_or_default(),
        memory_total: info.mem_total.unwrap_or_default(),
        docker_root_dir: info.docker_root_dir.unwrap_or_default(),
        containers_total: info.containers.unwrap_or_default(),
        containers_running: info.containers_running.unwrap_or_default(),
        containers_paused: info.containers_paused.unwrap_or_default(),
        containers_stopped: info.containers_stopped.unwrap_or_default(),
        images: info.images.unwrap_or_default(),
        security_options,
        warnings,
    }
}

/// Security options arrive as `name=value` pairs, e.g. `name=rootless`.
fn is_rootless_option(option: &str) -> bool {
    option.split(',').any(|part| part.trim() == "name=rootless")
}

fn enum_or_unknown(value: Option<String>) -> String {
    value
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// The archive endpoint reports times as RFC 3339, unlike the listings' epoch seconds.
/// An unparseable or absent time becomes 0 rather than an error: a file whose mtime we
/// cannot read is still a file worth listing.
pub fn epoch_seconds(value: Option<&str>) -> i64 {
    value
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map_or(0, |time| time.timestamp())
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;

    const IMAGES: &str = include_str!("../../tests/fixtures/images.json");
    const CONTAINERS: &str = include_str!("../../tests/fixtures/containers.json");
    const INFO: &str = include_str!("../../tests/fixtures/info.json");
    const VERSION: &str = include_str!("../../tests/fixtures/version.json");

    fn images() -> Vec<ImageSummary> {
        serde_json::from_str::<Vec<WireImage>>(IMAGES)
            .expect("fixture parses as the daemon's image list")
            .into_iter()
            .map(ImageSummary::from)
            .collect()
    }

    fn containers() -> Vec<ContainerSummary> {
        serde_json::from_str::<Vec<WireContainer>>(CONTAINERS)
            .expect("fixture parses as the daemon's container list")
            .into_iter()
            .map(ContainerSummary::from)
            .collect()
    }

    fn environment() -> EnvironmentSummary {
        let version: WireVersion = serde_json::from_str(VERSION).expect("version fixture parses");
        let info: WireInfo = serde_json::from_str(INFO).expect("info fixture parses");
        environment_summary(version, info)
    }

    #[test]
    fn the_image_fixture_converts_without_loss() {
        let images = images();

        assert_eq!(images.len(), 16);
        let first = images.first().expect("at least one image");
        assert!(first.id.starts_with("sha256:"));
        assert_eq!(first.repo_tags, vec!["pub-sub-tui:local"]);
        assert!(first.size > 0);
        assert!(first.created > 0);
    }

    #[test]
    fn the_container_fixture_converts_without_loss() {
        let containers = containers();

        assert_eq!(containers.len(), 5);
        let first = containers.first().expect("at least one container");
        assert!(!first.id.is_empty());
        assert_eq!(first.state, ContainerState::Exited);
        assert!(!first.state.is_active());
        assert!(!first.status.is_empty());
    }

    #[test]
    fn container_names_lose_dockers_leading_slash() {
        for container in containers() {
            for name in &container.names {
                assert!(!name.starts_with('/'), "name still has a slash: {name}");
                assert!(!name.is_empty());
            }
        }
    }

    #[test]
    fn compose_labels_and_networks_survive_conversion() {
        let container = containers()
            .into_iter()
            .find(|container| container.labels.contains_key("com.docker.compose.project"))
            .expect("the fixture contains compose-managed containers");

        assert!(!container.networks.is_empty());
        assert!(container.networks.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn the_environment_probe_reads_both_endpoints() {
        let environment = environment();

        assert_eq!(environment.server_version, "29.6.2");
        assert_eq!(environment.api_version, "1.55");
        assert_eq!(environment.os_type, "linux");
        assert_eq!(environment.architecture, "amd64");
        // Docker 29 renamed this driver from overlay2.
        assert_eq!(environment.storage_driver, "overlayfs");
        assert_eq!(environment.cgroup_version, "2");
        assert!(environment.cpus > 0);
        assert!(environment.memory_total > 0);
        assert!(!environment.kernel_version.is_empty());
    }

    #[test]
    fn this_daemon_is_reported_as_rootful() {
        assert!(!environment().rootless);
    }

    #[test]
    fn rootless_is_detected_from_the_security_options() {
        let info = WireInfo {
            security_options: Some(vec![
                "name=seccomp,profile=builtin".to_owned(),
                "name=rootless".to_owned(),
            ]),
            ..WireInfo::default()
        };

        assert!(environment_summary(WireVersion::default(), info).rootless);
    }

    #[test]
    fn the_native_storage_driver_earns_a_warning() {
        let info = WireInfo {
            driver: Some("native".to_owned()),
            ..WireInfo::default()
        };

        let environment = environment_summary(WireVersion::default(), info);

        assert!(
            environment
                .warnings
                .iter()
                .any(|warning| warning.contains("native storage driver")),
            "expected a native-driver warning, got {:?}",
            environment.warnings
        );
    }

    #[test]
    fn daemon_warnings_are_preserved_alongside_our_own() {
        let info = WireInfo {
            driver: Some("native".to_owned()),
            warnings: Some(vec!["No swap limit support".to_owned()]),
            ..WireInfo::default()
        };

        let warnings = environment_summary(WireVersion::default(), info).warnings;

        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0], "No swap limit support");
    }

    #[test]
    fn absent_fields_become_unknown_rather_than_empty() {
        let environment = environment_summary(WireVersion::default(), WireInfo::default());

        assert_eq!(environment.cgroup_version, "unknown");
        assert_eq!(environment.cgroup_driver, "unknown");
        assert_eq!(environment.min_api_version, None);
    }

    #[test]
    fn ports_default_to_tcp_when_the_daemon_omits_the_protocol() {
        let port = PortMapping::from(WirePort {
            ip: Some(String::new()),
            private_port: 80,
            public_port: Some(8080),
            typ: None,
        });

        assert_eq!(port.protocol, "tcp");
        assert_eq!(port.ip, None);
        assert_eq!(port.public_port, Some(8080));
    }

    #[test]
    fn a_volume_mount_falls_back_to_its_name_when_it_has_no_source() {
        let mount = MountSummary::from(WireMount {
            name: Some("app-data".to_owned()),
            source: Some(String::new()),
            destination: Some("/var/lib/app".to_owned()),
            rw: Some(true),
            ..WireMount::default()
        });

        assert_eq!(mount.source, "app-data");
        assert!(mount.read_write);
    }

    #[test]
    fn events_carry_the_actor_name_when_the_daemon_supplies_one() {
        let wire: WireEvent = serde_json::from_str(
            r#"{"Type":"container","Action":"start","Actor":{"ID":"abc123","Attributes":{"name":"web","image":"nginx"}},"time":1782058645}"#,
        )
        .expect("event parses");

        let event = EngineEvent::from(wire);

        assert_eq!(event.kind, "container");
        assert_eq!(event.action, "start");
        assert_eq!(event.actor_id, "abc123");
        assert_eq!(event.actor_name.as_deref(), Some("web"));
        assert!(event.affects_listing());
    }

    #[test]
    fn events_without_a_name_attribute_do_not_invent_one() {
        let wire: WireEvent = serde_json::from_str(
            r#"{"Type":"network","Action":"connect","Actor":{"ID":"net1","Attributes":{}},"time":1}"#,
        )
        .expect("event parses");

        let event = EngineEvent::from(wire);

        assert_eq!(event.actor_name, None);
        assert!(
            !event.affects_listing(),
            "network events do not change listings"
        );
    }

    #[test]
    fn labels_are_ordered_so_the_detail_pane_is_stable() {
        let wire = WireImage {
            labels: [
                ("z.last".to_owned(), "1".to_owned()),
                ("a.first".to_owned(), "2".to_owned()),
            ]
            .into_iter()
            .collect(),
            ..WireImage::default()
        };

        let image = ImageSummary::from(wire);
        let keys: Vec<&String> = image.labels.keys().collect();

        assert_eq!(keys, vec!["a.first", "z.last"]);
    }

    #[test]
    fn every_container_state_maps_to_a_distinct_label() {
        let states = [
            (ContainerSummaryStateEnum::CREATED, "created"),
            (ContainerSummaryStateEnum::RUNNING, "running"),
            (ContainerSummaryStateEnum::PAUSED, "paused"),
            (ContainerSummaryStateEnum::RESTARTING, "restarting"),
            (ContainerSummaryStateEnum::EXITED, "exited"),
            (ContainerSummaryStateEnum::REMOVING, "removing"),
            (ContainerSummaryStateEnum::DEAD, "dead"),
            (ContainerSummaryStateEnum::STOPPING, "stopping"),
            (ContainerSummaryStateEnum::EMPTY, "unknown"),
        ];

        for (wire, expected) in states {
            assert_eq!(ContainerState::from(wire).label(), expected);
        }
    }

    #[test]
    fn only_executing_states_count_as_active() {
        assert!(ContainerState::Running.is_active());
        assert!(ContainerState::Restarting.is_active());
        assert!(ContainerState::Paused.is_active());
        assert!(!ContainerState::Exited.is_active());
        assert!(!ContainerState::Created.is_active());
        assert!(!ContainerState::Dead.is_active());
    }
}
