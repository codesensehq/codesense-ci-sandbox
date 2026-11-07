// SPDX-License-Identifier: MIT
//! The agent service: the one API every agent surface (MCP tools, the Agents
//! tab, launches, routines) wraps.
//!
//! Every fn takes `(codesense_home, principal, …)`, needs no `AppHandle`, and
//! does blocking filesystem work; async callers run it inside `await_blocking`.
//!
//! Definition writes are revisioned: each one writes an immutable
//! `revisions/<n>.yml` snapshot and bumps `revision`, under a per-agent lock,
//! so a stale base is a typed [`AgentError::Conflict`]. That is a same-machine
//! stale-edit guard only; edits from two machines meet in git.
//!
//! There is no write authorization. Any teammate may edit, archive, restore or
//! delete a team agent: an edit is a local change to the team's context repo,
//! and sharing it is an ordinary git push the user's own agent makes. CodeSense
//! never commits or pushes. `created_by` and each revision's author are
//! attribution only. Personal agents stay private by placement: they live in
//! the caller's scratch space for one org, and another user's personal key
//! never resolves.
//!
//! The starter agents ([`builtin`]) are listed, read and resolved like any
//! other, but every change to one is [`AgentError::ReadOnly`].

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::context_repo::org_slug::validate_org_slug;
use crate::context_repo::scoped_key::{BUILTIN_ORG, PERSONAL_OWNER};
use crate::context_repo::scratch_space::is_scratch_team;
use crate::domain::{Principal, SourceRepoSlug};
use crate::routines::model::Author;
use crate::routines::PersonalOwner;

use super::builtin;
use super::context::RepoOverride;
use super::defs::{self, RevisionRecord};
use super::discover::{AgentScope, DiscoveredAgent};
use super::keys::AgentKey;
use super::manifest::AgentRevisionRef;
use super::manifest_resolve::{resolve_context, ContextRequest, ContextResolution, ManifestTarget};
use super::model::{is_valid_id, Agent, ValidationError};
use super::roots;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    Validation(ValidationError),
    /// The save's base revision is stale; nothing was written.
    Conflict {
        current_revision: u32,
    },
    NotFound(AgentKey),
    RevisionNotFound {
        key: AgentKey,
        revision: u32,
    },
    AlreadyExists(AgentKey),
    /// Something still references the agent, so it cannot be deleted. Archive
    /// hides it while keeping those references working.
    InUse {
        referrers: Vec<String>,
        /// Those of `referrers` that are the agent's own routines (they live
        /// under it); the rest only wait on it (`agent_done from_agent`).
        owned: Vec<String>,
    },
    /// The org/team/id does not name a place agents can live.
    InvalidLocation(String),
    /// A starter agent the app ships: it cannot be changed, only copied.
    ReadOnly(AgentKey),
    Io(String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(e) => write!(f, "{e}"),
            Self::Conflict { current_revision } => write!(
                f,
                "agent changed since you loaded it (now at revision {current_revision}); reload and retry"
            ),
            Self::NotFound(key) => write!(f, "agent {key} not found"),
            Self::RevisionNotFound { key, revision } => {
                write!(f, "agent {key} has no revision {revision}")
            }
            Self::AlreadyExists(key) => write!(f, "agent {key} already exists"),
            Self::InUse { referrers, owned } => {
                let waiting: Vec<&str> = referrers
                    .iter()
                    .filter(|r| !owned.contains(r))
                    .map(String::as_str)
                    .collect();
                let mut parts = Vec::new();
                if !owned.is_empty() {
                    parts.push(format!(
                        "it still has active routines: {}; archive them first",
                        owned.join(", ")
                    ));
                }
                if !waiting.is_empty() {
                    parts.push(format!(
                        "routines still wait on it finishing: {}; change their trigger first",
                        waiting.join(", ")
                    ));
                }
                write!(f, "agent cannot be deleted: {}", parts.join("; and "))
            }
            Self::InvalidLocation(why) => write!(f, "{why}"),
            Self::ReadOnly(_) => f.write_str(builtin::READ_ONLY_MESSAGE),
            Self::Io(e) => write!(f, "agent io error: {e}"),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<ValidationError> for AgentError {
    fn from(e: ValidationError) -> Self {
        Self::Validation(e)
    }
}

impl From<defs::DefsError> for AgentError {
    fn from(e: defs::DefsError) -> Self {
        Self::Io(e.to_string())
    }
}

/// Where a new agent is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// The caller's scratch space for the owner's org, created on first save.
    Personal(PersonalOwner),
    /// A locally-synced team.
    Team { org: String, team: String },
}

/// What a save does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveTarget {
    /// Write a new agent (revision 1) at the placement.
    Create(Placement),
    /// Replace an existing agent. `base_revision` is the revision the edit was
    /// made against.
    Update { key: AgentKey, base_revision: u32 },
}

impl From<crate::routines::Placement> for Placement {
    fn from(placement: crate::routines::Placement) -> Self {
        match placement {
            crate::routines::Placement::Personal(owner) => Self::Personal(owner),
            crate::routines::Placement::Team { org, team } => Self::Team { org, team },
        }
    }
}

/// Who still references an agent, so it is not deleted out from under them.
/// Passed in by the caller; there is no global registry.
pub trait AgentReferrers {
    /// A display line per referrer (e.g. a routine key). Empty when unused.
    fn referrers(&self, home: &Path, principal: &Principal, key: &AgentKey) -> Vec<String>;

    /// Those referrers that belong to the agent (its own routines), as
    /// opposed to ones that only wait on it.
    fn owned(&self, _home: &Path, _principal: &Principal, _key: &AgentKey) -> Vec<String> {
        Vec::new()
    }
}

/// Reports no referrers, for tests about agents alone.
#[cfg(test)]
pub struct NoReferrers;

#[cfg(test)]
impl AgentReferrers for NoReferrers {
    fn referrers(&self, _: &Path, _: &Principal, _: &AgentKey) -> Vec<String> {
        Vec::new()
    }
}

/// A saved agent and its key.
#[derive(Debug, Clone, Serialize)]
pub struct SavedAgent {
    pub key: AgentKey,
    pub agent: Agent,
}

/// One agent with its full history.
#[derive(Debug, Clone, Serialize)]
pub struct AgentDetail {
    pub key: AgentKey,
    pub scope: AgentScope,
    pub created_by_display: String,
    pub agent: Agent,
    /// Every revision, oldest first, each with its author.
    pub revisions: Vec<RevisionRecord>,
}

/// An agent exactly as it was at one revision, with the reference a launch
/// records.
#[derive(Debug, Clone, Serialize)]
pub struct PinnedAgent {
    pub reference: AgentRevisionRef,
    pub agent: Agent,
}

/// A resolved agent location.
struct Located {
    key: AgentKey,
    org_root: PathBuf,
    dir: PathBuf,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Team names become path segments: lowercase letters, digits, `-` and `_`.
pub(super) fn check_team(team: &str) -> Result<(), AgentError> {
    if is_scratch_team(team) {
        return Ok(());
    }
    let bytes = team.as_bytes();
    let ok = (1..=64).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_');
    if ok {
        Ok(())
    } else {
        Err(AgentError::InvalidLocation(format!(
            "team {team:?} is not a valid name"
        )))
    }
}

pub(super) fn check_org(org: &str) -> Result<(), AgentError> {
    // SW-CR-1: these two guards are the ACTIVE rejection path for the two
    // reserved owners — they match both the bare form (`builtin`) and the
    // two-segment form (`builtin/<x>`), so an agent key never aliases onto
    // the starter or personal namespace. `validate_org_slug` also rejects the
    // bare single-segment reservations, but as defense-in-depth: the
    // first-segment match here fires first and gives a service-shaped error.
    // SW-CR-5: the first-segment match is case-insensitive (ASCII) so a
    // user-authored `org.yml` carrying slug `"Builtin/..."` can't alias on a
    // case-folding filesystem.
    let first = org.split('/').next().unwrap_or_default();
    if first.eq_ignore_ascii_case(PERSONAL_OWNER) {
        return Err(AgentError::InvalidLocation(format!(
            "org {org:?} is reserved for scratch spaces"
        )));
    }
    if first.eq_ignore_ascii_case(BUILTIN_ORG) {
        return Err(AgentError::InvalidLocation(format!(
            "org {org:?} is reserved for starter agents"
        )));
    }
    match validate_org_slug(org) {
        None => Ok(()),
        Some(why) => Err(AgentError::InvalidLocation(format!(
            "org {org:?} is not valid: {why}"
        ))),
    }
}

/// Map a key to its directory, rejecting anything that could escape the orgs
/// tree. Another user's personal agent is not found: its path would be the
/// caller's own personal space.
/// A starter has no directory, so every write through here is refused.
fn locate(home: &Path, principal: &Principal, key: &AgentKey) -> Result<Located, AgentError> {
    let not_found = || AgentError::NotFound(key.clone());
    if key.is_builtin() {
        return Err(if builtin::is_starter(key) {
            AgentError::ReadOnly(key.clone())
        } else {
            not_found()
        });
    }
    if !key.resolves_for(principal) {
        return Err(not_found());
    }
    check_org(&key.org)?;
    check_team(&key.team)?;
    if !is_valid_id(&key.id) {
        return Err(not_found());
    }
    let agents_dir =
        roots::team_agents_dir(home, principal, &key.org, &key.team)?.ok_or_else(not_found)?;
    Ok(Located {
        key: key.clone(),
        org_root: roots::org_root(home, principal, &key.org, None)?,
        dir: agents_dir.join(&key.id),
    })
}

/// The `agents/` dir of a team a new agent may be written to: a valid team
/// that discovery scans, or an agent written there would never be listed.
fn team_placement_dir(
    home: &Path,
    principal: &Principal,
    org: &str,
    team: &str,
) -> Result<PathBuf, AgentError> {
    if is_scratch_team(team) {
        return Err(AgentError::InvalidLocation(
            "the scratch space is a personal placement, not a team".into(),
        ));
    }
    check_org(org)?;
    check_team(team)?;
    if !roots::is_synced_team(home, principal, org, team)? {
        return Err(AgentError::InvalidLocation(format!(
            "team {org}/{team} is not synced on this machine"
        )));
    }
    roots::team_agents_dir(home, principal, org, team)?
        .ok_or_else(|| AgentError::InvalidLocation(format!("team {org}/{team} has no agents dir")))
}

/// Resolve a placement to the new agent's key and `agents/` dir, creating the
/// personal space if this is its first save.
fn resolve_placement(
    home: &Path,
    principal: &Principal,
    placement: &Placement,
    id: &str,
) -> Result<(AgentKey, PathBuf), AgentError> {
    match placement {
        Placement::Personal(owner) => {
            check_org(&owner.org)?;
            let dir = roots::create_personal_agents_dir(home, principal, owner)?;
            Ok((AgentKey::personal(principal, &owner.org, id), dir))
        }
        Placement::Team { org, team } => Ok((
            AgentKey::scoped(principal, org, team, id),
            team_placement_dir(home, principal, org, team)?,
        )),
    }
}

/// Save a fully validated typed fixture at `placement`, as its revision.
#[cfg(test)]
pub(crate) fn save_typed_for_test(
    home: &Path,
    principal: &Principal,
    agent: &Agent,
    placement: &Placement,
) -> Result<AgentKey, String> {
    agent.validate().map_err(|e| e.to_string())?;
    let (key, root) =
        resolve_placement(home, principal, placement, &agent.id).map_err(|e| e.to_string())?;
    let dir = root.join(&agent.id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    defs::write_revision(
        &dir,
        agent,
        &Author::user(principal.as_str()),
        &now_rfc3339(),
    )
    .map_err(|e| e.to_string())?;
    Ok(key)
}

/// Everything [`save`] checks before creating `agent` at `placement`,
/// writing nothing (not even a first personal space): the body, the
/// placement and a free id. Returns the key it would get and the agent as it
/// would be stored.
pub fn check_create(
    home: &Path,
    principal: &Principal,
    placement: &Placement,
    agent: Agent,
) -> Result<(AgentKey, Agent), AgentError> {
    let mut agent = Agent {
        created_by: principal.to_string(),
        revision: 1,
        archived_at: None,
        ..agent
    };
    agent.normalize_startup()?;
    agent.validate()?;
    let (key, dir) = match placement {
        Placement::Personal(owner) => {
            roots::check_personal_owner(principal, owner)?;
            check_org(&owner.org)?;
            (
                AgentKey::personal(principal, &owner.org, &agent.id),
                roots::personal_agents_dir(home, principal, &owner.org)?,
            )
        }
        Placement::Team { org, team } => (
            AgentKey::scoped(principal, org, team, &agent.id),
            Some(team_placement_dir(home, principal, org, team)?),
        ),
    };
    if dir.is_some_and(|dir| holds_id(&dir.join(&agent.id))) {
        return Err(AgentError::AlreadyExists(key));
    }
    Ok((key, agent))
}

/// Whether an agent dir takes its id: it exists and is more than a home for
/// routines (a deleted agent's archived routines, or routines naming an agent
/// not made yet, SW-AGR-1), which a new agent of that id takes back.
fn holds_id(dir: &Path) -> bool {
    let routines_only = !dir.join(defs::AGENT_FILE).exists()
        && std::fs::read_dir(dir).is_ok_and(|entries| {
            entries
                .filter_map(Result::ok)
                .all(|e| e.file_name() == crate::routines::layout::ROUTINES_DIR)
        });
    dir.exists() && !routines_only
}

fn read_existing(loc: &Located) -> Result<Agent, AgentError> {
    defs::read_def(&loc.dir)?.ok_or_else(|| AgentError::NotFound(loc.key.clone()))
}

fn commit(loc: &Located, agent: Agent, author: &Author) -> Result<SavedAgent, AgentError> {
    defs::write_revision(&loc.dir, &agent, author, &now_rfc3339())?;
    Ok(SavedAgent {
        key: loc.key.clone(),
        agent,
    })
}

/// Commit `next` over `current`, first snapshotting `current` if it has no
/// revision file, so whatever pinned it keeps resolving.
fn advance(
    loc: &Located,
    current: &Agent,
    next: Agent,
    author: &Author,
) -> Result<SavedAgent, AgentError> {
    let at = now_rfc3339();
    defs::snapshot_if_missing(&loc.dir, current, &at)?;
    defs::write_revision(&loc.dir, &next, author, &at)?;
    Ok(SavedAgent {
        key: loc.key.clone(),
        agent: next,
    })
}

/// Every agent `principal` can see, tagged with its scope, org, team and
/// creator, then the starters. Archived agents only with `include_archived`.
pub fn list(
    home: &Path,
    principal: &Principal,
    include_archived: bool,
) -> Result<Vec<DiscoveredAgent>, AgentError> {
    list_for(home, principal, include_archived, &Default::default())
}

pub(crate) fn list_for(
    home: &Path,
    principal: &Principal,
    include_archived: bool,
    scope: &crate::context_repo::caller_scope::CallerScope,
) -> Result<Vec<DiscoveredAgent>, AgentError> {
    let mut found = super::discover::discover_agents_for(home, principal, include_archived, scope)?;
    found.extend(
        builtin::all()
            .into_iter()
            .map(|(key, agent)| DiscoveredAgent {
                scope: AgentScope::Builtin,
                org: key.org.clone(),
                team: key.team.clone(),
                created_by_display: builtin::CREATOR.to_string(),
                dir: PathBuf::new(),
                key,
                agent,
            }),
    );
    Ok(found)
}

/// A starter's one revision, credited to CodeSense.
fn builtin_revision(agent: &Agent) -> RevisionRecord {
    RevisionRecord {
        revision: agent.revision,
        author: Author::user(builtin::CREATOR),
        at: String::new(),
        definition_hash: builtin::definition_hash(agent),
        agent: agent.clone(),
        repo_overrides: Default::default(),
    }
}

/// One agent, archived or not, with every revision and its author.
pub fn get(home: &Path, principal: &Principal, key: &AgentKey) -> Result<AgentDetail, AgentError> {
    if let Some(agent) = builtin::get(key) {
        return Ok(AgentDetail {
            key: key.clone(),
            scope: AgentScope::Builtin,
            created_by_display: builtin::CREATOR.to_string(),
            revisions: vec![builtin_revision(&agent)],
            agent,
        });
    }
    let loc = locate(home, principal, key)?;
    let agent = read_existing(&loc)?;
    crate::session_usage::spend_caps::status(home, "agent", &key.to_string(), agent.budget_usd)
        .map_err(AgentError::Io)?;
    detail_at(&loc, agent)
}

fn detail_at(loc: &Located, agent: Agent) -> Result<AgentDetail, AgentError> {
    let revisions = defs::read_revisions(&loc.dir, agent.revision)?;
    Ok(AgentDetail {
        key: loc.key.clone(),
        scope: AgentScope::of_team(&loc.key.team),
        created_by_display: crate::routines::discover::OwnerNames::default().display(
            &loc.key.org,
            &loc.org_root,
            &agent.created_by,
        ),
        agent,
        revisions,
    })
}

/// The agent as it was at `revision`, archived or not, with its
/// `agent@revision` reference and the hash of the bytes it was read from.
pub(crate) fn get_snapshot(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
) -> Result<(AgentDetail, AgentRevisionRef), AgentError> {
    if let Some(agent) = builtin::get(key) {
        let reference = AgentRevisionRef {
            key: key.clone(),
            revision: agent.revision,
            definition_hash: builtin::definition_hash(&agent),
        };
        return Ok((get(home, principal, key)?, reference));
    }
    let loc = locate(home, principal, key)?;
    let pinned =
        defs::read_current_snapshot(&loc.dir)?.ok_or_else(|| AgentError::NotFound(key.clone()))?;
    let reference = AgentRevisionRef {
        key: key.clone(),
        revision: pinned.agent.revision,
        definition_hash: pinned.definition_hash,
    };
    let detail = detail_at(&loc, pinned.agent)?;
    Ok((detail, reference))
}

/// The agent as it was at `revision`, archived or not, with its reference.
pub fn get_pinned(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    revision: u32,
) -> Result<PinnedAgent, AgentError> {
    // A starter keeps no history: a revision an earlier build stamped (the
    // revision only rises with the app version) resolves to this build's
    // definition. One no build has stamped is not found.
    if let Some(agent) = builtin::get(key) {
        if revision == 0 || revision > agent.revision {
            return Err(AgentError::RevisionNotFound {
                key: key.clone(),
                revision,
            });
        }
        return Ok(PinnedAgent {
            reference: AgentRevisionRef {
                key: key.clone(),
                revision,
                definition_hash: builtin::definition_hash(&agent),
            },
            agent,
        });
    }
    let loc = locate(home, principal, key)?;
    let Some((agent, definition_hash)) = defs::read_pinned(&loc.dir, revision)? else {
        if defs::read_def(&loc.dir)?.is_none() {
            return Err(AgentError::NotFound(loc.key));
        }
        return Err(AgentError::RevisionNotFound {
            key: loc.key,
            revision,
        });
    };
    Ok(PinnedAgent {
        reference: AgentRevisionRef {
            key: loc.key,
            revision,
            definition_hash,
        },
        agent,
    })
}

/// Create or update an agent.
///
/// - **Create** writes revision 1 with `created_by` = the caller. The id must
///   be free: an archived agent still holds its id.
/// - **Update** replaces the body of an existing agent, whoever created it.
///   `created_by` and `archived_at` carry over; [`archive`] and [`unarchive`]
///   are the only ways to change the latter.
///
/// Startup paths are stored normalized.
pub fn save(
    home: &Path,
    principal: &Principal,
    target: SaveTarget,
    agent: Agent,
    author: &Author,
) -> Result<SavedAgent, AgentError> {
    save_with_overrides(home, principal, target, agent, &[], author)
}

/// [`save`], also replacing the agent's per-repo context override for each
/// repo in `overrides` (an empty override removes it). The overrides are part
/// of the revision: they are checked with the body, written under the same
/// lock before the revision is committed, and snapshotted with it, so a
/// stale `base_revision` refuses them too.
pub fn save_with_overrides(
    home: &Path,
    principal: &Principal,
    target: SaveTarget,
    mut agent: Agent,
    overrides: &[(SourceRepoSlug, RepoOverride)],
    author: &Author,
) -> Result<SavedAgent, AgentError> {
    // An update's key is checked before its body: a starter is refused even
    // with a body that would not save, since copying it is the fix.
    if let SaveTarget::Update { key, .. } = &target {
        locate(home, principal, key)?;
    }
    agent.normalize_startup()?;
    let overrides = check_repo_overrides(&agent, overrides)?;
    let saved = match target {
        SaveTarget::Create(placement) => {
            // Every check before the placement: a first personal save creates
            // the personal space, which a refused create must not leave behind.
            let (_, agent) = check_create(home, principal, &placement, agent)?;
            let (key, _) = resolve_placement(home, principal, &placement, &agent.id)?;
            let loc = locate(home, principal, &key)?;
            defs::with_agent_lock(&loc.dir, || {
                // A dir holding only `routines/` (a deleted agent's archived
                // routines, or routines naming an agent not made yet) does not
                // hold the id: the new agent takes those routines back.
                if holds_id(&loc.dir) {
                    return Err(AgentError::AlreadyExists(loc.key.clone()));
                }
                let written =
                    write_overrides(&loc, &overrides).and_then(|()| commit(&loc, agent, author));
                if written.is_err() {
                    // Leave no half-made agent dir: a retry must find the id free.
                    let _ = remove_keeping_routines(&loc.dir);
                }
                written
            })
        }
        SaveTarget::Update { key, base_revision } => {
            let loc = locate(home, principal, &key)?;
            if agent.id != key.id {
                return Err(AgentError::InvalidLocation(format!(
                    "agent id {:?} cannot be changed to {:?}",
                    key.id, agent.id
                )));
            }
            defs::with_agent_lock(&loc.dir, || {
                let current = read_existing(&loc)?;
                crate::session_usage::spend_caps::status(
                    home,
                    "agent",
                    &loc.key.to_string(),
                    current.budget_usd,
                )
                .map_err(AgentError::Io)?;
                if current.revision != base_revision {
                    return Err(AgentError::Conflict {
                        current_revision: current.revision,
                    });
                }
                let agent = Agent {
                    created_by: current.created_by.clone(),
                    archived_at: current.archived_at.clone(),
                    revision: current.revision + 1,
                    ..agent
                };
                agent.validate()?;
                // The current revision is pinned as it is (overrides included)
                // before anything changes.
                let at = now_rfc3339();
                defs::snapshot_if_missing(&loc.dir, &current, &at)?;
                let before = defs::snapshot_override_files(&loc.dir)?;
                let written = write_overrides(&loc, &overrides).and_then(|()| {
                    defs::write_revision(&loc.dir, &agent, author, &at)?;
                    Ok(())
                });
                if let Err(e) = written {
                    // The revision did not land: put the override files back,
                    // byte for byte.
                    let _ = defs::restore_override_files(&loc.dir, &before);
                    return Err(e);
                }
                Ok(SavedAgent {
                    key: loc.key.clone(),
                    agent,
                })
            })
        }
    }?;
    crate::session_usage::spend_caps::status(
        home,
        "agent",
        &saved.key.to_string(),
        saved.agent.budget_usd,
    )
    .map_err(AgentError::Io)?;
    super::events::changed_at(&saved.key.to_string(), saved.agent.revision);
    Ok(saved)
}

/// Each override normalized and checked against `agent`'s context (or the
/// runtime default it resolves to), or the first refusal. Writes nothing.
pub fn check_repo_overrides(
    agent: &Agent,
    changes: &[(SourceRepoSlug, RepoOverride)],
) -> Result<Vec<(SourceRepoSlug, RepoOverride)>, AgentError> {
    let context = match &agent.context {
        Some(context) => {
            super::context::normalize_entries(context).map_err(ValidationError::InvalidContext)?
        }
        None => super::context::default_context(agent.runtime, false),
    };
    let mut labels = std::collections::BTreeSet::new();
    changes
        .iter()
        .map(|(slug, o)| {
            let label =
                super::context::repo_label(slug).map_err(ValidationError::InvalidRepoOverride)?;
            if !labels.insert(label.clone()) {
                return Err(ValidationError::InvalidRepoOverride(
                    super::context::ContextError::InvalidRepo {
                        repo: label,
                        why: "is listed twice".into(),
                    },
                )
                .into());
            }
            let o = if o.is_empty() {
                RepoOverride::default()
            } else {
                o.normalized_with(&context)
                    .map_err(ValidationError::InvalidRepoOverride)?
            };
            Ok((slug.clone(), o))
        })
        .collect()
}

fn write_overrides(
    loc: &Located,
    overrides: &[(SourceRepoSlug, RepoOverride)],
) -> Result<(), AgentError> {
    for (slug, o) in overrides {
        let path = defs::repo_override_path(&loc.dir, slug)
            .map_err(ValidationError::InvalidRepoOverride)?;
        defs::write_repo_override_at(&loc.dir, &path, o)?;
    }
    Ok(())
}

/// Set or clear `archived_at` as a new revision. No body validation: retiring
/// an agent must work even when its stored body no longer passes this build's
/// checks. A stale `base_revision`, when given, is a conflict; already in the
/// asked state is a no-op.
fn set_archived(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    archived: bool,
    base_revision: Option<u32>,
    author: &Author,
) -> Result<SavedAgent, AgentError> {
    let loc = locate(home, principal, key)?;
    defs::with_agent_lock(&loc.dir, || {
        let current = read_existing(&loc)?;
        if base_revision.is_some_and(|base| base != current.revision) {
            return Err(AgentError::Conflict {
                current_revision: current.revision,
            });
        }
        if current.is_archived() == archived {
            return Ok(SavedAgent {
                key: loc.key.clone(),
                agent: current,
            });
        }
        let agent = Agent {
            archived_at: archived.then(now_rfc3339),
            revision: current.revision + 1,
            ..current.clone()
        };
        let saved = advance(&loc, &current, agent, author)?;
        super::events::changed(&saved.key.to_string());
        Ok(saved)
    })
}

/// Archive an agent as a new revision. It drops out of [`list`] and pickers,
/// while [`get`], [`get_pinned`] and everything that references it keep
/// resolving.
pub fn archive(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    author: &Author,
) -> Result<SavedAgent, AgentError> {
    set_archived(home, principal, key, true, None, author)
}

/// Bring an archived agent back, as a new revision. `base_revision` is the
/// revision the caller last read.
pub fn unarchive(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    base_revision: u32,
    author: &Author,
) -> Result<SavedAgent, AgentError> {
    set_archived(home, principal, key, false, Some(base_revision), author)
}

/// Write a new revision whose body is revision `revision`'s. `created_by` and
/// `archived_at` stay as they are now, so restoring never un-archives.
pub fn restore(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    revision: u32,
    base_revision: u32,
    author: &Author,
) -> Result<SavedAgent, AgentError> {
    let loc = locate(home, principal, key)?;
    defs::with_agent_lock(&loc.dir, || {
        let current = read_existing(&loc)?;
        if current.revision != base_revision {
            return Err(AgentError::Conflict {
                current_revision: current.revision,
            });
        }
        // The same revisions get_pinned serves: the live body at its own
        // revision, never a snapshot an interrupted save left past it.
        let pinned = defs::read_pinned_full(&loc.dir, revision)?.ok_or_else(|| {
            AgentError::RevisionNotFound {
                key: loc.key.clone(),
                revision,
            }
        })?;
        let old = pinned.agent;
        let mut agent = Agent {
            id: current.id.clone(),
            created_by: current.created_by.clone(),
            archived_at: current.archived_at.clone(),
            revision: current.revision + 1,
            ..old
        };
        // An old body may predate this build's rules (or be a hand edit): it
        // passes the same checks as any save before it becomes current.
        agent.normalize_startup()?;
        agent.validate()?;
        // SW-XS-13: the revision's per-repo overrides come back with it.
        let at = now_rfc3339();
        defs::snapshot_if_missing(&loc.dir, &current, &at)?;
        let before = defs::snapshot_override_files(&loc.dir)?;
        let restored = defs::replace_repo_overrides(&loc.dir, &pinned.repo_overrides)
            .and_then(|()| defs::write_revision(&loc.dir, &agent, author, &at));
        if let Err(e) = restored {
            let _ = defs::restore_override_files(&loc.dir, &before);
            return Err(e.into());
        }
        let saved = SavedAgent {
            key: loc.key.clone(),
            agent,
        };
        super::events::changed(&saved.key.to_string());
        Ok(saved)
    })
}

/// The current definition of `key`, without its history.
pub(crate) fn read_current(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
) -> Result<Agent, AgentError> {
    if let Some(agent) = builtin::get(key) {
        return Ok(agent);
    }
    let agent = read_existing(&locate(home, principal, key)?)?;
    crate::session_usage::spend_caps::status(home, "agent", &key.to_string(), agent.budget_usd)
        .map_err(AgentError::Io)?;
    Ok(agent)
}

/// SW-XS-13: every per-repo context override of `key`, keyed
/// `host/owner/repo`, and one message per override file that could not be
/// read (it is skipped). A starter has none.
pub fn repo_overrides(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
) -> Result<(defs::RepoOverrides, Vec<String>), AgentError> {
    if builtin::get(key).is_some() {
        return Ok(Default::default());
    }
    let loc = locate(home, principal, key)?;
    read_existing(&loc)?;
    Ok(defs::read_repo_overrides(&loc.dir)?)
}

/// `key`'s override for `slug`, if it has one.
pub fn repo_override(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    slug: &SourceRepoSlug,
) -> Result<Option<RepoOverride>, AgentError> {
    if builtin::get(key).is_some() {
        return Ok(None);
    }
    let loc = locate(home, principal, key)?;
    let path =
        defs::repo_override_path(&loc.dir, slug).map_err(ValidationError::InvalidRepoOverride)?;
    Ok(defs::read_repo_override_at(&path)?)
}

/// SW-XS-13: the want set `key` resolves to at `target` (the repo's override
/// applied when the target names a repo). Read-only; no launch consumes it
/// yet.
pub fn resolve_agent_context(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    target: &ManifestTarget,
    team_workspace: bool,
) -> Result<ContextResolution, AgentError> {
    resolve_agent_context_with(
        &super::content_reader::FsContentReader,
        home,
        principal,
        key,
        target,
        team_workspace,
    )
}

pub(crate) fn resolve_agent_context_with(
    reader: &dyn super::content_reader::ContentReader,
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    target: &ManifestTarget,
    team_workspace: bool,
) -> Result<ContextResolution, AgentError> {
    let agent = read_current(home, principal, key)?;
    let repo_override = match &target.repo {
        Some(slug) => repo_override(home, principal, key, slug)?,
        None => None,
    };
    Ok(super::manifest_resolve::context::resolve_context_with(
        reader,
        home,
        principal,
        ContextRequest {
            agent: &agent,
            target,
            repo_override: repo_override.as_ref(),
            team_workspace,
        },
    ))
}

/// SW-XS-7: the want set of `key` exactly at the revision a launch captured
/// (its definition and per-repo overrides at that revision). A definition
/// whose hash no longer matches the capture is `newIntentRequired`: the
/// launch was prepared against something that has since changed.
pub fn resolve_agent_context_pinned(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    revision: u32,
    definition_hash: &str,
    target: &ManifestTarget,
    team_workspace: bool,
) -> Result<ContextResolution, String> {
    let stale = || {
        format!(
            "newIntentRequired: {key} changed after the launch was prepared (revision {revision}); start a new launch"
        )
    };
    if let Some(agent) = builtin::get(key) {
        return Ok(resolve_context(
            home,
            principal,
            ContextRequest {
                agent: &agent,
                target,
                repo_override: None,
                team_workspace,
            },
        ));
    }
    let loc = locate(home, principal, key).map_err(|e| e.to_string())?;
    let pinned = defs::read_pinned_full(&loc.dir, revision)
        .map_err(|e| e.to_string())?
        .ok_or_else(stale)?;
    if pinned.definition_hash != definition_hash {
        return Err(stale());
    }
    let repo_override = target
        .repo
        .as_ref()
        .and_then(|slug| super::context::repo_label(slug).ok())
        .and_then(|label| pinned.repo_overrides.get(&label).cloned());
    Ok(resolve_context(
        home,
        principal,
        ContextRequest {
            agent: &pinned.agent,
            target,
            repo_override: repo_override.as_ref(),
            team_workspace,
        },
    ))
}

/// Who references the agent, as reported by `referrers`.
pub fn used_by(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    referrers: &dyn AgentReferrers,
) -> Result<Vec<String>, AgentError> {
    if builtin::is_starter(key) {
        return Ok(referrers.referrers(home, principal, key));
    }
    let loc = locate(home, principal, key)?;
    read_existing(&loc)?;
    Ok(referrers.referrers(home, principal, &loc.key))
}

/// Remove the agent's directory, history included, only when nothing
/// references it. Otherwise [`AgentError::InUse`] and nothing changes. A
/// stale `base_revision`, when given, is a conflict: the caller decided on a
/// definition that is no longer current.
pub fn delete(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    base_revision: Option<u32>,
    referrers: &dyn AgentReferrers,
) -> Result<(), AgentError> {
    let loc = locate(home, principal, key)?;
    defs::with_agent_lock(&loc.dir, || {
        let current = read_existing(&loc)?;
        if base_revision.is_some_and(|base| base != current.revision) {
            return Err(AgentError::Conflict {
                current_revision: current.revision,
            });
        }
        let referrers_of = referrers;
        let referrers = referrers_of.referrers(home, principal, &loc.key);
        if !referrers.is_empty() {
            let owned = referrers_of.owned(home, principal, &loc.key);
            return Err(AgentError::InUse { referrers, owned });
        }
        remove_keeping_routines(&loc.dir)
            .map_err(|e| AgentError::Io(format!("{}: {e}", loc.dir.display())))
    })?;
    super::events::changed(&loc.key.to_string());
    Ok(())
}

/// Remove an agent dir, history included, except its `routines/` (SW-AGR-1):
/// a delete is only allowed once every routine of the agent is archived, and
/// archived routines keep their definitions and history (they then read as
/// Unassigned). With no `routines/`, the whole dir goes.
fn remove_keeping_routines(dir: &Path) -> std::io::Result<()> {
    // A read-only space (the CodeSense docs) refuses before anything goes,
    // with or without `routines/` (SW-AGR-10: the docs' agent has one).
    crate::context_repo::codesense_docs::refuse_io_write_at(dir)?;
    let routines = dir.join(crate::routines::layout::ROUTINES_DIR);
    if !routines.is_dir() {
        return crate::shared::fs_atomic::remove_dir_all(dir);
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path == routines {
            continue;
        }
        if path.is_dir() {
            crate::shared::fs_atomic::remove_dir_all(&path)?;
        } else {
            crate::shared::fs_atomic::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Copy any visible agent (typically a teammate's, or a starter) into a new
/// one created by the caller: a fresh id, revision 1. A starter's copy keeps
/// its startup (the runtime's root instructions file), so it runs exactly as
/// the starter does until it is edited.
pub fn copy_to_mine(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    placement: Placement,
    author: &Author,
) -> Result<SavedAgent, AgentError> {
    let mut source = read_current(home, principal, key)?;
    // Before the placement: a first personal copy creates the personal space,
    // which a body that cannot be saved must not leave behind.
    source.normalize_startup()?;
    Agent {
        revision: 1,
        ..source.clone()
    }
    .validate()?;
    let (_, agents_dir) = resolve_placement(home, principal, &placement, &source.id)?;
    let stem: String = source.id.chars().take(55).collect();
    let id = first_free_id(&agents_dir, &format!("{stem}-copy"));
    save(
        home,
        principal,
        SaveTarget::Create(placement),
        Agent { id, ..source },
        author,
    )
}

/// `base`, then `base-2`, `base-3`, … : the first agent id with no definition
/// directory, archived or not, in `agents_root`.
pub fn first_free_id(agents_root: &Path, base: &str) -> String {
    let free = |candidate: &str| !agents_root.join(candidate).exists();
    if free(base) {
        return base.to_string();
    }
    (2u32..)
        .map(|n| format!("{base}-{n}"))
        .find(|c| free(c))
        .unwrap_or_else(|| base.to_string())
}
