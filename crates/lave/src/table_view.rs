//! Renders a [`Table`] as a sortable `GtkColumnView`.
//!
//! No decisions here — the columns, their order and their sort keys all come from
//! `lave_core::model::table`.

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

    use gtk::glib;
    use gtk::subclass::prelude::*;
    use lave_core::model::table::Row;

    /// Holds a whole row: the cells are positional, which `glib::Properties` cannot
    /// express, and the factories are written in Rust so properties buy nothing here.
    #[derive(Default)]
    pub struct TableRowObject {
        pub row: RefCell<Option<Row>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TableRowObject {
        const NAME: &'static str = "LaveTableRow";
        type Type = super::TableRowObject;
    }

    impl ObjectImpl for TableRowObject {}
}

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

    fn key(&self) -> Option<NodeId> {
        self.imp().row.borrow().as_ref().map(|row| row.key.clone())
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

/// Build the table.
///
/// `sort` is already resolved by the caller — the session's order if the user has set
/// one, the table's own default otherwise. `widths` are the widths the user has dragged
/// columns to, by title; a column with none sizes itself.
#[must_use]
pub fn build(
    table: &Table,
    sort: &SortOrder,
    widths: &BTreeMap<String, i32>,
    handlers: &TableHandlers,
) -> gtk::ColumnView {
    let store = gtk::gio::ListStore::new::<TableRowObject>();
    let objects: Vec<TableRowObject> = table
        .rows
        .iter()
        .cloned()
        .map(TableRowObject::new)
        .collect();
    store.splice(0, 0, &objects);

    let view = gtk::ColumnView::builder()
        .show_row_separators(true)
        .single_click_activate(true)
        .build();
    view.add_css_class("data-table");

    // The view's own sorter drives the model, so clicking a heading re-sorts.
    let sorted = gtk::SortListModel::new(Some(store), view.sorter());
    let selection = gtk::SingleSelection::new(Some(sorted));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    view.set_model(Some(&selection));

    // Leading, so the checkboxes line up down the left edge whatever the table.
    view.append_column(&build_check_column(handlers));

    for (index, column) in table.columns.iter().enumerate() {
        let built = build_column(index, column, &handlers.context);

        // Restoring a width makes GTK notify, which reports the width we just set; the
        // settings model treats storing an unchanged width as no change, so the store is
        // not rewritten on every render.
        if let Some(width) = widths.get(&column.title) {
            built.set_fixed_width(*width);
        }

        let resized = Rc::clone(&handlers.resized);
        let title = column.title.clone();
        built.connect_fixed_width_notify(move |column| {
            resized(title.clone(), column.fixed_width());
        });

        view.append_column(&built);
    }

    apply_sort(&view, sort);
    watch_sort(&view, Rc::clone(&handlers.sort_changed));

    let on_activate = Rc::clone(&handlers.activate);
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
        on_activate(node);
    });

    view
}

/// The leading column of checkboxes, which is what makes a bulk action possible.
///
/// The checked set lives in the window rather than in the widgets: the pane is rebuilt
/// whenever a refresh arrives, and ticks that vanished on every daemon event would be
/// unusable.
fn build_check_column(handlers: &TableHandlers) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();

    let toggle = Rc::clone(&handlers.toggle);
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
                    toggle(node, check.is_active());
                }
            }
        ));

        item.set_child(Some(&check));
    });

    let checked = Rc::clone(&handlers.checked);
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
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
        check.set_active(checked(&node));
    });

    gtk::ColumnViewColumn::builder()
        .title("")
        .factory(&factory)
        .resizable(false)
        .expand(false)
        .build()
}

/// Drop the gestures `bind` attached, so a recycled cell starts clean.
fn clear_gestures(child: &gtk::Widget) {
    let controllers = child.observe_controllers();
    // Collected first: removing while iterating the live model would skip entries.
    let gestures: Vec<gtk::GestureClick> = (0..controllers.n_items())
        .filter_map(|index| controllers.item(index).and_downcast::<gtk::GestureClick>())
        .collect();

    for gesture in gestures {
        child.remove_controller(&gesture);
    }
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
fn watch_sort(view: &gtk::ColumnView, on_sort_changed: Rc<dyn Fn(SortOrder)>) {
    let Some(sorter) = view.sorter().and_downcast::<gtk::ColumnViewSorter>() else {
        return;
    };

    sorter.connect_changed(move |sorter, _| {
        let column = sorter
            .primary_sort_column()
            .and_then(|column| column.title())
            .map(|title| title.to_string())
            .unwrap_or_default();

        on_sort_changed(SortOrder {
            column,
            descending: sorter.primary_sort_order() == gtk::SortType::Descending,
        });
    });
}

fn build_column(
    index: usize,
    column: &lave_core::model::table::Column,
    on_context: &Rc<dyn Fn(NodeId, gtk::Widget, f64, f64)>,
) -> gtk::ColumnViewColumn {
    let factory = gtk::SignalListItemFactory::new();
    let numeric = column.numeric;
    // Only the first column names the object, so only it carries the icon.
    let with_icon = index == 0;

    factory.connect_setup(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let label = gtk::Label::builder()
            .xalign(if numeric { 1.0 } else { 0.0 })
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();

        if with_icon {
            let content = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .build();
            content.append(&gtk::Image::new());
            content.append(&label);
            content.add_css_class("table-cell");
            item.set_child(Some(&content));
        } else {
            label.add_css_class("table-cell");
            item.set_child(Some(&label));
        }
    });

    let on_context = Rc::clone(on_context);
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = item.item().and_downcast::<TableRowObject>() else {
            return;
        };
        let Some(child) = item.child() else {
            return;
        };

        // Attached here rather than in setup because only now is the row known, and
        // removed again in unbind so a recycled cell does not accumulate gestures.
        // `ColumnViewRow` would be the natural home but is not a widget, so cannot
        // carry a controller.
        if let Some(node) = row.key() {
            let secondary = gtk::GestureClick::new();
            secondary.set_button(gtk::gdk::BUTTON_SECONDARY);
            let on_context = Rc::clone(&on_context);
            secondary.connect_pressed(move |gesture, _, x, y| {
                if let Some(widget) = gesture.widget() {
                    on_context(node.clone(), widget, x, y);
                }
            });
            child.add_controller(secondary);
        }

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
    });

    factory.connect_unbind(|_, item| {
        if let Some(item) = item.downcast_ref::<gtk::ListItem>()
            && let Some(child) = item.child()
        {
            clear_gestures(&child);
        }
    });

    gtk::ColumnViewColumn::builder()
        .title(&column.title)
        .factory(&factory)
        .resizable(true)
        .expand(column.expand)
        .sorter(&column_sorter(index))
        .build()
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
