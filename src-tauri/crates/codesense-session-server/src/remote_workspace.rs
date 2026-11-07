// SPDX-License-Identifier: MIT
//! Durable, private-host Git preparation. No terminal output is an operation receipt.
use codesense_pty_core::remote_workspace::{
    BundleChunk, BundleProgress, BundleUploadStatus, CheckWorktreeDirtyRemote,
    ForgetMissingWorkspace, InspectRepository, PrepareWorkspace, PreparedWorkspace, ReceivedChunks,
    RemoteWorktreeDirtyStatus, RemoveWorktree, RemoveWorktreeResult, RepositoryKnowledge,
    StagedBundleChunk, BUNDLE_CHUNK_BYTES, DIRTY_PATHS_WIRE_CAP, MAX_BUNDLE_BYTES,
    PIPELINED_CHUNK_BYTES, WORKSPACE_MISSING, WORKSPACE_OCCUPIED, WORKSPACE_UNSAFE,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) mod base_update;
pub(crate) mod bundle_staging;
pub(crate) mod context_batch;
pub(crate) mod diff;
pub(crate) mod durable;
pub(crate) mod executor;
pub(crate) mod export;
pub(crate) mod files;
pub(crate) mod locks;
pub(crate) mod manifest;
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod test_support;

// Test-only pause points: a test arms one on the thread that will run the
// operation, and the operation calls it at that point, still holding whatever
// it holds there. Barrier-based concurrency tests use these instead of timing.
#[cfg(test)]
type TestHold = (&'static str, Box<dyn FnOnce()>);
#[cfg(test)]
thread_local! {
    static TEST_HOLDS: std::cell::RefCell<Vec<TestHold>> =
        const { std::cell::RefCell::new(Vec::new()) };
}
#[cfg(test)]
pub(crate) fn arm_test_hold(point: &'static str, hold: impl FnOnce() + 'static) {
    TEST_HOLDS.with(|holds| holds.borrow_mut().push((point, Box::new(hold))));
}
#[cfg(test)]
fn test_hold(point: &'static str) {
    let hold = TEST_HOLDS.with(|holds| {
        let mut holds = holds.borrow_mut();
        let at = holds.iter().position(|(armed, _)| *armed == point)?;
        Some(holds.remove(at).1)
    });
    if let Some(hold) = hold {
        hold();
    }
}
#[cfg(not(test))]
#[inline(always)]
fn test_hold(_point: &'static str) {}
const LIMIT: usize = 2 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    intent: PrepareWorkspace,
    handle: String,
    clone_started: bool,
    worktree_started: bool,
    #[serde(default)]
    import_started: bool,
    commit: Option<String>,
    complete: bool,
    #[serde(default)]
    file_operation_generation: Option<String>,
    /// The base commit the worktree was last moved to by
    /// `update_workspace_base`; reported as the resolved commit from then on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_commit: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct WorktreeOwnership {
    origin_key: String,
    repository_key: String,
    branch: String,
    commit: String,
    request_id: String,
    complete: bool,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn io<T>(value: std::io::Result<T>) -> Result<T, String> {
    value.map_err(|_| "remote workspace filesystem unavailable".into())
}
fn name(value: &str) -> Result<CString, String> {
    if value.is_empty() || value == "." || value == ".." || value.contains('/') {
        return Err("invalid path component".into());
    }
    CString::new(value).map_err(|_| "invalid path component".into())
}
fn open(parent: &File, component: &str, flags: i32) -> Result<File, String> {
    let component = name(component)?;
    // SAFETY: held parent descriptor and valid component; successful FD has one owner.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            component.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        let errno = std::io::Error::last_os_error().raw_os_error();
        // Linux returns ENOTDIR for O_DIRECTORY | O_NOFOLLOW on a symlink.
        let symlink = if errno == Some(libc::ENOTDIR) {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: held parent, valid component and writable stat; no links followed.
            unsafe {
                libc::fstatat(
                    parent.as_raw_fd(),
                    component.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                ) == 0
                    && stat.assume_init().st_mode & libc::S_IFMT == libc::S_IFLNK
            }
        } else {
            false
        };
        let code = match errno {
            Some(libc::ENOENT) => WORKSPACE_MISSING,
            Some(libc::ELOOP | libc::EACCES | libc::EPERM) => WORKSPACE_UNSAFE,
            Some(libc::ENOTDIR) if symlink => WORKSPACE_UNSAFE,
            Some(libc::ENOTDIR | libc::EEXIST) => WORKSPACE_OCCUPIED,
            _ => return Err("remote workspace filesystem unavailable".into()),
        };
        return Err(format!("{code} remote path cannot be opened"));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn directory(parent: &File, component: &str, create: bool) -> Result<File, String> {
    if create {
        let c = name(component)?;
        // SAFETY: held parent and validated component.
        let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), c.as_ptr(), 0o700) };
        if rc != 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
            return Err("cannot create remote directory".into());
        }
    }
    open(parent, component, libc::O_RDONLY | libc::O_DIRECTORY)
}
/// [`directory`] with `create`, also saying whether this call created it
/// (whose parent then needs a sync before anything relies on it).
fn directory_created(parent: &File, component: &str) -> Result<(File, bool), String> {
    let c = name(component)?;
    // SAFETY: held parent and validated component.
    let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), c.as_ptr(), 0o700) };
    if rc != 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::AlreadyExists {
        return Err("cannot create remote directory".into());
    }
    Ok((
        open(parent, component, libc::O_RDONLY | libc::O_DIRECTORY)?,
        rc == 0,
    ))
}
fn absolute(path: &str) -> Result<(), String> {
    if !path.starts_with('/')
        || path.len() > 2048
        || path.contains('\0')
        || path == "/"
        || path.ends_with('/')
        || path
            .split('/')
            .skip(1)
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        return Err("remote destination must be an absolute normalized path".into());
    }
    Ok(())
}
fn open_directory(path: &str, create: bool) -> Result<File, String> {
    absolute(path)?;
    let mut fd = io(File::open("/"))?;
    for component in Path::new(path).components() {
        if let Component::Normal(component) = component {
            fd = directory(&fd, component.to_str().ok_or("invalid path")?, create)?;
        }
    }
    Ok(fd)
}
/// The parent directory of a managed destination. Under the account's own
/// `~/codesense`, missing levels are created (owner-only) and every level is
/// held to the owner check, healing a group/other-writable level the account
/// itself made (an umask 0002 shell), so a destination the desktop derived
/// from this host's home needs no setup first and no other account can swap a
/// level under it. Anywhere else the parent must already exist.
fn destination_parent(path: &str, home: Option<&Path>) -> Result<File, String> {
    let parent = Path::new(path)
        .parent()
        .and_then(Path::to_str)
        .ok_or("missing destination parent")?;
    let managed = home.and_then(|home| {
        Path::new(parent)
            .strip_prefix(home.join("codesense"))
            .ok()
            .map(|rest| (home, rest))
    });
    let Some((home, rest)) = managed else {
        return open_directory(parent, false);
    };
    let mut fd = open_directory(home.to_str().ok_or("invalid home")?, false)?;
    fd = directory(&fd, "codesense", true)?;
    owned_level(&fd)?;
    for component in rest.components() {
        let Component::Normal(component) = component else {
            return Err("remote destination must be an absolute normalized path".into());
        };
        fd = directory(&fd, component.to_str().ok_or("invalid path")?, true)?;
        owned_level(&fd)?;
    }
    Ok(fd)
}
/// One managed level: owned by this account, and not writable by group or
/// others (tightened in place when this account owns it).
fn owned_level(fd: &File) -> Result<(), String> {
    let meta = io(fd.metadata())?;
    // SAFETY: geteuid has no pointer preconditions.
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err(format!(
            "{WORKSPACE_UNSAFE} remote directory must be owned and not writable by others"
        ));
    }
    if meta.mode() & 0o022 != 0 {
        let mode = (meta.mode() & 0o7777 & !0o022) as libc::mode_t;
        // SAFETY: held directory descriptor; fchmod has no pointer arguments.
        if unsafe { libc::fchmod(fd.as_raw_fd(), mode) } != 0 {
            return Err(format!(
                "{WORKSPACE_UNSAFE} remote directory must be owned and not writable by others"
            ));
        }
    }
    owner_directory(fd)
}
/// The account's home with any symlink resolved, when it is an absolute,
/// existing directory.
pub(crate) fn resolved_account_home() -> Option<std::path::PathBuf> {
    dirs::home_dir()
        .and_then(|home| std::fs::canonicalize(home).ok())
        .filter(|home| home.is_absolute() && home.is_dir())
}
fn owner_directory(fd: &File) -> Result<(), String> {
    let meta = io(fd.metadata())?;
    // SAFETY: geteuid has no pointer preconditions.
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
        return Err(format!(
            "{WORKSPACE_UNSAFE} remote directory must be owned and not writable by others"
        ));
    }
    Ok(())
}
fn exists(parent: &File, component: &str) -> Result<bool, String> {
    let c = name(component)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: writable stat storage, held parent, validated name. Do not follow links.
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            c.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        return Ok(true);
    }
    if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
        Ok(false)
    } else {
        Err("cannot inspect remote path".into())
    }
}
fn read(parent: &File, component: &str) -> Result<Vec<u8>, String> {
    // A record being replaced is briefly unlinked (0 links) under a reader that
    // opened it; a fallback hard-link publish briefly has 2. Both settle within
    // the writer's commit, so look again before calling the file unsafe.
    let mut backoff = Duration::from_millis(1);
    let file = loop {
        let file = open(parent, component, libc::O_RDONLY | libc::O_NONBLOCK)?;
        let metadata = io(file.metadata())?;
        let links = metadata.nlink();
        if metadata.is_file() && (links == 0 || links == 2) && backoff < Duration::from_millis(64) {
            std::thread::sleep(backoff);
            backoff *= 2;
            continue;
        }
        if !metadata.is_file() || metadata.len() > LIMIT as u64 || links != 1 {
            return Err("unsafe or oversized remote file".into());
        }
        break file;
    };
    let mut bytes = Vec::new();
    io(file.take((LIMIT + 1) as u64).read_to_end(&mut bytes))?;
    if bytes.len() > LIMIT {
        return Err("remote file exceeds limit".into());
    }
    Ok(bytes)
}
fn write_new(parent: &File, component: &str, bytes: &[u8]) -> Result<(), String> {
    stage(parent, component, bytes)?;
    io(durable::sync(parent))
}
/// Create `component` with `bytes`, its data durable but its name
/// not yet: for a staged file that is renamed into place next, whose rename
/// is what the directory sync makes durable.
fn stage(parent: &File, component: &str, bytes: &[u8]) -> Result<(), String> {
    let mut file = open(
        parent,
        component,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
    )?;
    io(file.write_all(bytes))?;
    io(durable::sync(&file))
}
/// How a no-replace rename went.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NoReplace {
    Renamed,
    /// The destination already existed; nothing moved.
    Exists,
    /// This filesystem or kernel has no no-replace rename.
    Unsupported,
}
/// Rename `from` to `to` under one held directory, refusing to replace `to`.
/// The file keeps a single link throughout (no hard-link window).
pub(crate) fn rename_no_replace(parent: &File, from: &str, to: &str) -> Result<NoReplace, String> {
    let from = name(from)?;
    let to = name(to)?;
    #[cfg(target_os = "linux")]
    // SAFETY: two validated components under the same held directory.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        ) as libc::c_int
    };
    #[cfg(target_os = "macos")]
    // SAFETY: two validated components under the same held directory.
    let rc = unsafe {
        libc::renameatx_np(
            parent.as_raw_fd(),
            from.as_ptr(),
            parent.as_raw_fd(),
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let rc: libc::c_int = {
        let _ = (&from, &to);
        return Ok(NoReplace::Unsupported);
    };
    if rc == 0 {
        return Ok(NoReplace::Renamed);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::EEXIST) => Ok(NoReplace::Exists),
        Some(libc::EINVAL | libc::ENOSYS | libc::ENOTSUP) => Ok(NoReplace::Unsupported),
        #[allow(
            unreachable_patterns,
            reason = "EOPNOTSUPP equals ENOTSUP on some targets"
        )]
        Some(libc::EOPNOTSUPP) => Ok(NoReplace::Unsupported),
        _ => Err("remote write could not commit".into()),
    }
}
/// Remove our own staged file, best effort (an error path's cleanup).
pub(crate) fn discard(parent: &File, component: &str) {
    if let Ok(component) = name(component) {
        // SAFETY: our own staged name under the held directory.
        unsafe { libc::unlinkat(parent.as_raw_fd(), component.as_ptr(), 0) };
    }
}
/// Create `component` with `bytes` all at once, or not at all: readers never
/// see it partly written or doubly linked, and when another writer created it
/// first this returns `false` and leaves theirs. (A staged private file, then
/// a no-replace rename; where that rename is unsupported, an exclusive create.)
/// Every error path removes the staged file.
fn publish_new(parent: &File, component: &str, bytes: &[u8]) -> Result<bool, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let staged = format!(
        ".workspace-new-{}",
        digest(
            format!(
                "{component}:{}:{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            )
            .as_bytes()
        )
    );
    if let Err(error) = stage(parent, &staged, bytes) {
        discard(parent, &staged);
        return Err(error);
    }
    match rename_no_replace(parent, &staged, component) {
        Ok(NoReplace::Renamed) => {
            io(durable::sync(parent))?;
            Ok(true)
        }
        Ok(NoReplace::Exists) => {
            discard(parent, &staged);
            Ok(false)
        }
        Ok(NoReplace::Unsupported) => {
            discard(parent, &staged);
            match write_new(parent, component, bytes) {
                Ok(()) => Ok(true),
                Err(_) if exists(parent, component)? => Ok(false),
                Err(error) => Err(error),
            }
        }
        Err(error) => {
            discard(parent, &staged);
            Err(error)
        }
    }
}
fn replace(parent: &File, component: &str, bytes: &[u8]) -> Result<(), String> {
    let tmp = format!(
        ".workspace-write-{}",
        digest(&[component.as_bytes(), &std::process::id().to_ne_bytes()].concat())
    );
    if exists(parent, &tmp)? {
        return Err("prior remote write requires reconciliation".into());
    }
    stage(parent, &tmp, bytes)?;
    let src = name(&tmp)?;
    let dst = name(component)?;
    // SAFETY: both names are components under the same held directory.
    let rc = unsafe {
        libc::renameat(
            parent.as_raw_fd(),
            src.as_ptr(),
            parent.as_raw_fd(),
            dst.as_ptr(),
        )
    };
    if rc != 0 {
        return Err("remote write could not commit".into());
    }
    io(durable::sync(parent))
}
fn save(root: &File, key: &str, receipt: &Receipt, first: bool) -> Result<(), String> {
    let bytes = serde_json::to_vec(receipt).map_err(|_| "cannot encode workspace receipt")?;
    if first {
        if publish_new(root, key, &bytes)? {
            Ok(())
        } else {
            Err(format!(
                "{WORKSPACE_OCCUPIED} workspace receipt already exists"
            ))
        }
    } else {
        replace(root, key, &bytes)
    }
}
// Optional exit notification only: unlike the MCP supervisor capability, Git
// does not require pidfd signaling support. Older kernels retain timed polling.
#[cfg(target_os = "linux")]
fn git_exit_fd(pid: u32) -> Option<OwnedFd> {
    // SAFETY: pidfd_open has no pointer arguments; success returns an owned fd.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

#[cfg(not(target_os = "linux"))]
fn git_exit_fd(_pid: u32) -> Option<OwnedFd> {
    None
}

fn wait_git_exit(fd: Option<&OwnedFd>, timeout: Duration) -> bool {
    let started = Instant::now();
    if let Some(fd) = fd {
        let mut descriptor = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: descriptor is valid for this call and the fd remains owned.
        let result = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                timeout.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if result >= 0 && descriptor.revents & (libc::POLLERR | libc::POLLNVAL) == 0 {
            return descriptor.revents & libc::POLLIN != 0;
        }
        if result < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            return false; // Recheck authority/deadline before another wait.
        }
    }
    // Unsupported notification (or poll failure) keeps the previous bounded
    // sleep behavior; time already spent polling is not added to the interval.
    std::thread::sleep(timeout.saturating_sub(started.elapsed()));
    false
}

/// A read that ran out of its deadline with authority still live: refused
/// before a call starts (`out_of_time`), or a read-only diff killed mid-run
/// (`diff::reworded`). The desktop reads it as a retryable failure (it names
/// no authority).
pub(crate) const GIT_TIMED_OUT: &str = "remote Git took too long to answer; retry";
/// A Git call killed mid-run (deadline or retired authority): a mutating
/// call's outcome is unknown, so the original request must be reconciled.
pub(crate) const GIT_INTERRUPTED: &str = "Git interrupted; reconcile the original request";
/// The workspace's authority retired: the desktop reads it as access loss.
pub(crate) const AUTHORITY_RETIRED: &str = "remote workspace authority retired";

/// Refuse to start another Git call once the authority has retired or the
/// operation's deadline has passed. The two read differently on the desktop:
/// retired authority is an access failure, a deadline a retryable timeout
/// (never "no longer have access" for a slow diff).
fn out_of_time(alive: &dyn Fn() -> bool, deadline: Instant) -> Result<(), String> {
    if !alive() {
        return Err(AUTHORITY_RETIRED.into());
    }
    if Instant::now() >= deadline {
        return Err(GIT_TIMED_OUT.into());
    }
    Ok(())
}

fn git_output(
    cwd: &File,
    args: &[&str],
    alive: &dyn Fn() -> bool,
    deadline: Instant,
    limit: usize,
) -> Result<Vec<u8>, String> {
    out_of_time(alive, deadline)?;
    let cwd = io(cwd.try_clone())?;
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "core.fsmonitor=false",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o StrictHostKeyChecking=yes",
        )
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // SAFETY: only async-signal-safe syscalls between fork and exec; cwd FD is held.
    unsafe {
        command.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 || libc::fchdir(cwd.as_raw_fd()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| "Git is unavailable on this machine")?;
    let exit_fd = git_exit_fd(child.id());
    let mut stdout = child.stdout.take().ok_or("Git output unavailable")?;
    let output = std::thread::spawn(move || {
        let mut result = Vec::new();
        let mut buffer = [0; 8192];
        let mut overflow = false;
        while let Ok(n) = stdout.read(&mut buffer) {
            if n == 0 {
                break;
            }
            overflow |= result.len().saturating_add(n) > limit;
            if result.len() < limit {
                result.extend_from_slice(&buffer[..n.min(limit - result.len())]);
            }
        }
        (result, overflow)
    });
    let status = loop {
        if !alive() || Instant::now() >= deadline {
            // SAFETY: child owns this fresh process group; negative PID targets it.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            let _ = output.join();
            // Shared by mutating calls, whose outcome is now unknown: reconcile.
            // The read-only diff re-words it as a timeout (`diff::reworded`).
            return Err(GIT_INTERRUPTED.into());
        }
        if let Some(status) = io(child.try_wait())? {
            break status;
        }
        wait_git_exit(
            exit_fd.as_ref(),
            Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
        );
    };
    // SAFETY: this child created a private process group; reap any remaining Git helper.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let (bytes, overflow) = output.join().map_err(|_| "Git output unavailable")?;
    if !status.success() {
        return Err(
            "Git refused the operation; check remote Git credentials, ref and destination".into(),
        );
    }
    if overflow {
        return Err("Git output exceeds bounded complete-result limit".into());
    }
    Ok(bytes)
}
fn git(
    cwd: &File,
    args: &[&str],
    alive: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<String, String> {
    String::from_utf8(git_output(cwd, args, alive, deadline, 65536)?)
        .map(|s| s.trim().into())
        .map_err(|_| "invalid Git output".into())
}
fn validate(request: &PrepareWorkspace) -> Result<(), String> {
    if request.bundle.size > MAX_BUNDLE_BYTES
        || request.bundle.digest.len() != 64
        || !request.bundle.digest.bytes().all(|c| c.is_ascii_hexdigit())
        || request.bundle.commit.len() != 40
        || !request.bundle.commit.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err("invalid bundle identity".into());
    }
    absolute(&request.repo_path)?;
    absolute(&request.worktree_path)?;
    if request.repo_path == request.worktree_path
        || request
            .worktree_path
            .starts_with(&(request.repo_path.clone() + "/"))
        || request
            .repo_path
            .starts_with(&(request.worktree_path.clone() + "/"))
    {
        return Err("repository and worktree destinations must be separate".into());
    }
    if request.request_id.len() != 36
        || request.origin_key.len() != 64
        || request.repository_key.is_empty()
        || request.clone_url.starts_with('-')
        || request.clone_url.contains(['\n', '\r', '\0'])
        || request.base_ref.starts_with('-')
        || request.base_ref.is_empty()
        || request.base_ref.len() > 255
        || request.branch.starts_with('-')
        || request.branch.is_empty()
        || request.branch.len() > 255
    {
        return Err("invalid workspace intent".into());
    }
    if serde_json::to_vec(request)
        .map_err(|_| "invalid intent")?
        .len()
        > LIMIT / 2
    {
        return Err("workspace intent exceeds bounded request limit".into());
    }
    Ok(())
}

fn workspace_handle(request: &PrepareWorkspace) -> String {
    format!(
        "remote-workspace-{}",
        digest(format!("{}:{}", request.origin_key, request.worktree_path).as_bytes())
    )
}
/// Receipt key for one preparation request; also its bundle upload's id.
fn receipt_key(origin_key: &str, request_id: &str) -> String {
    format!(
        "{}.json",
        digest(format!("{origin_key}:{request_id}").as_bytes())
    )
}
fn reserve(root: &File, key: &str, request: &PrepareWorkspace) -> Result<Receipt, String> {
    let existing = || -> Result<Receipt, String> {
        let receipt: Receipt = serde_json::from_slice(&read(root, key)?)
            .map_err(|_| "workspace receipt unavailable")?;
        if receipt.intent != *request {
            return Err("request identity already belongs to another immutable intent".into());
        }
        Ok(receipt)
    };
    exists(root, key).and_then(|found| {
        if found {
            existing()
        } else {
            let receipt = Receipt {
                intent: request.clone(),
                handle: workspace_handle(request),
                clone_started: false,
                worktree_started: false,
                import_started: false,
                commit: None,
                complete: false,
                file_operation_generation: None,
                base_commit: None,
            };
            let bytes =
                serde_json::to_vec(&receipt).map_err(|_| "cannot encode workspace receipt")?;
            // A bundle chunk and a prepare of the same request may both be
            // first: exactly one receipt is created and the other reads it.
            if publish_new(root, key, &bytes)? {
                Ok(receipt)
            } else {
                existing()
            }
        }
    })
}

/// Reclaim upload `key`'s staging and its seal claim.
fn settle_upload(root: &File, key: &str, request: &PrepareWorkspace) -> Result<(), String> {
    release_seal(root, key, request);
    cleanup_payload(root, key)
}
fn cleanup_payload(root: &File, key: &str) -> Result<(), String> {
    let records = format!("bundle-{key}.chunks");
    if exists(root, &records)? {
        let file = open(root, &records, libc::O_RDONLY | libc::O_NONBLOCK)?;
        let meta = io(file.metadata())?;
        if !meta.is_file() || meta.nlink() != 1 {
            return Err("settled bundle staging is unsafe".into());
        }
        discard(root, &records);
    }
    bundle_staging::forget(&staging_key_in(root, key));
    let filename = format!("bundle-{key}");
    if !exists(root, &filename)? {
        return Ok(());
    }
    let file = open(root, &filename, libc::O_RDONLY | libc::O_NONBLOCK)?;
    let meta = io(file.metadata())?;
    if !meta.is_file() || meta.nlink() != 1 {
        return Err("settled bundle staging is unsafe".into());
    }
    let filename = name(&filename)?;
    // SAFETY: held private receipt directory; only this exact operation's regular payload.
    if unsafe { libc::unlinkat(root.as_raw_fd(), filename.as_ptr(), 0) } != 0 {
        return Err("settled bundle cleanup pending".into());
    }
    io(durable::sync(root))
}
fn completed(
    root: &File,
    key: &str,
    receipt: &Receipt,
    alive: &dyn Fn() -> bool,
) -> Result<PreparedWorkspace, String> {
    if !receipt.complete {
        return Err("workspace preparation has not completed".into());
    }
    let request = &receipt.intent;
    let worktree = open_directory(&request.worktree_path, false)?;
    owner_directory(&worktree)?;
    let marker = ownership_marker_name(&request.worktree_path);
    let ownership: WorktreeOwnership = serde_json::from_slice(
        &read(root, &marker).map_err(|_| "workspace ownership unavailable")?,
    )
    .map_err(|_| "workspace ownership unavailable")?;
    if !ownership.complete
        || ownership.origin_key != request.origin_key
        || ownership.repository_key != request.repository_key
        || ownership.branch != request.branch
        || ownership.commit != request.bundle.commit
    {
        return Err("workspace ownership changed".into());
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    if git(
        &worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        alive,
        deadline,
    )? != format!("{}/.git", request.repo_path)
        || git(
            &worktree,
            &["symbolic-ref", "--short", "HEAD"],
            alive,
            deadline,
        )? != request.branch
    {
        return Err("completed worktree identity changed; no automatic recreation".into());
    }
    if !alive() {
        return Err("workspace reconciliation authority retired".into());
    }
    Ok(PreparedWorkspace {
        status: "alreadyPrepared".into(),
        handle: Some(receipt.handle.clone()),
        cwd: Some(request.worktree_path.clone()),
        repo_path: Some(request.repo_path.clone()),
        repository_key: Some(request.repository_key.clone()),
        resolved_commit: receipt
            .base_commit
            .clone()
            .or_else(|| receipt.commit.clone()),
        branch: Some(request.branch.clone()),
        message: settle_upload(root, key, request).err(),
    })
}
// Read captured preparation evidence without opening its workspace directory.
fn captured_receipt(
    root: &Path,
    owner: u64,
    target: &codesense_pty_core::remote_files::FileTarget,
    alive: &dyn Fn() -> bool,
) -> Result<(File, String, Receipt), String> {
    if owner != target.owner_id
        || target.origin_key.len() != 64
        || uuid::Uuid::parse_str(&target.request_id).is_err()
        || !alive()
    {
        return Err("remote file authority unavailable".into());
    }
    let private = File::open(root).map_err(|_| "workspace private state unavailable")?;
    let receipts = directory(&private, "remote-workspaces", false)
        .map_err(|_| "workspace private state unavailable")?;
    let key = receipt_key(&target.origin_key, &target.request_id);
    let receipt: Receipt = serde_json::from_slice(
        &read(&receipts, &key).map_err(|_| "workspace receipt unavailable")?,
    )
    .map_err(|_| "workspace receipt unavailable")?;
    if receipt.handle != target.handle
        || receipt.intent.owner_id != owner
        || receipt.intent.origin_key != target.origin_key
        || receipt.intent.request_id != target.request_id
    {
        return Err("workspace handle does not belong to this captured origin".into());
    }
    Ok((receipts, key, receipt))
}

// Hydration, publication and counts verify the same completed preparation.
fn resolve_completed_target(
    root: &Path,
    owner: u64,
    target: &codesense_pty_core::remote_files::FileTarget,
    alive: &dyn Fn() -> bool,
) -> Result<(File, String, Receipt), String> {
    let (receipts, key, receipt) = captured_receipt(root, owner, target, alive)?;
    completed(&receipts, &key, &receipt, alive)?;
    Ok((receipts, key, receipt))
}

pub(crate) fn reconcile(
    root: &Path,
    owner: u64,
    request: PrepareWorkspace,
    alive: impl Fn() -> bool,
) -> Result<Option<PreparedWorkspace>, String> {
    validate(&request)?;
    if owner != request.owner_id || !alive() {
        return Err("private workspace reconciliation refused".into());
    }
    let _workspace = locks::workspace(&request.worktree_path, &alive)?;
    let root = io(File::open(root))?;
    if !exists(&root, "remote-workspaces")? {
        return Ok(None);
    }
    let root = directory(&root, "remote-workspaces", false)?;
    let key = receipt_key(&request.origin_key, &request.request_id);
    if !exists(&root, &key)? {
        return Ok(None);
    }
    let receipt: Receipt =
        serde_json::from_slice(&read(&root, &key)?).map_err(|_| "workspace receipt unavailable")?;
    if receipt.intent != request {
        return Err("request belongs to another immutable intent".into());
    }
    if !receipt.complete {
        return Ok(None);
    }
    completed(&root, &key, &receipt, &alive).map(Some)
}

pub(crate) fn inspect_repository(
    root: &Path,
    owner: u64,
    request: InspectRepository,
    alive: impl Fn() -> bool,
) -> Result<RepositoryKnowledge, String> {
    if owner != request.owner_id
        || !alive()
        || request.commit.len() != 40
        || !request.commit.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("private repository inspection refused".into());
    }
    absolute(&request.repo_path)?;
    let _repository = locks::repository(&request.repo_path, &alive)?;
    let root = io(File::open(root))?;
    if !exists(&root, "remote-workspaces")? {
        return Ok(RepositoryKnowledge::default());
    }
    let root = directory(&root, "remote-workspaces", false)?;
    let marker = format!("repo-{}.json", digest(request.repo_path.as_bytes()));
    if !exists(&root, &marker)? {
        return Ok(RepositoryKnowledge::default());
    }
    let identity = serde_json::to_vec(&(owner, &request.repository_key, &request.clone_url))
        .map_err(|_| "invalid repository identity")?;
    if read(&root, &marker)? != identity {
        return Err("repository identity changed".into());
    }
    let repo = open_directory(&request.repo_path, false)?;
    owner_directory(&repo)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    if git(
        &repo,
        &["config", "--get", "remote.origin.url"],
        &alive,
        deadline,
    )? != request.clone_url
    {
        return Err("managed repository remote identity changed".into());
    }
    let has_commit = git(
        &repo,
        &["cat-file", "-e", &format!("{}^{{commit}}", request.commit)],
        &alive,
        deadline,
    )
    .is_ok();
    let imported = git(
        &repo,
        &[
            "for-each-ref",
            "--count=32",
            "--sort=-creatordate",
            "--format=%(objectname)",
            "refs/codesense/imports/",
        ],
        &alive,
        deadline,
    )?;
    let imported_commits = imported
        .lines()
        .filter(|line| line.len() == 40 && line.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_string)
        .collect();
    if !alive() {
        return Err("repository inspection authority retired".into());
    }
    Ok(RepositoryKnowledge {
        has_commit,
        imported_commits,
    })
}

/// Tear down a managed worktree on this machine. Called by the
/// desktop-side `move_workspace` pipeline's step 3 when the source worktree
/// lives on a remote machine. Idempotent — a missing worktree returns
/// `removed: false` with a note, NOT an error, so a crash-and-reap retry is
/// safe. Holds the per-workspace lock from [`locks::workspace`] to block a
/// racing `prepare_remote_workspace` on the same path.
pub(crate) fn remove_worktree(
    root: &Path,
    owner: u64,
    request: RemoveWorktree,
    alive: impl Fn() -> bool,
    no_running_agents: impl Fn() -> Result<(), String>,
) -> Result<RemoveWorktreeResult, String> {
    remove_worktree_with(root, owner, request, alive, no_running_agents, || Ok(()))
}

fn remove_worktree_with(
    root: &Path,
    owner: u64,
    request: RemoveWorktree,
    alive: impl Fn() -> bool,
    no_running_agents: impl Fn() -> Result<(), String>,
    after_checks: impl FnOnce() -> Result<(), String>,
) -> Result<RemoveWorktreeResult, String> {
    if owner != request.owner_id || !alive() {
        return Err("private worktree removal refused".into());
    }
    absolute(&request.repo_path)?;
    absolute(&request.worktree_path)?;
    let _workspace = locks::workspace(&request.worktree_path, &alive)?;
    // Architecture: fence spawn admission before checking pending and registered sessions.
    let _removing =
        codesense_pty_core::remote_workspace::removal::begin_removal(&request.worktree_path)
            .map_err(|e| {
                codesense_pty_core::layout::source_refusal(format!(
                    "{e}; stop the agents or restart the terminal server"
                ))
            })?;
    let root_fd = io(File::open(root))?;
    let state = directory(&root_fd, "remote-workspaces", false)?;
    if !Path::new(&request.worktree_path).exists() {
        discard(&state, &ownership_marker_name(&request.worktree_path));
        return Ok(RemoveWorktreeResult {
            removed: false,
            note: Some("source already removed".into()),
        });
    }
    let worktree = open_directory(&request.worktree_path, false)?;
    owner_directory(&worktree)?;
    let deadline = Instant::now() + Duration::from_secs(60);
    if !request.discard_uncommitted
        && !git(&worktree, &["status", "--porcelain"], &alive, deadline)?.is_empty()
    {
        return Err("uncommitted changes appeared before removal; source retained".into());
    }
    // Architecture: a session started since the desktop report also retains its source.
    no_running_agents()?;
    let repo = open_directory(&request.repo_path, false)?;
    owner_directory(&repo)?;
    let branch = git(&worktree, &["symbolic-ref", "-q", "HEAD"], &alive, deadline).ok();
    // Architecture: HEAD is checked last, after dirty state and sessions, immediately before removal.
    let tip = git(&worktree, &["rev-parse", "HEAD"], &alive, deadline)?;
    if request.expected_tip.is_empty() || tip != request.expected_tip {
        return Err("source tip changed before removal; source retained".into());
    }
    after_checks()?;
    // Policy: only an explicit discard permits force; Git guards saves after the status check.
    let mut args = vec!["worktree", "remove"];
    if request.discard_uncommitted {
        args.push("--force");
    }
    args.extend(["--", request.worktree_path.as_str()]);
    git(&repo, &args, &alive, deadline)
        .map_err(|error| format!("worktree remove refused: {error}"))?;
    // Architecture: an external Git client bypasses our fence. Removing a
    // worktree retains its branch; report any commit that landed after HEAD was checked.
    let note =
        branch.and_then(
            |branch| match git(&repo, &["rev-parse", &branch], &alive, deadline) {
                Ok(tip) if tip != request.expected_tip => Some(format!(
                    "Warning: leftover tip {tip} stays on the source branch {branch}"
                )),
                Ok(_) => None,
                Err(error) => Some(format!(
                    "Warning: source branch {branch} could not be verified after removal: {error}"
                )),
            },
        );
    discard(&state, &ownership_marker_name(&request.worktree_path));
    Ok(RemoveWorktreeResult {
        removed: true,
        note,
    })
}

/// Clear only the marker of a completed workspace, after confirming absence
/// under the same lock as preparation. No workspace files or branches are removed.
pub(crate) fn forget_missing_workspace(
    root: &Path,
    owner: u64,
    request: ForgetMissingWorkspace,
    alive: impl Fn() -> bool,
) -> Result<(), String> {
    let (state, _, receipt) = captured_receipt(root, owner, &request.target, &alive)?;
    if !receipt.complete {
        return Err("workspace preparation has not completed".into());
    }
    let intent = &receipt.intent;
    let _workspace = locks::workspace(&intent.worktree_path, &alive)?;
    match open_directory(&intent.worktree_path, false) {
        Err(error) if error.starts_with(WORKSPACE_MISSING) => {}
        Ok(_) => return Err("workspace folder is there again".into()),
        Err(error) => return Err(error),
    }
    let marker = ownership_marker_name(&intent.worktree_path);
    if exists(&state, &marker)? {
        let ownership: WorktreeOwnership = serde_json::from_slice(&read(&state, &marker)?)
            .map_err(|_| "workspace ownership unavailable")?;
        if ownership.origin_key != intent.origin_key
            || ownership.repository_key != intent.repository_key
            || ownership.branch != intent.branch
            || !ownership.complete
        {
            return Err("workspace ownership changed".into());
        }
    }
    match open_directory(&intent.repo_path, false) {
        Ok(repo) => {
            owner_directory(&repo)?;
            git(
                &repo,
                &["worktree", "prune", "--expire", "now"],
                &alive,
                Instant::now() + Duration::from_secs(30),
            )?;
        }
        // A deleted clone has no worktree registration left to prune.
        Err(error) if error.starts_with(WORKSPACE_MISSING) => {}
        Err(error) => return Err(error),
    }
    if !alive() {
        return Err("workspace cleanup authority retired".into());
    }
    discard(&state, &marker);
    if exists(&state, &marker)? {
        return Err("workspace marker could not be forgotten".into());
    }
    Ok(())
}

/// One marker identity for preparation, removal and missing-folder cleanup.
pub(crate) fn ownership_marker_name(worktree_path: &str) -> String {
    format!("worktree-{}.json", digest(worktree_path.as_bytes()))
}

/// `git status --porcelain` for a managed worktree on
/// this machine. Mirrors the desktop-side `check_worktree_dirty` body with
/// the remote-side `git_bin()` indirection + lock surface. Paths are
/// truncated to [`DIRTY_PATHS_WIRE_CAP`] so a pathological working tree
/// cannot blow the frame cap.
pub(crate) fn check_worktree_dirty_remote(
    root: &Path,
    owner: u64,
    request: CheckWorktreeDirtyRemote,
    alive: impl Fn() -> bool,
) -> Result<RemoteWorktreeDirtyStatus, String> {
    // We only use `root` to prove the daemon has started; the dirty check
    // itself reads directly from the worktree.
    let _ = io(File::open(root))?;
    if owner != request.owner_id || !alive() {
        return Err("private dirty-check refused".into());
    }
    absolute(&request.repo_path)?;
    absolute(&request.worktree_path)?;
    let _workspace = locks::workspace(&request.worktree_path, &alive)?;
    let worktree = open_directory(&request.worktree_path, false)?;
    owner_directory(&worktree)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    // Architecture: preserve porcelain's leading status columns so tracked edits cannot disappear.
    let out = String::from_utf8(git_output(
        &worktree,
        &["status", "--porcelain"],
        &alive,
        deadline,
        65536,
    )?)
    .map_err(|_| "invalid Git status output")?;
    if !alive() {
        return Err("dirty-check authority retired".into());
    }
    // Porcelain lines are `XY path`, or `XY old -> new` for renames and
    // copies. Surface the destination (the useful side); the naive slice
    // approach would print `old -> new` which is not a path.
    let mut paths: Vec<String> = out
        .lines()
        .filter_map(porcelain_path)
        .filter(|p| !p.is_empty())
        .take(DIRTY_PATHS_WIRE_CAP + 1)
        .collect();
    paths.truncate(DIRTY_PATHS_WIRE_CAP);
    // Policy: a failed fetch uses cached refs and reports why, never pushes.
    let fetch_error = request
        .refresh_refs
        .then(|| {
            git(
                &worktree,
                &["fetch", "--all", "--no-tags"],
                &alive,
                deadline,
            )
            .err()
        })
        .flatten();
    // Architecture: a timed-out fetch must still permit a report from cached refs.
    let deadline = Instant::now() + Duration::from_secs(30);
    // Architecture: count against every remote-tracking ref, not only upstream.
    let source_tip = git(&worktree, &["rev-parse", "HEAD"], &alive, deadline)?;
    let unpushed_count = git(
        &worktree,
        &["rev-list", "--count", &source_tip, "--not", "--remotes"],
        &alive,
        deadline,
    )?
    .parse::<usize>()
    .map_err(|_| "invalid unpushed count")?;
    Ok(RemoteWorktreeDirtyStatus {
        source_tip,
        unpushed_count,
        fetch_error,
        dirty: !paths.is_empty(),
        paths,
    })
}

/// Extract the path (destination for renames) from a `git status --porcelain`
/// line. Lines too short to be a status entry are skipped.
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

#[allow(
    clippy::unnecessary_cast,
    reason = "statvfs field widths differ between supported Unix targets"
)]
fn headroom(file: &File, bytes: u64) -> Result<(), String> {
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: valid held descriptor and initialized output on success.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err("remote disk headroom unavailable".into());
    }
    let stat = unsafe { stat.assume_init() };
    let available = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
    if available < bytes.saturating_add(256 * 1024 * 1024) {
        return Err("remote disk lacks bundle/import headroom".into());
    }
    Ok(())
}

pub(crate) fn upload(
    root: &Path,
    owner: u64,
    chunk: BundleChunk,
    alive: impl Fn() -> bool,
) -> Result<BundleProgress, String> {
    validate(&chunk.request)?;
    if owner != chunk.request.owner_id || chunk.bytes.len() > BUNDLE_CHUNK_BYTES {
        return Err("invalid private bundle upload".into());
    }
    // Chunk staging takes no workspace or repository lock: only this
    // upload's own staging lock, so it never waits on any prepare or Apply.
    let key = receipt_key(&chunk.request.origin_key, &chunk.request.request_id);
    let _staging = locks::staging(&bundle_lock(root, &key), &alive)?;
    if !alive() {
        return Err("connection retired".into());
    }
    let root = directory(&io(File::open(root))?, "remote-workspaces", true)?;
    let receipt = reserve(&root, &key, &chunk.request)?;
    if receipt.complete {
        completed(&root, &key, &receipt, &alive)?;
        return Ok(BundleProgress {
            next_offset: chunk.request.bundle.size,
        });
    }
    let file_name = format!("bundle-{key}");
    if exists(&root, &format!("{file_name}.chunks"))? {
        return Err("bundle upload is pipelined; resume it by chunk".into());
    }
    if !exists(&root, &file_name)? {
        write_new(&root, &file_name, &[])?;
    }
    let mut file = open(&root, &file_name, libc::O_RDWR | libc::O_NONBLOCK)?;
    let meta = io(file.metadata())?;
    if !meta.is_file() || meta.nlink() != 1 || meta.len() > chunk.request.bundle.size {
        return Err("invalid bundle staging file".into());
    }
    headroom(
        &root,
        chunk
            .request
            .bundle
            .size
            .saturating_sub(meta.len())
            .saturating_mul(2),
    )?;
    if chunk.bytes.is_empty() {
        return Ok(BundleProgress {
            next_offset: meta.len(),
        });
    }
    let end = chunk
        .offset
        .checked_add(chunk.bytes.len() as u64)
        .ok_or("bundle offset overflow")?;
    if end > chunk.request.bundle.size || chunk.offset > meta.len() {
        return Err("bundle chunk outside declared range".into());
    }
    io(file.seek(SeekFrom::Start(chunk.offset)))?;
    if chunk.offset < meta.len() {
        if end > meta.len() {
            return Err("bundle chunk partially overlaps prior bytes".into());
        }
        let mut current = vec![0; chunk.bytes.len()];
        io(file.read_exact(&mut current))?;
        if current != chunk.bytes {
            return Err("bundle retry changed prior bytes".into());
        }
    } else {
        if !alive() {
            return Err("connection retired".into());
        }
        io(file.write_all(&chunk.bytes))?;
        io(durable::sync(&file))?;
    }
    Ok(BundleProgress {
        next_offset: meta.len().max(end),
    })
}

/// The in-memory key of one upload staged in the receipt directory `root`:
/// the directory's own identity plus the upload's key, so two private roots
/// never share an entry.
fn staging_key_in(root: &File, key: &str) -> String {
    match root.metadata() {
        Ok(meta) => format!("bundle:{}:{}:{key}", meta.dev(), meta.ino()),
        Err(_) => format!("bundle:?:{key}"),
    }
}

/// Open (creating on first use) the pipelined staging of upload `key`: the
/// bytes at full length and the per-chunk records. Nothing is flushed here.
fn pipelined_store(
    root: &File,
    key: &str,
    size: u64,
    create: bool,
) -> Result<Option<bundle_staging::FileStore>, String> {
    let data_name = format!("bundle-{key}");
    let records_name = format!("{data_name}.chunks");
    let chunks = ReceivedChunks::for_size(size).count() as u64;
    if !exists(root, &records_name)? {
        if !create {
            return Ok(None);
        }
        // A legacy partial upload of the same request is taken over: its
        // bytes are not recorded, so every chunk is sent again.
        if !exists(root, &data_name)? {
            drop(open(
                root,
                &data_name,
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
            )?);
        }
        drop(open(
            root,
            &records_name,
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
        )?);
    }
    let data = open(root, &data_name, libc::O_RDWR | libc::O_NONBLOCK)?;
    let records = open(root, &records_name, libc::O_RDWR | libc::O_NONBLOCK)?;
    for (file, len) in [(&data, size), (&records, chunks * 32)] {
        let meta = io(file.metadata())?;
        if !meta.is_file() || meta.nlink() != 1 || meta.len() > len {
            return Err("invalid bundle staging file".into());
        }
        if meta.len() < len {
            io(file.set_len(len))?;
        }
    }
    Ok(Some(bundle_staging::FileStore { data, records }))
}

/// The receipt directory and key of a pipelined upload, its staging lock
/// held, after the common checks every pipelined call makes.
fn pipelined_upload(
    root: &Path,
    owner: u64,
    request: &PrepareWorkspace,
    alive: &dyn Fn() -> bool,
) -> Result<(File, String, Receipt, locks::Held), String> {
    validate(request)?;
    if owner != request.owner_id || request.bundle.size == 0 {
        return Err("invalid private bundle upload".into());
    }
    let key = receipt_key(&request.origin_key, &request.request_id);
    // Staging takes no workspace or repository lock: only this
    // upload's own lock, so a chunk never waits on a prepare or an Apply.
    let held = locks::staging(&bundle_lock(root, &key), alive)?;
    if !alive() {
        return Err("connection retired".into());
    }
    let root = directory(&io(File::open(root))?, "remote-workspaces", true)?;
    let receipt = reserve(&root, &key, request)?;
    Ok((root, key, receipt, held))
}

/// Which chunks of a pipelined upload this machine holds.
pub(crate) fn bundle_status(
    root: &Path,
    owner: u64,
    request: PrepareWorkspace,
    alive: impl Fn() -> bool,
) -> Result<BundleUploadStatus, String> {
    let (root, key, receipt, _held) = pipelined_upload(root, owner, &request, &alive)?;
    let size = request.bundle.size;
    if receipt.complete || receipt.commit.is_some() {
        // Imported already: nothing more to send.
        let mut received = ReceivedChunks::for_size(size);
        (0..received.count()).for_each(|i| received.insert(i));
        return Ok(BundleUploadStatus {
            chunk_bytes: PIPELINED_CHUNK_BYTES as u32,
            received,
            sealed: true,
        });
    }
    headroom(&root, size.saturating_mul(2))?;
    let store = pipelined_store(&root, &key, size, true)?.ok_or("bundle staging unavailable")?;
    let state = bundle_staging::state(&staging_key_in(&root, &key), &store, size)?;
    Ok(BundleUploadStatus {
        chunk_bytes: PIPELINED_CHUNK_BYTES as u32,
        received: state.received,
        sealed: state.sealed,
    })
}

/// Stage one chunk of a pipelined upload, in any order, unflushed.
pub(crate) fn stage_chunk(
    root: &Path,
    owner: u64,
    chunk: StagedBundleChunk,
    alive: impl Fn() -> bool,
) -> Result<u32, String> {
    if chunk.bytes.len() > PIPELINED_CHUNK_BYTES {
        return Err("invalid private bundle upload".into());
    }
    let (root, key, receipt, _held) = pipelined_upload(root, owner, &chunk.request, &alive)?;
    if receipt.complete || receipt.commit.is_some() {
        return Ok(chunk.index);
    }
    let size = chunk.request.bundle.size;
    let store = pipelined_store(&root, &key, size, true)?.ok_or("bundle staging unavailable")?;
    if !alive() {
        return Err("connection retired".into());
    }
    bundle_staging::stage(
        &staging_key_in(&root, &key),
        &store,
        size,
        chunk.index,
        &chunk.sha256,
        &chunk.bytes,
    )?;
    Ok(chunk.index)
}

/// Seal a complete pipelined upload: one flush, then its digest. The
/// workspace lock makes one seal (or prepare) of this worktree run at a time.
pub(crate) fn seal_bundle(
    root: &Path,
    owner: u64,
    request: PrepareWorkspace,
    alive: impl Fn() -> bool,
) -> Result<(), String> {
    validate(&request)?;
    if owner != request.owner_id {
        return Err("invalid private bundle upload".into());
    }
    // Under the workspace lock, exactly one upload of this worktree
    // may be sealed at a time. The lock is held only for that claim: the
    // flush and the whole-bundle hash run under the upload's own lock.
    {
        let _workspace = locks::workspace(&request.worktree_path, &alive)?;
        let receipts = directory(&io(File::open(root))?, "remote-workspaces", true)?;
        claim_seal(
            root,
            &receipts,
            &receipt_key(&request.origin_key, &request.request_id),
            &request,
        )?;
    }
    let (root, key, receipt, _held) = pipelined_upload(root, owner, &request, &alive)?;
    if receipt.complete || receipt.commit.is_some() {
        return Ok(());
    }
    test_hold("seal");
    verify_pipelined(&root, &key, &request)?
        .ok_or_else(|| "bundle transfer is incomplete; resume original request".to_string())?;
    Ok(())
}

/// The marker naming the one upload of a worktree that may be sealed.
fn seal_marker(request: &PrepareWorkspace) -> String {
    format!("sealed-{}", digest(request.worktree_path.as_bytes()))
}

/// Claim the seal of `request.worktree_path` for upload `key`. Another
/// upload holds it while its pipelined staging exists; a stale claim (its
/// staging gone) is taken over. Called with the worktree's lock held.
///
/// Every claim runs under the worktree's lock, and every use of a sealed
/// upload (its verify, a prepare, a base update) holds that upload's staging
/// lock. So a holder whose staging lock is free is not running: it was
/// abandoned (an update that ended unknown, then the base moved on), and its
/// staging is reclaimed rather than wedging every later upload.
pub(crate) fn claim_seal(
    private: &Path,
    root: &File,
    key: &str,
    request: &PrepareWorkspace,
) -> Result<(), String> {
    let marker = seal_marker(request);
    if exists(root, &marker)? {
        let holder = String::from_utf8(read(root, &marker)?)
            .map_err(|_| "seal marker is invalid".to_string())?;
        if holder == key {
            return Ok(());
        }
        if exists(root, &format!("bundle-{holder}.chunks"))?
            && !reclaim_upload(private, root, &holder)?
        {
            return Err(
                "another upload of this workspace is being sealed or applied; let it finish, then retry"
                    .into(),
            );
        }
        replace(root, &marker, key.as_bytes())
    } else if publish_new(root, &marker, key.as_bytes())? {
        Ok(())
    } else {
        Err("another upload of this workspace claimed the seal; retry".into())
    }
}

/// The staging lock name of upload `key` under the private root `private`.
pub(crate) fn bundle_lock(private: &Path, key: &str) -> String {
    format!("bundle:{}:{key}", private.display())
}

/// Reclaim upload `key`'s staging (bytes, records, in-memory state) and, when
/// it only ever carried bytes, its receipt — unless the upload is in use.
/// `false` when it is in use.
pub(crate) fn reclaim_upload(private: &Path, root: &File, key: &str) -> Result<bool, String> {
    let Some(_staging) = locks::try_staging(&bundle_lock(private, key)) else {
        return Ok(false);
    };
    cleanup_payload(root, key)?;
    if exists(root, key)? {
        let carrier_only = read(root, key)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Receipt>(&bytes).ok())
            .is_some_and(|receipt| {
                !receipt.complete
                    && !receipt.clone_started
                    && !receipt.worktree_started
                    && !receipt.import_started
                    && receipt.commit.is_none()
            });
        if carrier_only {
            discard(root, key);
        }
    }
    Ok(true)
}

/// Release `key`'s seal claim, if it holds one.
pub(crate) fn release_seal(root: &File, key: &str, request: &PrepareWorkspace) {
    let marker = seal_marker(request);
    if read(root, &marker).is_ok_and(|holder| holder == key.as_bytes()) {
        discard(root, &marker);
    }
}

/// Seal and verify a pipelined upload, if this request has one.
fn verify_pipelined(
    root: &File,
    key: &str,
    request: &PrepareWorkspace,
) -> Result<Option<()>, String> {
    let size = request.bundle.size;
    let Some(store) = pipelined_store(root, key, size, false)? else {
        return Ok(None);
    };
    bundle_staging::verify(
        &staging_key_in(root, key),
        &store,
        size,
        &request.bundle.digest,
    )?;
    Ok(Some(()))
}

fn verified_bundle(
    private: &Path,
    root: &File,
    key: &str,
    request: &PrepareWorkspace,
) -> Result<String, String> {
    let file_name = format!("bundle-{key}");
    if exists(root, &format!("{file_name}.chunks"))? {
        // The caller holds the worktree's lock.
        claim_seal(private, root, key, request)?;
    }
    if verify_pipelined(root, key, request)?.is_some() {
        return staged_path(root, &file_name);
    }
    let mut file = open(root, &file_name, libc::O_RDONLY | libc::O_NONBLOCK)?;
    let meta = io(file.metadata())?;
    if !meta.is_file() || meta.nlink() != 1 || meta.len() != request.bundle.size {
        return Err("bundle transfer is incomplete; resume original request".into());
    }
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = io(file.read(&mut buf))?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    if format!("{:x}", hash.finalize()) != request.bundle.digest {
        return Err("bundle digest mismatch".into());
    }
    staged_path(root, &file_name)
}
fn staged_path(root: &File, file_name: &str) -> Result<String, String> {
    // The parent is an owner-held private descriptor. Git opens only this verified staging file.
    #[cfg(target_os = "linux")]
    let directory = format!("/proc/{}/fd/{}", std::process::id(), root.as_raw_fd());
    #[cfg(not(target_os = "linux"))]
    let directory = descriptor_path(root)?;
    Ok(format!("{directory}/{file_name}"))
}
#[cfg(not(target_os = "linux"))]
fn descriptor_path(file: &File) -> Result<String, String> {
    let mut path = [0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most PATH_MAX bytes into this buffer on macOS.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, path.as_mut_ptr()) } != 0 {
        return Err("private descriptor path unavailable".into());
    }
    let end = path
        .iter()
        .position(|b| *b == 0)
        .ok_or("invalid descriptor path")?;
    String::from_utf8(path[..end].to_vec()).map_err(|_| "invalid descriptor path".into())
}

pub(crate) fn prepare(
    root: &Path,
    owner: u64,
    request: PrepareWorkspace,
    alive: impl Fn() -> bool,
) -> PreparedWorkspace {
    if let Err(error) = validate(&request) {
        return PreparedWorkspace::failure("refused", error);
    }
    if owner != request.owner_id {
        return PreparedWorkspace::failure("refused", "private owner mismatch");
    }
    // The clone is shared by every worktree of this repository; the worktree
    // and its bundle staging are this preparation's own. The staging lock is
    // the seal: no chunk of this upload lands while the bundle is verified and
    // fetched, and the receipt names the single intent it belongs to.
    let key = receipt_key(&request.origin_key, &request.request_id);
    let held = locks::repository(&request.repo_path, &alive).and_then(|repository| {
        let workspace = locks::workspace(&request.worktree_path, &alive)?;
        let staging = locks::staging(&bundle_lock(root, &key), &alive)?;
        Ok((repository, workspace, staging))
    });
    let _held = match held {
        Ok(held) => held,
        Err(error) => return PreparedWorkspace::failure("refused", error),
    };
    test_hold("prepare");
    if !alive() {
        return PreparedWorkspace::failure("refused", "connection retired");
    }
    let private = root;
    // This descriptor path comes only from verified private serve startup.
    let root = match File::open(root)
        .map_err(|_| "private root unavailable".to_string())
        .and_then(|root| directory(&root, "remote-workspaces", true))
    {
        Ok(root) => root,
        Err(error) => return PreparedWorkspace::failure("refused", error),
    };
    let mut receipt = match reserve(&root, &key, &request) {
        Ok(value) => value,
        Err(error) => return PreparedWorkspace::failure("refused", error),
    };
    if receipt.complete {
        return completed(&root, &key, &receipt, &alive)
            .unwrap_or_else(|error| PreparedWorkspace::failure("unknown", error));
    }
    let result = execute(private, &root, &key, &mut receipt, &alive);
    let settled = match &result {
        Ok(()) if alive() => settle_upload(&root, &key, &request).err(),
        _ => None,
    };
    match result {
        Ok(()) if alive() => PreparedWorkspace {
            status: "prepared".into(),
            handle: Some(receipt.handle),
            cwd: Some(request.worktree_path),
            repo_path: Some(request.repo_path),
            repository_key: Some(request.repository_key),
            resolved_commit: receipt.commit,
            branch: Some(request.branch),
            message: settled,
        },
        Ok(_) => PreparedWorkspace::failure(
            "unknown",
            "connection retired after preparation; reconcile original request",
        ),
        Err(error) => PreparedWorkspace::failure(
            if receipt.clone_started || receipt.worktree_started || receipt.import_started {
                "unknown"
            } else {
                "refused"
            },
            error,
        ),
    }
}

/// Bring `request`'s bundle commit into the managed clone `repo` (unless it is
/// there already) and pin it under `refs/codesense/imports/`.
fn import_bundle(
    private: &Path,
    root: &File,
    key: &str,
    request: &PrepareWorkspace,
    repo: &File,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<(), String> {
    let writing_refs = |cwd: &File, args: &[&str]| -> Result<String, String> {
        let _refs = locks::refs(&request.repo_path, alive)?;
        git(cwd, args, alive, deadline)
    };
    let commit = request.bundle.commit.clone();
    if git(
        repo,
        &["cat-file", "-e", &format!("{commit}^{{commit}}")],
        alive,
        deadline,
    )
    .is_err()
    {
        if request.bundle.size == 0 {
            return Err(
                "previously present remote commit is unavailable; reconcile original request"
                    .into(),
            );
        }
        headroom(repo, request.bundle.size.saturating_mul(2))?;
        let bundle_path = verified_bundle(private, root, key, request)?;
        git(repo, &["bundle", "verify", &bundle_path], alive, deadline)?;
        writing_refs(
            repo,
            &[
                "fetch",
                "--no-tags",
                "--",
                &bundle_path,
                "refs/heads/codesense-prepared",
            ],
        )?;
    }
    if git(
        repo,
        &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
        alive,
        deadline,
    )? != commit
    {
        return Err("bundle commit is unavailable".into());
    }
    writing_refs(
        repo,
        &[
            "update-ref",
            &format!("refs/codesense/imports/{commit}"),
            &commit,
        ],
    )?;
    Ok(())
}

fn execute(
    private: &Path,
    root: &File,
    key: &str,
    receipt: &mut Receipt,
    alive: &dyn Fn() -> bool,
) -> Result<(), String> {
    let request = receipt.intent.clone();
    let deadline = Instant::now() + Duration::from_secs(30 * 60);
    // Ref-writing Git commands in the shared clone take its short refs lock
    // per command, so an export's `update-ref` never waits a whole prepare.
    let writing_refs = |cwd: &File, args: &[&str]| -> Result<String, String> {
        let _refs = locks::refs(&request.repo_path, alive)?;
        git(cwd, args, alive, deadline)
    };
    let repo_path = Path::new(&request.repo_path);
    let repo_parent = destination_parent(&request.repo_path, resolved_account_home().as_deref())?;
    owner_directory(&repo_parent)?;
    let repo_name = repo_path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("invalid repository path")?;
    let repo_marker = format!("repo-{}.json", digest(request.repo_path.as_bytes()));
    let repo_identity = serde_json::to_vec(&(
        request.owner_id,
        &request.repository_key,
        &request.clone_url,
    ))
    .map_err(|_| "invalid repo identity")?;
    git(
        &repo_parent,
        &["check-ref-format", "--branch", &request.branch],
        alive,
        deadline,
    )?;
    git(
        &repo_parent,
        &["check-ref-format", "--allow-onelevel", &request.base_ref],
        alive,
        deadline,
    )?;
    if !exists(root, &repo_marker)? {
        if exists(&repo_parent, repo_name)? {
            return Err("repository destination is not an owned managed clone".into());
        }
        write_new(root, &repo_marker, &repo_identity)?;
    } else if read(root, &repo_marker)? != repo_identity {
        return Err("repository destination belongs to another repository".into());
    }
    if !exists(&repo_parent, repo_name)? {
        let attempted_key = format!("clone-attempt-{}", digest(request.repo_path.as_bytes()));
        if receipt.clone_started || exists(root, &attempted_key)? {
            return Err(
                "prior clone has no authoritative completion; original request remains unknown"
                    .into(),
            );
        }
        write_new(root, &attempted_key, key.as_bytes())?;
        receipt.clone_started = true;
        save(root, key, receipt, false)?;
        git(&repo_parent, &["init", "--", repo_name], alive, deadline)?;
    }
    let repo = directory(&repo_parent, repo_name, false)?;
    owner_directory(&repo)?;

    receipt.import_started = true;
    save(root, key, receipt, false)?;
    let actual_url = git(
        &repo,
        &["config", "--get", "remote.origin.url"],
        alive,
        deadline,
    )
    .ok();
    if let Some(url) = actual_url {
        if url != request.clone_url {
            return Err("existing clone has a different remote identity".into());
        }
    } else {
        git(
            &repo,
            &["remote", "add", "origin", &request.clone_url],
            alive,
            deadline,
        )?;
    }
    let commit = request.bundle.commit.clone();
    import_bundle(private, root, key, &request, &repo, alive, deadline)?;
    receipt.commit = Some(commit.clone());
    save(root, key, receipt, false)?;
    let path = Path::new(&request.worktree_path);
    let parent = destination_parent(&request.worktree_path, resolved_account_home().as_deref())?;
    owner_directory(&parent)?;
    let leaf = path
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or("invalid worktree path")?;
    let ownership_key = ownership_marker_name(&request.worktree_path);
    let ownership_found = exists(root, &ownership_key)?;
    let mut ownership: WorktreeOwnership = if ownership_found {
        let owned: WorktreeOwnership = serde_json::from_slice(&read(root, &ownership_key)?)
            .map_err(|_| "worktree ownership unavailable")?;
        if owned.origin_key != request.origin_key
            || owned.repository_key != request.repository_key
            || owned.branch != request.branch
            || owned.commit != commit
        {
            return Err("worktree belongs to another immutable workspace identity".into());
        }
        if !owned.complete && owned.request_id != request.request_id {
            return Err(
                "prior worktree preparation remains unknown; reconcile original request".into(),
            );
        }
        owned
    } else {
        if exists(&parent, leaf)? {
            return Err("worktree destination is occupied".into());
        }
        let owned = WorktreeOwnership {
            origin_key: request.origin_key.clone(),
            repository_key: request.repository_key.clone(),
            branch: request.branch.clone(),
            commit: commit.clone(),
            request_id: request.request_id.clone(),
            complete: false,
        };
        write_new(
            root,
            &ownership_key,
            &serde_json::to_vec(&owned).map_err(|_| "invalid ownership")?,
        )?;
        owned
    };
    let mut expected_commit = commit.clone();
    if !exists(&parent, leaf)? {
        if receipt.worktree_started || ownership_found {
            return Err("prior worktree effect is unknown; no automatic replay".into());
        }
        receipt.worktree_started = true;
        save(root, key, receipt, false)?;
        let reference = format!("refs/heads/{}", request.branch);
        let branch_exists = git(
            &repo,
            &["for-each-ref", "--format=%(refname)", &reference],
            alive,
            deadline,
        )?
        .lines()
        .any(|name| name == reference);
        if branch_exists {
            expected_commit = git(&repo, &["rev-parse", &reference], alive, deadline)?;
            writing_refs(
                &repo,
                &[
                    "worktree",
                    "add",
                    "--",
                    &request.worktree_path,
                    &request.branch,
                ],
            )?;
        } else {
            writing_refs(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    &request.branch,
                    "--",
                    &request.worktree_path,
                    &commit,
                ],
            )?;
        }
    } else if !receipt.worktree_started && !ownership.complete {
        return Err("worktree destination is occupied by an incomplete operation".into());
    }
    let worktree = directory(&parent, leaf, false)?;
    owner_directory(&worktree)?;
    if git(&worktree, &["rev-parse", "HEAD"], alive, deadline)? != expected_commit
        || git(
            &worktree,
            &["symbolic-ref", "--short", "HEAD"],
            alive,
            deadline,
        )? != request.branch
        || git(
            &worktree,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            alive,
            deadline,
        )? != format!("{}/.git", request.repo_path)
    {
        return Err("worktree identity changed".into());
    }
    ownership.complete = true;
    replace(
        root,
        &ownership_key,
        &serde_json::to_vec(&ownership).map_err(|_| "invalid ownership")?,
    )?;
    receipt.complete = true;
    save(root, key, receipt, false)?;
    Ok(())
}

#[cfg(test)]
#[path = "remote_workspace/concurrency_tests.rs"]
pub(crate) mod concurrency_tests;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use codesense_host_proto::test_support::strip_attribution_env;
    use std::os::unix::fs::symlink;
    #[test]
    fn removal_rechecks_late_saves_tip_and_sessions_before_releasing_the_source() {
        let (temp, prepared_request) = fixture();
        let prepared = prepared(temp.path(), prepared_request.clone());
        assert_eq!(prepared.status, "prepared");
        let path = Path::new(&prepared_request.worktree_path);
        let mut request = RemoveWorktree {
            expected_tip: prepared_request.bundle.commit.clone(),
            discard_uncommitted: false,
            owner_id: 41,
            repository_key: prepared_request.repository_key.clone(),
            repo_path: prepared_request.repo_path.clone(),
            worktree_path: prepared_request.worktree_path.clone(),
        };
        let pending =
            codesense_pty_core::remote_workspace::removal::admit_spawn(path.to_str().unwrap())
                .unwrap();
        assert!(
            remove_worktree(temp.path(), 41, request.clone(), || true, || Ok(()))
                .unwrap_err()
                .contains("pending")
        );
        assert!(path.exists());
        drop(pending);
        let result = remove_worktree(
            temp.path(),
            41,
            request.clone(),
            || true,
            || {
                std::fs::write(path.join("late.txt"), "late save").unwrap();
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(path.join("late.txt")).unwrap(),
            "late save"
        );
        assert!(
            remove_worktree(temp.path(), 41, request.clone(), || true, || Ok(()))
                .unwrap_err()
                .contains("uncommitted")
        );
        request.discard_uncommitted = true;
        request.expected_tip = "0".repeat(40);
        assert!(
            remove_worktree(temp.path(), 41, request.clone(), || true, || Ok(()))
                .unwrap_err()
                .contains("tip changed")
        );
        request.expected_tip = prepared_request.bundle.commit;
        assert!(remove_worktree(
            temp.path(),
            41,
            request.clone(),
            || true,
            || Err("runningAgents".into())
        )
        .unwrap_err()
        .contains("runningAgents"));
        assert!(path.exists());
        assert!(
            remove_worktree(
                temp.path(),
                41,
                request,
                || true,
                || {
                    // A launch reaching admission during cleanup cannot enter this tree.
                    assert!(codesense_pty_core::remote_workspace::removal::admit_spawn(
                        path.to_str().unwrap()
                    )
                    .is_err());
                    Ok(())
                }
            )
            .unwrap()
            .removed
        );
        assert!(!path.exists());
    }

    #[test]
    fn removal_checks_head_last_and_reports_commits_after_that_check() {
        let (temp, prepared_request) = fixture();
        assert_eq!(
            prepared(temp.path(), prepared_request.clone()).status,
            "prepared"
        );
        let source = Path::new(&prepared_request.worktree_path);
        let commit = || {
            run(
                source,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "late external commit",
                ],
            );
        };
        let mut request = RemoveWorktree {
            expected_tip: prepared_request.bundle.commit,
            discard_uncommitted: false,
            owner_id: 41,
            repository_key: prepared_request.repository_key,
            repo_path: prepared_request.repo_path.clone(),
            worktree_path: prepared_request.worktree_path.clone(),
        };
        // A commit during the session check is caught by the final HEAD read.
        let refused = remove_worktree(
            temp.path(),
            41,
            request.clone(),
            || true,
            || {
                commit();
                Ok(())
            },
        );
        assert!(refused.unwrap_err().contains("tip changed"));
        assert!(source.exists());
        let head = || {
            String::from_utf8(
                Command::new("git")
                    .current_dir(&request.repo_path)
                    .args(["rev-parse", "refs/heads/work-branch"])
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .trim()
            .to_string()
        };
        request.expected_tip = head();
        // An external client can still commit between the last check and Git removal.
        let result = remove_worktree_with(
            temp.path(),
            41,
            request.clone(),
            || true,
            || Ok(()),
            || {
                commit();
                Ok(())
            },
        )
        .unwrap();
        let leftover = head();
        assert_ne!(leftover, request.expected_tip);
        assert!(result.removed);
        assert!(!source.exists());
        let warning = result.note.unwrap();
        assert!(warning.contains(&leftover));
        assert!(warning.contains("stays on the source branch refs/heads/work-branch"));
    }

    #[test]
    fn move_report_counts_all_remote_refs_and_reports_cached_fetch_failure() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap();
        let repo = path.join("repo");
        std::fs::create_dir(&repo).unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["commit", "--allow-empty", "-m", "base"],
        ] {
            let output = Command::new("git")
                .current_dir(&repo)
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        let check = |refresh_refs| {
            check_worktree_dirty_remote(
                &path,
                41,
                CheckWorktreeDirtyRemote {
                    refresh_refs,
                    owner_id: 41,
                    repository_key: "github.com/example/work".into(),
                    repo_path: repo.to_string_lossy().into_owned(),
                    worktree_path: repo.to_string_lossy().into_owned(),
                },
                || true,
            )
            .unwrap()
        };
        let initial = check(false);
        assert_eq!(initial.unpushed_count, 1);
        let output = Command::new("git")
            .current_dir(&repo)
            .args(["update-ref", "refs/remotes/other/main", &initial.source_tip])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(check(false).unpushed_count, 0);
        let output = Command::new("git")
            .current_dir(&repo)
            .args(["remote", "add", "origin", "/nonexistent/move-test-origin"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let cached = check(true);
        assert!(cached.fetch_error.is_some());
        assert_eq!(cached.source_tip, initial.source_tip);
        assert_eq!(cached.unpushed_count, 0);
        std::fs::write(repo.join("tracked.txt"), "first").unwrap();
        let output = Command::new("git")
            .current_dir(&repo)
            .args(["add", "tracked.txt"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let output = Command::new("git")
            .current_dir(&repo)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "tracked",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        std::fs::write(repo.join("tracked.txt"), "edited").unwrap();
        let dirty = check(false);
        assert!(dirty.dirty);
        assert_eq!(dirty.paths, vec!["tracked.txt"]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn git_completion_notification_wakes_before_wait_budget() {
        // A controlled child stays alive until the notification wait starts.
        let mut child = Command::new("sh")
            .args(["-c", "read line"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let fd = git_exit_fd(child.id()).expect("Linux fixture requires pidfd support");
        assert!(!wait_git_exit(Some(&fd), Duration::ZERO));
        let mut input = child.stdin.take().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            writeln!(input, "exit").unwrap();
        });
        let started = Instant::now();
        // A deliberately large wait makes this a readiness test, not a flaky
        // sub-20ms scheduler benchmark: unconditional sleep would take a second.
        let ready = wait_git_exit(Some(&fd), Duration::from_secs(1));
        let elapsed = started.elapsed();
        release.join().unwrap();
        let status = child.wait().unwrap();
        assert!(ready && status.success());
        assert!(elapsed < Duration::from_millis(500), "{elapsed:?}");
    }

    #[test]
    fn a_destination_under_the_account_codesense_dir_gets_its_parents() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let worktree = home.join("codesense/worktrees/team/SW-1");
        let parent = destination_parent(worktree.to_str().unwrap(), Some(&home)).unwrap();
        owner_directory(&parent).unwrap();
        let made = home.join("codesense/worktrees/team");
        assert!(made.is_dir());
        assert_eq!(made.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        assert!(!worktree.exists(), "only the parents are made");
        // Repeating is harmless; outside ~/codesense nothing is created.
        destination_parent(worktree.to_str().unwrap(), Some(&home)).unwrap();
        let elsewhere = home.join("elsewhere/repo");
        assert!(destination_parent(elsewhere.to_str().unwrap(), Some(&home)).is_err());
        assert!(!home.join("elsewhere").exists());
        assert!(destination_parent(worktree.to_str().unwrap(), None).is_ok());
        // A level the account left group-writable (umask 0002) is tightened.
        let shared = home.join("codesense/worktrees");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o775)).unwrap();
        destination_parent(worktree.to_str().unwrap(), Some(&home)).unwrap();
        assert_eq!(
            shared.metadata().unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn git_completion_falls_back_without_notification() {
        #[cfg(not(target_os = "linux"))]
        assert!(git_exit_fd(std::process::id()).is_none());
        assert!(!wait_git_exit(None, Duration::from_millis(2)));
    }

    #[test]
    fn git_completion_preserves_positive_output_bounds_and_interruption() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = File::open(temp.path()).unwrap();
        let bytes = git_output(
            &cwd,
            &["--version"],
            &|| true,
            Instant::now() + Duration::from_secs(2),
            1024,
        )
        .unwrap();
        assert!(bytes.starts_with(b"git version "));
        assert_eq!(
            git_output(
                &cwd,
                &["--version"],
                &|| true,
                Instant::now() + Duration::from_secs(2),
                1
            )
            .unwrap_err(),
            "Git output exceeds bounded complete-result limit"
        );
        // Before a call starts, a passed deadline and retired authority read
        // differently: a retryable timeout vs access loss.
        assert_eq!(
            git_output(&cwd, &["status"], &|| true, Instant::now(), 1024).unwrap_err(),
            GIT_TIMED_OUT
        );
        assert!(git_output(
            &cwd,
            &["status"],
            &|| false,
            Instant::now() + Duration::from_secs(5),
            1024
        )
        .unwrap_err()
        .contains("authority"));
        // The alias descendant retains stdout; returning promptly also proves
        // interrupted group cleanup closes the reader before its thread joins.
        // `PROMPT` sits well under the alias's 10s sleep and leaves room for
        // spawn under load; the authority case's deadline sits past it, so only
        // the lost authority can have stopped it in time.
        const PROMPT: Duration = Duration::from_secs(5);
        for cancel_authority in [false, true] {
            let started = Instant::now();
            let deadline = started
                + if cancel_authority {
                    Duration::from_secs(8)
                } else {
                    Duration::from_millis(60)
                };
            let error = git_output(
                &cwd,
                &["-c", "alias.fixture=!sleep 10", "fixture"],
                &|| !cancel_authority || started.elapsed() < Duration::from_millis(60),
                deadline,
                1024,
            )
            .unwrap_err();
            // Mid-run, the outcome of a mutating call is unknown either way.
            assert_eq!(error, GIT_INTERRUPTED);
            assert!(started.elapsed() < PROMPT);
        }
    }

    fn run(dir: &Path, args: &[&str]) {
        assert!(strip_attribution_env(&mut Command::new("git"))
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap()
            .success());
    }
    pub(crate) fn fixture() -> (tempfile::TempDir, PrepareWorkspace) {
        fixture_from(false)
    }
    /// A fixture whose preparation commit has a parent: an older commit, then
    /// `base` adding `prepared.txt`.
    pub(crate) fn fixture_with_history() -> (tempfile::TempDir, PrepareWorkspace) {
        fixture_from(true)
    }
    fn fixture_from(history: bool) -> (tempfile::TempDir, PrepareWorkspace) {
        let temp = tempfile::tempdir().unwrap();
        let canonical = temp.path().canonicalize().unwrap();
        let source = canonical.join("source");
        std::fs::create_dir(&source).unwrap();
        run(&source, &["init", "-b", "main"]);
        let commit = |message: &str| {
            run(
                &source,
                &[
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "commit",
                    "--allow-empty",
                    "-m",
                    message,
                ],
            )
        };
        if history {
            commit("older");
            std::fs::write(source.join("prepared.txt"), "prepared\n").unwrap();
            run(&source, &["add", "prepared.txt"]);
        }
        commit("base");
        run(&source, &["branch", "codesense-prepared"]);
        let bundle_path = temp.path().join("input.bundle");
        run(
            &source,
            &[
                "bundle",
                "create",
                bundle_path.to_str().unwrap(),
                "refs/heads/codesense-prepared",
            ],
        );
        let bundle_bytes = std::fs::read(&bundle_path).unwrap();
        let commit = String::from_utf8(
            strip_attribution_env(&mut Command::new("git"))
                .args(["rev-parse", "HEAD"])
                .current_dir(&source)
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        std::fs::rename(&source, temp.path().join("source-offline")).unwrap();
        let request = PrepareWorkspace {
            request_id: "00000000-0000-4000-8000-000000000001".into(),
            owner_id: 41,
            origin_key: "a".repeat(64),
            repository_key: "github:example/repo".into(),
            clone_url: source.to_string_lossy().into_owned(),
            repo_path: canonical.join("clone").to_string_lossy().into_owned(),
            worktree_path: canonical.join("work").to_string_lossy().into_owned(),
            base_ref: "main".into(),
            branch: "work-branch".into(),
            bundle: codesense_pty_core::remote_workspace::GitBundle {
                size: bundle_bytes.len() as u64,
                digest: digest(&bundle_bytes),
                commit,
            },
        };
        (temp, request)
    }
    pub(super) fn prepared(root: &Path, request: PrepareWorkspace) -> PreparedWorkspace {
        let bytes = std::fs::read(root.join("input.bundle")).unwrap();
        for (index, bytes) in bytes.chunks(BUNDLE_CHUNK_BYTES).enumerate() {
            if let Err(error) = upload(
                root,
                41,
                BundleChunk {
                    request: request.clone(),
                    offset: (index * BUNDLE_CHUNK_BYTES) as u64,
                    bytes: bytes.to_vec(),
                },
                || true,
            ) {
                return PreparedWorkspace::failure("refused", error);
            }
        }
        prepare(root, 41, request, || true)
    }
    #[test]
    fn completed_deleted_workspace_has_distinct_missing_outcome() {
        let (temp, request) = fixture();
        let ready = prepared(temp.path(), request.clone());
        assert_eq!(ready.status, "prepared", "{ready:?}");
        std::fs::remove_dir_all(&request.worktree_path).unwrap();
        let error = reconcile(temp.path(), 41, request, || true).unwrap_err();
        assert!(error.starts_with("REMOTE_WORKSPACE_MISSING:"), "{error}");
    }

    #[test]
    fn completed_path_outcomes_and_incomplete_receipt_are_distinct() {
        for kind in [
            "parent",
            "symlink",
            "file",
            "incomplete",
            "state",
            "receipt",
        ] {
            let (temp, mut request) = fixture();
            let parent = temp.path().canonicalize().unwrap().join("parent");
            std::fs::create_dir(&parent).unwrap();
            request.worktree_path = parent.join("work").to_string_lossy().into_owned();
            let ready = prepared(temp.path(), request.clone());
            assert_eq!(ready.status, "prepared", "{ready:?}");
            std::fs::remove_dir_all(&request.worktree_path).unwrap();
            let root = directory(
                &File::open(temp.path()).unwrap(),
                "remote-workspaces",
                false,
            )
            .unwrap();
            let key = receipt_key(&request.origin_key, &request.request_id);
            let expected = match kind {
                "parent" => {
                    std::fs::remove_dir_all(&parent).unwrap();
                    "REMOTE_WORKSPACE_MISSING:"
                }
                "symlink" => {
                    symlink(&request.repo_path, &request.worktree_path).unwrap();
                    "REMOTE_WORKSPACE_UNSAFE:"
                }
                "file" => {
                    std::fs::write(&request.worktree_path, "kept").unwrap();
                    "REMOTE_WORKSPACE_OCCUPIED:"
                }
                "incomplete" => {
                    let mut receipt: Receipt =
                        serde_json::from_slice(&read(&root, &key).unwrap()).unwrap();
                    receipt.complete = false;
                    save(&root, &key, &receipt, false).unwrap();
                    "workspace preparation has not completed"
                }
                "state" => {
                    std::fs::remove_dir_all(temp.path().join("remote-workspaces")).unwrap();
                    "workspace private state unavailable"
                }
                _ => {
                    std::fs::remove_file(temp.path().join("remote-workspaces").join(&key)).unwrap();
                    "workspace receipt unavailable"
                }
            };
            let target = codesense_pty_core::remote_files::FileTarget {
                owner_id: 41,
                origin_key: request.origin_key,
                request_id: request.request_id,
                handle: ready.handle.unwrap(),
            };
            let error = resolve_completed_target(temp.path(), 41, &target, &|| true)
                .err()
                .unwrap();
            assert!(error.starts_with(expected), "{kind}: {error}");
        }
    }

    #[test]
    fn forget_missing_allows_a_deleted_clone_and_preserves_other_state() {
        let (temp, mut request) = fixture();
        let parent = temp.path().canonicalize().unwrap().join("workspace-parent");
        std::fs::create_dir(&parent).unwrap();
        request.repo_path = parent.join("clone").to_string_lossy().into_owned();
        request.worktree_path = parent.join("work").to_string_lossy().into_owned();
        let ready = prepared(temp.path(), request.clone());
        assert_eq!(ready.status, "prepared", "{ready:?}");
        let state_path = temp.path().join("remote-workspaces");
        let marker = ownership_marker_name(&request.worktree_path);
        let other_state: std::collections::BTreeMap<_, _> = std::fs::read_dir(&state_path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.file_name().unwrap() != marker.as_str())
            .map(|path| {
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        std::fs::remove_dir_all(&parent).unwrap();
        let target = codesense_pty_core::remote_files::FileTarget {
            owner_id: 41,
            origin_key: request.origin_key,
            request_id: request.request_id,
            handle: ready.handle.unwrap(),
        };
        let error = resolve_completed_target(temp.path(), 41, &target, &|| true)
            .err()
            .unwrap();
        assert!(error.starts_with(WORKSPACE_MISSING), "{error}");
        forget_missing_workspace(temp.path(), 41, ForgetMissingWorkspace { target }, || true)
            .unwrap();
        assert!(!state_path.join(marker).exists());
        assert!(!parent.exists());
        for (path, bytes) in other_state {
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }

    #[test]
    fn forget_missing_refuses_existing_folder_then_prepares_again_preserving_branch_commits() {
        let (temp, request) = fixture();
        let ready = prepared(temp.path(), request.clone());
        assert_eq!(ready.status, "prepared", "{ready:?}");
        let target = codesense_pty_core::remote_files::FileTarget {
            owner_id: 41,
            origin_key: request.origin_key.clone(),
            request_id: request.request_id.clone(),
            handle: ready.handle.unwrap(),
        };
        let forget = || {
            forget_missing_workspace(
                temp.path(),
                41,
                ForgetMissingWorkspace {
                    target: target.clone(),
                },
                || true,
            )
        };
        std::fs::write(
            Path::new(&request.worktree_path).join("kept.txt"),
            "unpublished commit",
        )
        .unwrap();
        run(Path::new(&request.worktree_path), &["add", "kept.txt"]);
        run(
            Path::new(&request.worktree_path),
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "keep me",
            ],
        );
        let root = directory(
            &File::open(temp.path()).unwrap(),
            "remote-workspaces",
            false,
        )
        .unwrap();
        let marker = ownership_marker_name(&request.worktree_path);
        assert!(forget().unwrap_err().contains("there again"));
        assert!(exists(&root, &marker).unwrap());
        assert!(Path::new(&request.worktree_path).join("kept.txt").is_file());
        std::fs::remove_dir_all(&request.worktree_path).unwrap();
        forget().unwrap();
        assert!(!exists(&root, &marker).unwrap());
        assert!(!Path::new(&request.worktree_path).exists());
        let mut fresh = request;
        fresh.request_id = uuid::Uuid::new_v4().to_string();
        let ready = prepared(temp.path(), fresh.clone());
        assert_eq!(ready.status, "prepared", "{ready:?}");
        assert_eq!(
            std::fs::read_to_string(Path::new(&fresh.worktree_path).join("kept.txt")).unwrap(),
            "unpublished commit"
        );
        assert_eq!(
            reconcile(temp.path(), 41, fresh, || true)
                .unwrap()
                .unwrap()
                .status,
            "alreadyPrepared"
        );
    }

    #[test]
    fn prepares_git_only_reuses_remote_commit_and_preserves_worktree_edits() {
        let (temp, request) = fixture();
        let first = prepared(temp.path(), request.clone());
        assert_eq!(first.status, "prepared", "{first:?}");
        // The status hook rides the machine's own host: nothing lands in the workspace.
        assert!(!Path::new(&request.worktree_path).join(".claude").exists());
        let chosen = Path::new(&request.worktree_path).join("agent.txt");
        std::fs::write(&chosen, "agent edit").unwrap();
        let again = prepared(temp.path(), request.clone());
        assert_eq!(again.status, "alreadyPrepared", "{again:?}");
        assert_eq!(again.handle, first.handle);
        let payload = temp.path().join("remote-workspaces").join(format!(
            "bundle-{}.json",
            digest(format!("{}:{}", request.origin_key, request.request_id).as_bytes())
        ));
        assert!(!payload.exists(), "completed daemon staging is reclaimed");
        // Advancing the owned branch does not recreate or reset the worktree.
        let cwd = Path::new(&request.worktree_path);
        run(cwd, &["add", "agent.txt"]);
        run(
            cwd,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "agent change",
            ],
        );
        let settled = reconcile(temp.path(), 41, request.clone(), || true)
            .unwrap()
            .unwrap();
        assert_eq!(settled.status, "alreadyPrepared");
        assert_eq!(
            settled.resolved_commit.as_deref(),
            Some(request.bundle.commit.as_str())
        );
        assert_eq!(
            prepare(temp.path(), 41, request.clone(), || true).status,
            "alreadyPrepared"
        );
        assert_eq!(std::fs::read_to_string(&chosen).unwrap(), "agent edit");
        let knowledge = inspect_repository(
            temp.path(),
            41,
            InspectRepository {
                owner_id: 41,
                repository_key: request.repository_key.clone(),
                clone_url: request.clone_url.clone(),
                repo_path: request.repo_path.clone(),
                commit: request.bundle.commit.clone(),
            },
            || true,
        )
        .unwrap();
        assert!(knowledge.has_commit);
        assert!(knowledge.imported_commits.contains(&request.bundle.commit));
        let mut next = request.clone();
        next.request_id = "00000000-0000-4000-8000-000000000002".into();
        next.branch = "next-work".into();
        next.worktree_path = temp
            .path()
            .canonicalize()
            .unwrap()
            .join("next-work")
            .to_string_lossy()
            .into_owned();
        next.bundle.size = 0;
        next.bundle.digest = digest(&[]);
        let result = prepare(temp.path(), 41, next, || true);
        assert_eq!(result.status, "prepared", "{result:?}");
        let mut changed = request;
        changed.branch = "other".into();
        assert_eq!(prepared(temp.path(), changed).status, "refused");
    }
    #[test]
    fn refuses_unowned_symlink_traversal_and_retired_origin_without_effects() {
        let (temp, mut request) = fixture();
        assert_eq!(
            prepare(temp.path(), 42, request.clone(), || true).status,
            "refused"
        );
        assert_eq!(
            prepare(temp.path(), 41, request.clone(), || false).status,
            "refused"
        );
        request.repo_path.push_str("/../escape");
        assert_eq!(prepared(temp.path(), request).status, "refused");
        let (_, mut request) = fixture();
        let path = temp.path().canonicalize().unwrap().join("symlink");
        symlink(temp.path(), &path).unwrap();
        request.repo_path = path.to_string_lossy().into_owned();
        assert_ne!(prepared(temp.path(), request).status, "prepared");
    }
    #[test]
    fn subsequent_worktree_imports_bundle_with_confirmed_remote_prerequisite() {
        let (temp, first) = fixture();
        assert_eq!(prepared(temp.path(), first.clone()).status, "prepared");
        let source = temp.path().join("source-offline");
        std::fs::write(source.join("new.txt"), "new committed data").unwrap();
        run(&source, &["add", "new.txt"]);
        run(
            &source,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "next",
            ],
        );
        run(&source, &["branch", "-f", "codesense-prepared", "HEAD"]);
        std::fs::remove_file(temp.path().join("input.bundle")).unwrap();
        run(
            &source,
            &[
                "bundle",
                "create",
                temp.path().join("input.bundle").to_str().unwrap(),
                "refs/heads/codesense-prepared",
                &format!("^{}", first.bundle.commit),
            ],
        );
        let bytes = std::fs::read(temp.path().join("input.bundle")).unwrap();
        let commit = String::from_utf8(
            Command::new("git")
                .current_dir(&source)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        let mut next = first.clone();
        next.request_id = "00000000-0000-4000-8000-000000000002".into();
        next.worktree_path = temp
            .path()
            .canonicalize()
            .unwrap()
            .join("second")
            .to_string_lossy()
            .into_owned();
        next.branch = "second".into();
        next.bundle = codesense_pty_core::remote_workspace::GitBundle {
            size: bytes.len() as u64,
            digest: digest(&bytes),
            commit: commit.clone(),
        };
        let result = prepared(temp.path(), next.clone());
        assert_eq!(result.status, "prepared", "{result:?}");
        assert_eq!(result.resolved_commit.as_deref(), Some(commit.as_str()));
        assert_eq!(
            std::fs::read_to_string(Path::new(&next.worktree_path).join("new.txt")).unwrap(),
            "new committed data"
        );
        assert!(!Path::new(&first.worktree_path).join("new.txt").exists());
    }
    #[test]
    fn bundle_upload_resumes_exact_bytes_and_fifo_refuses_without_blocking() {
        let (temp, request) = fixture();
        let bytes = std::fs::read(temp.path().join("input.bundle")).unwrap();
        let half = bytes.len() / 2;
        let chunk = BundleChunk {
            request: request.clone(),
            offset: 0,
            bytes: bytes[..half].to_vec(),
        };
        assert_eq!(
            upload(temp.path(), 41, chunk.clone(), || true)
                .unwrap()
                .next_offset,
            half as u64
        );
        assert_eq!(
            upload(temp.path(), 41, chunk, || true).unwrap().next_offset,
            half as u64
        );
        assert_eq!(
            upload(
                temp.path(),
                41,
                BundleChunk {
                    request: request.clone(),
                    offset: 0,
                    bytes: vec![]
                },
                || true
            )
            .unwrap()
            .next_offset,
            half as u64
        );
        assert!(upload(
            temp.path(),
            41,
            BundleChunk {
                request: request.clone(),
                offset: 0,
                bytes: vec![42]
            },
            || true
        )
        .is_err());
        upload(
            temp.path(),
            41,
            BundleChunk {
                request: request.clone(),
                offset: half as u64,
                bytes: bytes[half..].to_vec(),
            },
            || true,
        )
        .unwrap();
        assert_eq!(
            prepare(temp.path(), 41, request, || true).status,
            "prepared"
        );
        let root = File::open(temp.path()).unwrap();
        let fifo = CString::new(temp.path().join("fifo").to_str().unwrap()).unwrap();
        // SAFETY: valid owned temporary path and fixed owner-only permissions.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        assert!(read(&root, "fifo").is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    /// A pipelined upload (chunks out of order, one seal) prepares
    /// the workspace, a legacy chunk can't mix in, and staging is reclaimed.
    #[test]
    fn xs9_prepare_accepts_a_pipelined_upload_and_reclaims_its_staging() {
        let (temp, mut request) = fixture();
        // Pad the fixture bundle's declared identity to span several chunks.
        let source = temp.path().join("source-offline");
        let mut random = 7u32;
        let data: Vec<u8> = (0..3 * PIPELINED_CHUNK_BYTES)
            .map(|_| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                random as u8
            })
            .collect();
        std::fs::write(source.join("big.bin"), data).unwrap();
        run(&source, &["add", "big.bin"]);
        run(
            &source,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "big",
            ],
        );
        run(&source, &["branch", "-f", "codesense-prepared", "HEAD"]);
        let bundle = temp.path().join("big.bundle");
        run(
            &source,
            &[
                "bundle",
                "create",
                bundle.to_str().unwrap(),
                "refs/heads/codesense-prepared",
            ],
        );
        let bytes = std::fs::read(&bundle).unwrap();
        request.bundle = codesense_pty_core::remote_workspace::GitBundle {
            size: bytes.len() as u64,
            digest: digest(&bytes),
            commit: String::from_utf8(
                Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(&source)
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .trim()
            .to_string(),
        };
        let status = bundle_status(temp.path(), 41, request.clone(), || true).unwrap();
        assert!(status.received.count() >= 3 && status.received.received() == 0);
        let mut order = status.received.missing();
        order.reverse();
        let chunk = |index: u32| {
            let (offset, len) =
                codesense_pty_core::remote_workspace::chunk_range(bytes.len() as u64, index)
                    .unwrap();
            let part = bytes[offset as usize..offset as usize + len].to_vec();
            StagedBundleChunk {
                request: request.clone(),
                index,
                sha256: digest(&part),
                bytes: part,
            }
        };
        for index in &order[1..] {
            assert_eq!(
                stage_chunk(temp.path(), 41, chunk(*index), || true),
                Ok(*index)
            );
        }
        assert!(seal_bundle(temp.path(), 41, request.clone(), || true)
            .unwrap_err()
            .contains("incomplete"));
        assert!(
            upload(
                temp.path(),
                41,
                BundleChunk {
                    request: request.clone(),
                    offset: 0,
                    bytes: vec![]
                },
                || true
            )
            .is_err(),
            "a legacy chunk never mixes into a pipelined upload"
        );
        let early = prepare(temp.path(), 41, request.clone(), || true);
        assert_ne!(early.status, "prepared");
        assert!(early.message.unwrap().contains("incomplete"));
        stage_chunk(temp.path(), 41, chunk(order[0]), || true).unwrap();
        let held = bundle_status(temp.path(), 41, request.clone(), || true).unwrap();
        assert!(held.received.is_complete() && !held.sealed);
        seal_bundle(temp.path(), 41, request.clone(), || true).unwrap();
        assert!(
            bundle_status(temp.path(), 41, request.clone(), || true)
                .unwrap()
                .sealed
        );
        let result = prepare(temp.path(), 41, request.clone(), || true);
        assert_eq!(result.status, "prepared", "{result:?}");
        assert!(Path::new(&request.worktree_path).join("big.bin").exists());
        let receipts = temp.path().join("remote-workspaces");
        let key = receipt_key(&request.origin_key, &request.request_id);
        assert!(!receipts.join(format!("bundle-{key}")).exists());
        assert!(!receipts.join(format!("bundle-{key}.chunks")).exists());
        // A late duplicate chunk of the settled upload is acknowledged, not restaged.
        assert_eq!(
            stage_chunk(temp.path(), 41, chunk(order[0]), || true),
            Ok(order[0])
        );
        assert!(!receipts.join(format!("bundle-{key}")).exists());
    }

    #[test]
    fn lost_clone_receipt_never_replays_absent_effect() {
        let (temp, request) = fixture();
        let root = directory(&File::open(temp.path()).unwrap(), "remote-workspaces", true).unwrap();
        let key = format!(
            "{}.json",
            digest(format!("{}:{}", request.origin_key, request.request_id).as_bytes())
        );
        let receipt = Receipt {
            intent: request.clone(),
            handle: "held".into(),
            clone_started: true,
            worktree_started: false,
            import_started: false,
            commit: None,
            complete: false,
            file_operation_generation: None,
            base_commit: None,
        };
        save(&root, &key, &receipt, true).unwrap();
        let result = prepared(temp.path(), request.clone());
        assert_eq!(result.status, "unknown");
        assert!(!Path::new(&request.repo_path).exists());
    }
}
