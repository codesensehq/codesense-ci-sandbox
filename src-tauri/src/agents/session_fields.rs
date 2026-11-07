// SPDX-License-Identifier: MIT
use crate::live_hub::projection::{FieldProvider, FieldProviders};
use serde_json::{Map, Value};
pub(crate) const AGENT_FIELDS: &[&str] = &[
    "agent",
    "runtime",
    "usage",
    "role",
    "queueRole",
    "roleJoinError",
    "avatar",
    "progress",
    "launchTelemetry",
    "releaseQualified",
    "releaseNote",
    "inboxWaiter",
    "subagents",
];
struct AgentFields;
impl FieldProvider for AgentFields {
    fn fields(&self) -> &'static [&'static str] {
        AGENT_FIELDS
    }
    fn contribute(&self, source: &Map<String, Value>, row: &mut Map<String, Value>) {
        for key in AGENT_FIELDS {
            if let Some(v) = source.get(*key) {
                row.insert((*key).into(), v.clone());
            }
        }
    }
}
pub(crate) fn register(providers: &mut FieldProviders) {
    providers
        .register(Box::new(AgentFields))
        .expect("unique agent fields");
}
