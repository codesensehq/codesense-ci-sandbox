// SPDX-License-Identifier: MIT
use super::{rows, tree, views};
use crate::live_hub::projection::TopicRegistry;
use serde_json::{json, Value};

pub(crate) struct Fixture {
    registry: TopicRegistry,
    rows: Vec<super::AgentListRow>,
    next: bool,
}
impl Fixture {
    pub(crate) fn build() -> Self {
        let input: tree::TreeInput=serde_json::from_value(json!({"teamKey":"acme/widgets:default","sessions":(0..50).map(|i|json!({"row":{"handle":format!("scale-{i:02}"),"sessionId":format!("session-{i}"),"kind":"dashboard-launched","alive":true,"machineId":if i < 25 {"local"} else {"emulator"}}})).collect::<Vec<_>>()})).unwrap();
        let rows = rows::rows(&input, &json!({}));
        let mut fixture = Self {
            registry: TopicRegistry::default(),
            rows,
            next: false,
        };
        fixture.delta();
        fixture
    }
    pub(crate) fn snapshot(&mut self) -> Vec<Value> {
        self.registry
            .snapshot("agentList", "all")
            .unwrap()
            .snapshot
            .into_iter()
            .filter(|row| row["rowType"] != "state")
            .collect()
    }
    pub(crate) fn delta(&mut self) -> usize {
        self.next = !self.next;
        let parent = self.next.then(|| self.rows[0].id.clone());
        self.rows.last_mut().unwrap().parent_id = parent;
        self.rows.last_mut().unwrap().parent_source = self.next.then(|| "chosen".into());
        let deltas = views::replace(
            &mut self.registry,
            "all",
            &self.rows,
            &json!({"ready":true}),
        )
        .unwrap();
        serde_json::to_vec(&deltas).unwrap().len()
    }
}
