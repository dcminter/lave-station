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
    fn a_container_being_removed_is_not_offered_removal_again() {
        let offers = for_container(&container("web", ContainerState::Removing), &[]);

        assert!(
            !actions(&offers)
                .iter()
                .any(|action| matches!(action, Action::RemoveContainer { .. }))
        );
    }
}
