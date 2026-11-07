// SPDX-License-Identifier: MIT
//! Rust terminal domain contract (Tauri-free).
//!
//! Newtype identities, the two lifecycle axes, and the narrow ports the terminal
//! runtime depends on — expressed with **no** `AppHandle`, Tauri window, or
//! command DTO in any signature. This mirrors the `AutomationSubjectProvider`
//! capability shape: a trait/type layer with zero framework coupling, adapters at
//! the edge. The session registry drives every host side effect through
//! [`TerminalEventSink`]; the host supplies the one adapter.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::domain::SubscribeResult;
use crate::layout::{
    CloseMode, CloseOutcome, GridState, LayoutError, LayoutSnapshot, OpenTab, Placement, TabEntry,
    TagPatch, ViewHint, WorkspaceDescriptor, WorkspaceLayout,
};
use crate::osc::OscEvent;
use crate::runner::ExitInfo;
use crate::term_state::StreamId;
use crate::SessionMeta;

/// Generate a `#[serde(transparent)]` string newtype so each identity is a
/// distinct type the compiler will not let callers interchange, while the
/// wire form stays a bare string (compatibility). DRY: every identity shares one
/// definition.
macro_rules! id_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self::new(value)
            }
        }

        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id_newtype!(
    /// The team that owns the epic rails and story workspaces.
    TeamId
);
id_newtype!(
    /// A grouped rail of story workspaces (the epic group).
    EpicRailId
);
id_newtype!(
    /// A workspace in the host's terminal layout. Opaque: the client mints it and
    /// the host never interprets it (story, team and worktree live in the
    /// workspace's descriptor).
    WorkspaceKey
);
id_newtype!(
    /// The foreground tile a workspace presents.
    ForegroundTileId
);
id_newtype!(
    /// A terminal tab within a workspace.
    TerminalTabId
);
id_newtype!(
    /// The Rust-owned PTY session backing a terminal tab.
    PtySessionId
);
id_newtype!(
    /// A transient viewport bound to a PTY session while visible.
    ViewportId
);
id_newtype!(
    /// The renderer instance a viewport currently drives.
    RendererId
);
id_newtype!(
    /// One attached client of a host — the owner of its own focus, scroll and
    /// requested terminal size.
    ViewerId
);
id_newtype!(
    /// One app launch of a client. A snapshot request under a new boot id means
    /// the client relaunched, so exited entries are pruned first.
    ClientBootId
);
id_newtype!(
    /// A host's stable identity, persisted beside the instance's data root so it
    /// survives restarts (unlike the per-boot layout epoch).
    ServerId
);

// The frontend contract also defines `DomFocusId`. It has no Rust peer by
// design: DOM focus is a browser-only concept the Rust terminal domain never
// owns or observes, so it lives only in the frontend contract. The layout keys
// workspaces by the client-minted `WorkspaceKey`, and adds the host-side
// `ViewerId`, `ClientBootId` and `ServerId`.

/// The PTY process axis — the OS process/session, owned by the runtime. Exists
/// independently of any viewport. It has no `Hidden`/`Detached` phase on
/// purpose: whether a viewport is attached is the *viewport* axis's concern, so
/// `Active` spans "running with a viewport attached" and "running with none" —
/// the PTY keeps living regardless, which is exactly why the axes are separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PtyLifecyclePhase {
    Spawn,
    Ready,
    Active,
    Terminated,
}

/// The viewport axis — a transient surface that attaches to a living PTY. None of
/// its transitions imply a PTY transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewportLifecyclePhase {
    Attach,
    Ready,
    Active,
    Hidden,
    Detached,
    Restored,
    Terminated,
}

/// A rejected lifecycle transition — structured, not a panic or bare bool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IllegalTransition {
    pub axis: &'static str,
    pub from: &'static str,
    pub to: &'static str,
}

impl std::fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} -> {} is not a legal transition",
            self.axis, self.from, self.to
        )
    }
}

impl std::error::Error for IllegalTransition {}

impl PtyLifecyclePhase {
    fn label(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::Ready => "ready",
            Self::Active => "active",
            Self::Terminated => "terminated",
        }
    }

    /// Teardown is reachable from any live phase and is absorbing.
    pub fn can_transition_to(self, to: Self) -> bool {
        matches!(
            (self, to),
            (Self::Spawn, Self::Ready)
                | (Self::Ready, Self::Active)
                | (Self::Active, Self::Ready)
                | (Self::Spawn | Self::Ready | Self::Active, Self::Terminated)
        )
    }

    pub fn validate(self, to: Self) -> Result<(), IllegalTransition> {
        if self.can_transition_to(to) {
            Ok(())
        } else {
            Err(IllegalTransition {
                axis: "pty",
                from: self.label(),
                to: to.label(),
            })
        }
    }
}

impl ViewportLifecyclePhase {
    fn label(self) -> &'static str {
        match self {
            Self::Attach => "attach",
            Self::Ready => "ready",
            Self::Active => "active",
            Self::Hidden => "hidden",
            Self::Detached => "detached",
            Self::Restored => "restored",
            Self::Terminated => "terminated",
        }
    }

    pub fn can_transition_to(self, to: Self) -> bool {
        use ViewportLifecyclePhase::*;
        let legal: &[ViewportLifecyclePhase] = match self {
            Attach => &[Ready, Detached, Terminated],
            Ready => &[Active, Hidden, Detached, Terminated],
            Active => &[Hidden, Detached, Ready, Terminated],
            Hidden => &[Restored, Detached, Terminated],
            Detached => &[Restored, Terminated],
            Restored => &[Ready, Active, Hidden, Terminated],
            Terminated => &[],
        };
        legal.contains(&to)
    }

    pub fn validate(self, to: Self) -> Result<(), IllegalTransition> {
        if self.can_transition_to(to) {
            Ok(())
        } else {
            Err(IllegalTransition {
                axis: "viewport",
                from: self.label(),
                to: to.label(),
            })
        }
    }
}

/// What a host answers when asked to attach to a session it does not hold (it
/// ended and was freed, or never existed). The webview matches this exact text
/// (`SESSION_NOT_FOUND` in `src/features/viewer/lib/terminal-attach.ts`) to show
/// an ended pane rather than a failure; any other error is a transport failure.
pub const SESSION_NOT_FOUND: &str = "session not found";

/// The error a port or a terminal host returns, carrying the exact cause. The
/// text is the registry's own (e.g. `write_pty: session not found: …`)
/// and survives a host's socket byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortUnavailable(pub String);

impl std::fmt::Display for PortUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PortUnavailable {}

impl From<PortUnavailable> for String {
    fn from(error: PortUnavailable) -> Self {
        error.0
    }
}

/// Why a session left the registry. Mirrors the host's audit vocabulary: a
/// deliberate close versus a background reclaim (the dead-session cap or a
/// failed background start).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TeardownReason {
    /// Deliberate close (tab close, stop, exit-time complement kill). The host
    /// also drops the session's staged launch provenance.
    Kill,
    /// Background reclaim — never logged as a human kill.
    Expire,
    /// A session whose process already ended, removed so a new process can take
    /// its id (a respawn in place). Nothing was killed and the tab stays: the
    /// host keeps its dashboard binding, audit and launch provenance, and its
    /// layout keeps the entry.
    Reap,
}

/// How a respawn in place began.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RespawnStart {
    /// The ended session is gone: spawn the new process now.
    Reaped,
    /// The entry showed exited but a process already runs under its id (an
    /// earlier respawn whose finish was lost): it is live again, nothing to spawn.
    Revived(crate::layout::TabEntry),
}

/// What reaping an ended session did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReapOutcome {
    /// The ended session is gone from the registry.
    Reaped,
    /// Refused: the session's process is still running.
    Running,
    /// The host has no such session.
    Missing,
}

/// Every side effect the session registry hands its host. `seq` is the absolute
/// **byte** watermark (never a UTF-16 length) at the end of the delivered data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TerminalDomainEvent {
    MachineStats {
        sample: codesense_host_proto::machine_stats::MachineStats,
    },
    /// Latest host-owned observation; revisions reconcile with ActivitySnapshot.
    Activity { snapshot: crate::activity::Snapshot },
    /// A wake delivery reached a final status (`SessionWakeV1`); pushed only to
    /// connections that asked for wake outcomes.
    Wake {
        outcome: Box<crate::session_wake::WakeOutcome>,
    },
    /// `InboxWaitV1`: the session's agent was handed the notice reserved under
    /// `token` (its waiter printed it, or its CLI accepted it). Pushed only to the
    /// session's desktop.
    InboxAck {
        session: PtySessionId,
        token: String,
    },
    /// `InboxWaitV1`: the notice reserved under `token` was refused by the session's
    /// CLI with no effect: it may be offered again. Pushed only to the session's
    /// desktop.
    InboxRelease {
        session: PtySessionId,
        token: String,
    },
    /// `InboxWaitV1`: whether the notice reserved under `token` reached the session's
    /// CLI is unknown: it must not be offered again until it is settled (told).
    /// Pushed only to the session's desktop.
    InboxUncertain {
        session: PtySessionId,
        token: String,
    },
    /// `InboxWaitV1`: whom new mail for the session reaches changed (a waiter, or a
    /// host submit). Pushed only to the session's desktop.
    InboxWaiter { state: InboxWaiterState },
    /// A delivered batch on the session-wide output channel.
    Output {
        session: PtySessionId,
        data: String,
        seq: u64,
    },
    /// The same batch for one attached viewport stream. An `Err` from the sink
    /// counts as a failed delivery in the subscriber's telemetry.
    StreamOutput {
        stream: StreamId,
        data: String,
        seq: u64,
    },
    /// The session's PTY changed size, for one attached viewport stream, in
    /// order with its output: the output before it was written at the old
    /// size. `at_seq` is the stream position then (it consumes none);
    /// `resize_gen` counts the session's size changes, one per event.
    StreamResized {
        stream: StreamId,
        at_seq: u64,
        resize_gen: u32,
        cols: u16,
        rows: u16,
    },
    /// An OSC 133 prompt marker the shell emitted (never an OSC 7 cwd report,
    /// which the registry keeps itself — see `CwdChanged`).
    PromptMarker {
        session: PtySessionId,
        marker: OscEvent,
    },
    /// The session's working directory as reported by OSC 7 mutated
    /// (the first OSC 7 after spawn, or a `cd` that changed the stored path).
    /// Path-equal re-reports are deduped at the batcher, so this fires once
    /// per real cwd change. The app sink forwards it to the frontend as the
    /// `pty-osc7` event, which `useSessionCwd`'s revision counter listens on
    /// so the editor + diff retarget without a session restart.
    CwdChanged { session: PtySessionId, cwd: PathBuf },
    /// The session's reader reached end-of-stream: its child exited. `status`
    /// is the real exit status when the child was reapable; `pool_member` tells
    /// the host whether this was an unattached warm-pool shell; `requested` is
    /// true when the host itself ended the session (a teardown killed the
    /// child), so a signal death is not a crash. Absent on the wire from an
    /// older host, it reads false. `incarnation` names the process that
    /// exited, unique per spawn on its host, so a repeat of an old process's
    /// exit is told from the exit of the process that took the id next; `0`
    /// when unknown (an older host, or an exit the app infers).
    Exit {
        session: PtySessionId,
        status: Option<ExitInfo>,
        pool_member: bool,
        #[serde(default)]
        requested: bool,
        #[serde(default)]
        incarnation: u64,
    },
    /// A session became live for `cwd` (a spawn or a warm handoff into it).
    WorktreeLive { cwd: PathBuf },
    /// A session left the registry (the id may already have been gone).
    SessionRemoved {
        session: PtySessionId,
        reason: TeardownReason,
    },
    /// One workspace of the host's terminal layout changed. `layout` is the whole
    /// shared workspace (idempotent to apply) or `None` once the workspace is
    /// gone. `rev` is the layout's revision after this change within the host
    /// lifetime `epoch` (each host counts its own); changes may be delivered out
    /// of order, so a viewer ignores any `rev` at or below the last it applied
    /// for that epoch and re-fetches the snapshot on a gap or an epoch change.
    /// A viewer filters out the exited entries its snapshot lists as hidden.
    LayoutChanged {
        epoch: String,
        rev: u64,
        workspace: WorkspaceKey,
        layout: Option<Box<WorkspaceLayout>>,
    },
}

/// A session's terminal as its emulator holds it, as plain text: `text` is the
/// requested lines joined by `\n`, as of stream position `seq`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalText {
    pub text: String,
    pub seq: u64,
    /// The alternate screen was active.
    pub alt_screen: bool,
}

/// What a person looking at `session` sees, as plain text: the rows of the
/// alternate screen while a full-screen program holds it, otherwise the last
/// `lines` lines of the normal buffer (scrollback included).
pub fn visible_text(
    domain: &dyn TerminalDomain,
    session: &PtySessionId,
    lines: u32,
) -> Result<TerminalText, PortUnavailable> {
    let normal = domain.read_terminal_text(session, lines, crate::emulator::TextMode::Normal)?;
    if !normal.alt_screen {
        return Ok(normal);
    }
    domain.read_terminal_text(session, lines, crate::emulator::TextMode::Screen)
}

/// Only the rows on screen now, in whichever buffer is active: the current
/// frame of a program that redraws in place on the normal screen (`top`),
/// without the earlier frames its redraws pushed into scrollback.
pub fn screen_text(
    domain: &dyn TerminalDomain,
    session: &PtySessionId,
) -> Result<TerminalText, PortUnavailable> {
    domain.read_terminal_text(session, u32::MAX, crate::emulator::TextMode::Screen)
}

/// A session's emulator terminal in full (the frontend's `referenceBuffer()`
/// fields in `dump`), with the foreground process group of its PTY so a
/// caller can freeze the program before comparing a display with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebugDump {
    pub seq: u64,
    pub resize_gen: u32,
    pub history_lost: bool,
    pub fg_pgid: Option<i32>,
    pub dump: serde_json::Map<String, serde_json::Value>,
}

/// What `debug_dump` answers on a host built without the `e2e-dump` feature.
pub const DEBUG_DUMP_UNSUPPORTED: &str = "unsupported: this host is not built with e2e-dump";

/// Emit domain events without knowing about Tauri. The host's one adapter
/// forwards them to its event bus and subsystems; tests use a recording fake.
pub trait TerminalEventSink: Send + Sync {
    fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable>;
}

/// Persist and reload a session snapshot. Replaces the persistence side-channel's
/// coupling to command state.
pub trait SessionPersistencePort: Send + Sync {
    fn persist(&self, session: &PtySessionId, snapshot: &[u8]) -> Result<(), PortUnavailable>;
    fn load(&self, session: &PtySessionId) -> Result<Option<Vec<u8>>, PortUnavailable>;
}

/// A monotonic clock — replaces `Instant::now()` in the subscribe settle loop so
/// timing is injectable and deterministic in tests.
pub trait ClockPort: Send + Sync {
    fn now_millis(&self) -> u64;
}

/// Liveness/process census without reaching into OS globals from the domain.
pub trait ProcessInspector: Send + Sync {
    fn is_alive(&self, pid: u32) -> bool;
}

/// The narrow ports the runtime depends on, bundled so the host wires them in one
/// place instead of threading four separate `Arc`s through the domain — and so
/// every port is part of a composed surface rather than a loose definition.
pub struct TerminalRuntimePorts {
    pub events: Arc<dyn TerminalEventSink>,
    pub persistence: Arc<dyn SessionPersistencePort>,
    pub clock: Arc<dyn ClockPort>,
    pub process: Arc<dyn ProcessInspector>,
}

/// What a submit-line hands back — whether the line was confirmed submitted
/// within the caller's budget (vs. settled/nudged out). Also the submit
/// commands' reply to the webview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SubmitOutcome {
    pub submitted: bool,
}

/// The Tauri-free terminal domain surface: one terminal host's session
/// operations and reads plus its layout verbs. The app reaches every host
/// through it — the in-process [`TerminalHost`](crate::TerminalHost) and the
/// session-server sidecar's client alike. No method accepts an `AppHandle`,
/// Tauri window, or command DTO; every error carries the host's exact text.
pub trait TerminalDomain: Send + Sync {
    fn activity_prepare_shell(
        &self,
        _session: &PtySessionId,
        _provider: crate::agent_cli::AgentCli,
    ) -> Result<crate::activity::ShellLaunch, PortUnavailable> {
        Err(PortUnavailable(
            "owner activity unavailable; host upgrade required".into(),
        ))
    }
    fn activity_snapshot(&self) -> Result<Vec<crate::activity::Snapshot>, PortUnavailable> {
        Err(PortUnavailable(
            "owner activity unavailable; host upgrade required".into(),
        ))
    }

    /// Read only output belonging to a captured host invocation/turn. Older hosts degrade to none.
    fn session_output(
        &self,
        _session: &PtySessionId,
        _identity: &crate::activity::Identity,
        _turn_generation: u64,
    ) -> Result<Option<crate::agent_cli::output::SessionRecord>, PortUnavailable> {
        Ok(None)
    }

    fn activity_mutate(
        &self,
        _mutation: crate::activity::Mutation,
    ) -> Result<crate::activity::Snapshot, PortUnavailable> {
        Err(PortUnavailable(
            "owner activity unavailable; host upgrade required".into(),
        ))
    }

    /// `InboxWaitV1`: hold `notice`, reserved under `token`, for `session`'s agent; its
    /// fate comes back as [`TerminalDomainEvent::InboxAck`],
    /// [`TerminalDomainEvent::InboxRelease`] or [`TerminalDomainEvent::InboxUncertain`].
    fn inbox_wake_holds(
        &self,
        _generation: u64,
        _holds: Vec<crate::inbox_host::WakeHold>,
    ) -> Result<(), PortUnavailable> {
        Err(PortUnavailable(
            "wake holds require a terminal server update".into(),
        ))
    }

    fn inbox_mail(
        &self,
        _session: &PtySessionId,
        _notice: &str,
        _token: &str,
    ) -> Result<(), PortUnavailable> {
        Err(PortUnavailable(
            "inbox notices unavailable; host upgrade required".into(),
        ))
    }

    /// `InboxWaitV1`: whom new mail reaches for each of this client's sessions with a
    /// way to be told, and from now on every change as a
    /// [`TerminalDomainEvent::InboxWaiter`].
    fn inbox_waiters(&self) -> Result<Vec<InboxWaiterState>, PortUnavailable> {
        Err(PortUnavailable(
            "inbox notices unavailable; host upgrade required".into(),
        ))
    }

    /// Attach a viewport: wait (bounded) for a warm handoff's prompt and a cold
    /// shell's first render, then register a live stream and ask the emulator
    /// for its terminal atomically against the batcher: the stream carries
    /// exactly what follows the snapshot — no gap, no duplicate.
    fn subscribe(
        &self,
        session: &PtySessionId,
        viewport: &ViewportId,
    ) -> Result<SubscribeResult, PortUnavailable>;

    /// Detach a viewport's stream. `Ok(false)` when this host has no such
    /// stream (already gone, or another host's).
    fn unsubscribe(&self, stream: StreamId) -> Result<bool, PortUnavailable>;

    /// The last `lines` lines of `session`'s emulator terminal as text (see
    /// [`TextMode`](crate::emulator::TextMode)), after all output delivered so
    /// far.
    fn read_terminal_text(
        &self,
        session: &PtySessionId,
        lines: u32,
        mode: crate::emulator::TextMode,
    ) -> Result<TerminalText, PortUnavailable>;

    /// `session`'s emulator terminal in full, after all output delivered so
    /// far. [`DEBUG_DUMP_UNSUPPORTED`] unless the host is built with
    /// `e2e-dump`.
    fn debug_dump(&self, session: &PtySessionId) -> Result<DebugDump, PortUnavailable>;

    /// Request a size for `session` on behalf of `viewer`. The PTY follows the
    /// most recently active viewer that requested a size for it.
    fn resize(
        &self,
        viewer: &ViewerId,
        session: &PtySessionId,
        cols: u16,
        rows: u16,
    ) -> Result<(), PortUnavailable>;

    fn write(&self, session: &PtySessionId, bytes: &[u8]) -> Result<(), PortUnavailable>;

    /// Submit a single line, running the byte-driven settle/confirm protocol
    /// (write → wait-quiet → confirm/nudge). `*_ms` are the caller's budget; the
    /// outcome reports whether submission was confirmed within it.
    fn submit_line(
        &self,
        session: &PtySessionId,
        line: &str,
        quiet_ms: u64,
        confirm_ms: u64,
        timeout_ms: u64,
        max_nudges: u32,
    ) -> Result<SubmitOutcome, PortUnavailable>;

    /// Submit a line into a bare shell: settle, then write it once. `false`
    /// (nothing written) when the prompt never settled.
    fn submit_shell_line(
        &self,
        session: &PtySessionId,
        line: &str,
        quiet_ms: u64,
        timeout_ms: u64,
    ) -> Result<SubmitOutcome, PortUnavailable>;

    /// Whether the session's foreground is idle — no live descendant process is
    /// running under the shell (the process-tree census, injected via
    /// [`ProcessInspector`]).
    fn is_idle(&self, session: &PtySessionId) -> Result<bool, PortUnavailable>;

    /// Tear `session` down; a missing id is a no-op.
    fn kill(&self, session: &PtySessionId) -> Result<(), PortUnavailable>;

    /// Hand the warm-pool shell `member` off to `session`: rename, resize,
    /// stamp `meta` and `cd` it to `target_cwd` under the output gate.
    fn handoff_warm_member(
        &self,
        member: &PtySessionId,
        session: &PtySessionId,
        target_cwd: &str,
        cols: u16,
        rows: u16,
        meta: SessionMeta,
    ) -> Result<(), PortUnavailable>;

    /// The exit-time complement kill: tear down every non-persisting session.
    fn reap_non_persisting(&self) -> Result<(), PortUnavailable>;

    /// Whether this host tracks `session` (membership, not liveness).
    fn has_session(&self, session: &PtySessionId) -> Result<bool, PortUnavailable>;

    /// Which of `sessions` have a running child.
    fn alive_session_ids(
        &self,
        sessions: &[PtySessionId],
    ) -> Result<HashSet<PtySessionId>, PortUnavailable>;

    /// Emit again the `Exit` of each of `sessions` that has exited but is
    /// still retained, for a client that missed the original; the ids whose
    /// exit went out. A host that predates the question answers an error.
    fn redeliver_exits(
        &self,
        _sessions: &[PtySessionId],
    ) -> Result<HashSet<PtySessionId>, PortUnavailable> {
        Err(PortUnavailable("exit redelivery unavailable".into()))
    }

    /// EVERY session on this host whose child is running — the
    /// live-PTY arm of the union roster. Unlike [`Self::alive_session_ids`] (which
    /// probes a caller-supplied id list) this ENUMERATES the whole host, so it also
    /// surfaces the registry-less bare sessions (root / notes / git-sync / `#` /
    /// no-MAIN-tab) the register-on-open ledger missed — it is NOT
    /// workspace-bound, so those bare sessions are kept. Exited-but-retained and
    /// warm-pool sessions are excluded.
    fn live_session_ids(&self) -> Result<HashSet<PtySessionId>, PortUnavailable>;

    /// The running shell pid of each of `sessions`, in order; `None` for a
    /// missing or exited one.
    fn live_child_pids(
        &self,
        sessions: &[PtySessionId],
    ) -> Result<Vec<Option<u32>>, PortUnavailable>;

    /// Probe named agents on their owning host; never export remote PIDs for
    /// a desktop process-table lookup. Older hosts leave telemetry unknown.
    fn agents_alive(
        &self,
        _queries: &[(PtySessionId, String)],
    ) -> Result<Vec<bool>, PortUnavailable> {
        Err(PortUnavailable(
            "owner process telemetry unavailable".into(),
        ))
    }

    /// `(total, dead_retained)` sessions, or `None` when the registry was busy
    /// (the memory sampler never blocks on it).
    /// Architecture: only current hosts can positively confirm canonical source-path usage.
    fn worktree_usage(
        &self,
        _source: &str,
    ) -> Result<crate::layout::WorktreeUsage, PortUnavailable> {
        Err(PortUnavailable(crate::layout::source_refusal(
            crate::layout::OLD_SERVER,
        )))
    }

    fn session_len_and_dead(&self) -> Result<Option<(usize, usize)>, PortUnavailable>;

    /// The spawn dirs of every running session, or `None` when the registry was
    /// busy — the caller fails open.
    fn live_session_home_dirs(&self) -> Result<Option<Vec<PathBuf>>, PortUnavailable>;

    /// The working directory `session` last reported (OSC 7).
    fn session_cwd(&self, session: &PtySessionId) -> Result<Option<PathBuf>, PortUnavailable>;

    /// The host's whole terminal layout as `viewer` sees it. A request under a
    /// `client_boot_id` other than the one `viewer` last used hides every exited
    /// entry from `viewer` first (a relaunched client restores only live tabs);
    /// an exited entry is freed once no viewer shows it.
    fn layout_snapshot(
        &self,
        viewer: &ViewerId,
        client_boot_id: &ClientBootId,
    ) -> Result<LayoutSnapshot, LayoutError>;

    /// Spawn a session through `spawn` and place it, atomically: the entry is
    /// never observable as live for a session that is already gone.
    fn open_tab(
        &self,
        request: OpenTab,
        spawn: Box<dyn FnOnce() -> Result<(), String> + '_>,
    ) -> Result<TabEntry, LayoutError>;

    /// Start a new process in `session`'s exited entry, under the same id and
    /// in the same place: refused (`Running`) while the entry or its process is
    /// live. The ended session leaves the registry (a reap: the tab, binding
    /// and provenance stay) before `spawn` runs, and the entry turns live once
    /// `spawn` registered the new process. A failed spawn leaves it exited.
    fn respawn_tab(
        &self,
        session: &PtySessionId,
        spawn: Box<dyn FnOnce() -> Result<(), String> + '_>,
    ) -> Result<TabEntry, LayoutError>;

    /// Close an entry for `viewer`. A live one is killed, or backgrounded when
    /// `mode` keeps a busy one; an exited one is removed for `viewer` and freed
    /// once no viewer shows it.
    fn close_tab(
        &self,
        viewer: &ViewerId,
        session: &PtySessionId,
        mode: CloseMode,
    ) -> Result<CloseOutcome, LayoutError>;

    /// Move an entry to `workspace` (created from `descriptor` when absent) with
    /// `placement`, at `index` or the end.
    fn place_tab(
        &self,
        session: &PtySessionId,
        workspace: &WorkspaceKey,
        placement: Placement,
        index: Option<usize>,
        descriptor: Option<WorkspaceDescriptor>,
    ) -> Result<(), LayoutError>;

    fn rename_tab(&self, session: &PtySessionId, name: String) -> Result<(), LayoutError>;

    fn move_tab(&self, session: &PtySessionId, to_index: usize) -> Result<(), LayoutError>;

    /// Focus `session` in `workspace` for `viewer` only; it also becomes the
    /// workspace's default for a fresh attach.
    fn set_active(
        &self,
        viewer: &ViewerId,
        workspace: &WorkspaceKey,
        session: Option<&PtySessionId>,
    ) -> Result<(), LayoutError>;

    fn set_grid(&self, workspace: &WorkspaceKey, grid: GridState) -> Result<(), LayoutError>;

    fn set_view_hint(
        &self,
        viewer: &ViewerId,
        session: &PtySessionId,
        hint: ViewHint,
    ) -> Result<(), LayoutError>;

    fn set_tags(&self, session: &PtySessionId, patch: TagPatch) -> Result<(), LayoutError>;

    fn set_descriptor(
        &self,
        workspace: &WorkspaceKey,
        descriptor: WorkspaceDescriptor,
    ) -> Result<(), LayoutError>;

    /// Kill every entry of `workspace`, which then disappears.
    fn close_workspace(&self, workspace: &WorkspaceKey) -> Result<(), LayoutError>;
}

/// Whether a session's agent has an inbox waiter on its host: one is waiting now, or for
/// `grace_ms` more its agent still reads as one that new mail wakes (it re-arms within
/// that grace; see `codesense_pty_core::inbox_waiters`). The grace is a duration, so the
/// two hosts' clocks never meet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxWaiterState {
    pub session: PtySessionId,
    pub waiting: bool,
    #[serde(default)]
    pub grace_ms: Option<u64>,
}
