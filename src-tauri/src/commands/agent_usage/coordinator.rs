// SPDX-License-Identifier: MIT
use std::collections::HashMap;

use super::types::{AgentUsageSnapshot, MAX_ACCOUNTS};

#[derive(Debug, Default)]
pub struct AgentUsageCoordinator {
    latest: HashMap<String, AgentUsageSnapshot>,
}

impl AgentUsageCoordinator {
    pub fn restore(&mut self, mut snapshot: AgentUsageSnapshot) -> Result<(), String> {
        if snapshot.account_profile_id.is_empty() {
            return Err("account profile id is required".into());
        }
        snapshot = snapshot.normalize()?;
        snapshot.usage_freshness = super::types::UsageFreshness::Stale;
        self.latest
            .entry(snapshot.account_profile_id.clone())
            .or_insert(snapshot);
        Ok(())
    }

    pub fn remove(&mut self, account_profile_id: &str) {
        self.latest.remove(account_profile_id);
    }

    /// Preserve the last known allowance windows while making a failed refresh
    /// explicit to consumers. A later successful ingest replaces this state.
    pub fn mark_refresh_failure(
        &mut self,
        account_profile_id: &str,
        provider: crate::install::agent_account_store::AgentProvider,
        now_epoch: i64,
    ) -> Option<AgentUsageSnapshot> {
        if !self.latest.contains_key(account_profile_id) {
            if self.latest.len() >= MAX_ACCOUNTS {
                return None;
            }
            let snapshot = AgentUsageSnapshot {
                account_profile_id: account_profile_id.to_string(),
                provider,
                availability: super::types::AgentAvailability::Error,
                allowance_windows: Vec::new(),
                sequence: 1,
                usage_received_at: now_epoch,
                source: "app_server".into(),
                usage_freshness: super::types::UsageFreshness::Stale,
                refresh_state: super::types::RefreshState::Backoff,
            };
            self.latest
                .insert(account_profile_id.to_string(), snapshot.clone());
            return Some(snapshot);
        }
        let snapshot = self.latest.get_mut(account_profile_id)?;
        if snapshot.refresh_state == super::types::RefreshState::Backoff
            && snapshot.availability == super::types::AgentAvailability::Error
        {
            return None;
        }
        snapshot.refresh_state = super::types::RefreshState::Backoff;
        snapshot.availability = super::types::AgentAvailability::Error;
        snapshot.sequence = snapshot.sequence.saturating_add(1);
        Some(snapshot.clone())
    }

    pub fn ingest(
        &mut self,
        mut snapshot: AgentUsageSnapshot,
        now_epoch: i64,
    ) -> Result<Option<&AgentUsageSnapshot>, String> {
        if snapshot.account_profile_id.is_empty() {
            return Err("account profile id is required".into());
        }
        if !self.latest.contains_key(&snapshot.account_profile_id)
            && self.latest.len() >= MAX_ACCOUNTS
        {
            return Err(format!("at most {MAX_ACCOUNTS} accounts may publish usage"));
        }
        for window in &snapshot.allowance_windows {
            if let Some(reset) = window.resets_at {
                if reset < now_epoch - 300 || reset > now_epoch + 31 * 24 * 60 * 60 {
                    return Err("usage reset timestamp is implausible".into());
                }
            }
        }
        snapshot = snapshot.normalize()?;
        if let Some(previous) = self.latest.get_mut(&snapshot.account_profile_id) {
            if previous.content_eq(&snapshot) {
                // A semantically identical, validated sample is still evidence that
                // the cached value is fresh. Preserve its sequence and suppress a
                // redundant publication while advancing only receipt metadata.
                previous.usage_received_at = snapshot.usage_received_at;
                return Ok(None);
            }
        }
        snapshot.sequence = self
            .latest
            .get(&snapshot.account_profile_id)
            .map_or(1, |previous| previous.sequence.saturating_add(1));
        let id = snapshot.account_profile_id.clone();
        self.latest.insert(id.clone(), snapshot);
        Ok(self.latest.get(&id))
    }

    #[cfg(test)]
    pub fn get(&self, account_profile_id: &str) -> Option<&AgentUsageSnapshot> {
        self.latest.get(account_profile_id)
    }

    pub fn all(&self) -> Vec<AgentUsageSnapshot> {
        let mut values = self.latest.values().cloned().collect::<Vec<_>>();
        values.sort_by(|a, b| a.account_profile_id.cmp(&b.account_profile_id));
        values
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::agent_usage::types::{
        AgentAvailability, AllowanceWindow, RefreshState, UsageFreshness,
    };
    use crate::install::agent_account_store::AgentProvider;

    fn snapshot(used: f64, reset: Option<i64>) -> AgentUsageSnapshot {
        AgentUsageSnapshot {
            account_profile_id: "one".into(),
            provider: AgentProvider::Codex,
            availability: AgentAvailability::Unknown,
            allowance_windows: vec![
                AllowanceWindow::new("primary", "Primary", used, reset, true).unwrap(),
            ],
            sequence: 0,
            usage_received_at: 100,
            source: "app_server".into(),
            usage_freshness: UsageFreshness::Live,
            refresh_state: RefreshState::Idle,
        }
    }

    #[test]
    fn assigns_sequence_and_keeps_last_good_on_rejection() {
        let mut coordinator = AgentUsageCoordinator::default();
        assert_eq!(
            coordinator
                .ingest(snapshot(10.0, Some(200)), 100)
                .unwrap()
                .unwrap()
                .sequence,
            1
        );
        assert!(coordinator
            .ingest(snapshot(10.0, Some(200)), 100)
            .unwrap()
            .is_none());
        assert!(coordinator
            .ingest(snapshot(20.0, Some(4_000_000)), 100)
            .is_err());
        assert_eq!(
            coordinator.get("one").unwrap().allowance_windows[0].used_percentage,
            Some(10.0)
        );
    }

    #[test]
    fn identical_sample_advances_freshness_without_publication() {
        let mut coordinator = AgentUsageCoordinator::default();
        coordinator.ingest(snapshot(10.0, Some(200)), 100).unwrap();
        let mut repeated = snapshot(10.0, Some(200));
        repeated.usage_received_at = 150;

        assert!(coordinator.ingest(repeated, 150).unwrap().is_none());
        let latest = coordinator.get("one").unwrap();
        assert_eq!(latest.sequence, 1);
        assert_eq!(latest.usage_received_at, 150);
    }

    #[test]
    fn refresh_failure_preserves_last_good_windows_and_advances_sequence_once() {
        let mut coordinator = AgentUsageCoordinator::default();
        coordinator.ingest(snapshot(10.0, Some(200)), 100).unwrap();

        let failed = coordinator
            .mark_refresh_failure("one", AgentProvider::Codex, 100)
            .unwrap();
        assert_eq!(failed.availability, AgentAvailability::Error);
        assert_eq!(failed.refresh_state, RefreshState::Backoff);
        assert_eq!(failed.sequence, 2);
        assert_eq!(failed.allowance_windows[0].used_percentage, Some(10.0));
        assert!(coordinator
            .mark_refresh_failure("one", AgentProvider::Codex, 101)
            .is_none());
    }

    #[test]
    fn initial_refresh_failure_is_visible_without_inventing_allowance_data() {
        let mut coordinator = AgentUsageCoordinator::default();
        let failed = coordinator
            .mark_refresh_failure("one", AgentProvider::Codex, 100)
            .unwrap();
        assert_eq!(failed.availability, AgentAvailability::Error);
        assert!(failed.allowance_windows.is_empty());
        assert_eq!(failed.usage_freshness, UsageFreshness::Stale);
        assert!(coordinator
            .mark_refresh_failure("one", AgentProvider::Codex, 101)
            .is_none());
    }
}
