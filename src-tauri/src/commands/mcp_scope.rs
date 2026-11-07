// SPDX-License-Identifier: MIT
//! SW-AN-41: per-profile MCP server scoping — the launch-time projection.
//!
//! A launch profile may declare an `mcp_allow` allowlist (see
//! [`crate::agents::adapter::ResolvedLaunchProfile::mcp_allow`]). At the spawn boundary
//! this module projects the effective set `(allow ∪ {bridge}) ∩ present` of the
//! folder's own project config into an **ephemeral, machine-local** config file and the
//! launcher points the agent at it with `--mcp-config <path> --strict-mcp-config`. The
//! folder's config is **read-only** here, and CodeSense never writes its own config into
//! a launch folder: every launch, scoped or not, gets the bridge (always this app's
//! resolved path) and, for Claude, the status hooks from per-session files
//! ([`project_profile_launch`]), so concurrent agents in one worktree can't collide.
//!
//! **Why `--strict-mcp-config` is mandatory:** `--mcp-config` alone MERGES its servers
//! on top of the project/user/global scopes (it does not scope down); only the strict
//! flag makes the launch use *only* the projected set. The two flags are always emitted
//! together (see [`crate::commands::mcp_workspace`]).
//!
//! **Secrets/hygiene:** the projected config copies server sub-objects verbatim, so it
//! can carry resolved secrets (a server's `env`) and the bridge's absolute path. It is
//! therefore written 0600 under this instance's private state root,
//! session-keyed, and removed on session teardown ([`remove_scope_config`]).

use crate::shared::config_format::ConfigFormat;
use codesense_pty_core::agent_cli::{descriptor, AgentCli};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use log::warn;
use serde_json::{json, Value};

pub(crate) mod inventory;

const CREDENTIAL_JOURNAL: &str = ".credential-copyback.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct CredentialCopyJournal {
    source: PathBuf,
    candidate: PathBuf,
    original_hash: String,
    original_len: u64,
    #[cfg(unix)]
    original_device: u64,
    #[cfg(unix)]
    original_inode: u64,
}

/// The server-map key CodeSense's own MCP bridge is registered under: the stable id an
/// agent addresses CodeSense's tools by. The adapters' `mcp_scope.bridge_id`
/// carries it in production; this anchors a drift test against the built-in adapters.
#[cfg(test)]
pub const BRIDGE_SERVER_KEY: &str = "codesense";

use codesense_pty_core::agent_cli::{
    render_scope_flags, McpScope, Passthrough, PassthroughMode, ScopeInjection,
};

/// Minimum age before an owned, confirmed-dead scope can be reaped at startup.
const STALE_AGE: Duration = Duration::from_secs(3600);

/// Read-only fallback for sessions launched before scopes moved into instance state.
/// Never swept: another instance may still have a live session here.
const LEGACY_SCOPE_DIR: &str = "codesense-mcp-scope";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopeCreation {
    session: String,
    stamp: u64,
}

struct ScopeRecord {
    creation: ScopeCreation,
    identity: (u64, u64),
    owner: Option<super::pty_host::LocalEffectClaim>,
    relay_enabled: bool,
    relay_retired: bool,
    prepared_at: std::time::Instant,
    detached_lease: Option<std::sync::Weak<()>>,
}

type LaunchRoot = std::sync::Arc<
    dyn Fn() -> Option<codesense_pty_core::activity::peer::PeerProcess> + Send + Sync,
>;

#[derive(Default)]
struct ScopeState {
    record: Option<ScopeRecord>,
    relay_identity: std::sync::Weak<crate::mcp::local_mcp_relay::identity::Identity>,
    relay_root: Option<LaunchRoot>,
}
impl ScopeState {
    fn as_ref(&self) -> Option<&ScopeRecord> {
        self.record.as_ref()
    }
    fn as_mut(&mut self) -> Option<&mut ScopeRecord> {
        self.record.as_mut()
    }
    fn clear(&mut self) {
        self.record = None;
        self.relay_identity = std::sync::Weak::new();
        self.relay_root = None;
    }
}
type ScopeSlot = std::sync::Arc<std::sync::Mutex<ScopeState>>;

pub(crate) fn bind_relay_identity(
    session: &str,
    identity: &std::sync::Arc<crate::mcp::local_mcp_relay::identity::Identity>,
) {
    scope_slot(session)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .relay_identity = std::sync::Arc::downgrade(identity);
}

pub(crate) fn relay_peer_proven(
    session: &str,
    peer: codesense_pty_core::activity::peer::PeerProcess,
) -> bool {
    let identity = scope_slot(session)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .relay_identity
        .upgrade();
    identity.is_some_and(|identity| identity.proves(peer))
}

/// Attach the engine's existing host lookup before its PTY or detached child starts.
/// The callback can return no root during startup; that is never permission to enter.
pub(crate) fn bind_relay_launch(app: &tauri::AppHandle, session: &str) {
    let app = app.clone();
    let name = session.to_string();
    let root: LaunchRoot =
        std::sync::Arc::new(move || crate::mcp::handler::local_session_root(&app, &name));
    let slot = scope_slot(session);
    let mut state = slot.lock().unwrap_or_else(|e| e.into_inner());
    if state.as_ref().is_some_and(|record| !record.relay_enabled) {
        return;
    }
    state.relay_root = Some(root);
}

pub(crate) fn relay_ingress_proven(
    session: &str,
    peer: codesense_pty_core::activity::peer::PeerProcess,
) -> bool {
    let source = scope_slot(session)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .relay_root
        .clone();
    let root = source.and_then(|source| source());
    crate::delivery::authority::bind_caller(
        &crate::delivery::authority::TransportIdentity::LocalPeer(peer),
        session,
        |_| root,
    )
    .is_ok()
}

#[cfg(test)]
pub(crate) fn bind_fixture_relay_root(
    session: &str,
    root: codesense_pty_core::activity::peer::PeerProcess,
) {
    scope_slot(session)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .relay_root = Some(std::sync::Arc::new(move || Some(root)));
}

fn scope_slot(session: &str) -> ScopeSlot {
    static RECORDS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, ScopeSlot>>,
    > = std::sync::OnceLock::new();
    RECORDS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(session.into())
        .or_default()
        .clone()
}

fn scope_identity(path: &Path) -> Result<(u64, u64), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("scope directory identity changed".into());
        }
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err("scope creation identity unavailable".into())
    }
}

const RELAY_PREPARE_LIFETIME: Duration = Duration::from_secs(120);

fn relay_admitted(record: &ScopeRecord) -> bool {
    !record.relay_enabled
        || (!record.relay_retired
            && (record.owner.is_some()
                || match &record.detached_lease {
                    Some(lease) => lease.upgrade().is_some(),
                    None => record.prepared_at.elapsed() < RELAY_PREPARE_LIFETIME,
                }))
}

/// The existing detached run task holds this lease across its entire execution.
/// Cancellation drops it; unlike prepared state, it never relies on an age sweep.
pub(crate) fn retain_detached_scope(session: &str) -> Result<Option<std::sync::Arc<()>>, String> {
    let slot = scope_slot(session);
    let mut record = slot.lock().unwrap_or_else(|error| error.into_inner());
    let Some(record) = record.as_mut() else {
        return Ok(None);
    };
    if !record.relay_enabled {
        return Ok(None);
    }
    if !relay_admitted(record) || record.owner.is_some() || record.detached_lease.is_some() {
        return Err("prepared MCP scope is no longer eligible for this detached run".into());
    }
    let lease = std::sync::Arc::new(());
    record.detached_lease = Some(std::sync::Arc::downgrade(&lease));
    Ok(Some(lease))
}

fn enable_relay(creation: &ScopeCreation) -> Result<(), String> {
    let slot = scope_slot(&creation.session);
    let mut record = slot.lock().unwrap_or_else(|error| error.into_inner());
    let current = record
        .as_mut()
        .filter(|record| record.creation == *creation)
        .ok_or("MCP scope creation retired before relay admission")?;
    current.relay_enabled = true;
    Ok(())
}

fn validate_creation(creation: &ScopeCreation) -> Result<(), String> {
    let slot = scope_slot(&creation.session);
    let record = slot.lock().unwrap_or_else(|error| error.into_inner());
    let current = record
        .as_ref()
        .filter(|record| record.creation == *creation && relay_admitted(record))
        .ok_or("local MCP launch has retired")?;
    if scope_identity(&mcp_scope_session_dir(&creation.session))? != current.identity {
        return Err("local MCP launch directory identity changed".into());
    }
    Ok(())
}

pub(crate) fn bind_scope_creation(
    creation: &ScopeCreation,
    owner: &super::pty_host::LocalEffectClaim,
) -> Result<(), String> {
    let slot = scope_slot(&creation.session);
    let mut record = slot.lock().unwrap_or_else(|e| e.into_inner());
    let record = record.as_mut().ok_or("scope creation is no longer owned")?;
    if record.creation != *creation
        || !relay_admitted(record)
        || creation.session != owner.session
        || record.owner.as_ref().is_some_and(|prior| prior != owner)
        || scope_identity(&mcp_scope_session_dir(&creation.session))? != record.identity
    {
        return Err("scope creation does not match this runtime claim".into());
    }
    record.owner = Some(owner.clone());
    Ok(())
}

/// Keep preparation and teardown serialized by the existing exact scope resource slot.
pub(crate) fn prepare_created_scope<T>(
    creation: &ScopeCreation,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let slot = scope_slot(&creation.session);
    let record = slot.lock().unwrap_or_else(|error| error.into_inner());
    let current = record
        .as_ref()
        .ok_or("managed scope was removed before preparation")?;
    if current.creation != *creation
        || scope_identity(&mcp_scope_session_dir(&creation.session))? != current.identity
    {
        return Err("managed scope identity changed before preparation".into());
    }
    operation()
}

pub(crate) fn remove_owned_scope(owner: &super::pty_host::LocalEffectClaim) -> bool {
    let slot = scope_slot(&owner.session);
    let mut record = slot.lock().unwrap_or_else(|e| e.into_inner());
    let Some(current) = record
        .as_ref()
        .filter(|record| record.owner.as_ref() == Some(owner))
    else {
        return false;
    };
    let dir = mcp_scope_session_dir(&owner.session);
    if scope_identity(&dir).ok() != Some(current.identity) {
        return false;
    }
    record.as_mut().expect("validated scope").relay_retired = true;
    // Only this exact resource reservation spans copyback/deletion, never an
    // admission/provenance/registry lock. Replacement creation uses the same slot.
    crate::agents::adapter::teardown_managed_home(&dir);
    if finish_credential_copyback(&dir).is_err()
        || !crate::session_conversation::capture_before_removal(&owner.session, &dir)
        || std::fs::remove_dir_all(&dir).is_err()
    {
        return false;
    }
    record.clear();
    true
}

/// Idempotent terminal cleanup, serialized by the existing resource slot rather
/// than an effect/admission lock. Absence never authorizes a filesystem delete.
pub(crate) fn settle_owned_scope(
    owner: &super::pty_host::LocalEffectClaim,
) -> Result<bool, String> {
    let slot = scope_slot(&owner.session);
    let mut record = slot.lock().unwrap_or_else(|error| error.into_inner());
    let Some(current) = record.as_ref() else {
        return Ok(false);
    };
    if current.owner.as_ref() != Some(owner) {
        return Err("terminal cleanup scope belongs to another owner".into());
    }
    let dir = mcp_scope_session_dir(&owner.session);
    if scope_identity(&dir)? != current.identity {
        return Err("terminal cleanup scope identity changed".into());
    }
    record.as_mut().expect("validated scope").relay_retired = true;
    crate::agents::adapter::teardown_managed_home(&dir);
    finish_credential_copyback(&dir)?;
    if !crate::session_conversation::capture_before_removal(&owner.session, &dir) {
        return Ok(false);
    }
    std::fs::remove_dir_all(&dir).map_err(|error| error.to_string())?;
    record.clear();
    Ok(true)
}

pub(crate) fn transfer_scope_creation(
    creation: &ScopeCreation,
    old: &super::pty_host::LocalEffectClaim,
    new: &super::pty_host::LocalEffectClaim,
) -> Result<(), String> {
    let slot = scope_slot(&creation.session);
    let mut record = slot.lock().unwrap_or_else(|e| e.into_inner());
    let record = record.as_mut().ok_or("scope creation unavailable")?;
    if record.creation != *creation
        || record.owner.as_ref() != Some(old)
        || old.session != new.session
        || scope_identity(&mcp_scope_session_dir(&creation.session))? != record.identity
    {
        return Err("scope transfer does not own original creation".into());
    }
    record.owner = Some(new.clone());
    Ok(())
}

pub(crate) fn owned_creation(
    owner: &super::pty_host::LocalEffectClaim,
) -> Result<Option<ScopeCreation>, String> {
    let slot = scope_slot(&owner.session);
    let record = slot.lock().unwrap_or_else(|error| error.into_inner());
    let Some(record) = record
        .as_ref()
        .filter(|record| record.owner.as_ref() == Some(owner))
    else {
        return Ok(None);
    };
    if scope_identity(&mcp_scope_session_dir(&owner.session))? != record.identity {
        return Err("owned scope creation changed during recovery".into());
    }
    Ok(Some(record.creation.clone()))
}

/// Identity was checked off the admission/claim lock; this critical section
/// moves metadata only. Every later deletion still checks filesystem identity.
pub(crate) fn transfer_checked_creation(
    creation: &ScopeCreation,
    old: &super::pty_host::LocalEffectClaim,
    new: &super::pty_host::LocalEffectClaim,
) -> Result<(), String> {
    let slot = scope_slot(&old.session);
    let mut record = slot.lock().unwrap_or_else(|error| error.into_inner());
    let record = record
        .as_mut()
        .filter(|record| record.creation == *creation && record.owner.as_ref() == Some(old))
        .ok_or("scope recovery predecessor changed")?;
    record.owner = Some(new.clone());
    Ok(())
}

/// Compute the effective `{"mcpServers": …}` for a scoped launch: the bridge plus the
/// allowlisted servers that are actually present in `canonical`, each sub-object copied
/// **verbatim** (resolved command/args/env intact). Pure — no I/O, no Tauri state.
///
/// - The **bridge is force-included** (decision #5): under `--strict-mcp-config` only
///   the projected servers exist, so an agent without the `codesense` bridge could never
///   `signal_done`. Even `allow = []` yields the bridge.
/// - The allowlist is projected **verbatim** (SW-AV-27): the capability-tier ceiling that
///   once dropped over-tier verbs was CodeSense enforcing agent authority and is removed
///   with the control plane. Permissions live in the agent's own `mcp_allow`,
///   so an allowlisted verb present in `canonical` is projected as-is.
/// - A **dangling** allowlist entry (a server not installed in this worktree) is dropped
///   with a `warn!` breadcrumb; the stored intent is untouched (it auto-recovers if the
///   server returns).
pub fn effective_mcp_servers(canonical: &Value, allow: &[String], scope: &McpScope) -> Value {
    let empty = serde_json::Map::new();
    let present = canonical
        .get(&scope.server_map_key)
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    let mut selected = serde_json::Map::new();

    // Force-include the bridge first so it survives the filter regardless of `allow`.
    match present.get(&scope.bridge_id) {
        Some(bridge) => {
            selected.insert(scope.bridge_id.clone(), bridge.clone());
        }
        None => warn!(
            target: "mcp_scope",
            "bridge server {:?} absent from {:?} during scope projection — the scoped agent \
             may be unable to signal_done",
            scope.bridge_id, scope.config_filename
        ),
    }

    // SW-AV-27: the capability-tier admission (which allowlist verbs exceed a profile's
    // declared tier) was CodeSense enforcing agent authority; it is removed with the
    // control plane. Permissions live in the agent's own `mcp_allow`, which is
    // projected verbatim below.
    for raw in allow {
        // Tolerate hand-authoring whitespace (mcp_allow is hand-authored until SW-AN-42):
        // a stray `" buildkite"` should match, not silently drop as dangling. Casing is
        // NOT normalized — server-map keys are case-sensitive, so `"Buildkite"` is a
        // genuinely different id and correctly degrades (AC-5).
        let id = raw.trim();
        if id.is_empty() || id == scope.bridge_id {
            continue; // blank entry, or the bridge (already force-included; map dedups)
        }
        match present.get(id) {
            Some(server) => {
                selected.insert(id.to_string(), server.clone());
            }
            None => warn!(
                target: "mcp_scope",
                "launch profile allowlists MCP server {id:?} which is not installed in this \
                 worktree — dropped from the scoped set (stored intent retained)"
            ),
        }
    }

    let mut out = serde_json::Map::new();
    out.insert(scope.server_map_key.clone(), Value::Object(selected));
    Value::Object(out)
}

/// `config` with the entries the adapter of the CLI that takes this scope adds to its inline
/// config. A launch without them still works, so a failure to prepare them is only logged.
/// `version` is the release the launch spawns; one outside the CLI's qualified range tells
/// the adapter so, and an unknown one is taken as qualified (the entries check their host).
fn with_adapter_entries(mut config: Value, scope: &McpScope, version: Option<&str>) -> Value {
    let owner = AgentCli::ALL
        .into_iter()
        .find(|cli| descriptor(*cli).mcp_scope.as_ref() == Some(scope));
    if let (Some(cli), Some(object)) = (owner, config.as_object_mut()) {
        let qualified = version.is_none_or(|v| descriptor(cli).release.version_qualified(v));
        match (crate::agents::adapter::desktop(cli).inline_config)(qualified) {
            Ok(entries) => object.extend(entries),
            Err(error) => {
                warn!(target: "mcp_scope", "inline launch config entries unavailable: {error}")
            }
        }
    }
    config
}

/// The per-launch JSON that OpenCode consumes from `OPENCODE_CONFIG_CONTENT`.
fn env_content_config(
    canonical: &Value,
    allow: &[String],
    bridge_tools: Option<&[String]>,
    scope: &McpScope,
) -> Value {
    let projected = effective_mcp_servers(canonical, allow, scope);
    let mut mcp = projected
        .get(&scope.server_map_key)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    // OpenCode merges OPENCODE_CONFIG_CONTENT with project configuration. Put explicit
    // disabled entries over every unselected project server so the merge cannot widen
    // this launch's allowlist.
    if matches!(&scope.injection, ScopeInjection::EnvContent { .. }) {
        if let Some(inherited) = canonical
            .get(&scope.server_map_key)
            .and_then(Value::as_object)
        {
            for id in inherited.keys() {
                if !mcp.contains_key(id) {
                    mcp.insert(id.clone(), json!({"enabled": false}));
                }
            }
        }
    }
    let mut config = serde_json::json!({
        "mcp": Value::Object(mcp),
        "share": "disabled",
        "update": "disable"
    });
    if let Some(bridge_tools) = bridge_tools {
        let prefix = format!("{}_", scope.bridge_id);
        let mut permissions =
            vec![json!({"action": format!("{prefix}*"), "resource": "*", "effect": "deny"})];
        for tool in bridge_tools {
            permissions.push(
                json!({"action": format!("{prefix}{tool}"), "resource": "*", "effect": "allow"}),
            );
        }
        config["permissions"] = json!(permissions);
    }
    config
}

/// Whether a runtime's launch settings share its scope's env-content variable, so the two
/// must be folded into one entry rather than added side by side.
fn merges_into_env_content(runtime: AgentCli) -> bool {
    let descriptor = descriptor(runtime);
    let codesense_pty_core::agent_cli::LaunchSettings::Env(settings_var) =
        &descriptor.launch_settings
    else {
        return false;
    };
    matches!(
        descriptor.mcp_scope.as_ref().map(|scope| &scope.injection),
        Some(ScopeInjection::EnvContent { env_var }) if env_var == settings_var
    )
}

/// Combine a launch's native settings with its MCP projection. A runtime whose launch
/// settings share its scope's env-content variable has them folded into that entry;
/// any other runtime's settings become a second entry.
pub(crate) fn extend_launch_env(
    runtime: AgentCli,
    env: &mut Vec<(String, String)>,
    additional: impl IntoIterator<Item = (String, String)>,
) {
    let merge = merges_into_env_content(runtime);
    for (key, value) in additional {
        if !merge {
            env.push((key, value));
            continue;
        }
        let Some(at) = env.iter().position(|(name, _)| name == &key) else {
            env.push((key, value));
            continue;
        };
        let mut settings: Value =
            serde_json::from_str(&value).expect("generated OpenCode settings");
        let mut scope: Value = serde_json::from_str(&env[at].1).expect("generated OpenCode scope");
        let rules = settings["permissions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(scoped) = scope.as_object_mut().unwrap().remove("permissions") {
            settings["permissions"] = json!(merge_native_permission_rules(
                rules,
                scoped.as_array().cloned().unwrap_or_default(),
            ));
        }
        settings
            .as_object_mut()
            .unwrap()
            .extend(scope.as_object().unwrap().clone());
        env[at].1 = settings.to_string();
    }
}

/// OpenCode resolves a permission with the last matching rule winning, so a rule's position
/// encodes its precedence. A specific `action` must outrank its namespace's wildcard no
/// matter which source (the launch's MCP scope or the agent's defaults) it came from: order
/// the catch-all first, prefix wildcards next and exact actions last, deduplicating on
/// `action` so the two sources' shared wildcard lands once.
fn merge_native_permission_rules(agent: Vec<Value>, scope: Vec<Value>) -> Vec<Value> {
    let tier = |rule: &Value| match rule["action"].as_str() {
        Some("*") => 0,
        Some(action) if action.ends_with('*') => 1,
        _ => 2,
    };
    let mut ordered = agent;
    ordered.extend(scope);
    ordered.sort_by_key(tier);
    let mut merged: Vec<Value> = Vec::new();
    for rule in ordered {
        let action = rule["action"].as_str().unwrap_or_default();
        match merged
            .iter_mut()
            .find(|existing| existing["action"].as_str() == Some(action))
        {
            Some(existing) => *existing = rule,
            None => merged.push(rule),
        }
    }
    merged
}

/// A fresh scoped launch that must own the PTY directly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopedExecution {
    pub argv: Vec<String>,
    pub native_launch: codesense_pty_core::agent_cli::AgentCli,
}

/// The exec of `program` with `args` for a CLI that must own its PTY directly, entered as
/// an interactive launch is (the same normalization the other managed entries plan).
pub(crate) fn scoped_execution(
    runtime: codesense_pty_core::agent_cli::AgentCli,
    program: &Path,
    args: Vec<String>,
) -> Option<ScopedExecution> {
    codesense_pty_core::agent_cli::adapter(runtime)
        .end_on_engine_loss()
        .then(|| ScopedExecution {
            argv: std::iter::once(program.to_string_lossy().into_owned())
                .chain(super::mcp_workspace::interactive_arguments(runtime, args))
                .collect(),
            native_launch: runtime,
        })
}

/// SW-AN-45: what a scoped launch contributes to the child — extra argv (a `config_flag`
/// agent) and/or extra env vars (an `env_dir` agent). Exactly one is populated per the
/// adapter's `injection`; an unscoped (or descriptor-less) launch yields both empty. Serialized
/// (camelCase) to the FE by the interactive-launch command so it can compose both.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeOutcome {
    /// Structured exec selected from adapter policy, after this scope was projected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<ScopedExecution>,
    pub extra_args: Vec<String>,
    /// Exact owned capture overrides to remove before appending extra_args on respawn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remove_args: Option<Vec<String>>,
    #[serde(skip_serializing)]
    pub extra_env: Vec<(String, String)>,
    /// Opaque renderer-safe handle for trusted spawn-time environment. Set by
    /// the interactive command wrapper; internal/headless callers use
    /// `extra_env` directly.
    #[serde(skip_deserializing)]
    pub environment_binding: Option<String>,
    /// Public projected names; credentials and native configurations stay local.
    #[serde(default)]
    pub server_aliases: std::collections::BTreeMap<String, String>,
}

/// Codex's --no-daemon is a root option, while its -c values must follow
/// profile configuration to retain scope precedence. Other providers are unchanged.
pub(crate) fn compose_args(mut launch: Vec<String>, mut scope: Vec<String>) -> Vec<String> {
    if scope.iter().any(|arg| arg == "--no-daemon") {
        launch.retain(|arg| arg != "--no-daemon");
        scope.retain(|arg| arg != "--no-daemon");
        launch.insert(0, "--no-daemon".into());
    }
    launch.extend(scope);
    launch
}

/// SW-AN-45: the ONE capability-gated scope seam shared by every launch path (dispatch PTY,
/// headless capture, interactive FE). Projects the scoped config **only** when the resolving
/// adapter declares an `mcp_scope`, then returns the launch additions in the adapter-declared
/// shape:
/// - `config_flag` → `extra_args` (rendered flags pointing the agent at the config file);
/// - `env_dir` → `extra_env` (the ephemeral per-session dir under the agent's home env var,
///   with credential passthrough brought in beside the config).
///
/// `None` (the resolving adapter declares no mechanism) → empty outcome (launch unfiltered) with
/// a breadcrumb — never a foreign agent's flags. `quote_path` shapes the config path for the
/// caller's launch builder before it goes into `config_flag` flags; it never touches env
/// values (set literally on the child). Collapses SW-AN-43's two-call `project_scope_path` +
/// `capability_scope_flags` dance into one seam.
#[cfg(test)]
pub fn project_scope(
    session_id: &str,
    cwd: &Path,
    allow: &[String],
    scope: Option<&McpScope>,
    quote_path: impl Fn(&str) -> String,
) -> Result<ScopeOutcome, String> {
    project_scope_with_home(session_id, cwd, allow, scope, None, quote_path)
}

/// Account-aware scope projection. `source_home` is a trusted,
/// backend-resolved provider home used only as the credential-copy source.
#[cfg(test)]
pub fn project_scope_with_home(
    session_id: &str,
    cwd: &Path,
    allow: &[String],
    scope: Option<&McpScope>,
    source_home: Option<&Path>,
    quote_path: impl Fn(&str) -> String,
) -> Result<ScopeOutcome, String> {
    project_scope_with_creation(session_id, cwd, allow, scope, source_home, quote_path)
        .map(|(outcome, _)| outcome)
}

#[cfg(test)]
pub(crate) fn project_scope_with_creation(
    session_id: &str,
    cwd: &Path,
    allow: &[String],
    scope: Option<&McpScope>,
    source_home: Option<&Path>,
    quote_path: impl Fn(&str) -> String,
) -> Result<(ScopeOutcome, Option<ScopeCreation>), String> {
    project_scope_for_cli(
        session_id,
        cwd,
        allow,
        None,
        scope,
        source_home,
        None,
        None,
        quote_path,
    )
}

/// The exact CLI a launch runs: its executable and the args it runs with. A projection
/// that asks the CLI itself what it has registered (its descriptor's MCP inventory) runs
/// this executable with these args' config selectors.
#[derive(Clone)]
pub(crate) struct LaunchCli<'a> {
    pub runtime: Option<AgentCli>,
    pub binary: &'a Path,
    pub args: &'a [String],
    /// The bridge the projection registers, in place of the installed one (tests).
    bridge: Option<&'a Path>,
    /// The release this executable reported when it was resolved, if it did.
    pub version: Option<&'a str>,
    /// Whether the selected terminal host trusts injected Codex checkpoint hooks.
    pub attribution_hook_trust: bool,
}

impl<'a> LaunchCli<'a> {
    pub(crate) fn new(binary: &'a Path, args: &'a [String]) -> Self {
        Self {
            runtime: None,
            binary,
            args,
            bridge: None,
            version: None,
            // Direct/headless launches run this desktop's current core. Interactive
            // app callers replace this with the selected terminal host's capability.
            attribution_hook_trust: true,
        }
    }

    pub(crate) fn with_attribution_hook_trust(mut self, supported: bool) -> Self {
        self.attribution_hook_trust = supported;
        self
    }

    pub(crate) fn with_version(mut self, version: Option<&'a str>) -> Self {
        self.version = version;
        self
    }
}

/// `canonical_base` (SW-AN-52): the content-layer MCP config resolved upstream
/// for an agent launch, deep-merged under the cwd `.mcp.json` to produce the
/// final canonical config. `None` keeps the pre-SW-AN-52 cwd-only behavior
/// for unnamed-agent launch profiles and authoring tools.
#[allow(clippy::too_many_arguments)]
pub(crate) fn project_scope_for_cli(
    session_id: &str,
    cwd: &Path,
    allow: &[String],
    bridge_tools: Option<&[String]>,
    scope: Option<&McpScope>,
    source_home: Option<&Path>,
    canonical_base: Option<&Value>,
    cli: Option<&LaunchCli<'_>>,
    quote_path: impl Fn(&str) -> String,
) -> Result<(ScopeOutcome, Option<ScopeCreation>), String> {
    let Some(scope) = scope else {
        warn!(
            target: "mcp_scope",
            "MCP scoping requested but adapter declares no scope mechanism — launching unfiltered"
        );
        return Ok((ScopeOutcome::default(), None));
    };
    if let Some(listed) = &scope.inventory {
        let cli = cli.ok_or("a scoped launch of this runtime requires its exact selected executable; retry through the managed agent launcher")?;
        return inventory::project(
            session_id,
            cwd,
            allow,
            bridge_tools,
            canonical_base,
            scope,
            listed,
            source_home,
            cli,
            &quote_path,
        );
    }
    if let ScopeInjection::EnvContent { env_var } = &scope.injection {
        let canonical = canonical_from_worktree(
            cwd,
            canonical_base,
            scope,
            crate::commands::deps::bridge_symlink_path().as_deref(),
            Some(session_id),
        )?;
        let content = with_adapter_entries(
            env_content_config(&canonical, allow, bridge_tools, scope),
            scope,
            cli.and_then(|cli| cli.version),
        );
        return Ok((
            ScopeOutcome {
                execution: None,
                extra_args: Vec::new(),
                extra_env: vec![(env_var.clone(), content.to_string())],
                environment_binding: None,
                remove_args: None,
                server_aliases: Default::default(),
            },
            None,
        ));
    }
    let (config_path, creation) =
        project_from_worktree_with_creation(session_id, cwd, allow, canonical_base, scope)?;
    let outcome: Result<ScopeOutcome, String> = match &scope.injection {
        ScopeInjection::ConfigFlag { flag_template, .. } => {
            let path = quote_path(&config_path.to_string_lossy());
            Ok(ScopeOutcome {
                execution: None,
                extra_args: render_scope_flags(flag_template, &path),
                extra_env: Vec::new(),
                environment_binding: None,
                remove_args: None,
                server_aliases: Default::default(),
            })
        }
        ScopeInjection::EnvDir {
            env_var,
            carry_home,
            passthrough,
            default_home,
        } => {
            // The scoped config lives in a per-session dir; export THAT dir under the agent's
            // home env var and bring declared credentials in beside it so the relocated home
            // still authenticates.
            let dir = config_path
                .parent()
                .ok_or_else(|| "scoped config path has no parent dir".to_string())?
                .to_path_buf();
            let preparation = (|| {
                if *carry_home {
                    let real_home = agent_real_home(env_var, default_home, source_home);
                    carry_home_config(&config_path, &real_home, scope)?;
                    apply_passthrough(&dir, passthrough, &real_home)?;
                }
                if let Some(runtime) = cli.and_then(|cli| cli.runtime) {
                    let desktop = crate::agents::adapter::desktop(runtime);
                    let qualified = cli
                        .and_then(|cli| cli.version)
                        .is_none_or(|v| descriptor(runtime).release.version_qualified(v));
                    let entries = (desktop.inline_config)(qualified)?;
                    if !serde_json::Map::is_empty(&entries) || desktop.home_config.is_some() {
                        let format = ConfigFormat::from(scope.format);
                        let mut config = format
                            .parse(&std::fs::read(&config_path).map_err(|e| e.to_string())?)
                            .map_err(|e| e.to_string())?;
                        config
                            .as_object_mut()
                            .ok_or("scoped config must be a table")?
                            .extend(entries);
                        if desktop.home_config.is_some() {
                            if let Some(tools) = bridge_tools {
                                config[&scope.server_map_key][&scope.bridge_id]["tools"] =
                                    serde_json::json!({"include": tools});
                            }
                        }
                        let staged = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
                        write_0600_exclusive(
                            &staged,
                            &format.serialize(&config).map_err(|e| e.to_string())?,
                        )?;
                        std::fs::rename(&staged, &config_path).map_err(|e| e.to_string())?;
                    }
                }
                Ok::<(), String>(())
            })();
            if let Err(error) = preparation {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(error);
            }
            Ok(ScopeOutcome {
                execution: None,
                extra_args: Vec::new(),
                extra_env: vec![(env_var.clone(), dir.to_string_lossy().into_owned())],
                environment_binding: None,
                remove_args: None,
                server_aliases: Default::default(),
            })
        }
        ScopeInjection::EnvContent { .. } => unreachable!("handled before file projection"),
    };
    Ok((outcome?, Some(creation)))
}

/// The file name of a profile launch's per-launch settings, inside the session's scope dir.
const LAUNCH_SETTINGS_FILE: &str = "launch-settings.json";

/// The per-session config of a launch that runs a launch profile rather than a named agent:
/// everything CodeSense adds for the session, none of it written into `cwd`.
///
/// - With an allowlist (`allow` is `Some`), the scoped projection every scoped launch gets
///   ([`project_scope_with_creation`]).
/// - Without one, a `config_flag` agent that declares an `open_flag_template` gets a config
///   holding only the bridge, added on top of its own servers. An `env_dir` agent adds
///   nothing here: its profile args already carry the adapter's bridge override.
/// - A `config_flag` agent that declares a `settings_flag_template` also gets a per-launch
///   settings file beside that config: CodeSense's status hooks for every lifecycle event
///   the folder's own settings do not already run them on, and, for a `headless` launch,
///   the CLI's unattended-safe settings (Claude: `dontAsk`), so a born-minimized run
///   auto-denies a tool outside its allow set instead of parking on a prompt nobody sees.
///
/// Both files live in the session's scope dir and go with it.
#[cfg(test)]
pub(crate) fn project_profile_launch(
    session_id: &str,
    cwd: &Path,
    allow: Option<&[String]>,
    runtime: AgentCli,
    source_home: Option<&Path>,
    launch: ProfileLaunch,
    quote_path: impl Fn(&str) -> String,
) -> Result<(ScopeOutcome, Option<ScopeCreation>), String> {
    let fixture = inventory::Fixture::new();
    let cli = fixture.launch(&[]);
    project_profile_launch_for_cli(
        session_id,
        cwd,
        allow,
        runtime,
        source_home,
        launch,
        Some(&cli),
        quote_path,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn project_profile_launch_for_cli(
    session_id: &str,
    cwd: &Path,
    allow: Option<&[String]>,
    runtime: AgentCli,
    source_home: Option<&Path>,
    launch: ProfileLaunch,
    cli: Option<&LaunchCli<'_>>,
    quote_path: impl Fn(&str) -> String,
) -> Result<(ScopeOutcome, Option<ScopeCreation>), String> {
    if let Some(prerequisite) = crate::agents::adapter::desktop(runtime).model_endpoint {
        return Err((prerequisite.missing_message)());
    }
    let Some(scope) = descriptor(runtime).mcp_scope.as_ref() else {
        return Ok((ScopeOutcome::default(), None));
    };
    let headless = launch.headless;
    let (mut outcome, creation) = match allow {
        // Launch-profile launches have no agent, so no content-layer MCP base to
        // thread in — cwd-only unchanged (SW-AN-52).
        Some(allow) => project_scope_for_cli(
            session_id,
            cwd,
            allow,
            None,
            Some(scope),
            source_home,
            None,
            cli,
            &quote_path,
        )?,
        None => {
            let mut open = project_open_scope(
                session_id,
                scope,
                cli.and_then(|cli| cli.version),
                &quote_path,
            )?;
            if launch.bridge_override {
                open.0
                    .extra_args
                    .extend(bridge_override_flags(scope, &quote_path));
            }
            open
        }
    };
    let settings_template = match &scope.injection {
        ScopeInjection::ConfigFlag {
            settings_flag_template: Some(template),
            ..
        } if creation.is_some() => template,
        _ => {
            if let codesense_pty_core::agent_cli::LaunchSettings::Env(env_var) =
                &descriptor(runtime).launch_settings
            {
                let settings = profile_launch_settings(cwd, headless, runtime);
                if settings
                    .as_object()
                    .is_some_and(|settings| !settings.is_empty())
                {
                    extend_launch_env(
                        runtime,
                        &mut outcome.extra_env,
                        [(env_var.clone(), settings.to_string())],
                    );
                }
            }
            return Ok((outcome, creation));
        }
    };
    match write_launch_settings(session_id, cwd, headless, runtime) {
        Ok(path) => {
            outcome.extra_args.extend(render_scope_flags(
                settings_template,
                &quote_path(&path.to_string_lossy()),
            ));
            Ok((outcome, creation))
        }
        Err(error) => {
            remove_scope_config(session_id);
            Err(error)
        }
    }
}

/// How a profile launch runs, for [`project_profile_launch`].
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ProfileLaunch {
    /// A headless run: its settings carry the unattended-safe approval mode.
    pub headless: bool,
    /// The launch runs the launcher's default args, not a resolved profile's (which
    /// already carry the adapter's bridge override): add the override, so an agent that
    /// takes the bridge as launch flags (Codex) reaches it.
    pub bridge_override: bool,
}

/// The adapter's bridge override flags with this app's bridge path, if it declares one.
/// For a CLI that scrubs a server's environment (Codex), the flags also pass the tab
/// session variable through (`-c mcp_servers.<id>.env_vars=[…]`), quoted by
/// `quote_path` like the other launch words that hold shell metacharacters.
fn bridge_override_flags(scope: &McpScope, quote_path: impl Fn(&str) -> String) -> Vec<String> {
    let mut flags = scope
        .bridge_override
        .as_ref()
        .zip(crate::commands::deps::bridge_symlink_path())
        .and_then(|(over, bridge)| over.render(&bridge))
        .unwrap_or_default();
    if !flags.is_empty() && passes_env_explicitly(scope) {
        flags.extend(bridge_env_override(scope, quote_path));
    }
    flags
}

/// `-c <map>.<bridge>.env_vars=["CODESENSE_TAB_SESSION_ID"]`, its value word quoted.
fn bridge_env_override(scope: &McpScope, quote_path: impl Fn(&str) -> String) -> [String; 2] {
    [
        "-c".to_string(),
        quote_path(&format!(
            "{}.{}.env_vars=[\"{TAB_SESSION_ENV}\"]",
            scope.server_map_key, scope.bridge_id
        )),
    ]
}

/// The session config of a `config_flag` launch with no allowlist: the bridge alone, passed
/// with the descriptor's `open_flag_template` so the agent keeps its own servers. Nothing
/// for an agent that declares no open template or scopes by its home dir.
fn project_open_scope(
    session_id: &str,
    scope: &McpScope,
    version: Option<&str>,
    quote_path: impl Fn(&str) -> String,
) -> Result<(ScopeOutcome, Option<ScopeCreation>), String> {
    if let ScopeInjection::EnvContent { env_var } = &scope.injection {
        let mut canonical = Value::Object(Default::default());
        with_bridge_server(
            &mut canonical,
            scope,
            crate::commands::deps::bridge_symlink_path().as_deref(),
            Some(session_id),
        );
        let config = with_adapter_entries(
            env_content_config(&canonical, &[], None, scope),
            scope,
            version,
        );
        return Ok((
            ScopeOutcome {
                execution: None,
                extra_args: Vec::new(),
                extra_env: vec![(env_var.clone(), config.to_string())],
                environment_binding: None,
                remove_args: None,
                server_aliases: Default::default(),
            },
            None,
        ));
    }
    let ScopeInjection::ConfigFlag {
        open_flag_template: Some(template),
        ..
    } = &scope.injection
    else {
        return Ok((ScopeOutcome::default(), None));
    };
    let mut config = Value::Object(Default::default());
    with_bridge_server(
        &mut config,
        scope,
        crate::commands::deps::bridge_symlink_path().as_deref(),
        Some(session_id),
    );
    let (path, creation) = write_scope_config_with_creation(session_id, &config, &[], scope)?;
    let outcome = ScopeOutcome {
        execution: None,
        extra_args: render_scope_flags(template, &quote_path(&path.to_string_lossy())),
        extra_env: Vec::new(),
        environment_binding: None,
        remove_args: None,
        server_aliases: Default::default(),
    };
    Ok((outcome, Some(creation)))
}

/// The per-launch settings body for a profile launch in `cwd`: the status hooks the
/// folder does not already wire (so they run exactly once), plus, headless, the CLI's
/// unattended-safe settings.
pub(crate) fn profile_launch_settings(cwd: &Path, headless: bool, runtime: AgentCli) -> Value {
    let adapter = crate::agents::adapter::desktop(runtime);
    let mut settings = if headless {
        (adapter.headless_settings)()
    } else {
        serde_json::json!({})
    };
    if headless {
        if let Some(rules) = adapter.native_permission_rules {
            settings = rules(&settings);
        }
    }
    crate::commands::content_engine::merge_launch_notify_hooks(&mut settings, cwd, runtime);
    settings
}

fn write_launch_settings(
    session_id: &str,
    cwd: &Path,
    headless: bool,
    runtime: AgentCli,
) -> Result<PathBuf, String> {
    let bytes = serde_json::to_vec_pretty(&profile_launch_settings(cwd, headless, runtime))
        .map_err(|e| format!("could not serialize launch settings: {e}"))?;
    let path = mcp_scope_session_dir(session_id).join(LAUNCH_SETTINGS_FILE);
    write_0600_exclusive(&path, &bytes)?;
    Ok(path)
}

/// The agent's real home an `env_dir` launch relocates away from: the account's home when
/// the launch runs as a managed account, else the env var's CURRENT value (before the launch
/// overrides it), else the descriptor's `default_home`.
fn agent_real_home(env_var: &str, default_home: &str, source_home: Option<&Path>) -> PathBuf {
    source_home.map(Path::to_path_buf).unwrap_or_else(|| {
        std::env::var(env_var)
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| expand_home(default_home))
    })
}

/// Carry the agent's own config from its real home into the relocated one, so a scoped
/// launch keeps the user's model, profiles and project trust: the real home's config file
/// (same name as the scoped one) minus its server map, with the scoped config's keys (the
/// projected server map) laid over it. The server map stays the projection's alone, so the
/// scope never widens. A missing file adds nothing; an unreadable or unparseable one is
/// skipped with a breadcrumb rather than failing the launch.
fn carry_home_config(config_path: &Path, real_home: &Path, scope: &McpScope) -> Result<(), String> {
    let Some(filename) = config_path.file_name() else {
        return Ok(());
    };
    let source = real_home.join(filename);
    let bytes = match std::fs::read(&source) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            warn!(target: "mcp_scope", "could not read {} to carry into the scoped home: {e}", source.display());
            return Ok(());
        }
    };
    let format = ConfigFormat::from(scope.format);
    let mut carried = match format.parse(&bytes) {
        Ok(Value::Object(map)) => map,
        Ok(_) | Err(_) => {
            warn!(target: "mcp_scope", "{} is not a config table — not carried into the scoped home", source.display());
            return Ok(());
        }
    };
    carried.remove(&scope.server_map_key);
    let scoped = std::fs::read(config_path)
        .map_err(|e| format!("could not read {}: {e}", config_path.display()))
        .and_then(|bytes| {
            format
                .parse(&bytes)
                .map_err(|e| format!("could not parse {}: {e}", config_path.display()))
        })?;
    if let Value::Object(scoped) = scoped {
        carried.extend(scoped);
    }
    let merged = format
        .serialize(&Value::Object(carried))
        .map_err(|e| format!("could not serialize {}: {e}", config_path.display()))?;
    let dir = config_path
        .parent()
        .ok_or_else(|| "scoped config path has no parent dir".to_string())?;
    let staged = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    write_0600_exclusive(&staged, &merged)?;
    std::fs::rename(&staged, config_path).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("could not replace {}: {e}", config_path.display())
    })
}

/// Bring each declared `passthrough` file from the agent's real home into the ephemeral
/// scope dir (so relocating the home via the env var doesn't strand credentials). The real
/// home is the env var's CURRENT value (before we override it) or `default_home`. An absent
/// source is skipped with a breadcrumb — the agent may authenticate another way (e.g. an API
/// key env var), so a missing file must not fail the launch.
fn apply_passthrough(
    dir: &Path,
    passthrough: &[Passthrough],
    real_home: &Path,
) -> Result<(), String> {
    for pt in passthrough {
        let src = real_home.join(&pt.name);
        let dst = dir.join(&pt.name);
        // `is_file` (follows symlinks) instead of `exists`: skip an absent source, a DANGLING
        // symlink, AND a directory — none is a credential file, and a dir source would EISDIR
        // the copy path / symlink a dir. Never fail the launch (the agent may auth another way).
        if !src.is_file() {
            warn!(
                target: "mcp_scope",
                "passthrough source {} is absent or not a regular file — skipping (the agent may authenticate another way)",
                src.display()
            );
            continue;
        }
        match pt.mode {
            PassthroughMode::Symlink => bring_in_symlink(&src, &dst)?,
            PassthroughMode::Copy => {
                if pt.name != "auth.json" {
                    return Err("credential copy fallback permits only auth.json".into());
                }
                bring_in_copy(&src, &dst)?;
                write_copyback_journal(dir, &src, &dst)?;
            }
        }
    }
    Ok(())
}

/// Symlink a passthrough source into the scope dir (no secret copied). Unix only.
#[cfg(unix)]
fn bring_in_symlink(src: &Path, dst: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(src, dst)
        .map_err(|e| format!("failed to symlink passthrough {}: {e}", dst.display()))
}

/// Non-unix fallback: symlinks need privileges on Windows, so copy instead.
#[cfg(not(unix))]
fn bring_in_symlink(src: &Path, dst: &Path) -> Result<(), String> {
    bring_in_copy(src, dst)
}

/// Copy a passthrough source into the scope dir, 0600 (it may carry credentials).
fn bring_in_copy(src: &Path, dst: &Path) -> Result<(), String> {
    let bytes = std::fs::read(src)
        .map_err(|e| format!("failed to read passthrough {}: {e}", src.display()))?;
    write_0600_exclusive(dst, &bytes)
}

fn write_copyback_journal(dir: &Path, source: &Path, candidate: &Path) -> Result<(), String> {
    let bytes =
        std::fs::read(source).map_err(|e| format!("failed to snapshot credential source: {e}"))?;
    let metadata =
        std::fs::metadata(source).map_err(|e| format!("failed to stat credential source: {e}"))?;
    let journal = CredentialCopyJournal {
        source: source.to_path_buf(),
        candidate: candidate.to_path_buf(),
        original_hash: blake3::hash(&bytes).to_hex().to_string(),
        original_len: metadata.len(),
        #[cfg(unix)]
        original_device: {
            use std::os::unix::fs::MetadataExt;
            metadata.dev()
        },
        #[cfg(unix)]
        original_inode: {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        },
    };
    let encoded = serde_json::to_vec(&journal).map_err(|e| e.to_string())?;
    write_0600_exclusive(&dir.join(CREDENTIAL_JOURNAL), &encoded)
}

fn finish_credential_copyback(dir: &Path) -> Result<(), String> {
    let journal_path = dir.join(CREDENTIAL_JOURNAL);
    let bytes = match std::fs::read(&journal_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("failed to read credential journal: {error}")),
    };
    let journal: CredentialCopyJournal =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid credential journal: {e}"))?;
    let candidate = std::fs::read(&journal.candidate)
        .map_err(|e| format!("failed to read credential candidate: {e}"))?;
    if blake3::hash(&candidate).to_hex().as_str() == journal.original_hash {
        return Ok(());
    }
    let current = std::fs::read(&journal.source).ok();
    let metadata = std::fs::metadata(&journal.source).ok();
    let identity_matches = current
        .as_ref()
        .is_some_and(|current| blake3::hash(current).to_hex().as_str() == journal.original_hash)
        && metadata.as_ref().is_some_and(|metadata| {
            metadata.len() == journal.original_len && {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    metadata.dev() == journal.original_device
                        && metadata.ino() == journal.original_inode
                }
                #[cfg(not(unix))]
                {
                    true
                }
            }
        });
    if identity_matches {
        crate::content_engine::write_tmp_fsync_rename(&journal.source, &candidate)
            .map_err(|e| format!("failed to commit refreshed credential: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&journal.source, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("failed to secure refreshed credential: {e}"))?;
        }
        return Ok(());
    }
    let recovery_dir = journal
        .source
        .parent()
        .ok_or("credential authority has no parent")?
        .join(".codesense-credential-recovery");
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&recovery_dir)
            .map_err(|e| format!("failed to create credential recovery directory: {e}"))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&recovery_dir)
        .map_err(|e| format!("failed to create credential recovery directory: {e}"))?;
    let recovery = recovery_dir.join(format!("{}.auth.json", crate::http::random_hex_secret()));
    write_0600_exclusive(&recovery, &candidate)?;
    // Recovery is now durably owner-only; retire the pending journal so a
    // restart cannot manufacture duplicate recovery candidates.
    std::fs::remove_file(&journal_path)
        .map_err(|e| format!("failed to retire recovered credential journal: {e}"))?;
    Err("credential_write_conflict: candidate preserved for recovery".into())
}

/// Expand ONLY the `~` and `~/…` forms of an adapter-declared `default_home` to the user's home
/// dir (via `dirs::home_dir`, the crate's established home-dir source — cross-platform). The
/// `~user` form is unsupported and returned verbatim, so it can't mis-join onto our own home
/// (`~bob/.codex` must not become `$HOME/bob/.codex`).
fn expand_home(path: &str) -> PathBuf {
    let rest = match path.strip_prefix('~') {
        Some(r) if r.is_empty() || r.starts_with('/') => r,
        _ => return PathBuf::from(path),
    };
    match dirs::home_dir() {
        Some(home) if rest.is_empty() => home,
        Some(home) => home.join(rest.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

/// Newly created scopes belong to this instance's state root.
fn scope_root() -> Option<PathBuf> {
    super::paths::codesense_home_dir().map(|home| home.join("mcp-scope"))
}

fn legacy_scope_root() -> PathBuf {
    std::env::temp_dir().join(LEGACY_SCOPE_DIR)
}

fn scope_roots() -> impl Iterator<Item = PathBuf> {
    scope_root()
        .into_iter()
        .chain(std::iter::once(legacy_scope_root()))
}

/// Resolve the session's scope without moving files a running CLI still reads.
/// Prefer instance state, falling back to a legacy folder only when it is absent.
pub fn mcp_scope_session_dir(session_id: &str) -> PathBuf {
    let legacy = legacy_scope_root().join(session_id);
    match scope_root().map(|root| root.join(session_id)) {
        Some(owned) if owned.exists() || !legacy.exists() => owned,
        _ => legacy,
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ScopeOwner {
    instance_root: PathBuf,
    server_id: String,
}

/// Project the effective set for `allow` over `canonical` and write it (0600, atomic
/// exclusive create) into the session's ephemeral dir (0700), named by the descriptor's
/// config filename (its last segment — the canonical may be a subpath like
/// `.codex/config.toml`, but an `env_dir` agent looks for `$HOME/config.toml`). Returns the
/// file path; its parent dir is the `env_dir` home. Serializes in the descriptor's `format`.
#[cfg(test)]
pub fn write_scope_config(
    session_id: &str,
    canonical: &Value,
    allow: &[String],
    scope: &McpScope,
) -> Result<PathBuf, String> {
    write_scope_config_with_creation(session_id, canonical, allow, scope).map(|(path, _)| path)
}

fn write_scope_config_with_creation(
    session_id: &str,
    canonical: &Value,
    allow: &[String],
    scope: &McpScope,
) -> Result<(PathBuf, ScopeCreation), String> {
    let effective = effective_mcp_servers(canonical, allow, scope);
    let bytes = ConfigFormat::from(scope.format)
        .serialize(&effective)
        .map_err(|e| format!("failed to serialize scoped mcp config: {e}"))?;
    write_scope_config_bytes(session_id, scope, &bytes, write_0600_exclusive)
}

struct ScopeWriteGuard {
    dir: PathBuf,
    committed: bool,
}

impl Drop for ScopeWriteGuard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn write_scope_config_bytes(
    session_id: &str,
    scope: &McpScope,
    bytes: &[u8],
    writer: impl FnOnce(&Path, &[u8]) -> Result<(), String>,
) -> Result<(PathBuf, ScopeCreation), String> {
    let slot = scope_slot(session_id);
    let mut owned = slot.lock().unwrap_or_else(|e| e.into_inner());
    let root = scope_root().ok_or("home directory unavailable for MCP scope")?;
    let home = root.parent().ok_or("MCP scope root has no parent")?;
    super::paths::ensure_private_dir(home)?;
    super::paths::ensure_private_dir(&root)?;
    let owner = ScopeOwner {
        instance_root: home.canonicalize().map_err(|e| e.to_string())?,
        server_id: std::fs::read_to_string(home.join("pty-server-id"))
            .unwrap_or_default()
            .trim()
            .to_string(),
    };
    let dir = root.join(session_id);
    // The final session directory is created exclusively. That makes the guard
    // below its sole owner, so rollback can never erase a pre-existing session.
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("failed to create mcp-scope dir {}: {e}", dir.display()))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(&dir)
        .map_err(|e| format!("failed to create mcp-scope dir {}: {e}", dir.display()))?;
    let mut guard = ScopeWriteGuard {
        dir: dir.clone(),
        committed: false,
    };
    write_0600_exclusive(
        &dir.join("owner.json"),
        &serde_json::to_vec(&owner).map_err(|e| e.to_string())?,
    )?;
    let identity = scope_identity(&dir)?;
    let filename = Path::new(&scope.config_filename)
        .file_name()
        .ok_or_else(|| {
            format!(
                "mcp_scope.config_filename {:?} has no file name",
                scope.config_filename
            )
        })?;
    let path = dir.join(filename);
    writer(&path, bytes)?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let stamp = NEXT
        .fetch_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |value| value.checked_add(1),
        )
        .map_err(|_| "scope creation stamp exhausted")?
        + 1;
    let creation = ScopeCreation {
        session: session_id.into(),
        stamp,
    };
    owned.relay_identity = std::sync::Weak::new();
    owned.relay_root = None;
    owned.record = Some(ScopeRecord {
        creation: creation.clone(),
        identity,
        owner: None,
        relay_enabled: false,
        relay_retired: false,
        prepared_at: std::time::Instant::now(),
        detached_lease: None,
    });
    guard.committed = true;
    Ok((path, creation))
}

/// Read the worktree's canonical `.mcp.json` and project the scoped config for a
/// session — the whole launch-time step in one call. Fails (rather than silently
/// launching unscoped, which would over-expose tools) if the canonical file can't be
/// read or parsed. Reused by the dispatch spawn spine (SW-AN-41) and SW-AN-42's
/// interactive-launch command, so the read + projection live in exactly one place.
#[cfg(test)]
pub fn project_from_worktree(
    session_id: &str,
    cwd: &Path,
    allow: &[String],
    scope: &McpScope,
) -> Result<PathBuf, String> {
    project_from_worktree_with_creation(session_id, cwd, allow, None, scope).map(|(path, _)| path)
}

/// SW-AN-52 test shim: project with a content-layer base and the cwd config,
/// returning just the written path.
#[cfg(test)]
pub fn project_from_worktree_with_base(
    session_id: &str,
    cwd: &Path,
    allow: &[String],
    canonical_base: &Value,
    scope: &McpScope,
) -> Result<PathBuf, String> {
    project_from_worktree_with_creation(session_id, cwd, allow, Some(canonical_base), scope)
        .map(|(path, _)| path)
}

/// SW-AN-52: build the session's canonical config and project it. When
/// `canonical_base` is `Some`, the content-layer config resolved upstream is
/// the base — the cwd `.mcp.json`, if any, deep-merges on top (cwd nearer than
/// any content layer). When `None`, the launch is cwd-only (unnamed-agent
/// launch profiles, authoring tools): the pre-SW-AN-52 behavior.
fn project_from_worktree_with_creation(
    session_id: &str,
    cwd: &Path,
    allow: &[String],
    canonical_base: Option<&Value>,
    scope: &McpScope,
) -> Result<(PathBuf, ScopeCreation), String> {
    let canonical = canonical_from_worktree(
        cwd,
        canonical_base,
        scope,
        crate::commands::deps::bridge_symlink_path().as_deref(),
        Some(session_id),
    )?;
    write_scope_config_with_creation(session_id, &canonical, allow, scope)
}

fn canonical_from_worktree(
    cwd: &Path,
    canonical_base: Option<&Value>,
    scope: &McpScope,
    bridge: Option<&Path>,
    session_id: Option<&str>,
) -> Result<Value, String> {
    let canonical_path = cwd.join(&scope.config_filename);
    let cwd_value: Option<Value> = match std::fs::read(&canonical_path) {
        Ok(bytes) => Some(
            ConfigFormat::from(scope.format)
                .parse(&bytes)
                .map_err(|e| {
                    format!(
                        "failed to parse {} for MCP scoping ({}): {e}",
                        scope.config_filename,
                        canonical_path.display()
                    )
                })?,
        ),
        // A folder with no project config of its own (a context repo's content dir, say)
        // is never written to: the session's scoped config carries the bridge alone.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(format!(
                "failed to read {} for MCP scoping ({}): {e}",
                scope.config_filename,
                canonical_path.display()
            ))
        }
    };
    let mut canonical = canonical_base
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    if let Some(cwd) = cwd_value {
        crate::shared::json_io::deep_merge(&mut canonical, &cwd);
    }
    with_bridge_server(&mut canonical, scope, bridge, session_id);
    Ok(canonical)
}

/// Put CodeSense's own bridge into `canonical`, the session's copy of the folder's
/// config, so a scoped launch reaches CodeSense without CodeSense writing the bridge into
/// the folder. The entry is always this app's resolved bridge: a stale `codesense` entry
/// the folder carries is replaced in the session's copy and left as is in the folder.
pub(crate) fn with_bridge_server(
    canonical: &mut Value,
    scope: &McpScope,
    bridge: Option<&Path>,
    session_id: Option<&str>,
) {
    let Some(bridge) = bridge.and_then(Path::to_str) else {
        return;
    };
    let Some(root) = canonical.as_object_mut() else {
        return;
    };
    let Some(servers) = root
        .entry(scope.server_map_key.clone())
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
    else {
        return;
    };
    let mut entry = scope.bridge_entry.render(bridge, session_id);
    if let (Some(key), Some(session)) = (scope.bridge_entry.explicit_environment_key(), session_id)
    {
        entry[key] = serde_json::json!({
            "CODESENSE_MCP_SOCK": crate::mcp::mcp_socket_path(),
            "CODESENSE_TAB_SESSION_ID": session,
        });
    }
    servers.insert(scope.bridge_id.clone(), entry);
}

/// The variable the bridge reads the calling tab's session from (`codesense-bridge`).
pub(crate) const TAB_SESSION_ENV: &str = "CODESENSE_TAB_SESSION_ID";

/// Whether this CLI hands a stdio MCP server only the variables its config names (Codex,
/// whose TOML server entries take `env_vars`), so the bridge's tab session must be named.
fn passes_env_explicitly(scope: &McpScope) -> bool {
    matches!(scope.injection, ScopeInjection::EnvContent { .. })
        || matches!(
            scope.format,
            codesense_pty_core::agent_cli::ConfigFragmentFormat::Toml
        )
}

/// Create `path` fresh with mode 0600 and write `bytes`. Uses `create_new`
/// (`O_CREAT|O_EXCL`) so it never follows/overwrites an existing file or symlink — a
/// resolved-secrets file must not land on an attacker-planted target (CWE-377/378).
/// Session ids are minted unique (`mint_session_id`), so a collision here is impossible;
/// we deliberately do NOT pre-`remove` (that would reopen a symlink-swap window between
/// remove and create) — an `AlreadyExists` surfaces as a real error instead.
fn write_0600_exclusive(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| format!("failed to create scoped mcp config {}: {e}", path.display()))?;
    f.write_all(bytes)
        .map_err(|e| format!("failed to write scoped mcp config {}: {e}", path.display()))
}

/// Best-effort removal of the ephemeral scope dir for `session_id`, called on session
/// teardown. A missing dir (unscoped session, already reaped) is a silent no-op.
pub fn remove_scope_config(session_id: &str) {
    let slot = scope_slot(session_id);
    let mut record = slot.lock().unwrap_or_else(|e| e.into_inner());
    if record.as_ref().is_some_and(|record| record.owner.is_some()) {
        return;
    }
    if let Some(record) = record.as_mut() {
        record.relay_retired = true;
    }
    record.relay_identity = std::sync::Weak::new();
    record.relay_root = None;
    let dir = mcp_scope_session_dir(session_id);
    crate::agents::adapter::teardown_managed_home(&dir);
    if let Err(error) = finish_credential_copyback(&dir) {
        log::warn!(target: "mcp_scope", "{error}; retaining scoped credential journal");
        return;
    }
    // A scoped Codex home holds the session's rollout; store the conversation before it goes.
    if !crate::session_conversation::capture_before_removal(session_id, &dir) {
        return;
    }
    if std::fs::remove_dir_all(dir).is_ok() {
        record.clear();
    }
}

/// Remove a session scope without blocking an async worker on final conversation capture.
pub async fn remove_scope_config_async(session_id: &str) {
    let session = session_id.to_owned();
    let _ = super::await_blocking("mcp_scope.remove", move || {
        remove_scope_config(&session);
        Ok::<(), String>(())
    })
    .await;
}

/// Best-effort sweep of the scope dir at app startup — reaps per-session dirs left by a
/// previous run that crashed or closed before teardown could remove them (they carry
/// resolved server `env` secrets and credentials, so they must not linger). A dir is
/// reaped only when it is older than [`STALE_AGE`] AND `alive` says its session is not
/// running: a session the session server kept alive across the restart still reads its
/// `--mcp-config`, `--settings` file and scoped home. When `alive` cannot tell (`None`),
/// nothing is removed. An unreadable entry, owner or mtime is left alone, as is a foreign
/// owner. Only the instance root is scanned; legacy folders are never swept.
pub fn sweep_stale_configs(
    alive: impl FnOnce(&[&str]) -> Option<std::collections::HashSet<String>>,
) {
    let Some(root) = scope_root() else {
        return;
    };
    sweep_configs_in(&root, SystemTime::now(), STALE_AGE, alive);
    crate::agents::adapter::sweep_managed_runtimes(&root);
}

fn sweep_configs_in(
    dir: &Path,
    now: SystemTime,
    max_age: Duration,
    alive: impl FnOnce(&[&str]) -> Option<std::collections::HashSet<String>>,
) {
    let Some(instance_root) =
        super::paths::codesense_home_dir().and_then(|home| home.canonicalize().ok())
    else {
        return;
    };
    let stale: Vec<(PathBuf, String)> = stale_paths_in(dir, now, max_age)
        .into_iter()
        .filter_map(|path| {
            let owner: ScopeOwner =
                serde_json::from_slice(&std::fs::read(path.join("owner.json")).ok()?).ok()?;
            if owner.instance_root != instance_root {
                return None;
            }
            let session = path.file_name()?.to_str()?.to_string();
            Some((path, session))
        })
        .collect();
    if stale.is_empty() {
        return;
    }
    let sessions: Vec<&str> = stale.iter().map(|(_, s)| s.as_str()).collect();
    let Some(live) = alive(&sessions) else {
        return;
    };
    let mut swept = 0u32;
    for (path, session) in &stale {
        if live.contains(session) {
            continue;
        }
        // A live scoped relay reads its descriptor for the full invocation,
        // unlike the old one-shot config reader. Never age out another app's
        // live grant. Missing/unreadable descriptors remain conservative.
        let relay = path.join("local-relay.json");
        if relay.exists() {
            match codesense_host_proto::scoped_mcp::read_descriptor(&relay) {
                Ok(descriptor)
                    if std::os::unix::net::UnixStream::connect(&descriptor.socket).is_err() => {}
                _ => continue,
            }
        }
        crate::agents::adapter::teardown_managed_home(path);
        if finish_credential_copyback(path).is_err() {
            continue;
        }
        if !crate::session_conversation::capture_before_removal(session, path) {
            continue;
        }
        if std::fs::remove_dir_all(path).is_ok() {
            swept += 1;
        }
    }
    if swept > 0 {
        log::debug!(target: "mcp_scope", "swept {swept} stale scoped-MCP config dir(s) from a prior run");
    }
}

/// Enumerate only entries we can confidently age; shared by the two launch-file sweeps.
pub(crate) fn stale_paths_in(dir: &Path, now: SystemTime, max_age: Duration) -> Vec<PathBuf> {
    if std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| {
            entry
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|modified| is_stale(modified, now, max_age))
        })
        .map(|entry| entry.path())
        .collect()
}

/// Is `modified` older than `max_age` relative to `now`? A future mtime (clock skew) →
/// `duration_since` `Err` → treated as NOT stale (conservative — never reap a file we can't
/// confidently age out). Pure, so the age policy is unit-testable without touching mtimes.
pub(crate) fn is_stale(modified: SystemTime, now: SystemTime, max_age: Duration) -> bool {
    now.duration_since(modified)
        .map(|age| age > max_age)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::BRIDGE_SERVER_KEY;
    use super::*;
    use codesense_pty_core::agent_cli::ConfigFragmentFormat;
    use serde_json::json;

    /// The bridge (a separate binary crate) reads the tab session from its own copy of
    /// the variable name; a rename on either side would silently drop Codex's
    /// `env_vars` passthrough, so the two copies are pinned together here.
    #[test]
    fn the_bridge_reads_the_tab_session_variable_codex_is_told_to_pass() {
        let bridge = include_str!("../../crates/codesense-bridge/src/main.rs");
        let expected = format!("const TAB_SESSION_ENV: &str = \"{TAB_SESSION_ENV}\";");
        assert!(
            bridge.contains(&expected),
            "codesense-bridge must read {TAB_SESSION_ENV}"
        );
    }

    fn exact_cleanup_claim(session: &str) -> super::super::pty_host::LocalEffectClaim {
        use crate::commands::machines::{
            RuntimeCounter, RuntimeMemberAddress, RuntimeMemberSlot, RuntimeMemberTarget,
            RuntimeNamespace,
        };
        super::super::pty_host::LocalEffectOwners::default()
            .reserve(
                RuntimeMemberTarget {
                    member: RuntimeMemberAddress::local(RuntimeMemberSlot::InProcess),
                    admission_revision: RuntimeCounter::new(1),
                    namespace: RuntimeNamespace {
                        server_id: session.into(),
                        epoch: "scope-cleanup".into(),
                        generation: RuntimeCounter::new(1),
                    },
                },
                session,
            )
            .unwrap()
    }

    fn codex_cleanup_source(
        session: &str,
        dir: &Path,
    ) -> crate::session_usage::record::SessionRecord {
        use crate::session_conversation::CODEX_FIXTURE;
        let native = uuid::Uuid::new_v4().to_string();
        let logs = dir.join("sessions/2026/10/27");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join(format!("rollout-2026-10-27T10-00-00-{native}.jsonl")),
            CODEX_FIXTURE,
        )
        .unwrap();
        let mut record = crate::session_usage::record::SessionRecord::new(session);
        record.cli = Some("codex".into());
        record.native_session_id = Some(native);
        record.config_home = Some(dir.to_string_lossy().into_owned());
        record
    }

    fn persist_cleanup_record(record: &crate::session_usage::record::SessionRecord) {
        crate::session_usage::UsageStore::open(&crate::session_usage::store_path().unwrap())
            .unwrap()
            .write_session_record(record)
            .unwrap();
    }

    fn codex_cleanup_evidence(session: &str, dir: &Path) {
        persist_cleanup_record(&codex_cleanup_source(session, dir));
    }

    fn assert_cleanup_conversation(session: &str) {
        use crate::session_conversation::{read_conversation_in, Availability, Selector};
        let page = read_conversation_in(
            &crate::session_usage::store_path().unwrap(),
            &crate::session_conversation::conversations_dir().unwrap(),
            session,
            &Selector::Page {
                cursor: None,
                limit: None,
            },
        )
        .unwrap();
        assert_eq!(page.state, Availability::Present, "{:?}", page.reason);
        assert_eq!(page.tool_call_count, 3);
        assert!(!page.turns.is_empty());
    }

    #[test]
    fn pbf210_owned_terminal_cleanup_captures_codex_before_deletion_and_duplicate_settlement() {
        let _home = crate::routines::test_support::TempHome::new();
        let session = format!("capture-owned-{}", uuid::Uuid::new_v4());
        let owner = exact_cleanup_claim(&session);
        let (path, creation) =
            write_scope_config_with_creation(&session, &canonical(), &[], &claude_scope()).unwrap();
        bind_scope_creation(&creation, &owner).unwrap();
        let dir = path.parent().unwrap();
        codex_cleanup_evidence(&session, dir);
        assert_eq!(settle_owned_scope(&owner), Ok(true));
        assert!(!dir.exists());
        assert_cleanup_conversation(&session);
        assert_eq!(settle_owned_scope(&owner), Ok(false));
        assert_cleanup_conversation(&session);
    }

    #[test]
    fn pbf210_owned_respawn_cleanup_captures_codex_before_deletion() {
        let _home = crate::routines::test_support::TempHome::new();
        let session = format!("capture-respawn-{}", uuid::Uuid::new_v4());
        let owner = exact_cleanup_claim(&session);
        let (path, creation) =
            write_scope_config_with_creation(&session, &canonical(), &[], &claude_scope()).unwrap();
        bind_scope_creation(&creation, &owner).unwrap();
        let dir = path.parent().unwrap();
        codex_cleanup_evidence(&session, dir);
        assert!(remove_owned_scope(&owner));
        assert!(!dir.exists());
        assert_cleanup_conversation(&session);
        assert!(!remove_owned_scope(&owner));
        assert_cleanup_conversation(&session);
    }

    #[test]
    fn pbf210_restarted_stale_sweep_captures_dead_codex_and_preserves_live_scope() {
        let _home = crate::routines::test_support::TempHome::new();
        let dead = format!("capture-dead-{}", uuid::Uuid::new_v4());
        let live = format!("capture-live-{}", uuid::Uuid::new_v4());
        for session in [&dead, &live] {
            let path = write_scope_config(session, &canonical(), &[], &claude_scope()).unwrap();
            codex_cleanup_evidence(session, path.parent().unwrap());
        }
        // Close and reopen the durable store as startup would; no launch writer remains.
        let store = crate::session_usage::store_path().unwrap();
        drop(crate::session_usage::UsageStore::open(&store).unwrap());
        let root = scope_root().unwrap();
        let later = SystemTime::now() + STALE_AGE + Duration::from_secs(60);
        sweep_configs_in(&root, later, STALE_AGE, |_| None);
        assert!(root.join(&dead).exists());
        sweep_configs_in(&root, later, STALE_AGE, |_| {
            Some(std::collections::HashSet::from([live.clone()]))
        });
        assert!(!root.join(&dead).exists());
        assert!(root.join(&live).exists());
        assert_cleanup_conversation(&dead);
        // A second startup cannot erase the already captured evidence.
        sweep_configs_in(&root, later, STALE_AGE, |_| {
            Some(std::collections::HashSet::from([live.clone()]))
        });
        assert_cleanup_conversation(&dead);
        remove_scope_config(&live);
    }

    #[test]
    fn pbf210_missing_record_retains_owned_and_unowned_rollouts_with_durable_reason() {
        use crate::session_conversation::{read_conversation_in, Selector};
        let _home = crate::routines::test_support::TempHome::new();
        for cleanup in ["unowned", "settle", "remove-owned"] {
            let session = format!("capture-timeout-{cleanup}-{}", uuid::Uuid::new_v4());
            let (path, creation) =
                write_scope_config_with_creation(&session, &canonical(), &[], &claude_scope())
                    .unwrap();
            let dir = path.parent().unwrap();
            let logs = dir.join("sessions");
            std::fs::create_dir(&logs).unwrap();
            let rollout = logs.join("preserve.jsonl");
            std::fs::write(&rollout, crate::session_conversation::CODEX_FIXTURE).unwrap();
            if cleanup != "unowned" {
                let owner = exact_cleanup_claim(&session);
                bind_scope_creation(&creation, &owner).unwrap();
                if cleanup == "settle" {
                    assert_eq!(settle_owned_scope(&owner), Ok(false));
                } else {
                    assert!(!remove_owned_scope(&owner));
                }
            } else {
                remove_scope_config(&session);
            }
            assert!(rollout.exists(), "record timeout must retain the source");
            // The reader opens the store again, so the reason must be durable.
            let page = read_conversation_in(
                &crate::session_usage::store_path().unwrap(),
                &crate::session_conversation::conversations_dir().unwrap(),
                &session,
                &Selector::Page {
                    cursor: None,
                    limit: None,
                },
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(page.state).unwrap(),
                json!("unavailable")
            );
            assert!(page
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("record")));
            assert!(page.turns.is_empty());
        }
    }

    #[test]
    fn rm5aa_exact_scope_cleanup_serializes_duplicate_removal() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let session = format!("scope-duplicate-{}", rand::random::<u64>());
        let owner = exact_cleanup_claim(&session);
        let (path, creation) =
            write_scope_config_with_creation(&session, &canonical(), &[], &claude_scope()).unwrap();
        bind_scope_creation(&creation, &owner).unwrap();
        assert_eq!(owned_creation(&owner).unwrap(), Some(creation));
        let slot = scope_slot(&session);
        let held = slot.lock().unwrap();
        let barrier = std::sync::Barrier::new(3);
        let results = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                settle_owned_scope(&owner)
            });
            let second = scope.spawn(|| {
                barrier.wait();
                settle_owned_scope(&owner)
            });
            barrier.wait();
            // Both callers have the same pre-removal creation; neither can inspect
            // or delete until the resource reservation, not an effects lock, opens.
            drop(held);
            [
                first.join().unwrap().unwrap(),
                second.join().unwrap().unwrap(),
            ]
        });
        assert_eq!(results.into_iter().filter(|removed| *removed).count(), 1);
        assert!(!path.exists());
        assert!(owned_creation(&owner).unwrap().is_none());
        // An untracked path is not cleanup authority, even after prior success.
        let dir = mcp_scope_session_dir(&session);
        std::fs::create_dir(&dir).unwrap();
        let marker = dir.join("foreign-marker");
        std::fs::write(&marker, b"preserve").unwrap();
        assert_eq!(settle_owned_scope(&owner), Ok(false));
        assert_eq!(std::fs::read(&marker).unwrap(), b"preserve");
        std::fs::remove_file(marker).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn rm5aa_exact_scope_cleanup_retains_foreign_owner_and_changed_identity() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let session = format!("scope-refusal-{}", rand::random::<u64>());
        let owner = exact_cleanup_claim(&session);
        let (path, creation) =
            write_scope_config_with_creation(&session, &canonical(), &[], &claude_scope()).unwrap();
        bind_scope_creation(&creation, &owner).unwrap();
        let mut foreign = owner.clone();
        foreign.target.namespace.epoch.push_str("-foreign");
        assert!(settle_owned_scope(&foreign).is_err());
        assert!(path.exists());
        assert_eq!(owned_creation(&owner).unwrap(), Some(creation.clone()));
        let dir = mcp_scope_session_dir(&session);
        let backup = dir.with_extension(format!("backup-{}", rand::random::<u64>()));
        std::fs::rename(&dir, &backup).unwrap();
        std::fs::create_dir(&dir).unwrap();
        let marker = dir.join("foreign-marker");
        std::fs::write(&marker, b"preserve").unwrap();
        assert!(settle_owned_scope(&owner).is_err());
        assert_eq!(std::fs::read(&marker).unwrap(), b"preserve");
        assert_eq!(
            scope_slot(&session)
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .creation,
            creation
        );
        std::fs::remove_file(marker).unwrap();
        std::fs::remove_dir(&dir).unwrap();
        std::fs::rename(&backup, &dir).unwrap();
        assert_eq!(settle_owned_scope(&owner), Ok(true));
    }

    fn credential_test_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "codesense-mcp-{tag}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ))
    }

    /// The descriptor the built-in Claude adapter declares — the values SW-AN-41/42
    /// hardcoded. Threaded through the projection tests so their output stays
    /// byte-identical to the pre-AN-43 behavior (AC-2).
    fn claude_scope() -> McpScope {
        McpScope {
            config_filename: ".mcp.json".to_string(),
            server_map_key: "mcpServers".to_string(),
            bridge_id: BRIDGE_SERVER_KEY.to_string(),
            bridge_entry: codesense_pty_core::agent_cli::McpBridgeEntry::Command,
            format: ConfigFragmentFormat::Json,
            injection: ScopeInjection::ConfigFlag {
                flag_template: vec![
                    "--mcp-config".to_string(),
                    "{path}".to_string(),
                    "--strict-mcp-config".to_string(),
                ],
                open_flag_template: None,
                settings_flag_template: None,
            },
            bridge_override: None,
            inventory: None,
        }
    }

    fn opencode_scope() -> McpScope {
        McpScope {
            config_filename: "opencode.json".to_string(),
            server_map_key: "mcp".to_string(),
            bridge_id: "codesense".to_string(),
            bridge_entry: codesense_pty_core::agent_cli::McpBridgeEntry::LocalCommandArray {
                environment_key: "environment",
            },
            format: ConfigFragmentFormat::Json,
            injection: ScopeInjection::EnvContent {
                env_var: "OPENCODE_CONFIG_CONTENT".to_string(),
            },
            bridge_override: None,
            inventory: None,
        }
    }

    #[test]
    fn opencode_inline_config_projects_mcp_and_bridge_tool_patterns() {
        let scope = opencode_scope();
        let canonical = json!({
            "mcp": {
                "codesense": {
                    "type": "local",
                    "command": ["/bridge"],
                    "enabled": true,
                    "environment": {"CODESENSE_TAB_SESSION_ID": "tab-7"}
                },
                "slack": {"type": "local", "command": ["slack-mcp"], "enabled": true},
                "unselected": {"type": "local", "command": ["private-mcp"], "enabled": true}
            }
        });
        let config = env_content_config(
            &canonical,
            &["slack".into()],
            Some(&["who_am_i".into()]),
            &scope,
        );
        assert_eq!(
            config["mcp"]["codesense"]["environment"][TAB_SESSION_ENV],
            "tab-7"
        );
        assert_eq!(config["mcp"]["slack"]["enabled"], true);
        assert_eq!(config["mcp"]["unselected"]["enabled"], false);
        assert!(config["mcp"]["unselected"].get("command").is_none());
        assert_eq!(config["share"], "disabled");
        assert_eq!(config["update"], "disable");
        assert!(config.get("tools").is_none());
        assert_eq!(permission_effect(&config, "*"), Value::Null);
        assert_eq!(permission_effect(&config, "codesense_*"), "deny");
        assert_eq!(permission_effect(&config, "codesense_who_am_i"), "allow");
        assert_eq!(config["permissions"].as_array().unwrap().len(), 2);
    }

    fn opencode_effective(config: &Value, agent: &Value, tool: &str) -> String {
        let settings = crate::agents::adapter::native_permission_rules(agent);
        let mut env = vec![("OPENCODE_CONFIG_CONTENT".into(), config.to_string())];
        extend_launch_env(
            AgentCli::OpenCode,
            &mut env,
            [("OPENCODE_CONFIG_CONTENT".into(), settings.to_string())],
        );
        let merged: Value = serde_json::from_str(&env[0].1).unwrap();
        merged["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|rule| {
                let action = rule["action"].as_str().unwrap();
                action == "*"
                    || action
                        .strip_suffix('*')
                        .is_some_and(|prefix| tool.starts_with(prefix))
                    || action == tool
            })
            .unwrap()["effect"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn permission_effect(config: &Value, action: &str) -> Value {
        config["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|rule| rule["action"] == action)
            .map(|rule| rule["effect"].clone())
            .unwrap_or(Value::Null)
    }

    #[test]
    fn opencode_env_permission_base_does_not_override_excluded_bridge_tools() {
        let canonical = json!({"mcp": {"codesense": {"type": "local", "command": ["/bridge"]}}});
        let config = env_content_config(
            &canonical,
            &[],
            Some(&["notes_read".into()]),
            &opencode_scope(),
        );
        // The shape `agents::permissions::opencode` generates for an agent with no overrides.
        let agent = json!({"*": "ask", "codesense_*": "deny", "codesense_who_am_i": "allow"});
        assert_eq!(
            opencode_effective(&config, &agent, "codesense_notes_read"),
            "allow"
        );
        assert_eq!(
            opencode_effective(&config, &agent, "codesense_notes_write"),
            "deny"
        );
        assert_eq!(
            opencode_effective(&config, &agent, "codesense_who_am_i"),
            "allow"
        );
        assert_eq!(opencode_effective(&config, &agent, "bash"), "ask");
    }

    #[test]
    fn opencode_unrestricted_bridge_adds_no_permission_rules_so_ordinary_tools_stay_reachable() {
        let canonical = json!({"mcp": {"codesense": {"type": "local", "command": ["/bridge"]}}});
        let config = env_content_config(&canonical, &[], None, &opencode_scope());
        assert!(config.get("permissions").is_none());
        // The shape `agents::permissions::opencode` generates for an agent with no allowlist.
        let agent = json!({"*": "ask", "codesense_who_am_i": "allow"});
        for tool in ["codesense_notes_read", "codesense_notes_write"] {
            assert_eq!(opencode_effective(&config, &agent, tool), "ask", "{tool}");
        }
        assert_eq!(
            opencode_effective(&config, &agent, "codesense_who_am_i"),
            "allow"
        );
    }

    #[test]
    fn opencode_projector_returns_inline_env_without_creating_a_scope_file() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::write(
            cwd.path().join("opencode.json"),
            r#"{"mcp":{"slack":{"type":"local","command":["slack-mcp"],"enabled":true},"ignored":{"type":"local","command":["other"],"enabled":true}}}"#,
        )
        .unwrap();
        let session = format!("opencode-scope-{}", rand::random::<u64>());
        let (outcome, creation) = project_scope_for_cli(
            &session,
            cwd.path(),
            &["slack".into()],
            Some(&["who_am_i".into()]),
            Some(&opencode_scope()),
            None,
            None,
            None,
            str::to_owned,
        )
        .unwrap();
        assert!(creation.is_none());
        assert!(outcome.extra_args.is_empty());
        assert_eq!(outcome.extra_env.len(), 1);
        let (_, raw) = &outcome.extra_env[0];
        let config: Value = serde_json::from_str(raw).unwrap();
        assert!(config["mcp"]["codesense"].is_object());
        assert_eq!(
            config["mcp"]["codesense"]["environment"][TAB_SESSION_ENV],
            session
        );
        assert!(config["mcp"]["slack"].is_object());
        assert_eq!(config["mcp"]["ignored"]["enabled"], false);
        assert!(config["mcp"]["ignored"].get("command").is_none());
        assert_eq!(permission_effect(&config, "codesense_who_am_i"), "allow");
        assert!(!mcp_scope_session_dir(&session).exists());
    }

    #[test]
    fn an_unqualified_opencode_version_projects_no_plugin_and_other_versions_do() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let cwd = tempfile::tempdir().unwrap();
        let binary = Path::new("/bin/opencode");
        let plugin_entries = |version: Option<&str>| {
            let session = format!("opencode-version-{}", rand::random::<u64>());
            let mut cli = LaunchCli::new(binary, &[]);
            cli.version = version;
            let (outcome, _) = project_scope_for_cli(
                &session,
                cwd.path(),
                &[],
                None,
                Some(&opencode_scope()),
                None,
                None,
                Some(&cli),
                str::to_owned,
            )
            .unwrap();
            let config: Value = serde_json::from_str(&outcome.extra_env[0].1).unwrap();
            // MCP and the bridge are untouched either way.
            assert!(config["mcp"]["codesense"].is_object());
            config.get("plugins").cloned()
        };
        assert!(plugin_entries(Some("2.0.1")).is_none());
        assert!(plugin_entries(Some("1.17.0")).is_none());
        assert_eq!(
            plugin_entries(Some("2.1.4"))
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        // A version nobody read is the plugin's own host check to make.
        assert!(plugin_entries(None).is_some());
    }

    #[test]
    fn opencode_open_profile_projects_only_the_bridge_inline() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let session = format!("opencode-open-{}", rand::random::<u64>());
        let (outcome, creation) =
            project_open_scope(&session, &opencode_scope(), None, str::to_owned).unwrap();
        assert!(creation.is_none());
        assert!(outcome.extra_args.is_empty());
        let config: Value = serde_json::from_str(&outcome.extra_env[0].1).unwrap();
        assert_eq!(config["mcp"].as_object().unwrap().len(), 1);
        assert_eq!(
            config["mcp"]["codesense"]["environment"][TAB_SESSION_ENV],
            session
        );
    }

    #[test]
    fn opencode_headless_profile_injects_permission_env_beside_mcp_config() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let cwd = tempfile::tempdir().unwrap();
        let session = format!("opencode-profile-{}", rand::random::<u64>());
        let (outcome, creation) = project_profile_launch(
            &session,
            cwd.path(),
            Some(&[]),
            AgentCli::OpenCode,
            None,
            ProfileLaunch {
                headless: true,
                bridge_override: false,
            },
            str::to_owned,
        )
        .unwrap();
        assert!(creation.is_none());
        assert_eq!(outcome.extra_env.len(), 1);
        let permission = outcome
            .extra_env
            .iter()
            .find(|(key, _)| key == "OPENCODE_CONFIG_CONTENT")
            .map(|(_, value)| serde_json::from_str::<Value>(value).unwrap())
            .unwrap();
        assert_eq!(permission_effect(&permission, "*"), "deny");
        assert_eq!(permission_effect(&permission, "read"), "allow");
        assert!(permission.get("permission").is_none());
    }

    /// A synthetic `env_dir` (Codex-shaped) descriptor for the injection tests. Hermetic:
    /// a test-only `env_var` that is guaranteed unset (so the real home comes from the
    /// supplied `default_home`, not the machine's `~/.codex`) — no `~/.codex` dependency.
    fn test_env_dir_scope(default_home: &str) -> McpScope {
        McpScope {
            config_filename: ".codex/config.toml".to_string(),
            server_map_key: "mcp_servers".to_string(),
            bridge_id: "codesense".to_string(),
            bridge_entry: codesense_pty_core::agent_cli::McpBridgeEntry::Command,
            format: ConfigFragmentFormat::Toml,
            injection: ScopeInjection::EnvDir {
                carry_home: true,
                env_var: "AN45_TEST_HOME_UNSET".to_string(),
                passthrough: vec![Passthrough {
                    name: "auth.json".to_string(),
                    mode: PassthroughMode::Symlink,
                }],
                default_home: default_home.to_string(),
            },
            bridge_override: None,
            inventory: None,
        }
    }

    fn canonical() -> Value {
        json!({
            "mcpServers": {
                BRIDGE_SERVER_KEY: { "command": "/abs/bridge" },
                "buildkite": { "command": "bk", "args": ["mcp"], "env": { "TOKEN": "secret" } },
                "playwright": { "command": "npx", "args": ["playwright-mcp"] }
            }
        })
    }

    #[test]
    fn bridge_is_always_included_even_with_empty_allow() {
        // AC-2 / decision #5: `allow = []` still yields the bridge (a bridge-less strict
        // launch could never signal_done).
        let eff = effective_mcp_servers(&canonical(), &[], &claude_scope());
        let servers = eff["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 1, "only the bridge");
        assert!(servers.contains_key(BRIDGE_SERVER_KEY));
    }

    #[test]
    fn effective_set_is_allow_union_bridge_intersect_present() {
        // AC-2: (allow ∪ {bridge}) ∩ present.
        let eff = effective_mcp_servers(&canonical(), &["buildkite".into()], &claude_scope());
        let servers = eff["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 2);
        assert!(servers.contains_key(BRIDGE_SERVER_KEY));
        assert!(servers.contains_key("buildkite"));
        assert!(
            !servers.contains_key("playwright"),
            "not allowlisted → absent"
        );
    }

    #[test]
    fn selected_server_sub_objects_are_byte_identical_to_canonical() {
        // AC-2: server entries copied verbatim (resolved command/args/env intact).
        let canon = canonical();
        let eff = effective_mcp_servers(&canon, &["buildkite".into()], &claude_scope());
        assert_eq!(
            eff["mcpServers"]["buildkite"], canon["mcpServers"]["buildkite"],
            "sub-object copied verbatim (env secret preserved)"
        );
        assert_eq!(
            eff["mcpServers"][BRIDGE_SERVER_KEY],
            canon["mcpServers"][BRIDGE_SERVER_KEY]
        );
    }

    #[test]
    fn dangling_allow_entry_is_dropped_without_error() {
        // AC-5: an uninstalled server is silently dropped (intent retained upstream).
        let eff = effective_mcp_servers(
            &canonical(),
            &["ghost".into(), "buildkite".into()],
            &claude_scope(),
        );
        let servers = eff["mcpServers"].as_object().unwrap();
        assert!(!servers.contains_key("ghost"));
        assert!(servers.contains_key("buildkite"));
        assert!(servers.contains_key(BRIDGE_SERVER_KEY));
    }

    #[test]
    fn is_stale_spares_recent_files_and_reaps_old_ones() {
        // The startup sweep must not reap a recent file (a concurrent instance's live
        // launch config), only genuinely aged-out orphans; a future mtime (clock skew)
        // is conservatively spared.
        let now = SystemTime::now();
        assert!(
            !is_stale(now - Duration::from_secs(5), now, STALE_AGE),
            "a 5s-old file (possible concurrent live launch) is spared"
        );
        assert!(
            is_stale(now - Duration::from_secs(7200), now, STALE_AGE),
            "a 2h-old orphan is reaped"
        );
        assert!(
            !is_stale(now + Duration::from_secs(60), now, STALE_AGE),
            "a future mtime (clock skew) is spared, not reaped"
        );
    }

    #[test]
    fn allowlist_entries_are_trimmed_and_blanks_ignored() {
        // Hand-authoring tolerance: a whitespace-padded id matches; a blank entry is
        // ignored (not treated as a dangling server).
        let eff = effective_mcp_servers(
            &canonical(),
            &[" buildkite ".into(), "   ".into()],
            &claude_scope(),
        );
        let servers = eff["mcpServers"].as_object().unwrap();
        assert!(
            servers.contains_key("buildkite"),
            "padded id matches canonical key"
        );
        assert!(servers.contains_key(BRIDGE_SERVER_KEY));
        assert_eq!(servers.len(), 2, "blank entry ignored, no phantom server");
    }

    #[test]
    fn allow_naming_the_bridge_does_not_double_insert() {
        let eff = effective_mcp_servers(
            &canonical(),
            &[BRIDGE_SERVER_KEY.into(), "buildkite".into()],
            &claude_scope(),
        );
        let servers = eff["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 2, "bridge counted once");
    }

    #[test]
    fn missing_mcp_servers_key_yields_bridge_less_empty_set() {
        // Defensive: a canonical with no `mcpServers` object → empty (bridge warn logged).
        let eff = effective_mcp_servers(&json!({}), &["buildkite".into()], &claude_scope());
        assert_eq!(eff["mcpServers"].as_object().unwrap().len(), 0);
    }

    #[test]
    fn write_scope_config_writes_0600_and_removes() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // AC-6: file materializes with the effective set, mode 0600, and teardown removes it.
        let session_id = format!("shell-mcp-test-{}", std::process::id());
        let path = write_scope_config(
            &session_id,
            &canonical(),
            &["buildkite".into()],
            &claude_scope(),
        )
        .unwrap();
        assert!(path.exists());
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let servers = written["mcpServers"].as_object().unwrap();
        assert!(servers.contains_key(BRIDGE_SERVER_KEY));
        assert!(servers.contains_key("buildkite"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "resolved-secrets file must be owner-only"
            );
        }
        remove_scope_config(&session_id);
        assert!(!path.exists(), "teardown removes the ephemeral config");
    }

    #[test]
    fn projection_never_mutates_the_canonical_mcp_json() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // AC-3: the whole point — reading + projecting leaves canonical `.mcp.json`
        // byte-for-byte identical (the two-writer fixed-point is never touched).
        let dir = std::env::temp_dir().join(format!("an41-canon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let canon_path = dir.join(".mcp.json");
        let canon_bytes = serde_json::to_vec_pretty(&canonical()).unwrap();
        std::fs::write(&canon_path, &canon_bytes).unwrap();

        // Mimic the spawn path: read canonical, project, write the ephemeral config.
        let read: Value = serde_json::from_slice(&std::fs::read(&canon_path).unwrap()).unwrap();
        let sid = format!("shell-mcp-canon-{}", std::process::id());
        let _ = write_scope_config(&sid, &read, &["buildkite".into()], &claude_scope()).unwrap();

        assert_eq!(
            std::fs::read(&canon_path).unwrap(),
            canon_bytes,
            "canonical .mcp.json must be byte-unchanged after a scoped projection"
        );
        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_differently_scoped_sessions_get_isolated_configs() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // AC-4 (headline): two agents in ONE worktree with disjoint allowlists get their
        // own session-keyed ephemeral configs — distinct paths, distinct effective sets,
        // neither affecting the other. (Per-process isolation via `--mcp-config`, the
        // concurrency-safe pattern; nothing shared is written.)
        let canon = canonical();
        let sid_a = format!("shell-mcp-a-{}", std::process::id());
        let sid_b = format!("shell-mcp-b-{}", std::process::id());
        let pa =
            write_scope_config(&sid_a, &canon, &["buildkite".into()], &claude_scope()).unwrap();
        let pb =
            write_scope_config(&sid_b, &canon, &["playwright".into()], &claude_scope()).unwrap();
        assert_ne!(pa, pb, "distinct session-keyed paths");

        let a: Value = serde_json::from_slice(&std::fs::read(&pa).unwrap()).unwrap();
        let b: Value = serde_json::from_slice(&std::fs::read(&pb).unwrap()).unwrap();
        assert!(
            a["mcpServers"].get("buildkite").is_some()
                && a["mcpServers"].get("playwright").is_none()
        );
        assert!(
            b["mcpServers"].get("playwright").is_some()
                && b["mcpServers"].get("buildkite").is_none()
        );
        // Both carry the bridge; neither leaked the other's server.
        assert!(a["mcpServers"].get(BRIDGE_SERVER_KEY).is_some());
        assert!(b["mcpServers"].get(BRIDGE_SERVER_KEY).is_some());

        remove_scope_config(&sid_a);
        remove_scope_config(&sid_b);
    }

    #[test]
    fn project_from_worktree_reads_canonical_and_scopes() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // The one-call launch step: read cwd/.mcp.json + project. Reused by AN-42.
        let dir = std::env::temp_dir().join(format!("an41-pfw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".mcp.json"),
            serde_json::to_vec_pretty(&canonical()).unwrap(),
        )
        .unwrap();
        let sid = format!("shell-mcp-pfw-{}", std::process::id());
        let path =
            project_from_worktree(&sid, &dir, &["buildkite".into()], &claude_scope()).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(written["mcpServers"].get("buildkite").is_some());
        assert!(written["mcpServers"].get(BRIDGE_SERVER_KEY).is_some());
        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_from_worktree_scopes_a_folder_without_config_to_the_bridge_and_errors_on_invalid() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // A folder with no canonical .mcp.json still launches scoped (never unscoped):
        // its scoped config holds the bridge alone, and nothing is written into it.
        let dir = std::env::temp_dir().join(format!("an41-pfw-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sid = format!("shell-mcp-pfwbad-{}", std::process::id());
        let path =
            project_from_worktree(&sid, &dir, &["buildkite".into()], &claude_scope()).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let servers = written["mcpServers"].as_object().unwrap();
        assert!(servers.keys().all(|k| k == BRIDGE_SERVER_KEY), "{written}");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        remove_scope_config(&sid);

        std::fs::write(dir.join(".mcp.json"), b"{not json").unwrap();
        let invalid = project_from_worktree(&sid, &dir, &[], &claude_scope()).unwrap_err();
        assert!(invalid.contains("failed to parse .mcp.json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Codex scrubs a stdio server's environment: the launch flags that register the
    /// bridge without a scoped config pass the tab session variable through by name.
    #[test]
    fn a_codex_bridge_override_passes_the_tab_session_through() {
        let codex = descriptor(AgentCli::Codex);
        let scope = codex.mcp_scope.as_ref().unwrap();
        let quoted = bridge_env_override(scope, codesense_pty_core::shell_quote);
        assert_eq!(
            quoted,
            [
                "-c".to_string(),
                r#"'mcp_servers.codesense.env_vars=["CODESENSE_TAB_SESSION_ID"]'"#.to_string()
            ]
        );
        assert!(passes_env_explicitly(scope));
        // Claude passes its environment through.
        assert!(!passes_env_explicitly(&claude_scope()));
    }

    #[test]
    fn with_bridge_server_always_puts_this_apps_bridge_in_the_session_copy() {
        let scope = claude_scope();
        let bridge = Path::new("/opt/codesense/bin/codesense-bridge");
        let mut empty = json!({});
        with_bridge_server(&mut empty, &scope, Some(bridge), None);
        assert_eq!(
            empty,
            json!({ "mcpServers": { "codesense": { "command": "/opt/codesense/bin/codesense-bridge" } } })
        );
        // A folder's stale entry is replaced in the session's copy; its other servers stay.
        let mut stale = json!({ "mcpServers": {
            "codesense": { "command": "/old/app/codesense-bridge", "args": ["--old"] },
            "slack": { "command": "slack-mcp" }
        } });
        with_bridge_server(&mut stale, &scope, Some(bridge), None);
        assert_eq!(
            stale["mcpServers"]["codesense"],
            json!({ "command": "/opt/codesense/bin/codesense-bridge" })
        );
        assert_eq!(stale["mcpServers"]["slack"]["command"], "slack-mcp");
        let mut untouched = json!({});
        with_bridge_server(&mut untouched, &scope, None, None);
        assert_eq!(untouched, json!({}));
    }

    // -------------------------------------------------------------------
    // SW-AN-52 — content-layer base unioned with cwd .mcp.json
    // -------------------------------------------------------------------

    /// AC-3 (Claude route): a server defined only in the content-layer base
    /// reaches the agent even when the cwd `.mcp.json` is empty / absent.
    #[test]
    fn content_layer_base_without_cwd_still_offers_the_server_to_launch() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let dir = std::env::temp_dir().join(format!("an52-base-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // No cwd .mcp.json. Base defines `slack`.
        let base = json!({
            "mcpServers": {
                "slack": { "command": "slack-mcp" }
            }
        });
        let sid = format!("shell-an52-base-{}", uuid::Uuid::new_v4());
        let path =
            project_from_worktree_with_base(&sid, &dir, &["slack".into()], &base, &claude_scope())
                .unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            written["mcpServers"]["slack"]["command"], "slack-mcp",
            "the content-layer base's server reaches the agent"
        );
        assert!(
            written["mcpServers"].get(BRIDGE_SERVER_KEY).is_some(),
            "bridge is still force-included"
        );
        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fixture #5: cwd `.mcp.json` deep-merges on top of the content-layer
    /// base (nearest-wins). A worktree-local override of `slack` masks the
    /// team-shared config.
    #[test]
    fn cwd_mcp_json_overrides_the_content_layer_base_entry() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let dir = std::env::temp_dir().join(format!("an52-cwd-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".mcp.json"),
            br#"{"mcpServers":{"slack":{"command":"slack-staging"}}}"#,
        )
        .unwrap();
        let base = json!({
            "mcpServers": {
                "slack": { "command": "slack-prod" }
            }
        });
        let sid = format!("shell-an52-cwd-{}", uuid::Uuid::new_v4());
        let path =
            project_from_worktree_with_base(&sid, &dir, &["slack".into()], &base, &claude_scope())
                .unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            written["mcpServers"]["slack"]["command"], "slack-staging",
            "cwd nearer than any content layer — cwd wins"
        );
        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fixture #4: a stale `codesense` entry in the content-layer base is
    /// overwritten by `with_bridge_server`. The team cannot accidentally kill
    /// its agents by shipping an outdated bridge path.
    #[test]
    fn content_layer_stale_bridge_entry_is_overwritten_by_the_real_bridge() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let Some(bridge) = crate::commands::deps::bridge_symlink_path() else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("an52-stale-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let base = json!({
            "mcpServers": {
                BRIDGE_SERVER_KEY: { "command": "/bogus/bridge" },
                "slack": { "command": "slack-mcp" }
            }
        });
        let sid = format!("shell-an52-stale-{}", uuid::Uuid::new_v4());
        let path =
            project_from_worktree_with_base(&sid, &dir, &["slack".into()], &base, &claude_scope())
                .unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            written["mcpServers"][BRIDGE_SERVER_KEY]["command"],
            json!(bridge.to_string_lossy()),
            "the real bridge path wins over a stale content-layer entry"
        );
        assert_eq!(
            written["mcpServers"]["slack"]["command"], "slack-mcp",
            "non-bridge servers from the base are preserved"
        );
        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// AC-4 (fixture #6): a worktree with a cwd `.mcp.json` and no content
    /// layers (base = `None`) still launches with cwd servers. Pre-SW-AN-52
    /// behavior preserved for every external repo / sample / BYO project.
    #[test]
    fn cwd_only_launch_without_a_content_layer_base_matches_pre_sw_an_52() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let dir = std::env::temp_dir().join(format!("an52-cwdonly-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".mcp.json"),
            br#"{"mcpServers":{"slack":{"command":"cwd-slack"}}}"#,
        )
        .unwrap();
        let sid = format!("shell-an52-cwdonly-{}", uuid::Uuid::new_v4());
        let path = project_from_worktree(&sid, &dir, &["slack".into()], &claude_scope()).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            written["mcpServers"]["slack"]["command"], "cwd-slack",
            "cwd-only path still carries cwd servers to the launch"
        );
        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_scoped_launch_never_runs_the_folders_stale_bridge_and_leaves_the_folder_alone() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let Some(bridge) = crate::commands::deps::bridge_symlink_path() else {
            return; // no home dir: nothing to resolve the bridge against
        };
        let dir = tempfile::tempdir().unwrap();
        let stale = br#"{"mcpServers":{"codesense":{"command":"/old/app/codesense-bridge"}}}"#;
        std::fs::write(dir.path().join(".mcp.json"), stale).unwrap();
        let sid = format!("shell-stale-{}", uuid::Uuid::new_v4());
        let path = project_from_worktree(&sid, dir.path(), &[], &claude_scope()).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            written["mcpServers"]["codesense"]["command"],
            json!(bridge.to_string_lossy())
        );
        assert_eq!(std::fs::read(dir.path().join(".mcp.json")).unwrap(), stale);
        remove_scope_config(&sid);
    }

    /// A git repo with one commit, as a story worktree is.
    fn git_repo(dir: &Path) {
        for args in [
            &["init", "-q"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ][..],
        ] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap()
                .success());
        }
    }

    fn git_status_all(dir: &Path) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "status",
                "--porcelain",
                "--ignored",
                "--untracked-files=all",
            ])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    /// How many hook commands on `event` run the status hook.
    const HEADLESS: ProfileLaunch = ProfileLaunch {
        headless: true,
        bridge_override: false,
    };

    fn notify_hook_count(settings: &Value, event: &str) -> usize {
        settings["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|group| group["hooks"].as_array().cloned().unwrap_or_default())
            .filter(|hook| {
                hook["command"]
                    .as_str()
                    .is_some_and(|c| c.contains("codesense-notify"))
            })
            .count()
    }

    #[test]
    fn a_profile_launch_with_no_allowlist_adds_the_bridge_and_hooks_without_narrowing() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(
            dir.path().join(".mcp.json"),
            br#"{"mcpServers":{"slack":{"command":"slack-mcp"}}}"#,
        )
        .unwrap();
        // Tracked, as a repo that commits its project servers does.
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x"
            ])
            .status()
            .unwrap()
            .success());
        let sid = format!("shell-open-{}", uuid::Uuid::new_v4());
        let (outcome, creation) = project_profile_launch(
            &sid,
            dir.path(),
            None,
            AgentCli::Claude,
            None,
            ProfileLaunch::default(),
            |p| p.to_string(),
        )
        .unwrap();
        assert!(creation.is_some());
        let args = &outcome.extra_args;
        assert_eq!(args[0], "--mcp-config");
        assert!(!args.iter().any(|a| a == "--strict-mcp-config"), "{args:?}");
        let config: Value = serde_json::from_slice(&std::fs::read(&args[1]).unwrap()).unwrap();
        let servers = config["mcpServers"].as_object().unwrap();
        assert!(servers.keys().all(|k| k == BRIDGE_SERVER_KEY), "{config}");
        let at = args.iter().position(|a| a == "--settings").unwrap();
        let settings: Value =
            serde_json::from_slice(&std::fs::read(&args[at + 1]).unwrap()).unwrap();
        assert!(
            settings["permissions"].get("defaultMode").is_none(),
            "an interactive launch keeps its mode"
        );
        // SV-CDE-8 d2e84d9c6 (#2691) intentionally adds credential deny rules.
        assert_eq!(
            settings["permissions"]["deny"],
            json!(codesense_pty_core::notify_hook::deny_rules())
        );
        for ev in crate::agent_state::status_hooks().map(|hook| hook.event) {
            assert_eq!(notify_hook_count(&settings, ev), 1, "{ev}");
        }
        assert!(Path::new(&args[at + 1]).starts_with(mcp_scope_session_dir(&sid)));
        let status = git_status_all(dir.path());
        assert_eq!(
            status, "?? .mcp.json\n",
            "only the repo's own file: {status}"
        );
        remove_scope_config(&sid);
        assert!(
            !mcp_scope_session_dir(&sid).exists(),
            "both files go with the session"
        );
    }

    #[test]
    fn a_scoped_profile_launch_gets_hooks_only_where_the_folder_lacks_them() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            json!({"hooks": {"Stop": [{"hooks": [{"type": "command",
                "command": codesense_pty_core::notify_hook::status_command()}]}]}})
            .to_string(),
        )
        .unwrap();
        let before = git_status_all(dir.path());
        let sid = format!("shell-scoped-{}", uuid::Uuid::new_v4());
        let (outcome, _) = project_profile_launch(
            &sid,
            dir.path(),
            Some(&[]),
            AgentCli::Claude,
            None,
            HEADLESS,
            |p| p.to_string(),
        )
        .unwrap();
        let args = &outcome.extra_args;
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
        let at = args.iter().position(|a| a == "--settings").unwrap();
        let settings: Value =
            serde_json::from_slice(&std::fs::read(&args[at + 1]).unwrap()).unwrap();
        assert_eq!(settings["permissions"]["defaultMode"], json!("dontAsk"));
        assert_eq!(
            notify_hook_count(&settings, "Stop"),
            0,
            "the folder runs it already"
        );
        assert_eq!(notify_hook_count(&settings, "PreToolUse"), 1);
        assert_eq!(
            git_status_all(dir.path()),
            before,
            "nothing new in the worktree"
        );
        remove_scope_config(&sid);
    }

    #[test]
    fn an_env_dir_profile_launch_with_no_allowlist_adds_nothing() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let dir = tempfile::tempdir().unwrap();
        let sid = format!("shell-codex-open-{}", uuid::Uuid::new_v4());
        let (outcome, creation) = project_profile_launch(
            &sid,
            dir.path(),
            None,
            AgentCli::Codex,
            None,
            HEADLESS,
            |p| p.to_string(),
        )
        .unwrap();
        assert_eq!(outcome, ScopeOutcome::default());
        assert!(creation.is_none());
        assert!(!mcp_scope_session_dir(&sid).exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_default_args_codex_launch_gets_the_bridge_as_launch_flags() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let Some(bridge) = crate::commands::deps::bridge_symlink_path() else {
            return; // no home dir: nothing to resolve the bridge against
        };
        let dir = tempfile::tempdir().unwrap();
        let sid = format!("shell-codex-default-{}", uuid::Uuid::new_v4());
        let launch = ProfileLaunch {
            headless: false,
            bridge_override: true,
        };
        let (outcome, creation) =
            project_profile_launch(&sid, dir.path(), None, AgentCli::Codex, None, launch, |p| {
                p.to_string()
            })
            .unwrap();
        assert_eq!(
            outcome.extra_args,
            vec![
                "-c".to_string(),
                format!("mcp_servers.codesense.command={}", bridge.display()),
                // Codex scrubs the bridge's environment: the tab session rides through.
                "-c".to_string(),
                r#"mcp_servers.codesense.env_vars=["CODESENSE_TAB_SESSION_ID"]"#.to_string(),
            ]
        );
        assert!(
            outcome.extra_env.is_empty(),
            "the user's own CODEX_HOME stays"
        );
        assert!(creation.is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        // Claude declares no override: its bridge rides the session config instead.
        let sid = format!("shell-claude-default-{}", uuid::Uuid::new_v4());
        let (outcome, _) = project_profile_launch(
            &sid,
            dir.path(),
            None,
            AgentCli::Claude,
            None,
            launch,
            |p| p.to_string(),
        )
        .unwrap();
        assert!(!outcome.extra_args.iter().any(|a| a == "-c"));
        remove_scope_config(&sid);
    }

    #[test]
    fn an_env_dir_scope_carries_the_users_config_minus_its_servers() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "model = \"gpt-5\"\nprofile = \"fast\"\n\n[profiles.fast]\nmodel = \"o4-mini\"\n\n\
             [projects.\"/work/repo\"]\ntrust_level = \"trusted\"\n\n\
             [mcp_servers.personal]\ncommand = \"personal-mcp\"\n\n\
             [mcp_servers.codesense]\ncommand = \"/old/bridge\"\n",
        )
        .unwrap();
        std::fs::write(home.path().join("auth.json"), b"{}").unwrap();
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cwd.path().join(".codex")).unwrap();
        std::fs::write(
            cwd.path().join(".codex/config.toml"),
            "[mcp_servers.slack]\ncommand = \"slack-mcp\"\n",
        )
        .unwrap();
        let scope = test_env_dir_scope("/nonexistent-default-home");
        let sid = format!("shell-carry-{}", uuid::Uuid::new_v4());
        let (outcome, _) = project_scope_with_creation(
            &sid,
            cwd.path(),
            &["slack".to_string()],
            Some(&scope),
            Some(home.path()),
            |p| p.to_string(),
        )
        .unwrap();
        let scoped_home = PathBuf::from(&outcome.extra_env[0].1);
        let config: toml::Value =
            toml::from_str(&std::fs::read_to_string(scoped_home.join("config.toml")).unwrap())
                .unwrap();
        assert_eq!(config["model"].as_str(), Some("gpt-5"));
        assert_eq!(config["profile"].as_str(), Some("fast"));
        assert_eq!(
            config["profiles"]["fast"]["model"].as_str(),
            Some("o4-mini")
        );
        assert_eq!(
            config["projects"]["/work/repo"]["trust_level"].as_str(),
            Some("trusted")
        );
        let servers = config["mcp_servers"].as_table().unwrap();
        assert!(
            servers.get("personal").is_none(),
            "the user's servers never widen the scope"
        );
        assert!(servers.get("slack").is_some());
        assert!(servers.get("codesense").is_some());
        assert_ne!(
            servers["codesense"]["command"].as_str(),
            Some("/old/bridge"),
            "the session's bridge is the projection's"
        );
        assert!(scoped_home.join("auth.json").exists());
        remove_scope_config(&sid);
    }

    #[test]
    fn an_env_dir_scope_with_no_user_config_is_the_projection_alone() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let scope = test_env_dir_scope("/nonexistent-default-home");
        let sid = format!("shell-carry-none-{}", uuid::Uuid::new_v4());
        let (outcome, _) = project_scope_with_creation(
            &sid,
            cwd.path(),
            &[],
            Some(&scope),
            Some(home.path()),
            |p| p.to_string(),
        )
        .unwrap();
        let scoped_home = PathBuf::from(&outcome.extra_env[0].1);
        let config: toml::Value =
            toml::from_str(&std::fs::read_to_string(scoped_home.join("config.toml")).unwrap())
                .unwrap();
        assert_eq!(
            config.as_table().unwrap().keys().collect::<Vec<_>>(),
            vec!["mcp_servers"]
        );
        remove_scope_config(&sid);
    }

    fn age_scope(dir: &Path) {
        std::fs::File::open(dir)
            .unwrap()
            .set_modified(SystemTime::now() - STALE_AGE - Duration::from_secs(60))
            .unwrap();
    }

    #[test]
    fn instance_scope_sweep_isolates_two_homes_with_shared_tmpdir() {
        let mut home = crate::routines::test_support::TempHome::new();
        let tmp = home.isolate_tmpdir();
        let other = home.other_mac("other", 2);
        let session_a = format!("instance-a-{}", uuid::Uuid::new_v4());
        let session_b = format!("instance-b-{}", uuid::Uuid::new_v4());
        let a = write_scope_config(&session_a, &canonical(), &[], &claude_scope()).unwrap();
        age_scope(a.parent().unwrap());
        home.act_as(&other);
        assert_eq!(std::env::temp_dir(), tmp);
        let b = write_scope_config(&session_b, &canonical(), &[], &claude_scope()).unwrap();
        age_scope(b.parent().unwrap());
        sweep_stale_configs(|_| Some(Default::default()));
        assert!(a.exists(), "home B must not reap home A's live scope");
        assert!(!b.exists(), "home B still reaps its own stale scope");
        let b_live = write_scope_config(&session_b, &canonical(), &[], &claude_scope()).unwrap();
        age_scope(b_live.parent().unwrap());
        home.act_as(home.root());
        sweep_stale_configs(|_| Some(std::collections::HashSet::from([session_a.clone()])));
        assert!(a.exists());
        assert!(b_live.exists(), "home A must not reap home B's live scope");
        remove_scope_config(&session_a);
    }

    #[test]
    fn instance_scope_sweep_requires_its_persisted_owner() {
        let mut home = crate::routines::test_support::TempHome::new();
        home.isolate_tmpdir();
        std::fs::write(home.codesense_home().join("pty-server-id"), "test-server").unwrap();
        let session = format!("instance-owned-{}", uuid::Uuid::new_v4());
        let path = write_scope_config(&session, &canonical(), &[], &claude_scope()).unwrap();
        let dir = path.parent().unwrap();
        let owner_path = dir.join("owner.json");
        let owner: Value = serde_json::from_slice(&std::fs::read(&owner_path).unwrap()).unwrap();
        assert_eq!(
            owner["instance_root"],
            home.codesense_home()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(
            owner["server_id"],
            std::fs::read_to_string(home.codesense_home().join("pty-server-id"))
                .unwrap()
                .trim()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&owner_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(dir.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        for replacement in [
            None,
            Some(
                serde_json::json!({"instance_root": "/other-instance", "server_id": owner["server_id"]}),
            ),
        ] {
            match replacement {
                Some(value) => {
                    std::fs::write(&owner_path, serde_json::to_vec(&value).unwrap()).unwrap()
                }
                None => std::fs::remove_file(&owner_path).unwrap(),
            }
            age_scope(dir);
            sweep_stale_configs(|_| Some(Default::default()));
            assert!(path.exists(), "an unconfirmed owner must be retained");
        }
        let mut stale_server = owner;
        stale_server["server_id"] = serde_json::json!("prior-server");
        std::fs::write(&owner_path, serde_json::to_vec(&stale_server).unwrap()).unwrap();
        age_scope(dir);
        sweep_stale_configs(|_| Some(Default::default()));
        assert!(!path.exists());
    }

    #[test]
    fn instance_scope_creation_does_not_mint_a_session_server_identity() {
        let mut home = crate::routines::test_support::TempHome::new();
        home.isolate_tmpdir();
        let session = format!("instance-no-server-{}", uuid::Uuid::new_v4());
        let path = write_scope_config(&session, &canonical(), &[], &claude_scope()).unwrap();
        assert!(!home.codesense_home().join("pty-server-id").exists());
        let owner: Value = serde_json::from_slice(
            &std::fs::read(path.parent().unwrap().join("owner.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(owner["server_id"], "");
        age_scope(path.parent().unwrap());
        sweep_stale_configs(|_| Some(Default::default()));
        assert!(
            !path.exists(),
            "missing diagnostic server identity does not prevent reaping"
        );
    }

    #[test]
    fn instance_own_stale_scope_is_still_reaped() {
        let mut home = crate::routines::test_support::TempHome::new();
        home.isolate_tmpdir();
        let session = format!("instance-stale-{}", uuid::Uuid::new_v4());
        let path = write_scope_config(&session, &canonical(), &[], &claude_scope()).unwrap();
        assert!(
            path.starts_with(home.codesense_home()),
            "creation must use the instance home"
        );
        age_scope(path.parent().unwrap());
        sweep_stale_configs(|_| Some(Default::default()));
        assert!(!path.exists());
    }

    #[test]
    fn instance_legacy_fallback_preserves_copyback_and_is_never_swept() {
        let mut home = crate::routines::test_support::TempHome::new();
        let tmp = home.isolate_tmpdir();
        let session = format!("instance-legacy-{}", uuid::Uuid::new_v4());
        let legacy = tmp.join("codesense-mcp-scope").join(&session);
        std::fs::create_dir_all(&legacy).unwrap();
        let source = home.root().join("auth.json");
        let candidate = legacy.join("auth.json");
        std::fs::write(&source, b"original").unwrap();
        std::fs::write(&candidate, b"original").unwrap();
        write_copyback_journal(&legacy, &source, &candidate).unwrap();
        std::fs::write(&candidate, b"refreshed").unwrap();
        assert_eq!(mcp_scope_session_dir(&session), legacy);
        age_scope(&legacy);
        sweep_stale_configs(|_| Some(Default::default()));
        assert!(legacy.exists(), "legacy folders must never be swept");
        remove_scope_config(&session);
        assert!(!legacy.exists());
        assert_eq!(std::fs::read(&source).unwrap(), b"refreshed");
        std::fs::create_dir_all(&legacy).unwrap();
        let path = write_scope_config(&session, &canonical(), &[], &claude_scope()).unwrap();
        assert!(path.starts_with(home.codesense_home()));
        assert_eq!(mcp_scope_session_dir(&session), path.parent().unwrap());
        remove_scope_config(&session);
        assert!(
            legacy.exists(),
            "new teardown must not touch the legacy folder"
        );
    }

    #[test]
    fn the_startup_sweep_keeps_a_live_sessions_scope_and_reaps_a_dead_ones() {
        let mut home = crate::routines::test_support::TempHome::new();
        home.isolate_tmpdir();
        let root = scope_root().unwrap();
        for session in ["live", "dead"] {
            let path = write_scope_config(session, &canonical(), &[], &claude_scope()).unwrap();
            std::fs::write(path.parent().unwrap().join("launch-settings.json"), b"{}").unwrap();
        }
        // Both are older than the cutoff.
        let later = SystemTime::now() + Duration::from_secs(2 * 3600);

        // A host that cannot tell leaves everything.
        sweep_configs_in(&root, later, STALE_AGE, |_| None);
        assert!(root.join("live").exists() && root.join("dead").exists());

        let mut asked = Vec::new();
        sweep_configs_in(&root, later, STALE_AGE, |sessions| {
            asked = sessions.iter().map(|s| s.to_string()).collect();
            Some(std::collections::HashSet::from(["live".to_string()]))
        });
        asked.sort();
        assert_eq!(asked, vec!["dead", "live"]);
        assert!(
            root.join("live/launch-settings.json").exists(),
            "a live session keeps it"
        );
        assert!(!root.join("dead").exists());

        // A young dir is never even asked about.
        sweep_configs_in(&root, SystemTime::now(), STALE_AGE, |sessions| {
            assert!(sessions.is_empty(), "{sessions:?}");
            Some(Default::default())
        });
        assert!(root.join("live").exists());
    }

    #[test]
    fn scope_config_path_is_outside_any_worktree() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let dir = mcp_scope_session_dir("shell-mcp-1-2");
        assert!(dir.starts_with(super::super::paths::codesense_home_dir().unwrap()));
        assert!(dir.to_string_lossy().contains("mcp-scope"));
    }

    #[test]
    fn builtin_descriptor_registers_the_bridge_key_guarding_const_drift() {
        // The projector force-includes `scope.bridge_id`, so a drift here would filter the
        // bridge OUT of every scoped launch (and it could never signal_done).
        let claude = descriptor(AgentCli::Claude)
            .mcp_scope
            .as_ref()
            .expect("built-in adapter declares mcp_scope");
        assert_eq!(claude.bridge_id, BRIDGE_SERVER_KEY);
        assert_eq!(claude.config_filename, ".mcp.json");
        assert_eq!(claude.server_map_key, "mcpServers");
        let codex = descriptor(AgentCli::Codex)
            .mcp_scope
            .as_ref()
            .expect("built-in Codex adapter declares mcp_scope");
        assert_eq!(codex.bridge_id, BRIDGE_SERVER_KEY);
    }

    #[test]
    fn tier_c_destructive_verb_has_no_surface_to_fire() {
        // Tier C (execute/destructive) has no verb surface — the classifier never
        // assigns it, so a destructive-shaped id classifies read-observe and, absent
        // from the canonical, never projects. There is nothing to admit and nothing
        // to fire.
        let canonical = json!({
            "mcpServers": {
                BRIDGE_SERVER_KEY: { "command": "/abs/bridge" },
                "read_file": { "command": "observe" },
            }
        });
        // A destructive shape a naive allowlist might carry. It is not advertised by
        // the canonical, so it cannot project regardless of tier.
        let allow = ["read_file".to_string(), "run_shell".to_string()];
        let eff = effective_mcp_servers(&canonical, &allow, &claude_scope());
        let servers = eff["mcpServers"].as_object().unwrap();
        assert!(
            !servers.contains_key("run_shell"),
            "a destructive verb has no surface — never in the effective set"
        );
        assert!(servers.contains_key("read_file"));
    }

    #[test]
    fn worker_and_triage_profiles_cannot_reach_queue_write_verbs() {
        // SW-WV-B2 AC-8 (TIER-DENY, seam 1): the allowlist projection is the
        // primary enforcement. Given a canonical server map that DOES advertise the
        // queue write verbs `wv_claim_queue` / `wv_resolve_queue`, a worker/triage
        // `mcp_allow` that omits them must yield an effective set that excludes both —
        // a profile lacking a verb cannot call it. Modelled as MCP servers so it
        // exercises the real `effective_mcp_servers` projection the launch path uses.
        // SW-WV-C2 AC-4 extends this fixture in-place: `wv_report_cycle` (the
        // mid-flight cycle report) is confined the same way as claim/resolve — by
        // `mcp_allow` omission from worker/triage, present on a profile that lists it.
        let canonical = json!({
            "mcpServers": {
                BRIDGE_SERVER_KEY: { "command": "/abs/bridge" },
                "wv_claim_queue": { "command": "allowlisted" },
                "wv_resolve_queue": { "command": "allowlisted" },
                "wv_report_cycle": { "command": "allowlisted" },
                "wv_queue_list": { "command": "read-only" }
            }
        });

        // A worker profile (Tier A) — read-only allowlist, no queue write verbs.
        let worker_allow = ["wv_queue_list".to_string()];
        let worker_eff = effective_mcp_servers(&canonical, &worker_allow, &claude_scope());
        let worker_servers = worker_eff["mcpServers"].as_object().unwrap();
        assert!(
            !worker_servers.contains_key("wv_claim_queue")
                && !worker_servers.contains_key("wv_resolve_queue")
                && !worker_servers.contains_key("wv_report_cycle"),
            "a worker profile must not reach the queue write verbs"
        );

        // A triage profile (Tier B, additive-write-to-wv_queue-only) — may enqueue
        // + list, but NOT claim/resolve/report-cycle.
        let triage_allow = ["wv_enqueue".to_string(), "wv_queue_list".to_string()];
        let triage_eff = effective_mcp_servers(&canonical, &triage_allow, &claude_scope());
        let triage_servers = triage_eff["mcpServers"].as_object().unwrap();
        assert!(
            !triage_servers.contains_key("wv_claim_queue")
                && !triage_servers.contains_key("wv_resolve_queue")
                && !triage_servers.contains_key("wv_report_cycle"),
            "the triage profile must not reach the claim/resolve/report verbs"
        );

        // A profile that IS allowlisted for all three does reach them —
        // proving the exclusion above is the allowlist doing its job, not the verbs
        // being absent from the canonical map.
        let allowed = [
            "wv_claim_queue".to_string(),
            "wv_resolve_queue".to_string(),
            "wv_report_cycle".to_string(),
        ];
        let allowed_eff = effective_mcp_servers(&canonical, &allowed, &claude_scope());
        let allowed_servers = allowed_eff["mcpServers"].as_object().unwrap();
        assert!(
            allowed_servers.contains_key("wv_claim_queue")
                && allowed_servers.contains_key("wv_resolve_queue")
                && allowed_servers.contains_key("wv_report_cycle"),
            "a profile allowlisted for the queue verbs reaches them"
        );
    }

    #[test]
    fn worker_and_triage_profiles_cannot_reach_b4_signals_coord_verbs() {
        // SW-WV-B4 AC-5 (TIER-DENY): extend B1's deny scope to the two new tables.
        // `wv_signal_drain` / `wv_coord_write` are INTERNAL Tauri commands (not MCP
        // verbs, not in EXPECTED_MCP_TOOLS), so no profile's `mcp_allow` carries them
        // and the effective server set never advertises them. SW-WV-D4 promoted
        // `wv_coord_read` to an MCP verb on the `codesense` bridge, so every agent
        // with the bridge can call it; it is no longer in this internal-deny set.
        // `wv_coord_write` stays internal
        // (write MCP exposure is deferred to SW-WV-C, out of D4 scope). Modelled
        // through the real `effective_mcp_servers` projection the launch path uses,
        // matching the sibling queue-verb deny test above.
        const B4_INTERNAL_VERBS: &[&str] = &["wv_signal_drain", "wv_coord_write"];
        let canonical = json!({
            "mcpServers": {
                BRIDGE_SERVER_KEY: { "command": "/abs/bridge" },
                "wv_signal_drain": { "command": "allowlisted" },
                "wv_coord_write": { "command": "allowlisted" },
                "wv_queue_list": { "command": "read-only" }
            }
        });

        // Worker (Tier A) and triage (Tier B, wv_queue-only) profiles: neither omits
        // a B4 internal verb by accident because neither allowlists one.
        for allow in [
            vec!["wv_queue_list".to_string()],
            vec!["wv_enqueue".to_string(), "wv_queue_list".to_string()],
        ] {
            let eff = effective_mcp_servers(&canonical, &allow, &claude_scope());
            let servers = eff["mcpServers"].as_object().unwrap();
            for verb in B4_INTERNAL_VERBS {
                assert!(
                    !servers.contains_key(*verb),
                    "no worker/triage profile may reach the B4 internal verb {verb:?} \
                     (mcp_allow {allow:?})"
                );
            }
        }
    }

    #[test]
    fn wv_triage_profile_excludes_peer_lane_verbs() {
        // SW-WV-D4 AC-5 (Test 2): the triage profile does NOT carry any peer-lane
        // verb — peer-to-peer coordination is not a triage concern. Triage's narrow
        // Tier B allowlist is Tier A observe + `wv_enqueue` only (mirrors D1).
        const PEER_LANE_VERBS: &[&str] = &["wv_coord_read", "wv_peer_read_file", "wv_escalate"];
        let canonical = json!({
            "mcpServers": {
                BRIDGE_SERVER_KEY: { "command": "/abs/bridge" },
                "wv_coord_read": { "command": "read-only" },
                "wv_peer_read_file": { "command": "read-only" },
                "wv_escalate": { "command": "narrow-tier-b" },
                "wv_enqueue": { "command": "triage-writable" }
            }
        });
        // The triage allowlist as D1 establishes it: observe + the sole queue-append.
        let triage_allow = vec!["wv_enqueue".to_string()];
        let eff = effective_mcp_servers(&canonical, &triage_allow, &claude_scope());
        let servers = eff["mcpServers"].as_object().unwrap();
        for verb in PEER_LANE_VERBS {
            assert!(
                !servers.contains_key(*verb),
                "triage profile must NOT reach peer-lane verb {verb:?}"
            );
        }
    }

    #[test]
    fn triage_profile_confines_dispatch_verbs_and_carries_enqueue() {
        // SW-WV-D1 AC-2 (narrow Tier B): the triage launch profile carries Tier A
        // observe tools + the ONE sanctioned queue-append write verb (`wv_enqueue`)
        // and NOTHING that dispatches or drives a worker. This is the load-bearing
        // proof for `routine_run_now` confinement (it is not in `INTERACTIVE_VERBS`, so
        // ONLY the `mcp_allow` omission excludes it).
        use crate::commands::interactive_gate::{GateRefusal, InteractiveGate};

        // A canonical server map advertising all three dispatch verbs plus the
        // triage-writable enqueue and the bridge — the worst case the projection
        // must confine.
        let canonical = json!({
            "mcpServers": {
                BRIDGE_SERVER_KEY: { "command": "/abs/bridge" },
                "routine_run_now": { "command": "allowlisted" },
                "send_to_session": { "command": "interactive-only" },
                "open_workspace": { "command": "interactive-only" },
                "wv_enqueue": { "command": "triage-writable" },
                "list_machines": { "command": "observe" },
                "routine_list": { "command": "observe" }
            }
        });

        // The narrow Tier B triage allowlist: Tier A observe tools + the sole write
        // verb. No dispatch/drive verb appears.
        let triage_allow = [
            "list_machines".to_string(),
            "routine_list".to_string(),
            "get_live_data_contract".to_string(),
            "wv_enqueue".to_string(),
        ];
        let eff = effective_mcp_servers(&canonical, &triage_allow, &claude_scope());
        let servers = eff["mcpServers"].as_object().unwrap();

        for verb in ["routine_run_now", "send_to_session", "open_workspace"] {
            assert!(
                !servers.contains_key(verb),
                "the triage profile must NOT reach the dispatch/drive verb {verb:?}"
            );
        }
        assert!(
            servers.contains_key("wv_enqueue"),
            "the triage profile MUST carry its sole write verb wv_enqueue"
        );
        assert!(
            servers.contains_key(BRIDGE_SERVER_KEY),
            "the bridge is force-included so triage can signal_done"
        );

        // Sub-test: `InteractiveGate` is a MUST-BE-ADMITTED check for interactive
        // sessions, NOT a deny-list. `send_to_session` / `open_workspace` ARE in
        // `INTERACTIVE_VERBS`, so the gate ADMITS them — the triage agent is denied
        // them solely by `mcp_allow` omission (proven above), never by the gate.
        assert!(
            InteractiveGate::admit("send_to_session").is_ok(),
            "the gate admits send_to_session — it is not a deny-list"
        );
        assert!(
            InteractiveGate::admit("open_workspace").is_ok(),
            "the gate admits open_workspace — it is not a deny-list"
        );

        // Sub-test: `wv_enqueue` is NOT in `INTERACTIVE_VERBS`, so the gate refuses
        // it (`NotGranted`) — confirming triage's enqueue access rides `mcp_allow`
        // only, never any interactive-gate admission.
        assert!(
            matches!(
                InteractiveGate::admit("wv_enqueue"),
                Err(GateRefusal::NotGranted(_))
            ),
            "wv_enqueue is not an interactive verb — triage reaches it via mcp_allow, not the gate"
        );
    }

    #[test]
    fn render_scope_flags_substitutes_path_and_is_quote_agnostic() {
        // AC-3: the ONE flag source. `{path}` is replaced with whatever the caller passes;
        // the caller owns quoting. A space-bearing path proves the dispatch site (which
        // passes a shell-quoted path for its verbatim-join builder) and the FE command
        // (which passes the raw path for its re-quoting builder) both render correctly.
        let template = [
            "--mcp-config".to_string(),
            "{path}".to_string(),
            "--strict-mcp-config".to_string(),
        ];
        let raw = render_scope_flags(&template, "/tmp/space dir/s.json");
        assert_eq!(
            raw,
            vec![
                "--mcp-config",
                "/tmp/space dir/s.json",
                "--strict-mcp-config"
            ],
            "raw path (FE contract): re-quoted downstream, so no quoting here"
        );
        let quoted = render_scope_flags(
            &template,
            &codesense_pty_core::shell_quote("/tmp/space dir/s.json"),
        );
        assert_eq!(
            quoted,
            vec![
                "--mcp-config",
                "'/tmp/space dir/s.json'",
                "--strict-mcp-config"
            ],
            "pre-quoted path (dispatch contract): verbatim-joined, so quoting is baked in"
        );
    }

    #[test]
    fn projection_honors_a_non_claude_descriptor() {
        // AC-2: a synthetic agent with a DIFFERENT filename, server-map key, and bridge id
        // projects correctly — the mechanism is agent-neutral, not Claude-shaped.
        let scope = McpScope {
            config_filename: "mcp.toml.json".to_string(),
            server_map_key: "servers".to_string(),
            bridge_id: "bridge".to_string(),
            bridge_entry: codesense_pty_core::agent_cli::McpBridgeEntry::Command,
            injection: ScopeInjection::ConfigFlag {
                flag_template: vec!["--config".to_string(), "{path}".to_string()],
                open_flag_template: None,
                settings_flag_template: None,
            },
            format: ConfigFragmentFormat::Json,
            bridge_override: None,
            inventory: None,
        };
        let canonical = json!({
            "servers": {
                "bridge": { "command": "/abs/bridge" },
                "acme": { "command": "acme-mcp" }
            }
        });
        let eff = effective_mcp_servers(&canonical, &["acme".into()], &scope);
        // Output is keyed by the descriptor's server-map key, and the descriptor's bridge
        // is force-included alongside the allowlisted server.
        let servers = eff["servers"].as_object().unwrap();
        assert_eq!(servers.len(), 2);
        assert!(
            servers.contains_key("bridge"),
            "descriptor bridge force-included"
        );
        assert!(servers.contains_key("acme"));
        assert!(eff.get("mcpServers").is_none(), "not keyed by Claude's key");
    }

    #[test]
    fn project_from_worktree_reads_the_descriptor_filename() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // AC-2: `project_from_worktree` joins the descriptor's `config_filename`, not a
        // hardcoded `.mcp.json` — a non-Claude agent reads its own file.
        let scope = McpScope {
            config_filename: "agent-mcp.json".to_string(),
            server_map_key: "servers".to_string(),
            bridge_id: "bridge".to_string(),
            bridge_entry: codesense_pty_core::agent_cli::McpBridgeEntry::Command,
            injection: ScopeInjection::ConfigFlag {
                flag_template: vec!["--config".to_string(), "{path}".to_string()],
                open_flag_template: None,
                settings_flag_template: None,
            },
            format: ConfigFragmentFormat::Json,
            bridge_override: None,
            inventory: None,
        };
        let dir = std::env::temp_dir().join(format!("an43-descfile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent-mcp.json"),
            br#"{"servers":{"bridge":{"command":"/b"},"acme":{"command":"a"}}}"#,
        )
        .unwrap();
        let sid = format!("shell-mcp-descfile-{}", std::process::id());
        let path = project_from_worktree(&sid, &dir, &["acme".into()], &scope).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(written["servers"].get("acme").is_some());
        assert!(written["servers"].get("bridge").is_some());
        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A Codex-shaped descriptor: TOML `config.toml` with an `[mcp_servers]` map.
    fn toml_scope() -> McpScope {
        McpScope {
            config_filename: "config.toml".to_string(),
            server_map_key: "mcp_servers".to_string(),
            bridge_id: "codesense".to_string(),
            bridge_entry: codesense_pty_core::agent_cli::McpBridgeEntry::Command,
            injection: ScopeInjection::ConfigFlag {
                flag_template: vec!["--config".to_string(), "{path}".to_string()],
                open_flag_template: None,
                settings_flag_template: None,
            },
            format: ConfigFragmentFormat::Toml,
            bridge_override: None,
            inventory: None,
        }
    }

    #[test]
    fn projection_reads_and_writes_toml_for_a_toml_descriptor() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // AC-1: a TOML-config agent reads an `[mcp_servers]` canonical file and writes a
        // correctly-formatted scoped `config.toml` (bridge + allowlisted, dropping the
        // rest) — through the shared config_format engine, no per-agent code. The
        // buildkite server carries an `env` sub-table to exercise TOML's values-before-
        // tables serialization ordering.
        let dir = std::env::temp_dir().join(format!("an45-toml-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            concat!(
                "[mcp_servers.codesense]\n",
                "command = \"/abs/bridge\"\n\n",
                "[mcp_servers.buildkite]\n",
                "command = \"bk\"\n",
                "args = [\"mcp\"]\n",
                "[mcp_servers.buildkite.env]\n",
                "TOKEN = \"secret\"\n\n",
                "[mcp_servers.playwright]\n",
                "command = \"npx\"\n",
            ),
        )
        .unwrap();
        let sid = format!("shell-mcp-toml-{}", std::process::id());
        let path = project_from_worktree(&sid, &dir, &["buildkite".into()], &toml_scope()).unwrap();

        // The scoped file re-parses as TOML → exactly the effective set.
        let written: Value = ConfigFormat::Toml
            .parse(&std::fs::read(&path).unwrap())
            .unwrap();
        let servers = written["mcp_servers"].as_object().unwrap();
        assert_eq!(servers.len(), 2, "bridge + buildkite only");
        assert!(servers.contains_key("codesense"), "bridge force-included");
        assert!(servers.contains_key("buildkite"), "allowlisted");
        assert!(
            !servers.contains_key("playwright"),
            "not allowlisted → dropped"
        );
        assert_eq!(written["mcp_servers"]["buildkite"]["command"], "bk");
        assert_eq!(
            written["mcp_servers"]["buildkite"]["env"]["TOKEN"], "secret",
            "server env sub-table survives the TOML round-trip verbatim"
        );
        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn claude_json_write_is_byte_identical_to_pre_format_serialization() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // AC-1 / AC-6: the JSON path is byte-for-byte what `serde_json::to_vec_pretty`
        // produced before `format` existed — Claude scoped configs are unchanged.
        let sid = format!("shell-mcp-jsoncmp-{}", std::process::id());
        let path =
            write_scope_config(&sid, &canonical(), &["buildkite".into()], &claude_scope()).unwrap();
        let got = std::fs::read(&path).unwrap();
        let expected = serde_json::to_vec_pretty(&effective_mcp_servers(
            &canonical(),
            &["buildkite".into()],
            &claude_scope(),
        ))
        .unwrap();
        assert_eq!(
            got, expected,
            "JSON scoped-config output must stay byte-identical to the pre-format path"
        );
        remove_scope_config(&sid);
    }

    #[test]
    fn project_scope_config_flag_returns_flags_and_no_env() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // AC-2: a `config_flag` adapter yields rendered flags (path quoted per the caller) and
        // NO env — the scoped config is delivered by flag, exactly as Claude does today.
        let dir = std::env::temp_dir().join(format!("an45-cf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".mcp.json"),
            serde_json::to_vec_pretty(&canonical()).unwrap(),
        )
        .unwrap();
        let sid = format!("shell-mcp-cf-{}", std::process::id());
        let outcome = project_scope(
            &sid,
            &dir,
            &["buildkite".into()],
            Some(&claude_scope()),
            |p| p.to_string(),
        )
        .unwrap();
        assert!(outcome.extra_env.is_empty(), "config_flag yields no env");
        assert_eq!(outcome.extra_args[0], "--mcp-config");
        assert!(
            outcome.extra_args[1].ends_with(".mcp.json"),
            "middle token is the scoped config path"
        );
        assert_eq!(outcome.extra_args[2], "--strict-mcp-config");
        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_scope_none_yields_empty_outcome() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // A resolving adapter that declares no mcp_scope → launch unfiltered: no flags, no env,
        // no file written (the projector is never reached).
        let outcome = project_scope(
            "shell-mcp-none",
            Path::new("/nonexistent"),
            &["x".into()],
            None,
            |p| p.to_string(),
        )
        .unwrap();
        assert_eq!(outcome, ScopeOutcome::default());
    }

    #[test]
    fn failed_passthrough_rolls_back_credential_scope() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let base = credential_test_dir("projection-rollback");
        let cwd = base.join("wt");
        let home = base.join("home");
        std::fs::create_dir_all(cwd.join(".codex")).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(
            cwd.join(".codex/config.toml"),
            "[mcp_servers.codesense]\ncommand = \"bridge\"\n",
        )
        .unwrap();
        std::fs::write(home.join("other.json"), b"secret").unwrap();
        let mut scope = test_env_dir_scope(&home.to_string_lossy());
        if let ScopeInjection::EnvDir { passthrough, .. } = &mut scope.injection {
            *passthrough = vec![Passthrough {
                name: "other.json".into(),
                mode: PassthroughMode::Copy,
            }];
        }
        let sid = format!("scope-rollback-{}", rand::random::<u64>());
        assert!(project_scope(&sid, &cwd, &[], Some(&scope), |p| p.to_string()).is_err());
        assert!(!mcp_scope_session_dir(&sid).exists());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn failed_config_write_removes_partial_secret_file_and_owned_directory() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use std::io::Write;

        let scope = claude_scope();
        let sid = format!("scope-write-fault-{}", rand::random::<u64>());
        let result = write_scope_config_bytes(
            &sid,
            &scope,
            br#"{"mcpServers":{"secret":{"env":{"TOKEN":"sensitive"}}}}"#,
            |path, bytes| {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)
                    .map_err(|error| error.to_string())?;
                file.write_all(&bytes[..16])
                    .map_err(|error| error.to_string())?;
                Err("injected write failure".into())
            },
        );
        assert!(result.is_err());
        assert!(!mcp_scope_session_dir(&sid).exists());
    }

    #[test]
    fn project_scope_env_dir_writes_dir_symlinks_auth_and_returns_env() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // AC-2: an `env_dir` adapter projects the scoped config INTO a per-session dir, brings
        // the declared passthrough (auth.json) in as a symlink from the agent's real home,
        // and returns (env_var, dir) with NO flags. This is the Codex vector, generically.
        let base = std::env::temp_dir().join(format!("an45-envdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // fake worktree with a canonical `.codex/config.toml` (a subpath descriptor).
        let cwd = base.join("wt");
        std::fs::create_dir_all(cwd.join(".codex")).unwrap();
        std::fs::write(
            cwd.join(".codex/config.toml"),
            "[mcp_servers.codesense]\ncommand = \"/abs/bridge\"\n\n\
             [mcp_servers.acme]\ncommand = \"acme\"\n",
        )
        .unwrap();
        // fake real home holding the credential to pass through.
        let home = base.join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("auth.json"), b"{\"token\":\"x\"}").unwrap();

        let scope = test_env_dir_scope(&home.to_string_lossy());
        let sid = format!("shell-mcp-envdir-{}", std::process::id());
        let outcome = project_scope(&sid, &cwd, &["acme".into()], Some(&scope), |p| {
            p.to_string()
        })
        .unwrap();

        assert!(outcome.extra_args.is_empty(), "env_dir yields no flags");
        assert_eq!(outcome.extra_env.len(), 1);
        let (var, dir_str) = &outcome.extra_env[0];
        assert_eq!(var, "AN45_TEST_HOME_UNSET");
        let dir = PathBuf::from(dir_str);
        assert_eq!(
            dir,
            mcp_scope_session_dir(&sid),
            "env points at the session dir"
        );

        // The scoped config.toml is inside the dir with the effective set.
        let cfg: Value = ConfigFormat::Toml
            .parse(&std::fs::read(dir.join("config.toml")).unwrap())
            .unwrap();
        let servers = cfg["mcp_servers"].as_object().unwrap();
        assert!(servers.contains_key("codesense") && servers.contains_key("acme"));

        // auth.json is brought in as a symlink to the real home's file (no secret copied).
        let link = dir.join("auth.json");
        assert!(link.exists(), "passthrough brought in");
        #[cfg(unix)]
        {
            let meta = std::fs::symlink_metadata(&link).unwrap();
            assert!(
                meta.file_type().is_symlink(),
                "auth.json symlinked, not copied"
            );
            assert_eq!(std::fs::read_link(&link).unwrap(), home.join("auth.json"));
        }
        remove_scope_config(&sid);
        assert!(
            !mcp_scope_session_dir(&sid).exists(),
            "teardown removes the whole session dir"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn explicit_selected_home_wins_over_default_home_for_codex_credentials() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let base = credential_test_dir("selected-account-source");
        let cwd = base.join("wt");
        let default_home = base.join("default-home");
        let selected_home = base.join("selected-home");
        std::fs::create_dir_all(cwd.join(".codex")).unwrap();
        std::fs::create_dir_all(&default_home).unwrap();
        std::fs::create_dir_all(&selected_home).unwrap();
        std::fs::write(
            cwd.join(".codex/config.toml"),
            "[mcp_servers.codesense]\ncommand = \"/abs/bridge\"\n",
        )
        .unwrap();
        std::fs::write(default_home.join("auth.json"), b"default-account").unwrap();
        std::fs::write(selected_home.join("auth.json"), b"selected-account").unwrap();

        let mut scope = test_env_dir_scope(&default_home.to_string_lossy());
        let ScopeInjection::EnvDir { passthrough, .. } = &mut scope.injection else {
            unreachable!("test descriptor is env-dir based")
        };
        passthrough[0].mode = PassthroughMode::Copy;
        let sid = format!("shell-selected-account-{}", rand::random::<u64>());
        let outcome = project_scope_with_home(
            &sid,
            &cwd,
            &[],
            Some(&scope),
            Some(&selected_home),
            |path| path.to_string(),
        )
        .unwrap();
        let scoped_home = PathBuf::from(&outcome.extra_env[0].1);

        assert_eq!(
            std::fs::read(scoped_home.join("auth.json")).unwrap(),
            b"selected-account",
            "headed and headless account-aware projection must source the selected managed home",
        );
        assert_ne!(
            std::fs::read(scoped_home.join("auth.json")).unwrap(),
            std::fs::read(default_home.join("auth.json")).unwrap(),
        );
        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn project_scope_env_dir_skips_absent_passthrough() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // A missing passthrough source is a warn+skip, not a launch failure — the agent may
        // authenticate another way. The scoped config is still written and the env returned.
        let base = std::env::temp_dir().join(format!("an45-nopass-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let cwd = base.join("wt");
        std::fs::create_dir_all(cwd.join(".codex")).unwrap();
        std::fs::write(
            cwd.join(".codex/config.toml"),
            "[mcp_servers.codesense]\ncommand = \"/b\"\n",
        )
        .unwrap();
        let home = base.join("empty-home"); // exists, but has no auth.json
        std::fs::create_dir_all(&home).unwrap();

        let scope = test_env_dir_scope(&home.to_string_lossy());
        let sid = format!("shell-mcp-nopass-{}", std::process::id());
        let outcome = project_scope(&sid, &cwd, &[], Some(&scope), |p| p.to_string()).unwrap();

        let dir = mcp_scope_session_dir(&sid);
        assert!(
            dir.join("config.toml").exists(),
            "scoped config still written"
        );
        assert!(
            !dir.join("auth.json").exists(),
            "absent passthrough skipped, never fabricated"
        );
        assert_eq!(outcome.extra_env.len(), 1, "still returns the env dir");
        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn project_scope_env_dir_skips_non_file_passthrough_source() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // Review P3: a passthrough source that is a DIRECTORY (or a dangling symlink) is
        // skipped like an absent one — never an EISDIR hard failure or a symlinked dir.
        let base = std::env::temp_dir().join(format!("an45-dirpass-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let cwd = base.join("wt");
        std::fs::create_dir_all(cwd.join(".codex")).unwrap();
        std::fs::write(
            cwd.join(".codex/config.toml"),
            "[mcp_servers.codesense]\ncommand = \"/b\"\n",
        )
        .unwrap();
        let home = base.join("home");
        // auth.json is a DIRECTORY, not a regular file.
        std::fs::create_dir_all(home.join("auth.json")).unwrap();

        let scope = test_env_dir_scope(&home.to_string_lossy());
        let sid = format!("shell-mcp-dirpass-{}", std::process::id());
        let outcome = project_scope(&sid, &cwd, &[], Some(&scope), |p| p.to_string()).unwrap();
        let dir = mcp_scope_session_dir(&sid);
        assert!(dir.join("config.toml").exists(), "config still written");
        assert!(
            !dir.join("auth.json").exists(),
            "non-file passthrough source skipped, never linked/copied"
        );
        assert_eq!(outcome.extra_env.len(), 1);
        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn expand_home_only_expands_tilde_slash_forms() {
        let _home_env = crate::routines::test_support::TempHome::new();
        // Review P6: `~` and `~/x` expand to HOME; `~user` and non-tilde paths stay verbatim
        // (so `~bob/.codex` never mis-joins onto our own home).
        let home = dirs::home_dir().expect("home dir");
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("~/.codex"), home.join(".codex"));
        assert_eq!(expand_home("~bob/.codex"), PathBuf::from("~bob/.codex"));
        assert_eq!(expand_home("/abs/.codex"), PathBuf::from("/abs/.codex"));
    }

    /// SW-AN-45 AC-5 — LIVE SMOKE (ignored: needs `codex` installed + logged in). Drives the
    /// REAL `codex` binary through the ACTUAL Codex adapter + `project_scope` (env_dir):
    /// projects a scoped `CODEX_HOME`, then `codex mcp list` under it must show ONLY the
    /// allowlisted server + the bridge (not the dropped one), and `codex login status` must be
    /// authenticated via the symlinked `auth.json`. Run with:
    ///   `cargo test --lib live_smoke_codex_scoped -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_smoke_codex_scoped_via_env_dir() {
        let _home_env = crate::routines::test_support::TempHome::new();
        use std::process::Command;
        let scope = descriptor(AgentCli::Codex)
            .mcp_scope
            .as_ref()
            .expect("codex declares mcp_scope");

        // A fake worktree whose canonical `.codex/config.toml` lists the bridge + two servers.
        let base = std::env::temp_dir().join(format!("an45-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join(".codex")).unwrap();
        std::fs::write(
            base.join(".codex/config.toml"),
            "[mcp_servers.codesense]\ncommand = \"/bin/echo\"\n\n\
             [mcp_servers.alpha]\ncommand = \"/bin/echo\"\nargs = [\"alpha\"]\n\n\
             [mcp_servers.beta]\ncommand = \"/bin/echo\"\nargs = [\"beta\"]\n",
        )
        .unwrap();

        // Project scoped to allow=["alpha"] through the real env_dir vector.
        let sid = format!("shell-an45-live-{}", std::process::id());
        let outcome = project_scope(&sid, &base, &["alpha".into()], Some(scope), |p| {
            p.to_string()
        })
        .expect("project_scope (env_dir) succeeds");
        let (var, dir) = outcome
            .extra_env
            .first()
            .expect("env_dir returns an env pair");
        assert_eq!(var, "CODEX_HOME");

        // `codex mcp list` under the scoped home: ONLY bridge + alpha, beta dropped.
        let out = Command::new("codex")
            .args(["mcp", "list", "--json"])
            .env(var, dir)
            .output()
            .expect("run codex mcp list");
        let servers: Value = serde_json::from_slice(&out.stdout).expect("codex mcp list --json");
        let names: Vec<&str> = servers
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"codesense"), "bridge visible: {names:?}");
        assert!(names.contains(&"alpha"), "allowlisted visible: {names:?}");
        assert!(
            !names.contains(&"beta"),
            "non-allowlisted dropped: {names:?}"
        );

        // Auth passthrough: the symlinked auth.json authenticates under the scoped home.
        // (codex may write the status to stdout or stderr, so check both.)
        let auth = Command::new("codex")
            .args(["login", "status"])
            .env(var, dir)
            .output()
            .expect("run codex login status");
        let auth_out = format!(
            "{}{}",
            String::from_utf8_lossy(&auth.stdout),
            String::from_utf8_lossy(&auth.stderr)
        );
        assert!(
            auth_out.contains("Logged in"),
            "symlinked auth.json authenticates under the scoped CODEX_HOME; got: {auth_out}"
        );

        remove_scope_config(&sid);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn credential_copyback_commits_only_when_authority_is_unchanged() {
        let base = credential_test_dir("credential-copyback");
        let source_home = base.join("source");
        let scope_home = base.join("scope");
        std::fs::create_dir_all(&source_home).unwrap();
        std::fs::create_dir_all(&scope_home).unwrap();
        let source = source_home.join("auth.json");
        let candidate = scope_home.join("auth.json");
        std::fs::write(&source, b"old").unwrap();
        bring_in_copy(&source, &candidate).unwrap();
        write_copyback_journal(&scope_home, &source, &candidate).unwrap();
        std::fs::write(&candidate, b"refreshed").unwrap();

        finish_credential_copyback(&scope_home).unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), b"refreshed");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn credential_copyback_preserves_candidate_on_concurrent_source_change() {
        let base = credential_test_dir("credential-conflict");
        let source_home = base.join("source");
        let scope_home = base.join("scope");
        std::fs::create_dir_all(&source_home).unwrap();
        std::fs::create_dir_all(&scope_home).unwrap();
        let source = source_home.join("auth.json");
        let candidate = scope_home.join("auth.json");
        std::fs::write(&source, b"old").unwrap();
        bring_in_copy(&source, &candidate).unwrap();
        write_copyback_journal(&scope_home, &source, &candidate).unwrap();
        std::fs::write(&source, b"other-session").unwrap();
        std::fs::write(&candidate, b"this-session").unwrap();

        assert!(finish_credential_copyback(&scope_home)
            .unwrap_err()
            .contains("credential_write_conflict"));
        assert_eq!(std::fs::read(&source).unwrap(), b"other-session");
        let recovery = source_home.join(".codesense-credential-recovery");
        assert_eq!(std::fs::read_dir(recovery).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(base);
    }

    /// LIVE SMOKE (ignored: needs `codex` on PATH). The bundled Codex default profile's
    /// resolved argv, handed to the real binary under an empty throwaway `CODEX_HOME`, must
    /// list the codesense bridge — the user's own Codex config is never read or written. Run:
    ///   `cargo test --lib live_smoke_codex_default_profile_lists_bridge -- --ignored`
    #[test]
    #[ignore]
    fn live_smoke_codex_default_profile_lists_bridge() {
        let home = tempfile::TempDir::new().unwrap();
        let bridge = std::path::Path::new("/opt/codesense/bin/codesense-bridge");
        let codex = descriptor(AgentCli::Codex);
        let flags = codex
            .mcp_scope
            .as_ref()
            .and_then(|scope| scope.bridge_override.as_ref())
            .and_then(|over| over.render(bridge))
            .expect("codex registers the bridge with launch flags");
        let out = std::process::Command::new(&codex.binary)
            .args(&flags)
            .args(["mcp", "list", "--json"])
            .env("CODEX_HOME", home.path())
            .output()
            .expect("run codex mcp list");
        assert!(
            out.status.success(),
            "codex mcp list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let servers: serde_json::Value =
            serde_json::from_slice(&out.stdout).expect("codex mcp list --json");
        let bridge_entry = servers
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "codesense")
            .unwrap_or_else(|| panic!("bridge listed: {servers}"));
        assert_eq!(
            bridge_entry["transport"]["command"],
            serde_json::json!(bridge.to_str().unwrap()),
            "{bridge_entry}"
        );
    }
    fn managed_gateway_capture_in_orders(orders: &[&str]) {
        use std::io::BufRead;
        use std::os::unix::{fs::symlink, process::CommandExt};
        use std::process::{Command, Stdio};
        struct Reap(std::process::Child);
        impl Drop for Reap {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let _home = crate::routines::test_support::TempHome::new();
        for &order in orders {
            let session = format!("managed-stop-{order}-{}", uuid::Uuid::new_v4());
            let scope = descriptor(AgentCli::Hermes).mcp_scope.as_ref().unwrap();
            let (config, creation) =
                write_scope_config_with_creation(&session, &canonical(), &[], scope).unwrap();
            let home = config.parent().unwrap();
            let runtime = _home.root().join(format!("runtime-{order}"));
            std::fs::create_dir_all(&runtime).unwrap();
            symlink(
                &runtime,
                home.join(codesense_pty_core::agent_cli::version_cache::RUNTIME_LINK),
            )
            .unwrap();
            let checkpoint = runtime.join("checkpoint");
            std::fs::create_dir_all(home.join("sessions")).unwrap();
            // Reuse the existing final-capture fixture; Hermes DB parsing is SV-ENG-109.
            let record = codex_cleanup_source(&session, home);
            if order != "exit-first" {
                persist_cleanup_record(&record);
            }
            let code = r#"import fcntl,json,os,pathlib,signal,sys,time
h=pathlib.Path(sys.argv[1]); checkpoint=pathlib.Path(sys.argv[2])
f=open(h/'gateway.lock','w+'); json.dump({'pid':os.getpid()},f); f.flush(); fcntl.flock(f,fcntl.LOCK_EX)
(h/'gateway.pid').write_text(json.dumps({'pid':os.getpid()}))
def stop(*args):
 final={'timestamp':'2026-10-27T10:00:12.000Z','type':'response_item','payload':{'type':'message','role':'assistant','content':[{'type':'output_text','text':'gateway final checkpoint written by TERM'}]}}
 with next(h.glob('sessions/**/rollout-*.jsonl')).open('a') as stream:
  stream.write(json.dumps(final)+'\n'); stream.flush(); os.fsync(stream.fileno())
 checkpoint.write_text('stopped-before-capture'); sys.exit(0)
signal.signal(signal.SIGTERM,stop)
print('ready',flush=True)
while True: time.sleep(.05)
"#;
            let child = Command::new("python3")
                .args(["-c", code])
                .arg(home)
                .arg(&checkpoint)
                .args(["hermes_cli.main", "gateway", "run"])
                .process_group(0)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut gateway = Reap(child);
            let mut ready = String::new();
            std::io::BufReader::new(gateway.0.stdout.take().unwrap())
                .read_line(&mut ready)
                .unwrap();
            assert_eq!(ready.trim(), "ready");
            let owner = exact_cleanup_claim(&session);
            if order == "record-first" {
                bind_scope_creation(&creation, &owner).unwrap();
            }
            match order {
                "record-first" => assert_eq!(settle_owned_scope(&owner), Ok(true)),
                "exit-first" => {
                    remove_scope_config(&session);
                    assert!(
                        home.exists(),
                        "late session record retains the capture source"
                    );
                    assert!(
                        checkpoint.exists(),
                        "gateway is already stopped while waiting for the record"
                    );
                    persist_cleanup_record(&record);
                    remove_scope_config(&session);
                }
                _ => {
                    drop(
                        crate::session_usage::UsageStore::open(
                            &crate::session_usage::store_path().unwrap(),
                        )
                        .unwrap(),
                    );
                    sweep_configs_in(
                        &scope_root().unwrap(),
                        SystemTime::now() + STALE_AGE + Duration::from_secs(60),
                        STALE_AGE,
                        |_| Some(Default::default()),
                    );
                }
            }
            assert!(
                checkpoint.exists(),
                "{order}: stop runs before home removal"
            );
            assert!(gateway.0.wait().unwrap().success());
            assert!(!home.exists(), "{order}: capture finishes before removal");
            assert_cleanup_conversation(&session);
            let stored = crate::session_conversation::read_stored(
                &crate::session_conversation::conversations_dir().unwrap(),
                &session,
            )
            .unwrap();
            assert!(stored.transcript.turns.iter().any(|turn| turn.role == "assistant"
                && turn.text.as_deref() == Some("gateway final checkpoint written by TERM")),
                "{order}: capture must retain the record appended during TERM, not merely pre-existing turns");
        }
    }
    #[test]
    fn managed_gateway_final_capture_survives_normal_cleanup_in_both_record_orders() {
        managed_gateway_capture_in_orders(&["record-first", "exit-first"]);
    }

    #[test]
    fn managed_gateway_final_capture_survives_restart_cleanup() {
        managed_gateway_capture_in_orders(&["restart"]);
    }

    #[test]
    fn carry_home_true_preserves_the_codex_projection_bytes() {
        let _home = crate::routines::test_support::TempHome::new();
        let source = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let scope = test_env_dir_scope(&source.path().to_string_lossy());
        std::fs::write(source.path().join("config.toml"),
            "model = 'saved-model'\n[projects.'/project']\ntrust_level = 'trusted'\n[mcp_servers.personal]\ncommand = 'must-not-carry'\n").unwrap();
        let original = canonical();
        std::fs::write(
            cwd.path().join(".mcp.json"),
            serde_json::to_vec(&original).unwrap(),
        )
        .unwrap();
        let session = format!("codex-byte-regression-{}", uuid::Uuid::new_v4());
        let (_, creation) = project_scope_for_cli(
            &session,
            cwd.path(),
            &[],
            None,
            Some(&scope),
            Some(source.path()),
            None,
            None,
            str::to_string,
        )
        .unwrap();
        let actual = std::fs::read(mcp_scope_session_dir(&session).join("config.toml")).unwrap();
        // Reconstruct the previous writer's exact input and format, including trust.
        let mut expected = ConfigFormat::Toml
            .parse(&std::fs::read(source.path().join("config.toml")).unwrap())
            .unwrap();
        expected.as_object_mut().unwrap().remove("mcp_servers");
        let canonical = canonical_from_worktree(
            cwd.path(),
            None,
            &scope,
            super::super::deps::bridge_symlink_path().as_deref(),
            Some(&session),
        )
        .unwrap();
        expected.as_object_mut().unwrap().extend(
            effective_mcp_servers(&canonical, &[], &scope)
                .as_object()
                .unwrap()
                .clone(),
        );
        assert_eq!(actual, ConfigFormat::Toml.serialize(&expected).unwrap());
        drop(creation);
        remove_scope_config(&session);
    }
    #[test]
    fn managed_preparation_and_teardown_are_serialized_in_both_orders() {
        let _home = crate::routines::test_support::TempHome::new();
        let session = format!("prepare-race-{}", uuid::Uuid::new_v4());
        let (config, creation) =
            write_scope_config_with_creation(&session, &canonical(), &[], &claude_scope()).unwrap();
        std::thread::scope(|threads| {
            let (entered, started) = std::sync::mpsc::channel();
            let (release, released) = std::sync::mpsc::channel();
            let creation = &creation;
            let config = &config;
            let worker = threads.spawn(move || {
                prepare_created_scope(creation, || {
                    entered.send(()).unwrap();
                    released.recv().unwrap();
                    assert!(config.exists(), "cleanup cannot remove a preparing scope");
                    Ok(())
                })
            });
            started.recv().unwrap();
            let (done, finished) = std::sync::mpsc::channel();
            let cleanup_session = &session;
            let cleanup = threads.spawn(move || {
                remove_scope_config(cleanup_session);
                done.send(()).unwrap();
            });
            let finished_early = finished.recv_timeout(Duration::from_millis(250)).is_ok();
            release.send(()).unwrap();
            worker.join().unwrap().unwrap();
            cleanup.join().unwrap();
            assert!(
                !finished_early,
                "teardown must wait for preparation's exact resource slot"
            );
        });
        assert!(!config.exists());
        assert!(prepare_created_scope(&creation, || -> Result<(), String> {
            panic!("removed scope must not recreate files")
        })
        .is_err());
    }
}

#[cfg(test)]
mod local_relay_lifetime_tests {
    use super::*;
    #[test]
    fn abandoned_preparation_expires_but_detached_lease_is_explicit_and_revocable() {
        let mut record = ScopeRecord {
            creation: ScopeCreation {
                session: "fixture".into(),
                stamp: 1,
            },
            identity: (0, 0),
            owner: None,
            relay_enabled: true,
            relay_retired: false,
            prepared_at: std::time::Instant::now()
                - RELAY_PREPARE_LIFETIME
                - Duration::from_secs(1),
            detached_lease: None,
        };
        assert!(!relay_admitted(&record));
        let lease = std::sync::Arc::new(());
        record.detached_lease = Some(std::sync::Arc::downgrade(&lease));
        assert!(relay_admitted(&record));
        drop(lease);
        assert!(!relay_admitted(&record));
        record.relay_enabled = false;
        assert!(
            relay_admitted(&record),
            "legacy config lifetimes are unchanged"
        );
    }
    #[test]
    fn root_codex_option_precedes_exec_and_scope_configuration_stays_last() {
        assert_eq!(
            compose_args(
                vec!["exec".into(), "-c".into(), "x=1".into()],
                vec!["--no-daemon".into(), "-c".into(), "x=2".into()]
            ),
            ["--no-daemon", "exec", "-c", "x=1", "-c", "x=2"]
        );
    }
}
