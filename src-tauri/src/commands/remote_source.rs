// SPDX-License-Identifier: MIT
//! DESIGN-remote-source.md §1 — the host-side Tauri commands for the remote
//! source + editor path.
//!
//! Routes every read/write through the existing
//! `codesense_pty_core::remote_files` wire protocol:
//! - Read   -> `FileOperation::Read`         (`Expected::Hash` short-circuit, `limit`)
//! - Write  -> `FileOperation::Apply{Write}` (`Expected::Hash` for optimistic lock)
//! - Resolve-> `FileOperation::Inspect`      (on `.git/HEAD`)
//! - Diff   -> `FileOperation::Changes` / `::Redline` (the machine's Git)
//!
//! The watcher is a host-side polling task over the diff stamp (falling back
//! to `FileOperation::TrackedPaths` on an older server); a follow-up swaps in
//! a pushed stream. The transport is a detail — the FE's
//! `use-remote-worktree-watch.ts` is identical in both worlds.

use crate::commands::git_sync::{
    build_redline, is_toggle_type, looks_binary, FileRedline, PublishFile, PublishFileBadge,
    PublishFileStatus,
};
use crate::commands::route_to_context::{derive_active_context, ActiveContext};
use crate::mcp::remote_workspace_target;
use crate::terminal::session_server::SessionServerClient;
use codesense_pty_core::remote_files as wire;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// DESIGN-remote-source.md §1 — the one typed error for every remote source
/// command. Every FE call site matches exhaustively (AD-3); no silent swallow.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RemoteSourceError {
    /// The machine is unreachable, or the connection retired mid-call.
    Disconnected,
    /// The caller's authority on the workspace was revoked.
    Unauthorized,
    /// The file was not found on the remote (deleted/renamed between read and write).
    NotFound,
    /// The remote reported the file exceeds `MAX_FILE_BYTES` (4 MiB).
    BoundsExceeded,
    /// Optimistic-lock mismatch — the server's `Observation::Regular` is returned
    /// so the FE's merge view can seed "theirs" without a second read.
    Conflict { observation: RemoteFileObservation },
    /// Catch-all for transport/serialization errors with a short diagnostic.
    TransportFailure { message: String },
}

impl RemoteSourceError {
    fn from_string(err: String) -> Self {
        // Heuristic string-match against the few shapes the resolve + session
        // server code actually produces, so the FE gets a typed error rather
        // than catching strings. New shapes fall through to TransportFailure.
        //
        // M-4 fix — specific terms (`authority`, `permission`, `denied`,
        // `absent`, `not found`, `exceeds`, `too large`) are checked BEFORE
        // the broader `retired`/`unavailable`/`disconnected` set, so a string
        // like "private remote authority retired" classifies as Unauthorized
        // (its actual meaning) rather than Disconnected.
        let s = err.to_lowercase();
        if s.contains("authority") || s.contains("permission") || s.contains("denied") {
            RemoteSourceError::Unauthorized
        } else if s.contains("not found") || s.contains("absent") {
            RemoteSourceError::NotFound
        } else if s.contains("exceeds") || s.contains("too large") {
            RemoteSourceError::BoundsExceeded
        } else if s.contains("retired") || s.contains("unavailable") || s.contains("disconnected") {
            RemoteSourceError::Disconnected
        } else {
            RemoteSourceError::TransportFailure { message: err }
        }
    }
}

/// FE-serialized mirror of `wire::Observation::Regular`. Hash is lower-hex
/// (64 chars); mtime is nanoseconds since epoch; mode is POSIX bits.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileObservation {
    pub hash: String,
    pub mtime_ns: u64,
    pub size: u64,
    pub mode: u32,
}

impl RemoteFileObservation {
    fn from_wire(obs: wire::Observation) -> Result<Self, RemoteSourceError> {
        match obs {
            wire::Observation::Regular {
                hash,
                mtime_ns,
                size,
                mode,
            } => Ok(RemoteFileObservation {
                hash: hex_encode(&hash),
                mtime_ns,
                size,
                mode,
            }),
            wire::Observation::Absent => Err(RemoteSourceError::NotFound),
            wire::Observation::Directory | wire::Observation::Unsafe => {
                Err(RemoteSourceError::TransportFailure {
                    message: "path is not a regular file".into(),
                })
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileRead {
    pub base64: String,
    pub observation: RemoteFileObservation,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileWrite {
    pub observation: RemoteFileObservation,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabSourceRepo {
    pub root: String,
    pub is_root: bool,
}

/// DESIGN-remote-phase-2.md §3 — one entry of a remote single-level directory
/// listing. Byte-identical shape to `codesense_fs_core::FileEntry` so the FE's
/// `FileEntry` type (and the `WorktreeFileTree` consumer) works unchanged, with
/// one wire difference: `path` is RELATIVE to the remote worktree root (the
/// host-agent speaks in rooted paths). The FE's `remoteList` wrapper re-anchors
/// each entry to the remote absolute path before handing to the tree.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteFileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub extension: Option<String>,
    /// Byte size for a file; `None` for a directory. We don't have per-entry
    /// metadata from `FileOperation::Enumerate` yet — this is reserved for the
    /// follow-up that lands a dedicated single-level `List` wire variant. The
    /// FE's §B pre-read size gate degrades to a plain read on `null`, so the
    /// tree stays browsable in the meantime.
    pub size: Option<u64>,
}

// ---- Internal helpers ------------------------------------------------------

/// Resolve the (client, target) pair for a `(machine_id, handle)` from the
/// current active context. Used by every command below.
async fn resolve_target(
    app: &AppHandle,
    machine_id: &str,
    handle: &str,
) -> Result<(Arc<SessionServerClient>, wire::FileTarget), RemoteSourceError> {
    let ctx: ActiveContext = derive_active_context(app)
        .await
        .map_err(RemoteSourceError::from_string)?;
    // `resolve` filters on machine_id and the workspace handle; the ambient
    // active team is the authorization scope. No `caller`: these are host
    // Tauri commands, not a reverse MCP call that captured one.
    let resolved =
        remote_workspace_target::resolve(app.clone(), ctx, Some(machine_id), handle, None)
            .await
            .map_err(RemoteSourceError::from_string)?;
    Ok((resolved.client, resolved.target))
}

fn parse_hex_hash(h: &str) -> Result<[u8; 32], RemoteSourceError> {
    if h.len() != 64 {
        return Err(RemoteSourceError::TransportFailure {
            message: "expectedHash must be 64 lower-hex characters".into(),
        });
    }
    let mut bytes = [0u8; 32];
    for (i, chunk) in h.as_bytes().chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        bytes[i] = (hi << 4) | lo;
    }
    Ok(bytes)
}

fn hex_nibble(b: u8) -> Result<u8, RemoteSourceError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(10 + b - b'a'),
        _ => Err(RemoteSourceError::TransportFailure {
            message: "expectedHash contains a non-hex character".into(),
        }),
    }
}

fn hex_encode(bytes: &[u8; 32]) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn file_target(client: Arc<SessionServerClient>, target: wire::FileTarget) -> FileAdapter {
    FileAdapter { client, target }
}

/// One-shot wrapper over `SessionServerClient::workspace_file` for a bound
/// `FileTarget`. Keeps the call-site in each command one line.
struct FileAdapter {
    client: Arc<SessionServerClient>,
    target: wire::FileTarget,
}

impl FileAdapter {
    fn call(&self, operation: wire::FileOperation) -> Result<wire::FileReply, RemoteSourceError> {
        self.call_raw(operation)
            .map_err(RemoteSourceError::from_string)
    }

    /// The host's error string unclassified, for callers that must tell one
    /// failure shape from another before it collapses into a typed variant.
    fn call_raw(&self, operation: wire::FileOperation) -> Result<wire::FileReply, String> {
        self.client.workspace_file(wire::FileRequest {
            target: self.target.clone(),
            operation,
        })
    }
}

/// Run one blocking file call off the async runtime's worker threads.
async fn call_blocking(
    adapter: FileAdapter,
    operation: wire::FileOperation,
) -> Result<wire::FileReply, RemoteSourceError> {
    tauri::async_runtime::spawn_blocking(move || adapter.call(operation))
        .await
        .map_err(|e| RemoteSourceError::TransportFailure {
            message: format!("remote call task failed: {e}"),
        })?
}

// ---- `#[tauri::command]` entry points --------------------------------------

/// DESIGN-remote-source.md §3.4 — remote mirror of `resolve_tab_source_repo`.
/// Reports whether the managed workspace's worktree is a git working tree by
/// probing `.git/HEAD` through `FileOperation::Inspect`. Carries the Diff half
/// of the Source/Diff capability split — the FE's `useTabSourceRepo` wraps the
/// `None` return as `{folder, repo:null}` so a non-git remote dir keeps its
/// Source affordance while the Diff icon stays dark.
#[tauri::command]
pub async fn resolve_tab_source_repo_remote(
    app: AppHandle,
    machine_id: String,
    handle: String,
) -> Option<TabSourceRepo> {
    let (client, target) = resolve_target(&app, &machine_id, &handle).await.ok()?;
    let adapter = file_target(client, target);
    let reply = call_blocking(
        adapter,
        wire::FileOperation::Inspect {
            path: ".git/HEAD".into(),
        },
    )
    .await
    .ok()?;
    match reply {
        wire::FileReply::Inspected {
            observation: wire::Observation::Regular { .. },
        } => Some(TabSourceRepo {
            root: format!("remote://{}/{}", machine_id, handle),
            is_root: true,
        }),
        _ => None,
    }
}

/// DESIGN-remote-source.md §4.2 — read a file on the remote worktree. Returns
/// base64-encoded bytes and the current observation (hash + mtime + size + mode).
#[tauri::command]
pub async fn source_remote_file_read(
    app: AppHandle,
    machine_id: String,
    handle: String,
    path: String,
    expected_hash: Option<String>,
    limit: Option<u64>,
) -> Result<RemoteFileRead, RemoteSourceError> {
    let (client, target) = resolve_target(&app, &machine_id, &handle).await?;
    let adapter = file_target(client, target);
    let expected = expected_hash.as_deref().map(parse_hex_hash).transpose()?;
    let max_limit = wire::MAX_FILE_BYTES;
    let limit = limit
        .map(|n| (n as usize).min(max_limit))
        .unwrap_or(max_limit);
    let reply = run_blocking(move || {
        match adapter.call_raw(wire::FileOperation::Read {
            path: path.clone(),
            expected_hash: expected,
            limit,
        }) {
            Ok(reply) => Ok(reply),
            Err(error) => {
                // The host answers one string for "absent", "too large" and
                // "changed mid-read"; ask what is actually there so the editor can
                // say which.
                let inspected = adapter.call_raw(wire::FileOperation::Inspect { path });
                Err(classify_read_failure(error, inspected, limit, expected))
            }
        }
    })
    .await??;
    match reply {
        wire::FileReply::Read { observation, bytes } => {
            let obs = RemoteFileObservation::from_wire(observation)?;
            let truncated = bytes.len() == max_limit && (obs.size as usize) > max_limit;
            Ok(RemoteFileRead {
                base64: base64_encode(&bytes),
                observation: obs,
                truncated,
            })
        }
        _ => Err(RemoteSourceError::TransportFailure {
            message: "unexpected reply to Read".into(),
        }),
    }
}

/// DESIGN-remote-source.md §4.2 — atomic write to a remote worktree file,
/// optimistic-locked against `expected_hash`. Maps `EffectOutcome::Conflict`
/// onto the typed `RemoteSourceError::Conflict{observation}` so the FE's
/// merge-view flow doesn't have to re-read.
#[tauri::command]
pub async fn source_remote_file_write(
    app: AppHandle,
    machine_id: String,
    handle: String,
    path: String,
    base64: String,
    expected_hash: Option<String>,
) -> Result<RemoteFileWrite, RemoteSourceError> {
    let bytes = base64_decode(&base64)?;
    if bytes.len() > wire::MAX_FILE_BYTES {
        return Err(RemoteSourceError::BoundsExceeded);
    }
    let (client, target) = resolve_target(&app, &machine_id, &handle).await?;
    let adapter = file_target(client, target);
    let expected = match expected_hash.as_deref() {
        None => wire::Expected::Absent,
        Some(h) => wire::Expected::Hash {
            hash: parse_hex_hash(h)?,
        },
    };
    // The operation_id is a nonce so a retried in-flight Apply can be matched
    // against its earlier result; v1 generates a fresh one per call.
    let operation_id = next_operation_id();
    let reply = call_blocking(
        adapter,
        wire::FileOperation::Apply {
            operation_id: operation_id.clone(),
            path,
            expected,
            effect: wire::Effect::Write { bytes, mode: None },
        },
    )
    .await?;
    match reply {
        wire::FileReply::Effect { outcome } => match outcome {
            wire::EffectOutcome::Applied { observation } => Ok(RemoteFileWrite {
                observation: RemoteFileObservation::from_wire(observation)?,
            }),
            wire::EffectOutcome::Conflict { observation } => Err(RemoteSourceError::Conflict {
                observation: RemoteFileObservation::from_wire(observation)?,
            }),
            wire::EffectOutcome::NotStarted { .. } | wire::EffectOutcome::Unknown { .. } => {
                Err(RemoteSourceError::TransportFailure {
                    message: "remote write outcome unknown".into(),
                })
            }
        },
        _ => Err(RemoteSourceError::TransportFailure {
            message: "unexpected reply to Apply".into(),
        }),
    }
}

/// DESIGN-remote-phase-2.md §3 / Bug A — single-level directory listing on a
/// remote worktree, mirroring the local `list_directory` command. The host
/// protocol doesn't ship a dedicated per-directory list op yet (that's the
/// follow-up), so this routes through `FileOperation::Enumerate` under the
/// requested prefix and reduces the recursive result down to the immediate
/// children. A nested path under `<child>/...` marks `<child>` as a directory;
/// anything else is a file.
///
/// `path` is the worktree-relative directory to list (empty string = root).
/// Returned entries' `path` is also worktree-relative (`src/foo.ts`,
/// `src/features`); the FE re-anchors them to the remote absolute root.
#[tauri::command]
pub async fn source_remote_list_directory(
    app: AppHandle,
    machine_id: String,
    handle: String,
    path: String,
) -> Result<Vec<RemoteFileEntry>, RemoteSourceError> {
    let (client, target) = resolve_target(&app, &machine_id, &handle).await?;
    let adapter = file_target(client, target);
    // Treat an empty / "." input as the root.
    let trimmed = path.trim_matches('/');
    let prefix = if trimmed.is_empty() || trimmed == "." {
        None
    } else {
        Some(trimmed.to_string())
    };
    run_blocking(move || list_directory_blocking(&adapter, prefix)).await?
}

impl FileAdapter {
    /// Whether the host answers the editor operations (`List`, `ReadHead`,
    /// `ListFiles`); an older host gets the fallback built from older ones.
    fn editor_ops(&self) -> bool {
        self.client.supports_workspace_editor()
    }
}

/// Run blocking remote work (one or more synchronous `workspace_file` calls)
/// off the async runtime's worker threads.
async fn run_blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, RemoteSourceError> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| RemoteSourceError::TransportFailure {
            message: format!("remote call task failed: {e}"),
        })
}

fn list_directory_blocking(
    adapter: &FileAdapter,
    prefix: Option<String>,
) -> Result<Vec<RemoteFileEntry>, RemoteSourceError> {
    if adapter.editor_ops() {
        return match adapter.call(wire::FileOperation::List {
            path: prefix.clone(),
        })? {
            wire::FileReply::Listing { entries } => {
                Ok(entries_from_listing(prefix.as_deref(), entries))
            }
            _ => Err(RemoteSourceError::TransportFailure {
                message: "unexpected reply to List".into(),
            }),
        };
    }
    // An older host only enumerates recursively, which fails outright once the
    // subtree (a `node_modules`, a `target`) passes the path limit. Fall back
    // to the tracked set then: incomplete for untracked files, but browsable.
    let enumerated = adapter.call_raw(wire::FileOperation::Enumerate {
        prefix: prefix.clone(),
        limit: wire::MAX_PATHS,
    });
    let paths = match enumerated {
        Ok(wire::FileReply::Paths { paths }) => paths,
        Ok(_) => {
            return Err(RemoteSourceError::TransportFailure {
                message: "unexpected reply to Enumerate".into(),
            })
        }
        Err(error) if error.contains("exceeds limit") => {
            match adapter.call(wire::FileOperation::TrackedPaths {
                prefix: prefix.clone(),
                limit: wire::MAX_PATHS,
            })? {
                wire::FileReply::Paths { paths } => paths,
                _ => {
                    return Err(RemoteSourceError::TransportFailure {
                        message: "unexpected reply to TrackedPaths".into(),
                    })
                }
            }
        }
        Err(error) => return Err(RemoteSourceError::from_string(error)),
    };
    Ok(reduce_enumeration_to_children(prefix.as_deref(), &paths))
}

/// Re-root a host `List` reply under `prefix` in the tree's entry shape.
fn entries_from_listing(
    prefix: Option<&str>,
    entries: Vec<wire::ListEntry>,
) -> Vec<RemoteFileEntry> {
    let base = prefix
        .map(|p| p.trim_matches('/'))
        .filter(|p| !p.is_empty());
    entries
        .into_iter()
        .map(|entry| {
            let path = match base {
                Some(base) => format!("{base}/{}", entry.name),
                None => entry.name.clone(),
            };
            let extension = if entry.is_dir {
                None
            } else {
                std::path::Path::new(&entry.name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_string)
            };
            RemoteFileEntry {
                name: entry.name,
                path,
                is_dir: entry.is_dir,
                extension,
                size: entry.size,
            }
        })
        .collect()
}

/// Turn a failed `Read` into the error the editor should show, using an
/// `Inspect` of the same path taken right after.
fn classify_read_failure(
    read_error: String,
    inspected: Result<wire::FileReply, String>,
    limit: usize,
    expected_hash: Option<[u8; 32]>,
) -> RemoteSourceError {
    match inspected {
        Ok(wire::FileReply::Inspected { observation }) => match observation {
            wire::Observation::Absent => RemoteSourceError::NotFound,
            wire::Observation::Directory | wire::Observation::Unsafe => {
                RemoteSourceError::TransportFailure {
                    message: "not a regular file the editor can open".into(),
                }
            }
            // Larger than this read may carry: a retry can never succeed.
            wire::Observation::Regular { size, .. } if size > limit as u64 => {
                RemoteSourceError::BoundsExceeded
            }
            // The caller pinned a version the file no longer has: retrying with
            // the same hash can never succeed, so hand back what is there.
            wire::Observation::Regular { hash, .. }
                if expected_hash.is_some_and(|expected| expected != hash) =>
            {
                match RemoteFileObservation::from_wire(observation) {
                    Ok(observation) => RemoteSourceError::Conflict { observation },
                    Err(error) => error,
                }
            }
            // Present and readable now: it changed during the read. Retrying
            // is the honest answer.
            wire::Observation::Regular { .. } => RemoteSourceError::TransportFailure {
                message: "the file changed while it was being read — try again".into(),
            },
        },
        Ok(_) => RemoteSourceError::from_string(read_error),
        Err(inspect_error) if inspect_error.contains("exceeds") => {
            RemoteSourceError::BoundsExceeded
        }
        Err(inspect_error) => RemoteSourceError::from_string(inspect_error),
    }
}

/// The committed bytes of a remote file, for the editor's change gutter.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileHead {
    /// Base64 bytes at `HEAD`; `None` when the file is new or untracked.
    pub base64: Option<String>,
    /// False when the host predates the operation — the editor then shows no
    /// change markers rather than marking every line new.
    pub supported: bool,
}

/// Quick-open's file list for a remote worktree.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileList {
    pub paths: Vec<String>,
    pub truncated: bool,
    /// False when an older host could only answer tracked files.
    pub includes_untracked: bool,
}

/// Inspect one remote file without transferring it — the open editor polls
/// this to notice an edit made on the machine, which the path-set watcher
/// cannot see (an in-place edit leaves the tracked set unchanged).
#[tauri::command]
pub async fn source_remote_file_inspect(
    app: AppHandle,
    machine_id: String,
    handle: String,
    path: String,
) -> Result<RemoteFileObservation, RemoteSourceError> {
    let (client, target) = resolve_target(&app, &machine_id, &handle).await?;
    let adapter = file_target(client, target);
    match call_blocking(adapter, wire::FileOperation::Inspect { path }).await? {
        wire::FileReply::Inspected { observation } => RemoteFileObservation::from_wire(observation),
        _ => Err(RemoteSourceError::TransportFailure {
            message: "unexpected reply to Inspect".into(),
        }),
    }
}

/// A remote file's committed (`HEAD`) bytes, for the editor's change gutter.
#[tauri::command]
pub async fn source_remote_file_head(
    app: AppHandle,
    machine_id: String,
    handle: String,
    path: String,
) -> Result<RemoteFileHead, RemoteSourceError> {
    let (client, target) = resolve_target(&app, &machine_id, &handle).await?;
    let adapter = file_target(client, target);
    if !adapter.editor_ops() {
        return Ok(RemoteFileHead {
            base64: None,
            supported: false,
        });
    }
    match call_blocking(adapter, wire::FileOperation::ReadHead { path }).await? {
        wire::FileReply::Head { bytes } => Ok(RemoteFileHead {
            base64: bytes.as_deref().map(base64_encode),
            supported: true,
        }),
        _ => Err(RemoteSourceError::TransportFailure {
            message: "unexpected reply to ReadHead".into(),
        }),
    }
}

/// Quick-open's file list for a remote worktree: tracked plus untracked files
/// Git does not ignore (tracked only on a host that predates `ListFiles`).
#[tauri::command]
pub async fn source_remote_list_files(
    app: AppHandle,
    machine_id: String,
    handle: String,
    limit: Option<usize>,
) -> Result<RemoteFileList, RemoteSourceError> {
    let limit = limit.unwrap_or(wire::MAX_PATHS).clamp(1, wire::MAX_PATHS);
    let (client, target) = resolve_target(&app, &machine_id, &handle).await?;
    let adapter = file_target(client, target);
    if adapter.editor_ops() {
        return match call_blocking(adapter, wire::FileOperation::ListFiles { limit }).await? {
            wire::FileReply::FileList { paths, truncated } => Ok(RemoteFileList {
                paths,
                truncated,
                includes_untracked: true,
            }),
            _ => Err(RemoteSourceError::TransportFailure {
                message: "unexpected reply to ListFiles".into(),
            }),
        };
    }
    match call_blocking(
        adapter,
        wire::FileOperation::TrackedPaths {
            prefix: None,
            limit: wire::MAX_PATHS,
        },
    )
    .await?
    {
        wire::FileReply::Paths { mut paths } => {
            let truncated = paths.len() > limit;
            paths.truncate(limit);
            Ok(RemoteFileList {
                paths,
                truncated,
                includes_untracked: false,
            })
        }
        _ => Err(RemoteSourceError::TransportFailure {
            message: "unexpected reply to TrackedPaths".into(),
        }),
    }
}

/// Reduce an `Enumerate` result (recursive, worktree-rooted paths) down to the
/// immediate children of `prefix`. Any descendant whose first path component
/// past the prefix also has further components marks that component as a
/// directory; a leaf (no further components) is a regular file. Deterministic
/// order (natural ASCII lexicographic) mirrors the local `FsService::list`
/// ordering well enough for the lazy tree (local orders dirs-first; the remote
/// side is reserved for the follow-up `List` op).
fn reduce_enumeration_to_children(prefix: Option<&str>, paths: &[String]) -> Vec<RemoteFileEntry> {
    use std::collections::BTreeMap;
    let strip = |p: &str| -> Option<String> {
        match prefix {
            None => Some(p.to_string()),
            Some(pre) => {
                let pre = pre.trim_matches('/');
                if pre.is_empty() {
                    return Some(p.to_string());
                }
                p.strip_prefix(pre)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .map(|s| s.to_string())
            }
        }
    };
    // name -> is_dir (true if ANY descendant lives below this name).
    let mut by_name: BTreeMap<String, bool> = BTreeMap::new();
    for p in paths {
        let Some(rel) = strip(p) else { continue };
        if rel.is_empty() {
            continue;
        }
        let (head, has_tail) = match rel.find('/') {
            Some(i) => (rel[..i].to_string(), true),
            None => (rel.clone(), false),
        };
        // Mark as directory if we see any descendant below it; otherwise it's a
        // leaf (regular file). An entry upgrades to dir if a later path reveals
        // a child — accomplished by OR-ing.
        let entry = by_name.entry(head).or_insert(false);
        if has_tail {
            *entry = true;
        }
    }
    by_name
        .into_iter()
        .map(|(name, is_dir)| {
            let full = match prefix {
                None => name.clone(),
                Some(pre) => {
                    let pre = pre.trim_matches('/');
                    if pre.is_empty() {
                        name.clone()
                    } else {
                        format!("{pre}/{name}")
                    }
                }
            };
            let extension = if is_dir {
                None
            } else {
                std::path::Path::new(&name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.to_string())
            };
            RemoteFileEntry {
                name,
                path: full,
                is_dir,
                extension,
                size: None,
            }
        })
        .collect()
}

/// Shown when the machine's terminal server predates the diff operations: the
/// server answers the call with "unsupported request" instead of a diff.
const DIFF_NEEDS_SERVER_UPDATE: &str =
    "This machine's terminal server is too old to diff its workspaces. Update the terminal server from Machines, then retry.";

fn diff_error(err: RemoteSourceError) -> RemoteSourceError {
    match err {
        RemoteSourceError::TransportFailure { message }
            if message.contains("unsupported request") =>
        {
            RemoteSourceError::TransportFailure {
                message: DIFF_NEEDS_SERVER_UPDATE.into(),
            }
        }
        other => other,
    }
}

/// The remote change list as the desktop's `PublishFile` records, classified
/// exactly like the local `records_from_diff`: A/M/D, a `rename` badge with the
/// pre-rename path, a `mode` badge, and a toggle-type rename (an `.excalidraw`,
/// an image…) un-paired into a clean delete + add so it never redlines by its
/// old path. The EOL-only badge needs both blobs and is left to the redline.
pub(crate) fn publish_files_from_wire(files: Vec<wire::ChangedFile>) -> Vec<PublishFile> {
    let status = |s: wire::ChangeStatus| match s {
        wire::ChangeStatus::Added => PublishFileStatus::Added,
        wire::ChangeStatus::Modified => PublishFileStatus::Modified,
        wire::ChangeStatus::Deleted => PublishFileStatus::Deleted,
    };
    let mut out = Vec::with_capacity(files.len());
    for file in files {
        if let Some(old) = file
            .old_path
            .as_deref()
            .filter(|_| is_toggle_type(&file.path))
        {
            out.push(PublishFile {
                path: file.path.clone(),
                status: PublishFileStatus::Added,
                badges: Vec::new(),
                rename_old: None,
                added: None,
                removed: None,
            });
            out.push(PublishFile {
                path: old.to_string(),
                status: PublishFileStatus::Deleted,
                badges: Vec::new(),
                rename_old: None,
                added: None,
                removed: None,
            });
            continue;
        }
        let mut badges = Vec::new();
        if file.old_path.is_some() {
            badges.push(PublishFileBadge::Rename);
        }
        if file.mode_changed {
            badges.push(PublishFileBadge::Mode);
        }
        out.push(PublishFile {
            path: file.path,
            status: status(file.status),
            badges,
            rename_old: file.old_path,
            added: file.added,
            removed: file.removed,
        });
    }
    out
}

/// The remote redline sides as the desktop's `FileRedline`, through the same
/// builder the local redline uses (status from which sides exist; binary when
/// either side sniffs binary or the remote marked a side opaque).
pub(crate) fn redline_from_wire(
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    opaque: bool,
) -> FileRedline {
    let side = |bytes: Option<Vec<u8>>| {
        bytes.map(|b| {
            let binary = opaque || looks_binary(&b);
            (b, binary)
        })
    };
    build_redline(side(before), side(after))
}

/// The remote mirror of `source_worktree_changeset`: the workspace's changed
/// files against its fork point (`base_ref: "HEAD"` for uncommitted only),
/// computed by the machine's Git over the workspace file protocol.
#[tauri::command]
pub async fn source_remote_changeset(
    app: AppHandle,
    machine_id: String,
    handle: String,
    base_ref: Option<String>,
) -> Result<Vec<PublishFile>, RemoteSourceError> {
    let (client, target) = resolve_target(&app, &machine_id, &handle).await?;
    let reply = call_blocking(
        file_target(client, target),
        wire::FileOperation::Changes {
            base_ref: base_ref.filter(|r| !r.is_empty()),
        },
    )
    .await
    .map_err(diff_error)?;
    match reply {
        wire::FileReply::Changes { files, .. } => Ok(publish_files_from_wire(files)),
        _ => Err(RemoteSourceError::TransportFailure {
            message: "unexpected reply to Changes".into(),
        }),
    }
}

/// The remote mirror of `source_file_redline`, against the same baseline as
/// [`source_remote_changeset`].
#[tauri::command]
pub async fn source_remote_file_redline(
    app: AppHandle,
    machine_id: String,
    handle: String,
    file_path: String,
    old_path: Option<String>,
    base_ref: Option<String>,
) -> Result<FileRedline, RemoteSourceError> {
    let (client, target) = resolve_target(&app, &machine_id, &handle).await?;
    let reply = call_blocking(
        file_target(client, target),
        wire::FileOperation::Redline {
            path: file_path,
            old_path: old_path.filter(|p| !p.is_empty()),
            base_ref: base_ref.filter(|r| !r.is_empty()),
        },
    )
    .await
    .map_err(diff_error)?;
    match reply {
        wire::FileReply::Redline {
            before,
            after,
            opaque,
        } => Ok(redline_from_wire(before, after, opaque)),
        _ => Err(RemoteSourceError::TransportFailure {
            message: "unexpected reply to Redline".into(),
        }),
    }
}

/// Which listing a watcher fingerprint came from. The two are never compared
/// with each other: a tick that falls back from the diff stamp to the tracked
/// listing (or back) must not read as a change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FingerprintSource {
    Diff,
    Tracked,
}

/// The polling watcher's memory between ticks.
#[derive(Debug)]
struct WatchMemory {
    /// The host answers the diff operations (cleared on "unsupported request").
    diff_supported: bool,
    /// Consecutive diff failures, and ticks left before the diff is tried again.
    diff_failures: u32,
    diff_skip: u32,
    last_diff: Option<u64>,
    last_tracked: Option<u64>,
    /// Nothing seen yet: the first fingerprint of a watch is announced.
    first: bool,
    /// Identity (generation) of the connection the last tick used.
    connection: Option<u64>,
}

impl Default for WatchMemory {
    fn default() -> Self {
        Self {
            diff_supported: true,
            diff_failures: 0,
            diff_skip: 0,
            last_diff: None,
            last_tracked: None,
            first: true,
            connection: None,
        }
    }
}

impl WatchMemory {
    /// A new connection may be to an updated server: what the old one could
    /// not answer, or kept failing at, is asked afresh.
    fn connected(&mut self, connection: u64) {
        if self.connection.is_some_and(|last| last != connection) {
            self.diff_supported = true;
            self.diff_failures = 0;
            self.diff_skip = 0;
        }
        self.connection = Some(connection);
    }

    /// Whether this tick's fingerprint should be announced: the first of the
    /// watch, a change against the last one from the same source, or the
    /// first from a source after the watch ran on the other one. Nothing from
    /// that source says what changed meanwhile (edits made while the diff
    /// failed, or a list that just grew too large), so it announces once; a
    /// later switch back and forth compares like with like and stays silent.
    fn observe(&mut self, source: FingerprintSource, fp: u64) -> bool {
        let (slot, other) = match source {
            FingerprintSource::Diff => (&mut self.last_diff, self.last_tracked),
            FingerprintSource::Tracked => (&mut self.last_tracked, self.last_diff),
        };
        let changed = slot.is_some_and(|last| last != fp);
        let first_after_other = slot.is_none() && other.is_some();
        *slot = Some(fp);
        let announce = changed || first_after_other || self.first;
        self.first = false;
        announce
    }

    /// A diff failure backs the diff off for a growing number of ticks (capped),
    /// so a worktree whose diff keeps failing isn't diffed on every tick.
    fn diff_failed(&mut self) {
        self.diff_failures = self.diff_failures.saturating_add(1);
        self.diff_skip = self
            .diff_failures
            .saturating_mul(self.diff_failures)
            .min(15);
    }
}

/// What the polling watcher compares between ticks. The diff stamp moves on any
/// content edit, commit or listing change; a server without the diff
/// operations only offers the tracked-path listing, which sees adds and
/// removals but not edits to an existing file.
fn watch_fingerprint(
    call: &dyn Fn(wire::FileOperation) -> Result<wire::FileReply, RemoteSourceError>,
    memory: &mut WatchMemory,
) -> Option<(FingerprintSource, u64)> {
    if memory.diff_supported && memory.diff_skip == 0 {
        match call(wire::FileOperation::Changes { base_ref: None }) {
            Ok(wire::FileReply::Changes { stamp, .. }) => {
                memory.diff_failures = 0;
                return Some((FingerprintSource::Diff, fingerprint_bytes(stamp.as_bytes())));
            }
            Err(RemoteSourceError::TransportFailure { message })
                if message.contains("unsupported request") =>
            {
                memory.diff_supported = false;
            }
            Err(RemoteSourceError::Disconnected | RemoteSourceError::Unauthorized) => return None,
            // Any other failure (an odd worktree the diff can't read, a busy
            // workspace): this tick falls back to the tracked-path listing
            // rather than going silent, and the diff backs off.
            _ => memory.diff_failed(),
        }
    } else if memory.diff_skip > 0 {
        memory.diff_skip -= 1;
    }
    let paths = match call(wire::FileOperation::TrackedPaths {
        prefix: None,
        limit: wire::MAX_PATHS,
    }) {
        Ok(wire::FileReply::Paths { paths }) => paths,
        _ => return None,
    };
    let mut joined = Vec::new();
    for p in &paths {
        joined.extend_from_slice(p.as_bytes());
        joined.push(0x1F);
    }
    Some((FingerprintSource::Tracked, fingerprint_bytes(&joined)))
}

fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    let mut fp: u64 = 0;
    for b in bytes {
        fp = fp.wrapping_mul(1099511628211).wrapping_add(*b as u64);
    }
    fp
}

// ---- Polling watcher -------------------------------------------------------

/// A started polling task's handle. DESIGN-remote-phase-2.md H-11 — the
/// Phase 1 shape stored the JoinHandle in a `_` field, which only DETACHES
/// the tokio task on drop. The task keeps polling for a handle the FE no
/// longer listens on until the Tauri app exits. The fix stores a real
/// `JoinHandle` and calls `.abort()` in `remove` so the task is actually
/// cancelled.
struct WatchEntry {
    handle: tauri::async_runtime::JoinHandle<()>,
}

/// Tauri-managed state keyed by the token the FE holds. The FE's ref-counted
/// module ensures ONE token per (machineId, handle), so this map is bounded by
/// the number of distinct open remote worktrees across all tabs.
pub struct RemoteWatchState {
    next: AtomicU64,
    entries: Mutex<std::collections::HashMap<u64, WatchEntry>>,
}

impl RemoteWatchState {
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
            entries: Mutex::new(std::collections::HashMap::new()),
        }
    }
    fn mint(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
    fn insert(&self, token: u64, handle: tauri::async_runtime::JoinHandle<()>) {
        self.entries
            .lock()
            .expect("remote watch state")
            .insert(token, WatchEntry { handle });
    }
    fn remove(&self, token: u64) {
        // H-11 — abort the tokio task, don't just drop the handle (which only
        // detaches it). Without `.abort()` the task runs forever polling for a
        // key the FE no longer listens on.
        if let Some(entry) = self
            .entries
            .lock()
            .expect("remote watch state")
            .remove(&token)
        {
            entry.handle.abort();
        }
    }
}

impl Default for RemoteWatchState {
    fn default() -> Self {
        Self::new()
    }
}

/// DESIGN-remote-source.md §3.3 / §5.4 — start a polling task for
/// `(machineId, handle)`. The task fingerprints the tracked-paths observation
/// every `poll_ms` (default 2000 ms) and emits
/// `remote-worktree-file-change-{machineId}-{handle}` on a fingerprint delta.
#[tauri::command]
pub async fn source_remote_watch_start(
    app: AppHandle,
    machine_id: String,
    handle: String,
    poll_ms: Option<u64>,
) -> Result<u64, RemoteSourceError> {
    use tauri::Manager;
    let state =
        app.try_state::<RemoteWatchState>()
            .ok_or_else(|| RemoteSourceError::TransportFailure {
                message: "RemoteWatchState not registered".into(),
            })?;
    let token = state.mint();
    let app_bg = app.clone();
    let event = format!("remote-worktree-file-change-{}-{}", machine_id, handle);
    let period = Duration::from_millis(poll_ms.unwrap_or(2000).max(500));
    let bound_machine = machine_id.clone();
    let bound_handle = handle.clone();
    let join = tauri::async_runtime::spawn(async move {
        // v1: the task does ONE resolve on start to warm the (client, target),
        // then polls `TrackedPaths` on each tick and emits on fingerprint
        // delta. A connection drop between ticks surfaces as a resolve-fail on
        // the next tick, which keeps the task alive (so a reconnect resumes
        // without the FE re-attaching).
        let mut memory = WatchMemory::default();
        loop {
            tokio::time::sleep(period).await;
            let Ok((client, target)) = resolve_target(&app_bg, &bound_machine, &bound_handle).await
            else {
                continue;
            };
            memory.connected(client.connection_generation());
            let adapter = file_target(client, target);
            // A full diff can take a while on a big worktree: off the async workers.
            let Ok((fp, kept)) = tauri::async_runtime::spawn_blocking(move || {
                let fp = watch_fingerprint(&|op| adapter.call(op), &mut memory);
                (fp, memory)
            })
            .await
            else {
                // The task panicked; its memory is gone. Start over rather
                // than announce a spurious change.
                memory = WatchMemory {
                    first: false,
                    ..WatchMemory::default()
                };
                continue;
            };
            memory = kept;
            let Some((source, fp)) = fp else {
                continue;
            };
            if memory.observe(source, fp) {
                // Payload is `{changedPath?: string | null}` — null signals
                // "list changed, caller should re-fetch the changeset". v2
                // push watch populates `changedPath`.
                if let Err(e) = app_bg.emit(&event, serde_json::json!({ "changedPath": null })) {
                    log::warn!(target: "remote_source", "watch emit failed: {e}");
                }
            }
        }
    });
    state.insert(token, join);
    Ok(token)
}

/// DESIGN-remote-phase-2.md §0 / B-3 — one-shot probe of the remote
/// worktree's tracked-paths count. Seeds `useRemoteCadence` so a repo over
/// `LARGE_REPO_THRESHOLD` (5000) auto-loosens to the `remoteLarge` preset.
/// Called once per `(machineId, handle)` by the FE watcher on its first
/// successful attach; failures are non-fatal (preset falls back to
/// `remoteSmall`).
#[tauri::command]
pub async fn source_remote_tracked_count(
    app: AppHandle,
    machine_id: String,
    handle: String,
) -> Result<u64, RemoteSourceError> {
    let (client, target) = resolve_target(&app, &machine_id, &handle).await?;
    let adapter = file_target(client, target);
    let reply = call_blocking(
        adapter,
        wire::FileOperation::TrackedPaths {
            prefix: None,
            limit: wire::MAX_PATHS,
        },
    )
    .await?;
    match reply {
        wire::FileReply::Paths { paths } => Ok(paths.len() as u64),
        _ => Err(RemoteSourceError::TransportFailure {
            message: "unexpected reply to TrackedPaths".into(),
        }),
    }
}

/// DESIGN-remote-source.md §3.3 — detach the polling task.
#[tauri::command]
pub async fn source_remote_watch_stop(app: AppHandle, token: u64) -> Result<(), RemoteSourceError> {
    use tauri::Manager;
    let state =
        app.try_state::<RemoteWatchState>()
            .ok_or_else(|| RemoteSourceError::TransportFailure {
                message: "RemoteWatchState not registered".into(),
            })?;
    state.remove(token);
    Ok(())
}

// ---- Local helpers (hashing + base64 are small enough to inline) -----------

fn base64_encode(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.encode(bytes)
}

fn base64_decode(s: &str) -> Result<Vec<u8>, RemoteSourceError> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD
        .decode(s)
        .map_err(|e| RemoteSourceError::TransportFailure {
            message: format!("invalid base64: {}", e),
        })
}

static OPERATION_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_operation_id() -> String {
    // A nonce the server echoes back; uniqueness is per-process (NOT a UUID
    // because we only need to distinguish retries within one session). Pre-
    // production: a UUIDv4 is more defensive if hosts ever share one process.
    let n = OPERATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("remote-source-{}", n)
}

// ---- Unit tests ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_file(
        path: &str,
        status: wire::ChangeStatus,
        old_path: Option<&str>,
        mode_changed: bool,
    ) -> wire::ChangedFile {
        wire::ChangedFile {
            path: path.into(),
            status,
            old_path: old_path.map(str::to_string),
            added: Some(2),
            removed: Some(1),
            mode_changed,
        }
    }

    #[test]
    fn wire_changes_classify_like_the_local_diff() {
        let files = publish_files_from_wire(vec![
            wire_file("src/a.rs", wire::ChangeStatus::Modified, None, true),
            wire_file(
                "docs/new.md",
                wire::ChangeStatus::Modified,
                Some("docs/old.md"),
                false,
            ),
            wire_file(
                "art/new.excalidraw",
                wire::ChangeStatus::Modified,
                Some("art/old.excalidraw"),
                false,
            ),
        ]);
        assert_eq!(files[0].badges, vec![PublishFileBadge::Mode]);
        assert_eq!((files[0].added, files[0].removed), (Some(2), Some(1)));
        assert_eq!(files[1].badges, vec![PublishFileBadge::Rename]);
        assert_eq!(files[1].rename_old.as_deref(), Some("docs/old.md"));
        // A toggle-type rename is un-paired into a clean add + delete, uncounted.
        assert_eq!(files[2].path, "art/new.excalidraw");
        assert_eq!(files[2].status, PublishFileStatus::Added);
        assert_eq!(files[3].path, "art/old.excalidraw");
        assert_eq!(files[3].status, PublishFileStatus::Deleted);
        assert_eq!(files[2].added, None);
    }

    #[test]
    fn wire_redline_derives_status_and_binary_like_the_local_redline() {
        let added = redline_from_wire(None, Some(b"new".to_vec()), false);
        assert_eq!(
            (added.status, added.binary),
            (PublishFileStatus::Added, false)
        );
        assert_eq!(added.after, "new");
        let deleted = redline_from_wire(Some(b"old".to_vec()), None, false);
        assert_eq!(deleted.status, PublishFileStatus::Deleted);
        let binary = redline_from_wire(Some(vec![0, 1]), Some(b"text".to_vec()), false);
        assert!(binary.binary && binary.before.is_empty() && binary.after.is_empty());
        let opaque = redline_from_wire(Some(Vec::new()), Some(b"text".to_vec()), true);
        assert!(opaque.binary);
    }

    #[test]
    fn an_old_server_reads_as_an_update_prompt_not_a_raw_decode_error() {
        let err = diff_error(RemoteSourceError::from_string(
            "unsupported request: decode: unknown variant `changes`".into(),
        ));
        match err {
            RemoteSourceError::TransportFailure { message } => {
                assert_eq!(message, DIFF_NEEDS_SERVER_UPDATE)
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    /// The remote diff (the machine's Git CLI, over the wire types) must list
    /// the same files with the same classification and counts as the local
    /// libgit2 diff of the same worktree — one review surface, two engines.
    #[test]
    fn remote_and_local_diff_agree_on_the_same_worktree() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("keep.txt"), "a\nb\nc\n").unwrap();
        std::fs::write(dir.join("gone.txt"), "bye\n").unwrap();
        let doc: String = (0..30).map(|i| format!("doc line {i}\n")).collect();
        std::fs::write(dir.join("old.md"), &doc).unwrap();
        std::fs::write(dir.join("run.sh"), "echo\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        git(dir, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(dir.join("feature.rs"), "fn f() {}\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "committed branch work"]);
        std::fs::write(dir.join("keep.txt"), "a\nB\nc\nd\n").unwrap();
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        git(dir, &["mv", "old.md", "new.md"]);
        std::fs::write(dir.join("new.md"), format!("{doc}tail\n")).unwrap();
        git(dir, &["update-index", "--chmod=+x", "run.sh"]);
        std::fs::set_permissions(
            dir.join("run.sh"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        std::fs::write(dir.join("untracked.txt"), "one\ntwo\nthree").unwrap();
        std::fs::write(dir.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();

        for base_ref in [None, Some("HEAD")] {
            let local =
                crate::commands::git_sync::source_worktree_changeset_impl(dir, base_ref).unwrap();
            let remote =
                match codesense_session_server::testing::worktree_diff_changes(dir, None, base_ref)
                    .unwrap()
                {
                    wire::FileReply::Changes { files, .. } => publish_files_from_wire(files),
                    other => panic!("unexpected {other:?}"),
                };
            let sorted = |mut v: Vec<PublishFile>| {
                v.sort_by(|a, b| a.path.cmp(&b.path));
                v
            };
            assert_eq!(sorted(remote), sorted(local), "base_ref {base_ref:?}");
        }

        for (path, old) in [
            ("keep.txt", None),
            ("new.md", Some("old.md")),
            ("gone.txt", None),
        ] {
            let local =
                crate::commands::git_sync::source_file_redline_impl(dir, path, old, None).unwrap();
            let remote = match codesense_session_server::testing::worktree_diff_redline(
                dir, path, old, None,
            )
            .unwrap()
            {
                wire::FileReply::Redline {
                    before,
                    after,
                    opaque,
                } => redline_from_wire(before, after, opaque),
                other => panic!("unexpected {other:?}"),
            };
            assert_eq!(remote, local, "{path}");
        }
    }

    /// A file untracked after the fork point (`git rm --cached`) is still in the
    /// baseline and still on disk: both engines list it once, the same way.
    #[test]
    fn remote_and_local_diff_agree_on_a_path_untracked_since_the_baseline() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("kept.txt"), "a\n").unwrap();
        std::fs::write(dir.join("dropped.txt"), "same\n").unwrap();
        std::fs::write(dir.join("edited.txt"), "one\n").unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "base"]);
        git(dir, &["rm", "-q", "--cached", "dropped.txt", "edited.txt"]);
        std::fs::write(dir.join("edited.txt"), "one\ntwo\n").unwrap();

        let local = crate::commands::git_sync::source_worktree_changeset_impl(dir, None).unwrap();
        let remote = match codesense_session_server::testing::worktree_diff_changes(dir, None, None)
            .unwrap()
        {
            wire::FileReply::Changes { files, .. } => publish_files_from_wire(files),
            other => panic!("unexpected {other:?}"),
        };
        let sorted = |mut v: Vec<PublishFile>| {
            v.sort_by(|a, b| a.path.cmp(&b.path));
            v
        };
        assert_eq!(sorted(remote), sorted(local));
    }

    /// The baseline rule's ladder (`diff.rs` module docs) is the local one:
    /// with no preparation commit in play, both engines pick the same fork
    /// point across every history shape the ladder has to read.
    #[test]
    fn remote_and_local_diff_pick_the_same_baseline_without_a_preparation_commit() {
        type Setup = fn(&std::path::Path);
        fn commit_file(dir: &std::path::Path, file: &str) {
            std::fs::write(dir.join(file), format!("{file}\n")).unwrap();
            git(dir, &["add", "-A"]);
            git(dir, &["commit", "-q", "-m", file]);
        }
        /// `upstream.txt` lands on `main` and is published as `origin/main`.
        fn upstream_moves(dir: &std::path::Path) {
            git(dir, &["checkout", "-q", "main"]);
            commit_file(dir, "upstream.txt");
            git(dir, &["update-ref", "refs/remotes/origin/main", "main"]);
            git(dir, &["checkout", "-q", "feature"]);
        }
        let cases: &[(&str, Setup, Option<&str>)] = &[
            (
                "origin/main moved on, not merged",
                |dir| {
                    commit_file(dir, "mine.txt");
                    upstream_moves(dir);
                },
                None,
            ),
            (
                "origin/main merged in",
                |dir| {
                    commit_file(dir, "mine.txt");
                    upstream_moves(dir);
                    git(dir, &["merge", "-q", "--no-edit", "origin/main"]);
                },
                None,
            ),
            (
                "rebased onto origin/main",
                |dir| {
                    commit_file(dir, "mine.txt");
                    upstream_moves(dir);
                    git(dir, &["rebase", "-q", "origin/main"]);
                },
                None,
            ),
            (
                "fast-forwarded onto origin/main",
                |dir| {
                    upstream_moves(dir);
                    git(dir, &["merge", "-q", "--ff-only", "origin/main"]);
                },
                None,
            ),
            (
                "no main: a tracked develop merged in",
                |dir| {
                    commit_file(dir, "mine.txt");
                    git(dir, &["checkout", "-q", "main"]);
                    commit_file(dir, "upstream.txt");
                    git(dir, &["update-ref", "refs/remotes/origin/develop", "main"]);
                    git(dir, &["checkout", "-q", "feature"]);
                    git(dir, &["branch", "-q", "-D", "main"]);
                    git(dir, &["remote", "add", "origin", "."]);
                    git(
                        dir,
                        &["branch", "-q", "--set-upstream-to", "origin/develop"],
                    );
                    git(dir, &["merge", "-q", "--no-edit", "origin/develop"]);
                },
                None,
            ),
            (
                "a named base ref",
                |dir| {
                    commit_file(dir, "mine.txt");
                    git(dir, &["tag", "reviewed"]);
                    commit_file(dir, "later.txt");
                },
                Some("reviewed"),
            ),
        ];
        for (name, setup, base_ref) in cases {
            let temp = tempfile::tempdir().unwrap();
            let dir = temp.path();
            git(dir, &["init", "-q", "-b", "main"]);
            commit_file(dir, "base.txt");
            git(dir, &["checkout", "-q", "-b", "feature"]);
            setup(dir);
            let local =
                crate::commands::git_sync::source_worktree_changeset_impl(dir, *base_ref).unwrap();
            let remote = match codesense_session_server::testing::worktree_diff_changes(
                dir, None, *base_ref,
            )
            .unwrap()
            {
                wire::FileReply::Changes { files, .. } => publish_files_from_wire(files),
                other => panic!("unexpected {other:?}"),
            };
            let paths = |v: &[PublishFile]| {
                let mut p: Vec<_> = v.iter().map(|f| f.path.clone()).collect();
                p.sort();
                p
            };
            assert_eq!(paths(&remote), paths(&local), "case: {name}");
            assert!(
                !paths(&local).contains(&"upstream.txt".to_string()),
                "case {name}: upstream work is not the branch's"
            );
        }
    }

    #[test]
    fn parse_hex_hash_roundtrips() {
        let bytes = [
            0u8, 1, 2, 3, 0xff, 0xee, 0xdd, 0xcc, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let hex = hex_encode(&bytes);
        assert_eq!(hex.len(), 64);
        assert_eq!(parse_hex_hash(&hex).unwrap(), bytes);
    }

    #[test]
    fn parse_hex_hash_rejects_wrong_length() {
        assert!(parse_hex_hash("abc").is_err());
    }

    #[test]
    fn parse_hex_hash_rejects_non_hex() {
        assert!(parse_hex_hash(&"g".repeat(64)).is_err());
    }

    #[test]
    fn remote_file_observation_from_absent_is_not_found() {
        assert!(matches!(
            RemoteFileObservation::from_wire(wire::Observation::Absent),
            Err(RemoteSourceError::NotFound)
        ));
    }

    #[test]
    fn remote_file_observation_from_directory_is_transport_failure() {
        let err = RemoteFileObservation::from_wire(wire::Observation::Directory);
        assert!(matches!(
            err,
            Err(RemoteSourceError::TransportFailure { .. })
        ));
    }

    #[test]
    fn from_string_maps_retired_to_disconnected() {
        let err = RemoteSourceError::from_string("original connection retired".into());
        assert!(matches!(err, RemoteSourceError::Disconnected));
    }

    /// A slow diff is a retryable failure (the watcher falls back to the
    /// tracked listing), never "no longer have access".
    #[test]
    fn a_remote_git_timeout_is_a_transport_failure_not_unauthorized() {
        let err =
            RemoteSourceError::from_string("remote Git took too long to answer; retry".into());
        assert!(matches!(err, RemoteSourceError::TransportFailure { .. }));
        let err = RemoteSourceError::from_string("remote workspace authority retired".into());
        assert!(matches!(err, RemoteSourceError::Unauthorized));
    }

    #[test]
    fn from_string_maps_authority_to_unauthorized() {
        // DESIGN-remote-phase-2.md M-4 — specific terms win over the broader
        // "retired/disconnected" bucket now, so "authority retired" classifies
        // as Unauthorized (its real meaning) not Disconnected.
        let err = RemoteSourceError::from_string("private remote authority retired".into());
        assert!(matches!(err, RemoteSourceError::Unauthorized));
    }

    #[test]
    fn from_string_prefers_permission_over_disconnected() {
        let err = RemoteSourceError::from_string("permission denied — unavailable".into());
        assert!(matches!(err, RemoteSourceError::Unauthorized));
    }

    #[test]
    fn entries_from_listing_reroots_under_the_prefix_and_keeps_sizes() {
        let entries = vec![
            wire::ListEntry {
                name: "lib".into(),
                is_dir: true,
                size: None,
            },
            wire::ListEntry {
                name: "main.rs".into(),
                is_dir: false,
                size: Some(42),
            },
        ];
        let rooted = entries_from_listing(Some("src/"), entries.clone());
        assert_eq!(rooted[0].path, "src/lib");
        assert!(rooted[0].is_dir && rooted[0].extension.is_none());
        assert_eq!(rooted[1].path, "src/main.rs");
        assert_eq!(rooted[1].extension.as_deref(), Some("rs"));
        assert_eq!(rooted[1].size, Some(42));
        let at_root = entries_from_listing(None, entries);
        assert_eq!(at_root[1].path, "main.rs");
    }

    #[test]
    fn classify_read_failure_names_what_is_actually_there() {
        let read_error = "remote read changed, unsafe or exceeds limit".to_string();
        let inspected = |observation| Ok(wire::FileReply::Inspected { observation });
        // The shared host string mentions "exceeds", but an absent file is NotFound.
        assert!(matches!(
            classify_read_failure(
                read_error.clone(),
                inspected(wire::Observation::Absent),
                64,
                None
            ),
            RemoteSourceError::NotFound
        ));
        assert!(matches!(
            classify_read_failure(
                read_error.clone(),
                inspected(wire::Observation::Directory),
                64,
                None
            ),
            RemoteSourceError::TransportFailure { .. }
        ));
        assert!(matches!(
            classify_read_failure(
                read_error.clone(),
                Err("remote file exceeds bounded read limit".into()),
                64,
                None
            ),
            RemoteSourceError::BoundsExceeded
        ));
        assert!(matches!(
            classify_read_failure(
                read_error.clone(),
                Err("original connection retired".into()),
                64,
                None
            ),
            RemoteSourceError::Disconnected
        ));
        let regular = |size| wire::Observation::Regular {
            hash: [0; 32],
            mtime_ns: 0,
            size,
            mode: 0o644,
        };
        match classify_read_failure(read_error.clone(), inspected(regular(1)), 64, None) {
            RemoteSourceError::TransportFailure { message } => assert!(message.contains("changed")),
            other => panic!("expected a retryable transport failure, got {other:?}"),
        }
        // Larger than the read's limit: retrying can never succeed.
        assert!(matches!(
            classify_read_failure(read_error.clone(), inspected(regular(65)), 64, None),
            RemoteSourceError::BoundsExceeded
        ));
        // A read pinned to a version the file no longer has: a conflict that
        // carries what is there now, not a retry that can never succeed.
        match classify_read_failure(read_error.clone(), inspected(regular(1)), 64, Some([7; 32])) {
            RemoteSourceError::Conflict { observation } => {
                assert_eq!(observation.hash, hex_encode(&[0; 32]))
            }
            other => panic!("expected a conflict, got {other:?}"),
        }
        // Pinned to the version that is there: the file moved mid-read.
        assert!(matches!(
            classify_read_failure(read_error, inspected(regular(1)), 64, Some([0; 32])),
            RemoteSourceError::TransportFailure { .. }
        ));
    }

    #[test]
    fn reduce_enumeration_at_root_classifies_dirs_and_files() {
        let paths: Vec<String> = vec![
            "README.md".into(),
            "src/lib.rs".into(),
            "src/features/foo.rs".into(),
            "pkg/Cargo.toml".into(),
        ]
        .into_iter()
        .collect();
        let children = reduce_enumeration_to_children(None, &paths);
        let names: Vec<(&str, bool)> = children
            .iter()
            .map(|e| (e.name.as_str(), e.is_dir))
            .collect();
        assert_eq!(
            names,
            vec![("README.md", false), ("pkg", true), ("src", true)]
        );
        // The returned `path` is worktree-relative (the FE re-anchors to the
        // remote root). A leaf's extension is populated like the local lister.
        let readme = children.iter().find(|e| e.name == "README.md").unwrap();
        assert_eq!(readme.path, "README.md");
        assert_eq!(readme.extension.as_deref(), Some("md"));
        let src = children.iter().find(|e| e.name == "src").unwrap();
        assert_eq!(src.path, "src");
        assert_eq!(src.extension, None);
    }

    #[test]
    fn reduce_enumeration_under_prefix_strips_and_lists_once() {
        let paths: Vec<String> = vec![
            "src/lib.rs".into(),
            "src/features/foo.rs".into(),
            "src/features/bar.rs".into(),
            "src/README".into(),
        ]
        .into_iter()
        .collect();
        let children = reduce_enumeration_to_children(Some("src"), &paths);
        let names: Vec<(&str, bool, &str)> = children
            .iter()
            .map(|e| (e.name.as_str(), e.is_dir, e.path.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("README", false, "src/README"),
                ("features", true, "src/features"),
                ("lib.rs", false, "src/lib.rs"),
            ]
        );
    }

    #[test]
    fn reduce_enumeration_handles_trailing_and_leading_slashes_in_prefix() {
        let paths: Vec<String> = vec!["a/b/c.txt".into(), "a/d.txt".into()];
        let children = reduce_enumeration_to_children(Some("/a/"), &paths);
        let names: Vec<&str> = children.iter().map(|e| e.name.as_str()).collect();
        // Prefix is normalized, so both `/a/` and `a` yield the same two children.
        assert_eq!(names, vec!["b", "d.txt"]);
    }

    #[test]
    fn reduce_enumeration_empty_result_when_prefix_has_no_matches() {
        let paths: Vec<String> = vec!["a/b.txt".into()];
        let children = reduce_enumeration_to_children(Some("x"), &paths);
        assert!(children.is_empty());
    }

    /// A tick that falls back from the diff stamp to the tracked listing (or
    /// back) is not a change; a change within one source is.
    #[test]
    fn watch_memory_compares_like_with_like() {
        let mut m = WatchMemory::default();
        assert!(
            m.observe(FingerprintSource::Diff, 1),
            "first fingerprint announces"
        );
        assert!(!m.observe(FingerprintSource::Diff, 1));
        assert!(
            m.observe(FingerprintSource::Tracked, 99),
            "the first fingerprint from the other source announces once"
        );
        assert!(
            !m.observe(FingerprintSource::Diff, 1),
            "back to the same stamp"
        );
        assert!(!m.observe(FingerprintSource::Tracked, 99));
        assert!(
            m.observe(FingerprintSource::Tracked, 100),
            "tracked listing moved"
        );
        assert!(m.observe(FingerprintSource::Diff, 2), "diff stamp moved");
    }

    /// The diff failed from the first tick (the watch ran on the tracked
    /// listing): edits made meanwhile are invisible to the listing, so the
    /// diff's first stamp announces instead of being baselined silently.
    #[test]
    fn the_first_diff_stamp_after_a_tracked_start_announces() {
        let mut m = WatchMemory::default();
        assert!(
            m.observe(FingerprintSource::Tracked, 7),
            "first of the watch"
        );
        assert!(!m.observe(FingerprintSource::Tracked, 7));
        assert!(
            m.observe(FingerprintSource::Diff, 1),
            "diff answers at last"
        );
        assert!(!m.observe(FingerprintSource::Diff, 1));
        assert!(
            !m.observe(FingerprintSource::Tracked, 7),
            "back: like with like"
        );
        // A watch restarted after a panic (both slots empty) stays silent.
        let mut m = WatchMemory {
            first: false,
            ..WatchMemory::default()
        };
        assert!(!m.observe(FingerprintSource::Diff, 1));
    }

    #[test]
    fn a_new_connection_asks_for_the_diff_again() {
        let mut m = WatchMemory::default();
        m.connected(1);
        m.diff_supported = false;
        m.diff_failed();
        m.connected(1);
        assert!(!m.diff_supported, "same connection keeps what it learned");
        m.connected(2);
        assert!(m.diff_supported);
        assert_eq!((m.diff_failures, m.diff_skip), (0, 0));
    }

    /// The watcher's per-tick decisions, driven through a scripted host: a
    /// failing diff falls back to the tracked listing and backs off, is asked
    /// again once the back-off runs out, and an old server's "unsupported
    /// request" switches it to the listing until a new connection.
    #[test]
    fn watch_fingerprint_falls_back_backs_off_and_retries_the_diff() {
        use std::cell::RefCell;
        let diff_answers: RefCell<Vec<Result<wire::FileReply, RemoteSourceError>>> =
            RefCell::new(Vec::new());
        let asked = RefCell::new(Vec::<&'static str>::new());
        let call = |op: wire::FileOperation| match op {
            wire::FileOperation::Changes { .. } => {
                asked.borrow_mut().push("diff");
                diff_answers.borrow_mut().remove(0)
            }
            wire::FileOperation::TrackedPaths { .. } => {
                asked.borrow_mut().push("tracked");
                Ok(wire::FileReply::Paths {
                    paths: vec!["a.txt".into()],
                })
            }
            other => panic!("unexpected {other:?}"),
        };
        let stamp = |s: &str| {
            Ok(wire::FileReply::Changes {
                baseline: "b".into(),
                files: Vec::new(),
                stamp: s.into(),
            })
        };
        let busy = || {
            Err(RemoteSourceError::TransportFailure {
                message: "remote workspace is busy".into(),
            })
        };
        let mut m = WatchMemory::default();
        let tick = |m: &mut WatchMemory,
                    answers: Vec<Result<wire::FileReply, RemoteSourceError>>| {
            *diff_answers.borrow_mut() = answers;
            asked.borrow_mut().clear();
            let fp = watch_fingerprint(&call, m).map(|(source, _)| source);
            (fp, asked.borrow().clone())
        };
        assert_eq!(
            tick(&mut m, vec![stamp("s1")]),
            (Some(FingerprintSource::Diff), vec!["diff"])
        );
        // A failure never goes silent: this tick reads the tracked listing.
        assert_eq!(
            tick(&mut m, vec![busy()]),
            (Some(FingerprintSource::Tracked), vec!["diff", "tracked"])
        );
        // Backed off for one tick, then the diff is asked again and its
        // success clears the failure count.
        assert_eq!(
            tick(&mut m, vec![]),
            (Some(FingerprintSource::Tracked), vec!["tracked"])
        );
        assert_eq!(
            tick(&mut m, vec![stamp("s2")]),
            (Some(FingerprintSource::Diff), vec!["diff"])
        );
        assert_eq!(m.diff_failures, 0);
        // An old server: the listing from now on, without asking again.
        assert_eq!(
            tick(
                &mut m,
                vec![Err(RemoteSourceError::TransportFailure {
                    message: "unsupported request: decode".into(),
                })]
            ),
            (Some(FingerprintSource::Tracked), vec!["diff", "tracked"])
        );
        assert_eq!(
            tick(&mut m, vec![]),
            (Some(FingerprintSource::Tracked), vec!["tracked"])
        );
        // A lost connection says nothing rather than read as a change.
        m.diff_supported = true;
        assert_eq!(
            tick(&mut m, vec![Err(RemoteSourceError::Disconnected)]),
            (None, vec!["diff"])
        );
    }

    #[test]
    fn repeated_diff_failures_back_off_capped() {
        let mut m = WatchMemory::default();
        m.diff_failed();
        assert_eq!(m.diff_skip, 1);
        m.diff_failed();
        assert_eq!(m.diff_skip, 4);
        for _ in 0..10 {
            m.diff_failed();
        }
        assert_eq!(m.diff_skip, 15);
    }
}
