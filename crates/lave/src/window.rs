//! The main window: sidebar tree on the left, metadata on the right.

use std::path::Path;
use std::rc::Rc;

use std::collections::{BTreeMap, HashSet};

use adw::prelude::*;
use adw::subclass::prelude::*;
use async_channel::Sender;
use gtk::glib;
use lave_core::activity::ActivityState;
use lave_core::engine::{ContainerSummary, ImageSummary, LogStream, TAIL_LINES};
use lave_core::model::action::{Action, BulkOffer, Confirmation, Offer, Tally};
use lave_core::model::detail;
use lave_core::model::format;
use lave_core::model::fs_tree::Node;
use lave_core::model::logs::{self, LogLine};
use lave_core::model::table::Table;
use lave_core::model::tree::{self, NodeId, Tone, TreeNode};

use crate::detail_pane;
use crate::runtime::{ActionRequest, BrowseTarget, Command, Snapshot, StatusView, now_seconds};
use crate::table_view::{SortOrder, TableHandlers};
use crate::tree_node::TreeNodeObject;

/// A confirmation lists what it will remove, scrolling past this height rather than
/// growing the dialog off the screen.
const CONFIRM_LIST_HEIGHT: i32 = 220;

/// One table row, in pixels. GTK will not report it before the first layout, so the
/// divider's opening position is estimated; a drag overrides the estimate either way.
const TABLE_ROW_HEIGHT: i32 = 34;
/// Column headings, the filter toggle above them, frame border and margins.
const TABLE_CHROME_HEIGHT: i32 = 116;

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};
    use std::collections::{HashMap, HashSet};

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
        /// The standing nodes, in the order the core lists them.
        pub top_nodes: Vec<TreeNodeObject>,
    }

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/com/paperstack/LaveStation/ui/window.ui")]
    pub struct LaveWindow {
        #[template_child]
        pub tree_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub paned: TemplateChild<gtk::Paned>,
        #[template_child]
        pub window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub detail_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub detail_paned: TemplateChild<gtk::Paned>,
        #[template_child]
        pub lead_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub tab_view: TemplateChild<adw::TabView>,
        #[template_child]
        pub tab_bar: TemplateChild<adw::TabBar>,
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
        pub raw: RefCell<HashMap<String, serde_json::Value>>,
        pub last_toast: RefCell<String>,
        /// Open log tabs by container ID, so streamed lines reach the right one when
        /// several are open at once.
        pub log_views: RefCell<HashMap<String, super::LogView>>,
        /// Output tabs currently open, so re-asking for one focuses it instead of
        /// stacking up a duplicate.
        pub open_tabs: RefCell<HashMap<super::TabKey, adw::TabPage>>,
        /// Keys of the objects checked for a bulk action. Held here rather than in the
        /// widgets because the pane is rebuilt on every refresh; cleared when the page
        /// changes, since a tick made on one page has no business acting from another.
        pub checked: RefCell<HashSet<String>>,
        /// The bulk-action button of the page currently rendered, if it has a table.
        pub cog: RefCell<Option<gtk::MenuButton>>,
        /// The select-all control beside it.
        pub select_all: RefCell<Option<gtk::CheckButton>>,
        /// The keys of every row on the page currently rendered, so select-all knows what
        /// it covers without interrogating the widgets.
        pub page_keys: RefCell<Vec<String>>,
        /// Set while select-all is being brought into line with the checked set, so that
        /// does not read as the user having operated it.
        pub syncing: Cell<bool>,
        /// How each table is sorted, by table id. Session-only, deliberately: the user's
        /// order is not written to the settings store.
        pub sorts: RefCell<HashMap<String, super::SortOrder>>,
        /// The context menu currently on screen, so the next one can replace it rather
        /// than requiring a dismissing click of its own.
        pub open_menu: RefCell<Option<gtk::Popover>>,
        /// Whether the pointer is over that menu, so a press on the menu itself is not
        /// mistaken for the press that dismisses it.
        pub menu_hovered: Cell<bool>,
        /// The open file browser, if any: what it is browsing, where it is, and the
        /// widgets a new listing replaces.
        pub browser: RefCell<Option<super::Browser>>,
        /// Live FUSE mounts, keyed by container ID. Held for the session: unmounting
        /// while a file manager is still looking at one would be rude. Dropping them
        /// unmounts, which is why they are kept rather than detached.
        pub mounts: RefCell<HashMap<String, crate::fuse_mount::Mount>>,
        /// Persisted view preferences, held here and written back when they change.
        pub settings: RefCell<lave_core::settings::Settings>,
        /// Where they are actually kept.
        pub prefs: crate::prefs::Prefs,
        /// Where the user dragged the table divider, if they have. Session-only: each
        /// launch sizes the table to the running containers afresh.
        pub lead_position: Cell<Option<i32>>,
        /// Set while the divider is being positioned in code, so that does not register
        /// as a drag.
        pub positioning: Cell<bool>,
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
            self.obj().store_sidebar_width();
            // A popover surviving into the next time the window is shown is nobody's
            // idea of useful.
            self.obj().close_context_menu();

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

        // The divider is draggable; where the user left it last time is restored here.
        let settings = self.imp().prefs.load();
        self.imp().paned.set_position(settings.sidebar_width);
        self.imp().settings.replace(settings);

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

        // A drag of the table divider sticks until the window closes; a refresh must
        // not silently undo it.
        self.imp()
            .detail_paned
            .connect_position_notify(glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |paned| {
                    if !window.imp().positioning.get() {
                        window.imp().lead_position.set(Some(paned.position()));
                    }
                }
            ));

        self.setup_actions();
        self.setup_menu_dismissal();
        self.setup_tabs();

        // The root node is selected at startup, as the README requires.
        self.expand_root();
        self.render_detail();
    }

    fn build_stores() -> imp::Stores {
        let empty = tree::build(None, &[], &[]);
        let root_node = TreeNodeObject::from_node(&empty);

        let root = gtk::gio::ListStore::new::<TreeNodeObject>();
        root.append(&root_node);

        // One object per standing node, in the order the core lists them: which of
        // Containers and Images comes first is decided there and nowhere else.
        let top_nodes: Vec<TreeNodeObject> = empty
            .children
            .iter()
            .map(TreeNodeObject::from_node)
            .collect();

        let top = gtk::gio::ListStore::new::<TreeNodeObject>();
        for node in &top_nodes {
            top.append(node);
        }

        imp::Stores {
            root,
            top,
            images: gtk::gio::ListStore::new::<TreeNodeObject>(),
            containers: gtk::gio::ListStore::new::<TreeNodeObject>(),
            root_node,
            top_nodes,
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

        self.update_prune_actions(&snapshot);

        if let Some(stores) = self.imp().stores.get() {
            stores.root_node.apply(&tree);

            // By identity rather than by position, so the sidebar's order can change
            // without any risk of a node being given another one's contents.
            for object in &stores.top_nodes {
                if let Some(id) = NodeId::from_key(&object.key())
                    && let Some(node) = tree.child(&id)
                {
                    object.apply(node);
                }
            }

            for (id, store) in [
                (NodeId::Images, &stores.images),
                (NodeId::Containers, &stores.containers),
            ] {
                fill(store, tree.child(&id).map_or(&[], |node| &node.children));
            }
        }

        self.prune_checks(&snapshot);
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

    /// Close the context menu when a press lands anywhere else in the window.
    ///
    /// The menu does not autohide, so GTK will not do this. Two things make the hand-rolled
    /// version behave, both of which the version 4 attempt got wrong:
    ///
    /// * **The phase.** Capture runs root-first, so this fires *before* the row that was
    ///   clicked. Right-clicking a second row therefore closes the first menu and then
    ///   opens the second, in that order, on one press. In the bubble phase the order
    ///   would reverse and the new menu would be shut the instant it opened.
    /// * **Telling a press on the menu apart from a press elsewhere.** Version 4 compared
    ///   coordinates — `compute_bounds` of the popover within the overlay. A popover is a
    ///   `GtkNative` with a surface of its own, so that is a comparison across surfaces
    ///   and it only has to be wrong once to dismiss a menu the user was choosing from.
    ///   Whether the pointer is over the menu is asked of the menu instead, which needs no
    ///   coordinates at all.
    ///
    /// The gesture never claims its sequence, so it cannot swallow the press it saw.
    fn setup_menu_dismissal(&self) {
        let outside = gtk::GestureClick::new();
        outside.set_button(0);
        outside.set_propagation_phase(gtk::PropagationPhase::Capture);
        outside.connect_pressed(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _, _, _| {
                if !window.imp().menu_hovered.get() {
                    window.close_context_menu();
                }
            }
        ));
        self.imp().toasts.add_controller(outside);
    }

    /// Follow the pointer in and out of the open menu.
    fn watch_menu_pointer(&self, popover: &gtk::Popover) {
        let motion = gtk::EventControllerMotion::new();
        motion.connect_enter(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _, _| window.imp().menu_hovered.set(true)
        ));
        motion.connect_leave(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_| window.imp().menu_hovered.set(false)
        ));
        popover.add_controller(motion);
    }

    /// Register the actions the primary menu invokes.
    fn setup_actions(&self) {
        for (name, action) in [
            ("prune-containers", Action::PruneContainers),
            ("prune-images", Action::PruneImages),
        ] {
            let prune = gtk::gio::SimpleAction::new(name, None);
            prune.connect_activate(glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |_, _| window.invoke_prune(action)
            ));
            self.add_action(&prune);
        }
    }

    /// Offer a prune from the primary menu, with the same preview a button gave.
    fn invoke_prune(&self, action: Action) {
        let borrowed = self.imp().snapshot.borrow();
        let Some(snapshot) = borrowed.as_ref() else {
            return;
        };

        let offer =
            lave_core::model::action::for_environment(&snapshot.containers, &snapshot.images)
                .into_iter()
                .find(|offer| offer.action == action);
        drop(borrowed);

        match offer {
            Some(offer) => self.invoke(&NodeId::Root, &offer),
            // The menu item is insensitive in this case, so this is belt and braces.
            None => self.toast("There is nothing to prune"),
        }
    }

    /// Enable each prune only when it would actually remove something.
    ///
    /// The snapshot is passed in rather than read from the window: this runs as part of
    /// applying a new one, and reading the field would ask the *previous* listing what is
    /// prunable — which on the first listing is nothing at all, leaving both items greyed
    /// out until a second one happened to arrive.
    fn update_prune_actions(&self, snapshot: &Snapshot) {
        let offers =
            lave_core::model::action::for_environment(&snapshot.containers, &snapshot.images);

        for (name, action) in [
            ("prune-containers", Action::PruneContainers),
            ("prune-images", Action::PruneImages),
        ] {
            let available = offers.iter().any(|offer| offer.action == action);
            if let Some(found) = self.lookup_action(name)
                && let Ok(simple) = found.downcast::<gtk::gio::SimpleAction>()
            {
                simple.set_enabled(available);
            }
        }
    }

    /// Show the actions for `node` at a point within `anchor`.
    pub(crate) fn show_context_menu(
        &self,
        node: &NodeId,
        anchor: &impl IsA<gtk::Widget>,
        x: f64,
        y: f64,
    ) {
        // The popover hangs from the toast overlay rather than from the widget that was
        // clicked, with the point translated into the overlay's coordinates.
        //
        // A table cell is not a durable anchor: the detail pane is rebuilt whenever a
        // refresh arrives, and a popover whose parent has left the widget tree cannot be
        // realized — which produced a GTK critical and, under broadway, a crash. The
        // overlay is the outermost content and is never rebuilt.
        let overlay = self.imp().toasts.clone();
        // Widget coordinates are f32; the pointer position does not need more than that.
        #[allow(clippy::cast_possible_truncation)]
        let clicked = gtk::graphene::Point::new(x as f32, y as f32);
        let Some(point) = anchor.as_ref().compute_point(&overlay, &clicked) else {
            // Only happens if the widget has already left the tree, which is not
            // something to open a menu about.
            return;
        };

        let offers = self.offers_for(node);
        if offers.is_empty() {
            return;
        }

        let entries: Vec<MenuEntry> = offers.iter().map(MenuEntry::from_offer).collect();
        let node = node.clone();
        let popover = menu_popover(
            &entries,
            // The whole point of a row menu: right-clicking a different row moves to it in
            // one click, which a grab would spend on dismissing this one.
            Dismissal::Watched,
            glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |index| {
                    if let Some(offer) = offers.get(index) {
                        window.invoke(&node, offer);
                    }
                }
            ),
        );

        // Replaces whatever was already showing rather than stacking a second one on it.
        self.close_context_menu();

        popover.set_parent(&overlay);
        // Truncating toward zero is right here: the rectangle only has to name the pixel
        // the pointer was over.
        #[allow(clippy::cast_possible_truncation)]
        let at = gtk::gdk::Rectangle::new(point.x() as i32, point.y() as i32, 1, 1);
        popover.set_pointing_to(Some(&at));
        self.watch_menu_pointer(&popover);
        // The press that opened this landed on a row, not on the menu that did not yet
        // exist; the pointer only counts as over the menu once it has been told so.
        self.imp().menu_hovered.set(false);

        // Without a grab the popover is not given the keyboard, so it has to ask.
        popover.popup();
        popover.grab_focus();

        popover.connect_closed(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |popover| {
                // Unparenting from inside the signal would destroy the emitter, and the
                // menu on record may already be a *later* one: closing this to open the
                // next fires here after the replacement has been stored.
                let popover = popover.clone();
                glib::idle_add_local_once(move || {
                    if window.imp().open_menu.borrow().as_ref() == Some(&popover) {
                        window.imp().open_menu.take();
                    }
                    retire(&popover);
                });
            }
        ));

        self.imp().open_menu.replace(Some(popover));
    }

    /// The icon this object carries in the sidebar, as a `GIcon` for a tab.
    fn node_icon(&self, node: &NodeId) -> gtk::gio::ThemedIcon {
        let borrowed = self.imp().snapshot.borrow();
        let containers = borrowed
            .as_ref()
            .map(|snapshot| snapshot.containers.as_slice())
            .unwrap_or_default();
        gtk::gio::ThemedIcon::new(tree::node_icon(node, containers))
    }

    /// Dismiss the context menu, if one is showing.
    pub(crate) fn close_context_menu(&self) {
        self.imp().menu_hovered.set(false);
        if let Some(popover) = self.imp().open_menu.take() {
            popover.popdown();
            retire(&popover);
        }
    }

    /// What may be done to this object right now.
    fn offers_for(&self, node: &NodeId) -> Vec<Offer> {
        let borrowed = self.imp().snapshot.borrow();
        let Some(snapshot) = borrowed.as_ref() else {
            return Vec::new();
        };

        match node {
            NodeId::Container(id) => snapshot
                .containers
                .iter()
                .find(|container| container.id == *id)
                .map(|container| {
                    lave_core::model::action::for_container(container, &snapshot.images)
                })
                .unwrap_or_default(),
            NodeId::Image(id) => snapshot
                .images
                .iter()
                .find(|image| image.id == *id)
                .map(|image| lave_core::model::action::for_image(image, &snapshot.containers))
                .unwrap_or_default(),
            // The daemon's own actions are prunes, which live on the primary menu.
            _ => Vec::new(),
        }
    }

    /// Tick or untick an object for the next bulk action.
    fn set_checked(&self, node: &NodeId, on: bool) {
        let changed = {
            let mut checked = self.imp().checked.borrow_mut();
            if on {
                checked.insert(node.key())
            } else {
                checked.remove(&node.key())
            }
        };

        if changed {
            self.update_bulk_controls();
        }
    }

    /// Forget every tick: the page has changed, or an action has just consumed them.
    fn clear_checks(&self) {
        if self.imp().checked.borrow().is_empty() {
            return;
        }
        self.imp().checked.borrow_mut().clear();
        self.update_bulk_controls();
    }

    /// Drop ticks for objects that are no longer there, so a bulk action cannot be
    /// launched against something already removed.
    fn prune_checks(&self, snapshot: &Snapshot) {
        let present: HashSet<String> = snapshot
            .containers
            .iter()
            .map(|container| NodeId::Container(container.id.clone()).key())
            .chain(
                snapshot
                    .images
                    .iter()
                    .map(|image| NodeId::Image(image.id.clone()).key()),
            )
            .collect();

        let changed = {
            let mut checked = self.imp().checked.borrow_mut();
            let before = checked.len();
            checked.retain(|key| present.contains(key));
            checked.len() != before
        };

        if changed {
            self.update_bulk_controls();
        }
    }

    /// Take charge of a freshly rendered cog.
    ///
    /// Its menu is built when it opens rather than when it is created, so it always
    /// describes what is checked at that moment rather than what was checked when the
    /// pane was last rebuilt.
    fn adopt_cog(&self, cog: &gtk::MenuButton) {
        cog.set_create_popup_func(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |button| {
                let offers = window.bulk_offers();
                let entries: Vec<MenuEntry> = offers.iter().map(MenuEntry::from_bulk).collect();
                let popover = menu_popover(
                    &entries,
                    // Hung off a button, so GTK's own dismissal is what is wanted.
                    Dismissal::Grab,
                    glib::clone!(
                        #[weak]
                        window,
                        move |index| {
                            if let Some(offer) = offers.get(index) {
                                window.invoke_bulk(offer);
                            }
                        }
                    ),
                );
                button.set_popover(Some(&popover));
            }
        ));

        self.imp().cog.replace(Some(cog.clone()));
        self.update_bulk_controls();
    }

    /// Take charge of a freshly rendered select-all control.
    fn adopt_select_all(&self, check: &gtk::CheckButton) {
        check.connect_toggled(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |check| {
                if !window.imp().syncing.get() {
                    window.set_all_checked(check.is_active());
                }
            }
        ));

        self.imp().select_all.replace(Some(check.clone()));
        self.update_bulk_controls();
    }

    /// Tick or untick every row on the page at once.
    fn set_all_checked(&self, on: bool) {
        {
            let keys = self.imp().page_keys.borrow();
            let mut checked = self.imp().checked.borrow_mut();
            for key in keys.iter() {
                if on {
                    checked.insert(key.clone());
                } else {
                    checked.remove(key);
                }
            }
        }

        self.update_bulk_controls();

        // The ticks live in the window, so the rows have to be rebuilt to redraw theirs —
        // but not from inside the control's own signal handler, which the rebuild would
        // destroy while it is still running.
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move || window.render_detail()
        ));
    }

    /// Bring the cog and the select-all control into line with what is checked.
    ///
    /// The cog is insensitive until something is checked, as there is then nothing for it
    /// to act on; select-all shows the mixed state when only some rows are ticked.
    fn update_bulk_controls(&self) {
        let tally = self.tally();

        if let Some(cog) = self.imp().cog.borrow().as_ref() {
            cog.set_sensitive(tally.any());
            cog.set_tooltip_text(Some(if tally.any() {
                "Act on the checked rows"
            } else {
                "Check some rows to act on them"
            }));
        }

        if let Some(check) = self.imp().select_all.borrow().as_ref() {
            // Setting these emits `toggled`, which would read as the user having clicked.
            self.imp().syncing.set(true);
            check.set_sensitive(!tally.is_empty());
            check.set_inconsistent(tally.is_partial());
            check.set_active(tally.is_complete());
            check.set_tooltip_text(Some(tally.select_all_label()));
            self.imp().syncing.set(false);
        }
    }

    /// How much of the page is checked.
    fn tally(&self) -> Tally {
        let keys = self.imp().page_keys.borrow();
        let checked = self.imp().checked.borrow();
        Tally::new(
            keys.len(),
            keys.iter().filter(|key| checked.contains(*key)).count(),
        )
    }

    /// What may be done to the checked objects. Only those still in the listing count: a
    /// tick that outlived its object acts on nothing.
    fn bulk_offers(&self) -> Vec<BulkOffer> {
        let borrowed = self.imp().snapshot.borrow();
        let Some(snapshot) = borrowed.as_ref() else {
            return Vec::new();
        };
        let checked = self.imp().checked.borrow();

        let containers: Vec<&ContainerSummary> = snapshot
            .containers
            .iter()
            .filter(|container| checked.contains(&NodeId::Container(container.id.clone()).key()))
            .collect();
        let images: Vec<&ImageSummary> = snapshot
            .images
            .iter()
            .filter(|image| checked.contains(&NodeId::Image(image.id.clone()).key()))
            .collect();

        lave_core::model::action::for_selection(
            &containers,
            &images,
            &snapshot.containers,
            &snapshot.images,
        )
    }

    /// Act on a chosen cog item, confirming first when the offer says to.
    fn invoke_bulk(&self, offer: &BulkOffer) {
        match &offer.confirmation {
            Some(confirmation) => {
                let window = self.clone();
                let offer = offer.clone();
                self.confirm(confirmation, move || window.dispatch_bulk(&offer));
            }
            None => self.dispatch_bulk(offer),
        }
    }

    /// Send the whole selection as one request, so the outcome is reported once rather
    /// than as a stack of toasts.
    fn dispatch_bulk(&self, offer: &BulkOffer) {
        let requests: Vec<ActionRequest> = offer
            .targets
            .iter()
            .map(|target| ActionRequest {
                action: target.action,
                id: target.id.clone(),
                label: target.label.clone(),
            })
            .collect();

        if requests.is_empty() {
            return;
        }

        self.send(Command::ActMany(requests));
        // The ticks refer to objects that are about to change state or vanish.
        self.clear_checks();
    }

    /// Bind the tab bar to the view and make the metadata page permanent.
    fn setup_tabs(&self) {
        let view = self.imp().tab_view.clone();
        self.imp().tab_bar.set_view(Some(&view));

        // Page zero is the metadata, added by the template. Pinning it removes its close
        // button: it follows the selection and there is nothing sensible to close it to.
        if let Some(page) = view.nth_page(0).into() {
            let page: adw::TabPage = page;
            page.set_title("Details");
            view.set_page_pinned(&page, true);
        }

        view.connect_close_page(glib::clone!(
            #[weak(rename_to = window)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, page| {
                window.on_tab_closed(page);
                glib::Propagation::Proceed
            }
        ));
    }

    /// Release whatever a closing tab was holding open.
    fn on_tab_closed(&self, page: &adw::TabPage) {
        let mut tabs = self.imp().open_tabs.borrow_mut();
        let Some(key) = tabs
            .iter()
            .find(|(_, candidate)| *candidate == page)
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        tabs.remove(&key);
        drop(tabs);

        // A follower left running against a chatty container, or a scratch container
        // standing in for an image, would otherwise outlive the tab that wanted it.
        match key.kind {
            TabKind::Logs => {
                self.imp().log_views.borrow_mut().remove(&key.object_id);
                self.send(Command::StopLogs {
                    container_id: key.object_id.clone(),
                });
            }
            TabKind::Files => {
                self.imp().browser.replace(None);
                self.send(Command::StopBrowsing);
            }
            TabKind::Dockerfile => {}
        }
    }

    /// Focus the tab for this object and kind, or make one from `build`.
    fn open_tab(
        &self,
        key: TabKey,
        node: &NodeId,
        title: &str,
        build: impl FnOnce() -> gtk::Widget,
    ) -> Option<adw::TabPage> {
        let view = self.imp().tab_view.clone();

        // Re-asking for something already open should bring it forward, not stack up a
        // second copy of it.
        if let Some(existing) = self.imp().open_tabs.borrow().get(&key) {
            view.set_selected_page(existing);
            return None;
        }

        let page = view.append(&build());
        page.set_title(title);
        page.set_icon(Some(&self.node_icon(node)));
        page.set_live_thumbnail(true);
        view.set_selected_page(&page);

        self.imp().open_tabs.borrow_mut().insert(key, page.clone());
        Some(page)
    }

    /// Mount the selection's filesystem and hand it to the desktop's file manager.
    ///
    /// The hand-off is `gtk::FileLauncher`, which routes through the XDG Desktop Portal
    /// where there is one and falls back to the session's default handler otherwise —
    /// which is what makes this work on KDE and Xfce rather than only GNOME.
    fn open_in_file_manager(&self, node: &NodeId) {
        // An image needs a container to stand in for it, and that is the runtime's job;
        // mounting one would need the scratch container's ID back here first.
        let NodeId::Container(container_id) = node.clone() else {
            self.toast(
                "Only containers can be mounted so far. Use Files to browse an image \
                 in the window.",
            );
            return;
        };

        let (_, label) = self.action_target(node);

        if let Some(existing) = self.imp().mounts.borrow().get(&container_id) {
            launch_file_manager(self, existing.path());
            return;
        }

        let Some(endpoint) = self
            .imp()
            .snapshot
            .borrow()
            .as_ref()
            .map(|snapshot| snapshot.resolved.endpoint.path().to_path_buf())
        else {
            self.toast("Not connected to a daemon yet");
            return;
        };

        match crate::fuse_mount::mount(&endpoint, &container_id, &label) {
            Ok(mount) => {
                launch_file_manager(self, mount.path());
                self.imp().mounts.borrow_mut().insert(container_id, mount);
            }
            Err(error) => {
                self.apply_action_outcome(&format!("Could not mount {label}: {error}"), true);
            }
        }
    }

    /// Unmount everything before the process goes away.
    ///
    /// Dropping a `Mount` unmounts it, so this is really just "drop them now, while
    /// there is still a running program to do it in".
    pub fn release_mounts(&self) {
        self.imp().mounts.borrow_mut().clear();
    }

    /// Open a file tree tab for the selection.
    ///
    /// The tree expands in place rather than drilling in. Because the archive endpoint
    /// is recursive, the first fetch indexes the whole subtree, so expanding a directory
    /// costs a round trip only the first time and nothing thereafter.
    fn open_browser(&self, target: &BrowseTarget, title: &str) {
        let object_id = match target {
            BrowseTarget::Container(id) | BrowseTarget::Image(id) => id.clone(),
        };

        let key = TabKey {
            kind: TabKind::Files,
            object_id,
        };

        let root = gtk::gio::ListStore::new::<crate::fs_node::FsNodeObject>();
        let notice = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .margin_top(6)
            .margin_start(12)
            .margin_end(12)
            .build();
        notice.add_css_class("warning");

        let stores = std::collections::HashMap::from([("/".to_owned(), root.clone())]);
        self.imp().browser.replace(Some(Browser {
            target: target.clone(),
            stores,
            root: root.clone(),
            notice: notice.clone(),
        }));

        let node = match target {
            BrowseTarget::Container(id) => NodeId::Container(id.clone()),
            BrowseTarget::Image(id) => NodeId::Image(id.clone()),
        };
        let opened = self.open_tab(key, &node, &format!("{title} \u{2014} Files"), || {
            let tree = gtk::TreeListModel::new(root, false, false, {
                let window = self.clone();
                move |item| window.children_model(item)
            });

            let view = gtk::ListView::builder()
                .model(&gtk::NoSelection::new(Some(tree)))
                .factory(&build_fs_factory())
                .build();

            let scroller = gtk::ScrolledWindow::builder()
                .child(&view)
                .vexpand(true)
                .build();

            let layout = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .build();
            layout.append(&notice);
            layout.append(&scroller);
            layout.upcast()
        });

        if opened.is_some() {
            self.send(Command::Browse {
                target: target.clone(),
                path: "/".to_owned(),
            });
        }
    }

    /// Supply the model for a directory's children, asking the daemon to fill it.
    ///
    /// `GtkTreeListModel` needs a model back immediately, so this returns an empty store
    /// and populates it when the listing arrives.
    fn children_model(&self, item: &glib::Object) -> Option<gtk::gio::ListModel> {
        let node = item.downcast_ref::<crate::fs_node::FsNodeObject>()?;
        if !node.expandable() {
            return None;
        }

        let path = node.path();
        // GTK calls this from inside signal emission, where a panic aborts rather than
        // unwinds. A contended borrow means some caller is holding it across a GTK call,
        // which is a bug — but reporting it beats taking the process down.
        let Ok(mut borrowed) = self.imp().browser.try_borrow_mut() else {
            tracing::error!("the file tree was asked for children while already borrowed");
            return None;
        };
        let browser = borrowed.as_mut()?;

        if let Some(existing) = browser.stores.get(&path) {
            return Some(existing.clone().upcast());
        }

        let store = gtk::gio::ListStore::new::<crate::fs_node::FsNodeObject>();
        browser.stores.insert(path.clone(), store.clone());
        let target = browser.target.clone();
        drop(borrowed);

        self.send(Command::Browse { target, path });

        Some(store.upcast())
    }

    /// Fill in a directory whose listing has arrived.
    pub fn apply_listing(&self, path: &str, entries: &[Node], notice: Option<&str>) {
        // The borrow is taken, used, and released *before* the splice below.
        //
        // Splicing emits `items-changed` synchronously, which sends GTK straight back
        // into `children_model` to ask whether the new rows expand — and that wants the
        // same `RefCell` mutably. Holding the borrow across the splice is a re-entrant
        // double borrow, and because it happens inside a `nounwind` GTK callback it
        // aborts the process rather than unwinding.
        let store = {
            let borrowed = self.imp().browser.borrow();
            let Some(browser) = borrowed.as_ref() else {
                // The tab closed while this was in flight.
                return;
            };

            match notice {
                Some(text) => {
                    browser.notice.set_label(text);
                    browser.notice.set_visible(true);
                }
                None => browser.notice.set_visible(false),
            }

            browser.stores.get(path).cloned()
        };

        let Some(store) = store else {
            return;
        };

        // Directories first, then files: the usual ordering for something being walked.
        let mut ordered: Vec<&Node> = entries.iter().collect();
        ordered.sort_by_key(|node| (!node.kind.is_directory(), node.name.to_lowercase()));

        let objects: Vec<crate::fs_node::FsNodeObject> = ordered
            .into_iter()
            .map(crate::fs_node::FsNodeObject::from_node)
            .collect();

        store.splice(0, store.n_items(), &objects);
    }

    /// Open a log tab for a container and start following it.
    ///
    /// The tail is the default view: a container that has been running for a week has
    /// more output than anyone wants delivered before the tab appears, and what is
    /// interesting is almost always the end of it.
    fn open_logs(&self, container_id: &str, title: &str) {
        let key = TabKey {
            kind: TabKind::Logs,
            object_id: container_id.to_owned(),
        };

        // Already open: focus it. Rebuilding would leave the visible tab attached to a
        // buffer nothing writes to any more.
        if let Some(existing) = self.imp().open_tabs.borrow().get(&key) {
            self.imp().tab_view.set_selected_page(existing);
            return;
        }

        let buffer = gtk::TextBuffer::new(None);
        for tag in log_tags() {
            buffer.tag_table().add(&tag);
        }

        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(12)
            .bottom_margin(12)
            .left_margin(12)
            .right_margin(12)
            .build();

        let scroller = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .build();

        // A permanent mark at the end of the buffer, with right gravity so lines are
        // inserted before it rather than after.
        //
        // The viewer scrolls to this mark rather than driving the scrollbar itself. A
        // text view lays its lines out lazily, so at the moment a line is inserted there
        // is no position to scroll to yet; `scroll_to_mark` is the one call that knows to
        // finish the job once there is. Setting the adjustment instead lands short of the
        // end and is undone again by the view's own scrolling a moment later.
        buffer.create_mark(Some(END_MARK), &buffer.end_iter(), false);

        let following = std::rc::Rc::new(std::cell::Cell::new(true));

        // Whether the viewer is still following is taken from what the user does, not
        // from where the view has ended up: while output is arriving the view is
        // somewhere between the two more or less constantly.
        let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let interrupt = std::rc::Rc::clone(&following);
        controller.connect_scroll(move |_, _, delta| {
            if delta < 0.0 {
                interrupt.set(false);
            }
            glib::Propagation::Proceed
        });
        scroller.add_controller(controller);

        // Paging up with the keyboard says the same thing as scrolling up with a wheel.
        let keys = gtk::EventControllerKey::new();
        let interrupt = std::rc::Rc::clone(&following);
        keys.connect_key_pressed(move |_, key, _, _| {
            if matches!(
                key,
                gtk::gdk::Key::Page_Up | gtk::gdk::Key::Up | gtk::gdk::Key::Home
            ) {
                interrupt.set(false);
            }
            glib::Propagation::Proceed
        });
        view.add_controller(keys);

        // Arriving back at the bottom, by whatever means, resumes it.
        let resume = std::rc::Rc::clone(&following);
        scroller.connect_edge_reached(move |_, edge| {
            if edge == gtk::PositionType::Bottom {
                resume.set(true);
            }
        });

        self.imp().log_views.borrow_mut().insert(
            container_id.to_owned(),
            LogView {
                buffer,
                view: view.clone(),
                scroller: scroller.clone(),
                following,
            },
        );

        let node = NodeId::Container(container_id.to_owned());
        let owned_id = container_id.to_owned();
        self.open_tab(key, &node, &format!("{title} \u{2014} Logs"), || {
            let layout = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .build();
            layout.append(&self.log_range_toggle(&owned_id));
            layout.append(&scroller);
            layout.upcast()
        });

        self.send(Command::Logs {
            container_id: container_id.to_owned(),
            follow: true,
            tail: Some(TAIL_LINES),
        });
    }

    /// The tail / whole-log switch, in the manner of the table's own filter.
    fn log_range_toggle(&self, container_id: &str) -> gtk::Box {
        let tail = gtk::ToggleButton::builder()
            .label("Tail")
            .tooltip_text(format!("Follow the last {TAIL_LINES} lines as they arrive"))
            .active(true)
            .build();
        let whole = gtk::ToggleButton::builder()
            .label("Whole Log")
            .tooltip_text("Load everything the container has written, and keep following")
            .group(&tail)
            .build();

        // Only the button becoming active reports, so the pair produces one change.
        for (button, whole_log) in [(&tail, false), (&whole, true)] {
            let window = self.clone();
            let container_id = container_id.to_owned();
            button.connect_toggled(move |button| {
                if button.is_active() {
                    window.set_log_range(&container_id, whole_log);
                }
            });
        }

        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .build();
        buttons.add_css_class("linked");
        buttons.append(&tail);
        buttons.append(&whole);

        let holder = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .halign(gtk::Align::End)
            .margin_top(6)
            .margin_end(12)
            .build();
        holder.append(&buttons);
        holder
    }

    /// Switch a log tab between the tail and the whole log.
    ///
    /// The stream is restarted rather than extended: the daemon has no way to send the
    /// earlier lines of a stream already in progress, so the buffer is cleared and
    /// refilled. The runtime replaces this container's stream and leaves every other
    /// tab's alone.
    fn set_log_range(&self, container_id: &str, whole: bool) {
        let view = self.imp().log_views.borrow().get(container_id).cloned();
        let Some(view) = view else {
            return;
        };
        view.buffer.set_text("");
        // Refilling starts at the end again, whatever the user was reading before.
        view.following.set(true);

        self.send(Command::Logs {
            container_id: container_id.to_owned(),
            follow: true,
            tail: if whole { None } else { Some(TAIL_LINES) },
        });
    }

    /// Append streamed lines, trimming as many from the top as the transcript dropped.
    pub fn apply_log_lines(&self, container_id: &str, lines: &[LogLine], dropped: usize) {
        // Cloned out and the borrow released: inserting into a text buffer runs GTK code
        // that must not find this map already borrowed.
        let view = self.imp().log_views.borrow().get(container_id).cloned();
        let Some(view) = view else {
            // The tab closed while these were in flight.
            return;
        };
        let buffer = &view.buffer;

        for _ in 0..dropped {
            let mut start = buffer.start_iter();
            let mut next = start;
            if next.forward_line() {
                buffer.delete(&mut start, &mut next);
            }
        }

        for line in lines {
            let first = buffer.end_iter().offset();
            let mut end = buffer.end_iter();
            buffer.insert(&mut end, &format!("{}\n", line.text));

            if line.stream == LogStream::Stderr {
                let start = buffer.iter_at_offset(first);
                let stop = buffer.end_iter();
                buffer.apply_tag_by_name(STDERR_TAG, &start, &stop);
            }

            // Structured output is worth reading rather than scanning, so a line that is
            // a whole JSON object gets its keys and values picked out. `highlight`
            // returns character offsets, which is what a text buffer wants.
            for span in logs::highlight(&line.text) {
                let start = buffer.iter_at_offset(first + i32::try_from(span.start).unwrap_or(0));
                let stop = buffer.iter_at_offset(first + i32::try_from(span.end).unwrap_or(0));
                buffer.apply_tag_by_name(token_tag(span.token), &start, &stop);
            }
        }

        // Re-issued for every batch, so a view that something else has scrolled — the
        // text view's own housekeeping does, now and then — is brought back to the end
        // rather than left stranded there.
        if view.following.get() {
            scroll_to_end(&view.view, buffer);
        }
    }

    /// Say why a stream stopped, but only when something went wrong: a container that
    /// simply finished writing needs no announcement.
    pub fn apply_logs_ended(&self, container_id: &str, error: Option<&str>) {
        let Some(error) = error else {
            return;
        };

        let label = self
            .action_target(&NodeId::Container(container_id.to_owned()))
            .1;
        self.apply_action_outcome(&format!("The log stream for {label} ended: {error}"), true);
    }

    /// Show a reconstructed Dockerfile in a tab.
    ///
    /// Its caveats are rendered into the text as leading comments, so they travel with
    /// it when copied out — which is the point of putting them there rather than in the
    /// tab's chrome.
    pub fn apply_dockerfile(&self, image_id: &str, title: &str, text: &str) {
        let key = TabKey {
            kind: TabKind::Dockerfile,
            object_id: image_id.to_owned(),
        };
        let owned = text.to_owned();

        let node = NodeId::Image(image_id.to_owned());
        self.open_tab(key, &node, &format!("{title} \u{2014} Dockerfile"), || {
            let view = gtk::TextView::builder()
                .editable(false)
                .cursor_visible(false)
                .monospace(true)
                .top_margin(12)
                .bottom_margin(12)
                .left_margin(12)
                .right_margin(12)
                .build();
            view.buffer().set_text(&owned);

            let scroller = gtk::ScrolledWindow::builder()
                .child(&view)
                .vexpand(true)
                .build();

            let copy = gtk::Button::builder()
                .label("Copy")
                .halign(gtk::Align::End)
                .margin_top(6)
                .margin_end(12)
                .build();
            let clipboard_text = owned.clone();
            copy.connect_clicked(glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |_| {
                    window.clipboard().set_text(&clipboard_text);
                    window.toast("Dockerfile copied");
                }
            ));

            let layout = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .build();
            layout.append(&copy);
            layout.append(&scroller);
            layout.upcast()
        });
    }

    /// Report what an action did. Failures persist longer, since they need reading
    /// rather than merely noticing.
    pub fn apply_action_outcome(&self, message: &str, failed: bool) {
        // Bypasses `toast`'s duplicate suppression: stopping two containers in a row
        // produces two identical messages, and swallowing the second would read as the
        // action having done nothing.
        let toast = adw::Toast::builder()
            .title(message)
            .timeout(if failed { 8 } else { 3 })
            .build();
        self.imp().toasts.add_toast(toast);
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

        // A refresh re-selects the same object, which notifies because the row's item is
        // new; only a genuine change of page discards the ticks.
        let previous = self.imp().selected.replace(Some(id.clone()));
        if previous.as_ref() != Some(&id) {
            self.clear_checks();
        }

        self.render_detail();
    }

    /// Keep the selection across a refresh, falling back to the root if it is gone.
    fn restore_selection(&self) {
        if self.select_key(&self.selected().key()) {
            return;
        }

        self.imp().selected.replace(Some(NodeId::Root));
        if let Some(selection) = self.imp().selection.get() {
            selection.set_selected(0);
        }
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

        if let Some(details) = self.imp().tab_view.nth_page(0).into() {
            let details: adw::TabPage = details;
            details.set_icon(Some(&self.node_icon(&selected)));
        }

        self.imp().window_title.set_title(&page.title);
        self.imp()
            .window_title
            .set_subtitle(page.subtitle.as_deref().unwrap_or_default());

        // These belong to the widgets about to be replaced; the window keeps whatever the
        // new render hands it.
        self.imp().cog.replace(None);
        self.imp().select_all.replace(None);

        // What select-all covers, taken from the page rather than from the widgets, since
        // the widgets only exist for the rows currently scrolled into view.
        self.imp().page_keys.replace(
            page.table
                .as_ref()
                .map(|table| table.rows.iter().map(|row| row.key.key()).collect())
                .unwrap_or_default(),
        );

        let table_id = page.table.as_ref().map_or("", |table| table.id);
        let handlers = self.handlers(table_id);
        let state = self.table_state(page.table.as_ref());
        detail_pane::render(
            &self.imp().lead_box,
            &self.imp().detail_box,
            &page,
            &state,
            &handlers,
        );
        self.position_divider(page.table_filter.as_ref());

        let inspected = self.imp().raw.borrow().contains_key(&selected.key());
        if matches!(selected, NodeId::Image(_) | NodeId::Container(_)) && !inspected {
            self.send(Command::Inspect(selected));
        }
    }

    /// Give the leading table the height its running containers ask for, unless the user
    /// has already dragged the divider somewhere of their own choosing.
    fn position_divider(&self, filter: Option<&detail::TableFilter>) {
        let Some(filter) = filter else {
            return;
        };

        let wanted = self.imp().lead_position.get().unwrap_or_else(|| {
            let rows = i32::try_from(filter.visible_rows).unwrap_or(i32::MAX);
            TABLE_CHROME_HEIGHT + rows.saturating_mul(TABLE_ROW_HEIGHT)
        });

        self.imp().positioning.set(true);
        self.imp().detail_paned.set_position(wanted);
        self.imp().positioning.set(false);
    }

    /// The callbacks the detail pane needs. Rebuilt per render, since the widgets it
    /// attaches them to are rebuilt too, and scoped to the table on the page: the sort
    /// and the column widths are stored against it by name.
    fn handlers(&self, table: &str) -> detail_pane::Handlers {
        let table = table.to_owned();

        detail_pane::Handlers {
            navigate: {
                let window = self.clone();
                Rc::new(move |target| window.navigate_to(&target))
            },
            set_filter: {
                let window = self.clone();
                Rc::new(move |kind, show_all| window.set_filter(kind, show_all))
            },
            cog_ready: {
                let window = self.clone();
                Rc::new(move |cog| window.adopt_cog(&cog))
            },
            select_all_ready: {
                let window = self.clone();
                Rc::new(move |check| window.adopt_select_all(&check))
            },
            table: TableHandlers {
                activate: {
                    let window = self.clone();
                    Rc::new(move |target| window.navigate_to(&target))
                },
                sort_changed: {
                    let window = self.clone();
                    let table = table.clone();
                    Rc::new(move |order| window.set_sort_order(&table, order))
                },
                context: {
                    let window = self.clone();
                    Rc::new(move |node, widget, x, y| {
                        window.show_context_menu(&node, &widget, x, y);
                    })
                },
                checked: {
                    let window = self.clone();
                    Rc::new(move |node: &NodeId| {
                        window.imp().checked.borrow().contains(&node.key())
                    })
                },
                toggle: {
                    let window = self.clone();
                    Rc::new(move |node, on| window.set_checked(&node, on))
                },
                resized: {
                    let window = self.clone();
                    let table = table.clone();
                    Rc::new(move |column, width| window.set_column_width(&table, &column, width))
                },
            },
        }
    }

    /// Act on a chosen menu item, confirming first when the offer says to.
    ///
    /// `node` is what was right-clicked, carried explicitly because opening a menu
    /// deliberately does **not** change the selection: acting on something must not
    /// move the panel away from whatever is being looked at.
    fn invoke(&self, node: &NodeId, offer: &Offer) {
        match &offer.confirmation {
            Some(confirmation) => {
                let window = self.clone();
                let offer = offer.clone();
                let node = node.clone();
                self.confirm(confirmation, move || window.dispatch(&node, &offer));
            }
            None => self.dispatch(node, offer),
        }
    }

    /// Ask before anything irreversible. Cancel is the default response, so a stray
    /// Return key cannot destroy anything.
    fn confirm(&self, confirmation: &Confirmation, on_confirm: impl Fn() + 'static) {
        let dialog = adw::AlertDialog::new(Some(&confirmation.heading), Some(&confirmation.body));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("confirm", &confirmation.confirm_label);
        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        // A prune names every object it will remove: "and 14 others" is not a preview.
        if !confirmation.items.is_empty() {
            dialog.set_extra_child(Some(&confirmation_list(&confirmation.items)));
        }

        dialog.connect_response(None, move |_, response| {
            if response == "confirm" {
                on_confirm();
            }
        });

        dialog.present(Some(self));
    }

    /// Send an agreed action to the runtime, or handle it here when it only reads.
    fn dispatch(&self, node: &NodeId, offer: &Offer) {
        let (_, title) = self.action_target(node);

        match offer.action {
            Action::ViewDockerfile => {
                if let NodeId::Image(image_id) = node {
                    self.send(Command::Dockerfile {
                        image_id: image_id.clone(),
                    });
                }
            }
            Action::ViewLogs => {
                if let NodeId::Container(container_id) = node {
                    self.open_logs(container_id, &title);
                }
            }
            Action::BrowseFilesystem => match node {
                NodeId::Container(id) => {
                    self.open_browser(&BrowseTarget::Container(id.clone()), &title);
                }
                NodeId::Image(id) => {
                    self.open_browser(&BrowseTarget::Image(id.clone()), &title);
                }
                _ => {}
            },
            Action::OpenInFileManager => self.open_in_file_manager(node),
            action => {
                let (id, label) = self.action_target(node);
                self.send(Command::Act(Box::new(ActionRequest { action, id, label })));
            }
        }
    }

    /// What a node identifies, as an ID for the daemon and a name for the user. The
    /// prunes apply to the daemon as a whole and carry no ID.
    fn action_target(&self, node: &NodeId) -> (String, String) {
        let snapshot = self.imp().snapshot.borrow();

        match node.clone() {
            NodeId::Container(id) => {
                let label = snapshot
                    .as_ref()
                    .and_then(|snapshot| {
                        snapshot
                            .containers
                            .iter()
                            .find(|container| container.id == id)
                    })
                    .map_or_else(|| format::short_id(&id), format::container_label);
                (id, label)
            }
            NodeId::Image(id) => {
                let label = snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.images.iter().find(|image| image.id == id))
                    .map_or_else(|| format::short_id(&id), format::image_label);
                (id, label)
            }
            _ => (String::new(), "the daemon".to_owned()),
        }
    }

    /// How a table should open: the order the user set this session, or the table's own
    /// default, together with the widths they have dragged its columns to.
    fn table_state(&self, table: Option<&Table>) -> detail_pane::TableState {
        let Some(table) = table else {
            return detail_pane::TableState {
                sort: SortOrder::default(),
                widths: BTreeMap::new(),
            };
        };

        let sort = self
            .imp()
            .sorts
            .borrow()
            .get(table.id)
            .cloned()
            .unwrap_or_else(|| SortOrder::from_default(table));

        let widths = self
            .imp()
            .settings
            .borrow()
            .column_widths
            .get(table.id)
            .cloned()
            .unwrap_or_default();

        detail_pane::TableState { sort, widths }
    }

    /// Widen or narrow a table's view, and remember the choice.
    fn set_filter(&self, kind: detail::FilterKind, show_all: bool) {
        {
            let mut settings = self.imp().settings.borrow_mut();
            let held = match kind {
                detail::FilterKind::StoppedContainers => &mut settings.show_stopped_containers,
                detail::FilterKind::UntaggedImages => &mut settings.show_untagged_images,
            };
            if *held == show_all {
                return;
            }
            *held = show_all;
        }
        self.store_settings();
        self.render_detail();
    }

    /// Remember a re-sort for the rest of the session, so a refresh does not undo it.
    /// The pane is not re-rendered: the view has already applied it, and rebuilding would
    /// discard the selection for no visible gain.
    fn set_sort_order(&self, table: &str, order: SortOrder) {
        self.imp()
            .sorts
            .borrow_mut()
            .insert(table.to_owned(), order);
    }

    /// Remember a column the user dragged. Restoring a width notifies too, which is why
    /// the model reports whether anything actually changed.
    fn set_column_width(&self, table: &str, column: &str, width: i32) {
        let changed = self
            .imp()
            .settings
            .borrow_mut()
            .set_column_width(table, column, width);

        if changed {
            self.store_settings();
        }
    }

    fn store_settings(&self) {
        self.imp().prefs.store(&self.imp().settings.borrow());
    }

    /// Follow a link from the detail pane: select the target in the sidebar, which
    /// re-renders the pane as a side effect, so the tree and the content stay in step.
    fn navigate_to(&self, target: &NodeId) {
        // The target may be collapsed out of view; the tree list model only offers rows
        // for expanded branches, so open the branch it lives in first.
        self.expand_all();

        if !self.select_key(&target.key()) {
            self.toast("That object is no longer present.");
        }
    }

    /// The flattened tree behind the sidebar, which only offers rows for branches that
    /// are currently expanded.
    fn tree_model(&self) -> Option<gtk::TreeListModel> {
        self.imp()
            .selection
            .get()
            .and_then(gtk::SingleSelection::model)
            .and_downcast::<gtk::TreeListModel>()
    }

    fn expand_all(&self) {
        let Some(model) = self.tree_model() else {
            return;
        };

        // Expanding inserts rows, so walk by index and re-read the count each time.
        let mut index = 0;
        while index < model.n_items() {
            if let Some(row) = model.row(index) {
                row.set_expanded(true);
            }
            index += 1;
        }
    }

    /// Select the row carrying a key. False when there is no such row.
    fn select_key(&self, key: &str) -> bool {
        let (Some(selection), Some(model)) = (self.imp().selection.get(), self.tree_model()) else {
            return false;
        };

        for index in 0..model.n_items() {
            let matches = model
                .row(index)
                .and_then(|row| row.item())
                .and_downcast::<TreeNodeObject>()
                .is_some_and(|node| node.key() == key);
            if matches {
                selection.set_selected(index);
                return true;
            }
        }

        false
    }

    /// Borrows are confined here so they cannot outlive the render.
    fn build_page(&self, selected: &NodeId) -> Option<detail::DetailPage> {
        let snapshot = self.imp().snapshot.borrow();
        let snapshot = snapshot.as_ref()?;

        let raw_cache = self.imp().raw.borrow();
        let cx = detail::Context {
            images: &snapshot.images,
            containers: &snapshot.containers,
            layers: &snapshot.layers,
            raw: raw_cache.get(&selected.key()),
            now: now_seconds(),
            offset: chrono::Offset::fix(chrono::Local::now().offset()),
            show_stopped: self.imp().settings.borrow().show_stopped_containers,
            show_untagged: self.imp().settings.borrow().show_untagged_images,
        };

        match selected {
            NodeId::Root => Some(detail::environment(
                &snapshot.environment,
                &snapshot.resolved,
                &cx,
            )),
            NodeId::Images => Some(detail::images(&cx)),
            NodeId::Containers => Some(detail::containers(&cx)),
            NodeId::Image(id) => snapshot
                .images
                .iter()
                .find(|image| &image.id == id)
                .map(|image| detail::image(image, &cx)),
            NodeId::Container(id) => snapshot
                .containers
                .iter()
                .find(|container| &container.id == id)
                .map(|container| detail::container(container, &cx)),
        }
    }

    /// Remember where the user left the divider, alongside whatever else has changed.
    pub fn store_sidebar_width(&self) {
        self.imp().settings.borrow_mut().sidebar_width = self.imp().paned.position();
        self.store_settings();
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
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        content.append(&icon);
        content.append(&label);

        let expander = gtk::TreeExpander::new();
        expander.set_child(Some(&content));

        // Attached per row rather than to the list: the row already knows which object
        // it stands for, so no coordinate-to-row mapping is needed.
        let secondary = gtk::GestureClick::new();
        secondary.set_button(gtk::gdk::BUTTON_SECONDARY);
        secondary.connect_pressed(|gesture, _, x, y| {
            let Some(expander) = gesture.widget().and_downcast::<gtk::TreeExpander>() else {
                return;
            };
            let Some(window) = expander.root().and_downcast::<LaveWindow>() else {
                return;
            };
            // The expander knows its own row, so the object is reachable without
            // mapping coordinates back to a list position.
            let Some(node) = expander
                .list_row()
                .and_then(|row| row.item())
                .and_downcast::<TreeNodeObject>()
                .and_then(|node| NodeId::from_key(&node.key()))
            else {
                return;
            };
            // As in the table: the row's own gesture takes any button, so the press has
            // to be claimed or it selects as well as opening the menu.
            gesture.set_state(gtk::EventSequenceState::Claimed);
            window.show_context_menu(&node, &expander, x, y);
        });
        expander.add_controller(secondary);
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

        icon.set_icon_name(Some(&node.icon()));
        crate::table_view::apply_tone_class(&icon, &node.tone());
        label.set_label(&node.label());
        // The row shows the name alone; the count or state is spoken rather than drawn.
        let spoken = format!("{} {}", node.label(), node.description());
        expander.update_property(&[gtk::accessible::Property::Label(spoken.trim_end())]);
    });

    factory
}

/// One line of a menu, in the terms the widget layer needs: what the core decided,
/// flattened so the same builder serves the row menu and the cog.
#[derive(Debug, Clone)]
pub struct MenuEntry {
    pub label: String,
    pub icon: &'static str,
    pub tone: Tone,
    pub destructive: bool,
}

impl MenuEntry {
    fn from_offer(offer: &Offer) -> Self {
        Self {
            label: offer.label.clone(),
            icon: offer.icon,
            tone: offer.tone(),
            destructive: offer.is_destructive(),
        }
    }

    fn from_bulk(offer: &BulkOffer) -> Self {
        Self {
            label: offer.label.clone(),
            icon: offer.icon,
            tone: offer.tone(),
            destructive: offer.is_destructive(),
        }
    }
}

/// How an open menu is closed, which is not a detail: it decides what the next click does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dismissal {
    /// GTK closes it, by taking a pointer grab. The grab **swallows** the click that
    /// dismisses the menu rather than letting it reach what it landed on, which is right
    /// for a menu hung off a button — the click was aimed at getting rid of the menu.
    Grab,
    /// The window closes it, from a gesture that watches for a press elsewhere. No grab,
    /// so the dismissing click still reaches what it landed on: a secondary click on
    /// another row closes this menu *and* opens that row's, on one click rather than two.
    Watched,
}

/// A menu of buttons in a popover, rather than a `GMenu` in a `GtkPopoverMenu`.
///
/// The model route cannot do what this application asks of a menu:
///
/// * **Icons.** `GtkPopoverMenu` reads the `icon` attribute, but the `GtkModelButton` it
///   builds hides its image whenever the item also has a label — deliberately, following
///   the GNOME guidelines, and not configurable. The icons added in version 4 were never
///   drawn; the widget tree said `visible=false` on every one of them.
/// * **Colour.** A `GMenu` is a model and cannot carry a CSS class, so tinting the stop
///   icon meant walking the built widgets and matching `GtkModelButton` by name, since
///   the type is private to GTK.
/// * **The scrollbar.** `GtkPopoverMenu` wraps its items in a `GtkScrolledWindow` for
///   menus taller than the screen, and that is what appeared down the side of a menu of
///   six items.
///
/// Buttons in a plain box answer all three, and arrow keys still move between them.
fn menu_popover(
    entries: &[MenuEntry],
    dismissal: Dismissal,
    choose: impl Fn(usize) + 'static,
) -> gtk::Popover {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();

    let popover = gtk::Popover::builder()
        .child(&content)
        .has_arrow(false)
        .autohide(dismissal == Dismissal::Grab)
        .position(gtk::PositionType::Bottom)
        .build();
    popover.add_css_class("context-menu");

    if dismissal == Dismissal::Watched {
        // GTK closes an autohiding popover on Escape; this one has to be told.
        let dismiss = gtk::EventControllerKey::new();
        dismiss.connect_key_pressed(glib::clone!(
            #[weak]
            popover,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key == gtk::gdk::Key::Escape {
                    popover.popdown();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        ));
        popover.add_controller(dismiss);
    }

    let choose: Rc<dyn Fn(usize)> = Rc::new(choose);
    let mut previous_destructive = false;

    for (index, entry) in entries.iter().enumerate() {
        // Destructive actions are set apart, so the rule sits between "Restart" and
        // "Remove" rather than anywhere arbitrary.
        if entry.destructive && !previous_destructive && index > 0 {
            content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        }
        previous_destructive = entry.destructive;

        content.append(&menu_button(entry, index, &popover, &choose));
    }

    popover
}

/// One line of the menu: an icon, tinted when the action removes or halts something, and
/// the label that actually says what it does.
fn menu_button(
    entry: &MenuEntry,
    index: usize,
    popover: &gtk::Popover,
    choose: &Rc<dyn Fn(usize)>,
) -> gtk::Button {
    let icon = gtk::Image::from_icon_name(entry.icon);
    if entry.tone == Tone::Bad {
        icon.add_css_class(entry.tone.css_class());
    }

    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    row.append(&icon);
    row.append(
        &gtk::Label::builder()
            .label(&entry.label)
            .xalign(0.0)
            .build(),
    );

    let button = gtk::Button::builder().child(&row).build();
    button.add_css_class("flat");

    let choose = Rc::clone(choose);
    // Weak, or the button would hold the popover that holds the button.
    button.connect_clicked(glib::clone!(
        #[weak]
        popover,
        move |_| {
            popover.popdown();
            choose(index);
        }
    ));

    button
}

/// Take a closed popover out of the widget tree. It is parented to the toast overlay and
/// would otherwise stay a child of it for the life of the window.
fn retire(popover: &gtk::Popover) {
    if popover.parent().is_some() {
        popover.unparent();
    }
}

/// The named objects a destructive confirmation will remove.
fn confirmation_list(items: &[String]) -> gtk::ScrolledWindow {
    let list = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();

    for item in items {
        let label = gtk::Label::builder()
            .label(item)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        label.add_css_class("monospace");
        list.append(&label);
    }

    gtk::ScrolledWindow::builder()
        .child(&list)
        .max_content_height(CONFIRM_LIST_HEIGHT)
        .propagate_natural_height(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build()
}

/// An open log tab. Cloned out of the map before the buffer is touched, so nothing holds
/// a borrow across a call into GTK.
#[derive(Clone)]
pub struct LogView {
    pub buffer: gtk::TextBuffer,
    pub view: gtk::TextView,
    pub scroller: gtk::ScrolledWindow,
    /// Whether the view is following the end of the log.
    ///
    /// Held rather than worked out per batch: a scroll is not applied until the new text
    /// has been laid out, so a view that has just been told to scroll still *reads* as
    /// being partway up. Deciding from that reading, the viewer would give up following
    /// the moment output arrived faster than it could lay out — which is exactly when
    /// following matters.
    pub following: std::rc::Rc<std::cell::Cell<bool>>,
}

/// The mark the viewer scrolls to. Permanent, and with right gravity, so it stays at the
/// end as lines arrive; a mark created and deleted per batch would be gone before the
/// view had laid out the lines it was meant to scroll past.
const END_MARK: &str = "end";

/// Scroll so the last line is at the bottom of the view. Deferred by GTK until the lines
/// in between have been laid out, which is the point of scrolling to a mark.
fn scroll_to_end(view: &gtk::TextView, buffer: &gtk::TextBuffer) {
    let Some(mark) = buffer.mark(END_MARK) else {
        return;
    };
    view.scroll_to_mark(&mark, 0.0, true, 0.0, 1.0);
}

/// The tag marking stderr in the log viewer.
const STDERR_TAG: &str = "stderr";

fn token_tag(token: logs::Token) -> &'static str {
    match token {
        logs::Token::Key => "json-key",
        logs::Token::Text => "json-text",
        logs::Token::Number => "json-number",
        logs::Token::Literal => "json-literal",
        logs::Token::Punctuation => "json-punctuation",
    }
}

/// Tags for the log viewer.
///
/// `GtkTextTag` cannot be styled from CSS, so these colours are chosen here rather than
/// in `style.css`, and follow the theme's light/dark setting because a palette legible
/// on one is washed out or glaring on the other.
fn log_tags() -> Vec<gtk::TextTag> {
    let dark = adw::StyleManager::default().is_dark();

    // Adwaita's palette, the darker shades on light backgrounds and lighter on dark.
    let (key, text, number, literal, punctuation) = if dark {
        ("#78aeed", "#8ff0a4", "#f9f06b", "#dc8add", "#9a9996")
    } else {
        ("#1c71d8", "#26a269", "#c64600", "#9141ac", "#77767b")
    };

    vec![
        gtk::TextTag::builder()
            .name(STDERR_TAG)
            .weight(700)
            .foreground(if dark { "#f66151" } else { "#c01c28" })
            .build(),
        gtk::TextTag::builder()
            .name("json-key")
            .foreground(key)
            .weight(700)
            .build(),
        gtk::TextTag::builder()
            .name("json-text")
            .foreground(text)
            .build(),
        gtk::TextTag::builder()
            .name("json-number")
            .foreground(number)
            .build(),
        gtk::TextTag::builder()
            .name("json-literal")
            .foreground(literal)
            .build(),
        gtk::TextTag::builder()
            .name("json-punctuation")
            .foreground(punctuation)
            .build(),
    ]
}

/// The open file tree's state.
pub struct Browser {
    pub target: BrowseTarget,
    /// One store per indexed directory, filled when its listing arrives.
    pub stores: std::collections::HashMap<String, gtk::gio::ListStore>,
    pub root: gtk::gio::ListStore,
    pub notice: gtk::Label,
}

/// Rows for the file tree: an expander, an icon, the name, and its size or link target.
fn build_fs_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, item| {
        let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let icon = gtk::Image::new();
        let name = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let detail = gtk::Label::builder().xalign(1.0).hexpand(true).build();
        detail.add_css_class("dim-label");
        detail.add_css_class("numeric");

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        content.append(&icon);
        content.append(&name);
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
        let Some(node) = row.item().and_downcast::<crate::fs_node::FsNodeObject>() else {
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
        let Some(name) = icon.next_sibling().and_downcast::<gtk::Label>() else {
            return;
        };
        let Some(detail) = name.next_sibling().and_downcast::<gtk::Label>() else {
            return;
        };

        icon.set_icon_name(Some(&node.icon()));
        name.set_label(&node.name());
        detail.set_label(&node.detail());
    });

    factory
}

/// What an output tab shows. The metadata page is not one of these: it is pinned, and
/// follows the selection rather than belonging to any one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TabKind {
    Logs,
    Dockerfile,
    Files,
}

/// Identifies an output tab. One per object per kind, so two containers' logs can sit
/// side by side.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TabKey {
    pub kind: TabKind,
    pub object_id: String,
}

/// Hand a directory to whatever the desktop uses to browse directories.
fn launch_file_manager(window: &LaveWindow, path: &Path) {
    let file = gtk::gio::File::for_path(path);
    let launcher = gtk::FileLauncher::new(Some(&file));

    launcher.launch(
        Some(window),
        gtk::gio::Cancellable::NONE,
        glib::clone!(
            #[weak]
            window,
            move |result| {
                if let Err(error) = result {
                    // Dismissing the portal's chooser arrives here too, and is not a
                    // failure worth shouting about.
                    if error.matches(gtk::gio::IOErrorEnum::Cancelled) {
                        return;
                    }
                    window.apply_action_outcome(
                        &format!("Mounted, but no file manager would open it: {error}"),
                        true,
                    );
                }
            }
        ),
    );
}

/// Widget-level checks that a display is needed for.
///
/// One test function, deliberately: GTK must be used from the thread that initialised it,
/// and the test harness runs `#[test]` functions on threads of its own.
#[cfg(all(test, feature = "live-gtk"))]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;

    fn entries() -> Vec<MenuEntry> {
        vec![
            MenuEntry {
                label: "Start".to_owned(),
                icon: "media-playback-start-symbolic",
                tone: Tone::Neutral,
                destructive: false,
            },
            MenuEntry {
                label: "View Logs".to_owned(),
                icon: "text-x-generic-symbolic",
                tone: Tone::Neutral,
                destructive: false,
            },
            MenuEntry {
                label: "Remove".to_owned(),
                icon: "user-trash-symbolic",
                tone: Tone::Bad,
                destructive: true,
            },
        ]
    }

    /// The buttons and separators of a built menu, in order.
    fn rows(popover: &gtk::Popover) -> Vec<gtk::Widget> {
        let mut found = Vec::new();
        let content = popover.child().expect("the menu has content");
        let mut child = content.first_child();
        while let Some(node) = child {
            found.push(node.clone());
            child = node.next_sibling();
        }
        found
    }

    #[test]
    fn the_context_menu_behaves_as_a_context_menu() {
        gtk::init().expect(
            "these tests need a display: run them with one, or without --features live-gtk",
        );

        // A row's menu must not autohide.
        //
        // This is the regression. Autohiding takes a pointer grab, and the grab consumes
        // the click that dismisses the menu instead of letting it through — so a secondary
        // click on a second row spends itself closing the first row's menu, and opening the
        // second one costs another click. Dismissal is `setup_menu_dismissal`'s job
        // precisely so that the press it watches still reaches the row underneath.
        let row_menu = menu_popover(&entries(), Dismissal::Watched, |_| {});
        assert!(
            !row_menu.is_autohide(),
            "a row's menu must not grab, or moving to another row's menu costs two clicks"
        );

        // A menu hung off a button is the opposite case: there is no second button to move
        // to, and the click that dismisses it was aimed at nothing else.
        let button_menu = menu_popover(&entries(), Dismissal::Grab, |_| {});
        assert!(
            button_menu.is_autohide(),
            "a button's menu should be dismissed by GTK rather than by hand"
        );

        // Every item shows its icon. `GtkPopoverMenu` could not: a `GtkModelButton` hides
        // its image whenever the item also has a label, so version 4's icons never drew.
        let built = rows(&row_menu);
        let buttons: Vec<gtk::Button> = built
            .iter()
            .filter_map(|row| row.clone().downcast::<gtk::Button>().ok())
            .collect();
        assert_eq!(buttons.len(), entries().len(), "one button per offer");

        for (button, entry) in buttons.iter().zip(entries()) {
            let content = button.child().expect("the button has content");
            let image = content
                .first_child()
                .and_downcast::<gtk::Image>()
                .expect("the button leads with its icon");
            // `get_visible` is the widget's own property; `is_visible` also asks its
            // ancestors, and this menu is not on screen. The property is what a
            // `GtkModelButton` clears, so the property is what has to be checked.
            assert!(image.get_visible(), "{}'s icon is hidden", entry.label);
            assert_eq!(
                image.icon_name().as_deref(),
                Some(entry.icon),
                "{} has the wrong icon",
                entry.label
            );
            assert_eq!(
                image.has_css_class(Tone::Bad.css_class()),
                entry.tone == Tone::Bad,
                "{} is tinted wrongly",
                entry.label
            );
        }

        // Destructive actions are set apart, so the rule falls before Remove rather than
        // anywhere arbitrary.
        let separators: Vec<usize> = built
            .iter()
            .enumerate()
            .filter(|(_, row)| row.is::<gtk::Separator>())
            .map(|(index, _)| index)
            .collect();
        assert_eq!(separators, vec![2], "one rule, immediately before Remove");
    }
}
