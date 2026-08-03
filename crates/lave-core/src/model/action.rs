//! What the user may do to an object, and what they are told before it happens.
//!
//! Every decision about the mutating surface lives here so that it is testable without
//! a daemon: which actions a state offers, whether one is destructive, and the exact
//! words of its confirmation. The widget layer renders this and may not invent an
//! action, a warning, or a dialog of its own.
//!
//! The classification is **reversibility, not severity** — see
//! `docs/iteration_3_plan.md` §4. Stopping a container is disruptive but recoverable,
//! so it acts immediately; a dialog on every stop teaches the user to dismiss dialogs
//! unread, which is what makes the removal dialog dangerous.

use crate::engine::{ContainerState, ContainerSummary, ImageSummary, Lifecycle};
use crate::model::format;
use crate::model::relations;
use crate::model::tree::Tone;

/// Something the user can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Reversible: the container survives.
    Lifecycle(Lifecycle),
    /// `force` kills a running container first.
    RemoveContainer {
        force: bool,
    },
    RemoveImage,
    PruneContainers,
    PruneImages,
    ViewLogs,
    ViewDockerfile,
    BrowseFilesystem,
    OpenInFileManager,
}

impl Action {
    /// Whether carrying this out can lose something the user cannot get back.
    #[must_use]
    pub fn is_destructive(self) -> bool {
        matches!(
            self,
            Action::RemoveContainer { .. }
                | Action::RemoveImage
                | Action::PruneContainers
                | Action::PruneImages
        )
    }

    /// Whether this halts something that is currently executing.
    ///
    /// Separate from destructive: stopping loses nothing, so it needs no dialog, but it
    /// is still the menu item a misclick regrets — which is why it is marked out.
    #[must_use]
    pub fn is_halting(self) -> bool {
        matches!(self, Action::Lifecycle(Lifecycle::Stop | Lifecycle::Kill))
    }
}

/// What the user is shown before a destructive action proceeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    pub heading: String,
    /// Why this matters, in concrete terms. Never a bare "are you sure?".
    pub body: String,
    /// The affirmative button. Names the act, so the choice is readable without the
    /// heading.
    pub confirm_label: String,
    /// Everything that will be removed, named. Empty for single-object removals, where
    /// the heading already names the one thing.
    pub items: Vec<String>,
}

/// An action as offered to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub action: Action,
    pub label: String,
    pub icon: &'static str,
    /// Present exactly when the action is destructive.
    pub confirmation: Option<Confirmation>,
}

impl Offer {
    #[must_use]
    pub fn is_destructive(&self) -> bool {
        self.action.is_destructive()
    }

    /// The tone this offer's icon carries in a menu: red for anything that removes
    /// something or halts something that is running, and nothing at all otherwise.
    ///
    /// Colour is reinforcement here, never the signal: the label says what it does, and
    /// destructive items sit in their own section besides.
    #[must_use]
    pub fn tone(&self) -> Tone {
        if self.action.is_destructive() || self.action.is_halting() {
            Tone::Bad
        } else {
            Tone::Neutral
        }
    }

    fn plain(action: Action, label: &str, icon: &'static str) -> Self {
        Self {
            action,
            label: label.to_owned(),
            icon,
            confirmation: None,
        }
    }
}

/// What is offered for a container in its current state.
#[must_use]
pub fn for_container(container: &ContainerSummary, images: &[ImageSummary]) -> Vec<Offer> {
    let mut offers = Vec::new();

    // Reversible transitions first: they are the common case and carry no dialog.
    match container.state {
        ContainerState::Running => {
            offers.push(lifecycle(Lifecycle::Stop));
            offers.push(lifecycle(Lifecycle::Restart));
            offers.push(lifecycle(Lifecycle::Pause));
        }
        ContainerState::Paused => {
            offers.push(lifecycle(Lifecycle::Unpause));
            offers.push(lifecycle(Lifecycle::Stop));
        }
        ContainerState::Created | ContainerState::Exited => {
            offers.push(lifecycle(Lifecycle::Start));
        }
        // Restarting and Stopping are mid-transition, so another would race the daemon;
        // Dead will not move at all, and removal is its only way out.
        ContainerState::Restarting
        | ContainerState::Stopping
        | ContainerState::Dead
        | ContainerState::Removing
        | ContainerState::Unknown => {}
    }

    // Kill is separate from Stop: it skips the grace period, so it is worth naming
    // rather than hiding behind a stop that seems slow.
    if container.state.is_active() {
        offers.push(lifecycle(Lifecycle::Kill));
    }

    offers.push(Offer::plain(Action::ViewLogs, "Logs", "view-list-symbolic"));
    offers.push(Offer::plain(
        Action::BrowseFilesystem,
        "Files",
        "folder-symbolic",
    ));
    offers.push(Offer::plain(
        Action::OpenInFileManager,
        "Open in Files",
        "external-link-symbolic",
    ));

    if container.state != ContainerState::Removing {
        offers.push(remove_container_offer(container, images));
    }

    offers
}

/// What is offered for an image.
#[must_use]
pub fn for_image(image: &ImageSummary, containers: &[ContainerSummary]) -> Vec<Offer> {
    vec![
        Offer::plain(
            Action::ViewDockerfile,
            "Dockerfile",
            "text-x-generic-symbolic",
        ),
        Offer::plain(Action::BrowseFilesystem, "Files", "folder-symbolic"),
        Offer::plain(
            Action::OpenInFileManager,
            "Open in Files",
            "external-link-symbolic",
        ),
        remove_image_offer(image, containers),
    ]
}

/// How much of a listing is checked, which is what the select-all control shows and what
/// decides whether the cog beside it has anything to act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub rows: usize,
    pub checked: usize,
}

impl Tally {
    /// More checked than there are rows cannot happen, but clamping keeps the three
    /// states below mutually exclusive whatever is passed in.
    #[must_use]
    pub fn new(rows: usize, checked: usize) -> Self {
        Self {
            rows,
            checked: checked.min(rows),
        }
    }

    /// Nothing to check: the listing is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// Something is checked, so a bulk action has a target.
    #[must_use]
    pub fn any(&self) -> bool {
        self.checked > 0
    }

    /// Some but not all — the mixed state a check button draws as a dash.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        self.any() && self.checked < self.rows
    }

    /// Everything present is checked.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        !self.is_empty() && self.checked == self.rows
    }

    /// What operating the select-all control would do, said before it is clicked.
    #[must_use]
    pub fn select_all_label(&self) -> &'static str {
        if self.is_empty() {
            "There is nothing to check"
        } else if self.is_complete() {
            "Uncheck every row"
        } else {
            "Check every row"
        }
    }
}

/// One object a bulk action will be applied to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkTarget {
    pub id: String,
    pub label: String,
    /// The action as it applies to **this** object. Removing a running container forces;
    /// removing a stopped one does not, and a selection may hold both.
    pub action: Action,
}

/// An action offered for several checked objects at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkOffer {
    /// The action in summary, for identifying the offer. What is actually carried out is
    /// each target's own action, which may differ in its flags.
    pub action: Action,
    pub label: String,
    pub icon: &'static str,
    pub targets: Vec<BulkTarget>,
    /// Present exactly when the action is destructive, as for a single object.
    pub confirmation: Option<Confirmation>,
}

impl BulkOffer {
    #[must_use]
    pub fn is_destructive(&self) -> bool {
        self.action.is_destructive()
    }

    #[must_use]
    pub fn tone(&self) -> Tone {
        if self.action.is_destructive() || self.action.is_halting() {
            Tone::Bad
        } else {
            Tone::Neutral
        }
    }
}

/// The order bulk lifecycle actions are listed in: least disruptive first, so the two
/// that halt a running container are furthest from the pointer's resting place.
const BULK_LIFECYCLE: [Lifecycle; 6] = [
    Lifecycle::Start,
    Lifecycle::Unpause,
    Lifecycle::Restart,
    Lifecycle::Pause,
    Lifecycle::Stop,
    Lifecycle::Kill,
];

/// What is offered for a set of checked objects.
///
/// An action appears when **at least one** of the checked objects would have been offered
/// it on its own, and applies only to those objects: starting a mixed selection starts
/// the stopped ones and leaves the running ones alone. The label says how many it will
/// touch, so the count is never a surprise.
///
/// Only mutations are offered. Logs, Dockerfiles and filesystems open a tab each, and
/// twenty tabs is not a thing anyone asked for.
#[must_use]
pub fn for_selection(
    selected_containers: &[&ContainerSummary],
    selected_images: &[&ImageSummary],
    containers: &[ContainerSummary],
    images: &[ImageSummary],
) -> Vec<BulkOffer> {
    // The single-object rules decide what is possible; this only gathers.
    let per_container: Vec<(&ContainerSummary, Vec<Offer>)> = selected_containers
        .iter()
        .map(|container| (*container, for_container(container, images)))
        .collect();

    let mut offers = Vec::new();

    for action in BULK_LIFECYCLE {
        let wanted = Action::Lifecycle(action);
        let targets: Vec<BulkTarget> = per_container
            .iter()
            .filter(|(_, offered)| offered.iter().any(|offer| offer.action == wanted))
            .map(|(container, _)| BulkTarget {
                id: container.id.clone(),
                label: format::container_label(container),
                action: wanted,
            })
            .collect();

        if targets.is_empty() {
            continue;
        }

        let single = lifecycle(action);
        offers.push(BulkOffer {
            action: wanted,
            label: bulk_label(&single.label, targets.len(), "Container", "Containers"),
            icon: single.icon,
            targets,
            confirmation: None,
        });
    }

    if let Some(offer) = bulk_remove_containers(&per_container) {
        offers.push(offer);
    }
    if let Some(offer) = bulk_remove_images(selected_images, containers) {
        offers.push(offer);
    }

    offers
}

fn bulk_remove_containers(per_container: &[(&ContainerSummary, Vec<Offer>)]) -> Option<BulkOffer> {
    let targets: Vec<BulkTarget> = per_container
        .iter()
        .filter_map(|(container, offered)| {
            let action = offered
                .iter()
                .find(|offer| matches!(offer.action, Action::RemoveContainer { .. }))?
                .action;
            Some(BulkTarget {
                id: container.id.clone(),
                label: format::container_label(container),
                action,
            })
        })
        .collect();

    if targets.is_empty() {
        return None;
    }

    let running = targets
        .iter()
        .filter(|target| target.action == Action::RemoveContainer { force: true })
        .count();

    let caveat = if running == 0 {
        String::new()
    } else {
        format!(
            " {running} of them {} still running and will be killed first.",
            if running == 1 { "is" } else { "are" }
        )
    };

    let mut items: Vec<String> = targets.iter().map(|target| target.label.clone()).collect();
    items.sort_by_key(|label| label.to_lowercase());

    Some(BulkOffer {
        // The summary flag; each target carries the flag that applies to it.
        action: Action::RemoveContainer { force: running > 0 },
        label: bulk_label("Remove", targets.len(), "Container", "Containers"),
        icon: "user-trash-symbolic",
        confirmation: Some(Confirmation {
            heading: format!(
                "Remove {}?",
                count(targets.len(), "container", "containers")
            ),
            body: format!(
                "Anything written inside them since they started will be lost. The images \
                 they came from are not affected.{caveat}"
            ),
            confirm_label: "Remove All".to_owned(),
            items,
        }),
        targets,
    })
}

fn bulk_remove_images(
    selected: &[&ImageSummary],
    containers: &[ContainerSummary],
) -> Option<BulkOffer> {
    if selected.is_empty() {
        return None;
    }

    let targets: Vec<BulkTarget> = selected
        .iter()
        .map(|image| BulkTarget {
            id: image.id.clone(),
            label: format::image_label(image),
            action: Action::RemoveImage,
        })
        .collect();

    let reclaimed: i64 = selected.iter().map(|image| image.size).sum();
    // The daemon refuses rather than cascading, so say so before the attempt.
    let in_use = selected
        .iter()
        .filter(|image| !relations::containers_of(image, containers).is_empty())
        .count();

    let caveat = if in_use == 0 {
        String::new()
    } else {
        format!(
            " {in_use} of them {} still in use by a container, and the daemon will refuse \
             to remove {}.",
            if in_use == 1 { "is" } else { "are" },
            if in_use == 1 { "it" } else { "them" }
        )
    };

    let mut items: Vec<String> = targets.iter().map(|target| target.label.clone()).collect();
    items.sort_by_key(|label| label.to_lowercase());

    Some(BulkOffer {
        action: Action::RemoveImage,
        label: bulk_label("Remove", targets.len(), "Image", "Images"),
        icon: "user-trash-symbolic",
        confirmation: Some(Confirmation {
            heading: format!("Remove {}?", count(targets.len(), "image", "images")),
            body: format!(
                "{} of disk space would be reclaimed. Anything built from them later will \
                 have to pull or rebuild.{caveat}",
                format::bytes(reclaimed)
            ),
            confirm_label: "Remove All".to_owned(),
            items,
        }),
        targets,
    })
}

/// How a bulk action's outcome reads, and whether it counts as a failure.
///
/// Reported whatever happened. A removal that silently did nothing would leave the user
/// believing objects are gone when they are not, which is the worst available outcome —
/// so a partial failure names what went wrong rather than rounding it to "done".
#[must_use]
pub fn bulk_outcome(action: Action, succeeded: usize, failures: &[String]) -> (String, bool) {
    let noun = if action == Action::RemoveImage {
        ("image", "images")
    } else {
        ("container", "containers")
    };

    let done = format!(
        "{} {}",
        past_participle(action),
        count(succeeded, noun.0, noun.1)
    );

    if failures.is_empty() {
        return (done, false);
    }

    // Named rather than counted, up to a point: a toast that lists thirty containers is
    // not read by anyone.
    let named: Vec<&str> = failures
        .iter()
        .take(NAMED_FAILURES)
        .map(String::as_str)
        .collect();
    let rest = failures.len().saturating_sub(named.len());
    let tail = if rest == 0 {
        String::new()
    } else {
        format!(", and {rest} more")
    };

    let message = if succeeded == 0 {
        format!("Could not {}: {}{tail}", verb(action), named.join("; "))
    } else {
        format!(
            "{done}; {} failed: {}{tail}",
            failures.len(),
            named.join("; ")
        )
    };

    (message, true)
}

/// How many failures a message names before it starts counting them instead.
const NAMED_FAILURES: usize = 3;

fn past_participle(action: Action) -> &'static str {
    match action {
        Action::Lifecycle(Lifecycle::Start) => "Started",
        Action::Lifecycle(Lifecycle::Stop) => "Stopped",
        Action::Lifecycle(Lifecycle::Restart) => "Restarted",
        Action::Lifecycle(Lifecycle::Pause) => "Paused",
        Action::Lifecycle(Lifecycle::Unpause) => "Resumed",
        Action::Lifecycle(Lifecycle::Kill) => "Killed",
        Action::RemoveContainer { .. } | Action::RemoveImage => "Removed",
        Action::PruneContainers | Action::PruneImages => "Pruned",
        Action::ViewLogs
        | Action::ViewDockerfile
        | Action::BrowseFilesystem
        | Action::OpenInFileManager => "Opened",
    }
}

/// The infinitive, for saying what could not be done.
#[must_use]
pub fn verb(action: Action) -> &'static str {
    match action {
        Action::Lifecycle(lifecycle) => lifecycle.verb(),
        Action::RemoveContainer { .. } | Action::RemoveImage => "remove",
        Action::PruneContainers | Action::PruneImages => "prune",
        Action::ViewLogs
        | Action::ViewDockerfile
        | Action::BrowseFilesystem
        | Action::OpenInFileManager => "open",
    }
}

/// "Stop 2 Containers": the count is in the label so it is read before it is chosen.
fn bulk_label(verb: &str, targets: usize, singular: &str, plural: &str) -> String {
    format!(
        "{verb} {targets} {}",
        if targets == 1 { singular } else { plural }
    )
}

/// What is offered for the daemon as a whole. Each prune appears only when it has
/// something to do.
#[must_use]
pub fn for_environment(containers: &[ContainerSummary], images: &[ImageSummary]) -> Vec<Offer> {
    let mut offers = Vec::new();

    let stopped = prunable_containers(containers);
    if !stopped.is_empty() {
        offers.push(prune_containers_offer(&stopped));
    }

    let dangling = prunable_images(images);
    if !dangling.is_empty() {
        offers.push(prune_images_offer(&dangling, containers));
    }

    offers
}

/// The stopped containers a prune would remove, in the order the dialog lists them.
#[must_use]
pub fn prunable_containers(containers: &[ContainerSummary]) -> Vec<&ContainerSummary> {
    let mut prunable: Vec<&ContainerSummary> = containers
        .iter()
        .filter(|container| !container.state.is_active())
        .collect();
    prunable.sort_by_key(|container| format::container_label(container).to_lowercase());
    prunable
}

/// The dangling images a prune would remove. Anything still carrying a tag is excluded:
/// the daemon would remove those too without the filter, which is not what the button
/// says.
#[must_use]
pub fn prunable_images(images: &[ImageSummary]) -> Vec<&ImageSummary> {
    let mut prunable: Vec<&ImageSummary> = images
        .iter()
        .filter(|image| format::is_untagged(image))
        .collect();
    prunable.sort_by_key(|image| std::cmp::Reverse(image.created));
    prunable
}

fn lifecycle(action: Lifecycle) -> Offer {
    let (label, icon) = match action {
        Lifecycle::Start => ("Start", "media-playback-start-symbolic"),
        Lifecycle::Stop => ("Stop", "media-playback-stop-symbolic"),
        Lifecycle::Restart => ("Restart", "view-refresh-symbolic"),
        Lifecycle::Pause => ("Pause", "media-playback-pause-symbolic"),
        Lifecycle::Unpause => ("Resume", "media-playback-start-symbolic"),
        Lifecycle::Kill => ("Kill", "process-stop-symbolic"),
    };

    Offer::plain(Action::Lifecycle(action), label, icon)
}

fn remove_container_offer(container: &ContainerSummary, images: &[ImageSummary]) -> Offer {
    let name = format::container_label(container);
    let running = container.state.is_active();

    // Naming the image makes the consequence legible: what is lost is this container's
    // writable layer, not the image it came from.
    let image = relations::running_image(container, images)
        .map_or_else(|| container.image.clone(), format::image_label);

    let body = if running {
        format!(
            "{name} is still running. It will be killed and then removed, and anything \
             written inside it since it started from {image} will be lost. The image itself \
             is not affected."
        )
    } else {
        format!(
            "Anything written inside {name} since it started from {image} will be lost. \
             The image itself is not affected."
        )
    };

    Offer {
        action: Action::RemoveContainer { force: running },
        label: "Remove".to_owned(),
        icon: "user-trash-symbolic",
        confirmation: Some(Confirmation {
            heading: if running {
                format!("Kill and remove {name}?")
            } else {
                format!("Remove {name}?")
            },
            body,
            confirm_label: if running {
                "Kill and Remove".to_owned()
            } else {
                "Remove".to_owned()
            },
            items: Vec::new(),
        }),
    }
}

fn remove_image_offer(image: &ImageSummary, containers: &[ContainerSummary]) -> Offer {
    let name = format::image_label(image);
    let users = relations::containers_of(image, containers);

    let body = if users.is_empty() {
        format!(
            "{} of disk space will be reclaimed. Any container built from this image later \
             will have to pull or rebuild it.",
            format::bytes(image.size)
        )
    } else {
        // The daemon will refuse rather than cascade, so say so before the attempt
        // instead of surfacing a 409 afterwards.
        format!(
            "{} {} still using this image, so the daemon will refuse to remove it. Remove \
             {} first.",
            users.len(),
            if users.len() == 1 {
                "container is"
            } else {
                "containers are"
            },
            if users.len() == 1 { "it" } else { "them" }
        )
    };

    Offer {
        action: Action::RemoveImage,
        label: "Remove".to_owned(),
        icon: "user-trash-symbolic",
        confirmation: Some(Confirmation {
            heading: format!("Remove {name}?"),
            body,
            confirm_label: "Remove".to_owned(),
            items: users
                .iter()
                .map(|container| format::container_label(container))
                .collect(),
        }),
    }
}

fn prune_containers_offer(prunable: &[&ContainerSummary]) -> Offer {
    Offer {
        action: Action::PruneContainers,
        label: "Prune Containers".to_owned(),
        icon: "user-trash-symbolic",
        confirmation: Some(Confirmation {
            heading: format!(
                "Remove {}?",
                count(prunable.len(), "stopped container", "stopped containers")
            ),
            body: "Each one's writable layer goes with it. Running containers are not \
                   affected."
                .to_owned(),
            confirm_label: "Remove All".to_owned(),
            items: prunable
                .iter()
                .map(|container| format::container_label(container))
                .collect(),
        }),
    }
}

fn prune_images_offer(prunable: &[&ImageSummary], containers: &[ContainerSummary]) -> Offer {
    let reclaimed: i64 = prunable.iter().map(|image| image.size).sum();

    // An untagged image is often what a running container is actually on, after its tag
    // moved. Removing it would work but leaves the container unable to be recreated.
    let in_use = prunable
        .iter()
        .filter(|image| !relations::containers_of(image, containers).is_empty())
        .count();

    let caveat = if in_use == 0 {
        String::new()
    } else {
        format!(
            " {in_use} of them {} still in use by a container and will be kept by the daemon.",
            if in_use == 1 { "is" } else { "are" }
        )
    };

    let body = format!(
        "These images have no tag, usually because a tag moved to a newer build. {} \
         would be reclaimed. Tagged images are not affected.{caveat}",
        format::bytes(reclaimed)
    );

    Offer {
        action: Action::PruneImages,
        label: "Prune Images".to_owned(),
        icon: "user-trash-symbolic",
        confirmation: Some(Confirmation {
            heading: format!(
                "Remove {}?",
                count(prunable.len(), "untagged image", "untagged images")
            ),
            body,
            confirm_label: "Remove All".to_owned(),
            items: prunable
                .iter()
                .map(|image| format::image_label(image))
                .collect(),
        }),
    }
}

fn count(value: usize, singular: &str, plural: &str) -> String {
    if value == 1 {
        format!("1 {singular}")
    } else {
        format!("{value} {plural}")
    }
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn an_empty_listing_has_nothing_to_check() {
        let tally = Tally::new(0, 0);

        assert!(tally.is_empty());
        assert!(!tally.any());
        assert!(!tally.is_partial());
        assert!(!tally.is_complete());
        assert_eq!(tally.select_all_label(), "There is nothing to check");
    }

    #[test]
    fn a_listing_with_nothing_checked_offers_to_check_everything() {
        let tally = Tally::new(4, 0);

        assert!(!tally.any());
        assert!(!tally.is_partial());
        assert!(!tally.is_complete());
        assert_eq!(tally.select_all_label(), "Check every row");
    }

    #[test]
    fn a_part_checked_listing_is_neither_checked_nor_unchecked() {
        let tally = Tally::new(4, 1);

        assert!(tally.any());
        assert!(tally.is_partial());
        assert!(!tally.is_complete());
        // Half-checked, the useful next move is still to check the rest.
        assert_eq!(tally.select_all_label(), "Check every row");
    }

    #[test]
    fn a_fully_checked_listing_offers_to_clear_itself() {
        let tally = Tally::new(4, 4);

        assert!(tally.any());
        assert!(!tally.is_partial());
        assert!(tally.is_complete());
        assert_eq!(tally.select_all_label(), "Uncheck every row");
    }

    #[test]
    fn more_checked_than_present_cannot_report_a_mixed_state() {
        // Ticks outlive the objects they were made against for a moment, between an
        // action landing and the listing that follows it.
        let tally = Tally::new(2, 5);

        assert_eq!(tally.checked, 2);
        assert!(tally.is_complete());
        assert!(!tally.is_partial());
    }

    fn container(name: &str, state: ContainerState) -> ContainerSummary {
        ContainerSummary {
            id: format!("id-of-{name}"),
            names: vec![name.to_owned()],
            image: "nginx:1.27".to_owned(),
            image_id: "sha256:nginx".to_owned(),
            state,
            ..ContainerSummary::default()
        }
    }

    fn image(tag: &str) -> ImageSummary {
        ImageSummary {
            id: "sha256:nginx".to_owned(),
            repo_tags: vec![tag.to_owned()],
            size: 1024 * 1024 * 50,
            ..ImageSummary::default()
        }
    }

    fn untagged(id: &str, created: i64) -> ImageSummary {
        ImageSummary {
            id: id.to_owned(),
            repo_tags: Vec::new(),
            created,
            size: 1024 * 1024 * 10,
            ..ImageSummary::default()
        }
    }

    fn actions(offers: &[Offer]) -> Vec<Action> {
        offers.iter().map(|offer| offer.action).collect()
    }

    #[test]
    fn a_running_container_can_be_stopped_but_not_started() {
        let offers = for_container(&container("web", ContainerState::Running), &[]);

        assert!(actions(&offers).contains(&Action::Lifecycle(Lifecycle::Stop)));
        assert!(!actions(&offers).contains(&Action::Lifecycle(Lifecycle::Start)));
    }

    #[test]
    fn a_stopped_container_can_be_started_but_not_stopped() {
        let offers = for_container(&container("web", ContainerState::Exited), &[]);

        assert!(actions(&offers).contains(&Action::Lifecycle(Lifecycle::Start)));
        assert!(!actions(&offers).contains(&Action::Lifecycle(Lifecycle::Stop)));
        assert!(
            !actions(&offers).contains(&Action::Lifecycle(Lifecycle::Kill)),
            "killing something already stopped is meaningless"
        );
    }

    #[test]
    fn a_container_in_mid_transition_is_offered_no_further_transition() {
        for state in [ContainerState::Restarting, ContainerState::Stopping] {
            let offers = for_container(&container("web", state.clone()), &[]);

            let transitions: Vec<Action> = actions(&offers)
                .into_iter()
                .filter(|action| matches!(action, Action::Lifecycle(_)))
                .collect();

            // Restarting is active, so Kill remains as the way out of a restart loop.
            assert!(
                transitions
                    .iter()
                    .all(|action| *action == Action::Lifecycle(Lifecycle::Kill)),
                "{state:?} offered {transitions:?}"
            );
        }
    }

    #[test]
    fn logs_and_files_are_offered_whatever_the_state_because_both_work_stopped() {
        for state in [
            ContainerState::Running,
            ContainerState::Exited,
            ContainerState::Created,
            ContainerState::Dead,
        ] {
            let offers = for_container(&container("web", state.clone()), &[]);

            assert!(
                actions(&offers).contains(&Action::ViewLogs),
                "{state:?} should offer logs"
            );
            assert!(
                actions(&offers).contains(&Action::BrowseFilesystem),
                "{state:?} should offer the filesystem"
            );
        }
    }

    #[test]
    fn every_reversible_action_acts_without_a_dialog() {
        let offers = for_container(&container("web", ContainerState::Running), &[]);

        for offer in &offers {
            if !offer.is_destructive() {
                assert!(
                    offer.confirmation.is_none(),
                    "{:?} is reversible and should not confirm",
                    offer.action
                );
            }
        }
    }

    #[test]
    fn every_destructive_action_confirms() {
        let running = container("web", ContainerState::Running);
        let images = vec![image("nginx:1.27")];
        let stopped = container("old", ContainerState::Exited);

        let all: Vec<Offer> = for_container(&running, &images)
            .into_iter()
            .chain(for_image(&images[0], &[]))
            .chain(for_environment(&[stopped], &[untagged("sha256:d", 1)]))
            .collect();

        for offer in &all {
            assert_eq!(
                offer.is_destructive(),
                offer.confirmation.is_some(),
                "{:?} disagrees with its confirmation",
                offer.action
            );
        }
    }

    #[test]
    fn removing_a_running_container_says_it_will_be_killed_first() {
        let offers = for_container(
            &container("web", ContainerState::Running),
            &[image("nginx:1.27")],
        );

        let remove = offers
            .iter()
            .find(|offer| matches!(offer.action, Action::RemoveContainer { .. }))
            .expect("remove is offered");
        let confirmation = remove.confirmation.as_ref().expect("it confirms");

        assert_eq!(remove.action, Action::RemoveContainer { force: true });
        assert!(
            confirmation.body.contains("killed"),
            "{}",
            confirmation.body
        );
        assert!(confirmation.heading.contains("web"));
    }

    #[test]
    fn removing_a_stopped_container_does_not_threaten_to_kill_it() {
        let offers = for_container(
            &container("old", ContainerState::Exited),
            &[image("nginx:1.27")],
        );

        let remove = offers
            .iter()
            .find(|offer| matches!(offer.action, Action::RemoveContainer { .. }))
            .expect("remove is offered");
        let confirmation = remove.confirmation.as_ref().expect("it confirms");

        assert_eq!(remove.action, Action::RemoveContainer { force: false });
        assert!(
            !confirmation.body.contains("killed"),
            "{}",
            confirmation.body
        );
    }

    #[test]
    fn a_removal_names_the_object_rather_than_only_its_id() {
        let offers = for_container(
            &container("web", ContainerState::Exited),
            &[image("nginx:1.27")],
        );

        let confirmation = offers
            .iter()
            .find_map(|offer| offer.confirmation.as_ref())
            .expect("something confirms");

        assert!(
            confirmation.heading.contains("web"),
            "{}",
            confirmation.heading
        );
        assert!(
            confirmation.body.contains("nginx:1.27"),
            "the image should be named so the consequence is legible: {}",
            confirmation.body
        );
    }

    #[test]
    fn removing_an_image_a_container_still_uses_warns_it_will_be_refused() {
        let image = image("nginx:1.27");
        let user = container("web", ContainerState::Running);

        let offers = for_image(&image, &[user]);
        let confirmation = offers
            .iter()
            .find(|offer| offer.action == Action::RemoveImage)
            .and_then(|offer| offer.confirmation.as_ref())
            .expect("remove confirms");

        assert!(
            confirmation.body.contains("refuse"),
            "{}",
            confirmation.body
        );
        assert_eq!(confirmation.items, vec!["web".to_owned()]);
    }

    #[test]
    fn removing_an_unused_image_reports_what_it_reclaims() {
        let image = image("nginx:1.27");

        let confirmation = for_image(&image, &[])
            .into_iter()
            .find(|offer| offer.action == Action::RemoveImage)
            .and_then(|offer| offer.confirmation)
            .expect("remove confirms");

        assert!(confirmation.items.is_empty());
        // 50 MiB, rendered in the decimal units format::bytes uses throughout.
        assert!(
            confirmation.body.contains("52.4 MB"),
            "{}",
            confirmation.body
        );
    }

    #[test]
    fn prune_is_not_offered_when_there_is_nothing_to_prune() {
        let running = container("web", ContainerState::Running);
        let tagged = image("nginx:1.27");

        assert!(for_environment(&[running], &[tagged]).is_empty());
    }

    #[test]
    fn a_prune_preview_names_every_object_rather_than_counting_them() {
        let containers = vec![
            container("beta", ContainerState::Exited),
            container("alpha", ContainerState::Exited),
            container("live", ContainerState::Running),
        ];

        let offers = for_environment(&containers, &[]);
        let confirmation = offers
            .iter()
            .find(|offer| offer.action == Action::PruneContainers)
            .and_then(|offer| offer.confirmation.as_ref())
            .expect("prune confirms");

        // Named, sorted, and the running one is absent.
        assert_eq!(
            confirmation.items,
            vec!["alpha".to_owned(), "beta".to_owned()]
        );
        assert!(
            confirmation.heading.contains('2'),
            "{}",
            confirmation.heading
        );
    }

    #[test]
    fn pruning_images_covers_only_untagged_ones() {
        let images = vec![image("nginx:1.27"), untagged("sha256:aaa", 100)];

        let offers = for_environment(&[], &images);
        let confirmation = offers
            .iter()
            .find(|offer| offer.action == Action::PruneImages)
            .and_then(|offer| offer.confirmation.as_ref())
            .expect("prune confirms");

        assert_eq!(confirmation.items.len(), 1);
        assert!(
            !confirmation.items.iter().any(|item| item.contains("nginx")),
            "a tagged image must never appear in the preview: {:?}",
            confirmation.items
        );
    }

    #[test]
    fn pruning_images_warns_when_one_is_still_carrying_a_container() {
        let orphan = ImageSummary {
            id: "sha256:orphan".to_owned(),
            repo_tags: Vec::new(),
            ..ImageSummary::default()
        };
        let user = ContainerSummary {
            image_id: "sha256:orphan".to_owned(),
            names: vec!["web".to_owned()],
            state: ContainerState::Running,
            ..ContainerSummary::default()
        };

        let offers = for_environment(&[user], &[orphan]);
        let confirmation = offers
            .iter()
            .find(|offer| offer.action == Action::PruneImages)
            .and_then(|offer| offer.confirmation.as_ref())
            .expect("prune confirms");

        assert!(
            confirmation.body.contains("still in use"),
            "{}",
            confirmation.body
        );
    }

    #[test]
    fn only_what_removes_or_halts_is_marked_out_in_red() {
        let running = container("web", ContainerState::Running);
        let offers = for_container(&running, &[image("nginx:1.27")]);

        let tone = |label: &str| {
            offers
                .iter()
                .find(|offer| offer.label == label)
                .map(Offer::tone)
                .expect("that action is offered")
        };

        assert_eq!(tone("Stop"), Tone::Bad);
        assert_eq!(tone("Kill"), Tone::Bad);
        assert_eq!(tone("Remove"), Tone::Bad);
        assert_eq!(tone("Restart"), Tone::Neutral);
        assert_eq!(tone("Pause"), Tone::Neutral);
        assert_eq!(tone("Logs"), Tone::Neutral);
    }

    #[test]
    fn a_mixed_selection_offers_each_action_for_the_containers_it_applies_to() {
        let running = container("web", ContainerState::Running);
        let stopped = container("old", ContainerState::Exited);
        let also_stopped = container("older", ContainerState::Exited);

        let offers = for_selection(&[&running, &stopped, &also_stopped], &[], &[], &[]);

        let find = |action: Action| {
            offers
                .iter()
                .find(|offer| offer.action == action)
                .expect("that action is offered")
        };

        let start = find(Action::Lifecycle(Lifecycle::Start));
        assert_eq!(start.targets.len(), 2, "only the stopped ones can start");
        assert_eq!(start.label, "Start 2 Containers");

        let stop = find(Action::Lifecycle(Lifecycle::Stop));
        assert_eq!(stop.targets.len(), 1, "only the running one can stop");
        assert_eq!(stop.label, "Stop 1 Container");
        assert_eq!(stop.targets[0].id, running.id);
    }

    #[test]
    fn a_bulk_action_is_not_offered_when_it_applies_to_nothing_selected() {
        let stopped = container("old", ContainerState::Exited);

        let actions: Vec<Action> = for_selection(&[&stopped], &[], &[], &[])
            .iter()
            .map(|offer| offer.action)
            .collect();

        assert!(!actions.contains(&Action::Lifecycle(Lifecycle::Stop)));
        assert!(!actions.contains(&Action::Lifecycle(Lifecycle::Kill)));
        assert!(actions.contains(&Action::Lifecycle(Lifecycle::Start)));
    }

    #[test]
    fn nothing_checked_offers_nothing() {
        assert!(for_selection(&[], &[], &[], &[]).is_empty());
    }

    #[test]
    fn only_mutations_are_offered_in_bulk_never_the_things_that_open_a_tab() {
        let running = container("web", ContainerState::Running);
        let images = vec![image("nginx:1.27")];

        let actions: Vec<Action> = for_selection(&[&running], &[&images[0]], &[], &images)
            .iter()
            .map(|offer| offer.action)
            .collect();

        for unwanted in [
            Action::ViewLogs,
            Action::ViewDockerfile,
            Action::BrowseFilesystem,
            Action::OpenInFileManager,
        ] {
            assert!(
                !actions.contains(&unwanted),
                "{unwanted:?} opens a tab each"
            );
        }
    }

    #[test]
    fn a_bulk_removal_forces_per_container_rather_than_across_the_whole_selection() {
        let running = container("web", ContainerState::Running);
        let stopped = container("old", ContainerState::Exited);

        let remove = for_selection(&[&running, &stopped], &[], &[], &[])
            .into_iter()
            .find(|offer| matches!(offer.action, Action::RemoveContainer { .. }))
            .expect("removal is offered");

        let force_of = |id: &str| {
            remove
                .targets
                .iter()
                .find(|target| target.id == id)
                .map(|target| target.action)
                .expect("that container is a target")
        };

        assert_eq!(
            force_of(&running.id),
            Action::RemoveContainer { force: true }
        );
        assert_eq!(
            force_of(&stopped.id),
            Action::RemoveContainer { force: false },
            "a stopped container must not be force-removed just because another is running"
        );
    }

    #[test]
    fn a_bulk_removal_names_every_object_and_says_what_is_still_running() {
        let running = container("web", ContainerState::Running);
        let stopped = container("old", ContainerState::Exited);

        let remove = for_selection(&[&stopped, &running], &[], &[], &[])
            .into_iter()
            .find(|offer| matches!(offer.action, Action::RemoveContainer { .. }))
            .expect("removal is offered");
        let confirmation = remove.confirmation.expect("it confirms");

        assert_eq!(
            confirmation.items,
            vec!["old".to_owned(), "web".to_owned()],
            "named and sorted, not counted"
        );
        assert!(
            confirmation.heading.contains('2'),
            "{}",
            confirmation.heading
        );
        assert!(
            confirmation.body.contains("killed first"),
            "{}",
            confirmation.body
        );
    }

    #[test]
    fn removing_only_stopped_containers_does_not_threaten_to_kill_anything() {
        let stopped = container("old", ContainerState::Exited);

        let confirmation = for_selection(&[&stopped], &[], &[], &[])
            .into_iter()
            .find(|offer| matches!(offer.action, Action::RemoveContainer { .. }))
            .and_then(|offer| offer.confirmation)
            .expect("removal confirms");

        assert!(
            !confirmation.body.contains("killed"),
            "{}",
            confirmation.body
        );
    }

    #[test]
    fn images_and_containers_can_be_checked_together_and_each_gets_its_own_removal() {
        let running = container("web", ContainerState::Running);
        let unused = image("alpine:3.20");

        let offers = for_selection(&[&running], &[&unused], &[], std::slice::from_ref(&unused));
        let labels: Vec<&str> = offers.iter().map(|offer| offer.label.as_str()).collect();

        assert!(labels.contains(&"Remove 1 Container"), "{labels:?}");
        assert!(labels.contains(&"Remove 1 Image"), "{labels:?}");
    }

    #[test]
    fn a_bulk_image_removal_reports_what_it_reclaims_and_what_is_in_use() {
        let held = image("nginx:1.27");
        let holder = container("web", ContainerState::Running);

        let confirmation = for_selection(&[], &[&held], &[holder], std::slice::from_ref(&held))
            .into_iter()
            .find(|offer| offer.action == Action::RemoveImage)
            .and_then(|offer| offer.confirmation)
            .expect("removal confirms");

        assert!(
            confirmation.body.contains("52.4 MB"),
            "{}",
            confirmation.body
        );
        assert!(
            confirmation.body.contains("refuse"),
            "{}",
            confirmation.body
        );
        assert_eq!(confirmation.items, vec!["nginx:1.27".to_owned()]);
    }

    #[test]
    fn every_destructive_bulk_offer_confirms_and_every_reversible_one_does_not() {
        let running = container("web", ContainerState::Running);
        let stopped = container("old", ContainerState::Exited);
        let images = vec![image("nginx:1.27")];

        let offers = for_selection(&[&running, &stopped], &[&images[0]], &[], &images);
        assert!(!offers.is_empty(), "the selection should offer something");

        for offer in &offers {
            assert_eq!(
                offer.is_destructive(),
                offer.confirmation.is_some(),
                "{} disagrees with its confirmation",
                offer.label
            );
        }
    }

    #[test]
    fn bulk_actions_are_listed_with_the_disruptive_ones_last() {
        let running = container("web", ContainerState::Running);
        let stopped = container("old", ContainerState::Exited);
        let images = vec![image("nginx:1.27")];

        let offers = for_selection(&[&running, &stopped], &[&images[0]], &[], &images);
        let position = |action: Action| {
            offers
                .iter()
                .position(|offer| offer.action == action)
                .expect("that action is offered")
        };

        assert!(
            position(Action::Lifecycle(Lifecycle::Start))
                < position(Action::Lifecycle(Lifecycle::Stop))
        );
        assert!(
            position(Action::Lifecycle(Lifecycle::Stop))
                < position(Action::Lifecycle(Lifecycle::Kill))
        );
        assert!(
            offers
                .iter()
                .position(BulkOffer::is_destructive)
                .is_some_and(|first| offers[first..].iter().all(BulkOffer::is_destructive)),
            "removals come last, so nothing reversible sits below them"
        );
    }

    #[test]
    fn a_bulk_action_that_worked_says_what_it_did() {
        assert_eq!(
            bulk_outcome(Action::Lifecycle(Lifecycle::Stop), 3, &[]),
            ("Stopped 3 containers".to_owned(), false)
        );
        assert_eq!(
            bulk_outcome(Action::RemoveImage, 1, &[]),
            ("Removed 1 image".to_owned(), false)
        );
    }

    #[test]
    fn a_partly_failed_bulk_action_reports_both_halves_rather_than_rounding() {
        let (message, failed) = bulk_outcome(
            Action::RemoveContainer { force: false },
            2,
            &["web: in use".to_owned()],
        );

        assert!(failed);
        assert!(message.contains("Removed 2 containers"), "{message}");
        assert!(message.contains("web: in use"), "{message}");
    }

    #[test]
    fn a_wholly_failed_bulk_action_does_not_claim_to_have_removed_nothing() {
        let (message, failed) =
            bulk_outcome(Action::RemoveImage, 0, &["nginx: still in use".to_owned()]);

        assert!(failed);
        assert!(
            !message.contains("Removed 0"),
            "\"Removed 0 images\" reads as success: {message}"
        );
        assert!(message.starts_with("Could not remove"), "{message}");
    }

    #[test]
    fn a_long_list_of_failures_is_counted_after_the_first_few() {
        let failures: Vec<String> = (0..10).map(|index| format!("container-{index}")).collect();

        let (message, _) = bulk_outcome(Action::Lifecycle(Lifecycle::Start), 0, &failures);

        assert!(message.contains("container-0"), "{message}");
        assert!(!message.contains("container-9"), "{message}");
        assert!(message.contains("and 7 more"), "{message}");
    }

    #[test]
    fn a_container_being_removed_is_not_offered_removal_again() {
        let offers = for_container(&container("web", ContainerState::Removing), &[]);

        assert!(
            !actions(&offers)
                .iter()
                .any(|action| matches!(action, Action::RemoveContainer { .. }))
        );
    }
}
