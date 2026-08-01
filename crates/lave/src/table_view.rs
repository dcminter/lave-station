//! Renders a [`Table`] as a sortable `GtkColumnView`.
//!
//! No decisions here — the columns, their order and their sort keys all come from
//! `lave_core::model::table`.

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

/// How a table is sorted, in terms the settings file can hold.
#[derive(Debug, Clone, Default)]
pub struct SortOrder {
    /// Column title. A title no longer present is ignored, leaving the table unsorted.
    pub column: String,
    pub descending: bool,
}

/// Build the table.
///
/// `on_activate` fires when a row is chosen, which is how the tables navigate to an
/// object. `on_sort_changed` reports the user re-sorting, so the choice can be stored;
/// it does not fire for the initial sort applied here.
#[must_use]
pub fn build(
    table: &Table,
    sort: &SortOrder,
    on_activate: Rc<dyn Fn(NodeId)>,
    on_sort_changed: Rc<dyn Fn(SortOrder)>,
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

    for (index, column) in table.columns.iter().enumerate() {
        view.append_column(&build_column(index, column));
    }

    apply_sort(&view, sort);
    watch_sort(&view, on_sort_changed);

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

fn build_column(index: usize, column: &lave_core::model::table::Column) -> gtk::ColumnViewColumn {
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
