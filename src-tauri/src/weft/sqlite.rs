// SPDX-License-Identifier: MIT
//! The `weft.sqlite3` schema, path helper, and the sanctioned write path
//! (SW-WV-B1).
//!
//! This module owns the sanctioned `wv_*` write primitives ([`wv_append`],
//! [`wv_auto_append`], [`wv_mutate_owned`]) — every mutation goes through one of
//! them and co-commits its audit row into `wv_write_log` inside the **same**
//! IMMEDIATE transaction (AC-7). The ONE-WRITER invariant is **dir-scoped over the
//! whole `src/weft/` module tree**, NOT this file alone: since SW-WV-B2 the raw
//! `wv_*` SQL also lives in sibling handlers here (e.g. `queue.rs`'s
//! `wv_queue`/`wv_key_ledger` transitions). The `only_weft_sqlite_writes_wv_tables`
//! CI test enforces exactly that — it excludes every file under `src/weft/` and
//! catches a raw `wv_*` write anywhere else in the crate.
//!
//! The store follows the orchestration store's proven open + migration idiom:
//! [`crate::shared::sqlite::open_wal_db`] → `execute_batch(SCHEMA)` →
//! [`crate::shared::sqlite::ensure_column`] for any column added after the
//! initial schema (ARCHITECTURE §1.5, mirrors `SqliteOrchestrationStore::open`).
//!
//! # Secret redaction
//!
//! [`crate::shared::secret_scan::redact_secrets`] is the single string-redaction
//! owner at persist. B1's helpers are generic (the caller supplies the SQL and
//! params), so they cannot know which bind param is user-facing prose; a caller
//! that persists a prose column (`note`, `summary`, `current_task`, …) redacts
//! that value *before* handing it to the helper. When B2+ add prose columns with
//! named write handlers, those handlers own the `redact_secrets` call.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use super::store::{MutateResult, WeftStore, WeftStoreError, WeftStoreResult};

static STORES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<PathBuf, WeftStore>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(Default::default()));

/// Give a migration fixture the first-open cache scope of a fresh process.
/// Only its own absolute path is evicted; other tests keep their cached stores.
#[cfg(test)]
pub(crate) fn reset_cached_store_for_test(path: &Path) {
    assert!(path.is_absolute());
    STORES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(path);
}

/// The `weft.sqlite3` schema.
///
/// B1 ships **stub** tables: each carries its final PRIMARY KEY from day one
/// (PK-CORRECT-STUB, ARCHITECTURE §1.5 — `ensure_column` cannot change a PK), plus
/// only the columns a reader needs at creation. B2–B5 add every remaining column
/// via `ensure_column` on top of these stubs, so a pre-existing `weft.sqlite3`
/// forward-migrates losslessly on open.
///
/// Id strategy is fixed per table and never mixed (ARCHITECTURE §1.4):
///   - `wv_queue` — live-max (`id INTEGER PRIMARY KEY`, no AUTOINCREMENT); ids
///     minted by [`wv_id_next`] inside each [`wv_append`].
///   - `wv_decisions` / `wv_approvals` / `wv_signals` / `wv_coord` /
///     `wv_attention` / `wv_write_log` / `wv_assertions` / `wv_recipient_acks` —
///     `AUTOINCREMENT` (ids never reused after delete).
///
/// `wv_attention.summary` is created here, NOT via a later `ensure_column`: it is
/// a DTO-read column, and a read-before-migrate window is forbidden
/// (ARCHITECTURE §1.5). `wv_write_log`'s core columns are likewise created here —
/// every mutation inserts into it immediately. `wv_signals.session_id` and
/// `wv_signals.status` are likewise created here (NOT via a later `ensure_column`):
/// C3's `WeftStore::has_success_signal` reads both at first use
/// (`WHERE session_id=? AND status='success'`), so a read-before-migrate window
/// would be an SW-WV-C3 bug. `wv_coord`'s `contract UNIQUE` and its `NOT NULL`
/// columns are minted only by this fresh CREATE (a later `ensure_column` cannot
/// add a UNIQUE constraint nor a NOT NULL without a default) — the Rust handlers
/// are the enforcement belt on a forward-healed table (mirrors the `wv_queue`
/// CHECK/UNIQUE reasoning above).
///
/// The audit column is `tbl`, not `table`: `table` is a reserved SQLite keyword
/// and using it as a column name would force quoting at every use site.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS wv_queue (
    id         INTEGER PRIMARY KEY,
    source     TEXT NOT NULL DEFAULT '',
    state      TEXT NOT NULL DEFAULT 'pending'
               CHECK(state IN ('pending','claimed','dispatched','resolved')),
    claimed_by TEXT,
    scope      TEXT,
    created    INTEGER NOT NULL DEFAULT 0,
    updated    INTEGER NOT NULL DEFAULT 0,
    refs       TEXT,
    note       TEXT,
    dedupe_key TEXT UNIQUE,
    -- SW-WV-C2: the dispatch-tripwire verdict, stamped at claim time (`keep` |
    -- `delegate`; `pending` until claimed) + the mid-flight verify-and-fix cycle
    -- count Row 5 tracks. The CHECK can only be minted by this fresh CREATE; on a
    -- forward-healed (ensure_column) table the Rust handlers are the belt.
    tripwire_verdict TEXT NOT NULL DEFAULT 'pending'
               CHECK(tripwire_verdict IN ('keep','delegate','pending')),
    verify_fix_cycles INTEGER NOT NULL DEFAULT 0,
    -- Which tripwire row (1..5) fired, NULL for a keep / not-yet-claimed row. The
    -- auditable "why" preserved into the key-ledger meta blob on resolve (AC-5).
    fired_row  INTEGER,
    -- SW-WV-D4 (AC-1): the peer-lane authorisation list (nullable JSON array of
    -- worker names); NULL = no peers = lane closed for that task.
    dispatch_peers TEXT,
    -- SW-WV-C5 (AC-8): release-time runtime state. `dispatched_at` (epoch seconds)
    -- and `dispatch_handle` (the opaque worker handle) are NULL until
    -- `wv_mark_dispatched` fires. The dispatch brief lives in `wv_dispatch_records`
    -- (C1, D4) — NOT here.
    dispatched_at  INTEGER,
    dispatch_handle TEXT
);

CREATE TABLE IF NOT EXISTS wv_key_ledger (
    queue_id    INTEGER NOT NULL,
    ref         TEXT,
    resolved_at INTEGER NOT NULL,
    -- SW-WV-C2 (AC-5): the tripwire decision preserved after the queue row is
    -- pruned — a JSON blob { tripwireVerdict, verifyFixCycles, firedRow } so a
    -- reviewer can reconstruct keep-vs-delegate after the fact. NULL for rows
    -- resolved before this column existed (no backfill needed).
    meta        TEXT
);

CREATE TABLE IF NOT EXISTS wv_roster (
    name TEXT PRIMARY KEY
);

CREATE TABLE IF NOT EXISTS wv_watermarks (
    source TEXT NOT NULL,
    scope  TEXT NOT NULL,
    PRIMARY KEY (source, scope)
);

CREATE TABLE IF NOT EXISTS wv_scopes (
    scope         TEXT PRIMARY KEY,
    parent_scope  TEXT,
    artifact_ptr  TEXT,
    lease_session TEXT,
    owner_session TEXT,
    created_at    INTEGER NOT NULL DEFAULT 0,
    updated_at    INTEGER NOT NULL DEFAULT 0,
    closed_at     INTEGER
);

CREATE TABLE IF NOT EXISTS wv_watches (
    scope TEXT NOT NULL,
    kind  TEXT NOT NULL,
    ref   TEXT NOT NULL,
    PRIMARY KEY (scope, kind, ref)
);

CREATE TABLE IF NOT EXISTS wv_decisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT
);

CREATE TABLE IF NOT EXISTS wv_approvals (
    id INTEGER PRIMARY KEY AUTOINCREMENT
);

CREATE TABLE IF NOT EXISTS wv_signals (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    worker      TEXT    NOT NULL,
    task_id     TEXT    NOT NULL,
    session_id  TEXT    NOT NULL,
    status      TEXT    NOT NULL,
    dispatch    TEXT,
    packet_refs TEXT,
    created     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS wv_coord (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    contract       TEXT    NOT NULL UNIQUE,
    owner_dispatch TEXT    NOT NULL,
    owner_worker   TEXT    NOT NULL,
    body           TEXT    NOT NULL,
    created        INTEGER NOT NULL,
    updated        INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS wv_attention (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    summary TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS wv_write_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    tbl        TEXT NOT NULL,
    operation  TEXT NOT NULL,
    row_id     INTEGER NOT NULL,
    written_at INTEGER NOT NULL,
    caller_session TEXT
);

CREATE TABLE IF NOT EXISTS wv_assertions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT    NOT NULL,
    run_id      TEXT,
    claim       TEXT    NOT NULL,
    channel     TEXT    NOT NULL,
    posted_at   TEXT    NOT NULL,
    disposition TEXT    NOT NULL DEFAULT 'pending',
    approval_id INTEGER
);

CREATE TABLE IF NOT EXISTS wv_recipient_acks (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    assertion_id INTEGER NOT NULL,
    recipient    TEXT    NOT NULL,
    ack          TEXT    NOT NULL,
    recorded_at  TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS wv_dispatch_records (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    story_id    TEXT    NOT NULL,
    task        TEXT    NOT NULL,
    context     TEXT    NOT NULL DEFAULT '',
    constraints TEXT    NOT NULL DEFAULT '',
    acceptance  TEXT    NOT NULL,
    state       TEXT    NOT NULL DEFAULT 'open'
                CHECK(state IN ('open','done')),
    scope       TEXT,
    session_id  TEXT,
    created_at  TEXT    NOT NULL,
    done_at     TEXT
);

CREATE TABLE IF NOT EXISTS wv_render_config (
    scope      TEXT    NOT NULL,
    kind       TEXT    NOT NULL,
    threshold  TEXT    NOT NULL,
    target     TEXT    NOT NULL,
    priority   INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (scope, kind, threshold)
);

CREATE TABLE IF NOT EXISTS wv_done_record (
    dispatch_id         TEXT PRIMARY KEY,
    queue_id            INTEGER NOT NULL,
    session_id          TEXT NOT NULL,
    run_id              TEXT NOT NULL,
    story_id            TEXT NOT NULL,
    subclass            TEXT NOT NULL,
    leg1_source         TEXT NOT NULL,
    leg1_verified       INTEGER NOT NULL DEFAULT 0,
    leg2_footprint      TEXT NOT NULL DEFAULT '[]',
    leg2_verified       INTEGER NOT NULL DEFAULT 0,
    leg3_packet         TEXT NOT NULL DEFAULT '{}',
    leg3_idempotent     INTEGER NOT NULL DEFAULT 0,
    leg3_verified       INTEGER NOT NULL DEFAULT 0,
    controller_verified INTEGER NOT NULL DEFAULT 0,
    done_when_met       INTEGER NOT NULL DEFAULT 0,
    created             INTEGER NOT NULL,
    verified_at         INTEGER
);

CREATE TABLE IF NOT EXISTS wv_handoff_records (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    story_id   TEXT    NOT NULL,
    session_id TEXT    NOT NULL,
    to_role    TEXT    NOT NULL DEFAULT '',
    finding    TEXT    NOT NULL,
    refs       TEXT    NOT NULL DEFAULT '[]',
    is_draft   INTEGER NOT NULL DEFAULT 1
               CHECK(is_draft = 1),
    created_at TEXT    NOT NULL
);

CREATE TABLE IF NOT EXISTS wv_posture (
    id         INTEGER PRIMARY KEY CHECK(id = 1),
    posture    TEXT    NOT NULL,
    windows    TEXT    NOT NULL DEFAULT '[]',
    updated_at INTEGER NOT NULL
);

-- Messages between agent sessions and the human. `sender` / `recipient` are session
-- handles or 'human' (`from` / `to` are SQL keywords). Times are epoch milliseconds;
-- `read_at` stays NULL until the recipient reads the row. Written only by `weft::inbox`.
CREATE TABLE IF NOT EXISTS wv_inbox (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    sender          TEXT    NOT NULL,
    recipient       TEXT    NOT NULL,
    text            TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    read_at         INTEGER,
    in_reply_to     INTEGER,
    in_reply_to_org TEXT,
    in_reply_to_team TEXT,
    primary_file    TEXT,
    on_click        TEXT,
    content_type    TEXT,
    summary         TEXT,
    kind            TEXT
);
CREATE INDEX IF NOT EXISTS wv_inbox_by_recipient ON wv_inbox (recipient, read_at);

-- Through which message each recipient's agent session was told about its mail, so each
-- batch of new mail is announced once, across app restarts too: a notice is reserved when
-- it is offered and marks `through_id` only once its delivery is confirmed. Written only
-- by `weft::inbox`.
CREATE TABLE IF NOT EXISTS wv_inbox_notified (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    recipient   TEXT    NOT NULL UNIQUE,
    -- The session it was told in: a new session under the handle starts unannounced.
    session     TEXT    NOT NULL,
    through_id  INTEGER NOT NULL,
    -- When the newest batch was announced, epoch ms (0: nothing confirmed yet).
    announced_at INTEGER NOT NULL,
    -- A notice offered and not yet confirmed: through which message it tells, its token,
    -- when it was offered (epoch ms) and the app run that offered it.
    reserved_through INTEGER,
    reserve_token    TEXT,
    reserved_at      INTEGER,
    reserved_by      TEXT,
    reserved_chat    TEXT
);

-- SW-BSI-2: role-keyed delivery layer.
-- A role mailbox is addressed as (user, org, team, role). Role is an agent id.
-- A role is shared (many live instances; one atomic claim selects one owner per
-- delivery) unless registered exclusive (one live holder). See _bmad-output/.../25-builder-skills-integration/GROOMING-PLAN.md.
--
-- One row per instance registration. An exclusive role has at most one live row;
-- a shared role has N. `session_id` names the session the app renews it for (NULL:
-- the caller renews); `ttl_secs` is the lifetime each renewal grants.
CREATE TABLE IF NOT EXISTS wv_role_registration (
    role            TEXT    NOT NULL,
    instance_id     TEXT    NOT NULL,
    generation      INTEGER NOT NULL,
    registered_at   INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL,
    last_renewed_at INTEGER NOT NULL,
    session_id      TEXT,
    exclusive       INTEGER NOT NULL DEFAULT 0,
    ttl_secs        INTEGER NOT NULL DEFAULT 60,
    UNIQUE(role, instance_id)
);
CREATE INDEX IF NOT EXISTS wv_role_registration_by_role ON wv_role_registration (role, expires_at);

-- One row per message addressed to a role. message_id references wv_inbox(id).
-- UNIQUE(message_id, role) so sending the same message to one role is one delivery
-- (dedupe), while addressing two different roles produces two deliveries.
CREATE TABLE IF NOT EXISTS wv_role_delivery (
    delivery_id            INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id             INTEGER NOT NULL,
    role                   TEXT    NOT NULL,
    state                  TEXT    NOT NULL DEFAULT 'pending'
                                   CHECK(state IN ('pending','claimed','accepted','acknowledged')),
    claim_token            TEXT,
    claim_owner_instance   TEXT,
    claim_owner_generation INTEGER,
    claim_expires_at       INTEGER,
    created_at             INTEGER NOT NULL,
    UNIQUE(message_id, role)
);
CREATE INDEX IF NOT EXISTS wv_role_delivery_by_role_state
    ON wv_role_delivery (role, state, created_at);

-- Permanent per-delivery acceptance receipt. Survives queue pruning.
-- UNIQUE(delivery_id) means a delivery has at most one accepted disposition.
CREATE TABLE IF NOT EXISTS wv_role_acceptance_receipt (
    receipt_id     TEXT    PRIMARY KEY,
    delivery_id    INTEGER NOT NULL,
    role           TEXT    NOT NULL,
    message_id     INTEGER NOT NULL,
    instance_id    TEXT    NOT NULL,
    generation     INTEGER NOT NULL,
    kind           TEXT    NOT NULL CHECK(kind IN ('work','no_work')),
    note_or_reason TEXT,
    queue_item_id  INTEGER,
    created_at     INTEGER NOT NULL,
    UNIQUE(delivery_id)
);
CREATE INDEX IF NOT EXISTS wv_role_acceptance_receipt_by_role
    ON wv_role_acceptance_receipt (role, created_at);

-- Request-id idempotency for send_message's role form. A caller supplies a
-- stable request_id + a payload hash; the same request_id + same hash replays
-- the stored (message_id, delivery_id); same request_id + different hash
-- conflicts and the stored outcome stays authoritative.
CREATE TABLE IF NOT EXISTS wv_send_message_request (
    request_id   TEXT    PRIMARY KEY,
    payload_hash TEXT    NOT NULL,
    message_id   INTEGER NOT NULL,
    delivery_id  INTEGER,
    created_at   INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS wv_mcp_request (
    request_id TEXT PRIMARY KEY,
    payload_hash TEXT NOT NULL,
    result TEXT
);

-- SW-BSI-UX: agent → UI progress notifications, surfaced in the Inbox pane's
-- "Updates" lane. Non-actionable: no reply, no attachment, no archive. Written
-- as a side effect of post_agent_message (fire-and-forget chat emits) so the
-- lane survives app restart. One row per agent emit, keyed by `(session_id,
-- created_at)`; dedupe is implicit via auto-increment id.
CREATE TABLE IF NOT EXISTS wv_agent_updates (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT    NOT NULL,
    agent_name TEXT    NOT NULL DEFAULT '',
    role       TEXT    NOT NULL DEFAULT 'agent',
    text       TEXT    NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS wv_agent_updates_by_created ON wv_agent_updates (created_at);

-- SW-BSI-UX: the human's online/away status for this team's inbox. Agents can
-- read it via MCP to adjust their behavior (don't ping at 3am; batch; escalate
-- differently). Singleton row — the team is implicit in the store the row lives
-- in. Default (no row) resolves to `online` with no note.
CREATE TABLE IF NOT EXISTS wv_human_status (
    id         INTEGER PRIMARY KEY CHECK(id = 1),
    status     TEXT    NOT NULL CHECK(status IN ('online','away','busy','offline')),
    note       TEXT,
    updated_at INTEGER NOT NULL
);

-- SV-INP-15: emoji reactions on inbox messages. Not mail: no read state, no wake. One
-- row per (message, reactor, emoji); `notified_at` is when the message's sender was
-- told through read_messages (NULL: not yet). Written only by `weft::reactions`.
CREATE TABLE IF NOT EXISTS wv_reactions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id  INTEGER NOT NULL,
    reactor     TEXT    NOT NULL,
    emoji       TEXT    NOT NULL,
    created_at  INTEGER NOT NULL,
    notified_at INTEGER,
    UNIQUE (message_id, reactor, emoji)
);
"#;

/// The weft-specific interactive-verb sub-registry (AC-8).
///
/// Any story adding a interactive `wv_*` interactive verb appends the verb
/// string to BOTH this constant AND
/// [`crate::commands::interactive_gate::INTERACTIVE_VERBS`], and gates it with
/// `InteractiveGate::admit` in `mcp/handler.rs`. The
/// `every_wv_interactive_verb_has_both_seams` test cross-checks the two
/// registries so a verb can never be added to one without the other.
///
/// SW-WV-E1 adds `wv_post_assertion` — the endorsement-gate write verb every
/// agent outward post must go through (ARCHITECTURE §3.1 TWO-SEAM-GATE).
/// SW-WV-E2 adds `wv_classify_action` — the read-only tier-classifier the
/// an agent calls before dispatching an action (ARCHITECTURE §3.1 names it in
/// the TWO-SEAM-GATE list).
///
/// Registration does not keep these verbs from workers: `InteractiveGate::admit`
/// checks only the verb name, not the caller, and a worker's `codesense` bridge
/// exposes every tool, these included (`mcp_allow` names servers, not tools).
pub const WV_INTERACTIVE_VERBS: &[&str] = &[
    "wv_spawn_worker",
    "wv_post_assertion",
    "wv_classify_action",
    // SW-WV-C1 — the interactive 4-field dispatch write. The read verb
    // `wv_dispatch_state` is Tier-A (no gate) and deliberately absent here.
    "wv_write_dispatch",
    // SW-WV-C3 — the interactive verify-don't-fold verb. Re-checks the 3-leg
    // done record before an agent may fold the queue row (a worker never
    // verifies its own done claim).
    "wv_verify_done",
    // SW-WV-C5 — the interactive release + closure verbs. `wv_mark_dispatched`
    // stamps the `claimed → dispatched` transition (start-and-release); `wv_close_task`
    // runs the two-condition close predicate (D5) and, only when both hold, delegates
    // the atomic prune to B2's `wv_resolve_queue` (closure is the caller's judgment,
    // never self-declared).
    "wv_mark_dispatched",
    "wv_close_task",
    // SW-WV-E3 — the Tier-2 approval-window verbs. `wv_open_t2_window` arms the
    // surfaced-item guard; `wv_check_t2_window` reads the pure verdict (no write, but
    // interactive like `wv_classify_action`); `wv_close_t2_window` auto-ratifies a
    // lapsed window in its surfacing session; `wv_pause_t2_windows` /
    // `wv_resume_t2_windows` are the single-sited presence pair E7's posture calls
    // (ARCHITECTURE §4.1 — nothing else writes `paused_at`). All interactive.
    "wv_open_t2_window",
    "wv_check_t2_window",
    "wv_close_t2_window",
    "wv_pause_t2_windows",
    "wv_resume_t2_windows",
    // SW-WA-D4 — the scope-level posture SOURCE verbs. `wv_read_posture` is the
    // self-initiation wrap's posture input; `wv_set_posture` writes the row and, on a
    // value change, fires the single-sited pause/resume above. Both interactive.
    "wv_read_posture",
    "wv_set_posture",
];

/// The per-team store's file name.
pub const WEFT_STORE_FILE: &str = "weft.sqlite3";

/// SQLite's WAL-mode sidecars, which travel with the main file.
const WEFT_STORE_SIDECAR_SUFFIXES: [&str; 2] = ["-wal", "-shm"];

/// The canonical per-team path to `weft.sqlite3`.
///
/// Resolves `{install_root}/{org_slug}/{team}/weft.sqlite3`, where
/// `install_root` is [`crate::context_repo::path_to_install_root`] — the same
/// machine-local, per-user root the lockfiles live under, a sibling of `orgs/`.
/// The store is runtime state (inboxes, role queues, agent updates, ledger
/// rows), so it lives OUTSIDE the context-repo clone where it can never be
/// committed or synced. The store always belongs to a team, so there is no
/// "no team" variant (the lockfiles' `_none` segment is reserved and rejected
/// here as a team name so the two layouts can't collide).
///
/// Pure path math — no filesystem touch. Callers resolve through
/// [`resolve_weft_store_path`], which also adopts a pre-existing in-clone store;
/// the global orchestration `default_store_path` is NOT this path and must not
/// be mirrored.
pub fn weft_store_path(
    codesense_home: &Path,
    principal: &crate::domain::Principal,
    org_slug: &str,
    team: &str,
) -> Result<PathBuf, String> {
    // Every segment must be a plain name, so the joins below cannot leave the
    // install root (`..`, separators, empty segments).
    let segment_error = crate::content_engine::layers::validate_path_segment;
    if let Some(reason) = org_slug.split('/').find_map(segment_error) {
        return Err(format!("org {org_slug:?}: {reason}"));
    }
    if let Some(reason) = segment_error(team) {
        return Err(format!("team {team:?}: {reason}"));
    }
    if team.eq_ignore_ascii_case(crate::content_engine::lockfile::NO_TEAM_SEGMENT) {
        return Err(format!("team {team:?}: reserved name"));
    }
    Ok(
        crate::context_repo::path_to_install_root(codesense_home, principal)
            .join(org_slug)
            .join(team)
            .join(WEFT_STORE_FILE),
    )
}

/// Where the store used to live: inside the context-repo clone, at
/// `{org_root}/{team}/install/weft.sqlite3`. Only read to adopt an old store.
pub(crate) fn legacy_in_clone_weft_store_path(org_root: &Path, team: &str) -> PathBuf {
    crate::context_repo::path_to_team_install_dir(org_root, team).join(WEFT_STORE_FILE)
}

/// Resolve a team's store path, adopting the old in-clone store on first use.
///
/// Every caller that opens the per-team store resolves its path here. When the
/// store does not exist yet at [`weft_store_path`] but an old one sits in the
/// context-repo clone, it is moved over (with its `-wal`/`-shm` sidecars) so the
/// team's inbox history carries across. Once the store exists at the new path
/// this is a single `exists` check.
pub fn resolve_weft_store_path(
    codesense_home: &Path,
    principal: &crate::domain::Principal,
    org_slug: &str,
    team: &str,
) -> Result<PathBuf, String> {
    let path = weft_store_path(codesense_home, principal, org_slug, team)?;
    if path.exists() {
        return Ok(path);
    }
    let org_root = crate::context_repo::path_to_org_root(codesense_home, principal, org_slug)?;
    let legacy = legacy_in_clone_weft_store_path(&org_root, team);
    adopt_legacy_weft_store(&legacy, &path).map_err(|e| {
        format!(
            "move team store {} -> {}: {e}",
            legacy.display(),
            path.display()
        )
    })?;
    Ok(path)
}

/// Move the store at `legacy` (and its sidecars) to `dest`, unless `dest`
/// already exists or there is nothing to move. Returns whether it moved.
///
/// The sidecars move first and the main file last, so a store at `dest` always
/// means the move finished; an interrupted move is completed by the next call
/// (a sidecar already moved is kept, the remaining files follow).
pub(crate) fn adopt_legacy_weft_store(legacy: &Path, dest: &Path) -> std::io::Result<bool> {
    static ADOPT: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ADOPT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if dest.exists() || !legacy.is_file() {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    for suffix in WEFT_STORE_SIDECAR_SUFFIXES {
        let from = with_suffix(legacy, suffix);
        if from.is_file() {
            move_file(&from, &with_suffix(dest, suffix))?;
        }
    }
    move_file(legacy, dest)?;
    log::info!(
        target: "weft",
        "moved team store out of the context repo: {} -> {}",
        legacy.display(),
        dest.display()
    );
    Ok(true)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Rename, falling back to copy + remove when the two paths sit on different
/// volumes (a user-cloned context repo need not share a disk with the home).
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)?;
    std::fs::remove_file(from)
}

/// `{col}` is a `role:x` spelling to re-key onto the bare `x` (not blank, not `human`).
/// GLOB is case-sensitive, as the prefix strip is.
fn prefixed_role(col: &str) -> String {
    format!(
        "{col} GLOB 'role:*' AND trim(substr({col}, 6)) <> '' \
         AND lower(trim(substr({col}, 6))) <> 'human'"
    )
}

/// Re-key `role:x` role-queue rows onto the bare `x` (see [`WeftStore::open`]). Runs
/// the UPDATEs only when a row would change, so an already-migrated store is read,
/// never written. A `role:x` registration whose instance already holds `x` cannot be
/// re-keyed (`UNIQUE(role, instance_id)`); the bare-name row stands and the
/// prefixed twin is dropped rather than stranded. A `role:x` delivery whose message
/// already has an `x` delivery is left as it is (nothing is deleted) and no longer
/// counts as needing the re-key.
fn rekey_prefixed_role_rows(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    let inbox = format!(
        "{} AND id IN (SELECT message_id FROM wv_role_delivery WHERE role = wv_inbox.recipient)",
        prefixed_role("recipient")
    );
    let role = prefixed_role("role");
    // A `role:x` delivery whose message already has a bare `x` delivery cannot be
    // re-keyed (`UNIQUE(message_id, role)`; the UPDATE OR IGNORE leaves it): it must
    // not keep the check true, or every open would take the write lock.
    let delivery = format!(
        "{role} AND NOT EXISTS (SELECT 1 FROM wv_role_delivery AS bare \
             WHERE bare.message_id = wv_role_delivery.message_id \
               AND bare.role = trim(substr(wv_role_delivery.role, 6)))"
    );
    let pending: bool = conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM wv_inbox WHERE {inbox}) \
                 OR EXISTS(SELECT 1 FROM wv_role_delivery WHERE {delivery}) \
                 OR EXISTS(SELECT 1 FROM wv_role_acceptance_receipt WHERE {role}) \
                 OR EXISTS(SELECT 1 FROM wv_role_registration WHERE {role})"
        ),
        [],
        |row| row.get(0),
    )?;
    if !pending {
        return Ok(());
    }
    conn.execute_batch(&format!(
        "BEGIN IMMEDIATE; \
         UPDATE wv_inbox SET recipient = trim(substr(recipient, 6)) WHERE {inbox}; \
         UPDATE OR IGNORE wv_role_delivery SET role = trim(substr(role, 6)) WHERE {role}; \
         UPDATE wv_role_acceptance_receipt SET role = trim(substr(role, 6)) WHERE {role}; \
         UPDATE OR IGNORE wv_role_registration SET role = trim(substr(role, 6)) WHERE {role}; \
         DELETE FROM wv_role_registration WHERE {role} AND EXISTS ( \
             SELECT 1 FROM wv_role_registration AS bare \
              WHERE bare.role = trim(substr(wv_role_registration.role, 6)) \
                AND bare.instance_id = wv_role_registration.instance_id); \
         COMMIT;"
    ))
}

impl WeftStore {
    /// Open (creating + forward-migrating on first use) the store at `path`.
    ///
    /// Idempotent (AC-1/AC-5): callable repeatedly on the same file without data
    /// loss or schema error. Follows `SqliteOrchestrationStore::open` exactly —
    /// `open_wal_db` → `execute_batch(SCHEMA)` → `ensure_column` for post-schema
    /// columns.
    ///
    /// SW-WV-B2: a pre-existing `weft.sqlite3` written by the B1 stub carries only
    /// `wv_queue(id)`. `execute_batch(SCHEMA)` is a no-op for that already-created
    /// table (all `CREATE ... IF NOT EXISTS`), so every column B2 needs is healed
    /// onto it here via `ensure_column` (forward-only, lossless — ARCHITECTURE
    /// §1.5). On a fresh DB these are all no-ops (the `CREATE TABLE` above already
    /// carries them, with the `CHECK`/`UNIQUE` constraints a later `ensure_column`
    /// cannot add). The `wv_key_ledger` table is created by `execute_batch(SCHEMA)`;
    /// the state/scope index is created AFTER the migration (below) so it never
    /// references a not-yet-healed column on a B1-stub table.
    /// SW-WV-A4: the store's on-disk path. The silence watchdog re-opens the store
    /// per off-thread write from this path (a `WeftStore` is cheap to re-open;
    /// carrying the path avoids threading the handle through the spawn task).
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn open(path: impl Into<PathBuf>) -> WeftStoreResult<Self> {
        use crate::shared::sqlite::ensure_column;
        let path = path.into();
        let path = if path.is_absolute() || path == std::path::Path::new(":memory:") {
            path
        } else {
            std::env::current_dir()?.join(path)
        };
        let mut stores = STORES.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(store) = stores.get(&path) {
            return Ok(store.clone());
        }
        let mut conn = crate::shared::sqlite::open_wal_db(&path)?;
        conn.execute_batch(SCHEMA)?;
        ensure_column(&conn, "wv_write_log", "caller_session", "TEXT")?;
        ensure_column(&conn, "wv_inbox", "in_reply_to_org", "TEXT")?;
        ensure_column(&conn, "wv_inbox", "in_reply_to_team", "TEXT")?;
        // SW-BSI-2: an inbox message may carry a managed-file reference (Markdown/HTML
        // the human previews). The path is a managed logical path; identity is the
        // conversation's, never the file's. Nullable — standalone messages have none.
        ensure_column(&conn, "wv_inbox", "primary_file", "TEXT")?;
        ensure_column(&conn, "wv_inbox", "attachments", "TEXT")?;
        // SW-BSI-UX: archived_at is the human's organizational signal — distinct from
        // read-state and from delivery/work-acceptance. Null = not archived.
        ensure_column(&conn, "wv_inbox", "archived_at", "INTEGER")?;
        // The sender withdrew the message before any agent picked it up (epoch ms).
        ensure_column(&conn, "wv_inbox", "withdrawn_at", "INTEGER")?;
        // Per-message click action chosen by the sender (JSON blob of
        // `MessageOnClick`). Nullable — a legacy row with no column reads NULL and
        // the Inbox row falls through to the ordinary "open the thread" click.
        ensure_column(&conn, "wv_inbox", "on_click", "TEXT")?;
        // How the renderer should format the body: `plain`, `markdown` (default
        // when NULL), or `html` (strictly sanitized FE-side). Nullable — a legacy
        // row reads NULL and the FE applies the renderer default.
        ensure_column(&conn, "wv_inbox", "content_type", "TEXT")?;
        // How the text reached the recipient: NULL through the inbox, `terminal` when
        // it was typed into the recipient's terminal (`send_to_session`) and is kept
        // only so the conversation shows it.
        ensure_column(&conn, "wv_inbox", "via", "TEXT")?;
        // Where replies to this message go (email Reply-To): a session handle, `human`
        // or `role:<id>`. Nullable — NULL means replies go to the sender.
        ensure_column(&conn, "wv_inbox", "reply_to", "TEXT")?;
        // The sender's one-line plain-text preview of the body (list views show it
        // instead of raw markup). Nullable — NULL means the list derives a preview.
        ensure_column(&conn, "wv_inbox", "summary", "TEXT")?;
        // SW-AGR-18: what the message asks of the human (`needs-you` / `update`), as the
        // sender set it. Nullable — NULL means the sender set none and reads resolve it.
        ensure_column(&conn, "wv_inbox", "kind", "TEXT")?;
        // When an instance first claimed a role delivery (epoch seconds, like the
        // table's other times). Kept when a lapsed claim returns it to pending.
        ensure_column(&conn, "wv_role_delivery", "first_claimed_at", "INTEGER")?;
        // A notice offered to the session's agent and not yet confirmed (see the table):
        // confirming it advances `through_id`; one never confirmed is offered again.
        ensure_column(&conn, "wv_inbox_notified", "reserved_through", "INTEGER")?;
        ensure_column(&conn, "wv_inbox_notified", "reserve_token", "TEXT")?;
        ensure_column(&conn, "wv_inbox_notified", "reserved_at", "INTEGER")?;
        ensure_column(&conn, "wv_inbox_notified", "reserved_by", "TEXT")?;
        ensure_column(&conn, "wv_inbox_notified", "reserved_chat", "TEXT")?;
        // The session that keeps a role registration alive, its one-holder mode and
        // the lifetime each renewal grants.
        ensure_column(&conn, "wv_role_registration", "session_id", "TEXT")?;
        ensure_column(
            &conn,
            "wv_role_registration",
            "exclusive",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "wv_role_registration",
            "ttl_secs",
            "INTEGER NOT NULL DEFAULT 60",
        )?;
        // Role queues are keyed by the bare role name (`role:x` and `x` name one
        // queue). Builds before that stored a `send_role_message {role:"role:x"}`
        // literally, so re-key those rows (their inbox row, delivery, receipt and
        // registration) onto the bare name. GLOB is case-sensitive, as the prefix
        // strip is; `role:human` stays put, since `human` is not a role queue.
        // Idempotent: a re-keyed row no longer matches. The store is opened on every
        // inbox read, so a cheap read-only check gates the writes: an already
        // migrated store never takes the write lock here.
        rekey_prefixed_role_rows(&conn)?;
        // Forward-migrate a B1-stub `wv_queue` (id-only) up to the B2 shape. Each
        // is a no-op on a fresh DB and heals an older DB on open. The `CHECK` on
        // `state` and the `UNIQUE` on `dedupe_key` can only be minted by the fresh
        // `CREATE TABLE`; on a healed table the Rust handlers are the enforcement
        // belt (AC-3 verifies `state` transitions, AC-5 verifies dedupe).
        for (col, decl) in [
            ("source", "TEXT NOT NULL DEFAULT ''"),
            ("state", "TEXT NOT NULL DEFAULT 'pending'"),
            ("claimed_by", "TEXT"),
            ("scope", "TEXT"),
            ("created", "INTEGER NOT NULL DEFAULT 0"),
            ("updated", "INTEGER NOT NULL DEFAULT 0"),
            ("refs", "TEXT"),
            ("note", "TEXT"),
            ("dedupe_key", "TEXT"),
            // SW-WV-C2: forward-heal the two tripwire columns onto a B1/B2 stub. The
            // CHECK on `tripwire_verdict` can only be minted by the fresh CREATE
            // above; on a healed table the default 'pending' + the Rust claim/report
            // handlers are the enforcement belt (mirrors the `state`/`dedupe_key`
            // reasoning). Both carry legal defaults so existing rows read correctly
            // (no backfill needed).
            ("tripwire_verdict", "TEXT NOT NULL DEFAULT 'pending'"),
            ("verify_fix_cycles", "INTEGER NOT NULL DEFAULT 0"),
            ("fired_row", "INTEGER"),
            // SW-WV-D4 (AC-1): the peer-lane authorisation list. A nullable JSON
            // array of worker-name strings (`["impl-1","review-1"]`) naming the
            // peers a claimed worker may read via `wv_coord_read` / `wv_peer_read_file`.
            // `NULL` = no peers listed = the lane is closed for that task. Added via
            // `ensure_column` so a pre-existing B1/B2 `weft.sqlite3` forward-migrates
            // losslessly (ARCHITECTURE §1.5). Triage writes rows with no peers; the
            // controller populates it at dispatch time (SW-WV-C1).
            ("dispatch_peers", "TEXT"),
            // SW-WV-C5 (AC-8): the release-time runtime state. `dispatched_at` (epoch
            // seconds) and `dispatch_handle` (the opaque worker handle from spawn) are
            // NULL until `wv_mark_dispatched` fires. Added via `ensure_column` so a
            // pre-existing B2 `weft.sqlite3` (which has `wv_queue` but lacks these)
            // forward-migrates on next open without data loss. Both are nullable, so
            // existing rows read a legal value (no backfill needed). The dispatch BRIEF
            // (goal/context/constraints/done-when) does NOT live here — it lives in
            // `wv_dispatch_records` (C1, D4); C5 only adds these two runtime columns.
            ("dispatched_at", "INTEGER"),
            ("dispatch_handle", "TEXT"),
        ] {
            crate::shared::sqlite::ensure_column(&conn, "wv_queue", col, decl)?;
        }
        // SW-WV-C2 (AC-5): forward-heal the key-ledger `meta` blob onto a B2 stub so
        // the tripwire verdict survives the queue-row prune (no-op on a fresh DB).
        crate::shared::sqlite::ensure_column(&conn, "wv_key_ledger", "meta", "TEXT")?;
        // The state/scope index is created AFTER the columns exist — on a B1-stub
        // table those columns are only present post-migration, so a
        // `CREATE INDEX ... (state, scope)` inside `execute_batch(SCHEMA)` would
        // reference not-yet-migrated columns and fail. Idempotent (`IF NOT EXISTS`).
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_wv_queue_state_scope ON wv_queue(state, scope);",
        )?;

        // SW-WV-B3 — the five sibling ledgers. Each B1 stub carries its correct PK
        // (and, for `wv_attention`, its DTO-read `summary` column) from day one; the
        // remaining columns are nullable-additions migrated in via `ensure_column`,
        // so a pre-existing `weft.sqlite3` from B1/B2 forward-migrates losslessly
        // (ARCHITECTURE §1.5). Defaults match the ACs so a row created before a
        // column existed reads a legal value.

        // wv_decisions (AC-1): standing decisions carry the artifact they bind
        // (`constrains`) and their retirement condition (`ends_when`).
        ensure_column(&conn, "wv_decisions", "date", "TEXT")?;
        ensure_column(&conn, "wv_decisions", "decision", "TEXT")?;
        ensure_column(&conn, "wv_decisions", "constrains", "TEXT")?;
        ensure_column(&conn, "wv_decisions", "ends_when", "TEXT")?;
        ensure_column(&conn, "wv_decisions", "owner", "TEXT")?;
        ensure_column(&conn, "wv_decisions", "source", "TEXT")?;
        ensure_column(
            &conn,
            "wv_decisions",
            "status",
            "TEXT NOT NULL DEFAULT 'active'",
        )?;

        // SW-WV-C6 (AC-1): harvest tracking. Every locked decision becomes a drafted
        // artifact the human approves — `harvest_artifact` is the stable ref (path /
        // PR URL / entity id, NULL while pending or when not-required); `harvest_status`
        // is the harvest state machine (`pending` → `drafted` → `approved`, or
        // `not-required` for a self-contained decision). Added via `ensure_column` on the
        // B3 `wv_decisions` shape so a pre-existing DB forward-migrates losslessly
        // (ARCHITECTURE §1.5); a row created before these existed reads a legal
        // `harvest_status='pending'` default (§1.5 — a default-carrying column is safe
        // via `ensure_column` because it is not a DTO-read-before-migrate column).
        ensure_column(&conn, "wv_decisions", "harvest_artifact", "TEXT")?;
        ensure_column(
            &conn,
            "wv_decisions",
            "harvest_status",
            "TEXT NOT NULL DEFAULT 'pending'",
        )?;

        // wv_roster (AC-2): the durable controller-tree source of truth. `relation`
        // + `parent` distinguish a peer (observe-only) from a child (scope-leased,
        // deprovisionable).
        ensure_column(&conn, "wv_roster", "workspace", "TEXT")?;
        ensure_column(&conn, "wv_roster", "pane", "TEXT")?;
        ensure_column(&conn, "wv_roster", "kind", "TEXT")?;
        ensure_column(&conn, "wv_roster", "subclass", "TEXT")?;
        ensure_column(
            &conn,
            "wv_roster",
            "lifecycle",
            "TEXT NOT NULL DEFAULT 'task'",
        )?;
        ensure_column(&conn, "wv_roster", "owned", "TEXT")?;
        ensure_column(&conn, "wv_roster", "orch_owner", "TEXT")?;
        ensure_column(
            &conn,
            "wv_roster",
            "relation",
            "TEXT NOT NULL DEFAULT 'peer'",
        )?;
        ensure_column(&conn, "wv_roster", "parent", "TEXT")?;
        ensure_column(&conn, "wv_roster", "current_task", "TEXT")?;
        ensure_column(&conn, "wv_roster", "cwd", "TEXT")?;
        ensure_column(&conn, "wv_roster", "status", "TEXT")?;
        // SW-WV-A2 — the typed-ephemeral-worker spawn path's roster columns. B3's
        // baseline `wv_roster` (PK `name`) carries no live-session correlation; A2
        // adds them here via `ensure_column`, NEVER a second `CREATE TABLE` (D2 /
        // ARCHITECTURE §1.5). `name` is the PK and A2 sets it to `story_id` (one
        // worker per story per scope); `story_id` is a redundant convenience column
        // for query readability. `session_id` folds in DW-B3-3 (the durable roster
        // had no session correlation column). All are nullable additions — a
        // pre-existing `weft.sqlite3` forward-migrates without data loss.
        ensure_column(&conn, "wv_roster", "story_id", "TEXT")?;
        ensure_column(&conn, "wv_roster", "session_id", "TEXT")?;
        // SW-WA-A5 (DW-B3-3 / DW-WV-A4-1) — the live-run correlation column. A2 added
        // `session_id` but left `run_id` unhealed, yet `resolve_done_record_context`
        // already SELECTs `run_id FROM wv_roster` (a latent `no such column` on any
        // real store). Added here via `ensure_column` so a pre-A5 `weft.sqlite3`
        // forward-migrates losslessly (nullable — a row written before it existed reads
        // NULL). Stamped alongside `session_id` at the spawn `provision → dispatch`
        // transition so the watchdog / done-record correlation reads the real run key.
        ensure_column(&conn, "wv_roster", "run_id", "TEXT")?;
        ensure_column(&conn, "wv_roster", "scope", "TEXT")?;
        ensure_column(&conn, "wv_roster", "created_at", "TEXT")?;
        ensure_column(&conn, "wv_roster", "updated_at", "TEXT")?;

        // wv_approvals (AC-3): the endorsement loop. `tier` is immutable at insert;
        // `window_state` is meaningful only for T2; `disposition` is the reply state.
        ensure_column(&conn, "wv_approvals", "tier", "TEXT")?;
        ensure_column(&conn, "wv_approvals", "claim", "TEXT")?;
        ensure_column(&conn, "wv_approvals", "posted_at", "TEXT")?;
        ensure_column(&conn, "wv_approvals", "source", "TEXT")?;
        ensure_column(
            &conn,
            "wv_approvals",
            "window_state",
            "TEXT NOT NULL DEFAULT 'open'",
        )?;
        ensure_column(
            &conn,
            "wv_approvals",
            "disposition",
            "TEXT NOT NULL DEFAULT 'pending'",
        )?;
        // SW-WV-E2 (AC-1): the three risk signals the tier-picker reads. Added via
        // `ensure_column` (the same forward-only idiom as every other approval
        // column) so a B3-era `weft.sqlite3` heals on next open without losing
        // rows. The defaults classify a pre-E2 row as low-risk (`irreversible=0`,
        // `blast_radius='low'`, `confidence='high'`) — no false-positive T3
        // escalations. These columns are written by `wv_write_approval` (which
        // stores the picker's decision in `tier`) and read by the tier-picker.
        ensure_column(
            &conn,
            "wv_approvals",
            "irreversible",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "wv_approvals",
            "blast_radius",
            "TEXT NOT NULL DEFAULT 'low'",
        )?;
        ensure_column(
            &conn,
            "wv_approvals",
            "confidence",
            "TEXT NOT NULL DEFAULT 'high'",
        )?;
        // SW-WV-E3 — the Tier-2 approval window. A T2 approval is pre-approved for a
        // bounded interval that CLOSES (auto-ratifies) only in the session it was
        // surfaced in, and whose clock PAUSES while the owner is away. All five
        // columns are epoch-seconds / opaque-text so the elapsed math is
        // deterministic under an injected `now` (no ISO parsing on the hot path).
        // Added via `ensure_column` (forward-only, ARCHITECTURE §1.5) so an
        // E1/E2-era `weft.sqlite3` heals on next open. Defaults classify a pre-E3
        // row as an un-opened window (`window_opened_at` NULL) that no
        // window-close can ratify — no false auto-ratify of a legacy row.
        //   - `window_opened_at`   epoch-s the window was opened (NULL = not yet a live window)
        //   - `window_duration_s`  the granted budget in seconds (NULL until opened)
        //   - `surfaced_session`   the session the item was surfaced to the owner in
        //   - `paused_at`          epoch-s the clock was paused (NULL = running); §4.1-owned
        //   - `pause_accumulated_s` total paused seconds already banked; §4.1-owned
        ensure_column(&conn, "wv_approvals", "window_opened_at", "INTEGER")?;
        ensure_column(&conn, "wv_approvals", "window_duration_s", "INTEGER")?;
        ensure_column(&conn, "wv_approvals", "surfaced_session", "TEXT")?;
        ensure_column(&conn, "wv_approvals", "paused_at", "INTEGER")?;
        ensure_column(
            &conn,
            "wv_approvals",
            "pause_accumulated_s",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

        // wv_watermarks (AC-4): per-source read progress keyed on `(source, scope)`
        // — the same dedup vocabulary as `deliveries`.
        ensure_column(&conn, "wv_watermarks", "last_handled", "TEXT")?;
        ensure_column(&conn, "wv_watermarks", "updated", "TEXT")?;

        // wv_attention (AC-8): every item that needs a human response. `summary` is
        // created in SCHEMA (a DTO-read column — no read-before-migrate window).
        ensure_column(
            &conn,
            "wv_attention",
            "state",
            "TEXT NOT NULL DEFAULT 'needs-you'",
        )?;
        ensure_column(&conn, "wv_attention", "source", "TEXT")?;
        ensure_column(&conn, "wv_attention", "ref", "TEXT")?;
        ensure_column(&conn, "wv_attention", "owner", "TEXT")?;
        ensure_column(&conn, "wv_attention", "created_at", "TEXT")?;
        ensure_column(&conn, "wv_attention", "updated_at", "TEXT")?;
        // SLW-TL-4: the DECLARED need (nullable — an older row / a weft item that
        // predates the field reads NULL, and the board falls back to its `state`).
        // Validated against `WV_ATTENTION_NEEDS` by the write handler, not a CHECK
        // (a forward-healed column cannot mint one).
        ensure_column(&conn, "wv_attention", "need", "TEXT")?;

        // SW-WV-B5 — the scope layer. `wv_scopes` is net-new here (its full shape
        // is created by SCHEMA above); the `ensure_column` calls forward-heal a
        // store that pre-dates any given column (all nullable or defaulted, so an
        // older row reads a legal value). `closed_at` is created at table creation,
        // not via a later `ensure_column`: SW-WV-A5's `deprovision_child` reads it,
        // and a DTO-read column must exist at creation (ARCHITECTURE §1.5). The
        // defaulted `created_at`/`updated_at` are stamped by the handlers at write
        // time; the DEFAULT 0 only guards a forward-migrated legacy row.
        ensure_column(&conn, "wv_scopes", "parent_scope", "TEXT")?;
        ensure_column(&conn, "wv_scopes", "artifact_ptr", "TEXT")?;
        ensure_column(&conn, "wv_scopes", "lease_session", "TEXT")?;
        // SW-AV-17: `owner_session` records who OPENED the scope (the opener's session
        // id), stamped at `wv_scope_create` and decoupled from the lease TTL — the
        // who-opened-whom tree (SW-AV-18) joins on THIS column, not `lease_session`, so
        // it survives the lease-acquire path's deletion (SW-AV-27). Additive + nullable:
        // pre-SW-AV-17 rows heal to a NULL owner (an honest "opener not recorded").
        ensure_column(&conn, "wv_scopes", "owner_session", "TEXT")?;
        ensure_column(
            &conn,
            "wv_scopes",
            "created_at",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "wv_scopes",
            "updated_at",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "wv_scopes", "closed_at", "INTEGER")?;

        // wv_watches — B1 shipped the PK-correct stub `(scope, kind, ref)`. Heal the
        // lifecycle columns (`lifecycle` defaults to 'open', the D1-triage read
        // filter's included state; `updated_at` epoch-ms, stamped by the handlers).
        ensure_column(
            &conn,
            "wv_watches",
            "lifecycle",
            "TEXT NOT NULL DEFAULT 'open'",
        )?;
        ensure_column(
            &conn,
            "wv_watches",
            "updated_at",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

        // The triage read-path index on `(scope, lifecycle)` is created AFTER the
        // columns exist — on a B1-stub `wv_watches` `lifecycle` is only present
        // post-migration, so a `CREATE INDEX ... (scope, lifecycle)` inside
        // `execute_batch(SCHEMA)` would reference a not-yet-healed column and fail.
        // Idempotent (`IF NOT EXISTS`).
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_wv_watches_scope ON wv_watches(scope, lifecycle);",
        )?;

        // SW-WV-E1 — the endorsement-gate ledgers (`wv_assertions`,
        // `wv_recipient_acks`). Both tables are created fresh in SCHEMA above
        // (`CREATE TABLE IF NOT EXISTS`), so on a B1/B2/B3-level DB the CREATE is the
        // real migration and these `ensure_column` guards are no-ops; they are the
        // forward-migration safety net if an earlier E1 DB predated a nullable column
        // (ARCHITECTURE §1.5, forward-only lossless). The `NOT NULL` columns are minted
        // only by the fresh CREATE (a later `ensure_column` cannot add a NOT NULL
        // without a default); the Rust write handlers are the enforcement belt on a
        // healed table (mirrors the `wv_queue` CHECK/UNIQUE reasoning above).
        //
        // `run_id` and `approval_id` are plain columns, NOT cross-file FKs: `runs`
        // lives in `coordinator.sqlite3` (a different DB file), so a SQLite FK to it is
        // impossible; the run-events audit trail is written by the ratify/revoke path
        // into that store directly (ARCH §2.2 — no runtime cross-file SQL join).
        ensure_column(&conn, "wv_assertions", "session_id", "TEXT")?;
        ensure_column(&conn, "wv_assertions", "run_id", "TEXT")?;
        ensure_column(&conn, "wv_assertions", "claim", "TEXT")?;
        ensure_column(&conn, "wv_assertions", "channel", "TEXT")?;
        ensure_column(&conn, "wv_assertions", "posted_at", "TEXT")?;
        ensure_column(
            &conn,
            "wv_assertions",
            "disposition",
            "TEXT NOT NULL DEFAULT 'pending'",
        )?;
        ensure_column(&conn, "wv_assertions", "approval_id", "INTEGER")?;
        // Index for the session+disposition read the FE / ratify path filters on.
        // Created after the columns exist (a B3-stub `wv_assertions` gains them via
        // the guards above), idempotent.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_wv_assertions_session \
             ON wv_assertions(session_id, disposition);",
        )?;

        ensure_column(&conn, "wv_recipient_acks", "assertion_id", "INTEGER")?;
        ensure_column(&conn, "wv_recipient_acks", "recipient", "TEXT")?;
        ensure_column(&conn, "wv_recipient_acks", "ack", "TEXT")?;
        ensure_column(&conn, "wv_recipient_acks", "recorded_at", "TEXT")?;

        // SW-WV-C1 — the 4-field dispatch record (`wv_dispatch_records`). Created
        // fresh in SCHEMA above; these guards forward-heal a store that predates a
        // nullable column (ARCHITECTURE §1.5, forward-only lossless). The NOT NULL
        // columns (`story_id`/`task`/`acceptance`/`created_at`) + the `state` CHECK
        // are minted only by the fresh CREATE (a later `ensure_column` cannot add a
        // NOT NULL without a default, nor a CHECK); the Rust write handlers are the
        // enforcement belt on a healed table (mirrors the `wv_queue` reasoning above).
        // `session_id` is a DTO-read column stamped when the dispatch is marked done.
        ensure_column(&conn, "wv_dispatch_records", "story_id", "TEXT")?;
        ensure_column(&conn, "wv_dispatch_records", "task", "TEXT")?;
        ensure_column(
            &conn,
            "wv_dispatch_records",
            "context",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(
            &conn,
            "wv_dispatch_records",
            "constraints",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(&conn, "wv_dispatch_records", "acceptance", "TEXT")?;
        ensure_column(
            &conn,
            "wv_dispatch_records",
            "state",
            "TEXT NOT NULL DEFAULT 'open'",
        )?;
        ensure_column(&conn, "wv_dispatch_records", "scope", "TEXT")?;
        ensure_column(&conn, "wv_dispatch_records", "session_id", "TEXT")?;
        ensure_column(&conn, "wv_dispatch_records", "created_at", "TEXT")?;
        ensure_column(&conn, "wv_dispatch_records", "done_at", "TEXT")?;
        // Every dispatch-record read filters on `story_id` (the done-stamp resolves a
        // session → story via `wv_roster`, then marks that story's open record done;
        // `dispatch_state` reads by `story_id`). Index the real read filter
        // `(story_id, state)`. Created after the columns exist, idempotent.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_wv_dispatch_story \
             ON wv_dispatch_records(story_id, state);",
        )?;

        // SW-WV-F6 — the render-tripwire rule table (`wv_render_config`). Created
        // fresh in SCHEMA above; the default `scope = 'root'` rules are seeded
        // once here. `INSERT OR IGNORE` on the `(scope, kind, threshold)` PK makes
        // the seed idempotent — re-opening an existing store is a no-op and never
        // clobbers a user override at the same key (ARCHITECTURE §1.5).
        seed_default_render_config(&conn, now_epoch_secs())?;
        // One-time, idempotent heal: older stores can carry pending
        // `wv_queue` rows that `accept_role_message{kind:"work"}` used to mirror
        // from role-inbox deliveries. The write is gone (role work lives only
        // in the acceptance receipt + ack), so prune exactly the stuck
        // `source='role_inbox' AND state='pending'` rows and write their
        // `wv_key_ledger` siblings so the audit trail survives. Rows in
        // `claimed`/`dispatched` keep their controller owner and resolve
        // through the normal `queue::resolve` path. A second open finds no
        // matching rows and performs no writes.
        super::queue::prune_orphan_role_inbox_rows(&mut conn, now_epoch_secs())?;
        // SW-WV-B4 — signals (ephemeral) + coord (durable, one-owner). Both tables
        // are created fresh in SCHEMA above; these guards forward-heal a store that
        // predates a nullable column (ARCHITECTURE §1.5, forward-only lossless). The
        // NOT NULL columns (`wv_signals.worker/task_id/session_id/status/created`,
        // `wv_coord.contract/owner_dispatch/owner_worker/body/created/updated`) + the
        // `wv_coord.contract UNIQUE` are minted only by the fresh CREATE (a later
        // `ensure_column` cannot add a NOT NULL without a default nor a UNIQUE); the
        // Rust write handlers are the enforcement belt on a healed table. Only the
        // truly-optional columns are ensured here — the required ones must not be
        // added post-hoc (they carry no default and would break a healed insert).
        ensure_column(&conn, "wv_signals", "dispatch", "TEXT")?;
        ensure_column(&conn, "wv_signals", "packet_refs", "TEXT")?;
        // The `wv_coord.owner_worker` read index (AC-2 grounding — Task 4). Created
        // after the column exists; idempotent (`IF NOT EXISTS`).
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_coord_owner ON wv_coord(owner_worker);",
        )?;

        // SW-WV-C3 — the 3-leg done record (`wv_done_record`). Created fresh in
        // SCHEMA above; these guards forward-heal a store that predates a nullable /
        // defaulted column (ARCHITECTURE §1.5, forward-only lossless). The `dispatch_id`
        // PRIMARY KEY and the `queue_id`/`session_id`/`run_id`/`story_id`/`subclass`/
        // `leg1_source`/`created` NOT NULL columns are minted only by the fresh CREATE
        // (a later `ensure_column` cannot add a NOT NULL without a default nor a PK);
        // the Rust write handlers are the enforcement belt on a healed table (mirrors
        // the `wv_queue` reasoning above). `queue_id` is a PLAIN column, NOT a
        // `REFERENCES wv_queue(id)` FK: the queue row is DELETED by the prune
        // (`wv_resolve_queue`) once the fold is permitted, so an FK would block the
        // very prune the done-record enables (a DEFERRABLE FK still forbids a dangling
        // child at commit). The done record + `wv_key_ledger` are the durable audit
        // trail that OUTLIVES the pruned queue row — same "plain id, no cross-lifetime
        // FK" reasoning as `wv_key_ledger.queue_id` and `wv_assertions.run_id`. Only
        // the defaulted verification columns are ensured here so a store created before
        // C3 heals its leg/verify columns to their legal `0` defaults on next open.
        ensure_column(
            &conn,
            "wv_done_record",
            "leg1_verified",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "wv_done_record",
            "leg2_footprint",
            "TEXT NOT NULL DEFAULT '[]'",
        )?;
        ensure_column(
            &conn,
            "wv_done_record",
            "leg2_verified",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "wv_done_record",
            "leg3_packet",
            "TEXT NOT NULL DEFAULT '{}'",
        )?;
        ensure_column(
            &conn,
            "wv_done_record",
            "leg3_idempotent",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "wv_done_record",
            "leg3_verified",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "wv_done_record",
            "controller_verified",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "wv_done_record",
            "done_when_met",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "wv_done_record", "verified_at", "INTEGER")?;
        // The queue-fold read index (`wv_verify_done` / `wv_resolve_queue` read by
        // `queue_id`). Created after the columns exist; idempotent.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_done_record_queue ON wv_done_record(queue_id);",
        )?;

        // SW-WV-C4 — the auto-drafted handoff packet (`wv_handoff_records`). Created
        // fresh in SCHEMA above; these guards forward-heal a store that predates a
        // nullable/defaulted column (ARCHITECTURE §1.5, forward-only lossless). The
        // `story_id`/`session_id`/`finding`/`created_at` NOT NULL columns and the
        // `is_draft = 1` CHECK are minted only by the fresh CREATE (a later
        // `ensure_column` cannot add a NOT NULL without a default nor a CHECK); the Rust
        // write handler is the enforcement belt on a healed table (mirrors the
        // `wv_dispatch_records` reasoning above). A handoff packet is DATA, never
        // instructions (ARCHITECTURE §3.3) and always a DRAFT — the schema forbids any
        // authoritative form: `is_draft` may only ever be `1` (draft ≠ authority; no
        // verb promotes a handoff), and there is no "instruction"/"command" column for a
        // finding to smuggle an order into.
        ensure_column(&conn, "wv_handoff_records", "story_id", "TEXT")?;
        ensure_column(&conn, "wv_handoff_records", "session_id", "TEXT")?;
        ensure_column(
            &conn,
            "wv_handoff_records",
            "to_role",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(&conn, "wv_handoff_records", "finding", "TEXT")?;
        ensure_column(
            &conn,
            "wv_handoff_records",
            "refs",
            "TEXT NOT NULL DEFAULT '[]'",
        )?;
        ensure_column(
            &conn,
            "wv_handoff_records",
            "is_draft",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        ensure_column(&conn, "wv_handoff_records", "created_at", "TEXT")?;
        // Every handoff read/resolve filters on `story_id` (the done hook resolves a
        // session → story via `wv_roster`, then flips the roster to `handoff` when a
        // packet exists; `handoff_state` reads by `story_id`). Index that read filter.
        // Created after the columns exist, idempotent.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_wv_handoff_story \
             ON wv_handoff_records(story_id);",
        )?;

        let store = Self {
            path: path.clone(),
            pool: std::sync::Arc::new(std::sync::Mutex::new(vec![conn])),
        };
        if path != std::path::Path::new(":memory:") {
            stores.insert(path, store.clone());
        }
        Ok(store)
    }

    /// Open a fresh connection to the shared DB file via the shared per-connection
    /// plumbing (busy-timeout + `synchronous=NORMAL` + `foreign_keys=ON`). Safe on
    /// the blocking pool; WAL is a durable file property set once at [`Self::open`].
    pub fn conn(&self) -> WeftStoreResult<super::store::StoreConnection> {
        let conn = self.pool.lock().unwrap_or_else(|e| e.into_inner()).pop();
        let connection = match conn {
            Some(conn) => conn,
            None => crate::shared::sqlite::open_conn(&self.path)?,
        };
        Ok(super::store::StoreConnection {
            connection: Some(connection),
            pool: self.pool.clone(),
        })
    }
}

// ---------- SW-WV-F6: render-tripwire rule table (`wv_render_config`) ----------

/// The 5 default render-routing rules, seeded once per store for `scope='root'`.
/// The `(kind, threshold, target, priority)` tuples mirror the story's default
/// table: `has_artifacts` > `subclass` > `message_len` by priority so the
/// tripwire's precedence is data-driven, not code-ordered. Seeded via
/// `INSERT OR IGNORE` so a user override at the same `(scope, kind, threshold)`
/// PK is never clobbered and re-opening is a no-op.
const DEFAULT_RENDER_RULES: &[(&str, &str, &str, i64)] = &[
    ("has_artifacts", "1", "artifact", 30),
    ("subclass", "review", "artifact", 20),
    ("subclass", "research", "drill_doc", 20),
    ("message_len", "300", "terminal", 10),
    ("message_len", "9999", "drill_doc", 10),
];

/// Seed the `scope='root'` default render rules. Idempotent: `INSERT OR IGNORE`
/// on the `(scope, kind, threshold)` PK means an existing DB (with the defaults
/// or with user overrides at those keys) is left untouched. This is the ONE
/// sanctioned writer of `wv_render_config` (the whole `src/weft/` tree is
/// exempted by the `only_weft_sqlite_writes_wv_tables` grep test).
fn seed_default_render_config(conn: &Connection, now: i64) -> WeftStoreResult<()> {
    for (kind, threshold, target, priority) in DEFAULT_RENDER_RULES {
        conn.execute(
            "INSERT OR IGNORE INTO wv_render_config \
                 (scope, kind, threshold, target, priority, created_at) \
             VALUES ('root', ?1, ?2, ?3, ?4, ?5)",
            params![kind, threshold, target, priority, now],
        )?;
    }
    Ok(())
}

impl WeftStore {
    /// Load every render rule for `scope`, highest `priority` first. Consumed by
    /// [`super::render_tripwire::evaluate_render_target`]; the ordering lets the
    /// pure evaluator return on first match without re-sorting (it re-sorts
    /// defensively anyway, so a hand-inserted out-of-order row is still correct).
    pub fn load_render_config(
        &self,
        scope: &str,
    ) -> WeftStoreResult<Vec<super::render_tripwire::RenderRule>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT kind, threshold, target, priority \
             FROM wv_render_config WHERE scope=?1 ORDER BY priority DESC",
        )?;
        let rows = stmt
            .query_map(params![scope], |r| {
                Ok(super::render_tripwire::RenderRule {
                    kind: r.get(0)?,
                    threshold: r.get(1)?,
                    target: r.get(2)?,
                    priority: r.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

/// A Unix-epoch-seconds timestamp for an audit row. Monotonic wall-clock is not
/// required here — `wv_write_log.written_at` is a coarse audit stamp.
fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// SW-WV-D1: the current unix-epoch seconds, for callers outside this module (the
/// triage sweep stamps `created`/`updated` and the watermark `now` with it).
pub(crate) fn now_epoch_secs_public() -> i64 {
    now_epoch_secs()
}

/// Append the audit row for a mutation into `wv_write_log`, on the *same*
/// transaction as the ledger write (AC-7). Called by every write handler before
/// `tx.commit()` — never fire-and-forget, so a crash between the ledger write and
/// this leaves neither row.
fn write_audit(
    tx: &Transaction,
    table: &'static str,
    operation: &'static str,
    row_id: i64,
) -> WeftStoreResult<()> {
    write_audit_as(tx, table, operation, row_id, None)
}

pub(super) fn write_audit_as(
    tx: &Transaction,
    table: &'static str,
    operation: &'static str,
    row_id: i64,
    caller_session: Option<&str>,
) -> WeftStoreResult<()> {
    tx.execute(
        "INSERT INTO wv_write_log (tbl, operation, row_id, written_at, caller_session) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![table, operation, row_id, now_epoch_secs(), caller_session],
    )?;
    Ok(())
}

/// SW-WV-B2: audit-write seam for sibling `weft::` modules (e.g. [`super::queue`])
/// that own their own IMMEDIATE transaction and must record the ledger mutation in
/// the SAME transaction (ARCHITECTURE §1.6, AC-7). Thin re-export of [`write_audit`]
/// so the audit-row SQL still lives only here (the sole sanctioned writer) — a
/// caller passes its open `tx` before `commit()`, never fire-and-forget.
pub(super) fn write_audit_public(
    tx: &Transaction,
    table: &'static str,
    operation: &'static str,
    row_id: i64,
) -> WeftStoreResult<()> {
    write_audit(tx, table, operation, row_id)
}

/// Mint the next id for a live-max table: `SELECT COALESCE(MAX(id),0)+1 FROM
/// {table}` on the *caller-supplied* IMMEDIATE transaction (AC-2).
///
/// It does NOT open its own transaction — [`wv_append`] owns the IMMEDIATE
/// transaction and passes it in, so the max-read and the subsequent INSERT are
/// atomic and a concurrent writer cannot steal the id between them. This is the
/// mechanical invariant that killed herdr's cached-counter collision; ids never
/// come from an in-memory counter.
///
/// `table` is a compile-time `&'static str` constant (never user input), so the
/// unavoidable interpolation into the SELECT carries no injection risk — the same
/// justification as `ensure_column` in `shared/sqlite.rs`. Used only for the
/// live-max `wv_queue` family.
pub fn wv_id_next(tx: &Transaction, table: &'static str) -> WeftStoreResult<i64> {
    let next: i64 = tx.query_row(
        &format!("SELECT COALESCE(MAX(id), 0) + 1 FROM {table}"),
        [],
        |r| r.get(0),
    )?;
    Ok(next)
}

/// Append a row to a **live-max** table (the `wv_queue` family) through the
/// sanctioned write path (AC-1, AC-2, AC-7).
///
/// Opens one IMMEDIATE transaction, mints the next id via [`wv_id_next`],
/// prepends it to the caller's params, executes the caller-supplied INSERT (whose
/// first bind param `?1` is the id), writes the audit row into `wv_write_log`
/// inside the same transaction, commits, and returns the inserted id. Mirrors
/// `enqueue_runnable_stmt`'s IMMEDIATE-transaction structure.
///
/// The caller-supplied `row_sql` must bind the id as `?1` (e.g.
/// `INSERT INTO wv_queue (id, state) VALUES (?1, ?2)`); `params` are the
/// remaining values in `?2..` order. `&mut Connection` is required because
/// opening a transaction takes `&mut self`.
pub fn wv_append(
    conn: &mut Connection,
    table: &'static str,
    row_sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> WeftStoreResult<i64> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = wv_id_next(&tx, table)?;
    // Prepend the minted id as `?1`; the caller's params follow in `?2..`.
    let mut bound: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(params.len() + 1);
    bound.push(&id);
    bound.extend_from_slice(params);
    tx.execute(row_sql, bound.as_slice())?;
    write_audit(&tx, table, "append", id)?;
    tx.commit()?;
    Ok(id)
}

/// Append a row to an **AUTOINCREMENT** table (every non-queue ledger) through
/// the sanctioned write path (AC-1, AC-7).
///
/// Shares [`wv_append`]'s IMMEDIATE-transaction + audit-row structure; the only
/// difference is id minting — SQLite assigns the id via `AUTOINCREMENT`, so the
/// caller's `insert_sql` omits the `id` column and the returned id comes from
/// `last_insert_rowid()`. B2+ callers use this for all non-queue tables.
pub fn wv_auto_append(
    conn: &mut Connection,
    insert_sql: &str,
    params: &[&dyn rusqlite::ToSql],
    table: &'static str,
) -> WeftStoreResult<i64> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(insert_sql, params)?;
    let id = tx.last_insert_rowid();
    write_audit(&tx, table, "append", id)?;
    tx.commit()?;
    Ok(id)
}

/// A single `wv_write_log` audit row, as read by the B6 inspection surface.
///
/// Fields mirror B1's actual `wv_write_log` columns 1:1 (`tbl` is surfaced as
/// `table_name` on the DTO for a clearer wire name). `written_at` is epoch
/// **seconds** (B1's [`now_epoch_secs`]); `row_id` is the affected `wv_*` row's
/// integer id. Queue writes also retain the current verified caller session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRow {
    /// The audit row's own AUTOINCREMENT id.
    pub id: i64,
    /// The affected `wv_*` table name (the `tbl` column).
    pub table_name: String,
    /// The mutation kind B1 recorded: `"append"` or `"mutate"`.
    pub operation: String,
    /// The affected `wv_*` row's id.
    pub row_id: i64,
    /// Epoch **seconds** the audit row was written.
    pub written_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_session: Option<String>,
}

/// Filters for [`WeftStore::query_audit`]. Every field is optional except
/// `limit`; supplied filters are ANDed. `since_id`/`before_id` are exclusive
/// cursor bounds on the AUTOINCREMENT `id`; `after_at` is an inclusive-lower
/// bound on `written_at` (epoch **seconds**).
#[derive(Debug, Clone, Default)]
pub struct AuditQueryParams {
    /// Filter on the `tbl` column (the affected `wv_*` table name).
    pub table_name: Option<String>,
    /// Filter on the `operation` column (`"append"` | `"mutate"`).
    pub operation: Option<String>,
    /// Exclusive lower bound on `id` (`id > since_id`) — cursor pagination.
    pub since_id: Option<i64>,
    /// Exclusive upper bound on `id` (`id < before_id`) — cursor pagination.
    pub before_id: Option<i64>,
    /// Lower bound on `written_at` in epoch seconds (`written_at >= after_at`).
    pub after_at: Option<i64>,
    /// Max rows to return. Clamped to `[1, AUDIT_QUERY_MAX_LIMIT]`; a 0 or
    /// absent value resolves to [`AUDIT_QUERY_DEFAULT_LIMIT`] at the caller.
    pub limit: u32,
}

/// Default page size for [`WeftStore::query_audit`] when the caller supplies no
/// (or a zero) limit.
pub const AUDIT_QUERY_DEFAULT_LIMIT: u32 = 100;
/// Hard cap on a single [`WeftStore::query_audit`] page.
pub const AUDIT_QUERY_MAX_LIMIT: u32 = 500;

impl WeftStore {
    /// Read audit rows from `wv_write_log` (B6 — AC-1, AC-2).
    ///
    /// Read-only: issues a single parameterised `SELECT … ORDER BY id DESC
    /// LIMIT ?`. Not a `wv_*` write, so it is exempt from the ONE-WRITER rule,
    /// but it still lives here (MODULE-HOME) — no `wv_*` SQL, read or write, in
    /// `handler.rs`. Runs synchronously on the calling thread; the MCP handler
    /// wraps it in `spawn_blocking` (ARCHITECTURE §1.6).
    ///
    /// `limit` is clamped to `[1, AUDIT_QUERY_MAX_LIMIT]`, defaulting a 0/absent
    /// value to [`AUDIT_QUERY_DEFAULT_LIMIT`]. Filters are ANDed; each is applied
    /// only when present, so an all-`None` params (with a limit) returns the most
    /// recent `limit` rows across the whole log.
    pub fn query_audit(&self, params: AuditQueryParams) -> WeftStoreResult<Vec<AuditRow>> {
        let limit = clamp_audit_limit(params.limit);

        // Build the WHERE incrementally with named params so an absent filter
        // adds neither clause nor bind. `table` is a reserved word so the column
        // is `tbl`; surface it as `table_name` on the DTO.
        let mut clauses: Vec<&str> = Vec::new();
        let mut binds: Vec<(&str, &dyn rusqlite::ToSql)> = Vec::new();
        if let Some(t) = &params.table_name {
            clauses.push("tbl = :table_name");
            binds.push((":table_name", t));
        }
        if let Some(op) = &params.operation {
            clauses.push("operation = :operation");
            binds.push((":operation", op));
        }
        if let Some(since) = &params.since_id {
            clauses.push("id > :since_id");
            binds.push((":since_id", since));
        }
        if let Some(before) = &params.before_id {
            clauses.push("id < :before_id");
            binds.push((":before_id", before));
        }
        if let Some(after) = &params.after_at {
            clauses.push("written_at >= :after_at");
            binds.push((":after_at", after));
        }
        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        // `limit` is a validated `i64` from a `u32` clamp — bind it too rather
        // than interpolate, keeping the whole statement parameterised.
        let limit_i64 = i64::from(limit);
        binds.push((":limit", &limit_i64));

        let sql = format!(
            "SELECT id, tbl, operation, row_id, written_at, caller_session FROM wv_write_log \
             {where_sql} ORDER BY id DESC LIMIT :limit"
        );

        let conn = self.conn()?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(binds.as_slice(), |r| {
            Ok(AuditRow {
                id: r.get(0)?,
                table_name: r.get(1)?,
                operation: r.get(2)?,
                row_id: r.get(3)?,
                written_at: r.get(4)?,
                caller_session: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Prune `wv_write_log` rows older than `cutoff_secs` (B6 — AC-3).
    ///
    /// The audit trail is append-only; this is the ONE maintenance mutation B6
    /// introduces, and it targets `wv_write_log` exclusively. It runs a single
    /// guarded `DELETE FROM wv_write_log WHERE written_at < ?1` and returns the
    /// number of rows removed. `cutoff_secs` is epoch **seconds** (B1's
    /// `written_at` unit); the MCP handler converts its millisecond arg first.
    ///
    /// No audit-of-the-audit row is written — the trail has no self-referential
    /// entry (story Out-of-scope: "Auditing `wv_write_log` itself"). Runs
    /// synchronously on the calling thread; the handler wraps it in
    /// `spawn_blocking`.
    pub fn prune_audit(&self, cutoff_secs: i64) -> WeftStoreResult<u64> {
        let conn = self.conn()?;
        let deleted = conn.execute(
            "DELETE FROM wv_write_log WHERE written_at < ?1",
            params![cutoff_secs],
        )?;
        Ok(deleted as u64)
    }
}

/// Clamp a caller-supplied audit page size into `[1, AUDIT_QUERY_MAX_LIMIT]`,
/// treating 0 (or an absent value that deserialised to 0) as
/// [`AUDIT_QUERY_DEFAULT_LIMIT`].
fn clamp_audit_limit(limit: u32) -> u32 {
    let effective = if limit == 0 {
        AUDIT_QUERY_DEFAULT_LIMIT
    } else {
        limit
    };
    effective.min(AUDIT_QUERY_MAX_LIMIT)
}

/// Mutate a single row under the single-writer rule (AC-3).
///
/// Opens one IMMEDIATE transaction and executes the caller-supplied UPDATE, which
/// MUST embed a `WHERE id=?1 AND {owner_col}=?2` guard as its first two bind
/// params (`id` then `owner`). Returns [`MutateResult::Updated`] when the guard
/// matched exactly one row (`changes()==1`) or [`MutateResult::NotOwner`] when it
/// matched none (`changes()==0`) — a CAS loss is an ordinary return, not an
/// error. The single-writer rule is enforced by the SQL guard, not Rust-level
/// locking (mirrors `claim_ready`'s CAS pattern).
///
/// The audit row is written inside the same transaction **only on `Updated`**;
/// `NotOwner` changed nothing, so it writes no audit row. `owner_col` is a
/// compile-time `&'static str` constant (e.g. `"claimed_by"` for queue rows,
/// `"orch_owner"` for roster rows) — never user input.
///
/// The caller passes `id` and `owner` for the guard; `params` are any *additional*
/// bind values the UPDATE's SET clause needs, in `?3..` order.
pub fn wv_mutate_owned(
    conn: &mut Connection,
    table: &'static str,
    id: i64,
    _owner_col: &'static str,
    owner: &str,
    update_sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> WeftStoreResult<MutateResult> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Bind the guard params first (`?1` = id, `?2` = owner), then any SET params.
    let mut bound: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(params.len() + 2);
    bound.push(&id);
    bound.push(&owner);
    bound.extend_from_slice(params);
    let changed = tx.execute(update_sql, bound.as_slice())?;
    if changed == 1 {
        write_audit(&tx, table, "mutate", id)?;
        tx.commit()?;
        Ok(MutateResult::Updated)
    } else {
        // No row changed — the caller is not the current owner. Nothing to audit;
        // drop the transaction (rolls back the no-op).
        Ok(MutateResult::NotOwner)
    }
}

// ============================================================================
// SW-WV-B3 — the five sibling ledgers' named write handlers.
//
// Every handler below lives HERE (the sole sanctioned writer file, ARCHITECTURE
// §1.2) and routes its ledger mutation + its `wv_write_log` audit row through one
// IMMEDIATE transaction (ARCHITECTURE §1.6, AC-9). None of them opens a
// transaction outside this file; the ONE-WRITER grep lint
// (`only_weft_sqlite_writes_wv_tables`) excludes only this file, so the raw
// `INSERT/UPDATE/DELETE wv_*` SQL is legal here and nowhere else.
//
// All handlers are synchronous SQLite; callers run them on the blocking pool
// (ARCHITECTURE §1.6, AC-7). No handler awaits — `wv_write_approval`'s T3 branch
// registers + emits through the injected [`ApprovalNotifier`] and returns; it never
// awaits the reply (that arrives via `wv_settle_approval`).
// ============================================================================

/// An ISO-8601 (RFC-3339) wall-clock stamp for a B3 write-time column
/// (`posted_at`, `created_at`, `updated_at`). Semantic timestamps a caller owns
/// (`wv_decisions.date`, `wv_watermarks.last_handled`) are passed in, not minted.
pub(super) fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// The allowed `source` enum for `wv_watermarks` (and `wv_queue.source`) — the
/// same values a source-sweep / dispatch dedup keys on (AC-4). A write with any
/// other value is rejected before it can poison the compound PK.
pub const WV_WATERMARK_SOURCES: &[&str] = &["slack", "jira", "github", "local", "agent"];

/// The allowed `wv_attention.state` transitions' value set (AC-8).
pub const WV_ATTENTION_STATES: &[&str] = &["needs-you", "waiting", "done-unacked"];

/// SLW-TL-4 — the fixed set of `need` values a board item may DECLARE (§2.3): what
/// the item needs from the human, so the Inbox can map need → the matching action
/// (SLW-TL-7 wires the actions; this story lands the field). Orthogonal to `state`
/// (the lane): a `needs-approval` item is a `needs-you` row; a `done` need is a
/// `done-unacked` row. A write with any other value is rejected. The internal
/// substrate stays `wv_attention` — only the item now carries a declared need so
/// ANY producer (agent / CLI / trigger), not just weft, can post an actionable item.
pub const WV_ATTENTION_NEEDS: &[&str] = &[
    "awaiting-answer",
    "needs-approval",
    "needs-attention",
    "awaiting-validation",
    "done",
];

/// The allowed `wv_approvals.disposition` settle targets (AC-3). `pending` is the
/// insert default and is never a settle target.
pub const WV_APPROVAL_DISPOSITIONS: &[&str] = &["ratified", "revoked"];

/// A validation refusal raised by a B3 write handler before it touches the store —
/// a blank required field, an out-of-enum value, or a guard the protocol semantics
/// forbid (e.g. deprovisioning a peer). Kept distinct from [`WeftStoreError`] so a
/// caller (the MCP verb / Tauri command) maps it to an `invalid_params` refusal,
/// never a storage error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WvValidation(pub String);

impl std::fmt::Display for WvValidation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for WvValidation {}

/// The result of a B3 write handler: either a storage error or a validation
/// refusal, distinguished so the boundary maps each to the right MCP outcome.
#[derive(Debug)]
pub enum WvWriteError {
    Store(WeftStoreError),
    Invalid(WvValidation),
}
impl std::fmt::Display for WvWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WvWriteError::Store(e) => write!(f, "{e}"),
            WvWriteError::Invalid(v) => write!(f, "{v}"),
        }
    }
}
impl std::error::Error for WvWriteError {}
impl From<WeftStoreError> for WvWriteError {
    fn from(e: WeftStoreError) -> Self {
        WvWriteError::Store(e)
    }
}
impl From<rusqlite::Error> for WvWriteError {
    fn from(e: rusqlite::Error) -> Self {
        WvWriteError::Store(WeftStoreError::Sqlite(e))
    }
}
impl From<WvValidation> for WvWriteError {
    fn from(v: WvValidation) -> Self {
        WvWriteError::Invalid(v)
    }
}

pub type WvWriteResult<T> = Result<T, WvWriteError>;

fn reject(msg: impl Into<String>) -> WvWriteError {
    WvWriteError::Invalid(WvValidation(msg.into()))
}

/// SW-WV-A5: construct a typed validation refusal from a cross-store failure message so a
/// coordinator-store error surfaces on the same `WvWriteError` channel A5's weft-store
/// writes use, rather than a bare string escaping the child-controller module.
pub fn wv_refusal(msg: impl Into<String>) -> WvWriteError {
    reject(msg)
}

pub(super) fn is_blank(s: &str) -> bool {
    s.trim().is_empty()
}

// ---------- wv_decisions (AC-1) ----------

/// Insert a standing decision (AC-1). Rejects a blank/whitespace `ends_when` — an
/// unbounded decision is a silent permanent veto, the exact failure `constrains`/
/// `ends_when` were added to prevent. `status` defaults to `'active'`.
///
/// Returns the AUTOINCREMENT id. Audit row co-committed (AC-9).
#[allow(clippy::too_many_arguments)]
pub fn wv_write_decision(
    store: &WeftStore,
    date: &str,
    decision: &str,
    constrains: &str,
    ends_when: &str,
    owner: &str,
    source: Option<&str>,
) -> WvWriteResult<i64> {
    if is_blank(ends_when) {
        return Err(reject(
            "ends_when must not be blank (a standing decision must be bounded)",
        ));
    }
    if is_blank(decision) {
        return Err(reject("decision must not be blank"));
    }
    if is_blank(constrains) {
        return Err(reject("constrains must not be blank"));
    }
    let mut conn = store.conn()?;
    let id = wv_auto_append(
        &mut conn,
        "INSERT INTO wv_decisions \
         (date, decision, constrains, ends_when, owner, source, status) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active')",
        &[&date, &decision, &constrains, &ends_when, &owner, &source],
        "wv_decisions",
    )?;
    Ok(id)
}

/// Retire a decision by id: set `status='superseded'`. Returns `false` when no
/// active decision matched the id (a no-op the caller can report), `true` when one
/// was retired. Audit row co-committed on the update (AC-9).
pub fn wv_retire_decision(store: &WeftStore, id: i64) -> WvWriteResult<bool> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_decisions SET status='superseded' WHERE id=?1 AND status<>'superseded'",
        params![id],
    )?;
    if changed == 1 {
        write_audit(&tx, "wv_decisions", "retire", id)?;
        tx.commit()?;
        Ok(true)
    } else {
        Ok(false)
    }
}

// ---------- wv_decisions harvest (SW-WV-C6) ----------

/// SW-WV-C6: the allowed `wv_decisions.harvest_status` values (AC-1/AC-2). `pending`
/// is the insert default; `drafted` after `wv_set_harvest_artifact`; `approved` after
/// the human 👍 closes the loop; `not-required` when the controller flags the decision
/// self-contained. A row's status never leaves `approved` (a terminal state).
pub const WV_HARVEST_STATUSES: &[&str] = &["pending", "drafted", "approved", "not-required"];

/// SW-WV-C6: the typed refusals the harvest mutations raise. Kept as a dedicated enum
/// so the boundary (`WvBlockingErr` → `unwrap_wv_blocking`) maps each to an
/// `invalid_params` outcome the caller branches on (never a storage error). The axiom
/// "they approve the artifact; you do the ferrying" is enforced structurally:
/// `AlreadyApproved` refuses any re-mutation of a closed harvest (a CAS loser gets a
/// typed error, not a silent no-op — ARCHITECTURE §1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarvestRefusal {
    /// A mutation was refused because the decision's `harvest_status` is already
    /// `'approved'` (a terminal state — the human has closed the loop).
    AlreadyApproved,
    /// `wv_approve_harvest` was called on a decision whose `harvest_artifact` is NULL
    /// (there is no drafted artifact to approve).
    NoDraftedArtifact,
    /// The `kind` argument to `wv_set_harvest_artifact` is not a recognized token
    /// per [`crate::commands::sprint_run::run_store::ArtifactKind::from_wire`]
    /// (`file` | `pr` | `entity`).
    InvalidArtifactKind,
}

impl HarvestRefusal {
    fn message(self) -> &'static str {
        match self {
            HarvestRefusal::AlreadyApproved => {
                "harvest already approved — a closed harvest is terminal and cannot be re-mutated"
            }
            HarvestRefusal::NoDraftedArtifact => {
                "no drafted artifact — set harvest_artifact via wv_set_harvest_artifact before approving"
            }
            HarvestRefusal::InvalidArtifactKind => {
                "kind must be one of: file | pr | entity"
            }
        }
    }
}

impl From<HarvestRefusal> for WvWriteError {
    fn from(r: HarvestRefusal) -> Self {
        WvWriteError::Invalid(WvValidation(r.message().to_string()))
    }
}

/// SW-WV-C6: read `(harvest_status, harvest_artifact)` for a decision inside a tx.
/// Returns `None` when the id names no row. Kept tiny so every harvest mutation reads
/// the current state through the same guard before its guarded `UPDATE`.
fn read_harvest_state(
    tx: &Transaction,
    id: i64,
) -> WeftStoreResult<Option<(String, Option<String>)>> {
    let row = tx
        .query_row(
            "SELECT harvest_status, harvest_artifact FROM wv_decisions WHERE id=?1",
            params![id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .ok();
    Ok(row)
}

/// SW-WV-C6 (AC-2): set `harvest_artifact` and advance `harvest_status` from
/// `'pending'` to `'drafted'`. The controller drafts the artifact; this records the
/// ferrying. Behavior:
/// - Unrecognized `kind` → `Err(InvalidArtifactKind)` (validated before any write).
/// - `harvest_status='approved'` → `Err(AlreadyApproved)` (terminal, per §1.2).
/// - `harvest_status='drafted'` and `artifact_ref` equals the stored ref → `Ok(())`
///   (idempotent re-draft, no write).
/// - Otherwise → set the ref + `'drafted'`; a lost CAS (`changes()==0`) is a typed
///   `IllegalState` (§1.2 — never a silent no-op).
///
/// Audit row co-committed in the same `IMMEDIATE` transaction (§1.6).
pub fn wv_set_harvest_artifact(
    store: &WeftStore,
    id: i64,
    artifact_ref: &str,
    kind: &str,
) -> WvWriteResult<()> {
    // Boundary-validate the kind first — a bad kind is a refusal before any tx.
    if crate::commands::sprint_run::run_store::ArtifactKind::from_wire(kind).is_none() {
        return Err(HarvestRefusal::InvalidArtifactKind.into());
    }
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    match read_harvest_state(&tx, id)? {
        None => {
            return Err(WvWriteError::Store(WeftStoreError::IllegalState {
                table: "wv_decisions",
                id,
                attempted: "set_harvest_artifact",
            }));
        }
        Some((status, existing_ref)) => {
            if status == "approved" {
                return Err(HarvestRefusal::AlreadyApproved.into());
            }
            // Idempotent re-draft: same ref on an already-drafted row is a no-op.
            if status == "drafted" && existing_ref.as_deref() == Some(artifact_ref) {
                return Ok(());
            }
        }
    }
    let changed = tx.execute(
        "UPDATE wv_decisions SET harvest_artifact=?2, harvest_status='drafted' \
         WHERE id=?1 AND harvest_status <> 'approved'",
        params![id, artifact_ref],
    )?;
    if changed != 1 {
        return Err(WvWriteError::Store(WeftStoreError::IllegalState {
            table: "wv_decisions",
            id,
            attempted: "set_harvest_artifact",
        }));
    }
    write_audit(&tx, "wv_decisions", "harvest_draft", id)?;
    tx.commit()?;
    Ok(())
}

/// SW-WV-C6 (AC-3): close the harvest loop by setting `harvest_status='approved'`.
/// Called from the `wv_settle_approval` reply path once a human 👍s a `harvest:<id>`
/// T3 approval. The reverse link (`harvest_artifact`) must be non-null first:
/// - `harvest_artifact` NULL → `Err(NoDraftedArtifact)` (nothing drafted to approve).
/// - `harvest_status='approved'` → `Err(AlreadyApproved)` (a CAS loser gets a typed
///   error, not a silent no-op — §1.2).
/// - Otherwise → set `'approved'`; a lost CAS (`changes()==0`) is a typed
///   `IllegalState`.
///
/// `approval_id` is the ratifying `wv_approvals` row — the backlink is the approval's
/// `source='harvest:<decision_id>'` string (no FK; both directions traverse without a
/// JOIN, AC-6). Audit row co-committed in the same `IMMEDIATE` transaction (§1.6).
pub fn wv_approve_harvest(
    store: &WeftStore,
    decision_id: i64,
    _approval_id: i64,
) -> WvWriteResult<()> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    approve_harvest_in_tx(&tx, decision_id)?;
    tx.commit()?;
    Ok(())
}

/// SW-WV-C6: the guarded harvest-close, tx-scoped so it can run either standalone
/// ([`wv_approve_harvest`]) or joined into the [`wv_settle_approval`] IMMEDIATE tx
/// (the `harvest:<id>` ratify path) — the settle and the close then commit atomically
/// (AUDIT-ATOMIC §1.6: the approval settle, the harvest flip, and both audit rows land
/// together or not at all). Reads state through the same guard, applies the guarded
/// `UPDATE`, and co-commits the audit row (the caller commits the tx).
fn approve_harvest_in_tx(tx: &Transaction, decision_id: i64) -> WvWriteResult<()> {
    match read_harvest_state(tx, decision_id)? {
        None => {
            return Err(WvWriteError::Store(WeftStoreError::IllegalState {
                table: "wv_decisions",
                id: decision_id,
                attempted: "approve_harvest",
            }));
        }
        Some((status, artifact)) => {
            if artifact.is_none() {
                return Err(HarvestRefusal::NoDraftedArtifact.into());
            }
            if status == "approved" {
                return Err(HarvestRefusal::AlreadyApproved.into());
            }
        }
    }
    let changed = tx.execute(
        "UPDATE wv_decisions SET harvest_status='approved' \
         WHERE id=?1 AND harvest_status <> 'approved'",
        params![decision_id],
    )?;
    if changed != 1 {
        return Err(WvWriteError::Store(WeftStoreError::IllegalState {
            table: "wv_decisions",
            id: decision_id,
            attempted: "approve_harvest",
        }));
    }
    write_audit(tx, "wv_decisions", "harvest_approve", decision_id)?;
    Ok(())
}

/// SW-WV-C6 (AC-3): parse the decision id from a `harvest:<id>` approval `source`
/// pointer. Returns `None` for any non-harvest source; `Some(Err)` for a `harvest:`
/// prefix with a non-integer suffix (a corrupt pointer — the caller logs and refuses,
/// never advances a decision row). Kept pure so the parse contract is unit-tested.
fn parse_harvest_source(source: &str) -> Option<Result<i64, ()>> {
    source
        .strip_prefix("harvest:")
        .map(|suffix| suffix.parse::<i64>().map_err(|_| ()))
}

/// SW-WV-C6 (AC-5): flag a decision self-contained — set `harvest_status='not-required'`.
/// - `harvest_status='approved'` → `Err(AlreadyApproved)` (terminal, unchanged).
/// - `harvest_status='not-required'` → `Ok(())` (idempotent no-op).
/// - Otherwise → set `'not-required'`; a lost CAS (`changes()==0`) is a typed
///   `IllegalState`.
///
/// Audit row co-committed in the same `IMMEDIATE` transaction (§1.6).
pub fn wv_mark_harvest_not_required(store: &WeftStore, id: i64) -> WvWriteResult<()> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    match read_harvest_state(&tx, id)? {
        None => {
            return Err(WvWriteError::Store(WeftStoreError::IllegalState {
                table: "wv_decisions",
                id,
                attempted: "mark_harvest_not_required",
            }));
        }
        Some((status, _)) => {
            if status == "approved" {
                return Err(HarvestRefusal::AlreadyApproved.into());
            }
            if status == "not-required" {
                return Ok(());
            }
        }
    }
    let changed = tx.execute(
        "UPDATE wv_decisions SET harvest_status='not-required' \
         WHERE id=?1 AND harvest_status <> 'approved'",
        params![id],
    )?;
    if changed != 1 {
        return Err(WvWriteError::Store(WeftStoreError::IllegalState {
            table: "wv_decisions",
            id,
            attempted: "mark_harvest_not_required",
        }));
    }
    write_audit(&tx, "wv_decisions", "harvest_not_required", id)?;
    tx.commit()?;
    Ok(())
}

/// SW-WV-C6 (AC-4): one overdue-harvest row — a `wv_decisions` row still `pending`
/// past the 24h window. The controller automation reads these to draft the missing
/// artifact (or `ask_user` a one-liner). The FE never reads this shape directly; the
/// caller writes a `needs-you` `wv_attention` row per result (that write is the
/// caller's, not the query's — the query stays side-effect free per AC-4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverdueHarvest {
    pub id: i64,
    pub date: String,
    pub decision: String,
    pub constrains: String,
    pub ends_when: String,
    pub owner: String,
    pub source: Option<String>,
}

/// SW-WV-C6 (AC-4): every active decision still `pending` whose `date` is older than
/// `now_unix - 86400` (24h). A pure read — no side effects. Excludes `drafted`,
/// `approved`, and `not-required` rows (all four non-pending values are excluded by
/// the `harvest_status='pending'` predicate), and any `status<>'active'` decision.
/// The `date` column is ISO-8601 text (B3's schema), so the window is computed with
/// `CAST(strftime('%s', date) AS INTEGER)`.
pub fn wv_list_overdue_harvests(
    store: &WeftStore,
    now_unix: i64,
) -> WvWriteResult<Vec<OverdueHarvest>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, date, decision, constrains, ends_when, owner, source \
         FROM wv_decisions \
         WHERE harvest_status = 'pending' \
           AND CAST(strftime('%s', date) AS INTEGER) < ?1 - 86400 \
           AND status = 'active' \
         ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![now_unix], |r| {
            Ok(OverdueHarvest {
                id: r.get(0)?,
                date: r.get(1)?,
                decision: r.get(2)?,
                constrains: r.get(3)?,
                ends_when: r.get(4)?,
                owner: r.get(5)?,
                source: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------- wv_roster (AC-2) ----------

/// A durable roster row — the FE reads this shape via `wv_list_roster` (Task 7).
/// `name` is the natural PK. `session_id` and `run_id` are the live-session
/// correlation columns (SW-WA-A5): the watchdog / roster-flip / done-record path
/// keys on `session_id` (the live PTY session), and `run_id` carries the durable
/// run key so a completion can correlate the live row to its run-store record. A
/// roster row without a live session simply carries `None` for both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WvRosterRow {
    pub name: String,
    pub workspace: Option<String>,
    pub pane: Option<String>,
    pub kind: Option<String>,
    pub subclass: Option<String>,
    pub lifecycle: String,
    pub owned: Option<String>,
    pub orch_owner: Option<String>,
    pub relation: String,
    pub parent: Option<String>,
    pub current_task: Option<String>,
    pub cwd: Option<String>,
    pub status: Option<String>,
    /// The live PTY session id this row is bound to (`None` for a row without a
    /// live worker). The watchdog / roster-flip / done-record path resolve by it.
    pub session_id: Option<String>,
    /// The durable run key correlating the live session to its run-store record
    /// (`None` when the worker reported no run).
    pub run_id: Option<String>,
}

/// Validate a `relation` value against the two allowed forms: `"peer"` or
/// `"child:<scope>"` with a non-empty `<scope>` (AC-2). `<scope>` is the scope
/// name, not a foreign key (scopes land in SW-WV-B5).
///
/// SW-WV-A5 re-exports this as the public `validate_roster_relation` lint so the lint
/// and B3's write-time enforcement can never drift — one validator, two callers.
pub(crate) fn validate_relation(relation: &str) -> WvWriteResult<()> {
    if relation == "peer" {
        return Ok(());
    }
    if let Some(scope) = relation.strip_prefix("child:") {
        if !scope.trim().is_empty() {
            return Ok(());
        }
    }
    Err(reject(
        "relation must be \"peer\" or \"child:<scope>\" (non-empty scope)",
    ))
}

/// Upsert a roster row on the `name` PK (AC-2), idempotent — a second upsert on the
/// same name replaces the row's fields, never inserts a duplicate. `relation` is
/// validated against the two allowed forms at write time. Audit co-committed (AC-9).
#[allow(clippy::too_many_arguments)]
pub fn wv_upsert_roster(store: &WeftStore, row: &WvRosterRow) -> WvWriteResult<()> {
    validate_relation(&row.relation)?;
    if is_blank(&row.name) {
        return Err(reject("roster name (PK) must not be blank"));
    }
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO wv_roster \
         (name, workspace, pane, kind, subclass, lifecycle, owned, orch_owner, \
          relation, parent, current_task, cwd, status, session_id, run_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) \
         ON CONFLICT(name) DO UPDATE SET \
          workspace=excluded.workspace, pane=excluded.pane, kind=excluded.kind, \
          subclass=excluded.subclass, lifecycle=excluded.lifecycle, owned=excluded.owned, \
          orch_owner=excluded.orch_owner, relation=excluded.relation, parent=excluded.parent, \
          current_task=excluded.current_task, cwd=excluded.cwd, status=excluded.status, \
          session_id=excluded.session_id, run_id=excluded.run_id",
        params![
            row.name,
            row.workspace,
            row.pane,
            row.kind,
            row.subclass,
            row.lifecycle,
            row.owned,
            row.orch_owner,
            row.relation,
            row.parent,
            row.current_task,
            row.cwd,
            row.status,
            row.session_id,
            row.run_id,
        ],
    )?;
    // Roster PK is a name, not an integer id; the audit row keys on a stable hash
    // of the name so the log stays queryable without an integer surrogate.
    write_audit_named(&tx, "wv_roster", "upsert", &row.name)?;
    tx.commit()?;
    Ok(())
}

/// Deprovision (remove) a roster row by name (AC-2). Refuses a `peer` row — a peer
/// is observe-only and never managed by this controller. Returns `false` when no
/// row matched (a no-op), `true` when a child row was removed. Audit co-committed.
pub fn wv_deprovision_roster(store: &WeftStore, name: &str) -> WvWriteResult<bool> {
    let conn = store.conn()?;
    let relation: Option<String> = conn
        .query_row(
            "SELECT relation FROM wv_roster WHERE name=?1",
            params![name],
            |r| r.get(0),
        )
        .ok();
    let Some(relation) = relation else {
        return Ok(false); // no such row → no-op
    };
    if relation == "peer" {
        return Err(reject(
            "cannot deprovision a peer controller (a peer is observe-only, never managed here)",
        ));
    }
    drop(conn);
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute("DELETE FROM wv_roster WHERE name=?1", params![name])?;
    if changed == 1 {
        write_audit_named(&tx, "wv_roster", "deprovision", name)?;
        tx.commit()?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// List every roster row (AC-2 / Task 7 read path). Read-only — the FE's
/// `wv_list_roster` command projects this into `AgentSessionRow[]`.
pub fn wv_list_roster(store: &WeftStore) -> WvWriteResult<Vec<WvRosterRow>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare(
        "SELECT name, workspace, pane, kind, subclass, lifecycle, owned, orch_owner, \
                relation, parent, current_task, cwd, status, session_id, run_id \
         FROM wv_roster ORDER BY name",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(WvRosterRow {
                name: r.get(0)?,
                workspace: r.get(1)?,
                pane: r.get(2)?,
                kind: r.get(3)?,
                subclass: r.get(4)?,
                lifecycle: r.get(5)?,
                owned: r.get(6)?,
                orch_owner: r.get(7)?,
                relation: r.get(8)?,
                parent: r.get(9)?,
                current_task: r.get(10)?,
                cwd: r.get(11)?,
                status: r.get(12)?,
                session_id: r.get(13)?,
                run_id: r.get(14)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// SW-WV-F3: the compact roster projection the fleet-view row reads for ONE live
/// worker, keyed by its live PTY `session_id` (the column A2's dispatch transition
/// stamps and the A3/A4 CAS paths key on — NOT `pane`, which the pre-DW-B3-3 spec
/// draft named before the `session_id` column folded in). A non-Weft session (no
/// matching roster row) reads back `Ok(None)` — graceful, never an error, so the
/// panel never surfaces a query miss for a plain shell.
pub fn wv_roster_view_for_session(
    store: &WeftStore,
    session_id: &str,
) -> WvWriteResult<Option<WvRosterView>> {
    if is_blank(session_id) {
        return Ok(None);
    }
    let conn = store.conn()?;
    let row = conn
        .query_row(
            "SELECT subclass, kind, current_task, status \
             FROM wv_roster WHERE session_id=?1",
            params![session_id],
            |r| {
                Ok(WvRosterView {
                    subclass: r.get(0)?,
                    kind: r.get(1)?,
                    current_task: r.get(2)?,
                    status: r.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// SW-WV-F3: the four display fields the fleet-view row projects from a roster row —
/// a keyed lookup kept OFF the hot `dashboard_list_sessions` enumerate-everything
/// snapshot (a Weft-only widening of that contract is the wrong integration point).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WvRosterView {
    pub subclass: Option<String>,
    pub kind: Option<String>,
    pub current_task: Option<String>,
    pub status: Option<String>,
}

// ---------- wv_roster spawn path (SW-WV-A2) ----------
//
// The typed-ephemeral-worker spawn writes the roster row in two guarded steps —
// a `provision` insert BEFORE the PTY is live, then a `provision → dispatch`
// transition AFTER `launch_story_session` returns a handle — and deletes the row
// (no audit) on a spawn failure. Every mutation co-commits its `wv_write_log`
// audit row in the SAME IMMEDIATE transaction (AUDIT-ATOMIC, §1.6); the dispatch
// transition carries a `WHERE name=? AND status='provision'` guard + `changes()==1`
// (ONE-WRITER, §1.2). These live HERE (the sanctioned writer file) so the raw
// `wv_roster` SQL and the shared audit helpers stay in one place; `weft/spawn.rs`
// holds the orchestration logic and calls through these handlers.

/// The status of a spawn-path roster row, as the idempotency guard reads it back.
/// A row in `provision`/`dispatch`/`signal` is "live" — a second spawn for the
/// same `name` (== `story_id`) returns `AlreadyLive`; `handoff`/`deprovision` are
/// terminal (the worker is gone) so a fresh spawn is allowed (AC-6).
pub const WV_WORKER_LIVE_STATES: &[&str] = &["provision", "dispatch", "signal"];

/// A live worker roster row as the spawn idempotency guard needs it (AC-6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WvLiveWorker {
    pub name: String,
    pub status: Option<String>,
    pub session_id: Option<String>,
}

/// Look up an existing LIVE worker row by `name` (== `story_id`) for the spawn
/// idempotency guard (AC-6). A row is live when its `status` is one of
/// [`WV_WORKER_LIVE_STATES`]; a `handoff`/`deprovision`/absent row returns `None`
/// so a re-spawn after exit is allowed. Read-only.
pub fn wv_find_live_worker(store: &WeftStore, name: &str) -> WvWriteResult<Option<WvLiveWorker>> {
    let conn = store.conn()?;
    let row = conn
        .query_row(
            "SELECT name, status, session_id FROM wv_roster \
             WHERE name=?1 AND status IN ('provision','dispatch','signal')",
            params![name],
            |r| {
                Ok(WvLiveWorker {
                    name: r.get(0)?,
                    status: r.get(1)?,
                    session_id: r.get(2)?,
                })
            },
        )
        .ok();
    Ok(row)
}

/// Count the LIVE child workers under one scope — the roster spare-slot read the
/// concurrency cap consults before an auto-dispatch. "Live" is the same
/// [`WV_WORKER_LIVE_STATES`] set the single-name idempotency guard uses
/// (`provision`/`dispatch`/`signal`); `handoff`/`deprovision`/`needs-attention` rows are
/// terminal or parked and never counted. Scoped by `relation='child:<scope>'` so a
/// `peer` (observe-only) controller row is excluded. Read-only — no audit.
pub fn wv_count_live_workers(store: &WeftStore, scope: &str) -> WvWriteResult<i64> {
    let relation = format!("child:{scope}");
    let conn = store.conn()?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM wv_roster \
         WHERE relation=?1 AND status IN ('provision','dispatch','signal')",
        params![relation],
        |r| r.get(0),
    )?;
    Ok(count)
}

/// Insert a worker roster row in `status='provision'` BEFORE the PTY is live
/// (AC-3), so the controller can read it synchronously. `name` is the PK and is
/// set to `story_id`; `subclass`/`scope`/`story_id`/`created_at`/`updated_at` are
/// A2-owned columns. Rejects a blank `name`. The insert co-commits its audit row
/// in one IMMEDIATE transaction (AUDIT-ATOMIC). Uses a fixed `relation='child:<scope>'`
/// (a spawned worker is a managed child, never an observe-only peer) via the
/// form B3 validates.
///
/// A row for the same `name` may already exist ONLY in a terminal state
/// (`handoff`/`deprovision`) — a prior worker for the same story that has since
/// exited (the caller's idempotency guard already refused a LIVE row, AC-6). Such a
/// terminal row is overwritten (`ON CONFLICT(name) DO UPDATE`), resetting the row
/// to a fresh `provision`; a fresh session for an exited story is the intended path.
pub fn wv_provision_worker_row(
    store: &WeftStore,
    name: &str,
    subclass: &str,
    scope: &str,
) -> WvWriteResult<()> {
    if is_blank(name) {
        return Err(reject(
            "worker name (== story_id, the roster PK) must not be blank",
        ));
    }
    let now = now_iso();
    let relation = format!("child:{scope}");
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO wv_roster \
         (name, story_id, subclass, scope, status, relation, lifecycle, session_id, created_at, updated_at) \
         VALUES (?1, ?1, ?2, ?3, 'provision', ?4, 'task', NULL, ?5, ?5) \
         ON CONFLICT(name) DO UPDATE SET \
          story_id=excluded.story_id, subclass=excluded.subclass, scope=excluded.scope, \
          status='provision', relation=excluded.relation, lifecycle=excluded.lifecycle, \
          session_id=NULL, created_at=excluded.created_at, updated_at=excluded.updated_at",
        params![name, subclass, scope, relation, now],
    )?;
    write_audit_named(&tx, "wv_roster", "provision", name)?;
    tx.commit()?;
    Ok(())
}

/// Transition a worker roster row `provision → dispatch` AFTER the PTY is live
/// (AC-3), stamping the resolved `session_id` and (SW-WA-A5) the live `run_id`.
/// `run_id` is `None` when the caller has no run correlation (the durable row then
/// carries NULL and the done-record path falls back to `story_id`). Carries the
/// ONE-WRITER guard `WHERE name=? AND status='provision'` + `changes()==1` (§1.2):
/// a row already past `provision` (or absent) is a CAS loss → [`MutateResult::NotOwner`],
/// not a silent no-op. The UPDATE co-commits its audit row (AUDIT-ATOMIC).
pub fn wv_dispatch_worker_row(
    store: &WeftStore,
    name: &str,
    session_id: &str,
    run_id: Option<&str>,
) -> WvWriteResult<MutateResult> {
    let now = now_iso();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_roster SET session_id=?1, run_id=?2, status='dispatch', updated_at=?3 \
         WHERE name=?4 AND status='provision'",
        params![session_id, run_id, now, name],
    )?;
    if changed == 1 {
        write_audit_named(&tx, "wv_roster", "dispatch", name)?;
        tx.commit()?;
        Ok(MutateResult::Updated)
    } else {
        // No provision row to advance — the caller lost the CAS (already dispatched,
        // or the row was deleted). Roll back; no audit row.
        Ok(MutateResult::NotOwner)
    }
}

/// Delete a `provision` roster row on a spawn failure (AC-3). NO audit row is
/// written for an aborted provision — the row never became a real worker, so the
/// ledger carries no dispatch/deprovision trace for it (the story's explicit
/// "no audit log entry for aborted provision"). Guarded to `status='provision'` so
/// a concurrently-dispatched row is never clobbered. Returns `true` when a
/// provision row was removed.
pub fn wv_delete_provision_row(store: &WeftStore, name: &str) -> WvWriteResult<bool> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "DELETE FROM wv_roster WHERE name=?1 AND status='provision'",
        params![name],
    )?;
    tx.commit()?;
    Ok(changed == 1)
}

// ---------- wv_roster lifecycle transitions (SW-WV-A3) ----------
//
// A3 canonicalises A2's ad-hoc `wv_roster.status` string column into a typed,
// table-driven state machine ([`super::worker_lifecycle`]) and adds the two
// transitions A2 left for this story: `signal` (from `signal_done`) and the two
// terminal steps `handoff` + `deprovision`. It does NOT add a second
// `lifecycle_state` column — that would be the shadow-state divergence Task 7
// forbids. Each transition is guarded twice: [`check_transition`] refuses an
// illegal step before the SQL, and the CAS `WHERE status=<expected>` +
// `changes()==1` guard makes exactly one concurrent writer win (ONE-WRITER §1.2).
// Every mutation co-commits its `wv_write_log` audit row in the SAME IMMEDIATE
// transaction (AUDIT-ATOMIC §1.6). All handlers are synchronous SQLite — callers
// run them on the blocking pool (§1.6).

use super::worker_lifecycle::{
    check_transition, ClosureCondition, LifecycleError, WorkerLifecycleState,
};

/// The `wv_roster.lifecycle` value marking an offered worker — one that requires an
/// explicit `force` override before it can be deprovisioned (AC-3, ARCHITECTURE
/// offered-worker exception). A2's default lifecycle is `'task'`.
pub const WV_ROSTER_OFFERED_LIFECYCLE: &str = "offered";

impl From<LifecycleError> for WvWriteError {
    fn from(e: LifecycleError) -> Self {
        WvWriteError::Invalid(WvValidation(e.to_string()))
    }
}

/// Advance a worker `dispatch → signal` (AC-1/AC-2) — the transition the
/// `signal_done` roster-update hook drives. Refuses an illegal predecessor via
/// [`check_transition`] BEFORE the SQL, then applies the CAS
/// `WHERE name=? AND status='dispatch'` + `changes()==1` guard so a concurrent
/// second transition loses and surfaces [`LifecycleError::IllegalTransition`]
/// (one winner, no silent double-transition). Audit co-committed (§1.6).
pub fn wv_worker_signal(store: &WeftStore, name: &str) -> WvWriteResult<()> {
    transition_worker(
        store,
        name,
        WorkerLifecycleState::Dispatched,
        WorkerLifecycleState::Signalled,
        "signal",
    )
}

/// Advance a worker `signal → handoff` (AC-1/AC-2) — the optional handoff-doc step
/// before deprovision. Same double-guard + audit-atomic contract as
/// [`wv_worker_signal`].
pub fn wv_worker_handoff(store: &WeftStore, name: &str) -> WvWriteResult<()> {
    transition_worker(
        store,
        name,
        WorkerLifecycleState::Signalled,
        WorkerLifecycleState::HandoffWritten,
        "handoff",
    )
}

/// Shared body for the simple single-predecessor lifecycle transitions
/// (`signal`, `handoff`). Validates `expected → target` against the transition
/// table, then runs the guarded UPDATE + audit in one IMMEDIATE transaction. A
/// `changes()==0` CAS loss (the row was not in `expected`) surfaces
/// [`LifecycleError::IllegalTransition`] — the same typed refusal the pure guard
/// raises, so a concurrent winner and a stale caller are indistinguishable to the
/// loser (AC-2).
fn transition_worker(
    store: &WeftStore,
    name: &str,
    expected: WorkerLifecycleState,
    target: WorkerLifecycleState,
    audit_op: &'static str,
) -> WvWriteResult<()> {
    check_transition(expected, target)?;
    if is_blank(name) {
        return Err(reject("worker name (roster PK) must not be blank"));
    }
    let now = now_iso();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_roster SET status=?1, updated_at=?2 WHERE name=?3 AND status=?4",
        params![target.as_wire(), now, name, expected.as_wire()],
    )?;
    if changed == 1 {
        write_audit_named(&tx, "wv_roster", audit_op, name)?;
        tx.commit()?;
        Ok(())
    } else {
        // CAS loss: the row was not in `expected` (already advanced, absent, or a
        // concurrent winner took it). Surface the same typed illegal-transition
        // refusal — never a silent no-op (AC-1/AC-2).
        Err(LifecycleError::IllegalTransition {
            from: expected.as_wire(),
            to: target.as_wire(),
        }
        .into())
    }
}

/// Advance the worker whose live `session_id` matches, `dispatch → signal`
/// (AC-4) — the transition the `signal_done` roster-update hook drives. The hook
/// only has the completing session's id, not the roster `name` (== story_id), so it
/// resolves the row by `session_id` here. Returns `Ok(false)` when no `dispatch`
/// row carries that session (a non-worker completion, or a worker already advanced)
/// — a graceful no-op so the hook never fails a normal `signal_done`, and
/// `Ok(true)` when the transition landed. The CAS `WHERE session_id=? AND
/// status='dispatch'` + `changes()==1` guard keeps one winner; audit co-committed.
pub fn wv_worker_signal_by_session(store: &WeftStore, session_id: &str) -> WvWriteResult<bool> {
    if is_blank(session_id) {
        return Ok(false);
    }
    // Find the roster name for this live dispatched session (for the audit key).
    let conn = store.conn()?;
    let name: Option<String> = conn
        .query_row(
            "SELECT name FROM wv_roster WHERE session_id=?1 AND status='dispatch'",
            params![session_id],
            |r| r.get(0),
        )
        .ok();
    drop(conn);
    let Some(name) = name else {
        return Ok(false); // no dispatched worker for this session — graceful no-op
    };
    let now = now_iso();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_roster SET status='signal', updated_at=?1 \
         WHERE session_id=?2 AND status='dispatch'",
        params![now, session_id],
    )?;
    if changed == 1 {
        write_audit_named(&tx, "wv_roster", "signal", &name)?;
        tx.commit()?;
        Ok(true)
    } else {
        // A concurrent transition won between the read and the CAS — graceful no-op
        // (the completion path never fails on a lost roster race).
        Ok(false)
    }
}

/// The `wv_roster.status` wire value the silence watchdog stamps when a dispatched
/// worker goes quiet past its TTL without a `signal_done` (SW-WV-A4, AC-5). It is
/// deliberately NOT one of the five lifecycle states ([`super::worker_lifecycle`]) —
/// silence is an out-of-band attention flag the runtime raises about a worker, not a
/// step the worker itself drove — so it never routes through [`transition_worker`].
/// The FE maps it onto the orange "Needs attention" chip (`deriveAgentStatus`).
pub const WV_ROSTER_NEEDS_ATTENTION_STATUS: &str = "needs-attention";

/// Flip the worker whose live `session_id` matches `dispatch → needs-attention`
/// (SW-WV-A4, AC-5) — the write the silence watchdog drives when the worker went
/// quiet before signalling. The CAS `WHERE session_id=? AND status='dispatch'` +
/// `changes()==1` guard is the load-bearing race belt: a worker that DID call
/// `signal_done` between the watchdog's TTL elapsing and this write already advanced
/// past `dispatch`, so the CAS matches nothing and no `needs-attention` is stamped
/// (`Ok(None)`) — silence is never confused with a confident finish (AC-6). On a win
/// the roster `name` is returned so the caller can key the paired `wv_attention` row.
/// Audit co-committed in the same IMMEDIATE transaction (§1.6).
pub fn wv_worker_needs_attention_by_session(
    store: &WeftStore,
    session_id: &str,
) -> WvWriteResult<Option<String>> {
    if is_blank(session_id) {
        return Ok(None);
    }
    // Resolve the still-dispatched worker's roster name (for the audit key + the
    // caller's attention-row ref). A row already past `dispatch` reads back nothing.
    let conn = store.conn()?;
    let name: Option<String> = conn
        .query_row(
            "SELECT name FROM wv_roster WHERE session_id=?1 AND status='dispatch'",
            params![session_id],
            |r| r.get(0),
        )
        .ok();
    drop(conn);
    let Some(name) = name else {
        return Ok(None); // no dispatched worker for this session — the worker finished
    };
    let now = now_iso();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_roster SET status=?1, updated_at=?2 \
         WHERE session_id=?3 AND status='dispatch'",
        params![WV_ROSTER_NEEDS_ATTENTION_STATUS, now, session_id],
    )?;
    if changed == 1 {
        write_audit_named(&tx, "wv_roster", "needs-attention", &name)?;
        tx.commit()?;
        Ok(Some(name))
    } else {
        // The worker advanced (signalled) between the read and the CAS — never
        // clobber a completed worker with a silence flag (AC-6).
        Ok(None)
    }
}

/// The operator-supplied inputs to a `deprovision` closure check (AC-3).
#[derive(Debug, Clone, Copy)]
pub struct DeprovisionInputs {
    /// The operator confirms the worker's packet was delivered to its canonical
    /// home (closure condition 2). Deprovision is refused when `false`.
    pub packet_delivered: bool,
    /// Explicit override to close an `lifecycle='offered'` worker (AC-3). Without
    /// it an offered worker returns [`LifecycleError::OfferedWorkerProtected`].
    pub force: bool,
}

/// The `wv_roster` columns [`wv_worker_deprovision`] reads back to run the
/// four-condition closure check (AC-3). Private read-model.
struct DeprovisionRowState {
    status: Option<String>,
    lifecycle: Option<String>,
    scope: Option<String>,
    session_id: Option<String>,
}

/// The successful outcome of [`wv_worker_deprovision`] (AC-6). `liveness_warned`
/// is `true` when the injected `is_idle` probe reported the worker's PTY still
/// alive at teardown — a `log::warn!` was emitted but deprovision still succeeded.
/// Surfaced as a side-channel so a test asserts the warning deterministically
/// without a log-capture crate (`tracing` is not a direct dependency here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeprovisionOutcome {
    pub liveness_warned: bool,
}

/// Deprovision a worker (`signal`/`handoff` → `deprovision`) after enforcing the
/// four-condition closure check (AC-3), then run the injectable liveness probe as a
/// non-blocking WARNING (AC-6). Returns `Ok(())` on success.
///
/// Closure conditions (each unmet → a typed [`LifecycleError::ClosureCheckFailed`]
/// naming the condition):
/// 1. **Signal received** — current state is `signal` or `handoff` (so the pure
///    transition guard admits `→ deprovision`).
/// 2. **Packet delivered** — `inputs.packet_delivered` is `true`.
/// 3. **No open follow-ups** — no `wv_queue` row in this worker's `scope` is in a
///    non-`resolved` state.
/// 4. **Not offered** — `lifecycle <> 'offered'`, OR `inputs.force` is set
///    (an offered worker without `force` returns
///    [`LifecycleError::OfferedWorkerProtected`]).
///
/// `is_idle` is an injectable `Fn(&str) -> bool` liveness probe (AC-6): the core is
/// probe-agnostic so a test injects a stub without a live PTY. When the worker's
/// `session_id` is present and the probe reports NOT idle (process still alive), a
/// `log::warn!` is emitted and [`DeprovisionOutcome::liveness_warned`] is set —
/// deprovision still returns `Ok` (the warning never blocks). The Tauri boundary
/// wires `session_is_idle_inner` as the real probe.
///
/// The state transition + audit co-commit in one IMMEDIATE transaction
/// (AUDIT-ATOMIC §1.6); the CAS `WHERE status IN ('signal','handoff')` +
/// `changes()==1` guard makes one concurrent deprovision win.
pub fn wv_worker_deprovision<F>(
    store: &WeftStore,
    name: &str,
    inputs: DeprovisionInputs,
    is_idle: F,
) -> WvWriteResult<DeprovisionOutcome>
where
    F: Fn(&str) -> bool,
{
    if is_blank(name) {
        return Err(reject("worker name (roster PK) must not be blank"));
    }
    let conn = store.conn()?;

    // Read the row's current state, lifecycle, scope, session for the closure gates.
    let row: Option<DeprovisionRowState> = conn
        .query_row(
            "SELECT status, lifecycle, scope, session_id FROM wv_roster WHERE name=?1",
            params![name],
            |r| {
                Ok(DeprovisionRowState {
                    status: r.get(0)?,
                    lifecycle: r.get(1)?,
                    scope: r.get(2)?,
                    session_id: r.get(3)?,
                })
            },
        )
        .ok();
    let Some(DeprovisionRowState {
        status,
        lifecycle,
        scope,
        session_id,
    }) = row
    else {
        // No such worker → treat as an illegal transition (nothing to close).
        return Err(LifecycleError::IllegalTransition {
            from: "absent",
            to: "deprovision",
        }
        .into());
    };

    // Condition 4 — offered-worker protection (AC-3). Checked first so an offered
    // worker gives the distinct `OfferedWorkerProtected` refusal.
    let is_offered = lifecycle.as_deref() == Some(WV_ROSTER_OFFERED_LIFECYCLE);
    if is_offered && !inputs.force {
        return Err(LifecycleError::OfferedWorkerProtected.into());
    }

    // Condition 1 — signal received (state is `signal` or `handoff`). The pure
    // transition table already encodes "only signalled/handoff → deprovision"; use
    // it as the gate so the closure message names the condition.
    let current = status.as_deref().and_then(WorkerLifecycleState::from_wire);
    let signal_received = matches!(
        current,
        Some(WorkerLifecycleState::Signalled) | Some(WorkerLifecycleState::HandoffWritten)
    );
    if !signal_received {
        return Err(LifecycleError::ClosureCheckFailed {
            condition: ClosureCondition::SignalReceived,
        }
        .into());
    }

    // Condition 2 — packet delivered (operator-confirmed).
    if !inputs.packet_delivered {
        return Err(LifecycleError::ClosureCheckFailed {
            condition: ClosureCondition::PacketDelivered,
        }
        .into());
    }

    // Condition 3 — no open follow-ups: no non-resolved `wv_queue` row in scope.
    if let Some(scope) = scope.as_deref().filter(|s| !s.is_empty()) {
        let open: i64 = conn.query_row(
            "SELECT COUNT(*) FROM wv_queue WHERE scope=?1 AND state<>'resolved'",
            params![scope],
            |r| r.get(0),
        )?;
        if open > 0 {
            return Err(LifecycleError::ClosureCheckFailed {
                condition: ClosureCondition::NoOpenFollowups,
            }
            .into());
        }
    }
    drop(conn);

    // All gates passed — apply the terminal transition. The `from` for the audit /
    // transition is the concrete current state (`signal` or `handoff`).
    let from = current.expect("signal_received implies a parseable current state");
    check_transition(from, WorkerLifecycleState::Deprovisioned)?;
    let now = now_iso();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_roster SET status='deprovision', updated_at=?1 \
         WHERE name=?2 AND status IN ('signal','handoff')",
        params![now, name],
    )?;
    if changed != 1 {
        // A concurrent deprovision (or a state change) won between the read and the
        // CAS — surface the typed refusal, never a silent no-op (AC-2).
        return Err(LifecycleError::IllegalTransition {
            from: from.as_wire(),
            to: "deprovision",
        }
        .into());
    }
    write_audit_named(&tx, "wv_roster", "deprovision", name)?;
    tx.commit()?;

    // AC-6 — liveness probe is a non-blocking WARNING (deprovision already
    // succeeded). Only meaningful when a live session is correlated. The
    // `liveness_warned` flag is the deterministic side-channel a test asserts on.
    let mut liveness_warned = false;
    if let Some(sid) = session_id.as_deref().filter(|s| !s.is_empty()) {
        if !is_idle(sid) {
            liveness_warned = true;
            log::warn!(
                target: "weft",
                "deprovision: worker {name} (session {sid}) PTY process still alive at teardown (not blocking)"
            );
        }
    }
    Ok(DeprovisionOutcome { liveness_warned })
}

// ---------- wv_approvals (AC-3, AC-7) ----------

/// The endorsement tier of an approval (AC-3). Immutable at insert (ARCHITECTURE
/// §4.1 — nothing writes a tier after insert).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalTier {
    /// Pre-authorized — inserts `disposition='ratified'` immediately, no window.
    T1,
    /// Lazy consent — inserts `disposition='pending'`; `window_state` carries the
    /// revocation window (`"open"` | `"paused"` | ISO expiry).
    T2,
    /// Explicit 👍 required — inserts `disposition='pending'` and surfaces the
    /// question via the injected [`ApprovalNotifier`]; the reply arrives through
    /// `wv_settle_approval`, never awaited here.
    T3,
}

impl ApprovalTier {
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "T1" => Some(ApprovalTier::T1),
            "T2" => Some(ApprovalTier::T2),
            "T3" => Some(ApprovalTier::T3),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalTier::T1 => "T1",
            ApprovalTier::T2 => "T2",
            ApprovalTier::T3 => "T3",
        }
    }
    /// The insert-time disposition per tier (ARCHITECTURE §4.1 — the tier-dispatch
    /// rule is visible in the schema/handler, not buried): T1 auto-ratifies; T2/T3
    /// insert pending.
    fn insert_disposition(self) -> &'static str {
        match self {
            ApprovalTier::T1 => "ratified",
            ApprovalTier::T2 | ApprovalTier::T3 => "pending",
        }
    }
}

/// The T3 surfacing seam (AC-3, AC-7). The real implementation is the
/// `AskUserRegistry` (register a correlation for the controller session + emit the
/// `mcp:ask-user` event); a test injects a stub that captures the `session_id`. The
/// call is NON-BLOCKING — it registers and emits, then returns; the reply is driven
/// by the UI via `wv_settle_approval`, never awaited inside `wv_write_approval`. So
/// `wv_write_approval` never calls `block_in_place` and is safe on a single-threaded
/// Tokio runtime (the AC-7 assertion).
pub trait ApprovalNotifier {
    /// Surface the approval question for `claim` on `session_id`. Returns a
    /// correlation id (the real registry mints it) so the caller can echo it.
    fn notify_t3(&self, session_id: &str, approval_id: i64, claim: &str) -> String;
}

/// A no-op [`ApprovalNotifier`] for the blocking-pool write half. The MCP verb
/// writes the row through the sanctioned handler on the blocking pool with this
/// notifier (so no borrowed `AppHandle`/registry has to cross the `spawn_blocking`
/// boundary), then performs the real T3 emit on the async side once the row is
/// durable. `notify_t3` here is never reached for a durable-then-emit caller
/// (they pass T1/T2 through, and re-emit T3 themselves), but returns an empty
/// correlation for completeness.
pub struct NullNotifier;
impl ApprovalNotifier for NullNotifier {
    fn notify_t3(&self, _session_id: &str, _approval_id: i64, _claim: &str) -> String {
        String::new()
    }
}

/// Insert an approval row with an immutable, PICKER-CHOSEN tier (SW-WV-E2, AC-3,
/// AC-5, AC-8).
///
/// The caller supplies the three risk signals (`irreversible`, `blast_radius`,
/// `confidence`) and a `preferred_tier`; [`super::tier_picker::pick_tier`] owns
/// the tier decision — the two-of-three rule forces T3 when the action is
/// high-stakes, otherwise the preferred tier stands (a pre-declared T3 is never
/// downgraded). The agent never sets the `tier` column directly; the picker's
/// result is stored (ARCHITECTURE §4.1 — an immutable tier written by
/// `pick_tier` at insert time).
///
/// The three risk signals are persisted to the `irreversible` / `blast_radius` /
/// `confidence` columns so the decision is auditable and re-classifiable, and the
/// picker-chosen tier's insert disposition follows the tier-dispatch rule (T1
/// auto-ratifies; T2/T3 land `pending` — spine §4.1).
///
/// For a picker-chosen T3 the injected `notifier` surfaces the question
/// (non-blocking); a chosen-T3 call REQUIRES a non-blank `session_id` (rejected
/// before the row is written). Returns `(id, chosen_tier, disposition)`. Audit
/// co-committed in the same IMMEDIATE transaction as the INSERT (AC-8, via
/// [`wv_auto_append`]).
#[allow(clippy::too_many_arguments)]
pub fn wv_write_approval(
    store: &WeftStore,
    preferred_tier: ApprovalTier,
    irreversible: bool,
    blast_radius: super::tier_picker::BlastRadius,
    confidence: super::tier_picker::Confidence,
    claim: &str,
    source: Option<&str>,
    window_state: Option<&str>,
    session_id: Option<&str>,
    notifier: &dyn ApprovalNotifier,
) -> WvWriteResult<(i64, ApprovalTier, String)> {
    if is_blank(claim) {
        return Err(reject("claim must not be blank"));
    }
    // The picker is the SOLE decider of the tier (never `preferred_tier`
    // directly). `reasons` is discarded here — the DB stores only the tier; the
    // MCP `wv_classify_action` verb returns the reasons.
    let (tier, _reasons) =
        super::tier_picker::pick_tier(irreversible, blast_radius, confidence, preferred_tier);
    // T3 routing contract: a missing/blank session_id is rejected at the Rust layer
    // BEFORE the row is written. This binds to the CHOSEN tier — an action the
    // picker forced to T3 needs routing just as much as a pre-declared T3.
    if tier == ApprovalTier::T3 && session_id.map(is_blank).unwrap_or(true) {
        return Err(reject("session_id is required for a T3 approval"));
    }
    let disposition = tier.insert_disposition();
    // T2 carries the caller's window; T1/T3 ignore it → the schema default 'open'.
    let window = if tier == ApprovalTier::T2 {
        window_state.unwrap_or("open")
    } else {
        "open"
    };
    // Persist the risk signals as their wire forms: `irreversible` as 0/1,
    // `blast_radius`/`confidence` as their lowercase strings (serde round-trips).
    let irreversible_int: i64 = i64::from(irreversible);
    let blast_str = blast_radius.as_wire();
    let confidence_str = confidence.as_wire();
    let mut conn = store.conn()?;
    let id = wv_auto_append(
        &mut conn,
        "INSERT INTO wv_approvals \
         (tier, claim, posted_at, source, window_state, disposition, \
          irreversible, blast_radius, confidence) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        &[
            &tier.as_str(),
            &claim,
            &now_iso(),
            &source,
            &window,
            &disposition,
            &irreversible_int,
            &blast_str,
            &confidence_str,
        ],
        "wv_approvals",
    )?;
    // T3: surface the question AFTER the row is durably written, so the UI reply
    // path (`wv_settle_approval`) always has a row to settle. Non-blocking.
    if tier == ApprovalTier::T3 {
        let sid = session_id.unwrap_or_default();
        notifier.notify_t3(sid, id, claim);
    }
    Ok((id, tier, disposition.to_string()))
}

/// Settle an approval's disposition to `ratified` or `revoked` (AC-3) — the reply
/// path for both T2 (window-expiry check) and T3 (owner 👍). Rejects any other
/// disposition value. Returns `false` when no pending approval matched (already
/// settled / unknown id), `true` when one was settled. Audit co-committed (AC-9).
pub fn wv_settle_approval(store: &WeftStore, id: i64, disposition: &str) -> WvWriteResult<bool> {
    if !WV_APPROVAL_DISPOSITIONS.contains(&disposition) {
        return Err(reject("disposition must be one of: ratified | revoked"));
    }
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Read the approval's source pointer BEFORE the settle so the harvest-close can
    // join this same IMMEDIATE tx (SW-WV-C6, AC-3): a `harvest:<id>` ratify settles
    // the approval AND flips the decision to `approved` atomically.
    let source: Option<String> = tx
        .query_row(
            "SELECT source FROM wv_approvals WHERE id=?1",
            params![id],
            |r| r.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten();
    // Guard on the current pending state so a double-settle is a no-op, not a
    // silent tier/disposition clobber.
    // tier is immutable after insert (SW-WV-E2 AC-5) — this settle touches ONLY
    // `disposition`, never the picker-chosen `tier` / risk-signal columns.
    let changed = tx.execute(
        "UPDATE wv_approvals SET disposition=?2 WHERE id=?1 AND disposition='pending'",
        params![id, disposition],
    )?;
    if changed != 1 {
        return Ok(false);
    }
    write_audit(&tx, "wv_approvals", "settle", id)?;
    // SW-WV-C6 (AC-3): close the harvest loop when a `harvest:<id>` approval is
    // ratified. A non-integer suffix is a corrupt pointer — refuse (rolling back the
    // whole tx, so no decision row advances and the approval settle is undone), never
    // advance a row. A non-harvest source is a normal approval settle (no-op here).
    if disposition == "ratified" {
        if let Some(src) = source.as_deref() {
            match parse_harvest_source(src) {
                Some(Ok(decision_id)) => {
                    approve_harvest_in_tx(&tx, decision_id)?;
                }
                Some(Err(())) => {
                    return Err(reject(format!(
                        "corrupt harvest approval source (non-integer decision id): {src}"
                    )));
                }
                None => {}
            }
        }
    }
    tx.commit()?;
    Ok(true)
}

/// SW-WV-F5 (AC-1, AC-3): one pending `wv_approvals` row the operator still owes a
/// decision on — the T2/T3 half of the YOU-OWE endorsement surface.
/// `not_surfaced_but_expired` is computed inline from the E3 window columns: a T2 whose
/// revocation budget is spent (effective present-time elapsed ≥ the granted duration)
/// yet is still `pending` did NOT auto-close — the surfaced-item guard held it back
/// because the background scan ran in a different session than the surfacing one — so it
/// carries to the next session (AC-3's "carries to next session" chip). A paused window
/// (owner away, clock frozen) or a not-yet-spent window is `false`, as is any T1/T3 row
/// (no revocation window).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WvApproval {
    pub id: i64,
    pub tier: String,
    pub claim: String,
    pub posted_at: String,
    pub source: Option<String>,
    pub window_state: String,
    pub disposition: String,
    /// The granted window budget in seconds (E3 `window_duration_s`); `None` until opened.
    pub duration_s: Option<i64>,
    /// Epoch-s the window was opened (E3 `window_opened_at`); `None` until opened.
    pub opened_at: Option<i64>,
    pub pause_accumulated_s: i64,
    pub surfaced_session: Option<String>,
    pub not_surfaced_but_expired: bool,
}

/// SW-WV-F5 (AC-1, AC-3): read every pending `wv_approvals` row — the T2/T3 rows the
/// YOU-OWE endorsement surface merges alongside pending assertions. `not_surfaced_but_expired`
/// is computed from the E3 window columns at `now`, reusing the SAME effective-elapsed
/// math ([`t2_effective_elapsed`]) the window verdict uses, so a "carries to next session"
/// chip can never disagree with the auto-close guard. Ordered by id (insert order) for a
/// stable list. Read-only.
pub fn wv_list_pending_approvals(store: &WeftStore, now: i64) -> WvWriteResult<Vec<WvApproval>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, tier, claim, posted_at, source, window_state, disposition, \
                window_duration_s, window_opened_at, pause_accumulated_s, surfaced_session \
         FROM wv_approvals WHERE disposition='pending' ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            let tier: String = r.get(1)?;
            let window_state: String = r.get(5)?;
            let duration_s: Option<i64> = r.get(7)?;
            let opened_at: Option<i64> = r.get(8)?;
            let pause_accumulated_s: i64 = r.get(9)?;
            let surfaced_session: Option<String> = r.get(10)?;
            // A still-pending T2 whose revocation budget is spent did NOT auto-close —
            // the surfaced-item guard held it back (the scan ran in a different session
            // than the surfacing one) — so it carries to the next session. The elapsed
            // math nets out banked pause; a paused window (state='paused') never counts
            // as carried (the clock is frozen — it neither runs down nor closes).
            let not_surfaced_but_expired = tier == "T2"
                && window_state != "paused"
                && matches!((opened_at, duration_s), (Some(o), Some(d))
                    if t2_effective_elapsed(now, o, None, pause_accumulated_s) >= d);
            Ok(WvApproval {
                id: r.get(0)?,
                tier,
                claim: r.get(2)?,
                posted_at: r.get(3)?,
                source: r.get(4)?,
                window_state,
                disposition: r.get(6)?,
                duration_s,
                opened_at,
                pause_accumulated_s,
                surfaced_session,
                not_surfaced_but_expired,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------- wv_approvals: the Tier-2 window (SW-WV-E3) ----------
//
// A T2 approval is *lazy consent*: pre-approved for a bounded interval during which
// the owner may still revoke. E3 makes that interval a first-class, durable window
// with two safety properties the roadmap demands:
//   1. Surfaced-item guard — the window may auto-close (ratify) ONLY when checked in
//      the same session the item was surfaced to the owner in. An action the owner
//      never saw can never lapse into consent.
//   2. Presence pause — the clock PAUSES while the owner is away (E7 posture) and
//      resumes when they return; away time is banked in `pause_accumulated_s` so the
//      owner always gets the full window of *present* time to react.
// The pause/resume columns (`paused_at`, `pause_accumulated_s`) are single-sited here
// (ARCHITECTURE §4.1): E7's away-enter CALLS `wv_pause_all_t2_windows` /
// `wv_resume_all_t2_windows`, it must never write those columns directly.

/// The outcome of a [`wv_check_t2_window`] evaluation — a pure verdict the caller
/// (the FE window ticker, or E7's posture check) branches on without re-reading the
/// row. `elapsed_s` is the effective present-time consumed (wall time minus banked +
/// in-flight pause); `remaining_s` saturates at 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum T2WindowVerdict {
    /// No live T2 window for this id (not T2, not opened, or already settled).
    NotOpen,
    /// The clock is paused (owner away) — the window neither runs down nor closes.
    Paused { elapsed_s: i64, remaining_s: i64 },
    /// Running, budget not yet spent — still revocable, not yet ratified.
    Open { elapsed_s: i64, remaining_s: i64 },
    /// The budget is spent but this call is in a DIFFERENT session than the one the
    /// item was surfaced in — the surfaced-item guard blocks the auto-close. The
    /// window stays `pending`; only a check in the surfacing session may ratify it.
    ExpiredWrongSession { elapsed_s: i64 },
    /// The budget is spent AND the check ran in the surfacing session — the caller
    /// SHOULD auto-ratify via [`wv_close_t2_window_if_lapsed`] (or the FE via
    /// `wv_settle_approval`). Pure verdict — this variant writes nothing.
    Lapsed { elapsed_s: i64 },
}

/// The effective present-time (seconds) a window has consumed at `now`, netting out
/// banked pause plus any in-flight pause. Pure — the single elapsed-math site both
/// the verdict and the auto-close reuse, so they can never disagree.
fn t2_effective_elapsed(
    now: i64,
    opened_at: i64,
    paused_at: Option<i64>,
    pause_accumulated_s: i64,
) -> i64 {
    let wall = now.saturating_sub(opened_at);
    let in_flight_pause = paused_at.map(|p| now.saturating_sub(p)).unwrap_or(0);
    (wall - pause_accumulated_s - in_flight_pause).max(0)
}

/// A snapshot of a T2 window's durable columns (the read half of every E3 op).
struct T2WindowRow {
    disposition: String,
    tier: String,
    opened_at: Option<i64>,
    duration_s: Option<i64>,
    surfaced_session: Option<String>,
    paused_at: Option<i64>,
    pause_accumulated_s: i64,
}

fn read_t2_window(conn: &rusqlite::Connection, id: i64) -> rusqlite::Result<Option<T2WindowRow>> {
    conn.query_row(
        "SELECT disposition, tier, window_opened_at, window_duration_s, \
                surfaced_session, paused_at, pause_accumulated_s \
         FROM wv_approvals WHERE id=?1",
        params![id],
        |r| {
            Ok(T2WindowRow {
                disposition: r.get(0)?,
                tier: r.get(1)?,
                opened_at: r.get(2)?,
                duration_s: r.get(3)?,
                surfaced_session: r.get(4)?,
                paused_at: r.get(5)?,
                pause_accumulated_s: r.get(6)?,
            })
        },
    )
    .optional()
}

/// Open the Tier-2 window on an existing `pending` T2 approval (SW-WV-E3): stamp the
/// session the item was surfaced to the owner in, the open time, and the granted
/// budget. This is what arms the surfaced-item guard — a window that was never opened
/// (no `surfaced_session`) can never lapse into consent.
///
/// Rejects a non-positive `duration_s`, a blank `surfaced_session`, or a row that is
/// not a `pending` T2 (a T1/T3 or already-settled row has no revocation window). The
/// guarded `UPDATE` carries `changes()==1`; a CAS loser gets a typed error, never a
/// silent no-op (ARCHITECTURE §1.2). Audit co-committed in the same IMMEDIATE tx
/// (§1.6). Returns the effective `window_expires_at` boundary is left to the reader —
/// this returns `true` when the window was armed.
pub fn wv_open_t2_window(
    store: &WeftStore,
    id: i64,
    surfaced_session: &str,
    duration_s: i64,
    now: i64,
) -> WvWriteResult<bool> {
    if is_blank(surfaced_session) {
        return Err(reject("surfaced_session must not be blank"));
    }
    if duration_s <= 0 {
        return Err(reject("duration_s must be positive"));
    }
    // Store the surfacing session in its canonical (trimmed) form so the later
    // surfaced-item comparison in `evaluate_t2_window` (which also trims the check
    // session) can never spuriously miss on surrounding whitespace.
    let surfaced_session = surfaced_session.trim();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Guard: only a pending T2 that has NOT already been opened has a fresh window
    // to arm. The WHERE binds tier + disposition + `window_opened_at IS NULL` so a
    // T1/T3/settled row is a typed refusal AND a re-open of a live window can never
    // clobber its opened-at / duration / banked-pause (which would silently reset
    // the owner's window). Opening is a one-time arm.
    let changed = tx.execute(
        "UPDATE wv_approvals SET \
           window_opened_at=?2, window_duration_s=?3, surfaced_session=?4, \
           window_state='open', paused_at=NULL, pause_accumulated_s=0 \
         WHERE id=?1 AND tier='T2' AND disposition='pending' \
           AND window_opened_at IS NULL",
        params![id, now, duration_s, surfaced_session],
    )?;
    if changed != 1 {
        return Err(reject(
            "no un-opened pending T2 approval for the given id (already opened/settled, or not a T2)",
        ));
    }
    write_audit(&tx, "wv_approvals", "open_window", id)?;
    tx.commit()?;
    Ok(true)
}

/// Evaluate a Tier-2 window at `now` WITHOUT mutating it (SW-WV-E3) — the pure
/// verdict the FE ticker and E7's posture check branch on. Applies BOTH E3 guards:
/// the clock is frozen while `paused_at` is set (presence pause), and a spent budget
/// only reports `Lapsed` when `check_session` matches the session the item was
/// surfaced in (surfaced-item guard); a spent budget in any other session reports
/// `ExpiredWrongSession` so the window stays `pending`.
pub fn wv_check_t2_window(
    store: &WeftStore,
    id: i64,
    check_session: &str,
    now: i64,
) -> WvWriteResult<T2WindowVerdict> {
    let conn = store.conn()?;
    let Some(row) = read_t2_window(&conn, id)? else {
        return Ok(T2WindowVerdict::NotOpen);
    };
    Ok(evaluate_t2_window(&row, check_session, now))
}

/// Pure window verdict (the single decision site `wv_check_t2_window` and
/// `wv_close_t2_window_if_lapsed` both call, so a live check and the auto-close can
/// never disagree). No I/O.
fn evaluate_t2_window(row: &T2WindowRow, check_session: &str, now: i64) -> T2WindowVerdict {
    // Only a still-pending, opened T2 has a live window.
    if row.tier != "T2" || row.disposition != "pending" {
        return T2WindowVerdict::NotOpen;
    }
    let (Some(opened_at), Some(duration_s)) = (row.opened_at, row.duration_s) else {
        return T2WindowVerdict::NotOpen;
    };
    let elapsed_s = t2_effective_elapsed(now, opened_at, row.paused_at, row.pause_accumulated_s);
    let remaining_s = (duration_s - elapsed_s).max(0);
    // Paused: the clock is frozen — never runs down, never closes (E7 away).
    if row.paused_at.is_some() {
        return T2WindowVerdict::Paused {
            elapsed_s,
            remaining_s,
        };
    }
    if elapsed_s < duration_s {
        return T2WindowVerdict::Open {
            elapsed_s,
            remaining_s,
        };
    }
    // Budget spent. The surfaced-item guard: auto-close is legal ONLY in the
    // surfacing session. A blank/mismatched session (or a row with no recorded
    // surfacing session) can never lapse into consent. Both sides are trimmed so the
    // guard matches on the same canonical form `wv_open_t2_window` stored.
    let check = check_session.trim();
    match row.surfaced_session.as_deref() {
        Some(s) if !is_blank(s) && s.trim() == check && !check.is_empty() => {
            T2WindowVerdict::Lapsed { elapsed_s }
        }
        _ => T2WindowVerdict::ExpiredWrongSession { elapsed_s },
    }
}

/// Auto-close a lapsed Tier-2 window: if the window has spent its budget AND this
/// call is in the surfacing session, settle the approval `ratified` (lazy consent
/// matured) in ONE IMMEDIATE transaction. Idempotent and guard-checked: a paused,
/// still-open, wrong-session, or already-settled window is a no-op returning the
/// unchanged verdict. Returns `true` only when it ratified. Audit co-committed.
///
/// This is the write half of the surfaced-item guard: the pure `evaluate_t2_window`
/// decides, and the settle re-guards `disposition='pending'` inside the tx so a
/// concurrent T3-style owner reply (revoke) can't be clobbered (ARCHITECTURE §1.2).
pub fn wv_close_t2_window_if_lapsed(
    store: &WeftStore,
    id: i64,
    check_session: &str,
    now: i64,
) -> WvWriteResult<bool> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let Some(row) = read_t2_window(&tx, id)? else {
        return Ok(false);
    };
    if !matches!(
        evaluate_t2_window(&row, check_session, now),
        T2WindowVerdict::Lapsed { .. }
    ) {
        // Not lapsed-in-surfacing-session — nothing to ratify.
        return Ok(false);
    }
    // Re-guard the pending state inside the tx: a settle that raced in between the
    // read and here (owner revoke via `wv_settle_approval`) must win — the window
    // never over-writes a human's explicit reply.
    let changed = tx.execute(
        "UPDATE wv_approvals SET disposition='ratified', window_state='expired' \
         WHERE id=?1 AND tier='T2' AND disposition='pending'",
        params![id],
    )?;
    if changed != 1 {
        return Ok(false);
    }
    write_audit(&tx, "wv_approvals", "window_lapsed_ratify", id)?;
    tx.commit()?;
    Ok(true)
}

/// Pause EVERY live (opened, pending, running) Tier-2 window (SW-WV-E3) — the
/// single-sited presence-pause entry point (ARCHITECTURE §4.1). E7's away-enter
/// posture CALLS this; nothing else writes `paused_at`. Stamps `paused_at=now` and
/// flips `window_state='paused'` only on windows that are opened, pending, and not
/// already paused (idempotent — a second call while away is a no-op). Returns the
/// number of windows paused. Audit co-committed (one summary row).
pub fn wv_pause_all_t2_windows(store: &WeftStore, now: i64) -> WvWriteResult<usize> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_approvals SET paused_at=?1, window_state='paused' \
         WHERE tier='T2' AND disposition='pending' \
           AND window_opened_at IS NOT NULL AND paused_at IS NULL",
        params![now],
    )?;
    if changed > 0 {
        // One audit row for the batch (row_id 0 = "all live T2 windows"), same tx.
        write_audit(&tx, "wv_approvals", "pause_all_t2", 0)?;
    }
    tx.commit()?;
    Ok(changed)
}

/// Resume EVERY paused Tier-2 window (SW-WV-E3) — the single-sited counterpart to
/// [`wv_pause_all_t2_windows`] (ARCHITECTURE §4.1; E7's away-exit CALLS this). Banks
/// the in-flight pause into `pause_accumulated_s` (so the owner keeps the full
/// window of present time), clears `paused_at`, and flips `window_state='open'`.
/// Idempotent — a resume with nothing paused is a no-op. Returns the count resumed.
pub fn wv_resume_all_t2_windows(store: &WeftStore, now: i64) -> WvWriteResult<usize> {
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Bank the paused span (never negative — a clock skew can't credit time) and
    // clear the pause marker, in one guarded UPDATE over the paused set.
    let changed = tx.execute(
        "UPDATE wv_approvals SET \
           pause_accumulated_s = pause_accumulated_s + MAX(?1 - paused_at, 0), \
           paused_at = NULL, window_state = 'open' \
         WHERE tier='T2' AND disposition='pending' AND paused_at IS NOT NULL",
        params![now],
    )?;
    if changed > 0 {
        write_audit(&tx, "wv_approvals", "resume_all_t2", 0)?;
    }
    tx.commit()?;
    Ok(changed)
}

// ---------- wv_posture (SW-WA-D4 / E7) — the controller-scoped posture SOURCE ----------

/// The outcome of a [`wv_set_posture`] write: whether the posture VALUE changed and,
/// on a change, the tier-2 directive that fired plus how many windows it touched. A
/// no-change set reports `changed=false` and fires nothing (idempotent) — the SPEC's
/// "setting the posture to its current value fires neither".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostureSetOutcome {
    /// Whether the posture VALUE (Normal ⇄ Away) changed relative to what was stored.
    pub changed: bool,
    /// The directive that fired on a change (`None` on a no-change set).
    pub directive: Option<super::postures::T2Directive>,
    /// The number of tier-2 windows the directive paused / resumed (0 when nothing fired).
    pub windows_affected: usize,
}

/// Read the stored controller-scoped posture (SW-WA-D4). Before any [`wv_set_posture`]
/// call — a fresh store with no `wv_posture` row — this returns the default
/// [`super::postures::PostureConfig::bare`] of [`super::postures::Posture::Normal`] with
/// no widening windows, so the posture layer is inert until the operator opts into Away.
/// A stored value that no longer parses falls back to `Normal` (never `Away`, which
/// could strand earned work) via [`super::postures::PostureConfig::from_wire`].
pub fn wv_read_posture(store: &WeftStore) -> WvWriteResult<super::postures::PostureConfig> {
    use super::postures::{Posture, PostureConfig};
    let conn = store.conn()?;
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT posture, windows FROM wv_posture WHERE id=1",
            [],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((posture, windows)) = row else {
        // No row yet — the inert default.
        return Ok(PostureConfig::bare(Posture::Normal));
    };
    // Reconstruct the config from the stored columns through the single wire seam so a
    // corrupt value fails open to Normal (never Away).
    let windows_json: serde_json::Value =
        serde_json::from_str(&windows).unwrap_or_else(|_| serde_json::json!([]));
    let value = serde_json::json!({ "posture": posture, "windows": windows_json });
    Ok(PostureConfig::from_wire(&value))
}

/// Persist the controller-scoped posture (SW-WA-D4) and, when the posture VALUE
/// CHANGES, fire the single-sited tier-2 pause/resume the transition implies:
/// entering [`super::postures::Posture::Away`] calls [`wv_pause_all_t2_windows`]
/// (`PauseAll`), returning to [`super::postures::Posture::Normal`] calls
/// [`wv_resume_all_t2_windows`] (`ResumeAll`). The posture row is stored as its wire
/// form (`Posture::as_str` + the serialized window list); the pause/resume writes stay
/// owned by those two functions (ARCHITECTURE §4.1 — this never writes `paused_at`
/// directly). A no-change set fires neither (idempotent).
pub fn wv_set_posture(
    store: &WeftStore,
    config: &super::postures::PostureConfig,
    now: i64,
) -> WvWriteResult<PostureSetOutcome> {
    // 1. What is stored now? (the transition's `from` — read BEFORE the write).
    let previous = wv_read_posture(store)?.posture();
    let next = config.posture();
    let changed = previous != next;

    // 2. Persist the new config (the posture SOURCE). Wire form: as_str + window list.
    let wire = config.to_wire();
    let posture_str = wire["posture"].as_str().unwrap_or("normal").to_string();
    let windows_str = serde_json::to_string(&wire["windows"]).unwrap_or_else(|_| "[]".to_string());
    {
        let mut conn = store.conn()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO wv_posture (id, posture, windows, updated_at) \
             VALUES (1, ?1, ?2, ?3) \
             ON CONFLICT(id) DO UPDATE SET \
               posture=excluded.posture, windows=excluded.windows, updated_at=excluded.updated_at",
            params![posture_str, windows_str, now],
        )?;
        write_audit(&tx, "wv_posture", "set", 1)?;
        tx.commit()?;
    }

    // 3. On a VALUE change, fire the tier-2 directive via the sanctioned single-sited
    //    functions — never a second pause table nor a raw pause-column write here.
    if !changed {
        return Ok(PostureSetOutcome {
            changed: false,
            directive: None,
            windows_affected: 0,
        });
    }
    let directive = config.t2_directive_on_enter();
    let windows_affected = match directive {
        super::postures::T2Directive::PauseAll => wv_pause_all_t2_windows(store, now)?,
        super::postures::T2Directive::ResumeAll => wv_resume_all_t2_windows(store, now)?,
    };
    Ok(PostureSetOutcome {
        changed: true,
        directive: Some(directive),
        windows_affected,
    })
}

// ---------- wv_watermarks (AC-4) ----------

/// Upsert a per-source read watermark on the `(source, scope)` compound PK (AC-4).
/// `source` must be one of [`WV_WATERMARK_SOURCES`]; an invalid value is rejected
/// before the write. A second upsert on the same key updates `last_handled` +
/// `updated`, never inserts a duplicate. Audit co-committed (AC-9).
pub fn wv_upsert_watermark(
    store: &WeftStore,
    source: &str,
    scope: &str,
    last_handled: &str,
) -> WvWriteResult<()> {
    if !WV_WATERMARK_SOURCES.contains(&source) {
        return Err(reject(
            "source must be one of: slack | jira | github | local | agent",
        ));
    }
    if is_blank(scope) {
        return Err(reject("scope must not be blank"));
    }
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO wv_watermarks (source, scope, last_handled, updated) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(source, scope) DO UPDATE SET \
          last_handled=excluded.last_handled, updated=excluded.updated",
        params![source, scope, last_handled, now_iso()],
    )?;
    write_audit_named(&tx, "wv_watermarks", "upsert", &format!("{source}/{scope}"))?;
    tx.commit()?;
    Ok(())
}

// ---------- wv_attention (AC-8) ----------

/// Insert or update an attention item (AC-8). On INSERT (`id = None`) `summary` is
/// required non-blank (a DTO-read column — a reader must never see a blank card)
/// and `state` defaults to `'needs-you'`. On UPDATE (`id = Some`) the given `state`
/// and `updated_at` are applied. `state` is validated against the allowed set.
/// Returns the row id. Audit co-committed (AC-9).
#[allow(clippy::too_many_arguments)]
pub fn wv_upsert_attention(
    store: &WeftStore,
    id: Option<i64>,
    state: Option<&str>,
    summary: Option<&str>,
    source: Option<&str>,
    reference: Option<&str>,
    owner: Option<&str>,
    need: Option<&str>,
) -> WvWriteResult<i64> {
    if let Some(state) = state {
        if !WV_ATTENTION_STATES.contains(&state) {
            return Err(reject(
                "state must be one of: needs-you | waiting | done-unacked",
            ));
        }
    }
    // SLW-TL-4: a declared need, when supplied, must be one of the fixed set — the
    // Inbox maps it to the matching action, so an unknown need can never leak in.
    if let Some(need) = need {
        if !WV_ATTENTION_NEEDS.contains(&need) {
            return Err(reject(
                "need must be one of: awaiting-answer | needs-approval | needs-attention | awaiting-validation | done",
            ));
        }
    }
    match id {
        None => {
            // Creation — summary is mandatory and non-blank.
            let summary = summary.unwrap_or("");
            if is_blank(summary) {
                return Err(reject(
                    "summary must not be blank at creation (a reader must never see a blank card)",
                ));
            }
            let state = state.unwrap_or("needs-you");
            let now = now_iso();
            let mut conn = store.conn()?;
            let new_id = wv_auto_append(
                &mut conn,
                "INSERT INTO wv_attention \
                 (state, summary, source, ref, owner, need, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                &[&state, &summary, &source, &reference, &owner, &need, &now],
                "wv_attention",
            )?;
            Ok(new_id)
        }
        Some(id) => {
            // Update an existing item's state (and, if supplied, its fields). The
            // `updated_at` stamp always advances; `summary` is only overwritten when
            // a non-blank value is supplied (never blanked on a state-only update).
            let now = now_iso();
            let mut conn = store.conn()?;
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed = tx.execute(
                "UPDATE wv_attention SET \
                  state = COALESCE(?2, state), \
                  summary = CASE WHEN ?3 IS NOT NULL AND TRIM(?3) <> '' THEN ?3 ELSE summary END, \
                  source = COALESCE(?4, source), \
                  ref = COALESCE(?5, ref), \
                  owner = COALESCE(?6, owner), \
                  need = COALESCE(?8, need), \
                  updated_at = ?7 \
                 WHERE id=?1",
                params![id, state, summary, source, reference, owner, now, need],
            )?;
            if changed == 1 {
                write_audit(&tx, "wv_attention", "update", id)?;
                tx.commit()?;
                Ok(id)
            } else {
                Err(reject("no attention row for the given id"))
            }
        }
    }
}

// ---------- wv_list_attention_board (SW-WV-F1) ----------
//
// The Weft Board's single read: five slices assembled in ONE read transaction.
// Three slices are `wv_attention` rows (by `state`); two are `wv_queue` rows (by
// dispatch state, ARCHITECTURE §2.4 — ON-AGENT is BOTH `claimed` AND `dispatched`,
// PARKED is `pending`). Read-only — no INSERT/UPDATE/DELETE against any `wv_*`
// table, so no audit row (audit records mutations, §1.6). F1 reads what B3/E1/A4/
// C3/C6 wrote; it never writes.

/// SW-WA-E5: who started an attention-board row — the provenance axis the 🤖 marker
/// reads. `Agent` is an agent-minted or agent-claimed row (an agent acting on its
/// own); `Human` is a human-relevant triage row no agent has touched. Derived on
/// READ from durable `wv_queue` columns (never a background writer); `Human` is the
/// deny-first default when no agent signal is present. Serialises to camelCase
/// `"agent"` | `"human"`.
///
/// SW-AV-3: the `agent` variant was renamed from `Controller` (the 🤖 marker means
/// agent-owned, not the retired controller). The derivation is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initiator {
    Agent,
    Human,
}

/// SW-AV-3: what an attention-board row IS, so the Activity chip derives from the
/// row's nature rather than the lane it landed in (design 4.3). A closed vocabulary
/// mirrored on the wire as [`crate::commands::weft_ledger::AttentionKindDto`] and
/// consumed by SW-AV-4's per-kind chip. Derived on READ alongside `initiator` (no
/// schema column, no data migration): the `wv_queue` slices map by dispatch state
/// (`pending` → `Parked`, active → `Running`); the `wv_attention` slices map by
/// `state` (`needs-you` → `Approval`, `waiting` → `Question`, `done-unacked` →
/// `Done`). `Assertion` and `Blocked` are part of the vocabulary AV-4 renders; a
/// finer per-row discriminator (assertion-vs-approval, blocked-vs-question) is a
/// future enrichment once a durable row-type column exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionKind {
    Assertion,
    Approval,
    Question,
    Parked,
    Blocked,
    Running,
    Done,
}

/// SLW-TL-4: what a board item DECLARES it needs — the fixed set (§2.3) any producer
/// (agent / CLI / trigger) stamps so the Inbox maps need → the matching action (the
/// actions land in SLW-TL-7). A durable `wv_attention.need` column value, read back
/// here; `from_wire` returns `None` for a legacy/blank/unknown value so an old weft
/// row (written before the field) reads cleanly and the board falls back to `state`.
/// Mirrored on the wire as [`crate::commands::weft_ledger::AttentionNeedDto`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionNeed {
    AwaitingAnswer,
    NeedsApproval,
    NeedsAttention,
    AwaitingValidation,
    Done,
}

impl AttentionNeed {
    /// Parse a stored `need` value; `None` for a missing / blank / unrecognised
    /// value (a legacy row never crashes the board read).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "awaiting-answer" => Some(Self::AwaitingAnswer),
            "needs-approval" => Some(Self::NeedsApproval),
            "needs-attention" => Some(Self::NeedsAttention),
            "awaiting-validation" => Some(Self::AwaitingValidation),
            "done" => Some(Self::Done),
            _ => None,
        }
    }
}

/// SW-AV-3: derive a `wv_attention` row's `kind` from its `state` — the only durable
/// signal a `wv_attention` row carries today. `waiting` (OWED-TO-YOU) is a `Question`
/// you are awaiting an answer on; `done-unacked` is `Done`; `needs-you` (YOU-OWE) and
/// any future state default to `Approval` (a decision you owe). Pure — no clock, no
/// I/O.
fn attention_kind_for_state(state: &str) -> AttentionKind {
    match state {
        "waiting" => AttentionKind::Question,
        "done-unacked" => AttentionKind::Done,
        _ => AttentionKind::Approval,
    }
}

/// One attention-board row on the read side (SW-WV-F1). A neutral shape both the
/// `wv_attention` slices and the `wv_queue`-derived slices map onto, so the FE
/// renders every section through one row renderer. `summary` is the ~140-char
/// standalone sentence a reader lands on; `source`/`ref_` are the opaque routing
/// refs the chevron uses (`wv_attention.source/ref`; for a queue row, its `source`
/// and `refs`). The Tauri layer serialises this into `AttentionRowDto` (camelCase,
/// `ref_` → `ref`).
///
/// SW-WA-E5 adds two derived-on-read axes: `initiator` (the 🤖-vs-human provenance)
/// and `live_session_id` (the ON-AGENT one-click live-terminal target — the queue
/// row's `dispatch_handle`, stamped verbatim; the FE owns the resolvability guard).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionBoardRow {
    pub id: i64,
    pub summary: String,
    pub source: Option<String>,
    pub ref_: Option<String>,
    /// SW-WA-E5: who started the row (derived from `wv_queue` columns on read).
    /// `wv_attention`-sourced slices (YOU-OWE / OWED-TO-YOU / DONE-UNACKED) are
    /// `Human` — they carry no queue provenance columns.
    pub initiator: Initiator,
    /// SW-AV-3: what the row IS (assertion / approval / question / parked / blocked /
    /// running / done) — the Activity chip derives from this, not the lane. Derived on
    /// read (see [`AttentionKind`]).
    pub kind: AttentionKind,
    /// SW-WA-E5: the live worker's dispatch handle for a `dispatched` queue row
    /// (the FE resolves it to a terminal tab; a bare-name handle yields no
    /// affordance). `None` for every non-dispatched row.
    pub live_session_id: Option<String>,
    /// SLW-TL-4: the DECLARED need this item carries (`None` for a legacy weft row
    /// or a queue-derived row that never declared one — the board then keys off the
    /// lane). Read from `wv_attention.need`; the generalized Inbox row renderer keys
    /// its plain label + matching-action off this.
    pub need: Option<AttentionNeed>,
}

/// The five board slices (SW-WV-F1), each an ordered `Vec<AttentionBoardRow>`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AttentionBoard {
    pub you_owe: Vec<AttentionBoardRow>,
    pub owed_to_you: Vec<AttentionBoardRow>,
    pub on_agent: Vec<AttentionBoardRow>,
    pub parked: Vec<AttentionBoardRow>,
    pub done_unacked: Vec<AttentionBoardRow>,
}

/// SW-WA-E5: derive a queue row's `initiator` from the durable columns that record
/// how it was started. `Agent` iff an agent claimed it (`claimed_by` is a non-empty
/// claim — a Keep or a worker/run Delegate, per `queue::claim_with_tripwire`), OR it
/// is an agent-owned escalation (`source = 'agent'`, per `wv_escalate_insert`). Every
/// other row — a human-relevant triage row (`source` in slack/jira/github/local,
/// `claimed_by = ''`) — is `Human`. Deny-first: absent an agent signal, a row is
/// `Human` (autonomy is never over-claimed). Pure — no clock, no I/O.
/// SW-AV-3: returns `Agent` (renamed from `Controller`); logic unchanged.
fn derive_initiator(claimed_by: Option<&str>, source: Option<&str>) -> Initiator {
    let claimed = claimed_by.unwrap_or("").trim();
    let is_controller_claim = !claimed.is_empty();
    let is_agent_escalation = source == Some("agent");
    if is_controller_claim || is_agent_escalation {
        Initiator::Agent
    } else {
        Initiator::Human
    }
}

/// SW-WV-F1: assemble the five-section attention board in one read transaction.
/// YOU-OWE / OWED-TO-YOU / DONE-UNACKED are `wv_attention` rows filtered by `state`;
/// ON-AGENT is `wv_queue WHERE state IN ('claimed','dispatched')` (BOTH active
/// states — ARCHITECTURE §2.4; omitting `dispatched` hides live workers); PARKED is
/// `wv_queue WHERE state='pending'`. Read-only.
pub fn wv_list_attention_board(store: &WeftStore) -> WvWriteResult<AttentionBoard> {
    let conn = store.conn()?;

    // Attention slice by `state` — ordered by id for a stable board.
    let attention_slice = |state: &str| -> rusqlite::Result<Vec<AttentionBoardRow>> {
        let mut stmt = conn.prepare(
            "SELECT id, summary, source, ref, need FROM wv_attention \
             WHERE state=?1 ORDER BY id",
        )?;
        let rows = stmt
            .query_map(params![state], |r| {
                let need: Option<String> = r.get(4)?;
                Ok(AttentionBoardRow {
                    id: r.get(0)?,
                    summary: r.get(1)?,
                    source: r.get(2)?,
                    ref_: r.get(3)?,
                    // A `wv_attention` row carries no queue provenance columns —
                    // it's a human-owned judgment surface, so `Human` / no live
                    // handle (SW-WA-E5). The agent-provenance derivation lives
                    // only on the queue slice below.
                    initiator: Initiator::Human,
                    // SW-AV-3: the kind derives from the attention `state`.
                    kind: attention_kind_for_state(state),
                    live_session_id: None,
                    // SLW-TL-4: the declared need, if this row carries one.
                    need: need.as_deref().and_then(AttentionNeed::from_wire),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    };

    // Queue slice — a live queue row rendered on the board. `note` is the row's
    // plain-English summary when present (the enqueuer's human sentence); it falls
    // back to `source` so a reader never sees a blank card. `source`/`refs` carry the
    // routing the chevron uses.
    let queue_slice = |states: &[&str]| -> rusqlite::Result<Vec<AttentionBoardRow>> {
        let placeholders = states
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT id, note, source, refs, state, claimed_by, dispatch_handle FROM wv_queue \
             WHERE state IN ({placeholders}) ORDER BY id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let params_dyn: Vec<&dyn rusqlite::ToSql> =
            states.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
        let rows = stmt
            .query_map(params_dyn.as_slice(), |r| {
                let id: i64 = r.get(0)?;
                let note: Option<String> = r.get(1)?;
                let source: Option<String> = r.get(2)?;
                let refs: Option<String> = r.get(3)?;
                let state: String = r.get(4)?;
                let claimed_by: Option<String> = r.get(5)?;
                let dispatch_handle: Option<String> = r.get(6)?;
                let summary = match note {
                    Some(n) if !n.trim().is_empty() => n,
                    _ => source.clone().unwrap_or_default(),
                };
                Ok(AttentionBoardRow {
                    id,
                    initiator: derive_initiator(claimed_by.as_deref(), source.as_deref()),
                    // SW-AV-3: a `pending` queue row is PARKED; an active (claimed /
                    // dispatched) one is a RUNNING agent.
                    kind: if state == "pending" {
                        AttentionKind::Parked
                    } else {
                        AttentionKind::Running
                    },
                    // The live-terminal target: a `dispatched` row's opaque worker
                    // handle, stamped VERBATIM (the FE resolves it to a live tab — a
                    // bare-name handle earns no affordance). A non-dispatched row
                    // carries no live handle (SW-WA-E5).
                    live_session_id: if state == "dispatched" {
                        dispatch_handle
                    } else {
                        None
                    },
                    summary,
                    source,
                    ref_: refs,
                    // A queue-derived row (ON-AGENT / PARKED) declares no need — it
                    // is a running/queued agent, not a posted attention item.
                    need: None,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    };

    Ok(AttentionBoard {
        you_owe: attention_slice("needs-you")?,
        owed_to_you: attention_slice("waiting")?,
        on_agent: queue_slice(&["claimed", "dispatched"])?,
        parked: queue_slice(&["pending"])?,
        done_unacked: attention_slice("done-unacked")?,
    })
}

// ============================================================================
// SW-WV-E1 — the endorsement gate: `wv_assertions` + `wv_recipient_acks`.
//
// The one genuinely new safety mechanism (ARCHITECTURE §4.1 "the wall never
// moves"): an agent assertion is a durable, 🤖-marked ledger row that counts for
// nothing until the owning human 👍s it. All writes route through here (the sole
// sanctioned writer file, §1.2) with their audit row co-committed on one IMMEDIATE
// transaction (§1.6). Ratification is a Tauri command (human action through the UI
// boundary), never an MCP verb — see `commands::weft_ledger::wv_ratify`.
// ============================================================================

/// The allowed `wv_recipient_acks.ack` value set (AC-3). `'declined'` (🚫) routes a
/// finding back to the worker but NEVER ratifies (AC-4); ratification stays the
/// owner's `wv_ratify` action.
pub const WV_ACK_VALUES: &[&str] = &["seen", "acknowledged", "declined"];

/// The allowed `wv_assertions.disposition` settle targets (AC-5). `pending` is the
/// insert default and is never a settle target.
pub const WV_ASSERTION_DISPOSITIONS: &[&str] = &["ratified", "revoked"];

/// Insert a pending agent assertion (AC-2). The endorsement gate's output record:
/// what the agent asserted + its ratification state. `disposition` is always
/// `'pending'` at insert — the wall never moves without an owner 👍 (`wv_ratify`).
/// `claim` and `channel` are redacted+size-bounded by the CALLER (the MCP verb) per
/// AC-2 hygiene before they reach here. Returns the AUTOINCREMENT id. Audit
/// co-committed (AC-9).
pub fn wv_insert_assertion(
    store: &WeftStore,
    session_id: &str,
    run_id: Option<&str>,
    claim: &str,
    channel: &str,
    approval_id: Option<i64>,
) -> WvWriteResult<i64> {
    if is_blank(session_id) {
        return Err(reject("session_id must not be blank"));
    }
    if is_blank(claim) {
        return Err(reject("claim must not be blank"));
    }
    if is_blank(channel) {
        return Err(reject("channel must not be blank"));
    }
    let mut conn = store.conn()?;
    let id = wv_auto_append(
        &mut conn,
        "INSERT INTO wv_assertions \
         (session_id, run_id, claim, channel, posted_at, disposition, approval_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
        &[
            &session_id,
            &run_id,
            &claim,
            &channel,
            &now_iso(),
            &approval_id,
        ],
        "wv_assertions",
    )?;
    Ok(id)
}

/// A durable assertion row read back by id (the ratify/revoke + decline paths need
/// `session_id`/`run_id`/`channel`/`claim`/`disposition`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WvAssertionRow {
    pub id: i64,
    pub session_id: String,
    pub run_id: Option<String>,
    pub claim: String,
    pub channel: String,
    pub disposition: String,
    pub posted_at: String,
    pub approval_id: Option<i64>,
}

/// Read one assertion by id (AC-4/AC-5 read path). Returns `None` if no such row.
pub fn wv_get_assertion(store: &WeftStore, id: i64) -> WvWriteResult<Option<WvAssertionRow>> {
    let conn = store.conn()?;
    let row = conn
        .query_row(
            "SELECT id, session_id, run_id, claim, channel, disposition, approval_id, posted_at \
             FROM wv_assertions WHERE id=?1",
            params![id],
            |r| {
                Ok(WvAssertionRow {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    run_id: r.get(2)?,
                    claim: r.get(3)?,
                    channel: r.get(4)?,
                    disposition: r.get(5)?,
                    approval_id: r.get(6)?,
                    posted_at: r.get(7)?,
                })
            },
        )
        .ok();
    Ok(row)
}

/// SW-WA-C0: find the most-recent `pending`
/// `wv_approvals` row whose `source` matches, so the `wv_post_assertion` handler can pair
/// the worker's assertion to the approval `wv_write_approval` just stamped (both derive
/// the SAME `queue:<queue_id>` server-side, no agent-carried id). `ORDER BY id DESC LIMIT
/// 1` picks the freshest open endorsement for that queue pointer. Returns `None` when no
/// pending row matches (the assertion is then written with `approval_id = None`, the
/// manual/non-M1 path). Read-only.
pub fn wv_find_pending_approval_by_source(
    store: &WeftStore,
    source: &str,
) -> WvWriteResult<Option<i64>> {
    let conn = store.conn()?;
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM wv_approvals \
             WHERE source=?1 AND disposition='pending' \
             ORDER BY id DESC LIMIT 1",
            params![source],
            |r| r.get(0),
        )
        .optional()?;
    Ok(id)
}

/// A pending assertion projected for the review-at-altitude header (SW-WV-F4). Adds
/// `posted_at` to [`WvAssertionRow`]'s fields — the header lists the assertions the
/// operator must judge, newest-informing-first by insert order. Serialised camelCase
/// through the Tauri DTO in `commands::weft_ledger`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WvPendingAssertion {
    pub id: i64,
    pub session_id: String,
    pub run_id: Option<String>,
    pub claim: String,
    pub channel: String,
    pub posted_at: String,
    pub disposition: String,
}

/// SW-WV-F4 (AC-1): read every `pending` assertion for a run — the review header's
/// source list. Filters to `disposition='pending'` in SQL so a settled (ratified /
/// revoked) row can never surface as still-needing-judgment. Ordered by id (insert
/// order) for a stable header. Read-only.
pub fn wv_read_pending_assertions(
    store: &WeftStore,
    run_id: &str,
) -> WvWriteResult<Vec<WvPendingAssertion>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare(
        "SELECT id, session_id, run_id, claim, channel, posted_at, disposition \
         FROM wv_assertions WHERE run_id=?1 AND disposition='pending' ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![run_id], |r| {
            Ok(WvPendingAssertion {
                id: r.get(0)?,
                session_id: r.get(1)?,
                run_id: r.get(2)?,
                claim: r.get(3)?,
                channel: r.get(4)?,
                posted_at: r.get(5)?,
                disposition: r.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Read just an assertion's disposition (AC-5 idempotency read). `None` when the id
/// is unknown.
pub fn wv_get_assertion_disposition(store: &WeftStore, id: i64) -> WvWriteResult<Option<String>> {
    let conn = store.conn()?;
    let disposition: Option<String> = conn
        .query_row(
            "SELECT disposition FROM wv_assertions WHERE id=?1",
            params![id],
            |r| r.get(0),
        )
        .ok();
    Ok(disposition)
}

/// CAS-style disposition transition (AC-5, ARCHITECTURE §1.2 ONE-WRITER): a guarded
/// `UPDATE ... WHERE id=?1 AND disposition=?2`. Returns `true` when it matched
/// exactly one row (`changes()==1` — the caller won the transition), `false` when
/// the row was already in a different state (the caller lost; report
/// `alreadySettled`). `next` MUST be one of [`WV_ASSERTION_DISPOSITIONS`]. Audit
/// co-committed only on the winning update.
pub fn wv_set_assertion_disposition(
    store: &WeftStore,
    id: i64,
    current: &str,
    next: &str,
) -> WvWriteResult<bool> {
    if !WV_ASSERTION_DISPOSITIONS.contains(&next) {
        return Err(reject(
            "next disposition must be one of: ratified | revoked",
        ));
    }
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_assertions SET disposition=?3 WHERE id=?1 AND disposition=?2",
        params![id, current, next],
    )?;
    if changed == 1 {
        write_audit(&tx, "wv_assertions", "settle", id)?;
        tx.commit()?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Record one recipient reaction (AC-3): 👀 `seen` / ✅ `acknowledged` / 🚫
/// `declined`. This NEVER touches `wv_assertions.disposition` — a recipient ack is
/// separate from owner ratification (the wall never moves on a recipient signal).
/// `ack` must be one of [`WV_ACK_VALUES`]. Returns the new ack row id. Audit
/// co-committed (AC-9).
pub fn wv_insert_recipient_ack(
    store: &WeftStore,
    assertion_id: i64,
    recipient: &str,
    ack: &str,
) -> WvWriteResult<i64> {
    if !WV_ACK_VALUES.contains(&ack) {
        return Err(reject("ack must be one of: seen | acknowledged | declined"));
    }
    if is_blank(recipient) {
        return Err(reject("recipient must not be blank"));
    }
    let mut conn = store.conn()?;
    let id = wv_auto_append(
        &mut conn,
        "INSERT INTO wv_recipient_acks (assertion_id, recipient, ack, recorded_at) \
         VALUES (?1, ?2, ?3, ?4)",
        &[&assertion_id, &recipient, &ack, &now_iso()],
        "wv_recipient_acks",
    )?;
    Ok(id)
}

/// Settle a `wv_attention` row identified by `(source, ref)` to `next_state`
/// (SW-WV-E1, D8 teardown hook). B3's `wv_upsert_attention` updates by integer id;
/// the E1 assertion→attention hook keys the YOU-OWE row on
/// `source="assertion"` + `ref=<assertion_id>`, so ratify/revoke settle it here by
/// that natural key. Guarded `UPDATE ... WHERE source=?1 AND ref=?2` +
/// `changes()>=1`; a no-op (no matching row) returns `false` rather than erroring
/// (the attention row may have been cleared already). `next_state` must be a legal
/// [`WV_ATTENTION_STATES`] value. Audit co-committed on the update.
pub fn wv_settle_attention(
    store: &WeftStore,
    source: &str,
    reference: &str,
    next_state: &str,
) -> WvWriteResult<bool> {
    if !WV_ATTENTION_STATES.contains(&next_state) {
        return Err(reject(
            "state must be one of: needs-you | waiting | done-unacked",
        ));
    }
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_attention SET state=?3, updated_at=?4 WHERE source=?1 AND ref=?2",
        params![source, reference, next_state, now_iso()],
    )?;
    if changed >= 1 {
        // Audit keyed on the natural (source/ref) key — no integer surrogate for a
        // by-key settle (mirrors the roster/watermark audit-by-name idiom).
        write_audit_named(
            &tx,
            "wv_attention",
            "settle",
            &format!("{source}/{reference}"),
        )?;
        tx.commit()?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Append an audit row keyed by a natural-PK name rather than an integer id (AC-9).
/// `wv_roster` / `wv_watermarks` have no integer surrogate; the audit `row_id`
/// column is an INTEGER, so a stable hash of the name is stored (queryable +
/// collision-safe for audit purposes; the human-readable key is in `operation`'s
/// row context via the table name). Runs on the same transaction as the mutation.
fn write_audit_named(
    tx: &Transaction,
    table: &'static str,
    operation: &'static str,
    natural_key: &str,
) -> WeftStoreResult<()> {
    let row_id = stable_key_hash(natural_key);
    tx.execute(
        "INSERT INTO wv_write_log (tbl, operation, row_id, written_at) VALUES (?1, ?2, ?3, ?4)",
        params![table, operation, row_id, now_epoch_secs()],
    )?;
    Ok(())
}

/// A stable non-cryptographic hash of a natural-PK string → an i64 audit `row_id`
/// (used for `wv_roster` / `wv_watermarks`, which have no integer surrogate). FNV-1a
/// so the value is reproducible across processes (a `DefaultHasher` is not).
fn stable_key_hash(key: &str) -> i64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in key.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as i64
}

// ============================================================================
// SW-WV-B5 — the scope layer: `wv_scopes` + `wv_watches` handlers.
//
// The two tables complete the ledger family's scope layer. Neither absorbs the
// artifacts it points at (`artifact_ptr` is a pointer, never a copy — AC-6). Every
// mutating handler below routes its ledger mutation + its `wv_write_log` audit row
// through ONE IMMEDIATE transaction (ARCHITECTURE §1.6, AC-9); none opens a
// transaction outside this file (ONE-WRITER §1.2). All handlers are synchronous
// SQLite — callers run them on the blocking pool (§1.6). Scope timestamps are
// epoch-**milliseconds** (`created_at`/`updated_at`/`closed_at`), distinct from the
// coarse epoch-seconds audit stamp in `wv_write_log.written_at`.
// ============================================================================

/// An epoch-**milliseconds** wall-clock stamp for a scope/watch mutation column
/// (`wv_scopes.created_at`/`updated_at`/`closed_at`, `wv_watches.updated_at`).
/// Distinct from [`now_epoch_secs`], which stamps the coarse audit trail.
fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// The lifecycle of a [`wv_watches`] row (AC-2, AC-5). The Rust enum is the gate:
/// [`wv_watch_upsert`] validates the wire value against this set BEFORE the SQL
/// call, so an unknown lifecycle is rejected at the handler, not only by SQLite.
///
/// - `Open` — polled every triage sweep.
/// - `Quiet` — idle past the quiet threshold (slower cadence, never auto-dropped).
/// - `Closed` — only on positive evidence ([`CloseReason`]); **silence never
///   closes a watch** (AC-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchLifecycle {
    Open,
    Quiet,
    Closed,
}

impl WatchLifecycle {
    /// Parse a wire lifecycle value; `None` for any unknown token (the handler
    /// maps that to a validation refusal).
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "open" => Some(WatchLifecycle::Open),
            "quiet" => Some(WatchLifecycle::Quiet),
            "closed" => Some(WatchLifecycle::Closed),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            WatchLifecycle::Open => "open",
            WatchLifecycle::Quiet => "quiet",
            WatchLifecycle::Closed => "closed",
        }
    }
}

/// The allowed `wv_watches.lifecycle` values (AC-2), surfaced for the MCP/Tauri
/// boundary and the drift tests. Mirrors [`WatchLifecycle`].
pub const WV_WATCH_LIFECYCLES: &[&str] = &["open", "quiet", "closed"];

/// The positive-evidence reason a watch is closed (AC-5). `Silence` is NOT a
/// variant — silence never closes a watch. [`wv_watch_close`] requires one of
/// these; a bare `closed` write with no reason is rejected at the handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// An explicit close command from the operator or controller.
    ExplicitCommand,
    /// The owning scope closed (cascades a close onto its watches).
    ScopeClosed,
    /// A "tail message" in a thread signalled the work is done.
    TailMessage,
}

impl CloseReason {
    /// Parse a wire close-reason value; `None` for any unknown/blank token.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "explicit_command" => Some(CloseReason::ExplicitCommand),
            "scope_closed" => Some(CloseReason::ScopeClosed),
            "tail_message" => Some(CloseReason::TailMessage),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            CloseReason::ExplicitCommand => "explicit_command",
            CloseReason::ScopeClosed => "scope_closed",
            CloseReason::TailMessage => "tail_message",
        }
    }
}

/// The allowed `wv_watch_close` reasons (AC-5), surfaced for the boundary. Mirrors
/// [`CloseReason`].
pub const WV_WATCH_CLOSE_REASONS: &[&str] = &["explicit_command", "scope_closed", "tail_message"];

/// A single `wv_scopes` row (AC-1). `lease_session` is a legacy column nothing
/// writes since the scope lease was removed (SW-AV-27); it is always `None` for a new
/// scope. Timestamps are epoch-**ms**; `closed_at` is `None` while the scope is open.
///
/// SW-AV-17: `owner_session` records WHO OPENED this scope (the opener's session id),
/// stamped once at `wv_scope_create`. Unlike `lease_session` — which the lease-acquire
/// path writes and clears with the lease TTL, and which SW-AV-27 deletes — `owner_session`
/// is decoupled from any lease: it is the stable key the who-opened-whom tree (SW-AV-18)
/// joins on, and it survives lease deletion. `None` for a pre-SW-AV-17 row (opener not
/// recorded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRow {
    pub scope: String,
    pub parent_scope: Option<String>,
    pub artifact_ptr: Option<String>,
    pub lease_session: Option<String>,
    pub owner_session: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub closed_at: Option<i64>,
}

/// A single `wv_watches` row (AC-2). `lifecycle` is one of [`WV_WATCH_LIFECYCLES`];
/// `updated_at` is epoch-**ms**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchRow {
    pub scope: String,
    pub kind: String,
    pub reference: String,
    pub lifecycle: String,
    pub updated_at: i64,
}

/// Append a scope-mutation audit row keyed by the scope name (AC-9). `wv_scopes`'
/// PK is a name, not an integer id; the audit `row_id` column is an INTEGER, so a
/// stable FNV-1a hash of the scope name is stored (queryable, reproducible), same
/// convention as [`write_audit_named`] for `wv_roster`/`wv_watermarks`. Runs on the
/// same transaction as the mutation.
fn write_audit_scope(
    tx: &Transaction,
    operation: &'static str,
    scope: &str,
) -> WeftStoreResult<()> {
    write_audit_named(tx, "wv_scopes", operation, scope)
}

/// Append a watch-mutation audit row keyed by the composite `(scope, kind, ref)`
/// natural key (AC-9). Same natural-key hashing convention as [`write_audit_scope`].
fn write_audit_watch(
    tx: &Transaction,
    operation: &'static str,
    scope: &str,
    kind: &str,
    reference: &str,
) -> WeftStoreResult<()> {
    write_audit_named(
        tx,
        "wv_watches",
        operation,
        &format!("{scope}/{kind}/{reference}"),
    )
}

// ---------- wv_scopes (AC-1, AC-3, AC-6, AC-9) ----------

/// Create a scope idempotently (AC-1). `INSERT OR IGNORE` on the `scope` PK:
/// returns `Ok(true)` when a new row was created, `Ok(false)` when the scope
/// already existed (no overwrite). `artifact_ptr` defaults to the empty string
/// (a valid pointer for a scope that pre-exists its first artifact — story §schema).
/// `parent_scope = None` marks a root scope. Audit co-committed only on a real
/// insert (AC-9).
///
/// SW-AV-17: `owner_session` records the opener's session id (who-opened-this-scope),
/// written HERE at creation and decoupled from any lease. It is INDEPENDENT of
/// `lease_session` (which is left `NULL` at create and only written by the lease-acquire
/// path): a scope with no lease still carries its `owner_session`, so the who-opened-whom
/// tree (SW-AV-18) survives the lease deletion (SW-AV-27). `None` records an unknown
/// opener (an honest empty owner, e.g. a pre-SW-AV-17 caller).
pub fn wv_scope_create(
    store: &WeftStore,
    scope: &str,
    parent_scope: Option<&str>,
    artifact_ptr: Option<&str>,
    owner_session: Option<&str>,
) -> WvWriteResult<bool> {
    if is_blank(scope) {
        return Err(reject("scope name (PK) must not be blank"));
    }
    let now = now_epoch_ms();
    let artifact_ptr = artifact_ptr.unwrap_or("");
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "INSERT OR IGNORE INTO wv_scopes \
         (scope, parent_scope, artifact_ptr, lease_session, owner_session, created_at, updated_at, closed_at) \
         VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?5, NULL)",
        params![scope, parent_scope, artifact_ptr, owner_session, now],
    )?;
    if changed == 1 {
        write_audit_scope(&tx, "create", scope)?;
        tx.commit()?;
        Ok(true)
    } else {
        // Row already existed — INSERT OR IGNORE changed nothing. Idempotent no-op;
        // drop the transaction (nothing to audit).
        Ok(false)
    }
}

/// Update a scope's `artifact_ptr` and stamp `updated_at` (AC-6). Purely a text
/// column write — it does NOT read, write, move, or copy any file at the pointed-at
/// path (the pointer is a pointer, never a copy). Returns `false` when no scope
/// matched the name (a no-op), `true` when the row was updated. Audit co-committed
/// on the update (AC-9).
pub fn wv_scope_update_artifact_ptr(
    store: &WeftStore,
    scope: &str,
    artifact_ptr: &str,
) -> WvWriteResult<bool> {
    let now = now_epoch_ms();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_scopes SET artifact_ptr=?2, updated_at=?3 WHERE scope=?1",
        params![scope, artifact_ptr, now],
    )?;
    if changed == 1 {
        write_audit_scope(&tx, "update_artifact_ptr", scope)?;
        tx.commit()?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Close a scope (AC-1). Clears the `lease_session` read cache, sets `closed_at`
/// to now (epoch-ms), and stamps `updated_at`. Does NOT delete the row — the closed
/// scope stays as an audit trail. The `WHERE ... AND closed_at IS NULL` guard +
/// `changes()==1` check make a double-close a typed no-op-error, not a silent
/// success (single-writer discipline, ARCHITECTURE §1.2). SW-WV-A5's
/// `deprovision_child` calls THIS handler rather than writing `closed_at` directly
/// (ONE-WRITER rule). Returns the `closed_at` stamp on success. Audit co-committed
/// (AC-9).
pub fn wv_scope_close(store: &WeftStore, scope: &str) -> WvWriteResult<i64> {
    let now = now_epoch_ms();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_scopes SET lease_session=NULL, closed_at=?2, updated_at=?2 \
         WHERE scope=?1 AND closed_at IS NULL",
        params![scope, now],
    )?;
    if changed == 1 {
        write_audit_scope(&tx, "close", scope)?;
        tx.commit()?;
        Ok(now)
    } else {
        // No open scope matched — the scope is unknown or already closed. A
        // double-close is a refusal, not a silent no-op (single-writer guard).
        Err(reject(
            "no open scope matched (unknown scope or already closed)",
        ))
    }
}

/// Read a single scope row (AC-1, AC-4). Read-only; the restart contract's
/// query-don't-carry read path — always hits the DB, never a cached copy. Returns
/// `None` when the scope does not exist.
pub fn wv_scope_get(store: &WeftStore, scope: &str) -> WvWriteResult<Option<ScopeRow>> {
    let conn = store.conn()?;
    let row = conn
        .query_row(
            "SELECT scope, parent_scope, artifact_ptr, lease_session, owner_session, \
                    created_at, updated_at, closed_at \
             FROM wv_scopes WHERE scope=?1",
            params![scope],
            |r| {
                Ok(ScopeRow {
                    scope: r.get(0)?,
                    parent_scope: r.get(1)?,
                    artifact_ptr: r.get(2)?,
                    lease_session: r.get(3)?,
                    owner_session: r.get(4)?,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                    closed_at: r.get(7)?,
                })
            },
        )
        .ok();
    Ok(row)
}

// ---------- wv_watches (AC-2, AC-3, AC-5, AC-9) ----------

/// Upsert a watch on the composite PK `(scope, kind, ref)` (AC-2). Validates
/// `lifecycle` against [`WatchLifecycle`] BEFORE the SQL — the Rust enum is the
/// gate, so an unknown lifecycle is rejected at the handler (AC-2). Uses SQLite
/// 3.24+ `ON CONFLICT ... DO UPDATE` (NOT `INSERT OR REPLACE`, which would
/// delete+reinsert and lose any future column). A second upsert on the same key
/// updates `lifecycle` + `updated_at`, never inserts a duplicate. Audit
/// co-committed (AC-9).
pub fn wv_watch_upsert(
    store: &WeftStore,
    scope: &str,
    kind: &str,
    reference: &str,
    lifecycle: &str,
) -> WvWriteResult<()> {
    if is_blank(scope) || is_blank(kind) || is_blank(reference) {
        return Err(reject("scope, kind, and ref must all be non-blank"));
    }
    let lifecycle = WatchLifecycle::from_wire(lifecycle)
        .ok_or_else(|| reject("lifecycle must be one of: open | quiet | closed"))?;
    let now = now_epoch_ms();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO wv_watches (scope, kind, ref, lifecycle, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(scope, kind, ref) DO UPDATE SET \
          lifecycle=excluded.lifecycle, updated_at=excluded.updated_at",
        params![scope, kind, reference, lifecycle.as_str(), now],
    )?;
    write_audit_watch(&tx, "upsert", scope, kind, reference)?;
    tx.commit()?;
    Ok(())
}

/// Close a watch with positive evidence (AC-5). REQUIRES a valid [`CloseReason`] —
/// a bare `closed` write with a missing/blank/unknown reason is rejected at the
/// handler (silence never closes a watch). Sets `lifecycle='closed'` + stamps
/// `updated_at`. Returns `false` when no watch matched the key (a no-op), `true`
/// when one was closed. Audit co-committed (AC-9). The reason itself is not a
/// stored column in B5's schema (the schema is the minimal replacement-map shape);
/// it is the handler-level gate that enforces the positive-evidence rule.
pub fn wv_watch_close(
    store: &WeftStore,
    scope: &str,
    kind: &str,
    reference: &str,
    close_reason: &str,
) -> WvWriteResult<bool> {
    if is_blank(close_reason) {
        return Err(reject(
            "close_reason is required (silence never closes a watch); \
             one of: explicit_command | scope_closed | tail_message",
        ));
    }
    let _reason = CloseReason::from_wire(close_reason).ok_or_else(|| {
        reject("close_reason must be one of: explicit_command | scope_closed | tail_message")
    })?;
    let now = now_epoch_ms();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_watches SET lifecycle='closed', updated_at=?4 \
         WHERE scope=?1 AND kind=?2 AND ref=?3",
        params![scope, kind, reference, now],
    )?;
    if changed == 1 {
        write_audit_watch(&tx, "close", scope, kind, reference)?;
        tx.commit()?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Mark a watch `quiet` (AC-5) — idle past the quiet threshold. Distinct from
/// `close`: `quiet` is a slower-cadence state, never auto-dropped, and does NOT set
/// `closed`. Sets `lifecycle='quiet'` + stamps `updated_at`. Returns `false` on a
/// no-op, `true` when one was marked. Audit co-committed (AC-9).
pub fn wv_watch_mark_quiet(
    store: &WeftStore,
    scope: &str,
    kind: &str,
    reference: &str,
) -> WvWriteResult<bool> {
    let now = now_epoch_ms();
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute(
        "UPDATE wv_watches SET lifecycle='quiet', updated_at=?4 \
         WHERE scope=?1 AND kind=?2 AND ref=?3",
        params![scope, kind, reference, now],
    )?;
    if changed == 1 {
        write_audit_watch(&tx, "mark_quiet", scope, kind, reference)?;
        tx.commit()?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// The default quiet threshold in days (AC-5) — a watch idle this long is a
/// candidate for `quiet`. Configurable by the caller; the handler does not
/// auto-transition on it (the triage sweep, D1, decides). Surfaced here as the
/// single source of the documented default.
pub const WV_WATCH_QUIET_THRESHOLD_DAYS: u32 = 3;

/// List a scope's watches (AC-2, AC-4) — the D1-triage read path. When
/// `include_closed` is false the query filters out `lifecycle='closed'` (the sweep
/// ignores closed watches), using the `idx_wv_watches_scope(scope, lifecycle)`
/// index. Read-only; ordered by `(kind, ref)` for a stable result.
pub fn wv_watch_list(
    store: &WeftStore,
    scope: &str,
    include_closed: bool,
) -> WvWriteResult<Vec<WatchRow>> {
    let conn = store.conn()?;
    let sql = if include_closed {
        "SELECT scope, kind, ref, lifecycle, updated_at \
         FROM wv_watches WHERE scope=?1 ORDER BY kind, ref"
    } else {
        "SELECT scope, kind, ref, lifecycle, updated_at \
         FROM wv_watches WHERE scope=?1 AND lifecycle<>'closed' ORDER BY kind, ref"
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map(params![scope], |r| {
            Ok(WatchRow {
                scope: r.get(0)?,
                kind: r.get(1)?,
                reference: r.get(2)?,
                lifecycle: r.get(3)?,
                updated_at: r.get(4)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

// ---------- wv_signals (SW-WV-B4) ----------

/// SW-WV-B4 (AC-1): insert a worker done-signal into `wv_signals` — the durable
/// consume-once registry pattern.
/// Written on `signal_done` (the authoritative done edge) and read by the close
/// predicate's success-signal check; never removed on process exit.
///
/// `status` must be one of `success` | `failed` | `needs-attention` (the
/// `signal_done` boundary already validated it; a non-signal status is rejected
/// here as a belt). Audit row co-committed in the same `IMMEDIATE` transaction
/// (AC-1 / ARCHITECTURE §1.6): the `wv_signals` insert and its `wv_write_log`
/// append run inside one `wv_auto_append` transaction, so B6 atomicity and B4
/// non-blocking both hold (the whole call runs off-thread inside the hook's
/// `spawn_blocking` closure). Returns the AUTOINCREMENT id.
pub fn wv_signal_insert(
    store: &WeftStore,
    worker: &str,
    task_id: &str,
    session_id: &str,
    status: &str,
    dispatch: Option<&str>,
    packet_refs: Option<&str>,
) -> WvWriteResult<i64> {
    if !matches!(status, "success" | "failed" | "needs-attention") {
        return Err(reject(
            "status must be one of: success | failed | needs-attention",
        ));
    }
    if is_blank(worker) {
        return Err(reject("worker must not be blank"));
    }
    if is_blank(session_id) {
        return Err(reject("session_id must not be blank"));
    }
    let mut conn = store.conn()?;
    let id = wv_auto_append(
        &mut conn,
        "INSERT INTO wv_signals \
         (worker, task_id, session_id, status, dispatch, packet_refs, created) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        &[
            &worker,
            &task_id,
            &session_id,
            &status,
            &dispatch,
            &packet_refs,
            &now_epoch_secs(),
        ],
        "wv_signals",
    )?;
    Ok(id)
}

/// SW-WV-B4 (AC-1): the `signal_done` `wv_signals-insert` hook's store entry
/// point. The A3 hook registry threads only the completing `session_id`, so this
/// resolves the worker's roster row by `session_id` (the SAME lookup key the
/// roster-update hook uses) to fill `worker` (roster `name`) and `task_id` (roster
/// `story_id`, else the session id when no live worker row exists), then inserts
/// via [`wv_signal_insert`]. `status`/`packet_refs` come from the `signal_done`
/// params (captured by the hook closure). Returns `Ok(None)` when the store has no
/// roster row for the session — a non-worker completion is a graceful no-op, so a
/// bare shell / probe session never accumulates a stale signal row (AC-1 graceful
/// degrade). Runs entirely inside the hook's off-thread `spawn_blocking` closure.
pub fn wv_signal_insert_by_session(
    store: &WeftStore,
    session_id: &str,
    status: &str,
    summary: Option<&str>,
) -> WvWriteResult<Option<i64>> {
    wv_signal_insert_for_worker(store, session_id, session_id, status, summary)
}

pub(crate) fn wv_signal_insert_for_worker(
    store: &WeftStore,
    worker_key: &str,
    session_id: &str,
    status: &str,
    summary: Option<&str>,
) -> WvWriteResult<Option<i64>> {
    if is_blank(session_id) {
        return Ok(None);
    }
    // Resolve the worker (roster `name`) + its `story_id`/`scope` for this session.
    // A non-worker completion (no roster row) is a graceful skip — no signal row.
    let conn = store.conn()?;
    let resolved: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT name, story_id, scope FROM wv_roster WHERE session_id=?1",
            params![worker_key],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .ok();
    drop(conn);
    let Some((worker, story_id, scope)) = resolved else {
        return Ok(None);
    };
    // `task_id` is the worker's story when known, else the completing session id
    // (a worker with no story stamp still gets a drainable, targeted key).
    let task_id = story_id.unwrap_or_else(|| session_id.to_string());
    let id = wv_signal_insert(
        store,
        &worker,
        &task_id,
        session_id,
        status,
        scope.as_deref(),
        summary,
    )?;
    Ok(Some(id))
}

// ---------- wv_coord (SW-WV-B4 — durable, append-only, one owner per contract) ----------

/// SW-WV-B4: the typed coord-access refusals. Kept as a dedicated enum (mirroring
/// [`HarvestRefusal`]) so the caller (a future SW-WV-C1 / D4 MCP verb) can branch
/// on the exact refusal — a peer writing a contract it does not own
/// ([`CoordRefusal::NotOwner`]) vs. a worker reading a contract its dispatch does
/// not list ([`CoordRefusal::NotAuthorized`]) — never conflating either with a
/// storage error. Both map into [`WvWriteError::Invalid`] so the boundary surfaces
/// them as `invalid_params`, not a store fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordRefusal {
    /// A `wv_coord_write` was refused because the contract already exists with a
    /// DIFFERENT `owner_worker` — a peer may never overwrite a row it does not own
    /// (AC-2). The row is unchanged.
    NotOwner,
    /// A `wv_coord_read` was refused because the calling worker is not listed in
    /// the dispatch's `peers:` for the contract owner (AC-4). No data is returned;
    /// no SQL is issued.
    NotAuthorized,
}

impl CoordRefusal {
    fn message(self) -> &'static str {
        match self {
            CoordRefusal::NotOwner => {
                "not owner — this contract is owned by another worker; a peer may read it but never write it"
            }
            CoordRefusal::NotAuthorized => {
                "not authorized — the calling worker's dispatch does not list this contract's owner under peers"
            }
        }
    }
}

impl From<CoordRefusal> for WvWriteError {
    fn from(r: CoordRefusal) -> Self {
        WvWriteError::Invalid(WvValidation(r.message().to_string()))
    }
}

/// SW-WV-B4 (AC-2, AC-3): write (create-or-update) a coord contract, one owner per
/// contract. Opens one `IMMEDIATE` transaction; if a row already exists for
/// `contract` under a DIFFERENT `owner_worker`, returns [`CoordRefusal::NotOwner`]
/// (the row is unchanged). Otherwise it either INSERTs a fresh row (stamping
/// `created`) or, when the same owner already holds the contract, UPDATEs `body` +
/// `updated` on the SAME row (id and `created` preserved). Co-commits the
/// `wv_write_log` audit row (`operation='coord_write'`) in the same transaction.
///
/// A same-owner update is a real `UPDATE`, NOT an `INSERT OR REPLACE`: REPLACE would
/// delete-and-reinsert the row (churning its id), and coord rows are durable and
/// never deleted (AC-2) — an update mutates the existing row in place. The guarded
/// `UPDATE ... WHERE contract=? AND owner_worker=?` + `changes()==1` keeps the write
/// single-owner even under a concurrent owner flip (a lost guard is a typed
/// `IllegalState`, never a silent no-op — ARCHITECTURE §1.2).
///
/// `coord/` is durable, append-only-by-owner, and read-only to peers — never a
/// message and never an approval (AC-3). This handler is an internal Rust call /
/// Tauri command in this story; MCP exposure (with the dispatch-record `peers:`
/// context) rides on SW-WV-C1 / D4.
pub fn wv_coord_write(
    store: &WeftStore,
    contract: &str,
    owner_dispatch: &str,
    owner_worker: &str,
    body: &str,
) -> WvWriteResult<()> {
    if is_blank(contract) {
        return Err(reject("contract must not be blank"));
    }
    if is_blank(owner_worker) {
        return Err(reject("owner_worker must not be blank"));
    }
    let mut conn = store.conn()?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // One-owner guard: a pre-existing row under a different worker refuses the write
    // (AC-2). Read the current owner inside the same tx.
    let existing_owner: Option<String> = tx
        .query_row(
            "SELECT owner_worker FROM wv_coord WHERE contract=?1",
            params![contract],
            |r| r.get::<_, String>(0),
        )
        .ok();
    let now = now_epoch_secs();
    let id = match existing_owner {
        Some(owner) if owner != owner_worker => {
            // Refuse — a peer may not overwrite a contract it does not own. Drop the
            // tx (no write, no audit row).
            return Err(CoordRefusal::NotOwner.into());
        }
        // Same-owner update — mutate `body`/`updated` on the existing row in place
        // (id + created preserved; the row is never deleted).
        Some(_) => {
            let changed = tx.execute(
                "UPDATE wv_coord SET owner_dispatch=?2, body=?3, updated=?4 \
                 WHERE contract=?1 AND owner_worker=?5",
                params![contract, owner_dispatch, body, now, owner_worker],
            )?;
            if changed != 1 {
                // A concurrent owner flip won the guard — never a silent no-op.
                return Err(WvWriteError::Store(WeftStoreError::IllegalState {
                    table: "wv_coord",
                    id: 0,
                    attempted: "coord_write",
                }));
            }
            tx.query_row(
                "SELECT id FROM wv_coord WHERE contract=?1",
                params![contract],
                |r| r.get::<_, i64>(0),
            )?
        }
        // Fresh contract — INSERT a new AUTOINCREMENT row stamping `created`.
        None => {
            tx.execute(
                "INSERT INTO wv_coord \
                 (contract, owner_dispatch, owner_worker, body, created, updated) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![contract, owner_dispatch, owner_worker, body, now],
            )?;
            tx.last_insert_rowid()
        }
    };
    write_audit(&tx, "wv_coord", "coord_write", id)?;
    tx.commit()?;
    Ok(())
}

/// SW-WV-B4 (AC-2, AC-3, AC-4): read the current body of a coord contract on the
/// peer read lane. Verifies `calling_worker` appears in `dispatch_peers` BEFORE
/// issuing any SQL; a caller not listed is refused with
/// [`CoordRefusal::NotAuthorized`] and no data is returned (the peer-lane bound —
/// AC-4). Otherwise `SELECT`s the body and returns it, or `None` when the contract
/// does not exist.
///
/// Read-only: `wv_coord_read` carries no delivery semantics — it returns state,
/// never a message (AC-3). The `dispatch_peers` list is supplied by the caller
/// (the dispatch-record context SW-WV-C1 wires); this story enforces the bound at
/// the handler.
pub fn wv_coord_read(
    store: &WeftStore,
    contract: &str,
    calling_worker: &str,
    dispatch_peers: &[String],
) -> WvWriteResult<Option<String>> {
    // Peer-lane bound (AC-4): the calling worker must be listed in its dispatch's
    // `peers:` before any query. Refuse WITHOUT issuing SQL if it is not.
    if !dispatch_peers.iter().any(|p| p == calling_worker) {
        return Err(CoordRefusal::NotAuthorized.into());
    }
    let conn = store.conn()?;
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM wv_coord WHERE contract=?1",
            params![contract],
            |r| r.get(0),
        )
        .ok();
    Ok(body)
}

// ---------- wv peer read lane (SW-WV-D4 — bounded worker-to-worker reads) ----------

/// The peer list of a worker's active queue item. A dispatched worker is a
/// subject selected by dispatch_handle; a worker claiming its own item is matched
/// through its trusted roster session. Claim ownership never doubles as a worker
/// name/run token. A missing peer list closes the lane.
pub fn wv_dispatch_peers_for_worker(store: &WeftStore, worker_name: &str) -> Vec<String> {
    let Ok(conn) = store.conn() else {
        return Vec::new();
    };
    let raw: Option<String> = conn
        .query_row(
            "SELECT dispatch_peers FROM wv_queue \
             WHERE state IN ('claimed','dispatched') AND (dispatch_handle=?1 OR \
             claimed_by=(SELECT session_id FROM wv_roster WHERE name=?1)) LIMIT 1",
            params![worker_name],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    match raw {
        Some(json) => serde_json::from_str::<Vec<String>>(&json).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// SW-WV-D4 (AC-3): the on-disk worktree root (`cwd`) recorded in `wv_roster` for a
/// named peer — the base a peer file read resolves against. Returns `None` when the
/// peer has no roster row or a `NULL` `cwd` (surfaced as `peer-not-found` by the
/// handler, never a panic). Read-only.
pub fn wv_roster_cwd_for_worker(
    store: &WeftStore,
    name: &str,
) -> WeftStoreResult<Option<std::path::PathBuf>> {
    let conn = store.conn()?;
    let cwd: Option<String> = conn
        .query_row(
            "SELECT cwd FROM wv_roster WHERE name=?1 LIMIT 1",
            params![name],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    Ok(cwd.map(std::path::PathBuf::from))
}

/// The scope of the worker's active item, correlated independently of the
/// verified session that owns the claim. A missing scope is refused.
pub fn wv_queue_scope_for_worker(store: &WeftStore, worker_name: &str) -> WeftStoreResult<String> {
    let conn = store.conn()?;
    let scope: Option<String> = conn
        .query_row(
            "SELECT scope FROM wv_queue \
             WHERE state IN ('claimed','dispatched') AND (dispatch_handle=?1 OR \
             claimed_by=(SELECT session_id FROM wv_roster WHERE name=?1)) LIMIT 1",
            params![worker_name],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    // No claimed row (or a NULL scope) for this worker — surfaced as an error the
    // handler maps to the default `"root"` escalation scope (AC-4).
    scope.ok_or(WeftStoreError::IllegalState {
        table: "wv_queue",
        id: 0,
        attempted: "scope_for_worker",
    })
}

/// SW-WV-D4 (AC-4): the sole write a worker may perform that crosses into another
/// worker's domain — insert a `pending`, agent-sourced `wv_queue` row the controller's
/// queue drain picks up. It NEVER modifies another worker's state: it only appends one
/// new escalation row, with no claim and no drive.
///
/// The row is stamped `source='agent'`, `state='pending'`, `claimed_by=''` (an
/// unclaimed, controller-owned item), `scope` = the caller's resolved scope, `note` =
/// the (already-truncated) one-line reason, and `refs` = the JSON blob naming the
/// involved workers. `dispatch_peers` is `NULL` — an escalation is not a dispatch.
/// Mints the id + co-commits the `wv_write_log` audit row via [`wv_append`] (the
/// sanctioned append path). Returns the new row id.
pub fn wv_escalate_insert(
    store: &WeftStore,
    scope: &str,
    note: &str,
    refs_json: &str,
    now: i64,
) -> WeftStoreResult<i64> {
    let mut conn = store.conn()?;
    wv_append(
        &mut conn,
        "wv_queue",
        "INSERT INTO wv_queue \
         (id, source, state, claimed_by, scope, created, updated, refs, note, dispatch_peers) \
         VALUES (?1, 'agent', 'pending', '', ?2, ?3, ?3, ?4, ?5, NULL)",
        &[&scope, &now, &refs_json, &note],
    )
}

#[cfg(test)]
mod a3_tests;
#[cfg(test)]
mod a5_tests;
#[cfg(test)]
mod a6_tests;
#[cfg(test)]
mod b3_tests;
#[cfg(test)]
mod b5_tests;
#[cfg(test)]
mod c5_tests;
#[cfg(test)]
mod c6_tests;
#[cfg(test)]
mod d4_tests;
#[cfg(test)]
mod e1_tests;
#[cfg(test)]
mod e2_tests;
#[cfg(test)]
mod e3_tests;
#[cfg(test)]
mod f1_tests;
#[cfg(test)]
mod f4_tests;
#[cfg(test)]
mod f5_tests;
#[cfg(test)]
mod f6_tests;
#[cfg(test)]
mod signals_coord_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod wa_d4_tests;
