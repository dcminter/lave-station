//! The activity monitor's state.
//!
//! A pure reducer, so reconnection behaviour is asserted in tests rather than observed
//! by watching the panel. `docs/container_daemon_integration.md` §6: subscribe to
//! events, reconnect with backoff, and replay the gap with `since`.

use std::collections::VecDeque;
use std::time::Duration;

use crate::engine::{EngineError, EngineEvent};

/// How many recent events the indicator's menu remembers.
pub const MAX_LOG_ENTRIES: usize = 20;

const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);

/// Doubling backoff, capped. Deterministic: no jitter, so it can be asserted.
#[must_use]
pub fn backoff(attempt: u32) -> Duration {
    let steps = attempt.saturating_sub(1).min(16);
    let seconds = BACKOFF_BASE.as_secs().saturating_mul(1u64 << steps);
    Duration::from_secs(seconds).min(BACKOFF_CAP)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityState {
    /// Establishing the connection, including the first attempt.
    Connecting,
    Connected,
    /// Lost, but worth retrying.
    Reconnecting {
        attempt: u32,
        delay: Duration,
    },
    /// Lost in a way retrying will not fix.
    Failed {
        reason: String,
        hint: Option<String>,
    },
}

impl ActivityState {
    #[must_use]
    pub fn is_connected(&self) -> bool {
        matches!(self, ActivityState::Connected)
    }
}

/// One line in the indicator's recent-activity list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    pub time: i64,
    pub text: String,
}

/// What the runtime tells the monitor.
#[derive(Debug, Clone)]
pub enum Signal {
    /// The event stream is open.
    Connected {
        at: i64,
    },
    Observed(EngineEvent),
    Lost(EngineError),
    /// The backoff wait has finished.
    RetryElapsed,
}

/// What the runtime should do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Open, or reopen, the event stream.
    Connect,
    /// Reload the listings.
    Refresh,
    /// Wait, then send [`Signal::RetryElapsed`].
    WaitThen(Duration),
    /// Stop trying.
    Stop,
}

#[derive(Debug, Clone)]
pub struct Activity {
    state: ActivityState,
    log: VecDeque<ActivityEntry>,
    attempt: u32,
    since: Option<i64>,
    last_event_at: Option<i64>,
}

impl Default for Activity {
    fn default() -> Self {
        Self::new()
    }
}

impl Activity {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: ActivityState::Connecting,
            log: VecDeque::new(),
            attempt: 0,
            since: None,
            last_event_at: None,
        }
    }

    #[must_use]
    pub fn state(&self) -> &ActivityState {
        &self.state
    }

    /// Most recent first.
    #[must_use]
    pub fn log(&self) -> &VecDeque<ActivityEntry> {
        &self.log
    }

    #[must_use]
    pub fn last_event_at(&self) -> Option<i64> {
        self.last_event_at
    }

    /// Where a reconnect should resume from, so events in the gap are not lost.
    #[must_use]
    pub fn since(&self) -> Option<i64> {
        self.since
    }

    pub fn apply(&mut self, signal: Signal) -> Vec<Effect> {
        match signal {
            Signal::Connected { at } => {
                self.state = ActivityState::Connected;
                self.attempt = 0;
                self.since.get_or_insert(at);
                vec![Effect::Refresh]
            }
            Signal::Observed(event) => {
                self.record(&event);
                if event.affects_listing() {
                    vec![Effect::Refresh]
                } else {
                    Vec::new()
                }
            }
            Signal::Lost(error) => {
                if error.is_transient() {
                    self.attempt = self.attempt.saturating_add(1);
                    let delay = backoff(self.attempt);
                    self.state = ActivityState::Reconnecting {
                        attempt: self.attempt,
                        delay,
                    };
                    vec![Effect::WaitThen(delay)]
                } else {
                    self.state = ActivityState::Failed {
                        reason: error.to_string(),
                        hint: error.hint().map(str::to_owned),
                    };
                    vec![Effect::Stop]
                }
            }
            Signal::RetryElapsed => {
                self.state = ActivityState::Connecting;
                vec![Effect::Connect]
            }
        }
    }

    fn record(&mut self, event: &EngineEvent) {
        self.last_event_at = Some(event.time);
        // Resume from the last event seen, not from the original connection.
        self.since = Some(event.time);

        self.log.push_front(ActivityEntry {
            time: event.time,
            text: describe(event),
        });
        self.log.truncate(MAX_LOG_ENTRIES);
    }
}

/// One event, as a line of text.
#[must_use]
pub fn describe(event: &EngineEvent) -> String {
    let subject = event
        .actor_name
        .clone()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| crate::model::format::short_id(&event.actor_id));

    let kind = if event.kind.is_empty() {
        "event"
    } else {
        event.kind.as_str()
    };
    let action = if event.action.is_empty() {
        "changed"
    } else {
        event.action.as_str()
    };

    if subject.is_empty() {
        format!("{kind} {action}")
    } else {
        format!("{kind} {action}: {subject}")
    }
}

#[cfg(test)]
mod tests {
    // expect is fine in tests; a failed assumption should abort the test.
    #![allow(clippy::expect_used)]

    use super::*;
    use std::io::ErrorKind;
    use std::path::Path;

    fn event(kind: &str, action: &str, name: &str, time: i64) -> EngineEvent {
        EngineEvent {
            kind: kind.to_owned(),
            action: action.to_owned(),
            actor_id: "13ef39df585fa5ea8df9325dffdc7c18".to_owned(),
            actor_name: if name.is_empty() {
                None
            } else {
                Some(name.to_owned())
            },
            time,
        }
    }

    fn transient() -> EngineError {
        EngineError::unreachable(
            Path::new("/var/run/docker.sock"),
            ErrorKind::ConnectionRefused,
        )
    }

    fn permanent() -> EngineError {
        EngineError::Protocol("unparseable frame".to_owned())
    }

    #[test]
    fn a_fresh_monitor_is_connecting_and_has_nothing_to_show() {
        let activity = Activity::new();

        assert_eq!(*activity.state(), ActivityState::Connecting);
        assert!(activity.log().is_empty());
        assert_eq!(activity.since(), None);
        assert_eq!(activity.last_event_at(), None);
    }

    #[test]
    fn connecting_triggers_a_refresh_of_the_listings() {
        let mut activity = Activity::new();

        let effects = activity.apply(Signal::Connected { at: 1000 });

        assert_eq!(effects, vec![Effect::Refresh]);
        assert!(activity.state().is_connected());
        assert_eq!(activity.since(), Some(1000));
    }

    #[test]
    fn container_and_image_events_refresh_the_tree() {
        let mut activity = Activity::new();
        activity.apply(Signal::Connected { at: 1000 });

        for kind in ["container", "image"] {
            let effects = activity.apply(Signal::Observed(event(kind, "start", "web", 1001)));
            assert_eq!(effects, vec![Effect::Refresh], "{kind} should refresh");
        }
    }

    #[test]
    fn other_events_are_logged_without_reloading_anything() {
        let mut activity = Activity::new();
        activity.apply(Signal::Connected { at: 1000 });

        let effects = activity.apply(Signal::Observed(event("network", "connect", "", 1001)));

        assert!(effects.is_empty());
        assert_eq!(activity.log().len(), 1);
    }

    #[test]
    fn the_log_keeps_the_most_recent_entries_newest_first() {
        let mut activity = Activity::new();
        activity.apply(Signal::Connected { at: 0 });

        for index in 0..(MAX_LOG_ENTRIES + 5) {
            let time = i64::try_from(index).expect("small");
            activity.apply(Signal::Observed(event("container", "start", "web", time)));
        }

        assert_eq!(activity.log().len(), MAX_LOG_ENTRIES);
        let newest = activity.log().front().expect("an entry");
        let oldest = activity.log().back().expect("an entry");
        assert!(newest.time > oldest.time);
        assert_eq!(
            newest.time,
            i64::try_from(MAX_LOG_ENTRIES + 4).expect("small")
        );
    }

    #[test]
    fn events_advance_the_replay_point_so_a_reconnect_loses_nothing() {
        let mut activity = Activity::new();
        activity.apply(Signal::Connected { at: 1000 });
        activity.apply(Signal::Observed(event("container", "die", "web", 1500)));

        assert_eq!(activity.since(), Some(1500));
        assert_eq!(activity.last_event_at(), Some(1500));

        activity.apply(Signal::Lost(transient()));
        activity.apply(Signal::RetryElapsed);
        activity.apply(Signal::Connected { at: 2000 });

        // Still 1500: the gap between 1500 and reconnection must be replayed.
        assert_eq!(activity.since(), Some(1500));
    }

    #[test]
    fn a_transient_loss_schedules_a_retry_with_doubling_backoff() {
        let mut activity = Activity::new();
        let expected = [1, 2, 4, 8, 16, 30, 30, 30];

        for (index, seconds) in expected.into_iter().enumerate() {
            let effects = activity.apply(Signal::Lost(transient()));
            assert_eq!(
                effects,
                vec![Effect::WaitThen(Duration::from_secs(seconds))],
                "attempt {} should wait {seconds}s",
                index + 1
            );
            activity.apply(Signal::RetryElapsed);
        }
    }

    #[test]
    fn the_backoff_is_capped_and_never_zero() {
        assert_eq!(backoff(0), Duration::from_secs(1));
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(6), BACKOFF_CAP);
        assert_eq!(backoff(u32::MAX), BACKOFF_CAP);
    }

    #[test]
    fn a_successful_reconnect_resets_the_backoff() {
        let mut activity = Activity::new();
        for _ in 0..4 {
            activity.apply(Signal::Lost(transient()));
            activity.apply(Signal::RetryElapsed);
        }
        activity.apply(Signal::Connected { at: 5000 });

        let effects = activity.apply(Signal::Lost(transient()));

        assert_eq!(effects, vec![Effect::WaitThen(Duration::from_secs(1))]);
    }

    #[test]
    fn retrying_moves_back_to_connecting_and_asks_for_a_connection() {
        let mut activity = Activity::new();
        activity.apply(Signal::Lost(transient()));

        let effects = activity.apply(Signal::RetryElapsed);

        assert_eq!(effects, vec![Effect::Connect]);
        assert_eq!(*activity.state(), ActivityState::Connecting);
    }

    #[test]
    fn reconnecting_state_reports_the_attempt_and_the_wait() {
        let mut activity = Activity::new();
        activity.apply(Signal::Lost(transient()));
        activity.apply(Signal::RetryElapsed);
        activity.apply(Signal::Lost(transient()));

        assert_eq!(
            *activity.state(),
            ActivityState::Reconnecting {
                attempt: 2,
                delay: Duration::from_secs(2)
            }
        );
    }

    #[test]
    fn an_unrecoverable_loss_stops_rather_than_retrying_forever() {
        let mut activity = Activity::new();

        let effects = activity.apply(Signal::Lost(permanent()));

        assert_eq!(effects, vec![Effect::Stop]);
        assert!(matches!(activity.state(), ActivityState::Failed { .. }));
    }

    #[test]
    fn a_failure_carries_the_hint_forward_to_the_user() {
        let mut activity = Activity::new();
        let denied = EngineError::Api {
            status: 400,
            message: "bad request".to_owned(),
        };

        activity.apply(Signal::Lost(denied));

        let ActivityState::Failed { reason, .. } = activity.state() else {
            panic!("expected Failed, got {:?}", activity.state());
        };
        assert_eq!(reason, "bad request");
    }

    #[test]
    fn events_are_described_using_the_name_when_there_is_one() {
        assert_eq!(
            describe(&event("container", "start", "web", 1)),
            "container start: web"
        );
    }

    #[test]
    fn nameless_events_fall_back_to_the_short_id() {
        assert_eq!(
            describe(&event("container", "destroy", "", 1)),
            "container destroy: 13ef39df585f"
        );
    }

    #[test]
    fn incomplete_events_still_describe_themselves() {
        let bare = EngineEvent::default();

        assert_eq!(describe(&bare), "event changed");
    }
}
