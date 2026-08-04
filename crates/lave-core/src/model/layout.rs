//! Where the detail groups sit: how many columns fit a given width, and which column
//! each group goes in.
//!
//! A flow box was the obvious widget for this and is the wrong one: it lays out in lines
//! and gives every child in a line the height of the tallest, so one long group leaves a
//! crater of blank space beside it. Packing each group under the last one in its own
//! column instead means a group is only ever as tall as its own contents.

/// A column narrower than this cannot show a label and its value side by side, so the
/// groups fold to a single column rather than wrapping every value.
pub const MIN_COLUMN_WIDTH: i32 = 380;
/// Between columns, and between the groups stacked within one.
pub const GUTTER: i32 = 18;
/// Beyond two the lines are too short to read comfortably at any window size.
pub const MAX_COLUMNS: usize = 2;

/// Where one group ends up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub column: usize,
    /// Distance from the top of the container.
    pub top: i32,
}

/// How many columns fit, never more than [`MAX_COLUMNS`] and never fewer than one.
#[must_use]
pub fn column_count(width: i32) -> usize {
    let mut columns = 1;
    while columns < MAX_COLUMNS {
        let wanted = i32::try_from(columns + 1).unwrap_or(1);
        if width < wanted * MIN_COLUMN_WIDTH + (wanted - 1) * GUTTER {
            break;
        }
        columns += 1;
    }
    columns
}

/// How many columns to use for `groups` of content.
///
/// Never more than fit, and never more than there are groups: a page showing one group
/// gives it the whole width rather than leaving half the pane empty beside it.
#[must_use]
pub fn columns_for(width: i32, groups: usize) -> usize {
    column_count(width).min(groups.max(1))
}

/// What one column gets of `width`, the gutters having been taken out first.
#[must_use]
pub fn column_width(width: i32, columns: usize) -> i32 {
    let columns = i32::try_from(columns.max(1)).unwrap_or(1);
    let gutters = (columns - 1) * GUTTER;
    ((width - gutters) / columns).max(1)
}

/// Place each group, and report the height the whole lot needs.
///
/// Every group goes into whichever column is shortest at the time, so a long group is
/// followed by short ones beside it rather than by empty space. Ties go to the leftmost
/// column, which keeps the reading order left to right for the common case of groups of
/// similar length.
#[must_use]
pub fn place(heights: &[i32], columns: usize) -> (Vec<Placement>, i32) {
    let columns = columns.max(1);
    // Where the next group in each column would start.
    let mut next = vec![0_i32; columns];
    let mut placements = Vec::with_capacity(heights.len());

    for height in heights {
        let column = shortest(&next);
        let top = next[column];
        placements.push(Placement { column, top });
        next[column] = top.saturating_add(height.max(&0).saturating_add(GUTTER));
    }

    // The trailing gutter belongs between groups, not below the last one.
    let total = next.iter().copied().max().unwrap_or(0);
    (placements, (total - GUTTER).max(0))
}

fn shortest(next: &[i32]) -> usize {
    let mut best = 0;
    for (index, height) in next.iter().enumerate() {
        if *height < next[best] {
            best = index;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_narrow_pane_folds_to_one_column() {
        assert_eq!(column_count(0), 1);
        assert_eq!(column_count(MIN_COLUMN_WIDTH), 1);
        assert_eq!(column_count(2 * MIN_COLUMN_WIDTH + GUTTER - 1), 1);
    }

    #[test]
    fn a_pane_wide_enough_for_two_columns_gets_two() {
        assert_eq!(column_count(2 * MIN_COLUMN_WIDTH + GUTTER), 2);
        assert_eq!(column_count(4000), MAX_COLUMNS);
    }

    #[test]
    fn a_lone_group_takes_the_whole_width() {
        // The Containers and Images pages show one group. Half a wide pane of summary
        // beside an empty half is worse than a summary the width of the table above it.
        assert_eq!(columns_for(4000, 1), 1);
        assert_eq!(column_width(4000, columns_for(4000, 1)), 4000);
    }

    #[test]
    fn several_groups_use_as_many_columns_as_fit() {
        assert_eq!(columns_for(4000, 2), 2);
        assert_eq!(columns_for(4000, 7), MAX_COLUMNS);
        assert_eq!(columns_for(500, 7), 1, "a narrow pane still folds");
    }

    #[test]
    fn nothing_to_show_still_reports_a_column() {
        assert_eq!(columns_for(4000, 0), 1);
    }

    #[test]
    fn columns_divide_the_width_with_the_gutters_taken_out_first() {
        assert_eq!(column_width(1000, 1), 1000);
        assert_eq!(column_width(1018, 2), 500);
    }

    #[test]
    fn a_column_never_measures_zero_however_little_there_is_to_share() {
        assert_eq!(column_width(0, 2), 1);
        assert_eq!(column_width(-50, 2), 1);
        assert_eq!(column_width(100, 0), 100);
    }

    #[test]
    fn groups_stack_under_one_another_within_a_column() {
        let (placed, height) = place(&[100, 200], 1);

        assert_eq!(placed[0], Placement { column: 0, top: 0 });
        assert_eq!(
            placed[1],
            Placement {
                column: 0,
                top: 100 + GUTTER
            }
        );
        assert_eq!(height, 100 + GUTTER + 200);
    }

    #[test]
    fn a_long_group_does_not_strand_the_short_one_beside_it() {
        // This is the whole point: with a flow box the 40-tall group would be given the
        // 400-tall group's height, and the two after it would start below both.
        let (placed, height) = place(&[400, 40, 40, 40], 2);

        assert_eq!(placed[0].column, 0);
        assert_eq!(placed[1], Placement { column: 1, top: 0 });
        // The short column keeps taking work until it catches up.
        assert_eq!(placed[2].column, 1);
        assert_eq!(placed[3].column, 1);
        assert_eq!(height, 400);
    }

    #[test]
    fn equal_groups_read_left_to_right() {
        let (placed, _) = place(&[100, 100, 100, 100], 2);

        assert_eq!(placed[0].column, 0);
        assert_eq!(placed[1].column, 1);
        assert_eq!(placed[2].column, 0);
        assert_eq!(placed[3].column, 1);
    }

    #[test]
    fn nothing_to_place_needs_no_height() {
        let (placed, height) = place(&[], 2);

        assert!(placed.is_empty());
        assert_eq!(height, 0);
    }

    #[test]
    fn one_group_is_as_tall_as_itself_and_no_taller() {
        let (_, height) = place(&[137], 2);

        assert_eq!(height, 137);
    }

    #[test]
    fn asking_for_no_columns_still_places_everything() {
        let (placed, height) = place(&[10, 20], 0);

        assert_eq!(placed.len(), 2);
        assert!(placed.iter().all(|placement| placement.column == 0));
        assert_eq!(height, 10 + GUTTER + 20);
    }

    #[test]
    fn a_group_that_measures_negative_is_treated_as_empty() {
        let (placed, height) = place(&[-5, 20], 1);

        assert_eq!(placed[1].top, GUTTER);
        assert_eq!(height, GUTTER + 20);
    }
}
