// SPDX-License-Identifier: MIT
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

use codesense_pty_core::domain::{PtySessionId, ServerId, WorkspaceKey};
use codesense_pty_core::layout::{
    EntryState, LayoutSnapshot, Placement, TabEntry, TagPatch, Tags, ViewerLayout, WorkspaceLayout,
};

use super::*;
use crate::commands::dashboard_launch::{
    record_bound_launch, session_row, LaunchMeta, LaunchRecord, LaunchTeam,
};

const CONTENT: &str = "/ctx/acme/backend/content";
const MANAGER: &str = "acme/backend/manager@2";
const DOC: &str = "/ctx/boards/agents.html";

fn manager_handle() -> String {
    format!("{CONTENT}#manager")
}

/// Apply a tag patch the way the layout host does: `Some` sets, `None` removes.
fn apply(tags: &mut Tags, patch: TagPatch) {
    for (key, value) in patch {
        match value {
            Some(value) => {
                tags.insert(key, value);
            }
            None => {
                tags.remove(&key);
            }
        }
    }
}

fn tab(session: &str, tags: Tags, state: EntryState) -> TabEntry {
    TabEntry {
        session: PtySessionId::new(session),
        name: session.into(),
        role: None,
        placement: Placement::Tab,
        state,
        tags,
    }
}

fn snapshot(tabs: Vec<TabEntry>) -> LayoutSnapshot {
    let layout = WorkspaceLayout {
        tabs,
        ..WorkspaceLayout::default()
    };
    LayoutSnapshot {
        schema: 1,
        server_id: ServerId::new("s"),
        server_name: "s".into(),
        epoch: "e".into(),
        rev: 1,
        workspaces: BTreeMap::from([(WorkspaceKey::new("acme:backend|ws-content"), layout)]),
        viewer: ViewerLayout::default(),
    }
}

fn tags(pairs: &[(&str, &str)]) -> Tags {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn exited() -> EntryState {
    EntryState::Exited {
        exit_code: Some(0),
        signal: None,
    }
}

/// A manager launch bound the way `dashboard_launch_bind` binds it, as an agent that
/// launched it named it.
fn bound_manager(state: &DashboardLaunchState, session: &str) {
    let handle = manager_handle();
    let record = LaunchRecord {
        meta: LaunchMeta {
            queue_role: None,
            role_join_error: None,
            launcher_context: None,
            display_name: Some("Conductor".into()),
            launched_by: Some("lead".into()),
            story: Some("SW-1".into()),
            role: Some("manager".into()),
            avatar: Some("#ff8800".into()),
            team: None,
            continues: None,
            reviews: None,
        },
        started_with: None,
        max_runtime_ms: None,
    };
    assert!(state.begin_with_provenance(&handle, None, record).unwrap());
    state.bind(&handle, session, Some(DOC)).unwrap();
    record_bound_launch(
        state,
        &handle,
        session,
        true,
        Some("user-1".into()),
        Some("acme:backend"),
        Some(MANAGER.into()),
    );
}

/// The tags a bound manager's tab carries.
fn manager_tags(session: &str) -> Tags {
    let before = DashboardLaunchState::new(4);
    bound_manager(&before, session);
    let mut recorded = Tags::new();
    apply(
        &mut recorded,
        before.launch_tags_for_session(session).unwrap(),
    );
    recorded
}

fn manager_launch(session: &str) -> RestoredLaunch {
    restorable_launches(&snapshot(vec![tab(
        session,
        manager_tags(session),
        EntryState::Live,
    )]))
    .remove(0)
}

#[test]
fn a_bound_launch_writes_its_whole_record_as_launch_tags() {
    let state = DashboardLaunchState::new(2);
    bound_manager(&state, "sid-ctl");
    let patch = state.launch_tags_for_session("sid-ctl").unwrap();
    let get = |key: &str| patch.get(key).cloned().flatten();
    assert_eq!(get(TAG_LAUNCH_HANDLE), Some(manager_handle()));
    assert_eq!(get(TAG_LAUNCH_AGENT_REVISION).as_deref(), Some(MANAGER));
    assert_eq!(get(TAG_LAUNCH_TEAM).as_deref(), Some("acme:backend"));
    assert_eq!(get(TAG_LAUNCH_PRINCIPAL).as_deref(), Some("user-1"));
    assert_eq!(get(TAG_LAUNCHER).as_deref(), Some(DOC));
    assert_eq!(
        get(TAG_LAUNCH_KIND).as_deref(),
        Some(KIND_DASHBOARD_LAUNCHED)
    );
    assert_eq!(get(TAG_LAUNCHED_BY).as_deref(), Some("lead"));
    assert_eq!(get(TAG_LAUNCH_NAME).as_deref(), Some("Conductor"));
    assert_eq!(get(TAG_LAUNCH_ROLE).as_deref(), Some("manager"));
    assert_eq!(get(TAG_LAUNCH_AVATAR).as_deref(), Some("#ff8800"));
    assert_eq!(get(TAG_LAUNCH_STORY).as_deref(), Some("SW-1"));
    assert!(get(TAG_LAUNCH_BOUND_AT).is_some());
    assert!(get(TAG_LAUNCH_STARTED_AT).is_some());
    // Leaving chatWorkspaceId unprotected keeps protected keys minimal: open-by-default policy, not architecture.
    assert!(patch
        .keys()
        .all(|key| is_launch_tag(key) || key == "chatWorkspaceId"));
    assert_eq!(state.launch_tags_for_session("sid-unknown"), None);
}

#[test]
fn a_rebind_clears_a_stale_revision_but_never_the_launcher() {
    let state = DashboardLaunchState::new(2);
    assert!(state.begin("h").unwrap());
    state.bind("h", "sid", None).unwrap();
    let mut held = tags(&[
        (TAG_LAUNCH_AGENT_REVISION, MANAGER),
        (TAG_LAUNCHER, DOC),
        ("hidden", "tab"),
    ]);
    apply(&mut held, state.launch_tags_for_session("sid").unwrap());
    assert_eq!(held.get(TAG_LAUNCH_AGENT_REVISION), None);
    assert_eq!(held.get(TAG_LAUNCHER).map(String::as_str), Some(DOC));
    assert_eq!(held.get(TAG_LAUNCH_HANDLE).map(String::as_str), Some("h"));
    assert_eq!(held.get("hidden").map(String::as_str), Some("tab"));
}

#[test]
fn the_bind_time_moves_only_when_the_session_changes() {
    let state = DashboardLaunchState::new(2);
    state.register("h", "sid-1", None);
    let at = |state: &DashboardLaunchState, session: &str| {
        state.launch_tags_for_session(session).unwrap()[TAG_LAUNCH_BOUND_AT].clone()
    };
    let first = at(&state, "sid-1");
    std::thread::sleep(std::time::Duration::from_millis(3));
    state.register("h", "sid-1", Some("codesense"));
    assert_eq!(
        at(&state, "sid-1"),
        first,
        "a re-registration keeps the bind time"
    );
    state.register("h", "sid-2", None);
    assert_ne!(at(&state, "sid-2"), first, "a new session is a new bind");
}

#[test]
fn a_restarted_app_restores_the_launch_its_tab_recorded() {
    let launch = manager_launch("sid-ctl");
    let after = DashboardLaunchState::new(4);
    assert!(after.restore_launch(&launch, None, launch.agent_revision.clone()));

    let handle = manager_handle();
    assert_eq!(after.resolve(&handle).as_deref(), Some("sid-ctl"));
    assert_eq!(
        after.agent_revision_for_session("sid-ctl").as_deref(),
        Some(MANAGER)
    );
    assert_eq!(
        after.owner_for_session("sid-ctl"),
        Some(SessionOwner {
            principal: Some("user-1".into()),
            org: "acme".into(),
            team: Some("backend".into()),
        })
    );
    assert_eq!(
        after.launch_team(&handle),
        Some(LaunchTeam {
            org: "acme".into(),
            team: "backend".into(),
        })
    );
    let row = after
        .snapshot()
        .into_iter()
        .find(|s| s.handle == handle)
        .unwrap();
    assert_eq!(row.kind, KIND_DASHBOARD_LAUNCHED);
    assert!(
        row.backgrounded,
        "a restored dashboard launch holds its slot"
    );
    assert_eq!(row.launcher.as_deref(), Some(DOC));
    assert_eq!(row.started_at_ms, launch.started_at_ms);
    assert_eq!(row.meta.display_name.as_deref(), Some("Conductor"));
    assert_eq!(row.meta.launched_by.as_deref(), Some("lead"));
    assert_eq!(row.meta.role.as_deref(), Some("manager"));
    assert_eq!(row.meta.avatar.as_deref(), Some("#ff8800"));
    assert_eq!(row.meta.story.as_deref(), Some("SW-1"));

    // The roster row `list_sessions` serves for it names the launch, not a bare live PTY.
    let listed = session_row(row, true);
    assert_eq!(listed.handle, handle);
    assert_eq!(listed.agent_revision.as_deref(), Some(MANAGER));
    assert_eq!(listed.kind, KIND_DASHBOARD_LAUNCHED);
}

#[test]
fn a_restarted_app_restores_an_opened_tabs_handle_team_and_name() {
    // A tab `open_terminal_tab` opened: its handle, its workspace's team and its name.
    let before = DashboardLaunchState::new(4);
    before.register("term-tab-1", "sid-tab", Some("codesense"));
    before.set_launch_team_if_absent(
        "term-tab-1",
        LaunchTeam {
            org: "acme".into(),
            team: "backend".into(),
        },
    );
    before.set_display_name("term-tab-1", "test C");
    let mut tags = Tags::new();
    apply(
        &mut tags,
        before.launch_tags_for_session("sid-tab").unwrap(),
    );

    // The app restarts; the session server keeps the tab and its tags.
    let launches = restorable_launches(&snapshot(vec![tab("sid-tab", tags, EntryState::Live)]));
    assert_eq!(launches.len(), 1);
    let after = DashboardLaunchState::new(4);
    assert!(after.restore_launch(&launches[0], None, None));
    // Mail and tools addressed to its handle still reach it, in its own team's inbox.
    assert_eq!(after.resolve("term-tab-1").as_deref(), Some("sid-tab"));
    assert_eq!(
        after.launch_team("term-tab-1"),
        Some(LaunchTeam {
            org: "acme".into(),
            team: "backend".into(),
        })
    );
    assert_eq!(after.display_name("term-tab-1").as_deref(), Some("test C"));
}

#[test]
fn a_restored_launch_counts_against_a_full_cap_so_new_launches_wait() {
    let state = DashboardLaunchState::new(1);
    assert!(state.begin("busy").unwrap());
    let launch = manager_launch("sid-ctl");
    assert!(
        state.restore_launch(&launch, None, None),
        "a running agent is never refused"
    );
    assert_eq!(state.available(), 0);
    state.abort("busy");
    assert!(
        state.begin("next").is_err(),
        "the restored launch still holds a slot"
    );
}

#[test]
fn exited_remote_and_untagged_tabs_are_not_restored() {
    let launch = tags(&[(TAG_LAUNCH_HANDLE, "h")]);
    let mut remote = launch.clone();
    remote.insert("remoteHandle".into(), "r".into());
    let launches = restorable_launches(&snapshot(vec![
        tab("exited", launch.clone(), exited()),
        tab("remote", remote, EntryState::Live),
        tab("plain", tags(&[("hidden", "tab")]), EntryState::Live),
        tab(
            "blank",
            tags(&[(TAG_LAUNCH_HANDLE, "  ")]),
            EntryState::Live,
        ),
    ]));
    assert!(launches.is_empty(), "{launches:?}");
}

#[test]
fn of_two_live_tabs_naming_one_handle_the_more_recently_bound_wins() {
    let older = tags(&[(TAG_LAUNCH_HANDLE, "h"), (TAG_LAUNCH_BOUND_AT, "100")]);
    let newer = tags(&[(TAG_LAUNCH_HANDLE, "h"), (TAG_LAUNCH_BOUND_AT, "200")]);
    for tabs in [
        vec![
            tab("sid-old", older.clone(), EntryState::Live),
            tab("sid-new", newer.clone(), EntryState::Live),
        ],
        vec![
            tab("sid-new", newer.clone(), EntryState::Live),
            tab("sid-old", older.clone(), EntryState::Live),
        ],
    ] {
        let launches = restorable_launches(&snapshot(tabs));
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].session, "sid-new");
    }
}

#[test]
fn restoring_is_idempotent_never_overwrites_a_launch_and_takes_over_a_plain_registration() {
    let launch = manager_launch("sid-ctl");

    let state = DashboardLaunchState::new(4);
    assert!(state.restore_launch(&launch, None, Some(MANAGER.into())));
    assert!(!state.restore_launch(&launch, None, None), "second pass");
    assert_eq!(
        state.agent_revision_for_session("sid-ctl").as_deref(),
        Some(MANAGER)
    );

    // The handle already runs in another session in this process.
    let live = DashboardLaunchState::new(4);
    assert!(live.begin(&manager_handle()).unwrap());
    live.bind(&manager_handle(), "sid-live", None).unwrap();
    assert!(!live.restore_launch(&launch, None, None));
    assert_eq!(live.resolve(&manager_handle()).as_deref(), Some("sid-live"));

    // The session runs another launch.
    let taken = DashboardLaunchState::new(4);
    assert!(taken.begin("other").unwrap());
    taken.bind("other", "sid-ctl", None).unwrap();
    assert!(!taken.restore_launch(&launch, None, None));
    assert_eq!(
        taken.handle_for_session("sid-ctl").as_deref(),
        Some("other")
    );

    // The nav named the tab its workspace's main session before the restore ran.
    let nav = DashboardLaunchState::new(4);
    nav.register(CONTENT, "sid-ctl", Some("codesense"));
    assert!(nav.restore_launch(&launch, None, Some(MANAGER.into())));
    assert_eq!(nav.handle_for_session("sid-ctl"), Some(manager_handle()));
    assert!(!nav.has_handle(CONTENT));
    // ...and the nav naming it again afterwards never relabels it.
    assert_eq!(
        nav.register_owned(CONTENT, "sid-ctl", Some("codesense"), None),
        Ok(false)
    );
    assert_eq!(nav.handle_for_session("sid-ctl"), Some(manager_handle()));
}

#[test]
fn the_restore_runs_once_unless_it_stopped_short() {
    let once = RestoreOnce::default();
    assert!(once.begin());
    assert!(!once.begin(), "one at a time");
    once.release();
    assert!(
        once.begin(),
        "a later layout read starts a restore that stopped short"
    );

    let fresh = RestoreOnce::default();
    fresh.settle();
    assert!(
        !fresh.begin(),
        "a server this process started has nothing to restore"
    );

    assert!(retry_delay(1) < retry_delay(2));
    assert_eq!(retry_delay(50), std::time::Duration::from_secs(30));
}

#[test]
fn the_inbox_gate_reads_a_released_restore_as_settled_but_a_wake_waits() {
    use crate::routines::wake::Restore;
    let once = RestoreOnce::default();
    // A fresh boot, before the first layout read starts a restore: not settled.
    assert_eq!(once.state(), Restore::Pending);
    assert!(!once.caller_gate_settled());
    assert!(once.begin());
    assert!(!once.caller_gate_settled(), "a restore pass runs");
    once.release();
    assert_eq!(once.state(), Restore::Pending, "a wake still waits");
    assert!(
        once.caller_gate_settled(),
        "nothing restores now: the inbox does not refuse forever"
    );
    assert!(once.begin());
    assert!(
        !once.caller_gate_settled(),
        "a later layout read restores again"
    );
    once.give_up();
    assert_eq!(once.state(), Restore::GaveUp);
    assert!(once.caller_gate_settled());
    assert!(once.begin());
    assert!(!once.caller_gate_settled());
    once.settle();
    assert!(once.caller_gate_settled());
}

#[test]
fn launch_tags_are_an_explicit_list() {
    for key in LAUNCH_TAGS {
        assert!(is_launch_tag(key), "{key}");
    }
    assert!(!is_launch_tag("hidden"));
    assert!(!is_launch_tag("remoteHandle"));
    assert!(!is_launch_tag("launchedFrom"));
}

// ---- The restore pass, against a fake app around a real registry ----------------------

struct FakeHost {
    state: DashboardLaunchState,
    signed_in: Result<Option<String>, String>,
    revisions: HashMap<String, RestoredRevision>,
    alive: HashMap<String, Result<bool, String>>,
    unclaimable: HashSet<String>,
    probes: Mutex<Vec<(String, String)>>,
    announced: AtomicUsize,
}

impl FakeHost {
    fn new() -> Self {
        Self {
            state: DashboardLaunchState::new(4),
            signed_in: Ok(Some("user-1".into())),
            revisions: HashMap::from([(
                MANAGER.to_string(),
                RestoredRevision::Verified {
                    revision: MANAGER.into(),
                    binary: "claude".into(),
                },
            )]),
            alive: HashMap::from([("sid-ctl".to_string(), Ok(true))]),
            unclaimable: HashSet::new(),
            probes: Mutex::new(Vec::new()),
            announced: AtomicUsize::new(0),
        }
    }

    fn run(&self, snapshot: &LayoutSnapshot) -> bool {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(restore_local_launches(self, snapshot))
    }

    fn announced(&self) -> usize {
        self.announced.load(Ordering::SeqCst)
    }
}

impl RestoreHost for FakeHost {
    fn state(&self) -> &DashboardLaunchState {
        &self.state
    }

    async fn signed_in(&self) -> Result<Option<String>, String> {
        self.signed_in.clone()
    }

    async fn check_revision(&self, label: &str) -> RestoredRevision {
        self.revisions
            .get(label)
            .cloned()
            .unwrap_or_else(|| RestoredRevision::Rejected("unknown".into()))
    }

    async fn agent_alive(&self, session: &str, binary: &str) -> Result<bool, String> {
        self.probes
            .lock()
            .unwrap()
            .push((session.to_string(), binary.to_string()));
        self.alive.get(session).cloned().unwrap_or(Ok(false))
    }

    async fn commit_restore(
        &self,
        launch: &RestoredLaunch,
        agent_revision: Option<String>,
    ) -> Result<bool, String> {
        if self.unclaimable.contains(&launch.session) {
            return Err("sidecar ownership is unknown".into());
        }
        Ok(self.state.restore_launch(launch, None, agent_revision))
    }

    fn announce(&self) {
        self.announced.fetch_add(1, Ordering::SeqCst);
    }
}

fn manager_layout() -> LayoutSnapshot {
    snapshot(vec![tab(
        "sid-ctl",
        manager_tags("sid-ctl"),
        EntryState::Live,
    )])
}

#[test]
fn a_pass_restores_a_running_agent() {
    let host = FakeHost::new();
    assert!(host.run(&manager_layout()));
    assert_eq!(
        host.state.agent_revision_for_session("sid-ctl").as_deref(),
        Some(MANAGER)
    );
    assert_eq!(
        *host.probes.lock().unwrap(),
        vec![("sid-ctl".to_string(), "claude".to_string())]
    );
    assert_eq!(host.announced(), 1);

    // Idempotent: a second pass finds it restored and changes nothing.
    assert!(host.run(&manager_layout()));
    assert_eq!(host.announced(), 1);
}

#[test]
fn a_tab_whose_agent_no_longer_runs_is_left_for_a_fresh_start() {
    let mut host = FakeHost::new();
    host.alive.insert("sid-ctl".into(), Ok(false));
    assert!(host.run(&manager_layout()), "settled: nothing to retry");
    assert!(!host.state.has_handle(&manager_handle()));
    assert_eq!(host.announced(), 0);
}

#[test]
fn an_unreadable_process_probe_defers_the_tab() {
    let mut host = FakeHost::new();
    host.alive.insert(
        "sid-ctl".into(),
        Err("owner process telemetry unavailable".into()),
    );
    assert!(!host.run(&manager_layout()));
    assert!(!host.state.has_handle(&manager_handle()));

    host.alive.insert("sid-ctl".into(), Ok(true));
    assert!(host.run(&manager_layout()), "the next pass restores it");
    assert!(host.state.has_handle(&manager_handle()));
}

#[test]
fn a_rejected_revision_is_skipped_and_a_transient_one_deferred() {
    let mut host = FakeHost::new();
    host.revisions.insert(
        MANAGER.into(),
        RestoredRevision::Rejected("revision 2 of acme/backend/manager not found".into()),
    );
    assert!(host.run(&manager_layout()));
    assert!(!host.state.has_handle(&manager_handle()));
    assert!(host.probes.lock().unwrap().is_empty());

    let mut host = FakeHost::new();
    host.revisions.insert(
        MANAGER.into(),
        RestoredRevision::Transient("agent not on disk yet".into()),
    );
    assert!(!host.run(&manager_layout()));
    assert!(!host.state.has_handle(&manager_handle()));
}

#[test]
fn a_launch_of_another_account_is_skipped_and_an_unknown_account_defers() {
    let mut host = FakeHost::new();
    host.signed_in = Ok(Some("user-2".into()));
    assert!(host.run(&manager_layout()));
    assert!(!host.state.has_handle(&manager_handle()));

    let mut host = FakeHost::new();
    host.signed_in = Ok(None);
    assert!(
        host.run(&manager_layout()),
        "signed out: not this account's"
    );
    assert!(!host.state.has_handle(&manager_handle()));

    let mut host = FakeHost::new();
    host.signed_in = Err("WatcherState not registered".into());
    assert!(!host.run(&manager_layout()));
    assert!(!host.state.has_handle(&manager_handle()));
}

#[test]
fn an_unclaimable_session_defers_and_a_plain_launch_needs_no_probe() {
    let mut host = FakeHost::new();
    host.unclaimable.insert("sid-ctl".into());
    assert!(!host.run(&manager_layout()));
    assert!(!host.state.has_handle(&manager_handle()));

    let host = FakeHost::new();
    let plain = snapshot(vec![tab(
        "sid-shell",
        tags(&[(TAG_LAUNCH_HANDLE, "/wt/x"), (TAG_LAUNCHER, "codesense")]),
        EntryState::Live,
    )]);
    assert!(host.run(&plain));
    assert_eq!(host.state.resolve("/wt/x").as_deref(), Some("sid-shell"));
    assert!(host.probes.lock().unwrap().is_empty());
}

/// The seam with a real terminal host across an app restart: the patch a bind writes
/// lands on the tab through the host's own tag verb, and the snapshot a NEW app launch
/// (another boot id) reads restores the launch.
#[cfg(unix)]
#[test]
fn a_real_hosts_layout_carries_the_bound_launch_to_a_restarted_app() {
    use codesense_pty_core::domain::{ClientBootId, TerminalDomain, ViewerId};
    use codesense_pty_core::layout::{OpenTab, ServerIdentity, WorkspaceDescriptor};
    use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner};
    use codesense_pty_core::TerminalHost;
    use codesense_session_server::testing::{registration, sh_spec, RecordingSink};
    use std::sync::Arc;

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let host = TerminalHost::new(
        ServerIdentity::ephemeral("restore"),
        Arc::new(RecordingSink::default()),
        runtime.handle().clone(),
    );
    let viewer = ViewerId::new("app");
    host.layout_snapshot(&viewer, &ClientBootId::new("boot-1"))
        .unwrap();
    let session = PtySessionId::new("sid-ctl");
    host.open_tab(
        OpenTab {
            viewer: viewer.clone(),
            workspace: WorkspaceKey::new("acme:backend|ws-content"),
            session: session.clone(),
            descriptor: Some(WorkspaceDescriptor {
                display_name: "content".into(),
                kind: "generic".into(),
                ..WorkspaceDescriptor::default()
            }),
            name: None,
            role: None,
            tags: Tags::new(),
            placement: Placement::Tab,
            index: None,
            activate: true,
        },
        Box::new(|| {
            let pty = runtime
                .block_on(LocalPtyRunner.spawn(sh_spec("sid-ctl", "sleep 30")))
                .map_err(|e| e.to_string())?;
            host.register(pty, registration("sid-ctl", None, false))
        }),
    )
    .unwrap();

    let before = DashboardLaunchState::new(2);
    bound_manager(&before, "sid-ctl");
    host.set_tags(&session, before.launch_tags_for_session("sid-ctl").unwrap())
        .unwrap();

    // The app restarts: a new boot reads the layout the session server kept.
    let restarted = host
        .layout_snapshot(&viewer, &ClientBootId::new("boot-2"))
        .unwrap();
    let fake = FakeHost::new();
    assert!(fake.run(&restarted));
    assert_eq!(
        fake.state.resolve(&manager_handle()).as_deref(),
        Some("sid-ctl")
    );
    assert_eq!(
        fake.state.agent_revision_for_session("sid-ctl").as_deref(),
        Some(MANAGER)
    );
    host.kill(&session).unwrap();
}

// ──────────────────────────────────────────────────────────────────

/// A bound entry with a deadline writes `launchDeadlineAt` into its tab tags,
/// and the restorable-launches read turns the tag back into
/// `RestoredLaunch.deadline_ms`.
#[test]
fn a_bound_launch_with_a_max_runtime_records_launch_deadline_at() {
    let state = DashboardLaunchState::new(4);
    // Record a launch with a 30m budget: deadline_ms = bound_at + 30m.
    assert!(state
        .begin_with_provenance(
            "W",
            None,
            LaunchRecord {
                meta: LaunchMeta {
                    display_name: Some("worker".into()),
                    launched_by: Some("mgr".into()),
                    ..LaunchMeta::default()
                },
                started_with: None,
                max_runtime_ms: Some(30 * 60_000),
            },
        )
        .unwrap());
    state.bind("W", "sid-w", Some("codesense")).unwrap();

    // The deadline tag is populated.
    let patch = state.launch_tags_for_session("sid-w").unwrap();
    let get = |k: &str| patch.get(k).cloned().flatten();
    let deadline_tag = get(TAG_LAUNCH_DEADLINE).expect("deadline tag written");
    let deadline: u64 = deadline_tag.parse().expect("deadline tag is numeric");
    let bound_at: u64 = get(TAG_LAUNCH_BOUND_AT)
        .expect("bound tag written")
        .parse()
        .unwrap();
    assert_eq!(
        deadline,
        bound_at + 30 * 60_000,
        "deadline = bound_at + max_runtime_ms"
    );

    // And the restorable-launches path reads it back.
    let mut recorded = Tags::new();
    apply(&mut recorded, patch);
    let launch =
        restorable_launches(&snapshot(vec![tab("sid-w", recorded, EntryState::Live)])).remove(0);
    assert_eq!(launch.deadline_ms, Some(deadline));
}

/// AC-6: a restored launch whose `launchDeadlineAt` is in the past arms the
/// sweep with that same deadline, so the overdue-restore branch of
/// `deadline::sweep` kills it on the first tick.
#[test]
fn a_restored_launch_carries_its_recorded_deadline() {
    let mut launch = manager_launch("sid-overdue");
    launch.deadline_ms = Some(1_000);
    let state = DashboardLaunchState::new(4);
    assert!(state.restore_launch(&launch, None, launch.agent_revision.clone()));
    let entries = state.deadline_snapshot();
    let entry = entries
        .iter()
        .find(|e| e.session_id == "sid-overdue")
        .expect("the restored launch arms a deadline");
    assert_eq!(entry.deadline_ms, 1_000);
    assert!(entry.restored_overdue);
    assert_eq!(entry.exit_sent_at_ms, None, "fresh exit-sent on restore");
}

/// AC-6 fallback: when the tab tag is lost, the on-disk record carries the
/// deadline forward. Here the restore sees `launch.deadline_ms = None` but the
/// in-memory on-disk mirror has one, and the restored entry's snapshot shows it.
#[test]
fn restore_falls_back_to_the_persisted_deadline() {
    use crate::commands::dashboard_launch::persistence::{
        PersistedLaunch, PersistedRegistry, Store, REGISTRY_FILENAME,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(REGISTRY_FILENAME);
    let state = DashboardLaunchState::new(4);
    // Seed the on-disk record with a deadline but NO `launched_by` field used by
    // the launch (we only need the `deadline_ms` fallback here).
    let mut registry = PersistedRegistry::default();
    registry.launches.insert(
        manager_handle(),
        PersistedLaunch {
            launcher_context: None,
            launched_by: Some("lead".into()),
            launched_at: None,
            team_key: Some("acme:backend".into()),
            handle: manager_handle(),
            deadline_ms: Some(42_424_242),
            session_id: None,
            worktree_dir: None,
        },
    );
    let mut store = Store::new(path.clone(), registry);
    // The store writes atomically; one upsert flushes the seed to disk.
    store
        .upsert(
            &manager_handle(),
            PersistedLaunch {
                launcher_context: None,
                launched_by: Some("lead".into()),
                launched_at: None,
                team_key: Some("acme:backend".into()),
                handle: manager_handle(),
                deadline_ms: Some(42_424_242),
                session_id: None,
                worktree_dir: None,
            },
        )
        .unwrap();
    state.install_persistence(path);

    // The restored launch's tag has no deadline — the fallback kicks in.
    let mut launch = manager_launch("sid-fallback");
    launch.deadline_ms = None;
    assert!(state.restore_launch(&launch, None, launch.agent_revision.clone()));
    let entries = state.deadline_snapshot();
    let entry = entries
        .iter()
        .find(|e| e.session_id == "sid-fallback")
        .expect("the fallback deadline arms the sweep");
    assert_eq!(entry.deadline_ms, 42_424_242);
}

#[test]
fn a_recorded_revision_is_verified_rejected_or_deferred() {
    use crate::commands::agents_launch::restored_agent_revision;
    let home = tempfile::tempdir().unwrap();
    let principal = crate::domain::Principal::from(7u64);
    match restored_agent_revision(home.path(), &principal, "builtin/claude@1") {
        RestoredRevision::Verified { revision, binary } => {
            assert_eq!(revision.split('@').next(), Some("builtin/claude"));
            assert!(!binary.is_empty());
        }
        other => panic!("{other:?}"),
    }
    for rejected in [
        "not a revision",
        "builtin/claude@x",
        "builtin/claude@999999999",
    ] {
        assert!(
            matches!(
                restored_agent_revision(home.path(), &principal, rejected),
                RestoredRevision::Rejected(_)
            ),
            "{rejected}"
        );
    }
    // A team agent whose team content is not on disk yet.
    assert!(matches!(
        restored_agent_revision(home.path(), &principal, MANAGER),
        RestoredRevision::Transient(_)
    ));
}

#[test]
fn a_future_restored_deadline_remains_eligible_for_grace() {
    let mut launch = manager_launch("sid-future");
    let deadline = super::super::now_epoch_ms() + 60_000;
    launch.deadline_ms = Some(deadline);
    let state = DashboardLaunchState::new(4);
    assert!(state.restore_launch(&launch, None, launch.agent_revision.clone()));
    let entry = state.deadline_snapshot().remove(0);
    assert_eq!(entry.deadline_ms, deadline);
    assert!(!entry.restored_overdue);
}

#[test]
fn normal_local_restore_keeps_the_captured_launcher_context() {
    use crate::commands::dashboard_launch::persistence::{
        PersistedLaunch, PersistedRegistry, Store, REGISTRY_FILENAME,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(REGISTRY_FILENAME);
    let mut store = Store::new(path.clone(), PersistedRegistry::default());
    store
        .upsert(
            &manager_handle(),
            PersistedLaunch {
                handle: manager_handle(),
                launcher_context: Some("cw_sprint".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let launch = manager_launch("sid-restored");
    for _ in 0..2 {
        let state = DashboardLaunchState::new(4);
        state.install_persistence(path.clone());
        assert!(state.restore_launch(&launch, None, launch.agent_revision.clone()));
        assert_eq!(
            state.launcher_context("sid-restored").as_deref(),
            Some("cw_sprint")
        );
        assert_eq!(
            state
                .persisted_launcher_context(&manager_handle())
                .as_deref(),
            Some("cw_sprint")
        );
    }
}

#[test]
fn restart_restore_recovers_workspace_only_for_the_saved_session() {
    use crate::commands::dashboard_launch::persistence::REGISTRY_FILENAME;

    fn persisted(path: &std::path::Path, session_id: &str) {
        let handle = manager_handle();
        let mut value = serde_json::json!({ "version": 1, "launches": {} });
        value["launches"][&handle] = serde_json::json!({
            "handle": handle,
            "launchedBy": "lead",
            "worktreeDir": "/worktree/worker",
            "sessionId": session_id
        });
        std::fs::write(path, value.to_string()).unwrap();
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(REGISTRY_FILENAME);
    persisted(&path, "sid-restart");
    let state = DashboardLaunchState::new(4);
    state.install_persistence(path.clone());
    let launch = manager_launch("sid-restart");
    assert!(state.restore_launch(&launch, None, launch.agent_revision.clone()));
    assert_eq!(
        state.worktree_dir_for_session("sid-restart"),
        Some(std::path::PathBuf::from("/worktree/worker"))
    );

    persisted(&path, "sid-before-restart");
    let later = DashboardLaunchState::new(4);
    later.install_persistence(path);
    let launch = manager_launch("sid-new-session");
    assert!(later.restore_launch(&launch, None, launch.agent_revision.clone()));
    assert_eq!(later.worktree_dir_for_session("sid-new-session"), None);
}

#[test]
fn chat_binding_survives_layout_restore_without_changing_story_identity() {
    let before = DashboardLaunchState::new(4);
    before.begin("saved").unwrap();
    before.bind("saved", "sid", None).unwrap();
    before.bind_chat_descriptor(
        "saved",
        "sid",
        &codesense_pty_core::layout::WorkspaceDescriptor {
            kind: "story".into(),
            story_id: Some("SV-X".into()),
            ..Default::default()
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let chat = crate::commands::workspace_chat::WorkspaceChatStore::default();
    chat.install(dir.path());
    chat.position("story:SV-X", "saved", "sid").unwrap();
    chat.note_read("story:SV-X", "saved", "sid", 4).unwrap();
    chat.note_told("story:SV-X", "saved", "sid", 6).unwrap();
    let mut recorded = Tags::new();
    apply(
        &mut recorded,
        before.launch_tags_for_session("sid").unwrap(),
    );
    let launches = restorable_launches(&snapshot(vec![tab("sid", recorded, EntryState::Live)]));
    let after = DashboardLaunchState::new(4);
    assert!(after.restore_launch(&launches[0], None, None));
    assert_eq!(
        after.workspace_for_session("sid").as_deref(),
        Some("story:SV-X")
    );
    let restored_session = after.resolve("saved").unwrap();
    assert_eq!(restored_session, "sid");
    let restored_chat = crate::commands::workspace_chat::WorkspaceChatStore::default();
    restored_chat.install(dir.path());
    let p = restored_chat
        .position("story:SV-X", "saved", &restored_session)
        .unwrap();
    assert_eq!((p.read, p.told), (4, 6));
    assert!(after.snapshot()[0].meta.story.is_none());
}

#[test]
fn a_worker_of_a_restored_routine_uses_its_durable_parent_task_and_playbook_after_reopen() {
    use crate::commands::dashboard_launch::compose_launch_write;
    use crate::session_usage::record::{LaunchFacts, SessionRecord};
    use crate::session_usage::task::{SkillVersion, TaskRecord, LINK_SESSION};
    use crate::session_usage::task_capture::{capture_launch_task, Capture, TaskInputs};
    use crate::session_usage::UsageStore;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("usage.sqlite");
    let open = || UsageStore::open(&path).unwrap();
    let mut session = SessionRecord::new("sid-routine");
    session.org = Some("acme".into());
    session.team = Some("backend".into());
    session.run_id = Some("run-routine".into());
    open().write_session_record(&session).unwrap();
    let mut parent = TaskRecord::new("run-routine", "Lead the routine");
    parent.org = session.org.clone();
    parent.team = session.team.clone();
    parent.main_skill = Some("bmad-build".into());
    parent.layers.skills = vec![SkillVersion {
        name: "bmad-build".into(), hash: Some("recorded-launch-hash".into()), reason: None,
    }];
    open().insert_task_if_absent(&parent).unwrap();
    open().add_task_link("run-routine", LINK_SESSION, "sid-routine").unwrap();
    assert!(open().read_task("sid-routine").unwrap().is_none());

    let mut launch = manager_launch("sid-routine");
    launch.routine_key = Some("acme/backend/triage".into());
    let restored = DashboardLaunchState::new(4);
    restored.set_task_store_path_for_test(path.clone());
    assert!(restored.restore_launch(&launch, None, launch.agent_revision.clone()));
    assert!(restored.snapshot()[0].provenance.is_none(), "normal restore has no transient run provenance");
    assert!(restored.begin_with_provenance("child", None, LaunchRecord {
        meta: LaunchMeta { launched_by: Some("sid-routine".into()), ..Default::default() },
        ..Default::default()
    }).unwrap());
    restored.bind("child", "sid-child", Some("codesense")).unwrap();
    let write = compose_launch_write(&restored, "sid-child", "child", LaunchFacts::default().with_task(TaskInputs {
        brief: Some("Build the child task".into()), ..Default::default()
    })).unwrap();
    let store = open();
    store.write_session_record(&write.record).unwrap();
    capture_launch_task(&store, &Capture {
        session_id: "sid-child", record: &write.record, inputs: &write.task,
        routine: write.routine.as_ref(), launcher_task: write.launcher.as_deref(),
        story_file: None, now_ms: 10,
    }).unwrap();
    drop(store);
    let view = open().task_view("run-routine").unwrap().unwrap();
    assert_eq!(view.children.len(), 1);
    assert_eq!(view.children[0].task.task_id, "sid-child");
    assert_eq!(view.children[0].task.parent_id.as_deref(), Some("run-routine"));
    assert_eq!(view.children[0].playbook["skill"], "bmad-build");
    assert_eq!(view.children[0].playbook["hash"], "recorded-launch-hash");
    assert!(open().read_task("sid-routine").unwrap().is_none());
}

#[test]
fn a_routine_launch_tag_survives_registry_restoration() {
    let before = DashboardLaunchState::new(2);
    bound_manager(&before, "sid-routine");
    before.stamp_provenance(
        &manager_handle(),
        super::super::RunProvenance {
            routine_key: Some("acme/backend/triage".into()),
            ..Default::default()
        },
    );
    let patch = before.launch_tags_for_session("sid-routine").unwrap();
    assert_eq!(
        patch.get("launchRoutineKey").cloned().flatten().as_deref(),
        Some("acme/backend/triage")
    );
    let mut held = Tags::new();
    apply(&mut held, patch);
    let launch =
        restorable_launches(&snapshot(vec![tab("sid-routine", held, EntryState::Live)])).remove(0);
    let after = DashboardLaunchState::new(2);
    assert!(after.restore_launch(&launch, None, launch.agent_revision.clone()));
    assert_eq!(
        after
            .launch_tags_for_session("sid-routine")
            .unwrap()
            .get("launchRoutineKey")
            .cloned()
            .flatten()
            .as_deref(),
        Some("acme/backend/triage")
    );
}
