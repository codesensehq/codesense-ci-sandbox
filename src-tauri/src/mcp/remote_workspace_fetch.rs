// SPDX-License-Identifier: MIT
//! Bring a machine workspace's committed branch home into the local clone.
//!
//! The machine packs the commits its checkout is on (the host export ops);
//! only that bundle crosses the admitted connection. Here it is fetched into
//! the team's local clone as `refs/remotes/codesense-machine-<machine>/<branch>`,
//! and the local branch of the same name moves only when that is a
//! fast-forward nobody's checkout would lose work to. Nothing is ever merged,
//! rebased or forced.
//!
//! The machine is trusted less than this Mac. The branch is the one this Mac
//! prepared the workspace on (the machine's report is only checked against
//! it), the bundle's objects are checked as a receive would before any ref
//! moves, and the refs the bundle names are never read. A pack's size is
//! capped (`--max-input-size`); how far its deltas expand is not.
use super::{remote_authority::RemoteAuthority, remote_workspace_bundle as bundle};
use crate::terminal::session_server::{ExportCallError, SessionServerClient};
use codesense_pty_core::{
    remote_export::{
        ExportDescriptor, ExportIntent, ExportOperation, ExportReply, ExportRequest, WorkspaceHead,
    },
    remote_files::FileTarget,
    remote_workspace::{BUNDLE_CHUNK_BYTES, MAX_BUNDLE_BYTES},
};
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// At most this many arrived commits are listed; the rest are counted.
const MAX_LISTED_COMMITS: usize = 200;
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteWorkspaceFetchParams {
    /// The machine the workspace lives on. Optional when `storyId` or
    /// `workspaceId` names an open workspace that lives on a machine.
    #[serde(default)]
    pub machine_id: Option<String>,
    /// The managed workspace handle (`remote-workspace-…`) the workspace
    /// record keeps. Optional when `storyId` or `workspaceId` is given.
    #[serde(default)]
    pub workspace_handle: Option<String>,
    /// An open story workspace's story id, resolved to its machine and handle.
    #[serde(default)]
    pub story_id: Option<String>,
    /// An open workspace's id, resolved to its machine and handle.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// The team whose open workspaces `storyId`/`workspaceId` are looked up
    /// in, when more than one team has a match.
    #[serde(default)]
    pub team_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrivedCommit {
    pub sha: String,
    pub subject: String,
}

/// Where the local branch stands after the fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LocalBranch {
    /// Moved forward to the machine's commit.
    FastForwarded,
    /// Did not exist locally; created at the machine's commit.
    Created,
    /// Already at the machine's commit.
    UpToDate,
    /// Already contains the machine's commit and more.
    Ahead,
    /// Left where it was; `reason` says why.
    NotFastForwarded,
}

/// Why the local branch was left where it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum NotFastForwarded {
    /// The local branch has commits the machine's branch does not.
    Diverged,
    /// The branch is checked out in the local clone with uncommitted changes.
    DirtyCheckout,
    /// The branch is checked out in another worktree of the local clone, or
    /// a rebase or bisect of it is in progress there.
    CheckedOutElsewhere,
    /// The local clone's checkout moved to another branch during the fetch.
    CheckoutChanged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchReport {
    pub machine_id: String,
    pub workspace_handle: String,
    /// The local clone the commits were fetched into.
    pub source_repo_path: String,
    /// The branch the workspace was prepared on (and the machine is on).
    pub branch: String,
    /// The commit the machine's checkout is on.
    pub commit: String,
    /// The fetched ref in the local clone, now at `commit`.
    pub remote_ref: String,
    /// Where `remoteRef` was before, if it existed.
    pub previous_remote_ref: Option<String>,
    /// Commits on the machine's branch that neither the previous fetched ref,
    /// the local branch nor the workspace's starting commit had, newest first.
    pub commits: Vec<ArrivedCommit>,
    /// How many arrived commits there were in all (`commits` is capped).
    pub commit_count: usize,
    /// The local branch of the same name.
    pub local_branch: String,
    pub local_branch_state: LocalBranch,
    pub fast_forwarded: bool,
    pub reason: Option<NotFastForwarded>,
    /// A sentence for a person, when the local branch did not move.
    pub message: Option<String>,
}

/// The host side of a fetch: what the machine's terminal server answers.
pub(crate) trait ExportHost: Send + Sync {
    fn head(&self, target: &FileTarget) -> Result<WorkspaceHead, String>;
    fn export(&self, request: ExportRequest) -> Result<ExportReply, ExportCallError>;
}

impl ExportHost for SessionServerClient {
    fn head(&self, target: &FileTarget) -> Result<WorkspaceHead, String> {
        self.workspace_head(target.clone())
            .map_err(|error| error.to_string())
    }
    fn export(&self, request: ExportRequest) -> Result<ExportReply, ExportCallError> {
        self.workspace_export(request)
    }
}

/// One fetch: which workspace, which machine, into which clone.
pub(crate) struct FetchSource<'a> {
    pub host: &'a dyn ExportHost,
    pub target: FileTarget,
    pub machine_id: String,
    /// The commit the workspace was prepared from; usually already local, so
    /// the machine sends only what came after it.
    pub prepared_commit: String,
    /// The base commit the machine reports the workspace on, when
    /// `update_workspace_base` moved it past `prepared_commit`. Its history is
    /// already local (it came from this clone), so it is never fetched again.
    pub base_commit: Option<String>,
    /// The branch the workspace was prepared on. The machine's word for its
    /// branch is only ever checked against this, never used to name a ref.
    pub prepared_branch: String,
    pub clone: PathBuf,
    pub valid: &'a (dyn Fn() -> Result<(), String> + Send + Sync),
}

/// The MCP tool: locate the workspace, check it is the caller's team's, then
/// fetch into that team's local clone.
pub(crate) async fn fetch(
    app: tauri::AppHandle,
    params: RemoteWorkspaceFetchParams,
    caller: Option<RemoteAuthority>,
) -> Result<FetchReport, String> {
    let (machine_id, handle) = locate(&app, &params)?;
    let ambient = match &caller {
        Some(caller) => caller
            .context()
            .await
            .map_err(|error| error.message.to_string())?,
        None => {
            let (codesense_home, user_id) =
                crate::commands::route_to_context::resolve_ambient(&app).await?;
            crate::commands::route_to_context::ActiveContext {
                codesense_home,
                user_id,
                org_slug: String::new(),
                team_name: None,
            }
        }
    };
    let resolved = super::remote_workspace_target::resolve_selected(
        app,
        ambient,
        Some(&machine_id),
        &handle,
        caller,
        false,
    )
    .await?;
    let clone = bundle::local_seed(&resolved.context, &resolved.source_repo)?.ok_or_else(|| {
        format!(
            "No local clone of {}/{} is attached on this Mac; attach it, then fetch again",
            resolved.source_repo.owner, resolved.source_repo.repo
        )
    })?;
    let client = resolved.client.clone();
    let target = resolved.target.clone();
    let machine = resolved.machine_id.clone();
    let prepared = resolved.prepared_commit.clone();
    let base_commit = resolved.base_commit.clone();
    let prepared_branch = resolved.prepared_branch.clone();
    let valid = resolved.validate.clone();
    crate::commands::await_blocking("remote_workspace_fetch", move || {
        fetch_into_clone(FetchSource {
            host: client.as_ref(),
            target,
            machine_id: machine,
            prepared_commit: prepared,
            base_commit,
            prepared_branch,
            clone,
            valid: valid.as_ref(),
        })
    })
    .await
}

/// The machine and handle the request names, either directly or through an
/// open workspace record (`location` on a workspace that lives on a machine).
pub(crate) fn locate(
    app: &tauri::AppHandle,
    params: &RemoteWorkspaceFetchParams,
) -> Result<(String, String), String> {
    use tauri::Manager;
    if let (Some(machine), Some(handle)) = (&params.machine_id, &params.workspace_handle) {
        return Ok((machine.clone(), handle.clone()));
    }
    let viewer = app.state::<super::state::McpViewerState>().get();
    locate_in_viewer(&viewer, params)
}

fn locate_in_viewer(
    viewer: &serde_json::Value,
    params: &RemoteWorkspaceFetchParams,
) -> Result<(String, String), String> {
    if params.story_id.is_none() && params.workspace_id.is_none() {
        return Err(
            "name the workspace: machineId and workspaceHandle, or a storyId or workspaceId".into(),
        );
    }
    let mut found: Vec<(String, String)> = Vec::new();
    let teams = viewer
        .get("workspaces")
        .and_then(|value| value.as_object())
        .into_iter()
        .flatten()
        .filter(|(team, _)| {
            params
                .team_key
                .as_ref()
                .is_none_or(|wanted| wanted == *team)
        });
    for (_, rows) in teams {
        for row in rows.as_array().into_iter().flatten() {
            let text = |key: &str| row.get(key).and_then(|value| value.as_str());
            if params
                .workspace_id
                .as_deref()
                .is_some_and(|id| text("id") != Some(id))
                || params
                    .story_id
                    .as_deref()
                    .is_some_and(|id| text("storyId") != Some(id) && text("id") != Some(id))
            {
                continue;
            }
            let Some(location) = row.get("location") else {
                continue;
            };
            let field = |key: &str| location.get(key).and_then(|value| value.as_str());
            if field("kind") != Some("machine") {
                continue;
            }
            let (Some(machine), Some(handle)) = (field("machineId"), field("handle")) else {
                return Err("that workspace is still being created on its machine".into());
            };
            if params
                .machine_id
                .as_deref()
                .is_some_and(|wanted| wanted != machine)
                || params
                    .workspace_handle
                    .as_deref()
                    .is_some_and(|wanted| wanted != handle)
            {
                continue;
            }
            let pair = (machine.to_string(), handle.to_string());
            if !found.contains(&pair) {
                found.push(pair);
            }
        }
    }
    match found.len() {
        0 => Err("no open workspace on a machine matches; it may live on this Mac".into()),
        1 => Ok(found.remove(0)),
        _ => Err("more than one open workspace on a machine matches; pass teamKey".into()),
    }
}

/// Fetched refs live under their own prefix, so no machine id can name (or
/// overwrite) a real remote such as `origin`.
const REMOTE_REF_PREFIX: &str = "refs/remotes/codesense-machine-";

/// The machine id as one ref path segment. Only ids that are already a safe
/// segment are accepted: no lossy mapping can make two machines share refs.
fn ref_segment(machine_id: &str) -> Result<&str, String> {
    if machine_id.is_empty()
        || machine_id.len() > 128
        || !machine_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err("the machine id cannot name a ref".into());
    }
    Ok(machine_id)
}

/// A branch name that is safe to put after `refs/heads/` and on a Git
/// command line.
fn valid_branch(branch: &str) -> bool {
    !branch.is_empty()
        && !branch.starts_with('-')
        && !branch.contains("..")
        && !branch.starts_with("refs/")
        && git2::Reference::is_valid_name(&format!("refs/heads/{branch}"))
}

fn valid_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn git(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "submodule.recurse=false",
        ])
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES");
    command
}

/// A short local Git query; `None` when Git answers with a failure.
fn query(dir: &Path, args: &[&str]) -> Result<Option<String>, String> {
    let output = git(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|_| "local Git executable unavailable")?;
    if !output.status.success() {
        return Ok(None);
    }
    String::from_utf8(output.stdout)
        .map(|text| Some(text.trim_end_matches('\n').to_string()))
        .map_err(|_| "local Git answered with non-text output".into())
}

/// What a ref holds: its own value (the compare-and-set old value) and the
/// commit it peels to (`None` when it names no commit).
#[derive(Clone, Debug, PartialEq, Eq)]
struct RefValue {
    raw: String,
    commit: Option<String>,
}

fn read_ref(clone: &Path, reference: &str) -> Result<Option<RefValue>, String> {
    let Some(raw) = query(clone, &["rev-parse", "--verify", "--quiet", reference])? else {
        return Ok(None);
    };
    let commit = query(
        clone,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{raw}^{{commit}}"),
        ],
    )?;
    Ok(Some(RefValue { raw, commit }))
}

fn has_commit(clone: &Path, commit: &str) -> Result<bool, String> {
    Ok(query(clone, &["cat-file", "-e", &format!("{commit}^{{commit}}")])?.is_some())
}

fn is_ancestor(clone: &Path, ancestor: &str, descendant: &str) -> Result<bool, String> {
    let status = git(clone)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|_| "local Git executable unavailable")?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err("local Git could not compare commits".into()),
    }
}

/// Moves `reference` from exactly `old` (absent: the zero id) to `new`.
fn compare_and_set(
    clone: &Path,
    reference: &str,
    new: &str,
    old: Option<&str>,
) -> Result<(), String> {
    let status = git(clone)
        .args([
            "update-ref",
            "-m",
            "codesense: remote_workspace_fetch",
            reference,
            new,
            old.unwrap_or(ZERO_OID),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|_| "local Git executable unavailable")?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{reference} changed while fetching; fetch again"))
    }
}

fn common_dir(clone: &Path) -> Result<PathBuf, String> {
    query(
        clone,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?
    .map(PathBuf::from)
    .ok_or_else(|| "the attached local clone is not a Git repository".into())
}

/// The worktree of the local clone that has `reference` checked out, if any.
fn checkout_of(clone: &Path, reference: &str) -> Result<Option<PathBuf>, String> {
    let list = query(clone, &["worktree", "list", "--porcelain"])?
        .ok_or("local clone worktrees unavailable")?;
    let mut path: Option<PathBuf> = None;
    for line in list.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(rest));
        } else if line.strip_prefix("branch ") == Some(reference) {
            return Ok(path);
        }
    }
    Ok(None)
}

/// The worktree in which a rebase or bisect of `branch` is in progress, the
/// way `git branch -f` counts it as checked out: the worktree's HEAD is
/// detached then, so the worktree list does not name the branch.
fn operation_on(clone: &Path, common: &Path, branch: &str) -> Result<Option<PathBuf>, String> {
    let full = format!("refs/heads/{branch}");
    let names = |text: &str| {
        let text = text.trim();
        text == full || text == branch
    };
    let mut gitdirs = vec![(common.to_path_buf(), clone.to_path_buf())];
    if let Ok(entries) = std::fs::read_dir(common.join("worktrees")) {
        for entry in entries.flatten() {
            let dir = entry.path();
            let checkout = std::fs::read_to_string(dir.join("gitdir"))
                .ok()
                .and_then(|text| Path::new(text.trim()).parent().map(Path::to_path_buf))
                .unwrap_or_else(|| dir.clone());
            gitdirs.push((dir, checkout));
        }
    }
    for (gitdir, checkout) in gitdirs {
        for file in [
            "rebase-merge/head-name",
            "rebase-apply/head-name",
            "BISECT_START",
        ] {
            if std::fs::read_to_string(gitdir.join(file)).is_ok_and(|text| names(&text)) {
                return Ok(Some(checkout));
            }
        }
    }
    Ok(None)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Commits reachable from `commit` but from none of `known`, newest first,
/// capped at [`MAX_LISTED_COMMITS`], with the full count.
fn arrived(
    clone: &Path,
    commit: &str,
    known: &[&str],
) -> Result<(Vec<ArrivedCommit>, usize), String> {
    let mut args = vec![
        "rev-list".to_string(),
        "--count".to_string(),
        commit.to_string(),
    ];
    args.extend(known.iter().map(|sha| format!("^{sha}")));
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let count = query(clone, &refs)?
        .and_then(|text| text.trim().parse::<usize>().ok())
        .ok_or("local Git could not count the fetched commits")?;
    let mut args = vec![
        "log".to_string(),
        "--no-show-signature".to_string(),
        format!("--max-count={MAX_LISTED_COMMITS}"),
        "--format=%H%x1f%s".to_string(),
        commit.to_string(),
    ];
    args.extend(known.iter().map(|sha| format!("^{sha}")));
    args.push("--".to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let text = query(clone, &refs)?.ok_or("local Git could not list the fetched commits")?;
    let commits = text
        .lines()
        .filter_map(|line| line.split_once('\u{1f}'))
        .map(|(sha, subject)| ArrivedCommit {
            sha: sha.to_string(),
            subject: subject.to_string(),
        })
        .collect();
    Ok((commits, count))
}

fn validate_descriptor(descriptor: &ExportDescriptor, intent: &ExportIntent) -> Result<(), String> {
    if descriptor.request_id != intent.request_id
        || descriptor.bundle.commit != intent.expected_head
        || descriptor.bundle.size == 0
        || descriptor.bundle.size > MAX_BUNDLE_BYTES
        || descriptor.bundle.digest.len() != 64
        || !descriptor
            .bundle
            .digest
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
        || descriptor.released
    {
        return Err("the machine answered with a different export than asked for".into());
    }
    Ok(())
}

/// Where the pack starts in a v2/v3 Git bundle: after the header's blank
/// line. Refs the header names are never read, so none can be written.
fn pack_offset(payload: &Path) -> Result<u64, String> {
    const MAX_HEADER: u64 = 1024 * 1024;
    let mut header = Vec::new();
    std::fs::File::open(payload)
        .map_err(|_| "the fetched bundle is unavailable")?
        .take(MAX_HEADER + 4)
        .read_to_end(&mut header)
        .map_err(|_| "the fetched bundle is unreadable")?;
    if !(header.starts_with(b"# v2 git bundle\n") || header.starts_with(b"# v3 git bundle\n")) {
        return Err("the fetched file is not a Git bundle".into());
    }
    let end = header
        .windows(2)
        .position(|pair| pair == b"\n\n")
        .ok_or("the fetched bundle's header is malformed")?
        + 2;
    if end as u64 > MAX_HEADER || !header[end..].starts_with(b"PACK") {
        return Err("the fetched bundle's header is malformed".into());
    }
    Ok(end as u64)
}

/// Download the prepared bundle and index its objects into the clone, every
/// object checked as a receive would (`--strict`). No ref is written here.
fn import(
    source: &FetchSource<'_>,
    intent: &ExportIntent,
    descriptor: &ExportDescriptor,
) -> Result<(), String> {
    let alive = || (source.valid)().is_ok();
    let common = common_dir(&source.clone)?;
    // The bundle is staged on the clone's own disk, and the objects land there.
    bundle::headroom(&common, descriptor.bundle.size.saturating_mul(2))?;
    let staging = tempfile::Builder::new()
        .prefix("codesense-fetch-")
        .tempdir_in(&common)
        .map_err(|_| "local staging for the fetched commits unavailable")?;
    let payload = staging.path().join("objects.bundle");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&payload)
        .map_err(|_| "local staging for the fetched commits unavailable")?;
    let mut offset = 0;
    while offset < descriptor.bundle.size {
        (source.valid)()?;
        let (reply_offset, bytes) = match source.host.export(ExportRequest {
            intent: intent.clone(),
            operation: ExportOperation::Read { offset },
        }) {
            Ok(ExportReply::Chunk { offset, bytes }) => (offset, bytes),
            Ok(_) => return Err("unexpected export chunk".into()),
            Err(error) => return Err(error.to_string()),
        };
        if reply_offset != offset
            || bytes.is_empty()
            || bytes.len() > BUNDLE_CHUNK_BYTES
            || offset + bytes.len() as u64 > descriptor.bundle.size
        {
            return Err("invalid export chunk bounds".into());
        }
        file.write_all(&bytes)
            .map_err(|_| "writing the fetched commits failed")?;
        offset += bytes.len() as u64;
    }
    file.sync_all()
        .map_err(|_| "the fetched commits are not durable")?;
    drop(file);
    if bundle::inspect(&payload)? != (descriptor.bundle.size, descriptor.bundle.digest.clone()) {
        return Err("the fetched bundle does not match what the machine prepared".into());
    }
    let deadline = Instant::now() + Duration::from_secs(30 * 60);
    let mut verify = git(&source.clone);
    verify.args(["bundle", "verify", "--quiet"]).arg(&payload);
    bundle::run(verify, &alive, deadline, None)
        .map_err(|_| "the local clone cannot read the fetched bundle")?;
    let mut pack =
        std::fs::File::open(&payload).map_err(|_| "the fetched bundle is unavailable")?;
    pack.seek(SeekFrom::Start(pack_offset(&payload)?))
        .map_err(|_| "the fetched bundle is unreadable")?;
    let mut index = git(&source.clone);
    index.args([
        "-c",
        "core.fsync=committed",
        "-c",
        "fetch.fsckObjects=true",
        "-c",
        "transfer.fsckObjects=true",
        "index-pack",
        "--stdin",
        "--fix-thin",
        "--strict",
        &format!("--max-input-size={}", descriptor.bundle.size),
    ]);
    bundle::run_with_input(index, pack, &alive, deadline).map_err(|_| {
        "the machine's commits failed Git's object checks; nothing was written".to_string()
    })?;
    if !has_commit(&source.clone, &intent.expected_head)? {
        return Err("the fetched bundle lacks the machine's commit".into());
    }
    Ok(())
}

/// Tell the machine it may drop the bundle: the commits are home, or the
/// attempt is over and a retry prepares a fresh one. Best effort.
fn release(source: &FetchSource<'_>, intent: &ExportIntent, descriptor: &ExportDescriptor) {
    let confirmed = matches!(
        source.host.export(ExportRequest {
            intent: intent.clone(),
            operation: ExportOperation::Release {
                bundle: descriptor.bundle.clone(),
            },
        }),
        Ok(ExportReply::Released { descriptor: released })
            if released.released && released.bundle == descriptor.bundle
    );
    if !confirmed {
        log::warn!(target: "mcp", "remote_workspace_fetch: export bundle not released");
    }
}

/// Ask the machine for a bundle of `head` (excluding what a prerequisite the
/// clone has already gives it) and bring its objects home. Every prepared
/// bundle is released, whatever happens after it was made.
fn transfer(
    source: &FetchSource<'_>,
    head: &str,
    candidates: Vec<Option<String>>,
) -> Result<(), String> {
    let mut last = String::from("the machine could not prepare its commits");
    for prerequisite in candidates {
        (source.valid)()?;
        let intent = ExportIntent {
            target: source.target.clone(),
            request_id: uuid::Uuid::new_v4().to_string(),
            expected_head: head.to_string(),
            prerequisite,
        };
        let prepare = || {
            source.host.export(ExportRequest {
                intent: intent.clone(),
                operation: ExportOperation::Prepare,
            })
        };
        match prepare() {
            Ok(ExportReply::Prepared { descriptor }) => {
                let result = validate_descriptor(&descriptor, &intent)
                    .and_then(|()| import(source, &intent, &descriptor));
                release(source, &intent, &descriptor);
                return result;
            }
            Ok(_) => return Err("unexpected export response".into()),
            Err(ExportCallError::Lost(error)) => {
                // The machine may have built it: ask once more for the same
                // export, so a bundle it holds is released.
                if let Ok(ExportReply::Prepared { descriptor }) = prepare() {
                    release(source, &intent, &descriptor);
                }
                return Err(error);
            }
            Err(ExportCallError::Refused(error)) => last = error,
        }
    }
    Err(last)
}

/// Bring the commits the machine's checkout is on into the clone.
pub(crate) fn fetch_into_clone(source: FetchSource<'_>) -> Result<FetchReport, String> {
    (source.valid)()?;
    if !source.clone.is_dir() {
        return Err("the attached local clone is missing on this Mac".into());
    }
    let branch = source.prepared_branch.clone();
    if !valid_branch(&branch) {
        return Err("the workspace's branch name cannot be used here".into());
    }
    let local_ref = format!("refs/heads/{branch}");
    let remote_ref = format!(
        "{REMOTE_REF_PREFIX}{}/{branch}",
        ref_segment(&source.machine_id)?
    );
    if !git2::Reference::is_valid_name(&remote_ref) {
        return Err("the machine's ref name cannot be used here".into());
    }
    let head = source.host.head(&source.target)?;
    if !valid_commit(&head.commit) {
        return Err("the machine named an invalid commit".into());
    }
    match head.branch.as_deref() {
        Some(reported) if reported == branch => {}
        Some(reported) => {
            return Err(format!(
                "the machine reports its checkout on {reported:?}, not the workspace's branch {branch}; nothing was fetched"
            ))
        }
        None => {
            return Err(
                "the machine's checkout is not on a branch (detached HEAD); check out the workspace's branch there, then fetch again".into(),
            )
        }
    }
    let previous_remote = read_ref(&source.clone, &remote_ref)?;
    let previous_local = read_ref(&source.clone, &local_ref)?;
    let previous_remote_commit = previous_remote.as_ref().and_then(|v| v.commit.clone());
    let previous_local_commit = previous_local.as_ref().and_then(|v| v.commit.clone());

    if !has_commit(&source.clone, &head.commit)? {
        let mut candidates: Vec<Option<String>> = Vec::new();
        for known in [
            previous_remote_commit.clone(),
            source.base_commit.clone(),
            Some(source.prepared_commit.clone()),
        ]
        .into_iter()
        .flatten()
        {
            if known != head.commit
                && valid_commit(&known)
                && has_commit(&source.clone, &known)?
                && !candidates.contains(&Some(known.clone()))
            {
                candidates.push(Some(known));
            }
        }
        candidates.push(None);
        transfer(&source, &head.commit, candidates)?;
    }
    (source.valid)()?;

    let prepared_known = (valid_commit(&source.prepared_commit)
        && has_commit(&source.clone, &source.prepared_commit)?)
    .then_some(source.prepared_commit.as_str());
    let base_known = match source.base_commit.as_deref() {
        Some(base) if valid_commit(base) && has_commit(&source.clone, base)? => Some(base),
        _ => None,
    };
    let known: Vec<&str> = [
        previous_remote_commit.as_deref(),
        previous_local_commit.as_deref(),
        prepared_known,
        base_known,
    ]
    .into_iter()
    .flatten()
    .collect();
    let (commits, commit_count) = arrived(&source.clone, &head.commit, &known)?;
    if previous_remote.as_ref().map(|v| v.raw.as_str()) != Some(head.commit.as_str()) {
        compare_and_set(
            &source.clone,
            &remote_ref,
            &head.commit,
            previous_remote.as_ref().map(|v| v.raw.as_str()),
        )?;
    }

    let not_moved = |reason: NotFastForwarded, message: String| {
        (LocalBranch::NotFastForwarded, Some(reason), Some(message))
    };
    let common = common_dir(&source.clone)?;
    let (state, reason, message) = match (&previous_local, previous_local_commit.as_deref()) {
        (None, _) => {
            compare_and_set(&source.clone, &local_ref, &head.commit, None)?;
            (LocalBranch::Created, None, None)
        }
        (Some(_), None) => not_moved(
            NotFastForwarded::Diverged,
            format!("Local {branch} does not name a commit; the machine's work is at {remote_ref}"),
        ),
        (Some(_), Some(local)) if local == head.commit => (LocalBranch::UpToDate, None, None),
        (Some(_), Some(local)) if is_ancestor(&source.clone, &head.commit, local)? => {
            (LocalBranch::Ahead, None, None)
        }
        (Some(_), Some(local)) if !is_ancestor(&source.clone, local, &head.commit)? => not_moved(
            NotFastForwarded::Diverged,
            format!(
                "Local {branch} has commits the machine's branch does not; the machine's work is at {remote_ref} for you to combine"
            ),
        ),
        (Some(old), Some(_)) => {
            if let Some(worktree) = operation_on(&source.clone, &common, &branch)? {
                not_moved(
                    NotFastForwarded::CheckedOutElsewhere,
                    format!(
                        "A rebase or bisect of {branch} is in progress in {}; the machine's work is at {remote_ref}",
                        worktree.display()
                    ),
                )
            } else {
                match checkout_of(&source.clone, &local_ref)? {
                    None => {
                        compare_and_set(&source.clone, &local_ref, &head.commit, Some(&old.raw))?;
                        (LocalBranch::FastForwarded, None, None)
                    }
                    Some(worktree) if same_dir(&worktree, &source.clone) => {
                        fast_forward_checkout(&source.clone, &branch, &local_ref, &remote_ref, &head.commit)?
                    }
                    Some(worktree) => not_moved(
                        NotFastForwarded::CheckedOutElsewhere,
                        format!(
                            "{branch} is checked out in {}; the machine's work is at {remote_ref}",
                            worktree.display()
                        ),
                    ),
                }
            }
        }
    };
    Ok(FetchReport {
        machine_id: source.machine_id.clone(),
        workspace_handle: source.target.handle.clone(),
        source_repo_path: source.clone.to_string_lossy().into_owned(),
        branch: branch.clone(),
        commit: head.commit.clone(),
        remote_ref,
        previous_remote_ref: previous_remote_commit,
        commits,
        commit_count,
        local_branch: branch,
        local_branch_state: state,
        fast_forwarded: state == LocalBranch::FastForwarded,
        reason,
        message,
    })
}

type BranchOutcome = (LocalBranch, Option<NotFastForwarded>, Option<String>);

/// Fast-forward `local_ref`, checked out in the clone itself, to `commit`.
///
/// The checkout's index lock is held for the whole update, so no checkout or
/// switch can run meanwhile; under it HEAD must still be on `local_ref` and
/// the checkout clean. The files move with a two-tree `read-tree -m -u` from
/// the branch's current commit to `commit` (into a scratch copy of the index
/// that replaces the real one only on success), then `local_ref` alone moves
/// by compare-and-set. No other branch's ref or files can be touched.
fn fast_forward_checkout(
    clone: &Path,
    branch: &str,
    local_ref: &str,
    remote_ref: &str,
    commit: &str,
) -> Result<BranchOutcome, String> {
    fast_forward_checkout_with(clone, branch, local_ref, remote_ref, commit, |_| {})
}

/// Points inside [`fast_forward_checkout`] where tests interleave other Git
/// commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckoutStep {
    /// The first look passed; the index lock is not held yet.
    BeforeLock,
    /// The index lock is held and the checkout re-verified.
    Locked,
    /// The files and the branch moved; the index is not replaced yet.
    BeforeCommit,
}

fn fast_forward_checkout_with(
    clone: &Path,
    branch: &str,
    local_ref: &str,
    remote_ref: &str,
    commit: &str,
    mut step: impl FnMut(CheckoutStep),
) -> Result<BranchOutcome, String> {
    let dirty = || {
        Ok::<_, String>((
            LocalBranch::NotFastForwarded,
            Some(NotFastForwarded::DirtyCheckout),
            Some(format!(
                "{branch} is checked out in the local clone with uncommitted changes; the machine's work is at {remote_ref}"
            )),
        ))
    };
    let changed = || {
        Ok::<_, String>((
            LocalBranch::NotFastForwarded,
            Some(NotFastForwarded::CheckoutChanged),
            Some(format!(
                "the local clone's checkout moved off {branch} during the fetch; the machine's work is at {remote_ref}"
            )),
        ))
    };
    let is_clean = || -> Result<bool, String> {
        Ok(
            query(clone, &["status", "--porcelain", "--untracked-files=no"])?
                .ok_or("local clone status unavailable")?
                .trim()
                .is_empty(),
        )
    };
    let on_branch = || -> Result<bool, String> {
        Ok(query(clone, &["symbolic-ref", "-q", "HEAD"])?.as_deref() == Some(local_ref))
    };

    if !is_clean()? {
        return dirty();
    }
    if !on_branch()? {
        return changed();
    }
    step(CheckoutStep::BeforeLock);

    let index = index_path(clone)?;
    // A lock already there is Git's (or another tool's); it is never taken
    // over or deleted. If no Git command is running, it was left by one that
    // crashed, and the message names it for the person to remove.
    let Some(lock) = IndexLock::acquire(&index)? else {
        return Ok((
            LocalBranch::NotFastForwarded,
            Some(NotFastForwarded::CheckoutChanged),
            Some(format!(
                "another Git command is using the local clone's checkout of {branch} ({} exists; if no Git command is running, it was left behind and can be removed); the machine's work is at {remote_ref}",
                IndexLock::path_for(&index).display()
            )),
        ));
    };
    if !on_branch()? {
        return changed();
    }
    if !is_clean()? {
        return dirty();
    }
    let old = read_ref(clone, local_ref)?
        .and_then(|value| value.commit)
        .ok_or_else(|| format!("{local_ref} changed while fetching; fetch again"))?;
    if old == commit {
        return Ok((LocalBranch::UpToDate, None, None));
    }
    if !is_ancestor(clone, &old, commit)? {
        return Err(format!("{local_ref} changed while fetching; fetch again"));
    }
    step(CheckoutStep::Locked);

    ScratchIndex::remove_stale(&index);
    let scratch = ScratchIndex::copy_of(&index)?;
    if !scratch.read_tree(clone, &old, commit)? {
        return Ok((
            LocalBranch::NotFastForwarded,
            Some(NotFastForwarded::DirtyCheckout),
            Some(format!(
                "{branch} is checked out in the local clone and its files block the update; the machine's work is at {remote_ref}"
            )),
        ));
    }
    if let Err(error) = compare_and_set(clone, local_ref, commit, Some(&old)) {
        // The files moved but the branch did not: put them back. The real
        // index was never replaced.
        let _ = scratch.read_tree(clone, commit, &old);
        return Err(error);
    }
    step(CheckoutStep::BeforeCommit);
    if let Err(error) = lock.commit(&scratch.path) {
        // The branch and files moved but the index did not: left so, the
        // checkout would show the machine's work as staged to be undone. Put
        // the branch and files back; the index never changed.
        let _ = compare_and_set(clone, local_ref, &old, Some(commit));
        let _ = scratch.read_tree(clone, commit, &old);
        return Err(format!(
            "{error}; {branch} was left where it was and the machine's work is at {remote_ref}"
        ));
    }
    Ok((LocalBranch::FastForwarded, None, None))
}

/// The checkout's index file (absolute).
fn index_path(clone: &Path) -> Result<PathBuf, String> {
    let index = query(clone, &["rev-parse", "--git-path", "index"])?
        .ok_or("local clone index unavailable")?;
    let index = PathBuf::from(index);
    Ok(if index.is_absolute() {
        index
    } else {
        clone.join(index)
    })
}

/// `<index>.lock`, held the way Git holds it: created exclusively, removed
/// on drop unless committed (renamed over the index).
struct IndexLock {
    index: PathBuf,
    path: PathBuf,
    committed: bool,
}

impl IndexLock {
    fn path_for(index: &Path) -> PathBuf {
        let mut name = index.as_os_str().to_owned();
        name.push(".lock");
        PathBuf::from(name)
    }

    /// `None` when another Git command holds the lock.
    fn acquire(index: &Path) -> Result<Option<Self>, String> {
        let path = Self::path_for(index);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o666)
            .open(&path)
        {
            Ok(_) => Ok(Some(Self {
                index: index.to_path_buf(),
                path,
                committed: false,
            })),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(_) => Err("could not lock the local clone's index".into()),
        }
    }

    /// Replace the index with `contents`' bytes, through the lock file.
    fn commit(mut self, contents: &Path) -> Result<(), String> {
        let bytes =
            std::fs::read(contents).map_err(|_| "could not read the updated index".to_string())?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)
            .map_err(|_| "could not write the local clone's index".to_string())?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| "could not write the local clone's index".to_string())?;
        std::fs::rename(&self.path, &self.index)
            .map_err(|_| "could not replace the local clone's index".to_string())?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for IndexLock {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// A private copy of the index the update is staged in, removed on drop.
struct ScratchIndex {
    path: PathBuf,
}

const SCRATCH_INFIX: &str = ".codesense-fetch-";

impl ScratchIndex {
    /// Remove scratch indexes a fetch that crashed left behind. Only called
    /// with the index lock held, so no other fetch is using one.
    fn remove_stale(index: &Path) {
        let (Some(dir), Some(name)) = (index.parent(), index.file_name()) else {
            return;
        };
        let prefix = format!("{}{SCRATCH_INFIX}", name.to_string_lossy());
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&prefix)
                && entry.file_type().is_ok_and(|kind| kind.is_file())
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    fn copy_of(index: &Path) -> Result<Self, String> {
        let mut name = index.as_os_str().to_owned();
        name.push(format!("{SCRATCH_INFIX}{}", uuid::Uuid::new_v4().simple()));
        let path = PathBuf::from(name);
        if index.exists() {
            std::fs::copy(index, &path)
                .map_err(|_| "could not copy the local clone's index".to_string())?;
        }
        Ok(Self { path })
    }

    /// Two-tree `read-tree -m -u from to` against this index; `false` when
    /// Git refuses (files that would be overwritten).
    fn read_tree(&self, clone: &Path, from: &str, to: &str) -> Result<bool, String> {
        Ok(git(clone)
            .env("GIT_INDEX_FILE", &self.path)
            .args(["read-tree", "-m", "-u", from, to])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|_| "local Git executable unavailable")?
            .success())
    }
}

impl Drop for ScratchIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let mut lock = self.path.as_os_str().to_owned();
        lock.push(".lock");
        let _ = std::fs::remove_file(PathBuf::from(lock));
    }
}

#[cfg(test)]
mod tests;
