// SPDX-License-Identifier: MIT
//! Git primitives for Epic 31's context repo surface.
//!
//! This module holds every B-layer helper that operates on context repo
//! storage: URL normalization, scaffold helpers, worktree management,
//! remote reads, etc. It's deliberately a library module (no Tauri
//! commands, no AppHandle) so primitives can be unit-tested in isolation.
//! Downstream C-layer stories add thin Tauri command wrappers when they
//! need to expose these primitives to the frontend.

pub mod attachments_io;
pub(crate) mod branch_common;
pub(crate) mod caller_scope;
pub mod codesense_docs;
mod content;
pub mod context_attachments;
pub(crate) mod inheritance;
#[cfg(test)]
mod inheritance_guard;
pub mod layout_resolver;
pub mod migrate_install_tree;
mod mutate;
pub mod org_bindings;
pub(crate) mod org_enumeration;
pub mod org_slug;
mod path;
pub mod pending_org_creates;
mod remove_worktree;
pub mod sample_source_repo;
mod scaffold;
pub mod scoped_key;
pub mod scratch_space;
mod team_rename;
mod url;
mod workdir_write;
mod worktree;

#[cfg(test)]
pub(crate) mod test_fixtures;

// Re-exports are intentionally unused at merge time — downstream
// consumers live in stories C.2 (first-launch bootstrap), C.3
// (connect-to-org), C.4 (create-new-org flow), and later B stories.
// Suppress the unused-imports warning on the public surface so the
// module can ship standalone.
#[allow(unused_imports)]
pub use branch_common::with_branch_lock;
#[allow(unused_imports)]
pub use content::{
    bootstrap_workspace_content, seed_repo_content, BootstrapError, BootstrapResult, SeedError,
    SeedOutcome,
};
#[allow(unused_imports)]
pub use mutate::{
    apply_add_identity_to_person, apply_add_person_by_name, apply_registry_edit, check_team_parent,
    pr_copy, AppliedEdit, EditContext, PrCopy, RegistryEdit, RegistryEditError,
};
#[allow(unused_imports)]
pub use path::{
    classify_epic_worktree_path, classify_epic_worktree_path_with_branch, default_skeleton_dirs,
    derive_org_root, effective_managed_root_dir, org_slug_from_org_root, override_wins_key,
    path_to_branch_workspace, path_to_epic_workspace, path_to_install_root,
    path_to_integration_worktree, path_to_managed_org_root, path_to_managed_source_clone,
    path_to_org_root, path_to_personal_libraries_dir, path_to_registry_file,
    path_to_repo_content_dir, path_to_repo_install_dir, path_to_team_config_dir,
    path_to_team_content_dir, path_to_team_install_dir, path_to_team_libraries_dir,
    path_to_team_notes_dir, path_to_team_panes_dir, path_to_team_repo_dir,
    path_to_user_attachments_file, path_to_user_home, path_to_user_org_bindings_file,
    path_to_user_pending_org_creates_file, path_to_worker_workspace, path_to_worktree_config,
    read_from_origin_head, resolve_default_branch, resolve_managed_clone_dir,
    team_config_dir_beside_content, team_panes_dir_beside_content, try_path_to_repo_content_dir,
    try_path_to_repo_install_dir, try_path_to_worktree_config, DefaultBranchError,
    EpicWorktreeCoords, ReadError, ATTACHMENTS_FILE, BMAD_OUTPUT_DIR, CLAUDE_CONFIG_DIR,
    CODESENSE_INTERNAL_DIR, CONFIG_DIR, EPIC_DIR, INSTALLS_DIR, INSTALL_DIR, LAYER_MARKER_DIRS,
    NOTES_DIR, ORGS_DIR, ORG_BINDINGS_FILE, PANES_DIR, PENDING_ORG_CREATES_FILE, PERSONAL_DIR,
    REPOS_DIR, SOURCES_DIR, TEAM_CONTENT_DIR, USERS_DIR, WORKER_DIR, WORKTREES_DIR,
};
#[allow(unused_imports)]
pub use remove_worktree::{
    remove_integration_worktree, RemoveWorktreeError, RemoveWorktreeOutcome,
};
#[allow(unused_imports)]
pub use scaffold::{scaffold_org_context_repo, ScaffoldError, ScaffoldOrgInputs, ScaffoldOutcome};
#[allow(unused_imports)]
pub use team_rename::{rename_team, RenameTeamOutcome};
#[allow(unused_imports)]
pub use url::{canonical_remote_key, normalize_source_repo_url, NormalizedRepoUrl, RepoUrlError};
#[allow(unused_imports)]
pub use workdir_write::{write_edit_to_workdir, write_edit_to_workdir_locked};
#[allow(unused_imports)]
pub use worktree::{
    add_integration_worktree, AddWorktreeError, AddWorktreeOutcome, IntegrationWorktreeResult,
};
