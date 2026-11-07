// SPDX-License-Identifier: MIT
//! Where machines are kept.
//!
//! A machine is two records:
//!
//! - its **declaration** — where it is and the host key pinned for it, and
//!   nothing secret or personal. It lives in a context layer's `config/`
//!   (`machines.d/<machine>.yaml`): the personal layer's — the local profile's
//!   own directory, outside every context repo, so it follows the profile into
//!   every org — or one team's, where the team's normal sync publishes it to
//!   every teammate;
//! - each person's **binding** — how *they* sign in to it (their user name,
//!   key file, keychain handle), whether they hid it, and the terminal-server
//!   identity they last observed. Bindings live in the user's own directory,
//!   outside every context repo, so none ever syncs. Each binding keeps the
//!   publisher-verified identity its person accepted.

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Private connection admission is not wired yet.")
)]
pub mod private;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::context_repo::{path_to_org_root, path_to_team_config_dir, path_to_user_home};
use crate::domain::{CredentialRef, Principal};

pub(super) const IDENTITY_CHANGED: &str = "Machine identity changed in team config";

/// The directory of declarations inside a layer's `config/`.
const DECLARATIONS_DIR: &str = "machines.d";

/// The user's bindings file, in their own directory.
const BINDINGS_FILE: &str = "machine-bindings.json";

/// The user's ledger of issued passphrase handles, in their own directory.
const ISSUED_FILE: &str = "machine-passphrases.json";

#[cfg(test)]
thread_local! {
    static FAIL_DECLARATION_WRITES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static FAIL_BINDING_UPDATE_ON: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn fail_on_countdown(cell: &'static std::thread::LocalKey<std::cell::Cell<usize>>) -> bool {
    cell.with(|remaining| {
        let value = remaining.get();
        if value == 0 {
            false
        } else {
            remaining.set(value - 1);
            value == 1
        }
    })
}

/// Where a declaration lives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Scope {
    Personal,
    #[serde(rename_all = "camelCase")]
    Team {
        org_slug: String,
        team: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileRevision {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub modified_nanos: u128,
    pub len: u64,
    pub contents: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegistryRevision {
    pub declaration: Option<FileRevision>,
    pub bindings: Option<FileRevision>,
}

/// A machine's shareable half. It never holds a user name, a key, a
/// passphrase or any other credential: a teammate who reads it signs in as
/// themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    pub machine_id: String,
    pub label: String,
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub jump_host: Option<String>,
    #[serde(default)]
    pub default_base_dir: Option<String>,
    /// The machine's public host key (`"<type> <base64>"`), pinned when a
    /// person trusted its fingerprint. `None` until the machine is set up.
    #[serde(default)]
    pub pinned_host_key: Option<String>,
    /// The terminal-server identity verified by the publisher. It is public
    /// machine identity, not a credential. Each person's binding records
    /// the values they accepted before connecting.
    #[serde(default)]
    pub pinned_server_id: Option<String>,
}

/// A person's own half of a machine. Never synced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Binding {
    pub username: Option<String>,
    pub key_path: Option<String>,
    /// The keychain entry holding the key's passphrase.
    pub credential_ref: Option<CredentialRef>,
    pub auto_connect: bool,
    /// The local-only Hide.
    pub hidden: bool,
    /// Whether this person finished setting the machine up (its checklist
    /// passed and it connected). Until then it reads "Needs setup".
    pub set_up: bool,
    /// The identity this person accepted, independent of edits in team git.
    pub accepted_identity: Option<AcceptedIdentity>,
    /// Last terminal server this person observed. Diagnostic only.
    pub observed_server_id: Option<String>,
}

/// Labels and working folders can change without changing who receives the
/// person's connection, so acceptance covers only connection identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedIdentity {
    pub host: String,
    pub port: u16,
    pub jump_host: Option<String>,
    pub host_key: Option<String>,
    pub server_id: Option<String>,
}

impl From<&Declaration> for AcceptedIdentity {
    fn from(declaration: &Declaration) -> Self {
        Self {
            host: declaration.host.clone(),
            port: declaration.port,
            jump_host: declaration.jump_host.clone(),
            host_key: declaration.pinned_host_key.clone(),
            server_id: declaration.pinned_server_id.clone(),
        }
    }
}

impl Binding {
    pub fn validate_declaration(&self, declaration: &Declaration) -> Result<(), String> {
        if let Some(accepted) = &self.accepted_identity {
            private::validate_binding_declaration(
                accepted,
                &AcceptedIdentity::from(declaration),
                IDENTITY_CHANGED,
            )?;
            if let (Some(expected), Some(observed)) =
                (&accepted.server_id, &self.observed_server_id)
            {
                private::validate_binding_declaration(expected, observed, IDENTITY_CHANGED)?;
            }
        }
        Ok(())
    }
}

/// A machine id: lowercase words, a dash, and a random suffix, so two
/// people's (or teams') machines never share one.
pub fn new_machine_id(label: &str) -> String {
    let mut slug: String = label
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    let slug: String = if slug.is_empty() { "machine" } else { slug }
        .chars()
        .take(32)
        .collect();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("{}-{}", slug.trim_end_matches('-'), &suffix[..6])
}

/// Whether `id` can name a machine's files: the shape [`new_machine_id`]
/// produces, and never a path.
pub fn is_machine_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && !id.starts_with(['-', '.'])
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A path component from the app (a team, one segment of an org slug): never
/// a traversal.
fn check_segment(what: &str, value: &str) -> Result<(), String> {
    let ok =
        !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\', '\0']);
    if ok {
        Ok(())
    } else {
        Err(format!("{what} {value:?} is not valid"))
    }
}

/// One user's machines on disk.
#[derive(Debug, Clone)]
pub struct Registry {
    home: PathBuf,
    principal: Principal,
}

/// One read of every layer's declaration files.
#[derive(Debug, Default)]
pub(crate) struct DeclarationScan {
    /// Readable declarations, sorted by id.
    pub found: Vec<(Scope, Declaration)>,
    /// Machine ids whose file exists but can't be read, with why.
    pub unreadable: BTreeMap<String, String>,
}

/// A machine file that exists but can't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnreadableFile {
    pub machine_id: String,
    pub path: PathBuf,
    pub error: String,
}

impl Registry {
    pub(crate) fn file_revision(path: &Path) -> Result<Option<FileRevision>, String> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("stat {path:?}: {error}")),
        };
        let modified_nanos = metadata
            .modified()
            .map_err(|error| format!("modified time {path:?}: {error}"))?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("modified time {path:?}: {error}"))?
            .as_nanos();
        let contents = std::fs::read(path).map_err(|error| format!("read {path:?}: {error}"))?;
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;
        Ok(Some(FileRevision {
            path: path.to_path_buf(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(not(unix))]
            device: 0,
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(not(unix))]
            inode: 0,
            modified_nanos,
            len: metadata.len(),
            contents,
        }))
    }

    pub(crate) fn manager_revision(
        &self,
        scope: Option<&Scope>,
        machine_id: &str,
    ) -> Result<RegistryRevision, String> {
        let declaration = match scope {
            Some(scope) => {
                Self::file_revision(&self.dir(scope)?.join(format!("{machine_id}.yaml")))?
            }
            None => None,
        };
        Ok(RegistryRevision {
            declaration,
            bindings: Self::file_revision(&self.user_file(BINDINGS_FILE))?,
        })
    }
    #[cfg(test)]
    pub(crate) fn fail_next_declaration_write_for_test(&self) {
        FAIL_DECLARATION_WRITES.with(|remaining| remaining.set(1));
    }

    #[cfg(test)]
    pub(crate) fn fail_binding_update_on_for_test(&self, call: usize) {
        FAIL_BINDING_UPDATE_ON.with(|remaining| remaining.set(call));
    }
    /// The registry of `principal` under the CodeSense home `home`.
    pub fn new(home: PathBuf, principal: Principal) -> Self {
        Self { home, principal }
    }

    /// The layer directory holding `scope`'s declarations.
    pub fn dir(&self, scope: &Scope) -> Result<PathBuf, String> {
        match scope {
            // The profile's own machines: per profile, never per org, so a personal
            // machine is there in every org (SLW-PT-19, founder decision).
            Scope::Personal => Ok(self.user_file(DECLARATIONS_DIR)),
            Scope::Team { org_slug, team } => {
                org_slug
                    .split('/')
                    .try_for_each(|segment| check_segment("org", segment))?;
                check_segment("team", team)?;
                let org_root = path_to_org_root(&self.home, &self.principal, org_slug)?;
                Ok(path_to_team_config_dir(&org_root, team).join(DECLARATIONS_DIR))
            }
        }
    }

    /// Whether this user can publish to `scope`'s layer: the personal one
    /// always; a team's when its context is on this machine. Whoever can
    /// publish to a team's context governs its machines — no roles.
    pub fn can_publish(&self, scope: &Scope) -> bool {
        match scope {
            Scope::Personal => true,
            Scope::Team { .. } => self
                .dir(scope)
                .ok()
                .and_then(|dir| dir.parent()?.parent().map(Path::is_dir))
                .unwrap_or(false),
        }
    }

    /// The declarations in `scope`'s layer, sorted by id. An unreadable or
    /// malformed file is skipped with a warning: one bad declaration never
    /// hides the rest.
    pub fn declarations(&self, scope: &Scope) -> Result<Vec<Declaration>, String> {
        let dir = self.dir(scope)?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("read {dir:?}: {e}")),
        };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".yaml"))
            else {
                continue;
            };
            match read_declaration(&path) {
                Ok(decl) if decl.machine_id == stem && is_machine_id(stem) => found.push(decl),
                Ok(_) => {
                    log::warn!(target: "machines", "skipping {path:?}: its id is not its name")
                }
                Err(e) => log::warn!(target: "machines", "skipping {path:?}: {e}"),
            }
        }
        found.sort_by(|a, b| a.machine_id.cmp(&b.machine_id));
        Ok(found)
    }

    /// The machine files in `scope`'s layer that exist but can't be used: not
    /// readable, not a declaration, or naming another machine. Sorted by id.
    pub(crate) fn unreadable_files(&self, scope: &Scope) -> Result<Vec<UnreadableFile>, String> {
        let dir = self.dir(scope)?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("read {dir:?}: {e}")),
        };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(stem) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".yaml"))
                .filter(|stem| is_machine_id(stem))
            else {
                continue;
            };
            let error = match read_declaration(&path) {
                Ok(decl) if decl.machine_id == stem => continue,
                Ok(_) => "its id is not its name".to_string(),
                Err(e) => e,
            };
            found.push(UnreadableFile {
                machine_id: stem.to_string(),
                path,
                error,
            });
        }
        found.sort_by(|a, b| a.machine_id.cmp(&b.machine_id));
        Ok(found)
    }

    /// Machine folders the personal layer used before it moved into the
    /// profile's own directory: each org's scratch space. Nothing reads them
    /// now; these are the ones that still hold machine files.
    pub(crate) fn stranded_personal_machine_dirs(&self) -> Vec<PathBuf> {
        use crate::context_repo::scratch_space;
        let orgs = scratch_space::scratch_root(&self.home, &self.principal).join("orgs");
        let subdirs = |dir: &Path| -> Vec<(String, PathBuf)> {
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
                .collect()
        };
        let holds_machines = |dir: &Path| {
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .any(|entry| entry.path().extension().is_some_and(|ext| ext == "yaml"))
        };
        let mut stranded = Vec::new();
        for (owner, owner_dir) in subdirs(&orgs) {
            for (repo, _) in subdirs(&owner_dir) {
                let Ok(space) = scratch_space::org_space_at(
                    &self.home,
                    &self.principal,
                    &format!("{owner}/{repo}"),
                ) else {
                    continue;
                };
                let config = space.config_dir();
                for dir in [
                    config.join(DECLARATIONS_DIR),
                    config.join("machines-private-v1.d"),
                ] {
                    if holds_machines(&dir) {
                        stranded.push(dir);
                    }
                }
            }
        }
        stranded.sort();
        stranded
    }

    /// Write `decl` into `scope`'s layer.
    pub fn write(&self, scope: &Scope, decl: &Declaration) -> Result<(), String> {
        #[cfg(test)]
        if fail_on_countdown(&FAIL_DECLARATION_WRITES) {
            return Err("injected declaration write failure".into());
        }
        if !is_machine_id(&decl.machine_id) {
            return Err(format!("{:?} is not a machine id", decl.machine_id));
        }
        static DECLARATION_FILES: std::sync::Mutex<()> = std::sync::Mutex::new(());
        static STAGE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let _held = DECLARATION_FILES.lock().unwrap_or_else(|e| e.into_inner());
        let dir = self.dir(scope)?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("create {dir:?}: {e}"))?;
        let text = serde_yaml::to_string(decl).map_err(|e| e.to_string())?;
        let target = dir.join(format!("{}.yaml", decl.machine_id));
        let staged = dir.join(format!(
            ".{}.yaml.incoming-{}-{}",
            decl.machine_id,
            std::process::id(),
            STAGE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let result = std::fs::write(&staged, text)
            .and_then(|()| std::fs::rename(&staged, &target))
            .map_err(|e| format!("write {target:?}: {e}"));
        if result.is_err() {
            let _ = std::fs::remove_file(&staged);
        }
        result
    }

    /// Delete `machine_id`'s declaration from `scope`'s layer.
    pub fn delete(&self, scope: &Scope, machine_id: &str) -> Result<(), String> {
        if !is_machine_id(machine_id) {
            return Err(format!("{machine_id:?} is not a machine id"));
        }
        let path = self.dir(scope)?.join(format!("{machine_id}.yaml"));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("remove {path:?}: {e}")),
        }
    }

    /// Every layer on this machine that may hold declarations: the personal
    /// one first, then each team of each org.
    fn scopes(&self) -> Vec<Scope> {
        self.scopes_with_directory(DECLARATIONS_DIR)
    }

    fn scopes_with_directory(&self, directory: &str) -> Vec<Scope> {
        let mut scopes = vec![Scope::Personal];
        let orgs = path_to_user_home(&self.home, &self.principal).join("orgs");
        let dirs = |path: &Path| -> Vec<(String, PathBuf)> {
            let mut out: Vec<(String, PathBuf)> = std::fs::read_dir(path)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().is_dir())
                .filter_map(|e| Some((e.file_name().to_str()?.to_string(), e.path())))
                .filter(|(name, _)| !name.starts_with(['.', '_']))
                .collect();
            out.sort();
            out
        };
        for (owner, owner_dir) in dirs(&orgs) {
            for (repo, repo_dir) in dirs(&owner_dir) {
                for (team, _) in dirs(&repo_dir) {
                    let scope = Scope::Team {
                        org_slug: format!("{owner}/{repo}"),
                        team,
                    };
                    if self
                        .dir(&scope)
                        .map(|d| {
                            d.parent()
                                .is_some_and(|parent| parent.join(directory).is_dir())
                        })
                        .unwrap_or(false)
                    {
                        scopes.push(scope);
                    }
                }
            }
        }
        scopes
    }

    /// Every layer's declarations, first layer winning per id. A file that
    /// can't be read is skipped and logged (as [`Self::declarations`] does),
    /// and its id reported, so one bad file never hides the rest and a
    /// reconcile never mistakes it for a removal.
    pub(crate) fn all_declarations(&self) -> Result<DeclarationScan, String> {
        let mut declarations = BTreeMap::new();
        let mut unreadable = BTreeMap::new();
        for scope in self.scopes() {
            let dir = self.dir(&scope)?;
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("read {dir:?}: {error}")),
            };
            for entry in entries {
                let entry = entry.map_err(|error| format!("read entry in {dir:?}: {error}"))?;
                let path = entry.path();
                let Some(stem) = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_suffix(".yaml"))
                else {
                    continue;
                };
                let problem = match read_declaration(&path) {
                    Ok(declaration) if declaration.machine_id == stem && is_machine_id(stem) => {
                        declarations
                            .entry(declaration.machine_id.clone())
                            .or_insert_with(|| (scope.clone(), declaration));
                        continue;
                    }
                    Ok(_) => "declaration id does not match its file name".to_string(),
                    Err(error) => error,
                };
                log::warn!(target: "machines", "skipping {path:?}: {problem}");
                if is_machine_id(stem) {
                    unreadable
                        .entry(stem.to_string())
                        .or_insert_with(|| format!("{path:?}: {problem}"));
                }
            }
        }
        Ok(DeclarationScan {
            found: declarations.into_values().collect(),
            unreadable,
        })
    }

    pub(crate) fn watch_root(&self) -> PathBuf {
        path_to_user_home(&self.home, &self.principal)
    }

    /// Why `machine_id`'s declaration file exists in some layer but can't be
    /// read, if it does.
    pub(crate) fn unreadable_declaration(&self, machine_id: &str) -> Option<String> {
        if !is_machine_id(machine_id) {
            return None;
        }
        self.scopes().into_iter().find_map(|scope| {
            let path = self.dir(&scope).ok()?.join(format!("{machine_id}.yaml"));
            if !path.exists() {
                return None;
            }
            match read_declaration(&path) {
                Ok(declaration) if declaration.machine_id == machine_id => None,
                Ok(_) => Some(format!("{path:?}: its id is not its name")),
                Err(error) => Some(format!("{path:?}: {error}")),
            }
        })
    }

    /// Find `machine_id`'s declaration in any layer.
    pub fn find(&self, machine_id: &str) -> Result<Option<(Scope, Declaration)>, String> {
        if !is_machine_id(machine_id) {
            return Ok(None);
        }
        for scope in self.scopes() {
            if let Some(decl) = self
                .declarations(&scope)?
                .into_iter()
                .find(|d| d.machine_id == machine_id)
            {
                return Ok(Some((scope, decl)));
            }
        }
        Ok(None)
    }

    fn user_file(&self, name: &str) -> PathBuf {
        path_to_user_home(&self.home, &self.principal).join(name)
    }

    /// Every binding of this user, by machine id.
    pub fn bindings(&self) -> Result<BTreeMap<String, Binding>, String> {
        let bindings: BTreeMap<String, Binding> = read_json(&self.user_file(BINDINGS_FILE))?;
        let adoption = bindings
            .iter()
            .filter(|(_, b)| b.set_up && b.accepted_identity.is_none())
            .map(|(id, _)| {
                self.find(id)
                    .map(|found| found.map(|(_, d)| (id.clone(), AcceptedIdentity::from(&d))))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        if adoption.is_empty() {
            return Ok(bindings);
        }
        // Persist once under the existing binding lock, without overwriting a
        // concurrent setup or confirmation.
        self.update_bindings(|all| {
            for (id, identity) in adoption {
                if let Some(binding) = all.get_mut(&id) {
                    if binding.set_up && binding.accepted_identity.is_none() {
                        binding.accepted_identity = Some(identity);
                    }
                }
            }
            all.clone()
        })
    }

    /// Change this user's bindings with `change`, atomically on disk and one
    /// change at a time.
    pub fn update_bindings<T>(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, Binding>) -> T,
    ) -> Result<T, String> {
        #[cfg(test)]
        if fail_on_countdown(&FAIL_BINDING_UPDATE_ON) {
            return Err("injected binding update failure".into());
        }
        update_json(&self.user_file(BINDINGS_FILE), change)
    }

    /// Change the ledger of keychain handles this user was given for
    /// passphrases, with when (seconds since the epoch) — so one the panel
    /// dropped before binding it can be found and forgotten.
    pub fn update_issued_passphrases<T>(
        &self,
        change: impl FnOnce(&mut BTreeMap<String, u64>) -> T,
    ) -> Result<T, String> {
        update_json(&self.user_file(ISSUED_FILE), change)
    }
}

/// Serializes every read-change-write of the user's machine files: commands
/// run concurrently, and a lost update could drop a binding or a pin.
static USER_FILES: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("read {path:?}: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(format!("read {path:?}: {e}")),
    }
}

fn update_json<V, T>(path: &Path, change: impl FnOnce(&mut V) -> T) -> Result<T, String>
where
    V: serde::de::DeserializeOwned + Serialize + Default,
{
    use std::os::unix::fs::PermissionsExt;
    let _held = USER_FILES.lock().unwrap_or_else(|e| e.into_inner());
    let mut value: V = read_json(path)?;
    let out = change(&mut value);
    let dir = path.parent().ok_or("no user directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {dir:?}: {e}"))?;
    let staged = path.with_extension("json.incoming");
    let text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
    std::fs::write(&staged, text)
        .and_then(|()| std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600)))
        .and_then(|()| std::fs::rename(&staged, path))
        .map_err(|e| format!("write {path:?}: {e}"))?;
    Ok(out)
}

/// Keys a layer's declaration must never carry, at any depth: anything
/// naming a user, key, passphrase or credential. A declaration that does is
/// refused, so a secret committed into a team's context is surfaced, never read.
const SECRET_KEYS: [&str; 10] = [
    "user",
    "pass",
    "pw",
    "secret",
    "credential",
    "token",
    "identity",
    "private",
    "key_path",
    "auth",
];

/// The first key under `value`, at any depth, that names a secret.
fn secret_key(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::Mapping(map) => map.iter().find_map(|(key, value)| {
            let key = match key {
                serde_yaml::Value::Tagged(tagged) => &tagged.value,
                key => key,
            };
            let name = key.as_str().unwrap_or_default().to_ascii_lowercase();
            if SECRET_KEYS.iter().any(|secret| name.contains(secret)) {
                Some(name)
            } else {
                secret_key(value)
            }
        }),
        serde_yaml::Value::Sequence(items) => items.iter().find_map(secret_key),
        serde_yaml::Value::Tagged(tagged) => secret_key(&tagged.value),
        _ => None,
    }
}

fn read_declaration(path: &Path) -> Result<Declaration, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let raw: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|e| e.to_string())?;
    if let Some(key) = secret_key(&raw) {
        return Err(format!("refused: a declaration may not carry {key:?}"));
    }
    let decl: Declaration = serde_yaml::from_value(raw).map_err(|e| e.to_string())?;
    codesense_session_server::ssh::Address::new(
        &decl.host,
        decl.port,
        None,
        decl.jump_host.as_deref(),
    )?;
    if let Some(key) = &decl.pinned_host_key {
        codesense_session_server::ssh::HostKey::parse(key)?;
    }
    Ok(decl)
}

#[cfg(test)]
mod stranded_tests {
    use super::*;

    #[test]
    fn machine_files_left_in_an_orgs_scratch_space_are_found() {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(41u64);
        let registry = Registry::new(home.path().to_path_buf(), principal.clone());
        assert!(registry.stranded_personal_machine_dirs().is_empty());

        let space =
            crate::context_repo::scratch_space::org_space_at(home.path(), &principal, "acme/ctx")
                .unwrap();
        let old = space.config_dir().join("machines-private-v1.d");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(space.config_dir().join(DECLARATIONS_DIR)).unwrap();
        assert!(
            registry.stranded_personal_machine_dirs().is_empty(),
            "empty folders are not news"
        );
        std::fs::write(old.join("droplet.yaml"), "machine: {}").unwrap();
        assert_eq!(registry.stranded_personal_machine_dirs(), vec![old]);
    }
}
