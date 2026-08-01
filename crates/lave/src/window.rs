//! The main window: sidebar tree on the left, metadata on the right.

use adw::prelude::*;
use adw::subclass::prelude::*;
use async_channel::Sender;
use gtk::glib;
use lave_core::activity::ActivityState;
use lave_core::model::detail;
use lave_core::model::tree::{self, NodeId, TreeNode};

use crate::detail_pane;
use crate::runtime::{Command, Snapshot, StatusView, now_seconds};
use crate::tree_node::TreeNodeObject;

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};
    use std::collections::HashMap;

    use adw::subclass::prelude::*;
    use async_channel::Sender;
    use gtk::glib::Propagation;
    use gtk::prelude::WidgetExt;
    use gtk::{CompositeTemplate, glib};
    use lave_core::model::tree::NodeId;

    use crate::runtime::{Command, Snapshot};
    use crate::tree_node::TreeNodeObject;

    /// The list stores behind the tree. Held so refreshes update them in place.
    pub struct Stores {
        pub root: gtk::gio::ListStore,
        pub top: gtk::gio::ListStore,
        pub images: gtk::gio::ListStore,
        pub containers: gtk::gio::ListStore,
        pub root_node: TreeNodeObject,
        pub images_node: TreeNodeObject,
        pub containers_node: TreeNodeObject,
    }

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/com/paperstack/LaveStation/ui/window.ui")]
    pub struct LaveWindow {
        #[template_child]
        pub tree_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub content_page: TemplateChild<adw::NavigationPage>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub detail_page: TemplateChild<adw::PreferencesPage>,
        #[template_child]
        pub status_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub retry_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub toasts: TemplateChild<adw::ToastOverlay>,

        pub stores: OnceCell<Stores>,
        pub selection: OnceCell<gtk::SingleSelection>,
        pub commands: OnceCell<Sender<Command>>,
        pub snapshot: RefCell<Option<Snapshot>>,
        pub selected: RefCell<Option<NodeId>>,
        pub groups: RefCell<Vec<adw::PreferencesGroup>>,
        pub raw: RefCell<HashMap<String, serde_json::Value>>,
        pub last_toast: RefCell<String>,
        /// True once an indicator is confirmed present: only then may closing hide.
        pub keep_running: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LaveWindow {
        const NAME: &'static str = "LaveWindow";
        type Type = super::LaveWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LaveWindow {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }
    }

    impl WidgetImpl for LaveWindow {}

    impl WindowImpl for LaveWindow {
        /// With an indicator in the panel, closing hides the window; without one it
        /// would leave the app unreachable, so then it really does close.
        fn close_request(&self) -> Propagation {
            if self.keep_running.get() {
                self.obj().set_visible(false);
                return Propagation::Stop;
            }
            self.parent_close_request()
        }
    }

    impl ApplicationWindowImpl for LaveWindow {}
    impl AdwApplicationWindowImpl for LaveWindow {}
}

glib::wrapper! {
    pub struct LaveWindow(ObjectSubclass<imp::LaveWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gtk::gio::ActionGroup, gtk::gio::ActionMap, gtk::Accessible,
                    gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::Root,
                    gtk::ShortcutManager;
}

impl LaveWindow {
    #[must_use]
    pub fn new(application: &adw::Application, commands: Sender<Command>) -> Self {
        let window: Self = glib::Object::builder()
            .property("application", application)
            .build();
        let _ = window.imp().commands.set(commands);
        window
    }

    fn setup(&self) {
        let stores = Self::build_stores();
        let model = Self::build_tree_model(&stores);
        let _ = self.imp().stores.set(stores);

        let selection = gtk::SingleSelection::new(Some(model));
        selection.set_autoselect(true);
        self.imp().tree_view.set_factory(Some(&build_factory()));
        self.imp().tree_view.set_model(Some(&selection));

        selection.connect_selected_item_notify(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |selection| window.on_selection_changed(selection)
        ));
        let _ = self.imp().selection.set(selection);

        self.imp().retry_button.connect_clicked(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_| window.send(Command::Refresh)
        ));

        // The root node is selected at startup, as the README requires.
        self.expand_root();
        self.render_detail();
    }

    fn build_stores() -> imp::Stores {
        let empty = tree::build(None, &[], &[]);
        let root_node = TreeNodeObject::from_node(&empty);
        let images_node = TreeNodeObject::from_node(&empty.children[0]);
        let containers_node = TreeNodeObject::from_node(&empty.children[1]);

        let root = gtk::gio::ListStore::new::<TreeNodeObject>();
        root.append(&root_node);

        let top = gtk::gio::ListStore::new::<TreeNodeObject>();
        top.append(&images_node);
        top.append(&containers_node);

        imp::Stores {
            root,
            top,
            images: gtk::gio::ListStore::new::<TreeNodeObject>(),
            containers: gtk::gio::ListStore::new::<TreeNodeObject>(),
            root_node,
            images_node,
            containers_node,
        }
    }

    fn build_tree_model(stores: &imp::Stores) -> gtk::TreeListModel {
        let top = stores.top.clone();
        let images = stores.images.clone();
        let containers = stores.containers.clone();

        gtk::TreeListModel::new(stores.root.clone(), false, false, move |item| {
            let node = item.downcast_ref::<TreeNodeObject>()?;
            match NodeId::from_key(&node.key())? {
                NodeId::Root => Some(top.clone().upcast()),
                NodeId::Images => Some(images.clone().upcast()),
                NodeId::Containers => Some(containers.clone().upcast()),
                NodeId::Image(_) | NodeId::Container(_) => None,
            }
        })
    }

    fn expand_root(&self) {
        if let Some(selection) = self.imp().selection.get()
            && let Some(model) = selection.model().and_downcast::<gtk::TreeListModel>()
            && let Some(row) = model.row(0)
        {
            row.set_expanded(true);
            selection.set_selected(0);
        }
    }

    /// A new listing arrived: update the tree in place and re-render.
    pub fn apply_snapshot(&self, snapshot: Snapshot) {
        let tree = tree::build(
            Some(&snapshot.environment),
            &snapshot.images,
            &snapshot.containers,
        );

        if let Some(stores) = self.imp().stores.get() {
            stores.root_node.apply(&tree);
            stores.images_node.apply(&tree.children[0]);
            stores.containers_node.apply(&tree.children[1]);
            fill(&stores.images, &tree.children[0].children);
            fill(&stores.containers, &tree.children[1].children);
        }

        self.imp().snapshot.replace(Some(snapshot));
        self.restore_selection();
        self.imp().content_stack.set_visible_child_name("detail");
        self.render_detail();
    }

    /// Raw inspect output for a node, cached so reselecting does not refetch.
    pub fn apply_inspect(&self, id: &NodeId, raw: serde_json::Value) {
        self.imp().raw.borrow_mut().insert(id.key(), raw);
        if self.selected() == *id {
            self.render_detail();
        }
    }

    pub fn apply_status(&self, status: &StatusView) {
        let has_data = self.imp().snapshot.borrow().is_some();

        if has_data {
            // Keep showing what we have; a blip should not blank the window.
            if !matches!(status.state, ActivityState::Connected) {
                self.toast(&status.message);
            }
            return;
        }

        let title = match status.state {
            ActivityState::Connected | ActivityState::Connecting => "Connecting",
            ActivityState::Reconnecting { .. } => "Reconnecting",
            ActivityState::Failed { .. } => "Cannot reach Docker",
        };

        let description = match &status.hint {
            Some(hint) => format!("{}\n\n{hint}", status.message),
            None => status.message.clone(),
        };

        self.imp().status_page.set_title(title);
        self.imp().status_page.set_description(Some(&description));
        self.imp().content_stack.set_visible_child_name("status");
    }

    /// Without a panel indicator, closing the window must really close it.
    pub fn set_indicator_available(&self, available: bool) {
        self.imp().keep_running.set(available);
        if !available {
            self.toast(
                "No panel indicator: this desktop has no StatusNotifier host. \
                 On GNOME, install the AppIndicator extension.",
            );
        }
    }

    fn toast(&self, message: &str) {
        if *self.imp().last_toast.borrow() == message {
            return;
        }
        self.imp().last_toast.replace(message.to_owned());
        self.imp().toasts.add_toast(adw::Toast::new(message));
    }

    fn selected(&self) -> NodeId {
        self.imp().selected.borrow().clone().unwrap_or(NodeId::Root)
    }

    fn on_selection_changed(&self, selection: &gtk::SingleSelection) {
        let Some(node) = selection
            .selected_item()
            .and_downcast::<gtk::TreeListRow>()
            .and_then(|row| row.item())
            .and_downcast::<TreeNodeObject>()
        else {
            return;
        };

        let Some(id) = NodeId::from_key(&node.key()) else {
            return;
        };

        self.imp().selected.replace(Some(id));
        self.render_detail();
    }

    /// Keep the selection across a refresh, falling back to the root if it is gone.
    fn restore_selection(&self) {
        let (Some(selection), wanted) = (self.imp().selection.get(), self.selected()) else {
            return;
        };
        let Some(model) = selection.model().and_downcast::<gtk::TreeListModel>() else {
            return;
        };

        let key = wanted.key();
        for index in 0..model.n_items() {
            let matches = model
                .row(index)
                .and_then(|row| row.item())
                .and_downcast::<TreeNodeObject>()
                .is_some_and(|node| node.key() == key);
            if matches {
                selection.set_selected(index);
                return;
            }
        }

        self.imp().selected.replace(Some(NodeId::Root));
        selection.set_selected(0);
    }

    fn render_detail(&self) {
        let selected = self.selected();

        // A selected object can vanish between refreshes; fall back to the root.
        let Some(page) = self.build_page(&selected) else {
            if selected != NodeId::Root {
                self.imp().selected.replace(Some(NodeId::Root));
                self.render_detail();
            }
            return;
        };

        self.imp().content_page.set_title(&page.title);

        let previous = self.imp().groups.take();
        let added = detail_pane::render(&self.imp().detail_page, previous, &page);
        self.imp().groups.replace(added);

        let inspected = self.imp().raw.borrow().contains_key(&selected.key());
        if matches!(selected, NodeId::Image(_) | NodeId::Container(_)) && !inspected {
            self.send(Command::Inspect(selected));
        }
    }

    /// Borrows are confined here so they cannot outlive the render.
    fn build_page(&self, selected: &NodeId) -> Option<detail::DetailPage> {
        let snapshot = self.imp().snapshot.borrow();
        let snapshot = snapshot.as_ref()?;

        let raw_cache = self.imp().raw.borrow();
        let raw = raw_cache.get(&selected.key());
        let now = now_seconds();
        let offset = chrono::Offset::fix(chrono::Local::now().offset());

        match selected {
            NodeId::Root => Some(detail::environment(
                &snapshot.environment,
                &snapshot.resolved,
                None,
            )),
            NodeId::Images => Some(detail::images(&snapshot.images)),
            NodeId::Containers => Some(detail::containers(&snapshot.containers)),
            NodeId::Image(id) => snapshot
                .images
                .iter()
                .find(|image| &image.id == id)
                .map(|image| detail::image(image, raw, now, offset)),
            NodeId::Container(id) => snapshot
                .containers
                .iter()
                .find(|container| &container.id == id)
                .map(|container| detail::container(container, raw, now, offset)),
        }
    }

    fn send(&self, command: Command) {
        if let Some(commands) = self.imp().commands.get()
            && commands.try_send(command).is_err()
        {
            tracing::warn!("the daemon runtime is no longer accepting commands");
        }
    }
}

/// Replace a store's contents. Image and container rows have no children, so nothing
/// expansion-related is lost by rebuilding them.
fn fill(store: &gtk::gio::ListStore, nodes: &[TreeNode]) {
    let objects: Vec<TreeNodeObject> = nodes.iter().map(TreeNodeObject::from_node).collect();
    store.splice(0, store.n_items(), &objects);
}

fn build_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let icon = gtk::Image::new();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let detail = gtk::Label::builder().xalign(1.0).hexpand(true).build();
        detail.add_css_class("dim-label");

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        content.append(&icon);
        content.append(&label);
        content.append(&detail);

        let expander = gtk::TreeExpander::new();
        expander.set_child(Some(&content));
        item.set_child(Some(&expander));
    });

    factory.connect_bind(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        let Some(node) = row.item().and_downcast::<TreeNodeObject>() else {
            return;
        };
        let Some(expander) = item.child().and_downcast::<gtk::TreeExpander>() else {
            return;
        };

        expander.set_list_row(Some(&row));

        let Some(content) = expander.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let Some(icon) = content.first_child().and_downcast::<gtk::Image>() else {
            return;
        };
        let Some(label) = icon.next_sibling().and_downcast::<gtk::Label>() else {
            return;
        };
        let Some(detail) = label.next_sibling().and_downcast::<gtk::Label>() else {
            return;
        };

        icon.set_icon_name(Some(&node.icon()));
        label.set_label(&node.label());
        detail.set_label(&node.detail());
        // Screen readers get the count or state, not just the name.
        expander.update_property(&[gtk::accessible::Property::Label(&format!(
            "{} {}",
            node.label(),
            node.detail()
        ))]);
    });

    factory
}
