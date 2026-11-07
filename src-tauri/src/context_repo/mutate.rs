// SPDX-License-Identifier: MIT
//! Registry-file mutations. Reads the affected files on `main`'s
//! working tree, applies the in-memory edit, and returns the list of
//! `(relative-path, new-bytes)` to stage into a single commit.
//!
//! No git2, no network. Kept pure so the commit/push step upstream
//! can apply the same file-writes uniformly for single- and multi-
//! file variants.

use std::fmt;
use std::path::{Path, PathBuf};

use rand::RngCore;

use super::branch_common::{validate_team_name, TeamNameError};
use crate::domain::{
    parse_org_yml, parse_people_yml, parse_teams_yml, Person, RawIdentity, RawPeopleYml,
    RawPersonEntry, RawSourceRepoEntry, RawTeamEntry, RawTeamsYml, RegistryFile, SourceRepoSlug,
};

/// The user-intent expressed as a mutation to one or more registry
/// files. Marked `#[non_exhaustive]` so each downstream story can add
/// its own variant without breaking exhaustive-match compile errors
/// here.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum RegistryEdit {
    RenameWorkspace {
        new_display_name: String,
    },
    /// Append a new person to `people.yml` by GitHub username. The
    /// Tauri command resolves the username to a full profile before
    /// this variant reaches `apply_add_person` — `apply_registry_edit`
    /// does NOT handle this variant directly (it requires network).
    AddPersonByUsername {
        username: String,
    },
    AddPersonByName {
        display_name: String,
    },
    AddIdentityToPerson {
        person_id: String,
        provider: String,
        handle: String,
        provider_user_id: Option<String>,
    },
    /// Remove a person from `people.yml`. Single commit, single-file
    /// `AppliedEdit` (teams no longer carry members).
    RemovePersonByUserId {
        github_user_id: u64,
    },
    /// Remove any roster person by their stable, provider-independent key.
    RemovePersonById {
        person_id: String,
    },
    /// Append a source repo slug to a team's `source_repos` in `teams.yml`.
    AddSourceRepoToTeam {
        team_name: String,
        owner: String,
        repo: String,
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        host: Option<String>,
    },
    /// Remove a source repo slug from a team's `source_repos` in `teams.yml`.
    RemoveSourceRepoFromTeam {
        team_name: String,
        owner: String,
        repo: String,
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        host: Option<String>,
    },
    /// Append a brand-new team to `teams.yml` (`source_repos` empty). The
    /// creator's profile rides in the payload (not resolved via network) so this
    /// stays a pure file-mutation; it is appended to `people.yml` if absent so
    /// the org roster records the creator (open access — `connect_to_org` does
    /// not seed the joiner). Teams no longer carry members, so the team
    /// entry itself only names the team.
    CreateTeam {
        team_name: String,
        #[serde(default)]
        creator_id: u64,
        #[serde(default)]
        creator_username: String,
        creator_display_name: Option<String>,
        creator_avatar_url: Option<String>,
    },
    /// DE-REGISTER a team — strip its entry from `teams.yml` and
    /// nothing else (AC-3). Content under `_team/{team}/`, installs, worktree
    /// config, and the `.meta/` mirror are deliberately left on disk; the
    /// removed entry is a revertible commit, so a delete is git-recoverable.
    /// Local-only, mirrors `CreateTeam` (no remote write / PR).
    DeleteTeam {
        team_name: String,
    },
    /// Set (or clear) a team's `archived` flag in `teams.yml`. Archive
    /// is org-wide, reversible retirement — distinct from the per-user
    /// `shownTeamsByOrgSlug` hide (AC-4). `archived: false` unarchives.
    /// Local-only.
    ArchiveTeam {
        team_name: String,
        archived: bool,
    },
    /// Rename a team in place: rewrite its `name` in `teams.yml` AND move
    /// everything keyed by the old name (its `_team/{name}/` content subtree,
    /// its `installs/{org}/{name}/…` lockfiles, and any team-keyed metadata).
    /// Unlike every other variant this is NOT a pure `(path, bytes)` file-write
    /// — it moves directories across the git / non-git boundary, so it is
    /// rejected by `apply_registry_edit` and handled by the dedicated
    /// `rename_team` orchestrator (see `team_rename.rs`), which owns the
    /// pre-flight, the single commit, the guarded non-git moves, and rollback.
    RenameTeam {
        old_name: String,
        new_name: String,
    },
    /// Set (or clear) a team's `parent_team` in `teams.yml`. `None`
    /// clears the link (the team becomes a root). A pure in-place field flip —
    /// mirrors `ArchiveTeam`: only the matched entry changes, so untouched teams
    /// serialize byte-stable. Local-only. Cycles and dangling parents are caught
    /// at load time; the picker prevents cycles at write time.
    SetTeamParent {
        team_name: String,
        parent_team: Option<String>,
    },
    /// Set (or clear) a team's starter page — `teams.yml` `landing_page:`, the
    /// file the viewer opens whenever the team has nothing open. `Some(path)` is
    /// relative to the team's content folder and must name a file there (no
    /// `..`, no hidden file or folder); `None` removes the key, so the app falls
    /// back to README.md / README.html / index.html. A pure in-place field flip
    /// like `SetTeamParent`: untouched teams serialize byte-stable. Local-only.
    SetTeamLandingPage {
        team_name: String,
        landing_page: Option<String>,
    },
    /// Set (or clear) the per-org `workspace_root_dir` in
    /// `org.yml::config_flags`. `Some(abs_path)` points managed clones at a
    /// visible root; `None` reverts to the hidden default. A pure single-file
    /// `org.yml` rewrite — mirrors `RenameWorkspace`/`apply_rename`. Affects only
    /// FUTURE clones (existing managed clones stay put). Local-only.
    SetWorkspaceRoot {
        workspace_root_dir: Option<String>,
    },
}

impl RegistryEdit {
    /// The variant name, spelled exactly as the wire `type` tag the frontend
    /// sends — so a listener can switch on it without a second vocabulary.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::RenameWorkspace { .. } => "RenameWorkspace",
            Self::AddPersonByUsername { .. } => "AddPersonByUsername",
            Self::AddPersonByName { .. } => "AddPersonByName",
            Self::AddIdentityToPerson { .. } => "AddIdentityToPerson",
            Self::RemovePersonByUserId { .. } => "RemovePersonByUserId",
            Self::RemovePersonById { .. } => "RemovePersonById",
            Self::AddSourceRepoToTeam { .. } => "AddSourceRepoToTeam",
            Self::RemoveSourceRepoFromTeam { .. } => "RemoveSourceRepoFromTeam",
            Self::CreateTeam { .. } => "CreateTeam",
            Self::DeleteTeam { .. } => "DeleteTeam",
            Self::ArchiveTeam { .. } => "ArchiveTeam",
            Self::RenameTeam { .. } => "RenameTeam",
            Self::SetTeamParent { .. } => "SetTeamParent",
            Self::SetTeamLandingPage { .. } => "SetTeamLandingPage",
            Self::SetWorkspaceRoot { .. } => "SetWorkspaceRoot",
        }
    }

    /// The team this edit is about after it lands (the NEW name for a
    /// rename); `None` for an org- or roster-wide edit.
    pub fn team(&self) -> Option<&str> {
        match self {
            Self::AddSourceRepoToTeam { team_name, .. }
            | Self::RemoveSourceRepoFromTeam { team_name, .. }
            | Self::CreateTeam { team_name, .. }
            | Self::DeleteTeam { team_name }
            | Self::ArchiveTeam { team_name, .. }
            | Self::SetTeamParent { team_name, .. }
            | Self::SetTeamLandingPage { team_name, .. } => Some(team_name),
            Self::RenameTeam { new_name, .. } => Some(new_name),
            Self::RenameWorkspace { .. }
            | Self::AddPersonByUsername { .. }
            | Self::AddPersonByName { .. }
            | Self::AddIdentityToPerson { .. }
            | Self::RemovePersonByUserId { .. }
            | Self::RemovePersonById { .. }
            | Self::SetWorkspaceRoot { .. } => None,
        }
    }

    /// The team's name before a rename; `None` for every other edit.
    pub fn previous_team(&self) -> Option<&str> {
        match self {
            Self::RenameTeam { old_name, .. } => Some(old_name),
            _ => None,
        }
    }
}

/// The result of applying a `RegistryEdit` in-memory.
///
/// `files` contains one tuple per modified registry file with a path
/// RELATIVE TO `org_root`. Single-file variants (rename) produce one
/// tuple; multi-file variants (member removal in Part 4) produce
/// several. The local apply path (`write_edit_to_workdir`) writes each
/// file to the working tree in turn without committing; the git-commit
/// paths (PR flow, `rename_team`) stage all files into a single commit.
#[derive(Debug, Clone)]
pub struct AppliedEdit {
    pub files: Vec<(PathBuf, Vec<u8>)>,

    pub commit_title: String,
}

/// Distinct error domains mapped to per-action toast templates on the
/// frontend. Each variant's `Display` prefix (e.g. `"duplicate: "`) is
/// what `handlePrFlowResult`'s classifier sniffs to select the
/// kind-specific template.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum RegistryEditError {
    Io(String),
    Parse(String),
    /// A target user is already present in `people.yml`. Surfaces the
    /// "already a member" toast on add.
    Duplicate(String),
    /// A target user / id doesn't exist (on GitHub for add; in
    /// `people.yml` for remove). Surfaces a kind-specific "not a
    /// member" / "no such user" toast.
    NotFound(String),
}

impl fmt::Display for RegistryEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(m) => write!(f, "io: {}", m),
            Self::Parse(m) => write!(f, "parse: {}", m),
            Self::Duplicate(m) => write!(f, "duplicate: {}", m),
            Self::NotFound(m) => write!(f, "not_found: {}", m),
        }
    }
}

impl std::error::Error for RegistryEditError {}

// ---------- pr_copy ----------

/// Context values available to copy generators. Fields are optional;
/// only the relevant subset is populated per variant.
pub struct EditContext<'a> {
    /// Pre-edit `display_name` of the workspace — used by `RenameWorkspace`.
    pub old_display_name: Option<&'a str>,
    /// GitHub username being added / removed — used by add/remove.
    pub target_username: Option<&'a str>,
    /// Display name of the added person (resolved from GitHub's profile
    /// or falling back to `target_username` when GitHub returns `name: null`).
    pub target_display_name: Option<&'a str>,
}

/// Per-variant commit + PR copy. The single source of truth for every
/// string that reaches GitHub or the commit log. Adding a new variant
/// means adding one `match` arm here; no template strings anywhere
/// else in the mutation / PR-flow path.
pub fn pr_copy(edit: &RegistryEdit, ctx: &EditContext<'_>) -> PrCopy {
    match edit {
        RegistryEdit::RenameWorkspace { new_display_name } => {
            let old = ctx.old_display_name.unwrap_or("(unknown)");
            let new = new_display_name.trim();
            PrCopy {
                commit_title: format!("Rename workspace: {} → {}", old, new),
            }
        }
        RegistryEdit::AddPersonByUsername { username } => {
            let u = ctx.target_username.unwrap_or(username);
            let _name = ctx.target_display_name.unwrap_or(u);
            PrCopy {
                commit_title: format!("Add @{}", u),
            }
        }
        RegistryEdit::AddPersonByName { display_name } => PrCopy {
            commit_title: format!("Add {}", display_name.trim()),
        },
        RegistryEdit::AddIdentityToPerson { handle, .. } => PrCopy {
            commit_title: format!("Link @{}", handle.trim()),
        },
        RegistryEdit::RemovePersonByUserId { .. } => {
            let u = ctx.target_username.unwrap_or("user");
            PrCopy {
                commit_title: format!("Remove @{}", u),
            }
        }
        RegistryEdit::RemovePersonById { .. } => PrCopy {
            commit_title: "Remove workspace person".to_string(),
        },
        RegistryEdit::AddSourceRepoToTeam {
            team_name,
            owner,
            repo,
            ..
        } => PrCopy {
            commit_title: format!("Track {}/{} under team {}", owner, repo, team_name),
        },
        RegistryEdit::RemoveSourceRepoFromTeam {
            team_name,
            owner,
            repo,
            ..
        } => PrCopy {
            commit_title: format!("Untrack {}/{} from team {}", owner, repo, team_name),
        },
        RegistryEdit::CreateTeam { team_name, .. } => PrCopy {
            commit_title: format!("Create team {}", team_name),
        },
        RegistryEdit::DeleteTeam { team_name } => PrCopy {
            commit_title: format!("Delete team {}", team_name),
        },
        RegistryEdit::ArchiveTeam {
            team_name,
            archived,
        } => {
            let verb = if *archived { "Archive" } else { "Unarchive" };
            PrCopy {
                commit_title: format!("{} team {}", verb, team_name),
            }
        }
        RegistryEdit::RenameTeam { old_name, new_name } => PrCopy {
            commit_title: format!("Rename team {} → {}", old_name, new_name),
        },
        RegistryEdit::SetTeamParent {
            team_name,
            parent_team,
        } => match parent_team {
            Some(parent) => PrCopy {
                commit_title: format!("Set team {} to inherit from {}", team_name, parent),
            },
            None => PrCopy {
                commit_title: format!("Clear parent team for {}", team_name),
            },
        },
        RegistryEdit::SetTeamLandingPage {
            team_name,
            landing_page,
        } => match landing_page {
            Some(page) => PrCopy {
                commit_title: format!(
                    "Set the starter page of team {} to {}",
                    team_name,
                    page.trim()
                ),
            },
            None => PrCopy {
                commit_title: format!("Clear the starter page of team {}", team_name),
            },
        },
        RegistryEdit::SetWorkspaceRoot { workspace_root_dir } => match workspace_root_dir {
            Some(root) => PrCopy {
                commit_title: format!("Set workspace root to {}", root),
            },
            None => PrCopy {
                commit_title: "Clear workspace root (revert to default)".to_string(),
            },
        },
    }
}

pub struct PrCopy {
    pub commit_title: String,
}

// ---------- apply_registry_edit ----------

/// Compute the new bytes for every registry file affected by `edit`.
///
/// Does NOT touch git or the network — the caller stages the returned
/// `(path, bytes)` tuples into a single commit and pushes.
///
/// `AddPersonByUsername` is NOT handled here (it requires a GitHub
/// API call to resolve the username). Use `apply_add_person` instead,
/// passing a pre-fetched `RawPersonEntry` from `fetch_github_profile`.
pub fn apply_registry_edit(
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<AppliedEdit, RegistryEditError> {
    match edit {
        RegistryEdit::RenameWorkspace { new_display_name } => {
            apply_rename(new_display_name, edit, org_root)
        }
        RegistryEdit::RemovePersonByUserId { github_user_id } => {
            apply_remove(*github_user_id, edit, org_root)
        }
        RegistryEdit::RemovePersonById { person_id } => {
            apply_remove_by_id(person_id, edit, org_root)
        }
        RegistryEdit::AddPersonByUsername { .. } => Err(RegistryEditError::Parse(
            "add person requires a resolved profile — use apply_add_person".to_string(),
        )),
        RegistryEdit::AddPersonByName { .. } => Err(RegistryEditError::Parse(
            "name-only add returns a person — use apply_add_person_by_name".to_string(),
        )),
        RegistryEdit::AddIdentityToPerson { .. } => Err(RegistryEditError::Parse(
            "identity link returns a person — use apply_add_identity_to_person".to_string(),
        )),
        RegistryEdit::RenameTeam { .. } => Err(RegistryEditError::Parse(
            "rename moves directories across the git/non-git boundary — use rename_team"
                .to_string(),
        )),
        RegistryEdit::AddSourceRepoToTeam {
            team_name,
            owner,
            repo,
            provider,
            host,
        } => apply_add_source_repo(
            team_name,
            owner,
            repo,
            provider.as_deref(),
            host.as_deref(),
            edit,
            org_root,
        ),
        RegistryEdit::RemoveSourceRepoFromTeam {
            team_name,
            owner,
            repo,
            provider,
            host,
        } => apply_remove_source_repo(
            team_name,
            owner,
            repo,
            provider.as_deref(),
            host.as_deref(),
            edit,
            org_root,
        ),
        RegistryEdit::CreateTeam {
            team_name,
            creator_id,
            creator_username,
            creator_display_name,
            creator_avatar_url,
        } => apply_create_team(
            team_name,
            *creator_id,
            creator_username,
            creator_display_name.as_deref(),
            creator_avatar_url.as_deref(),
            edit,
            org_root,
        ),
        RegistryEdit::DeleteTeam { team_name } => apply_delete_team(team_name, edit, org_root),
        RegistryEdit::ArchiveTeam {
            team_name,
            archived,
        } => apply_archive_team(team_name, *archived, edit, org_root),
        RegistryEdit::SetTeamParent {
            team_name,
            parent_team,
        } => apply_set_team_parent(team_name, parent_team.as_deref(), edit, org_root),
        RegistryEdit::SetTeamLandingPage {
            team_name,
            landing_page,
        } => apply_set_team_landing_page(team_name, landing_page.as_deref(), edit, org_root),
        RegistryEdit::SetWorkspaceRoot { workspace_root_dir } => {
            apply_set_workspace_root(workspace_root_dir.as_deref(), edit, org_root)
        }
    }
}

fn apply_rename(
    new_display_name: &str,
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<AppliedEdit, RegistryEditError> {
    let trimmed = new_display_name.trim();
    if trimmed.is_empty() {
        return Err(RegistryEditError::Parse(
            "display name must not be empty".to_string(),
        ));
    }

    let org_yml_path = org_root.join(RegistryFile::OrgYml.filename());
    let bytes = std::fs::read(&org_yml_path)
        .map_err(|e| RegistryEditError::Io(format!("read org.yml: {}", e)))?;
    let mut raw = parse_org_yml(&bytes)
        .map_err(|e| RegistryEditError::Parse(format!("parse org.yml: {}", e)))?;

    let old_display_name = raw.display_name.clone();
    raw.display_name = trimmed.to_string();

    let new_bytes = serde_yaml::to_string(&raw)
        .map_err(|e| RegistryEditError::Parse(format!("serialize org.yml: {}", e)))?
        .into_bytes();

    let copy = pr_copy(
        edit,
        &EditContext {
            old_display_name: Some(&old_display_name),
            target_username: None,
            target_display_name: None,
        },
    );

    Ok(AppliedEdit {
        files: vec![(PathBuf::from(RegistryFile::OrgYml.filename()), new_bytes)],

        commit_title: copy.commit_title,
    })
}

/// SW-MC-2 (AC-3/AC-9): set or clear `org.yml::config_flags.workspace_root_dir`.
/// A pure single-file rewrite mirroring [`apply_rename`]. Setting validates the
/// path is **absolute** and non-empty at write-time (a relative root would fail
/// only later with a poor error); clearing (`None`) reverts to the default.
fn apply_set_workspace_root(
    workspace_root_dir: Option<&str>,
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<AppliedEdit, RegistryEditError> {
    let normalized = match workspace_root_dir {
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(RegistryEditError::Parse(
                    "workspace root must not be empty — clear it to revert to the default"
                        .to_string(),
                ));
            }
            // Accept an absolute path (e.g. a shared `/mnt/team/code` mount) OR a
            // `~`-relative path. The value is SHARED via `org.yml` (per-org sync),
            // so `~/code` is the PORTABLE form — each machine expands `~` to its
            // own home at clone time (see `commands::attachments::expand_workspace_root`),
            // so a synced setting doesn't hard-code one user's absolute home.
            let is_tilde = trimmed == "~" || trimmed.starts_with("~/");
            if !is_tilde && !Path::new(trimmed).is_absolute() {
                return Err(RegistryEditError::Parse(format!(
                    "workspace root must be an absolute path (e.g. /Users/you/code) or a `~`-relative path (e.g. ~/code); got `{}`",
                    trimmed
                )));
            }
            // Reject `..` segments — `resolve_managed_clone_dir` joins
            // `owner/repo` onto the (expanded) root verbatim (no normalization),
            // and `owner`/`repo` are already `..`-hardened by `SourceRepoSlug`;
            // harden the root symmetrically so a clone can't land at a traversed
            // location. (`~` alone is a `Normal` component, not `ParentDir`.)
            if Path::new(trimmed)
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(RegistryEditError::Parse(format!(
                    "workspace root must not contain `..` segments; got `{}`",
                    trimmed
                )));
            }
            Some(PathBuf::from(trimmed))
        }
        None => None,
    };

    let org_yml_path = org_root.join(RegistryFile::OrgYml.filename());
    let bytes = std::fs::read(&org_yml_path)
        .map_err(|e| RegistryEditError::Io(format!("read org.yml: {}", e)))?;
    let mut raw = parse_org_yml(&bytes)
        .map_err(|e| RegistryEditError::Parse(format!("parse org.yml: {}", e)))?;

    raw.config_flags.workspace_root_dir = normalized;

    let new_bytes = serde_yaml::to_string(&raw)
        .map_err(|e| RegistryEditError::Parse(format!("serialize org.yml: {}", e)))?
        .into_bytes();

    let copy = pr_copy(
        edit,
        &EditContext {
            old_display_name: None,
            target_username: None,
            target_display_name: None,
        },
    );

    Ok(AppliedEdit {
        files: vec![(PathBuf::from(RegistryFile::OrgYml.filename()), new_bytes)],

        commit_title: copy.commit_title,
    })
}

/// Read `people.yml`, hand the parsed roster to `mutate`, then serialize the
/// result back into a single-file `AppliedEdit`. The shared scaffold behind
/// add / remove / refresh: each caller supplies only the branch tag and the
/// mutation. The closure also yields the commit/PR copy, because that copy
/// depends on what the mutation discovers (the removed username, the refreshed
/// display name, and so on).
fn edit_people_yml(
    org_root: &Path,
    mutate: impl FnOnce(&mut RawPeopleYml) -> Result<PrCopy, RegistryEditError>,
) -> Result<AppliedEdit, RegistryEditError> {
    let people_yml_path = org_root.join(RegistryFile::PeopleYml.filename());
    let bytes = std::fs::read(&people_yml_path)
        .map_err(|e| RegistryEditError::Io(format!("read people.yml: {}", e)))?;
    let mut people = parse_people_yml(&bytes)
        .map_err(|e| RegistryEditError::Parse(format!("parse people.yml: {}", e)))?;

    let copy = mutate(&mut people)?;

    let new_bytes = serde_yaml::to_string(&people)
        .map_err(|e| RegistryEditError::Parse(format!("serialize people.yml: {}", e)))?
        .into_bytes();

    Ok(AppliedEdit {
        files: vec![(PathBuf::from(RegistryFile::PeopleYml.filename()), new_bytes)],

        commit_title: copy.commit_title,
    })
}

fn apply_remove(
    target_id: u64,
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<AppliedEdit, RegistryEditError> {
    // SW-TM-3: removing a person only rewrites people.yml. Teams no longer carry
    // members, so there is nothing to strip from teams.yml on person removal.
    edit_people_yml(org_root, |people| {
        // Locate the target by id; reject if absent (stale-remove guard).
        let target_idx = people
            .people
            .iter()
            .position(|p| p.github_user_id() == Some(target_id))
            .ok_or_else(|| {
                RegistryEditError::NotFound(format!(
                    "no person with id {} in people.yml",
                    target_id
                ))
            })?;
        let removed = people.people.remove(target_idx);
        Ok(pr_copy(
            edit,
            &EditContext {
                old_display_name: None,
                target_username: removed.github_handle(),
                target_display_name: None,
            },
        ))
    })
}

fn apply_remove_by_id(
    person_id: &str,
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<AppliedEdit, RegistryEditError> {
    let person_id = person_id.trim();
    if person_id.is_empty() {
        return Err(RegistryEditError::Parse(
            "person id is required".to_string(),
        ));
    }
    edit_people_yml(org_root, |people| {
        let target_idx = people
            .people
            .iter()
            .position(|person| {
                person
                    .person_id()
                    .as_ref()
                    .is_some_and(|id| id.as_str() == person_id)
            })
            .ok_or_else(|| {
                RegistryEditError::NotFound(format!("no person with id {person_id} in people.yml"))
            })?;
        let removed = people.people.remove(target_idx);
        Ok(pr_copy(
            edit,
            &EditContext {
                old_display_name: None,
                target_username: removed.github_handle(),
                target_display_name: removed.display_name.as_deref(),
            },
        ))
    })
}

fn mint_person_id() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    )
}

fn validate_display_name(display_name: &str) -> Result<String, RegistryEditError> {
    let name = display_name.trim();
    if name.is_empty() {
        return Err(RegistryEditError::Parse(
            "display name is required".to_string(),
        ));
    }
    if name.chars().count() > 100 {
        return Err(RegistryEditError::Parse(
            "display name must be at most 100 characters".to_string(),
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(RegistryEditError::Parse(
            "display name cannot contain control characters".to_string(),
        ));
    }
    Ok(name.to_string())
}

pub fn apply_add_person_by_name(
    display_name: &str,
    org_root: &Path,
) -> Result<(AppliedEdit, Person), RegistryEditError> {
    let display_name = validate_display_name(display_name)?;
    let mut added = None;
    let applied = edit_people_yml(org_root, |people| {
        let mut id = mint_person_id();
        while people.people.iter().any(|person| {
            person
                .person_id()
                .is_some_and(|existing| existing.as_str() == id)
        }) {
            id = mint_person_id();
        }
        let entry = RawPersonEntry {
            id: Some(id.clone()),
            identities: Vec::new(),
            display_name: Some(display_name.clone()),
            avatar_url: None,
        };
        let person = entry
            .clone()
            .into_domain()
            .map_err(|e| RegistryEditError::Parse(e.to_string()))?
            .ok_or_else(|| RegistryEditError::Parse("new person did not compose".to_string()))?;
        people.people.push(entry);
        added = Some(person);
        Ok(pr_copy(
            &RegistryEdit::AddPersonByName {
                display_name: display_name.clone(),
            },
            &EditContext {
                old_display_name: None,
                target_username: None,
                target_display_name: Some(&display_name),
            },
        ))
    })?;
    Ok((applied, added.expect("edit closure always sets added")))
}

pub fn apply_add_identity_to_person(
    person_id: &str,
    provider: &str,
    handle: &str,
    provider_user_id: Option<&str>,
    org_root: &Path,
) -> Result<(AppliedEdit, Person), RegistryEditError> {
    let person_id = person_id.trim();
    let provider = provider.trim().to_ascii_lowercase();
    let handle = handle.trim();
    let provider_user_id = provider_user_id
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if person_id.is_empty() || provider.is_empty() || handle.is_empty() {
        return Err(RegistryEditError::Parse(
            "person, provider, and handle are required".to_string(),
        ));
    }
    for (field, value, max) in [
        ("person id", person_id, 255usize),
        ("provider", provider.as_str(), 64usize),
        ("handle", handle, 255usize),
    ] {
        if value.chars().count() > max || value.chars().any(char::is_control) {
            return Err(RegistryEditError::Parse(format!(
                "{field} must be at most {max} characters and contain no control characters"
            )));
        }
    }
    if provider_user_id
        .is_some_and(|value| value.chars().count() > 255 || value.chars().any(char::is_control))
    {
        return Err(RegistryEditError::Parse(
            "provider user id must be at most 255 characters and contain no control characters"
                .to_string(),
        ));
    }
    let mut updated = None;
    let applied = edit_people_yml(org_root, |people| {
        if people.people.iter().any(|candidate| {
            candidate
                .person_id()
                .as_ref()
                .is_some_and(|id| id.as_str() != person_id)
                && candidate.identities.iter().any(|identity| {
                    identity.provider.eq_ignore_ascii_case(&provider)
                        && (identity
                            .handle
                            .as_deref()
                            .is_some_and(|value| value.eq_ignore_ascii_case(handle))
                            || provider_user_id.is_some_and(|new_id| {
                                identity.provider_user_id.as_deref() == Some(new_id)
                            }))
                })
        }) {
            return Err(RegistryEditError::Duplicate(format!(
                "@{handle} on {provider} already belongs to another person"
            )));
        }
        let entry = people
            .people
            .iter_mut()
            .find(|entry| {
                entry
                    .person_id()
                    .as_ref()
                    .is_some_and(|id| id.as_str() == person_id)
            })
            .ok_or_else(|| RegistryEditError::NotFound(format!("no person with id {person_id}")))?;
        if let Some(identity_idx) = entry
            .identities
            .iter()
            .position(|identity| identity.provider.eq_ignore_ascii_case(&provider))
        {
            let identity = &mut entry.identities[identity_idx];
            let same_handle = identity
                .handle
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(handle));
            let conflicting_user_id = identity.provider_user_id.is_some()
                && provider_user_id.is_some()
                && identity.provider_user_id.as_deref() != provider_user_id;
            if !same_handle || conflicting_user_id {
                return Err(RegistryEditError::Duplicate(format!(
                    "person {person_id} already has a different {provider} identity"
                )));
            }
            if identity.provider_user_id.is_none() {
                identity.provider_user_id = provider_user_id.map(str::to_string);
            }
        } else {
            entry.identities.push(RawIdentity {
                provider: provider.clone(),
                handle: Some(handle.to_string()),
                provider_user_id: provider_user_id.map(str::to_string),
            });
        }
        let person = entry
            .clone()
            .into_domain()
            .map_err(|e| RegistryEditError::Parse(e.to_string()))?
            .ok_or_else(|| {
                RegistryEditError::Parse("updated person did not compose".to_string())
            })?;
        updated = Some(person);
        Ok(pr_copy(
            &RegistryEdit::AddIdentityToPerson {
                person_id: person_id.to_string(),
                provider: provider.clone(),
                handle: handle.to_string(),
                provider_user_id: provider_user_id.map(str::to_string),
            },
            &EditContext {
                old_display_name: None,
                target_username: Some(handle),
                target_display_name: None,
            },
        ))
    })?;
    Ok((applied, updated.expect("edit closure always sets updated")))
}

/// Read + parse `teams.yml` from `org_root`, mapping I/O and parse errors to
/// the shared `RegistryEditError` variants. Errors if the file is absent — the
/// one caller that tolerates a missing file (`apply_create_team`, for a
/// zero-team org) handles that case itself rather than through this helper.
pub(crate) fn read_teams_yml(org_root: &Path) -> Result<RawTeamsYml, RegistryEditError> {
    let path = org_root.join(RegistryFile::TeamsYml.filename());
    let bytes =
        std::fs::read(&path).map_err(|e| RegistryEditError::Io(format!("read teams.yml: {e}")))?;
    parse_teams_yml(&bytes).map_err(|e| RegistryEditError::Parse(format!("parse teams.yml: {e}")))
}

/// The single source of the "no such team" error. Every team-scoped edit
/// (add/remove source repo, delete, archive, rename) reports a missing target
/// through this so the message can't drift between call sites.
pub(crate) fn team_not_found(name: &str) -> RegistryEditError {
    RegistryEditError::NotFound(format!("team '{name}' not found in teams.yml"))
}

/// Find a mutable team entry by exact name, or `NotFound`. Shared by the
/// find-then-mutate apply fns (add/remove source repo, archive). `rename`/
/// `delete` need an index / a `retain`, so they call `team_not_found` directly.
fn find_team_mut<'a>(
    teams: &'a mut RawTeamsYml,
    name: &str,
) -> Result<&'a mut RawTeamEntry, RegistryEditError> {
    teams
        .teams
        .iter_mut()
        .find(|t| t.name == name)
        .ok_or_else(|| team_not_found(name))
}

/// The shared tail of every `teams.yml`-only edit (add/remove source repo,
/// delete/archive team): serialize the mutated `teams.yml` into a single-file
/// `AppliedEdit` carrying the per-variant commit/PR copy. None of these edits
/// needs the username/display-name `EditContext` fields, so they're all `None`.
fn teams_yml_applied_edit(
    teams: &RawTeamsYml,
    edit: &RegistryEdit,
) -> Result<AppliedEdit, RegistryEditError> {
    let new_bytes = serde_yaml::to_string(teams)
        .map_err(|e| RegistryEditError::Parse(format!("serialize teams.yml: {e}")))?
        .into_bytes();

    let copy = pr_copy(
        edit,
        &EditContext {
            old_display_name: None,
            target_username: None,
            target_display_name: None,
        },
    );

    Ok(AppliedEdit {
        files: vec![(PathBuf::from(RegistryFile::TeamsYml.filename()), new_bytes)],

        commit_title: copy.commit_title,
    })
}

/// Max team-name length. Slug-safe means ASCII, so byte-len == char-len.
/// Mirrors the frontend's `TEAM_NAME_MAX` (`create-team-dialog.tsx`).
pub(crate) const TEAM_NAME_MAX: usize = 40;

/// Validate a proposed team name with the SAME rules the create path uses:
/// slug-safe + reserved `-meta` suffix rejection (`validate_team_name`, shared
/// with every branch caller) plus a defense-in-depth length cap. The Tauri
/// boundary accepts arbitrary payloads, so both `CreateTeam` and `RenameTeam`
/// funnel a candidate name through here rather than re-deriving the rules.
pub(crate) fn validate_team_name_full(name: &str) -> Result<(), RegistryEditError> {
    validate_team_name(name).map_err(|e| match e {
        TeamNameError::InvalidSlug(d) => {
            RegistryEditError::Parse(format!("invalid team name: {d}"))
        }
        TeamNameError::ReservedMetaSuffix => RegistryEditError::Parse(format!(
            "team name '{name}' ends with the reserved `-meta` suffix",
        )),
        TeamNameError::ReservedLayerMarker => RegistryEditError::Parse(format!(
            "team name '{name}' is a reserved layer-marker directory name \
             (content/repos/libraries/worktrees)",
        )),
    })?;
    if name.len() > TEAM_NAME_MAX {
        return Err(RegistryEditError::Parse(format!(
            "team name must be {TEAM_NAME_MAX} characters or fewer",
        )));
    }
    Ok(())
}

// ---------- apply_add_source_repo ----------

fn apply_add_source_repo(
    team_name: &str,
    owner: &str,
    repo: &str,
    provider: Option<&str>,
    host: Option<&str>,
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<AppliedEdit, RegistryEditError> {
    // Validate the slug (catches traversal, empty segments, control chars).
    let identity =
        SourceRepoSlug::with_provider_and_host(owner, repo, provider.unwrap_or("github"), host)
            .map_err(|e| RegistryEditError::Parse(format!("invalid source repo slug: {e}")))?;

    let mut teams = read_teams_yml(org_root)?;

    let team = find_team_mut(&mut teams, team_name)?;

    // Check duplicate.
    let already_present = team
        .source_repos
        .iter()
        .filter_map(|sr| {
            SourceRepoSlug::with_provider_and_host(
                &sr.owner,
                &sr.repo,
                sr.provider_id(),
                sr.host.as_deref(),
            )
            .ok()
        })
        .any(|sr| sr.remote_key() == identity.remote_key());
    if already_present {
        return Err(RegistryEditError::Duplicate(format!(
            "{}/{} already on team {}",
            owner, repo, team_name,
        )));
    }

    team.source_repos.push(RawSourceRepoEntry {
        owner: owner.to_string(),
        repo: repo.to_string(),
        provider: identity.provider,
        host: identity.host,
    });

    teams_yml_applied_edit(&teams, edit)
}

// ---------- apply_remove_source_repo ----------

fn apply_remove_source_repo(
    team_name: &str,
    owner: &str,
    repo: &str,
    provider: Option<&str>,
    host: Option<&str>,
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<AppliedEdit, RegistryEditError> {
    // Defense-in-depth: validate the slug even though it was validated at
    // add time. The Tauri command boundary accepts arbitrary payloads.
    let identity =
        SourceRepoSlug::with_provider_and_host(owner, repo, provider.unwrap_or("github"), host)
            .map_err(|e| RegistryEditError::Parse(format!("invalid source repo slug: {e}")))?;

    let mut teams = read_teams_yml(org_root)?;

    let team = find_team_mut(&mut teams, team_name)?;

    // Check the slug is present before removing.
    let present = team
        .source_repos
        .iter()
        .filter_map(|sr| {
            SourceRepoSlug::with_provider_and_host(
                &sr.owner,
                &sr.repo,
                sr.provider_id(),
                sr.host.as_deref(),
            )
            .ok()
        })
        .any(|sr| sr.remote_key() == identity.remote_key());
    if !present {
        return Err(RegistryEditError::NotFound(format!(
            "{}/{} not tracked on team {}",
            owner, repo, team_name,
        )));
    }

    team.source_repos.retain(|sr| {
        SourceRepoSlug::with_provider_and_host(
            &sr.owner,
            &sr.repo,
            sr.provider_id(),
            sr.host.as_deref(),
        )
        .map(|sr| sr.remote_key() != identity.remote_key())
        .unwrap_or(true)
    });

    teams_yml_applied_edit(&teams, edit)
}

// ---------- apply_delete_team / apply_archive_team (SW-HB-7) ----------

/// DE-REGISTER a team (SW-HB-7 AC-3): strip its entry from `teams.yml` and
/// leave disk untouched. Content under `_team/{team}/`, `installs/…` lockfiles,
/// worktree config, and the `.meta/` mirror survive — the removed entry is a
/// revertible commit, so a delete is recoverable via git history. Errors
/// `NotFound` on a missing target (mirrors person-remove).
///
/// SLW-IN-3 (AC-3) — **reparent on delete.** Before removing the entry, every
/// child (a team whose `parent_team` names the target) is re-pointed to the
/// target's own `parent_team` — its grandparent — so no team is left inheriting
/// from a team that no longer exists. Deleting a **root** (target had no parent)
/// re-points its children to `None`, making them roots. The reparent and the
/// removal mutate the SAME in-memory `teams.yml` and land in ONE atomic
/// `AppliedEdit` (mirrors `apply_remove`'s cascade strip), so a crash can never
/// leave a child dangling. Only direct children reference the target, so a
/// single-level reparent is complete (grandchildren reference the still-present
/// children).
fn apply_delete_team(
    team_name: &str,
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<AppliedEdit, RegistryEditError> {
    let mut teams = read_teams_yml(org_root)?;

    // Capture the target's parent (the grandparent) and reject a missing target
    // up front — replaces the old retain-length check so the grandparent is
    // known before the entry is dropped.
    let grandparent = match teams.teams.iter().find(|t| t.name == team_name) {
        Some(target) => target.parent_team.clone(),
        None => return Err(team_not_found(team_name)),
    };
    // Defense-in-depth: `Org::new` rejects a self-parent at load (SLW-IN-3
    // AC-1), but this apply path edits raw `teams.yml` and cannot assume the
    // file is load-valid. If the target is its own parent, treat the grandparent
    // as `None` so children become roots rather than being re-pointed at the
    // team we are about to delete — which would leave them dangling, the very
    // outcome reparent exists to prevent.
    let grandparent = grandparent.filter(|g| g != team_name);

    // Reparent every direct child to the grandparent (`None` → child becomes a
    // root). No-op when the target has no children — untouched entries serialize
    // byte-stable.
    for team in teams.teams.iter_mut() {
        if team.parent_team.as_deref() == Some(team_name) {
            team.parent_team = grandparent.clone();
        }
    }

    // Now drop the target itself.
    teams.teams.retain(|t| t.name != team_name);

    teams_yml_applied_edit(&teams, edit)
}

/// Set (or clear) a team's `archived` flag in `teams.yml` (AC-4). Reversible:
/// `archived: false` unarchives. Only the matched entry is mutated; untouched
/// entries serialize byte-stable (the flag is `skip_serializing_if` false, so
/// active teams' bytes never churn — AC-2). Errors `NotFound` on a missing
/// target.
fn apply_archive_team(
    team_name: &str,
    archived: bool,
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<AppliedEdit, RegistryEditError> {
    let mut teams = read_teams_yml(org_root)?;

    let team = find_team_mut(&mut teams, team_name)?;
    team.archived = archived;

    teams_yml_applied_edit(&teams, edit)
}

/// Set (or clear) a team's `parent_team` in `teams.yml` (SLW-IN-5). `None`
/// clears the link (the team becomes a root). Mirrors `apply_archive_team`:
/// only the matched entry is mutated, so untouched entries serialize
/// byte-stable (`parent_team` is `skip_serializing_if` `Option::is_none`, so
/// root teams' bytes never churn). Errors `NotFound` on a missing target.
///
/// Cycle / dangling-parent validation is deliberately NOT performed here — it is
/// SLW-IN-3's authoritative load-time backstop; the IN-5 picker prevents cycles
/// at write time by excluding self + descendants.
/// Refuse a parent link the org loader would reject: the parent must be a
/// team, and neither `team_name` itself nor one of its descendants (which
/// would close a cycle).
/// Whether `org_root`'s `teams.yml` would accept `parent` as `team_name`'s
/// parent — the same check a `SetTeamParent` edit runs, for a caller that wants
/// to refuse a bad parent before writing anything else (e.g. before creating
/// `team_name`).
pub fn check_team_parent(
    org_root: &Path,
    team_name: &str,
    parent: &str,
) -> Result<(), RegistryEditError> {
    check_parent_link(&read_teams_yml(org_root)?, team_name, parent)
}

fn check_parent_link(
    teams: &RawTeamsYml,
    team_name: &str,
    parent: &str,
) -> Result<(), RegistryEditError> {
    if parent == team_name {
        return Err(RegistryEditError::Parse(format!(
            "team '{team_name}' cannot be its own parent"
        )));
    }
    let parent_of = |name: &str| {
        teams
            .teams
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.parent_team.as_deref())
    };
    if parent_of(parent).is_none() {
        return Err(RegistryEditError::NotFound(format!(
            "parent team '{parent}' not found in teams.yml"
        )));
    }
    // Walk up from the proposed parent; reaching `team_name` means the parent
    // is nested under it. The visited set stops on a loop already on disk.
    let mut seen = std::collections::HashSet::new();
    let mut cursor = Some(parent);
    while let Some(name) = cursor {
        if name == team_name {
            return Err(RegistryEditError::Parse(format!(
                "cannot nest '{team_name}' under '{parent}': '{parent}' is nested under '{team_name}'"
            )));
        }
        if !seen.insert(name) {
            break;
        }
        cursor = parent_of(name).flatten();
    }
    Ok(())
}

fn apply_set_team_parent(
    team_name: &str,
    parent_team: Option<&str>,
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<AppliedEdit, RegistryEditError> {
    let mut teams = read_teams_yml(org_root)?;

    if let Some(parent) = parent_team {
        check_parent_link(&teams, team_name, parent)?;
    }
    let team = find_team_mut(&mut teams, team_name)?;
    team.parent_team = parent_team.map(str::to_string);

    teams_yml_applied_edit(&teams, edit)
}

/// Set or clear `team_name`'s `landing_page`. A page must be content-relative
/// (no `..`, no hidden segment) and name a regular file whose real path is
/// inside the team's content folder, so the app never records a page it would
/// refuse to open.
fn apply_set_team_landing_page(
    team_name: &str,
    landing_page: Option<&str>,
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<AppliedEdit, RegistryEditError> {
    let mut teams = read_teams_yml(org_root)?;
    find_team_mut(&mut teams, team_name)?;
    let page = match landing_page.map(str::trim) {
        None => None,
        Some(page) => {
            check_landing_page(org_root, team_name, page)?;
            Some(page.to_string())
        }
    };
    let team = find_team_mut(&mut teams, team_name)?;
    team.landing_page = page;
    teams_yml_applied_edit(&teams, edit)
}

/// `page` is a file the team's starter page may be: see
/// [`apply_set_team_landing_page`].
fn check_landing_page(
    org_root: &Path,
    team_name: &str,
    page: &str,
) -> Result<(), RegistryEditError> {
    if !crate::domain::is_content_relative(page) {
        return Err(RegistryEditError::Parse(format!(
            "starter page {page:?} must be a file path inside the team's content folder: relative, without `..` and not a hidden file or folder"
        )));
    }
    // The resolver's own check: exact on-disk spelling (a case-insensitive disk
    // would otherwise accept `Guide/Start.md` for `guide/start.md`), a regular
    // file really inside the content, nothing hidden behind a symlink.
    let content = super::path_to_team_content_dir(org_root, team_name);
    if crate::commands::team_landing::declared_page_resolves(&content, page, org_root) {
        Ok(())
    } else {
        Err(RegistryEditError::NotFound(format!(
            "no file {page:?} in team '{team_name}''s content folder (the name must match its spelling on disk)"
        )))
    }
}

// ---------- apply_create_team ----------

/// Append a brand-new team to `teams.yml` (`source_repos` empty). Mirrors
/// `apply_add_source_repo`: read → parse → validate → dup-check → mutate →
/// serialize. Tolerates a missing/empty `teams.yml` so create works on a
/// zero-team org.
///
/// SW-TM-3: teams no longer carry members, so the team entry itself only names
/// the team. The creator is still recorded in `people.yml` if absent (open
/// access — `connect_to_org` does not seed the joiner), producing a multi-file
/// `AppliedEdit` (people.yml + teams.yml) committed atomically, so the org
/// roster knows who created the team.
///
/// Name is validated via `validate_team_name` — the same slug-safe rule
/// (`Team::new`) plus the reserved `-meta` suffix rejection every branch
/// caller shares — so a team name can never collide with the metadata
/// branch namespace. Uniqueness is enforced here (exact-name match)
/// because `Org::new` only catches a duplicate at whole-org load time.
fn apply_create_team(
    team_name: &str,
    creator_id: u64,
    creator_username: &str,
    creator_display_name: Option<&str>,
    creator_avatar_url: Option<&str>,
    edit: &RegistryEdit,
    org_root: &Path,
) -> Result<AppliedEdit, RegistryEditError> {
    // Validate the name (slug-safe + reserved `-meta` suffix + length cap)
    // before any read — the same rule the rename path reuses.
    validate_team_name_full(team_name)?;

    // Read the existing teams.yml; a missing file means a zero-team org
    // (post-OB-2), so start from an empty list rather than erroring.
    let teams_yml_path = org_root.join(RegistryFile::TeamsYml.filename());
    let mut teams = match std::fs::read(&teams_yml_path) {
        Ok(bytes) => parse_teams_yml(&bytes)
            .map_err(|e| RegistryEditError::Parse(format!("parse teams.yml: {e}")))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => RawTeamsYml { teams: vec![] },
        Err(e) => return Err(RegistryEditError::Io(format!("read teams.yml: {e}"))),
    };

    // Reject a duplicate name (exact match — names are lowercase-only).
    if teams.teams.iter().any(|t| t.name == team_name) {
        return Err(RegistryEditError::Duplicate(format!(
            "team '{team_name}' already exists",
        )));
    }

    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();

    // Record the creator in the org roster: append their profile to people.yml
    // if absent — idempotent, mirrors `apply_add_person`. Tolerate a missing
    // people.yml (start empty).
    let people_yml_path = org_root.join(RegistryFile::PeopleYml.filename());
    let mut people = match std::fs::read(&people_yml_path) {
        Ok(bytes) => parse_people_yml(&bytes)
            .map_err(|e| RegistryEditError::Parse(format!("parse people.yml: {e}")))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => RawPeopleYml { people: vec![] },
        Err(e) => return Err(RegistryEditError::Io(format!("read people.yml: {e}"))),
    };
    // Add-if-absent, keyed on id. If the creator is already known we leave
    // their entry untouched — create-team is not a profile-sync path, so a
    // changed username/display_name is deliberately NOT rewritten here.
    if !people.people.iter().any(|p| {
        p.github_user_id() == Some(creator_id)
            || p.display_name.as_deref() == Some(creator_display_name.unwrap_or(creator_username))
    }) {
        // SW-FG-4: append the provider-neutral identities shape (one github identity).
        let _ = creator_avatar_url;
        people.people.push(RawPersonEntry {
            id: Some(mint_person_id()),
            identities: Vec::new(),
            display_name: Some(creator_display_name.unwrap_or(creator_username).to_string()),
            avatar_url: None,
        });
        let people_bytes = serde_yaml::to_string(&people)
            .map_err(|e| RegistryEditError::Parse(format!("serialize people.yml: {e}")))?
            .into_bytes();
        files.push((
            PathBuf::from(RegistryFile::PeopleYml.filename()),
            people_bytes,
        ));
    }

    teams.teams.push(RawTeamEntry {
        name: team_name.to_string(),
        source_repos: vec![],
        archived: false,
        // SLW-IN-1: a freshly created team is a root (no parent). Re-parenting
        // is a later, explicit action (SLW-IN-5).
        parent_team: None,
        landing_page: None,
    });
    let teams_bytes = serde_yaml::to_string(&teams)
        .map_err(|e| RegistryEditError::Parse(format!("serialize teams.yml: {e}")))?
        .into_bytes();
    files.push((
        PathBuf::from(RegistryFile::TeamsYml.filename()),
        teams_bytes,
    ));

    let copy = pr_copy(
        edit,
        &EditContext {
            old_display_name: None,
            target_username: None,
            target_display_name: None,
        },
    );

    Ok(AppliedEdit {
        files,

        commit_title: copy.commit_title,
    })
}

#[cfg(test)]
mod tests {
    /// `kind()` must spell the same tag the wire `type` field carries, or a
    /// listener switching on the event's `edit` would miss the edit.
    #[test]
    fn registry_edit_kind_matches_the_wire_tag() {
        // One payload per variant: a new variant must be added to `kind()` (the
        // match is exhaustive) and here.
        for (json, team, previous) in [
            (
                r#"{"type":"RenameWorkspace","new_display_name":"A"}"#,
                None,
                None,
            ),
            (
                r#"{"type":"AddPersonByUsername","username":"u"}"#,
                None,
                None,
            ),
            (
                r#"{"type":"AddPersonByName","display_name":"U"}"#,
                None,
                None,
            ),
            (
                r#"{"type":"AddIdentityToPerson","person_id":"p","provider":"gitlab","handle":"u","provider_user_id":null}"#,
                None,
                None,
            ),
            (
                r#"{"type":"RemovePersonByUserId","github_user_id":1}"#,
                None,
                None,
            ),
            (r#"{"type":"RemovePersonById","person_id":"p"}"#, None, None),
            (
                r#"{"type":"AddSourceRepoToTeam","team_name":"pay","owner":"o","repo":"r"}"#,
                Some("pay"),
                None,
            ),
            (
                r#"{"type":"RemoveSourceRepoFromTeam","team_name":"pay","owner":"o","repo":"r"}"#,
                Some("pay"),
                None,
            ),
            (
                r#"{"type":"CreateTeam","team_name":"pay","creator_id":1,"creator_username":"u"}"#,
                Some("pay"),
                None,
            ),
            (
                r#"{"type":"DeleteTeam","team_name":"pay"}"#,
                Some("pay"),
                None,
            ),
            (
                r#"{"type":"ArchiveTeam","team_name":"pay","archived":true}"#,
                Some("pay"),
                None,
            ),
            (
                r#"{"type":"RenameTeam","old_name":"pay","new_name":"billing"}"#,
                Some("billing"),
                Some("pay"),
            ),
            (
                r#"{"type":"SetTeamParent","team_name":"pay","parent_team":null}"#,
                Some("pay"),
                None,
            ),
            (
                r#"{"type":"SetTeamLandingPage","team_name":"pay","landing_page":null}"#,
                Some("pay"),
                None,
            ),
            (
                r#"{"type":"SetWorkspaceRoot","workspace_root_dir":null}"#,
                None,
                None,
            ),
        ] {
            let tag: serde_json::Value = serde_json::from_str(json).unwrap();
            let edit: RegistryEdit = serde_json::from_str(json).unwrap();
            assert_eq!(edit.kind(), tag["type"].as_str().unwrap());
            assert_eq!(edit.team(), team);
            assert_eq!(edit.previous_team(), previous);
        }
    }
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn fixture_org_yml(dir: &Path, display_name: &str) {
        let content = format!(
            "slug: acme\ndisplay_name: {}\ncreated_at: 2026-01-01T00:00:00Z\nconfig_flags: {{}}\n",
            display_name,
        );
        fs::write(dir.join("org.yml"), content).unwrap();
    }

    // ---------- SLW-IN-7 D-1: flat-root collision safety (AC-4) ----------

    #[test]
    fn class_a_flat_root_system_entries_are_rejected_by_slug_rule() {
        // Class A (lock the existing `is_slug_safe` invariant — no new guard):
        // no slug-safe team name can collide with a flattened-root system entry.
        for reserved in ["_codesense", ".git", "org.yml", "Backend"] {
            assert!(
                validate_team_name_full(reserved).is_err(),
                "'{reserved}' must be rejected as a team name (flat-root collision)"
            );
        }
        for ok in ["default", "org-root", "backend", "team-1"] {
            assert!(
                validate_team_name_full(ok).is_ok(),
                "'{ok}' is a legal slug-safe team name"
            );
        }
    }

    #[test]
    fn class_b_layer_marker_and_worktrees_names_are_reserved() {
        // Class B guard: the layer-marker names plus the `worktrees` root sibling
        // are slug-safe (so Class A does NOT catch them) but must be rejected — a
        // marker name breaks the flat parser anchor, and `worktrees` collides
        // on-disk with the git worktrees container. Gates BOTH create and rename
        // (both funnel through `validate_team_name_full`). SLW-UD-8 retired the
        // team-level install tier, so `install` is NO LONGER a marker and is now
        // an allowed team name (asserted in the accepted list below + in
        // `ud8_install_is_an_allowed_team_name`).
        for reserved in ["content", "repos", "libraries", "worktrees"] {
            assert!(
                validate_team_name_full(reserved).is_err(),
                "reserved flat-root name '{reserved}' must be rejected (D-1 Class B)"
            );
        }
        // A plain team name that merely CONTAINS a reserved substring is fine.
        for ok in [
            "content-team",
            "installer",
            "install", // SLW-UD-8: unreserved (team-level install tier retired)
            "my-repos",
            "worktrees-2",
            "default",
        ] {
            assert!(
                validate_team_name_full(ok).is_ok(),
                "'{ok}' is not a reserved name and must be allowed"
            );
        }
    }

    /// SLW-UD-8 AC-5: with the team-level install tier retired, `install` is
    /// dropped from `LAYER_MARKER_DIRS` and is a legal team name at both create
    /// and rename (both funnel through `validate_team_name_full`). The other
    /// three markers and `worktrees` stay reserved.
    #[test]
    fn ud8_install_is_an_allowed_team_name() {
        assert!(
            validate_team_name_full("install").is_ok(),
            "'install' must be allowed after the SLW-UD-8 marker unreserve"
        );
        for still_reserved in ["content", "repos", "libraries", "worktrees"] {
            assert!(
                validate_team_name_full(still_reserved).is_err(),
                "'{still_reserved}' must remain reserved after UD-8"
            );
        }
    }

    #[test]
    fn rename_applies_and_returns_new_bytes() {
        let tmp = tempdir().unwrap();
        fixture_org_yml(tmp.path(), "Old Name");
        let edit = RegistryEdit::RenameWorkspace {
            new_display_name: "New Name".to_string(),
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();

        assert_eq!(applied.files.len(), 1);
        let (path, bytes) = &applied.files[0];
        assert_eq!(path, &PathBuf::from("org.yml"));
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("display_name: New Name"));
        assert!(text.contains("slug: acme"));
    }

    #[test]
    fn rename_rejects_empty_name() {
        let tmp = tempdir().unwrap();
        fixture_org_yml(tmp.path(), "Old Name");
        let edit = RegistryEdit::RenameWorkspace {
            new_display_name: "   ".to_string(),
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::Parse(_)));
    }

    #[test]
    fn rename_trims_whitespace_before_write() {
        let tmp = tempdir().unwrap();
        fixture_org_yml(tmp.path(), "Old Name");
        let edit = RegistryEdit::RenameWorkspace {
            new_display_name: "  Acme  ".to_string(),
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();
        let text = String::from_utf8(applied.files[0].1.clone()).unwrap();
        assert!(text.contains("display_name: Acme"));
        assert!(!text.contains("  Acme  "));
    }

    // ---------- SW-MC-2 (AC-3/AC-9): set/clear workspace_root_dir ----------

    #[test]
    fn set_workspace_root_writes_absolute_path() {
        let tmp = tempdir().unwrap();
        fixture_org_yml(tmp.path(), "Acme");
        let edit = RegistryEdit::SetWorkspaceRoot {
            workspace_root_dir: Some("/Users/dev/mycode".to_string()),
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();
        assert_eq!(applied.files.len(), 1);
        assert_eq!(applied.files[0].0, PathBuf::from("org.yml"));
        // Round-trips through the parser as the configured value.
        let raw = parse_org_yml(&applied.files[0].1).unwrap();
        assert_eq!(
            raw.config_flags.workspace_root_dir.as_deref(),
            Some(Path::new("/Users/dev/mycode"))
        );
        // Untouched fields survive the rewrite.
        assert_eq!(raw.display_name, "Acme");
    }

    #[test]
    fn set_workspace_root_clear_reverts_to_none() {
        let tmp = tempdir().unwrap();
        fixture_org_yml(tmp.path(), "Acme");
        // Set, write to disk, then clear — the clear must drop the key.
        let set = apply_registry_edit(
            &RegistryEdit::SetWorkspaceRoot {
                workspace_root_dir: Some("/Users/dev/mycode".to_string()),
            },
            tmp.path(),
        )
        .unwrap();
        std::fs::write(tmp.path().join("org.yml"), &set.files[0].1).unwrap();

        let cleared = apply_registry_edit(
            &RegistryEdit::SetWorkspaceRoot {
                workspace_root_dir: None,
            },
            tmp.path(),
        )
        .unwrap();
        let raw = parse_org_yml(&cleared.files[0].1).unwrap();
        assert_eq!(raw.config_flags.workspace_root_dir, None);
        let text = String::from_utf8(cleared.files[0].1.clone()).unwrap();
        assert!(
            !text.contains("workspace_root_dir"),
            "cleared root must not be emitted"
        );
    }

    #[test]
    fn set_workspace_root_rejects_relative_path() {
        let tmp = tempdir().unwrap();
        fixture_org_yml(tmp.path(), "Acme");
        let edit = RegistryEdit::SetWorkspaceRoot {
            workspace_root_dir: Some("relative/dir".to_string()),
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(
            matches!(&err, RegistryEditError::Parse(m) if m.contains("absolute")),
            "relative root must be rejected as non-absolute, got: {err:?}"
        );
    }

    #[test]
    fn set_workspace_root_rejects_empty_string() {
        let tmp = tempdir().unwrap();
        fixture_org_yml(tmp.path(), "Acme");
        let edit = RegistryEdit::SetWorkspaceRoot {
            workspace_root_dir: Some("   ".to_string()),
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::Parse(_)));
    }

    #[test]
    fn set_workspace_root_rejects_parent_dir_segments() {
        let tmp = tempdir().unwrap();
        fixture_org_yml(tmp.path(), "Acme");
        let edit = RegistryEdit::SetWorkspaceRoot {
            workspace_root_dir: Some("/Users/dev/../../etc/foo".to_string()),
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(
            matches!(&err, RegistryEditError::Parse(m) if m.contains("..")),
            "absolute path with `..` must be rejected, got: {err:?}"
        );
    }

    #[test]
    fn set_workspace_root_accepts_tilde_relative_and_stores_it_verbatim() {
        // The portable form: `~/code` is a valid value and is stored VERBATIM in
        // org.yml (not pre-expanded) — expansion happens per-machine at read time.
        let tmp = tempdir().unwrap();
        fixture_org_yml(tmp.path(), "Acme");
        let applied = apply_registry_edit(
            &RegistryEdit::SetWorkspaceRoot {
                workspace_root_dir: Some("~/code".to_string()),
            },
            tmp.path(),
        )
        .unwrap();
        let raw = parse_org_yml(&applied.files[0].1).unwrap();
        assert_eq!(
            raw.config_flags.workspace_root_dir.as_deref(),
            Some(Path::new("~/code")),
            "the portable ~-form must be stored verbatim, not expanded at write time"
        );
    }

    #[test]
    fn set_workspace_root_rejects_tilde_with_parent_dir() {
        let tmp = tempdir().unwrap();
        fixture_org_yml(tmp.path(), "Acme");
        let err = apply_registry_edit(
            &RegistryEdit::SetWorkspaceRoot {
                workspace_root_dir: Some("~/../etc/foo".to_string()),
            },
            tmp.path(),
        )
        .unwrap_err();
        assert!(
            matches!(&err, RegistryEditError::Parse(m) if m.contains("..")),
            "a ~-relative path with `..` must be rejected, got: {err:?}"
        );
    }

    #[test]
    fn rename_io_err_when_org_yml_missing() {
        let tmp = tempdir().unwrap();
        let edit = RegistryEdit::RenameWorkspace {
            new_display_name: "Any".to_string(),
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::Io(_)));
    }

    #[test]
    fn rename_parse_err_when_org_yml_malformed() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("org.yml"), "!!!not yaml at all: :::").unwrap();
        let edit = RegistryEdit::RenameWorkspace {
            new_display_name: "Any".to_string(),
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::Parse(_)));
    }

    #[test]
    fn pr_copy_rename_produces_diff_arrow_text() {
        let copy = pr_copy(
            &RegistryEdit::RenameWorkspace {
                new_display_name: "New".to_string(),
            },
            &EditContext {
                old_display_name: Some("Old"),
                target_username: None,
                target_display_name: None,
            },
        );
        assert_eq!(copy.commit_title, "Rename workspace: Old → New");
    }

    #[test]
    fn pr_copy_add_person_uses_username_and_display_name() {
        let copy = pr_copy(
            &RegistryEdit::AddPersonByUsername {
                username: "octocat".to_string(),
            },
            &EditContext {
                old_display_name: None,
                target_username: Some("octocat"),
                target_display_name: Some("Octo Cat"),
            },
        );
        assert_eq!(copy.commit_title, "Add @octocat");
    }

    #[test]
    fn pr_copy_remove_surfaces_username_in_title_and_id_in_body() {
        let copy = pr_copy(
            &RegistryEdit::RemovePersonByUserId {
                github_user_id: 12345,
            },
            &EditContext {
                old_display_name: None,
                target_username: Some("octocat"),
                target_display_name: None,
            },
        );
        assert_eq!(copy.commit_title, "Remove @octocat");
    }

    // ---------- apply_add_person ----------

    fn fixture_people_yml(dir: &Path, entries: &[(u64, &str, Option<&str>)]) {
        let body = entries
            .iter()
            .map(|(id, username, name)| {
                let name_line = match name {
                    Some(n) => format!("    display_name: {}\n", n),
                    None => String::new(),
                };
                format!(
                    "  - github_user_id: {}\n    github_username: {}\n{}",
                    id, username, name_line,
                )
            })
            .collect::<String>();
        let content = format!("people:\n{}", body);
        fs::write(dir.join("people.yml"), content).unwrap();
    }

    #[test]
    fn name_only_add_mints_persisted_id_and_composes() {
        let tmp = tempdir().unwrap();
        fixture_people_yml(tmp.path(), &[]);
        let (applied, person) = apply_add_person_by_name(" Grace Hopper ", tmp.path()).unwrap();
        assert_eq!(person.display_name, "Grace Hopper");
        assert!(person.github_user_id().is_none());
        let written = parse_people_yml(&applied.files[0].1).unwrap();
        assert_eq!(written.people[0].id.as_deref(), Some(person.id.as_str()));
        assert_eq!(
            written.people[0].clone().into_domain().unwrap().unwrap().id,
            person.id
        );
    }

    #[test]
    fn identity_link_updates_same_person_and_rejects_cross_person_collision() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("people.yml"),
            "people:\n  - id: p-1\n    display_name: Grace\n  - id: p-2\n    display_name: Ada\n",
        )
        .unwrap();
        let (applied, person) =
            apply_add_identity_to_person("p-1", "gitlab", "grace", None, tmp.path()).unwrap();
        assert_eq!(person.id.as_str(), "p-1");
        fs::write(tmp.path().join("people.yml"), &applied.files[0].1).unwrap();

        let (_, person_with_github) =
            apply_add_identity_to_person("p-1", "github", "grace-gh", Some("42"), tmp.path())
                .unwrap();
        assert_eq!(person_with_github.id.as_str(), "p-1");

        let err =
            apply_add_identity_to_person("p-2", "gitlab", "GRACE", None, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::Duplicate(_)));
    }

    #[test]
    fn identity_link_enforces_provider_and_provider_user_id_invariants() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("people.yml"),
            "people:\n  - id: p-1\n    display_name: Grace\n  - id: p-2\n    display_name: Ada\n    identities:\n      - provider: github\n        handle: ada\n        provider_user_id: gh-2\n",
        )
        .unwrap();
        let (applied, _) =
            apply_add_identity_to_person("p-1", "gitlab", "grace", Some("gl-1"), tmp.path())
                .unwrap();
        fs::write(tmp.path().join("people.yml"), &applied.files[0].1).unwrap();

        // An exact replay is idempotent; every conflicting mapping is rejected.
        apply_add_identity_to_person("p-1", "gitlab", "GRACE", Some("gl-1"), tmp.path()).unwrap();
        for result in [
            apply_add_identity_to_person("p-1", "gitlab", "other", Some("gl-1"), tmp.path()),
            apply_add_identity_to_person("p-1", "gitlab", "grace", Some("different"), tmp.path()),
            apply_add_identity_to_person("p-1", "github", "grace-gh", Some("gh-2"), tmp.path()),
        ] {
            assert!(matches!(result, Err(RegistryEditError::Duplicate(_))));
        }
        assert!(
            apply_add_identity_to_person("p-1", "gitlab", "bad\nhandle", None, tmp.path()).is_err()
        );
    }

    #[test]
    fn stable_id_remove_supports_people_without_github() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("people.yml"),
            "people:\n  - id: p-1\n    display_name: Grace\n",
        )
        .unwrap();
        let edit = RegistryEdit::RemovePersonById {
            person_id: "p-1".to_string(),
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();
        assert!(parse_people_yml(&applied.files[0].1)
            .unwrap()
            .people
            .is_empty());
    }

    // ---------- apply_remove ----------

    fn fixture_teams_yml(dir: &Path, team_names: &[&str]) {
        let body = team_names
            .iter()
            .map(|name| format!("  - name: {}\n    source_repos: []\n", name))
            .collect::<String>();
        let content = format!("teams:\n{}", body);
        fs::write(dir.join("teams.yml"), content).unwrap();
    }

    #[test]
    fn apply_remove_rewrites_only_people_yml() {
        // SW-TM-3: teams no longer carry members, so removing a person only
        // rewrites people.yml — teams.yml is never touched.
        let tmp = tempdir().unwrap();
        fixture_people_yml(
            tmp.path(),
            &[(1, "alice", Some("Alice")), (2, "bob", Some("Bob"))],
        );
        fixture_teams_yml(tmp.path(), &["backend"]);
        let edit = RegistryEdit::RemovePersonByUserId { github_user_id: 2 };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();
        assert_eq!(applied.files.len(), 1, "only people.yml changed");
        assert_eq!(applied.files[0].0, PathBuf::from("people.yml"));
        let text = String::from_utf8(applied.files[0].1.clone()).unwrap();
        // SW-FG-4: re-serialized in the new identities shape.
        assert!(text.contains("handle: alice"));
        assert!(!text.contains("handle: bob"));
    }

    #[test]
    fn apply_remove_errs_when_target_id_is_absent() {
        let tmp = tempdir().unwrap();
        fixture_people_yml(tmp.path(), &[(1, "alice", Some("Alice"))]);
        let edit = RegistryEdit::RemovePersonByUserId {
            github_user_id: 999,
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::NotFound(_)));
    }

    #[test]
    fn add_person_variant_through_apply_registry_edit_errs_with_hint() {
        // The public apply_registry_edit can't handle add-person (requires a
        // network fetch). The error guides callers to use apply_add_person.
        let tmp = tempdir().unwrap();
        fixture_people_yml(tmp.path(), &[]);
        let edit = RegistryEdit::AddPersonByUsername {
            username: "anyone".to_string(),
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::Parse(_)));
        assert!(err.to_string().contains("apply_add_person"));
    }

    // --- AddSourceRepoToTeam ---

    fn fixture_teams_yml_with_repos(dir: &Path, teams: &[(&str, &[(&str, &str)])]) {
        let body: String = teams
            .iter()
            .map(|(name, repos)| {
                let repos_block: String = repos
                    .iter()
                    .map(|(o, r)| format!("      - owner: {}\n        repo: {}\n", o, r))
                    .collect();
                if repos.is_empty() {
                    format!("  - name: {}\n    source_repos: []\n", name)
                } else {
                    format!("  - name: {}\n    source_repos:\n{}", name, repos_block)
                }
            })
            .collect();
        let content = format!("teams:\n{}", body);
        fs::write(dir.join("teams.yml"), content).unwrap();
    }

    #[test]
    fn add_source_repo_happy_path() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("default", &[])]);

        let edit = RegistryEdit::AddSourceRepoToTeam {
            team_name: "default".into(),
            owner: "alderco".into(),
            repo: "codesense".into(),
            provider: None,
            host: None,
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();

        assert_eq!(applied.files.len(), 1);
        let (path, bytes) = &applied.files[0];
        assert_eq!(path, &PathBuf::from("teams.yml"));
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("alderco"), "should contain owner");
        assert!(text.contains("codesense"), "should contain repo");
    }

    #[test]
    fn add_source_repo_team_not_found() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("default", &[])]);

        let edit = RegistryEdit::AddSourceRepoToTeam {
            team_name: "nonexistent".into(),
            owner: "org".into(),
            repo: "repo".into(),
            provider: None,
            host: None,
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::NotFound(_)));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn add_source_repo_duplicate_slug() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("default", &[("org", "repo")])]);

        let edit = RegistryEdit::AddSourceRepoToTeam {
            team_name: "default".into(),
            owner: "org".into(),
            repo: "repo".into(),
            provider: None,
            host: None,
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::Duplicate(_)));
        assert!(err.to_string().contains("org/repo"));
    }

    #[test]
    fn add_source_repo_duplicate_slug_case_insensitive() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("default", &[("Org", "Repo")])]);

        let edit = RegistryEdit::AddSourceRepoToTeam {
            team_name: "default".into(),
            owner: "org".into(),
            repo: "repo".into(),
            provider: None,
            host: None,
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(
            matches!(err, RegistryEditError::Duplicate(_)),
            "case-insensitive duplicate should be rejected"
        );
    }

    #[test]
    fn add_and_remove_same_slug_on_different_host_targets_exact_identity() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("default", &[("org", "repo")])]);
        let add = RegistryEdit::AddSourceRepoToTeam {
            team_name: "default".into(),
            owner: "org".into(),
            repo: "repo".into(),
            provider: Some("gitlab".into()),
            host: Some("gitlab.com".into()),
        };
        let added = apply_registry_edit(&add, tmp.path()).unwrap();
        fs::write(tmp.path().join("teams.yml"), &added.files[0].1).unwrap();
        let remove = RegistryEdit::RemoveSourceRepoFromTeam {
            team_name: "default".into(),
            owner: "org".into(),
            repo: "repo".into(),
            provider: Some("gitlab".into()),
            host: Some("gitlab.com".into()),
        };
        let removed = apply_registry_edit(&remove, tmp.path()).unwrap();
        let text = String::from_utf8(removed.files[0].1.clone()).unwrap();
        assert_eq!(text.matches("owner: org").count(), 1);
        assert!(!text.contains("provider: gitlab"));
    }

    #[test]
    fn add_source_repo_invalid_slug() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("default", &[])]);

        let edit = RegistryEdit::AddSourceRepoToTeam {
            team_name: "default".into(),
            owner: "..".into(),
            repo: "evil".into(),
            provider: None,
            host: None,
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::Parse(_)));
    }

    #[test]
    fn add_source_repo_pr_copy_output() {
        let edit = RegistryEdit::AddSourceRepoToTeam {
            team_name: "default".into(),
            owner: "org".into(),
            repo: "repo".into(),
            provider: None,
            host: None,
        };
        let copy = pr_copy(
            &edit,
            &EditContext {
                old_display_name: None,
                target_username: None,
                target_display_name: None,
            },
        );
        assert_eq!(copy.commit_title, "Track org/repo under team default");
    }

    // --- RemoveSourceRepoFromTeam ---

    #[test]
    fn remove_source_repo_happy_path() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(
            tmp.path(),
            &[("default", &[("alderco", "codesense"), ("org", "other")])],
        );

        let edit = RegistryEdit::RemoveSourceRepoFromTeam {
            team_name: "default".into(),
            owner: "alderco".into(),
            repo: "codesense".into(),
            provider: None,
            host: None,
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();

        assert_eq!(applied.files.len(), 1);
        let (path, bytes) = &applied.files[0];
        assert_eq!(path, &PathBuf::from("teams.yml"));
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(!text.contains("alderco"), "removed repo should be gone");
        assert!(text.contains("other"), "other repo should remain");
    }

    #[test]
    fn remove_source_repo_case_insensitive() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("default", &[("Alderco", "Codesense")])]);

        let edit = RegistryEdit::RemoveSourceRepoFromTeam {
            team_name: "default".into(),
            owner: "alderco".into(),
            repo: "codesense".into(),
            provider: None,
            host: None,
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();
        let text = String::from_utf8(applied.files[0].1.clone()).unwrap();
        assert!(
            !text.contains("Alderco"),
            "case-insensitive remove should work"
        );
    }

    #[test]
    fn remove_source_repo_not_found() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("default", &[("org", "repo")])]);

        let edit = RegistryEdit::RemoveSourceRepoFromTeam {
            team_name: "default".into(),
            owner: "nonexistent".into(),
            repo: "repo".into(),
            provider: None,
            host: None,
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::NotFound(_)));
        assert!(err.to_string().contains("nonexistent/repo"));
    }

    #[test]
    fn remove_source_repo_team_not_found() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("default", &[("org", "repo")])]);

        let edit = RegistryEdit::RemoveSourceRepoFromTeam {
            team_name: "nonexistent".into(),
            owner: "org".into(),
            repo: "repo".into(),
            provider: None,
            host: None,
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::NotFound(_)));
        assert!(err.to_string().contains("nonexistent"));
    }

    #[test]
    fn remove_source_repo_pr_copy_output() {
        let edit = RegistryEdit::RemoveSourceRepoFromTeam {
            team_name: "default".into(),
            owner: "org".into(),
            repo: "repo".into(),
            provider: None,
            host: None,
        };
        let copy = pr_copy(
            &edit,
            &EditContext {
                old_display_name: None,
                target_username: None,
                target_display_name: None,
            },
        );
        assert_eq!(copy.commit_title, "Untrack org/repo from team default");
    }

    // ---------- CreateTeam (SW-OB-1) ----------

    fn create_team_edit(name: &str, creator: u64) -> RegistryEdit {
        RegistryEdit::CreateTeam {
            team_name: name.into(),
            creator_id: creator,
            creator_username: format!("user{creator}"),
            creator_display_name: None,
            creator_avatar_url: None,
        }
    }

    /// Find a staged file's bytes by its org-root-relative name.
    fn staged<'a>(applied: &'a AppliedEdit, name: &str) -> Option<&'a [u8]> {
        applied
            .files
            .iter()
            .find(|(p, _)| p == &PathBuf::from(name))
            .map(|(_, b)| b.as_slice())
    }

    #[test]
    fn create_team_appends_without_dropping_existing() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(
            tmp.path(),
            &[("platform", &[("alderco", "codesense")]), ("infra", &[])],
        );
        fixture_people_yml(tmp.path(), &[(42, "user42", None)]);

        let applied = apply_registry_edit(&create_team_edit("payments", 42), tmp.path()).unwrap();

        // Creator already known → teams.yml is the only staged file.
        assert_eq!(applied.files.len(), 1);
        let bytes = staged(&applied, "teams.yml").expect("teams.yml staged");

        // Existing teams survive AND the new team is present.
        let parsed = parse_teams_yml(bytes).unwrap();
        let names: Vec<&str> = parsed.teams.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["platform", "infra", "payments"]);
        // Existing team's repos preserved (no clobber).
        let platform = parsed.teams.iter().find(|t| t.name == "platform").unwrap();
        assert_eq!(platform.source_repos.len(), 1);
    }

    #[test]
    fn create_team_adds_team_without_membership() {
        // SW-TM-3: a created team carries only a name (+ empty source_repos) —
        // no member/lead seeding.
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("platform", &[])]);
        fixture_people_yml(tmp.path(), &[(99, "user99", None)]);

        let applied = apply_registry_edit(&create_team_edit("payments", 99), tmp.path()).unwrap();
        let parsed = parse_teams_yml(staged(&applied, "teams.yml").unwrap()).unwrap();
        let team = parsed.teams.iter().find(|t| t.name == "payments").unwrap();

        assert_eq!(team.name, "payments");
        assert!(team.source_repos.is_empty());
    }

    #[test]
    fn create_team_appends_creator_to_people_when_absent() {
        // Creator joined the org and is NOT in people.yml — create-team records
        // them in the roster (people.yml) in the same commit.
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("platform", &[])]);
        fixture_people_yml(tmp.path(), &[(1, "user1", None)]); // creator 42 absent

        let edit = RegistryEdit::CreateTeam {
            team_name: "payments".into(),
            creator_id: 42,
            creator_username: "octocat".into(),
            creator_display_name: Some("The Octocat".into()),
            creator_avatar_url: None,
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();

        // Multi-file commit: people.yml + teams.yml.
        assert_eq!(applied.files.len(), 2);
        let people = parse_people_yml(staged(&applied, "people.yml").unwrap()).unwrap();
        let creator = people
            .people
            .iter()
            .find(|p| p.display_name.as_deref() == Some("The Octocat"))
            .unwrap();
        assert_eq!(creator.github_handle(), None);
        assert_eq!(creator.display_name.as_deref(), Some("The Octocat"));
        // Existing person preserved.
        assert!(people.people.iter().any(|p| p.github_user_id() == Some(1)));
    }

    #[test]
    fn create_team_on_missing_teams_yml_starts_empty() {
        // Zero-team org (post-OB-2): no teams.yml on disk yet.
        let tmp = tempdir().unwrap();
        fixture_people_yml(tmp.path(), &[(7, "user7", None)]);

        let applied = apply_registry_edit(&create_team_edit("first", 7), tmp.path()).unwrap();
        let parsed = parse_teams_yml(staged(&applied, "teams.yml").unwrap()).unwrap();

        assert_eq!(parsed.teams.len(), 1);
        assert_eq!(parsed.teams[0].name, "first");
    }

    #[test]
    fn create_team_rejects_duplicate_name() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("platform", &[])]);

        let err = apply_registry_edit(&create_team_edit("platform", 1), tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::Duplicate(_)));
        assert!(err.to_string().contains("platform"));
    }

    #[test]
    fn create_team_rejects_invalid_and_uppercase_names() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("platform", &[])]);

        for bad in ["", "Payments", "has space", "-leading"] {
            let err = apply_registry_edit(&create_team_edit(bad, 1), tmp.path()).unwrap_err();
            assert!(
                matches!(err, RegistryEditError::Parse(_)),
                "'{bad}' should be a Parse error",
            );
        }
    }

    #[test]
    fn create_team_rejects_reserved_meta_suffix() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("platform", &[])]);

        let err =
            apply_registry_edit(&create_team_edit("payments-meta", 1), tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::Parse(_)));
        assert!(err.to_string().contains("meta"));
    }

    #[test]
    fn create_team_rejects_over_long_name() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("platform", &[])]);
        fixture_people_yml(tmp.path(), &[(1, "user1", None)]);

        let long = "a".repeat(41);
        let err = apply_registry_edit(&create_team_edit(&long, 1), tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::Parse(_)));
        assert!(err.to_string().contains("40 characters"));
    }

    #[test]
    fn create_team_pr_copy_output() {
        let copy = pr_copy(
            &create_team_edit("payments", 1),
            &EditContext {
                old_display_name: None,
                target_username: None,
                target_display_name: None,
            },
        );
        assert_eq!(copy.commit_title, "Create team payments");
    }

    // ---------- SW-HB-7: DeleteTeam / ArchiveTeam ----------

    /// AC-1/AC-3: delete strips EXACTLY the target entry, produces a single
    /// `teams.yml` write with `branch_kind: Remove`, and never stages any other
    /// file (de-register only — content, installs, worktree, `.meta` untouched).
    #[test]
    fn delete_team_strips_only_target_and_is_teams_yml_only() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["backend", "frontend"]);
        let edit = RegistryEdit::DeleteTeam {
            team_name: "backend".into(),
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();

        assert_eq!(applied.files.len(), 1, "de-register touches only teams.yml");
        assert_eq!(applied.files[0].0, PathBuf::from("teams.yml"));

        let text = String::from_utf8(applied.files[0].1.clone()).unwrap();
        assert!(!text.contains("name: backend"), "target gone");
        assert!(text.contains("name: frontend"), "sibling retained");
    }

    /// AC-3: apply is pure — deleting a team leaves any `_team/{team}/content/`
    /// on disk (apply never writes/deletes disk paths; only the teams.yml bytes
    /// change). Seeds a content dir and asserts it survives the apply call.
    #[test]
    fn delete_team_leaves_content_dir_on_disk() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["backend"]);
        let epic_dir = tmp.path().join("backend/content/epic-1");
        fs::create_dir_all(&epic_dir).unwrap();

        let edit = RegistryEdit::DeleteTeam {
            team_name: "backend".into(),
        };
        apply_registry_edit(&edit, tmp.path()).unwrap();

        assert!(epic_dir.exists(), "authored content survives de-register");
    }

    /// AC-6: delete of a non-existent team errors `NotFound` (mirrors
    /// person-remove), surfaced inline.
    #[test]
    fn delete_team_errs_not_found_for_missing_target() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["backend"]);
        let edit = RegistryEdit::DeleteTeam {
            team_name: "ghost".into(),
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::NotFound(_)));
    }

    /// SLW-IN-3 helper: write a `teams.yml` where each entry may carry a
    /// `parent_team`. Kept local to the reparent tests — the shared
    /// `fixture_teams_yml` intentionally stays parent-free.
    fn fixture_teams_yml_with_parents(dir: &Path, teams: &[(&str, Option<&str>)]) {
        let body = teams
            .iter()
            .map(|(name, parent)| {
                let parent_line = match parent {
                    Some(p) => format!("    parent_team: {}\n", p),
                    None => String::new(),
                };
                format!("  - name: {}\n    source_repos: []\n{}", name, parent_line,)
            })
            .collect::<String>();
        fs::write(dir.join("teams.yml"), format!("teams:\n{}", body)).unwrap();
    }

    /// SLW-IN-3 AC-3: deleting a mid-chain team re-points its child to the
    /// grandparent. In root ← mid ← leaf, deleting `mid` gives `leaf` parent
    /// `root` — no child left dangling.
    #[test]
    fn delete_team_reparents_child_to_grandparent() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_parents(
            tmp.path(),
            &[("root", None), ("mid", Some("root")), ("leaf", Some("mid"))],
        );
        let edit = RegistryEdit::DeleteTeam {
            team_name: "mid".into(),
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();

        let reparsed = parse_teams_yml(&applied.files[0].1).unwrap();
        assert!(
            reparsed.teams.iter().all(|t| t.name != "mid"),
            "target removed"
        );
        let leaf = reparsed.teams.iter().find(|t| t.name == "leaf").unwrap();
        assert_eq!(
            leaf.parent_team.as_deref(),
            Some("root"),
            "child reparented to grandparent"
        );
    }

    /// SLW-IN-3 AC-3: deleting a ROOT team (no parent) makes each child a root
    /// (`parent_team: None`) rather than leaving it dangling.
    #[test]
    fn delete_root_team_makes_children_roots() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_parents(tmp.path(), &[("root", None), ("child", Some("root"))]);
        let edit = RegistryEdit::DeleteTeam {
            team_name: "root".into(),
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();

        let reparsed = parse_teams_yml(&applied.files[0].1).unwrap();
        let child = reparsed.teams.iter().find(|t| t.name == "child").unwrap();
        assert_eq!(
            child.parent_team, None,
            "child of deleted root becomes a root"
        );
    }

    /// SLW-IN-3 AC-3: deleting a CHILDLESS team touches no other entry's
    /// `parent_team` — siblings/parents remain byte-stable (no reparent churn).
    #[test]
    fn delete_childless_team_leaves_other_parents_untouched() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_parents(
            tmp.path(),
            &[("root", None), ("mid", Some("root")), ("leaf", Some("mid"))],
        );
        // Delete the leaf (nothing inherits from it).
        let edit = RegistryEdit::DeleteTeam {
            team_name: "leaf".into(),
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();

        let reparsed = parse_teams_yml(&applied.files[0].1).unwrap();
        let mid = reparsed.teams.iter().find(|t| t.name == "mid").unwrap();
        assert_eq!(
            mid.parent_team.as_deref(),
            Some("root"),
            "unrelated parent pointer unchanged"
        );
        assert!(reparsed.teams.iter().all(|t| t.name != "leaf"));
    }

    /// SLW-IN-3 AC-3: reparent-on-delete produces a single atomic teams.yml
    /// write (grandparent re-point + removal in one AppliedEdit) — never a
    /// second commit that could crash between and orphan a child.
    #[test]
    fn delete_team_reparent_is_single_atomic_teams_yml_write() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_parents(
            tmp.path(),
            &[("root", None), ("mid", Some("root")), ("leaf", Some("mid"))],
        );
        let edit = RegistryEdit::DeleteTeam {
            team_name: "mid".into(),
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();
        assert_eq!(applied.files.len(), 1, "reparent + delete = one file");
        assert_eq!(applied.files[0].0, PathBuf::from("teams.yml"));

        // The single write actually CARRIES the reparent — asserting the shape
        // alone would pass even if reparent were skipped (teams_yml edits are
        // always one file), so pin the reparented content in the same bytes.
        let reparsed = parse_teams_yml(&applied.files[0].1).unwrap();
        let leaf = reparsed.teams.iter().find(|t| t.name == "leaf").unwrap();
        assert_eq!(
            leaf.parent_team.as_deref(),
            Some("root"),
            "reparent is fused into the single delete write"
        );
    }

    /// SLW-IN-3 defense-in-depth: deleting a self-parented target (a malformed
    /// on-disk chain `Org::new` would reject at load, but this apply path edits
    /// raw teams.yml) makes its children roots — NOT dangling on the deleted
    /// team. Guards the "never orphan" principle even on a broken file.
    #[test]
    fn delete_self_parented_team_makes_children_roots() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_parents(tmp.path(), &[("mid", Some("mid")), ("leaf", Some("mid"))]);
        let edit = RegistryEdit::DeleteTeam {
            team_name: "mid".into(),
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();

        let reparsed = parse_teams_yml(&applied.files[0].1).unwrap();
        assert!(
            reparsed.teams.iter().all(|t| t.name != "mid"),
            "target gone"
        );
        let leaf = reparsed.teams.iter().find(|t| t.name == "leaf").unwrap();
        assert_eq!(
            leaf.parent_team, None,
            "child of a self-parented deleted team becomes a root, not dangling"
        );
    }

    /// AC-4: archive sets the flag on the matched entry (round-trips through
    /// parse), with `branch_kind: Rename`. AC-2: an untouched sibling stays
    /// byte-stable — no `archived` key is emitted for the active team.
    #[test]
    fn archive_team_sets_flag_and_leaves_sibling_byte_stable() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["backend", "frontend"]);
        let edit = RegistryEdit::ArchiveTeam {
            team_name: "backend".into(),
            archived: true,
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();

        let text = String::from_utf8(applied.files[0].1.clone()).unwrap();

        // Exactly ONE archived key emitted — the sibling active team gains none
        // (skip_serializing_if keeps active YAML byte-stable, AC-2).
        assert_eq!(
            text.matches("archived:").count(),
            1,
            "only the archived team carries the key"
        );

        // Round-trips: re-parsing yields backend archived, frontend active.
        let reparsed = parse_teams_yml(text.as_bytes()).unwrap();
        let backend = reparsed.teams.iter().find(|t| t.name == "backend").unwrap();
        let frontend = reparsed
            .teams
            .iter()
            .find(|t| t.name == "frontend")
            .unwrap();
        assert!(backend.archived);
        assert!(!frontend.archived);
    }

    /// AC-4: unarchive (`archived: false`) clears the flag and — because the
    /// field is `skip_serializing_if` false — leaves NO `archived` key behind,
    /// so the entry returns to byte-stable active form.
    #[test]
    fn unarchive_team_clears_flag_and_key() {
        let tmp = tempdir().unwrap();
        // Seed an already-archived team.
        fs::write(
            tmp.path().join("teams.yml"),
            "teams:\n  - name: backend\n    source_repos: []\n    archived: true\n",
        )
        .unwrap();
        let edit = RegistryEdit::ArchiveTeam {
            team_name: "backend".into(),
            archived: false,
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();
        let text = String::from_utf8(applied.files[0].1.clone()).unwrap();
        assert!(
            !text.contains("archived"),
            "unarchive removes the key entirely"
        );
    }

    /// SLW-IN-1 AC-2b: the in-place `find_team_mut` mutations (here: archive)
    /// carry an unrelated `parent_team` link through untouched — a team's parent
    /// survives an archive with the key intact.
    #[test]
    fn archive_preserves_parent_team_link() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("teams.yml"),
            "teams:\n  - name: backend\n    source_repos: []\n    parent_team: platform\n",
        )
        .unwrap();
        let edit = RegistryEdit::ArchiveTeam {
            team_name: "backend".into(),
            archived: true,
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();
        let text = String::from_utf8(applied.files[0].1.clone()).unwrap();
        let reparsed = parse_teams_yml(text.as_bytes()).unwrap();
        let backend = reparsed.teams.iter().find(|t| t.name == "backend").unwrap();
        assert!(backend.archived, "archive flag applied");
        assert_eq!(
            backend.parent_team,
            Some("platform".to_string()),
            "parent_team survives the in-place archive mutation"
        );
    }

    /// SLW-IN-1 AC-2a: a freshly created team is a root — the CreateTeam struct
    /// literal seeds `parent_team: None`, so no key is emitted (byte-stable).
    #[test]
    fn create_team_is_parentless_and_byte_stable() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml_with_repos(tmp.path(), &[("platform", &[])]);
        fixture_people_yml(tmp.path(), &[(42, "user42", None)]);

        let applied = apply_registry_edit(&create_team_edit("payments", 42), tmp.path()).unwrap();
        let bytes = staged(&applied, "teams.yml").expect("teams.yml staged");
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(
            !text.contains("parent_team"),
            "new team must not emit a parent_team key:\n{text}"
        );
    }

    /// AC-6: archive of a non-existent team errors `NotFound`.
    #[test]
    fn archive_team_errs_not_found_for_missing_target() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["backend"]);
        let edit = RegistryEdit::ArchiveTeam {
            team_name: "ghost".into(),
            archived: true,
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::NotFound(_)));
    }

    /// AC-2: a pre-field `teams.yml` (no `archived` key) parses with every team
    /// active, and reserializes with no `archived` key introduced.
    #[test]
    fn prefield_teams_yml_parses_all_active_and_bytes_stable() {
        let yaml = "teams:\n  - name: backend\n    source_repos: []\n";
        let parsed = parse_teams_yml(yaml.as_bytes()).unwrap();
        assert!(parsed.teams.iter().all(|t| !t.archived), "none archived");
        let reserialized = serde_yaml::to_string(&parsed).unwrap();
        assert!(
            !reserialized.contains("archived"),
            "no archived key churns into active teams"
        );
    }

    /// AC-1: pr_copy arms for the new variants produce the expected commit copy.
    #[test]
    fn pr_copy_for_delete_and_archive() {
        let ctx = EditContext {
            old_display_name: None,
            target_username: None,
            target_display_name: None,
        };
        let del = pr_copy(
            &RegistryEdit::DeleteTeam {
                team_name: "payments".into(),
            },
            &ctx,
        );
        assert_eq!(del.commit_title, "Delete team payments");

        let arch = pr_copy(
            &RegistryEdit::ArchiveTeam {
                team_name: "payments".into(),
                archived: true,
            },
            &ctx,
        );
        assert_eq!(arch.commit_title, "Archive team payments");

        let unarch = pr_copy(
            &RegistryEdit::ArchiveTeam {
                team_name: "payments".into(),
                archived: false,
            },
            &ctx,
        );
        assert_eq!(unarch.commit_title, "Unarchive team payments");
    }

    /// SLW-IN-5: set a team's parent — the flag round-trips through parse with
    /// `branch_kind: Rename`, and an untouched sibling stays byte-stable (no
    /// `parent_team` key emitted for the root sibling).
    #[test]
    fn set_team_parent_sets_link_and_leaves_sibling_byte_stable() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["child", "parent"]);
        let edit = RegistryEdit::SetTeamParent {
            team_name: "child".into(),
            parent_team: Some("parent".into()),
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();

        let text = String::from_utf8(applied.files[0].1.clone()).unwrap();

        // Exactly ONE parent_team key — the root sibling gains none.
        assert_eq!(
            text.matches("parent_team:").count(),
            1,
            "only the reparented team carries the key"
        );
        let reparsed = parse_teams_yml(text.as_bytes()).unwrap();
        let child = reparsed.teams.iter().find(|t| t.name == "child").unwrap();
        let parent = reparsed.teams.iter().find(|t| t.name == "parent").unwrap();
        assert_eq!(child.parent_team, Some("parent".to_string()));
        assert_eq!(parent.parent_team, None);
    }

    /// SLW-IN-5: clearing (`parent_team: None`) removes the key entirely, so the
    /// entry returns to byte-stable root form.
    #[test]
    fn set_team_parent_none_clears_key() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("teams.yml"),
            "teams:\n  - name: child\n    source_repos: []\n    parent_team: parent\n",
        )
        .unwrap();
        let edit = RegistryEdit::SetTeamParent {
            team_name: "child".into(),
            parent_team: None,
        };
        let applied = apply_registry_edit(&edit, tmp.path()).unwrap();
        let text = String::from_utf8(applied.files[0].1.clone()).unwrap();
        assert!(
            !text.contains("parent_team"),
            "cleared parent must emit no key:\n{text}"
        );
    }

    /// SLW-IN-5: set-parent of a non-existent team errors `NotFound`.
    #[test]
    fn set_team_parent_errs_not_found_for_missing_target() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["child"]);
        let edit = RegistryEdit::SetTeamParent {
            team_name: "ghost".into(),
            parent_team: Some("child".into()),
        };
        let err = apply_registry_edit(&edit, tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::NotFound(_)));
    }

    fn set_parent(
        org_root: &Path,
        team: &str,
        parent: &str,
    ) -> Result<AppliedEdit, RegistryEditError> {
        apply_registry_edit(
            &RegistryEdit::SetTeamParent {
                team_name: team.into(),
                parent_team: Some(parent.into()),
            },
            org_root,
        )
    }

    /// A parent that is not a team would leave `teams.yml` unloadable.
    #[test]
    fn set_team_parent_rejects_an_unknown_parent() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["child"]);
        let err = set_parent(tmp.path(), "child", "ghost").unwrap_err();
        assert!(matches!(err, RegistryEditError::NotFound(_)), "{err}");
        assert!(err.to_string().contains("parent team 'ghost'"), "{err}");
    }

    #[test]
    fn set_team_parent_rejects_the_team_itself() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["child"]);
        let err = set_parent(tmp.path(), "child", "child").unwrap_err();
        assert!(
            err.to_string().contains("cannot be its own parent"),
            "{err}"
        );
    }

    /// Nesting a team under its own child, or deeper descendant, makes a cycle.
    #[test]
    fn set_team_parent_rejects_a_descendant() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("teams.yml"),
            "teams:\n  - name: a\n    source_repos: []\n  - name: b\n    source_repos: []\n    parent_team: a\n  - name: c\n    source_repos: []\n    parent_team: b\n",
        )
        .unwrap();
        for descendant in ["b", "c"] {
            let err = set_parent(tmp.path(), "a", descendant).unwrap_err();
            assert!(
                err.to_string()
                    .contains(&format!("'{descendant}' is nested under 'a'")),
                "{descendant}: {err}"
            );
        }
        // A non-descendant is still accepted.
        assert!(set_parent(tmp.path(), "c", "a").is_ok());
    }

    /// SLW-IN-5: pr_copy for set vs clear.
    #[test]
    fn pr_copy_for_set_team_parent() {
        let ctx = EditContext {
            old_display_name: None,
            target_username: None,
            target_display_name: None,
        };
        let set = pr_copy(
            &RegistryEdit::SetTeamParent {
                team_name: "child".into(),
                parent_team: Some("parent".into()),
            },
            &ctx,
        );
        assert_eq!(set.commit_title, "Set team child to inherit from parent");

        let clear = pr_copy(
            &RegistryEdit::SetTeamParent {
                team_name: "child".into(),
                parent_team: None,
            },
            &ctx,
        );
        assert_eq!(clear.commit_title, "Clear parent team for child");
    }

    // ---------- SetTeamLandingPage: the team's starter page ----------

    fn landing(team: &str, page: Option<&str>) -> RegistryEdit {
        RegistryEdit::SetTeamLandingPage {
            team_name: team.into(),
            landing_page: page.map(str::to_string),
        }
    }

    fn content_file(org_root: &Path, team: &str, rel: &str) {
        let path = org_root.join(team).join("content").join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
    }

    #[test]
    fn set_team_landing_page_sets_the_key_and_leaves_siblings_byte_stable() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["docs", "web"]);
        content_file(tmp.path(), "docs", "guide/start.md");
        let applied =
            apply_registry_edit(&landing("docs", Some(" guide/start.md ")), tmp.path()).unwrap();
        let text = String::from_utf8(applied.files[0].1.clone()).unwrap();
        assert_eq!(text.matches("landing_page:").count(), 1, "{text}");
        let parsed = parse_teams_yml(text.as_bytes()).unwrap();
        assert_eq!(
            parsed.teams[0].landing_page.as_deref(),
            Some("guide/start.md")
        );
        assert_eq!(parsed.teams[1].landing_page, None);
        assert_eq!(
            applied.commit_title,
            "Set the starter page of team docs to guide/start.md"
        );
    }

    #[test]
    fn clearing_the_landing_page_removes_the_key() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("teams.yml"),
            "teams:\n  - name: docs\n    source_repos: []\n    landing_page: start.md\n",
        )
        .unwrap();
        let applied = apply_registry_edit(&landing("docs", None), tmp.path()).unwrap();
        let text = String::from_utf8(applied.files[0].1.clone()).unwrap();
        assert!(!text.contains("landing_page"), "{text}");
        assert_eq!(applied.commit_title, "Clear the starter page of team docs");
    }

    #[test]
    fn a_landing_page_outside_the_content_or_hidden_is_refused() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["docs"]);
        content_file(tmp.path(), "docs", ".env");
        content_file(tmp.path(), "docs", ".claude/notes.md");
        fs::write(tmp.path().join("org.yml"), "o").unwrap();
        for bad in [
            "../../org.yml",
            "/etc/hosts",
            ".env",
            ".claude/notes.md",
            "a/./b.md",
            "",
        ] {
            let err = apply_registry_edit(&landing("docs", Some(bad)), tmp.path()).unwrap_err();
            assert!(matches!(err, RegistryEditError::Parse(_)), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_missing_page_or_a_folder_is_not_found() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["docs"]);
        content_file(tmp.path(), "docs", "guide/start.md");
        for missing in ["nope.md", "guide"] {
            let err = apply_registry_edit(&landing("docs", Some(missing)), tmp.path()).unwrap_err();
            assert!(err.to_string().contains("no file"), "{missing}: {err}");
        }
        let err = apply_registry_edit(&landing("ghost", None), tmp.path()).unwrap_err();
        assert!(matches!(err, RegistryEditError::NotFound(_)), "{err}");
        let err =
            apply_registry_edit(&landing("ghost", Some("guide/start.md")), tmp.path()).unwrap_err();
        assert_eq!(
            err.to_string(),
            "not_found: team 'ghost' not found in teams.yml"
        );
    }

    #[test]
    fn a_landing_page_in_the_wrong_letter_case_is_refused() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["docs"]);
        content_file(tmp.path(), "docs", "guide/start.md");
        let err =
            apply_registry_edit(&landing("docs", Some("Guide/Start.md")), tmp.path()).unwrap_err();
        assert!(err.to_string().contains("no file"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_visible_symlink_to_a_hidden_file_is_refused() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["docs"]);
        content_file(tmp.path(), "docs", ".env");
        std::os::unix::fs::symlink(".env", tmp.path().join("docs/content/notes.md")).unwrap();
        let err = apply_registry_edit(&landing("docs", Some("notes.md")), tmp.path()).unwrap_err();
        assert!(err.to_string().contains("no file"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_landing_page_symlinked_out_of_the_content_is_refused() {
        let tmp = tempdir().unwrap();
        fixture_teams_yml(tmp.path(), &["docs"]);
        content_file(tmp.path(), "docs", "README.md");
        let secret = tmp.path().join("secret.txt");
        fs::write(&secret, "s").unwrap();
        std::os::unix::fs::symlink(&secret, tmp.path().join("docs/content/link.md")).unwrap();
        let err = apply_registry_edit(&landing("docs", Some("link.md")), tmp.path()).unwrap_err();
        assert!(err.to_string().contains("no file"), "{err}");
    }
}
