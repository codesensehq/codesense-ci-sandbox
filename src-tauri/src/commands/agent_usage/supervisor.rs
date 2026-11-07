// SPDX-License-Identifier: MIT
use std::collections::{HashMap, VecDeque};

use super::types::{RefreshOutcome, RefreshState};

/// How many accounts' CLIs may be asked for usage at once.
pub const MAX_POLLS: usize = 4;

#[derive(Debug, Default)]
pub struct PollAdmission {
    running: HashMap<String, u64>,
    queued: VecDeque<(String, u64)>,
}

impl PollAdmission {
    pub fn request(&mut self, account_id: &str, generation: u64) -> RefreshOutcome {
        if self.running.contains_key(account_id) {
            return RefreshOutcome::Coalesced;
        }
        if self.running.len() < MAX_POLLS {
            self.running.insert(account_id.into(), generation);
            return RefreshOutcome::Started;
        }
        if !self.queued.iter().any(|(id, _)| id == account_id) {
            self.queued.push_back((account_id.into(), generation));
            RefreshOutcome::Queued
        } else {
            RefreshOutcome::Coalesced
        }
    }

    pub fn release(&mut self, account_id: &str) -> Option<(String, u64)> {
        self.running.remove(account_id);
        let next = self.queued.pop_front()?;
        self.running.insert(next.0.clone(), next.1);
        Some(next)
    }

    pub fn cancel(&mut self, account_id: &str) {
        // A running process continues to consume a slot until its worker observes
        // cancellation and calls `release`. Removing it here could admit a fifth
        // usage poll while the cancelled child is still shutting down.
        self.queued.retain(|(id, _)| id != account_id);
    }

    pub fn state(&self, account_id: &str) -> RefreshState {
        if self.running.contains_key(account_id) {
            RefreshState::Running
        } else if self.queued.iter().any(|(id, _)| id == account_id) {
            RefreshState::Queued
        } else {
            RefreshState::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_admits_a_fifth_and_coalesces_queue() {
        let mut admission = PollAdmission::default();
        for id in ["a", "b", "c", "d"] {
            assert_eq!(admission.request(id, 1), RefreshOutcome::Started);
        }
        assert_eq!(admission.request("e", 2), RefreshOutcome::Queued);
        assert_eq!(admission.request("e", 2), RefreshOutcome::Coalesced);
        assert_eq!(admission.release("a"), Some(("e".into(), 2)));
        assert_eq!(admission.state("e"), RefreshState::Running);
    }

    #[test]
    fn cancelling_running_work_does_not_release_its_slot_early() {
        let mut admission = PollAdmission::default();
        for id in ["a", "b", "c", "d"] {
            assert_eq!(admission.request(id, 1), RefreshOutcome::Started);
        }
        admission.cancel("a");
        assert_eq!(admission.request("e", 1), RefreshOutcome::Queued);
        assert_eq!(admission.release("a"), Some(("e".into(), 1)));
    }
}
