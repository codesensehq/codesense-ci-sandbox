// SPDX-License-Identifier: MIT
use crate::live_hub::projection::{FieldProvider, FieldProviders};
use serde_json::{Map, Value};

const FIELDS: &[&str] = &[
    "sessionRef",
    "inboxRef",
    "teamKey",
    "placementTeamRef",
    "parentId",
    "parentSource",
    "order",
    "workspaceRef",
    "storyRef",
    "workspaceRank",
    "crewParentId",
    "crewOrder",
    "crewLead",
];
struct AgentListFields;
impl FieldProvider for AgentListFields {
    fn fields(&self) -> &'static [&'static str] {
        FIELDS
    }
    fn contribute(&self, source: &Map<String, Value>, row: &mut Map<String, Value>) {
        for key in FIELDS {
            if let Some(value) = source.get(*key) {
                row.insert((*key).into(), value.clone());
            }
        }
    }
}
pub(crate) fn register(providers: &mut FieldProviders) {
    providers
        .register(Box::new(AgentListFields))
        .expect("unique agent-list fields");
}
