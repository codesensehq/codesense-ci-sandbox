// SPDX-License-Identifier: MIT
//! The one cost calculation for a session's usage rows. `report_with` (per session and its
//! `costUsd`), `session_read` (a session's `cost`) and the story totals index (a session's
//! contribution) all add their rows here, so the three can never disagree.
use super::{db_err, UsageStore};
use rusqlite::params;
use std::collections::BTreeMap;

/// The usage rows of the event ids in `?1` (a JSON list): `session_read` reads its tokens and cost
/// sources from them, and the cost rollup reads `model` and `cost_usd` (columns 0 and 6), so both
/// select the same rows from one query text.
pub(crate) const SESSION_USAGE_SQL: &str = "SELECT model, input, output, cache_read, cache_write, cache_write_1h, cost_usd, cost_from
         FROM usage_event WHERE session_id IN (SELECT value FROM json_each(?1))";

/// Per model: the sum of its priced rows, and whether any of its rows has no price.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CostRollup {
    models: BTreeMap<String, (f64, bool)>,
}

impl CostRollup {
    pub(crate) fn new() -> Self {
        Self { models: Default::default() }
    }

    /// Add one usage row's cost (`None`: the row has no price).
    pub(crate) fn add(&mut self, model: &str, cost: Option<f64>) {
        let entry = self.models.entry(model.to_owned()).or_insert((0.0, false));
        match cost {
            Some(cost) => entry.0 += cost,
            None => entry.1 = true,
        }
    }

    /// The session's cost: null when it has no usage or any model of it is unpriced, never partial.
    pub(crate) fn total(&self) -> Option<f64> {
        if matches!(self.models.len(), 0) || self.models.values().any(|(_, unpriced)| *unpriced) {
            return None;
        }
        Some(self.models.values().map(|(cost, _)| *cost).sum())
    }

    /// The models with an unpriced row, sorted.
    pub(crate) fn unpriced(&self) -> Vec<String> {
        self.models
            .iter()
            .filter(|(_, (_, unpriced))| *unpriced)
            .map(|(model, _)| model.clone())
            .collect()
    }
}

impl UsageStore {
    /// What every usage row of one session (all its event id-spaces, as `session_read` reads
    /// them) adds up to.
    pub(crate) fn session_cost(&self, session: &str) -> Result<CostRollup, String> {
        let ids = serde_json::to_string(&self.event_session_ids(session)?).map_err(|e| e.to_string())?;
        let mut stmt = self
            .conn
            .prepare_cached(SESSION_USAGE_SQL)
            .map_err(db_err)?;
        let rows = stmt
            .query_map(params![ids], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(6)?)))
            .map_err(db_err)?;
        let mut rollup = CostRollup::new();
        for row in rows {
            let (model, cost) = row.map_err(db_err)?;
            CostRollup::add(&mut rollup, &model, cost);
        }
        Ok(rollup)
    }
}
