// SPDX-License-Identifier: MIT
//! Bind local MCP to its hosted process or a relay-spawned bridge process.
use crate::commands::pty_backend::Liveness;
use codesense_pty_core::activity::peer::{self, PeerProcess};
use rmcp::ErrorData;
use serde_json::{Map, Value};

pub(crate) const UNBOUND: &str = "CodeSense MCP has no verified tab identity. Start a supported agent CLI inside a CodeSense terminal tab, or launch the agent through CodeSense. Passing sessionId alone cannot establish identity; MCP cannot act as the human.";

/// The terminal server can't say yet which process the tab runs: temporary, so the
/// agent retries instead of restarting a healthy session.
pub(crate) const UNVERIFIED_YET: &str = "CodeSense can't confirm this tab's identity right now (its terminal server isn't answering). Retry shortly; you don't need to restart.";

pub(crate) fn refused() -> ErrorData {
    ErrorData::invalid_request(UNBOUND, None)
}

/// The tab a local connection proved at initialize, with the process its proof
/// found the tab running. Held by that connection alone; never persisted or shared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Binding {
    pub session: String,
    /// `None` when a relay proved the caller rather than the tab's process tree.
    pub root: Option<PeerProcess>,
}

/// What the terminal server answers about the process a session runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Lookup {
    Root(PeerProcess),
    /// No root found; whether the session is live, if that can be asked.
    NoRoot(Liveness<bool>),
}

/// Bind a connection at initialize: `session` is what its client named.
pub(crate) fn capture(
    session: Option<String>,
    peer: Option<PeerProcess>,
    lookup: Lookup,
) -> Result<Option<Binding>, ErrorData> {
    let Some(session) = session else {
        return Ok(None);
    };
    let root = verify(&session, peer, lookup, None)?;
    Ok(Some(Binding { session, root }))
}

/// Check a bound connection's caller again on a later call. While the terminal
/// server can't name the tab's process, the root proven at initialize stands in
/// for it as long as it still runs; the caller's ancestry is always walked afresh.
pub(crate) fn revalidate(
    binding: &Binding,
    peer: Option<PeerProcess>,
    lookup: Lookup,
) -> Result<(), ErrorData> {
    verify(&binding.session, peer, lookup, binding.root).map(drop)
}

/// The root to check a caller against: the fresh one, else, unless the session is
/// known gone, the root this connection proved while it still runs.
pub(crate) fn usable_root(lookup: Lookup, cached: Option<PeerProcess>) -> Option<PeerProcess> {
    match lookup {
        Lookup::Root(root) => Some(root),
        Lookup::NoRoot(Liveness::Known(false)) => None,
        Lookup::NoRoot(_) => cached.filter(|root| peer::runs(*root)),
    }
}

/// The root `peer` was proven under (`None` for a relay proof).
fn verify(
    session: &str,
    peer: Option<PeerProcess>,
    lookup: Lookup,
    cached: Option<PeerProcess>,
) -> Result<Option<PeerProcess>, ErrorData> {
    let Some(peer) = peer else {
        return Err(refused());
    };
    let unknown = lookup == Lookup::NoRoot(Liveness::Unknown);
    let proven = usable_root(lookup, cached).filter(|root| {
        crate::delivery::authority::bind_caller(
            &crate::delivery::authority::TransportIdentity::LocalPeer(peer),
            session,
            |_| Some(*root),
        )
        .is_ok()
    });
    if proven.is_some() || crate::commands::mcp_scope::relay_peer_proven(session, peer) {
        Ok(proven)
    } else if unknown && cached.is_none() {
        Err(ErrorData::internal_error(UNVERIFIED_YET, None))
    } else {
        Err(refused())
    }
}

pub(crate) fn note_initialize(
    state: &std::sync::RwLock<Option<Binding>>,
    binding: Option<Binding>,
) {
    *state.write().unwrap_or_else(|e| e.into_inner()) = binding;
}

pub(crate) fn stored(state: &std::sync::RwLock<Option<Binding>>) -> Option<Binding> {
    state.read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub(crate) fn stored_session(state: &std::sync::RwLock<Option<Binding>>) -> Option<String> {
    stored(state).map(|binding| binding.session)
}

pub(crate) fn arguments(
    name: &str,
    args: &mut Map<String, Value>,
    session: Option<&str>,
    handle: Option<&str>,
) -> Result<(), ErrorData> {
    let Some(session) = session else {
        return if super::remote_authority::is_identity_tool(name) {
            Err(refused())
        } else {
            Ok(())
        };
    };
    super::remote_authority::bind_identity(name, args, Some((handle.unwrap_or(session), session)))
        .map_err(|message| ErrorData::invalid_params(message, None))
}

pub(crate) fn session<'a>(
    explicit: Option<&str>,
    connection: Option<&'a str>,
) -> Result<&'a str, ErrorData> {
    let session = connection
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(refused)?;
    if explicit
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some_and(|s| s != session)
    {
        return Err(ErrorData::invalid_params(
            "sessionId must name this connection's own tab; MCP cannot act as the human",
            None,
        ));
    }
    Ok(session)
}

/// The session has already passed the transport resolver. Only trusted launch
/// provenance may grant a stable routine/role owner; arguments never enter here.
pub(crate) fn queue_caller(
    session: &str,
    launch: Option<&crate::commands::dashboard_launch::DashboardLaunchState>,
    remote_owner: Option<String>,
) -> crate::weft::queue::QueueCaller {
    crate::weft::queue::QueueCaller {
        owner: remote_owner
            .or_else(|| launch.and_then(|s| s.queue_owner_for_session(session)))
            .unwrap_or_else(|| session.to_owned()),
        session: session.to_owned(),
    }
}

/// A bound caller always uses its launch team. Ambient UI state is only for
/// tools that do not speak as a session and are called without a session.
pub(crate) fn weft_store_path(
    context: &crate::commands::route_to_context::ActiveContext,
    session: Option<&str>,
    launch: Option<&crate::commands::dashboard_launch::DashboardLaunchState>,
    remote_team: Option<crate::commands::dashboard_launch::LaunchTeam>,
) -> Result<std::path::PathBuf, String> {
    weft_store(context, session, launch, remote_team).map(|(path, _)| path)
}

/// [`weft_store_path`] with the team whose store it is.
pub(crate) fn weft_store(
    context: &crate::commands::route_to_context::ActiveContext,
    session: Option<&str>,
    launch: Option<&crate::commands::dashboard_launch::DashboardLaunchState>,
    remote_team: Option<crate::commands::dashboard_launch::LaunchTeam>,
) -> Result<
    (
        std::path::PathBuf,
        crate::commands::dashboard_launch::LaunchTeam,
    ),
    String,
> {
    let team = if let Some(session) = session {
        remote_team
            .or_else(|| launch.and_then(|state| state.launch_team_for_session(session)))
            .ok_or_else(|| "verified caller has no captured launch team".to_owned())?
    } else {
        crate::commands::dashboard_launch::LaunchTeam {
            org: context.org_slug.clone(),
            team: context.team_name.clone().ok_or("no active team selected")?,
        }
    };
    let path = crate::weft::resolve_weft_store_path(
        &context.codesense_home,
        &context.principal(),
        &team.org,
        &team.team,
    )?;
    Ok((path, team))
}

/// Run ids are correlation, never an alternative caller identity. The roster is
/// populated by trusted launch/dispatch, not by MCP arguments. Validate before any
/// assertion or queue write so a refusal has no side effects.
pub(crate) fn weft_run(
    store: &crate::weft::WeftStore,
    session: &str,
    launch_handle: Option<&str>,
    claimed: Option<&str>,
) -> Result<Option<String>, String> {
    let trusted = store
        .run_for_worker(launch_handle.unwrap_or(session))
        .map_err(|e| format!("STORE:{e}"))?;
    owned_run(claimed, trusted.as_deref())
}

pub(crate) fn owned_run(
    claimed: Option<&str>,
    trusted: Option<&str>,
) -> Result<Option<String>, String> {
    if claimed
        .map(str::trim)
        .filter(|run| !run.is_empty())
        .is_some_and(|run| Some(run) != trusted)
    {
        return Err("INVALID:runId must belong to this connection's verified session".into());
    }
    Ok(trusted.map(str::to_owned))
}

pub(crate) fn inbox_caller<'a>(
    explicit: Option<&str>,
    connection: Option<&'a str>,
) -> Result<crate::commands::agent_inbox::Caller<'a>, ErrorData> {
    Ok(crate::commands::agent_inbox::Caller::Named(Some(session(
        explicit, connection,
    )?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_role_ownership_requires_the_trusted_startup_queue_role() {
        use crate::commands::dashboard_launch::DashboardLaunchState;
        let launch = DashboardLaunchState::new(4);
        launch.register("controller", "session-1", Some("codesense"));
        assert_eq!(
            queue_caller("session-1", Some(&launch), None).owner,
            "session-1"
        );
        launch.set_queue_role("controller", Some("controller".into()));
        assert_eq!(launch.queue_owner_for_session("session-1"), None);
        launch.set_launch_team_if_absent(
            "controller",
            crate::commands::dashboard_launch::LaunchTeam {
                org: "org/repo".into(),
                team: "team".into(),
            },
        );
        assert_eq!(
            queue_caller("session-1", Some(&launch), None).owner,
            "role:org/repo/team/controller"
        );
        launch.set_role_join_error("controller", "not admitted".into());
        assert_eq!(
            queue_caller("session-1", Some(&launch), None).owner,
            "session-1"
        );
        assert_eq!(
            queue_caller(
                "remote-session",
                None,
                Some("routine:org/team/controller".into())
            )
            .owner,
            "routine:org/team/controller"
        );
    }

    #[test]
    fn same_role_in_two_teams_uses_the_launch_store_while_ui_views_b() {
        use crate::commands::dashboard_launch::{DashboardLaunchState, LaunchTeam};
        use crate::commands::route_to_context::ActiveContext;
        use crate::weft::queue::{self, QueueError};
        let dir = tempfile::tempdir().unwrap();
        let launch = DashboardLaunchState::new(4);
        let ui = ActiveContext::for_team(dir.path().into(), 42, "org/repo".into(), "b".into());
        let mut callers = Vec::new();
        let mut paths = Vec::new();
        for team in ["a", "b"] {
            let session = format!("manager-{team}");
            launch.register(&session, &session, Some("codesense"));
            launch.set_queue_role(&session, Some("manager".into()));
            launch.set_launch_team_if_absent(
                &session,
                LaunchTeam {
                    org: "org/repo".into(),
                    team: team.into(),
                },
            );
            let caller = queue_caller(&session, Some(&launch), None);
            assert_eq!(caller.owner, format!("role:org/repo/{team}/manager"));
            let (path, store_team) = weft_store(&ui, Some(&session), Some(&launch), None).unwrap();
            // The store's team is the caller's captured one, whose org a spawn resolves in.
            assert_eq!(
                (store_team.org.as_str(), store_team.team.as_str()),
                ("org/repo", team)
            );
            let expected =
                crate::weft::weft_store_path(dir.path(), &ui.principal(), "org/repo", team)
                    .unwrap();
            assert_eq!(path, expected);
            let store = crate::weft::WeftStore::open(&path).unwrap();
            let id = queue::enqueue(&store, "role", None, None, None, None, None, 100).unwrap();
            assert_eq!(id, 1);
            let input = crate::weft::TripwireInput {
                scope: None,
                touched_paths: Vec::new(),
                added_lines: 1000,
                lifecycle: crate::weft::Lifecycle::Persistent,
                requires_diff_read: true,
                verify_fix_cycles: 0,
            };
            queue::claim_with_tripwire(&store, &input, caller.clone(), None, 110)
                .unwrap()
                .unwrap();
            callers.push(caller);
            paths.push(path);
        }
        let store_b = crate::weft::WeftStore::open(&paths[1]).unwrap();
        assert_eq!(
            queue::resolve(&store_b, 1, callers[0].clone(), 120).unwrap(),
            Err(QueueError::OwnerMismatch)
        );
        for (path, caller) in paths.iter().zip(callers) {
            let store = crate::weft::WeftStore::open(path).unwrap();
            queue::resolve(&store, 1, caller, 130).unwrap().unwrap();
        }
        launch.register("no-team", "no-team", Some("codesense"));
        launch.set_queue_role("no-team", Some("manager".into()));
        assert!(weft_store_path(&ui, Some("no-team"), Some(&launch), None).is_err());
        // Remote authorities have captured teams even without a local launch entry.
        let remote = weft_store_path(
            &ui,
            Some("remote"),
            None,
            Some(LaunchTeam {
                org: "org/repo".into(),
                team: "a".into(),
            }),
        )
        .unwrap();
        assert_eq!(remote, paths[0]);
        assert!(weft_store_path(&ui, Some("remote"), None, None).is_err());
    }

    #[test]
    fn weft_run_claims_follow_the_trusted_session_roster() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::weft::WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
        let launch = crate::commands::dashboard_launch::DashboardLaunchState::new(4);
        // Seed through the sanctioned writer, as the trusted launch/dispatch hook does.
        for (name, session, run) in [
            ("story-a", "session-a", "run-a"),
            ("story-b", "session-b", "run-b"),
        ] {
            let handle = format!("worker-{session}");
            launch.register(&handle, session, Some("codesense"));
            crate::weft::wv_upsert_roster(
                &store,
                &crate::weft::WvRosterRow {
                    name: name.into(),
                    session_id: Some(handle),
                    run_id: Some(run.into()),
                    status: Some("dispatch".into()),
                    workspace: None,
                    pane: None,
                    kind: None,
                    subclass: None,
                    lifecycle: "ephemeral".into(),
                    owned: None,
                    orch_owner: None,
                    relation: "peer".into(),
                    parent: None,
                    current_task: None,
                    cwd: None,
                },
            )
            .unwrap();
        }
        let handle = launch.handle_for_session("session-a").unwrap();
        assert_ne!(handle, "session-a");
        for claimed in [None, Some("run-a")] {
            assert_eq!(
                weft_run(&store, "session-a", Some(&handle), claimed)
                    .unwrap()
                    .as_deref(),
                Some("run-a")
            );
        }
        assert!(weft_run(&store, "session-a", Some(&handle), Some("run-b")).is_err());
        assert!(weft_run(&store, "session-a", Some(&handle), Some("controller")).is_err());
        assert!(weft_run(&store, "unbound", None, Some("run-a")).is_err());
        assert_eq!(weft_run(&store, "unbound", None, None).unwrap(), None);
    }

    #[test]
    fn review_r1_a_readable_descriptor_is_not_a_callers_identity_proof() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use codesense_host_proto::scoped_mcp::{self, Descriptor};
        use codesense_pty_core::activity::peer;
        let parent = crate::commands::mcp_scope::mcp_scope_session_dir("unused")
            .parent()
            .unwrap()
            .to_path_buf();
        std::fs::create_dir_all(&parent).unwrap();
        let scope = tempfile::Builder::new()
            .prefix("untrusted-descriptor-")
            .tempdir_in(parent)
            .unwrap();
        let session = scope
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let descriptor = Descriptor {
            version: scoped_mcp::VERSION,
            socket: "/tmp/fabricated-relay".into(),
            capability: "c".repeat(64),
        };
        scoped_mcp::write_descriptor(&scope.path().join("local-relay.json"), &descriptor).unwrap();
        let (socket, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        let direct_peer = peer::of_socket(&socket).unwrap();
        assert!(
            capture(
                Some(session),
                Some(direct_peer),
                Lookup::NoRoot(Liveness::Known(true))
            )
            .is_err(),
            "a readable or fabricated descriptor must not authenticate a direct socket peer"
        );
    }

    #[test]
    fn hand_started_codex_configuration_forwards_the_tabs_identity_environment() {
        let dir = tempfile::tempdir().unwrap();
        let environment = crate::commands::agent_autoconnect::write_agent_files(
            dir.path(),
            std::path::Path::new("/fixture/bridge"),
        )
        .unwrap();
        let codex = environment
            .iter()
            .find(|(name, _)| name == crate::commands::agent_autoconnect::CODEX_MCP_ENV)
            .unwrap();
        let config: toml::Value = toml::from_str(&codex.1).unwrap();
        assert_eq!(
            config
                .get("mcp_servers")
                .and_then(|v| v.get("codesense"))
                .and_then(|v| v.get("env_vars"))
                .and_then(|v| v.get(0))
                .and_then(toml::Value::as_str),
            Some(crate::commands::mcp_scope::TAB_SESSION_ENV)
        );
    }

    #[test]
    fn a_hand_started_process_binds_only_inside_its_own_tabs_process_tree() {
        use codesense_pty_core::activity::peer;
        let (socket, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        let peer = peer::of_socket(&socket).unwrap();
        let root = peer::inspect(std::os::unix::process::parent_id()).unwrap();
        assert_eq!(
            capture(
                Some("hand-started-tab".into()),
                Some(peer),
                Lookup::Root(root)
            )
            .unwrap(),
            Some(Binding {
                session: "hand-started-tab".into(),
                root: Some(root)
            })
        );
        let mut sibling = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let unrelated = peer::inspect(sibling.id()).unwrap();
        let other_tab = capture(
            Some("another-tab".into()),
            Some(peer),
            Lookup::Root(unrelated),
        );
        sibling.kill().unwrap();
        sibling.wait().unwrap();
        assert!(other_tab.is_err());
        assert!(capture(
            Some("departed-tab".into()),
            Some(peer),
            Lookup::NoRoot(Liveness::Known(false))
        )
        .is_err());
        assert_eq!(
            capture(None, Some(peer), Lookup::NoRoot(Liveness::Unknown)).unwrap(),
            None
        );
    }

    /// A connection bound in this test process's tree: the socket peer is this
    /// process, its root the process that started it.
    fn bound_here() -> (PeerProcess, Binding) {
        let (socket, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        let peer = peer::of_socket(&socket).unwrap();
        let root = peer::inspect(std::os::unix::process::parent_id()).unwrap();
        let binding = capture(Some("tab".into()), Some(peer), Lookup::Root(root))
            .unwrap()
            .unwrap();
        (peer, binding)
    }

    #[test]
    fn a_bound_connection_keeps_its_identity_while_the_terminal_server_cannot_answer() {
        let (peer, binding) = bound_here();
        assert!(binding.root.is_some());
        for liveness in [Liveness::Unknown, Liveness::Known(true)] {
            revalidate(&binding, Some(peer), Lookup::NoRoot(liveness)).unwrap();
        }
    }

    #[test]
    fn a_bound_connection_is_refused_when_its_tab_is_gone_or_left() {
        let (peer, binding) = bound_here();
        let unbound = |result: Result<(), ErrorData>| result.unwrap_err().message == UNBOUND;
        assert!(unbound(revalidate(
            &binding,
            Some(peer),
            Lookup::NoRoot(Liveness::Known(false))
        )));
        // A fresh root is checked itself, never the cached one.
        let mut sibling = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let sibling_root = peer::inspect(sibling.id()).unwrap();
        let fresh = revalidate(&binding, Some(peer), Lookup::Root(sibling_root));
        // A cached root that has exited stands for nothing.
        let exited = Binding {
            root: Some(sibling_root),
            ..binding.clone()
        };
        sibling.kill().unwrap();
        sibling.wait().unwrap();
        assert!(unbound(fresh));
        assert!(!peer::runs(sibling_root));
        for liveness in [Liveness::Unknown, Liveness::Known(true)] {
            assert!(unbound(revalidate(
                &exited,
                Some(peer),
                Lookup::NoRoot(liveness)
            )));
        }
        // A caller outside the cached root's tree is refused, not told to retry.
        let mut stranger = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let elsewhere = Binding {
            root: peer::inspect(stranger.id()),
            ..binding.clone()
        };
        let outside = revalidate(&elsewhere, Some(peer), Lookup::NoRoot(Liveness::Unknown));
        stranger.kill().unwrap();
        stranger.wait().unwrap();
        assert!(unbound(outside));
        assert!(unbound(revalidate(
            &binding,
            None,
            Lookup::NoRoot(Liveness::Unknown)
        )));
    }

    #[test]
    fn a_bound_connection_keeps_its_own_timers_while_the_terminal_server_cannot_answer() {
        use crate::delivery::{authority::TransportIdentity, wakeup};
        let (peer, binding) = bound_here();
        let owner = |lookup: Lookup| {
            wakeup::timer_owner(
                &TransportIdentity::LocalPeer(peer),
                "tab",
                |_| usable_root(lookup, binding.root),
                |_| false,
            )
        };
        assert_eq!(owner(Lookup::NoRoot(Liveness::Unknown)).unwrap().session, "tab");
        assert!(owner(Lookup::NoRoot(Liveness::Known(false))).is_err());
    }

    #[test]
    fn an_unanswerable_lookup_with_nothing_proven_says_retry() {
        let (socket, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        let peer = peer::of_socket(&socket).unwrap();
        let retry = |result: Result<(), ErrorData>| result.unwrap_err().message == UNVERIFIED_YET;
        assert!(retry(
            capture(
                Some("tab".into()),
                Some(peer),
                Lookup::NoRoot(Liveness::Unknown)
            )
            .map(drop)
        ));
        let relay_bound = Binding {
            session: "tab".into(),
            root: None,
        };
        assert!(retry(revalidate(
            &relay_bound,
            Some(peer),
            Lookup::NoRoot(Liveness::Unknown)
        )));
        // Known answers are not temporary.
        for liveness in [Liveness::Known(true), Liveness::Known(false)] {
            assert_eq!(
                capture(Some("tab".into()), Some(peer), Lookup::NoRoot(liveness))
                    .unwrap_err()
                    .message,
                UNBOUND
            );
        }
    }

    #[test]
    fn a_new_connection_proves_its_own_tab_whatever_another_connection_holds() {
        let (_, first) = bound_here();
        let held = std::sync::RwLock::new(None);
        note_initialize(&held, Some(first));
        let mut stranger = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let (socket, _other) = std::os::unix::net::UnixStream::pair().unwrap();
        let peer = peer::of_socket(&socket).unwrap();
        let second = capture(
            Some("tab".into()),
            Some(peer),
            Lookup::Root(peer::inspect(stranger.id()).unwrap()),
        );
        stranger.kill().unwrap();
        stranger.wait().unwrap();
        assert_eq!(second.unwrap_err().message, UNBOUND);
        assert_eq!(stored_session(&held).as_deref(), Some("tab"));
    }

    #[tokio::test]
    async fn a_daemonized_process_that_left_its_tab_is_refused() {
        use codesense_pty_core::activity::peer;
        use std::io::{BufRead, Write};
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("mcp.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let script = r#"import os,socket,sys,time
child=os.fork()
if child:
 print(child,flush=True)
 sys.stdin.readline()
 sys.exit(0)
os.setsid()
s=socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
time.sleep(30)
"#;
        let mut tab = std::process::Command::new("python3")
            .args(["-c", script])
            .arg(&socket)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let root = peer::inspect(tab.id()).unwrap();
        let mut child = String::new();
        std::io::BufReader::new(tab.stdout.take().unwrap())
            .read_line(&mut child)
            .unwrap();
        let child: i32 = child.trim().parse().unwrap();
        tab.stdin.take().unwrap().write_all(b"exit\n").unwrap();
        tab.wait().unwrap();
        let (connection, _) =
            tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
        let caller = peer::of_socket(&connection).unwrap();
        let result = capture(
            Some("departed-tab".into()),
            Some(caller),
            Lookup::Root(root),
        );
        unsafe {
            libc::kill(child, libc::SIGKILL);
        }
        assert!(result.unwrap_err().message.contains("CodeSense terminal"));
    }

    #[test]
    fn unbound_identity_calls_and_bound_impersonation_are_refused() {
        for name in [
            "send_message",
            "reply",
            "react",
            "signal_done",
            "read_messages",
            "inbox_post",
            "send_role_message",
        ] {
            assert!(
                arguments(name, &mut Map::new(), None, None).is_err(),
                "{name}"
            );
            let mut args = Map::new();
            arguments(name, &mut args, Some("session"), Some("handle")).unwrap();
            if [
                "send_message",
                "reply",
                "react",
                "signal_done",
                "read_messages",
            ]
            .contains(&name)
            {
                assert_eq!(args["sessionId"], "session");
                args.insert("sessionId".into(), "human".into());
                assert!(arguments(name, &mut args, Some("session"), Some("handle")).is_err());
            }
        }
        assert!(arguments("list_sessions", &mut Map::new(), None, None).is_ok());
    }
}
