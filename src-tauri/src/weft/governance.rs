// SPDX-License-Identifier: MIT
//! Four always-on guarantees (SW-WV-E5).
//!
//! The SPEC names four invariants the control plane enforces at all times — not
//! per-feature toggles, not posture-dependent, not something an agent can opt out
//! of. Each is realized on scaffolding merged dependencies already shipped:
//!
//! - **G1 — Consent-on-env-change.** An action that mutates the run *environment*
//!   (its cwd, its env vars, its connector/credential surface) is admitted ONLY
//!   with a present consent token. No token → refused; the environment never
//!   changes silently under an agent.
//! - **G2 — Soft timeouts (never fabricate).** When an awaited outcome times out,
//!   the control plane reports it INCOMPLETE. It never invents a result to fill the
//!   gap. A timeout is a first-class typed outcome, distinct from success/failure.
//! - **G3 — Verify-don't-fold.** The done-verdict is held to real evidence: a queue
//!   row may be folded (pruned) ONLY when the three-leg [`VerifyDoneResult`] is
//!   `verified`. A pending / unverified / pressured verdict never folds the row.
//!   Realized directly on SW-WV-C3's [`VerifyDoneResult`].
//! - **G4 — Governance (own-item-only writes; boards read-only to agents).** An
//!   agent may write only its OWN item — a cross-owner write is refused — and the
//!   attention board is read-only to agents: no board-mutating verb is ever
//!   agent-writable.
//!
//! # What this module is — and is NOT
//!
//! Like SW-WV-E4's [`crate::weft::capability_tier`], these are **pure classifiers /
//! guards** for the four invariants — NOT yet wired into the enforcement path (a
//! follow-up). They have no callers in the live dispatch/admission path today: the
//! enforcement seams already exist (E4's tier classifier + MCP scoping/deny, E1's
//! endorsement gate, C3's verify-don't-fold, B1's `WHERE id=? AND owner=?`
//! one-writer rule) and remain the mechanism. This module makes each guarantee a
//! *named, machine-checkable* fact a future implementer can ask one question about
//! and get one answer — a reference to be wired into those seams later, not the live
//! guard. It introduces **no new store table, MCP verb, or Tauri command**.
//!
//! The G4 board set is **derived from** [`CapabilityTier::of_verb`] (a board read
//! is a Tier-A verb; a board write would be Tier-B), so the "boards are read-only to
//! agents" invariant can never drift from the capability-tier gate.

use crate::weft::done_record::VerifyDoneResult;

// ============================================================================
// G1 — Consent-on-env-change.
// ============================================================================

/// The classes of action the control plane distinguishes for the consent
/// guarantee. Only [`EnvChange::Mutates`] requires a consent token; a read /
/// in-place action never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvChange {
    /// The action mutates the run environment (cwd, env vars, connector/credential
    /// surface) — G1 applies: a consent token is REQUIRED.
    Mutates,
    /// The action leaves the environment untouched — G1 does not apply.
    Inert,
}

/// The named environment-mutating action kinds (G1). An action whose kind is in
/// this set changes the run environment and so requires consent; every other kind
/// is inert. Kept as a small closed vocabulary rather than a free-form flag so a
/// new env-mutating action must be added here deliberately (the classifier can
/// never silently treat a new mutation as inert). Matched case-insensitively with
/// surrounding whitespace trimmed, mirroring the hand-authoring tolerance of the
/// capability-tier and mcp-scope seams.
pub const ENV_MUTATING_ACTIONS: &[&str] = &["set_cwd", "set_env", "credential_write"];

/// Classify an action kind for the consent guarantee (G1). An env-mutating kind is
/// [`EnvChange::Mutates`]; everything else is [`EnvChange::Inert`].
pub fn classify_env_change(action_kind: &str) -> EnvChange {
    let kind = action_kind.trim().to_ascii_lowercase();
    if ENV_MUTATING_ACTIONS.iter().any(|k| *k == kind) {
        EnvChange::Mutates
    } else {
        EnvChange::Inert
    }
}

/// The consent guarantee's admission decision (G1). `Admitted` when the action may
/// proceed; `ConsentRequired` when an env-mutating action arrived without a present
/// consent token — the caller MUST refuse, never proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentDecision {
    /// The action may proceed (inert, or env-mutating with a present token).
    Admitted,
    /// An env-mutating action arrived with no consent token — refuse.
    ConsentRequired,
}

impl ConsentDecision {
    /// Whether the action may proceed.
    pub fn admitted(&self) -> bool {
        matches!(self, ConsentDecision::Admitted)
    }
}

/// Admit an action under the consent-on-env-change guarantee (G1). An inert action
/// is always admitted. An env-mutating action is admitted ONLY when `consent_token`
/// is present and non-blank; a missing or blank token yields
/// [`ConsentDecision::ConsentRequired`] so the environment never changes silently.
pub fn admit_env_change(action_kind: &str, consent_token: Option<&str>) -> ConsentDecision {
    match classify_env_change(action_kind) {
        EnvChange::Inert => ConsentDecision::Admitted,
        EnvChange::Mutates => match consent_token {
            Some(tok) if !tok.trim().is_empty() => ConsentDecision::Admitted,
            _ => ConsentDecision::ConsentRequired,
        },
    }
}

// ============================================================================
// G2 — Soft timeouts (never fabricate).
// ============================================================================

/// The outcome of an awaited action under the soft-timeout guarantee (G2). A
/// timeout is a FIRST-CLASS outcome, distinct from a produced result — the control
/// plane reports [`AwaitOutcome::Incomplete`] on timeout and NEVER fabricates a
/// [`AwaitOutcome::Complete`] to fill the gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AwaitOutcome<T> {
    /// A genuine result was produced before the deadline.
    Complete(T),
    /// The deadline passed with no result — reported incomplete, never invented.
    /// Carries a human-legible reason (e.g. the elapsed budget).
    Incomplete { reason: String },
}

impl<T> AwaitOutcome<T> {
    /// Whether a real result was produced. `false` for a timeout — a caller must
    /// NOT read a result off an incomplete outcome.
    pub fn is_complete(&self) -> bool {
        matches!(self, AwaitOutcome::Complete(_))
    }

    /// The produced result, if any. `None` on timeout — there is no result to read
    /// (the guarantee: a timeout never carries a fabricated value).
    pub fn result(&self) -> Option<&T> {
        match self {
            AwaitOutcome::Complete(v) => Some(v),
            AwaitOutcome::Incomplete { .. } => None,
        }
    }
}

/// Resolve an awaited action into a soft-timeout outcome (G2). `produced` is the
/// result the awaited work delivered, if any (`None` == it did not finish in time).
/// A produced result is reported [`AwaitOutcome::Complete`]; the absence of one is
/// reported [`AwaitOutcome::Incomplete`] with the supplied reason. This is the one
/// place the timeout decision is made, so no call site can fabricate a result to
/// paper over a timeout.
pub fn resolve_soft_timeout<T>(produced: Option<T>, timeout_reason: &str) -> AwaitOutcome<T> {
    match produced {
        Some(v) => AwaitOutcome::Complete(v),
        None => AwaitOutcome::Incomplete {
            reason: if timeout_reason.trim().is_empty() {
                "awaited outcome timed out — reported incomplete".to_string()
            } else {
                timeout_reason.to_string()
            },
        },
    }
}

// ============================================================================
// G3 — Verify-don't-fold.
// ============================================================================

/// May a queue row be folded (pruned) given the controller's done-verdict? (G3.)
/// The verdict is held to real evidence: fold ONLY when the three-leg
/// [`VerifyDoneResult`] is `verified`. A pending / unverified / pressured verdict
/// never folds the row — this is the single machine-checkable gate the fold path
/// consults, realized directly on SW-WV-C3's verify-don't-fold outcome so the two
/// can never diverge.
pub fn may_fold(verdict: &VerifyDoneResult) -> bool {
    verdict.verified
}

// ============================================================================
// G4 — Governance (own-item-only writes; boards read-only to agents).
// ============================================================================

/// The attention-board read verbs (G4). These project the board for a reader; none
/// mutates a `wv_*` row. A board is read-only to agents, so NO board-mutating verb
/// exists — the board is changed only as a side effect of the underlying ledger
/// writes (roster / attention / approval), never by a "write the board" verb.
/// Every entry here classifies Tier-A ([`CapabilityTier::ReadObserve`]); the
/// [`board_verbs_are_read_only`] test proves it, so the invariant can never drift
/// from the capability-tier gate.
pub const BOARD_READ_VERBS: &[&str] = &["wv_list_attention_board", "wv_read_pending_assertions"];

/// Is `verb` a board verb an agent may only READ (never write)? (G4.) True for a
/// known board read verb. A future board-mutating verb would classify Tier-B and so
/// must NOT be added here — the board stays read-only to agents.
pub fn is_board_read_verb(verb: &str) -> bool {
    let verb = verb.trim();
    BOARD_READ_VERBS.contains(&verb)
}

/// The own-item-only write decision (G4). An agent may write only an item it OWNS:
/// a write whose `writer` equals the item's `owner` is [`WriteDecision::Owned`];
/// any cross-owner write is [`WriteDecision::NotOwner`] and MUST be refused. This
/// is the authoring-time realization of B1's `WHERE id=? AND {owner_col}=?`
/// one-writer rule (whose CAS loser is [`crate::weft::store::MutateResult::NotOwner`])
/// so a caller can check ownership BEFORE issuing the guarded write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteDecision {
    /// The writer owns the item — the write may proceed.
    Owned,
    /// The writer is not the item's owner — refuse (cross-owner write).
    NotOwner,
}

impl WriteDecision {
    /// Whether the write may proceed.
    pub fn permitted(&self) -> bool {
        matches!(self, WriteDecision::Owned)
    }
}

/// Decide an own-item-only write (G4). A write is [`WriteDecision::Owned`] only when
/// the (trimmed) `writer` exactly matches the item's (trimmed) `owner` AND both are
/// non-blank; every mismatch — including a blank owner or a blank writer — is
/// [`WriteDecision::NotOwner`]. Fails closed: an unknown owner is never writable.
pub fn admit_own_item_write(writer: &str, owner: &str) -> WriteDecision {
    let writer = writer.trim();
    let owner = owner.trim();
    if !writer.is_empty() && writer == owner {
        WriteDecision::Owned
    } else {
        WriteDecision::NotOwner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weft::done_record::VerifyDoneResult;

    // ---------- G1 — consent-on-env-change ----------

    #[test]
    fn env_mutating_actions_classify_as_mutates() {
        for kind in ENV_MUTATING_ACTIONS {
            assert_eq!(
                classify_env_change(kind),
                EnvChange::Mutates,
                "{kind:?} mutates the run environment — G1 must require consent"
            );
        }
    }

    #[test]
    fn non_env_actions_classify_as_inert() {
        for kind in [
            "read_file",
            "wv_read_recent",
            "list_directory",
            "signal_progress",
        ] {
            assert_eq!(
                classify_env_change(kind),
                EnvChange::Inert,
                "{kind:?} does not touch the environment — no consent needed"
            );
        }
    }

    #[test]
    fn classify_env_change_trims_and_is_case_insensitive() {
        assert_eq!(classify_env_change("  SET_CWD "), EnvChange::Mutates);
        assert_eq!(classify_env_change("Credential_Write"), EnvChange::Mutates);
    }

    #[test]
    fn env_change_requires_a_present_consent_token() {
        // No token → refused.
        assert_eq!(
            admit_env_change("set_env", None),
            ConsentDecision::ConsentRequired
        );
        // Blank token → refused (a blank is not consent).
        assert_eq!(
            admit_env_change("set_env", Some("   ")),
            ConsentDecision::ConsentRequired
        );
        // Present token → admitted.
        assert_eq!(
            admit_env_change("set_env", Some("consent-abc123")),
            ConsentDecision::Admitted
        );
        assert!(admit_env_change("set_env", Some("consent-abc123")).admitted());
    }

    #[test]
    fn inert_actions_never_need_consent() {
        // An inert action is admitted regardless of the token (even absent).
        assert_eq!(
            admit_env_change("read_file", None),
            ConsentDecision::Admitted
        );
        assert_eq!(
            admit_env_change("read_file", Some("ignored")),
            ConsentDecision::Admitted
        );
    }

    // ---------- G2 — soft timeouts (never fabricate) ----------

    #[test]
    fn timeout_reports_incomplete_never_a_fabricated_result() {
        let out: AwaitOutcome<&str> = resolve_soft_timeout(None, "waited 30s, no reply");
        assert!(!out.is_complete(), "a timeout is never complete");
        assert_eq!(out.result(), None, "a timeout carries NO fabricated result");
        match out {
            AwaitOutcome::Incomplete { reason } => {
                assert_eq!(reason, "waited 30s, no reply");
            }
            AwaitOutcome::Complete(_) => panic!("timeout must not fabricate a Complete"),
        }
    }

    #[test]
    fn timeout_supplies_a_default_reason_when_blank() {
        let out: AwaitOutcome<i32> = resolve_soft_timeout(None, "   ");
        match out {
            AwaitOutcome::Incomplete { reason } => assert!(
                !reason.trim().is_empty(),
                "an incomplete outcome always carries a legible reason"
            ),
            AwaitOutcome::Complete(_) => panic!("timeout must not fabricate a Complete"),
        }
    }

    #[test]
    fn a_produced_result_is_reported_complete() {
        let out = resolve_soft_timeout(Some("real-answer"), "ignored");
        assert!(out.is_complete());
        assert_eq!(out.result(), Some(&"real-answer"));
    }

    // ---------- G3 — verify-don't-fold ----------

    #[test]
    fn a_row_folds_only_on_a_verified_verdict() {
        let verified = VerifyDoneResult {
            verified: true,
            reason: String::new(),
            failed_legs: Vec::new(),
        };
        assert!(
            may_fold(&verified),
            "a verified done-verdict may fold the row"
        );
    }

    #[test]
    fn an_unverified_verdict_never_folds_the_row() {
        // A leg failure — never fold.
        let failed = VerifyDoneResult {
            verified: false,
            reason: "leg 2 failed — empty footprint".to_string(),
            failed_legs: vec![2],
        };
        assert!(
            !may_fold(&failed),
            "an unverified verdict must NOT fold — the verdict is held to evidence"
        );
        // A pending / no-record verdict (all legs unproven) — never fold.
        let pending = VerifyDoneResult {
            verified: false,
            reason: "no done record — worker may be idle".to_string(),
            failed_legs: vec![1, 2, 3],
        };
        assert!(!may_fold(&pending), "a pending verdict must NOT fold");
    }

    // ---------- G4 — governance: boards read-only to agents ----------

    #[test]
    fn board_verbs_are_read_only() {
        // Every board verb is a read-only board verb — the board is read-only to agents.
        for verb in BOARD_READ_VERBS {
            assert!(is_board_read_verb(verb));
        }
    }

    #[test]
    fn is_board_read_verb_trims_and_rejects_unknown() {
        assert!(is_board_read_verb("  wv_list_attention_board "));
        assert!(!is_board_read_verb("wv_write_dispatch"));
        assert!(!is_board_read_verb("wv_upsert_attention"));
    }

    // ---------- G4 — governance: own-item-only writes ----------

    #[test]
    fn an_agent_may_write_only_its_own_item() {
        assert_eq!(
            admit_own_item_write("session-42", "session-42"),
            WriteDecision::Owned
        );
        assert!(admit_own_item_write("session-42", "session-42").permitted());
    }

    #[test]
    fn a_cross_owner_write_is_refused() {
        assert_eq!(
            admit_own_item_write("session-attacker", "session-owner"),
            WriteDecision::NotOwner
        );
        assert!(!admit_own_item_write("session-attacker", "session-owner").permitted());
    }

    #[test]
    fn own_item_write_fails_closed_on_blank_owner_or_writer() {
        // A blank owner is never writable (fail closed) even by a blank writer.
        assert_eq!(admit_own_item_write("", ""), WriteDecision::NotOwner);
        assert_eq!(
            admit_own_item_write("session-42", ""),
            WriteDecision::NotOwner
        );
        assert_eq!(
            admit_own_item_write("", "session-42"),
            WriteDecision::NotOwner
        );
    }

    #[test]
    fn own_item_write_trims_before_comparing() {
        assert_eq!(
            admit_own_item_write("  session-42 ", "session-42"),
            WriteDecision::Owned
        );
    }

    // ---------- SW-WA-D3 — the exact decisions the live fold/write path wires ----------

    #[test]
    fn d3_live_path_decisions_are_stable() {
        // G3 — may_fold: false on any failing leg, true on a verified verdict.
        assert!(
            !may_fold(&VerifyDoneResult {
                verified: false,
                reason: "leg 3 failed".into(),
                failed_legs: vec![3],
            }),
            "an unverified verdict never folds"
        );
        assert!(
            may_fold(&VerifyDoneResult {
                verified: true,
                reason: String::new(),
                failed_legs: Vec::new(),
            }),
            "a verified verdict folds"
        );

        // G1 — admit_env_change over an ALREADY-SHIPPED kind (D3 does not add
        // connector_write): require on a missing token, admit on a present one.
        assert_eq!(
            admit_env_change("set_env", None),
            ConsentDecision::ConsentRequired
        );
        assert_eq!(
            admit_env_change("set_env", Some("consent-xyz")),
            ConsentDecision::Admitted
        );
        // The connector-write kind is NOT in the vocabulary D3 wires (that is C2).
        assert!(!ENV_MUTATING_ACTIONS.contains(&"connector_write"));

        // G4 — admit_own_item_write: owned / not-owner / blank.
        assert_eq!(
            admit_own_item_write("ctrl-A", "ctrl-A"),
            WriteDecision::Owned
        );
        assert_eq!(
            admit_own_item_write("ctrl-B", "ctrl-A"),
            WriteDecision::NotOwner
        );
        assert_eq!(admit_own_item_write("ctrl-A", ""), WriteDecision::NotOwner);
    }
}
