//! The desktop panel indicator, published as a `StatusNotifierItem`.
//!
//! A thin adapter: what the indicator says is decided in `lave_core::indicator`.
//! Activations arrive on a D-Bus task, never on the GTK main thread, so they are sent
//! back through the update channel rather than touching widgets.

use async_channel::Sender;
use ksni::menu::StandardItem;
use ksni::{Tray, TrayMethods};
use lave_core::activity::Activity;
use lave_core::indicator::{self, Counts, IndicatorModel, MenuAction, MenuItem};

use crate::runtime::Update;

const APP_ID: &str = "com.paperstack.LaveStation";

pub struct TrayHandle(ksni::Handle<LaveTray>);

pub struct LaveTray {
    model: IndicatorModel,
    updates: Sender<Update>,
}

impl Tray for LaveTray {
    fn id(&self) -> String {
        APP_ID.to_owned()
    }

    fn title(&self) -> String {
        "Lave Station".to_owned()
    }

    /// Stock Adwaita names, so the icon resolves without installing the app.
    fn icon_name(&self) -> String {
        self.model.icon.icon_name().to_owned()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_name: self.model.icon.icon_name().to_owned(),
            icon_pixmap: Vec::new(),
            title: "Lave Station".to_owned(),
            description: self.model.tooltip.clone(),
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.request(Update::OpenRequested);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        self.model
            .items
            .iter()
            .map(|item| match item {
                MenuItem::Separator => ksni::MenuItem::Separator,
                MenuItem::Info(text) => StandardItem {
                    label: escape(text),
                    enabled: false,
                    ..StandardItem::default()
                }
                .into(),
                MenuItem::Action { action, label } => {
                    let action = *action;
                    StandardItem {
                        label: escape(label),
                        activate: Box::new(move |tray: &mut Self| match action {
                            MenuAction::Open => tray.request(Update::OpenRequested),
                            MenuAction::Quit => tray.request(Update::QuitRequested),
                        }),
                        ..StandardItem::default()
                    }
                    .into()
                }
            })
            .collect()
    }
}

impl LaveTray {
    fn request(&self, update: Update) {
        if self.updates.try_send(update).is_err() {
            tracing::warn!("the window is no longer listening to the indicator");
        }
    }
}

/// Menu labels treat a doubled underscore as a literal one.
fn escape(label: &str) -> String {
    label.replace('_', "__")
}

/// Publish the indicator, if the desktop has anywhere to put it.
pub async fn start(
    updates: Sender<Update>,
    activity: &Activity,
    counts: Counts,
) -> Option<TrayHandle> {
    if !host_available().await {
        tracing::warn!("no StatusNotifier host is registered; the panel indicator will not appear");
        return None;
    }

    let tray = LaveTray {
        model: indicator::model(activity, counts),
        updates,
    };

    match tray.spawn().await {
        Ok(handle) => Some(TrayHandle(handle)),
        Err(error) => {
            tracing::warn!("could not publish the panel indicator: {error}");
            None
        }
    }
}

pub async fn refresh(handle: &TrayHandle, activity: &Activity, counts: Counts) {
    let model = indicator::model(activity, counts);
    handle.0.update(move |tray| tray.model = model).await;
}

/// GNOME ships no `StatusNotifier` host of its own, so this is routinely false there.
/// The window falls back to quitting on close when it is.
async fn host_available() -> bool {
    let Ok(connection) = zbus::Connection::session().await else {
        return false;
    };

    let proxy = zbus::Proxy::new(
        &connection,
        "org.kde.StatusNotifierWatcher",
        "/StatusNotifierWatcher",
        "org.kde.StatusNotifierWatcher",
    )
    .await;

    match proxy {
        Ok(proxy) => proxy
            .get_property::<bool>("IsStatusNotifierHostRegistered")
            .await
            .unwrap_or(false),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underscores_in_labels_survive_the_menu() {
        assert_eq!(escape("pub_sub"), "pub__sub");
        assert_eq!(escape("web"), "web");
    }
}
