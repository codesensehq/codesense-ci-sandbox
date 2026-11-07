// SPDX-License-Identifier: MIT
//! SW-FX-launcher-persistence — persist each dashboard launch's `launched_by`
//! across app restarts.
//!
//! The on-disk companion to the in-memory [`super::DashboardLaunchState`]
//! registry: a per-principal JSON file under
//! `~/.codesense/users/{principal}/launch-registry.json` that records just
//! enough of each launch for the agent-list tree to reassemble after a sleep /
//! wake / app restart.
//!
//! What this layer owns:
//!   - the file shape (`version` + `launches: handle → PersistedLaunch`),
//!   - the atomic read/write choke,
//!   - the mapping between a live [`LaunchEntry`] and its persisted shadow.
//!
//! What it deliberately does NOT persist: status, run id, permit or process
//! state. The session server owns those. The session id is stored only to bind
//! a captured workspace to the exact live session that originally owned it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::context_repo::path_to_user_home;
use crate::domain::Principal;
use crate::shared::json_io::{read_json, write_json_atomic_private, JsonIoError};

/// The file the per-user launch registry is persisted to under
/// `path_to_user_home(home, principal)`.
pub(crate) const REGISTRY_FILENAME: &str = "launch-registry.json";

/// Current on-disk schema version. Bump when the shape changes in a way an
/// older reader cannot ignore; the loader treats a mismatch as "no record"
/// rather than fail boot.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// What the on-disk map keeps for one launch — the minimum needed to
/// re-stamp a reconnected session with the launcher it had before.
///
/// `launched_by` is the single load-bearing field: it is what the agent-list
/// tree reads to decide whether a session is a child of another session. The
/// other fields are display-only ride-alongs recorded so a future consumer
/// (e.g. agent-dag telemetry, cross-restart elapsed display) can read them
/// without re-reading the session-server tags.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistedLaunch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher_context: Option<String>,
    /// The launching session's handle (or raw session id for a tab with no
    /// handle), normalized the way [`super::DashboardLaunchState::begin_with_provenance`]
    /// records it. `None` means a human launched it, which is the agent-tree root.
    pub launched_by: Option<String>,
    /// ISO-8601 timestamp of when the launch was first recorded. Display-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launched_at: Option<String>,
    /// The team key (`org:team`) the launch was made in. Display-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_key: Option<String>,
    /// The handle this record is keyed by, mirrored into the value for log
    /// clarity when scanning the file by eye.
    pub handle: String,
    /// the deadline-sweep budget's epoch-ms deadline. Secondary
    /// source behind the session-server tag `launchDeadlineAt`; both can
    /// drop (unbounded or legacy launch), the restore then arms no
    /// deadline. Additive — a reader that doesn't know this key still
    /// reads the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    /// The session currently owning `worktree_dir`; prevents a later session
    /// under the same handle from inheriting a previous session's workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The app-captured workspace used by local publish admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_dir: Option<PathBuf>,
}

/// The full on-disk document. `launches` is keyed by handle to match the
/// in-memory [`super::Inner::by_handle`] shape, so a reconnect enrichment is
/// a direct `get(&handle)` lookup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistedRegistry {
    pub version: u32,
    pub launches: HashMap<String, PersistedLaunch>,
}

impl Default for PersistedRegistry {
    fn default() -> Self {
        PersistedRegistry {
            version: SCHEMA_VERSION,
            launches: HashMap::new(),
        }
    }
}

/// The absolute path to the per-principal registry file. Pure path math — no
/// filesystem touch, no existence check (so a signed-out `None` principal path
/// is resolved by the caller, not here).
pub(crate) fn registry_path(codesense_home: &Path, principal: &Principal) -> PathBuf {
    path_to_user_home(codesense_home, principal).join(REGISTRY_FILENAME)
}

/// Load the registry from disk. A missing file returns the default (empty)
/// registry silently — that is the fresh-install case. A corrupt file is
/// logged and treated as empty: the on-disk companion must never fail the
/// app boot, since the live session server is the authority on which sessions
/// still run.
///
/// A schema version mismatch is logged and treated as empty: a format-breaking
/// bump means an older reader cannot trust the newer file's shape, and
/// discarding it just means launched-by rebind degrades back to the behaviour
/// before this file existed.
pub(crate) fn load_registry(path: &Path) -> PersistedRegistry {
    match read_json::<PersistedRegistry>(path) {
        Ok(registry) if registry.version == SCHEMA_VERSION => registry,
        Ok(other) => {
            log::warn!(
                target: "agents",
                "launch registry at {} has unsupported version {} (expected {}); ignoring",
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
                target: "agents",
                "launch registry at {} could not be read ({error}); starting empty",
                path.display(),
            );
            PersistedRegistry::default()
        }
    }
}

/// Write `registry` atomically. The caller holds whatever lock is needed to
/// serialize writes — this is a pure IO choke that never partially writes.
pub(crate) fn write_registry(path: &Path, registry: &PersistedRegistry) -> Result<(), JsonIoError> {
    write_json_atomic_private(path, registry)
}

/// Format an epoch-millisecond timestamp as an ISO-8601 UTC string
/// (`YYYY-MM-DDTHH:MM:SSZ`). Used to stamp `launched_at` for display.
/// `None` when `epoch_ms` cannot be represented as a `SystemTime`.
pub(crate) fn iso_from_epoch_ms(epoch_ms: u64) -> Option<String> {
    let instant = UNIX_EPOCH.checked_add(Duration::from_millis(epoch_ms))?;
    Some(
        chrono::DateTime::<chrono::Utc>::from(instant)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

/// The in-memory + on-disk mirror of launcher provenance for the signed-in
/// principal. Writes are serialized through the owner — the
/// [`crate::commands::dashboard_launch::DashboardLaunchState`] holds this
/// behind a `Mutex` — so the memory map and the on-disk file cannot diverge
/// as long as every mutation goes through [`Self::upsert`] or [`Self::remove`].
#[derive(Debug)]
pub(crate) struct Store {
    path: PathBuf,
    registry: PersistedRegistry,
}

impl Store {
    /// Build a store around `registry` (as loaded from `path`). Does NOT touch
    /// the filesystem.
    pub(crate) fn new(path: PathBuf, registry: PersistedRegistry) -> Self {
        Store { path, registry }
    }

    /// The file this store writes to. Used by
    /// [`crate::commands::dashboard_launch::DashboardLaunchState::install_persistence`]
    /// to detect a no-op reinstall for the same principal.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The persisted record for `handle`, if any.
    pub(crate) fn get(&self, handle: &str) -> Option<&PersistedLaunch> {
        self.registry.launches.get(handle)
    }

    /// Upsert `record` under `handle`, then flush the whole registry to disk.
    /// If the write succeeds the memory map and the file agree; if it fails
    /// the memory map is rolled back to its prior shape so a later successful
    /// write starts from the on-disk truth.
    pub(crate) fn upsert(
        &mut self,
        handle: &str,
        record: PersistedLaunch,
    ) -> Result<(), JsonIoError> {
        let previous = self.registry.launches.insert(handle.to_string(), record);
        match write_registry(&self.path, &self.registry) {
            Ok(()) => Ok(()),
            Err(error) => {
                // Roll back the in-memory change so a later retry starts from
                // the on-disk shape (never a partially-applied state).
                match previous {
                    Some(previous) => {
                        self.registry.launches.insert(handle.to_string(), previous);
                    }
                    None => {
                        self.registry.launches.remove(handle);
                    }
                }
                Err(error)
            }
        }
    }

    /// Drop `handle`'s record and flush. A missing handle is a cheap no-op.
    pub(crate) fn remove(&mut self, handle: &str) -> Result<(), JsonIoError> {
        let Some(previous) = self.registry.launches.remove(handle) else {
            return Ok(());
        };
        match write_registry(&self.path, &self.registry) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.registry.launches.insert(handle.to_string(), previous);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp_registry() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(REGISTRY_FILENAME);
        (dir, path)
    }

    #[test]
    fn missing_file_returns_default_silently() {
        let (dir, path) = tmp_registry();
        let registry = load_registry(&path);
        assert_eq!(registry.version, SCHEMA_VERSION);
        assert!(registry.launches.is_empty());
        drop(dir);
    }

    #[test]
    fn corrupt_file_returns_default_without_panic() {
        let (_dir, path) = tmp_registry();
        std::fs::write(&path, "{this is not json").unwrap();
        let registry = load_registry(&path);
        assert_eq!(registry.version, SCHEMA_VERSION);
        assert!(registry.launches.is_empty());
    }

    #[test]
    fn unknown_version_returns_default() {
        let (_dir, path) = tmp_registry();
        std::fs::write(
            &path,
            r#"{"version": 99, "launches": {"H": {"launchedBy": "M", "handle": "H"}}}"#,
        )
        .unwrap();
        let registry = load_registry(&path);
        assert!(registry.launches.is_empty());
    }

    #[test]
    fn round_trip_preserves_launched_by() {
        let (_dir, path) = tmp_registry();
        let mut registry = PersistedRegistry::default();
        registry.launches.insert(
            "jimmy-helper".into(),
            PersistedLaunch {
                launcher_context: None,
                launched_by: Some("jimmy".into()),
                launched_at: Some("2026-10-06T12:00:00Z".into()),
                team_key: Some("alderco:codesense".into()),
                handle: "jimmy-helper".into(),
                deadline_ms: None,
                session_id: None,
                worktree_dir: None,
            },
        );
        write_registry(&path, &registry).unwrap();
        let loaded = load_registry(&path);
        assert_eq!(loaded.launches, registry.launches);
    }

    #[test]
    fn registry_path_lives_under_user_home() {
        let home = PathBuf::from("/tmp/cs");
        let principal = Principal::from(42u64);
        let path = registry_path(&home, &principal);
        assert!(path.ends_with("launch-registry.json"));
        assert!(path.to_string_lossy().contains("/users/42/"));
    }

    #[test]
    fn write_creates_parent_dir() {
        let dir = TempDir::new().unwrap();
        // Deep path that does not exist yet.
        let path = dir.path().join("users").join("42").join(REGISTRY_FILENAME);
        write_registry(&path, &PersistedRegistry::default()).unwrap();
        assert!(path.exists());
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn write_creates_file_0600_in_dir_0700() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("users").join("42").join(REGISTRY_FILENAME);
        write_registry(&path, &PersistedRegistry::default()).unwrap();
        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(mode_of(path.parent().unwrap()), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn replace_keeps_file_0600() {
        let (_dir, path) = tmp_registry();
        write_registry(&path, &PersistedRegistry::default()).unwrap();
        write_registry(&path, &PersistedRegistry::default()).unwrap();
        assert_eq!(mode_of(&path), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn existing_0644_file_and_0755_dir_are_tightened_on_next_write() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = tmp_registry();
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        write_registry(&path, &PersistedRegistry::default()).unwrap();
        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(mode_of(dir.path()), 0o700);
    }

    #[test]
    fn store_upsert_persists_to_disk() {
        let (_dir, path) = tmp_registry();
        let mut store = Store::new(path.clone(), PersistedRegistry::default());
        store
            .upsert(
                "worker",
                PersistedLaunch {
                    launcher_context: None,
                    launched_by: Some("manager".into()),
                    launched_at: Some("2026-10-06T12:00:00Z".into()),
                    team_key: Some("alderco:codesense".into()),
                    handle: "worker".into(),
                    deadline_ms: None,
                    session_id: None,
                    worktree_dir: None,
                },
            )
            .unwrap();
        // Reload from disk; the write must have landed.
        let loaded = load_registry(&path);
        assert_eq!(
            loaded
                .launches
                .get("worker")
                .and_then(|l| l.launched_by.as_deref()),
            Some("manager"),
        );
    }

    #[test]
    fn store_remove_drops_record_and_flushes() {
        let (_dir, path) = tmp_registry();
        let mut registry = PersistedRegistry::default();
        registry.launches.insert(
            "worker".into(),
            PersistedLaunch {
                launcher_context: None,
                launched_by: Some("manager".into()),
                launched_at: None,
                team_key: None,
                handle: "worker".into(),
                deadline_ms: None,
                session_id: None,
                worktree_dir: None,
            },
        );
        write_registry(&path, &registry).unwrap();
        let mut store = Store::new(path.clone(), registry);
        store.remove("worker").unwrap();
        let loaded = load_registry(&path);
        assert!(loaded.launches.is_empty());
    }

    #[test]
    fn store_remove_unknown_handle_is_noop() {
        let (_dir, path) = tmp_registry();
        let mut store = Store::new(path, PersistedRegistry::default());
        // Must not error, and must not create the file (nothing to write).
        store.remove("never-persisted").unwrap();
        assert!(store.get("never-persisted").is_none());
    }

    #[test]
    fn store_get_returns_persisted_launched_by() {
        let (_dir, path) = tmp_registry();
        let mut registry = PersistedRegistry::default();
        registry.launches.insert(
            "worker".into(),
            PersistedLaunch {
                launcher_context: None,
                launched_by: Some("manager".into()),
                launched_at: None,
                team_key: None,
                handle: "worker".into(),
                deadline_ms: None,
                session_id: None,
                worktree_dir: None,
            },
        );
        let store = Store::new(path, registry);
        assert_eq!(
            store.get("worker").and_then(|l| l.launched_by.as_deref()),
            Some("manager"),
        );
        assert!(store.get("absent").is_none());
    }

    #[test]
    fn iso_from_epoch_ms_formats_utc_rfc3339() {
        // 2026-10-06T12:00:00Z — epoch ms for that instant.
        // Using chrono as the oracle so the assertion survives timezone drift.
        let dt = chrono::DateTime::parse_from_rfc3339("2026-10-06T12:00:00Z").unwrap();
        let ms = dt.timestamp_millis() as u64;
        assert_eq!(
            iso_from_epoch_ms(ms).as_deref(),
            Some("2026-10-06T12:00:00Z")
        );
    }
}
