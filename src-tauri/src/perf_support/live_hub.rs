// SPDX-License-Identifier: MIT
use crate::weft::{inbox, store::WeftStore};
use tempfile::TempDir;

pub struct SessionsFixture {
    _home: TempDir,
    hub: crate::live_hub::LiveHub,
}
impl SessionsFixture {
    pub fn build() -> Self {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("inbox.sqlite3");
        let store = WeftStore::open(&path).unwrap();
        let handles: Vec<_> = (0..50).map(|i| format!("scale-{i}")).collect();
        for i in 0..5000 {
            inbox::inbox_insert(
                &store,
                "seed",
                &handles[i % 50],
                &format!("Message {i}"),
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        }
        let launch = crate::commands::dashboard_launch::DashboardLaunchState::new(50);
        for handle in &handles {
            launch.register(handle, handle, Some("codesense"));
        }
        let registry = launch
            .snapshot()
            .into_iter()
            .map(|s| crate::commands::dashboard_launch::session_row(s, true))
            .collect();
        let mut rows = crate::mcp::session_list::session_list_rows_with_tabs(
            registry,
            |_| None,
            None,
            |_| None,
        );
        let unread = inbox::inbox_unread_for(&store, &handles).unwrap();
        for row in &mut rows {
            row.unread = unread.get(&row.handle).map(|u| u.count);
        }
        let hub = crate::live_hub::LiveHub::default();
        hub.seed_rows(rows);
        Self { _home: home, hub }
    }
    pub fn read(&self) -> usize {
        self.hub.rows(None).0.len()
    }
}

pub struct InboxFixture {
    registry: crate::live_hub::projection::TopicRegistry,
}
impl InboxFixture {
    pub fn build() -> Self {
        use crate::live_hub::{inbox::InboxProjection, projection::TopicRegistry};
        let home = tempfile::tempdir().unwrap();
        let store = WeftStore::open(home.path().join("inbox.sqlite3")).unwrap();
        for i in 0..5000 {
            inbox::inbox_insert(
                &store,
                "sender",
                &format!("scale-{}", i % 50),
                &format!("Message {i}"),
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        }
        let team = crate::commands::dashboard_launch::LaunchTeam {
            org: "scale".into(),
            team: "default".into(),
        };
        let mut inbox = InboxProjection::default();
        let mut rows = Vec::new();
        let mut before = None;
        loop {
            let page = inbox::inbox_page(&store, before, 200).unwrap();
            let count = page.len();
            before = page.last().map(|m| m.id);
            for m in page {
                let kind = m.effective_kind();
                rows.push((
                    Some("team:scale/default".into()),
                    serde_json::json!({"id":m.id.to_string(),"messageId":m.id,"text":m.text}),
                ));
                inbox.put(&team, m, kind);
            }
            if count < 200 {
                break;
            }
        }
        rows.extend(inbox.count_rows());
        let mut registry = TopicRegistry::default();
        registry.replace("inbox", rows, None).unwrap();
        Self { registry }
    }
    pub fn first_snapshot(&mut self) -> usize {
        self.registry
            .subscribe_window("bench", "inbox", "team:scale/default", None, None)
            .unwrap()
            .snapshot
            .unwrap()
            .len()
    }
    pub fn window(&mut self) -> usize {
        self.registry
            .subscribe_window("window", "inbox", "team:scale/default", None, Some([0, 50]))
            .unwrap();
        self.registry
            .move_window("window", [25, 75])
            .unwrap()
            .upserts
            .len()
    }
}

/// Exercises the same order-only projection path native and HTML docks subscribe to.
pub struct AgentListFixture(crate::live_hub::agent_list::bench::Fixture);
impl AgentListFixture {
    pub fn build() -> Self {
        Self(crate::live_hub::agent_list::bench::Fixture::build())
    }
    pub fn snapshot(&mut self) -> usize {
        self.0.snapshot().len()
    }
    pub fn delta(&mut self) -> usize {
        self.0.delta()
    }
}
