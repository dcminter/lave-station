//! Which tabs a "close several" command closes, and so whether it is offered at all.
//!
//! Positional arithmetic rather than widgets: the tab bar hands over how many tabs there
//! are, how many of them are pinned, and which one the menu was opened on.

/// The three commands the tab context menu offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every tab that can be closed, including the one the menu was opened on.
    All,
    /// Those before it.
    ToLeft,
    /// Those after it.
    ToRight,
}

impl Scope {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "Close All Tabs",
            Self::ToLeft => "Close All Tabs to the Left",
            Self::ToRight => "Close All Tabs to the Right",
        }
    }
}

/// The tabs `scope` closes, by position.
///
/// Pinned tabs are never among them: they are pinned precisely because there is nothing
/// sensible to close them to. A tab view keeps its pinned tabs at the front, so they are
/// the first `pinned` positions.
#[must_use]
pub fn closing(scope: Scope, tabs: usize, subject: usize, pinned: usize) -> Vec<usize> {
    let first = pinned.min(tabs);
    let subject = subject.min(tabs.saturating_sub(1));

    let range = match scope {
        Scope::All => first..tabs,
        Scope::ToLeft => first..subject.max(first),
        Scope::ToRight => subject.saturating_add(1).max(first)..tabs,
    };

    range.collect()
}

/// Whether the command is worth offering: a command that would close nothing is greyed
/// out rather than left to do nothing when chosen.
#[must_use]
pub fn is_offered(scope: Scope, tabs: usize, subject: usize, pinned: usize) -> bool {
    !closing(scope, tabs, subject, pinned).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The usual arrangement: one pinned tab and three others.
    const TABS: usize = 4;
    const PINNED: usize = 1;

    #[test]
    fn closing_them_all_spares_the_pinned_ones() {
        assert_eq!(closing(Scope::All, TABS, 2, PINNED), vec![1, 2, 3]);
        assert!(is_offered(Scope::All, TABS, 2, PINNED));
    }

    #[test]
    fn the_tabs_to_the_left_stop_at_the_pinned_ones() {
        assert_eq!(closing(Scope::ToLeft, TABS, 3, PINNED), vec![1, 2]);
        assert_eq!(closing(Scope::ToLeft, TABS, 2, PINNED), vec![1]);
    }

    #[test]
    fn nothing_but_a_pinned_tab_to_the_left_is_not_offered() {
        // The user's own example: the environment's tab is to the left of the first one
        // that can be closed, and it is not going anywhere.
        assert_eq!(closing(Scope::ToLeft, TABS, 1, PINNED), Vec::<usize>::new());
        assert!(!is_offered(Scope::ToLeft, TABS, 1, PINNED));
    }

    #[test]
    fn the_tabs_to_the_right_are_the_ones_after_it() {
        assert_eq!(closing(Scope::ToRight, TABS, 1, PINNED), vec![2, 3]);
        assert_eq!(closing(Scope::ToRight, TABS, 2, PINNED), vec![3]);
    }

    #[test]
    fn the_last_tab_has_nothing_to_its_right() {
        assert!(!is_offered(Scope::ToRight, TABS, 3, PINNED));
    }

    #[test]
    fn a_menu_on_a_pinned_tab_still_reaches_the_rest() {
        // Nothing to its left, since it is the leftmost; everything else to its right.
        assert!(!is_offered(Scope::ToLeft, TABS, 0, PINNED));
        assert_eq!(closing(Scope::ToRight, TABS, 0, PINNED), vec![1, 2, 3]);
    }

    #[test]
    fn a_bar_holding_nothing_but_pinned_tabs_offers_none_of_it() {
        for scope in [Scope::All, Scope::ToLeft, Scope::ToRight] {
            assert!(!is_offered(scope, 1, 0, 1), "{}", scope.label());
            assert!(!is_offered(scope, 0, 0, 0), "{}", scope.label());
        }
    }

    #[test]
    fn a_subject_past_the_end_is_treated_as_the_last_tab() {
        // Nothing should ask this, but a stale position must not produce a range that
        // closes tabs nobody pointed at.
        assert_eq!(
            closing(Scope::ToRight, TABS, 99, PINNED),
            Vec::<usize>::new()
        );
        assert_eq!(closing(Scope::ToLeft, TABS, 99, PINNED), vec![1, 2]);
    }

    #[test]
    fn more_pinned_tabs_than_there_are_closes_nothing() {
        assert_eq!(closing(Scope::All, 2, 0, 9), Vec::<usize>::new());
    }
}
