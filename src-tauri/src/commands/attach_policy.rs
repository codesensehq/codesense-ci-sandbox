// SPDX-License-Identifier: MIT
//! SW-WL-3 (AC-9b/9d/9e): the table-driven attach policy that decides, per
//! watch *category*, whether a workspace's watchers should be attached.
//!
//! Per-workspace watches split by purpose into two categories: **context** dest
//! watchers (projected content — story dir, docs, framework dirs, derived from
//! CP-1's manifest) and **install** dest watchers (`WatchConfig.install_dirs`).
//! Whether each category attaches is governed by a small `const` table
//! ([`DEFAULT_POLICIES`]) of `(category, reasons-that-trigger-attach)`, so
//! moving a reason between categories or adding a new reason/category is a
//! one-line, additive change — not a rewrite of the dispatch path (AC-9d).
//!
//! The reasons evaluate against a [`WorldState`], the injection seam for the
//! frontend-owned facts. Today the only live reason is [`AttachReason::WorkspaceOpen`];
//! [`AttachReason::SoftClosedWithRunningPty`] is wired but evaluates `false`
//! until PT-10 ships its `softClosedWorkspaces` slice + `has_running_pty`
//! (F-6) — at which point a richer `WorldState` impl flips it on with NO change
//! to this table's machinery. Until the CP-4 reverse-sync consumer lands the
//! context category is wired-but-inert (F-7); the policy lands now so both are
//! one-line activations later.
//!
//! Forward-dependency seam: the live attach path (`lazy_attach_workspace`)
//! attaches both categories together today, because without PT-10 + CP-4 the
//! split is behaviour-neutral. The policy table + `WorldState` land now,
//! fully unit-tested, so PT-10/CP-4 flip them on with a one-line table/impl
//! change rather than a rewrite. `allow(dead_code)` mirrors the established
//! seam pattern (cf. SW-CP-5's symlink primitives) until that wiring lands.
#![allow(dead_code)]

use std::collections::HashSet;

/// Categories of per-workspace watch dirs (the "what").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchCategory {
    /// Projected content dest dirs (CP-1 manifest `dest_prefix`s).
    Context,
    /// Install dest dirs (`WatchConfig.install_dirs`).
    Install,
    // Future: TeamContext, PersonalLayer, OrgLayer, ...
}

/// Predicates that can trigger attach for a category (the "when").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachReason {
    /// The workspace surface is open (`WorkspaceOpenSet.has`).
    WorkspaceOpen,
    /// The workspace is soft-closed but still has a running PTY (PT-10).
    SoftClosedWithRunningPty,
    // Future: PendingOperation, ExplicitUserPin, WaveInProgress, ...
}

impl AttachReason {
    /// Evaluate this reason for `workspace` against the current world.
    pub fn evaluate(self, workspace: &str, world: &dyn WorldState) -> bool {
        match self {
            Self::WorkspaceOpen => world.is_workspace_open(workspace),
            // F-6: false until PT-10 backs `soft_closed_with_running_pty`.
            Self::SoftClosedWithRunningPty => world.soft_closed_with_running_pty(workspace),
        }
    }
}

/// One row of the policy table: a category and the reasons whose disjunction
/// (OR) attaches it.
#[derive(Debug)]
pub struct AttachPolicy {
    pub category: WatchCategory,
    pub attach_when: &'static [AttachReason],
}

/// The current attach decisions, made declarative. Editing a row's
/// `attach_when` is the ONLY change needed to move/add a reason (AC-9d).
pub const DEFAULT_POLICIES: &[AttachPolicy] = &[
    AttachPolicy {
        category: WatchCategory::Context,
        attach_when: &[
            AttachReason::WorkspaceOpen,
            AttachReason::SoftClosedWithRunningPty,
        ],
    },
    AttachPolicy {
        category: WatchCategory::Install,
        // Install files don't participate in claude's background writes, so the
        // soft-close reason intentionally does NOT apply here (AC-9b).
        attach_when: &[AttachReason::WorkspaceOpen],
    },
];

fn policy_for(category: WatchCategory) -> &'static AttachPolicy {
    DEFAULT_POLICIES
        .iter()
        .find(|p| p.category == category)
        .expect("DEFAULT_POLICIES must cover every WatchCategory")
}

/// Compose the policy for `category`: attached iff ANY of its reasons holds.
/// The dispatch path is identical regardless of how many categories or reasons
/// exist — growth is additive to the table, not here (AC-9d).
pub fn should_attach(category: WatchCategory, workspace: &str, world: &dyn WorldState) -> bool {
    policy_for(category)
        .attach_when
        .iter()
        .any(|reason| reason.evaluate(workspace, world))
}

/// The frontend-owned facts the reasons evaluate against. A trait so PT-10 can
/// supply a richer impl (with `softClosedWorkspaces` + `has_running_pty`)
/// without touching the policy machinery (F-6).
pub trait WorldState {
    fn is_workspace_open(&self, workspace: &str) -> bool;
    fn soft_closed_with_running_pty(&self, workspace: &str) -> bool;
}

/// SW-WL-3 (F-6): the world before PT-10 ships. A workspace is "open" exactly
/// when it is in the open set we were told about; soft-close never holds (the
/// `softClosedWorkspaces` slice does not exist yet), so the soft-close reason
/// degrades to `false` and context watchers detach on close like install ones.
/// When PT-10 lands, a sibling `WorldState` impl returns `true` here.
pub struct PreSoftCloseWorld<'a> {
    pub open: &'a HashSet<String>,
}

impl WorldState for PreSoftCloseWorld<'_> {
    fn is_workspace_open(&self, workspace: &str) -> bool {
        self.open.contains(workspace)
    }
    fn soft_closed_with_running_pty(&self, _workspace: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(open: &[&str]) -> HashSet<String> {
        open.iter().map(|s| s.to_string()).collect()
    }

    // ---- AC-9e: one unit test per AttachReason::evaluate arm ----

    #[test]
    fn evaluate_workspace_open_true_when_set_contains_workspace() {
        let open = world(&["ws-1"]);
        let w = PreSoftCloseWorld { open: &open };
        assert!(AttachReason::WorkspaceOpen.evaluate("ws-1", &w));
        assert!(!AttachReason::WorkspaceOpen.evaluate("ws-2", &w));
    }

    #[test]
    fn evaluate_soft_closed_with_pty_is_false_until_pt10() {
        let open = world(&["ws-1"]);
        let w = PreSoftCloseWorld { open: &open };
        // F-6: the pre-PT-10 world never reports soft-close, even for an
        // otherwise-open workspace.
        assert!(!AttachReason::SoftClosedWithRunningPty.evaluate("ws-1", &w));
        assert!(!AttachReason::SoftClosedWithRunningPty.evaluate("ws-2", &w));
    }

    /// The PT-10 state, modelled now: the workspace is soft-closed (NOT open)
    /// yet has a running PTY. Lets the table's soft-close branch be exercised
    /// ahead of PT-10's real `WorldState` impl. AC-9b/F-6 say this pure-table
    /// behaviour is testable today (only the live claude-PTY e2e is deferred).
    struct SoftClosedWithPtyWorld;
    impl WorldState for SoftClosedWithPtyWorld {
        fn is_workspace_open(&self, _workspace: &str) -> bool {
            false
        }
        fn soft_closed_with_running_pty(&self, _workspace: &str) -> bool {
            true
        }
    }

    #[test]
    fn evaluate_soft_closed_with_pty_true_arm() {
        // AC-9e: the `true` branch of the SoftClosedWithRunningPty evaluate arm
        // (the false branch is covered above against PreSoftCloseWorld). With
        // the workspace not open, WorkspaceOpen is independently false — the
        // soft-close reason is what carries the attach.
        let w = SoftClosedWithPtyWorld;
        assert!(AttachReason::SoftClosedWithRunningPty.evaluate("ws-1", &w));
        assert!(!AttachReason::WorkspaceOpen.evaluate("ws-1", &w));
    }

    // ---- AC-9d: composition per (category × reason) ----

    #[test]
    fn context_attached_when_workspace_open() {
        let open = world(&["ws-1"]);
        let w = PreSoftCloseWorld { open: &open };
        assert!(should_attach(WatchCategory::Context, "ws-1", &w));
        assert!(!should_attach(WatchCategory::Context, "ws-2", &w));
    }

    #[test]
    fn install_attached_when_workspace_open() {
        let open = world(&["ws-1"]);
        let w = PreSoftCloseWorld { open: &open };
        assert!(should_attach(WatchCategory::Install, "ws-1", &w));
        assert!(!should_attach(WatchCategory::Install, "ws-2", &w));
    }

    #[test]
    fn soft_closed_with_pty_attaches_context_but_not_install() {
        // AC-9b test (a): the table's central correctness claim — when the
        // soft-close predicate is forced true (PT-10 state), CONTEXT watchers
        // stay attached (claude writes projected content) while INSTALL
        // watchers detach (claude doesn't write install files). Composed via
        // `should_attach`, not just the table shape, so a regression that
        // wired the soft-close reason into the Install row would fail here.
        let w = SoftClosedWithPtyWorld;
        assert!(should_attach(WatchCategory::Context, "ws-1", &w));
        assert!(!should_attach(WatchCategory::Install, "ws-1", &w));
    }

    #[test]
    fn closed_workspace_detaches_both_categories_pre_pt10() {
        // AC-9b: with the workspace closed and no PT-10 soft-close, neither
        // context nor install watchers attach.
        let open = world(&[]);
        let w = PreSoftCloseWorld { open: &open };
        assert!(!should_attach(WatchCategory::Context, "ws-1", &w));
        assert!(!should_attach(WatchCategory::Install, "ws-1", &w));
    }

    #[test]
    fn install_policy_excludes_soft_close_reason() {
        // AC-9b: install's row must NOT list SoftClosedWithRunningPty (so when
        // PT-10 flips that reason on, install still detaches on soft-close
        // while context stays attached). This is a structural guard on the
        // table itself, independent of any WorldState.
        let install = policy_for(WatchCategory::Install);
        assert!(!install
            .attach_when
            .contains(&AttachReason::SoftClosedWithRunningPty));
        let context = policy_for(WatchCategory::Context);
        assert!(context
            .attach_when
            .contains(&AttachReason::SoftClosedWithRunningPty));
    }

    #[test]
    fn policy_table_covers_every_category() {
        // policy_for panics if a category is missing — assert both resolve.
        for cat in [WatchCategory::Context, WatchCategory::Install] {
            let _ = policy_for(cat);
        }
    }
}
