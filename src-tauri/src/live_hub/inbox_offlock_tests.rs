// SPDX-License-Identifier: MIT
//! Materialising the inbox view must never run while the LiveHub model lock is held:
//! cloning every message row of a team under that lock froze every hub reader for hours
//! once the process was swapped out (SV-PBF-169, same class as SV-PBF-100 and 151).
use super::*;
use crate::commands::dashboard_launch::LaunchTeam;
use std::sync::mpsc;
use std::time::Duration;

/// A bound on how long a reader may take; only reached when the guard fails.
const BOUND: Duration = Duration::from_secs(5);

fn team() -> LaunchTeam {
    LaunchTeam {
        org: "acme".into(),
        team: "default".into(),
    }
}

struct TestHost;
#[async_trait::async_trait]
impl crate::commands::agent_inbox::InboxHost for TestHost {
    async fn team_store(
        &self,
        _team: &crate::commands::dashboard_launch::LaunchTeam,
    ) -> Result<std::path::PathBuf, String> {
        Err("no store".into())
    }
    async fn active_team(&self) -> Result<crate::commands::dashboard_launch::LaunchTeam, String> {
        Err("no active team".into())
    }
    fn launch_team(&self, _handle: &str) -> Option<crate::commands::dashboard_launch::LaunchTeam> {
        None
    }
    fn handle_for_session(&self, _session_id: &str) -> Option<String> {
        None
    }
    async fn sessions(&self) -> Result<crate::commands::agent_inbox::KnownSessions, String> {
        Ok(crate::commands::agent_inbox::KnownSessions::new(vec![]))
    }
    fn has_handle(&self, _handle: &str) -> bool {
        false
    }
    fn display_name(&self, _handle: &str) -> Option<String> {
        None
    }
    fn inbox_changed(&self) {}
}

fn hub_with_one_message() -> (std::sync::Arc<LiveHub>, i64) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(hub_with_one_message_async())
}

async fn hub_with_one_message_async() -> (std::sync::Arc<LiveHub>, i64) {
    use crate::commands::{agent_inbox::KnownSessions, dashboard_launch::LaunchTeam};
    use crate::weft::{inbox as ledger, WeftStore};

    let hub = std::sync::Arc::new(LiveHub::default());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.sqlite3");
    let store = WeftStore::open(&path).unwrap();
    let id = ledger::inbox_insert(
        &store, "worker", "human", "note", None, None, None, None, None,
    )
    .unwrap();
    let team = LaunchTeam {
        org: "acme".into(),
        team: "default".into(),
    };
    let batch = super::inbox::read_batch(path, true, vec![]).await.unwrap();
    let host = TestHost;
    let sessions = KnownSessions::new(vec![]);
    {
        let mut model = hub.model.lock().unwrap();
        std::sync::Arc::make_mut(&mut model.inbox).apply_batch(&host, &sessions, &team, batch, &[]);
    }
    (hub, id)
}

#[test]
fn hub_readers_proceed_while_the_inbox_view_is_being_materialised() {
    let (hub, id) = hub_with_one_message();
    let expected = (id, "note".to_string());

    let (entered_tx, entered) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let builder = {
        let hub = hub.clone();
        std::thread::spawn(move || {
            let gate = std::cell::RefCell::new(Some((entered_tx, release_rx)));
            inbox::VIEW_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    if let Some((entered, release)) = gate.borrow_mut().take() {
                        entered.send(()).unwrap();
                        let _ = release.recv();
                    }
                }));
            });
            hub.inbox_view(&team()).unwrap()
        })
    };
    let entry_res = entered.recv_timeout(BOUND);

    let (done_tx, done) = mpsc::channel();
    let reader = {
        let hub = hub.clone();
        std::thread::spawn(move || {
            hub.rows(None);
            hub.human_unread_total();
            hub.chat_unread("worker");
            let view = hub.inbox_view(&team()).unwrap();
            let pair = view
                .messages
                .iter()
                .next()
                .map(|m| (m.message_id, m.text.clone()));
            let _ = done_tx.send(pair);
        })
    };
    let reader_res = done.recv_timeout(BOUND);

    // Release even on timeout and join both workers before any assertion.
    drop(release_tx);
    let builder_join = builder.join();
    let reader_join = reader.join();
    inbox::VIEW_HOOK.with(|hook| *hook.borrow_mut() = None);

    entry_res.expect("the view was never materialised");
    let reader_pair = reader_res.expect("hub reader timed out while the view was materialised");
    let builder_view = builder_join.expect("builder thread panicked");
    reader_join.expect("reader thread panicked");

    let builder_pair = builder_view
        .messages
        .iter()
        .next()
        .map(|m| (m.message_id, m.text.clone()));
    assert_eq!(builder_pair, Some(expected.clone()));
    assert_eq!(reader_pair, Some(expected));
}

/// The same rule, read from the source: a `hub.model` scope in `inbox_view` may take shared
/// handles but never materialise the view.
#[test]
fn inbox_view_builds_only_after_the_model_scope_ends() {
    let source = include_str!("mod.rs");
    let body = &source[source.find("fn inbox_view(").unwrap()..];
    let body = &body[..body.find("\n    }\n").unwrap()];
    let scope_end = body
        .find("std::sync::Arc::clone(&model.inbox)\n        };")
        .expect("handles are taken in a scope");
    let build = body.find(".build()").expect("the view is built");
    assert!(
        scope_end < build,
        "inbox_view builds the view under the model lock"
    );
    let pre_scope = &body[..scope_end];
    let without_arc_clones = pre_scope.replace("std::sync::Arc::clone", "");
    assert!(
        !without_arc_clones.contains(".clone()"),
        "inbox_view clones under the model lock"
    );
}
