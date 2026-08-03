//! View preferences that outlive a run.
//!
//! Only the *shape* of them lives here: the values, their permitted ranges, and the
//! clamping that keeps a hand-edited store from wedging the window. Where they are
//! actually kept is the widget layer's business — see `lave::prefs`, which binds these
//! to `GSettings`.
//!
//! Reading is total: a missing or nonsensical value yields a default rather than an
//! error, because a lost sidebar width is not worth refusing to open the window over.

use std::collections::BTreeMap;

/// Wide enough for a tag like `mcr.microsoft.com/playwright:v1.49.0-noble`.
pub const DEFAULT_SIDEBAR_WIDTH: i32 = 300;
/// Narrow enough to be a deliberate choice, not an accidental collapse.
pub const MIN_SIDEBAR_WIDTH: i32 = 180;
pub const MAX_SIDEBAR_WIDTH: i32 = 900;

/// Narrow enough to hide a column's contents, wide enough to still be grabbable.
pub const MIN_COLUMN_WIDTH: i32 = 32;
/// Beyond this the column is wider than any window it could be dragged in.
pub const MAX_COLUMN_WIDTH: i32 = 2000;

/// Widths the user dragged columns to, by table and then by column title.
pub type ColumnWidths = BTreeMap<String, BTreeMap<String, i32>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub sidebar_width: i32,
    /// Whether the container tables include the ones that are not running. One answer for
    /// both pages that list containers, since it is one question.
    pub show_stopped_containers: bool,
    /// Whether the image table includes the ones carrying no tag.
    pub show_untagged_images: bool,
    /// Column widths, keyed by table id and column title. A table or column that no
    /// longer exists is ignored at render time rather than dropped here, so renaming a
    /// column cannot make a stored value invalid.
    ///
    /// Sort order is deliberately absent: it lasts for the session only.
    pub column_widths: ColumnWidths,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            // The rest of the application shows stopped containers, so this does too
            // until the user says otherwise. Likewise untagged images: hiding things by
            // default would leave a new user wondering where they went.
            show_stopped_containers: true,
            show_untagged_images: true,
            column_widths: ColumnWidths::new(),
        }
    }
}

impl Settings {
    /// Bring every field within its permitted range. Applied on the way in and on the
    /// way out, so neither a hand-edited store nor an odd window state can wedge the
    /// sidebar at an unusable width.
    #[must_use]
    pub fn clamped(self) -> Self {
        let column_widths = self
            .column_widths
            .into_iter()
            .map(|(table, columns)| {
                let columns = columns
                    .into_iter()
                    .map(|(column, width)| (column, clamp_column(width)))
                    .collect();
                (table, columns)
            })
            .collect();

        Self {
            sidebar_width: self
                .sidebar_width
                .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH),
            column_widths,
            ..self
        }
    }

    /// The width to open a column at, or `None` for whatever the view works out itself.
    #[must_use]
    pub fn column_width(&self, table: &str, column: &str) -> Option<i32> {
        self.column_widths
            .get(table)
            .and_then(|columns| columns.get(column))
            .copied()
    }

    /// Remember a column's width. Returns whether anything actually changed, so a caller
    /// can skip writing the store when a restored width is merely being echoed back.
    ///
    /// A negative width is GTK saying "size this yourself", which is the absence of a
    /// stored width rather than a width of its own.
    pub fn set_column_width(&mut self, table: &str, column: &str, width: i32) -> bool {
        if width < 0 {
            return self
                .column_widths
                .get_mut(table)
                .is_some_and(|columns| columns.remove(column).is_some());
        }

        let width = clamp_column(width);
        let columns = self.column_widths.entry(table.to_owned()).or_default();

        if columns.get(column) == Some(&width) {
            return false;
        }
        columns.insert(column.to_owned(), width);
        true
    }
}

fn clamp_column(width: i32) -> i32 {
    width.clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH)
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;

    #[test]
    fn the_defaults_show_everything_at_a_readable_width() {
        let settings = Settings::default();

        assert_eq!(settings.sidebar_width, DEFAULT_SIDEBAR_WIDTH);
        assert!(settings.show_stopped_containers);
        assert!(settings.column_widths.is_empty());
    }

    #[test]
    fn an_unreasonable_stored_width_is_brought_back_into_range() {
        let clamp = |width: i32| {
            Settings {
                sidebar_width: width,
                ..Settings::default()
            }
            .clamped()
            .sidebar_width
        };

        assert_eq!(clamp(5), MIN_SIDEBAR_WIDTH);
        assert_eq!(clamp(99_999), MAX_SIDEBAR_WIDTH);
        assert_eq!(clamp(-1), MIN_SIDEBAR_WIDTH);
        assert_eq!(clamp(420), 420, "a width within range is left alone");
    }

    #[test]
    fn a_column_width_is_stored_against_its_table_and_read_back() {
        let mut settings = Settings::default();

        assert!(settings.set_column_width("containers", "Image", 240));

        assert_eq!(settings.column_width("containers", "Image"), Some(240));
        assert_eq!(
            settings.column_width("images", "Image"),
            None,
            "a column of the same name in another table is a different column"
        );
        assert_eq!(settings.column_width("containers", "Ports"), None);
    }

    #[test]
    fn storing_the_width_a_column_already_has_reports_no_change() {
        // Restoring a width makes GTK notify, which would otherwise write the store back
        // on every render.
        let mut settings = Settings::default();
        assert!(settings.set_column_width("containers", "Image", 240));

        assert!(!settings.set_column_width("containers", "Image", 240));
        assert!(settings.set_column_width("containers", "Image", 241));
    }

    #[test]
    fn a_negative_width_means_size_it_yourself_and_forgets_any_stored_one() {
        let mut settings = Settings::default();
        settings.set_column_width("containers", "Image", 240);

        assert!(settings.set_column_width("containers", "Image", -1));
        assert_eq!(settings.column_width("containers", "Image"), None);
        assert!(
            !settings.set_column_width("containers", "Image", -1),
            "forgetting what was already forgotten changes nothing"
        );
    }

    #[test]
    fn an_absurd_column_width_is_brought_into_range_on_the_way_in() {
        let mut settings = Settings::default();

        settings.set_column_width("containers", "Image", 1);
        settings.set_column_width("containers", "Ports", 99_999);

        assert_eq!(
            settings.column_width("containers", "Image"),
            Some(MIN_COLUMN_WIDTH)
        );
        assert_eq!(
            settings.column_width("containers", "Ports"),
            Some(MAX_COLUMN_WIDTH)
        );
    }

    #[test]
    fn a_stored_width_from_elsewhere_is_brought_into_range_on_the_way_out() {
        // Nothing stops the store being edited by hand, or by an older version.
        let settings = Settings {
            column_widths: ColumnWidths::from([(
                "containers".to_owned(),
                BTreeMap::from([("Image".to_owned(), 99_999), ("Ports".to_owned(), 0)]),
            )]),
            ..Settings::default()
        }
        .clamped();

        assert_eq!(
            settings.column_width("containers", "Image"),
            Some(MAX_COLUMN_WIDTH)
        );
        assert_eq!(
            settings.column_width("containers", "Ports"),
            Some(MIN_COLUMN_WIDTH)
        );
    }

    #[test]
    fn a_width_for_a_table_that_no_longer_exists_is_kept_rather_than_discarded() {
        // Whether a table is still rendered is decided where it is rendered; storage
        // keeps whatever it was given, so a downgrade does not lose the user's drags.
        let settings = Settings {
            column_widths: ColumnWidths::from([(
                "volumes".to_owned(),
                BTreeMap::from([("Name".to_owned(), 200)]),
            )]),
            ..Settings::default()
        }
        .clamped();

        assert_eq!(settings.column_width("volumes", "Name"), Some(200));
    }
}
