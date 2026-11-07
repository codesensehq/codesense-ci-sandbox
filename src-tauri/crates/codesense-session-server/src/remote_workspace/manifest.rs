// SPDX-License-Identifier: MIT
//! The workspace manifest under declared prefixes, answered from a
//! per-workspace hash cache so an unchanged file is never read again.
//!
//! - Cache key `(st_dev, ino, size, mtime_ns, ctime_ns)`; the file's header
//!   holds the filesystem's id and the time of the walk that wrote it. A
//!   changed filesystem id drops the cache.
//! - Racy-clean rule: an entry whose `max(mtime, ctime)` is within the
//!   timestamp granularity of the previous walk is hashed again, so an edit
//!   inside one timestamp tick is never mistaken for the cached bytes.
//! - A file is hashed through one descriptor, `fstat`ed before and after; a
//!   file that changed meanwhile is hashed again, then reported `unstable`
//!   and never cached.
//! - The cache file is replaced by tmp + rename under the workspace's cache
//!   lock; a cache that does not parse is discarded and rebuilt.
//! - The walk never follows a symlink (`AT_SYMLINK_NOFOLLOW` / `O_NOFOLLOW`)
//!   and skips any directory holding its own `.git`.
//! - It takes no workspace lock: it runs inside the workspace's write
//!   generation seqlock, so it never reports a write half done.
use super::*;
use codesense_pty_core::remote_files::MAX_FILE_BYTES;
use codesense_pty_core::remote_manifest::{
    nfc, normalized_mode, EntryKind, ManifestContent, ManifestEntry, ManifestPart,
    ManifestProgress, ManifestReply, ManifestRequest, UnreadFile, ENTRIES_PER_PART, MAX_ENTRIES,
    MAX_READ_BYTES, READ_CHUNK_BYTES,
};
use std::collections::{BTreeMap, HashSet};
use std::ffi::CStr;

const CACHE_VERSION: u32 = 2;
const CACHE_DIR: &str = "remote-manifest-cache";
/// Timestamp granularity on a filesystem with nanosecond timestamps; covers
/// the kernel's coarse clock tick.
const FINE_GRANULARITY_NS: i128 = 50_000_000;
/// On a filesystem that keeps whole seconds (and FAT's two).
const COARSE_GRANULARITY_NS: i128 = 2_000_000_000;
/// A progress-only part every this many entries walked.
const PROGRESS_EVERY: u64 = 1_000;
/// And at least this often while hashing, so the desktop's idle timeout
/// (30 s) never fires on a walk that is still working.
const HEARTBEAT: Duration = Duration::from_secs(2);
/// Index listing bound for the `tracked` flags.
const TRACKED_BYTES: usize = 32 * 1024 * 1024;
/// Below the desktop's 30 s idle timeout: no part is sent while Git runs.
const TRACKED_DEADLINE: Duration = Duration::from_secs(20);
/// Cached entries under a prefix not walked for this long are dropped.
const CACHE_RETENTION_NS: i128 = 30 * 24 * 60 * 60 * 1_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CachedFile {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
    ctime_ns: i128,
    hash: String,
    mode: u32,
    /// When this entry's bytes were last read and hashed: the racy-clean rule
    /// is judged against this, per entry, not against the newest cache write
    /// (an entry carried over from another prefix was not verified then).
    verified_at_ns: i128,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    fs_id: String,
    /// When the walk that wrote this cache started.
    written_at_ns: i128,
    /// When each prefix was last walked, for retention.
    #[serde(default)]
    walked: BTreeMap<String, i128>,
    entries: BTreeMap<String, CachedFile>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Key {
    dev: u64,
    ino: u64,
    size: u64,
    mtime_ns: i128,
    ctime_ns: i128,
}
impl Key {
    // `stat` field widths differ between macOS and Linux.
    #[allow(clippy::unnecessary_cast)]
    fn of(stat: &libc::stat) -> Self {
        #[cfg(test)]
        let coarse = COARSE_TIMESTAMPS.with(|flag| flag.get());
        #[cfg(not(test))]
        let coarse = false;
        let ns = |seconds: i64, nanos: i64| {
            i128::from(seconds) * 1_000_000_000 + if coarse { 0 } else { i128::from(nanos) }
        };
        Self {
            dev: stat.st_dev as u64,
            ino: stat.st_ino as u64,
            size: stat.st_size as u64,
            mtime_ns: ns(stat.st_mtime as i64, stat.st_mtime_nsec as i64),
            ctime_ns: ns(stat.st_ctime as i64, stat.st_ctime_nsec as i64),
        }
    }
    fn granularity(&self) -> i128 {
        if self.mtime_ns % 1_000_000_000 == 0 && self.ctime_ns % 1_000_000_000 == 0 {
            COARSE_GRANULARITY_NS
        } else {
            FINE_GRANULARITY_NS
        }
    }
    /// Racy-clean: changed so close to when it was hashed that an edit in the
    /// same timestamp tick could keep this exact key.
    fn racy(&self, verified_at_ns: i128) -> bool {
        self.mtime_ns.max(self.ctime_ns) >= verified_at_ns - self.granularity()
    }
    fn matches(&self, cached: &CachedFile) -> bool {
        self.dev == cached.dev
            && self.ino == cached.ino
            && self.size == cached.size
            && self.mtime_ns == cached.mtime_ns
            && self.ctime_ns == cached.ctime_ns
    }
}

#[cfg(test)]
type HashHook = Option<Box<dyn FnMut(&str)>>;
#[cfg(test)]
thread_local! {
    /// Simulates a filesystem with whole-second timestamps.
    pub(super) static COARSE_TIMESTAMPS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Hashing hook: runs between the two `fstat`s of one file.
    pub(super) static DURING_HASH: std::cell::RefCell<HashHook> =
        const { std::cell::RefCell::new(None) };
    /// Runs at the start of every walk attempt of one call.
    pub(super) static EACH_ATTEMPT: std::cell::RefCell<HashHook> =
        const { std::cell::RefCell::new(None) };
    /// A virtual clock: added to the wall clock.
    pub(super) static CLOCK_OFFSET_NS: std::cell::Cell<i128> = const { std::cell::Cell::new(0) };
}

fn now_ns() -> i128 {
    #[cfg(test)]
    let offset = CLOCK_OFFSET_NS.with(|offset| offset.get());
    #[cfg(not(test))]
    let offset = 0;
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as i128)
        .unwrap_or(0)
        + offset
}

/// A lookup's answer: `Ok(None)` only for ENOENT (gone since it was listed);
/// every other failure is an error that makes the manifest incomplete.
type Lookup<T> = Result<Option<T>, String>;
fn failure(what: &str, path: &str) -> Lookup<std::convert::Infallible> {
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ENOENT) {
        Ok(None)
    } else {
        Err(format!("{what} {path:?} failed: {error}"))
    }
}
fn stat_at(parent: &File, name: &CStr, path: &str) -> Lookup<libc::stat> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: held parent, NUL-terminated name, writable storage; never follows a link.
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        // SAFETY: initialized by the successful call.
        return Ok(Some(unsafe { stat.assume_init() }));
    }
    failure("inspecting", path).map(|_| None)
}
fn fstat(file: &File) -> Option<libc::stat> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: held descriptor and writable storage.
    let rc = unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) };
    (rc == 0).then(|| unsafe { stat.assume_init() })
}
fn open_at(parent: &File, name: &CStr, flags: i32, path: &str) -> Lookup<File> {
    // SAFETY: held parent and NUL-terminated name; a successful FD has one owner.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd >= 0 {
        // SAFETY: a fresh descriptor we own.
        return Ok(Some(unsafe { File::from_raw_fd(fd) }));
    }
    failure("opening", path).map(|_| None)
}
/// The directory at worktree-relative `path`, opened one no-follow component
/// at a time; only the result stays open.
fn open_directory_at(worktree: &File, path: &str) -> Lookup<File> {
    let mut directory = io(worktree.try_clone())?;
    let mut walked = String::new();
    for part in path.split('/') {
        if !walked.is_empty() {
            walked.push('/');
        }
        walked.push_str(part);
        let component = CString::new(part).map_err(|_| "invalid path component")?;
        match open_at(
            &directory,
            &component,
            libc::O_RDONLY | libc::O_DIRECTORY,
            &walked,
        )? {
            Some(next) => directory = next,
            None => return Ok(None),
        }
    }
    Ok(Some(directory))
}
fn kind_of(stat: &libc::stat) -> libc::mode_t {
    stat.st_mode & libc::S_IFMT
}

/// One directory's raw entry names (no `.`/`..`), and whether it holds `.git`.
fn entries(directory: &File) -> Result<(Vec<Vec<u8>>, bool), String> {
    // A fresh open-file description: the stream offset is ours alone.
    // SAFETY: fixed dot component of the held directory.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "directory reopen failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: fd is an owned directory descriptor transferred to fdopendir.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        // SAFETY: fdopendir did not take ownership on failure.
        unsafe { libc::close(fd) };
        return Err("remote directory cannot be enumerated".into());
    }
    struct Stream(*mut libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            // SAFETY: the stream is owned and closed once.
            unsafe { libc::closedir(self.0) };
        }
    }
    let stream = Stream(stream);
    let mut names = Vec::new();
    let mut git = false;
    loop {
        // SAFETY: owned stream; the name is copied before the next call.
        let entry = unsafe {
            *files::directory_errno() = 0;
            libc::readdir(stream.0)
        };
        if entry.is_null() {
            // SAFETY: thread-local errno.
            if unsafe { *files::directory_errno() } != 0 {
                return Err("remote directory scan failed; result is incomplete".into());
            }
            break;
        }
        // SAFETY: readdir's name is NUL-terminated within the entry.
        let raw = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if raw == b"." || raw == b".." {
            continue;
        }
        if raw.eq_ignore_ascii_case(b".git") {
            git = true;
            continue;
        }
        names.push(raw.to_vec());
    }
    names.sort();
    Ok((names, git))
}

struct Walk<'a> {
    previous: &'a CacheFile,
    /// Every file this attempt saw, cached (hits and fresh hashes).
    seen: BTreeMap<String, CachedFile>,
    /// Fresh, `fstat`-stable hashes from every attempt of this call: kept
    /// even when the call ends busy, so a retry does not hash them again.
    hashed: &'a mut BTreeMap<String, CachedFile>,
    started_ns: i128,
    entries: Vec<ManifestEntry>,
    progress: ManifestProgress,
    limit: usize,
    incomplete: Option<String>,
    last_part: Instant,
    part: &'a mut dyn FnMut(ManifestPart) -> bool,
    alive: &'a dyn Fn() -> bool,
}
const CLOSED: &str = "the connection closed during the manifest";
const RETIRED: &str = "remote file authority retired";
const LIMIT: &str = "the manifest exceeds its limit";
impl Walk<'_> {
    fn send_progress(&mut self) -> Result<(), String> {
        self.last_part = Instant::now();
        if (self.part)(ManifestPart {
            progress: self.progress,
            entries: Vec::new(),
            contents: Vec::new(),
        }) {
            Ok(())
        } else {
            Err(CLOSED.into())
        }
    }
    fn heartbeat(&mut self) -> Result<(), String> {
        if !(self.alive)() {
            return Err(RETIRED.into());
        }
        if self.last_part.elapsed() >= HEARTBEAT {
            self.send_progress()?;
        }
        Ok(())
    }
    /// A failure that leaves the walk going but the manifest incomplete.
    fn incomplete(&mut self, reason: String) {
        self.incomplete.get_or_insert(reason);
    }
    fn push(&mut self, entry: ManifestEntry) -> Result<(), String> {
        if self.entries.len() >= self.limit {
            self.incomplete(LIMIT.into());
            return Err(LIMIT.into());
        }
        self.entries.push(entry);
        self.progress.scanned += 1;
        if self.progress.scanned.is_multiple_of(PROGRESS_EVERY) {
            self.send_progress()?;
        }
        self.heartbeat()
    }
    fn skipped(&mut self, path: String, kind: EntryKind, reason: &str) -> Result<(), String> {
        self.push(ManifestEntry {
            path,
            kind,
            hash: None,
            size: 0,
            mode: 0,
            tracked: false,
            oversize: false,
            disk_name: None,
            reason: Some(reason.into()),
        })
    }
    fn found(
        &mut self,
        path: String,
        disk_name: Option<String>,
        cached: &CachedFile,
    ) -> Result<(), String> {
        let hash = hex_hash(&cached.hash)?;
        self.push(ManifestEntry {
            path,
            kind: EntryKind::File,
            hash: Some(hash),
            size: cached.size,
            mode: cached.mode,
            tracked: false,
            oversize: cached.size > MAX_FILE_BYTES as u64,
            disk_name,
            reason: None,
        })
    }

    /// One regular file: from the cache when its key still matches and it is
    /// not racy, else hashed through one descriptor between two `fstat`s.
    fn file(
        &mut self,
        parent: &File,
        name: &CStr,
        path: String,
        disk_name: Option<String>,
        listed: &libc::stat,
    ) -> Result<(), String> {
        // SAFETY: geteuid takes no pointers.
        if listed.st_uid != unsafe { libc::geteuid() } || listed.st_nlink != 1 {
            return self.skipped(
                path,
                EntryKind::Unsafe,
                "hard-linked or owned by another account; never written",
            );
        }
        let key = Key::of(listed);
        let fresh = self.hashed.get(&path).cloned();
        let known = fresh.as_ref().or_else(|| self.previous.entries.get(&path));
        if let Some(cached) = known {
            if key.matches(cached) && !key.racy(cached.verified_at_ns) {
                let cached = cached.clone();
                if fresh.is_none() {
                    self.progress.cache_hits += 1;
                }
                self.seen.insert(path.clone(), cached.clone());
                return self.found(path, disk_name, &cached);
            }
        }
        for _attempt in 0..3 {
            let file = match open_at(parent, name, libc::O_RDONLY, &path) {
                Ok(Some(file)) => file,
                Ok(None) => return Ok(()), // Removed since it was listed.
                Err(error) => {
                    self.incomplete(error);
                    return Ok(());
                }
            };
            let Some(before) = fstat(&file) else {
                self.incomplete(format!("inspecting {path:?} failed"));
                return Ok(());
            };
            if kind_of(&before) != libc::S_IFREG
                || before.st_dev != listed.st_dev
                || before.st_ino != listed.st_ino
            {
                return self.skipped(path, EntryKind::Unstable, "replaced during the manifest");
            }
            let mut hasher = blake3::Hasher::new();
            let mut buffer = vec![0u8; 256 * 1024];
            let mut reader = &file;
            let mut read = 0u64;
            loop {
                let count = match reader.read(&mut buffer) {
                    Ok(count) => count,
                    Err(error) => {
                        self.incomplete(format!("reading {path:?} failed: {error}"));
                        return Ok(());
                    }
                };
                if count == 0 {
                    break;
                }
                read += count as u64;
                hasher.update(&buffer[..count]);
                self.heartbeat()?;
            }
            #[cfg(test)]
            DURING_HASH.with(|hook| {
                if let Some(hook) = hook.borrow_mut().as_mut() {
                    hook(&path)
                }
            });
            let Some(after) = fstat(&file) else {
                self.incomplete(format!("inspecting {path:?} failed"));
                return Ok(());
            };
            let (start, end) = (Key::of(&before), Key::of(&after));
            if start != end || read != end.size {
                continue;
            }
            self.progress.hashed += 1;
            self.progress.hashed_bytes += read;
            let hash = *hasher.finalize().as_bytes();
            #[allow(clippy::unnecessary_cast)]
            let cached = CachedFile {
                dev: end.dev,
                ino: end.ino,
                size: end.size,
                mtime_ns: end.mtime_ns,
                ctime_ns: end.ctime_ns,
                hash: hex(&hash),
                mode: normalized_mode(after.st_mode as u32),
                verified_at_ns: self.started_ns,
            };
            self.hashed.insert(path.clone(), cached.clone());
            self.seen.insert(path.clone(), cached.clone());
            return self.found(path, disk_name, &cached);
        }
        self.skipped(
            path,
            EntryKind::Unstable,
            "kept changing while it was hashed; not cached",
        )
    }

    /// Everything under the worktree-relative directory `root`. The pending
    /// stack holds paths, never open descriptors: one directory is open at a
    /// time however wide the tree is.
    fn tree(&mut self, worktree: &File, root: String) -> Result<(), String> {
        let mut pending = vec![root];
        while let Some(prefix) = pending.pop() {
            let directory = match open_directory_at(worktree, &prefix) {
                Ok(Some(directory)) => directory,
                Ok(None) => continue,
                Err(error) => {
                    self.incomplete(error);
                    continue;
                }
            };
            let (names, git) = match entries(&directory) {
                Ok(listing) => listing,
                Err(error) => {
                    self.incomplete(format!("listing {prefix:?} failed: {error}"));
                    continue;
                }
            };
            if git {
                self.skipped(
                    prefix,
                    EntryKind::NestedRepo,
                    "a nested Git repository is skipped",
                )?;
                continue;
            }
            let mut normalized = HashSet::new();
            for raw in names {
                let Ok(name) = std::str::from_utf8(&raw) else {
                    let lossy = String::from_utf8_lossy(&raw).into_owned();
                    self.skipped(
                        format!("{prefix}/{lossy}"),
                        EntryKind::InvalidName,
                        "name is not UTF-8",
                    )?;
                    continue;
                };
                // Compared in NFC: a name that is not NFC is listed by
                // its NFC form, with its spelling on disk.
                let normal = nfc(name);
                let relative = format!("{prefix}/{normal}");
                if !normalized.insert(normal.clone()) {
                    self.skipped(
                        relative,
                        EntryKind::InvalidName,
                        "another name here has the same NFC form",
                    )?;
                    continue;
                }
                let disk_name = (normal != name).then(|| name.to_string());
                let component = CString::new(raw.clone()).map_err(|_| "invalid entry name")?;
                let stat = match stat_at(&directory, &component, &relative) {
                    Ok(Some(stat)) => stat,
                    Ok(None) => continue, // Gone since the listing.
                    Err(error) => {
                        self.incomplete(error);
                        continue;
                    }
                };
                match kind_of(&stat) {
                    libc::S_IFREG => {
                        self.file(&directory, &component, relative, disk_name, &stat)?
                    }
                    libc::S_IFLNK => self.skipped(
                        relative,
                        EntryKind::Symlink,
                        "symlinks are reported, never followed",
                    )?,
                    libc::S_IFDIR if disk_name.is_some() => self.skipped(
                        relative,
                        EntryKind::InvalidName,
                        "a directory whose name is not NFC is not walked",
                    )?,
                    libc::S_IFDIR => pending.push(relative),
                    _ => self.skipped(relative, EntryKind::Other, "not a regular file")?,
                }
            }
        }
        Ok(())
    }

    /// One declared prefix: a directory walked whole, or a single file.
    fn prefix(&mut self, worktree: &File, prefix: &str) -> Result<(), String> {
        let (parent, leaf) = match prefix.rsplit_once('/') {
            Some((parent, leaf)) => (Some(parent), leaf),
            None => (None, prefix),
        };
        let directory = match parent {
            Some(parent) => match open_directory_at(worktree, parent) {
                Ok(Some(directory)) => directory,
                Ok(None) => return Ok(()), // Absent: nothing under it.
                Err(error) => {
                    self.incomplete(error);
                    return Ok(());
                }
            },
            None => io(worktree.try_clone())?,
        };
        let component = CString::new(leaf).map_err(|_| "invalid context prefix")?;
        let stat = match stat_at(&directory, &component, prefix) {
            Ok(Some(stat)) => stat,
            Ok(None) => return Ok(()),
            Err(error) => {
                self.incomplete(error);
                return Ok(());
            }
        };
        match kind_of(&stat) {
            libc::S_IFDIR => self.tree(worktree, prefix.to_string()),
            libc::S_IFREG => self.file(&directory, &component, prefix.to_string(), None, &stat),
            libc::S_IFLNK => self.skipped(
                prefix.into(),
                EntryKind::Symlink,
                "symlinks are reported, never followed",
            ),
            _ => self.skipped(prefix.into(), EntryKind::Other, "not a directory or a file"),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn hex_hash(value: &str) -> Result<[u8; 32], String> {
    if value.len() != 64 {
        return Err("invalid cached hash".into());
    }
    let mut hash = [0u8; 32];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "invalid cached hash")?;
    }
    Ok(hash)
}

/// The filesystem a worktree lives on: if this changes (restored from a
/// backup, moved disks), every cached key is meaningless.
fn fs_id(worktree: &File) -> Result<String, String> {
    let stat = fstat(worktree).ok_or("remote worktree cannot be inspected")?;
    let mut vfs = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: held descriptor and writable storage.
    if unsafe { libc::fstatvfs(worktree.as_raw_fd(), vfs.as_mut_ptr()) } != 0 {
        return Err("remote worktree filesystem cannot be inspected".into());
    }
    // SAFETY: initialized by the successful call above.
    let vfs = unsafe { vfs.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    Ok(format!("{}:{}", stat.st_dev as u64, vfs.f_fsid as u64))
}

fn cache_path(root: &Path, worktree_path: &str) -> (std::path::PathBuf, String) {
    let key = digest(locks::canonical(worktree_path).as_bytes());
    (root.join(CACHE_DIR).join(format!("{key}.json")), key)
}
/// The previous cache, or an empty one when it is missing, unreadable, from
/// another version or from another filesystem.
fn load(path: &Path, fs_id: &str) -> CacheFile {
    let empty = CacheFile {
        version: CACHE_VERSION,
        fs_id: fs_id.into(),
        ..CacheFile::default()
    };
    let Ok(bytes) = std::fs::read(path) else {
        return empty;
    };
    match serde_json::from_slice::<CacheFile>(&bytes) {
        Ok(cache) if cache.version == CACHE_VERSION && cache.fs_id == fs_id => cache,
        Ok(_) => empty,
        Err(_) => {
            log::warn!("remote manifest cache did not parse; rebuilding it");
            empty
        }
    }
}
/// Replace the cache file atomically: a reader sees the old or the new one.
fn store(path: &Path, cache: &CacheFile) -> Result<(), String> {
    let directory = path.parent().ok_or("invalid manifest cache path")?;
    io(std::fs::create_dir_all(directory))?;
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        uuid::Uuid::new_v4().simple()
    ));
    let bytes = serde_json::to_vec(cache).map_err(|_| "cannot encode manifest cache")?;
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        super::durable::sync(&file)?;
        std::fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    io(written)
}

/// Git-index paths under the prefixes, for the `tracked` flags. A
/// destination that is not a Git checkout tracks nothing.
fn tracked(
    worktree: &File,
    prefixes: &[String],
    alive: &dyn Fn() -> bool,
) -> Result<HashSet<String>, String> {
    if stat_at(worktree, c".git", ".git")?.is_none() {
        return Ok(HashSet::new());
    }
    let mut args = vec!["--literal-pathspecs", "ls-files", "-z", "--cached", "--"];
    args.extend(prefixes.iter().map(String::as_str));
    let bytes = git_output(
        worktree,
        &args,
        alive,
        Instant::now() + TRACKED_DEADLINE,
        TRACKED_BYTES,
    )?;
    Ok(bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| std::str::from_utf8(path).ok().map(nfc))
        .collect())
}

fn under(prefixes: &[String], cached: &str) -> bool {
    prefixes.iter().any(|prefix| {
        cached == prefix
            || cached
                .strip_prefix(prefix.as_str())
                .is_some_and(|tail| tail.starts_with('/'))
    })
}
/// The next cache: what this call saw under its prefixes (or, when it saw
/// only part of them, also what it did not disprove), plus every fresh hash,
/// plus other prefixes' entries while they were walked recently enough.
fn next_cache(
    previous: &CacheFile,
    prefixes: &[String],
    seen: Option<BTreeMap<String, CachedFile>>,
    hashed: BTreeMap<String, CachedFile>,
    fs_id: String,
    started_ns: i128,
) -> CacheFile {
    let mut walked = previous.walked.clone();
    let complete = seen.is_some();
    for prefix in prefixes {
        walked.insert(prefix.clone(), started_ns);
    }
    walked.retain(|_, at| *at >= started_ns - CACHE_RETENTION_NS);
    let retained: Vec<String> = walked.keys().cloned().collect();
    let mut entries = seen.unwrap_or_default();
    for (path, entry) in &previous.entries {
        let mine = under(prefixes, path);
        if (!mine || !complete) && under(&retained, path) {
            entries.entry(path.clone()).or_insert_with(|| entry.clone());
        }
    }
    entries.extend(hashed);
    if entries.len() > MAX_ENTRIES {
        // Bounded: beyond this the cache only costs, so start over.
        entries.clear();
    }
    CacheFile {
        version: CACHE_VERSION,
        fs_id,
        written_at_ns: started_ns,
        walked,
        entries,
    }
}

/// Answer one manifest request. Each [`ManifestPart`] goes to `part` (false:
/// the connection is gone) ahead of the returned reply.
pub(crate) fn handle(
    root: &Path,
    owner: u64,
    request: ManifestRequest,
    alive: impl Fn() -> bool,
    part: &mut dyn FnMut(ManifestPart) -> bool,
) -> Result<ManifestReply, String> {
    let prefixes = request.validate()?;
    let (worktree, located) = files::reader(root, owner, &request.target, &alive)?;
    let id = fs_id(&worktree)?;
    let (path, key) = cache_path(root, &located.worktree_path);
    let lock = format!("manifest-cache:{key}");
    let previous = {
        let _cache = locks::staging(&lock, &alive)?;
        load(&path, &id)
    };
    let started = now_ns();
    let mut hashed = BTreeMap::new();
    // Lock-free: a walk that overlapped a write in this worktree walks again
    // (warm by then); one that keeps overlapping answers busy, and the desktop
    // retries. Progress parts may repeat; entries are sent once, after.
    let walked = locks::consistent(&located.worktree_path, || {
        #[cfg(test)]
        EACH_ATTEMPT.with(|hook| {
            if let Some(hook) = hook.borrow_mut().as_mut() {
                hook("")
            }
        });
        let mut walk = Walk {
            previous: &previous,
            seen: BTreeMap::new(),
            hashed: &mut hashed,
            started_ns: started,
            entries: Vec::new(),
            progress: ManifestProgress::default(),
            limit: request.limit,
            incomplete: None,
            last_part: Instant::now(),
            part: &mut *part,
            alive: &alive,
        };
        for prefix in &prefixes {
            match walk.prefix(&worktree, prefix) {
                Ok(()) => {}
                Err(error) if error == LIMIT => break,
                Err(error) => return Err(error),
            }
        }
        Ok((walk.entries, walk.seen, walk.progress, walk.incomplete))
    });
    let (mut entries, seen, progress, mut incomplete) = match walked {
        Ok(walked) => walked,
        Err(error) => {
            // Busy (or cut): every stable hash computed so far still stands.
            if !hashed.is_empty() {
                let cache = next_cache(&previous, &prefixes, None, hashed, id, started);
                let _cache = locks::staging(&lock, &alive)?;
                if let Err(error) = store(&path, &cache) {
                    log::warn!("remote manifest cache not saved: {error}");
                }
            }
            return Err(error);
        }
    };
    match tracked(&worktree, &prefixes, &alive) {
        Ok(index) => {
            for entry in &mut entries {
                entry.tracked = index.contains(&entry.path);
            }
        }
        Err(error) => {
            incomplete.get_or_insert(format!("the Git index is unavailable: {error}"));
        }
    }
    let changed = !hashed.is_empty()
        || incomplete.is_none()
            && previous
                .entries
                .keys()
                .any(|cached| under(&prefixes, cached) && !seen.contains_key(cached))
        || prefixes
            .iter()
            .any(|prefix| !previous.walked.contains_key(prefix));
    if changed {
        let seen = incomplete.is_none().then_some(seen);
        let cache = next_cache(&previous, &prefixes, seen, hashed, id, started);
        let _cache = locks::staging(&lock, &alive)?;
        if let Err(error) = store(&path, &cache) {
            // The manifest stands; only the next one is slower.
            log::warn!("remote manifest cache not saved: {error}");
        }
    }
    let total = entries.len() as u64;
    for chunk in entries.chunks(ENTRIES_PER_PART) {
        if !part(ManifestPart {
            progress,
            entries: chunk.to_vec(),
            contents: Vec::new(),
        }) {
            return Err(CLOSED.into());
        }
    }
    let (read, unread) = if request.read.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        stream_contents(&worktree, &entries, &request.read, progress, &alive, part)?
    };
    if !alive() {
        return Err(RETIRED.into());
    }
    let workspace_exclude = request
        .workspace_id
        .as_deref()
        .map(|id| files::workspace_exclude_state(&located.repo_path, id));
    Ok(ManifestReply {
        complete: incomplete.is_none(),
        incomplete_reason: incomplete,
        entries: total,
        progress,
        prefixes,
        workspace_exclude,
        read,
        unread,
    })
}

/// The bytes of every file `read` names, streamed after the
/// entries in parts of at most [`READ_CHUNK_BYTES`]. Each file is read
/// through no-follow descriptors and re-hashed: one that changed since the
/// walk hashed it is reported unread, never sent with a stale hash. A file
/// the walk did not list as a regular file, or one too large for a batch
/// member, is unread too. Asking for more than [`MAX_READ_BYTES`] fails the
/// call before anything is read.
fn stream_contents(
    worktree: &File,
    entries: &[ManifestEntry],
    read: &[String],
    progress: ManifestProgress,
    alive: &dyn Fn() -> bool,
    part: &mut dyn FnMut(ManifestPart) -> bool,
) -> Result<(Vec<String>, Vec<UnreadFile>), String> {
    let listed: BTreeMap<&str, &ManifestEntry> = entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::File)
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let mut wanted: Vec<&str> = read.iter().map(String::as_str).collect();
    wanted.sort_unstable();
    wanted.dedup();
    let total: u64 = wanted
        .iter()
        .filter_map(|path| listed.get(path))
        .map(|entry| entry.size)
        .sum();
    if total > MAX_READ_BYTES {
        return Err(format!(
            "reading {total} bytes at once is over the {MAX_READ_BYTES}-byte limit; name fewer files"
        ));
    }
    let mut sent = Vec::new();
    let mut unread = Vec::new();
    let mut pending: Vec<ManifestContent> = Vec::new();
    let mut pending_bytes = 0usize;
    let mut flush = |pending: &mut Vec<ManifestContent>, pending_bytes: &mut usize| {
        if pending.is_empty() {
            return Ok(());
        }
        *pending_bytes = 0;
        if part(ManifestPart {
            progress,
            entries: Vec::new(),
            contents: std::mem::take(pending),
        }) {
            Ok(())
        } else {
            Err(CLOSED.to_string())
        }
    };
    for path in wanted {
        if !alive() {
            return Err(RETIRED.into());
        }
        let Some(entry) = listed.get(path) else {
            unread.push(UnreadFile {
                path: path.into(),
                reason: "not a regular file in the workspace".into(),
            });
            continue;
        };
        if entry.oversize || entry.size > MAX_FILE_BYTES as u64 {
            unread.push(UnreadFile {
                path: path.into(),
                reason: "too large to publish from a machine".into(),
            });
            continue;
        }
        let bytes = match read_listed(worktree, entry) {
            Ok(bytes) => bytes,
            Err(reason) => {
                unread.push(UnreadFile {
                    path: path.into(),
                    reason,
                });
                continue;
            }
        };
        let mut offset = 0usize;
        loop {
            let room = READ_CHUNK_BYTES - pending_bytes;
            let end = (offset + room).min(bytes.len());
            let last = end == bytes.len();
            pending.push(ManifestContent {
                path: path.into(),
                offset: offset as u64,
                bytes: bytes[offset..end].to_vec(),
                last,
            });
            pending_bytes += end - offset;
            offset = end;
            if pending_bytes >= READ_CHUNK_BYTES {
                flush(&mut pending, &mut pending_bytes)?;
            }
            if last {
                break;
            }
        }
        sent.push(path.to_string());
    }
    flush(&mut pending, &mut pending_bytes)?;
    Ok((sent, unread))
}

/// One listed file's bytes, read through no-follow descriptors and checked
/// against the hash the walk reported.
fn read_listed(worktree: &File, entry: &ManifestEntry) -> Result<Vec<u8>, String> {
    let (parent, leaf) = match entry.path.rsplit_once('/') {
        Some((parent, leaf)) => (Some(parent), leaf),
        None => (None, entry.path.as_str()),
    };
    let leaf = entry.disk_name.as_deref().unwrap_or(leaf);
    let directory = match parent {
        Some(parent) => open_directory_at(worktree, parent)?.ok_or("it is gone")?,
        None => io(worktree.try_clone())?,
    };
    let name = CString::new(leaf).map_err(|_| "invalid file name")?;
    let file = open_at(&directory, &name, libc::O_RDONLY, &entry.path)?.ok_or("it is gone")?;
    let stat = fstat(&file).ok_or("it cannot be inspected")?;
    if kind_of(&stat) != libc::S_IFREG {
        return Err("it is no longer a regular file".into());
    }
    let mut bytes = Vec::with_capacity(entry.size as usize);
    (&file)
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("reading it failed: {error}"))?;
    if Some(*blake3::hash(&bytes).as_bytes()) != entry.hash {
        return Err("it changed while it was read".into());
    }
    Ok(bytes)
}

/// The frame carrying one streamed part of call `rid`'s manifest. The host
/// queues it on the caller's connection ahead of the call's reply.
pub(crate) fn part_frame(rid: u64, part: ManifestPart) -> Option<codesense_host_proto::Frame> {
    use codesense_pty_core::protocol::{Message, Push};
    codesense_host_proto::Frame::control(&Message::Push {
        push: Push::ManifestPart {
            rid,
            part: Box::new(part),
        },
    })
    .map_err(|error| log::warn!("manifest part {rid} not sent: {error}"))
    .ok()
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
