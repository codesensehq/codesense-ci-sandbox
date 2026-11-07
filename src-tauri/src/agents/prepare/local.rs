// SPDX-License-Identifier: MIT
//! SW-XS-7: Step 2 on this Mac. The managed record is the stat cache: a
//! wanted path whose `(dev, ino, size, mtime_ns, ctime_ns)` still equals what
//! the engine recorded — and is a tick older than the record (racy-clean,
//! §11.6) — is never read. Writes are copy-on-write clones hashed as they
//! land, renamed into place only while the destination still holds what the
//! diff saw (an add never replaces a file that appeared meanwhile). A git
//! worktree's exclude block is read on every run and repaired when it lost
//! paths; it lists every managed path before any file lands and is trimmed
//! after removals (§11.4).
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::diff::{Observation, Observed, Step};
use super::{PreparePhase, Run, StatKey};
use codesense_pty_core::remote_manifest::normalized_mode;

/// The worktree's Git index, for the tracked-path skip (§11.3).
pub(super) struct Tracked(Option<git2::Index>, PathBuf);

impl Tracked {
    /// Fails CLOSED: a destination with `.git` whose index cannot be read is
    /// an error, never "nothing is tracked".
    pub(super) fn open(root: &Path) -> Result<Self, String> {
        let Some(git) =
            crate::content_engine::git_worktree::GitWorktree::discover(root).map_err(|error| {
                format!(
                    "Couldn't read the Git index of {} to keep tracked files untouched: {error}",
                    root.display()
                )
            })?
        else {
            return Ok(Self(None, PathBuf::new()));
        };
        git2::Repository::open(&git.root)
            .and_then(|repo| repo.index())
            .map(|index| Self(Some(index), git.prefix))
            .map_err(|error| {
                format!(
                    "Couldn't read the Git index of {} to keep tracked files untouched: {error}",
                    root.display()
                )
            })
    }
    pub(super) fn contains(&self, path: &str) -> bool {
        self.0
            .as_ref()
            .is_some_and(|index| index.get_path(&self.1.join(path), 0).is_some())
    }
}

struct Observer<'a> {
    root: &'a Path,
    tracked: Tracked,
    saved_at_ns: i64,
    /// Directories already known to be real directories (no symlink, no
    /// nested repository).
    safe_dirs: RefCell<HashSet<PathBuf>>,
    seen: RefCell<std::collections::HashMap<String, (Observation, Option<StatKey>)>>,
    hashed: RefCell<usize>,
}

impl Observer<'_> {
    /// Every ancestor of `relative` below the root is a real directory (or
    /// absent) and not a nested repository: nothing is written through a
    /// symlink, over a file, or into another repo (§11.6).
    fn ancestors_safe(&self, relative: &Path) -> Result<(), String> {
        let mut at = self.root.to_path_buf();
        let components: Vec<_> = relative.components().collect();
        for component in &components[..components.len().saturating_sub(1)] {
            at.push(component);
            if self.safe_dirs.borrow().contains(&at) {
                continue;
            }
            match std::fs::symlink_metadata(&at) {
                Ok(meta) if meta.file_type().is_dir() => {
                    if std::fs::symlink_metadata(at.join(".git")).is_ok() {
                        return Err(format!("{} is a nested repository", at.display()));
                    }
                    self.safe_dirs.borrow_mut().insert(at.clone());
                }
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(format!("{} is a symlink", at.display()))
                }
                Ok(_) => return Err(format!("{} is not a directory", at.display())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(())
    }

    fn observe(&self, dest: &str, cached: Option<&super::ManagedEntry>) -> Observation {
        if let Some((seen, _)) = self.seen.borrow().get(dest) {
            return seen.clone();
        }
        let (observation, stat) = self.look(dest, cached);
        self.seen
            .borrow_mut()
            .insert(dest.to_string(), (observation.clone(), stat));
        observation
    }

    fn look(
        &self,
        dest: &str,
        cached: Option<&super::ManagedEntry>,
    ) -> (Observation, Option<StatKey>) {
        let tracked = self.tracked.contains(dest);
        let other = |reason: String| {
            (
                Observation {
                    state: Observed::Other { reason },
                    tracked,
                },
                None,
            )
        };
        if let Err(reason) = self.ancestors_safe(Path::new(dest)) {
            return other(reason);
        }
        let path = self.root.join(dest);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (
                    Observation {
                        state: Observed::Absent,
                        tracked,
                    },
                    None,
                )
            }
            Err(error) => return other(error.to_string()),
        };
        if meta.file_type().is_symlink() {
            return other("a symlink is there".into());
        }
        if !meta.file_type().is_file() {
            return other("a directory or special file is there".into());
        }
        let key = StatKey::of(&meta);
        #[cfg(unix)]
        let mode = {
            use std::os::unix::fs::PermissionsExt;
            normalized_mode(meta.permissions().mode())
        };
        #[cfg(not(unix))]
        let mode = 0o644;
        let hash = match cached.filter(|entry| key.trusted(entry.dest_stat, self.saved_at_ns)) {
            Some(entry) => entry.hash.0,
            None => {
                *self.hashed.borrow_mut() += 1;
                match crate::content_engine::atomic_io::hash_file(&path) {
                    Ok(hash) => hash,
                    Err(error) => return other(error.to_string()),
                }
            }
        };
        (
            Observation {
                state: Observed::File { hash, mode },
                tracked,
            },
            Some(key),
        )
    }
}

/// The exclude block as `info/exclude` has it: `Ok(paths)` (none when there
/// is no block), `Err(reason)` when it was edited by hand.
pub(super) fn actual_block(
    root: &Path,
    workspace_id: &str,
) -> Result<Result<Vec<String>, String>, String> {
    match crate::content_engine::git_exclude::workspace_block(root, workspace_id) {
        Ok(paths) => {
            let mut paths = paths.unwrap_or_default();
            if let Some(git) = crate::content_engine::git_worktree::GitWorktree::discover(root)? {
                if git.nested_linked() {
                    paths = paths
                        .into_iter()
                        .filter_map(|p| {
                            Path::new(&p)
                                .strip_prefix(&git.prefix)
                                .ok()
                                .map(|p| p.to_string_lossy().into_owned())
                        })
                        .collect();
                }
            }
            paths.sort();
            paths.dedup();
            Ok(Ok(paths))
        }
        Err(crate::content_engine::Error::InvalidExcludeBlock { reason }) => Ok(Err(reason)),
        Err(error) => Err(format!(
            "Couldn't read the agent context's git exclude: {error}"
        )),
    }
}

/// Git's NUL-delimited porcelain retains unquoted paths, including whitespace.
/// Uncertainty is reported to the locked writer as `None`, never an empty set.
pub(super) fn live_workspace_ids(root: &Path) -> Option<HashSet<String>> {
    let list = match crate::commands::worktree::run_git(
        &["worktree", "list", "--porcelain", "-z"],
        root,
    ) {
        Ok(list) => list,
        Err(error) => {
            log::warn!(target: "content_engine::git_exclude", "skipping workspace exclude GC for {}: {error}", Path::display(root));
            return None;
        }
    };
    let mut ids = HashSet::new();
    for path in list
        .split('\0')
        .filter_map(|field| field.strip_prefix("worktree "))
    {
        let root = Path::new(path);
        match std::fs::metadata(root) {
            Ok(_) => {}
            Err(error) if std::io::Error::kind(&error) == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                log::warn!(target: "content_engine::git_exclude",
                    "skipping workspace exclude GC: couldn't inspect listed worktree {}: {error}", Path::display(root));
                return None;
            }
        }
        ids.insert(super::workspace_id(&super::DestinationIdentity::Local {
            root: root.to_path_buf(),
        }));
        ids.insert(super::workspace_id(
            &crate::content_engine::context_lifecycle::local_identity(root),
        ));
    }
    Some(ids)
}

/// Write the block only while it still holds `expected` (compare-and-swap
/// under the per-repo lock); a change in between is merged once.
pub(super) fn write_block(
    root: &Path,
    workspace_id: &str,
    expected: &[String],
    paths: &[String],
) -> Result<Vec<String>, String> {
    use crate::content_engine::git_exclude::replace_workspace_block_if_with_gc;
    let failed =
        |e: crate::content_engine::Error| format!("Couldn't hide the agent context from git: {e}");
    let git = crate::content_engine::git_worktree::GitWorktree::discover(root)?;
    let destination_root = root
        .canonicalize()
        .map_err(|error| format!("workspace {} is unavailable: {error}", Path::display(root)))?;
    // Nested linked destinations share a private exclude file, not repo history.
    let live_ids = || {
        if git.as_ref().is_some_and(|git| git.nested_linked()) {
            None
        } else {
            live_workspace_ids(root)
        }
    };
    let encode = |paths: &[String]| -> Vec<String> {
        paths
            .iter()
            .map(|p| match &git {
                Some(g) if g.nested_linked() => g.relative(p),
                _ => p.clone(),
            })
            .collect()
    };
    if replace_workspace_block_if_with_gc(
        root,
        workspace_id,
        Some(&encode(expected)),
        &encode(paths),
        Some(&destination_root),
        live_ids,
    )
    .map_err(failed)?
    {
        return Ok(paths.to_vec());
    }
    // Someone else changed the block meanwhile: keep their paths too.
    let current = match actual_block(root, workspace_id)? {
        Ok(current) => current,
        Err(reason) => return Err(format!("the exclude block was edited by hand: {reason}")),
    };
    let mut merged: Vec<String> = current.iter().chain(paths).cloned().collect();
    merged.sort();
    merged.dedup();
    if replace_workspace_block_if_with_gc(
        root,
        workspace_id,
        Some(&encode(&current)),
        &encode(&merged),
        Some(&destination_root),
        live_ids,
    )
    .map_err(failed)?
    {
        Ok(merged)
    } else {
        Err("the exclude block kept changing; launch again".into())
    }
}

/// Run Step 2 against `root`.
pub(super) fn run(run: &mut Run<'_, '_>, root: &Path) -> Result<(), String> {
    let root = root
        .canonicalize()
        .map_err(|e| format!("workspace {} is unavailable: {e}", root.display()))?;
    let tracked = Tracked::open(&root)?;
    run.hash_wants()?;
    let observer = Observer {
        root: &root,
        tracked,
        saved_at_ns: run.record.saved_at_ns,
        safe_dirs: RefCell::default(),
        seen: RefCell::default(),
        hashed: RefCell::new(0),
    };
    let managed = run.record.entries.clone();
    run.classify(&|dest| observer.observe(dest, managed.get(dest)));
    run.hashed += *observer.hashed.borrow();
    for (dest, (_, stat)) in observer.seen.borrow().iter() {
        if let Some(stat) = stat {
            run.dest_stats.insert(dest.clone(), *stat);
        }
    }
    let seen = observer.seen.into_inner();
    let git = crate::content_engine::git_worktree::GitWorktree::discover(&root)?.is_some();
    let workspace_id = super::workspace_id(&run.identity);

    // Read the block every run (§11.4): a dropped or rewritten block is
    // repaired even when no file changes.
    let actual = if git {
        match actual_block(&root, &workspace_id)? {
            Ok(paths) => Some(paths),
            Err(reason) => {
                run.exclude_unusable(&reason);
                return Ok(());
            }
        }
    } else {
        None
    };
    let desired = actual.as_ref().map(|actual| run.exclude_desired(actual));
    let writes: Vec<(usize, Step)> = run.plan.writes().copied().collect();
    let chmods: Vec<usize> = run
        .plan
        .steps
        .iter()
        .filter(|(_, step)| matches!(step, Step::Chmod { .. }))
        .map(|(index, _)| *index)
        .collect();
    if writes.is_empty() && chmods.is_empty() && run.plan.removes.is_empty() {
        if let (Some(actual), Some(desired)) = (&actual, &desired) {
            // Equal paths can still need root metadata or shared-history cleanup.
            (run.request.validate)()?;
            run.record.exclude = Some(write_block(&root, &workspace_id, actual, desired)?);
        }
        return Ok(());
    }
    (run.request.validate)()?;
    // Exclude every managed path and every planned write before any lands;
    // a file about to be removed stays excluded until it is gone.
    let mut before = desired.clone().unwrap_or_default();
    if let (Some(actual), Some(_)) = (&actual, &desired) {
        before.extend(run.plan.removes.iter().map(|(path, _)| path.clone()));
        before.sort();
        before.dedup();
        before = write_block(&root, &workspace_id, actual, &before)?;
    }
    let bytes_total: u64 = writes
        .iter()
        .map(|(index, _)| run.want().files[*index].bytes)
        .sum();
    if !writes.is_empty() {
        run.emit(PreparePhase::Transfer, 0, 0, bytes_total);
    }
    let mut swept: HashSet<PathBuf> = HashSet::new();
    let mut staged: Vec<(usize, PathBuf)> = Vec::new();
    let mut copied_bytes = 0u64;
    for (index, _) in &writes {
        if run.cancelled() {
            run.cancelled_run = true;
            break;
        }
        let file = &run.request.want.files[*index];
        match stage(&root, file, run.hash_of(*index), &mut swept) {
            Ok(tmp) => {
                copied_bytes += file.bytes;
                staged.push((*index, tmp));
                run.emit(
                    PreparePhase::Transfer,
                    staged.len() as u64,
                    copied_bytes,
                    bytes_total,
                );
            }
            Err(error) => run.failed.push((file.dest.clone(), error)),
        }
    }
    if run.cancelled_run || run.cancelled() {
        // SW-XS-8: cancelled before anything landed: no staged copy stays.
        run.cancelled_run = true;
        for (_, tmp) in staged.drain(..) {
            let _ = std::fs::remove_file(tmp);
        }
    } else {
        run.emit(PreparePhase::Apply, 0, 0, copied_bytes);
    }
    let mut applied = 0u64;
    let mut staged = staged.into_iter();
    while let Some((index, tmp)) = staged.next() {
        if run.cancelled() {
            // Stop between files: what landed stays (and is recorded),
            // nothing else is left behind.
            run.cancelled_run = true;
            let _ = std::fs::remove_file(&tmp);
            for (_, tmp) in staged.by_ref() {
                let _ = std::fs::remove_file(tmp);
            }
            break;
        }
        let file = &run.request.want.files[index];
        let step = writes
            .iter()
            .find(|(i, _)| *i == index)
            .map(|(_, step)| *step)
            .expect("staged from a write");
        let expected = seen.get(&file.dest).and_then(|(_, stat)| *stat);
        match land(&root, &file.dest, &tmp, step, expected, run.hash_of(index)) {
            Ok(stat) => {
                run.dest_stats.insert(file.dest.clone(), stat);
                run.written.push(file.dest.clone());
                run.bytes_written += file.bytes;
            }
            Err(error) => {
                let _ = std::fs::remove_file(&tmp);
                run.failed.push((file.dest.clone(), error));
            }
        }
        applied += 1;
        run.emit(
            PreparePhase::Apply,
            applied,
            run.bytes_written,
            copied_bytes,
        );
    }
    let chmods = if run.cancelled_run {
        Vec::new()
    } else {
        chmods
    };
    if run.cancelled_run {
        run.plan.removes.clear();
    }
    for index in chmods {
        let file = &run.request.want.files[index];
        match chmod(&root.join(&file.dest), file.mode) {
            Ok(stat) => {
                run.dest_stats.insert(file.dest.clone(), stat);
                run.chmodded.push(file.dest.clone());
            }
            Err(error) => run.failed.push((file.dest.clone(), error)),
        }
    }
    for (path, expected) in run.plan.removes.clone() {
        let target = root.join(&path);
        if std::fs::symlink_metadata(&target).is_err() {
            // Already gone: the removal holds (§11.7 AlreadyAbsent).
            run.removed.push(path);
            continue;
        }
        match crate::content_engine::atomic_io::hash_file(&target) {
            Ok(hash) if hash == expected => match std::fs::remove_file(&target) {
                Ok(()) => run.removed.push(path),
                Err(error) => run.failed.push((path, error.to_string())),
            },
            Ok(_) => run.plan.kept_modified.push(path),
            Err(error) => run.failed.push((path, error.to_string())),
        }
    }
    // Trim the block to what is managed now: a write that failed where no
    // managed file was is no longer excluded (a user's file there must show).
    if git {
        // A planned write that did not land (failed, or cancelled first).
        let written: HashSet<&str> = run.written.iter().map(String::as_str).collect();
        let failed: HashSet<&str> = writes
            .iter()
            .map(|(index, _)| run.request.want.files[*index].dest.as_str())
            .filter(|dest| !written.contains(dest))
            .chain(run.failed.iter().map(|(p, _)| p.as_str()))
            .collect();
        let previously: HashSet<&str> = actual
            .iter()
            .flatten()
            .map(String::as_str)
            .chain(run.record.entries.keys().map(String::as_str))
            .collect();
        let after: Vec<String> = before
            .iter()
            .filter(|path| previously.contains(path.as_str()) || !failed.contains(path.as_str()))
            .filter(|path| !run.removed.contains(path) && !run.plan.forget.contains(path))
            .cloned()
            .collect();
        run.record.exclude = Some(if after != before {
            write_block(&root, &workspace_id, &before, &after)?
        } else {
            after
        });
    }
    Ok(())
}

/// Remove `.<name>.codesense-prep.<id>` files a cut run left in `dir`, once
/// per directory per run, when they are over ten minutes old.
pub(super) fn sweep_stale_temps(dir: &Path, swept: &mut HashSet<PathBuf>) {
    if !swept.insert(dir.to_path_buf()) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !(name.starts_with('.') && name.contains(".codesense-prep.")) {
            continue;
        }
        if entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| modified < cutoff)
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Clone (or copy) the source beside its destination and hash what landed:
/// a source that changed since the diff aborts this entry (§11.7).
fn stage(
    root: &Path,
    file: &super::WantFile,
    hash: [u8; 32],
    swept: &mut HashSet<PathBuf>,
) -> Result<PathBuf, String> {
    let dest = root.join(&file.dest);
    let parent = dest.parent().ok_or("destination has no parent")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("couldn't create its folder: {e}"))?;
    // Re-check after creating: no component became a symlink meanwhile.
    let canonical = parent
        .canonicalize()
        .map_err(|e| format!("couldn't resolve its folder: {e}"))?;
    if !canonical.starts_with(root) {
        return Err("its folder resolves outside the workspace".into());
    }
    sweep_stale_temps(parent, swept);
    let leaf = dest
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("destination has no file name")?;
    let tmp = parent.join(format!(
        ".{leaf}.codesense-prep.{}",
        uuid::Uuid::new_v4().simple()
    ));
    crate::content_engine::atomic_io::clone_or_copy(&file.source, &tmp)
        .map_err(|e| format!("couldn't copy it: {e}"))?;
    let landed = crate::content_engine::atomic_io::hash_file(&tmp);
    let fail = |why: String| {
        let _ = std::fs::remove_file(&tmp);
        Err(why)
    };
    match landed {
        Ok(landed) if landed == hash => {}
        Ok(_) => return fail("the team file changed while it was copied; launch again".into()),
        Err(error) => return fail(format!("couldn't read the copy: {error}")),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) =
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(file.mode))
        {
            return fail(format!("couldn't set its mode: {error}"));
        }
        use std::os::unix::io::AsRawFd;
        if let Ok(handle) = std::fs::File::open(&tmp) {
            // SAFETY: a valid descriptor. A plain fsync, not F_FULLFSYNC: the
            // rename is atomic and a lost copy is re-made by the next launch.
            unsafe { libc::fsync(handle.as_raw_fd()) };
        }
    }
    Ok(tmp)
}

/// Rename `from` onto `to` only when `to` does not exist (an add never
/// replaces a file that appeared after the diff).
fn rename_exclusive(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let (from_path, to_path) = (from, to);
        let from = std::ffi::CString::new(from.as_os_str().as_bytes())?;
        let to = std::ffi::CString::new(to.as_os_str().as_bytes())?;
        // SAFETY: two valid NUL-terminated paths.
        #[cfg(target_os = "macos")]
        let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(target_os = "linux")]
        let rc = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        // A file system without exclusive rename: a hard link is just as
        // exclusive (it fails if `to` exists), then the temp name goes.
        match error.raw_os_error() {
            Some(code)
                if code == libc::ENOTSUP
                    || code == libc::EOPNOTSUPP
                    || code == libc::EINVAL
                    || code == libc::ENOSYS =>
            {
                match std::fs::hard_link(from_path, to_path) {
                    Ok(()) => {
                        let _ = std::fs::remove_file(from_path);
                        Ok(())
                    }
                    Err(link) if link.kind() == std::io::ErrorKind::AlreadyExists => Err(link),
                    // No hard links either: check, then rename.
                    Err(_) => {
                        if to_path.symlink_metadata().is_ok() {
                            return Err(std::io::ErrorKind::AlreadyExists.into());
                        }
                        std::fs::rename(from_path, to_path)
                    }
                }
            }
            _ => Err(error),
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        if to.symlink_metadata().is_ok() {
            return Err(std::io::ErrorKind::AlreadyExists.into());
        }
        std::fs::rename(from, to)
    }
}

/// Rename the staged copy into place while the destination still holds what
/// the diff saw; a destination that already holds the wanted bytes is a
/// success (two launches racing, §11.7).
pub(super) fn land(
    root: &Path,
    dest: &str,
    tmp: &Path,
    step: Step,
    expected: Option<StatKey>,
    hash: [u8; 32],
) -> Result<StatKey, String> {
    let path = root.join(dest);
    let already = || {
        std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_file())
            && crate::content_engine::atomic_io::hash_file(&path).ok() == Some(hash)
    };
    let stat_of = || {
        std::fs::symlink_metadata(&path)
            .map(|meta| StatKey::of(&meta))
            .map_err(|e| e.to_string())
    };
    match step {
        Step::Add => match rename_exclusive(tmp, &path) {
            Ok(()) => stat_of(),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if already() {
                    let _ = std::fs::remove_file(tmp);
                    stat_of()
                } else {
                    Err("a file appeared there while it was being prepared".into())
                }
            }
            Err(error) => Err(format!("couldn't put it in place: {error}")),
        },
        Step::Replace { expected: want } => {
            let unchanged = std::fs::symlink_metadata(&path).is_ok_and(|meta| {
                meta.file_type().is_file()
                    && (Some(StatKey::of(&meta)) == expected
                        || crate::content_engine::atomic_io::hash_file(&path).ok() == Some(want))
            });
            if !unchanged {
                if already() {
                    let _ = std::fs::remove_file(tmp);
                    return stat_of();
                }
                return Err("it changed in the workspace while it was being prepared".into());
            }
            std::fs::rename(tmp, &path).map_err(|e| format!("couldn't put it in place: {e}"))?;
            stat_of()
        }
        _ => Err("not a write".into()),
    }
}

fn chmod(path: &Path, mode: u32) -> Result<StatKey, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| e.to_string())?;
    }
    std::fs::symlink_metadata(path)
        .map(|meta| StatKey::of(&meta))
        .map_err(|e| e.to_string())
}
