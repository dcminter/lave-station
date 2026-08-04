//! Turns a [`DetailPage`] into widgets. No decisions here — see `lave_core::model::detail`.

use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use lave_core::model::action::Offer;
use lave_core::model::detail::{DetailGroup, DetailPage, FilterKind, TableFilter};
use lave_core::model::tree::{NodeId, Tone};

use crate::group_columns::GroupColumns;
use crate::table_view::{self, SortOrder, TableHandlers};

/// Groups are clamped for readability; tables are not, since width is the point of them.
const GROUP_CLAMP_WIDTH: i32 = 1500;

/// What the pane needs to be told, beyond the page itself.
pub struct Handlers {
    /// A row or link was chosen: select that object in the sidebar.
    pub navigate: Rc<dyn Fn(NodeId)>,
    /// A table's filter toggle was operated: the kind of filter, and whether the rows it
    /// would leave out are now wanted.
    pub set_filter: Rc<dyn Fn(FilterKind, bool)>,
    /// Everything the table itself reports, already scoped to the table on this page.
    pub table: TableHandlers,
    /// The bulk-action button has been built: the window drives its sensitivity and
    /// fills in its menu, both of which depend on what is checked at the time.
    pub cog_ready: Rc<dyn Fn(gtk::MenuButton)>,
    /// Likewise the select-all control, whose own state is a summary of the row ticks.
    pub select_all_ready: Rc<dyn Fn(gtk::CheckButton)>,
    /// A button in the action strip was pressed, by index into the page's own actions.
    pub act: Rc<dyn Fn(usize)>,
}

/// One page's worth of widgets: a draggable table above, and everything else scrolling
/// below it.
///
/// Built here rather than in the window template because there is one per open detail
/// tab: the pages are all open at once, and a widget has one parent, so there is nothing
/// to share. Cloning one is cloning three reference-counted handles to the same widgets.
#[derive(Clone)]
pub struct Surface {
    /// What goes in a tab.
    pub paned: gtk::Paned,
    /// The upper half, shown only for a page whose table leads.
    pub lead: gtk::Box,
    /// The lower half, which scrolls.
    pub body: gtk::Box,
}

impl Default for Surface {
    fn default() -> Self {
        Self::new()
    }
}

impl Surface {
    #[must_use]
    pub fn new() -> Self {
        let lead = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(18)
            .margin_start(18)
            .margin_end(18)
            .margin_bottom(6)
            .build();

        // Unclamped: tables span the full width, and the groups clamp themselves.
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build();

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&body)
            .build();

        let paned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .resize_start_child(false)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .start_child(&lead)
            .end_child(&scroller)
            .build();

        Self { paned, lead, body }
    }
}

/// How the table on this page is currently viewed. Neither is part of the page itself:
/// the sort lasts for the session, and the widths outlive the run.
pub struct TableState {
    pub sort: SortOrder,
    /// By column title.
    pub widths: BTreeMap<String, i32>,
}

/// Replace a surface's contents.
///
/// The surface's upper half is used only by a page whose table comes first, and is
/// hidden otherwise; the lower half scrolls and holds everything else.
pub fn render(surface: &Surface, detail: &DetailPage, state: &TableState, handlers: &Handlers) {
    let (lead, body) = (&surface.lead, &surface.body);
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

    // First line of an object's page: what may be done to it, without going back to the
    // table it was reached from.
    if detail.shows_action_bar() {
        body.append(&clamped(&action_bar(&detail.actions, &handlers.act)));
    }

    if !detail.groups.is_empty() {
        body.append(&clamped(&group_columns(&detail.groups, &handlers.navigate)));
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

/// The offers for one object, as a row of buttons that wraps when the pane is too narrow
/// to hold them all.
///
/// A flow box is right here and was wrong for the groups: these are all one line tall, so
/// giving every child in a line the height of the tallest costs nothing.
fn action_bar(actions: &[Offer], act: &Rc<dyn Fn(usize)>) -> gtk::FlowBox {
    let bar = gtk::FlowBox::builder()
        .orientation(gtk::Orientation::Horizontal)
        .selection_mode(gtk::SelectionMode::None)
        .column_spacing(6)
        .row_spacing(6)
        .max_children_per_line(u32::try_from(actions.len().max(1)).unwrap_or(1))
        .build();

    for (index, offer) in actions.iter().enumerate() {
        bar.append(&action_button(offer, index, act));
    }

    bar
}

/// One action: the icon it carries in the context menu, tinted the same way, beside the
/// label that says what it does.
fn action_button(offer: &Offer, index: usize, act: &Rc<dyn Fn(usize)>) -> gtk::Button {
    let icon = gtk::Image::from_icon_name(offer.icon);
    if offer.tone() == Tone::Bad {
        icon.add_css_class(offer.tone().css_class());
    }

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    content.append(&icon);
    content.append(&gtk::Label::new(Some(&offer.label)));

    let button = gtk::Button::builder().child(&content).build();

    let act = Rc::clone(act);
    button.connect_clicked(move |_| act(index));
    button
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

/// The strip above a table: select-all and bulk actions on the left, the filter on the
/// right.
fn table_header(detail: &DetailPage, handlers: &Handlers) -> gtk::Box {
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();

    // Lines up with the column of row checkboxes below it, so what it governs is legible
    // without a label. Its three states are the window's business.
    let select_all = gtk::CheckButton::builder()
        .tooltip_text("Check every row")
        .valign(gtk::Align::Center)
        .margin_start(6)
        .margin_end(6)
        .build();
    select_all.update_property(&[gtk::accessible::Property::Label("Check every row")]);
    header.append(&select_all);
    (handlers.select_all_ready)(select_all);

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
        let toggle = filter_toggle(filter, &handlers.set_filter);
        toggle.set_hexpand(true);
        header.append(&toggle);
    }

    header
}

/// Two linked toggle buttons, in the manner of a view switcher.
fn filter_toggle(filter: &TableFilter, set_filter: &Rc<dyn Fn(FilterKind, bool)>) -> gtk::Box {
    let holder = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .halign(gtk::Align::End)
        .build();

    let narrow = gtk::ToggleButton::builder()
        .label(&filter.narrow_label)
        .active(!filter.showing_all)
        .build();
    let all = gtk::ToggleButton::builder()
        .label(&filter.all_label)
        .active(filter.showing_all)
        .group(&narrow)
        .build();

    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .build();
    buttons.add_css_class("linked");
    buttons.append(&narrow);
    buttons.append(&all);
    holder.append(&buttons);

    // Only act on the button becoming active, so the pair reports one change, not two.
    for (button, showing_all) in [(&narrow, false), (&all, true)] {
        let handler = Rc::clone(set_filter);
        let kind = filter.kind;
        button.connect_toggled(move |button| {
            if button.is_active() {
                handler(kind, showing_all);
            }
        });
    }

    holder
}

/// Groups laid out in as many columns as the width allows, at most two, each group only
/// as tall as its own contents.
///
/// A flow box was the obvious widget and is the wrong one: it lays out in lines and gives
/// every child in a line the height of the tallest, so a group of two rows beside one of
/// fifteen was allocated the height of the fifteen and left a crater below itself.
/// `AdwWrapBox` has the same line-based problem, and arrived in libadwaita 1.7 besides,
/// above this application's floor of 1.5.
fn group_columns(groups: &[DetailGroup], on_navigate: &Rc<dyn Fn(NodeId)>) -> GroupColumns {
    let columns = GroupColumns::new();

    for group in groups {
        let widget = adw::PreferencesGroup::builder().title(&group.title).build();

        for row in &group.rows {
            widget.add(&action_row(row, on_navigate));
        }

        columns.append(&widget);
    }

    columns
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
