//! Turns a [`DetailPage`] into widgets. No decisions here — see `lave_core::model::detail`.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use adw::prelude::*;
use lave_core::model::action::Offer;
use lave_core::model::detail::{DetailGroup, DetailPage, FilterKind, TableFilter};
use lave_core::model::tree::{NodeId, Tone};

use crate::group_columns::GroupColumns;
use crate::table_view::{self, SortOrder, TableHandlers, TableView};

/// Groups are clamped for readability; tables are not, since width is the point of them.
const GROUP_CLAMP_WIDTH: i32 = 1500;

/// Keeps a scroller where the reader left it across a redraw.
///
/// Replacing a scroller's contents empties it, and an empty scroller clamps to the top,
/// so the offset cannot simply be written back afterwards. It is held instead, and put
/// back once GTK has measured the new contents and the position exists again.
///
/// Redraws can arrive faster than GTK lays them out — a snapshot and a memory sample in
/// the same turn do — so an offset still waiting is never overwritten by the empty
/// scroller a previous redraw has just built.
#[derive(Clone, Default)]
struct ScrollMemory {
    /// Where to scroll back to, until that has been done.
    wanted: Rc<Cell<Option<f64>>>,
    /// The scroller this is keeping the place in.
    scroller: Rc<RefCell<Option<gtk::ScrolledWindow>>>,
}

impl ScrollMemory {
    /// Adopt a scroller, which is restored to the held offset as soon as it is tall
    /// enough to reach it. Once per scroller: the handler dies with the adjustment.
    fn watch(&self, scroller: &gtk::ScrolledWindow) {
        // Only the offset is captured, so a scroller is not held alive by a handler on
        // its own adjustment.
        let wanted = Rc::clone(&self.wanted);
        scroller.vadjustment().connect_changed(move |adjustment| {
            let Some(offset) = wanted.get() else {
                return;
            };
            // Nothing in it yet: this is the redraw emptying it, not the measurement of
            // what replaced it, so keep hold of the offset and wait.
            let reach = adjustment.upper() - adjustment.page_size();
            if reach <= 0.0 {
                return;
            }
            // Measured. A page that came back shorter than the reader had scrolled is
            // put at its end rather than left waiting for one that never comes.
            wanted.set(None);
            adjustment.set_value(offset.min(reach));
        });
        self.scroller.replace(Some(scroller.clone()));
    }

    /// Note where the reader is, before the contents are replaced.
    fn hold(&self) {
        // An offset still waiting to be applied is the reader's real place; what the
        // scroller reads now is the empty one an earlier redraw left behind.
        if self.wanted.get().is_some() {
            return;
        }
        let offset = self
            .scroller
            .borrow()
            .as_ref()
            .map(|scroller| scroller.vadjustment().value());
        self.wanted.set(offset);
    }
}

/// One property row's identity: its label, and where it navigates to if it does.
type RowShape = (String, Option<NodeId>);

/// A group's title and the rows under it, by identity rather than by value.
type GroupShape = (String, Vec<RowShape>);

/// The lower half of a page, apart from the values a refresh moves.
///
/// Two pages of the same shape are drawn by the same widgets: the values are written
/// into the rows already on screen. This is what a refresh compares against to decide
/// whether it has anything to build at all.
#[derive(PartialEq)]
struct BodyShape {
    /// Compared whole: a button carries the offer it was built from, so an offer that
    /// has changed is a button that has to be built again.
    actions: Vec<Offer>,
    /// Group titles, and the label and link of each row within them. Not the values.
    groups: Vec<GroupShape>,
    raw: bool,
    /// Whether the table sits below the groups, and so is one of these widgets.
    table_below: bool,
}

impl BodyShape {
    fn of(detail: &DetailPage, leading: bool) -> Self {
        Self {
            actions: if detail.shows_action_bar() {
                detail.actions.clone()
            } else {
                Vec::new()
            },
            groups: detail
                .groups
                .iter()
                .map(|group| {
                    let rows = group
                        .rows
                        .iter()
                        .map(|row| (row.label.clone(), row.link.clone()))
                        .collect();
                    (group.title.clone(), rows)
                })
                .collect(),
            raw: detail.raw.is_some(),
            table_below: detail.table.is_some() && !leading,
        }
    }

    /// Every row's identity, in the order the rows are built, so a row can be looked for
    /// again in a page that has changed shape around it.
    ///
    /// The group is part of it: "Images" is a row of Contents and a row of Footprint
    /// both.
    fn identities(&self) -> Vec<(String, String)> {
        self.groups
            .iter()
            .flat_map(|(title, rows)| {
                rows.iter()
                    .map(move |(label, _)| (title.clone(), label.clone()))
            })
            .collect()
    }
}

/// Where the focus was in the lower half, named by the row it was in rather than by the
/// widget holding it: the widgets are about to be thrown away.
struct FocusMark {
    /// The group's title and the row's label.
    row: (String, String),
    /// Whether it was the value inside the row rather than the row itself. A selectable
    /// value is its own focusable widget, and clicking the text lands in it.
    value: bool,
}

/// The widgets the lower half was built from, and what they were built for.
struct BodyWidgets {
    shape: BodyShape,
    /// Every property row, in the order the page lists them, so a refresh can write the
    /// new values straight into them.
    rows: Vec<adw::ActionRow>,
    /// The raw inspect output, which is the one other thing that changes under a page.
    raw: Option<gtk::Label>,
}

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
/// to share. Cloning one is cloning reference-counted handles to the same widgets.
#[derive(Clone)]
pub struct Surface {
    /// What goes in a tab.
    pub paned: gtk::Paned,
    /// The upper half, shown only for a page whose table leads.
    pub lead: gtk::Box,
    /// The lower half, which scrolls.
    pub body: gtk::Box,
    /// The table, built once and brought up to date thereafter.
    section: Rc<RefCell<Option<TableSection>>>,
    /// The lower half, likewise: rebuilt only when the page changes shape.
    body_widgets: Rc<RefCell<Option<BodyWidgets>>>,
    /// Where the lower half was scrolled to, for the redraws that do rebuild it.
    body_scroll: ScrollMemory,
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

        let body_scroll = ScrollMemory::default();
        body_scroll.watch(&scroller);

        Self {
            paned,
            lead,
            body,
            section: Rc::new(RefCell::new(None)),
            body_widgets: Rc::new(RefCell::new(None)),
            body_scroll,
        }
    }

    /// Whether the lower half on screen is already the right widgets for this page.
    fn body_holds(&self, shape: &BodyShape) -> bool {
        self.body_widgets
            .borrow()
            .as_ref()
            .is_some_and(|held| held.shape == *shape)
    }

    /// Remember what the lower half was just built from.
    fn body_built(&self, widgets: BodyWidgets) {
        self.body_widgets.replace(Some(widgets));
    }

    /// Write a refresh's values into the rows already on screen.
    ///
    /// Only what has moved is written: setting a label to what it already says is work
    /// for nothing, several times a minute, for every row of every open page.
    fn write_values(&self, detail: &DetailPage) {
        let held = self.body_widgets.borrow();
        let Some(widgets) = held.as_ref() else {
            return;
        };

        let values = detail
            .groups
            .iter()
            .flat_map(|group| group.rows.iter().map(|row| row.value.as_str()));

        for (action, value) in widgets.rows.iter().zip(values) {
            if action.subtitle().as_deref() != Some(value) {
                action.set_subtitle(value);
            }
        }

        if let (Some(label), Some(raw)) = (&widgets.raw, &detail.raw)
            && label.label() != *raw
        {
            label.set_label(raw);
        }
    }

    /// Note which row the focus is in, and take the focus off it.
    ///
    /// Taken off rather than left: the widget holding it is about to go, and GTK answers
    /// that by giving the focus to whatever it finds instead, which is a row at the top
    /// of the page. A scroller keeps its focused child in view, so that alone drags the
    /// page to the top — and keeps dragging it back there on every layout, however often
    /// the reader scrolls down again.
    ///
    /// The row is named rather than held: the widgets are about to be replaced, and it is
    /// the row the reader clicked into that has to be found among the new ones.
    fn take_focus(&self) -> Option<FocusMark> {
        let root = self.body.root()?;
        let focused = gtk::prelude::RootExt::focus(&root)?;

        // Focus anywhere else in the window is not this pane's to take.
        let ancestry: Vec<gtk::Widget> =
            std::iter::successors(Some(focused.clone()), gtk::prelude::WidgetExt::parent).collect();
        let body: &gtk::Widget = self.body.upcast_ref();
        if !ancestry.iter().any(|widget| widget == body) {
            return None;
        }

        gtk::prelude::RootExt::set_focus(&root, gtk::Widget::NONE);

        // The focus may have been on the row or on the value inside it; either way, the
        // row is what can be found again afterwards.
        let held = self.body_widgets.borrow();
        let widgets = held.as_ref()?;
        let index = ancestry.iter().find_map(|widget| {
            widget
                .downcast_ref::<adw::ActionRow>()
                .and_then(|row| widgets.rows.iter().position(|held| held == row))
        })?;

        widgets.shape.identities().get(index).map(|row| FocusMark {
            row: row.clone(),
            value: !focused.is::<adw::ActionRow>(),
        })
    }

    /// Put the focus back in the row it was in, now that row has been built again.
    ///
    /// A row the page no longer has leaves the focus where `take_focus` left it, which is
    /// nowhere. That is the point: there is no row of the reader's to put it on, and any
    /// other is one the scroller would go chasing.
    fn restore_focus(&self, mark: Option<&FocusMark>) {
        let Some(mark) = mark else {
            return;
        };
        let held = self.body_widgets.borrow();
        let Some(widgets) = held.as_ref() else {
            return;
        };

        let Some(row) = widgets
            .shape
            .identities()
            .iter()
            .position(|identity| *identity == mark.row)
            .and_then(|index| widgets.rows.get(index))
        else {
            return;
        };

        if mark.value
            && let Some(label) = value_label(row)
        {
            label.grab_focus();
            return;
        }
        row.grab_focus();
    }

    /// Redraw the row ticks. They describe what the window has checked, which the rows
    /// themselves know nothing about.
    pub fn refresh_checks(&self) {
        if let Some(section) = self.section.borrow().as_ref() {
            section.table.redraw();
        }
    }

    /// The table for this page, brought up to date.
    ///
    /// Kept from one redraw to the next unless the page wants a different shape of
    /// table, in which case there is nothing to keep and it is built again.
    fn table_section(
        &self,
        table: &lave_core::model::table::Table,
        detail: &DetailPage,
        state: &TableState,
        handlers: &Handlers,
    ) -> gtk::Widget {
        let section = match self.section.take() {
            Some(section) if section.leading == detail.table_first && section.table.fits(table) => {
                section
            }
            _ => TableSection::new(table, detail, state, handlers),
        };

        section
            .table
            .update(table, &state.sort, Some(handlers.table.clone()));
        let (root, header) = (section.root.clone(), section.header.clone());

        // Put back before the strip is filled: doing that hands the window its select-all
        // and its cog, and the window answers by asking this same table to redraw.
        self.section.replace(Some(section));
        fill_header(&header, detail, handlers);

        root.upcast()
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

    // Brought up to date rather than rebuilt, and so still holding whatever the reader
    // had scrolled to, clicked or checked.
    let table = detail
        .table
        .as_ref()
        .map(|table| surface.table_section(table, detail, state, handlers));

    let leading = detail.table_first && table.is_some();

    // The upper half holds the table and nothing else, so emptying it destroys nothing
    // the reader could be holding on to.
    clear_except(lead, table.as_ref().filter(|_| leading));
    lead.set_visible(leading);

    if let Some(table) = &table
        && leading
        && table.parent().is_none()
    {
        lead.append(table);
    }

    // The lower half is the same widgets from one refresh to the next unless the page
    // has actually changed shape — a refresh moves the values, and only the values are
    // written back. Rebuilding it would take with it whatever the reader had clicked,
    // and a scroller follows its focus.
    let shape = BodyShape::of(detail, leading);
    let settled = surface.body_holds(&shape)
        && table
            .as_ref()
            .is_none_or(|table| leading || table.parent().is_some());

    if settled {
        surface.write_values(detail);
        return;
    }
    // Before the widgets holding the reader's place are thrown away.
    //
    // The focus comes off the widgets that are going, and goes back on afterwards: left
    // to GTK, it lands on a row at the top and takes the page with it.
    let mark = surface.take_focus();

    surface.body_scroll.hold();
    clear_except(body, table.as_ref().filter(|_| !leading));

    // First line of an object's page: what may be done to it, without going back to the
    // table it was reached from.
    if detail.shows_action_bar() {
        body.append(&clamped(&action_bar(&detail.actions, &handlers.act)));
    }

    let mut rows = Vec::new();
    if !detail.groups.is_empty() {
        let (columns, built) = group_columns(&detail.groups, &handlers.navigate);
        body.append(&clamped(&columns));
        rows = built;
    }

    if let Some(table) = &table
        && !leading
    {
        // Already in the body, and kept there: moved into place rather than re-added,
        // since taking it out and putting it back is the teardown this avoids.
        let last = body.last_child();
        if table.parent().is_none() {
            body.append(table);
        } else if last.as_ref() != Some(table) {
            body.reorder_child_after(table, last.as_ref());
        }
    }

    let mut raw_label = None;
    if let Some(raw) = &detail.raw {
        let (group, label) = raw_group(raw);
        body.append(&clamped(&group));
        raw_label = Some(label);
    }

    surface.body_built(BodyWidgets {
        shape,
        rows,
        raw: raw_label,
    });

    surface.restore_focus(mark.as_ref());
}

/// The selectable value inside a row, which is the widget a click on the text lands in.
fn value_label(row: &adw::ActionRow) -> Option<gtk::Label> {
    let mut stack = vec![row.clone().upcast::<gtk::Widget>()];

    while let Some(node) = stack.pop() {
        if let Some(label) = node.downcast_ref::<gtk::Label>()
            && label.is_selectable()
        {
            return Some(label.clone());
        }
        let mut child = node.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            stack.push(widget);
        }
    }

    None
}

/// Empty a box, except for one child that is kept — and kept parented, so it is not
/// unrealized and does not lose what the widgets below it are holding.
fn clear_except(container: &gtk::Box, keep: Option<&gtk::Widget>) {
    let mut child = container.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if Some(&widget) != keep {
            container.remove(&widget);
        }
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

/// The strip above a table and the table itself, kept between redraws.
struct TableSection {
    /// What goes in the pane.
    root: gtk::Box,
    /// Refilled on each redraw: what the strip holds describes the page, and it holds
    /// nothing the reader can lose their place in.
    header: gtk::Box,
    table: TableView,
    /// Built to lead, which is arranged differently from a table below the groups.
    leading: bool,
}

impl TableSection {
    fn new(
        table: &lave_core::model::table::Table,
        detail: &DetailPage,
        state: &TableState,
        handlers: &Handlers,
    ) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        let header = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        root.append(&header);

        let view = TableView::new(table, &state.sort, &state.widths, handlers.table.clone());

        let frame = gtk::Frame::new(None);
        frame.add_css_class("view");

        if detail.table_first {
            // The paned above decides the height; anything beyond it scrolls, which is
            // what dragging the divider reveals.
            let scroller = gtk::ScrolledWindow::builder()
                .child(view.widget())
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Automatic)
                .build();
            frame.set_child(Some(&scroller));
            frame.set_vexpand(true);
        } else {
            frame.set_child(Some(view.widget()));
        }

        root.append(&frame);

        Self {
            root,
            header,
            table: view,
            leading: detail.table_first,
        }
    }
}

/// The strip above a table: select-all and bulk actions on the left, the filter on the
/// right.
fn fill_header(header: &gtk::Box, detail: &DetailPage, handlers: &Handlers) {
    table_view::clear(header);

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

    if let Some(summary) = &detail.table_summary {
        let label = gtk::Label::builder()
            .label(summary)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .margin_start(6)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        label.add_css_class("dim-label");
        header.append(&label);
    }

    if let Some(filter) = &detail.table_filter {
        let toggle = filter_toggle(filter, &handlers.set_filter);
        toggle.set_hexpand(true);
        header.append(&toggle);
    }
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
fn group_columns(
    groups: &[DetailGroup],
    on_navigate: &Rc<dyn Fn(NodeId)>,
) -> (GroupColumns, Vec<adw::ActionRow>) {
    let columns = GroupColumns::new();
    let mut built = Vec::new();

    for group in groups {
        let widget = adw::PreferencesGroup::builder().title(&group.title).build();

        for row in &group.rows {
            let action = action_row(row, on_navigate);
            widget.add(&action);
            built.push(action);
        }

        columns.append(&widget);
    }

    (columns, built)
}

fn action_row(
    row: &lave_core::model::detail::DetailRow,
    on_navigate: &Rc<dyn Fn(NodeId)>,
) -> adw::ActionRow {
    // Titles and subtitles are Pango markup by default, and these are plain text: a value
    // like "<0.1%", or an image name with an "&" in it, is not markup.
    //
    // Markup is turned off before the text arrives rather than alongside it: a row parses
    // each label as it is set, so a property list that carries the value as well is too
    // late — the parse has already failed and been complained about by the time the
    // property lands. It lands, so the text is right; GTK warns on every render anyway.
    let action = adw::ActionRow::builder().use_markup(false).build();
    action.set_title(&row.label);
    action.set_subtitle(&row.value);
    action.set_subtitle_selectable(row.link.is_none());
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

fn raw_group(raw: &str) -> (adw::PreferencesGroup, gtk::Label) {
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
    (group, label)
}
