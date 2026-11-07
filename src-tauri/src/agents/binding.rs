// SPDX-License-Identifier: MIT
//! Which provider CLI login (a Claude or Codex account from the agent-accounts
//! store) this user runs an agent with.
//!
//! The binding is this user's own, machine-local choice. It is never part of
//! the shared definition and never written under a team dir, so it is never
//! git-synced: two teammates launching one team agent each use their own
//! login. With no binding, the runtime's active account applies (at launch).
//!
//! It also carries this user's raw-terminal consent for the agent: whether an
//! interactive launch of it may keep a raw snapshot of the terminal. Consent is
//! personal for the same reason the account is.
//!
//! Stored as `{ "<agent key>": { "account_id": "<account id>",
//! "capture_raw_terminal": true } }` in one file per local user, located by
//! [`bindings_file`] alone. Either field may be absent.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::context_repo::path_to_user_home;
use crate::domain::Principal;
use crate::install::agent_account_store::{list_profiles, AgentProvider};
use crate::shared::json_io::write_json_atomic;

use super::keys::AgentKey;
use super::service::{self, AgentError};
use codesense_pty_core::agent_cli::AgentCli;

pub const BINDINGS_FILE: &str = "agent-bindings.json";

/// The machine-local bindings file of `principal`.
pub fn bindings_file(home: &Path, principal: &Principal) -> PathBuf {
    path_to_user_home(home, principal).join(BINDINGS_FILE)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct StoredBinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    capture_raw_terminal: bool,
}

impl StoredBinding {
    fn is_empty(&self) -> bool {
        self.account_id.is_none() && !self.capture_raw_terminal
    }
}

/// One agent's binding as read back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Binding {
    pub account_id: String,
    /// False when the bound account no longer exists in the accounts store.
    /// The binding is reported, never silently dropped.
    pub account_exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingError {
    Agent(AgentError),
    AccountNotFound(String),
    RuntimeHasNoManagedAccounts(AgentCli),
    /// The account logs into a different CLI than the agent runs on.
    ProviderMismatch {
        account_id: String,
        provider: AgentProvider,
        runtime: AgentCli,
    },
    Io(String),
}

impl fmt::Display for BindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Agent(e) => write!(f, "{e}"),
            Self::AccountNotFound(id) => write!(f, "agent account {id} not found"),
            Self::RuntimeHasNoManagedAccounts(runtime) => write!(
                f,
                "{runtime} uses its own login and cannot be bound to a CodeSense account"
            ),
            Self::ProviderMismatch {
                account_id,
                provider,
                runtime,
            } => write!(
                f,
                "account {account_id} is a {} account but the agent runs on {runtime}",
                provider.as_str()
            ),
            Self::Io(e) => write!(f, "agent binding io error: {e}"),
        }
    }
}

impl std::error::Error for BindingError {}

impl From<AgentError> for BindingError {
    fn from(e: AgentError) -> Self {
        Self::Agent(e)
    }
}

/// Serializes read-modify-write of a bindings file within this process.
static BINDINGS_LOCK: Mutex<()> = Mutex::new(());

fn read_all(path: &Path) -> Result<BTreeMap<String, StoredBinding>, BindingError> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|e| BindingError::Io(format!("{}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(BindingError::Io(format!("{}: {e}", path.display()))),
    }
}

fn write_all(path: &Path, all: &BTreeMap<String, StoredBinding>) -> Result<(), BindingError> {
    write_json_atomic(path, all).map_err(|e| BindingError::Io(format!("{}: {e}", path.display())))
}

/// What this user keeps for one agent, as stored: the bound account, if any,
/// and the raw-terminal consent. Reading it touches only the bindings file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredEntry {
    pub account_id: Option<String>,
    pub capture_raw_terminal: bool,
}

/// This user's stored entry for `key`.
pub fn stored(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
) -> Result<StoredEntry, BindingError> {
    let stored = read_all(&bindings_file(home, principal))?
        .remove(&key.to_string())
        .unwrap_or_default();
    Ok(StoredEntry {
        account_id: stored.account_id,
        capture_raw_terminal: stored.capture_raw_terminal,
    })
}

/// This user's account binding for `key`, if any.
pub fn get(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
) -> Result<Option<Binding>, BindingError> {
    let Some(account_id) = stored(home, principal, key)?.account_id else {
        return Ok(None);
    };
    let account_exists = list_profiles(home, principal)
        .map_err(|e| BindingError::Io(e.to_string()))?
        .iter()
        .any(|p| p.id == account_id);
    Ok(Some(Binding {
        account_id,
        account_exists,
    }))
}

/// A change to this user's entry for one agent; `None` leaves that part as is.
#[derive(Debug, Clone, Copy, Default)]
pub struct BindingChange<'a> {
    pub account_id: Option<&'a str>,
    pub capture_raw_terminal: Option<bool>,
}

/// Apply `change` to this user's entry for `key`, all or nothing. The agent
/// must resolve, and a named account must exist and log into the agent's
/// runtime; otherwise nothing is written. Consent only affects interactive
/// launches: a headless launch never captures.
pub fn update(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    change: BindingChange<'_>,
) -> Result<StoredEntry, BindingError> {
    let runtime = service::read_current(home, principal, key)?.runtime;
    if let Some(account_id) = change.account_id {
        let profiles =
            list_profiles(home, principal).map_err(|e| BindingError::Io(e.to_string()))?;
        let account = profiles
            .iter()
            .find(|p| p.id == account_id)
            .ok_or_else(|| BindingError::AccountNotFound(account_id.to_string()))?;
        let Some(provider) = crate::agents::adapter::provider_of(runtime) else {
            return Err(BindingError::RuntimeHasNoManagedAccounts(runtime));
        };
        if account.provider != provider {
            return Err(BindingError::ProviderMismatch {
                account_id: account_id.to_string(),
                provider: account.provider,
                runtime,
            });
        }
    }
    let path = bindings_file(home, principal);
    let name = key.to_string();
    let _guard = BINDINGS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = read_all(&path)?;
    let before = all.get(&name).cloned().unwrap_or_default();
    let mut after = before.clone();
    if let Some(account_id) = change.account_id {
        after.account_id = Some(account_id.to_string());
    }
    if let Some(capture) = change.capture_raw_terminal {
        after.capture_raw_terminal = capture;
    }
    if after != before {
        if after.is_empty() {
            all.remove(&name);
        } else {
            all.insert(name.clone(), after.clone());
        }
        write_all(&path, &all)?;
        super::events::changed(&name);
    }
    Ok(StoredEntry {
        account_id: after.account_id,
        capture_raw_terminal: after.capture_raw_terminal,
    })
}

/// Bind `key` to `account_id` for this user; see [`update`]. Tests bind through it.
#[cfg(test)]
pub fn set(
    home: &Path,
    principal: &Principal,
    key: &AgentKey,
    account_id: &str,
) -> Result<Binding, BindingError> {
    update(
        home,
        principal,
        key,
        BindingChange {
            account_id: Some(account_id),
            capture_raw_terminal: None,
        },
    )?;
    Ok(Binding {
        account_id: account_id.to_string(),
        account_exists: true,
    })
}

/// Drop this user's account binding for `key`; the raw-terminal consent stays.
/// False when there was no binding.
pub fn clear(home: &Path, principal: &Principal, key: &AgentKey) -> Result<bool, BindingError> {
    let path = bindings_file(home, principal);
    let _guard = BINDINGS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = read_all(&path)?;
    let name = key.to_string();
    let Some(stored) = all.get_mut(&name) else {
        return Ok(false);
    };
    if stored.account_id.take().is_none() {
        return Ok(false);
    }
    if stored.is_empty() {
        all.remove(&name);
    }
    write_all(&path, &all)?;
    super::events::changed(&name);
    Ok(true)
}
