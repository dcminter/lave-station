//! What the desktop panel indicator shows.
//!
//! Pure data. `crates/lave/src/indicator_tray.rs` renders it as a `StatusNotifierItem`;
//! nothing here knows about D-Bus.

use crate::activity::{Activity, ActivityState};

/// Indicator states. Each has a distinct icon shape as well as distinct text, so no
/// information is carried by colour alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicatorIcon {
    Connected,
    Connecting,
    Reconnecting,
    Failed,
}

impl IndicatorIcon {
    /// Fallback themed icon name, used when the embedded pixmaps are unavailable.
    #[must_use]
    pub fn icon_name(self) -> &'static str {
        match self {
            IndicatorIcon::Connected => "media-playback-start-symbolic",
            IndicatorIcon::Connecting => "content-loading-symbolic",
            IndicatorIcon::Reconnecting => "view-refresh-symbolic",
            IndicatorIcon::Failed => "dialog-warning-symbolic",
        }
    }
}

/// Menu entries the user can invoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Open,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuItem {
    Action {
        action: MenuAction,
        label: String,
    },
    /// Non-interactive status text.
    Info(String),
    Separator,
}

/// What the tree currently holds, for the menu's summary lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub images: usize,
    pub containers: usize,
    pub running: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndicatorModel {
    pub icon: IndicatorIcon,
    pub tooltip: String,
    pub items: Vec<MenuItem>,
}

/// Build the indicator from the activity state and the current counts.
#[must_use]
pub fn model(activity: &Activity, counts: Counts) -> IndicatorModel {
    let (icon, status) = match activity.state() {
        ActivityState::Connected => (IndicatorIcon::Connected, "Connected".to_owned()),
        ActivityState::Connecting => (IndicatorIcon::Connecting, "Connecting\u{2026}".to_owned()),
        ActivityState::Reconnecting { delay, .. } => (
            IndicatorIcon::Reconnecting,
            format!("Reconnecting in {}s", delay.as_secs()),
        ),
        ActivityState::Failed { reason, .. } => {
            (IndicatorIcon::Failed, format!("Disconnected: {reason}"))
        }
    };

    let mut items = vec![
        MenuItem::Action {
            action: MenuAction::Open,
            label: "Open Lave Station".to_owned(),
        },
        MenuItem::Separator,
        MenuItem::Info(status.clone()),
    ];

    if activity.state().is_connected() {
        items.push(MenuItem::Info(plural(counts.images, "image", "images")));
        items.push(MenuItem::Info(containers_line(counts)));
    }

    if let Some(latest) = activity.log().front() {
        items.push(MenuItem::Separator);
        items.push(MenuItem::Info(latest.text.clone()));
    }

    items.push(MenuItem::Separator);
    items.push(MenuItem::Action {
        action: MenuAction::Quit,
        label: "Quit".to_owned(),
    });

    IndicatorModel {
        icon,
        tooltip: format!("Lave Station \u{2014} {status}"),
        items,
    }
}

fn containers_line(counts: Counts) -> String {
    let containers = plural(counts.containers, "container", "containers");
    if counts.running == 0 {
        containers
    } else {
        format!("{containers} ({} running)", counts.running)
    }
}

fn plural(count: usize, singular: &str, plural: &str) -> String {
    if count == 1 {
        format!("1 {singular}")
    } else {
        format!("{count} {plural}")
    }
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;
    use crate::activity::Signal;
    use crate::engine::{EngineError, EngineEvent};
    use std::io::ErrorKind;
    use std::path::Path;

    fn counts() -> Counts {
        Counts {
            images: 16,
            containers: 5,
            running: 1,
        }
    }

    fn connected() -> Activity {
        let mut activity = Activity::new();
        activity.apply(Signal::Connected { at: 1000 });
        activity
    }

    fn labels(model: &IndicatorModel) -> Vec<String> {
        model
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { label, .. } | MenuItem::Info(label) => Some(label.clone()),
                MenuItem::Separator => None,
            })
            .collect()
    }

    fn actions(model: &IndicatorModel) -> Vec<MenuAction> {
        model
            .items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { action, .. } => Some(*action),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_menu_always_offers_open_and_quit() {
        for activity in [Activity::new(), connected()] {
            let model = model(&activity, counts());
            assert_eq!(actions(&model), vec![MenuAction::Open, MenuAction::Quit]);
        }
    }

    #[test]
    fn a_connected_indicator_reports_the_counts() {
        let model = model(&connected(), counts());

        assert_eq!(model.icon, IndicatorIcon::Connected);
        assert!(labels(&model).contains(&"16 images".to_owned()));
        assert!(labels(&model).contains(&"5 containers (1 running)".to_owned()));
        assert_eq!(model.tooltip, "Lave Station \u{2014} Connected");
    }

    #[test]
    fn counts_are_pluralised() {
        let single = Counts {
            images: 1,
            containers: 1,
            running: 0,
        };

        let rendered = labels(&model(&connected(), single));

        assert!(rendered.contains(&"1 image".to_owned()));
        assert!(rendered.contains(&"1 container".to_owned()));
    }

    #[test]
    fn a_zero_running_count_is_omitted_rather_than_shown_as_zero() {
        let idle = Counts {
            images: 3,
            containers: 2,
            running: 0,
        };

        assert!(labels(&model(&connected(), idle)).contains(&"2 containers".to_owned()));
    }

    #[test]
    fn counts_are_hidden_while_disconnected_because_they_would_be_stale() {
        let model = model(&Activity::new(), counts());

        assert_eq!(model.icon, IndicatorIcon::Connecting);
        assert!(!labels(&model).contains(&"16 images".to_owned()));
    }

    #[test]
    fn reconnecting_says_how_long_the_wait_is() {
        let mut activity = Activity::new();
        let error =
            EngineError::unreachable(Path::new("/var/run/docker.sock"), ErrorKind::NotFound);
        activity.apply(Signal::Lost(error));

        let model = model(&activity, counts());

        assert_eq!(model.icon, IndicatorIcon::Reconnecting);
        assert!(
            model.tooltip.contains("Reconnecting in 1s"),
            "got {}",
            model.tooltip
        );
    }

    #[test]
    fn a_failure_names_the_reason_in_the_tooltip() {
        let mut activity = Activity::new();
        activity.apply(Signal::Lost(EngineError::Protocol("bad frame".to_owned())));

        let model = model(&activity, counts());

        assert_eq!(model.icon, IndicatorIcon::Failed);
        assert!(model.tooltip.contains("bad frame"), "got {}", model.tooltip);
    }

    #[test]
    fn the_most_recent_event_is_surfaced_in_the_menu() {
        let mut activity = connected();
        activity.apply(Signal::Observed(EngineEvent {
            kind: "container".to_owned(),
            action: "start".to_owned(),
            actor_id: "abc".to_owned(),
            actor_name: Some("web".to_owned()),
            time: 1001,
        }));

        assert!(labels(&model(&activity, counts())).contains(&"container start: web".to_owned()));
    }

    #[test]
    fn every_state_has_its_own_icon_so_colour_is_never_the_only_cue() {
        let icons = [
            IndicatorIcon::Connected,
            IndicatorIcon::Connecting,
            IndicatorIcon::Reconnecting,
            IndicatorIcon::Failed,
        ];

        let mut names: Vec<&str> = icons.iter().map(|icon| icon.icon_name()).collect();
        names.sort_unstable();
        names.dedup();

        assert_eq!(names.len(), icons.len(), "icon names must be distinct");
    }

    #[test]
    fn separators_never_lead_or_trail_the_menu() {
        let model = model(&connected(), counts());

        assert!(!matches!(model.items.first(), Some(MenuItem::Separator)));
        assert!(!matches!(model.items.last(), Some(MenuItem::Separator)));
    }
}
