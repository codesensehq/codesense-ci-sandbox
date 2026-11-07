// SPDX-License-Identifier: MIT
//! Versioned private discovery is never visible to legacy registry readers.

pub mod target;

use super::*;
use crate::commands::auth::private_authority::{PrivateAuthorityLease, PrivateAuthoritySnapshot};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

const PRIVATE_DIR: &str = "machines-private-v1.d";
const PRIVATE_FILE: &str = "machine-private-bindings-v1.json";
const SCHEMA: &str = "codesense.machine.private-account/v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateMachine {
    pub machine_id: String,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub jump_host: Option<String>,
    pub default_base_dir: Option<String>,
    pub pinned_host_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateDeclaration {
    schema: String,
    pub machine: PrivateMachine,
}

impl PrivateDeclaration {
    /// Only a new, unpinned declaration can enter private mode this way.
    /// Migration separately preserves the old declaration and its daemon pin.
    pub fn from_legacy(declaration: Declaration) -> Result<Self, String> {
        if declaration.pinned_server_id.is_some() {
            return Err("legacy daemon pins cannot become private authority".into());
        }
        let result = Self {
            schema: SCHEMA.into(),
            machine: PrivateMachine {
                machine_id: declaration.machine_id,
                label: declaration.label,
                host: declaration.host,
                port: declaration.port,
                jump_host: declaration.jump_host,
                default_base_dir: declaration.default_base_dir,
                pinned_host_key: declaration.pinned_host_key,
            },
        };
        result.validate()?;
        Ok(result)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA
            || !is_machine_id(&self.machine.machine_id)
            || self.machine.host.is_empty()
            || self.machine.port == 0
        {
            return Err("invalid private machine declaration".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateAccount {
    pub uid: u32,
    pub canonical_home: String,
    pub root_version: u32,
    pub enrollment_id: String,
}

impl PrivateAccount {
    fn validate(&self) -> Result<(), String> {
        let home = Path::new(&self.canonical_home);
        let canonical: PathBuf = home.components().collect();
        let enrollment =
            uuid::Uuid::parse_str(&self.enrollment_id).map_err(|_| "invalid private enrollment")?;
        if self.root_version != 1
            || !home.is_absolute()
            || canonical.as_os_str() != home.as_os_str()
            || home.components().any(|c| {
                !matches!(
                    c,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            })
            || self.canonical_home.contains('\0')
            || enrollment.is_nil()
        {
            return Err("invalid private account binding".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PrivateSetup {
    NeedsEnrollment,
    Enrolled {
        account: PrivateAccount,
    },
    Ready {
        account: PrivateAccount,
        server_id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateBinding {
    pub username: Option<String>,
    pub key_path: Option<String>,
    pub credential_ref: Option<CredentialRef>,
    pub setup: PrivateSetup,
    pub host_key: Option<String>,
    pub declaration_revision: String,
    pub revision: u64,
    pub auto_connect: bool,
    #[serde(default)]
    pub remote_mcp_control: bool,
    #[serde(default)]
    pub hidden: bool,
    /// Background reconnect a switched legacy machine had: a binding may only
    /// reconnect once Ready, so it is restored then and this is cleared.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub restore_auto_connect: bool,
    /// The machine held open terminal tabs when last seen, so it reconnects
    /// at launch. Kept by the connection manager from the host's layout.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open_tabs: bool,
}

impl PrivateBinding {
    fn validate(&self, _id: &str) -> Result<(), String> {
        if self.revision == 0
            || !valid_digest(&self.declaration_revision)
            || self.credential_ref.as_ref().is_some_and(|c| {
                c.provider != "ssh" || c.account.is_empty() || c.account.len() > 128
            })
        {
            return Err("invalid private machine binding".into());
        }
        if !matches!(self.setup, PrivateSetup::NeedsEnrollment)
            && self.host_key.as_ref().is_none_or(|key| key.is_empty())
        {
            return Err("private host identity is incomplete".into());
        }
        match &self.setup {
            PrivateSetup::NeedsEnrollment => {
                if self.auto_connect || self.remote_mcp_control {
                    return Err("private setup is incomplete".into());
                }
            }
            PrivateSetup::Enrolled { account } => {
                account.validate()?;
                if self.auto_connect || self.remote_mcp_control {
                    return Err("private setup is incomplete".into());
                }
            }
            PrivateSetup::Ready { account, server_id } => {
                account.validate()?;
                if server_id.is_empty()
                    || server_id.contains('\0')
                    || self.host_key.as_ref().is_none_or(|key| key.is_empty())
                {
                    return Err("private identity is incomplete".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Refusal {
    transaction_id: String,
    declaration_revision: String,
    published: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonalState {
    version: u32,
    bindings: BTreeMap<String, PrivateBinding>,
    binding_generations: BTreeMap<String, u64>,
    migrated_mode_refusals: BTreeMap<String, Refusal>,
}

impl Default for PersonalState {
    fn default() -> Self {
        Self {
            version: 1,
            bindings: BTreeMap::new(),
            binding_generations: BTreeMap::new(),
            migrated_mode_refusals: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub enum PrivateEntry {
    Ready(Scope, Box<PrivateDeclaration>, String),
    Refused,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MigrationBackup {
    declaration: Vec<u8>,
    personal_bindings: Option<Vec<u8>>,
}

pub struct PrivateMigration {
    scope: Scope,
    declaration: PrivateDeclaration,
    backup: MigrationBackup,
    transaction_id: String,
    principal_root: PathBuf,
    lease: PrivateAuthorityLease,
}

pub struct PrivateRegistry {
    legacy: Registry,
    lease: PrivateAuthorityLease,
}

impl PrivateRegistry {
    pub fn new(legacy: Registry, authority: &PrivateAuthoritySnapshot) -> Result<Self, String> {
        let lease = authority.lease();
        lease.validate()?;
        if legacy.principal != authority.principal() {
            return Err("private registry principal mismatch".into());
        }
        Ok(Self { legacy, lease })
    }

    pub fn dir(&self, scope: &Scope) -> Result<PathBuf, String> {
        Self::dir_for(&self.legacy, scope)
    }

    pub(crate) fn dir_for(registry: &Registry, scope: &Scope) -> Result<PathBuf, String> {
        let legacy = registry.dir(scope)?;
        Ok(legacy
            .parent()
            .ok_or("missing configuration directory")?
            .join(PRIVATE_DIR))
    }

    fn write(&self, path: &Path, bytes: &[u8], exclusive: bool) -> Result<(), String> {
        let staged = PreparedWrite::new(path, bytes, exclusive)?;
        // Only this immutable, principal-bound operation may finish after retirement.
        // The commit gate does no filesystem work and cannot authorize later writes.
        let committed = self.lease.commit(|| staged)?;
        committed.persist()?;
        self.lease.validate()
    }

    fn strict_scopes(&self) -> Result<Vec<Scope>, String> {
        Self::scopes(&self.legacy)
    }
    fn scopes(legacy: &Registry) -> Result<Vec<Scope>, String> {
        let mut scopes = vec![Scope::Personal];
        let orgs = path_to_user_home(&legacy.home, &legacy.principal).join("orgs");
        for (owner, owner_dir) in directories(&orgs)? {
            for (repo, repo_dir) in directories(&owner_dir)? {
                for (team, _) in directories(&repo_dir)? {
                    scopes.push(Scope::Team {
                        org_slug: format!("{owner}/{repo}"),
                        team,
                    });
                }
            }
        }
        Ok(scopes)
    }

    pub(crate) fn migration_reserved(&self, id: &str) -> Result<bool, String> {
        Self::claims_reserved(&self.legacy, id)
    }

    /// Discovery/refusal is not an operation grant and must survive auth denial.
    pub(crate) fn claims_reserved(legacy: &Registry, id: &str) -> Result<bool, String> {
        if !is_machine_id(id) {
            return Err("invalid machine id".into());
        }
        let state: PersonalState = read_json(&legacy.user_file(PRIVATE_FILE))?;
        if state.version != 1 {
            return Err("unsupported private binding schema".into());
        }
        if state.migrated_mode_refusals.contains_key(id) || state.bindings.contains_key(id) {
            return Ok(true);
        }
        for scope in Self::scopes(legacy)? {
            if yaml_files(
                &legacy
                    .dir(&scope)?
                    .parent()
                    .ok_or("missing configuration directory")?
                    .join(PRIVATE_DIR),
            )?
            .iter()
            .any(|p| p.file_stem().and_then(|s| s.to_str()) == Some(id))
            {
                return Ok(true);
            }
        }
        let backups = legacy.user_file("machine-private-migrations-v1");
        match std::fs::read_dir(backups) {
            Ok(entries) => {
                for entry in entries {
                    let name = entry.map_err(|e| e.to_string())?.file_name();
                    let name = name.to_str().ok_or("invalid migration backup name")?;
                    if name.starts_with(&format!("{id}.")) {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }

    fn personal_path(&self) -> PathBuf {
        self.legacy.user_file(PRIVATE_FILE)
    }

    fn lock(&self) -> Result<File, String> {
        self.lease.validate()?;
        let path = self.legacy.user_file("machine-private-v1.lock");
        let parent = path.parent().ok_or("missing principal directory")?;
        create_directory_durable(parent)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .map_err(|e| e.to_string())?;
        // A concurrent SSH fork can retain this close-on-exec descriptor until
        // exec, briefly keeping the prior owner's lock alive after its close.
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    std::thread::sleep(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(10)),
                    );
                }
                Err(error) => return Err(error.to_string()),
            }
        }
        self.lease.validate()?;
        Ok(file)
    }

    fn state(&self) -> Result<PersonalState, String> {
        Self::read_state(&self.legacy)
    }

    fn read_state(legacy: &Registry) -> Result<PersonalState, String> {
        let state: PersonalState = read_json(&legacy.user_file(PRIVATE_FILE))?;
        if state.version != 1 {
            return Err("unsupported private binding schema".into());
        }
        for (id, binding) in &state.bindings {
            if !is_machine_id(id) || !state.migrated_mode_refusals.contains_key(id) {
                return Err("invalid private binding inventory".into());
            }
            binding.validate(id)?;
            if state.binding_generations.get(id) != Some(&binding.revision) {
                return Err("invalid private binding generation".into());
            }
        }
        if state
            .binding_generations
            .iter()
            .any(|(id, generation)| !is_machine_id(id) || *generation == 0)
        {
            return Err("invalid private binding generations".into());
        }
        for (id, refusal) in &state.migrated_mode_refusals {
            if !is_machine_id(id)
                || uuid::Uuid::parse_str(&refusal.transaction_id).is_err()
                || !valid_digest(&refusal.declaration_revision)
            {
                return Err("invalid private refusal inventory".into());
            }
        }
        Ok(state)
    }

    fn save_state(&self, state: &PersonalState) -> Result<(), String> {
        self.lease.validate()?;
        self.write(
            &self.personal_path(),
            &serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?,
            false,
        )?;
        self.lease.validate()
    }

    pub fn inventory(&self) -> Result<BTreeMap<String, PrivateEntry>, String> {
        self.lease.validate()?;
        let entries = Self::inventory_for(&self.legacy)?;
        self.lease.validate()?;
        Ok(entries)
    }

    /// Declaration-only inventory; grants no mutation, binding or connection access.
    pub fn inventory_for(legacy: &Registry) -> Result<BTreeMap<String, PrivateEntry>, String> {
        let state = Self::read_state(legacy)?;
        let mut entries = BTreeMap::new();
        let mut visited_scopes = std::collections::BTreeSet::new();
        for scope in Self::scopes(legacy)? {
            // Scopes can be discovered through either directory. Visit once.
            let private_dir = Self::dir_for(legacy, &scope)?;
            if !visited_scopes.insert(private_dir.clone()) {
                continue;
            }
            for path in yaml_files(&legacy.dir(&scope)?)? {
                if let Some(id) = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|id| is_machine_id(id))
                {
                    entries.insert(id.to_string(), PrivateEntry::Refused);
                }
            }
            for path in yaml_files(&private_dir)? {
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|id| is_machine_id(id))
                    .ok_or("invalid private machine filename")?
                    .to_string();
                let parsed = std::fs::read(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| {
                        serde_yaml::from_slice::<PrivateDeclaration>(&bytes)
                            .map(|declaration| (declaration, digest(&bytes)))
                            .map_err(|e| e.to_string())
                    })
                    .and_then(|(declaration, revision)| {
                        declaration.validate()?;
                        if declaration.machine.machine_id != id {
                            return Err("private machine filename mismatch".into());
                        }
                        Ok((declaration, revision))
                    });
                match (entries.entry(id), parsed) {
                    (
                        std::collections::btree_map::Entry::Vacant(entry),
                        Ok((declaration, revision)),
                    ) => {
                        entry.insert(PrivateEntry::Ready(
                            scope.clone(),
                            Box::new(declaration),
                            revision,
                        ));
                    }
                    (entry, _) => match entry {
                        std::collections::btree_map::Entry::Vacant(e) => {
                            e.insert(PrivateEntry::Refused);
                        }
                        std::collections::btree_map::Entry::Occupied(mut e) => {
                            *e.get_mut() = PrivateEntry::Refused;
                        }
                    },
                }
            }
        }
        // A completed backup is itself a durable reservation. A crash before
        // the personal ledger publication must not revive legacy access.
        let backup_dir = legacy.user_file("machine-private-migrations-v1");
        match std::fs::read_dir(backup_dir) {
            Ok(backups) => {
                for backup in backups {
                    let name = backup.map_err(|e| e.to_string())?.file_name();
                    let name = name.to_str().ok_or("invalid migration backup name")?;
                    if name.starts_with('.') {
                        continue;
                    }
                    let (id, transaction) = name
                        .strip_suffix(".json")
                        .and_then(|name| name.rsplit_once('.'))
                        .ok_or("invalid migration backup name")?;
                    if !is_machine_id(id) || uuid::Uuid::parse_str(transaction).is_err() {
                        return Err("invalid migration backup name".into());
                    }
                    if !state
                        .migrated_mode_refusals
                        .get(id)
                        .is_some_and(|r| r.published && r.transaction_id == transaction)
                    {
                        entries.insert(id.to_string(), PrivateEntry::Refused);
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        for (id, refusal) in &state.migrated_mode_refusals {
            if !refusal.published {
                entries.insert(id.clone(), PrivateEntry::Refused);
            } else {
                entries.entry(id.clone()).or_insert(PrivateEntry::Refused);
            }
        }
        Ok(entries)
    }

    pub fn find(&self, id: &str) -> Result<Option<(Scope, PrivateDeclaration)>, String> {
        Ok(self
            .capture(id)?
            .map(|(scope, declaration, _)| (scope, declaration)))
    }

    pub(crate) fn capture(
        &self,
        id: &str,
    ) -> Result<Option<(Scope, PrivateDeclaration, String)>, String> {
        if !is_machine_id(id) {
            return Err("invalid machine id".into());
        }
        match self.inventory()?.remove(id) {
            Some(PrivateEntry::Ready(scope, declaration, revision)) => {
                Ok(Some((scope, *declaration, revision)))
            }
            Some(PrivateEntry::Refused) => {
                Err("private machine configuration requires repair".into())
            }
            None => Ok(None),
        }
    }

    pub fn publish(&self, scope: &Scope, declaration: &PrivateDeclaration) -> Result<(), String> {
        let _lock = self.lock()?;
        declaration.validate()?;
        if !self.legacy.can_publish(scope) {
            return Err("machine scope is not publishable".into());
        }
        let id = &declaration.machine.machine_id;
        if self.inventory()?.contains_key(id) {
            return Err("machine id is already reserved".into());
        }
        let bytes = serde_yaml::to_string(declaration)
            .map_err(|e| e.to_string())?
            .into_bytes();
        let mut state = self.state()?;
        state.migrated_mode_refusals.insert(
            id.clone(),
            Refusal {
                transaction_id: uuid::Uuid::new_v4().to_string(),
                declaration_revision: digest(&bytes),
                published: false,
            },
        );
        // Durable refusal precedes discovery: interrupted publication cannot
        // reactivate an old binding or declaration.
        self.save_state(&state)?;
        self.write(&self.dir(scope)?.join(format!("{id}.yaml")), &bytes, true)?;
        state
            .migrated_mode_refusals
            .get_mut(id)
            .ok_or("missing publication reservation")?
            .published = true;
        self.save_state(&state)?;
        self.find(id)?
            .ok_or("private publication requires repair")?;
        self.lease.validate()
    }

    /// First verified host key for an unconfigured draft. Refuse any re-pin.
    pub(crate) fn pin_draft(&self, id: &str, host_key: &str) -> Result<(), String> {
        let _lock = self.lock()?;
        let (scope, mut declaration, revision) =
            self.capture(id)?.ok_or("private machine not found")?;
        if let Some(key) = &declaration.machine.pinned_host_key {
            return if key == host_key {
                Ok(())
            } else {
                Err("private host key changed".into())
            };
        }
        if !self.legacy.can_publish(&scope) {
            return Err("publisher must verify this machine host key".into());
        }
        let mut state = self.state()?;
        if state
            .bindings
            .get(id)
            .is_some_and(|b| !matches!(b.setup, PrivateSetup::NeedsEnrollment))
        {
            return Err("private setup already bound".into());
        }
        declaration.machine.pinned_host_key = Some(host_key.into());
        let bytes = serde_yaml::to_string(&declaration)
            .map_err(|e| e.to_string())?
            .into_bytes();
        let next = digest(&bytes);
        let refusal = state
            .migrated_mode_refusals
            .get_mut(id)
            .ok_or("private reservation missing")?;
        if refusal.declaration_revision != revision {
            return Err("private declaration changed".into());
        }
        refusal.declaration_revision = next.clone();
        refusal.published = false;
        self.save_state(&state)?;
        self.write(&self.dir(&scope)?.join(format!("{id}.yaml")), &bytes, false)?;
        if let Some(binding) = state.bindings.get_mut(id) {
            binding.host_key = Some(host_key.into());
            binding.declaration_revision = next;
        }
        state
            .migrated_mode_refusals
            .get_mut(id)
            .ok_or("private reservation missing")?
            .published = true;
        self.save_state(&state)
    }

    pub(crate) fn remove_declaration(&self, id: &str) -> Result<Option<PrivateBinding>, String> {
        let _lock = self.lock()?;
        let (scope, _, _) = self.capture(id)?.ok_or("private machine not found")?;
        if !self.legacy.can_publish(&scope) {
            return Err("private machine is not publishable".into());
        }
        let mut state = self.state()?;
        let prior = state.bindings.remove(id);
        state
            .migrated_mode_refusals
            .get_mut(id)
            .ok_or("private reservation missing")?
            .published = false;
        self.save_state(&state)?;
        self.lease.validate()?;
        std::fs::remove_file(self.dir(&scope)?.join(format!("{id}.yaml")))
            .map_err(|e| e.to_string())?;
        self.lease.validate()?;
        Ok(prior)
    }
    pub(crate) fn unshare(&self, id: &str) -> Result<(), String> {
        let _lock = self.lock()?;
        let (scope, _, revision) = self.capture(id)?.ok_or("private machine not found")?;
        if !matches!(scope, Scope::Team { .. }) || !self.legacy.can_publish(&scope) {
            return Err("private machine is not shareable".into());
        }
        let old = self.dir(&scope)?.join(format!("{id}.yaml"));
        let bytes = std::fs::read(&old).map_err(|e| e.to_string())?;
        if digest(&bytes) != revision {
            return Err("private declaration changed".into());
        }
        self.write(
            &self.dir(&Scope::Personal)?.join(format!("{id}.yaml")),
            &bytes,
            true,
        )?;
        self.lease.validate()?;
        std::fs::remove_file(old).map_err(|e| e.to_string())?;
        self.lease.validate()
    }

    /// Capture the original bytes without deleting or altering legacy data.
    pub fn prepare_migration(&self, scope: &Scope, id: &str) -> Result<PrivateMigration, String> {
        let _lock = self.lock()?;
        if !self.legacy.can_publish(scope) {
            return Err("machine scope is not publishable".into());
        }
        if !is_machine_id(id) || self.migration_reserved(id)? {
            return Err("private migration requires explicit reconciliation".into());
        }
        let name = format!("{id}.yaml");
        let mut occurrences = 0;
        for candidate in self.strict_scopes()? {
            if self.legacy.dir(&candidate)?.join(&name).exists() {
                occurrences += 1;
            }
        }
        if occurrences != 1 {
            return Err("legacy declaration is missing or conflicting".into());
        }
        let path = self.legacy.dir(scope)?.join(&name);
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        let mut old: Declaration = serde_yaml::from_slice(&bytes).map_err(|e| e.to_string())?;
        if old.machine_id != id {
            return Err("legacy machine filename mismatch".into());
        }
        // The original pin is preserved in the backup; it is never promoted.
        old.pinned_server_id = None;
        let declaration = PrivateDeclaration::from_legacy(old)?;
        let binding_path = self.legacy.user_file(BINDINGS_FILE);
        let personal_bindings = match std::fs::read(binding_path) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        self.lease.validate()?;
        Ok(PrivateMigration {
            scope: scope.clone(),
            declaration,
            backup: MigrationBackup {
                declaration: bytes,
                personal_bindings,
            },
            transaction_id: uuid::Uuid::new_v4().to_string(),
            principal_root: self.legacy.watch_root(),
            lease: self.lease.clone(),
        })
    }

    /// Preserve, reserve, then publish exclusively. The old writer does not
    /// participate in our lock, so even an unchanged legacy file is not unlinked.
    pub fn publish_migration(&self, migration: PrivateMigration) -> Result<(), String> {
        let _lock = self.lock()?;
        migration.lease.validate()?;
        if !self.legacy.can_publish(&migration.scope) {
            return Err("machine scope is not publishable".into());
        }
        if migration.principal_root != self.legacy.watch_root() {
            return Err("private migration principal mismatch".into());
        }
        let id = &migration.declaration.machine.machine_id;
        let mut state = self.state()?;
        if self.migration_reserved(id)? {
            return Err("private migration requires explicit reconciliation".into());
        }
        let backup_path = self
            .legacy
            .user_file("machine-private-migrations-v1")
            .join(format!("{id}.{}.json", migration.transaction_id));
        self.write(
            &backup_path,
            &serde_json::to_vec(&migration.backup).map_err(|e| e.to_string())?,
            true,
        )?;
        state.migrated_mode_refusals.insert(
            id.clone(),
            Refusal {
                transaction_id: migration.transaction_id,
                declaration_revision: digest(&migration.backup.declaration),
                published: false,
            },
        );
        self.save_state(&state)?;
        let bytes = serde_yaml::to_string(&migration.declaration).map_err(|e| e.to_string())?;
        self.write(
            &self.dir(&migration.scope)?.join(format!("{id}.yaml")),
            bytes.as_bytes(),
            true,
        )?;
        self.find(id)?
            .ok_or("private migration requires reconciliation")?;
        Ok(())
    }

    /// The owner's explicit request to run a legacy machine as a
    /// private-account machine under the same id. Unlike the passive
    /// migration this reconciles: it backs up the original declaration and
    /// personal bindings, reserves the id, publishes the private declaration,
    /// removes the legacy one and completes the reservation. The host-key pin
    /// carries over; the legacy terminal-server pin does not. A crash before
    /// completion leaves the id refused with its backup intact, and asking
    /// again finishes the conversion. A legacy writer that recreates the old
    /// file afterwards meets the duplicate-id refusal, never a fallback.
    pub fn convert_legacy(&self, scope: &Scope, id: &str) -> Result<(), String> {
        let migration = if self.migration_reserved(id)? {
            None
        } else {
            Some(self.prepare_migration(scope, id)?)
        };
        let _lock = self.lock()?;
        let mut state = self.state()?;
        let legacy_path = self.legacy.dir(scope)?.join(format!("{id}.yaml"));
        let private_path = self.dir(scope)?.join(format!("{id}.yaml"));
        let backup_path = |transaction: &str| {
            self.legacy
                .user_file("machine-private-migrations-v1")
                .join(format!("{id}.{transaction}.json"))
        };
        let transaction = match migration {
            Some(migration) => {
                if self.migration_reserved(id)? {
                    return Err("private migration requires explicit reconciliation".into());
                }
                if std::fs::read(&legacy_path).map_err(|e| e.to_string())?
                    != migration.backup.declaration
                {
                    return Err("legacy declaration changed".into());
                }
                let bytes = serde_yaml::to_string(&migration.declaration)
                    .map_err(|e| e.to_string())?
                    .into_bytes();
                self.write(
                    &backup_path(&migration.transaction_id),
                    &serde_json::to_vec(&migration.backup).map_err(|e| e.to_string())?,
                    true,
                )?;
                state.migrated_mode_refusals.insert(
                    id.to_string(),
                    Refusal {
                        transaction_id: migration.transaction_id.clone(),
                        declaration_revision: digest(&bytes),
                        published: false,
                    },
                );
                self.save_state(&state)?;
                self.write(&private_path, &bytes, true)?;
                migration.transaction_id
            }
            None => {
                // Resume only this conversion's own interrupted transaction.
                let refusal = state
                    .migrated_mode_refusals
                    .get(id)
                    .filter(|refusal| !refusal.published)
                    .ok_or("private migration requires explicit reconciliation")?;
                let backup: MigrationBackup = serde_json::from_slice(
                    &std::fs::read(backup_path(&refusal.transaction_id))
                        .map_err(|_| "private migration requires explicit reconciliation")?,
                )
                .map_err(|e| e.to_string())?;
                let published = std::fs::read(&private_path)
                    .map_err(|_| "private migration requires explicit reconciliation")?;
                let legacy = match std::fs::read(&legacy_path) {
                    Ok(bytes) => Some(bytes),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => return Err(e.to_string()),
                };
                if digest(&published) != refusal.declaration_revision
                    || legacy.is_some_and(|legacy| legacy != backup.declaration)
                {
                    return Err("private migration requires explicit reconciliation".into());
                }
                refusal.transaction_id.clone()
            }
        };
        self.legacy.delete(scope, id)?;
        let refusal = state
            .migrated_mode_refusals
            .get_mut(id)
            .filter(|refusal| refusal.transaction_id == transaction)
            .ok_or("private migration requires explicit reconciliation")?;
        refusal.published = true;
        self.save_state(&state)?;
        self.find(id)?
            .ok_or("private migration requires reconciliation")?;
        self.lease.validate()
    }

    pub fn binding(&self, id: &str) -> Result<Option<PrivateBinding>, String> {
        let (_, declaration, revision) = self.capture(id)?.ok_or("private machine not found")?;
        let result = self.state()?.bindings.remove(id);
        if let Some(binding) = &result {
            validate_private_binding_declaration(binding, &declaration, &revision)?;
        }
        self.lease.validate()?;
        Ok(result)
    }

    /// Includes deleted bindings so a reopened setup can allocate the next
    /// revision. The write still compares this generation under the file lock.
    pub fn binding_generation(&self, id: &str) -> Result<u64, String> {
        self.find(id)?.ok_or("private machine not found")?;
        let generation = self
            .state()?
            .binding_generations
            .get(id)
            .copied()
            .unwrap_or(0);
        self.lease.validate()?;
        Ok(generation)
    }

    pub fn save_binding(
        &self,
        id: &str,
        binding: PrivateBinding,
        expected_revision: Option<u64>,
    ) -> Result<(), String> {
        let _lock = self.lock()?;
        binding.validate(id)?;
        let (_, declared, revision) = self.capture(id)?.ok_or("private machine not found")?;
        validate_private_binding_declaration(&binding, &declared, &revision)?;
        let mut state = self.state()?;
        if state.bindings.get(id).map(|b| b.revision) != expected_revision
            || state
                .binding_generations
                .get(id)
                .copied()
                .unwrap_or(0)
                .checked_add(1)
                != Some(binding.revision)
        {
            return Err("private binding changed".into());
        }
        state
            .migrated_mode_refusals
            .entry(id.into())
            .or_insert_with(|| Refusal {
                transaction_id: uuid::Uuid::new_v4().to_string(),
                declaration_revision: revision,
                published: true,
            });
        state
            .binding_generations
            .insert(id.into(), binding.revision);
        state.bindings.insert(id.into(), binding);
        self.save_state(&state)
    }

    pub fn remove_binding(&self, id: &str, expected_revision: u64) -> Result<(), String> {
        let _lock = self.lock()?;
        let mut state = self.state()?;
        if state.bindings.get(id).map(|b| b.revision) != Some(expected_revision) {
            return Err("private binding changed".into());
        }
        state.bindings.remove(id);
        self.save_state(&state)
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn yaml_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.map(|e| e.path()).map_err(|e| e.to_string()))
            .filter(|path| {
                path.as_ref()
                    .map(|p| p.extension().is_some_and(|e| e == "yaml"))
                    .unwrap_or(true)
            })
            .collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e.to_string()),
    }
}

fn create_directory_durable(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        return Ok(());
    }
    let parent = path.parent().ok_or("missing storage ancestor")?;
    create_directory_durable(parent)?;
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(e) => return Err(e.to_string()),
    }
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|e| e.to_string())
}

struct PreparedWrite {
    staged: tempfile::NamedTempFile,
    destination: PathBuf,
    exclusive: bool,
}

impl PreparedWrite {
    fn new(path: &Path, bytes: &[u8], exclusive: bool) -> Result<Self, String> {
        let parent = path.parent().ok_or("missing storage directory")?;
        create_directory_durable(parent)?;
        let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
        staged
            .write_all(bytes)
            .and_then(|()| staged.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        Ok(Self {
            staged,
            destination: path.into(),
            exclusive,
        })
    }

    fn persist(self) -> Result<(), String> {
        let parent = self
            .destination
            .parent()
            .ok_or("missing storage directory")?;
        if self.exclusive {
            self.staged
                .persist_noclobber(&self.destination)
                .map_err(|e| e.to_string())?;
        } else {
            self.staged
                .persist(&self.destination)
                .map_err(|e| e.to_string())?;
        }
        File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| e.to_string())
    }
}

fn validate_private_binding_declaration(
    binding: &PrivateBinding,
    declaration: &PrivateDeclaration,
    revision: &str,
) -> Result<(), String> {
    validate_binding_declaration(
        &binding.host_key,
        &declaration.machine.pinned_host_key,
        "private host pin does not match declaration",
    )?;
    validate_binding_declaration(
        revision,
        binding.declaration_revision.as_str(),
        "private declaration changed",
    )
}

pub(super) fn validate_binding_declaration<T: PartialEq + ?Sized>(
    accepted: &T,
    declared: &T,
    refusal: &str,
) -> Result<(), String> {
    if accepted != declared {
        return Err(refusal.into());
    }
    Ok(())
}

fn directories(path: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.to_string()),
    };
    let mut result = vec![];
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "invalid private scope name")?;
        if name.starts_with(['.', '_']) {
            continue;
        }
        if std::fs::metadata(entry.path())
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            result.push((name, entry.path()));
        }
    }
    result.sort();
    Ok(result)
}

#[cfg(test)]
mod tests;
