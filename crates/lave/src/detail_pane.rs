//! Turns a [`DetailPage`] into widgets. No decisions here — see `lave_core::model::detail`.

use adw::prelude::*;
use lave_core::model::detail::DetailPage;

/// Replace the page's contents. Returns the groups added, so they can be removed next
/// time: `AdwPreferencesPage` has no clear-all.
#[must_use]
pub fn render(
    page: &adw::PreferencesPage,
    previous: Vec<adw::PreferencesGroup>,
    detail: &DetailPage,
) -> Vec<adw::PreferencesGroup> {
    for group in previous {
        page.remove(&group);
    }

    let mut added = Vec::new();

    for group in &detail.groups {
        let widget = adw::PreferencesGroup::builder().title(&group.title).build();

        for row in &group.rows {
            let action = adw::ActionRow::builder()
                .title(&row.label)
                .subtitle(&row.value)
                .subtitle_selectable(true)
                .build();
            // Adwaita's .property class emphasises the value over the label.
            action.add_css_class("property");
            widget.add(&action);
        }

        page.add(&widget);
        added.push(widget);
    }

    if let Some(raw) = &detail.raw {
        let group = adw::PreferencesGroup::new();
        let expander = adw::ExpanderRow::builder()
            .title("Raw inspect output")
            .subtitle("As reported by the daemon")
            .build();

        let label = gtk::Label::builder()
            .label(raw)
            .selectable(true)
            .wrap(false)
            .xalign(0.0)
            .build();
        label.add_css_class("raw-inspect");

        let scroller = gtk::ScrolledWindow::builder()
            .child(&label)
            .min_content_height(320)
            .propagate_natural_height(true)
            .build();

        expander.add_row(&scroller);
        group.add(&expander);
        page.add(&group);
        added.push(group);
    }

    added
}
