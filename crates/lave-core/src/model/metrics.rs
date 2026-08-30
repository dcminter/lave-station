//! Live resource figures, kept beside the listings rather than inside them.
//!
//! A listing describes what exists and changes only when something is created or
//! removed; memory changes constantly. Keeping the two apart is what lets the samples
//! be refreshed on their own timer without rebuilding the tree.

use std::collections::BTreeMap;

use crate::engine::{ContainerStats, ContainerSummary};

/// The most recent memory sample for each container, by ID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatsIndex {
    by_id: BTreeMap<String, ContainerStats>,
}

/// Memory across a set of containers, with how much of that set was actually measured.
///
/// The count matters: a total over three of five running containers is not the
/// machine's figure, and a page that showed it as one would be lying.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryTotal {
    pub bytes: i64,
    pub measured: usize,
    pub unmeasured: usize,
}

impl MemoryTotal {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.measured == 0
    }

    /// True when something was left out, and so the total is a floor rather than a sum.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        self.unmeasured > 0
    }
}

impl StatsIndex {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, stats: ContainerStats) {
        self.by_id.insert(stats.id.clone(), stats);
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ContainerStats> {
        self.by_id.get(id)
    }

    /// Bytes in use by one container, or `None` when it has not been measured.
    #[must_use]
    pub fn memory(&self, id: &str) -> Option<i64> {
        self.get(id)
            .filter(|stats| stats.has_memory())
            .map(|stats| stats.memory_usage)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Forget the containers that are no longer there, so a long run does not
    /// accumulate samples for things that have been removed.
    pub fn retain_known(&mut self, containers: &[ContainerSummary]) {
        self.by_id
            .retain(|id, _| containers.iter().any(|container| &container.id == id));
    }

    /// Memory across the containers that are executing. Stopped ones are not counted
    /// as unmeasured: they are holding nothing, and there is nothing to measure.
    #[must_use]
    pub fn running_total(&self, containers: &[ContainerSummary]) -> MemoryTotal {
        let mut total = MemoryTotal::default();

        for container in containers.iter().filter(|c| c.state.is_active()) {
            match self.memory(&container.id) {
                Some(bytes) => {
                    total.bytes += bytes;
                    total.measured += 1;
                }
                None => total.unmeasured += 1,
            }
        }

        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ContainerState;

    fn container(id: &str, state: ContainerState) -> ContainerSummary {
        ContainerSummary {
            id: id.to_owned(),
            state,
            ..ContainerSummary::default()
        }
    }

    fn sample(id: &str, usage: i64) -> ContainerStats {
        ContainerStats {
            id: id.to_owned(),
            memory_usage: usage,
            memory_limit: 8_000_000_000,
        }
    }

    #[test]
    fn a_sample_is_found_by_the_container_it_describes() {
        let mut index = StatsIndex::new();
        index.insert(sample("web", 1_000));

        assert_eq!(index.memory("web"), Some(1_000));
        assert_eq!(index.memory("db"), None);
    }

    #[test]
    fn a_later_sample_replaces_the_one_before_it() {
        let mut index = StatsIndex::new();
        index.insert(sample("web", 1_000));
        index.insert(sample("web", 2_000));

        assert_eq!(index.len(), 1);
        assert_eq!(index.memory("web"), Some(2_000));
    }

    #[test]
    fn a_sample_the_daemon_could_not_measure_reads_as_absent() {
        let mut index = StatsIndex::new();
        index.insert(ContainerStats {
            id: "web".to_owned(),
            memory_usage: -1,
            memory_limit: -1,
        });

        assert!(index.get("web").is_some(), "the sample itself is held");
        assert_eq!(index.memory("web"), None, "but it has no figure to give");
    }

    #[test]
    fn the_running_total_adds_up_the_containers_that_are_executing() {
        let mut index = StatsIndex::new();
        index.insert(sample("web", 1_000));
        index.insert(sample("api", 2_500));
        // A sample left over from before this one stopped must not be counted.
        index.insert(sample("old", 9_000));

        let total = index.running_total(&[
            container("web", ContainerState::Running),
            container("api", ContainerState::Paused),
            container("old", ContainerState::Exited),
        ]);

        assert_eq!(total.bytes, 3_500);
        assert_eq!(total.measured, 2);
        assert_eq!(total.unmeasured, 0);
        assert!(!total.is_partial());
    }

    #[test]
    fn a_running_container_with_no_sample_makes_the_total_a_floor() {
        let mut index = StatsIndex::new();
        index.insert(sample("web", 1_000));

        let total = index.running_total(&[
            container("web", ContainerState::Running),
            container("api", ContainerState::Running),
        ]);

        assert_eq!(total.bytes, 1_000);
        assert_eq!(total.unmeasured, 1);
        assert!(total.is_partial(), "one of the two was never measured");
    }

    #[test]
    fn a_machine_with_nothing_running_has_an_empty_total_rather_than_a_zero_one() {
        let index = StatsIndex::new();
        let total = index.running_total(&[container("old", ContainerState::Exited)]);

        assert!(total.is_empty());
        assert!(!total.is_partial());
    }

    #[test]
    fn samples_for_containers_that_have_gone_are_dropped() {
        let mut index = StatsIndex::new();
        index.insert(sample("web", 1_000));
        index.insert(sample("gone", 2_000));

        index.retain_known(&[container("web", ContainerState::Running)]);

        assert_eq!(index.len(), 1);
        assert_eq!(index.memory("gone"), None);
    }
}
