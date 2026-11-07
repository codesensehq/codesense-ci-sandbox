// SPDX-License-Identifier: MIT
//! SW-DX-7 / SW-DX-11 — trusted-side registry for dashboard-addressable agent
//! sessions, keyed by an **opaque handle**.
//!
//! The dashboard "action bridge" exposes spawn-class verbs (`openWorkspace`,
//! `runPrompt`, and SW-DX-11's `openContextWorkspace`/`openBranchWorkspace`) and
//! the `sendToSession` verb. This module is the **trusted process's authoritative
//! binding** between a `handle` and the PTY `sessionId` spawned for it, plus the
//! [`SpawnCap`] permit that keeps a dashboard from flooding the machine.
//!
//! **One flat, handle-agnostic registry (SW-DX-11 §2).** A handle is just an
//! opaque string — a `storyId`, an epic's `epicDocPath`, a `team::epic` context
//! key, or a `repo::branch`. There is ONE map ([`by_handle`](Inner::by_handle)),
//! ONE resolver ([`resolve`](DashboardLaunchState::resolve)) — a single lookup,
//! no per-kind branching or ordered walk — and ONE stamp path. Adding a new
//! session kind is one `bind`/`register` call with a fresh non-colliding key;
//! **no resolver or verb change**. Do NOT add per-kind maps or resolvers.
//!
//! Two ways a handle enters the map:
//!  - **Dashboard-launched** (`openWorkspace`/`openContext`/`openBranch`): the
//!    two-step [`begin`](DashboardLaunchState::begin) (reserves a [`SpawnCap`]
//!    slot) → [`bind`](DashboardLaunchState::bind) (stamps the session id). These
//!    consume the supervisory cap.
//!  - **Externally-spawned** (an app-opened epic/context terminal): a single
//!    cap-free [`register`](DashboardLaunchState::register) — the user opened it,
//!    so it is addressable but does NOT count against the dashboard spawn cap.
//!
//! Why it lives in Rust (not the FE broker): the injected dashboard shim is
//! forgeable, so the write target for `sendToSession`/`runPrompt` must **never**
//! be a session id supplied/relayed by the doc. Instead the trusted side stamps
//! `handle → sessionId` here and resolves the fire target from *this* map. A
//! forged session id in a dashboard request cannot redirect the injection
//! (SW-DX-7 AC-10/AC-12; SW-DX-11 preserves this for every handle kind).
//!
//! Concurrency cap (SW-DX-7 AC-8, consuming the SW-DX-6 [`SpawnCap`] primitive):
//! [`begin`](DashboardLaunchState::begin) reserves a permit **before** any
//! worktree/spawn work — `try_acquire` never blocks, so a saturated cap fails
//! fast with `busy`. The permit is **held for the session's whole live-or-parked
//! lifetime** and released only when the session is *explicitly* cleared/killed
//! (via [`release_for_session`], hooked into `pty::kill_session`) — **not** on a
//! natural `pty-exit`. So a session that dies mid-task but is never dismissed
//! **parks holding its slot**: a wall of stuck agents can't be papered over with
//! new launches until the human clears them.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::Manager;

#[cfg(test)]
mod caller_scope_tests;

use super::agent_audit::{self, AuditEvent};
use super::async_helpers::await_blocking;
use super::spawn_cap::{SpawnCap, SpawnCapError, SpawnPermit};
use crate::domain::Principal;
use crate::shared::proc_capture::{
    spawn_capture, CaptureChunk, CaptureOutput, CaptureRequest, CaptureStream,
};
use codesense_pty_core::agent_cli::{InteractiveStructured, TranscriptFormat};

/// Default background-agent limit until the FE pushes the user setting. MUST match
/// the FE `DEFAULT_BACKGROUND_AGENT_LIMIT` (`background-agent-limit.ts`) so the
/// count Rust enforces and the "N of M" the panel shows agree in the brief window
/// before `useBackgroundAgentLimit`'s first push lands (SW-DX-17d review). The
/// limit is human attention — how many unattended agents a person can supervise —
/// not a security control.
pub const DEFAULT_SPAWN_CAP: usize = 6;

/// SW-DX-17a: the two ways a session enters the registry, surfaced verbatim in
/// `dashboard_list_sessions` rows and audit lines. A dashboard-launched session
/// holds a [`SpawnCap`] permit; a `register`ed one is cap-free.
pub const KIND_DASHBOARD_LAUNCHED: &str = "dashboard-launched";
pub const KIND_REGISTERED: &str = "registered";

/// SW-AV-1 (INV-3): a live-PTY session with NO launch-registry entry — the
/// register-on-open ledger silently missed it (a root workspace / notes / git-sync /
/// `#`-named shell / any open path that skipped `registerNavWorkspaceSession`). The
/// union enumerator ([`list_sessions_inner`]) synthesizes one of these rows per
/// registry-less alive session so "if it's running, it shows". It carries no
/// registry metadata; SW-AV-2 ranks it as a bare/utility row and the FE recovers
/// `teamKey`/`workspaceId` from the viewer store.
pub const KIND_LIVE_PTY: &str = "live-pty";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentAccountProvenance {
    pub runtime: String,
    pub provider: String,
    pub source: String,
    pub reason: String,
    pub account_id: Option<String>,
    pub label: Option<String>,
    /// The account's managed CLI home, when the launch ran as one.
    pub home: Option<String>,
}

impl From<super::agent_accounts::LaunchAccountProvenance> for AgentAccountProvenance {
    fn from(value: super::agent_accounts::LaunchAccountProvenance) -> Self {
        Self {
            runtime: value.runtime.as_str().to_owned(),
            provider: value
                .provider
                .map(|provider| provider.as_str().to_owned())
                .unwrap_or_default(),
            source: serde_json::to_value(value.source)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            reason: serde_json::to_value(value.reason)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            account_id: value.account_id,
            label: value.label,
            home: value.home.map(|h| h.to_string_lossy().into_owned()),
        }
    }
}

/// What a run with no terminal never inherits: a terminal host's activity and inbox
/// bindings. It gets its own instead ([`HeadlessActivity`]).
const HEADLESS_HOSTLESS_ENV: [&str; 3] = [
    codesense_pty_core::activity::HOST_ENV,
    codesense_pty_core::activity::SOCKET_ENV,
    codesense_pty_core::activity::TOKEN_ENV,
];

/// A headless run's place on this Mac's in-process host, as a one-shot run: its inbox
/// hooks reach that host, look once for mail the desktop pushed there, and never hold
/// the run open. Removed when dropped (the run ended).
pub(crate) struct HeadlessActivity {
    owner: Arc<codesense_pty_core::activity::ActivityOwner>,
    session: String,
    incarnation: String,
    ended: Arc<AtomicBool>,
}

impl HeadlessActivity {
    /// Reserves `session`'s place and returns the bindings its child runs with. `None`
    /// when its CLI has no waiter (the hooks a headless run's settings carry; a CLI its
    /// host starts turns for runs on no engine here, and its run carries no checks), the
    /// in-process host or its activity service is not installed yet, or the reservation
    /// is refused: its hooks then do nothing, and no mail is pushed for it.
    pub(crate) fn prepare(
        app: &tauri::AppHandle,
        session: &str,
        provider: codesense_pty_core::agent_cli::AgentCli,
    ) -> Option<(Self, Vec<(String, String)>)> {
        if !matches!(
            codesense_pty_core::agent_cli::adapter(provider).inbox(),
            codesense_pty_core::agent_cli::InboxDelivery::Waiter { .. }
        ) {
            return None;
        }
        let host = crate::commands::pty_backend::in_process_host_handle(app)?;
        let service = app.try_state::<codesense_pty_core::activity::ActivityService>()?;
        let (host_env, root) = service.shell_env()?;
        let socket = service.path().to_string_lossy().into_owned();
        let owner = host.activity().clone();
        let (writer, incarnation) = owner.prepare_batch(session, provider, &socket).ok()?;
        let env = vec![
            (host_env.to_string(), root),
            (
                codesense_pty_core::activity::SOCKET_ENV.to_string(),
                writer.socket,
            ),
            (
                codesense_pty_core::activity::TOKEN_ENV.to_string(),
                writer.token,
            ),
        ];
        Some((
            Self {
                owner,
                session: session.to_string(),
                incarnation,
                ended: Arc::new(AtomicBool::new(false)),
            },
            env,
        ))
    }

    /// Commits the run's PID once `spawn_capture` publishes it in `pid`, so its hooks are
    /// admitted; gives up when the run ends first.
    pub(crate) fn commit_when_spawned(&self, pid: Arc<AtomicU32>) {
        let (owner, session, incarnation, ended) = (
            self.owner.clone(),
            self.session.clone(),
            self.incarnation.clone(),
            self.ended.clone(),
        );
        let _ = std::thread::Builder::new()
            .name("headless-activity".into())
            .spawn(move || {
                while !ended.load(Ordering::Acquire) {
                    match pid.load(Ordering::Acquire) {
                        0 => std::thread::sleep(Duration::from_millis(10)),
                        pid => {
                            owner.commit(&session, &incarnation, pid);
                            return;
                        }
                    }
                }
            });
    }
}

impl Drop for HeadlessActivity {
    fn drop(&mut self) {
        self.ended.store(true, Ordering::Release);
        self.owner.remove(&self.session, &self.incarnation);
    }
}

/// SW-OR-10: a **PTY-less** headless run (`run_agent`/`claude -p`), distinct from
/// the two PTY origins above. It holds a [`SpawnCap`] slot like any unattended
/// dashboard launch, but has no child PTY to probe — so its liveness is derived
/// from **registry presence** (see [`list_sessions_inner`]): while the entry is in
/// the map it is `Running`; the run's completion path removes it (the outcome then
/// lives on the managed-session chip, driven by the emitted `mcp:agent-done`).
/// This is the documented, tested exception to the "alive = child liveness"
/// posture the other kinds obey. NOT the SW-GW-7 backgrounded-PTY launcher.
pub const KIND_HEADLESS: &str = "headless";

/// SW-OR-14: the ONE definition of "a headless pseudo-session is alive" — a
/// [`KIND_HEADLESS`] entry with a bound session id (registry presence = liveness,
/// there is no PTY child to probe). Shared by [`list_sessions_inner`] (renders the
/// `Running` row) and [`DashboardLaunchState::has_live_headless_session`] (the MCP
/// gate widening) so the rule can never drift between the two.
pub(crate) fn is_live_headless(kind: &str, session_id: Option<&str>) -> bool {
    kind == KIND_HEADLESS && session_id.is_some()
}

/// SW-OR-10: wall-clock kill ceiling for a headless `run_agent` launch. Far longer
/// than the 60s `analyze` default — a headless *agent* run is a real multi-turn
/// task, not a single-shot probe — while still bounding a wedged child. Longer-term
/// pruning is the SW-AU-46 watchdog's job (by registry presence, not liveness).
const HEADLESS_RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// SW-OR-10: process-local monotonic counter for minting collision-free headless
/// run/session ids (paired with an epoch-ms stamp).
static HEADLESS_SEQ: AtomicU64 = AtomicU64::new(0);

/// SW-OR-10: mint the `(run_id, session_id)` pair for a headless launch. The
/// `headless-` session-id prefix cannot collide with a real PTY session id (those
/// carry the `shell-` prefix from `mint_session_id`), so the synthetic pseudo-session
/// is safely distinguishable everywhere a session id flows.
fn mint_headless_ids() -> (String, String) {
    let n = HEADLESS_SEQ.fetch_add(1, Ordering::Relaxed);
    let ms = now_epoch_ms();
    (
        format!("run-headless-{ms}-{n}"),
        format!("{HEADLESS_SESSION_PREFIX}{ms}-{n}"),
    )
}

const HEADLESS_SESSION_PREFIX: &str = "headless-";

/// Whether `session_id` is a headless launch's pseudo-session rather than a terminal's.
pub(crate) fn is_headless_session_id(session_id: &str) -> bool {
    session_id.starts_with(HEADLESS_SESSION_PREFIX)
}

/// SW-AU-52: the minimal run identity a durable-outcome write needs, resolved from a session by
/// [`DashboardLaunchState::run_context_for_session`].
#[derive(Clone)]
pub(crate) struct RunOutcomeContext {
    run_id: String,
    story_id: String,
    automation_id: Option<String>,
    trigger_source: Option<String>,
    workspace: Option<String>,
    scheduled_at: Option<String>,
    fired_at: Option<String>,
    started_at_ms: Option<u64>,
    /// SW-OR-15: the run's interactive structured-transcript descriptor, read from the live
    /// `LaunchEntry` at ctx-resolve time (write-before-reap). `Some` drives the
    /// interactive teardown capture (resolve+read the agent's session log → `{run_id}.turns.json`);
    /// `None` (a bare shell or an adapter with no interactive capability) leaves the run on the neutral
    /// outcome+summary spine.
    interactive_capture: Option<InteractiveCaptureContext>,
    work_product: Option<crate::commands::sprint_run::run_store::RunWorkProductProvenance>,
    /// SW-AU-96B: the run's managed worktree dir (launch-time), read by the completion path to diff
    /// the work-product branch for the per-file footprint. `None` leaves the run on the branch-level
    /// entity SW-AU-96 already captures.
    worktree_dir: Option<PathBuf>,
    capture_raw_terminal: bool,
    /// The routine this run belongs to, so its ending reaches the routine run.
    routine_key: Option<String>,
    /// The org and team the session was launched under, and the session that does the work.
    org: Option<String>,
    team: Option<String>,
    session_ids: Vec<String>,
    /// The run's process output is not a routine's result text (see
    /// [`TranscriptFormat::result_is_signal_only`]).
    result_signal_only: bool,
}

/// Everything the interactive teardown needs, captured at launch without re-reading the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InteractiveCaptureContext {
    pub(crate) descriptor: InteractiveStructured,
    pub(crate) agent_session_id: Option<String>,
}

impl RunOutcomeContext {
    /// The context of a headless run whose stdout declares `format`.
    pub(crate) fn with_headless_format(mut self, format: Option<TranscriptFormat>) -> Self {
        self.result_signal_only =
            Option::is_some_and(format, TranscriptFormat::result_is_signal_only);
        self
    }

    pub(crate) fn should_capture_raw_terminal(&self) -> bool {
        self.capture_raw_terminal
    }

    /// SW-AU-96B: the per-file footprint capture target for this completion — `Some((worktree_dir,
    /// story_id, run_id))` only when the run captured a work product AND its managed worktree is known;
    /// `None` degrades to the branch-level entity SW-AU-96 shows. The work product and the worktree dir
    /// were co-resolved at launch (`snapshot_story_work_product`) and travel here on separate channels
    /// (durable provenance vs the transient entry field); this re-joins them. Pure, so the completion
    /// gate is unit-tested without I/O.
    fn per_file_footprint_target(&self) -> Option<(PathBuf, String, String)> {
        match (&self.work_product, &self.worktree_dir) {
            (Some(_), Some(worktree_dir)) => Some((
                worktree_dir.clone(),
                self.story_id.clone(),
                self.run_id.clone(),
            )),
            _ => None,
        }
    }
}

pub(crate) async fn capture_raw_snapshot_if_enabled<F, Fut>(
    enabled: bool,
    snapshot: F,
) -> Option<String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    if enabled {
        snapshot().await
    } else {
        None
    }
}

/// The session's terminal as its emulator shows it, as plain text (see
/// [`crate::commands::pty_backend::visible_text`]), for an opted-in capture;
/// `None` (logged) when it cannot be read.
pub(crate) async fn raw_terminal_snapshot(
    app: &tauri::AppHandle,
    session_id: &str,
) -> Option<String> {
    match crate::commands::pty_backend::visible_text(app, session_id).await {
        Ok(text) => Some(text.text),
        Err(error) => {
            log::warn!(target: "terminal::raw_capture", "opted-in raw snapshot skipped: {error}");
            None
        }
    }
}

/// A `signal_done` write for a headless session, begun before the signal is emitted so the
/// process-exit sweep defers to the agent's verdict. `context` is the run identity the durable
/// write needs, captured while the session is still registered.
pub(crate) struct SignalOutcomeGuard {
    pub(crate) context: Option<RunOutcomeContext>,
}

impl SignalOutcomeGuard {
    pub(crate) fn begin(state: &DashboardLaunchState, session_id: &str) -> Option<Self> {
        let (attested, context) = state.begin_signal_outcome(session_id);
        attested.then_some(Self { context })
    }
}

/// Persist a `signal_done`'s durable outcome. A headless session whose signal was attested
/// (`attested`) and a session the registry still holds run context for both write through the
/// shared outcome funnel. A session with no context at all (a visible routine run that survived
/// an app restart) can still end its routine run, found by its session id.
pub(crate) async fn record_signal_outcome<F, Fut>(
    state: Option<&DashboardLaunchState>,
    attested: Option<RunOutcomeContext>,
    session_id: &str,
    status: &str,
    summary: Option<String>,
    snapshot: F,
    mut native_record: Option<codesense_pty_core::agent_cli::output::SessionRecord>,
) where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    use crate::commands::sprint_run::run_store::OutcomeSource;
    let Some(parsed) = crate::dashboard_stream::SessionStatus::from_wire(status) else {
        return;
    };
    close_task_after_capture(state, session_id, status).await;
    if let Some(ctx) = attested {
        let raw_snapshot =
            capture_raw_snapshot_if_enabled(ctx.should_capture_raw_terminal(), snapshot).await;
        DashboardLaunchState::record_captured_outcome_with_destination(
            ctx,
            CapturedOutcomeInput {
                status: parsed,
                summary,
                exit_code: None,
                source: OutcomeSource::Signal,
                label: "signal_done.outcome",
                raw_snapshot,
            },
            CapturedOutcomeDestination::Production,
        )
        .await;
        return;
    }
    if let Some(state) = state {
        // Remote tabs retain their launch request id in layout/output tags. Their
        // transcript survives a tab close without fabricating a story/run outcome.
        if let Some(run_id) = state.remote_output_run_id(session_id) {
            if let Some(record) = native_record.take() {
                crate::commands::sprint_run::run_transcript::persist_session_record(
                    &run_id, record, None,
                )
                .await;
            }
        }
        let recorded = state
            .record_session_outcome_with(
                session_id,
                SessionOutcomeInput {
                    status: status.to_string(),
                    summary: summary.clone(),
                    exit_code: None,
                    source: OutcomeSource::Signal,
                    label: "signal_done.outcome",
                },
                snapshot,
                CapturedOutcomeDestination::Production,
            )
            .await;
        if recorded {
            return;
        }
    }
    let session_id = session_id.to_string();
    let _ = await_blocking("signal_done.routine_session", move || {
        crate::routines::finish::on_signal_without_context(&session_id, parsed, summary);
        Ok::<(), String>(())
    })
    .await;
}

struct SessionOutcomeInput {
    pub status: String,
    pub summary: Option<String>,
    pub exit_code: Option<i32>,
    pub source: crate::commands::sprint_run::run_store::OutcomeSource,
    pub label: &'static str,
}

struct CapturedOutcomeInput {
    status: crate::dashboard_stream::SessionStatus,
    summary: Option<String>,
    exit_code: Option<i32>,
    source: crate::commands::sprint_run::run_store::OutcomeSource,
    label: &'static str,
    raw_snapshot: Option<String>,
}

enum CapturedOutcomeDestination {
    Production,
    #[cfg(test)]
    TestArtifactDir(PathBuf),
}

fn run_outcome_context(entry: &LaunchEntry) -> Option<RunOutcomeContext> {
    let prov = entry.provenance.as_ref()?;
    if prov.run_id.trim().is_empty() || prov.story_id.trim().is_empty() {
        return None;
    }
    Some(RunOutcomeContext {
        run_id: prov.run_id.clone(),
        story_id: prov.story_id.clone(),
        automation_id: prov
            .automation_id
            .clone()
            .filter(|value| !value.trim().is_empty()),
        trigger_source: prov
            .trigger_source
            .clone()
            .filter(|value| !value.trim().is_empty()),
        workspace: prov.workspace.clone(),
        scheduled_at: prov.scheduled_at.clone(),
        fired_at: prov.fired_at.clone(),
        started_at_ms: entry.started_at_ms,
        interactive_capture: entry.interactive_capture.clone(),
        work_product: prov.work_product.clone(),
        worktree_dir: entry.worktree_dir.clone(),
        capture_raw_terminal: entry.capture_raw_terminal,
        routine_key: prov.routine_key.clone(),
        org: entry.owner.as_ref().map(|owner| owner.org.clone()),
        team: entry.owner.as_ref().and_then(|owner| owner.team.clone()),
        session_ids: entry.session_id.iter().cloned().collect(),
        result_signal_only: false,
    })
}

/// SW-AU-52: the write core both completion paths funnel (AC-4, no fork) — given a resolved run
/// identity + parsed status, append the terminal `outcome` to `run_store` off the async runtime
/// (AC-5), best-effort/never-fatal. The signal path resolves the ctx live
/// ([`record_signal_outcome`]); the headless task resolves it at spawn (while
/// provenance is guaranteed present) and passes it here, so a concurrent Stop that tears the slot
/// down early can't erase the durable outcome.
async fn append_run_outcome(
    ctx: RunOutcomeContext,
    status: crate::dashboard_stream::SessionStatus,
    summary: Option<String>,
    exit_code: Option<i32>,
    source: crate::commands::sprint_run::run_store::OutcomeSource,
    label: &'static str,
    // SW-OR-12 (AC-3): the run's persisted-transcript pointer, written before this call. The
    // headless path supplies a real ref; the interactive/signal path (`record_signal_outcome`)
    // has no captured artifact and passes `None` (interactive transcripts are out of scope → OR-15).
    artifact_ref: Option<String>,
) {
    use crate::commands::sprint_run::{run_store, work_product_footprint};
    // SW-AU-96B: when the run captured a work product AND its managed worktree is known, diff that
    // worktree for the per-file footprint alongside the outcome append — the SAME off-thread boundary,
    // best-effort, never on the render path. Resolved (before `ctx` moves into `build_run_outcome`) by
    // the pure, unit-tested gate on the context.
    let per_file_footprint = ctx.per_file_footprint_target();
    let routine_run = ctx.routine_key.is_some().then(|| ctx.run_id.clone());
    let signal_only = ctx.result_signal_only;
    let routine_summary = summary.clone();
    let _ = await_blocking(label, move || {
        // A routine run's ending is the routine's run truth; it is written first so the routine
        // never lags the run store.
        if let Some(run_id) = routine_run {
            crate::routines::finish::on_session_outcome(
                &run_id,
                status,
                routine_summary,
                source,
                signal_only,
            );
        }
        let event = build_run_outcome(
            ctx,
            status,
            summary,
            exit_code,
            source,
            run_store::now_rfc3339(),
            artifact_ref,
        );
        run_store::record_run_outcome(event);
        if let Some((worktree_dir, story_id, run_id)) = per_file_footprint {
            work_product_footprint::capture_per_file_footprint(&worktree_dir, &story_id, &run_id);
        }
        Ok::<(), String>(())
    })
    .await;
}

fn build_run_outcome(
    ctx: RunOutcomeContext,
    status: crate::dashboard_stream::SessionStatus,
    summary: Option<String>,
    exit_code: Option<i32>,
    source: crate::commands::sprint_run::run_store::OutcomeSource,
    ended_at: String,
    artifact_ref: Option<String>,
) -> crate::commands::sprint_run::run_store::RunOutcomeEvent {
    crate::commands::sprint_run::run_store::RunOutcomeEvent {
        story_id: ctx.story_id,
        run_id: ctx.run_id,
        automation_id: ctx.automation_id,
        trigger_source: ctx.trigger_source,
        workspace: ctx.workspace,
        scheduled_at: ctx.scheduled_at,
        fired_at: ctx.fired_at,
        status,
        summary,
        exit_code,
        started_at: ctx
            .started_at_ms
            .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms as i64))
            .map(|dt| dt.to_rfc3339()),
        ended_at,
        artifact_ref,
        validator_step: None,
        structured_declared: ctx.interactive_capture.is_some().then_some(true),
        org: ctx.org,
        team: ctx.team,
        session_ids: ctx.session_ids,
        work_product: ctx.work_product,
        source,
    }
}

/// SW-OR-10 (AC-5): map a captured headless run to the `signal_done` status vocabulary
/// (`success | failed | needs-attention`) + a short human summary — the payload the
/// `mcp:agent-done` emit carries so the managed-session chip renders Done / Errored.
///
/// Unlike `run_agent` (which treats empty stdout as an error — right for `analyze`, WRONG
/// for a headless *agent* whose value is a side effect: a commit, a file, an MCP call),
/// success here is **exit 0 regardless of stdout**. A timeout is `needs-attention` (a
/// human should look); a non-zero exit / signal / spawn failure is `failed`. Pure, so the
/// mapping is unit-tested without a spawn.
pub(crate) fn classify_headless_outcome(
    result: &Result<CaptureOutput, String>,
) -> (&'static str, String) {
    match result {
        // Spawn syscall failure or a `spawn_blocking` panic surfaced by `await_blocking`.
        Err(e) => ("failed", summarize_or(e, "failed to run")),
        // SW-OR-13: a cooperative Stop (pre-spawn or mid-run). Same terminal `failed` chip a
        // `killpg`ed Stop already resolved to, but with a truthful "stopped" summary instead
        // of the generic "exited non-zero" the raw signal-exit would otherwise produce.
        Ok(out) if out.cancelled => ("failed", HEADLESS_STOPPED_SUMMARY.to_string()),
        Ok(out) if out.timed_out => ("needs-attention", "timed out".to_string()),
        // The exit classifier the interactive `pty-exit` shares: only a clean
        // exit succeeds; an abnormal or unknown status is a failure here.
        Ok(out)
            if crate::agent_state::classify_exit_status(out.status.as_ref())
                == crate::agent_state::ExitClass::Clean =>
        {
            let stdout = String::from_utf8_lossy(&out.stdout);
            ("success", summarize_or(&stdout, "completed"))
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            ("failed", summarize_or(&stderr, "agent exited non-zero"))
        }
    }
}

/// The summary of a headless run someone stopped, which its process exit reports as `failed`.
pub(crate) const HEADLESS_STOPPED_SUMMARY: &str = "stopped";

/// SW-OR-10: the clamped first line of `text`, or `fallback` when it is blank — so the
/// chip always carries a non-empty, bounded summary.
fn summarize_or(text: &str, fallback: &str) -> String {
    let s = summarize_headless(text);
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

/// SW-OR-10: clamp a headless run's stdout/error to a short, single-line summary for
/// the `mcp:agent-done` chip — the full artifact is the run's own output, not this
/// supervisory label. Bounded so a large stdout never bloats the UI event.
fn summarize_headless(text: &str) -> String {
    const MAX: usize = 200;
    let one_line = text.split('\n').next().unwrap_or("").trim();
    if one_line.chars().count() > MAX {
        let clamped: String = one_line.chars().take(MAX).collect();
        format!("{clamped}…")
    } else {
        one_line.to_string()
    }
}

/// The status and summary a finished headless run reports. A format whose record names a failure
/// fails a run that exited 0 (`classify_headless_outcome` alone only reads the exit).
///
/// SW-OR-16 (AC-6): when the run emitted a structured format, re-derive the summary from the agent's
/// own conclusion (a sentence) rather than clamping a raw NDJSON line. `classify_headless_outcome` /
/// `summarize_headless` stay the owners — we feed them the derived string and reuse the same
/// first-line/200-char clamp.
/// AC-4: this summary is persisted durably AND emitted to the FE, so it must never carry a raw
/// secret. We redact the FULL capture BEFORE the clamp (not the already-clamped string), so a secret
/// straddling the 200-char boundary can't leave a truncated fragment. Source mirrors
/// `classify_headless_outcome`: a failed run summarizes stderr, otherwise stdout — over REDACTED text.
/// A timeout keeps `classify_headless_outcome`'s clean `"timed out"` label (secret-free by
/// construction) rather than deriving from partial captured output. A spawn error (`result` is
/// `Err`) carries our own message, not captured bytes — scrubbed defensively for a structured run; a
/// non-declaring run keeps its plain summary (OR-12).
pub(crate) fn headless_run_outcome(
    structured_format: Option<TranscriptFormat>,
    result: &Result<CaptureOutput, String>,
) -> (&'static str, String) {
    let (mut status, summary) = classify_headless_outcome(result);
    if let (Some(format), Ok(out)) = (structured_format, result.as_ref()) {
        if status == "success"
            && crate::commands::sprint_run::run_transcript::run_failure(
                format,
                &crate::shared::secret_scan::redact_secrets(&String::from_utf8_lossy(&out.stdout)),
            )
            .is_some()
        {
            status = "failed";
        }
        // A stopped run keeps its own `"stopped"` label, as a timeout keeps `"timed out"`: the
        // routine tells a cancel from a failure by it.
        if status != "needs-attention" && !out.cancelled {
            return (
                status,
                structured_run_summary(format, status, &out.stdout, &out.stderr),
            );
        }
    }
    // A spawn error (`result` is `Err`) carries our own message, not captured bytes — scrub
    // defensively for a structured run; a non-declaring run keeps its plain summary (OR-12).
    (
        status,
        redact_structured_summary(structured_format, summary),
    )
}

/// SW-OR-16 (AC-4): scrub a headless run's outcome `summary` before it is persisted / emitted, when
/// the adapter declared a structured format. Used for the spawn-error case (no captured bytes, our own
/// message) and as the non-structured passthrough. Idempotent. A non-structured run is unchanged.
fn redact_structured_summary(
    structured_format: Option<TranscriptFormat>,
    summary: String,
) -> String {
    if structured_format.is_some() {
        crate::shared::secret_scan::redact_secrets(&summary)
    } else {
        summary
    }
}

/// SW-OR-16 (AC-4/AC-6): derive a structured run's outcome summary. Prefer the agent's `result`-event
/// summary (from redacted stdout); else summarize the redacted capture — stderr on a failed run,
/// stdout otherwise, mirroring [`classify_headless_outcome`]'s source choice. Redaction runs on the
/// FULL capture BEFORE [`summarize_headless`]'s 200-char clamp, so a secret straddling the clamp
/// boundary can't leave a truncated fragment in the summary. Persisted durably + emitted to the FE, so
/// it must be secret-free on every path.
fn structured_run_summary(
    format: TranscriptFormat,
    status: &str,
    stdout: &[u8],
    stderr: &[u8],
) -> String {
    use crate::shared::secret_scan::redact_secrets;
    let redacted_stdout = redact_secrets(&String::from_utf8_lossy(stdout));
    match crate::commands::sprint_run::run_transcript::result_summary(format, &redacted_stdout) {
        // The format decodes the text it summarizes (a JSON-escaped string comes out plain), so the
        // result is scrubbed again: the scan of the raw capture can miss what only the decoded
        // text shows.
        Some(rs) => summarize_headless(&redact_secrets(&rs)),
        None => {
            let src = if status == "failed" {
                redact_secrets(&String::from_utf8_lossy(stderr))
            } else {
                redacted_stdout
            };
            summarize_headless(&src)
        }
    }
}

/// How long after a person's last keystroke in its pane a `maxRuntime` session keeps
/// running at least.
pub(crate) const HUMAN_ACTIVE_GRACE_MS: u64 = 15 * 60 * 1000;

/// The deadline a bounded session gets when a person types into its pane at `now_ms`:
/// at least [`HUMAN_ACTIVE_GRACE_MS`] later, rounded up to the minute so steady typing
/// moves it (and saves it) at most once a minute. `None` when it need not move.
fn deadline_after_human_input(deadline_ms: u64, now_ms: u64) -> Option<u64> {
    const MINUTE_MS: u64 = 60_000;
    let floor = now_ms.saturating_add(HUMAN_ACTIVE_GRACE_MS);
    let rounded = floor.div_ceil(MINUTE_MS).saturating_mul(MINUTE_MS);
    (rounded > deadline_ms).then_some(rounded)
}

/// Wall-clock epoch-ms, for a session's [`LaunchEntry::started_at_ms`] birth
/// anchor (SW-DX-17d AC-2 "elapsed"). Best-effort: a pre-epoch clock reads `0`
/// (the FE then shows no elapsed rather than a negative one).
pub(crate) fn now_epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// SW-GW-8 (AC-5): record an existing entry's launching surface — **first-writer
/// wins**. The `launcher` answers "which surface OPENED this agent", so once set
/// it is never overwritten: a `None` re-stamp (a respawn/rebind that doesn't
/// re-assert provenance) must not erase it, AND a later real launcher that
/// COLLAPSES onto the same handle (e.g. a nav-open of a story a dashboard already
/// launched, or vice-versa) must not clobber the original surface. Shared by
/// `bind` and `register` for existing entries so the two can't drift; a fresh
/// entry sets `launcher` directly at construction (that IS the first write).
fn stamp_launcher(entry: &mut LaunchEntry, launcher: Option<&str>) {
    if entry.launcher.is_none() {
        if let Some(l) = launcher {
            entry.launcher = Some(l.to_string());
        }
    }
}

/// SW-SR-3 (AC-4): the provenance stamp linking a launched worker to the auto-run that
/// dispatched it. `run_id` is mandatory; `automation_id` is reserved (absent for a bare
/// sprint run, set by an SE-7 automation). This is the exact join key SE-7's read-model
/// roll-up keys on (`architecture.md` AD-6/AD-7 `{ automation_id, run_id }`), so SR-4's
/// monitor rolls up into the Automations pane later without re-plumbing dispatch. Carried
/// verbatim onto the [`SessionSnapshot`]/[`SessionRow`] the panel + monitor read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunProvenance {
    /// The dispatching run's id (mandatory — every auto-run worker has one).
    pub run_id: String,
    /// The story the worker is working (the registry handle, echoed for the join key).
    pub story_id: String,
    /// The owning automation (SE-7), when the run was automation-triggered. `None` for a
    /// bare sprint run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    /// The trigger-source category driving the run (`manual`, `headless`, `routine`), from
    /// [`crate::commands::trigger_source`]. `None` for a run with no distinct trigger.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_source: Option<String>,
    /// The owning team, stamped ONLY at a non-story managed launch — the epic-whole worker, whose
    /// handle `_epic/{id}` is not a story_id, so a reader cannot resolve its team from the story.
    /// `None` for a story worker and for any hand-launched / user-terminal session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// The deterministic workspace and trusted trigger timing of a routine run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_product: Option<crate::commands::sprint_run::run_store::RunWorkProductProvenance>,
    /// The routine whose run this is (`<org>/<team>/<id>`); `run_id` is then the routine run's
    /// id, and the run's ending is written back to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routine_key: Option<String>,
    /// The named agent this session runs, as `<key>@<revision>`, for every agent launch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The startup manifest hash of that agent launch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_hash: Option<String>,
    /// The routine's name and task, for a routine run's task record. Never serialized: the
    /// panel's rows do not carry it.
    #[serde(skip)]
    pub routine: Option<crate::session_usage::task_capture::RoutineBrief>,
}

/// Where a launched session belongs: the account signed in when it launched, and the org
/// and team it was launched in. `team` is `None` for a launch made outside any team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionOwner {
    pub principal: Option<String>,
    pub org: String,
    pub team: Option<String>,
}

/// The org and team a launch is made in, for its [`SessionOwner`].
#[derive(Debug, Clone)]
pub(crate) struct LaunchedIn {
    pub org: String,
    pub team: Option<String>,
}

/// Stamp `handle`'s [`SessionOwner`]: launched in `org`/`team` by the account signed in now.
/// A signed-out app stamps no account, so account-scoped matching treats the session as
/// unknown rather than guessing.
pub(crate) async fn stamp_launch_owner(
    app: &tauri::AppHandle,
    launch: &DashboardLaunchState,
    handle: &str,
    org: String,
    team: Option<String>,
) {
    let principal = launching_principal(app).await;
    launch.set_owner_if_absent(
        handle,
        SessionOwner {
            principal,
            org,
            team,
        },
    );
}

/// The account signed in now, as a launch records it; `None` when signed out.
async fn launching_principal(app: &tauri::AppHandle) -> Option<String> {
    super::route_to_context::resolve_ambient(app)
        .await
        .ok()
        .map(|(_, user_id)| Principal::from(user_id).as_str().to_string())
}

/// Record what a frontend launch knew when it bound `handle`: the team it was launched in
/// (a `org:team` team key) by `principal`, and the launch profile it runs. A `fresh` session
/// (the handle's first, or a respawn replacing a superseded one) is stamped as launched; a
/// re-bind of the session already bound only fills gaps, so a later focus never relabels it.
pub(crate) fn stamp_bound_launch(
    launch: &DashboardLaunchState,
    handle: &str,
    fresh: bool,
    principal: Option<String>,
    team_key: Option<&str>,
) {
    let owner = team_key
        .map(super::pty_layout::parse_team_key)
        .filter(|(org, _)| !org.is_empty())
        .map(|(org, team)| SessionOwner {
            principal,
            org: org.to_string(),
            team: team.map(str::to_string),
        });
    if fresh {
        launch.restamp_launch(handle, owner);
        return;
    }
    if let Some(owner) = owner {
        launch.set_owner_if_absent(handle, owner);
    }
}

/// One registry entry: the bound session id (`None` between `begin` and `bind`
/// — the brief mid-launch window) plus an OPTIONAL held [`SpawnPermit`].
///
/// SW-DX-17d decoupled the two axes that were previously conflated:
/// - `kind` — the STORED origin (`dashboard-launched` vs `registered`). It is no
///   longer *derived* from the permit, because `adopt` now toggles the permit
///   without changing where the session came from.
/// - `_permit` — `Some` ⟺ the session is **currently backgrounded (unattended)**,
///   the thing the user-visible background-agent limit counts. A dashboard
///   `begin` acquires one at birth; `adopt` drops it; a cap-free `register`
///   (app-opened terminal) starts `None`.
#[derive(Debug)]
struct LaunchEntry {
    /// The `$CODESENSE_TAB_SESSION_ID` spawned for this handle, once bound.
    session_id: Option<String>,
    /// Whether the trusted request named the story before any inheritance. Never serialized.
    story_was_explicit: bool,
    /// The launcher's task identity, captured under the reservation lock before async startup.
    launcher_task: Option<String>,
    local_effect_owner: Option<super::pty_host::LocalEffectClaim>,
    chat_workspace_id: Option<String>,
    remote_shell: Option<super::pty_layout::RemoteShellRecord>,
    /// `Some` while the session is backgrounded (unattended) — dropping it frees
    /// a limit slot. `None` while attended (foreground tab / just-adopted) or a
    /// cap-free external registration.
    _permit: Option<SpawnPermit>,
    /// STORED origin label (SW-DX-17d) — survives an `adopt` permit-toggle so the
    /// row's `{kind}` degrade label stays stable.
    kind: &'static str,
    /// SW-DX-17d (AC-2): what the session was launched with (the composed skill
    /// line / task), surfaced as the row's "Started with" subtitle. `None` for a
    /// plain shell with no launch prompt → the row degrades to `{kind}`.
    started_with: Option<String>,
    /// SW-DX-17d (AC-2): epoch-ms birth time, anchoring the row's "elapsed". Set
    /// once at entry creation; the FE ticks elapsed = now − this.
    started_at_ms: Option<u64>,
    /// SW-GW-8 (AC-5): the launching SURFACE — the dashboard `docPath` for a
    /// dashboard launch, `"codesense"` for a nav/manual open, `None` for an
    /// origin-less session. Distinct from `kind` (which
    /// is only the `dashboard-launched` vs `registered` axis): `launcher` names
    /// WHICH surface, so the panel row is legible instead of a soup. Stamped at
    /// bind/register; a `None` re-stamp never clobbers a prior value.
    launcher: Option<String>,
    /// Immutable launch-time account identity. It is stamped once at bind and
    /// never joined back to mutable account/profile state.
    account_provenance: Option<AgentAccountProvenance>,
    /// SW-SR-3 (AC-4): the auto-run provenance stamp, set by [`stamp_provenance`] on a
    /// dispatched worker. `None` for a hand-launched / externally-registered session.
    provenance: Option<RunProvenance>,
    /// Durable routine identity, retained independently of a restored run outcome.
    routine_key: Option<String>,
    /// SW-AU-96B: the run's managed worktree dir, stamped at launch while it is guaranteed live.
    /// The completion path reads it to diff the work-product branch for the per-file footprint
    /// (`work_product_footprint`) — an in-memory value, resolved once at launch, never re-derived at
    /// teardown. `None` for a session launched without a managed worktree (a bare shell / external
    /// register); a reaped worktree by completion degrades honestly (best-effort diff).
    worktree_dir: Option<PathBuf>,
    /// The launch profile the session was started with, stamped by every launch that
    /// resolved one. Routine event triggers match a finished agent on it.
    profile_id: Option<String>,
    /// The account that launched the session and the org and team it works in, stamped by
    /// the launches that know them. Routine event triggers are scoped by it.
    owner: Option<SessionOwner>,
    /// SW-OR-10: for a [`KIND_HEADLESS`] entry, the live child's PID slot (published by
    /// `spawn_capture` at spawn; `0` until then). It is the child's pgid (the run spawns
    /// with `process_group: true`), so the Stop / app-exit paths `killpg` it to terminate
    /// the whole agent tree — the ONLY handle to a PTY-less child. `None` for every PTY
    /// kind (there is a real session to kill instead).
    headless_pid: Option<Arc<AtomicU32>>,
    /// SW-OR-13: for a [`KIND_HEADLESS`] entry, the cooperative-cancel flag threaded into
    /// `spawn_capture`. `release_for_session` sets it alongside the `killpg` so a Stop that
    /// lands in the pre-spawn window (before `headless_pid` is published, so no `killpg` can
    /// yet reach the child) still aborts the run at `spawn_capture`'s pre-spawn check. `None`
    /// for every PTY kind.
    headless_cancel: Option<Arc<AtomicBool>>,
    /// SW-OR-15: the launched adapter's interactive structured-transcript descriptor (its session-log
    /// glob, format id, and optional exact id), stashed at launch by
    /// [`set_interactive_capture_if_absent`] and
    /// projected from the agent runtime's adapter. The session-END teardown choke
    /// ([`record_signal_outcome`]) reads it — an in-memory lookup, never an adapter re-read at
    /// teardown — to resolve+read the agent's own session log into `{run_id}.turns.json`. `None` for
    /// a session whose adapter declares no interactive capability (the run stays on the neutral spine).
    interactive_capture: Option<InteractiveCaptureContext>,
    /// Launch-time user consent; never inferred from an adapter or workspace.
    capture_raw_terminal: bool,
    /// SW-OR-19: for a [`KIND_HEADLESS`] entry, the wall-clock epoch-ms of the child's
    /// last drained output (`spawn_capture` writes it per chunk; `0` until first output).
    /// [`list_sessions_inner`] reads it into [`SessionRow::last_output_ms`] to seed the
    /// fleet-row "last output Ns ago" heartbeat; the live emitter re-freshens it between
    /// snapshots. `None` for every PTY kind.
    last_drain_ms: Option<Arc<AtomicU64>>,
    /// How an agent named and attributed this session when it launched it through
    /// `open_workspace`. Written once, on the fresh reserve; empty for every other launch.
    meta: LaunchMeta,
    /// The named agent the session runs (`<key>@<revision>`), stamped by agent launches.
    agent_revision: Option<String>,
    /// Epoch ms the current session was bound to this entry: set when the bound session
    /// changes, never by a re-registration of the same session.
    bound_at_ms: Option<u64>,
    /// Epoch ms the bound session was first observed ready; set once per bound
    /// session, cleared with a rebind.
    ready_at_ms: Option<u64>,
    /// The startup-screen note for the bound session has been delivered.
    startup_noted: bool,
    /// the caller's `maxRuntime` for this launch, in milliseconds.
    /// Preserved verbatim so the deadline sweep can report it in the kill note
    /// (`"exceeded maxRuntime <X>"`). `None` → the launch was not bounded.
    max_runtime_ms: Option<u64>,
    /// epoch-ms wall-clock deadline. Stamped at bind
    /// (`bound_at_ms + max_runtime_ms`) and persisted as `launchDeadlineAt`. The
    /// sweep sends `/exit` once at this threshold, kills after
    /// [`deadline::EXIT_GRACE_MS`], and leaves unbounded entries alone (`None`).
    deadline_ms: Option<u64>,
    /// epoch-ms when the deadline sweep sent `/exit`. `None` while
    /// still before the deadline; set exactly once by `mark_exit_sent`.
    exit_sent_at_ms: Option<u64>,
    restored_overdue: bool,
    /// The agent launch's per-launch settings file, removed when the entry goes.
    launch_settings: Option<crate::agents::launch::LaunchSettingsFile>,
}

fn entry_for_session_mut<'a>(inner: &'a mut Inner, session: &str) -> Option<&'a mut LaunchEntry> {
    let handle = inner.by_session.get(session).cloned().or_else(|| {
        inner
            .by_handle
            .iter()
            .find(|(_, entry)| {
                entry
                    .remote_shell
                    .as_ref()
                    .is_some_and(|record| record.target.session.as_str() == session)
            })
            .map(|(handle, _)| handle.clone())
    })?;
    inner.by_handle.get_mut(&handle)
}

fn deadline_entry(handle: &str, entry: &LaunchEntry) -> Option<deadline::DeadlineEntry> {
    if entry.remote_shell.is_some() {
        return None;
    }
    Some(deadline::DeadlineEntry {
        handle: handle.to_string(),
        session_id: entry.session_id.clone()?,
        launched_by: entry.meta.launched_by.clone(),
        team: entry.meta.team.clone(),
        max_runtime_ms: entry.max_runtime_ms,
        deadline_ms: entry.deadline_ms?,
        exit_sent_at_ms: entry.exit_sent_at_ms,
        restored_overdue: entry.restored_overdue,
        enforced: None,
    })
}

fn current_deadline(
    inner: &Inner,
    expected: &deadline::DeadlineEntry,
) -> Option<deadline::DeadlineEntry> {
    inner
        .pending_deadlines
        .get(&expected.session_id)
        .cloned()
        .or_else(|| {
            inner
                .by_handle
                .get(&expected.handle)
                .and_then(|entry| deadline_entry(&expected.handle, entry))
        })
}

impl LaunchEntry {
    /// A newly bound session starts its bound clock: when it was bound, and not yet
    /// ready or noted.
    fn start_bound_clock(&mut self) {
        self.bound_at_ms = Some(now_epoch_ms());
        self.ready_at_ms = None;
        self.startup_noted = false;
    }

    /// The session the startup rule watches: the bound local session, or the remote
    /// tab's session on its machine.
    fn startup_session(&self) -> Option<String> {
        self.session_id.clone().or_else(|| {
            self.remote_shell
                .as_ref()
                .map(|record| record.target.session.as_str().to_string())
        })
    }

    /// The team its inbox lives in: the one recorded at launch, else a remote tab's
    /// workspace team.
    fn launch_team(&self) -> Option<LaunchTeam> {
        self.meta.team.clone().or_else(|| {
            self.remote_shell
                .as_ref()
                .and_then(|record| launch_team_from_key(&record.team_key))
        })
    }

    /// When the bound clock started: the bind, or for a remote tab its reservation.
    fn bound_clock_ms(&self) -> Option<u64> {
        self.bound_at_ms
            .or_else(|| self.remote_shell.as_ref().and_then(|_| self.started_at_ms))
    }

    /// Whether this entry is a launch of a named agent or a dashboard launch: a session it
    /// holds is never relabelled by a plain registration.
    fn names_an_agent(&self) -> bool {
        self.agent_revision.is_some() || self.kind == KIND_DASHBOARD_LAUNCHED
    }

    /// A cap-free entry for a session the app did not reserve a launch for: an app-opened
    /// terminal, or a launch restored from its tab after an app restart.
    fn registered(
        session_id: &str,
        launcher: Option<&str>,
        owner: Option<super::pty_host::LocalEffectClaim>,
    ) -> Self {
        LaunchEntry {
            chat_workspace_id: None,
            remote_shell: None,
            local_effect_owner: owner,
            session_id: Some(session_id.to_string()),
            launcher_task: None,
            story_was_explicit: false,
            _permit: None, // cap-free — the user opened this, not a dashboard.
            kind: KIND_REGISTERED,
            started_with: None,
            started_at_ms: Some(now_epoch_ms()),
            launcher: launcher.map(str::to_string),
            account_provenance: None,
            provenance: None,
            routine_key: None,
            worktree_dir: None,
            profile_id: None,
            owner: None,
            headless_pid: None,
            headless_cancel: None,
            interactive_capture: None,
            capture_raw_terminal: false,
            last_drain_ms: None,
            meta: LaunchMeta::default(),
            agent_revision: None,
            bound_at_ms: Some(now_epoch_ms()),
            ready_at_ms: None,
            startup_noted: false,
            max_runtime_ms: None,
            deadline_ms: None,
            exit_sent_at_ms: None,
            restored_overdue: false,
            launch_settings: None,
        }
    }
}

/// A launch's display identity and its effective requested queue role. Actual holders
/// come from the session-linked role registrations, never these labels.
/// The launcher context is captured at launch and used only when explicitly requested.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchMeta {
    pub launcher_context: Option<String>,
    /// "Sprint 1", or the story id for a story launch.
    pub display_name: Option<String>,
    /// The launcher's handle, or its raw session id when it has no handle. `None` when a
    /// person launched it.
    pub launched_by: Option<String>,
    pub story: Option<String>,
    /// The org and team the session was launched in, where its inbox lives. Not on the
    /// wire.
    #[serde(skip)]
    pub team: Option<LaunchTeam>,
    /// The role the launcher gave the session (`manager`, `worker` or any
    /// free label such as `reviewer`). Display only.
    pub role: Option<String>,
    /// The effective queue requested for this launch; membership is read from registrations.
    pub queue_role: Option<String>,
    pub role_join_error: Option<String>,
    /// The avatar the launcher gave the session (a built-in avatar id, or a colour or
    /// emoji hint). Display only.
    pub avatar: Option<String>,
    /// The session this launch continues (a restore relaunch's predecessor): its task links
    /// `relates_to:continues` to that session's. Not on the wire.
    #[serde(skip)]
    pub continues: Option<String>,
    /// The task a reviewer launch reviews, as `open_workspace` named it. Not on the wire.
    #[serde(skip)]
    pub reviews: Option<String>,
}

/// An org and one of its teams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct LaunchTeam {
    pub org: String,
    pub team: String,
}

/// The team a viewer team key (`org:team`, split at the first colon) names; `None` for a
/// key with no team (`org:_none`) or a malformed one.
pub(crate) fn launch_team_from_key(key: &str) -> Option<LaunchTeam> {
    let (org, team) = key.split_once(':')?;
    (!org.is_empty() && !team.is_empty() && team != "_none").then(|| LaunchTeam {
        org: org.to_string(),
        team: team.to_string(),
    })
}

/// A Unicode format character (category Cf: bidi controls, zero-width marks, tag
/// characters) that would hide or reorder text. The zero-width joiner stays: it joins
/// emoji sequences.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{0600}'..='\u{0605}'
            | '\u{061C}'
            | '\u{06DD}'
            | '\u{070F}'
            | '\u{0890}'..='\u{0891}'
            | '\u{08E2}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200C}'
            | '\u{200E}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{110BD}'
            | '\u{110CD}'
            | '\u{13430}'..='\u{1343F}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

/// The longest role or avatar label a launch records, in characters.
pub(crate) const LAUNCH_LABEL_MAX_CHARS: usize = 64;

/// A caller-supplied role or avatar as a launch records it: control and invisible format
/// characters removed, trimmed and cut to [`LAUNCH_LABEL_MAX_CHARS`]; `None` when nothing
/// is left. Never a refusal.
pub(crate) fn launch_label(value: Option<&str>) -> Option<String> {
    let cleaned: String = value?
        .chars()
        .filter(|&c| !c.is_control() && !is_invisible_format(c))
        .collect();
    let label: String = cleaned
        .trim()
        .chars()
        .take(LAUNCH_LABEL_MAX_CHARS)
        .collect();
    let label = label.trim_end();
    (!label.is_empty()).then(|| label.to_string())
}

/// What a caller hands [`DashboardLaunchState::begin_with_provenance`] to record on a
/// fresh reserve: the display [`LaunchMeta`] (its `launched_by` still raw) and the
/// "Started with" line.
#[derive(Debug, Clone, Default)]
pub(crate) struct LaunchRecord {
    pub meta: LaunchMeta,
    pub started_with: Option<String>,
    /// the caller's per-launch max-runtime budget, in milliseconds.
    /// `None` → no deadline is stamped at bind and the launch is left alone by
    /// the deadline sweep. Set at `open_workspace`'s call site; UI launches
    /// leave it empty (the UI sheet does not expose it today).
    pub max_runtime_ms: Option<u64>,
}

/// A reported launcher id as the registry records it: the handle its session is bound
/// to, else the raw id (a tab with no handle). A session id and its handle therefore
/// name the same launcher.
fn normalize_launcher(inner: &Inner, raw: &str) -> String {
    inner
        .by_session
        .get(raw)
        .cloned()
        .unwrap_or_else(|| raw.to_string())
}

#[derive(Debug, Default)]
struct Inner {
    /// `handle → LaunchEntry` — the ONE flat map (SW-DX-11 §2). The handle is an
    /// opaque string (storyId / epicDocPath / `team::epic` / `repo::branch`).
    by_handle: HashMap<String, LaunchEntry>,
    /// `sessionId → handle` reverse index, so a session-keyed release (from
    /// `kill_session`) can find and drop the owning handle's entry in O(1).
    by_session: HashMap<String, String>,
    /// SW-OR-14 (AC-3): headless session ids that SELF-attested via `signal_done`. Held
    /// SEPARATELY from the entry (not a `LaunchEntry` field) SO IT SURVIVES entry removal —
    /// the completion task must observe "did the agent signal?" regardless of WHICH teardown
    /// path (natural, Stop, cap-reclaim, app-exit) reaped the entry first. If the flag rode on
    /// the entry, a Stop that consumed the entry before `run_headless_to_completion`'s own
    /// release would drop it, and the exit classifier would clobber the agent's `Signal`
    /// outcome (Signal > ProcessExit defeated). `run_headless_to_completion` drains its own id
    /// via [`take_signal_attested`](DashboardLaunchState::take_signal_attested), so the set holds
    /// at most one id per in-flight self-attested headless run.
    attested: HashSet<String>,
    /// Headless children whose capture has returned but whose artifact/outcome teardown is still
    /// running. Registry provenance remains available; liveness does not.
    headless_exited: HashSet<String>,
    /// Linearization guard: raw capture entitlement is consumed before snapshot/write.
    raw_capture_taken: HashSet<String>,
    /// a per-session, one-shot reason string the next Kill audit
    /// line picks up. The deadline sweep sets `maxRuntime` here before calling
    /// `pty::kill_session`; `settle_terminal_cleanup` drains it when writing
    /// the `Kill` line. First-writer wins — a concurrent kill reason cannot
    /// clobber the stamped one.
    kill_reasons: HashMap<String, &'static str>,
    /// Deadline work survives removal of a session during its exit grace.
    pending_deadlines: HashMap<String, deadline::DeadlineEntry>,
    /// What a frontend agent launch prepared for its terminal session before the session is
    /// bound, keyed by session id. Bind moves it onto the session's entry; a session torn
    /// down unbound drops it.
    staged_launches: HashMap<String, StagedAgentLaunch>,
    /// SW-XS-1 (X3): remote launches whose caller was answered before they settled,
    /// by `(owner, team, machine, request id)`: still running, or what they ended
    /// as, with when that was recorded (entries expire after
    /// [`LAUNCH_SETTLEMENT_TTL`]).
    ///
    /// [`LAUNCH_SETTLEMENT_TTL`]: super::pty_layout::LAUNCH_SETTLEMENT_TTL
    remote_settlements:
        HashMap<SettlementKey, (super::pty_layout::LaunchSettlement, std::time::Instant)>,
}

/// A frontend agent launch prepared for a session that is not bound yet: the agent revision
/// it runs and its per-launch settings file.
#[derive(Debug)]
pub(crate) struct StagedAgentLaunch {
    pub agent_revision: String,
    pub queue_role: Option<String>,
    pub settings: Option<crate::agents::launch::LaunchSettingsFile>,
    /// The launching user consented to a raw terminal snapshot of this interactive launch.
    pub capture_raw_terminal: bool,
    /// The dir the agent is launched in, recorded on the session (SW-AGR-2).
    pub cwd: Option<std::path::PathBuf>,
    /// The agent CLI binary the launch resolved, recorded on the bind's telemetry.
    pub binary: Option<super::deps::ResolvedAgentBinary>,
    /// What preparation knew about the launch, for its session record at bind.
    pub facts: Option<crate::session_usage::record::LaunchFacts>,
}

/// How long a layout read that found no team for a handle stands before it is read again.
const TAB_TEAM_MISS_TTL: std::time::Duration = std::time::Duration::from_secs(3);

/// What this Mac's terminal layout said about a handle's team (see
/// [`DashboardLaunchState::known_tab_team`]).
#[derive(Debug, Clone)]
enum TabTeam {
    /// Its tab's workspace team, kept from the first read that named it.
    Kept(LaunchTeam),
    /// The read at this time found no tab for it.
    Missed(std::time::Instant),
}

/// Managed Tauri state: the [`SpawnCap`] plus the `handle ↔ sessionId` registry.
#[derive(Debug)]
pub struct DashboardLaunchState {
    cap: SpawnCap,
    inner: std::sync::Arc<Mutex<Inner>>,
    /// The last agent state each session's dot showed (`list_sessions`), fed by every dot
    /// write: admitted hooks and MCP signals. Held here so the release choke drops a
    /// session's entry with its binding.
    turns: crate::agent_state::SessionTurnState,
    /// What this app knows of its tabs' inbox waiters, on this Mac and its machines.
    host_inboxes: std::sync::Arc<super::host_inbox::HostInboxes>,
    /// The team of each handle with no launch team (a tab opened by hand), as this Mac's
    /// layout named it: kept from the first read that did, so its inbox stays in one
    /// team's store; a read that found none stands briefly, so it is not read again at
    /// once.
    tab_teams: Mutex<HashMap<String, TabTeam>>,
    /// Restoring the launches a reused session server kept alive, once per app process.
    local_restore: local_restore::RestoreOnce,
    /// Serializes launch-tag writes, so the last write always carries the latest entry.
    tag_writes: Mutex<()>,
    /// SW-FX-launcher-persistence: the on-disk launcher-provenance mirror. `None`
    /// until the signed-in principal is known and [`Self::install_persistence`] is
    /// called; writes and reads before then are no-ops (safe degradation to the
    /// pre-fix behavior). See [`persistence::PersistedLaunch`].
    persistence: std::sync::Arc<Mutex<Option<persistence::Store>>>,
    /// Handles whose record changed in memory and is owed to disk. Every mutation
    /// that should reach the registry file marks its handle here after dropping
    /// `inner`; the one writer ([`Self::flush_dirty`]) reads the handle's CURRENT
    /// entry when it writes, so a write can never put back an older state.
    ///
    /// Lock order: `persistence` → `inner` → (nothing); `dirty` is a leaf taken alone.
    /// Only the writer holds `persistence` while it takes `inner`; no path takes
    /// `persistence` while holding `inner`.
    dirty: std::sync::Arc<Mutex<HashSet<String>>>,
    /// Wakes the writer thread, started on the first mutation.
    writer: Mutex<Option<std::sync::mpsc::Sender<WriterMsg>>>,
    /// Only completion persistence waits on an in-flight launch capture; launching stays detached.
    task_captures: std::sync::Arc<Mutex<HashMap<String, (tokio::sync::watch::Receiver<()>, String)>>>,
    #[cfg(test)]
    task_capture_gate: Mutex<Option<tokio::sync::watch::Receiver<()>>>,
    #[cfg(test)]
    task_store_path: Mutex<Option<PathBuf>>,
    #[cfg(test)]
    async_persist: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    persist_hook: std::sync::Arc<TestPersistHook>,
}

/// What wakes the registry writer thread.
#[derive(Debug)]
enum WriterMsg {
    /// Handles were marked dirty.
    Wake,
    /// Answer once everything marked before this message is on disk.
    Flush(std::sync::mpsc::Sender<()>),
}

/// Test seam: runs inside a registry write, after the snapshot it writes was read.
#[cfg(test)]
#[derive(Default)]
struct TestPersistHook(Mutex<Option<std::sync::Arc<dyn Fn() + Send + Sync>>>);

#[cfg(test)]
impl TestPersistHook {
    fn run(&self) {
        let hook = self.0.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

#[cfg(test)]
impl std::fmt::Debug for TestPersistHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TestPersistHook")
    }
}

/// SW-DX-17a: what `release_for_session` dropped — the owning handle and its
/// kind — so the kill choke can write one audit line for a real agent session
/// (and skip the audit entirely when `None`, i.e. a non-registered terminal).
#[derive(Debug, Clone)]
pub struct ReleasedSession {
    pub handle: String,
    pub kind: &'static str,
}

/// The stop list for one run's workers, produced by [`DashboardLaunchState::drain_run`]:
/// its workers split into the two teardown paths [`teardown_stop_targets`] runs AFTER the
/// registry lock is dropped:
/// - `to_kill` — the bound session ids, routed through `pty::kill_session` (the
///   SAME choke `dashboard_kill_by_handle` uses → `release_for_session` frees the
///   slot + writes the Kill audit line).
/// - `aborted` — the handles of reserved-but-unbound entries already dropped in
///   `drain_run` (no PTY to kill); the teardown writes their Abort audit
///   lines (audit stays in the command layer, consistent with the per-agent Kill).
///
/// How many it stopped (`to_kill.len() + aborted.len()`) stays on the trusted side: a
/// run's kill falls back to killing the run's own session when it stopped none.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RunStopTargets {
    pub to_kill: Vec<String>,
    pub aborted: Vec<String>,
}

/// SW-DX-17a: one registry entry as snapshotted for enumeration, BEFORE the
/// liveness probe. `session_id` is `None` in the brief `begin`→`bind` window
/// (a reserved-but-unspawned launch). `kind` is [`KIND_DASHBOARD_LAUNCHED`] or
/// [`KIND_REGISTERED`]. Internal — the command maps this to a serialized
/// [`SessionRow`] once `alive` is known.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    /// Native output artifact identity, independent of story provenance and tab retention.
    pub output_run_id: Option<String>,
    pub handle: String,
    pub session_id: Option<String>,
    pub launcher_task: Option<String>,
    story_was_explicit: bool,
    pub kind: &'static str,
    /// SW-DX-17d: `true` while the session is backgrounded (holds a limit slot).
    pub backgrounded: bool,
    pub started_with: Option<String>,
    pub started_at_ms: Option<u64>,
    /// SW-GW-8 (AC-5): the launching surface (dashboard `docPath` / `"codesense"`).
    pub launcher: Option<String>,
    pub account_provenance: Option<AgentAccountProvenance>,
    /// SW-SR-3 (AC-4): the auto-run provenance stamp, when this worker was dispatched.
    pub provenance: Option<RunProvenance>,
    /// SW-OR-19: a headless run's last-output epoch-ms (read from `last_drain_ms`), or
    /// `None` for a non-headless / no-output-yet row. Seeds the fleet-row heartbeat.
    pub last_output_ms: Option<u64>,
    pub meta: LaunchMeta,
    /// The managed dir the session was launched in, when the launch spine recorded one.
    pub worktree_dir: Option<PathBuf>,
    pub agent_revision: Option<String>,
    /// epoch-ms deadline, or `None` for an unbounded launch.
    pub deadline_at_ms: Option<u64>,
    /// epoch-ms when the sweep sent `/exit`, or `None` while before
    /// the deadline. The caller derives [`SessionRow::expiring`] from this plus
    /// the grace window; a stand-alone field keeps that derivation pure.
    pub exit_sent_at_ms: Option<u64>,
    /// The machine a remote launch runs on, by id; `None` on this Mac.
    pub machine_id: Option<String>,
    /// The team a remote launch was made in (its record's team key), whose machine list
    /// names its machine; `None` on this Mac.
    pub launch_team_key: Option<String>,
}

/// SW-BA-1 (AC-1/AC-3): the ONE agent-lifecycle status vocabulary, serialized to
/// the FE as `running|paused|done|errored` (camelCase). Mirrored by the TS
/// `AgentStatus` type; the SE-6 panel and SE-5 header both render the SAME chip
/// from it. Distinct from `kind` (origin) and `alive` (raw liveness) — this is the
/// derived supervisory signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentStatus {
    Running,
    Paused,
    Done,
    Errored,
}

/// SW-BA-1 (AC-2): derive the lifecycle status from liveness + binding + gate —
/// the ONE trusted-side authority (the FE `deriveAgentStatus` mirror asserts the
/// identical permutation table, so the two can't drift):
///
/// | `session_id` | `alive` | `gate_suspended` | → status |
/// |--------------|---------|------------------|----------|
/// | `None`       | —       | —                | errored  |  reserved-but-unbound leak
/// | `Some`       | true    | false            | running  |
/// | `Some`       | true    | true             | paused   |
/// | `Some`       | false   | —                | done     |
///
/// Per OQ-1 no child exit code is retained (`PtyState` only knows alive/dead), so
/// a crash degrades honestly to `done` — never guessed. The only `errored` the
/// registry can express today is the reserved-but-unbound launch (a `begin` that
/// never `bind`ed); the slot exists for a follow-up to light up on real crash
/// detection. `gate_suspended` defaults `false` until SE-5 `SW-OR-5` (OQ-2).
pub fn derive_status(alive: bool, session_id: Option<&str>, gate_suspended: bool) -> AgentStatus {
    match (session_id, alive, gate_suspended) {
        (None, _, _) => AgentStatus::Errored,
        (Some(_), true, false) => AgentStatus::Running,
        (Some(_), true, true) => AgentStatus::Paused,
        (Some(_), false, _) => AgentStatus::Done,
    }
}

/// SW-DX-17a: the serialized "Running agents" row the FE panel renders. `alive`
/// reflects real child-process liveness (see `dashboard_list_sessions`): a
/// naturally-exited session is retained in `PtyState` for replay yet reads
/// `alive: false`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    /// Native output artifact identity, independent of story provenance and tab retention.
    pub output_run_id: Option<String>,
    pub handle: String,
    pub session_id: Option<String>,
    pub kind: String,
    pub alive: bool,
    /// SW-BA-1 (AC-3): the derived lifecycle status — the authoritative supervisory
    /// signal (see [`derive_status`]). `alive` is retained alongside it for
    /// back-compat during the migration.
    pub status: AgentStatus,
    /// SW-DX-17d (AC-3): `true` while backgrounded (unattended) — the FE tallies
    /// these for the panel header's "N of M running unattended". An adopted
    /// (attended) session reads `false` and does NOT count toward the limit.
    pub backgrounded: bool,
    /// SW-DX-17d (AC-2): the "Started with" task/prompt, or `None` → `{kind}`.
    pub started_with: Option<String>,
    /// SW-DX-17d (AC-2): epoch-ms birth anchor for the FE-computed "elapsed".
    pub started_at_ms: Option<u64>,
    /// SW-GW-8 (AC-5): the launching surface (dashboard `docPath` / `"codesense"`),
    /// or `None` for an origin-less background. Serialized as `launcher`.
    pub launcher: Option<String>,
    pub account_provenance: Option<AgentAccountProvenance>,
    /// SW-SR-3 (AC-4): the auto-run provenance stamp `{ runId, storyId, automationId? }`,
    /// or `None` for a hand-launched session. SR-4's monitor rolls up on this key.
    pub provenance: Option<RunProvenance>,
    /// SW-OR-19: a live headless run's last-output epoch-ms — the FE computes "last output
    /// Ns ago" against its local clock. `None` for non-headless / no-output-yet rows; the
    /// heartbeat event freshens it between snapshots.
    pub last_output_ms: Option<u64>,
    /// `displayName` / `launchedBy` / `story` for a session an agent launched through
    /// `open_workspace`; all null otherwise.
    #[serde(flatten)]
    pub meta: LaunchMeta,
    /// The managed dir the session runs in, when known.
    pub workspace: Option<String>,
    pub chat_workspace_id: Option<String>,
    pub group_unread: u32,
    pub group_read_id: i64,
    /// The named agent the session runs, as `<key>@<revision>`.
    pub agent_revision: Option<String>,
    /// epoch-ms deadline the deadline sweep enforces on a bounded
    /// local launch. `None` for an unbounded or remote row; the row disappears
    /// after the sweep's kill releases the entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_at_ms: Option<u64>,
    /// `true` only during the ~30s grace window between the sweep's
    /// `/exit` and its kill. `false` by default; serialized only when set.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub expiring: bool,
    /// The machine the session runs on, by id, from its launch record: known before
    /// the machine's layout shows its tab. Always serialized: `None` (null) on this Mac,
    /// as every row this command lists is known to be local or remote.
    pub machine_id: Option<String>,
    /// The team a remote launch was made in, whose machine list names its machine, as
    /// `<org>:<team>`. Always serialized: `None` (null) on this Mac.
    pub launch_team_key: Option<String>,
}

/// SW-SR-5 (AC-2): the run→worker join predicate [`DashboardLaunchState::drain_run`] matches
/// on — an entry belongs to `run_id` iff its provenance stamp carries that id.
fn provenance_run_id_matches(entry: &LaunchEntry, run_id: &str) -> bool {
    entry
        .provenance
        .as_ref()
        .is_some_and(|p| p.run_id == run_id)
}

/// Fields projected out of a [`LaunchEntry`] (which is `!Clone`) so the registry
/// writer can read an entry under the inner-registry mutex, drop it, and write.
///
/// Lock order, everywhere: the persistence mutex may be held while taking the
/// inner mutex (the writer, [`DashboardLaunchState::flush_dirty`]), never the other
/// way round. Code holding the inner mutex only marks a handle dirty after
/// dropping it ([`DashboardLaunchState::queue_persist`]).
#[derive(Debug, Clone)]
struct PersistSnapshot {
    launcher_context: Option<String>,
    launched_by: Option<String>,
    team: Option<LaunchTeam>,
    started_at_ms: Option<u64>,
    /// deadline epoch-ms, mirrored into the on-disk record so a
    /// post-restart restore can re-arm the sweep when the launch tag is lost.
    deadline_ms: Option<u64>,
    session_id: Option<String>,
    worktree_dir: Option<PathBuf>,
}

impl PersistSnapshot {
    fn from_entry(entry: &LaunchEntry) -> Self {
        Self {
            launcher_context: entry.meta.launcher_context.clone(),
            launched_by: entry.meta.launched_by.clone(),
            team: entry.meta.team.clone(),
            started_at_ms: entry.started_at_ms,
            deadline_ms: entry.deadline_ms,
            session_id: entry.session_id.clone(),
            worktree_dir: entry.worktree_dir.clone(),
        }
    }
}

impl DashboardLaunchState {
    /// Create the registry with a cap of `n` concurrent spawn-class sessions.
    /// Launcher-provenance persistence is OFF until
    /// [`install_persistence`](Self::install_persistence) wires it to a per-principal path.
    pub fn new(n: usize) -> Self {
        DashboardLaunchState {
            cap: SpawnCap::new(n),
            inner: std::sync::Arc::new(Mutex::new(Inner::default())),
            turns: crate::agent_state::SessionTurnState::default(),
            host_inboxes: std::sync::Arc::default(),
            tab_teams: Mutex::default(),
            local_restore: local_restore::RestoreOnce::default(),
            tag_writes: Mutex::new(()),
            persistence: std::sync::Arc::new(Mutex::new(None)),
            dirty: std::sync::Arc::default(),
            writer: Mutex::new(None),
            task_captures: std::sync::Arc::default(),
            #[cfg(test)]
            task_capture_gate: Mutex::new(None),
            #[cfg(test)]
            task_store_path: Mutex::new(None),
            #[cfg(test)]
            async_persist: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            persist_hook: std::sync::Arc::default(),
        }
    }

    fn begin_task_capture(&self, session_id: &str, task_id: &str) -> tokio::sync::watch::Sender<()> {
        let (done, wait) = tokio::sync::watch::channel(());
        self.task_captures.lock().unwrap().insert(session_id.to_string(), (wait, task_id.to_string()));
        done
    }

    async fn wait_for_task_capture(&self, session_id: &str) {
        let pending = self.task_captures.lock().unwrap().get(session_id).map(|(wait, _)| wait.clone());
        if let Some(mut pending) = pending {
            // Sender drop means capture finished, including a capture that failed. A stuck
            // capture must never keep signal_done or headless teardown waiting indefinitely.
            if tokio::time::timeout(std::time::Duration::from_secs(2), pending.changed()).await.is_err() {
                log::warn!(target: "session_usage", "task capture for {session_id} is still pending at close");
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn set_task_store_path_for_test(&self, path: PathBuf) {
        *self.task_store_path.lock().unwrap() = Some(path);
    }

    #[cfg(test)]
    fn task_store_path(&self) -> Result<PathBuf, String> {
        task_store_path(Some(self))
    }

    /// Writes leave the calling thread (as in production) instead of running inline.
    #[cfg(test)]
    pub(crate) fn set_async_persist_for_test(&self, on: bool) {
        self.async_persist
            .store(on, std::sync::atomic::Ordering::SeqCst);
    }

    /// `hook` runs inside each registry write, once the snapshot it writes was read.
    #[cfg(test)]
    pub(crate) fn set_persist_hook_for_test(&self, hook: impl Fn() + Send + Sync + 'static) {
        *self.persist_hook.0.lock().unwrap() = Some(std::sync::Arc::new(hook));
    }

    /// Holds the registry-write mutex, standing in for a slow disk.
    #[cfg(test)]
    pub(crate) fn hold_persistence_for_test(&self) -> impl Sized + '_ {
        self.persistence.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// SW-FX-launcher-persistence: wire the on-disk launcher-provenance file for
    /// the signed-in principal and hydrate the in-memory snapshot from it.
    ///
    /// Idempotent — a second call for the SAME path is a no-op; a call with a
    /// DIFFERENT path replaces the store (a principal switch mid-process starts
    /// fresh against the new user's file).
    ///
    /// Hydration never fails the caller: a missing / unreadable / unversioned
    /// file is logged by the loader and treated as empty, so an on-disk failure
    /// never prevents launches.
    pub(crate) fn install_persistence(&self, path: PathBuf) {
        let mut guard = self.persistence.lock().unwrap_or_else(|e| e.into_inner());
        if guard.as_ref().is_some_and(|store| store.path() == path) {
            return;
        }
        let registry = persistence::load_registry(&path);
        let count = registry.launches.len();
        *guard = Some(persistence::Store::new(path.clone(), registry));
        log::info!(
            target: "agents",
            "launch registry hydrated from {} ({count} launch{})",
            path.display(),
            if count == 1 { "" } else { "es" },
        );
    }

    pub(crate) fn launcher_context(&self, session: &str) -> Option<String> {
        let inner = self.lock();
        let handle = inner
            .by_session
            .get(session)
            .map_or(session, String::as_str);
        inner.by_handle.get(handle)?.meta.launcher_context.clone()
    }

    pub(crate) fn context_of_launcher(
        &self,
        home: &Path,
        launcher: Option<&str>,
    ) -> Option<String> {
        let launcher = launcher?;
        let cwd = self
            .snapshot()
            .into_iter()
            .find(|r| r.handle == launcher || r.session_id.as_deref() == Some(launcher))?
            .worktree_dir?;
        crate::install::worktree_index::context_for_cwd(home, &cwd).map(|r| r.id)
    }

    /// The persisted `launched_by` for `handle`, when persistence is on and the
    /// handle was recorded in a prior app run. Looked up by the reconnect path so
    /// a session reappearing after a restart carries the launcher relationship it
    /// had before (step 3 of SW-FX-launcher-persistence).
    pub(crate) fn persisted_launched_by(&self, handle: &str) -> Option<String> {
        let guard = self.persistence.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .as_ref()?
            .get(handle)
            .and_then(|p| p.launched_by.clone())
    }

    fn persisted_launcher_context(&self, handle: &str) -> Option<String> {
        self.persistence
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()?
            .get(handle)?
            .launcher_context
            .clone()
    }

    /// the persisted deadline for `handle`. Second-best source
    /// behind the live-tab `launchDeadlineAt` tag; `None` for an unbounded
    /// launch or when persistence is off.
    pub(crate) fn persisted_deadline_ms(&self, handle: &str) -> Option<u64> {
        let guard = self.persistence.lock().unwrap_or_else(|e| e.into_inner());
        guard.as_ref()?.get(handle).and_then(|p| p.deadline_ms)
    }

    pub(crate) fn persisted_worktree_dir(&self, handle: &str, session_id: &str) -> Option<PathBuf> {
        let guard = self.persistence.lock().unwrap_or_else(|e| e.into_inner());
        let record = guard.as_ref()?.get(handle)?;
        if record.session_id.as_deref() == Some(session_id) {
            record.worktree_dir.clone()
        } else {
            None
        }
    }

    /// A person typed into `session_id`'s pane at `now_ms`. A bounded session that has
    /// not been sent `/exit` keeps running at least [`HUMAN_ACTIVE_GRACE_MS`] past the
    /// keystroke, so `maxRuntime` does not stop a session someone is using. Unbounded
    /// sessions, unknown ones, and ones already sent `/exit` are left alone.
    pub(crate) fn note_human_input(&self, session_id: &str, now_ms: u64) -> bool {
        let mut inner = self.lock();
        let Some(handle) = inner.by_session.get(session_id).cloned() else {
            return false;
        };
        let Some(entry) = inner.by_handle.get_mut(&handle) else {
            return false;
        };
        if entry.exit_sent_at_ms.is_some() {
            return false;
        }
        let Some(next) = entry
            .deadline_ms
            .and_then(|deadline| deadline_after_human_input(deadline, now_ms))
        else {
            return false;
        };
        entry.deadline_ms = Some(next);
        // A restore past its old deadline is no longer overdue once a person is
        // using it: the sweep gives it the usual `/exit` grace when its time comes.
        entry.restored_overdue = false;
        drop(inner);
        self.queue_persist(&handle);
        true
    }

    /// Owe `handle`'s record to disk: the registry file follows the in-memory entry
    /// (written while it exists and carries something to restore, removed once it is
    /// gone). Call after dropping `inner`. Never writes on the calling thread, so an
    /// async launch worker or the effects gate never waits on the file.
    fn queue_persist(&self, handle: &str) {
        self.dirty
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handle.to_string());
        #[cfg(test)]
        if !self.async_persist.load(std::sync::atomic::Ordering::SeqCst) {
            self.flush_now();
            return;
        }
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if writer
            .as_ref()
            .is_some_and(|wake| wake.send(WriterMsg::Wake).is_ok())
        {
            return;
        }
        let (wake, woken) = std::sync::mpsc::channel::<WriterMsg>();
        let (inner, persistence, dirty) = (
            self.inner.clone(),
            self.persistence.clone(),
            self.dirty.clone(),
        );
        #[cfg(test)]
        let hook = self.persist_hook.clone();
        let spawned = std::thread::Builder::new()
            .name("launch-registry-writer".into())
            .spawn(move || {
                let after_read = || {
                    #[cfg(test)]
                    hook.run();
                };
                while let Ok(first) = woken.recv() {
                    let mut waiting = Vec::new();
                    for message in
                        std::iter::once(first).chain(woken.try_iter().collect::<Vec<_>>())
                    {
                        if let WriterMsg::Flush(ack) = message {
                            waiting.push(ack);
                        }
                    }
                    Self::flush_dirty(&inner, &persistence, &dirty, &after_read);
                    for ack in waiting {
                        let _ = ack.send(());
                    }
                }
                Self::flush_dirty(&inner, &persistence, &dirty, &after_read);
            });
        if spawned.is_ok() && wake.send(WriterMsg::Wake).is_ok() {
            *writer = Some(wake);
        } else {
            *writer = None;
            drop(writer);
            log::warn!(target: "agents", "launch registry writer unavailable; writing inline");
            self.flush_now();
        }
    }

    /// Wait until every registry write queued before this call is on disk, or
    /// `timeout`. Holds no lock while waiting. `false` on timeout.
    pub(crate) fn flush_and_wait(&self, timeout: std::time::Duration) -> bool {
        let writer = self
            .writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let (ack, acked) = std::sync::mpsc::channel();
        let Some(writer) = writer.filter(|writer| writer.send(WriterMsg::Flush(ack)).is_ok())
        else {
            // No writer thread: nothing was queued off-thread; write what is owed here.
            self.flush_now();
            return true;
        };
        match acked.recv_timeout(timeout) {
            Ok(()) => true,
            Err(_) => {
                drop(writer);
                log::warn!(
                    target: "agents",
                    "launch registry writes still pending after {timeout:?}",
                );
                false
            }
        }
    }

    fn flush_now(&self) {
        #[cfg(test)]
        let after_read = || self.persist_hook.run();
        #[cfg(not(test))]
        let after_read = || {};
        Self::flush_dirty(&self.inner, &self.persistence, &self.dirty, &after_read);
    }

    /// Write every dirty handle's current record to disk. Holds `persistence` for the
    /// whole drain, so writes happen one at a time in the order their state was read,
    /// and each handle is read (under `inner`, briefly) just before it is written; a
    /// mutation after that read re-marks the handle and is written by a later pass.
    ///
    /// A record is kept for an agent launch's launcher relationship, a bounded
    /// launch's deadline (a person's launch included), or a captured workspace;
    /// with none of them, or once the entry is gone, it is removed. A write failure
    /// is logged and swallowed: the next write catches up.
    fn flush_dirty(
        inner: &Mutex<Inner>,
        persistence: &Mutex<Option<persistence::Store>>,
        dirty: &Mutex<HashSet<String>>,
        after_read: &dyn Fn(),
    ) {
        let mut guard = persistence.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let handle = {
                let mut dirty = dirty.lock().unwrap_or_else(|e| e.into_inner());
                let Some(handle) = dirty.iter().next().cloned() else {
                    return;
                };
                dirty.remove(&handle);
                handle
            };
            let snapshot = inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .by_handle
                .get(&handle)
                .map(PersistSnapshot::from_entry);
            after_read();
            let Some(store) = guard.as_mut() else {
                continue;
            };
            let record = snapshot.as_ref().filter(|snap| {
                snap.launched_by.is_some()
                    || snap.deadline_ms.is_some()
                    || snap.worktree_dir.is_some()
            });
            let Some(snap) = record else {
                if let Err(error) = store.remove(&handle) {
                    log::warn!(
                        target: "agents",
                        "launch registry prune failed for {handle}: {error}",
                    );
                }
                continue;
            };
            let team_key = snap
                .team
                .as_ref()
                .map(|team| crate::commands::pty_layout::team_key(&team.org, Some(&team.team)));
            let record = persistence::PersistedLaunch {
                launcher_context: snap.launcher_context.clone(),
                launched_by: snap.launched_by.clone(),
                launched_at: snap.started_at_ms.and_then(persistence::iso_from_epoch_ms),
                team_key,
                handle: handle.clone(),
                // mirror the deadline so a restart restores the sweep
                // when the session-server launch tag is lost.
                deadline_ms: snap.deadline_ms,
                session_id: snap.session_id.clone(),
                worktree_dir: snap.worktree_dir.clone(),
            };
            if let Err(error) = store.upsert(&handle, record) {
                log::warn!(
                    target: "agents",
                    "launch registry write failed for {handle}: {error}",
                );
            }
        }
    }

    /// Reserve a cap slot for `handle` **before** any spawn work (AC-8).
    /// Returns `true` when this call **freshly reserved** the slot, `false` when
    /// `handle` was already launching/launched (idempotent — no new permit).
    ///
    /// - **Idempotent focus:** a `handle` already present returns `Ok(false)`
    ///   **without** taking a second permit — a repeated Run focuses the
    ///   existing session, it does not fork a spawn. The caller uses the
    ///   `false` return to know it does **not** own the permit (so a later
    ///   failure must not [`abort`](Self::abort) someone else's live launch).
    /// - **Fail-fast:** an un-launched `handle` takes a non-blocking
    ///   `try_acquire`; a saturated cap returns [`SpawnCapError::Busy`]
    ///   immediately (never awaits — the bridge dispatch can't stall).
    pub fn begin(&self, handle: &str) -> Result<bool, SpawnCapError> {
        self.begin_with_provenance(handle, None, LaunchRecord::default())
    }

    /// Reserve a fresh launch and install its immutable run provenance and launch record
    /// under the same lock. An idempotent focus never overwrites the existing worker's
    /// ownership, name or launcher.
    pub(crate) fn begin_with_provenance(
        &self,
        handle: &str,
        provenance: Option<RunProvenance>,
        mut record: LaunchRecord,
    ) -> Result<bool, SpawnCapError> {
        let inner = self.lock();
        if inner.by_handle.contains_key(handle) {
            // Already launching or launched — idempotent focus, no new permit.
            return Ok(false);
        }
        // Normalized under the same lock that reads the reverse index, so the recorded
        // launcher is the handle that session had at launch time.
        let launched_by = record
            .meta
            .launched_by
            .as_deref()
            .map(|raw| normalize_launcher(&inner, raw));
        // Keep the stable session before releasing the lock: display handles can be reused
        // during the store read or startup. A live run is a fallback until capture arrives.
        let story_was_explicit = record.meta.story.is_some();
        let launcher_entry = launched_by.as_deref().and_then(|handle| inner.by_handle.get(handle));
        // Keep the story with the same launcher snapshot as the task: the handle may be
        // reused while the durable task is read. An explicitly named story always wins.
        if record.meta.story.is_none() {
            record.meta.story = match launcher_entry {
                Some(entry) if entry.launch_team() == record.meta.team && record.meta.team.is_some() => entry.meta.story.clone(),
                _ => None,
            };
        }
        let launcher = launcher_entry.and_then(|launcher| {
            let session = launcher.session_id.as_ref()?;
            let fallback = launcher.provenance.as_ref()
                .filter(|p| p.routine.is_some())
                .map(|p| p.run_id.clone())
                .unwrap_or_else(|| session.clone());
            Some((session.clone(), fallback))
        });
        drop(inner);
        #[cfg(test)]
        if let Some(hook) = tests::CDE36_RESERVATION_GAP.with(|hook| hook.borrow_mut().take()) {
            hook(self);
        }

        // Restoration does not retain transient run provenance. Prefer the captured root's
        // durable identity, without holding the registry lock across SQLite I/O.
        let launcher_task = launcher.map(|(session, fallback)| {
            task_store_path(Some(self))
                .and_then(|path| crate::session_usage::UsageStore::open(&path))
                .and_then(|store| {
                    let id = crate::session_usage::task_capture::task_id_of_session(&store, &session)?;
                    Ok(store.read_task(&id)?.map(|task| task.task_id))
                })
                .ok()
                .flatten()
                .unwrap_or(fallback)
        });
        let mut inner = self.lock();
        if inner.by_handle.contains_key(handle) {
            return Ok(false);
        }
        let permit = self.cap.try_acquire()?;
        inner.by_handle.insert(
            handle.to_string(),
            LaunchEntry {
                chat_workspace_id: None,
                remote_shell: None,
                local_effect_owner: None,
                session_id: None,
                launcher_task,
                story_was_explicit,
                _permit: Some(permit),
                bound_at_ms: None,
                ready_at_ms: None,
                startup_noted: false,
                kind: KIND_DASHBOARD_LAUNCHED,
                started_with: record.started_with,
                started_at_ms: Some(now_epoch_ms()),
                // Stamped at `bind` once the launching dashboard is known (SW-GW-8).
                launcher: None,
                account_provenance: None,
                routine_key: provenance.as_ref().and_then(|p| p.routine_key.clone()),
                provenance,
                // SW-AU-96B: stamped post-bind by `set_worktree_dir_if_absent`.
                worktree_dir: None,
                profile_id: None,
                owner: None,
                // Set by `mark_headless` only for a PTY-less headless run (SW-OR-10).
                headless_pid: None,
                headless_cancel: None,
                // Stamped by `set_interactive_capture_if_absent` at the launch spine.
                interactive_capture: None,
                capture_raw_terminal: false,
                last_drain_ms: None,
                meta: LaunchMeta {
                    launched_by,
                    ..record.meta
                },
                agent_revision: None,
                // carry the per-launch budget; `deadline_ms` is
                // stamped at `bind_owned` once the session's bind-time is known.
                max_runtime_ms: record.max_runtime_ms,
                deadline_ms: None,
                exit_sent_at_ms: None,
                restored_overdue: false,
                launch_settings: None,
            },
        );
        // SW-FX-launcher-persistence (step 1): mirror the just-committed launcher
        // relationship to disk so a later app process can rebind on reconnect.
        drop(inner);
        self.queue_persist(handle);
        Ok(true)
    }

    /// Stamp the spawned `session_id` onto `handle`'s reserved entry (AC-12).
    /// Errors if `begin` was not called first (no reserved permit) — the write
    /// path never falls through to an unreserved launch. SW-GW-8: `launcher`
    /// records the launching surface (the dashboard `docPath`); a `None` never
    /// clobbers a launcher already stamped (re-bind/respawn keeps provenance).
    #[cfg(test)]
    pub fn bind(
        &self,
        handle: &str,
        session_id: &str,
        launcher: Option<&str>,
    ) -> Result<(), String> {
        self.bind_with_account(handle, session_id, launcher, None)
    }

    pub fn bind_with_account(
        &self,
        handle: &str,
        session_id: &str,
        launcher: Option<&str>,
        account_provenance: Option<AgentAccountProvenance>,
    ) -> Result<(), String> {
        self.bind_owned(handle, session_id, launcher, account_provenance, None)
    }

    pub(crate) fn bind_owned(
        &self,
        handle: &str,
        session_id: &str,
        launcher: Option<&str>,
        account_provenance: Option<AgentAccountProvenance>,
        owner: Option<super::pty_host::LocalEffectClaim>,
    ) -> Result<(), String> {
        let mut inner = self.lock();
        if inner
            .by_session
            .get(session_id)
            .and_then(|handle| inner.by_handle.get(handle))
            .is_some_and(|entry| entry.local_effect_owner != owner)
        {
            return Err("session is bound to another runtime claim".into());
        }
        let Some(entry) = inner.by_handle.get_mut(handle) else {
            return Err(format!(
                "unavailable: no reserved launch for handle '{handle}' (begin must precede bind)"
            ));
        };
        entry.local_effect_owner = owner;
        stamp_launcher(entry, launcher);
        if entry.account_provenance.is_none() {
            entry.account_provenance = account_provenance;
        }
        // Re-bind (respawn/tab reuse): drop the stale reverse-index entry first
        // so a superseded session id can't resolve to this story (AC-7 drift).
        let prev = entry.session_id.replace(session_id.to_string());
        let budget_changed = if prev.as_deref() != Some(session_id) {
            entry.start_bound_clock();
            // anchor the deadline to this bind's wall-clock. A
            // rebind restarts the budget (the agent that would have run out
            // got replaced); unbounded entries never gain one.
            entry.deadline_ms = entry
                .max_runtime_ms
                .and_then(|ms| entry.bound_at_ms.map(|at| at.saturating_add(ms)));
            entry.exit_sent_at_ms = None;
            entry.restored_overdue = false;
            true
        } else {
            false
        };
        if let Some(prev) = prev.filter(|prev| prev != session_id) {
            inner.by_session.remove(&prev);
        }
        inner
            .by_session
            .insert(session_id.to_string(), handle.to_string());
        // mirror the just-stamped deadline to the on-disk record so a
        // later app process can restore the sweep's clock through the file
        // fallback when the tab's tag is lost. Gated on a bounded entry (or one
        // holding a captured workspace, whose record must carry the session id the
        // restore check matches) so an unbounded rebind does not pay a redundant
        // file write.
        let persist = budget_changed
            && inner
                .by_handle
                .get(handle)
                .is_some_and(|entry| entry.deadline_ms.is_some() || entry.worktree_dir.is_some());
        drop(inner);
        if persist {
            self.queue_persist(handle);
        }
        Ok(())
    }

    /// SW-DX-11: register an EXTERNALLY-spawned session (an app-opened epic /
    /// context terminal) under `handle` **cap-free** — these sessions are not
    /// dashboard-launched, so they must not consume a supervisory [`SpawnCap`]
    /// slot, yet they must still be addressable by `sendToSession` exactly like a
    /// dashboard launch. Idempotent: re-registering the same handle rebinds its
    /// session id (dropping the stale reverse-index entry, mirroring [`bind`]).
    /// This is the ONE additional stamp path §2 calls for — NOT a per-kind map.
    /// SW-GW-8: `launcher` records the launching surface (`"codesense"` for a
    /// nav/manual open); a `None` re-stamp never clobbers a prior value.
    pub fn register(&self, handle: &str, session_id: &str, launcher: Option<&str>) {
        let _ = self.register_owned(handle, session_id, launcher, None);
    }

    /// [`register`](Self::register) under the session's runtime claim. `Ok(false)` when the
    /// session is left to the launch that named it, and nothing was registered.
    pub(crate) fn register_owned(
        &self,
        handle: &str,
        session_id: &str,
        launcher: Option<&str>,
        owner: Option<super::pty_host::LocalEffectClaim>,
    ) -> Result<bool, String> {
        // SW-FX-launcher-persistence (step 3): read the persisted record BEFORE
        // taking the inner-registry lock, so the persistence mutex and the inner
        // mutex are never nested. If the session is new to the registry we may
        // need this below to re-stamp the launcher relationship it had in a
        // prior app run.
        let persisted_launcher_context = self.persisted_launcher_context(handle);
        let persisted_launched_by = self.persisted_launched_by(handle);
        let mut inner = self.lock();
        if inner
            .by_session
            .get(session_id)
            .and_then(|handle| inner.by_handle.get(handle))
            .is_some_and(|entry| entry.local_effect_owner != owner)
        {
            return Err("session is registered to another runtime claim".into());
        }
        // A session belongs to exactly ONE handle: if this session id was already
        // bound to a DIFFERENT handle, drop that stale entry first so it can't keep
        // resolving to a session it no longer owns (review r1; mirrors the reverse
        // hygiene `bind` applies on a re-bind).
        if let Some(prev_handle) = inner.by_session.get(session_id).cloned() {
            if prev_handle != handle {
                // A session a launch named stays that launch's: registering a workspace's
                // main session never relabels an agent (a manager, a launched worker)
                // that runs in it.
                if inner
                    .by_handle
                    .get(&prev_handle)
                    .is_some_and(LaunchEntry::names_an_agent)
                {
                    return Ok(false);
                }
                inner.by_handle.remove(&prev_handle);
            }
        }
        if let Some(entry) = inner.by_handle.get_mut(handle) {
            entry.local_effect_owner = owner;
            // Existing handle (e.g. re-open) — rebind, dropping any stale reverse
            // entry so a superseded session id can't resolve to this handle.
            stamp_launcher(entry, launcher);
            let prev = entry.session_id.replace(session_id.to_string());
            if prev.as_deref() != Some(session_id) {
                entry.start_bound_clock();
            }
            if let Some(prev) = prev.filter(|prev| prev != session_id) {
                inner.by_session.remove(&prev);
            }
            inner
                .by_session
                .insert(session_id.to_string(), handle.to_string());
            return Ok(true);
        }
        // SW-FX-launcher-persistence (step 3): enrich the fresh entry with the
        // launcher relationship it had in a prior app run (captured above before
        // taking this inner lock). This covers the restart path where
        // local_restore's tag-driven restore cannot run (unreadable layout,
        // deferred agent probe, etc.).
        let mut entry = LaunchEntry::registered(session_id, launcher, owner);
        entry.meta.launcher_context = persisted_launcher_context;
        if persisted_launched_by.is_some() {
            entry.meta.launched_by = persisted_launched_by;
        }
        inner.by_handle.insert(handle.to_string(), entry);
        inner
            .by_session
            .insert(session_id.to_string(), handle.to_string());
        Ok(true)
    }

    pub(crate) fn release_owned(
        &self,
        claim: &super::pty_host::LocalEffectClaim,
    ) -> Option<ReleasedSession> {
        let mut inner = self.lock();
        // A session that leaves the registry takes what its agent launch staged, bound or
        // not (a terminal killed before its bind).
        let staged = inner.staged_launches.remove(&claim.session);
        let handle = inner
            .by_session
            .get(&claim.session)
            .cloned()
            .filter(|handle| {
                inner.by_handle.get(handle).is_some_and(|entry| {
                    entry.kind != KIND_HEADLESS && entry.local_effect_owner.as_ref() == Some(claim)
                })
            });
        let entry = handle.as_ref().and_then(|handle| {
            inner.by_session.remove(&claim.session);
            inner.by_handle.remove(handle)
        });
        // The entry and anything staged go after the lock: their settings files are deleted
        // on drop.
        drop(inner);
        drop(staged);
        // SW-FX-launcher-persistence (step 4): mirror the in-memory release to
        // disk so the on-disk map stays bounded (the writer removes the record once
        // the entry is gone). Marked AFTER the inner lock is dropped.
        if let Some(handle) = handle.as_deref() {
            self.queue_persist(handle);
        }
        Some(ReleasedSession {
            handle: handle?,
            kind: entry?.kind,
        })
    }

    pub(crate) fn transfer_owned(
        &self,
        old: &super::pty_host::LocalEffectClaim,
        new: &super::pty_host::LocalEffectClaim,
    ) {
        let mut inner = self.lock();
        if let Some(handle) = inner.by_session.get(&old.session).cloned() {
            if let Some(entry) = inner.by_handle.get_mut(&handle) {
                if entry.local_effect_owner.as_ref() == Some(old) {
                    entry.local_effect_owner = Some(new.clone());
                }
            }
        }
    }

    /// SW-SR-3 (AC-4): stamp the auto-run provenance onto `handle`'s entry after a
    /// dispatch. Idempotent (a re-tick that re-dispatches the same story overwrites with
    /// the same value). A no-op for an unknown handle — the caller only stamps a handle it
    /// just launched, so this can't lose a live worker's provenance. Best-effort: dispatch
    /// stamps AFTER the launch commits, so a lister in the brief gap sees `None`, never a
    /// wrong value.
    pub fn stamp_provenance(&self, handle: &str, provenance: RunProvenance) {
        if let Some(entry) = self.lock().by_handle.get_mut(handle) {
            entry.routine_key = provenance.routine_key.clone();
            entry.provenance = Some(provenance);
        }
    }

    /// Snapshot deadline work without holding the registry lock across I/O.
    pub(crate) fn deadline_snapshot(&self) -> Vec<deadline::DeadlineEntry> {
        let inner = self.lock();
        let mut entries: Vec<_> = inner.pending_deadlines.values().cloned().collect();
        entries.extend(inner.by_handle.iter().filter_map(|(handle, entry)| {
            let entry = deadline_entry(handle, entry)?;
            (!inner.pending_deadlines.contains_key(&entry.session_id)).then_some(entry)
        }));
        entries
    }

    /// Stamp `session` ready the first time it is observed; later calls keep the first.
    pub(crate) fn mark_ready(&self, session: &str) {
        let mut inner = self.lock();
        if let Some(entry) = entry_for_session_mut(&mut inner, session) {
            entry.ready_at_ms.get_or_insert_with(now_epoch_ms);
        }
    }

    /// When `session`'s bound clock started and, if it has been observed, when it first was.
    pub(crate) fn readiness(&self, session: &str) -> Option<(Option<u64>, Option<u64>)> {
        let mut inner = self.lock();
        let entry = entry_for_session_mut(&mut inner, session)?;
        Some((entry.bound_clock_ms(), entry.ready_at_ms))
    }

    /// Sessions never observed ready and not yet noted: the startup sweep's candidates.
    pub(crate) fn startup_candidates(&self) -> Vec<deadline::StartupEntry> {
        let inner = self.lock();
        inner
            .by_handle
            .iter()
            .filter(|(_, entry)| entry.ready_at_ms.is_none() && !entry.startup_noted)
            .filter_map(|(handle, entry)| {
                entry.bound_clock_ms()?;
                Some(deadline::StartupEntry {
                    handle: handle.clone(),
                    session_id: entry.startup_session()?,
                    launched_by: entry.meta.launched_by.clone(),
                    team: entry.launch_team(),
                })
            })
            .collect()
    }

    /// Record the startup note as delivered, unless the entry moved to another session.
    pub(crate) fn complete_startup(&self, expected: &deadline::StartupEntry) {
        let mut inner = self.lock();
        if let Some(entry) = inner.by_handle.get_mut(&expected.handle) {
            if entry.startup_session().as_deref() == Some(&expected.session_id) {
                entry.startup_noted = true;
            }
        }
    }

    pub(crate) fn deadline_is_current(&self, expected: &deadline::DeadlineEntry) -> bool {
        let inner = self.lock();
        current_deadline(&inner, expected).as_ref() == Some(expected)
    }

    /// Match the session and deadline, so a stale sweep cannot arm a reused handle.
    pub(crate) fn mark_exit_sent(&self, expected: &deadline::DeadlineEntry, at_ms: u64) -> bool {
        let mut inner = self.lock();
        if current_deadline(&inner, expected).as_ref() != Some(expected)
            || expected.exit_sent_at_ms.is_some()
        {
            return false;
        }
        let Some(entry) = inner.by_handle.get_mut(&expected.handle) else {
            return false;
        };
        entry.exit_sent_at_ms = Some(at_ms);
        let mut pending = expected.clone();
        pending.exit_sent_at_ms = Some(at_ms);
        inner
            .pending_deadlines
            .insert(pending.session_id.clone(), pending);
        true
    }

    /// Timestamp completion of submission, not the beginning of a serial sweep.
    pub(crate) fn deadline_exit_submitted(&self, expected: &deadline::DeadlineEntry, at_ms: u64) {
        let mut inner = self.lock();
        if let Some(pending) = inner.pending_deadlines.get_mut(&expected.session_id) {
            if pending.enforced.is_none() {
                pending.exit_sent_at_ms = Some(at_ms);
            }
        }
        if let Some(entry) = inner.by_handle.get_mut(&expected.handle) {
            if entry.session_id.as_deref() == Some(&expected.session_id)
                && entry.deadline_ms == Some(expected.deadline_ms)
            {
                entry.exit_sent_at_ms = Some(at_ms);
            }
        }
    }

    /// Keep the completed outcome until the inbox acknowledges the notification.
    pub(crate) fn mark_deadline_enforced(&self, expected: &deadline::DeadlineEntry, killed: bool) {
        let mut pending = expected.clone();
        pending.enforced = Some(killed);
        self.lock()
            .pending_deadlines
            .insert(pending.session_id.clone(), pending);
    }

    pub(crate) fn complete_deadline(&self, expected: &deadline::DeadlineEntry) {
        let mut inner = self.lock();
        inner.pending_deadlines.remove(&expected.session_id);
        if let Some(entry) = inner.by_handle.get_mut(&expected.handle) {
            if entry.session_id.as_deref() == Some(&expected.session_id)
                && entry.deadline_ms == Some(expected.deadline_ms)
            {
                entry.deadline_ms = None;
                entry.exit_sent_at_ms = None;
            }
        }
    }

    /// stamp a one-shot reason for `session_id`'s next `Kill` audit
    /// line. Set by the deadline sweep before it calls `pty::kill_session`;
    /// drained by [`take_kill_reason`] when the audit append runs. First
    /// writer wins. A no-op when a reason is already stamped (another concurrent
    /// kill would have a different intent, but we never relabel the first one).
    pub(crate) fn record_kill_reason(&self, session_id: &str, reason: &'static str) {
        let mut inner = self.lock();
        inner
            .kill_reasons
            .entry(session_id.to_string())
            .or_insert(reason);
    }

    /// drain the stamped reason for `session_id`'s `Kill` audit line.
    /// `None` for a kill the sweep did not initiate.
    pub(crate) fn take_kill_reason(&self, session_id: &str) -> Option<&'static str> {
        let mut inner = self.lock();
        inner.kill_reasons.remove(session_id)
    }

    /// SW-OR-10: mark `handle`'s (already-bound) entry as a PTY-less headless run —
    /// flipping its stored [`kind`](LaunchEntry::kind) to [`KIND_HEADLESS`] so
    /// [`list_sessions_inner`] derives its liveness from registry presence rather than
    /// a (non-existent) PTY probe, and stashing the child's `pid` slot so the Stop /
    /// app-exit paths can `killpg` it. A no-op for an unknown handle. The permit reserved
    /// by [`begin`](Self::begin) is retained (a headless run is an unattended agent, so it
    /// counts toward the background-agent limit until its completion/kill releases it).
    pub fn mark_headless(
        &self,
        handle: &str,
        pid: Arc<AtomicU32>,
        cancel: Arc<AtomicBool>,
        last_drain_ms: Arc<AtomicU64>,
    ) {
        if let Some(entry) = self.lock().by_handle.get_mut(handle) {
            entry.kind = KIND_HEADLESS;
            entry.headless_pid = Some(pid);
            entry.headless_cancel = Some(cancel);
            // SW-OR-19: the heartbeat anchor `spawn_capture` stamps per drained chunk.
            entry.last_drain_ms = Some(last_drain_ms);
        }
    }

    /// SW-OR-10: every live headless child's pgid (skipping not-yet-spawned `0`s), for the
    /// app-exit backstop to `killpg` so a PTY-less agent never orphans past app close. The
    /// live Stop / drain paths instead reap via [`release_for_session`](Self::release_for_session)
    /// (the one choke they all funnel through); this direct sweep exists only because
    /// app-exit kills PtyState sessions WITHOUT going through that choke.
    pub fn headless_child_pgids(&self) -> Vec<u32> {
        self.lock()
            .by_handle
            .values()
            .filter(|e| e.kind == KIND_HEADLESS)
            .filter_map(|e| e.headless_pid.as_ref().map(|p| p.load(Ordering::SeqCst)))
            .filter(|&pid| pid != 0)
            .collect()
    }

    /// SW-PB-23: how many headless (no-terminal) agent runs are going right now. A restart ends
    /// them, so the required-update deadline treats any as someone at the Mac. Beta channels only.
    #[cfg(not(cs_channel = "internal"))]
    pub fn running_headless_count(&self) -> usize {
        // Every headless entry counts, a run still starting (its pid not yet known) included,
        // but not one whose child has exited and only waits on its transcript.
        let inner = self.lock();
        inner
            .by_handle
            .values()
            .filter(|e| e.kind == KIND_HEADLESS)
            .filter(|e| {
                e.session_id
                    .as_deref()
                    .is_none_or(|sid| !inner.headless_exited.contains(sid))
            })
            .count()
    }

    /// SW-AU-96B: record the launch's managed worktree dir onto `handle`'s entry — fill-gap and
    /// focus-safe (never overwritten once set). The completion path reads it back
    /// in-memory to diff the work-product branch for the per-file footprint, never re-deriving the
    /// path at teardown. A no-op for an unknown handle or once already set.
    pub fn set_worktree_dir_if_absent(&self, handle: &str, worktree_dir: PathBuf) {
        {
            let mut inner = self.lock();
            let Some(entry) = inner.by_handle.get_mut(handle) else {
                return;
            };
            if entry.worktree_dir.is_some() {
                return;
            }
            entry.worktree_dir = Some(worktree_dir);
        }
        self.queue_persist(handle);
    }

    /// Record the resolved launch profile id onto `handle`'s entry — fill-gap only: an
    /// idempotent focus never relabels the live session with a later profile.
    pub fn set_profile_id_if_absent(&self, handle: &str, profile_id: String) {
        if let Some(entry) = self.lock().by_handle.get_mut(handle) {
            if entry.profile_id.is_none() {
                entry.profile_id = Some(profile_id);
            }
        }
    }

    /// Record the named agent (`<key>@<revision>`) `handle`'s session runs — fill-gap only,
    /// like the profile id.
    pub fn set_agent_revision_if_absent(&self, handle: &str, agent_revision: String) {
        if let Some(entry) = self.lock().by_handle.get_mut(handle) {
            if entry.agent_revision.is_none() {
                entry.agent_revision = Some(agent_revision);
            }
        }
    }

    /// Hand `handle`'s entry its launch's per-launch settings file, so the file goes when the
    /// session's entry does. With no entry (the launch was already torn down) the file is
    /// dropped, and so removed, at once.
    pub(crate) fn hold_launch_settings(
        &self,
        handle: &str,
        settings: crate::agents::launch::LaunchSettingsFile,
    ) {
        if let Some(entry) = self.lock().by_handle.get_mut(handle) {
            entry.launch_settings = Some(settings);
        }
    }

    /// Stage what an agent launch prepared for `session_id` before that session is bound, so
    /// bind can record it on the session's entry. Ending the session first removes it.
    pub(crate) fn stage_agent_launch(&self, session_id: &str, staged: StagedAgentLaunch) {
        let replaced = self
            .lock()
            .staged_launches
            .insert(session_id.to_string(), staged);
        drop(replaced);
    }

    /// The agent revision an agent launch staged for `session_id`, if any.
    pub(crate) fn staged_agent_revision(&self, session_id: &str) -> Option<String> {
        self.lock()
            .staged_launches
            .get(session_id)
            .map(|staged| staged.agent_revision.clone())
    }

    /// The agent CLI binary an agent launch staged for `session_id`, if any.
    pub(crate) fn staged_agent_binary(
        &self,
        session_id: &str,
    ) -> Option<super::deps::ResolvedAgentBinary> {
        self.lock()
            .staged_launches
            .get(session_id)
            .and_then(|staged| staged.binary.clone())
    }

    /// The launch facts an agent launch staged for `session_id`, if any.
    pub(crate) fn staged_launch_facts(
        &self,
        session_id: &str,
    ) -> Option<crate::session_usage::record::LaunchFacts> {
        self.lock()
            .staged_launches
            .get(session_id)
            .and_then(|staged| staged.facts.clone())
    }

    /// Take what an agent launch staged for `session_id`, if anything.
    pub(crate) fn take_staged_agent_launch(&self, session_id: &str) -> Option<StagedAgentLaunch> {
        self.lock().staged_launches.remove(session_id)
    }

    /// Replace what `handle`'s entry says about where and as what its session was launched: a
    /// fresh session bound to a reused handle is not the session the old stamps described.
    pub fn restamp_launch(&self, handle: &str, owner: Option<SessionOwner>) {
        let superseded = self.lock().by_handle.get_mut(handle).and_then(|entry| {
            entry.owner = owner;
            entry.profile_id = None;
            entry.agent_revision = None;
            entry.capture_raw_terminal = false;
            entry.launch_settings.take()
        });
        // Removed after the lock: dropping the file deletes it.
        drop(superseded);
    }

    /// Record where `handle`'s session belongs — fill-gap only, so an idempotent focus from
    /// another team never moves the live session.
    pub fn set_owner_if_absent(&self, handle: &str, owner: SessionOwner) {
        if let Some(entry) = self.lock().by_handle.get_mut(handle) {
            if entry.owner.is_none() {
                entry.owner = Some(owner);
            }
        }
    }

    /// SW-OR-15: record the launched adapter's interactive structured-transcript descriptor onto
    /// `handle`'s entry — fill-gap only (an idempotent focus must never overwrite the
    /// live run's stamped descriptor with a later focus's profile). A no-op for an unknown handle.
    pub fn set_interactive_capture_if_absent(
        &self,
        handle: &str,
        context: InteractiveCaptureContext,
    ) {
        if let Some(entry) = self.lock().by_handle.get_mut(handle) {
            if entry.interactive_capture.is_none() {
                entry.interactive_capture = Some(context);
            }
        }
    }

    /// Stamp user consent only while the caller owns a fresh launch reservation.
    pub fn enable_raw_terminal_capture_for_fresh_launch(&self, handle: &str) {
        if let Some(entry) = self.lock().by_handle.get_mut(handle) {
            entry.capture_raw_terminal = true;
        }
    }

    #[cfg(test)]
    pub(crate) fn raw_capture_enabled_for_session(&self, session_id: &str) -> bool {
        self.with_entry(session_id, |entry| entry.capture_raw_terminal)
            .unwrap_or(false)
    }

    /// SW-OR-14: is `session_id` a LIVE headless pseudo-session? Resolves the reverse
    /// index (`by_session → by_handle`) and applies the shared [`is_live_headless`] rule.
    /// The MCP liveness gate (`require_live_session`) ORs this with the PTY probe so a
    /// PTY-less headless agent can reach the session-keyed verbs — parity with an
    /// interactive session, same trust boundary (the id is codesense's own synthetic
    /// stamp echoed back). `false` after child exit even while registry provenance is retained for
    /// teardown, and for an unknown / PTY / reserved-but-unbound session.
    /// Where the restore of this Mac's launches after an app restart stands
    /// (SW-AGR-2: a wake never resolves before it settles).
    pub(crate) fn local_restore_state(&self) -> crate::routines::wake::Restore {
        self.local_restore.state()
    }

    /// Whether the inbox may take an unknown connection-implied tab for the human
    /// ([`local_restore::RestoreOnce::caller_gate_settled`]).
    pub(crate) fn local_restore_caller_gate_settled(&self) -> bool {
        self.local_restore.caller_gate_settled()
    }

    pub fn has_live_headless_session(&self, session_id: &str) -> bool {
        let inner = self.lock();
        if inner.headless_exited.contains(session_id) {
            return false;
        }
        inner
            .by_session
            .get(session_id)
            .and_then(|handle| inner.by_handle.get(handle))
            .is_some_and(|entry| is_live_headless(entry.kind, entry.session_id.as_deref()))
    }

    /// The ONE `sessionId → handle → LaunchEntry` two-hop read: resolve `session_id` through
    /// the reverse index and project its entry through `f`. `None` for an unbound session (so a
    /// caller maps that to its own miss value). One brief registry lock, released on return —
    /// every per-session accessor funnels through here so the two-hop lives in one place.
    fn with_entry<T>(&self, session_id: &str, f: impl FnOnce(&LaunchEntry) -> T) -> Option<T> {
        let inner = self.lock();
        let handle = inner.by_session.get(session_id)?;
        inner.by_handle.get(handle).map(f)
    }

    /// The `sessionId → handle → provenance` read shared by the provenance-derived accessors:
    /// project the entry's stamped [`RunProvenance`] through `f`. `None` for an unbound / un-stamped session.
    fn with_provenance<T>(
        &self,
        session_id: &str,
        f: impl FnOnce(&RunProvenance) -> T,
    ) -> Option<T> {
        self.with_entry(session_id, |e| e.provenance.as_ref().map(f))
            .flatten()
    }

    /// SW-AU-52: the durable-outcome join context for a session — its run identity (from
    /// provenance) plus the launch birth time — resolved in ONE lock. `None` for a session with no
    /// provenance (a bare hand-launched shell has no `run_id`, so its outcome is not persisted).
    pub(crate) fn run_context_for_session(&self, session_id: &str) -> Option<RunOutcomeContext> {
        self.with_entry(session_id, run_outcome_context).flatten()
    }

    /// SW-AU-96: the `(run_id, story_id)` a reported step result is keyed by, resolved from a live
    /// session's launch provenance (in-memory — no store read). `None` for a bare shell or unknown
    /// session, so the MCP reporting tools no-op gracefully rather than erroring the agent.
    pub(crate) fn run_identity_for_session(&self, session_id: &str) -> Option<(String, String)> {
        self.run_context_for_session(session_id)
            .map(|ctx| (ctx.run_id, ctx.story_id))
    }

    fn take_run_context_for_session(&self, session_id: &str) -> Option<RunOutcomeContext> {
        let mut inner = self.lock();
        let handle = inner.by_session.get(session_id)?.clone();
        let mut context = run_outcome_context(inner.by_handle.get(&handle)?)?;
        context.capture_raw_terminal &= inner.raw_capture_taken.insert(session_id.to_string());
        Some(context)
    }

    fn take_run_context_for_outcome(
        &self,
        session_id: &str,
        status: &str,
    ) -> Option<(crate::dashboard_stream::SessionStatus, RunOutcomeContext)> {
        let status = crate::dashboard_stream::SessionStatus::from_wire(status)?;
        let context = self.take_run_context_for_session(session_id)?;
        Some((status, context))
    }

    /// Atomically begin a headless Signal outcome and capture the provenance its durable write
    /// needs, so completion teardown cannot invalidate an already-authorized call.
    pub(crate) fn begin_signal_outcome(
        &self,
        session_id: &str,
    ) -> (bool, Option<RunOutcomeContext>) {
        let mut inner = self.lock();
        let Some(entry) = inner
            .by_session
            .get(session_id)
            .and_then(|handle| inner.by_handle.get(handle))
        else {
            return (false, None);
        };
        if !is_live_headless(entry.kind, entry.session_id.as_deref()) {
            return (false, None);
        }
        let mut context = run_outcome_context(entry);
        if let Some(ctx) = context.as_mut() {
            ctx.capture_raw_terminal &= inner.raw_capture_taken.insert(session_id.to_string());
        }
        inner.attested.insert(session_id.to_string());
        (true, context)
    }

    /// SW-AU-52: the ONE seam both completion paths funnel (AC-4, no headless-only fork) — resolve
    /// the run identity from the session's provenance and append its terminal `outcome` to
    /// `run_store`. Off the async runtime via `await_blocking` (AC-5, no fs on a hot worker); the
    /// append is best-effort/never-fatal. A session with no provenance (a bare shell → no `run_id`)
    /// is a documented no-op. Callers wanting write-before-reap must invoke this before releasing
    /// the session (release drops the provenance this reads).
    async fn record_session_outcome_with<F, Fut>(
        &self,
        session_id: &str,
        input: SessionOutcomeInput,
        snapshot: F,
        destination: CapturedOutcomeDestination,
    ) -> bool
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Option<String>>,
    {
        let Some((status, ctx)) = self.take_run_context_for_outcome(session_id, &input.status)
        else {
            return false;
        };
        let raw_snapshot =
            capture_raw_snapshot_if_enabled(ctx.should_capture_raw_terminal(), snapshot).await;
        Self::record_captured_outcome_with_destination(
            ctx,
            CapturedOutcomeInput {
                status,
                summary: input.summary,
                exit_code: input.exit_code,
                source: input.source,
                label: input.label,
                raw_snapshot,
            },
            destination,
        )
        .await;
        true
    }

    #[cfg(test)]
    pub(crate) async fn record_captured_outcome(
        ctx: RunOutcomeContext,
        status: crate::dashboard_stream::SessionStatus,
        summary: Option<String>,
        exit_code: Option<i32>,
        source: crate::commands::sprint_run::run_store::OutcomeSource,
        label: &'static str,
        raw_snapshot: Option<String>,
    ) {
        Self::record_captured_outcome_with_destination(
            ctx,
            CapturedOutcomeInput {
                status,
                summary,
                exit_code,
                source,
                label,
                raw_snapshot,
            },
            CapturedOutcomeDestination::Production,
        )
        .await;
    }

    async fn record_captured_outcome_with_destination(
        ctx: RunOutcomeContext,
        input: CapturedOutcomeInput,
        destination: CapturedOutcomeDestination,
    ) {
        if ctx.capture_raw_terminal {
            if let Some(snapshot) = input.raw_snapshot {
                match &destination {
                    #[cfg(test)]
                    CapturedOutcomeDestination::TestArtifactDir(dir) => {
                        crate::commands::sprint_run::run_artifact::write_raw_terminal_snapshot_to(
                            dir.clone(),
                            &ctx.run_id,
                            snapshot,
                        )
                        .await;
                    }
                    CapturedOutcomeDestination::Production => {
                        crate::commands::sprint_run::run_artifact::write_raw_terminal_snapshot(
                            &ctx.run_id,
                            snapshot,
                        )
                        .await;
                    }
                }
            }
        }
        // The conversation is stored by session, not by run: store the run's sessions' now, so the
        // run reads it as soon as its outcome is recorded. A bare shell has no run context and
        // never gets here; a session with nothing to read stores nothing and the run keeps its
        // outcome and summary.
        for session in &ctx.session_ids {
            crate::session_conversation::capture_now(session).await;
        }
        let artifact_ref: Option<String> = None;
        if matches!(destination, CapturedOutcomeDestination::Production) {
            append_run_outcome(
                ctx,
                input.status,
                input.summary,
                input.exit_code,
                input.source,
                input.label,
                artifact_ref,
            )
            .await;
        }
    }

    pub(crate) fn remote_output_run_id(&self, session_id: &str) -> Option<String> {
        self.remote_shell_for_session(session_id)?.output_run_id()
    }

    /// Whether `session_id` is a remote launch whose CLI keeps a native record the host can read:
    /// the one session whose conversation is read from its host, not from this Mac's files.
    pub(crate) fn native_output_declared(&self, session_id: &str) -> bool {
        self.remote_output_run_id(session_id).is_some()
    }

    /// The launch profile `session_id` was started with, or `None` when it is unbound or was
    /// launched without a resolved profile.
    pub fn profile_id_for_session(&self, session_id: &str) -> Option<String> {
        self.with_entry(session_id, |e| e.profile_id.clone())
            .flatten()
    }

    /// The named agent `session_id` runs, as `<key>@<revision>`, when it was launched as one.
    pub fn agent_revision_for_session(&self, session_id: &str) -> Option<String> {
        self.with_entry(session_id, |e| e.agent_revision.clone())
            .flatten()
    }

    /// The runtime an account-bound launch of `session_id` resolved its account for.
    pub fn account_runtime_for_session(
        &self,
        session_id: &str,
    ) -> Option<codesense_pty_core::agent_cli::AgentCli> {
        self.with_entry(session_id, |e| {
            e.account_provenance
                .as_ref()
                .and_then(|a| a.runtime.parse().ok())
        })
        .flatten()
    }

    /// Where `session_id` belongs, or `None` when it is unbound or its launch did not say.
    pub fn owner_for_session(&self, session_id: &str) -> Option<SessionOwner> {
        self.with_entry(session_id, |e| e.owner.clone()).flatten()
    }

    /// Architecture: admission reads capture through the canonical session join.
    pub(crate) fn worktree_dir_for_session(&self, session_id: &str) -> Option<PathBuf> {
        self.with_entry(session_id, |entry| entry.worktree_dir.clone())
            .flatten()
    }

    /// SW-AGR-9: the `<key>@<revision>` agent `session_id` was launched as.
    #[allow(dead_code, reason = "SW-AGR-2's wake posture is the caller")]
    pub fn agent_stamp_for_session(&self, session_id: &str) -> Option<String> {
        self.with_provenance(session_id, |p| p.agent.clone())
            .flatten()
    }

    /// SW-AGR-9: a raw remote terminal's record for `session_id`, if it is one.
    #[allow(dead_code, reason = "SW-AGR-2's wake posture is the caller")]
    pub(crate) fn remote_shell_for_session(
        &self,
        session_id: &str,
    ) -> Option<crate::commands::pty_layout::RemoteShellRecord> {
        let handle = self.handle_for_session(session_id)?;
        self.remote_shell_by_handle(&handle)
            .filter(|record| record.target.session.as_str() == session_id)
    }

    /// SW-AGR-9: the process of a headless run `session_id` (no PTY), once
    /// spawned. The slot is non-zero only while this app's own unreaped child
    /// holds it (the capture clears it before reaping), so the pid cannot name
    /// a recycled process: its start time read now is the child's.
    pub fn headless_pid_for_session(&self, session_id: &str) -> Option<u32> {
        self.with_entry(session_id, |e| {
            e.headless_pid
                .as_ref()
                .map(|pid| pid.load(Ordering::SeqCst))
                .filter(|pid| *pid != 0)
        })
        .flatten()
    }

    /// The routine whose run `session_id` is, from its provenance.
    pub fn routine_key_for_session(&self, session_id: &str) -> Option<String> {
        self.with_provenance(session_id, |p| p.routine_key.clone())
            .flatten()
    }

    /// Stable queue authority granted by this launch, never a display role label.
    pub(crate) fn queue_owner_for_session(&self, session: &str) -> Option<String> {
        if let Some(key) = self.routine_key_for_session(session) {
            return Some(format!("routine:{key}"));
        }
        let role = self
            .with_entry(session, |entry| {
                entry
                    .meta
                    .role_join_error
                    .is_none()
                    .then(|| entry.meta.queue_role.clone())
                    .flatten()
            })
            .flatten()?;
        let team = self.launch_team_for_session(session)?;
        Some(format!("role:{}/{}/{role}", team.org, team.team))
    }

    pub(crate) fn launch_team_for_session(&self, session: &str) -> Option<LaunchTeam> {
        self.launch_team(&self.handle_for_session(session)?)
    }

    /// The handle bound to `session_id` (a story worker's handle IS its story_id), or `None` if the
    /// session isn't bound.
    pub fn handle_for_session(&self, session_id: &str) -> Option<String> {
        self.lock().by_session.get(session_id).cloned()
    }

    /// Whether `session_id` is a session CodeSense launched an agent in: a dashboard, MCP or
    /// headless launch, or one that names an agent. An app-opened terminal registered here is
    /// an ordinary shell, whatever agent is typed into it by hand.
    pub fn is_agent_launch(&self, session_id: &str) -> bool {
        self.with_entry(session_id, LaunchEntry::names_an_agent)
            .unwrap_or(false)
    }

    /// The org and team a registered terminal's workspace belongs to, for the hand-started
    /// record of a CLI typed into it: its trusted owner, else its tab's workspace team.
    pub fn terminal_owner(&self, session_id: &str) -> (Option<String>, Option<String>) {
        let owner = self.owner_for_session(session_id);
        let tab_team = self
            .with_entry(session_id, |e| e.meta.team.clone())
            .flatten();
        let org = owner
            .as_ref()
            .map(|o| o.org.clone())
            .filter(|org| !org.is_empty())
            .or_else(|| tab_team.as_ref().map(|t| t.org.clone()));
        let team = owner
            .and_then(|o| o.team)
            .or_else(|| tab_team.map(|t| t.team));
        (org, team)
    }

    /// SW-WC-2: resolve the workspace a session belongs to for workspace-chat
    /// admission. Returns `None` for an unbound session or an entry with
    /// neither a `story` nor a `worktree_dir`. Pure wrapper over
    /// `commands::workspace_chat::session_workspace_id`, so the resolver
    /// stays unit-testable without Tauri state.
    pub fn workspace_for_session(&self, session_id: &str) -> Option<String> {
        let inner = self.lock();
        let entry = inner
            .by_session
            .get(session_id)
            .and_then(|h| inner.by_handle.get(h))
            .or_else(|| {
                inner.by_handle.values().find(|e| {
                    e.remote_shell
                        .as_ref()
                        .is_some_and(|r| r.target.session.as_str() == session_id)
                })
            })?;
        chat_workspace_for_entry(entry)
    }

    pub(crate) fn bind_chat_descriptor(
        &self,
        handle: &str,
        session: &str,
        descriptor: &codesense_pty_core::layout::WorkspaceDescriptor,
    ) {
        if let Some(entry) = self
            .lock()
            .by_handle
            .get_mut(handle)
            .filter(|e| e.session_id.as_deref() == Some(session))
        {
            entry.chat_workspace_id =
                super::workspace_chat::resolve::descriptor_workspace_id(descriptor);
        }
    }

    /// Workspace membership follows the binding even when a host is disconnected.
    pub(crate) fn chat_recipients(&self, workspace: &str, except: Option<&str>) -> Vec<String> {
        self.chat_bindings()
            .into_iter()
            .filter(|(handle, room)| except != Some(handle.as_str()) && room == workspace)
            .map(|(handle, _)| handle)
            .collect()
    }
    pub(crate) fn chat_workspaces(&self) -> Vec<String> {
        self.chat_bindings()
            .into_iter()
            .map(|(_, room)| room)
            .collect()
    }
    fn chat_bindings(&self) -> Vec<(String, String)> {
        self.lock()
            .by_handle
            .iter()
            .filter(|(_, e)| e.session_id.is_some() || e.remote_shell.is_some())
            .filter_map(|(h, e)| chat_workspace_for_entry(e).map(|room| (h.clone(), room)))
            .collect()
    }

    /// A launcher id normalized the way [`begin_with_provenance`] records `launched_by`,
    /// so a filter by session id and by its handle select the same sessions.
    ///
    /// [`begin_with_provenance`]: Self::begin_with_provenance
    pub fn normalize_launched_by(&self, raw: &str) -> String {
        normalize_launcher(&self.lock(), raw)
    }

    /// The per-session turn-state cache every dot write feeds (admitted hooks and MCP
    /// signals alike).
    pub fn turns(&self) -> &crate::agent_state::SessionTurnState {
        &self.turns
    }

    /// Whether `handle` has a registry entry (reserved, running, or exited but not yet
    /// cleared): the state in which [`begin_with_provenance`] answers `Ok(false)`.
    ///
    /// [`begin_with_provenance`]: Self::begin_with_provenance
    pub fn has_handle(&self, handle: &str) -> bool {
        self.lock().by_handle.contains_key(handle)
    }

    /// Capture the recorded PTY owner; never reinterpret a headless or legacy
    /// ownerless registration as whichever local runtime now has its wire ID.
    pub(crate) fn resolve_owned_pty(
        &self,
        handle: &str,
    ) -> Option<super::pty_host::LocalEffectClaim> {
        let inner = self.lock();
        let entry = inner.by_handle.get(handle)?;
        let claim = entry.local_effect_owner.as_ref()?;
        (entry.kind != KIND_HEADLESS && entry.session_id.as_deref() == Some(&claim.session))
            .then(|| claim.clone())
    }

    /// The org and team `open_workspace` launched `handle` in, if it recorded one.
    /// A tab opened on a machine by hand records no launch team, but its tab belongs to
    /// the team whose workspace it runs in: its inbox lives there, so the mail sent to it
    /// and the mail its hooks announce stay in one store whichever team the UI shows.
    pub(crate) fn set_role_join_error(&self, handle: &str, error: String) {
        if let Some(entry) = self.lock().by_handle.get_mut(handle) {
            entry.meta.role_join_error = Some(error);
        }
    }
    pub(crate) fn set_queue_role(&self, handle: &str, role: Option<String>) {
        if let Some(entry) = self.lock().by_handle.get_mut(handle) {
            entry.meta.queue_role = role;
        }
    }

    pub fn launch_team(&self, handle: &str) -> Option<LaunchTeam> {
        self.lock()
            .by_handle
            .get(handle)
            .and_then(LaunchEntry::launch_team)
    }

    /// Record the team `handle` runs in unless one is already recorded (a rebind never
    /// moves a session's inbox). A tab on a machine already has one: the team of the
    /// workspace it runs in, which [`Self::launch_team`] falls back to; that one is kept.
    pub fn set_launch_team_if_absent(&self, handle: &str, team: LaunchTeam) {
        if let Some(entry) = self.lock().by_handle.get_mut(handle) {
            if entry.meta.team.is_none() {
                let present = entry
                    .remote_shell
                    .as_ref()
                    .and_then(|record| launch_team_from_key(&record.team_key));
                entry.meta.team = Some(present.unwrap_or(team));
            }
        }
    }

    /// What the layout last said about `handle`'s team: `Some(Some(team))` kept,
    /// `Some(None)` found none a moment ago, `None` to be read.
    pub(crate) fn known_tab_team(&self, handle: &str) -> Option<Option<LaunchTeam>> {
        match self
            .tab_teams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(handle)?
        {
            TabTeam::Kept(team) => Some(Some(team.clone())),
            TabTeam::Missed(at) if at.elapsed() < TAB_TEAM_MISS_TTL => Some(None),
            TabTeam::Missed(_) => None,
        }
    }

    /// Keep `team` as `handle`'s team, unless one is kept already. Returns the team kept.
    pub(crate) fn keep_tab_team(&self, handle: &str, team: LaunchTeam) -> LaunchTeam {
        let mut teams = self.tab_teams.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(TabTeam::Kept(kept)) = teams.get(handle) {
            return kept.clone();
        }
        teams.insert(handle.to_string(), TabTeam::Kept(team.clone()));
        team
    }

    /// A layout read found no tab for `handle`.
    pub(crate) fn tab_team_missed(&self, handle: &str) {
        let mut teams = self.tab_teams.lock().unwrap_or_else(|e| e.into_inner());
        teams.retain(|_, known| match known {
            TabTeam::Kept(_) => true,
            TabTeam::Missed(at) => at.elapsed() < TAB_TEAM_MISS_TTL,
        });
        teams
            .entry(handle.to_string())
            .or_insert(TabTeam::Missed(std::time::Instant::now()));
    }

    pub(crate) fn host_inboxes(&self) -> std::sync::Arc<super::host_inbox::HostInboxes> {
        self.host_inboxes.clone()
    }

    /// The name `open_workspace` recorded for `handle`, if any.
    pub fn display_name(&self, handle: &str) -> Option<String> {
        self.lock()
            .by_handle
            .get(handle)
            .and_then(|e| e.meta.display_name.clone())
    }

    /// Stamp `name` as the display label on `handle`'s entry. No-op when the
    /// handle is unknown. Called by registration paths (such as MCP's
    /// `open_terminal_tab`) that mint a memorable default name after a tab
    /// opens, so the agent list reads that name instead of the raw handle.
    pub fn set_display_name(&self, handle: &str, name: &str) {
        if let Some(entry) = self.lock().by_handle.get_mut(handle) {
            entry.meta.display_name = Some(name.to_string());
        }
    }

    /// Resolve the **authoritative** fire target for `handle` (AC-10/AC-12):
    /// the bound session id, or `None` if the handle has no live binding (in
    /// which case `sendToSession`/`runPrompt` is rejected rather than falling
    /// through to any caller-supplied id). ONE lookup, kind-agnostic (§2).
    pub fn resolve(&self, handle: &str) -> Option<String> {
        self.lock()
            .by_handle
            .get(handle)
            .and_then(|e| e.session_id.clone())
    }

    /// Abort a launch by `handle`, releasing its reserved slot — the
    /// failure-path counterpart to [`release_for_session`]. `begin` reserves a
    /// permit **before** the session exists, so if the open fails between
    /// `begin` and a live session (resolve/worktree/preflight/spawn error) the
    /// FE calls this to free the slot rather than leaking it. Drops the entry
    /// (and any bound session's reverse index).
    ///
    /// SW-DX-17a: returns `true` when an entry was actually removed, `false` for
    /// an unknown handle (a no-op). Callers audit the abort ONLY on `true`, so a
    /// no-op abort never writes a phantom line — and `dashboard_kill_by_handle`
    /// uses it to clear a reserved-but-unbound row (which has no session to kill).
    pub fn abort(&self, handle: &str) -> bool {
        let mut inner = self.lock();
        let removed = match inner.by_handle.remove(handle) {
            Some(entry) => {
                if let Some(sid) = entry.session_id {
                    inner.by_session.remove(&sid);
                    inner.raw_capture_taken.remove(&sid);
                    inner.headless_exited.remove(&sid);
                }
                true
            }
            None => false,
        };
        drop(inner);
        // SW-FX-launcher-persistence (step 4): an aborted reservation is never
        // live again under the same handle, so drop its on-disk record too. The
        // prune is cheap when the handle was not persisted (a reservation that
        // failed before any launched_by was recorded).
        if removed {
            self.queue_persist(handle);
        }
        removed
    }

    /// Release the slot held for `session_id` — drops the [`SpawnPermit`] and
    /// both index entries (AC-8). Called from `pty::kill_session` (the
    /// **explicit** clear/kill choke), so a natural `pty-exit` does NOT release:
    /// a dead-but-undismissed session keeps its slot until the human clears it.
    /// Unknown/non-dashboard sessions are a no-op.
    ///
    /// SW-DX-17a: returns the released [`ReleasedSession`] (owning handle + kind)
    /// when a registered session was actually dropped, so the kill choke can
    /// write ONE audit line — and `None` for a non-dashboard session (a plain
    /// terminal / warm-pool member), which must NOT be audited as an agent kill.
    pub fn release_for_session(&self, session_id: &str) -> Option<ReleasedSession> {
        // Any torn-down session (a plain tab too) loses its turn state, so a session killed
        // before its agent's `SessionEnd` hook leaves nothing behind.
        self.turns.forget(session_id);
        let mut inner = self.lock();
        // A session that ends before it is bound takes its staged launch with it, removed
        // after the lock since dropping its settings file deletes it.
        let staged = inner.staged_launches.remove(session_id);
        let Some(handle) = inner.by_session.remove(session_id) else {
            drop(inner);
            drop(staged);
            return None;
        };
        inner.headless_exited.remove(session_id);
        inner.raw_capture_taken.remove(session_id);
        // Dropping the entry drops any held SpawnPermit → frees the limit slot.
        // SW-DX-17d: report the STORED origin kind, not a permit-derived guess —
        // an adopted (permit-released) dashboard session must still audit as
        // `dashboard-launched`, not flip to `registered`.
        let removed = inner.by_handle.remove(&handle);
        // SW-OR-10: a headless entry's child is a PTY-less process group with NO
        // `PtyState` session for the kill choke to reach — reap it HERE, in the ONE
        // release choke every teardown path funnels through (per-agent Stop,
        // run/story drain, cap reclaim), so none of them can orphan the agent tree.
        // `killpg` AFTER the lock; a gone/exited group is a harmless ESRCH.
        let headless_pgid = removed
            .as_ref()
            .and_then(|e| e.headless_pid.as_ref())
            .map(|p| p.load(Ordering::SeqCst))
            .filter(|&p| p != 0);
        // SW-OR-13: capture the cancel flag before dropping the entry. Setting it (below,
        // after the lock) closes the pre-spawn race the `killpg` above cannot: while the
        // child does not yet exist, its pid is `0` and `killpg` is a no-op, but the flag is
        // observed at `spawn_capture`'s pre-spawn check and aborts the run.
        let headless_cancel = removed.as_ref().and_then(|e| e.headless_cancel.clone());
        let kind = removed
            .as_ref()
            .map(|entry| entry.kind)
            .unwrap_or(KIND_REGISTERED);
        drop(inner);
        // The entry and anything staged go after the lock: their settings files are deleted
        // on drop.
        drop(removed);
        drop(staged);
        if let Some(cancel) = headless_cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        if let Some(pgid) = headless_pgid {
            kill_process_group(pgid);
        }
        // SW-FX-launcher-persistence (step 4): the session is terminal — drop
        // its on-disk record so the file stays bounded over the long run. Marked
        // AFTER dropping the inner mutex. A persistence failure is logged and
        // swallowed; the in-memory release has already happened.
        self.queue_persist(&handle);
        Some(ReleasedSession { handle, kind })
    }

    /// SW-OR-14 (AC-3): record that `session_id` SELF-attested via `signal_done`, so the
    /// headless completion task suppresses its exit-classification emit + durable write
    /// (Signal > ProcessExit). Recorded in the `attested` set — which OUTLIVES entry removal —
    /// so the suppression holds no matter which teardown path reaps the entry first (the bug a
    /// per-entry flag has: a Stop that consumes the entry before the completion task's own
    /// release would drop the flag and clobber the `Signal` outcome). Only HEADLESS ids are
    /// recorded (a PTY session's terminal state comes from `signal_done`'s own emit, never an
    /// exit sweep, and it has no completion task to drain the set — recording it would leak).
    /// `signal_done` still calls this unconditionally; the kind gate lives here.
    ///
    /// Contract: the signal must land while the entry is still live (the normal case — the agent
    /// is blocked on the `signal_done` response, so its child is alive and the entry present). A
    /// signal that races in strictly AFTER an external Stop already reaped the entry records
    /// nothing (the kind gate sees the gone entry) — the exit classifier then wins with `failed`.
    /// That is the AC-3 residual corner (a run Stopped mid-signal surfacing `failed` is
    /// acceptable); closing it fully would require `signal_done`'s durable `Signal` write to be
    /// registry-independent too (SW-AU-52 scope), and it likewise no-ops on the reaped entry.
    /// End headless liveness as soon as process capture returns without discarding the provenance
    /// an in-flight `signal_done` still needs for its durable outcome write.
    pub fn mark_headless_exited(&self, session_id: &str) {
        let mut inner = self.lock();
        let is_headless = inner
            .by_session
            .get(session_id)
            .and_then(|handle| inner.by_handle.get(handle))
            .is_some_and(|entry| is_live_headless(entry.kind, entry.session_id.as_deref()));
        if is_headless {
            inner.headless_exited.insert(session_id.to_string());
        }
    }

    /// SW-OR-14 (AC-3): consume the self-attest record for `session_id` — returns `true` iff it
    /// self-attested, and removes it (the headless completion task is the sole, one-shot drainer,
    /// so the set never accumulates). `false` for a run that never signaled → the caller emits
    /// its exit classification.
    pub fn take_signal_attested(&self, session_id: &str) -> bool {
        self.lock().attested.remove(session_id)
    }

    /// Share the launch owner's agent distinction with projection consumers.
    pub(crate) fn names_an_agent(&self, handle: &str) -> bool {
        self.lock()
            .by_handle
            .get(handle)
            .is_some_and(LaunchEntry::names_an_agent)
    }

    /// SW-DX-17a: snapshot every live registry entry for `dashboard_list_sessions`.
    /// Clones the `(handle, sessionId, kind)` triples under the lock and returns
    /// immediately — liveness is probed by the caller AFTER the lock is dropped
    /// (never hold the registry lock across a `PtyState` probe). Iterates the ONE
    /// `by_handle` map (§2) — dashboard-launched and cap-free `register`ed alike.
    pub fn snapshot(&self) -> Vec<SessionSnapshot> {
        let inner = self.lock();
        inner
            .by_handle
            .iter()
            .map(|(handle, entry)| SessionSnapshot {
                output_run_id: entry
                    .remote_shell
                    .as_ref()
                    .and_then(|record| record.output_run_id()),
                handle: handle.clone(),
                session_id: entry.session_id.clone(),
                launcher_task: entry.launcher_task.clone(),
                story_was_explicit: entry.story_was_explicit,
                // SW-DX-17d: STORED origin, decoupled from the (now backgrounded-
                // meaning) permit.
                kind: entry.kind,
                backgrounded: entry._permit.is_some(),
                started_with: entry.started_with.clone(),
                started_at_ms: entry.started_at_ms,
                launcher: entry.launcher.clone(),
                account_provenance: entry.account_provenance.clone(),
                provenance: entry.provenance.clone(),
                // SW-OR-19: load the anchor under the same lock as the rest of the snapshot
                // (a cheap `Relaxed` read — a standalone timestamp, no cross-atomic ordering
                // needed). The `0` seed means "no output yet" → surface `None`, NOT `Some(0)`
                // (which the FE would misrender as a ~56-year-old "last output"). `None` for
                // every non-headless entry.
                last_output_ms: entry
                    .last_drain_ms
                    .as_ref()
                    .map(|a| a.load(Ordering::Relaxed))
                    .filter(|&ms| ms != 0),
                meta: entry.meta.clone(),
                worktree_dir: entry.worktree_dir.clone(),
                agent_revision: entry.agent_revision.clone(),
                // enumerate the deadline + grace fields so a lister
                // can render `deadlineAtMs` / `expiring` without taking the
                // registry lock a second time.
                deadline_at_ms: entry.deadline_ms,
                exit_sent_at_ms: entry.exit_sent_at_ms,
                machine_id: entry
                    .remote_shell
                    .as_ref()
                    .map(|record| record.machine_id.clone()),
                launch_team_key: entry
                    .remote_shell
                    .as_ref()
                    .map(|record| record.team_key.clone()),
            })
            .collect()
    }

    /// SW-DX-17d (AC-1/AC-3b): **bring to foreground** — reattach a backgrounded
    /// session to a tab. Releases its limit slot (drops the [`SpawnPermit`]) while
    /// KEEPING the registry entry (still addressable) and its
    /// STORED `kind`. Idempotent: foregrounding an entry that holds no permit (never
    /// backgrounded, or already foreground) is a no-op. The FE opens the tab
    /// separately — this call only fixes the accounting so an attended agent stops
    /// counting against the background limit.
    ///
    /// Returns `true` when a permit was ACTUALLY released (a real background→
    /// foreground transition), `false` for a no-op (unknown handle, or an entry
    /// that already held none). SW-AM-8 gates its lifecycle emit on this so a
    /// no-op foreground doesn't fire a spurious roster refetch — mirroring the
    /// `abort`-returns-bool gating.
    pub fn foreground(&self, handle: &str) -> bool {
        let mut inner = self.lock();
        match inner.by_handle.get_mut(handle) {
            // `take()` drops the permit (frees the limit slot via RAII) and tells us
            // whether one was actually held.
            Some(entry) => entry._permit.take().is_some(),
            None => false,
        }
    }

    /// SW-SR-5 (AC-2): the stop list for **one run's** workers, the entries whose SR-3
    /// provenance stamp carries `run_id` ([`provenance_run_id_matches`]), collected in ONE
    /// lock pass. A bound entry's `session_id` goes to `to_kill` (routed through
    /// `pty::kill_session` AFTER the lock drops — never under it, AC-5); a
    /// reserved-but-unbound entry (a `begin` that never `bind`ed) has no PTY, so it is
    /// aborted in place (entry dropped to free the leaked slot) and its handle returned in
    /// `aborted`. In-flight worktrees are left recoverable — drain-all, no cascade.
    pub fn drain_run(&self, run_id: &str) -> RunStopTargets {
        let mut inner = self.lock();
        // Snapshot the matching handles first — can't mutate `by_handle` while iterating it.
        let matching: Vec<String> = inner
            .by_handle
            .iter()
            .filter(|(_, e)| provenance_run_id_matches(e, run_id))
            .map(|(h, _)| h.clone())
            .collect();
        let mut targets = RunStopTargets::default();
        for handle in matching {
            match inner
                .by_handle
                .get(&handle)
                .and_then(|e| e.session_id.clone())
            {
                Some(sid) => targets.to_kill.push(sid),
                None => {
                    inner.by_handle.remove(&handle);
                    targets.aborted.push(handle);
                }
            }
        }
        targets
    }

    /// SW-DX-17d (AC-3): set the user-owned background-agent limit live. Pushed
    /// from the FE settings hook; non-destructive when lowered (see
    /// [`SpawnCap::set_limit`]).
    pub fn set_limit(&self, limit: usize) {
        self.cap.set_limit(limit);
    }

    /// Free cap slots (test/observability).
    #[allow(dead_code)] // observability / test assertions only.
    pub fn available(&self) -> usize {
        self.cap.available()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // A poisoned lock means a prior panic while holding it; recover the guard
        // rather than propagate — the registry is best-effort bookkeeping, and a
        // poisoned cap would otherwise wedge every future launch.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Default for DashboardLaunchState {
    fn default() -> Self {
        Self::new(DEFAULT_SPAWN_CAP)
    }
}

// ── Tauri commands (thin wrappers over the methods above) ───────────────────

/// SW-AM-8: the Tauri event announcing a session-registry lifecycle transition
/// (create / foreground / kill), so the Agents roster hooks
/// refetch WITHOUT a manual Refresh. Emitted from the command layer — which owns
/// the `AppHandle` — rather than from the pure `DashboardLaunchState` methods
/// (they hold no handle and are built bare in unit tests). Mirrors the `pty-exit`
/// posture exactly (see `pty::…` emit). ONE const so the emit and the FE
/// subscribers (`session-events.ts`) can't typo-drift.
pub const SESSION_STATE_CHANGED: &str = "session-state-changed";

/// Fire-and-forget the lifecycle event. Cheap and off the UI path — it touches no
/// PTY and no filesystem; the FE handler does the (debounced, async) re-snapshot.
/// A dropped emit is harmless: the manual Refresh button remains the fallback.
pub(crate) fn emit_session_state_changed<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    use tauri::Emitter;
    let _ = app.emit(SESSION_STATE_CHANGED, ());
}

fn chat_workspace_for_entry(entry: &LaunchEntry) -> Option<String> {
    if let Some(remote) = &entry.remote_shell {
        return super::workspace_chat::resolve::remote_workspace_id(
            remote
                .identity
                .story
                .as_deref()
                .or(remote.workspace_story_id.as_deref()),
            &remote.workspace_id,
        );
    }
    entry.chat_workspace_id.clone().or_else(|| {
        super::workspace_chat::session_workspace_id(
            entry.meta.story.as_deref(),
            entry.worktree_dir.as_deref(),
        )
    })
}

/// Capture workspace context without changing launch provenance or story identity.
pub(crate) async fn refresh_chat_binding(app: &tauri::AppHandle, session: &str) {
    use super::machines::RuntimeLayoutReply;
    let Some(state) = app.try_state::<DashboardLaunchState>() else {
        return;
    };
    if state.remote_handle_for_session(session).is_some() {
        return;
    }
    let Some(Ok(layouts)) = super::pty_layout::webview_local_layout_snapshots(app.clone()).await
    else {
        return;
    };
    let descriptor = layouts.into_iter().find_map(|reply| match reply {
        RuntimeLayoutReply::Live { snapshot, .. } => snapshot
            .workspaces
            .into_values()
            .find(|w| {
                w.tabs
                    .iter()
                    .any(|t| t.session.as_str() == session && !t.state.is_exited())
            })
            .map(|w| w.descriptor),
        _ => None,
    });
    let Some(descriptor) =
        descriptor.filter(|d| super::workspace_chat::resolve::descriptor_workspace_id(d).is_some())
    else {
        return;
    };
    let handle = match DashboardLaunchState::handle_for_session(&state, session) {
        Some(handle) => handle,
        None => {
            let (app_task, sid) = (app.clone(), session.to_string());
            let Ok(claim) = await_blocking("chat.bind_owner", move || {
                super::pty_backend::capture_local_effect_claim(&app_task, &sid)
            })
            .await
            else {
                return;
            };
            if super::pty_backend::commit_local_effect_claim(app, &claim, || {
                DashboardLaunchState::register_owned(
                    &state,
                    session,
                    session,
                    None,
                    Some(claim.clone()),
                )
            })
            .is_err()
            {
                return;
            }
            if let (Some(org), Some(team)) = (&descriptor.org, &descriptor.team) {
                state.set_launch_team_if_absent(
                    session,
                    LaunchTeam {
                        org: org.clone(),
                        team: team.clone(),
                    },
                );
            }
            session.to_string()
        }
    };
    state.bind_chat_descriptor(&handle, session, &descriptor);
}

/// AC-8: reserve a cap slot for a dashboard launch. `busy` when saturated.
/// Resolves `true` when this call freshly reserved the slot, `false` when the
/// story was already launching/launched (the FE then focuses, and knows not to
/// abort a launch it doesn't own).
#[tauri::command]
pub fn dashboard_launch_begin(
    state: tauri::State<'_, DashboardLaunchState>,
    handle: String,
) -> Result<bool, String> {
    state.begin(&handle).map_err(|e| e.to_string())
}

/// The session record of a bound launch: what the launch registry holds for it (who it belongs
/// to, its account, run, story and worktree) with what trusted preparation knew ([`LaunchFacts`]).
/// Every launch path composes its record here; a fact neither knew says why it is missing.
pub(crate) fn record_from_launch(
    session_id: &str,
    snapshot: &SessionSnapshot,
    owner: Option<&SessionOwner>,
    facts: &crate::session_usage::record::LaunchFacts,
) -> crate::session_usage::record::SessionRecord {
    let mut record = crate::session_usage::record::SessionRecord::new(session_id);
    // The trusted launch owner names the org whether or not the launch has a team.
    let owner_org = owner.map(|o| o.org.as_str()).filter(|org| !org.is_empty());
    record.org = owner_org.map(str::to_owned).or_else(|| snapshot.meta.team.as_ref().map(|t| t.org.clone()));
    record.team = owner
        .and_then(|o| o.team.clone())
        .or_else(|| snapshot.meta.team.as_ref().map(|t| t.team.clone()));
    record.handle = Some(snapshot.handle.clone());
    record.launched_by = snapshot.meta.launched_by.clone().or_else(|| snapshot.launcher.clone());
    record.account = snapshot.account_provenance.clone().map(|a| crate::session_usage::record::RecordAccount {
        provider: Some(a.provider).filter(|p| !p.is_empty()),
        account_id: a.account_id,
        label: a.label,
        source: Some(a.source).filter(|s| !s.is_empty()),
    });
    if let Some(provenance) = &snapshot.provenance {
        record.run_id = Some(provenance.run_id.clone());
    }
    record.story_id = snapshot.meta.story.clone();
    record.worktree = snapshot.worktree_dir.as_ref().map(|d| d.to_string_lossy().into_owned());
    record.agent = facts.agent.clone().or_else(|| snapshot.agent_revision.clone());
    record.manifest_hash = facts
        .manifest_hash
        .clone()
        .or_else(|| snapshot.provenance.as_ref().and_then(|p| p.manifest_hash.clone()));
    record.cli = facts.cli.clone();
    record.cli_version = facts.cli_version.clone();
    record.model = facts.model.clone();
    record.native_session_id = facts.native_session_id.clone();
    record.mcp_allow = facts.mcp_allow.clone();
    record.bridge_tools = facts.bridge_tools.clone();
    record.workspace = facts.workspace.clone();
    // The CLI keeps its files where the launch's own environment puts them (a scoped Codex home),
    // else in its account's home, else in its default home.
    record.config_home = facts
        .scope_home
        .clone()
        .or_else(|| snapshot.account_provenance.as_ref().and_then(|a| a.home.clone()))
        .or_else(|| facts.config_home.clone());
    record.provided_skills = facts.provided_skills.clone();
    for (missing, field, reason) in [
        (record.native_session_id.is_none(), "nativeSessionId", "the CLI reports its own session id with its first telemetry"),
        (record.cli_version.is_none(), "cliVersion", "the CLI's version was not read at launch"),
        (record.model.is_none(), "model", "no model was requested; the CLI chose its default"),
        (record.account.is_none(), "account", "no account was staged for this launch"),
        (record.org.is_none(), "org", "the launch is outside any org"),
        (record.team.is_none(), "team", "the launch is outside any team"),
        (record.run_id.is_none(), "runId", "the launch belongs to no run"),
        (record.story_id.is_none(), "storyId", "the launch belongs to no story"),
        (record.worktree.is_none(), "worktree", "the launch has no managed worktree"),
    ] {
        if missing {
            record.unavailable(field, reason);
        }
    }
    // Managed Claude settings send this session's usage elsewhere, so none will be recorded.
    let runtime = record.cli.as_deref().and_then(|cli| cli.parse::<codesense_pty_core::agent_cli::AgentCli>().ok());
    if runtime.is_some_and(crate::session_usage::managed_telemetry::unrecorded) {
        record.unavailable("usage", crate::session_usage::managed_telemetry::USAGE_REDIRECTED);
    }
    record.missing_as("the launch did not report it");
    record
}

/// What a bound launch writes: its session record and what its task is made from.
pub(crate) struct LaunchWrite {
    pub record: crate::session_usage::record::SessionRecord,
    pub task: crate::session_usage::task_capture::TaskInputs,
    pub routine: Option<crate::session_usage::task_capture::RoutineBrief>,
    /// The launching session's task id, captured at reservation: handles are reused.
    pub launcher: Option<String>,
    workspace: Option<String>,
    story_was_explicit: bool,
}

/// Compose what a bound launch writes from the registry and the facts its path held.
pub(crate) fn compose_launch_write(
    state: &DashboardLaunchState,
    session_id: &str,
    handle: &str,
    facts: crate::session_usage::record::LaunchFacts,
) -> Option<LaunchWrite> {
    let snapshot = state.snapshot().into_iter().find(|s| s.handle == handle)?;
    let owner = state.owner_for_session(session_id);
    let record = record_from_launch(session_id, &snapshot, owner.as_ref(), &facts);
    // What the task is typed and linked from, beyond the brief: the launch's role, its saved
    // agent's queue role, the task it reviews and the session it continues.
    let mut task = facts.task;
    task.role = snapshot.meta.role.clone();
    task.queue_role = snapshot.meta.queue_role.clone();
    task.reviews = snapshot.meta.reviews.clone();
    task.continues = snapshot.meta.continues.clone();
    Some(LaunchWrite {
        routine: snapshot.provenance.as_ref().and_then(|p| p.routine.clone()),
        launcher: snapshot.launcher_task,
        story_was_explicit: snapshot.story_was_explicit,
        record,
        task,
        workspace: facts.workspace,
    })
}

/// The common writer without an app-backed source resolver, for isolated store tests.
#[cfg(test)]
pub(crate) fn write_launch_record(
    state: &DashboardLaunchState,
    session_id: &str,
    handle: &str,
    facts: crate::session_usage::record::LaunchFacts,
) {
    write_launch_record_resolving(state, session_id, handle, facts, |_, _| async { None });
}

/// Use the launch's own team to resolve its story, independently of the selected UI team.
pub(crate) fn write_launch_record_for_app<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    state: &DashboardLaunchState,
    session_id: &str,
    handle: &str,
    facts: crate::session_usage::record::LaunchFacts,
) {
    let app = app.clone();
    write_launch_record_resolving(state, session_id, handle, facts, move |record, story_was_explicit| async move {
        let story = record.story_id.as_deref()?;
        let org = record.org.as_deref()?;
        let team = record.team.as_deref()?;
        let Ok((home, user_id)) = super::route_to_context::resolve_ambient(&app).await else {
            return None;
        };
        let active: super::route_to_context::ActiveContext = launch_story_context(&app, home, user_id);
        let scope = crate::context_repo::caller_scope::CallerScope::team(
            &active.codesense_home, &active.principal(),
            &LaunchTeam { org: org.to_string(), team: team.to_string() },
        );
        let mut link = crate::session_usage::StoryLink {
            story_id: story.to_string(), org: org.to_string(), source: PathBuf::new(),
        };
        let resolved = await_blocking("launch_record.story_source", move || {
            match resolve_story_path_in_org_for(&app, &active, &link.org, &link.story_id, &scope) {
                Ok(resolved) => {
                    link.source = resolved.story_file_path.into();
                    Ok(Some(link))
                }
                Err(_) if story_was_explicit => Ok(Some(link)),
                Err(error) => Err(error),
            }
        }).await;
        match resolved {
            Ok(link) => link,
            Err(error) => {
                log::warn!(target: "session_usage", "launch story source not resolved: {error}");
                None
            }
        }
    });
}

/// The canonical resolver needs to know which org its resident cache holds. This is
/// cache identity only; the target story and its scope always come from the launch.
fn launch_story_context<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    codesense_home: PathBuf,
    user_id: u64,
) -> super::route_to_context::ActiveContext {
    let org_slug = app.try_state::<crate::mcp::state::ActiveTeamState>()
        .and_then(|state| state.get()).map(|active| active.org_slug).unwrap_or_default();
    super::route_to_context::ActiveContext { codesense_home, user_id, org_slug, team_name: None }
}

/// The one writer of a launch's session record and task: the MCP, renderer and headless launch
/// paths all call it once the session is bound, with the facts their preparation held. Best
/// effort: a failure is logged and the launch goes on. The branch is read off the launch path.
fn write_launch_record_resolving<F, Fut>(
    state: &DashboardLaunchState,
    session_id: &str,
    handle: &str,
    facts: crate::session_usage::record::LaunchFacts,
    resolve_story: F,
) where
    F: FnOnce(crate::session_usage::record::SessionRecord, bool) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Option<crate::session_usage::StoryLink>> + Send,
{
    let Some(LaunchWrite { mut record, task, routine, launcher, workspace, story_was_explicit }) =
        compose_launch_write(state, session_id, handle, facts)
    else {
        return;
    };
    let task_id = routine.as_ref().and(record.run_id.as_deref()).unwrap_or(session_id);
    let captured = state.begin_task_capture(session_id, task_id);
    let path = task_store_path(Some(state));
    #[cfg(test)]
    let capture_gate = state.task_capture_gate.lock().unwrap().clone();
    let pending = state.task_captures.clone();
    let capture_session = session_id.to_string();
    let pending_record = path.as_ref().ok().map(|path| {
        crate::session_usage::record::begin_record_write(path, session_id)
    });
    tokio::spawn(async move {
        let _pending_record = pending_record;
        #[cfg(test)]
        if let Some(mut gate) = capture_gate { let _ = gate.changed().await; }
        let branch = match workspace {
            Some(dir) => await_blocking("launch_record.branch", move || {
                Ok::<_, String>(crate::session_usage::record::current_branch(Path::new(&dir)))
            })
            .await
            .ok()
            .flatten(),
            None => None,
        };
        match branch {
            Some(branch) => {
                record.branch = Some(branch);
                record.unavailable.remove("branch");
            }
            None => record.unavailable("branch", "the working folder is on no branch"),
        }
        let session_path = path.clone();
        let session_record = record.clone();
        let session_result = await_blocking("launch_record.session", move || {
            crate::session_usage::record::persist_record_in(&session_path?, &session_record)
        }).await;
        if let Err(error) = session_result {
            log::warn!(target: "session_usage", "session record not written: {error}");
        }
        let story = resolve_story(record.clone(), story_was_explicit).await;
        // Resolve the actual launch cwd first in either arrival order. Once a link exists,
        // INSERT OR IGNORE keeps it, including a link the usage collector already recorded.
        let story_path = path.clone();
        let cwd = record.worktree.clone().or_else(|| record.workspace.clone());
        let story_session = record.session_id.clone();
        let story_result = await_blocking("launch_record.story", move || {
            let path = story_path?;
            let worktree = path.parent().and_then(|home| {
                cwd.as_deref().and_then(|cwd| crate::session_usage::StoryResolver::for_home(home, Default::default()).worktree_link(cwd))
            });
            if let Some(link) = worktree.or(story) {
                crate::session_usage::UsageStore::open(&path)?.link_story(&story_session, &link)?;
            }
            Ok::<_, String>(())
        }).await;
        if let Err(error) = story_result {
            log::warn!(target: "session_usage", "launch story link not written: {error}");
        }
        crate::session_usage::task_capture::record_task_async(path, record, task, routine, launcher).await;
        pending.lock().unwrap().remove(&capture_session);
        drop(captured);
    });
}

/// AC-12: bind the spawned session id to the story (stamped trusted-side).
/// SW-GW-8: `launcher` is the launching dashboard's `docPath` (provenance).
/// `team_key` is where the frontend launched the session, so a finished interactive agent can
/// be matched by the routines it is in reach of.
/// `agent_revision` (`<key>@<revision>`) is the named agent the session runs; it is recorded
/// only when that revision exists and resolves for the signed-in account, and otherwise
/// dropped with a warning (the launch stands). A settings file staged for the session by
/// its agent launch moves onto the session's entry here.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn dashboard_launch_bind(
    app: tauri::AppHandle,
    state: tauri::State<'_, DashboardLaunchState>,
    handle: String,
    session_id: String,
    launcher: Option<String>,
    team_key: Option<String>,
    agent_revision: Option<String>,
) -> Result<(), String> {
    let owner = {
        let app = app.clone();
        let session = session_id.clone();
        await_blocking("dashboard_launch_bind.owner", move || {
            super::pty_backend::capture_local_effect_claim(&app, &session)
        })
        .await?
    };
    let accounts = app.state::<super::agent_accounts::AgentAccountRuntimeState>();
    let account_provenance = accounts.owned_provenance(&owner).map(Into::into);
    // Resolved before the bind so no await separates the bind from its stamps: a respawned
    // session must never be seen carrying the superseded session's owner.
    let principal = if team_key.is_some() {
        launching_principal(&app).await
    } else {
        None
    };
    // An agent launch this app prepared for the session is the trusted record of what it
    // runs; a revision named by the renderer alone is checked against the agent store.
    // Peeked here and taken only once the bind commits, so a failed bind never deletes the
    // settings file a live agent was started with.
    let agent_revision = match staged_bind_revision(&state, &session_id, agent_revision.as_deref())
    {
        Some(prepared) => Some(prepared),
        None => verified_bind_revision(&app, agent_revision).await?,
    };
    // SW-WA-F11: capture the agent id for the FE-path telemetry record BEFORE
    // `agent_revision` is moved into `record_bound_launch` below.
    let fe_agent_id = fe_agent_telemetry_id(agent_revision.as_deref());
    // Read before `record_bound_launch` takes the staged launch.
    let staged_binary = state.staged_agent_binary(&session_id);
    let staged_facts = state.staged_launch_facts(&session_id);
    let fresh = state.resolve(&handle).as_deref() != Some(session_id.as_str());
    super::pty_backend::commit_local_effect_claim(&app, &owner, || {
        bind_supervised(
            &state,
            app.try_state::<crate::mcp::session_status::SessionOutcomes>()
                .as_deref(),
            &handle,
            &session_id,
            launcher.as_deref(),
            account_provenance,
            Some(owner.clone()),
        )
    })?;
    accounts.take_owned_provenance(&owner);
    record_bound_launch(
        &state,
        &handle,
        &session_id,
        fresh,
        principal,
        team_key.as_deref(),
        agent_revision,
    );
    if fe_agent_id != "shell" {
        write_launch_record_for_app(&app, &state, &session_id, &handle, staged_facts.unwrap_or_default());
        crate::session_usage::bind_launch_async(session_id.clone(), handle.clone()).await;
    }
    // The tab carries what the registry now holds, so an app restart that keeps the
    // session server can restore this launch. Best-effort: never fails the bind.
    refresh_chat_binding(&app, &session_id).await;
    tag_bound_launch(&app, &session_id).await;
    // SW-WA-F11: FE-driven launches also get a telemetry record so
    // `list_sessions` carries `launchTelemetry` for human-driven agents
    // (otherwise AC-3 would be systematically biased: every MCP launch would
    // carry a record and every FE launch would read `null`). The FE
    // orchestrates settle / inject / wait-live itself and only calls Rust at
    // bind-time, so the trusted-side timing fields are unknown here —
    // shell-mode with explicit-null timings tells the retrospect jq this is
    // a shell-path launch whose fine-grained timings were measured FE-side
    // (and future F12/F13 can plumb them through this verb if useful).
    //
    // Audit is spawned so the launch's return never waits on JSONL I/O
    // (parity with the MCP path's AC-5 contract).
    let previous = if fresh {
        None
    } else {
        app.try_state::<crate::commands::launch_telemetry::LaunchTelemetryTracker>()
            .and_then(|tracker| tracker.latest_many(&[&session_id]).remove(&session_id))
    };
    let fe_record = fe_bind_telemetry(
        session_id.clone(),
        fe_agent_id,
        staged_binary.as_ref(),
        previous,
    );
    if let Some(tracker) =
        app.try_state::<crate::commands::launch_telemetry::LaunchTelemetryTracker>()
    {
        tracker.complete(fe_record.clone());
    }
    let fe_handle = handle.clone();
    let fe_session_id = session_id.clone();
    tokio::spawn(async move {
        agent_audit::append_with_telemetry(
            AuditEvent::Spawn,
            &fe_handle,
            Some(&fe_session_id),
            KIND_DASHBOARD_LAUNCHED,
            &fe_record,
        );
    });
    // SW-AM-8: the session just became live/addressable — the "appears" moment for
    // the roster (bind, not begin: a reserved-but-unbound row shows as dead, and
    // bind follows within ms; the failure path is covered by `dashboard_launch_abort`).
    emit_session_state_changed(&app);
    // SW-WC-2: on a fresh bind, emit the "Session {name} joined" system row to
    // the workspace chat. Idempotent on re-bind (guarded by `fresh`).
    if fresh {
        emit_session_joined_system_message(&app, &state, &handle, &session_id);
    }
    Ok(())
}

pub(crate) fn bind_supervised(
    state: &DashboardLaunchState,
    outcomes: Option<&crate::mcp::session_status::SessionOutcomes>,
    handle: &str,
    session_id: &str,
    launcher: Option<&str>,
    account_provenance: Option<AgentAccountProvenance>,
    owner: Option<super::pty_host::LocalEffectClaim>,
) -> Result<(), String> {
    state.bind_owned(handle, session_id, launcher, account_provenance, owner)?;
    if let Some(outcomes) = outcomes {
        // Architecture: supervision begins at the same successful bind as armLaunchStatus;
        // restoring a parked launch must not manufacture a new running task.
        let observation = state.turns().observation(session_id);
        outcomes.arm_launch(
            session_id,
            observation.map_or(0, |p| p.generation),
            observation
                .map(|p| p.state.terminal_dot_wire())
                .filter(|s| *s != "idle"),
        );
    }
    Ok(())
}

/// SW-WC-2: emit a `Session joined` system row into the chat of the workspace
/// `handle` is bound to. Best-effort — a chat engine failure never fails the
/// bind path. No-op when the session's binding does not resolve to a
/// workspace (unbound tab / plain shell).
pub(crate) fn emit_session_joined_system_message(
    app: &tauri::AppHandle,
    state: &DashboardLaunchState,
    handle: &str,
    session_id: &str,
) {
    use tauri::Manager;
    let Some(workspace_id) = state.workspace_for_session(session_id) else {
        return;
    };
    let display = state
        .display_name(handle)
        .unwrap_or_else(|| handle.to_string());
    let store = app.state::<super::workspace_chat::WorkspaceChatStore>();
    if !store.is_installed() {
        return;
    }
    super::host_inbox::mail_stored(app, handle);
    // Chain `Chat started.` and `Session joined.` into one spawned task so
    // the two rows are strictly ordered: a fresh workspace must see
    // `Chat started.` (message_id=1) BEFORE `Session joined.` (CR2-2).
    // The previous approach fired two independent `tauri::async_runtime::spawn`
    // tasks whose completion order is non-deterministic — on a cold bind
    // the join row could land first and the "day one" anchor was lost (or
    // worse: `ensure_chat_started`'s check saw the join row already in
    // place and never wrote the anchor at all). Fire-and-forget on the
    // outer spawn preserves CR-4: a slow chat-store fsync does not extend
    // the bind path.
    let app_task = app.clone();
    let ws = workspace_id.clone();
    let text = format!("Session {display} joined.");
    tauri::async_runtime::spawn(async move {
        super::workspace_chat::ensure_chat_started(&app_task, &ws).await;
        if let Err(e) = super::workspace_chat::emit_system_message(&app_task, &ws, text).await {
            log::warn!(
                target: "workspace_chat",
                "could not emit session_joined for {ws}: {e}"
            );
        }
    });
}

/// The agent revision a bind records for `session_id` when its agent launch staged one: the
/// prepared revision, whatever the renderer named (`named`). `None` when nothing was staged,
/// so a renderer-named revision must be verified instead. The staged launch stays in place
/// until the bind commits, so a failed bind never deletes the settings file a live agent was
/// started with.
pub(crate) fn staged_bind_revision(
    state: &DashboardLaunchState,
    session_id: &str,
    named: Option<&str>,
) -> Option<String> {
    let prepared = state.staged_agent_revision(session_id)?;
    if named.is_some_and(|named| named != prepared) {
        log::warn!(target: "agents", "bind named {named:?} for a session prepared as {prepared:?}; recording the prepared one");
    }
    Some(prepared)
}

/// What a committed bind of `session_id` to `handle` records on the entry: where it was
/// launched, the agent revision it runs, and what its agent launch
/// staged (the settings file and the scoped-home entries a relaunch writes again). A fresh
/// session never inherits the superseded one's stamps, even when this bind says nothing
/// about where it ran.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_bound_launch(
    state: &DashboardLaunchState,
    handle: &str,
    session_id: &str,
    fresh: bool,
    principal: Option<String>,
    team_key: Option<&str>,
    agent_revision: Option<String>,
) {
    let staged = state.take_staged_agent_launch(session_id);
    if fresh || team_key.is_some() {
        stamp_bound_launch(state, handle, fresh, principal, team_key);
    }
    // After the stamps: a fresh bind's restamp clears whatever the superseded session held.
    if let Some(agent_revision) = agent_revision {
        state.set_agent_revision_if_absent(handle, agent_revision);
    }
    let Some(staged) = staged else { return };
    if let Some(cwd) = staged.cwd {
        state.set_worktree_dir_if_absent(handle, cwd);
    }
    if fresh {
        state.set_queue_role(handle, staged.queue_role);
    }
    if fresh && staged.capture_raw_terminal {
        state.enable_raw_terminal_capture_for_fresh_launch(handle);
    }
    if let Some(settings) = staged.settings {
        state.hold_launch_settings(handle, settings);
    }
}

/// The renderer-named agent revision a bind may record: kept only when that revision exists
/// and resolves for the signed-in account, otherwise dropped with a warning.
async fn verified_bind_revision(
    app: &tauri::AppHandle,
    label: Option<String>,
) -> Result<Option<String>, String> {
    let Some(label) = label.filter(|label| !label.trim().is_empty()) else {
        return Ok(None);
    };
    match super::route_to_context::derive_active_context(app).await {
        Ok(ctx) => {
            let (home, principal) = (ctx.codesense_home.clone(), ctx.principal());
            await_blocking("dashboard_launch_bind.agent_revision", move || {
                Ok(super::agents_launch::agent_revision_for_bind(
                    &home, &principal, &label,
                ))
            })
            .await
        }
        Err(error) => {
            log::warn!(target: "agents", "not recording agent revision {label:?} on a launch: {error}");
            Ok(None)
        }
    }
}

/// SW-DX-17a: the ONE audit line for a reserved-but-unbound teardown — a `begin`
/// that never `bind`ed, freed via [`DashboardLaunchState::abort`]. Every site that
/// aborts such a reservation writes the SAME line (Abort, no session id, always
/// `dashboard-launched` — only a dashboard launch can be in the reserved-but-unbound
/// state): the FE failure-path [`dashboard_launch_abort`], the per-agent
/// [`dashboard_kill_by_handle`] fallback, and the run-scoped stop. Converged here so
/// they can never drift on event/kind.
fn audit_unbound_abort(handle: &str) {
    agent_audit::append(AuditEvent::Abort, handle, None, KIND_DASHBOARD_LAUNCHED);
}

/// SW-WA-F11 — the agent id a FE-driven launch's telemetry record carries.
/// The FE passes an `agent_revision` of the form `"<key>@<revision>"` when a
/// saved Agent is being launched; strip the revision and keep the key so the
/// field matches the MCP path's `agent_telemetry_id`. A `None`/malformed
/// revision falls back to `"shell"` — the FE path only launches via a shell
/// prelude today (there is no argv-mode on the FE spine).
fn fe_agent_telemetry_id(agent_revision: Option<&str>) -> String {
    agent_revision
        .and_then(crate::agents::manifest::AgentRevisionRef::key_of)
        .map(|key| key.to_string())
        .unwrap_or_else(|| "shell".to_string())
}

/// The telemetry record a frontend bind writes: a shell-mode success whose fine-grained
/// timings were measured renderer-side, carrying the agent CLI binary the backend
/// resolved for the launch when one was staged (absent for a plain shell).
pub(crate) fn fe_bind_telemetry(
    session_id: String,
    agent_id: String,
    staged_binary: Option<&super::deps::ResolvedAgentBinary>,
    previous: Option<crate::commands::launch_telemetry::LaunchTelemetry>,
) -> crate::commands::launch_telemetry::LaunchTelemetry {
    // Rebinding the same session consumes no new prepared launch. Preserve its
    // original provenance and timings rather than manufacturing another launch.
    if staged_binary.is_none() {
        if let Some(record) = previous.filter(|record| record.session_id == session_id) {
            return record;
        }
    }
    let mut builder = crate::commands::launch_telemetry::LaunchTelemetryBuilder::start(
        session_id,
        agent_id,
        crate::commands::launch_telemetry::LaunchMode::Shell,
    );
    if let Some(binary) = staged_binary {
        builder.set_binary(&binary.path, binary.version.as_deref());
    }
    builder.finish(
        crate::commands::launch_telemetry::LaunchOutcome::Succeeded,
        None,
    )
}

/// AC-8: abort a reserved launch on the FE failure path, freeing its cap slot.
#[tauri::command]
pub fn dashboard_launch_abort(
    app: tauri::AppHandle,
    state: tauri::State<'_, DashboardLaunchState>,
    handle: String,
) {
    // SW-DX-17a: audit ONLY when an entry was actually freed — a no-op abort
    // (unknown/already-gone handle) must not write a phantom line that would make
    // the trail claim a session was aborted when nothing was.
    if state.abort(&handle) {
        audit_unbound_abort(&handle);
        // SW-AM-8: a reserved-but-unbound row was dropped (has no PTY, so `pty-exit`
        // never fires for it) → tell the roster to drop it.
        emit_session_state_changed(&app);
    }
}

/// SW-DX-11: register an externally-spawned (app-opened epic/context) terminal
/// under `handle`, CAP-FREE, so `sendToSession` can address it. The FE calls this
/// right after such a terminal's PTY session id is known. Anti-forgery is
/// preserved: the trusted side owns the map; the doc supplies only an opaque
/// handle to `sendToSession`, never a session id.
#[tauri::command]
pub async fn dashboard_register_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, DashboardLaunchState>,
    handle: String,
    session_id: String,
    launcher: Option<String>,
    team_key: Option<String>,
) -> Result<(), String> {
    let owner = {
        let app = app.clone();
        let session = session_id.clone();
        await_blocking("dashboard_register_session.owner", move || {
            super::pty_backend::capture_local_effect_claim(&app, &session)
        })
        .await?
    };
    let registered = super::pty_backend::commit_local_effect_claim(&app, &owner, || {
        state.register_owned(
            &handle,
            &session_id,
            launcher.as_deref(),
            Some(owner.clone()),
        )
    })?;
    if !registered {
        // The session runs a launched agent; it keeps that launch's handle.
        return Ok(());
    }
    if let Some(team) = team_key.as_deref().and_then(launch_team_from_key) {
        state.set_launch_team_if_absent(&handle, team);
    }
    refresh_chat_binding(&app, &session_id).await;
    tag_bound_launch(&app, &session_id).await;
    super::host_inbox::mail_stored(&app, &handle);
    // SW-DX-17a: a cap-free app-opened terminal became addressable → audit it.
    agent_audit::append(
        AuditEvent::Spawn,
        &handle,
        Some(session_id.as_str()),
        KIND_REGISTERED,
    );
    // SW-AM-8: an externally-spawned session just entered the roster → make it live.
    emit_session_state_changed(&app);
    Ok(())
}

/// What a headless launch returns — the synthetic pseudo-session id and the run id (the
/// provenance join key).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadlessLaunchResult {
    pub session_id: String,
    pub run_id: String,
    /// An agent launch's warnings for the prompt it was given. Never a refusal.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<crate::agents::launch_check::LaunchWarning>,
}

/// The PTY-less launch of `agent` (routine runs): the run's own provenance and timeout, and
/// the agent's args, MCP servers, account and generated entries on its runtime's built-in
/// descriptor (no bypass flag unless the agent's args carry one).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn launch_headless_session(
    app: tauri::AppHandle,
    launch: tauri::State<'_, DashboardLaunchState>,
    handle: String,
    story_id: String,
    cwd: String,
    prompt: String,
    story_path: String,
    launcher: Option<String>,
    launched_in: Option<LaunchedIn>,
    provenance: Option<RunProvenance>,
    run_timeout: Option<Duration>,
    mut agent: crate::agents::launch::AgentLaunch,
) -> Result<HeadlessLaunchResult, String> {
    agent.refuse_unusable_account()?;
    let (resolved, account_binding) =
        super::agent_accounts::resolve_for_launch(&app, agent.account_input()).await?;
    if !resolved.spawn_eligible {
        return Err(crate::agents::launch::account_refusal(resolved.reason));
    }
    let launch_args = agent.launch.args.clone();
    let profile_id = agent.launch.id.clone();
    let expected_provider = crate::agents::adapter::provider_of(agent.runtime);
    let account_binding = Some(account_binding);
    // With no prompt of its own, an agent starts with its default one.
    let prompt = match agent.default_prompt.clone() {
        Some(default) if prompt.trim().is_empty() => default,
        _ => prompt,
    };
    let descriptor = crate::agents::adapter::LaunchDescriptor::of(agent.runtime);

    // The agent binary is resolved at call time by its runtime's basename, through the one
    // launch-binary authority the managed spine shares.
    let resolved_binary =
        super::mcp_workspace::resolve_agent_for_launch(&app, &descriptor.binary_basename).await?;
    log::info!(
        target: "deps",
        "headless launch {handle}: {} (version {})",
        resolved_binary.path.display(),
        resolved_binary.version.as_deref().unwrap_or("unknown"),
    );
    agent.note_cli_version(resolved_binary.version.as_deref());
    let binary = resolved_binary.path;

    // SW-OR-11: bind the run's TASK by precedence (explicit prompt → story recipe → refuse),
    // UP FRONT — before reserving a slot or spawning — so a task-less run can never launch a
    // no-op `claude -p ""`. A headless run is one-shot with no TTY, so its task must be
    // delivered on stdin at launch (pairs with SW-OR-10's EOF discipline). See
    // [`resolve_headless_task`].
    let task = resolve_headless_task(&prompt, &story_path)?;

    // Account eligibility performs durable generation/home validation. Keep it
    // off the async reactor and complete it before reserving a launch slot or
    // minting any session identity.
    let account_home = {
        super::agent_accounts::resolved_launch_binding_home(
            account_binding.as_ref(),
            expected_provider,
        )?
    };

    // Reserve the supervisory slot BEFORE any spawn work (fail-fast `busy`). A handle
    // already launching returns `false` — a headless run has nothing to focus, so refuse
    // rather than fork a second run onto the same handle (we don't own that permit).
    if !launch.begin(&handle).map_err(|e| e.to_string())? {
        return Err(format!(
            "busy: a session is already active for handle '{handle}'"
        ));
    }

    let product = crate::agents::product_bootstrap::prepare(&app, &agent, Path::new(&cwd))
        .await
        .inspect_err(|_| {
            launch.abort(&handle);
        })?;
    let cwd = product
        .canonical_root()
        .map(|root| root.to_string_lossy().into_owned())
        .unwrap_or(cwd);
    product.validate().inspect_err(|_| {
        launch.abort(&handle);
    })?;

    let (generated_run_id, session_id) = mint_headless_ids();
    let run_id = provenance
        .as_ref()
        .map(|p| p.run_id.clone())
        .unwrap_or(generated_run_id);
    let cwd_path = PathBuf::from(&cwd);
    // The per-session config OFF the async worker (it touches the filesystem).
    // On any failure, free the reserved slot so a failed launch never leaks a permit.
    let scope_flags = {
        let cwd_path = cwd_path.clone();
        let session_id = session_id.clone();
        let agent = agent.clone();
        let scope_binary = binary.clone();
        let scope_launch_args = launch_args.clone();
        await_blocking("launch_headless_session.provision", move || {
            provision_headless_scope_for_cli(
                &cwd_path,
                &session_id,
                &agent,
                account_home.as_deref(),
                Some(
                    &super::mcp_scope::LaunchCli::new(&scope_binary, &scope_launch_args)
                        .with_version(agent.cli_version.as_deref()),
                ),
            )
        })
        .await
    };
    let (scope, applied) = match scope_flags {
        Ok(outcome) => outcome,
        Err(e) => {
            launch.abort(&handle);
            super::mcp_scope::remove_scope_config_async(&session_id).await;
            return Err(e);
        }
    };

    // Consume only after all provisioning/projection succeeds. This is the
    // final account-policy check before the capture request can be spawned.
    let consumed_account = match await_blocking("launch_headless_session.account", move || {
        super::agent_accounts::consume_resolved_launch_binding(account_binding)
    })
    .await {
        Ok(account) => account,
        Err(error) => {
            launch.abort(&handle);
            super::mcp_scope::remove_scope_config_async(&session_id).await;
            return Err(error);
        }
    };

    // Register the pseudo-session: bind the synthetic id, stamp provenance (SW-AU-53's
    // `headless` trigger cause, derived from the enum so a rename can't desync), and flip
    // the entry to the headless kind — stashing the child's `pid` slot so Stop / app-exit
    // can `killpg` it — so it renders `Running` by registry presence (AC-2).
    let account_provenance = consumed_account.provenance.clone().map(Into::into);
    if let Err(e) = launch.bind_with_account(
        &handle,
        &session_id,
        launcher.as_deref(),
        account_provenance,
    ) {
        launch.abort(&handle);
        super::mcp_scope::remove_scope_config_async(&session_id).await;
        return Err(e);
    }
    let provenance = provenance.unwrap_or_else(|| RunProvenance {
        run_id: run_id.clone(),
        story_id,
        trigger_source: Some(crate::commands::trigger_source::TRIGGER_SOURCE_HEADLESS.to_string()),
        ..Default::default()
    });
    let mut launch_args = launch_args;
    applied.drop_agent_settings(&mut launch_args);
    launch_args.extend(applied.argv(str::to_string));
    if let Some(settings) = applied.settings {
        launch.hold_launch_settings(&handle, settings);
    }
    launch.set_agent_revision_if_absent(&handle, agent.revision_label());
    let provenance = agent.provenance(Some(provenance), &handle);
    launch.stamp_provenance(&handle, provenance);
    // Give the run its agent session id up front, as interactive launches do. Its usage is
    // attributed to who ran it, including personal launches without an org.
    let agent_session_id = match descriptor.session_id_argv.as_deref() {
        Some(template) => {
            let id = uuid::Uuid::new_v4().to_string();
            let argv = match crate::agents::adapter::expand_session_id_argv(template, &id) {
                Ok(argv) => argv,
                Err(error) => {
                    launch.abort(&handle);
                    super::mcp_scope::remove_scope_config_async(&session_id).await;
                    return Err(error);
                }
            };
            launch_args.extend(argv);
            Some(id)
        }
        None => None,
    };
    launch.set_profile_id_if_absent(&handle, profile_id.clone());
    if let Some(LaunchedIn { org, team }) = &launched_in {
        if let Some(team) = team {
            launch.set_launch_team_if_absent(
                &handle,
                LaunchTeam {
                    org: org.clone(),
                    team: team.clone(),
                },
            );
        }
        stamp_launch_owner(&app, &launch, &handle, org.clone(), team.clone()).await;
    }
    let api_port = app
        .try_state::<crate::http::HttpApiPort>()
        .map(|port| port.0);
    let mut scope = scope;
    let mut account_env = consumed_account.environment;
    let recorded_scope_env = scope.extra_env.clone();
    account_env.append(&mut scope.extra_env);
    headless_usage_owner(launched_in.as_ref(), &profile_id, &handle, &cwd)
        .prepare_export(
            crate::session_usage::store_path(),
            &session_id,
            descriptor.telemetry.as_ref(),
            api_port,
            &mut account_env,
            &mut launch_args,
        )
        .await;
    if let Some(id) = agent_session_id {
        crate::session_usage::attribute_launch(
            id,
            launched_in
                .as_ref()
                .map(|owner| owner.org.clone())
                .unwrap_or_default(),
            launched_in.as_ref().and_then(|owner| owner.team.clone()),
            profile_id.clone(),
            handle.clone(),
            Some(cwd.clone()),
        )
        .await;
    }
    let pid = Arc::new(AtomicU32::new(0));
    // SW-OR-13: the cooperative-cancel flag (stashed in the entry so `release_for_session`
    // can trip it, closing the pre-spawn Stop race) and the live output channel (drained
    // by `run_headless_to_completion`'s emitter → the "View output" panel's live tail).
    let cancel = Arc::new(AtomicBool::new(false));
    // SW-OR-19: the last-output heartbeat anchor — `spawn_capture` stamps it per drained
    // chunk, `list_sessions_inner` reads it into the row, the emitter re-freshens it live.
    let last_drain_ms = Arc::new(AtomicU64::new(0));
    launch.mark_headless(&handle, pid.clone(), cancel.clone(), last_drain_ms.clone());
    emit_session_state_changed(&app);
    launch.set_queue_role(&handle, agent.queue_role.clone());
    // The run is one turn, begun now and ended when its capture completes.
    crate::session_usage::timing::note_headless_start(&session_id, agent.runtime);
    write_launch_record_for_app(
        &app,
        &launch,
        &session_id,
        &handle,
        crate::session_usage::record::LaunchFacts::new(
            agent.runtime,
            agent.cli_version.as_deref(),
            &launch_args,
            agent.launch.mcp_allow.clone(),
            agent.launch.effective_bridge_tools.clone(),
            agent.revision_label(),
            agent.manifest_hash.clone(),
            &cwd,
        )
        .with_launch_env(agent.runtime, &agent.launch.env, &recorded_scope_env)
        .with_provided_skills(agent.provided_skills())
        .with_task(crate::session_usage::task_capture::TaskInputs::from_launch(
            &agent,
            Some(&prompt),
        )),
    );
    let facts = super::role_startup::register(&app, &handle, &session_id).await;
    let task = match facts {
        Some(facts) => format!("{task}\n\n{facts}"),
        None => task,
    };
    let mut role_env = launch
        .launch_team(&handle)
        .map(|team| super::role_startup::environment(&team, agent.queue_role.as_deref()))
        .unwrap_or_default();

    // Build the PTY-less run request and hand it to the blocking pool, then return NOW —
    // the run completes detached (AC-7). `process_group: true` + the `pid` sink make the
    // child (and its whole tree) reachable by a `killpg` from Stop / app-exit. SW-OR-11: the
    // run's resolved TASK (explicit prompt → story recipe) is fed on stdin + EOF by
    // `spawn_capture` (AC-3); the adapter's headless entry is ensured in `headless_argv`.
    // SW-OR-16: capture the declared headless structured-transcript format id BEFORE `descriptor` is
    // moved into the request builder — `None` keeps the run on OR-12's plain text floor. The
    // `request_argv` that switches the agent's stdout to that format is injected backend-side inside
    // `headless_capture_request` (never threaded through the FE), keeping agent-format knowledge in
    // the adapter.
    let structured_format = descriptor.structured_headless.as_ref().map(|h| h.format);
    if let Err(error) = product.validate() {
        launch.abort(&handle);
        super::mcp_scope::remove_scope_config_async(&session_id).await;
        return Err(error);
    }
    let mut request = headless_capture_request(
        binary,
        descriptor,
        launch_args,
        scope,
        task,
        cwd_path,
        pid.clone(),
        &session_id,
        api_port,
        account_env,
        consumed_account.env_remove,
        agent.launch.env.clone(),
        run_timeout,
    );
    request.env.append(&mut role_env);
    let helper = activity_helper(&app);
    let runtime = agent.runtime;
    let folded = await_blocking("launch_headless_session.fold", move || {
        fold_headless_launch(&mut request, runtime, helper.as_deref()).map(|()| request)
    })
    .await;
    let mut request = match folded {
        Ok(request) => request,
        Err(error) => {
            launch.abort(&handle);
            super::mcp_scope::remove_scope_config_async(&session_id).await;
            return Err(error);
        }
    };
    // Its own place on this Mac's in-process host, so its inbox hooks look for the mail
    // pushed there (added after the strip of any inherited host's bindings).
    let activity =
        HeadlessActivity::prepare(&app, &session_id, agent.runtime).map(|(activity, env)| {
            request.env.extend(env);
            activity
        });
    // SW-OR-13: wire the live output channel + the cancel flag. The sink streams each
    // drained chunk to the emitter (below); the flag is the same Arc the registry entry
    // holds, so a Stop trips both this run's pre-spawn check and its wait loop.
    let (chunk_tx, chunk_rx) = std::sync::mpsc::channel::<CaptureChunk>();
    request.output_sink = Some(chunk_tx);
    request.cancel = Some(cancel);
    // SW-OR-19: the SAME anchor stashed on the registry entry — so the row snapshot and the
    // live heartbeat event both read the one last-output truth (can't contradict).
    request.last_drain_ms = Some(last_drain_ms);
    // SW-AU-52: resolve the run identity NOW, while provenance is guaranteed present (just stamped
    // above) — the completion task owns it, so a concurrent Stop that releases the slot early (and
    // `killpg`s the child) can't erase the durable outcome. The write no longer re-reads the registry.
    let outcome_ctx = launch
        .run_context_for_session(&session_id)
        .map(|ctx| ctx.with_headless_format(structured_format));
    let scope_lease = match super::mcp_scope::retain_detached_scope(&session_id) {
        Ok(lease) => lease,
        Err(error) => {
            launch.abort(&handle);
            super::mcp_scope::remove_scope_config_async(&session_id).await;
            return Err(error);
        }
    };
    if scope_lease.is_some() {
        super::mcp_scope::bind_relay_launch(&app, &session_id);
    }
    let completion_session = session_id.clone();
    tauri::async_runtime::spawn(async move {
        let _scope_lease = scope_lease;
        if let Some(activity) = &activity {
            // The mail already waiting is pushed to its host before it starts, for its
            // boot look to find; its PID is committed once it runs.
            super::host_inbox::offer_headless(&app, &completion_session).await;
            activity.commit_when_spawned(pid);
        }
        let _activity = activity;
        run_headless_to_completion(
            app,
            completion_session,
            outcome_ctx,
            request,
            structured_format,
            chunk_rx,
            agent.runtime,
        )
        .await;
    });

    Ok(HeadlessLaunchResult {
        session_id,
        run_id,
        warnings: agent.warnings,
    })
}

fn headless_usage_owner(
    launched_in: Option<&LaunchedIn>,
    agent: &str,
    handle: &str,
    cwd: &str,
) -> crate::session_usage::LaunchOwner {
    crate::session_usage::LaunchOwner::new(
        launched_in.map(|owner| owner.org.clone()),
        launched_in.and_then(|owner| owner.team.clone()),
        agent.to_string(),
        handle.to_string(),
        Some(cwd.to_string()),
    )
}

/// The blocking provisioning half of a headless launch in `cwd`: the agent's per-session MCP
/// scope and generated entries (returned to be held for the run), none of it written into
/// `cwd`. Headless joins argv directly into `spawn_capture` (no shell), so paths are RAW.
#[cfg(test)]
fn provision_headless_scope(
    cwd: &Path,
    session_id: &str,
    agent: &crate::agents::launch::AgentLaunch,
    account_home: Option<&Path>,
) -> Result<
    (
        crate::commands::mcp_scope::ScopeOutcome,
        crate::agents::launch::AppliedLaunch,
    ),
    String,
> {
    let fixture = super::mcp_scope::inventory::Fixture::new();
    let cli = fixture.launch(&[]);
    provision_headless_scope_for_cli(cwd, session_id, agent, account_home, Some(&cli))
}

/// [`provision_headless_scope`] for the CLI the launch runs.
fn provision_headless_scope_for_cli(
    cwd: &Path,
    session_id: &str,
    agent: &crate::agents::launch::AgentLaunch,
    account_home: Option<&Path>,
    cli: Option<&super::mcp_scope::LaunchCli<'_>>,
) -> Result<
    (
        crate::commands::mcp_scope::ScopeOutcome,
        crate::agents::launch::AppliedLaunch,
    ),
    String,
> {
    crate::agents::launch::project_and_apply_for_cli(
        agent,
        session_id,
        cwd,
        account_home,
        cli,
        |p| p.to_string(),
    )
    .map(|(scope, _, applied)| (scope, applied))
}

/// SW-OR-11: the canonical one-shot task a headless story run derives when the launch
/// gesture supplies no explicit prompt. `{storyPath}` is substituted with the agent-visible
/// story path (see [`resolve_headless_task`]). This binds *which* story; the *how* (TDD,
/// tests, conventions) rides on the team's `.claude` that SW-OR-10 provisions into the
/// worktree before spawn — the same context an interactive open gives its agent. Written as
/// a template (not a `format!`) so a future per-profile `default_task` slots in unchanged.
const HEADLESS_STORY_TASK_TEMPLATE: &str =
    "Read the story at {storyPath} and complete it, satisfying every acceptance criterion.";

/// SW-OR-11: resolve a headless run's TASK by precedence — the required-task guard.
///
/// 1. an explicit launch prompt (whitespace ≡ absent) is used verbatim; else
/// 2. the story recipe: render [`HEADLESS_STORY_TASK_TEMPLATE`] against the story path via
///    the SHARED `substitute_template_vars` engine (the one automation dispatch uses — one
///    substitution semantics, keyed by `canonical_field_key` so `{storyPath}` resolves); else
/// 3. `Err` — codesense never spawns a bare `claude -p` on empty stdin.
///
/// Pure (no I/O): the story path is resolved trusted-side by the caller and threaded in, so
/// this stays unit-testable and off any filesystem.
fn resolve_headless_task(prompt: &str, story_path: &str) -> Result<String, String> {
    if !prompt.trim().is_empty() {
        // AC-3: an explicit prompt is the caller's verbatim task — NOT template-expanded,
        // matching the interactive story-open seed (which also delivers `initialPrompt`
        // as-is; only the separate `runPrompt` drive primitive expands `{storyPath}`).
        return Ok(prompt.to_string());
    }
    // Story recipe: a whitespace-only path is simply not bound, so the template's
    // `{storyPath}` stays unresolved and the single guard below refuses — one source of
    // truth for "is the task complete", reachable and tested for both outcomes.
    let mut values = std::collections::BTreeMap::new();
    if !story_path.trim().is_empty() {
        values.insert(
            crate::commands::pipeline_format::canonical_field_key("storyPath"),
            story_path.trim().to_string(),
        );
    }
    let (task, unresolved) = crate::commands::field_schema::substitute_template_vars(
        HEADLESS_STORY_TASK_TEMPLATE,
        &values,
    );
    if unresolved.is_empty() {
        return Ok(task);
    }
    Err(
        "unavailable: a headless launch requires a task (no prompt and no resolvable story)"
            .to_string(),
    )
}

/// SW-OR-10 → SW-AN-45: compose the headless argv — the adapter's `headless_entry` tokens
/// (Claude `-p`, the stdin-print flag; Codex `exec`, its non-interactive subcommand) ADDED
/// where the profile omitted them and never stripped, then the profile's argv, then the MCP
/// scope flags. Agent-neutral: the mode entry is adapter data, not a hardcoded flag in core.
fn headless_argv(
    entry: &[String],
    launch_args: Vec<String>,
    scope_flags: Vec<String>,
) -> Vec<String> {
    let mut args = Vec::with_capacity(entry.len() + launch_args.len() + scope_flags.len());
    // SW-AN-45: prepend the adapter's headless-mode entry tokens the profile didn't already
    // supply (Claude `-p`, Codex `exec`) — agent-neutral, the mode entry is adapter data.
    for tok in entry {
        if !launch_args.iter().any(|a| a == tok) {
            args.push(tok.clone());
        }
    }
    args.extend(launch_args);
    super::mcp_scope::compose_args(args, scope_flags)
}

/// Assemble the PTY-less headless run request from the agent runtime's
/// `descriptor` — `binary` (already resolved from its basename) → `program`, `headless_entry`
/// → the argv lead, `env_suppress` → `env_remove` — plus the projected `scope` (its `extra_args`
/// appended to argv, `extra_env` as real child env) and the agent's own `agent_env`, which
/// every variable CodeSense sets or strips wins over, as in a terminal spawn. Split out of
/// `launch_headless_session` so this descriptor→request wiring is unit-testable without a live
/// app (AC-2: a Codex descriptor yields `program` basename `codex` + the `exec` entry +
/// `OPENAI_API_KEY` strip, never `claude`/`-p`/`ANTHROPIC_API_KEY`). `account_env_remove` is a
/// managed account's credential scrub (empty on a system-default launch): it joins the strip, so
/// it also drops the agent's own copy of those variables.
#[allow(clippy::too_many_arguments)]
pub(crate) fn headless_capture_request(
    binary: PathBuf,
    descriptor: crate::agents::adapter::LaunchDescriptor,
    launch_args: Vec<String>,
    scope: crate::commands::mcp_scope::ScopeOutcome,
    prompt: String,
    cwd_path: PathBuf,
    pid: Arc<AtomicU32>,
    session_id: &str,
    api_port: Option<u16>,
    account_env: Vec<(String, String)>,
    account_env_remove: Vec<String>,
    agent_env: BTreeMap<String, String>,
    timeout: Option<Duration>,
) -> CaptureRequest {
    // SW-AN-45: an `env_dir` adapter (e.g. Codex → `CODEX_HOME`) contributes real child env
    // vars; `config_flag` / unscoped leaves this empty.
    // Account authority is the source used to build the scoped home; the
    // per-session scoped home must be the final child value so its MCP config
    // and journaled credential copy travel together.
    let mut env = account_env;
    env.extend(scope.extra_env);
    // Usage registration and export are prepared together before the request is assembled.
    // SW-OR-14 (AC-2): the launch IDENTITY — parity with the interactive PTY spawn
    // (`pty.rs`), which the PTY-less path bypasses. Pushed AFTER the adapter env so the
    // identity wins on the (unlikely) colliding key, and it survives `env_remove`
    // (applied first in `spawn_capture`). Lets a headless agent echo its
    // `$CODESENSE_TAB_SESSION_ID` into any session-keyed MCP verb and reach the local
    // API on `$CODESENSE_API_PORT`.
    env.push((
        "CODESENSE_TAB_SESSION_ID".to_string(),
        session_id.to_string(),
    ));
    if let Some(port) = api_port {
        env.push(("CODESENSE_API_PORT".to_string(), port.to_string()));
    }
    // SW-OR-16: when the adapter declares a headless structured-transcript mechanism, append its
    // `request_argv` (e.g. `--output-format stream-json --verbose`) so the agent emits the structured
    // format on the SAME stdout pipe — near-zero capture cost, parsed on demand at completion. Pack
    // data, appended backend-side after the neutral argv; a non-declaring adapter adds nothing.
    let structured_argv: Vec<String> = descriptor
        .structured_headless
        .as_ref()
        .map(|h| h.request_argv.clone())
        .unwrap_or_default();
    // A CLI whose structured mode reads its query from an option takes the task as one literal
    // joined token (`--query=<task>`), never on stdin; every other CLI keeps stdin.
    let argv_prompt = descriptor
        .structured_headless
        .as_ref()
        .and_then(|h| h.argv_prompt_flag.as_deref())
        .map(|flag| format!("{flag}{prompt}"));
    let mut args = headless_argv(&descriptor.headless_entry, launch_args, scope.extra_args);
    args.extend(structured_argv);
    args.extend(argv_prompt.clone());
    let env_remove: Vec<String> = descriptor
        .env_suppress
        .into_iter()
        .chain(account_env_remove)
        .chain(HEADLESS_HOSTLESS_ENV.iter().map(|name| name.to_string()))
        .collect();
    // The agent's own variables go first, so CodeSense's above win on a clash. `spawn_capture`
    // strips before it sets, so a variable the launch strips is left out here, as are the ones
    // that are always CodeSense's and `PATH`, which a terminal spawn sets itself.
    let mut env: Vec<(String, String)> = agent_env
        .into_iter()
        .filter(|(name, _)| {
            codesense_pty_core::spawn::agent_may_set(name)
                && name != "PATH"
                && !env_remove.contains(name)
        })
        .chain(env)
        .collect();
    crate::session_usage::telemetry::configure_export(
        descriptor.telemetry.as_ref(),
        api_port,
        session_id,
        &mut env,
        &mut args,
    );
    CaptureRequest {
        program: binary,
        // Prepend the runtime's headless-mode entry tokens (Codex `exec`, not always
        // Claude `-p`), then the profile argv, then the MCP scope flags, then (SW-OR-16) the
        // structured-output request tokens.
        args,
        cwd: Some(cwd_path),
        env,
        // Strip the runtime's guarded env (Codex → `OPENAI_API_KEY`) from the
        // child — the PTY-less parity of the interactive spine's spawn `env_remove`. Any
        // terminal host the app inherited is not the run's: the launch adds its own
        // ([`HeadlessActivity`]), or none.
        env_remove,
        stdin: argv_prompt.is_none().then(|| prompt.into_bytes()),
        // A caller with its own run bound (a routine's `max_runtime`) replaces the
        // generic ceiling, longer or shorter.
        timeout: timeout.unwrap_or(HEADLESS_RUN_TIMEOUT),
        process_group: true,
        pid_sink: Some(pid),
        // SW-OR-13/OR-19: the caller sets these on the returned request — the live output
        // channel, the cooperative-cancel flag, and the heartbeat anchor stashed in the
        // registry entry.
        output_sink: None,
        last_drain_ms: None,
        max_output_bytes: None,
        cancel: None,
    }
}

/// Fold the CLI's final launch settings (managed hooks, observers, refusal of an option that
/// bypasses them) into the home `request` will run in, before it spawns: the one writer an
/// interactive launch also reaches, so a headless run never starts on an unfolded home.
pub(crate) fn fold_headless_launch(
    request: &mut CaptureRequest,
    cli: codesense_pty_core::agent_cli::AgentCli,
    helper: Option<&str>,
) -> Result<(), String> {
    request.args = codesense_pty_core::activity::fold_launch_settings(
        cli,
        &request.program,
        &request.args,
        &request.env,
        helper,
    )?;
    Ok(())
}

/// The packaged activity helper of this app's in-process host, once it is installed.
fn activity_helper(app: &tauri::AppHandle) -> Option<String> {
    let service = app.try_state::<codesense_pty_core::activity::ActivityService>()?;
    let (_, root) = service.shell_env()?;
    let helper = format!("{root}/{}", codesense_pty_core::activity::HELPER_NAME);
    std::path::Path::new(&helper).is_file().then_some(helper)
}

/// Resolve the same task store for a session's signal and process exit. Tests inject their
/// own store on the registry, so the asynchronous writer never reaches the user's home.
fn task_store_path(_state: Option<&DashboardLaunchState>) -> Result<PathBuf, String> {
    #[cfg(test)]
    {
        _state.and_then(|state| state.task_store_path.lock().unwrap().clone())
            .ok_or_else(|| "no task usage store injected for this test".to_string())
    }
    #[cfg(not(test))]
    {
        crate::session_usage::store_path()
    }
}

/// An ending can arrive before the detached launch capture finishes. Order persistence only,
/// then use the one close funnel; callers do not block launching or the async runtime.
async fn close_task_after_capture(
    state: Option<&DashboardLaunchState>,
    session_id: &str,
    status: &str,
) {
    let task_id = state.and_then(|state| state.task_captures.lock().unwrap()
        .get(session_id).map(|(_, task_id)| task_id.clone()));
    if let Some(state) = state {
        state.wait_for_task_capture(session_id).await;
    }
    crate::session_usage::task_outcome::close_for_session_async(
        task_store_path(state), session_id, status, task_id,
    ).await;
}

/// The headless completion decision: an attested signal owns the outcome; otherwise close
/// the task through the same asynchronous funnel and allow the exit's write and event.
async fn close_headless_task_on_exit(
    launch: &DashboardLaunchState,
    session_id: &str,
    status: &str,
) -> bool {
    let task_id = launch.task_captures.lock().unwrap()
        .get(session_id).map(|(_, task_id)| task_id.clone());
    launch.wait_for_task_capture(session_id).await;
    if launch.take_signal_attested(session_id) {
        return false;
    }
    crate::session_usage::task_outcome::close_for_session_async(
        task_store_path(Some(launch)), session_id, status, task_id,
    ).await;
    true
}

/// A headless run ended: close its timing, and store its conversation now, before the run's
/// scoped home (a Codex rollout lives in it) is removed.
async fn finish_headless_session(
    session_id: &str,
    cli: codesense_pty_core::agent_cli::AgentCli,
    status: &str,
) {
    crate::session_usage::timing::note_headless_end(session_id, cli, status);
    crate::session_conversation::capture_now(session_id).await;
}

/// SW-OR-10 (AC-5/AC-7): run the headless agent to completion on the blocking pool via
/// `spawn_capture` (the PTY-less primitive `run_agent` itself wraps — used directly so the
/// child is killable and exit-0 counts as success regardless of stdout), then surface the
/// outcome. Owns the SINGLE teardown: it releases the registry slot (drops the `Running`
/// row + frees the SpawnCap), reaps the ephemeral scope config, and — only if it actually
/// performed the release (the row was still registered, i.e. not already torn down by an
/// app-exit sweep) — emits `mcp:agent-done` (→ the chip renders Done / Errored) + pings the
/// roster. Entirely off the command's return path. Note a `killpg` from Stop is observed
/// here as a non-zero/​signal exit → `failed`, so a user-stopped run resolves cleanly.
async fn run_headless_to_completion(
    app: tauri::AppHandle,
    session_id: String,
    outcome_ctx: Option<RunOutcomeContext>,
    request: CaptureRequest,
    structured_format: Option<TranscriptFormat>,
    chunk_rx: std::sync::mpsc::Receiver<CaptureChunk>,
    cli: codesense_pty_core::agent_cli::AgentCli,
) {
    // SW-OR-13 (AC-1): stream drained stdout/stderr chunks to the run-scoped event the
    // "View output" panel subscribes to (`mcp:agent-output-{run_id}`), for the live tail.
    // Off the async runtime on its own thread; the loop ends when every `spawn_capture`
    // sender drops (both drains hit EOF + the request drops), so it never outlives the run.
    // A dropped receiver (no run_id, or panel never opened) is fine — the drains discard
    // the sends and the final buffer stays intact for the persisted artifact.
    // SW-OR-19: hand the emitter a clone of the heartbeat anchor BEFORE `request` moves into
    // the blocking spawn, so it can read the one last-output truth for the throttled event.
    let heartbeat_anchor = request.last_drain_ms.clone();
    spawn_output_emitter(
        &app,
        outcome_ctx.as_ref().map(|c| c.run_id.clone()),
        heartbeat_anchor,
        chunk_rx,
    );

    let result = await_blocking("launch_headless_session.run", move || {
        spawn_capture(request).map_err(|e| e.to_string())
    })
    .await;

    // The child is no longer watchdog-live once capture returns. Preserve the registry entry until
    // normal teardown because an in-flight `signal_done` may still need its provenance.
    {
        use tauri::Manager;
        app.state::<DashboardLaunchState>()
            .mark_headless_exited(&session_id);
    }

    let (status, summary) = headless_run_outcome(structured_format, &result);
    finish_headless_session(&session_id, cli, status).await;
    let exit_code = result.as_ref().ok().and_then(|out| out.exit_code());

    // SW-OR-12 (AC-1/AC-2): persist the captured transcript from the spawn-resolved `run_id` (never
    // a re-call of private registry context), so a fire-and-exit run leaves a readable log.
    // Best-effort and off the async runtime; a failed/empty write yields `None`. The
    // ref is threaded onto the durable ProcessExit outcome below; a self-attested run's `Signal`
    // outcome keeps `None` (the file is still discoverable by `run_id` — ref-on-signal is SW-OR-15).
    let artifact_ref = match (outcome_ctx.as_ref(), result.as_ref().ok()) {
        (Some(ctx), Some(out)) => match structured_format {
            // SW-OR-16 (AC-4/AC-2): a declaring run scrubs the raw stdout (unredacted `stream-json`
            // tool I/O) BEFORE persist, writes the redacted text floor to `{run_id}.log`, and parses
            // the redacted stream into `{run_id}.turns.json` for the rich panel mode — all off-thread.
            Some(format) => {
                crate::commands::sprint_run::run_transcript::persist_structured_headless_run(
                    &ctx.run_id,
                    format,
                    &out.stdout,
                    &out.stderr,
                )
                .await
            }
            // Non-declaring: OR-12's plain text-floor write, byte-identical.
            None => {
                crate::commands::sprint_run::run_artifact::write_run_artifact(&ctx.run_id, out)
                    .await
            }
        },
        _ => None,
    };

    use tauri::Manager;
    let launch = app.state::<DashboardLaunchState>();
    // Idempotent: Stop/drain may already have released the row.
    launch.release_for_session(&session_id);
    crate::commands::mcp_scope::remove_scope_config_async(&session_id).await;
    // SW-OR-14 (AC-4): sweep any outstanding `ask_user` oneshot for this session so a
    // headless agent (or its caller) resolves to a typed `cancel` instead of wedging for
    // the full 600s. The interactive path does this from the PTY-exit batcher; a PTY-less
    // headless run never hits that code, so it funnels the same primitive through HERE —
    // the one headless teardown that holds an `AppHandle` and runs on natural completion
    // AND post-kill (a `killpg`ed child returns → this task resumes). A no-op when there
    // are no pending asks.
    if let Some(asks) = app.try_state::<crate::mcp::state::AskUserRegistry>() {
        asks.cancel_by_session(&session_id);
    }
    // SW-OR-14 (AC-3) — Signal > ProcessExit across BOTH surfaces (chip + durable store). Drain
    // the self-attest record (which OUTLIVES entry removal, so this holds regardless of which
    // teardown path reaped the entry — natural, Stop, cap-reclaim). If the agent self-attested,
    // `signal_done` already emitted its verdict AND wrote the authoritative `Signal` outcome
    // (SW-AU-52) → skip BOTH the exit emit and the durable `ProcessExit` write, keeping AU-52's
    // fold a plain latest-wins. Otherwise (exited or was killed WITHOUT signaling) → write + emit
    // the exit classification so both surfaces still flip terminal (a `killpg`ed child = `failed`).
    if close_headless_task_on_exit(&launch, &session_id, status).await {
        // SW-AU-52 (AC-1): persist the terminal outcome from the identity captured at spawn.
        // `status` always parses (it comes from `classify_headless_outcome`); `outcome_ctx` is
        // `Some` for every real headless launch.
        if let (Some(ctx), Some(session_status)) = (
            outcome_ctx,
            crate::dashboard_stream::SessionStatus::from_wire(status),
        ) {
            append_run_outcome(
                ctx,
                session_status,
                Some(summary.clone()),
                exit_code,
                crate::commands::sprint_run::run_store::OutcomeSource::ProcessExit,
                "launch_headless_session.outcome",
                artifact_ref,
            )
            .await;
        }
        emit_agent_done(&app, &session_id, status, &summary);
    }
    emit_session_state_changed(&app);
}

/// SW-OR-10 (AC-5): emit `mcp:agent-done` byte-identically to the `signal_done` MCP verb
/// (same event name + `{ sessionId, status, summary }` shape), so the existing FE
/// managed-session bridge flips the chip terminal with NO new FE plumbing.
fn emit_agent_done(app: &tauri::AppHandle, session_id: &str, status: &str, summary: &str) {
    use tauri::Emitter;
    let _ = app.emit(
        "mcp:agent-done",
        serde_json::json!({
            "sessionId": session_id,
            "status": status,
            "summary": summary,
        }),
    );
}

/// SW-OR-13: the length of the longest prefix of `bytes` that ends on a complete UTF-8 boundary —
/// all of `bytes` except a trailing INCOMPLETE multi-byte sequence. A genuinely-invalid byte is NOT
/// held back (it can never complete), so it stays in the prefix for `from_utf8_lossy` to replace.
/// Lets the streamed emitter carry a codepoint split across a read boundary to the next chunk
/// instead of rendering U+FFFD at the seam.
fn complete_utf8_len(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(e) if e.error_len().is_none() => e.valid_up_to(),
        Err(_) => bytes.len(),
    }
}

/// SW-OR-19: the global roster event carrying a single run's fresh last-output timestamp
/// `{ runId, lastOutputMs }`. The one app-root roster listener patches JUST that row's anchor
/// (no `list_sessions` reload), keeping the fleet-row heartbeat fresh at scale.
const AGENT_HEARTBEAT_EVENT: &str = "mcp:agent-heartbeat";

/// SW-OR-19: the coarse floor between heartbeat nudges for a single run — one active run emits
/// at most one roster patch per this window regardless of output volume, so a 50-agent fleet
/// stays cheap. First output emits immediately (the anchor was `0`), then this throttles.
const HEARTBEAT_THROTTLE: Duration = Duration::from_secs(3);

/// SW-OR-13: drain a run's live output channel on a dedicated thread, emitting each chunk
/// as `mcp:agent-output-{run_id}` (`{ stream, chunk }`) — the event the "View output" panel's
/// live tail subscribes to, scoped per run so no other panel receives it. `run_id: None`
/// (no durable identity) drops the receiver, so the drains discard their sends and the run
/// is byte-identical to a non-streamed one. The thread exits when the channel closes (all
/// `spawn_capture` senders dropped at drain EOF), so it never outlives the run.
///
/// A multi-byte UTF-8 char can be split across a 64 KiB read boundary, so per stream we hold back
/// only a trailing INCOMPLETE sequence and prepend it to the next chunk of that stream — no mojibake
/// at chunk seams. (The persisted artifact decodes the whole buffer once, so this only affects the
/// live view.) Any leftover partial bytes are flushed lossily when the channel closes.
fn spawn_output_emitter(
    app: &tauri::AppHandle,
    run_id: Option<String>,
    last_drain_ms: Option<Arc<AtomicU64>>,
    chunk_rx: std::sync::mpsc::Receiver<CaptureChunk>,
) {
    let Some(run_id) = run_id else { return };
    let app = app.clone();
    std::thread::spawn(move || {
        use tauri::Emitter;
        let event = format!("mcp:agent-output-{run_id}");
        let emit = |stream: &str, bytes: &[u8]| {
            if bytes.is_empty() {
                return;
            }
            let text = String::from_utf8_lossy(bytes);
            let _ = app.emit(
                &event,
                serde_json::json!({ "stream": stream, "chunk": text.as_ref() }),
            );
        };
        let mut pending_stdout: Vec<u8> = Vec::new();
        let mut pending_stderr: Vec<u8> = Vec::new();
        // SW-OR-19: the fleet-row heartbeat. Each drained chunk means this run is alive NOW;
        // at a coarse throttle we nudge the roster with the run's fresh last-output epoch-ms so
        // the Agents-panel row's "last output Ns ago" resets — a single-row patch (payload
        // carries `runId`), NOT a full `list_sessions` reload, so it stays scale-safe at fleet
        // size. First chunk emits immediately so the cue appears promptly.
        let mut last_heartbeat: Option<Instant> = None;
        for chunk in chunk_rx {
            let (label, pending) = match chunk.stream {
                CaptureStream::Stdout => ("stdout", &mut pending_stdout),
                CaptureStream::Stderr => ("stderr", &mut pending_stderr),
            };
            pending.extend_from_slice(&chunk.bytes);
            let split = complete_utf8_len(pending);
            emit(label, &pending[..split]);
            pending.drain(..split);
            if let Some(anchor) = &last_drain_ms {
                let now = Instant::now();
                let due =
                    last_heartbeat.is_none_or(|t| now.duration_since(t) >= HEARTBEAT_THROTTLE);
                if due {
                    last_heartbeat = Some(now);
                    let _ = app.emit(
                        AGENT_HEARTBEAT_EVENT,
                        serde_json::json!({
                            "runId": run_id,
                            "lastOutputMs": anchor.load(Ordering::Relaxed),
                        }),
                    );
                }
            }
        }
        // Channel closed (run over): flush any trailing partial bytes lossily — nothing is lost.
        emit("stdout", &pending_stdout);
        emit("stderr", &pending_stderr);
    });
}

/// SW-OR-10: SIGKILL a process group — the headless child plus its whole subprocess tree
/// (tool subprocesses, stdio MCP servers). The pgid is the published child pid (the run
/// spawned with its own group). A gone group is a harmless ESRCH. Unix-only (macOS/Linux
/// are the targets); a no-op elsewhere.
pub(crate) fn kill_process_group(pgid: u32) {
    #[cfg(unix)]
    // Safety: `killpg` only sends a signal; an empty/exited group is a harmless ESRCH.
    unsafe {
        libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pgid;
}

/// AC-10/AC-12: resolve the authoritative fire target for a handle. Rejects a
/// handle with no live binding instead of trusting a caller-supplied id. ONE
/// lookup for every handle kind (story / epic / context / branch), §2.
#[tauri::command]
pub fn dashboard_resolve_session(
    state: tauri::State<'_, DashboardLaunchState>,
    handle: String,
) -> Result<String, String> {
    state
        .resolve(&handle)
        .ok_or_else(|| format!("unavailable: no live session for target '{handle}'"))
}

/// Whether `session_id` is a live CodeSense session: a live headless run (its
/// liveness is registry presence), or a session any terminal host tracks. The
/// terminal hosts are asked off the async workers — a sidecar answers over its
/// socket — and a host that cannot be asked leaves it `Unknown`.
pub async fn session_liveness(
    app: &tauri::AppHandle,
    session_id: &str,
) -> crate::commands::pty_backend::Liveness<bool> {
    let (app, session) = (app.clone(), session_id.to_string());
    await_blocking("session_liveness", move || {
        Ok(session_liveness_now(&app, &session))
    })
    .await
    .unwrap_or(crate::commands::pty_backend::Liveness::Unknown)
}

/// [`session_liveness`] for a caller already off the async runtime.
pub fn session_liveness_now(
    app: &tauri::AppHandle,
    session_id: &str,
) -> crate::commands::pty_backend::Liveness<bool> {
    use tauri::Manager;
    if app
        .try_state::<DashboardLaunchState>()
        .is_some_and(|state| state.has_live_headless_session(session_id))
    {
        return crate::commands::pty_backend::Liveness::Known(true);
    }
    crate::commands::pty_backend::has_session(app, session_id)
}

/// SW-DX-17a (AC-1/AC-3) + SW-AV-1 (INV-3): enumerate EVERY live managed session as
/// the UNION of the launch registry and the live-PTY set, deduped by `session_id`,
/// for the "Running agents" roster.
///
/// - The **registry** arm stays authoritative for dashboard-launched / cap-free
///   `register`ed sessions AND the headless pseudo-sessions (which have no PTY child
///   to probe — their liveness is registry presence, via [`is_live_headless`]).
/// - The **live-PTY** arm ([`crate::commands::pty_backend::live_session_ids`], which
///   spans the in-process host AND the session-server sidecar) adds the registry-less
///   alive sessions the register-on-open ledger silently missed (root / notes /
///   git-sync / `#` / no-MAIN-tab) so "if it's running, it shows".
///
/// The in-process enumeration error is SURFACED (propagated as the command's `Err`),
/// never swallowed into an empty/registry-only list — INV-3 forbids silently dropping
/// the live-PTY arm; a sidecar that cannot answer contributes nothing this pass
/// (fail-open, like the other cross-host reads). Off the UI thread via
/// [`await_blocking`]: it locks the registry (snapshot, drop) then probes the hosts.
/// `alive` is real child-process liveness, NOT map membership — a naturally-exited
/// session stays in the registry for replay yet reads `alive:false`.
#[tauri::command]
pub async fn dashboard_list_sessions(app: tauri::AppHandle) -> Result<Vec<SessionRow>, String> {
    await_blocking("dashboard_list_sessions", move || list_sessions_inner(&app)).await
}

pub(crate) fn list_sessions_inner(app: &tauri::AppHandle) -> Result<Vec<SessionRow>, String> {
    use tauri::Manager;
    let launch = app.state::<DashboardLaunchState>();
    // Snapshot under the registry lock, then release it BEFORE probing the
    // terminal hosts (never hold both locks — see `snapshot`).
    let rows = launch.snapshot();
    // Probe every bound session's liveness in ONE `sessions`-lock pass rather
    // than re-locking the hot map per row.
    let bound_ids: Vec<&str> = rows
        .iter()
        .filter_map(|s| s.session_id.as_deref())
        .collect();
    // Unknown is not empty: while a terminal host cannot be asked, a bound session
    // reads alive — a roster that read a still-running agent as dead would offer to
    // dismiss or respawn it.
    let alive_ids = crate::commands::pty_backend::alive_session_ids(app, &bound_ids);
    // An agent launched on a machine has no local session: its tab in the machine's
    // layout says whether it still runs. Read only when there is one.
    let remote_agents = launch.remote_agents();
    let remote_sessions = launch.remote_agent_sessions();
    let remote_alive = if remote_agents.is_empty() {
        HashMap::new()
    } else {
        let replies = crate::commands::pty_layout::member_layout_snapshots_blocking(app)
            .and_then(Result::ok)
            .unwrap_or_default();
        let tabs = crate::mcp::terminal_tabs::live_tabs(&replies, &Default::default());
        let live_machines: Vec<String> = replies
            .iter()
            .filter_map(|reply| match reply {
                super::machines::RuntimeLayoutReply::Live { target, .. } => {
                    Some(target.member.machine_id.clone())
                }
                _ => None,
            })
            .collect();
        let seen: Vec<RemoteTabSeen<'_>> = tabs
            .iter()
            .map(|tab| RemoteTabSeen {
                handle: tab.handle.as_deref(),
                machine_id: &tab.machine_id,
                exited: tab.exited,
            })
            .collect();
        remote_agent_liveness(&remote_agents, &seen, &live_machines)
    };
    let registry_rows: Vec<SessionRow> = rows
        .into_iter()
        .map(|s| {
            if let Some(&alive) = remote_alive.get(&s.handle) {
                let session = remote_sessions.get(&s.handle).cloned();
                return remote_agent_row(s, alive, session);
            }
            // SW-OR-10 (AC-2): a headless pseudo-session has no PTY child to probe, so
            // its liveness is registry presence — while its entry is in the map the run
            // is in flight (`Running`); its completion/kill path removes the entry. This
            // is the documented exception to "alive = child liveness"; every other kind
            // still probes `PtyState`.
            let alive = if is_live_headless(s.kind, s.session_id.as_deref()) {
                true
            } else {
                // Reserved (`begin`) but not yet `bind`ed → no session to probe → dead.
                s.session_id
                    .as_deref()
                    .is_some_and(|sid| !alive_ids.known_gone(sid))
            };
            session_row(s, alive)
        })
        .collect();
    // SW-AV-1 (INV-3): fold in the live-PTY arm across BOTH terminal hosts
    // (in-process + session-server sidecar). The in-process enumeration error
    // surfaces as `Err` (never a silent registry-only fallback); a sidecar that
    // cannot answer contributes nothing this pass (fail-open, like the other
    // cross-host reads).
    let mut rows = union_registry_and_live_pty(
        registry_rows,
        crate::commands::pty_backend::live_session_ids(app),
    )?;
    fill_session_chat(app, &mut rows);
    Ok(rows)
}

pub(crate) fn fill_session_chat(app: &tauri::AppHandle, rows: &mut [SessionRow]) {
    use tauri::Manager;
    let launch = app.state::<DashboardLaunchState>();
    let hub = app.try_state::<crate::live_hub::LiveHub>();
    for row in rows {
        row.chat_workspace_id = row
            .session_id
            .as_deref()
            .and_then(|sid| launch.workspace_for_session(sid));
        if let Some(hub) = &hub {
            (row.group_unread, row.group_read_id) = hub.chat_unread(&row.handle);
        }
    }
}

/// A remote agent's roster row. It has no local session, so its row carries the session
/// its tab runs under in the machine's layout: the session the viewer's tab, its live dot
/// and anything it launched (`launched_by` its session id) are keyed by. Its status is
/// its tab's: running while the tab runs, done once it ends.
pub(crate) fn remote_agent_row(
    s: SessionSnapshot,
    alive: bool,
    session: Option<String>,
) -> SessionRow {
    let mut row = session_row(s, alive);
    if let Some(session) = session {
        row.status = derive_status(alive, Some(&session), false);
        row.session_id = Some(session);
    }
    row
}

/// One tab of a host's layout, as remote agent liveness reads it.
pub(crate) struct RemoteTabSeen<'a> {
    pub handle: Option<&'a str>,
    pub machine_id: &'a str,
    pub exited: bool,
}

/// Whether each remote agent (`(handle, machine)`) still runs: its tab in a live layout
/// says so. A machine whose layout can't be read now (offline, not connected) leaves it
/// running, as an unanswered host does for a local session; a live layout without its
/// tab means it ended.
pub(crate) fn remote_agent_liveness(
    agents: &[(String, String)],
    tabs: &[RemoteTabSeen<'_>],
    live_machines: &[String],
) -> HashMap<String, bool> {
    agents
        .iter()
        .map(|(handle, machine)| {
            let alive = match tabs
                .iter()
                .find(|tab| tab.handle == Some(handle.as_str()) && tab.machine_id == machine)
            {
                Some(tab) => !tab.exited,
                None => !live_machines.iter().any(|m| m == machine),
            };
            (handle.clone(), alive)
        })
        .collect()
}

/// SW-AV-1 (INV-3): the pure union seam — fold the live-PTY arm into the already
/// liveness-probed registry rows. `live_pty` is EITHER every alive managed session
/// id OR the enumeration error; the error is PROPAGATED (INV-3: never degrade to the
/// registry-only view, which would silently drop the registry-less sessions). A live
/// id already bound to a registry row is the registry's (dedup by `session_id`, so a
/// session shows exactly once); the remaining registry-less ids become synthetic
/// [`KIND_LIVE_PTY`] rows. The appended ids are sorted so the roster order is
/// deterministic (a `HashSet` iterates in an arbitrary order).
pub(crate) fn union_registry_and_live_pty(
    registry_rows: Vec<SessionRow>,
    live_pty: Result<std::collections::HashSet<String>, String>,
) -> Result<Vec<SessionRow>, String> {
    let live_ids = live_pty?;
    let bound: std::collections::HashSet<&str> = registry_rows
        .iter()
        .filter_map(|r| r.session_id.as_deref())
        .collect();
    let mut registry_less: Vec<String> = live_ids
        .into_iter()
        .filter(|id| !bound.contains(id.as_str()))
        .collect();
    registry_less.sort();
    let mut rows = registry_rows;
    rows.extend(registry_less.into_iter().map(live_pty_session_row));
    Ok(rows)
}

/// SW-AV-1 (INV-3): synthesize a roster row for a registry-less live-PTY session. It
/// is genuinely alive (its child is running), so `alive:true` / `Running`; it holds
/// no registry metadata (SW-AV-2 demotes it to a bare/utility row; the FE recovers
/// `teamKey`/`workspaceId` from the viewer store). `handle` mirrors the session id
/// so the roster's handle-keyed paths still resolve.
fn live_pty_session_row(session_id: String) -> SessionRow {
    SessionRow {
        output_run_id: None,
        handle: session_id.clone(),
        alive: true,
        status: derive_status(true, Some(&session_id), false),
        kind: KIND_LIVE_PTY.to_string(),
        session_id: Some(session_id),
        backgrounded: false,
        started_with: None,
        started_at_ms: None,
        launcher: None,
        account_provenance: None,
        provenance: None,
        last_output_ms: None,
        meta: LaunchMeta::default(),
        workspace: None,
        chat_workspace_id: None,
        group_unread: 0,
        group_read_id: 0,
        agent_revision: None,
        deadline_at_ms: None,
        expiring: false,
        machine_id: None,
        launch_team_key: None,
    }
}

/// One registry snapshot as the panel row, given its probed liveness.
pub(crate) fn session_row(s: SessionSnapshot, alive: bool) -> SessionRow {
    // SW-BA-1 (AC-3): derive the lifecycle status from the SAME probe — no
    // new PtyState pass. `gate_suspended` defaults false until SW-OR-5 (OQ-2).
    let status = derive_status(alive, s.session_id.as_deref(), false);
    // `expiring` is the ~30s grace window between the sweep's
    // `/exit` and its kill. Both bounds live on the entry so the lister never
    // consults the clock here.
    let expiring = s.exit_sent_at_ms.is_some();
    SessionRow {
        output_run_id: s.output_run_id,
        handle: s.handle,
        session_id: s.session_id,
        kind: s.kind.to_string(),
        alive,
        status,
        backgrounded: s.backgrounded,
        started_with: s.started_with,
        started_at_ms: s.started_at_ms,
        launcher: s.launcher,
        account_provenance: s.account_provenance,
        provenance: s.provenance,
        last_output_ms: s.last_output_ms,
        meta: s.meta,
        workspace: s.worktree_dir.map(|d| d.to_string_lossy().into_owned()),
        chat_workspace_id: None,
        group_unread: 0,
        group_read_id: 0,
        agent_revision: s.agent_revision,
        deadline_at_ms: s.deadline_at_ms,
        expiring,
        machine_id: s.machine_id,
        launch_team_key: s.launch_team_key,
    }
}

/// SW-GW-3 (AC-2): probe REAL child liveness for an ARBITRARY set of session ids
/// — the OBSERVE-half story-workspace liveness path. Unlike `dashboard_list_sessions`
/// (which is FILTERED to registry-bound sessions), this answers for a session
/// opened by ANY means, because a story worktree opened through the normal viewer
/// UI never enters the launch registry (the origin bug). It probes `PtyState`
/// directly with the SAME `try_wait` helper (`alive_session_ids`) — the SW-DX-17a
/// rule holds: `alive` = the child has not exited, NOT map membership and NOT
/// "a tab exists in the store". Returns the alive subset, preserving input order.
/// Off the UI thread; takes NO registry lock (a pure `PtyState` read).
#[tauri::command]
pub async fn dashboard_probe_sessions_alive(
    app: tauri::AppHandle,
    session_ids: Vec<String>,
) -> Result<Vec<String>, String> {
    await_blocking("dashboard_probe_sessions_alive", move || {
        let refs: Vec<&str> = session_ids.iter().map(String::as_str).collect();
        let alive = crate::commands::pty_backend::alive_session_ids(&app, &refs);
        Ok(retain_alive(session_ids, &alive))
    })
    .await
}

/// The order-preserving alive-subset filter, extracted pure so the "return only
/// what the probe reported alive, in the caller's order" contract is regression-
/// guarded without a real PTY. Unknown is not empty: while a terminal host cannot be
/// asked every session reads alive, so the frontend never heals (evicts and
/// respawns) an agent that may still be running.
fn retain_alive(
    session_ids: Vec<String>,
    alive: &crate::commands::pty_backend::Liveness<std::collections::HashSet<String>>,
) -> Vec<String> {
    session_ids
        .into_iter()
        .filter(|s| !alive.known_gone(s))
        .collect()
}

/// SW-DX-17a (AC-2/AC-4): kill the session bound to `handle`, routing through the
/// ONE authoritative `pty::kill_session` choke — so `release_for_session` fires
/// (cap slot freed, both indices dropped) and the kill audit line is written.
/// A handle with no live binding is a typed no-op (`Ok(())`), never an error.
#[tauri::command]
pub fn dashboard_kill_by_handle(app: tauri::AppHandle, handle: String) -> Result<(), String> {
    use tauri::Manager;
    let launch = app.state::<DashboardLaunchState>();
    if let Some(session_id) = launch.resolve(&handle) {
        // Bound session: reuse the tab-close/app-exit kill path verbatim (no new
        // teardown). `kill_session` fires `release_for_session` → cap slot freed +
        // Kill audit line. SW-OR-10: a headless session has no PTY, but the same
        // `release_for_session` choke reaps its process group (see there), so no
        // special case is needed here — every kind kills through the one path.
        crate::commands::pty::kill_session(&app, &session_id);
    } else if launch.abort(&handle) {
        // Reserved-but-unbound (a stuck `begin` that never bound — exactly the
        // leak the panel exists to reveal): there is no PTY session to kill, but
        // the leaked cap slot + registry entry must still be cleared, else the
        // row is undismissable and the slot leaks forever. `abort` frees it;
        // audit the teardown (only fires when `abort` actually removed an entry).
        audit_unbound_abort(&handle);
        // SW-AM-8: no PTY → no `pty-exit`, so the roster needs this to drop the row.
        emit_session_state_changed(&app);
    }
    Ok(())
}

/// SW-SR-5 (AC-2): the teardown over a resolved [`RunStopTargets`] for the run-scoped stop
/// (`drain_run`). Bound sessions go through the per-agent `pty::kill_session` choke verbatim — it fires
/// `release_for_session` → cap slot freed + Kill audit line, so audit + cap accounting stay
/// identical to a manual Kill (no new teardown path). Reserved-but-unbound entries were
/// already dropped by the drain; each gets its Abort audit line here (audit stays in the
/// command layer, consistent with `dashboard_kill_by_handle`'s abort branch). Returns how
/// many it stopped (`to_kill.len() + aborted.len()`), which a run's kill reads to fall back
/// to killing the run's own session when it stopped none. Runs AFTER the registry lock drops — never a
/// `kill_session` under the lock (AC-5).
pub(crate) fn teardown_stop_targets(app: &tauri::AppHandle, targets: RunStopTargets) -> usize {
    let RunStopTargets { to_kill, aborted } = targets;
    for sid in &to_kill {
        crate::commands::pty::kill_session(app, sid);
    }
    for handle in &aborted {
        audit_unbound_abort(handle);
    }
    // SW-AM-8: the bound kills each fired `pty-exit` (the roster refetches on those),
    // but reserved-but-unbound aborts have no PTY → one emit so those rows drop too.
    if !aborted.is_empty() {
        emit_session_state_changed(app);
    }
    to_kill.len() + aborted.len()
}

/// SW-DX-17d (AC-1/AC-3b): **bring to foreground** — release the background-limit
/// slot held by `handle` (it's becoming an attended foreground tab). Keeps the
/// entry so it stays addressable. Idempotent no-op if it
/// held no slot. The FE opens the tab; this only corrects the accounting.
#[tauri::command]
pub fn dashboard_foreground_session(
    app: tauri::AppHandle,
    state: tauri::State<'_, DashboardLaunchState>,
    handle: String,
) {
    // SW-AM-8: foregrounding only drops the permit (the PTY stays alive), so the
    // roster has no other signal that the backgrounded count fell.
    // Gate on a REAL release so a no-op foreground doesn't fire a spurious refetch.
    if state.foreground(&handle) {
        emit_session_state_changed(&app);
    }
}

/// SW-DX-17d (AC-3): push the user-owned background-agent limit into the trusted
/// side. Called by the FE settings hook (mirror of `useBackgroundSetupCap`) on
/// mount and whenever the setting changes. Non-destructive when lowered.
#[tauri::command]
pub fn dashboard_set_background_limit(state: tauri::State<'_, DashboardLaunchState>, limit: usize) {
    state.set_limit(limit);
}

/// The trusted-side resolution of a dashboard's `storyId` to its managed story
/// doc (AC-11). The path comes from the org's frontmatter cache, which only
/// indexes managed context docs, so it is confined to a managed target by
/// construction — never a caller-supplied or relative path.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryPathResolution {
    /// The story's managed absolute file path.
    pub story_file_path: String,
    /// The story's team (the FE pairs it with the active org to form the teamKey).
    pub team: String,
    /// SW-GT-27: the story's `source_repo` slug (`owner/repo`), or `None` when absent.
    /// The repo whose content layers fill this story's prompt/validator slots at dispatch
    /// and validation.
    pub source_repo: Option<String>,
}

/// AC-11: resolve `storyId → managed story path` trusted-side via the org's
/// frontmatter cache (`by_story_id`). Rejects an unknown or ambiguous id rather
/// than guessing — the FE never supplies the path. The cache indexes only
/// managed context docs, so the returned path passes the managed-target wall by
/// construction. (Story-SPECIFIC — distinct from the generic handle registry.)
///
/// SW-CR-9: On an initial cache miss, falls back to a bounded disk scan of the
/// active-org content tree — an agent that drops a story MD via its own raw
/// filesystem writer never fires an FS watcher event, so the shared cache would
/// otherwise stay cold until a cold restart or a `move_content_file` dance. The
/// scan runs ONLY on `NotFound` (the hot path is unchanged); a hit triggers one
/// targeted `refresh_under_lock` and a single retry.
#[tauri::command]
pub fn resolve_story_path(
    app: tauri::AppHandle,
    story_id: String,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<StoryPathResolution, String> {
    use tauri::Manager;
    let shared = app
        .try_state::<crate::install::frontmatter_cache::SharedFrontmatterCache>()
        .ok_or_else(|| "unavailable: frontmatter cache not ready".to_string())?;
    // Active-org scan root for the SW-CR-9 fallback. Reads happen without the
    // async team-activation lock — this sync command runs on a Tauri runtime
    // worker and cannot await — but the shared `SharedFrontmatterCache` the
    // fallback's `refresh_under_lock` publishes IS the one `derive_active_context`
    // ultimately feeds, so a transient activation race only means the scan
    // targets the previously-active org (a benign no-op cache miss, never a
    // write to the wrong tree). A missing active team / profile / home → `None`,
    // which degrades to the pre-SW-CR-9 behaviour (vanilla NotFound).
    let scan_root = active_org_root_sync(&app);
    let scope =
        crate::context_repo::caller_scope::for_page(&app, page.as_deref(), frame_id.as_deref())?;
    resolve_story_path_with_fallback_for(shared.inner(), scan_root.as_deref(), &story_id, &scope)
}

/// SW-CR-9: sync resolve of the active org's root dir for the fallback scan.
/// Does not take the async team-activation lock (see `resolve_story_path` for
/// why that is safe here).
fn active_org_root_sync(app: &tauri::AppHandle) -> Option<PathBuf> {
    let active = app
        .try_state::<crate::mcp::state::ActiveTeamState>()?
        .get()?;
    let home = crate::commands::paths::codesense_home_dir()?;
    let user_id = crate::commands::local_profile::active(app)
        .ok()?
        .principal
        .numeric_id()
        .ok()?;
    let principal = Principal::from(user_id);
    crate::context_repo::path_to_org_root(&home, &principal, &active.org_slug).ok()
}

/// SW-CR-9: shared "snapshot → scan on miss → refresh → retry → better error"
/// path used by the active-org story resolvers. `scan_root` is the active-org
/// content tree (`{home}/users/{uid}/orgs/{org}`) when known; `None` disables
/// the fallback and the function behaves exactly like the pre-SW-CR-9 flow.
pub(crate) fn resolve_story_path_with_fallback(
    shared: &crate::install::frontmatter_cache::SharedFrontmatterCache,
    scan_root: Option<&Path>,
    story_id: &str,
) -> Result<StoryPathResolution, String> {
    use crate::install::frontmatter_cache::{FrontmatterCache, StoryIdLookup};
    let snap = FrontmatterCache::snapshot(shared);
    match resolve_story_path_in(&snap, story_id) {
        Ok(r) => return Ok(r),
        // Ambiguous ids are already a resolved collision (surfaced as
        // `DUPLICATE_STORY_ID` elsewhere); the fallback would not disambiguate
        // them — fall through to the original error.
        Err(err) if !matches!(classify_story_err(&err), Some(StoryIdLookup::NotFound)) => {
            return Err(err);
        }
        Err(_) => {}
    }

    // SW-CR-9 (R2): keep each error's FIRST LINE identical to a pre-SW-CR-9 typed
    // refusal (`no managed story with id 'X'` or the new `story file candidate …`
    // kind) so the FE `userFacingErrorMessage` regex matches it off `reasonLine` and
    // renders the friendly copy. The hint lives on the next line and is dropped by
    // `reasonLine`, but preserved in the raw string for an agent that reads it.
    let Some(scan_root) = scan_root else {
        return Err(format!(
            "unavailable: no managed story with id '{story_id}'\n\
             Frontmatter cache + fallback scan both missed; verify the file is in a \
             content layer with type: story and story_id: {story_id}. Call \
             refresh_frontmatter_cache(path) if the file was just written outside CodeSense."
        ));
    };

    let candidate = FrontmatterCache::scan_org_for_story_id(scan_root, story_id);
    if let Some((ref path, ref team)) = candidate {
        FrontmatterCache::refresh_under_lock(shared, path, team);
        let retry = FrontmatterCache::snapshot(shared);
        if let Ok(r) = resolve_story_path_in(&retry, story_id) {
            return Ok(r);
        }
    }

    Err(match candidate {
        Some((path, _)) => format!(
            "unavailable: story file candidate at {} does not carry story_id '{}'\n\
             Verify its frontmatter has type: story, story_id: {}, and source_repo. \
             Call refresh_frontmatter_cache(path) after fixing it.",
            path.display(),
            story_id,
            story_id,
        ),
        None => format!(
            "unavailable: no managed story with id '{story_id}'\n\
             Frontmatter cache + fallback scan both missed; verify the file is in a \
             content layer with type: story and story_id: {story_id}. Call \
             refresh_frontmatter_cache(path) if the file was just written outside CodeSense."
        ),
    })
}

/// SW-CR-9: classify a `resolve_story_path_in` error so the fallback can decide
/// whether to scan. Only the `NotFound` arm triggers a scan; `Ambiguous` and
/// malformed-team errors fall through unchanged. Matches the error-message
/// prefixes `resolve_story_path_in` produces.
fn classify_story_err(err: &str) -> Option<crate::install::frontmatter_cache::StoryIdLookup> {
    if err.starts_with("unavailable: no managed story with id") {
        Some(crate::install::frontmatter_cache::StoryIdLookup::NotFound)
    } else if err.starts_with("unavailable: ambiguous story id") {
        // Carry the count as 0 — the fallback only reads the discriminant.
        Some(crate::install::frontmatter_cache::StoryIdLookup::Ambiguous(
            0,
        ))
    } else {
        None
    }
}

/// Whether a story lookup's error is a miss: the first line of the not-found text, or of
/// the fallback scan's "story file candidate" text (a file that does not carry the id).
pub(crate) fn is_story_not_found(error: &str) -> bool {
    let first = error.lines().next().unwrap_or_default();
    matches!(
        classify_story_err(first),
        Some(crate::install::frontmatter_cache::StoryIdLookup::NotFound)
    ) || first.starts_with("unavailable: story file candidate at ")
}

pub(crate) fn resolve_story_path_with_fallback_for(
    shared: &crate::install::frontmatter_cache::SharedFrontmatterCache,
    scan_root: Option<&Path>,
    story_id: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<StoryPathResolution, String> {
    use crate::install::frontmatter_cache::FrontmatterCache;
    if matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        return resolve_story_path_with_fallback(shared, scan_root, story_id);
    }
    let snapshot = FrontmatterCache::snapshot(shared);
    let found = resolve_story_path_in_for(&snapshot, story_id, scope);
    if found.is_ok() {
        return found;
    }
    if let Some(root) = scan_root {
        let scanned = FrontmatterCache::build_from_scan(root);
        if let Ok(resolved) = resolve_story_path_in_for(&scanned, story_id, scope) {
            FrontmatterCache::refresh_under_lock(
                shared,
                Path::new(&resolved.story_file_path),
                &resolved.team,
            );
            return resolve_story_path_in_for(&FrontmatterCache::snapshot(shared), story_id, scope);
        }
    }
    found
}

pub(crate) fn resolve_story_path_in_for(
    cache: &crate::install::frontmatter_cache::FrontmatterCache,
    story_id: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<StoryPathResolution, String> {
    if matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        return resolve_story_path_in(cache, story_id);
    }
    resolve_story_path_in(
        &crate::install::org_story_resolver::visible_cache(cache, scope),
        story_id,
    )
    .map_err(|error| {
        if error.starts_with("unavailable: no managed story") {
            crate::context_repo::caller_scope::MISSING.into()
        } else {
            error
        }
    })
}

pub(crate) fn resolve_story_path_in(
    cache: &crate::install::frontmatter_cache::FrontmatterCache,
    story_id: &str,
) -> Result<StoryPathResolution, String> {
    // Shared arity gate (`single_by_story_id`) — one authority for "which story
    // is this?", also used by the trusted MCP cold-create path (SW-OR-1a).
    use crate::install::frontmatter_cache::StoryIdLookup;
    let entry = cache.single_by_story_id(story_id).map_err(|e| match e {
        StoryIdLookup::NotFound => {
            format!("unavailable: no managed story with id '{story_id}'")
        }
        // Story ids are unique within an org; a 2+ result is a genuine collision
        // (surfaced elsewhere as DUPLICATE_STORY_ID) — refuse to guess a target.
        StoryIdLookup::Ambiguous(n) => {
            format!("unavailable: ambiguous story id '{story_id}' ({n} indexed)")
        }
    })?;
    let team = entry.team();
    if team.is_empty() {
        // A malformed doc with no team would form an invalid `orgSlug:` teamKey on
        // the FE — refuse rather than open under a bad bucket.
        return Err(format!(
            "unavailable: story '{story_id}' has no team (malformed frontmatter?)"
        ));
    }
    let source_repo = {
        let sr = entry.source_repo();
        (!sr.is_empty()).then(|| sr.to_string())
    };
    Ok(StoryPathResolution {
        story_file_path: entry.path.display().to_string(),
        team: team.to_string(),
        source_repo,
    })
}

/// SW-AU-51: resolve `story_id → managed story path` in an arbitrary **synced org**
/// — the cross-org sibling of [`resolve_story_path`]. Where that command reads the
/// active-org `SharedFrontmatterCache`, this resolves against `org_slug`'s index via
/// the [`OrgStoryResolver`] (the active org delegates back to the shared cache; a
/// non-active synced org is built + held; a never-cloned org resolves empty). Reuses
/// the same arity gate [`resolve_story_path_in`], so "which story is this?" has one
/// authority across the active and cross-org paths.
///
/// `active` supplies the ambient facts (`codesense_home`, `principal`) — user-global,
/// so the active context's are correct for any target org — and names the active org
/// so the resolver can delegate when `org_slug` is it. `scope` limits the lookup to
/// what the caller can see.
pub(crate) fn resolve_story_path_in_org_for<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    active: &crate::commands::route_to_context::ActiveContext,
    org_slug: &str,
    story_id: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<StoryPathResolution, String> {
    use tauri::Manager;
    let shared = app
        .try_state::<crate::install::frontmatter_cache::SharedFrontmatterCache>()
        .ok_or_else(|| "unavailable: frontmatter cache not ready".to_string())?;
    // SW-CR-9: on the active-org path, go through the fallback wrapper so an
    // agent-authored story that just landed via a raw filesystem writer still
    // resolves without a `move_content_file` dance. Cross-org resolution keeps
    // its original build-once semantics via `OrgStoryResolver` — the resolver
    // scans the whole target org on first touch, so a cache-miss there is a
    // genuine miss and the fallback has nothing to offer.
    if org_slug == active.org_slug {
        let scan_root = crate::context_repo::path_to_org_root(
            &active.codesense_home,
            &active.principal(),
            org_slug,
        )
        .ok();
        return resolve_story_path_with_fallback_for(
            shared.inner(),
            scan_root.as_deref(),
            story_id,
            scope,
        );
    }
    let resolver = app
        .try_state::<std::sync::Arc<crate::install::org_story_resolver::OrgStoryResolver>>()
        .ok_or_else(|| "unavailable: org story resolver not ready".to_string())?;
    let cache = crate::install::org_story_resolver::OrgStoryResolver::snapshot_for(
        &resolver,
        &shared,
        Some(&active.org_slug),
        &active.codesense_home,
        &active.principal(),
        org_slug,
    );
    resolve_story_path_in_for(&cache, story_id, scope)
}

/// [`resolve_story_path_in_org_for`], rescanning a held non-active org once when it misses:
/// its index is point-in-time, so a story written there after the first build would
/// otherwise resolve only after a restart. The active org has its own fallback scan, and an
/// org this lookup built is not rescanned: that build was the fresh scan. A scoped `MISSING`
/// is never rescanned: the caller asks about a story it cannot see.
/// Blocking: a cold or rescanned org is a full scan, so async callers run it off the runtime.
pub(crate) fn resolve_story_path_in_org_rescanning<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    active: &crate::commands::route_to_context::ActiveContext,
    org_slug: &str,
    story_id: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<StoryPathResolution, String> {
    let held = org_index_held(app, org_slug);
    let found = resolve_story_path_in_org_for(app, active, org_slug, story_id, scope);
    if !held || !matches!(&found, Err(error) if is_story_not_found(error)) {
        return found;
    }
    rescan_story_path_in_org(app, active, org_slug, story_id, scope).unwrap_or(found)
}

/// An agent's story lookup in `org_slug`: the result is what `scope` sees, a story it cannot
/// see being `MISSING`. A miss in a held org is first rescanned unscoped, so a story just
/// written there is found; that pass only refreshes the index, and its result (which can
/// name stories and paths outside the caller's view) is dropped. A team's error is the
/// scoped lookup's own, unmapped: an ambiguous or malformed lookup's text counts and names
/// stories, so it reaches an agent only through
/// [`crate::commands::workspace_open::story_open_refusal`]. An unknown caller gets `MISSING`.
pub(crate) fn resolve_story_path_in_org_as<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    active: &crate::commands::route_to_context::ActiveContext,
    org_slug: &str,
    story_id: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<StoryPathResolution, String> {
    use crate::context_repo::caller_scope::{CallerScope, MISSING};
    let _ =
        resolve_story_path_in_org_rescanning(app, active, org_slug, story_id, &CallerScope::Human);
    let found = resolve_story_path_in_org_for(app, active, org_slug, story_id, scope);
    if matches!(scope, CallerScope::Unknown) {
        return found.map_err(|_| MISSING.into());
    }
    found
}

/// Whether `org_slug` has a held index a rescan could refresh. `false` for the active org
/// (served from the shared cache) and for an org not built yet, whose next lookup is itself
/// a full scan.
pub(crate) fn org_index_held<R: tauri::Runtime>(app: &tauri::AppHandle<R>, org_slug: &str) -> bool {
    use tauri::Manager;
    app.try_state::<std::sync::Arc<crate::install::org_story_resolver::OrgStoryResolver>>()
        .is_some_and(|resolver| resolver.is_held(org_slug))
}

/// Evict `org_slug`'s held index and resolve `story_id` in a fresh scan of it. `None` when
/// there was nothing to rescan: the active org (its lookup already falls back to a scan)
/// or an org with no held index (none built yet, or an empty build). A caller whose own
/// lookup just built the index skips this: see [`org_index_held`].
pub(crate) fn rescan_story_path_in_org<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    active: &crate::commands::route_to_context::ActiveContext,
    org_slug: &str,
    story_id: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Option<Result<StoryPathResolution, String>> {
    use tauri::Manager;
    if org_slug == active.org_slug {
        return None;
    }
    let evicted = app
        .try_state::<std::sync::Arc<crate::install::org_story_resolver::OrgStoryResolver>>()
        .is_some_and(|resolver| resolver.evict(org_slug));
    evicted.then(|| resolve_story_path_in_org_for(app, active, org_slug, story_id, scope))
}

/// SW-DX-16: the resolved location of an EXPLICIT epic (`{team, epic}`). Unlike
/// the [`StoryPathResolution`] it parallels, it does NOT echo `team`: for a story
/// the resolver DISCOVERS the owning team the FE didn't know, but for an epic the
/// team is FE *input* (it passed the teamKey), so the FE already holds it — the
/// resolver only needs to return what the FE can't compute: the canonical mainDoc
/// (`epic_file_path`, the integration id / re-entrancy key) and the leaf
/// `epic_dir_name` the viewer record keys on.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EpicPathResolution {
    /// The epic's canonical mainDoc absolute path (README/index by precedence).
    pub epic_file_path: String,
    /// The epic directory's leaf name.
    pub epic_dir_name: String,
}

/// SW-DX-16: resolve an explicit `{team, epic}` → the epic's canonical mainDoc,
/// trusted-side — the epic analog of [`resolve_story_path`], and what widens the
/// `openWorkspace` epic target beyond the board's OWN dir. The dashboard supplies
/// only a content-root-relative epic identifier (a dir or a doc path); the
/// trusted side canonicalizes + confines it and picks the mainDoc by the
/// scanner's precedence. The command context (`codesense_home`/`org_slug`/…)
/// comes from the FE's already-resolved `EpicCommandContext` — the SAME shape
/// `ensure_epic_workspace` takes — never from doc input, so the doc can only
/// name a team/org the USER actually has.
#[tauri::command]
pub fn resolve_epic_path(
    app: tauri::AppHandle,
    codesense_home: String,
    github_user_id: u64,
    org_slug: String,
    team: String,
    epic: String,
    frame_id: Option<String>,
    page: Option<String>,
) -> Result<EpicPathResolution, String> {
    let principal = Principal::from(github_user_id);
    let scope =
        crate::context_repo::caller_scope::for_page(&app, page.as_deref(), frame_id.as_deref())?;
    let resolved = resolve_epic_path_in(
        Path::new(&codesense_home),
        &principal,
        &org_slug,
        &team,
        &epic,
    );
    if matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        return resolved;
    }
    let resolved = resolved.map_err(|_| crate::context_repo::caller_scope::MISSING.to_string())?;
    scope.existing(Path::new(&resolved.epic_file_path))?;
    Ok(resolved)
}

fn resolve_epic_path_in(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    team: &str,
    epic: &str,
) -> Result<EpicPathResolution, String> {
    let content_canon = resolve_team_content_root(codesense_home, principal, org_slug, team)?;
    // Canonicalize the requested epic under the (already-canonical) content root;
    // the POST-canonicalization prefix check catches `..` and out-of-tree symlink
    // escape — the same wall `read_workspace_file` uses. `join` correctly handles
    // an absolute `epic` that already lives under the tree (it replaces, then the
    // prefix check re-confines).
    let target = content_canon
        .join(epic)
        .canonicalize()
        .map_err(|_| format!("unavailable: no epic at '{epic}'"))?;
    if !target.starts_with(&content_canon) {
        return Err(format!("traversal: '{epic}' escapes the team content root"));
    }
    // Accept a dir OR an epic doc path — resolve to the epic directory.
    let epic_dir = if target.is_dir() {
        target.clone()
    } else {
        target
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| target.clone())
    };
    // The content ROOT is never itself an epic. Without this an `epic` that
    // resolves ONTO the content root — `"."`, `"./"`, `"epics/.."`, `""`, or a
    // file that lives directly under the root — would pass the `starts_with`
    // wall (a path starts-with itself), find the root's top-level README, and
    // materialize/spawn against the WHOLE team content tree. Reject by equality
    // (the prefix check can't catch a target that IS the root).
    if epic_dir == content_canon {
        return Err(format!(
            "unavailable: '{epic}' resolves to the team content root, not an epic"
        ));
    }
    // Managed-target confinement (AC-3, defense in depth — the traversal wall
    // already confines to `content` ⊂ `users`). The canon helper canonicalizes
    // `codesense_home` so the pure prefix check doesn't false-miss on a symlinked
    // home (macOS `/var` → `/private/var`), matching the canonicalized target.
    if !crate::install::worktree_index::is_codesense_managed_target_canon(&epic_dir, codesense_home)
        && !crate::context_repo::context_attachments::contains_for_identity(
            codesense_home,
            principal,
            org_slug,
            &epic_dir,
        )?
    {
        return Err("unmanaged: epic dir is not a codesense-managed target".to_string());
    }
    // MainDoc by the scanner's precedence (shared `MAIN_DOC_DEFAULTS`). The FE
    // passes the explicit-epic case through here, so it honest-degrades to `null`
    // on a config-only custom mainDoc exactly as the own-epic path does — the
    // resolver is custom-name-capable but the bridge supplies no team config, so
    // `None` keeps the two epic paths behaviorally consistent.
    let main_doc = crate::commands::pipeline_scanner::resolve_epic_main_doc(&epic_dir, None)
        .ok_or_else(|| format!("unavailable: no epic doc in '{epic}'"))?;
    let epic_dir_name = epic_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    Ok(EpicPathResolution {
        epic_file_path: main_doc.display().to_string(),
        epic_dir_name,
    })
}

/// RF-7: resolve a team's **managed content-repo root** — the dir that directly
/// holds the install-layer `.claude/skills/` — to anchor a `run_skill` agent's
/// cwd so a cs-published `/<skill>` resolves. Mirrors the content-root prelude of
/// [`resolve_epic_path_in`] (`path_to_org_root` → `path_to_team_content_dir` →
/// `canonicalize`) and shares its symlink-safe managed-surface wall
/// (`is_codesense_managed_target_canon`, SLW-FX-13). A missing content dir or a
/// non-managed surface returns a typed `unavailable:` so the FE degrades honestly
/// — never a silent run in the wrong place. Pure over its inputs + the FS, so it
/// is unit-testable against a tempdir without Tauri state.
pub(crate) fn resolve_managed_team_content_root(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    team: &str,
) -> Result<PathBuf, String> {
    resolve_managed_team_content_root_for(
        codesense_home,
        principal,
        org_slug,
        team,
        &Default::default(),
    )
}

pub(crate) fn resolve_managed_team_content_root_for(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    team: &str,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<PathBuf, String> {
    if !matches!(scope, crate::context_repo::caller_scope::CallerScope::Human) {
        let root = crate::context_repo::path_to_org_root(codesense_home, principal, org_slug)?;
        let content =
            crate::context_repo::inheritance::team_view(&root, org_slug, team).own_content();
        scope.existing(
            content
                .own_path()
                .ok_or(crate::context_repo::caller_scope::MISSING)?,
        )?;
    }
    let content_canon = resolve_team_content_root(codesense_home, principal, org_slug, team)?;
    if !crate::install::worktree_index::is_codesense_managed_target_canon(
        &content_canon,
        codesense_home,
    ) && !crate::context_repo::context_attachments::contains_for_identity(
        codesense_home,
        principal,
        org_slug,
        &content_canon,
    )? {
        return Err(format!(
            "unavailable: content root for team '{team}' is not a codesense-managed surface"
        ));
    }
    Ok(content_canon)
}

/// Resolve a team's content-repo root to its canonical path
/// (`path_to_org_root` → `path_to_team_content_dir` → `canonicalize`). Pure over
/// its inputs + the FS. The single home of this resolution — shared by
/// [`resolve_epic_path_in`] and [`resolve_managed_team_content_root`] so the two
/// can never drift on layout or the missing-content degrade. Does NOT check
/// managed-surface confinement; callers add the wall that fits their target
/// (the epic dir vs. the content root itself).
fn resolve_team_content_root(
    codesense_home: &Path,
    principal: &Principal,
    org_slug: &str,
    team: &str,
) -> Result<PathBuf, String> {
    let org_root = crate::context_repo::path_to_org_root(codesense_home, principal, org_slug)?;
    crate::context_repo::path_to_team_content_dir(&org_root, team)
        .canonicalize()
        .map_err(|_| format!("unavailable: no content for team '{team}'"))
}

impl DashboardLaunchState {
    /// Opaque handle lookup only; callers must validate current owner and origin before use.
    pub(crate) fn remote_shell_by_handle(
        &self,
        handle: &str,
    ) -> Option<super::pty_layout::RemoteShellRecord> {
        self.lock()
            .by_handle
            .get(handle)
            .and_then(|entry| entry.remote_shell.clone())
    }

    pub(crate) fn remote_shell_record(
        &self,
        team: &str,
        machine: &str,
        reference: &super::pty_layout::RemoteTabReference,
    ) -> Option<super::pty_layout::RemoteShellRecord> {
        self.lock()
            .by_handle
            .values()
            .filter_map(|entry| entry.remote_shell.as_ref())
            .find(|record| {
                record.team_key == team
                    && record.machine_id == machine
                    && match reference {
                        super::pty_layout::RemoteTabReference::Handle(handle) => {
                            &record.handle == handle
                        }
                        super::pty_layout::RemoteTabReference::RequestId(id) => {
                            &record.request_id == id
                        }
                    }
            })
            .cloned()
    }

    pub(crate) fn reserve_remote_shell(
        &self,
        record: super::pty_layout::RemoteShellRecord,
    ) -> Result<(), Box<super::pty_layout::RemoteShellRecord>> {
        let mut inner = self.lock();
        if let Some(existing) = inner
            .by_handle
            .values()
            .filter_map(|entry| entry.remote_shell.as_ref())
            .find(|existing| {
                existing.owner_id == record.owner_id
                    && existing.team_key == record.team_key
                    && existing.machine_id == record.machine_id
                    && existing.request_id == record.request_id
            })
        {
            return Err(Box::new(existing.clone()));
        }
        let meta = remote_launch_meta(&record);
        let started_with = (!record.identity.is_empty())
            .then(|| record.agent.as_ref()?.startup.prompt.clone())
            .flatten();
        // The saved Agent it runs, so its row offers its inbox like a local one's.
        let agent_revision = record
            .agent
            .as_ref()
            .map(|agent| agent.manifest.agent.to_string());
        inner.by_handle.insert(
            record.handle.clone(),
            LaunchEntry {
                chat_workspace_id: None,
                remote_shell: Some(record),
                bound_at_ms: None,
                ready_at_ms: None,
                startup_noted: false,
                session_id: None,
                launcher_task: None,
                story_was_explicit: false,
                local_effect_owner: None,
                _permit: None,
                kind: KIND_REGISTERED,
                started_with,
                started_at_ms: Some(now_epoch_ms()),
                launcher: None,
                account_provenance: None,
                provenance: None,
                routine_key: None,
                worktree_dir: None,
                profile_id: None,
                owner: None,
                headless_pid: None,
                headless_cancel: None,
                interactive_capture: None,
                capture_raw_terminal: false,
                last_drain_ms: None,
                meta,
                agent_revision,
                // remote launches are not deadline-swept today;
                // enforcement needs the remote plumbing (follow-up).
                max_runtime_ms: None,
                deadline_ms: None,
                exit_sent_at_ms: None,
                restored_overdue: false,
                launch_settings: None,
            },
        );
        Ok(())
    }

    /// Every agent launched on a machine through an agent launch, as `(handle, machine)`.
    pub(crate) fn remote_agents(&self) -> Vec<(String, String)> {
        self.remote_agent_records()
            .into_iter()
            .map(|(handle, machine, _)| (handle, machine))
            .collect()
    }

    /// Every agent launched on a machine through an agent launch, by handle: its session
    /// in the machine's layout (the session its tab shows under).
    pub(crate) fn remote_agent_sessions(&self) -> HashMap<String, String> {
        self.remote_agent_records()
            .into_iter()
            .map(|(handle, _, session)| (handle, session))
            .collect()
    }

    fn remote_agent_records(&self) -> Vec<(String, String, String)> {
        self.lock()
            .by_handle
            .values()
            .filter_map(|entry| entry.remote_shell.as_ref())
            .filter(|record| record.agent.is_some() && !record.identity.is_empty())
            .map(|record| {
                (
                    record.handle.clone(),
                    record.machine_id.clone(),
                    record.target.session.as_str().to_string(),
                )
            })
            .collect()
    }

    /// The remote tab handle a tab on a machine runs under, by its session there (its
    /// `$CODESENSE_TAB_SESSION_ID`). Any tab qualifies, as a local tab does: an agent
    /// started by hand in a plain Claude or shell tab still reads and sends its inbox.
    pub(crate) fn remote_handle_for_session(&self, session_id: &str) -> Option<String> {
        self.lock()
            .by_handle
            .values()
            .filter_map(|entry| entry.remote_shell.as_ref())
            .find(|record| record.target.session.as_str() == session_id)
            .map(|record| record.handle.clone())
    }

    /// Every remote tab this app launched on `machine`.
    pub(crate) fn remote_shells_on_machine(
        &self,
        machine: &str,
    ) -> Vec<super::pty_layout::RemoteShellRecord> {
        self.lock()
            .by_handle
            .values()
            .filter_map(|entry| entry.remote_shell.as_ref())
            .filter(|record| record.machine_id == machine)
            .cloned()
            .collect()
    }

    /// SW-AGR-11: the remote tab on `machine` whose session there is
    /// `session_id`, if this app launched it.
    pub(crate) fn remote_shell_on_machine(
        &self,
        machine: &str,
        session_id: &str,
    ) -> Option<super::pty_layout::RemoteShellRecord> {
        self.lock()
            .by_handle
            .values()
            .filter_map(|entry| entry.remote_shell.as_ref())
            .find(|record| {
                record.machine_id == machine && record.target.session.as_str() == session_id
            })
            .cloned()
    }

    /// Replace a remote tab's row, keeping a newer admission of the same runtime
    /// that a reconnect gave it meanwhile.
    pub(crate) fn update_remote_shell(&self, mut record: super::pty_layout::RemoteShellRecord) {
        if let Some(entry) = self.lock().by_handle.get_mut(&record.handle) {
            if let Some(old) = entry.remote_shell.as_ref() {
                let (held, given) = (&old.target.runtime, &record.target.runtime);
                let mut same = held.clone();
                same.admission_revision = given.admission_revision;
                if same == *given && held.admission_revision > given.admission_revision {
                    record.target.runtime = held.clone();
                }
            }
            entry.remote_shell = Some(record);
        }
    }
}

impl DashboardLaunchState {
    pub(crate) fn remote_shell_for_target(
        &self,
        target: &super::machines::RuntimeSessionTarget,
    ) -> Option<super::pty_layout::RemoteShellRecord> {
        self.lock()
            .by_handle
            .values()
            .filter_map(|entry| entry.remote_shell.as_ref())
            .find(|record| record.target == *target)
            .cloned()
    }
}

/// What a remote agent's registry row shows: its tab name, who launched it, its story,
/// team, role and avatar. A tab a person opened carries none of it.
fn remote_launch_meta(record: &super::pty_layout::RemoteShellRecord) -> LaunchMeta {
    let identity = &record.identity;
    if identity.is_empty() {
        return LaunchMeta::default();
    }
    LaunchMeta {
        launcher_context: identity.launcher_context.clone(),
        display_name: record.name.clone(),
        launched_by: identity.launched_by.clone(),
        story: identity.story.clone(),
        team: launch_team_from_key(&record.team_key),
        role: identity.role.clone(),
        queue_role: identity.queue_role.clone().flatten(),
        role_join_error: None,
        avatar: identity.avatar.clone(),
        continues: None,
        reviews: None,
    }
}

/// Identifies one remote launch's settlement: `(owner, team, machine, request id)`.
pub(crate) type SettlementKey = (u64, String, String, uuid::Uuid);

impl DashboardLaunchState {
    /// The caller of remote launch `key` was answered while the launch still runs.
    pub(crate) fn remote_launch_running(&self, key: SettlementKey) {
        self.remote_launch_running_at(key, std::time::Instant::now());
    }
    pub(crate) fn remote_launch_running_at(&self, key: SettlementKey, now: std::time::Instant) {
        self.record_settlement(key, super::pty_layout::LaunchSettlement::Running, now);
    }
    /// Remote launch `key` ended after its caller was answered.
    pub(crate) fn settle_remote_launch(
        &self,
        key: SettlementKey,
        outcome: super::pty_layout::RemoteTabOutcome,
    ) {
        self.settle_remote_launch_at(key, outcome, std::time::Instant::now());
    }
    pub(crate) fn settle_remote_launch_at(
        &self,
        key: SettlementKey,
        outcome: super::pty_layout::RemoteTabOutcome,
        now: std::time::Instant,
    ) {
        self.record_settlement(
            key,
            super::pty_layout::LaunchSettlement::Settled(Box::new(outcome)),
            now,
        );
    }
    /// What remote launch `key` is known as, until its record expires.
    pub(crate) fn remote_launch_settlement(
        &self,
        key: &SettlementKey,
    ) -> Option<super::pty_layout::LaunchSettlement> {
        self.remote_launch_settlement_at(key, std::time::Instant::now())
    }
    pub(crate) fn remote_launch_settlement_at(
        &self,
        key: &SettlementKey,
        now: std::time::Instant,
    ) -> Option<super::pty_layout::LaunchSettlement> {
        let mut inner = self.lock();
        expire_settlements(&mut inner, now);
        inner
            .remote_settlements
            .get(key)
            .map(|(settlement, _)| settlement.clone())
    }
    fn record_settlement(
        &self,
        key: SettlementKey,
        settlement: super::pty_layout::LaunchSettlement,
        now: std::time::Instant,
    ) {
        let mut inner = self.lock();
        expire_settlements(&mut inner, now);
        inner.remote_settlements.insert(key, (settlement, now));
    }
}

fn expire_settlements(inner: &mut Inner, now: std::time::Instant) {
    inner.remote_settlements.retain(|_, (_, at)| {
        now.saturating_duration_since(*at) <= super::pty_layout::LAUNCH_SETTLEMENT_TTL
    });
}

pub(crate) enum RemoteShellReservation {
    Fresh,
    Existing(Box<super::pty_layout::RemoteShellRecord>),
}

const MAX_REMOTE_INTENT_BYTES: usize = 16 * 1024 * 1024;
fn remote_intent_path(
    root: &std::path::Path,
    record: &super::pty_layout::RemoteShellRecord,
) -> Result<std::path::PathBuf, String> {
    intent_path(
        root,
        &record.team_key,
        &record.machine_id,
        record.request_id,
    )
}
/// Where the receipt of `request` on `machine` for `team` is kept.
fn intent_path(
    root: &std::path::Path,
    team: &str,
    machine: &str,
    request: uuid::Uuid,
) -> Result<std::path::PathBuf, String> {
    let key = serde_json::to_vec(&(team, machine, request)).map_err(|e| e.to_string())?;
    Ok(root.join(format!("{}.json", blake3::hash(&key).to_hex())))
}

fn read_remote_intent(
    path: &std::path::Path,
) -> Result<Option<super::pty_layout::RemoteShellRecord>, String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("consumed remote intent unavailable: {e}")),
    };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.len() > MAX_REMOTE_INTENT_BYTES as u64
    {
        return Err("remote intent is unsafe or exceeds its size bound".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_REMOTE_INTENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_REMOTE_INTENT_BYTES {
        return Err("remote intent exceeds its size bound".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| format!("consumed remote intent is incomplete; do not create again: {e}"))
}

impl DashboardLaunchState {
    pub(crate) fn refresh_remote_spawn_phase(
        &self,
        root: &std::path::Path,
        record: &mut super::pty_layout::RemoteShellRecord,
    ) -> Result<(), String> {
        let marker = remote_intent_path(root, record)?.with_extension("spawn");
        match std::fs::symlink_metadata(marker) {
            Ok(_) => record.mark_spawn_attempted(), // Any marker, including partial evidence, consumes spawn.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("spawn-attempt evidence unavailable: {error}")),
        }
        Ok(())
    }

    /// Atomic cross-process boundary: once created, even an uncertain/partial
    /// marker forbids any second spawn. Persist before sending the spawn request.
    pub(crate) fn begin_remote_spawn(
        &self,
        root: &std::path::Path,
        record: &mut super::pty_layout::RemoteShellRecord,
    ) -> Result<bool, String> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = remote_intent_path(root, record)?.with_extension("spawn");
        let mut marker = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                record.mark_spawn_attempted();
                return Ok(false);
            }
            Err(error) => return Err(format!("cannot persist spawn attempt: {error}")),
        };
        record.mark_spawn_attempted();
        self.update_remote_shell(record.clone());
        marker
            .write_all(record.target.session.as_str().as_bytes())
            .and_then(|()| marker.sync_all())
            .map_err(|e| format!("spawn attempt uncertain: {e}"))?;
        std::fs::File::open(root)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| format!("spawn attempt durability uncertain: {e}"))?;
        Ok(true)
    }

    /// Reserves and returns the receipt `reference` names, or `None` when no
    /// readable receipt matches. A handle is found by scanning the receipts:
    /// one that cannot be read (corrupt, unsafe, or an older schema) is skipped
    /// with a warning and fails only its own match. The caller then treats that
    /// tab like one with no receipt and looks for it in its machine's layout,
    /// which attaches to an existing tab and never creates one.
    pub(crate) fn restore_remote_shell(
        &self,
        root: &std::path::Path,
        owner: u64,
        team: &str,
        machine: &str,
        reference: &super::pty_layout::RemoteTabReference,
    ) -> Result<Option<super::pty_layout::RemoteShellRecord>, String> {
        if let super::pty_layout::RemoteTabReference::RequestId(id) = reference {
            return self.consumed_remote_shell(root, owner, team, machine, *id);
        }
        let super::pty_layout::RemoteTabReference::Handle(handle) = reference else {
            unreachable!()
        };
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        let mut found = None;
        for (count, entry) in entries.enumerate() {
            if count >= 100_000 {
                return Err("remote intent directory exceeds bound".into());
            }
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let record = match read_remote_intent(&path) {
                Ok(Some(record)) => record,
                Ok(None) => continue,
                Err(error) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    log::warn!(target: "remote", "skip unreadable receipt {name}: {error}");
                    continue;
                }
            };
            if record.owner_id == owner
                && record.team_key == team
                && record.machine_id == machine
                && record.handle == *handle
            {
                if found.is_some() {
                    return Err("ambiguous remote intent handle".into());
                }
                found = Some(record.request_id);
            }
        }
        found
            .map(|id| self.consumed_remote_shell(root, owner, team, machine, id))
            .transpose()
            .map(Option::flatten)
    }

    /// After an app restart: each tab `live` holds comes back into the registry
    /// from its own receipt (found by its team and request), when the receipt is
    /// `owner`'s for the same handle and session in a terminal server lifetime
    /// `current` accepts. The receipt is checked before anything is reserved; a
    /// restored row is marked live, as its machine holds it now, and names the
    /// machine's admission `now` (it moves on every reconnect). One already in
    /// the registry is left as it is, and a receipt that cannot be read keeps
    /// only its own tab out. Returns how many came back.
    pub(crate) fn restore_remote_shells(
        &self,
        root: &std::path::Path,
        owner: u64,
        machine: &str,
        live: &[super::pty_layout::LiveRemoteTab],
        now: &super::machines::RuntimeMemberTarget,
        current: impl Fn(&super::machines::RuntimeMemberTarget) -> bool,
    ) -> usize {
        let mut restored = 0;
        for tab in live {
            let receipt = intent_path(root, &tab.team, machine, tab.request_id)
                .and_then(|path| read_remote_intent(&path));
            let record = match receipt {
                Ok(Some(record)) => record,
                Ok(None) => continue,
                Err(error) => {
                    log::debug!(target: "remote", "restore {}: {error}", tab.handle);
                    continue;
                }
            };
            if record.owner_id != owner
                || record.team_key != tab.team
                || record.machine_id != machine
                || record.handle != tab.handle
                || record.target.session.as_str() != tab.session
                || !current(&record.target.runtime)
                || self.lock().by_handle.contains_key(&tab.handle)
            {
                continue;
            }
            // Reserved here, under one lock with the check: a row another path
            // holds by now is its own, and is left as it is.
            let mut record = record;
            if let Err(error) = self.refresh_remote_spawn_phase(root, &mut record) {
                log::debug!(target: "remote", "restore {}: {error}", tab.handle);
                continue;
            }
            record.status = "alreadyLive";
            // Same lifetime, reached through this connection's admission.
            record.target.runtime = now.clone();
            if self.reserve_remote_shell(record).is_ok() {
                restored += 1;
            }
        }
        restored
    }

    /// `machine` reconnected: each of its tabs already in the registry that its
    /// live layout still holds, in the same terminal-server lifetime, is reached
    /// through the admission `now`. The admission revision grows on every
    /// reconnect, and a row only ever moves to a newer one, so a slower
    /// reconnect's call never takes it back.
    pub(crate) fn readmit_remote_shells(
        &self,
        owner: u64,
        machine: &str,
        live: &[super::pty_layout::LiveRemoteTab],
        now: &super::machines::RuntimeMemberTarget,
        current: impl Fn(&super::machines::RuntimeMemberTarget) -> bool,
    ) -> usize {
        let mut inner = self.lock();
        let mut readmitted = 0;
        for tab in live {
            let Some(record) = inner
                .by_handle
                .get_mut(&tab.handle)
                .and_then(|entry| entry.remote_shell.as_mut())
            else {
                continue;
            };
            if record.owner_id == owner
                && record.team_key == tab.team
                && record.machine_id == machine
                && record.request_id == tab.request_id
                && record.target.session.as_str() == tab.session
                && now.admission_revision > record.target.runtime.admission_revision
                && current(&record.target.runtime)
            {
                record.target.runtime = now.clone();
                readmitted += 1;
            }
        }
        readmitted
    }

    /// Read consumption before consulting runtime availability. Absence alone
    /// permits a new reservation; unreadable evidence never permits replay.
    pub(crate) fn consumed_remote_shell(
        &self,
        root: &std::path::Path,
        owner: u64,
        team: &str,
        machine: &str,
        request: uuid::Uuid,
    ) -> Result<Option<super::pty_layout::RemoteShellRecord>, String> {
        let path = intent_path(root, team, machine, request)?;
        let Some(mut existing) = read_remote_intent(&path)? else {
            return Ok(None);
        };
        self.refresh_remote_spawn_phase(root, &mut existing)?;
        if existing.owner_id != owner
            || existing.team_key != team
            || existing.machine_id != machine
            || existing.request_id != request
        {
            return Err("consumed remote intent identity mismatch".into());
        }
        let mut retained = self
            .reserve_remote_shell(existing.clone())
            .err()
            .map(|record| *record)
            .unwrap_or(existing);
        self.refresh_remote_spawn_phase(root, &mut retained)?;
        Ok(Some(retained))
    }

    /// Consume the request on disk before any dispatch. An incomplete/corrupt
    /// receipt refuses replay; a restart never makes a consumed UUID available.
    pub(crate) fn reserve_remote_shell_durable(
        &self,
        root: &std::path::Path,
        record: super::pty_layout::RemoteShellRecord,
    ) -> Result<RemoteShellReservation, String> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = remote_intent_path(root, &record)?;
        std::fs::create_dir_all(root).map_err(|e| format!("cannot reserve remote intent: {e}"))?;
        let bytes = serde_json::to_vec(&record).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_REMOTE_INTENT_BYTES {
            return Err("saved-Agent intent exceeds durable receipt limit".into());
        }
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(&bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|e| format!("remote intent consumption is uncertain: {e}"))?;
                std::fs::File::open(root)
                    .and_then(|dir| dir.sync_all())
                    .map_err(|e| format!("remote intent durability is uncertain: {e}"))?;
                match self.reserve_remote_shell(record) {
                    Ok(()) => Ok(RemoteShellReservation::Fresh),
                    Err(existing) => Ok(RemoteShellReservation::Existing(existing)),
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let mut existing =
                    read_remote_intent(&path)?.ok_or("consumed remote intent disappeared")?;
                self.refresh_remote_spawn_phase(root, &mut existing)?;
                if existing.owner_id != record.owner_id
                    || existing.team_key != record.team_key
                    || existing.machine_id != record.machine_id
                    || existing.request_id != record.request_id
                {
                    return Err("consumed remote intent identity mismatch".into());
                }
                let retained = self
                    .reserve_remote_shell(existing.clone())
                    .err()
                    .unwrap_or_else(|| Box::new(existing));
                Ok(RemoteShellReservation::Existing(retained))
            }
            Err(error) => Err(format!("cannot reserve remote intent: {error}")),
        }
    }
}

mod local_restore;
#[cfg(test)]
pub(crate) use local_restore::RestoredLaunch;
pub(crate) use local_restore::{
    is_launch_tag, spawn_local_launch_restore, tag_bound_launch, TAG_LAUNCH_AGENT_REVISION,
    TAG_LAUNCH_HANDLE, TAG_LAUNCH_PRINCIPAL, TAG_LAUNCH_ROUTINE, TAG_LAUNCH_STORY,
};

mod persistence;
pub(crate) use persistence::registry_path as persistence_registry_path;

pub(crate) mod deadline;
pub(crate) use deadline::{run_deadline_sweep, run_startup_sweep};

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    async fn headless_task_exit_case(code: i32, signal: Option<&str>) {
        use crate::session_usage::{task::TaskRecord, UsageStore};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let state = DashboardLaunchState::new(4);
        state.set_task_store_path_for_test(path.clone());
        assert!(state.begin("headless-task").unwrap());
        state.bind("headless-task", "headless-task-session", None).unwrap();
        state.mark_headless(
            "headless-task",
            std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        );
        UsageStore::open(&path).unwrap()
            .insert_task_if_absent(&TaskRecord::new("headless-task-session", "Finish this task"))
            .unwrap();
        if let Some(status) = signal {
            state.begin_signal_outcome("headless-task-session");
            crate::session_usage::task_outcome::close_for_session_async(
                state.task_store_path(), "headless-task-session", status, None,
            ).await;
        }
        let before_exit = UsageStore::open(&path).unwrap()
            .read_task("headless-task-session").unwrap().unwrap();
        // Use an actual child exit, then the classifier and close decision completion uses.
        let out = tokio::task::spawn_blocking(move || {
            std::process::Command::new("/bin/sh")
                .args(["-c", &format!("exit {code}")]).output().unwrap()
        }).await.unwrap();
        let capture = Ok(CaptureOutput {
            status: Some(out.status), stdout: out.stdout, stderr: out.stderr,
            timed_out: false, cancelled: false,
        });
        let (status, _) = headless_run_outcome(None, &capture);
        state.mark_headless_exited("headless-task-session");
        state.release_for_session("headless-task-session");
        assert_eq!(
            close_headless_task_on_exit(&state, "headless-task-session", status).await,
            signal.is_none(),
        );
        // Reopen after the async writer completes: the result is durable and went through tamper.
        let task = UsageStore::open(&path).unwrap()
            .read_task("headless-task-session").unwrap().unwrap();
        let raw = signal.unwrap_or(status);
        assert_eq!(task.status, crate::session_usage::task_outcome::status_for(raw));
        assert_eq!(task.raw_status.as_deref(), Some(raw));
        assert!(task.closed_at_ms.is_some());
        assert!(task.unavailable["tamper"].contains("no tamper_globs"));
        if signal.is_some() {
            assert_eq!(task.closed_at_ms, before_exit.closed_at_ms);
        }
        // Even a late duplicate with a different verdict cannot move the first close.
        crate::session_usage::task_outcome::close_for_session_async(
            state.task_store_path(), "headless-task-session", "failed", None,
        ).await;
        assert_eq!(UsageStore::open(&path).unwrap()
            .read_task("headless-task-session").unwrap().unwrap(), task);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unsignaled_clean_headless_exit_closes_its_task_through_the_funnel() {
        headless_task_exit_case(0, None).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unsignaled_failed_headless_exit_closes_its_task_through_the_funnel() {
        headless_task_exit_case(7, None).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_earlier_signal_keeps_its_task_verdict_when_the_headless_process_exits() {
        headless_task_exit_case(0, Some("needs-attention")).await;
    }

    async fn task_capture_completion_order(signal: bool, capture_first: bool) {
        use crate::session_usage::{task::TaskRecord, UsageStore};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let state = DashboardLaunchState::new(4);
        state.set_task_store_path_for_test(path.clone());
        let session = "fast-session";
        // This registers synchronously before write_launch_record detaches its capture.
        let captured = state.begin_task_capture(session, session);
        let capture = || {
            UsageStore::open(&path).unwrap()
                .insert_task_if_absent(&TaskRecord::new(session, "A short task"))
                .unwrap();
        };
        let mut captured = Some(captured);
        if capture_first {
            capture();
            drop(captured.take());
        }
        let close = async {
            if signal {
                close_task_after_capture(Some(&state), session, "needs-attention").await;
            } else {
                assert!(close_headless_task_on_exit(&state, session, "success").await);
            }
        };
        tokio::pin!(close);
        if !capture_first {
            assert!(tokio::time::timeout(std::time::Duration::from_millis(100), &mut close)
                .await.is_err(), "terminal persistence must wait for launch capture");
            capture();
            drop(captured.take());
        }
        close.await;
        let task = UsageStore::open(&path).unwrap().read_task(session).unwrap().unwrap();
        assert_eq!(task.raw_status.as_deref(), Some(if signal { "needs-attention" } else { "success" }));
        assert!(task.closed_at_ms.is_some());
        assert!(task.unavailable.contains_key("tamper"));
        // A duplicate ending and a later duplicate launch capture cannot reopen the task.
        close_task_after_capture(Some(&state), session, "failed").await;
        capture();
        assert_eq!(UsageStore::open(&path).unwrap().read_task(session).unwrap().unwrap(), task);
    }

    #[tokio::test]
    async fn a_fast_signal_waits_for_its_launch_task_capture() {
        task_capture_completion_order(true, false).await;
        task_capture_completion_order(true, true).await;
    }

    #[tokio::test]
    async fn a_fast_unsignaled_exit_waits_for_its_launch_task_capture() {
        task_capture_completion_order(false, false).await;
        task_capture_completion_order(false, true).await;
    }

    #[tokio::test]
    async fn a_signal_during_launch_capture_still_suppresses_the_headless_exit() {
        use crate::session_usage::{task::TaskRecord, UsageStore};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let state = DashboardLaunchState::new(4);
        state.set_task_store_path_for_test(path.clone());
        assert!(state.begin("fast-headless").unwrap());
        state.bind("fast-headless", "fast-headless-session", None).unwrap();
        state.mark_headless(
            "fast-headless",
            std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        );
        let captured = state.begin_task_capture("fast-headless-session", "fast-headless-session");
        let exit = close_headless_task_on_exit(&state, "fast-headless-session", "success");
        tokio::pin!(exit);
        assert!(tokio::time::timeout(std::time::Duration::from_millis(100), &mut exit)
            .await.is_err());
        // The signal reaches the registry while the exit is waiting for the same capture.
        state.begin_signal_outcome("fast-headless-session");
        UsageStore::open(&path).unwrap()
            .insert_task_if_absent(&TaskRecord::new("fast-headless-session", "A fast task"))
            .unwrap();
        drop(captured);
        assert!(!exit.await, "attestation must be checked after capture completes");
        close_task_after_capture(Some(&state), "fast-headless-session", "needs-attention").await;
        assert_eq!(UsageStore::open(&path).unwrap().read_task("fast-headless-session")
            .unwrap().unwrap().raw_status.as_deref(), Some("needs-attention"));
    }

    #[tokio::test(start_paused = true)]
    async fn the_real_launch_writer_completes_an_early_terminal_row_once_in_either_order() {
        use crate::session_usage::{task::EARLY_CLOSE, UsageStore};
        for routine in [false, true] {
            for signal in [false, true] {
                for capture_first in [false, true] {
                    let dir = tempfile::tempdir().unwrap();
                    let path = dir.path().join("usage.sqlite");
                    let (state, _) = bound_launch(SessionOwner {
                        principal: None, org: "acme".into(), team: Some("dev".into()),
                    });
                    state.set_task_store_path_for_test(path.clone());
                    if routine {
                        state.stamp_provenance("SV-1", RunProvenance {
                            run_id: "routine-run-early".into(),
                            routine: Some(RoutineBrief { name: "Daily check".into(), task: "Check the count".into() }),
                            ..Default::default()
                        });
                    }
                    let id = if routine { "routine-run-early" } else { "sess-1" };
                    let (release, gate) = tokio::sync::watch::channel(());
                    *state.task_capture_gate.lock().unwrap() = Some(gate);
                    write_launch_record(&state, "sess-1", "SV-1", task_facts(Some("Task-Type: feature\nBuild task records")));
                    let mut captured = state.task_captures.lock().unwrap().get("sess-1").unwrap().0.clone();
                    let mut release = Some(release);
                    if capture_first {
                        drop(release.take());
                        let _ = captured.changed().await;
                    }
                    let status = if signal { "needs-attention" } else { "success" };
                    if signal {
                        close_task_after_capture(Some(&state), "sess-1", status).await;
                    } else {
                        assert!(close_headless_task_on_exit(&state, "sess-1", status).await);
                    }
                    let first = UsageStore::open(&path).unwrap().read_task(id).unwrap().unwrap();
                    if !capture_first {
                        assert_eq!(first.created_by.as_deref(), Some(EARLY_CLOSE));
                        assert_eq!(first.goal, "Task capture pending");
                        assert_eq!(first.task_type, "unclassified");
                        assert!(first.unavailable["tamper"].contains("pending"));
                        assert!(first.org.is_none(), "the record has not arrived; scope is not invented");
                        close_task_after_capture(Some(&state), "sess-1", "failed").await;
                        drop(release.take());
                        let _ = captured.changed().await;
                    }
                    let complete = UsageStore::open(&path).unwrap().read_task(id).unwrap().unwrap();
                    assert_eq!(complete.raw_status.as_deref(), Some(status));
                    assert_eq!(complete.closed_at_ms, first.closed_at_ms);
                    assert_ne!(complete.created_by.as_deref(), Some(EARLY_CLOSE));
                    assert_eq!(complete.org.as_deref(), Some("acme"));
                    assert!(!complete.unavailable["tamper"].contains("pending"));
                    assert_eq!(complete.goal, if routine { "Daily check: Check the count" } else { "Build task records" });
                    if routine {
                        assert!(UsageStore::open(&path).unwrap().read_task("sess-1").unwrap().is_none());
                    }
                    // A duplicate real launch write must not rewrite the captured snapshot or close.
                    *state.task_capture_gate.lock().unwrap() = None;
                    write_launch_record(&state, "sess-1", "SV-1", task_facts(Some("Task-Type: ops\nA changed brief")));
                    let mut duplicate = state.task_captures.lock().unwrap().get("sess-1").unwrap().0.clone();
                    let _ = duplicate.changed().await;
                    assert_eq!(UsageStore::open(&path).unwrap().read_task(id).unwrap().unwrap(), complete);
                }
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_stuck_launch_capture_cannot_hold_a_terminal_close_forever() {
        use crate::session_usage::{task::TaskRecord, UsageStore};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let state = DashboardLaunchState::new(4);
        state.set_task_store_path_for_test(path.clone());
        let _still_capturing = state.begin_task_capture("stuck-session", "stuck-session");
        UsageStore::open(&path).unwrap()
            .insert_task_if_absent(&TaskRecord::new("stuck-session", "Captured task"))
            .unwrap();
        close_task_after_capture(Some(&state), "stuck-session", "success").await;
        assert_eq!(UsageStore::open(&path).unwrap().read_task("stuck-session")
            .unwrap().unwrap().status, "done");
    }

    #[tokio::test]
    async fn a_headless_launch_without_an_org_attributes_its_export() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let token = "headless-session";
        let cli = codesense_pty_core::agent_cli::AgentCli::Codex;
        let mut env = Vec::new();
        let mut args = Vec::new();
        headless_usage_owner(None, "personal/worker", "routine:daily@run", "/work")
            .prepare_export(
                Ok(path.clone()),
                token,
                codesense_pty_core::agent_cli::descriptor(cli)
                    .telemetry
                    .as_ref(),
                Some(4567),
                &mut env,
                &mut args,
            )
            .await;
        assert!(args.iter().any(|value| value.contains(token)));
        crate::session_usage::tests::assert_launch_attribution(
            &path,
            token,
            cli,
            "personal/worker",
            "routine:daily@run",
            "",
            None,
        );
    }

    #[test]
    fn a_remote_agent_runs_while_its_tab_does() {
        let agents = vec![
            ("remote-tab-a".to_string(), "box".to_string()),
            ("remote-tab-b".to_string(), "box".to_string()),
            ("remote-tab-c".to_string(), "box".to_string()),
            ("remote-tab-d".to_string(), "offline".to_string()),
        ];
        let tabs = [
            RemoteTabSeen {
                handle: Some("remote-tab-a"),
                machine_id: "box",
                exited: false,
            },
            RemoteTabSeen {
                handle: Some("remote-tab-b"),
                machine_id: "box",
                exited: true,
            },
            RemoteTabSeen {
                handle: None,
                machine_id: "box",
                exited: false,
            },
        ];
        let alive = remote_agent_liveness(&agents, &tabs, &["box".to_string()]);
        assert!(alive["remote-tab-a"], "its tab runs");
        assert!(!alive["remote-tab-b"], "its tab exited");
        assert!(
            !alive["remote-tab-c"],
            "the machine's layout no longer holds it"
        );
        assert!(alive["remote-tab-d"], "its machine can't be read now");
    }

    #[tokio::test]
    async fn raw_snapshot_opt_out_never_invokes_the_copy() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let result = capture_raw_snapshot_if_enabled(false, || async {
            calls.fetch_add(1, Ordering::Relaxed);
            Some("should not be copied".to_string())
        })
        .await;
        assert_eq!(result, None);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    fn raw_enabled_state(session_id: &str) -> DashboardLaunchState {
        let state = DashboardLaunchState::new(1);
        assert!(state.begin("story").unwrap());
        state.bind("story", session_id, None).unwrap();
        state.stamp_provenance(
            "story",
            RunProvenance {
                run_id: format!("run-{session_id}"),
                story_id: "story".into(),
                ..Default::default()
            },
        );
        state.enable_raw_terminal_capture_for_fresh_launch("story");
        state
    }

    /// A run's completion stores its sessions' conversations (the one capture path, keyed by
    /// session) instead of writing a per-run turns file, and needs no raw-terminal consent.
    #[tokio::test]
    async fn a_runs_completion_requests_its_sessions_conversation_not_a_turns_file() {
        let state = DashboardLaunchState::new(1);
        assert!(state.begin("story").unwrap());
        state.bind("story", "native", None).unwrap();
        state.stamp_provenance(
            "story",
            RunProvenance {
                run_id: "run-native-completion".into(),
                story_id: "story".into(),
                ..Default::default()
            },
        );
        let dir = tempfile::tempdir().unwrap();
        crate::session_conversation::take_noted();
        assert!(
            state
                .record_session_outcome_with(
                    "native",
                    SessionOutcomeInput {
                        status: "success".into(),
                        summary: None,
                        exit_code: None,
                        source: crate::commands::sprint_run::run_store::OutcomeSource::Signal,
                        label: "test.native"
                    },
                    || async { panic!("raw terminal was not opted in") },
                    CapturedOutcomeDestination::TestArtifactDir(dir.path().into()),
                )
                .await
        );
        assert_eq!(
            crate::session_conversation::take_noted(),
            [("native".to_owned(), crate::session_conversation::Trigger::End)]
        );
        assert!(!dir.path().join("run-native-completion.turns.json").exists());
        assert!(!dir.path().join("run-native-completion.raw.log").exists());
    }

    #[tokio::test]
    async fn invalid_status_does_not_consume_raw_capture_entitlement() {
        let state = raw_enabled_state("session-invalid");
        let dir = tempfile::tempdir().unwrap();
        state
            .record_session_outcome_with(
                "session-invalid",
                SessionOutcomeInput {
                    status: "not-a-status".into(),
                    summary: None,
                    exit_code: None,
                    source: crate::commands::sprint_run::run_store::OutcomeSource::Signal,
                    label: "test.invalid",
                },
                || async { None },
                CapturedOutcomeDestination::TestArtifactDir(dir.path().to_path_buf()),
            )
            .await;
        let context = state
            .take_run_context_for_session("session-invalid")
            .unwrap();
        assert!(context.capture_raw_terminal);
    }

    #[test]
    fn release_cleans_entitlement_so_a_reused_session_id_can_capture() {
        let state = raw_enabled_state("session-reused");
        assert!(
            state
                .take_run_context_for_session("session-reused")
                .unwrap()
                .capture_raw_terminal
        );
        state.release_for_session("session-reused").unwrap();

        assert!(state.begin("story-2").unwrap());
        state.bind("story-2", "session-reused", None).unwrap();
        state.stamp_provenance(
            "story-2",
            RunProvenance {
                run_id: "run-reused-2".into(),
                story_id: "story-2".into(),
                ..Default::default()
            },
        );
        state.enable_raw_terminal_capture_for_fresh_launch("story-2");
        assert!(
            state
                .take_run_context_for_session("session-reused")
                .unwrap()
                .capture_raw_terminal
        );
    }

    #[tokio::test]
    async fn entitled_completion_captures_once_and_duplicate_cannot_recapture() {
        let state = raw_enabled_state("session-once");
        let first = state.take_run_context_for_session("session-once").unwrap();
        let duplicate = state.take_run_context_for_session("session-once").unwrap();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        for enabled in [
            first.should_capture_raw_terminal(),
            duplicate.should_capture_raw_terminal(),
        ] {
            let _ = capture_raw_snapshot_if_enabled(enabled, || async {
                calls.fetch_add(1, Ordering::Relaxed);
                Some("snapshot".into())
            })
            .await;
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn entitled_completion_writes_discoverable_raw_artifact_exactly_once() {
        let state = raw_enabled_state("session-artifact");
        let dir = tempfile::tempdir().unwrap();
        for snapshot in ["first entitled snapshot", "duplicate must not overwrite"] {
            state
                .record_session_outcome_with(
                    "session-artifact",
                    SessionOutcomeInput {
                        status: "success".into(),
                        summary: None,
                        exit_code: Some(0),
                        source: crate::commands::sprint_run::run_store::OutcomeSource::Signal,
                        label: "test.raw.destination",
                    },
                    || async { Some(snapshot.into()) },
                    CapturedOutcomeDestination::TestArtifactDir(dir.path().to_path_buf()),
                )
                .await;
        }

        assert_eq!(
            std::fs::read_to_string(dir.path().join("run-session-artifact.typescript")).unwrap(),
            "first entitled snapshot"
        );
    }

    #[tokio::test]
    async fn every_accepted_status_can_reach_capture_but_unprovenanced_sessions_cannot() {
        for (index, wire) in ["success", "failed", "needs-attention"]
            .into_iter()
            .enumerate()
        {
            let session_id = format!("session-status-{index}");
            let state = raw_enabled_state(&session_id);
            let (_, context) = state
                .take_run_context_for_outcome(&session_id, wire)
                .unwrap();
            let calls = std::sync::atomic::AtomicUsize::new(0);
            let snapshot =
                capture_raw_snapshot_if_enabled(context.should_capture_raw_terminal(), || async {
                    calls.fetch_add(1, Ordering::Relaxed);
                    Some(wire.to_string())
                })
                .await;
            assert_eq!(snapshot.as_deref(), Some(wire));
            assert_eq!(calls.load(Ordering::Relaxed), 1);
        }

        let bare = DashboardLaunchState::new(1);
        assert!(bare.begin("bare").unwrap());
        bare.bind("bare", "session-bare", None).unwrap();
        assert!(bare
            .take_run_context_for_outcome("session-bare", "success")
            .is_none());
        assert!(bare
            .take_run_context_for_outcome("session-unknown", "success")
            .is_none());
    }

    #[tokio::test]
    async fn absent_snapshot_is_nonfatal_and_does_not_fabricate_capture() {
        let result = capture_raw_snapshot_if_enabled(true, || async { None }).await;
        assert_eq!(result, None);
    }

    #[test]
    fn raw_capture_entitlement_does_not_change_the_primary_artifact_reference() {
        let context = raw_enabled_state("session-primary-ref")
            .take_run_context_for_session("session-primary-ref")
            .unwrap();
        let event = build_run_outcome(
            context,
            crate::dashboard_stream::SessionStatus::Success,
            None,
            Some(0),
            crate::commands::sprint_run::run_store::OutcomeSource::Signal,
            "2026-09-01T00:00:00Z".into(),
            Some("run-artifacts/run-primary.log".into()),
        );
        assert_eq!(
            event.artifact_ref.as_deref(),
            Some("run-artifacts/run-primary.log")
        );
    }

    #[test]
    fn session_state_changed_event_name_matches_the_frontend_contract() {
        // SW-AM-8: the FE `SESSION_STATE_CHANGED_EVENT` (session-events.ts) and both
        // roster subscribers key on this EXACT string — a rename on either side must
        // fail loudly here rather than silently stop the panel going live.
        assert_eq!(SESSION_STATE_CHANGED, "session-state-changed");
    }

    #[test]
    fn retain_alive_keeps_only_probed_alive_in_input_order() {
        let alive = crate::commands::pty_backend::Liveness::Known(
            ["b", "d"].iter().map(|s| s.to_string()).collect(),
        );
        let got = retain_alive(vec!["a".into(), "b".into(), "c".into(), "d".into()], &alive);
        // Only the alive subset, and in the caller's original order (not the set's).
        assert_eq!(got, vec!["b".to_string(), "d".to_string()]);
    }

    /// SW-RT-6d (AC-6): with a terminal host that cannot be asked, every probed
    /// session reads alive — never dead — so no still-running agent is healed
    /// (evicted and respawned) on a guess.
    #[test]
    fn retain_alive_keeps_every_session_while_liveness_is_unknown() {
        let got = retain_alive(
            vec!["a".into(), "b".into()],
            &crate::commands::pty_backend::Liveness::Unknown,
        );
        assert_eq!(got, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn stamp_provenance_records_the_run_link_and_snapshots_it() {
        // AC-4: a dispatched worker carries { runId, storyId, automationId? }, and the
        // snapshot the panel/monitor read surfaces it — the SE-7 roll-up join key.
        let state = DashboardLaunchState::new(4);
        assert!(state.begin("SW-SR-3").unwrap());
        state.bind("SW-SR-3", "shell-1", Some("codesense")).unwrap();
        // No provenance until stamped (a hand-launched session stays None).
        assert!(state.snapshot().iter().all(|s| s.provenance.is_none()));

        state.stamp_provenance(
            "SW-SR-3",
            RunProvenance {
                run_id: "run-1".into(),
                story_id: "SW-SR-3".into(),
                automation_id: None,
                ..Default::default()
            },
        );
        let after = state.snapshot();
        let row = after.iter().find(|s| s.handle == "SW-SR-3").unwrap();
        let prov = row.provenance.as_ref().expect("provenance stamped");
        assert_eq!(prov.run_id, "run-1");
        assert_eq!(prov.story_id, "SW-SR-3");
        assert_eq!(prov.automation_id, None);
    }

    #[test]
    fn idempotent_focus_cannot_replace_fresh_launch_provenance() {
        let state = DashboardLaunchState::new(1);
        let original = RunProvenance {
            run_id: "run-original".into(),
            story_id: "SW-OR-17".into(),
            ..Default::default()
        };
        assert!(state
            .begin_with_provenance("SW-OR-17", Some(original), LaunchRecord::default())
            .unwrap());
        assert!(!state
            .begin_with_provenance(
                "SW-OR-17",
                Some(RunProvenance {
                    run_id: "run-colliding".into(),
                    story_id: "SW-OR-17".into(),
                    ..Default::default()
                }),
                LaunchRecord::default(),
            )
            .unwrap());

        let row = state.snapshot().into_iter().next().unwrap();
        assert_eq!(row.provenance.unwrap().run_id, "run-original");
    }

    fn launch_record(name: &str, launched_by: Option<&str>, prompt: &str) -> LaunchRecord {
        LaunchRecord {
            meta: LaunchMeta {
                display_name: Some(name.into()),
                launched_by: launched_by.map(str::to_string),
                ..LaunchMeta::default()
            },
            started_with: Some(prompt.into()),
            max_runtime_ms: None,
        }
    }

    /// A tab a person opened on a machine records no launch team: its inbox is its
    /// workspace's team, so mail sent to it and announced by its hooks share one store.
    #[test]
    fn a_hand_opened_remote_tab_keeps_its_workspace_team_inbox() {
        let record: super::super::pty_layout::RemoteShellRecord =
            serde_json::from_value(serde_json::json!({
                "handle": "remote-tab-x", "owner_id": 41, "request_id": uuid::Uuid::new_v4(),
                "machine_id": "box", "team_key": "acme:design", "workspace_id": "w",
                "workspace": "acme:design|w", "cwd": "/home/dev", "name": null, "launch": "claude",
                "target": {"member":{"machineId":"box","slot":"remote"},"admissionRevision":"1",
                    "namespace":{"serverId":"s","epoch":"e","generation":"1"}, "session": "s1"}
            }))
            .unwrap();
        let state = DashboardLaunchState::new(4);
        state.reserve_remote_shell(record).unwrap();
        assert_eq!(
            state.launch_team("remote-tab-x"),
            launch_team_from_key("acme:design")
        );
        assert_eq!(
            state.remote_handle_for_session("s1").as_deref(),
            Some("remote-tab-x")
        );
        assert_eq!(state.launch_team("unknown"), None);
        // A later stamp never moves it: the workspace team counts as recorded.
        state
            .set_launch_team_if_absent("remote-tab-x", launch_team_from_key("acme:other").unwrap());
        assert_eq!(
            state.launch_team("remote-tab-x"),
            launch_team_from_key("acme:design")
        );
    }

    #[test]
    fn a_hand_opened_tabs_team_is_kept_from_its_first_read() {
        let state = DashboardLaunchState::new(4);
        let team = |name: &str| LaunchTeam {
            org: "acme".into(),
            team: name.into(),
        };
        assert_eq!(state.known_tab_team("shell-1"), None);
        assert_eq!(
            state.keep_tab_team("shell-1", team("design")),
            team("design")
        );
        // A later read naming another team (the tab moved) never moves its inbox.
        assert_eq!(
            state.keep_tab_team("shell-1", team("default")),
            team("design")
        );
        assert_eq!(state.known_tab_team("shell-1"), Some(Some(team("design"))));
        // A read that finds no tab stands a moment; one never undoes a kept team.
        state.tab_team_missed("shell-2");
        assert_eq!(state.known_tab_team("shell-2"), Some(None));
        state.tab_team_missed("shell-1");
        assert_eq!(state.known_tab_team("shell-1"), Some(Some(team("design"))));
    }

    /// Every launch that knows its team records it: the story launch spine passes the
    /// story's own team on the reserve, a nav-opened tab registers with the viewer's team
    /// key. The first team recorded stays: a rebind never moves a session's inbox.
    #[test]
    fn launch_team_recorded_on_reserve_and_register_first_wins() {
        let design = LaunchTeam {
            org: "acme".into(),
            team: "design".into(),
        };
        let state = DashboardLaunchState::new(4);
        // The story spine: the record carries the story's team.
        let record = LaunchRecord {
            meta: LaunchMeta {
                team: Some(design.clone()),
                ..LaunchMeta::default()
            },
            started_with: None,
            max_runtime_ms: None,
        };
        assert!(state.begin_with_provenance("SW-1", None, record).unwrap());
        state.set_launch_team_if_absent(
            "SW-1",
            LaunchTeam {
                org: "acme".into(),
                team: "default".into(),
            },
        );
        assert_eq!(state.launch_team("SW-1"), Some(design));

        // A nav-opened tab: registered, then stamped from the viewer's team key.
        state.register("/x/scratch", "sess-nav", Some("codesense"));
        assert_eq!(state.launch_team("/x/scratch"), None);
        let team = launch_team_from_key("acme:default").unwrap();
        state.set_launch_team_if_absent("/x/scratch", team.clone());
        assert_eq!(state.launch_team("/x/scratch"), Some(team));

        assert_eq!(launch_team_from_key("acme:_none"), None);
        assert_eq!(launch_team_from_key("acme"), None);
        assert_eq!(
            launch_team_from_key("acme:a:b").map(|t| t.team),
            Some("a:b".to_string())
        );
    }

    #[test]
    fn launch_meta_stamped_once_and_never_overwritten_by_already_live() {
        let state = DashboardLaunchState::new(2);
        let handle = "_worker/acme/default/sprint-1";
        assert!(state
            .begin_with_provenance(
                handle,
                None,
                launch_record("Sprint 1", Some("S1"), "/manager sprint 1")
            )
            .unwrap());
        // A second open of the same live handle is an idempotent focus: it reserves
        // nothing and rewrites neither the name, the launcher nor "Started with".
        assert!(!state
            .begin_with_provenance(handle, None, launch_record("Other", Some("S2"), "/other"))
            .unwrap());

        let snap = state.snapshot().into_iter().next().unwrap();
        assert_eq!(snap.meta.display_name.as_deref(), Some("Sprint 1"));
        assert_eq!(snap.meta.launched_by.as_deref(), Some("S1"));
        assert_eq!(snap.started_with.as_deref(), Some("/manager sprint 1"));
        let row = serde_json::to_value(session_row(snap, true)).unwrap();
        assert_eq!(row["displayName"], "Sprint 1");
        assert_eq!(row["launchedBy"], "S1");
        assert_eq!(row["startedWith"], "/manager sprint 1");
    }

    #[test]
    fn launched_by_normalized_to_handle_else_raw_id() {
        let state = DashboardLaunchState::new(4);
        assert!(state.begin("MANAGER").unwrap());
        state.bind("MANAGER", "sess-manager", Some("mcp")).unwrap();

        // A launcher whose session is bound to a handle is recorded as that handle.
        assert!(state
            .begin_with_provenance(
                "CHILD",
                None,
                launch_record("Child", Some("sess-manager"), "/x")
            )
            .unwrap());
        // A launcher with no handle (a plain tab) is recorded as its raw id.
        assert!(state
            .begin_with_provenance(
                "OTHER",
                None,
                launch_record("Other", Some("sess-tab"), "/x")
            )
            .unwrap());

        let launched_by = |handle: &str| {
            state
                .snapshot()
                .into_iter()
                .find(|s| s.handle == handle)
                .and_then(|s| s.meta.launched_by)
        };
        assert_eq!(launched_by("CHILD").as_deref(), Some("MANAGER"));
        assert_eq!(launched_by("OTHER").as_deref(), Some("sess-tab"));
        assert_eq!(launched_by("MANAGER"), None);
        assert_eq!(state.normalize_launched_by("sess-manager"), "MANAGER");
        assert_eq!(state.normalize_launched_by("MANAGER"), "MANAGER");
        assert_eq!(state.normalize_launched_by("sess-tab"), "sess-tab");
    }

    #[test]
    fn run_context_for_session_resolves_provenance_and_no_ops_a_bare_session() {
        // SW-AU-52 (AC-4): a provenanced session resolves its run identity; a bare (no-provenance)
        // session is a documented no-op (None) — so the shared outcome seam writes nothing for it.
        let state = DashboardLaunchState::new(4);
        assert!(state.begin("SW-AU-52").unwrap());
        state
            .bind("SW-AU-52", "shell-1", Some("codesense"))
            .unwrap();
        // No provenance yet → no resolvable run identity (the no-op gate).
        assert!(state.run_context_for_session("shell-1").is_none());
        // Unknown session id → None.
        assert!(state.run_context_for_session("nope").is_none());
        // Stamped → resolves to { run_id, story_id }.
        state.stamp_provenance(
            "SW-AU-52",
            RunProvenance {
                run_id: "run-1".into(),
                story_id: "SW-AU-52".into(),
                automation_id: Some("review".into()),
                trigger_source: Some("headless".into()),
                ..Default::default()
            },
        );
        let ctx = state
            .run_context_for_session("shell-1")
            .expect("resolves once provenance is stamped");
        assert_eq!(ctx.run_id, "run-1");
        assert_eq!(ctx.story_id, "SW-AU-52");
        assert_eq!(ctx.automation_id.as_deref(), Some("review"));
        assert_eq!(ctx.trigger_source.as_deref(), Some("headless"));
        state.stamp_provenance(
            "SW-AU-52",
            RunProvenance {
                run_id: "run-1".into(),
                story_id: "SW-AU-52".into(),
                automation_id: Some(" ".into()),
                trigger_source: Some("\t".into()),
                ..Default::default()
            },
        );
        let ctx = state.run_context_for_session("shell-1").unwrap();
        assert_eq!(ctx.automation_id, None);
        assert_eq!(ctx.trigger_source, None);
        // Defense-in-depth: an empty run_id cannot key a durable outcome → no-op (guards a
        // cross-run collision under `("", ...)`), same as absent provenance.
        state.stamp_provenance(
            "SW-AU-52",
            RunProvenance {
                run_id: String::new(),
                story_id: "SW-AU-52".into(),
                ..Default::default()
            },
        );
        assert!(state.run_context_for_session("shell-1").is_none());
    }

    #[test]
    fn outcome_builder_persists_captured_provenance() {
        let work_product = crate::commands::sprint_run::run_store::RunWorkProductProvenance {
            source_repo: crate::domain::SourceRepoSlug::new("acme", "app").unwrap(),
            branch: "feat/run-1".into(),
            credential_ref: Some(crate::domain::CredentialRef {
                provider: "github".into(),
                account: "work".into(),
            }),
        };
        let event = build_run_outcome(
            RunOutcomeContext {
                run_id: "run-1".into(),
                story_id: "SW-AU-55".into(),
                automation_id: Some("review".into()),
                trigger_source: Some("headless".into()),
                workspace: None,
                scheduled_at: None,
                fired_at: None,
                started_at_ms: None,
                interactive_capture: None,
                work_product: Some(work_product.clone()),
                worktree_dir: None,
                capture_raw_terminal: false,
                routine_key: None,
                org: None,
                team: None,
                session_ids: Vec::new(),
                result_signal_only: false,
            },
            crate::dashboard_stream::SessionStatus::Success,
            Some("done".into()),
            Some(0),
            crate::commands::sprint_run::run_store::OutcomeSource::ProcessExit,
            "2026-08-30T00:00:00Z".into(),
            None,
        );
        assert_eq!(event.automation_id.as_deref(), Some("review"));
        assert_eq!(event.trigger_source.as_deref(), Some("headless"));
        assert_eq!(event.work_product, Some(work_product));
    }

    #[test]
    fn signal_outcome_context_survives_registry_teardown() {
        let state = DashboardLaunchState::new(1);
        assert!(state.begin("SW-AU-54").unwrap());
        state
            .bind("SW-AU-54", "headless-1", Some("codesense"))
            .unwrap();
        state.stamp_provenance(
            "SW-AU-54",
            RunProvenance {
                run_id: "run-54".into(),
                story_id: "SW-AU-54".into(),
                ..Default::default()
            },
        );
        state.mark_headless(
            "SW-AU-54",
            Arc::new(AtomicU32::new(0)),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
        );

        let (pending, context) = state.begin_signal_outcome("headless-1");
        assert!(pending);
        let context = context.expect("provenance captured atomically with attestation");
        state.release_for_session("headless-1");

        assert!(state.run_context_for_session("headless-1").is_none());
        assert_eq!(context.story_id, "SW-AU-54");
        assert_eq!(context.run_id, "run-54");
    }

    #[test]
    fn run_context_for_session_captures_the_stamped_worktree_dir_before_reap() {
        // SW-AU-96B: the outcome ctx carries the run's managed worktree dir, resolved from the live
        // `LaunchEntry` (write-before-reap) — so the completion path can diff the work-product branch
        // for the per-file footprint. `None` until stamped; `Some` once the launch spine stamps it via
        // `set_worktree_dir_if_absent`; a later focus never clobbers it (fill-gap).
        let state = DashboardLaunchState::new(4);
        assert!(state.begin("SW-AU-96B").unwrap());
        state
            .bind("SW-AU-96B", "shell-1", Some("codesense"))
            .unwrap();
        state.stamp_provenance(
            "SW-AU-96B",
            RunProvenance {
                run_id: "run-1".into(),
                story_id: "SW-AU-96B".into(),
                ..Default::default()
            },
        );
        // Not stamped yet → ctx carries None (the branch-entity-only degrade path).
        assert_eq!(
            state
                .run_context_for_session("shell-1")
                .unwrap()
                .worktree_dir,
            None
        );
        // Once the launch spine stamps the worktree, the ctx captures it for the completion diff.
        state.set_worktree_dir_if_absent("SW-AU-96B", std::path::PathBuf::from("/tmp/wt"));
        assert_eq!(
            state
                .run_context_for_session("shell-1")
                .unwrap()
                .worktree_dir
                .as_deref(),
            Some(std::path::Path::new("/tmp/wt"))
        );
        // Fill-gap: a second stamp never clobbers the first.
        state.set_worktree_dir_if_absent("SW-AU-96B", std::path::PathBuf::from("/tmp/other"));
        assert_eq!(
            state
                .run_context_for_session("shell-1")
                .unwrap()
                .worktree_dir
                .as_deref(),
            Some(std::path::Path::new("/tmp/wt"))
        );
    }

    #[test]
    fn per_file_footprint_target_gates_on_work_product_and_worktree() {
        // SW-AU-96B: the completion gate fires the per-file diff ONLY when BOTH a work product and the
        // managed worktree are known; either absent degrades to the branch-level entity (no capture).
        let ctx = |work_product, worktree_dir| RunOutcomeContext {
            run_id: "run-1".into(),
            story_id: "SW-AU-96B".into(),
            automation_id: None,
            trigger_source: None,
            workspace: None,
            scheduled_at: None,
            fired_at: None,
            started_at_ms: None,
            interactive_capture: None,
            work_product,
            worktree_dir,
            capture_raw_terminal: false,
            routine_key: None,
            org: None,
            team: None,
            session_ids: Vec::new(),
            result_signal_only: false,
        };
        let wp = || crate::commands::sprint_run::run_store::RunWorkProductProvenance {
            source_repo: crate::domain::SourceRepoSlug::new("acme", "app").unwrap(),
            branch: "feat/run-1".into(),
            credential_ref: None,
        };
        let wt = || PathBuf::from("/tmp/wt");

        // Both present → the capture target carries the worktree + run identity.
        assert_eq!(
            ctx(Some(wp()), Some(wt())).per_file_footprint_target(),
            Some((wt(), "SW-AU-96B".to_string(), "run-1".to_string()))
        );
        // Work product absent (a bare run) → no capture.
        assert_eq!(ctx(None, Some(wt())).per_file_footprint_target(), None);
        // Worktree unknown (reaped / external register) → degrade to branch entity, no capture.
        assert_eq!(ctx(Some(wp()), None).per_file_footprint_target(), None);
        assert_eq!(ctx(None, None).per_file_footprint_target(), None);
    }

    #[test]
    fn run_context_for_session_captures_the_stashed_interactive_descriptor() {
        // SW-OR-15: the durable outcome ctx carries the run's interactive structured-transcript
        // descriptor, resolved from the live `LaunchEntry` (write-before-reap) — so the teardown
        // capture can read the agent's own session log without an adapter re-read. `None` until stamped;
        // `Some` once the launch spine stamps it; a later focus never clobbers it (fill-gap).
        let state = DashboardLaunchState::new(4);
        assert!(state.begin("SW-OR-15").unwrap());
        state
            .bind("SW-OR-15", "shell-1", Some("codesense"))
            .unwrap();
        state.stamp_provenance(
            "SW-OR-15",
            RunProvenance {
                run_id: "run-1".into(),
                story_id: "SW-OR-15".into(),
                ..Default::default()
            },
        );
        // No interactive capability declared → ctx carries None (the neutral-spine path).
        assert!(state
            .run_context_for_session("shell-1")
            .unwrap()
            .interactive_capture
            .is_none());
        // Once the launch spine stamps the descriptor, the ctx captures it for the teardown read.
        let descriptor = InteractiveStructured {
            session_log_glob: "~/.claude/projects/{enc_cwd}/{session_id}.jsonl".into(),
            session_id_argv: Some(vec!["--session-id".into(), "{session_id}".into()]),
            format: TranscriptFormat::AnthropicSessionJsonl,
        };
        let capture = InteractiveCaptureContext {
            descriptor: descriptor.clone(),
            agent_session_id: Some("agent-id".into()),
        };
        state.set_interactive_capture_if_absent("SW-OR-15", capture.clone());
        assert_eq!(
            state
                .run_context_for_session("shell-1")
                .unwrap()
                .interactive_capture
                .as_ref(),
            Some(&capture)
        );
        // Fill-gap: a second stamp (a later focus's profile) never clobbers the live run's descriptor.
        state.set_interactive_capture_if_absent(
            "SW-OR-15",
            InteractiveCaptureContext {
                descriptor: InteractiveStructured {
                    session_log_glob: "/other/{session_id}.jsonl".into(),
                    session_id_argv: None,
                    format: TranscriptFormat::AnthropicStreamJson,
                },
                agent_session_id: Some("other-id".into()),
            },
        );
        assert_eq!(
            state
                .run_context_for_session("shell-1")
                .unwrap()
                .interactive_capture
                .as_ref(),
            Some(&capture),
            "an idempotent focus must not overwrite the stamped descriptor"
        );
    }

    #[test]
    fn headless_run_is_marked_and_its_liveness_is_registry_presence() {
        // SW-OR-10 (AC-2): a headless pseudo-session flips to KIND_HEADLESS and, having
        // no PTY, reads `Running` from registry presence — then its completion path
        // (release_for_session) drops the row entirely. This is the documented exception
        // to "alive = child liveness".
        let state = DashboardLaunchState::new(4);
        assert!(state.begin("SW-OR-10").unwrap());
        state
            .bind("SW-OR-10", "headless-1", Some("dash.md"))
            .unwrap();
        let pid = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        state.mark_headless(
            "SW-OR-10",
            pid.clone(),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        );

        let snap = state.snapshot();
        let row = snap.iter().find(|s| s.handle == "SW-OR-10").unwrap();
        assert_eq!(row.kind, KIND_HEADLESS, "flipped to the headless origin");
        assert_eq!(row.session_id.as_deref(), Some("headless-1"));
        // The list liveness rule for a headless row is "present ⟹ alive", so a bound
        // headless entry derives `Running` (never Errored/Done while in flight).
        let alive = row.kind == KIND_HEADLESS && row.session_id.is_some();
        assert!(alive, "a bound headless entry is alive by presence");
        assert_eq!(
            derive_status(alive, row.session_id.as_deref(), false),
            AgentStatus::Running,
        );

        // App-exit reachability: not-yet-spawned children (pid 0) are skipped; a published
        // pid surfaces for the backstop's `killpg` sweep. Read-only — no signal is sent.
        assert!(
            state.headless_child_pgids().is_empty(),
            "pid 0 = not spawned"
        );
        pid.store(4242, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(state.headless_child_pgids(), vec![4242]);
        // Reset to 0 BEFORE release so the release-path `killpg` never signals the fake
        // pgid 4242 (which could hit a real, unrelated process on the test machine).
        pid.store(0, std::sync::atomic::Ordering::SeqCst);

        // Completion (or any Stop/drain) drops the entry (frees the SpawnCap slot) → the
        // row disappears; the outcome then lives on the managed-session chip, not the roster.
        let released = state.release_for_session("headless-1").expect("released");
        assert_eq!(released.kind, KIND_HEADLESS);
        assert!(state.snapshot().is_empty(), "row gone after completion");
    }

    /// SW-PB-23: the required-update deadline counts every headless run, one still starting
    /// (pid 0) included, until it is released.
    #[cfg(not(cs_channel = "internal"))]
    #[test]
    fn license_headless_runs_count_a_starting_run_until_it_is_released() {
        let state = DashboardLaunchState::new(4);
        assert_eq!(state.running_headless_count(), 0);
        assert!(state.begin("SW-PB-23").unwrap());
        state.bind("SW-PB-23", "headless-23", None).unwrap();
        state.mark_headless(
            "SW-PB-23",
            std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        );
        assert_eq!(
            state.running_headless_count(),
            1,
            "a run still starting counts"
        );
        state.mark_headless_exited("headless-23");
        assert_eq!(
            state.running_headless_count(),
            0,
            "an exited run waiting on its transcript doesn't"
        );
        state.release_for_session("headless-23").expect("released");
        assert_eq!(state.running_headless_count(), 0);
    }

    #[test]
    fn snapshot_surfaces_the_headless_last_output_anchor() {
        // SW-OR-19 (AC-1/AC-4): the row seeds its "last output Ns ago" heartbeat from the
        // registry anchor `spawn_capture` stamps. The snapshot reads the SAME atomic the live
        // heartbeat event carries, so the two can't contradict. A non-headless row reads `None`.
        let state = DashboardLaunchState::new(4);
        assert!(state.begin("SW-OR-19").unwrap());
        state
            .bind("SW-OR-19", "headless-1", Some("dash.md"))
            .unwrap();
        let anchor = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        state.mark_headless(
            "SW-OR-19",
            std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            anchor.clone(),
        );
        // No output yet → the `0` seed surfaces as `None`, NOT `Some(0)` (which the FE would
        // misrender as a ~56-year-old "last output"). The row shows elapsed alone until output.
        let row = state
            .snapshot()
            .into_iter()
            .find(|s| s.handle == "SW-OR-19")
            .unwrap();
        assert_eq!(
            row.last_output_ms, None,
            "no output yet → None, never Some(0)"
        );
        // A drained chunk advances the anchor; the next snapshot reflects it live.
        anchor.store(1_724_800_000_123, std::sync::atomic::Ordering::Relaxed);
        let row = state
            .snapshot()
            .into_iter()
            .find(|s| s.handle == "SW-OR-19")
            .unwrap();
        assert_eq!(row.last_output_ms, Some(1_724_800_000_123));
    }

    #[test]
    fn has_live_headless_session_gates_only_live_headless_ids() {
        // SW-OR-14 (AC-1): the predicate the MCP liveness gate ORs with the PTY probe.
        // TRUE only for a bound KIND_HEADLESS entry; every other shape is FALSE.
        let state = DashboardLaunchState::new(4);

        // Unknown id → false (nothing bound).
        assert!(!state.has_live_headless_session("nope"));

        // A bound-but-NOT-headless entry (a dashboard PTY session: begin+bind, no
        // mark_headless) → false. The gate must not accept a PTY id via this path;
        // that is the PtyState probe's job.
        assert!(state.begin("pty-handle").unwrap());
        state.bind("pty-handle", "pty-1", Some("dash.md")).unwrap();
        assert!(
            !state.has_live_headless_session("pty-1"),
            "a non-headless bound entry is not a live headless session"
        );

        // A live headless entry → true.
        assert!(state.begin("hl-handle").unwrap());
        state.bind("hl-handle", "hl-1", Some("dash.md")).unwrap();
        state.mark_headless(
            "hl-handle",
            std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        );
        assert!(state.has_live_headless_session("hl-1"));

        state.mark_headless_exited("hl-1");
        assert!(!state.has_live_headless_session("hl-1"));
        assert_eq!(
            state.resolve("hl-handle").as_deref(),
            Some("hl-1"),
            "registry provenance remains available during post-exit persistence"
        );

        // After teardown → false (registry presence gone).
        state.release_for_session("hl-1").expect("released");
        assert!(
            !state.has_live_headless_session("hl-1"),
            "a reaped headless session is no longer live"
        );
    }

    /// SW-OR-14 (AC-3): Signal > ProcessExit, through the REAL registry chain
    /// (begin_signal_outcome → [teardown] → take_signal_attested) — the "green-but-wrong"
    /// guard for the silent-clobber risk. `run_headless_to_completion` gates BOTH its
    /// `emit_agent_done` AND its `append_run_outcome(ProcessExit)` durable write on
    /// `!take_signal_attested(id)`. The suppression record OUTLIVES entry removal, so it holds no
    /// matter which teardown path reaped the entry — the fix for the signal-then-external-reap
    /// clobber a per-entry flag would drop. The durable hazard it prevents (a later `ProcessExit`
    /// clobbering an earlier `Signal` under the plain latest-wins fold) is proven by
    /// `sprint_run::run_store::tests::latest_run_outcome_returns_latest_by_ts_seq`.
    #[test]
    fn signal_attested_survives_any_teardown_and_suppresses_the_exit() {
        fn live_headless(handle: &str, sid: &str) -> DashboardLaunchState {
            let state = DashboardLaunchState::new(4);
            assert!(state.begin(handle).unwrap());
            state.bind(handle, sid, Some("dash.md")).unwrap();
            state.mark_headless(
                handle,
                std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            );
            state
        }

        // Self-attest then NATURAL completion: the completion task's own release reaps the
        // entry, then take_ drains the record → true → suppress (agent's verdict wins).
        let state = live_headless("h1", "attested-1");
        state.begin_signal_outcome("attested-1");
        state.release_for_session("attested-1");
        assert!(
            state.take_signal_attested("attested-1"),
            "a self-attested outcome must suppress the exit classifier"
        );

        // THE FIX — self-attest then EXTERNAL reap (Stop / cap-reclaim): a foreign
        // release_for_session consumes the entry BEFORE the completion task runs. The record
        // lives OUTSIDE the entry, so it still drains true → suppress. A per-entry flag would
        // have been discarded here, letting a ProcessExit clobber the Signal outcome.
        let state = live_headless("h2", "reaped-1");
        state.begin_signal_outcome("reaped-1");
        assert!(
            state.release_for_session("reaped-1").is_some(),
            "external Stop reaps the entry first"
        );
        assert!(
            state.take_signal_attested("reaped-1"),
            "self-attest survives an external reap of the entry"
        );

        // Ran to exit WITHOUT signaling → take_ is false → emit the exit classification.
        let state = live_headless("h3", "quiet-1");
        state.release_for_session("quiet-1");
        assert!(
            !state.take_signal_attested("quiet-1"),
            "an unsignaled exit must surface its classification"
        );

        // take_ is one-shot: a second drain (or a redundant release path) reads false, so a
        // suppressed run can't accidentally re-suppress a later unrelated decision.
        let state = live_headless("h4", "once-1");
        state.begin_signal_outcome("once-1");
        assert!(state.take_signal_attested("once-1"));
        assert!(!state.take_signal_attested("once-1"), "drain is one-shot");

        // begin_signal_outcome only records HEADLESS ids (a PTY entry has no completion task to
        // drain the set → recording it would leak) and is a harmless no-op for an unknown id.
        let state = DashboardLaunchState::new(4);
        assert!(state.begin("pty-h").unwrap());
        state.bind("pty-h", "pty-1", Some("dash.md")).unwrap(); // bound, NOT mark_headless
        state.begin_signal_outcome("pty-1");
        assert!(
            !state.take_signal_attested("pty-1"),
            "a PTY session is never recorded in the attested set"
        );
        state.begin_signal_outcome("ghost"); // unknown id → no panic, no record
        assert!(!state.take_signal_attested("ghost"));
    }

    #[cfg(unix)]
    #[test]
    fn classify_headless_outcome_maps_capture_to_done_status() {
        // SW-OR-10 (AC-5): exit 0 is SUCCESS even with empty stdout (a headless agent's
        // value is its side effect, not its stdout); a timeout is `needs-attention`; a
        // non-zero exit / signal / spawn error is `failed`.
        use std::os::unix::process::ExitStatusExt;
        let out = |code: i32, timed_out: bool, stdout: &str, stderr: &str| CaptureOutput {
            status: (!timed_out).then(|| std::process::ExitStatus::from_raw(code << 8)),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
            timed_out,
            cancelled: false,
        };
        assert_eq!(
            classify_headless_outcome(&Ok(out(0, false, "", ""))),
            ("success", "completed".to_string()),
            "exit 0 + empty stdout = success (not a false Errored)",
        );
        assert_eq!(
            classify_headless_outcome(&Ok(out(0, false, "built it\nmore", ""))),
            ("success", "built it".to_string()),
        );
        assert_eq!(
            classify_headless_outcome(&Ok(out(0, true, "", ""))).0,
            "needs-attention",
        );
        assert_eq!(
            classify_headless_outcome(&Ok(out(1, false, "", "boom"))),
            ("failed", "boom".to_string()),
        );
        assert_eq!(
            classify_headless_outcome(&Err("failed to spawn: nope".to_string())).0,
            "failed",
        );
        // A signal death (raw status 9 = SIGKILL, no code) and an unknown status fail.
        let with_status = |status: Option<std::process::ExitStatus>| CaptureOutput {
            status,
            stdout: b"partial".to_vec(),
            stderr: Vec::new(),
            timed_out: false,
            cancelled: false,
        };
        assert_eq!(
            classify_headless_outcome(&Ok(with_status(Some(std::process::ExitStatus::from_raw(
                9
            )))))
            .0,
            "failed",
        );
        assert_eq!(
            classify_headless_outcome(&Ok(with_status(None))).0,
            "failed"
        );
        // SW-OR-13 (AC-4): a cooperative Stop → terminal `failed` with a truthful "stopped"
        // summary, NOT the generic "exited non-zero" a raw signal-exit would produce.
        let stopped = CaptureOutput {
            status: None,
            stdout: Vec::new(),
            stderr: Vec::new(),
            timed_out: false,
            cancelled: true,
        };
        assert_eq!(
            classify_headless_outcome(&Ok(stopped)),
            ("failed", "stopped".to_string()),
        );
    }

    #[test]
    fn complete_utf8_len_carries_a_split_codepoint() {
        // SW-OR-13: a multi-byte char split across a read boundary is held back (only the complete
        // prefix is emitted), so the live tail never shows U+FFFD at a chunk seam.
        let full = "ab…".as_bytes(); // 'a','b', then U+2026 = E2 80 A6 (3 bytes) → len 5
        assert_eq!(
            complete_utf8_len(full),
            full.len(),
            "all-complete → full length"
        );
        assert_eq!(
            complete_utf8_len(&full[..4]),
            2,
            "trailing E2 80 is an incomplete 3-byte seq → carry it, emit only \"ab\"",
        );
        assert_eq!(
            complete_utf8_len(&full[..3]),
            2,
            "trailing lead byte E2 is incomplete → carry it",
        );
        assert_eq!(
            complete_utf8_len(&[b'a', 0xFF, b'b']),
            3,
            "a genuinely-invalid byte is NOT carried (emitted lossy)",
        );
    }

    #[test]
    fn summarize_headless_clamps_and_takes_first_line() {
        assert_eq!(summarize_headless("  one line  "), "one line");
        assert_eq!(summarize_headless("first\nsecond\nthird"), "first");
        let long = "x".repeat(500);
        let s = summarize_headless(&long);
        assert!(s.ends_with('…'));
        assert_eq!(s.chars().count(), 201, "200 chars + ellipsis");
    }

    #[test]
    fn redact_structured_summary_scrubs_the_fallback_but_leaves_plain_runs() {
        // SW-OR-16 AC-4: a structured run's FALLBACK summary (clamped from raw stderr on a failed run)
        // is scrubbed before it is persisted/emitted; a non-structured run is untouched.
        let leak = "error: bad token ghp_abcdefghijklmnopqrstuvwxyz0123".to_string();
        let scrubbed =
            redact_structured_summary(Some(TranscriptFormat::AnthropicStreamJson), leak.clone());
        assert!(!scrubbed.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"));
        assert!(scrubbed.contains("[redacted]"));
        // A non-declaring run keeps its summary verbatim (OR-12 behavior, out of OR-16 scope).
        assert_eq!(redact_structured_summary(None, leak.clone()), leak);
    }

    #[test]
    fn structured_run_summary_redacts_before_the_clamp_on_the_failed_stderr_path() {
        // SW-OR-16 AC-4: a failed structured run summarizes stderr; redaction runs on the FULL text
        // BEFORE the 200-char clamp, so a secret straddling char 200 leaves no usable fragment.
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123";
        // Pad so the secret straddles the 200-char clamp boundary of the first line.
        let stderr = format!("{}error token {secret} tail", "x".repeat(190));
        let out = structured_run_summary(
            TranscriptFormat::AnthropicStreamJson,
            "failed",
            b"",
            stderr.as_bytes(),
        );
        assert!(
            !out.contains(secret),
            "no boundary fragment survives: {out}"
        );
        // A success run with no `result` event summarizes redacted stdout.
        let so = format!("first line has {secret} in it");
        let s = structured_run_summary(
            TranscriptFormat::AnthropicStreamJson,
            "success",
            so.as_bytes(),
            b"",
        );
        assert!(!s.contains(secret));
        assert!(s.contains("[redacted]"));
    }

    #[test]
    fn codex_headless_launch_line_leads_with_its_exec_entry() {
        // The built-in Codex descriptor's headless launch leads with its `exec` entry (the
        // descriptor's `headless_entry`, NOT a hardcoded `-p`) ahead of the agent's own args.
        let codex = codesense_pty_core::agent_cli::descriptor(
            codesense_pty_core::agent_cli::AgentCli::Codex,
        );
        let argv = headless_argv(&codex.headless_entry, vec![], vec![]);
        assert_eq!(
            argv.first().map(String::as_str),
            Some("exec"),
            "codex headless argv leads with `exec`, never `-p`: {argv:?}"
        );
        assert!(!argv.iter().any(|a| a == "-p"), "{argv:?}");
    }

    #[test]
    fn resolve_headless_task_prefers_explicit_prompt() {
        // SW-OR-11 (AC-3): an explicit launch prompt wins over the story recipe, verbatim —
        // the story path is present but ignored.
        let task = resolve_headless_task("do the thing", "/managed/SW-OR-11.md").unwrap();
        assert_eq!(task, "do the thing");
    }

    #[test]
    fn resolve_headless_task_derives_from_story_when_prompt_blank() {
        // SW-OR-11 (AC-2): a blank prompt (whitespace ≡ absent) derives the canonical task
        // from the story, `{storyPath}` substituted with the agent-visible path.
        let task = resolve_headless_task("   ", "/managed/SW-OR-11.md").unwrap();
        assert_eq!(
            task,
            "Read the story at /managed/SW-OR-11.md and complete it, satisfying every acceptance criterion.",
        );
        assert!(
            !task.contains("{storyPath}"),
            "the token must be resolved, never left literal",
        );
        // A padded path is trimmed before substitution — no stray whitespace in the task.
        let padded = resolve_headless_task("", "  /managed/SW-OR-11.md  ").unwrap();
        assert_eq!(
            padded,
            "Read the story at /managed/SW-OR-11.md and complete it, satisfying every acceptance criterion.",
        );
    }

    #[test]
    fn headless_capture_request_wires_the_runtimes_binary_entry_and_env_strip() {
        // The request the headless launch hands to `spawn_capture` carries the agent runtime's
        // binary + `exec` entry + `OPENAI_API_KEY` strip, NOT Claude's
        // `claude`/`-p`/`ANTHROPIC_API_KEY`.
        use crate::agents::adapter::LaunchDescriptor;
        use crate::commands::mcp_scope::ScopeOutcome;
        use codesense_pty_core::agent_cli::descriptor;
        use codesense_pty_core::agent_cli::AgentCli;
        use std::sync::atomic::AtomicU32;
        use std::sync::Arc;

        let codex = LaunchDescriptor::of(AgentCli::Codex);
        let req = headless_capture_request(
            PathBuf::from("/usr/local/bin/codex"),
            codex,
            vec!["--model".to_string(), "gpt".to_string()],
            ScopeOutcome::default(),
            "do the thing".to_string(),
            PathBuf::from("/tmp/an46"),
            Arc::new(AtomicU32::new(0)),
            "headless-abc",
            Some(58919),
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            Some(Duration::from_secs(2 * 60 * 60)),
        );
        assert_eq!(
            req.timeout,
            Duration::from_secs(2 * 60 * 60),
            "a caller's own run bound replaces the generic ceiling"
        );
        assert_eq!(
            req.program.file_name().and_then(|s| s.to_str()),
            Some("codex"),
            "headless resolves the runtime's binary, not claude"
        );
        assert_eq!(req.args.first().map(String::as_str), Some("exec"));
        assert!(!req.args.iter().any(|a| a == "-p"), "never Claude's `-p`");
        assert_eq!(req.env_remove[0], "OPENAI_API_KEY");
        // No terminal host's bindings: its inbox hooks end at once.
        assert_eq!(&req.env_remove[1..], HEADLESS_HOSTLESS_ENV);
        // SW-OR-14 (AC-2): the launch identity is injected so a headless agent can echo
        // its session id into session-keyed verbs and reach the local API — parity with
        // the interactive PTY spawn.
        assert!(
            req.env
                .iter()
                .any(|(k, v)| k == "CODESENSE_TAB_SESSION_ID" && v == "headless-abc"),
            "session id injected as launch identity"
        );
        assert!(
            req.env
                .iter()
                .any(|(k, v)| k == "CODESENSE_API_PORT" && v == "58919"),
            "api port injected"
        );

        // Claude's runtime: its binary/entry/strip. A `None` api_port (API not yet bound)
        // omits the port var but still stamps the id.
        let claude_req = headless_capture_request(
            PathBuf::from("/opt/homebrew/bin/claude"),
            LaunchDescriptor::of(AgentCli::Claude),
            vec![],
            ScopeOutcome::default(),
            "p".to_string(),
            PathBuf::from("/tmp/an46"),
            Arc::new(AtomicU32::new(0)),
            "headless-xyz",
            None,
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            None,
        );
        assert_eq!(
            claude_req.timeout, HEADLESS_RUN_TIMEOUT,
            "without one, the generic ceiling holds"
        );
        assert!(
            claude_req
                .env
                .iter()
                .any(|(k, v)| k == "CODESENSE_TAB_SESSION_ID" && v == "headless-xyz"),
            "session id stamped even when the api port is unbound"
        );
        assert!(
            !claude_req
                .env
                .iter()
                .any(|(k, _)| k == "CODESENSE_API_PORT"),
            "no port var when the api port is None"
        );
        assert_eq!(
            claude_req.program.file_name().and_then(|s| s.to_str()),
            Some("claude")
        );
        assert_eq!(
            claude_req.env_remove,
            descriptor(AgentCli::Claude)
                .env_suppress
                .iter()
                .cloned()
                .chain(HEADLESS_HOSTLESS_ENV.iter().map(|name| name.to_string()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn opencode_headless_runs_json_events_with_the_prompt_on_stdin_and_a_structured_format() {
        use crate::agents::adapter::LaunchDescriptor;
        use crate::commands::mcp_scope::ScopeOutcome;
        use codesense_pty_core::agent_cli::AgentCli;
        use std::sync::atomic::AtomicU32;

        let descriptor = LaunchDescriptor::of(AgentCli::OpenCode);
        let format = descriptor.structured_headless.as_ref().map(|h| h.format);
        assert_eq!(format, Some(TranscriptFormat::OpenCodeRunJson));
        let req = headless_capture_request(
            PathBuf::from("/opt/homebrew/bin/opencode"),
            descriptor,
            vec!["-m".to_string(), "fake/model".to_string()],
            ScopeOutcome::default(),
            "-starts with a dash".to_string(),
            PathBuf::from("/tmp/oc"),
            Arc::new(AtomicU32::new(0)),
            "headless-oc",
            None,
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            None,
        );
        assert_eq!(
            req.args,
            ["run", "--format", "json", "-m", "fake/model"],
            "the structured request adds nothing: the entry already asks for JSON"
        );
        assert_eq!(req.stdin.as_deref(), Some(&b"-starts with a dash"[..]));
        assert_eq!(
            req.program.file_name().and_then(|s| s.to_str()),
            Some("opencode")
        );
    }

    #[test]
    fn a_hermes_run_that_never_ends_is_stopped_by_the_configured_capture_deadline() {
        use crate::routines::test_support::HERMES_DEADLINE_RUN;
        // The recorded 5xx run printed only its init line before the deadline killed it.
        let dir = tempfile::tempdir().unwrap();
        let recorded = dir.path().join("recorded.jsonl");
        std::fs::write(&recorded, HERMES_DEADLINE_RUN).unwrap();
        let request = CaptureRequest {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "cat \"$0\"; exec sleep 60".into(), recorded.to_string_lossy().into_owned()],
            cwd: Some(dir.path().to_path_buf()),
            env: Vec::new(),
            env_remove: Vec::new(),
            stdin: None,
            timeout: Duration::from_millis(500),
            process_group: true,
            pid_sink: None,
            output_sink: None,
            last_drain_ms: None,
            max_output_bytes: None,
            cancel: None,
        };
        let out = spawn_capture(request).unwrap();
        assert!(out.timed_out);
        let result = Ok(out);
        let (status, _) = headless_run_outcome(Some(TranscriptFormat::HermesStreamJson), &result);
        assert_eq!(status, "needs-attention");
        assert_eq!(HEADLESS_RUN_TIMEOUT, Duration::from_secs(30 * 60));
    }

    #[test]
    fn opencode_headless_request_carries_the_headless_permission_map() {
        use crate::agents::adapter::LaunchDescriptor;
        use crate::commands::mcp_scope::ScopeOutcome;
        use codesense_pty_core::agent_cli::AgentCli;
        use std::sync::atomic::AtomicU32;

        let agent = crate::routines::test_support::agent("oc", AgentCli::OpenCode);
        let entry = crate::agents::permissions::generate(
            &agent,
            AgentCli::OpenCode,
            crate::agents::model::LaunchMode::Headless,
        )
        .remove(0);
        let req = headless_capture_request(
            PathBuf::from("/opt/homebrew/bin/opencode"),
            LaunchDescriptor::of(AgentCli::OpenCode),
            Vec::new(),
            ScopeOutcome {
                extra_env: vec![("OPENCODE_PERMISSION".into(), entry.content.to_string())],
                ..ScopeOutcome::default()
            },
            "task".to_string(),
            PathBuf::from("/tmp/oc"),
            Arc::new(AtomicU32::new(0)),
            "headless-oc",
            None,
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            None,
        );
        let value = req
            .env
            .iter()
            .find(|(name, _)| name == "OPENCODE_PERMISSION")
            .map(|(_, value)| serde_json::from_str::<serde_json::Value>(value).unwrap())
            .expect("the permission env reaches the child");
        // The map reaches OpenCode as ordered native rules: one `{ action, resource, effect }` each.
        let effect = |action: &str| {
            value["permissions"]
                .as_array()
                .expect("the native rule list")
                .iter()
                .find(|rule| rule["action"] == action)
                .map(|rule| rule["effect"].clone())
        };
        assert_eq!(effect("*"), Some(serde_json::json!("deny")));
        assert_eq!(effect("read"), Some(serde_json::json!("allow")));
        assert_eq!(effect("codesense_signal_done"), Some(serde_json::json!("allow")));
    }

    #[test]
    fn a_structured_opencode_run_reports_its_final_text_and_fails_on_an_error_event() {
        use crate::routines::test_support::{exit_code, OPENCODE_ERROR_RUN, OPENCODE_TEXT_RUN};
        let format = Some(TranscriptFormat::OpenCodeRunJson);
        let capture = |code, stdout: &str| {
            Ok(CaptureOutput {
                status: Some(exit_code(code)),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
                timed_out: false,
                cancelled: false,
            })
        };
        assert_eq!(
            headless_run_outcome(format, &capture(0, OPENCODE_TEXT_RUN)),
            (
                "success",
                "FINAL-ANSWER: the fake model says hello.".to_string()
            )
        );
        // An `error` event is a failure even when the process exited 0.
        assert_eq!(
            headless_run_outcome(format, &capture(0, OPENCODE_ERROR_RUN)),
            ("failed", "fake bad request".to_string())
        );
        assert_eq!(
            headless_run_outcome(format, &capture(1, OPENCODE_ERROR_RUN)),
            ("failed", "fake bad request".to_string())
        );
        // A run that declares no format is judged by its exit alone.
        assert_eq!(
            headless_run_outcome(None, &capture(0, OPENCODE_ERROR_RUN)).0,
            "success"
        );
        // A secret in the final text never reaches the summary.
        let leaked = OPENCODE_TEXT_RUN.replace(
            "the fake model says hello",
            "ghp_abcdefghijklmnopqrstuvwxyz0123",
        );
        assert!(!headless_run_outcome(format, &capture(0, &leaked))
            .1
            .contains("ghp_abcdefghijklmnopqrstuvwxyz0123"));
    }

    #[test]
    fn a_stopped_structured_run_keeps_its_stopped_label_whatever_it_had_printed() {
        use crate::routines::test_support::{killed_by, OPENCODE_TEXT_RUN};
        for stdout in [OPENCODE_TEXT_RUN, ""] {
            let stopped = Ok(CaptureOutput {
                status: Some(killed_by(15)),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
                timed_out: false,
                cancelled: true,
            });
            assert_eq!(
                headless_run_outcome(Some(TranscriptFormat::OpenCodeRunJson), &stopped),
                ("failed", HEADLESS_STOPPED_SUMMARY.to_string())
            );
        }
    }

    #[test]
    fn a_decoded_structured_summary_is_scrubbed_before_it_is_persisted() {
        use crate::routines::test_support::exit_code;
        // FAKE values. The first is JSON text whose quotes OpenCode escapes in its stdout, so the
        // raw scan misses the assignment; the second is a token whose first letter is a `\u`
        // escape, which only the decoded text shows.
        let password = "hunter2-opaque-xyz";
        let token_tail = "hp_abcdefghijklmnopqrstuvwxyz0123";
        let credentials = serde_json::json!({ "password": password }).to_string();
        let escaped_final = |text: &str| {
            let text = serde_json::to_string(text).unwrap();
            format!(
                "{{\"type\":\"step_start\",\"sessionID\":\"s\",\"part\":{{}}}}\n\
                 {{\"type\":\"text\",\"sessionID\":\"s\",\"part\":{{\"text\":{text}}}}}\n\
                 {{\"type\":\"step_finish\",\"sessionID\":\"s\",\"part\":{{\"reason\":\"stop\",\"tokens\":{{}}}}}}\n"
            )
        };
        let capture = |stdout: String| {
            Ok(CaptureOutput {
                status: Some(exit_code(0)),
                stdout: stdout.into_bytes(),
                stderr: Vec::new(),
                timed_out: false,
                cancelled: false,
            })
        };
        let format = Some(TranscriptFormat::OpenCodeRunJson);
        let (_, summary) = headless_run_outcome(format, &capture(escaped_final(&credentials)));
        assert!(!summary.contains(password), "{summary}");
        let raw_token = format!(
            "{{\"type\":\"text\",\"sessionID\":\"s\",\"part\":{{\"text\":\"key \\u0067{token_tail}\"}}}}\n\
             {{\"type\":\"step_finish\",\"sessionID\":\"s\",\"part\":{{\"reason\":\"stop\"}}}}\n"
        );
        let (_, summary) = headless_run_outcome(format, &capture(raw_token));
        assert!(!summary.contains(token_tail), "{summary}");
    }

    #[cfg(unix)]
    #[test]
    fn injected_identity_reaches_a_real_headless_child() {
        // SW-OR-14 (AC-2), END-TO-END: prove the identity injected by `headless_capture_request`
        // actually lands in a spawned child's ENVIRONMENT — not just the request struct. Build the
        // request through the real helper, spawn a fixture that echoes its `$CODESENSE_TAB_SESSION_ID`
        // via the real `spawn_capture`, and assert we read the id back. This closes the
        // request-builder-only gap: the injection reaches a live process, the same way a headless
        // agent would read it to key its session-scoped MCP verbs.
        use crate::commands::mcp_scope::ScopeOutcome;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("echo-id.sh");
        std::fs::write(
            &bin,
            "#!/bin/sh\nprintf '%s' \"$CODESENSE_TAB_SESSION_ID\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

        let request = headless_capture_request(
            bin,
            crate::agents::adapter::LaunchDescriptor::of(
                codesense_pty_core::agent_cli::AgentCli::Claude,
            ),
            vec![],
            ScopeOutcome::default(),
            String::new(), // empty stdin; the fixture doesn't read it (drops → EOF, no hang)
            dir.path().to_path_buf(),
            std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            "headless-e2e-1",
            Some(4321),
            Vec::new(),
            Vec::new(),
            BTreeMap::new(),
            None,
        );
        let out = spawn_capture(request).expect("fixture spawns");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "headless-e2e-1",
            "the injected session id is visible in the child's environment"
        );
    }

    /// A saved agent's env reaches a real headless child, and every variable CodeSense sets or
    /// strips wins over the agent's of the same name: the launch identity, the API port, the
    /// account environment, the usage export and the runtime's guarded-variable strip.
    #[cfg(unix)]
    #[tokio::test]
    async fn agent_env_reaches_a_real_headless_child_under_codesense_variables() {
        use crate::commands::mcp_scope::ScopeOutcome;
        use std::os::unix::fs::PermissionsExt;

        for cli in [
            codesense_pty_core::agent_cli::AgentCli::Claude,
            codesense_pty_core::agent_cli::AgentCli::Codex,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let bin = dir.path().join("echo-env.sh");
            let guarded = if cli == codesense_pty_core::agent_cli::AgentCli::Claude {
                "ANTHROPIC_API_KEY"
            } else {
                "OPENAI_API_KEY"
            };
            std::fs::write(
            &bin,
            "#!/bin/sh\nfor v in AGENT_ONLY CODESENSE_TAB_SESSION_ID CODESENSE_API_PORT ACCOUNT_VAR \
             OTEL_LOGS_EXPORTER OTEL_EXPORTER_OTLP_LOGS_ENDPOINT OTEL_SDK_DISABLED ANTHROPIC_API_KEY; do printf '%s=%s;' \"$v\" \"$(printenv $v)\"; done\n\
             case \"$PATH\" in /agent/bin*) echo AGENT_PATH;; esac\n\
             if [ \"$CODESENSE_MCP_CONFIG\" = /agent.json ]; then echo AGENT_MCP_CONFIG; fi\n".replace("ANTHROPIC_API_KEY", guarded),
        )
        .unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            let agent_env: BTreeMap<String, String> = [
                ("AGENT_ONLY", "from-agent"),
                ("CODESENSE_TAB_SESSION_ID", "spoofed"),
                ("CODESENSE_API_PORT", "1"),
                ("ACCOUNT_VAR", "agent"),
                ("OTEL_LOGS_EXPORTER", "none"),
                (
                    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
                    "https://other.example/logs",
                ),
                ("OTEL_SDK_DISABLED", "true"),
                (guarded, "agent-key"),
                ("CODESENSE_MCP_CONFIG", "/agent.json"),
                ("PATH", "/agent/bin"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

            let mut env = vec![("ACCOUNT_VAR".to_string(), "account".to_string())];
            let mut args = Vec::new();
            headless_usage_owner(
                None,
                "personal/worker",
                "handle",
                &dir.path().to_string_lossy(),
            )
            .prepare_export(
                Ok(dir.path().join("usage.sqlite")),
                "headless-env-1",
                codesense_pty_core::agent_cli::descriptor(cli)
                    .telemetry
                    .as_ref(),
                Some(4321),
                &mut env,
                &mut args,
            )
            .await;
            let request = headless_capture_request(
                bin,
                crate::agents::adapter::LaunchDescriptor::of(cli),
                args,
                ScopeOutcome::default(),
                String::new(),
                dir.path().to_path_buf(),
                std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
                "headless-env-1",
                Some(4321),
                env,
                Vec::new(),
                agent_env,
                None,
            );
            let out = spawn_capture(request).expect("fixture spawns");
            assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            format!("AGENT_ONLY=from-agent;CODESENSE_TAB_SESSION_ID=headless-env-1;CODESENSE_API_PORT=4321;ACCOUNT_VAR=account;OTEL_LOGS_EXPORTER=otlp;OTEL_EXPORTER_OTLP_LOGS_ENDPOINT={}/v1/logs;OTEL_SDK_DISABLED=false;{guarded}=;", crate::http::routes::telemetry::endpoint_for(4321, "headless-env-1"))
        );
        }
    }

    /// A managed account's headless run drops the credentials its CLI ranks above the login,
    /// even when the saved agent's own env carries them; a run on the person's own login keeps
    /// the agent's.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_managed_account_headless_run_drops_a_token_the_saved_agent_carries() {
        use crate::commands::mcp_scope::ScopeOutcome;
        use std::os::unix::fs::PermissionsExt;

        let cli = codesense_pty_core::agent_cli::AgentCli::Claude;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("echo-env.sh");
        std::fs::write(
            &bin,
            "#!/bin/sh\nfor v in CLAUDE_CODE_OAUTH_TOKEN ANTHROPIC_AUTH_TOKEN; do printf '%s=%s;' \"$v\" \"$(printenv $v)\"; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let agent_env: BTreeMap<String, String> = [
            ("CLAUDE_CODE_OAUTH_TOKEN", "agent-token"),
            ("ANTHROPIC_AUTH_TOKEN", "agent-auth"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let scrub = codesense_pty_core::agent_cli::descriptor(cli)
            .account_env_scrub
            .clone();
        let run = |managed: bool| {
            let request = headless_capture_request(
                bin.clone(),
                crate::agents::adapter::LaunchDescriptor::of(cli),
                Vec::new(),
                ScopeOutcome::default(),
                String::new(),
                dir.path().to_path_buf(),
                std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
                "headless-scrub-1",
                None,
                Vec::new(),
                if managed { scrub.clone() } else { Vec::new() },
                agent_env.clone(),
                None,
            );
            String::from_utf8_lossy(&spawn_capture(request).expect("fixture spawns").stdout)
                .trim()
                .to_string()
        };
        assert_eq!(run(true), "CLAUDE_CODE_OAUTH_TOKEN=;ANTHROPIC_AUTH_TOKEN=;");
        assert_eq!(
            run(false),
            "CLAUDE_CODE_OAUTH_TOKEN=agent-token;ANTHROPIC_AUTH_TOKEN=agent-auth;"
        );
    }

    #[test]
    fn resolve_headless_task_refuses_when_no_prompt_and_no_story() {
        // SW-OR-11 (AC-4): neither an explicit prompt nor a resolvable story → REFUSED, so a
        // bare `claude -p` is never spawned. A whitespace-only story path counts as absent.
        let err = resolve_headless_task("", "").unwrap_err();
        assert!(err.starts_with("unavailable:"), "typed refusal: {err}");
        assert!(resolve_headless_task("  ", "   ").is_err());
    }

    #[test]
    fn headless_argv_ensures_dash_p_and_appends_scope_flags() {
        // SW-OR-10 (AC-3): `-p` is ADDED when the profile omits it (stdin-print mode),
        // NOT duplicated when present, and the MCP scope flags follow the profile argv.
        let scope = vec![
            "--mcp-config".to_string(),
            "/tmp/s.json".to_string(),
            "--strict-mcp-config".to_string(),
        ];
        let dash_p = ["-p".to_string()];
        let added = headless_argv(
            &dash_p,
            vec!["--model".into(), "claude".into()],
            scope.clone(),
        );
        assert_eq!(
            added,
            vec![
                "-p",
                "--model",
                "claude",
                "--mcp-config",
                "/tmp/s.json",
                "--strict-mcp-config"
            ],
        );
        let kept = headless_argv(&dash_p, vec!["-p".into(), "--verbose".into()], vec![]);
        assert_eq!(kept, vec!["-p", "--verbose"], "no duplicate entry token");

        // SW-AN-45: the entry is adapter data — a Codex-shaped `["exec"]` entry is prepended
        // just the same (agent-neutral; core never hardcodes `-p`).
        let exec = ["exec".to_string()];
        assert_eq!(
            headless_argv(
                &exec,
                vec!["--dangerously-bypass-approvals-and-sandbox".into()],
                vec![]
            ),
            vec!["exec", "--dangerously-bypass-approvals-and-sandbox"],
        );
        assert_eq!(
            headless_argv(&exec, vec!["exec".into(), "--foo".into()], vec![]),
            vec!["exec", "--foo"],
            "entry not duplicated when the profile already supplies it",
        );
    }

    // SW-AU-45: `handle_for_session` recovers the bound handle (a story worker's = its story_id) that
    // the reactive fire maps to an owning team. A bound session yields its handle; an unbound one None.
    #[test]
    fn handle_for_session_resolves_bound_handle_else_none() {
        let state = DashboardLaunchState::new(4);
        assert!(state.handle_for_session("nobody").is_none());
        state.register("SW-AU-45", "sess-1", Some("codesense"));
        assert_eq!(
            state.handle_for_session("sess-1").as_deref(),
            Some("SW-AU-45")
        );
    }

    fn launch_facts(args: &[&str]) -> crate::session_usage::record::LaunchFacts {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        crate::session_usage::record::LaunchFacts::new(
            codesense_pty_core::agent_cli::AgentCli::Claude,
            Some("2.5.0"),
            &args,
            Some(vec!["codesense".into()]),
            vec!["signal_done".into()],
            "acme/dev/worker@3".into(),
            "hash-1".into(),
            "/work/wt",
        )
        .with_provided_skills(vec!["bmad-help".into()])
    }

    /// A bound launch the way every launch path leaves it: owner, account, run and worktree stamped.
    fn bound_launch(owner: SessionOwner) -> (DashboardLaunchState, SessionSnapshot) {
        let state = DashboardLaunchState::new(4);
        assert!(state.begin_with_provenance("SV-1", None, LaunchRecord {
            meta: LaunchMeta { story: Some("SV-1".into()), ..Default::default() },
            ..Default::default()
        }).unwrap());
        state.bind("SV-1", "sess-1", Some("codesense")).unwrap();
        state.restamp_launch("SV-1", Some(owner));
        state.set_worktree_dir_if_absent("SV-1", PathBuf::from("/work/wt"));
        state.stamp_provenance(
            "SV-1",
            RunProvenance {
                run_id: "agent-launch:r1".into(),
                story_id: "SV-1".into(),
                ..Default::default()
            },
        );
        state.set_agent_revision_if_absent("SV-1", "acme/dev/worker@3".into());
        let snapshot = state.snapshot().into_iter().find(|s| s.handle == "SV-1").unwrap();
        (state, snapshot)
    }

    /// Every launch path hands the same facts to the one composer, so one complete record covers
    /// them: what trusted preparation knew is all there, and only what no path can know is
    /// missing, with its reason.
    #[test]
    fn a_launch_record_carries_every_fact_preparation_held() {
        let owner = SessionOwner { principal: None, org: "acme".into(), team: Some("dev".into()) };
        let (state, snapshot) = bound_launch(owner);
        let facts = launch_facts(&["--model=claude-opus-5-5", "--session-id", "native-1"]);
        let owner = state.owner_for_session("sess-1");
        let r = record_from_launch("sess-1", &snapshot, owner.as_ref(), &facts);
        assert_eq!(r.cli.as_deref(), Some("claude"));
        assert_eq!(r.cli_version.as_deref(), Some("2.5.0"));
        assert_eq!(r.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(r.native_session_id.as_deref(), Some("native-1"));
        assert_eq!(r.agent.as_deref(), Some("acme/dev/worker@3"));
        assert_eq!(r.manifest_hash.as_deref(), Some("hash-1"));
        assert_eq!(r.mcp_allow.as_deref(), Some(&["codesense".to_string()][..]));
        assert_eq!(r.bridge_tools.as_deref(), Some(&["signal_done".to_string()][..]));
        assert_eq!((r.org.as_deref(), r.team.as_deref()), (Some("acme"), Some("dev")));
        assert_eq!(r.handle.as_deref(), Some("SV-1"));
        assert_eq!(r.run_id.as_deref(), Some("agent-launch:r1"));
        assert_eq!(r.story_id.as_deref(), Some("SV-1"));
        assert_eq!(r.workspace.as_deref(), Some("/work/wt"));
        assert_eq!(r.worktree.as_deref(), Some("/work/wt"));
        assert_eq!(r.provided_skills, Some(vec!["bmad-help".to_string()]));
        // Only the account (none staged here) and the branch (read off the launch path) are open.
        let open: Vec<_> = r.unavailable.keys().map(String::as_str).collect();
        assert_eq!(open, ["account", "branch"]);
        assert!(r.config_home.as_deref().is_some_and(|h| h.ends_with(".claude")), "{:?}", r.config_home);
    }

    /// Under managed Claude settings that send usage elsewhere, a Claude launch's record says its
    /// usage isn't recorded; a Codex launch on the same Mac doesn't.
    #[test]
    fn a_claude_launch_record_says_usage_is_not_recorded_under_managed_settings() {
        let owner = SessionOwner { principal: None, org: "acme".into(), team: Some("dev".into()) };
        let (state, snapshot) = bound_launch(owner);
        let owner = state.owner_for_session("sess-1");
        let usage = |facts: &crate::session_usage::record::LaunchFacts| {
            record_from_launch("sess-1", &snapshot, owner.as_ref(), facts).unavailable.get("usage").cloned()
        };
        let codex = crate::session_usage::record::LaunchFacts {
            cli: Some("codex".into()),
            ..launch_facts(&[])
        };
        assert_eq!(usage(&launch_facts(&[])), None, "no managed settings, no reason");
        let read_from_its_log = crate::session_usage::managed_telemetry::Fixture::redirecting();
        assert_eq!(usage(&launch_facts(&[])), None, "Claude's session log is read");
        drop(read_from_its_log);
        let _managed = crate::session_usage::managed_telemetry::Fixture::redirecting_unread();
        assert_eq!(
            usage(&launch_facts(&[])).as_deref(),
            Some(crate::session_usage::managed_telemetry::USAGE_REDIRECTED)
        );
        assert_eq!(usage(&codex), None);
    }

    /// An account launch runs its CLI in the account's own home, and the record says so; every
    /// other launch records the CLI's default home.
    #[test]
    fn a_launch_record_keeps_the_accounts_home_over_the_default() {
        let owner = SessionOwner { principal: None, org: "acme".into(), team: Some("dev".into()) };
        let (state, mut snapshot) = bound_launch(owner);
        let owner = state.owner_for_session("sess-1");
        snapshot.account_provenance = Some(AgentAccountProvenance {
            runtime: "claude".into(),
            provider: "claude".into(),
            source: "explicit".into(),
            reason: "explicit-override".into(),
            account_id: Some("work".into()),
            label: None,
            home: Some("/accounts/work/claude".into()),
        });
        let r = record_from_launch("sess-1", &snapshot, owner.as_ref(), &launch_facts(&[]));
        assert_eq!(r.config_home.as_deref(), Some("/accounts/work/claude"));
        assert!(!r.unavailable.contains_key("configHome"));
        // A home the launch's own environment sets (Codex's per-session home) wins over both.
        let scoped = launch_facts(&[]).with_launch_env(
            codesense_pty_core::agent_cli::AgentCli::Claude,
            &Default::default(),
            &[("CLAUDE_CONFIG_DIR".to_owned(), "/scope/session-1".to_owned())],
        );
        let r = record_from_launch("sess-1", &snapshot, owner.as_ref(), &scoped);
        assert_eq!(r.config_home.as_deref(), Some("/scope/session-1"));
        snapshot.account_provenance = None;
        let r = record_from_launch("sess-1", &snapshot, owner.as_ref(), &launch_facts(&[]));
        assert!(r.config_home.as_deref().is_some_and(|h| h.ends_with("/.claude")));
    }

    #[test]
    fn a_launch_records_the_home_its_environment_names_in_precedence_order() {
        use std::collections::BTreeMap;
        let claude = codesense_pty_core::agent_cli::AgentCli::Claude;
        let codex = codesense_pty_core::agent_cli::AgentCli::Codex;
        let none = BTreeMap::new();
        let base = |cli| {
            crate::session_usage::record::LaunchFacts::new(
                cli, None, &[], None, vec![], "a@1".into(), "h".into(), "/w",
            )
        };
        // The launch's own variables (a scoped Codex home) win; others are ignored.
        let scope = [
            ("OTHER".to_owned(), "x".to_owned()),
            ("CODEX_HOME".to_owned(), "/scope/s1".to_owned()),
        ];
        assert_eq!(base(codex).with_launch_env(codex, &none, &scope).scope_home.as_deref(), Some("/scope/s1"));
        assert_eq!(base(codex).with_launch_env(codex, &none, &scope[..1]).scope_home, None);
        // A saved agent's own variable replaces the default home (R4: not the app's environment).
        let agent: BTreeMap<String, String> =
            [("CLAUDE_CONFIG_DIR".to_owned(), "/custom/claude".to_owned())].into();
        let facts = base(claude).with_launch_env(claude, &agent, &[]);
        assert_eq!(facts.config_home.as_deref(), Some("/custom/claude"));
        assert_eq!(facts.scope_home, None);
        // A relative value is no home: the default stays.
        let relative: BTreeMap<String, String> =
            [("CLAUDE_CONFIG_DIR".to_owned(), "relative".to_owned())].into();
        assert!(base(claude)
            .with_launch_env(claude, &relative, &[])
            .config_home
            .as_deref()
            .is_some_and(|h| h.ends_with("/.claude")));
    }

    /// Launch preparation → record → capture, the way a bound launch goes: a saved Claude agent
    /// with its own `CLAUDE_CONFIG_DIR`, an account launch, and a scoped Codex launch each leave a
    /// session whose conversation is found where that CLI really wrote it.
    #[test]
    fn a_launch_is_captured_from_the_home_its_own_environment_gave_the_cli() {
        use std::collections::BTreeMap;
        use crate::session_conversation::{conversation_file, TestWriter};
        let claude_log = include_str!("../../crates/codesense-pty-core/tests/fixtures/claude/session-2.1.296.jsonl");
        let codex_log = include_str!("../../crates/codesense-pty-core/tests/fixtures/codex/rollout-0.164.0.jsonl");
        let (claude_id, claude_id2, codex_id) = (
            "c1a0de00-0000-4000-a000-000000000201",
            "c1a0de00-0000-4000-a000-000000000202",
            "01a17e00-0000-7000-b000-000000000003",
        );
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let writer = TestWriter::install(root.join("usage.sqlite"), root.join("conv"), root.join("home"));
        let store = crate::session_usage::UsageStore::open(&writer.store).unwrap();
        let place = |home: &std::path::Path, claude: Option<&str>| {
            if let Some(id) = claude {
                let dir = home.join("projects/-w");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(format!("{id}.jsonl")), claude_log).unwrap();
            } else {
                let dir = home.join("sessions/2026/10/27");
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join(format!("rollout-2026-10-27T10-00-00-{codex_id}.jsonl")), codex_log).unwrap();
            }
        };
        let claude = codesense_pty_core::agent_cli::AgentCli::Claude;
        let codex = codesense_pty_core::agent_cli::AgentCli::Codex;
        let facts = |cli, native: &str| {
            crate::session_usage::record::LaunchFacts::new(
                cli, None, &["--session-id".to_owned(), native.to_owned()], None, vec![], "a@1".into(), "h".into(), "/w",
            )
        };
        let launch = |session: &str, facts, account_home: Option<&str>| {
            let owner = SessionOwner { principal: None, org: "acme".into(), team: None };
            let (state, mut snapshot) = bound_launch(owner.clone());
            snapshot.account_provenance = account_home.map(|h| AgentAccountProvenance {
                runtime: "claude".into(), provider: "claude".into(), source: "explicit".into(),
                reason: "explicit-override".into(), account_id: Some("work".into()), label: None, home: Some(h.into()),
            });
            let owner = state.owner_for_session("sess-1");
            let mut record = record_from_launch(session, &snapshot, owner.as_ref(), &facts);
            record.native_session_id = facts.native_session_id.clone();
            store.write_session_record(&record).unwrap();
        };
        // 1. A saved agent with its own CLAUDE_CONFIG_DIR.
        let custom = root.join("custom-claude");
        place(&custom, Some(claude_id));
        let agent_env: BTreeMap<String, String> = [("CLAUDE_CONFIG_DIR".to_owned(), custom.to_string_lossy().into_owned())].into();
        launch("agent-env", facts(claude, claude_id).with_launch_env(claude, &agent_env, &[]), None);
        crate::session_conversation::note_end("agent-env");
        assert!(conversation_file(&writer.dir, "agent-env").unwrap().exists(), "agent env home");
        // 2. An account launch.
        let account = root.join("accounts/work/claude");
        place(&account, Some(claude_id2));
        std::fs::remove_dir_all(root.join("custom-claude")).unwrap();
        launch("acct", facts(claude, claude_id2), Some(&account.to_string_lossy()));
        crate::session_conversation::note_end("acct");
        assert!(conversation_file(&writer.dir, "acct").unwrap().exists(), "account home");
        // 3. A scoped Codex launch.
        let scoped = root.join("scope/s1");
        place(&scoped, None);
        let scope_env = [("CODEX_HOME".to_owned(), scoped.to_string_lossy().into_owned())];
        launch("cx-scope", facts(codex, codex_id).with_launch_env(codex, &BTreeMap::new(), &scope_env), None);
        crate::session_conversation::note_end("cx-scope");
        assert!(conversation_file(&writer.dir, "cx-scope").unwrap().exists(), "scoped codex home");
    }

    /// The headless run's end, through the function the run calls: its conversation is stored
    /// before the run's scoped home goes, whichever CLI ran it.
    #[tokio::test]
    async fn a_headless_runs_end_stores_its_conversation() {
        use crate::session_conversation::{nth_uuid, TestWriter};
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let w = TestWriter::install(root.join("usage.sqlite"), root.join("conv"), root.join("home"));
        w.seed_codex("hl-codex", &nth_uuid(1));
        w.seed_claude("hl-claude", &nth_uuid(2));
        finish_headless_session("hl-codex", codesense_pty_core::agent_cli::AgentCli::Codex, "success").await;
        finish_headless_session("hl-claude", codesense_pty_core::agent_cli::AgentCli::Claude, "failed").await;
        assert_eq!(w.stored("hl-codex").unwrap().1.len(), 3);
        assert_eq!(w.stored("hl-claude").unwrap().1.len(), 3);
    }

    #[tokio::test]
    async fn pbf210_the_real_task_backed_writer_unblocks_conversation_capture_in_both_orders() {
        use crate::session_conversation::{nth_uuid, TestWriter};
        for early in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let root = root.path().canonicalize().unwrap();
            let w = TestWriter::install(root.join("usage.sqlite"), root.join("conv"), root.join("home"));
            let session = "pbf210-real-launch-writer";
            let native = nth_uuid(if early { 71 } else { 72 });
            w.seed_claude(session, &native);
            rusqlite::Connection::open(&w.store).unwrap()
                .execute("DELETE FROM session_record WHERE session_id = ?1", [session]).unwrap();
            let state = DashboardLaunchState::new(1);
            state.set_task_store_path_for_test(w.store.clone());
            assert!(state.begin("pbf210-real-launch").unwrap());
            state.bind("pbf210-real-launch", session, None).unwrap();
            let (release, gate) = tokio::sync::watch::channel(());
            *state.task_capture_gate.lock().unwrap() = Some(gate);
            write_launch_record(&state, session, "pbf210-real-launch", crate::session_usage::record::LaunchFacts {
                cli: Some("claude".into()), native_session_id: Some(native),
                config_home: Some(w.home.join(".claude").to_string_lossy().into_owned()),
                ..Default::default()
            });
            let mut captured = state.task_captures.lock().unwrap().get(session).unwrap().0.clone();
            if early {
                drop(release);
                let _ = captured.changed().await;
                finish_headless_session(session, codesense_pty_core::agent_cli::AgentCli::Claude, "success").await;
            } else {
                let path = w.store.clone();
                let (waiting, ready) = tokio::sync::oneshot::channel();
                let observer = std::thread::spawn(move || {
                    crate::session_usage::record::wait_for_record_waiter(&path, session);
                    waiting.send(()).unwrap();
                });
                tokio::join!(
                    finish_headless_session(session, codesense_pty_core::agent_cli::AgentCli::Claude, "success"),
                    async { ready.await.unwrap(); drop(release); },
                );
                observer.join().unwrap();
                let _ = captured.changed().await;
            }
            assert_eq!(w.stored(session).expect("the real launch write releases final capture").1.len(), 3);
            let store = crate::session_usage::UsageStore::open(&w.store).unwrap();
            assert!(store.read_session_record(session).unwrap().is_some());
            assert!(store.read_task(session).unwrap().is_some(), "task capture remains intact");
        }
    }

    #[tokio::test]
    async fn pbf210_a_headless_end_before_the_record_write_stores_the_conversation() {
        use crate::session_conversation::{nth_uuid, TestWriter};
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let w = TestWriter::install(root.join("usage.sqlite"), root.join("conv"), root.join("home"));
        let session = "late-headless-codex";
        w.seed_codex(session, &nth_uuid(13));
        let store = crate::session_usage::UsageStore::open(&w.store).unwrap();
        let record = store.read_session_record(session).unwrap().unwrap();
        rusqlite::Connection::open(&w.store).unwrap()
            .execute("DELETE FROM session_record WHERE session_id = ?1", [session]).unwrap();
        let path = w.store.clone();
        let (tick, ticked) = std::sync::mpsc::channel();
        let ticker = tokio::spawn(async move { tick.send(()).unwrap(); });
        let writer = std::thread::spawn(move || {
            crate::session_usage::record::wait_for_record_waiter(&path, session);
            ticked.recv_timeout(std::time::Duration::from_secs(5))
                .expect("the single-thread runtime must progress while capture waits");
            crate::session_usage::record::persist_record_in(&path, &record).unwrap();
        });
        finish_headless_session(session, codesense_pty_core::agent_cli::AgentCli::Codex, "success").await;
        ticker.await.unwrap();
        writer.join().unwrap();
        assert_eq!(w.stored(session).expect("headless end must wait for its record").1.len(), 3);
    }

    #[tokio::test]
    async fn pbf210_a_run_outcome_before_the_record_write_stores_the_conversation() {
        use crate::session_conversation::{nth_uuid, TestWriter};
        let root = tempfile::tempdir().unwrap();
        let root = root.path().canonicalize().unwrap();
        let w = TestWriter::install(root.join("usage.sqlite"), root.join("conv"), root.join("home"));
        let session = "late-run-outcome";
        w.seed_claude(session, &nth_uuid(14));
        let store = crate::session_usage::UsageStore::open(&w.store).unwrap();
        let record = store.read_session_record(session).unwrap().unwrap();
        rusqlite::Connection::open(&w.store).unwrap()
            .execute("DELETE FROM session_record WHERE session_id = ?1", [session]).unwrap();
        let state = DashboardLaunchState::new(1);
        assert!(state.begin("late-story").unwrap());
        state.bind("late-story", session, None).unwrap();
        state.stamp_provenance("late-story", RunProvenance {
            run_id: "late-run-outcome".into(), story_id: "late-story".into(), ..Default::default()
        });
        let path = w.store.clone();
        let writer = std::thread::spawn(move || {
            crate::session_usage::record::wait_for_record_waiter(&path, session);
            crate::session_usage::record::persist_record_in(&path, &record).unwrap();
        });
        assert!(state.record_session_outcome_with(
            session,
            SessionOutcomeInput {
                status: "success".into(), summary: None, exit_code: None,
                source: crate::commands::sprint_run::run_store::OutcomeSource::Signal,
                label: "test.late-record",
            },
            || async { panic!("raw terminal was not opted in") },
            CapturedOutcomeDestination::TestArtifactDir(root.join("artifacts")),
        ).await);
        writer.join().unwrap();
        assert_eq!(w.stored(session).expect("run outcome must wait for its record").1.len(), 3);
    }

    /// Backstop for the behavior tests above and in `pty_host`: they drive the functions that store a
    /// conversation; this proves the end handlers still call them, so a new end path that skips
    /// them fails here.
    #[test]
    fn every_session_end_seam_asks_for_a_conversation_capture() {
        fn body_of<'a>(source: &'a str, signature: &str) -> &'a str {
            let start = source.find(signature).unwrap_or_else(|| panic!("{signature} moved"));
            let end = source[start..].find("\n}\n").map_or(source.len(), |e| start + e);
            &source[start..end]
        }
        fn each_followed_by(source: &str, anchor: &str, wanted: &str) -> usize {
            let mut count = 0;
            for (at, _) in source.match_indices(anchor) {
                let window = &source[at..(at + 260).min(source.len())];
                assert!(window.contains(wanted), "{anchor} at {at} is not followed by {wanted}");
                count += 1;
            }
            count
        }
        let pty_host = include_str!("pty_host.rs");
        let dashboard = include_str!("dashboard_launch.rs");
        // PTY exit and session removed.
        assert_eq!(
            each_followed_by(pty_host, "timing::note_session_end(", "session_conversation::note_end("),
            2
        );
        // A headless run's end.
        assert_eq!(
            // The headless run's end goes through the one function that stores its conversation.
            body_of(dashboard, "async fn run_headless_to_completion(").matches("finish_headless_session(").count(),
            1
        );
    }

    #[test]
    fn a_launch_without_prepared_facts_says_so_for_each_fact() {
        let owner = SessionOwner { principal: None, org: "acme".into(), team: Some("dev".into()) };
        let (state, snapshot) = bound_launch(owner);
        let owner = state.owner_for_session("sess-1");
        let r = record_from_launch("sess-1", &snapshot, owner.as_ref(), &Default::default());
        assert_eq!(r.cli, None);
        assert!(r.unavailable.contains_key("cli"));
        assert!(r.unavailable.contains_key("mcpAllow"));
        // What the registry holds still lands.
        assert_eq!(r.run_id.as_deref(), Some("agent-launch:r1"));
        assert_eq!(r.agent.as_deref(), Some("acme/dev/worker@3"));
    }

    #[test]
    fn an_org_with_no_team_keeps_its_org() {
        let owner = SessionOwner { principal: None, org: "acme".into(), team: None };
        let (state, snapshot) = bound_launch(owner);
        let owner = state.owner_for_session("sess-1");
        let r = record_from_launch("sess-1", &snapshot, owner.as_ref(), &launch_facts(&[]));
        assert_eq!(r.org.as_deref(), Some("acme"));
        assert_eq!(r.team, None);
        assert!(!r.unavailable.contains_key("org"));
        assert!(r.unavailable.contains_key("team"));
    }

    #[test]
    fn both_model_syntaxes_and_the_short_flag_are_read_by_the_one_parser() {
        for (args, model) in [
            (&["--model", "gpt-6"][..], Some("gpt-6")),
            (&["--model=gpt-6"][..], Some("gpt-6")),
            (&["-m", "gpt-6"][..], Some("gpt-6")),
            (&["--model="][..], None),
            (&["--other", "x"][..], None),
        ] {
            assert_eq!(launch_facts(args).model.as_deref(), model, "{args:?}");
        }
        assert_eq!(launch_facts(&["--session-id=abc"]).native_session_id.as_deref(), Some("abc"));
    }

    /// Every path that binds a managed launch writes its record through the one writer. A new
    /// launch path that skips it fails here.
    #[test]
    fn every_managed_launch_path_writes_its_record_through_the_one_writer() {
        fn body<'a>(source: &'a str, signature: &str) -> &'a str {
            let start = source.find(signature).unwrap_or_else(|| panic!("{signature} moved"));
            let end = source[start..].find("\n}\n").map_or(source.len(), |e| start + e);
            &source[start..end]
        }
        let dashboard = include_str!("dashboard_launch.rs");
        let mcp = include_str!("mcp_workspace.rs");
        for (name, text) in [
            ("renderer bind", body(dashboard, "pub async fn dashboard_launch_bind(")),
            ("headless launch", body(dashboard, "pub(crate) async fn launch_headless_session(")),
            ("MCP launch", body(mcp, "async fn spawn_and_bind(")),
        ] {
            assert!(text.contains("write_launch_record_for_app("), "{name} does not write a launch record");
        }
        // The renderer bind records the facts its prepare step staged, so that is where its
        // launch names the skills its manifest provides.
        for (name, text) in [
            ("headless launch", body(dashboard, "pub(crate) async fn launch_headless_session(")),
            ("MCP launch", body(mcp, "async fn spawn_and_bind(")),
            ("agent prepare (renderer bind)", include_str!("agents_launch.rs")),
        ] {
            assert!(
                text.contains(".with_provided_skills("),
                "{name} does not record the skills its manifest provides"
            );
        }
        let renderer = body(dashboard, "pub async fn dashboard_launch_bind(");
        assert!(renderer.find("write_launch_record_for_app(").unwrap()
            < renderer.find("bind_launch_async(").unwrap(),
            "register capture before the renderer bind yields to an early ending");
        // A `signal_done` closes the session's task through the shared outcome funnel.
        assert!(
            body(dashboard, "pub(crate) async fn record_signal_outcome<F, Fut>(")
                .contains("close_task_after_capture("),
            "record_signal_outcome does not close the task"
        );
        // Each path's facts carry the brief the task record is made from (the renderer's are
        // staged by `agents_launch` before the bind).
        for (name, text) in [
            ("renderer bind", include_str!("agents_launch.rs")),
            ("headless launch", body(dashboard, "pub(crate) async fn launch_headless_session(")),
            ("MCP launch", body(mcp, "async fn spawn_and_bind(")),
        ] {
            assert!(text.contains("TaskInputs::from_launch("), "{name} does not hand its brief to the task record");
        }
    }

    // ---- SV-CDE-30: a launch's task, through each entry path ----
    //
    // Every path hands the registry a bound launch and its facts to `compose_launch_write`,
    // whose result is what `write_launch_record` captures. These tests bind a launch the way
    // each path does and capture it into a real store.

    use crate::session_usage::task_capture::{
        capture_launch_task, Capture, RoutineBrief, TaskInputs,
    };

    include!("dashboard_launch/story_tests.rs");

    fn task_facts(brief: Option<&str>) -> crate::session_usage::record::LaunchFacts {
        launch_facts(&[]).with_task(TaskInputs {
            brief: brief.map(str::to_owned),
            ..Default::default()
        })
    }

    /// Bind `session` to `handle`, launched by `launched_by` (a handle or a session id), the
    /// way the registry records every launch.
    fn bind_launch(
        state: &DashboardLaunchState,
        handle: &str,
        session: &str,
        launched_by: Option<&str>,
        story: &str,
        provenance: Option<RunProvenance>,
    ) {
        let record = LaunchRecord {
            meta: LaunchMeta {
                launched_by: launched_by.map(str::to_owned),
                story: provenance.as_ref().is_none_or(|p| p.routine.is_none()).then(|| story.to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(state.begin_with_provenance(handle, provenance.clone(), record).unwrap());
        state.bind(handle, session, Some("codesense")).unwrap();
        state.restamp_launch(
            handle,
            Some(SessionOwner { principal: None, org: "acme".into(), team: Some("dev".into()) }),
        );
        if provenance.is_none() {
            state.stamp_provenance(
                handle,
                RunProvenance { run_id: format!("run-{session}"), story_id: story.into(), ..Default::default() },
            );
        }
    }

    /// What `write_launch_record` would capture for `session`, written to `store`.
    fn capture_bound(
        state: &DashboardLaunchState,
        store: &crate::session_usage::UsageStore,
        handle: &str,
        session: &str,
        facts: crate::session_usage::record::LaunchFacts,
    ) -> crate::session_usage::task::TaskRecord {
        let write = compose_launch_write(state, session, handle, facts).expect("a bound launch");
        store.write_session_record(&write.record).unwrap();
        capture_launch_task(
            store,
            &Capture {
                session_id: session,
                record: &write.record,
                inputs: &write.task,
                routine: write.routine.as_ref(),
                launcher_task: write.launcher.as_deref(),
                story_file: None,
                now_ms: 1,
            },
        )
        .unwrap()
    }

    #[test]
    fn the_renderer_bind_makes_the_task_from_the_brief_its_launch_staged() {
        let (_dir, store) = crate::session_usage::tests::store();
        let state = DashboardLaunchState::new(4);
        // `agents_launch` stages the facts, brief included, before the renderer binds.
        state.stage_agent_launch(
            "sess-r",
            StagedAgentLaunch {
                queue_role: None,
                agent_revision: "acme/dev/worker@3".into(),
                settings: None,
                capture_raw_terminal: false,
                cwd: None,
                binary: None,
                facts: Some(task_facts(Some("Task-Type: ops\nRotate the signing keys."))),
            },
        );
        bind_launch(&state, "SV-1", "sess-r", None, "SV-1", None);
        let facts = state.staged_launch_facts("sess-r").unwrap();
        let t = capture_bound(&state, &store, "SV-1", "sess-r", facts);
        assert_eq!(t.task_id, "sess-r");
        assert_eq!((t.goal.as_str(), t.task_type.as_str()), ("Rotate the signing keys.", "ops"));
        assert_eq!((t.parent_id.as_deref(), t.parent_source.as_deref()), (Some("story:SV-1"), Some("story")));
        assert_eq!(store.task_links("sess-r").unwrap()[0].target, "sess-r");
    }

    #[test]
    fn an_mcp_launch_by_an_agent_is_a_child_of_that_agents_task() {
        let (_dir, store) = crate::session_usage::tests::store();
        let state = DashboardLaunchState::new(4);
        bind_launch(&state, "lead-handle", "lead-tab", None, "SV-1", None);
        let lead = capture_bound(&state, &store, "lead-handle", "lead-tab", task_facts(Some("Task-Type: feature\nLead SV-1.")));
        // `open_workspace` records the launcher as the caller's session id; the registry
        // normalizes it to the launcher's handle.
        bind_launch(&state, "SV-1#dev", "dev-tab", Some("lead-tab"), "SV-1", None);
        let dev = capture_bound(&state, &store, "SV-1#dev", "dev-tab", task_facts(Some("Build the store.")));
        assert_eq!((dev.parent_id.as_deref(), dev.parent_source.as_deref()), (Some(lead.task_id.as_str()), Some("launcher")));
        assert_eq!(dev.goal, "Build the store.");
        let tree: Vec<String> = store.task_subtree("lead-tab").unwrap().into_iter().map(|t| t.task_id).collect();
        assert_eq!(tree, ["lead-tab", "dev-tab"]);
    }

    #[test]
    fn a_worker_spawned_through_wv_spawn_worker_is_a_child_of_the_verified_lead() {
        use crate::weft::WorkerSubclass;
        let (_dir, store) = crate::session_usage::tests::store();
        let state = DashboardLaunchState::new(4);
        bind_launch(&state, "lead-handle", "lead-tab", None, "SV-1", None);
        capture_bound(&state, &store, "lead-handle", "lead-tab", task_facts(Some("Lead SV-1.")));
        // The handler builds the request from its verified connection. A `launched_by` in the
        // inputs is ignored: the inputs have no such field.
        let params: crate::mcp::handler::WvSpawnWorkerParams = serde_json::from_value(serde_json::json!({
            "story_id": "SV-1", "subclass": "impl", "initial_prompt": "Build it.",
            "launched_by": "forged-tab", "launchedBy": "forged-tab"
        }))
        .unwrap();
        let verified = params.into_request(WorkerSubclass::Impl, None, "root".into(), Some("lead-tab".into()));
        assert_eq!(verified.launched_by.as_deref(), Some("lead-tab"));
        let unbound: crate::mcp::handler::WvSpawnWorkerParams = serde_json::from_value(serde_json::json!({
            "story_id": "SV-1", "subclass": "impl", "launched_by": "forged-tab"
        }))
        .unwrap();
        assert_eq!(unbound.into_request(WorkerSubclass::Impl, None, "root".into(), None).launched_by, None);
        // The launch carries the verified session as its launcher.
        bind_launch(&state, "SV-1", "worker-tab", verified.launched_by.as_deref(), "SV-1", None);
        let worker = capture_bound(&state, &store, "SV-1", "worker-tab", task_facts(verified.initial_prompt.as_deref()));
        assert_eq!((worker.parent_id.as_deref(), worker.parent_source.as_deref()), (Some("lead-tab"), Some("launcher")));
        assert_eq!(worker.story_id.as_deref(), Some(verified.story_id.as_str()));
        assert_eq!(store.read_session_record("worker-tab").unwrap().unwrap().story_id.as_deref(), Some(verified.story_id.as_str()));
        // A forged id that no session or task has never becomes a parent.
        bind_launch(&state, "SV-1#other", "other-tab", Some("forged-tab"), "SV-1", None);
        let other = capture_bound(&state, &store, "SV-1#other", "other-tab", task_facts(Some("Other.")));
        assert_eq!((other.parent_id.as_deref(), other.parent_source.as_deref()), (Some("story:SV-1"), Some("story")));
        assert!(other.unavailable["parent"].contains("launched by a person"));
    }

    #[test]
    fn a_child_captured_before_its_launcher_still_has_the_launcher_as_parent_after_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let open = || crate::session_usage::UsageStore::open(&path).unwrap();
        let state = DashboardLaunchState::new(4);
        bind_launch(&state, "lead-handle", "lead-tab", None, "SV-1", None);
        bind_launch(&state, "SV-1#dev", "dev-tab", Some("lead-tab"), "SV-1", None);
        // The child's records are written first: the launcher has neither a session record nor
        // a task yet, and the parent is still the launcher, as the launch resolved it.
        let dev = capture_bound(&state, &open(), "SV-1#dev", "dev-tab", task_facts(Some("Build.")));
        assert_eq!((dev.parent_id.as_deref(), dev.parent_source.as_deref()), (Some("lead-tab"), Some("launcher")));
        assert!(open().read_task("lead-tab").unwrap().is_none());
        capture_bound(&state, &open(), "lead-handle", "lead-tab", task_facts(Some("Task-Type: feature\nLead.")));
        let tree: Vec<String> = open().task_subtree("lead-tab").unwrap().into_iter().map(|t| t.task_id).collect();
        assert_eq!(tree, ["lead-tab", "dev-tab"]);
    }

    #[test]
    fn a_session_launched_by_a_routine_session_is_a_child_of_the_routine_runs_task() {
        let (_dir, store) = crate::session_usage::tests::store();
        let state = DashboardLaunchState::new(4);
        let handle = "routine:acme/dev/digest@run-3";
        let provenance = RunProvenance {
            run_id: "run-3".into(),
            story_id: handle.into(),
            routine_key: Some("acme/dev/digest".into()),
            routine: Some(RoutineBrief { name: "Digest".into(), task: "Summarise.".into() }),
            ..Default::default()
        };
        bind_launch(&state, handle, "routine-tab", None, handle, Some(provenance));
        bind_launch(&state, "SV-1#w", "w-tab", Some("routine-tab"), "SV-1", None);
        let w = capture_bound(&state, &store, "SV-1#w", "w-tab", task_facts(Some("Sub-job.")));
        assert_eq!((w.parent_id.as_deref(), w.parent_source.as_deref()), (Some("run-3"), Some("launcher")));
    }

    #[test]
    fn a_reserved_child_keeps_its_attested_parent_when_the_launcher_handle_is_reused_before_bind() {
        for caller in ["dev-1", "SV-1#dev"] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("usage.sqlite");
            let open = || crate::session_usage::UsageStore::open(&path).unwrap();
            let state = DashboardLaunchState::new(4);
            bind_launch(&state, "SV-1#dev", "dev-1", None, "SV-1", None);
            capture_bound(&state, &open(), "SV-1#dev", "dev-1", task_facts(Some("Original lead.")));
            assert!(state.begin_with_provenance("SV-1#worker", None, LaunchRecord {
                meta: LaunchMeta { launched_by: Some(caller.into()), ..Default::default() },
                ..Default::default()
            }).unwrap());
            // Async launch preparation runs while the user closes and reopens the launcher.
            state.release_for_session("dev-1");
            bind_launch(&state, "SV-1#dev", "dev-2", None, "SV-1", None);
            capture_bound(&state, &open(), "SV-1#dev", "dev-2", task_facts(Some("Replacement lead.")));
            assert!(!state.begin_with_provenance("SV-1#worker", None, LaunchRecord {
                meta: LaunchMeta { launched_by: Some("dev-2".into()), ..Default::default() },
                ..Default::default()
            }).unwrap(), "idempotent reservation preserves identity");
            state.bind("SV-1#worker", "worker", Some("codesense")).unwrap();
            capture_bound(&state, &open(), "SV-1#worker", "worker", task_facts(Some("Work.")));
            let child = open().read_task("worker").unwrap().unwrap();
            assert_eq!((child.parent_id.as_deref(), child.parent_source.as_deref()),
                (Some("dev-1"), Some("launcher")), "{caller}");
            assert_eq!(open().task_subtree("dev-1").unwrap().len(), 2);
        }
    }

    #[test]
    fn a_reused_launcher_handle_does_not_reparent_the_first_workers_task() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.sqlite");
        let open = || crate::session_usage::UsageStore::open(&path).unwrap();
        let state = DashboardLaunchState::new(4);
        // `SV-1#dev` is held by one session, which launches a worker.
        bind_launch(&state, "SV-1#dev", "dev-1", None, "SV-1", None);
        capture_bound(&state, &open(), "SV-1#dev", "dev-1", task_facts(Some("First lead.")));
        bind_launch(&state, "SV-1#w1", "w-1", Some("SV-1#dev"), "SV-1", None);
        capture_bound(&state, &open(), "SV-1#w1", "w-1", task_facts(Some("First worker.")));
        // The handle is reopened as a new session, which launches another worker.
        state.release_for_session("dev-1");
        bind_launch(&state, "SV-1#dev", "dev-2", None, "SV-1", None);
        capture_bound(&state, &open(), "SV-1#dev", "dev-2", task_facts(Some("Second lead.")));
        bind_launch(&state, "SV-1#w2", "w-2", Some("SV-1#dev"), "SV-1", None);
        capture_bound(&state, &open(), "SV-1#w2", "w-2", task_facts(Some("Second worker.")));
        let parent = |id: &str| open().read_task(id).unwrap().unwrap().parent_id;
        assert_eq!(parent("w-1").as_deref(), Some("dev-1"));
        assert_eq!(parent("w-2").as_deref(), Some("dev-2"));
        // A reopen of the store re-resolves nothing.
        assert_eq!(parent("w-1").as_deref(), Some("dev-1"));
    }

    #[test]
    fn a_headless_launch_makes_its_task_from_the_run_prompt() {
        let (_dir, store) = crate::session_usage::tests::store();
        let state = DashboardLaunchState::new(4);
        bind_launch(&state, "SV-2", "head-1", None, "SV-2", None);
        state.mark_headless("SV-2", Default::default(), Default::default(), Default::default());
        let t = capture_bound(&state, &store, "SV-2", "head-1", task_facts(Some("Task-Type: bug fix\nFix the flicker on close.")));
        assert_eq!((t.goal.as_str(), t.task_type.as_str()), ("Fix the flicker on close.", "bugfix"));
        assert_eq!(t.story_id.as_deref(), Some("SV-2"));
    }

    #[test]
    fn a_routine_run_visible_or_headless_has_the_run_id_as_its_task_and_no_parent() {
        for visible in [true, false] {
            let (_dir, store) = crate::session_usage::tests::store();
            let state = DashboardLaunchState::new(4);
            let handle = format!("routine:acme/dev/digest@run-{visible}");
            let provenance = RunProvenance {
                run_id: format!("run-{visible}"),
                story_id: handle.clone(),
                routine_key: Some("acme/dev/digest".into()),
                routine: Some(RoutineBrief { name: "Nightly digest".into(), task: "Summarise merged PRs.\nPost it.".into() }),
                ..Default::default()
            };
            bind_launch(&state, &handle, "routine-tab", None, &handle, Some(provenance));
            let t = capture_bound(&state, &store, &handle, "routine-tab", task_facts(Some("fired prompt")));
            assert_eq!(t.task_id, format!("run-{visible}"), "visible: {visible}");
            assert_eq!(t.goal, "Nightly digest: Summarise merged PRs.");
            assert_eq!((t.task_type.as_str(), t.parent_id, t.story_id), ("routine", None, None));
        }
    }

    /// An app-opened terminal is registered but is an ordinary shell: an agent typed into it by
    /// hand is a hand-started CLI, and a launch is not.
    #[test]
    fn a_registered_terminal_is_not_an_agent_launch_and_a_launch_is() {
        let state = DashboardLaunchState::new(4);
        state.register("epic-term", "term-1", Some("codesense"));
        assert!(!state.is_agent_launch("term-1"));
        assert!(!state.is_agent_launch("nobody"));
        let owner = SessionOwner { principal: None, org: "acme".into(), team: Some("dev".into()) };
        state.restamp_launch("epic-term", Some(owner));
        assert_eq!(state.terminal_owner("term-1"), (Some("acme".into()), Some("dev".into())));
        let (launch, _) = bound_launch(SessionOwner { principal: None, org: "acme".into(), team: None });
        launch.set_agent_revision_if_absent("SV-1", "acme/dev/worker@3".into());
        assert!(launch.is_agent_launch("sess-1"));
    }

    #[test]
    fn a_session_reports_its_launch_profile_and_routine() {
        let state = DashboardLaunchState::new(4);
        assert!(state.begin("routine:k@r").unwrap());
        state.bind("routine:k@r", "sess-1", None).unwrap();
        assert_eq!(state.profile_id_for_session("sess-1"), None);
        assert_eq!(state.routine_key_for_session("sess-1"), None);

        state.set_profile_id_if_absent("routine:k@r", "reviewer".into());
        // A later focus with another profile does not relabel the live session.
        state.set_profile_id_if_absent("routine:k@r", "dev".into());
        state.stamp_provenance(
            "routine:k@r",
            RunProvenance {
                run_id: "r".into(),
                story_id: "routine:k@r".into(),
                routine_key: Some("acme/api/backend/k".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            state.profile_id_for_session("sess-1").as_deref(),
            Some("reviewer")
        );
        assert_eq!(
            state.routine_key_for_session("sess-1").as_deref(),
            Some("acme/api/backend/k")
        );
        assert_eq!(state.profile_id_for_session("nobody"), None);

        assert_eq!(state.owner_for_session("sess-1"), None);
        let owner = |team: &str| SessionOwner {
            principal: Some("1".into()),
            org: "acme".into(),
            team: Some(team.into()),
        };
        state.set_owner_if_absent("routine:k@r", owner("backend"));
        // A later focus from another team does not move the live session.
        state.set_owner_if_absent("routine:k@r", owner("frontend"));
        assert_eq!(state.owner_for_session("sess-1"), Some(owner("backend")));
        assert_eq!(state.owner_for_session("nobody"), None);
    }

    #[test]
    fn stamp_provenance_on_unknown_handle_is_a_noop() {
        let state = DashboardLaunchState::new(4);
        state.stamp_provenance(
            "ghost",
            RunProvenance {
                run_id: "r".into(),
                story_id: "ghost".into(),
                automation_id: None,
                ..Default::default()
            },
        );
        assert!(
            state.snapshot().is_empty(),
            "no entry created for an unknown handle"
        );
    }

    #[test]
    fn provenance_serializes_camel_case_and_omits_absent_automation() {
        // The FE reads { runId, storyId } and, when present, automationId — omitted (not
        // null) for a bare sprint run so the shape stays tidy.
        let bare = RunProvenance {
            run_id: "run-1".into(),
            story_id: "SW-SR-3".into(),
            automation_id: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&bare).unwrap();
        assert!(json.contains("\"runId\":\"run-1\""));
        assert!(json.contains("\"storyId\":\"SW-SR-3\""));
        assert!(
            !json.contains("automationId"),
            "absent automation is omitted: {json}"
        );

        let automated = RunProvenance {
            run_id: "run-2".into(),
            story_id: "SW-SR-3".into(),
            automation_id: Some("auto-9".into()),
            ..Default::default()
        };
        assert!(serde_json::to_string(&automated)
            .unwrap()
            .contains("\"automationId\":\"auto-9\""));
    }

    #[test]
    fn retain_alive_empty_probe_returns_nothing() {
        let got = retain_alive(
            vec!["a".into(), "b".into()],
            &crate::commands::pty_backend::Liveness::Known(std::collections::HashSet::new()),
        );
        assert!(
            got.is_empty(),
            "no session probed alive → empty (never claims a dead session live)"
        );
    }

    // ── SW-BA-1: status vocabulary + derivation (AC-1/AC-2/AC-3) ─────────────

    #[test]
    fn derive_status_covers_the_permutation_table() {
        // Reserved-but-unbound (no session) → errored, regardless of the other
        // inputs (the leak the panel exists to reveal).
        assert_eq!(derive_status(false, None, false), AgentStatus::Errored);
        assert_eq!(derive_status(true, None, true), AgentStatus::Errored);
        // Bound + live splits on the gate bit.
        assert_eq!(derive_status(true, Some("s"), false), AgentStatus::Running);
        assert_eq!(derive_status(true, Some("s"), true), AgentStatus::Paused);
        // Bound + exited → done (a crash degrades to done, OQ-1 — never guessed).
        assert_eq!(derive_status(false, Some("s"), false), AgentStatus::Done);
        assert_eq!(derive_status(false, Some("s"), true), AgentStatus::Done);
    }

    #[test]
    fn agent_status_serializes_camelcase() {
        // The FE `AgentStatus` union depends on this exact wire spelling.
        assert_eq!(
            serde_json::to_string(&AgentStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::Paused).unwrap(),
            "\"paused\""
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::Done).unwrap(),
            "\"done\""
        );
        assert_eq!(
            serde_json::to_string(&AgentStatus::Errored).unwrap(),
            "\"errored\""
        );
    }

    #[test]
    fn session_row_serializes_status_field() {
        let row = SessionRow {
            output_run_id: None,
            handle: "SW-A".to_string(),
            session_id: Some("sess-1".to_string()),
            kind: KIND_DASHBOARD_LAUNCHED.to_string(),
            alive: true,
            status: derive_status(true, Some("sess-1"), false),
            backgrounded: false,
            started_with: None,
            started_at_ms: None,
            launcher: None,
            account_provenance: None,
            provenance: None,
            last_output_ms: None,
            meta: LaunchMeta::default(),
            workspace: None,
            chat_workspace_id: None,
            group_unread: 0,
            group_read_id: 0,
            agent_revision: None,
            deadline_at_ms: None,
            expiring: false,
            machine_id: None,
            launch_team_key: None,
        };
        let v: serde_json::Value = serde_json::to_value(&row).unwrap();
        // camelCase key, running spelling — the panel/header read `row.status`.
        assert_eq!(v["status"], serde_json::json!("running"));
        // `alive` is retained alongside `status` (back-compat, AC-3/OQ-3).
        assert_eq!(v["alive"], serde_json::json!(true));
    }

    /// The roster row and `list_sessions` read the same launch metadata, so an agent's
    /// launcher reads identically on both: the launcher's handle when it has one (even
    /// when the caller reported its session id), and an explicit null for a person.
    #[test]
    fn roster_row_launched_by_matches_list_sessions() {
        let state = DashboardLaunchState::new(4);
        assert!(state.begin("MANAGER").unwrap());
        state.bind("MANAGER", "sess-manager", Some("mcp")).unwrap();
        assert!(state
            .begin_with_provenance(
                "WORKER",
                None,
                launch_record("Worker", Some("sess-manager"), "/dev")
            )
            .unwrap());

        let rows: Vec<SessionRow> = state
            .snapshot()
            .into_iter()
            .map(|s| session_row(s, true))
            .collect();
        let wire: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| serde_json::to_value(r).unwrap())
            .collect();
        let roster_launched_by = |handle: &str| {
            wire.iter()
                .find(|r| r["handle"] == handle)
                .map(|r| r["launchedBy"].clone())
                .unwrap()
        };
        assert_eq!(roster_launched_by("WORKER"), serde_json::json!("MANAGER"));
        assert_eq!(roster_launched_by("MANAGER"), serde_json::Value::Null);

        let listed = crate::mcp::session_list::session_list_rows(rows, |_| None, None);
        for row in listed {
            let expected = row
                .launched_by
                .map_or(serde_json::Value::Null, serde_json::Value::from);
            assert_eq!(roster_launched_by(&row.handle), expected, "{}", row.handle);
        }
    }

    /// Every key the roster row puts on the wire is declared on the FE's
    /// `AgentSessionRow`, so a field added here cannot silently go unread there.
    #[test]
    fn session_row_wire_keys_are_declared_on_agent_session_row() {
        const TS: &str = include_str!("../../../src/tauri/commands.ts");
        let start = TS
            .find("export interface AgentSessionRow {")
            .expect("AgentSessionRow in commands.ts");
        let body = &TS[start..start + TS[start..].find("\n}").expect("interface ends")];
        let row = session_row(
            SessionSnapshot {
                output_run_id: None,
                handle: "H".into(),
                session_id: None,
                launcher_task: None,
                story_was_explicit: false,
                kind: KIND_DASHBOARD_LAUNCHED,
                backgrounded: false,
                started_with: None,
                started_at_ms: None,
                launcher: None,
                account_provenance: None,
                provenance: None,
                last_output_ms: None,
                meta: LaunchMeta::default(),
                worktree_dir: None,
                agent_revision: None,
                deadline_at_ms: None,
                exit_sent_at_ms: None,
                machine_id: None,
                launch_team_key: None,
            },
            false,
        );
        let wire = serde_json::to_value(row).unwrap();
        let keys = wire.as_object().unwrap().keys();
        let undeclared: Vec<&String> = keys
            .filter(|k| {
                !body.contains(&format!("\n  {k}: ")) && !body.contains(&format!("\n  {k}?: "))
            })
            .collect();
        assert!(
            undeclared.is_empty(),
            "undeclared on AgentSessionRow: {undeclared:?}"
        );
        assert!(wire.get("launchedBy").is_some());
    }

    /// A remote tab's launch record on `machine` in team `acme:design`, under `session`, at
    /// admission revision `revision`; with `agent`, a saved Agent's launch that an agent
    /// started (its roster row is built by [`remote_agent_row`]).
    fn roster_remote_record(
        handle: &str,
        session: &str,
        revision: &str,
        agent: bool,
    ) -> crate::commands::pty_layout::RemoteShellRecord {
        let mut record = serde_json::json!({
            "handle": handle, "owner_id": 41,
            "request_id": "00000000-0000-4000-8000-000000000001",
            "machine_id": "droplet", "team_key": "acme:design", "workspace_id": "w",
            "workspace": "acme:design|w", "cwd": "/home/dev", "name": null, "launch": "claude",
            "target": {"member":{"machineId":"droplet","slot":"remote"},"admissionRevision":revision,
                "namespace":{"serverId":"s","epoch":"e","generation":"1"}, "session": session}
        });
        if agent {
            record["agent"] = serde_json::json!({
                "workspace_handle": "ws-1", "name": "Worker",
                "manifest": {
                    "agent": {"key": "acme/design/worker", "revision": 1, "definition_hash": "h"},
                    "runtime": "claude", "entries": [], "essentials": [], "generated": [],
                    "totals": {"files": 0, "bytes": 0}, "errors": [], "manifest_hash": ""
                },
                "runtime": "claude",
                "startup": {"args": [], "settings": {}, "prompt": null},
                "mcp_allow": []
            });
            record["identity"] = serde_json::json!({"launched_by": "SW-1#lead"});
        }
        serde_json::from_value(record).unwrap()
    }

    /// The registry's roster rows on the wire, as `list_sessions_inner` builds them: a
    /// remote agent's through [`remote_agent_row`] with its session on the machine.
    #[test]
    fn saved_agent_and_mcp_story_bindings_share_one_room_without_changing_provenance() {
        use codesense_pty_core::layout::WorkspaceDescriptor;
        let state = DashboardLaunchState::new(4);
        state.begin("saved").unwrap();
        state.bind("saved", "saved-session", None).unwrap();
        state.set_worktree_dir_if_absent("saved", "/work/fix".into());
        state.bind_chat_descriptor(
            "saved",
            "saved-session",
            &WorkspaceDescriptor {
                kind: "story".into(),
                story_id: Some("SV-X".into()),
                worktree_path: Some("/work/fix".into()),
                ..Default::default()
            },
        );
        state
            .begin_with_provenance(
                "mcp",
                None,
                LaunchRecord {
                    meta: LaunchMeta {
                        story: Some("SV-X".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        state.bind("mcp", "mcp-session", None).unwrap();
        assert_eq!(
            state.workspace_for_session("saved-session"),
            state.workspace_for_session("mcp-session")
        );
        assert!(state
            .snapshot()
            .into_iter()
            .find(|r| r.handle == "saved")
            .unwrap()
            .meta
            .story
            .is_none());
        state.register("root", "root-session", None);
        state.bind_chat_descriptor(
            "root",
            "root-session",
            &WorkspaceDescriptor {
                kind: "root".into(),
                worktree_path: Some("/repo".into()),
                ..Default::default()
            },
        );
        assert_eq!(state.workspace_for_session("root-session"), None);
    }

    #[test]
    fn a_remote_launch_has_a_startup_clock_a_candidate_entry_and_one_stamp() {
        let state = DashboardLaunchState::new(4);
        state
            .reserve_remote_shell(roster_remote_record("remote", "remote-session", "1", true))
            .unwrap();
        let (clock, ready) = state.readiness("remote-session").unwrap();
        assert!(clock.is_some() && ready.is_none());
        let candidates = state.startup_candidates();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].handle, "remote");
        assert_eq!(candidates[0].session_id, "remote-session");
        assert_eq!(candidates[0].launched_by.as_deref(), Some("SW-1#lead"));
        // The first observation stamps it once and ends the candidacy.
        state.mark_ready("remote-session");
        let stamped = state.readiness("remote-session").unwrap().1;
        assert!(stamped.is_some());
        state.mark_ready("remote-session");
        assert_eq!(state.readiness("remote-session").unwrap().1, stamped);
        assert!(state.startup_candidates().is_empty());
    }

    #[test]
    fn a_raw_remote_launch_keeps_its_workspace_team_as_the_startup_notes_team() {
        // A tab a person opened on a machine records no launch team in its metadata.
        let state = DashboardLaunchState::new(4);
        state
            .reserve_remote_shell(roster_remote_record("raw", "raw-session", "1", false))
            .unwrap();
        let candidate = state.startup_candidates().remove(0);
        assert_eq!(
            candidate.team,
            Some(LaunchTeam {
                org: "acme".into(),
                team: "design".into()
            })
        );
        assert_eq!(candidate.team, state.launch_team("raw"));
    }

    #[test]
    fn a_remote_startup_note_completes_only_for_its_own_session() {
        let state = DashboardLaunchState::new(4);
        state
            .reserve_remote_shell(roster_remote_record("remote", "remote-session", "1", true))
            .unwrap();
        let mut stale = state.startup_candidates().remove(0);
        stale.session_id = "an-older-session".into();
        state.complete_startup(&stale);
        assert_eq!(state.startup_candidates().len(), 1);
        state.complete_startup(&state.startup_candidates()[0]);
        assert!(state.startup_candidates().is_empty());
    }

    #[test]
    fn remote_sessions_share_their_recorded_workspace_binding() {
        let state = DashboardLaunchState::new(4);
        let mut record = roster_remote_record("remote", "remote-session", "1", true);
        record.workspace_id = "sv-x-fix-login".into();
        record.workspace_story_id = Some("SV-X".into());
        state.reserve_remote_shell(record).unwrap();
        assert_eq!(
            state.workspace_for_session("remote-session").as_deref(),
            Some("story:SV-X")
        );
        state
            .begin_with_provenance(
                "local",
                None,
                LaunchRecord {
                    meta: LaunchMeta {
                        story: Some("SV-X".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        state.bind("local", "local-session", None).unwrap();
        assert_eq!(
            state.workspace_for_session("local-session"),
            state.workspace_for_session("remote-session")
        );
        let mut other = roster_remote_record("nonstory", "nonstory-session", "1", false);
        other.request_id = uuid::Uuid::new_v4();
        other.workspace_id = "story:SV-X".into();
        state.reserve_remote_shell(other).unwrap();
        assert_eq!(
            state.workspace_for_session("nonstory-session").as_deref(),
            Some("remote:story:SV-X")
        );
    }

    fn roster_wire(state: &DashboardLaunchState) -> Vec<serde_json::Value> {
        let remote_sessions = state.remote_agent_sessions();
        state
            .snapshot()
            .into_iter()
            .map(|s| match remote_sessions.get(&s.handle).cloned() {
                Some(session) => remote_agent_row(s, true, Some(session)),
                None => session_row(s, true),
            })
            .map(|row| serde_json::to_value(row).unwrap())
            .collect()
    }

    fn wire_row<'a>(wire: &'a [serde_json::Value], handle: &str) -> &'a serde_json::Value {
        wire.iter().find(|r| r["handle"] == handle).unwrap()
    }

    /// A remote launch's roster row names its machine and the team it was launched in
    /// from the launch record, before the machine's layout shows its tab; a local
    /// session's row carries both keys, null.
    #[test]
    fn roster_row_carries_the_machine_of_a_remote_launch() {
        use serde_json::{json, Value};
        let state = DashboardLaunchState::new(4);
        state
            .reserve_remote_shell(roster_remote_record(
                "remote-tab-a",
                "remote-shell-a",
                "1",
                false,
            ))
            .unwrap();
        state.register("local#1", "sess-local", Some("codesense"));

        let wire = roster_wire(&state);
        let remote = wire_row(&wire, "remote-tab-a");
        assert_eq!(remote.get("machineId"), Some(&json!("droplet")));
        assert_eq!(remote.get("launchTeamKey"), Some(&json!("acme:design")));
        let local = wire_row(&wire, "local#1");
        assert_eq!(local.get("machineId"), Some(&Value::Null), "present, null");
        assert_eq!(
            local.get("launchTeamKey"),
            Some(&Value::Null),
            "present, null"
        );

        // Re-admitted on a reconnect, its row still names its machine and team.
        state.update_remote_shell(roster_remote_record(
            "remote-tab-a",
            "remote-shell-a",
            "2",
            false,
        ));
        let wire = roster_wire(&state);
        let remote = wire_row(&wire, "remote-tab-a");
        assert_eq!(remote.get("machineId"), Some(&json!("droplet")));
        assert_eq!(remote.get("launchTeamKey"), Some(&json!("acme:design")));
    }

    /// An agent's launch on a machine lists through [`remote_agent_row`] under its session
    /// there, and its row still names its machine and team, re-admitted too.
    #[test]
    fn a_remote_agents_roster_row_carries_its_machine_and_team() {
        use serde_json::json;
        let state = DashboardLaunchState::new(4);
        state
            .reserve_remote_shell(roster_remote_record(
                "remote-agent-a",
                "agent-session",
                "1",
                true,
            ))
            .unwrap();
        assert_eq!(
            state.remote_agents(),
            vec![("remote-agent-a".to_string(), "droplet".to_string())]
        );
        for revision in ["1", "2"] {
            state.update_remote_shell(roster_remote_record(
                "remote-agent-a",
                "agent-session",
                revision,
                true,
            ));
            let wire = roster_wire(&state);
            let row = wire_row(&wire, "remote-agent-a");
            assert_eq!(row.get("sessionId"), Some(&json!("agent-session")));
            assert_eq!(row.get("machineId"), Some(&json!("droplet")));
            assert_eq!(row.get("launchTeamKey"), Some(&json!("acme:design")));
        }
    }

    // ── SW-AV-1: union enumerator seam (registry ∪ live-PTY) ─────────────────

    /// A registry SessionRow as `list_sessions_inner` has already probed it (the
    /// registry arm hands the union seam liveness-derived rows).
    fn registry_row(kind: &'static str, session_id: &str, alive: bool) -> SessionRow {
        SessionRow {
            output_run_id: None,
            handle: format!("H:{session_id}"),
            session_id: Some(session_id.to_string()),
            kind: kind.to_string(),
            alive,
            status: derive_status(alive, Some(session_id), false),
            backgrounded: false,
            started_with: None,
            started_at_ms: None,
            launcher: None,
            account_provenance: None,
            provenance: None,
            last_output_ms: None,
            meta: LaunchMeta::default(),
            workspace: None,
            chat_workspace_id: None,
            group_unread: 0,
            group_read_id: 0,
            agent_revision: None,
            deadline_at_ms: None,
            expiring: false,
            machine_id: None,
            launch_team_key: None,
        }
    }

    /// SW-AV-1 (INV-3) SEAM: the union folds a registry-only headless agent and a
    /// registry-less live-PTY session into ONE roster, deduped by `session_id` — the
    /// two live ones both appear exactly once, and a session that is BOTH registered
    /// and live is not double-counted. (The "exited-but-replayed does NOT appear as
    /// live" half is proven at the source, `PtyState::live_session_ids` — see the
    /// pty-core `live_session_ids_reports_alive_non_pool_only` test — since the live
    /// arm here is exactly that already-filtered set.)
    #[test]
    fn union_folds_registry_and_live_pty_deduped() {
        // Registry arm: one headless agent (registry-authoritative, no PTY) + one
        // dashboard-launched PTY agent that is ALSO alive in the live-PTY set.
        let registry_rows = vec![
            registry_row(KIND_HEADLESS, "headless-1", true),
            registry_row(KIND_DASHBOARD_LAUNCHED, "shell-dash", true),
        ];
        // Live-PTY arm: the registered PTY agent (dedup target) + a registry-less
        // root/notes shell the register-on-open ledger missed.
        let live = Ok(std::collections::HashSet::from([
            "shell-dash".to_string(),
            "shell-root".to_string(),
        ]));

        let rows = union_registry_and_live_pty(registry_rows, live).expect("no enum error");

        // Every session id appears exactly once.
        let ids: Vec<&str> = rows
            .iter()
            .filter_map(|r| r.session_id.as_deref())
            .collect();
        assert_eq!(ids.len(), 3, "3 distinct sessions, none duplicated");
        assert!(ids.contains(&"headless-1"));
        assert!(ids.contains(&"shell-dash"));
        assert!(ids.contains(&"shell-root"));

        // The registry arm stays authoritative for the shared id: `shell-dash`
        // keeps its registry kind, NOT a synthetic live-PTY kind.
        let dash = rows
            .iter()
            .find(|r| r.session_id.as_deref() == Some("shell-dash"))
            .unwrap();
        assert_eq!(dash.kind, KIND_DASHBOARD_LAUNCHED);

        // The registry-less live session became a synthetic, alive live-PTY row.
        let root = rows
            .iter()
            .find(|r| r.session_id.as_deref() == Some("shell-root"))
            .unwrap();
        assert_eq!(root.kind, KIND_LIVE_PTY);
        assert!(root.alive);
        assert_eq!(root.status, AgentStatus::Running);
        assert_eq!(root.handle, "shell-root");
    }

    /// A remote session the live-PTY arm also reports keeps its registry row, and with it
    /// its machine: a live-PTY row never stands in for it, and one of its own reads null.
    #[test]
    fn union_never_lets_a_live_pty_row_override_a_remote_rows_machine() {
        use serde_json::{json, Value};
        let mut remote = registry_row(KIND_DASHBOARD_LAUNCHED, "remote-shell-a", true);
        remote.machine_id = Some("droplet".into());
        remote.launch_team_key = Some("acme:design".into());
        let live = Ok(std::collections::HashSet::from([
            "remote-shell-a".to_string(),
            "shell-root".to_string(),
        ]));

        let rows = union_registry_and_live_pty(vec![remote], live).expect("no enum error");

        let wire: Vec<Value> = rows
            .iter()
            .map(|r| serde_json::to_value(r).unwrap())
            .collect();
        let of =
            |id: &str| -> Vec<&Value> { wire.iter().filter(|r| r["sessionId"] == id).collect() };
        let remote = of("remote-shell-a");
        assert_eq!(remote.len(), 1, "one row for the remote session");
        assert_eq!(remote[0].get("machineId"), Some(&json!("droplet")));
        assert_eq!(remote[0]["kind"], KIND_DASHBOARD_LAUNCHED);
        let root = of("shell-root");
        assert_eq!(root.len(), 1);
        assert_eq!(root[0]["kind"], KIND_LIVE_PTY);
        assert_eq!(root[0].get("machineId"), Some(&Value::Null));
    }

    /// SW-AV-1 (INV-3) SEAM: an enumeration failure is SURFACED as `Err`, never
    /// swallowed into an empty or registry-only list — the roster must not silently
    /// drop the whole live-PTY arm (the original swallowed-`.catch(()=>{})` bug).
    #[test]
    fn union_surfaces_enumeration_error() {
        let registry_rows = vec![registry_row(KIND_HEADLESS, "headless-1", true)];
        let live = Err("Lock error: poisoned".to_string());

        let result = union_registry_and_live_pty(registry_rows, live);

        assert_eq!(
            result.err().as_deref(),
            Some("Lock error: poisoned"),
            "the enumeration error propagates, not an empty/registry-only list"
        );
    }

    #[test]
    fn begin_reserves_one_slot_and_is_idempotent() {
        let st = DashboardLaunchState::new(2);
        assert_eq!(st.available(), 2);

        assert!(
            st.begin("SW-A").expect("first begin"),
            "first is a fresh reserve"
        );
        assert_eq!(st.available(), 1, "one permit reserved");

        // A repeated Run for the same story must NOT take a second permit, and
        // must report `false` so the caller knows it doesn't own the permit.
        assert!(
            !st.begin("SW-A").expect("idempotent begin"),
            "repeat is not a fresh reserve"
        );
        assert_eq!(st.available(), 1, "still one permit — idempotent focus");
    }

    // SW-DX-17e (AC-2/AC-3/AC-4): a `sessionMode:'new'` fork mints a DISTINCT
    // `storyId#attempt-N` handle FE-side and feeds it to the same `begin`. The
    // registry is unchanged: distinct handles are independent entries, each takes
    // its OWN permit, and `resolve` keeps them apart — so two forks of one story are
    // two live sessions, and releasing one frees exactly one slot (siblings intact).
    #[test]
    fn fork_handles_reserve_and_resolve_independently() {
        let st = DashboardLaunchState::new(3);
        // The main session + two anonymous forks of the SAME story.
        assert!(st.begin("SW-A").expect("main"), "main is a fresh reserve");
        assert!(
            st.begin("SW-A#attempt-2").expect("fork 2"),
            "fork-2 is a fresh reserve"
        );
        assert!(
            st.begin("SW-A#attempt-3").expect("fork 3"),
            "fork-3 is a fresh reserve"
        );
        assert_eq!(
            st.available(),
            0,
            "each fork consumed its own permit (AC-4)"
        );

        st.bind("SW-A#attempt-2", "sess-2", None)
            .expect("bind fork 2");
        st.bind("SW-A#attempt-3", "sess-3", None)
            .expect("bind fork 3");
        // Each fork resolves to ITS OWN session — no cross-talk (AC-3/AC-5).
        assert_eq!(st.resolve("SW-A#attempt-2").as_deref(), Some("sess-2"));
        assert_eq!(st.resolve("SW-A#attempt-3").as_deref(), Some("sess-3"));

        // Releasing one fork frees EXACTLY one slot; its sibling is untouched (AC-4).
        st.release_for_session("sess-2").expect("release fork 2");
        assert_eq!(st.available(), 1, "one slot freed");
        assert!(st.resolve("SW-A#attempt-2").is_none(), "fork 2 gone");
        assert_eq!(
            st.resolve("SW-A#attempt-3").as_deref(),
            Some("sess-3"),
            "sibling fork untouched"
        );
    }

    #[test]
    fn begin_is_busy_when_cap_saturated() {
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").expect("first fits");
        // A different story with the cap full fails fast with `busy`.
        let err = st.begin("SW-B").expect_err("second story over cap");
        assert_eq!(err, SpawnCapError::Busy);
        assert_eq!(err.to_string(), "busy");
    }

    #[test]
    fn bind_then_resolve_returns_the_session() {
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-1", None).expect("bind after begin");
        assert_eq!(st.resolve("SW-A").as_deref(), Some("sess-1"));
    }

    /// SW-OR-1 (AC-2) anti-forgery: `send_to_session` addresses a session ONLY
    /// by its opaque HANDLE and fires at `resolve(handle)`. A caller who forges
    /// the underlying `sessionId` and passes it as the handle must NOT reach the
    /// session — the id is a *value* in the registry, never a key. So resolving
    /// the session id yields `None` (refused), while the real handle resolves.
    #[test]
    fn resolve_refuses_a_forged_session_id_used_as_handle() {
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-1", None).expect("bind after begin");
        // The real handle fires the write.
        assert_eq!(st.resolve("SW-A").as_deref(), Some("sess-1"));
        // The forged session id does NOT — the MCP tool refuses it, never
        // falling through to a caller-supplied target (module-doc invariant).
        assert!(
            st.resolve("sess-1").is_none(),
            "a forged sessionId must never resolve to a fire target"
        );
    }

    #[test]
    fn bind_without_begin_is_rejected() {
        let st = DashboardLaunchState::new(1);
        let err = st
            .bind("SW-A", "sess-1", None)
            .expect_err("bind needs a begin");
        assert!(err.starts_with("unavailable:"), "got: {err}");
        assert!(st.resolve("SW-A").is_none());
    }

    #[test]
    fn resolve_before_bind_is_none() {
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        // Reserved but not yet bound → no fire target (runPrompt rejects).
        assert!(st.resolve("SW-A").is_none());
    }

    #[test]
    fn release_frees_the_slot_and_drops_bindings() {
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-1", None).unwrap();
        assert_eq!(st.available(), 0);

        st.release_for_session("sess-1");
        assert_eq!(st.available(), 1, "explicit release frees the slot");
        assert!(st.resolve("SW-A").is_none());
    }

    #[test]
    fn parked_session_holds_its_slot_until_released() {
        // AC-8: a bound session that is never released (dead-but-undismissed,
        // i.e. "parked" needs-attention) keeps its permit — the cap stays full.
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-1", None).unwrap();

        // The natural pty-exit does NOT call release; the slot stays held.
        assert_eq!(st.available(), 0);
        assert_eq!(st.begin("SW-B").unwrap_err(), SpawnCapError::Busy);

        // Only the explicit clear/kill (release) frees it.
        st.release_for_session("sess-1");
        assert_eq!(st.available(), 1);
        st.begin("SW-B").expect("slot freed after explicit release");
    }

    #[test]
    fn abort_frees_a_reserved_but_unbound_slot() {
        // The FE failure path: begin reserved a permit, but the open failed
        // before a session was bound. abort must free the slot (not leak it).
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        assert_eq!(st.available(), 0);
        st.abort("SW-A");
        assert_eq!(st.available(), 1, "abort frees the reserved slot");
        st.begin("SW-B").expect("slot reusable after abort");
    }

    #[test]
    fn abort_after_bind_also_releases_and_clears_reverse_index() {
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-1", None).unwrap();
        st.abort("SW-A");
        assert_eq!(st.available(), 1);
        // The reverse index is gone too — a later release-by-session is a no-op.
        st.release_for_session("sess-1");
        assert_eq!(st.available(), 1);
    }

    #[test]
    fn release_for_session_evicts_turn_state() {
        use crate::agent_state::AgentState;
        let st = DashboardLaunchState::new(2);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-1", None).unwrap();
        // The agent's last hook before it was killed; no `SessionEnd` ever arrives.
        st.turns().record_hook("sess-1", "Stop");
        // A plain terminal running an agent is torn down through the same choke.
        st.turns().record_hook("plain-tab", "PreToolUse");
        assert_eq!(st.turns().last("sess-1"), Some(AgentState::Done));

        st.release_for_session("sess-1");
        st.release_for_session("plain-tab");
        assert_eq!(st.turns().last("sess-1"), None);
        assert_eq!(st.turns().last("plain-tab"), None);
    }

    #[test]
    fn release_of_unknown_session_is_a_noop() {
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-1", None).unwrap();
        // A non-dashboard session id (a normal terminal) must not disturb state.
        st.release_for_session("some-other-session");
        assert_eq!(st.available(), 0);
        assert_eq!(st.resolve("SW-A").as_deref(), Some("sess-1"));
    }

    #[test]
    fn rebind_drops_the_stale_reverse_index() {
        // A respawn/tab-reuse re-binds a new session id; the old id must stop
        // resolving so a status update for the dead session can't hit this story.
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-old", None).unwrap();
        st.bind("SW-A", "sess-new", None).unwrap();
        assert_eq!(st.resolve("SW-A").as_deref(), Some("sess-new"));
        // Releasing by the stale id is a no-op (it no longer maps anywhere).
        st.release_for_session("sess-old");
        assert_eq!(st.available(), 0, "stale-id release must not free the slot");
        // The live id still releases correctly.
        st.release_for_session("sess-new");
        assert_eq!(st.available(), 1);
    }

    // ── SW-DX-11: cap-free register + kind-agnostic resolve (§2) ─────────────

    #[test]
    fn register_is_cap_free_and_resolvable() {
        // An app-opened epic terminal registers WITHOUT consuming a cap slot, yet
        // becomes addressable through the SAME resolve() as a dashboard launch.
        let st = DashboardLaunchState::new(1);
        let epic = "/ctx/epic/README.md"; // epicDocPath handle
        st.register(epic, "sess-epic", None);
        assert_eq!(st.resolve(epic).as_deref(), Some("sess-epic"));
        // Cap untouched — a dashboard spawn can STILL reserve the single slot.
        assert_eq!(st.available(), 1, "register must not consume a cap slot");
        st.begin("SW-A")
            .expect("cap-1 slot still free after a cap-free register");
    }

    #[test]
    fn resolve_is_kind_agnostic_across_handle_shapes() {
        // ONE map, ONE lookup: a story id, an epicDocPath, a team::epic context
        // key, and a repo::branch handle all resolve through the same resolve().
        let st = DashboardLaunchState::new(4);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "s-story", None).unwrap();
        st.register("/ctx/epic/README.md", "s-epic", None);
        st.register("mycorp/9north-context:smart-web::DEMO-1888", "s-ctx", None);
        st.register("owner/name::feature/x", "s-branch", None);
        assert_eq!(st.resolve("SW-A").as_deref(), Some("s-story"));
        assert_eq!(st.resolve("/ctx/epic/README.md").as_deref(), Some("s-epic"));
        assert_eq!(
            st.resolve("mycorp/9north-context:smart-web::DEMO-1888")
                .as_deref(),
            Some("s-ctx")
        );
        assert_eq!(
            st.resolve("owner/name::feature/x").as_deref(),
            Some("s-branch")
        );
        assert!(st.resolve("no-such-handle").is_none());
    }

    #[test]
    fn register_moves_a_session_between_handles_without_stranding_the_old_one() {
        // Review r1: registering one session id under a second handle must NOT
        // leave the first handle resolving to a session it no longer owns.
        let st = DashboardLaunchState::new(2);
        st.register("A", "s1", None);
        st.register("B", "s1", None); // same session, new handle
        assert!(st.resolve("A").is_none(), "old handle must stop resolving");
        assert_eq!(st.resolve("B").as_deref(), Some("s1"));
        // Releasing the session clears the surviving handle cleanly.
        st.release_for_session("s1");
        assert!(st.resolve("B").is_none());
    }

    #[test]
    fn register_rebinds_and_release_clears_a_registered_session() {
        let st = DashboardLaunchState::new(1);
        st.register("epic-1", "sess-old", None);
        st.register("epic-1", "sess-new", None); // rebind
        assert_eq!(st.resolve("epic-1").as_deref(), Some("sess-new"));
        // Stale id no longer resolves anywhere.
        st.release_for_session("sess-old");
        assert_eq!(st.resolve("epic-1").as_deref(), Some("sess-new"));
        // The live id releases the (cap-free) entry.
        st.release_for_session("sess-new");
        assert!(st.resolve("epic-1").is_none());
        // available() was never consumed by the registration.
        assert_eq!(st.available(), 1);
    }

    #[test]
    fn registering_a_workspace_main_session_never_relabels_a_launched_agent() {
        let st = DashboardLaunchState::new(2);
        let manager = "/ctx/content#manager";
        assert!(st.begin(manager).unwrap());
        st.bind(manager, "sess-ctl", None).unwrap();
        st.set_agent_revision_if_absent(manager, "acme/backend/manager@2".into());
        // The nav open of the content folder names the manager's tab its main session.
        st.register("/ctx/content", "sess-ctl", Some("codesense"));
        assert_eq!(st.handle_for_session("sess-ctl").as_deref(), Some(manager));
        assert_eq!(
            st.agent_revision_for_session("sess-ctl").as_deref(),
            Some("acme/backend/manager@2")
        );
        assert!(!st.has_handle("/ctx/content"));

        // A plain registered shell still moves to the handle that names it now.
        st.register("old", "sess-shell", None);
        st.register("new", "sess-shell", None);
        assert_eq!(st.handle_for_session("sess-shell").as_deref(), Some("new"));
        assert!(!st.has_handle("old"));
    }

    // ── SW-DX-17a: snapshot + release-with-info (list + kill surface) ────────

    #[test]
    fn snapshot_lists_all_entries_with_the_right_kind() {
        let st = DashboardLaunchState::new(4);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-a", None).unwrap();
        st.register("/ctx/epic/README.md", "sess-epic", None);

        let mut rows = st.snapshot();
        rows.sort_by(|a, b| a.handle.cmp(&b.handle));
        assert_eq!(rows.len(), 2);

        let epic = rows
            .iter()
            .find(|r| r.handle == "/ctx/epic/README.md")
            .unwrap();
        assert_eq!(epic.session_id.as_deref(), Some("sess-epic"));
        assert_eq!(epic.kind, KIND_REGISTERED, "register → cap-free kind");

        let story = rows.iter().find(|r| r.handle == "SW-A").unwrap();
        assert_eq!(story.session_id.as_deref(), Some("sess-a"));
        assert_eq!(
            story.kind, KIND_DASHBOARD_LAUNCHED,
            "begin+bind → dashboard-launched kind"
        );
    }

    #[test]
    fn launcher_records_the_surface_and_a_none_restamp_preserves_it() {
        // SW-GW-8 AC-5: `launcher` names WHICH surface opened the session — the
        // dashboard docPath for a dashboard launch, "codesense" for a nav/manual
        // open — so the panel row is legible beyond the coarse `kind` axis.
        let st = DashboardLaunchState::new(4);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-a", Some("/ctx/boards/dash.html"))
            .unwrap();
        st.register("SW-B", "sess-b", Some("codesense"));

        let rows = st.snapshot();
        let dash = rows.iter().find(|r| r.handle == "SW-A").unwrap();
        assert_eq!(dash.launcher.as_deref(), Some("/ctx/boards/dash.html"));
        let nav = rows.iter().find(|r| r.handle == "SW-B").unwrap();
        assert_eq!(nav.launcher.as_deref(), Some("codesense"));

        // A respawn/rebind that doesn't re-assert provenance (`None`) must NOT
        // erase the launcher stamped at first bind — the row stays legible.
        st.bind("SW-A", "sess-a2", None).unwrap();
        let after = st.snapshot();
        let dash2 = after.iter().find(|r| r.handle == "SW-A").unwrap();
        assert_eq!(
            dash2.launcher.as_deref(),
            Some("/ctx/boards/dash.html"),
            "a None re-stamp preserves the original launcher"
        );

        // First-writer-wins: a story a dashboard already launched, then opened
        // from the nav, COLLAPSES onto the same handle — the LATER surface must
        // NOT clobber the original. The row keeps the dashboard's docPath, not
        // "codesense", so it answers "which surface OPENED this agent".
        st.register("SW-A", "sess-a2", Some("codesense"));
        let collapsed = st.snapshot();
        let dash3 = collapsed.iter().find(|r| r.handle == "SW-A").unwrap();
        assert_eq!(
            dash3.launcher.as_deref(),
            Some("/ctx/boards/dash.html"),
            "a colliding later launcher must not overwrite the original surface"
        );
    }

    #[test]
    fn account_provenance_is_stamped_once_and_projected() {
        let state = DashboardLaunchState::new(2);
        state.begin("SW-A").unwrap();
        let first = AgentAccountProvenance {
            runtime: "claude".into(),
            provider: "claude".into(),
            source: "explicit".into(),
            reason: "explicit-override".into(),
            account_id: Some("account-a".into()),
            label: Some("Account A".into()),
            home: None,
        };
        state
            .bind_with_account("SW-A", "session-a", Some("dashboard"), Some(first.clone()))
            .unwrap();
        state
            .bind_with_account(
                "SW-A",
                "session-a",
                Some("other"),
                Some(AgentAccountProvenance {
                    account_id: Some("account-b".into()),
                    label: Some("Account B".into()),
                    ..first.clone()
                }),
            )
            .unwrap();

        assert_eq!(state.snapshot()[0].account_provenance, Some(first));
    }

    #[test]
    fn snapshot_surfaces_a_reserved_but_unbound_launch() {
        // A `begin` with no `bind` (a stuck/mid-launch or leaked reservation) is
        // exactly the kind of thing the panel must reveal: present, no session id.
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        let rows = st.snapshot();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].handle, "SW-A");
        assert!(
            rows[0].session_id.is_none(),
            "reserved-but-unbound → no session"
        );
        assert_eq!(rows[0].kind, KIND_DASHBOARD_LAUNCHED);
    }

    #[test]
    fn release_returns_the_handle_and_kind_for_a_dashboard_launch() {
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-a", None).unwrap();
        let released = st
            .release_for_session("sess-a")
            .expect("a bound dashboard session releases with info");
        assert_eq!(released.handle, "SW-A");
        assert_eq!(released.kind, KIND_DASHBOARD_LAUNCHED);
    }

    #[test]
    fn release_returns_registered_kind_for_a_cap_free_session() {
        let st = DashboardLaunchState::new(1);
        st.register("/ctx/epic/README.md", "sess-epic", None);
        let released = st
            .release_for_session("sess-epic")
            .expect("a registered session releases with info");
        assert_eq!(released.handle, "/ctx/epic/README.md");
        assert_eq!(released.kind, KIND_REGISTERED);
    }

    #[test]
    fn abort_reports_whether_it_removed_an_entry() {
        // SW-DX-17a: the audit + kill-by-handle fallback gate on this bool.
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        assert!(st.abort("SW-A"), "a reserved handle is removed → true");
        assert!(
            !st.abort("SW-A"),
            "a second abort of the same handle removed nothing → false"
        );
        assert!(
            !st.abort("never-reserved"),
            "unknown handle → false (no-op)"
        );
    }

    #[test]
    fn kill_by_handle_fallback_clears_a_reserved_but_unbound_reservation() {
        // The Major from review r1: a stuck `begin` (no `bind`) has no session, so
        // `resolve` is `None` and `kill_session` can't free it. `dashboard_kill_by_handle`
        // falls back to `abort`, which frees the leaked cap slot + clears the row.
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        assert!(st.resolve("SW-A").is_none(), "unbound → nothing to kill");
        assert_eq!(st.available(), 0, "slot is held by the reservation");
        // The fallback the command runs when resolve() is None:
        assert!(
            st.abort("SW-A"),
            "abort clears the reserved-but-unbound entry"
        );
        assert_eq!(st.available(), 1, "leaked cap slot is freed");
    }

    #[test]
    fn release_of_a_non_registered_session_returns_none() {
        // A plain terminal / warm-pool member id was never in the registry →
        // `None` so `kill_session` writes NO audit line for it.
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-a", None).unwrap();
        assert!(
            st.release_for_session("some-other-session").is_none(),
            "unknown session → no ReleasedSession → no kill audit"
        );
        // The real session is untouched.
        assert_eq!(st.resolve("SW-A").as_deref(), Some("sess-a"));
    }

    // ── SW-DX-17d: foreground (permit release) ────

    #[test]
    fn foreground_releases_a_dashboard_launch_slot_but_keeps_the_stored_kind() {
        // AC-3b + S-1: the named incoherence — a dashboard-launched agent holds a
        // slot; foregrounding it (opening it as a tab) must FREE the slot, yet the
        // row must still read `dashboard-launched`, not flip to `registered`.
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        st.bind("SW-A", "sess-a", None).unwrap();
        assert_eq!(st.available(), 0, "a dashboard launch holds a slot");

        assert!(
            st.foreground("SW-A"),
            "a real permit release returns true (SW-AM-8 gates the emit on this)"
        );
        assert_eq!(
            st.available(),
            1,
            "foregrounding frees the slot (attended now)"
        );
        let rows = st.snapshot();
        let row = rows.iter().find(|r| r.handle == "SW-A").unwrap();
        assert!(!row.backgrounded, "foreground → not backgrounded");
        assert_eq!(
            row.kind, KIND_DASHBOARD_LAUNCHED,
            "stored origin survives the permit toggle"
        );

        // And it releases (kill) as a dashboard launch, not a registered one.
        let released = st.release_for_session("sess-a").expect("still registered");
        assert_eq!(released.kind, KIND_DASHBOARD_LAUNCHED);
    }

    #[test]
    fn foreground_is_idempotent_and_a_noop_on_an_unheld_entry() {
        let st = DashboardLaunchState::new(1);
        st.register("/ctx/epic/README.md", "sess-epic", None); // cap-free, no permit
        assert!(
            !st.foreground("/ctx/epic/README.md"),
            "nothing to release → false (SW-AM-8 skips the spurious emit)"
        );
        assert_eq!(st.available(), 1);
        assert!(
            !st.foreground("no-such-handle"),
            "unknown handle → false, no panic, no-op"
        );
        assert_eq!(st.available(), 1);
    }

    // ── SW-SR-5 (AC-2): run-scoped worker resolution + drain ────────────────

    /// Stamp a bound worker into `run_id` in one helper (begin → bind → provenance).
    fn bind_worker_for_run(st: &DashboardLaunchState, handle: &str, sid: &str, run_id: &str) {
        st.begin(handle).expect("begin worker");
        st.bind(handle, sid, None).expect("bind worker");
        st.stamp_provenance(
            handle,
            RunProvenance {
                run_id: run_id.to_string(),
                story_id: handle.to_string(),
                automation_id: None,
                ..Default::default()
            },
        );
    }

    #[test]
    fn drain_run_is_scoped_to_one_run_and_leaves_siblings_untouched() {
        // AC-2: a run-scoped stop kills ONLY its own run's workers; a concurrent run's
        // sessions are never in the kill list.
        let st = DashboardLaunchState::new(4);
        bind_worker_for_run(&st, "SW-A", "sess-a", "run-1");
        bind_worker_for_run(&st, "SW-C", "sess-c", "run-2");

        let targets = st.drain_run("run-1");
        assert_eq!(targets.to_kill, vec!["sess-a".to_string()]);
        assert!(targets.aborted.is_empty());
        // run-2's worker is untouched — still addressable.
        assert_eq!(st.resolve("SW-C").as_deref(), Some("sess-c"));
    }

    #[test]
    fn drain_run_sweeps_a_reserved_but_unbound_worker_of_the_run() {
        // AC-2: an unbound worker (begin, never bound) stamped to the run has no PTY — it is
        // aborted in place (slot freed) and reported in `aborted`.
        let st = DashboardLaunchState::new(3);
        bind_worker_for_run(&st, "SW-BOUND", "sess-bound", "run-1");
        // A reserved-but-unbound entry can't be stamped via bind; provenance goes on the
        // begun handle directly (the dispatch stamps provenance after bind in prod, but the
        // drain filter only reads provenance + session_id, so this exercises the unbound arm).
        st.begin("SW-UNBOUND").expect("begin unbound");
        st.stamp_provenance(
            "SW-UNBOUND",
            RunProvenance {
                run_id: "run-1".to_string(),
                story_id: "SW-UNBOUND".to_string(),
                automation_id: None,
                ..Default::default()
            },
        );
        assert_eq!(st.available(), 1, "two slots held");

        let targets = st.drain_run("run-1");
        assert_eq!(targets.to_kill, vec!["sess-bound".to_string()]);
        assert_eq!(targets.aborted, vec!["SW-UNBOUND".to_string()]);
        assert_eq!(st.available(), 2, "unbound slot freed by drain");
        st.release_for_session("sess-bound"); // simulate kill_session's release
        assert_eq!(st.available(), 3, "every slot freed once the choke fires");
    }

    #[test]
    fn drain_run_of_an_unknown_run_is_empty() {
        let st = DashboardLaunchState::new(2);
        bind_worker_for_run(&st, "SW-A", "sess-a", "run-1");
        assert_eq!(
            st.drain_run("ghost"),
            RunStopTargets::default(),
            "no workers for an unknown run"
        );
    }

    #[test]
    fn snapshot_stamps_a_birth_time_for_elapsed() {
        let st = DashboardLaunchState::new(1);
        st.begin("SW-A").unwrap();
        let rows = st.snapshot();
        assert!(
            rows[0].started_at_ms.is_some_and(|t| t > 0),
            "a launched session carries a birth-time anchor for elapsed"
        );
    }

    fn org_cache_with_story(
        team: &str,
        handle: &str,
    ) -> (
        tempfile::TempDir,
        crate::install::frontmatter_cache::FrontmatterCache,
    ) {
        let org = tempfile::tempdir().unwrap();
        let content = org.path().join(team).join("content");
        std::fs::create_dir_all(&content).unwrap();
        std::fs::write(
            content.join(format!("{handle}-slug.md")),
            format!(
                "---\nstatus: todo\nstory_id: {handle}\nsource_repo: acme/codesense\n---\n# Story"
            ),
        )
        .unwrap();
        let cache =
            crate::install::frontmatter_cache::FrontmatterCache::build_from_scan(org.path());
        (org, cache)
    }

    #[test]
    fn resolve_story_path_returns_the_managed_doc_and_team() {
        let (_org, cache) = org_cache_with_story("default", "SW-DX-7");
        let res = resolve_story_path_in(&cache, "SW-DX-7").expect("resolves the indexed story");
        assert!(
            res.story_file_path.ends_with("SW-DX-7-slug.md"),
            "got {}",
            res.story_file_path
        );
        assert_eq!(res.team, "default");
    }

    #[test]
    fn resolve_story_path_rejects_an_unknown_id() {
        let (_org, cache) = org_cache_with_story("default", "SW-DX-7");
        let err = resolve_story_path_in(&cache, "SW-NOPE").expect_err("unknown id rejects");
        assert!(err.starts_with("unavailable:"), "got {err}");
    }

    // ----- SW-CR-9: fallback refresh on cache miss --------------------------

    /// Build a cold shared frontmatter cache rooted at a tempdir org tree.
    /// Returns `(tempdir, shared, org_root)` so tests can drop raw story MDs
    /// into the content tree and observe the fallback behaviour.
    fn shared_cache_with_tree(
        team: &str,
        seed_handle: Option<&str>,
    ) -> (
        tempfile::TempDir,
        crate::install::frontmatter_cache::SharedFrontmatterCache,
        std::path::PathBuf,
    ) {
        let org = tempfile::tempdir().unwrap();
        let content = org.path().join(team).join("content");
        std::fs::create_dir_all(&content).unwrap();
        if let Some(handle) = seed_handle {
            std::fs::write(
                content.join(format!("{handle}-slug.md")),
                format!(
                    "---\nstatus: todo\nstory_id: {handle}\nsource_repo: acme/codesense\n---\n# Story"
                ),
            )
            .unwrap();
        }
        let cache = crate::install::frontmatter_cache::FrontmatterCache::shared(
            crate::install::frontmatter_cache::FrontmatterCache::build_from_scan(org.path()),
        );
        let path = org.path().to_path_buf();
        (org, cache, path)
    }

    /// AC-1: a story MD dropped onto disk after cold build is not in the
    /// cache; the fallback scans, refreshes the single file, and the retry
    /// hits — the exact flow an agent-authored story needs.
    #[test]
    fn sw_cr_9_fallback_refresh_unblocks_the_agent_authoring_path() {
        let (org, shared, root) = shared_cache_with_tree("default", None);
        // Pre-condition: nothing in cache.
        assert!(
            resolve_story_path_with_fallback(&shared, Some(&root), "SW-NEW-1")
                .unwrap_err()
                .contains("no managed story with id 'SW-NEW-1'")
        );

        // Drop the story on disk the way a raw agent writer would — no FS
        // watcher event, no CodeSense file-API call.
        let content = org.path().join("default").join("content");
        std::fs::write(
            content.join("SW-NEW-1.md"),
            "---\ntype: story\nstatus: todo\nstory_id: SW-NEW-1\nsource_repo: acme/codesense\n---\n# S",
        )
        .unwrap();

        let resolved = resolve_story_path_with_fallback(&shared, Some(&root), "SW-NEW-1")
            .expect("fallback refresh picks up the agent-authored story");
        assert!(
            resolved.story_file_path.ends_with("SW-NEW-1.md"),
            "resolved to {}",
            resolved.story_file_path
        );
        assert_eq!(resolved.team, "default");
    }

    /// AC-3: a story_id that has no file anywhere returns the better error —
    /// names the id and names the next step (`refresh_frontmatter_cache`).
    /// The FIRST LINE stays the familiar `unavailable: no managed story with id 'X'`
    /// shape so the FE `userFacingErrorMessage` regex still renders friendly copy;
    /// the hint lives on line 2 (dropped by `reasonLine`, preserved for agents).
    #[test]
    fn sw_cr_9_better_error_names_the_next_step_on_a_genuine_miss() {
        let (_org, shared, root) = shared_cache_with_tree("default", Some("SW-OTHER"));
        let err = resolve_story_path_with_fallback(&shared, Some(&root), "SW-GHOST")
            .expect_err("a genuine miss returns an error");
        let first_line = err.lines().next().expect("at least one line");
        assert_eq!(
            first_line, "unavailable: no managed story with id 'SW-GHOST'",
            "first line stays the FE-regex-matching typed refusal: {err}"
        );
        assert!(
            err.contains("refresh_frontmatter_cache"),
            "subsequent lines point at the explicit refresh seam: {err}"
        );
    }

    /// AC-3: when the scan finds a file that still won't index after refresh
    /// (frontmatter is malformed — a missing `source_repo` orphan-skips in
    /// `build_cache_entry`), the first line names the candidate path + queried
    /// id (so the FE can render a dedicated friendly message) and the hint on
    /// the next line calls out the fix.
    #[test]
    fn sw_cr_9_error_names_the_candidate_path_when_fallback_cannot_heal() {
        let (org, shared, root) = shared_cache_with_tree("default", None);
        // Scanner will find a file carrying the story_id, but the cache's
        // orphan guard (`build_cache_entry` requires `source_repo`) rejects
        // it — so the refresh runs but the retry still misses.
        let content = org.path().join("default").join("content");
        let path = content.join("SW-ORPHAN.md");
        std::fs::write(
            &path,
            "---\ntype: story\nstatus: todo\nstory_id: SW-ORPHAN\n---\n# orphan",
        )
        .unwrap();

        let err = resolve_story_path_with_fallback(&shared, Some(&root), "SW-ORPHAN")
            .expect_err("orphan candidate still misses after refresh");
        let first_line = err.lines().next().expect("at least one line");
        assert!(
            first_line.starts_with("unavailable: story file candidate at ")
                && first_line.contains("SW-ORPHAN.md")
                && first_line.ends_with("does not carry story_id 'SW-ORPHAN'"),
            "first line names the typed kind, candidate path, and queried id: {err}"
        );
        assert!(
            err.contains("source_repo"),
            "subsequent lines hint at the orphan-skip reason: {err}"
        );
        assert!(
            err.contains("refresh_frontmatter_cache"),
            "subsequent lines point at the explicit refresh seam: {err}"
        );
    }

    /// AC-4: the hot (cache-hit) path returns immediately. A `scan_root` of
    /// `None` would be a no-op for the fallback, so this test uses that signal
    /// — the fallback never runs when the first lookup succeeds. (A seeded
    /// story resolves even with `scan_root = None`.)
    #[test]
    fn sw_cr_9_hot_path_bypasses_the_fallback_scan() {
        let (_org, shared, _root) = shared_cache_with_tree("default", Some("SW-HOT"));
        let resolved = resolve_story_path_with_fallback(&shared, None, "SW-HOT")
            .expect("hot cache path resolves without a scan root");
        assert!(resolved.story_file_path.ends_with("SW-HOT-slug.md"));
    }

    /// AC-6: `refresh_under_lock` self-healing from the fallback does not
    /// change the arity gate. An ambiguous id (two files, same story_id) is
    /// still an ambiguous error — not re-cast to NotFound by the fallback.
    #[test]
    fn sw_cr_9_ambiguous_ids_are_not_scanned() {
        let (org, _shared, root) = shared_cache_with_tree("default", None);
        let content = org.path().join("default").join("content");
        for name in ["a.md", "b.md"] {
            std::fs::write(
                content.join(name),
                "---\ntype: story\nstatus: todo\nstory_id: SW-DUPE\nsource_repo: acme/codesense\n---\n# dupe",
            )
            .unwrap();
        }
        // Re-build so the cache sees both — a cold `build_from_scan` indexes
        // every file encountered, so the ambiguous arity is live from the start.
        let shared = crate::install::frontmatter_cache::FrontmatterCache::shared(
            crate::install::frontmatter_cache::FrontmatterCache::build_from_scan(org.path()),
        );
        let err = resolve_story_path_with_fallback(&shared, Some(&root), "SW-DUPE")
            .expect_err("ambiguous ids reject");
        assert!(
            err.contains("ambiguous story id"),
            "ambiguous error preserved: {err}"
        );
    }

    /// AC-7: the fallback scan completes well under 500 ms on a tree of
    /// 1000+ pipeline files. Uses the real scanner against disk — this is
    /// an upper-bound sanity check, not a microbenchmark, so the budget is
    /// generous.
    #[test]
    fn sw_cr_9_fallback_scan_is_bounded_under_500_ms() {
        let (org, shared, root) = shared_cache_with_tree("default", None);
        let content = org.path().join("default").join("content");
        for i in 0..1_050 {
            std::fs::write(
                content.join(format!("f{i}.md")),
                format!(
                    "---\ntype: story\nstatus: todo\nstory_id: NOISE-{i}\nsource_repo: acme/codesense\n---\n# n",
                ),
            )
            .unwrap();
        }
        // The target is written LAST so a short-circuiting walker's worst-
        // case covers the full tree before it hits.
        std::fs::write(
            content.join("target.md"),
            "---\ntype: story\nstatus: todo\nstory_id: SW-TARGET\nsource_repo: acme/codesense\n---\n# t",
        )
        .unwrap();

        let start = std::time::Instant::now();
        let res = resolve_story_path_with_fallback(&shared, Some(&root), "SW-TARGET")
            .expect("fallback finds the target in a large tree");
        let elapsed = start.elapsed();
        assert!(
            res.story_file_path.ends_with("target.md"),
            "resolved to {}",
            res.story_file_path
        );
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "fallback scan took {elapsed:?}, over the 500 ms budget"
        );
    }

    // ----- SW-DX-16: explicit-epic resolver (resolve_epic_path) -------------

    /// Build a managed content tree at `{home}/users/{uid}/orgs/{org}/{team}/
    /// content/<epic>` and return `(home_tempdir, home_path)`. The tree lives
    /// under `users/`, so it passes `is_codesense_managed_target`.
    fn managed_epic_tree(
        org_slug: &str,
        team: &str,
        epic_rel: &str,
        files: &[(&str, &str)],
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let content = crate::context_repo::path_to_team_content_dir(
            &crate::context_repo::path_to_org_root(home.path(), &Principal::from(42u64), org_slug)
                .unwrap(),
            team,
        );
        let epic_dir = content.join(epic_rel);
        std::fs::create_dir_all(&epic_dir).unwrap();
        for (name, body) in files {
            std::fs::write(epic_dir.join(name), body).unwrap();
        }
        let path = home.path().to_path_buf();
        (home, path)
    }

    #[test]
    fn resolve_epic_path_resolves_dir_to_maindoc() {
        let (_home, home_path) = managed_epic_tree(
            "acme",
            "default",
            "epics/my-epic",
            &[("README.md", "# Epic")],
        );
        let res = resolve_epic_path_in(
            &home_path,
            &Principal::from(42u64),
            "acme",
            "default",
            "epics/my-epic",
        )
        .expect("resolves the managed epic");
        assert!(
            res.epic_file_path.ends_with("README.md"),
            "got {}",
            res.epic_file_path
        );
        assert_eq!(res.epic_dir_name, "my-epic");
    }

    #[test]
    fn resolve_epic_path_accepts_a_doc_path() {
        // Pointing `epic` at the mainDoc file itself resolves to the same epic.
        let (_home, home_path) = managed_epic_tree(
            "acme",
            "default",
            "epics/my-epic",
            &[("README.md", "# Epic")],
        );
        let res = resolve_epic_path_in(
            &home_path,
            &Principal::from(42u64),
            "acme",
            "default",
            "epics/my-epic/README.md",
        )
        .expect("a doc path resolves to its epic dir");
        assert!(res.epic_file_path.ends_with("README.md"));
        assert_eq!(res.epic_dir_name, "my-epic");
    }

    #[test]
    fn resolve_epic_path_rejects_unknown() {
        let (_home, home_path) = managed_epic_tree(
            "acme",
            "default",
            "epics/my-epic",
            &[("README.md", "# Epic")],
        );
        let err = resolve_epic_path_in(
            &home_path,
            &Principal::from(42u64),
            "acme",
            "default",
            "epics/nope",
        )
        .expect_err("unknown epic rejects before any spawn");
        assert!(err.starts_with("unavailable:"), "got {err}");
    }

    #[test]
    fn resolve_epic_path_rejects_epic_with_no_maindoc() {
        let (_home, home_path) =
            managed_epic_tree("acme", "default", "epics/bare", &[("notes.txt", "x")]);
        let err = resolve_epic_path_in(
            &home_path,
            &Principal::from(42u64),
            "acme",
            "default",
            "epics/bare",
        )
        .expect_err("a dir with no README/index honest-degrades to a typed reject");
        assert!(err.starts_with("unavailable:"), "got {err}");
    }

    // ----- RF-7: managed team content-root resolver (run_skill cwd) ---------

    #[test]
    fn resolve_managed_team_content_root_returns_the_canonical_content_dir() {
        // The resolver returns the managed content root (the `.claude/skills/`
        // holder) that anchors a `run_skill` agent's cwd — canonicalized, and on
        // a managed surface.
        let (_home, home_path) = managed_epic_tree(
            "acme",
            "default",
            "epics/my-epic",
            &[("README.md", "# Epic")],
        );
        let root = resolve_managed_team_content_root(
            &home_path,
            &Principal::from(42u64),
            "acme",
            "default",
        )
        .expect("managed content root resolves");
        let expect = crate::context_repo::path_to_team_content_dir(
            &crate::context_repo::path_to_org_root(&home_path, &Principal::from(42u64), "acme")
                .unwrap(),
            "default",
        )
        .canonicalize()
        .unwrap();
        assert_eq!(root, expect);
        assert!(
            crate::install::worktree_index::is_codesense_managed_target_canon(&root, &home_path),
            "the resolved root must be a managed surface"
        );
    }

    #[test]
    fn resolve_managed_team_content_root_missing_team_degrades_to_unavailable() {
        let (_home, home_path) = managed_epic_tree(
            "acme",
            "default",
            "epics/my-epic",
            &[("README.md", "# Epic")],
        );
        let err = resolve_managed_team_content_root(
            &home_path,
            &Principal::from(42u64),
            "acme",
            "no-such-team",
        )
        .expect_err("a team with no content dir honest-degrades");
        assert!(err.starts_with("unavailable:"), "got {err}");
    }

    #[test]
    fn resolve_epic_path_rejects_traversal() {
        // `..` escapes `content` (resolves to the existing `{team}` dir, which is
        // OUTSIDE content) → typed `traversal:` before any spawn.
        let (_home, home_path) = managed_epic_tree(
            "acme",
            "default",
            "epics/my-epic",
            &[("README.md", "# Epic")],
        );
        let err =
            resolve_epic_path_in(&home_path, &Principal::from(42u64), "acme", "default", "..")
                .expect_err("`..` escapes the content root");
        assert!(err.starts_with("traversal:"), "got {err}");
    }

    #[test]
    fn resolve_epic_path_rejects_the_content_root_itself() {
        // `.` / `""` / a content-root file must NOT resolve to the whole content
        // tree as a spawnable "epic" (the `starts_with` wall can't catch a target
        // that IS the root — the equality guard does).
        let (_home, home_path) = managed_epic_tree(
            "acme",
            "default",
            "epics/my-epic",
            &[("README.md", "# Epic")],
        );
        // Put a top-level README at the content root so mainDoc resolution WOULD
        // succeed if the root leaked through — the guard must fire first.
        let content = crate::context_repo::path_to_team_content_dir(
            &crate::context_repo::path_to_org_root(&home_path, &Principal::from(42u64), "acme")
                .unwrap(),
            "default",
        );
        std::fs::write(content.join("README.md"), "# Team root").unwrap();
        for epic in [".", "./", "epics/..", ""] {
            let err =
                resolve_epic_path_in(&home_path, &Principal::from(42u64), "acme", "default", epic)
                    .unwrap_err();
            assert!(
                err.starts_with("unavailable:"),
                "epic={epic:?} must reject the content root, got {err}"
            );
        }
        // A file directly under the content root resolves its parent = the root
        // → same guard.
        let err = resolve_epic_path_in(
            &home_path,
            &Principal::from(42u64),
            "acme",
            "default",
            "README.md",
        )
        .unwrap_err();
        assert!(err.starts_with("unavailable:"), "got {err}");
    }

    // SW-WA-F3 — the handle-resolve seam `read_terminal_tab` reads back through.
    // The MCP verb resolves the caller's OPAQUE handle to a sessionId here BEFORE
    // touching the PTY ring; a raw/forged sessionId or an unknown handle must
    // resolve to `None` so the verb refuses (`denied:`) and never reads a session
    // it was not handed. These exercise the exact `register` → `resolve` seam the
    // verb uses (`handler.rs` `open_terminal_tab` binds; `read_terminal_tab`
    // resolves).

    #[test]
    fn read_terminal_tab_resolves_a_registered_handle_to_its_session() {
        // AC-1: the handle `open_terminal_tab` minted + bound resolves to the tab's
        // sessionId — the address `read_terminal_tab` reads the replay of.
        let state = DashboardLaunchState::new(1);
        state.register("term-tab-1-0", "pty-session-abc", Some("codesense"));
        assert_eq!(
            state.resolve("term-tab-1-0").as_deref(),
            Some("pty-session-abc"),
        );
    }

    #[test]
    fn read_terminal_tab_refuses_a_forged_session_id() {
        // AC-2 (red): a caller that supplies a raw sessionId in the `handle` slot
        // (bypassing the opaque handle) resolves to `None` → the verb refuses. The
        // reverse index binds session→handle, but `resolve` only accepts a HANDLE,
        // never a sessionId, so the raw value is not a valid key.
        let state = DashboardLaunchState::new(1);
        state.register("term-tab-2-0", "pty-session-xyz", Some("codesense"));
        assert!(
            state.resolve("pty-session-xyz").is_none(),
            "a raw sessionId must never resolve — only the opaque handle does",
        );
    }

    #[test]
    fn read_terminal_tab_refuses_an_unknown_handle() {
        // AC-2 (red): a handle that was never registered (or whose session is gone)
        // resolves to `None` → refused, mirroring `send_to_session`.
        let state = DashboardLaunchState::new(1);
        assert!(state.resolve("term-tab-never-registered").is_none());
    }

    /// An agent launched headless: the real provisioning step and request builder with what
    /// `prepare` resolved, on the agent runtime's built-in descriptor.
    mod agent_headless {
        use std::sync::atomic::AtomicU32;
        use std::sync::Arc;

        use serde_json::json;

        use super::*;
        use crate::agents::adapter::LaunchDescriptor;
        use crate::agents::launch::{prepare, AgentLaunch, PrepareOptions};
        use crate::agents::model::{Agent, ClaudePermissions, LaunchMode, Permissions};
        use crate::agents::permissions::claude_generated_allow;
        use crate::agents::resolve::tests::{alice, draft, save, team_content, write};
        use codesense_pty_core::agent_cli::AgentCli;

        fn headless(home: &Path, agent: Agent) -> AgentLaunch {
            let key = save(home, &alice(), agent);
            prepare(
                home,
                &alice(),
                &key,
                PrepareOptions {
                    mode_override: Some(crate::agents::adapter::LaunchMode::Headless),
                    ..PrepareOptions::default()
                },
            )
            .unwrap()
        }

        /// Provision and build the request exactly as `launch_headless_session` does for an
        /// agent launch.
        fn spawn(
            agent: &AgentLaunch,
            cwd: &Path,
            session: &str,
            account_home: &Path,
        ) -> (CaptureRequest, crate::agents::launch::AppliedLaunch) {
            let (scope, applied) =
                provision_headless_scope(cwd, session, agent, Some(account_home)).unwrap();
            let mut args = agent.launch.args.clone();
            applied.drop_agent_settings(&mut args);
            args.extend(applied.argv(str::to_string));
            let request = headless_capture_request(
                PathBuf::from(format!("/usr/local/bin/{}", agent.launch.binary)),
                LaunchDescriptor::of(agent.runtime),
                args,
                scope,
                "go".to_string(),
                cwd.to_path_buf(),
                Arc::new(AtomicU32::new(0)),
                session,
                None,
                Vec::new(),
                Vec::new(),
                agent.launch.env.clone(),
                None,
            );
            (request, applied)
        }

        #[test]
        fn a_codex_agent_with_no_policy_runs_exec_with_its_args_and_the_default_policy() {
            let _home_env = crate::routines::test_support::TempHome::new();
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().join("home");
            team_content(&home, &alice());
            let agent = headless(
                &home,
                Agent {
                    launch: LaunchMode::Headless,
                    ..draft("triage", AgentCli::Codex)
                },
            );
            let cwd = tmp.path().join("worktree");
            write(&cwd, ".mcp.json", br#"{"mcpServers": {}}"#);
            write(
                &cwd,
                ".codex/config.toml",
                b"[mcp_servers.codesense]\ncommand = \"bridge\"\n[mcp_servers.slack]\ncommand = \"slack-mcp\"\n",
            );
            let session = format!("headless-{}", uuid::Uuid::new_v4());

            let (request, applied) = spawn(&agent, &cwd, &session, tmp.path());
            assert_eq!(
                request.program.file_name().and_then(|s| s.to_str()),
                Some("codex")
            );
            assert!(request.args.starts_with(&[
                "--no-daemon".into(),
                "exec".into(),
                "--model".into(),
                "opus".into()
            ]));
            assert!(request.args.contains(&"approval_policy=\"never\"".into()));
            assert!(request
                .args
                .contains(&"sandbox_mode=\"workspace-write\"".into()));
            assert!(request.args.iter().any(|arg| arg.contains("--local-scope")));
            assert!(!request
                .args
                .iter()
                .any(|a| a.contains("dangerously") || a == "--skip-git-repo-check"));
            assert!(applied.settings.is_none());
            let codex_home = crate::commands::mcp_scope::mcp_scope_session_dir(&session);
            assert!(request
                .env
                .iter()
                .any(|(k, v)| k == "CODEX_HOME" && Path::new(v) == tmp.path()));
            assert!(!codex_home.join("auth.json").exists());
            crate::commands::mcp_scope::remove_scope_config(&session);
        }

        #[test]
        fn a_claude_agent_spawns_with_its_own_settings_file_and_leaves_the_workspace_alone() {
            let _home_env = crate::routines::test_support::TempHome::new();
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().join("home");
            team_content(&home, &alice());
            let agent = headless(
                &home,
                Agent {
                    permissions: Some(Permissions {
                        claude: Some(ClaudePermissions {
                            deny: vec!["Bash(git push --force:*)".into()],
                            ..ClaudePermissions::default()
                        }),
                        ..Permissions::default()
                    }),
                    ..draft("guarded", AgentCli::Claude)
                },
            );
            let cwd = tmp.path().join("worktree");
            write(
                &cwd,
                ".mcp.json",
                br#"{"mcpServers": {"codesense": {"command": "bridge"}}}"#,
            );
            let session = format!("headless-{}", uuid::Uuid::new_v4());

            let (request, applied) = spawn(&agent, &cwd, &session, tmp.path());
            let file = applied.settings.as_ref().unwrap().path().to_path_buf();
            let at = request.args.iter().position(|a| a == "--settings").unwrap();
            assert_eq!(Path::new(&request.args[at + 1]), file);
            assert_eq!(request.args.first().map(String::as_str), Some("-p"));
            assert!(!request.args.iter().any(|a| a.contains("dangerously")));
            let settings: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            // The agent's rules, with CodeSense's credential-file denies added to its own.
            let mut expected = agent.generated[0].content["permissions"].clone();
            let deny = expected["deny"].as_array_mut().unwrap();
            deny.extend(
                codesense_pty_core::notify_hook::deny_rules()
                    .iter()
                    .map(|rule| json!(rule)),
            );
            assert_eq!(settings["permissions"], expected);
            assert!(settings["hooks"]["Stop"]
                .to_string()
                .contains("codesense-notify"));
            let denies: Vec<&str> = ["Bash(git push --force:*)"]
                .into_iter()
                .chain(codesense_pty_core::notify_hook::deny_rules().iter().map(String::as_str))
                .collect();
            assert_eq!(
                json!({ "permissions": settings["permissions"].clone() }),
                json!({ "permissions": {
                    "allow": claude_generated_allow(),
                    "deny": denies,
                    "defaultMode": "dontAsk"
                } })
            );
            // No permission rule or mode lands in the shared workspace settings.
            assert!(!cwd.join(".claude/settings.local.json").exists());
            crate::commands::mcp_scope::remove_scope_config(&session);
        }

        /// A git repo with one commit, as a story worktree is.
        fn git_repo(dir: &Path) {
            std::fs::create_dir_all(dir).unwrap();
            for args in [
                &["init", "-q"][..],
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    "init",
                ][..],
            ] {
                assert!(std::process::Command::new("git")
                    .arg("-C")
                    .arg(dir)
                    .args(args)
                    .status()
                    .unwrap()
                    .success());
            }
        }

        /// Every path git sees in `dir`, untracked and ignored included.
        fn git_status_all(dir: &Path) -> String {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args([
                    "status",
                    "--porcelain",
                    "--ignored",
                    "--untracked-files=all",
                ])
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        }

        #[test]
        fn no_headless_launch_writes_codesense_config_into_a_git_worktree() {
            let _home_env = crate::routines::test_support::TempHome::new();
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().join("home");
            team_content(&home, &alice());
            let cwd = tmp.path().join("worktree");
            git_repo(&cwd);
            let claude = headless(&home, draft("claude-worker", AgentCli::Claude));
            let codex = headless(&home, draft("codex-worker", AgentCli::Codex));

            let mut sessions = Vec::new();
            for agent in [&claude, &codex] {
                let session = format!("headless-{}", uuid::Uuid::new_v4());
                let (_, applied) =
                    provision_headless_scope(&cwd, &session, agent, Some(tmp.path())).unwrap();
                drop(applied);
                sessions.push(session);
            }

            assert_eq!(
                git_status_all(&cwd),
                "",
                "no launch writes into the worktree"
            );
            for session in &sessions {
                crate::commands::mcp_scope::remove_scope_config(session);
            }
        }

        #[test]
        fn a_launch_in_a_team_content_folder_writes_nothing_there() {
            let _home_env = crate::routines::test_support::TempHome::new();
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().join("home");
            team_content(&home, &alice());
            // A context repo's content folder, with the team's own commands committed.
            let content = tmp.path().join("ctx/team/content");
            git_repo(&content);
            std::fs::create_dir_all(content.join(".claude/commands")).unwrap();
            std::fs::write(content.join(".claude/commands/plan.md"), "plan").unwrap();
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(&content)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["add", "-A"])
                .status()
                .unwrap()
                .success());
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(&content)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["commit", "-q", "-m", "team"])
                .status()
                .unwrap()
                .success());
            let manager = headless(&home, draft("manager", AgentCli::Claude));
            let named = format!("headless-{}", uuid::Uuid::new_v4());
            let (_, applied) = provision_headless_scope(&content, &named, &manager, None).unwrap();
            assert_eq!(
                git_status_all(&content),
                "",
                "the content folder is untouched"
            );
            drop(applied);
            crate::commands::mcp_scope::remove_scope_config(&named);
        }

        #[test]
        fn the_builtin_claude_agents_launch_with_the_status_hooks_settings_file() {
            let _home_env = crate::routines::test_support::TempHome::new();
            // Notify is unchanged: a headless launch of the built-in Claude agent and of the
            // unattended default carries its per-launch `--settings <file>`, which wires
            // CodeSense's status hooks for every Claude lifecycle event, and writes nothing
            // into the worktree. The unattended default runs with permission checks skipped.
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().join("home");
            team_content(&home, &alice());
            let cwd = tmp.path().join("worktree");
            git_repo(&cwd);
            let revision = crate::agents::builtin::revision();
            for (key, label, mode) in [
                (
                    crate::agents::builtin::preferred_key(),
                    format!("builtin/claude@{revision}"),
                    "dontAsk",
                ),
                (
                    crate::agents::builtin::default_key(true),
                    format!("builtin/claude-unattended@{revision}"),
                    "bypassPermissions",
                ),
            ] {
                let agent = prepare(
                    &home,
                    &alice(),
                    &key,
                    PrepareOptions {
                        mode_override: Some(crate::agents::adapter::LaunchMode::Headless),
                        ..PrepareOptions::default()
                    },
                )
                .unwrap();
                assert_eq!(agent.revision_label(), label);
                let session = format!("headless-{}", uuid::Uuid::new_v4());

                let (request, applied) = spawn(&agent, &cwd, &session, tmp.path());
                let file = applied.settings.as_ref().unwrap().path().to_path_buf();
                let args = &request.args;
                let at = args.iter().position(|a| a == "--settings").unwrap();
                assert_eq!(Path::new(&args[at + 1]), file);
                assert_eq!(
                    args.iter().any(|a| a == "--dangerously-skip-permissions"),
                    mode == "bypassPermissions",
                    "{label}: {args:?}"
                );
                let settings: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
                assert_eq!(settings["permissions"]["defaultMode"], mode, "{label}");
                // A hidden session that skips permission checks never stops on Claude's
                // one-time bypass-mode acknowledgement.
                assert_eq!(
                    settings["skipDangerousModePermissionPrompt"].as_bool(),
                    (mode == "bypassPermissions").then_some(true),
                    "{label}"
                );
                for ev in crate::agent_state::status_hooks().map(|hook| hook.event) {
                    let groups = settings["hooks"][ev].as_array().unwrap();
                    assert_eq!(
                        groups
                            .iter()
                            .filter(|g| g.to_string().contains("codesense-notify"))
                            .count(),
                        1,
                        "{label}: {ev} wired once"
                    );
                }
                // Its inbox waiter arms at the boot, every turn end and after each tool
                // call, once each.
                for (ev, marker) in [
                    ("SessionStart", "--inbox-wait"),
                    ("Stop", "--inbox-wait"),
                    ("StopFailure", "--inbox-wait"),
                    ("PostToolUse", "--inbox-wait"),
                ] {
                    let groups = settings["hooks"][ev].as_array().unwrap();
                    assert_eq!(
                        groups
                            .iter()
                            .filter(|g| g.to_string().contains(marker))
                            .count(),
                        1,
                        "{label}: {ev} {marker} once"
                    );
                }
                drop(applied);
                assert_eq!(git_status_all(&cwd), "");
                crate::commands::mcp_scope::remove_scope_config(&session);
            }
        }

        #[test]
        fn an_agent_launch_records_its_revision_and_holds_its_settings_for_the_run() {
            let state = DashboardLaunchState::new(2);
            let tmp = tempfile::tempdir().unwrap();
            let file = tmp.path().join("held.json");
            std::fs::write(&file, "{}").unwrap();
            assert!(state.begin("h").unwrap());
            state.set_agent_revision_if_absent("h", "acme/api/backend/triage@1".into());
            state.set_agent_revision_if_absent("h", "acme/api/backend/triage@2".into());
            state.hold_launch_settings(
                "h",
                crate::agents::launch::LaunchSettingsFile::adopt(file.clone()),
            );
            state.bind("h", "s1", Some("codesense")).unwrap();

            let row = session_row(state.snapshot().remove(0), true);
            assert_eq!(
                row.agent_revision.as_deref(),
                Some("acme/api/backend/triage@1")
            );
            assert_eq!(
                serde_json::to_value(&row).unwrap()["agentRevision"],
                json!("acme/api/backend/triage@1")
            );
            state.release_for_session("s1");
            assert!(!file.exists());
        }

        #[test]
        fn a_fresh_plain_session_on_a_reused_handle_drops_the_old_agent_and_its_settings() {
            let state = DashboardLaunchState::new(2);
            let tmp = tempfile::tempdir().unwrap();
            let file = tmp.path().join("held.json");
            std::fs::write(&file, "{}").unwrap();
            assert!(state.begin("h").unwrap());
            state.set_agent_revision_if_absent("h", "acme/api/backend/triage@1".into());
            state.hold_launch_settings(
                "h",
                crate::agents::launch::LaunchSettingsFile::adopt(file.clone()),
            );
            state.bind("h", "s1", None).unwrap();

            stamp_bound_launch(&state, "h", true, None, Some("acme:backend"));
            assert_eq!(state.snapshot()[0].agent_revision, None);
            assert!(!file.exists());
        }
    }

    /// SW-FX-launcher-persistence — AC-1..AC-4 integration tests for the on-disk
    /// launcher-provenance mirror. Each test wires persistence to a
    /// [`tempfile::TempDir`] so no test touches real `~/.codesense`.
    mod persistence_tests {
        use super::*;
        use crate::commands::dashboard_launch::persistence::{
            load_registry, PersistedLaunch, PersistedRegistry, REGISTRY_FILENAME,
        };

        fn wired_state() -> (DashboardLaunchState, tempfile::TempDir, std::path::PathBuf) {
            let state = DashboardLaunchState::new(4);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(REGISTRY_FILENAME);
            state.install_persistence(path.clone());
            (state, dir, path)
        }

        /// AC-1: committing a launch persists `launched_by` under its handle.
        #[test]
        fn begin_with_provenance_persists_launched_by() {
            let (state, _dir, path) = wired_state();
            let record = LaunchRecord {
                meta: LaunchMeta {
                    launched_by: Some("manager".into()),
                    display_name: Some("Worker".into()),
                    team: Some(LaunchTeam {
                        org: "alderco".into(),
                        team: "codesense".into(),
                    }),
                    ..Default::default()
                },
                started_with: Some("review".into()),
                max_runtime_ms: None,
            };
            assert!(state
                .begin_with_provenance("worker-handle", None, record)
                .unwrap());

            let registry = load_registry(&path);
            let persisted = registry
                .launches
                .get("worker-handle")
                .expect("worker-handle must be persisted");
            assert_eq!(persisted.launched_by.as_deref(), Some("manager"));
            assert_eq!(persisted.handle, "worker-handle");
            assert_eq!(
                persisted.team_key.as_deref(),
                Some("alderco:codesense"),
                "team_key is included for display",
            );
            assert!(persisted.launched_at.is_some(), "launched_at is stamped");
        }

        /// A launch with no `launched_by` (a human-launched root) does not pollute
        /// the file — the registry is only interesting when it has edges to rebuild.
        #[test]
        fn begin_with_no_launched_by_leaves_file_empty() {
            let (state, _dir, path) = wired_state();
            assert!(state
                .begin_with_provenance("root-handle", None, LaunchRecord::default())
                .unwrap());

            let registry = load_registry(&path);
            assert!(
                !registry.launches.contains_key("root-handle"),
                "a root launch has no edge to persist",
            );
        }

        #[test]
        fn workspace_stamp_persists_for_unbounded_launch_and_release_prunes_it() {
            let (state, _dir, path) = wired_state();
            state.begin("human-launch").unwrap();
            state.bind("human-launch", "session-a", None).unwrap();
            state.set_worktree_dir_if_absent("human-launch", "/worktree/a".into());

            let registry = load_registry(&path);
            let value =
                serde_json::to_value(registry.launches.get("human-launch").unwrap()).unwrap();
            assert_eq!(value["worktreeDir"], "/worktree/a");
            assert_eq!(value["sessionId"], "session-a");

            state.release_for_session("session-a");
            assert!(!load_registry(&path).launches.contains_key("human-launch"));
        }

        #[test]
        fn unbounded_bind_after_workspace_stamp_persists_session_and_workspace() {
            let (state, _dir, path) = wired_state();
            state.begin("worker").unwrap();
            state.set_worktree_dir_if_absent("worker", "/worktree/a".into());
            state.bind("worker", "session-a", None).unwrap();

            let registry = load_registry(&path);
            let value = serde_json::to_value(registry.launches.get("worker").unwrap()).unwrap();
            assert_eq!(value["worktreeDir"], "/worktree/a");
            assert_eq!(value["sessionId"], "session-a");

            state.release_for_session("session-a");
            assert!(!load_registry(&path).launches.contains_key("worker"));
        }

        #[test]
        fn unbounded_stamp_after_bind_persists_session_and_workspace() {
            let (state, _dir, path) = wired_state();
            state.begin("worker").unwrap();
            state.bind("worker", "session-a", None).unwrap();
            state.set_worktree_dir_if_absent("worker", "/worktree/a".into());

            let registry = load_registry(&path);
            let value = serde_json::to_value(registry.launches.get("worker").unwrap()).unwrap();
            assert_eq!(value["worktreeDir"], "/worktree/a");
            assert_eq!(value["sessionId"], "session-a");
        }

        #[test]
        fn workspace_stamp_does_not_wait_on_the_registry_write() {
            let (state, _dir, _path) = wired_state();
            let state = std::sync::Arc::new(state);
            state.set_async_persist_for_test(true);
            state.begin("worker").unwrap();
            let hold = state.hold_persistence_for_test();
            let (done, stamped) = std::sync::mpsc::channel();
            let stamper = {
                let state = state.clone();
                std::thread::spawn(move || {
                    state.set_worktree_dir_if_absent("worker", "/worktree/a".into());
                    done.send(()).unwrap();
                })
            };
            let waited_on_disk = stamped
                .recv_timeout(std::time::Duration::from_secs(3))
                .is_err();
            drop(hold);
            stamper.join().unwrap();
            assert!(!waited_on_disk, "the stamp waited on the registry write");
        }

        #[test]
        fn unbounded_bind_without_workspace_or_deadline_persists_nothing() {
            let (state, _dir, path) = wired_state();
            state.begin("worker").unwrap();
            state.bind("worker", "session-a", None).unwrap();
            assert!(!load_registry(&path).launches.contains_key("worker"));
        }

        #[test]
        fn first_worktree_stamp_is_persisted_and_later_stamps_do_not_replace_it() {
            let (state, _dir, path) = wired_state();
            state.begin("worker").unwrap();
            state.bind("worker", "session-a", None).unwrap();
            state.set_worktree_dir_if_absent("worker", "/worktree/first".into());
            state.set_worktree_dir_if_absent("worker", "/worktree/second".into());

            let registry = load_registry(&path);
            let value = serde_json::to_value(registry.launches.get("worker").unwrap()).unwrap();
            assert_eq!(value["worktreeDir"], "/worktree/first");
        }

        #[test]
        fn old_persisted_launch_without_workspace_fields_still_loads() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(REGISTRY_FILENAME);
            std::fs::write(
                &path,
                r#"{"version":1,"launches":{"legacy":{"handle":"legacy","launchedBy":"manager"}}}"#,
            )
            .unwrap();
            let registry = load_registry(&path);
            let value = serde_json::to_value(registry.launches.get("legacy").unwrap()).unwrap();
            assert!(value.get("worktreeDir").is_none());
            assert!(value.get("sessionId").is_none());
            assert_eq!(value["launchedBy"], "manager");
        }

        /// AC-2: `install_persistence` reads a round-trip file and populates the
        /// in-memory snapshot. The lookup side (`persisted_launched_by`) returns
        /// the stored launcher.
        #[test]
        fn install_persistence_round_trips_launched_by() {
            // Pre-seed a registry file.
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(REGISTRY_FILENAME);
            let mut seed = PersistedRegistry::default();
            seed.launches.insert(
                "jimmy-helper".into(),
                PersistedLaunch {
                    launcher_context: None,
                    launched_by: Some("jimmy".into()),
                    launched_at: Some("2026-10-06T12:00:00Z".into()),
                    team_key: Some("alderco:codesense".into()),
                    handle: "jimmy-helper".into(),
                    deadline_ms: None,
                    session_id: None,
                    worktree_dir: None,
                },
            );
            persistence::write_registry(&path, &seed).unwrap();

            // Fresh state wires persistence and must read the seeded file.
            let state = DashboardLaunchState::new(4);
            state.install_persistence(path);

            assert_eq!(
                state.persisted_launched_by("jimmy-helper").as_deref(),
                Some("jimmy"),
            );
            assert!(state.persisted_launched_by("absent").is_none());
        }

        /// AC-2: a missing file is treated as empty, never a panic.
        #[test]
        fn install_persistence_missing_file_returns_default() {
            let state = DashboardLaunchState::new(4);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("does-not-exist.json");
            state.install_persistence(path);
            assert!(state.persisted_launched_by("anything").is_none());
        }

        /// AC-2: a corrupt file is treated as empty, never a panic.
        #[test]
        fn install_persistence_corrupt_file_returns_default() {
            let state = DashboardLaunchState::new(4);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(REGISTRY_FILENAME);
            std::fs::write(&path, "this is not json").unwrap();
            state.install_persistence(path);
            assert!(state.persisted_launched_by("anything").is_none());
        }

        /// AC-3: a session reregistered after an app restart carries the
        /// `launched_by` the on-disk map remembered.
        #[test]
        fn register_owned_enriches_launched_by_from_disk() {
            let (state, _dir, _path) = wired_state();
            // Pretend a prior app process launched a worker under a manager —
            // its begin_with_provenance mirror lands in the file.
            assert!(state
                .begin_with_provenance(
                    "worker",
                    None,
                    LaunchRecord {
                        meta: LaunchMeta {
                            launched_by: Some("manager".into()),
                            ..Default::default()
                        },
                        started_with: None,
                        max_runtime_ms: None,
                    },
                )
                .unwrap());

            // Release it so the in-memory entry is gone — the on-disk record
            // alone must carry the launcher relationship forward.
            // (Simulating "app restart kept the session server, lost in-memory state".)
            // No-op release: worker was never bound to a session.
            state.release_for_session("never-bound");
            // Direct removal to simulate the "fresh in-memory state" case: the
            // on-disk mirror must still have the record because the release
            // above never fired against the worker handle.
            {
                let mut inner = state.lock();
                inner.by_handle.remove("worker");
            }

            // Session server now re-announces the worker; we `register_owned`
            // (the cap-free reconnect path).
            let registered = state
                .register_owned("worker", "s-worker", Some("codesense"), None)
                .expect("register must succeed");
            assert!(registered);

            // The fresh entry carries the persisted launched_by.
            let snapshot = state.snapshot();
            let row = snapshot
                .iter()
                .find(|r| r.handle == "worker")
                .expect("worker row present");
            assert_eq!(row.meta.launched_by.as_deref(), Some("manager"));
        }

        /// AC-4: a terminal session releases its entry from disk atomically.
        #[test]
        fn release_for_session_prunes_the_on_disk_record() {
            let (state, _dir, path) = wired_state();
            assert!(state
                .begin_with_provenance(
                    "worker",
                    None,
                    LaunchRecord {
                        meta: LaunchMeta {
                            launched_by: Some("manager".into()),
                            ..Default::default()
                        },
                        started_with: None,
                        max_runtime_ms: None,
                    },
                )
                .unwrap());
            state.bind("worker", "s-worker", None).unwrap();
            assert!(load_registry(&path).launches.contains_key("worker"));

            let released = state.release_for_session("s-worker");
            assert!(released.is_some());
            let registry = load_registry(&path);
            assert!(
                !registry.launches.contains_key("worker"),
                "the on-disk record is pruned when the session goes terminal",
            );
        }

        /// `abort` is the failure-path counterpart to release and must also prune.
        #[test]
        fn abort_prunes_the_on_disk_record() {
            let (state, _dir, path) = wired_state();
            assert!(state
                .begin_with_provenance(
                    "worker",
                    None,
                    LaunchRecord {
                        meta: LaunchMeta {
                            launched_by: Some("manager".into()),
                            ..Default::default()
                        },
                        started_with: None,
                        max_runtime_ms: None,
                    },
                )
                .unwrap());
            assert!(load_registry(&path).launches.contains_key("worker"));

            assert!(state.abort("worker"));
            let registry = load_registry(&path);
            assert!(!registry.launches.contains_key("worker"));
        }

        /// With persistence NOT installed, every path is a no-op — the pre-fix
        /// behavior is preserved exactly so a signed-out boot still launches.
        #[test]
        fn without_install_persistence_mutations_are_noops() {
            let state = DashboardLaunchState::new(4);
            // No install_persistence call — persistence is None.
            assert!(state
                .begin_with_provenance(
                    "worker",
                    None,
                    LaunchRecord {
                        meta: LaunchMeta {
                            launched_by: Some("manager".into()),
                            ..Default::default()
                        },
                        started_with: None,
                        max_runtime_ms: None,
                    },
                )
                .unwrap());
            // No file to inspect; the lookup must still be safe.
            assert!(state.persisted_launched_by("worker").is_none());
            // And release does not panic.
            state.bind("worker", "s-worker", None).unwrap();
            state.release_for_session("s-worker");
        }

        /// A second `install_persistence` for the SAME path is a no-op (idempotent).
        /// A call with a DIFFERENT path replaces the store (principal switch
        /// mid-process starts fresh against the new user's file).
        #[test]
        fn install_persistence_is_idempotent_for_the_same_path() {
            let (state, _dir, path) = wired_state();
            // Second install for the same path leaves the hydrated state in place.
            state.install_persistence(path.clone());

            // Now install a DIFFERENT path and verify it replaced.
            let other_dir = tempfile::tempdir().unwrap();
            let other_path = other_dir.path().join(REGISTRY_FILENAME);
            let mut other = PersistedRegistry::default();
            other.launches.insert(
                "other-handle".into(),
                PersistedLaunch {
                    launcher_context: None,
                    launched_by: Some("someone-else".into()),
                    launched_at: None,
                    team_key: None,
                    handle: "other-handle".into(),
                    deadline_ms: None,
                    session_id: None,
                    worktree_dir: None,
                },
            );
            persistence::write_registry(&other_path, &other).unwrap();
            state.install_persistence(other_path);
            assert_eq!(
                state.persisted_launched_by("other-handle").as_deref(),
                Some("someone-else"),
            );
        }
    }
}

#[cfg(test)]
mod human_input_tests {
    use super::*;

    fn bounded(state: &DashboardLaunchState, max_runtime_ms: u64) -> u64 {
        assert!(state
            .begin_with_provenance(
                "W",
                None,
                LaunchRecord {
                    meta: LaunchMeta::default(),
                    started_with: None,
                    max_runtime_ms: Some(max_runtime_ms),
                },
            )
            .unwrap());
        state.bind("W", "sid-w", Some("codesense")).unwrap();
        deadline_of(state)
    }

    fn deadline_of(state: &DashboardLaunchState) -> u64 {
        state
            .deadline_snapshot()
            .into_iter()
            .find(|entry| entry.session_id == "sid-w")
            .expect("bounded entry")
            .deadline_ms
    }

    #[test]
    fn the_deadline_moves_to_a_grace_after_the_keystroke_rounded_to_the_minute() {
        let now = 10 * 60_000 + 1;
        let want = now + HUMAN_ACTIVE_GRACE_MS + 60_000 - 1;
        assert_eq!(deadline_after_human_input(now, now), Some(want));
        assert_eq!(
            deadline_after_human_input(now + HUMAN_ACTIVE_GRACE_MS + 120_000, now),
            None
        );
    }

    #[test]
    fn typing_near_the_deadline_keeps_the_session_running() {
        let state = DashboardLaunchState::new(4);
        let deadline = bounded(&state, 30 * 60_000);
        // Long before the deadline nothing moves.
        state.note_human_input("sid-w", deadline - 25 * 60_000);
        assert_eq!(deadline_of(&state), deadline);
        // A keystroke a minute before it pushes it to at least 15 minutes later.
        let now = deadline - 60_000;
        state.note_human_input("sid-w", now);
        assert!(deadline_of(&state) >= now + HUMAN_ACTIVE_GRACE_MS);
        // A session nobody bounded, or nobody knows, is left alone.
        state.note_human_input("sid-unknown", now);
    }

    #[test]
    fn a_sweep_holding_the_old_deadline_cannot_send_exit_after_an_extension() {
        let state = DashboardLaunchState::new(4);
        let deadline = bounded(&state, 30 * 60_000);
        let stale = state
            .deadline_snapshot()
            .into_iter()
            .find(|entry| entry.session_id == "sid-w")
            .unwrap();
        state.note_human_input("sid-w", deadline - 60_000);
        assert!(!state.deadline_is_current(&stale));
        assert!(!state.mark_exit_sent(&stale, deadline));
    }

    #[test]
    fn typing_after_exit_was_sent_does_not_undo_the_stop() {
        let state = DashboardLaunchState::new(4);
        let deadline = bounded(&state, 30 * 60_000);
        let entry = state
            .deadline_snapshot()
            .into_iter()
            .find(|entry| entry.session_id == "sid-w")
            .unwrap();
        assert!(state.mark_exit_sent(&entry, deadline));
        state.note_human_input("sid-w", deadline + 1_000);
        assert_eq!(deadline_of(&state), deadline);
    }
}

#[cfg(test)]
mod context_launcher_tests {
    use super::*;

    #[test]
    fn launcher_context_is_captured_and_remains_after_the_launcher_exits() {
        let fx = crate::agents::prepare::step3::tests::Fx::new();
        let row = fx.context("manager");
        let state = DashboardLaunchState::new(3);
        state.begin("manager").unwrap();
        state.bind("manager", "manager-session", None).unwrap();
        state.set_worktree_dir_if_absent("manager", row.launch_cwd.clone());
        let captured = state.context_of_launcher(fx.home.path(), Some("manager-session"));
        assert_eq!(captured.as_deref(), Some(row.id.as_str()));
        state
            .begin_with_provenance(
                "story-agent",
                None,
                LaunchRecord {
                    meta: LaunchMeta {
                        launched_by: Some("manager".into()),
                        launcher_context: captured,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        state.bind("story-agent", "child-session", None).unwrap();
        state.release_for_session("manager-session").unwrap();
        assert_eq!(
            state.launcher_context("child-session").as_deref(),
            Some(row.id.as_str())
        );
        assert_eq!(
            state.context_of_launcher(fx.home.path(), Some("manager")),
            None
        );
        let mut ctx = crate::content_engine::ResolverContext {
            context_root: None,
            codesense_home: fx.home.path().into(),
            user_id: 1,
            org_slug: row.org,
            team_name: Some(row.team),
        };
        crate::mcp::context_operations::bind_context(
            &mut ctx,
            None,
            state.launcher_context("child-session").as_deref(),
        )
        .unwrap();
        assert!(ctx.context_root.is_none());
        crate::mcp::context_operations::bind_context(
            &mut ctx,
            Some(crate::mcp::context_operations::ContextSelection::Launcher),
            state.launcher_context("child-session").as_deref(),
        )
        .unwrap();
        assert_eq!(ctx.context_root.unwrap().id, row.id);
    }
}

#[cfg(test)]
pub(crate) mod attribution_entry_tests {
    //! SV-CDE-7: the headless (routine) launch's provisioning switches git-ai capture on for
    //! a managed worktree, and never fails the launch when git-ai is not usable.
    use super::*;
    use crate::attribution::testing::{assert_unwired, assert_wired, Launch};
    use crate::attribution::{AttributionStatus, OffReason};
    use codesense_pty_core::agent_cli::AgentCli;

    pub(crate) fn provisioned(
        fixture: &Launch,
    ) -> (
        Vec<String>,
        Vec<(String, String)>,
        Option<serde_json::Value>,
        String,
    ) {
        let id = format!("headless-attr-{}", uuid::Uuid::new_v4());
        let (scope, applied) = provision_headless_scope(&fixture.cwd, &id, &fixture.launch, None)
            .expect("the launch itself succeeds");
        let mut args = fixture.launch.launch.args.clone();
        args.extend(applied.argv(str::to_string));
        let settings = applied
            .settings
            .as_ref()
            .map(|s| serde_json::from_slice(&std::fs::read(s.path()).unwrap()).unwrap());
        (args, scope.extra_env, settings, id)
    }

    #[test]
    fn claude_and_codex_launch_with_the_hooks_and_trace_env() {
        let _home_env = crate::routines::test_support::TempHome::new();
        for runtime in [AgentCli::Claude, AgentCli::Codex] {
            let fixture = Launch::new(runtime);
            let _daemon = fixture.daemon_running();
            let (args, env, settings, id) = provisioned(&fixture);
            assert_wired(runtime, &args, settings.as_ref(), &env);
            assert_eq!(fixture.status(), Some(AttributionStatus::Active));
            crate::commands::mcp_scope::remove_scope_config(&id);
        }
    }

    #[test]
    fn opencode_off_repos_and_a_missing_git_ai_launch_unwired() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let opencode = Launch::new(AgentCli::OpenCode);
        let (args, env, settings, id) = provisioned(&opencode);
        assert_unwired(&args, settings.as_ref(), &env);
        crate::attribution::testing::assert_reflog_marker(&env, Some("opencode"));
        assert_eq!(
            opencode.status(),
            Some(AttributionStatus::Unsupported {
                reason: "opencode".into()
            })
        );
        crate::commands::mcp_scope::remove_scope_config(&id);

        let off = Launch::new(AgentCli::Claude);
        off.set_prefs(crate::attribution::prefs::AttributionPrefs {
            mode: Some(crate::attribution::prefs::AttributionMode::Off),
            ..Default::default()
        });
        let (args, env, settings, id) = provisioned(&off);
        assert_unwired(&args, settings.as_ref(), &env);
        assert_eq!(
            off.status(),
            Some(AttributionStatus::Off {
                reason: OffReason::DisabledForRepo
            })
        );
        crate::commands::mcp_scope::remove_scope_config(&id);

        for runtime in [AgentCli::Claude, AgentCli::Codex] {
            let missing = Launch::new(runtime);
            missing.uninstall();
            let (args, env, settings, id) = provisioned(&missing);
            assert_unwired(&args, settings.as_ref(), &env);
            assert_eq!(missing.status(), Some(AttributionStatus::NotInstalled));
            crate::commands::mcp_scope::remove_scope_config(&id);
        }
    }
}
