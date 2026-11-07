// SPDX-License-Identifier: MIT
//! Offline profile selection. Import identity metadata only; never refresh or alter credentials.
use super::auth::private_authority::{
    PrivateAuthority, PrivateAuthoritySnapshot, PrivateAuthorityStatus,
};
use crate::avatars::{self, FaceFormat};
use crate::domain::Principal;
use base64::Engine;
pub use codesense_session_server::machine_owner::MachineOwner;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::Manager;

/// The biggest custom-avatar bytes accepted. Fits in the yaml with the rest of
/// the profile without stressing the on-disk store.
pub const MAX_IDENTITY_AVATAR_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LocalProfile {
    pub principal: Principal,
    pub display_name: String,
    pub machine_owner: Option<MachineOwner>,
    /// The human's own face as `pack:id` (an avatar pack face) or a `data:` URL
    /// (a custom image uploaded here). Null falls back to initials of
    /// `display_name`. Per-user per-machine, never team-synced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_avatar: Option<String>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Profiles {
    active: Option<Principal>,
    profiles: BTreeMap<String, LocalProfile>,
    #[serde(default)]
    recovery_complete: bool,
}
impl Profiles {
    fn selected(&self) -> Option<&LocalProfile> {
        self.active
            .as_ref()
            .and_then(|id| self.profiles.get(id.as_str()))
    }
}

fn ensure_active_matches(
    profiles: &Profiles,
    expected: Option<&str>,
    error: &'static str,
) -> Result<(), String> {
    if profiles.active.as_ref().map(Principal::as_str) == expected {
        Ok(())
    } else {
        Err(error.into())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataRecovery {
    pub pending: bool,
    pub error: Option<String>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalProfileStatus {
    pub active: Option<LocalProfile>,
    pub profiles: Vec<LocalProfile>,
    pub private_authority: Option<PrivateAuthorityStatus>,
    pub metadata_recovery: MetadataRecovery,
}
pub struct LocalProfileState {
    path: PathBuf,
    // Published memory only. Never retain this guard across file I/O.
    profiles: Mutex<Profiles>,
    // Serialize persistence and profile-checked side effects with selection.
    // Only blocking callers may wait here; readers use `profiles` above.
    io: Mutex<()>,
    // Orders short in-memory publication with profile retirement/selection.
    // Never held during persistence.
    publication: Mutex<ProfilePublication>,
    revocations: std::sync::atomic::AtomicU64,
    authority: PrivateAuthority,
    selection_unpinned: std::sync::atomic::AtomicBool,
    // Runtime-only revocation; all accesses are serialized by `profiles`.
    machine_access_revoked: std::sync::atomic::AtomicBool,
    recovery_error: Mutex<Option<String>>,
}
#[derive(Default)]
struct ProfilePublication {
    retiring: HashSet<String>,
}
#[cfg(test)]
thread_local! {
    static PERSIST_HOOK: std::cell::RefCell<Option<Box<dyn Fn()>>> = const { std::cell::RefCell::new(None) };
}

impl LocalProfileState {
    fn published(&self) -> std::sync::LockResult<std::sync::MutexGuard<'_, Profiles>> {
        crate::shared::lock_ext::lock_named(&self.profiles, "profiles snapshot")
    }

    fn persist(&self, profiles: &Profiles) -> Result<(), String> {
        #[cfg(test)]
        PERSIST_HOOK.with(|hook| {
            if let Some(hook) = hook.borrow().as_ref() {
                hook();
            }
        });
        crate::shared::yaml_io::write_yaml_value_atomic(&self.path, profiles)
            .map_err(|e| e.to_string())
    }

    pub(crate) fn revoke(&self) {
        // Recovery and explicit selection publish under this same lock. A delayed
        // metadata read must never undo a revocation in either race ordering.
        let _profiles = self.published().unwrap_or_else(|e| e.into_inner());
        self.revocations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.machine_access_revoked
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.authority.publish_local(None);
    }
    #[cfg(test)]
    pub(crate) fn for_test(home: &Path, id: u64, owner: MachineOwner) -> Self {
        let principal = Principal::from(id);
        let profile = LocalProfile {
            principal: principal.clone(),
            display_name: "Test profile".into(),
            machine_owner: Some(owner),
            identity_avatar: None,
        };
        let profiles = Profiles {
            active: Some(principal.clone()),
            profiles: BTreeMap::from([(principal.to_string(), profile)]),
            recovery_complete: false,
        };
        crate::shared::yaml_io::write_yaml_value_atomic(
            &home.join("local-profiles.yml"),
            &profiles,
        )
        .unwrap();
        Self::load(home).unwrap()
    }

    pub fn load(home: &Path) -> Result<Self, String> {
        let path = home.join("local-profiles.yml");
        let import_pending = !path.exists();
        let mut profiles: Profiles =
            crate::shared::yaml_io::read_yaml_or_default(&path).map_err(|e| e.to_string())?;
        for (key, profile) in &profiles.profiles {
            Principal::parse_local(key)?;
            if key != profile.principal.as_str() {
                return Err("Invalid local profile association".into());
            }
            if let Some(owner) = &profile.machine_owner {
                owner.validate()?;
                if let MachineOwner::LegacyGithub { id } = owner {
                    if profile.principal.numeric_id()? != *id
                        || *id == crate::domain::LOCAL_GUEST_USER_ID
                    {
                        return Err("Invalid legacy profile association".into());
                    }
                }
            }
        }
        if let Some(active) = &profiles.active {
            if !profiles.profiles.contains_key(active.as_str()) {
                return Err("Selected local profile is missing".into());
            }
        }
        match std::fs::read_dir(home.join("users")) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|e| e.to_string())?;
                    if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                        continue;
                    }
                    let key = entry.file_name().to_string_lossy().into_owned();
                    if let Ok(principal) = Principal::parse_local(&key) {
                        profiles
                            .profiles
                            .entry(key.clone())
                            .or_insert(LocalProfile {
                                principal,
                                display_name: format!("Profile {key}"),
                                machine_owner: None,
                                identity_avatar: None,
                            });
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        // A fresh install retains the existing local namespace. Historical discovery
        // never assigns machine ownership from a directory name.
        if profiles.profiles.is_empty() {
            let principal = Principal::local_guest();
            profiles.active = Some(principal.clone());
            profiles.profiles.insert(
                principal.to_string(),
                LocalProfile {
                    principal,
                    display_name: "Local profile".into(),
                    machine_owner: Some(MachineOwner::Local {
                        id: uuid::Uuid::new_v4().to_string(),
                    }),
                    identity_avatar: None,
                },
            );
        }
        if profiles.active.is_none() && profiles.profiles.len() == 1 {
            // A sole historical data namespace is unambiguous, but carries no
            // legacy machine association merely because its directory exists.
            profiles.active = profiles
                .profiles
                .values()
                .next()
                .map(|p| p.principal.clone());
        }
        let state = Self {
            path,
            profiles: Mutex::new(profiles),
            io: Mutex::new(()),
            publication: Mutex::new(ProfilePublication::default()),
            revocations: std::sync::atomic::AtomicU64::new(0),
            authority: PrivateAuthority::default(),
            selection_unpinned: std::sync::atomic::AtomicBool::new(import_pending),
            recovery_error: Mutex::new(None),
            machine_access_revoked: std::sync::atomic::AtomicBool::new(false),
        };
        state.persist_publish()?;
        Ok(state)
    }
    fn persist_publish(&self) -> Result<(), String> {
        let next = self
            .published()
            .map_err(|_| "Local profiles unavailable")?
            .clone();
        self.persist(&next)?;
        let profiles = self.published().map_err(|_| "Local profiles unavailable")?;
        self.publish(&profiles);
        Ok(())
    }
    fn publish(&self, profiles: &Profiles) {
        if self
            .machine_access_revoked
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            self.authority.publish_local(None);
            return;
        }
        let identity = profiles.selected().and_then(|p| {
            p.machine_owner
                .clone()
                .map(|owner| (p.principal.clone(), owner))
        });
        self.authority.publish_local(identity);
    }
    pub fn active(&self) -> Result<LocalProfile, String> {
        let profiles = self.published().map_err(|_| "Local profiles unavailable")?;
        let active = profiles
            .selected()
            .cloned()
            .ok_or_else(|| "Choose a local profile to continue".to_string())?;
        self.selection_unpinned
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(active)
    }
    /// The selected profile, read without pinning the selection — for
    /// background readers (the context index) that must not decide which
    /// profile a pending legacy import lands on, the way [`Self::active`] does.
    /// `None` once machine access is revoked.
    pub fn peek_active(&self) -> Option<LocalProfile> {
        let profiles = self.published().ok()?;
        if self
            .machine_access_revoked
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return None;
        }
        profiles.selected().cloned()
    }
    pub fn snapshot(&self) -> Result<PrivateAuthoritySnapshot, String> {
        self.authority.snapshot()
    }
    fn status(&self) -> Result<LocalProfileStatus, String> {
        let p = self.published().map_err(|_| "Local profiles unavailable")?;
        Ok(LocalProfileStatus {
            active: p.selected().cloned(),
            profiles: p.profiles.values().cloned().collect(),
            private_authority: self.authority.presentation(),
            metadata_recovery: MetadataRecovery {
                pending: !p.recovery_complete,
                error: self
                    .recovery_error
                    .lock()
                    .map_err(|_| "Local profiles unavailable")?
                    .clone(),
            },
        })
    }
    /// A short selection check for callers that capture their own principal.
    /// It does not serialize any later side effects with a profile switch.
    pub(crate) fn check_expected(&self, expected: Option<&str>) -> Result<(), String> {
        if let Some(expected) = expected {
            Principal::parse_local(expected)?;
        }
        let profiles = self.published().map_err(|_| "Local profiles unavailable")?;
        ensure_active_matches(
            &profiles,
            expected,
            "Local profile changed; reload this window",
        )?;
        if expected.is_some() {
            self.selection_unpinned
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    }

    /// Run a short in-memory publication while the selected principal remains
    /// pinned. This deliberately skips the writer mutex so memory-only updates
    /// can publish while profile persistence is stalled.
    pub(crate) fn with_expected_published<T>(
        &self,
        expected: &str,
        run: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        Principal::parse_local(expected)?;
        let publication = self
            .publication
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if publication.retiring.contains(expected) {
            return Err("Local profile is retiring; reload this window".into());
        }
        let profiles = self.published().map_err(|_| "Local profiles unavailable")?;
        ensure_active_matches(
            &profiles,
            Some(expected),
            "Local profile changed; reload this window",
        )?;
        run()
    }

    /// Serialize profile-checked side effects with selection, off runtime workers.
    /// Readers keep using the last published snapshot while persistence is stalled.
    pub(crate) fn with_expected<T>(
        &self,
        expected: Option<&str>,
        run: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        if let Some(expected) = expected {
            Principal::parse_local(expected)?;
        }
        let _io = crate::shared::lock_ext::lock_named(&self.io, "profile I/O")
            .map_err(|_| "Local profiles unavailable")?;
        {
            let profiles = self.published().map_err(|_| "Local profiles unavailable")?;
            ensure_active_matches(
                &profiles,
                expected,
                "Local profile changed; reload this window",
            )?;
        }
        let result = run();
        if result.is_ok() && expected.is_some() {
            self.selection_unpinned
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        result
    }
    #[cfg(test)]
    pub(crate) fn select(
        &self,
        principal: &str,
        display_name: Option<String>,
    ) -> Result<(), String> {
        self.select_after(principal, display_name, |_| Ok(()))
    }
    /// Edit the viewer's display name and chosen avatar in place. Writes the
    /// active profile's row of `local-profiles.yml` and never changes the
    /// active principal. Fields passed `None` are kept as-is.
    pub(crate) fn set_identity(
        &self,
        expected_principal: &str,
        display_name: Option<String>,
        identity_avatar: Option<Option<String>>,
    ) -> Result<LocalProfile, String> {
        let _io = crate::shared::lock_ext::lock_named(&self.io, "profile I/O")
            .map_err(|_| "Local profiles unavailable")?;
        let p = self.published().map_err(|_| "Local profiles unavailable")?;
        ensure_active_matches(
            &p,
            Some(expected_principal),
            "The selected local profile changed before the identity update",
        )?;
        let active = p
            .active
            .clone()
            .ok_or_else(|| "No local profile is active".to_string())?;
        let mut next = p.clone();
        let profile = next
            .profiles
            .get_mut(active.as_str())
            .ok_or("Unknown local profile")?;
        if let Some(name) = display_name {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                profile.display_name = trimmed.to_string();
            }
        }
        if let Some(avatar) = identity_avatar {
            profile.identity_avatar = avatar;
        }
        let edited = profile.clone();
        drop(p);
        self.persist(&next)?;
        let mut p = self.published().map_err(|_| "Local profiles unavailable")?;
        self.publish(&next);
        *p = next;
        Ok(edited)
    }
    fn select_after(
        &self,
        principal: &str,
        display_name: Option<String>,
        retire: impl FnOnce(Option<&Principal>) -> Result<(), String>,
    ) -> Result<(), String> {
        self.select_after_with_activation(principal, display_name, retire, |_| {})
    }
    fn select_after_with_activation(
        &self,
        principal: &str,
        display_name: Option<String>,
        retire: impl FnOnce(Option<&Principal>) -> Result<(), String>,
        activate: impl FnOnce(Option<&Principal>),
    ) -> Result<(), String> {
        Principal::parse_local(principal)?;
        let _io = crate::shared::lock_ext::lock_named(&self.io, "profile I/O")
            .map_err(|_| "Local profiles unavailable")?;
        let p = self.published().map_err(|_| "Local profiles unavailable")?;
        let revocations = self.revocations.load(std::sync::atomic::Ordering::SeqCst);
        let mut next = p.clone();
        let previous = p.active.clone();
        drop(p);
        let profile = next
            .profiles
            .get_mut(principal)
            .ok_or("Unknown local profile")?;
        if let Some(name) = display_name {
            profile.display_name = name.trim().to_string();
        }
        // Explicit local selection permits new setup with a unique local marker,
        // never access to legacy Github markers with the same numeric namespace.
        if profile.machine_owner.is_none() {
            profile.machine_owner = Some(MachineOwner::Local {
                id: uuid::Uuid::new_v4().to_string(),
            });
        }
        next.active = Some(profile.principal.clone());
        let switched = previous != next.active;
        if switched {
            {
                let mut publication = self
                    .publication
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if let Some(previous) = &previous {
                    publication.retiring.insert(previous.to_string());
                }
            }
            if let Err(error) = retire(previous.as_ref()) {
                if let Some(previous) = &previous {
                    self.publication
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .retiring
                        .remove(previous.as_str());
                }
                return Err(error);
            }
        }
        if let Err(error) = self.persist(&next) {
            if switched {
                let mut publication = self
                    .publication
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if let Some(previous) = &previous {
                    publication.retiring.remove(previous.as_str());
                }
            }
            return Err(error);
        }
        let mut publication = self
            .publication
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut p = self.published().map_err(|_| "Local profiles unavailable")?;
        self.selection_unpinned
            .store(false, std::sync::atomic::Ordering::SeqCst);
        // Only a successfully persisted explicit selection/setup restores access.
        if self.revocations.load(std::sync::atomic::Ordering::SeqCst) == revocations {
            self.machine_access_revoked
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        self.publish(&next);
        let selected = next.active.clone();
        *p = next;
        if switched {
            activate(selected.as_ref());
            if let Some(selected) = selected {
                publication.retiring.remove(selected.as_str());
            }
        }
        Ok(())
    }
    fn recover_metadata(
        &self,
        legacy: Result<Option<super::auth::AuthFile>, String>,
    ) -> Result<(), String> {
        let result = (|| {
            let legacy = legacy?;
            let _io = crate::shared::lock_ext::lock_named(&self.io, "profile I/O")
                .map_err(|_| "Local profiles unavailable")?;
            let p = self.published().map_err(|_| "Local profiles unavailable")?;
            let mut next = p.clone();
            drop(p);
            if let Some((key, user, id)) = legacy
                .as_ref()
                .and_then(|legacy| legacy.connections.get("github"))
                .and_then(|github| github.active_account.as_ref().map(|key| (github, key)))
                .and_then(|(github, key)| {
                    github
                        .accounts
                        .get(key)
                        .and_then(|a| a.user_info.as_ref())
                        .map(|user| (key, user))
                })
                .and_then(|(key, user)| {
                    user.user_id
                        .filter(|id| {
                            *id != 0
                                && *id != crate::domain::LOCAL_GUEST_USER_ID
                                && id.to_string() == *key
                        })
                        .map(|id| (key, user, id))
                })
            {
                let principal = Principal::from(id);
                let profile = next
                    .profiles
                    .entry(key.clone())
                    .or_insert_with(|| LocalProfile {
                        principal: principal.clone(),
                        display_name: user
                            .display_name
                            .clone()
                            .unwrap_or_else(|| user.username.clone()),
                        machine_owner: None,
                        identity_avatar: None,
                    });
                // Metadata may restore a missing association, never replace an owner
                // established by explicit local setup or move an in-use namespace.
                if profile.machine_owner.is_none() {
                    profile.machine_owner = Some(MachineOwner::LegacyGithub { id });
                }
                if self
                    .selection_unpinned
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    next.active = Some(principal);
                }
            }
            next.recovery_complete = true;
            loop {
                self.persist(&next)
                    .map_err(|_| "Historical identity metadata could not be saved; recovery will retry on the next startup".to_string())?;
                let mut p = self.published().map_err(|_| "Local profiles unavailable")?;
                // An active() reader may have pinned the old snapshot during I/O.
                // Never move a namespace it has already begun using.
                if next.active != p.active
                    && !self
                        .selection_unpinned
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    next.active = p.active.clone();
                    drop(p);
                    continue;
                }
                self.publish(&next);
                *p = next;
                break;
            }
            Ok(())
        })();
        *self
            .recovery_error
            .lock()
            .map_err(|_| "Local profiles unavailable")? = result.as_ref().err().cloned();
        result
    }
    #[cfg(test)]
    fn import(&self, legacy: &super::auth::AuthFile) -> Result<(), String> {
        self.recover_metadata(Ok(Some(legacy.clone())))
    }
    pub fn start_import(app: tauri::AppHandle) {
        let state = app.state::<Self>();
        state.authority.bridge_retirement(app.clone());
        if state
            .published()
            .map(|p| p.recovery_complete)
            .unwrap_or(false)
        {
            return;
        }
        std::thread::spawn(move || {
            use super::credential_store::{default_store, AUTH_ACCOUNT, AUTH_SERVICE};
            let message = "Historical identity metadata could not be read; local use remains available and recovery will retry on the next startup";
            let keychain = default_store(AUTH_SERVICE)
                .get(AUTH_ACCOUNT)
                .map_err(|_| message.to_string())
                .and_then(|secret| {
                    secret
                        .map(|secret| {
                            serde_json::from_str::<super::auth::AuthFile>(secret.expose())
                                .map_err(|_| message.to_string())
                        })
                        .transpose()
                });
            let legacy = match keychain {
                Ok(Some(legacy)) => Ok(Some(legacy)),
                keychain => {
                    let file = app
                        .path()
                        .app_config_dir()
                        .map_err(|_| message.to_string())
                        .and_then(|p| match std::fs::read(p.join("auth.json")) {
                            Ok(bytes) => serde_json::from_slice(&bytes)
                                .map(Some)
                                .map_err(|_| message.to_string()),
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                            Err(_) => Err(message.to_string()),
                        });
                    match file {
                        Ok(Some(legacy)) => Ok(Some(legacy)),
                        Ok(None) => keychain,
                        Err(e) => Err(e),
                    }
                }
            };
            let _ = app.state::<Self>().recover_metadata(legacy);
            // SW-OC-26: recovery may change the selected profile — re-index for it.
            crate::context_query::reset_footprint(&app);
            use tauri::Emitter;
            let _ = app.emit("local-profiles-changed", ());
        });
    }
}
/// The selected profile without pinning the selection (see
/// [`LocalProfileState::peek_active`]).
pub fn peek_active<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Option<LocalProfile> {
    app.try_state::<LocalProfileState>()?.peek_active()
}
pub fn active<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<LocalProfile, String> {
    app.try_state::<LocalProfileState>()
        .ok_or("Local profiles unavailable")?
        .active()
}
fn clear_profile_viewer_mirror(viewer: &crate::mcp::state::McpViewerState) {
    viewer.update(serde_json::json!({}));
}
#[tauri::command]
pub fn local_profile_status(
    state: tauri::State<'_, LocalProfileState>,
) -> Result<LocalProfileStatus, String> {
    state.status()
}
#[tauri::command]
pub async fn select_local_profile(
    app: tauri::AppHandle,
    principal: String,
    display_name: Option<String>,
) -> Result<LocalProfileStatus, String> {
    let watcher = app.try_state::<super::watch::WatcherState>();
    let _activation = match watcher.as_ref() {
        Some(watcher) => Some(watcher.lock_team_activation().await),
        None => None,
    };
    let handle = app.clone();
    // SW-OC-26: stop serving the current profile's context before the switch …
    crate::context_query::reset_footprint(&app);
    super::await_blocking("select local profile", move || {
        let state = handle.state::<LocalProfileState>();
        state.select_after_with_activation(
            &principal,
            display_name,
            |old| {
                super::system::preserve_profile_workspace(&handle, old)?;
                if let Some(old) = old {
                    if let Some(coord) = handle
                        .try_state::<std::sync::Arc<crate::content_engine::sync::WaveCoordinator>>()
                    {
                        coord.detach_principal(old.as_str());
                    }
                    super::watch::retire_profile_watches(&handle)?;
                }
                if let Some(active) = handle.try_state::<crate::mcp::state::ActiveTeamState>() {
                    active.clear();
                }
                crate::context_repo::scratch_space::clear_active_orgs();
                if let Some(viewer) = handle.try_state::<crate::mcp::state::McpViewerState>() {
                    clear_profile_viewer_mirror(&viewer);
                }
                Ok(())
            },
            |_| {
                if let Some(viewer) = handle.try_state::<crate::mcp::state::McpViewerState>() {
                    clear_profile_viewer_mirror(&viewer);
                }
            },
        )?;
        // … and again after it, so no pass begun before the switch publishes
        // the previous profile's context (its private space included).
        crate::context_query::reset_footprint(&handle);
        state.status()
    })
    .await
}
#[tauri::command]
pub fn revoke_local_machine_access(
    app: tauri::AppHandle,
    state: tauri::State<'_, LocalProfileState>,
) -> Result<(), String> {
    state.revoke();
    crate::context_query::reset_footprint(&app);
    Ok(())
}

/// The avatar an edit asks for: a pack face, a custom image uploaded as raw
/// bytes (validated and stored as a `data:` URL), or `clear` to drop it.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum IdentityAvatarInput {
    /// A `pack:id` reference to an avatar pack face.
    Pack {
        #[serde(rename = "ref")]
        reference: String,
    },
    /// A raw image (PNG, WebP or SVG) uploaded from the viewer, at most
    /// [`MAX_IDENTITY_AVATAR_BYTES`] big. The server sniffs the bytes to pick
    /// the mime type and cleans SVGs so nothing executable is stored.
    Custom { bytes: Vec<u8> },
    /// Drop the current avatar, falling back to initials.
    Clear,
}

fn resolve_identity_avatar(input: IdentityAvatarInput) -> Result<Option<String>, String> {
    match input {
        IdentityAvatarInput::Clear => Ok(None),
        IdentityAvatarInput::Pack { reference } => {
            let trimmed = reference.trim();
            avatars::parse_face_ref(trimmed)
                .ok_or_else(|| format!("`{trimmed}` is not a `pack:id` avatar reference"))?;
            Ok(Some(trimmed.to_string()))
        }
        IdentityAvatarInput::Custom { bytes } => {
            if bytes.is_empty() {
                return Err("The avatar image is empty".to_string());
            }
            if bytes.len() > MAX_IDENTITY_AVATAR_BYTES {
                return Err(format!(
                    "The avatar image is {} bytes; the limit is {MAX_IDENTITY_AVATAR_BYTES} bytes",
                    bytes.len()
                ));
            }
            let format = FaceFormat::sniff(&bytes)
                .ok_or_else(|| "The avatar must be a PNG, WebP or SVG image".to_string())?;
            let (mime, encoded_bytes) = match format {
                FaceFormat::Svg => {
                    let text = std::str::from_utf8(&bytes)
                        .map_err(|_| "The SVG is not UTF-8 text".to_string())?;
                    let cleaned = avatars::svg::sanitize(text)?.svg;
                    ("image/svg+xml", cleaned.into_bytes())
                }
                FaceFormat::Png | FaceFormat::Webp => (format.mime(), bytes),
            };
            let data = base64::engine::general_purpose::STANDARD.encode(&encoded_bytes);
            Ok(Some(format!("data:{mime};base64,{data}")))
        }
    }
}

/// Edit the viewer's display name and chosen avatar for the active local
/// profile. Private, per-user per-machine; never team-synced.
#[tauri::command]
pub async fn set_local_identity(
    app: tauri::AppHandle,
    expected_principal: String,
    display_name: Option<String>,
    avatar: Option<IdentityAvatarInput>,
) -> Result<LocalProfile, String> {
    super::await_blocking("set local identity", move || {
        let avatar = avatar.map(resolve_identity_avatar).transpose()?;
        app.state::<LocalProfileState>()
            .set_identity(&expected_principal, display_name, avatar)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cp3_active_reader_pins_namespace_while_metadata_persistence_is_stalled() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        let home = tempfile::tempdir().unwrap();
        let state = Arc::new(LocalProfileState::load(home.path()).unwrap());
        let original = state.peek_active().unwrap().principal;
        let importing = state.clone();
        let (entered, waiting) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let import = std::thread::spawn(move || {
            let once = std::cell::Cell::new(false);
            PERSIST_HOOK.with(|hook| {
                hook.replace(Some(Box::new(move || {
                    if !once.replace(true) {
                        entered.send(()).unwrap();
                        resume.recv().unwrap();
                    }
                })));
            });
            importing.import(&legacy(41, "41"))
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let reading = state.clone();
        let (read, observed) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            read.send(reading.active().map(|profile| profile.principal))
                .unwrap();
        });
        let active = observed.recv_timeout(Duration::from_millis(250));
        release.send(()).unwrap();
        import.join().unwrap().unwrap();
        reader.join().unwrap();
        assert_eq!(
            active
                .expect("active reader waited on metadata I/O")
                .unwrap(),
            original
        );
        assert_eq!(state.active().unwrap().principal, original);
        assert_eq!(
            LocalProfileState::load(home.path())
                .unwrap()
                .active()
                .unwrap()
                .principal,
            original
        );
        assert!(state
            .status()
            .unwrap()
            .profiles
            .iter()
            .any(|p| p.principal.as_str() == "41"));
    }

    #[test]
    fn cp3_memory_publication_does_not_wait_for_profile_writer_io() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("users/41")).unwrap();
        std::fs::create_dir_all(home.path().join("users/42")).unwrap();
        let state = Arc::new(LocalProfileState::load(home.path()).unwrap());
        state.select("41", None).unwrap();
        let writer_state = state.clone();
        let (entered, waiting) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            PERSIST_HOOK.with(|hook| {
                hook.replace(Some(Box::new(move || {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                })));
            });
            // Persist a profile update without retiring A. An expected-A
            // memory publication must remain independent of writer I/O.
            writer_state.select("41", Some("Renamed profile".into()))
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let publisher_state = state.clone();
        let (published, observed) = mpsc::channel();
        let publisher = std::thread::spawn(move || {
            publisher_state.with_expected_published("41", || {
                published.send(()).unwrap();
                Ok(())
            })
        });
        let published_while_writer_stalled = observed.recv_timeout(Duration::from_millis(100));
        release.send(()).unwrap();
        writer.join().unwrap().unwrap();
        let publication_result = publisher.join().unwrap();
        assert!(
            published_while_writer_stalled.is_ok(),
            "memory-only publication waited for profile persistence"
        );
        publication_result.unwrap();
    }

    #[test]
    fn cp3_retired_profile_push_cannot_repopulate_viewer_mirror() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        let home = tempfile::tempdir().unwrap();
        for id in ["41", "42"] {
            std::fs::create_dir_all(home.path().join("users").join(id)).unwrap();
        }
        let profile = Arc::new(LocalProfileState::load(home.path()).unwrap());
        profile.select("41", None).unwrap();
        let viewer = Arc::new(crate::mcp::state::McpViewerState::default());
        crate::mcp::state::publish_viewer_state(
            &profile,
            &viewer,
            Some("41"),
            1,
            serde_json::json!({"workspaces":["A"]}),
        )
        .unwrap();

        let selecting = profile.clone();
        let retiring_viewer = viewer.clone();
        let (entered, waiting) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            PERSIST_HOOK.with(|hook| {
                hook.replace(Some(Box::new(move || {
                    entered.send(()).unwrap();
                    resume.recv().unwrap();
                })));
            });
            selecting.select_after_with_activation(
                "42",
                None,
                |old| {
                    assert_eq!(old.unwrap().as_str(), "41");
                    clear_profile_viewer_mirror(&retiring_viewer);
                    Ok(())
                },
                |_| clear_profile_viewer_mirror(&retiring_viewer),
            )
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let stale_push = crate::mcp::state::publish_viewer_state(
            &profile,
            &viewer,
            Some("41"),
            2,
            serde_json::json!({"workspaces":["stale A"]}),
        );
        release.send(()).unwrap();
        writer.join().unwrap().unwrap();

        assert!(
            stale_push.is_err(),
            "retiring profile publication was accepted"
        );
        assert_eq!(viewer.get(), serde_json::json!({}));
        crate::mcp::state::publish_viewer_state(
            &profile,
            &viewer,
            Some("42"),
            3,
            serde_json::json!({"workspaces":["B"]}),
        )
        .unwrap();
        assert_eq!(viewer.get(), serde_json::json!({"workspaces":["B"]}));
    }

    #[test]
    fn cp3_revoke_during_selection_io_wins_and_readers_keep_the_old_snapshot() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        let home = tempfile::tempdir().unwrap();
        for id in ["41", "42"] {
            std::fs::create_dir_all(home.path().join("users").join(id)).unwrap();
        }
        let state = Arc::new(LocalProfileState::load(home.path()).unwrap());
        state.select("41", None).unwrap();
        let lease = state.snapshot().unwrap().lease();
        let writer_state = state.clone();
        let (entered, waiting) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            writer_state.select_after("42", None, |old| {
                assert_eq!(old.unwrap().as_str(), "41");
                entered.send(()).unwrap();
                resume.recv().unwrap();
                Ok(())
            })
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let reading = state.clone();
        let (read, observed) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let active = reading.active().unwrap().principal;
            let peek = reading.peek_active().unwrap().principal;
            reading.revoke();
            read.send((
                active,
                peek,
                reading.peek_active().is_none(),
                lease.validate().is_err(),
            ))
            .unwrap();
        });
        let result = observed.recv_timeout(Duration::from_millis(250));
        release.send(()).unwrap();
        writer.join().unwrap().unwrap();
        reader.join().unwrap();
        let (active, peek, revoked, retired) = result.expect("read/revoke waited on selection I/O");
        assert_eq!(active.as_str(), "41");
        assert_eq!(peek.as_str(), "41");
        assert!(revoked && retired);
        assert_eq!(state.active().unwrap().principal.as_str(), "42");
        assert!(state.peek_active().is_none());
        assert!(state.snapshot().is_err());
        state.select("42", None).unwrap();
        assert!(state.snapshot().is_ok());
    }

    fn legacy(id: u64, key: &str) -> super::super::auth::AuthFile {
        serde_json::from_value(serde_json::json!({"connections":{"github":{
            "active_account":key,"accounts":{(key):{"user_info":{"username":"Ada","user_id":id,"orgs":[]},"tokens":{"access_token":"expired","expires_at":"2000-01-01T00:00:00Z"}}}
        }}})).unwrap()
    }
    #[test]
    fn fresh_offline_profile_reuses_local_namespace_and_persists_unique_machine_identity() {
        let home = tempfile::tempdir().unwrap();
        let state = LocalProfileState::load(home.path()).unwrap();
        let active = state.active().unwrap();
        assert_eq!(active.principal, Principal::local_guest());
        let owner = active.machine_owner.clone().unwrap();
        assert!(matches!(owner, MachineOwner::Local { .. }));
        assert_eq!(
            LocalProfileState::load(home.path())
                .unwrap()
                .active()
                .unwrap()
                .machine_owner,
            Some(owner.clone())
        );
        let another = tempfile::tempdir().unwrap();
        assert_ne!(
            LocalProfileState::load(another.path())
                .unwrap()
                .active()
                .unwrap()
                .machine_owner,
            Some(owner)
        );
    }
    #[test]
    fn historical_directories_require_selection_and_never_grant_legacy_identity() {
        let home = tempfile::tempdir().unwrap();
        for id in ["41", "42"] {
            std::fs::create_dir_all(home.path().join("users").join(id)).unwrap();
        }
        let state = LocalProfileState::load(home.path()).unwrap();
        assert!(state.active().is_err());
        assert!(state.snapshot().is_err());
        state.select("41", None).unwrap();
        assert!(matches!(
            state.snapshot().unwrap().machine_owner(),
            MachineOwner::Local { .. }
        ));
        let lease = state.snapshot().unwrap().lease();
        state.select("42", None).unwrap();
        assert!(lease.validate().is_err());
        state.select("41", None).unwrap();
        assert!(lease.validate().is_err());
        let lease = state.snapshot().unwrap().lease();
        state.authority.publish_local(None);
        assert!(lease.validate().is_err());
    }
    #[test]
    fn matching_selected_metadata_imports_expired_account_without_refresh() {
        let home = tempfile::tempdir().unwrap();
        for id in ["41", "42"] {
            std::fs::create_dir_all(home.path().join("users").join(id)).unwrap();
        }
        let state = LocalProfileState::load(home.path()).unwrap();
        state.import(&legacy(41, "42")).unwrap();
        assert!(state.active().is_err());
        state.import(&legacy(41, "41")).unwrap();
        assert_eq!(state.active().unwrap().principal.as_str(), "41");
        assert_eq!(
            state.snapshot().unwrap().machine_owner(),
            MachineOwner::LegacyGithub { id: 41 }
        );
        assert_eq!(state.status().unwrap().profiles.len(), 2);
        state.select("42", None).unwrap();
        state.import(&legacy(41, "41")).unwrap();
        assert_eq!(state.active().unwrap().principal.as_str(), "42");
    }
    #[test]
    fn persisted_path_injection_is_rejected_without_mutating_data() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("local-profiles.yml");
        let data = "active: ../41\nprofiles:\n  ../41:\n    principal: ../41\n    displayName: bad\n    machineOwner: null\n";
        std::fs::write(&path, data).unwrap();
        assert!(LocalProfileState::load(home.path()).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), data);
    }
    #[test]
    fn metadata_status_does_not_pin_but_active_use_and_selection_do() {
        for pin in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let state = LocalProfileState::load(home.path()).unwrap();
            state.status().unwrap();
            if pin {
                state.active().unwrap();
            }
            state.import(&legacy(41, "41")).unwrap();
            assert_eq!(
                state.active().unwrap().principal.numeric_id().unwrap(),
                if pin {
                    crate::domain::LOCAL_GUEST_USER_ID
                } else {
                    41
                }
            );
        }
    }
    #[test]
    fn stale_window_and_failed_retirement_cannot_publish_identity() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("users/41")).unwrap();
        std::fs::create_dir_all(home.path().join("users/42")).unwrap();
        let state = LocalProfileState::load(home.path()).unwrap();
        state.select("41", None).unwrap();
        assert!(state
            .select_after("42", None, |_| Err("cannot retire".into()))
            .is_err());
        assert_eq!(state.active().unwrap().principal.as_str(), "41");
        let lease = state.snapshot().unwrap();
        state.select("42", None).unwrap();
        assert!(state.with_expected(Some("41"), || Ok(())).is_err());
        assert!(state.with_expected(Some("042"), || Ok(())).is_err());
        state.with_expected(Some("42"), || Ok(())).unwrap();
        state.import(&legacy(41, "41")).unwrap();
        assert_eq!(state.active().unwrap().principal.as_str(), "42");
        assert!(lease.lease().validate().is_err());
    }
    #[test]
    fn unrelated_malformed_metadata_preserves_exact_legacy_account() {
        let auth: super::super::auth::AuthFile =
            serde_json::from_value(serde_json::json!({"connections": {
                "broken": [42], "github": {"active_account_id": 41, "accounts": {
                    "bad": "malformed", "41": {"user_info": {"username":"Ada", "user_id":41}}
                }}
            }}))
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let state = LocalProfileState::load(home.path()).unwrap();
        state.import(&auth).unwrap();
        assert_eq!(
            state.active().unwrap().machine_owner,
            Some(MachineOwner::LegacyGithub { id: 41 })
        );
    }
    #[test]
    fn sole_historical_namespace_opens_offline_without_claiming_machine_identity() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("users/41")).unwrap();
        let state = LocalProfileState::load(home.path()).unwrap();
        let selected = state.active().unwrap();
        assert_eq!(selected.principal.as_str(), "41");
        assert!(selected.machine_owner.is_none());
    }
    #[test]
    fn delayed_metadata_recovers_matching_missing_owner_after_local_use_and_restart() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("users/41")).unwrap();
        let state = LocalProfileState::load(home.path()).unwrap();
        assert!(state.active().unwrap().machine_owner.is_none());
        assert!(state
            .recover_metadata(Err("Historical metadata unavailable".into()))
            .is_err());
        assert!(state.status().unwrap().metadata_recovery.pending);
        assert!(state.status().unwrap().metadata_recovery.error.is_some());
        let state = LocalProfileState::load(home.path()).unwrap();
        state.import(&legacy(41, "41")).unwrap();
        assert_eq!(
            state.active().unwrap().machine_owner,
            Some(MachineOwner::LegacyGithub { id: 41 })
        );
        assert!(!state.status().unwrap().metadata_recovery.pending);
        assert!(state.status().unwrap().metadata_recovery.error.is_none());
        assert!(
            !LocalProfileState::load(home.path())
                .unwrap()
                .status()
                .unwrap()
                .metadata_recovery
                .pending
        );
    }
    #[test]
    fn explicit_local_owner_is_never_replaced_by_delayed_metadata() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("users/41")).unwrap();
        let state = LocalProfileState::load(home.path()).unwrap();
        state.select("41", None).unwrap();
        let before = state.active().unwrap().machine_owner;
        state.import(&legacy(41, "41")).unwrap();
        assert_eq!(state.active().unwrap().machine_owner, before);
    }
    #[test]
    fn metadata_write_failure_preserves_selection_and_recovery_pending() {
        let home = tempfile::tempdir().unwrap();
        let mut state = LocalProfileState::load(home.path()).unwrap();
        let bytes = std::fs::read(&state.path).unwrap();
        let before = state.status().unwrap().active.unwrap().principal;
        let blocker = home.path().join("not-a-directory");
        std::fs::write(&blocker, "sentinel").unwrap();
        state.path = blocker.join("local-profiles.yml");
        assert!(state.import(&legacy(41, "41")).is_err());
        let status = state.status().unwrap();
        assert_eq!(status.active.unwrap().principal, before);
        assert!(status.metadata_recovery.pending);
        assert!(status
            .metadata_recovery
            .error
            .unwrap()
            .contains("could not be saved"));
        assert_eq!(
            std::fs::read(home.path().join("local-profiles.yml")).unwrap(),
            bytes
        );
        assert_eq!(std::fs::read_to_string(blocker).unwrap(), "sentinel");
    }
    #[test]
    fn inactive_profile_recovers_without_retargeting_selected_local_owner() {
        let home = tempfile::tempdir().unwrap();
        for id in ["41", "42"] {
            std::fs::create_dir_all(home.path().join("users").join(id)).unwrap();
        }
        let state = LocalProfileState::load(home.path()).unwrap();
        state.select("41", None).unwrap();
        let owner = state.active().unwrap().machine_owner;
        state.import(&legacy(42, "42")).unwrap();
        assert_eq!(state.active().unwrap().principal.as_str(), "41");
        assert_eq!(state.active().unwrap().machine_owner, owner);
        state.select("42", None).unwrap();
        assert_eq!(
            state.active().unwrap().machine_owner,
            Some(MachineOwner::LegacyGithub { id: 42 })
        );
    }
    #[test]
    fn revoked_access_survives_empty_and_inactive_metadata_recovery() {
        for metadata in [None, Some(legacy(42, "42"))] {
            let home = tempfile::tempdir().unwrap();
            for id in ["41", "42"] {
                std::fs::create_dir_all(home.path().join("users").join(id)).unwrap();
            }
            let state = LocalProfileState::load(home.path()).unwrap();
            state.select("41", None).unwrap();
            let lease = state.snapshot().unwrap().lease();
            state.revoke();
            state.recover_metadata(Ok(metadata.clone())).unwrap();
            assert!(state.snapshot().is_err());
            assert!(lease.validate().is_err());
            let status = state.status().unwrap();
            assert_eq!(status.active.unwrap().principal.as_str(), "41");
            assert!(!status.metadata_recovery.pending);
            if metadata.is_some() {
                assert_eq!(
                    status
                        .profiles
                        .iter()
                        .find(|p| p.principal.as_str() == "42")
                        .unwrap()
                        .machine_owner,
                    Some(MachineOwner::LegacyGithub { id: 42 })
                );
            }
            state.select("41", None).unwrap();
            assert!(state.snapshot().unwrap().lease().validate().is_ok());
            assert!(lease.validate().is_err());
        }
    }

    #[test]
    fn identity_edit_persists_name_and_face_avatar_without_changing_the_active_principal() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("users/41")).unwrap();
        let state = LocalProfileState::load(home.path()).unwrap();
        state.select("41", None).unwrap();
        state
            .set_identity(
                "41",
                Some("Ada Lovelace".into()),
                Some(Some("builtin-funny:bear".into())),
            )
            .unwrap();
        let active = state.active().unwrap();
        assert_eq!(active.display_name, "Ada Lovelace");
        assert_eq!(
            active.identity_avatar.as_deref(),
            Some("builtin-funny:bear")
        );
        let saved = state.active().unwrap();
        drop(state);
        let reloaded = LocalProfileState::load(home.path())
            .unwrap()
            .active()
            .unwrap();
        assert_eq!(reloaded.display_name, saved.display_name);
        assert_eq!(reloaded.identity_avatar, saved.identity_avatar);
    }

    #[test]
    fn cp3_identity_edit_for_retired_profile_is_rejected_without_retargeting() {
        let home = tempfile::tempdir().unwrap();
        for id in ["41", "42"] {
            std::fs::create_dir_all(home.path().join("users").join(id)).unwrap();
        }
        let state = LocalProfileState::load(home.path()).unwrap();
        state.select("41", None).unwrap();
        state.select("42", None).unwrap();
        let result = state.set_identity("41", Some("A's name".into()), None);
        assert!(result.is_err());
        assert_eq!(state.active().unwrap().principal.as_str(), "42");
        assert_ne!(state.active().unwrap().display_name, "A's name");
    }

    #[test]
    fn identity_edit_can_clear_or_keep_the_avatar() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("users/41")).unwrap();
        let state = LocalProfileState::load(home.path()).unwrap();
        state.select("41", None).unwrap();
        state
            .set_identity("41", None, Some(Some("builtin-funny:bear".into())))
            .unwrap();
        assert_eq!(
            state.active().unwrap().identity_avatar.as_deref(),
            Some("builtin-funny:bear"),
        );
        // None keeps the stored value.
        state.set_identity("41", Some("Ada".into()), None).unwrap();
        let a = state.active().unwrap();
        assert_eq!(a.display_name, "Ada");
        assert_eq!(a.identity_avatar.as_deref(), Some("builtin-funny:bear"));
        // Some(None) clears it.
        state.set_identity("41", None, Some(None)).unwrap();
        assert!(state.active().unwrap().identity_avatar.is_none());
    }

    #[test]
    fn identity_avatar_input_face_refs_are_validated() {
        assert!(resolve_identity_avatar(IdentityAvatarInput::Pack {
            reference: "not a ref".into()
        })
        .is_err());
        assert_eq!(
            resolve_identity_avatar(IdentityAvatarInput::Pack {
                reference: "builtin-funny:bear".into()
            })
            .unwrap()
            .as_deref(),
            Some("builtin-funny:bear"),
        );
        assert!(resolve_identity_avatar(IdentityAvatarInput::Clear)
            .unwrap()
            .is_none());
    }

    #[test]
    fn identity_avatar_input_custom_bytes_are_sniffed_and_capped() {
        let too_big = vec![0u8; MAX_IDENTITY_AVATAR_BYTES + 1];
        assert!(resolve_identity_avatar(IdentityAvatarInput::Custom { bytes: too_big }).is_err());

        let bogus = b"not an image".to_vec();
        assert!(resolve_identity_avatar(IdentityAvatarInput::Custom { bytes: bogus }).is_err());

        // A 1x1 PNG.
        let png: &[u8] = &[
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H',
            b'D', b'R', 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, b'I', b'D', b'A', b'T', 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82,
        ];
        let out = resolve_identity_avatar(IdentityAvatarInput::Custom {
            bytes: png.to_vec(),
        })
        .unwrap()
        .unwrap();
        assert!(out.starts_with("data:image/png;base64,"));

        // SVG runs through the sanitiser.
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 4 4\"><script>x()</script><rect width=\"4\" height=\"4\"/></svg>".to_vec();
        let out = resolve_identity_avatar(IdentityAvatarInput::Custom { bytes: svg })
            .unwrap()
            .unwrap();
        assert!(out.starts_with("data:image/svg+xml;base64,"));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(out.trim_start_matches("data:image/svg+xml;base64,"))
            .unwrap();
        assert!(!String::from_utf8(decoded).unwrap().contains("script"));
    }

    #[test]
    fn revoked_access_survives_matching_missing_owner_recovery_until_explicit_setup() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("users/41")).unwrap();
        let state = LocalProfileState::load(home.path()).unwrap();
        assert!(state.active().unwrap().machine_owner.is_none());
        state.revoke();
        state.import(&legacy(41, "41")).unwrap();
        assert!(state.snapshot().is_err());
        assert_eq!(
            state.active().unwrap().machine_owner,
            Some(MachineOwner::LegacyGithub { id: 41 })
        );
        // Durable metadata is recovered even though this session remains revoked.
        let saved: Profiles = crate::shared::yaml_io::read_yaml_or_default(&state.path).unwrap();
        assert!(saved.recovery_complete);
        assert_eq!(
            saved.profiles["41"].machine_owner,
            Some(MachineOwner::LegacyGithub { id: 41 })
        );
        state.select("41", None).unwrap();
        let lease = state.snapshot().unwrap().lease();
        state.revoke();
        state.import(&legacy(41, "41")).unwrap();
        assert!(state.snapshot().is_err());
        assert!(lease.validate().is_err());
        state.select("41", None).unwrap();
        assert!(state.snapshot().unwrap().lease().validate().is_ok());
        assert!(lease.validate().is_err());
    }
}
