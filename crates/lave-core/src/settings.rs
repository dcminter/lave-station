//! Window preferences that outlive a run.
//!
//! A plain file under `$XDG_CONFIG_HOME` rather than `GSettings`, which would need a
//! compiled schema installed system-wide before the application would start at all.
//! Reading is total: a corrupt or partial file yields defaults rather than an error,
//! because a lost sidebar width is not worth refusing to open the window over.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::endpoint::EnvSource;

/// Wide enough for a tag like `mcr.microsoft.com/playwright:v1.49.0-noble`.
pub const DEFAULT_SIDEBAR_WIDTH: i32 = 300;
/// Narrow enough to be a deliberate choice, not an accidental collapse.
pub const MIN_SIDEBAR_WIDTH: i32 = 180;
pub const MAX_SIDEBAR_WIDTH: i32 = 900;

/// Newest first, as `docker ps` itself lists them.
pub const DEFAULT_SORT_COLUMN: &str = "Created";

const DIRECTORY: &str = "lave-station";
const FILE: &str = "settings.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub sidebar_width: i32,
    /// Whether the environment page's container table includes stopped containers.
    pub show_stopped_containers: bool,
    /// Column title the container table is sorted by. A title that no longer exists is
    /// ignored at render time rather than rejected here, so renaming a column cannot
    /// make a stored file invalid.
    pub container_sort_column: String,
    pub container_sort_descending: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            // The rest of the application shows stopped containers, so this does too
            // until the user says otherwise.
            show_stopped_containers: true,
            container_sort_column: DEFAULT_SORT_COLUMN.to_owned(),
            container_sort_descending: true,
        }
    }
}

impl Settings {
    /// Bring every field within its permitted range. Applied on the way in and on the
    /// way out, so neither a hand-edited file nor an odd window state can wedge the
    /// sidebar at an unusable width.
    #[must_use]
    pub fn clamped(self) -> Self {
        Self {
            sidebar_width: self
                .sidebar_width
                .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH),
            ..self
        }
    }
}

/// Where the settings file lives: `$XDG_CONFIG_HOME/lave-station/settings.json`,
/// falling back to `$HOME/.config` as the XDG base directory specification requires.
#[must_use]
pub fn path(env: &dyn EnvSource) -> Option<PathBuf> {
    let base = env
        .var("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env.var("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;

    Some(base.join(DIRECTORY).join(FILE))
}

/// Parse stored settings, falling back to defaults for anything missing or malformed.
#[must_use]
pub fn parse(text: &str) -> Settings {
    serde_json::from_str::<Settings>(text)
        .unwrap_or_default()
        .clamped()
}

/// Render settings for storage.
#[must_use]
pub fn serialize(settings: &Settings) -> String {
    serde_json::to_string_pretty(settings).unwrap_or_else(|_| String::new())
}

/// Read the settings file, or return defaults when there is not one to read.
#[must_use]
pub fn load(env: &dyn EnvSource) -> Settings {
    path(env)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map_or_else(Settings::default, |text| parse(&text))
}

/// Write the settings file, creating its directory. Failure is logged, not propagated:
/// nothing the application does depends on the write succeeding.
pub fn save(env: &dyn EnvSource, settings: &Settings) {
    let Some(path) = path(env) else {
        tracing::debug!("no config directory: settings not saved");
        return;
    };

    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("could not create {}: {error}", parent.display());
        return;
    }

    write(&path, &serialize(&settings.clone().clamped()));
}

fn write(path: &Path, contents: &str) {
    if let Err(error) = std::fs::write(path, contents) {
        tracing::warn!("could not write {}: {error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;
    use std::collections::BTreeMap;

    struct FakeEnv(BTreeMap<String, String>);

    impl FakeEnv {
        fn new(pairs: &[(&str, &str)]) -> Self {
            Self(
                pairs
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect(),
            )
        }
    }

    impl EnvSource for FakeEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    #[test]
    fn the_file_lives_under_the_xdg_config_directory() {
        let env = FakeEnv::new(&[("XDG_CONFIG_HOME", "/home/dave/.config")]);

        assert_eq!(
            path(&env),
            Some(PathBuf::from(
                "/home/dave/.config/lave-station/settings.json"
            ))
        );
    }

    #[test]
    fn without_xdg_config_home_the_specified_fallback_is_used() {
        let env = FakeEnv::new(&[("HOME", "/home/dave")]);

        assert_eq!(
            path(&env),
            Some(PathBuf::from(
                "/home/dave/.config/lave-station/settings.json"
            ))
        );
    }

    #[test]
    fn an_empty_xdg_variable_is_treated_as_unset_rather_than_as_the_root() {
        let env = FakeEnv::new(&[("XDG_CONFIG_HOME", ""), ("HOME", "/home/dave")]);

        assert_eq!(
            path(&env),
            Some(PathBuf::from(
                "/home/dave/.config/lave-station/settings.json"
            ))
        );
    }

    #[test]
    fn with_no_home_at_all_there_is_nowhere_to_store_settings() {
        assert_eq!(path(&FakeEnv::new(&[])), None);
    }

    #[test]
    fn settings_round_trip_through_their_stored_form() {
        let settings = Settings {
            sidebar_width: 420,
            show_stopped_containers: false,
            container_sort_column: "Status".to_owned(),
            container_sort_descending: false,
        };

        assert_eq!(parse(&serialize(&settings)), settings);
    }

    #[test]
    fn the_container_table_defaults_to_newest_first_showing_everything() {
        let settings = Settings::default();

        assert_eq!(settings.container_sort_column, "Created");
        assert!(settings.container_sort_descending);
        assert!(settings.show_stopped_containers);
    }

    #[test]
    fn a_stored_file_from_before_these_options_existed_still_loads() {
        // Version 2.0 wrote only the width; it must not read back as a broken file.
        let settings = parse("{\"sidebar_width\": 420}");

        assert_eq!(settings.sidebar_width, 420);
        assert_eq!(settings.container_sort_column, DEFAULT_SORT_COLUMN);
        assert!(settings.show_stopped_containers);
    }

    #[test]
    fn a_sort_column_that_no_longer_exists_is_preserved_rather_than_discarded() {
        // Whether it is usable is decided where the table is rendered; storage keeps
        // whatever it was given, so a downgrade does not lose the user's choice.
        assert_eq!(
            parse("{\"container_sort_column\": \"Nonexistent\"}").container_sort_column,
            "Nonexistent"
        );
    }

    #[test]
    fn a_missing_field_falls_back_to_its_default_rather_than_failing() {
        assert_eq!(parse("{}"), Settings::default());
    }

    #[test]
    fn a_corrupt_file_yields_defaults_rather_than_an_error() {
        for text in ["", "not json", "[1, 2, 3]", "{\"sidebar_width\": \"wide\"}"] {
            assert_eq!(parse(text), Settings::default(), "text was {text:?}");
        }
    }

    #[test]
    fn an_unreasonable_stored_width_is_brought_back_into_range() {
        assert_eq!(
            parse("{\"sidebar_width\": 5}").sidebar_width,
            MIN_SIDEBAR_WIDTH
        );
        assert_eq!(
            parse("{\"sidebar_width\": 99999}").sidebar_width,
            MAX_SIDEBAR_WIDTH
        );
        assert_eq!(
            parse("{\"sidebar_width\": -1}").sidebar_width,
            MIN_SIDEBAR_WIDTH
        );
    }

    #[test]
    fn a_width_within_range_is_left_alone() {
        assert_eq!(parse("{\"sidebar_width\": 420}").sidebar_width, 420);
    }

    #[test]
    fn unknown_fields_are_ignored_so_a_newer_version_can_add_some() {
        assert_eq!(
            parse("{\"sidebar_width\": 420, \"future_option\": true}").sidebar_width,
            420
        );
    }

    #[test]
    fn loading_with_nowhere_to_load_from_gives_defaults() {
        assert_eq!(load(&FakeEnv::new(&[])), Settings::default());
    }

    #[test]
    fn settings_survive_a_round_trip_through_a_real_file() {
        // Exercises the parts unit tests of parse and serialize cannot reach: creating
        // the directory, and reading back what was actually written.
        let directory = std::env::temp_dir().join(format!("lave-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let env = FakeEnv::new(&[("XDG_CONFIG_HOME", &directory.to_string_lossy())]);

        assert_eq!(load(&env), Settings::default(), "nothing stored yet");

        save(
            &env,
            &Settings {
                sidebar_width: 421,
                ..Settings::default()
            },
        );
        assert_eq!(load(&env).sidebar_width, 421);

        // Storing something absurd still yields something usable on the way back.
        save(
            &env,
            &Settings {
                sidebar_width: -5,
                ..Settings::default()
            },
        );
        assert_eq!(load(&env).sidebar_width, MIN_SIDEBAR_WIDTH);

        let _ = std::fs::remove_dir_all(&directory);
    }
}
