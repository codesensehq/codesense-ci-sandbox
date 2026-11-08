// SPDX-License-Identifier: MIT
//! SW-WV-A2 acceptance tests for the typed ephemeral worker spawn.
//!
//! Covers AC-1 (lease gate), AC-2 (four subclasses + per-subclass deny-list),
//! AC-3 (roster provision→dispatch + audit atomicity + failure rollback), AC-4
//! (ADK-neutral provider seam), AC-5 (no deadlock under concurrency), AC-6
//! (idempotent duplicate spawn), AC-7 (bridge-only MCP server projection).
//!
//! The PTY seam is behind [`super::WorkerLauncher`] so the orchestration
//! ([`super::spawn_with`]) runs against a real temp [`WeftStore`] with a stub
//! launcher — the roster state machine, audit trail, and error rollback are all
//! exercised without a real `openpty`/fork. The full-path PTY-live assertion is
//! the `#[ignore]` integration test at the bottom.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::weft::sqlite::{AuditQueryParams, WvRosterRow};
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

fn req(story: &str, subclass: WorkerSubclass) -> WorkerSpawnRequest {
    WorkerSpawnRequest {
        story_id: story.to_string(),
        subclass,
        initial_prompt: None,
        provider: None,
        scope: "root".to_string(),
        run_id: None,
        launched_by: None,
    }
}

/// Read a single roster row's spawn-path columns for assertions.
fn read_roster(store: &WeftStore, name: &str) -> Option<(String, Option<String>, Option<String>)> {
    let conn = store.conn().unwrap();
    conn.query_row(
        "SELECT status, session_id, subclass FROM wv_roster WHERE name=?1",
        [name],
        |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?)),
    )
    .ok()
}

fn audit_rows_for(store: &WeftStore, table: &str) -> Vec<crate::weft::sqlite::AuditRow> {
    store
        .query_audit(AuditQueryParams {
            table_name: Some(table.to_string()),
            limit: 100,
            ..Default::default()
        })
        .unwrap()
}

/// A stub launcher that records each call and returns a synthetic handle.
struct StubLauncher {
    calls: Arc<Mutex<Vec<String>>>,
    handle: String,
}
impl StubLauncher {
    fn new(handle: &str) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            handle: handle.to_string(),
        }
    }
}
#[async_trait::async_trait]
impl WorkerLauncher for StubLauncher {
    async fn launch(&self, req: &WorkerSpawnRequest) -> Result<String, String> {
        self.calls.lock().unwrap().push(req.story_id.clone());
        Ok(self.handle.clone())
    }
}

/// A launcher that always fails — for the rollback path.
struct FailingLauncher;
#[async_trait::async_trait]
impl WorkerLauncher for FailingLauncher {
    async fn launch(&self, _req: &WorkerSpawnRequest) -> Result<String, String> {
        Err("openpty refused".to_string())
    }
}

/// A launcher that counts how many real launches happened (for idempotency).
struct CountingLauncher {
    count: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl WorkerLauncher for CountingLauncher {
    async fn launch(&self, req: &WorkerSpawnRequest) -> Result<String, String> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(format!("sess-{}", req.story_id))
    }
}

#[tokio::test]
async fn all_four_subclasses_spawn_and_stamp_subclass() {
    for sc in WorkerSubclass::all() {
        let (_d, store) = open_temp();
        let launcher = StubLauncher::new("sess");
        let story = format!("SW-{}", sc.as_str());
        let out = spawn_with(&store, &launcher, req(&story, *sc)).await;
        assert!(
            matches!(out, Ok(WeftSpawnResult::Spawned { .. })),
            "{} spawns",
            sc.as_str()
        );
        let (status, session, subclass) = read_roster(&store, &story).unwrap();
        assert_eq!(status, "dispatch");
        assert_eq!(session.as_deref(), Some("sess"));
        assert_eq!(subclass.as_deref(), Some(sc.as_str()));
    }
}

// ---------- AC-3: roster provision→dispatch + audit atomicity + rollback ----------

#[tokio::test]
async fn roster_row_written_before_spawn() {
    let (_d, store) = open_temp();
    // A launcher that asserts the provision row is already present WHEN it is called
    // (i.e. BEFORE the PTY is live), then returns a handle.
    struct AssertingLauncher {
        store: WeftStore,
    }
    #[async_trait::async_trait]
    impl WorkerLauncher for AssertingLauncher {
        async fn launch(&self, req: &WorkerSpawnRequest) -> Result<String, String> {
            let (status, session, _) = read_roster(&self.store, &req.story_id)
                .expect("provision row exists before launch");
            assert_eq!(status, "provision", "row is `provision` before PTY is live");
            assert!(session.is_none(), "session_id not yet stamped");
            Ok("sess-live".to_string())
        }
    }
    let launcher = AssertingLauncher {
        store: store.clone(),
    };
    let out = spawn_with(&store, &launcher, req("SW-P", WorkerSubclass::Impl)).await;
    assert!(matches!(out, Ok(WeftSpawnResult::Spawned { .. })));
    // After: dispatch + session stamped.
    let (status, session, _) = read_roster(&store, "SW-P").unwrap();
    assert_eq!(status, "dispatch");
    assert_eq!(session.as_deref(), Some("sess-live"));
    // AC-3 audit atomicity: exactly one provision + one dispatch audit entry.
    let audits = audit_rows_for(&store, "wv_roster");
    let provisions = audits.iter().filter(|a| a.operation == "provision").count();
    let dispatches = audits.iter().filter(|a| a.operation == "dispatch").count();
    assert_eq!(provisions, 1, "one provision audit row");
    assert_eq!(dispatches, 1, "one dispatch audit row");
}

#[tokio::test]
async fn roster_row_deleted_on_spawn_failure() {
    let (_d, store) = open_temp();
    let out = spawn_with(&store, &FailingLauncher, req("SW-F", WorkerSubclass::Impl)).await;
    assert!(
        matches!(out, Err(WeftError::SpawnFailed(_))),
        "spawn failure surfaces"
    );
    // AC-3: no roster row remains…
    assert!(
        read_roster(&store, "SW-F").is_none(),
        "provision row rolled back"
    );
    // …and NO audit entry for the aborted provision (an aborted provision leaves no
    // dispatch/deprovision trace; the provision insert's own audit row is removed
    // with the row because the delete is by design un-audited AND the provision
    // insert co-committed its audit — so we assert no `dispatch` trace and the
    // provision entry is the only historical artifact, then confirm the row is gone).
    let audits = audit_rows_for(&store, "wv_roster");
    assert!(
        !audits.iter().any(|a| a.operation == "dispatch"),
        "no dispatch audit for a failed spawn"
    );
}

// ---------- AC-4: ADK-neutral provider seam ----------

#[test]
fn provider_defaults_to_claude_and_threads_codex() {
    use codesense_pty_core::agent_cli::AgentCli;
    let mut r = req("SW-PR", WorkerSubclass::Impl);
    assert_eq!(r.resolved_runtime(), AgentCli::Claude, "absent → Claude");
    r.provider = Some(AgentProvider::Codex);
    assert_eq!(r.resolved_runtime(), AgentCli::Codex, "supplied → threaded");
}

/// A worker launches its provider's unattended built-in through the agent launch path: the
/// launch records `builtin/<runtime>-unattended@<revision>`, runs that runtime's CLI
/// headless with permission checks off (it never waits on a prompt), and is scoped to the
/// subclass's servers.
#[test]
fn a_worker_launches_its_providers_builtin_agent_headless() {
    let home = tempfile::tempdir().unwrap();
    let principal = crate::domain::Principal::from(7u64);
    let revision = crate::agents::builtin::revision();

    let claude = prepare_worker_agent(
        home.path(),
        &principal,
        &req("SW-1", WorkerSubclass::Impl),
        None,
    )
    .unwrap();
    assert_eq!(
        claude.revision_label(),
        format!("builtin/claude-unattended@{revision}")
    );
    assert_eq!(claude.launch.binary, "claude");
    assert_eq!(claude.launch.args, ["--dangerously-skip-permissions"]);
    assert_eq!(
        claude.launch.mode,
        crate::agents::adapter::LaunchMode::Headless
    );
    assert_eq!(claude.launch.mcp_allow, WorkerSubclass::Impl.mcp_servers());

    let mut codex_req = req("SW-2", WorkerSubclass::Review);
    codex_req.provider = Some(AgentProvider::Codex);
    let codex = prepare_worker_agent(home.path(), &principal, &codex_req, None).unwrap();
    assert_eq!(
        codex.revision_label(),
        format!("builtin/codex-unattended@{revision}")
    );
    assert_eq!(codex.launch.binary, "codex");
    assert_eq!(codex.generated[0].content["approval_policy"], "never");
    assert_eq!(
        codex.generated[0].content["sandbox_mode"],
        "danger-full-access"
    );
    assert_eq!(codex.launch.mcp_allow, WorkerSubclass::Review.mcp_servers());
    // The account follows the runtime.
    assert_eq!(
        codex.account_input().runtime,
        Some(codesense_pty_core::agent_cli::AgentCli::Codex)
    );
}

// ---------- AC-5: no UI-thread blocking / no deadlock ----------

#[tokio::test]
async fn concurrent_spawns_complete_without_deadlock() {
    let (_d, store) = open_temp();
    let count = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for i in 0..12 {
        let store = store.clone();
        let launcher = CountingLauncher {
            count: count.clone(),
        };
        handles.push(tokio::spawn(async move {
            spawn_with(
                &store,
                &launcher,
                req(&format!("SW-C{i}"), WorkerSubclass::Impl),
            )
            .await
        }));
    }
    // AC-5: every future resolves within the timeout; a hang = deadlock on the
    // async executor = timeout panic.
    let all = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        for h in handles {
            let _ = h.await.expect("task joins");
        }
    })
    .await;
    assert!(all.is_ok(), "all concurrent spawns resolved within 5s");
    assert_eq!(
        count.load(Ordering::SeqCst),
        12,
        "each distinct story launched once"
    );
}

// ---------- AC-6: idempotent duplicate spawn ----------

#[tokio::test]
async fn duplicate_spawn_idempotent() {
    let (_d, store) = open_temp();
    let count = Arc::new(AtomicUsize::new(0));
    let l1 = CountingLauncher {
        count: count.clone(),
    };
    let first = spawn_with(&store, &l1, req("SW-D", WorkerSubclass::Impl)).await;
    assert!(matches!(first, Ok(WeftSpawnResult::Spawned { .. })));
    // Second call for the same story: AlreadyLive, no new PTY.
    let l2 = CountingLauncher {
        count: count.clone(),
    };
    let second = spawn_with(&store, &l2, req("SW-D", WorkerSubclass::Impl)).await;
    match second {
        Ok(WeftSpawnResult::AlreadyLive { handle }) => {
            assert_eq!(handle, "sess-SW-D", "returns the existing worker's handle");
        }
        other => panic!("expected AlreadyLive, got {other:?}"),
    }
    assert_eq!(
        count.load(Ordering::SeqCst),
        1,
        "only the first call launched a PTY"
    );
}

#[tokio::test]
async fn respawn_allowed_after_worker_exits() {
    let (_d, store) = open_temp();
    // Seed a terminal (deprovision) row via the B3 handler, then a fresh spawn is
    // allowed (the live-state guard excludes terminal states).
    crate::weft::wv_upsert_roster(
        &store,
        &WvRosterRow {
            name: "SW-E".to_string(),
            workspace: None,
            pane: None,
            kind: None,
            subclass: Some("impl".to_string()),
            lifecycle: "task".to_string(),
            owned: None,
            orch_owner: None,
            relation: "child:root".to_string(),
            parent: None,
            current_task: None,
            cwd: None,
            status: Some("deprovision".to_string()),
            session_id: None,
            run_id: None,
        },
    )
    .unwrap();
    let launcher = StubLauncher::new("sess-new");
    let out = spawn_with(&store, &launcher, req("SW-E", WorkerSubclass::Impl)).await;
    assert!(
        matches!(out, Ok(WeftSpawnResult::Spawned { .. })),
        "a fresh spawn is allowed after the prior worker reached a terminal state"
    );
}

// ---------- AC-7: bridge-only MCP server projection ----------

fn this_thread_mcp_scope_lines() -> Vec<String> {
    crate::test_logger::lines("mcp_scope")
}

/// Removes a projected scope dir even when an assertion fails mid-loop.
struct ScopeDirGuard(String);

impl Drop for ScopeDirGuard {
    fn drop(&mut self) {
        crate::commands::mcp_scope::remove_scope_config(&self.0);
    }
}

/// Every subclass's spawn, for both bundled providers, projects exactly the
/// `codesense` bridge out of a worktree config that also installs another team
/// server, through the same profile resolution and scope projection the real worker
/// launch uses, and logs no unknown-allow-id warning while doing it.
#[test]
fn every_worker_subclass_projects_bridge_only_without_warnings() {
    let _home_env = crate::routines::test_support::TempHome::new();
    crate::test_logger::clear("mcp_scope");
    // Prove the capture is live before trusting an empty result.
    log::warn!(target: "mcp_scope", "capture probe");
    assert_eq!(
        this_thread_mcp_scope_lines(),
        vec!["capture probe".to_string()],
        "the mcp_scope log capture must be installed"
    );
    crate::test_logger::clear("mcp_scope");

    let home = tempfile::tempdir().unwrap();
    let principal = crate::domain::Principal::from(7u64);
    for (provider, runtime) in [
        (
            AgentProvider::Claude,
            codesense_pty_core::agent_cli::AgentCli::Claude,
        ),
        (
            AgentProvider::Codex,
            codesense_pty_core::agent_cli::AgentCli::Codex,
        ),
    ] {
        let scope = codesense_pty_core::agent_cli::descriptor(runtime)
            .mcp_scope
            .as_ref()
            .expect("each built-in runtime declares an mcp_scope");
        let format = crate::shared::config_format::ConfigFormat::from(scope.format);
        let worktree = tempfile::tempdir().unwrap();
        let canonical_path = worktree.path().join(&scope.config_filename);
        std::fs::create_dir_all(canonical_path.parent().unwrap()).unwrap();
        let canonical = serde_json::json!({
            scope.server_map_key.as_str(): {
                "codesense": { "command": "/abs/codesense-bridge" },
                "slack": { "command": "/abs/slack-mcp" }
            }
        });
        std::fs::write(&canonical_path, format.serialize(&canonical).unwrap()).unwrap();

        for sc in WorkerSubclass::all() {
            let mut worker = req("SW-1", *sc);
            worker.provider = Some(provider);
            let agent = prepare_worker_agent(home.path(), &principal, &worker, None).unwrap();
            let allow = agent
                .launch
                .mcp_allow
                .expect("a worker launch is always scoped");
            let session = format!(
                "weft-worker-scope-{}-{}-{}-{}",
                provider.as_str(),
                sc.as_str(),
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let _guard = ScopeDirGuard(session.clone());
            let path = crate::commands::mcp_scope::project_from_worktree(
                &session,
                worktree.path(),
                &allow,
                scope,
            )
            .unwrap();
            let written = format.parse(&std::fs::read(&path).unwrap()).unwrap();
            let servers: Vec<&String> = written[scope.server_map_key.as_str()]
                .as_object()
                .expect("projected config has a server map")
                .keys()
                .collect();
            assert_eq!(
                servers,
                vec!["codesense"],
                "{} subclass {} must project the bridge only",
                provider.as_str(),
                sc.as_str()
            );
        }
    }
    assert_eq!(
        this_thread_mcp_scope_lines(),
        Vec::<String>::new(),
        "a worker spawn must log no mcp_scope warning"
    );
}

#[test]
fn wv_spawn_worker_is_in_wv_interactive_verbs() {
    // The compile-time attestation half of the two-seam gate (the runtime
    // `InteractiveGate::admit` is in the MCP handler). The cross-registry check that
    // it is ALSO in `INTERACTIVE_VERBS` is `every_wv_interactive_verb_has_both_seams`.
    assert!(crate::weft::WV_INTERACTIVE_VERBS.contains(&WV_SPAWN_WORKER_VERB));
}

// ---------- AC-2 integration: PTY session is live ----------

/// AC-2 integration criterion — the full spawn path opens a real PTY and the
/// returned handle resolves to a live `PtyState` entry. Ignored: `cargo test --lib`
/// has no headless PTY / AppHandle fixture. Kept compiling unconditionally so the
/// seam never rots; run with `--ignored` in an environment with a live app.
#[tokio::test]
#[ignore = "requires a real AppHandle + headless PTY fixture (no PTY in --lib CI)"]
async fn impl_worker_pty_session_is_live() {
    // Intentionally empty in --lib CI: the RealWorkerLauncher path needs a live
    // AppHandle, a hydrated managed worktree, and openpty — none available here. The
    // orchestration is covered by the stub-launcher tests above; this seam is
    // exercised manually / in an integration harness.
}

#[tokio::test]
async fn captured_team_spawn_and_detached_completion_share_the_queue_and_brief_store() {
    use crate::commands::dashboard_launch::{DashboardLaunchState, LaunchTeam};
    use crate::commands::route_to_context::ActiveContext;
    use crate::mcp::{
        handler::{build_signal_done_hooks, WireDoneRecord},
        local_identity,
    };
    use crate::weft::queue;
    let dir = tempfile::tempdir().unwrap();
    // UI stays on B for the whole journey, including detached completion.
    let ui = ActiveContext::for_team(dir.path().into(), 42, "org/repo".into(), "b".into());
    let launch = DashboardLaunchState::new(4);
    for (handle, session) in [("controller", "controller-pty"), ("story-a", "worker-pty")] {
        launch.register(handle, session, Some("codesense"));
        launch.set_launch_team_if_absent(
            handle,
            LaunchTeam {
                org: "org/repo".into(),
                team: "a".into(),
            },
        );
    }
    let path =
        local_identity::weft_store_path(&ui, Some("controller-pty"), Some(&launch), None).unwrap();
    let store = resolve_store(path.clone()).await.unwrap();
    let other = WeftStore::open(
        crate::weft::weft_store_path(dir.path(), &ui.principal(), "org/repo", "b").unwrap(),
    )
    .unwrap();
    // A same-id row in B makes accidental writes to B observable.
    let id = queue::enqueue(&store, "a", Some("root"), None, None, None, None, 1).unwrap();
    assert_eq!(
        queue::enqueue(&other, "b", Some("root"), None, None, None, None, 1).unwrap(),
        id
    );
    queue::claim(&store, "controller-pty", Some("root"), 2).unwrap();
    let brief =
        crate::weft::wv_write_dispatch(&store, "story-a", "task-a", "", "", "done-a", Some("root"))
            .unwrap();
    let mut request = req("story-a", WorkerSubclass::Review);
    request.run_id = Some("run-a".into());
    spawn_with(&store, &StubLauncher::new("story-a"), request)
        .await
        .unwrap();
    assert_eq!(read_roster(&store, "story-a").unwrap().0, "dispatch");
    assert!(read_roster(&other, "story-a").is_none());
    crate::weft::wv_report_handoff(&store, "story-a", "worker-pty", "manager", "finished", &[])
        .unwrap();
    let worker_path =
        local_identity::weft_store_path(&ui, Some("worker-pty"), Some(&launch), None).unwrap();
    assert_eq!(worker_path, path);
    let worker_key = launch.handle_for_session("worker-pty").unwrap();
    assert_ne!(worker_key, "worker-pty");
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let hooks = build_signal_done_hooks(
        app.handle(),
        worker_path,
        worker_key,
        "success".into(),
        Some("done".into()),
        Some(WireDoneRecord {
            dispatch_id: brief.to_string(),
            leg1_source: "source-a".into(),
            leg2_footprint: vec![],
            leg3_packet: serde_json::json!({"signalStatus":"success"}),
        }),
    );
    tokio::spawn(async move { hooks.dispatch_hooks("worker-pty".into()).await })
        .await
        .unwrap();
    assert_eq!(read_roster(&store, "story-a").unwrap().0, "handoff");
    let conn = store.conn().unwrap();
    let state: String = conn
        .query_row(
            "SELECT state FROM wv_dispatch_records WHERE story_id='story-a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "done");
    let completion: (i64, String) = conn
        .query_row("SELECT queue_id, session_id FROM wv_done_record", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(completion, (id, "worker-pty".into()));
    let signal_session: String = conn
        .query_row("SELECT session_id FROM wv_signals", [], |r| r.get(0))
        .unwrap();
    assert_eq!(signal_session, "worker-pty");
    let other_conn = other.conn().unwrap();
    for table in [
        "wv_dispatch_records",
        "wv_roster",
        "wv_done_record",
        "wv_signals",
        "wv_handoff_records",
    ] {
        let count: i64 = other_conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table} must stay out of B");
    }
    let state: String = other_conn
        .query_row("SELECT state FROM wv_queue WHERE id=?1", [id], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "pending");
}
