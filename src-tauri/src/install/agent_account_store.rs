// SPDX-License-Identifier: MIT
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::content_engine::{write_tmp_fsync_rename, Error};
use crate::context_repo::path_to_user_home;
use crate::domain::Principal;

pub const SCHEMA_VERSION: u32 = 1;
const ACCOUNTS_DIR: &str = "agent-accounts";
const ACTIVE_DIR: &str = "active-agent-accounts";
const HOMES_DIR: &str = "agent-account-homes";
const IDENTITY_KEY_FILE: &str = ".agent-account-identity-key";
const LOCK_FILE: &str = ".agent-accounts.lock";
const LOCK_ATTEMPTS: usize = 25;
const LOCK_DELAY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentProvider {
    Claude,
    Codex,
}

impl AgentProvider {
    /// Every login provider. A new variant fails to compile in [`Self::position`] until it is
    /// given a place; list it here at that place too.
    pub const ALL: [AgentProvider; 2] = [AgentProvider::Claude, AgentProvider::Codex];

    /// Its stable id: its serde spelling, and how tools name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// The provider whose id is `id`.
    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|provider| provider.as_str() == id)
    }

    /// Its place in [`Self::ALL`]: a new variant fails to compile here until it is given
    /// the next place, and the check below holds each listed provider to its place.
    const fn position(self) -> usize {
        match self {
            Self::Claude => 0,
            Self::Codex => 1,
        }
    }
}

const _: () = {
    let mut index = 0;
    while index < AgentProvider::ALL.len() {
        assert!(AgentProvider::ALL[index].position() == index);
        index += 1;
    }
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentAccountAuthState {
    NotConfigured,
    AuthenticationRequired,
    Ready,
    Error,
}

/// Versioned pointer used for future launches. It names an account and follows it through
/// rename and re-sign-in: the resolver ignores `account_generation` (still written when the
/// selection is made) and refuses, never falls back, when the named account cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveAgentAccount {
    pub schema_version: u32,
    pub provider: AgentProvider,
    pub account_id: Option<String>,
    pub account_generation: Option<u64>,
    pub revision: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveAccountChange {
    pub previous: Option<ActiveAgentAccount>,
    pub current: Option<ActiveAgentAccount>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum AccountRequirement {
    #[default]
    None,
    Optional,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountResolutionSource {
    Explicit,
    Active,
    SystemDefault,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountResolutionReason {
    ExplicitOverride,
    ActiveSelection,
    OptionalSystemDefault,
    AccountNotRequired,
    RequiredMissing,
    Detached,
    AuthRequired,
    ProviderMismatch,
    GenerationMismatch,
    AccountMissing,
    FocusAccountMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountResolutionAvailability {
    Available,
    Unavailable,
    Exhausted,
    AuthRequired,
    Error,
    Unknown,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountResolutionFreshness {
    AwaitingFirstUse,
    Live,
    Stale,
    Unsupported,
    Unknown,
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentAccount {
    pub source: AccountResolutionSource,
    pub account_id: Option<String>,
    pub provider: Option<AgentProvider>,
    pub label: Option<String>,
    pub generation: Option<u64>,
    pub reason: AccountResolutionReason,
    pub availability: AccountResolutionAvailability,
    pub freshness: AccountResolutionFreshness,
    pub spawn_eligible: bool,
}

/// A runtime that uses its own login has no managed provider or account to resolve.
pub fn resolve_without_managed_account() -> ResolvedAgentAccount {
    ResolvedAgentAccount {
        source: AccountResolutionSource::None,
        account_id: None,
        provider: None,
        label: None,
        generation: None,
        reason: AccountResolutionReason::AccountNotRequired,
        availability: AccountResolutionAvailability::NotApplicable,
        freshness: AccountResolutionFreshness::NotApplicable,
        spawn_eligible: true,
    }
}

/// Pure account policy. It owns no I/O and returns only redacted launch facts.
pub fn resolve_agent_account(
    requirement: AccountRequirement,
    provider: AgentProvider,
    profiles: &[AgentAccountProfile],
    active: Option<&ActiveAgentAccount>,
    override_account_id: Option<&str>,
    override_account_generation: Option<u64>,
) -> ResolvedAgentAccount {
    if requirement == AccountRequirement::None {
        if override_account_id.is_some() {
            return unavailable(
                AccountResolutionSource::Explicit,
                Some(provider),
                AccountResolutionReason::AccountNotRequired,
            );
        }
        return resolve_without_managed_account();
    }

    // A launcher-chosen account must match the generation the launcher saw. Active follows its
    // account, so it carries no generation; when it is set but cannot be used the launch
    // refuses, and only an unset Active falls back to the person's own login.
    let selected = override_account_id
        .map(|id| (id, AccountResolutionSource::Explicit))
        .or_else(|| {
            active
                .and_then(|record| record.account_id.as_deref())
                .map(|id| (id, AccountResolutionSource::Active))
        });
    let Some((id, source)) = selected else {
        return fallback_or_unavailable(
            requirement,
            AccountResolutionSource::Active,
            provider,
            AccountResolutionReason::RequiredMissing,
        );
    };
    let Some(profile) = profiles.iter().find(|profile| profile.id == id) else {
        return unavailable(
            source,
            Some(provider),
            AccountResolutionReason::AccountMissing,
        );
    };
    let failure = if profile.provider != provider {
        AccountResolutionReason::ProviderMismatch
    } else if !profile.is_attached() {
        AccountResolutionReason::Detached
    } else if profile.auth_state != AgentAccountAuthState::Ready {
        AccountResolutionReason::AuthRequired
    } else if source == AccountResolutionSource::Explicit
        && override_account_generation != Some(profile.generation)
    {
        AccountResolutionReason::GenerationMismatch
    } else {
        return ResolvedAgentAccount {
            source,
            account_id: Some(profile.id.clone()),
            provider: Some(profile.provider),
            label: Some(profile.label.clone()),
            generation: Some(profile.generation),
            reason: if source == AccountResolutionSource::Explicit {
                AccountResolutionReason::ExplicitOverride
            } else {
                AccountResolutionReason::ActiveSelection
            },
            availability: AccountResolutionAvailability::Unknown,
            freshness: AccountResolutionFreshness::Unknown,
            spawn_eligible: true,
        };
    };
    unavailable(source, Some(provider), failure)
}

fn unavailable(
    source: AccountResolutionSource,
    provider: Option<AgentProvider>,
    reason: AccountResolutionReason,
) -> ResolvedAgentAccount {
    let availability = match reason {
        AccountResolutionReason::AuthRequired => AccountResolutionAvailability::AuthRequired,
        _ => AccountResolutionAvailability::Unavailable,
    };
    ResolvedAgentAccount {
        source,
        account_id: None,
        provider,
        label: None,
        generation: None,
        reason,
        availability,
        freshness: AccountResolutionFreshness::Unknown,
        spawn_eligible: false,
    }
}

/// Nothing was selected: Required refuses, Optional uses the person's own login.
fn fallback_or_unavailable(
    requirement: AccountRequirement,
    source: AccountResolutionSource,
    provider: AgentProvider,
    reason: AccountResolutionReason,
) -> ResolvedAgentAccount {
    if requirement == AccountRequirement::Required {
        return unavailable(source, Some(provider), reason);
    }
    ResolvedAgentAccount {
        source: AccountResolutionSource::SystemDefault,
        account_id: None,
        provider: Some(provider),
        label: None,
        generation: None,
        reason: AccountResolutionReason::OptionalSystemDefault,
        availability: AccountResolutionAvailability::NotApplicable,
        freshness: AccountResolutionFreshness::NotApplicable,
        spawn_eligible: true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityVerification {
    Verified,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredProviderIdentity {
    #[serde(default)]
    pub duplicate_key: Option<String>,
    #[serde(default)]
    pub display_hint: Option<String>,
    pub verification: IdentityVerification,
    pub verified_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentAccountProfile {
    pub schema_version: u32,
    pub id: String,
    pub provider: AgentProvider,
    pub label: String,
    pub auth_state: AgentAccountAuthState,
    #[serde(default)]
    pub auth_checked_at: Option<u64>,
    #[serde(default)]
    pub auth_error: Option<String>,
    #[serde(default)]
    pub identity: Option<StoredProviderIdentity>,
    #[serde(default)]
    pub status_line: Option<ManagedStatusLine>,
    pub generation: u64,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub detached_at: Option<u64>,
    #[serde(default)]
    pub(crate) pending_lifecycle_events: Vec<PendingLifecycleEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingLifecycleEvent {
    pub id: String,
    pub account_id: String,
    pub provider: AgentProvider,
    pub generation: u64,
    pub state: String,
    pub order: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedStatusLine {
    pub wrapper_hash: String,
    #[serde(default)]
    pub prior_settings_hash: Option<String>,
    pub installed_at: u64,
}

impl AgentAccountProfile {
    pub fn is_attached(&self) -> bool {
        self.detached_at.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicateIdentity {
    Attached(AgentAccountProfile),
    Detached(AgentAccountProfile),
}

/// The wire prefix that marks an optimistic-concurrency (CAS) conflict. Both the
/// `Display` below and the set-active command / MCP handler that classify a
/// conflict share this constant so a reword cannot silently break the
/// conflict-recovery path (return-current-selection + Retry/Undo).
pub const CONFLICT_ERROR_PREFIX: &str = "conflict:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreMutationError {
    Busy,
    Conflict { current_generation: u64 },
    NotFound,
    Duplicate(Box<DuplicateIdentity>),
    Invalid(String),
    Io(String),
}

impl std::fmt::Display for StoreMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => write!(f, "busy: agent account store is in use; retry"),
            Self::Conflict { current_generation } => {
                write!(
                    f,
                    "{CONFLICT_ERROR_PREFIX} current generation is {current_generation}"
                )
            }
            Self::NotFound => write!(f, "not-found: agent account"),
            Self::Duplicate(duplicate) => match duplicate.as_ref() {
                DuplicateIdentity::Attached(profile) => {
                    write!(f, "duplicate: already connected as {}", profile.label)
                }
                DuplicateIdentity::Detached(profile) => {
                    write!(f, "duplicate-detached: reattach {}", profile.label)
                }
            },
            Self::Invalid(message) | Self::Io(message) => f.write_str(message),
        }
    }
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn invalid(message: impl Into<String>) -> StoreMutationError {
    StoreMutationError::Invalid(format!("invalid: {}", message.into()))
}

fn io_error(error: impl std::fmt::Display) -> StoreMutationError {
    StoreMutationError::Io(format!("agent account store: {error}"))
}

fn validate_token(value: &str) -> Result<(), StoreMutationError> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(invalid("account id must be a bare token"));
    }
    Ok(())
}

fn validate_label(value: &str) -> Result<String, StoreMutationError> {
    let label = value.trim();
    if label.is_empty() || label.len() > 64 || label.chars().any(char::is_control) {
        return Err(invalid(
            "label must be 1..=64 bytes without control characters",
        ));
    }
    Ok(label.to_string())
}

fn user_root(codesense_home: &Path, principal: &Principal) -> PathBuf {
    path_to_user_home(codesense_home, principal)
}

fn accounts_root(codesense_home: &Path, principal: &Principal) -> PathBuf {
    user_root(codesense_home, principal).join(ACCOUNTS_DIR)
}

fn active_path(codesense_home: &Path, principal: &Principal, provider: AgentProvider) -> PathBuf {
    user_root(codesense_home, principal)
        .join(ACTIVE_DIR)
        .join(format!("{}.json", provider.as_str()))
}

fn read_active_at(path: &Path) -> Result<Option<ActiveAgentAccount>, StoreMutationError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    let active: ActiveAgentAccount = serde_json::from_slice(&bytes).map_err(io_error)?;
    if active.schema_version != SCHEMA_VERSION {
        return Err(invalid(format!(
            "unsupported active account schema {}",
            active.schema_version
        )));
    }
    if active.account_id.is_some() != active.account_generation.is_some() {
        return Err(invalid(
            "inconsistent active account identity and generation",
        ));
    }
    Ok(Some(active))
}

pub fn get_active_account(
    codesense_home: &Path,
    principal: &Principal,
    provider: AgentProvider,
) -> Result<Option<ActiveAgentAccount>, StoreMutationError> {
    let active = read_active_at(&active_path(codesense_home, principal, provider))?;
    if active
        .as_ref()
        .is_some_and(|record| record.provider != provider)
    {
        return Err(invalid("active account provider does not match its record"));
    }
    Ok(active)
}

/// Bump the revision and atomically persist one Active pointer record. Shared by
/// the set and clear paths so the revision/permissions/write sequence lives in
/// exactly one place. Callers hold the store lock and have already validated the
/// account (for the set path).
fn persist_active_record(
    path: &Path,
    provider: AgentProvider,
    account_id: Option<String>,
    account_generation: Option<u64>,
    current_revision: u64,
) -> Result<ActiveAgentAccount, StoreMutationError> {
    let revision = current_revision
        .checked_add(1)
        .ok_or_else(|| invalid("active account revision exhausted"))?;
    let record = ActiveAgentAccount {
        schema_version: SCHEMA_VERSION,
        provider,
        account_id,
        account_generation,
        revision,
        updated_at: now_epoch(),
    };
    let parent = path
        .parent()
        .ok_or_else(|| invalid("active account record has no parent"))?;
    fs::create_dir_all(parent).map_err(io_error)?;
    set_mode(parent, 0o700)?;
    let bytes = serde_json::to_vec_pretty(&record).map_err(io_error)?;
    write_tmp_fsync_rename(path, &bytes).map_err(|error: Error| io_error(error))?;
    set_mode(path, 0o600)?;
    Ok(record)
}

/// Compare-and-swap the Active pointer. `account_id = None` clears it. Clearing
/// still checks the current revision, while setting validates the exact account
/// generation under the same store lock before publishing the new pointer.
pub fn compare_and_set_active_account(
    codesense_home: &Path,
    principal: &Principal,
    provider: AgentProvider,
    expected_revision: u64,
    account_id: Option<&str>,
    expected_account_generation: Option<u64>,
) -> Result<ActiveAccountChange, StoreMutationError> {
    let _lock = StoreLock::acquire(codesense_home, principal)?;
    let path = active_path(codesense_home, principal, provider);
    let previous = read_active_at(&path)?;
    let current_revision = previous.as_ref().map_or(0, |record| record.revision);
    if current_revision != expected_revision {
        return Err(StoreMutationError::Conflict {
            current_generation: current_revision,
        });
    }
    let current = if let Some(id) = account_id {
        let expected_generation = expected_account_generation
            .ok_or_else(|| invalid("account generation is required when setting Active"))?;
        let profile = read_profile(&account_path(codesense_home, principal, id)?)?
            .ok_or(StoreMutationError::NotFound)?;
        if profile.provider != provider {
            return Err(invalid("account provider does not match Active provider"));
        }
        if !profile.is_attached() || profile.auth_state != AgentAccountAuthState::Ready {
            return Err(invalid(
                "Active account must be attached and authentication-ready",
            ));
        }
        if profile.generation != expected_generation {
            return Err(StoreMutationError::Conflict {
                current_generation: profile.generation,
            });
        }
        Some(persist_active_record(
            &path,
            provider,
            Some(profile.id),
            Some(profile.generation),
            current_revision,
        )?)
    } else {
        Some(persist_active_record(
            &path,
            provider,
            None,
            None,
            current_revision,
        )?)
    };
    Ok(ActiveAccountChange { previous, current })
}

fn account_path(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
) -> Result<PathBuf, StoreMutationError> {
    validate_token(id)?;
    Ok(accounts_root(codesense_home, principal).join(format!("{id}.json")))
}

pub fn managed_home(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
) -> Result<PathBuf, StoreMutationError> {
    validate_token(id)?;
    let principal_root = user_root(codesense_home, principal);
    reject_symlink_components(&principal_root)?;
    let root = principal_root.join(HOMES_DIR);
    reject_symlink_components(&root)?;
    Ok(root.join(id))
}

fn reject_symlink_components(path: &Path) -> Result<(), StoreMutationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(invalid("managed home contains a symlink component"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), StoreMutationError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(io_error)
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), StoreMutationError> {
    Ok(())
}

pub fn ensure_managed_home(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
) -> Result<PathBuf, StoreMutationError> {
    let path = managed_home(codesense_home, principal, id)?;
    // Reject a pre-positioned account-home symlink before any create/chmod can
    // follow it. The parent was checked by `managed_home` and every remaining
    // component is a backend-derived opaque token.
    reject_symlink_components(&path)?;
    fs::create_dir_all(&path).map_err(io_error)?;
    reject_symlink_components(&path)?;
    set_mode(&path, 0o700)?;
    Ok(path)
}

struct StoreLock(File);

impl StoreLock {
    fn acquire(codesense_home: &Path, principal: &Principal) -> Result<Self, StoreMutationError> {
        let root = user_root(codesense_home, principal);
        fs::create_dir_all(&root).map_err(io_error)?;
        set_mode(&root, 0o700)?;
        let path = root.join(LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(io_error)?;
        set_mode_from_file(&file, 0o600)?;
        for _ in 0..LOCK_ATTEMPTS {
            if try_lock_exclusive(&file)? {
                return Ok(Self(file));
            }
            std::thread::sleep(LOCK_DELAY);
        }
        Err(StoreMutationError::Busy)
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = unlock(&self.0);
    }
}

#[cfg(unix)]
fn set_mode_from_file(file: &File, mode: u32) -> Result<(), StoreMutationError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(io_error)
}

#[cfg(not(unix))]
fn set_mode_from_file(_file: &File, _mode: u32) -> Result<(), StoreMutationError> {
    Ok(())
}

#[cfg(unix)]
fn try_lock_exclusive(file: &File) -> Result<bool, StoreMutationError> {
    use std::os::fd::AsRawFd;
    // SAFETY: `file` owns a live descriptor for the duration of this call;
    // `flock` neither retains the pointer nor accesses Rust-managed memory.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    let code = error.raw_os_error();
    if code == Some(libc::EWOULDBLOCK) || code == Some(libc::EAGAIN) {
        Ok(false)
    } else {
        Err(io_error(error))
    }
}

#[cfg(not(unix))]
fn try_lock_exclusive(_file: &File) -> Result<bool, StoreMutationError> {
    Ok(true)
}

#[cfg(unix)]
fn unlock(file: &File) -> Result<(), StoreMutationError> {
    use std::os::fd::AsRawFd;
    // SAFETY: `file` still owns the descriptor locked by `StoreLock`; unlocking
    // does not retain it or access Rust-managed memory.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if result == 0 {
        Ok(())
    } else {
        Err(io_error(std::io::Error::last_os_error()))
    }
}

#[cfg(not(unix))]
fn unlock(_file: &File) -> Result<(), StoreMutationError> {
    Ok(())
}

fn read_profile(path: &Path) -> Result<Option<AgentAccountProfile>, StoreMutationError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    let profile: AgentAccountProfile = serde_json::from_slice(&bytes).map_err(io_error)?;
    if profile.schema_version != SCHEMA_VERSION {
        return Err(invalid(format!(
            "unsupported account schema {}",
            profile.schema_version
        )));
    }
    Ok(Some(profile))
}

fn write_profile(path: &Path, profile: &AgentAccountProfile) -> Result<(), StoreMutationError> {
    let mut canonical = profile.clone();
    canonical.schema_version = SCHEMA_VERSION;
    let bytes = serde_json::to_vec_pretty(&canonical).map_err(io_error)?;
    write_tmp_fsync_rename(path, &bytes).map_err(|error: Error| io_error(error))?;
    set_mode(path, 0o600)
}

fn next_lifecycle_order(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<u64, StoreMutationError> {
    Ok(list_profiles(codesense_home, principal)?
        .into_iter()
        .flat_map(|profile| profile.pending_lifecycle_events)
        .map(|event| event.order)
        .max()
        .unwrap_or(0)
        .saturating_add(1))
}

fn lifecycle_event(profile: &AgentAccountProfile, order: u64) -> PendingLifecycleEvent {
    PendingLifecycleEvent {
        id: format!("{:032x}", rand::random::<u128>()),
        account_id: profile.id.clone(),
        provider: profile.provider,
        generation: profile.generation,
        state: if profile.is_attached() {
            "attached".into()
        } else {
            "detached".into()
        },
        order,
    }
}

pub fn pending_lifecycle_events(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<Vec<PendingLifecycleEvent>, StoreMutationError> {
    let mut events: Vec<_> = list_profiles(codesense_home, principal)?
        .into_iter()
        .flat_map(|profile| profile.pending_lifecycle_events)
        .collect();
    events.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
    Ok(events)
}

pub fn acknowledge_lifecycle_event(
    codesense_home: &Path,
    principal: &Principal,
    account_id: &str,
    event_id: &str,
) -> Result<bool, StoreMutationError> {
    validate_token(event_id)?;
    let _lock = StoreLock::acquire(codesense_home, principal)?;
    let path = account_path(codesense_home, principal, account_id)?;
    let mut profile = read_profile(&path)?.ok_or(StoreMutationError::NotFound)?;
    let original_len = profile.pending_lifecycle_events.len();
    profile
        .pending_lifecycle_events
        .retain(|event| event.id != event_id);
    if profile.pending_lifecycle_events.len() == original_len {
        return Ok(false);
    }
    // Delivery bookkeeping is not a user-visible account mutation, so it must
    // not advance the optimistic-concurrency generation.
    write_profile(&path, &profile)?;
    Ok(true)
}

pub fn list_profiles(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<Vec<AgentAccountProfile>, StoreMutationError> {
    let root = accounts_root(codesense_home, principal);
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error(error)),
    };
    let mut profiles = Vec::new();
    for entry in entries {
        let entry = entry.map_err(io_error)?;
        if entry.path().extension().and_then(|v| v.to_str()) != Some("json") {
            continue;
        }
        if let Some(profile) = read_profile(&entry.path())? {
            profiles.push(profile);
        }
    }
    profiles.sort_by(|a, b| {
        a.provider
            .as_str()
            .cmp(b.provider.as_str())
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(profiles)
}

pub fn create_profile(
    codesense_home: &Path,
    principal: &Principal,
    provider: AgentProvider,
    label: &str,
) -> Result<AgentAccountProfile, StoreMutationError> {
    let _lock = StoreLock::acquire(codesense_home, principal)?;
    let label = validate_label(label)?;
    let profiles_root = accounts_root(codesense_home, principal);
    fs::create_dir_all(&profiles_root).map_err(io_error)?;
    set_mode(&profiles_root, 0o700)?;
    // Provision the per-user digest key while the store is still empty. Once
    // profiles exist, `identity_key` deliberately refuses silent rotation.
    let _ = identity_key(codesense_home, principal)?;
    let id = format!("{:032x}", rand::random::<u128>());
    let home = ensure_managed_home(codesense_home, principal, &id)?;
    let now = now_epoch();
    let mut profile = AgentAccountProfile {
        schema_version: SCHEMA_VERSION,
        id: id.clone(),
        provider,
        label,
        auth_state: AgentAccountAuthState::NotConfigured,
        auth_checked_at: None,
        auth_error: None,
        identity: None,
        status_line: None,
        generation: 1,
        created_at: now,
        updated_at: now,
        detached_at: None,
        pending_lifecycle_events: Vec::new(),
    };
    let order = next_lifecycle_order(codesense_home, principal)?;
    profile
        .pending_lifecycle_events
        .push(lifecycle_event(&profile, order));
    if let Err(error) = write_profile(&account_path(codesense_home, principal, &id)?, &profile) {
        // The home is created solely for this new opaque id. If the durable
        // profile commit fails, remove the empty home so a failed create does
        // not leave an undiscoverable credential location behind.
        let _ = fs::remove_dir(&home);
        return Err(error);
    }
    Ok(profile)
}

pub fn update_profile<F>(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
    expected_generation: u64,
    mutate: F,
) -> Result<AgentAccountProfile, StoreMutationError>
where
    F: FnOnce(&mut AgentAccountProfile) -> Result<(), StoreMutationError>,
{
    let _lock = StoreLock::acquire(codesense_home, principal)?;
    let path = account_path(codesense_home, principal, id)?;
    let mut profile = read_profile(&path)?.ok_or(StoreMutationError::NotFound)?;
    if profile.generation != expected_generation {
        return Err(StoreMutationError::Conflict {
            current_generation: profile.generation,
        });
    }
    let before = profile.clone();
    mutate(&mut profile)?;
    if profile == before {
        return Ok(profile);
    }
    profile.generation = profile.generation.saturating_add(1);
    profile.updated_at = now_epoch();
    write_profile(&path, &profile)?;
    Ok(profile)
}

pub fn rename_profile(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
    expected_generation: u64,
    label: &str,
) -> Result<AgentAccountProfile, StoreMutationError> {
    let label = validate_label(label)?;
    update_profile_with_lifecycle(
        codesense_home,
        principal,
        id,
        expected_generation,
        move |profile| {
            profile.label = label;
            Ok(())
        },
    )
}

pub fn detach_profile(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
    expected_generation: u64,
) -> Result<AgentAccountProfile, StoreMutationError> {
    update_profile_with_lifecycle(
        codesense_home,
        principal,
        id,
        expected_generation,
        |profile| {
            if profile.detached_at.is_none() {
                profile.detached_at = Some(now_epoch());
            }
            Ok(())
        },
    )
}

pub fn reattach_profile(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
    expected_generation: u64,
) -> Result<AgentAccountProfile, StoreMutationError> {
    let home = managed_home(codesense_home, principal, id)?;
    if !home.is_dir() {
        return Err(invalid("managed account home is missing"));
    }
    update_profile_with_lifecycle(
        codesense_home,
        principal,
        id,
        expected_generation,
        |profile| {
            profile.detached_at = None;
            Ok(())
        },
    )
}

fn update_profile_with_lifecycle<F>(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
    expected_generation: u64,
    mutate: F,
) -> Result<AgentAccountProfile, StoreMutationError>
where
    F: FnOnce(&mut AgentAccountProfile) -> Result<(), StoreMutationError>,
{
    let _lock = StoreLock::acquire(codesense_home, principal)?;
    let path = account_path(codesense_home, principal, id)?;
    let mut profile = read_profile(&path)?.ok_or(StoreMutationError::NotFound)?;
    if profile.generation != expected_generation {
        return Err(StoreMutationError::Conflict {
            current_generation: profile.generation,
        });
    }
    let before = profile.clone();
    mutate(&mut profile)?;
    if profile == before {
        return Ok(profile);
    }
    profile.generation = profile.generation.saturating_add(1);
    profile.updated_at = now_epoch();
    let order = next_lifecycle_order(codesense_home, principal)?;
    profile
        .pending_lifecycle_events
        .push(lifecycle_event(&profile, order));
    write_profile(&path, &profile)?;
    Ok(profile)
}

pub fn set_auth_state(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
    expected_generation: u64,
    state: AgentAccountAuthState,
    redacted_error: Option<String>,
) -> Result<AgentAccountProfile, StoreMutationError> {
    update_profile(
        codesense_home,
        principal,
        id,
        expected_generation,
        move |profile| {
            apply_auth_state(profile, state, redacted_error);
            Ok(())
        },
    )
}

/// Record what a re-check of the login found. Unlike [`set_auth_state`] it writes, and so bumps
/// the generation, only when the state or the error changed: a check that finds nothing new
/// must not invalidate a launch binding minted a moment ago.
pub fn record_auth_check(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
    expected_generation: u64,
    state: AgentAccountAuthState,
    redacted_error: Option<String>,
) -> Result<AgentAccountProfile, StoreMutationError> {
    update_profile(
        codesense_home,
        principal,
        id,
        expected_generation,
        move |profile| {
            let mut next = profile.clone();
            apply_auth_state(&mut next, state, redacted_error);
            if next.auth_state != profile.auth_state || next.auth_error != profile.auth_error {
                *profile = next;
            }
            Ok(())
        },
    )
}

fn apply_auth_state(
    profile: &mut AgentAccountProfile,
    state: AgentAccountAuthState,
    redacted_error: Option<String>,
) {
    profile.auth_state = state;
    profile.auth_checked_at = Some(now_epoch());
    profile.auth_error = redacted_error.map(|value| value.chars().take(280).collect());
}

pub fn set_status_line(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
    expected_generation: u64,
    status_line: ManagedStatusLine,
) -> Result<AgentAccountProfile, StoreMutationError> {
    update_profile(
        codesense_home,
        principal,
        id,
        expected_generation,
        move |profile| {
            profile.status_line = Some(status_line);
            Ok(())
        },
    )
}

fn identity_key_path(codesense_home: &Path, principal: &Principal) -> PathBuf {
    user_root(codesense_home, principal).join(IDENTITY_KEY_FILE)
}

fn identity_key(
    codesense_home: &Path,
    principal: &Principal,
) -> Result<[u8; 32], StoreMutationError> {
    let path = identity_key_path(codesense_home, principal);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(invalid("identity key must not be a symlink"));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(invalid("identity key must be a regular file"));
        }
        Ok(metadata) => validate_identity_key_permissions(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error(error)),
    }
    match File::open(&path) {
        Ok(mut file) => {
            let mut key = [0u8; 32];
            file.read_exact(&mut key).map_err(io_error)?;
            let mut extra = [0u8; 1];
            if file.read(&mut extra).map_err(io_error)? != 0 {
                return Err(invalid("identity key has invalid length"));
            }
            Ok(key)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A missing key is safe only before the first profile is written.
            // Regenerating it later would silently change every duplicate
            // digest and make two copies of the same provider identity appear
            // unrelated. Fail closed and let an explicit recovery flow repair
            // the index instead.
            if !list_profiles(codesense_home, principal)?.is_empty() {
                return Err(invalid(
                    "identity key is missing; account identity reindex is required",
                ));
            }
            let mut key = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut key);
            fs::create_dir_all(path.parent().ok_or_else(|| invalid("identity key root"))?)
                .map_err(io_error)?;
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            match options.open(&path) {
                Ok(mut file) => {
                    file.write_all(&key).map_err(io_error)?;
                    file.sync_all().map_err(io_error)?;
                    set_mode_from_file(&file, 0o600)?;
                    Ok(key)
                }
                Err(race) if race.kind() == std::io::ErrorKind::AlreadyExists => {
                    identity_key(codesense_home, principal)
                }
                Err(other) => Err(io_error(other)),
            }
        }
        Err(error) => Err(io_error(error)),
    }
}

#[cfg(unix)]
fn validate_identity_key_permissions(metadata: &fs::Metadata) -> Result<(), StoreMutationError> {
    use std::os::unix::fs::PermissionsExt;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(invalid("identity key permissions must be owner-only"));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_identity_key_permissions(_metadata: &fs::Metadata) -> Result<(), StoreMutationError> {
    Ok(())
}

pub fn duplicate_key(
    codesense_home: &Path,
    principal: &Principal,
    provider: AgentProvider,
    stable_identifier: &str,
) -> Result<String, StoreMutationError> {
    let identifier = stable_identifier.trim().to_lowercase();
    if identifier.is_empty() || identifier.len() > 512 {
        return Err(invalid("provider identity is empty or oversized"));
    }
    let key = identity_key(codesense_home, principal)?;
    let input = format!(
        "codesense-agent-account-v1\0{}\0{identifier}",
        provider.as_str()
    );
    Ok(blake3::keyed_hash(&key, input.as_bytes())
        .to_hex()
        .to_string())
}

#[cfg(test)]
pub fn commit_identity(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
    expected_generation: u64,
    identity: StoredProviderIdentity,
) -> Result<AgentAccountProfile, StoreMutationError> {
    commit_identity_inner(
        codesense_home,
        principal,
        id,
        expected_generation,
        None,
        identity,
    )
}

/// Complete a provider login even if the only concurrent profile mutation was
/// a label rename. Other lifecycle/auth changes remain generation conflicts.
pub fn commit_login_identity(
    codesense_home: &Path,
    principal: &Principal,
    started: &AgentAccountProfile,
    identity: StoredProviderIdentity,
) -> Result<AgentAccountProfile, StoreMutationError> {
    commit_identity_inner(
        codesense_home,
        principal,
        &started.id,
        started.generation,
        Some(started),
        identity,
    )
}

fn commit_identity_inner(
    codesense_home: &Path,
    principal: &Principal,
    id: &str,
    expected_generation: u64,
    started: Option<&AgentAccountProfile>,
    identity: StoredProviderIdentity,
) -> Result<AgentAccountProfile, StoreMutationError> {
    let _lock = StoreLock::acquire(codesense_home, principal)?;
    let path = account_path(codesense_home, principal, id)?;
    let mut profile = read_profile(&path)?.ok_or(StoreMutationError::NotFound)?;
    if profile.generation != expected_generation
        && !started.is_some_and(|snapshot| only_label_changed(snapshot, &profile))
    {
        return Err(StoreMutationError::Conflict {
            current_generation: profile.generation,
        });
    }
    if let Some(wanted) = identity.duplicate_key.as_deref() {
        if let Some(existing) =
            list_profiles(codesense_home, principal)?
                .into_iter()
                .find(|candidate| {
                    candidate.id != profile.id
                        && candidate.provider == profile.provider
                        && candidate
                            .identity
                            .as_ref()
                            .and_then(|value| value.duplicate_key.as_deref())
                            == Some(wanted)
                })
        {
            return Err(StoreMutationError::Duplicate(Box::new(
                if existing.is_attached() {
                    DuplicateIdentity::Attached(existing)
                } else {
                    DuplicateIdentity::Detached(existing)
                },
            )));
        }
    }
    profile.identity = Some(identity);
    profile.auth_state = AgentAccountAuthState::Ready;
    profile.auth_checked_at = Some(now_epoch());
    profile.auth_error = None;
    profile.generation = profile.generation.saturating_add(1);
    profile.updated_at = now_epoch();
    write_profile(&path, &profile)?;
    Ok(profile)
}

fn only_label_changed(before: &AgentAccountProfile, after: &AgentAccountProfile) -> bool {
    let mut normalized = after.clone();
    normalized.label = before.label.clone();
    normalized.generation = before.generation;
    normalized.updated_at = before.updated_at;
    normalized.pending_lifecycle_events = before.pending_lifecycle_events.clone();
    normalized == *before && after.label != before.label
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn principal() -> Principal {
        Principal::from(42u64)
    }

    #[test]
    fn create_list_detach_restart_and_reattach_preserve_home() {
        let temp = TempDir::new().unwrap();
        let created =
            create_profile(temp.path(), &principal(), AgentProvider::Claude, "Primary").unwrap();
        let home = managed_home(temp.path(), &principal(), &created.id).unwrap();
        assert!(home.is_dir());
        let detached =
            detach_profile(temp.path(), &principal(), &created.id, created.generation).unwrap();
        assert!(detached.detached_at.is_some());
        assert_eq!(list_profiles(temp.path(), &principal()).unwrap().len(), 1);
        let reattached =
            reattach_profile(temp.path(), &principal(), &created.id, detached.generation).unwrap();
        assert!(reattached.is_attached());
        assert_eq!(reattached.id, created.id);
    }

    #[test]
    fn lifecycle_event_survives_reload_until_acknowledged() {
        let temp = TempDir::new().unwrap();
        let created =
            create_profile(temp.path(), &principal(), AgentProvider::Claude, "Primary").unwrap();

        let pending = pending_lifecycle_events(temp.path(), &principal()).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].account_id, created.id);
        assert_eq!(pending[0].generation, created.generation);
        assert_eq!(pending[0].state, "attached");

        assert!(acknowledge_lifecycle_event(
            temp.path(),
            &principal(),
            &created.id,
            &pending[0].id,
        )
        .unwrap());
        assert!(pending_lifecycle_events(temp.path(), &principal())
            .unwrap()
            .is_empty());
        assert!(!acknowledge_lifecycle_event(
            temp.path(),
            &principal(),
            &created.id,
            &pending[0].id,
        )
        .unwrap());
    }

    #[test]
    fn stale_generation_never_overwrites_a_newer_mutation() {
        let temp = TempDir::new().unwrap();
        let created =
            create_profile(temp.path(), &principal(), AgentProvider::Codex, "One").unwrap();
        let renamed = rename_profile(
            temp.path(),
            &principal(),
            &created.id,
            created.generation,
            "Two",
        )
        .unwrap();
        assert_eq!(renamed.label, "Two");
        assert!(matches!(
            detach_profile(temp.path(), &principal(), &created.id, created.generation),
            Err(StoreMutationError::Conflict { .. })
        ));
    }

    #[test]
    fn repeated_detach_and_reattach_are_idempotent() {
        let temp = TempDir::new().unwrap();
        let created =
            create_profile(temp.path(), &principal(), AgentProvider::Claude, "Primary").unwrap();
        let detached =
            detach_profile(temp.path(), &principal(), &created.id, created.generation).unwrap();
        let detached_again =
            detach_profile(temp.path(), &principal(), &created.id, detached.generation).unwrap();
        assert_eq!(detached_again.generation, detached.generation);

        let attached = reattach_profile(
            temp.path(),
            &principal(),
            &created.id,
            detached_again.generation,
        )
        .unwrap();
        let attached_again =
            reattach_profile(temp.path(), &principal(), &created.id, attached.generation).unwrap();
        assert_eq!(attached_again.generation, attached.generation);
    }

    #[test]
    fn login_completion_survives_label_only_concurrent_change() {
        let temp = TempDir::new().unwrap();
        let started =
            create_profile(temp.path(), &principal(), AgentProvider::Codex, "Before").unwrap();
        rename_profile(
            temp.path(),
            &principal(),
            &started.id,
            started.generation,
            "After",
        )
        .unwrap();
        let completed = commit_login_identity(
            temp.path(),
            &principal(),
            &started,
            StoredProviderIdentity {
                duplicate_key: None,
                display_hint: None,
                verification: IdentityVerification::Unverified,
                verified_at: now_epoch(),
            },
        )
        .unwrap();
        assert_eq!(completed.label, "After");
        assert_eq!(completed.auth_state, AgentAccountAuthState::Ready);
    }

    #[test]
    fn missing_identity_key_after_profile_creation_fails_closed() {
        let temp = TempDir::new().unwrap();
        let created =
            create_profile(temp.path(), &principal(), AgentProvider::Claude, "Primary").unwrap();
        fs::remove_file(identity_key_path(temp.path(), &principal())).unwrap();

        let error = duplicate_key(
            temp.path(),
            &principal(),
            created.provider,
            "stable-provider-identity",
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("account identity reindex is required"));
        assert!(!identity_key_path(temp.path(), &principal()).exists());
    }

    #[test]
    fn duplicate_identity_is_provider_scoped_and_tombstone_aware() {
        let temp = TempDir::new().unwrap();
        let first = create_profile(temp.path(), &principal(), AgentProvider::Codex, "One").unwrap();
        let second =
            create_profile(temp.path(), &principal(), AgentProvider::Codex, "Two").unwrap();
        let key =
            duplicate_key(temp.path(), &principal(), AgentProvider::Codex, "A@B.test").unwrap();
        let identity = StoredProviderIdentity {
            duplicate_key: Some(key.clone()),
            display_hint: Some("a•••@b.test".into()),
            verification: IdentityVerification::Verified,
            verified_at: now_epoch(),
        };
        let first = commit_identity(
            temp.path(),
            &principal(),
            &first.id,
            first.generation,
            identity.clone(),
        )
        .unwrap();
        let duplicate = commit_identity(
            temp.path(),
            &principal(),
            &second.id,
            second.generation,
            identity.clone(),
        )
        .unwrap_err();
        assert!(matches!(
            duplicate,
            StoreMutationError::Duplicate(value)
                if matches!(value.as_ref(), DuplicateIdentity::Attached(_))
        ));
        let first = detach_profile(temp.path(), &principal(), &first.id, first.generation).unwrap();
        assert!(first.detached_at.is_some());
        let duplicate = commit_identity(
            temp.path(),
            &principal(),
            &second.id,
            second.generation,
            identity,
        )
        .unwrap_err();
        assert!(matches!(
            duplicate,
            StoreMutationError::Duplicate(value)
                if matches!(value.as_ref(), DuplicateIdentity::Detached(_))
        ));
    }

    #[test]
    fn identity_digest_survives_restart_without_persisting_raw_identity() {
        let temp = TempDir::new().unwrap();
        let first = duplicate_key(
            temp.path(),
            &principal(),
            AgentProvider::Claude,
            "user@test",
        )
        .unwrap();
        let second = duplicate_key(
            temp.path(),
            &principal(),
            AgentProvider::Claude,
            "USER@test",
        )
        .unwrap();
        assert_eq!(first, second);
        assert!(!first.contains("user"));
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn managed_home_rejects_symlinked_user_path() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let temp = TempDir::new().unwrap();
            let outside = TempDir::new().unwrap();
            let user = user_root(temp.path(), &principal());
            fs::create_dir_all(&user).unwrap();
            symlink(outside.path(), user.join(HOMES_DIR)).unwrap();
            assert!(ensure_managed_home(temp.path(), &principal(), "safe").is_err());
        }
    }

    #[test]
    fn identity_key_rejects_symlinks_and_group_readable_files() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{symlink, PermissionsExt};
            let temp = TempDir::new().unwrap();
            let root = user_root(temp.path(), &principal());
            fs::create_dir_all(&root).unwrap();
            let outside = temp.path().join("outside-key");
            fs::write(&outside, [7u8; 32]).unwrap();
            let key_path = identity_key_path(temp.path(), &principal());
            symlink(&outside, &key_path).unwrap();
            assert!(
                duplicate_key(temp.path(), &principal(), AgentProvider::Claude, "one").is_err()
            );

            fs::remove_file(&key_path).unwrap();
            fs::write(&key_path, [8u8; 32]).unwrap();
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o640)).unwrap();
            assert!(
                duplicate_key(temp.path(), &principal(), AgentProvider::Claude, "two").is_err()
            );
        }
    }

    #[test]
    fn resolver_covers_managed_override_optional_required_none_and_generation_race() {
        let mut account = AgentAccountProfile {
            schema_version: SCHEMA_VERSION,
            id: "account-a".into(),
            provider: AgentProvider::Claude,
            label: "Work".into(),
            auth_state: AgentAccountAuthState::Ready,
            auth_checked_at: None,
            auth_error: None,
            identity: None,
            status_line: None,
            generation: 4,
            created_at: 1,
            updated_at: 1,
            detached_at: None,
            pending_lifecycle_events: Vec::new(),
        };
        let active = ActiveAgentAccount {
            schema_version: SCHEMA_VERSION,
            provider: AgentProvider::Claude,
            account_id: Some(account.id.clone()),
            account_generation: Some(4),
            revision: 2,
            updated_at: 1,
        };
        let managed = resolve_agent_account(
            AccountRequirement::Required,
            AgentProvider::Claude,
            &[account.clone()],
            Some(&active),
            None,
            None,
        );
        assert_eq!(managed.source, AccountResolutionSource::Active);
        assert_eq!(managed.label.as_deref(), Some("Work"));

        let override_resolution = resolve_agent_account(
            AccountRequirement::Required,
            AgentProvider::Claude,
            &[account.clone()],
            None,
            Some("account-a"),
            Some(4),
        );
        assert_eq!(
            override_resolution.source,
            AccountResolutionSource::Explicit
        );
        let hostile = resolve_agent_account(
            AccountRequirement::None,
            AgentProvider::Claude,
            &[account.clone()],
            None,
            Some("account-a"),
            Some(4),
        );
        assert!(!hostile.spawn_eligible);
        assert_eq!(hostile.reason, AccountResolutionReason::AccountNotRequired);

        account.generation = 5;
        let followed = resolve_agent_account(
            AccountRequirement::Optional,
            AgentProvider::Claude,
            &[account.clone()],
            Some(&active),
            None,
            None,
        );
        assert_eq!(followed.source, AccountResolutionSource::Active);
        assert_eq!(followed.generation, Some(5));
        let stale_explicit = resolve_agent_account(
            AccountRequirement::Required,
            AgentProvider::Claude,
            &[account],
            None,
            Some("account-a"),
            Some(4),
        );
        assert!(!stale_explicit.spawn_eligible);
        assert_eq!(
            stale_explicit.reason,
            AccountResolutionReason::GenerationMismatch
        );
    }

    #[test]
    fn active_account_is_generation_validated_and_revision_cas_protected() {
        let temp = TempDir::new().unwrap();
        let created =
            create_profile(temp.path(), &principal(), AgentProvider::Codex, "Primary").unwrap();
        let ready = update_profile(
            temp.path(),
            &principal(),
            &created.id,
            created.generation,
            |profile| {
                profile.auth_state = AgentAccountAuthState::Ready;
                Ok(())
            },
        )
        .unwrap();
        let changed = compare_and_set_active_account(
            temp.path(),
            &principal(),
            AgentProvider::Codex,
            0,
            Some(&ready.id),
            Some(ready.generation),
        )
        .unwrap();
        assert_eq!(changed.current.as_ref().unwrap().revision, 1);
        assert!(matches!(
            compare_and_set_active_account(
                temp.path(),
                &principal(),
                AgentProvider::Codex,
                0,
                Some(&ready.id),
                Some(ready.generation)
            ),
            Err(StoreMutationError::Conflict {
                current_generation: 1
            })
        ));
        assert_eq!(
            get_active_account(temp.path(), &principal(), AgentProvider::Codex)
                .unwrap()
                .unwrap()
                .account_id,
            Some(ready.id)
        );
    }

    fn ready_account_with_active(
        temp: &TempDir,
        provider: AgentProvider,
        label: &str,
    ) -> (AgentAccountProfile, ActiveAgentAccount) {
        let created = create_profile(temp.path(), &principal(), provider, label).unwrap();
        let ready = update_profile(
            temp.path(),
            &principal(),
            &created.id,
            created.generation,
            |profile| {
                profile.auth_state = AgentAccountAuthState::Ready;
                Ok(())
            },
        )
        .unwrap();
        let change = compare_and_set_active_account(
            temp.path(),
            &principal(),
            provider,
            0,
            Some(&ready.id),
            Some(ready.generation),
        )
        .unwrap();
        (ready, change.current.unwrap())
    }

    fn resolve_unpinned(
        temp: &TempDir,
        requirement: AccountRequirement,
        provider: AgentProvider,
    ) -> ResolvedAgentAccount {
        let profiles = list_profiles(temp.path(), &principal()).unwrap();
        let active = get_active_account(temp.path(), &principal(), provider).unwrap();
        resolve_agent_account(
            requirement,
            provider,
            &profiles,
            active.as_ref(),
            None,
            None,
        )
    }

    fn identity() -> StoredProviderIdentity {
        StoredProviderIdentity {
            duplicate_key: Some("key".into()),
            display_hint: None,
            verification: IdentityVerification::Verified,
            verified_at: 1,
        }
    }

    #[test]
    fn active_follows_its_account_through_rename() {
        let temp = TempDir::new().unwrap();
        let (ready, _) = ready_account_with_active(&temp, AgentProvider::Claude, "Work");
        rename_profile(
            temp.path(),
            &principal(),
            &ready.id,
            ready.generation,
            "Work (Acme)",
        )
        .unwrap();
        for requirement in [AccountRequirement::Optional, AccountRequirement::Required] {
            let resolved = resolve_unpinned(&temp, requirement, AgentProvider::Claude);
            assert!(resolved.spawn_eligible);
            assert_eq!(resolved.source, AccountResolutionSource::Active);
            assert_eq!(resolved.reason, AccountResolutionReason::ActiveSelection);
            assert_eq!(resolved.label.as_deref(), Some("Work (Acme)"));
            assert_eq!(resolved.generation, Some(ready.generation + 1));
        }
    }

    #[test]
    fn active_follows_its_account_through_re_sign_in() {
        let temp = TempDir::new().unwrap();
        let (ready, _) = ready_account_with_active(&temp, AgentProvider::Codex, "Work");
        let signed_in =
            commit_login_identity(temp.path(), &principal(), &ready, identity()).unwrap();
        let resolved = resolve_unpinned(&temp, AccountRequirement::Optional, AgentProvider::Codex);
        assert!(resolved.spawn_eligible);
        assert_eq!(resolved.source, AccountResolutionSource::Active);
        assert_eq!(resolved.reason, AccountResolutionReason::ActiveSelection);
        assert_eq!(resolved.generation, Some(signed_in.generation));
    }

    #[test]
    fn a_broken_active_refuses_instead_of_using_the_own_login() {
        let cases: [(&str, AccountResolutionReason); 4] = [
            ("detached", AccountResolutionReason::Detached),
            ("missing", AccountResolutionReason::AccountMissing),
            ("auth-required", AccountResolutionReason::AuthRequired),
            ("provider", AccountResolutionReason::ProviderMismatch),
        ];
        for (case, reason) in cases {
            for requirement in [AccountRequirement::Optional, AccountRequirement::Required] {
                let temp = TempDir::new().unwrap();
                let (ready, active) =
                    ready_account_with_active(&temp, AgentProvider::Claude, "Work");
                let mut profiles = list_profiles(temp.path(), &principal()).unwrap();
                let mut provider = AgentProvider::Claude;
                match case {
                    "detached" => profiles[0].detached_at = Some(5),
                    "missing" => profiles.clear(),
                    "auth-required" => {
                        profiles[0].auth_state = AgentAccountAuthState::AuthenticationRequired
                    }
                    _ => provider = AgentProvider::Codex,
                }
                let mut active = active;
                active.provider = provider;
                let resolved = resolve_agent_account(
                    requirement,
                    provider,
                    &profiles,
                    Some(&active),
                    None,
                    None,
                );
                assert!(!resolved.spawn_eligible, "{case} {requirement:?}");
                assert_eq!(resolved.source, AccountResolutionSource::Active, "{case}");
                assert_eq!(resolved.reason, reason, "{case} {requirement:?}");
            }
        }
    }

    #[test]
    fn a_cleared_active_uses_the_system_default() {
        let temp = TempDir::new().unwrap();
        let (_, active) = ready_account_with_active(&temp, AgentProvider::Claude, "Work");
        compare_and_set_active_account(
            temp.path(),
            &principal(),
            AgentProvider::Claude,
            active.revision,
            None,
            None,
        )
        .unwrap();
        let optional = resolve_unpinned(&temp, AccountRequirement::Optional, AgentProvider::Claude);
        assert!(optional.spawn_eligible);
        assert_eq!(optional.source, AccountResolutionSource::SystemDefault);
        assert_eq!(
            optional.reason,
            AccountResolutionReason::OptionalSystemDefault
        );
        let required = resolve_unpinned(&temp, AccountRequirement::Required, AgentProvider::Claude);
        assert!(!required.spawn_eligible);
        assert_eq!(required.reason, AccountResolutionReason::RequiredMissing);
    }

    #[test]
    fn an_explicit_stale_generation_still_mismatches() {
        let temp = TempDir::new().unwrap();
        let (ready, _) = ready_account_with_active(&temp, AgentProvider::Claude, "Work");
        let renamed = rename_profile(
            temp.path(),
            &principal(),
            &ready.id,
            ready.generation,
            "Work 2",
        )
        .unwrap();
        let profiles = list_profiles(temp.path(), &principal()).unwrap();
        let stale = resolve_agent_account(
            AccountRequirement::Optional,
            AgentProvider::Claude,
            &profiles,
            None,
            Some(&ready.id),
            Some(ready.generation),
        );
        assert!(!stale.spawn_eligible);
        assert_eq!(stale.source, AccountResolutionSource::Explicit);
        assert_eq!(stale.reason, AccountResolutionReason::GenerationMismatch);
        let current = resolve_agent_account(
            AccountRequirement::Optional,
            AgentProvider::Claude,
            &profiles,
            None,
            Some(&ready.id),
            Some(renamed.generation),
        );
        assert!(current.spawn_eligible);
    }

    #[test]
    fn an_explicit_missing_account_has_its_own_reason() {
        let resolved = resolve_agent_account(
            AccountRequirement::Optional,
            AgentProvider::Claude,
            &[],
            None,
            Some("gone"),
            Some(1),
        );
        assert!(!resolved.spawn_eligible);
        assert_eq!(resolved.reason, AccountResolutionReason::AccountMissing);
    }

    #[test]
    fn an_unchanged_auth_check_does_not_bump_the_generation() {
        let temp = TempDir::new().unwrap();
        let (ready, _) = ready_account_with_active(&temp, AgentProvider::Claude, "Work");
        let same = record_auth_check(
            temp.path(),
            &principal(),
            &ready.id,
            ready.generation,
            AgentAccountAuthState::Ready,
            None,
        )
        .unwrap();
        assert_eq!(same.generation, ready.generation);
        assert_eq!(same, ready);
    }

    #[test]
    fn a_changed_auth_check_bumps_the_generation_and_keeps_the_binding_guard() {
        let temp = TempDir::new().unwrap();
        let (ready, _) = ready_account_with_active(&temp, AgentProvider::Claude, "Work");
        let expired = record_auth_check(
            temp.path(),
            &principal(),
            &ready.id,
            ready.generation,
            AgentAccountAuthState::AuthenticationRequired,
            None,
        )
        .unwrap();
        assert_eq!(expired.generation, ready.generation + 1);
        assert_eq!(
            expired.auth_state,
            AgentAccountAuthState::AuthenticationRequired
        );
        assert!(expired.auth_checked_at.is_some());
        let errored = record_auth_check(
            temp.path(),
            &principal(),
            &ready.id,
            expired.generation,
            AgentAccountAuthState::Error,
            Some("Claude CLI unavailable".into()),
        )
        .unwrap();
        assert_eq!(errored.generation, expired.generation + 1);
        // The same error again changes nothing.
        let again = record_auth_check(
            temp.path(),
            &principal(),
            &ready.id,
            errored.generation,
            AgentAccountAuthState::Error,
            Some("Claude CLI unavailable".into()),
        )
        .unwrap();
        assert_eq!(again.generation, errored.generation);
        // A check that raced a newer write is a conflict, never an overwrite.
        assert!(matches!(
            record_auth_check(
                temp.path(),
                &principal(),
                &ready.id,
                ready.generation,
                AgentAccountAuthState::Ready,
                None,
            ),
            Err(StoreMutationError::Conflict { .. })
        ));
    }
}
