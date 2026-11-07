// SPDX-License-Identifier: MIT
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::DomainError;
use super::org::{Org, OrgConfigFlags};
use super::person::{Identity, Person, PersonId};
use super::source_repo_slug::SourceRepoSlug;
use super::team::{is_slug_safe, Team};

// ---------- Raw shape structs ----------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawOrgYml {
    pub slug: String,
    pub display_name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub config_flags: OrgConfigFlags,
    /// SW-DX-1: the optional team-defined tag registry, authored as an additive
    /// `tag_schema:` key on `org.yml`. Captured as a **raw `serde_yaml::Value`**
    /// (not a strict `TagSchema`) on purpose:
    /// - **lossless re-serialize** — the org-rename write-back
    ///   (`context_repo::mutate`) round-trips `RawOrgYml`; a raw value re-emits
    ///   the authored schema verbatim rather than rewriting an unknown/aliased
    ///   `type:` or reflowing the block;
    /// - **shape-total tolerance** — a malformed schema shape can't fail the
    ///   whole deserialize (which would drop teams/people); the lenient
    ///   [`super::TagSchema::from_raw_value`] converts it in `into_org`.
    ///
    /// `#[serde(default)]` ⇒ absent parses as `Value::Null`; `skip_serializing_if`
    /// on null keeps a never-opted-in `org.yml` byte-stable.
    #[serde(default, skip_serializing_if = "serde_yaml::Value::is_null")]
    pub tag_schema: serde_yaml::Value,
}

// Does NOT set `deny_unknown_fields` so legacy `org.yml` files with
// retired keys (e.g. `is_personal`) load cleanly. Unknown keys are
// silently dropped on read.

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawPeopleYml {
    #[serde(default)]
    pub people: Vec<RawPersonEntry>,
}

/// SW-FG-4: the provider id used to key the GitHub identity row. GitHub stays a
/// first-class provider for v1 (the domain `Person` is still GitHub-keyed), so
/// this string is the one place the provider name is spelled — the write
/// producers mint their identity through [`RawPersonEntry::github`], never the
/// literal.
pub(crate) const GITHUB_PROVIDER: &str = "github";

/// SW-FG-4 (AC-1): one provider identity within a person's roster entry. A person
/// holds a LIST of these so one human can carry a GitHub handle AND a GitLab
/// handle in a single org roster (Alice = `alice` on GitHub, `alice-gl` on
/// GitLab). `provider_user_id` is a `String` so an opaque provider id (Bitbucket
/// `account_id`/UUID) fits — GitHub's numeric id renders to its decimal string,
/// byte-identical to the value FG-1 already stores. Both `handle` and
/// `provider_user_id` are optional (AC-5): a UI-added person with no handle yet
/// is an identity row with a null handle.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct RawIdentity {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_user_id: Option<String>,
}

/// SW-FG-4b: the raw→domain identity crossing (the two shapes are field-identical,
/// but kept distinct types so the raw/domain boundary holds — AC-1).
impl From<RawIdentity> for Identity {
    fn from(raw: RawIdentity) -> Self {
        Identity {
            provider: raw.provider,
            handle: raw.handle,
            provider_user_id: raw.provider_user_id,
        }
    }
}

/// SW-FG-4 (AC-1): a person now carries a LIST of provider identities. Legacy
/// files with the flat `github_user_id`/`github_username` scalars still load via
/// [`RawPersonEntryWire`] (AC-1b) and are re-emitted in the new shape on the next
/// write (migrate-on-load, AC-3). `#[serde(from)]` — mirroring FG-1b's
/// `AuthFileWire` — keeps the fold infallible and unit-testable; the legacy
/// scalars are the only GitHub-named residue, dropped in FG-12.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(from = "RawPersonEntryWire")]
pub struct RawPersonEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<RawIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

/// SW-FG-4: read-only legacy bridge for [`RawPersonEntry`]'s deserializer,
/// mirroring FG-1b's `AuthFileWire` (`commands/auth.rs`). A new-shape entry nests
/// provider rows under `identities:`; a legacy entry carries the flat
/// `github_user_id` / `github_username` scalars. A present `identities` list
/// WINS over the legacy scalars (a half-migrated file never silently drops a
/// provider), and the `From` is infallible so a malformed piece defaults rather
/// than failing the whole roster. Dropped in FG-12.
#[derive(Deserialize)]
pub struct RawPersonEntryWire {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    identities: Vec<RawIdentity>,
    #[serde(default)]
    github_user_id: Option<u64>,
    #[serde(default)]
    github_username: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    avatar_url: Option<String>,
}

impl From<RawPersonEntryWire> for RawPersonEntry {
    fn from(wire: RawPersonEntryWire) -> Self {
        // New `identities:` wins; fall back to folding the legacy scalars into a
        // one-element GitHub identity (AC-1b) only when no identities are present.
        let identities = if !wire.identities.is_empty() {
            wire.identities
        } else if wire.github_user_id.is_some() || wire.github_username.is_some() {
            vec![RawIdentity {
                provider: GITHUB_PROVIDER.to_string(),
                handle: wire.github_username,
                provider_user_id: wire.github_user_id.map(|id| id.to_string()),
            }]
        } else {
            Vec::new()
        };
        RawPersonEntry {
            id: wire.id,
            identities,
            display_name: wire.display_name,
            avatar_url: wire.avatar_url,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawTeamsYml {
    #[serde(default)]
    pub teams: Vec<RawTeamEntry>,
}

// SW-TM-3: `member_ids`/`lead_ids` were dropped once SW-OB-7 made membership a
// non-gate. There is no `deny_unknown_fields`, so an OLD `teams.yml` still
// listing those keys parses cleanly (they're ignored) and they're dropped on the
// next re-serialize.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawTeamEntry {
    pub name: String,
    #[serde(default)]
    pub source_repos: Vec<RawSourceRepoEntry>,
    /// SW-HB-7: org-wide, reversible team retirement. Additive + back-compat —
    /// `#[serde(default)]` so a pre-field `teams.yml` parses as all-active, and
    /// `skip_serializing_if` so an untouched (active) team's YAML bytes are
    /// unchanged (AC-2). Absent ⇔ active.
    #[serde(default, skip_serializing_if = "is_false")]
    pub archived: bool,
    /// SLW-IN-1: single parent team, or absent for a root team. `#[serde(default)]`
    /// so a pre-field `teams.yml` parses as all-roots, and `skip_serializing_if`
    /// so a parentless team's YAML bytes are unchanged (AC-3). Absent ⇔ root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_team: Option<String>,
    /// The team's home page, opened in the viewer whenever it has nothing open; relative to the
    /// team's content root (`{org_root}/{team}/content`). Absent ⇒ the app falls
    /// back to README.md / README.html / index.html there. `skip_serializing_if`
    /// keeps teams without one byte-stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landing_page: Option<String>,
}

/// serde `skip_serializing_if` predicate: omit `archived` from the emitted
/// YAML when it's `false`, keeping active teams' bytes byte-stable (AC-2).
fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawSourceRepoEntry {
    pub owner: String,
    pub repo: String,
    /// SW-FG-10a: which forge this repo lives on, by the same provider id the
    /// connection store uses (`commands/providers.rs`, e.g. `"github"`,
    /// `"jira"`). Read logic through [`provider_id`](Self::provider_id), not this
    /// raw field — the field preserves the authored bytes, `provider_id`
    /// normalizes them to the canonical id. `#[serde(default)]` → github so a
    /// legacy bare entry migrates on load, and `skip_serializing_if` keeps an
    /// unchanged (github) entry's YAML bytes stable across a re-serialize
    /// (`teams.yml` is rewritten by `team_rename`/`mutate`) — note this also
    /// elides an *explicitly* authored `provider: github` on rewrite, which is
    /// intended (it re-defaults to github on load). The default-to-github hook is
    /// reaped by SW-FG-12.
    #[serde(default = "default_provider", skip_serializing_if = "is_github")]
    pub provider: String,
    /// SW-FG-10a: optional self-hosted host (Gitea/Forgejo). Absent for cloud
    /// providers whose id already implies the host. Absent ⇔ cloud. Carried on
    /// the entry (round-trips) but NOT yet consumed by the provider reconcile —
    /// host-aware reconciliation lands with the self-hosted providers that give
    /// it meaning (a later FG story). Absent ⇔ cloud.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

impl RawSourceRepoEntry {
    /// A github-hosted entry — the single source of the github default for
    /// hand-construction (the add-repo edit path). The `provider` field also
    /// defaults to github on deserialize; this keeps both paths in agreement.
    pub fn github(owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            provider: default_provider(),
            host: None,
        }
    }

    /// The normalized provider **id** for this entry — the value the reconcile
    /// and the connection store key on. `teams.yml` is hand-authored, so trim +
    /// lowercase the raw `provider` (`GitHub` / `Jira ` resolve to the canonical
    /// `github` / `jira` ids the connected/supported sets use), and treat a
    /// blank value the same as an absent one: the github default. Without this,
    /// a case/whitespace slip would classify a fully-supported forge as
    /// `unsupported`. The raw `provider` field is left as authored on disk.
    pub fn provider_id(&self) -> String {
        let normalized = self.provider.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            default_provider()
        } else {
            normalized
        }
    }
}

/// serde `default` for [`RawSourceRepoEntry::provider`]: legacy bare entries
/// migrate to `github` on load (SW-FG-10a; reaped by SW-FG-12).
fn default_provider() -> String {
    "github".to_string()
}

/// serde `skip_serializing_if` predicate: omit `provider` from emitted YAML when
/// it's the `github` default, keeping legacy entries' bytes byte-stable.
fn is_github(provider: &str) -> bool {
    provider == "github"
}

// ---------- Error type ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegistryFile {
    OrgYml,
    PeopleYml,
    TeamsYml,
}

impl RegistryFile {
    pub fn filename(self) -> &'static str {
        match self {
            Self::OrgYml => "org.yml",
            Self::PeopleYml => "people.yml",
            Self::TeamsYml => "teams.yml",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    InvalidUtf8 {
        file: RegistryFile,
    },
    Yaml {
        file: RegistryFile,
        /// Field path from `serde_path_to_error`, e.g. `.teams[0].source_repos[2]`.
        /// Empty when the error occurred before the deserializer walked any field.
        path: String,
        /// 1-indexed; `0` is the "unknown" sentinel when location isn't available.
        line: usize,
        /// 1-indexed; `0` is the "unknown" sentinel.
        column: usize,
        message: String,
    },
    Invariant {
        file: RegistryFile,
        source: DomainError,
    },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUtf8 { file } => {
                write!(f, "{} is not valid UTF-8", file.filename())
            }
            Self::Yaml {
                file,
                path,
                line,
                column,
                message,
            } => {
                if path.is_empty() {
                    write!(
                        f,
                        "{} failed to parse at line {}, column {}: {}",
                        file.filename(),
                        line,
                        column,
                        message
                    )
                } else {
                    write!(
                        f,
                        "{} failed to parse at `{}` (line {}, column {}): {}",
                        file.filename(),
                        path,
                        line,
                        column,
                        message
                    )
                }
            }
            Self::Invariant { file, source } => {
                write!(
                    f,
                    "{} violated a domain invariant: {}",
                    file.filename(),
                    source
                )
            }
        }
    }
}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invariant { source, .. } => Some(source),
            _ => None,
        }
    }
}

// ---------- Parse functions (source-agnostic) ----------

pub fn parse_org_yml(bytes: &[u8]) -> Result<RawOrgYml, ParseError> {
    parse_with_path::<RawOrgYml>(bytes, RegistryFile::OrgYml)
}

pub fn parse_people_yml(bytes: &[u8]) -> Result<RawPeopleYml, ParseError> {
    let people = parse_with_path::<RawPeopleYml>(bytes, RegistryFile::PeopleYml)?;
    let mut ids = std::collections::HashSet::new();
    for person in &people.people {
        if let Some(id) = person.person_id() {
            if !ids.insert(id.clone()) {
                return Err(ParseError::Invariant {
                    file: RegistryFile::PeopleYml,
                    source: DomainError::DuplicatePerson(id),
                });
            }
        }
    }
    Ok(people)
}

pub fn parse_teams_yml(bytes: &[u8]) -> Result<RawTeamsYml, ParseError> {
    parse_with_path::<RawTeamsYml>(bytes, RegistryFile::TeamsYml)
}

fn parse_with_path<T>(bytes: &[u8], file: RegistryFile) -> Result<T, ParseError>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let text = std::str::from_utf8(bytes).map_err(|_| ParseError::InvalidUtf8 { file })?;
    // Strip leading UTF-8 BOM(s). `trim_start_matches` handles the
    // (rare) double-BOM case that arises when a file gets saved twice by
    // a BOM-writing editor.
    let text = text.trim_start_matches('\u{feff}');

    // Wrap serde_yaml's Deserializer with serde_path_to_error so we capture
    // the field path where parsing failed. Do NOT replace this with
    // `serde_yaml::from_str` — that bypasses the path tracking entirely.
    let de = serde_yaml::Deserializer::from_str(text);
    serde_path_to_error::deserialize::<_, T>(de).map_err(|err| {
        let path = err.path().to_string();
        let yaml_err = err.into_inner();
        ParseError::Yaml {
            file,
            // serde_path_to_error renders root as "." — normalize to empty
            // so the Display impl uses the shorter no-path form.
            path: if path == "." { String::new() } else { path },
            // NOTE: non-root paths render without a leading dot
            // (e.g. "people[1].github_user_id"), not with one.
            line: yaml_err.location().map(|l| l.line()).unwrap_or(0),
            column: yaml_err.location().map(|l| l.column()).unwrap_or(0),
            message: yaml_err.to_string(),
        }
    })
}

// ---------- Raw → domain conversion ----------

impl RawPersonEntry {
    /// Stable roster key: preserve a persisted CodeSense-owned id when present,
    /// otherwise derive the legacy key from the GitHub identity.
    pub fn person_id(&self) -> Option<PersonId> {
        if let Some(id) = self
            .id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            return Some(PersonId::from(id));
        }
        if let Some(github_id) = self.github_user_id().filter(|&id| id != 0) {
            return Some(PersonId::from(github_id));
        }
        None
    }
    /// SW-FG-4 DRY seam: mint a roster entry from a GitHub profile. The ONE place
    /// the three `people.yml` write producers (`registry_pr`, `mutate`, `org`)
    /// build the one-element GitHub identity, so none of them hand-roll the
    /// provider string — the direct replacement for the two-scalar constructor
    /// they used before.
    pub fn github(
        github_user_id: u64,
        github_username: impl Into<String>,
        display_name: Option<String>,
        avatar_url: Option<String>,
    ) -> Self {
        RawPersonEntry {
            id: None,
            identities: vec![RawIdentity {
                provider: GITHUB_PROVIDER.to_string(),
                handle: Some(github_username.into()),
                provider_user_id: Some(github_user_id.to_string()),
            }],
            display_name,
            avatar_url,
        }
    }

    /// The GitHub identity row, if this person carries one (first match wins).
    /// `person_by_username` + the FE twin still resolve against the GitHub handle
    /// (FG-4 non-goal: no second identity↔roster crossing), so this is the single
    /// accessor the domain bridge reads.
    pub fn github_identity(&self) -> Option<&RawIdentity> {
        self.identities
            .iter()
            .find(|i| i.provider == GITHUB_PROVIDER)
    }

    /// The GitHub numeric id (parsed from the github identity's
    /// `provider_user_id`), used as the add-if-absent roster key by the write
    /// producers. `None` for a non-GitHub or malformed entry.
    pub fn github_user_id(&self) -> Option<u64> {
        self.github_identity()
            .and_then(|i| i.provider_user_id.as_deref())
            .and_then(|s| s.parse::<u64>().ok())
    }

    /// The GitHub handle (login) from the github identity row, if present. `None`
    /// for a handle-less (AC-5) or non-GitHub entry.
    pub fn github_handle(&self) -> Option<&str> {
        self.github_identity().and_then(|i| i.handle.as_deref())
    }

    /// Compose the domain [`Person`] from this roster entry, or `Ok(None)` when the
    /// entry is valid config the v1 domain intentionally doesn't compose yet.
    ///
    /// SW-FG-4b: a row composes when it carries a GitHub identity yielding a usable
    /// id — a numeric, non-zero `provider_user_id` — **even when its `handle` is
    /// null** (the "added, not yet linked" teammate; AC-2/AC-6). It is SKIPPED
    /// (`Ok(None)`, never a hard error that would drop every person AND team) for:
    /// a genuinely empty row (no identity, nothing to render), a GitHub identity
    /// with an unusable id (zero / non-numeric / half-migrated), or a row with **no
    /// GitHub identity** (a pure-non-GitHub teammate — deferred to FG-13, which
    /// gives those people a minted [`PersonId`]). Skipped rows still round-trip in
    /// the config. The whole identity list is carried onto the `Person`, so a
    /// GitHub + GitLab row keeps its GitLab identity for FG-13 to correlate.
    pub fn into_domain(self) -> Result<Option<Person>, DomainError> {
        // SW-FG-4: one identity per provider is the invariant. Two `github` rows in
        // one entry is a hand-edit / merge artifact — resolve first-wins
        // (deterministic, via `github_identity`) but warn so the ambiguity is
        // visible rather than a silently-dropped second identity.
        let github_rows = self
            .identities
            .iter()
            .filter(|i| i.provider == GITHUB_PROVIDER)
            .count();
        if github_rows > 1 {
            log::warn!(
                target: "domain::raw_yaml",
                "people.yml roster entry carries {github_rows} github identities; \
                 using the first and ignoring the rest (one identity per provider is expected)."
            );
        }
        let Some(id) = self.person_id() else {
            return Ok(None);
        };
        let identities = self.identities.into_iter().map(Identity::from).collect();
        // Carry the display name faithfully (empty when unset); the id/handle
        // fallback for a label lives in `Person::display_name_or_username`.
        let display_name = self.display_name.unwrap_or_default();
        Ok(Some(Person::from_identities(
            id,
            identities,
            display_name,
            self.avatar_url,
        )))
    }
}

impl RawTeamEntry {
    pub fn into_domain(self) -> Result<Team, DomainError> {
        let source_repos = self
            .source_repos
            .into_iter()
            .filter_map(|r| {
                let provider = r.provider_id().to_string();
                match SourceRepoSlug::with_provider_and_host(r.owner, r.repo, provider, r.host) {
                    Ok(slug) => Some(slug),
                    Err(error) => {
                        log::warn!(
                            target: "domain::raw_yaml",
                            "skipping malformed source repository row: {error}"
                        );
                        None
                    }
                }
            })
            .collect();
        let mut team = Team::new(self.name, source_repos)?;
        // SW-HB-7: carry the on-disk archived flag onto the validated domain
        // type so the frontend `Team.archived` mirror and the archived-filter
        // (V-1) can read it. Not a `Team::new` param — it's not a validity
        // concern, just state.
        team.archived = self.archived;
        // SLW-IN-1: carry the parent link, validating only its slug-shape (reuse
        // the same `is_slug_safe` rule as `Team::name`). Reachability/acyclicity
        // is deferred to SLW-IN-3, so we do NOT check the parent exists here.
        if let Some(parent) = &self.parent_team {
            if !is_slug_safe(parent) {
                return Err(DomainError::InvalidParentTeam(parent.clone()));
            }
        }
        team.parent_team = self.parent_team;
        // A landing page is a convenience, never a reason to refuse the team: one
        // that would leave the team's content (absolute, `..`) or name a hidden
        // file or folder is dropped.
        team.landing_page = match self.landing_page.map(|p| p.trim().to_string()) {
            Some(page) if is_content_relative(&page) => Some(page),
            Some(page) => {
                log::warn!(
                    target: "domain::raw_yaml",
                    "ignoring landing_page {page:?} of team {}: it must be a path inside the team's content",
                    team.name
                );
                None
            }
            None => None,
        };
        Ok(team)
    }
}

/// A relative `/`-separated path with no empty segment and no hidden one (a
/// segment starting with `.`, which also rules out `.` and `..`): a page the app
/// may open on its own never leaves the team's content nor names a dotfile
/// (`.env`) or a file under a dot-folder (`.claude/`, `.git/`).
pub(crate) fn is_content_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|seg| !seg.is_empty() && !seg.starts_with('.'))
}

// ---------- Classification table ----------

/// Classifies a `DomainError` to the registry file a user would edit to fix it,
/// and the absolute filesystem path to that file under `org_root`.
pub(crate) fn classify_domain_error(err: &DomainError, org_root: &Path) -> (RegistryFile, PathBuf) {
    let file = match err {
        DomainError::EmptyField { field } if *field == "slug" || *field == "display_name" => {
            RegistryFile::OrgYml
        }
        DomainError::EmptyField { field }
            if *field == "name" || *field == "owner" || *field == "repo" =>
        {
            RegistryFile::TeamsYml
        }
        DomainError::InvalidTeamName(_) => RegistryFile::TeamsYml,
        DomainError::InvalidParentTeam(_) => RegistryFile::TeamsYml,
        DomainError::DuplicateSourceRepo(_) => RegistryFile::TeamsYml,
        DomainError::InvalidSourceRepoSegment { .. } => RegistryFile::TeamsYml,
        DomainError::DuplicatePerson(_) => RegistryFile::PeopleYml,
        DomainError::DuplicateTeam(_) => RegistryFile::TeamsYml,
        // SLW-IN-3: the parent_team chain invariants are all fixed by editing
        // teams.yml, so they classify there (→ "Open to repair → teams.yml").
        DomainError::TeamParentCycle { .. } => RegistryFile::TeamsYml,
        DomainError::DanglingParentTeam { .. } => RegistryFile::TeamsYml,
        DomainError::TeamChainTooDeep { .. } => RegistryFile::TeamsYml,
        // Fallthrough for any new EmptyField field name — default to org.yml.
        DomainError::EmptyField { .. } => RegistryFile::OrgYml,
    };
    (file, org_root.join(file.filename()))
}

fn wrap_domain_err(err: DomainError) -> ParseError {
    let (file, _path) = classify_domain_error(&err, Path::new(""));
    ParseError::Invariant { file, source: err }
}

// ---------- RawOrgComposition ----------

use super::loaded_org::PartialOrg;

pub struct RawOrgComposition {
    pub raw_org: RawOrgYml,
    pub people: Vec<Person>,
    pub teams: Vec<Team>,
    pub remote_url: Option<String>,
}

impl RawOrgComposition {
    /// Composes the per-entry raw shapes into validated domain types.
    /// Caller must ensure `partial.org_yml`, `partial.people_yml`, and
    /// `partial.teams_yml` are all `Some` — the loader guards this via its
    /// `errors.is_empty()` check before calling.
    pub fn from_partial(
        partial: &PartialOrg,
        remote_url: Option<String>,
    ) -> Result<Self, ParseError> {
        let raw_org = partial
            .org_yml
            .as_ref()
            .expect("from_partial called without org_yml");
        let raw_people = partial
            .people_yml
            .as_ref()
            .expect("from_partial called without people_yml");
        let raw_teams = partial
            .teams_yml
            .as_ref()
            .expect("from_partial called without teams_yml");

        // SW-FG-4: `into_domain` yields `None` for a valid but not-yet-composable
        // row (a pure-non-GitHub teammate, or a handle-less GitHub identity); skip
        // those so one such row can't fail the whole compose. A genuine invariant
        // error still propagates. The skip is logged (a dropped teammate is worth a
        // breadcrumb, like the org.yml slug-mismatch warn) — the hint is any handle
        // on the row, since a composable GitHub id/handle is exactly what's missing.
        let mut people = Vec::with_capacity(raw_people.people.len());
        for raw in raw_people.people.iter().cloned() {
            let handle_hint = raw
                .identities
                .iter()
                .find_map(|i| i.handle.as_deref())
                .map(str::to_string);
            if let Some(person) = raw.into_domain().map_err(wrap_domain_err)? {
                people.push(person);
            } else {
                log::warn!(
                    target: "domain::raw_yaml",
                    "skipping a people.yml roster entry (handle hint: {handle_hint:?}) with no composable \
                     GitHub identity from the v1 domain roster; it remains in the config."
                );
            }
        }

        let teams = raw_teams
            .teams
            .iter()
            .cloned()
            .map(RawTeamEntry::into_domain)
            .collect::<Result<Vec<_>, _>>()
            .map_err(wrap_domain_err)?;

        Ok(Self {
            raw_org: raw_org.clone(),
            people,
            teams,
            remote_url,
        })
    }

    /// Composes the domain `Org`, using `dir_slug` (the on-disk org directory
    /// name) as the authoritative `Org.slug`. `org.yml::slug` is informational
    /// only — see [`reconcile_org_slug`]; a mismatch is logged but never used
    /// for path math.
    pub fn into_org(self, dir_slug: String) -> Result<Org, ParseError> {
        if let Some(yaml_slug) = reconcile_org_slug(&dir_slug, &self.raw_org.slug) {
            log::warn!(
                target: "domain::raw_yaml",
                "org.yml::slug ({yaml_slug:?}) does not match the org directory name ({dir_slug:?}); \
                 using the directory name for Org.slug. org.yml::slug is informational only."
            );
        }
        // SW-DX-1: carry the org-authored tag registry onto the validated domain
        // (like `Team.archived`, it's state — not a validity concern — so it's
        // set after construction rather than being an `Org::new` param). The raw
        // value is leniently converted so a malformed schema degrades to empty
        // instead of failing the whole org compose.
        let tag_schema = super::TagSchema::from_raw_value(&self.raw_org.tag_schema);
        let mut org = Org::new(
            dir_slug,
            self.raw_org.display_name,
            self.raw_org.created_at,
            self.remote_url,
            self.raw_org.config_flags,
            self.people,
            self.teams,
        )
        .map_err(wrap_domain_err)?;
        org.tag_schema = tag_schema;
        Ok(org)
    }
}

/// Decides which slug is authoritative when composing an [`Org`].
///
/// The directory name under `users/{uid}/orgs/` is the single source of truth
/// for path math — it's the value `path_to_org_root` joins and the watcher
/// reads from. `org.yml::slug` is informational only and can drift (it's
/// editable in the context repo and shared across machines via git).
///
/// Returns the discarded `yaml_slug` when the two disagree (so the caller can
/// log the drift), or `None` when they match.
fn reconcile_org_slug<'a>(dir_slug: &str, yaml_slug: &'a str) -> Option<&'a str> {
    if dir_slug == yaml_slug {
        None
    } else {
        Some(yaml_slug)
    }
}

// ---------- Tests ----------

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- parse_org_yml ----------

    #[test]
    fn parse_org_happy_path() {
        let yml = br#"
slug: alpha-corp
display_name: Alpha Corp
created_at: "2026-01-01T00:00:00Z"
"#;
        let raw = parse_org_yml(yml).unwrap();
        assert_eq!(raw.slug, "alpha-corp");
        assert_eq!(raw.display_name, "Alpha Corp");
    }

    #[test]
    fn org_yml_loads_legacy_is_personal_field_gracefully() {
        let yml_true = br#"
slug: test--ctx
display_name: Test
created_at: "2026-04-16T00:00:00Z"
is_personal: true
"#;
        let raw = parse_org_yml(yml_true).unwrap();
        assert_eq!(raw.slug, "test--ctx");

        let yml_false = br#"
slug: test--ctx
display_name: Test
created_at: "2026-04-16T00:00:00Z"
is_personal: false
"#;
        let raw = parse_org_yml(yml_false).unwrap();
        assert_eq!(raw.slug, "test--ctx");
    }

    #[test]
    fn org_yml_loads_without_is_personal_cleanly() {
        let yml = br#"
slug: test--ctx
display_name: Test
created_at: "2026-04-16T00:00:00Z"
"#;
        let raw = parse_org_yml(yml).unwrap();
        assert_eq!(raw.slug, "test--ctx");
    }

    #[test]
    fn org_yml_writer_does_not_emit_is_personal() {
        let raw = RawOrgYml {
            slug: "test".into(),
            display_name: "Test".into(),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            config_flags: OrgConfigFlags::default(),
            tag_schema: serde_yaml::Value::Null,
        };
        let yaml_str = serde_yaml::to_string(&raw).unwrap();
        assert!(
            !yaml_str.contains("is_personal"),
            "serialized org.yml should not contain is_personal"
        );
    }

    #[test]
    fn org_yml_without_config_flags_defaults_workspace_root_to_none() {
        let yml = br#"
slug: test--ctx
display_name: Test
created_at: "2026-04-16T00:00:00Z"
"#;
        let raw = parse_org_yml(yml).unwrap();
        assert_eq!(raw.config_flags.workspace_root_dir, None);
    }

    #[test]
    fn org_yml_round_trips_workspace_root_dir() {
        let yml = br#"
slug: test--ctx
display_name: Test
created_at: "2026-04-16T00:00:00Z"
config_flags:
  workspace_root_dir: /Users/dev/mycode
"#;
        let raw = parse_org_yml(yml).unwrap();
        assert_eq!(
            raw.config_flags.workspace_root_dir.as_deref(),
            Some(std::path::Path::new("/Users/dev/mycode"))
        );
        // Round-trips back out.
        let out = serde_yaml::to_string(&raw).unwrap();
        assert!(
            out.contains("workspace_root_dir"),
            "serialized org.yml should carry the set workspace_root_dir"
        );
    }

    #[test]
    fn org_yml_writer_omits_workspace_root_dir_when_unset() {
        let raw = RawOrgYml {
            slug: "test".into(),
            display_name: "Test".into(),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            config_flags: OrgConfigFlags::default(),
            tag_schema: serde_yaml::Value::Null,
        };
        let out = serde_yaml::to_string(&raw).unwrap();
        assert!(
            !out.contains("workspace_root_dir"),
            "unset workspace_root_dir must not be emitted (skip_serializing_if)"
        );
    }

    #[test]
    fn parse_org_missing_slug() {
        let yml = br#"
display_name: Alpha
created_at: "2026-01-01T00:00:00Z"
"#;
        let err = parse_org_yml(yml).unwrap_err();
        assert!(matches!(
            err,
            ParseError::Yaml {
                file: RegistryFile::OrgYml,
                ..
            }
        ));
    }

    #[test]
    fn parse_org_malformed_yaml() {
        let yml = b":::garbage:::";
        let err = parse_org_yml(yml).unwrap_err();
        match err {
            ParseError::Yaml { file, line, .. } => {
                assert_eq!(file, RegistryFile::OrgYml);
                assert!(line > 0, "expected a line number on malformed yaml");
            }
            _ => panic!("expected ParseError::Yaml, got {:?}", err),
        }
    }

    #[test]
    fn parse_org_empty_input_errors() {
        let err = parse_org_yml(b"").unwrap_err();
        assert!(matches!(
            err,
            ParseError::Yaml {
                file: RegistryFile::OrgYml,
                ..
            }
        ));
    }

    #[test]
    fn parse_org_invalid_utf8() {
        let bytes = vec![0xff, 0xfe, 0xfd];
        let err = parse_org_yml(&bytes).unwrap_err();
        assert_eq!(
            err,
            ParseError::InvalidUtf8 {
                file: RegistryFile::OrgYml
            }
        );
    }

    #[test]
    fn parse_org_strips_utf8_bom() {
        // A Windows editor may prepend a BOM. Without stripping, serde_yaml
        // fails with "did not find expected key" on line 1.
        let mut yml: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
        yml.extend_from_slice(
            br#"slug: x
display_name: X
created_at: "2026-01-01T00:00:00Z"
"#,
        );
        let raw = parse_org_yml(&yml).unwrap();
        assert_eq!(raw.slug, "x");
    }

    #[test]
    fn parse_org_strips_multiple_leading_boms() {
        // Double-save by a BOM-writing editor produces `\u{feff}\u{feff}`
        // at the start; the strip must handle any count.
        let mut yml: Vec<u8> = vec![0xEF, 0xBB, 0xBF, 0xEF, 0xBB, 0xBF];
        yml.extend_from_slice(
            br#"slug: x
display_name: X
created_at: "2026-01-01T00:00:00Z"
"#,
        );
        let raw = parse_org_yml(&yml).unwrap();
        assert_eq!(raw.slug, "x");
    }

    #[test]
    fn parse_org_unknown_field_is_tolerated() {
        let yml = br#"
slug: x
display_name: X
created_at: "2026-01-01T00:00:00Z"
totally_new_field: 42
"#;
        let raw = parse_org_yml(yml).unwrap();
        assert_eq!(raw.slug, "x");
    }

    // ---------- SW-DX-1: tag_schema rides on org.yml ----------

    #[test]
    fn parse_org_without_tag_schema_is_empty() {
        // AC-1: a pre-field org.yml (no tag_schema key) parses as an empty
        // (unopinionated) schema — never an error.
        let yml = br#"
slug: x
display_name: X
created_at: "2026-01-01T00:00:00Z"
"#;
        let raw = parse_org_yml(yml).unwrap();
        assert!(raw.tag_schema.is_null(), "absent tag_schema ⇒ null value");
    }

    #[test]
    fn parse_org_with_tag_schema_carries_through_to_domain() {
        // AC-1: a tag_schema authored on org.yml parses and survives composition
        // onto Org.tag_schema (the load_org spine), preserving ordered vocab.
        let yml = br#"
slug: x
display_name: X
created_at: "2026-01-01T00:00:00Z"
tag_schema:
  tags:
    - name: context_level
      type: ordered
      allowed_values: [machine, team, management, leadership]
    - name: priority
      type: select
      allowed_values: [low, high]
"#;
        let raw = parse_org_yml(yml).unwrap();
        assert!(raw.tag_schema.get("tags").is_some(), "raw value captured");

        let partial = partial_with_org(raw);
        let org = RawOrgComposition::from_partial(&partial, None)
            .and_then(|c| c.into_org("x".into()))
            .unwrap();
        let def = org.tag_schema.tag("context_level").expect("tag present");
        assert_eq!(def.order_index("management"), Some(2));
        assert_eq!(
            org.tag_schema.tag("priority").unwrap().allowed_values.len(),
            2
        );
    }

    #[test]
    fn tag_schema_round_trips_losslessly_through_reserialize() {
        // Review round-2 M-A (Major): the org-rename write-back re-serializes
        // RawOrgYml. Capturing tag_schema as a raw value must re-emit the
        // authored strings verbatim — an unknown/forward-compat `type: kanban`
        // and the kebab `multi-select` alias must NOT be rewritten (data loss).
        let yml = br#"
slug: x
display_name: X
created_at: "2026-01-01T00:00:00Z"
tag_schema:
  tags:
    - name: board
      type: kanban
    - name: owners
      type: multi-select
      allowed_values: [alice]
"#;
        let raw = parse_org_yml(yml).unwrap();
        let out = serde_yaml::to_string(&raw).unwrap();
        assert!(out.contains("kanban"), "unknown type preserved: {out}");
        assert!(
            !out.contains("type: unknown"),
            "must NOT rewrite kanban→unknown: {out}"
        );
        assert!(out.contains("multi-select"), "kebab alias preserved: {out}");
    }

    #[test]
    fn into_org_without_tag_schema_defaults_empty() {
        let partial = partial_with_org(valid_raw_org());
        let org = RawOrgComposition::from_partial(&partial, None)
            .and_then(|c| c.into_org("x".into()))
            .unwrap();
        assert!(org.tag_schema.is_empty());
    }

    #[test]
    fn org_yml_with_unknown_tag_type_still_loads_the_org() {
        // Review M1 (Major): an unknown `type:` string in a git-shared org.yml
        // must NOT fail the whole RawOrgYml deserialize (which would drop
        // teams/people too). It degrades to `Unknown` and the org loads.
        let yml = br#"
slug: x
display_name: X
created_at: "2026-01-01T00:00:00Z"
tag_schema:
  tags:
    - name: board
      type: kanban
"#;
        let raw = parse_org_yml(yml).expect("unknown tag type must not fail org.yml parse");
        let schema = super::super::TagSchema::from_raw_value(&raw.tag_schema);
        assert_eq!(schema.tags.len(), 1);
        assert_eq!(schema.tags[0].r#type, super::super::TagType::Unknown);
    }

    #[test]
    fn org_yml_with_malformed_tag_schema_shape_still_loads_the_org() {
        // Review round-2 M-B (Major): a malformed tag_schema SHAPE (not just an
        // unknown type value) must NOT fail the whole org.yml — teams/people must
        // survive. The raw-value capture + lenient per-tag conversion degrades
        // each malformed case to an empty (or partial) schema, org still loads.
        for bad in [
            "tag_schema:\n  tags: not-a-list\n",          // tags is a scalar
            "tag_schema:\n  tags:\n    - type: select\n", // a nameless tag
            "tag_schema:\n  tags:\n    - name: p\n      allowed_values: scalar\n", // bad field shape
            "tag_schema: garbage\n", // tag_schema not a mapping
        ] {
            let yml =
                format!("slug: x\ndisplay_name: X\ncreated_at: \"2026-01-01T00:00:00Z\"\n{bad}");
            let raw = parse_org_yml(yml.as_bytes()).unwrap_or_else(|e| {
                panic!("malformed tag_schema must not fail parse ({bad:?}): {e}")
            });
            let partial = partial_with_org(raw);
            let org = RawOrgComposition::from_partial(&partial, None)
                .and_then(|c| c.into_org("x".into()))
                .unwrap_or_else(|e| panic!("org must still compose ({bad:?}): {e}"));
            // A nameless-but-otherwise-valid tag is dropped; the others yield an
            // empty schema. Either way the org loads (the whole point).
            assert!(
                org.tag_schema.tag("p").is_none(),
                "malformed tag must not surface as a usable tag ({bad:?})"
            );
        }
    }

    // ---------- parse_people_yml ----------

    #[test]
    fn parse_people_happy_path() {
        let yml = br#"
people:
  - github_user_id: 1
    github_username: alice
    display_name: Alice
  - github_user_id: 2
    github_username: bob
"#;
        let raw = parse_people_yml(yml).unwrap();
        assert_eq!(raw.people.len(), 2);
        // SW-FG-4 (AC-1b): the legacy scalars fold into a one-element github
        // identity on load.
        let alice = &raw.people[0];
        assert_eq!(
            alice.github_identity().unwrap().handle.as_deref(),
            Some("alice")
        );
        assert_eq!(alice.github_user_id(), Some(1));
        assert_eq!(alice.display_name.as_deref(), Some("Alice"));
        assert_eq!(raw.people[1].github_user_id(), Some(2));
        assert_eq!(raw.people[1].display_name, None);
    }

    #[test]
    fn parse_people_field_path_surfacing() {
        // Second entry's github_user_id is a non-numeric string. This verifies
        // serde_path_to_error is engaged — if someone swaps back to
        // serde_yaml::from_str, the path field will be empty and this fails.
        let yml = br#"
people:
  - github_user_id: 1
    github_username: alice
  - github_user_id: "not-a-number"
    github_username: bob
"#;
        let err = parse_people_yml(yml).unwrap_err();
        match err {
            ParseError::Yaml { path, file, .. } => {
                assert_eq!(file, RegistryFile::PeopleYml);
                assert_eq!(path, "people[1].github_user_id");
            }
            _ => panic!("expected ParseError::Yaml, got {:?}", err),
        }
    }

    // ---------- parse_teams_yml ----------

    #[test]
    fn parse_teams_happy_path() {
        let yml = br#"
teams:
  - name: backend
    source_repos:
      - owner: alderco
        repo: codesense
"#;
        let raw = parse_teams_yml(yml).unwrap();
        assert_eq!(raw.teams.len(), 1);
        assert_eq!(raw.teams[0].name, "backend");
        assert_eq!(raw.teams[0].source_repos[0].owner, "alderco");
    }

    #[test]
    fn parse_teams_tolerates_legacy_member_and_lead_ids() {
        // SW-TM-3 back-compat: an OLD teams.yml still carrying member_ids/lead_ids
        // parses cleanly (unknown keys ignored — no deny_unknown_fields) and the
        // fields are dropped on the next re-serialize.
        let yml = br#"
teams:
  - name: backend
    member_ids: [1, 2]
    lead_ids: [1]
    source_repos: []
"#;
        let raw = parse_teams_yml(yml).unwrap();
        assert_eq!(raw.teams.len(), 1);
        assert_eq!(raw.teams[0].name, "backend");
        let reserialized = serde_yaml::to_string(&raw).unwrap();
        assert!(!reserialized.contains("member_ids"), "{reserialized}");
        assert!(!reserialized.contains("lead_ids"), "{reserialized}");
    }

    #[test]
    fn parse_teams_nested_path_surfacing() {
        // source_repos[0].owner on the second team is a number.
        let yml = br#"
teams:
  - name: backend
    source_repos: []
  - name: frontend
    source_repos:
      - owner: [1, 2]
        repo: foo
"#;
        let err = parse_teams_yml(yml).unwrap_err();
        match err {
            ParseError::Yaml { path, .. } => {
                assert_eq!(path, "teams[1].source_repos[0].owner");
            }
            _ => panic!("expected ParseError::Yaml, got {:?}", err),
        }
    }

    // ---------- RawPersonEntry conversion ----------

    #[test]
    fn raw_person_with_display_name() {
        let raw = RawPersonEntry::github(1, "octo", Some("Octo P".into()), None);
        let p = raw.into_domain().unwrap().unwrap();
        assert_eq!(p.display_name, "Octo P");
        assert_eq!(p.display_name_or_username(), "Octo P");
    }

    #[test]
    fn raw_person_fallback_when_display_name_missing() {
        // SW-FG-4b: `into_domain` carries display_name faithfully (empty when the
        // row omits it); the handle/id fallback for a LABEL lives in
        // `Person::display_name_or_username`, not in a mutated field.
        let raw = RawPersonEntry::github(1, "octo", None, Some("https://example.com/a.png".into()));
        let p = raw.into_domain().unwrap().unwrap();
        assert_eq!(p.display_name, "");
        assert_eq!(p.display_name_or_username(), "octo");
        assert_eq!(p.avatar_url.as_deref(), Some("https://example.com/a.png"));
    }

    #[test]
    fn people_parser_rejects_duplicate_stable_ids() {
        let err = parse_people_yml(
            b"people:\n  - id: p-1\n    display_name: Grace\n  - id: p-1\n    display_name: Ada\n",
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ParseError::Invariant {
                source: DomainError::DuplicatePerson(_),
                ..
            }
        ));
    }

    #[test]
    fn raw_person_unusable_github_id_skips_not_fails() {
        // SW-FG-4 (code-review HIGH follow-up): a GitHub identity that can't yield a
        // usable id is SKIPPED (Ok(None)), never a hard error — otherwise one such
        // row would brick the whole org load.
        // Zero id:
        assert_eq!(
            RawPersonEntry::github(0, "octo", None, None).into_domain(),
            Ok(None)
        );
        // Non-numeric / opaque id under a github identity:
        let non_numeric = RawPersonEntry {
            id: None,
            identities: vec![RawIdentity {
                provider: "github".into(),
                handle: Some("octo".into()),
                provider_user_id: Some("not-a-number".into()),
            }],
            display_name: None,
            avatar_url: None,
        };
        assert_eq!(non_numeric.into_domain(), Ok(None));
    }

    #[test]
    fn raw_person_handle_less_github_now_composes() {
        // SW-FG-4b (AC-2/AC-6): the "added, not yet linked" GitHub teammate — a
        // GitHub identity with a usable id but a NULL handle — now COMPOSES (FG-4
        // dropped it). They are present/assignable by id but not correlatable by
        // handle.
        let handle_less = RawPersonEntry {
            id: None,
            identities: vec![RawIdentity {
                provider: "github".into(),
                handle: None,
                provider_user_id: Some("500".into()),
            }],
            display_name: Some("Grace".into()),
            avatar_url: None,
        };
        let person = handle_less.into_domain().unwrap().expect("composes");
        assert_eq!(person.github_user_id(), Some(500));
        assert_eq!(person.github_username(), None);
        assert_eq!(person.id, PersonId::from(500u64));
        assert_eq!(person.display_name_or_username(), "Grace");
    }

    #[test]
    fn raw_person_two_github_identities_resolve_first_wins() {
        // SW-FG-4: a malformed entry with two github rows resolves deterministically
        // to the FIRST (and logs a warning); the second is ignored, never a panic
        // or a nondeterministic pick.
        let two = RawPersonEntry {
            id: None,
            identities: vec![
                RawIdentity {
                    provider: "github".into(),
                    handle: Some("first".into()),
                    provider_user_id: Some("1".into()),
                },
                RawIdentity {
                    provider: "github".into(),
                    handle: Some("second".into()),
                    provider_user_id: Some("2".into()),
                },
            ],
            display_name: None,
            avatar_url: None,
        };
        assert_eq!(two.github_user_id(), Some(1));
        assert_eq!(two.github_handle(), Some("first"));
        let person = two.into_domain().unwrap().unwrap();
        assert_eq!(person.github_user_id(), Some(1));
        assert_eq!(person.github_username(), Some("first"));
    }

    // ---------- RawTeamEntry conversion ----------

    #[test]
    fn raw_team_duplicate_source_repo() {
        let raw = RawTeamEntry {
            name: "backend".into(),
            source_repos: vec![
                RawSourceRepoEntry::github("o", "r"),
                RawSourceRepoEntry::github("o", "r"),
            ],
            archived: false,
            parent_team: None,
            landing_page: None,
        };
        assert_eq!(
            raw.into_domain(),
            Err(DomainError::DuplicateSourceRepo(
                SourceRepoSlug::new("o", "r").unwrap()
            ))
        );
    }

    #[test]
    fn raw_team_invalid_name() {
        let raw = RawTeamEntry {
            name: "Backend".into(),
            source_repos: vec![],
            archived: false,
            parent_team: None,
            landing_page: None,
        };
        assert_eq!(
            raw.into_domain(),
            Err(DomainError::InvalidTeamName("Backend".into()))
        );
    }

    // ---------- landing_page: a team's home page ----------

    #[test]
    fn landing_page_loads_onto_the_domain_team() {
        let yml = br#"
teams:
  - name: getting-started
    landing_page: README.html
  - name: backend
"#;
        let raw = parse_teams_yml(yml).unwrap();
        assert_eq!(raw.teams[0].landing_page.as_deref(), Some("README.html"));
        assert_eq!(raw.teams[1].landing_page, None);
        let team = raw.teams[0].clone().into_domain().unwrap();
        assert_eq!(team.landing_page.as_deref(), Some("README.html"));
        let mut padded = raw.teams[0].clone();
        padded.landing_page = Some("  README.html \n".into());
        assert_eq!(
            padded.into_domain().unwrap().landing_page.as_deref(),
            Some("README.html"),
            "surrounding whitespace is trimmed, as the app does"
        );
        let plain = raw.teams[1].clone().into_domain().unwrap();
        assert_eq!(plain.landing_page, None);
    }

    #[test]
    fn landing_page_round_trips_and_is_omitted_when_absent() {
        let mut entry: RawTeamEntry = serde_yaml::from_str("name: backend\n").unwrap();
        let bare = serde_yaml::to_string(&RawTeamsYml {
            teams: vec![entry.clone()],
        })
        .unwrap();
        assert!(!bare.contains("landing_page"), "{bare}");
        entry.landing_page = Some("guide/start.md".into());
        let yaml_str = serde_yaml::to_string(&RawTeamsYml { teams: vec![entry] }).unwrap();
        assert!(
            yaml_str.contains("landing_page: guide/start.md"),
            "{yaml_str}"
        );
        let back = parse_teams_yml(yaml_str.as_bytes()).unwrap();
        assert_eq!(
            back.teams[0].landing_page.as_deref(),
            Some("guide/start.md")
        );
    }

    #[test]
    fn landing_page_outside_the_team_content_is_dropped_not_fatal() {
        for bad in [
            "/etc/passwd",
            "../org.yml",
            "a/../../x",
            "",
            "a\\b.md",
            ".env",
            ".claude/notes.md",
            "docs/.secret/x.md",
        ] {
            let mut entry: RawTeamEntry = serde_yaml::from_str("name: backend\n").unwrap();
            entry.landing_page = Some(bad.into());
            let team = entry
                .into_domain()
                .expect("a bad landing page never breaks the team");
            assert_eq!(team.landing_page, None, "{bad:?} should be dropped");
        }
    }

    // ---------- SLW-IN-1: parent_team data model + persistence ----------

    #[test]
    fn parse_teams_with_parent_team() {
        // AC-2: a `parent_team:` entry loads onto the validated domain Team.
        let yml = br#"
teams:
  - name: platform
    source_repos: []
  - name: backend
    source_repos: []
    parent_team: platform
"#;
        let raw = parse_teams_yml(yml).unwrap();
        assert_eq!(raw.teams[0].parent_team, None, "root team stays None");
        assert_eq!(raw.teams[1].parent_team, Some("platform".to_string()));
        // and it survives into the validated domain type
        let team = raw.teams[1].clone().into_domain().unwrap();
        assert_eq!(team.parent_team, Some("platform".to_string()));
    }

    // SW-FG-10a AC-1: a `provider:` entry round-trips onto the parsed raw entry;
    // a bare entry defaults to github. An optional `host:` round-trips too.
    #[test]
    fn source_repo_provider_round_trips_and_legacy_defaults_to_github() {
        let yml = br#"
teams:
  - name: backend
    source_repos:
      - owner: a
        repo: gl
        provider: gitlab
      - owner: a
        repo: gh
      - owner: a
        repo: self
        provider: gitea
        host: git.example.com
"#;
        let raw = parse_teams_yml(yml).unwrap();
        let repos = &raw.teams[0].source_repos;
        assert_eq!(repos[0].provider, "gitlab");
        assert_eq!(repos[0].host, None);
        assert_eq!(
            repos[1].provider, "github",
            "legacy bare entry defaults to github"
        );
        assert_eq!(repos[2].provider, "gitea");
        assert_eq!(repos[2].host.as_deref(), Some("git.example.com"));

        let domain = raw.teams[0].clone().into_domain().unwrap();
        assert_eq!(domain.source_repos[0].host.as_deref(), Some("gitlab.com"));
        assert_eq!(domain.source_repos[1].host.as_deref(), Some("github.com"));
        assert_eq!(
            domain.source_repos[2].remote_key(),
            "git.example.com/a/self"
        );
    }

    // SW-FG-10a AC-1: re-serializing an all-legacy (no provider) teams.yml is
    // byte-identical — the `skip_serializing_if = "is_github"` guard keeps a
    // github entry from injecting a `provider:` key. Guards against churn across
    // every team.yml on the next `team_rename`/`mutate` re-serialize.
    #[test]
    fn legacy_teams_yml_reserializes_without_provider_key() {
        let raw = RawTeamsYml {
            teams: vec![RawTeamEntry {
                name: "backend".into(),
                source_repos: vec![RawSourceRepoEntry::github("alderco", "codesense")],
                archived: false,
                parent_team: None,
                landing_page: None,
            }],
        };
        let out = serde_yaml::to_string(&raw).unwrap();
        assert!(
            !out.contains("provider"),
            "github default must not serialize a provider key, got:\n{out}"
        );
        assert!(
            !out.contains("host"),
            "absent host must not serialize, got:\n{out}"
        );
    }

    // SW-FG-10a AC-1: a non-github provider DOES serialize (round-trips through
    // a write), so an authored gitlab entry survives a re-serialize.
    #[test]
    fn non_github_provider_reserializes() {
        let entry = RawSourceRepoEntry {
            owner: "a".into(),
            repo: "b".into(),
            provider: "gitlab".into(),
            host: None,
        };
        let out = serde_yaml::to_string(&entry).unwrap();
        assert!(out.contains("provider: gitlab"), "got:\n{out}");
    }

    // SW-FG-10a: `provider_id` normalizes hand-authored casing/whitespace and
    // treats a blank value as the github default — the canonical id logic keys on.
    #[test]
    fn provider_id_normalizes_casing_whitespace_and_blank() {
        let mk = |p: &str| RawSourceRepoEntry {
            owner: "o".into(),
            repo: "r".into(),
            provider: p.into(),
            host: None,
        };
        assert_eq!(mk("GitHub").provider_id(), "github");
        assert_eq!(mk("  Jira  ").provider_id(), "jira");
        assert_eq!(mk("GITLAB").provider_id(), "gitlab");
        assert_eq!(mk("").provider_id(), "github", "blank ⇒ github default");
        assert_eq!(
            mk("   ").provider_id(),
            "github",
            "whitespace-only ⇒ github default"
        );
        // The raw field is left as authored; only provider_id() normalizes.
        assert_eq!(mk("GitHub").provider, "GitHub");
    }

    #[test]
    fn parse_teams_without_parent_team_is_none() {
        // AC-3/AC-5: a pre-field teams.yml (no parent_team key) loads unchanged,
        // resolving to a root team — byte-identical behavior to before.
        let yml = br#"
teams:
  - name: backend
    source_repos: []
"#;
        let team = parse_teams_yml(yml).unwrap().teams[0]
            .clone()
            .into_domain()
            .unwrap();
        assert_eq!(team.parent_team, None);
    }

    #[test]
    fn parentless_team_is_byte_stable() {
        // AC-3: a parentless team must NOT emit a spurious `parent_team:` key,
        // exactly as `archived: false` is omitted — keeps existing teams.yml
        // bytes unchanged. Guaranteed by skip_serializing_if = "Option::is_none".
        let raw = RawTeamsYml {
            teams: vec![RawTeamEntry {
                name: "backend".into(),
                source_repos: vec![],
                archived: false,
                parent_team: None,
                landing_page: None,
            }],
        };
        let yaml_str = serde_yaml::to_string(&raw).unwrap();
        assert!(
            !yaml_str.contains("parent_team"),
            "parentless team leaked a parent_team key:\n{yaml_str}"
        );
    }

    #[test]
    fn parent_team_round_trips_through_yaml() {
        // AC-2: a team WITH a parent re-serializes the key so the link persists.
        let raw = RawTeamsYml {
            teams: vec![RawTeamEntry {
                name: "backend".into(),
                source_repos: vec![],
                archived: false,
                parent_team: Some("platform".into()),
                landing_page: None,
            }],
        };
        let yaml_str = serde_yaml::to_string(&raw).unwrap();
        assert!(yaml_str.contains("parent_team: platform"), "{yaml_str}");
        let back = parse_teams_yml(yaml_str.as_bytes()).unwrap();
        assert_eq!(back.teams[0].parent_team, Some("platform".to_string()));
    }

    #[test]
    fn invalid_parent_team_rejected_as_slug_and_classified_to_teams_yml() {
        // AC-4: parent_team is slug-validated (reusing is_slug_safe), and the
        // error routes the user to teams.yml. Reachability is NOT checked here.
        let raw = RawTeamEntry {
            name: "backend".into(),
            source_repos: vec![],
            archived: false,
            parent_team: Some("Bad Name".into()),
            landing_page: None,
        };
        let err = raw.into_domain().unwrap_err();
        assert_eq!(err, DomainError::InvalidParentTeam("Bad Name".into()));
        let (file, _path) = classify_domain_error(&err, Path::new(""));
        assert_eq!(file, RegistryFile::TeamsYml);
    }

    #[test]
    fn dangling_parent_team_is_accepted_here_deferred_to_in3() {
        // AC-4: a parent naming a team that doesn't exist is slug-safe, so IN-1
        // accepts it. Detecting the dangling reference is SLW-IN-3's job.
        let raw = RawTeamEntry {
            name: "backend".into(),
            source_repos: vec![],
            archived: false,
            parent_team: Some("ghost".into()),
            landing_page: None,
        };
        let team = raw.into_domain().unwrap();
        assert_eq!(team.parent_team, Some("ghost".to_string()));
    }

    #[test]
    fn malformed_source_repo_row_is_skipped_without_dropping_valid_rows() {
        let raw = RawTeamEntry {
            name: "backend".into(),
            source_repos: vec![
                RawSourceRepoEntry::github("", "r"),
                RawSourceRepoEntry::github("valid", "repo"),
            ],
            archived: false,
            parent_team: None,
            landing_page: None,
        };
        let team = raw.into_domain().unwrap();
        assert_eq!(team.source_repos.len(), 1);
        assert_eq!(team.source_repos[0].owner, "valid");
    }

    #[test]
    fn raw_source_repo_missing_repo_key_rejected_at_serde() {
        let yml = br#"
teams:
  - name: backend
    source_repos:
      - owner: o
"#;
        let err = parse_teams_yml(yml).unwrap_err();
        assert!(matches!(err, ParseError::Yaml { .. }));
    }

    // ---------- RawOrgComposition invariant routing ----------

    fn partial_with_org(raw_org: RawOrgYml) -> PartialOrg {
        PartialOrg {
            org_yml: Some(raw_org),
            people_yml: Some(RawPeopleYml { people: vec![] }),
            teams_yml: Some(RawTeamsYml { teams: vec![] }),
        }
    }

    fn valid_raw_org() -> RawOrgYml {
        RawOrgYml {
            slug: "x".into(),
            display_name: "X".into(),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            config_flags: OrgConfigFlags::default(),
            tag_schema: serde_yaml::Value::Null,
        }
    }

    #[test]
    fn composition_empty_dir_slug_surfaces_invariant() {
        // The authoritative slug is the dir name; an empty dir slug must still
        // be rejected by Org::new's invariant, classified to org.yml.
        let partial = partial_with_org(valid_raw_org());
        let err = RawOrgComposition::from_partial(&partial, None)
            .and_then(|c| c.into_org("".into()))
            .unwrap_err();
        assert_eq!(
            err,
            ParseError::Invariant {
                file: RegistryFile::OrgYml,
                source: DomainError::EmptyField { field: "slug" }
            }
        );
    }

    #[test]
    fn composition_whitespace_dir_slug_rejected() {
        let partial = partial_with_org(valid_raw_org());
        let err = RawOrgComposition::from_partial(&partial, None)
            .and_then(|c| c.into_org("   ".into()))
            .unwrap_err();
        assert!(matches!(
            err,
            ParseError::Invariant {
                file: RegistryFile::OrgYml,
                source: DomainError::EmptyField { field: "slug" }
            }
        ));
    }

    #[test]
    fn composition_whitespace_display_name_rejected() {
        let mut raw = valid_raw_org();
        raw.display_name = "   ".into();
        let partial = partial_with_org(raw);
        let err = RawOrgComposition::from_partial(&partial, None)
            .and_then(|c| c.into_org("x".into()))
            .unwrap_err();
        assert!(matches!(
            err,
            ParseError::Invariant {
                file: RegistryFile::OrgYml,
                source: DomainError::EmptyField {
                    field: "display_name"
                }
            }
        ));
    }

    #[test]
    fn composition_happy_path_with_people_and_teams() {
        let partial = PartialOrg {
            org_yml: Some(valid_raw_org()),
            people_yml: Some(RawPeopleYml {
                people: vec![
                    RawPersonEntry::github(1, "alice", Some("Alice".into()), None),
                    RawPersonEntry::github(2, "bob", None, None),
                ],
            }),
            teams_yml: Some(RawTeamsYml {
                teams: vec![RawTeamEntry {
                    name: "backend".into(),
                    source_repos: vec![RawSourceRepoEntry::github("alderco", "codesense")],
                    archived: false,
                    parent_team: None,
                    landing_page: None,
                }],
            }),
        };
        let org = RawOrgComposition::from_partial(&partial, Some("git@x".into()))
            .and_then(|c| c.into_org("x".into()))
            .unwrap();
        assert_eq!(org.slug, "x");
        assert_eq!(org.people.len(), 2);
        assert_eq!(org.teams.len(), 1);
        assert_eq!(org.remote_url.as_deref(), Some("git@x"));
    }

    // ---------- dir-name slug is authoritative ----------

    #[test]
    fn reconcile_org_slug_flags_mismatch_and_passes_match() {
        assert_eq!(
            reconcile_org_slug("acme--context", "ignored-yaml-value"),
            Some("ignored-yaml-value")
        );
        assert_eq!(reconcile_org_slug("acme--context", "acme--context"), None);
    }

    #[test]
    fn into_org_uses_dir_slug_not_yaml_slug() {
        let mut raw = valid_raw_org();
        raw.slug = "ignored-yaml-value".into();
        let partial = partial_with_org(raw);
        let org = RawOrgComposition::from_partial(&partial, None)
            .and_then(|c| c.into_org("acme--context".into()))
            .unwrap();
        assert_eq!(org.slug, "acme--context");
    }

    #[test]
    fn into_org_tolerates_empty_yaml_slug_when_dir_slug_valid() {
        // org.yml::slug is informational only — an empty/blank value no longer
        // fails composition; the valid dir slug wins.
        let mut raw = valid_raw_org();
        raw.slug = "".into();
        let partial = partial_with_org(raw);
        let org = RawOrgComposition::from_partial(&partial, None)
            .and_then(|c| c.into_org("acme--context".into()))
            .unwrap();
        assert_eq!(org.slug, "acme--context");
    }

    // ---------- SW-FG-4: provider-neutral people.yml migration ----------

    #[test]
    fn people_legacy_scalars_fold_into_one_github_identity() {
        // AC-1b: a legacy entry (flat github_user_id + github_username) folds into
        // a one-element `provider: "github"` identity on load, id as a String.
        let yml = br#"
people:
  - github_user_id: 42
    github_username: octo
    display_name: Octo P
"#;
        let raw = parse_people_yml(yml).unwrap();
        let person = &raw.people[0];
        assert_eq!(person.identities.len(), 1);
        let id = &person.identities[0];
        assert_eq!(id.provider, "github");
        assert_eq!(id.handle.as_deref(), Some("octo"));
        assert_eq!(id.provider_user_id.as_deref(), Some("42"));
        assert_eq!(person.github_user_id(), Some(42));
    }

    #[test]
    fn people_bitbucket_style_opaque_string_id_round_trips() {
        // AC-1: an opaque provider_user_id (Bitbucket account_id/UUID) is a String,
        // so it survives parse → serialize → parse unchanged — the whole point of
        // dropping the u64.
        let yml = br#"
people:
  - identities:
      - provider: bitbucket
        handle: octo
        provider_user_id: "557058:f0e1-42a3-uuid"
"#;
        let raw = parse_people_yml(yml).unwrap();
        assert_eq!(
            raw.people[0].identities[0].provider_user_id.as_deref(),
            Some("557058:f0e1-42a3-uuid")
        );
        let out = serde_yaml::to_string(&raw).unwrap();
        let back = parse_people_yml(out.as_bytes()).unwrap();
        assert_eq!(raw.people[0].identities, back.people[0].identities);
    }

    #[test]
    fn people_mixed_provider_entry_parses_directly() {
        // AC-1: the same human carries a GitHub AND a GitLab handle in one row.
        let yml = br#"
people:
  - identities:
      - provider: github
        handle: alice
        provider_user_id: "1"
      - provider: gitlab
        handle: alice-gl
        provider_user_id: "99"
    display_name: Alice
"#;
        let raw = parse_people_yml(yml).unwrap();
        let person = &raw.people[0];
        assert_eq!(person.identities.len(), 2);
        assert_eq!(
            person.github_identity().unwrap().handle.as_deref(),
            Some("alice")
        );
        // The GitLab identity is preserved in config (surfaced to the domain by FG-11+).
        assert_eq!(person.identities[1].provider, "gitlab");
        assert_eq!(person.identities[1].handle.as_deref(), Some("alice-gl"));
    }

    #[test]
    fn people_both_present_prefers_identities() {
        // AC-3: a half-migrated file with BOTH the new `identities:` and the legacy
        // scalars keeps the new list (new-shape wins) — legacy scalars are ignored.
        let yml = br#"
people:
  - identities:
      - provider: github
        handle: newname
        provider_user_id: "7"
    github_user_id: 999
    github_username: oldname
"#;
        let raw = parse_people_yml(yml).unwrap();
        assert_eq!(raw.people[0].identities.len(), 1);
        assert_eq!(raw.people[0].github_user_id(), Some(7));
        assert_eq!(
            raw.people[0].github_identity().unwrap().handle.as_deref(),
            Some("newname")
        );
    }

    #[test]
    fn people_handle_less_identity_round_trips() {
        // AC-5: a person added with no handle yet is an identity row with a null
        // handle — expressible and byte-preserved (the domain consumes it in
        // FG-8/12). `handle: None` skips serializing rather than emitting `null`.
        let yml = br#"
people:
  - identities:
      - provider: github
        provider_user_id: "500"
"#;
        let raw = parse_people_yml(yml).unwrap();
        assert_eq!(raw.people[0].identities[0].handle, None);
        assert_eq!(raw.people[0].github_user_id(), Some(500));
        let out = serde_yaml::to_string(&raw).unwrap();
        assert!(
            !out.contains("handle"),
            "null handle must not be emitted: {out}"
        );
    }

    #[test]
    fn people_migrate_on_load_emits_new_shape_only() {
        // AC-3 (golden): a legacy roster parses, then re-serializes in the new
        // shape — the emitted bytes carry `identities:` and NO bare
        // `github_user_id`/`github_username` scalar. This is THE migrate-on-load guard.
        let legacy = br#"
people:
  - github_user_id: 1
    github_username: alice
    display_name: Alice
  - github_user_id: 2
    github_username: bob
"#;
        let raw = parse_people_yml(legacy).unwrap();
        let out = serde_yaml::to_string(&raw).unwrap();
        assert!(
            out.contains("identities"),
            "new shape must be emitted: {out}"
        );
        assert!(out.contains("provider: github"), "{out}");
        assert!(
            !out.contains("github_user_id"),
            "legacy scalar must not be re-emitted: {out}"
        );
        assert!(
            !out.contains("github_username"),
            "legacy scalar must not be re-emitted: {out}"
        );
        // Idempotent: re-reading the migrated bytes yields the same people.
        let back = parse_people_yml(out.as_bytes()).unwrap();
        assert_eq!(raw.people, back.people);
    }

    #[test]
    fn people_legacy_and_mixed_rosters_resolve_to_domain() {
        // AC-4: a legacy GitHub roster and a mixed-provider roster both compose to
        // the expected domain people (the domain stays GitHub-keyed for v1).
        let legacy = parse_people_yml(
            br#"
people:
  - github_user_id: 1
    github_username: alice
    display_name: Alice
"#,
        )
        .unwrap();
        let alice = legacy.people[0].clone().into_domain().unwrap().unwrap();
        assert_eq!(alice.github_user_id(), Some(1));
        assert_eq!(alice.github_username(), Some("alice"));
        assert_eq!(alice.display_name, "Alice");

        let mixed = parse_people_yml(
            br#"
people:
  - identities:
      - provider: github
        handle: bob
        provider_user_id: "2"
      - provider: gitlab
        handle: bob-gl
        provider_user_id: "abc"
"#,
        )
        .unwrap();
        let bob = mixed.people[0].clone().into_domain().unwrap().unwrap();
        assert_eq!(bob.github_user_id(), Some(2));
        assert_eq!(bob.github_username(), Some("bob"));

        // AC-1 / regression (code-review HIGH): a PURE-non-GitHub roster row is a
        // valid entry the v1 domain can't hold yet — it must SKIP (Ok(None)), not
        // fail. Previously this folded to id 0 and bricked the whole org load.
        let gitlab_only = parse_people_yml(
            br#"
people:
  - identities:
      - provider: gitlab
        handle: grace-gl
        provider_user_id: "gl-7"
    display_name: Grace
"#,
        )
        .unwrap();
        assert_eq!(gitlab_only.people[0].clone().into_domain().unwrap(), None);
    }

    #[test]
    fn org_load_skips_pure_non_github_people_instead_of_failing() {
        // Regression (code-review HIGH): a roster mixing a GitHub teammate with a
        // pure-non-GitHub one composes the org, SKIPPING the non-GitHub row — it
        // must NOT fail the whole load (which would drop everyone AND every team).
        let partial = PartialOrg {
            org_yml: Some(valid_raw_org()),
            people_yml: Some(RawPeopleYml {
                people: vec![
                    RawPersonEntry::github(1, "alice", Some("Alice".into()), None),
                    RawPersonEntry {
                        id: None,
                        identities: vec![RawIdentity {
                            provider: "gitlab".into(),
                            handle: Some("grace-gl".into()),
                            provider_user_id: Some("gl-7".into()),
                        }],
                        display_name: Some("Grace".into()),
                        avatar_url: None,
                    },
                ],
            }),
            teams_yml: Some(RawTeamsYml { teams: vec![] }),
        };
        let org = RawOrgComposition::from_partial(&partial, None)
            .and_then(|c| c.into_org("x".into()))
            .unwrap();
        assert_eq!(org.people.len(), 1, "only the GitHub teammate composes");
        assert_eq!(org.people[0].github_user_id(), Some(1));
    }

    #[test]
    fn org_load_composes_handle_less_github_teammate() {
        // SW-FG-4b (AC-2/AC-6), through the real compose seam: a roster with a
        // handle-less GitHub teammate (the FG-4 AC-5 "added, not yet linked" shape)
        // now lands them in org.people — no longer silently dropped.
        let partial = PartialOrg {
            org_yml: Some(valid_raw_org()),
            people_yml: Some(RawPeopleYml {
                people: vec![
                    RawPersonEntry::github(1, "alice", Some("Alice".into()), None),
                    RawPersonEntry {
                        id: None,
                        identities: vec![RawIdentity {
                            provider: "github".into(),
                            handle: None,
                            provider_user_id: Some("500".into()),
                        }],
                        display_name: Some("Grace".into()),
                        avatar_url: None,
                    },
                ],
            }),
            teams_yml: Some(RawTeamsYml { teams: vec![] }),
        };
        let org = RawOrgComposition::from_partial(&partial, None)
            .and_then(|c| c.into_org("x".into()))
            .unwrap();
        assert_eq!(org.people.len(), 2, "both GitHub teammates compose");
        let grace = org
            .people
            .iter()
            .find(|p| p.github_user_id() == Some(500))
            .expect("handle-less teammate present");
        assert_eq!(grace.github_username(), None);
        assert_eq!(grace.display_name_or_username(), "Grace");
        // Present but not correlatable by login.
        assert!(org.person_by_username("grace").is_none());
    }
}
