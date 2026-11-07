// SPDX-License-Identifier: MIT
//! Remote Machines: the machine registry and the SSH connector.
//!
//! The registry ([`registry`]) keeps each machine's shareable declaration in
//! a context layer — personal, or one team's — and each person's binding
//! (how they sign in) outside every repo. The connector, the Add-machine
//! checklist and the install are `codesense-session-server`'s, the same code
//! the container tests drive; this module only gathers their inputs and
//! shapes their answers for the Machines UI. Connecting and keeping a
//! connection belong to the connection manager, not here.

mod passphrase;
pub(crate) mod registry;
#[cfg(test)]
pub(crate) mod rm4f_support;
pub(crate) mod terminal_server_info;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use codesense_session_server::askpass::{self, Askpass};
use codesense_session_server::check::{self, Bundled, NextStep, Status, StepId, Trust};
use codesense_session_server::install;
use codesense_session_server::ssh::{self, Address, Connector, HostKey, Mux, Paths, SignIn};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Manager;

use crate::commands::credential_store::{default_store, CredentialStore, Secret};
use crate::domain::{CredentialRef, Principal};
use registry::{Binding, Declaration, Registry, Scope};

/// This Mac's id: local is a machine like any other, always first.
pub const LOCAL_MACHINE_ID: &str = "local";

/// Lossless IPC representation of an owner-issued counter. Numeric ordering is
/// retained in Rust; JavaScript receives canonical decimal strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuntimeCounter(u64);

impl RuntimeCounter {
    pub(crate) fn new(value: u64) -> Self {
        Self(value)
    }
}

impl Serialize for RuntimeCounter {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for RuntimeCounter {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        let number = value.parse::<u64>().map_err(serde::de::Error::custom)?;
        if number.to_string() != value {
            return Err(serde::de::Error::custom("expected canonical decimal u64"));
        }
        Ok(Self(number))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeMemberSlot {
    InProcess,
    Sidecar,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "RuntimeMemberAddressWire")]
pub struct RuntimeMemberAddress {
    pub machine_id: String,
    pub slot: RuntimeMemberSlot,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeMemberAddressWire {
    machine_id: String,
    slot: RuntimeMemberSlot,
}

impl TryFrom<RuntimeMemberAddressWire> for RuntimeMemberAddress {
    type Error = &'static str;
    fn try_from(value: RuntimeMemberAddressWire) -> Result<Self, Self::Error> {
        if value.machine_id.is_empty()
            || (value.machine_id == LOCAL_MACHINE_ID) == (value.slot == RuntimeMemberSlot::Remote)
        {
            return Err("runtime slot does not belong to this machine");
        }
        Ok(Self {
            machine_id: value.machine_id,
            slot: value.slot,
        })
    }
}

impl RuntimeMemberAddress {
    pub(crate) fn local(slot: RuntimeMemberSlot) -> Self {
        assert!(
            slot != RuntimeMemberSlot::Remote,
            "local owner cannot issue a remote member"
        );
        Self {
            machine_id: LOCAL_MACHINE_ID.into(),
            slot,
        }
    }
    pub(crate) fn remote(machine_id: &str) -> Self {
        Self {
            machine_id: machine_id.into(),
            slot: RuntimeMemberSlot::Remote,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeNamespace {
    pub server_id: String,
    pub epoch: String,
    pub generation: RuntimeCounter,
}

impl From<&crate::terminal::session_server::EventNamespace> for RuntimeNamespace {
    fn from(value: &crate::terminal::session_server::EventNamespace) -> Self {
        Self {
            server_id: value.server_id.clone(),
            epoch: value.epoch.clone(),
            generation: RuntimeCounter::new(value.generation()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FrozenRuntimeLayout {
    pub last_namespace: Option<RuntimeNamespace>,
    pub server_id: String,
    pub epoch: String,
    pub snapshot: codesense_pty_core::layout::LayoutSnapshot,
    pub reason: RuntimeHistoryReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeHistoryReason {
    Incompatible,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeAdmissionState {
    Live {
        namespace: RuntimeNamespace,
    },
    Historical {
        historical: Box<FrozenRuntimeLayout>,
    },
    Unavailable,
    Removed,
}

impl Serialize for RuntimeAdmissionState {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let (state, namespace, historical) = match self {
            Self::Live { namespace } => ("live", Some(namespace), None),
            Self::Historical { historical } => ("historical", None, Some(historical)),
            Self::Unavailable => ("unavailable", None, None),
            Self::Removed => ("removed", None, None),
        };
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("state", state)?;
        map.serialize_entry("namespace", &namespace)?;
        map.serialize_entry("historical", &historical)?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for RuntimeAdmissionState {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            state: String,
            namespace: Option<RuntimeNamespace>,
            historical: Option<Box<FrozenRuntimeLayout>>,
        }
        let wire = Wire::deserialize(deserializer)?;
        match (wire.state.as_str(), wire.namespace, wire.historical) {
            ("live", Some(namespace), None) => Ok(Self::Live { namespace }),
            ("historical", None, Some(historical)) => Ok(Self::Historical { historical }),
            ("unavailable", None, None) => Ok(Self::Unavailable),
            ("removed", None, None) => Ok(Self::Removed),
            _ => Err(serde::de::Error::custom("invalid runtime admission state")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeAdmissionRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_authority_epoch: Option<String>,
    pub member: RuntimeMemberAddress,
    pub revision: RuntimeCounter,
    #[serde(flatten)]
    pub state: RuntimeAdmissionState,
}

impl RuntimeAdmissionRow {
    #[cfg(test)]
    pub(crate) fn live(
        member: RuntimeMemberAddress,
        revision: u64,
        namespace: &crate::terminal::session_server::EventNamespace,
    ) -> Self {
        Self {
            member,
            revision: RuntimeCounter::new(revision),
            private_authority_epoch: None,
            state: RuntimeAdmissionState::Live {
                namespace: namespace.into(),
            },
        }
    }

    pub(crate) fn matches(&self, target: &RuntimeMemberTarget) -> bool {
        self.member == target.member
            && self.revision == target.admission_revision
            && matches!(&self.state, RuntimeAdmissionState::Live { namespace } if namespace == &target.namespace)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeMemberTarget {
    pub member: RuntimeMemberAddress,
    pub admission_revision: RuntimeCounter,
    pub namespace: RuntimeNamespace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeAdmissionBaseline {
    pub rows: Vec<RuntimeAdmissionRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum RuntimeLayoutReply {
    Live {
        #[serde(
            default,
            rename = "privateAuthorityEpoch",
            skip_serializing_if = "Option::is_none"
        )]
        private_authority_epoch: Option<String>,
        #[serde(flatten)]
        target: RuntimeMemberTarget,
        snapshot: Box<codesense_pty_core::layout::LayoutSnapshot>,
    },
    Historical {
        #[serde(
            default,
            rename = "privateAuthorityEpoch",
            skip_serializing_if = "Option::is_none"
        )]
        private_authority_epoch: Option<String>,
        member: RuntimeMemberAddress,
        #[serde(rename = "admissionRevision")]
        admission_revision: RuntimeCounter,
        frozen: Box<FrozenRuntimeLayout>,
    },
    Unavailable {
        member: RuntimeMemberAddress,
        #[serde(rename = "admissionRevision")]
        admission_revision: RuntimeCounter,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSessionTarget {
    #[serde(flatten)]
    pub runtime: RuntimeMemberTarget,
    pub session: codesense_pty_core::domain::PtySessionId,
}

/// The legacy branch is local-only and requires a unique live owner. An object
/// must decode as a complete admitted target; it can never fall back to an id.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RuntimeSessionReference {
    Admitted(Box<RuntimeSessionTarget>),
    Legacy(String),
}

impl RuntimeSessionReference {
    pub(crate) fn session(&self) -> &str {
        match self {
            Self::Admitted(target) => target.session.as_str(),
            Self::Legacy(session) => session,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeWorkspaceTarget {
    #[serde(flatten)]
    pub runtime: RuntimeMemberTarget,
    pub workspace: codesense_pty_core::domain::WorkspaceKey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RuntimeWorkspaceReference {
    Admitted(Box<RuntimeWorkspaceTarget>),
    Legacy(codesense_pty_core::domain::WorkspaceKey),
}

impl RuntimeWorkspaceReference {
    pub(crate) fn workspace(&self) -> &codesense_pty_core::domain::WorkspaceKey {
        match self {
            Self::Admitted(target) => &target.workspace,
            Self::Legacy(key) => key,
        }
    }
}

/// The id a machine being added connects as, until it is saved.
const DRAFT_ID: &str = "draft";

// ---------------------------------------------------------------------------
// The contract (mirrors `src/tauri/commands.ts`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineDeclarationDto {
    pub mode: MachineMode,
    pub host: String,
    pub port: u16,
    pub jump_host: Option<String>,
    pub default_base_dir: Option<String>,
    pub pinned_host_key_fp: Option<String>,
    pub pinned_server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_change: Option<identity::IdentityChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineBindingDto {
    pub username: Option<String>,
    pub key_path: Option<String>,
    pub credential_ref: Option<CredentialRef>,
    pub auto_connect: bool,
    pub hidden: bool,
    /// It held open tabs when last seen and connects at launch so they come
    /// back (never for a sign-in that could prompt).
    pub reconnect_at_launch: bool,
}

/// What the registry knows of a machine's state. Live connection states
/// come from the connection manager.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum MachineStatus {
    Connected {
        workspaces: Option<usize>,
        /// SW-XS-6: the running terminal server is not the build this app
        /// ships (or lacks a capability this app needs), even when both report
        /// the same version: offer "Update terminal server".
        #[serde(
            rename = "terminalServerUpdate",
            skip_serializing_if = "std::ops::Not::not"
        )]
        terminal_server_update: bool,
        /// With an update: the sessions the running server holds, which the
        /// update ends (§11.13).
        #[serde(
            rename = "terminalServerSessions",
            skip_serializing_if = "Option::is_none"
        )]
        terminal_server_sessions: Option<u32>,
    },
    Connecting,
    Handshaking,
    TerminalServerFrozen {
        count: Option<u32>,
        #[serde(rename = "replacementSupported")]
        replacement_supported: bool,
    },
    ReplacementReserved {
        count: Option<u32>,
    },
    Replacing {
        count: Option<u32>,
    },
    Readmitting {
        count: Option<u32>,
    },
    ReplacementFailed {
        #[serde(rename = "oldStopped")]
        old_stopped: bool,
        count: Option<u32>,
        repair: String,
    },
    ConnectionFailed {
        #[serde(rename = "lastError")]
        last_error: String,
    },
    Disconnected,
    Idle,
    NeedsCredential,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineCapabilities {
    pub agent_callbacks: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineSummary {
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub machine_stats: Option<codesense_host_proto::machine_stats::MachineStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine_stats_supported: Option<bool>,
    pub machine_id: String,
    pub kind: &'static str,
    pub label: String,
    pub scope: Option<Scope>,
    pub declaration: Option<MachineDeclarationDto>,
    pub binding: Option<MachineBindingDto>,
    pub can_publish: bool,
    pub remote_mcp_control: bool,
    pub status: MachineStatus,
    pub attention: u32,
    pub capabilities: MachineCapabilities,
    /// SW-AGR-19: a connected machine's running terminal server next to this
    /// app (version, up to date, and This Mac's executable and start). Absent
    /// while not connected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_server: Option<terminal_server_info::TerminalServerInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfigHostDto {
    pub alias: String,
    pub host_name: String,
    pub user: Option<String>,
    pub port: u16,
    pub proxy_jump: Option<String>,
    pub identity_file: Option<String>,
    pub proxy_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshAgentKeyDto {
    pub key_type: String,
    pub fingerprint: String,
    pub comment: String,
    pub public_key: String,
    /// A public key file naming exactly this key: signing in with it as the
    /// key file (`-i` with `IdentitiesOnly`) offers the agent only this key.
    pub identity_file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshLocalSources {
    pub config_hosts: Vec<SshConfigHostDto>,
    pub agent_keys: Vec<SshAgentKeyDto>,
}

/// How the person signs in. A passphrase crosses IPC only as the keychain
/// handle [`capture_machine_passphrase`] returned.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "method", rename_all = "camelCase")]
pub enum MachineSignIn {
    Agent,
    #[serde(rename_all = "camelCase")]
    KeyFile {
        key_path: String,
    },
    #[serde(rename_all = "camelCase")]
    KeychainPassphrase {
        key_path: String,
        credential_ref: CredentialRef,
    },
}

impl MachineSignIn {
    fn key_path(&self) -> Option<&str> {
        match self {
            Self::Agent => None,
            Self::KeyFile { key_path } | Self::KeychainPassphrase { key_path, .. } => {
                Some(key_path)
            }
        }
    }

    fn credential_ref(&self) -> Option<&CredentialRef> {
        match self {
            Self::KeychainPassphrase { credential_ref, .. } => Some(credential_ref),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MachineMode {
    Legacy,
    PrivateAccount,
}

/// The mode an operation on `input` runs in. Only a private account opens
/// terminal tabs (admission is granted per signed-in owner, and a legacy
/// shared account has none), so a new machine is private unless the caller
/// asks for legacy. A saved machine keeps its own mode unless the caller
/// names one: switching a legacy machine is always an explicit request.
fn effective_mode(
    env: &Env,
    machine_id: Option<&str>,
    input: &MachineDraftInput,
) -> Result<MachineMode, String> {
    if let Some(mode) = input.mode {
        return Ok(mode);
    }
    Ok(match machine_id {
        Some(id) if !private_setup::claims(env, id)? && env.registry.find(id)?.is_some() => {
            MachineMode::Legacy
        }
        _ => MachineMode::PrivateAccount,
    })
}

pub mod identity;
pub(crate) mod legacy_retire;
pub(crate) mod local_update;
mod private_setup;
pub(crate) mod reinstall;
pub(crate) mod restore;
pub(crate) mod resume;
pub(crate) mod resume_brief;
pub(crate) mod strays;
pub(crate) mod update_all;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineDraftInput {
    /// Absent: a saved machine's own mode, else a private account.
    #[serde(default)]
    pub mode: Option<MachineMode>,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub jump_host: Option<String>,
    pub sign_in: MachineSignIn,
    pub trusted_host_key_fp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineTestStepDto {
    pub id: &'static str,
    pub status: &'static str,
    pub detail: String,
    pub next_steps: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPlanDto {
    pub size_bytes: u64,
    pub target_dir: String,
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineTestReportDto {
    pub steps: Vec<MachineTestStepDto>,
    pub install_plan: Option<InstallPlanDto>,
    pub file_view_only: bool,
}

/// An install's outcome. A failed one is always safe to run again: an
/// install is idempotent and never leaves a partial version in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum InstallTerminalServerResult {
    #[serde(rename_all = "camelCase")]
    Installed {
        ok: bool,
        version: String,
        /// Set when the machine ends the terminal server at logout: what
        /// happens, and the commands that prevent it.
        logout_warning: Option<String>,
    },
    Failed {
        ok: bool,
        code: &'static str,
        message: String,
    },
}

// ---------------------------------------------------------------------------
// The environment the commands run in
// ---------------------------------------------------------------------------

/// Everything a machine operation needs from the app, resolved once.
pub(crate) struct Env {
    pub private_authority:
        Option<crate::commands::auth::private_authority::PrivateAuthoritySnapshot>,
    pub registry: Registry,
    pub codesense_home: PathBuf,
    pub home: Option<PathBuf>,
    /// This app's terminal-server builds for other machines.
    pub artifacts: PathBuf,
    pub shell_init: PathBuf,
    /// The askpass program: the session-server binary.
    pub askpass_program: PathBuf,
    /// The `ssh` program.
    pub ssh_program: PathBuf,
}

impl Env {
    fn ssh_dir(&self) -> PathBuf {
        self.codesense_home.join("ssh")
    }

    fn paths(&self) -> Paths {
        Paths::under(&self.codesense_home)
    }

    fn scratch(&self) -> PathBuf {
        self.ssh_dir().join("scan")
    }

    fn askpass_dir(&self) -> PathBuf {
        self.ssh_dir().join("askpass")
    }

    /// A fresh directory for pins a check makes: a check changes no saved
    /// machine's pin, and two drafts checked at once never share one.
    fn throwaway_pins(&self) -> Result<Throwaway, String> {
        let path = self
            .scratch()
            .join(format!("pins-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&path).map_err(|e| format!("create {path:?}: {e}"))?;
        Ok(Throwaway { path })
    }

    fn bundled(&self) -> Bundled<'_> {
        Bundled {
            artifacts: &self.artifacts,
            shell_init: &self.shell_init,
        }
    }

    /// `~/…` against the user's home.
    fn expand(&self, path: &str) -> PathBuf {
        match (path.strip_prefix("~/"), &self.home) {
            (Some(rest), Some(home)) => home.join(rest),
            _ => PathBuf::from(path),
        }
    }

    /// The connector's own directories, owner-only.
    fn prepare_dirs(&self) -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt;
        let paths = self.paths();
        for dir in [self.ssh_dir(), paths.known_hosts_dir, paths.control_dir] {
            std::fs::create_dir_all(&dir).map_err(|e| format!("create {dir:?}: {e}"))?;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("protect {dir:?}: {e}"))?;
        }
        Ok(())
    }
}

/// A directory removed when dropped.
struct Throwaway {
    path: PathBuf,
}

impl Drop for Throwaway {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A machine ready to reach: its connector, and the askpass serving its
/// key's passphrase for as long as this lives.
struct Reach {
    connector: Connector,
    askpass: Option<Askpass>,
}

/// The SSH-policy-authenticated byte transport RM-4 consumes. RM-4 must still
/// perform greeting-first protocol admission on these exact bytes, checking
/// the greeted server id against `expected_server_id` before publication.
/// Callers receive no [`Connector`], [`install::Ssh`], argv, pin path, or
/// credential material.
pub struct PinnedSshTransport {
    pub(crate) private_lease:
        Option<crate::commands::auth::private_authority::PrivateAuthorityLease>,
    pub reader: ChildStdout,
    pub writer: ChildStdin,
    pub child_guard: SshProcessGuard,
    /// The declaration's authoritative, secret-free terminal-server identity.
    pub expected_server_id: String,
    /// A digest of the declaration, personal binding, and pin snapshot used to
    /// construct this transport. The constructor revalidates it after spawn.
    pub generation: String,
}

/// Sealed evidence that the pinned SSH control plane identified the canonical
/// protocol-1 daemon without opening its terminal data stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacyHostProof {
    server_id: String,
    version: String,
    manifest_digest: String,
    pid: u32,
    process_start: u64,
    executable_identity: String,
    executable: String,
    root: String,
    uid: u32,
}

impl LegacyHostProof {
    pub(crate) fn expected_server_id(&self) -> &str {
        &self.server_id
    }

    pub(crate) fn expected_pid(&self) -> u32 {
        self.pid
    }

    #[cfg(test)]
    pub(crate) fn server_id(&self) -> &str {
        &self.server_id
    }

    #[cfg(test)]
    pub(crate) fn test_proof(server_id: &str) -> Self {
        Self {
            server_id: server_id.into(),
            version: "v1".into(),
            manifest_digest: "a".repeat(64),
            pid: 42,
            process_start: 1,
            executable_identity: "1:1".into(),
            executable: "/home/test/.codesense/host/v1/codesense-session-server".into(),
            root: "/home/test/.codesense".into(),
            uid: 1000,
        }
    }

    pub(crate) fn authorization_record(&self, nonce: &str, epoch: &str) -> String {
        let (executable_dev, executable_ino) = self
            .executable_identity
            .split_once(':')
            .expect("validated executable identity");
        format!(
            "schema=codesense-replace-v2\nuid={}\npid={}\nstart={}\nexecutable={}\nexecutable_dev={}\nexecutable_ino={}\nroot={}\nmanifest={}\nversion={}\nserver_id={}\nnonce={}\nepoch={}\n",
            self.uid,
            self.pid,
            self.process_start,
            self.executable,
            executable_dev,
            executable_ino,
            self.root,
            self.manifest_digest,
            self.version,
            self.server_id,
            nonce,
            epoch,
        )
    }
}

fn legacy_control_script() -> install::Step {
    install::Step::new(format!(
        r#"
set -eu
H="$HOME/{host}"
v=$(readlink "$H/{current}" 2>/dev/null || true)
case "$v" in ''|*[!A-Za-z0-9._-]*) exit 20;; esac
root=$(readlink -f "$HOME/.codesense")
H=$(readlink -f "$H")
d=$(readlink -f "$H/$v")
[ "$H" = "$root/host" ] && [ "$d" = "$root/host/$v" ] || exit 21
[ -x "$d/{binary}" ] && [ -f "$d/.codesense-manifest" ] || exit 22
if command -v sha256sum >/dev/null 2>&1; then sum='sha256sum'; else sum='shasum -a 256'; fi
(cd "$d" && $sum -c .codesense-manifest >/dev/null 2>&1) || exit 23
digest=$($sum "$d/.codesense-manifest" | awk '{{print $1}}')
pid=$(ps -ax -o pid= -o command= | awk -v x="$d/{binary}" '$2 == x {{print $1; exit}}')
case "$pid" in ''|*[!0-9]*) exit 24;; esac
start=$(awk '{{print $22}}' "/proc/$pid/stat" 2>/dev/null || true)
case "$start" in ''|*[!0-9]*) exit 26;; esac
inode=$(stat -Lc '%d:%i' "$d/{binary}" 2>/dev/null || true)
running_inode=$(stat -Lc '%d:%i' "/proc/$pid/exe" 2>/dev/null || true)
[ -n "$inode" ] && [ "$running_inode" = "$inode" ] || exit 28
sid=$(cat "$HOME/.codesense/pty-server-id" 2>/dev/null || true)
[ -n "$sid" ] || exit 25
exe=$(readlink -f "$d/{binary}")
[ "$exe" = "$root/host/$v/{binary}" ] || exit 32
uid=$(id -u)
# Re-read the stable process identity at the final edge of proof publication.
[ "$start" = "$(awk '{{print $22}}' "/proc/$pid/stat" 2>/dev/null || true)" ] || exit 29
[ "$inode" = "$(stat -Lc '%d:%i' "$d/{binary}" 2>/dev/null || true)" ] || exit 30
[ "$inode" = "$(stat -Lc '%d:%i' "/proc/$pid/exe" 2>/dev/null || true)" ] || exit 31
printf 'server_id=%s\nversion=%s\nmanifest=%s\npid=%s\nstart=%s\ninode=%s\nexecutable=%s\nroot=%s\nuid=%s\n' "$sid" "$v" "$digest" "$pid" "$start" "$inode" "$exe" "$root" "$uid"
"#,
        host = install::HOST_DIR,
        current = install::CURRENT,
        binary = install::BINARY,
    ))
}

fn parse_legacy_host_proof(output: &[u8], expected_server_id: &str) -> Option<LegacyHostProof> {
    let text = std::str::from_utf8(output).ok()?;
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix('='))
    };
    let server_id = field("server_id")?;
    let version = field("version")?;
    let manifest_digest = field("manifest")?;
    let pid = field("pid")?.parse().ok()?;
    let process_start = field("start")?.parse().ok()?;
    let executable_identity = field("inode")?;
    let executable = field("executable")?;
    let root = field("root")?;
    let uid = field("uid")?.parse().ok()?;
    if server_id != expected_server_id
        || version.is_empty()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || manifest_digest.len() != 64
        || !manifest_digest.bytes().all(|b| b.is_ascii_hexdigit())
        || pid == 0
        || process_start == 0
        || !executable_identity
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b':')
        || executable_identity.matches(':').count() != 1
        || !executable.starts_with('/')
        || !root.starts_with('/')
    {
        return None;
    }
    Some(LegacyHostProof {
        server_id: server_id.into(),
        version: version.into(),
        manifest_digest: manifest_digest.into(),
        pid,
        process_start,
        executable_identity: executable_identity.into(),
        executable: executable.into(),
        root: root.into(),
        uid,
    })
}

/// Result categories RM-4 can handle while admitting a newly opened stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinnedTransportGenerationError {
    /// The saved declaration, personal binding, or verified pin is no longer
    /// available as one coherent attachable machine snapshot.
    Missing(String),
    /// The machine still exists, but its attach inputs changed.
    Stale,
}

/// Structured failure from the pinned transport open boundary. Only the
/// terminal-version check may nominate the separately verified legacy path.
#[derive(Debug)]
pub enum PinnedSshOpenError {
    LegacyProtocolCandidate(String),
    Other(String),
}

impl std::fmt::Display for PinnedSshOpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LegacyProtocolCandidate(detail) | Self::Other(detail) => {
                formatter.write_str(detail)
            }
        }
    }
}

impl From<String> for PinnedSshOpenError {
    fn from(value: String) -> Self {
        Self::Other(value)
    }
}

impl From<&str> for PinnedSshOpenError {
    fn from(value: &str) -> Self {
        Self::Other(value.into())
    }
}

impl std::fmt::Display for PinnedTransportGenerationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(detail) => {
                write!(formatter, "machine transport is unavailable: {detail}")
            }
            Self::Stale => formatter.write_str("machine transport generation is stale"),
        }
    }
}

impl std::error::Error for PinnedTransportGenerationError {}

/// Owns one attached SSH process group and the askpass server (when the saved
/// binding uses one). Normal disconnect/shutdown must call
/// [`SshProcessGuard::close_and_reap`] on the blocking executor. Drop is a
/// bounded fail-safe: it kills synchronously and guarantees an eventual
/// detached reap without blocking an async worker indefinitely.
pub struct SshProcessGuard {
    private_bootstrap: Option<codesense_session_server::private_account::Bootstrap>,
    child: Option<Child>,
    _askpass: Option<Askpass>,
}

impl SshProcessGuard {
    fn signal_group(child: &mut Child) {
        let pid = child.id();
        // SAFETY: spawn_pinned creates a fresh process group led by this exact
        // child. A negative pid addresses only that owned group.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
        let _ = child.kill();
    }

    /// Terminate and synchronously reap the SSH process group. RM-4 calls this
    /// on the blocking executor during explicit disconnect/shutdown; it is
    /// safe after natural child exit. It consumes the guard, making double
    /// close impossible for the owning connection manager.
    pub fn close_and_reap(mut self) -> Result<(), String> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        Self::signal_group(&mut child);
        let result = child
            .wait()
            .map(|_| ())
            .map_err(|e| format!("reap SSH attach: {e}"));
        // Explicit close is on a blocking worker; keep Askpass alive through
        // the bounded cleanup SSH invocation for encrypted-key accounts.
        drop(self.private_bootstrap.take());
        result
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("SSH process guard has a child")
    }
}

impl Drop for SshProcessGuard {
    fn drop(&mut self) {
        if let Some(bootstrap) = self.private_bootstrap.take() {
            let askpass = self._askpass.take();
            std::thread::spawn(move || {
                drop(bootstrap);
                drop(askpass);
            });
        }
        let Some(mut child) = self.child.take() else {
            return;
        };
        Self::signal_group(&mut child);
        // Drop may run on an async worker: bound its synchronous fallback.
        // The group is already dead; if reaping is momentarily delayed, a
        // dedicated owner finishes it rather than leaking the Child handle.
        #[cfg(test)]
        let defer = DEFER_DROP_REAP.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let defer = false;
        let polls = if defer { 0 } else { 100 };
        for _ in 0..polls {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
        std::thread::spawn(move || {
            #[cfg(test)]
            if defer {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            let _ = child.wait();
        });
    }
}

#[cfg(test)]
thread_local! {
    static DEFER_DROP_REAP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn spawn_pinned(
    ssh: install::Ssh,
    version: &str,
    askpass: Option<Askpass>,
) -> Result<(ChildStdout, ChildStdin, SshProcessGuard), String> {
    spawn_pinned_script(ssh, &install::attach_script(version)?, askpass)
}

fn spawn_pinned_script(
    ssh: install::Ssh,
    script: &str,
    askpass: Option<Askpass>,
) -> Result<(ChildStdout, ChildStdin, SshProcessGuard), String> {
    use std::os::unix::process::CommandExt;
    let mut command = ssh.command(script);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Protocol bytes are stdout-only. Inherited/null stderr also avoids a
        // hidden pipe filling and deadlocking a long-lived transport.
        .stderr(Stdio::null());
    // SAFETY: pre_exec invokes only the async-signal-safe setpgid syscall.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let child = command
        .spawn()
        .map_err(|e| format!("start SSH attach: {e}"))?;
    // Establish ownership immediately: every later early return runs Drop and
    // kills/reaps the whole group.
    let mut guard = SshProcessGuard {
        private_bootstrap: None,
        child: Some(child),
        _askpass: askpass,
    };
    let reader = guard
        .child_mut()
        .stdout
        .take()
        .ok_or("SSH attach has no output")?;
    let writer = guard
        .child_mut()
        .stdin
        .take()
        .ok_or("SSH attach has no input")?;
    Ok((reader, writer, guard))
}

#[cfg(test)]
pub(crate) fn test_pinned_transport(
    program: &std::path::Path,
) -> Result<(PinnedSshTransport, libc::pid_t), String> {
    let (reader, writer, child_guard) =
        spawn_pinned(install::Ssh::new(program, Vec::new()), "1.0.0", None)?;
    let pid = child_guard
        .child
        .as_ref()
        .expect("test guard owns child")
        .id() as libc::pid_t;
    Ok((
        PinnedSshTransport {
            reader,
            writer,
            child_guard,
            expected_server_id: "test-server".into(),
            generation: "test-generation".into(),
            private_lease: None,
        },
        pid,
    ))
}

impl Reach {
    fn ssh(&self, mux: Mux) -> install::Ssh {
        self.connector.ssh(mux, self.askpass.as_ref())
    }
}

fn reach(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: &str,
    input: &MachineDraftInput,
) -> Result<Reach, String> {
    identity::validate_saved(env, machine_id, None)?;
    env.prepare_dirs()?;
    let address = Address::new(
        input.host.trim(),
        input.port,
        input
            .username
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty()),
        input
            .jump_host
            .as_deref()
            .map(str::trim)
            .filter(|j| !j.is_empty()),
    )?;
    let (sign_in, askpass) = match &input.sign_in {
        MachineSignIn::Agent => (SignIn::Agent, None),
        MachineSignIn::KeyFile { key_path } => (SignIn::Key(env.expand(key_path)), None),
        MachineSignIn::KeychainPassphrase {
            key_path,
            credential_ref,
        } => {
            let secret = passphrase::load(store, credential_ref)?;
            let key = env.expand(key_path);
            let askpass = Askpass::serve(
                &env.askpass_dir(),
                &env.askpass_program,
                &key,
                secret.expose(),
            )
            .map_err(|e| format!("serve the key's passphrase: {e}"))?;
            (SignIn::KeyWithPassphrase(key), Some(askpass))
        }
    };
    let connector =
        Connector::new(machine_id, address, sign_in, env.paths())?.with_program(&env.ssh_program);
    #[cfg(test)]
    let connector = match std::env::var_os("RM4F_SCAN_PROGRAM") {
        Some(program) => connector.with_scan_program(PathBuf::from(program)),
        None => connector,
    };
    Ok(Reach { connector, askpass })
}

/// The key a saved machine's check holds its host to: its pin, while the
/// address is the one it was pinned for. A new address is a first sight.
fn pinned_key(
    existing: Option<&Declaration>,
    input: &MachineDraftInput,
) -> Result<Option<HostKey>, String> {
    match existing {
        Some(decl) if decl.host == input.host.trim() && decl.port == input.port => decl
            .pinned_host_key
            .as_deref()
            .map(HostKey::parse)
            .transpose(),
        _ => Ok(None),
    }
}

fn trust<'a>(pinned: Option<&'a HostKey>, input: &'a MachineDraftInput) -> Trust<'a> {
    match pinned {
        Some(key) => Trust::Pinned(key),
        None => Trust::Trusted(input.trusted_host_key_fp.as_deref()),
    }
}

/// The saved machine `machine_id` names, if any.
fn existing(env: &Env, machine_id: Option<&str>) -> Result<Option<(Scope, Declaration)>, String> {
    if machine_id
        .map(|id| private_setup::claims(env, id))
        .transpose()?
        .unwrap_or(false)
    {
        return Err("private machine requires private setup".into());
    }
    match machine_id {
        Some(id) => env
            .registry
            .find(id)?
            .map(Some)
            .ok_or_else(|| format!("No machine {id:?}")),
        None => Ok(None),
    }
}

fn stored_sign_in(binding: &Binding) -> Result<MachineSignIn, String> {
    match (&binding.key_path, &binding.credential_ref) {
        (None, None) => Ok(MachineSignIn::Agent),
        (Some(key_path), None) => Ok(MachineSignIn::KeyFile {
            key_path: key_path.clone(),
        }),
        (Some(key_path), Some(credential_ref)) => Ok(MachineSignIn::KeychainPassphrase {
            key_path: key_path.clone(),
            credential_ref: credential_ref.clone(),
        }),
        (None, Some(_)) => Err("the saved machine credential has no key file".into()),
    }
}

#[derive(Debug)]
struct SavedTransportSnapshot {
    declaration: Declaration,
    binding: Binding,
    generation: String,
}

fn transport_snapshot(env: &Env, machine_id: &str) -> Result<SavedTransportSnapshot, String> {
    let transaction = save_transaction(machine_id);
    let _transaction = transaction.lock().unwrap_or_else(|e| e.into_inner());
    let (scope, declaration) = existing(env, Some(machine_id))?.ok_or("no machine")?;
    let binding = env
        .registry
        .bindings()?
        .remove(machine_id)
        .ok_or_else(|| format!("Machine {machine_id:?} needs personal sign-in setup"))?;
    if !binding.set_up {
        return Err(format!(
            "Machine {machine_id:?} needs setup before connecting"
        ));
    }
    binding.validate_declaration(&declaration)?;
    let pin_path = env.paths().pin_file(machine_id);
    let pin = std::fs::read(&pin_path)
        .map_err(|e| format!("read verified host-key pin {pin_path:?}: {e}"))?;
    let mut digest = Sha256::new();
    digest.update(
        serde_json::to_vec(&(&scope, &declaration, &binding))
            .map_err(|e| format!("encode machine transport snapshot: {e}"))?,
    );
    digest.update([0]);
    digest.update(pin);
    let generation = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(SavedTransportSnapshot {
        declaration,
        binding,
        generation,
    })
}

fn revalidate_transport_generation(
    env: &Env,
    machine_id: &str,
    generation: &str,
) -> Result<(), PinnedTransportGenerationError> {
    if private_setup::claims(env, machine_id).map_err(PinnedTransportGenerationError::Missing)? {
        return if private_setup::generation(env, machine_id)
            .map_err(PinnedTransportGenerationError::Missing)?
            == generation
        {
            Ok(())
        } else {
            Err(PinnedTransportGenerationError::Stale)
        };
    }
    let current =
        transport_snapshot(env, machine_id).map_err(PinnedTransportGenerationError::Missing)?;
    if current.generation == generation {
        Ok(())
    } else {
        Err(PinnedTransportGenerationError::Stale)
    }
}

fn publish_pinned_transport(
    env: &Env,
    machine_id: &str,
    expected_server_id: String,
    generation: String,
    reader: ChildStdout,
    writer: ChildStdin,
    child_guard: SshProcessGuard,
) -> Result<PinnedSshTransport, String> {
    match transport_snapshot(env, machine_id) {
        Ok(current) if current.generation == generation => Ok(PinnedSshTransport {
            private_lease: None,
            reader,
            writer,
            child_guard,
            expected_server_id,
            generation,
        }),
        Ok(_) | Err(_) => {
            let _ = child_guard.close_and_reap();
            Err(format!(
                "stale machine binding: {machine_id:?} changed while SSH was opening"
            ))
        }
    }
}

/// Authenticate and open a saved machine's pinned SSH stdio relay. Every connection input is
/// resolved from RM-3's declaration and personal binding; RM-4 supplies only
/// the stable machine id and cannot weaken SSH arguments, authentication,
/// host-key pinning, or terminal-server identity verification.
#[cfg(test)]
fn open_pinned_saved(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: &str,
) -> Result<PinnedSshTransport, PinnedSshOpenError> {
    open_pinned_saved_with(env, store, machine_id, &OpenProgress::default())
}

fn open_pinned_saved_with(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: &str,
    progress: &OpenProgress,
) -> Result<PinnedSshTransport, PinnedSshOpenError> {
    if private_setup::claims(env, machine_id)? {
        return private_setup::open(env, store, machine_id, progress).map_err(Into::into);
    }
    let snapshot = transport_snapshot(env, machine_id)?;
    let declaration = &snapshot.declaration;
    let binding = &snapshot.binding;
    let pinned = declaration
        .pinned_host_key
        .as_deref()
        .map(HostKey::parse)
        .transpose()?
        .ok_or_else(|| format!("Machine {machine_id:?} has no verified host key"))?;
    let expected_server_id = identity::expected_server_id(binding, machine_id)?;
    let input = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: declaration.label.clone(),
        host: declaration.host.clone(),
        port: declaration.port,
        username: binding.username.clone(),
        jump_host: declaration.jump_host.clone(),
        sign_in: stored_sign_in(binding)?,
        trusted_host_key_fp: None,
    };
    let reach = reach(env, store, machine_id, &input)?;
    progress.enter("reading the installed terminal protocol");
    let current_protocol = reach
        .ssh(Mux::Fresh)
        .output_within(
            &install::Step::new(format!(
                "cat \"$HOME/{}/{}/{}\" 2>/dev/null || true",
                install::HOST_DIR,
                install::CURRENT,
                install::PROTOCOL_MAJOR_FILE
            )),
            check::GREETING_TIMEOUT,
        )
        .map_err(PinnedSshOpenError::Other)?;
    if String::from_utf8_lossy(&current_protocol.stdout).trim() == "1" {
        return Err(PinnedSshOpenError::LegacyProtocolCandidate(
            "installed terminal protocol 1 requires control-plane verification".into(),
        ));
    }
    progress.enter("checking the host key and terminal server");
    let report = check::run(
        &reach.connector,
        Trust::Pinned(&pinned),
        reach.askpass.as_ref(),
        env.bundled(),
        &env.scratch(),
    );
    if !report.passed() {
        let problem = report
            .first_problem()
            .map(|step| step.detail.clone())
            .unwrap_or_else(|| "the connection check did not pass".into());
        let terminal_version_failure = report
            .first_problem()
            .is_some_and(|step| step.id == StepId::TerminalServer);
        return Err(if terminal_version_failure {
            PinnedSshOpenError::LegacyProtocolCandidate(problem)
        } else {
            PinnedSshOpenError::Other(problem)
        });
    }
    let version = report.version.ok_or_else(|| {
        PinnedSshOpenError::LegacyProtocolCandidate("no terminal server version".into())
    })?;
    progress.enter("verifying the terminal server identity");
    let ssh = reach.ssh(Mux::Fresh);
    let protocol = ssh
        .output_within(
            &install::Step::new(format!(
                "cat \"$HOME/{}/{}/{}\" 2>/dev/null || true",
                install::HOST_DIR,
                version,
                install::PROTOCOL_MAJOR_FILE
            )),
            check::GREETING_TIMEOUT,
        )
        .map_err(PinnedSshOpenError::Other)?;
    if String::from_utf8_lossy(&protocol.stdout).trim() == "1" {
        return Err(PinnedSshOpenError::LegacyProtocolCandidate(
            "installed terminal protocol 1 requires control-plane verification".into(),
        ));
    }
    let server_id = check::read_server_id(&ssh, &version, check::GREETING_TIMEOUT)?;
    identity::verify_server(env, machine_id, binding, &server_id)?;
    progress.enter("starting the terminal server relay");
    let (reader, writer, child_guard) = spawn_pinned(ssh, &version, reach.askpass)?;
    Ok(publish_pinned_transport(
        env,
        machine_id,
        expected_server_id.to_string(),
        snapshot.generation,
        reader,
        writer,
        child_guard,
    )?)
}

// ---------------------------------------------------------------------------
// The operations (Tauri-free, so tests drive them directly)
// ---------------------------------------------------------------------------

fn declaration_dto(decl: &Declaration) -> MachineDeclarationDto {
    MachineDeclarationDto {
        mode: MachineMode::Legacy,
        host: decl.host.clone(),
        port: decl.port,
        jump_host: decl.jump_host.clone(),
        default_base_dir: decl.default_base_dir.clone(),
        pinned_host_key_fp: decl
            .pinned_host_key
            .as_deref()
            .and_then(|k| HostKey::parse(k).ok())
            .map(|k| k.fingerprint()),
        pinned_server_id: decl.pinned_server_id.clone(),
        identity_change: None,
    }
}

fn binding_dto(binding: &Binding) -> MachineBindingDto {
    MachineBindingDto {
        username: binding.username.clone(),
        key_path: binding.key_path.clone(),
        credential_ref: binding.credential_ref.clone(),
        auto_connect: binding.auto_connect,
        hidden: binding.hidden,
        reconnect_at_launch: false,
    }
}

fn summary(
    env: &Env,
    scope: Scope,
    decl: &Declaration,
    binding: Option<&Binding>,
) -> MachineSummary {
    let set_up = binding.is_some_and(|b| b.set_up) && decl.pinned_host_key.is_some();
    let mut declaration = declaration_dto(decl);
    declaration.identity_change = identity::change(env, &scope, decl, binding);
    let changed = declaration.identity_change.is_some();
    MachineSummary {
        machine_id: decl.machine_id.clone(),
        kind: "ssh",
        label: decl.label.clone(),
        can_publish: env.registry.can_publish(&scope),
        remote_mcp_control: false,
        scope: Some(scope),
        declaration: Some(declaration),
        binding: binding.map(binding_dto),
        status: if changed {
            MachineStatus::ConnectionFailed {
                last_error: identity::REFUSAL.into(),
            }
        } else if set_up {
            MachineStatus::Idle
        } else {
            MachineStatus::NeedsCredential
        },
        attention: 0,
        // A shared (team) declaration has no reverse MCP relay: only a private-account
        // machine with remote MCP control on carries agent callbacks (set by the
        // private summary).
        capabilities: MachineCapabilities {
            agent_callbacks: false,
        },
        machine_stats: None,
        machine_stats_supported: None,
        terminal_server: None,
    }
}

fn this_mac(workspaces: usize) -> MachineSummary {
    MachineSummary {
        machine_id: LOCAL_MACHINE_ID.into(),
        kind: "local",
        label: "This Mac".into(),
        scope: None,
        declaration: None,
        binding: None,
        can_publish: false,
        remote_mcp_control: false,
        status: MachineStatus::Connected {
            workspaces: Some(workspaces),
            terminal_server_update: false,
            terminal_server_sessions: None,
        },
        attention: 0,
        capabilities: MachineCapabilities {
            agent_callbacks: true,
        },
        machine_stats: None,
        machine_stats_supported: None,
        terminal_server: None,
    }
}

/// The layers a team's machine list reads: the team's, then the personal one.
fn list_scopes(env: &Env, org_slug: &str, team: Option<&str>) -> Result<Vec<Scope>, String> {
    let mut scopes = Vec::new();
    if let Some(team) = team {
        let scope = Scope::Team {
            org_slug: org_slug.to_string(),
            team: team.to_string(),
        };
        if env.registry.dir(&scope)? != env.registry.dir(&Scope::Personal)? {
            scopes.push(scope);
        }
    }
    scopes.push(Scope::Personal);
    Ok(scopes)
}

/// A saved machine file a team's list skips because it can't be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnreadableMachineFile {
    pub machine_id: String,
    pub scope: Scope,
    pub path: String,
    pub error: String,
}

/// Every machine file the team's list skips as unreadable, team layer first.
pub(crate) fn unreadable_machine_files(
    env: &Env,
    org_slug: &str,
    team: Option<&str>,
) -> Result<Vec<UnreadableMachineFile>, String> {
    let mut out = Vec::new();
    for scope in list_scopes(env, org_slug, team)? {
        for file in env.registry.unreadable_files(&scope)? {
            out.push(UnreadableMachineFile {
                machine_id: file.machine_id,
                scope: scope.clone(),
                path: file.path.to_string_lossy().into_owned(),
                error: file.error,
            });
        }
    }
    Ok(out)
}

/// `machine_id`'s unreadable file in the team's list, if it still is one.
fn unreadable_machine_file(
    env: &Env,
    org_slug: &str,
    team: Option<&str>,
    machine_id: &str,
) -> Result<UnreadableMachineFile, String> {
    unreadable_machine_files(env, org_slug, team)?
        .into_iter()
        .find(|file| file.machine_id == machine_id)
        .ok_or_else(|| format!("Machine {machine_id:?}'s file is no longer unreadable"))
}

/// The machines `org_slug`/`team` sees: This Mac, the team's, then the
/// user's personal ones.
pub(crate) fn machines_for(
    env: &Env,
    org_slug: &str,
    team: Option<&str>,
    local_workspaces: usize,
) -> Result<Vec<MachineSummary>, String> {
    // Unreadable bindings hide nobody's machines: they list as not set up,
    // and a later write reports the problem instead of overwriting it.
    let bindings = env.registry.bindings().unwrap_or_else(|e| {
        log::warn!(target: "machines", "{e}");
        Default::default()
    });
    let mut out = vec![this_mac(local_workspaces)];
    for scope in list_scopes(env, org_slug, team)? {
        for decl in env.registry.declarations(&scope)? {
            if private_setup::claims(env, &decl.machine_id)? {
                continue;
            }
            let binding = bindings.get(&decl.machine_id);
            out.push(summary(env, scope.clone(), &decl, binding));
        }
    }
    out.extend(private_setup::summaries(env)?);
    Ok(out)
}

fn summary_of(env: &Env, machine_id: &str) -> Result<MachineSummary, String> {
    if private_setup::claims(env, machine_id)? {
        return private_setup::summaries(env)?
            .into_iter()
            .find(|s| s.machine_id == machine_id)
            .ok_or("private machine unavailable".into());
    }
    let (scope, decl) = existing(env, Some(machine_id))?.ok_or("no machine")?;
    let bindings = env.registry.bindings()?;
    Ok(summary(env, scope, &decl, bindings.get(machine_id)))
}

fn step_id(id: StepId) -> &'static str {
    match id {
        StepId::Reachable => "reachable",
        StepId::HostKey => "hostKey",
        StepId::Auth => "auth",
        StepId::Platform => "platform",
        StepId::DataDir => "dataDir",
        StepId::TerminalServer => "terminalServer",
        StepId::AgentPrerequisite => "agentPrerequisite",
    }
}

fn step_status(status: Status) -> &'static str {
    match status {
        Status::Ok => "ok",
        Status::NeedsTrust => "needsTrust",
        Status::Warn => "warn",
        Status::Fail => "fail",
        Status::Blocked => "blocked",
        Status::Skipped => "skipped",
    }
}

fn next_step(next: NextStep) -> &'static str {
    match next {
        NextStep::EditAddress => "editAddress",
        NextStep::UseJumpHost => "useJumpHost",
        NextStep::Retry => "retry",
        NextStep::ChooseKey => "chooseKey",
        NextStep::ChangeUser => "changeUser",
        NextStep::CopyPublicKey => "copyPublicKey",
    }
}

fn report_dto(report: &check::Report) -> MachineTestReportDto {
    MachineTestReportDto {
        steps: report
            .steps
            .iter()
            .map(|s| MachineTestStepDto {
                id: step_id(s.id),
                status: step_status(s.status),
                detail: s.detail.clone(),
                next_steps: s.next_steps.iter().copied().map(next_step).collect(),
                fingerprint: s.fingerprint.clone(),
            })
            .collect(),
        install_plan: report.install.as_ref().map(|offer| InstallPlanDto {
            size_bytes: offer.size_bytes,
            target_dir: offer.target_dir.clone(),
            commands: offer.commands.clone(),
        }),
        file_view_only: report.file_view_only,
    }
}

/// Run the Add-machine checklist for `input` (a saved machine's pin applies).
pub(crate) fn test_connection(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: Option<&str>,
    input: &MachineDraftInput,
) -> Result<MachineTestReportDto, String> {
    if let Some(id) = machine_id {
        identity::validate_saved(env, id, None)?;
    }
    if effective_mode(env, machine_id, input)? == MachineMode::PrivateAccount {
        return private_setup::test(env, store, machine_id, input);
    }
    let existing = existing(env, machine_id)?;
    let pinned = pinned_key(existing.as_ref().map(|(_, d)| d), input)?;
    let pins = env.throwaway_pins()?;
    let mut reach = reach(env, store, machine_id.unwrap_or(DRAFT_ID), input)?;
    reach.connector = reach.connector.pinning_in(pins.path.clone());
    let report = check::run(
        &reach.connector,
        trust(pinned.as_ref(), input),
        reach.askpass.as_ref(),
        env.bundled(),
        &env.scratch(),
    );
    Ok(report_dto(&report))
}

/// Put this app's terminal server on the machine `input` reaches.
pub(crate) fn install_server(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: Option<&str>,
    input: &MachineDraftInput,
) -> Result<InstallTerminalServerResult, String> {
    if let Some(id) = machine_id {
        identity::validate_saved(env, id, None)?;
    }
    if effective_mode(env, machine_id, input)? == MachineMode::PrivateAccount {
        return private_setup::install(env, store, machine_id, input);
    }
    let existing = existing(env, machine_id)?;
    let pinned = pinned_key(existing.as_ref().map(|(_, d)| d), input)?;
    let pins = env.throwaway_pins()?;
    let mut reach = reach(env, store, machine_id.unwrap_or(DRAFT_ID), input)?;
    reach.connector = reach.connector.pinning_in(pins.path.clone());
    check::verified_host_key(
        &reach.connector,
        trust(pinned.as_ref(), input),
        &env.scratch(),
    )?;
    let ssh = reach.ssh(Mux::Fresh);
    Ok(
        match install::install(&ssh, &env.artifacts, &env.shell_init) {
            Ok(installed) => {
                let attach = install::attach_script(&installed.version)?;
                let remedies = installed.survival.remedies(&attach);
                let logout_warning = match &installed.survival {
                    codesense_session_server::survival::Survival::Survives => None,
                    _ if remedies.is_empty() => Some(
                        "This machine may end the terminal server when you log out, and only \
                         its administrator can change that (logind KillExcludeUsers)."
                            .to_string(),
                    ),
                    _ => Some(format!(
                        "This machine may end the terminal server when you log out. On the \
                         machine, run: {}",
                        remedies.join(" && ")
                    )),
                };
                InstallTerminalServerResult::Installed {
                    ok: true,
                    version: installed.version,
                    logout_warning,
                }
            }
            // An upload cut off is resumed by running the install again;
            // anything else needs its cause fixed first.
            Err(message) => InstallTerminalServerResult::Failed {
                ok: false,
                code: install_failure_code(&message),
                message,
            },
        },
    )
}

/// Whether a failed install is worth resuming: the connection broke (ssh
/// exits 255, or it was killed on a timeout) — but not when ssh refused the
/// sign-in or the machine, which also exit 255 and need fixing first.
fn install_failure_code(message: &str) -> &'static str {
    if message.contains("Host key verification failed") {
        return "failed";
    }
    let broke = message.contains("exit status: 255")
        || message.contains("signal:")
        || message.contains("did not finish within");
    match ssh::classify(message) {
        ssh::Failure::AuthFailed(_)
        | ssh::Failure::HostKeyChanged
        | ssh::Failure::Unresolved(_) => "failed",
        _ if broke => "interrupted",
        _ => "failed",
    }
}

/// A save is a transaction across a declaration, a personal binding, and a
/// host-key pin. Hold one per-machine lock from the first snapshot through
/// commit or rollback so two saves cannot base rollback on stale state while
/// unrelated machines remain independent.
fn save_transaction(machine_id: &str) -> std::sync::Arc<std::sync::Mutex<()>> {
    static TRANSACTIONS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::sync::Weak<std::sync::Mutex<()>>>>,
    > = std::sync::OnceLock::new();
    let mut transactions = TRANSACTIONS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    transactions.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = transactions
        .get(machine_id)
        .and_then(std::sync::Weak::upgrade)
    {
        lock
    } else {
        let lock = std::sync::Arc::new(std::sync::Mutex::new(()));
        transactions.insert(machine_id.to_string(), std::sync::Arc::downgrade(&lock));
        lock
    }
}

#[cfg(test)]
thread_local! {
    static AFTER_SAVE_IDENTITY_VALIDATION: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn after_save_identity_validation() {
    let hook = AFTER_SAVE_IDENTITY_VALIDATION.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

fn failed_with_rollback(cause: String, rollback: Result<(), String>) -> String {
    match rollback {
        Ok(()) => cause,
        Err(rollback) => format!("{cause}; rollback also failed: {rollback}"),
    }
}

/// Add or change a machine. With `connect`, the checklist must pass and the
/// terminal server's identity must match this person's pin; only then is
/// anything written. Without, it is kept as a personal "Needs setup" draft.
pub(crate) fn save(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: Option<&str>,
    input: &MachineDraftInput,
    share: &Scope,
    connect: bool,
) -> Result<MachineSummary, String> {
    save_as(
        env,
        store,
        machine_id,
        input,
        share,
        connect,
        crate::panes::commands::Actor::Human,
    )
}

fn save_as(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: Option<&str>,
    input: &MachineDraftInput,
    share: &Scope,
    connect: bool,
    actor: crate::panes::commands::Actor,
) -> Result<MachineSummary, String> {
    let id = match machine_id {
        Some(id) => id.to_string(),
        None => registry::new_machine_id(if input.label.trim().is_empty() {
            &input.host
        } else {
            &input.label
        }),
    };
    let transaction = save_transaction(&id);
    let _transaction = transaction.lock().unwrap_or_else(|e| e.into_inner());
    // The private registry owns its declaration/binding snapshot and guard.
    // A legacy save captures its declaration and binding once below: a git
    // update cannot change the identity the writer admitted midway through.
    if private_setup::claims(env, &id)? {
        if input.mode == Some(MachineMode::Legacy) {
            return Err("private machine requires private setup".into());
        }
        identity::validate_saved(
            env,
            &id,
            (actor == crate::panes::commands::Actor::Agent).then_some(input),
        )?;
        return private_setup::save_locked(env, store, &id, input, share, connect);
    }
    let existing = machine_id
        .map(|id| env.registry.find(id))
        .transpose()?
        .flatten();
    let bindings = env.registry.bindings()?;
    let old_binding = bindings.get(&id).cloned().unwrap_or_default();
    if let Some((_, declaration)) = &existing {
        identity::validate_snapshot(
            declaration,
            Some(&old_binding),
            (actor == crate::panes::commands::Actor::Agent).then_some(input),
        )?;
    }
    #[cfg(test)]
    after_save_identity_validation();
    let mode = input.mode.unwrap_or(if existing.is_some() {
        MachineMode::Legacy
    } else {
        MachineMode::PrivateAccount
    });
    if mode == MachineMode::PrivateAccount {
        return private_setup::save_locked(env, store, &id, input, share, connect);
    }
    if machine_id.is_some() && existing.is_none() {
        return Err(format!("No machine {id:?}"));
    }
    // Only whoever can publish to a declaration's layer changes it (and its
    // scope); anyone else binds their own sign-in to it.
    let publisher = existing
        .as_ref()
        .is_none_or(|(scope, _)| env.registry.can_publish(scope));
    let target = match &existing {
        Some((scope, _)) if !publisher => scope.clone(),
        _ => share.clone(),
    };
    if !env.registry.can_publish(&target) && publisher {
        return Err("You can't publish to that team's context from this Mac".into());
    }
    // What was set up stays set up only while nothing about reaching the
    // machine changed: its address, its jump host, and how this person signs in.
    let new_user = input
        .username
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty());
    let new_jump = input
        .jump_host
        .as_deref()
        .map(str::trim)
        .filter(|j| !j.is_empty());
    let same_address = existing.as_ref().is_some_and(|(_, d)| {
        d.host == input.host.trim() && d.port == input.port && d.jump_host.as_deref() == new_jump
    });
    let same_reach = same_address
        && old_binding.username.as_deref() == new_user
        && old_binding.key_path.as_deref() == input.sign_in.key_path()
        && old_binding.credential_ref.as_ref() == input.sign_in.credential_ref();
    let mut expected_server_id = existing
        .as_ref()
        .and_then(|(_, decl)| decl.pinned_server_id.clone())
        .filter(|_| same_address);
    let mut observed_server_id = old_binding
        .observed_server_id
        .clone()
        .filter(|_| same_reach);
    // A kept passphrase must still be there to bind. Its ledger entry is
    // refreshed, so no sweep takes it before the binding does; a save that
    // fails leaves it there, to be swept when it goes unbound.
    if let Some(handle) = input.sign_in.credential_ref() {
        env.registry.update_issued_passphrases(|issued| {
            if let Some(at) = issued.get_mut(&handle.account) {
                *at = now_secs();
            }
        })?;
        passphrase::load(store, handle)?;
    }

    let mut host_key = pinned_key(existing.as_ref().map(|(_, d)| d), input)?;
    let mut verified = None;
    if connect {
        let pins = env.throwaway_pins()?;
        let mut reach = reach(env, store, &id, input)?;
        reach.connector = reach.connector.pinning_in(pins.path.clone());
        let report = check::run(
            &reach.connector,
            trust(host_key.as_ref(), input),
            reach.askpass.as_ref(),
            env.bundled(),
            &env.scratch(),
        );
        if !report.passed() {
            let problem = report
                .first_problem()
                .map(|s| s.detail.clone())
                .unwrap_or_default();
            return Err(format!("The connection check didn't pass: {problem}"));
        }
        let version = report.version.clone().ok_or("no terminal server version")?;
        let server_id =
            check::read_server_id(&reach.ssh(Mux::Fresh), &version, check::GREETING_TIMEOUT)?;
        if !publisher && expected_server_id.is_none() {
            return Err(
                "The shared machine has no publisher-verified terminal-server identity".into(),
            );
        }
        if same_address
            && old_binding
                .accepted_identity
                .as_ref()
                .and_then(|identity| identity.server_id.as_ref())
                .is_some()
        {
            identity::verify_server(env, &id, &old_binding, &server_id)?;
        } else {
            check::check_server_id(expected_server_id.as_deref(), &server_id)?;
        }
        if publisher {
            expected_server_id = Some(server_id.clone());
        }
        observed_server_id = Some(server_id);
        host_key = report.host_key;
        verified = host_key.clone();
    }

    let decl = match (&existing, publisher) {
        (Some((_, decl)), false) => decl.clone(),
        _ => Declaration {
            machine_id: id.clone(),
            label: match input.label.trim() {
                "" => input.host.trim().to_string(),
                label => label.to_string(),
            },
            host: input.host.trim().to_string(),
            port: input.port,
            jump_host: input
                .jump_host
                .as_deref()
                .map(str::trim)
                .filter(|j| !j.is_empty())
                .map(String::from),
            default_base_dir: existing
                .as_ref()
                .and_then(|(_, d)| d.default_base_dir.clone()),
            pinned_host_key: host_key.as_ref().map(HostKey::to_line),
            pinned_server_id: expected_server_id,
        },
    };
    // The declaration is checked the way a teammate's app reads it.
    Address::new(&decl.host, decl.port, None, decl.jump_host.as_deref())?;
    // Setting a machine up changes nothing a teammate sees unless the
    // declaration itself changed.
    let unchanged = existing
        .as_ref()
        .is_some_and(|(scope, old)| *scope == target && *old == decl);
    let binding = Binding {
        username: input
            .username
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(String::from),
        key_path: input.sign_in.key_path().map(String::from),
        credential_ref: input.sign_in.credential_ref().cloned(),
        // Enrollment and an explicit Connect are not a background-reconnect
        // preference. Only the personal setting command may change this flag.
        auto_connect: old_binding.auto_connect,
        hidden: old_binding.hidden,
        set_up: connect || (old_binding.set_up && same_reach),
        accepted_identity: if actor == crate::panes::commands::Actor::Agent
            && old_binding.accepted_identity.is_some()
        {
            // An agent can use an acceptance, never replace it. First setup
            // below still derives its pin from the admitted snapshot/check.
            old_binding.accepted_identity.clone()
        } else if connect || (publisher && old_binding.accepted_identity.is_some()) {
            Some(registry::AcceptedIdentity::from(&decl))
        } else {
            old_binding.accepted_identity.clone()
        },
        observed_server_id,
    };
    let replaced = old_binding
        .credential_ref
        .clone()
        .filter(|old| Some(old) != binding.credential_ref.as_ref());
    let had_binding = bindings.contains_key(&id);
    let pin_path = env.paths().pin_file(&id);
    let old_pin = match std::fs::read(&pin_path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("read existing host-key pin {pin_path:?}: {e}")),
    };
    env.registry
        .update_bindings(|all| all.insert(id.clone(), binding))?;
    let rollback_personal = || -> Result<(), String> {
        let binding = env.registry.update_bindings(|all| {
            if had_binding {
                all.insert(id.clone(), old_binding.clone());
            } else {
                all.remove(&id);
            }
        });
        let pin = match &old_pin {
            Some(bytes) => std::fs::write(&pin_path, bytes)
                .map_err(|e| format!("restore host-key pin {pin_path:?}: {e}")),
            None => match std::fs::remove_file(&pin_path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(format!("remove new host-key pin {pin_path:?}: {e}")),
            },
        };
        match (binding, pin) {
            (Ok(_), Ok(())) => Ok(()),
            (Err(binding), Ok(())) => Err(binding),
            (Ok(_), Err(pin)) => Err(pin),
            (Err(binding), Err(pin)) => Err(format!("{binding}; {pin}")),
        }
    };
    if let Some(key) = &verified {
        if let Err(e) = env.paths().pin(&id, key) {
            return Err(failed_with_rollback(e, rollback_personal()));
        }
    }
    if publisher && !unchanged {
        if let Err(e) = env.registry.write(&target, &decl) {
            return Err(failed_with_rollback(e, rollback_personal()));
        }
        if let Some((old_scope, _)) = &existing {
            if *old_scope != target {
                if let Err(e) = env.registry.delete(old_scope, &id) {
                    // Never leave the machine in two layers.
                    let declaration = env.registry.delete(&target, &id);
                    let personal = rollback_personal();
                    let rollback = match (declaration, personal) {
                        (Ok(()), Ok(())) => Ok(()),
                        (Err(a), Ok(())) | (Ok(()), Err(a)) => Err(a),
                        (Err(a), Err(b)) => Err(format!("{a}; {b}")),
                    };
                    return Err(failed_with_rollback(e, rollback));
                }
            }
        }
    }
    if let Some(old) = replaced {
        if let Err(e) = passphrase::forget(store, &old) {
            log::warn!(target: "machines", "forget replaced passphrase: {e}");
        }
    }
    sweep_passphrases(env, store, None);
    // Display may observe a newer git declaration and show its identity
    // change. This projection is never used to choose what gets accepted.
    summary_of(env, &id)
}

pub(crate) fn set_hidden(env: &Env, machine_id: &str, hidden: bool) -> Result<(), String> {
    if private_setup::claims(env, machine_id)? {
        return private_setup::preferences(env, machine_id, Some(hidden), None);
    }
    let transaction = save_transaction(machine_id);
    let _transaction = transaction.lock().unwrap_or_else(|e| e.into_inner());
    existing(env, Some(machine_id))?;
    env.registry.update_bindings(|all| {
        all.entry(machine_id.to_string()).or_default().hidden = hidden;
    })
}

/// Set the personal, explicit background-reconnect preference. Enrollment,
/// connection state, and application shutdown never infer this value.
pub(crate) fn set_auto_connect(
    env: &Env,
    machine_id: &str,
    auto_connect: bool,
) -> Result<(), String> {
    if machine_id == LOCAL_MACHINE_ID {
        return Err("This Mac has no remote-machine auto-connect binding".into());
    }
    if private_setup::claims(env, machine_id)? {
        return private_setup::preferences(env, machine_id, None, Some(auto_connect));
    }
    let transaction = save_transaction(machine_id);
    let _transaction = transaction.lock().unwrap_or_else(|e| e.into_inner());
    existing(env, Some(machine_id))?.ok_or("no machine")?;
    let binding = env
        .registry
        .bindings()?
        .remove(machine_id)
        .ok_or("Set up this machine before changing auto-connect")?;
    if !binding.set_up {
        return Err("Set up this machine before changing auto-connect".into());
    }
    env.registry
        .update_bindings(|all| {
            all.get_mut(machine_id).map(|binding| {
                binding.auto_connect = auto_connect;
            })
        })?
        .ok_or_else(|| "The machine binding changed before auto-connect could be saved".into())
}

/// Move a team's machine back to its publisher's personal layer.
pub(crate) fn unshare(env: &Env, machine_id: &str) -> Result<MachineSummary, String> {
    if private_setup::claims(env, machine_id)? {
        return private_setup::unshare(env, machine_id);
    }
    let transaction = save_transaction(machine_id);
    let _transaction = transaction.lock().unwrap_or_else(|e| e.into_inner());
    let (scope, decl) = existing(env, Some(machine_id))?.ok_or("no machine")?;
    if !matches!(scope, Scope::Team { .. }) {
        return Err(format!("{} isn't shared", decl.label));
    }
    if !env.registry.can_publish(&scope) {
        return Err(format!("You can't publish to {}'s team", decl.label));
    }
    if env
        .registry
        .declarations(&Scope::Personal)?
        .iter()
        .any(|personal| personal.machine_id == machine_id)
    {
        return Err(format!(
            "A personal machine already uses id {machine_id}; resolve it before unsharing"
        ));
    }
    env.registry.write(&Scope::Personal, &decl)?;
    if let Err(e) = env.registry.delete(&scope, machine_id) {
        let _ = env.registry.delete(&Scope::Personal, machine_id);
        return Err(e);
    }
    summary_of(env, machine_id)
}

/// Delete a machine: its declaration (for everyone, when it was shared), and
/// this person's binding, keychain entry and host-key pin.
pub(crate) fn remove(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: &str,
) -> Result<(), String> {
    if private_setup::claims(env, machine_id)? {
        return private_setup::remove(env, store, machine_id);
    }
    let transaction = save_transaction(machine_id);
    let _transaction = transaction.lock().unwrap_or_else(|e| e.into_inner());
    let (scope, decl) = existing(env, Some(machine_id))?.ok_or("no machine")?;
    if !env.registry.can_publish(&scope) {
        return Err(format!("You can't remove {} from its team", decl.label));
    }
    // This person's own traces go first, best effort, so a failure leaves
    // the machine listed and the remove can simply be run again.
    let bound = env.registry.bindings()?.remove(machine_id);
    if let Some(handle) = bound.as_ref().and_then(|b| b.credential_ref.as_ref()) {
        passphrase::forget(store, handle)?;
    }
    let pin = env.paths().pin_file(machine_id);
    if let Err(e) = std::fs::remove_file(&pin) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(format!("remove {pin:?}: {e}"));
        }
    }
    env.registry.update_bindings(|all| all.remove(machine_id))?;
    env.registry.delete(&scope, machine_id)
}

/// Keep `key`'s passphrase, asked for by `prompt`, in the keychain once it
/// is shown to unlock the key. `None` when the person cancelled.
pub(crate) fn capture_passphrase(
    env: &Env,
    store: &dyn CredentialStore,
    key_path: &str,
    prompt: impl FnOnce(&Path) -> Result<Option<Secret>, String>,
    unlocks: impl FnOnce(&Path, &Secret) -> Result<(), String>,
) -> Result<Option<CredentialRef>, String> {
    env.prepare_dirs()?;
    let key = env.expand(key_path.trim());
    let Some(secret) = prompt(&key)? else {
        return Ok(None);
    };
    unlocks(&key, &secret)?;
    let handle = passphrase::store(store, &secret)?;
    let issued = env.registry.update_issued_passphrases(|issued| {
        issued.insert(handle.account.clone(), now_secs());
    });
    if let Err(e) = issued {
        // Unrecorded, it could never be found again: don't keep it.
        let _ = passphrase::forget(store, &handle);
        return Err(e);
    }
    sweep_passphrases(env, store, Some(&handle.account));
    Ok(Some(handle))
}

/// How long a kept passphrase may wait to be bound to a machine before it
/// is forgotten: the Add-machine panel dropped it (another key, another
/// passphrase, closed or cancelled).
const UNBOUND_PASSPHRASE_SECS: u64 = 24 * 60 * 60;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Settle the ledger of issued passphrase handles: a bound one belongs to its
/// binding now; an unbound one past its day (never `keep`) is forgotten.
/// Best effort — a failure leaves it for the next sweep.
fn sweep_passphrases(env: &Env, store: &dyn CredentialStore, keep: Option<&str>) {
    let now = now_secs();
    // Decided and forgotten under the ledger's lock, against the bindings as
    // they are then: a save binding a handle meanwhile is never undercut.
    let swept = env.registry.update_issued_passphrases(|issued| {
        let Ok(bindings) = env.registry.bindings() else {
            return;
        };
        let Ok(private_bound) = private_setup::credential_accounts(env) else {
            return;
        };
        let bound: std::collections::BTreeSet<&str> = bindings
            .values()
            .filter_map(|b| b.credential_ref.as_ref())
            .map(|r| r.account.as_str())
            .collect();
        issued.retain(|account, at| {
            if bound.contains(account.as_str()) || private_bound.contains(account) {
                return false;
            }
            let stale =
                Some(account.as_str()) != keep && now.saturating_sub(*at) > UNBOUND_PASSPHRASE_SECS;
            let handle = CredentialRef {
                provider: passphrase::SSH_PROVIDER.into(),
                account: account.clone(),
            };
            match stale.then(|| passphrase::forget(store, &handle)) {
                Some(Err(e)) => {
                    log::warn!(target: "machines", "forget an unbound passphrase: {e}");
                    true
                }
                Some(Ok(())) => false,
                None => true,
            }
        });
    });
    if let Err(e) = swept {
        log::warn!(target: "machines", "sweep kept passphrases: {e}");
    }
}

fn local_sources(home: Option<&Path>) -> SshLocalSources {
    SshLocalSources {
        config_hosts: home
            .map(ssh::config_hosts)
            .unwrap_or_default()
            .into_iter()
            .map(|h| SshConfigHostDto {
                alias: h.alias,
                host_name: h.host_name,
                user: h.user,
                port: h.port,
                proxy_jump: h.proxy_jump,
                identity_file: h.identity_file,
                proxy_command: h.proxy_command,
            })
            .collect(),
        agent_keys: agent_key_dtos(home, ssh::agent_keys()),
    }
}

fn agent_key_dtos(home: Option<&Path>, keys: Vec<ssh::AgentKey>) -> Vec<SshAgentKeyDto> {
    keys.into_iter()
        .map(|k| SshAgentKeyDto {
            identity_file: home.and_then(|home| agent_identity_file(home, &k.public_key)),
            key_type: k.key_type,
            fingerprint: k.fingerprint,
            comment: k.comment,
            public_key: k.public_key,
        })
        .collect()
}

/// A public key file for the agent key `public_key`: the user's own
/// `~/.ssh/*.pub` holding it, else one CodeSense keeps for it (public keys
/// only, never a private one).
fn agent_identity_file(home: &Path, public_key: &str) -> Option<String> {
    let blob = |line: &str| {
        let mut words = line.split_whitespace();
        Some((words.next()?.to_string(), words.next()?.to_string()))
    };
    let wanted = blob(public_key)?;
    let ssh_dir = home.join(".ssh");
    if let Ok(entries) = std::fs::read_dir(&ssh_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "pub")
                && std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| blob(&text))
                    .as_ref()
                    == Some(&wanted)
            {
                return Some(path.to_string_lossy().into_owned());
            }
        }
    }
    use std::os::unix::fs::PermissionsExt;
    let dir = home.join(".codesense").join("agent-keys");
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).ok()?;
    let digest: String = Sha256::digest(wanted.1.as_bytes())
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = dir.join(format!("{digest}.pub"));
    if std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| blob(&text))
        != Some(wanted)
    {
        std::fs::write(&path, format!("{}\n", public_key.trim())).ok()?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok()?;
    }
    Some(path.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

async fn private_authority_for_app<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Option<crate::commands::auth::private_authority::PrivateAuthoritySnapshot> {
    app.try_state::<crate::commands::local_profile::LocalProfileState>()?
        .snapshot()
        .ok()
}

pub(crate) async fn env_of(app: &tauri::AppHandle) -> Result<Env, String> {
    let (codesense_home, user_id) = crate::commands::route_to_context::resolve_ambient(app).await?;
    let resource = |rel: &str| crate::commands::paths::resolve_bundled_resource(app, rel);
    let artifacts = resource("host-agent")
        .or_else(|| resource("binaries"))
        .ok_or("this build carries no terminal server for other machines")?;
    let shell_init =
        resource("resources/shell-integration").ok_or("this build carries no shell integration")?;
    let askpass_program =
        crate::commands::deps::resolve_sidecar_source("codesense-session-server")?;
    let private_authority = private_authority_for_app(app).await;
    Ok(Env {
        private_authority,
        registry: Registry::new(codesense_home.clone(), Principal::from(user_id)),
        codesense_home,
        home: dirs::home_dir(),
        artifacts,
        shell_init,
        askpass_program,
        ssh_program: PathBuf::from("ssh"),
    })
}

fn keychain() -> Box<dyn CredentialStore> {
    default_store(passphrase::SSH_SERVICE)
}

fn manager_record(
    env: &Env,
    machine_id: &str,
) -> Result<Option<super::host_connection_manager::MachineManagerRecord>, String> {
    if private_setup::claims(env, machine_id)? {
        return private_setup::record(env, machine_id);
    }
    let Some((scope, declaration)) = existing(env, Some(machine_id))? else {
        return Ok(None);
    };
    let binding = env.registry.bindings()?.remove(machine_id);
    let encoded = serde_json::to_vec(&(scope.clone(), declaration.clone(), binding.clone()))
        .map_err(|error| error.to_string())?;
    let fingerprint = Sha256::digest(encoded)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let revision = env.registry.manager_revision(Some(&scope), machine_id)?;
    Ok(Some(super::host_connection_manager::MachineManagerRecord {
        machine_id: machine_id.to_string(),
        scope: Some(scope),
        revision: Some(revision),
        fingerprint,
        set_up: binding.as_ref().is_some_and(|binding| {
            binding.set_up && binding.validate_declaration(&declaration).is_ok()
        }),
        auto_connect: binding.as_ref().is_some_and(|binding| binding.auto_connect),
        open_tabs: false,
    }))
}

/// Every saved machine's manager record, plus the ids whose declaration file
/// exists but can't be read: those are skipped, never treated as removed.
fn manager_records(
    env: &Env,
) -> Result<
    (
        Vec<super::host_connection_manager::MachineManagerRecord>,
        std::collections::BTreeSet<String>,
    ),
    String,
> {
    let bindings = env.registry.bindings()?;
    let scan = env.registry.all_declarations()?;
    let unreadable = scan.unreadable.into_keys().collect();
    let declarations = scan
        .found
        .into_iter()
        .filter_map(
            |item| match private_setup::claims(env, &item.1.machine_id) {
                Ok(true) => None,
                Ok(false) => Some(Ok(item)),
                Err(error) => Some(Err(error)),
            },
        )
        .collect::<Result<Vec<_>, String>>()?;
    let mut records: Vec<_> = declarations
        .into_iter()
        .map(|(scope, declaration)| {
            let binding = bindings.get(&declaration.machine_id).cloned();
            let encoded =
                serde_json::to_vec(&(scope.clone(), declaration.clone(), binding.clone()))
                    .map_err(|error| error.to_string())?;
            let fingerprint = Sha256::digest(encoded)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            let revision = env
                .registry
                .manager_revision(Some(&scope), &declaration.machine_id)?;
            Ok(super::host_connection_manager::MachineManagerRecord {
                machine_id: declaration.machine_id.clone(),
                scope: Some(scope),
                revision: Some(revision),
                fingerprint,
                set_up: binding.as_ref().is_some_and(|binding| {
                    binding.set_up && binding.validate_declaration(&declaration).is_ok()
                }),
                auto_connect: binding.as_ref().is_some_and(|binding| binding.auto_connect),
                open_tabs: false,
            })
        })
        .collect::<Result<_, String>>()?;
    for summary in private_setup::summaries(env)? {
        if let Some(record) = private_setup::record(env, &summary.machine_id)? {
            records.push(record);
        }
    }
    Ok((records, unreadable))
}

/// How long a connected machine stays up while its saved file can't be
/// read. The file may be mid-write (a pull, an editor); past this it is
/// treated as gone for connecting, though the machine stays listed.
const UNREADABLE_GRACE: Duration = Duration::from_secs(30);

/// Since when each machine's file has been unreadable, as last observed, and
/// which of those machines a grace period disconnected.
#[derive(Default)]
pub(crate) struct UnreadableGrace(std::sync::Mutex<GraceState>);

#[derive(Default)]
pub(crate) struct GraceState {
    since: std::collections::HashMap<String, std::time::Instant>,
    disconnected: std::collections::BTreeSet<String>,
}

impl GraceState {
    /// Forget the machines whose files read again; returns those of them a
    /// grace period had disconnected.
    fn forget_readable(&mut self, unreadable: &std::collections::BTreeSet<String>) -> Vec<String> {
        self.since.retain(|id, _| unreadable.contains(id));
        let repaired: Vec<String> = self
            .disconnected
            .iter()
            .filter(|id| !unreadable.contains(*id))
            .cloned()
            .collect();
        for id in &repaired {
            self.disconnected.remove(id);
        }
        repaired
    }
}

/// What one observation of the unreadable files changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct GraceNote {
    /// Files that just became unreadable: their grace periods start.
    fresh: Vec<String>,
    /// Machines a grace period disconnected whose files read again.
    repaired: Vec<String>,
}

impl UnreadableGrace {
    fn state(&self) -> std::sync::MutexGuard<'_, GraceState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Record the machines whose files are unreadable now.
    fn note(
        &self,
        unreadable: &std::collections::BTreeSet<String>,
        now: std::time::Instant,
    ) -> GraceNote {
        let mut state = self.state();
        let repaired = state.forget_readable(unreadable);
        let mut fresh = Vec::new();
        for id in unreadable {
            if !state.since.contains_key(id) {
                state.since.insert(id.clone(), now);
                fresh.push(id.clone());
            }
        }
        GraceNote { fresh, repaired }
    }

    /// Forget the machines whose files read again, adding none; returns the
    /// repaired ones a grace period had disconnected.
    fn refresh(&self, unreadable: &std::collections::BTreeSet<String>) -> Vec<String> {
        self.state().forget_readable(unreadable)
    }

    /// Whether `machine_id`'s file has stayed unreadable for `grace`.
    fn expired(&self, machine_id: &str, now: std::time::Instant, grace: Duration) -> bool {
        self.state()
            .since
            .get(machine_id)
            .is_some_and(|since| now.duration_since(*since) >= grace)
    }

    /// `machine_id` was disconnected for its unreadable file.
    fn mark_disconnected(&self, machine_id: &str) {
        self.state().disconnected.insert(machine_id.to_owned());
    }
}

fn unreadable_grace(app: &tauri::AppHandle) -> tauri::State<'_, UnreadableGrace> {
    if app.try_state::<UnreadableGrace>().is_none() {
        app.manage(UnreadableGrace::default());
    }
    app.state::<UnreadableGrace>()
}

/// A machine a grace period disconnected whose file reads again may
/// reconnect in the background as before: the disconnect was not the
/// person's. Even a byte-identical repair counts.
fn resume_repaired(
    manager: &super::host_connection_manager::HostConnectionManager,
    repaired: &[String],
) {
    for machine_id in repaired {
        manager.resume_auto_connect(machine_id);
    }
}

/// Start the grace period of every machine whose file just became
/// unreadable: once it ends with the file still unreadable, the machine is
/// disconnected. Nothing is removed; the file may still be repaired. Returns
/// the machines a grace period disconnected whose files read again.
fn schedule_unreadable_grace(
    app: &tauri::AppHandle,
    unreadable: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let note = unreadable_grace(app).note(unreadable, std::time::Instant::now());
    for machine_id in note.fresh {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(UNREADABLE_GRACE).await;
            let Ok(env) = env_of(&app).await else {
                return;
            };
            let Ok((_, unreadable)) =
                crate::commands::await_blocking("machine_manager_registry", move || {
                    manager_records(&env)
                })
                .await
            else {
                return;
            };
            let grace = unreadable_grace(&app);
            let manager = app.state::<super::host_connection_manager::HostConnectionManager>();
            resume_repaired(&manager, &UnreadableGrace::refresh(&grace, &unreadable));
            if !grace.expired(&machine_id, std::time::Instant::now(), UNREADABLE_GRACE) {
                return;
            }
            if manager.is_member(&machine_id) {
                log::warn!(target: "machines", "disconnecting {machine_id}: its saved file stayed unreadable");
                grace.mark_disconnected(&machine_id);
                if let Err(error) = manager.disconnect(&machine_id).await {
                    log::debug!(target: "machines", "disconnect {machine_id}: {error}");
                }
            }
        });
    }
    note.repaired
}

pub(crate) async fn observe_external_registry(
    app: tauri::AppHandle,
    seq: super::host_connection_manager::ObservationSeq,
    hinted_ids: std::collections::BTreeSet<String>,
    broad: bool,
    event_revisions: Vec<registry::FileRevision>,
) -> Result<(), String> {
    let env = env_of(&app).await?;
    let (records, unreadable) =
        crate::commands::await_blocking("machine_manager_registry", move || manager_records(&env))
            .await?;
    let manager = app.state::<super::host_connection_manager::HostConnectionManager>();
    manager
        .observe_external_keeping(
            seq,
            records,
            hinted_ids,
            broad,
            event_revisions,
            &unreadable,
        )
        .await;
    let repaired = schedule_unreadable_grace(&app, &unreadable);
    resume_repaired(&manager, &repaired);
    schedule_auto_connects(&app, manager.auto_connect_candidates());
    Ok(())
}

/// Background connects run at most this many at once, so a launch with many
/// machines to bring back doesn't open every SSH connection together.
const BACKGROUND_CONNECTS: usize = 3;

fn background_connects() -> &'static tokio::sync::Semaphore {
    static PERMITS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    PERMITS.get_or_init(|| tokio::sync::Semaphore::new(BACKGROUND_CONNECTS))
}

fn schedule_auto_connects(app: &tauri::AppHandle, machine_ids: Vec<String>) {
    for machine_id in machine_ids {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move {
            let Ok(_permit) = background_connects().acquire().await else {
                return;
            };
            let manager = handle.state::<super::host_connection_manager::HostConnectionManager>();
            if let Err(error) = manager
                .connect_remote(
                    handle.clone(),
                    machine_id.clone(),
                    super::host_connection_manager::ConnectOrigin::Auto,
                )
                .await
            {
                log::debug!(target: "machines", "auto-connect {machine_id}: {error}");
            }
        });
    }
}

/// Rebuild the connection manager's members from the registry and keep
/// watching it. Runs at startup and whenever a webview (re)registers: the
/// startup attempt can fail before the active profile is readable, and a
/// profile switch moves the registry root, so neither may be the last word.
pub(crate) async fn sync_connection_manager(app: &tauri::AppHandle) -> Result<(), String> {
    let env = env_of(app).await?;
    let manager = app.state::<super::host_connection_manager::HostConnectionManager>();
    manager.watch_registry(app.clone(), &env.registry.watch_root())?;
    let writer_app = app.clone();
    manager.set_open_tabs_writer(std::sync::Arc::new(move |machine_id, open_tabs| {
        let app = writer_app.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(error) = remember_open_tabs(&app, machine_id.clone(), open_tabs).await {
                log::debug!(target: "machines", "keep open tabs of {machine_id}: {error}");
            }
        });
    }));
    reconcile_manager(&manager, env).await?;
    schedule_auto_connects(app, manager.auto_connect_candidates());
    Ok(())
}

/// Keep whether `machine_id` holds open tabs, so a launch knows to bring it back.
async fn remember_open_tabs(
    app: &tauri::AppHandle,
    machine_id: String,
    open_tabs: bool,
) -> Result<(), String> {
    let env = env_of(app).await?;
    crate::commands::await_blocking("machine_open_tabs", move || {
        if private_setup::claims(&env, &machine_id)? {
            private_setup::set_open_tabs(&env, &machine_id, open_tabs)
        } else {
            Ok(())
        }
    })
    .await
}

/// One broad observation of every saved machine. Unchanged machines keep
/// their connections; only additions, edits and removals take effect. A
/// declaration file that can't be read is skipped and its machine left as
/// the manager last knew it.
pub(crate) async fn reconcile_manager(
    manager: &super::host_connection_manager::HostConnectionManager,
    env: Env,
) -> Result<(), String> {
    reconcile_manager_returning(manager, env).await.1
}

/// [`reconcile_manager`], handing `env` back for a follow-up read.
async fn reconcile_manager_returning(
    manager: &super::host_connection_manager::HostConnectionManager,
    env: Env,
) -> (Option<Env>, Result<(), String>) {
    let seq = manager.reserve_observation();
    let read = crate::commands::await_blocking("machine_manager_registry", move || {
        let records = manager_records(&env);
        Ok((env, records))
    })
    .await;
    let (env, records) = match read {
        Ok((env, records)) => (env, records),
        Err(error) => return (None, Err(error)),
    };
    match records {
        Ok((records, unreadable)) => {
            manager
                .observe_external_keeping(
                    seq,
                    records,
                    std::collections::BTreeSet::new(),
                    true,
                    Vec::new(),
                    &unreadable,
                )
                .await;
            (Some(env), Ok(()))
        }
        Err(error) => (Some(env), Err(error)),
    }
}

/// Make sure connect and retry can act on `machine_id`: when the manager
/// does not know it, re-read the registry once, and otherwise say why.
pub(crate) async fn ensure_manager_member(
    app: &tauri::AppHandle,
    machine_id: &str,
) -> Result<(), String> {
    let manager = app.state::<super::host_connection_manager::HostConnectionManager>();
    if manager.is_member(machine_id) {
        return Ok(());
    }
    let env = env_of(app).await?;
    if let Err(error) = manager.watch_registry(app.clone(), &env.registry.watch_root()) {
        log::warn!(target: "machines", "watch machine registry: {error}");
    }
    ensure_member(&manager, env, machine_id).await?;
    schedule_auto_connects(app, manager.auto_connect_candidates());
    Ok(())
}

/// The registry half of [`ensure_manager_member`]: a broad reconcile, then
/// (when that read fails) just `machine_id`'s own record, then the reason
/// it still can't be registered.
pub(crate) async fn ensure_member(
    manager: &super::host_connection_manager::HostConnectionManager,
    env: Env,
    machine_id: &str,
) -> Result<(), String> {
    if manager.is_member(machine_id) {
        return Ok(());
    }
    let (env, broad) = reconcile_manager_returning(manager, env).await;
    if manager.is_member(machine_id) {
        return Ok(());
    }
    let Some(env) = env else {
        return Err(format!(
            "Can't read the saved machines: {}",
            broad.err().unwrap_or_default()
        ));
    };
    let id = machine_id.to_owned();
    let env = if let Err(error) = broad {
        log::warn!(target: "machines", "read saved machines: {error}; reading {id:?} alone");
        let seq = manager.reserve_observation();
        let target = id.clone();
        let (env, record) =
            crate::commands::await_blocking("machine_manager_snapshot", move || {
                let record = manager_record(&env, &target);
                Ok((env, record))
            })
            .await?;
        if let Ok(Some(record)) = record {
            manager
                .observe_mutation(seq, id.clone(), Some(record))
                .await;
            if manager.is_member(&id) {
                return Ok(());
            }
        }
        env
    } else {
        env
    };
    Err(
        crate::commands::await_blocking("machine_member_reason", move || {
            Ok(missing_member_reason(&env, &id))
        })
        .await?,
    )
}

/// Why a machine the manager could not register can't connect, in words
/// that point at the fix.
pub(crate) fn missing_member_reason(env: &Env, machine_id: &str) -> String {
    if let Some(error) = env.registry.unreadable_declaration(machine_id) {
        return format!(
            "Machine {machine_id:?}'s saved record can't be read ({error}); fix or remove that file"
        );
    }
    let not_saved =
        || format!("Machine {machine_id:?} isn't saved for this profile; add it again in Machines");
    match manager_record(env, machine_id) {
        Ok(Some(_)) => format!("Machine {machine_id:?} is saved but not ready yet; try again"),
        Ok(None) => not_saved(),
        Err(_)
            if matches!(env.registry.find(machine_id), Ok(None))
                && matches!(private_setup::claims(env, machine_id), Ok(false)) =>
        {
            not_saved()
        }
        Err(error) => format!("Machine {machine_id:?}'s saved record can't be read: {error}"),
    }
}

/// The UI's one cue to re-read a machine list: the layer that changed
/// (`None` when unknown, which re-reads every list) and the machine.
pub(crate) const MACHINES_CHANGED_EVENT: &str = "machines-changed";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MachinesChanged {
    pub scope: Option<Scope>,
    pub machine_id: String,
}

/// Every machines-changed this app emitted, by layer and machine, in order:
/// a registry write skips the announce the connection manager already made.
#[derive(Default)]
pub(crate) struct MachinesChangedLedger {
    seq: std::sync::atomic::AtomicU64,
    last: std::sync::Mutex<std::collections::HashMap<(String, String), u64>>,
}

fn changed_key(scope: &Option<Scope>, machine_id: &str) -> (String, String) {
    (
        serde_json::to_string(scope).unwrap_or_default(),
        machine_id.to_owned(),
    )
}

fn changed_ledger<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> tauri::State<'_, MachinesChangedLedger> {
    if app.try_state::<MachinesChangedLedger>().is_none() {
        app.manage(MachinesChangedLedger::default());
    }
    app.state::<MachinesChangedLedger>()
}

/// A mark to pass to [`announce_registry_write`]: events emitted after it
/// count as already announced.
pub(crate) fn machines_changed_mark<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> u64 {
    changed_ledger(app).seq.load(Ordering::SeqCst)
}

/// Whether `scope`/`machine_id` was announced after `mark`.
fn announced_since<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    scope: &Option<Scope>,
    machine_id: &str,
    mark: u64,
) -> bool {
    changed_ledger(app)
        .last
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&changed_key(scope, machine_id))
        .is_some_and(|seq| *seq > mark)
}

pub(crate) fn emit_machines_changed<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    scope: Option<Scope>,
    machine_id: &str,
) {
    use tauri::Emitter;
    {
        let ledger = changed_ledger(app);
        let seq = ledger.seq.fetch_add(1, Ordering::SeqCst) + 1;
        ledger
            .last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(changed_key(&scope, machine_id), seq);
    }
    let payload = MachinesChanged {
        scope,
        machine_id: machine_id.to_owned(),
    };
    if let Err(error) = app.emit(MACHINES_CHANGED_EVENT, payload) {
        log::warn!(target: "machines", "emit machines changed: {error}");
    }
}

/// Every connection-manager event reaches the webview as its runtime
/// admission row (when that changed) and as a machine-list change.
pub(crate) fn forward_manager_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    event: &super::host_connection_manager::ManagerEvent,
) {
    use tauri::Emitter;
    if let Some(row) = &event.admission {
        if let Err(error) = app.emit("terminal-layout-admission-changed", row) {
            log::warn!(target: "machines", "emit remote runtime admission: {error}");
        }
    }
    emit_machines_changed(app, event.scope.clone(), &event.machine_id);
}

/// The layer `machine_id` is declared in right now, if it can be read.
fn current_scope(env: &Env, machine_id: &str) -> Option<Scope> {
    match private_setup::claims(env, machine_id) {
        Ok(true) => {
            private_setup::summaries(env)
                .ok()?
                .into_iter()
                .find(|summary| summary.machine_id == machine_id)?
                .scope
        }
        Ok(false) => existing(env, Some(machine_id))
            .ok()
            .flatten()
            .map(|(scope, _)| scope),
        Err(_) => None,
    }
}

/// SW-AGR-19: this app's CodeSense home, for the restore manifests.
pub(crate) async fn codesense_home_of(app: &tauri::AppHandle) -> Option<PathBuf> {
    env_of(app).await.ok().map(|env| env.codesense_home)
}

/// SW-AGR-19: a machine's label, for the person (its id when unreadable).
pub(crate) async fn label_of(app: &tauri::AppHandle, machine_id: &str) -> String {
    let Ok(env) = env_of(app).await else {
        return machine_id.to_string();
    };
    summary_of(&env, machine_id)
        .map(|summary| summary.label)
        .unwrap_or_else(|_| machine_id.to_string())
}

pub(crate) async fn manager_machine_summary(
    app: &tauri::AppHandle,
    machine_id: String,
) -> Result<MachineSummary, String> {
    let env = env_of(app).await?;
    let summary_machine_id = machine_id.clone();
    let mut summary = crate::commands::await_blocking("manager_machine_summary", move || {
        summary_of(&env, &summary_machine_id)
    })
    .await?;
    let manager_state = app.state::<super::host_connection_manager::HostConnectionManager>();
    let projection = manager_state.projection_snapshot([machine_id.clone()]);
    match projection.get(&machine_id) {
        Some(super::host_connection_manager::ManagerProjection::Present(state)) => {
            apply_manager_state(&mut summary, state.clone());
            annotate_terminal_servers(app, std::slice::from_mut(&mut summary), true).await;
        }
        Some(super::host_connection_manager::ManagerProjection::ConfirmedRemoved)
            if machine_id != LOCAL_MACHINE_ID =>
        {
            return Err("machine was removed while its summary was being read".into());
        }
        _ => {}
    }
    Ok(summary)
}

/// SW-XS-6: the terminal-server builds this app ships, one per Linux triple
/// (`{version}-{digest}`). Cached once found; an empty set (no artifacts
/// readable yet, or a development build) is asked again next time.
pub(super) fn shipped_terminal_builds(env: &Env) -> std::collections::HashSet<String> {
    static BUILDS: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
        std::sync::Mutex::new(None);
    let mut cached = BUILDS.lock().unwrap_or_else(|poison| poison.into_inner());
    if let Some(builds) = cached.as_ref() {
        return builds.clone();
    }
    let builds: std::collections::HashSet<String> = install::LINUX_TRIPLES
        .iter()
        .filter_map(|triple| install::Bundle::load(&env.artifacts, &env.shell_init, triple).ok())
        .map(|bundle| bundle.version)
        .collect();
    if !builds.is_empty() {
        *cached = Some(builds.clone());
    }
    builds
}

/// SW-AGR-19: each connected summary's running terminal server, from the
/// live client — on every list, connect and retry, so a reload never drops
/// it and an auto-connected machine gets it too. A remote server older than
/// this app is flagged for the update, with the sessions the update ends.
async fn annotate_terminal_servers(
    app: &tauri::AppHandle,
    summaries: &mut [MachineSummary],
    count_sessions: bool,
) {
    let connected: Vec<String> = summaries
        .iter()
        .filter(|summary| matches!(summary.status, MachineStatus::Connected { .. }))
        .map(|summary| summary.machine_id.clone())
        .collect();
    if connected.is_empty() {
        return;
    }
    let env = match env_of(app).await {
        Ok(env) => env,
        Err(error) => {
            log::warn!(target: "machines", "terminal server info unavailable: {error}");
            return;
        }
    };
    let local_client = app
        .try_state::<crate::terminal::session_server::SessionServerState>()
        .and_then(|state| state.connected());
    let remote_clients: Vec<(
        String,
        Arc<crate::terminal::session_server::SessionServerClient>,
    )> = connected
        .iter()
        .filter(|id| id.as_str() != LOCAL_MACHINE_ID)
        .filter_map(|id| {
            crate::commands::pty_backend::resolve_spawn_backend(app, id, "")
                .ok()
                .and_then(|backend| backend.client())
                .map(|client| (id.clone(), client))
        })
        .collect();
    let infos = crate::commands::await_blocking("terminal_server_info", move || {
        let mut infos = std::collections::HashMap::new();
        if let Some(client) = local_client {
            let shipped_exe = crate::commands::deps::resolve_sidecar_source(
                crate::terminal::session_server::SIDECAR_BIN,
            )
            .ok();
            infos.insert(
                LOCAL_MACHINE_ID.to_string(),
                (
                    terminal_server_info::local_info(
                        client.announced_version(),
                        client.server_pid(),
                        client.build_stamp(),
                        shipped_exe.as_deref(),
                    ),
                    None,
                ),
            );
        }
        let shipped = shipped_terminal_builds(&env);
        for (id, client) in remote_clients {
            let info = terminal_server_info::remote_info(
                &shipped,
                client.announced_version(),
                client.proto_minor(),
                client.build_stamp(),
            );
            // A round trip to the host, asked only for one machine's summary
            // (connect, retry) and only when the update would end them; a
            // list never waits on every stale host in turn.
            let sessions = (count_sessions
                && info.relation == terminal_server_info::ServerRelation::Older)
                .then(|| client.live_session_count())
                .flatten();
            infos.insert(id, (info, sessions));
        }
        Ok(infos)
    })
    .await
    .unwrap_or_else(|error| {
        log::warn!(target: "machines", "terminal server info unavailable: {error}");
        Default::default()
    });
    apply_terminal_servers(summaries, &infos);
    for summary in summaries {
        if matches!(summary.status, MachineStatus::Connected { .. }) {
            if let Some(client) = crate::live_hub::machine_stats::client(app, &summary.machine_id) {
                summary.machine_stats_supported = Some(client.supports_machine_stats());
                summary.machine_stats = client.cached_machine_stats();
            }
        }
    }
}

/// Each machine's server, as read, onto its summary (after the manager's
/// state, which knows nothing of the server's build).
fn apply_terminal_servers(
    summaries: &mut [MachineSummary],
    infos: &std::collections::HashMap<
        String,
        (terminal_server_info::TerminalServerInfo, Option<u32>),
    >,
) {
    for summary in summaries {
        let Some((info, sessions)) = infos.get(&summary.machine_id) else {
            continue;
        };
        if let MachineStatus::Connected {
            terminal_server_update,
            terminal_server_sessions,
            ..
        } = &mut summary.status
        {
            // This Mac (SW-AGR-19 PR-5) is flagged for an older server too;
            // a remote server also carries the sessions an update ends (This
            // Mac's are read when its update is confirmed).
            *terminal_server_update = info.relation == terminal_server_info::ServerRelation::Older;
            *terminal_server_sessions =
                if *terminal_server_update && summary.machine_id != LOCAL_MACHINE_ID {
                    *sessions
                } else {
                    None
                };
            summary.terminal_server = Some(info.clone());
        }
    }
}

fn apply_manager_state(
    summary: &mut MachineSummary,
    state: super::host_connection_manager::HostConnectionState,
) {
    if summary
        .declaration
        .as_ref()
        .is_some_and(|d| d.identity_change.is_some())
    {
        return;
    }
    summary.status = match state {
        super::host_connection_manager::HostConnectionState::Connected { .. } => {
            let workspaces = match &summary.status {
                MachineStatus::Connected { workspaces, .. } => *workspaces,
                _ => None,
            };
            MachineStatus::Connected {
                workspaces,
                terminal_server_update: false,
                terminal_server_sessions: None,
            }
        }
        super::host_connection_manager::HostConnectionState::Connecting => {
            MachineStatus::Connecting
        }
        super::host_connection_manager::HostConnectionState::Handshaking => {
            MachineStatus::Handshaking
        }
        super::host_connection_manager::HostConnectionState::Unreachable { last_error } => {
            MachineStatus::ConnectionFailed { last_error }
        }
        super::host_connection_manager::HostConnectionState::Incompatible { n_sessions, proof } => {
            MachineStatus::TerminalServerFrozen {
                count: Some(n_sessions),
                replacement_supported: proof.is_some(),
            }
        }
        super::host_connection_manager::HostConnectionState::UnsupportedLegacy { .. } => {
            MachineStatus::TerminalServerFrozen {
                count: None,
                replacement_supported: false,
            }
        }
        super::host_connection_manager::HostConnectionState::ReplacementReserved { count } => {
            MachineStatus::ReplacementReserved { count }
        }
        super::host_connection_manager::HostConnectionState::Replacing { count } => {
            MachineStatus::Replacing { count }
        }
        super::host_connection_manager::HostConnectionState::Readmitting { count } => {
            MachineStatus::Readmitting { count }
        }
        super::host_connection_manager::HostConnectionState::ReplacementFailed {
            old_stopped,
            count,
            repair,
        } => MachineStatus::ReplacementFailed {
            old_stopped,
            count,
            repair,
        },
        super::host_connection_manager::HostConnectionState::Disconnected => {
            MachineStatus::Disconnected
        }
    };
}

#[cfg(test)]
fn overlay_manager_states(
    manager: &super::host_connection_manager::HostConnectionManager,
    summaries: &mut [MachineSummary],
) {
    for summary in summaries {
        if let Some(state) = manager.state(&summary.machine_id) {
            apply_manager_state(summary, state);
        }
    }
}

fn apply_projection_snapshot(
    manager: &super::host_connection_manager::HostConnectionManager,
    summaries: &mut Vec<MachineSummary>,
) {
    let projection = manager.projection_snapshot(
        summaries
            .iter()
            .filter(|summary| summary.machine_id != LOCAL_MACHINE_ID)
            .map(|summary| summary.machine_id.clone()),
    );
    summaries.retain(|summary| {
        summary.machine_id == LOCAL_MACHINE_ID
            || !matches!(
                projection.get(&summary.machine_id),
                Some(super::host_connection_manager::ManagerProjection::ConfirmedRemoved)
            )
    });
    for summary in summaries {
        if let Some(super::host_connection_manager::ManagerProjection::Present(state)) =
            projection.get(&summary.machine_id)
        {
            apply_manager_state(summary, state.clone());
        }
    }
}

/// Tell the connection manager and the UI about a registry write. The UI
/// hears about it even when the manager has nothing to change (a machine it
/// never registered), and about both layers when the write moved a machine.
async fn notify_manager_mutation(
    app: &tauri::AppHandle,
    machine_id: String,
    prior_scope: Option<Scope>,
) -> Result<(), String> {
    let mark = machines_changed_mark(app);
    let observed = observe_manager_mutation(app, machine_id.clone()).await;
    let scope = observed.as_ref().ok().cloned().flatten();
    announce_registry_write(app, prior_scope, scope, &machine_id, Some(mark));
    observed.map(|_| ())
}

/// Announce a registry write to the UI, once per layer it touched. A layer
/// the connection manager already announced for this machine since `mark`
/// (it does when the write changed its member) is not announced again.
fn announce_registry_write<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    prior_scope: Option<Scope>,
    scope: Option<Scope>,
    machine_id: &str,
    mark: Option<u64>,
) {
    let announce = |scope: Option<Scope>| {
        if !mark.is_some_and(|mark| announced_since(app, &scope, machine_id, mark)) {
            emit_machines_changed(app, scope, machine_id);
        }
    };
    if prior_scope.is_some() && scope.is_some() && prior_scope != scope {
        announce(prior_scope.clone());
    }
    announce(scope.or(prior_scope));
}

async fn observe_manager_mutation(
    app: &tauri::AppHandle,
    machine_id: String,
) -> Result<Option<Scope>, String> {
    let manager = app.state::<super::host_connection_manager::HostConnectionManager>();
    let seq = manager.reserve_observation();
    let env = env_of(app).await?;
    let (scope, settle_revision) =
        observe_registry_write(&manager, seq, env, machine_id.clone()).await?;
    if let Some(settle_revision) = settle_revision {
        let settle_app = app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(75)).await;
            let manager =
                settle_app.state::<super::host_connection_manager::HostConnectionManager>();
            schedule_auto_connects(
                &settle_app,
                manager.settle_self_write(&machine_id, seq, &settle_revision),
            );
        });
    }
    Ok(scope)
}

type SettleRevision = Option<registry::RegistryRevision>;

/// Apply one registry write to the connection manager. Returns the machine's
/// layer and, while it is still saved, the revision its watcher echo carries.
async fn observe_registry_write(
    manager: &super::host_connection_manager::HostConnectionManager,
    seq: super::host_connection_manager::ObservationSeq,
    env: Env,
    machine_id: String,
) -> Result<(Option<Scope>, Option<SettleRevision>), String> {
    let snapshot_id = machine_id.clone();
    let record = crate::commands::await_blocking("machine_manager_snapshot", move || {
        manager_record(&env, &snapshot_id)
    })
    .await?;
    let scope = record.as_ref().and_then(|record| record.scope.clone());
    let settle_revision = record.as_ref().map(|record| record.revision.clone());
    manager.observe_mutation(seq, machine_id, record).await;
    Ok((scope, settle_revision))
}

/// Open the sole policy-authenticated and pinned SSH transport for a saved
/// remote machine. RM-4 still owns greeting-first protocol admission.
///
/// This is an internal Rust API for RM-4, not a webview command. Its only
/// caller-controlled connection value is `machine_id`; the declaration,
/// personal binding, credentials, pins, and expected terminal-server identity
/// are loaded and verified here. RM-4 must run `child_guard.close_and_reap()`
/// on its blocking executor for normal disconnect/shutdown; Drop is only the
/// bounded cancellation fail-safe.
pub(crate) struct PinnedSshOpenAttempt {
    cancelled: Arc<AtomicBool>,
    progress: OpenProgress,
    worker: tokio::task::JoinHandle<Result<PinnedSshTransport, PinnedSshOpenError>>,
}

/// The step a pinned open is in and when each began, so a connect that runs
/// out of time can say which remote round trip stalled.
#[derive(Clone)]
pub(crate) struct OpenProgress(Arc<std::sync::Mutex<Vec<(&'static str, std::time::Instant)>>>);

impl Default for OpenProgress {
    fn default() -> Self {
        Self(Arc::new(std::sync::Mutex::new(vec![(
            "starting",
            std::time::Instant::now(),
        )])))
    }
}

impl OpenProgress {
    pub(crate) fn enter(&self, step: &'static str) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((step, std::time::Instant::now()));
    }

    pub(crate) fn current(&self) -> &'static str {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .map_or("starting", |(step, _)| step)
    }

    /// Each step entered so far and how long it took, the last one until now.
    pub(crate) fn timings(&self) -> Vec<(&'static str, Duration)> {
        let steps = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let now = std::time::Instant::now();
        steps
            .iter()
            .enumerate()
            .map(|(i, (step, at))| {
                let end = steps.get(i + 1).map_or(now, |(_, next)| *next);
                (*step, end.duration_since(*at))
            })
            .collect()
    }
}

impl PinnedSshOpenAttempt {
    fn spawn(
        open: impl FnOnce(&OpenProgress) -> Result<PinnedSshTransport, PinnedSshOpenError>
            + Send
            + 'static,
    ) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        let progress = OpenProgress::default();
        let worker_progress = progress.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let opened = open(&worker_progress);
            if worker_cancelled.load(Ordering::SeqCst) {
                if let Ok(transport) = opened {
                    drop(transport.reader);
                    drop(transport.writer);
                    transport.child_guard.close_and_reap()?;
                }
                return Err(PinnedSshOpenError::Other(
                    "pinned SSH open cancelled".into(),
                ));
            }
            opened
        });
        Self {
            cancelled,
            progress,
            worker,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_spawn(
        open: impl FnOnce() -> Result<PinnedSshTransport, String> + Send + 'static,
    ) -> Self {
        Self::spawn(move |_| open().map_err(PinnedSshOpenError::Other))
    }

    /// The step the open is in now.
    pub(crate) fn step(&self) -> &'static str {
        self.progress.current()
    }

    /// Each step so far and its duration, for diagnosing a slow connect.
    pub(crate) fn timings(&self) -> Vec<(&'static str, Duration)> {
        self.progress.timings()
    }

    pub(crate) async fn wait(&mut self) -> Result<PinnedSshTransport, PinnedSshOpenError> {
        (&mut self.worker).await.map_err(|error| {
            PinnedSshOpenError::Other(format!("open_pinned_ssh_transport task failed: {error}"))
        })?
    }

    pub(crate) async fn finish(mut self) -> Result<PinnedSshTransport, PinnedSshOpenError> {
        self.wait().await
    }

    pub(crate) async fn cancel_and_reap(self) -> Result<(), String> {
        self.cancelled.store(true, Ordering::SeqCst);
        match self.finish().await {
            Ok(transport) => {
                crate::commands::await_blocking("close timed-out pinned SSH transport", move || {
                    drop(transport.reader);
                    drop(transport.writer);
                    transport.child_guard.close_and_reap()
                })
                .await
            }
            Err(PinnedSshOpenError::Other(error)) if error == "pinned SSH open cancelled" => Ok(()),
            Err(_) => Ok(()),
        }
    }
}

/// A legacy machine's refusal to open a terminal tab, naming the fix.
pub(crate) const LEGACY_TAB_REFUSAL: &str = "Terminal tabs need a private account on this machine. \
     In Machines, choose Switch to private account: it keeps the machine's name, host key and settings.";

/// Why `machine_id` has no terminal-tab admission, and what to do about it.
pub(crate) async fn tab_admission_unavailable_reason(
    app: &tauri::AppHandle,
    machine_id: &str,
) -> &'static str {
    #[derive(Clone, Copy)]
    enum Setup {
        Legacy,
        PrivateUnready,
        Other,
    }
    let setup = match env_of(app).await {
        Ok(env) => {
            let id = machine_id.to_string();
            crate::commands::await_blocking("machine setup for tab refusal", move || {
                Ok(if private_setup::claims(&env, &id)? {
                    if private_setup::ready(&env, &id)? {
                        Setup::Other
                    } else {
                        Setup::PrivateUnready
                    }
                } else if env.registry.find(&id)?.is_some() {
                    Setup::Legacy
                } else {
                    Setup::Other
                })
            })
            .await
            .unwrap_or(Setup::Other)
        }
        Err(_) => Setup::Other,
    };
    match setup {
        Setup::Legacy => LEGACY_TAB_REFUSAL,
        Setup::PrivateUnready => {
            "Finish this machine's private setup in Machines (enroll your account, then install its terminal server)."
        }
        Setup::Other => {
            "This machine is not connected. Connect it in Machines, then open the tab again."
        }
    }
}

pub(crate) async fn begin_pinned_ssh_transport(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<PinnedSshOpenAttempt, String> {
    let env = env_of(&app).await?;
    Ok(PinnedSshOpenAttempt::spawn(move |progress| {
        open_pinned_saved_with(&env, keychain().as_ref(), &machine_id, progress)
    }))
}

fn legacy_host_proof_saved(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: &str,
) -> Result<Option<LegacyHostProof>, String> {
    if private_setup::claims(env, machine_id)? {
        return private_setup::proof(env, store, machine_id);
    }
    let snapshot = transport_snapshot(env, machine_id)?;
    let declaration = &snapshot.declaration;
    let binding = &snapshot.binding;
    let pinned = declaration
        .pinned_host_key
        .as_deref()
        .map(HostKey::parse)
        .transpose()?
        .ok_or_else(|| format!("Machine {machine_id:?} has no verified host key"))?;
    let expected_server_id = identity::expected_server_id(binding, machine_id)?;
    let input = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: declaration.label.clone(),
        host: declaration.host.clone(),
        port: declaration.port,
        username: binding.username.clone(),
        jump_host: declaration.jump_host.clone(),
        sign_in: stored_sign_in(binding)?,
        trusted_host_key_fp: None,
    };
    let reach = reach(env, store, machine_id, &input)?;
    check::verified_host_key(&reach.connector, Trust::Pinned(&pinned), &env.scratch())?;
    let output = reach
        .ssh(Mux::Fresh)
        .output_within(&legacy_control_script(), Duration::from_secs(5))?;
    if !output.status.success() {
        return Err(format!(
            "legacy control-plane verification exited {:?}",
            output.status.code()
        ));
    }
    let proof = parse_legacy_host_proof(&output.stdout, expected_server_id).ok_or_else(|| {
        "legacy control-plane evidence was malformed or did not match the pinned server".to_string()
    })?;
    revalidate_transport_generation(env, machine_id, &snapshot.generation)
        .map_err(|error| error.to_string())?;
    Ok(Some(proof))
}

pub(crate) async fn legacy_host_proof(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<Option<LegacyHostProof>, String> {
    let env = env_of(&app).await?;
    crate::commands::await_blocking("verify legacy host control plane", move || {
        legacy_host_proof_saved(&env, keychain().as_ref(), &machine_id)
    })
    .await
}

pub async fn open_pinned_ssh_transport(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<PinnedSshTransport, PinnedSshOpenError> {
    begin_pinned_ssh_transport(app, machine_id)
        .await?
        .finish()
        .await
}

/// Revalidate the opaque generation returned by [`open_pinned_ssh_transport`]
/// immediately before RM-4 publishes/adopts the greeting-admitted stream.
/// This reads the same coherent declaration, personal binding, and verified
/// pin snapshot as the constructor and exposes none of those inputs.
pub async fn revalidate_pinned_transport_generation(
    app: tauri::AppHandle,
    machine_id: String,
    generation: String,
) -> Result<(), PinnedTransportGenerationError> {
    let env = env_of(&app)
        .await
        .map_err(PinnedTransportGenerationError::Missing)?;
    tokio::task::spawn_blocking(move || {
        revalidate_transport_generation(&env, &machine_id, &generation)
    })
    .await
    .map_err(|error| {
        PinnedTransportGenerationError::Missing(format!("revalidation task failed: {error}"))
    })?
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplacementDispatchFailure {
    pub old_stopped: bool,
    pub message: String,
}

fn replace_pinned_saved(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: &str,
    proof: &LegacyHostProof,
    nonce: &str,
    epoch: &str,
) -> Result<String, ReplacementDispatchFailure> {
    let fail = |message| ReplacementDispatchFailure {
        old_stopped: false,
        message,
    };
    if private_setup::claims(env, machine_id).map_err(fail)? {
        return private_setup::replace(env, store, machine_id, proof, nonce, epoch);
    }
    let snapshot = transport_snapshot(env, machine_id).map_err(fail)?;
    let declaration = &snapshot.declaration;
    let binding = &snapshot.binding;
    let pinned = declaration
        .pinned_host_key
        .as_deref()
        .map(HostKey::parse)
        .transpose()
        .map_err(fail)?
        .ok_or_else(|| fail("machine has no verified host key".into()))?;
    let input = MachineDraftInput {
        mode: Some(MachineMode::Legacy),
        label: declaration.label.clone(),
        host: declaration.host.clone(),
        port: declaration.port,
        username: binding.username.clone(),
        jump_host: declaration.jump_host.clone(),
        sign_in: stored_sign_in(binding).map_err(fail)?,
        trusted_host_key_fp: None,
    };
    let reach = reach(env, store, machine_id, &input).map_err(fail)?;
    check::verified_host_key(&reach.connector, Trust::Pinned(&pinned), &env.scratch())
        .map_err(fail)?;
    let ssh = reach.ssh(Mux::Fresh);
    let installed = install::install(&ssh, &env.artifacts, &env.shell_init).map_err(fail)?;
    revalidate_transport_generation(env, machine_id, &snapshot.generation)
        .map_err(|e| fail(e.to_string()))?;
    let observed = legacy_host_proof_saved(env, store, machine_id)
        .map_err(fail)?
        .ok_or_else(|| fail("replacement process proof disappeared".into()))?;
    revalidate_replacement_process_proof(proof, &observed).map_err(fail)?;

    let authorization_dir = env
        .scratch()
        .join(format!("replace-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&authorization_dir)
        .map_err(|e| fail(format!("create replacement authorization: {e}")))?;
    let authorization = authorization_dir.join("authorization");
    std::fs::write(&authorization, proof.authorization_record(nonce, epoch))
        .map_err(|e| fail(format!("write replacement authorization: {e}")))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&authorization, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| fail(format!("protect replacement authorization: {e}")))?;
    }
    let step = install::Step {
        script: install::replace_script(&installed.version).map_err(fail)?,
        upload: Some(authorization),
    };
    let output = ssh
        .output_within(&step, Duration::from_secs(20))
        .map_err(|message| ReplacementDispatchFailure {
            // Once the destructive child is dispatched, transport loss cannot
            // prove that its pidfd signal did not succeed.
            old_stopped: true,
            message,
        });
    let _ = std::fs::remove_dir_all(&authorization_dir);
    let output = output?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let old_stopped = stderr.contains("CODESENSE_REPLACE_PHASE=old-stopped");
    if !output.status.success() {
        return Err(ReplacementDispatchFailure {
            old_stopped,
            message: stderr.trim().to_string(),
        });
    }
    revalidate_transport_generation(env, machine_id, &snapshot.generation).map_err(|e| {
        ReplacementDispatchFailure {
            old_stopped,
            message: e.to_string(),
        }
    })?;
    Ok(snapshot.generation)
}

fn revalidate_replacement_process_proof(
    expected: &LegacyHostProof,
    observed: &LegacyHostProof,
) -> Result<(), String> {
    #[cfg(test)]
    if std::env::var("RM4D_INJECT").as_deref() == Ok("rm4d_pid_reuse_after_probe") {
        return Err("replacement process identity changed after probe".into());
    }
    if expected == observed {
        Ok(())
    } else {
        Err("replacement process identity changed after probe".into())
    }
}

pub(crate) async fn replace_pinned_terminal_server(
    app: tauri::AppHandle,
    machine_id: String,
    proof: LegacyHostProof,
    nonce: String,
    epoch: String,
) -> Result<String, ReplacementDispatchFailure> {
    let env = env_of(&app)
        .await
        .map_err(|message| ReplacementDispatchFailure {
            old_stopped: false,
            message,
        })?;
    tokio::task::spawn_blocking(move || {
        replace_pinned_saved(
            &env,
            keychain().as_ref(),
            &machine_id,
            &proof,
            &nonce,
            &epoch,
        )
    })
    .await
    .map_err(|error| ReplacementDispatchFailure {
        old_stopped: false,
        message: format!("replacement task failed: {error}"),
    })?
}

/// Personal machines moved out of each org's scratch space into the profile's
/// own directory, with no migration: say once, in the log, where any that
/// were left behind are, since the list no longer shows them.
fn warn_stranded_personal_machines(env: &Env) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    if WARNED.swap(true, Ordering::SeqCst) {
        return;
    }
    let now = env
        .registry
        .dir(&Scope::Personal)
        .ok()
        .and_then(|dir| dir.parent().map(Path::to_path_buf));
    for dir in env.registry.stranded_personal_machine_dirs() {
        log::warn!(
            target: "machines",
            "machine files in {dir:?} are not read: personal machines now live in {now:?}; move or re-add them"
        );
    }
}

/// The machines `orgSlug`/`team` sees: This Mac first, then the team's
/// shared machines, then the user's personal ones.
#[tauri::command]
pub async fn list_machines(
    app: tauri::AppHandle,
    org_slug: String,
    team: Option<String>,
    fresh_stats: Option<bool>,
) -> Result<Vec<MachineSummary>, String> {
    let env = env_of(&app).await?;
    let local = match crate::commands::pty_layout::webview_layout_snapshots(app.clone()).await {
        Some(Ok(snapshots)) => snapshots.iter().map(|s| s.workspaces.len()).sum(),
        _ => 0,
    };
    let mut summaries = crate::commands::await_blocking("list_machines", move || {
        warn_stranded_personal_machines(&env);
        machines_for(&env, &org_slug, team.as_deref(), local)
    })
    .await?;
    let manager = app.state::<super::host_connection_manager::HostConnectionManager>();
    apply_projection_snapshot(&manager, &mut summaries);
    // After the manager's state, which resets the update flag.
    annotate_terminal_servers(&app, &mut summaries, false).await;
    if fresh_stats.unwrap_or(false) {
        crate::live_hub::machine_stats::refresh_for_tool(&app, &mut summaries).await;
    }
    Ok(summaries)
}

/// The machine files a team's list skips because they can't be read, so
/// Machines can name them instead of hiding them.
#[tauri::command]
pub async fn list_unreadable_machine_files(
    app: tauri::AppHandle,
    org_slug: String,
    team: Option<String>,
) -> Result<Vec<UnreadableMachineFile>, String> {
    let env = env_of(&app).await?;
    crate::commands::await_blocking("list_unreadable_machine_files", move || {
        unreadable_machine_files(&env, &org_slug, team.as_deref())
    })
    .await
}

/// `path` if it is a plain file of its own: never a link (a teammate can
/// commit one pointing at an app or a script) and never a folder or device.
fn plain_file(path: &Path) -> Result<&Path, String> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|e| format!("Couldn't read {path:?}: {e}"))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{path:?} is a link, not a machine file; remove it instead"
        ));
    }
    if !metadata.is_file() {
        return Err(format!("{path:?} is not a plain file; remove it instead"));
    }
    Ok(path)
}

/// Open an unreadable machine file as text, to repair it. Only a plain file
/// the team's list reports as unreadable is opened, and only in the text
/// editor: never with the file's own handler, which could run it.
#[tauri::command]
pub async fn open_unreadable_machine_file(
    app: tauri::AppHandle,
    org_slug: String,
    team: Option<String>,
    machine_id: String,
) -> Result<(), String> {
    let env = env_of(&app).await?;
    crate::commands::await_blocking("open_unreadable_machine_file", move || {
        let file = unreadable_machine_file(&env, &org_slug, team.as_deref(), &machine_id)?;
        let path = plain_file(Path::new(&file.path))?;
        let status = std::process::Command::new("/usr/bin/open")
            .arg("-t")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("Couldn't open {}: {e}", file.path))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("Couldn't open {} as text", file.path))
        }
    })
    .await
}

/// Delete an unreadable machine file. Its machine, kept while the file could
/// not be read, is then removed like any machine whose file went away.
#[tauri::command]
pub async fn remove_unreadable_machine_file(
    app: tauri::AppHandle,
    org_slug: String,
    team: Option<String>,
    machine_id: String,
) -> Result<(), String> {
    let env = env_of(&app).await?;
    let (env, scope) = crate::commands::await_blocking("remove_unreadable_machine_file", {
        let machine_id = machine_id.clone();
        move || {
            let file = unreadable_machine_file(&env, &org_slug, team.as_deref(), &machine_id)?;
            std::fs::remove_file(&file.path)
                .map_err(|e| format!("Couldn't remove {}: {e}", file.path))?;
            Ok((env, file.scope))
        }
    })
    .await?;
    let manager = app.state::<super::host_connection_manager::HostConnectionManager>();
    if let Err(error) = reconcile_manager(&manager, env).await {
        log::warn!(target: "machines", "reconcile after removing {machine_id}'s file: {error}");
    }
    emit_machines_changed(&app, Some(scope), &machine_id);
    Ok(())
}

/// What the local SSH setup already knows: `~/.ssh/config` hosts and the
/// ssh-agent's keys.
#[tauri::command]
pub async fn list_ssh_sources() -> Result<SshLocalSources, String> {
    crate::commands::await_blocking("list_ssh_sources", || {
        Ok(local_sources(dirs::home_dir().as_deref()))
    })
    .await
}

/// Ask for `keyPath`'s passphrase in a system dialog and keep it in the
/// keychain; returns its handle, or null when the person cancelled.
#[tauri::command]
pub async fn capture_machine_passphrase(
    app: tauri::AppHandle,
    key_path: String,
) -> Result<Option<CredentialRef>, String> {
    let env = env_of(&app).await?;
    crate::commands::await_blocking("capture_machine_passphrase", move || {
        let unlocks = |key: &Path, secret: &Secret| {
            askpass::check_passphrase(
                key,
                secret.expose(),
                &env.askpass_program,
                &env.askpass_dir(),
            )
        };
        capture_passphrase(
            &env,
            keychain().as_ref(),
            &key_path,
            passphrase::prompt,
            unlocks,
        )
    })
    .await
}

/// Run the Add-machine checklist (nothing is saved).
#[tauri::command]
pub async fn test_machine_connection(
    app: tauri::AppHandle,
    machine_id: Option<String>,
    input: MachineDraftInput,
) -> Result<MachineTestReportDto, String> {
    let env = env_of(&app).await?;
    crate::commands::await_blocking("test_machine_connection", move || {
        test_connection(&env, keychain().as_ref(), machine_id.as_deref(), &input)
    })
    .await
}

/// Whether this call switches the saved legacy machine `machine_id` to a
/// private account: only when the caller asks for one explicitly.
fn switches_legacy(env: &Env, machine_id: Option<&str>, input: &MachineDraftInput) -> bool {
    input.mode == Some(MachineMode::PrivateAccount)
        && machine_id.is_some_and(|id| {
            effective_mode(
                env,
                Some(id),
                &MachineDraftInput {
                    mode: None,
                    ..input.clone()
                },
            )
            .is_ok_and(|mode| mode == MachineMode::Legacy)
        })
}

/// Switching a connected legacy machine first ends its legacy connection, so
/// nothing keeps using the shared-account transport under the private record.
async fn end_legacy_connection_before_switch(
    app: &tauri::AppHandle,
    machine_id: Option<&str>,
    input: &MachineDraftInput,
) {
    let Ok(env) = env_of(app).await else { return };
    let (id, input) = (machine_id.map(String::from), input.clone());
    let switching = crate::commands::await_blocking("machine switch check", {
        let id = id.clone();
        move || Ok(switches_legacy(&env, id.as_deref(), &input))
    })
    .await
    .unwrap_or(false);
    if let (true, Some(id)) = (switching, id) {
        let manager = app.state::<super::host_connection_manager::HostConnectionManager>();
        if let Err(error) = manager.disconnect(&id).await {
            log::info!(target: "machines", "switching {id} to a private account: {error}");
        }
    }
}

/// Upload this app's terminal server to the machine over SSH (no sudo, never
/// a download on the machine).
#[tauri::command]
pub async fn enroll_private_machine_account(
    app: tauri::AppHandle,
    machine_id: Option<String>,
    input: MachineDraftInput,
) -> Result<MachineTestReportDto, String> {
    if input.mode == Some(MachineMode::Legacy) {
        return Err("private mode required".into());
    }
    let env = env_of(&app).await?;
    end_legacy_connection_before_switch(&app, machine_id.as_deref(), &input).await;
    let enrolled_id = machine_id.clone();
    let report = crate::commands::await_blocking("enroll_private_machine_account", move || {
        private_setup::enroll(&env, keychain().as_ref(), machine_id.as_deref(), &input)
    })
    .await?;
    // Enrollment rewrites a saved machine's private binding, which the
    // registry watcher does not see.
    if let Some(id) = enrolled_id {
        emit_machines_changed(&app, None, &id);
    }
    Ok(report)
}

#[tauri::command]
pub async fn install_terminal_server(
    app: tauri::AppHandle,
    machine_id: Option<String>,
    input: MachineDraftInput,
) -> Result<InstallTerminalServerResult, String> {
    let env = env_of(&app).await?;
    end_legacy_connection_before_switch(&app, machine_id.as_deref(), &input).await;
    crate::commands::await_blocking("install_terminal_server", move || {
        install_server(&env, keychain().as_ref(), machine_id.as_deref(), &input)
    })
    .await
}

/// Agent saves execute the same writer as native saves; caller authority is
/// fixed by this backend receiving operation, never by tool arguments.
pub(crate) fn save_for_agent(
    env: &Env,
    store: &dyn CredentialStore,
    machine_id: Option<&str>,
    input: &MachineDraftInput,
    share: &Scope,
    connect: bool,
) -> Result<MachineSummary, String> {
    save_as(
        env,
        store,
        machine_id,
        input,
        share,
        connect,
        crate::panes::commands::Actor::Agent,
    )
}

pub(crate) async fn save_machine_for_agent(
    app: tauri::AppHandle,
    machine_id: Option<String>,
    input: MachineDraftInput,
    share: Scope,
    connect: bool,
) -> Result<MachineSummary, String> {
    save_machine_as(
        app,
        machine_id,
        input,
        share,
        connect,
        crate::panes::commands::Actor::Agent,
    )
    .await
}

/// Add or change a machine; `connect: false` keeps a "Needs setup" draft.
#[tauri::command]
pub async fn save_machine(
    app: tauri::AppHandle,
    machine_id: Option<String>,
    input: MachineDraftInput,
    share: Scope,
    connect: bool,
) -> Result<MachineSummary, String> {
    save_machine_as(
        app,
        machine_id,
        input,
        share,
        connect,
        crate::panes::commands::Actor::Human,
    )
    .await
}

async fn save_machine_as(
    app: tauri::AppHandle,
    machine_id: Option<String>,
    input: MachineDraftInput,
    share: Scope,
    connect: bool,
    actor: crate::panes::commands::Actor,
) -> Result<MachineSummary, String> {
    let env = env_of(&app).await?;
    end_legacy_connection_before_switch(&app, machine_id.as_deref(), &input).await;
    let (prior, saved) = crate::commands::await_blocking("save_machine", move || {
        let prior = machine_id.as_deref().and_then(|id| current_scope(&env, id));
        let operation = match actor {
            crate::panes::commands::Actor::Human => save,
            crate::panes::commands::Actor::Agent => save_for_agent,
        };
        operation(
            &env,
            keychain().as_ref(),
            machine_id.as_deref(),
            &input,
            &share,
            connect,
        )
        .map(|saved| (prior, saved))
    })
    .await?;
    notify_manager_mutation(&app, saved.machine_id.clone(), prior).await?;
    Ok(saved)
}

/// The local-only Hide.
#[tauri::command]
pub async fn set_machine_hidden(
    app: tauri::AppHandle,
    machine_id: String,
    hidden: bool,
) -> Result<(), String> {
    let env = env_of(&app).await?;
    let notify_id = machine_id.clone();
    crate::commands::await_blocking("set_machine_hidden", move || {
        set_hidden(&env, &machine_id, hidden)
    })
    .await?;
    notify_manager_mutation(&app, notify_id, None).await
}

/// Explicitly opt one personal machine binding into or out of background
/// reconnect. This is the only RM-3 command that mutates `auto_connect`.
#[tauri::command]
pub async fn set_machine_auto_connect(
    app: tauri::AppHandle,
    machine_id: String,
    auto_connect: bool,
) -> Result<(), String> {
    let env = env_of(&app).await?;
    let notify_id = machine_id.clone();
    crate::commands::await_blocking("set_machine_auto_connect", move || {
        set_auto_connect(&env, &machine_id, auto_connect)
    })
    .await?;
    notify_manager_mutation(&app, notify_id, None).await
}

/// Move a team's machine back to its publisher's personal layer.
#[tauri::command]
pub async fn unshare_machine(
    app: tauri::AppHandle,
    machine_id: String,
) -> Result<MachineSummary, String> {
    routines_let_go_of(&app, &machine_id).await?;
    let env = env_of(&app).await?;
    let (prior, result) = crate::commands::await_blocking("unshare_machine", move || {
        let prior = current_scope(&env, &machine_id);
        unshare(&env, &machine_id).map(|result| (prior, result))
    })
    .await?;
    notify_manager_mutation(&app, result.machine_id.clone(), prior).await?;
    Ok(result)
}

/// Delete a machine (for its whole team, when shared).
#[tauri::command]
pub async fn remove_machine(app: tauri::AppHandle, machine_id: String) -> Result<(), String> {
    routines_let_go_of(&app, &machine_id).await?;
    let env = env_of(&app).await?;
    let notify_id = machine_id.clone();
    let prior = crate::commands::await_blocking("remove_machine", move || {
        let prior = current_scope(&env, &machine_id);
        remove(&env, keychain().as_ref(), &machine_id).map(|()| prior)
    })
    .await?;
    notify_manager_mutation(&app, notify_id, prior).await
}

/// SW-AGR-8: a machine that routines are armed on, or that has not yet
/// confirmed it stopped routines moved off it, is not removed (nor taken out
/// of the team): it may keep firing them, unseen. Move them first, or
/// release a handover from a machine that is gone.
async fn routines_let_go_of(app: &tauri::AppHandle, machine_id: &str) -> Result<(), String> {
    let Ok((home, user)) = crate::commands::route_to_context::resolve_ambient(app).await else {
        // Nobody is signed in: no routines are read, none are stranded.
        return Ok(());
    };
    let principal = crate::domain::Principal::from(user);
    let machine = machine_id.to_string();
    crate::commands::await_blocking("machine_routines", move || {
        crate::routines::machine_arm_app::check_machine_removal(&home, &principal, &machine)
    })
    .await
}

/// Every private machine's label by id: where a remote tab runs, as people
/// name it.
pub(crate) async fn machine_labels(
    app: &tauri::AppHandle,
) -> std::collections::BTreeMap<String, String> {
    let Ok(env) = env_of(app).await else {
        return Default::default();
    };
    crate::commands::await_blocking("machine_labels", move || {
        Ok(private_setup::summaries(&env)
            .unwrap_or_default()
            .into_iter()
            .map(|machine| (machine.machine_id, machine.label))
            .collect())
    })
    .await
    .unwrap_or_default()
}

/// The machines this person hid, or never finished setting up (SW-AGR-2b):
/// a `machine: any` routine does not wait to read their sessions.
pub(crate) async fn unattended_machines(app: &tauri::AppHandle) -> Vec<String> {
    let Ok(env) = env_of(app).await else {
        return Vec::new();
    };
    crate::commands::await_blocking("unattended_machines", move || {
        Ok(private_setup::summaries(&env)
            .unwrap_or_default()
            .into_iter()
            .filter(|machine| {
                machine
                    .binding
                    .as_ref()
                    .is_none_or(|binding| binding.hidden)
            })
            .map(|machine| machine.machine_id)
            .collect())
    })
    .await
    .unwrap_or_default()
}

/// Check the current profile's exact team lens before remote terminal work.
pub(crate) async fn authorize_terminal_machine(
    app: &tauri::AppHandle,
    machine_id: &str,
    team_key: &str,
) -> Result<(), String> {
    let (org, team) = super::pty_layout::parse_team_key(team_key);
    let team = team
        .filter(|team| !team.is_empty())
        .ok_or("an accessible team is required")?;
    if machine_id == LOCAL_MACHINE_ID || org.is_empty() {
        return Err("a configured remote machine is required".into());
    }
    let env = env_of(app).await?;
    terminal_machine_visible(
        &machines_for(&env, org, Some(team), 0)?,
        machine_id,
        org,
        team,
    )
    .then_some(())
    .ok_or_else(|| "machine is unavailable to this team".into())
}

/// Whether `machine_id` is a remote machine `team` of `org` may run terminals
/// on: one of its listed ssh machines, personal or shared with that team.
pub(crate) fn terminal_machine_visible(
    machines: &[MachineSummary],
    machine_id: &str,
    org: &str,
    team: &str,
) -> bool {
    machines.iter().any(|machine| {
        machine.machine_id == machine_id
            && machine.kind == "ssh"
            && machine
                .declaration
                .as_ref()
                .is_none_or(|d| d.identity_change.is_none())
            && match &machine.scope {
                Some(Scope::Personal) => true,
                Some(Scope::Team {
                    org_slug,
                    team: scoped_team,
                }) => org_slug == org && scoped_team == team,
                None => false,
            }
    })
}

/// Explicit personal opt-in. The normal binding mutation revokes old connections.
#[tauri::command]
pub async fn set_machine_remote_mcp_control(
    app: tauri::AppHandle,
    machine_id: String,
    enabled: bool,
) -> Result<(), String> {
    let env = env_of(&app).await?;
    let notify_id = machine_id.clone();
    crate::commands::await_blocking("set_machine_remote_mcp_control", move || {
        private_setup::set_remote_mcp_control(&env, &machine_id, enabled)
    })
    .await?;
    notify_manager_mutation(&app, notify_id, None).await
}

pub(crate) async fn authorize_remote_mcp_control(
    app: &tauri::AppHandle,
    machine_id: &str,
) -> Result<(), String> {
    let env = env_of(app).await?;
    let machine_id = machine_id.to_owned();
    crate::commands::await_blocking("authorize_remote_mcp_control", move || {
        private_setup::authorize_remote_mcp_control(&env, &machine_id)
    })
    .await
}
