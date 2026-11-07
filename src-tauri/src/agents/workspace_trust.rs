// SPDX-License-Identifier: MIT
//! SW-WA-F13: pre-trust Claude's and Codex's folder-trust state for
//! codesense-managed workspaces, so a headless `open_workspace` into a brand-
//! new managed worktree doesn't stall on `Do you trust this folder?` with
//! nobody to answer it.
//!
//! Only dirs that pass the managed-surface check
//! (`is_codesense_managed_target`) are ever written — raw or canonical. This
//! writer must never be pointable at an arbitrary user folder, so the check
//! runs here even though callers also run it.
//!
//! ### Claude
//!
//! `.claude.json` holds `projects["<cwd>"].hasTrustDialogAccepted = true` for
//! every folder Claude has shown its trust dialog in. Both the raw path and
//! its `canonicalize()` spelling are written, so a worktree reached through a
//! symlink (macOS `/var/…` → `/private/var/…`) matches whichever spelling
//! Claude ends up with. Every other field of each project entry is preserved
//! byte-for-byte.
//!
//! Writes serialize against our own sidecar `flock` on
//! `<config_dir>/.codesense-claude-trust.lock`, and the RMW is same-dir tmp +
//! `rename`. A live Claude session doesn't honor our lock, so the mtime/len
//! re-check after writing the tmp file and before the rename narrows the
//! lost-update window; a bounded retry (3) handles the common race, and
//! anything beyond that returns `Err` leaving `.claude.json` byte-identical to
//! the state we last read.
//!
//! An unparseable file is NEVER truncated: the function returns `Err` and the
//! file is left byte-identical.
//!
//! ### Codex
//!
//! Folder trust rides per-launch config overrides for the managed raw/canonical paths.
//! The real Codex home is kept, and no unrelated persistent trust is changed.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use indexmap::IndexMap;
use serde_json::value::RawValue;

use crate::agents::launch::AgentLaunch;
use crate::domain::Principal;
use crate::install::agent_account_store::{self, AgentProvider};
use crate::install::worktree_index::{
    is_codesense_managed_target, is_codesense_managed_target_canon,
};

/// Order-preserving JSON object: parsed with `indexmap` so the user's existing
/// top-level, `projects`, and per-entry key order is kept across a write.
/// Switching to this type (instead of `serde_json::Value::Object`, backed by
/// `BTreeMap`) is a dedicated, local choice — enabling `serde_json`'s
/// `preserve_order` feature crate-wide would change every JSON output in the
/// app and is not what SW-WA-F13 wants.
type OrderedObject = IndexMap<String, Box<RawValue>>;

const CLAUDE_CONFIG_FILE: &str = ".claude.json";
const CLAUDE_PROJECTS_KEY: &str = "projects";
const CLAUDE_TRUST_KEY: &str = "hasTrustDialogAccepted";
const CLAUDE_LOCK_FILE: &str = ".codesense-claude-trust.lock";

const MAX_RETRIES: usize = 3;
const LOCK_ATTEMPTS: usize = 50;
const LOCK_DELAY: Duration = Duration::from_millis(20);

/// Where Claude reads its folder-trust state for this launch. Prefers the
/// bound account's own `.claude.json` (account `CLAUDE_CONFIG_DIR` → the
/// account's managed home), falls back to an inherited `CLAUDE_CONFIG_DIR`,
/// then `~/.claude.json`.
pub(crate) fn claude_config_json(account_home: Option<&Path>) -> PathBuf {
    if let Some(home) = account_home {
        return home.join(CLAUDE_CONFIG_FILE);
    }
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.is_empty() {
            return PathBuf::from(dir).join(CLAUDE_CONFIG_FILE);
        }
    }
    dirs::home_dir()
        .map(|h| h.join(CLAUDE_CONFIG_FILE))
        .unwrap_or_else(|| PathBuf::from(CLAUDE_CONFIG_FILE))
}

/// Pre-trust `dir` for Claude by setting `hasTrustDialogAccepted = true` under
/// `projects["<dir>"]` in `config_json`. Writes both the raw path and its
/// `canonicalize()` spelling when they differ.
///
/// Refuses when the resolved target is not a codesense-managed surface. The
/// file is created when absent (`{"projects":{"<dir>":{"hasTrustDialogAccepted":true}}}`),
/// and otherwise merged key-only: every other byte-level value is preserved,
/// including fields on the project entry we touched. **No-op short-circuit:**
/// if the trust key is already `true` for every target, returns `Ok` without
/// taking the lock or rewriting the file — so re-launches don't churn the
/// user's config, and a live Claude session's writes don't race ours when we
/// have nothing to add.
pub(crate) fn pretrust_claude(
    config_json: &Path,
    dir: &Path,
    codesense_home: &Path,
) -> Result<(), String> {
    let targets = managed_targets_for(dir, codesense_home)?;
    with_claude_lock(config_json, |existing| apply_pretrust(existing, &targets))
}

/// Best-effort reverse of [`pretrust_claude`] for the reap path. Removes
/// `hasTrustDialogAccepted` from each project entry [`pretrust_claude`] would
/// have written — raw, `canonicalize()`'d, and (on macOS) the `/private`
/// prefixed/unprefixed sibling spellings — and if the project entry becomes
/// empty, removes the entry itself. Preserves every other field, and the
/// user's existing key order across top-level + `projects` + per-entry.
/// Failures are logged, never surfaced. **No-op short-circuit:** if none of
/// the targets had a `hasTrustDialogAccepted` key, returns without rewriting.
///
/// Deliberately does NOT re-run the managed-surface gate here: the writer
/// gates on the way in, and the reap path by definition runs after the
/// worktree is gone (so `canonicalize()` fails and `is_codesense_managed_target`
/// may return false against a now-missing dir). All this does is remove one
/// specific key from one specific entry — if nothing was there, the function
/// is a no-op; it can never damage unrelated state.
pub(crate) fn untrust_claude(config_json: &Path, dir: &Path) {
    let targets = reap_target_spellings(dir);
    let result = with_claude_lock(config_json, |existing| apply_untrust(existing, &targets));
    if let Err(e) = result {
        log::warn!(
            target: "workspace_trust",
            "untrust_claude {}: {e}",
            config_json.display()
        );
    }
}

/// Compute the new bytes for a pretrust write, or `None` when nothing changed.
/// Preserves order at the root, inside `projects`, and inside each project
/// entry — see [`OrderedObject`]. A non-object entry under `projects[<dir>]`
/// is **skipped** rather than overwritten (we don't clobber a value we don't
/// understand); a corruption breadcrumb is logged.
fn apply_pretrust(existing: Option<&[u8]>, targets: &[PathBuf]) -> Result<Option<Vec<u8>>, String> {
    let mut root = parse_root_or_empty(existing)?;
    let mut projects = pluck_projects(&root)?;
    let mut changed = false;
    for target in targets {
        let key = target.to_string_lossy().into_owned();
        let Some(mut entry) = parse_or_skip_entry(&projects, &key) else {
            continue;
        };
        if entry_has_true_trust(&entry) {
            continue;
        }
        entry.insert(
            CLAUDE_TRUST_KEY.to_string(),
            RawValue::from_string("true".to_string()).map_err(|e| format!("encode `true`: {e}"))?,
        );
        let serialized = serde_json::to_string(&entry)
            .map_err(|e| format!("serialize project entry `{key}`: {e}"))?;
        projects.insert(
            key,
            RawValue::from_string(serialized).map_err(|e| format!("encode entry: {e}"))?,
        );
        changed = true;
    }
    if !changed {
        return Ok(None);
    }
    install_projects(&mut root, projects)?;
    Ok(Some(
        serde_json::to_vec_pretty(&root).map_err(|e| format!("serialize root: {e}"))?,
    ))
}

/// Compute the new bytes for an untrust write, or `None` when nothing changed.
fn apply_untrust(existing: Option<&[u8]>, targets: &[PathBuf]) -> Result<Option<Vec<u8>>, String> {
    let mut root = parse_root_or_empty(existing)?;
    if existing.is_none() {
        return Ok(None);
    }
    let mut projects = pluck_projects(&root)?;
    let mut changed = false;
    for target in targets {
        let key = target.to_string_lossy().into_owned();
        let Some(mut entry) = parse_or_skip_entry(&projects, &key) else {
            continue;
        };
        if entry.shift_remove(CLAUDE_TRUST_KEY).is_none() {
            continue;
        }
        changed = true;
        if entry.is_empty() {
            projects.shift_remove(&key);
        } else {
            let serialized = serde_json::to_string(&entry)
                .map_err(|e| format!("serialize project entry `{key}`: {e}"))?;
            projects.insert(
                key,
                RawValue::from_string(serialized).map_err(|e| format!("encode entry: {e}"))?,
            );
        }
    }
    if !changed {
        return Ok(None);
    }
    install_projects(&mut root, projects)?;
    Ok(Some(
        serde_json::to_vec_pretty(&root).map_err(|e| format!("serialize root: {e}"))?,
    ))
}

fn parse_root_or_empty(existing: Option<&[u8]>) -> Result<OrderedObject, String> {
    match existing {
        None => Ok(OrderedObject::new()),
        Some(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => Ok(OrderedObject::new()),
        Some(bytes) => serde_json::from_slice::<OrderedObject>(bytes)
            .map_err(|e| format!("unparseable config (leaving byte-identical): {e}")),
    }
}

fn pluck_projects(root: &OrderedObject) -> Result<OrderedObject, String> {
    match root.get(CLAUDE_PROJECTS_KEY) {
        None => Ok(OrderedObject::new()),
        Some(raw) => serde_json::from_str::<OrderedObject>(raw.get())
            .map_err(|e| format!("`projects` is not a JSON object: {e}")),
    }
}

/// Overwrite `root[projects]` in place, preserving its original position in
/// the user's top-level key order. [`IndexMap::insert`] replaces the value for
/// an existing key without disturbing its position; a brand-new key goes to
/// the end, which is the correct behavior when `projects` was absent.
fn install_projects(root: &mut OrderedObject, projects: OrderedObject) -> Result<(), String> {
    let serialized =
        serde_json::to_string(&projects).map_err(|e| format!("serialize `projects`: {e}"))?;
    root.insert(
        CLAUDE_PROJECTS_KEY.to_string(),
        RawValue::from_string(serialized).map_err(|e| format!("encode `projects`: {e}"))?,
    );
    Ok(())
}

/// Parse `projects[key]` as an object, returning `None` with a logged
/// breadcrumb when the existing value is NOT an object (e.g., a legacy string
/// or a corrupted boolean). The writer must never silently overwrite a value
/// it doesn't understand; the caller just skips this target.
fn parse_or_skip_entry(projects: &OrderedObject, key: &str) -> Option<OrderedObject> {
    match projects.get(key) {
        None => Some(OrderedObject::new()),
        Some(raw) => match serde_json::from_str::<OrderedObject>(raw.get()) {
            Ok(obj) => Some(obj),
            Err(e) => {
                log::warn!(
                    target: "workspace_trust",
                    "projects[{key}] is not a JSON object; skipping without overwrite ({e})"
                );
                None
            }
        },
    }
}

fn entry_has_true_trust(entry: &OrderedObject) -> bool {
    entry
        .get(CLAUDE_TRUST_KEY)
        .is_some_and(|raw| raw.get() == "true")
}

/// Every path spelling [`pretrust_claude`] could have written for `dir`,
/// resolved WITHOUT requiring the dir to still exist on disk. Covers the raw
/// path, a successful `canonicalize()`, and the macOS `/var`↔`/private/var`
/// sibling — so the reap cleanup catches the canonical spelling we wrote at
/// launch time even after the worktree is gone.
fn reap_target_spellings(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::with_capacity(4);
    let raw = dir.to_path_buf();
    out.push(raw.clone());
    if let Ok(canon) = dir.canonicalize() {
        if !out.iter().any(|p| p == &canon) {
            out.push(canon);
        }
    }
    // macOS: `/var/…` and `/private/var/…` are the same place; pretrust may have
    // written either. Also `/tmp` and `/private/tmp`, and `/etc`. Covered by
    // adding the sibling spelling we didn't already include.
    for spelling in macos_private_siblings(&raw) {
        if !out.iter().any(|p| p == &spelling) {
            out.push(spelling);
        }
    }
    out
}

/// `/private/var`↔`/var` and its siblings are a macOS-only firmlink; on Linux
/// `/private/var` is a plain nonexistent path, so adding its sibling to the
/// candidate set would write (or try to remove) bogus keys that match no real
/// path. Gate the whole family on macOS.
#[cfg(target_os = "macos")]
fn macos_private_siblings(dir: &Path) -> Vec<PathBuf> {
    fn has_boundary(s: &str, prefix: &str) -> bool {
        // Only accept `prefix` followed by `/` or end-of-string, so `/var2`
        // doesn't match `/var` and `/private/vartemp` doesn't match
        // `/private/var`.
        s.len() == prefix.len() || s.as_bytes().get(prefix.len()) == Some(&b'/')
    }

    let s = dir.to_string_lossy();
    let mut out = Vec::new();
    for root in ["/var", "/tmp", "/etc"] {
        let prefixed = format!("/private{root}");
        if s.starts_with(root) && has_boundary(&s, root) && !s.starts_with(&prefixed) {
            out.push(PathBuf::from(format!("{prefixed}{}", &s[root.len()..])));
        }
        if s.starts_with(&prefixed) && has_boundary(&s, &prefixed) {
            out.push(PathBuf::from(format!("{root}{}", &s[prefixed.len()..])));
        }
    }
    out
}

#[cfg(not(target_os = "macos"))]
fn macos_private_siblings(_dir: &Path) -> Vec<PathBuf> {
    Vec::new()
}

/// The one call site for the managed launch spine. Runs the pre-trust writer of
/// `agent`'s runtime, best-effort: a failure logs a breadcrumb and returns —
/// launches are never aborted by trust-write failure.
pub(crate) fn pretrust_managed_launch(
    agent: &AgentLaunch,
    cwd: &Path,
    session_id: &str,
) -> Vec<String> {
    (crate::agents::adapter::desktop(agent.runtime).pretrust)(agent, cwd, session_id)
}

/// Claude's pre-trust: its `.claude.json` in the launch's account home.
pub(crate) fn pretrust_claude_launch(
    agent: &AgentLaunch,
    cwd: &Path,
    _session_id: &str,
) -> Vec<String> {
    let codesense_home = agent.startup_context.home.clone();
    let account_home = account_home(agent);
    let config = claude_config_json(account_home.as_deref());
    if let Err(e) = pretrust_claude(&config, cwd, &codesense_home) {
        log::warn!(
            target: "workspace_trust",
            "pretrust_claude({}): {e}",
            config.display()
        );
    }
    Vec::new()
}

/// Per-launch trust flags for authorized managed workspaces. The adapter owns the format.
pub(crate) fn pretrust_codex_launch(
    agent: &AgentLaunch,
    cwd: &Path,
    _session_id: &str,
) -> Vec<String> {
    managed_trust_flags(agent.runtime, cwd, &agent.startup_context.home)
}

fn managed_trust_flags(
    cli: codesense_pty_core::agent_cli::AgentCli,
    cwd: &Path,
    home: &Path,
) -> Vec<String> {
    match managed_targets_for(cwd, home) {
        Ok(paths) => codesense_pty_core::agent_cli::adapter(cli).workspace_trust(&paths),
        Err(error) => {
            log::warn!(target: "workspace_trust", "workspace trust refused: {error}");
            Vec::new()
        }
    }
}

/// The managed home of the account of `agent`'s runtime the launch resolved against
/// (`None` when no such account is bound or the account is not spawn-eligible).
/// Mirrors what `consume_resolved_launch_binding` would set as the runtime's home
/// variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) on the child, so what is read or
/// written there is the file the agent will actually read.
pub(crate) fn account_home(agent: &AgentLaunch) -> Option<PathBuf> {
    let preview = &agent.account_preview;
    if !preview.spawn_eligible {
        return None;
    }
    if preview
        .provider
        .map(crate::agents::adapter::cli_of_provider)
        != Some(agent.runtime)
    {
        return None;
    }
    let id = preview.account_id.as_ref()?;
    agent_account_store::managed_home(
        &agent.startup_context.home,
        &agent.startup_context.principal,
        id,
    )
    .ok()
}

/// The one call site for the worktree-reap path. Removes the trust key SW-WA-F13
/// wrote for `dir` from the default `.claude.json` and from every attached
/// Claude account's `.claude.json` (when `principal` is known). Best-effort:
/// every write is a `untrust_claude`, whose failures already log + swallow.
pub(crate) fn untrust_managed_launch_dir(
    codesense_home: &Path,
    principal: Option<&Principal>,
    dir: &Path,
) {
    untrust_claude(&claude_config_json(None), dir);
    let Some(principal) = principal else {
        return;
    };
    let profiles = match agent_account_store::list_profiles(codesense_home, principal) {
        Ok(p) => p,
        Err(e) => {
            log::warn!(
                target: "workspace_trust",
                "list_profiles for reap untrust: {e}"
            );
            return;
        }
    };
    for profile in profiles {
        if profile.provider != AgentProvider::Claude || !profile.is_attached() {
            continue;
        }
        let Ok(home) = agent_account_store::managed_home(codesense_home, principal, &profile.id)
        else {
            continue;
        };
        if !home.is_dir() {
            continue;
        }
        untrust_claude(&home.join(CLAUDE_CONFIG_FILE), dir);
    }
}

// --------------- internals ---------------

/// Resolve `dir` to the set of path spellings to write, refusing when
/// the resolved target is NOT a codesense-managed surface.
///
/// Both `dir` as given and `canonicalize(dir)` must independently fail the
/// managed-surface check for a refusal; if EITHER spelling is managed, both
/// are written (so Claude's own canonicalization lands on an entry we wrote).
fn managed_targets_for(dir: &Path, codesense_home: &Path) -> Result<Vec<PathBuf>, String> {
    let raw = dir.to_path_buf();
    let canon = dir.canonicalize().ok();
    let raw_managed = is_codesense_managed_target(&raw, codesense_home);
    let canon_managed = canon
        .as_ref()
        .is_some_and(|p| is_codesense_managed_target_canon(p, codesense_home));
    if !raw_managed && !canon_managed {
        return Err(format!(
            "refused: dir '{}' is not a codesense-managed target",
            dir.display()
        ));
    }
    let mut out = vec![raw];
    if let Some(c) = canon {
        if !out.iter().any(|p| p == &c) {
            out.push(c);
        }
    }
    Ok(out)
}

fn with_claude_lock<F>(config_json: &Path, mutate: F) -> Result<(), String>
where
    F: Fn(Option<&[u8]>) -> Result<Option<Vec<u8>>, String>,
{
    // Fast-path, lock-free no-op check: if the caller already has nothing to
    // write against the current file, we skip the lock entirely so a
    // re-launch of an already-trusted dir never contends with a live Claude
    // session.
    let existing = read_bytes_opt(config_json)?;
    if mutate(existing.as_deref())?.is_none() {
        return Ok(());
    }
    let parent = config_json
        .parent()
        .ok_or_else(|| format!("config path '{}' has no parent", config_json.display()))?;
    fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    let lock_path = parent.join(CLAUDE_LOCK_FILE);
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("open lock {}: {e}", lock_path.display()))?;
    for _ in 0..LOCK_ATTEMPTS {
        if try_flock_ex(&lock_file)? {
            let result = write_locked(config_json, parent, &mutate);
            let _ = unflock(&lock_file);
            return result;
        }
        std::thread::sleep(LOCK_DELAY);
    }
    Err(format!(
        "could not lock {} after {} attempts",
        lock_path.display(),
        LOCK_ATTEMPTS
    ))
}

fn write_locked<F>(config_json: &Path, parent: &Path, mutate: &F) -> Result<(), String>
where
    F: Fn(Option<&[u8]>) -> Result<Option<Vec<u8>>, String>,
{
    let mut last_err: Option<String> = None;
    for _ in 0..MAX_RETRIES {
        let (existing_bytes, stamp) = read_with_stamp(config_json)?;
        let was_absent = existing_bytes.is_none();
        // Under the lock a concurrent CodeSense writer may already have
        // written our key, so re-evaluate the diff; a `None` here is the
        // "already up to date" short-circuit.
        let Some(new_bytes) = mutate(existing_bytes.as_deref())? else {
            return Ok(());
        };
        // Belt-and-suspenders no-op guard: the mutate layer's `changed` bool
        // works at a parsed-value level; this catches any serializer-level
        // determinism gap too.
        if Some(new_bytes.as_slice()) == existing_bytes.as_deref() {
            return Ok(());
        }
        let staged = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
        if let Err(e) = fs::write(&staged, &new_bytes) {
            // Partial write (ENOSPC, EIO, etc.) can leave a byte-truncated
            // file at `staged`; drop it before surfacing so we don't accrete
            // orphan tmp files across retries / crashes.
            let _ = fs::remove_file(&staged);
            return Err(format!("write {}: {e}", staged.display()));
        }
        // Owner-only on the staged tmp — `.claude.json` holds OAuth tokens, so
        // CREATING it must not drop a world-readable file where none existed.
        // On an EXISTING file whose mode was 0644, this tightens to 0600 (and
        // the PR body calls that out explicitly).
        set_owner_only(&staged);
        let after = stamp_of(config_json);
        if after != stamp {
            // A concurrent writer touched the file between our read and the
            // rename; drop the staged copy and retry.
            let _ = fs::remove_file(&staged);
            last_err = Some(format!(
                "concurrent writer touched {} between read and rename",
                config_json.display()
            ));
            continue;
        }
        // Content-hash secondary check: `stamp` may hide a same-second,
        // same-length concurrent write on filesystems with 1s mtime
        // granularity (HFS+, FAT); re-read and compare bytes before the rename
        // to catch what mtime alone can't.
        if !was_absent {
            let reread = fs::read(config_json);
            if let Ok(reread_bytes) = reread {
                if Some(reread_bytes.as_slice()) != existing_bytes.as_deref() {
                    let _ = fs::remove_file(&staged);
                    last_err = Some(format!(
                        "concurrent writer touched {} within the mtime window",
                        config_json.display()
                    ));
                    continue;
                }
            }
        }
        fs::rename(&staged, config_json).map_err(|e| {
            let _ = fs::remove_file(&staged);
            format!(
                "rename {} → {}: {e}",
                staged.display(),
                config_json.display()
            )
        })?;
        return Ok(());
    }
    Err(last_err.unwrap_or_else(|| {
        format!(
            "could not pre-trust {} after {} retries",
            config_json.display(),
            MAX_RETRIES
        )
    }))
}

/// Read the file's bytes, returning `None` on NotFound. Used by the fast-path
/// lock-free no-op check in [`with_claude_lock`].
fn read_bytes_opt(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

#[cfg(unix)]
fn set_owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) {}

type FileStamp = (SystemTime, u64);
type StampedBytes = (Option<Vec<u8>>, Option<FileStamp>);

fn read_with_stamp(path: &Path) -> Result<StampedBytes, String> {
    match fs::metadata(path) {
        Ok(m) => {
            let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
            let modified = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            Ok((Some(bytes), Some((modified, m.len()))))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((None, None)),
        Err(e) => Err(format!("stat {}: {e}", path.display())),
    }
}

fn stamp_of(path: &Path) -> Option<FileStamp> {
    let m = fs::metadata(path).ok()?;
    let modified = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    Some((modified, m.len()))
}

#[cfg(unix)]
fn try_flock_ex(file: &File) -> Result<bool, String> {
    use std::os::fd::AsRawFd;
    // SAFETY: `file` owns a live descriptor for the duration of this call;
    // `flock` neither retains the pointer nor accesses Rust-managed memory.
    let r = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if r == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    // On every unix we ship on, `EWOULDBLOCK == EAGAIN`; one arm is enough.
    if matches!(err.raw_os_error(), Some(e) if e == libc::EWOULDBLOCK || e == libc::EAGAIN) {
        Ok(false)
    } else {
        Err(format!("flock LOCK_EX: {err}"))
    }
}

#[cfg(not(unix))]
fn try_flock_ex(_file: &File) -> Result<bool, String> {
    Ok(true)
}

#[cfg(unix)]
fn unflock(file: &File) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    // SAFETY: `file` owns the descriptor our LOCK_EX acquired; unlocking is
    // the normal counterpart.
    let r = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if r == 0 {
        Ok(())
    } else {
        Err(format!(
            "flock LOCK_UN: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(unix))]
fn unflock(_file: &File) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::fs;

    /// A codesense-managed dir under `users/`: the `(b)` prefix path in
    /// `is_codesense_managed_target` — pure, no FS scan.
    fn managed_dir_under(cs_home: &Path, name: &str) -> PathBuf {
        let p = cs_home.join("users").join("1234567").join(name);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn ac3_pretrust_claude_preserves_unrelated_toplevel_keys_and_entry_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let cs_home = tmp.path().join("codesense_home");
        fs::create_dir_all(&cs_home).unwrap();
        let dir = managed_dir_under(&cs_home, "worktree-A");

        let cfg = tmp.path().join(".claude.json");
        let before = json!({
            "feedbackSurvey": {"shown": true, "when": "yesterday"},
            "theme": "dark",
            "projects": {
                dir.to_string_lossy(): {
                    "allowedTools": ["bash", "edit"],
                    "mcpServers": {"foo": {"command": "bar"}},
                }
            }
        });
        fs::write(&cfg, serde_json::to_vec_pretty(&before).unwrap()).unwrap();

        pretrust_claude(&cfg, &dir, &cs_home).unwrap();

        let after: Value = serde_json::from_slice(&fs::read(&cfg).unwrap()).unwrap();
        // Top-level unrelated keys survive.
        assert_eq!(after["feedbackSurvey"]["shown"], json!(true));
        assert_eq!(after["feedbackSurvey"]["when"], "yesterday");
        assert_eq!(after["theme"], "dark");
        // Project entry's other fields survive, trust key is added true.
        let entry = &after["projects"][dir.to_string_lossy().as_ref()];
        assert_eq!(entry["allowedTools"], json!(["bash", "edit"]));
        assert_eq!(entry["mcpServers"]["foo"]["command"], "bar");
        assert_eq!(entry[CLAUDE_TRUST_KEY], json!(true));
    }

    #[test]
    fn ac3_missing_file_is_created_with_just_the_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let cs_home = tmp.path().join("codesense_home");
        fs::create_dir_all(&cs_home).unwrap();
        let dir = managed_dir_under(&cs_home, "worktree-B");
        let cfg = tmp.path().join("fresh.json");
        assert!(!cfg.exists());

        pretrust_claude(&cfg, &dir, &cs_home).unwrap();

        let after: Value = serde_json::from_slice(&fs::read(&cfg).unwrap()).unwrap();
        assert_eq!(
            after["projects"][dir.to_string_lossy().as_ref()][CLAUDE_TRUST_KEY],
            json!(true)
        );
        // Only `projects` at the root — no stray keys.
        assert_eq!(after.as_object().unwrap().len(), 1);
    }

    #[test]
    fn ac3_unparseable_file_is_left_byte_identical_and_errs() {
        let tmp = tempfile::tempdir().unwrap();
        let cs_home = tmp.path().join("codesense_home");
        fs::create_dir_all(&cs_home).unwrap();
        let dir = managed_dir_under(&cs_home, "worktree-C");
        let cfg = tmp.path().join(".claude.json");
        let garbage = b"this is not {json".to_vec();
        fs::write(&cfg, &garbage).unwrap();

        let err = pretrust_claude(&cfg, &dir, &cs_home).unwrap_err();
        assert!(err.contains("unparseable"), "got: {err}");
        // File is byte-identical.
        assert_eq!(fs::read(&cfg).unwrap(), garbage);
    }

    #[test]
    fn ac4_right_file_account_home() {
        let tmp = tempfile::tempdir().unwrap();
        let path = claude_config_json(Some(tmp.path()));
        assert_eq!(path, tmp.path().join(".claude.json"));
    }

    #[test]
    fn ac4_right_file_env_claude_config_dir() {
        // Serialize env mutation against other tests that touch env.
        let _lock = env_mutex().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        with_env(
            &[("CLAUDE_CONFIG_DIR", Some(tmp.path().to_str().unwrap()))],
            || {
                let path = claude_config_json(None);
                assert_eq!(path, tmp.path().join(".claude.json"));
            },
        );
    }

    #[test]
    fn ac4_right_file_home_fallback() {
        let _lock = env_mutex().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        with_env(
            &[
                ("CLAUDE_CONFIG_DIR", None),
                ("HOME", Some(tmp.path().to_str().unwrap())),
            ],
            || {
                let path = claude_config_json(None);
                assert_eq!(path, tmp.path().join(".claude.json"));
            },
        );
    }

    #[cfg(unix)]
    #[test]
    fn ac5_both_spellings_when_symlinked() {
        let tmp = tempfile::tempdir().unwrap();
        let cs_home = tmp.path().join("codesense_home");
        fs::create_dir_all(&cs_home).unwrap();
        let real = managed_dir_under(&cs_home, "real-worktree");
        let link = tmp.path().join("link-to-worktree");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let cfg = tmp.path().join(".claude.json");
        pretrust_claude(&cfg, &link, &cs_home).unwrap();

        let after: Value = serde_json::from_slice(&fs::read(&cfg).unwrap()).unwrap();
        let projects = after["projects"].as_object().unwrap();
        let link_key = link.to_string_lossy().into_owned();
        let real_key = real.canonicalize().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            projects.get(&link_key).unwrap()[CLAUDE_TRUST_KEY],
            json!(true)
        );
        assert_eq!(
            projects.get(&real_key).unwrap()[CLAUDE_TRUST_KEY],
            json!(true)
        );
    }

    #[test]
    fn ac6_concurrent_writers_dont_lose_entries_or_tear_json() {
        use std::sync::Arc;
        use std::thread;

        let tmp = tempfile::tempdir().unwrap();
        let cs_home = tmp.path().join("codesense_home");
        fs::create_dir_all(&cs_home).unwrap();
        let cfg = Arc::new(tmp.path().join(".claude.json"));
        // Seed with an unrelated key an "unrelated writer" will overwrite below.
        fs::write(cfg.as_path(), br#"{"unrelated": "seed"}"#).unwrap();

        let n = 8usize;
        let dirs: Vec<PathBuf> = (0..n)
            .map(|i| managed_dir_under(&cs_home, &format!("wt-{i}")))
            .collect();
        let dirs = Arc::new(dirs);
        let cs_home = Arc::new(cs_home);

        let mut handles = Vec::new();
        for i in 0..n {
            let cfg = Arc::clone(&cfg);
            let dirs = Arc::clone(&dirs);
            let cs_home = Arc::clone(&cs_home);
            handles.push(thread::spawn(move || {
                pretrust_claude(&cfg, &dirs[i], &cs_home).unwrap();
            }));
        }

        // Unrelated-writer thread rewrites a top-level key through the SAME lock,
        // so the invariant "no torn JSON" holds while we also verify the
        // unrelated key ends up at the last value it wrote.
        let unrelated_values = ["one", "two", "three", "four", "five"];
        let cfg_u = Arc::clone(&cfg);
        let cs_home_u = Arc::clone(&cs_home);
        let dirs_u = Arc::clone(&dirs);
        let unrelated = thread::spawn(move || {
            for v in unrelated_values {
                // Reuse `pretrust_claude` plumbing by routing through the shared
                // `with_claude_lock` seam via a custom closure. Writing an
                // unrelated top-level key is the way to prove the merge preserves
                // concurrent writes outside our key.
                with_claude_lock(&cfg_u, |existing| {
                    let mut root = parse_root_or_empty(existing)?;
                    root.insert(
                        "unrelated".into(),
                        RawValue::from_string(format!("\"{v}\"")).unwrap(),
                    );
                    Ok(Some(
                        serde_json::to_vec_pretty(&root).map_err(|e| format!("serialize: {e}"))?,
                    ))
                })
                .unwrap();
                // Also run a real pretrust_claude so both the real and unrelated
                // writers compete, exercising the mtime/len retry path.
                pretrust_claude(&cfg_u, &dirs_u[0], &cs_home_u).unwrap();
            }
        });

        for h in handles {
            h.join().unwrap();
        }
        unrelated.join().unwrap();

        let bytes = fs::read(cfg.as_path()).unwrap();
        let root: Value = serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!("torn JSON after concurrent writers: {e}; bytes={:?}", bytes)
        });
        let projects = root["projects"].as_object().unwrap();
        for d in dirs.iter() {
            let entry = projects
                .get(d.to_string_lossy().as_ref())
                .unwrap_or_else(|| panic!("missing project entry for {}", d.display()));
            assert_eq!(entry[CLAUDE_TRUST_KEY], json!(true));
        }
        assert_eq!(
            root["unrelated"],
            unrelated_values[unrelated_values.len() - 1]
        );
    }

    #[test]
    fn ac7_refuses_non_managed_paths_and_leaves_file_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let cs_home = tmp.path().join("codesense_home");
        fs::create_dir_all(&cs_home).unwrap();
        // Not under `codesense_home/users/` and not a registered worktree.
        let outside = tmp.path().join("outside-random");
        fs::create_dir_all(&outside).unwrap();

        let cfg = tmp.path().join(".claude.json");
        let before = br#"{"keep": true}"#;
        fs::write(&cfg, before).unwrap();

        let err = pretrust_claude(&cfg, &outside, &cs_home).unwrap_err();
        assert!(err.contains("not a codesense-managed target"), "got: {err}");
        assert_eq!(fs::read(&cfg).unwrap(), before);
    }

    #[test]
    fn ac9_untrust_removes_only_what_we_added_and_cleans_empty_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let cs_home = tmp.path().join("codesense_home");
        fs::create_dir_all(&cs_home).unwrap();
        let a = managed_dir_under(&cs_home, "only-trust");
        let b = managed_dir_under(&cs_home, "trust-plus-fields");
        let cfg = tmp.path().join(".claude.json");
        let seed = json!({
            "projects": {
                a.to_string_lossy(): {CLAUDE_TRUST_KEY: true},
                b.to_string_lossy(): {
                    CLAUDE_TRUST_KEY: true,
                    "allowedTools": ["bash"],
                },
            }
        });
        fs::write(&cfg, serde_json::to_vec_pretty(&seed).unwrap()).unwrap();

        untrust_claude(&cfg, &a);
        untrust_claude(&cfg, &b);

        let after: Value = serde_json::from_slice(&fs::read(&cfg).unwrap()).unwrap();
        let projects = after["projects"].as_object().unwrap();
        // Entry whose only field was trust is gone.
        assert!(projects.get(a.to_string_lossy().as_ref()).is_none());
        // Entry with other fields keeps them and only loses the trust key.
        let b_entry = &projects[b.to_string_lossy().as_ref()];
        assert_eq!(b_entry["allowedTools"], json!(["bash"]));
        assert!(b_entry.get(CLAUDE_TRUST_KEY).is_none());
    }

    #[test]
    fn pretrust_is_no_op_when_already_trusted_mtime_and_bytes_unchanged() {
        // Round-2 manager finding: re-launching into an already-trusted dir
        // must NOT rewrite `~/.claude.json` — every rewrite is another chance
        // to lose a concurrent write from a live Claude session.
        let tmp = tempfile::tempdir().unwrap();
        let cs_home = tmp.path().join("codesense_home");
        fs::create_dir_all(&cs_home).unwrap();
        let dir = managed_dir_under(&cs_home, "already-trusted");
        let cfg = tmp.path().join(".claude.json");

        // First call creates the file.
        pretrust_claude(&cfg, &dir, &cs_home).unwrap();
        let bytes1 = fs::read(&cfg).unwrap();
        let mtime1 = fs::metadata(&cfg).unwrap().modified().unwrap();

        // Second call MUST short-circuit: bytes + mtime identical.
        // Sleep past the filesystem's mtime granularity so any inadvertent
        // write would be visible as an mtime change.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        pretrust_claude(&cfg, &dir, &cs_home).unwrap();
        let bytes2 = fs::read(&cfg).unwrap();
        let mtime2 = fs::metadata(&cfg).unwrap().modified().unwrap();

        assert_eq!(bytes1, bytes2, "already-trusted dir rewrote the file");
        assert_eq!(
            mtime1, mtime2,
            "already-trusted dir touched the file's mtime"
        );
    }

    #[test]
    fn pretrust_preserves_key_order_at_every_level() {
        // Round-2 manager finding: `serde_json::Value` sorts keys. Users have
        // non-alphabetical top-level keys in `.claude.json` (`feedbackSurvey`,
        // `theme`, `projects`, `mcpContextUris`, …) and non-alphabetical
        // per-entry keys (`allowedTools`, `mcpServers`, …). SW-WA-F13's writer
        // must preserve this order while surgically adding one key.
        let tmp = tempfile::tempdir().unwrap();
        let cs_home = tmp.path().join("codesense_home");
        fs::create_dir_all(&cs_home).unwrap();
        let dir = managed_dir_under(&cs_home, "ordered-wt");
        let other = managed_dir_under(&cs_home, "other-wt");
        let cfg = tmp.path().join(".claude.json");

        // Deliberately non-alphabetical top-level keys and per-entry keys.
        // `zebra` before `apple` at the top; `zebra_tool` before `apple_tool`
        // in the entry; and `other-wt` listed BEFORE `ordered-wt` so we can
        // verify the dir we touch keeps its position (`insert` on an existing
        // key preserves it) and NEW keys land at the end.
        let seed = format!(
            r#"{{"zebra":"first","theme":"dark","apple":"last","projects":{{"{other}":{{"zebra_tool":1,"apple_tool":2}}}}}}"#,
            other = other.to_string_lossy(),
        );
        fs::write(&cfg, seed.as_bytes()).unwrap();

        pretrust_claude(&cfg, &dir, &cs_home).unwrap();

        let after_bytes = fs::read(&cfg).unwrap();
        let after_text = std::str::from_utf8(&after_bytes).unwrap();

        // Top-level order: zebra → theme → apple → projects (unchanged;
        // `projects` kept its original position).
        let zebra = after_text.find("\"zebra\"").unwrap();
        let theme = after_text.find("\"theme\"").unwrap();
        let apple = after_text.find("\"apple\"").unwrap();
        let projects = after_text.find("\"projects\"").unwrap();
        assert!(
            zebra < theme && theme < apple && apple < projects,
            "top-level key order was not preserved: {after_text}"
        );

        // `projects` order: `other-wt` first (seeded first), then our new
        // `ordered-wt` (inserted at end).
        let other_at = after_text.find(other.to_string_lossy().as_ref()).unwrap();
        let ordered_at = after_text.find(dir.to_string_lossy().as_ref()).unwrap();
        assert!(
            other_at < ordered_at,
            "`projects` key order was not preserved (new key must append): {after_text}"
        );

        // Per-entry order on `other-wt`: zebra_tool → apple_tool (unchanged).
        let zebra_tool = after_text.find("\"zebra_tool\"").unwrap();
        let apple_tool = after_text.find("\"apple_tool\"").unwrap();
        assert!(
            zebra_tool < apple_tool,
            "per-entry key order was not preserved: {after_text}"
        );

        // And the one touched entry (`ordered-wt`) carries only the trust key.
        let after_json: Value = serde_json::from_slice(&after_bytes).unwrap();
        assert_eq!(
            after_json["projects"][dir.to_string_lossy().as_ref()][CLAUDE_TRUST_KEY],
            json!(true)
        );
    }

    #[test]
    fn pretrust_skips_non_object_entry_without_overwriting() {
        // Round-2 Edge Case Hunter finding: the writer must NOT clobber a
        // non-object `projects[<dir>]` value it doesn't understand. The seam
        // `parse_or_skip_entry` returns None in that case; the merge logs a
        // breadcrumb and leaves the entry alone (though it may still write a
        // sibling spelling for the canonical path — that's a different key).
        let tmp = tempfile::tempdir().unwrap();
        let cs_home = tmp.path().join("codesense_home");
        fs::create_dir_all(&cs_home).unwrap();
        let dir = managed_dir_under(&cs_home, "non-object-entry");
        let cfg = tmp.path().join(".claude.json");
        let seed = format!(
            r#"{{"projects":{{"{d}":"legacy-string-value"}}}}"#,
            d = dir.to_string_lossy()
        );
        fs::write(&cfg, seed.as_bytes()).unwrap();

        pretrust_claude(&cfg, &dir, &cs_home).unwrap();

        // The non-object entry at the raw spelling is preserved verbatim —
        // the writer MUST NOT replace `"legacy-string-value"` with an object.
        let after: Value = serde_json::from_slice(&fs::read(&cfg).unwrap()).unwrap();
        assert_eq!(
            after["projects"][dir.to_string_lossy().as_ref()],
            json!("legacy-string-value"),
            "non-object entry was overwritten"
        );
    }

    #[cfg(unix)]
    #[test]
    fn pretrust_sets_owner_only_perms_on_new_claude_json() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let cs_home = tmp.path().join("codesense_home");
        fs::create_dir_all(&cs_home).unwrap();
        let dir = managed_dir_under(&cs_home, "perm-check");
        let cfg = tmp.path().join(".claude.json");
        assert!(!cfg.exists());

        pretrust_claude(&cfg, &dir, &cs_home).unwrap();

        let mode = fs::metadata(&cfg).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "new `.claude.json` must be 0600, got {mode:o}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reap_sibling_expansion_respects_path_boundaries() {
        // Round-2 review finding: naive `starts_with("/var")` would match
        // `/var2/foo`, generating a bogus `/private/var2/foo` spelling.
        // Boundary check must only match on `/` or end-of-string.
        let siblings = super::macos_private_siblings(std::path::Path::new("/var2/foo"));
        assert!(
            siblings.is_empty(),
            "`/var2/foo` must not be treated as `/var/…`: {siblings:?}"
        );
        let siblings = super::macos_private_siblings(std::path::Path::new("/private/vartemp"));
        assert!(
            siblings.is_empty(),
            "`/private/vartemp` must not be treated as `/private/var/…`: {siblings:?}"
        );
        let siblings = super::macos_private_siblings(std::path::Path::new("/var/foo"));
        assert_eq!(siblings, vec![std::path::PathBuf::from("/private/var/foo")]);
        let siblings = super::macos_private_siblings(std::path::Path::new("/private/var/foo"));
        assert_eq!(siblings, vec![std::path::PathBuf::from("/var/foo")]);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn reap_sibling_expansion_is_empty_off_macos() {
        // `/private/var` is a macOS firmlink; on Linux it's a plain nonexistent
        // path. Generating sibling spellings there would write (or try to
        // remove) bogus keys.
        assert!(super::macos_private_siblings(std::path::Path::new("/var/foo")).is_empty());
        assert!(super::macos_private_siblings(std::path::Path::new("/private/var/foo")).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn ac9_reap_untrust_covers_canonical_and_private_sibling_spellings_even_after_removal() {
        // The reap-path invariant Reviewer flagged: pretrust writes raw +
        // canonical (and macOS `/private` sibling) at launch; by the time
        // `untrust_claude` runs, the dir is gone — `canonicalize()` fails — but
        // the entries we wrote at launch are still in `.claude.json`. The
        // reap-time spellings expansion catches them.
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join(".claude.json");

        // Imagine a worktree that existed under `/private/var/foo/wt-X` at
        // launch time: pretrust wrote both `/var/foo/wt-X` and
        // `/private/var/foo/wt-X`. By reap time the dir is gone, so
        // `dir.canonicalize()` would fail; the reap caller only has the raw
        // `/var/foo/wt-X` spelling (that's what the FE / registry passes). The
        // reap-time macOS sibling expansion must still clean up the canonical
        // spelling.
        let seed = json!({
            "projects": {
                "/var/foo/wt-X": {CLAUDE_TRUST_KEY: true},
                "/private/var/foo/wt-X": {CLAUDE_TRUST_KEY: true},
                // Unrelated managed dir — must survive.
                "/Users/anybody/live-wt": {CLAUDE_TRUST_KEY: true, "allowedTools": ["bash"]},
            }
        });
        fs::write(&cfg, serde_json::to_vec_pretty(&seed).unwrap()).unwrap();

        // Call untrust with just the raw spelling (the only one the FE knows),
        // on a path that does NOT exist on disk — simulating post-reap.
        untrust_claude(&cfg, Path::new("/var/foo/wt-X"));

        let after: Value = serde_json::from_slice(&fs::read(&cfg).unwrap()).unwrap();
        let projects = after["projects"].as_object().unwrap();
        assert!(
            projects.get("/var/foo/wt-X").is_none(),
            "raw spelling must be cleaned"
        );
        assert!(
            projects.get("/private/var/foo/wt-X").is_none(),
            "canonical/`/private` sibling must be cleaned even when canonicalize fails"
        );
        // Unrelated entry untouched.
        let live = &projects["/Users/anybody/live-wt"];
        assert_eq!(live[CLAUDE_TRUST_KEY], json!(true));
        assert_eq!(live["allowedTools"], json!(["bash"]));
    }

    #[cfg(unix)]
    #[test]
    fn ac10_pretrust_failure_on_unwritable_config_is_reported_and_doesnt_corrupt_anything() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let cs_home = tmp.path().join("codesense_home");
        fs::create_dir_all(&cs_home).unwrap();
        let dir = managed_dir_under(&cs_home, "wt-unwritable");

        // A read-only parent dir makes the staged tmp write fail — the
        // production equivalent of a `.claude.json` on a locked-down home.
        let config_dir = tmp.path().join("ro");
        fs::create_dir_all(&config_dir).unwrap();
        let cfg = config_dir.join(".claude.json");
        // Seed an existing file so the writer takes the read-modify-write path
        // (not the create-fresh path).
        fs::write(&cfg, br#"{"keep":true}"#).unwrap();
        let before = fs::read(&cfg).unwrap();
        fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o500)).unwrap();

        let result = pretrust_claude(&cfg, &dir, &cs_home);

        // Restore so tempdir cleanup can run.
        fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o700)).unwrap();

        assert!(result.is_err(), "unwritable dir must surface Err");
        // Existing file is byte-identical: never truncated, never damaged.
        assert_eq!(fs::read(&cfg).unwrap(), before);
    }

    #[test]
    fn cx2_managed_folder_trust_reaches_launch_overrides_only() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("codesense_home");
        fs::create_dir_all(&home).unwrap();
        let managed = managed_dir_under(&home, "codex-wt");
        let cli = codesense_pty_core::agent_cli::AgentCli::Codex;
        let flags = managed_trust_flags(cli, &managed, &home);
        assert!(!flags.is_empty());
        let flag = flags
            .iter()
            .find(|flag| flag.starts_with("projects="))
            .unwrap();
        let value: toml::Value = flag.parse().unwrap();
        assert_eq!(
            value["projects"][managed.to_str().unwrap()]["trust_level"].as_str(),
            Some("trusted")
        );
        let unmanaged = tmp.path().join("personal");
        fs::create_dir(&unmanaged).unwrap();
        assert!(managed_trust_flags(cli, &unmanaged, &home).is_empty());
    }

    // ---------- env-mutation helpers (tests only) ----------

    fn env_mutex() -> &'static std::sync::Mutex<()> {
        static M: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        M.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn with_env<F: FnOnce()>(pairs: &[(&str, Option<&str>)], f: F) {
        let mut prev: HashMap<String, Option<String>> = HashMap::new();
        for (k, v) in pairs {
            prev.insert((*k).into(), std::env::var(*k).ok());
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        for (k, v) in prev {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }
}
