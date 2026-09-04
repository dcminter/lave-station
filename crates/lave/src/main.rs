//! Lave Station: a GTK interface to Docker.

mod application;
mod background;
mod detail_pane;
mod fs_node;
mod fuse_mount;
mod group_columns;
mod indicator_tray;
mod list_rows;
mod prefs;
mod runtime;
mod table_view;
mod tree_node;
mod window;

use clap::Parser;
use gtk::glib;
use lave_core::cli::Cli;

fn main() -> glib::ExitCode {
    let cli = Cli::parse();

    // zbus warns loudly when a service is simply absent, which is normal here.
    tracing_subscriber::fmt()
        .with_env_filter(format!("{},zbus=error", cli.log_level.as_filter()))
        .init();

    // Bootstrap: without its own resources the application cannot draw anything.
    #[allow(
        clippy::expect_used,
        reason = "CLAUDE.md permits expect during bootstrap"
    )]
    gtk::gio::resources_register_include!("lave.gresource")
        .expect("the compiled-in resource bundle should load");

    application::run(&cli)
}
