//! A container that packs its children into columns, each child only as tall as itself.
//!
//! No decisions here — how many columns fit and which one each child goes in both come
//! from `lave_core::model::layout`.

use gtk::glib;
use gtk::prelude::*;
use lave_core::model::layout;

mod imp {
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{Orientation, SizeRequestMode, glib};
    use lave_core::model::layout;

    #[derive(Default)]
    pub struct ColumnsLayout;

    #[glib::object_subclass]
    impl ObjectSubclass for ColumnsLayout {
        const NAME: &'static str = "LaveColumnsLayout";
        type Type = super::ColumnsLayout;
        type ParentType = gtk::LayoutManager;
    }

    impl ObjectImpl for ColumnsLayout {}

    impl LayoutManagerImpl for ColumnsLayout {
        /// The height depends on the width, since the width decides the column count.
        fn request_mode(&self, _widget: &gtk::Widget) -> SizeRequestMode {
            SizeRequestMode::HeightForWidth
        }

        fn measure(
            &self,
            widget: &gtk::Widget,
            orientation: Orientation,
            for_size: i32,
        ) -> (i32, i32, i32, i32) {
            if orientation == Orientation::Horizontal {
                let (minimum, natural) = super::widest(widget);
                // One column at its narrowest; at its natural width, as many columns as
                // are allowed — but never more than there are children, so a page of one
                // group does not ask for room for a second.
                let wanted = super::children(widget).len().clamp(1, layout::MAX_COLUMNS);
                let columns = i32::try_from(wanted).unwrap_or(1);
                let gutters = (columns - 1) * layout::GUTTER;
                return (minimum, natural * columns + gutters, -1, -1);
            }

            let (_, height) = super::plan(widget, for_size);
            (height, height, -1, -1)
        }

        fn allocate(&self, widget: &gtk::Widget, width: i32, _height: i32, _baseline: i32) {
            let (placements, _) = super::plan(widget, width);
            let children = super::children(widget);
            let columns = layout::columns_for(width, children.len());
            let column_width = layout::column_width(width, columns);

            for (child, placement) in children.into_iter().zip(placements) {
                let column = i32::try_from(placement.column).unwrap_or(0);
                let height = child.measure(Orientation::Vertical, column_width).1;
                child.size_allocate(
                    &gtk::Allocation::new(
                        column * (column_width + layout::GUTTER),
                        placement.top,
                        column_width,
                        height,
                    ),
                    -1,
                );
            }
        }
    }

    /// The widget itself does nothing but hold children; the layout above places them.
    #[derive(Default)]
    pub struct GroupColumns;

    #[glib::object_subclass]
    impl ObjectSubclass for GroupColumns {
        const NAME: &'static str = "LaveGroupColumns";
        type Type = super::GroupColumns;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<super::ColumnsLayout>();
        }
    }

    impl ObjectImpl for GroupColumns {
        /// A GTK widget must unparent its children itself, or they leak.
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for GroupColumns {}
}

glib::wrapper! {
    pub struct ColumnsLayout(ObjectSubclass<imp::ColumnsLayout>) @extends gtk::LayoutManager;
}

glib::wrapper! {
    pub struct GroupColumns(ObjectSubclass<imp::GroupColumns>)
        @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for GroupColumns {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl GroupColumns {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&self, child: &impl IsA<gtk::Widget>) {
        child.as_ref().set_parent(self);
    }
}

/// The children that take part in the layout. A hidden child is skipped rather than
/// given an empty slot of its own.
fn children(widget: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut found = Vec::new();
    let mut child = widget.first_child();
    while let Some(node) = child {
        if node.should_layout() {
            found.push(node.clone());
        }
        child = node.next_sibling();
    }
    found
}

/// The widest any one child wants to be, which sets the container's own width request.
fn widest(widget: &gtk::Widget) -> (i32, i32) {
    let mut minimum = layout::MIN_COLUMN_WIDTH;
    let mut natural = layout::MIN_COLUMN_WIDTH;

    for child in children(widget) {
        let (child_minimum, child_natural, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
        minimum = minimum.max(child_minimum);
        natural = natural.max(child_natural);
    }

    (minimum, natural)
}

/// Measure every child at the column width `width` implies, and place them.
fn plan(widget: &gtk::Widget, width: i32) -> (Vec<layout::Placement>, i32) {
    // GTK measures with -1 when it wants the natural size before a width is settled.
    let width = if width < 0 {
        layout::MIN_COLUMN_WIDTH
    } else {
        width
    };
    let children = children(widget);
    let columns = layout::columns_for(width, children.len());
    let column_width = layout::column_width(width, columns);

    let heights: Vec<i32> = children
        .into_iter()
        .map(|child| child.measure(gtk::Orientation::Vertical, column_width).1)
        .collect();

    layout::place(&heights, columns)
}
