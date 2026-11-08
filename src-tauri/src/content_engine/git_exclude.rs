// SPDX-License-Identifier: MIT
//! Block-scoped `.git/info/exclude` management.
//!
//! Used by SS.5 (materialization-exclusion migration) and SS.4b (sync
//! engine) to register and revoke codesense-managed paths inside a git
//! working tree without touching the tracked `.gitignore`. Path
//! resolution is delegated to `git rev-parse --git-path info/exclude`
//! so submodules, `GIT_DIR` overrides, and worktree resolution are
//! exactly what git itself uses.
//!
//! **Worktree behavior.** `info/exclude` is **shared** across all linked
//! worktrees of a repo — git treats only `HEAD`, `index`, `logs/HEAD`,
//! and a small set of other paths as per-worktree, but `info/exclude`
//! is repo-wide. From inside a linked worktree, `add_entries` therefore
//! mutates the *main* repo's `.git/info/exclude`, which is the correct
//! shape for codesense (every worktree of a source repo sees the same
//! materialized layer set, so a single shared exclude list applied
//! uniformly is what we want). The `worktree_resolves_to_shared_main_exclude`
//! test pins this so a future git change cannot silently produce
//! per-worktree excludes the rest of the engine doesn't expect.
//!
//! Block markers (`# >>> codesense managed` / `# <<< codesense managed`)
//! are intentionally distinct from the legacy `operations/gitignore.rs`
//! markers, so the two managers coexist until SS.12 owns the one-time
//! rewrite from `.gitignore` to `.git/info/exclude`.
//!
//! **Marker takeover trade-off.** The block-start detection is an exact
//! `line.trim() == "# >>> codesense managed"` match — a user (or any
//! other tool) writing that exact line outside our block will be
//! interpreted as the start of our managed section, and following
//! content will be treated as managed entries until the matching end
//! marker is found (or `Error::InvalidExcludeBlock` surfaces). This is
//! the trade-off for stable, human-readable markers; entries supplied
//! via [`add_entries`] are validated to reject any value that would
//! itself produce a marker line, so the only path to corruption is a
//! user editing the file by hand to insert a literal marker.
//!
//! **Content normalization caveat.** `add_entries` / `remove_entries`
//! preserve content outside the managed block on a line-by-line basis,
//! but the round-trip normalizes line endings to `\n` and ensures the
//! file ends with a single `\n` (git's own convention for `info/exclude`).
//! Whitespace inside the managed block is also normalized — blank lines
//! are dropped and trailing whitespace on entries is trimmed. Both
//! behaviors match how git tools themselves rewrite the file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use log::debug;

use super::atomic_io::write_tmp_fsync_rename;
use super::Error;

const BLOCK_START: &str = "# >>> codesense managed";
const BLOCK_END: &str = "# <<< codesense managed";
const WORKSPACE_ROOT: &str = "# root: ";

/// Resolve the absolute path of `info/exclude` for the working tree
/// containing `working_tree`. Delegates to `git rev-parse --git-path
/// info/exclude` so worktrees, submodules, and `GIT_DIR` overrides are
/// honored exactly as git sees them.
pub fn resolve_info_exclude_path(working_tree: &Path) -> Result<PathBuf, Error> {
    if let Some(git) = super::git_worktree::GitWorktree::discover(working_tree)
        .map_err(|e| Error::Io(std::io::Error::other(e)))?
    {
        if git.nested_linked() {
            return Ok(git.exclude());
        }
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(working_tree)
        .arg("rev-parse")
        .arg("--git-path")
        .arg("info/exclude")
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::GitNotFound
            } else {
                Error::Io(e)
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(Error::NotAGitWorkingTree {
            path: working_tree.to_path_buf(),
            stderr,
        });
    }

    let rel = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if rel.is_empty() {
        return Err(Error::NotAGitWorkingTree {
            path: working_tree.to_path_buf(),
            stderr: "git rev-parse returned empty path".into(),
        });
    }

    let candidate = PathBuf::from(&rel);
    let absolute = if candidate.is_absolute() {
        candidate
    } else {
        working_tree.join(candidate)
    };
    Ok(absolute)
}

/// Idempotently add `entries` to the codesense-managed block, creating
/// the file and the block if absent. Entries already present are not
/// duplicated. Content outside the managed block is preserved on a
/// line-by-line basis (see module-level normalization caveat). The
/// write goes through [`write_tmp_fsync_rename`] so the file is never
/// observed half-written.
///
/// # Errors
///
/// Returns [`Error::InvalidEntry`] without touching the file if any
/// entry is empty, contains a newline, or would render as a block-marker
/// line (`# >>> ...` / `# <<< ...`). Returns [`Error::Io`] /
/// [`Error::NotAGitWorkingTree`] / [`Error::GitNotFound`] as documented
/// in [`resolve_info_exclude_path`].
///
/// # No-op
///
/// `add_entries(wt, &[])` returns `Ok(())` without reading or writing
/// the file.
///
/// # Concurrency
///
/// The read-modify-write sequence is not internally locked. Concurrent
/// callers from the same process can lose updates (last-writer-wins on
/// the final rename, even with the unique-tmp-suffix race protection in
/// [`write_tmp_fsync_rename`]). Callers that need serialization should
/// wrap calls in their own mutex; SS.4b / SS.5 own that layer.
/// Validate that every entry would be accepted by [`add_entries`] WITHOUT touching
/// the filesystem — the same per-entry checks `add_entries` runs before it writes.
/// Lets a caller pre-flight an entry it intends to add later, so a durable earlier
/// side effect (e.g. writing content before hiding it) can be gated on the hide
/// being legal FIRST (SLW-WD-3 RD-8: never leave a file materialized-but-not-hidden
/// because the exclude write turned out to be rejected — e.g. a `#`-prefixed name).
pub fn validate_entries(entries: &[&str]) -> Result<(), Error> {
    for entry in entries {
        validate_entry(entry)?;
    }
    Ok(())
}

pub fn add_entries(working_tree: &Path, entries: &[&str]) -> Result<(), Error> {
    if entries.is_empty() {
        return Ok(());
    }
    for entry in entries {
        validate_entry(entry)?;
    }

    let path = resolve_info_exclude_path(working_tree)?;
    let existing = read_file_or_empty(&path)?;

    let (before, current, after) = split_block(&existing)?;
    let mut merged = current.clone();
    let mut added = 0;
    for entry in entries {
        let s = entry.to_string();
        if !merged.iter().any(|e| e == &s) {
            merged.push(s);
            added += 1;
        }
    }
    if added == 0 && block_is_present(&existing) {
        debug!(
            target: "content_engine::git_exclude",
            "no-op add to {}: all entries already present",
            path.display()
        );
        return Ok(());
    }

    let next = render(&before, Some(&merged), &after);
    write_tmp_fsync_rename(&path, next.as_bytes())
}

/// Idempotently remove `entries` from the codesense-managed block.
/// Deletes the block entirely when no entries remain. Returns success
/// when the file or the block is absent.
///
/// # No-op
///
/// `remove_entries(wt, &[])` returns `Ok(())` without reading or writing
/// the file. (Without this guard, the implementation would re-write the
/// file with the same content as before — a benign no-op for content
/// but a real filesystem-event that any attached watcher would observe.)
pub fn remove_entries(working_tree: &Path, entries: &[&str]) -> Result<(), Error> {
    if entries.is_empty() {
        return Ok(());
    }

    let path = resolve_info_exclude_path(working_tree)?;
    let existing = match read_file_or_none(&path)? {
        Some(s) => s,
        None => return Ok(()),
    };

    if !block_is_present(&existing) {
        return Ok(());
    }

    let (before, current, after) = split_block(&existing)?;
    let to_remove: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
    let current_len = current.len();
    let kept: Vec<String> = current
        .into_iter()
        .filter(|e| !to_remove.contains(e))
        .collect();

    // No-op when no entry actually matched — symmetric with `add_entries`'s
    // "all entries already present" short-circuit. Skipping the rewrite
    // avoids a spurious watcher event for a content-equivalent file.
    if kept.len() == current_len {
        debug!(
            target: "content_engine::git_exclude",
            "no-op remove from {}: none of the requested entries were present",
            path.display()
        );
        return Ok(());
    }

    let next = if kept.is_empty() {
        render(&before, None, &after)
    } else {
        render(&before, Some(&kept), &after)
    };
    write_tmp_fsync_rename(&path, next.as_bytes())
}

/// Read the current codesense-managed entries. Returns an empty `Vec`
/// when the file or the block is absent.
pub fn read_managed_entries(working_tree: &Path) -> Result<Vec<String>, Error> {
    let path = resolve_info_exclude_path(working_tree)?;
    let existing = match read_file_or_none(&path)? {
        Some(s) => s,
        None => return Ok(Vec::new()),
    };
    if !block_is_present(&existing) {
        return Ok(Vec::new());
    }
    let (_before, current, _after) = split_block(&existing)?;
    Ok(current)
}

/// Reject entries that would corrupt the managed block when written
/// or be silently ignored by git on read-back.
///
/// - Newlines split entries across lines, breaking the block format.
/// - Carriage returns confuse `split_block`'s line-based parser.
/// - Empty strings can't round-trip (`split_block` drops blank lines).
/// - Whitespace-only entries trim to empty and disappear on read-back,
///   producing a non-idempotent round-trip.
/// - Literal marker text would be re-parsed as our own block markers.
/// - Entries beginning with `#` are git comment lines: git silently
///   ignores them, so the caller would believe a pattern is registered
///   when it isn't.
fn validate_entry(entry: &str) -> Result<(), Error> {
    if entry.is_empty() {
        return Err(Error::InvalidEntry {
            entry: entry.to_string(),
            reason: "empty entries are not permitted",
        });
    }
    if entry.contains('\n') || entry.contains('\r') {
        return Err(Error::InvalidEntry {
            entry: entry.to_string(),
            reason: "entries may not contain newline or carriage-return characters",
        });
    }
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidEntry {
            entry: entry.to_string(),
            reason: "whitespace-only entries are silently dropped on read-back",
        });
    }
    if trimmed == BLOCK_START || trimmed == BLOCK_END {
        return Err(Error::InvalidEntry {
            entry: entry.to_string(),
            reason: "entry collides with the codesense managed block markers",
        });
    }
    if trimmed.starts_with('#') {
        return Err(Error::InvalidEntry {
            entry: entry.to_string(),
            reason:
                "entries beginning with '#' are git comment lines and would be silently ignored",
        });
    }
    Ok(())
}

fn read_file_or_empty(path: &Path) -> Result<String, Error> {
    Ok(read_file_or_none(path)?.unwrap_or_default())
}

fn read_file_or_none(path: &Path) -> Result<Option<String>, Error> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::Io(e)),
    }
}

fn block_is_present(content: &str) -> bool {
    content.lines().any(|l| l.trim() == BLOCK_START)
}

/// `(lines_before_block, entries_inside_block, lines_after_block)` —
/// the structural decomposition `split_block` returns.
type SplitBlock = (Vec<String>, Vec<String>, Vec<String>);

/// Split `content` into `(before, entries, after)` around the managed
/// block. Lines outside the block are preserved verbatim. Entries
/// returned are trimmed of trailing whitespace; blank lines and comment
/// lines inside the block are dropped on read but the block markers
/// themselves are reconstructed by [`render`].
fn split_block(content: &str) -> Result<SplitBlock, Error> {
    let mut before = Vec::new();
    let mut entries = Vec::new();
    let mut after = Vec::new();
    let mut state = State::Before;

    for line in content.lines() {
        match state {
            State::Before => {
                if line.trim() == BLOCK_START {
                    state = State::Inside;
                } else if line.trim() == BLOCK_END {
                    return Err(Error::InvalidExcludeBlock {
                        reason: "stray end marker before start".into(),
                    });
                } else {
                    before.push(line.to_string());
                }
            }
            State::Inside => {
                if line.trim() == BLOCK_END {
                    state = State::After;
                } else if line.trim() == BLOCK_START {
                    return Err(Error::InvalidExcludeBlock {
                        reason: "nested start marker".into(),
                    });
                } else {
                    let trimmed = line.trim_end();
                    if !trimmed.is_empty() {
                        entries.push(trimmed.to_string());
                    }
                }
            }
            State::After => {
                if line.trim() == BLOCK_START || line.trim() == BLOCK_END {
                    return Err(Error::InvalidExcludeBlock {
                        reason: "second block marker after first block".into(),
                    });
                }
                after.push(line.to_string());
            }
        }
    }

    if matches!(state, State::Inside) {
        return Err(Error::InvalidExcludeBlock {
            reason: "start marker without matching end".into(),
        });
    }

    Ok((before, entries, after))
}

enum State {
    Before,
    Inside,
    After,
}

fn render(before: &[String], entries: Option<&[String]>, after: &[String]) -> String {
    let mut out = String::new();
    for line in before {
        out.push_str(line);
        out.push('\n');
    }
    if let Some(entries) = entries {
        out.push_str(BLOCK_START);
        out.push('\n');
        for entry in entries {
            out.push_str(entry);
            out.push('\n');
        }
        out.push_str(BLOCK_END);
        out.push('\n');
    }
    for line in after {
        out.push_str(line);
        out.push('\n');
    }
    out
}

// ============================================================================
// SW-XS-7 (SPEC §11.4): one marker block per workspace.
// ----------------------------------------------------------------------------
// `info/exclude` is shared by every linked worktree of a repo, so each
// workspace owns its own block, `# >>> codesense managed workspace <id>` …
// `# <<< codesense managed workspace <id>` — the same shape the terminal
// server writes on a machine. Every write holds a per-repo lock (an in-process
// mutex plus `flock` on a sibling lock file, so another process is excluded
// too) across read → replace → rename, so two workspaces of one repo never
// lose each other's block. Bytes outside this workspace's block are preserved.
// ============================================================================

/// This workspace's block markers.
fn workspace_markers(workspace_id: &str) -> (String, String) {
    (
        format!("{BLOCK_START} workspace {workspace_id}"),
        format!("{BLOCK_END} workspace {workspace_id}"),
    )
}

/// A workspace-relative file path as an anchored exclude pattern (glob
/// characters escaped, as the terminal server writes them).
pub fn workspace_exclude_pattern(path: &str) -> String {
    let mut pattern = String::with_capacity(path.len() + 2);
    pattern.push('/');
    for character in path.chars() {
        if matches!(character, '\\' | '*' | '?' | '[' | ']') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    if pattern.ends_with(' ') {
        let trimmed = pattern.trim_end_matches(' ').len();
        let spaces = pattern.len() - trimmed;
        pattern.truncate(trimmed);
        for _ in 0..spaces {
            pattern.push_str("\\ ");
        }
    }
    pattern
}

fn pattern_path(line: &str) -> Option<String> {
    let body = line.strip_prefix('/')?;
    let mut path = String::with_capacity(body.len());
    let mut escaped = false;
    for character in body.chars() {
        if escaped {
            path.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            path.push(character);
        }
    }
    Some(path)
}

/// Byte ranges let a single scan validate all workspace markers without
/// normalizing the surviving blocks, CRLFs, or an unterminated last line.
struct WorkspaceBlock<'a> {
    id: &'a str,
    start: usize,
    end: usize,
    root: Option<&'a Path>,
}

fn workspace_root(line: &str) -> Result<Option<&Path>, Error> {
    let Some(path) = line.strip_prefix(WORKSPACE_ROOT) else {
        return Ok(None);
    };
    let root = Path::new(path);
    if !root.is_absolute() {
        return Err(Error::InvalidExcludeBlock {
            reason: "workspace root metadata must be absolute".into(),
        });
    }
    Ok(Some(root))
}

type WorkspaceSplit<'a> = (&'a str, Option<Vec<String>>, &'a str, Option<&'a Path>);

/// Split only this workspace's block, preserving all surrounding bytes.
/// Foreign markers are outside this block's ownership and do not prevent a write.
fn split_workspace_block<'a>(
    text: &'a str,
    workspace_id: &str,
) -> Result<WorkspaceSplit<'a>, Error> {
    let (start_marker, end_marker) = workspace_markers(workspace_id);
    let mut start = None;
    let mut end = None;
    let mut offset = 0;
    let invalid = || Error::InvalidExcludeBlock {
        reason: format!("workspace {workspace_id} exclude markers are ambiguous"),
    };
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end();
        if line == start_marker
            && Option::replace(&mut start, (offset, offset + raw.len())).is_some()
        {
            return Err(invalid());
        }
        if line == end_marker && Option::replace(&mut end, (offset, offset + raw.len())).is_some() {
            return Err(invalid());
        }
        offset += raw.len();
    }
    match (start, end) {
        (None, None) => Ok((text, None, "", None)),
        (Some((start, body)), Some((end, after))) if start < end => {
            let mut paths = Vec::new();
            let mut root = None;
            for line in text[body..end].lines() {
                if let Some(value) = workspace_root(line)? {
                    if Option::replace(&mut root, value).is_some() {
                        return Err(invalid());
                    }
                } else {
                    let line = line.trim_end();
                    if !line.is_empty() {
                        paths.push(pattern_path(line).ok_or_else(|| {
                            Error::InvalidExcludeBlock {
                                reason: format!("workspace {workspace_id} block holds {line:?}"),
                            }
                        })?);
                    }
                }
            }
            Ok((&text[..start], Some(paths), &text[after..], root))
        }
        _ => Err(invalid()),
    }
}

/// Validate every workspace marker before collecting any historical block.
/// One scan and byte ranges keep collection linear even with large histories.
fn workspace_blocks(text: &str) -> Result<Vec<WorkspaceBlock<'_>>, Error> {
    let mut blocks = Vec::new();
    let mut seen = HashSet::new();
    let mut open: Option<(&str, usize, Option<&Path>)> = None;
    let mut offset = 0;
    let invalid = || Error::InvalidExcludeBlock {
        reason: "foreign workspace exclude markers are ambiguous".into(),
    };
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end();
        let start = line
            .strip_prefix(BLOCK_START)
            .and_then(|s| s.strip_prefix(" workspace "));
        let end = line
            .strip_prefix(BLOCK_END)
            .and_then(|s| s.strip_prefix(" workspace "));
        if let Some(id) = start {
            if open.is_some() || str::is_empty(id) || !seen.insert(id) {
                return Err(invalid());
            }
            open = Some((id, offset, None));
        } else if let Some(id) = end {
            let Some((opened, start, root)) = open.take() else {
                return Err(invalid());
            };
            if opened != id {
                return Err(invalid());
            }
            blocks.push(WorkspaceBlock {
                id: opened,
                start,
                end: offset + raw.len(),
                root,
            });
        } else if let Some((_, _, root)) = open.as_mut() {
            if let Some(value) = workspace_root(raw.trim_end_matches(['\r', '\n']))? {
                if Option::replace(root, value).is_some() {
                    return Err(invalid());
                }
            }
        }
        offset += raw.len();
    }
    if open.is_some() {
        return Err(invalid());
    }
    Ok(blocks)
}

fn repo_lock(exclude: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(exclude.to_path_buf())
        .or_default()
        .clone()
}

/// Hold the per-repo exclude lock (in-process and across processes) for `f`.
fn with_repo_lock<T>(exclude: &Path, f: impl FnOnce() -> Result<T, Error>) -> Result<T, Error> {
    let lock = repo_lock(exclude);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let parent = exclude.parent().ok_or_else(|| Error::InvalidExcludeBlock {
        reason: "exclude path has no parent".into(),
    })?;
    std::fs::create_dir_all(parent)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(parent.join("codesense-exclude.lock"))?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // SAFETY: a valid open descriptor; the lock is released on close.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
    }
    let result = f();
    drop(file);
    result
}

/// The paths this workspace's block lists (`None` when it has no block).
pub fn workspace_block(
    working_tree: &Path,
    workspace_id: &str,
) -> Result<Option<Vec<String>>, Error> {
    let exclude = resolve_info_exclude_path(working_tree)?;
    let text = match std::fs::read_to_string(&exclude) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    };
    Ok(split_workspace_block(&text, workspace_id)?.1)
}

/// Replace this workspace's block with `paths` (sorted, deduplicated; an
/// empty list removes the block), under the per-repo lock. Returns the
/// resolved `info/exclude` path. A path holding a newline is refused.
pub fn replace_workspace_block(
    working_tree: &Path,
    workspace_id: &str,
    paths: &[String],
) -> Result<PathBuf, Error> {
    replace_workspace_block_if(working_tree, workspace_id, None, paths)?;
    resolve_info_exclude_path(working_tree)
}

/// [`replace_workspace_block`] as a compare-and-swap: written only while the
/// block (read under the per-repo lock) still lists exactly `expected` (no
/// block = none); `Ok(false)` when it changed. `None` replaces it whatever it
/// lists. Broken markers are [`Error::InvalidExcludeBlock`] either way.
pub fn replace_workspace_block_if(
    working_tree: &Path,
    workspace_id: &str,
    expected: Option<&[String]>,
    paths: &[String],
) -> Result<bool, Error> {
    replace_workspace_block_if_with_gc(working_tree, workspace_id, expected, paths, None, || None)
}

/// Replace and collect stale local blocks in the same locked transaction.
/// The provider is called only after acquiring the repo lock; `None` means
/// the live worktree set is uncertain and leaves every historical block intact.
pub(crate) fn replace_workspace_block_if_with_gc(
    working_tree: &Path,
    workspace_id: &str,
    expected: Option<&[String]>,
    paths: &[String],
    destination_root: Option<&Path>,
    live_ids: impl FnOnce() -> Option<HashSet<String>>,
) -> Result<bool, Error> {
    if let Some(root) = destination_root {
        if !root.is_absolute() || root.to_string_lossy().contains(['\r', '\n']) {
            return Err(Error::InvalidEntry {
                entry: Path::display(root).to_string(),
                reason: "invalid workspace root",
            });
        }
    }
    if str::is_empty(workspace_id) || workspace_id.contains(['\n', '\r']) {
        return Err(Error::InvalidEntry {
            entry: workspace_id.to_string(),
            reason: "invalid workspace id",
        });
    }
    for path in paths {
        if path.is_empty() || path.contains(['\n', '\r']) {
            return Err(Error::InvalidEntry {
                entry: path.clone(),
                reason: "invalid exclude path",
            });
        }
    }
    let exclude = resolve_info_exclude_path(working_tree)?;
    replace_workspace_block_in_with_gc(
        &exclude,
        workspace_id,
        expected,
        paths,
        destination_root,
        live_ids,
    )
}

/// [`replace_workspace_block_if`] against an already resolved `info/exclude`
/// path: for teardown, after the worktree that resolved it is gone.
pub fn replace_workspace_block_in(
    exclude: &Path,
    workspace_id: &str,
    expected: Option<&[String]>,
    paths: &[String],
) -> Result<bool, Error> {
    // Teardown and per-worktree generated-config writes intentionally do not
    // enumerate or collect another workspace's shared history.
    replace_workspace_block_in_with_gc(exclude, workspace_id, expected, paths, None, || None)
}

// One MiB is an early warning for expensive ignore-matcher construction, well
// before stale history can grow to tens of MiB. Live blocks may still exceed it.
const MAX_SHARED_EXCLUDE_BYTES: usize = 1024 * 1024;

fn replace_workspace_block_in_with_gc(
    exclude: &Path,
    workspace_id: &str,
    expected: Option<&[String]>,
    paths: &[String],
    destination_root: Option<&Path>,
    live_ids: impl FnOnce() -> Option<HashSet<String>>,
) -> Result<bool, Error> {
    let sorted = |paths: &[String]| {
        let mut paths = paths.to_vec();
        paths.sort();
        paths.dedup();
        paths
    };
    let exclude = exclude.to_path_buf();
    with_repo_lock(&exclude, || {
        let live_ids = live_ids();
        let text = match std::fs::read_to_string(&exclude) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(Error::Io(e)),
        };
        let (before, current, after, current_root) = split_workspace_block(&text, workspace_id)?;
        let current = current.map(|c| sorted(&c));
        if let Some(expected) = expected {
            if current.clone().unwrap_or_default() != sorted(expected) {
                return Ok(false);
            }
        }
        let own_range = || {
            current
                .as_ref()
                .map(|_| WorkspaceBlock {
                    id: workspace_id,
                    start: before.len(),
                    end: text.len() - after.len(),
                    root: current_root,
                })
                .into_iter()
                .collect::<Vec<_>>()
        };
        let blocks = if live_ids.is_some() {
            match workspace_blocks(&text) {
                Ok(blocks) => blocks,
                Err(error) => {
                    log::warn!(target: "content_engine::git_exclude",
                        "skipping workspace exclude GC for {}: {error}", Path::display(&exclude));
                    own_range()
                }
            }
        } else {
            own_range()
        };
        let wanted = sorted(paths);
        let unchanged = (current.as_ref() == Some(&wanted)
            || (current.is_none() && Vec::is_empty(&wanted)))
            && destination_root.is_none_or(|root| current_root == Some(root));
        let mut next = String::with_capacity(text.len());
        let mut cursor = 0;
        let append_block = |out: &mut String| {
            let (start, end) = workspace_markers(workspace_id);
            out.push_str(&start);
            out.push('\n');
            if let Some(root) = destination_root {
                out.push_str(WORKSPACE_ROOT);
                out.push_str(&root.to_string_lossy());
                out.push('\n');
            }
            for path in &wanted {
                out.push_str(&workspace_exclude_pattern(path));
                out.push('\n');
            }
            out.push_str(&end);
            out.push('\n');
        };
        let mut stat_warning_emitted = false;
        for block in &blocks {
            let own = block.id == workspace_id;
            let stale = !own
                && block.id.starts_with("local-")
                && live_ids.as_ref().is_some_and(|ids| match block.root {
                    Some(root) => match std::fs::metadata(root) {
                        Ok(_) => false,
                        Err(error) if std::io::Error::kind(&error) == std::io::ErrorKind::NotFound => true,
                        Err(error) => {
                            if !stat_warning_emitted {
                                stat_warning_emitted = true;
                                log::warn!(target: "content_engine::git_exclude",
                                    "couldn't inspect workspace root {} in {}; keeping its block: {error}",
                                    Path::display(root), Path::display(&exclude));
                            }
                            false
                        }
                    },
                    None => !ids.contains(block.id),
                });
            if stale || (own && !unchanged) {
                next.push_str(&text[cursor..block.start]);
                if own && !Vec::is_empty(&wanted) {
                    append_block(&mut next);
                }
                cursor = block.end;
            }
        }
        next.push_str(&text[cursor..]);
        if current.is_none() && !Vec::is_empty(&wanted) {
            if !String::is_empty(&next) && !next.ends_with('\n') {
                next.push('\n');
            }
            append_block(&mut next);
        }
        if next != text {
            write_tmp_fsync_rename(&exclude, next.as_bytes())?;
        }
        if next.len() > MAX_SHARED_EXCLUDE_BYTES {
            let block_count = next
                .lines()
                .filter(|line| {
                    line.trim_end()
                        .strip_prefix(BLOCK_START)
                        .is_some_and(|tail| tail.starts_with(" workspace "))
                })
                .count();
            log::warn!(target: "content_engine::git_exclude",
                "shared exclude {} remains large after workspace write: {} bytes, {} workspace blocks",
                Path::display(&exclude), next.len(), block_count);
        }
        debug!(
            "git_exclude: workspace {workspace_id} block now lists {} path(s) in {}",
            wanted.len(),
            Path::display(&exclude)
        );
        Ok(true)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    fn git_init(dir: &Path) {
        let status = Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(dir)
            .status()
            .expect("git init");
        assert!(status.success());
    }

    #[test]
    fn workspace_gc_provider_runs_under_lock_and_preserves_cas() {
        let dir = TempDir::new().unwrap();
        let exclude = dir.path().join("exclude");
        let history = "# >>> codesense managed workspace local-stale\n/stale\n# <<< codesense managed workspace local-stale\n";
        fs::write(&exclude, history).unwrap();
        let calls = std::cell::Cell::new(0);
        let provider = || {
            assert!(
                repo_lock(&exclude).try_lock().is_err(),
                "live discovery must be locked"
            );
            calls.set(calls.get() + 1);
            Some(HashSet::new())
        };
        assert!(!replace_workspace_block_in_with_gc(
            &exclude,
            "local-own",
            Some(&["unexpected".into()]),
            &[],
            None,
            provider
        )
        .unwrap());
        assert_eq!(fs::read_to_string(&exclude).unwrap(), history);
        assert!(replace_workspace_block_in_with_gc(
            &exclude,
            "local-own",
            Some(&[]),
            &[],
            None,
            provider
        )
        .unwrap());
        assert_eq!(calls.get(), 2);
        assert_eq!(fs::read_to_string(exclude).unwrap(), "");
    }

    #[test]
    fn workspace_gc_size_and_foreign_warning_emission() {
        crate::test_logger::clear("content_engine::git_exclude");
        let dir = TempDir::new().unwrap();
        let exclude = dir.path().join("size-warning");
        let matching = || {
            crate::test_logger::lines("content_engine::git_exclude")
                .iter()
                .filter(|line| line.contains(&exclude.display().to_string()))
                .cloned()
                .collect::<Vec<_>>()
        };
        for size in [
            MAX_SHARED_EXCLUDE_BYTES - 1,
            MAX_SHARED_EXCLUDE_BYTES,
            MAX_SHARED_EXCLUDE_BYTES + 1,
        ] {
            fs::write(&exclude, "x".repeat(size)).unwrap();
            replace_workspace_block_in_with_gc(&exclude, "local-own", None, &[], None, || {
                Some(HashSet::new())
            })
            .unwrap();
            assert_eq!(fs::metadata(&exclude).unwrap().len(), size as u64);
        }
        {
            let logs = matching();
            assert_eq!(
                logs.len(),
                1,
                "one size warning above the bound only: {logs:?}"
            );
            assert!(logs[0].contains(&(MAX_SHARED_EXCLUDE_BYTES + 1).to_string()));
            assert!(logs[0].contains("0 workspace blocks"));
        }
        let foreign = "# >>> codesense managed workspace local-broken\n/path\n";
        fs::write(&exclude, foreign).unwrap();
        replace_workspace_block_in_with_gc(
            &exclude,
            "local-own",
            None,
            &["own".into()],
            None,
            || Some(HashSet::new()),
        )
        .unwrap();
        assert!(fs::read_to_string(&exclude).unwrap().starts_with(foreign));
        {
            let logs = matching();
            assert_eq!(logs.len(), 2);
            assert!(logs[1].contains("skipping workspace exclude GC"));
        }
        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            let parent = dir.path().join("restricted metadata");
            let roots = [parent.join("a"), parent.join("b")];
            let mut protected = String::new();
            for (index, root) in roots.iter().enumerate() {
                fs::create_dir_all(root).unwrap();
                protected.push_str(&format!("# >>> codesense managed workspace local-protected-{index}\n# root: {}\n/path\n# <<< codesense managed workspace local-protected-{index}\n", root.display()));
            }
            let stale = "# >>> codesense managed workspace local-stale\n/path\n# <<< codesense managed workspace local-stale\n";
            fs::write(&exclude, format!("{protected}{stale}")).unwrap();
            let permissions = fs::metadata(&parent).unwrap().permissions();
            fs::set_permissions(&parent, fs::Permissions::from_mode(0)).unwrap();
            let probe = fs::metadata(&roots[0]);
            let result =
                replace_workspace_block_in_with_gc(&exclude, "local-own", None, &[], None, || {
                    Some(HashSet::new())
                });
            fs::set_permissions(&parent, permissions).unwrap();
            assert_eq!(
                probe.unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
            result.unwrap();
            assert_eq!(fs::read_to_string(&exclude).unwrap(), protected);
            {
                let logs = matching();
                assert_eq!(
                    logs.len(),
                    3,
                    "two inaccessible roots emit only one stat warning: {logs:?}"
                );
                assert!(logs[2].contains("couldn't inspect workspace root"));
            }
        }
    }

    /// SW-IS-F9 AC-5 probe (2): size the per-peer cost of `add_entries` on the
    /// fan-out path. The wave passes the FULL manifest every persist; with a block
    /// of ~9,400 entries the membership scan is `O(manifest × block)`. This measures
    /// three shapes so the F9 gate can compare the add-side leg against the store
    /// persist span: (a) re-add the whole manifest (today's fan-out per-peer cost —
    /// short-circuits the WRITE but still scans), (b) add a single already-present
    /// entry (the pure-edit case), (c) add one brand-new entry (new-file case). Run:
    /// `cargo test -p codesense --lib content_engine::git_exclude::tests::measure_add_entries_fanout_cost -- --ignored --nocapture`.
    ///
    /// Each shape is timed ONCE (no warm-up/iteration): the load-bearing figure is (a) — the
    /// ~O(manifest²) full re-add, hundreds of ms, robust to noise and the number the F10
    /// go/no-go rests on. The sub-10ms (b)/(c) figures are single-shot and indicative only.
    #[test]
    #[ignore = "manual performance measurement; run explicitly with --ignored --nocapture"]
    fn measure_add_entries_fanout_cost() {
        use std::time::Instant;
        const MANIFEST: usize = 9_400;
        let dir = TempDir::new().unwrap();
        git_init(dir.path());

        // Seed the managed block with MANIFEST entries (one build write).
        let owned: Vec<String> = (0..MANIFEST)
            .map(|i| format!("skills/skill-{i}.md"))
            .collect();
        let full: Vec<&str> = owned.iter().map(String::as_str).collect();
        add_entries(dir.path(), &full).unwrap();

        // (a) Re-add the whole manifest — the current per-peer fan-out cost. All
        // entries already present → `added == 0` short-circuit (no write), but the
        // O(manifest × block) membership scan still runs.
        let a = Instant::now();
        add_entries(dir.path(), &full).unwrap();
        let full_readd = a.elapsed();

        // (b) Add a single already-present entry — the F9-optimized pure-edit case.
        let one_present = [full[MANIFEST / 2]];
        let b = Instant::now();
        add_entries(dir.path(), &one_present).unwrap();
        let single_present = b.elapsed();

        // (c) Add one brand-new entry — the new-file case (a real write).
        let new_name = "skills/brand-new-file.md".to_string();
        let one_new = [new_name.as_str()];
        let c = Instant::now();
        add_entries(dir.path(), &one_new).unwrap();
        let single_new = c.elapsed();

        println!(
            "F9-PROBE2 git_exclude::add_entries manifest={MANIFEST} \
             full_readd={full_readd:?} single_present={single_present:?} single_new={single_new:?}"
        );
    }

    #[test]
    fn resolve_path_for_main_working_tree() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        let path = resolve_info_exclude_path(dir.path()).unwrap();
        assert!(
            path.ends_with(".git/info/exclude") || path.ends_with(".git/info\\exclude"),
            "unexpected path: {}",
            path.display()
        );
    }

    #[test]
    fn resolve_path_rejects_non_git_directory() {
        let dir = TempDir::new().unwrap();
        let err = resolve_info_exclude_path(dir.path()).expect_err("expected NotAGitWorkingTree");
        assert!(matches!(err, Error::NotAGitWorkingTree { .. }), "{:?}", err);
    }

    #[test]
    fn add_entries_creates_block() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        add_entries(dir.path(), &["foo", "bar"]).unwrap();
        let entries = read_managed_entries(dir.path()).unwrap();
        assert_eq!(entries, vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn add_entries_is_idempotent() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        add_entries(dir.path(), &["foo"]).unwrap();
        add_entries(dir.path(), &["foo"]).unwrap();
        let entries = read_managed_entries(dir.path()).unwrap();
        assert_eq!(entries, vec!["foo".to_string()]);
    }

    /// SW-SY-1 (AC-3): re-adding an already-present entry must write ZERO bytes —
    /// the `.git/info/exclude` reconcile is already only-on-change, so a no-op
    /// forward materialize (`sync_projection_dir_exclude` re-adds the same dir
    /// entry every pass) never rewrites the file. Proven by inode identity: a real
    /// write goes through `write_tmp_fsync_rename`, which renames a fresh tmp over
    /// the dest and thus changes the inode.
    #[test]
    fn add_entries_present_is_zero_write() {
        use std::os::unix::fs::MetadataExt;
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        add_entries(dir.path(), &["foo/"]).unwrap();
        let path = resolve_info_exclude_path(dir.path()).unwrap();
        let ino = fs::metadata(&path).unwrap().ino();
        // Re-add the SAME entry → short-circuits before the write.
        add_entries(dir.path(), &["foo/"]).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().ino(),
            ino,
            "re-adding a present entry must not rewrite .git/info/exclude"
        );
    }

    #[test]
    fn add_entries_with_empty_input_is_noop() {
        // P1: previously wrote a degenerate `# >>>` / `# <<<` block with
        // zero entries; now must early-return without touching disk.
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        add_entries(dir.path(), &[]).unwrap();
        let path = resolve_info_exclude_path(dir.path()).unwrap();
        let raw = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !raw.contains(BLOCK_START),
            "empty add_entries must not create a block: {}",
            raw
        );
        assert!(read_managed_entries(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn add_entries_preserves_existing_user_content() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        let path = resolve_info_exclude_path(dir.path()).unwrap();
        fs::write(&path, "user_pattern\nother_pattern\n").unwrap();

        add_entries(dir.path(), &["managed"]).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("user_pattern"), "{}", raw);
        assert!(raw.contains("other_pattern"), "{}", raw);
        assert!(raw.contains("# >>> codesense managed"), "{}", raw);
        assert!(raw.contains("managed"), "{}", raw);
    }

    #[test]
    fn add_entries_rejects_newline_in_entry() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        let err = add_entries(dir.path(), &["bad\nentry"]).expect_err("expected InvalidEntry");
        assert!(matches!(err, Error::InvalidEntry { .. }), "{:?}", err);
    }

    #[test]
    fn add_entries_rejects_block_marker_as_entry() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        let err = add_entries(dir.path(), &[BLOCK_START]).expect_err("expected InvalidEntry");
        assert!(matches!(err, Error::InvalidEntry { .. }), "{:?}", err);
        let err = add_entries(dir.path(), &[BLOCK_END]).expect_err("expected InvalidEntry");
        assert!(matches!(err, Error::InvalidEntry { .. }), "{:?}", err);
    }

    #[test]
    fn add_entries_rejects_empty_string_entry() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        let err = add_entries(dir.path(), &[""]).expect_err("expected InvalidEntry");
        assert!(matches!(err, Error::InvalidEntry { .. }), "{:?}", err);
    }

    #[test]
    fn add_entries_rejects_whitespace_only_entry() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        let err = add_entries(dir.path(), &["   "]).expect_err("expected InvalidEntry");
        assert!(matches!(err, Error::InvalidEntry { .. }), "{:?}", err);
        // Tabs alone are also whitespace-only.
        let err = add_entries(dir.path(), &["\t\t"]).expect_err("expected InvalidEntry");
        assert!(matches!(err, Error::InvalidEntry { .. }), "{:?}", err);
    }

    #[test]
    fn add_entries_rejects_comment_prefix_entry() {
        // Git treats `#`-prefix lines as comments; we reject them at the
        // boundary so the caller doesn't believe a pattern is registered
        // when git silently ignores it.
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        let err = add_entries(dir.path(), &["# my comment"]).expect_err("expected InvalidEntry");
        assert!(matches!(err, Error::InvalidEntry { .. }), "{:?}", err);
        let err = add_entries(dir.path(), &["#leading-hash"]).expect_err("expected InvalidEntry");
        assert!(matches!(err, Error::InvalidEntry { .. }), "{:?}", err);
        // Whitespace-then-hash should also be rejected — `trim().starts_with('#')`.
        let err = add_entries(dir.path(), &["  # padded"]).expect_err("expected InvalidEntry");
        assert!(matches!(err, Error::InvalidEntry { .. }), "{:?}", err);
    }

    #[test]
    fn add_entries_validates_before_touching_disk() {
        // Mixed-batch input: one valid + one invalid → entire call must
        // fail without leaving any partial block on disk.
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        let err = add_entries(dir.path(), &["good", ""]).expect_err("expected InvalidEntry");
        assert!(matches!(err, Error::InvalidEntry { .. }), "{:?}", err);

        let path = resolve_info_exclude_path(dir.path()).unwrap();
        let raw = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !raw.contains(BLOCK_START),
            "no block should exist after validation failure: {}",
            raw
        );
    }

    #[test]
    fn remove_entries_drops_specified_entries() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        add_entries(dir.path(), &["a", "b", "c"]).unwrap();
        remove_entries(dir.path(), &["b"]).unwrap();
        let entries = read_managed_entries(dir.path()).unwrap();
        assert_eq!(entries, vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn remove_entries_deletes_empty_block() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        add_entries(dir.path(), &["only"]).unwrap();
        remove_entries(dir.path(), &["only"]).unwrap();

        let path = resolve_info_exclude_path(dir.path()).unwrap();
        let raw = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            !raw.contains("# >>> codesense managed"),
            "block should be gone: {}",
            raw
        );
    }

    #[test]
    fn remove_entries_noop_on_missing_block() {
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        remove_entries(dir.path(), &["foo"]).unwrap();
        assert!(read_managed_entries(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn remove_entries_noop_when_no_entry_actually_matched() {
        // P3 (iter-2 follow-up): removing entries that aren't in the
        // block must not rewrite the file (which would emit a spurious
        // watcher event).
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        add_entries(dir.path(), &["a"]).unwrap();
        let path = resolve_info_exclude_path(dir.path()).unwrap();
        let mtime_before = fs::metadata(&path).unwrap().modified().unwrap();
        // Sleep a beat so any rewrite would produce a distinct mtime.
        std::thread::sleep(std::time::Duration::from_millis(20));
        remove_entries(dir.path(), &["nonexistent"]).unwrap();
        let mtime_after = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "file must not be rewritten when no entry was actually removed"
        );
        // Existing entry is intact.
        assert_eq!(
            read_managed_entries(dir.path()).unwrap(),
            vec!["a".to_string()]
        );
    }

    #[test]
    fn remove_entries_with_empty_input_is_noop_even_with_existing_block() {
        // P2: previously the empty-input path fell through and rewrote
        // the file with no entries kept, deleting an existing block.
        let dir = TempDir::new().unwrap();
        git_init(dir.path());
        add_entries(dir.path(), &["keep-me"]).unwrap();
        remove_entries(dir.path(), &[]).unwrap();
        let entries = read_managed_entries(dir.path()).unwrap();
        assert_eq!(entries, vec!["keep-me".to_string()]);
    }

    #[test]
    fn split_block_rejects_unterminated_block() {
        let bad = format!("{}\nfoo\n", BLOCK_START);
        let err = split_block(&bad).expect_err("expected InvalidExcludeBlock");
        assert!(
            matches!(err, Error::InvalidExcludeBlock { .. }),
            "{:?}",
            err
        );
    }

    #[test]
    fn split_block_rejects_stray_end_marker() {
        let bad = format!("{}\n", BLOCK_END);
        let err = split_block(&bad).expect_err("expected InvalidExcludeBlock");
        assert!(
            matches!(err, Error::InvalidExcludeBlock { .. }),
            "{:?}",
            err
        );
    }

    /// SS.5 AC-3 / AC-7 cutover surface: callers pass a worktree path
    /// through to `add_entries` unchanged (no per-worktree path
    /// construction at the consumer layer). The entry lands in the
    /// **main** repo's `.git/info/exclude`, and the worktree's own
    /// `.git` is a gitdir-pointer file (confirming we're inside a linked
    /// worktree, not the main tree). The underlying behavior is
    /// pinned by `worktree_resolves_to_shared_main_exclude` below; this
    /// test asserts the cutover surface explicitly so a future
    /// refactor that introduces a "for_worktree(..)" variant or splits
    /// the resolver per-worktree fails here before silently changing
    /// what gets written.
    #[test]
    fn ss5_cutover_writes_through_worktree_resolve_to_main_exclude() {
        let main_dir = TempDir::new().unwrap();
        git_init(main_dir.path());

        Command::new("git")
            .arg("-C")
            .arg(main_dir.path())
            .args(["commit", "--allow-empty", "-m", "seed", "--quiet"])
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .unwrap();

        let wt_root = TempDir::new().unwrap();
        let wt_path = wt_root.path().join("ss5-wt");
        let status = Command::new("git")
            .arg("-C")
            .arg(main_dir.path())
            .args(["worktree", "add", "--quiet", "-B", "ss5-cutover"])
            .arg(&wt_path)
            .status()
            .unwrap();
        assert!(status.success(), "git worktree add failed");

        // Consumer perspective: pass the worktree path through to
        // add_entries as-is — no special handling, no main-repo lookup.
        add_entries(&wt_path, &["_bmad-output"]).unwrap();

        // The entry lands in the main repo's .git/info/exclude, not a
        // per-worktree path under .git/worktrees/<name>/info/exclude.
        let main_exclude = main_dir.path().join(".git/info/exclude");
        let main_raw = fs::read_to_string(&main_exclude).unwrap();
        assert!(
            main_raw.contains("_bmad-output"),
            "shared exclude should contain entry written from worktree: {}",
            main_raw
        );

        // Confirm we genuinely wrote from inside a linked worktree: the
        // worktree's own `.git` is a file (gitdir pointer), not a
        // directory. If this regressed to a `.git` directory, the test
        // assumptions no longer hold and the AC-3 claim is meaningless.
        let wt_git = wt_path.join(".git");
        let wt_git_meta = fs::symlink_metadata(&wt_git).unwrap();
        assert!(
            wt_git_meta.file_type().is_file(),
            "linked worktree's .git must be a gitdir-pointer file, got: {:?}",
            wt_git_meta.file_type()
        );

        // Resolver from the worktree returns the shared main path.
        let resolved_from_wt = resolve_info_exclude_path(&wt_path).unwrap();
        let canon_main = fs::canonicalize(&main_exclude).unwrap();
        let canon_resolved = fs::canonicalize(&resolved_from_wt).unwrap();
        assert_eq!(canon_main, canon_resolved);
    }

    /// `git rev-parse --git-path info/exclude` resolves to the **shared**
    /// `.git/info/exclude` even from inside a linked worktree (git treats
    /// info/exclude as shared by default; only HEAD/index/logs/HEAD are
    /// per-worktree). This is correct for codesense's use case: every
    /// worktree of a source repo sees the same materialized layer set,
    /// so a single exclude list applied uniformly is what we want. The
    /// test pins the behavior so a future git change (or a misguided
    /// "fix") doesn't silently produce per-worktree excludes the rest of
    /// the engine doesn't expect.
    #[test]
    fn worktree_resolves_to_shared_main_exclude() {
        let main_dir = TempDir::new().unwrap();
        git_init(main_dir.path());

        // git worktree add requires at least one commit on a branch.
        Command::new("git")
            .arg("-C")
            .arg(main_dir.path())
            .args(["commit", "--allow-empty", "-m", "seed", "--quiet"])
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .unwrap();

        let wt_root = TempDir::new().unwrap();
        let wt_path = wt_root.path().join("wt1");
        let status = Command::new("git")
            .arg("-C")
            .arg(main_dir.path())
            .args(["worktree", "add", "--quiet", "-B", "wt1"])
            .arg(&wt_path)
            .status()
            .unwrap();
        assert!(status.success(), "git worktree add failed");

        add_entries(&wt_path, &["from-wt"]).unwrap();

        let main_exclude = main_dir.path().join(".git/info/exclude");
        let main_raw = fs::read_to_string(&main_exclude).unwrap();
        assert!(
            main_raw.contains("from-wt"),
            "shared exclude should contain entry written from worktree: {}",
            main_raw
        );

        // Resolution from the worktree returns the same (shared) path.
        let resolved_from_wt = resolve_info_exclude_path(&wt_path).unwrap();
        let canon_main = fs::canonicalize(&main_exclude).unwrap();
        let canon_resolved = fs::canonicalize(&resolved_from_wt).unwrap();
        assert_eq!(canon_main, canon_resolved);
    }
}
