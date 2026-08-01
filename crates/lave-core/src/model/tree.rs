//! The sidebar's structure.
//!
//! Produced as plain data so the widget layer only has to walk it.

use crate::engine::{ContainerState, ContainerSummary, EnvironmentSummary, ImageSummary};

use super::format::{container_label, image_label, short_id};

/// Identifies a node, and therefore what the detail pane should show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeId {
    Root,
    Images,
    Containers,
    Image(String),
    Container(String),
}

impl NodeId {
    /// Stable string form, for carrying the identity through a `GObject`.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            NodeId::Root => "root".to_owned(),
            NodeId::Images => "images".to_owned(),
            NodeId::Containers => "containers".to_owned(),
            NodeId::Image(id) => format!("image:{id}"),
            NodeId::Container(id) => format!("container:{id}"),
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "root" => Some(NodeId::Root),
            "images" => Some(NodeId::Images),
            "containers" => Some(NodeId::Containers),
            _ => match key.split_once(':') {
                Some(("image", id)) if !id.is_empty() => Some(NodeId::Image(id.to_owned())),
                Some(("container", id)) if !id.is_empty() => Some(NodeId::Container(id.to_owned())),
                _ => None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub id: NodeId,
    pub label: String,
    /// Dimmed secondary text.
    pub detail: Option<String>,
    /// Symbolic icon name from the Adwaita theme.
    pub icon: &'static str,
    pub children: Vec<TreeNode>,
}

const ICON_ROOT: &str = "network-server-symbolic";
const ICON_IMAGES: &str = "drive-harddisk-symbolic";
const ICON_IMAGE: &str = "media-optical-symbolic";
const ICON_CONTAINERS: &str = "view-list-symbolic";
const ICON_RUNNING: &str = "media-playback-start-symbolic";
const ICON_PAUSED: &str = "media-playback-pause-symbolic";
const ICON_STOPPED: &str = "media-playback-stop-symbolic";
const ICON_DEAD: &str = "dialog-warning-symbolic";

/// State is shown by icon shape as well as by text, never by colour alone.
#[must_use]
pub fn state_icon(state: &ContainerState) -> &'static str {
    match state {
        ContainerState::Running | ContainerState::Restarting => ICON_RUNNING,
        ContainerState::Paused => ICON_PAUSED,
        ContainerState::Dead => ICON_DEAD,
        _ => ICON_STOPPED,
    }
}

/// Build the whole tree. The root is the local environment; its children are the
/// Images and Containers nodes the README calls for.
#[must_use]
pub fn build(
    environment: Option<&EnvironmentSummary>,
    images: &[ImageSummary],
    containers: &[ContainerSummary],
) -> TreeNode {
    TreeNode {
        id: NodeId::Root,
        label: root_label(environment),
        detail: environment
            .map(|environment| environment.server_version.clone())
            .filter(|version| !version.is_empty()),
        icon: ICON_ROOT,
        children: vec![images_node(images), containers_node(containers)],
    }
}

fn root_label(environment: Option<&EnvironmentSummary>) -> String {
    environment
        .map(|environment| environment.name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Docker".to_owned())
}

fn images_node(images: &[ImageSummary]) -> TreeNode {
    let mut children: Vec<TreeNode> = images
        .iter()
        .map(|image| TreeNode {
            id: NodeId::Image(image.id.clone()),
            label: image_label(image),
            detail: Some(short_id(&image.id)),
            icon: ICON_IMAGE,
            children: Vec::new(),
        })
        .collect();
    sort_children(&mut children);

    TreeNode {
        id: NodeId::Images,
        label: "Images".to_owned(),
        detail: Some(images.len().to_string()),
        icon: ICON_IMAGES,
        children,
    }
}

fn containers_node(containers: &[ContainerSummary]) -> TreeNode {
    let mut children: Vec<TreeNode> = containers
        .iter()
        .map(|container| TreeNode {
            id: NodeId::Container(container.id.clone()),
            label: container_label(container),
            detail: Some(container.state.label().to_owned()),
            icon: state_icon(&container.state),
            children: Vec::new(),
        })
        .collect();
    sort_children(&mut children);

    let running = containers
        .iter()
        .filter(|container| container.state.is_active())
        .count();
    let detail = if running == 0 {
        containers.len().to_string()
    } else {
        format!("{} ({running} running)", containers.len())
    };

    TreeNode {
        id: NodeId::Containers,
        label: "Containers".to_owned(),
        detail: Some(detail),
        icon: ICON_CONTAINERS,
        children,
    }
}

/// Case-insensitive by label, with the key as a tie-break so the order is total.
fn sort_children(children: &mut [TreeNode]) {
    children.sort_by(|left, right| {
        left.label
            .to_lowercase()
            .cmp(&right.label.to_lowercase())
            .then_with(|| left.id.key().cmp(&right.id.key()))
    });
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::model::format::UNTAGGED;

    fn image(id: &str, tag: &str) -> ImageSummary {
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

    fn container(id: &str, name: &str, state: ContainerState) -> ContainerSummary {
        ContainerSummary {
            id: id.to_owned(),
            names: if name.is_empty() {
                vec![]
            } else {
                vec![name.to_owned()]
            },
            state,
            ..ContainerSummary::default()
        }
    }

    fn labels(node: &TreeNode) -> Vec<&str> {
        node.children
            .iter()
            .map(|child| child.label.as_str())
            .collect()
    }

    #[test]
    fn the_root_has_exactly_the_two_nodes_the_readme_asks_for() {
        let tree = build(None, &[], &[]);

        assert_eq!(tree.id, NodeId::Root);
        assert_eq!(labels(&tree), vec!["Images", "Containers"]);
        assert_eq!(tree.children[0].id, NodeId::Images);
        assert_eq!(tree.children[1].id, NodeId::Containers);
    }

    #[test]
    fn the_root_is_named_for_the_daemon_when_it_is_known() {
        let environment = EnvironmentSummary {
            name: "workstation".to_owned(),
            server_version: "29.6.2".to_owned(),
            ..EnvironmentSummary::default()
        };

        let tree = build(Some(&environment), &[], &[]);

        assert_eq!(tree.label, "workstation");
        assert_eq!(tree.detail.as_deref(), Some("29.6.2"));
    }

    #[test]
    fn the_root_falls_back_to_a_generic_name_before_the_probe_returns() {
        let tree = build(None, &[], &[]);

        assert_eq!(tree.label, "Docker");
        assert_eq!(tree.detail, None);
    }

    #[test]
    fn a_blank_daemon_name_does_not_produce_an_empty_label() {
        let environment = EnvironmentSummary {
            name: "   ".to_owned(),
            ..EnvironmentSummary::default()
        };

        assert_eq!(build(Some(&environment), &[], &[]).label, "Docker");
    }

    #[test]
    fn images_are_listed_case_insensitively_by_tag() {
        let images = [
            image("aaa", "Zebra:latest"),
            image("bbb", "alpine:3.20"),
            image("ccc", "nginx:1.27"),
        ];

        let tree = build(None, &images, &[]);

        assert_eq!(
            labels(&tree.children[0]),
            vec!["alpine:3.20", "nginx:1.27", "Zebra:latest"]
        );
    }

    #[test]
    fn untagged_images_appear_under_a_placeholder_and_stay_distinguishable() {
        let images = [image("aaa", ""), image("bbb", "")];

        let node = &build(None, &images, &[]).children[0];

        assert_eq!(labels(node), vec![UNTAGGED, UNTAGGED]);
        assert_eq!(node.children[0].detail.as_deref(), Some("aaa"));
        assert_ne!(node.children[0].id, node.children[1].id);
    }

    #[test]
    fn each_node_carries_the_count_of_its_children() {
        let images = [image("aaa", "one:1"), image("bbb", "two:2")];
        let containers = [container("c1", "web", ContainerState::Exited)];

        let tree = build(None, &images, &containers);

        assert_eq!(tree.children[0].detail.as_deref(), Some("2"));
        assert_eq!(tree.children[1].detail.as_deref(), Some("1"));
    }

    #[test]
    fn the_container_count_calls_out_running_ones() {
        let containers = [
            container("c1", "web", ContainerState::Running),
            container("c2", "db", ContainerState::Exited),
            container("c3", "cache", ContainerState::Paused),
        ];

        let tree = build(None, &[], &containers);

        assert_eq!(tree.children[1].detail.as_deref(), Some("3 (2 running)"));
    }

    #[test]
    fn containers_show_their_state_as_text_and_as_a_distinct_icon() {
        let containers = [
            container("c1", "runner", ContainerState::Running),
            container("c2", "sleeper", ContainerState::Exited),
            container("c3", "held", ContainerState::Paused),
            container("c4", "broken", ContainerState::Dead),
        ];

        let node = &build(None, &[], &containers).children[1];
        let by_label = |label: &str| {
            node.children
                .iter()
                .find(|child| child.label == label)
                .expect("child present")
                .clone()
        };

        assert_eq!(by_label("runner").detail.as_deref(), Some("running"));
        assert_eq!(by_label("runner").icon, ICON_RUNNING);
        assert_eq!(by_label("sleeper").icon, ICON_STOPPED);
        assert_eq!(by_label("held").icon, ICON_PAUSED);
        assert_eq!(by_label("broken").icon, ICON_DEAD);
    }

    #[test]
    fn an_unnamed_container_is_listed_by_its_short_id() {
        let containers = [container(
            "13ef39df585fa5ea8df9325dffdc7c18",
            "",
            ContainerState::Exited,
        )];

        let node = &build(None, &[], &containers).children[1];

        assert_eq!(labels(node), vec!["13ef39df585f"]);
    }

    #[test]
    fn empty_listings_produce_empty_nodes_rather_than_missing_ones() {
        let tree = build(None, &[], &[]);

        assert!(tree.children[0].children.is_empty());
        assert!(tree.children[1].children.is_empty());
        assert_eq!(tree.children[0].detail.as_deref(), Some("0"));
    }

    #[test]
    fn node_keys_round_trip() {
        let ids = [
            NodeId::Root,
            NodeId::Images,
            NodeId::Containers,
            NodeId::Image("sha256:abc".to_owned()),
            NodeId::Container("13ef39df".to_owned()),
        ];

        for id in ids {
            let key = id.key();
            assert_eq!(NodeId::from_key(&key), Some(id), "key was {key}");
        }
    }

    #[test]
    fn unrecognised_keys_are_rejected_rather_than_guessed_at() {
        for key in ["", "image:", "nonsense", "volume:abc", ":"] {
            assert_eq!(NodeId::from_key(key), None, "key was {key:?}");
        }
    }
}
