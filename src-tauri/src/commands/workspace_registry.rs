// SPDX-License-Identifier: MIT
//! SW-WC-1 — the per-workspace archive flag (shrunk-C from the workspace-chat
//! design review's OQ-4).
//!
//! A workspace is "archived" when the operator explicitly marks it so — an
//! intent, not a derivation of worktree reap/merge. In v1 the only thing the
//! flag freezes is workspace chat (SW-WC-2 reads it); terminals, context, and
//! file ops stay writable. v2 may expand the scope.
//!
//! Design (per `_bmad-output/21-workspace-chat/stories/SW-WC-1-workspace-archive.md`):
//!
//!   * One nullable timestamp field, `archived_at_ms: Option<i64>`, keyed by
//!     the viewer workspace id string (`root:<path>` / `story:<id>` /
//!     `generic:<path>` — the same form every other Tauri surface uses, see
//!     `OpenTerminalTabParams.workspace_id` and `pty_layout::workspace_key`).
//!   * Lives at `~/.codesense/workspace-registry.json`, atomic write via the
//!     shared `tmp-then-rename` helper (same pattern as the launch registry
//!     at `~/.codesense/users/{principal}/launch-registry.json`). This is the
//!     smallest existing-pattern home; the viewer state is session-only and
//!     the sync-state store is the wrong domain. A tiny registry here keeps
//!     the archive flag durable and easy to inspect by eye.
//!   * Four Tauri commands wire it to the UI:
//!     * `archive_workspace { workspaceId }` → stamps `archived_at_ms`,
//!     * `unarchive_workspace { workspaceId }` → clears it,
//!     * `get_workspace_archive_state { workspaceId }` → one entry,
//!     * `list_workspace_archive_states` → every entry on disk.
//!   * Mutations emit `codesense:workspace-archive-changed` with
//!     `{ workspaceId, archivedAtMs }` so a dashboard / viewer can react
//!     without polling.
//!
//! What this module **does not** touch (per the story's non-goals):
//!
//!   * No lifecycle state machine — just one nullable timestamp.
//!   * No auto-archive — the operator is the only writer.
//!   * No archive-only chat query (SW-WC-2 owns that).
//!   * No worktree reap/merge logic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::shared::json_io::{read_json, write_json_atomic, JsonIoError};

/// The event the FE listens for to re-render archive badges / lock the chat
/// compose box without re-fetching the whole list.
pub(crate) const ARCHIVE_CHANGED_EVENT: &str = "codesense:workspace-archive-changed";

/// Current on-disk schema version. Bump when the shape changes in a way an
/// older reader cannot ignore; the loader treats an unknown version as "no
/// record" rather than fail boot (same discipline as the launch registry).
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// The registry file's basename under `~/.codesense/`.
pub(crate) const REGISTRY_FILENAME: &str = "workspace-registry.json";

/// Hard cap on a workspace id's length. The viewer id form is already short
/// (`root:<path>` / `story:<id>` / `generic:<path>`); 256 bytes is the same
/// cap `WorkspaceKey::new` enforces on the pty-layout key and matches the
/// workspace-chat design doc's §1 note.
const WORKSPACE_ID_MAX_LEN: usize = 256;

/// One workspace's persisted archive flag. `archived_at_ms = None` is the
/// active state; the entry exists in the map iff a toggle has happened at
/// least once (an unarchive leaves `None` behind so the FE can tell "known,
/// explicitly active" from "no history").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceArchiveEntry {
    /// Mirror of the map key, so a line-by-eye reader never has to cross-
    /// reference by position. Matches the `launch-registry.json` discipline.
    pub workspace_id: String,
    /// Epoch-millisecond timestamp at archive. `None` after unarchive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at_ms: Option<i64>,
}

/// The on-disk document. Keyed by workspace id — same string form the viewer
/// and `OpenTerminalTabParams` use.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistedRegistry {
    pub version: u32,
    pub workspaces: HashMap<String, WorkspaceArchiveEntry>,
}

impl Default for PersistedRegistry {
    fn default() -> Self {
        PersistedRegistry {
            version: SCHEMA_VERSION,
            workspaces: HashMap::new(),
        }
    }
}

/// The absolute path to the workspace registry under `codesense_home`. Pure
/// path math; the caller handles a signed-out / headless `None` home.
pub(crate) fn registry_path(codesense_home: &Path) -> PathBuf {
    codesense_home.join(REGISTRY_FILENAME)
}

/// Load the registry from disk. A missing file is the fresh-install case and
/// returns the default silently. A corrupt or wrong-version file is logged
/// and treated as empty — the on-disk companion must never fail app boot
/// (matches `dashboard_launch::persistence::load_registry`).
pub(crate) fn load_registry(path: &Path) -> PersistedRegistry {
    match read_json::<PersistedRegistry>(path) {
        Ok(registry) if registry.version == SCHEMA_VERSION => registry,
        Ok(other) => {
            log::warn!(
                target: "workspace_registry",
                "workspace registry at {} has unsupported version {} (expected {}); ignoring",
                path.display(),
                other.version,
                SCHEMA_VERSION,
            );
            PersistedRegistry::default()
        }
        Err(JsonIoError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            PersistedRegistry::default()
        }
        Err(error) => {
            log::warn!(
                target: "workspace_registry",
                "workspace registry at {} could not be read ({error}); starting empty",
                path.display(),
            );
            PersistedRegistry::default()
        }
    }
}

/// Write `registry` atomically. The caller owns whatever write lock serializes
/// callers; this is a pure IO choke that never partially writes.
pub(crate) fn write_registry(path: &Path, registry: &PersistedRegistry) -> Result<(), JsonIoError> {
    write_json_atomic(path, registry)
}

/// Validation failure for a workspace id handed to the archive commands.
#[derive(Debug, PartialEq, Eq)]
enum WorkspaceIdError {
    /// Empty after trimming — the FE must send a real id.
    Empty,
    /// Longer than [`WORKSPACE_ID_MAX_LEN`].
    TooLong,
    /// Contains a newline or NUL — never valid in a viewer id form.
    ControlChar,
    /// Not one of the recognised viewer-id prefixes.
    UnknownForm,
}

impl WorkspaceIdError {
    fn message(&self) -> &'static str {
        match self {
            WorkspaceIdError::Empty => "workspaceId must not be empty",
            WorkspaceIdError::TooLong => "workspaceId is too long",
            WorkspaceIdError::ControlChar => "workspaceId must not contain control characters",
            WorkspaceIdError::UnknownForm => {
                "workspaceId must start with root:, story:, or generic:"
            }
        }
    }
}

/// Pure validator for a workspace id. Shape-only — the FE only exposes archive
/// on workspaces it already displays, so a viewer-state round-trip would add
/// a false-reject risk (empty viewer state on cold boot) without catching a
/// different class of bugs. Rejects the forms we know are nonsense.
fn validate_workspace_id(raw: &str) -> Result<String, WorkspaceIdError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WorkspaceIdError::Empty);
    }
    if trimmed.len() > WORKSPACE_ID_MAX_LEN {
        return Err(WorkspaceIdError::TooLong);
    }
    if trimmed.chars().any(|c| c == '\n' || c == '\r' || c == '\0') {
        return Err(WorkspaceIdError::ControlChar);
    }
    let form_ok = trimmed.starts_with("root:")
        || trimmed.starts_with("story:")
        || trimmed.starts_with("generic:");
    if !form_ok {
        return Err(WorkspaceIdError::UnknownForm);
    }
    Ok(trimmed.to_string())
}

/// Tauri-managed state. Owns the on-disk file path and the in-memory mirror;
/// every mutation goes through [`Self::archive`] / [`Self::unarchive`] so the
/// map and file never diverge.
pub struct WorkspaceRegistryState {
    inner: RwLock<Inner>,
}

struct Inner {
    /// `None` when `codesense_home_dir()` was unavailable at startup — writes
    /// then no-op (the entry is kept in memory for the session but there is
    /// no file to atomically swap). The signed-out / headless case.
    path: Option<PathBuf>,
    registry: PersistedRegistry,
}

impl Default for WorkspaceRegistryState {
    fn default() -> Self {
        WorkspaceRegistryState {
            inner: RwLock::new(Inner {
                path: None,
                registry: PersistedRegistry::default(),
            }),
        }
    }
}

/// The read-model wire shape for one workspace's archive state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceArchiveState {
    pub workspace_id: String,
    pub archived_at_ms: Option<i64>,
}

impl WorkspaceRegistryState {
    /// Install the on-disk file this state writes to. Loads the current
    /// contents into memory. Called once at app boot — see `lib.rs`.
    pub fn install(&self, codesense_home: &Path) {
        let path = registry_path(codesense_home);
        let registry = load_registry(&path);
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.path = Some(path);
        inner.registry = registry;
    }

    /// Read the archive state for one workspace. Absent → `archived_at_ms:
    /// None` (the "not archived, no history" shape the FE renders identically
    /// to an explicitly-unarchived workspace).
    pub fn get(&self, workspace_id: &str) -> WorkspaceArchiveState {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        WorkspaceArchiveState {
            workspace_id: workspace_id.to_string(),
            archived_at_ms: inner
                .registry
                .workspaces
                .get(workspace_id)
                .and_then(|e| e.archived_at_ms),
        }
    }

    /// Every known workspace entry, in no particular order (the FE sorts for
    /// its own display).
    pub fn list(&self) -> Vec<WorkspaceArchiveState> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        inner
            .registry
            .workspaces
            .values()
            .map(|entry| WorkspaceArchiveState {
                workspace_id: entry.workspace_id.clone(),
                archived_at_ms: entry.archived_at_ms,
            })
            .collect()
    }

    /// Set `archived_at_ms = Some(now)` for `workspace_id`, flush, and return
    /// the resulting state. Writes are serialized under the write lock; a
    /// failed flush rolls the in-memory map back so the next successful call
    /// starts from the on-disk truth (same discipline as the launch registry).
    pub fn archive(
        &self,
        workspace_id: &str,
        now_ms: i64,
    ) -> Result<WorkspaceArchiveState, JsonIoError> {
        self.upsert(workspace_id, Some(now_ms))
    }

    /// Clear `archived_at_ms` for `workspace_id`. Keeps the entry so the FE
    /// can distinguish "explicitly unarchived" from "never archived".
    pub fn unarchive(&self, workspace_id: &str) -> Result<WorkspaceArchiveState, JsonIoError> {
        self.upsert(workspace_id, None)
    }

    fn upsert(
        &self,
        workspace_id: &str,
        archived_at_ms: Option<i64>,
    ) -> Result<WorkspaceArchiveState, JsonIoError> {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let previous = inner.registry.workspaces.insert(
            workspace_id.to_string(),
            WorkspaceArchiveEntry {
                workspace_id: workspace_id.to_string(),
                archived_at_ms,
            },
        );
        if let Some(path) = inner.path.clone() {
            if let Err(error) = write_registry(&path, &inner.registry) {
                // Roll the memory map back so a later retry starts from the
                // on-disk shape, never a partially-applied state.
                match previous {
                    Some(prev) => {
                        inner
                            .registry
                            .workspaces
                            .insert(workspace_id.to_string(), prev);
                    }
                    None => {
                        inner.registry.workspaces.remove(workspace_id);
                    }
                }
                return Err(error);
            }
        }
        Ok(WorkspaceArchiveState {
            workspace_id: workspace_id.to_string(),
            archived_at_ms,
        })
    }
}

fn now_epoch_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
}

fn emit_archive_changed(app: &AppHandle, state: &WorkspaceArchiveState) {
    if let Err(error) = app.emit(ARCHIVE_CHANGED_EVENT, state) {
        log::warn!(
            target: "workspace_registry",
            "failed to emit {} for {}: {error}",
            ARCHIVE_CHANGED_EVENT,
            state.workspace_id,
        );
    }
}

/// Stamp `workspace_id` as archived now. Emits
/// `codesense:workspace-archive-changed` on success.
#[tauri::command]
pub async fn archive_workspace(
    app: AppHandle,
    workspace_id: String,
) -> Result<WorkspaceArchiveState, String> {
    let id = validate_workspace_id(&workspace_id).map_err(|e| e.message().to_string())?;
    let now = now_epoch_ms();
    // CR3-7: close the archive TOCTOU with the chat engine.
    //
    //   1. Emit the "Workspace archived." lifecycle row FIRST (System row
    //      bypasses the store's archive gate). CR-11/CR2-1 ordering: the
    //      row must land before `emit_archive_changed` so a FE listener's
    //      post-archive refetch sees it.
    //   2. Stamp the chat-local archive flag inside the chat store's own
    //      mutex. From this point on, `store.append` under that same mutex
    //      sees the stamp and refuses new user/agent writes — any append
    //      racing the archive is strictly ordered against this call.
    //   3. Write the registry archive. Future top-gate `is_archived`
    //      checks (reads this state) refuse immediately.
    //   4. Emit `codesense:workspace-archive-changed` so FE listeners
    //      refetch.
    emit_chat_lifecycle_row(&app, &id, "Workspace archived.").await;
    propagate_chat_archive_stamp(&app, &id, Some(now));
    let state = app
        .state::<WorkspaceRegistryState>()
        .archive(&id, now)
        .map_err(|e| e.to_string())?;
    emit_archive_changed(&app, &state);
    Ok(state)
}

/// Clear the archive flag for `workspace_id`. Emits
/// `codesense:workspace-archive-changed` on success.
#[tauri::command]
pub async fn unarchive_workspace(
    app: AppHandle,
    workspace_id: String,
) -> Result<WorkspaceArchiveState, String> {
    let id = validate_workspace_id(&workspace_id).map_err(|e| e.message().to_string())?;
    // CR3-7: inverse order to archive_workspace.
    //
    //   1. Clear the registry archive FIRST. Top-gate `is_archived` for
    //      the next command returns false, but the chat-local stamp is
    //      still set so any append slipping past the top gate is still
    //      refused by the store.
    //   2. Clear the chat-local stamp. Writes now flow end-to-end.
    //   3. Emit the "Workspace unarchived." lifecycle row (now that the
    //      stamp is clear; System rows would bypass the gate anyway).
    //   4. Emit `codesense:workspace-archive-changed`.
    let state = app
        .state::<WorkspaceRegistryState>()
        .unarchive(&id)
        .map_err(|e| e.to_string())?;
    propagate_chat_archive_stamp(&app, &id, None);
    emit_chat_lifecycle_row(&app, &id, "Workspace unarchived.").await;
    emit_archive_changed(&app, &state);
    Ok(state)
}

/// Mirror the archive flag onto the chat store's chat-local
/// `archived_at_ms` stamp (CR3-7). Best-effort — a chat-engine hiccup must
/// not fail the registry archive command.
fn propagate_chat_archive_stamp(app: &AppHandle, workspace_id: &str, archived_at_ms: Option<i64>) {
    let chat_store = app.state::<super::workspace_chat::WorkspaceChatStore>();
    if !chat_store.is_installed() {
        return;
    }
    if let Err(e) = chat_store.set_archive_stamp(workspace_id, archived_at_ms) {
        log::warn!(
            target: "workspace_registry",
            "could not propagate archive stamp to chat store for {workspace_id}: {e}"
        );
    }
}

/// Append a `MessageKind::System` row to a workspace's chat (if the engine
/// is installed and the chat exists), awaiting the fsync so the archive /
/// unarchive command does not emit `codesense:workspace-archive-changed`
/// before the row is durable. Best-effort — any chat-engine failure logs
/// and is otherwise swallowed so the archive command never fails on a chat
/// hiccup.
async fn emit_chat_lifecycle_row(app: &AppHandle, workspace_id: &str, text: &str) {
    {
        let chat_store = app.state::<super::workspace_chat::WorkspaceChatStore>();
        if !chat_store.is_installed() || !chat_store.exists(workspace_id) {
            return;
        }
    }
    if let Err(e) = super::workspace_chat::emit_system_message(app, workspace_id, text).await {
        log::warn!(
            target: "workspace_registry",
            "could not emit chat lifecycle row for {workspace_id}: {e}"
        );
    }
}

/// Read one workspace's archive state. Unknown workspace → `archivedAtMs:
/// null` (the FE renders identically to an active workspace).
#[tauri::command]
pub async fn get_workspace_archive_state(
    app: AppHandle,
    workspace_id: String,
) -> Result<WorkspaceArchiveState, String> {
    let id = validate_workspace_id(&workspace_id).map_err(|e| e.message().to_string())?;
    Ok(app.state::<WorkspaceRegistryState>().get(&id))
}

/// List every workspace with a persisted archive entry (archived or
/// explicitly unarchived). The FE merges this with its visible workspace set.
#[tauri::command]
pub async fn list_workspace_archive_states(
    app: AppHandle,
) -> Result<Vec<WorkspaceArchiveState>, String> {
    Ok(app.state::<WorkspaceRegistryState>().list())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp_path() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(REGISTRY_FILENAME);
        (dir, path)
    }

    #[test]
    fn missing_file_returns_default_silently() {
        let (dir, path) = tmp_path();
        let registry = load_registry(&path);
        assert_eq!(registry.version, SCHEMA_VERSION);
        assert!(registry.workspaces.is_empty());
        drop(dir);
    }

    #[test]
    fn corrupt_file_returns_default_without_panic() {
        let (_dir, path) = tmp_path();
        std::fs::write(&path, "{not valid json").unwrap();
        let registry = load_registry(&path);
        assert_eq!(registry.version, SCHEMA_VERSION);
        assert!(registry.workspaces.is_empty());
    }

    #[test]
    fn unknown_version_returns_default() {
        let (_dir, path) = tmp_path();
        std::fs::write(
            &path,
            r#"{"version": 99, "workspaces": {"story:X": {"workspaceId": "story:X"}}}"#,
        )
        .unwrap();
        let registry = load_registry(&path);
        assert!(registry.workspaces.is_empty());
    }

    #[test]
    fn round_trip_preserves_entries() {
        let (_dir, path) = tmp_path();
        let mut registry = PersistedRegistry::default();
        registry.workspaces.insert(
            "story:SW-WC-1".into(),
            WorkspaceArchiveEntry {
                workspace_id: "story:SW-WC-1".into(),
                archived_at_ms: Some(123_456_789),
            },
        );
        write_registry(&path, &registry).unwrap();
        let loaded = load_registry(&path);
        assert_eq!(loaded.workspaces, registry.workspaces);
    }

    #[test]
    fn registry_path_lives_under_codesense_home() {
        let home = PathBuf::from("/tmp/cs");
        let path = registry_path(&home);
        assert_eq!(path, PathBuf::from("/tmp/cs/workspace-registry.json"));
    }

    #[test]
    fn write_creates_parent_dir() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sub").join(REGISTRY_FILENAME);
        write_registry(&path, &PersistedRegistry::default()).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn state_archive_persists_and_round_trips() {
        let (_dir, path) = tmp_path();
        let state = WorkspaceRegistryState::default();
        state.install(path.parent().unwrap());
        let result = state.archive("story:SW-WC-1", 1_700_000_000_000).unwrap();
        assert_eq!(result.archived_at_ms, Some(1_700_000_000_000));
        // Reload from disk; the write must have landed atomically.
        let loaded = load_registry(&path);
        assert_eq!(
            loaded
                .workspaces
                .get("story:SW-WC-1")
                .and_then(|e| e.archived_at_ms),
            Some(1_700_000_000_000),
        );
    }

    #[test]
    fn state_unarchive_keeps_entry_with_none() {
        let (_dir, path) = tmp_path();
        let state = WorkspaceRegistryState::default();
        state.install(path.parent().unwrap());
        state.archive("story:S", 100).unwrap();
        let result = state.unarchive("story:S").unwrap();
        assert_eq!(result.archived_at_ms, None);
        let loaded = load_registry(&path);
        assert!(loaded.workspaces.contains_key("story:S"));
        assert_eq!(
            loaded
                .workspaces
                .get("story:S")
                .and_then(|e| e.archived_at_ms),
            None
        );
    }

    #[test]
    fn state_get_absent_returns_none() {
        let (_dir, path) = tmp_path();
        let state = WorkspaceRegistryState::default();
        state.install(path.parent().unwrap());
        let got = state.get("story:never");
        assert_eq!(got.archived_at_ms, None);
        assert_eq!(got.workspace_id, "story:never");
    }

    #[test]
    fn state_list_reports_all_entries() {
        let (_dir, path) = tmp_path();
        let state = WorkspaceRegistryState::default();
        state.install(path.parent().unwrap());
        state.archive("story:A", 10).unwrap();
        state.archive("root:/x", 20).unwrap();
        state.unarchive("story:A").unwrap();
        let mut entries = state.list();
        entries.sort_by(|a, b| a.workspace_id.cmp(&b.workspace_id));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].workspace_id, "root:/x");
        assert_eq!(entries[0].archived_at_ms, Some(20));
        assert_eq!(entries[1].workspace_id, "story:A");
        assert_eq!(entries[1].archived_at_ms, None);
    }

    #[test]
    fn validate_workspace_id_rejects_empty() {
        assert_eq!(validate_workspace_id(""), Err(WorkspaceIdError::Empty));
        assert_eq!(validate_workspace_id("   "), Err(WorkspaceIdError::Empty));
    }

    #[test]
    fn validate_workspace_id_rejects_too_long() {
        let long = "story:".to_string() + &"a".repeat(300);
        assert_eq!(validate_workspace_id(&long), Err(WorkspaceIdError::TooLong));
    }

    #[test]
    fn validate_workspace_id_rejects_control_chars() {
        assert_eq!(
            validate_workspace_id("story:a\nb"),
            Err(WorkspaceIdError::ControlChar)
        );
        assert_eq!(
            validate_workspace_id("story:a\0b"),
            Err(WorkspaceIdError::ControlChar)
        );
    }

    #[test]
    fn validate_workspace_id_rejects_unknown_form() {
        assert_eq!(
            validate_workspace_id("branch:main"),
            Err(WorkspaceIdError::UnknownForm)
        );
        assert_eq!(
            validate_workspace_id("plain-id"),
            Err(WorkspaceIdError::UnknownForm)
        );
    }

    #[test]
    fn validate_workspace_id_accepts_known_forms() {
        assert_eq!(
            validate_workspace_id("story:SW-WC-1").unwrap(),
            "story:SW-WC-1"
        );
        assert_eq!(
            validate_workspace_id("root:/abs/path").unwrap(),
            "root:/abs/path"
        );
        assert_eq!(
            validate_workspace_id("generic:/scratch").unwrap(),
            "generic:/scratch"
        );
        // Whitespace is trimmed.
        assert_eq!(validate_workspace_id("  story:S  ").unwrap(), "story:S");
    }

    #[test]
    fn state_without_install_round_trips_in_memory_without_write() {
        // No install() → no path; writes are a no-op but the in-memory map
        // still updates (so the FE sees the toggle for the session).
        let state = WorkspaceRegistryState::default();
        let got = state.archive("story:ephemeral", 50).unwrap();
        assert_eq!(got.archived_at_ms, Some(50));
        assert_eq!(state.get("story:ephemeral").archived_at_ms, Some(50));
    }

    #[test]
    fn state_install_reloads_from_disk() {
        let (_dir, path) = tmp_path();
        // Seed disk directly, then install must pick it up.
        let mut registry = PersistedRegistry::default();
        registry.workspaces.insert(
            "story:preload".into(),
            WorkspaceArchiveEntry {
                workspace_id: "story:preload".into(),
                archived_at_ms: Some(999),
            },
        );
        write_registry(&path, &registry).unwrap();
        let state = WorkspaceRegistryState::default();
        state.install(path.parent().unwrap());
        assert_eq!(state.get("story:preload").archived_at_ms, Some(999));
    }

    #[test]
    fn state_second_archive_overwrites_timestamp() {
        let (_dir, path) = tmp_path();
        let state = WorkspaceRegistryState::default();
        state.install(path.parent().unwrap());
        state.archive("story:S", 100).unwrap();
        let second = state.archive("story:S", 200).unwrap();
        assert_eq!(second.archived_at_ms, Some(200));
        let loaded = load_registry(&path);
        assert_eq!(
            loaded
                .workspaces
                .get("story:S")
                .and_then(|e| e.archived_at_ms),
            Some(200)
        );
    }
}
