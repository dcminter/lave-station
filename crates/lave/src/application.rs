//! Application setup: actions, styling, and the update loop that feeds the window.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use async_channel::Receiver;
use gtk::glib;
use lave_core::cli::Cli;

use crate::background::BackgroundOutcome;
use crate::runtime::{self, Update};
use crate::window::LaveWindow;

pub const APP_ID: &str = "com.paperstack.LaveStation";
const RESOURCE_PATH: &str = "/com/paperstack/LaveStation";

pub fn run(cli: &Cli) -> glib::ExitCode {
    let application = adw::Application::builder().application_id(APP_ID).build();

    application.connect_startup(|application| {
        setup_actions(application);
        load_style();
        // Empty mount points left by a run that did not unmount cleanly.
        crate::fuse_mount::sweep_stale_mounts();
    });

    let docker_host = cli.docker_host.clone();
    let want_indicator = !cli.no_indicator;
    let existing: Rc<RefCell<Option<LaveWindow>>> = Rc::new(RefCell::new(None));

    // Quitting from the menu or the tray never emits close-request, so the sidebar
    // width has to be captured here as well as on window close.
    application.connect_shutdown(glib::clone!(
        #[strong]
        existing,
        move |_| {
            if let Some(window) = existing.borrow().as_ref() {
                window.store_sidebar_width();
                // Unmount while there is still a program to do it in.
                window.release_mounts();
            }
        }
    ));

    application.connect_activate(move |application| {
        if let Some(window) = existing.borrow().as_ref() {
            window.present();
            return;
        }

        let handle = runtime::start(docker_host.clone(), want_indicator);
        let window = LaveWindow::new(application, handle.commands.clone());
        consume_updates(application, &window, handle.updates);
        window.present();

        existing.replace(Some(window));
    });

    // clap has already parsed the command line; GApplication must not parse it again.
    application.run_with_args(&["lave"])
}

/// Updates arrive from the runtime thread and are applied here, on the main thread,
/// which is the only place widgets may be touched.
fn consume_updates(application: &adw::Application, window: &LaveWindow, updates: Receiver<Update>) {
    glib::spawn_future_local(glib::clone!(
        #[weak]
        application,
        #[weak]
        window,
        async move {
            while let Ok(update) = updates.recv().await {
                match update {
                    Update::Snapshot(snapshot) => window.apply_snapshot(*snapshot),
                    Update::Inspected { id, raw } => window.apply_inspect(&id, *raw),
                    Update::Status(status) => window.apply_status(&status),
                    Update::ActionOutcome { message, failed } => {
                        window.apply_action_outcome(&message, failed);
                    }
                    Update::Dockerfile {
                        image_id,
                        title,
                        text,
                    } => {
                        window.apply_dockerfile(&image_id, &title, &text);
                    }
                    Update::LogLines {
                        container_id,
                        lines,
                        dropped,
                    } => {
                        window.apply_log_lines(&container_id, &lines, dropped);
                    }
                    Update::LogsEnded { error } => {
                        window.apply_logs_ended(error.as_deref());
                    }
                    Update::Listing {
                        path,
                        entries,
                        notice,
                    } => {
                        window.apply_listing(&path, &entries, notice.as_deref());
                    }
                    Update::IndicatorAvailable(available) => {
                        window.set_indicator_available(available);
                    }
                    Update::Background(outcome) => {
                        if outcome == BackgroundOutcome::Denied {
                            window.set_indicator_available(false);
                        }
                    }
                    Update::OpenRequested => window.present(),
                    Update::QuitRequested => application.quit(),
                }
            }
        }
    ));
}

fn setup_actions(application: &adw::Application) {
    let quit = gtk::gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        application,
        move |_, _| application.quit()
    ));
    application.add_action(&quit);
    application.set_accels_for_action("app.quit", &["<Control>q"]);

    let about = gtk::gio::SimpleAction::new("about", None);
    about.connect_activate(glib::clone!(
        #[weak]
        application,
        move |_, _| show_about(&application)
    ));
    application.add_action(&about);
}

fn show_about(application: &adw::Application) {
    let dialog = adw::AboutDialog::builder()
        .application_name("Lave Station")
        .application_icon(APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("Dave Minter")
        .comments("A GTK interface to Docker, speaking the daemon's API directly.")
        .license_type(gtk::License::MitX11)
        .build();

    dialog.present(application.active_window().as_ref());
}

fn load_style() {
    let provider = gtk::CssProvider::new();
    provider.load_from_resource(&format!("{RESOURCE_PATH}/style.css"));

    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        // Our own icons come from the binary, so they work before installation.
        gtk::IconTheme::for_display(&display).add_resource_path(&format!("{RESOURCE_PATH}/icons"));
    }
}
