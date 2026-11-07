// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};

use crate::install::agent_account_store::AgentProvider;

pub const MAX_ACCOUNTS: usize = 32;
pub const MAX_WINDOWS: usize = 8;
pub const MAX_ADAPTER_MESSAGE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAvailability {
    Unknown,
    Available,
    Exhausted,
    AuthRequired,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageFreshness {
    AwaitingFirstUse,
    Live,
    Stale,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RefreshState {
    #[default]
    Idle,
    Queued,
    Running,
    Backoff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshOutcome {
    Started,
    Coalesced,
    Queued,
    Cached,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AllowanceWindow {
    pub key: String,
    pub label: String,
    pub used_percentage: Option<f64>,
    pub remaining_percentage: Option<f64>,
    pub resets_at: Option<i64>,
    pub required: bool,
}

impl AllowanceWindow {
    pub fn new(
        key: impl Into<String>,
        label: impl Into<String>,
        used_percentage: f64,
        resets_at: Option<i64>,
        required: bool,
    ) -> Result<Self, String> {
        if !used_percentage.is_finite() || !(0.0..=100.0).contains(&used_percentage) {
            return Err("used percentage must be finite and within 0..=100".into());
        }
        let key = key.into();
        let label = label.into();
        if key.is_empty() || key.len() > 64 || label.is_empty() || label.len() > 64 {
            return Err("usage window key/label must be 1..=64 bytes".into());
        }
        Ok(Self {
            key,
            label,
            used_percentage: Some(used_percentage),
            remaining_percentage: Some(100.0 - used_percentage),
            resets_at,
            required,
        })
    }
    pub fn unknown(
        key: impl Into<String>,
        label: impl Into<String>,
        required: bool,
    ) -> Result<Self, String> {
        let mut window = Self::new(key, label, 0.0, None, required)?;
        window.used_percentage = None;
        window.remaining_percentage = None;
        Ok(window)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentUsageSnapshot {
    pub account_profile_id: String,
    pub provider: AgentProvider,
    pub availability: AgentAvailability,
    pub allowance_windows: Vec<AllowanceWindow>,
    pub sequence: u64,
    pub usage_received_at: i64,
    pub source: String,
    pub usage_freshness: UsageFreshness,
    #[serde(default)]
    pub refresh_state: RefreshState,
}

impl AgentUsageSnapshot {
    pub fn normalize(mut self) -> Result<Self, String> {
        if self.allowance_windows.len() > MAX_WINDOWS {
            return Err(format!("at most {MAX_WINDOWS} usage windows are allowed"));
        }
        self.allowance_windows.sort_by(|a, b| a.key.cmp(&b.key));
        if self
            .allowance_windows
            .windows(2)
            .any(|pair| pair[0].key == pair[1].key)
        {
            return Err("duplicate usage window key".into());
        }
        let required = self
            .allowance_windows
            .iter()
            .filter(|window| window.required)
            .collect::<Vec<_>>();
        self.availability = if required.is_empty() {
            AgentAvailability::Unknown
        } else if required
            .iter()
            .any(|window| window.used_percentage.is_some_and(|used| used >= 100.0))
        {
            AgentAvailability::Exhausted
        } else if required
            .iter()
            .any(|window| window.used_percentage.is_none())
        {
            AgentAvailability::Unknown
        } else {
            AgentAvailability::Available
        };
        Ok(self)
    }

    pub fn content_eq(&self, other: &Self) -> bool {
        self.account_profile_id == other.account_profile_id
            && self.provider == other.provider
            && self.availability == other.availability
            && self.allowance_windows == other.allowance_windows
            && self.source == other.source
            && self.usage_freshness == other.usage_freshness
            && self.refresh_state == other.refresh_state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(windows: Vec<AllowanceWindow>) -> AgentUsageSnapshot {
        AgentUsageSnapshot {
            account_profile_id: "a".into(),
            provider: AgentProvider::Claude,
            availability: AgentAvailability::Unknown,
            allowance_windows: windows,
            sequence: 0,
            usage_received_at: 100,
            source: "test".into(),
            usage_freshness: UsageFreshness::Live,
            refresh_state: RefreshState::Idle,
        }
    }

    #[test]
    fn any_required_exhausted_window_blocks_availability() {
        let value = snapshot(vec![
            AllowanceWindow::new("five_hour", "5 hour", 100.0, None, true).unwrap(),
            AllowanceWindow::new("seven_day", "7 day", 40.0, None, true).unwrap(),
        ])
        .normalize()
        .unwrap();
        assert_eq!(value.availability, AgentAvailability::Exhausted);
        assert_eq!(value.allowance_windows[0].remaining_percentage, Some(0.0));
    }

    #[test]
    fn receipt_metadata_does_not_defeat_deduplication() {
        let first = snapshot(vec![AllowanceWindow::new(
            "five_hour",
            "5 hour",
            10.0,
            None,
            true,
        )
        .unwrap()])
        .normalize()
        .unwrap();
        let mut second = first.clone();
        second.sequence = 99;
        second.usage_received_at = 999;
        assert!(first.content_eq(&second));
    }

    #[test]
    fn renderer_contract_uses_camel_case_fields() {
        let value = serde_json::to_value(snapshot(vec![AllowanceWindow::new(
            "five_hour",
            "5 hour",
            10.0,
            Some(200),
            true,
        )
        .unwrap()]))
        .unwrap();
        assert!(value.get("accountProfileId").is_some());
        assert!(value.get("allowanceWindows").is_some());
        assert!(value.get("usageReceivedAt").is_some());
        assert!(value.get("account_profile_id").is_none());
        let window = &value["allowanceWindows"][0];
        assert!(window.get("remainingPercentage").is_some());
        assert!(window.get("resetsAt").is_some());
    }
}
