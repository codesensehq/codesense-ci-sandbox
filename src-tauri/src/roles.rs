// SPDX-License-Identifier: MIT
//! SW-GW-4 — the semantic **role** contract.
//!
//! A role is the ONE finite thing the live-progress `phase` (SW-GW-4) and the
//! durable, team-configurable story `status` (SW-SC-1) agree on. Code keys control
//! flow off the *role*, never off a (renameable) status literal, so a team can
//! rename/reorder/add/remove its `status` states without breaking rollups, board
//! columns, or readiness.
//!
//! This module is deliberately dependency-light. Today it owns the **fixed**
//! `phase → role` map (the phase side is a closed lifecycle — the skill pipeline is
//! the same regardless of team naming). SW-SC-1 will extend it with the
//! data-driven `status → role` map loaded from the team taxonomy. Keep the `Role`
//! set here as the shared spine both halves import; do not redefine it elsewhere.

/// The finite semantic role set. Every live `phase` and every durable `status`
/// resolves to exactly one of these; the UI and rollups key off the role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Not yet begun — planning/backlog. Readiness-eligible.
    NotStarted,
    /// Actively in flight — dev underway.
    Active,
    /// Under review / QA / PR-in-flight.
    Review,
    /// Terminal success.
    Done,
    /// Stalled, needs a human.
    Blocked,
}

impl Role {
    /// The stable wire token (snake_case) — the same string SW-SC-1's taxonomy
    /// `role:` field uses, so the two halves are byte-compatible.
    pub fn as_wire(&self) -> &'static str {
        match self {
            Role::NotStarted => "not_started",
            Role::Active => "active",
            Role::Review => "review",
            Role::Done => "done",
            Role::Blocked => "blocked",
        }
    }
}

/// The FIXED live-progress lifecycle phase (SW-GW-4). Distinct from the durable
/// `status`: it tracks *where in the run* a story is as its skills execute. The set
/// is closed — a new phase is a code change, not a caller-supplied value — so the
/// trust surface and the UI stay finite (AC-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Planning,
    Dev,
    Review,
    Pr,
    Blocked,
    Done,
}

/// Every phase, in lifecycle order. The single list drives iteration + the parity
/// test; keep `Phase`, `from_wire`, `as_wire`, and `role` in sync with it.
pub const PHASES: &[Phase] = &[
    Phase::Planning,
    Phase::Dev,
    Phase::Review,
    Phase::Pr,
    Phase::Blocked,
    Phase::Done,
];

impl Phase {
    /// Parse the wire value; an unknown phase is `None` so the MCP handler can
    /// reject it with a typed error (AC-2) rather than broadcast garbage.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "planning" => Some(Phase::Planning),
            "dev" => Some(Phase::Dev),
            "review" => Some(Phase::Review),
            "pr" => Some(Phase::Pr),
            "blocked" => Some(Phase::Blocked),
            "done" => Some(Phase::Done),
            _ => None,
        }
    }

    /// The stable wire token.
    pub fn as_wire(&self) -> &'static str {
        match self {
            Phase::Planning => "planning",
            Phase::Dev => "dev",
            Phase::Review => "review",
            Phase::Pr => "pr",
            Phase::Blocked => "blocked",
            Phase::Done => "done",
        }
    }

    /// The fixed `phase → role` map — the phase half of the shared contract. Match
    /// is compiler-exhaustive, so every phase is guaranteed to resolve to a role.
    ///
    /// `Planning → NotStarted` and `Pr → Review` keep the phase's role aligned with
    /// the durable status of the same word (a story in `pr` is review-role, heading
    /// to done), so the live chip and the resting status render the same role.
    pub fn role(&self) -> Role {
        match self {
            Phase::Planning => Role::NotStarted,
            Phase::Dev => Role::Active,
            Phase::Review => Role::Review,
            Phase::Pr => Role::Review,
            Phase::Blocked => Role::Blocked,
            Phase::Done => Role::Done,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_role_map_is_exact() {
        // Pins the phase half of the shared contract — a wrong edit is caught here.
        assert_eq!(Phase::Planning.role(), Role::NotStarted);
        assert_eq!(Phase::Dev.role(), Role::Active);
        assert_eq!(Phase::Review.role(), Role::Review);
        assert_eq!(Phase::Pr.role(), Role::Review);
        assert_eq!(Phase::Blocked.role(), Role::Blocked);
        assert_eq!(Phase::Done.role(), Role::Done);
    }

    #[test]
    fn wire_round_trips_for_every_phase() {
        for &p in PHASES {
            assert_eq!(Phase::from_wire(p.as_wire()), Some(p), "round-trip {p:?}");
        }
    }

    #[test]
    fn unknown_phase_is_rejected() {
        for bad in ["", "PLANNING", "shipped", "in-progress", "todo", "qa"] {
            assert_eq!(Phase::from_wire(bad), None, "must reject {bad:?}");
        }
    }

    #[test]
    fn every_phase_has_a_role_and_wire() {
        // PHASES must list every variant (guards a forgotten append).
        assert_eq!(PHASES.len(), 6);
        for &p in PHASES {
            let _ = p.role();
            assert!(!p.as_wire().is_empty());
        }
    }

    #[test]
    fn role_wire_tokens_are_snake_case_stable() {
        assert_eq!(Role::NotStarted.as_wire(), "not_started");
        assert_eq!(Role::Active.as_wire(), "active");
        assert_eq!(Role::Review.as_wire(), "review");
        assert_eq!(Role::Done.as_wire(), "done");
        assert_eq!(Role::Blocked.as_wire(), "blocked");
    }
}
