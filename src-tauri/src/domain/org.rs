// SPDX-License-Identifier: MIT
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::error::DomainError;
use super::person::Person;
use super::raw_yaml::GITHUB_PROVIDER;
use super::team::Team;

/// The maximum `parent_team` chain depth, shared by the two defenses that must
/// agree on the ceiling: SLW-IN-3's load-time cap (`Org::new` →
/// [`validate_parent_chains`], `TeamChainTooDeep`) and SLW-IN-2's resolver-side
/// belt-and-suspenders walk ([`Org::team_chain`]). Keeping it in one place means
/// a cycle-free-but-absurd chain is rejected at load with the same bound the
/// walk would otherwise silently truncate.
pub(crate) const MAX_CHAIN_DEPTH: usize = 64;

/// Per-org config flags, decoded from `org.yml::config_flags`. Every field is
/// `#[serde(default)]` so a legacy `org.yml` without the key (or the whole
/// block) loads clean — the parser is not `deny_unknown_fields`, so this stays
/// a no-migration extension point.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgConfigFlags {
    /// Where codesense lands managed source clones for this org. `None` ⇒ the
    /// hidden default under `{codesense_home}/users/{uid}/sources/`; `Some(root)`
    /// ⇒ a visible, `cd`-able workspace root (clones go to `{root}/{owner}/{repo}/`).
    /// Resolved by [`crate::context_repo::resolve_managed_clone_dir`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root_dir: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Org {
    /// URL-normalized org slug; matches the directory name under
    /// `users/{uid}/orgs/`. This is the authoritative value for path math —
    /// it derives from the on-disk directory, not `org.yml::slug`.
    pub slug: String,
    pub display_name: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub remote_url: Option<String>,
    /// Runtime-derived provider for the context-repository remote.
    #[serde(default)]
    pub remote_provider: Option<String>,
    #[serde(default)]
    pub config_flags: OrgConfigFlags,
    #[serde(default)]
    pub people: Vec<Person>,
    #[serde(default)]
    pub teams: Vec<Team>,
    /// SW-DX-1: the optional team-defined tag registry (rides on `org.yml`).
    /// Set post-construction in `RawOrgComposition::into_org`; an empty schema is
    /// the unopinionated default. `#[serde(default)]` so cached/legacy `Org`
    /// JSON without the field deserializes cleanly.
    #[serde(default)]
    pub tag_schema: super::TagSchema,
    /// SW-FG-9: the context repo's bound connection. `None` ⇒ resolve the token
    /// from the implicit-active connection (today's behavior, byte-for-byte);
    /// `Some` ⇒ resolve the named `(provider, account)`'s token instead, so a
    /// context repo can legitimately live on a non-active connection (and span
    /// providers). Reuses FG-3's shared `CredentialRef` — no twin. Naming a
    /// machine-local account id, it is per-user state (like FG-3's attachment
    /// binding), not team-shared `org.yml` config; the surface that *sets* and
    /// persists it is FG-10b/10c. `#[serde(default)]` so cached/legacy `Org` JSON
    /// without the field loads unbound — the AC-3 back-compat guarantee.
    #[serde(default)]
    pub credential_ref: Option<super::CredentialRef>,
    #[serde(skip_deserializing, default)]
    pub org_root: String,
    /// Backend-authoritative absolute base directory for each `(team, remote)`.
    /// Keys are `team\ncanonical-remote-key`; this runtime DTO field is never
    /// persisted to the shared registry.
    #[serde(skip_deserializing, default)]
    pub resolved_repository_paths: std::collections::BTreeMap<String, String>,
}

impl Org {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        slug: impl Into<String>,
        display_name: impl Into<String>,
        created_at: DateTime<Utc>,
        remote_url: Option<String>,
        config_flags: OrgConfigFlags,
        people: Vec<Person>,
        teams: Vec<Team>,
    ) -> Result<Self, DomainError> {
        let slug: String = slug.into();
        let display_name: String = display_name.into();
        if slug.trim().is_empty() {
            return Err(DomainError::EmptyField { field: "slug" });
        }
        if display_name.trim().is_empty() {
            return Err(DomainError::EmptyField {
                field: "display_name",
            });
        }

        // Duplicate person ids (keyed on the provider-neutral PersonId — SW-FG-4b).
        let mut seen_people = HashSet::new();
        for p in &people {
            if !seen_people.insert(p.id.clone()) {
                return Err(DomainError::DuplicatePerson(p.id.clone()));
            }
        }

        // Duplicate team names
        let mut seen_team_names = HashSet::new();
        for t in &teams {
            if !seen_team_names.insert(t.name.clone()) {
                return Err(DomainError::DuplicateTeam(t.name.clone()));
            }
        }

        // SLW-IN-3 (AC-1/AC-2/AC-5): validate the single-parent chains once, at
        // composition — a cycle, a dangling parent, or a pathologically deep
        // chain becomes a typed error before it can reach the pure resolver.
        // Runs AFTER the duplicate-team check above, which guarantees name
        // uniqueness and so makes the visited-set cycle detection sound.
        validate_parent_chains(&teams)?;

        Ok(Self {
            slug,
            display_name,
            created_at,
            remote_url,
            remote_provider: None,
            config_flags,
            people,
            teams,
            // SW-DX-1: empty by default; the org-load path
            // (`RawOrgComposition::into_org`) populates it from `org.yml`.
            tag_schema: super::TagSchema::default(),
            // SW-FG-9: unbound by default (⇒ implicit-active connection). Set
            // post-construction by FG-10b/10c's connect/re-bind flow, like
            // `tag_schema` — it is state, not a construction-time validity concern.
            credential_ref: None,
            org_root: String::new(),
            resolved_repository_paths: std::collections::BTreeMap::new(),
        })
    }

    pub fn team(&self, name: &str) -> Option<&Team> {
        self.teams.iter().find(|t| t.name == name)
    }

    /// SLW-IN-2: the ordered single-parent resolution chain for `self_name`,
    /// nearest-first — `[self, parent, grandparent, …, root]`. A first-wins
    /// walk of `Team.parent_team` (the field SLW-IN-1 landed): each hop follows
    /// the named parent until a team has no parent (a root) or the parent name
    /// doesn't resolve to a team (dangling — included as a name so resolution
    /// simply misses on its absent dir, exactly as today; SLW-IN-3 turns that
    /// into a typed load-time error).
    ///
    /// A **parentless** team returns `[self_name]` — CodeSense prescribes no
    /// root team (SW-LT-7), so a team inherits from exactly the `parent_team`
    /// chain it declares and nothing is appended. "No parent" means inherit
    /// nothing but the builtin projection floor.
    ///
    /// **Defensive bound (AC-5).** `teams.yml` is hand-editable, so a cycle
    /// (`a→b→a`, self-parent) or a pathologically long chain is a when-not-if.
    /// A visited-set stops the walk the moment a team repeats, and a depth cap
    /// bounds it regardless — so the chain can never loop **before** SLW-IN-3's
    /// load-time validation exists. This is belt-and-suspenders: the visited-set
    /// already bounds the walk to at most one entry per distinct team.
    /// Registry validation and team_view only.
    pub(crate) fn team_chain(&self, self_name: &str) -> Vec<String> {
        // `MAX_CHAIN_DEPTH` (module const) bounds the walk independently of the
        // visited-set (which already caps it at the org's team count) — a cheap
        // second guard against a malformed org with an implausibly deep linear
        // chain. Shared with SLW-IN-3's load-time validator so both agree.
        let mut chain: Vec<String> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut cursor: Option<String> = Some(self_name.to_string());

        while let Some(name) = cursor {
            // Cycle guard: a repeated team ends the walk (nearer-wins already
            // recorded the first occurrence).
            if !visited.insert(name.clone()) {
                break;
            }
            if chain.len() >= MAX_CHAIN_DEPTH {
                break;
            }
            // Follow this team's parent pointer BEFORE pushing, so a dangling
            // parent (no matching team) still contributes its name to the chain
            // (resolution misses on the absent dir) and then terminates.
            cursor = self.team(&name).and_then(|t| t.parent_team.clone());
            chain.push(name);
        }
        chain
    }

    /// SW-FG-4b (AC-4): resolve a person by a **provider-scoped** handle — the seam
    /// FG-13's MR-author correlation consumes. Handles are not unique across
    /// providers (someone's GitLab handle may be another's GitHub handle), so the
    /// lookup MUST take a provider. Both the query and each stored handle are
    /// normalized the same way ([`normalize_login`]) and compared case-insensitively
    /// — for `github` this is byte-for-byte the SW-ID-1 contract the TS twin
    /// enforces. A person whose matching identity has a null handle never matches
    /// (assignable, not correlatable — AC-6). First match wins; an empty/`@`-only
    /// query resolves to `None`.
    pub fn person_by_handle(&self, provider: &str, handle: &str) -> Option<&Person> {
        let handle = normalize_login(handle);
        if handle.is_empty() {
            return None;
        }
        self.people.iter().find(|p| {
            p.identities.iter().any(|i| {
                i.provider == provider
                    && i.handle
                        .as_deref()
                        .is_some_and(|h| normalize_login(h).eq_ignore_ascii_case(handle))
            })
        })
    }

    /// SW-ID-1: resolve a person by GitHub **login** — a thin shim over
    /// [`Org::person_by_handle`] scoped to the `github` provider. `people.yml` keys
    /// by id, but `assigned_to` carries a bare login, so this is the seam SW-ID-4's
    /// `assigned_to` validation consumes.
    ///
    /// Query and stored login are normalized the same way ([`normalize_login`]:
    /// strip ASCII whitespace + one leading `@`), then compared
    /// case-insensitively — codepoint-for-codepoint the contract the TS twin
    /// `personByLogin` enforces via `usernameKey` (`shared/lib/github-username.ts`),
    /// so both sides resolve a roster identically. GitHub logins are ASCII, so
    /// `eq_ignore_ascii_case` is an exact case-fold; any non-ASCII byte (an
    /// exotic-whitespace or non-Latin char in a hand-edited entry) is left
    /// untouched and compared verbatim on both sides — never a cross-side
    /// divergence. An empty/`@`-only query resolves to `None`.
    ///
    /// First match wins if two entries share a normalized login — `Org::new`
    /// enforces uniqueness on id, not login, so a collision is possible; a
    /// deterministic first-wins keeps resolution total (never panics/misses).
    /// Unlike the TS index build, this per-call lookup has no build step in
    /// which to warn on a duplicate, so it stays silent by design.
    pub fn person_by_username(&self, login: &str) -> Option<&Person> {
        self.person_by_handle(GITHUB_PROVIDER, login)
    }
}

/// SW-ID-1: normalize a GitHub login for the case-insensitive roster lookup —
/// strip surrounding ASCII whitespace, then strip a single leading `@`. Applied
/// to BOTH the query and each stored `github_username` in
/// [`Org::person_by_username`] so the two are compared on equal footing.
///
/// Trims the **ASCII whitespace set only** (space, `\t`, `\n`, `\x0b`, `\x0c`,
/// `\r`) rather than `str::trim` — deliberately, so it matches the TS
/// `usernameKey` (`shared/lib/github-username.ts`) codepoint-for-codepoint.
/// `str::trim` (Unicode White_Space) and JS `String.trim` disagree on exotic
/// blanks (e.g. U+0085 NEL, U+FEFF BOM); pinning both sides to the ASCII set —
/// the only whitespace a `[a-zA-Z0-9-]` login could accidentally carry — keeps
/// the two resolvers from ever disagreeing on a hand-edited roster. Returns a
/// borrowed slice — no allocation.
fn normalize_login(login: &str) -> &str {
    let trimmed =
        login.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'));
    trimmed.strip_prefix('@').unwrap_or(trimmed)
}

/// SLW-IN-3: validate every team's `parent_team` chain at composition time.
///
/// For each team, walk its ancestry (`parent_team` links) and reject:
/// - **cycle** — the walk revisits a team (`TeamParentCycle`);
/// - **dangling** — a `parent_team` names a team absent from the set
///   (`DanglingParentTeam`);
/// - **too deep** — an acyclic chain longer than [`MAX_CHAIN_DEPTH`]
///   (`TeamChainTooDeep`).
///
/// Pure and set-aware: it needs the whole team collection (a per-entry parser
/// cannot decide resolvability), which is exactly why it lives here beside the
/// other whole-org invariants rather than in `RawTeamEntry::into_domain`. The
/// caller (`Org::new`) runs it after the duplicate-team check, so team names are
/// unique and the per-team visited-set is a sound cycle oracle.
fn validate_parent_chains(teams: &[Team]) -> Result<(), DomainError> {
    // name → its parent_team (both borrowed from `teams`). Uniqueness is
    // guaranteed by the caller's duplicate-team check, so later inserts can't
    // clobber an earlier team's parent pointer.
    let parent_of: HashMap<&str, Option<&str>> = teams
        .iter()
        .map(|t| (t.name.as_str(), t.parent_team.as_deref()))
        .collect();

    for team in teams {
        let mut visited: HashSet<&str> = HashSet::new();
        let mut cursor: &str = &team.name;
        let mut depth: usize = 0;

        loop {
            // Revisiting a team means the chain loops (cycle or self-parent).
            if !visited.insert(cursor) {
                return Err(DomainError::TeamParentCycle {
                    team: cursor.to_string(),
                });
            }
            // Depth guard for a legitimately-acyclic-but-absurd chain. The
            // visited-set catches cycles first, so this only fires past
            // MAX_CHAIN_DEPTH *distinct* ancestors.
            depth += 1;
            if depth > MAX_CHAIN_DEPTH {
                return Err(DomainError::TeamChainTooDeep {
                    team: team.name.clone(),
                    depth,
                });
            }
            match parent_of.get(cursor) {
                // This team names a parent — it must resolve to a real team.
                Some(Some(parent)) => {
                    if !parent_of.contains_key(parent) {
                        return Err(DomainError::DanglingParentTeam {
                            team: cursor.to_string(),
                            parent: parent.to_string(),
                        });
                    }
                    cursor = parent;
                }
                // Reached a root (no parent) — this chain is well-formed.
                Some(None) => break,
                // `cursor` is not a team. Only reachable if `team.name` itself
                // is somehow absent from its own map (impossible), so treat as
                // a terminated chain rather than inventing an error.
                None => break,
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::person::{Identity, PersonId};
    use crate::domain::source_repo_slug::SourceRepoSlug;

    fn fixture_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// Build a single-identity GitHub person, the shape `into_domain` composes.
    fn gh(id: u64, login: &str, name: &str) -> Person {
        Person::from_identities(
            PersonId::from(id),
            vec![Identity {
                provider: GITHUB_PROVIDER.to_string(),
                handle: Some(login.to_string()),
                provider_user_id: Some(id.to_string()),
            }],
            name,
            None,
        )
    }

    fn fixture_org() -> Org {
        let people = vec![
            gh(1, "alice", "Alice"),
            gh(2, "bob", "Bob"),
            gh(3, "carol", "Carol"),
        ];
        let teams = vec![
            Team::new(
                "backend",
                vec![SourceRepoSlug::new("alderco", "api").unwrap()],
            )
            .unwrap(),
            Team::new("frontend", vec![]).unwrap(),
        ];
        Org::new(
            "alpha-corp",
            "Alpha Corp",
            fixture_time(),
            Some("git@github.com:alpha/context.git".to_string()),
            OrgConfigFlags::default(),
            people,
            teams,
        )
        .unwrap()
    }

    #[test]
    fn happy_path() {
        let o = fixture_org();
        assert_eq!(o.slug, "alpha-corp");
        assert_eq!(o.teams.len(), 2);
    }

    #[test]
    fn rejects_empty_slug() {
        let err = Org::new(
            "",
            "X",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            vec![],
            vec![],
        );
        assert_eq!(err, Err(DomainError::EmptyField { field: "slug" }));
    }

    #[test]
    fn rejects_whitespace_only_slug() {
        let err = Org::new(
            "   ",
            "X",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            vec![],
            vec![],
        );
        assert_eq!(err, Err(DomainError::EmptyField { field: "slug" }));
    }

    #[test]
    fn rejects_whitespace_only_display_name() {
        let err = Org::new(
            "x",
            "   ",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            vec![],
            vec![],
        );
        assert_eq!(
            err,
            Err(DomainError::EmptyField {
                field: "display_name"
            })
        );
    }

    #[test]
    fn rejects_empty_display_name() {
        let err = Org::new(
            "x",
            "",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            vec![],
            vec![],
        );
        assert_eq!(
            err,
            Err(DomainError::EmptyField {
                field: "display_name"
            })
        );
    }

    #[test]
    fn rejects_duplicate_person() {
        let people = vec![gh(1, "alice", "Alice"), gh(1, "alice2", "Alice 2")];
        let err = Org::new(
            "x",
            "X",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            people,
            vec![],
        );
        assert_eq!(err, Err(DomainError::DuplicatePerson(PersonId::from(1u64))));
    }

    #[test]
    fn rejects_duplicate_team() {
        let people = vec![gh(1, "alice", "Alice")];
        let teams = vec![
            Team::new("backend", vec![]).unwrap(),
            Team::new("backend", vec![]).unwrap(),
        ];
        let err = Org::new(
            "x",
            "X",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            people,
            teams,
        );
        assert_eq!(err, Err(DomainError::DuplicateTeam("backend".into())));
    }

    #[test]
    fn team_lookup() {
        let o = fixture_org();
        assert!(o.team("backend").is_some());
        assert!(o.team("nope").is_none());
    }

    /// Build an org whose teams carry the given `(name, parent)` links.
    /// Members are empty so no `people` entries are required.
    fn org_with_parents(links: &[(&str, Option<&str>)]) -> Org {
        let teams: Vec<Team> = links
            .iter()
            .map(|(name, parent)| {
                let mut t = Team::new(*name, vec![]).unwrap();
                t.parent_team = parent.map(String::from);
                t
            })
            .collect();
        Org::new(
            "x",
            "X",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            vec![],
            teams,
        )
        .unwrap()
    }

    /// Build an org with the given parent links WITHOUT going through
    /// SLW-IN-3's load-time chain validation: every team is created parentless
    /// (so `Org::new` accepts it), then the parent pointers — possibly a cycle
    /// or a dangling name — are injected directly. This is precisely the
    /// "chain constructed in a test, bypassing load validation" case AC-4's
    /// belt-and-suspenders walk bound must survive, now that `Org::new` rejects
    /// such chains at composition.
    fn org_with_parents_unchecked(links: &[(&str, Option<&str>)]) -> Org {
        let teams: Vec<Team> = links
            .iter()
            .map(|(name, _)| Team::new(*name, vec![]).unwrap())
            .collect();
        let mut org = Org::new(
            "x",
            "X",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            vec![],
            teams,
        )
        .unwrap();
        for (team, (_, parent)) in org.teams.iter_mut().zip(links.iter()) {
            team.parent_team = parent.map(String::from);
        }
        org
    }

    #[test]
    fn team_chain_parentless_is_length_one() {
        // AC-3: a parentless team resolves as a length-1 chain — identical to
        // today's single active team.
        let o = org_with_parents(&[("solo", None)]);
        assert_eq!(o.team_chain("solo"), vec!["solo".to_string()]);
    }

    #[test]
    fn team_chain_walks_linear_ancestry_nearest_first() {
        // a → b → c: nearest-first [a, b, c].
        let o = org_with_parents(&[("a", Some("b")), ("b", Some("c")), ("c", None)]);
        assert_eq!(
            o.team_chain("a"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        // Starting mid-chain walks only the remaining ancestry.
        assert_eq!(o.team_chain("b"), vec!["b".to_string(), "c".to_string()]);
    }

    #[test]
    fn team_chain_cycle_terminates() {
        // AC-4: a → b → a must NOT loop; the visited-set stops at the repeat.
        // Uses the unchecked builder because `Org::new` now rejects this chain
        // at load (AC-1) — the walk bound is the belt-and-suspenders backstop.
        let o = org_with_parents_unchecked(&[("a", Some("b")), ("b", Some("a"))]);
        assert_eq!(o.team_chain("a"), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn team_chain_self_parent_terminates() {
        // AC-4: a self-parent (a → a) yields just [a], no loop. Unchecked build
        // (Org::new rejects a self-parent at load, AC-1).
        let o = org_with_parents_unchecked(&[("a", Some("a"))]);
        assert_eq!(o.team_chain("a"), vec!["a".to_string()]);
    }

    #[test]
    fn team_chain_dangling_parent_ends_at_missing_name() {
        // A dangling parent ("ghost" is not a team) is included as a name — the
        // resolver simply misses on its absent dir — then the walk terminates.
        // SLW-IN-3 now turns dangling parents into a typed load-time error
        // (AC-2), so this walk-level behavior is exercised via the unchecked
        // builder (the chain that bypassed load validation).
        let o = org_with_parents_unchecked(&[("a", Some("ghost"))]);
        assert_eq!(
            o.team_chain("a"),
            vec!["a".to_string(), "ghost".to_string()]
        );
    }

    /// Like `org_with_parents` but returns the `Result` so cycle/dangling/depth
    /// rejections can be asserted. Accepts owned pairs so callers can generate
    /// long chains programmatically.
    fn try_org_with_parents(links: &[(String, Option<String>)]) -> Result<Org, DomainError> {
        let teams: Vec<Team> = links
            .iter()
            .map(|(name, parent)| {
                let mut t = Team::new(name, vec![]).unwrap();
                t.parent_team = parent.clone();
                t
            })
            .collect();
        Org::new(
            "x",
            "X",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            vec![],
            teams,
        )
    }

    #[test]
    fn new_rejects_parent_cycle() {
        // AC-1: a → b → a is a typed cycle error at composition, naming the node
        // the walk revisits. "a" is validated first, so it reports "a".
        let err = try_org_with_parents(&[
            ("a".into(), Some("b".into())),
            ("b".into(), Some("a".into())),
        ])
        .unwrap_err();
        assert_eq!(err, DomainError::TeamParentCycle { team: "a".into() });
    }

    #[test]
    fn new_rejects_self_parent() {
        // AC-1: a self-parent (a → a) is a cycle, not an infinite loop.
        let err = try_org_with_parents(&[("a".into(), Some("a".into()))]).unwrap_err();
        assert_eq!(err, DomainError::TeamParentCycle { team: "a".into() });
    }

    #[test]
    fn new_rejects_dangling_parent() {
        // AC-2: a parent naming an absent team is a typed dangling error naming
        // the referring team and the missing parent — NOT a silent load.
        let err = try_org_with_parents(&[("a".into(), Some("ghost".into()))]).unwrap_err();
        assert_eq!(
            err,
            DomainError::DanglingParentTeam {
                team: "a".into(),
                parent: "ghost".into(),
            }
        );
    }

    #[test]
    fn new_rejects_dangling_parent_mid_chain() {
        // AC-2: the dangling error points at the exact hop that dangles, even
        // when it is not the team being validated first.
        let err = try_org_with_parents(&[
            ("a".into(), Some("b".into())),
            ("b".into(), Some("ghost".into())),
        ])
        .unwrap_err();
        assert_eq!(
            err,
            DomainError::DanglingParentTeam {
                team: "b".into(),
                parent: "ghost".into(),
            }
        );
    }

    #[test]
    fn new_rejects_chain_too_deep() {
        // AC-5: a legitimately-acyclic chain longer than MAX_CHAIN_DEPTH is a
        // typed error. Build t0 → t1 → … → tN with N = MAX_CHAIN_DEPTH + 1 so
        // the walk from t0 exceeds the cap before reaching the root.
        let n = MAX_CHAIN_DEPTH + 1;
        let links: Vec<(String, Option<String>)> = (0..=n)
            .map(|i| {
                let name = format!("t{i}");
                let parent = if i < n {
                    Some(format!("t{}", i + 1))
                } else {
                    None
                };
                (name, parent)
            })
            .collect();
        let err = try_org_with_parents(&links).unwrap_err();
        match err {
            DomainError::TeamChainTooDeep { team, depth } => {
                assert_eq!(team, "t0");
                assert_eq!(depth, MAX_CHAIN_DEPTH + 1);
            }
            other => panic!("expected TeamChainTooDeep, got {other:?}"),
        }
    }

    #[test]
    fn new_accepts_valid_multi_parent_chain() {
        // Characterization: a well-formed linear chain a → b → c composes Ok —
        // validation only adds rejection paths for malformed input.
        let org = try_org_with_parents(&[
            ("a".into(), Some("b".into())),
            ("b".into(), Some("c".into())),
            ("c".into(), None),
        ])
        .expect("valid chain should compose");
        assert_eq!(org.teams.len(), 3);
        // And the resolver-side walk still produces the full chain.
        assert_eq!(
            org.team_chain("a"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn new_accepts_chain_at_exactly_max_depth() {
        // Boundary: a chain of exactly MAX_CHAIN_DEPTH teams is accepted; only
        // exceeding it errors.
        let n = MAX_CHAIN_DEPTH; // teams t0..t(n-1), chain length == n
        let links: Vec<(String, Option<String>)> = (0..n)
            .map(|i| {
                let name = format!("t{i}");
                let parent = if i + 1 < n {
                    Some(format!("t{}", i + 1))
                } else {
                    None
                };
                (name, parent)
            })
            .collect();
        assert!(
            try_org_with_parents(&links).is_ok(),
            "a chain of exactly MAX_CHAIN_DEPTH must be accepted"
        );
    }

    #[test]
    fn team_chain_unknown_self_is_singleton() {
        // A `self_name` that isn't a team (shouldn't happen in practice) yields
        // [self] and terminates — no panic, no loop.
        let o = org_with_parents(&[("a", None)]);
        assert_eq!(o.team_chain("nope"), vec!["nope".to_string()]);
    }

    // ----- SW-LT-7: no prescribed root — nothing is appended -------------

    #[test]
    fn team_chain_parentless_is_self_only() {
        // No root team exists; a parentless team's chain is exactly `[self]`.
        let o = org_with_parents(&[("solo", None)]);
        assert_eq!(o.team_chain("solo"), vec!["solo".to_string()]);
    }

    #[test]
    fn team_chain_is_exactly_the_declared_parents() {
        // A→B→C resolves to exactly the declared ancestry, nearest-first — no
        // implicit tail appended for any team (SW-LT-7 removed the org-root
        // append). A team named `org-root` is now ordinary and NOT special.
        let o = org_with_parents(&[
            ("a", Some("b")),
            ("b", Some("org-root")),
            ("org-root", None),
        ]);
        assert_eq!(
            o.team_chain("a"),
            vec!["a".to_string(), "b".to_string(), "org-root".to_string()]
        );
        // A DIFFERENT parentless team does NOT pick up `org-root` just because it
        // exists — the old implicit append is gone.
        let o2 = org_with_parents(&[("solo", None), ("org-root", None)]);
        assert_eq!(o2.team_chain("solo"), vec!["solo".to_string()]);
    }

    #[test]
    fn person_by_username_resolves_exact_login() {
        let o = fixture_org();
        assert_eq!(
            o.person_by_username("alice")
                .and_then(|p| p.github_user_id()),
            Some(1)
        );
    }

    #[test]
    fn person_by_username_is_case_insensitive() {
        let o = fixture_org();
        // Mixed / upper case resolves the same lowercase-stored login.
        assert_eq!(
            o.person_by_username("ALICE")
                .and_then(|p| p.github_user_id()),
            Some(1)
        );
        assert_eq!(
            o.person_by_username("Bob").and_then(|p| p.github_user_id()),
            Some(2)
        );
    }

    #[test]
    fn person_by_username_trims_whitespace() {
        let o = fixture_org();
        assert_eq!(
            o.person_by_username("  carol  ")
                .and_then(|p| p.github_user_id()),
            Some(3)
        );
    }

    #[test]
    fn person_by_username_strips_leading_at() {
        // Parity with the TS twin (`usernameKey` strips a leading `@`): a
        // `@login` query resolves the same person as the bare login.
        let o = fixture_org();
        assert_eq!(
            o.person_by_username("@alice")
                .and_then(|p| p.github_user_id()),
            Some(1)
        );
    }

    #[test]
    fn person_by_username_normalizes_stored_username() {
        // The identity handle is stored verbatim (no trim, no regex), so a
        // hand-edited padded entry is possible on disk. Both query and stored
        // login are normalized, so it still resolves — matching TS, whose index
        // keys the stored login through the same `usernameKey`.
        let people = vec![gh(1, "  alice  ", "Alice")];
        let o = Org::new(
            "x",
            "X",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            people,
            vec![],
        )
        .unwrap();
        assert_eq!(
            o.person_by_username("alice")
                .and_then(|p| p.github_user_id()),
            Some(1)
        );
    }

    #[test]
    fn person_by_username_blank_or_at_only_is_none() {
        // An empty, whitespace, or `@`-only query normalizes to "" and resolves
        // to None — it must never match a (malformed) blank stored username.
        let o = fixture_org();
        assert!(o.person_by_username("").is_none());
        assert!(o.person_by_username("   ").is_none());
        assert!(o.person_by_username("@").is_none());
    }

    #[test]
    fn person_by_username_trims_ascii_whitespace_but_not_exotic() {
        // Parity with TS `usernameKey`: strip the ASCII whitespace set (here VT,
        // U+000B) but leave non-ASCII blanks (U+FEFF BOM) verbatim, so a
        // hand-edited exotic-whitespace login resolves to nothing on BOTH sides
        // (never one-side-only). `str::trim` would wrongly strip neither/both.
        let o = fixture_org();
        assert_eq!(
            o.person_by_username("\u{000b}alice\u{000b}")
                .and_then(|p| p.github_user_id()),
            Some(1)
        );
        assert!(o.person_by_username("\u{feff}alice").is_none());
    }

    #[test]
    fn person_by_username_strips_only_one_leading_at() {
        // Parity with TS `replace(/^@/, "")`: exactly one leading `@` is
        // stripped, so a doubled `@@alice` no longer resolves.
        let o = fixture_org();
        assert_eq!(
            o.person_by_username("@alice")
                .and_then(|p| p.github_user_id()),
            Some(1)
        );
        assert!(o.person_by_username("@@alice").is_none());
    }

    #[test]
    fn person_by_username_folds_ascii_case_only() {
        // ASCII letters fold; a non-ASCII letter is matched byte-exact — the
        // same contract as TS `usernameKey`, so the two resolvers agree on an
        // accented login. `CAFé` folds to the stored `café`; `CAFÉ` does not
        // (É is not folded to é).
        let people = vec![gh(1, "café", "Café")];
        let o = Org::new(
            "x",
            "X",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            people,
            vec![],
        )
        .unwrap();
        assert_eq!(
            o.person_by_username("CAFé")
                .and_then(|p| p.github_user_id()),
            Some(1)
        );
        assert!(o.person_by_username("CAFÉ").is_none());
    }

    #[test]
    fn person_by_username_unknown_is_none() {
        let o = fixture_org();
        assert!(o.person_by_username("nobody").is_none());
    }

    #[test]
    fn person_by_username_empty_roster_is_none() {
        let o = Org::new(
            "x",
            "X",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            vec![],
            vec![],
        )
        .unwrap();
        assert!(o.person_by_username("alice").is_none());
    }

    #[test]
    fn person_by_username_duplicate_login_is_first_wins() {
        // `Org::new` enforces uniqueness on id, NOT login — two ids can share a
        // case-folded login. Resolution is deterministic first-wins in
        // people.yml order (never panics, never ambiguous).
        let people = vec![gh(1, "octo", "Octo One"), gh(2, "OCTO", "Octo Two")];
        let o = Org::new(
            "x",
            "X",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            people,
            vec![],
        )
        .unwrap();
        assert_eq!(
            o.person_by_username("octo")
                .and_then(|p| p.github_user_id()),
            Some(1)
        );
    }

    #[test]
    fn person_by_handle_is_provider_scoped_and_handle_less_misses() {
        // AC-4/AC-6: person_by_handle resolves per provider (a handle is not unique
        // across forges); a GitHub+GitLab person resolves under BOTH providers; the
        // github shim still resolves by login; a handle-less person misses every
        // by-handle lookup (present/assignable by id, not correlatable).
        let alice = Person::from_identities(
            PersonId::from(1u64),
            vec![
                Identity {
                    provider: GITHUB_PROVIDER.to_string(),
                    handle: Some("alice".into()),
                    provider_user_id: Some("1".into()),
                },
                Identity {
                    provider: "gitlab".into(),
                    handle: Some("alice-gl".into()),
                    provider_user_id: Some("gl-1".into()),
                },
            ],
            "Alice",
            None,
        );
        let ghost = Person::from_identities(
            PersonId::from(2u64),
            vec![Identity {
                provider: GITHUB_PROVIDER.to_string(),
                handle: None,
                provider_user_id: Some("2".into()),
            }],
            "Ghost",
            None,
        );
        let o = Org::new(
            "x",
            "X",
            fixture_time(),
            None,
            OrgConfigFlags::default(),
            vec![alice, ghost],
            vec![],
        )
        .unwrap();

        // GitLab-scoped resolves Alice by her gitlab handle...
        assert_eq!(
            o.person_by_handle("gitlab", "alice-gl")
                .and_then(|p| p.github_user_id()),
            Some(1)
        );
        // ...and the github shim still resolves her by login.
        assert_eq!(
            o.person_by_username("alice")
                .and_then(|p| p.github_user_id()),
            Some(1)
        );
        // Cross-provider miss: her github login is not a gitlab handle.
        assert!(o.person_by_handle("gitlab", "alice").is_none());
        // The handle-less github person is present but not correlatable by handle.
        assert!(o.person_by_username("ghost").is_none());
        assert!(o.person_by_handle("github", "ghost").is_none());
        assert!(o.people.iter().any(|p| p.id == PersonId::from(2u64)));
    }

    #[test]
    fn serde_roundtrip_full_fixture() {
        let o = fixture_org();
        let json = serde_json::to_string_pretty(&o).unwrap();
        let back: Org = serde_json::from_str(&json).unwrap();
        assert_eq!(o, back);
    }

    #[test]
    fn serde_defaults_kick_in_when_optional_fields_omitted() {
        // Minimal JSON that omits config_flags, remote_url, people, teams —
        // verifies #[serde(default)] survives.
        let json = r#"{
            "slug": "x",
            "display_name": "X",
            "created_at": "2026-01-01T00:00:00Z"
        }"#;
        let o: Org = serde_json::from_str(json).unwrap();
        assert_eq!(o.remote_url, None);
        assert_eq!(o.config_flags, OrgConfigFlags::default());
        assert!(o.people.is_empty());
        assert!(o.teams.is_empty());
        // SW-FG-9 AC-3: a pre-field Org JSON loads unbound — the resolver then
        // takes the implicit-active path, byte-for-byte today's behavior.
        assert_eq!(o.credential_ref, None);

        // An explicit `"credential_ref": null` is equivalent to absent — also None.
        let with_null = r#"{
            "slug": "x",
            "display_name": "X",
            "created_at": "2026-01-01T00:00:00Z",
            "credential_ref": null
        }"#;
        let o: Org = serde_json::from_str(with_null).unwrap();
        assert_eq!(o.credential_ref, None);
    }

    // SW-FG-9 AC-1: a bound `credential_ref` round-trips (and spans providers —
    // a gitlab binding on an Org whose remote looks like github is legitimate),
    // while the unbound default fixture round-trips as `None`.
    #[test]
    fn credential_ref_roundtrips_bound_and_unbound() {
        // Unbound: the default fixture carries no binding.
        let unbound = fixture_org();
        assert_eq!(unbound.credential_ref, None);
        let back: Org = serde_json::from_str(&serde_json::to_string(&unbound).unwrap()).unwrap();
        assert_eq!(unbound, back);

        // Bound: set the binding post-construction (as FG-10b/10c will) and
        // prove it survives a serialize → deserialize round-trip.
        let mut bound = fixture_org();
        bound.credential_ref = Some(crate::domain::CredentialRef {
            provider: "gitlab".into(),
            account: "42".into(),
        });
        let back: Org = serde_json::from_str(&serde_json::to_string(&bound).unwrap()).unwrap();
        assert_eq!(bound, back);
        assert_eq!(
            back.credential_ref,
            Some(crate::domain::CredentialRef {
                provider: "gitlab".into(),
                account: "42".into(),
            })
        );
    }
}
