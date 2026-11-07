// SPDX-License-Identifier: MIT
//! `weft::` — the sole canonical module that reads and writes the `wv_*` ledger
//! tables (SW-WV-B1).
//!
//! # The three load-bearing rules (ARCHITECTURE §0)
//!
//! 1. **One store, one writer, one path.** All `wv_*` ledger tables live in a
//!    single `weft.sqlite3` at ONE canonical per-team path
//!    ([`sqlite::weft_store_path`]), written ONLY through this module. No other
//!    module issues raw `INSERT`/`UPDATE`/`DELETE` against a `wv_*` table — a
//!    CI grep-test ([`sqlite`] `only_weft_sqlite_writes_wv_tables`) enforces it.
//! 2. **Ids never come from a cached counter.** The mutable/pruneable `wv_queue`
//!    family mints ids live-max inside an IMMEDIATE transaction
//!    ([`sqlite::wv_id_next`]); append-only log tables use `AUTOINCREMENT`. The
//!    two strategies are never mixed on one table (ARCHITECTURE §1.4).
//! 3. **Single-writer per record.** Every state-transition `UPDATE` carries a
//!    `WHERE id=?1 AND {owner_col}=?2` guard and checks `changes()==1`; a CAS
//!    loser gets [`store::MutateResult::NotOwner`], never a silent no-op
//!    ([`sqlite::wv_mutate_owned`]).
//!
//! Every ledger mutation writes an audit row into `wv_write_log` inside the
//! **same IMMEDIATE transaction** (ARCHITECTURE §1.6, AC-7): a crash between the
//! ledger write and the audit write leaves neither row.
//!
//! `#![allow(dead_code)]` — the same reader-side-API-ahead-of-its-writer pattern
//! as `principal_lifecycle`: B1 is the pure ledger *substrate*. Its store handle,
//! write handlers, path helper, and error type are the contract every B2+ story
//! writes through, but B1 itself wires no in-crate caller (the `#[cfg(test)]`
//! suite exercises the full surface, proving each AC). The module allow keeps the
//! substrate honest until B2 opens the first store, rather than sprinkling
//! per-item allows that would have to be removed one by one. The paired
//! `unused_imports` allow covers the `pub use` re-exports below — the sanctioned
//! public API surface B2+ imports from `weft::` — which likewise has no in-crate
//! consumer yet.

#![allow(dead_code, unused_imports)]

// SW-WA-A6 — the bounded auto-dispatch concurrency cap: the pure spare-slot / admittance
// decision the controller consults between the graduation gate and the spawn (the
// runaway-dispatch rail). The fleet live-count read it reads lives in `sqlite.rs`
// (`wv_count_live_workers`); this module holds only the const + the pure decision.
pub mod concurrency_cap;
pub mod dispatch;
pub mod done_record;
pub mod handoff;
// SW-WV-F7 — the FE dispatch bridge: the `wv_dispatch_worker` Tauri command (a
// thin door over the C1 dispatch core; MODULE-HOME §5.3 keeps it under `weft::`,
// never `commands::weft::`).
pub mod dispatch_bridge;
pub mod events;
// SW-WV-E5 — the four always-on guarantees: the canonical, machine-checkable layer
// for consent-on-env-change, soft-timeouts (never fabricate), verify-don't-fold, and
// governance (own-item-only writes / boards read-only to agents). Pure classifiers +
// guards over E4's tier scaffolding, E1's gate, and C3's verify — no new
// store/verb/command.
pub mod governance;
// The inbox agent sessions and the human message each other through.
pub mod inbox;
// SW-BSI-UX: agent → UI progress notifications persisted for the Updates lane.
pub mod agent_updates;
// SW-BSI-UX: the human's online/away status (closed-set enum + optional note).
pub mod human_status;
pub mod observe;
// SW-BSI-2: role-keyed delivery on top of the inbox. Role mailboxes are
// addressed as `(user, org, team, role)`; a role registered `exclusive` caps at one
// live instance; shared roles admit multiple instances with atomic per-delivery
// claims. See `_bmad-output/.../25-builder-skills-integration/GROOMING-PLAN.md`.
pub mod role_inbox;
// A read of the role mailboxes (registrations and deliveries); claims nothing.
pub mod role_state;
// SW-WV-E7 — postures: the canonical, machine-checkable "widen initiation only"
// layer. A posture (night-mode / away) widens which subclasses may self-initiate by
// subclass × time-of-day; it never mints authority (never elevates the E4 tier,
// never bypasses E5's gates or E6's graduation). Away = deny-by-default + the E3
// tier-2 windows paused. Pure classifier + guard over the existing seams — no new
// store/verb/command.
pub mod postures;
pub mod preflight;
pub mod queue;
pub mod reactions;
pub mod render_tripwire;
pub mod roster_query;
pub mod spawn;
pub mod sqlite;
pub mod store;
pub mod tier_picker;
pub mod tripwire;
pub mod watchdog;
pub mod worker_lifecycle;

// SW-WV-F1 — the Weft FE push-channel event constant (emitted by B3's write
// handler, subscribed by F1's board hook).
pub use events::WEFT_ATTENTION_CHANGED_EVENT;

pub use dispatch::{
    dispatch_done_by_session, dispatch_state, wv_mark_dispatch_done, wv_write_dispatch,
    DispatchError, DispatchRecord,
};
// SW-WV-C3 — the 3-leg done-state + verify-don't-fold. `DoneRecord`/`VerifyDoneResult`
// live exclusively in `weft::` (MODULE-HOME, AC-8); the handler imports them here.
pub use done_record::{DoneRecord, VerifyDoneResult};
// SW-WV-C4 — the auto-drafted handoff packet (draft ≠ authority). The two MCP verbs
// (`wv_report_handoff` write, `wv_handoff_state` read) + the `handoff-record`
// `signal_done` hook import these from `weft::`.
pub use handoff::{
    handoff_advance_by_session, handoff_state, wv_report_handoff, DraftedHandoff, HandoffError,
};
// SW-WV-C2 — the dispatch tripwire: the pure keep-vs-delegate classifier + its
// input/verdict types. The `wv_claim_queue` claim path and the `wv_report_cycle`
// MCP verb import these from `weft::`.
pub use queue::{CloseCondition, CloseOutcome, CycleReport, QueueError, QueueRow, TripwireClaim};
pub use sqlite::wv_refusal;
pub use sqlite::{
    resolve_weft_store_path, weft_store_path, wv_append, wv_auto_append, wv_id_next,
    wv_mutate_owned, AuditQueryParams, AuditRow, AUDIT_QUERY_DEFAULT_LIMIT, AUDIT_QUERY_MAX_LIMIT,
    WV_INTERACTIVE_VERBS,
};
pub use store::{MutateResult, WeftStore, WeftStoreError};
pub use tripwire::{
    cycle_crosses_threshold, evaluate_tripwire, Lifecycle, TouchedPath, TripwireInput,
    TripwireOutcome, TripwireVerdict, ADDED_LINES_THRESHOLD, LOCAL_TIER_SCOPES,
    VERIFY_FIX_CYCLE_THRESHOLD,
};

// SW-WV-E2 — the endorsement tier-picker: the pure two-of-three classifier +
// its risk-signal enums. `wv_write_approval` (below) and the `wv_classify_action`
// MCP verb import these from `weft::`.
pub use tier_picker::{pick_tier, BlastRadius, Confidence};

// SW-WV-E5 — the four always-on guarantees: the canonical consent / soft-timeout /
// verify-don't-fold / governance classifiers + guards.
pub use governance::{
    admit_env_change, admit_own_item_write, classify_env_change, is_board_read_verb, may_fold,
    resolve_soft_timeout, AwaitOutcome, ConsentDecision, EnvChange, WriteDecision,
    BOARD_READ_VERBS, ENV_MUTATING_ACTIONS,
};

// SW-WV-E7 — postures: the widen-initiation-only classifier. `T2Directive` names the
// E3 pause/resume the away transition calls (single-sited, ARCHITECTURE §4.1).
pub use postures::{
    Posture, PostureConfig, PostureWindow, T2Directive, TimeOfDayWindow, MINUTES_PER_DAY,
};

// SW-WV-B3 — the five sibling ledgers' named write handlers + their types. This is
// the sanctioned public API B3's MCP verbs and Tauri commands import from `weft::`.
pub use sqlite::{
    wv_deprovision_roster, wv_list_roster, wv_retire_decision, wv_settle_approval,
    wv_upsert_attention, wv_upsert_roster, wv_upsert_watermark, wv_write_approval,
    wv_write_decision, ApprovalNotifier, ApprovalTier, NullNotifier, WvRosterRow, WvValidation,
    WvWriteError, WvWriteResult, WV_APPROVAL_DISPOSITIONS, WV_ATTENTION_NEEDS, WV_ATTENTION_STATES,
    WV_WATERMARK_SOURCES,
};

// SW-WV-E3 — the Tier-2 approval-window handlers: open (arms the surfaced-item
// guard), check (pure verdict), auto-close-if-lapsed, and the single-sited
// pause/resume-all pair E7's posture calls (ARCHITECTURE §4.1).
pub use sqlite::{
    wv_check_t2_window, wv_close_t2_window_if_lapsed, wv_open_t2_window, wv_pause_all_t2_windows,
    wv_resume_all_t2_windows, T2WindowVerdict,
};
// SW-WV-F5 — the pending-approvals read the YOU-OWE endorsement surface merges (T2/T3
// rows), with the `not_surfaced_but_expired` carry-over flag computed from E3's window.
pub use sqlite::{wv_list_pending_approvals, WvApproval};

// SW-WA-D4 — the controller-scoped posture SOURCE the dispatch-seam wrap reads and the
// Away/Normal transition writes. `wv_set_posture` fires the single-sited E3 pause/resume
// on a value change; `wv_read_posture` reconstructs the stored `PostureConfig`.
pub use sqlite::{wv_read_posture, wv_set_posture, PostureSetOutcome};

// SW-WV-A2 — the typed-ephemeral-worker spawn surface + its roster write handlers.
pub use spawn::{
    wv_spawn_worker, WeftError, WeftSpawnResult, WorkerSpawnRequest, WorkerSubclass,
    WV_SPAWN_WORKER_VERB,
};
pub use sqlite::{
    wv_delete_provision_row, wv_dispatch_worker_row, wv_find_live_worker, wv_provision_worker_row,
    WvLiveWorker, WV_WORKER_LIVE_STATES,
};

// SW-WA-A6 — the bounded auto-dispatch concurrency cap: the fleet live-count read
// (`wv_count_live_workers`, in `sqlite.rs`) + the pure spare-slot / admittance decision.
pub use concurrency_cap::{dispatch_admittance, spare_slots, CapDecision, DEFAULT_CONCURRENCY_CAP};
pub use sqlite::wv_count_live_workers;
// SW-WV-B5 — the scope layer's named write handlers + their types. The sanctioned
// public API B5's MCP verbs and Tauri commands import from `weft::`.
pub use sqlite::{
    wv_scope_close, wv_scope_create, wv_scope_get, wv_scope_update_artifact_ptr, wv_watch_close,
    wv_watch_list, wv_watch_mark_quiet, wv_watch_upsert, CloseReason, ScopeRow, WatchLifecycle,
    WatchRow, WV_WATCH_CLOSE_REASONS, WV_WATCH_LIFECYCLES, WV_WATCH_QUIET_THRESHOLD_DAYS,
};
// SW-WV-C6 — the decision-harvest-norm write handlers + the overdue-harvest read.
pub use sqlite::{
    wv_approve_harvest, wv_list_overdue_harvests, wv_mark_harvest_not_required,
    wv_set_harvest_artifact, HarvestRefusal, OverdueHarvest, WV_HARVEST_STATUSES,
};
// SW-WV-E1 — the endorsement gate: the assertion ledger's named write handlers +
// their types. The sanctioned public API E1's MCP verb + Tauri commands import.
pub use sqlite::{
    wv_get_assertion, wv_get_assertion_disposition, wv_insert_assertion, wv_insert_recipient_ack,
    wv_read_pending_assertions, wv_set_assertion_disposition, wv_settle_attention, WvAssertionRow,
    WvPendingAssertion, WV_ACK_VALUES, WV_ASSERTION_DISPOSITIONS,
};
// SW-WA-C0 — the endorsement→queue link reverse-lookup (`wv_approvals.source`) that
// pairs an assertion with its endorsement.
pub use sqlite::wv_find_pending_approval_by_source;

// SW-WV-A3 — the worker lifecycle state machine + the ordered signal_done hook
// registry (A3 sole owner, D6/D13), and the store transition handlers.
pub use sqlite::{
    wv_worker_deprovision, wv_worker_handoff, wv_worker_signal, wv_worker_signal_by_session,
    DeprovisionInputs, DeprovisionOutcome, WV_ROSTER_OFFERED_LIFECYCLE,
};
pub use worker_lifecycle::{
    check_transition, ClosureCondition, HookFuture, LifecycleError, SignalDoneHook,
    SignalDoneHooks, WorkerLifecycleState, LEGAL_TRANSITIONS,
};
// SW-WV-F1 — the Weft Board's single read: the five-section attention board. F1
// reads what B3/E1/A4/C3/C6 wrote (read-only); it never writes a `wv_*` row.
pub use sqlite::{
    wv_list_attention_board, AttentionBoard, AttentionBoardRow, AttentionKind, AttentionNeed,
    Initiator,
};

// SW-WV-B4 — signals + coord (durable, append-only, one owner per contract, peers
// read-only). The `wv_signals` insert rides the `signal_done` hook registry (A3
// owner); `wv_coord_write`/`wv_coord_read` are internal Tauri commands (MCP
// exposure deferred to SW-WV-C1/D4). C3 reads `wv_signals` via `has_success_signal`.
pub use sqlite::{
    wv_coord_read, wv_coord_write, wv_signal_insert, wv_signal_insert_by_session, CoordRefusal,
};
// SW-WV-D4 — the bounded worker-to-worker peer read lane: a worker may read a named
// peer's `wv_coord` state + worktree files ONLY when its dispatch lists that peer, and
// escalates any cross-worker decision to the controller (never acts on it directly).
// These back the `wv_coord_read` / `wv_peer_read_file` / `wv_escalate` MCP verbs.
pub use sqlite::{
    wv_dispatch_peers_for_worker, wv_escalate_insert, wv_queue_scope_for_worker,
    wv_roster_cwd_for_worker,
};
// SW-WV-F2 — the dispatch preflight: the one read the Board's dispatch form runs
// before it enables "Dispatch & release" (lease / queue-row / capacity → one verdict).
pub use preflight::{preflight_wv_dispatch, WvPreflightResult};

// SW-WV-A4 — the observe-only read primitives (Tier A): the non-polling `wv_wait`
// resolver + the OSC-133 broadcast relay. The MCP verbs in `handler.rs` import these; `wait_for_done` accepts abstract channels so it is
// unit-testable without a Tauri app context.
pub use observe::{wait_for_done, WaitResolution, WaitResult, WeftOsc133Relay};
// SW-WV-A4 — the silence watchdog + its per-team registry (the `watchdog-cancel`
// hook cancels via this on `signal_done`). `AppHandleEffects` is the production
// effect surface; `arm_silence_watchdog` is armed at dispatch time (A3 path).
pub use watchdog::{
    arm_silence_watchdog, AppHandleEffects, WatchdogEffects, WeftWatchdogRegistry,
    WATCHDOG_SUMMARY, WORKER_SILENCE_TTL_S,
};
// SW-WV-A4 — the silence-flag roster writer (single-writer store handler) + its wire
// status constant. The watchdog drives `dispatch → needs-attention` through this.
pub use sqlite::{wv_worker_needs_attention_by_session, WV_ROSTER_NEEDS_ATTENTION_STATUS};

// SW-WV-F3 — the fleet-view roster read projection (keyed by live `session_id`). Reads
// only; the `roster_query` Tauri commands import these from `weft::`.
pub use roster_query::{wv_roster_for_session, wv_worker_force_close};
pub use sqlite::{wv_roster_view_for_session, WvRosterView};
