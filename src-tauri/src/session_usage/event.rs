// SPDX-License-Identifier: MIT
//! The one shape every agent CLI's usage is reduced to. Each source, whether a telemetry mapper or
//! a database reader, produces these, and the store and reports only ever see these.

use codesense_pty_core::agent_cli::AgentCli;
use serde::Serialize;

/// Token counts for one call. `cache_read` and `cache_write` are billed at their own rates, and
/// `input` never includes them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// The part of `cache_write` written to the one-hour cache, which bills higher than the
    /// five-minute cache. Kept for pricing only; reports show `cache_write`, the total.
    #[serde(skip)]
    pub cache_write_1h: u64,
}

impl Tokens {
    pub fn add(&mut self, other: &Tokens) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
        self.cache_write = self.cache_write.saturating_add(other.cache_write);
        self.cache_write_1h = self.cache_write_1h.saturating_add(other.cache_write_1h);
    }
}

/// One model call's usage, as one CLI reported it.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageEvent {
    pub cli: AgentCli,
    /// Unique within `cli`: the CLI's own request id, or a stable key from its record.
    pub event_id: String,
    pub session_id: String,
    pub at_ms: i64,
    pub cwd: Option<String>,
    pub model: String,
    pub tokens: Tokens,
    /// The cost the CLI itself reported, when it reports one. Preferred over our rate table.
    pub reported_cost_usd: Option<f64>,
}
