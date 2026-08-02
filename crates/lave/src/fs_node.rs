//! A `GObject` wrapper so filesystem entries can live in a `GListModel`.

use gtk::glib;
use lave_core::model::format;
use lave_core::model::fs_tree::{EntryKind, Node};

mod imp {
    use std::cell::{Cell, RefCell};

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::FsNodeObject)]
    pub struct FsNodeObject {
        /// Absolute path within the container, which is also its identity.
        #[property(get, set)]
        pub path: RefCell<String>,
        #[property(get, set)]
        pub name: RefCell<String>,
        /// Size and mode, or a symlink's target.
        #[property(get, set)]
        pub detail: RefCell<String>,
        #[property(get, set)]
        pub icon: RefCell<String>,
        /// Whether the tree may offer an expander for it.
        #[property(get, set)]
        pub expandable: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FsNodeObject {
        const NAME: &'static str = "LaveFsNode";
        type Type = super::FsNodeObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for FsNodeObject {}
}

glib::wrapper! {
    pub struct FsNodeObject(ObjectSubclass<imp::FsNodeObject>);
}

impl FsNodeObject {
    #[must_use]
    pub fn from_node(node: &Node) -> Self {
        let object: Self = glib::Object::new();
        object.set_path(node.path.clone());
        object.set_name(node.name.clone());
        object.set_detail(describe(node));
        object.set_icon(icon_for(node.kind));
        // Only directories have anything beneath them. A symlink to a directory would
        // too, but following it needs the target resolved, which the index does not do.
        object.set_expandable(node.kind.is_directory());
        object
    }
}

fn icon_for(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Directory => "folder-symbolic",
        EntryKind::Symlink | EntryKind::HardLink => "emblem-symbolic-link",
        EntryKind::File => "text-x-generic-symbolic",
        // Devices, fifos and sockets: listed for completeness, but not openable.
        EntryKind::Other => "application-x-executable-symbolic",
    }
}

/// The secondary text for one entry: what it is, and how big.
fn describe(node: &Node) -> String {
    let mode = format!("{:o}", node.mode & 0o7777);

    match node.kind {
        EntryKind::Directory => mode,
        EntryKind::Symlink | EntryKind::HardLink => format!("\u{2192} {}", node.link_target),
        _ => format!(
            "{} \u{00b7} {}",
            format::bytes(i64::try_from(node.size).unwrap_or(i64::MAX)),
            mode
        ),
    }
}
