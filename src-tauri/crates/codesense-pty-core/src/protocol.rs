// SPDX-License-Identifier: MIT
//! The terminal service of the host protocol, between a host of this runtime
//! that lives out of process (the `codesense-session-server` sidecar) and its
//! client (the CodeSense app). Framing, the handshake and the version skew rule
//! are `codesense-host-proto`'s; this module supplies what the terminal service
//! carries in them.
//!
//! A connection opens with each side's `Hello` (the sidecar's followed by its
//! census); only a client of the same instance at the same protocol major may
//! go on. The client then sends [`Request`]s as control calls, answered by
//! replies carrying the same `rid`, and keystrokes as raw bytes on an input
//! channel per session ([`Inputs`] on the client), each answered like a call.
//! After [`Request::Listen`] the sidecar pushes every runtime side effect its
//! registry emits, unchanged: output batches as raw bytes on a channel per
//! output source — a session, or one attached viewport's stream — ([`Egress`]
//! on the sidecar, [`Ingress`] on the client), everything else as a control
//! push. The client feeds the events to the same event sink the in-process
//! registry uses. Error strings cross verbatim.
//!
//! The socket and the singleton lock live under the host instance's app-data
//! root, and the handshake carries that root's identity, so two instances with
//! different roots can never share a sidecar.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use codesense_host_proto::{
    read_frame, write_frame, Capability, Control, Envelope, Frame, Hello, Kind,
    FIRST_STREAM_CHANNEL, PROTO_MAJOR, PROTO_MINOR,
};
use serde::{Deserialize, Serialize};

use crate::domain::{
    ClientBootId, DebugDump, PtySessionId, SubscribeResult, TerminalDomainEvent, TerminalText,
    ViewerId, ViewportId, WorkspaceKey,
};
use crate::emulator::TextMode;
use crate::layout::{
    CloseMode, CloseOutcome, GridState, LayoutError, LayoutSnapshot, OpenTab, Placement, TabEntry,
    TagPatch, ViewHint, WorkspaceDescriptor,
};
use crate::spawn::SpawnIntent;
use crate::term_state::StreamId;
use crate::{SessionMeta, SessionRegistration};

/// The sidecar socket's file name inside an instance's app-data root.
const SOCKET_FILE_NAME: &str = "pty-server.sock";

/// The sidecar's singleton lock file, next to the socket.
const LOCK_FILE_NAME: &str = "pty-server.lock";

/// The sidecar socket of the instance rooted at `root`.
pub fn socket_path(root: &Path) -> PathBuf {
    root.join(SOCKET_FILE_NAME)
}

/// The singleton lock of the instance rooted at `root`.
pub fn lock_path(root: &Path) -> PathBuf {
    root.join(LOCK_FILE_NAME)
}

/// Whether a host process serves the instance rooted at `root`, answering or
/// not: one holds the instance's singleton lock for as long as it runs. The
/// probe briefly takes the lock shared, so a host starting at that instant
/// would find it held: never probe while this instance may be launching its
/// host. `Err` when it cannot be told.
pub fn host_running(root: &Path) -> Result<bool, String> {
    use std::os::fd::AsRawFd;
    let path = lock_path(root);
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("open {path:?}: {e}")),
    };
    // SAFETY: `flock` on a descriptor `file` owns and keeps open for the call;
    // the shared lock taken on success is released as `file` closes.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        return Ok(false);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(true)
    } else {
        Err(format!("probe {path:?}: {error}"))
    }
}

/// The pid the host serving the instance rooted at `root` recorded in its
/// lock file, if it did.
pub fn host_pid(root: &Path) -> Option<u32> {
    std::fs::read_to_string(lock_path(root))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The identity of the instance rooted at `root`: its canonical path, so two
/// spellings of one root agree and two roots never do.
pub fn instance_id(root: &Path) -> String {
    std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// A client's hello: the instance it belongs to, its version, its viewer, and
/// the terminal service it wants.
pub fn client_hello(instance_id: String, agent_version: &str, viewer: &ViewerId) -> Hello {
    Hello {
        proto_major: PROTO_MAJOR,
        proto_minor: PROTO_MINOR,
        agent_version: agent_version.to_string(),
        instance_id,
        capabilities: vec![Capability::Terminals],
        viewer_id: Some(viewer.as_str().to_string()),
        pid: std::process::id(),
        ..Hello::default()
    }
}

/// The operations a client drives on the hosted runtime. Each maps to exactly
/// one [`TerminalDomain`](crate::domain::TerminalDomain) call (or the spawn +
/// register pair) on the sidecar side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    MachineStats,
    MachineStatsWatch {
        watch: bool,
    },
    ActivitySnapshot,
    SessionOutput {
        session: PtySessionId,
        identity: crate::activity::Identity,
        turn_generation: u64,
    },
    MutateActivity {
        mutation: crate::activity::Mutation,
    },
    PrepareActivity {
        session: PtySessionId,
        provider: crate::agent_cli::AgentCli,
    },
    PrepareShellActivity {
        session: PtySessionId,
        provider: crate::agent_cli::AgentCli,
    },
    StartRemoteExecutor {
        request: Box<crate::remote_executor::StartExecutor>,
    },
    OpenRemoteExecutor {
        task_id: String,
        channel: u32,
    },
    StopRemoteExecutor {
        task_id: String,
    },
    WorkspaceExport {
        request: Box<crate::remote_export::ExportRequest>,
    },
    /// The commit and branch a completed managed workspace's checkout is on.
    /// Answered with [`Reply::WorkspaceHead`].
    WorkspaceHead {
        target: Box<crate::remote_files::FileTarget>,
    },
    WorkspaceContextBatch {
        request: Box<crate::context_batch::BatchRequest>,
    },
    WorkspaceFile {
        request: Box<crate::remote_files::FileRequest>,
    },
    /// The streamed, hash-cached manifest under some prefixes.
    /// Answered by [`Push::ManifestPart`]s on the same `rid`, then
    /// [`Reply::WorkspaceManifest`]. Only sent to a host advertising
    /// `ContextManifestV1`.
    WorkspaceManifest {
        request: Box<crate::remote_manifest::ManifestRequest>,
    },
    ReconcileRemoteWorkspace {
        request: crate::remote_workspace::PrepareWorkspace,
    },
    InspectWorkspaceRepository {
        request: crate::remote_workspace::InspectRepository,
    },
    /// Tear down a managed worktree on the enrolled daemon.
    /// Idempotent — a missing worktree answers `removed:false` with a note.
    ForgetMissingWorkspace {
        request: crate::remote_workspace::ForgetMissingWorkspace,
    },
    RemoveWorktree {
        request: crate::remote_workspace::RemoveWorktree,
    },
    /// `git status --porcelain` for a managed worktree on the enrolled daemon.
    /// Used by `move_workspace` pre-flight when the source worktree lives on a
    /// remote machine (a remote → local move).
    CheckWorktreeDirtyRemote {
        request: crate::remote_workspace::CheckWorktreeDirtyRemote,
    },
    UploadWorkspaceBundle {
        chunk: crate::remote_workspace::BundleChunk,
    },
    PrepareRemoteWorkspace {
        request: crate::remote_workspace::PrepareWorkspace,
    },
    /// `BundlePipelineV1`: which chunks of a pipelined upload the
    /// machine holds.
    BundleUploadStatus {
        request: crate::remote_workspace::PrepareWorkspace,
    },
    /// Stage one chunk; acks may come back in any order.
    UploadBundleChunk {
        chunk: crate::remote_workspace::StagedBundleChunk,
    },
    /// Flush the complete upload once and verify its digest.
    SealWorkspaceBundle {
        request: crate::remote_workspace::PrepareWorkspace,
    },
    /// `WorkspaceBaseUpdateV1`: move a prepared worktree to a newer base.
    UpdateWorkspaceBase {
        request: Box<crate::remote_workspace::UpdateWorkspaceBase>,
    },
    /// Push the registry's events to this connection from now on.
    Listen,
    /// Provision a reverse channel before this connection spawns the session.
    /// Resolve native executable, flags and private settings without spawning.
    /// Saved-Agent clients negotiate AgentStartup before using this barrier.
    PreflightNativeRemote {
        session: PtySessionId,
        cwd: String,
        launch: codesense_host_proto::agent_cli::NativeLaunch,
    },
    /// Whether the native CLI `launch` names is on the host account's login
    /// PATH, read from `cwd`, without starting it. Answered with
    /// [`Reply::Flag`] (installed).
    ProbeNativeRuntime {
        cwd: String,
        launch: codesense_host_proto::agent_cli::NativeLaunch,
    },
    PrepareReverseMcp {
        session: PtySessionId,
        binding_nonce: String,
        #[serde(
            default = "codesense_host_proto::reverse_mcp::default_servers",
            skip_serializing_if = "codesense_host_proto::reverse_mcp::is_default_servers"
        )]
        server_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remote_entries: Vec<codesense_host_proto::reverse_mcp::RemoteStdioEntry>,
        #[serde(
            default,
            skip_serializing_if = "codesense_host_proto::agent_cli::NativeLaunch::is_shell"
        )]
        launch: codesense_host_proto::agent_cli::NativeLaunch,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_startup: Option<codesense_host_proto::reverse_mcp::AgentStartup>,
    },
    /// Cancel only this connection's reverse-session provision.
    ReleaseReverseMcp {
        session: PtySessionId,
    },
    /// Enter the one-way draining lifecycle and retire this exact lifetime.
    ShutdownForReplacement {
        nonce: String,
        server_id: String,
        epoch: String,
        process_start: u64,
    },
    /// Resolve the intent against the host's own environment, spawn it and
    /// register the session. Answered with [`Reply::Spawned`].
    Spawn {
        intent: SpawnIntent,
        registration: SessionRegistration,
    },
    /// Acquire this connection's writable attachment without subscribing output.
    /// Repeating on the same connection succeeds; another owner is refused.
    ClaimWritable {
        session: PtySessionId,
    },
    /// Release writable ownership, without killing or unsubscribing the session.
    ReleaseWritable {
        session: PtySessionId,
    },
    /// From now on [`Kind::INPUT`] frames on `channel` write to `session`.
    OpenInput {
        session: PtySessionId,
        channel: u32,
    },
    /// `channel` carries no more input.
    CloseInput {
        channel: u32,
    },
    /// Resize on behalf of `viewer`, under the host's most-recently-active
    /// viewer policy.
    Resize {
        viewer: ViewerId,
        session: PtySessionId,
        cols: u16,
        rows: u16,
    },
    Kill {
        session: PtySessionId,
    },
    /// Cancel a spawn of `session` whose reply was lost. Answered once the
    /// sidecar no longer runs it: a spawn in flight is torn down as it
    /// registers, a registered one is killed, and a spawn read shortly after is
    /// refused. Only a registered session is reported removed.
    CancelSpawn {
        session: PtySessionId,
    },
    /// Answered with [`Reply::Flag`].
    IsIdle {
        session: PtySessionId,
    },
    /// The agent submit protocol, run where the session lives. Answered
    /// with [`Reply::Flag`] (submitted).
    SubmitLine {
        session: PtySessionId,
        line: String,
        quiet_ms: u64,
        confirm_ms: u64,
        timeout_ms: u64,
        max_nudges: u32,
    },
    /// The bare-shell submit protocol. Answered with [`Reply::Flag`].
    SubmitShellLine {
        session: PtySessionId,
        line: String,
        quiet_ms: u64,
        timeout_ms: u64,
    },
    /// Hand a warm-pool member off to a frontend session.
    HandoffWarmMember {
        member: PtySessionId,
        session: PtySessionId,
        target_cwd: String,
        cols: u16,
        rows: u16,
        meta: SessionMeta,
    },
    /// The exit-time complement kill: tear down every non-persisting session.
    /// Kept for clients that predate `ReapOwnedNonPersisting`.
    ReapNonPersisting,
    /// The exit-time complement kill of one client: tear down the
    /// non-persisting sessions among `sessions`, the ones it opened (its
    /// warm-pool members included), and nothing another client opened. Asked
    /// only of a host that advertises `Capability::ScopedReapV1`. Answered
    /// with [`Reply::Done`].
    ReapOwnedNonPersisting {
        sessions: Vec<PtySessionId>,
    },
    /// `SessionWakeV1`: queue an idle-gated delivery. A repeated
    /// idempotency key is answered with its stored outcome and writes
    /// nothing. Answered with [`Reply::WakeDelivery`].
    WakeDeliver {
        request: Box<crate::session_wake::DeliverRequest>,
    },
    /// `SessionWakeV1`: cancel a queued delivery this connection's desktop
    /// asked for (`undelivered(archived)`). One already being written, or
    /// already final, is answered with its outcome unchanged. Answered with
    /// [`Reply::WakeDelivery`].
    WakeCancel {
        session: PtySessionId,
        idempotency_key: String,
    },
    /// `SessionWakeV1`: set a timer on a session. Answered with
    /// [`Reply::WakeTimer`].
    WakeTimerSet {
        spec: Box<crate::session_wake::TimerSpec>,
    },
    /// `SessionWakeV1`: the active timers, of one session or of all.
    /// Answered with [`Reply::WakeTimers`].
    WakeTimerList {
        #[serde(default)]
        session: Option<PtySessionId>,
    },
    /// `SessionWakeV1`: cancel a session's timer, and the fire it has queued
    /// (`undelivered(archived)`). Answered with [`Reply::Flag`] (it was
    /// active).
    WakeTimerCancel {
        session: PtySessionId,
        timer_id: String,
    },
    /// `SessionWakeV1`: the outcomes logged after `since`, and from now on
    /// every outcome as a [`TerminalDomainEvent::Wake`] push. Answered with
    /// [`Reply::WakeOutcomes`].
    WakeOutcomes {
        since: u64,
    },
    /// `InboxWaitV1`: a mail notice for `session`'s agent, reserved under `token`
    /// by the desktop the session is bound to. The host holds it for the agent
    /// (replacing a held one; the same token again changes nothing) and pushes
    /// [`TerminalDomainEvent::InboxAck`] once the agent was handed it,
    /// [`TerminalDomainEvent::InboxRelease`] when its CLI refused it, or
    /// [`TerminalDomainEvent::InboxUncertain`] when whether it got it is unknown.
    /// Refused for a session bound to another desktop. Answered with
    /// [`Reply::Done`].
    InboxMail {
        session: PtySessionId,
        notice: String,
        token: String,
    },
    /// `InboxWaitV1`: whom new mail reaches for this desktop's sessions now, and
    /// from now on every change as a [`TerminalDomainEvent::InboxWaiter`] push (with
    /// every notice's fate). Answered with
    /// [`Reply::InboxWaiters`].
    InboxWaiters,
    InboxWakeHolds {
        generation: u64,
        holds: Vec<crate::inbox_host::WakeHold>,
    },
    /// `LocalMcpReadinessV1`: replace this app connection's initialized local
    /// MCP sessions. Disconnection clears the snapshot. Answered with `Done`.
    LocalMcpSessions {
        sessions: Vec<PtySessionId>,
    },
    /// `RoutineArmV1`: arm (or re-arm after an edit) a routine on this host,
    /// fenced by its arm epoch. Answered with [`Reply::RoutineArmAnswer`].
    RoutineArm {
        arm: Box<crate::routine_arm::RoutineArm>,
    },
    /// `RoutineArmV1`: stop firing a routine, leaving a tombstone at `epoch`
    /// so an older arm can never come back. Its queued wakes are cancelled
    /// (`undelivered(archived)`). Answered with [`Reply::RoutineArmAnswer`].
    RoutineDisarm {
        key: String,
        epoch: u64,
    },
    /// `RoutineArmV1`: the armed routines and the tombstones. Answered with
    /// [`Reply::RoutineArms`].
    RoutineArms,
    /// `RoutineArmV1`: the run log after `since`. Answered with
    /// [`Reply::RoutineArmRuns`].
    RoutineArmRuns {
        since: u64,
    },
    /// `RoutineArmV1`: fire an armed routine now, by hand, through its own
    /// policy and this host's single flight. `task` replaces the arm's task
    /// for this run (the desktop rendered its variables). Answered with
    /// [`Reply::RoutineArmFired`].
    RoutineArmFireNow {
        key: String,
        epoch: u64,
        #[serde(default)]
        task: Option<String>,
    },
    /// `RoutineArmV1`: finish a run this host fired into `session` (an
    /// agent's `routine_done`, or its `signal_done` with no `run_id`, that a
    /// desktop relayed). Answered with [`Reply::RoutineArmCompleted`].
    RoutineArmComplete {
        session: String,
        #[serde(default)]
        run_id: Option<String>,
        outcome: String,
        #[serde(default)]
        summary: String,
    },
    /// `RoutineArmV1`: the secret this connection's desktop accepts the
    /// CodeSense MCP of the agents this host starts with (a routine fired
    /// while it is connected): the binding's nonce is keyed by it, so the
    /// desktop knows the binding is this host's. Answered with
    /// [`Reply::Done`]. Forgotten when the connection closes.
    ///
    /// The desktop's stable identity (per install and person),
    /// which the agents it relayed are handed back to when it reconnects;
    /// absent from an older desktop (its viewer stands in). Binding also
    /// arms the connection's liveness: a link silent past its heartbeats is
    /// reaped.
    RoutineArmBindSecret {
        secret: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        desktop: Option<String>,
    },
    /// Attach a viewport, in the host where its emulator terminal lives. Its
    /// stream's output is pushed to this connection. Answered with
    /// [`Reply::Subscribed`].
    Subscribe {
        session: PtySessionId,
        viewport: ViewportId,
    },
    /// Detach a viewport's stream. Answered with [`Reply::Flag`] (it was here).
    Unsubscribe {
        stream: StreamId,
    },
    /// Answered with [`Reply::Flag`].
    HasSession {
        session: PtySessionId,
    },
    /// Answered with [`Reply::Sessions`].
    AliveSessionIds {
        sessions: Vec<PtySessionId>,
    },
    /// Emit again the `Exit` of each of `sessions` that ended but is still
    /// retained; asked only of a host that advertises
    /// `Capability::ExitRedeliveryV1`. Answered with [`Reply::Sessions`] (the
    /// ids whose exit went out).
    RedeliverExits {
        sessions: Vec<PtySessionId>,
    },
    /// What this host holds, for managing it; asked only of a host that
    /// advertises `Capability::ServerStatusV1`. Answered with
    /// [`Reply::ServerStatus`].
    ServerStatus,
    /// The asking app holds `sessions` (they are attributed to it from
    /// now on); asked only of a host that advertises
    /// `Capability::ServerStatusV1`. Answered with [`Reply::Done`].
    ClaimSessions {
        sessions: Vec<PtySessionId>,
    },
    /// End `session` if, checked as it ends, no app holds it; refused
    /// (nothing ends) otherwise. Asked only of a host that advertises
    /// `Capability::ServerStatusV1`. Answered with [`Reply::Done`].
    EndUnownedSession {
        session: PtySessionId,
    },
    /// Enumerate every alive, non-pool session on the host.
    /// Answered with [`Reply::Sessions`].
    LiveSessionIds,
    AgentsAlive {
        queries: Vec<(PtySessionId, String)>,
    },
    /// Answered with [`Reply::Pids`].
    LiveChildPids {
        sessions: Vec<PtySessionId>,
    },
    /// Answered with [`Reply::Counts`].
    SessionLenAndDead,
    /// Answered with [`Reply::Dirs`].
    LiveSessionHomeDirs,
    /// Answered with [`Reply::Cwd`].
    SessionCwd {
        session: PtySessionId,
    },
    /// The host account's home directory, absolute and existing, or `None`.
    /// Answered with [`Reply::Cwd`].
    AccountHome,
    /// The usage spool's records after `after_seq`, at most `limit`, for the desktop to pull.
    /// Asking says the desktop has stored every record through `after_seq`, which lets the host
    /// prune them once they are old enough. Answered with [`Reply::UsageRead`]; asked only of a host
    /// that advertises `Capability::UsageSpoolV1`.
    UsageRead {
        after_seq: u64,
        limit: u32,
    },
    /// The session's cwd, the host account's home and the Git checkout the cwd
    /// is in, for the caption's prompt segment. Answered with [`Reply::Prompt`];
    /// asked only of a host that advertises `Capability::SessionPrompt`.
    SessionPrompt {
        session: PtySessionId,
    },
    /// The session's emulator terminal as text. Answered with [`Reply::Text`].
    ReadText {
        session: PtySessionId,
        lines: u32,
        mode: TextMode,
    },
    /// The session's emulator terminal in full. Answered with [`Reply::Dump`],
    /// or refused by a host built without `e2e-dump`.
    DebugDump {
        session: PtySessionId,
    },
    /// Canonical path usage, including openings; requires WorktreeSafetyV1.
    WorktreeUsage {
        source: String,
    },
    /// The host's layout as `viewer` sees it. Answered with [`Reply::Layout`].
    LayoutSnapshot {
        viewer: ViewerId,
        client_boot_id: ClientBootId,
    },
    /// The first half of an `open_tab` whose session this connection spawns
    /// here next: reserve it. Answered with [`Reply::Done`]. Ended by
    /// [`Request::FinishTab`], or by the connection closing, which places a
    /// session it did spawn and drops the reservation of one it did not.
    ReserveTab {
        request: OpenTab,
    },
    /// Preserve an uncertain spawn's reserved correlation if its session exists.
    /// An absent session leaves the reservation held, never claims non-dispatch.
    ReconcileTab {
        session: PtySessionId,
    },
    /// Place the reserved `session` once its spawn ended as `spawned`.
    /// Answered with [`Reply::Tab`].
    FinishTab {
        session: PtySessionId,
        spawned: Result<(), String>,
    },
    /// The first half of a `respawn_tab` whose new process this connection
    /// spawns next: reap `session`'s ended process, keeping its entry (held for
    /// this connection until [`Request::FinishRespawn`] or the connection
    /// closes). Answered with [`Reply::Done`] once the reap is complete — the
    /// `SessionRemoved { reason: Reap }` push precedes that reply — or with
    /// [`Reply::Tab`] when a process already ran under the id and its entry was
    /// revived (nothing to spawn).
    BeginRespawn {
        session: PtySessionId,
    },
    /// Turn the respawned `session`'s entry live once its spawn ended as
    /// `spawned`. Answered with [`Reply::Tab`].
    FinishRespawn {
        session: PtySessionId,
        spawned: Result<(), String>,
    },
    /// Answered with [`Reply::Closed`].
    CloseTab {
        viewer: ViewerId,
        session: PtySessionId,
        mode: CloseMode,
    },
    PlaceTab {
        session: PtySessionId,
        workspace: WorkspaceKey,
        placement: Placement,
        index: Option<usize>,
        descriptor: Option<WorkspaceDescriptor>,
    },
    RenameTab {
        session: PtySessionId,
        name: String,
    },
    MoveTab {
        session: PtySessionId,
        to_index: usize,
    },
    SetActive {
        viewer: ViewerId,
        workspace: WorkspaceKey,
        session: Option<PtySessionId>,
    },
    SetGrid {
        workspace: WorkspaceKey,
        grid: GridState,
    },
    SetViewHint {
        viewer: ViewerId,
        session: PtySessionId,
        hint: ViewHint,
    },
    SetTags {
        session: PtySessionId,
        patch: TagPatch,
    },
    SetDescriptor {
        workspace: WorkspaceKey,
        descriptor: WorkspaceDescriptor,
    },
    CloseWorkspace {
        workspace: WorkspaceKey,
    },
}

/// What the usage spool holds past a sequence number. `epoch` names the spool file: a new one means
/// it was lost and the desktop must read again from zero. `oldest_seq` is the first record still held,
/// for telling a gap from pruning: `None` when it holds none, and may be `None` when nothing is past
/// the asked-for sequence number (no gap is possible then). `last_seq` is the newest (0 when none).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSpoolRead {
    pub epoch: String,
    pub oldest_seq: Option<u64>,
    pub last_seq: u64,
    pub records: Vec<UsageSpoolRecord>,
}

/// One usage record as its CLI sent it, with the launch token it was exported under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSpoolRecord {
    pub seq: u64,
    pub token: String,
    pub record: serde_json::Value,
}

/// What a terminal server holds, as it answers `ServerStatus`. Warm-pool
/// shells (idle, unhanded) are not counted anywhere.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    /// When this host process started, in ms since the Unix epoch.
    pub started_at_ms: u64,
    /// Its sessions, running or exited but kept (their terminal readable).
    pub sessions: u32,
    /// Of them, the ones whose process still runs.
    pub running: u32,
    /// Of the running ones, those running an agent CLI it observes.
    pub running_agents: u32,
    /// Of them, the exited ones it keeps.
    pub exited_kept: u32,
    /// Apps listening to it (counted by client id, so one app's several
    /// connections count once), the asker's included.
    pub clients: u32,
    /// Of them, the ones other than the asker's.
    pub other_clients: u32,
    /// The sessions no app holds: opened (or last claimed) by an app that has
    /// had no connection for a grace period, and shown by no viewport.
    pub unowned: Vec<UnownedSession>,
    /// This host process's open-file budget; `None` from a
    /// server that predates it, or one that cannot read it.
    #[serde(default)]
    pub nofile: Option<crate::nofile::Usage>,
}

/// A session no connected app holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnownedSession {
    pub session: PtySessionId,
    /// What it runs: its foreground job, else its background jobs; `None`
    /// for a bare shell, an exited session, or when it cannot be told.
    pub command: Option<String>,
    /// When it started, in ms since the Unix epoch.
    pub started_at_ms: u64,
    /// Its process still runs (else it exited and is kept).
    pub running: bool,
    /// It runs an agent CLI the host observes.
    #[serde(default)]
    pub running_agent: bool,
}

/// A successful reply's payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Reply {
    MachineStats {
        sample: Option<codesense_host_proto::machine_stats::MachineStats>,
    },
    SessionOutput {
        record: Option<crate::agent_cli::output::SessionRecord>,
    },
    WorktreeUsage {
        usage: crate::layout::WorktreeUsage,
    },

    ActivityUpdated {
        snapshot: Box<crate::activity::Snapshot>,
    },
    ActivityWriter {
        writer: Box<crate::activity::Writer>,
    },
    ShellActivity {
        launch: Box<crate::activity::ShellLaunch>,
    },
    ActivitySnapshot {
        snapshots: Vec<crate::activity::Snapshot>,
    },
    RemoteExecutorStarted {
        result: crate::remote_executor::ExecutorStarted,
    },
    WorkspaceExport {
        result: Box<crate::remote_export::ExportReply>,
    },
    WorkspaceHead {
        head: crate::remote_export::WorkspaceHead,
    },
    WorkspaceContextBatch {
        result: Box<crate::context_batch::BatchReply>,
    },
    WorkspaceFile {
        result: Box<crate::remote_files::FileReply>,
    },
    WorkspaceManifest {
        result: Box<crate::remote_manifest::ManifestReply>,
    },
    RemoteWorkspaceReceipt {
        result: Option<Box<crate::remote_workspace::PreparedWorkspace>>,
    },
    WorkspaceRepositoryKnowledge {
        knowledge: crate::remote_workspace::RepositoryKnowledge,
    },
    /// Reply to [`Request::RemoveWorktree`].
    WorktreeRemoved {
        result: crate::remote_workspace::RemoveWorktreeResult,
    },
    /// Reply to [`Request::CheckWorktreeDirtyRemote`].
    WorktreeDirtyRemote {
        status: crate::remote_workspace::RemoteWorktreeDirtyStatus,
    },
    WorkspaceBundleProgress {
        progress: crate::remote_workspace::BundleProgress,
    },
    RemoteWorkspacePrepared {
        result: Box<crate::remote_workspace::PreparedWorkspace>,
    },
    BundleUploadStatus {
        status: crate::remote_workspace::BundleUploadStatus,
    },
    BundleChunkStaged {
        index: u32,
    },
    WorkspaceBaseUpdated {
        result: Box<crate::remote_workspace::WorkspaceBaseUpdate>,
    },
    Done,
    /// The exact session already has a different writable connection.
    WritableBusy,
    Retired {
        nonce: String,
        server_id: String,
        epoch: String,
        process_start: u64,
        sessions: u32,
    },
    /// The spawn was not completed because the session was killed while it
    /// was spawning. Final: the caller must not open it anywhere else.
    Cancelled,
    Spawned {
        pid: Option<u32>,
    },
    Flag {
        value: bool,
    },
    Subscribed {
        result: Box<SubscribeResult>,
    },
    Sessions {
        sessions: HashSet<PtySessionId>,
    },
    ServerStatus {
        status: Box<ServerStatus>,
    },
    AgentsAlive {
        alive: Vec<bool>,
    },
    Pids {
        pids: Vec<Option<u32>>,
    },
    Counts {
        counts: Option<(usize, usize)>,
    },
    Dirs {
        dirs: Option<Vec<PathBuf>>,
    },
    /// The usage spool past the requested sequence number. Answered to [`Request::UsageRead`].
    UsageRead {
        read: Box<UsageSpoolRead>,
    },
    Cwd {
        cwd: Option<PathBuf>,
    },
    Prompt {
        prompt: Box<crate::git_prompt::SessionPrompt>,
    },
    Text {
        text: TerminalText,
    },
    Dump {
        dump: Box<DebugDump>,
    },
    Layout {
        snapshot: Box<LayoutSnapshot>,
    },
    Tab {
        entry: Box<TabEntry>,
    },
    Closed {
        outcome: CloseOutcome,
    },
    WakeDelivery {
        outcome: Box<crate::session_wake::WakeOutcome>,
    },
    WakeTimer {
        timer: Box<crate::session_wake::TimerInfo>,
    },
    WakeTimers {
        timers: Vec<crate::session_wake::TimerInfo>,
    },
    WakeOutcomes {
        log: Box<crate::session_wake::OutcomeLog>,
    },
    InboxWaiters {
        waiters: Vec<crate::domain::InboxWaiterState>,
    },
    RoutineArmAnswer {
        answer: Box<crate::routine_arm::ArmAnswer>,
    },
    RoutineArms {
        list: Box<crate::routine_arm::ArmList>,
    },
    RoutineArmRuns {
        log: Box<crate::routine_arm::ArmRunLog>,
    },
    RoutineArmFired {
        answer: Box<crate::routine_arm::ArmFireAnswer>,
    },
    RoutineArmCompleted {
        answer: Box<crate::routine_arm::ArmCompleteAnswer>,
    },
    /// The host's layout refused a layout verb: its error, whole, so it reads
    /// exactly as the same refusal in-process. Every other layout verb that
    /// succeeds is answered with [`Reply::Done`].
    LayoutRefused {
        error: LayoutError,
    },
}

/// Whose output an output channel carries.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputSource {
    /// A session's output channel ([`TerminalDomainEvent::Output`]).
    Session { session: PtySessionId },
    /// One attached viewport stream ([`TerminalDomainEvent::StreamOutput`]).
    Stream { stream: StreamId },
}

/// A sidecar push on the control channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Push {
    /// A side effect the hosted registry emitted (any but output).
    Event { event: TerminalDomainEvent },
    /// From now on `channel` carries `source`'s output batches.
    OutputOpened { channel: u32, source: OutputSource },
    /// `channel` carries no more output.
    OutputClosed { channel: u32 },
    /// One streamed piece of call `rid`'s manifest, ahead of its reply.
    ManifestPart {
        rid: u64,
        part: Box<crate::remote_manifest::ManifestPart>,
    },
}

/// The terminal service's control message.
pub type Message = Control<Request, Reply, Push>;

/// One connection's outbound events on the sidecar. Each output source gets a
/// channel of its own the first time it has output, announced by a
/// [`Push::OutputOpened`] ahead of its first batch, and closed by a
/// [`Push::OutputClosed`]: a session's channel, and the channels of the
/// viewport streams [`bind`](Self::bind)ed to it, once its child has exited
/// (the session's last output precedes its exit) or it left the registry; a
/// stream's once it is [`close`](Self::close)d. Output after a close opens a
/// new channel, which the session's exit then closes.
#[derive(Debug, Default)]
pub struct Egress {
    channels: HashMap<OutputSource, u32>,
    opened: u32,
    streams: HashMap<StreamId, PtySessionId>,
}

impl Egress {
    /// The frames that carry `event`, in order.
    pub fn frames(&mut self, event: TerminalDomainEvent) -> Result<Vec<Frame>, String> {
        let (source, seq, data) = match event {
            TerminalDomainEvent::Output { session, data, seq } => {
                (OutputSource::Session { session }, seq, data)
            }
            TerminalDomainEvent::StreamOutput { stream, data, seq } => {
                (OutputSource::Stream { stream }, seq, data)
            }
            event => {
                let mut frames = match &event {
                    TerminalDomainEvent::Exit { session, .. }
                    | TerminalDomainEvent::SessionRemoved { session, .. } => {
                        self.close_session(session)?
                    }
                    _ => Vec::new(),
                };
                frames.push(Frame::control(&Message::Push {
                    push: Push::Event { event },
                })?);
                return Ok(frames);
            }
        };
        let mut frames = Vec::with_capacity(2);
        let channel = match self.channels.get(&source) {
            Some(channel) => *channel,
            None => {
                let channel = next_channel(&mut self.opened, "output")?;
                frames.push(Frame::control(&Message::Push {
                    push: Push::OutputOpened {
                        channel,
                        source: source.clone(),
                    },
                })?);
                self.channels.insert(source, channel);
                channel
            }
        };
        frames.push(Frame::output(channel, seq, data.as_bytes()));
        Ok(frames)
    }

    /// `stream` is a viewport of `session`: its channel closes with the
    /// session's output.
    pub fn bind(&mut self, stream: StreamId, session: PtySessionId) {
        self.streams.insert(stream, session);
    }

    /// Close `stream`'s channel (its viewport detached): the frame announcing
    /// it, when it was open.
    pub fn close(&mut self, stream: StreamId) -> Result<Option<Frame>, String> {
        self.streams.remove(&stream);
        self.closed(&OutputSource::Stream { stream })
    }

    /// Close `session`'s channel and its viewports'.
    pub fn close_session(&mut self, session: &PtySessionId) -> Result<Vec<Frame>, String> {
        let streams: Vec<StreamId> = self
            .streams
            .iter()
            .filter(|(_, owner)| *owner == session)
            .map(|(stream, _)| *stream)
            .collect();
        let mut frames = Vec::new();
        for stream in streams {
            frames.extend(self.close(stream)?);
        }
        frames.extend(self.closed(&OutputSource::Session {
            session: session.clone(),
        })?);
        Ok(frames)
    }

    fn closed(&mut self, source: &OutputSource) -> Result<Option<Frame>, String> {
        self.channels
            .remove(source)
            .map(|channel| {
                Frame::control(&Message::Push {
                    push: Push::OutputClosed { channel },
                })
            })
            .transpose()
    }
}

/// The next never-used byte-stream channel of a table that has opened
/// `opened` so far; a connection that ran out refuses more.
fn next_channel(opened: &mut u32, what: &str) -> Result<u32, String> {
    let channel = FIRST_STREAM_CHANNEL
        .checked_add(*opened)
        .ok_or_else(|| format!("the connection ran out of {what} channels"))?;
    *opened += 1;
    Ok(channel)
}

/// What a sidecar frame means to its client.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    Reply {
        rid: u64,
        result: Result<Reply, String>,
    },
    Event(TerminalDomainEvent),
    /// A streamed piece of call `rid`'s answer; its reply follows.
    ManifestPart {
        rid: u64,
        part: Box<crate::remote_manifest::ManifestPart>,
    },
}

/// One connection's inbound frames on the client: replies, and the runtime's
/// events rebuilt from control pushes and output channels. The inverse of
/// [`Egress`].
#[derive(Debug, Default)]
pub struct Ingress {
    channels: HashMap<u32, OutputSource>,
}

impl Ingress {
    /// Interpret one frame. `Ok(None)` for a frame that carries nothing for
    /// the caller: a channel announcement, or a kind, call or push this build
    /// does not take. A reply this build cannot read answers its caller with
    /// an error; an `Err` is a frame that breaks the protocol.
    pub fn accept(&mut self, frame: &Frame) -> Result<Option<Inbound>, String> {
        match frame.kind {
            Kind::CONTROL => Ok(match frame.decode_json::<Message>() {
                Err(error) => match Envelope::of(frame) {
                    Some(Envelope::Reply { rid }) => Some(Inbound::Reply {
                        rid,
                        result: Err(format!("unreadable session server reply: {error}")),
                    }),
                    _ => {
                        log::warn!(target: "pty", "skipped an unreadable session server message: {error}");
                        None
                    }
                },
                Ok(message) => self.control(message),
            }),
            Kind::OUTPUT => {
                // A channel this build could not read the announcement of.
                let Some(source) = self.channels.get(&frame.channel) else {
                    log::warn!(target: "pty", "skipped output on unknown channel {}", frame.channel);
                    return Ok(None);
                };
                let (seq, data) = frame.output_parts()?;
                let data = String::from_utf8(data.to_vec())
                    .map_err(|e| format!("output is not UTF-8: {e}"))?;
                Ok(Some(Inbound::Event(match source.clone() {
                    OutputSource::Session { session } => {
                        TerminalDomainEvent::Output { session, data, seq }
                    }
                    OutputSource::Stream { stream } => {
                        TerminalDomainEvent::StreamOutput { stream, data, seq }
                    }
                })))
            }
            _ => Ok(None),
        }
    }

    fn control(&mut self, message: Message) -> Option<Inbound> {
        match message {
            Message::Reply { rid, result } => Some(Inbound::Reply { rid, result }),
            Message::Push {
                push: Push::OutputOpened { channel, source },
            } => {
                self.channels.insert(channel, source);
                None
            }
            Message::Push {
                push: Push::OutputClosed { channel },
            } => {
                self.channels.remove(&channel);
                None
            }
            Message::Push {
                push: Push::Event { event },
            } => Some(Inbound::Event(event)),
            Message::Push {
                push: Push::ManifestPart { rid, part },
            } => Some(Inbound::ManifestPart { rid, part }),
            Message::Call { .. } => None,
        }
    }
}

/// One connection's session inputs on the client: each session's keystrokes
/// ride a raw-byte channel of its own, opened by a call ahead of its first
/// write and closed once the session is gone. The inverse of the host's
/// channel table.
#[derive(Debug, Default)]
pub struct Inputs {
    channels: HashMap<PtySessionId, u32>,
    opened: u32,
}

impl Inputs {
    /// The frames that write `data` to `session` as call `rid`: the opening
    /// of `session`'s channel as call `open_rid()` the first time, then the
    /// input itself, byte-identical.
    pub fn write(
        &mut self,
        session: &PtySessionId,
        rid: u64,
        data: &[u8],
        open_rid: impl FnOnce() -> u64,
    ) -> Result<Vec<Frame>, String> {
        let mut frames = Vec::with_capacity(2);
        let channel = match self.channels.get(session) {
            Some(channel) => *channel,
            None => {
                let channel = next_channel(&mut self.opened, "input")?;
                frames.push(Frame::control(&Message::Call {
                    rid: open_rid(),
                    request: Request::OpenInput {
                        session: session.clone(),
                        channel,
                    },
                })?);
                self.channels.insert(session.clone(), channel);
                channel
            }
        };
        frames.push(Frame::input(channel, rid, data));
        Ok(frames)
    }

    /// `session`'s channel, if it has one.
    pub fn channel(&self, session: &PtySessionId) -> Option<u32> {
        self.channels.get(session).copied()
    }

    /// The call (as `rid()`) closing `session`'s channel, if it has one — only
    /// `channel`, when given: a later channel of the same id (a respawn's) is
    /// left open.
    pub fn close(
        &mut self,
        session: &PtySessionId,
        channel: Option<u32>,
        rid: impl FnOnce() -> u64,
    ) -> Result<Option<Frame>, String> {
        let open = self.channels.get(session).copied();
        if open.is_none() || channel.is_some_and(|only| open != Some(only)) {
            return Ok(None);
        }
        self.channels
            .remove(session)
            .map(|channel| {
                Frame::control(&Message::Call {
                    rid: rid(),
                    request: Request::CloseInput { channel },
                })
            })
            .transpose()
    }
}

/// Send `request` as call `rid` on a connection that takes no pushes, and read
/// its reply.
pub fn call_once(
    reader: &mut impl Read,
    writer: &mut impl Write,
    rid: u64,
    request: Request,
) -> Result<Result<Reply, String>, String> {
    write_frame(writer, &Frame::control(&Message::Call { rid, request })?)
        .map_err(|e| e.to_string())?;
    let mut ingress = Ingress::default();
    loop {
        let frame = read_frame(reader)?.ok_or_else(|| "closed before answering".to_string())?;
        if let Some(Inbound::Reply { rid: got, result }) = ingress.accept(&frame)? {
            if got == rid {
                return Ok(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TeardownReason;
    use crate::osc::OscEvent;
    use crate::runner::ExitInfo;

    fn session(id: &str) -> PtySessionId {
        PtySessionId::new(id)
    }

    fn output(id: &str, data: &str, seq: u64) -> TerminalDomainEvent {
        TerminalDomainEvent::Output {
            session: session(id),
            data: data.into(),
            seq,
        }
    }

    /// Carry `events` from an egress to an ingress through the wire bytes.
    fn carry(events: Vec<TerminalDomainEvent>) -> (Vec<Frame>, Vec<TerminalDomainEvent>) {
        let mut egress = Egress::default();
        let frames: Vec<Frame> = events
            .into_iter()
            .flat_map(|event| egress.frames(event).unwrap())
            .collect();
        let wire: Vec<u8> = frames.iter().flat_map(Frame::encode).collect();
        let mut reader = wire.as_slice();
        let mut ingress = Ingress::default();
        let mut delivered = Vec::new();
        while let Some(frame) = read_frame(&mut reader).unwrap() {
            if let Some(Inbound::Event(event)) = ingress.accept(&frame).unwrap() {
                delivered.push(event);
            }
        }
        (frames, delivered)
    }

    #[test]
    fn machine_stats_requests_reply_and_push_round_trip() {
        let sample: codesense_host_proto::machine_stats::MachineStats =
            serde_json::from_value(serde_json::json!({
                "load1":3.04,"cpuCount":8,"memUsedBytes":12,"memTotalBytes":16,
                "diskFreeBytes":84,"diskTotalBytes":460,"diskPath":"/workspaces","sampledAt":1000
            }))
            .unwrap();
        for message in [
            Message::Call {
                rid: 1,
                request: Request::MachineStats,
            },
            Message::Call {
                rid: 2,
                request: Request::MachineStatsWatch { watch: true },
            },
            Message::Call {
                rid: 3,
                request: Request::MachineStatsWatch { watch: false },
            },
            Message::Reply {
                rid: 1,
                result: Ok(Reply::MachineStats {
                    sample: Some(sample.clone()),
                }),
            },
        ] {
            let frame = Frame::json(Kind::CONTROL, &message).unwrap();
            assert_eq!(frame.decode_json::<Message>().unwrap(), message);
        }
        let events = vec![TerminalDomainEvent::MachineStats { sample }];
        assert_eq!(carry(events.clone()).1, events);
    }

    /// Every runtime event crosses the wire unchanged, in order — including
    /// the child's real exit status, a signal death, and an unknown status.
    #[test]
    fn every_event_round_trips() {
        let events = vec![
            output("s1", "h\u{e9}llo\r\n", 7),
            TerminalDomainEvent::StreamOutput {
                stream: StreamId(3),
                data: "x".into(),
                seq: 8,
            },
            TerminalDomainEvent::PromptMarker {
                session: session("s1"),
                marker: OscEvent::CommandEnd(Some(2)),
            },
            TerminalDomainEvent::Exit {
                session: session("s1"),
                status: Some(ExitInfo {
                    exit_code: Some(42),
                    signal: None,
                }),
                pool_member: false,
                requested: false,
                incarnation: 0,
            },
            TerminalDomainEvent::Exit {
                session: session("s1"),
                status: Some(ExitInfo {
                    exit_code: None,
                    signal: Some("Killed: 9".into()),
                }),
                pool_member: true,
                requested: true,
                incarnation: 0,
            },
            TerminalDomainEvent::Exit {
                session: session("s1"),
                status: None,
                pool_member: false,
                requested: false,
                incarnation: 0,
            },
            TerminalDomainEvent::WorktreeLive {
                cwd: PathBuf::from("/tmp/wt"),
            },
            TerminalDomainEvent::SessionRemoved {
                session: session("s1"),
                reason: TeardownReason::Expire,
            },
            TerminalDomainEvent::LayoutChanged {
                epoch: "epoch-1".into(),
                rev: 4,
                workspace: crate::domain::WorkspaceKey::new("root:acme/app"),
                layout: Some(Box::default()),
            },
            TerminalDomainEvent::LayoutChanged {
                epoch: "epoch-1".into(),
                rev: 5,
                workspace: crate::domain::WorkspaceKey::new("root:acme/app"),
                layout: None,
            },
        ];
        assert_eq!(carry(events.clone()).1, events);
    }

    /// Output travels as raw bytes, each source on its own channel, and never
    /// as JSON: the bytes on the wire are the output itself.
    #[test]
    fn output_rides_a_raw_byte_channel_per_source() {
        let (frames, delivered) = carry(vec![
            output("a", "one", 1),
            output("b", "two", 1),
            output("a", "three", 2),
        ]);
        let outputs: Vec<&Frame> = frames.iter().filter(|f| f.kind == Kind::OUTPUT).collect();
        let channels: Vec<u32> = outputs.iter().map(|f| f.channel).collect();
        assert_eq!(channels, [2, 3, 2]);
        assert_eq!(outputs[2].output_parts(), Ok((2, &b"three"[..])));
        assert_eq!(
            frames.len(),
            5,
            "one announcement per source, then bare batches"
        );
        assert_eq!(
            delivered,
            [
                output("a", "one", 1),
                output("b", "two", 1),
                output("a", "three", 2)
            ]
        );
    }

    /// An exited session's channel closes after its last batch, with the
    /// channels of the viewport streams bound to it; the id's next session
    /// opens a new one, and a stale channel is not reused.
    #[test]
    fn an_exited_sessions_channels_close_after_its_last_output() {
        let stream = |data: &str, seq| TerminalDomainEvent::StreamOutput {
            stream: StreamId(7),
            data: data.into(),
            seq,
        };
        let exit = TerminalDomainEvent::Exit {
            session: session("a"),
            status: None,
            pool_member: false,
            requested: false,
            incarnation: 0,
        };
        let mut egress = Egress::default();
        egress.bind(StreamId(7), session("a"));
        let mut frames = Vec::new();
        for event in [
            output("a", "old", 1),
            stream("old", 1),
            exit.clone(),
            output("a", "new", 1),
        ] {
            frames.extend(egress.frames(event).unwrap());
        }
        let pushes: Vec<Push> = frames
            .iter()
            .filter(|f| f.kind == Kind::CONTROL)
            .map(|f| match f.decode_json::<Message>().unwrap() {
                Message::Push { push } => push,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            pushes[2..5],
            [
                Push::OutputClosed { channel: 3 },
                Push::OutputClosed { channel: 2 },
                Push::Event {
                    event: exit.clone()
                }
            ],
            "the stream's and the session's channels close ahead of the exit"
        );
        let channels: Vec<u32> = frames
            .iter()
            .filter(|f| f.kind == Kind::OUTPUT)
            .map(|f| f.channel)
            .collect();
        assert_eq!(channels, [2, 3, 4]);

        let mut ingress = Ingress::default();
        let delivered: Vec<TerminalDomainEvent> = frames
            .iter()
            .filter_map(|f| match ingress.accept(f).unwrap() {
                Some(Inbound::Event(event)) => Some(event),
                _ => None,
            })
            .collect();
        assert_eq!(
            delivered,
            [
                output("a", "old", 1),
                stream("old", 1),
                exit,
                output("a", "new", 1)
            ]
        );
        assert_eq!(
            ingress.accept(&Frame::output(2, 1, b"x")),
            Ok(None),
            "a closed channel carries nothing"
        );
    }

    /// A detached viewport's stream channel closes; only an open one is
    /// announced closed.
    #[test]
    fn a_detached_streams_channel_closes() {
        let mut egress = Egress::default();
        egress
            .frames(TerminalDomainEvent::StreamOutput {
                stream: StreamId(4),
                data: "x".into(),
                seq: 1,
            })
            .unwrap();
        let closed = egress.close(StreamId(4)).unwrap().expect("closed");
        assert!(matches!(
            closed.decode_json::<Message>(),
            Ok(Message::Push {
                push: Push::OutputClosed { channel: 2 }
            })
        ));
        assert_eq!(egress.close(StreamId(4)), Ok(None));
        assert_eq!(egress.close(StreamId(5)), Ok(None));
    }

    /// Keystrokes ride a raw-byte input channel per session, opened by a call
    /// ahead of the session's first write and closed on request; the bytes on
    /// the wire are the input itself.
    #[test]
    fn input_rides_a_raw_byte_channel_per_session() {
        let mut inputs = Inputs::default();
        let mut rids = 10..;
        let mut next = || rids.next().unwrap();
        let first = inputs.write(&session("a"), 1, b"ls\r", &mut next).unwrap();
        assert_eq!(first.len(), 2);
        match first[0].decode_json::<Message>().unwrap() {
            Message::Call {
                rid: 10,
                request:
                    Request::OpenInput {
                        session: s,
                        channel: 2,
                    },
            } => assert_eq!(s, session("a")),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(first[1].kind, Kind::INPUT);
        assert_eq!(first[1].channel, 2);
        assert_eq!(first[1].input_parts(), Ok((1, &b"ls\r"[..])));

        let again = inputs.write(&session("a"), 2, b"\x03", &mut next).unwrap();
        assert_eq!(
            again,
            [Frame::input(2, 2, b"\x03")],
            "the channel is reused"
        );
        let other = inputs.write(&session("b"), 3, b"x", &mut next).unwrap();
        assert_eq!(other[1].channel, 3);

        assert_eq!(inputs.channel(&session("a")), Some(2));
        match inputs.close(&session("a"), None, &mut next).unwrap() {
            Some(frame) => assert!(matches!(
                frame.decode_json::<Message>(),
                Ok(Message::Call {
                    request: Request::CloseInput { channel: 2 },
                    ..
                })
            )),
            None => panic!("an open channel closes"),
        }
        assert_eq!(inputs.close(&session("a"), None, &mut next), Ok(None));
        let reopened = inputs.write(&session("a"), 4, b"y", &mut next).unwrap();
        assert_eq!(reopened.len(), 2, "a closed session's next write reopens");
        assert_eq!(reopened[1].channel, 4);

        // A late close of the id's earlier channel (a respawned session's old
        // incarnation leaving) leaves the new channel open.
        assert_eq!(inputs.close(&session("a"), Some(2), &mut next), Ok(None));
        assert_eq!(inputs.channel(&session("a")), Some(4));
        assert!(inputs
            .close(&session("a"), Some(4), &mut next)
            .unwrap()
            .is_some());
    }

    /// A connection that has used up its output channels refuses more output
    /// rather than reusing a channel.
    #[test]
    fn a_connection_out_of_output_channels_refuses_more_output() {
        let mut egress = Egress {
            opened: u32::MAX - FIRST_STREAM_CHANNEL,
            ..Egress::default()
        };
        assert!(egress.frames(output("last", "x", 1)).is_ok());
        assert!(egress.frames(output("one-more", "x", 1)).is_err());
        assert!(
            egress.frames(output("last", "y", 2)).is_ok(),
            "an open channel still carries output"
        );
    }

    #[test]
    fn local_mcp_readiness_snapshot_round_trips() {
        let request = Request::LocalMcpSessions {
            sessions: vec![PtySessionId::new("s")],
        };
        let frame = Frame::control(&Message::Call {
            rid: 1,
            request: request.clone(),
        })
        .unwrap();
        assert_eq!(
            frame.decode_json::<Message>().unwrap(),
            Message::Call { rid: 1, request }
        );
    }

    /// An intent without a login PATH keeps its old wire shape, one with
    /// it round-trips, and a field a host does not know (an older sidecar meeting a
    /// newer field) is ignored rather than refusing the spawn.
    #[test]
    fn the_login_path_is_optional_on_the_wire() {
        let mut intent = SpawnIntent {
            remote_launch: Default::default(),
            command: None,
            argv: Some(vec!["/opt/claude".into()]),
            shell: None,
            cwd: "/tmp".into(),
            cols: 80,
            rows: 24,
            shell_integration: false,
            starship_preset: None,
            api_port: None,
            env_remove: Vec::new(),
            env: Vec::new(),
            agent_env: Vec::new(),
            login_path: None,
        };
        let wire = serde_json::to_value(&intent).unwrap();
        assert!(wire.get("login_path").is_none(), "{wire}");
        assert_eq!(serde_json::from_value::<SpawnIntent>(wire).unwrap(), intent);

        intent.login_path = Some("/opt/homebrew/bin:/usr/bin:/bin".into());
        let wire = serde_json::to_value(&intent).unwrap();
        assert_eq!(
            serde_json::from_value::<SpawnIntent>(wire.clone()).unwrap(),
            intent
        );

        let mut unknown = wire;
        unknown
            .as_object_mut()
            .unwrap()
            .insert("a_field_from_a_newer_app".into(), serde_json::json!("x"));
        assert_eq!(
            serde_json::from_value::<SpawnIntent>(unknown).unwrap(),
            intent
        );
    }

    #[test]
    fn calls_and_replies_round_trip() {
        let registration = SessionRegistration {
            session_id: "s1".into(),
            meta: SessionMeta {
                story_id: Some("SW-1".into()),
                workspace_id: None,
                epic_id: None,
            },
            home_cwd: Some(PathBuf::from("/tmp")),
            osc_enabled: true,
            is_pool_member: false,
            scrollback: crate::emulator::DEFAULT_SCROLLBACK,
        };
        let call = Frame::control(&Message::Call {
            rid: 9,
            request: Request::Spawn {
                intent: SpawnIntent {
                    remote_launch: Default::default(),
                    command: Some("true".into()),
                    argv: None,
                    shell: None,
                    cwd: "/tmp".into(),
                    cols: 80,
                    rows: 24,
                    shell_integration: false,
                    starship_preset: None,
                    api_port: None,
                    env_remove: Vec::new(),
                    env: vec![("A".into(), "b".into())],
                    agent_env: Vec::new(),
                    login_path: None,
                },
                registration,
            },
        })
        .unwrap();
        match call.decode_json::<Message>().unwrap() {
            Message::Call {
                rid: 9,
                request:
                    Request::Spawn {
                        intent,
                        registration,
                    },
            } => {
                assert_eq!(intent.command.as_deref(), Some("true"));
                assert_eq!(intent.env, [("A".to_string(), "b".to_string())]);
                assert_eq!(registration.meta.story_id.as_deref(), Some("SW-1"));
            }
            other => panic!("unexpected {other:?}"),
        }
        for result in [
            Ok(Reply::Spawned { pid: Some(4) }),
            Ok(Reply::Flag { value: true }),
            Ok(Reply::Subscribed {
                result: Box::new(SubscribeResult {
                    snapshot: crate::emulator::TerminalSnapshot {
                        cols: 100,
                        rows: 30,
                        seq: 5,
                        resize_gen: 2,
                        scrollback: 1000,
                        vt: "h\u{e9}\r\n\x1b[2;3H".into(),
                    },
                    stream_id: StreamId(3),
                    story_id: Some("SW-1".into()),
                    workspace_id: None,
                    epic_id: None,
                }),
            }),
            Ok(Reply::Text {
                text: TerminalText {
                    text: "one\ntwo".into(),
                    seq: 9,
                    alt_screen: true,
                },
            }),
            Ok(Reply::Dump {
                dump: Box::new(DebugDump {
                    seq: 9,
                    resize_gen: 1,
                    history_lost: false,
                    fg_pgid: Some(42),
                    dump: [("cols".to_string(), 80.into())].into_iter().collect(),
                }),
            }),
            Ok(Reply::Sessions {
                sessions: [session("s1")].into(),
            }),
            Ok(Reply::Pids {
                pids: vec![Some(7), None],
            }),
            Ok(Reply::Counts {
                counts: Some((3, 1)),
            }),
            Ok(Reply::Dirs {
                dirs: Some(vec![PathBuf::from("/tmp/wt")]),
            }),
            Ok(Reply::Cwd { cwd: None }),
            Ok(Reply::Prompt {
                prompt: Box::new(crate::git_prompt::SessionPrompt {
                    cwd: Some(PathBuf::from("/home/me/repo")),
                    home: Some(PathBuf::from("/home/me")),
                    git: Some(crate::git_prompt::GitPromptStatus {
                        branch: Some("main".into()),
                        sha: Some("abc".into()),
                        staged: 1,
                        ahead: 2,
                        ..Default::default()
                    }),
                }),
            }),
            Err("write_pty: session not found: s1".to_string()),
        ] {
            let frame = Frame::control(&Message::Reply {
                rid: 1,
                result: result.clone(),
            })
            .unwrap();
            assert_eq!(
                Ingress::default().accept(&frame),
                Ok(Some(Inbound::Reply { rid: 1, result }))
            );
        }
    }

    /// The emulator reads cross as calls in both text modes.
    #[test]
    fn emulator_reads_round_trip() {
        for request in [
            Request::ReadText {
                session: session("s1"),
                lines: 40,
                mode: TextMode::Normal,
            },
            Request::ReadText {
                session: session("s1"),
                lines: 0,
                mode: TextMode::Screen,
            },
            Request::DebugDump {
                session: session("s1"),
            },
        ] {
            let frame = Frame::control(&Message::Call {
                rid: 4,
                request: request.clone(),
            })
            .unwrap();
            assert_eq!(
                frame.decode_json::<Message>().unwrap(),
                Message::Call { rid: 4, request }
            );
        }
    }

    /// A resize reaches the client after the output before it and before the
    /// output after it, though output rides its own channel.
    #[test]
    fn a_resize_stays_in_order_with_the_stream_output() {
        let mut egress = Egress::default();
        let stream = StreamId(7);
        let resized = TerminalDomainEvent::StreamResized {
            stream,
            at_seq: 3,
            resize_gen: 1,
            cols: 120,
            rows: 40,
        };
        let events = [
            TerminalDomainEvent::StreamOutput {
                stream,
                data: "abc".into(),
                seq: 3,
            },
            resized.clone(),
            TerminalDomainEvent::StreamOutput {
                stream,
                data: "def".into(),
                seq: 6,
            },
        ];
        let mut ingress = Ingress::default();
        let mut received = Vec::new();
        for event in events.clone() {
            for frame in egress.frames(event).unwrap() {
                if let Some(Inbound::Event(event)) = ingress.accept(&frame).unwrap() {
                    received.push(event);
                }
            }
        }
        assert_eq!(received, events);
    }

    #[test]
    fn codesense_only_prepare_keeps_legacy_wire_shape() {
        let request = Request::PrepareReverseMcp {
            session: PtySessionId::new("s"),
            binding_nonce: "a".repeat(64),
            server_ids: codesense_host_proto::reverse_mcp::default_servers(),
            launch: Default::default(),
            remote_entries: Vec::new(),
            agent_startup: None,
        };
        let value = serde_json::to_value(&request).unwrap();
        assert!(!value.to_string().contains("server_ids"));
        let decoded: Request = serde_json::from_value(value).unwrap();
        let Request::PrepareReverseMcp { server_ids, .. } = decoded else {
            panic!("wrong request")
        };
        assert_eq!(server_ids, ["codesense"]);
    }

    /// A native launch crosses as the bare spelling the host has always read:
    /// `"shell"` or the CLI's id, and a shell prepare leaves it out.
    #[test]
    fn native_launches_keep_their_wire_spelling() {
        use codesense_host_proto::agent_cli::{AgentCli, NativeLaunch};
        for (launch, spelling) in [
            (NativeLaunch::Shell, "shell"),
            (NativeLaunch::Cli(AgentCli::Claude), "claude"),
            (NativeLaunch::Cli(AgentCli::Codex), "codex"),
        ] {
            let request = Request::ProbeNativeRuntime {
                cwd: "/w".into(),
                launch,
            };
            let value = serde_json::to_value(&request).unwrap();
            assert_eq!(value["launch"], spelling, "{value}");
            let decoded: Request = serde_json::from_value(value).unwrap();
            assert!(matches!(
                decoded,
                Request::ProbeNativeRuntime { launch: decoded, .. } if decoded == launch
            ));
        }
        let prepare = |launch| Request::PrepareReverseMcp {
            session: PtySessionId::new("s"),
            binding_nonce: "a".repeat(64),
            server_ids: codesense_host_proto::reverse_mcp::default_servers(),
            launch,
            remote_entries: Vec::new(),
            agent_startup: None,
        };
        let shell = serde_json::to_value(prepare(NativeLaunch::Shell)).unwrap();
        assert!(shell.get("launch").is_none(), "{shell}");
        let claude = serde_json::to_value(prepare(NativeLaunch::Cli(AgentCli::Claude))).unwrap();
        assert_eq!(claude["launch"], "claude");
    }

    /// Every layout verb crosses as a call and its answer as a reply, whole —
    /// including each way the host's layout refuses one.
    #[test]
    fn layout_calls_and_replies_round_trip() {
        use crate::domain::ServerId;
        use crate::layout::{EntryState, GridPreset, LAYOUT_SCHEMA};
        let viewer = ViewerId::new("app");
        let workspace = WorkspaceKey::new("root:acme/app");
        let descriptor = WorkspaceDescriptor {
            display_name: "app".into(),
            kind: "root".into(),
            worktree_path: Some(PathBuf::from("/w")),
            ..WorkspaceDescriptor::default()
        };
        let requests = vec![
            Request::PrepareReverseMcp {
                session: PtySessionId::new("s"),
                binding_nonce: "a".repeat(64),
                server_ids: codesense_host_proto::reverse_mcp::default_servers(),
                launch: Default::default(),
                remote_entries: Vec::new(),
                agent_startup: None,
            },
            Request::ReleaseReverseMcp {
                session: PtySessionId::new("s"),
            },
            Request::ClaimWritable {
                session: PtySessionId::new("s"),
            },
            Request::ReconcileTab {
                session: PtySessionId::new("s"),
            },
            Request::ReleaseWritable {
                session: PtySessionId::new("s"),
            },
            Request::LayoutSnapshot {
                viewer: viewer.clone(),
                client_boot_id: ClientBootId::new("boot"),
            },
            Request::ReserveTab {
                request: OpenTab {
                    viewer: viewer.clone(),
                    workspace: workspace.clone(),
                    session: session("s1"),
                    descriptor: Some(descriptor.clone()),
                    name: Some("shell".into()),
                    role: None,
                    tags: [("launcher".to_string(), "mcp".to_string())].into(),
                    placement: Placement::Tab,
                    index: Some(0),
                    activate: true,
                },
            },
            Request::FinishTab {
                session: session("s1"),
                spawned: Err("spawn_pty: no shell".into()),
            },
            Request::BeginRespawn {
                session: session("s1"),
            },
            Request::FinishRespawn {
                session: session("s1"),
                spawned: Ok(()),
            },
            Request::CloseTab {
                viewer: viewer.clone(),
                session: session("s1"),
                mode: CloseMode::KeepIfBusy,
            },
            Request::PlaceTab {
                session: session("s1"),
                workspace: workspace.clone(),
                placement: Placement::Background,
                index: None,
                descriptor: Some(descriptor.clone()),
            },
            Request::RenameTab {
                session: session("s1"),
                name: "build".into(),
            },
            Request::MoveTab {
                session: session("s1"),
                to_index: 2,
            },
            Request::SetActive {
                viewer: viewer.clone(),
                workspace: workspace.clone(),
                session: None,
            },
            Request::SetGrid {
                workspace: workspace.clone(),
                grid: GridState {
                    paned: vec![session("s1")],
                    preset: GridPreset::Rows,
                    collapsed: true,
                },
            },
            Request::SetViewHint {
                viewer: viewer.clone(),
                session: session("s1"),
                hint: ViewHint {
                    scroll_from_bottom: 3,
                },
            },
            Request::SetTags {
                session: session("s1"),
                patch: [("a".to_string(), None)].into(),
            },
            Request::SetDescriptor {
                workspace: workspace.clone(),
                descriptor: descriptor.clone(),
            },
            Request::CloseWorkspace {
                workspace: workspace.clone(),
            },
        ];
        assert_eq!(
            requests.len(),
            20,
            "layout verbs, writable ownership and uncertain reservation reconciliation"
        );
        for request in requests {
            let frame = Frame::control(&Message::Call {
                rid: 2,
                request: request.clone(),
            })
            .unwrap();
            assert_eq!(
                frame.decode_json::<Message>(),
                Ok(Message::Call { rid: 2, request })
            );
        }

        let entry = TabEntry {
            session: session("s1"),
            name: "Terminal 1".into(),
            role: None,
            placement: Placement::Tab,
            state: EntryState::Exited {
                exit_code: Some(2),
                signal: None,
            },
            tags: Default::default(),
        };
        let snapshot = LayoutSnapshot {
            schema: LAYOUT_SCHEMA,
            server_id: ServerId::new("server"),
            server_name: "mac".into(),
            epoch: "epoch".into(),
            rev: 7,
            workspaces: [(workspace.clone(), Default::default())].into(),
            viewer: Default::default(),
        };
        let mut replies = vec![
            Reply::Layout {
                snapshot: Box::new(snapshot),
            },
            Reply::Tab {
                entry: Box::new(entry),
            },
            Reply::Closed {
                outcome: CloseOutcome::Backgrounded,
            },
        ];
        replies.extend(
            [
                LayoutError::UnknownSession(session("s1")),
                LayoutError::UnknownWorkspace(workspace),
                LayoutError::AlreadyPlaced(session("s1")),
                LayoutError::SessionGone(session("s1")),
                LayoutError::InvalidGrid("s2 is not a tab".into()),
                LayoutError::Spawn("spawn_pty: no shell".into()),
                LayoutError::SpawnUnknown("spawn reply lost".into()),
                LayoutError::CwdConflict(WorkspaceKey::new("team|workspace")),
                LayoutError::Unavailable,
                LayoutError::Unreachable("gone".into()),
            ]
            .map(|error| Reply::LayoutRefused { error }),
        );
        for reply in replies {
            let frame = Frame::control(&Message::Reply {
                rid: 3,
                result: Ok(reply.clone()),
            })
            .unwrap();
            assert_eq!(
                Ingress::default().accept(&frame),
                Ok(Some(Inbound::Reply {
                    rid: 3,
                    result: Ok(reply)
                }))
            );
        }
    }

    /// A newer minor's messages do not break the connection: an unreadable
    /// reply answers its caller with an error, an unreadable push is skipped.
    #[test]
    fn a_newer_minors_messages_are_tolerated() {
        let control = |json: &str| Frame {
            kind: Kind::CONTROL,
            channel: 0,
            payload: json.as_bytes().to_vec(),
        };
        let mut ingress = Ingress::default();
        match ingress.accept(&control(
            r#"{"type":"reply","rid":3,"result":{"Ok":{"reply":"added_later"}}}"#,
        )) {
            Ok(Some(Inbound::Reply {
                rid: 3,
                result: Err(message),
            })) => {
                assert!(
                    message.starts_with("unreadable session server reply"),
                    "{message}"
                )
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            ingress.accept(&control(
                r#"{"type":"push","push":{"kind":"output_opened","channel":9,"source":{"kind":"added_later"}}}"#
            )),
            Ok(None)
        );
        assert_eq!(
            ingress.accept(&Frame::output(9, 1, b"x")),
            Ok(None),
            "output on a channel it could not read the announcement of is skipped"
        );
        assert_eq!(
            ingress.accept(&Frame {
                kind: Kind(0x7f),
                channel: 0,
                payload: vec![1]
            }),
            Ok(None),
            "an unknown frame kind is skipped"
        );
    }

    /// A one-shot call skips anything but its own reply.
    #[test]
    fn call_once_reads_its_own_reply() {
        let mut wire = Vec::new();
        for rid in [1, 5] {
            wire.extend(
                Frame::control(&Message::Reply {
                    rid,
                    result: Ok(Reply::Flag { value: rid == 5 }),
                })
                .unwrap()
                .encode(),
            );
        }
        let mut sent = Vec::new();
        assert_eq!(
            call_once(
                &mut wire.as_slice(),
                &mut sent,
                5,
                Request::ReapNonPersisting
            ),
            Ok(Ok(Reply::Flag { value: true }))
        );
        match read_frame(&mut sent.as_slice())
            .unwrap()
            .unwrap()
            .decode_json()
        {
            Ok(Message::Call {
                rid: 5,
                request: Request::ReapNonPersisting,
            }) => {}
            other => panic!("unexpected {other:?}"),
        }
        assert!(call_once(&mut &[][..], &mut Vec::new(), 1, Request::Listen).is_err());
    }

    /// A client hello names its instance and viewer and asks for terminals.
    #[test]
    fn a_client_hello_asks_for_terminals() {
        let hello = client_hello("/root".into(), "1.2.3", &ViewerId::new("v"));
        assert_eq!(hello.proto_major, PROTO_MAJOR);
        assert_eq!(hello.instance_id, "/root");
        assert_eq!(hello.viewer_id.as_deref(), Some("v"));
        assert!(hello.supports(&Capability::Terminals));
    }

    /// The socket, the lock and the identity all derive from the instance
    /// root; two roots never collide.
    #[test]
    fn paths_and_identity_derive_from_the_instance_root() {
        let a = Path::new("/tmp/instance-a");
        let b = Path::new("/tmp/instance-b");
        assert_eq!(socket_path(a), a.join("pty-server.sock"));
        assert_eq!(lock_path(a), a.join("pty-server.lock"));
        assert_ne!(socket_path(a), socket_path(b));
        assert_ne!(instance_id(a), instance_id(b));
    }
}
