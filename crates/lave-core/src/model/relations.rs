//! How images and containers relate to one another.
//!
//! Since Docker 1.10 an image is a config document naming an ordered stack of layers,
//! not a chain of parent images: `Parent` is empty on anything `BuildKit` produced. So
//! derivation is not a pointer to follow but a *shared prefix of layer digests* — if
//! every layer of A is also the bottom of B, in order, then B was built `FROM` A.
//! Everything here is pure: the layer digests are supplied by the caller.

use std::collections::BTreeMap;

use crate::engine::{ContainerSummary, ImageSummary};

/// Layer digests per image ID, ordered base first.
///
/// `GET /images/json` does not carry them, so they arrive by inspecting each image and
/// are cached here across refreshes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LayerIndex(BTreeMap<String, Vec<String>>);

impl LayerIndex {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, image_id: &str, layers: Vec<String>) {
        self.0.insert(image_id.to_owned(), layers);
    }

    #[must_use]
    pub fn get(&self, image_id: &str) -> Option<&[String]> {
        self.0.get(image_id).map(Vec::as_slice)
    }

    #[must_use]
    pub fn contains(&self, image_id: &str) -> bool {
        self.0.contains_key(image_id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Drop entries for images the daemon no longer has, so a long-running session does
    /// not accumulate the layer stacks of every image ever pulled.
    pub fn retain_known(&mut self, images: &[ImageSummary]) {
        self.0
            .retain(|id, _| images.iter().any(|image| image.id.as_str() == id.as_str()));
    }
}

/// Containers built from this image, matched on the image ID the daemon recorded when
/// each container was created.
#[must_use]
pub fn containers_of<'a>(
    image: &ImageSummary,
    containers: &'a [ContainerSummary],
) -> Vec<&'a ContainerSummary> {
    containers
        .iter()
        .filter(|container| container.image_id == image.id)
        .collect()
}

/// The image a container is actually running.
#[must_use]
pub fn running_image<'a>(
    container: &ContainerSummary,
    images: &'a [ImageSummary],
) -> Option<&'a ImageSummary> {
    images.iter().find(|image| image.id == container.image_id)
}

/// The image the container's reference names *now*.
///
/// A container records both the reference it was created from and the ID it actually
/// runs. Pull the same tag again and the tag moves to the new image while the container
/// keeps the old one, so these stop agreeing — see [`tag_has_moved`].
#[must_use]
pub fn tagged_image<'a>(
    container: &ContainerSummary,
    images: &'a [ImageSummary],
) -> Option<&'a ImageSummary> {
    if container.image.is_empty() {
        return None;
    }
    images
        .iter()
        .find(|image| matches_reference(image, &container.image))
}

/// True when the reference a container was created from no longer names the image it
/// runs. The image it runs is then usually untagged, which is where those mystery
/// `<none>` entries come from.
#[must_use]
pub fn tag_has_moved(container: &ContainerSummary, images: &[ImageSummary]) -> bool {
    match tagged_image(container, images) {
        Some(tagged) => tagged.id != container.image_id,
        None => false,
    }
}

/// Whether an image answers to a reference: by ID, by tag, or by digest.
#[must_use]
pub fn matches_reference(image: &ImageSummary, reference: &str) -> bool {
    if reference.is_empty() {
        return false;
    }
    if image.id == reference {
        return true;
    }
    if image.repo_digests.iter().any(|digest| digest == reference) {
        return true;
    }

    let normalised = normalise_reference(reference);
    image.repo_tags.contains(&normalised)
}

/// Docker records `nginx` on a container but `nginx:latest` in the image's tags.
/// The colon in `registry:5000/thing` is part of the host, so only the final path
/// segment can carry a tag.
fn normalise_reference(reference: &str) -> String {
    if reference.contains('@') || reference.starts_with("sha256:") {
        return reference.to_owned();
    }

    let last_segment = reference.rsplit('/').next().unwrap_or(reference);
    if last_segment.contains(':') {
        reference.to_owned()
    } else {
        format!("{reference}:latest")
    }
}

/// How many layers, counting up from the base, two images have in common.
#[must_use]
pub fn shared_prefix(left: &[String], right: &[String]) -> usize {
    left.iter()
        .zip(right.iter())
        .take_while(|(left, right)| left == right)
        .count()
}

/// The image this one was built `FROM`: the local image whose entire layer stack is a
/// proper prefix of this one's. The longest such match wins, so a Node image derived
/// from Alpine is reported against Node rather than against Alpine.
#[must_use]
pub fn base_of<'a>(
    image: &ImageSummary,
    images: &'a [ImageSummary],
    layers: &LayerIndex,
) -> Option<&'a ImageSummary> {
    let own = layers.get(&image.id)?;
    if own.is_empty() {
        return None;
    }

    images
        .iter()
        .filter(|candidate| candidate.id != image.id)
        .filter_map(|candidate| {
            let theirs = layers.get(&candidate.id)?;
            // A proper prefix: shorter, and matching all the way up.
            let proper = !theirs.is_empty()
                && theirs.len() < own.len()
                && shared_prefix(own, theirs) == theirs.len();
            proper.then_some((candidate, theirs.len()))
        })
        .max_by_key(|(candidate, depth)| (*depth, candidate.id.clone()))
        .map(|(candidate, _)| candidate)
}

/// Images built directly from this one. Only immediate descendants: an image whose own
/// base is something nearer belongs under that instead, so the relation stays a tree.
#[must_use]
pub fn derived_from<'a>(
    image: &ImageSummary,
    images: &'a [ImageSummary],
    layers: &LayerIndex,
) -> Vec<&'a ImageSummary> {
    images
        .iter()
        .filter(|candidate| candidate.id != image.id)
        .filter(|candidate| {
            base_of(candidate, images, layers).is_some_and(|base| base.id == image.id)
        })
        .collect()
}

/// Other images sharing this one's exact layer stack. Two configs over identical layers
/// differ only in metadata, so neither is the other's base.
#[must_use]
pub fn same_layers_as<'a>(
    image: &ImageSummary,
    images: &'a [ImageSummary],
    layers: &LayerIndex,
) -> Vec<&'a ImageSummary> {
    let Some(own) = layers.get(&image.id).filter(|own| !own.is_empty()) else {
        return Vec::new();
    };

    images
        .iter()
        .filter(|candidate| candidate.id != image.id)
        .filter(|candidate| layers.get(&candidate.id) == Some(own))
        .collect()
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;

    fn image(id: &str, tags: &[&str]) -> ImageSummary {
        ImageSummary {
            id: format!("sha256:{id}"),
            repo_tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            ..ImageSummary::default()
        }
    }

    fn container(id: &str, reference: &str, image_id: &str) -> ContainerSummary {
        ContainerSummary {
            id: id.to_owned(),
            names: vec![id.to_owned()],
            image: reference.to_owned(),
            image_id: format!("sha256:{image_id}"),
            ..ContainerSummary::default()
        }
    }

    fn layers(digests: &[&str]) -> Vec<String> {
        digests.iter().map(|digest| (*digest).to_owned()).collect()
    }

    /// The real shape observed on the author's daemon: web-gui is built FROM
    /// node:22-alpine, which is itself built FROM alpine.
    fn family() -> (Vec<ImageSummary>, LayerIndex) {
        let images = vec![
            image("alpine", &["alpine:3.24"]),
            image("node", &["node:22-alpine"]),
            image("webgui", &["pub-sub-gui-web-gui:latest"]),
            image("unrelated", &["hello-world:latest"]),
        ];

        let mut index = LayerIndex::new();
        index.insert("sha256:alpine", layers(&["l1"]));
        index.insert("sha256:node", layers(&["l1", "l2", "l3", "l4"]));
        index.insert(
            "sha256:webgui",
            layers(&["l1", "l2", "l3", "l4", "l5", "l6"]),
        );
        index.insert("sha256:unrelated", layers(&["x1"]));

        (images, index)
    }

    fn find<'a>(images: &'a [ImageSummary], id: &str) -> &'a ImageSummary {
        images
            .iter()
            .find(|image| image.id == format!("sha256:{id}"))
            .expect("image present in the fixture")
    }

    #[test]
    fn an_image_lists_the_containers_built_from_it() {
        let images = [image("nginx", &["nginx:1.27"])];
        let containers = [
            container("web", "nginx:1.27", "nginx"),
            container("api", "nginx:1.27", "nginx"),
            container("db", "postgres:16", "postgres"),
        ];

        let using = containers_of(&images[0], &containers);

        assert_eq!(using.len(), 2);
        assert_eq!(using[0].id, "web");
        assert_eq!(using[1].id, "api");
    }

    #[test]
    fn an_image_with_no_containers_reports_an_empty_list_rather_than_guessing() {
        let images = [image("nginx", &["nginx:1.27"])];

        assert!(containers_of(&images[0], &[]).is_empty());
    }

    #[test]
    fn a_container_resolves_to_the_image_it_is_actually_running() {
        let images = [image("nginx", &["nginx:1.27"])];
        let running = container("web", "nginx:1.27", "nginx");

        assert_eq!(
            running_image(&running, &images).map(|image| image.id.as_str()),
            Some("sha256:nginx")
        );
    }

    #[test]
    fn a_container_whose_image_was_deleted_resolves_to_nothing() {
        let running = container("web", "nginx:1.27", "gone");

        assert!(running_image(&running, &[]).is_none());
    }

    #[test]
    fn a_moved_tag_is_detected_and_both_images_are_reachable() {
        // nginx:1.27 was pulled again: the tag moved to the new image, and the old one
        // that the container is still running lost its tag.
        let images = [image("old", &[]), image("new", &["nginx:1.27"])];
        let running = container("web", "nginx:1.27", "old");

        assert!(tag_has_moved(&running, &images));
        assert_eq!(
            running_image(&running, &images).map(|image| image.id.as_str()),
            Some("sha256:old")
        );
        assert_eq!(
            tagged_image(&running, &images).map(|image| image.id.as_str()),
            Some("sha256:new")
        );
    }

    #[test]
    fn a_tag_that_still_points_at_the_running_image_has_not_moved() {
        let images = [image("nginx", &["nginx:1.27"])];
        let running = container("web", "nginx:1.27", "nginx");

        assert!(!tag_has_moved(&running, &images));
    }

    #[test]
    fn a_reference_with_no_tag_is_matched_against_latest() {
        let images = [image("nginx", &["nginx:latest"])];
        let running = container("web", "nginx", "nginx");

        assert!(!tag_has_moved(&running, &images));
        assert_eq!(
            tagged_image(&running, &images).map(|image| image.id.as_str()),
            Some("sha256:nginx")
        );
    }

    #[test]
    fn a_port_in_a_registry_host_is_not_mistaken_for_a_tag() {
        let images = [image("thing", &["registry:5000/thing:latest"])];
        let running = container("web", "registry:5000/thing", "thing");

        assert_eq!(
            tagged_image(&running, &images).map(|image| image.id.as_str()),
            Some("sha256:thing")
        );
    }

    #[test]
    fn a_container_created_by_digest_or_id_still_resolves() {
        let mut by_digest = image("thing", &[]);
        by_digest.repo_digests = vec!["thing@sha256:deadbeef".to_owned()];
        let images = [by_digest];

        let digest_ref = container("web", "thing@sha256:deadbeef", "thing");
        assert!(!tag_has_moved(&digest_ref, &images));

        let id_ref = container("api", "sha256:thing", "thing");
        assert_eq!(
            tagged_image(&id_ref, &images).map(|image| image.id.as_str()),
            Some("sha256:thing")
        );
    }

    #[test]
    fn an_unresolvable_reference_is_not_reported_as_a_moved_tag() {
        // The named image is gone entirely; that is absence, not divergence.
        let running = container("web", "nginx:1.27", "old");

        assert!(!tag_has_moved(&running, &[image("old", &[])]));
    }

    #[test]
    fn the_shared_base_is_counted_from_the_bottom_up() {
        assert_eq!(
            shared_prefix(&layers(&["a", "b", "c"]), &layers(&["a", "b"])),
            2
        );
        assert_eq!(shared_prefix(&layers(&["a", "b"]), &layers(&["a", "b"])), 2);
        assert_eq!(shared_prefix(&layers(&["a"]), &layers(&["b"])), 0);
        assert_eq!(shared_prefix(&[], &layers(&["a"])), 0);
        // Divergence part-way up stops the count: later matches do not resume it.
        assert_eq!(
            shared_prefix(&layers(&["a", "x", "c"]), &layers(&["a", "y", "c"])),
            1
        );
    }

    #[test]
    fn the_nearest_ancestor_is_reported_as_the_base_not_the_most_distant() {
        let (images, index) = family();

        let base = base_of(find(&images, "webgui"), &images, &index);

        assert_eq!(base.map(|image| image.id.as_str()), Some("sha256:node"));
    }

    #[test]
    fn a_base_image_of_its_own_has_one_too() {
        let (images, index) = family();

        assert_eq!(
            base_of(find(&images, "node"), &images, &index).map(|image| image.id.as_str()),
            Some("sha256:alpine")
        );
    }

    #[test]
    fn an_image_at_the_bottom_of_the_stack_has_no_base() {
        let (images, index) = family();

        assert!(base_of(find(&images, "alpine"), &images, &index).is_none());
        assert!(base_of(find(&images, "unrelated"), &images, &index).is_none());
    }

    #[test]
    fn derivation_lists_only_immediate_children() {
        let (images, index) = family();

        let children = derived_from(find(&images, "alpine"), &images, &index);

        // web-gui descends from alpine, but through node, so it belongs under node.
        assert_eq!(
            children
                .iter()
                .map(|image| image.id.as_str())
                .collect::<Vec<_>>(),
            vec!["sha256:node"]
        );
        assert_eq!(
            derived_from(find(&images, "node"), &images, &index)
                .iter()
                .map(|image| image.id.as_str())
                .collect::<Vec<_>>(),
            vec!["sha256:webgui"]
        );
        assert!(derived_from(find(&images, "webgui"), &images, &index).is_empty());
    }

    #[test]
    fn base_and_derived_are_exact_duals_of_each_other() {
        let (images, index) = family();

        for image in &images {
            for child in derived_from(image, &images, &index) {
                assert_eq!(
                    base_of(child, &images, &index).map(|base| base.id.as_str()),
                    Some(image.id.as_str()),
                    "{} listed {} as derived",
                    image.id,
                    child.id
                );
            }
        }
    }

    #[test]
    fn images_are_never_reported_as_their_own_base_or_child() {
        let (images, index) = family();

        for image in &images {
            assert!(base_of(image, &images, &index).is_none_or(|base| base.id != image.id));
            assert!(
                derived_from(image, &images, &index)
                    .iter()
                    .all(|child| child.id != image.id)
            );
        }
    }

    #[test]
    fn images_without_layer_data_yield_no_relationships() {
        let (images, _) = family();
        let empty = LayerIndex::new();

        assert!(base_of(find(&images, "webgui"), &images, &empty).is_none());
        assert!(derived_from(find(&images, "alpine"), &images, &empty).is_empty());
    }

    #[test]
    fn identical_layer_stacks_are_siblings_rather_than_ancestors() {
        let images = vec![image("one", &["one:latest"]), image("two", &["two:latest"])];
        let mut index = LayerIndex::new();
        index.insert("sha256:one", layers(&["l1", "l2"]));
        index.insert("sha256:two", layers(&["l1", "l2"]));

        assert!(base_of(&images[0], &images, &index).is_none());
        assert!(derived_from(&images[0], &images, &index).is_empty());
        assert_eq!(
            same_layers_as(&images[0], &images, &index)
                .iter()
                .map(|image| image.id.as_str())
                .collect::<Vec<_>>(),
            vec!["sha256:two"]
        );
    }

    #[test]
    fn the_layer_index_forgets_images_the_daemon_no_longer_has() {
        let (images, mut index) = family();
        assert_eq!(index.len(), 4);

        index.retain_known(&images[0..2]);

        assert_eq!(index.len(), 2);
        assert!(index.contains("sha256:alpine"));
        assert!(!index.contains("sha256:webgui"));
    }
}
