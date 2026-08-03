//! Where [`Settings`] are actually kept: `GSettings`.
//!
//! The schema is looked for in the usual place first, so an installed copy under
//! `/usr/share/glib-2.0/schemas` wins. Failing that it falls back to the directory
//! `build.rs` compiled at build time, which is what makes an uninstalled `cargo run`
//! work without a system-wide install.
//!
//! A store that cannot be opened at all is not fatal: the application runs on defaults
//! and simply forgets them at exit, which beats refusing to start over a sidebar width.

use std::collections::HashMap;

use gtk::gio;
use gtk::prelude::{SettingsExt, SettingsExtManual};
use lave_core::settings::{ColumnWidths, Settings};

const SCHEMA_ID: &str = "com.paperstack.LaveStation";

const SIDEBAR_WIDTH: &str = "sidebar-width";
const SHOW_STOPPED: &str = "show-stopped-containers";
const SHOW_UNTAGGED: &str = "show-untagged-images";
const COLUMN_WIDTHS: &str = "column-widths";

/// Compiled by `build.rs`; present whether or not the schema is installed system-wide.
const BUILT_SCHEMA_DIR: &str = env!("LAVE_SCHEMA_DIR");

/// The settings store, or nothing when no schema could be found.
pub struct Prefs {
    settings: Option<gio::Settings>,
}

impl Prefs {
    /// Open the user's store.
    #[must_use]
    pub fn new() -> Self {
        Self::open(None)
    }

    /// Open a store against a specific backend. Tests pass a memory backend so they
    /// cannot write to the user's real settings.
    fn open(backend: Option<&gio::SettingsBackend>) -> Self {
        let Some(schema) = lookup_schema() else {
            tracing::warn!(
                "no {SCHEMA_ID} GSettings schema found: preferences will not be remembered"
            );
            return Self { settings: None };
        };

        Self {
            settings: Some(gio::Settings::new_full(&schema, backend, None)),
        }
    }

    #[must_use]
    pub fn load(&self) -> Settings {
        let Some(settings) = &self.settings else {
            return Settings::default();
        };

        Settings {
            sidebar_width: settings.int(SIDEBAR_WIDTH),
            show_stopped_containers: settings.boolean(SHOW_STOPPED),
            show_untagged_images: settings.boolean(SHOW_UNTAGGED),
            column_widths: read_widths(settings),
        }
        .clamped()
    }

    /// Write everything back. Each key is set individually rather than as one blob, so a
    /// value the schema rejects cannot take the others down with it.
    pub fn store(&self, settings: &Settings) {
        let Some(store) = &self.settings else {
            return;
        };
        let settings = settings.clone().clamped();

        report(
            SIDEBAR_WIDTH,
            store.set_int(SIDEBAR_WIDTH, settings.sidebar_width),
        );
        report(
            SHOW_STOPPED,
            store.set_boolean(SHOW_STOPPED, settings.show_stopped_containers),
        );
        report(
            SHOW_UNTAGGED,
            store.set_boolean(SHOW_UNTAGGED, settings.show_untagged_images),
        );
        report(
            COLUMN_WIDTHS,
            store.set(COLUMN_WIDTHS, write_widths(&settings.column_widths)),
        );
    }
}

impl Default for Prefs {
    fn default() -> Self {
        Self::new()
    }
}

fn report(key: &str, outcome: Result<(), gtk::glib::BoolError>) {
    if let Err(error) = outcome {
        tracing::warn!("could not store {key}: {error}");
    }
}

/// `BTreeMap` in the model, because tests want a determinate order; `HashMap` here,
/// because that is what glib knows how to turn into a dictionary.
fn write_widths(widths: &ColumnWidths) -> HashMap<String, HashMap<String, i32>> {
    widths
        .iter()
        .map(|(table, columns)| {
            let columns = columns
                .iter()
                .map(|(column, width)| (column.clone(), *width))
                .collect();
            (table.clone(), columns)
        })
        .collect()
}

fn read_widths(settings: &gio::Settings) -> ColumnWidths {
    let Some(stored) = settings
        .value(COLUMN_WIDTHS)
        .get::<HashMap<String, HashMap<String, i32>>>()
    else {
        // The schema fixes the type, so this means the store holds something written by
        // a version that disagreed with this one. Defaults are the safe reading.
        tracing::warn!("stored column widths were not of the expected shape; ignoring them");
        return ColumnWidths::new();
    };

    stored
        .into_iter()
        .map(|(table, columns)| (table, columns.into_iter().collect()))
        .collect()
}

/// The schema, from wherever it can be found.
fn lookup_schema() -> Option<gio::SettingsSchema> {
    if let Some(source) = gio::SettingsSchemaSource::default()
        && let Some(schema) = source.lookup(SCHEMA_ID, true)
    {
        return Some(schema);
    }

    // Not installed: fall back to the copy compiled alongside the binary. `trusted` is
    // true because we compiled it ourselves during this build.
    match gio::SettingsSchemaSource::from_directory(
        BUILT_SCHEMA_DIR,
        gio::SettingsSchemaSource::default().as_ref(),
        true,
    ) {
        Ok(source) => source.lookup(SCHEMA_ID, true),
        Err(error) => {
            tracing::warn!("could not read schemas from {BUILT_SCHEMA_DIR}: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;
    use std::collections::BTreeMap;

    /// A store backed by memory, so a test run cannot touch the user's real settings.
    fn scratch() -> Prefs {
        let backend = gio::functions::memory_settings_backend_new();
        let prefs = Prefs::open(Some(&backend));
        assert!(
            prefs.settings.is_some(),
            "the schema build.rs compiled should always be findable"
        );
        prefs
    }

    #[test]
    fn an_untouched_store_reads_back_as_the_models_own_defaults() {
        // The schema's defaults and Settings::default must agree, or the first run would
        // differ from every run after it.
        assert_eq!(scratch().load(), Settings::default());
    }

    #[test]
    fn settings_survive_a_round_trip_through_the_store() {
        let prefs = scratch();

        let stored = Settings {
            sidebar_width: 421,
            show_stopped_containers: false,
            show_untagged_images: false,
            column_widths: ColumnWidths::from([
                (
                    "containers".to_owned(),
                    BTreeMap::from([("Image".to_owned(), 240), ("Ports".to_owned(), 90)]),
                ),
                (
                    "images".to_owned(),
                    BTreeMap::from([("Size".to_owned(), 120)]),
                ),
            ]),
        };
        prefs.store(&stored);

        assert_eq!(prefs.load(), stored);
    }

    #[test]
    fn an_absurd_width_is_brought_into_range_rather_than_stored_as_given() {
        let prefs = scratch();

        prefs.store(&Settings {
            sidebar_width: 99_999,
            ..Settings::default()
        });

        assert_eq!(
            prefs.load().sidebar_width,
            lave_core::settings::MAX_SIDEBAR_WIDTH
        );
    }

    #[test]
    fn a_store_with_no_schema_behind_it_still_answers_with_defaults() {
        // What happens on a desktop where neither the installed schema nor the compiled
        // one can be read: the window opens, and forgets.
        let prefs = Prefs { settings: None };

        prefs.store(&Settings {
            sidebar_width: 421,
            ..Settings::default()
        });

        assert_eq!(prefs.load(), Settings::default());
    }
}
