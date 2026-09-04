//! Renders a [`Table`] as a sortable `GtkColumnView`.
//!
//! No decisions here — the columns, their order and their sort keys all come from
//! `lave_core::model::table`.

use std::cell::RefCell;
use std::collections::BTreeMap;

use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use lave_core::model::table::{Row, Sort, Table};
use lave_core::model::tree::{NodeId, Tone};

/// Strip every tone from a recycled icon, then apply the one this row calls for.
/// Driven from `Tone::ALL` so the list cannot drift from the enum.
pub fn apply_tone(icon: &gtk::Image, tone: Tone) {
    apply_tone_class(icon, tone.css_class());
}

/// As [`apply_tone`], for callers holding the class the core already chose.
pub fn apply_tone_class(icon: &gtk::Image, class: &str) {
    for candidate in Tone::ALL {
        icon.remove_css_class(candidate.css_class());
    }
    icon.add_css_class(class);
}

mod imp {
    use std::cell::RefCell;
    use std::sync::OnceLock;

    use gtk::glib;
    use gtk::glib::subclass::Signal;
    use gtk::subclass::prelude::*;
    use lave_core::model::table::Row;

    /// Holds a whole row: the cells are positional, which `glib::Properties` cannot
    /// express, and the factories are written in Rust so properties buy nothing here.
    ///
    /// The contents are replaced in place on a refresh rather than the object being
    /// replaced in the model, so `updated` stands in for the notification a property
    /// would have given: the cells bound to this row re-read themselves when it fires.
    #[derive(Default)]
    pub struct TableRowObject {
        pub row: RefCell<Option<Row>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TableRowObject {
        const NAME: &'static str = "LaveTableRow";
        type Type = super::TableRowObject;
    }

    impl ObjectImpl for TableRowObject {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder(super::UPDATED).build()])
        }
    }
}

/// Emitted when a row's contents are replaced in place.
const UPDATED: &str = "updated";

glib::wrapper! {
    pub struct TableRowObject(ObjectSubclass<imp::TableRowObject>);
}

impl TableRowObject {
    fn new(row: Row) -> Self {
        let object: Self = glib::Object::new();
        object.imp().row.replace(Some(row));
        object
    }

    fn cell_text(&self, index: usize) -> String {
        self.imp()
            .row
            .borrow()
            .as_ref()
            .and_then(|row| row.cells.get(index))
            .map(|cell| cell.text.clone())
            .unwrap_or_default()
    }

    fn sort_key(&self, index: usize) -> Option<Sort> {
        self.imp()
            .row
            .borrow()
            .as_ref()
            .and_then(|row| row.cells.get(index))
            .map(|cell| cell.sort.clone())
    }

    pub(crate) fn key(&self) -> Option<NodeId> {
        self.imp().row.borrow().as_ref().map(|row| row.key.clone())
    }

    /// Whether this object already holds that row, and so needs no replacing.
    fn holds(&self, row: &Row) -> bool {
        self.imp().row.borrow().as_ref() == Some(row)
    }

    /// Take fresh contents without leaving the model.
    ///
    /// This is the whole point of the object: a row that is replaced in the model takes
    /// its widget with it, and the reader loses the row they had clicked, the focus that
    /// went with it and the place they had scrolled to. A row updated in place keeps all
    /// three, and the cells bound to it re-read themselves.
    fn set_row(&self, row: Row) {
        self.imp().row.replace(Some(row));
        self.redraw();
    }

    /// Tell whatever is bound to this row to read it again.
    fn redraw(&self) {
        self.emit_by_name::<()>(UPDATED, &[]);
    }

    fn icon(&self) -> Option<&'static str> {
        self.imp().row.borrow().as_ref().map(|row| row.icon)
    }

    fn tone(&self) -> Tone {
        self.imp()
            .row
            .borrow()
            .as_ref()
            .map_or(Tone::Neutral, |row| row.tone)
    }
}

/// How a table is sorted. Held for the session only: the user's own order is deliberately
/// not written to the settings store, so every launch opens on the table's own default.
#[derive(Debug, Clone, Default)]
pub struct SortOrder {
    /// Column title. A title no longer present is ignored, leaving the table unsorted.
    pub column: String,
    pub descending: bool,
}

impl SortOrder {
    /// The order a table opens in, before the user has said otherwise.
    #[must_use]
    pub fn from_default(table: &Table) -> Self {
        table.default_sort.map_or_else(Self::default, |sort| Self {
            column: sort.column.to_owned(),
            descending: sort.descending,
        })
    }
}

/// What the window has to tell a table, beyond the rows themselves.
///
/// Cloned out of the cell the widgets hold before each call: a handler may redraw the
/// pane, which swaps the cell's contents, and it must not do that through a live borrow.
#[derive(Clone)]
pub struct TableHandlers {
    /// A row was chosen, which is how the tables navigate to an object.
    pub activate: Rc<dyn Fn(NodeId)>,
    /// The user re-sorted. Does not fire for the initial sort applied here.
    pub sort_changed: Rc<dyn Fn(SortOrder)>,
    /// A secondary click, carrying the cell that was hit so a menu can be anchored.
    pub context: Rc<dyn Fn(NodeId, gtk::Widget, f64, f64)>,
    /// Whether a row is checked for the next bulk action.
    pub checked: Rc<dyn Fn(&NodeId) -> bool>,
    /// A row's checkbox was operated.
    pub toggle: Rc<dyn Fn(NodeId, bool)>,
    /// A column was dragged to a new width, by title.
    pub resized: Rc<dyn Fn(String, i32)>,
}

/// A built table: the view, and the model that drives it.
///
/// Kept across redraws rather than rebuilt. A `GtkColumnView` is driven by its model, so
/// a refresh replaces the rows in the store and leaves the widgets alone — which is what
/// keeps the place the reader had scrolled to, the row they had clicked, the focus that
/// went with it, and the widths they had dragged. Rebuilding the view loses all of it.
pub struct TableView {
    view: gtk::ColumnView,
    store: gtk::gio::ListStore,
    /// Swapped on every redraw: the handlers close over the page being shown, and these
    /// widgets outlive it.
    handlers: Rc<RefCell<TableHandlers>>,
    /// The columns it was built for, by title. A table with different ones needs a new
    /// view; a table with the same ones needs only new rows.
    columns: Vec<String>,
}

impl TableView {
    /// Build the table.
    ///
    /// `sort` is already resolved by the caller — the session's order if the user has set
    /// one, the table's own default otherwise. `widths` are the widths the user has
    /// dragged columns to, by title; a column with none sizes itself.
    #[must_use]
    pub fn new(
        table: &Table,
        sort: &SortOrder,
        widths: &BTreeMap<String, i32>,
        handlers: TableHandlers,
    ) -> Self {
        let store = gtk::gio::ListStore::new::<TableRowObject>();
        let handlers = Rc::new(RefCell::new(handlers));

        let view = gtk::ColumnView::builder()
            .show_row_separators(true)
            .single_click_activate(true)
            .build();
        view.add_css_class("data-table");

        // The view's own sorter drives the model, so clicking a heading re-sorts.
        let sorted = gtk::SortListModel::new(Some(store.clone()), view.sorter());
        let selection = gtk::SingleSelection::new(Some(sorted));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        view.set_model(Some(&selection));

        // Leading, so the checkboxes line up down the left edge whatever the table.
        view.append_column(&build_check_column(&handlers));

        for (index, column) in table.columns.iter().enumerate() {
            let built = build_column(index, column, &handlers);

            // Restoring a width makes GTK notify, which reports the width we just set;
            // the settings model treats storing an unchanged width as no change, so the
            // store is not rewritten on every render.
            if let Some(width) = widths.get(&column.title) {
                built.set_fixed_width(*width);
            }

            let cell = Rc::clone(&handlers);
            let title = column.title.clone();
            built.connect_fixed_width_notify(move |column| {
                let resized = Rc::clone(&cell.borrow().resized);
                resized(title.clone(), column.fixed_width());
            });

            view.append_column(&built);
        }

        apply_sort(&view, sort);
        watch_sort(&view, &handlers);

        let cell = Rc::clone(&handlers);
        view.connect_activate(move |view, position| {
            let Some(model) = view.model() else {
                return;
            };
            let Some(node) = model
                .item(position)
                .and_downcast::<TableRowObject>()
                .and_then(|row| row.key())
            else {
                return;
            };
            let activate = Rc::clone(&cell.borrow().activate);
            activate(node);
        });

        let built = Self {
            view,
            store,
            handlers,
            columns: table
                .columns
                .iter()
                .map(|column| column.title.clone())
                .collect(),
        };
        built.update(table, sort, None);
        built
    }

    #[must_use]
    pub fn widget(&self) -> &gtk::ColumnView {
        &self.view
    }

    /// Whether this view can show that table, or must be built again for it.
    #[must_use]
    pub fn fits(&self, table: &Table) -> bool {
        self.columns.len() == table.columns.len()
            && self
                .columns
                .iter()
                .zip(&table.columns)
                .all(|(title, column)| *title == column.title)
    }

    /// Sort as asked, unless that is already how the view is sorted.
    ///
    /// The view is where the user's own sorting lives — clicking a heading sorts it and
    /// tells the window — so it is left alone unless it disagrees with what it is given.
    fn sync_sort(&self, sort: &SortOrder) {
        let sorted_by = self
            .view
            .sorter()
            .and_downcast::<gtk::ColumnViewSorter>()
            .map(|sorter| SortOrder {
                column: sorter
                    .primary_sort_column()
                    .and_then(|column| column.title())
                    .map(|title| title.to_string())
                    .unwrap_or_default(),
                descending: sorter.primary_sort_order() == gtk::SortType::Descending,
            });

        let agrees = sorted_by.is_some_and(|current| {
            current.column == sort.column && current.descending == sort.descending
        });
        if !agrees {
            apply_sort(&self.view, sort);
        }
    }

    /// Redraw what the rows are showing, without changing what they hold.
    ///
    /// The ticks live in the window rather than in the rows, so a bulk selection changes
    /// nothing here that the rows could notice by themselves.
    pub fn redraw(&self) {
        for object in self.store.iter::<TableRowObject>().flatten() {
            object.redraw();
        }
    }

    /// Show these rows, and answer to these handlers from now on.
    ///
    /// Row by row: a refresh that moved one figure replaces one row, and every other row
    /// keeps the widget it already had. The sort, the column widths and the scroll
    /// position all live in the view, which is not touched.
    pub fn update(&self, table: &Table, sort: &SortOrder, handlers: Option<TableHandlers>) {
        if let Some(handlers) = handlers {
            self.handlers.replace(handlers);
        }

        self.sync_sort(sort);

        let held: Vec<TableRowObject> = self.store.iter::<TableRowObject>().flatten().collect();

        // The same objects, still in the same order, describing the same things? Then
        // this is a refresh of the figures, and every row keeps the widget it has.
        let same_objects = held.len() == table.rows.len()
            && held
                .iter()
                .zip(&table.rows)
                .all(|(object, row)| object.key().as_ref() == Some(&row.key));

        if !same_objects {
            // Rows have come or gone: there is no row-for-row correspondence to keep.
            let objects: Vec<TableRowObject> = table
                .rows
                .iter()
                .cloned()
                .map(TableRowObject::new)
                .collect();
            self.store.splice(0, self.store.n_items(), &objects);
            return;
        }

        let mut moved = false;
        for (object, row) in held.iter().zip(&table.rows) {
            if !object.holds(row) {
                object.set_row(row.clone());
                moved = true;
            }
        }

        // Sorting is by the values that have just changed, so the order has to be asked
        // for again: nothing was added or removed to ask on the model's behalf.
        if moved && let Some(sorter) = self.view.sorter() {
            sorter.changed(gtk::SorterChange::Different);
        }
    }
}

/// The leading column of checkboxes, which is what makes a bulk action possible.
///
/// The checked set lives in the window rather than in the widgets: the pane is rebuilt
/// whenever a refresh arrives, and ticks that vanished on every daemon event would be
/// unusable.
fn build_check_column(handlers: &Rc<RefCell<TableHandlers>>) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();

    let toggle = Rc::clone(handlers);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let check = gtk::CheckButton::builder()
            .halign(gtk::Align::Center)
            .tooltip_text("Include this row in the next bulk action")
            .build();

        // Connected once, and reads the row it is bound to at the time it fires: the
        // list item's row is replaced before `bind` runs, so this always addresses what
        // is on screen. Weak, because the list item owns this widget.
        let toggle = Rc::clone(&toggle);
        let owner = item.clone();
        check.connect_toggled(glib::clone!(
            #[weak]
            owner,
            move |check| {
                if let Some(node) = owner
                    .item()
                    .and_downcast::<TableRowObject>()
                    .and_then(|row| row.key())
                {
                    let toggle = Rc::clone(&toggle.borrow().toggle);
                    toggle(node, check.is_active());
                }
            }
        ));

        item.set_child(Some(&check));
    });

    let checked = Rc::clone(handlers);
    crate::list_rows::follow(
        &factory,
        UPDATED,
        |item| item.item().and_downcast::<TableRowObject>(),
        move |item| {
            let Some(check) = item.child().and_downcast::<gtk::CheckButton>() else {
                return;
            };
            let Some(node) = item
                .item()
                .and_downcast::<TableRowObject>()
                .and_then(|row| row.key())
            else {
                return;
            };

            // Setting this fires `toggled`, which writes back the value just read. Toggling
            // is idempotent, so the round trip changes nothing.
            let checked = Rc::clone(&checked.borrow().checked);
            check.set_active(checked(&node));
        },
    );

    gtk::ColumnViewColumn::builder()
        .title("")
        .factory(&factory)
        .resizable(false)
        .expand(false)
        .build()
}

/// Restore a stored sort. A column title that no longer exists is simply not applied.
fn apply_sort(view: &gtk::ColumnView, sort: &SortOrder) {
    if sort.column.is_empty() {
        return;
    }

    let Some(column) = view
        .columns()
        .iter::<glib::Object>()
        .flatten()
        .filter_map(|object| object.downcast::<gtk::ColumnViewColumn>().ok())
        .find(|column| column.title().is_some_and(|title| title == sort.column))
    else {
        tracing::debug!("stored sort column {:?} is not in this table", sort.column);
        return;
    };

    let direction = if sort.descending {
        gtk::SortType::Descending
    } else {
        gtk::SortType::Ascending
    };
    view.sort_by_column(Some(&column), direction);
}

/// Report the user re-sorting. Connected after the initial sort is applied, so
/// restoring a stored order does not immediately write it back.
fn watch_sort(view: &gtk::ColumnView, handlers: &Rc<RefCell<TableHandlers>>) {
    let Some(sorter) = view.sorter().and_downcast::<gtk::ColumnViewSorter>() else {
        return;
    };

    let handlers = Rc::clone(handlers);
    sorter.connect_changed(move |sorter, _| {
        let column = sorter
            .primary_sort_column()
            .and_then(|column| column.title())
            .map(|title| title.to_string())
            .unwrap_or_default();

        let on_sort_changed = Rc::clone(&handlers.borrow().sort_changed);
        on_sort_changed(SortOrder {
            column,
            descending: sorter.primary_sort_order() == gtk::SortType::Descending,
        });
    });
}

fn build_column(
    index: usize,
    column: &lave_core::model::table::Column,
    handlers: &Rc<RefCell<TableHandlers>>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let numeric = column.numeric;
    // Only the first column names the object, so only it carries the icon.
    let with_icon = index == 0;

    let on_context = Rc::clone(handlers);
    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let label = gtk::Label::builder()
            .xalign(if numeric { 1.0 } else { 0.0 })
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();

        let child: gtk::Widget = if with_icon {
            let content = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .build();
            content.append(&gtk::Image::new());
            content.append(&label);
            content.add_css_class("table-cell");
            content.upcast()
        } else {
            label.add_css_class("table-cell");
            label.upcast()
        };

        // Attached once and asking the list item which row it holds when it fires, rather
        // than attached per bind and torn down per unbind: a recycled cell then cannot end
        // up with no gesture, or with two.
        //
        // `ColumnViewRow` would be the natural home but is not a widget, so cannot carry
        // a controller.
        let secondary = gtk::GestureClick::new();
        secondary.set_button(gtk::gdk::BUTTON_SECONDARY);
        let on_context = Rc::clone(&on_context);
        secondary.connect_pressed(glib::clone!(
            #[weak]
            item,
            move |gesture, _, x, y| {
                let Some(node) = item
                    .item()
                    .and_downcast::<TableRowObject>()
                    .and_then(|row| row.key())
                else {
                    return;
                };
                // Claiming stops the row's own click gesture seeing this press. It takes
                // any button, so without this a right-click also selected and — with
                // single-click activation — activated the row, which navigates and rebuilds
                // the pane out from under the menu that was about to open.
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if let Some(widget) = gesture.widget() {
                    let on_context = Rc::clone(&on_context.borrow().context);
                    on_context(node, widget, x, y);
                }
            }
        ));
        child.add_controller(secondary);

        item.set_child(Some(&child));
    });

    crate::list_rows::follow(
        &factory,
        UPDATED,
        |item| item.item().and_downcast::<TableRowObject>(),
        move |item| draw_cell(item, index, with_icon),
    );

    gtk::ColumnViewColumn::builder()
        .title(&column.title)
        .factory(&factory)
        .resizable(true)
        .expand(column.expand)
        .sorter(&column_sorter(index))
        .build()
}

/// Draw one cell from the row its list item currently holds.
fn draw_cell(item: &gtk::ListItem, index: usize, with_icon: bool) {
    let Some(row) = item.item().and_downcast::<TableRowObject>() else {
        return;
    };
    let Some(child) = item.child() else {
        return;
    };

    let label = if with_icon {
        let Some(content) = child.downcast_ref::<gtk::Box>() else {
            return;
        };
        let Some(icon) = content.first_child().and_downcast::<gtk::Image>() else {
            return;
        };
        icon.set_icon_name(row.icon());
        apply_tone(&icon, row.tone());
        icon.next_sibling().and_downcast::<gtk::Label>()
    } else {
        child.downcast::<gtk::Label>().ok()
    };

    if let Some(label) = label {
        let text = row.cell_text(index);
        label.set_tooltip_text(Some(&text));
        label.set_label(&text);
    }
}

/// Sorts by the cell's key rather than by the text it shows, so sizes and ages order
/// by value. Rows missing a key sort last rather than being dropped.
fn column_sorter(index: usize) -> gtk::CustomSorter {
    gtk::CustomSorter::new(move |left, right| {
        let left = left
            .downcast_ref::<TableRowObject>()
            .and_then(|row| row.sort_key(index));
        let right = right
            .downcast_ref::<TableRowObject>()
            .and_then(|row| row.sort_key(index));

        match (left, right) {
            (Some(Sort::Number(left)), Some(Sort::Number(right))) => left.cmp(&right).into(),
            (Some(Sort::Text(left)), Some(Sort::Text(right))) => left.cmp(&right).into(),
            // Mixed kinds cannot happen within a column, but ordering must stay total.
            (Some(_), Some(_)) | (None, None) => gtk::Ordering::Equal,
            (Some(_), None) => gtk::Ordering::Smaller,
            (None, Some(_)) => gtk::Ordering::Larger,
        }
    })
}

/// Remove every child of a box. `GtkBox` has no clear-all.
pub fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}
