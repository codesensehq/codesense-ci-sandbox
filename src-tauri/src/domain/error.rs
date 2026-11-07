// SPDX-License-Identifier: MIT
use std::fmt;

use super::person::PersonId;
use super::source_repo_slug::SourceRepoSlug;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    EmptyField {
        field: &'static str,
    },
    InvalidTeamName(String),
    /// SLW-IN-1: `parent_team` names a team, so it must be slug-safe like
    /// `Team::name`. Whether the parent *resolves* / is acyclic is not checked
    /// here (SLW-IN-3) — only the string shape.
    InvalidParentTeam(String),
    DuplicateSourceRepo(SourceRepoSlug),
    DuplicatePerson(PersonId),
    DuplicateTeam(String),
    /// SLW-IN-3 (AC-1): a team's `parent_team` chain loops back on itself
    /// (`a→b→a`, or a self-parent `a→a`). `team` names the node at which the
    /// walk revisited an already-seen team — the cycle's entry point. Detected
    /// at composition (`Org::new`), so a hand-edited cycle in `teams.yml` is a
    /// typed load error, never an infinite loop or a silent bad resolution.
    TeamParentCycle {
        team: String,
    },
    /// SLW-IN-3 (AC-2): `team`'s `parent_team` names `parent`, which is not a
    /// team in `teams.yml`. Distinct from `InvalidParentTeam` (IN-1), which only
    /// checks the string *shape*; this checks *resolvability*, which requires the
    /// full team set and so lives at whole-org composition, not per-entry parse.
    DanglingParentTeam {
        team: String,
        parent: String,
    },
    /// SLW-IN-3 (AC-5): a legitimately-acyclic `parent_team` chain rooted at
    /// `team` exceeds `MAX_CHAIN_DEPTH`. Guards a pathological-but-acyclic chain
    /// (a cycle is caught by `TeamParentCycle` first). Reuses the same depth
    /// constant as the resolver-side defensive walk (`Org::team_chain`).
    TeamChainTooDeep {
        team: String,
        depth: usize,
    },
    InvalidSourceRepoSegment {
        field: &'static str,
        segment: String,
        reason: &'static str,
    },
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField { field } => write!(f, "{} cannot be empty", field),
            Self::InvalidTeamName(name) => write!(
                f,
                "team name '{}' is not slug-safe (must match ^[a-z0-9][a-z0-9-]*$)",
                name
            ),
            Self::InvalidParentTeam(name) => write!(
                f,
                "parent_team '{}' is not slug-safe (must match ^[a-z0-9][a-z0-9-]*$)",
                name
            ),
            Self::DuplicateSourceRepo(slug) => {
                write!(f, "duplicate source repo '{}' in team.source_repos", slug)
            }
            Self::DuplicatePerson(id) => {
                write!(f, "duplicate person with id {} in org.people", id)
            }
            Self::DuplicateTeam(name) => {
                write!(f, "duplicate team name '{}' in org.teams", name)
            }
            Self::TeamParentCycle { team } => write!(
                f,
                "team '{}' is part of a parent_team cycle (a team cannot inherit from itself, directly or transitively)",
                team
            ),
            Self::DanglingParentTeam { team, parent } => write!(
                f,
                "team '{}' has parent_team '{}', which is not a team in teams.yml",
                team, parent
            ),
            Self::TeamChainTooDeep { team, depth } => write!(
                f,
                "team '{}' has a parent_team chain deeper than the maximum of {} (depth {})",
                team,
                super::org::MAX_CHAIN_DEPTH,
                depth
            ),
            Self::InvalidSourceRepoSegment {
                field,
                segment,
                reason,
            } => write!(
                f,
                "source repo {} '{}' is invalid: {}",
                field, segment, reason
            ),
        }
    }
}

impl std::error::Error for DomainError {}
