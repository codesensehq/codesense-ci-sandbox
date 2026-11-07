// SPDX-License-Identifier: MIT
//! The terminal host: the live session registry plus the layout it owns, built
//! the same way by every host (the in-process app and the session server) so
//! both run one runtime. The host's sink is wrapped so a process exit turns its
//! layout entry `Exited` and a removed session leaves the layout, each announced
//! as a `LayoutChanged` through the same sink.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use log::warn;

#[cfg(not(feature = "e2e-dump"))]
use crate::domain::DEBUG_DUMP_UNSUPPORTED;
use crate::domain::{
    ClientBootId, DebugDump, MonotonicClock, PortUnavailable, PtySessionId, PtySessionRuntime,
    ReapOutcome, RespawnStart, SubmitOutcome, SubscribeResult, TeardownReason, TerminalDomain,
    TerminalDomainEvent, TerminalEventSink, TerminalRegistry, TerminalText, ViewerId, ViewportId,
    WorkspaceKey,
};
use crate::emulator::{
    EmulatorConfig, EmulatorDump, EmulatorError, EmulatorStatus, EmulatorSupervisor, EmulatorText,
    TerminalSnapshot, TextMode,
};
use crate::layout::{
    CloseMode, CloseOutcome, Effects, GridState, HostIdentity, LayoutError, LayoutModel,
    LayoutSnapshot, OpenTab, Placement, ServerIdentity, TabEntry, TagPatch, ViewHint,
    WorkspaceDescriptor,
};
use crate::runner::PtySession;
use crate::state::PtyState;
use crate::term_state::StreamId;
use crate::{SessionMeta, SessionRegistration};

/// One host's sessions and their layout. The session registry is reachable
/// only through [`TerminalDomain`] (plus [`register`](Self::register) for a
/// session this host's driver just spawned).
pub struct TerminalHost {
    pub(crate) sessions: PtyState,
    layout: Arc<LayoutModel>,
    /// The host's own sink — where layout announcements go.
    events: Arc<dyn TerminalEventSink>,
    /// The host's emulator worker, once started.
    emulator: OnceLock<EmulatorSupervisor>,
}

/// How long a read of a session's emulator terminal waits for the worker.
const EMULATOR_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl TerminalHost {
    pub fn activity(&self) -> &Arc<crate::activity::ActivityOwner> {
        &self.sessions.activity
    }

    /// The one constructor every host uses. `sink` is the host's side-effect
    /// adapter; `blocking` is the pool reaps run on.
    pub fn new(
        identity: ServerIdentity,
        sink: Arc<dyn TerminalEventSink>,
        blocking: tokio::runtime::Handle,
    ) -> Self {
        let layout = Arc::new(LayoutModel::new(identity));
        let layered = Arc::new(LayoutSink {
            inner: sink.clone(),
            layout: layout.clone(),
        });
        Self {
            sessions: PtyState::new(layered, blocking, layout.clone(), crate::agent_cli::adapter),
            layout,
            events: sink,
            emulator: OnceLock::new(),
        }
    }

    /// Start this host's emulator worker. A host that cannot start one keeps
    /// serving its terminals; the error says why there is no emulator.
    pub fn start_emulator(&self, config: EmulatorConfig) -> Result<(), EmulatorError> {
        if self.emulator.get().is_some() {
            return Err(EmulatorError::AlreadyStarted);
        }
        let supervisor = EmulatorSupervisor::start(config, self.sessions.emulator.clone())?;
        // A concurrent start that won drops this one, stopping its worker.
        self.emulator
            .set(supervisor)
            .map_err(|_| EmulatorError::AlreadyStarted)
    }

    /// Run this host's terminals in an echo worker instead of the real one
    /// (see [`attach_echo`](crate::emulator::attach_echo)).
    #[cfg(any(test, feature = "test-support"))]
    pub fn attach_echo_emulator(&self) {
        crate::emulator::attach_echo(&self.sessions.emulator);
    }

    /// The emulator worker's status; `None` before it started (or when it
    /// could not).
    pub fn emulator_status(&self) -> Option<EmulatorStatus> {
        self.emulator.get().map(EmulatorSupervisor::status)
    }

    /// `session`'s terminal as the emulator worker has parsed it, after every
    /// batch and resize made before this call.
    pub fn emulator_dump(
        &self,
        session: &PtySessionId,
        timeout: std::time::Duration,
    ) -> Result<EmulatorDump, String> {
        self.sessions.emulator_dump(session.as_str(), timeout)
    }

    /// `session`'s terminal as text, ordered like
    /// [`emulator_dump`](Self::emulator_dump).
    pub fn emulator_text(
        &self,
        session: &PtySessionId,
        lines: u32,
        mode: TextMode,
        timeout: std::time::Duration,
    ) -> Result<EmulatorText, String> {
        self.sessions
            .emulator_text(session.as_str(), lines, mode, timeout)
    }

    /// `session`'s terminal serialized, ordered like
    /// [`emulator_dump`](Self::emulator_dump).
    pub fn emulator_snapshot(
        &self,
        session: &PtySessionId,
        timeout: std::time::Duration,
    ) -> Result<TerminalSnapshot, String> {
        self.sessions.emulator_snapshot(session.as_str(), timeout)
    }

    /// Output of `session` the emulator worker has not parsed yet, in bytes.
    pub fn emulator_lag_bytes(&self, session: &PtySessionId) -> Option<u64> {
        self.sessions.emulator_lag_bytes(session.as_str())
    }

    /// Whether `session`'s output has ended (its `Exit` is emitted or on its
    /// way), or the session is gone.
    /// The byte watermark of `session`'s output so far; `None` when gone.
    pub fn output_seq(&self, session: &PtySessionId) -> Option<u64> {
        self.sessions.output_seq(session.as_str())
    }

    pub fn output_ended(&self, session: &PtySessionId) -> bool {
        self.sessions.output_ended(session.as_str())
    }

    /// The host's persisted identity, this host lifetime's epoch, and its
    /// layout's current revision.
    pub fn identity(&self) -> Result<HostIdentity, LayoutError> {
        self.layout.identity()
    }

    /// Every layout entry, with its workspace, in strip order. Read-only.
    pub fn tab_entries(&self) -> Result<Vec<(WorkspaceKey, TabEntry)>, LayoutError> {
        self.layout.entries()
    }

    /// The first half of [`TerminalDomain::open_tab`], for a spawn that runs
    /// outside this call (a client of this host across its socket spawns
    /// between two requests): reserve `request.session` so its exit or removal
    /// during the spawn is recorded. An absent workspace needs a descriptor,
    /// checked before anything spawns. Every reservation ends in
    /// [`finish_tab`](Self::finish_tab).
    pub fn reserve_tab(&self, request: &OpenTab) -> Result<(), LayoutError> {
        let session = &request.session;
        if self.sessions.has_session(session.as_str()) {
            // A spawn would displace a session this layout does not own.
            return Err(LayoutError::AlreadyPlaced(session.clone()));
        }
        self.layout
            .reserve(session, &request.workspace, request.descriptor.as_ref())
    }

    /// The second half: place the reserved session once its spawn ended as
    /// `spawned`. A failed spawn only drops the reservation. The spawn must
    /// have registered this very session; one that is not in the registry is
    /// never placed as a live entry, and nothing unplaced keeps running.
    pub fn finish_tab(
        &self,
        request: OpenTab,
        spawned: Result<(), String>,
    ) -> Result<TabEntry, LayoutError> {
        self.reservation(&request.session).finish(request, spawned)
    }

    /// The first half of [`TerminalDomain::respawn_tab`], for a spawn that runs
    /// outside this call: `session`'s entry must be exited, and no other respawn
    /// of it under way — the entry is then held until
    /// [`finish_respawn`](Self::finish_respawn) or
    /// [`cancel_respawn`](Self::cancel_respawn). The ended session leaves the
    /// registry (a reap: the entry, binding and provenance stay; its scrollback
    /// is carried to the next session under its id) before this returns; so is
    /// one whose process outlived its terminal (its output ended), which shows
    /// nothing and takes no input. When the process under the id is an earlier
    /// respawn's own — its finish never arrived — it is revived instead; any
    /// other process still running there refuses.
    pub fn begin_respawn(&self, session: &PtySessionId) -> Result<RespawnStart, LayoutError> {
        self.layout.reserve_respawn(session)?;
        let hold = self.respawn_hold(session);
        let start = match self.sessions.reap_ended(session.as_str(), true) {
            ReapOutcome::Reaped => RespawnStart::Reaped,
            ReapOutcome::Missing => {
                self.sessions.arm_respawn(session.as_str());
                RespawnStart::Reaped
            }
            ReapOutcome::Running if self.sessions.take_respawned(session.as_str()) => {
                RespawnStart::Revived(self.revive(session)?)
            }
            ReapOutcome::Running => return Err(LayoutError::Running(session.clone())),
        };
        hold.keep();
        Ok(start)
    }

    /// The second half: once the spawn ended as `spawned`, turn the entry live
    /// (it keeps its place) unless the new process already exited. A failed
    /// spawn leaves it exited and drops the carried scrollback; a new process
    /// whose entry went meanwhile is not left running.
    pub fn finish_respawn(
        &self,
        session: &PtySessionId,
        spawned: Result<(), String>,
    ) -> Result<TabEntry, LayoutError> {
        if let Err(error) = spawned {
            self.cancel_respawn(session);
            return Err(LayoutError::Spawn(error));
        }
        self.sessions.drop_respawn(session.as_str());
        self.revive(session)
    }

    /// Abandon a respawn between its halves (its spawn failed, or the client
    /// that began it went away): the entry stays exited.
    pub fn cancel_respawn(&self, session: &PtySessionId) {
        self.layout.release_respawn(session);
        self.sessions.abandon_respawn(session.as_str());
    }

    fn revive(&self, session: &PtySessionId) -> Result<TabEntry, LayoutError> {
        match self.layout.finish_respawn(session) {
            Ok((entry, effects)) => {
                self.apply(effects);
                Ok(entry)
            }
            Err(error) => {
                self.sessions.kill(session.as_str());
                Err(error)
            }
        }
    }

    /// The guard of `session`'s respawn hold, between its halves.
    fn respawn_hold(&self, session: &PtySessionId) -> RespawnHold<'_> {
        RespawnHold {
            host: self,
            session: session.clone(),
            armed: true,
        }
    }

    /// The guard of `session`'s reservation.
    fn reservation(&self, session: &PtySessionId) -> Reservation<'_> {
        Reservation {
            host: self,
            session: session.clone(),
            armed: true,
        }
    }

    /// Place `request`'s reserved session once its spawn ended as `spawned`;
    /// the reservation is consumed either way.
    fn place_reserved(
        &self,
        request: OpenTab,
        spawned: Result<(), String>,
    ) -> Result<TabEntry, LayoutError> {
        let session = request.session.clone();
        if let Err(error) = spawned {
            self.layout.release(&session);
            return Err(LayoutError::Spawn(error));
        }
        let placed = if self.sessions.has_session(session.as_str()) {
            self.layout.place_opened(request)
        } else {
            self.layout.release(&session);
            Err(LayoutError::SessionGone(session.clone()))
        };
        match placed {
            Ok((entry, effects)) => {
                self.apply(effects);
                Ok(entry)
            }
            Err(error) => {
                self.sessions.kill(session.as_str());
                Err(error)
            }
        }
    }

    /// Carry out a layout mutation's effects, outside the layout lock.
    fn apply(&self, effects: Effects) {
        for event in effects.events {
            let _ = self.events.emit(event);
        }
        for (session, cols, rows) in effects.resize {
            if let Err(e) = self.sessions.resize(session.as_str(), cols, rows) {
                warn!(target: "pty", "layout resize of {session} failed: {e}");
            }
        }
        for session in effects.kill {
            self.sessions.kill(session.as_str());
        }
    }

    fn apply_ok(&self, effects: Result<Effects, LayoutError>) -> Result<(), LayoutError> {
        self.apply(effects?);
        Ok(())
    }

    /// Register a session this host's [`PtyRunner`](crate::runner::PtyRunner)
    /// just spawned: insert it (retiring any same-id owner) and start its
    /// reader + batcher. Synchronous and potentially blocking — call it off an
    /// async worker. On failure the session is torn down.
    pub fn register(
        &self,
        pty: PtySession,
        registration: SessionRegistration,
    ) -> Result<(), String> {
        self.sessions.register(pty, registration)
    }

    /// How many sessions have a running child, not counting unattached
    /// warm-pool shells: the sessions restarting this host would end.
    pub fn live_session_count(&self) -> usize {
        self.sessions.live_session_count()
    }

    pub fn drain_for_replacement(&self, deadline: std::time::Instant) -> Result<usize, String> {
        self.sessions.drain_for_replacement(deadline)
    }

    /// Emit a retained session's exit again whenever it is used as if it
    /// still ran (a failed write, an attach): for a host whose clients can
    /// miss events, such as a session server's connections.
    pub fn redeliver_exits_on_use(&self) {
        self.sessions.redeliver_exits_on_use();
    }

    /// The exit-time complement kill of one client: the non-persisting
    /// sessions among `owned`, the ones that client opened; with
    /// `unowned_pool` every unhanded warm-pool member; and with `leftovers`
    /// every non-persisting session unviewed for longer than its grace.
    pub fn reap_owned_non_persisting(
        &self,
        owned: &[PtySessionId],
        unowned_pool: bool,
        leftovers: Option<crate::LeftoverReap>,
    ) {
        let owned = owned.iter().map(|id| id.as_str().to_string()).collect();
        self.sessions
            .kill_owned_non_persisting(&owned, unowned_pool, leftovers);
    }

    /// Every session as a status read sees it.
    pub fn session_facts(&self) -> Vec<crate::SessionFacts> {
        self.sessions.session_facts()
    }

    /// The sessions whose agent CLI this host observes still running.
    pub fn running_agent_sessions(&self) -> std::collections::HashSet<PtySessionId> {
        self.activity()
            .snapshots()
            .into_iter()
            .filter(|snapshot| snapshot.exit.is_none())
            .map(|snapshot| PtySessionId::new(snapshot.session))
            .collect()
    }

    /// The session runtime over this host's registry.
    fn runtime<'a>(&'a self, clock: &'a MonotonicClock) -> PtySessionRuntime<'a> {
        PtySessionRuntime::new(&self.sessions, clock)
    }
}

/// The bare ids of `sessions`, for the registry's batched reads.
fn id_strs(sessions: &[PtySessionId]) -> Vec<&str> {
    sessions.iter().map(PtySessionId::as_str).collect()
}

/// Wraps a host's sink: exits and removals update the layout first, then the
/// original event and any resulting `LayoutChanged` go through the host's sink.
struct LayoutSink {
    inner: Arc<dyn TerminalEventSink>,
    layout: Arc<LayoutModel>,
}

impl TerminalEventSink for LayoutSink {
    fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
        let effects = match &event {
            TerminalDomainEvent::Exit {
                session, status, ..
            } => self.layout.on_exit(session, status.clone()),
            // A reaped session's entry stays: a new process takes its id next.
            TerminalDomainEvent::SessionRemoved {
                reason: TeardownReason::Reap,
                ..
            } => Effects::default(),
            TerminalDomainEvent::SessionRemoved { session, .. } => self.layout.on_removed(session),
            _ => Effects::default(),
        };
        let delivered = self.inner.emit(event);
        for change in effects.events {
            let _ = self.inner.emit(change);
        }
        delivered
    }
}

/// An `open_tab` reservation between its spawn and its placement. Finishing
/// consumes it; dropped unfinished (the spawn panicked), it is released.
struct Reservation<'a> {
    host: &'a TerminalHost,
    session: PtySessionId,
    armed: bool,
}

impl Reservation<'_> {
    fn finish(
        mut self,
        request: OpenTab,
        spawned: Result<(), String>,
    ) -> Result<TabEntry, LayoutError> {
        let finished = self.host.place_reserved(request, spawned);
        self.armed = false;
        finished
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.host.layout.release(&self.session);
        }
    }
}

/// A respawn hold that is cancelled unless its respawn finishes: a spawn that
/// panics (or an early return) never leaves the tab "already restarting", or
/// its carried scrollback behind.
struct RespawnHold<'a> {
    host: &'a TerminalHost,
    session: PtySessionId,
    armed: bool,
}

impl RespawnHold<'_> {
    /// The hold outlives this guard: its respawn continues elsewhere (a
    /// client's spawn between two requests).
    fn keep(mut self) {
        self.armed = false;
    }

    /// Finish the respawn once its spawn ended as `spawned`.
    fn finish(mut self, spawned: Result<(), String>) -> Result<TabEntry, LayoutError> {
        self.armed = false;
        self.host.finish_respawn(&self.session, spawned)
    }
}

impl Drop for RespawnHold<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.host.cancel_respawn(&self.session);
        }
    }
}

impl TerminalHost {
    /// `submit_line`, with `bracket` (see `PtySessionRuntime::submit_line_as`): a
    /// single line goes as one bracketed paste, for a TUI that has it on.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_line_bracketed(
        &self,
        session: &PtySessionId,
        line: &str,
        quiet_ms: u64,
        confirm_ms: u64,
        timeout_ms: u64,
        max_nudges: u32,
        bracket: bool,
    ) -> Result<SubmitOutcome, PortUnavailable> {
        let submitted = self
            .runtime(&MonotonicClock::new())
            .submit_line_as(
                session.as_str(),
                line,
                quiet_ms,
                confirm_ms,
                timeout_ms,
                max_nudges,
                bracket,
            )
            .map_err(PortUnavailable)?;
        Ok(SubmitOutcome { submitted })
    }
}

impl TerminalDomain for TerminalHost {
    fn session_output(
        &self,
        session: &PtySessionId,
        identity: &crate::activity::Identity,
        turn_generation: u64,
    ) -> Result<Option<crate::agent_cli::output::SessionRecord>, PortUnavailable> {
        Ok(self
            .activity()
            .session_output(session.as_str(), identity, turn_generation))
    }

    fn activity_snapshot(&self) -> Result<Vec<crate::activity::Snapshot>, PortUnavailable> {
        Ok(self.activity().snapshots())
    }

    fn activity_prepare_shell(
        &self,
        session: &PtySessionId,
        provider: crate::agent_cli::AgentCli,
    ) -> Result<crate::activity::ShellLaunch, PortUnavailable> {
        self.activity()
            .prepare_shell_launch(session.as_str(), provider)
            .map_err(PortUnavailable)
    }

    fn activity_mutate(
        &self,
        mutation: crate::activity::Mutation,
    ) -> Result<crate::activity::Snapshot, PortUnavailable> {
        self.activity()
            .mutate(mutation)
            .map_err(|error| PortUnavailable(error.into()))
    }

    fn inbox_wake_holds(
        &self,
        generation: u64,
        holds: Vec<crate::inbox_host::WakeHold>,
    ) -> Result<(), PortUnavailable> {
        self.activity().inbox().set_wake_holds(generation, holds);
        Ok(())
    }

    fn inbox_mail(
        &self,
        session: &PtySessionId,
        notice: &str,
        token: &str,
    ) -> Result<(), PortUnavailable> {
        self.activity()
            .inbox_mail(session.as_str(), notice.to_string(), token.to_string())
            .map_err(PortUnavailable)
    }

    fn inbox_waiters(&self) -> Result<Vec<crate::domain::InboxWaiterState>, PortUnavailable> {
        Ok(self.activity().inbox_waiters())
    }

    fn subscribe(
        &self,
        session: &PtySessionId,
        _viewport: &ViewportId,
    ) -> Result<SubscribeResult, PortUnavailable> {
        self.runtime(&MonotonicClock::new())
            .subscribe(session.as_str())
            .map_err(PortUnavailable)
    }

    fn unsubscribe(&self, stream: StreamId) -> Result<bool, PortUnavailable> {
        self.sessions.unsubscribe(stream).map_err(PortUnavailable)
    }

    fn read_terminal_text(
        &self,
        session: &PtySessionId,
        lines: u32,
        mode: TextMode,
    ) -> Result<TerminalText, PortUnavailable> {
        let text = self
            .sessions
            .emulator_text(session.as_str(), lines, mode, EMULATOR_READ_TIMEOUT)
            .map_err(PortUnavailable)?;
        Ok(TerminalText {
            text: text.lines.join("\n"),
            seq: text.seq_end,
            alt_screen: text.alt_screen,
        })
    }

    #[cfg(feature = "e2e-dump")]
    fn debug_dump(&self, session: &PtySessionId) -> Result<DebugDump, PortUnavailable> {
        self.sessions
            .debug_dump(session.as_str(), EMULATOR_READ_TIMEOUT)
            .map_err(PortUnavailable)
    }

    #[cfg(not(feature = "e2e-dump"))]
    fn debug_dump(&self, _session: &PtySessionId) -> Result<DebugDump, PortUnavailable> {
        Err(PortUnavailable(DEBUG_DUMP_UNSUPPORTED.to_string()))
    }

    fn resize(
        &self,
        viewer: &ViewerId,
        session: &PtySessionId,
        cols: u16,
        rows: u16,
    ) -> Result<(), PortUnavailable> {
        // The PTY takes the size requested by the most recently active viewer
        // that sized it; an unknown id has nothing to arbitrate (or remember).
        let (cols, rows) = if self.sessions.has_session(session.as_str()) {
            self.layout.request_size(viewer, session, cols, rows)
        } else {
            (cols, rows)
        };
        self.sessions
            .resize(session.as_str(), cols, rows)
            .map_err(PortUnavailable)
    }

    fn write(&self, session: &PtySessionId, bytes: &[u8]) -> Result<(), PortUnavailable> {
        TerminalRegistry::write(&self.sessions, session.as_str(), bytes).map_err(PortUnavailable)
    }

    fn submit_line(
        &self,
        session: &PtySessionId,
        line: &str,
        quiet_ms: u64,
        confirm_ms: u64,
        timeout_ms: u64,
        max_nudges: u32,
    ) -> Result<SubmitOutcome, PortUnavailable> {
        self.submit_line_bracketed(
            session, line, quiet_ms, confirm_ms, timeout_ms, max_nudges, false,
        )
    }

    fn submit_shell_line(
        &self,
        session: &PtySessionId,
        line: &str,
        quiet_ms: u64,
        timeout_ms: u64,
    ) -> Result<SubmitOutcome, PortUnavailable> {
        let submitted = self
            .runtime(&MonotonicClock::new())
            .submit_shell_line(session.as_str(), line, quiet_ms, timeout_ms)
            .map_err(PortUnavailable)?;
        Ok(SubmitOutcome { submitted })
    }

    fn is_idle(&self, session: &PtySessionId) -> Result<bool, PortUnavailable> {
        self.sessions
            .is_idle(session.as_str())
            .map_err(PortUnavailable)
    }

    fn kill(&self, session: &PtySessionId) -> Result<(), PortUnavailable> {
        self.sessions.kill(session.as_str());
        Ok(())
    }

    fn handoff_warm_member(
        &self,
        member: &PtySessionId,
        session: &PtySessionId,
        target_cwd: &str,
        cols: u16,
        rows: u16,
        meta: SessionMeta,
    ) -> Result<(), PortUnavailable> {
        // Architecture: warm handoff enters a worktree through the same fence as a cold launch.
        let _admission =
            crate::remote_workspace::removal::admit_spawn(target_cwd).map_err(PortUnavailable)?;
        self.sessions
            .handoff_warm_member(
                member.as_str(),
                session.as_str(),
                target_cwd,
                cols,
                rows,
                meta,
            )
            .map_err(PortUnavailable)
    }

    fn reap_non_persisting(&self) -> Result<(), PortUnavailable> {
        self.sessions.kill_non_persisting();
        Ok(())
    }

    fn has_session(&self, session: &PtySessionId) -> Result<bool, PortUnavailable> {
        Ok(self.sessions.has_session(session.as_str()))
    }

    fn alive_session_ids(
        &self,
        sessions: &[PtySessionId],
    ) -> Result<HashSet<PtySessionId>, PortUnavailable> {
        Ok(self
            .sessions
            .alive_session_ids(&id_strs(sessions))
            .into_iter()
            .map(PtySessionId::new)
            .collect())
    }

    fn redeliver_exits(
        &self,
        sessions: &[PtySessionId],
    ) -> Result<HashSet<PtySessionId>, PortUnavailable> {
        Ok(self
            .sessions
            .redeliver_exits(&id_strs(sessions))
            .into_iter()
            .map(PtySessionId::new)
            .collect())
    }

    fn live_session_ids(&self) -> Result<HashSet<PtySessionId>, PortUnavailable> {
        Ok(self
            .sessions
            .live_session_ids()
            .map_err(PortUnavailable)?
            .into_iter()
            .map(PtySessionId::new)
            .collect())
    }

    fn live_child_pids(
        &self,
        sessions: &[PtySessionId],
    ) -> Result<Vec<Option<u32>>, PortUnavailable> {
        self.sessions
            .live_child_pids(&id_strs(sessions))
            .map_err(PortUnavailable)
    }

    fn agents_alive(
        &self,
        queries: &[(PtySessionId, String)],
    ) -> Result<Vec<bool>, PortUnavailable> {
        let ids: Vec<_> = queries.iter().map(|(session, _)| session.clone()).collect();
        let pids = self.live_child_pids(&ids)?;
        let index = pids
            .iter()
            .any(Option::is_some)
            .then(crate::process::ProcessIndex::build);
        Ok(queries
            .iter()
            .zip(pids)
            .map(|((_, name), pid)| match (pid, index.as_ref()) {
                (Some(pid), Some(index)) => index.has_named_process(pid, name),
                _ => false,
            })
            .collect())
    }

    fn worktree_usage(
        &self,
        source: &str,
    ) -> Result<crate::layout::WorktreeUsage, PortUnavailable> {
        self.layout
            .worktree_usage(std::path::Path::new(source))
            .map_err(|e| PortUnavailable(crate::layout::source_refusal(e)))
    }

    fn session_len_and_dead(&self) -> Result<Option<(usize, usize)>, PortUnavailable> {
        Ok(self.sessions.session_len_and_dead())
    }

    fn live_session_home_dirs(&self) -> Result<Option<Vec<PathBuf>>, PortUnavailable> {
        Ok(self.sessions.live_session_home_dirs())
    }

    fn session_cwd(&self, session: &PtySessionId) -> Result<Option<PathBuf>, PortUnavailable> {
        Ok(self.sessions.session_cwd(session.as_str()))
    }

    fn layout_snapshot(
        &self,
        viewer: &ViewerId,
        client_boot_id: &ClientBootId,
    ) -> Result<LayoutSnapshot, LayoutError> {
        let (snapshot, effects) = self.layout.snapshot(viewer, client_boot_id)?;
        self.apply(effects);
        Ok(snapshot)
    }

    fn open_tab(
        &self,
        request: OpenTab,
        spawn: Box<dyn FnOnce() -> Result<(), String> + '_>,
    ) -> Result<TabEntry, LayoutError> {
        self.reserve_tab(&request)?;
        let reservation = self.reservation(&request.session);
        let spawned = spawn();
        reservation.finish(request, spawned)
    }

    fn respawn_tab(
        &self,
        session: &PtySessionId,
        spawn: Box<dyn FnOnce() -> Result<(), String> + '_>,
    ) -> Result<TabEntry, LayoutError> {
        match self.begin_respawn(session)? {
            RespawnStart::Revived(entry) => Ok(entry),
            RespawnStart::Reaped => {
                let hold = self.respawn_hold(session);
                let spawned = spawn();
                hold.finish(spawned)
            }
        }
    }

    fn close_tab(
        &self,
        viewer: &ViewerId,
        session: &PtySessionId,
        mode: CloseMode,
    ) -> Result<CloseOutcome, LayoutError> {
        // The process census runs outside the layout lock; the close itself is
        // decided under it.
        let keep_running = mode == CloseMode::KeepIfBusy
            && !self.layout.entry_state(session)?.is_exited()
            && !self.sessions.is_idle(session.as_str()).unwrap_or(false);
        let (outcome, effects) = self.layout.close(viewer, session, keep_running)?;
        self.apply(effects);
        Ok(outcome)
    }

    fn place_tab(
        &self,
        session: &PtySessionId,
        workspace: &WorkspaceKey,
        placement: Placement,
        index: Option<usize>,
        descriptor: Option<WorkspaceDescriptor>,
    ) -> Result<(), LayoutError> {
        self.apply_ok(
            self.layout
                .place(session, workspace, placement, index, descriptor),
        )
    }

    fn rename_tab(&self, session: &PtySessionId, name: String) -> Result<(), LayoutError> {
        self.apply_ok(self.layout.rename(session, name))
    }

    fn move_tab(&self, session: &PtySessionId, to_index: usize) -> Result<(), LayoutError> {
        self.apply_ok(self.layout.move_to(session, to_index))
    }

    fn set_active(
        &self,
        viewer: &ViewerId,
        workspace: &WorkspaceKey,
        session: Option<&PtySessionId>,
    ) -> Result<(), LayoutError> {
        self.apply_ok(self.layout.set_active(viewer, workspace, session))
    }

    fn set_grid(&self, workspace: &WorkspaceKey, grid: GridState) -> Result<(), LayoutError> {
        self.apply_ok(self.layout.set_grid(workspace, grid))
    }

    fn set_view_hint(
        &self,
        viewer: &ViewerId,
        session: &PtySessionId,
        hint: ViewHint,
    ) -> Result<(), LayoutError> {
        self.layout.set_view_hint(viewer, session, hint)
    }

    fn set_tags(&self, session: &PtySessionId, patch: TagPatch) -> Result<(), LayoutError> {
        self.apply_ok(self.layout.set_tags(session, patch))
    }

    fn set_descriptor(
        &self,
        workspace: &WorkspaceKey,
        descriptor: WorkspaceDescriptor,
    ) -> Result<(), LayoutError> {
        self.apply_ok(self.layout.set_descriptor(workspace, descriptor))
    }

    fn close_workspace(&self, workspace: &WorkspaceKey) -> Result<(), LayoutError> {
        self.apply_ok(self.layout.close_workspace(workspace))
    }
}

#[cfg(test)]
mod tests;
