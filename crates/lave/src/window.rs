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
use lave_core::model::tabs;
use lave_core::model::tree::{self, NodeId, Tone, TreeNode};

use crate::detail_pane;
use crate::runtime::{ActionRequest, BrowseTarget, Command, Snapshot, StatusView, now_seconds};
use crate::table_view::{SortOrder, TableHandlers};
use crate::tree_node::TreeNodeObject;
use lave_core::model::metrics::StatsIndex;

/// A confirmation lists what it will remove, scrolling past this height rather than
/// growing the dialog off the screen.
const CONFIRM_LIST_HEIGHT: i32 = 220;

/// One table row, in pixels. GTK will not report it before the first layout, so the
/// divider's opening position is estimated; a drag overrides the estimate either way.
const TABLE_ROW_HEIGHT: i32 = 34;
/// Column headings, the filter toggle above them, frame border and margins.
const TABLE_CHROME_HEIGHT: i32 = 116;

/// The tab menu's commands, by action name. The labels are in the template, next to the
/// order they appear in.
const TAB_COMMANDS: [(&str, tabs::Scope); 3] = [
    ("close-tabs-left", tabs::Scope::ToLeft),
    ("close-tabs-right", tabs::Scope::ToRight),
    ("close-tabs-all", tabs::Scope::All),
];

mod imp {
    use std::cell::{Cell, OnceCell, RefCell};
    use std::collections::{HashMap, HashSet};

    use adw::subclass::prelude::*;
    use async_channel::Sender;
    use gtk::glib::Propagation;
    use gtk::prelude::WidgetExt;
    use gtk::{CompositeTemplate, glib};
    use lave_core::model::detail::DetailPage;
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
        /// The open detail tabs, one per object being looked at. The first is the
        /// environment's, which is pinned and never closed.
        pub detail_tabs: RefCell<Vec<super::DetailTab>>,
        /// The tab whose context menu was last opened, which is what its commands
        /// measure from. Not cleared when the menu closes: the item is activated after
        /// that, and it would have nothing left to act on.
        pub menu_tab: RefCell<Option<adw::TabPage>>,
        /// Set while the sidebar is being brought into line with a tab that has come
        /// forward, so that does not read as the user having selected something.
        pub following: Cell<bool>,
        /// Set while a new listing is being spliced into the tree, for the same reason:
        /// the selection model moves as rows come and go, and none of that is a choice.
        pub settling: Cell<bool>,
        /// What the sidebar has selected.
        pub selected: RefCell<Option<NodeId>>,
        /// What the detail tab on screen is currently showing, which is the environment
        /// whenever its own tab is to the front.
        pub viewing: RefCell<Option<NodeId>>,
        /// The page last drawn, and the surface it went into, so a refresh that changes
        /// nothing a person can see does not rebuild the pane under them.
        pub rendered: RefCell<Option<(gtk::Paned, DetailPage)>>,
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

/// One detail tab: what it shows, the tab itself, and the widgets it shows it in.
///
/// Cheap to clone — the fields are all reference-counted handles — which keeps the list
/// of them from being borrowed across a render.
#[derive(Clone)]
pub struct DetailTab {
    pub node: NodeId,
    pub page: adw::TabPage,
    pub surface: detail_pane::Surface,
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

        // Splicing the stores below makes the selection model pick a neighbour whenever
        // the row it was on goes away. That is the model shuffling, not the user
        // choosing, and acting on it would open a tab for whatever it happened to land
        // on; `restore_selection` decides where the selection ends up.
        self.imp().settling.set(true);

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

        self.imp().settling.set(false);

        self.prune_checks(&snapshot);
        self.imp().snapshot.replace(Some(snapshot));
        // In this order: the selection lands somewhere that still exists first, so
        // closing the tabs of objects that have gone cannot be closing the one on screen.
        self.restore_selection();
        self.prune_detail_tabs();
        self.imp().content_stack.set_visible_child_name("detail");
        self.render_detail();
    }

    /// Fresh memory samples for a snapshot that is otherwise unchanged.
    ///
    /// Only the page on screen is redrawn: the tree, the tabs and the checked rows all
    /// describe what exists, and none of that has moved.
    pub fn apply_stats(&self, stats: StatsIndex) {
        let mut borrowed = self.imp().snapshot.borrow_mut();
        let Some(snapshot) = borrowed.as_mut() else {
            return;
        };
        snapshot.stats = stats;
        drop(borrowed);

        self.render_detail();
    }

    /// Raw inspect output for a node, cached so reselecting does not refetch.
    pub fn apply_inspect(&self, id: &NodeId, raw: serde_json::Value) {
        self.imp().raw.borrow_mut().insert(id.key(), raw);
        // Only the page on screen: the inspect that arrives for a page already navigated
        // away from is cached and shown when it is next opened.
        if self.imp().viewing.borrow().as_ref() == Some(id) {
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
        // The description is markup and has no plain-text mode, so a daemon's own
        // wording — which may contain "<" — is escaped rather than parsed.
        self.imp()
            .status_page
            .set_description(Some(&glib::markup_escape_text(&description)));
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

    /// Register the actions the primary menu and the tab menu invoke.
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

        for (name, scope) in TAB_COMMANDS {
            let close = gtk::gio::SimpleAction::new(name, None);
            close.connect_activate(glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |_, _| window.close_tabs(scope)
            ));
            self.add_action(&close);
        }
    }

    /// Close a run of tabs, as the tab menu asks.
    ///
    /// The pages are gathered before any of them goes, since closing one moves every
    /// position after it.
    fn close_tabs(&self, scope: tabs::Scope) {
        let view = self.imp().tab_view.clone();
        let (count, pinned) = self.tab_counts();

        let doomed: Vec<adw::TabPage> = tabs::closing(scope, count, self.menu_subject(), pinned)
            .into_iter()
            .filter_map(|position| i32::try_from(position).ok())
            .map(|position| view.nth_page(position))
            .collect();

        for page in doomed {
            view.close_page(&page);
        }
    }

    /// Grey out a tab command that would close nothing, so the menu says what is possible
    /// rather than offering three items of which two do nothing.
    fn update_tab_actions(&self, page: Option<&adw::TabPage>) {
        if let Some(page) = page {
            self.imp().menu_tab.replace(Some(page.clone()));
        }

        let (count, pinned) = self.tab_counts();
        let subject = self.menu_subject();

        for (name, scope) in TAB_COMMANDS {
            if let Some(found) = self.lookup_action(name)
                && let Ok(simple) = found.downcast::<gtk::gio::SimpleAction>()
            {
                simple.set_enabled(tabs::is_offered(scope, count, subject, pinned));
            }
        }
    }

    /// How many tabs there are, and how many of those are pinned.
    fn tab_counts(&self) -> (usize, usize) {
        let view = &self.imp().tab_view;
        (
            usize::try_from(view.n_pages()).unwrap_or(0),
            usize::try_from(view.n_pinned_pages()).unwrap_or(0),
        )
    }

    /// Where the tab these commands are measured from sits: the one the menu was opened
    /// on, or the one in view.
    ///
    /// Found by walking the bar rather than by asking `page_position`, which requires the
    /// page to still be in the view — and the tab a menu was last opened on need not be.
    fn menu_subject(&self) -> usize {
        let view = self.imp().tab_view.clone();
        let Some(wanted) = self
            .imp()
            .menu_tab
            .borrow()
            .clone()
            .or_else(|| view.selected_page())
        else {
            return 0;
        };

        (0..view.n_pages())
            .position(|position| view.nth_page(position) == wanted)
            .unwrap_or(0)
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

        // Redraws every row's tick, which is all that has changed.
        self.update_bulk_controls();
    }

    /// Bring the cog and the select-all control into line with what is checked.
    ///
    /// The cog is insensitive until something is checked, as there is then nothing for it
    /// to act on; select-all shows the mixed state when only some rows are ticked.
    fn update_bulk_controls(&self) {
        let tally = self.tally();

        // The ticks live here rather than in the rows, so the rows are told to read them
        // again. Told, not rebuilt: a redraw would cost the reader their place.
        if let Some(tab) = self.viewed() {
            tab.surface.refresh_checks();
        }

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

    /// Build the environment's tab and bind the tab bar to the view.
    ///
    /// Every detail tab carries a set of widgets of its own: they are all open at once,
    /// and a widget has one parent, so there is nothing to share.
    fn setup_tabs(&self) {
        let view = self.imp().tab_view.clone();
        self.imp().tab_bar.set_view(Some(&view));

        // Pinned: it is where the application starts and there is nothing sensible to
        // close it to. A pinned tab is drawn as its icon alone, so it carries a tooltip.
        let surface = self.adopt_surface();
        let page = view.append_pinned(&surface.paned);
        page.set_title("Docker");
        page.set_tooltip("Docker");
        page.set_icon(Some(&self.node_icon(&NodeId::Root)));

        self.imp().detail_tabs.borrow_mut().push(DetailTab {
            node: NodeId::Root,
            page,
            surface,
        });

        // Whichever detail tab is brought forward draws itself: only one is on screen,
        // so only one is worth rendering.
        view.connect_selected_page_notify(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_| window.follow_tab()
        ));

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

        // Emitted as a tab's context menu opens, carrying the tab it was opened on, and
        // again with nothing when it closes. The one moment its commands can be measured.
        view.connect_setup_menu(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_, page| window.update_tab_actions(page)
        ));
    }

    /// A fresh set of detail widgets, watching its divider.
    ///
    /// A drag of that divider sticks until the window closes; a refresh must not silently
    /// undo it, and neither must moving to another tab.
    fn adopt_surface(&self) -> detail_pane::Surface {
        let surface = detail_pane::Surface::new();
        surface.paned.connect_position_notify(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |paned| {
                if !window.imp().positioning.get() {
                    window.imp().lead_position.set(Some(paned.position()));
                }
            }
        ));
        surface
    }

    /// The tab showing `node`, opened if there is not one yet.
    ///
    /// Tabs accumulate rather than replacing one another, as the log and file tabs do:
    /// looking at a second container is not a reason to lose the first one's page. Asking
    /// for one already open brings it forward instead of stacking a duplicate.
    fn detail_tab(&self, node: &NodeId) -> adw::TabPage {
        if let Some(tab) = self.detail_tab_for(node) {
            return tab.page;
        }

        let surface = self.adopt_surface();
        let page = self.imp().tab_view.append(&surface.paned);
        page.set_live_thumbnail(true);

        self.imp().detail_tabs.borrow_mut().push(DetailTab {
            node: node.clone(),
            page: page.clone(),
            surface,
        });

        page
    }

    /// The open detail tab for a node, if there is one.
    fn detail_tab_for(&self, node: &NodeId) -> Option<DetailTab> {
        self.imp()
            .detail_tabs
            .borrow()
            .iter()
            .find(|tab| tab.node == *node)
            .cloned()
    }

    /// Bring the tab for what the sidebar has selected forward, opening it if need be.
    fn show_selection(&self) {
        let view = self.imp().tab_view.clone();
        let before = view.selected_page();

        let page = self.detail_tab(&self.selected());
        view.set_selected_page(&page);

        // Bringing a different tab forward renders it through the notify; asking for the
        // one already showing does not, so that case renders here.
        if view.selected_page() == before {
            self.render_detail();
        }
    }

    /// Follow the tab that has just come forward: the sidebar moves to whatever it shows,
    /// so the two never disagree about what is being looked at.
    fn follow_tab(&self) {
        let Some(tab) = self.viewed() else {
            // An output tab, which draws itself and stands for the object its own title
            // names; moving the sidebar for it would be presumptuous.
            return;
        };

        self.imp().following.set(true);
        self.imp().selected.replace(Some(tab.node.clone()));
        if !self.select_key(&tab.node.key()) {
            // The branch it lives in has been collapsed since it was opened.
            self.expand_all();
            self.select_key(&tab.node.key());
        }
        self.imp().following.set(false);

        self.render_detail();
    }

    /// Close the tab showing a node, which is what an object leaving the daemon does to
    /// its page.
    fn close_detail_tab(&self, node: &NodeId) {
        if let Some(tab) = self.detail_tab_for(node) {
            self.imp().tab_view.close_page(&tab.page);
        }
    }

    /// Drop the tabs of objects that are no longer there. Their pages have nothing left
    /// to show and their actions nothing left to act on.
    fn prune_detail_tabs(&self) {
        let borrowed = self.imp().snapshot.borrow();
        let Some(snapshot) = borrowed.as_ref() else {
            return;
        };

        let gone: Vec<NodeId> = self
            .imp()
            .detail_tabs
            .borrow()
            .iter()
            .map(|tab| tab.node.clone())
            .filter(|node| match node {
                NodeId::Container(id) => !snapshot.containers.iter().any(|object| object.id == *id),
                NodeId::Image(id) => !snapshot.images.iter().any(|object| object.id == *id),
                // The standing nodes are always there, whatever they hold.
                NodeId::Root | NodeId::Images | NodeId::Containers => false,
            })
            .collect();
        drop(borrowed);

        for node in &gone {
            self.close_detail_tab(node);
        }
    }

    /// Release whatever a closing tab was holding open.
    fn on_tab_closed(&self, page: &adw::TabPage) {
        // A detail tab holds nothing but its own widgets. Which tab takes its place is
        // the tab view's decision, and `follow_tab` moves the sidebar to match.
        let was_detail = {
            let mut tabs = self.imp().detail_tabs.borrow_mut();
            let before = tabs.len();
            tabs.retain(|tab| tab.page != *page);
            tabs.len() != before
        };
        if was_detail {
            return;
        }

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
            .use_markup(false)
            .timeout(if failed { 8 } else { 3 })
            .build();
        self.imp().toasts.add_toast(toast);
    }

    fn toast(&self, message: &str) {
        if *self.imp().last_toast.borrow() == message {
            return;
        }
        self.imp().last_toast.replace(message.to_owned());
        // Plain text: a daemon's message is not markup, and one containing "<" would be
        // dropped rather than shown.
        let toast = adw::Toast::builder()
            .title(message)
            .use_markup(false)
            .build();
        self.imp().toasts.add_toast(toast);
    }

    fn selected(&self) -> NodeId {
        self.imp().selected.borrow().clone().unwrap_or(NodeId::Root)
    }

    fn on_selection_changed(&self, selection: &gtk::SingleSelection) {
        // A refresh is rebuilding the tree; where the selection lands in the meantime
        // says nothing about what the user is looking at.
        if self.imp().settling.get() {
            return;
        }

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

        // A tab coming forward moves the sidebar to match; opening its own tab again on
        // the way back would be going round in circles.
        if !self.imp().following.get() {
            self.show_selection();
        }
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

    /// The detail tab on screen. `None` while an output tab is to the front, which draws
    /// itself.
    fn viewed(&self) -> Option<DetailTab> {
        let showing = self.imp().tab_view.selected_page()?;
        self.imp()
            .detail_tabs
            .borrow()
            .iter()
            .find(|tab| tab.page == showing)
            .cloned()
    }

    /// Draw whichever detail tab is on screen.
    fn render_detail(&self) {
        let Some(DetailTab {
            node: selected,
            page: tab,
            surface,
        }) = self.viewed()
        else {
            return;
        };

        // Before the first listing there is nothing to draw and nothing is wrong. After
        // one, a page that cannot be built is an object that has gone, and its tab has
        // nothing left to show.
        let Some(page) = self.build_page(&selected) else {
            if self.imp().snapshot.borrow().is_some() {
                self.close_detail_tab(&selected);
            }
            return;
        };

        // The stats timer redraws on a schedule of its own, and most of its ticks change
        // nothing a person can see. Rebuilding the pane under one of those would cost the
        // user their scroll position for no gain, so an identical page is left alone.
        // Keyed by the surface as well as the page: a tab just opened has an empty one,
        // whatever it is about to be given.
        let unchanged = self
            .imp()
            .rendered
            .borrow()
            .as_ref()
            .is_some_and(|(into, before)| *into == surface.paned && *before == page);
        if unchanged {
            return;
        }
        self.imp()
            .rendered
            .replace(Some((surface.paned.clone(), page.clone())));

        tab.set_title(&page.title);
        tab.set_tooltip(&page.title);
        tab.set_icon(Some(&self.node_icon(&selected)));

        self.imp().window_title.set_title(&page.title);
        self.imp()
            .window_title
            .set_subtitle(page.subtitle.as_deref().unwrap_or_default());

        // Ticks made on one page have no business acting from another, and moving to
        // another detail tab is a change of page like any other.
        if self.imp().viewing.replace(Some(selected.clone())).as_ref() != Some(&selected) {
            self.clear_checks();
        }

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

        let handlers = self.handlers(&page, &selected);
        let state = self.table_state(page.table.as_ref());
        detail_pane::render(&surface, &page, &state, &handlers);
        self.position_divider(&surface.paned, page.table_filter.as_ref());

        let inspected = self.imp().raw.borrow().contains_key(&selected.key());
        if matches!(selected, NodeId::Image(_) | NodeId::Container(_)) && !inspected {
            self.send(Command::Inspect(selected));
        }
    }

    /// Give the leading table the height its running containers ask for, unless the user
    /// has already dragged the divider somewhere of their own choosing.
    fn position_divider(&self, paned: &gtk::Paned, filter: Option<&detail::TableFilter>) {
        let Some(filter) = filter else {
            return;
        };

        let wanted = self.imp().lead_position.get().unwrap_or_else(|| {
            let rows = i32::try_from(filter.visible_rows).unwrap_or(i32::MAX);
            TABLE_CHROME_HEIGHT + rows.saturating_mul(TABLE_ROW_HEIGHT)
        });

        self.imp().positioning.set(true);
        paned.set_position(wanted);
        self.imp().positioning.set(false);
    }

    /// The callbacks the detail pane needs. Rebuilt per render, since the widgets it
    /// attaches them to are rebuilt too, and scoped to the page: the sort and the column
    /// widths are stored against its table by name, and the action strip acts on the
    /// object the page describes.
    fn handlers(&self, page: &detail::DetailPage, node: &NodeId) -> detail_pane::Handlers {
        let table = page.table.as_ref().map_or("", |table| table.id).to_owned();

        detail_pane::Handlers {
            act: {
                let window = self.clone();
                let node = node.clone();
                let offers = page.actions.clone();
                Rc::new(move |index: usize| {
                    if let Some(offer) = offers.get(index) {
                        window.invoke(&node, offer);
                    }
                })
            },
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
            stats: &snapshot.stats,
            disk: &snapshot.disk,
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
/// Bring a branch of the sidebar into line with a fresh listing.
///
/// The objects are kept and updated wherever the listing still names the same things in
/// the same order. Replacing them would take the row widgets with them, and the reader
/// would lose the place they had scrolled to, the row they had clicked, and the focus
/// that went with it — every few seconds, for as long as the daemon keeps talking.
fn fill(store: &gtk::gio::ListStore, nodes: &[TreeNode]) {
    let held: Vec<TreeNodeObject> = store.iter::<TreeNodeObject>().flatten().collect();

    let same_objects = held.len() == nodes.len()
        && held
            .iter()
            .zip(nodes)
            .all(|(object, node)| object.key() == node.id.key());

    if same_objects {
        for (object, node) in held.iter().zip(nodes) {
            object.apply(node);
        }
        return;
    }

    // Something has come or gone, so there is no row-for-row correspondence to keep.
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

    // A node is updated in place rather than replaced on a refresh, so the row follows
    // its node's properties rather than being drawn once at bind and left.
    crate::list_rows::follow(
        &factory,
        "notify",
        |item| {
            item.item()
                .and_downcast::<gtk::TreeListRow>()
                .and_then(|row| row.item())
                .and_downcast::<TreeNodeObject>()
        },
        draw_tree_row,
    );

    factory
}

/// Draw one sidebar row from the node its list item currently holds.
fn draw_tree_row(item: &gtk::ListItem) {
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
/// One `#[test]` function, deliberately: GTK must be used from the thread that
/// initialised it, and the test harness runs each `#[test]` on a thread of its own. The
/// checks themselves are named functions called from it.
#[cfg(all(test, feature = "live-gtk"))]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use std::cell::Cell;
    use std::sync::Mutex;

    use lave_core::engine::{ContainerState, ContainerSummary};
    use lave_core::model::relations::LayerIndex;

    use super::*;

    /// Every message `GLib` logged since the writer was installed.
    ///
    /// A markup parse failure is reported by warning alone: the label recovers as soon as
    /// `use-markup` is turned off after it, so the widget's own state says nothing about
    /// whether the value was ever handed to the parser. The log is the only witness.
    static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// GTK logs through the structured API, which goes to the writer and not to the
    /// handler set by `g_log_set_default_handler`. The messages are still written out.
    fn record_logging() {
        glib::log_set_writer_func(|level, fields| {
            for field in fields {
                if field.key() == "MESSAGE"
                    && let Some(message) = field.value_str()
                {
                    LOGGED
                        .lock()
                        .expect("the log is not held across a panic")
                        .push(message.to_owned());
                }
            }
            glib::log_writer_default(level, fields)
        });
    }

    /// What has been logged so far, oldest first.
    fn logged() -> Vec<String> {
        LOGGED
            .lock()
            .expect("the log is not held across a panic")
            .clone()
    }

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
    fn the_widget_layer_behaves_as_the_iterations_decided() {
        adw::init().expect(
            "these tests need a display: run them with one, or without --features live-gtk",
        );
        record_logging();

        the_context_menu_behaves_as_a_context_menu();
        an_object_page_leads_with_its_actions();
        a_listing_page_states_its_memory_total_beside_the_table();
        a_row_shows_its_value_verbatim_rather_than_as_markup();
        let window = detail_pages_get_a_tab_each_and_keep_it();
        the_tab_menu_offers_only_what_it_can_close(&window);
        a_sample_that_says_nothing_new_does_not_rebuild_the_pane(&window);
        the_containers_panel_keeps_its_place_across_a_refresh(&window);
        a_click_survives_a_refresh(&window);
        checking_every_row_shows_the_ticks(&window);
        a_table_sorted_by_a_live_column_reorders_as_the_figures_move(&window);
        the_sidebar_keeps_its_place_across_a_refresh(&window);
        a_containers_page_keeps_its_place_across_a_refresh(&window);
        a_selected_value_survives_a_refresh(&window);
        a_value_that_goes_away_does_not_take_the_page_with_it(&window);
    }

    fn the_context_menu_behaves_as_a_context_menu() {
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

    /// Handlers that record which action was chosen and do nothing else.
    fn recording(chosen: &Rc<Cell<Option<usize>>>) -> detail_pane::Handlers {
        detail_pane::Handlers {
            navigate: Rc::new(|_| {}),
            set_filter: Rc::new(|_, _| {}),
            cog_ready: Rc::new(|_| {}),
            select_all_ready: Rc::new(|_| {}),
            act: {
                let chosen = Rc::clone(chosen);
                Rc::new(move |index| chosen.set(Some(index)))
            },
            table: TableHandlers {
                activate: Rc::new(|_| {}),
                sort_changed: Rc::new(|_| {}),
                context: Rc::new(|_, _, _, _| {}),
                checked: Rc::new(|_| false),
                toggle: Rc::new(|_, _| {}),
                resized: Rc::new(|_, _| {}),
            },
        }
    }

    /// The buttons of an action strip, in order. Each sits in a `GtkFlowBoxChild`.
    fn strip_buttons(bar: &gtk::FlowBox) -> Vec<gtk::Button> {
        let mut found = Vec::new();
        let mut child = bar.first_child();
        while let Some(node) = child {
            if let Some(button) = node.first_child().and_downcast::<gtk::Button>() {
                found.push(button);
            }
            child = node.next_sibling();
        }
        found
    }

    fn an_object_page_leads_with_its_actions() {
        let container = ContainerSummary {
            id: "abc123".to_owned(),
            names: vec!["web".to_owned()],
            state: ContainerState::Running,
            ..ContainerSummary::default()
        };
        let containers = [container.clone()];
        let layers = LayerIndex::new();
        let samples = StatsIndex::new();
        let disk = lave_core::engine::DiskUsage::default();
        let cx = detail::Context {
            images: &[],
            containers: &containers,
            layers: &layers,
            stats: &samples,
            disk: &disk,
            raw: None,
            now: 0,
            offset: chrono::FixedOffset::east_opt(0).expect("UTC is a valid offset"),
            show_stopped: false,
            show_untagged: false,
        };

        let page = detail::container(&container, &cx);
        assert!(
            !page.actions.is_empty(),
            "there is something to do to a running container"
        );

        let chosen: Rc<Cell<Option<usize>>> = Rc::new(Cell::new(None));
        let surface = detail_pane::Surface::new();
        let state = detail_pane::TableState {
            sort: SortOrder::default(),
            widths: BTreeMap::new(),
        };
        detail_pane::render(&surface, &page, &state, &recording(&chosen));

        // The strip comes first, above the groups: the point of it is not having to go
        // back to the table to act on what is being looked at.
        let bar = surface
            .body
            .first_child()
            .and_downcast::<adw::Clamp>()
            .and_then(|clamp| clamp.child())
            .and_downcast::<gtk::FlowBox>()
            .expect("an object's page leads with its action strip");

        let buttons = strip_buttons(&bar);
        assert_eq!(
            buttons.len(),
            page.actions.len(),
            "one button per offer, the same offers the row menu makes"
        );

        buttons
            .first()
            .expect("the strip has buttons")
            .emit_clicked();
        assert_eq!(
            chosen.get(),
            Some(0),
            "a button acts on the offer it was built from"
        );
    }

    /// The first descendant of a given type that answers a question, depth-first.
    fn descendant<T: IsA<gtk::Widget>>(
        root: &impl IsA<gtk::Widget>,
        wanted: impl Fn(&T) -> bool,
    ) -> Option<T> {
        let mut stack = vec![root.clone().upcast::<gtk::Widget>()];

        while let Some(node) = stack.pop() {
            if let Ok(found) = node.clone().downcast::<T>()
                && wanted(&found)
            {
                return Some(found);
            }
            let mut child = node.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                stack.push(widget);
            }
        }

        None
    }

    /// One running container, sampled, as a page can be built from.
    fn sampled(usage: i64, limit: i64) -> (Vec<ContainerSummary>, StatsIndex) {
        let container = ContainerSummary {
            id: "abc123".to_owned(),
            names: vec!["web".to_owned()],
            state: ContainerState::Running,
            ..ContainerSummary::default()
        };
        let mut samples = StatsIndex::new();
        samples.insert(lave_core::engine::ContainerStats {
            id: container.id.clone(),
            memory_usage: usage,
            memory_limit: limit,
        });
        (vec![container], samples)
    }

    fn rendered(page: &detail::DetailPage) -> detail_pane::Surface {
        let surface = detail_pane::Surface::new();
        let state = detail_pane::TableState {
            sort: SortOrder::default(),
            widths: BTreeMap::new(),
        };
        detail_pane::render(
            &surface,
            page,
            &state,
            &recording(&Rc::new(Cell::new(None))),
        );
        surface
    }

    fn a_listing_page_states_its_memory_total_beside_the_table() {
        let (containers, samples) = sampled(700_000_000, 8_000_000_000);
        let layers = LayerIndex::new();
        let disk = lave_core::engine::DiskUsage::default();
        let cx = detail::Context {
            images: &[],
            containers: &containers,
            layers: &layers,
            stats: &samples,
            disk: &disk,
            raw: None,
            now: 0,
            offset: chrono::FixedOffset::east_opt(0).expect("UTC is a valid offset"),
            show_stopped: false,
            show_untagged: false,
        };

        let surface = rendered(&detail::containers(&cx));

        // In the strip above the table, so the total is read with the rows it sums
        // rather than after scrolling past them.
        let label = descendant::<gtk::Label>(&surface.lead, |label| {
            label.text().starts_with("Memory in use")
        })
        .expect("the containers page totals its memory above the table");

        assert_eq!(label.text(), "Memory in use: 700.0 MB in 1 container");
    }

    fn a_row_shows_its_value_verbatim_rather_than_as_markup() {
        // The regression: a value is plain text, but a row parses it as Pango markup by
        // default, and "<0.1%" is not markup — GTK dropped the whole label.
        let (containers, samples) = sampled(2_000_000, 8_000_000_000);
        let layers = LayerIndex::new();
        let disk = lave_core::engine::DiskUsage::default();
        let cx = detail::Context {
            images: &[],
            containers: &containers,
            layers: &layers,
            stats: &samples,
            disk: &disk,
            raw: None,
            now: 0,
            offset: chrono::FixedOffset::east_opt(0).expect("UTC is a valid offset"),
            show_stopped: false,
            show_untagged: false,
        };

        let page = detail::container(&containers[0], &cx);
        assert_eq!(
            page.value("Memory", "In use"),
            Some("2.0 MB of 8.0 GB (<0.1%)"),
            "the value this test is about"
        );

        let before = logged().len();
        let surface = rendered(&page);
        let row = descendant::<adw::ActionRow>(&surface.body, |row| row.title() == "In use")
            .expect("the memory row is on the page");

        assert!(!row.uses_markup(), "a row's text is not markup");
        assert_eq!(row.subtitle().as_deref(), Some("2.0 MB of 8.0 GB (<0.1%)"));

        // Turning markup off after the value is set is too late: the row parses each
        // value as it arrives, so the parser must be off before the value, not after.
        let complaints: Vec<String> = logged()
            .split_off(before)
            .into_iter()
            .filter(|message| message.contains("markup"))
            .collect();
        assert!(
            complaints.is_empty(),
            "rendering a row must provoke no markup complaint: {complaints:?}"
        );
    }

    fn detail_pages_get_a_tab_each_and_keep_it() -> LaveWindow {
        // Opening a page for a container must take nothing away: not the daemon's page,
        // which version 5 replaced, and not the previous container's, which the first cut
        // of version 6 replaced.
        gtk::gio::resources_register_include!("lave.gresource")
            .expect("the compiled-in resource bundle should load");

        let app = adw::Application::builder()
            .application_id("com.paperstack.LaveStation.Tests")
            .build();
        app.register(gtk::gio::Cancellable::NONE)
            .expect("the application registers");

        let (commands, _receiver) = async_channel::unbounded();
        let window = LaveWindow::new(&app, commands);
        let view = window.imp().tab_view.clone();

        let environment = window
            .detail_tab_for(&NodeId::Root)
            .expect("the environment has a tab of its own")
            .page;
        assert_eq!(view.n_pages(), 1);
        assert!(
            environment.is_pinned(),
            "the environment's tab cannot be closed"
        );

        let first = NodeId::Container("one".to_owned());
        let second = NodeId::Container("two".to_owned());
        let one = window.detail_tab(&first);
        let two = window.detail_tab(&second);

        assert_eq!(view.n_pages(), 3, "a tab each, and the environment's kept");
        assert_eq!(
            view.nth_page(0),
            environment,
            "the environment stays where it was"
        );
        assert_eq!(view.nth_page(1), one);
        assert_eq!(view.nth_page(2), two);
        assert_eq!(
            window.detail_tab(&first),
            one,
            "asking again brings the same tab forward rather than stacking another"
        );

        // An object that leaves the daemon takes its page with it, and only its own.
        window.close_detail_tab(&first);
        assert_eq!(view.n_pages(), 2);
        assert_eq!(view.nth_page(0), environment);
        assert_eq!(view.nth_page(1), two);
        assert!(window.detail_tab_for(&first).is_none());

        window
    }

    /// Whether a tab command is offered right now.
    fn offered(window: &LaveWindow, name: &str) -> bool {
        window
            .lookup_action(name)
            .expect("the tab commands are registered")
            .is_enabled()
    }

    fn the_tab_menu_offers_only_what_it_can_close(window: &LaveWindow) {
        let view = window.imp().tab_view.clone();
        window.detail_tab(&NodeId::Container("three".to_owned()));
        window.detail_tab(&NodeId::Container("four".to_owned()));

        // The leftmost tab that can be closed: only the pinned one is to its left.
        let leftmost = view.nth_page(view.n_pinned_pages());
        window.update_tab_actions(Some(&leftmost));
        assert!(
            !offered(window, "close-tabs-left"),
            "the environment's tab is pinned and is not going anywhere"
        );
        assert!(offered(window, "close-tabs-right"));
        assert!(offered(window, "close-tabs-all"));

        let last = view.nth_page(view.n_pages() - 1);
        window.update_tab_actions(Some(&last));
        assert!(offered(window, "close-tabs-left"));
        assert!(
            !offered(window, "close-tabs-right"),
            "nothing lies to the right of the last tab"
        );

        // And the commands close what they name, measured from the tab the menu was
        // opened on rather than the one in view.
        window.close_tabs(tabs::Scope::ToLeft);
        assert_eq!(view.n_pages(), 2, "the pinned tab, and the menu's own");
        assert_eq!(view.nth_page(1), last);

        window.close_tabs(tabs::Scope::All);
        assert_eq!(view.n_pages(), 1, "closing them all spares the pinned one");
        assert!(view.nth_page(0).is_pinned());

        // Opening the menu on the tab that is left offers none of the three, since none
        // of them would close anything.
        window.update_tab_actions(Some(&view.nth_page(0)));
        for (name, scope) in TAB_COMMANDS {
            assert!(
                !offered(window, name),
                "{} has nothing to close",
                scope.label()
            );
        }
    }

    /// Let GTK get on with it: events, frame ticks and the idles they queue, until the
    /// widgets have been laid out. Bounded, so a test cannot hang.
    fn settle() {
        let context = glib::MainContext::default();
        for _ in 0..2_000 {
            while context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// A listing with enough rows to scroll, sampled so a refresh can change something.
    fn snapshot_of(count: usize, usage: i64) -> Snapshot {
        let mut containers = Vec::new();
        let mut stats = StatsIndex::new();
        for index in 0..count {
            let id = format!("c{index:03}");
            containers.push(ContainerSummary {
                id: id.clone(),
                names: vec![format!("worker-{index:03}")],
                image: "nginx:1.27".to_owned(),
                state: ContainerState::Running,
                ..ContainerSummary::default()
            });
            stats.insert(lave_core::engine::ContainerStats {
                id,
                memory_usage: usage,
                memory_limit: 8_000_000_000,
            });
        }

        Snapshot {
            resolved: lave_core::endpoint::Resolved {
                endpoint: lave_core::endpoint::Endpoint::Unix("/var/run/docker.sock".into()),
                source: lave_core::endpoint::EndpointSource::RootfulSocket,
            },
            environment: lave_core::engine::EnvironmentSummary::default(),
            images: Vec::new(),
            containers,
            layers: LayerIndex::new(),
            stats,
            disk: lave_core::engine::DiskUsage::default(),
        }
    }

    /// The real thing: a laid-out window, scrolled by hand, refreshed as the daemon
    /// refreshes it. Where the reader was must still be under them afterwards.
    #[allow(clippy::float_cmp)]
    fn the_containers_panel_keeps_its_place_across_a_refresh(window: &LaveWindow) {
        window.apply_snapshot(snapshot_of(60, 1_000_000));

        let listing = NodeId::Containers;
        let tab = window.detail_tab(&listing);
        window.imp().tab_view.set_selected_page(&tab);
        window.set_default_size(1000, 500);
        window.present();
        window.render_detail();
        settle();

        let surface = window
            .detail_tab_for(&listing)
            .expect("the containers listing has a tab")
            .surface;
        let scroller = descendant::<gtk::ScrolledWindow>(&surface.lead, |_| true)
            .expect("the listing's table scrolls");
        let adjustment = scroller.vadjustment();
        assert!(
            adjustment.upper() > adjustment.page_size() + 200.0,
            "sixty rows must be taller than the pane, or there is nothing to scroll: \
             upper {} page {}",
            adjustment.upper(),
            adjustment.page_size()
        );

        adjustment.set_value(200.0);
        settle();
        assert_eq!(adjustment.value(), 200.0, "the reader scrolled down");

        window.apply_stats(snapshot_of(60, 900_000_000).stats);
        settle();

        let redrawn = descendant::<gtk::ScrolledWindow>(&surface.lead, |_| true)
            .expect("the listing still scrolls");
        assert_eq!(
            redrawn.vadjustment().value(),
            200.0,
            "a sample must not throw the reader back to the top"
        );

        // The listing is redrawn by the whole snapshot too, which is what actually
        // arrives from the daemon every few seconds.
        window.apply_snapshot(snapshot_of(60, 500_000_000));
        settle();

        let redrawn = descendant::<gtk::ScrolledWindow>(&surface.lead, |_| true)
            .expect("the listing still scrolls");
        assert_eq!(
            redrawn.vadjustment().value(),
            200.0,
            "a snapshot must not throw the reader back to the top either"
        );

        // Two redraws before GTK has had a frame to lay the first one out. A snapshot
        // and a sample landing in the same turn do exactly this, and the second redraw
        // must not take the empty scroller the first just built for the reader's place.
        window.apply_stats(snapshot_of(60, 700_000_000).stats);
        window.apply_stats(snapshot_of(60, 300_000_000).stats);
        settle();

        let redrawn = descendant::<gtk::ScrolledWindow>(&surface.lead, |_| true)
            .expect("the listing still scrolls");
        assert_eq!(
            redrawn.vadjustment().value(),
            200.0,
            "two redraws in a turn must not lose the place either"
        );
    }

    /// A row of a list that the reader can see, and so could have clicked.
    ///
    /// Measured in the list's own coordinates, so "visible" means inside the window the
    /// adjustment is showing rather than merely realised: a list keeps widgets for rows
    /// a little beyond its edges, and clicking one of those is not something a reader
    /// can do.
    fn visible_row(list: &gtk::Widget, adjustment: &gtk::Adjustment) -> Option<gtk::Widget> {
        let mut rows = Vec::new();
        let mut stack = vec![list.clone()];

        while let Some(node) = stack.pop() {
            let named = node.type_().name();
            let visible = || {
                node.compute_bounds(list).is_some_and(|bounds| {
                    f64::from(bounds.y()) >= adjustment.value()
                        && f64::from(bounds.y() + bounds.height())
                            <= adjustment.value() + adjustment.page_size()
                })
            };
            if (named.contains("ColumnViewRow") || named.contains("ListItem")) && visible() {
                rows.push(node.clone());
            }
            let mut child = node.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                stack.push(widget);
            }
        }

        rows.get(rows.len() / 2).cloned()
    }

    /// The regression the redraw guard did not cover: scrolling was kept, but clicking a
    /// row and then waiting for a refresh threw the reader somewhere else entirely.
    ///
    /// A click focuses that row's widget. Rebuilding the view destroyed the focused row,
    /// and the focus GTK then found put the list wherever that widget was — which is why
    /// the view, its model and its rows are all kept rather than rebuilt.
    #[allow(clippy::float_cmp)]
    fn a_click_survives_a_refresh(window: &LaveWindow) {
        window.apply_snapshot(snapshot_of(60, 1_000_000));

        let listing = NodeId::Containers;
        let tab = window.detail_tab(&listing);
        window.imp().tab_view.set_selected_page(&tab);
        window.set_default_size(1000, 500);
        window.present();
        window.render_detail();
        settle();

        let surface = window
            .detail_tab_for(&listing)
            .expect("the containers listing has a tab")
            .surface;
        let scroller = descendant::<gtk::ScrolledWindow>(&surface.lead, |_| true)
            .expect("the listing's table scrolls");
        let adjustment = scroller.vadjustment();
        adjustment.set_value(200.0);
        settle();

        // What GTK's own click gesture does to the row it lands on.
        let view = descendant::<gtk::ColumnView>(&surface.lead, |_| true).expect("a table");
        let clicked = visible_row(&view.clone().upcast(), &adjustment).expect("rows are on screen");
        clicked.grab_focus();
        let _ = clicked.activate_action("listitem.select", Some(&(false, false).to_variant()));
        settle();
        assert_eq!(
            adjustment.value(),
            200.0,
            "clicking a row the reader can see does not move the list"
        );

        window.apply_stats(snapshot_of(60, 800_000_000).stats);
        settle();
        assert_eq!(
            adjustment.value(),
            200.0,
            "a refresh after a click must not move the list either"
        );
        assert_eq!(
            gtk::prelude::RootExt::focus(window).as_ref(),
            Some(&clicked),
            "the row that was clicked still has the focus: it was never destroyed"
        );
    }

    /// Sorting by a column whose figures move used to come free, because the whole model
    /// was replaced on every refresh. It is asked for now, and must still happen.
    fn a_table_sorted_by_a_live_column_reorders_as_the_figures_move(window: &LaveWindow) {
        let heaviest = |window: &LaveWindow, listing: &NodeId| {
            let surface = window
                .detail_tab_for(listing)
                .expect("the containers listing has a tab")
                .surface;
            descendant::<gtk::ColumnView>(&surface.lead, |_| true)
                .and_then(|view| view.model())
                .and_then(|model| model.item(0))
                .and_downcast::<crate::table_view::TableRowObject>()
                .and_then(|row| row.key())
        };

        let mut snapshot = snapshot_of(60, 1_000_000);
        snapshot.stats.insert(lave_core::engine::ContainerStats {
            id: "c007".to_owned(),
            memory_usage: 7_000_000_000,
            memory_limit: 8_000_000_000,
        });
        window.apply_snapshot(snapshot);

        let listing = NodeId::Containers;
        let tab = window.detail_tab(&listing);
        window.imp().tab_view.set_selected_page(&tab);
        window.present();
        window.render_detail();
        settle();

        // Heaviest first, sorted the way the reader sorts it: by clicking the heading.
        let surface = window
            .detail_tab_for(&listing)
            .expect("the containers listing has a tab")
            .surface;
        let view = descendant::<gtk::ColumnView>(&surface.lead, |_| true).expect("a table");
        let memory = view
            .columns()
            .iter::<glib::Object>()
            .flatten()
            .filter_map(|object| object.downcast::<gtk::ColumnViewColumn>().ok())
            .find(|column| column.title().is_some_and(|title| title == "Memory"))
            .expect("the containers table has a Memory column");
        view.sort_by_column(Some(&memory), gtk::SortType::Descending);
        settle();
        assert_eq!(
            heaviest(window, &listing),
            Some(NodeId::Container("c007".to_owned())),
            "the heaviest container sorts to the top"
        );

        // The figures move: another container is now the heaviest.
        let mut moved = snapshot_of(60, 1_000_000).stats;
        moved.insert(lave_core::engine::ContainerStats {
            id: "c042".to_owned(),
            memory_usage: 7_500_000_000,
            memory_limit: 8_000_000_000,
        });
        window.apply_stats(moved);
        settle();
        assert_eq!(
            heaviest(window, &listing),
            Some(NodeId::Container("c042".to_owned())),
            "and the order follows them"
        );
    }

    /// The ticks live in the window, not in the rows, so a table that is no longer
    /// rebuilt has to be told when they change.
    fn checking_every_row_shows_the_ticks(window: &LaveWindow) {
        window.apply_snapshot(snapshot_of(60, 1_000_000));

        let listing = NodeId::Containers;
        let tab = window.detail_tab(&listing);
        window.imp().tab_view.set_selected_page(&tab);
        window.present();
        window.render_detail();
        settle();

        let surface = window
            .detail_tab_for(&listing)
            .expect("the containers listing has a tab")
            .surface;
        let ticked = || {
            let mut found = 0;
            let mut stack = vec![surface.lead.clone().upcast::<gtk::Widget>()];
            while let Some(node) = stack.pop() {
                if let Some(check) = node.downcast_ref::<gtk::CheckButton>()
                    && check.is_active()
                {
                    found += 1;
                }
                let mut child = node.first_child();
                while let Some(widget) = child {
                    child = widget.next_sibling();
                    stack.push(widget);
                }
            }
            found
        };

        assert_eq!(ticked(), 0, "nothing is checked to begin with");

        window.set_all_checked(true);
        settle();
        assert!(
            ticked() > 1,
            "checking every row must show in the rows, not only in the strip above them"
        );

        window.set_all_checked(false);
        settle();
        assert_eq!(ticked(), 0, "and unchecking must clear them again");
    }

    /// A container's own page has no table: it is groups of properties, and they scroll
    /// in the lower half. That is the panel a container is read in.
    #[allow(clippy::float_cmp)]
    fn a_containers_page_keeps_its_place_across_a_refresh(window: &LaveWindow) {
        window.apply_snapshot(snapshot_of(60, 1_000_000));

        let object = NodeId::Container("c007".to_owned());
        let tab = window.detail_tab(&object);
        window.imp().tab_view.set_selected_page(&tab);
        window.set_default_size(1000, 400);
        window.present();
        window.render_detail();
        settle();

        let surface = window
            .detail_tab_for(&object)
            .expect("the container has a tab")
            .surface;
        let scroller = surface
            .paned
            .end_child()
            .and_downcast::<gtk::ScrolledWindow>()
            .expect("the lower half scrolls");
        let adjustment = scroller.vadjustment();
        assert!(
            adjustment.upper() > adjustment.page_size() + 50.0,
            "the page must be taller than the pane: upper {} page {}",
            adjustment.upper(),
            adjustment.page_size()
        );

        let target = (adjustment.upper() - adjustment.page_size()).min(120.0);
        adjustment.set_value(target);
        settle();
        assert_eq!(adjustment.value(), target, "the reader scrolled down");

        window.apply_stats(snapshot_of(60, 900_000_000).stats);
        settle();
        assert_eq!(
            adjustment.value(),
            target,
            "a sample must not throw the reader back to the top"
        );

        window.apply_snapshot(snapshot_of(60, 500_000_000));
        settle();
        assert_eq!(adjustment.value(), target, "nor must a snapshot");

        // Nor two of them landing before GTK has laid the first out.
        window.apply_stats(snapshot_of(60, 700_000_000).stats);
        window.apply_stats(snapshot_of(60, 300_000_000).stats);
        settle();
        assert_eq!(
            adjustment.value(),
            target,
            "two redraws in a turn must not lose the place either"
        );

        // Last, since clicking a row moves the page to it: a selectable value takes the
        // focus, and a scroller scrolls to keep a focused widget in view — so a redraw
        // that destroyed it left the page wherever the new focus landed.
        if let Some(row) =
            descendant::<adw::ActionRow>(&surface.body, gtk::prelude::WidgetExt::is_mapped)
        {
            row.grab_focus();
            settle();
            let after_click = adjustment.value();

            window.apply_stats(snapshot_of(60, 400_000_000).stats);
            settle();
            assert_eq!(
                adjustment.value(),
                after_click,
                "a refresh after a click must not move the page either"
            );
        }
    }

    /// The daemon as the reader actually has it: two containers running, two stopped,
    /// disk usage accounted for and the daemon with something to say for itself.
    fn daemon_snapshot(usage: i64) -> Snapshot {
        let mut containers = Vec::new();
        let mut stats = StatsIndex::new();
        for index in 0..4 {
            let id = format!("d{index:03}");
            let running = index < 2;
            containers.push(ContainerSummary {
                id: id.clone(),
                names: vec![format!("service-{index:03}")],
                image: "nginx:1.27".to_owned(),
                state: if running {
                    ContainerState::Running
                } else {
                    ContainerState::Exited
                },
                ..ContainerSummary::default()
            });
            if running {
                stats.insert(lave_core::engine::ContainerStats {
                    id,
                    memory_usage: usage,
                    memory_limit: 8_000_000_000,
                });
            }
        }

        let category = |size: i64| {
            Some(lave_core::engine::DiskCategory {
                total_count: 4,
                active_count: 2,
                size,
                reclaimable: size / 4,
            })
        };

        Snapshot {
            resolved: lave_core::endpoint::Resolved {
                endpoint: lave_core::endpoint::Endpoint::Unix("/var/run/docker.sock".into()),
                source: lave_core::endpoint::EndpointSource::RootfulSocket,
            },
            environment: lave_core::engine::EnvironmentSummary {
                name: "workshop".to_owned(),
                server_version: "27.3.1".to_owned(),
                api_version: "1.47".to_owned(),
                min_api_version: Some("1.24".to_owned()),
                os_type: "linux".to_owned(),
                architecture: "x86_64".to_owned(),
                operating_system: "Debian GNU/Linux 13 (trixie)".to_owned(),
                kernel_version: "6.12.101".to_owned(),
                storage_driver: "overlay2".to_owned(),
                logging_driver: "json-file".to_owned(),
                cgroup_version: "2".to_owned(),
                cgroup_driver: "systemd".to_owned(),
                rootless: false,
                cpus: 16,
                memory_total: 32_000_000_000,
                docker_root_dir: "/var/lib/docker".to_owned(),
                containers_total: 4,
                containers_running: 2,
                containers_paused: 0,
                containers_stopped: 2,
                images: 12,
                security_options: vec!["apparmor".to_owned(), "seccomp".to_owned()],
                warnings: vec!["No swap limit support".to_owned()],
            },
            images: Vec::new(),
            containers,
            layers: LayerIndex::new(),
            stats,
            disk: lave_core::engine::DiskUsage {
                images: category(9_000_000_000),
                containers: category(400_000_000),
                volumes: category(2_000_000_000),
                build_cache: category(1_500_000_000),
            },
        }
    }

    /// The reader's report, acted out: the daemon's own page, scrolled down, a value
    /// clicked into — and then the redraws that arrive every few seconds.
    ///
    /// Clicking a value is not clicking a row: a selectable subtitle is its own focusable
    /// widget. A redraw that has to build the lower half again takes that widget with it,
    /// and GTK hands the focus to whatever it finds instead, which is a row at the top.
    /// The viewport inside a scroller follows the focus — so from that moment the page is
    /// dragged back to the top on every layout, however often the reader scrolls down
    /// again. That last part is what they noticed most.
    #[allow(clippy::float_cmp)]
    fn a_selected_value_survives_a_refresh(window: &LaveWindow) {
        window.apply_snapshot(daemon_snapshot(1_000_000));

        let root = NodeId::Root;
        let tab = window.detail_tab(&root);
        window.imp().tab_view.set_selected_page(&tab);
        window.set_default_size(1400, 900);
        window.present();
        window.render_detail();
        settle();

        let surface = window
            .detail_tab_for(&root)
            .expect("the daemon has a tab")
            .surface;
        let scroller = surface
            .paned
            .end_child()
            .and_downcast::<gtk::ScrolledWindow>()
            .expect("the lower half scrolls");
        let adjustment = scroller.vadjustment();
        assert!(
            adjustment.upper() > adjustment.page_size() + 100.0,
            "the daemon's page must be taller than the pane: upper {} page {}",
            adjustment.upper(),
            adjustment.page_size()
        );

        // The value the reader clicks into, and the widget that click lands in.
        let row = descendant::<adw::ActionRow>(&surface.body, |row| row.title() == "Memory in use")
            .expect("the footprint group states the memory in use");
        let value = descendant::<gtk::Label>(&row, |label: &gtk::Label| label.is_selectable())
            .expect("the value is selectable, so it can be copied");

        // Scrolled to where the reader would be to click it: the row is on screen, so
        // clicking it moves nothing by itself.
        let top = row
            .compute_bounds(&surface.body)
            .map(|bounds| f64::from(bounds.y()))
            .expect("the row is laid out");
        let target = (top - 60.0)
            .max(0.0)
            .min(adjustment.upper() - adjustment.page_size());
        adjustment.set_value(target);
        settle();
        assert_eq!(
            adjustment.value(),
            target,
            "the reader scrolled to the value"
        );

        value.grab_focus();
        settle();
        assert_eq!(
            adjustment.value(),
            target,
            "clicking a value already on screen does not move the page"
        );

        // The samples that arrive in between, which change the value but not the page.
        window.apply_snapshot(daemon_snapshot(900_000_000));
        settle();
        assert_eq!(
            adjustment.value(),
            target,
            "a sample after the click must not throw the reader back"
        );

        // And the redraw that does have to build the lower half again: the daemon has
        // begun reporting something it was not reporting before, so the page is a
        // different shape and its widgets cannot be kept.
        let mut reshaped = daemon_snapshot(800_000_000);
        reshaped
            .environment
            .security_options
            .push("userns".to_owned());
        window.apply_snapshot(reshaped.clone());
        settle();
        assert_eq!(
            adjustment.value(),
            target,
            "a redraw that rebuilds the lower half must still leave the reader where \
             they were"
        );

        // The focus must be back in the value the reader clicked, and not on a row at
        // the top: a scroller keeps its focused child in view, and a focus at the top is
        // what drags the page up again on every layout from then on.
        let focused = gtk::prelude::RootExt::focus(window).expect("something has the focus");
        let clicked =
            descendant::<adw::ActionRow>(&surface.body, |row| row.title() == "Memory in use")
                .expect("the row was built again")
                .upcast::<gtk::Widget>();
        let inside = std::iter::successors(Some(focused.clone()), gtk::prelude::WidgetExt::parent)
            .any(|widget| widget == clicked);
        assert!(
            inside,
            "the focus belongs in the row the reader clicked, not wherever GTK put it: \
             it is on a {}",
            focused.type_().name()
        );

        // And the part the reader noticed most: scrolling down again, and being thrown
        // back up by the next redraw that touches nothing at all.
        adjustment.set_value(target);
        settle();
        for usage in [700_000_000_i64, 600_000_000] {
            let mut later = daemon_snapshot(usage);
            later
                .environment
                .security_options
                .clone_from(&reshaped.environment.security_options);
            window.apply_snapshot(later);
            settle();
            assert_eq!(
                adjustment.value(),
                target,
                "and every refresh after it must leave the reader alone too"
            );
        }
    }

    /// The other half of the same story: the row the reader had clicked into is not on
    /// the page at all any more, so there is nothing to give the focus back to.
    ///
    /// It is given to nothing, deliberately. Left where GTK puts it when the widget under
    /// it goes, the focus is on a row at the top, and the scroller follows it there.
    #[allow(clippy::float_cmp)]
    fn a_value_that_goes_away_does_not_take_the_page_with_it(window: &LaveWindow) {
        window.apply_snapshot(daemon_snapshot(1_000_000));

        let root = NodeId::Root;
        let tab = window.detail_tab(&root);
        window.imp().tab_view.set_selected_page(&tab);
        window.set_default_size(1400, 900);
        window.present();
        window.render_detail();
        settle();

        let surface = window
            .detail_tab_for(&root)
            .expect("the daemon has a tab")
            .surface;
        let scroller = surface
            .paned
            .end_child()
            .and_downcast::<gtk::ScrolledWindow>()
            .expect("the lower half scrolls");
        let adjustment = scroller.vadjustment();

        // A row that exists only while the daemon itemises its disk usage.
        let row = descendant::<adw::ActionRow>(&surface.body, |row| row.title() == "Total on disk")
            .expect("the footprint group totals the disk");
        let value = descendant::<gtk::Label>(&row, |label: &gtk::Label| label.is_selectable())
            .expect("the value is selectable, so it can be copied");

        let top = row
            .compute_bounds(&surface.body)
            .map(|bounds| f64::from(bounds.y()))
            .expect("the row is laid out");
        let target = (top - 60.0)
            .max(0.0)
            .min(adjustment.upper() - adjustment.page_size());
        adjustment.set_value(target);
        value.grab_focus();
        settle();

        // Where the reader is once the row is on screen and clicked into. That is what
        // the redraw below must not disturb.
        let target = adjustment.value();
        assert!(target > 0.0, "the reader is not at the top to begin with");

        // The daemon stops accounting for the disk, and takes five rows with it.
        let mut without = daemon_snapshot(900_000_000);
        without.disk = lave_core::engine::DiskUsage::default();
        window.apply_snapshot(without);
        settle();

        let reachable = (adjustment.upper() - adjustment.page_size()).max(0.0);
        assert_eq!(
            adjustment.value(),
            target.min(reachable),
            "a shorter page leaves the reader as near where they were as it can, and \
             not at the top"
        );
    }

    /// The sidebar is a listing too, and the snapshot that redraws it arrives every few
    /// seconds. Expanding a branch and reading down it must survive one.
    #[allow(clippy::float_cmp)]
    fn the_sidebar_keeps_its_place_across_a_refresh(window: &LaveWindow) {
        window.apply_snapshot(snapshot_of(60, 1_000_000));
        window.expand_all();
        settle();

        let adjustment = window
            .imp()
            .tree_view
            .vadjustment()
            .expect("the tree scrolls");
        assert!(
            adjustment.upper() > adjustment.page_size() + 100.0,
            "sixty containers must overflow the sidebar: upper {} page {}",
            adjustment.upper(),
            adjustment.page_size()
        );

        adjustment.set_value(150.0);
        settle();
        assert_eq!(adjustment.value(), 150.0, "the reader scrolled down");

        // Nothing has come or gone: the same objects, with figures that have moved.
        window.apply_snapshot(snapshot_of(60, 900_000_000));
        settle();
        assert_eq!(
            adjustment.value(),
            150.0,
            "a refresh must leave the sidebar where the reader put it"
        );

        // And the same again once the reader has clicked one of the rows, which is what
        // puts the focus inside the list.
        let clicked = visible_row(&window.imp().tree_view.clone().upcast(), &adjustment)
            .expect("rows are on screen");
        clicked.grab_focus();
        let _ = clicked.activate_action("listitem.select", Some(&(false, false).to_variant()));
        settle();
        let after_click = adjustment.value();

        window.apply_snapshot(snapshot_of(60, 500_000_000));
        settle();
        assert_eq!(
            adjustment.value(),
            after_click,
            "a refresh after a click must leave the sidebar alone too"
        );
    }

    /// A snapshot of one running container, with whatever memory figure is given.
    fn snapshot_holding(usage: i64) -> Snapshot {
        let container = ContainerSummary {
            id: "held".to_owned(),
            names: vec!["held".to_owned()],
            image: "nginx:1.27".to_owned(),
            state: ContainerState::Running,
            ..ContainerSummary::default()
        };

        let mut stats = StatsIndex::new();
        stats.insert(lave_core::engine::ContainerStats {
            id: "held".to_owned(),
            memory_usage: usage,
            memory_limit: 8_000_000_000,
        });

        Snapshot {
            resolved: lave_core::endpoint::Resolved {
                endpoint: lave_core::endpoint::Endpoint::Unix("/var/run/docker.sock".into()),
                source: lave_core::endpoint::EndpointSource::RootfulSocket,
            },
            environment: lave_core::engine::EnvironmentSummary::default(),
            images: Vec::new(),
            containers: vec![container],
            layers: LayerIndex::new(),
            stats,
            disk: lave_core::engine::DiskUsage::default(),
        }
    }

    /// Samples arrive every few seconds whether or not they say anything new, and the
    /// pane they redraw is the one the user is reading. One that changes nothing must
    /// leave the widgets — and so the scroll position — exactly where they were.
    fn a_sample_that_says_nothing_new_does_not_rebuild_the_pane(window: &LaveWindow) {
        window.apply_snapshot(snapshot_holding(1_000_000));
        let held = NodeId::Container("held".to_owned());
        let tab = window.detail_tab(&held);
        window.imp().tab_view.set_selected_page(&tab);
        window.render_detail();

        let surface = window
            .detail_tab_for(&held)
            .expect("the container has a tab")
            .surface;
        let before = surface.body.first_child().expect("the page was drawn");

        let mut same = StatsIndex::new();
        same.insert(lave_core::engine::ContainerStats {
            id: "held".to_owned(),
            memory_usage: 1_000_000,
            memory_limit: 8_000_000_000,
        });
        window.apply_stats(same);
        assert_eq!(
            surface.body.first_child().as_ref(),
            Some(&before),
            "an identical page must not be rebuilt"
        );

        let reading = || {
            descendant::<adw::ActionRow>(&surface.body, |row| row.title() == "In use")
                .and_then(|row| row.subtitle())
                .map(|value| value.to_string())
        };
        let was = reading().expect("the memory row is on the page");

        let mut moved = StatsIndex::new();
        moved.insert(lave_core::engine::ContainerStats {
            id: "held".to_owned(),
            memory_usage: 900_000_000,
            memory_limit: 8_000_000_000,
        });
        window.apply_stats(moved);

        // Written into the row rather than drawn again: the widgets are what the reader
        // has their place in, their click on, and their focus in.
        assert_eq!(
            surface.body.first_child().as_ref(),
            Some(&before),
            "a page of the same shape is not rebuilt for a figure that moved"
        );
        assert_ne!(
            reading(),
            Some(was),
            "and the figure that moved must still reach the screen"
        );
    }
}
