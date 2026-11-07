// SPDX-License-Identifier: MIT
//! The endorsement tier-picker (SW-WV-E2).
//!
//! A pure, I/O-free function that classifies a proposed agent action into an
//! endorsement tier at write time. It codifies the epic's make-or-break rule:
//!
//! - **Tier-3 (explicit 👍 required)** whenever two or more of
//!   {`irreversible`, `blast_radius == High`, `confidence == Low`} hold — so the
//!   wall never moves on high-stakes actions, regardless of the caller's
//!   preferred tier.
//! - **Otherwise the caller-supplied preferred tier** (T1 pre-authorized, or T2
//!   lazy-consent per the action's pre-authorization scope).
//!
//! No-downgrade: a caller that already declared `preferred = T3` stays T3 even
//! with zero risk signals — that falls out of the "return preferred when signal
//! count < 2" branch.
//!
//! The picker is the SOLE decider of the `tier` column: the store handler
//! ([`super::sqlite::wv_write_approval`]) calls it before the INSERT and stores
//! the picker's result, never the agent-supplied preferred tier (ARCHITECTURE
//! §4.1 — an immutable tier written by `pick_tier` at insert time). The tier is
//! reused across [`ApprovalTier`] (`weft/sqlite.rs`) rather than re-declaring a
//! parallel tier enum (the one-name / MODULE-HOME rule).

use super::sqlite::ApprovalTier;

/// The blast radius of a proposed action — one of the three risk signals the
/// tier-picker reads (SW-WV-E2, AC-2).
///
/// Deserialized from lowercase wire strings (`"low" | "medium" | "high"`) with
/// NO `#[serde(other)]` catch-all: an unrecognized value is a typed serde
/// deserialization error at the MCP boundary (mapped to `invalid_params`), never
/// a silent fold into a variant (AC-7).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum BlastRadius {
    Low,
    Medium,
    High,
}

impl BlastRadius {
    /// The lowercase wire string stored in the `wv_approvals.blast_radius` column
    /// (matches the `#[serde(rename_all = "lowercase")]` representation).
    pub fn as_wire(self) -> &'static str {
        match self {
            BlastRadius::Low => "low",
            BlastRadius::Medium => "medium",
            BlastRadius::High => "high",
        }
    }
}

/// The confidence in a proposed action — one of the three risk signals (AC-2).
///
/// Same lowercase-wire, no-catch-all contract as [`BlastRadius`] (AC-7).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    /// The lowercase wire string stored in the `wv_approvals.confidence` column.
    pub fn as_wire(self) -> &'static str {
        match self {
            Confidence::Low => "low",
            Confidence::Medium => "medium",
            Confidence::High => "high",
        }
    }
}

/// The wire name of the `irreversible` risk signal, as reported in the
/// tier-picker `reasons` list.
pub const REASON_IRREVERSIBLE: &str = "irreversible";
/// The wire name of the high-blast-radius risk signal.
pub const REASON_BLAST_RADIUS_HIGH: &str = "blast_radius=high";
/// The wire name of the low-confidence risk signal.
pub const REASON_CONFIDENCE_LOW: &str = "confidence=low";

/// Pure tier-picker — no I/O, no state (AC-2, AC-6).
///
/// Returns `(chosen_tier, reasons)`:
/// - `T3` when two or more of {`irreversible`, `blast_radius == High`,
///   `confidence == Low`} hold — `reasons` names each triggering signal.
/// - `preferred` otherwise — `reasons` is empty. This includes `preferred == T3`
///   with fewer than two signals: a pre-declared T3 is never downgraded.
///
/// The caller ([`super::sqlite::wv_write_approval`]) discards `reasons` for the
/// DB write (the tier alone is stored) and includes them in the MCP response
/// (`wv_classify_action`) so an agent can branch.
pub fn pick_tier(
    irreversible: bool,
    blast_radius: BlastRadius,
    confidence: Confidence,
    preferred: ApprovalTier,
) -> (ApprovalTier, Vec<&'static str>) {
    let mut reasons: Vec<&'static str> = Vec::new();
    if irreversible {
        reasons.push(REASON_IRREVERSIBLE);
    }
    if blast_radius == BlastRadius::High {
        reasons.push(REASON_BLAST_RADIUS_HIGH);
    }
    if confidence == Confidence::Low {
        reasons.push(REASON_CONFIDENCE_LOW);
    }
    if reasons.len() >= 2 {
        (ApprovalTier::T3, reasons)
    } else {
        // Fewer than two signals: honor the caller's preferred tier (including a
        // pre-declared T3 — no downgrade). `reasons` is empty: the tier was not
        // pushed to T3 by the two-of-three rule.
        (preferred, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC-2: all eight signal combinations (2^3). The four with two or more
    // high-risk signals must produce T3 (regardless of `preferred`); the four
    // with fewer must return `preferred`.

    #[test]
    fn none_returns_preferred_t1() {
        let (tier, reasons) =
            pick_tier(false, BlastRadius::Low, Confidence::High, ApprovalTier::T1);
        assert_eq!(tier, ApprovalTier::T1);
        assert!(reasons.is_empty());
    }

    #[test]
    fn irreversible_only_returns_preferred() {
        // One signal → preferred, not T3.
        let (tier, reasons) = pick_tier(true, BlastRadius::Low, Confidence::High, ApprovalTier::T2);
        assert_eq!(tier, ApprovalTier::T2);
        assert!(reasons.is_empty());
    }

    #[test]
    fn blast_high_only_returns_preferred() {
        let (tier, reasons) =
            pick_tier(false, BlastRadius::High, Confidence::High, ApprovalTier::T1);
        assert_eq!(tier, ApprovalTier::T1);
        assert!(reasons.is_empty());
    }

    #[test]
    fn confidence_low_only_returns_preferred() {
        let (tier, reasons) = pick_tier(false, BlastRadius::Low, Confidence::Low, ApprovalTier::T2);
        assert_eq!(tier, ApprovalTier::T2);
        assert!(reasons.is_empty());
    }

    #[test]
    fn irreversible_and_blast_high_is_t3() {
        let (tier, reasons) =
            pick_tier(true, BlastRadius::High, Confidence::High, ApprovalTier::T1);
        assert_eq!(tier, ApprovalTier::T3);
        assert_eq!(reasons, vec![REASON_IRREVERSIBLE, REASON_BLAST_RADIUS_HIGH]);
    }

    #[test]
    fn irreversible_and_confidence_low_is_t3() {
        let (tier, reasons) = pick_tier(true, BlastRadius::Low, Confidence::Low, ApprovalTier::T1);
        assert_eq!(tier, ApprovalTier::T3);
        assert_eq!(reasons, vec![REASON_IRREVERSIBLE, REASON_CONFIDENCE_LOW]);
    }

    #[test]
    fn blast_high_and_confidence_low_is_t3() {
        let (tier, reasons) =
            pick_tier(false, BlastRadius::High, Confidence::Low, ApprovalTier::T2);
        assert_eq!(tier, ApprovalTier::T3);
        assert_eq!(
            reasons,
            vec![REASON_BLAST_RADIUS_HIGH, REASON_CONFIDENCE_LOW]
        );
    }

    #[test]
    fn all_three_signals_is_t3() {
        let (tier, reasons) = pick_tier(true, BlastRadius::High, Confidence::Low, ApprovalTier::T1);
        assert_eq!(tier, ApprovalTier::T3);
        assert_eq!(
            reasons,
            vec![
                REASON_IRREVERSIBLE,
                REASON_BLAST_RADIUS_HIGH,
                REASON_CONFIDENCE_LOW
            ]
        );
    }

    // AC-2: the additional no-downgrade case.
    #[test]
    fn preferred_t3_with_zero_signals_stays_t3() {
        let (tier, reasons) =
            pick_tier(false, BlastRadius::Low, Confidence::High, ApprovalTier::T3);
        assert_eq!(tier, ApprovalTier::T3);
        // `reasons` is empty — the tier was NOT pushed to T3 by the risk rule; it
        // is the caller's own pre-declared T3.
        assert!(reasons.is_empty());
    }

    #[test]
    fn two_signals_override_preferred_t1() {
        // A caller preferring T1 cannot escape T3 when the risk rule fires.
        let (tier, _) = pick_tier(true, BlastRadius::High, Confidence::High, ApprovalTier::T1);
        assert_eq!(tier, ApprovalTier::T3);
    }

    // AC-7 (unit half): unknown wire strings are a typed serde error, never a
    // silent fold. The MCP boundary maps this to invalid_params.
    #[test]
    fn unknown_blast_radius_is_a_deserialization_error() {
        let err = serde_json::from_str::<BlastRadius>("\"catastrophic\"");
        assert!(err.is_err());
    }

    #[test]
    fn unknown_confidence_is_a_deserialization_error() {
        let err = serde_json::from_str::<Confidence>("\"very-low\"");
        assert!(err.is_err());
    }

    #[test]
    fn valid_lowercase_signals_deserialize() {
        assert_eq!(
            serde_json::from_str::<BlastRadius>("\"high\"").unwrap(),
            BlastRadius::High
        );
        assert_eq!(
            serde_json::from_str::<Confidence>("\"low\"").unwrap(),
            Confidence::Low
        );
    }
}
