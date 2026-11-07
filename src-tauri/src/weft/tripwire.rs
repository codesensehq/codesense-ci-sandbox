// SPDX-License-Identifier: MIT
//! SW-WV-C2 — the dispatch tripwire: a mechanical, after-the-fact-checkable rule
//! that decides whether a queue item belongs with the controller (`Keep`) or must
//! become a dispatch to a worker (`Delegate`).
//!
//! This module holds the PURE decision logic: [`evaluate_tripwire`] takes a
//! [`TripwireInput`] built from already-observed evidence (git stat, transcript
//! cycle count, task-description flags) and returns a [`TripwireVerdict`]. It
//! performs no I/O and is never `async` — no git subprocess, no filesystem, no DB.
//! Every evidence field is supplied by the caller from state it already holds, so
//! every AC case is a plain `#[test]` with zero setup (the `not_async` test is the
//! compile-time proof the function carries no `.await`).
//!
//! "Keep vs. delegate" is thereby never an in-the-moment judgment call: the rule
//! fires on five row conditions, and the verdict is stamped onto the `wv_queue`
//! row (`tripwire_verdict`) at claim time so the decision is auditable after the
//! fact (ARCHITECTURE — the dispatch tripwire; `_lab/weft/supervised-autonomy-playbook.md`).

/// The soft threshold (in added lines) above which Row 2 fires. A single-file edit
/// that adds more than this many lines is worker-shaped even without a create.
/// Held as a constant for now; tunable config is out of scope (SW-WV-E).
pub const ADDED_LINES_THRESHOLD: u64 = 50;

/// The verify-and-fix cycle count at which Row 5 fires. Three completed cycles is
/// the line — a fourth run → edit iteration is worker-shaped work the controller
/// must hand off. Constant for now (tunable config is SW-WV-E / A5).
pub const VERIFY_FIX_CYCLE_THRESHOLD: u32 = 3;

/// The local-tier ledger scopes the controller ALWAYS owns (AC-6). An item whose
/// `scope` is one of these is `Keep` unconditionally — these are the `wv_*` ledger
/// writes that are the controller's own bookkeeping and are never worker work.
pub const LOCAL_TIER_SCOPES: &[&str] = &[
    "wv_queue",
    "wv_roster",
    "wv_decisions",
    "wv_approvals",
    "wv_attention",
];

/// One touched path plus the git status that produced it — the after-the-fact
/// evidence for Rows 1–3. `is_create` is the git `A` (added) status; `is_ledger`
/// marks a path under the `wv_*` local-tier ledger tree (excluded from the Row-1
/// "outside local-tier" count and from the Row-3 persistent-code check).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedPath {
    /// The repo-relative path the worker/controller touched.
    pub path: String,
    /// The path is a git `A` (create) — feeds Row 2.
    pub is_create: bool,
    /// The path is a `wv_*` ledger write (local-tier). Excluded from the Row-1
    /// count and Row-3 persistent-code check — the ledger is never worker work.
    pub is_ledger: bool,
}

/// The whole after-the-fact evidence packet the caller assembles for one queue
/// item. Every field is already-observed state — the tripwire reads it, it never
/// gathers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TripwireInput {
    /// The queue item's `scope` — checked against [`LOCAL_TIER_SCOPES`] first (AC-6).
    pub scope: Option<String>,
    /// The paths the work touched, with their git status. Row 1 counts the
    /// non-ledger ones; Rows 2 and 3 inspect them.
    pub touched_paths: Vec<TouchedPath>,
    /// Total added lines across the change (git `--numstat`). Feeds Row 2.
    pub added_lines: u64,
    /// The work's lifecycle. Row 3 only fires when this is NOT `one_shot` — a
    /// one-shot task that touches code is not persistent-code work.
    pub lifecycle: Lifecycle,
    /// The task description asks for a diff-read / `git show` / grep-fan-out /
    /// repo-sweep action. Set by the caller from the dispatch text. Feeds Row 4.
    pub requires_diff_read: bool,
    /// The verify-and-fix cycle count the controller tracks per queue item
    /// mid-flight (`wv_queue.verify_fix_cycles`). Feeds Row 5.
    pub verify_fix_cycles: u32,
}

/// The work item's lifecycle. Only `OneShot` exempts a code-touching item from
/// Row 3 (a throwaway probe that touches a `.rs` file is not persistent code).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// A throwaway, run-once action — exempt from Row 3.
    OneShot,
    /// Anything that persists (the default when the caller has no better signal).
    Persistent,
}

/// The tripwire outcome — who does the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TripwireOutcome {
    /// The controller keeps the item (stamped `claimed_by = 'controller'`).
    Keep,
    /// The item is worker-shaped and must be dispatched.
    Delegate,
}

/// The full verdict: the outcome plus which row fired (1–5), or `None` when the
/// item is kept (either exempt or no row tripped). `fired_row` is the auditable
/// "why" preserved into the key ledger on resolve (AC-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TripwireVerdict {
    pub outcome: TripwireOutcome,
    /// The 1-based row that fired (`Some(1..=5)`), or `None` for a `Keep`.
    pub fired_row: Option<u8>,
}

impl TripwireVerdict {
    /// The verdict string as stored in `wv_queue.tripwire_verdict`
    /// (`keep` | `delegate`) — matches the column's CHECK constraint.
    pub fn verdict_str(&self) -> &'static str {
        match self.outcome {
            TripwireOutcome::Keep => "keep",
            TripwireOutcome::Delegate => "delegate",
        }
    }
}

/// The persistent-code extensions (Row 3): executable code that survives the run.
/// `.yaml`/`.yml` are included per AC-1 (they are persisted config outside the
/// store); `weft.sqlite3` is the store itself and is not code — it is excluded by
/// the caller marking those touches `is_ledger` (a `wv_*` write is never a file
/// path here).
const PERSISTENT_CODE_EXTS: &[&str] = &["rs", "ts", "tsx", "js", "py", "sh", "yaml", "yml"];

/// Return the lowercased extension of a path (no leading dot), or `None`.
fn extension_of(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    // A leading-dot filename (`.gitignore`) has no extension for this purpose.
    let (stem, ext) = name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

/// Is this a persistent-code path (Row 3 candidate)? A ledger write is never code.
fn is_persistent_code_path(tp: &TouchedPath) -> bool {
    if tp.is_ledger {
        return false;
    }
    extension_of(&tp.path)
        .map(|ext| PERSISTENT_CODE_EXTS.contains(&ext.as_str()))
        .unwrap_or(false)
}

/// Evaluate the five-row dispatch tripwire. PURE — no I/O, no `async` (AC-1).
///
/// The local-tier exemption (AC-6) is a single pre-check BEFORE any row: an item
/// scoped to a `wv_*` ledger the controller always owns returns `Keep`
/// unconditionally, regardless of file count or line count.
///
/// Otherwise the rows are checked in order and the FIRST to fire wins (its 1-based
/// index is recorded as `fired_row`). If no row fires the item is kept:
/// - Row 1 — more than one touched path OUTSIDE the local-tier ledger tree.
/// - Row 2 — any touched path is a create (`A`), or `added_lines` exceeds the
///   soft threshold ([`ADDED_LINES_THRESHOLD`]).
/// - Row 3 — any touched path is persistent executable code AND the work's
///   `lifecycle` is not `OneShot`.
/// - Row 4 — the task requires a diff-read / repo-sweep (`requires_diff_read`).
/// - Row 5 — the verify-and-fix cycle count has reached the threshold
///   ([`VERIFY_FIX_CYCLE_THRESHOLD`]).
pub fn evaluate_tripwire(input: &TripwireInput) -> TripwireVerdict {
    // AC-6: local-tier exemption pre-check — the controller's own ledger writes are
    // never worker work, no matter how many files or lines they touch.
    if let Some(scope) = input.scope.as_deref() {
        if LOCAL_TIER_SCOPES.contains(&scope) {
            return keep();
        }
    }

    // Row 1 — multi-file OUTSIDE the local-tier ledger. Ledger touches don't count
    // toward the delegate threshold (AC-1: "paths not under the wv_* ledger tree").
    let non_ledger_paths = input
        .touched_paths
        .iter()
        .filter(|tp| !tp.is_ledger)
        .count();
    if non_ledger_paths > 1 {
        return delegate(1);
    }

    // Row 2 — a create, or more than the soft line threshold.
    let has_create = input.touched_paths.iter().any(|tp| tp.is_create);
    if has_create || input.added_lines > ADDED_LINES_THRESHOLD {
        return delegate(2);
    }

    // Row 3 — persistent executable code AND lifecycle != one_shot.
    if input.lifecycle != Lifecycle::OneShot
        && input.touched_paths.iter().any(is_persistent_code_path)
    {
        return delegate(3);
    }

    // Row 4 — the task carries a diff-read / repo-sweep action.
    if input.requires_diff_read {
        return delegate(4);
    }

    // Row 5 — the verify-and-fix cycle count crossed the line mid-flight.
    if input.verify_fix_cycles >= VERIFY_FIX_CYCLE_THRESHOLD {
        return delegate(5);
    }

    keep()
}

/// The mid-flight (Row-5-only) re-evaluation used by the `wv_report_cycle` handler
/// after it increments `verify_fix_cycles` (AC-4). Returns `true` when the new
/// cycle count has crossed the threshold and the remainder must be delegated.
/// This is deliberately the SAME threshold constant Row 5 uses, so the mid-flight
/// path and the claim-time path can never diverge.
pub fn cycle_crosses_threshold(verify_fix_cycles: u32) -> bool {
    verify_fix_cycles >= VERIFY_FIX_CYCLE_THRESHOLD
}

/// A kept verdict (no row fired).
fn keep() -> TripwireVerdict {
    TripwireVerdict {
        outcome: TripwireOutcome::Keep,
        fired_row: None,
    }
}

/// A delegate verdict for the given 1-based row.
fn delegate(row: u8) -> TripwireVerdict {
    TripwireVerdict {
        outcome: TripwireOutcome::Delegate,
        fired_row: Some(row),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A base `Keep`-shaped input: single non-code path, no create, few lines,
    /// persistent lifecycle, no diff-read, zero cycles. Each test perturbs ONE
    /// field so the row under test is the only thing that fires.
    fn base() -> TripwireInput {
        TripwireInput {
            scope: Some("root".into()),
            touched_paths: vec![TouchedPath {
                path: "docs/notes.md".into(),
                is_create: false,
                is_ledger: false,
            }],
            added_lines: 10,
            lifecycle: Lifecycle::Persistent,
            requires_diff_read: false,
            verify_fix_cycles: 0,
        }
    }

    fn code(path: &str, is_create: bool) -> TouchedPath {
        TouchedPath {
            path: path.into(),
            is_create,
            is_ledger: false,
        }
    }

    // AC-1 Row 1 — fires on two tracked (non-ledger) paths; not on one.
    #[test]
    fn row1_fires_on_two_paths_not_on_one() {
        let mut input = base();
        input.touched_paths = vec![code("a.md", false), code("b.md", false)];
        let v = evaluate_tripwire(&input);
        assert_eq!(v.outcome, TripwireOutcome::Delegate);
        assert_eq!(v.fired_row, Some(1));

        // One path — no Row-1 fire (falls through to Keep with these fields).
        let mut one = base();
        one.touched_paths = vec![code("a.md", false)];
        assert_eq!(evaluate_tripwire(&one).outcome, TripwireOutcome::Keep);
    }

    // AC-1 Row 1 — ledger paths don't count toward the multi-file threshold.
    #[test]
    fn row1_ignores_ledger_paths() {
        let mut input = base();
        input.touched_paths = vec![
            code("a.md", false),
            TouchedPath {
                path: "wv_queue".into(),
                is_create: false,
                is_ledger: true,
            },
        ];
        // One non-ledger path + one ledger path → not multi-file.
        assert_eq!(evaluate_tripwire(&input).outcome, TripwireOutcome::Keep);
    }

    // AC-1 Row 2 — fires on a create; fires above the line threshold; not at 49.
    #[test]
    fn row2_fires_on_create_and_over_threshold() {
        let mut created = base();
        created.touched_paths = vec![code("new.md", true)];
        let v = evaluate_tripwire(&created);
        assert_eq!(v.fired_row, Some(2));

        // added_lines just over the threshold (51 > 50) → fires.
        let mut over = base();
        over.added_lines = ADDED_LINES_THRESHOLD + 1;
        assert_eq!(evaluate_tripwire(&over).fired_row, Some(2));

        // 49 lines (below 50) → does not fire on Row 2 (base is otherwise Keep).
        let mut under = base();
        under.added_lines = 49;
        assert_eq!(evaluate_tripwire(&under).outcome, TripwireOutcome::Keep);

        // Exactly at the threshold (50) does NOT fire (`> threshold`).
        let mut at = base();
        at.added_lines = ADDED_LINES_THRESHOLD;
        assert_eq!(evaluate_tripwire(&at).outcome, TripwireOutcome::Keep);
    }

    // AC-1 Row 3 — fires on a .rs path with lifecycle != one_shot; not one_shot.
    #[test]
    fn row3_fires_on_persistent_code_not_one_shot() {
        let mut persistent = base();
        persistent.touched_paths = vec![code("src/lib.rs", false)];
        assert_eq!(evaluate_tripwire(&persistent).fired_row, Some(3));

        // Same code path but one_shot lifecycle → Row 3 does not fire → Keep.
        let mut one_shot = base();
        one_shot.touched_paths = vec![code("src/lib.rs", false)];
        one_shot.lifecycle = Lifecycle::OneShot;
        assert_eq!(evaluate_tripwire(&one_shot).outcome, TripwireOutcome::Keep);
    }

    // AC-1 Row 4 — fires when requires_diff_read; not otherwise.
    #[test]
    fn row4_fires_on_diff_read() {
        let mut input = base();
        input.requires_diff_read = true;
        assert_eq!(evaluate_tripwire(&input).fired_row, Some(4));

        assert_eq!(evaluate_tripwire(&base()).outcome, TripwireOutcome::Keep);
    }

    // AC-1 / AC-4 Row 5 — fires at cycles >= 3; not at 2.
    #[test]
    fn row5_fires_at_threshold_not_below() {
        let mut at = base();
        at.verify_fix_cycles = VERIFY_FIX_CYCLE_THRESHOLD;
        assert_eq!(evaluate_tripwire(&at).fired_row, Some(5));

        let mut below = base();
        below.verify_fix_cycles = VERIFY_FIX_CYCLE_THRESHOLD - 1;
        assert_eq!(evaluate_tripwire(&below).outcome, TripwireOutcome::Keep);

        // The mid-flight helper agrees with Row 5 at the boundary.
        assert!(cycle_crosses_threshold(VERIFY_FIX_CYCLE_THRESHOLD));
        assert!(!cycle_crosses_threshold(VERIFY_FIX_CYCLE_THRESHOLD - 1));
    }

    // AC-6 — a local-tier ledger scope returns Keep regardless of row conditions.
    #[test]
    fn local_tier_scope_exempt_regardless() {
        // An input that would fire EVERY row, but scoped to a ledger the controller
        // always owns → Keep, no fired_row.
        let input = TripwireInput {
            scope: Some("wv_queue".into()),
            touched_paths: vec![code("src/a.rs", true), code("src/b.rs", true)],
            added_lines: 10_000,
            lifecycle: Lifecycle::Persistent,
            requires_diff_read: true,
            verify_fix_cycles: 99,
        };
        let v = evaluate_tripwire(&input);
        assert_eq!(v.outcome, TripwireOutcome::Keep);
        assert_eq!(v.fired_row, None);

        // Every declared local-tier scope is exempt.
        for scope in LOCAL_TIER_SCOPES {
            let mut i = input.clone();
            i.scope = Some((*scope).into());
            assert_eq!(evaluate_tripwire(&i).outcome, TripwireOutcome::Keep);
        }
    }

    // Row precedence — a create (Row 2) on a single path fires Row 2, not Row 3,
    // because Row 2 is checked first (the lowest-numbered firing row wins).
    #[test]
    fn lowest_row_wins() {
        let mut input = base();
        input.touched_paths = vec![code("src/new.rs", true)];
        // Both Row 2 (create) and Row 3 (persistent .rs) would fire; Row 2 wins.
        assert_eq!(evaluate_tripwire(&input).fired_row, Some(2));
    }

    // verdict_str maps the outcome to the column's CHECK values.
    #[test]
    fn verdict_str_matches_column_check() {
        assert_eq!(keep().verdict_str(), "keep");
        assert_eq!(delegate(1).verdict_str(), "delegate");
    }

    // AC-7 — `evaluate_tripwire` is synchronous: this non-async `#[test]` calls it
    // and binds the result with no `.await`. Compile-time proof the function is not
    // async (no `static_assertions` in this codebase — the plain non-async call is
    // the canonical proof).
    #[test]
    fn evaluate_tripwire_is_not_async() {
        let verdict: TripwireVerdict = evaluate_tripwire(&base());
        assert_eq!(verdict.outcome, TripwireOutcome::Keep);
    }

    // Extension helper: no false-positive on a leading-dot filename or no extension.
    #[test]
    fn extension_helper_edge_cases() {
        assert_eq!(extension_of("src/lib.rs").as_deref(), Some("rs"));
        assert_eq!(extension_of("Makefile"), None);
        assert_eq!(extension_of(".gitignore"), None);
        assert_eq!(extension_of("a/b/c.TS").as_deref(), Some("ts"));
    }
}
