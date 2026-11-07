// SPDX-License-Identifier: MIT
//! SW-GT-13 — the durable, machine-local run/verdict store.
//!
//! The single durable home for pipeline *execution telemetry*: every status
//! advance (auto or human override), every per-step validation verdict, and every
//! action config-set edit. It lives at `{codesense_home}/run-store.jsonl`,
//! **out of the story's frontmatter by design** — `status` (frontmatter) is the
//! load-bearing, git-synced gate position; the records here are machine-local
//! audit that never rides content-sync (AC-8) and never derives or overwrites the
//! story doc.
//!
//! **Event-sourced, read via fold/projection (WAL / event-sourcing).** The store
//! is never an in-place-mutated state file: records are appended, and the read
//! model ([`GateStateView`], AC-7a) is a projection computed by folding the log —
//! latest-verdict-per-`step_id`, then the ∀-steps conjunction for the rolled-up
//! outcome. That is what makes both *resume* (SW-GT-15 AC-4) and *history* fall
//! out of one artifact.
//!
//! **Durability discipline reused from `agent_audit.rs` (SW-DX-17a), NOT its
//! bare-synchronous I/O.** The append is `create(true).append(true)`, one JSON
//! line, never fatal (a failed append is a `warn!` + return — a status advance is
//! never failed because its audit line could not be written). Unlike
//! `agent_audit` (rare, tiny, write-only → safe bare-synchronous), this store also
//! *folds* a growing file and its reads are invoked from Tauri commands, so
//! **every read/write MUST run off the async runtime via `await_blocking`**. The
//! SLW-RI-2 backstop enforces that discipline.
//!
//! **Legacy gate records.** Per-step `verdict` and `retry_progress` records were
//! written by the retired status gate. Nothing writes them any more, but they still
//! decode (run evidence and identity read them for old runs) and compaction still
//! collapses them.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::commands::paths::codesense_home_dir;
use crate::dashboard_stream::SessionStatus;

/// The store's on-disk schema version. Bumped only for a shape change the reader
/// can't tolerate additively. A record whose `schema_version` is greater than this
/// is from a newer app and is **skipped with a counted warning** (AC-9), never a
/// panic — append-only, no in-place migration.
pub const SCHEMA_VERSION: u32 = 1;

/// Process-local append sequence. Disambiguates two records written in the same
/// millisecond within one process; wall-clock `ts` orders across restarts. The
/// fold sorts by `(ts, seq)`, so a reset-to-0 on restart is harmless (a restart
/// implies a later `ts`).
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Serializes the two operations that *mutate* the store file — the append
/// (`append_to`) and the SW-GT-25 compaction swap (`compaction::compact_store`).
/// The store has a **single writer process** (this app; every append originates on
/// a reactor/command thread), so a process-global `Mutex` is sufficient — no
/// cross-process / advisory file lock is needed. It exists so a compaction pass
/// (read → rewrite → atomic rename) can never lose a verdict appended mid-pass:
/// the append and the swap are mutually exclusive. Reads (`read_records`, the fold)
/// are lock-free — they tolerate a concurrent atomic rename by design.
static STORE_MUTATION: Mutex<()> = Mutex::new(());

/// Run `f` holding the store-mutation lock. Poison-tolerant (`into_inner`): a prior
/// panic while holding the lock must not wedge every future append — the store's
/// whole contract is never-fatal, so a poisoned lock degrades to "proceed", never
/// to a panic.
pub(crate) fn with_store_lock<R>(f: impl FnOnce() -> R) -> R {
    let _guard = STORE_MUTATION.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

/// A step's recorded outcome. Wire values are `passed` | `failed` | `pending`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepResult {
    Passed,
    Failed,
    Pending,
}

/// How a recorded step outcome was reached. Wire values are `signal` | `evaluator` | `human`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// The agent's own completion signal.
    Signal,
    /// A validator command ran trusted-side.
    Evaluator,
    /// A human asserted it.
    Human,
}

/// What moved the story across a gate — the actor *role*, never an agent vendor
/// (epic-08 neutrality).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trigger {
    /// A human moved the story (the reasoned override path, AC-1).
    Human,
    /// The reactor advanced it on a validated gate (or parked it on fail).
    AutoRun,
}

// ---------------------------------------------------------------------------
// Record envelope + event kinds
// ---------------------------------------------------------------------------

/// One appended line. The `schema_version` / `seq` / `ts` envelope wraps a tagged
/// [`RunEvent`]; every record carries enough identity
/// (`story_id`, `action_identity`, `config_version`, `step_id`, `kind`, `seq`/`ts`)
/// that SW-GT-25 compaction can collapse superseded verdicts to the latest per
/// `(story_id, action_identity, config_version, step_id)` with **no migration**
/// (AC-11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreRecord {
    pub schema_version: u32,
    pub seq: u64,
    /// Unix epoch milliseconds (append order across restarts).
    pub ts: u128,
    #[serde(flatten)]
    pub event: RunEvent,
}

/// The tagged event payload. An unknown `kind` from a newer writer fails to
/// deserialize and is skipped-with-count by the reader (AC-9), same as a torn
/// line — never a panic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunEvent {
    /// A status advance — gate-validated or human override (AC-2).
    Advance(AdvanceEvent),
    /// A per-step validation verdict (AC-6). SW-GT-15's reactor appends these (a
    /// purely additive `append(RunEvent::Verdict(..))` call) as it validates each
    /// step; the ledger lives here, never in frontmatter. GT-13 ships the sink +
    /// fold read model that hold and project them.
    Verdict(VerdictEvent),
    /// An action config-set edit (AC-5) — who changed which action's step set,
    /// when. A distinct kind from an advance; surfaced by SW-GT-17 AC-4.
    ConfigEdit(ConfigEditEvent),
    /// SW-GT-24: a **whole-gate retry** attempt's cursor — the durable state a
    /// [[SW-GT-23]] `on_fail: { retry }` route writes so the reactor can re-admit a
    /// due retry (`next_run_at`) up to `max` attempts. Keyed the same way a
    /// [`VerdictEvent`] is (`story_id` + `action_identity` + `config_version`) so the
    /// fold scopes it to the same gate and SW-GT-25 compaction collapses superseded
    /// lines to the latest per key. GT-24 owns this record kind, its fold into
    /// [`GateStateView::retry`], the `record_retry_progress` writer, and the pure
    /// `due_retries` read; GT-23 owns the *policy* that decides `attempt`/`max`/
    /// `backoff` and calls the writer. This is a store *record* kind, not a new
    /// verdict/outcome kind.
    RetryProgress(RetryProgressEvent),
    /// SW-AU-52: a run's terminal **outcome** (see [`RunOutcomeEvent`]) — the run-level roll-up
    /// written once when a run completes, so a fire-and-exit headless run's result survives its
    /// process and a restart. Appended by BOTH completion paths (the `signal_done` handler and
    /// the headless `run_headless_to_completion`) through the one [`record_run_outcome`] writer;
    /// folded latest-wins per `run_id` by [`latest_run_outcome`]. Distinct from a per-step
    /// [`VerdictEvent`]: this is the single run verdict, not a step in the DAG.
    Outcome(Box<RunOutcomeEvent>),
    /// SW-AU-96: a per-step reported result (see [`StepResultEvent`]) — the
    /// summary/outputs/artifacts/footprint a step declared (via the `report_step_result` /
    /// `report_artifact` MCP tools) or the harness auto-captured. Scrubbed before append; folded by
    /// `build_run_evidence` into `RunEvidence.per_node_results`. Distinct from a per-step
    /// [`VerdictEvent`] (pass/fail) and a run-level [`RunOutcomeEvent`] (the terminal roll-up): this
    /// records what a node *produced and touched*, keyed by `(story_id, run_id, node_id)`.
    StepResult(StepResultEvent),
}

/// A status advance record (AC-2). On a pure override the `steps` snapshot carries
/// the partial per-step verdict state at override time (passed/pending per step)
/// plus `overridden: true` + the `reason`, so "advanced without validation" is
/// never silently empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvanceEvent {
    pub story_id: String,
    /// Prior status (`None` if the writer could not read it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub to: String,
    pub trigger: Trigger,
    #[serde(default, skip_serializing_if = "is_false")]
    pub overridden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Partial per-step verdict snapshot at advance time (passed/pending per step).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Actor in the spec grammar (`human:<login>` / `codesense` / `agent:<profile>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
}

/// One step's outcome captured in an advance snapshot (AC-2). Compact by design —
/// the full verdict lives in its own [`VerdictEvent`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepSnapshot {
    pub step_id: String,
    pub result: StepResult,
}

/// A per-step verdict record (AC-6). The verdict's *outcome* is recorded here; the
/// step's *prompt*/*validator* are repo-scoped slots resolved at dispatch
/// (SW-GT-27), so `action_identity` + `config_version` identify the invariant step
/// while the command that actually ran was repo-resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerdictEvent {
    pub story_id: String,
    /// A phase transition's `from→to` edge, or a named action's `id` (SW-GT-14).
    pub action_identity: String,
    /// The `RunProvenance` join key alongside `story_id`.
    pub run_id: String,
    pub step_id: String,
    pub result: StepResult,
    /// RFC-3339 timestamp.
    pub at: String,
    /// Actor role/profile (`human:<login>` / `codesense` / `agent:<profile>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    pub via: Via,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// Stable content hash of the resolved Action (SW-GT-15 owns the derivation).
    pub config_version: String,
}

/// An action config-set edit record (AC-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigEditEvent {
    /// The team whose config changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Which action's step set changed (`from→to` edge or named id), if scoped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_identity: Option<String>,
    /// RFC-3339 timestamp.
    pub at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// The new `config_version` after the edit, if the writer computed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_version: Option<String>,
}

/// SW-GT-24: one whole-gate retry attempt's durable cursor — keyed by
/// `(story_id, action_identity, config_version)`. Written by the retired status gate's
/// `on_fail: { retry }` policy; nothing re-admits them since the sprint auto-run retired, and
/// old lines still decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryProgressEvent {
    pub story_id: String,
    /// A phase transition's `from→to` edge, or a named action's `id` (SW-GT-14).
    pub action_identity: String,
    pub run_id: String,
    /// Stable content hash of the resolved Action (scopes to the live config).
    pub config_version: String,
    /// Attempts consumed so far (1 after the first fail records a retry).
    pub attempt: u32,
    /// The bounded ceiling (GT-23's `retry.max`) — the hard cap re-admission honors.
    pub max: u32,
    /// RFC-3339 time the next attempt is armed for (`now + backoff`); absent once the
    /// retry is exhausted / routed away (no further re-admission).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<String>,
    /// The state the fall-through route lands on when attempts are exhausted (default
    /// `needs-attention`); carried for the drawer's read-back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routed_to: Option<String>,
    /// True once `attempt >= max` (or the policy gave up) — the terminal marker that
    /// drops the story from the due scan and routes it to `routed_to`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub exhausted: bool,
    /// SW-GT-23: the dispatch-episode nonce (from the story's claim) this cursor was armed
    /// under. Fresh each re-dispatch, so the retry seam can tell a same-episode parallel sibling
    /// step (same nonce) from a genuine re-run (new nonce) without a timing heuristic. Absent on
    /// a cursor armed outside a live claim (e.g. a human drawer assert); GT-24 ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_nonce: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// SW-AU-52: where a run's terminal outcome came from — an agent's self-attested `signal_done`
/// (`Signal`) or an inferred process exit-classification (`ProcessExit`, the headless completion
/// path). Recorded as honest provenance so SW-AU-55 can render a self-report distinctly and
/// SW-AU-54 can weight it; it does **not** drive the fold ([`latest_run_outcome`] is plain
/// latest-wins) because a given `run_id` gets exactly one source today.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeSource {
    Signal,
    ProcessExit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunWorkProductProvenance {
    pub source_repo: crate::domain::SourceRepoSlug,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<crate::domain::CredentialRef>,
}

/// SW-AU-52: a run's terminal outcome — the minimal run-level roll-up (not a transcript, not a
/// per-step [`VerdictEvent`]). Keyed by `run_id` (scoped to `story_id` for the fold, matching every
/// other event). The optional provenance fields are copied from the live launch entry at completion
/// so history remains attributable after that entry is reaped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOutcomeEvent {
    pub story_id: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_product: Option<RunWorkProductProvenance>,
    pub status: SessionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The process exit code when known (headless); `None` for a self-attested signal or a
    /// killed/timed-out child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// RFC-3339 run start, when the launch birth time is resolvable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// RFC-3339 completion time.
    pub ended_at: String,
    /// An optional pointer to the run's artifact (not the artifact itself — full capture is out of
    /// scope, SW-AU-52).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    /// SW-BA-8: the validator gate (a step id) a run recorded before the status gate retired.
    /// Read-only: new outcomes leave it `None`, so it is never emitted for them. It still
    /// round-trips through compaction so an old record keeps its legacy verdict join
    /// ([`project_validated_outcome`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validator_step: Option<String>,
    /// SW-OR-15: `Some(true)` when this run declared an interactive structured-transcript capability
    /// (its pack named a session-log glob). It lets the panel tell a benign "no output captured" (no
    /// capability) apart from a "declared but no turns landed" degrade (crash / torn / locked file) —
    /// the interactive arm writes no `.log` floor, so those two otherwise look identical. `None` for a
    /// headless run or an interactive run with no declared capability. Additive + defaulted (like
    /// `artifact_ref` / `validator_step`), so `SCHEMA_VERSION` is unchanged and an old record reads
    /// back as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_declared: Option<bool>,
    /// The org and team the run was launched under: the trusted launch owner, never derived from
    /// the story name. Additive + defaulted, so `SCHEMA_VERSION` is unchanged and a row written
    /// before this reads back as `None` (shown as `unlinked`, never rewritten).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// The CodeSense session id(s) that did the run's work; `session_record` and `list_sessions`
    /// take them. Empty for a row written before this.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_ids: Vec<String>,
    pub source: OutcomeSource,
}

/// Whether a run's outcome names the session that did the work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunLink {
    Linked,
    #[default]
    Unlinked,
}

impl RunLink {
    fn of(outcome: &RunOutcomeEvent) -> Self {
        if outcome.session_ids.is_empty() {
            Self::Unlinked
        } else {
            Self::Linked
        }
    }
}

/// SW-AU-96: the kind of a reported artifact or footprint entry — a file on disk, a PR/MR, or an
/// external entity (issue/record). Drives SW-AU-97's per-kind affordance (preview / open / deep-link)
/// and is the dedup discriminator alongside the `reference`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    File,
    Pr,
    Entity,
}

impl ArtifactKind {
    /// Parse the wire token an MCP reporting tool receives (`file` | `pr` | `entity`) — the boundary
    /// validation so a bad kind is a typed error, never a silently-stored garbage variant.
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw.trim() {
            "file" => Some(Self::File),
            "pr" => Some(Self::Pr),
            "entity" => Some(Self::Entity),
            _ => None,
        }
    }
}

/// SW-AU-96: one named artifact a step produced — a stable ref (path / URL / id) plus its kind and an
/// optional human label. Provenance is structural, not duplicated: an artifact is only ever read
/// inside a [`crate::commands::sprint_run::run_evidence::RunEvidence`] keyed by `run_id`, so the run
/// linkage a mint (SW-AU-94) needs is the header's `run_id` — one fact, one owner (ARCH-INV-24).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepArtifact {
    pub kind: ArtifactKind,
    /// The stable reference: a repo-relative path, a PR/MR URL, or an entity id.
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// SW-AU-96: whether a footprint entry was created or modified by the step (Dagster-materialization
/// style). On a dedup collision for one `(kind, reference)`, `Created` wins over `Modified` — a
/// create-then-modify is still a create for provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FootprintChange {
    Created,
    Modified,
}

impl FootprintChange {
    /// Parse the wire token an MCP reporting tool receives (`created` | `modified`). An absent value
    /// defaults to `Modified` at the call site; an unrecognized one is a typed error.
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw.trim() {
            "created" => Some(Self::Created),
            "modified" => Some(Self::Modified),
            _ => None,
        }
    }
}

/// SW-AU-96: one entity a step touched — the same typed ref as an artifact, plus whether it was
/// created or modified. Deduped by `(kind, reference)` when folded, so a harness auto-captured write
/// and an agent-reported one converge to a single entry rather than double-counting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FootprintEntry {
    pub kind: ArtifactKind,
    pub reference: String,
    pub change: FootprintChange,
}

/// SW-AU-96: one typed output key/value a step declared (GitHub Actions / Dagster `outputs` style).
/// `value` is arbitrary JSON so SW-AU-97 can render a table/JSON toggle; it is scrubbed at write time
/// through [`crate::commands::sprint_run::causation::scrub_payload`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepOutput {
    pub key: String,
    pub value: serde_json::Value,
}

/// SW-AU-96: the default step/node id a reported result attaches to when the agent names none. Shared
/// by the MCP tools AND the harness work-product auto-capture so both land on the SAME node (they
/// converge to one `PerNodeResult` rather than splitting into two rows for one agent step).
pub const DEFAULT_STEP_NODE_ID: &str = "agent";

/// SW-AU-96: the per-channel entry-count cap (AC-5 "bounded"). The `scrub_payload` size bound guards
/// each value; this guards the *count*, so a looping/misbehaving agent cannot grow one event without
/// limit. Generous — a real step reports a handful.
pub const MAX_REPORTED_ENTRIES: usize = 256;

/// SW-AU-96: a per-step reported result (AC-1/AC-4) — one additive event kind appended to the same
/// JSONL ledger, keyed by `(story_id, run_id, node_id)`. Both MCP reporting tools
/// (`report_step_result`, `report_artifact`) and the harness auto-capture funnel through
/// [`record_step_result`]; the fold in `build_run_evidence` accumulates every event for a node
/// (summary/outputs latest-wins, artifacts + footprint union-deduped). Each event is passed through
/// [`StepResultEvent::scrubbed`] BEFORE it is appended (AC-5), so the ledger never holds a secret and
/// the render path can trust it. Additive + defaulted, so `SCHEMA_VERSION` is unchanged and an old
/// reader that cannot parse the `kind` skips it with a count (never a panic).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepResultEvent {
    pub story_id: String,
    pub run_id: String,
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<StepOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<StepArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footprint: Vec<FootprintEntry>,
}

impl StepResultEvent {
    /// AC-5: scrub AND bound every reported channel BEFORE the event is appended, over the ONE scrub
    /// boundary — `scrub_payload` for JSON `outputs` (redaction + `PAYLOAD_MAX_BYTES` cap → truncation
    /// marker) and its string companion `scrub_str` for `summary` / artifact `reference`+`label` /
    /// footprint `reference` (redaction + the same size cap). The per-channel entry *count* is also
    /// bounded to `MAX_REPORTED_ENTRIES`. Pure and synchronous — safe to call inline on the async
    /// executor. The ledger consequently never holds a credential-shaped or unbounded value, so the
    /// render fold trusts it.
    #[must_use]
    pub fn scrubbed(mut self) -> Self {
        use crate::commands::sprint_run::causation::{scrub_payload, scrub_str};

        self.summary = self.summary.as_deref().map(scrub_str);
        self.outputs.truncate(MAX_REPORTED_ENTRIES);
        self.artifacts.truncate(MAX_REPORTED_ENTRIES);
        self.footprint.truncate(MAX_REPORTED_ENTRIES);
        for output in &mut self.outputs {
            output.value = scrub_payload(std::mem::take(&mut output.value));
        }
        for artifact in &mut self.artifacts {
            artifact.reference = scrub_str(&artifact.reference);
            artifact.label = artifact.label.as_deref().map(scrub_str);
        }
        for entry in &mut self.footprint {
            entry.reference = scrub_str(&entry.reference);
        }
        self
    }

    /// SW-AU-96: a report with no summary and no outputs/artifacts/footprint carries no information —
    /// the caller drops it rather than appending an empty ledger record that would fold into an empty
    /// `PerNodeResult` node.
    #[must_use]
    pub fn is_empty_content(&self) -> bool {
        self.summary.as_deref().is_none_or(|s| s.trim().is_empty())
            && self.outputs.is_empty()
            && self.artifacts.is_empty()
            && self.footprint.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Read model (AC-7 / AC-7a / AC-7b)
// ---------------------------------------------------------------------------

/// One folded step row (AC-7a) — the latest verdict for a `step_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepVerdictView {
    pub step_id: String,
    pub result: StepResult,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    pub via: Via,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// SW-AU-49: the per-step verdict rows for ONE run + that run's `action_identity`
/// (derived from the matched verdicts, so the caller resolves the run's step-DAG
/// without carrying the identity separately). `action_identity` is `None` — and
/// `steps` empty — when the run recorded no verdicts (an older or non-workflow run),
/// so the caller degrades to the run-level pill rather than a blank strip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RunStepVerdicts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_identity: Option<String>,
    pub steps: Vec<StepVerdictView>,
}

/// Store health signal (AC-10) — surfaced so "the store is degraded" is legible
/// rather than a silent wrong-answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreHealth {
    /// Lines the fold could not parse (torn tail, malformed interior, unknown
    /// schema/kind) and skipped.
    pub skipped_lines: usize,
}

// ---------------------------------------------------------------------------
// Path + append (write side)
// ---------------------------------------------------------------------------

/// `{codesense_home}/run-store.jsonl`, or `None` if the home dir is unresolvable
/// (headless CI with no `$HOME`) — in which case recording no-ops. `pub(crate)` so
/// SW-GT-25 compaction targets the same path the writers/readers use.
pub(crate) fn store_path() -> Option<PathBuf> {
    codesense_home_dir().map(|h| h.join("run-store.jsonl"))
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// The advisory `human:<login>` actor for an audit `by` field — the OS user from
/// the env (`USER`/`USERNAME`), matching `agent_audit`'s advisory "who". `None` if
/// neither is set; a reader must not over-trust it (epic-08 neutrality: a role, not
/// a vendor). Shared by the override (AC-1) and config-edit (AC-5) writers.
pub fn human_actor() -> Option<String> {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .map(|u| format!("human:{u}"))
}

/// The RFC-3339 `at` timestamp an audit record stamps — the single convention point for the
/// config-edit / provenance writers, beside the audit `by` helper ([`human_actor`]).
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Append one `event` to the default store. Best-effort: resolves the path, stamps
/// the envelope, and swallows a missing home. Callers on a hot/async path must
/// wrap this in `await_blocking` — the append itself is synchronous fs.
pub fn append(event: RunEvent) {
    let Some(path) = store_path() else {
        return;
    };
    append_to(&path, event);
}

/// The path-injected core (unit-testable against a tempdir). Non-fatal on every
/// failure: creates the parent dir, opens append-only, writes one JSON line +
/// newline. A write error is logged and dropped (AC-2 never-fatal).
pub fn append_to(path: &Path, event: RunEvent) {
    use std::fs::OpenOptions;
    use std::io::Write;

    let record = StoreRecord {
        schema_version: SCHEMA_VERSION,
        seq: SEQ.fetch_add(1, Ordering::Relaxed),
        ts: now_millis(),
        event,
    };
    let line = match serde_json::to_string(&record) {
        Ok(l) => l,
        Err(e) => {
            log::warn!(target: "run_store", "record serialize failed: {e}");
            return;
        }
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Hold the store-mutation lock so an append can never interleave with a
    // compaction swap (SW-GT-25 AC-3): the compactor takes the same lock across its
    // read → rewrite → atomic rename, so no verdict written here is lost to a pass.
    let stored = with_store_lock(|| {
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(mut f) => match writeln!(f, "{line}") {
                Ok(()) => true,
                Err(e) => {
                    log::warn!(target: "run_store", "append write failed: {e}");
                    false
                }
            },
            Err(e) => {
                log::warn!(target: "run_store", "append open failed for {}: {e}", path.display());
                false
            }
        }
    });
    if stored {
        crate::live_hub::run_review::written(
            path,
            crate::live_hub::run_review::Written::Record(record),
        );
    }
}

/// Convenience: record a status advance (AC-2) — the audit seam a user's status move
/// (`move_story_status`) funnels. Best-effort.
pub fn record_status_advance(event: AdvanceEvent) {
    append(RunEvent::Advance(event));
}

/// SW-AU-52: record a run's terminal outcome (AC-1). The single seam both completion paths funnel
/// (AC-4, no headless-only fork) — the `signal_done` handler and the headless
/// `run_headless_to_completion`. Best-effort — the same never-fatal append the verdict/retry
/// writers use; a dropped line only costs this run's durable outcome (the run itself is unaffected).
/// The append is synchronous fs, so an async caller MUST wrap this in `await_blocking` (AC-5).
pub fn record_run_outcome(event: RunOutcomeEvent) {
    append(RunEvent::Outcome(Box::new(event)));
}

/// SW-AU-96: record one per-step reported result (AC-2/AC-4). The single seam both MCP reporting
/// tools AND the harness auto-capture funnel — best-effort, the same never-fatal append the
/// verdict/outcome writers use; a dropped line only costs that one report. The caller MUST pass an
/// already-[`StepResultEvent::scrubbed`] event (AC-5) and, on an async path, wrap this in
/// `await_blocking` — the append is synchronous fs.
pub fn record_step_result(event: StepResultEvent) {
    append(RunEvent::StepResult(event));
}

/// SW-AU-96: every reported-result event for ONE `run_id` (scoped to `story_id`), returned in append
/// order `(ts, seq)` so the caller can fold them per node with latest-wins on the scalar channels and
/// union-dedup on artifacts/footprint. Mirrors [`latest_run_outcome`]'s scan; the fold shaping lives
/// in `build_run_evidence` (the one evidence reader), never a second projection here.
pub fn step_result_events<'a>(
    records: &'a [StoreRecord],
    story_id: &str,
    run_id: &str,
) -> Vec<&'a StepResultEvent> {
    let story_id = story_id.trim();
    let run_id = run_id.trim();
    let mut ordered: Vec<(u128, u64, &StepResultEvent)> = Vec::new();
    for rec in records {
        let RunEvent::StepResult(event) = &rec.event else {
            continue;
        };
        if event.story_id.trim() != story_id || event.run_id.trim() != run_id {
            continue;
        }
        ordered.push((rec.ts, rec.seq, event));
    }
    ordered.sort_by_key(|(ts, seq, _)| (*ts, *seq));
    ordered.into_iter().map(|(_, _, event)| event).collect()
}

/// SW-AU-52: the latest terminal outcome for ONE `run_id` (scoped to `story_id`), folded from the
/// store by `(ts, seq)` — latest-wins, the same rule every other single-key read uses. This is the producer's observable contract that SW-AU-54 (watchdog
/// exemption) and SW-AU-55 (pane render) consume. `None` when the run never recorded an outcome —
/// still in-flight, or crashed before any write. A restart is transparent: re-reading the JSONL
/// (`read_records`) then calling this returns the persisted outcome (AC-2).
// The live consumers arrive next in the slice: SW-AU-54 (watchdog terminal-outcome exemption) and
// SW-AU-55 (pane render). SW-OR-12's `read_run_outcome` command is the first FE-facing consumer.
pub fn latest_run_outcome<'a>(
    records: &'a [StoreRecord],
    story_id: &str,
    run_id: &str,
) -> Option<&'a RunOutcomeEvent> {
    let story_id = story_id.trim();
    let mut best: Option<(u128, u64, &RunOutcomeEvent)> = None;
    for rec in records {
        let RunEvent::Outcome(o) = &rec.event else {
            continue;
        };
        if o.story_id.trim() != story_id || o.run_id != run_id {
            continue;
        }
        if best.is_none_or(|(ts, seq, _)| (rec.ts, rec.seq) >= (ts, seq)) {
            best = Some((rec.ts, rec.seq, o));
        }
    }
    best.map(|(_, _, o)| o)
}

/// SW-OR-12/OR-13 (AC-1): the `AgentResultPanel` header source — the durable terminal outcome for a
/// `run_id`, PLUS its validator-aware display state. Folds both the raw process outcome and
/// [`project_validated_outcome`] (the gate-aware state, SW-BA-8) over the SAME records — so the
/// live-completion chip OR-13 swaps in and the durable AU-55 row read the ONE composed truth and
/// cannot diverge. `None` when the run recorded no outcome yet (in-flight) → the panel degrades to
/// its live/chip-only header. Off the async runtime (AC-2); `run_id`-native.
#[tauri::command]
pub async fn read_run_outcome(run_id: String) -> Result<Option<RunOutcomeRead>, String> {
    crate::commands::await_blocking("read_run_outcome", move || {
        let (records, _health) = read_default_records();
        Ok(project_run_outcome_with_transcripts(
            &records,
            &run_id,
            &super::run_transcript::materialized_transcript_run_ids(),
            &crate::session_conversation::stored_session_ids(),
        ))
    })
    .await
}

/// Return the latest durable outcome for every run in one off-thread store fold.
#[tauri::command]
pub async fn list_run_outcomes() -> Result<Vec<RunOutcomeRead>, String> {
    crate::commands::await_blocking("list_run_outcomes", move || {
        let (records, _health) = read_default_records();
        Ok(project_run_outcomes_with_transcripts(
            &records,
            &super::run_transcript::materialized_transcript_run_ids(),
            &crate::session_conversation::stored_session_ids(),
        ))
    })
    .await
}

/// SW-OR-13 (AC-1): the widened `read_run_outcome` payload — the raw durable process outcome
/// paired with its validator-aware display state, so ONE read drives both the live-completion
/// chip swap and the durable AU-55 row from the same composed source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOutcomeRead {
    pub outcome: RunOutcomeEvent,
    pub validated: ValidatedRunOutcome,
    #[serde(default)]
    pub has_structured_transcript: bool,
    /// `linked` when the outcome names its session(s); `unlinked` for a run recorded before
    /// outcomes carried them.
    #[serde(default)]
    pub link: RunLink,
}

/// The CodeSense sessions a run names, in the order they first appeared; empty for a run recorded
/// before outcomes carried them.
pub(crate) fn session_ids_for_run(run_id: &str) -> Vec<String> {
    let (records, _health) = read_default_records();
    project_run_outcome(&records, run_id)
        .map(|(_, read)| read.outcome.session_ids)
        .unwrap_or_default()
}

/// A run has a structured transcript when one of its sessions has a stored conversation, or an
/// older run wrote its own `{run_id}.turns.json`.
fn enrich_transcript_presence(
    reads: &mut [RunOutcomeRead],
    materialized_run_ids: &HashSet<String>,
    stored_sessions: &HashSet<String>,
) {
    for read in reads {
        read.has_structured_transcript = materialized_run_ids.contains(&read.outcome.run_id)
            || read
                .outcome
                .session_ids
                .iter()
                .any(|session| stored_sessions.contains(session));
    }
}

fn project_run_outcome_with_transcripts(
    records: &[StoreRecord],
    run_id: &str,
    materialized_run_ids: &HashSet<String>,
    stored_sessions: &HashSet<String>,
) -> Option<RunOutcomeRead> {
    let mut read = project_run_outcome(records, run_id).map(|(_, read)| read)?;
    enrich_transcript_presence(
        std::slice::from_mut(&mut read),
        materialized_run_ids,
        stored_sessions,
    );
    Some(read)
}

fn project_run_outcomes_with_transcripts(
    records: &[StoreRecord],
    materialized_run_ids: &HashSet<String>,
    stored_sessions: &HashSet<String>,
) -> Vec<RunOutcomeRead> {
    let mut reads = project_run_outcomes(records);
    enrich_transcript_presence(&mut reads, materialized_run_ids, stored_sessions);
    reads
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunWorkProductRead {
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_request: Option<serde_json::Value>,
}

#[tauri::command]
pub async fn read_run_work_product(
    app: tauri::AppHandle,
    run_id: String,
) -> Result<Option<RunWorkProductRead>, String> {
    crate::commands::local_profile::active(&app)?;
    run_work_product(run_id).await
}

/// One run's work product — the branch the harness captured when it ended — or `None`
/// when it recorded none. Shared by the run-detail view, the `read_run_work_product`
/// MCP tool and a dashboard's `runWorkProduct({runId})`.
pub(crate) async fn run_work_product(run_id: String) -> Result<Option<RunWorkProductRead>, String> {
    crate::commands::await_blocking("read_run_work_product", move || {
        let (records, _) = read_default_records();
        Ok::<_, String>(work_product_from_records(&records, &run_id))
    })
    .await
}

/// The work product of `run_id`'s latest outcome in `records`, with a blank branch read
/// as none.
pub(crate) fn work_product_from_records(
    records: &[StoreRecord],
    run_id: &str,
) -> Option<RunWorkProductRead> {
    let provenance =
        project_run_outcome(records, run_id).and_then(|(_, read)| read.outcome.work_product)?;
    let branch = normalized_work_product_branch(&provenance.branch)?;
    Some(RunWorkProductRead {
        branch,
        change_request: None,
    })
}

fn normalized_work_product_branch(raw: &str) -> Option<String> {
    let branch = raw.trim();
    (!branch.is_empty()).then(|| branch.to_string())
}

/// Compose the latest raw and validator-aware outcome for one run from one immutable record slice.
/// The envelope key is returned for deterministic list ordering without leaking it onto the wire.
pub(crate) fn project_run_outcome(
    records: &[StoreRecord],
    run_id: &str,
) -> Option<((u128, u64), RunOutcomeRead)> {
    if run_id.trim().is_empty() {
        return None;
    }
    let mut rows: Vec<((u128, u64), &RunOutcomeEvent)> = records
        .iter()
        .filter_map(|record| match &record.event {
            RunEvent::Outcome(outcome) if outcome.run_id == run_id => {
                Some(((record.ts, record.seq), &**outcome))
            }
            _ => None,
        })
        .collect();
    rows.sort_by_key(|(key, _)| *key);
    let &(key, outcome) = rows.last()?;
    // A run several sessions worked has a row per session: it names all of them, in the order
    // they first appeared, and keeps the org and team of the latest row that has them.
    let mut folded = outcome.clone();
    folded.session_ids.clear();
    for (_, row) in &rows {
        for id in &row.session_ids {
            if !folded.session_ids.contains(id) {
                folded.session_ids.push(id.clone());
            }
        }
    }
    folded.org = rows.iter().rev().find_map(|(_, row)| row.org.clone());
    folded.team = rows.iter().rev().find_map(|(_, row)| row.team.clone());
    let outcome = &folded;
    let story_id = outcome.story_id.trim();
    let validated = project_validated_outcome(records, story_id, run_id)
        .unwrap_or(ValidatedRunOutcome::Completed);
    Some((
        key,
        RunOutcomeRead {
            link: RunLink::of(outcome),
            outcome: folded.clone(),
            validated,
            has_structured_transcript: false,
        },
    ))
}

fn project_run_outcomes(records: &[StoreRecord]) -> Vec<RunOutcomeRead> {
    let run_ids: BTreeSet<&str> = records
        .iter()
        .filter_map(|record| match &record.event {
            RunEvent::Outcome(outcome) if !outcome.run_id.trim().is_empty() => {
                Some(outcome.run_id.as_str())
            }
            _ => None,
        })
        .collect();
    let mut projected: Vec<_> = run_ids
        .into_iter()
        .filter_map(|run_id| project_run_outcome(records, run_id))
        .collect();
    projected.sort_by(|(key_a, a), (key_b, b)| {
        let ended_a = chrono::DateTime::parse_from_rfc3339(&a.outcome.ended_at).ok();
        let ended_b = chrono::DateTime::parse_from_rfc3339(&b.outcome.ended_at).ok();
        ended_b
            .cmp(&ended_a)
            .then_with(|| key_b.cmp(key_a))
            .then_with(|| a.outcome.run_id.cmp(&b.outcome.run_id))
    });
    projected.into_iter().map(|(_, read)| read).collect()
}

/// SW-BA-8: the display state of a completed run — the projection SW-AU-55 renders (mapping to
/// the FE `AGENT_STATUS_CONFIG` vocabulary). A run's process outcome is its display state, except
/// that a legacy record's failed validator verdict downgrades `Success` to `NeedsAttention`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValidatedRunOutcome {
    Completed,
    NeedsAttention,
    Failed,
}

/// SW-BA-8: the display state of ONE run's latest terminal outcome ([`latest_run_outcome`],
/// SW-AU-52). `None` when the run recorded no outcome yet (still in-flight, or crashed before any
/// write).
///
/// A legacy record (written while the status gate existed) may carry a `validator_step` and a
/// stored verdict for it. A failed check is never shown as success: a `Success` process whose
/// legacy verdict FAILED reads `NeedsAttention`; a passed or missing verdict reads `Completed`.
pub fn project_validated_outcome(
    records: &[StoreRecord],
    story_id: &str,
    run_id: &str,
) -> Option<ValidatedRunOutcome> {
    let outcome = latest_run_outcome(records, story_id, run_id)?;
    Some(match outcome.status {
        SessionStatus::Failed => ValidatedRunOutcome::Failed,
        SessionStatus::NeedsAttention => ValidatedRunOutcome::NeedsAttention,
        SessionStatus::Success => match outcome.validator_step.as_deref() {
            Some(step)
                if legacy_verdict_for_run(records, story_id, run_id, step, &outcome.ended_at)
                    == Some(StepResult::Failed) =>
            {
                ValidatedRunOutcome::NeedsAttention
            }
            _ => ValidatedRunOutcome::Completed,
        },
    })
}

/// The legacy verdict recorded for ONE run's `validator_step`. Prefers a run-scoped verdict;
/// otherwise an unscoped (`run_id = ""`, out-of-band) verdict recorded in
/// `[ended_at, next same-step run's ended_at)` — so neither an earlier attempt's verdict leaks
/// forward nor a later attempt's leaks backward. Latest-wins by `(ts, seq)`; `Pending` is ignored;
/// an unparseable timestamp is never attributed.
fn legacy_verdict_for_run(
    records: &[StoreRecord],
    story_id: &str,
    run_id: &str,
    step_id: &str,
    ended_at: &str,
) -> Option<StepResult> {
    let story_id = story_id.trim();
    let ended_at = chrono::DateTime::parse_from_rfc3339(ended_at).ok();
    let next_boundary = ended_at.and_then(|end| {
        records
            .iter()
            .filter_map(|rec| match &rec.event {
                RunEvent::Outcome(o)
                    if o.story_id.trim() == story_id
                        && o.run_id != run_id
                        && o.validator_step.as_deref() == Some(step_id) =>
                {
                    chrono::DateTime::parse_from_rfc3339(&o.ended_at).ok()
                }
                _ => None,
            })
            .filter(|other_end| *other_end >= end)
            .min()
    });
    let mut run_scoped: Option<(u128, u64, StepResult)> = None;
    let mut unscoped: Option<(u128, u64, StepResult)> = None;
    for rec in records {
        let RunEvent::Verdict(v) = &rec.event else {
            continue;
        };
        if v.story_id.trim() != story_id || v.step_id != step_id {
            continue;
        }
        if v.result == StepResult::Pending {
            continue;
        }
        if !run_id.is_empty() && v.run_id == run_id {
            if run_scoped.is_none_or(|(ts, seq, _)| (rec.ts, rec.seq) >= (ts, seq)) {
                run_scoped = Some((rec.ts, rec.seq, v.result));
            }
        } else if v.run_id.is_empty()
            && ended_at.is_some_and(|end| {
                chrono::DateTime::parse_from_rfc3339(&v.at)
                    .is_ok_and(|at| at >= end && next_boundary.is_none_or(|next| at < next))
            })
            && unscoped.is_none_or(|(ts, seq, _)| (rec.ts, rec.seq) >= (ts, seq))
        {
            unscoped = Some((rec.ts, rec.seq, v.result));
        }
    }
    run_scoped.or(unscoped).map(|(_, _, result)| result)
}

/// The identity that scopes a legacy verdict to a concrete gate attempt — the invariant
/// `(story_id, action_identity)` plus the run's `run_id`.
pub struct VerdictIdentity {
    pub story_id: String,
    pub action_identity: String,
    pub run_id: String,
}

// ---------------------------------------------------------------------------
// Read + fold (read side)
// ---------------------------------------------------------------------------

/// Read the default store at `{codesense_home}/run-store.jsonl`. A missing home
/// or file yields an empty read (`Never` upstream). Blocking — call inside
/// `await_blocking`.
pub fn read_default_records() -> (Vec<StoreRecord>, StoreHealth) {
    match store_path() {
        Some(path) => read_records(&path),
        None => (Vec::new(), StoreHealth::default()),
    }
}

/// Checked sibling for callers whose safety decision depends on distinguishing an empty store from
/// an unreadable one. Missing storage is still a valid empty store.
pub(crate) fn read_default_records_checked() -> Result<(Vec<StoreRecord>, StoreHealth), String> {
    match store_path() {
        Some(path) => read_records_checked(&path),
        None => Ok((Vec::new(), StoreHealth::default())),
    }
}

/// Read + parse every record from `path`, tolerating a degraded store (AC-9/AC-10):
/// a missing file reads as empty (`Never` upstream, not an error); an unknown
/// `schema_version` / `kind`, a malformed interior line, or a truncated final line
/// is skipped and counted. Never panics, never fails the caller.
///
/// Blocking — call inside `await_blocking`.
pub fn read_records(path: &Path) -> (Vec<StoreRecord>, StoreHealth) {
    match read_records_checked(path) {
        Ok(result) => result,
        Err(error) => {
            log::warn!(target: "run_store", "{error}");
            (Vec::new(), StoreHealth::default())
        }
    }
}

pub(crate) fn read_records_checked(path: &Path) -> Result<(Vec<StoreRecord>, StoreHealth), String> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), StoreHealth::default()));
        }
        Err(error) => {
            return Err(format!(
                "run_store: read failed for {}: {error}",
                path.display()
            ));
        }
    };

    Ok(parse_records(&body))
}

fn parse_records(body: &str) -> (Vec<StoreRecord>, StoreHealth) {
    let mut health = StoreHealth::default();
    let mut records = Vec::new();
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Some(rec) => records.push(rec),
            None => health.skipped_lines += 1,
        }
    }
    (records, health)
}

/// Parse one line, gating on `schema_version` before the full decode so a newer
/// record is skipped rather than mis-read (AC-9). Returns `None` for any
/// unparseable / future-version line (the caller counts it).
fn parse_line(line: &str) -> Option<StoreRecord> {
    // A permissive first pass reads just the version, so a future record that no
    // longer matches `StoreRecord` is still classified as "future", not "corrupt".
    let raw: serde_json::Value = serde_json::from_str(line).ok()?;
    let version = raw.get("schema_version").and_then(|v| v.as_u64())?;
    // Valid range is exactly `[1, SCHEMA_VERSION]`: a future version is skipped
    // (AC-9), and `0` / any sub-v1 value is a foreign or corrupt artifact — skip it
    // rather than decode it as a current record.
    if version == 0 || version > SCHEMA_VERSION as u64 {
        return None;
    }
    serde_json::from_value(raw).ok()
}

/// Fold verdict records to the latest per `step_id` (append order, last-write-wins)
/// — the latest-per-step rule [`run_step_verdicts`] folds with. A `BTreeMap` gives deterministic per-step ordering.
fn latest_verdicts_per_step<'a>(
    verdicts: impl IntoIterator<Item = &'a VerdictEvent>,
) -> std::collections::BTreeMap<&'a str, &'a VerdictEvent> {
    let mut by_step = std::collections::BTreeMap::new();
    for v in verdicts {
        by_step.insert(v.step_id.as_str(), v);
    }
    by_step
}

/// Project a folded latest-per-step map into the FE-facing [`StepVerdictView`] rows.
fn project_step_views(
    by_step: &std::collections::BTreeMap<&str, &VerdictEvent>,
) -> Vec<StepVerdictView> {
    by_step
        .values()
        .map(|v| StepVerdictView {
            step_id: v.step_id.clone(),
            result: v.result,
            at: v.at.clone(),
            by: v.by.clone(),
            via: v.via,
            evidence: v.evidence.clone(),
        })
        .collect()
}

/// Collect one run's verdicts for a story, ordered by the store's canonical `(ts, seq)` —
/// the pre-fold step for [`run_step_verdicts`], NOT raw file position. `append_to`
/// stamps `seq`/`ts` before it takes the store lock, so two concurrent appends can
/// land in the file in the opposite order to their `(ts, seq)`; without this sort a
/// consumer would pick a different "latest" per step than the gate view (and than
/// SW-GT-25 compaction, which keeps the `(ts, seq)`-max record per key). A stable sort
/// keeps file order for an exact tie, matching the gate fold's last-insert-wins.
fn sorted_run_verdicts<'a>(
    records: &'a [StoreRecord],
    story_id: &str,
    run_id: &str,
) -> Vec<&'a VerdictEvent> {
    let mut verdicts: Vec<(u128, u64, &VerdictEvent)> = records
        .iter()
        .filter_map(|rec| match &rec.event {
            RunEvent::Verdict(v) if v.story_id == story_id && v.run_id == run_id => {
                Some((rec.ts, rec.seq, v))
            }
            _ => None,
        })
        .collect();
    verdicts.sort_by_key(|(ts, seq, _)| (*ts, *seq));
    verdicts.into_iter().map(|(_, _, v)| v).collect()
}

/// SW-AU-49: fold ONE run's verdicts to the latest per `step_id`, reusing the SAME
/// [`latest_verdicts_per_step`] rule (NOT a second fold) narrowed to `run_id`, and
/// projecting through the shared [`project_step_views`]. Also surfaces the run's
/// `action_identity` from the newest matched verdict so the caller can resolve the
/// run's step-DAG skeleton. An unknown run (no matched verdicts) yields an empty
/// result → the caller degrades to the run-level pill.
pub fn run_step_verdicts(records: &[StoreRecord], story_id: &str, run_id: &str) -> RunStepVerdicts {
    // A blank `run_id` is not a real run — reject it rather than fold every unattributed
    // (`run_id: ""`) verdict into one bogus run. The FE never passes an empty id (a
    // `RunInstance` always carries one), so this only hardens a direct/MCP caller.
    if run_id.is_empty() {
        return RunStepVerdicts::default();
    }
    let verdicts = sorted_run_verdicts(records, story_id, run_id);
    // Every verdict in a run shares its action — take the newest matched one's.
    let action_identity = verdicts.last().map(|v| v.action_identity.clone());
    let by_step = latest_verdicts_per_step(verdicts.iter().copied());
    RunStepVerdicts {
        action_identity,
        steps: project_step_views(&by_step),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(story: &str, step: &str, result: StepResult, cfg: &str) -> VerdictEvent {
        VerdictEvent {
            story_id: story.into(),
            action_identity: "todo->in-progress".into(),
            run_id: "run-1".into(),
            step_id: step.into(),
            result,
            at: "2026-08-17T00:00:00Z".into(),
            by: Some("agent:dev".into()),
            via: Via::Evaluator,
            evidence: None,
            config_version: cfg.into(),
        }
    }

    /// Wrap an event in a minimal on-disk record envelope (for the pure fold tests
    /// that don't round-trip through the file).
    fn rec(event: RunEvent) -> StoreRecord {
        StoreRecord {
            schema_version: SCHEMA_VERSION,
            seq: 0,
            ts: 0,
            event,
        }
    }

    fn advance(story: &str, to: &str, overridden: bool) -> AdvanceEvent {
        AdvanceEvent {
            story_id: story.into(),
            from: Some("todo".into()),
            to: to.into(),
            trigger: if overridden {
                Trigger::Human
            } else {
                Trigger::AutoRun
            },
            overridden,
            reason: overridden.then(|| "shipping the hotfix".into()),
            steps: Vec::new(),
            action_identity: Some("todo->in-progress".into()),
            run_id: None,
            by: Some("human:sam".into()),
        }
    }

    #[test]
    fn record_verdict_round_trips_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        let ev = verdict("SW-A", "dev", StepResult::Passed, "c1");
        append_to(&path, RunEvent::Verdict(ev.clone()));
        let (records, health) = read_records(&path);
        assert_eq!(health.skipped_lines, 0);
        assert_eq!(records.len(), 1);
        match &records[0].event {
            RunEvent::Verdict(got) => assert_eq!(got, &ev),
            other => panic!("expected a verdict record, got {other:?}"),
        }
    }

    /// Wrap an event with an explicit `(ts, seq)` so latest-wins ordering is exercised.
    fn rec_at(ts: u128, seq: u64, event: RunEvent) -> StoreRecord {
        StoreRecord {
            schema_version: SCHEMA_VERSION,
            seq,
            ts,
            event,
        }
    }

    // ── SW-GT-24: retry-progress persistence + fold into retry cursor (AC-1) ─────────────────

    fn retry_progress(story: &str, attempt: u32, next_run_at: Option<&str>) -> RetryProgressEvent {
        RetryProgressEvent {
            story_id: story.into(),
            action_identity: "todo->in-progress".into(),
            run_id: "run-1".into(),
            config_version: "c1".into(),
            attempt,
            max: 3,
            next_run_at: next_run_at.map(str::to_string),
            routed_to: Some("needs-attention".into()),
            exhausted: false,
            dispatch_nonce: None,
        }
    }

    #[test]
    fn retry_progress_round_trips_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        let ev = retry_progress("SW-A", 2, Some("2026-08-18T00:30:00Z"));
        append_to(&path, RunEvent::RetryProgress(ev.clone()));
        let (records, health) = read_records(&path);
        assert_eq!(health.skipped_lines, 0);
        assert_eq!(records.len(), 1);
        match &records[0].event {
            RunEvent::RetryProgress(got) => assert_eq!(got, &ev),
            other => panic!("expected a retry-progress record, got {other:?}"),
        }
    }

    // ── SW-AU-52: durable run outcome ───────────────────────────────────────────────────────

    fn run_outcome(run_id: &str, status: SessionStatus, source: OutcomeSource) -> RunOutcomeEvent {
        RunOutcomeEvent {
            story_id: "SW-A".into(),
            run_id: run_id.into(),
            automation_id: None,
            trigger_source: None,
            workspace: None,
            scheduled_at: None,
            fired_at: None,
            work_product: None,
            status,
            summary: Some("done".into()),
            exit_code: Some(0),
            started_at: Some("2026-08-26T00:00:00Z".into()),
            ended_at: "2026-08-26T00:01:00Z".into(),
            artifact_ref: None,
            validator_step: None,
            structured_declared: None,
            org: None,
            team: None,
            session_ids: Vec::new(),
            source,
        }
    }

    // AC-1: an outcome round-trips through the append-only store unchanged.
    #[test]
    fn run_outcome_round_trips_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        let ev = run_outcome("run-1", SessionStatus::Success, OutcomeSource::ProcessExit);
        append_to(&path, RunEvent::Outcome(Box::new(ev.clone())));
        let (records, health) = read_records(&path);
        assert_eq!(health.skipped_lines, 0);
        assert_eq!(records.len(), 1);
        match &records[0].event {
            RunEvent::Outcome(got) => assert_eq!(**got, ev),
            other => panic!("expected an outcome record, got {other:?}"),
        }
    }

    #[test]
    fn checked_read_distinguishes_missing_store_from_io_failure() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.jsonl");
        assert_eq!(
            read_records_checked(&missing).unwrap(),
            (Vec::new(), StoreHealth::default())
        );

        let error = read_records_checked(dir.path()).expect_err("a directory is not a JSONL file");
        assert!(error.contains("read failed"), "got: {error}");
    }

    #[test]
    fn checked_read_preserves_degraded_health() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        std::fs::write(&path, "not-json\n").unwrap();
        let (records, health) = read_records_checked(&path).unwrap();
        assert!(records.is_empty());
        assert_eq!(health.skipped_lines, 1);
    }

    #[test]
    fn run_outcome_provenance_is_additive_and_legacy_compatible() {
        let mut event = run_outcome("run-1", SessionStatus::Success, OutcomeSource::ProcessExit);
        let without = serde_json::to_value(&event).unwrap();
        assert!(without.get("automation_id").is_none());
        assert!(without.get("trigger_source").is_none());
        assert!(without.get("work_product").is_none());

        event.automation_id = Some("review".into());
        event.trigger_source = Some("headless".into());
        event.work_product = Some(RunWorkProductProvenance {
            source_repo: crate::domain::SourceRepoSlug::new("acme", "app").unwrap(),
            branch: "feat/run-1".into(),
            credential_ref: Some(crate::domain::CredentialRef {
                provider: "github".into(),
                account: "work".into(),
            }),
        });
        let with = serde_json::to_value(&event).unwrap();
        assert_eq!(with["automation_id"], "review");
        assert_eq!(with["trigger_source"], "headless");
        assert_eq!(with["work_product"]["branch"], "feat/run-1");
        assert!(with["work_product"].get("token").is_none());

        let mut legacy = with;
        legacy.as_object_mut().unwrap().remove("automation_id");
        legacy.as_object_mut().unwrap().remove("trigger_source");
        legacy.as_object_mut().unwrap().remove("work_product");
        let decoded: RunOutcomeEvent = serde_json::from_value(legacy).unwrap();
        assert_eq!(decoded.automation_id, None);
        assert_eq!(decoded.trigger_source, None);
        assert_eq!(decoded.work_product, None);
    }

    #[test]
    fn project_run_outcome_keeps_each_runs_immutable_branch() {
        let provenance = |branch: &str| RunWorkProductProvenance {
            source_repo: crate::domain::SourceRepoSlug::new("acme", "app").unwrap(),
            branch: branch.into(),
            credential_ref: None,
        };
        let mut first = run_outcome("run-1", SessionStatus::Success, OutcomeSource::Signal);
        first.work_product = Some(provenance("feat/first"));
        let mut second = run_outcome("run-2", SessionStatus::Success, OutcomeSource::Signal);
        second.work_product = Some(provenance("feat/second"));
        let records = [
            rec_at(10, 0, RunEvent::Outcome(Box::new(first))),
            rec_at(20, 0, RunEvent::Outcome(Box::new(second))),
        ];

        let branch = |run_id| {
            project_run_outcome(&records, run_id)
                .and_then(|(_, read)| read.outcome.work_product)
                .map(|work_product| work_product.branch)
        };
        assert_eq!(branch("run-1").as_deref(), Some("feat/first"));
        assert_eq!(branch("run-2").as_deref(), Some("feat/second"));
    }

    #[test]
    fn project_run_outcomes_deduplicates_and_orders_deterministically() {
        let mut older = run_outcome("run-a", SessionStatus::Success, OutcomeSource::Signal);
        older.ended_at = "2026-08-26T00:00:00Z".into();
        let mut newer = run_outcome("run-a", SessionStatus::Failed, OutcomeSource::ProcessExit);
        newer.story_id = "SW-NEW".into();
        newer.ended_at = "2026-08-26T00:02:00Z".into();
        let mut malformed = run_outcome("run-b", SessionStatus::Success, OutcomeSource::Signal);
        malformed.ended_at = "not-a-time".into();
        let records = [
            rec_at(10, 0, RunEvent::Outcome(Box::new(older))),
            rec_at(20, 0, RunEvent::Outcome(Box::new(newer))),
            rec_at(30, 0, RunEvent::Outcome(Box::new(malformed))),
        ];

        let projected = project_run_outcomes(&records);
        assert_eq!(projected.len(), 2);
        assert_eq!(projected[0].outcome.run_id, "run-a");
        assert_eq!(projected[0].outcome.story_id, "SW-NEW");
        assert_eq!(projected[0].validated, ValidatedRunOutcome::Failed);
        assert_eq!(projected[1].outcome.run_id, "run-b");
    }

    #[test]
    fn project_run_outcomes_uses_record_key_then_run_id_for_time_ties() {
        let a = run_outcome("run-a", SessionStatus::Success, OutcomeSource::Signal);
        let b = run_outcome("run-b", SessionStatus::Success, OutcomeSource::Signal);
        let c = run_outcome("run-c", SessionStatus::Success, OutcomeSource::Signal);
        let records = [
            rec_at(10, 1, RunEvent::Outcome(Box::new(a))),
            rec_at(20, 1, RunEvent::Outcome(Box::new(b))),
            rec_at(20, 1, RunEvent::Outcome(Box::new(c))),
        ];
        let ids: Vec<_> = project_run_outcomes(&records)
            .into_iter()
            .map(|read| read.outcome.run_id)
            .collect();
        assert_eq!(ids, ["run-b", "run-c", "run-a"]);
        assert!(project_run_outcomes(&[]).is_empty());
    }

    #[test]
    fn project_run_outcomes_omits_blank_legacy_run_ids() {
        let blank = run_outcome("  ", SessionStatus::Success, OutcomeSource::Signal);
        let records = [rec(RunEvent::Outcome(Box::new(blank)))];
        assert!(project_run_outcome(&records, "  ").is_none());
        assert!(project_run_outcomes(&records).is_empty());
    }

    #[test]
    fn transcript_presence_enriches_single_and_batched_projections_identically() {
        let mut declared_missing = run_outcome(
            "run-declared-missing",
            SessionStatus::Success,
            OutcomeSource::Signal,
        );
        declared_missing.structured_declared = Some(true);
        let records = [
            rec_at(
                10,
                0,
                RunEvent::Outcome(Box::new(run_outcome(
                    "run-interactive",
                    SessionStatus::Success,
                    OutcomeSource::Signal,
                ))),
            ),
            rec_at(
                20,
                0,
                RunEvent::Outcome(Box::new(run_outcome(
                    "run-headless",
                    SessionStatus::Success,
                    OutcomeSource::ProcessExit,
                ))),
            ),
            rec_at(30, 0, RunEvent::Outcome(Box::new(declared_missing))),
            rec_at(
                40,
                0,
                RunEvent::Outcome(Box::new(run_outcome(
                    "run-plain",
                    SessionStatus::Success,
                    OutcomeSource::ProcessExit,
                ))),
            ),
        ];
        let dir = tempfile::tempdir().unwrap();
        let transcript = r#"{"turns":[{"role":"assistant","text":"done"}]}"#;
        std::fs::write(dir.path().join("run-interactive.turns.json"), transcript).unwrap();
        std::fs::write(dir.path().join("run-headless.turns.json"), transcript).unwrap();
        let materialized =
            super::super::run_transcript::materialized_transcript_run_ids_in(dir.path());

        let single =
            project_run_outcome_with_transcripts(&records, "run-interactive", &materialized, &HashSet::new())
                .unwrap();
        let batch = project_run_outcomes_with_transcripts(&records, &materialized, &HashSet::new());

        assert!(single.has_structured_transcript);
        let presence = |run_id| {
            batch
                .iter()
                .find(|read| read.outcome.run_id == run_id)
                .unwrap()
                .has_structured_transcript
        };
        assert!(presence("run-interactive"));
        assert!(presence("run-headless"));
        assert!(!presence("run-declared-missing"));
        assert!(!presence("run-plain"));
    }

    #[test]
    fn a_run_has_a_structured_transcript_when_one_of_its_sessions_has_a_stored_conversation() {
        let mut with_session = run_outcome("run-new", SessionStatus::Success, OutcomeSource::Signal);
        with_session.session_ids = vec!["tab-1".into(), "tab-2".into()];
        let mut other = run_outcome("run-other", SessionStatus::Success, OutcomeSource::Signal);
        other.session_ids = vec!["tab-3".into()];
        let records = vec![
            rec_at(10, 0, RunEvent::Outcome(Box::new(with_session))),
            rec_at(20, 0, RunEvent::Outcome(Box::new(other))),
            rec_at(
                30,
                0,
                RunEvent::Outcome(Box::new(run_outcome(
                    "run-unlinked",
                    SessionStatus::Success,
                    OutcomeSource::Signal,
                ))),
            ),
        ];
        let stored: HashSet<String> = ["tab-2".to_owned()].into();
        let batch = project_run_outcomes_with_transcripts(&records, &HashSet::new(), &stored);
        let presence = |run_id| {
            batch
                .iter()
                .find(|read| read.outcome.run_id == run_id)
                .unwrap()
                .has_structured_transcript
        };
        assert!(presence("run-new"));
        assert!(!presence("run-other"));
        assert!(!presence("run-unlinked"));
        let single = project_run_outcome_with_transcripts(&records, "run-new", &HashSet::new(), &stored)
            .unwrap();
        assert!(single.has_structured_transcript);
    }

    #[test]
    fn transcript_presence_defaults_false_for_legacy_read_envelopes() {
        let legacy = serde_json::json!({
            "outcome": run_outcome("run-legacy", SessionStatus::Success, OutcomeSource::Signal),
            "validated": "completed"
        });
        let read: RunOutcomeRead = serde_json::from_value(legacy).unwrap();
        assert!(!read.has_structured_transcript);
    }

    // AC-2: the durable outcome survives a restart — re-reading the JSONL from disk (a fresh
    // process' read, no in-memory state) then folding returns the persisted outcome.
    #[test]
    fn latest_run_outcome_survives_a_reread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        let ev = run_outcome("run-7", SessionStatus::Success, OutcomeSource::ProcessExit);
        append_to(&path, RunEvent::Outcome(Box::new(ev.clone())));
        let (records, _) = read_records(&path);
        assert_eq!(latest_run_outcome(&records, "SW-A", "run-7"), Some(&ev));
    }

    // AC-6 / fold: latest-wins by (ts, seq); `source` is preserved metadata, not a precedence
    // driver, so a later record wins regardless of its source.
    #[test]
    fn latest_run_outcome_returns_latest_by_ts_seq() {
        let mut a = rec(RunEvent::Outcome(Box::new(run_outcome(
            "run-1",
            SessionStatus::Success,
            OutcomeSource::Signal,
        ))));
        a.ts = 10;
        let mut b = rec(RunEvent::Outcome(Box::new(run_outcome(
            "run-1",
            SessionStatus::Failed,
            OutcomeSource::ProcessExit,
        ))));
        b.ts = 20;
        let records = [a, b];
        let got = latest_run_outcome(&records, "SW-A", "run-1").unwrap();
        assert_eq!(got.status, SessionStatus::Failed, "later record wins");
        assert_eq!(got.source, OutcomeSource::ProcessExit);
    }

    // The fold scopes to run_id — another run's outcome is never returned.
    #[test]
    fn latest_run_outcome_scopes_to_run_id() {
        let records = vec![rec(RunEvent::Outcome(Box::new(run_outcome(
            "run-1",
            SessionStatus::Success,
            OutcomeSource::Signal,
        ))))];
        assert_eq!(latest_run_outcome(&records, "SW-A", "run-2"), None);
        assert!(latest_run_outcome(&records, "SW-A", "run-1").is_some());
    }

    // ---- SW-AU-96: reported step results ----

    fn step_result(run_id: &str, node_id: &str) -> StepResultEvent {
        StepResultEvent {
            story_id: "SW-AU-96".into(),
            run_id: run_id.into(),
            node_id: node_id.into(),
            summary: None,
            outputs: Vec::new(),
            artifacts: Vec::new(),
            footprint: Vec::new(),
        }
    }

    // AC-5: every free-text / JSON channel is scrubbed before the event is appended, and an over-cap
    // output value collapses to the legible truncation marker — so the ledger never holds a secret.
    #[test]
    fn scrubbed_redacts_every_channel_and_bounds_outputs() {
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123";
        let mut event = step_result("run-1", "dev");
        event.summary = Some(format!("token={secret}"));
        event.outputs = vec![
            StepOutput {
                key: "leak".into(),
                value: serde_json::json!(format!("token={secret}")),
            },
            StepOutput {
                key: "big".into(),
                // Comfortably above the 64 KiB `PAYLOAD_MAX_BYTES` cap → truncation marker.
                value: serde_json::json!("x".repeat(70 * 1024)),
            },
        ];
        event.artifacts = vec![StepArtifact {
            kind: ArtifactKind::Pr,
            reference: format!("https://x/?token={secret}"),
            label: Some(format!("label {secret}")),
        }];
        event.footprint = vec![FootprintEntry {
            kind: ArtifactKind::File,
            reference: format!("path?token={secret}"),
            change: FootprintChange::Modified,
        }];

        let scrubbed = event.scrubbed();
        let rendered = serde_json::to_string(&scrubbed).unwrap();
        assert!(
            !rendered.contains(secret),
            "no secret survives in any channel"
        );
        assert!(
            scrubbed.outputs[1].value.get("truncated").is_some(),
            "over-cap output collapses to the truncation marker"
        );
    }

    // AC-4: reported results scope to `(story_id, run_id)` and fold-source order is append order
    // `(ts, seq)` — the assertion is load-bearing (the events carry distinct summaries, so a wrong
    // sort order would be observed).
    #[test]
    fn step_result_events_scope_and_order() {
        let mk = |seq: u64, ts: u128, run_id: &str, summary: &str| {
            let mut event = step_result(run_id, "dev");
            event.summary = Some(summary.into());
            StoreRecord {
                schema_version: SCHEMA_VERSION,
                seq,
                ts,
                event: RunEvent::StepResult(event),
            }
        };
        let records = vec![
            mk(2, 20, "run-1", "later"),
            mk(0, 30, "run-2", "other run"), // excluded
            mk(1, 10, "run-1", "earlier"),
        ];
        let events = step_result_events(&records, "SW-AU-96", "run-1");
        assert_eq!(events.len(), 2, "scoped to run-1, run-2 excluded");
        // Ordered by (ts, seq): the ts=10 record precedes the ts=20 one, so "later" sorts last and
        // would win a scalar latest-wins fold.
        assert_eq!(
            (events[0].summary.as_deref(), events[1].summary.as_deref()),
            (Some("earlier"), Some("later"))
        );
    }

    // AC-5: the entry-count bound and the string-channel size bound both hold — a runaway agent cannot
    // grow one event without limit, and an over-cap summary truncates to the legible marker.
    #[test]
    fn scrubbed_bounds_entry_counts_and_string_channels() {
        let mut event = step_result("run-1", "dev");
        event.summary = Some("s".repeat(70 * 1024));
        event.footprint = (0..MAX_REPORTED_ENTRIES + 50)
            .map(|i| FootprintEntry {
                kind: ArtifactKind::File,
                reference: format!("f{i}.rs"),
                change: FootprintChange::Modified,
            })
            .collect();
        let scrubbed = event.scrubbed();
        assert_eq!(
            scrubbed.footprint.len(),
            MAX_REPORTED_ENTRIES,
            "entry count is bounded"
        );
        assert!(
            scrubbed.summary.as_deref().unwrap().contains("[truncated]"),
            "over-cap summary truncates to the legible marker"
        );
    }

    // SW-AU-96: an empty report carries no information and is recognized so the caller can drop it.
    #[test]
    fn empty_report_is_recognized() {
        assert!(step_result("run-1", "dev").is_empty_content());
        let mut with_summary = step_result("run-1", "dev");
        with_summary.summary = Some("did a thing".into());
        assert!(!with_summary.is_empty_content());
        let mut whitespace = step_result("run-1", "dev");
        whitespace.summary = Some("   ".into());
        assert!(whitespace.is_empty_content(), "whitespace summary is empty");
    }

    // AC-1/AC-3: the MCP boundary parses artifact/footprint wire tokens strictly — a good token maps,
    // a bad one is rejected (typed error at the tool), never a silently-stored garbage variant.
    #[test]
    fn artifact_and_footprint_kinds_parse_from_wire() {
        assert_eq!(ArtifactKind::from_wire("file"), Some(ArtifactKind::File));
        assert_eq!(ArtifactKind::from_wire(" pr "), Some(ArtifactKind::Pr));
        assert_eq!(
            ArtifactKind::from_wire("entity"),
            Some(ArtifactKind::Entity)
        );
        assert_eq!(ArtifactKind::from_wire("branch"), None);
        assert_eq!(
            FootprintChange::from_wire("created"),
            Some(FootprintChange::Created)
        );
        assert_eq!(
            FootprintChange::from_wire("modified"),
            Some(FootprintChange::Modified)
        );
        assert_eq!(FootprintChange::from_wire("deleted"), None);
    }

    // AC-4: a `StepResult` event round-trips through the JSONL store on the current schema version —
    // the additive variant is written and read back with no migration.
    #[test]
    fn step_result_round_trips_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        let mut event = step_result("run-9", "dev");
        event.artifacts = vec![StepArtifact {
            kind: ArtifactKind::File,
            reference: "a.rs".into(),
            label: None,
        }];
        append_to(&path, RunEvent::StepResult(event.clone()));
        let (records, _) = read_records(&path);
        let got = step_result_events(&records, "SW-AU-96", "run-9");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].artifacts[0].reference, "a.rs");
    }

    // SW-OR-12 (AC-3): the artifact_ref threaded onto an outcome round-trips through the store and
    // folds back via the same run_id-native projection that the read command uses.
    #[test]
    fn artifact_ref_joins_onto_the_outcome_and_folds_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        let ev = RunOutcomeEvent {
            artifact_ref: Some("run-artifacts/run-9.log".into()),
            ..run_outcome("run-9", SessionStatus::Success, OutcomeSource::ProcessExit)
        };
        append_to(&path, RunEvent::Outcome(Box::new(ev.clone())));
        let (records, _) = read_records(&path);
        let (_, got) = project_run_outcome(&records, "run-9").unwrap();
        assert_eq!(
            got.outcome.artifact_ref.as_deref(),
            Some("run-artifacts/run-9.log")
        );
    }

    // SW-OR-15 (AC-9): the `structured_declared` attribution bit round-trips through the store and
    // folds back on the SAME read path the panel uses, so the FE can tell "no capability" from
    // "declared but no turns landed". Additive + defaulted: an old record (no field) reads back `None`.
    #[test]
    fn structured_declared_joins_onto_the_outcome_and_folds_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        let declared = RunOutcomeEvent {
            structured_declared: Some(true),
            org: None,
            team: None,
            session_ids: Vec::new(),
            ..run_outcome("run-int", SessionStatus::Success, OutcomeSource::Signal)
        };
        append_to(&path, RunEvent::Outcome(Box::new(declared)));
        let (records, _) = read_records(&path);
        let (_, got) = project_run_outcome(&records, "run-int").unwrap();
        assert_eq!(got.outcome.structured_declared, Some(true));
        // A run with no declared capability leaves the field absent (the benign path).
        assert_eq!(
            run_outcome("run-x", SessionStatus::Success, OutcomeSource::Signal).structured_declared,
            None
        );
    }

    // -- SW-BA-8: durable display projection ------------------------------

    // A run's terminal outcome, optionally gated on a validator step.
    fn gated_outcome(run_id: &str, status: SessionStatus, validator: Option<&str>) -> StoreRecord {
        let mut o = run_outcome(run_id, status, OutcomeSource::ProcessExit);
        o.validator_step = validator.map(str::to_string);
        // ended_at is fixed at 2026-08-26T00:01:00Z by `run_outcome` — the staleness boundary.
        rec(RunEvent::Outcome(Box::new(o)))
    }

    // A verdict record with an explicit run_id ("" = the unscoped, reaped out-of-band case), an
    // RFC-3339 `at`, and a controllable append position (ts) for latest-wins.
    fn verdict_rec(
        step: &str,
        result: StepResult,
        run_id: &str,
        at: &str,
        ts: u128,
    ) -> StoreRecord {
        let mut v = verdict("SW-A", step, result, "cfg-1");
        v.run_id = run_id.into();
        v.at = at.into();
        let mut r = rec(RunEvent::Verdict(v));
        r.ts = ts;
        r
    }

    // AC-3: no validator configured → Completed, and an unrelated verdict never interferes.
    #[test]
    fn no_validator_reads_completed() {
        let records = [
            gated_outcome("run-1", SessionStatus::Success, None),
            verdict_rec("build", StepResult::Failed, "", "2026-08-26T00:05:00Z", 20),
        ];
        assert_eq!(
            project_validated_outcome(&records, "SW-A", "run-1"),
            Some(ValidatedRunOutcome::Completed)
        );
    }

    // A legacy record gated on a validator step never shows a FAILED check as success: a stored
    // failed verdict (run-scoped, or unscoped and recorded after the run ended) reads
    // NeedsAttention; a passed or missing verdict reads Completed.
    #[test]
    fn legacy_validator_step_failed_verdict_reads_needs_attention() {
        let run_scoped_fail = [
            gated_outcome("run-1", SessionStatus::Success, Some("build")),
            verdict_rec(
                "build",
                StepResult::Failed,
                "run-1",
                "2026-08-26T00:05:00Z",
                20,
            ),
        ];
        assert_eq!(
            project_validated_outcome(&run_scoped_fail, "SW-A", "run-1"),
            Some(ValidatedRunOutcome::NeedsAttention)
        );
        let unscoped_fail = [
            gated_outcome("run-1", SessionStatus::Success, Some("build")),
            verdict_rec("build", StepResult::Failed, "", "2026-08-26T00:05:00Z", 20),
        ];
        assert_eq!(
            project_validated_outcome(&unscoped_fail, "SW-A", "run-1"),
            Some(ValidatedRunOutcome::NeedsAttention)
        );
        // An unscoped failure recorded BEFORE this run ended belongs to an earlier attempt.
        let stale_fail = [
            gated_outcome("run-1", SessionStatus::Success, Some("build")),
            verdict_rec("build", StepResult::Failed, "", "2026-08-26T00:00:30Z", 20),
        ];
        assert_eq!(
            project_validated_outcome(&stale_fail, "SW-A", "run-1"),
            Some(ValidatedRunOutcome::Completed)
        );
        // Latest verdict wins: a later pass supersedes an earlier fail.
        let fail_then_pass = [
            gated_outcome("run-1", SessionStatus::Success, Some("build")),
            verdict_rec(
                "build",
                StepResult::Failed,
                "run-1",
                "2026-08-26T00:05:00Z",
                20,
            ),
            verdict_rec(
                "build",
                StepResult::Passed,
                "run-1",
                "2026-08-26T00:06:00Z",
                30,
            ),
        ];
        assert_eq!(
            project_validated_outcome(&fail_then_pass, "SW-A", "run-1"),
            Some(ValidatedRunOutcome::Completed)
        );
        let unverdicted = [gated_outcome(
            "run-2",
            SessionStatus::Success,
            Some("build"),
        )];
        assert_eq!(
            project_validated_outcome(&unverdicted, "SW-A", "run-2"),
            Some(ValidatedRunOutcome::Completed)
        );
    }

    // A failed PROCESS outcome is Failed regardless of any validator/verdict (gate axis not consulted).
    #[test]
    fn process_failed_reads_failed_regardless_of_verdict() {
        let records = [
            gated_outcome("run-1", SessionStatus::Failed, Some("build")),
            verdict_rec("build", StepResult::Passed, "", "2026-08-26T00:05:00Z", 20),
        ];
        assert_eq!(
            project_validated_outcome(&records, "SW-A", "run-1"),
            Some(ValidatedRunOutcome::Failed)
        );
    }

    // A NeedsAttention process outcome passes straight through; a run with no outcome yet → None.
    #[test]
    fn needs_attention_passes_through_and_absent_outcome_is_none() {
        let attn = [gated_outcome("run-1", SessionStatus::NeedsAttention, None)];
        assert_eq!(
            project_validated_outcome(&attn, "SW-A", "run-1"),
            Some(ValidatedRunOutcome::NeedsAttention)
        );
        let only_verdict = [verdict_rec(
            "build",
            StepResult::Passed,
            "",
            "2026-08-26T00:05:00Z",
            20,
        )];
        assert_eq!(
            project_validated_outcome(&only_verdict, "SW-A", "run-1"),
            None
        );
    }

    // The reused SessionStatus + OutcomeSource serialize to the signal_done wire vocabulary, so a
    // store line matches what from_wire/json_status already emit.
    #[test]
    fn run_outcome_serializes_to_wire_vocabulary() {
        let line = serde_json::to_string(&run_outcome(
            "run-1",
            SessionStatus::NeedsAttention,
            OutcomeSource::Signal,
        ))
        .unwrap();
        assert!(line.contains("\"status\":\"needs-attention\""), "{line}");
        assert!(line.contains("\"source\":\"signal\""), "{line}");
    }

    #[test]
    fn append_writes_one_versioned_line_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        append_to(
            &path,
            RunEvent::Verdict(verdict("SW-A", "dev", StepResult::Passed, "c1")),
        );
        append_to(&path, RunEvent::Advance(advance("SW-A", "review", false)));

        let (records, health) = read_records(&path);
        assert_eq!(records.len(), 2);
        assert_eq!(health.skipped_lines, 0);
        assert_eq!(records[0].schema_version, SCHEMA_VERSION);
        assert!(records[1].seq > records[0].seq, "seq is monotonic");
    }

    #[test]
    fn append_creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("deep")
            .join("nested")
            .join("run-store.jsonl");
        append_to(
            &path,
            RunEvent::Verdict(verdict("SW-A", "dev", StepResult::Passed, "c1")),
        );
        let (records, _) = read_records(&path);
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn missing_file_reads_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.jsonl");
        let (records, health) = read_records(&path);
        assert!(records.is_empty());
        assert_eq!(health.skipped_lines, 0);
    }

    #[test]
    fn schema_version_zero_is_skipped_not_misread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(
                f,
                r#"{{"schema_version":0,"seq":0,"ts":1,"kind":"advance","story_id":"SW-A","to":"done","trigger":"human"}}"#
            )
            .unwrap();
        }
        let (records, health) = read_records(&path);
        assert!(records.is_empty(), "a sub-v1 record is not decoded");
        assert_eq!(health.skipped_lines, 1);
    }

    #[test]
    fn corrupt_interior_line_is_skipped_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        append_to(
            &path,
            RunEvent::Verdict(verdict("SW-A", "dev", StepResult::Passed, "c1")),
        );
        // Simulate a torn/garbage interior line and a good trailing line.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "{{not valid json").unwrap();
        }
        append_to(
            &path,
            RunEvent::Verdict(verdict("SW-A", "review", StepResult::Passed, "c1")),
        );
        let (records, health) = read_records(&path);
        assert_eq!(records.len(), 2, "good lines still fold");
        assert_eq!(health.skipped_lines, 1, "corrupt line counted");
    }

    #[test]
    fn future_schema_version_is_skipped_not_misread() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            // A record from a hypothetical future app version.
            writeln!(
                f,
                r#"{{"schema_version":999,"seq":0,"ts":1,"kind":"advance","story_id":"SW-A","to":"done","trigger":"human"}}"#
            )
            .unwrap();
        }
        let (records, health) = read_records(&path);
        assert!(records.is_empty(), "future-version record not decoded");
        assert_eq!(health.skipped_lines, 1);
    }

    #[test]
    fn unknown_event_kind_is_skipped_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(
                f,
                r#"{{"schema_version":1,"seq":0,"ts":1,"kind":"some_future_kind","x":1}}"#
            )
            .unwrap();
        }
        let (records, health) = read_records(&path);
        assert!(records.is_empty());
        assert_eq!(health.skipped_lines, 1);
    }

    #[test]
    fn on_disk_record_uses_snake_case_schema() {
        // The on-disk JSONL is the compaction contract (AC-11) — field names are
        // snake_case and stable, independent of the camelCase IPC view.
        let rec = StoreRecord {
            schema_version: SCHEMA_VERSION,
            seq: 0,
            ts: 1,
            event: RunEvent::Verdict(verdict("SW-A", "dev", StepResult::Passed, "c1")),
        };
        let json = serde_json::to_value(&rec).unwrap();
        assert!(json.get("schema_version").is_some());
        assert_eq!(json.get("kind").unwrap(), "verdict");
        assert_eq!(json.get("story_id").unwrap(), "SW-A");
        assert_eq!(json.get("config_version").unwrap(), "c1");
        assert_eq!(json.get("action_identity").unwrap(), "todo->in-progress");
    }

    #[test]
    fn run_step_verdicts_folds_latest_result_per_step() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        append_to(
            &path,
            RunEvent::Verdict(verdict("SW-A", "dev", StepResult::Failed, "c1")),
        );
        append_to(
            &path,
            RunEvent::Verdict(verdict("SW-A", "dev", StepResult::Passed, "c1")),
        );
        append_to(
            &path,
            RunEvent::Verdict(verdict("SW-A", "review", StepResult::Pending, "c1")),
        );
        let (records, _) = read_records(&path);
        let snap = run_step_verdicts(&records, "SW-A", "run-1").steps;
        assert_eq!(snap.len(), 2);
        let dev = snap.iter().find(|s| s.step_id == "dev").unwrap();
        assert_eq!(dev.result, StepResult::Passed, "latest wins");
        let review = snap.iter().find(|s| s.step_id == "review").unwrap();
        assert_eq!(review.result, StepResult::Pending);
    }

    #[test]
    fn run_step_verdicts_orders_by_ts_seq_not_file_position() {
        // Two verdicts for the same step, out of order in the file vs (ts, seq): the
        // (ts, seq)-latest must win, matching SW-GT-25 compaction. `append_to` stamps (ts, seq) before the store lock, so a
        // concurrent lock-race can land records in this order in real operation.
        let mut later = rec(RunEvent::Verdict(verdict(
            "SW-A",
            "dev",
            StepResult::Passed,
            "c1",
        )));
        later.ts = 100;
        later.seq = 5;
        let mut earlier = rec(RunEvent::Verdict(verdict(
            "SW-A",
            "dev",
            StepResult::Failed,
            "c1",
        )));
        earlier.ts = 50;
        earlier.seq = 6; // higher seq but lower ts → earlier in (ts, seq)

        // File order: [later(ts=100), earlier(ts=50)] — file-last is the ts-earlier one.
        let records = vec![later, earlier];
        let snap = run_step_verdicts(&records, "SW-A", "run-1").steps;
        let dev = snap.iter().find(|s| s.step_id == "dev").unwrap();
        assert_eq!(
            dev.result,
            StepResult::Passed,
            "the (ts, seq)-latest verdict wins, not the file-last one"
        );
    }

    #[test]
    fn advance_record_round_trips_override_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        let mut a = advance("SW-A", "done", true);
        a.steps = vec![StepSnapshot {
            step_id: "dev".into(),
            result: StepResult::Pending,
        }];
        append_to(&path, RunEvent::Advance(a));
        let (records, _) = read_records(&path);
        match &records[0].event {
            RunEvent::Advance(a) => {
                assert!(a.overridden);
                assert_eq!(a.reason.as_deref(), Some("shipping the hotfix"));
                assert_eq!(a.steps.len(), 1);
                assert_eq!(a.steps[0].result, StepResult::Pending);
            }
            other => panic!("expected Advance, got {other:?}"),
        }
    }

    // ── SW-AU-49: run-scoped per-step read (automation last-run strip) ───────────────────────

    #[test]
    fn run_step_verdicts_scopes_to_one_run_not_latest_across_runs() {
        // Story SW-A ran three times; each run recorded a verdict for step "review".
        // The run-scoped read must return run-2's OWN outcome, never run-3's — the
        // run-AGNOSTIC gate fold would return the latest across all runs (mixing them).
        let mk = |run: &str, result: StepResult| VerdictEvent {
            run_id: run.into(),
            result,
            ..verdict("SW-A", "review", StepResult::Pending, "c1")
        };
        let records = vec![
            rec_at(10, 0, RunEvent::Verdict(mk("run-1", StepResult::Passed))),
            rec_at(20, 0, RunEvent::Verdict(mk("run-2", StepResult::Failed))),
            rec_at(30, 0, RunEvent::Verdict(mk("run-3", StepResult::Passed))),
        ];
        let got = run_step_verdicts(&records, "SW-A", "run-2");
        assert_eq!(got.steps.len(), 1);
        assert_eq!(got.steps[0].step_id, "review");
        assert_eq!(
            got.steps[0].result,
            StepResult::Failed,
            "run-2's own verdict, not run-3's latest-across-runs"
        );
        assert_eq!(
            got.action_identity.as_deref(),
            Some("todo->in-progress"),
            "surfaces the run's action so the caller can resolve its DAG"
        );
    }

    #[test]
    fn run_step_verdicts_folds_latest_per_step_within_a_run() {
        // Within one run, step "dev" went Pending → Passed (latest wins), and "review"
        // folds alongside — the SAME latest-per-step rule the gate fold uses.
        let mk = |step: &str, result: StepResult, ts: u128| {
            rec_at(
                ts,
                0,
                RunEvent::Verdict(VerdictEvent {
                    run_id: "run-2".into(),
                    ..verdict("SW-A", step, result, "c1")
                }),
            )
        };
        let records = vec![
            mk("dev", StepResult::Pending, 10),
            mk("dev", StepResult::Passed, 20),
            mk("review", StepResult::Pending, 30),
        ];
        let got = run_step_verdicts(&records, "SW-A", "run-2");
        assert_eq!(got.steps.len(), 2);
        let dev = got.steps.iter().find(|s| s.step_id == "dev").unwrap();
        assert_eq!(dev.result, StepResult::Passed, "latest within the run wins");
    }

    #[test]
    fn run_step_verdicts_empty_run_id_is_rejected_not_a_wildcard() {
        // A blank run_id must NOT fold every verdict — including any with an empty run_id —
        // into one bogus run. It returns empty (→ the strip's pill), not a cross-run fold.
        let records = vec![
            rec(RunEvent::Verdict(VerdictEvent {
                run_id: String::new(),
                ..verdict("SW-A", "dev", StepResult::Passed, "c1")
            })),
            rec(RunEvent::Verdict(verdict(
                "SW-A",
                "review",
                StepResult::Passed,
                "c1",
            ))),
        ];
        let got = run_step_verdicts(&records, "SW-A", "");
        assert!(got.steps.is_empty());
        assert!(got.action_identity.is_none());
    }

    #[test]
    fn run_step_verdicts_unknown_run_is_empty_and_degradable() {
        // A run with no recorded verdicts (an older or non-workflow run) yields an empty
        // result + no action_identity → the strip degrades to the run-level pill.
        let records = vec![rec(RunEvent::Verdict(verdict(
            "SW-A",
            "dev",
            StepResult::Passed,
            "c1",
        )))];
        let got = run_step_verdicts(&records, "SW-A", "run-does-not-exist");
        assert!(got.steps.is_empty());
        assert!(got.action_identity.is_none());
    }

    // SV-CDE-8: a run's outcome names its org, team and session ids, and a row written before that
    // reads as unlinked, never rewritten.
    #[test]
    fn a_run_outcome_carries_org_team_and_session_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        let ev = RunOutcomeEvent {
            org: Some("acme".into()),
            team: Some("riverside".into()),
            session_ids: vec!["tab-1".into()],
            ..run_outcome("run-L", SessionStatus::Success, OutcomeSource::Signal)
        };
        append_to(&path, RunEvent::Outcome(Box::new(ev)));
        let (records, _) = read_records(&path);
        let (_, got) = project_run_outcome(&records, "run-L").unwrap();
        assert_eq!(got.outcome.org.as_deref(), Some("acme"));
        assert_eq!(got.outcome.team.as_deref(), Some("riverside"));
        assert_eq!(got.outcome.session_ids, ["tab-1"]);
        assert_eq!(got.link, RunLink::Linked);
    }

    #[test]
    fn an_old_run_row_reads_as_unlinked() {
        let old = serde_json::json!({
            "story_id": "SW-A", "run_id": "run-old", "status": "success", "ended_at": "2026-08-26T00:01:00Z",
            "source": "signal"
        });
        let outcome: RunOutcomeEvent = serde_json::from_value(old).unwrap();
        assert_eq!(outcome.org, None);
        assert!(outcome.session_ids.is_empty());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run-store.jsonl");
        append_to(&path, RunEvent::Outcome(Box::new(outcome)));
        let (records, _) = read_records(&path);
        let (_, got) = project_run_outcome(&records, "run-old").unwrap();
        assert_eq!(got.link, RunLink::Unlinked);
        let wire = serde_json::to_value(&got).unwrap();
        assert_eq!(wire["link"], "unlinked");
        assert!(wire["outcome"].get("session_ids").is_none(), "nothing is invented for an old row");
    }

    fn session_outcome(run_id: &str, sessions: &[&str], org: Option<&str>, team: Option<&str>) -> RunOutcomeEvent {
        RunOutcomeEvent {
            org: org.map(str::to_owned),
            team: team.map(str::to_owned),
            session_ids: sessions.iter().map(|s| s.to_string()).collect(),
            ..run_outcome(run_id, SessionStatus::Success, OutcomeSource::Signal)
        }
    }

    // ENG-46 AC1: a run worked by several sessions names all of them.
    #[test]
    fn a_runs_outcome_rows_fold_to_the_union_of_their_sessions() {
        let mut first = rec(RunEvent::Outcome(Box::new(session_outcome("run-M", &["tab-1"], Some("acme"), Some("sv")))));
        first.ts = 10;
        let mut second = rec(RunEvent::Outcome(Box::new(session_outcome("run-M", &["tab-2", "tab-1"], None, None))));
        second.ts = 20;
        let (_, got) = project_run_outcome(&[first, second], "run-M").unwrap();
        assert_eq!(got.outcome.session_ids, ["tab-1", "tab-2"], "first-seen order, no repeats");
        assert_eq!(got.outcome.org.as_deref(), Some("acme"), "the latest row that has an org");
        assert_eq!(got.outcome.team.as_deref(), Some("sv"));
        assert_eq!(got.link, RunLink::Linked);
    }
}
