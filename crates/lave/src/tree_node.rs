//! A `GObject` wrapper so tree nodes can live in a `GListModel`.

use gtk::glib;
use lave_core::model::tree::TreeNode;

mod imp {
    use std::cell::RefCell;

    use gtk::glib;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::TreeNodeObject)]
    pub struct TreeNodeObject {
        #[property(get, set)]
        pub key: RefCell<String>,
        #[property(get, set)]
        pub label: RefCell<String>,
        /// Not shown: it is the row's accessible description and nothing else.
        #[property(get, set)]
        pub description: RefCell<String>,
        #[property(get, set)]
        pub icon: RefCell<String>,
        /// CSS class carrying the icon's colour.
        #[property(get, set)]
        pub tone: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TreeNodeObject {
        const NAME: &'static str = "LaveTreeNode";
        type Type = super::TreeNodeObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for TreeNodeObject {}
}

glib::wrapper! {
    pub struct TreeNodeObject(ObjectSubclass<imp::TreeNodeObject>);
}

impl TreeNodeObject {
    #[must_use]
    pub fn from_node(node: &TreeNode) -> Self {
        let object: Self = glib::Object::new();
        object.apply(node);
        object
    }

    /// Update in place, so the scroll position, the expansion, the selection and the
    /// focus all survive a refresh.
    ///
    /// Only what has actually moved is set: each setter notifies, and a row redraws
    /// itself on being notified, so setting a value back to itself is work for nothing
    /// several times a minute per visible row.
    pub fn apply(&self, node: &TreeNode) {
        let description = node.description.clone().unwrap_or_default();

        if self.key() != node.id.key() {
            self.set_key(node.id.key());
        }
        if self.label() != node.label {
            self.set_label(node.label.clone());
        }
        if self.description() != description {
            self.set_description(description);
        }
        if self.icon() != node.icon {
            self.set_icon(node.icon);
        }
        if self.tone() != node.tone.css_class() {
            self.set_tone(node.tone.css_class());
        }
    }
}
