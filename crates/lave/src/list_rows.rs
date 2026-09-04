//! Keeping a bound list cell in step with the object behind it.
//!
//! The list views here are model-driven: a refresh replaces what an object holds rather
//! than replacing the object in the model. That is what lets the widgets survive a
//! refresh — and with them the place the reader had scrolled to, the row they had
//! clicked, and the focus that went with it. What the widgets no longer get for free is
//! the redraw a rebuild used to give them, which is what this arranges.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

/// Draw a cell when it is bound, and again whenever its object says it has changed.
///
/// `signal` is what the object emits when its contents are replaced: `notify` for an
/// object with properties, a signal of its own for one without. `object` says how to
/// reach it from the list item, since a tree hands over a row rather than the item
/// itself.
///
/// The handler is remembered so that unbinding can take it off again: list cells are
/// recycled, and one still listening to the row it used to hold would redraw itself with
/// another row's contents.
pub fn follow<T>(
    factory: &gtk::SignalListItemFactory,
    signal: &'static str,
    object: impl Fn(&gtk::ListItem) -> Option<T> + 'static,
    draw: impl Fn(&gtk::ListItem) + 'static,
) where
    T: IsA<glib::Object> + Clone + 'static,
{
    let object = Rc::new(object);
    let draw = Rc::new(draw);
    let followed: Rc<RefCell<HashMap<gtk::ListItem, (T, glib::SignalHandlerId)>>> = Rc::default();

    let bound = Rc::clone(&followed);
    let on_bind = Rc::clone(&draw);
    let source = Rc::clone(&object);
    factory.connect_bind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(object) = source(item) else {
            return;
        };

        on_bind(item);

        let handler = object.connect_local(signal, false, {
            let item = item.clone();
            let draw = Rc::clone(&on_bind);
            move |_| {
                draw(&item);
                None
            }
        });

        // A cell bound twice without an unbind between would otherwise be left listening
        // to both rows.
        if let Some((previous, handler)) =
            bound.borrow_mut().insert(item.clone(), (object, handler))
        {
            previous.disconnect(handler);
        }
    });

    let unbound = Rc::clone(&followed);
    factory.connect_unbind(move |_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        if let Some((object, handler)) = unbound.borrow_mut().remove(item) {
            object.disconnect(handler);
        }
    });
}
