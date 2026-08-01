//! Business logic for Lave Station. Deliberately free of GTK and D-Bus so that every
//! decision in the application is testable without a display or a session bus.

pub mod activity;
pub mod cli;
pub mod endpoint;
pub mod engine;
pub mod indicator;
pub mod model;
pub mod settings;
