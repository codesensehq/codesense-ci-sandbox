// SPDX-License-Identifier: MIT
//! The host over real PTYs: the layout follows each process exit and removal
//! through the wrapped sink, `open_tab` never leaves a phantom-live entry, and
//! closes free or background exactly as their mode says.

use std::sync::Arc;
use std::time::Duration;

use super::*;
use crate::domain::TeardownReason;
use crate::fixtures::{fixture_runtime, registration, sh, spawn, RecordingSink};
use crate::layout::{EntryState, Tags};

const WAIT: Duration = Duration::from_secs(5);

fn host() -> (Arc<RecordingSink>, TerminalHost) {
    let sink = Arc::new(RecordingSink::default());
    let host = TerminalHost::new(
        ServerIdentity::ephemeral("test-host"),
        sink.clone(),
        fixture_runtime(),
    );
    host.attach_echo_emulator();
    host.layout_snapshot(&viewer(), &ClientBootId::new("boot"))
        .unwrap();
    (sink, host)
}

fn viewer() -> ViewerId {
    ViewerId::new("app")
}

fn sid(id: &str) -> PtySessionId {
    PtySessionId::new(id)
}

fn ws() -> WorkspaceKey {
    WorkspaceKey::new("ws")
}

fn request(session: &str) -> OpenTab {
    OpenTab {
        viewer: viewer(),
        workspace: ws(),
        session: sid(session),
        descriptor: Some(WorkspaceDescriptor {
            display_name: "ws".into(),
            kind: "cli".into(),
            ..WorkspaceDescriptor::default()
        }),
        name: None,
        role: None,
        tags: Tags::new(),
        placement: Placement::Tab,
        index: None,
        activate: true,
    }
}

/// Open `session` running `script`, spawned and registered inside `open_tab`.
fn open(host: &TerminalHost, session: &str, script: &str) -> Result<TabEntry, LayoutError> {
    host.open_tab(
        request(session),
        Box::new(|| spawn(&host.sessions, sh(script), registration(session))),
    )
}

fn state_of(host: &TerminalHost, session: &str) -> Option<EntryState> {
    host.layout_snapshot(&viewer(), &ClientBootId::new("boot"))
        .unwrap()
        .workspaces
        .values()
        .flat_map(|ws| ws.tabs.iter())
        .find(|t| t.session.as_str() == session)
        .map(|t| t.state.clone())
}

fn wait_for_state(host: &TerminalHost, session: &str, done: impl Fn(&EntryState) -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !state_of(host, session).as_ref().is_some_and(&done) {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out; {session} is {:?}",
            state_of(host, session)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn layout_changes(sink: &RecordingSink) -> Vec<(u64, Option<WorkspaceLayoutView>)> {
    sink.snapshot()
        .into_iter()
        .filter_map(|e| match e {
            TerminalDomainEvent::LayoutChanged { rev, layout, .. } => Some((
                rev,
                layout.map(|l| WorkspaceLayoutView {
                    states: l.tabs.into_iter().map(|t| t.state).collect(),
                }),
            )),
            _ => None,
        })
        .collect()
}

#[derive(Debug, PartialEq)]
struct WorkspaceLayoutView {
    states: Vec<EntryState>,
}

#[test]
fn an_exit_turns_the_entry_exited_with_its_real_status_and_close_frees_it() {
    let (sink, host) = host();
    let entry = open(&host, "exit-3", "printf 'last-words'; exit 3").unwrap();
    assert_eq!(
        entry.name,
        crate::memorable_name::memorable_name_from_str("exit-3"),
    );

    wait_for_state(&host, "exit-3", EntryState::is_exited);
    assert_eq!(
        state_of(&host, "exit-3"),
        Some(EntryState::Exited {
            exit_code: Some(3),
            signal: None
        })
    );
    // Announced through the host's own sink, after the entry was placed live.
    let changes = layout_changes(&sink);
    assert!(changes.windows(2).all(|w| w[0].0 < w[1].0), "{changes:?}");
    assert!(changes.iter().any(|(_, layout)| layout
        .as_ref()
        .is_some_and(|l| l.states.iter().any(EntryState::is_exited))));
    // The exited entry keeps its scrollback readable.
    let text = TerminalDomain::read_terminal_text(
        &host,
        &sid("exit-3"),
        u32::MAX,
        crate::emulator::TextMode::Normal,
    )
    .unwrap()
    .text;
    assert!(text.contains("last-words"), "text was {text:?}");

    assert_eq!(
        host.close_tab(&viewer(), &sid("exit-3"), CloseMode::KeepIfBusy),
        Ok(CloseOutcome::Removed)
    );
    assert_eq!(state_of(&host, "exit-3"), None);
    assert!(
        !host.sessions.has_session("exit-3"),
        "closing an exited entry frees its ring"
    );
}

#[test]
fn a_process_that_dies_during_open_is_never_phantom_live() {
    let (sink, host) = host();
    // The child exits and its exit is delivered before the entry is placed.
    let entry = host
        .open_tab(
            request("instant"),
            Box::new(|| {
                spawn(&host.sessions, sh("exit 9"), registration("instant"))?;
                sink.wait_for(WAIT, |events| {
                    events.iter().find(|e| {
                        matches!(e, TerminalDomainEvent::Exit { session, .. } if session.as_str() == "instant")
                    }).map(|_| ())
                });
                Ok(())
            }),
        )
        .unwrap();
    assert_eq!(
        entry.state,
        EntryState::Exited {
            exit_code: Some(9),
            signal: None
        }
    );

    // A session that leaves the registry during the open is never placed.
    let gone = host.open_tab(
        request("reaped"),
        Box::new(|| {
            spawn(&host.sessions, sh("exit 0"), registration("reaped"))?;
            host.sessions.kill("reaped");
            Ok(())
        }),
    );
    assert_eq!(gone, Err(LayoutError::SessionGone(sid("reaped"))));
    assert_eq!(state_of(&host, "reaped"), None);
}

#[test]
fn a_failed_spawn_leaves_nothing_and_frees_the_session_id() {
    let (_sink, host) = host();
    let failed = host.open_tab(request("s"), Box::new(|| Err("no shell".to_string())));
    assert_eq!(failed, Err(LayoutError::Spawn("no shell".to_string())));
    assert_eq!(state_of(&host, "s"), None);
    open(&host, "s", "sleep 30; true").expect("the id is free again");
    host.close_workspace(&ws()).unwrap();
}

#[test]
fn close_keeps_a_busy_process_in_the_background_and_kills_an_idle_one() {
    let (_sink, host) = host();
    open(&host, "busy", "sleep 30; true").unwrap();
    open(&host, "idle", "read line").unwrap();
    // Let the shells fork their foreground child (or settle in `read`).
    let deadline = std::time::Instant::now() + WAIT;
    while host.sessions.is_idle("busy").unwrap() {
        assert!(std::time::Instant::now() < deadline, "sleep never started");
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(
        host.close_tab(&viewer(), &sid("busy"), CloseMode::KeepIfBusy),
        Ok(CloseOutcome::Backgrounded)
    );
    let snap = host
        .layout_snapshot(&viewer(), &ClientBootId::new("boot"))
        .unwrap();
    let placements: Vec<(&str, Placement)> = snap.workspaces[&ws()]
        .tabs
        .iter()
        .map(|t| (t.session.as_str(), t.placement))
        .collect();
    assert_eq!(
        placements,
        vec![("busy", Placement::Background), ("idle", Placement::Tab)]
    );
    assert!(
        host.sessions.has_session("busy"),
        "the process kept running"
    );

    assert_eq!(
        host.close_tab(&viewer(), &sid("idle"), CloseMode::KeepIfBusy),
        Ok(CloseOutcome::Killed)
    );
    assert!(!host.sessions.has_session("idle"));
    assert_eq!(
        host.close_tab(&viewer(), &sid("busy"), CloseMode::Kill),
        Ok(CloseOutcome::Killed)
    );
    assert!(!host.sessions.has_session("busy"));
    assert_eq!(state_of(&host, "busy"), None);
}

#[test]
fn a_session_killed_outside_the_layout_leaves_it() {
    let (sink, host) = host();
    open(&host, "a", "sleep 30; true").unwrap();
    host.sessions.kill("a");
    assert_eq!(state_of(&host, "a"), None);
    assert!(sink.snapshot().iter().any(|e| matches!(
        e,
        TerminalDomainEvent::SessionRemoved { session, reason: TeardownReason::Kill }
            if session.as_str() == "a"
    )));
    assert_eq!(layout_changes(&sink).last(), Some(&(2, None)));
}

#[test]
fn closing_a_workspace_kills_every_session() {
    let (_sink, host) = host();
    open(&host, "a", "sleep 30; true").unwrap();
    open(&host, "b", "sleep 30; true").unwrap();
    host.close_workspace(&ws()).unwrap();
    assert!(!host.sessions.has_session("a") && !host.sessions.has_session("b"));
    assert!(host
        .layout_snapshot(&viewer(), &ClientBootId::new("boot"))
        .unwrap()
        .workspaces
        .is_empty());
}

#[test]
fn the_session_operations_work_through_the_domain_contract() {
    let (_sink, host) = host();
    open(&host, "cat", "cat").unwrap();
    let domain: &dyn TerminalDomain = &host;
    let session = sid("cat");
    domain.write(&session, b"ping\n").unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    let mut seen = String::new();
    while !seen.contains("ping") {
        assert!(std::time::Instant::now() < deadline, "no echo: {seen:?}");
        seen = domain
            .read_terminal_text(&session, u32::MAX, crate::emulator::TextMode::Normal)
            .unwrap()
            .text;
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(domain
        .subscribe(&session, &ViewportId::new("vp"))
        .unwrap()
        .snapshot
        .vt
        .contains("ping"));
    let text = domain
        .read_terminal_text(&session, 5, crate::emulator::TextMode::Normal)
        .unwrap();
    assert!(text.text.contains("ping"), "{text:?}");
    #[cfg(not(feature = "e2e-dump"))]
    assert_eq!(
        domain.debug_dump(&session).unwrap_err().0,
        crate::domain::DEBUG_DUMP_UNSUPPORTED
    );
    domain.resize(&viewer(), &session, 120, 40).unwrap();
    // The line reaches the process (the confirm verdict is the protocol's own,
    // tested with the runtime).
    domain.submit_line(&session, "pong", 5, 5, 200, 0).unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    let mut echoed = String::new();
    while !echoed.contains("pong") {
        assert!(std::time::Instant::now() < deadline, "no echo: {echoed:?}");
        echoed = domain
            .read_terminal_text(&session, u32::MAX, crate::emulator::TextMode::Normal)
            .unwrap()
            .text;
        std::thread::sleep(Duration::from_millis(5));
    }
    domain.kill(&session).unwrap();
    assert_eq!(state_of(&host, "cat"), None);
}

#[test]
fn a_relaunch_frees_the_exited_sessions_it_prunes() {
    let (sink, host) = host();
    open(&host, "live", "sleep 30; true").unwrap();
    open(&host, "dead", "exit 4").unwrap();
    wait_for_state(&host, "dead", EntryState::is_exited);
    let rev_before = host
        .layout_snapshot(&viewer(), &ClientBootId::new("boot"))
        .unwrap()
        .rev;

    let relaunched = host
        .layout_snapshot(&viewer(), &ClientBootId::new("boot-2"))
        .unwrap();
    assert_eq!(relaunched.rev, rev_before + 1);
    assert_eq!(state_of(&host, "dead"), None);
    assert!(
        !host.sessions.has_session("dead"),
        "its scrollback is freed"
    );
    assert_eq!(
        layout_changes(&sink).last().map(|c| c.0),
        Some(rev_before + 1)
    );
    assert!(host.sessions.has_session("live"));
    host.close_workspace(&ws()).unwrap();
}

#[test]
fn a_spawn_that_registers_another_session_is_never_placed() {
    let (_sink, host) = host();
    let wrong = host.open_tab(
        request("asked-for"),
        Box::new(|| spawn(&host.sessions, sh("sleep 30; true"), registration("other"))),
    );
    assert_eq!(wrong, Err(LayoutError::SessionGone(sid("asked-for"))));
    assert_eq!(state_of(&host, "asked-for"), None);
    host.sessions.kill("other");
    assert!(
        host.reserve_tab(&request("asked-for")).is_ok(),
        "the id is free again"
    );
}

/// A spawn that panics leaves no reservation behind.
#[test]
fn a_panicking_spawn_frees_the_session_id() {
    let (_sink, host) = host();
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        host.open_tab(request("boom"), Box::new(|| panic!("spawn blew up")))
    }));
    assert!(panicked.is_err());
    assert!(
        host.reserve_tab(&request("boom")).is_ok(),
        "the id is free again"
    );
}

#[test]
fn opening_an_id_already_in_the_registry_is_refused() {
    let (_sink, host) = host();
    spawn(&host.sessions, sh("sleep 30; true"), registration("taken")).unwrap();
    let spawned = std::cell::Cell::new(false);
    let refused = host.open_tab(
        request("taken"),
        Box::new(|| {
            spawned.set(true);
            Ok(())
        }),
    );
    assert_eq!(refused, Err(LayoutError::AlreadyPlaced(sid("taken"))));
    assert!(!spawned.get(), "no spawn displaces the running session");
    assert!(host.sessions.has_session("taken"));
    host.sessions.kill("taken");
}

#[test]
fn the_dead_session_cap_never_evicts_an_exited_entry() {
    let (_sink, host) = host();
    let ids: Vec<String> = (0..10).map(|i| format!("gone-{i}")).collect();
    for id in &ids {
        open(&host, id, "exit 0").unwrap();
        wait_for_state(&host, id, EntryState::is_exited);
    }
    // One more spawn runs the cap pass over ten unviewed dead sessions.
    open(&host, "trigger", "sleep 30; true").unwrap();
    for id in &ids {
        assert!(host.sessions.has_session(id), "{id} kept its scrollback");
        assert!(state_of(&host, id).is_some_and(|s| s.is_exited()));
    }
    host.close_workspace(&ws()).unwrap();
}

/// A respawn whose spawn panics is abandoned, not left half-done: the tab is
/// not "already restarting" forever, and the next Restart runs.
#[test]
fn a_panicking_respawn_spawn_releases_the_hold() {
    let (_sink, host) = host();
    open(&host, "rp", "printf 'HISTORY\\n'; exit 3").unwrap();
    wait_for_state(&host, "rp", EntryState::is_exited);

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        host.respawn_tab(&sid("rp"), Box::new(|| panic!("spawn blew up")))
    }));
    assert!(panicked.is_err());
    assert!(state_of(&host, "rp").is_some_and(|s| s.is_exited()));

    let entry = host
        .respawn_tab(
            &sid("rp"),
            Box::new(|| spawn(&host.sessions, sh("sleep 60"), registration("rp"))),
        )
        .expect("the next restart is not refused as already in progress");
    assert_eq!(entry.state, EntryState::Live);
    let _ = host.kill(&sid("rp"));
}

/// An exited entry whose id still runs a process, as the host would see a
/// process that outlived its terminal: the layout heard its end-of-stream
/// exit (no status), the process lives on. `silent` also ends its output.
fn outlived(host: &TerminalHost, session: &str, silent: bool) {
    open(host, session, "sleep 60").unwrap();
    if silent {
        let sessions = host.sessions.sessions.lock().unwrap();
        sessions.get(session).unwrap().runtime.end_output();
    }
    host.apply(host.layout.on_exit(&sid(session), None));
    assert!(state_of(host, session).is_some_and(|s| s.is_exited()));
}

/// A process that closed its terminal but kept running shows nothing and takes
/// no input: Restart reaps it and starts a new one — it is never revived as the
/// tab's live process (only a respawn's own process is).
#[test]
fn a_process_that_outlived_its_terminal_is_replaced_not_revived() {
    let (_sink, host) = host();
    outlived(&host, "orphan", true);

    let mut spawned = false;
    host.respawn_tab(
        &sid("orphan"),
        Box::new(|| {
            spawned = true;
            spawn(&host.sessions, sh("sleep 60"), registration("orphan"))
        }),
    )
    .unwrap();
    // (Its entry's state is not asserted: this stand-in's reader still runs, so
    // the reap's kill reports one more exit under the id — a real silent
    // process reported its end-of-stream exit long before.)
    assert!(
        spawned,
        "a new process was started, the silent one not revived"
    );
    assert!(
        !host.output_ended(&sid("orphan")),
        "the new process, talking"
    );
    let _ = host.kill(&sid("orphan"));
}

/// A process still talking under an exited entry that no respawn started is
/// not adopted as the tab's new life: the restart is refused as still running,
/// and the next one is not held up.
#[test]
fn only_a_respawns_own_process_is_revived() {
    let (_sink, host) = host();
    outlived(&host, "stray", false);

    let refused = host.respawn_tab(&sid("stray"), Box::new(|| panic!("never spawns")));
    assert_eq!(refused, Err(LayoutError::Running(sid("stray"))));
    assert!(state_of(&host, "stray").is_some_and(|s| s.is_exited()));
    let again = host.respawn_tab(&sid("stray"), Box::new(|| panic!("never spawns")));
    assert_eq!(
        again,
        Err(LayoutError::Running(sid("stray"))),
        "no hold left behind"
    );
    let _ = host.kill(&sid("stray"));
}

#[test]
fn conflicting_cwd_is_refused_before_spawn_including_pending_reservations() {
    let (_, host) = host();
    let mut first = request("cwd-first");
    first.descriptor.as_mut().unwrap().worktree_path = Some("/one".into());
    host.reserve_tab(&first).unwrap();
    let mut second = request("cwd-second");
    second.descriptor.as_mut().unwrap().worktree_path = Some("/two".into());
    let result = host.open_tab(second.clone(), Box::new(|| panic!("must not spawn")));
    assert!(matches!(result, Err(LayoutError::CwdConflict(_))));
    host.finish_tab(first, Err("cancelled".into())).unwrap_err();
    host.reserve_tab(&second).unwrap();
}

#[test]
fn existing_tabs_keep_their_host_directory() {
    let (_, host) = host();
    let mut first = request("cwd-live");
    first.descriptor.as_mut().unwrap().worktree_path = Some("/one".into());
    host.open_tab(
        first,
        Box::new(|| spawn(&host.sessions, sh("sleep 30"), registration("cwd-live"))),
    )
    .unwrap();
    let mut second = request("cwd-other");
    second.descriptor.as_mut().unwrap().worktree_path = Some("/two".into());
    assert!(matches!(
        host.open_tab(second, Box::new(|| panic!("must not spawn"))),
        Err(LayoutError::CwdConflict(_))
    ));
    host.kill(&sid("cwd-live")).unwrap();
}

#[test]
fn cleanup_counts_cold_and_warm_sessions_until_finish_tab() {
    for warm in [false, true] {
        let (_sink, host) = host();
        let source = tempfile::tempdir().unwrap();
        let cwd = source.path().to_str().unwrap();
        let mut open = request("opening");
        open.descriptor.as_mut().unwrap().worktree_path = Some(source.path().to_path_buf());
        host.reserve_tab(&open).unwrap();
        if warm {
            let mut member = registration("pool");
            member.is_pool_member = true;
            spawn(&host.sessions, sh("cat"), member).unwrap();
            let removing = crate::remote_workspace::removal::begin_removal(cwd).unwrap();
            let refused = host.handoff_warm_member(
                &sid("pool"),
                &sid("opening"),
                cwd,
                80,
                24,
                SessionMeta::default(),
            );
            assert!(refused.unwrap_err().to_string().contains("removing"));
            assert!(host.sessions.has_session("pool"));
            drop(removing);
            host.handoff_warm_member(
                &sid("pool"),
                &sid("opening"),
                cwd,
                80,
                24,
                SessionMeta::default(),
            )
            .unwrap();
        } else {
            let admission = crate::remote_workspace::removal::admit_spawn(cwd).unwrap();
            spawn(&host.sessions, sh("cat"), registration("opening")).unwrap();
            drop(admission);
        }
        // Spawn/Handoff returned and released admission, but FinishTab has not run.
        assert!(host.tab_entries().unwrap().is_empty());
        let removing = crate::remote_workspace::removal::begin_removal(cwd).unwrap();
        assert_eq!(
            host.worktree_usage(cwd).unwrap().opening,
            vec![sid("opening")]
        );
        drop(removing);
        host.finish_tab(open, Ok(())).unwrap();
        assert_eq!(
            host.worktree_usage(cwd).unwrap().entries[0].1.session,
            sid("opening")
        );
        host.kill(&sid("opening")).unwrap();
    }
}
