// SPDX-License-Identifier: MIT
//! SW-DP-21 AC-1 to AC-6, AC-9, AC-10: the agent streams over a fixture
//! [`AgentHost`] whose launch registry is the app's real `DashboardLaunchState`.

use super::*;
use crate::commands::dashboard_launch::{DashboardLaunchState, LaunchTeam};
use std::sync::Mutex;

/// What the fixture saw of the terminal domain: only subscribes and unsubscribes
/// exist on it, and they are recorded (AC-5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    Subscribe(String),
    Unsubscribe(u64),
    /// SW-DP-23: a write of these bytes to this session.
    Write(String, String),
    /// SW-DP-23: a resize as the `dashboard` viewer.
    Resize(String, u16, u16),
}

pub(crate) struct FakeHost {
    pub team: Mutex<Option<Team>>,
    pub launch: DashboardLaunchState,
    pub rows: Mutex<Vec<SessionListRow>>,
    pub tabs: Mutex<Vec<LayoutTab>>,
    pub branches: Vec<WorktreeBranch>,
    pub titles: HashMap<String, String>,
    pub snapshot: Mutex<TerminalSnap>,
    pub calls: Mutex<Vec<Call>>,
    /// The sender of the latest subscribe's events.
    pub events: Mutex<Option<tokio::sync::mpsc::UnboundedSender<TermEvent>>>,
    next_stream: Mutex<u64>,
    /// How many session reads fail next.
    pub fail_sessions: std::sync::atomic::AtomicU32,
    /// Layout and worktree reads fail while set.
    pub layout_fails: std::sync::atomic::AtomicBool,
    pub worktrees_fail: std::sync::atomic::AtomicBool,
    /// SW-DP-23: each write takes this long (recorded when it starts).
    pub write_delay: Mutex<Option<std::time::Duration>>,
    /// SV-INP-30: the faces this Mac holds.
    pub faces: Mutex<FakeFaces>,
    /// The context index is not ready: no folder has a team until it is.
    pub not_ready: std::sync::atomic::AtomicBool,
    /// The next lookup is answered from the not-ready snapshot, then the index becomes ready.
    pub finish_after_lookup: std::sync::atomic::AtomicBool,
    /// A real context index the team is read from (as the app does), instead of `team`.
    pub index: Mutex<Option<Arc<crate::context_query::ContextIndexService>>>,
}

/// The face sources of the fake Mac.
#[derive(Default)]
pub(crate) struct FakeFaces {
    /// A saved Agent's default face ref, by key.
    pub agent_avatars: HashMap<String, String>,
    /// The human's own face ref.
    pub human: Option<String>,
    /// The refs the team deals to agents with no face, when it set a default pack.
    pub pool: Option<Vec<String>>,
    /// Team and personal pack faces, by `(pack, id)`.
    pub packs: HashMap<(String, String), (crate::avatars::FaceFormat, Vec<u8>)>,
}

pub(crate) fn acme() -> Team {
    Team {
        org: "acme".into(),
        team: "emea".into(),
    }
}

impl FakeHost {
    pub(crate) fn new() -> FakeHost {
        FakeHost {
            team: Mutex::new(Some(acme())),
            launch: DashboardLaunchState::new(16),
            rows: Mutex::new(Vec::new()),
            tabs: Mutex::new(Vec::new()),
            branches: vec![WorktreeBranch {
                story: "SW-1".into(),
                org: Some("acme".into()),
                team: Some("emea".into()),
                branch: Some("feat/SW-1-login".into()),
            }],
            titles: HashMap::from([("SW-1".to_string(), "Sign in with a passkey".to_string())]),
            snapshot: Mutex::new(TerminalSnap {
                cols: 120,
                rows: 30,
                seq: 100,
                vt: "$ claude\r\nhello\r\n".into(),
            }),
            calls: Mutex::new(Vec::new()),
            events: Mutex::new(None),
            next_stream: Mutex::new(0),
            fail_sessions: Default::default(),
            layout_fails: Default::default(),
            worktrees_fail: Default::default(),
            write_delay: Mutex::new(None),
            faces: Default::default(),
            not_ready: Default::default(),
            finish_after_lookup: Default::default(),
            index: Mutex::new(None),
        }
    }

    /// A session launched (and bound) in `team`, listed on this Mac with a live tab.
    pub(crate) fn launch(
        &self,
        handle: &str,
        team: Option<(&str, &str)>,
        workspace: &str,
        story: Option<&str>,
    ) {
        self.launch
            .begin_with_provenance(handle, None, Default::default())
            .unwrap();
        self.launch
            .bind(handle, &format!("sess-{handle}"), Some("mcp"))
            .unwrap();
        if let Some((org, team)) = team {
            self.launch.set_launch_team_if_absent(
                handle,
                LaunchTeam {
                    org: org.into(),
                    team: team.into(),
                },
            );
        }
        self.rows.lock().unwrap().push(row(
            handle,
            SessionState::Running,
            "This Mac",
            None,
            Some(workspace),
            story,
        ));
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub(crate) fn emit(&self, event: TermEvent) {
        let _ = self.events.lock().unwrap().as_ref().unwrap().send(event);
    }
}

pub(crate) fn row(
    handle: &str,
    state: SessionState,
    machine: &str,
    machine_id: Option<&str>,
    workspace: Option<&str>,
    story: Option<&str>,
) -> SessionListRow {
    SessionListRow {
        queue_role: None,
        role_join_error: None,
        id: handle.to_string(),
        team: None,
        chat_workspace_id: None,
        group_unread: 0,
        group_read_id: 0,
        handle: handle.into(),
        display_name: format!("Agent {handle}"),
        state,
        launched_by: Some("human".into()),
        story: story.map(String::from),
        workspace: workspace.map(String::from),
        role: Some("dev".into()),
        avatar: None,
        agent: Some("builtin/claude".into()),
        machine: Some(machine.into()),
        machine_id: machine_id.map(String::from),
        workspace_handle: None,
        unread: Some(0),
        oldest_unread_at: None,
        mail_notice: None,
        notified_at: None,
        subagents: 0,
        inbox_waiter: Default::default(),
        progress: None,
        launch_telemetry: None,
        release_qualified: None,
        release_note: None,
        deadline_at_ms: None,
        expiring: false,
        session: Some(format!("sess-{handle}")),
        failed: false,
    }
}

impl AgentHost for FakeHost {
    fn agent_avatar(&self, key: &str) -> Option<String> {
        self.faces.lock().unwrap().agent_avatars.get(key).cloned()
    }
    fn human_avatar(&self) -> Option<String> {
        self.faces.lock().unwrap().human.clone()
    }
    fn default_pool(&self, _team: &Team) -> Option<Vec<String>> {
        self.faces.lock().unwrap().pool.clone()
    }
    fn avatar_face(
        &self,
        _team: &Team,
        pack: &str,
        id: &str,
    ) -> Option<(crate::avatars::FaceFormat, Vec<u8>)> {
        let faces = self.faces.lock().unwrap();
        faces.packs.get(&(pack.to_string(), id.to_string())).cloned()
    }
    fn folder_team(&self, folder: &Path) -> TeamLookup {
        use std::sync::atomic::Ordering;
        if let Some(index) = self.index.lock().unwrap().as_ref() {
            return index_team(index, folder);
        }
        let lookup = TeamLookup {
            team: self.team.lock().unwrap().clone(),
            ready: !self.not_ready.load(Ordering::SeqCst),
        };
        // The index finishes right after this lookup read the empty snapshot.
        if self.finish_after_lookup.swap(false, Ordering::SeqCst) {
            *self.team.lock().unwrap() = Some(acme());
            self.not_ready.store(false, Ordering::SeqCst);
        }
        lookup
    }
    fn sessions(&self) -> BoxFuture<'static, Result<Vec<SessionListRow>, String>> {
        use std::sync::atomic::Ordering;
        let failing = self
            .fail_sessions
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        let rows = self.rows.lock().unwrap().clone();
        Box::pin(async move {
            if failing {
                Err("the registry is busy".to_string())
            } else {
                Ok(rows)
            }
        })
    }
    fn launch_team(&self, handle: &str) -> Option<Team> {
        self.launch.launch_team(handle).map(|t| Team {
            org: t.org,
            team: t.team,
        })
    }
    fn resolve(&self, handle: &str) -> Option<String> {
        self.launch.resolve(handle)
    }
    fn layout(&self) -> BoxFuture<'static, Option<Vec<LayoutTab>>> {
        let tabs = self.tabs.lock().unwrap().clone();
        let fails = self.layout_fails.load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move { (!fails).then_some(tabs) })
    }
    fn worktrees(&self) -> BoxFuture<'static, Option<Vec<WorktreeBranch>>> {
        let b = self.branches.clone();
        let fails = self
            .worktrees_fail
            .load(std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move { (!fails).then_some(b) })
    }
    fn story_title(&self, team: &Team, id: &str) -> Option<String> {
        (team == &acme())
            .then(|| self.titles.get(id).cloned())
            .flatten()
    }
    fn subscribe(&self, session: String) -> BoxFuture<'static, Result<TerminalWatch, String>> {
        self.calls.lock().unwrap().push(Call::Subscribe(session));
        let (tx, events) = tokio::sync::mpsc::unbounded_channel();
        *self.events.lock().unwrap() = Some(tx);
        let mut next = self.next_stream.lock().unwrap();
        *next += 1;
        let watch = TerminalWatch {
            snapshot: self.snapshot.lock().unwrap().clone(),
            handle: WatchHandle {
                stream: *next,
                listeners: Vec::new(),
            },
            events,
        };
        Box::pin(async move { Ok(watch) })
    }
    fn write(&self, session: String, data: String) -> BoxFuture<'static, Result<(), String>> {
        self.calls.lock().unwrap().push(Call::Write(session, data));
        let delay = *self.write_delay.lock().unwrap();
        Box::pin(async move {
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            Ok(())
        })
    }
    fn resize(
        &self,
        session: String,
        cols: u16,
        rows: u16,
    ) -> BoxFuture<'static, Result<(), String>> {
        self.calls
            .lock()
            .unwrap()
            .push(Call::Resize(session, cols, rows));
        Box::pin(async { Ok(()) })
    }
    fn unsubscribe(&self, handle: WatchHandle) {
        self.calls
            .lock()
            .unwrap()
            .push(Call::Unsubscribe(handle.stream));
    }
}

const EMEA: Option<(&str, &str)> = Some(("acme", "emea"));

fn ctx(host: &Arc<FakeHost>) -> LiveCtx {
    let agents: Arc<dyn AgentHost> = host.clone();
    LiveCtx {
        agents: Some(agents),
        ..Default::default()
    }
}

/// A call of a dashboard published with `scope` saved (`EMEA` as publish saves it).
fn call(args: Value, scope: Option<(&str, &str)>) -> StreamCall {
    StreamCall {
        args,
        fields: Vec::new(),
        folder: PathBuf::from("/tmp/dash"),
        scope: scope.map(|(org, team)| super::super::state::Scope {
            org: org.into(),
            team: team.into(),
        }),
        snap: Arc::new(tokio::sync::Notify::new()),
    }
}

/// The two-team fixture of AC-1.
fn two_teams() -> Arc<FakeHost> {
    let host = FakeHost::new();
    host.launch("a1", Some(("acme", "emea")), "SW-1", Some("SW-1"));
    host.launch("a2", Some(("acme", "emea")), "ops", None);
    host.launch("b1", Some(("acme", "apac")), "SW-9", Some("SW-9"));
    host.launch("n1", None, "scratch", None);
    // Remote: the same team, on another machine.
    host.launch("r1", Some(("acme", "emea")), "SW-1", None);
    // Exited, and listed but never bound.
    host.launch("x1", Some(("acme", "emea")), "SW-1", None);
    {
        let mut rows = host.rows.lock().unwrap();
        for r in rows.iter_mut() {
            if r.handle == "r1" {
                r.machine = Some("build-box".into());
                r.machine_id = Some("m-2".into());
            }
            if r.handle == "x1" {
                r.state = SessionState::Exited;
            }
        }
        rows.push(row(
            "u1",
            SessionState::Running,
            "This Mac",
            None,
            Some("SW-1"),
            None,
        ));
    }
    Arc::new(host)
}

async fn first(items: &mut Items) -> StreamItem {
    items.next().await.expect("an item")
}

/// AC-1: only this Mac's live sessions launched in the dashboard's team, with the
/// `sessions` fields plus `watchable`; never another team's, a teamless one, a
/// remote one, an exited one or one with no binding. Redacted.
#[tokio::test]
async fn terminals_lists_only_the_teams_local_live_sessions() {
    let host = two_teams();
    host.rows.lock().unwrap()[0].display_name = "token sk-ant-abcdefghijklmnop".into();
    let mut items = open_terminals(ctx(&host), call(json!({}), Some(("acme", "emea"))))
        .await
        .unwrap();
    let StreamItem::Value(value) = first(&mut items).await else {
        panic!("a value")
    };
    let rows = value["terminals"].as_array().unwrap();
    let handles: Vec<&str> = rows.iter().map(|r| r["handle"].as_str().unwrap()).collect();
    assert_eq!(handles, ["a1", "a2"]);
    let a1 = &rows[0];
    assert_eq!(a1["watchable"], true);
    assert_eq!(a1["state"], "running");
    assert_eq!(a1["machine"], "This Mac");
    assert!(a1.get("session").is_none() && a1.get("machineId").is_none());
    // DP-INV-10 as amended: the list is redacted.
    assert!(!a1["displayName"]
        .as_str()
        .unwrap()
        .contains("sk-ant-abcdefghijklmnop"));
    // The same fields as the sessions query's rows, plus workspace and watchable.
    let mut keys: Vec<&str> = a1.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    let shared = catalog::session_value(&host.rows.lock().unwrap()[0], &[]);
    let mut want: Vec<&str> = shared
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    want.extend(["watchable", "workspace"]);
    want.sort();
    assert_eq!(keys, want);
}

/// AC-1: a folder in no team's context repo is refused `no_team`; a folder
/// now in another team than the one saved at publish is `not_found`.
#[tokio::test]
async fn a_folder_with_no_team_is_refused() {
    let host = two_teams();
    *host.team.lock().unwrap() = None;
    for open in [open_terminals, open_workspaces] {
        let refused = open(ctx(&host), call(json!({}), EMEA)).await.err();
        assert_eq!(refused, Some(QueryError::Refused("no_team")));
    }
    let refused = open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
        .await
        .err();
    assert_eq!(refused, Some(QueryError::Refused("no_team")));
    // No team saved at publish: refused too, as queryContext is (check_scope).
    *host.team.lock().unwrap() = Some(acme());
    let unsaved = open_terminals(ctx(&host), call(json!({}), None))
        .await
        .err();
    assert_eq!(unsaved, Some(QueryError::Refused("no_team")));
    *host.team.lock().unwrap() = Some(acme());
    let moved = open_terminals(ctx(&host), call(json!({}), Some(("acme", "apac"))))
        .await
        .err();
    assert_eq!(moved, Some(QueryError::Refused("not_found")));
    let args = open_terminals(ctx(&host), call(json!({"x": 1}), EMEA))
        .await
        .err();
    assert!(matches!(args, Some(QueryError::InvalidArgs(_))));
}

/// AC-2: a session starting or ending gives one value within 3 s; an unchanged
/// list gives none.
#[tokio::test(start_paused = true)]
async fn the_list_is_sent_only_when_it_changes() {
    let host = two_teams();
    let mut items = open_terminals(ctx(&host), call(json!({}), EMEA))
        .await
        .unwrap();
    assert!(matches!(first(&mut items).await, StreamItem::Value(_)));
    // Ten unchanged seconds: nothing.
    let quiet = tokio::time::timeout(Duration::from_secs(10), items.next()).await;
    assert!(quiet.is_err(), "an unchanged list sent a value");
    host.launch("a3", Some(("acme", "emea")), "ops", None);
    let started = Instant::now();
    let StreamItem::Value(v) = first(&mut items).await else {
        panic!()
    };
    assert!(started.elapsed() <= Duration::from_secs(3));
    assert_eq!(v["terminals"].as_array().unwrap().len(), 3);
    host.rows.lock().unwrap().retain(|r| r.handle != "a3");
    let ended = Instant::now();
    let StreamItem::Value(v) = first(&mut items).await else {
        panic!()
    };
    assert!(ended.elapsed() <= Duration::from_secs(3));
    assert_eq!(v["terminals"].as_array().unwrap().len(), 2);
    // A team change ends the list.
    *host.team.lock().unwrap() = Some(Team {
        org: "acme".into(),
        team: "apac".into(),
    });
    assert_eq!(first(&mut items).await, StreamItem::End("not_found"));
    assert!(items.next().await.is_none());
}

/// AC-9: each row's workspace: id as list_sessions reports it, the story's title
/// and worktree branch for a story workspace, the id as the name otherwise.
#[tokio::test]
async fn each_row_names_its_workspace() {
    let host = two_teams();
    let mut items = open_terminals(ctx(&host), call(json!({}), EMEA))
        .await
        .unwrap();
    let StreamItem::Value(v) = first(&mut items).await else {
        panic!()
    };
    let rows = v["terminals"].as_array().unwrap();
    assert_eq!(
        rows[0]["workspace"],
        json!({"id": "SW-1", "name": "Sign in with a passkey", "story": "SW-1", "branch": "feat/SW-1-login"})
    );
    assert_eq!(rows[1]["workspace"], json!({"id": "ops", "name": "ops"}));
    // workspace.id is the list_sessions `workspace` of the same handle.
    let listed = host.rows.lock().unwrap().clone();
    for r in rows {
        let same = listed.iter().find(|l| l.handle == r["handle"]).unwrap();
        assert_eq!(r["workspace"]["id"].as_str(), same.workspace.as_deref());
    }
}

fn tab(key: &str) -> LayoutTab {
    LayoutTab {
        workspace_key: key.into(),
        workspace: key.split_once('|').unwrap().1.into(),
    }
}

/// AC-10: every workspace of the team in this Mac's layout, with its sessions and
/// tab count; none of another team or of no team; the key (with the org) never
/// sent; opening one gives one value within 3 s.
#[tokio::test(start_paused = true)]
async fn workspaces_lists_the_teams_open_workspaces() {
    let host = two_teams();
    *host.tabs.lock().unwrap() = vec![
        tab("acme:emea|SW-1"),
        tab("acme:emea|SW-1"),
        tab("acme:emea|SW-1"),
        tab("acme:emea|ops"),
        tab("acme:apac|SW-9"),
        tab("acme:_none|scratch"),
        tab("acme:emeax|SW-2"),
    ];
    let mut items = open_workspaces(ctx(&host), call(json!({}), Some(("acme", "emea"))))
        .await
        .unwrap();
    let StreamItem::Value(v) = first(&mut items).await else {
        panic!()
    };
    assert_eq!(
        v,
        json!({"workspaces": [
            {"id": "SW-1", "name": "Sign in with a passkey", "story": "SW-1", "branch": "feat/SW-1-login", "sessions": ["a1"], "tabs": 3},
            {"id": "ops", "name": "ops", "sessions": ["a2"], "tabs": 1},
        ]})
    );
    assert!(!v.to_string().contains("acme"));
    host.tabs.lock().unwrap().push(tab("acme:emea|SW-3"));
    let opened = Instant::now();
    let StreamItem::Value(v) = first(&mut items).await else {
        panic!()
    };
    assert!(opened.elapsed() <= Duration::from_secs(3));
    assert_eq!(
        v["workspaces"][1],
        json!({"id": "SW-3", "name": "SW-3", "sessions": [], "tabs": 1})
    );
    let quiet = tokio::time::timeout(Duration::from_secs(10), items.next()).await;
    assert!(quiet.is_err());
}

/// The home path of the owner, as a generic workspace id carries it on a real Mac.
const HOME_DIR: &str =
    "/Users/grace/.codesense/users/1234567/_worker/acme/ctx/riverside/auto-worker";

/// SW-DP-21 follow-up: a generic or root workspace is named by its folder's
/// basename, and its id is an opaque key: no local path leaves the Mac in
/// either list. The id still joins a `terminals` row to its `workspaces` entry.
#[tokio::test]
async fn path_workspaces_get_a_friendly_name_and_no_path() {
    let host = two_teams();
    let generic = format!("generic:{HOME_DIR}");
    let root = "root:/Users/grace/src/codesense/".to_string();
    host.launch("g1", Some(("acme", "emea")), &generic, None);
    host.launch("g2", Some(("acme", "emea")), &root, None);
    *host.tabs.lock().unwrap() = vec![
        tab(&format!("acme:emea|{generic}")),
        tab(&format!("acme:emea|{generic}")),
        tab(&format!("acme:emea|{root}")),
        tab("acme:emea|root:octocat-dev/codesense"),
        tab("acme:emea|generic:C:\\Users\\grace\\work\\ops-box"),
    ];
    let mut terminals = open_terminals(ctx(&host), call(json!({}), EMEA))
        .await
        .unwrap();
    let StreamItem::Value(t) = first(&mut terminals).await else {
        panic!()
    };
    let mut workspaces = open_workspaces(ctx(&host), call(json!({}), EMEA))
        .await
        .unwrap();
    let StreamItem::Value(w) = first(&mut workspaces).await else {
        panic!()
    };
    for v in [&t, &w] {
        let text = v.to_string();
        assert!(!text.contains("/Users"), "a home path left the Mac: {text}");
        assert!(
            !text.contains("grace"),
            "a user name left the Mac: {text}"
        );
        assert!(
            !text.contains("octocat-dev"),
            "a slug owner left the Mac: {text}"
        );
        assert!(
            !text.contains("C:\\\\Users"),
            "a Windows path left the Mac: {text}"
        );
    }
    let rows = t["terminals"].as_array().unwrap();
    let g1 = rows.iter().find(|r| r["handle"] == "g1").unwrap();
    let g2 = rows.iter().find(|r| r["handle"] == "g2").unwrap();
    assert_eq!(g1["workspace"]["name"], "auto-worker");
    assert_eq!(g2["workspace"]["name"], "codesense");
    let list = w["workspaces"].as_array().unwrap();
    let named = |name: &str| list.iter().find(|x| x["name"] == name).unwrap().clone();
    let auto = named("auto-worker");
    assert_eq!(auto["tabs"], 2);
    assert_eq!(auto["sessions"], json!(["g1"]));
    // The same opaque id in both lists, so a page can join them.
    assert_eq!(auto["id"], g1["workspace"]["id"]);
    assert_eq!(named("codesense")["sessions"], json!(["g2"]));
    assert_eq!(named("codesense")["id"], g2["workspace"]["id"]);
    assert_eq!(named("ops-box")["sessions"], json!([]));
    // A repo slug is no local path: its basename names it too.
    assert!(list
        .iter()
        .any(|x| x["name"] == "codesense" && x["tabs"] == 1 && x["sessions"] == json!([])));
    // Story and plain ids are unchanged.
    let sw1 = rows.iter().find(|r| r["handle"] == "a1").unwrap();
    assert_eq!(sw1["workspace"]["id"], "SW-1");
    let opaque = auto["id"].as_str().unwrap();
    assert!(opaque.starts_with("ws-") && opaque.len() == 19, "{opaque}");
    assert_ne!(named("codesense")["id"], named("ops-box")["id"]);
}

#[test]
fn friendly_names_and_public_ids() {
    assert_eq!(
        friendly_workspace_name("generic:/a/b/auto-worker"),
        "auto-worker"
    );
    assert_eq!(
        friendly_workspace_name("generic:/a/b/auto-worker/"),
        "auto-worker"
    );
    assert_eq!(friendly_workspace_name("root:/x/repo"), "repo");
    assert_eq!(friendly_workspace_name("root:owner/repo"), "repo");
    assert_eq!(
        friendly_workspace_name("generic:C:\\Users\\a\\proj"),
        "proj"
    );
    assert_eq!(friendly_workspace_name("story:SW-1"), "SW-1");
    assert_eq!(friendly_workspace_name("generic:/"), "Workspace");
    assert_eq!(friendly_workspace_name("generic:"), "Workspace");
    assert_eq!(friendly_workspace_name("ops"), "ops");
    assert_eq!(friendly_workspace_name("git-sync"), "git-sync");
    assert_eq!(public_workspace_id("SW-1"), "SW-1");
    assert_eq!(public_workspace_id("ops"), "ops");
    let a = public_workspace_id("generic:/Users/x/a");
    assert_eq!(a, public_workspace_id("generic:/Users/x/a"));
    assert_ne!(a, public_workspace_id("generic:/Users/y/a"));
    assert!(!a.contains("Users"));
    assert!(!public_workspace_id("generic:C:\\Users\\a").contains("Users"));
    assert!(!public_workspace_id("root:owner/repo").contains('/'));
}

/// AC-3: the snapshot (with the terminal's size) first, then output past its
/// seq, in order, with no repeat; resizes in order.
#[tokio::test]
async fn a_terminal_sends_its_snapshot_then_its_output() {
    let host = two_teams();
    let mut items = open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
        .await
        .unwrap();
    assert_eq!(host.calls(), [Call::Subscribe("sess-a1".into())]);
    assert_eq!(
        first(&mut items).await,
        StreamItem::Snapshot {
            vt: "$ claude\r\nhello\r\n".into(),
            cols: Some(120),
            rows: Some(30)
        }
    );
    host.emit(TermEvent::Output {
        data: "in the snapshot".into(),
        seq: 100,
    });
    host.emit(TermEvent::Output {
        data: "one".into(),
        seq: 104,
    });
    host.emit(TermEvent::Output {
        data: "again".into(),
        seq: 104,
    });
    host.emit(TermEvent::Resized {
        cols: 100,
        rows: 40,
    });
    host.emit(TermEvent::Output {
        data: "two".into(),
        seq: 108,
    });
    assert_eq!(first(&mut items).await, StreamItem::Bytes("one".into()));
    assert_eq!(
        first(&mut items).await,
        StreamItem::Resized {
            cols: 100,
            rows: 40
        }
    );
    assert_eq!(first(&mut items).await, StreamItem::Bytes("two".into()));
}

/// AC-3: a `snap` subscribes again and sends the fresh snapshot, then lets the
/// old subscription go.
#[tokio::test]
async fn a_snap_resubscribes_before_it_lets_go() {
    let host = two_teams();
    let c = call(json!({"handle": "a1"}), EMEA);
    let snap = c.snap.clone();
    let mut items = open_terminal(ctx(&host), c).await.unwrap();
    first(&mut items).await;
    host.snapshot.lock().unwrap().seq = 200;
    snap.notify_one();
    assert!(matches!(
        first(&mut items).await,
        StreamItem::Snapshot { .. }
    ));
    assert_eq!(
        host.calls(),
        [
            Call::Subscribe("sess-a1".into()),
            Call::Subscribe("sess-a1".into()),
            Call::Unsubscribe(1)
        ]
    );
    host.emit(TermEvent::Output {
        data: "old".into(),
        seq: 150,
    });
    host.emit(TermEvent::Output {
        data: "new".into(),
        seq: 201,
    });
    assert_eq!(first(&mut items).await, StreamItem::Bytes("new".into()));
}

/// AC-4: an unknown handle, another team's handle, a teamless, remote or
/// exited session's handle and a raw session id all get the same `not_found`,
/// and nothing is subscribed.
#[tokio::test]
async fn a_handle_outside_the_team_is_not_found() {
    let host = two_teams();
    for handle in ["nope", "b1", "n1", "r1", "x1", "u1", "sess-a1", ""] {
        let got = open_terminal(ctx(&host), call(json!({"handle": handle}), EMEA))
            .await
            .err();
        assert_eq!(got, Some(QueryError::Refused("not_found")), "{handle}");
    }
    let long = "h".repeat(MAX_HANDLE + 1);
    let got = open_terminal(ctx(&host), call(json!({"handle": long}), EMEA))
        .await
        .err();
    assert_eq!(got, Some(QueryError::Refused("not_found")));
    assert!(matches!(
        open_terminal(ctx(&host), call(json!({}), EMEA)).await.err(),
        Some(QueryError::InvalidArgs(_))
    ));
    assert!(host.calls().is_empty());
}

/// AC-5 and AC-6: watching only subscribes and unsubscribes (the host has no
/// input or resize at all); the session's exit ends `exited` and unsubscribes;
/// a stream dropped (the relay's `u`, or the link going) unsubscribes too.
#[tokio::test]
async fn watching_subscribes_and_unsubscribes_only() {
    let host = two_teams();
    let mut items = open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
        .await
        .unwrap();
    first(&mut items).await;
    host.emit(TermEvent::Exited);
    assert_eq!(first(&mut items).await, StreamItem::End("exited"));
    assert!(items.next().await.is_none());
    drop(items);
    assert_eq!(
        host.calls(),
        [Call::Subscribe("sess-a1".into()), Call::Unsubscribe(1)]
    );

    let items = open_terminal(ctx(&host), call(json!({"handle": "a2"}), EMEA))
        .await
        .unwrap();
    drop(items);
    assert_eq!(
        host.calls()[2..],
        [Call::Subscribe("sess-a2".into()), Call::Unsubscribe(2)]
    );
}

/// Failure modes: a team change ends an open terminal `not_found` at the next check.
#[tokio::test(start_paused = true)]
async fn a_team_change_ends_an_open_terminal() {
    let host = two_teams();
    let mut items = open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
        .await
        .unwrap();
    first(&mut items).await;
    *host.team.lock().unwrap() = Some(Team {
        org: "acme".into(),
        team: "apac".into(),
    });
    let started = Instant::now();
    assert_eq!(first(&mut items).await, StreamItem::End("not_found"));
    // Two checks in a row: one miss (an index rebuilding) is not an end.
    assert!(started.elapsed() <= TEAM_CHECK_EVERY * 2 + Duration::from_millis(10));
}

/// K30: the most recent lines win, within the line and JSON budgets, starting at
/// a line after an attribute reset; a snapshot within both is sent as it is.
#[test]
fn snapshots_are_cut_to_k30() {
    let small = "a\r\nb\r\n";
    assert_eq!(cut_snapshot(small, 2000, 1000), small);
    let lines: String = (0..3000).map(|i| format!("line {i}\r\n")).collect();
    let cut = cut_snapshot(&lines, 2000, usize::MAX);
    assert!(cut.starts_with("\x1b[0mline 1000\r\n"), "{}", &cut[..30]);
    assert!(cut.ends_with("line 2999\r\n"));
    // ESC counts as six JSON bytes.
    let colored: String = (0..100)
        .map(|i| format!("\x1b[31m{i}\x1b[0m\r\n"))
        .collect();
    let cut = cut_snapshot(&colored, 2000, 300);
    assert!(super::super::live::json_len(&cut) <= 300);
    assert!(cut.ends_with("\x1b[31m99\x1b[0m\r\n"));
    let one = "x".repeat(5000);
    let cut = cut_snapshot(&one, 2000, 1000);
    assert!(super::super::live::json_len(&cut) <= 1000 && cut.ends_with('x'));
    // The real ceiling.
    let big: String = (0..1500)
        .map(|_| format!("{}\r\n", "y".repeat(1000)))
        .collect();
    let cut = cut_snapshot(&big, SNAPSHOT_LINES, SNAPSHOT_JSON_BYTES);
    assert!(super::super::live::json_len(&cut) <= super::super::live::MAX_SNAPSHOT_BYTES);
}

/// The app's stream payloads (`stream_payload` in pty_host) read as events.
#[test]
fn stream_payloads_read_as_events() {
    assert_eq!(
        stream_event(&json!({"data": "hi", "seq": 7})),
        Some(TermEvent::Output {
            data: "hi".into(),
            seq: 7
        })
    );
    assert_eq!(
        stream_event(
            &json!({"resized": {"at_seq": 7, "resize_gen": 2, "cols": 90, "rows": 30}, "privateAuthorityEpoch": "e"})
        ),
        Some(TermEvent::Resized { cols: 90, rows: 30 })
    );
    assert_eq!(stream_event(&json!({"other": 1})), None);
}

/// Polls `items` in a task of its own, so the paused clock runs the stream
/// while the test changes the fixture.
fn poll(mut items: Items) -> tokio::sync::mpsc::UnboundedReceiver<StreamItem> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(item) = items.next().await {
            if tx.send(item).is_err() {
                break;
            }
        }
    });
    rx
}

fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<StreamItem>) -> Vec<StreamItem> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

/// Review rounds 2 and 3: failed reads. Two failed session reads send nothing
/// and the next good one sends the list; the count starts again after a good
/// read; three in a row end it `failed`. A failed layout read sends nothing
/// (never an empty list), and three end the list too; a failed worktree read
/// keeps the last branches.
#[tokio::test(start_paused = true)]
async fn failed_reads_skip_a_pass_and_three_end_the_list() {
    use std::sync::atomic::Ordering;
    let host = two_teams();
    let mut rx = poll(
        open_terminals(ctx(&host), call(json!({}), EMEA))
            .await
            .unwrap(),
    );
    tokio::time::sleep(LIST_EVERY / 2).await;
    assert_eq!(drain(&mut rx).len(), 1);
    for _ in 0..2 {
        host.fail_sessions.store(2, Ordering::SeqCst);
        host.launch(
            &format!("more{}", host.rows.lock().unwrap().len()),
            Some(("acme", "emea")),
            "ops",
            None,
        );
        tokio::time::sleep(LIST_EVERY * 3).await;
        let got = drain(&mut rx);
        assert!(matches!(got.as_slice(), [StreamItem::Value(_)]), "{got:?}");
    }
    host.fail_sessions.store(3, Ordering::SeqCst);
    tokio::time::sleep(LIST_EVERY * 4).await;
    assert_eq!(drain(&mut rx), vec![StreamItem::End("failed")]);

    let host = two_teams();
    *host.tabs.lock().unwrap() = vec![tab("acme:emea|SW-1")];
    let mut rx = poll(
        open_workspaces(ctx(&host), call(json!({}), EMEA))
            .await
            .unwrap(),
    );
    tokio::time::sleep(LIST_EVERY / 2).await;
    let got = drain(&mut rx);
    let [StreamItem::Value(v)] = got.as_slice() else {
        panic!("{got:?}")
    };
    assert_eq!(v["workspaces"][0]["branch"], "feat/SW-1-login");
    host.layout_fails.store(true, Ordering::SeqCst);
    host.worktrees_fail.store(true, Ordering::SeqCst);
    tokio::time::sleep(LIST_EVERY * 2).await;
    assert!(
        drain(&mut rx).is_empty(),
        "a failed layout read sent a value"
    );
    host.layout_fails.store(false, Ordering::SeqCst);
    tokio::time::sleep(WORKTREES_EVERY).await;
    host.tabs.lock().unwrap().push(tab("acme:emea|ops"));
    tokio::time::sleep(LIST_EVERY * 2).await;
    let got = drain(&mut rx);
    let [StreamItem::Value(v)] = got.as_slice() else {
        panic!("{got:?}")
    };
    assert_eq!(
        v["workspaces"][0]["branch"], "feat/SW-1-login",
        "the last branches are kept"
    );
    host.layout_fails.store(true, Ordering::SeqCst);
    tokio::time::sleep(LIST_EVERY * 4).await;
    assert_eq!(drain(&mut rx), vec![StreamItem::End("failed")]);
}

/// Review round 3: one failed team check (an index rebuilding) is not an end,
/// and the count starts again after a good check.
#[tokio::test(start_paused = true)]
async fn one_failed_team_check_is_not_an_end() {
    // The list re-reads at 0, 1.5, 3 s...: each time one read misses, the next is good.
    let host = two_teams();
    let mut rx = poll(
        open_terminals(ctx(&host), call(json!({}), EMEA))
            .await
            .unwrap(),
    );
    tokio::time::sleep(LIST_EVERY / 2).await;
    for _ in 0..3 {
        *host.team.lock().unwrap() = None;
        tokio::time::sleep(LIST_EVERY).await;
        *host.team.lock().unwrap() = Some(acme());
        tokio::time::sleep(LIST_EVERY * 2).await;
    }
    assert!(!drain(&mut rx)
        .iter()
        .any(|i| matches!(i, StreamItem::End(_))));
    // The terminal checks at 5, 10, 15 s...: the same.
    let mut term = poll(
        open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
            .await
            .unwrap(),
    );
    tokio::time::sleep(TEAM_CHECK_EVERY / 2).await;
    for _ in 0..3 {
        *host.team.lock().unwrap() = None;
        tokio::time::sleep(TEAM_CHECK_EVERY).await;
        *host.team.lock().unwrap() = Some(acme());
        tokio::time::sleep(TEAM_CHECK_EVERY * 2).await;
    }
    assert!(!drain(&mut term)
        .iter()
        .any(|i| matches!(i, StreamItem::End(_))));
}

/// Review round 2: a row with no tab (machine none) is neither listed nor opened.
#[tokio::test]
async fn a_session_with_no_tab_is_not_watchable() {
    let host = two_teams();
    host.rows.lock().unwrap()[0].machine = None;
    let mut items = open_terminals(ctx(&host), call(json!({}), EMEA))
        .await
        .unwrap();
    let StreamItem::Value(v) = first(&mut items).await else {
        panic!()
    };
    let handles: Vec<&str> = v["terminals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["handle"].as_str().unwrap())
        .collect();
    assert_eq!(handles, ["a2"]);
    // Bound and in the team, listed with no tab for now: `closed`, which a page retries.
    let got = open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
        .await
        .err();
    assert_eq!(got, Some(QueryError::Refused("closed")));
    host.rows.lock().unwrap()[0].machine = Some("This Mac".into());
    host.rows.lock().unwrap()[0].state = SessionState::Offline;
    let got = open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
        .await
        .err();
    assert_eq!(got, Some(QueryError::Refused("closed")));
}

/// Review round 2: an exit the listener missed ends the terminal at the next
/// check, from the session list; a row missing from one read does not.
#[tokio::test(start_paused = true)]
async fn an_exit_seen_in_the_session_list_ends_the_terminal() {
    let host = two_teams();
    let mut items = open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
        .await
        .unwrap();
    first(&mut items).await;
    let kept = host.rows.lock().unwrap().remove(0);
    let quiet = tokio::time::timeout(TEAM_CHECK_EVERY + TEAM_CHECK_EVERY / 2, items.next()).await;
    assert!(quiet.is_err(), "one missing row ended the terminal");
    let mut exited = kept;
    exited.state = SessionState::Exited;
    host.rows.lock().unwrap().insert(0, exited);
    assert_eq!(first(&mut items).await, StreamItem::End("exited"));
    drop(items);
    assert_eq!(host.calls().last(), Some(&Call::Unsubscribe(1)));
}

/// Review round 2: a folder that left every team ends an open list `no_team`.
#[tokio::test(start_paused = true)]
async fn a_folder_leaving_every_team_ends_no_team() {
    let host = two_teams();
    let mut items = open_terminals(ctx(&host), call(json!({}), EMEA))
        .await
        .unwrap();
    first(&mut items).await;
    *host.team.lock().unwrap() = None;
    assert_eq!(first(&mut items).await, StreamItem::End("no_team"));
}

/// A fake Anthropic key: `sk-ant-…`, which `redact_secrets` alone does not match.
pub(crate) const FAKE_KEY: &str = "sk-ant-api03-FAKEFAKEFAKEFAKEFAKEFAKE";

/// One step of a terminal's output in the redaction table.
enum Step {
    Out(String),
    /// More than `STREAM_FRAME_EVERY` with no output (held output is flushed).
    Quiet,
    Resize,
}

fn out(text: impl Into<String>) -> Step {
    Step::Out(text.into())
}

/// Runs `steps` on an open terminal whose snapshot is `vt`, and returns the
/// sent snapshot and every byte of output sent after it, as one text.
async fn redacted(vt: &str, steps: Vec<Step>) -> (String, String) {
    use super::super::live::STREAM_FRAME_EVERY;
    let host = two_teams();
    host.snapshot.lock().unwrap().vt = vt.into();
    let items = open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
        .await
        .unwrap();
    let mut rx = poll(items);
    for (i, step) in steps.into_iter().enumerate() {
        match step {
            Step::Out(data) => host.emit(TermEvent::Output {
                data,
                seq: 101 + i as u64,
            }),
            Step::Quiet => tokio::time::sleep(STREAM_FRAME_EVERY * 2).await,
            Step::Resize => host.emit(TermEvent::Resized { cols: 90, rows: 20 }),
        }
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (mut snapshot, mut output) = (String::new(), String::new());
    for item in drain(&mut rx) {
        match item {
            StreamItem::Snapshot { vt, .. } => snapshot = vt,
            StreamItem::Bytes(data) => output.push_str(&data),
            StreamItem::Resized { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    (snapshot, output)
}

/// Text no row may send, except where its row says it is a documented limit.
const SECRETS: &[&str] = &["FAKE", "ter2", "val1234", "secret123"];

/// SV-PBF-286 rows 1, 2, 2b–2j, 5b, 5c, 7, 7d–7l and 8, events fed one by one
/// through the open terminal: a secret in one event, split across events (at any
/// point, after a long line, by a newline between key and value, by a quiet
/// flush, by a resize, echoed a character at a time, a token recognised at its
/// prefix), a long bearer token across flushes, a JWT whose header is longer
/// than the lookback,
/// a value after whitespace (within the bound, and past it: the documented
/// limit), escapes inside a value or token, and a PEM key (over the old hold,
/// split END, JSON-escaped newline split) masked through its END line with the
/// output after it untouched. The key, the PEM header and escapes stay visible.
#[tokio::test(start_paused = true)]
async fn terminal_output_is_redacted_across_events() {
    let long = "x".repeat(300);
    let pem_line = format!("{}\r\n", "FAKE".repeat(16));
    let token = "FAKE".repeat(100);
    let big_pem = pem_line.repeat(16 * 1024 / pem_line.len() + 1);
    let json_line = format!("{}\\n", "FAKE".repeat(16));
    let jwt = format!(
        "jwt eyJ{}.eyJ{}.FAKEsig\r\n",
        "FAKE".repeat(150),
        "FAKE".repeat(5)
    );
    let rows: Vec<(&str, Vec<Step>, String)> = vec![
        (
            "1 one event",
            vec![out(format!("export ANTHROPIC_API_KEY={FAKE_KEY}\r\n"))],
            "export ANTHROPIC_API_KEY=[redacted]\r\n".into(),
        ),
        (
            "2 split in two",
            vec![
                out("key sk-ant-api03-FAKE"),
                out("FAKEFAKEFAKEFAKEFAKE\r\n"),
            ],
            "key [redacted]\r\n".into(),
        ),
        (
            "2 split in three, no newline",
            vec![
                out("key sk-a"),
                out("nt-api03-FAKEFAKE"),
                out("FAKEFAKEFAKE done"),
            ],
            "key [redacted] done".into(),
        ),
        (
            "2 split after a long line",
            vec![
                out(format!("{long}sk-ant-api03-FAKEFAKE")),
                out("FAKEFAKEFAKE\r\n"),
            ],
            format!("{long}[redacted]\r\n"),
        ),
        (
            "2 a token runs on through value characters",
            vec![out(format!("{long} {FAKE_KEY}{long}")), out(" end\r\n")],
            format!("{long} [redacted] end\r\n"),
        ),
        (
            "2 key and value split",
            vec![out("password: hun"), out("ter2secret\r\n")],
            "password: [redacted]\r\n".into(),
        ),
        (
            "2b key and value split by a newline",
            vec![out("\"password\":\n"), out("\"hunter2secret\"\r\n")],
            "\"password\":\n\"[redacted]\"\r\n".into(),
        ),
        (
            "2c a token split by a quiet flush, recognised at its prefix",
            vec![
                out("key sk-ant-api"),
                Step::Quiet,
                out("03-FAKEFAKEFAKEFAKEFAKE\r\n"),
            ],
            "key [redacted]\r\n".into(),
        ),
        (
            "2c a token split inside its prefix by a quiet flush (that start is the limit)",
            vec![
                out("key s"),
                Step::Quiet,
                out("k-ant-api03-FAKEFAKEFAKEFAKEFAKE\r\n"),
            ],
            "key s[redacted]\r\n".into(),
        ),
        (
            "2c a value echoed a character at a time",
            vec![
                out("password: "),
                Step::Quiet,
                out("h"),
                Step::Quiet,
                out("u"),
                Step::Quiet,
                out("n"),
                Step::Quiet,
                out("t"),
                Step::Quiet,
                out("er2\r\n"),
            ],
            "password: [redacted]\r\n".into(),
        ),
        (
            "2d a token split by a resize",
            vec![
                out("key sk-ant-api"),
                Step::Resize,
                out("03-FAKEFAKEFAKEFAKEFAKE\r\n"),
            ],
            "key [redacted]\r\n".into(),
        ),
        (
            "2i a JWT with a 600-character header, split after 520",
            vec![out(&jwt[..524]), out(&jwt[524..])],
            "jwt [redacted]\r\n".into(),
        ),
        (
            "2e a 400-character bearer token across a quiet flush and a resize",
            vec![
                out("Authorization: Bearer "),
                out(&token[..300]),
                Step::Quiet,
                out(&token[300..350]),
                Step::Resize,
                out(format!("{}\r\n", &token[350..])),
            ],
            "Authorization: Bearer [redacted]\r\n".into(),
        ),
        (
            "2f a value ten spaces after its key, split",
            vec![
                out("password:"),
                out("     "),
                out("     hunter2secret"),
                out("\r\n"),
            ],
            format!("password:{}[redacted]\r\n", " ".repeat(10)),
        ),
        (
            "2j a decomposed Unicode value, masked whole",
            vec![out("password: cafe\u{301}secret123\r\n")],
            "password: [redacted]\r\n".into(),
        ),
        (
            "2g a value 65 spaces after its key (the documented bound)",
            vec![out(format!("password:{}hunter2secret\r\n", " ".repeat(65)))],
            format!("password:{}hunter2secret\r\n", " ".repeat(65)),
        ),
        (
            "8 a PEM key over three events",
            vec![
                out("-----BEGIN PRIVATE KEY-----\r\n"),
                out(pem_line.clone()),
                out("-----END PRIVATE KEY-----\r\n$ "),
            ],
            "-----BEGIN PRIVATE KEY-----[redacted]\r\n$ ".into(),
        ),
        (
            "8 a PEM key over 16 KB, masked to its END line",
            vec![
                out("-----BEGIN PRIVATE KEY-----\r\n"),
                out(big_pem.clone()),
                out(pem_line.clone()),
                out("-----END PRIVATE KEY-----\r\n$ "),
            ],
            "-----BEGIN PRIVATE KEY-----[redacted]\r\n$ ".into(),
        ),
        (
            "7d an END line split across events, after 16 KB of body",
            vec![
                out("-----BEGIN PRIVATE KEY-----\r\n"),
                out(big_pem.clone()),
                out("FAKEFAKE\r\n-----END PRIVATE "),
                out("KEY-----\r\nbuild complete\r\n"),
            ],
            "-----BEGIN PRIVATE KEY-----[redacted]\r\nbuild complete\r\n".into(),
        ),
        (
            "7e a JSON-escaped PEM key, 16 KB on, with a newline split at its backslash",
            vec![
                out("{\"key\": \"-----BEGIN PRIVATE KEY-----\\n"),
                out(format!("{}FAKEFAKE\\", json_line.repeat(250))),
                out("nFAKEFAKE\\n-----END PRIVATE KEY-----\\n\"}\r\n"),
            ],
            "{\"key\": \"-----BEGIN PRIVATE KEY-----[redacted]\\n\"}\r\n".into(),
        ),
        (
            "7 an SGR inside a token",
            vec![out("sk-ant-api\x1b[1m03-FAKEFAKEFAKEFAKE\r\n")],
            "[redacted]\x1b[1m[redacted]\r\n".into(),
        ),
        (
            "7f SGRs inside a key and its value",
            vec![out("pass\x1b[1mword: fake\x1b[0mval1234\r\n")],
            "pass\x1b[1mword: [redacted]\x1b[0m[redacted]\r\n".into(),
        ),
        (
            "7g a window title with a bearer header, split",
            vec![
                out("\x1b]0;curl -H \"Authorization: Bea"),
                out("rer FAKEFAKEFAKEFAKEFAKE\" https://x\x07$ "),
            ],
            "\x1b]0;curl -H \"[redacted]\" https://x\x07$ ".into(),
        ),
        (
            "7h a hyperlink with an access token",
            vec![out(
                "\x1b]8;;https://x.example/cb?access_token=FAKEtok123&s=1\x1b\\link\x1b]8;;\x1b\\\r\n",
            )],
            "\x1b]8;;https://x.example/cb?access_token=[redacted]&s=1\x1b\\link\x1b]8;;\x1b\\\r\n"
                .into(),
        ),
        (
            "7i a title over 4 KB, masked whole",
            vec![out(format!("\x1b]0;{}\x07ok\r\n", "FAKE".repeat(1100)))],
            "\x1b]0;[redacted]\x07ok\r\n".into(),
        ),
        (
            "7j a C1 CSI inside a key",
            vec![out("pass\u{9b}1mword: hunter2secret\r\n")],
            "pass\u{9b}1mword: [redacted]\r\n".into(),
        ),
        (
            "7k a title ended by a C1 ST, then ordinary output",
            vec![out("\u{9d}0;title\u{9c}build ok\r\n")],
            "\u{9d}0;title\u{9c}build ok\r\n".into(),
        ),
        (
            "7l a C1 title with a bearer header",
            vec![out(
                "\u{9d}0;curl -H \"Authorization: Bearer FAKEFAKEFAKEFAKEFAKE\"\u{9c}$ ",
            )],
            "\u{9d}0;curl -H \"[redacted]\"\u{9c}$ ".into(),
        ),
        (
            "5b a short value after a key is masked too (over-redaction)",
            vec![out("secret: it's\r\n")],
            "secret: [redacted]'s\r\n".into(),
        ),
        (
            "5c text already redacted is shown as it is",
            vec![out("token: [redacted]\r\n")],
            "token: [redacted]\r\n".into(),
        ),
        (
            "plain output",
            vec![out("$ ls\r\n"), out("a b")],
            "$ ls\r\na b".into(),
        ),
    ];
    for (name, steps, want) in rows {
        let (_, got) = redacted("$ claude\r\n", steps).await;
        assert_eq!(got, want, "{name}");
        if !name.starts_with("2g") {
            for secret in SECRETS {
                assert!(!got.contains(secret), "{name}: {got}");
            }
        }
    }
}

/// SV-PBF-286 rows 4, 4b–4e: the snapshot runs through the same redactor
/// before its K30 cut (a PEM key whose header is outside the kept lines loses
/// its body too) and seeds the output after it, at the open and after a
/// resubscribe, through the emulator's state trailer (cursor, pen and mode
/// escapes after the text): a credential begun in the snapshot and finished in
/// the output, or a PEM key open at the snapshot, is masked on both sides.
/// Output held at a resubscribe is in the fresh snapshot and is not sent again.
#[tokio::test(start_paused = true)]
async fn terminal_snapshots_are_redacted_and_seed_the_output() {
    // Row 4c: raw, the last 2,000 lines start inside the key.
    let pem = format!(
        "-----BEGIN PRIVATE KEY-----\r\n{}-----END PRIVATE KEY-----\r\n",
        "FAKEBODYFAKEBODY\r\n".repeat(30)
    );
    let raw = format!("{pem}{}", "ok\r\n".repeat(SNAPSHOT_LINES - 10));
    let (vt, _) = redacted(&raw, vec![]).await;
    assert!(!vt.contains("FAKEBODY"), "a key line survived the cut");
    assert!(
        vt.starts_with("-----BEGIN PRIVATE KEY-----[redacted]\r\nok\r\n"),
        "{}",
        &vt[..60]
    );
    // Rows 4b and 4d at the open: a value begun in the snapshot, before the trailer.
    let (vt, output) = redacted(
        "$ login\r\npassword: hun\x1b[?25l\x1b[2;14H",
        vec![out("ter2secret\r\n")],
    )
    .await;
    assert_eq!(
        (vt.as_str(), output.as_str()),
        (
            "$ login\r\npassword: [redacted]\x1b[?25l\x1b[2;14H",
            "[redacted]\r\n"
        )
    );
    // Row 4e: a snapshot taken inside a PEM key.
    let (vt, output) = redacted(
        "$ cat key.pem\r\n-----BEGIN PRIVATE KEY-----\r\nFAKEFAKE\r\nFAKE\x1b[4;5H",
        vec![out("FAKE\r\n-----END PRIVATE KEY-----\r\n$ ")],
    )
    .await;
    assert_eq!(
        (vt.as_str(), output.as_str()),
        (
            "$ cat key.pem\r\n-----BEGIN PRIVATE KEY-----[redacted]\x1b[4;5H",
            "[redacted]\r\n$ "
        )
    );

    let host = two_teams();
    let line = format!("KEY={FAKE_KEY}\r\n");
    host.snapshot.lock().unwrap().vt = format!("$ env\r\n{line}");
    let c = call(json!({"handle": "a1"}), EMEA);
    let snap = c.snap.clone();
    let mut items = open_terminal(ctx(&host), c).await.unwrap();
    let StreamItem::Snapshot { vt, .. } = first(&mut items).await else {
        panic!("not a snapshot")
    };
    assert_eq!(vt, "$ env\r\nKEY=[redacted]\r\n");
    host.emit(TermEvent::Output {
        data: "held".into(),
        seq: 101,
    });
    // Held, not sent yet.
    assert!(
        tokio::time::timeout(Duration::from_millis(10), items.next())
            .await
            .is_err()
    );
    // Rows 4, 4b and 4d on resubscribe: a snapshot over K30 until its keys are
    // masked, ending in an unfinished value before the full state trailer.
    let trailer = "\x1b[1;1H\x1b[0m\x1b7\x1b[2 q\x1b[3;9H";
    {
        let mut s = host.snapshot.lock().unwrap();
        s.vt = format!("{}token=ab{trailer}", line.repeat(SNAPSHOT_LINES + 10));
        s.seq = 200;
    }
    snap.notify_one();
    let StreamItem::Snapshot { vt, .. } = first(&mut items).await else {
        panic!("not a snapshot")
    };
    assert!(!vt.contains("FAKE"));
    assert_eq!(vt.matches("KEY=[redacted]").count(), SNAPSHOT_LINES - 1);
    assert!(
        vt.ends_with(&format!("token=[redacted]{trailer}")),
        "{}",
        &vt[vt.len() - 60..]
    );
    host.emit(TermEvent::Output {
        data: "cdef1234\r\n".into(),
        seq: 201,
    });
    assert_eq!(
        first(&mut items).await,
        StreamItem::Bytes("[redacted]\r\n".into())
    );
}

/// SV-PBF-286 row 9, the class guard: a text with every kind of secret (titles,
/// links, an oversized title, JWTs and C1 controls among them) gives
/// the same redacted output fed whole, split at every point, or a character at
/// a time (then flushed), so no boundary between events can change what is
/// masked.
#[test]
fn every_split_of_the_output_redacts_the_same() {
    let oversized = format!("\x1b]0;{}\x07", "FAKE".repeat(1100));
    let text = [
        concat!(
            "$ export ANTHROPIC_API_KEY=sk-ant-api03-FAKEFAKEFAKEFAKE\r\n",
            "{\"password\": \"hunter2secret\", \"user\": \"ana\"}\r\n",
            "Authorization: Bearer FAKEFAKEFAKEFAKEFAKEFAKE\r\n",
            "curl https://api.example.com?access_token=FAKEtok123&page=2\r\n",
            "-----BEGIN RSA PRIVATE KEY-----\r\nFAKEFAKEFAKE\r\nFAKE\\nFAKE\r\n",
            "-----END RSA PRIVATE KEY-----\r\n",
            "ghp_FAKEFAKEFAKEFAKEFAKEFAKE done\r\n",
            "\x1b[1mtoken=\x1b[0mFAKEVAL\x1b]0;title\x07\r\n",
            "bearer FAKEFAKEFAKEFAKEFAKEFAKE end\r\n",
            "jwt=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJGQUtFIn0.FAKEsig\r\n",
            "openai sk-proj-FAKEFAKE\r\n",
            "pass\u{9b}1mword: hunter2secret\r\n",
            "\u{9d}0;curl -H \"Authorization: Bearer FAKEFAKEFAKEFAKEFAKE\"\u{9c}build ok\r\n",
            "\x1b]0;curl -H \"Authorization: Bearer FAKEFAKEFAKEFAKEFAKE\"\x07",
            "\x1b]8;;https://x.example/cb?access_token=FAKEtok123\x1b\\link\x1b]8;;\x1b\\\r\n",
        ),
        &oversized,
        "$ ",
    ]
    .concat();
    let text = text.as_str();
    let run = |parts: &[&str]| {
        let mut redactor = TerminalRedactor::default();
        let mut sent: String = parts.iter().map(|part| redactor.push(part)).collect();
        sent.push_str(&redactor.flush().unwrap_or_default());
        sent
    };
    let whole = run(&[text]);
    assert_eq!(
        whole,
        concat!(
            "$ export ANTHROPIC_API_KEY=[redacted]\r\n",
            "{\"password\": \"[redacted]\", \"user\": \"ana\"}\r\n",
            "Authorization: Bearer [redacted]\r\n",
            "curl https://api.example.com?access_token=[redacted]&page=2\r\n",
            "-----BEGIN RSA PRIVATE KEY-----[redacted]\r\n",
            "[redacted] done\r\n",
            "\x1b[1mtoken=\x1b[0m[redacted]\x1b]0;title\x07\r\n",
            "bearer [redacted] end\r\n",
            "jwt=[redacted]\r\n",
            "openai [redacted]\r\n",
            "pass\u{9b}1mword: [redacted]\r\n",
            "\u{9d}0;curl -H \"[redacted]\"\u{9c}build ok\r\n",
            "\x1b]0;curl -H \"[redacted]\"\x07",
            "\x1b]8;;https://x.example/cb?access_token=[redacted]\x1b\\link\x1b]8;;\x1b\\\r\n",
            "\x1b]0;[redacted]\x07",
            "$ ",
        )
    );
    for (at, _) in text.char_indices().skip(1) {
        assert_eq!(run(&[&text[..at], &text[at..]]), whole, "split at {at}");
    }
    let chars: Vec<String> = text.chars().map(String::from).collect();
    let chars: Vec<&str> = chars.iter().map(String::as_str).collect();
    assert_eq!(run(&chars), whole, "a character at a time");
}

/// SV-PBF-286 row 9b: a credential key followed by `Bearer <opaque token>`
/// masks the token too (the key's value ends at the space, and the bare bearer
/// pattern starts before that point), however the output is split.
#[test]
fn a_bearer_token_after_a_credential_key_is_masked() {
    let text = concat!(
        "X-Auth-Token: Bearer AbCdEfGhIjKlMnOpQrStUvWx\r\n",
        "{\"token\": \"Bearer AbCdEfGhIjKlMnOpQrStUvWx\"}\r\n",
        "$ ",
    );
    let run = |parts: &[&str]| {
        let mut redactor = TerminalRedactor::default();
        let mut sent: String = parts.iter().map(|part| redactor.push(part)).collect();
        sent.push_str(&redactor.flush().unwrap_or_default());
        sent
    };
    let whole = run(&[text]);
    assert!(!whole.contains("AbCdEf"), "{whole}");
    for (at, _) in text.char_indices().skip(1) {
        assert_eq!(run(&[&text[..at], &text[at..]]), whole, "split at {at}");
    }
    let chars: Vec<String> = text.chars().map(String::from).collect();
    let chars: Vec<&str> = chars.iter().map(String::as_str).collect();
    assert_eq!(run(&chars), whole, "a character at a time");
    let (_, batch) = TerminalRedactor::after_snapshot(text);
    assert!(!batch.contains("AbCdEf"), "{batch}");
}

/// A scheme word after a credential key is not its value: the credential after
/// it is masked too (`X-Auth-Token: Basic <base64>`), at every split, a
/// character at a time and in the snapshot, in a header and in JSON.
#[test]
fn a_credential_after_a_scheme_word_is_masked() {
    let run = |parts: &[&str]| {
        let mut redactor = TerminalRedactor::default();
        let mut sent: String = parts.iter().map(|part| redactor.push(part)).collect();
        sent.push_str(&redactor.flush().unwrap_or_default());
        sent
    };
    for scheme in ["Basic", "Bearer", "Token"] {
        let text = format!(
            "X-Auth-Token: {scheme} QWxhZGRpbjpvcGVu\r\n{{\"token\": \"{scheme} QWxhZGRpbjpvcGVu\"}}\r\n$ "
        );
        let whole = run(&[&text]);
        assert_eq!(
            whole,
            "X-Auth-Token: [redacted] [redacted]\r\n{\"token\": \"[redacted] [redacted]\"}\r\n$ ",
            "{scheme}"
        );
        for (at, _) in text.char_indices().skip(1) {
            assert_eq!(
                run(&[&text[..at], &text[at..]]),
                whole,
                "{scheme} split at {at}"
            );
        }
        let chars: Vec<String> = text.chars().map(String::from).collect();
        let chars: Vec<&str> = chars.iter().map(String::as_str).collect();
        assert_eq!(run(&chars), whole, "{scheme} a character at a time");
        let (_, batch) = TerminalRedactor::after_snapshot(&text);
        assert_eq!(batch, whole, "{scheme} snapshot");
    }
}

/// SV-PBF-286 row 10, the cost guard: a credential-dense snapshot (2,000 JSON
/// password fields in one batch) is redacted with each pattern searching the
/// batch once, however many secrets it holds.
#[test]
fn a_credential_dense_snapshot_searches_each_pattern_once() {
    let vt = "\"password\":\"FAKEFAKE\",".repeat(2000);
    PATTERN_SEARCHES.with(|n| n.set(0));
    let (_, sent) = TerminalRedactor::after_snapshot(&vt);
    assert_eq!(sent, "\"password\":\"[redacted]\",".repeat(2000));
    let patterns = &*secret_scan::STREAM_PATTERNS;
    // keys, the PEM header, tokens, catalog's extra tokens and bare bearer.
    let per_batch = patterns.keys.len() + 1 + patterns.tokens.len() + 1 + 1;
    assert_eq!(PATTERN_SEARCHES.with(|n| n.get()), per_batch);
}

/// The redactor's character classes are the detectors' own: `value_head`,
/// `value_tail` and `pem_body` match what `VALUE_HEAD`, `value_tail_class()`
/// and `pem_char_class()` (plus an escaped newline's backslash) do, on every
/// ASCII character and on combining marks, connector punctuation, other
/// scripts, digits and spaces (row 2j's class).
#[test]
fn the_stream_classes_match_the_detectors() {
    let class = |pattern: &str| regex::Regex::new(&format!("^{pattern}$")).unwrap();
    let (head, tail, body) = (
        class("[A-Za-z0-9]"),
        class(&secret_scan::value_tail_class()),
        class(&secret_scan::pem_char_class()),
    );
    let others = [
        'é', 'ß', '中', 'ب', 'क', 'ж', 'ａ', '٣', '５', '\u{301}', '\u{93f}',
        '\u{20dd}', // combining marks
        '_', '\u{203f}', '\u{2040}', '\u{fe4f}', // connector punctuation
        '\u{a0}', '\u{2003}', '\u{3000}', '\u{200b}', // spaces
        '\u{9b}', '\u{9c}', '€', '·', '’', '😀',
    ];
    for c in (0u8..128).map(char::from).chain(others) {
        let s = c.to_string();
        assert_eq!(secret_scan::value_head(c), head.is_match(&s), "{c:?}");
        assert_eq!(secret_scan::value_tail(c), tail.is_match(&s), "{c:?}");
        assert_eq!(
            secret_scan::pem_body(c),
            body.is_match(&s) || c == '\\',
            "{c:?}"
        );
    }
}

/// SV-PBF-286 row 5: a held run (it may be the start of a secret) goes after
/// `STREAM_FRAME_EVERY` of quiet and before a resize; a recognised secret is
/// sent at once, masked, before the end.
#[tokio::test(start_paused = true)]
async fn held_output_is_flushed_on_quiet_resize_and_end() {
    use super::super::live::STREAM_FRAME_EVERY;
    let host = two_teams();
    let mut items = open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
        .await
        .unwrap();
    first(&mut items).await;
    let at = Instant::now();
    host.emit(TermEvent::Output {
        data: "partial".into(),
        seq: 101,
    });
    assert_eq!(first(&mut items).await, StreamItem::Bytes("partial".into()));
    assert_eq!(at.elapsed(), STREAM_FRAME_EVERY);
    host.emit(TermEvent::Output {
        data: "before-resize".into(),
        seq: 102,
    });
    host.emit(TermEvent::Resized { cols: 90, rows: 20 });
    assert_eq!(
        first(&mut items).await,
        StreamItem::Bytes("before-resize".into())
    );
    assert_eq!(
        first(&mut items).await,
        StreamItem::Resized { cols: 90, rows: 20 }
    );
    host.emit(TermEvent::Output {
        data: format!("key {FAKE_KEY}"),
        seq: 103,
    });
    host.emit(TermEvent::Exited);
    assert_eq!(
        first(&mut items).await,
        StreamItem::Bytes("key [redacted]".into())
    );
    assert_eq!(first(&mut items).await, StreamItem::End("exited"));
    assert!(items.next().await.is_none());
}

/// A cut never starts inside an escape sequence: CSI, OSC (BEL or ST), DCS and
/// an ESC with an intermediate, wherever the cut lands in it.
#[test]
fn a_cut_never_starts_inside_an_escape_sequence() {
    for seq in [
        "\x1b[38;5;209m",
        "\x1b]8;;https://example.com/a\x07",
        "\x1b]0;title\x1b\\",
        "\x1bPq#0;2;0;0;0\x1b\\",
        "\x1b(B",
    ] {
        let text = format!("plain{seq}after");
        let start = 5;
        assert_eq!(
            outside_escape(&text, start),
            start,
            "{seq:?} before its ESC"
        );
        for at in start + 1..=start + seq.len() {
            let rest = &text[outside_escape(&text, at)..];
            assert_eq!(rest, "after", "{seq:?} cut at {at}");
        }
    }
    // Outside any sequence the cut stays where it is.
    assert_eq!(outside_escape("ab\x1b[0mcd", 2), 2);
    assert_eq!(outside_escape("ab\x1b[0mcd", 7), 7);
    // A CSI cut short by a new ESC, and a string never closed (the next line starts the cut).
    assert_eq!(
        &"ab\x1b[3\x1b[31mX"[outside_escape("ab\x1b[3\x1b[31mX", 4)..],
        "\x1b[31mX"
    );
    let open = "a\x1b]0;never closed\nnext line";
    assert_eq!(&open[outside_escape(open, 6)..], "next line");
    // Never closed and no line after: nothing of it is kept.
    assert_eq!(outside_escape("a\x1b]0;never", 5), "a\x1b]0;never".len());
    // A cut after a CSI was cut short by ESC, and CSIs cancelled by CAN or SUB.
    assert_eq!(
        &"ab\x1b[3\x1b[31mX"[outside_escape("ab\x1b[3\x1b[31mX", 7)..],
        "X"
    );
    assert_eq!(outside_escape("ab\x1b[3\x18cd", 6), 6);
    assert_eq!(outside_escape("ab\x1b[3\x1acd", 6), 6);
    // A string ended by an ESC that starts a new sequence keeps that sequence whole.
    let text = "a\x1b]0;t\x1b[31mX";
    assert_eq!(&text[outside_escape(text, 5)..], "\x1b[31mX");
    // A cut just after that ESC never moves back before where it was asked for.
    assert!(outside_escape(text, 7) >= 7);
    assert_eq!(&text[outside_escape(text, 7)..], "X");
    // ESC ESC [ ... and a string cancelled by CAN.
    assert_eq!(
        &"a\x1b\x1b[31mX"[outside_escape("a\x1b\x1b[31mX", 3)..],
        "X"
    );
    assert_eq!(
        &"a\x1b]0;t\x18X"[outside_escape("a\x1b]0;t\x18X", 4)..],
        "X"
    );
    // A line that is over budget alone is cut with its escapes whole.
    let line: String = (0..400).map(|_| "\x1b[31mX\x1b[0m").collect();
    for budget in [100, 101, 102, 103, 104, 105, 106, 107] {
        let cut = cut_snapshot(&line, 2000, budget);
        let body = cut.strip_prefix(RESET).unwrap();
        assert!(
            body.starts_with('\x1b') || body.starts_with('X'),
            "{budget}: {:?}",
            &body[..10.min(body.len())]
        );
    }
}

/// Review round 1 and 2: a subscribe whose caller has gone is undone, whether
/// it finished before (left in the channel) or after the caller dropped.
#[tokio::test]
async fn a_subscribe_with_no_caller_is_undone() {
    let undone = Arc::new(Mutex::new(Vec::new()));
    let watch = |stream| TerminalWatch {
        snapshot: TerminalSnap {
            cols: 1,
            rows: 1,
            seq: 0,
            vt: String::new(),
        },
        handle: WatchHandle {
            stream,
            listeners: Vec::new(),
        },
        events: tokio::sync::mpsc::unbounded_channel().1,
    };
    let record = |u: &Arc<Mutex<Vec<u64>>>| {
        let u = u.clone();
        move |h: WatchHandle| u.lock().unwrap().push(h.stream)
    };
    // Finished after the caller dropped.
    let (done, got) = tokio::sync::oneshot::channel();
    drop(Pending {
        got: Some(got),
        undo: Some(Box::new(record(&undone))),
    });
    hand_over(done, Ok(watch(1)), record(&undone));
    // Finished before: handed over, then the caller dropped without reading it.
    let (done, got) = tokio::sync::oneshot::channel();
    hand_over(done, Ok(watch(2)), record(&undone));
    drop(Pending {
        got: Some(got),
        undo: Some(Box::new(record(&undone))),
    });
    // Read by its caller: nothing undone.
    let (done, got) = tokio::sync::oneshot::channel();
    hand_over(done, Ok(watch(3)), record(&undone));
    let mut pending = Pending {
        got: Some(got),
        undo: Some(Box::new(record(&undone))),
    };
    let read = pending.got.as_mut().unwrap().await.unwrap().unwrap();
    pending.got = None;
    drop(pending);
    assert_eq!(read.handle.stream, 3);
    assert_eq!(*undone.lock().unwrap(), vec![1, 2]);
}

/// Review rounds 3 to 5: a session moved to another machine while watched ends
/// the terminal `not_found` at the next check; a row listed with no tab (a
/// layout read that failed) does not.
#[tokio::test(start_paused = true)]
async fn a_moved_session_ends_the_terminal() {
    let host = two_teams();
    let mut items = open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
        .await
        .unwrap();
    first(&mut items).await;
    host.rows.lock().unwrap()[0].machine = None;
    let quiet = tokio::time::timeout(TEAM_CHECK_EVERY * 3, items.next()).await;
    assert!(quiet.is_err(), "a row with no tab ended the terminal");
    host.rows.lock().unwrap()[0].machine_id = Some("m-2".into());
    let ended =
        tokio::time::timeout(TEAM_CHECK_EVERY + Duration::from_millis(10), items.next()).await;
    assert_eq!(ended.ok().flatten(), Some(StreamItem::End("not_found")));
    drop(items);
    assert_eq!(host.calls().last(), Some(&Call::Unsubscribe(1)));
}

/// Review rounds 4 and 5: Offline for one read (a host restarting) is not an
/// end; for two reads in a row it ends `closed` (a page may retry).
#[tokio::test(start_paused = true)]
async fn an_offline_row_needs_two_reads() {
    let host = two_teams();
    let mut items = open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
        .await
        .unwrap();
    first(&mut items).await;
    host.rows.lock().unwrap()[0].state = SessionState::Offline;
    let quiet = tokio::time::timeout(TEAM_CHECK_EVERY + TEAM_CHECK_EVERY / 2, items.next()).await;
    assert!(quiet.is_err(), "one Offline read ended the terminal");
    host.rows.lock().unwrap()[0].state = SessionState::Running;
    let quiet = tokio::time::timeout(TEAM_CHECK_EVERY * 2, items.next()).await;
    assert!(quiet.is_err(), "the miss count did not start again");
    host.rows.lock().unwrap()[0].state = SessionState::Offline;
    let ended = tokio::time::timeout(TEAM_CHECK_EVERY * 3, items.next()).await;
    assert_eq!(ended.ok().flatten(), Some(StreamItem::End("closed")));
}

/// Confirmation round: `closed` never leaks past the team rule (AC-4). A session
/// of another team, with no team, unbound, remote or exited gets `not_found` even
/// when it is Offline or listed with no tab.
#[tokio::test]
async fn closed_is_only_for_the_teams_own_bound_sessions() {
    for (machine, offline) in [(None, false), (Some("This Mac"), true)] {
        let host = two_teams();
        for r in host.rows.lock().unwrap().iter_mut() {
            r.machine = machine.map(String::from);
            if offline && r.state != SessionState::Exited {
                r.state = SessionState::Offline;
            }
        }
        for handle in ["b1", "n1", "u1", "r1", "x1"] {
            let got = open_terminal(ctx(&host), call(json!({"handle": handle}), EMEA))
                .await
                .err();
            assert_eq!(
                got,
                Some(QueryError::Refused("not_found")),
                "{handle} {machine:?} {offline}"
            );
        }
        let own = open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
            .await
            .err();
        assert_eq!(own, Some(QueryError::Refused("closed")));
    }
}

/// An index that is not ready serves no team for any folder. A watched list waits that out and
/// goes on sending once it is ready; the miss count does not run while it waits.
#[tokio::test(start_paused = true)]
async fn a_list_waits_out_an_index_that_is_not_ready() {
    use std::sync::atomic::Ordering;
    let host = two_teams();
    let mut rx = poll(
        open_terminals(ctx(&host), call(json!({}), EMEA))
            .await
            .unwrap(),
    );
    tokio::time::sleep(LIST_EVERY / 2).await;
    assert!(!drain(&mut rx).is_empty(), "the first list");
    *host.team.lock().unwrap() = None;
    host.not_ready.store(true, Ordering::SeqCst);
    tokio::time::sleep(INDEXING_PATIENCE / 2).await;
    assert!(!drain(&mut rx).iter().any(|i| matches!(i, StreamItem::End(_))));
    // Ready again, and the team is back: the next change is sent, as before.
    *host.team.lock().unwrap() = Some(acme());
    host.not_ready.store(false, Ordering::SeqCst);
    host.launch("a3", Some(("acme", "emea")), "ops-2", None);
    tokio::time::sleep(LIST_EVERY * 3).await;
    let seen = drain(&mut rx);
    assert!(seen.iter().any(|i| matches!(i, StreamItem::Value(_))), "{seen:?}");
    assert!(!seen.iter().any(|i| matches!(i, StreamItem::End(_))));
    // One miss after a ready check is still only a miss.
    *host.team.lock().unwrap() = None;
    tokio::time::sleep(LIST_EVERY).await;
    *host.team.lock().unwrap() = Some(acme());
    tokio::time::sleep(LIST_EVERY * 3).await;
    assert!(!drain(&mut rx).iter().any(|i| matches!(i, StreamItem::End(_))));
}

/// The wait is bounded: an index that stays not ready past the patience ends the list `failed`
/// (a passing end: the page opens it again), never `no_team`.
#[tokio::test(start_paused = true)]
async fn an_index_that_never_gets_ready_ends_the_list_failed() {
    let host = two_teams();
    let mut items = open_terminals(ctx(&host), call(json!({}), EMEA))
        .await
        .unwrap();
    first(&mut items).await;
    *host.team.lock().unwrap() = None;
    host.not_ready.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(first(&mut items).await, StreamItem::End("failed"));
}

#[tokio::test(start_paused = true)]
async fn a_terminal_waits_out_an_index_that_is_not_ready() {
    use std::sync::atomic::Ordering;
    let host = two_teams();
    let mut term = poll(
        open_terminal(ctx(&host), call(json!({"handle": "a1"}), EMEA))
            .await
            .unwrap(),
    );
    tokio::time::sleep(TEAM_CHECK_EVERY / 2).await;
    *host.team.lock().unwrap() = None;
    host.not_ready.store(true, Ordering::SeqCst);
    tokio::time::sleep(INDEXING_PATIENCE / 2).await;
    assert!(!drain(&mut term).iter().any(|i| matches!(i, StreamItem::End(_))));
    *host.team.lock().unwrap() = Some(acme());
    host.not_ready.store(false, Ordering::SeqCst);
    tokio::time::sleep(TEAM_CHECK_EVERY * 4).await;
    assert!(!drain(&mut term).iter().any(|i| matches!(i, StreamItem::End(_))));
    // And a terminal past the patience ends `failed`.
    *host.team.lock().unwrap() = None;
    host.not_ready.store(true, Ordering::SeqCst);
    tokio::time::sleep(INDEXING_PATIENCE + TEAM_CHECK_EVERY * 2).await;
    assert!(drain(&mut term).contains(&StreamItem::End("failed")));
}

/// Opening while the index is not ready is a failed open (the page tries again), not `no_team`;
/// with the index ready, a folder in no team is still `no_team`.
#[tokio::test]
async fn opening_while_the_index_is_not_ready_fails_and_does_not_refuse() {
    let host = two_teams();
    *host.team.lock().unwrap() = None;
    host.not_ready.store(true, std::sync::atomic::Ordering::SeqCst);
    let err = open_terminals(ctx(&host), call(json!({}), EMEA)).await.err();
    assert_eq!(err, Some(QueryError::Failed(INDEXING.into())));
    host.not_ready.store(false, std::sync::atomic::Ordering::SeqCst);
    let err = open_terminals(ctx(&host), call(json!({}), EMEA)).await.err();
    assert_eq!(err, Some(QueryError::Refused("no_team")));
}

/// A lookup that read the empty snapshot is judged by that snapshot: the index becoming ready
/// right after does not turn it into `no_team`, for a query or an open.
#[tokio::test]
async fn a_lookup_is_classified_by_the_snapshot_it_read() {
    use std::sync::atomic::Ordering;
    let host = two_teams();
    *host.team.lock().unwrap() = None;
    host.not_ready.store(true, Ordering::SeqCst);
    host.finish_after_lookup.store(true, Ordering::SeqCst);
    let agents: Arc<dyn AgentHost> = host.clone();
    let scope = super::super::state::Scope { org: "acme".into(), team: "emea".into() };
    let err = scoped_team(&agents, Path::new("/tmp/dash"), Some(&scope)).await.err();
    assert_eq!(err, Some(QueryError::Failed(INDEXING.into())));
    // The next lookup finds the team.
    assert_eq!(scoped_team(&agents, Path::new("/tmp/dash"), Some(&scope)).await.ok(), Some(acme()));
    // And an open that raced the same way fails, it is not refused.
    *host.team.lock().unwrap() = None;
    host.not_ready.store(true, Ordering::SeqCst);
    host.finish_after_lookup.store(true, Ordering::SeqCst);
    let err = open_terminals(ctx(&host), call(json!({}), EMEA)).await.err();
    assert_eq!(err, Some(QueryError::Failed(INDEXING.into())));
}
