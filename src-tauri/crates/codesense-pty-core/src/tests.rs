// SPDX-License-Identifier: MIT
//! Registry tests over real PTYs: spawn → stream → exit through the sink, the
//! teardown/reap family, the dead-session cap, the exit-time complement kill,
//! warm handoff, and the liveness / replay queries.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::domain::{
    PortUnavailable, PtySessionId, ReapOutcome, TeardownReason, TerminalDomainEvent,
    TerminalEventSink, TerminalRegistry,
};
use crate::fixtures::{
    fixture_state, make_isolated_session, registration, sh, spawn, FakeWorker, NoopSink,
    RecordingSink,
};
use crate::runner::ExitInfo;
use crate::state::{is_persisting, PtyState, SessionMeta, TerminalSession};
use crate::term_state::{StreamId, SubscriberHandle};

fn recording_state() -> (Arc<RecordingSink>, PtyState) {
    let sink = Arc::new(RecordingSink::default());
    let state = fixture_state(sink.clone());
    (sink, state)
}

fn exit_of(events: &[TerminalDomainEvent], id: &str) -> Option<(Option<ExitInfo>, bool)> {
    events.iter().find_map(|event| match event {
        TerminalDomainEvent::Exit {
            session,
            status,
            pool_member,
            ..
        } if session.as_str() == id => Some((status.clone(), *pool_member)),
        _ => None,
    })
}

/// Build a real `TerminalSession` backed by an actual PTY + child so the cap's
/// end-to-end teardown (map removal + secondary-index cleanup) can be exercised
/// against the registry directly. `alive=false` blocks until the child has
/// exited (deterministic — no sleep-based race) and stamps its death order.
fn make_test_session(id: &str, alive: bool, has_subscribers: bool) -> TerminalSession {
    let session = make_isolated_session(
        id,
        if alive { "sleep 30" } else { "exit 0" },
        has_subscribers,
    )
    .expect("test session");
    if !alive {
        session.pty.child().wait();
        session.runtime.mark_dead();
    }
    session
}

/// The spawn → stream → exit pipeline end to end: output reaches the sink on
/// the session channel with its byte watermark, advances the session's byte
/// count, and the natural exit reports the child's REAL, non-zero exit status.
#[test]
fn spawned_session_streams_output_and_reports_its_real_exit_status() {
    let (sink, state) = recording_state();
    spawn(
        &state,
        sh("printf 'hello-from-pty'; exit 3"),
        registration("exit-3"),
    )
    .expect("spawn");

    let (status, pool_member) =
        sink.wait_for(Duration::from_secs(5), |events| exit_of(events, "exit-3"));
    assert_eq!(
        status,
        Some(ExitInfo {
            exit_code: Some(3),
            signal: None,
        }),
        "pty-exit carries the real, non-zero exit status"
    );
    assert!(!pool_member);

    let events = sink.snapshot();
    let output: String = events
        .iter()
        .filter_map(|event| match event {
            TerminalDomainEvent::Output { session, data, .. } if session.as_str() == "exit-3" => {
                Some(data.as_str())
            }
            _ => None,
        })
        .collect();
    assert!(output.contains("hello-from-pty"), "output was {output:?}");
    let last_seq = events
        .iter()
        .rev()
        .find_map(|event| match event {
            TerminalDomainEvent::Output { seq, .. } => Some(*seq),
            _ => None,
        })
        .expect("an output batch");
    let head = state.shared_handle("exit-3").expect("retained");
    assert_eq!(
        head.lock().unwrap().seq,
        last_seq,
        "the last batch's seq is the session's byte count"
    );
    // Retained for replay after natural exit, stamped dead, reads idle.
    assert!(state.has_session("exit-3"));
    assert!(state.is_idle("exit-3").unwrap());
    state.kill("exit-3");
}

/// `session`'s exit status and whether the host asked for it.
fn requested_exit_of(events: &[TerminalDomainEvent], id: &str) -> Option<(Option<ExitInfo>, bool)> {
    events.iter().find_map(|event| match event {
        TerminalDomainEvent::Exit {
            session,
            status,
            requested,
            ..
        } if session.as_str() == id => Some((status.clone(), *requested)),
        _ => None,
    })
}

/// A child that ends on its own — cleanly, with an error, or killed by a
/// signal from outside — reports an exit nobody asked for, with its real
/// status; a child the host kills reports a requested exit.
#[test]
fn an_exit_reports_whether_the_host_asked_for_it() {
    let (sink, state) = recording_state();
    spawn(&state, sh("exit 0"), registration("clean")).expect("spawn");
    spawn(&state, sh("exit 7"), registration("failed")).expect("spawn");
    spawn(&state, sh("kill -9 $$"), registration("sigkill")).expect("spawn");
    spawn(&state, sh("sleep 30"), registration("stopped")).expect("spawn");
    let wait = |id: &'static str| {
        sink.wait_for(Duration::from_secs(5), |events| {
            requested_exit_of(events, id)
        })
    };

    assert_eq!(
        wait("clean"),
        (
            Some(ExitInfo {
                exit_code: Some(0),
                signal: None
            }),
            false
        )
    );
    assert_eq!(
        wait("failed"),
        (
            Some(ExitInfo {
                exit_code: Some(7),
                signal: None
            }),
            false
        )
    );
    let (killed, requested) = wait("sigkill");
    let killed = killed.expect("reaped");
    assert_eq!(killed.exit_code, None);
    assert!(killed.signal.is_some(), "a signal death names its signal");
    assert!(!requested, "a kill from outside the host is not requested");

    state.kill("stopped");
    let (_, requested) = wait("stopped");
    assert!(requested, "the host's own kill is a requested exit");
    for id in ["clean", "failed", "sigkill"] {
        state.kill(id);
    }
}

/// An attached viewport receives each batch on its own stream, and the resize /
/// write / subscribe surface drives the live PTY.
#[test]
fn attached_viewport_streams_live_bytes_on_its_own_stream() {
    let (sink, state) = recording_state();
    crate::emulator::attach_echo(&state.emulator);
    spawn(&state, sh("cat"), registration("echo")).expect("spawn");
    let shared = state.shared_handle("echo").unwrap();
    let attach = state.attach_viewport("echo", &shared).unwrap();
    state.resize("echo", 120, 40).unwrap();
    state.write("echo", b"ping\n").unwrap();

    let streamed = sink.wait_for(Duration::from_secs(5), |events| {
        let data: String = events
            .iter()
            .filter_map(|event| match event {
                TerminalDomainEvent::StreamOutput { stream, data, .. }
                    if *stream == attach.stream_id =>
                {
                    Some(data.as_str())
                }
                _ => None,
            })
            .collect();
        data.contains("ping").then_some(data)
    });
    assert!(streamed.contains("ping"));
    assert!(
        shared.lock().unwrap().subscribers[&attach.stream_id].bytes_sent > 0,
        "delivery telemetry counts the streamed bytes"
    );
    state.unsubscribe(attach.stream_id).unwrap();
    assert!(shared.lock().unwrap().subscribers.is_empty());
    state.kill("echo");
}

/// The correlation triplet folded into [`SessionMeta`] must round-trip
/// through the destructure the spawn / warm-handoff paths do at their body tops.
/// Also pins the load-bearing invariant that `SessionMeta::default()` is
/// all-`None`: that is the warm-pool-member sentinel the persistence classifier
/// keys on (`is_persisting` gets `has_story_id = false` for it, so an unattached
/// pool shell is never persisted).
#[test]
fn session_meta_round_trips_and_default_is_pool_member_sentinel() {
    let meta = SessionMeta {
        story_id: Some("RF-15".into()),
        workspace_id: Some("ws-1".into()),
        epic_id: Some("09".into()),
    };
    // Clone (the warm-handoff path) then destructure (the body-top pattern).
    let SessionMeta {
        story_id,
        workspace_id,
        epic_id,
    } = meta.clone();
    assert_eq!(story_id.as_deref(), Some("RF-15"));
    assert_eq!(workspace_id.as_deref(), Some("ws-1"));
    assert_eq!(epic_id.as_deref(), Some("09"));

    // Sentinel: default is all-None → classifier sees a non-persisting pool
    // member.
    let default = SessionMeta::default();
    assert!(default.story_id.is_none());
    assert!(default.workspace_id.is_none());
    assert!(default.epic_id.is_none());
    assert!(!is_persisting(
        /* is_pool_member */ true,
        default.story_id.is_some(),
        /* has_subscribers */ false,
    ));
}

/// RF-perf-6: the dead-session cap decision. Tuple is
/// `(id, alive, has_subscribers, exit_seq)`. LIVE sessions and currently-VIEWED
/// dead sessions are never eviction candidates; among the remaining
/// (dead, unviewed, stamped) sessions the `cap` most-recently-dead (highest
/// `exit_seq`) are retained and the older tail is returned OLDEST-first; a
/// dead-but-unstamped session (`exit_seq == None`) is retained this pass. Pure,
/// so the whole rule is proven without a real PTY.
#[test]
fn dead_sessions_to_evict_bounds_dead_and_never_touches_live() {
    use crate::state::dead_sessions_to_evict;
    // Empty / at-or-below-cap → nothing evicted.
    assert!(dead_sessions_to_evict(&[], 2).is_empty());
    let live_only = vec![
        ("a".to_string(), true, false, None),
        ("b".to_string(), true, false, None),
    ];
    assert!(
        dead_sessions_to_evict(&live_only, 1).is_empty(),
        "live sessions are never eviction candidates"
    );
    let at_cap = vec![
        ("d1".to_string(), false, false, Some(1)),
        ("d2".to_string(), false, false, Some(2)),
    ];
    assert!(
        dead_sessions_to_evict(&at_cap, 2).is_empty(),
        "dead count == cap → nothing evicted"
    );

    // Over cap (input intentionally unsorted to exercise the ordering):
    // three dead+unviewed+stamped + one live + one dead-but-unstamped + one
    // dead-BUT-VIEWED (open scrollback tab), cap = 1.
    let over = vec![
        ("d_new".to_string(), false, false, Some(30)),
        ("live".to_string(), true, false, None),
        ("d_old".to_string(), false, false, Some(10)),
        ("fresh".to_string(), false, false, None),
        ("viewed_old".to_string(), false, true, Some(1)), // dead but on-screen
        ("d_mid".to_string(), false, false, Some(20)),
    ];
    let victims = dead_sessions_to_evict(&over, 1);
    // Keep the single most-recently-dead unviewed (`d_new`); evict the older
    // unviewed tail, oldest death first. `viewed_old` — oldest of all — is
    // NEVER evicted because a viewport is attached.
    assert_eq!(victims, vec!["d_old".to_string(), "d_mid".to_string()]);
    for retained in ["live", "fresh", "d_new", "viewed_old"] {
        assert!(
            !victims.contains(&retained.to_string()),
            "{retained} must be retained"
        );
    }
}

/// RF-perf-17: `live_session_home_dirs` returns the stable spawn dirs of
/// exactly the ALIVE sessions that HAVE an anchor — a dead (retained-for-replay)
/// session and an anchorless (`home_cwd == None`) session both contribute
/// nothing. This is the source-of-truth for reverse-sync liveness, so the
/// membership-vs-liveness rule and the None-anchor skip are guarded here.
#[test]
fn live_session_home_dirs_reports_alive_anchored_only() {
    let state = fixture_state(Arc::new(NoopSink));
    let dir_a = PathBuf::from("/tmp/rfperf17/worktree-a");
    let dir_b = PathBuf::from("/tmp/rfperf17/worktree-b");
    let seed = |id: &str, alive: bool, home: Option<PathBuf>| {
        let mut s = make_test_session(id, alive, false);
        s.home_cwd = home;
        state.sessions.lock().unwrap().insert(id.to_string(), s);
    };
    seed("live-a", true, Some(dir_a.clone())); // alive + anchored → included
    seed("dead-b", false, Some(dir_b.clone())); // dead → excluded despite anchor
    seed("live-none", true, None); // alive but no anchor → excluded

    let mut got = state
        .live_session_home_dirs()
        .expect("uncontended try_lock");
    got.sort();
    assert_eq!(got, vec![dir_a]);
    for id in ["live-a", "dead-b", "live-none"] {
        state.kill(id);
    }
}

/// `live_session_ids` is the live-PTY arm of the union roster — it
/// enumerates EVERY alive child, EXCLUDES an exited-but-replay-retained session
/// (dead reads dead, not live) and an unattached warm-pool member, and surfaces the
/// emitted `current_id`. A registry-less bare session (a root/notes shell that never
/// entered the launch registry) is included exactly like any other alive session.
#[test]
fn live_session_ids_reports_alive_non_pool_only() {
    let state = fixture_state(Arc::new(NoopSink));
    let seed = |id: &str, alive: bool, pool: bool| {
        let s = make_test_session(id, alive, false);
        s.runtime.is_pool_member.store(pool, Ordering::Relaxed);
        state.sessions.lock().unwrap().insert(id.to_string(), s);
    };
    seed("registry-less-live", true, false); // bare session, alive → included
    seed("exited-replayed", false, false); // dead-but-retained → excluded
    seed("warm-member", true, true); // alive pool member → excluded

    let got = state.live_session_ids().expect("uncontended lock");
    assert_eq!(
        got,
        HashSet::from(["registry-less-live".to_string()]),
        "only the alive, non-pool session is a live-PTY id"
    );
    for id in ["registry-less-live", "exited-replayed", "warm-member"] {
        state.kill(id);
    }
}

/// A respawn reaps only a session whose process has ended: a running one is
/// refused and kept, an ended one leaves the registry before the call returns
/// (the host told, as a reap, not a kill), and an unknown id is reported
/// missing.
#[test]
fn reap_ended_refuses_a_running_session_and_removes_an_ended_one() {
    let (sink, state) = recording_state();
    let seed = |id: &str, alive: bool| {
        state
            .sessions
            .lock()
            .unwrap()
            .insert(id.to_string(), make_test_session(id, alive, false));
    };
    seed("running", true);
    seed("ended", false);

    let running = state.reap_ended("running", false);
    let running_kept = state.has_session("running");
    state.kill("running");
    assert_eq!(running, ReapOutcome::Running);
    assert!(running_kept, "a running session is never reaped");

    assert_eq!(state.reap_ended("ended", false), ReapOutcome::Reaped);
    assert!(!state.has_session("ended"), "gone before the call returns");
    assert!(sink
        .snapshot()
        .contains(&TerminalDomainEvent::SessionRemoved {
            session: PtySessionId::new("ended"),
            reason: TeardownReason::Reap,
        }));
    assert_eq!(state.reap_ended("ended", false), ReapOutcome::Missing);
}

/// RF-perf-6 — end-to-end: `enforce_dead_session_cap` settles the
/// `sessions` map at the cap for dead+unviewed sessions, evicting the OLDEST
/// dead first; a LIVE session and a VIEWED dead session are never evicted. Uses
/// real PTYs so the map removal is genuinely exercised, not mocked. Evictions
/// are background reclaims, never human kills.
#[test]
fn enforce_dead_session_cap_settles_map() {
    let (sink, state) = recording_state();
    let seed = |id: &str, alive: bool, viewed: bool| {
        state
            .sessions
            .lock()
            .unwrap()
            .insert(id.to_string(), make_test_session(id, alive, viewed));
    };
    // Death order dead1 < dead2 < dead3 (sequential mark_dead).
    seed("dead1", false, false);
    seed("dead2", false, false);
    seed("dead3", false, false);
    seed("live", true, false);
    seed("viewed", false, true); // dead but on-screen

    state.enforce_dead_session_cap(1); // cap=1 → keep newest dead

    // Snapshot every asserted fact into locals FIRST, then reap the live child
    // BEFORE asserting — so a failing assertion can never orphan the `sleep 30`
    // process (the native child is not killed on drop).
    let (has_dead1, has_dead2, has_dead3, has_live, has_viewed, map_len) = {
        let s = state.sessions.lock().unwrap();
        (
            s.contains_key("dead1"),
            s.contains_key("dead2"),
            s.contains_key("dead3"),
            s.contains_key("live"),
            s.contains_key("viewed"),
            s.len(),
        )
    };
    state.kill("live");

    assert!(!has_dead1, "oldest dead evicted");
    assert!(!has_dead2, "next-oldest dead evicted");
    assert!(has_dead3, "newest dead within cap kept");
    assert!(has_live, "live never evicted");
    assert!(has_viewed, "viewed dead never evicted");
    assert_eq!(map_len, 3, "map settles at cap(1) + live + viewed");
    let expired: Vec<String> = sink
        .snapshot()
        .into_iter()
        .filter_map(|event| match event {
            TerminalDomainEvent::SessionRemoved {
                session,
                reason: TeardownReason::Expire,
            } => Some(session.as_str().to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(expired, vec!["dead1".to_string(), "dead2".to_string()]);
}

/// A failed reader/batcher thread start tears the half-started session down: the
/// child and its process group are reaped, the registry forgets it, and the host
/// is told (as a background reclaim) so it releases the session's dashboard
/// binding.
#[cfg(target_os = "macos")]
#[test]
fn failed_background_start_releases_pty_and_dashboard_registration() {
    use crate::fixtures::{
        stable_process_identity, wait_for_process_exit, LeakSessionGuard, RegisteredLeakSession,
    };
    use crate::state::BackgroundThreadFailure;

    for (role, failure) in [
        ("PTY reader", BackgroundThreadFailure::Reader),
        ("PTY batcher", BackgroundThreadFailure::Batcher),
    ] {
        let (sink, state) = recording_state();
        let mut session = LeakSessionGuard::new(
            make_isolated_session("startup-failed", "sleep 30", false)
                .expect("startup-failure test session"),
        );
        let pid = session.session().pty.pid().expect("child pid") as libc::pid_t;
        let pgid = session
            .session()
            .pty
            .child()
            .process_group()
            .expect("child process group");
        let identity = stable_process_identity(pid).expect("stable child identity");
        state
            .insert_session_reaping_displaced("startup-failed".to_string(), session.take())
            .expect("insert failed-start session");
        let registered = RegisteredLeakSession {
            state: &state,
            id: "startup-failed",
        };

        let error = state
            .spawn_session_background_threads(
                "startup-failed",
                ("injected-reader".to_string(), || {}),
                ("injected-batcher".to_string(), || {}),
                failure,
            )
            .expect_err("injected thread start must fail");
        assert!(error.contains(role), "error must identify failed thread");
        assert!(!state.has_session("startup-failed"));
        assert!(
            sink.snapshot()
                .contains(&TerminalDomainEvent::SessionRemoved {
                    session: PtySessionId::new("startup-failed"),
                    reason: TeardownReason::Expire,
                }),
            "failed startup must hand the host its teardown so the dashboard binding is released"
        );
        wait_for_process_exit(pid, Some(pgid), identity, Duration::from_secs(2))
            .expect("failed-start child should be reaped");
        drop(registered);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn same_id_replacement_reaps_displaced_process_group() {
    use crate::fixtures::{
        stable_process_identity, wait_for_process_exit, LeakSessionGuard, RegisteredLeakSession,
    };

    let state = fixture_state(Arc::new(NoopSink));
    let mut old = LeakSessionGuard::new(
        make_isolated_session("replace-me", "sleep 30 & wait", false).expect("old test session"),
    );
    let old_pid = old.session().pty.pid().expect("old child pid") as libc::pid_t;
    let old_pgid = old
        .session()
        .pty
        .child()
        .process_group()
        .expect("old child process group");
    let old_identity = stable_process_identity(old_pid).expect("old stable identity");
    state
        .insert_session_reaping_displaced("replace-me".to_string(), old.take())
        .expect("insert old session");
    let registered = RegisteredLeakSession {
        state: &state,
        id: "replace-me",
    };

    let mut replacement = LeakSessionGuard::new(
        make_isolated_session("replace-me", "sleep 30", false).expect("replacement test session"),
    );
    let replacement_pid = replacement
        .session()
        .pty
        .pid()
        .expect("replacement child pid") as libc::pid_t;
    let replacement_pgid = replacement
        .session()
        .pty
        .child()
        .process_group()
        .expect("replacement child process group");
    let replacement_identity =
        stable_process_identity(replacement_pid).expect("replacement stable identity");
    state
        .insert_session_reaping_displaced("replace-me".to_string(), replacement.take())
        .expect("replace session");

    wait_for_process_exit(
        old_pid,
        Some(old_pgid),
        old_identity,
        Duration::from_secs(2),
    )
    .expect("displaced child and descendants should be reaped");
    drop(registered);
    wait_for_process_exit(
        replacement_pid,
        Some(replacement_pgid),
        replacement_identity,
        Duration::from_secs(2),
    )
    .expect("replacement child should be reaped");
}

#[cfg(unix)]
#[test]
fn teardown_reaps_the_killed_child_without_blocking_the_caller() {
    use crate::fixtures::{stable_process_identity, wait_for_process_exit};

    let (sink, state) = recording_state();
    let session = make_test_session("reap-me", true, false);
    let pid = session.pty.pid().expect("test child pid") as libc::pid_t;
    state
        .sessions
        .lock()
        .unwrap()
        .insert("reap-me".to_string(), session);

    let pgid = unsafe { libc::getpgid(pid) };
    assert!(pgid > 0, "test child process group must be measurable");
    let identity = stable_process_identity(pid).expect("stable child identity");
    state.kill("reap-me");

    wait_for_process_exit(pid, Some(pgid), identity, Duration::from_secs(2))
        .expect("killed child should be reaped before the deadline");
    assert_eq!(
        sink.snapshot(),
        vec![TerminalDomainEvent::SessionRemoved {
            session: PtySessionId::new("reap-me"),
            reason: TeardownReason::Kill,
        }],
        "a deliberate close is handed to the host as a kill"
    );
}

/// A PTY write to one terminal must NOT serialize behind
/// another terminal's in-flight `write_all`+`flush`. Mirrors the registry's
/// `write` structure — clone the per-session `Arc<Mutex<writer>>` out of the map
/// under a brief map lock, then write under the PER-SESSION lock — and proves
/// that holding session A's writer lock does not block cloning-out and writing
/// session B. Deterministic via barriers (no sleeps): if writes shared one lock,
/// B's write (strictly ordered while A's lock is held) would deadlock and the
/// test would hang.
#[test]
fn per_session_writer_lock_does_not_serialize_cross_session_writes() {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Barrier;
    use std::thread;

    struct CountingWriter(Arc<AtomicUsize>);
    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.fetch_add(buf.len(), Ordering::SeqCst);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;
    let a_count = Arc::new(AtomicUsize::new(0));
    let b_count = Arc::new(AtomicUsize::new(0));
    let sessions: Mutex<HashMap<String, SharedWriter>> = Mutex::new(HashMap::new());
    {
        let mut map = sessions.lock().unwrap();
        map.insert(
            "a".into(),
            Arc::new(Mutex::new(Box::new(CountingWriter(a_count.clone())))),
        );
        map.insert(
            "b".into(),
            Arc::new(Mutex::new(Box::new(CountingWriter(b_count.clone())))),
        );
    }
    // Clone-out step mirroring the registry's write: brief map lock, then release.
    let clone_writer =
        |id: &str| -> SharedWriter { sessions.lock().unwrap().get(id).unwrap().clone() };

    let a_locked = Arc::new(Barrier::new(2));
    let b_done = Arc::new(Barrier::new(2));

    let a_writer = clone_writer("a");
    let a_locked_t = a_locked.clone();
    let b_done_t = b_done.clone();
    let handle = thread::spawn(move || {
        let mut guard = a_writer.lock().unwrap();
        a_locked_t.wait(); // A's writer lock is now held
        guard.write_all(b"a-bytes").unwrap();
        b_done_t.wait(); // hold A's lock until B has proven it wrote
        drop(guard);
    });

    a_locked.wait(); // block until the thread holds A's writer lock
                     // B's write proceeds while A's writer lock is held — no shared lock, so no
                     // serialization. (If serialized, this would deadlock against `b_done`.)
    {
        let b_writer = clone_writer("b");
        let mut g = b_writer.lock().unwrap();
        g.write_all(b"b-bytes").unwrap();
        g.flush().unwrap();
    }
    assert_eq!(
        b_count.load(Ordering::SeqCst),
        7,
        "B wrote while A's lock was held"
    );
    b_done.wait(); // release the A thread
    handle.join().unwrap();
    assert_eq!(
        a_count.load(Ordering::SeqCst),
        7,
        "A also completed its write"
    );
}

/// The persisting-classifier rule. Active-workspace terminal
/// (subscribers>0) and story-scoped terminal (story_id set) persist; a
/// 0-subscriber, no-story terminal (soft-closed / team-switch-continuity)
/// does not; a warm-pool member never persists regardless.
#[test]
fn is_persisting_matches_ac11_rule() {
    // active-workspace tab (live viewport, no story) → persists
    assert!(is_persisting(false, false, true));
    // story-scoped (story id, no live viewport) → persists
    assert!(is_persisting(false, true, false));
    // soft-closed / team-switch-continuity (no story, no viewport) → killed
    assert!(!is_persisting(false, false, false));
    // warm-pool member is never persisted, even if it somehow looked alive
    assert!(!is_persisting(true, true, true));
}

/// One quit path: the exit-time complement kill reaps every non-persisting
/// session — warm-pool members included — and spares story-scoped and viewed
/// sessions.
#[test]
fn kill_non_persisting_reaps_warm_pool_members_and_spares_persisting() {
    let (sink, state) = recording_state();
    let insert = |id: &str, pool: bool, story: bool, viewed: bool| {
        let mut s = make_test_session(id, true, viewed);
        s.runtime.is_pool_member.store(pool, Ordering::Relaxed);
        s.story_id = story.then(|| "SW-1".to_string());
        state.sessions.lock().unwrap().insert(id.to_string(), s);
    };
    insert("__warm_pool_1", true, false, false);
    insert("orphan", false, false, false);
    insert("story", false, true, false);
    insert("viewed", false, false, true);

    state.kill_non_persisting();

    let mut killed: Vec<String> = sink
        .snapshot()
        .into_iter()
        .filter_map(|event| match event {
            TerminalDomainEvent::SessionRemoved {
                session,
                reason: TeardownReason::Kill,
            } => Some(session.as_str().to_string()),
            _ => None,
        })
        .collect();
    killed.sort();
    assert_eq!(
        killed,
        vec!["__warm_pool_1".to_string(), "orphan".to_string()],
        "the pool member and the orphan are reaped by the one exit-time path"
    );
    assert!(state.has_session("story"));
    assert!(state.has_session("viewed"));
    state.kill("story");
    state.kill("viewed");
}

/// A client's exit reap takes only the non-persisting sessions it opened: a
/// non-persisting session opened by another client, or claimed by none, and a
/// persisting one of its own all run on. As the last client it takes every
/// warm-pool member and every leftover unviewed past the grace too.
#[test]
fn kill_owned_non_persisting_reaps_only_the_owned() {
    let (sink, state) = recording_state();
    let insert = |id: &str, pool: bool, story: bool| {
        let mut s = make_test_session(id, true, false);
        s.runtime.is_pool_member.store(pool, Ordering::Relaxed);
        s.story_id = story.then(|| "SW-1".to_string());
        state.sessions.lock().unwrap().insert(id.to_string(), s);
    };
    insert("__warm_pool_mine", true, false);
    insert("mine", false, false);
    insert("mine-story", false, true);
    insert("__warm_pool_theirs", true, false);
    insert("theirs", false, false);
    insert("unclaimed", false, false);

    let owned: HashSet<String> = ["__warm_pool_mine", "mine", "mine-story"]
        .into_iter()
        .map(String::from)
        .collect();
    state.kill_owned_non_persisting(&owned, false, None);

    let mut killed: Vec<String> = sink
        .snapshot()
        .into_iter()
        .filter_map(|event| match event {
            TerminalDomainEvent::SessionRemoved {
                session,
                reason: TeardownReason::Kill,
            } => Some(session.as_str().to_string()),
            _ => None,
        })
        .collect();
    killed.sort();
    assert_eq!(killed, ["__warm_pool_mine", "mine"]);
    for id in ["mine-story", "__warm_pool_theirs", "theirs", "unclaimed"] {
        assert_eq!(state.alive_session_ids(&[id]).len(), 1, "{id} runs on");
    }

    // The last client also takes every unhanded warm-pool member, an idle
    // shell its owner refills, and a leftover no viewport has shown for
    // longer than the grace; another app's tab unviewed only briefly (its
    // app may be reconnecting) runs on.
    let grace = Duration::from_secs(300);
    let base = std::time::Instant::now();
    let unviewed_since = |id: &str, since: std::time::Instant| {
        let shared = state.shared_handle(id).unwrap();
        shared.lock().unwrap().unviewed_since = Some(since);
    };
    unviewed_since("unclaimed", base);
    unviewed_since("theirs", base + Duration::from_secs(120));
    // Pool members only: nothing else of another app goes.
    state.kill_owned_non_persisting(&HashSet::new(), true, None);
    assert!(!state.has_session("__warm_pool_theirs"));
    assert!(state.has_session("unclaimed"));
    // At `base` + grace + 1 s: `unclaimed` is past the grace, `theirs` not.
    let reap = crate::LeftoverReap {
        now: base + grace + Duration::from_secs(1),
        grace,
    };
    state.kill_owned_non_persisting(&HashSet::new(), true, Some(reap));
    assert!(!state.has_session("unclaimed"), "a leftover past the grace");
    for id in ["mine-story", "theirs"] {
        assert_eq!(state.alive_session_ids(&[id]).len(), 1, "{id} runs on");
        state.kill(id);
    }
}

/// A session's unviewed time starts when its last viewport detaches, not
/// while one is attached.
#[test]
fn the_last_viewport_detaching_starts_the_unviewed_time() {
    let (_sink, state) = recording_state();
    crate::emulator::attach_echo(&state.emulator);
    spawn(&state, sh("cat"), registration("viewed")).expect("spawn");
    let shared = state.shared_handle("viewed").unwrap();
    assert_eq!(shared.lock().unwrap().unviewed_since, None);
    let first = state.attach_viewport("viewed", &shared).unwrap();
    let second = state.attach_viewport("viewed", &shared).unwrap();
    state.unsubscribe(first.stream_id).unwrap();
    assert_eq!(
        shared.lock().unwrap().unviewed_since,
        None,
        "one still shows it"
    );
    state.unsubscribe(second.stream_id).unwrap();
    assert!(shared.lock().unwrap().unviewed_since.is_some());
    state.kill("viewed");
}

/// A warm member handed off is re-keyed to the frontend id, gated until
/// its prompt (the 1 s timeout guard flushes when no OSC 133 marker comes),
/// announced live for its target, and — no longer a pool member — follows the
/// normal retain path.
#[test]
fn warm_handoff_rekeys_gates_and_flushes_the_member() {
    let (sink, state) = recording_state();
    let mut member = registration("__warm_pool_9");
    member.is_pool_member = true;
    spawn(&state, sh("cat"), member).expect("spawn");
    let target = std::env::temp_dir();
    let target_cwd = target.to_str().expect("utf-8 temp dir");

    state
        .handoff_warm_member(
            "__warm_pool_9",
            "shell-handed",
            target_cwd,
            100,
            30,
            SessionMeta {
                story_id: None,
                workspace_id: Some("ws".into()),
                epic_id: None,
            },
        )
        .expect("handoff");
    assert!(!state.has_session("__warm_pool_9"));
    let view = state.session_view("shell-handed").expect("re-keyed");
    assert_eq!(view.runtime.current_id(), "shell-handed");
    assert!(!view.runtime.is_pool_member.load(Ordering::Relaxed));
    assert_eq!(view.workspace_id.as_deref(), Some("ws"));
    assert!(sink
        .snapshot()
        .contains(&TerminalDomainEvent::WorktreeLive {
            cwd: target.clone()
        }));

    // `cat` echoes the handoff line with no prompt marker: the guard flushes it.
    let flushed = sink.wait_for(Duration::from_secs(5), |events| {
        events.iter().find_map(|event| match event {
            TerminalDomainEvent::Output { session, data, .. }
                if session.as_str() == "shell-handed" && data.contains("CODESENSE_TAB") =>
            {
                Some(data.clone())
            }
            _ => None,
        })
    });
    assert!(flushed.contains("export CODESENSE_TAB_SESSION_ID='shell-handed'"));
    assert!(!view.runtime.suppress_until_prompt.load(Ordering::Relaxed));
    state.kill("shell-handed");
}

/// A pool member's natural exit is reported as such, so the host reclaims and
/// refills the pool rather than retaining it for replay.
#[test]
fn pool_member_exit_is_reported_as_a_pool_member() {
    let (sink, state) = recording_state();
    let mut member = registration("__warm_pool_7");
    member.is_pool_member = true;
    spawn(&state, sh("exit 0"), member).expect("spawn");
    let (_, pool_member) = sink.wait_for(Duration::from_secs(5), |events| {
        exit_of(events, "__warm_pool_7")
    });
    assert!(pool_member);
    state.kill("__warm_pool_7");
}

/// The batcher emits one `CwdChanged` event per real OSC 7 cwd
/// mutation and dedupes path-equal reports — a shell that re-emits the same
/// cwd never bumps the frontend revision counter. Three OSC 7 writes with
/// paths `/a`, `/a`, `/b` yield exactly two events (`/a`, then `/b`).
#[test]
fn osc7_cwd_changes_emit_cwd_changed_once_per_real_change() {
    let (sink, state) = recording_state();
    let mut reg = registration("osc7-cwd");
    reg.osc_enabled = true;
    // Three OSC 7 reports: path /tmp/a repeated, then switching to /tmp/b.
    // The parser is byte-exact so the printf escapes drive it directly.
    spawn(
        &state,
        sh("printf '\\x1b]7;file:///tmp/a\\x07\\x1b]7;file:///tmp/a\\x07\\x1b]7;file:///tmp/b\\x07'; exit 0"),
        reg,
    )
    .expect("spawn");

    // Wait for the natural exit so the batcher has drained every byte.
    sink.wait_for(Duration::from_secs(5), |events| exit_of(events, "osc7-cwd"));

    let events = sink.snapshot();
    let cwd_events: Vec<PathBuf> = events
        .iter()
        .filter_map(|event| match event {
            TerminalDomainEvent::CwdChanged { session, cwd } if session.as_str() == "osc7-cwd" => {
                Some(cwd.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        cwd_events,
        vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")],
        "dedupe keeps one event per real cwd change (actual events: {cwd_events:?})",
    );
    // The stored cwd lands on the latest reported path.
    assert_eq!(state.session_cwd("osc7-cwd"), Some(PathBuf::from("/tmp/b")));
    state.kill("osc7-cwd");
}

/// A deliberate teardown of a live pool member — here the exit-time complement
/// kill — is not the member's unexpected death: its closing stream reports a
/// plain exit, so the host never reclaims-and-refills the pool while quitting.
#[test]
fn deliberately_killed_pool_member_exit_does_not_ask_for_a_refill() {
    let (sink, state) = recording_state();
    let mut member = registration("__warm_pool_3");
    member.is_pool_member = true;
    spawn(&state, sh("sleep 30"), member).expect("spawn");

    state.kill_non_persisting();

    let (_, pool_member) = sink.wait_for(Duration::from_secs(5), |events| {
        exit_of(events, "__warm_pool_3")
    });
    assert!(
        !pool_member,
        "a killed member's exit must not trigger the pool's self-heal refill"
    );
    assert!(!state.has_session("__warm_pool_3"));
}

/// A handoff into an id that is already registered retires the displaced
/// session (killed + reaped), exactly like a same-id respawn.
#[cfg(target_os = "macos")]
#[test]
fn warm_handoff_retires_a_displaced_same_id_session() {
    use crate::fixtures::{stable_process_identity, wait_for_process_exit};

    let (_sink, state) = recording_state();
    let mut member = registration("__warm_pool_5");
    member.is_pool_member = true;
    spawn(&state, sh("cat"), member).expect("spawn");
    let old = make_test_session("shell-taken", true, false);
    let old_pid = old.pty.pid().expect("pid") as libc::pid_t;
    let old_pgid = unsafe { libc::getpgid(old_pid) };
    let old_identity = stable_process_identity(old_pid).expect("identity");
    state
        .sessions
        .lock()
        .unwrap()
        .insert("shell-taken".to_string(), old);
    let target = std::env::temp_dir();

    state
        .handoff_warm_member(
            "__warm_pool_5",
            "shell-taken",
            target.to_str().unwrap(),
            80,
            24,
            SessionMeta::default(),
        )
        .expect("handoff");

    wait_for_process_exit(
        old_pid,
        Some(old_pgid),
        old_identity,
        Duration::from_secs(2),
    )
    .expect("the displaced session is killed and reaped");
    state.kill("shell-taken");
}

/// A failed handoff retires the popped member itself (nothing is left for the
/// caller to reclaim), and its exit is reported under the member's own id —
/// never under the frontend id the caller cold-spawns next.
#[test]
fn failed_warm_handoff_retires_the_member_under_its_own_id() {
    let (sink, state) = recording_state();
    let mut member = registration("__warm_pool_8");
    member.is_pool_member = true;
    spawn(&state, sh("sleep 30"), member).expect("spawn");
    // Poison the member's replay lock so the handoff fails after the rename.
    let shared = state.shared_handle("__warm_pool_8").unwrap();
    let poison = shared.clone();
    let _ = std::thread::spawn(move || {
        let _guard = poison.lock().unwrap();
        panic!("poison the replay lock");
    })
    .join();

    let error = state
        .handoff_warm_member(
            "__warm_pool_8",
            "shell-fresh",
            std::env::temp_dir().to_str().unwrap(),
            80,
            24,
            SessionMeta::default(),
        )
        .expect_err("the handoff fails on the poisoned replay lock");
    assert!(error.contains("Lock error"), "was: {error}");
    assert!(!state.has_session("__warm_pool_8"));
    assert!(!state.has_session("shell-fresh"));

    sink.wait_for(Duration::from_secs(5), |events| {
        exit_of(events, "__warm_pool_8")
    });
    assert!(
        exit_of(&sink.snapshot(), "shell-fresh").is_none(),
        "the retired member never reports an exit under the frontend id"
    );
}

/// RF-perf-5: the counting probe on an empty registry reports no
/// sessions and leaves the table intact (idempotent).
#[test]
fn empty_pty_state_reports_no_sessions_idempotently() {
    let state = fixture_state(Arc::new(NoopSink));
    assert_eq!(state.session_len_and_dead(), Some((0, 0)));
    assert_eq!(state.session_len_and_dead(), Some((0, 0)));
}

/// A viewport stream the host fails to deliver to is counted as a failed
/// emit in its telemetry, never as bytes sent; the session channel still gets
/// the batch.
#[test]
fn failed_stream_delivery_counts_as_an_emit_failure() {
    struct RejectStreams;
    impl TerminalEventSink for RejectStreams {
        fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
            match event {
                TerminalDomainEvent::StreamOutput { .. } => {
                    Err(PortUnavailable("stream closed".into()))
                }
                _ => Ok(()),
            }
        }
    }
    let mut subscribers = HashMap::new();
    subscribers.insert(StreamId(1), SubscriberHandle::default());
    crate::state::deliver_output(&RejectStreams, &mut subscribers, "s", "bytes", 5);
    let handle = &subscribers[&StreamId(1)];
    assert_eq!(handle.emit_failures, 1);
    assert_eq!(handle.bytes_sent, 0);
}

/// An exit from a host that predates the `requested` flag reads as not
/// requested.
#[test]
fn an_exit_without_the_requested_flag_reads_as_not_requested() {
    let exit = TerminalDomainEvent::Exit {
        session: PtySessionId::new("s1"),
        status: None,
        pool_member: false,
        requested: true,
        incarnation: 0,
    };
    let mut json = serde_json::to_value(&exit).unwrap();
    let fields = json
        .as_object_mut()
        .and_then(|o| o.values_mut().next())
        .and_then(|v| v.as_object_mut())
        .expect("an externally tagged variant");
    assert!(fields.remove("requested").is_some());
    let older: TerminalDomainEvent = serde_json::from_value(json).unwrap();
    assert!(matches!(
        older,
        TerminalDomainEvent::Exit {
            requested: false,
            ..
        }
    ));
}

/// Every `Exit` recorded for `id`, by status.
fn exits_of(events: &[TerminalDomainEvent], id: &str) -> Vec<Option<ExitInfo>> {
    events
        .iter()
        .filter_map(|event| match event {
            TerminalDomainEvent::Exit {
                session, status, ..
            } if session.as_str() == id => Some(status.clone()),
            _ => None,
        })
        .collect()
}

/// A write into a retained session whose child is gone fails, and on a host
/// whose clients can miss events emits the session's exit again with its real
/// status, every time: a client that missed the original learns the session
/// ended, and one that has it drops the repeat. An in-process host, whose one
/// client hears every event, does not repeat it.
#[test]
fn a_write_into_an_exited_session_emits_its_exit_again() {
    let (sink, state) = recording_state();
    state.redeliver_exits_on_use();
    spawn(&state, sh("exit 3"), registration("gone")).expect("spawn");
    sink.wait_for(Duration::from_secs(5), |events| exit_of(events, "gone"));
    let exited = Some(ExitInfo {
        exit_code: Some(3),
        signal: None,
    });
    assert!(state.has_session("gone"), "retained after its exit");

    assert!(state.write("gone", b"lost keystroke").is_err());
    assert!(state.write("gone", b"another").is_err());
    let exits = sink.wait_for(Duration::from_secs(5), |events| {
        let exits = exits_of(events, "gone");
        (exits.len() == 3).then_some(exits)
    });
    assert_eq!(exits, vec![exited; 3]);
    state.kill("gone");

    let (sink, in_process) = recording_state();
    spawn(&in_process, sh("exit 3"), registration("local")).expect("spawn");
    sink.wait_for(Duration::from_secs(5), |events| exit_of(events, "local"));
    assert!(in_process.write("local", b"x").is_err());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(exits_of(&sink.snapshot(), "local").len(), 1);
    in_process.kill("local");
}

/// Every viewport attached to a retained, exited session is told the session
/// ended: its exit goes out again on each attach, so a client that attaches
/// later (a reload, another app) learns it too.
#[test]
fn attaching_to_an_exited_session_emits_its_exit_again() {
    let (sink, state) = recording_state();
    state.redeliver_exits_on_use();
    crate::emulator::attach_echo(&state.emulator);
    spawn(&state, sh("exit 4"), registration("ended")).expect("spawn");
    sink.wait_for(Duration::from_secs(5), |events| exit_of(events, "ended"));

    let shared = state.shared_handle("ended").unwrap();
    let attach = state.attach_viewport("ended", &shared).unwrap();
    state.unsubscribe(attach.stream_id).unwrap();
    let again = state.attach_viewport("ended", &shared).unwrap();
    assert_eq!(
        exits_of(&sink.snapshot(), "ended"),
        vec![
            Some(ExitInfo {
                exit_code: Some(4),
                signal: None,
            });
            3
        ]
    );
    state.unsubscribe(again.stream_id).unwrap();
    state.kill("ended");
}

/// An exited session's exit is never delivered onto the process that takes
/// its id next: not after a respawn reaped it, nor after a spawn displaced it,
/// nor once its removal was announced.
#[test]
fn a_redelivered_exit_never_lands_on_the_ids_next_process() {
    let (sink, state) = recording_state();
    state.redeliver_exits_on_use();
    crate::emulator::attach_echo(&state.emulator);
    for id in ["respawned", "displaced"] {
        spawn(&state, sh("exit 6"), registration(id)).expect("spawn");
        sink.wait_for(Duration::from_secs(5), |events| exit_of(events, id));
    }
    let old = state
        .sessions
        .lock()
        .unwrap()
        .get("respawned")
        .unwrap()
        .runtime
        .clone();
    assert_eq!(
        state.reap_ended("respawned", false),
        ReapOutcome::Reaped,
        "the respawn reaps the ended session"
    );
    assert!(
        !old.redeliver_exit(|| true, |_| panic!("redelivered after its removal")),
        "a removed session's exit is retired"
    );
    spawn(&state, sh("cat"), registration("respawned")).expect("respawn");
    spawn(&state, sh("cat"), registration("displaced")).expect("displacing spawn");

    assert!(state
        .redeliver_exits(&["respawned", "displaced"])
        .is_empty());
    for id in ["respawned", "displaced"] {
        state.write(id, b"alive\n").unwrap();
        let shared = state.shared_handle(id).unwrap();
        let attach = state.attach_viewport(id, &shared).unwrap();
        state.unsubscribe(attach.stream_id).unwrap();
    }
    std::thread::sleep(Duration::from_millis(100));
    for id in ["respawned", "displaced"] {
        assert_eq!(exits_of(&sink.snapshot(), id).len(), 1, "{id}");
        assert!(state.alive_session_ids(&[id]).contains(id));
        state.kill(id);
    }
}

/// A running session is never reported ended: a write, an attach and an asked
/// redelivery leave it alone, while a retained exited one has its exit emitted
/// again with its status, and a missing one is skipped.
#[test]
fn a_running_session_is_never_reported_ended() {
    let (sink, state) = recording_state();
    crate::emulator::attach_echo(&state.emulator);
    spawn(&state, sh("cat"), registration("running")).expect("spawn");
    spawn(&state, sh("exit 5"), registration("exited")).expect("spawn");
    sink.wait_for(Duration::from_secs(5), |events| exit_of(events, "exited"));

    state.write("running", b"still here\n").unwrap();
    let shared = state.shared_handle("running").unwrap();
    let attach = state.attach_viewport("running", &shared).unwrap();
    assert_eq!(
        state.redeliver_exits(&["running", "exited", "missing"]),
        HashSet::from(["exited".to_string()])
    );
    assert_eq!(
        exits_of(&sink.snapshot(), "exited"),
        vec![
            Some(ExitInfo {
                exit_code: Some(5),
                signal: None
            });
            2
        ]
    );
    std::thread::sleep(Duration::from_millis(100));
    assert!(exits_of(&sink.snapshot(), "running").is_empty());
    assert!(state.alive_session_ids(&["running"]).contains("running"));
    state.unsubscribe(attach.stream_id).unwrap();
    state.kill("running");
    state.kill("exited");
}

/// A program printing an invalid UTF-8 byte keeps streaming: the subscriber
/// sees the byte as U+FFFD followed by everything after it, within a bounded
/// wait, and the replay ring holds exactly what the subscriber was sent. The
/// child waits for a line before printing (with echo off) so the viewport is
/// attached before the first byte, and stays alive afterwards so this pins the
/// live path, not the end-of-stream flush.
#[test]
fn invalid_utf8_does_not_stall_session() {
    let (sink, state) = recording_state();
    crate::emulator::attach_echo(&state.emulator);
    spawn(
        &state,
        sh("stty -echo; read _; printf 'ok\\377after\\nDONE\\n'; sleep 30"),
        registration("bad-utf8"),
    )
    .expect("spawn");
    let shared = state.shared_handle("bad-utf8").unwrap();
    let attach = state.attach_viewport("bad-utf8", &shared).unwrap();
    // Give `stty -echo` time to land before the line is written, so the
    // newline is not echoed into the stream.
    std::thread::sleep(Duration::from_millis(200));
    state.write("bad-utf8", b"\n").unwrap();

    let expected = "ok\u{FFFD}after\r\nDONE\r\n";
    let streamed = sink.wait_for(Duration::from_secs(2), |events| {
        let data: String = events
            .iter()
            .filter_map(|event| match event {
                TerminalDomainEvent::StreamOutput { stream, data, .. }
                    if *stream == attach.stream_id =>
                {
                    Some(data.as_str())
                }
                _ => None,
            })
            .collect();
        data.contains("DONE\r\n").then_some(data)
    });
    assert_eq!(streamed, expected);
    assert_eq!(
        shared.lock().unwrap().seq,
        streamed.len() as u64,
        "the byte count is what was streamed"
    );
    state.kill("bad-utf8");
}

/// The dev recorder writes a resize between the output lines of the batches it
/// fell between, at the byte offset the stream stood at when it was applied.
#[cfg(debug_assertions)]
#[test]
fn a_recorded_resize_lands_between_the_batches_around_it() {
    use base64::Engine as _;

    let (sink, state) = recording_state();
    spawn(&state, sh("cat"), registration("rec")).expect("spawn");
    let dir = tempfile::tempdir().unwrap();
    state.shared_handle("rec").unwrap().lock().unwrap().recorder =
        Some(crate::recorder::PtyRecorder::create(dir.path(), "rec").unwrap());
    let wait_output = |needle: &str| {
        sink.wait_for(Duration::from_secs(5), |events| {
            events
                .iter()
                .any(|event| {
                    matches!(event, TerminalDomainEvent::Output { data, .. } if data.contains(needle))
                })
                .then_some(())
        })
    };

    state.write("rec", b"first\n").unwrap();
    wait_output("first");
    state.resize("rec", 100, 30).unwrap();
    state.write("rec", b"second\n").unwrap();
    wait_output("second");

    let log = std::fs::read_to_string(dir.path().join("rec.ptylog")).unwrap();
    let lines: Vec<Vec<&str>> = log.lines().map(|line| line.split('\t').collect()).collect();
    let at = lines
        .iter()
        .position(|fields| fields[0] == "R")
        .expect("a resize line");
    assert_eq!(lines[at][2..], ["100", "30"]);
    let at_seq: u64 = lines[at][1].parse().unwrap();
    let decode = |fields: &Vec<&str>| {
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(fields[2])
                .unwrap(),
        )
        .unwrap()
    };
    let (before, after) = lines.split_at(at);
    let before: Vec<_> = before.iter().filter(|f| f[0] == "O").collect();
    let after: Vec<_> = after[1..].iter().filter(|f| f[0] == "O").collect();
    assert!(before
        .iter()
        .map(|f| decode(f))
        .collect::<String>()
        .contains("first"));
    assert!(after
        .iter()
        .map(|f| decode(f))
        .collect::<String>()
        .contains("second"));
    assert!(!before
        .iter()
        .map(|f| decode(f))
        .collect::<String>()
        .contains("second"));
    assert_eq!(
        before.last().unwrap()[1].parse::<u64>().unwrap(),
        at_seq,
        "the resize sits at the end of the last batch before it"
    );
    assert!(after.iter().all(|f| f[1].parse::<u64>().unwrap() > at_seq));
    state.kill("rec");
}

/// The worker's key for `session`'s terminal.
fn emulator_key(state: &PtyState, session: &str) -> u64 {
    state
        .shared_handle(session)
        .unwrap()
        .lock()
        .unwrap()
        .emulator
        .as_ref()
        .expect("an emulator terminal")
        .key()
}

/// The requests about the terminal `key`.
fn requests_for(requests: &[serde_json::Value], key: u64) -> Vec<serde_json::Value> {
    requests
        .iter()
        .filter(|r| r["session"] == key)
        .cloned()
        .collect()
}

fn fed_data(requests: &[serde_json::Value]) -> String {
    requests
        .iter()
        .filter(|r| r["type"] == "feed")
        .map(|r| r["data"].as_str().unwrap())
        .collect()
}

#[test]
fn resize_gen_counts_applied_resizes() {
    let (_sink, state) = recording_state();
    let worker = FakeWorker::attach(&state);
    spawn(&state, sh("sleep 30"), registration("rz")).expect("spawn");
    let key = emulator_key(&state, "rz");
    state.resize("rz", 100, 30).unwrap();
    state.resize("rz", 120, 40).unwrap();
    // The current size: nothing is sent and the count does not move.
    state.resize("rz", 120, 40).unwrap();
    state.resize("rz", 121, 40).unwrap();

    let resizes = |requests: &[serde_json::Value]| -> Vec<(u64, u64, u64)> {
        requests_for(requests, key)
            .iter()
            .filter(|r| r["type"] == "resize")
            .map(|r| {
                (
                    r["cols"].as_u64().unwrap(),
                    r["rows"].as_u64().unwrap(),
                    r["resize_gen"].as_u64().unwrap(),
                )
            })
            .collect()
    };
    let requests = worker.wait_for(Duration::from_secs(5), |r| resizes(r).len() >= 3);
    assert_eq!(
        resizes(&requests),
        [(100, 30, 1), (120, 40, 2), (121, 40, 3)]
    );
    let first = &requests_for(&requests, key)[0];
    assert_eq!(
        first["type"], "create",
        "the terminal exists before its resizes"
    );
    assert_eq!(
        (first["cols"].as_u64(), first["rows"].as_u64()),
        (Some(80), Some(24))
    );
    state.kill("rz");
}

#[test]
fn respawn_orders_seam_before_new_output() {
    let (sink, state) = recording_state();
    let worker = FakeWorker::attach(&state);
    spawn(&state, sh("printf 'old output'"), registration("rs")).expect("spawn");
    let key = emulator_key(&state, "rs");
    sink.wait_for(Duration::from_secs(5), |events| exit_of(events, "rs"));
    assert_eq!(state.reap_ended("rs", false), ReapOutcome::Reaped);
    spawn(&state, sh("printf NEW; sleep 30"), registration("rs")).expect("respawn");
    assert_eq!(
        emulator_key(&state, "rs"),
        key,
        "the respawn keeps the terminal"
    );

    let requests = worker.wait_for(Duration::from_secs(5), |r| {
        fed_data(&requests_for(r, key)).contains("NEW")
    });
    let kinds: Vec<String> = requests_for(&requests, key)
        .iter()
        .map(|r| {
            let kind = r["type"].as_str().unwrap();
            match r["data"].as_str() {
                Some(data) if data.contains("old") => "feed-old".into(),
                Some(data) if data.contains("NEW") => "feed-new".into(),
                _ => kind.into(),
            }
        })
        .collect();
    assert_eq!(kinds, ["create", "feed-old", "respawn", "feed-new"]);
    let respawn = requests_for(&requests, key)
        .into_iter()
        .find(|r| r["type"] == "respawn")
        .unwrap();
    // No raw output is carried: the new session's stream starts at zero, and
    // the worker carries the old terminal itself.
    assert_eq!(respawn["seq_base"].as_u64(), Some(0));
    state.kill("rs");
}

/// Hand a pool member (`cat`, echo off) off, with the gate suppressing the
/// echoed `cd` line until a prompt-start; subscribe while it is suppressed;
/// then print a prompt. What the emulator got must be what the displays got.
fn check_warm_echo_not_fed(feed_full_batch: bool) -> Result<(), String> {
    let (sink, state) = recording_state();
    let worker = FakeWorker::attach(&state);
    let mut member = registration("__warm_pool_e");
    member.is_pool_member = true;
    member.osc_enabled = true;
    spawn(&state, sh("stty -echo; cat"), member).expect("spawn");
    std::thread::sleep(Duration::from_millis(200));
    let target = std::env::temp_dir();
    state
        .handoff_warm_member(
            "__warm_pool_e",
            "warm-e",
            target.to_str().unwrap(),
            100,
            30,
            SessionMeta::default(),
        )
        .expect("handoff");
    let shared = state.shared_handle("warm-e").unwrap();
    let view = state.session_view("warm-e").unwrap();
    view.runtime
        .feed_emulator_full_batch
        .store(feed_full_batch, Ordering::Relaxed);
    let key = emulator_key(&state, "warm-e");
    // The echo of the handoff line is held back by the gate.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !shared.lock().unwrap().held.contains("CODESENSE_TAB") {
        assert!(std::time::Instant::now() < deadline, "no echo");
        std::thread::sleep(Duration::from_millis(5));
    }
    let attach = state.attach_viewport("warm-e", &shared).unwrap();
    state.write("warm-e", b"\x1b]133;A\x07$ ready\n").unwrap();

    let first_output = sink.wait_for(Duration::from_secs(5), |events| {
        events.iter().find_map(|event| match event {
            TerminalDomainEvent::StreamOutput { stream, seq, data }
                if *stream == attach.stream_id =>
            {
                Some((*seq, data.clone()))
            }
            _ => None,
        })
    });
    let requests = worker.wait_for(Duration::from_secs(5), |r| {
        fed_data(&requests_for(r, key)).contains("ready")
    });
    let ours = requests_for(&requests, key);
    let full_seq = shared.lock().unwrap().seq;
    state.kill("warm-e");

    if first_output.0 <= attach.snapshot.seq {
        return Err("the subscriber's first output is not past its subscribe point".into());
    }
    let fed = fed_data(&ours);
    if fed.contains("CODESENSE_TAB") {
        return Err(format!("the suppressed echo reached the emulator: {fed:?}"));
    }
    if fed != first_output.1 {
        return Err(format!(
            "emulator got {fed:?}, displays got {:?}",
            first_output.1
        ));
    }
    let feed = ours.iter().rev().find(|r| r["type"] == "feed").unwrap();
    if feed["seq_end"].as_u64() != Some(full_seq) {
        return Err(format!(
            "feed seq_end {} does not count the full batch ({full_seq})",
            feed["seq_end"]
        ));
    }
    Ok(())
}

#[test]
fn warm_pool_echo_not_fed_to_emulator() {
    check_warm_echo_not_fed(false).unwrap();
}

/// Negative control: feeding the whole batch, suppressed echo included, is
/// caught.
#[test]
fn warm_pool_echo_check_catches_a_full_batch_feed() {
    let error = check_warm_echo_not_fed(true).unwrap_err();
    assert!(error.contains("suppressed echo"), "{error}");
}

#[test]
fn subscribers_unaffected_by_worker_stall_or_crash() {
    let (sink, state) = recording_state();
    let worker = FakeWorker::attach(&state);
    spawn(&state, sh("stty -echo; cat"), registration("st")).expect("spawn");
    let key = emulator_key(&state, "st");
    let shared = state.shared_handle("st").unwrap();
    let attach = state.attach_viewport("st", &shared).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let streamed = |events: &[TerminalDomainEvent]| -> String {
        events
            .iter()
            .filter_map(|event| match event {
                TerminalDomainEvent::StreamOutput { stream, data, .. }
                    if *stream == attach.stream_id =>
                {
                    Some(data.as_str())
                }
                _ => None,
            })
            .collect()
    };
    let expect_lines = |from: usize, to: usize| {
        (from..to)
            .map(|i| format!("line {i}\r\n"))
            .collect::<String>()
    };

    // A worker that stopped reading: every line still streams, in order.
    worker.stall(true);
    for i in 0..200 {
        state.write("st", format!("line {i}\n").as_bytes()).unwrap();
    }
    let before = sink.wait_for(Duration::from_secs(5), |events| {
        let data = streamed(events);
        data.contains("line 199\r\n").then_some(data)
    });
    assert_eq!(before, expect_lines(0, 200));
    assert!(
        state.emulator_lag_bytes("st").unwrap() > 0,
        "the stall shows as lag"
    );

    // The worker dies and a new one takes its place.
    state.emulator.detach(worker.generation);
    worker.stall(false);
    let restarted = FakeWorker::attach(&state);
    for i in 200..210 {
        state.write("st", format!("line {i}\n").as_bytes()).unwrap();
    }
    let after = sink.wait_for(Duration::from_secs(5), |events| {
        let data = streamed(events);
        data.contains("line 209\r\n").then_some(data)
    });
    assert_eq!(after, expect_lines(0, 210));

    let requests = restarted.wait_for(Duration::from_secs(5), |r| {
        fed_data(&requests_for(r, key)).contains("line 209")
    });
    let ours = requests_for(&requests, key);
    let create = &ours[0];
    assert_eq!(
        create["type"], "create",
        "re-created before any feed: {ours:#?}"
    );
    assert_eq!(create["history_lost"], true);
    let first_feed = &ours[1];
    let fed_len = first_feed["data"].as_str().unwrap().len() as u64;
    assert_eq!(
        create["seq_base"].as_u64().unwrap() + fed_len,
        first_feed["seq_end"].as_u64().unwrap(),
        "the new terminal starts where its first feed does"
    );
    assert_eq!(
        create["seq_base"].as_u64().unwrap(),
        expect_lines(0, 200).len() as u64
    );
    assert!(
        !fed_data(&ours).contains("line 199"),
        "nothing of the old worker's is replayed"
    );
    state.kill("st");
}

/// Worst-case lock hold of the enqueue: 64 KiB batches under the session lock.
/// Run in a release build: `cargo test --release -p codesense-pty-core
/// lock_hold_enqueue -- --ignored --nocapture`.
#[test]
#[ignore = "budget measurement; release build"]
fn lock_hold_enqueue() {
    let (_sink, state) = recording_state();
    let _worker = FakeWorker::attach_discarding(&state);
    let shared = Mutex::new(crate::term_state::SharedTermState::default());
    shared.lock().unwrap().emulator = Some(crate::emulator::SessionEmulator::start(
        &state.emulator,
        (120, 40),
        crate::emulator::DEFAULT_SCROLLBACK,
        0,
    ));
    let batch = "\x1b[32mcodex\x1b[0m output line with some width to it\r\n".repeat(1400)
        [..64 * 1024]
        .to_string();
    let mut samples = Vec::with_capacity(2000);
    for _ in 0..2000 {
        let started = std::time::Instant::now();
        {
            let mut g = shared.lock().unwrap();
            g.seq += batch.len() as u64;
            let seq = g.seq;
            g.emulator.as_mut().unwrap().feed(seq, &batch);
        }
        samples.push(started.elapsed());
        // Paced like a real session so the writer drains between batches.
        std::thread::sleep(Duration::from_micros(200));
    }
    samples.sort();
    let pct = |p: f64| samples[((samples.len() as f64 * p) as usize).min(samples.len() - 1)];
    println!(
        "| lock hold, 64 KiB enqueue | n={} | p50 {:?} | p99 {:?} | max {:?} |",
        samples.len(),
        pct(0.5),
        pct(0.99),
        samples[samples.len() - 1]
    );
    assert!(
        pct(0.99) <= Duration::from_millis(1),
        "p99 {:?} > 1 ms",
        pct(0.99)
    );
}

#[test]
fn a_dump_is_answered_by_its_reply_and_fails_when_the_worker_restarts() {
    let (_sink, state) = recording_state();
    let worker = FakeWorker::attach(&state);
    spawn(&state, sh("sleep 30"), registration("dq")).expect("spawn");
    let key = emulator_key(&state, "dq");
    let state = Arc::new(state);

    let asking = {
        let state = state.clone();
        std::thread::spawn(move || state.emulator_dump("dq", Duration::from_secs(5)))
    };
    let requests = worker.wait_for(Duration::from_secs(5), |r| {
        r.iter().any(|r| r["type"] == "dump" && r["session"] == key)
    });
    let id = requests.iter().find(|r| r["type"] == "dump").unwrap()["id"].clone();
    worker.reply(serde_json::json!({
        "type": "dump", "id": id, "session": key, "seq_end": 0, "resize_gen": 0,
        "history_lost": false, "cols": 80, "lines": [],
    }));
    let dump = asking.join().unwrap().expect("answered");
    assert_eq!(dump.buffer["cols"], 80);

    // A dump still waiting when its worker is replaced is not left hanging.
    let asking = {
        let state = state.clone();
        std::thread::spawn(move || state.emulator_dump("dq", Duration::from_secs(5)))
    };
    worker.wait_for(Duration::from_secs(5), |r| {
        r.iter().filter(|r| r["type"] == "dump").count() == 2
    });
    state.emulator.detach(worker.generation);
    let error = asking.join().unwrap().unwrap_err();
    assert!(error.contains("restarted"), "{error}");
    state.kill("dq");
}

/// Each attached stream's output and resizes, in emission order.
#[derive(Debug, Clone, PartialEq)]
enum StreamItem {
    Output { seq_end: u64, data: String },
    Resized { at_seq: u64, resize_gen: u32 },
}

fn stream_items(events: &[TerminalDomainEvent], stream: StreamId) -> Vec<StreamItem> {
    events
        .iter()
        .filter_map(|event| match event {
            TerminalDomainEvent::StreamOutput {
                stream: s,
                data,
                seq,
            } if *s == stream => Some(StreamItem::Output {
                seq_end: *seq,
                data: data.clone(),
            }),
            TerminalDomainEvent::StreamResized {
                stream: s,
                at_seq,
                resize_gen,
                ..
            } if *s == stream => Some(StreamItem::Resized {
                at_seq: *at_seq,
                resize_gen: *resize_gen,
            }),
            _ => None,
        })
        .collect()
}

/// A resize between two output batches reaches every subscriber between
/// them, at the first batch's end, counting one size change; a resize to the
/// current size sends nothing.
#[test]
fn resize_emits_ordered_resized() {
    let (sink, state) = recording_state();
    crate::emulator::attach_echo(&state.emulator);
    spawn(&state, sh("stty -echo; cat"), registration("rsz")).expect("spawn");
    let shared = state.shared_handle("rsz").unwrap();
    let a = state.attach_viewport("rsz", &shared).unwrap().stream_id;
    let b = state.attach_viewport("rsz", &shared).unwrap().stream_id;
    std::thread::sleep(Duration::from_millis(200));
    state.write("rsz", b"one\n").unwrap();
    sink.wait_for(Duration::from_secs(5), |events| {
        stream_items(events, a)
            .iter()
            .any(|item| matches!(item, StreamItem::Output { data, .. } if data.contains("one")))
            .then_some(())
    });
    state.resize("rsz", 100, 30).unwrap();
    state.resize("rsz", 100, 30).unwrap();
    state.write("rsz", b"two\n").unwrap();
    for stream in [a, b] {
        let items = sink.wait_for(Duration::from_secs(5), |events| {
            let items = stream_items(events, stream);
            items
                .iter()
                .any(|item| matches!(item, StreamItem::Output { data, .. } if data.contains("two")))
                .then_some(items)
        });
        let resized: Vec<_> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item, StreamItem::Resized { .. }))
            .collect();
        assert_eq!(resized.len(), 1, "one size change, one event: {items:?}");
        let (at, item) = resized[0];
        let StreamItem::Output {
            seq_end: first_end, ..
        } = &items[at - 1]
        else {
            panic!("output precedes the resize: {items:?}");
        };
        assert_eq!(
            item,
            &StreamItem::Resized {
                at_seq: *first_end,
                resize_gen: 1
            }
        );
        assert!(
            matches!(&items[at + 1], StreamItem::Output { data, .. } if data.contains("two")),
            "output follows the resize: {items:?}"
        );
    }
    state.kill("rsz");
}

/// Subscribe at points spread over a live stream: each snapshot holds exactly
/// the output before its stream's first batch. `Err` names the first gap or
/// duplicate.
fn check_snapshot_then_live(snapshot_after_unlock: bool) -> Result<(), String> {
    let (sink, state) = recording_state();
    crate::emulator::attach_echo(&state.emulator);
    spawn(
        &state,
        sh("i=0; while [ $i -lt 400 ]; do echo line-$i; i=$((i+1)); sleep 0.002; done; sleep 30"),
        registration("live"),
    )
    .expect("spawn");
    let shared = state.shared_handle("live").unwrap();
    shared.lock().unwrap().snapshot_after_unlock = snapshot_after_unlock;
    let mut attached = Vec::new();
    for _ in 0..8 {
        attached.push(state.attach_viewport("live", &shared)?);
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = |events: &[TerminalDomainEvent]| -> String {
        events
            .iter()
            .filter_map(|event| match event {
                TerminalDomainEvent::Output { session, data, .. } if session.as_str() == "live" => {
                    Some(data.as_str())
                }
                _ => None,
            })
            .collect()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !output(&sink.snapshot()).contains("line-399") {
        if std::time::Instant::now() > deadline {
            return Err("the stream did not finish".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(100));
    state.kill("live");
    let events = sink.snapshot();
    let ring = output(&events);
    for attach in attached {
        let mut rebuilt = attach.snapshot.vt.clone();
        for item in stream_items(&events, attach.stream_id) {
            if let StreamItem::Output { seq_end, data } = item {
                if seq_end <= attach.snapshot.seq {
                    return Err(format!(
                        "output ending at {seq_end} is also in the snapshot at {}",
                        attach.snapshot.seq
                    ));
                }
                rebuilt.push_str(&data);
            }
        }
        if rebuilt != ring {
            return Err(format!(
                "snapshot at {} + stream differ from the session's output",
                attach.snapshot.seq
            ));
        }
    }
    Ok(())
}

#[test]
fn subscribe_snapshot_then_live_no_gap() {
    check_snapshot_then_live(false).unwrap();
}

/// Negative control: asking for the snapshot after releasing the session
/// lock lets output land in both the snapshot and the stream.
#[test]
fn snapshot_then_live_check_catches_a_snapshot_after_unlock() {
    let error = check_snapshot_then_live(true).unwrap_err();
    assert!(error.contains("also in the snapshot"), "{error}");
}
