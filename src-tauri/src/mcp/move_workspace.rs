// SPDX-License-Identifier: MIT
//! Move a workspace to another machine. `move_workspace` is the one
//! verb that mutates a workspace's `location.machineId`; the 7+ "stays where
//! it was created" refusal sites keep their predicates and observe the new
//! value naturally on next read.
//!
//! State is persisted at `~/.codesense/users/{uid}/move-workspace-intents-v1/
//! {sha256(team_key + workspace_id)}.json`, keyed by `(team_key, workspace_id)`
//! so at most one move is in flight per workspace. The three-step pipeline
//! (`preparing-target` → `swapping-location` → `cleaning-source`) is durable:
//! each step advances the on-disk file atomically before touching foreign
//! state. A boot reaper advances every open intent to a terminal state on
//! next launch; see `reap_move_workspace_intents`.
use log::warn;
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Serializer / lock around `move-workspace-intents-v1/*.json` writes so a
/// racing boot reaper and a user-driven `move_workspace` call never shred
/// each other.
static INTENT_WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

// Architecture: only one in-process run may inspect or advance a team's workspace move.
static EXECUTING_MOVES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashSet<(String, String)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

#[derive(Debug)]
struct ExecutingMove((String, String));

impl ExecutingMove {
    fn acquire(team: &str, workspace: &str) -> Result<Self, String> {
        let key = (team.to_owned(), workspace.to_owned());
        if !EXECUTING_MOVES
            .lock()
            .map_err(|_| "move execution lock poisoned")?
            .insert(key.clone())
        {
            return Err(PreflightRefusal::InProgress.to_string());
        }
        Ok(Self(key))
    }
}

impl Drop for ExecutingMove {
    fn drop(&mut self) {
        if let Ok(mut executing) = EXECUTING_MOVES.lock() {
            executing.remove(&self.0);
        }
    }
}

/// Max bytes we're willing to read for one intent file. Matches the
/// `remote-workspace-intents-v1` cap: a corrupt oversized file is log-and-skip,
/// never a parse attempt.
const INTENT_MAX_BYTES: u64 = 1024 * 1024;

const INTENT_DIR_NAME: &str = "move-workspace-intents-v1";

/// Where a workspace lives (or is moving from/to). `None` is This Mac, same
/// convention as `WorkspacePlace::ThisMac`. Serialized as `null` so JSON and
/// Rust agree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct MachineRef(pub Option<String>);

impl MachineRef {
    pub fn is_local(&self) -> bool {
        self.0.is_none() || self.0.as_deref() == Some(crate::commands::machines::LOCAL_MACHINE_ID)
    }

    /// Normalized machine id for `prepare_remote_workspace` style callers that
    /// take a `machine_id: String`. Local → `"local"`.
    pub fn as_machine_id(&self) -> &str {
        self.0
            .as_deref()
            .unwrap_or(crate::commands::machines::LOCAL_MACHINE_ID)
    }
}

/// The source side of a move — carries enough state to roll back or clean up
/// after the swap lands. Resume paths read `repo_path` + `repository_key`
/// out of here so a restart that closed the workspace tile can still drive
/// cleanup via `remove_worktree`/`git worktree remove`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveSource {
    #[serde(rename = "machineId")]
    pub machine_id: MachineRef,
    pub request_id: String,
    pub handle: Option<String>,
    pub cwd: Option<String>,
    /// Absolute source repo path — the dir `git worktree remove` runs from
    /// (local) and the ownership-marker key for the remote RPC. Captured at
    /// move-start so resume works without the viewer snapshot.
    #[serde(default)]
    pub repo_path: String,
    /// `SourceRepoSlug::remote_key()` — the ownership-marker's second half.
    #[serde(default)]
    pub repository_key: String,
}

/// The target side of a move. `request_id` is minted fresh when the intent is
/// written and is the key `prepare_remote_workspace` / `local_prepare_workspace`
/// use for idempotency on retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveTarget {
    #[serde(rename = "machineId")]
    pub machine_id: MachineRef,
    pub request_id: String,
}

/// Pipeline step. Advancing a step is a durable transition — the intent file
/// is rewritten atomically before any foreign state is touched.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum MoveStep {
    PreparingTarget,
    SwappingLocation,
    CleaningSource,
}

/// On-disk intent. Optional verification fields let older intents remain readable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveIntent {
    pub team_key: String,
    pub workspace_id: String,
    pub from: MoveSource,
    pub to: MoveTarget,
    pub step: MoveStep,
    pub dirty_allowed: bool,
    /// Architecture: cleanup, including boot recovery, requires durable proof.
    #[serde(default)]
    pub source_tip: Option<String>,
    #[serde(default)]
    pub source_repo: Option<SourceRepoSlug>,
    #[serde(default)]
    pub expected_tip: Option<String>,
    #[serde(default)]
    pub target_cwd: Option<String>,
    #[serde(default)]
    pub target_handle: Option<String>,
    #[serde(default)]
    pub failure: Option<String>,
    /// Architecture: retain evidence of commits left on the source branch after removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_warning: Option<String>,
    /// Architecture: source kept requires a fresh post-step presence confirmation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source_kept: bool,
    pub started_at_ms: u64,
    /// Wall-clock epoch ms (NEVER `Instant::now()` — `Instant` is monotonic
    /// from process start and does not survive restart; the reaper would see
    /// every intent as "0 ms since progress" on reboot). Bumped from the
    /// `ProgressSink` wrapper during step 1 and when the pipeline advances a
    /// step.
    pub last_progress_at_ms: u64,
}

impl MoveIntent {
    /// New intent at `PreparingTarget` with both timestamps = now.
    pub fn new(
        team_key: String,
        workspace_id: String,
        from: MoveSource,
        to: MoveTarget,
        dirty_allowed: bool,
    ) -> Self {
        let now = now_epoch_ms();
        Self {
            team_key,
            workspace_id,
            from,
            to,
            step: MoveStep::PreparingTarget,
            dirty_allowed,
            source_tip: None,
            source_repo: None,
            expected_tip: None,
            target_cwd: None,
            target_handle: None,
            failure: None,
            cleanup_warning: None,
            source_kept: false,
            started_at_ms: now,
            last_progress_at_ms: now,
        }
    }

    /// The stable filesystem key — `sha256(team_key + workspace_id)` as hex.
    /// Keyed by `(team_key, workspace_id)` so at most one move is in flight
    /// per workspace; a second concurrent `move_workspace` call refuses
    /// `in_progress` on `create_new`.
    pub fn file_key(team_key: &str, workspace_id: &str) -> String {
        format!(
            "{:x}",
            Sha256::digest(format!("{team_key}\0{workspace_id}").as_bytes())
        )
    }

    pub fn path_in(root: &Path, team_key: &str, workspace_id: &str) -> PathBuf {
        root.join(format!("{}.json", Self::file_key(team_key, workspace_id)))
    }
}

/// Wall-clock epoch ms. Prefer this over `Instant` anywhere that gets written
/// to disk; see the field doc on `MoveIntent::last_progress_at_ms`.
pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Returns `{codesense_home}/users/{principal}/move-workspace-intents-v1/`.
/// Mirrors `remote-workspace-intents-v1/`'s root convention: namespaced per
/// principal so a sign-out / principal change does not expose another user's
/// pending moves.
pub fn intent_root(codesense_home: &Path, principal: &crate::domain::Principal) -> PathBuf {
    crate::context_repo::path_to_user_home(codesense_home, principal).join(INTENT_DIR_NAME)
}

/// Why a write failed. The caller distinguishes "another move is in flight"
/// (`AlreadyExists`) from other I/O errors because it maps to the `in_progress`
/// pre-flight refusal.
#[derive(Debug, PartialEq, Eq)]
pub enum IntentWriteError {
    /// A move intent file already exists for this `(team_key, workspace_id)`.
    /// Maps to the `in_progress` pre-flight refusal; the caller must NOT
    /// clobber it (two moves in flight corrupts both).
    AlreadyExists,
    /// Underlying I/O surface. Message is safe to render to the user.
    Io(String),
}

/// Why a read failed. `Corrupt` is specifically "the bytes don't parse" —
/// the reaper logs and skips (does NOT delete; a human inspects); the on-demand
/// reader surfaces it as `Io` so the user gets a clean error.
#[derive(Debug, PartialEq, Eq)]
pub enum IntentReadError {
    NotFound,
    Corrupt(String),
    Io(String),
}

/// Write a NEW intent file atomically. Fails `AlreadyExists` if any file at
/// the same key exists — never overwrites.
///
/// Race safety: uses `create_new(true)` on the destination path directly
/// (mirroring the `remote-workspace-intents-v1` pattern) so a concurrent
/// caller either lands its file and wins, or opens the already-existing
/// file and gets `AlreadyExists` — never clobbers. The in-process
/// `INTENT_WRITES` guard additionally serialises writes so a reaper and a
/// user-driven call can't interleave on the same file descriptor.
pub fn write_new(root: &Path, intent: &MoveIntent) -> Result<(), IntentWriteError> {
    let _guard = INTENT_WRITES
        .lock()
        .map_err(|_| IntentWriteError::Io("intent write lock poisoned".into()))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(root)
        .map_err(|e| IntentWriteError::Io(format!("intent root unavailable: {e}")))?;
    let path = MoveIntent::path_in(root, &intent.team_key, &intent.workspace_id);
    if path.exists() {
        let prior = read_path(&path)
            .map_err(|e| IntentWriteError::Io(format!("read prior intent: {e:?}")))?;
        // Architecture: a failed prepare can be retried; pending source cleanup still owns its move.
        if prior.step != MoveStep::PreparingTarget || prior.failure.is_none() {
            return Err(IntentWriteError::AlreadyExists);
        }
        archive_intent_locked(root, &prior)?;
    }
    let bytes =
        serde_json::to_vec(intent).map_err(|e| IntentWriteError::Io(format!("serialize: {e}")))?;
    if bytes.len() as u64 > INTENT_MAX_BYTES {
        return Err(IntentWriteError::Io("intent too large".into()));
    }
    use std::io::Write;
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(IntentWriteError::AlreadyExists);
        }
        Err(e) => return Err(IntentWriteError::Io(format!("open: {e}"))),
    };
    file.write_all(&bytes)
        .map_err(|e| IntentWriteError::Io(format!("write: {e}")))?;
    file.sync_all()
        .map_err(|e| IntentWriteError::Io(format!("fsync: {e}")))?;
    sync_dir(root);
    Ok(())
}

// Architecture: retry evidence is kept outside the active-intent scan before its slot is released.
fn archive_intent_locked(root: &Path, intent: &MoveIntent) -> Result<(), IntentWriteError> {
    use std::io::Write;
    let archive = root.join(format!(
        "{}-{}.failed",
        MoveIntent::file_key(&intent.team_key, &intent.workspace_id),
        uuid::Uuid::new_v4()
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(archive)
        .map_err(|e| IntentWriteError::Io(e.to_string()))?;
    file.write_all(&serde_json::to_vec(intent).map_err(|e| IntentWriteError::Io(e.to_string()))?)
        .and_then(|()| file.sync_all())
        .map_err(|e| IntentWriteError::Io(e.to_string()))?;
    std::fs::remove_file(MoveIntent::path_in(
        root,
        &intent.team_key,
        &intent.workspace_id,
    ))
    .map_err(|e| IntentWriteError::Io(e.to_string()))?;
    sync_dir(root);
    Ok(())
}

fn rollback_preparing(root: &Path, intent: &MoveIntent) -> Result<(), String> {
    let _guard = INTENT_WRITES
        .lock()
        .map_err(|_| "intent write lock poisoned")?;
    let mut current = match read(root, &intent.team_key, &intent.workspace_id) {
        Ok(current) => current,
        Err(IntentReadError::NotFound) => return Ok(()),
        Err(error) => return Err(format!("rollback read: {error:?}")),
    };
    // Architecture: a stale boot task must not archive a newer retry.
    if current.to.request_id != intent.to.request_id || current.step != MoveStep::PreparingTarget {
        return Ok(());
    }
    current
        .failure
        .get_or_insert_with(|| "rolled back before location swap".into());
    archive_intent_locked(root, &current).map_err(|e| format!("rollback archive: {e:?}"))
}

/// Rewrite an EXISTING intent file atomically — step advance or progress bump.
/// Returns `Io` with "intent missing" if the file isn't there; this is a
/// programming error on the caller's part, not a race (reaper deletes only
/// after the pipeline completes).
pub fn rewrite(root: &Path, intent: &MoveIntent) -> Result<(), IntentWriteError> {
    let _guard = INTENT_WRITES
        .lock()
        .map_err(|_| IntentWriteError::Io("intent write lock poisoned".into()))?;
    let path = MoveIntent::path_in(root, &intent.team_key, &intent.workspace_id);
    if !path.exists() {
        return Err(IntentWriteError::Io(format!(
            "intent missing at {}",
            path.display()
        )));
    }
    let bytes =
        serde_json::to_vec(intent).map_err(|e| IntentWriteError::Io(format!("serialize: {e}")))?;
    if bytes.len() as u64 > INTENT_MAX_BYTES {
        return Err(IntentWriteError::Io("intent too large".into()));
    }
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
            .map_err(|e| IntentWriteError::Io(format!("open tmp: {e}")))?;
        file.write_all(&bytes)
            .map_err(|e| IntentWriteError::Io(format!("write: {e}")))?;
        file.sync_all()
            .map_err(|e| IntentWriteError::Io(format!("fsync: {e}")))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        IntentWriteError::Io(format!("rename: {e}"))
    })?;
    sync_dir(root);
    Ok(())
}

/// Read one intent file at `(team_key, workspace_id)`. `NotFound` is a clean
/// "no move in flight"; `Corrupt` is the bytes-don't-parse case the reaper
/// logs-and-skips. Kept crate-public even though `scan` is the common path —
/// the `in_progress` pre-flight refusal is a point query and future call
/// sites (tests, debug tools) read by key.
pub fn read(
    root: &Path,
    team_key: &str,
    workspace_id: &str,
) -> Result<MoveIntent, IntentReadError> {
    let path = MoveIntent::path_in(root, team_key, workspace_id);
    read_path(&path)
}

fn read_path(path: &Path) -> Result<MoveIntent, IntentReadError> {
    let mut file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(IntentReadError::NotFound);
        }
        Err(e) => return Err(IntentReadError::Io(format!("open: {e}"))),
    };
    let metadata = file
        .metadata()
        .map_err(|e| IntentReadError::Io(format!("stat: {e}")))?;
    if !metadata.is_file() {
        return Err(IntentReadError::Io("intent is not a regular file".into()));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(INTENT_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| IntentReadError::Io(format!("read: {e}")))?;
    if bytes.len() as u64 > INTENT_MAX_BYTES {
        return Err(IntentReadError::Corrupt("intent exceeds size cap".into()));
    }
    serde_json::from_slice(&bytes).map_err(|e| IntentReadError::Corrupt(format!("parse: {e}")))
}

/// Delete the intent file for `(team_key, workspace_id)`. Idempotent — a
/// missing file is `Ok(())`, matching the "completed → gone" idiom.
pub fn delete(root: &Path, team_key: &str, workspace_id: &str) -> Result<(), IntentWriteError> {
    let _guard = INTENT_WRITES
        .lock()
        .map_err(|_| IntentWriteError::Io("intent write lock poisoned".into()))?;
    let path = MoveIntent::path_in(root, team_key, workspace_id);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            sync_dir(root);
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(IntentWriteError::Io(format!("remove: {e}"))),
    }
}

/// Scan every intent file under `root`. **Corrupt files are logged and
/// skipped** — one bad JSON blob does NOT abort the scan. Callers that need
/// to react to a corruption pattern (operator alerting, say) read the warn
/// log stream. The file itself stays on disk so a human can inspect.
///
/// This is the shape the boot reaper consumes; see
/// `reap_move_workspace_intents`.
pub fn scan(root: &Path) -> Vec<MoveIntent> {
    let entries = match std::fs::read_dir(root) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            warn!(
                "move-workspace intent root unreadable at {}: {e}; skipping scan",
                root.display()
            );
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        match read_path(&path) {
            Ok(intent) => out.push(intent),
            Err(IntentReadError::NotFound) => {
                // Vanished between readdir and open; skip quietly.
            }
            Err(IntentReadError::Corrupt(reason)) => {
                warn!(
                    "move-workspace intent corrupt at {}: {reason} — skipping; file left for inspection",
                    path.display()
                );
            }
            Err(IntentReadError::Io(reason)) => {
                warn!(
                    "move-workspace intent I/O error at {}: {reason} — skipping",
                    path.display()
                );
            }
        }
    }
    out
}

fn sync_dir(root: &Path) {
    if let Ok(dir) = std::fs::File::open(root) {
        let _ = dir.sync_all();
    }
}

// ────────────────────────────────────────────────────────────────────────────
// MCP tool surface — `move_workspace`, `list_move_intents`, and the
// boot reaper that resumes any pipeline the previous process left in flight.
// ────────────────────────────────────────────────────────────────────────────

use crate::domain::SourceRepoSlug;

/// Event names. `move-workspace-progress` is the step-1 progress stream (we
/// reuse the shape of `remote-workspace-progress` so FE listeners share
/// code); `move-workspace-intent-hydrate` tells FE to repopulate
/// `ws.setupState` from the current intent, and `move-workspace-intent-cleared`
/// tells it to drop the projection.
pub const MOVE_WORKSPACE_PROGRESS_EVENT: &str = "move-workspace-progress";
pub const MOVE_WORKSPACE_INTENT_HYDRATE_EVENT: &str = "move-workspace-intent-hydrate";
pub const MOVE_WORKSPACE_INTENT_CLEARED_EVENT: &str = "move-workspace-intent-cleared";

/// MCP tool inputs — the user chooses only `storyId` + `targetMachine` +
/// optional `discardUncommitted` + optional `targetWorktreePath` (required
/// when target is local — the FE computes it from the worktree.yaml template
/// so the Rust handler stays agnostic to local-worktree-path derivation).
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoveWorkspaceParams {
    /// Workspace handle in the active team — the viewer id, matching the
    /// field the 7 refusal sites compare against.
    pub story_id: String,
    /// Target machine id from `list_machines`, or `"local"` for This Mac.
    pub target_machine: String,
    /// Default `false`. When `false`, an uncommitted change on the source
    /// worktree refuses with `dirty_worktree` (listing the dirty paths and
    /// the flag hint).
    #[serde(default)]
    pub discard_uncommitted: bool,
    /// Policy: explicitly stop source sessions through the normal close path.
    #[serde(default)]
    pub stop_running_agents: bool,
    /// Policy: carry committed work without publishing it to a remote.
    #[serde(default)]
    pub carry_unpushed: bool,
    /// Policy: explicitly release commits absent from remote-tracking refs.
    #[serde(default)]
    pub discard_unpushed: bool,
    /// Required when `target_machine == "local"` — the FE-derived managed
    /// worktree path (via `render_worktree_dir(worktree.yaml, clone, branch)`).
    /// For remote targets, this is derived server-side from the account
    /// home and ignored if given.
    #[serde(default)]
    pub target_worktree_path: Option<String>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MoveWorkspaceResult {
    pub moved: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub source_kept: bool,
    pub report: MoveSourceReport,
    pub stopped_agents: Vec<MoveAgent>,
    pub reason: Option<String>,
    pub from: MachineRef,
    pub to: MachineRef,
    pub handle: Option<String>,
    pub cwd: Option<String>,
    /// `true` when step 2 (location swap) completed but step 3 (source
    /// cleanup) failed. Cleanup retries on next boot via the reaper.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cleanup_pending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MoveAgent {
    pub handle: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MoveSourceReport {
    pub running_agents: Vec<MoveAgent>,
    pub uncommitted_paths: Vec<String>,
    pub unpushed_count: usize,
    pub source_tip: String,
    /// "fetched" or "cached"; the latter includes the fetch failure.
    pub unpushed_check: String,
    pub unpushed_check_reason: Option<String>,
}

fn assess_source(
    params: &MoveWorkspaceParams,
    status: codesense_pty_core::remote_workspace::RemoteWorktreeDirtyStatus,
    tabs: &[super::terminal_tabs::LiveTab],
    caller: Option<&str>,
    remote_to_remote: bool,
) -> Result<MoveSourceReport, String> {
    let report = MoveSourceReport {
        running_agents: tabs.iter().map(agent_report).collect(),
        uncommitted_paths: status.paths,
        source_tip: status.source_tip,
        unpushed_count: status.unpushed_count,
        unpushed_check: if status.fetch_error.is_some() {
            "cached"
        } else {
            "fetched"
        }
        .into(),
        unpushed_check_reason: status.fetch_error,
    };
    let self_close = caller.is_some_and(|caller| tabs.iter().any(|tab| tab.session == caller));
    check_choices(params, &report, self_close, remote_to_remote)?;
    Ok(report)
}

fn with_report(reason: impl std::fmt::Display, report: &MoveSourceReport) -> String {
    format!(
        "{reason}; report: {}",
        serde_json::to_string(report).unwrap_or_default()
    )
}

// Architecture: all refusals precede stopping agents or preparing a target.
fn check_choices(
    params: &MoveWorkspaceParams,
    report: &MoveSourceReport,
    self_close: bool,
    remote_to_remote: bool,
) -> Result<(), String> {
    let reason = if params.carry_unpushed && params.discard_unpushed {
        Some("invalid arguments: carryUnpushed and discardUnpushed are mutually exclusive".into())
    } else if self_close {
        Some("selfClose: a session cannot move the workspace that holds it".into())
    } else if !report.running_agents.is_empty() && !params.stop_running_agents {
        Some("runningAgents: stop source agents first or pass stopRunningAgents: true".into())
    } else if !report.uncommitted_paths.is_empty() && !params.discard_uncommitted {
        Some(
            PreflightRefusal::DirtyWorktree {
                paths: report.uncommitted_paths.clone(),
            }
            .to_string(),
        )
    } else if report.unpushed_count > 0 && !params.carry_unpushed && !params.discard_unpushed {
        Some("unpushedCommits: choose carryUnpushed or discardUnpushed".into())
    } else if remote_to_remote && params.carry_unpushed {
        Some("carryUnpushed between machines is not supported; bring the workspace to this Mac first".into())
    } else {
        None
    };
    reason.map_or(Ok(()), |reason: String| Err(with_report(reason, report)))
}

/// A single open `MoveIntent`, surfaced to the FE for pull-on-init so a
/// restarted FE can bootstrap its `setupState` projection without waiting
/// for an event emit from the reaper.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MoveIntentView {
    pub team_key: String,
    pub workspace_id: String,
    pub from: MoveSource,
    pub to: MoveTarget,
    pub step: MoveStep,
    pub dirty_allowed: bool,
    pub started_at_ms: u64,
    pub last_progress_at_ms: u64,
}

impl From<&MoveIntent> for MoveIntentView {
    fn from(i: &MoveIntent) -> Self {
        Self {
            team_key: i.team_key.clone(),
            workspace_id: i.workspace_id.clone(),
            from: i.from.clone(),
            to: i.to.clone(),
            step: i.step,
            dirty_allowed: i.dirty_allowed,
            started_at_ms: i.started_at_ms,
            last_progress_at_ms: i.last_progress_at_ms,
        }
    }
}

/// Current workspace shape read from `McpViewerState`, mirroring the FE's
/// `MachineLocation` record. Only the fields the pipeline needs.
#[derive(Clone, Debug)]
pub(crate) struct WorkspaceSnapshot {
    pub machine_id: MachineRef,
    pub request_id: String,
    pub handle: Option<String>,
    pub cwd: Option<String>,
    pub source_repo: SourceRepoSlug,
    pub base_ref: String,
    pub branch: String,
    pub repo_path: String,
    pub worktree_path: String,
}

/// Read the open workspace at `(team_key, story_id)` from the viewer's pushed
/// `workspaces` rows. Returns `None` when no such row is open.
///
/// The row shape is the FE's `Workspace` (viewer-store.ts), which differs from
/// what this reader used to expect:
/// - `storyId` is the story id; `id` is the path-derived identity. Match either.
/// - a workspace on This Mac has NO `location`; its directory is the row's `cwd`
///   and its clone is `sourceRepoPath`. Only a machine workspace carries a
///   `location` (`machineId`, `requestId`, `handle`, `cwd`, `branch`, `baseRef`).
/// - the source repo is `sourceRepoSlug` ("owner/repo"), not a `sourceRepo` object.
///   A `sourceRepo` object is still accepted for callers that already send one.
pub(crate) fn read_workspace_snapshot(
    viewer: &serde_json::Value,
    team_key: &str,
    story_id: &str,
) -> Option<WorkspaceSnapshot> {
    let rows = viewer.get("workspaces")?.get(team_key)?.as_array()?;
    let names = |row: &&serde_json::Value, field: &str| {
        row.get(field).and_then(|v| v.as_str()) == Some(story_id)
    };
    let row = rows
        .iter()
        .find(|row| names(row, "storyId"))
        .or_else(|| rows.iter().find(|row| names(row, "id")))?;
    let text = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let location = row.get("location").filter(|l| l.is_object());
    let machine_id = match location.and_then(|l| text(l, "kind")).as_deref() {
        Some("machine") => MachineRef(location.and_then(|l| text(l, "machineId"))),
        _ => MachineRef(None),
    };
    let remote = !machine_id.is_local();
    // A remote workspace's directory is on the machine (`location.cwd`); a local
    // one's is the row's own `cwd`.
    let cwd = if remote {
        location.and_then(|l| text(l, "cwd"))
    } else {
        text(row, "cwd")
    };
    let source_repo: SourceRepoSlug = match row.get("sourceRepo") {
        Some(obj) if obj.is_object() => serde_json::from_value(obj.clone()).ok()?,
        _ => crate::install::worktree_index::slug_from_source_repo(&text(row, "sourceRepoSlug")?)?,
    };
    let loc = |key: &str| location.and_then(|l| text(l, key)).unwrap_or_default();
    let repo_path = text(row, "sourceRepoPath")
        .or_else(|| text(row, "repoPath"))
        .unwrap_or_default();
    let worktree_path = cwd.clone().unwrap_or_default();
    Some(WorkspaceSnapshot {
        machine_id,
        request_id: loc("requestId"),
        handle: location.and_then(|l| text(l, "handle")),
        cwd,
        source_repo,
        base_ref: loc("baseRef"),
        branch: loc("branch"),
        repo_path,
        worktree_path,
    })
}

/// The team's workspace store for this Mac: the worktree→story index
/// (`install::worktree_index`), which `open_workspace` writes when it establishes
/// a story's managed worktree. It survives a viewer reload and needs no open tab,
/// so a workspace opened only over MCP is found here. Only the team's own stories
/// count: the record's org must match and its story's `_team/<team>/` must be
/// `team`. A record whose worktree directory is gone is not a movable workspace.
pub(crate) fn snapshot_from_worktree_index(
    home: &Path,
    principal: &crate::domain::Principal,
    team_key: &str,
    story_id: &str,
) -> Result<Option<WorkspaceSnapshot>, String> {
    let Some((org_slug, team)) = team_key.split_once(':') else {
        return Ok(None);
    };
    let records = crate::install::worktree_index::iter_story_records(home, principal)
        .map_err(|e| format!("reading the worktree index failed: {e}"))?;
    let found = records.into_iter().find(|r| {
        r.story_id == story_id
            && r.org_slug == org_slug
            && crate::install::worktree_index::resolve_team(r).as_deref() == Some(team)
            && r.code_repo_path.is_dir()
    });
    Ok(found.map(|r| {
        let path = r.code_repo_path.to_string_lossy().into_owned();
        WorkspaceSnapshot {
            machine_id: MachineRef(None),
            request_id: String::new(),
            handle: None,
            cwd: Some(path.clone()),
            source_repo: r.source_repo,
            base_ref: String::new(),
            branch: String::new(),
            repo_path: String::new(),
            worktree_path: path,
        }
    }))
}

/// Fill the fields a local snapshot cannot carry from the viewer or the index:
/// the checked-out branch, and the clone the worktree belongs to (the git common
/// dir's parent). Both come from git, so this runs off the async workers.
async fn complete_local_snapshot(mut ws: WorkspaceSnapshot) -> Result<WorkspaceSnapshot, String> {
    if !ws.machine_id.is_local() {
        return Ok(ws);
    }
    let Some(cwd) = ws.cwd.clone() else {
        return Ok(ws);
    };
    let need_branch = ws.branch.is_empty();
    let need_repo = ws.repo_path.is_empty();
    if !need_branch && !need_repo {
        return Ok(ws);
    }
    let (branch, repo) = crate::commands::await_blocking("move_workspace_local_git", move || {
        let worktree = Path::new(&cwd);
        let branch = need_branch
            .then(|| crate::commands::worktree::current_branch_of_worktree(worktree))
            .flatten();
        let repo = need_repo
            .then(|| clone_root_of_worktree(worktree))
            .flatten();
        Ok((branch, repo))
    })
    .await?;
    if let Some(branch) = branch {
        ws.branch = branch;
    }
    if let Some(repo) = repo {
        ws.repo_path = repo;
    }
    Ok(ws)
}

/// The clone a worktree was made from: the parent of the git common dir
/// (`<clone>/.git`). `None` outside a git repo.
fn clone_root_of_worktree(worktree: &Path) -> Option<String> {
    let out = std::process::Command::new(crate::commands::worktree::git_bin())
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(worktree)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let common = String::from_utf8(out.stdout).ok()?;
    Path::new(common.trim())
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Resolve the workspace a move names, in order: the viewer's open row (it
/// carries the machine location of a remote workspace), then the team's
/// workspace store. `None` means neither has it.
pub(crate) async fn resolve_move_source(
    app: &tauri::AppHandle,
    ctx: &crate::commands::route_to_context::ActiveContext,
    team_key: &str,
    story_id: &str,
) -> Result<Option<WorkspaceSnapshot>, String> {
    let viewer = viewer_snapshot(app);
    let ws = match read_workspace_snapshot(&viewer, team_key, story_id) {
        Some(ws) => ws,
        None => {
            let home = ctx.codesense_home.clone();
            let principal = ctx.principal();
            let (tk, sid) = (team_key.to_string(), story_id.to_string());
            let found = crate::commands::await_blocking("move_workspace_store_lookup", move || {
                snapshot_from_worktree_index(&home, &principal, &tk, &sid)
            })
            .await?;
            match found {
                Some(ws) => ws,
                None => return Ok(None),
            }
        }
    };
    if !ws.machine_id.is_local() {
        let remote = super::remote_workspace_target::resolve_selected(
            app.clone(),
            ctx.clone(),
            Some(ws.machine_id.as_machine_id()),
            ws.handle.as_deref().ok_or("remote source handle missing")?,
            None,
            false,
        )
        .await?;
        let mut ws = ws;
        ws.repo_path = remote.prepared_request.repo_path;
        ws.worktree_path = remote.cwd.clone();
        ws.cwd = Some(remote.cwd);
        ws.branch = remote.prepared_branch;
        ws.base_ref = remote.base_ref;
        return Ok(Some(ws));
    }
    complete_local_snapshot(ws).await.map(Some)
}

/// Progress payload emitted on [`MOVE_WORKSPACE_PROGRESS_EVENT`]. Keyed by the
/// `(team_key, workspace_id)` pair the FE uses to find the `ws.setupState`
/// to update; the step name tracks which stage of the pipeline is live.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveProgressPayload {
    pub team_key: String,
    pub workspace_id: String,
    pub step: MoveStep,
    pub message: Option<String>,
}

/// Emit [`MOVE_WORKSPACE_INTENT_HYDRATE_EVENT`] carrying the intent view so FE
/// can rebuild `ws.setupState` without a separate `list_move_intents` round
/// trip. Fire-and-forget: FE may not be listening yet; it falls back to
/// pull-on-init via `list_move_intents` on next store load.
pub(crate) fn emit_hydrate(app: &tauri::AppHandle, intent: &MoveIntent) {
    use tauri::Emitter;
    let view: MoveIntentView = intent.into();
    let _ = app.emit(MOVE_WORKSPACE_INTENT_HYDRATE_EVENT, &view);
}

pub(crate) fn emit_cleared(app: &tauri::AppHandle, team_key: &str, workspace_id: &str) {
    use tauri::Emitter;
    // Tauri's `emit` requires `Serialize + Clone`.
    #[derive(Serialize, Clone)]
    #[serde(rename_all = "camelCase")]
    struct Cleared {
        team_key: String,
        workspace_id: String,
    }
    let _ = app.emit(
        MOVE_WORKSPACE_INTENT_CLEARED_EVENT,
        Cleared {
            team_key: team_key.to_string(),
            workspace_id: workspace_id.to_string(),
        },
    );
}

pub(crate) fn emit_progress(
    app: &tauri::AppHandle,
    team_key: &str,
    workspace_id: &str,
    step: MoveStep,
    message: Option<String>,
) {
    use tauri::Emitter;
    let _ = app.emit(
        MOVE_WORKSPACE_PROGRESS_EVENT,
        MoveProgressPayload {
            team_key: team_key.to_string(),
            workspace_id: workspace_id.to_string(),
            step,
            message,
        },
    );
}

/// Read the viewer snapshot that `McpViewerState` keeps.
pub(crate) fn viewer_snapshot(app: &tauri::AppHandle) -> serde_json::Value {
    use tauri::Manager;
    app.state::<crate::mcp::state::McpViewerState>().get()
}

/// List the open move intents for the current principal. Mirrors
/// `remote_workspace::prepared_places` conceptually but exposes the full
/// intent view (step + timestamps) so FE pull-on-init can rebuild
/// `ws.setupState` on each affected workspace. Honours log-and-skip on
/// corrupt files — one bad intent never aborts the list.
pub async fn list_intents(app: tauri::AppHandle) -> Result<Vec<MoveIntentView>, String> {
    let ctx = crate::commands::route_to_context::derive_active_context(&app).await?;
    let root = intent_root(&ctx.codesense_home, &ctx.principal());
    let intents =
        crate::commands::await_blocking("list_move_intents", move || Ok(scan(&root))).await?;
    Ok(intents.iter().map(MoveIntentView::from).collect())
}

/// What the handler should do with a `MoveIntent` whose pipeline
/// returned an error. Pure function of the current step — tested in
/// isolation without needing the full Tauri runtime.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum RollbackAction {
    /// Pre-swap: retain the failure for safe boot rollback; the source stays intact.
    Rollback,
    /// Post-step-2: the swap is on disk and the FE applied it. Leave the
    /// intent in place so the boot reaper retries step 3; report
    /// `cleanup_pending: true` so the caller knows a background retry owes
    /// a finish. The user's workspace IS moved.
    RollForwardWithCleanupPending,
}

/// The rollback decision for a pipeline failure.
pub(crate) fn rollback_decision(step: MoveStep) -> RollbackAction {
    match step {
        MoveStep::PreparingTarget => RollbackAction::Rollback,
        MoveStep::SwappingLocation | MoveStep::CleaningSource => {
            RollbackAction::RollForwardWithCleanupPending
        }
    }
}

/// Pre-flight refusal reasons. All checked BEFORE any state change — a
/// refusal here leaves the workspace exactly as it was.
#[derive(Debug)]
pub enum PreflightRefusal {
    /// The story is neither open in the viewer nor in this team's workspace store.
    NoSuchWorkspace {
        story_id: String,
    },
    SameMachine,
    TargetUnreachable(String),
    DirtyWorktree {
        paths: Vec<String>,
    },
    InProgress,
    MissingTargetPath,
    Internal(String),
}

impl std::fmt::Display for PreflightRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchWorkspace { story_id } => write!(
                f,
                "no such workspace in the active team: story \"{story_id}\" is not open in the viewer and has no managed worktree in this team's workspace store (this Mac's worktree index). Run open_workspace with storyId \"{story_id}\" (it creates and registers the worktree), then retry move_workspace. For a workspace on another machine, open it there with open_workspace and its machineId first"
            ),
            Self::SameMachine => write!(f, "workspace already lives on the target machine"),
            Self::TargetUnreachable(why) => write!(f, "target machine unreachable: {why}"),
            Self::DirtyWorktree { paths } => {
                let shown = paths.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
                let more = if paths.len() > 5 {
                    format!(" (and {} more)", paths.len() - 5)
                } else {
                    String::new()
                };
                write!(
                    f,
                    "worktree has uncommitted changes: {shown}{more} — pass `discardUncommitted: true` to discard them"
                )
            }
            Self::InProgress => write!(
                f,
                "another move_workspace is already in flight for this workspace"
            ),
            Self::MissingTargetPath => write!(
                f,
                "target_worktree_path is required when target_machine is \"local\""
            ),
            Self::Internal(e) => write!(f, "{e}"),
        }
    }
}

/// Pipeline entry point. Called from the `move_workspace` MCP tool handler.
///
/// Pre-flight → write `MoveIntent{preparing-target}` → step 1 prepare target
/// → rewrite `MoveIntent{swapping-location}` → emit hydrate (FE applies the
/// swap) → rewrite `MoveIntent{cleaning-source}` → step 3 cleanup → delete
/// intent → emit cleared. Each `rewrite` is the durable transition; a crash
/// between a rewrite and the following foreign action is caught by the
/// boot reaper.
pub async fn handle(
    app: tauri::AppHandle,
    team_key: String,
    params: MoveWorkspaceParams,
    caller: Option<&str>,
) -> Result<MoveWorkspaceResult, String> {
    let _executing = ExecutingMove::acquire(&team_key, &params.story_id)?;
    let ctx = crate::commands::route_to_context::derive_active_context(&app).await?;
    let root = intent_root(&ctx.codesense_home, &ctx.principal());

    // Architecture: after the swap, a retry finishes the original move at its durable
    // target, regardless of the caller's newly requested location.
    if let Ok(mut pending) = read(&root, &team_key, &params.story_id) {
        if pending.step != MoveStep::PreparingTarget {
            let mut host = AppMoveHost {
                app: &app,
                ctx: None,
            };
            let resumed = resume_intent_with(&root, &mut pending, &mut host).await;
            let report = MoveSourceReport {
                source_tip: pending.source_tip.clone().unwrap_or_default(),
                ..Default::default()
            };
            return match resumed {
                Ok(()) => Ok(completed_move_result(&pending, report, Vec::new())),
                Err(error) => {
                    record_failure(&root, &mut pending, &error)?;
                    Ok(pending_move_result(&pending, report, Vec::new(), error))
                }
            };
        }
    }

    // 1. Pre-flight. All refusals here leave the workspace untouched.
    let ws = resolve_move_source(&app, &ctx, &team_key, &params.story_id)
        .await?
        .ok_or_else(|| {
            PreflightRefusal::NoSuchWorkspace {
                story_id: params.story_id.clone(),
            }
            .to_string()
        })?;
    let target = MachineRef(
        if params.target_machine == crate::commands::machines::LOCAL_MACHINE_ID {
            None
        } else {
            Some(params.target_machine.clone())
        },
    );
    // Architecture: collect the entire report before any destructive action.
    let live = source_tabs(&app, &ws.machine_id, ws.cwd.as_deref()).await?;
    let status = check_source_dirty(&app, &ws, true).await?;
    let report = assess_source(
        &params,
        status,
        &live,
        caller,
        !ws.machine_id.is_local() && !target.is_local(),
    )?;
    if target == ws.machine_id {
        return Err(with_report(PreflightRefusal::SameMachine, &report));
    }
    if target.is_local() && params.target_worktree_path.is_none() {
        return Err(with_report(PreflightRefusal::MissingTargetPath, &report));
    }
    if !target.is_local() {
        crate::commands::machines::authorize_terminal_machine(
            &app,
            target.as_machine_id(),
            &team_key,
        )
        .await
        .map_err(|e| with_report(PreflightRefusal::TargetUnreachable(e), &report))?;
    }
    // 2. Write MoveIntent{preparing-target}. `AlreadyExists` → `in_progress`
    //    refusal; the race window is zero because `write_new` + rename-at
    //    is atomic.
    let mut intent = MoveIntent::new(
        team_key.clone(),
        params.story_id.clone(),
        MoveSource {
            machine_id: ws.machine_id.clone(),
            request_id: ws.request_id.clone(),
            handle: ws.handle.clone(),
            cwd: ws.cwd.clone(),
            repo_path: ws.repo_path.clone(),
            repository_key: ws.source_repo.remote_key(),
        },
        MoveTarget {
            machine_id: target.clone(),
            request_id: uuid::Uuid::new_v4().to_string(),
        },
        params.discard_uncommitted,
    );
    intent.source_tip = Some(report.source_tip.clone());
    intent.source_repo = Some(ws.source_repo.clone());
    match write_new(&root, &intent) {
        Ok(()) => {}
        Err(IntentWriteError::AlreadyExists) => {
            return Err(with_report(PreflightRefusal::InProgress, &report));
        }
        Err(IntentWriteError::Io(e)) => {
            return Err(PreflightRefusal::Internal(e).to_string());
        }
    }
    emit_progress(
        &app,
        &team_key,
        &params.story_id,
        MoveStep::PreparingTarget,
        Some(format!(
            "source report: {}",
            serde_json::to_string(&report).map_err(|e| e.to_string())?
        )),
    );
    let mut stopped_agents = Vec::new();
    // Policy: the explicit flag opts into the same Kill path as close_workspace.
    for tab in &live {
        let session = crate::commands::machines::RuntimeSessionReference::Admitted(Box::new(
            tab.target.clone(),
        ));
        if let Err(error) = crate::commands::pty_layout::terminal_close_tab(
            app.clone(),
            session,
            codesense_pty_core::layout::CloseMode::Kill,
        )
        .await
        {
            record_failure(&root, &mut intent, &error)?;
            return Err(with_report(error, &report));
        }
        stopped_agents.push(agent_report(tab));
    }
    emit_hydrate(&app, &intent);
    emit_progress(
        &app,
        &team_key,
        &params.story_id,
        MoveStep::PreparingTarget,
        Some("starting".into()),
    );

    // Architecture: failures retain the durable step. Only a verified swap can roll forward.

    let mut prepared_opt: Option<codesense_pty_core::remote_workspace::PreparedWorkspace> = None;
    let pipeline_result = run_pipeline(
        &app,
        &ctx,
        &root,
        &mut intent,
        &params,
        &ws,
        &mut prepared_opt,
    )
    .await;
    match pipeline_result {
        Ok(()) => Ok(completed_move_result(&intent, report, stopped_agents)),
        Err(e) if intent.step == MoveStep::PreparingTarget && is_source_refusal(&e) => {
            Ok(MoveWorkspaceResult {
                moved: false,
                source_kept: false,
                report,
                stopped_agents,
                reason: Some(e),
                from: ws.machine_id,
                to: target,
                handle: ws.handle,
                cwd: ws.cwd,
                cleanup_pending: false,
            })
        }
        Err(e) => match rollback_decision(intent.step) {
            RollbackAction::Rollback => {
                // Architecture: retain the failed step for diagnosis and safe boot rollback.
                record_failure(&root, &mut intent, &e)?;
                Err(with_report(e, &report))
            }
            RollbackAction::RollForwardWithCleanupPending => {
                // Post-step-2 error: swap on disk, FE has (or will soon have)
                // applied it. Leave the intent in place so the boot reaper
                // retries step 3; report the move as done with
                // `cleanup_pending: true`.
                log::warn!(
                    "move_workspace {}#{} step-3 failed, cleanup deferred to reaper: {e}",
                    intent.team_key,
                    intent.workspace_id
                );
                record_failure(&root, &mut intent, &e)?;
                Ok(pending_move_result(&intent, report, stopped_agents, e))
            }
        },
    }
}

// Architecture: live completion and a cleanup retry report the same durable target.
fn completed_move_result(
    intent: &MoveIntent,
    report: MoveSourceReport,
    stopped_agents: Vec<MoveAgent>,
) -> MoveWorkspaceResult {
    MoveWorkspaceResult {
        moved: true,
        source_kept: intent.source_kept,
        report,
        stopped_agents,
        reason: intent.cleanup_warning.clone(),
        from: intent.from.machine_id.clone(),
        to: intent.to.machine_id.clone(),
        handle: intent.target_handle.clone(),
        cwd: intent.target_cwd.clone(),
        cleanup_pending: false,
    }
}

fn pending_move_result(
    intent: &MoveIntent,
    report: MoveSourceReport,
    stopped_agents: Vec<MoveAgent>,
    reason: String,
) -> MoveWorkspaceResult {
    let mut result = completed_move_result(intent, report, stopped_agents);
    result.source_kept = false;
    result.cleanup_pending = true;
    result.reason = Some(reason);
    result
}

/// Resolve the dirty-check source: local uses `check_worktree_dirty`; remote
/// uses the new `check_worktree_dirty_remote` RPC. Returns the shape the
/// pre-flight refusal needs (dirty bool + paths list for the error text).
async fn check_source_dirty(
    app: &tauri::AppHandle,
    ws: &WorkspaceSnapshot,
    refresh_refs: bool,
) -> Result<codesense_pty_core::remote_workspace::RemoteWorktreeDirtyStatus, String> {
    if ws.machine_id.is_local() {
        let path = ws.worktree_path.clone();
        crate::commands::await_blocking("move_workspace_source_report", move || {
            local_source_status(&path, refresh_refs)
        })
        .await
    } else {
        // Remote — the session-server RPC we added in Phase A.
        let machine_id = ws
            .machine_id
            .0
            .clone()
            .ok_or_else(|| "remote dirty check needs a machine id".to_string())?;
        let backend = crate::commands::pty_backend::resolve_spawn_backend(
            app,
            &machine_id,
            "move_workspace_dirty_check",
        )?;
        backend.revalidate(app)?;
        let client = backend
            .client()
            .filter(|_| backend.is_remote())
            .ok_or_else(|| "remote dirty check unavailable (local backend)".to_string())?;
        let lease = client
            .private_lease()
            .ok_or("private authority unavailable")?;
        lease.validate()?;
        let request = codesense_pty_core::remote_workspace::CheckWorktreeDirtyRemote {
            refresh_refs,
            owner_id: lease.owner_id(),
            repository_key: ws.source_repo.remote_key(),
            repo_path: ws.repo_path.clone(),
            worktree_path: ws.worktree_path.clone(),
        };
        let status = client
            .check_worktree_dirty_remote(request)
            .map_err(|e| format!("remote dirty check failed: {e}"))?;
        Ok(status)
    }
}

fn local_source_status(
    path: &str,
    refresh_refs: bool,
) -> Result<codesense_pty_core::remote_workspace::RemoteWorktreeDirtyStatus, String> {
    let fetch_error = if refresh_refs {
        let mut command = std::process::Command::new(crate::commands::worktree::git_bin());
        command
            .current_dir(path)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["fetch", "--all", "--no-tags"]);
        super::remote_workspace_bundle::run(
            command,
            &|| true,
            std::time::Instant::now() + std::time::Duration::from_secs(30),
            None,
        )
        .err()
    } else {
        None
    };
    let source_tip = local_git(path, &["rev-parse", "HEAD"])?;
    let count = local_git(
        path,
        &["rev-list", "--count", &source_tip, "--not", "--remotes"],
    )?;
    let paths = local_dirty_paths(path).ok_or("local dirty paths unavailable")?;
    Ok(
        codesense_pty_core::remote_workspace::RemoteWorktreeDirtyStatus {
            dirty: !paths.is_empty(),
            paths,
            source_tip,
            unpushed_count: count.parse().map_err(|_| "invalid unpushed count")?,
            fetch_error,
        },
    )
}

fn local_git(path: &str, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new(crate::commands::worktree::git_bin())
        .current_dir(path)
        .args(args)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn agent_report(tab: &super::terminal_tabs::LiveTab) -> MoveAgent {
    MoveAgent {
        handle: tab.handle.clone().unwrap_or_else(|| tab.session.clone()),
        name: tab.name.clone(),
    }
}

async fn source_tabs(
    app: &tauri::AppHandle,
    machine: &MachineRef,
    cwd: Option<&str>,
) -> Result<Vec<super::terminal_tabs::LiveTab>, String> {
    let current_rows = || source_runtime_rows(app, machine);
    let rows = current_rows();
    discover_source_usage(
        machine,
        cwd,
        rows,
        |target, source| {
            let app = app.clone();
            async move {
                crate::commands::await_blocking("move_source_usage", move || {
                    let backend =
                        crate::commands::pty_backend::resolve_member_backend(&app, &target, true)?;
                    backend.revalidate(&app)?;
                    let usage = backend
                        .domain()
                        .worktree_usage(&source)
                        .map_err(|e| e.to_string())?;
                    backend.revalidate(&app)?;
                    Ok(usage)
                })
                .await
            }
        },
        current_rows,
    )
    .await
}

fn source_runtime_rows(
    app: &tauri::AppHandle,
    machine: &MachineRef,
) -> Vec<crate::commands::machines::RuntimeAdmissionRow> {
    crate::commands::pty_layout::terminal_layout_admission(app.clone())
        .rows
        .into_iter()
        .filter(|row| row.member.machine_id == machine.as_machine_id())
        .collect()
}

// Architecture: one source rule serves the report, self-close, prepare and cleanup:
// every source runtime must positively answer the current canonical-path usage query.
async fn discover_source_usage<F, Fut, C>(
    machine: &MachineRef,
    cwd: Option<&str>,
    rows: Vec<crate::commands::machines::RuntimeAdmissionRow>,
    mut query: F,
    current_rows: C,
) -> Result<Vec<super::terminal_tabs::LiveTab>, String>
where
    F: FnMut(crate::commands::machines::RuntimeMemberTarget, String) -> Fut,
    Fut: std::future::Future<Output = Result<codesense_pty_core::layout::WorktreeUsage, String>>,
    C: FnOnce() -> Vec<crate::commands::machines::RuntimeAdmissionRow>,
{
    use crate::commands::machines::{
        RuntimeAdmissionState, RuntimeMemberSlot, RuntimeMemberTarget,
    };
    use codesense_pty_core::layout::{source_refusal, START_SERVER};
    let source =
        cwd.ok_or_else(|| source_refusal("source cwd missing; reopen the source workspace"))?;
    let rows: Vec<_> = rows
        .into_iter()
        .filter(|row| row.member.machine_id == machine.as_machine_id())
        .collect();
    if rows.is_empty()
        || (machine.is_local()
            && [RuntimeMemberSlot::InProcess, RuntimeMemberSlot::Sidecar]
                .iter()
                .any(|slot| !rows.iter().any(|row| row.member.slot == *slot)))
    {
        return Err(source_refusal(START_SERVER));
    }
    let mut tabs = Vec::new();
    for row in &rows {
        let RuntimeAdmissionState::Live { namespace } = &row.state else {
            return Err(source_refusal(START_SERVER));
        };
        let target = RuntimeMemberTarget {
            member: row.member.clone(),
            admission_revision: row.revision,
            namespace: namespace.clone(),
        };
        let usage = query(target.clone(), source.to_string())
            .await
            .map_err(|e| {
                if is_source_refusal(&e) {
                    e
                } else {
                    source_refusal(format!("{START_SERVER}: {e}"))
                }
            })?;
        if !usage.opening.is_empty() {
            return Err(source_refusal("runningAgents: stop the agents running in or starting into the source worktree, then retry"));
        }
        let label = if machine.is_local() {
            super::terminal_tabs::THIS_MAC
        } else {
            machine.as_machine_id()
        };
        tabs.extend(
            usage.entries.iter().map(|(key, tab)| {
                super::terminal_tabs::LiveTab::from_entry(&target, label, key, tab)
            }),
        );
    }
    // Architecture: an omitted or replaced runtime cannot count as a positive confirmation.
    let after: Vec<_> = current_rows()
        .into_iter()
        .filter(|row| row.member.machine_id == machine.as_machine_id())
        .collect();
    if rows != after {
        return Err(source_refusal(START_SERVER));
    }
    Ok(tabs)
}

fn is_source_refusal(error: &str) -> bool {
    error.contains(codesense_pty_core::layout::SOURCE_REFUSAL)
}

fn no_source_agents(tabs: &[super::terminal_tabs::LiveTab]) -> Result<(), String> {
    if tabs.is_empty() {
        Ok(())
    } else {
        Err(codesense_pty_core::layout::source_refusal("runningAgents: stop the agents running in or starting into the source worktree, then retry"))
    }
}

// Architecture: a refusal is terminal evidence, not an indefinitely retrying active move.
fn resolve_source_refusal(
    root: &Path,
    intent: &mut MoveIntent,
    reason: &str,
) -> Result<(), String> {
    let _guard = INTENT_WRITES
        .lock()
        .map_err(|_| "intent write lock poisoned")?;
    let mut current = read(root, &intent.team_key, &intent.workspace_id)
        .map_err(|e| format!("refusal read: {e:?}"))?;
    if current.to.request_id != intent.to.request_id {
        return Err("move changed before refusal".into());
    }
    current.failure = Some(reason.to_string());
    current.source_kept = intent.source_kept;
    current.cleanup_warning = intent.cleanup_warning.clone();
    archive_intent_locked(root, &current).map_err(|e| format!("refusal archive: {e:?}"))?;
    *intent = current;
    Ok(())
}

fn local_dirty_paths(worktree_path: &str) -> Option<Vec<String>> {
    use std::process::Command;
    let out = Command::new(crate::commands::worktree::git_bin())
        .args(["status", "--porcelain"])
        .current_dir(worktree_path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Porcelain lines are `XY path`, or `XY old -> new` for renames and
    // copies. We show the destination (the useful side); a naive
    // `l[3..].trim()` would surface `old -> new` as the dirty entry.
    let paths: Vec<String> = stdout
        .lines()
        .filter_map(porcelain_path)
        .filter(|p| !p.is_empty())
        .take(DIRTY_PATHS_UI_CAP + 1)
        .collect();
    Some(paths)
}

/// Extract the path (destination for renames) from a `git status --porcelain`
/// line. Lines are too short to be a status entry are skipped.
fn porcelain_path(line: &str) -> Option<String> {
    if line.len() < 4 {
        return None;
    }
    let rest = &line[3..];
    let path = if let Some((_old, new)) = rest.split_once(" -> ") {
        new.trim()
    } else {
        rest.trim()
    };
    Some(path.to_string())
}

pub const DIRTY_PATHS_UI_CAP: usize = 32;

/// Three-step pipeline advance. Each `rewrite` is the durable transition.
/// A crash between a `rewrite` and the following foreign action is caught
/// by the boot reaper.
/// Advance the 3-step pipeline. The caller passes a `prepared_out` buffer so
/// the handle/cwd minted by step 1 is observable after a step-3 failure
/// (post-step-2 errors return `cleanup_pending: true` with the real
/// prepared handle, not a null one).
async fn run_pipeline(
    app: &tauri::AppHandle,
    ctx: &crate::commands::route_to_context::ActiveContext,
    root: &Path,
    intent: &mut MoveIntent,
    params: &MoveWorkspaceParams,
    ws: &WorkspaceSnapshot,
    prepared_out: &mut Option<codesense_pty_core::remote_workspace::PreparedWorkspace>,
) -> Result<(), String> {
    let mut host = AppMoveHost {
        app,
        ctx: Some(ctx),
    };
    run_pipeline_with(root, intent, params, ws, prepared_out, &mut host).await
}

// Architecture: the live pipeline and boot recovery use one durable transition/cleanup seam.
trait MovePipelineHost {
    fn prepare(
        &mut self,
        root: &Path,
        intent: &mut MoveIntent,
        params: &MoveWorkspaceParams,
        ws: &WorkspaceSnapshot,
        start_ref: &str,
    ) -> impl std::future::Future<
        Output = Result<codesense_pty_core::remote_workspace::PreparedWorkspace, String>,
    > + Send;
    fn confirm_source(
        &mut self,
        intent: &MoveIntent,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;
    fn source_exists(
        &mut self,
        intent: &MoveIntent,
    ) -> impl std::future::Future<Output = Result<bool, String>> + Send;
    fn cleanup(
        &mut self,
        intent: &MoveIntent,
    ) -> impl std::future::Future<Output = Result<Option<String>, String>> + Send;
    fn changed(&mut self, intent: &MoveIntent);
    fn cleared(&mut self, intent: &MoveIntent);
}

struct AppMoveHost<'a> {
    app: &'a tauri::AppHandle,
    ctx: Option<&'a crate::commands::route_to_context::ActiveContext>,
}

impl MovePipelineHost for AppMoveHost<'_> {
    async fn prepare(
        &mut self,
        root: &Path,
        intent: &mut MoveIntent,
        params: &MoveWorkspaceParams,
        ws: &WorkspaceSnapshot,
        start_ref: &str,
    ) -> Result<codesense_pty_core::remote_workspace::PreparedWorkspace, String> {
        prepare_move_target(
            self.app,
            self.ctx.ok_or("move preparation context missing")?,
            root,
            intent,
            params,
            ws,
            start_ref,
        )
        .await
    }
    async fn confirm_source(&mut self, intent: &MoveIntent) -> Result<(), String> {
        let tabs = source_tabs(
            self.app,
            &intent.from.machine_id,
            intent.from.cwd.as_deref(),
        )
        .await?;
        no_source_agents(&tabs)
    }
    async fn source_exists(&mut self, intent: &MoveIntent) -> Result<bool, String> {
        if intent.from.machine_id.is_local() {
            let path = intent.from.cwd.clone().ok_or("source cwd missing")?;
            crate::commands::await_blocking("move_workspace_source_exists", move || {
                Path::new(&path)
                    .try_exists()
                    .map_err(|error| error.to_string())
            })
            .await
        } else {
            let ws = cleanup_snapshot(intent)?;
            match check_source_dirty(self.app, &ws, false).await {
                Ok(_) => Ok(true),
                Err(error)
                    if error.contains(codesense_pty_core::remote_workspace::WORKSPACE_MISSING) =>
                {
                    Ok(false)
                }
                Err(error) => Err(error),
            }
        }
    }
    async fn cleanup(&mut self, intent: &MoveIntent) -> Result<Option<String>, String> {
        // Architecture: cleanup checks for an already-removed source before querying
        // its layout, then applies the existing fenced safety checks before removal.
        cleanup_source_from_intent(self.app, intent).await
    }

    fn changed(&mut self, intent: &MoveIntent) {
        emit_hydrate(self.app, intent);
        emit_progress(
            self.app,
            &intent.team_key,
            &intent.workspace_id,
            intent.step,
            intent.cleanup_warning.clone(),
        );
    }
    fn cleared(&mut self, intent: &MoveIntent) {
        emit_cleared(self.app, &intent.team_key, &intent.workspace_id);
    }
}

async fn run_pipeline_with<H: MovePipelineHost>(
    root: &Path,
    intent: &mut MoveIntent,
    params: &MoveWorkspaceParams,
    ws: &WorkspaceSnapshot,
    prepared_out: &mut Option<codesense_pty_core::remote_workspace::PreparedWorkspace>,
    host: &mut H,
) -> Result<(), String> {
    // Policy: carry selects the source branch itself; discard follows the configured base.
    let start_ref = if params.carry_unpushed {
        format!("refs/heads/{}", ws.branch)
    } else if ws.base_ref.is_empty() {
        // An empty base is a local workspace's (remote snapshots carry their stored base):
        // resolve its clone's default branch once, here, so preparation and the receipt
        // check agree on it. A remote workspace with an empty stored base refuses rather
        // than guessing a branch.
        crate::commands::worktree::resolve_story_start_point(Path::new(&ws.repo_path))?
    } else {
        ws.base_ref.clone()
    };
    let prepared = host.prepare(root, intent, params, ws, &start_ref).await?;
    record_prepared(root, intent, &prepared)?;
    *prepared_out = Some(prepared);
    if let Err(error) = host.confirm_source(intent).await {
        if is_source_refusal(&error) {
            resolve_source_refusal(root, intent, &error)?;
            host.cleared(intent);
        }
        return Err(error);
    }
    intent.step = MoveStep::SwappingLocation;
    intent.last_progress_at_ms = now_epoch_ms();
    rewrite(root, intent).map_err(|e| format!("intent rewrite: {e:?}"))?;
    host.changed(intent);
    resume_intent_with(root, intent, host).await
}

async fn resume_intent_with<H: MovePipelineHost>(
    root: &Path,
    intent: &mut MoveIntent,
    host: &mut H,
) -> Result<(), String> {
    if intent.step == MoveStep::PreparingTarget {
        rollback_preparing(root, intent)?;
    } else {
        // Architecture: a retained flag from an earlier attempt is not fresh proof.
        if intent.source_kept {
            intent.cleanup_warning = None;
        }
        intent.source_kept = false;
        // Architecture: only a retry's fresh missing-source confirmation completes
        // uncertain cleanup, before target-tip or agent checks can refuse it.
        if intent.step == MoveStep::CleaningSource && !host.source_exists(intent).await? {
            intent.failure = None;
            delete(root, &intent.team_key, &intent.workspace_id)
                .map_err(|error| format!("intent clear: {error:?}"))?;
            host.cleared(intent);
            return Ok(());
        }
        if intent.step == MoveStep::SwappingLocation {
            intent.step = MoveStep::CleaningSource;
            intent.last_progress_at_ms = now_epoch_ms();
            rewrite(root, intent).map_err(|e| format!("intent rewrite: {e:?}"))?;
            host.changed(intent);
        }
        let cleaned = match host.cleanup(intent).await {
            // Architecture: sourceKept requires a fresh positive presence check
            // immediately after the failed step; every other outcome stays pending.
            Err(error) => match host.source_exists(intent).await {
                Ok(true) => {
                    let reason = format!("workspace moved; source kept: {error}");
                    intent.source_kept = true;
                    intent.cleanup_warning = Some(reason.clone());
                    resolve_source_refusal(root, intent, &reason)?;
                    host.changed(intent);
                    host.cleared(intent);
                    return Ok(());
                }
                Ok(false) => {
                    return Err(format!(
                        "cleanup pending: {error}; source missing; reaper will confirm"
                    ))
                }
                Err(check) => {
                    return Err(format!(
                        "cleanup pending: {error}; source presence unknown: {check}"
                    ))
                }
            },
            Ok(cleaned) => cleaned,
        };
        if let Some(warning) = cleaned {
            intent.cleanup_warning = Some(warning.clone());
            // Architecture: persist and publish the leftover tip for both live moves and recovery.
            rewrite(root, intent).map_err(|e| format!("intent warning: {e:?}"))?;
            log::warn!(
                "move_workspace {}#{}: {warning}",
                intent.team_key,
                intent.workspace_id
            );
            host.changed(intent);
        }
        delete(root, &intent.team_key, &intent.workspace_id)
            .map_err(|e| format!("intent clear: {e:?}"))?;
    }
    host.cleared(intent);
    Ok(())
}

async fn prepare_move_target(
    app: &tauri::AppHandle,
    ctx: &crate::commands::route_to_context::ActiveContext,
    root: &Path,
    intent: &mut MoveIntent,
    params: &MoveWorkspaceParams,
    ws: &WorkspaceSnapshot,
    start_ref: &str,
) -> Result<codesense_pty_core::remote_workspace::PreparedWorkspace, String> {
    // Architecture: replay never chooses carry/discard again; failed prepares roll back at boot.
    let mut target_ws = ws.clone();
    if intent.to.machine_id.is_local() {
        let clone = super::remote_workspace_bundle::local_seed(ctx, &ws.source_repo)?
            .ok_or("no attached local clone for the target")?;
        target_ws.repo_path = clone.to_string_lossy().into_owned();
        if params.carry_unpushed {
            let fetched = super::remote_workspace_fetch::fetch(
                app.clone(),
                super::remote_workspace_fetch::RemoteWorkspaceFetchParams {
                    machine_id: ws.machine_id.0.clone(),
                    workspace_handle: ws.handle.clone(),
                    story_id: Some(intent.workspace_id.clone()),
                    team_key: Some(intent.team_key.clone()),
                    ..Default::default()
                },
                None,
            )
            .await?;
            // Architecture: divergence, a local-ahead branch, or a changing remote never discards work.
            require_tip(intent.source_tip.as_deref(), Some(&fetched.commit))?;
            let branch_tip = local_git(
                &target_ws.repo_path,
                &["rev-parse", &format!("refs/heads/{}", ws.branch)],
            )?;
            require_tip(intent.source_tip.as_deref(), Some(&branch_tip))?;
        }
    }
    target_ws.base_ref = start_ref.to_string();
    // Architecture: resolve the chosen start before prepare; target receipts must match exactly.
    let seed = super::remote_workspace_bundle::local_seed(ctx, &ws.source_repo)?
        .ok_or("no attached local clone for target verification")?;
    let seed_path = seed.to_string_lossy().into_owned();
    let expected_ref = if intent.to.machine_id.is_local() {
        crate::commands::worktree::story_worktree_start_point(&seed, &ws.branch, start_ref)
            .reference
    } else {
        start_ref.to_string()
    };
    let expected = local_git(
        &seed_path,
        &["rev-parse", &format!("{expected_ref}^{{commit}}")],
    )?;
    if params.carry_unpushed {
        require_tip(intent.source_tip.as_deref(), Some(&expected))?;
    }
    intent.expected_tip = Some(expected);
    rewrite(root, intent).map_err(|e| format!("intent rewrite: {e:?}"))?;
    // Step 1 — prepare target.
    emit_progress(
        app,
        &intent.team_key,
        &intent.workspace_id,
        MoveStep::PreparingTarget,
        Some("preparing worktree on target".into()),
    );
    let prepared = if intent.to.machine_id.is_local() {
        let target_wt = params
            .target_worktree_path
            .clone()
            .ok_or_else(|| PreflightRefusal::MissingTargetPath.to_string())?;
        local_prepare_workspace(&target_ws, &target_wt).await?
    } else {
        let target_machine = intent
            .to
            .machine_id
            .0
            .clone()
            .ok_or_else(|| "remote move missing machine id".to_string())?;
        // Architecture: target paths belong to the target account, never the source host.
        let home = crate::commands::pty_layout::host_home(app, &target_machine)
            .await
            .map_err(|error| format!("target home unavailable: {error:?}"))?;
        let (repo_path, worktree_path) = super::remote_story_workspace::story_workspace_paths(
            &home,
            &ws.source_repo,
            &intent.team_key,
            &intent.workspace_id,
        )?;
        let prepare_params = super::remote_workspace::PrepareRemoteWorkspaceParams {
            request_id: intent.to.request_id.clone(),
            machine_id: target_machine,
            team_key: intent.team_key.clone(),
            workspace_id: intent.workspace_id.clone(),
            source_repo: ws.source_repo.clone(),
            base_ref: start_ref.to_string(),
            branch: ws.branch.clone(),
            repo_path,
            worktree_path,
        };
        let prepared = super::remote_workspace::prepare(app.clone(), prepare_params).await;
        if prepared.status == "refused" {
            return Err(PreflightRefusal::TargetUnreachable(
                prepared.message.unwrap_or_else(|| "refused".into()),
            )
            .to_string());
        }
        if prepared.status != "prepared" && prepared.status != "alreadyPrepared" {
            return Err(format!(
                "target prepare returned status: {} ({})",
                prepared.status,
                prepared.message.as_deref().unwrap_or("")
            ));
        }
        prepared
    };
    Ok(prepared)
}

// Architecture: target proof is durable before any location swap can release the source.
fn record_prepared(
    root: &Path,
    intent: &mut MoveIntent,
    prepared: &codesense_pty_core::remote_workspace::PreparedWorkspace,
) -> Result<(), String> {
    require_tip(
        intent.expected_tip.as_deref(),
        prepared.resolved_commit.as_deref(),
    )?;
    intent.target_cwd = prepared.cwd.clone();
    intent.target_handle = prepared.handle.clone();
    intent.last_progress_at_ms = now_epoch_ms();
    rewrite(root, intent).map_err(|e| format!("intent rewrite failed: {e:?}"))
}

fn record_failure(root: &Path, intent: &mut MoveIntent, error: &str) -> Result<(), String> {
    intent.failure = Some(error.to_string());
    rewrite(root, intent).map_err(|e| format!("intent rewrite failed: {e:?}"))
}

/// Thin wrapper over `fetch_and_create_worktree` that produces a
/// `PreparedWorkspace`-shaped result for the local target path. Reuses the
/// existing idempotent local-create primitive; no carve needed (~30 LOC).
///
/// The synthetic `local-workspace-{64-hex}` handle shape matches
/// the Rust-side `remote-workspace-{digest(...)}` convention so FE code
/// that treats "handle" as an opaque string doesn't need to branch on
/// local-vs-remote. Downstream callers (`agent_launch`, `open_workspace`,
/// `list_sessions`, `list_hydrated`, `list_worktrees`) carry zero
/// handle-shape checks today, so the shape above is a conventional contract,
/// not a load-bearing one.
pub(super) async fn local_prepare_workspace(
    ws: &WorkspaceSnapshot,
    target_worktree_path: &str,
) -> Result<codesense_pty_core::remote_workspace::PreparedWorkspace, String> {
    // An empty base means the clone's default branch, resolved only for a new branch.
    let start_point = (!ws.base_ref.is_empty()).then(|| ws.base_ref.clone());
    // Honour the "alreadyPrepared" contract: if the target already
    // exists AND is a managed worktree (carries `.git`), the caller gets a
    // signal that step 1 was a no-op. Matches the shape
    // `prepare_remote_workspace` returns on idempotent re-run.
    let preexisted = std::path::Path::new(target_worktree_path)
        .join(".git")
        .exists();
    let created = crate::commands::worktree::fetch_and_create_worktree(
        ws.repo_path.clone(),
        ws.branch.clone(),
        target_worktree_path.to_string(),
        start_point,
        None,
    )
    .await?;
    let status = if preexisted {
        "alreadyPrepared".to_string()
    } else {
        "prepared".to_string()
    };
    Ok(codesense_pty_core::remote_workspace::PreparedWorkspace {
        status,
        handle: Some(local_workspace_handle(
            &ws.repo_path,
            &created.worktree_path,
        )),
        cwd: Some(created.worktree_path.clone()),
        repo_path: Some(ws.repo_path.clone()),
        repository_key: Some(ws.source_repo.remote_key()),
        resolved_commit: Some(local_git(&created.worktree_path, &["rev-parse", "HEAD"])?),
        branch: Some(ws.branch.clone()),
        message: None,
    })
}

/// Synthetic handle shape for a local workspace prepared via
/// `local_prepare_workspace`. The 64-hex suffix mirrors the Rust-side
/// `remote-workspace-{digest(origin_key:worktree_path)}` convention so FE
/// code that renders "handle" as opaque text doesn't need to branch on
/// local-vs-remote. The hash input combines repo_path + worktree_path
/// (sha256-ed into a 64-char hex) — unique per clone + worktree pair,
/// deterministic, cheap. Not load-bearing; downstream callers do not
/// parse this.
fn local_workspace_handle(repo_path: &str, worktree_path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(repo_path.as_bytes());
    hasher.update(b"\0");
    hasher.update(worktree_path.as_bytes());
    format!("local-workspace-{:x}", hasher.finalize())
}

/// Remove the verified source through Git or the remote `remove_worktree` RPC.
async fn cleanup_source(
    app: &tauri::AppHandle,
    intent: &MoveIntent,
    ws: &WorkspaceSnapshot,
) -> Result<Option<String>, String> {
    if ws.machine_id.is_local() {
        if !Path::new(intent.from.cwd.as_deref().ok_or("source cwd missing")?).exists() {
            return Ok(None);
        }
        // Architecture: acquire the shared app/sidecar fence BEFORE the fresh
        // session read, and hold it through removal. Pending launches refuse it.
        let removing = codesense_pty_core::remote_workspace::removal::begin_removal(
            intent.from.cwd.as_deref().ok_or("source cwd missing")?,
        )?;
        let tabs = source_tabs(app, &intent.from.machine_id, intent.from.cwd.as_deref()).await?;
        no_source_agents(&tabs)?;
        let intent = intent.clone();
        crate::commands::await_blocking("move_workspace_local_cleanup", move || {
            let _removing = removing;
            cleanup_local_worktree_fenced(&intent, || Ok(()))
        })
        .await
    } else {
        match check_source_dirty(app, ws, false).await {
            Ok(status) => check_cleanup(intent, &status)?,
            // Architecture: main's typed missing-path prefix preserves idempotent cleanup retries.
            Err(error)
                if error.contains(codesense_pty_core::remote_workspace::WORKSPACE_MISSING) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(error),
        }
        no_source_agents(&source_tabs(app, &ws.machine_id, intent.from.cwd.as_deref()).await?)?;
        let machine_id = ws
            .machine_id
            .0
            .clone()
            .ok_or_else(|| "remote cleanup needs a machine id".to_string())?;
        let backend = crate::commands::pty_backend::resolve_spawn_backend(
            app,
            &machine_id,
            "move_workspace_remote_cleanup",
        )?;
        backend.revalidate(app)?;
        let client = backend
            .client()
            .filter(|_| backend.is_remote())
            .ok_or_else(|| "remote cleanup unavailable (local backend)".to_string())?;
        let lease = client
            .private_lease()
            .ok_or("private authority unavailable")?;
        lease.validate()?;
        let request = codesense_pty_core::remote_workspace::RemoveWorktree {
            expected_tip: intent.source_tip.clone().ok_or("source tip missing")?,
            discard_uncommitted: intent.dirty_allowed,
            owner_id: lease.owner_id(),
            repository_key: ws.source_repo.remote_key(),
            repo_path: ws.repo_path.clone(),
            worktree_path: ws.worktree_path.clone(),
        };
        let result = client
            .remove_worktree(request)
            .map_err(|e| format!("remote remove_worktree failed: {e}"))?;
        Ok(result.note.filter(|_| result.removed))
    }
}

/// Boot reaper. Scans every open `MoveIntent` and advances each to a
/// terminal state (completed OR rolled back). Per-intent isolation via
/// `tokio::spawn` so one failure doesn't abort the pass. Corrupt files are
/// log-and-skipped by `scan` itself (Pass-1 Rev 2).
pub async fn reap_move_workspace_intents(app: tauri::AppHandle) {
    let ctx = match crate::commands::route_to_context::derive_active_context(&app).await {
        Ok(c) => c,
        Err(e) => {
            log::warn!("move-workspace reaper: active context unavailable — {e}");
            return;
        }
    };
    let root = intent_root(&ctx.codesense_home, &ctx.principal());
    let intents = scan(&root);
    if intents.is_empty() {
        return;
    }
    log::info!(
        "move-workspace reaper: {} open intent(s) under {}",
        intents.len(),
        root.display()
    );
    // Resume each in parallel, isolated.
    let mut handles = Vec::new();
    for intent in intents {
        let Ok(executing) = ExecutingMove::acquire(&intent.team_key, &intent.workspace_id) else {
            continue;
        };
        // Architecture: refresh the scan's snapshot only after owning execution.
        let Ok(intent) = read(&root, &intent.team_key, &intent.workspace_id) else {
            continue;
        };
        emit_hydrate(&app, &intent);
        let app = app.clone();
        let root = root.clone();
        handles.push(tokio::spawn(async move {
            let _executing = executing;
            let root_resume = root.clone();
            if let Err(e) = resume_intent(app.clone(), root_resume, intent.clone()).await {
                log::warn!(
                    "move-workspace reaper: intent {}#{} failed to resume — {e}",
                    intent.team_key,
                    intent.workspace_id
                );
                // Architecture: retain the actual failed step for the next safe retry.
                let mut failed =
                    read(&root, &intent.team_key, &intent.workspace_id).unwrap_or(intent);
                let _ = record_failure(&root, &mut failed, &e);
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
}

async fn resume_intent(
    app: tauri::AppHandle,
    root: PathBuf,
    mut intent: MoveIntent,
) -> Result<(), String> {
    let mut host = AppMoveHost {
        app: &app,
        ctx: None,
    };
    resume_intent_with(&root, &mut intent, &mut host).await
}

// Architecture: absent proof, mismatching HEADs, and moved source tips retain the source.
fn require_tip(expected: Option<&str>, actual: Option<&str>) -> Result<(), String> {
    match (expected, actual) {
        (Some(expected), Some(actual)) if !expected.is_empty() && expected == actual => Ok(()),
        _ => Err(format!(
            "tip verification failed: expected {expected:?}, got {actual:?}; source retained"
        )),
    }
}

fn check_cleanup(
    intent: &MoveIntent,
    status: &codesense_pty_core::remote_workspace::RemoteWorktreeDirtyStatus,
) -> Result<(), String> {
    require_tip(intent.source_tip.as_deref(), Some(&status.source_tip)).map_err(|error| {
        format!(
            "{error}; leftover source tip {} stays on the source branch",
            status.source_tip
        )
    })?;
    if status.dirty && !intent.dirty_allowed {
        return Err("uncommitted changes appeared before cleanup; source retained".into());
    }
    Ok(())
}

async fn verify_target(app: &tauri::AppHandle, intent: &MoveIntent) -> Result<(), String> {
    let actual = if intent.to.machine_id.is_local() {
        local_git(
            intent
                .target_cwd
                .as_deref()
                .ok_or("verified target cwd missing")?,
            &["rev-parse", "HEAD"],
        )?
    } else {
        let ctx = crate::commands::route_to_context::derive_active_context(app).await?;
        let resolved = super::remote_workspace_target::resolve_selected(
            app.clone(),
            ctx,
            Some(intent.to.machine_id.as_machine_id()),
            intent
                .target_handle
                .as_deref()
                .ok_or("verified target handle missing")?,
            None,
            false,
        )
        .await?;
        (resolved.validate)()?;
        resolved
            .client
            .workspace_head(resolved.target)
            .map_err(|e| e.to_string())?
            .commit
    };
    require_tip(intent.expected_tip.as_deref(), Some(&actual))
}

// Architecture: live moves and boot recovery share this final, Git-aware removal.
#[cfg(test)]
fn cleanup_local_worktree(intent: &MoveIntent) -> Result<Option<String>, String> {
    cleanup_local_worktree_with(intent, || Ok(()))
}

#[cfg(test)]
fn cleanup_local_worktree_with(
    intent: &MoveIntent,
    before_remove: impl FnOnce() -> Result<(), String>,
) -> Result<Option<String>, String> {
    let _removing = codesense_pty_core::remote_workspace::removal::begin_removal(
        intent.from.cwd.as_deref().ok_or("source cwd missing")?,
    )?;
    cleanup_local_worktree_fenced(intent, before_remove)
}

/// Record the story branch tip of the worktree at `root` before a move removes it, in the
/// app's home. Tests never reach the real home: there it does nothing.
#[cfg(not(test))]
fn sight_before_removal(root: &Path) {
    if let Some(home) = crate::commands::paths::codesense_home_dir() {
        crate::install::story_tips::sight_worktree(&home, root);
    }
}

#[cfg(test)]
fn sight_before_removal(_root: &Path) {}

fn cleanup_local_worktree_fenced(
    intent: &MoveIntent,
    before_remove: impl FnOnce() -> Result<(), String>,
) -> Result<Option<String>, String> {
    if intent.to.machine_id.is_local() {
        let target = intent
            .target_cwd
            .as_deref()
            .ok_or("verified target cwd missing")?;
        require_tip(
            intent.expected_tip.as_deref(),
            Some(&local_git(target, &["rev-parse", "HEAD"])?),
        )?;
    }
    let path = intent.from.cwd.as_deref().ok_or("source cwd missing")?;
    if !Path::new(path).exists() {
        return Ok(None);
    }
    check_cleanup(intent, &local_source_status(path, false)?)?;
    let branch = local_git(path, &["symbolic-ref", "-q", "HEAD"]).ok();
    // Architecture: a commit made after the earlier check cannot be released by this move.
    require_tip(
        intent.source_tip.as_deref(),
        Some(&local_git(path, &["rev-parse", "HEAD"])?),
    )?;
    before_remove()?;
    // Keep the story branch's tip in the story's tips before the worktree goes.
    sight_before_removal(Path::new(path));
    // Policy: force only when the caller released uncommitted changes; Git otherwise guards late writes.
    let mut args = vec!["worktree", "remove"];
    if intent.dirty_allowed {
        args.push("--force");
    }
    args.extend(["--", path]);
    local_git(&intent.from.repo_path, &args)?;
    // Architecture: Git outside our fence may commit after the last HEAD check.
    // The source branch survives removal, so expose its leftover tip to the caller and reaper.
    Ok(branch.and_then(
        |branch| match local_git(&intent.from.repo_path, &["rev-parse", &branch]) {
            Ok(tip) if Some(tip.as_str()) != intent.source_tip.as_deref() => Some(format!(
                "Warning: leftover tip {tip} stays on the source branch {branch}"
            )),
            Ok(_) => None,
            Err(error) => Some(format!(
                "Warning: source branch {branch} could not be verified after removal: {error}"
            )),
        },
    ))
}

/// Resume-path cleanup — reads source worktree path + machine from the
/// intent's `from` block (which was captured at `move_workspace`-call time),
/// so we don't need the full `WorkspaceSnapshot` (which may be gone after a
/// restart that closed the workspace tile).
async fn cleanup_source_from_intent(
    app: &tauri::AppHandle,
    intent: &MoveIntent,
) -> Result<Option<String>, String> {
    verify_target(app, intent).await?;
    let ws = cleanup_snapshot(intent)?;
    cleanup_source(app, intent, &ws).await
}

fn cleanup_snapshot(intent: &MoveIntent) -> Result<WorkspaceSnapshot, String> {
    let source_repo = intent
        .source_repo
        .clone()
        .ok_or("cleanup source repository identity unavailable")?;
    Ok(WorkspaceSnapshot {
        machine_id: intent.from.machine_id.clone(),
        request_id: intent.from.request_id.clone(),
        handle: intent.from.handle.clone(),
        cwd: intent.from.cwd.clone(),
        source_repo,
        base_ref: String::new(),
        branch: String::new(),
        repo_path: intent.from.repo_path.clone(),
        worktree_path: intent.from.cwd.clone().ok_or("source cwd missing")?,
    })
}

// ────────────────────────────────────────────────────────────────────────────
// Tauri `#[tauri::command]` bridges so the FE can invoke `move_workspace`
// and `list_move_intents` without going through the MCP socket. Follows the
// `wv_signal_done_bridge` convention — these are thin wrappers that reuse
// the MCP tool bodies and nothing else.
// ────────────────────────────────────────────────────────────────────────────

/// Bridge for the `move_workspace` MCP tool. Called from the workspace
/// header "Move to another machine…" dialog. Returns the same shape the
/// MCP tool does.
#[tauri::command]
pub async fn move_workspace_bridge(
    app: tauri::AppHandle,
    params: MoveWorkspaceParams,
) -> Result<MoveWorkspaceResult, String> {
    crate::commands::interactive_gate::InteractiveGate::admit("move_workspace")
        .map_err(|e| e.to_string())?;
    let ctx = crate::commands::route_to_context::derive_active_context(&app).await?;
    let team = ctx
        .team_name
        .as_deref()
        .ok_or_else(|| "no team is selected".to_string())?;
    let team_key = crate::commands::pty_layout::team_key(&ctx.org_slug, Some(team));
    handle(app, team_key, params, None).await
}

/// Bridge for the `list_move_intents` MCP read tool. Called by the FE
/// viewer-store hook on init (pull-on-init pattern — see
/// `use-move-workspace-sync.ts`).
#[tauri::command]
pub async fn list_move_intents_bridge(
    app: tauri::AppHandle,
) -> Result<Vec<MoveIntentView>, String> {
    list_intents(app).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn explicit_move_choices_are_accepted() {
        let params = serde_json::from_value::<MoveWorkspaceParams>(serde_json::json!({
            "storyId": "S", "targetMachine": "local", "stopRunningAgents": true,
            "carryUnpushed": true, "discardUnpushed": false
        }));
        assert!(
            params.is_ok(),
            "explicit move choices must be accepted: {params:?}"
        );
    }

    fn choices() -> MoveWorkspaceParams {
        serde_json::from_value(serde_json::json!({ "storyId":"S", "targetMachine":"local" }))
            .unwrap()
    }

    #[test]
    fn refusals_include_the_entire_report_and_caller_choices_release_only_their_work() {
        let mut params = choices();
        let mut report = MoveSourceReport {
            running_agents: vec![MoveAgent {
                handle: "agent-handle".into(),
                name: "Builder".into(),
            }],
            uncommitted_paths: vec!["work.rs".into()],
            unpushed_count: 2,
            source_tip: "abc".into(),
            unpushed_check: "cached".into(),
            unpushed_check_reason: Some("offline".into()),
        };
        let error = check_choices(&params, &report, false, false).unwrap_err();
        for value in [
            "runningAgents",
            "agent-handle",
            "Builder",
            "work.rs",
            "cached",
            "offline",
            "abc",
        ] {
            assert!(error.contains(value), "{error}");
        }
        params.stop_running_agents = true;
        assert!(check_choices(&params, &report, true, false)
            .unwrap_err()
            .contains("selfClose"));
        assert!(check_choices(&params, &report, false, false)
            .unwrap_err()
            .contains("uncommitted"));
        params.discard_uncommitted = true;
        assert!(check_choices(&params, &report, false, false)
            .unwrap_err()
            .contains("unpushedCommits"));
        params.discard_unpushed = true;
        assert!(check_choices(&params, &report, false, false).is_ok());
        params.carry_unpushed = true;
        assert!(check_choices(&params, &report, false, false)
            .unwrap_err()
            .contains("invalid arguments"));
        params.discard_unpushed = false;
        assert!(check_choices(&params, &report, false, true)
            .unwrap_err()
            .contains("between machines"));
        assert!(check_choices(&params, &report, false, false).is_ok());
        report.running_agents.clear();
        report.uncommitted_paths.clear();
        report.unpushed_count = 0;
        assert!(check_choices(&choices(), &report, false, false).is_ok());
    }

    fn git(path: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new(crate::commands::worktree::git_bin())
            .current_dir(path)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().into()
    }

    #[test]
    fn source_tip_and_all_remote_refs_guard_cleanup_and_half_done_recovery() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["commit", "--allow-empty", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["update-ref", "refs/remotes/other/main", &base]);
        let source = dir.path().join("source");
        git(
            &repo,
            &["worktree", "add", "-b", "story", source.to_str().unwrap()],
        );
        git(&source, &["commit", "--allow-empty", "-m", "unpushed"]);
        let tip = git(&source, &["rev-parse", "HEAD"]);
        let target = dir.path().join("target");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                target.to_str().unwrap(),
                &tip,
            ],
        );

        assert_eq!(
            local_git(
                source.to_str().unwrap(),
                &["rev-list", "--count", &tip, "--not", "--remotes"]
            )
            .unwrap(),
            "1"
        );
        let root = dir.path().join("intents");
        let mut intent = sample_intent();
        intent.source_tip = Some(tip.clone());
        intent.expected_tip = Some(tip.clone());
        intent.from.cwd = Some(source.to_string_lossy().into_owned());
        intent.from.repo_path = repo.to_string_lossy().into_owned();
        intent.to.machine_id = MachineRef(None);
        intent.target_cwd = Some(target.to_string_lossy().into_owned());
        write_new(&root, &intent).unwrap();
        let prepared = codesense_pty_core::remote_workspace::PreparedWorkspace {
            status: "prepared".into(),
            handle: None,
            cwd: intent.target_cwd.clone(),
            repo_path: Some(intent.from.repo_path.clone()),
            repository_key: None,
            resolved_commit: Some(tip.clone()),
            branch: Some("story".into()),
            message: None,
        };
        record_prepared(&root, &mut intent, &prepared).unwrap();
        // Architecture: an injected failure after prepare preserves the failed step and source.
        record_failure(&root, &mut intent, "injected after prepare").unwrap();
        assert!(source.join(".git").exists());
        assert_eq!(
            read(&root, &intent.team_key, &intent.workspace_id)
                .unwrap()
                .step,
            MoveStep::PreparingTarget
        );
        intent.step = MoveStep::CleaningSource;
        rewrite(&root, &intent).unwrap();
        let recovered = scan(&root).pop().unwrap();
        let status = |tip: String, dirty: bool| {
            codesense_pty_core::remote_workspace::RemoteWorktreeDirtyStatus {
                source_tip: tip,
                dirty,
                ..Default::default()
            }
        };
        assert!(check_cleanup(&recovered, &status(tip.clone(), false)).is_ok());
        assert!(check_cleanup(&recovered, &status(tip.clone(), true)).is_err());
        git(&source, &["commit", "--allow-empty", "-m", "late work"]);
        let changed = git(&source, &["rev-parse", "HEAD"]);
        assert!(check_cleanup(&recovered, &status(changed, false)).is_err());
        assert!(source.join(".git").exists());
        assert!(cleanup_local_worktree(&recovered).is_err());
        assert!(
            source.join(".git").exists(),
            "a moved source tip is retained on recovery"
        );
        assert!(require_tip(Some(&tip), Some(&base)).is_err());
        assert!(require_tip(None, Some(&tip)).is_err());
        assert!(require_tip(Some(&tip), None).is_err());
    }

    #[test]
    fn verified_half_done_move_reaps_only_an_unchanged_source() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["commit", "--allow-empty", "-m", "base"]);
        let tip = git(&repo, &["rev-parse", "HEAD"]);
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        for path in [&source, &target] {
            git(
                &repo,
                &["worktree", "add", "--detach", path.to_str().unwrap(), &tip],
            );
        }
        let mut intent = sample_intent();
        intent.from.repo_path = repo.to_string_lossy().into_owned();
        intent.from.cwd = Some(source.to_string_lossy().into_owned());
        intent.to.machine_id = MachineRef(None);
        intent.target_cwd = Some(target.to_string_lossy().into_owned());
        intent.source_tip = Some(tip.clone());
        intent.expected_tip = Some(tip);
        intent.step = MoveStep::CleaningSource;
        let root = dir.path().join("intents");
        write_new(&root, &intent).unwrap();
        let recovered = scan(&root).pop().unwrap();
        std::fs::write(source.join("late.txt"), "late work").unwrap();
        assert!(cleanup_local_worktree(&recovered).is_err());
        assert!(source.exists());
        std::fs::remove_file(source.join("late.txt")).unwrap();
        git(
            &target,
            &["commit", "--allow-empty", "-m", "target changed"],
        );
        assert!(cleanup_local_worktree(&recovered).is_err());
        assert!(source.exists());
        git(
            &target,
            &[
                "checkout",
                "--detach",
                recovered.expected_tip.as_deref().unwrap(),
            ],
        );
        cleanup_local_worktree(&recovered).unwrap();
        assert!(!source.exists());
        assert!(target.join(".git").exists());
        cleanup_local_worktree(&recovered).unwrap();
    }

    #[test]
    fn local_remove_refuses_a_save_after_its_clean_check() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["commit", "--allow-empty", "-m", "base"]);
        let tip = git(&repo, &["rev-parse", "HEAD"]);
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        for path in [&source, &target] {
            git(
                &repo,
                &["worktree", "add", "--detach", path.to_str().unwrap(), &tip],
            );
        }
        let mut intent = sample_intent();
        intent.from.repo_path = repo.to_string_lossy().into_owned();
        intent.from.cwd = Some(source.to_string_lossy().into_owned());
        intent.source_tip = Some(tip.clone());
        intent.expected_tip = Some(tip);
        intent.target_cwd = Some(target.to_string_lossy().into_owned());
        intent.to.machine_id = MachineRef(None);
        let pending =
            codesense_pty_core::remote_workspace::removal::admit_spawn(source.to_str().unwrap())
                .unwrap();
        assert!(cleanup_local_worktree(&intent)
            .unwrap_err()
            .to_string()
            .contains("pending"));
        assert!(source.exists());
        drop(pending);
        let result = cleanup_local_worktree_with(&intent, || {
            assert!(codesense_pty_core::remote_workspace::removal::admit_spawn(
                source.to_str().unwrap()
            )
            .is_err());
            std::fs::write(source.join("late.txt"), "editor save").unwrap();
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(source.join("late.txt")).unwrap(),
            "editor save"
        );
        intent.dirty_allowed = true;
        cleanup_local_worktree(&intent).unwrap();
        assert!(!source.exists());
    }

    #[tokio::test]
    async fn pushed_story_above_main_uses_the_preparers_selected_tip() {
        for remote_only in [false, true] {
            let dir = tempdir().unwrap();
            let repo = dir.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            git(&repo, &["init", "-b", "main"]);
            git(&repo, &["commit", "--allow-empty", "-m", "base"]);
            let base = git(&repo, &["rev-parse", "HEAD"]);
            git(&repo, &["checkout", "-b", "story"]);
            git(&repo, &["commit", "--allow-empty", "-m", "pushed story"]);
            let tip = git(&repo, &["rev-parse", "HEAD"]);
            git(&repo, &["update-ref", "refs/remotes/origin/story", &tip]);
            git(&repo, &["checkout", "main"]);
            if remote_only {
                git(&repo, &["branch", "-D", "story"]);
            }
            let selected =
                crate::commands::worktree::story_worktree_start_point(&repo, "story", "main");
            let expected =
                local_git(repo.to_str().unwrap(), &["rev-parse", &selected.reference]).unwrap();
            let ws = WorkspaceSnapshot {
                machine_id: MachineRef(None),
                request_id: String::new(),
                handle: None,
                cwd: None,
                source_repo: SourceRepoSlug::new("example", "work").unwrap(),
                base_ref: "main".into(),
                branch: "story".into(),
                repo_path: repo.to_string_lossy().into_owned(),
                worktree_path: String::new(),
            };
            let prepared =
                local_prepare_workspace(&ws, dir.path().join("target").to_str().unwrap())
                    .await
                    .unwrap();
            assert_ne!(tip, base);
            assert_eq!(expected, tip);
            require_tip(Some(&expected), prepared.resolved_commit.as_deref()).unwrap();
        }
    }

    #[test]
    fn failed_prepares_are_retryable_while_their_step_and_reason_remain_archived() {
        let dir = tempdir().unwrap();
        let mut failed = sample_intent();
        write_new(dir.path(), &failed).unwrap();
        record_failure(dir.path(), &mut failed, "prepare failed").unwrap();
        let mut retry = failed.clone();
        retry.to.request_id = "retry".into();
        retry.failure = None;
        write_new(dir.path(), &retry).unwrap();
        assert_eq!(
            read(dir.path(), &retry.team_key, &retry.workspace_id).unwrap(),
            retry
        );
        let archived: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "failed"))
            .collect();
        assert_eq!(archived.len(), 1);
        let evidence: MoveIntent =
            serde_json::from_slice(&std::fs::read(archived[0].path()).unwrap()).unwrap();
        assert_eq!(evidence, failed);
        assert_eq!(scan(dir.path()), vec![retry.clone()]);
        assert_eq!(
            write_new(dir.path(), &retry),
            Err(IntentWriteError::AlreadyExists)
        );
        rollback_preparing(dir.path(), &failed).unwrap();
        assert_eq!(
            read(dir.path(), &retry.team_key, &retry.workspace_id).unwrap(),
            retry
        );
        rollback_preparing(dir.path(), &retry).unwrap();
        assert!(scan(dir.path()).is_empty());
        write_new(dir.path(), &retry).unwrap();
    }

    struct FakeMoveHost {
        target: PathBuf,
        fail_after_prepare: bool,
        commit_during_cleanup: bool,
        commit_during_prepare: bool,
        cleanup_failure: Option<String>,
        source_failure: Option<String>,
        starts: Vec<String>,
        steps: Vec<MoveStep>,
        cleanups: usize,
    }

    impl MovePipelineHost for FakeMoveHost {
        async fn prepare(
            &mut self,
            root: &Path,
            intent: &mut MoveIntent,
            _params: &MoveWorkspaceParams,
            ws: &WorkspaceSnapshot,
            start_ref: &str,
        ) -> Result<codesense_pty_core::remote_workspace::PreparedWorkspace, String> {
            self.starts.push(start_ref.to_string());
            let expected = local_git(&ws.repo_path, &["rev-parse", start_ref])?;
            intent.expected_tip = Some(expected.clone());
            rewrite(root, intent).map_err(|e| format!("test intent: {e:?}"))?;
            local_git(
                &ws.repo_path,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    self.target.to_str().unwrap(),
                    start_ref,
                ],
            )?;
            if self.fail_after_prepare {
                return Err("injected after prepare".into());
            }
            if self.commit_during_prepare {
                git(
                    Path::new(&ws.worktree_path),
                    &["commit", "--allow-empty", "-m", "work during preparation"],
                );
            }
            Ok(codesense_pty_core::remote_workspace::PreparedWorkspace {
                status: "prepared".into(),
                handle: Some("mock-target".into()),
                cwd: Some(self.target.to_string_lossy().into_owned()),
                repo_path: Some(ws.repo_path.clone()),
                repository_key: Some(ws.source_repo.remote_key()),
                resolved_commit: Some(expected),
                branch: Some(ws.branch.clone()),
                message: None,
            })
        }
        async fn confirm_source(&mut self, _intent: &MoveIntent) -> Result<(), String> {
            self.source_failure.clone().map_or(Ok(()), Err)
        }
        async fn source_exists(&mut self, intent: &MoveIntent) -> Result<bool, String> {
            Path::new(intent.from.cwd.as_deref().ok_or("source cwd missing")?)
                .try_exists()
                .map_err(|error| error.to_string())
        }
        async fn cleanup(&mut self, intent: &MoveIntent) -> Result<Option<String>, String> {
            self.cleanups += 1;
            if let Some(error) = &self.cleanup_failure {
                return Err(error.clone());
            }
            self.confirm_source(intent).await?;
            require_tip(
                intent.expected_tip.as_deref(),
                Some(&local_git(
                    self.target.to_str().unwrap(),
                    &["rev-parse", "HEAD"],
                )?),
            )?;
            cleanup_local_worktree_with(intent, || {
                if self.commit_during_cleanup {
                    git(
                        Path::new(intent.from.cwd.as_deref().unwrap()),
                        &["commit", "--allow-empty", "-m", "late external commit"],
                    );
                }
                Ok(())
            })
        }
        fn changed(&mut self, intent: &MoveIntent) {
            self.steps.push(intent.step);
        }
        fn cleared(&mut self, _intent: &MoveIntent) {}
    }

    fn pipeline_fixture() -> (
        tempfile::TempDir,
        PathBuf,
        MoveIntent,
        WorkspaceSnapshot,
        FakeMoveHost,
    ) {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["commit", "--allow-empty", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", &base]);
        let source = dir.path().join("source");
        git(
            &repo,
            &["worktree", "add", "-b", "story", source.to_str().unwrap()],
        );
        std::fs::write(source.join("private.txt"), "unpushed work").unwrap();
        git(&source, &["add", "private.txt"]);
        git(&source, &["commit", "-m", "private work"]);
        let mut intent = sample_intent();
        intent.from.repo_path = repo.to_string_lossy().into_owned();
        intent.from.cwd = Some(source.to_string_lossy().into_owned());
        intent.source_tip = Some(git(&source, &["rev-parse", "HEAD"]));
        let ws = WorkspaceSnapshot {
            machine_id: MachineRef(None),
            request_id: String::new(),
            handle: None,
            cwd: intent.from.cwd.clone(),
            source_repo: SourceRepoSlug::new("example", "work").unwrap(),
            base_ref: "main".into(),
            branch: "story".into(),
            repo_path: intent.from.repo_path.clone(),
            worktree_path: source.to_string_lossy().into_owned(),
        };
        intent.source_repo = Some(ws.source_repo.clone());
        let root = dir.path().join("intents");
        write_new(&root, &intent).unwrap();
        let host = FakeMoveHost {
            target: dir.path().join("target"),
            fail_after_prepare: false,
            commit_during_cleanup: false,
            commit_during_prepare: false,
            cleanup_failure: None,
            source_failure: None,
            starts: Vec::new(),
            steps: Vec::new(),
            cleanups: 0,
        };
        (dir, root, intent, ws, host)
    }

    #[tokio::test]
    async fn pipeline_selects_carry_or_discard_from_the_callers_flags_and_finishes_cleanup() {
        for carry in [false, true] {
            let (_dir, root, mut intent, ws, mut host) = pipeline_fixture();
            let mut params = choices();
            params.carry_unpushed = carry;
            params.discard_unpushed = !carry;
            let status = local_source_status(&ws.worktree_path, false).unwrap();
            assert_eq!(status.unpushed_count, 1);
            assess_source(&params, status, &[], None, false).unwrap();
            let mut prepared = None;
            run_pipeline_with(&root, &mut intent, &params, &ws, &mut prepared, &mut host)
                .await
                .unwrap();
            assert_eq!(
                host.starts,
                vec![if carry { "refs/heads/story" } else { "main" }]
            );
            let expected = if carry {
                intent.source_tip.clone().unwrap()
            } else {
                git(Path::new(&ws.repo_path), &["rev-parse", "main"])
            };
            assert_eq!(git(&host.target, &["rev-parse", "HEAD"]), expected);
            assert_eq!(host.target.join("private.txt").exists(), carry);
            assert!(!Path::new(&ws.worktree_path).exists());
            assert_eq!(
                host.steps,
                vec![MoveStep::SwappingLocation, MoveStep::CleaningSource]
            );
            assert_eq!(host.cleanups, 1);
            assert!(scan(&root).is_empty());
        }
    }

    /// The pipeline fixture's clone, renamed so its only default is `trunk` (local and
    /// cached, `origin/HEAD` → trunk): no `main` anywhere.
    fn rename_default_to_trunk(repo: &Path) -> String {
        git(repo, &["branch", "-m", "main", "trunk"]);
        let base = git(repo, &["rev-parse", "trunk"]);
        git(repo, &["update-ref", "-d", "refs/remotes/origin/main"]);
        git(repo, &["update-ref", "refs/remotes/origin/trunk", &base]);
        git(
            repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        base
    }

    #[tokio::test]
    async fn a_discard_move_with_no_recorded_base_starts_from_the_repos_default_branch() {
        let (_dir, root, mut intent, mut ws, mut host) = pipeline_fixture();
        let base = rename_default_to_trunk(Path::new(&ws.repo_path));
        ws.base_ref = String::new();
        let mut params = choices();
        params.discard_unpushed = true;

        run_pipeline_with(&root, &mut intent, &params, &ws, &mut None, &mut host)
            .await
            .unwrap();

        // The cached remote ref wins over the local branch of the same name.
        assert_eq!(host.starts, vec!["refs/remotes/origin/trunk"]);
        assert_eq!(git(&host.target, &["rev-parse", "HEAD"]), base);
    }

    #[tokio::test]
    async fn a_discard_move_with_no_resolvable_default_refuses_before_preparing() {
        let (_dir, root, mut intent, mut ws, mut host) = pipeline_fixture();
        let repo = PathBuf::from(&ws.repo_path);
        git(&repo, &["checkout", "--detach"]);
        git(&repo, &["branch", "-D", "main"]);
        git(&repo, &["update-ref", "-d", "refs/remotes/origin/main"]);
        ws.base_ref = String::new();
        let mut params = choices();
        params.discard_unpushed = true;

        let error = run_pipeline_with(&root, &mut intent, &params, &ws, &mut None, &mut host)
            .await
            .unwrap_err();

        assert!(
            error.contains("git remote set-head origin --auto"),
            "{error}"
        );
        assert!(
            host.starts.is_empty(),
            "nothing is prepared: {:?}",
            host.starts
        );
        assert!(Path::new(&ws.worktree_path).exists(), "the source stays");
    }

    #[tokio::test]
    async fn local_prepare_with_no_recorded_base_mints_from_the_repos_default_branch() {
        let (dir, _root, _intent, mut ws, _host) = pipeline_fixture();
        let base = rename_default_to_trunk(Path::new(&ws.repo_path));
        ws.base_ref = String::new();
        ws.branch = "feat/new-story".into();
        let target = dir.path().join("prepared");

        let prepared = local_prepare_workspace(&ws, target.to_str().unwrap())
            .await
            .unwrap();

        assert_eq!(prepared.resolved_commit.as_deref(), Some(base.as_str()));
    }

    #[tokio::test]
    async fn pipeline_and_reaper_report_a_commit_landing_after_the_final_checks() {
        for recover in [false, true] {
            let (_dir, root, mut intent, ws, mut host) = pipeline_fixture();
            host.commit_during_cleanup = true;
            let mut params = choices();
            params.carry_unpushed = true;
            let original_tip = intent.source_tip.clone().unwrap();
            if recover {
                let prepared = host
                    .prepare(&root, &mut intent, &params, &ws, "refs/heads/story")
                    .await
                    .unwrap();
                record_prepared(&root, &mut intent, &prepared).unwrap();
                intent.step = MoveStep::CleaningSource;
                rewrite(&root, &intent).unwrap();
                let mut recovered = read(&root, &intent.team_key, &intent.workspace_id).unwrap();
                resume_intent_with(&root, &mut recovered, &mut host)
                    .await
                    .unwrap();
                intent = recovered;
            } else {
                run_pipeline_with(&root, &mut intent, &params, &ws, &mut None, &mut host)
                    .await
                    .unwrap();
            }
            let leftover = git(Path::new(&ws.repo_path), &["rev-parse", "refs/heads/story"]);
            assert_ne!(leftover, original_tip);
            assert_eq!(git(&host.target, &["rev-parse", "HEAD"]), original_tip);
            assert!(!Path::new(&ws.worktree_path).exists());
            let warning = intent.cleanup_warning.unwrap();
            assert!(warning.contains(&leftover));
            assert!(warning.contains("stays on the source branch refs/heads/story"));
            assert!(scan(&root).is_empty());
        }
    }

    #[test]
    fn executing_move_refuses_duplicates_without_touching_the_intent_until_drop() {
        let dir = tempdir().unwrap();
        let intent = sample_intent();
        write_new(dir.path(), &intent).unwrap();
        let run = ExecutingMove::acquire(&intent.team_key, &intent.workspace_id).unwrap();
        assert_eq!(
            ExecutingMove::acquire(&intent.team_key, &intent.workspace_id).unwrap_err(),
            PreflightRefusal::InProgress.to_string()
        );
        assert_eq!(
            read(dir.path(), &intent.team_key, &intent.workspace_id).unwrap(),
            intent
        );
        let _other = ExecutingMove::acquire(&intent.team_key, "another-workspace").unwrap();
        drop(run);
        let _retry = ExecutingMove::acquire(&intent.team_key, &intent.workspace_id).unwrap();
    }

    #[tokio::test]
    async fn cleanup_rechecks_source_presence_after_a_failed_remove() {
        for confirmed in [false, true] {
            let (_dir, root, mut intent, ws, mut host) = pipeline_fixture();
            let prepared = host
                .prepare(&root, &mut intent, &choices(), &ws, "refs/heads/story")
                .await
                .unwrap();
            record_prepared(&root, &mut intent, &prepared).unwrap();
            intent.step = MoveStep::SwappingLocation;
            rewrite(&root, &intent).unwrap();
            host.cleanup_failure = Some(if confirmed {
                "safety guard refused removal".into()
            } else {
                // The source was removed, but its reply was lost.
                git(
                    Path::new(&ws.repo_path),
                    &["worktree", "remove", &ws.worktree_path],
                );
                "remove reply lost".into()
            });
            let resumed = resume_intent_with(&root, &mut intent, &mut host).await;
            if confirmed {
                resumed.unwrap();
                assert_completed_with_source_kept(&root, &intent, &ws);
            } else {
                let error = resumed.unwrap_err();
                record_failure(&root, &mut intent, &error).unwrap();
                let result =
                    pending_move_result(&intent, MoveSourceReport::default(), vec![], error);
                assert!(result.moved && result.cleanup_pending && !result.source_kept);
                assert_eq!(
                    read(&root, &intent.team_key, &intent.workspace_id)
                        .unwrap()
                        .step,
                    MoveStep::CleaningSource
                );
                host.cleanup_failure = None;
                resume_intent_with(&root, &mut intent, &mut host)
                    .await
                    .unwrap();
                assert!(
                    scan(&root).is_empty(),
                    "already-removed source is a soft miss"
                );
            }
        }
    }

    #[tokio::test]
    async fn lost_remove_reply_and_changed_target_stay_pending_until_the_missing_source_reaps() {
        let (_dir, root, mut intent, ws, mut host) = pipeline_fixture();
        let prepared = host
            .prepare(&root, &mut intent, &choices(), &ws, "refs/heads/story")
            .await
            .unwrap();
        record_prepared(&root, &mut intent, &prepared).unwrap();
        intent.step = MoveStep::SwappingLocation;
        rewrite(&root, &intent).unwrap();
        git(
            Path::new(&ws.repo_path),
            &["worktree", "remove", &ws.worktree_path],
        );
        git(
            &host.target,
            &["commit", "--allow-empty", "-m", "target changed after move"],
        );
        host.cleanup_failure = Some("remove reply lost".into());
        let error = resume_intent_with(&root, &mut intent, &mut host)
            .await
            .unwrap_err();
        record_failure(&root, &mut intent, &error).unwrap();
        let pending = pending_move_result(&intent, MoveSourceReport::default(), vec![], error);
        assert!(pending.moved && pending.cleanup_pending && !pending.source_kept);
        let mut recovered = read(&root, &intent.team_key, &intent.workspace_id).unwrap();
        assert_eq!(recovered.step, MoveStep::CleaningSource);
        assert!(!recovered.source_kept);
        host.cleanup_failure = None;
        resume_intent_with(&root, &mut recovered, &mut host)
            .await
            .unwrap();
        let completed = completed_move_result(&recovered, MoveSourceReport::default(), vec![]);
        assert!(completed.moved && !completed.cleanup_pending && !completed.source_kept);
        assert!(completed.reason.is_none());
        assert_eq!(
            host.cleanups, 1,
            "the retry checks absence before target verification"
        );
        assert!(scan(&root).is_empty());
    }

    fn assert_completed_with_source_kept(root: &Path, intent: &MoveIntent, ws: &WorkspaceSnapshot) {
        let result = completed_move_result(intent, MoveSourceReport::default(), Vec::new());
        let value = serde_json::to_value(result).unwrap();
        assert_eq!(value["moved"], true);
        assert_eq!(value["sourceKept"], true);
        assert_eq!(
            value["to"],
            serde_json::to_value(&intent.to.machine_id).unwrap()
        );
        assert_eq!(value["handle"], intent.target_handle.clone().unwrap());
        assert_eq!(value["cwd"], intent.target_cwd.clone().unwrap());
        assert!(value.get("cleanupPending").is_none());
        assert!(Path::new(&ws.worktree_path).join("private.txt").exists());
        assert!(scan(root).is_empty());
        let archived = std::fs::read_dir(root)
            .unwrap()
            .flatten()
            .find(|entry| entry.path().extension().is_some_and(|ext| ext == "failed"))
            .unwrap();
        let evidence: MoveIntent =
            serde_json::from_slice(&std::fs::read(archived.path()).unwrap()).unwrap();
        assert!(evidence.source_kept);
        assert_eq!(evidence.failure, intent.cleanup_warning);
        assert_eq!(evidence.target_cwd, intent.target_cwd);
        let mut next = sample_intent();
        next.to.request_id = "new-move".into();
        write_new(root, &next).unwrap();
    }

    #[tokio::test]
    async fn post_swap_cleanup_refusals_complete_at_the_recorded_target_and_release_the_slot() {
        for recover in [false, true] {
            for refusal in [
                codesense_pty_core::layout::source_refusal("runningAgents: stop the agents"),
                codesense_pty_core::layout::source_refusal("start or restart the terminal server"),
                "uncommitted changes appeared before cleanup; source retained".into(),
            ] {
                let (_dir, root, mut intent, ws, mut host) = pipeline_fixture();
                let mut params = choices();
                params.carry_unpushed = true;
                host.cleanup_failure = Some(refusal.clone());
                if recover {
                    let prepared = host
                        .prepare(&root, &mut intent, &params, &ws, "refs/heads/story")
                        .await
                        .unwrap();
                    record_prepared(&root, &mut intent, &prepared).unwrap();
                    intent.step = MoveStep::SwappingLocation;
                    rewrite(&root, &intent).unwrap();
                    resume_intent_with(&root, &mut intent, &mut host)
                        .await
                        .unwrap();
                } else {
                    run_pipeline_with(&root, &mut intent, &params, &ws, &mut None, &mut host)
                        .await
                        .unwrap();
                }
                assert!(intent
                    .cleanup_warning
                    .as_deref()
                    .unwrap()
                    .contains(&refusal));
                assert_completed_with_source_kept(&root, &intent, &ws);
            }
        }
    }

    #[tokio::test]
    async fn source_commit_during_preparation_completes_with_leftover_tip_and_no_pending_intent() {
        for recover in [false, true] {
            let (_dir, root, mut intent, ws, mut host) = pipeline_fixture();
            let mut params = choices();
            params.carry_unpushed = true;
            host.commit_during_prepare = true;
            let original = intent.source_tip.clone().unwrap();
            if recover {
                let prepared = host
                    .prepare(&root, &mut intent, &params, &ws, "refs/heads/story")
                    .await
                    .unwrap();
                record_prepared(&root, &mut intent, &prepared).unwrap();
                intent.step = MoveStep::CleaningSource;
                rewrite(&root, &intent).unwrap();
                resume_intent_with(&root, &mut intent, &mut host)
                    .await
                    .unwrap();
            } else {
                run_pipeline_with(&root, &mut intent, &params, &ws, &mut None, &mut host)
                    .await
                    .unwrap();
            }
            let leftover = git(Path::new(&ws.worktree_path), &["rev-parse", "HEAD"]);
            assert_ne!(leftover, original);
            assert_eq!(git(&host.target, &["rev-parse", "HEAD"]), original);
            let reason = intent.cleanup_warning.as_deref().unwrap();
            assert!(reason.contains(&format!("leftover source tip {leftover}")));
            assert_completed_with_source_kept(&root, &intent, &ws);
        }
    }

    #[tokio::test]
    async fn pipeline_failure_after_prepare_keeps_the_source_and_failed_step_for_retry() {
        let (_dir, root, mut intent, ws, mut host) = pipeline_fixture();
        let mut params = choices();
        params.carry_unpushed = true;
        host.fail_after_prepare = true;
        let error = run_pipeline_with(&root, &mut intent, &params, &ws, &mut None, &mut host)
            .await
            .unwrap_err();
        record_failure(&root, &mut intent, &error).unwrap();
        assert!(host.target.join(".git").exists());
        assert!(Path::new(&ws.worktree_path).join("private.txt").exists());
        assert_eq!(host.cleanups, 0);
        let failed = read(&root, &intent.team_key, &intent.workspace_id).unwrap();
        assert_eq!(failed.step, MoveStep::PreparingTarget);
        assert_eq!(failed.failure.as_deref(), Some("injected after prepare"));
    }

    #[tokio::test]
    async fn reaper_seam_resumes_the_durable_half_done_move_through_real_cleanup() {
        let (_dir, root, mut intent, ws, mut host) = pipeline_fixture();
        let mut params = choices();
        params.carry_unpushed = true;
        let prepared = host
            .prepare(&root, &mut intent, &params, &ws, "refs/heads/story")
            .await
            .unwrap();
        record_prepared(&root, &mut intent, &prepared).unwrap();
        intent.step = MoveStep::SwappingLocation;
        rewrite(&root, &intent).unwrap();
        let mut recovered = read(&root, &intent.team_key, &intent.workspace_id).unwrap();
        resume_intent_with(&root, &mut recovered, &mut host)
            .await
            .unwrap();
        assert_eq!(host.steps, vec![MoveStep::CleaningSource]);
        assert_eq!(host.cleanups, 1);
        assert!(!Path::new(&ws.worktree_path).exists());
        assert!(host.target.join("private.txt").exists());
        assert!(scan(&root).is_empty());
    }

    fn current_source_reply() -> Vec<crate::commands::machines::RuntimeAdmissionRow> {
        use crate::commands::machines::{
            RuntimeAdmissionRow, RuntimeAdmissionState, RuntimeLayoutReply, RuntimeMemberSlot,
        };
        [RuntimeMemberSlot::InProcess, RuntimeMemberSlot::Sidecar]
            .into_iter()
            .map(|slot| {
                let RuntimeLayoutReply::Live { mut target, .. } =
                    super::super::terminal_tabs::tests::reply("local", &[])
                else {
                    panic!()
                };
                target.member.slot = slot;
                RuntimeAdmissionRow {
                    private_authority_epoch: None,
                    member: target.member,
                    revision: target.admission_revision,
                    state: RuntimeAdmissionState::Live {
                        namespace: target.namespace,
                    },
                }
            })
            .collect()
    }

    #[tokio::test]
    async fn generic_path_usage_feeds_report_and_self_close_refusal() {
        use codesense_pty_core::domain::{PtySessionId, WorkspaceKey};
        use codesense_pty_core::layout::{EntryState, Placement, TabEntry, WorktreeUsage};
        let usage = WorktreeUsage {
            entries: vec![(
                WorkspaceKey::new("generic:/source/nested"),
                TabEntry {
                    session: PtySessionId::new("builder"),
                    name: "Builder".into(),
                    role: None,
                    placement: Placement::Tab,
                    state: EntryState::Live,
                    tags: [("handle".into(), "builder-handle".into())].into(),
                },
            )],
            opening: vec![],
        };
        let tabs = discover_source_usage(
            &MachineRef(None),
            Some("/source"),
            current_source_reply(),
            move |target, _| {
                let usage = if target.member.slot
                    == crate::commands::machines::RuntimeMemberSlot::Sidecar
                {
                    usage.clone()
                } else {
                    Default::default()
                };
                async { Ok(usage) }
            },
            current_source_reply,
        )
        .await
        .unwrap();
        let status = || codesense_pty_core::remote_workspace::RemoteWorktreeDirtyStatus {
            source_tip: "a".repeat(40),
            ..Default::default()
        };
        let mut params = choices();
        let refusal = assess_source(&params, status(), &tabs, None, false).unwrap_err();
        assert!(refusal.contains("Builder") && refusal.contains("builder-handle"));
        assert!(no_source_agents(&tabs)
            .unwrap_err()
            .contains("stop the agents"));
        params.stop_running_agents = true;
        assert!(
            assess_source(&params, status(), &tabs, Some("builder"), false)
                .unwrap_err()
                .contains("selfClose")
        );
    }

    #[tokio::test]
    async fn source_confirmation_refuses_old_unreachable_and_stopped_servers() {
        use codesense_pty_core::layout::{source_refusal, WorktreeUsage, OLD_SERVER};
        for reason in [
            source_refusal(OLD_SERVER),
            "timed out".into(),
            "stopped".into(),
        ] {
            let error = discover_source_usage(
                &MachineRef(None),
                Some("/source"),
                current_source_reply(),
                move |_, _| {
                    let reason = reason.clone();
                    async { Err::<WorktreeUsage, _>(reason) }
                },
                current_source_reply,
            )
            .await
            .unwrap_err();
            assert!(is_source_refusal(&error));
            assert!(error.contains("restart the terminal server") || error.contains("restart it"));
        }
        use crate::commands::machines::{RuntimeAdmissionState, RuntimeCounter};
        let mut partial = current_source_reply();
        partial[1].state = RuntimeAdmissionState::Unavailable;
        assert!(discover_source_usage(
            &MachineRef(None),
            Some("/source"),
            partial,
            |_, _| async { Ok(WorktreeUsage::default()) },
            current_source_reply
        )
        .await
        .is_err());
        let mut missing = current_source_reply();
        missing.pop();
        assert!(discover_source_usage(
            &MachineRef(None),
            Some("/source"),
            missing,
            |_, _| async { Ok(WorktreeUsage::default()) },
            current_source_reply
        )
        .await
        .is_err());
        let mut changed = current_source_reply();
        changed[1].revision = RuntimeCounter::new(2);
        assert!(discover_source_usage(
            &MachineRef(None),
            Some("/source"),
            current_source_reply(),
            |_, _| async { Ok(WorktreeUsage::default()) },
            || changed
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn live_current_server_confirms_an_empty_source_and_an_opening_refuses() {
        use codesense_pty_core::layout::WorktreeUsage;
        let tabs = discover_source_usage(
            &MachineRef(None),
            Some("/source"),
            current_source_reply(),
            |_, _| async { Ok(WorktreeUsage::default()) },
            current_source_reply,
        )
        .await
        .unwrap();
        no_source_agents(&tabs).unwrap();
        let error = discover_source_usage(
            &MachineRef(None),
            Some("/source"),
            current_source_reply(),
            |_, _| async {
                Ok(WorktreeUsage {
                    entries: vec![],
                    opening: vec![codesense_pty_core::domain::PtySessionId::new("opening")],
                })
            },
            current_source_reply,
        )
        .await
        .unwrap_err();
        assert!(error.contains("stop the agents"));
    }

    #[tokio::test]
    async fn stopped_source_resolves_cleanup_refusal_and_releases_the_active_slot() {
        for reaper in [false, true] {
            let (_dir, root, mut intent, ws, mut host) = pipeline_fixture();
            let mut params = choices();
            params.carry_unpushed = true;
            host.source_failure = Some(codesense_pty_core::layout::source_refusal(
                "terminal server stopped; start or restart the terminal server",
            ));
            let error = if reaper {
                let prepared = host
                    .prepare(&root, &mut intent, &params, &ws, "refs/heads/story")
                    .await
                    .unwrap();
                record_prepared(&root, &mut intent, &prepared).unwrap();
                // Source refusal resolves even when target verification would also fail.
                git(
                    &host.target,
                    &["commit", "--allow-empty", "-m", "target changed"],
                );
                intent.step = MoveStep::CleaningSource;
                rewrite(&root, &intent).unwrap();
                resume_intent_with(&root, &mut intent, &mut host)
                    .await
                    .unwrap();
                let result = completed_move_result(&intent, MoveSourceReport::default(), vec![]);
                assert!(result.moved && result.source_kept && !result.cleanup_pending);
                result.reason.unwrap()
            } else {
                run_pipeline_with(&root, &mut intent, &params, &ws, &mut None, &mut host)
                    .await
                    .unwrap_err()
            };
            assert!(error.contains("refused, source intact"));
            assert!(Path::new(&ws.worktree_path).exists());
            assert!(scan(&root).is_empty());
            let evidence: Vec<_> = std::fs::read_dir(&root)
                .unwrap()
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|e| e == "failed"))
                .collect();
            assert_eq!(evidence.len(), 1);
            let failed: MoveIntent =
                serde_json::from_slice(&std::fs::read(evidence[0].path()).unwrap()).unwrap();
            assert_eq!(failed.failure.as_deref(), Some(error.as_str()));
            let mut retry = sample_intent();
            retry.to.request_id = "retry".into();
            write_new(&root, &retry).unwrap();
        }
    }

    fn sample_intent() -> MoveIntent {
        MoveIntent::new(
            "alderco:default".into(),
            "DEMO-X".into(),
            MoveSource {
                machine_id: MachineRef(None),
                request_id: "src-rid".into(),
                handle: None,
                cwd: Some("/tmp/src".into()),
                repo_path: "/tmp/repo".into(),
                repository_key: "github.com/owner/repo".into(),
            },
            MoveTarget {
                machine_id: MachineRef(Some("aws-dev".into())),
                request_id: "dst-rid".into(),
            },
            false,
        )
    }

    #[test]
    fn file_key_is_stable_per_team_workspace_pair() {
        let a = MoveIntent::file_key("org:team", "DEMO-X");
        let b = MoveIntent::file_key("org:team", "DEMO-X");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64, "sha256 hex");
    }

    #[test]
    fn file_key_differs_across_workspaces_in_the_same_team() {
        let a = MoveIntent::file_key("org:team", "DEMO-X");
        let b = MoveIntent::file_key("org:team", "SD-Y");
        assert_ne!(a, b);
    }

    #[test]
    fn write_new_then_read_round_trips() {
        let root = tempdir().unwrap();
        let intent = sample_intent();
        write_new(root.path(), &intent).unwrap();
        let back = read(root.path(), &intent.team_key, &intent.workspace_id).unwrap();
        assert_eq!(back, intent);
    }

    #[test]
    fn write_new_rejects_second_write_in_flight() {
        let root = tempdir().unwrap();
        let intent = sample_intent();
        write_new(root.path(), &intent).unwrap();
        let err = write_new(root.path(), &intent).unwrap_err();
        assert_eq!(err, IntentWriteError::AlreadyExists);
    }

    #[test]
    fn rewrite_advances_step_without_losing_other_fields() {
        let root = tempdir().unwrap();
        let mut intent = sample_intent();
        write_new(root.path(), &intent).unwrap();
        intent.step = MoveStep::SwappingLocation;
        intent.last_progress_at_ms += 100;
        rewrite(root.path(), &intent).unwrap();
        let back = read(root.path(), &intent.team_key, &intent.workspace_id).unwrap();
        assert_eq!(back.step, MoveStep::SwappingLocation);
        assert_eq!(back.from, intent.from);
        assert_eq!(back.to, intent.to);
    }

    #[test]
    fn delete_is_idempotent() {
        let root = tempdir().unwrap();
        let intent = sample_intent();
        write_new(root.path(), &intent).unwrap();
        delete(root.path(), &intent.team_key, &intent.workspace_id).unwrap();
        // Second delete is Ok, not an error — "completed → gone" idiom.
        delete(root.path(), &intent.team_key, &intent.workspace_id).unwrap();
    }

    #[test]
    fn read_missing_is_not_found() {
        let root = tempdir().unwrap();
        let err = read(root.path(), "no-team", "no-ws").unwrap_err();
        assert_eq!(err, IntentReadError::NotFound);
    }

    #[test]
    fn scan_skips_corrupt_files_and_returns_the_rest() {
        let root = tempdir().unwrap();
        let good = sample_intent();
        write_new(root.path(), &good).unwrap();
        // Drop a corrupt file beside the good one — scan must log + skip,
        // never abort.
        std::fs::write(root.path().join("deadbeef.json"), b"{this is not json").unwrap();
        // And an unrelated non-JSON file — ignored by extension.
        std::fs::write(root.path().join("README"), b"hello").unwrap();
        let intents = scan(root.path());
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0], good);
    }

    #[test]
    fn scan_missing_root_returns_empty() {
        let root = tempdir().unwrap();
        let intents = scan(&root.path().join("does-not-exist"));
        assert!(intents.is_empty());
    }

    #[test]
    fn now_epoch_ms_is_wall_clock_not_instant() {
        // Belt-and-suspenders: the field doc says NEVER Instant::now(); this
        // test proves the helper returns something broadly consistent with
        // wall clock (greater than Jan 1 2020 in ms).
        let now = now_epoch_ms();
        assert!(now > 1_577_836_800_000, "epoch ms, not Instant since boot");
    }

    #[test]
    fn machine_ref_is_local_covers_both_none_and_literal_local() {
        assert!(MachineRef(None).is_local());
        assert!(MachineRef(Some("local".into())).is_local());
        assert!(!MachineRef(Some("aws-dev".into())).is_local());
    }

    #[test]
    fn machine_ref_serializes_as_null_for_local() {
        // Keeps the on-disk shape compatible with the architecture doc's JSON
        // example (`"machineId": null` for local).
        let r = MachineRef(None);
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, "null");
    }

    // ────────────────────────────────────────────────────────────────
    // Handler rollback contract — unit tests on the pure helper.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn rollback_decision_rolls_back_on_preparing_target() {
        // Pre-step-2 error → delete intent, keep ws.location = SOURCE,
        // propagate the error up. Target orphan accepted.
        assert_eq!(
            rollback_decision(MoveStep::PreparingTarget),
            RollbackAction::Rollback
        );
    }

    #[test]
    fn rollback_decision_rolls_forward_on_swapping_location() {
        // A step-2 error (rewrite failed) is still past the point
        // of no return — the swap intent is being written NOW; next boot
        // reaper will finish or retry. Caller gets cleanup_pending: true.
        assert_eq!(
            rollback_decision(MoveStep::SwappingLocation),
            RollbackAction::RollForwardWithCleanupPending
        );
    }

    #[test]
    fn rollback_decision_rolls_forward_on_cleaning_source() {
        // The common case: step 3's cleanup failed. The swap landed
        // (ws.location = TARGET on the FE); the boot reaper owns a retry.
        // Handler returns Ok({cleanup_pending: true}), does NOT delete the
        // intent.
        assert_eq!(
            rollback_decision(MoveStep::CleaningSource),
            RollbackAction::RollForwardWithCleanupPending
        );
    }

    // ────────────────────────────────────────────────────────────────
    // Safety gate — ownership marker name derivation + usage.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn local_workspace_handle_matches_remote_shape() {
        // The synthetic handle has the shape
        // `local-workspace-{64-hex}`. Zero downstream callers parse this;
        // this test documents the shape so a future renamer notices.
        let h = local_workspace_handle("/repos/owner/repo", "/wt/branch");
        assert!(h.starts_with("local-workspace-"), "handle shape");
        let hex = &h[16..];
        assert_eq!(hex.len(), 64, "64-char sha256 hex");
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn local_workspace_handle_is_deterministic_per_pair() {
        let a = local_workspace_handle("/r", "/wt");
        let b = local_workspace_handle("/r", "/wt");
        assert_eq!(a, b);
        let c = local_workspace_handle("/r", "/other-wt");
        assert_ne!(a, c);
    }

    // ────────────────────────────────────────────────────────────────
    // Porcelain parser handles `R  old -> new` rename lines.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn porcelain_path_extracts_destination_for_rename_lines() {
        assert_eq!(
            porcelain_path("R  old/path.rs -> new/path.rs"),
            Some("new/path.rs".to_string())
        );
    }

    #[test]
    fn porcelain_path_extracts_path_for_simple_lines() {
        assert_eq!(
            porcelain_path(" M src/lib.rs"),
            Some("src/lib.rs".to_string())
        );
        assert_eq!(
            porcelain_path("?? untracked.md"),
            Some("untracked.md".to_string())
        );
    }

    #[test]
    fn porcelain_path_skips_too_short_lines() {
        assert_eq!(porcelain_path(""), None);
        assert_eq!(porcelain_path(" M "), None); // 3 chars — no path
    }

    // ────────────────────────────────────────────────────────────────
    // `same_machine` pre-flight refusal name.
    // The handler constructs this error via `PreflightRefusal::SameMachine`;
    // the display string is the contract the UI shows the user.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn preflight_same_machine_renders_a_clear_refusal() {
        let msg = PreflightRefusal::SameMachine.to_string();
        assert!(
            msg.contains("already lives on the target"),
            "same_machine message: {msg}"
        );
    }

    // ────────────────────────────────────────────────────────────────
    // `in_progress` refusal — the handler maps
    // `IntentWriteError::AlreadyExists` to this surface. The persistence
    // layer's `write_new_rejects_second_write_in_flight` test above proves
    // the detection; this test proves the user-visible refusal copy.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn preflight_in_progress_renders_a_clear_refusal() {
        let msg = PreflightRefusal::InProgress.to_string();
        assert!(msg.contains("in flight"), "in_progress message: {msg}");
    }

    #[test]
    fn preflight_dirty_worktree_lists_paths_and_hints_the_flag() {
        let msg = PreflightRefusal::DirtyWorktree {
            paths: vec!["src/a.rs".into(), "src/b.rs".into()],
        }
        .to_string();
        assert!(msg.contains("src/a.rs"), "lists dirty paths: {msg}");
        assert!(msg.contains("discardUncommitted"), "hints the flag: {msg}");
    }

    #[test]
    fn preflight_dirty_worktree_truncates_long_lists_with_and_more() {
        let paths: Vec<String> = (0..8).map(|i| format!("file-{i}.rs")).collect();
        let msg = PreflightRefusal::DirtyWorktree { paths }.to_string();
        assert!(msg.contains("file-0.rs"));
        assert!(msg.contains("file-4.rs")); // 5th path (index 4) still shown
        assert!(
            msg.contains("and 3 more"),
            "truncates past the 5-path cap: {msg}"
        );
    }

    #[test]
    fn preflight_missing_target_path_names_the_local_requirement() {
        let msg = PreflightRefusal::MissingTargetPath.to_string();
        assert!(
            msg.contains("target_worktree_path") && msg.contains("local"),
            "missing_target_path message: {msg}"
        );
    }

    // ────────────────────────────────────────────────────────────────
    // Step-1 failure rollback — the persistence
    // invariants a failed step 1 relies on. The step-1 action itself
    // (prepare_remote_workspace / fetch_and_create_worktree) requires a
    // live Tauri runtime + git repo, so the full end-to-end sim is
    // deferred; the sim below asserts the state-machine invariant:
    // after `write_new(step=PreparingTarget)` + `delete`, the intent
    // file is gone and a second move can start fresh.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn step1_failure_rollback_cycle_clears_the_intent_file() {
        let root = tempdir().unwrap();
        let intent = sample_intent();
        assert_eq!(intent.step, MoveStep::PreparingTarget);
        write_new(root.path(), &intent).unwrap();
        // Rollback: handler deletes the intent + emits cleared.
        delete(root.path(), &intent.team_key, &intent.workspace_id).unwrap();
        // Second move can start fresh — no `in_progress` refusal.
        write_new(root.path(), &intent).unwrap();
    }

    // ────────────────────────────────────────────────────────────────
    // Step-2 → step-3 transition — `rewrite` to
    // `CleaningSource` lands on disk BEFORE the pipeline advances. The
    // handler's rewrite-then-emit ordering is tested by proving the
    // persistence invariant: after step-2 completes, re-reading the
    // file sees `CleaningSource`.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn step2_to_step3_transition_lands_on_disk_before_further_action() {
        let root = tempdir().unwrap();
        let mut intent = sample_intent();
        write_new(root.path(), &intent).unwrap();
        // Simulate the step 2 → 3 transition the handler performs.
        intent.step = MoveStep::CleaningSource;
        intent.last_progress_at_ms += 1;
        rewrite(root.path(), &intent).unwrap();
        // Reaper (on another process) would read this and resume.
        let on_disk = read(root.path(), &intent.team_key, &intent.workspace_id).unwrap();
        assert_eq!(on_disk.step, MoveStep::CleaningSource);
        assert_eq!(on_disk.last_progress_at_ms, intent.last_progress_at_ms);
    }

    // ────────────────────────────────────────────────────────────────
    // Reaper advance of each step — the step
    // dispatch logic (not the full `resume_intent`, which needs an app
    // handle, but the state-machine shape). The reaper reads the intent,
    // then advances based on `intent.step`.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn reaper_scan_includes_each_step_variant() {
        let root = tempdir().unwrap();
        for (ws_id, step) in [
            ("WS-A", MoveStep::PreparingTarget),
            ("WS-B", MoveStep::SwappingLocation),
            ("WS-C", MoveStep::CleaningSource),
        ] {
            let mut intent = sample_intent();
            intent.workspace_id = ws_id.into();
            intent.step = step;
            write_new(root.path(), &intent).unwrap();
        }
        let scanned = scan(root.path());
        assert_eq!(scanned.len(), 3);
        let steps: std::collections::HashSet<_> = scanned.iter().map(|i| i.step).collect();
        assert_eq!(steps.len(), 3);
        assert!(steps.contains(&MoveStep::PreparingTarget));
        assert!(steps.contains(&MoveStep::SwappingLocation));
        assert!(steps.contains(&MoveStep::CleaningSource));
    }

    // ────────────────────────────────────────────────────────────────
    // Post-step-2 error — intent remains at
    // `CleaningSource` for the reaper. The handler's err branch MUST NOT
    // delete the intent when `step != PreparingTarget`.
    // ────────────────────────────────────────────────────────────────

    #[test]
    fn post_step2_error_intent_survives_for_reaper() {
        let root = tempdir().unwrap();
        let mut intent = sample_intent();
        write_new(root.path(), &intent).unwrap();
        // Pipeline advanced to CleaningSource before failing step 3.
        intent.step = MoveStep::CleaningSource;
        rewrite(root.path(), &intent).unwrap();
        // The rollback decision says RollForward: handler does NOT delete.
        assert_eq!(
            rollback_decision(intent.step),
            RollbackAction::RollForwardWithCleanupPending
        );
        // Next boot reaper picks it up and retries.
        let found = scan(root.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].step, MoveStep::CleaningSource);
    }

    // ────────────────────────────────────────────────────────────────
    // The resolver. Viewer rows are the FE's `Workspace` shape;
    // the worktree index is the team's store on this Mac.
    // ────────────────────────────────────────────────────────────────

    fn viewer_with(rows: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "workspaces": { "acme:alpha": rows } })
    }

    #[test]
    fn a_local_viewer_row_resolves_from_its_own_cwd_and_source_slug() {
        // A This Mac workspace has no `location`; its cwd is the row's own.
        let viewer = viewer_with(serde_json::json!([{
            "id": "story:SW-1", "kind": "story", "storyId": "SW-1", "status": "ready",
            "cwd": "/wt/sw-1", "sourceRepoSlug": "acme/codesense",
            "sourceRepoPath": "/clone/codesense"
        }]));
        let ws = read_workspace_snapshot(&viewer, "acme:alpha", "SW-1")
            .expect("a local row must resolve");
        assert!(ws.machine_id.is_local());
        assert_eq!(ws.cwd.as_deref(), Some("/wt/sw-1"));
        assert_eq!(ws.worktree_path, "/wt/sw-1");
        assert_eq!(ws.repo_path, "/clone/codesense");
        assert_eq!(ws.source_repo.owner, "acme");
        assert_eq!(ws.source_repo.repo, "codesense");
        assert!(ws.handle.is_none());
    }

    #[test]
    fn a_row_matches_by_path_id_when_no_story_id_field_is_present() {
        let viewer = viewer_with(serde_json::json!([{
            "id": "SW-2", "kind": "story", "cwd": "/wt/sw-2", "sourceRepoSlug": "acme/codesense"
        }]));
        assert!(read_workspace_snapshot(&viewer, "acme:alpha", "SW-2").is_some());
    }

    #[test]
    fn a_remote_row_takes_its_directory_and_request_from_the_location() {
        let viewer = viewer_with(serde_json::json!([{
            "id": "SW-3", "kind": "story", "storyId": "SW-3", "cwd": "",
            "sourceRepoSlug": "acme/codesense",
            "location": {
                "kind": "machine", "machineId": "box", "requestId": "req-3",
                "handle": "SW-3", "cwd": "/home/u/wt/sw-3",
                "branch": "feat/sw-3", "baseRef": "main"
            }
        }]));
        let ws = read_workspace_snapshot(&viewer, "acme:alpha", "SW-3").expect("resolves");
        assert_eq!(ws.machine_id.0.as_deref(), Some("box"));
        assert_eq!(ws.request_id, "req-3");
        assert_eq!(ws.handle.as_deref(), Some("SW-3"));
        assert_eq!(ws.cwd.as_deref(), Some("/home/u/wt/sw-3"));
        assert_eq!(ws.branch, "feat/sw-3");
        assert_eq!(ws.base_ref, "main");
    }

    #[test]
    fn a_story_the_viewer_does_not_hold_is_absent_from_it() {
        let viewer = viewer_with(serde_json::json!([]));
        assert!(read_workspace_snapshot(&viewer, "acme:alpha", "SW-9").is_none());
    }

    #[test]
    fn the_worktree_index_finds_a_team_story_with_no_viewer_row() {
        use crate::install::worktree_index::{self, WorktreeStoryRecord};
        let home = tempdir().unwrap();
        let wt = home.path().join("wt-sw-4");
        std::fs::create_dir(&wt).unwrap();
        let principal = crate::domain::Principal::from(42u64);
        let story = home
            .path()
            .join("users/42/orgs/acme/codesense-context/alpha/content/impl/SW-4.md");
        let record = WorktreeStoryRecord::new(
            wt.clone(),
            "acme",
            SourceRepoSlug::new("acme", "codesense").unwrap(),
            "SW-4",
            story,
        );
        worktree_index::write_record(home.path(), &principal, &record).unwrap();

        let found = snapshot_from_worktree_index(home.path(), &principal, "acme:alpha", "SW-4")
            .unwrap()
            .expect("the team's managed worktree is movable without a viewer row");
        assert!(found.machine_id.is_local());
        assert_eq!(found.worktree_path, wt.to_string_lossy());
        assert_eq!(found.source_repo.repo, "codesense");

        // Another team's story with the same id is not this team's workspace.
        assert!(
            snapshot_from_worktree_index(home.path(), &principal, "acme:beta", "SW-4")
                .unwrap()
                .is_none()
        );
        // A record whose worktree directory is gone is not movable.
        std::fs::remove_dir(&wt).unwrap();
        assert!(
            snapshot_from_worktree_index(home.path(), &principal, "acme:alpha", "SW-4")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn no_such_workspace_refusal_names_the_step_that_adds_it() {
        let msg = PreflightRefusal::NoSuchWorkspace {
            story_id: "SW-9".into(),
        }
        .to_string();
        assert!(msg.starts_with("no such workspace in the active team"));
        assert!(msg.contains("open_workspace"));
        assert!(msg.contains("\"SW-9\""));
    }
}
