// SPDX-License-Identifier: MIT
//! The subagents an invocation's agent has running: a set of the CLI's own subagent ids,
//! not a counter, so a resumed subagent's repeated start changes nothing and a stop no
//! start matched (a CLI's internal agents) is ignored.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// For a CLI that has never listed its running subagents, one nothing was heard from for
/// this long is dropped, so a lost stop can't keep its agent busy for ever. Long enough
/// for a subagent's longest quiet tool call (a test suite, a build).
pub(super) const STALE: Duration = Duration::from_secs(6 * 60 * 60);

/// The most subagents an invocation holds open, and remembers as finished.
const CAPACITY: usize = codesense_host_proto::activity::MAX_SUBAGENTS;

#[derive(Default)]
pub(super) struct OpenSubagents {
    /// Each open subagent, with when it was last heard from.
    open: HashMap<String, Instant>,
    /// The latest finished ones: a late hook of theirs doesn't open them again.
    finished: VecDeque<String>,
    /// Its CLI listed the ones still running at a turn's end, which corrects a lost stop:
    /// none times out any more.
    listed: bool,
}

impl OpenSubagents {
    pub(super) fn count(&self) -> u32 {
        self.open.len() as u32
    }

    /// `id` started, or resumed.
    pub(super) fn start(&mut self, id: &str, now: Instant) {
        self.finished.retain(|finished| finished != id);
        self.open_at(id, now);
    }

    /// `id` finished.
    pub(super) fn finish(&mut self, id: &str) {
        self.open.remove(id);
        self.remember_finished(id.to_owned());
    }

    /// A hook of `id`'s own, which opens it when no start was seen (a start's hook can
    /// fail) and it has not finished. Whether it is open now.
    pub(super) fn heard(&mut self, id: &str, now: Instant) -> bool {
        if let Some(at) = self.open.get_mut(id) {
            *at = now;
            return true;
        }
        !self.finished.iter().any(|finished| finished == id) && self.open_at(id, now)
    }

    /// The CLI says only `running` still run; the others finished unseen. Nothing is
    /// opened from it: a stop already counted can't come back.
    pub(super) fn reconcile(&mut self, running: &[String]) {
        self.listed = true;
        let stopped: Vec<String> = self
            .open
            .keys()
            .filter(|id| !running.contains(id))
            .cloned()
            .collect();
        for id in stopped {
            self.finish(&id);
        }
    }

    /// Drops the subagents unheard from for [`STALE`], unless its CLI lists them; one
    /// heard from again reopens. Whether the count changed.
    pub(super) fn expire(&mut self, now: Instant) -> bool {
        if self.listed {
            return false;
        }
        let before = self.open.len();
        self.open
            .retain(|_, at| now.saturating_duration_since(*at) < STALE);
        self.open.len() != before
    }

    /// Its agent exited, ended its session or started a new context: none runs on, and
    /// a late hook of one opens nothing.
    pub(super) fn clear(&mut self) {
        let open: Vec<String> = self.open.drain().map(|(id, _)| id).collect();
        for id in open {
            self.remember_finished(id);
        }
    }

    fn remember_finished(&mut self, id: String) {
        if self.finished.contains(&id) {
            return;
        }
        if self.finished.len() == CAPACITY {
            self.finished.pop_front();
        }
        self.finished.push_back(id);
    }

    fn open_at(&mut self, id: &str, now: Instant) -> bool {
        if self.open.len() >= CAPACITY && !self.open.contains_key(id) {
            return false;
        }
        self.open.insert(id.to_owned(), now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resumed_start_and_a_stray_or_late_stop_leave_the_count_right() {
        let now = Instant::now();
        let mut subagents = OpenSubagents::default();
        subagents.start("a", now);
        subagents.start("a", now);
        subagents.start("b", now);
        assert_eq!(subagents.count(), 2);
        // An internal agent's stop matches no start.
        subagents.finish("internal");
        assert_eq!(subagents.count(), 2);
        subagents.finish("a");
        // A late hook of a finished subagent does not reopen it; its resume does.
        assert!(!subagents.heard("a", now));
        subagents.start("a", now);
        assert_eq!(subagents.count(), 2);
        subagents.finish("a");
        // A hook of one whose start was missed opens it.
        assert!(subagents.heard("c", now));
        assert!(subagents.heard("c", now));
        assert_eq!(subagents.count(), 2);
    }

    #[test]
    fn the_bound_expires_the_unheard_until_a_list_corrects_lost_stops() {
        let now = Instant::now();
        let mut subagents = OpenSubagents::default();
        subagents.start("b", now);
        subagents.start("c", now + STALE / 2);
        assert!(!subagents.expire(now + STALE - Duration::from_secs(1)));
        assert!(subagents.expire(now + STALE));
        assert_eq!(subagents.count(), 1);
        // One dropped by the bound still runs if it is heard from again.
        assert!(subagents.heard("b", now + STALE));
        // Once its CLI lists them, one it no longer lists finished for good, and the
        // rest never time out.
        subagents.reconcile(&["b".into(), "z".into()]);
        assert_eq!(subagents.count(), 1);
        assert!(!subagents.heard("c", now + STALE));
        assert!(!subagents.expire(now + 3 * STALE));
        assert_eq!(subagents.count(), 1);
        subagents.clear();
        assert_eq!(subagents.count(), 0);
        assert!(!subagents.heard("b", now + STALE));
    }

    #[test]
    fn it_holds_at_most_its_capacity_and_a_repeated_stop_forgets_nothing() {
        let now = Instant::now();
        let mut subagents = OpenSubagents::default();
        for i in 0..CAPACITY + 5 {
            subagents.start(&format!("a{i}"), now);
        }
        assert_eq!(subagents.count() as usize, CAPACITY);
        subagents.finish("a0");
        subagents.start("new", now);
        assert_eq!(subagents.count() as usize, CAPACITY);
        for i in 1..CAPACITY {
            subagents.finish(&format!("a{i}"));
        }
        // The finished memory is full; stopping "a1" again must not evict "a0".
        subagents.finish("a1");
        assert!(!subagents.heard("a0", now));
    }
}
