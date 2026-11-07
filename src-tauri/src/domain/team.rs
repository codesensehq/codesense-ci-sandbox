// SPDX-License-Identifier: MIT
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::error::DomainError;
use super::source_repo_slug::SourceRepoSlug;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    pub name: String,
    #[serde(default)]
    pub source_repos: Vec<SourceRepoSlug>,
    /// SW-HB-7: org-wide, reversible retirement. `#[serde(default)]` so any
    /// pre-field serialized `Team` (JSON to the frontend, or persisted state)
    /// reads as active. Set via `RawTeamEntry::into_domain`, not `Team::new` —
    /// it's state, not a validity invariant.
    #[serde(default)]
    pub archived: bool,
    /// SLW-IN-1: the single parent team this team inherits from, or `None` for a
    /// root team (every team today). Mirrors `archived`: `#[serde(default)]` so
    /// pre-field serialized `Team`s read as roots, and set via
    /// `RawTeamEntry::into_domain`, not `Team::new` — the *link* is state; only
    /// its slug-shape is validated. Reachability/acyclicity is SLW-IN-3.
    #[serde(default)]
    pub parent_team: Option<String>,
    /// SLW-PT-1: DTO-only marker that self-identifies a synthesized **provider**
    /// (personal-team) row to the frontend — flagship `Local`/private legibility
    /// vs. an `Applied from {provider}` overlay. `#[serde(default)]` so every
    /// existing `Team` reads as `false`. Set ONLY on synthesized provider values,
    /// never `Team::new`; it CANNOT round-trip into `teams.yml` because the
    /// persisted `RawTeamEntry` has no such field (the split is the guard). The
    /// durable personal identity is the reserved slug, not this marker.
    #[serde(default)]
    pub is_personal: bool,
    /// The team's home page, opened whenever it has nothing open; relative to its content root
    /// (`teams.yml` `landing_page:`). `None` ⇒ the app's README/index fallback.
    /// Set via `RawTeamEntry::into_domain`, which drops a path that would leave
    /// the team's content.
    #[serde(default)]
    pub landing_page: Option<String>,
}

impl Team {
    pub fn new(
        name: impl Into<String>,
        source_repos: Vec<SourceRepoSlug>,
    ) -> Result<Self, DomainError> {
        let name: String = name.into();
        if name.is_empty() {
            return Err(DomainError::EmptyField { field: "name" });
        }
        if !is_slug_safe(&name) {
            return Err(DomainError::InvalidTeamName(name));
        }

        let mut repo_set = HashSet::new();
        for repo in &source_repos {
            if !repo_set.insert(repo.remote_key()) {
                return Err(DomainError::DuplicateSourceRepo(repo.clone()));
            }
        }

        Ok(Self {
            name,
            source_repos,
            archived: false,
            parent_team: None,
            is_personal: false,
            landing_page: None,
        })
    }

    pub fn content_branch_name(&self) -> String {
        format!("team-{}", self.name)
    }

    pub fn meta_branch_name(&self) -> String {
        format!("team-{}-meta", self.name)
    }
}

/// `^[a-z0-9][a-z0-9-]*$` — the team-name slug rule.
///
/// SLW-IN-7 (D-1 Class A): this doubles as the **flat-root collision guard**.
/// Since teams are stored flat at the context-repo root (`{org_root}/{team}/`),
/// a team name must never collide with a root system entry (`_org`,
/// `_codesense`, `.git`, `*.yml`). This rule structurally forbids every such
/// shape — leading `_`/`.`, any `.`, uppercase, spaces — so no new guard is
/// needed for that class (locked by a regression test at
/// `validate_team_name_full`). The separate layer-marker reservation (Class B,
/// `install`/`content`/`repos`/`libraries`) lives in `validate_team_name`
/// because those names ARE slug-safe.
pub(crate) fn is_slug_safe(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path() {
        let t = Team::new("backend", vec![]).unwrap();
        assert_eq!(t.name, "backend");
        assert!(t.source_repos.is_empty());
    }

    #[test]
    fn rejects_empty_name() {
        assert_eq!(
            Team::new("", vec![]),
            Err(DomainError::EmptyField { field: "name" })
        );
    }

    #[test]
    fn rejects_uppercase_name() {
        assert!(matches!(
            Team::new("Backend", vec![]),
            Err(DomainError::InvalidTeamName(_))
        ));
    }

    #[test]
    fn rejects_space_in_name() {
        assert!(matches!(
            Team::new("back end", vec![]),
            Err(DomainError::InvalidTeamName(_))
        ));
    }

    #[test]
    fn rejects_slash_in_name() {
        assert!(matches!(
            Team::new("back/end", vec![]),
            Err(DomainError::InvalidTeamName(_))
        ));
    }

    #[test]
    fn rejects_leading_dash() {
        assert!(matches!(
            Team::new("-back", vec![]),
            Err(DomainError::InvalidTeamName(_))
        ));
    }

    #[test]
    fn accepts_digit_start_and_hyphen() {
        assert!(Team::new("1-backend", vec![]).is_ok());
        assert!(Team::new("back-end", vec![]).is_ok());
    }

    #[test]
    fn rejects_duplicate_source_repos() {
        let repos = vec![
            SourceRepoSlug::new("o", "r").unwrap(),
            SourceRepoSlug::new("o", "r").unwrap(),
        ];
        let err = Team::new("backend", repos);
        assert_eq!(
            err,
            Err(DomainError::DuplicateSourceRepo(
                SourceRepoSlug::new("o", "r").unwrap()
            ))
        );
    }

    #[test]
    fn allows_same_slug_on_different_hosts() {
        let repos = vec![
            SourceRepoSlug::new("o", "r").unwrap(),
            SourceRepoSlug::with_provider("o", "r", "gitlab").unwrap(),
        ];
        assert!(Team::new("backend", repos).is_ok());
    }

    #[test]
    fn branch_name_derivations() {
        let t = Team::new("backend", vec![]).unwrap();
        assert_eq!(t.content_branch_name(), "team-backend");
        assert_eq!(t.meta_branch_name(), "team-backend-meta");
    }

    #[test]
    fn serde_roundtrip() {
        let t = Team::new("backend", vec![SourceRepoSlug::new("o", "r").unwrap()]).unwrap();
        let json = serde_json::to_string(&t).unwrap();
        let back: Team = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    // SLW-PT-1 AC-6: the synthesized-provider DTO marker reaches the frontend,
    // while a normal team defaults to `false`. (Non-leakage into `teams.yml` is
    // guarded separately by the `RawTeamEntry` split — see the negative tests in
    // `raw_yaml.rs`, which have no `is_personal` field to emit.)
    #[test]
    fn is_personal_serializes_to_frontend_and_defaults_false() {
        let mut provider = Team::new("personal", vec![]).unwrap();
        provider.is_personal = true;
        let json = serde_json::to_string(&provider).unwrap();
        assert!(
            json.contains("\"is_personal\":true"),
            "synthesized provider carries is_personal to the FE: {json}"
        );

        let normal = Team::new("backend", vec![]).unwrap();
        assert!(!normal.is_personal, "Team::new never sets the marker");
        // Absent key deserializes as false (serde default) → no migration.
        let back: Team = serde_json::from_str(r#"{"name":"x"}"#).unwrap();
        assert!(!back.is_personal);
    }
}
