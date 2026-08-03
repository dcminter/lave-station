//! Turns a [`DetailPage`] into widgets. No decisions here — see `lave_core::model::detail`.

use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use lave_core::model::detail::{ContainerFilter, DetailGroup, DetailPage};
use lave_core::model::tree::NodeId;

use crate::table_view::{self, SortOrder, TableHandlers};

/// Below this the groups will not sit two abreast, so the flow box folds to one column.
const GROUP_MIN_WIDTH: i32 = 380;
/// Groups are clamped for readability; tables are not, since width is the point of them.
const GROUP_CLAMP_WIDTH: i32 = 1500;

/// What the pane needs to be told, beyond the page itself.
pub struct Handlers {
    /// A row or link was chosen: select that object in the sidebar.
    pub navigate: Rc<dyn Fn(NodeId)>,
    /// The running-only / all toggle was operated.
    pub set_show_stopped: Rc<dyn Fn(bool)>,
    /// Everything the table itself reports, already scoped to the table on this page.
    pub table: TableHandlers,
    /// The bulk-action button has been built: the window drives its sensitivity and
    /// fills in its menu, both of which depend on what is checked at the time.
    pub cog_ready: Rc<dyn Fn(gtk::MenuButton)>,
}

/// How the table on this page is currently viewed. Neither is part of the page itself:
/// the sort lasts for the session, and the widths outlive the run.
pub struct TableState {
    pub sort: SortOrder,
    /// By column title.
    pub widths: BTreeMap<String, i32>,
}

/// Replace the pane's contents.
///
/// `lead` is the draggable upper half, used only by a page whose table comes first;
/// it is hidden otherwise. `body` scrolls and holds everything else.
pub fn render(
    lead: &gtk::Box,
    body: &gtk::Box,
    detail: &DetailPage,
    state: &TableState,
    handlers: &Handlers,
) {
    table_view::clear(lead);
    table_view::clear(body);

    let table = detail
        .table
        .as_ref()
        .map(|table| table_section(table, detail, state, handlers));

    let leading = detail.table_first && table.is_some();
    lead.set_visible(leading);

    if let Some(table) = &table
        && leading
    {
        lead.append(table);
    }

    if !detail.groups.is_empty() {
        body.append(&clamped(&group_flow(&detail.groups, &handlers.navigate)));
    }

    if let Some(table) = &table
        && !leading
    {
        body.append(table);
    }

    if let Some(raw) = &detail.raw {
        body.append(&clamped(&raw_group(raw)));
    }
}

/// Groups are held to a readable measure; tables are deliberately not.
fn clamped(child: &impl IsA<gtk::Widget>) -> adw::Clamp {
    adw::Clamp::builder()
        .maximum_size(GROUP_CLAMP_WIDTH)
        .tightening_threshold(900)
        .child(child)
        .build()
}

fn table_section(
    table: &lave_core::model::table::Table,
    detail: &DetailPage,
    state: &TableState,
    handlers: &Handlers,
) -> gtk::Widget {
    let section = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build();

    section.append(&table_header(detail, handlers));

    let view = table_view::build(table, &state.sort, &state.widths, &handlers.table);

    let frame = gtk::Frame::new(None);
    frame.add_css_class("view");

    if detail.table_first {
        // The paned above decides the height; anything beyond it scrolls, which is what
        // dragging the divider reveals.
        let scroller = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .build();
        frame.set_child(Some(&scroller));
        frame.set_vexpand(true);
    } else {
        frame.set_child(Some(&view));
    }

    section.append(&frame);
    section.upcast()
}

/// The strip above a table: bulk actions on the left, the filter on the right.
fn table_header(detail: &DetailPage, handlers: &Handlers) -> gtk::Box {
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();

    // Insensitive until something is checked, which is the window's business: what is
    // checked outlives this widget.
    let cog = gtk::MenuButton::builder()
        .icon_name("emblem-system-symbolic")
        .tooltip_text("Act on the checked rows")
        .valign(gtk::Align::Center)
        .build();
    cog.add_css_class("flat");
    cog.update_property(&[gtk::accessible::Property::Label("Act on the checked rows")]);
    header.append(&cog);
    (handlers.cog_ready)(cog);

    if let Some(filter) = &detail.table_filter {
        let toggle = filter_toggle(filter, &handlers.set_show_stopped);
        toggle.set_hexpand(true);
        header.append(&toggle);
    }

    header
}

/// Two linked toggle buttons, in the manner of a view switcher.
fn filter_toggle(filter: &ContainerFilter, set_show_stopped: &Rc<dyn Fn(bool)>) -> gtk::Box {
    let holder = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .halign(gtk::Align::End)
        .build();

    let running = gtk::ToggleButton::builder()
        .label(&filter.running_label)
        .active(!filter.showing_all)
        .build();
    let all = gtk::ToggleButton::builder()
        .label(&filter.all_label)
        .active(filter.showing_all)
        .group(&running)
        .build();

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .build();
    buttons.add_css_class("linked");
    buttons.append(&running);
    buttons.append(&all);
    holder.append(&buttons);

    // Only act on the button becoming active, so the pair reports one change, not two.
    let handler = Rc::clone(set_show_stopped);
    running.connect_toggled(move |button| {
        if button.is_active() {
            handler(false);
        }
    });
    let handler = Rc::clone(set_show_stopped);
    all.connect_toggled(move |button| {
        if button.is_active() {
            handler(true);
        }
    });

    holder
}

/// Groups laid out in as many columns as the width allows, at most two. `AdwWrapBox`
/// would be the natural fit but arrived in libadwaita 1.7, above this application's
/// floor of 1.5, so a flow box does the same job.
fn group_flow(groups: &[DetailGroup], on_navigate: &Rc<dyn Fn(NodeId)>) -> gtk::FlowBox {
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(2)
        .homogeneous(true)
        .row_spacing(18)
        .column_spacing(18)
        .build();

    for group in groups {
        let widget = adw::PreferencesGroup::builder()
            .title(&group.title)
            .width_request(GROUP_MIN_WIDTH)
            .valign(gtk::Align::Start)
            .build();

        for row in &group.rows {
            widget.add(&action_row(row, on_navigate));
        }

        flow.append(&widget);
    }

    flow
}

fn action_row(
    row: &lave_core::model::detail::DetailRow,
    on_navigate: &Rc<dyn Fn(NodeId)>,
) -> adw::ActionRow {
    let action = adw::ActionRow::builder()
        .title(&row.label)
        .subtitle(&row.value)
        .subtitle_selectable(row.link.is_none())
        .build();
    // Adwaita's .property class emphasises the value over the label.
    action.add_css_class("property");

    if let Some(target) = row.link.clone() {
        // Selectable text swallows clicks, so a navigable row gives that up.
        action.set_activatable(true);
        action.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        action.set_tooltip_text(Some("Show this in the sidebar"));

        let on_navigate = Rc::clone(on_navigate);
        action.connect_activated(move |_| on_navigate(target.clone()));
    }

    action
}

fn raw_group(raw: &str) -> adw::PreferencesGroup {
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
    group
}
