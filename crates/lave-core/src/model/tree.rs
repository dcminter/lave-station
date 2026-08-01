//! The sidebar's structure.
//!
//! Produced as plain data so the widget layer only has to walk it.

use std::cmp::Reverse;

use crate::engine::{ContainerState, ContainerSummary, EnvironmentSummary, ImageSummary};

use super::format::{container_label, image_label, is_untagged, primary_tag};

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

/// What an icon's colour means. Decided here rather than in CSS so the mapping is
/// testable; the widget layer only turns each tone into an Adwaita named colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    #[default]
    Neutral,
    /// Doing what it should be doing.
    Good,
    /// Transient, or worth a second look.
    Warn,
    /// Not running.
    Bad,
    /// The daemon itself.
    Brand,
    /// The Images collection.
    Images,
    /// The Containers collection.
    Containers,
}

impl Tone {
    /// Every tone. The widget layer strips all of these from a recycled icon before
    /// applying the right one, so it must not be able to drift from this enum.
    pub const ALL: [Tone; 7] = [
        Tone::Neutral,
        Tone::Good,
        Tone::Warn,
        Tone::Bad,
        Tone::Brand,
        Tone::Images,
        Tone::Containers,
    ];

    /// CSS class the widget layer attaches to the icon.
    #[must_use]
    pub fn css_class(self) -> &'static str {
        match self {
            Tone::Neutral => "tone-neutral",
            Tone::Good => "tone-good",
            Tone::Warn => "tone-warn",
            Tone::Bad => "tone-bad",
            Tone::Brand => "tone-brand",
            Tone::Images => "tone-images",
            Tone::Containers => "tone-containers",
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
    pub tone: Tone,
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

/// State is shown by icon shape and by text as well as by colour, never by colour alone.
#[must_use]
pub fn state_icon(state: &ContainerState) -> &'static str {
    match state {
        ContainerState::Running | ContainerState::Restarting => ICON_RUNNING,
        ContainerState::Paused => ICON_PAUSED,
        ContainerState::Dead => ICON_DEAD,
        _ => ICON_STOPPED,
    }
}

/// Green for running, red for stopped, amber for anything in between. A container that
/// was created but never started is neutral: nothing has gone wrong with it.
#[must_use]
pub fn state_tone(state: &ContainerState) -> Tone {
    match state {
        ContainerState::Running => Tone::Good,
        ContainerState::Restarting
        | ContainerState::Paused
        | ContainerState::Stopping
        | ContainerState::Removing => Tone::Warn,
        ContainerState::Exited | ContainerState::Dead => Tone::Bad,
        ContainerState::Created | ContainerState::Unknown => Tone::Neutral,
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
        tone: Tone::Brand,
        children: vec![images_node(images), containers_node(containers)],
    }
}

fn root_label(environment: Option<&EnvironmentSummary>) -> String {
    environment
        .map(|environment| environment.name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Docker".to_owned())
}

/// Tagged images first, in alphabetical order; untagged ones after, newest first.
/// Shared with the table so the sidebar and the overview agree.
#[must_use]
pub fn sorted_images(images: &[ImageSummary]) -> Vec<&ImageSummary> {
    let mut sorted: Vec<&ImageSummary> = images.iter().collect();
    sorted.sort_by_key(|image| image_order(image));
    sorted
}

/// By name, case-insensitively.
#[must_use]
pub fn sorted_containers(containers: &[ContainerSummary]) -> Vec<&ContainerSummary> {
    let mut sorted: Vec<&ContainerSummary> = containers.iter().collect();
    sorted.sort_by_key(|container| {
        (
            container_label(container).to_lowercase(),
            container.id.clone(),
        )
    });
    sorted
}

fn images_node(images: &[ImageSummary]) -> TreeNode {
    let children: Vec<TreeNode> = sorted_images(images)
        .into_iter()
        .map(|image| TreeNode {
            id: NodeId::Image(image.id.clone()),
            label: image_label(image),
            // Nothing: the tag names it, and when there is no tag the label is the ID.
            detail: None,
            icon: ICON_IMAGE,
            // Untagged images are usually residue left by a tag moving elsewhere.
            tone: if is_untagged(image) {
                Tone::Warn
            } else {
                Tone::Neutral
            },
            children: Vec::new(),
        })
        .collect();

    TreeNode {
        id: NodeId::Images,
        label: "Images".to_owned(),
        detail: Some(images.len().to_string()),
        icon: ICON_IMAGES,
        tone: Tone::Images,
        children,
    }
}

/// The ID breaks ties so the order is total whatever the daemon returns.
fn image_order(image: &ImageSummary) -> (u8, String, Reverse<i64>, String) {
    match primary_tag(image) {
        Some(tag) => (0, tag.to_lowercase(), Reverse(0), image.id.clone()),
        None => (1, String::new(), Reverse(image.created), image.id.clone()),
    }
}

fn containers_node(containers: &[ContainerSummary]) -> TreeNode {
    let children: Vec<TreeNode> = sorted_containers(containers)
        .into_iter()
        .map(|container| TreeNode {
            id: NodeId::Container(container.id.clone()),
            label: container_label(container),
            detail: Some(container.state.label().to_owned()),
            icon: state_icon(&container.state),
            tone: state_tone(&container.state),
            children: Vec::new(),
        })
        .collect();

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
        tone: Tone::Containers,
        children,
    }
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;

    fn image(id: &str, tag: &str) -> ImageSummary {
        image_at(id, tag, 0)
    }

    fn image_at(id: &str, tag: &str, created: i64) -> ImageSummary {
        ImageSummary {
            id: format!("sha256:{id}"),
            repo_tags: if tag.is_empty() {
                vec![]
            } else {
                vec![tag.to_owned()]
            },
            created,
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
    fn untagged_images_are_listed_by_id_rather_than_a_placeholder() {
        let images = [image("aaa", ""), image("bbb", "")];

        let node = &build(None, &images, &[]).children[0];

        assert_eq!(labels(node), vec!["aaa", "bbb"]);
        assert_ne!(node.children[0].id, node.children[1].id);
    }

    #[test]
    fn image_rows_carry_no_secondary_text_at_all() {
        // A tagged image is named by its tag; the ID beside it was just noise.
        let images = [image("aaa", "nginx:1.27"), image("bbb", "")];

        let node = &build(None, &images, &[]).children[0];

        assert!(
            node.children.iter().all(|child| child.detail.is_none()),
            "no image row should have secondary text"
        );
    }

    #[test]
    fn tagged_images_sort_before_untagged_ones() {
        let images = [
            image("aaa", ""),
            image("bbb", "zebra:latest"),
            image("ccc", ""),
            image("ddd", "alpine:3.20"),
        ];

        let node = &build(None, &images, &[]).children[0];

        assert_eq!(
            labels(node),
            vec!["alpine:3.20", "zebra:latest", "aaa", "ccc"]
        );
    }

    #[test]
    fn untagged_images_sort_newest_first() {
        let images = [
            image_at("old", "", 1_000),
            image_at("new", "", 3_000),
            image_at("mid", "", 2_000),
        ];

        let node = &build(None, &images, &[]).children[0];

        assert_eq!(labels(node), vec!["new", "mid", "old"]);
    }

    #[test]
    fn creation_date_does_not_disturb_the_alphabetical_order_of_tagged_images() {
        let images = [
            image_at("aaa", "zebra:1", 9_000),
            image_at("bbb", "alpine:1", 1_000),
        ];

        let node = &build(None, &images, &[]).children[0];

        assert_eq!(labels(node), vec!["alpine:1", "zebra:1"]);
    }

    #[test]
    fn an_untagged_image_is_marked_as_worth_a_look() {
        let images = [image("aaa", ""), image("bbb", "nginx:1.27")];

        let node = &build(None, &images, &[]).children[0];

        assert_eq!(node.children[0].tone, Tone::Neutral, "nginx sorts first");
        assert_eq!(node.children[1].tone, Tone::Warn);
    }

    #[test]
    fn containers_are_listed_by_name_case_insensitively() {
        let containers = [
            container("c1", "Zulu", ContainerState::Exited),
            container("c2", "alpha", ContainerState::Running),
            container("c3", "mike", ContainerState::Exited),
        ];

        let node = &build(None, &[], &containers).children[1];

        assert_eq!(labels(node), vec!["alpha", "mike", "Zulu"]);
    }

    #[test]
    fn container_state_carries_a_colour_as_well_as_a_shape() {
        assert_eq!(state_tone(&ContainerState::Running), Tone::Good);
        assert_eq!(state_tone(&ContainerState::Exited), Tone::Bad);
        assert_eq!(state_tone(&ContainerState::Dead), Tone::Bad);
        assert_eq!(state_tone(&ContainerState::Paused), Tone::Warn);
        assert_eq!(state_tone(&ContainerState::Restarting), Tone::Warn);
        // Created but never started is not a failure.
        assert_eq!(state_tone(&ContainerState::Created), Tone::Neutral);
    }

    #[test]
    fn the_three_standing_nodes_are_each_coloured_distinctly() {
        let tree = build(None, &[], &[]);

        assert_eq!(tree.tone, Tone::Brand, "the daemon gets Docker's own blue");
        assert_eq!(tree.children[0].tone, Tone::Images);
        assert_eq!(tree.children[1].tone, Tone::Containers);
    }

    #[test]
    fn category_colours_are_not_reused_for_object_state() {
        // Otherwise a pastel would read as a state, or a state as a category.
        let states = [
            ContainerState::Running,
            ContainerState::Exited,
            ContainerState::Paused,
            ContainerState::Created,
            ContainerState::Dead,
        ];

        for state in states {
            let tone = state_tone(&state);
            assert!(
                !matches!(tone, Tone::Brand | Tone::Images | Tone::Containers),
                "{state:?} used a category colour"
            );
        }
    }

    #[test]
    fn every_tone_has_a_distinct_css_class() {
        let classes = Tone::ALL.map(Tone::css_class).to_vec();
        let unique: std::collections::BTreeSet<&str> = classes.iter().copied().collect();

        assert_eq!(unique.len(), classes.len());
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
