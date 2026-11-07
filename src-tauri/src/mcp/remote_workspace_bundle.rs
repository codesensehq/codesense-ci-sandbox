// SPDX-License-Identifier: MIT
//! Desktop-only repository authentication. Only the resulting Git objects cross the wire.
use super::remote_workspace::PrepareProgress;
use crate::commands::{attachments, route_to_context::ActiveContext};
use crate::domain::SourceRepoSlug;
use codesense_pty_core::remote_workspace::{GitBundle, RepositoryKnowledge, MAX_BUNDLE_BYTES};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::{
    fs::{DirBuilderExt, OpenOptionsExt},
    process::CommandExt,
};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) fn local_seed(
    ctx: &ActiveContext,
    source: &SourceRepoSlug,
) -> Result<Option<PathBuf>, String> {
    let path =
        crate::context_repo::path_to_user_attachments_file(&ctx.codesense_home, &ctx.principal());
    let data = crate::context_repo::attachments_io::read_attachments(&path)
        .map_err(|_| "local repository attachments unavailable")?;
    Ok(attachments::group_attachments_by_remote(&data)
        .get(&source.remote_key())
        .map(|row| PathBuf::from(&row.repo_path)))
}
pub(crate) struct BuildSource {
    pub url: String,
    pub repository_key: String,
    pub base_ref: String,
    pub seed: Option<PathBuf>,
}
pub(super) fn lock_operation(
    path: &Path,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<File, String> {
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "bundle operation lock unavailable")?;
    if !lock
        .metadata()
        .map_err(|_| "bundle lock unavailable")?
        .is_file()
    {
        return Err("bundle lock is not regular".into());
    }
    loop {
        // SAFETY: live owned descriptor; lock releases on File drop.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(lock);
        }
        if !alive() || Instant::now() >= deadline {
            return Err("bundle operation is busy".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[allow(
    clippy::unnecessary_cast,
    reason = "statvfs field widths differ between supported Unix targets"
)]
pub(super) fn headroom(path: &Path, bytes: u64) -> Result<(), String> {
    let file = File::open(path).map_err(|_| "local disk unavailable")?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: held file descriptor and valid output storage.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err("local disk headroom unavailable".into());
    }
    let stat = unsafe { stat.assume_init() };
    if (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64)
        < bytes.saturating_add(256 * 1024 * 1024)
    {
        return Err("local disk lacks bundle preparation headroom".into());
    }
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct BundleState {
    intent_hash: String,
    url: String,
    base_ref: String,
    commit: Option<String>,
    bundle: Option<GitBundle>,
    #[serde(default)]
    settled: bool,
}
fn save_state(dir: &Path, state: &BundleState) -> Result<(), String> {
    let mut file =
        tempfile::NamedTempFile::new_in(dir).map_err(|_| "bundle state staging unavailable")?;
    file.write_all(&serde_json::to_vec(state).map_err(|_| "bundle state invalid")?)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|_| "bundle state not durable")?;
    file.persist(dir.join("intent.json"))
        .map_err(|_| "bundle state cannot commit")?;
    File::open(dir)
        .and_then(|f| f.sync_all())
        .map_err(|_| "bundle directory not durable".into())
}
fn read_state(path: &Path) -> Result<BundleState, String> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "bundle state unavailable")?;
    if !file
        .metadata()
        .map_err(|_| "bundle state unavailable")?
        .is_file()
    {
        return Err("bundle state is not regular".into());
    }
    let mut bytes = Vec::new();
    file.take(16384)
        .read_to_end(&mut bytes)
        .map_err(|_| "bundle state unreadable")?;
    serde_json::from_slice(&bytes).map_err(|_| "bundle state invalid".into())
}
struct GitChild(std::process::Child);
impl Drop for GitChild {
    fn drop(&mut self) {
        // SAFETY: this child was spawned into its own process group.
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}
pub(super) fn run(
    command: Command,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
    payload: Option<&Path>,
) -> Result<(), String> {
    run_inner(command, alive, deadline, payload, None, None, None)
}

/// The Git stages whose counts are a fetch's transfer: received in a pack, or
/// unpacked as loose objects (a small fetch).
pub(crate) const FETCH_STAGES: [&str; 2] = ["Receiving objects", "Unpacking objects"];
/// The Git stage whose count is a bundle's object count.
pub(crate) const BUNDLE_STAGE: &str = "Writing objects";

/// One Git progress line: `stage` (`Receiving objects`, …), `done`, and
/// `total` when Git shows a percentage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct GitProgress {
    pub stage: String,
    pub done: u64,
    pub total: Option<u64>,
}

/// Parse a line of Git's `--progress` output, e.g.
/// `remote: Counting objects:  45% (450/1000)` or `Enumerating objects: 12, done.`
pub(crate) fn parse_git_progress(line: &str) -> Option<GitProgress> {
    let line = line.trim().trim_start_matches("remote:").trim();
    let (stage, rest) = line.split_once(':')?;
    if !stage.ends_with("objects") && !stage.ends_with("deltas") {
        return None;
    }
    let rest = rest.trim();
    if let Some(open) = rest.find('(') {
        let inner = &rest[open + 1..rest[open..].find(')')? + open];
        let (done, total) = inner.split_once('/')?;
        return Some(GitProgress {
            stage: stage.trim().into(),
            done: done.trim().parse().ok()?,
            total: Some(total.trim().parse().ok()?),
        });
    }
    let count: String = rest.chars().take_while(char::is_ascii_digit).collect();
    Some(GitProgress {
        stage: stage.trim().into(),
        done: count.parse().ok()?,
        total: None,
    })
}

/// Run `command(true)` (with `--progress`) reporting its counts; when that
/// fails, run `command(false)` once. Progress is never worth a failed step:
/// a Git older than 2.31 has no `bundle create --progress`.
pub(super) fn run_or_without_progress(
    command: &dyn Fn(bool) -> Command,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
    payload: Option<&Path>,
    progress: &dyn Fn(GitProgress),
) -> Result<(), String> {
    if run_with_progress(command(true), alive, deadline, payload, progress).is_ok() {
        return Ok(());
    }
    if let Some(payload) = payload {
        let _ = std::fs::remove_file(payload);
    }
    run(command(false), alive, deadline, payload)
}

/// [`run`], handing each Git `--progress` line to `progress` as it arrives.
pub(super) fn run_with_progress(
    command: Command,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
    payload: Option<&Path>,
    progress: &dyn Fn(GitProgress),
) -> Result<(), String> {
    run_inner(
        command,
        alive,
        deadline,
        payload,
        None,
        None,
        Some(progress),
    )
}

/// [`run`], with `input` as the child's standard input.
pub(super) fn run_with_input(
    command: Command,
    input: File,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
) -> Result<(), String> {
    run_inner(command, alive, deadline, None, None, Some(input), None)
}

fn run_inner(
    mut command: Command,
    alive: &dyn Fn() -> bool,
    deadline: Instant,
    payload: Option<&Path>,
    output: Option<&Path>,
    input: Option<File>,
    progress: Option<&dyn Fn(GitProgress)>,
) -> Result<(), String> {
    let working_dir = command.get_current_dir().map(Path::to_path_buf);
    command
        .stdin(input.map_or_else(Stdio::null, Stdio::from))
        .stdout(match output {
            Some(path) => Stdio::from(
                std::fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(path)
                    .map_err(|_| "Git output unavailable")?,
            ),
            None => Stdio::null(),
        })
        .stderr(Stdio::piped())
        .env("GIT_PROGRESS_DELAY", "0")
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    // SAFETY: setpgid is async-signal-safe; only this subprocess group is cancelled.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    if !alive() || Instant::now() >= deadline {
        return Err("local Git preparation cancelled".into());
    }
    let mut child = GitChild(
        command
            .spawn()
            .map_err(|_| "local Git executable unavailable")?,
    );
    // Git rewrites one progress line with `\r`; each rewrite is a reading.
    let lines = child.0.stderr.take().map(|stderr| {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let mut stderr = stderr;
            let mut line = Vec::new();
            let mut buffer = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                for byte in &buffer[..n] {
                    if matches!(byte, b'\r' | b'\n') {
                        if !line.is_empty()
                            && tx.send(String::from_utf8_lossy(&line).into()).is_err()
                        {
                            return;
                        }
                        line.clear();
                    } else if line.len() < 512 {
                        line.push(*byte);
                    }
                }
            }
            if !line.is_empty() {
                let _ = tx.send(String::from_utf8_lossy(&line).into());
            }
        });
        rx
    });
    let diagnostics = std::cell::RefCell::new(GitDiagnostics::default());
    let drain = |final_read: bool| {
        if let Some(lines) = &lines {
            let take = |line: String| match parse_git_progress(&line) {
                Some(reading) => {
                    if let Some(progress) = progress {
                        progress(reading);
                    }
                }
                None => diagnostics.borrow_mut().push(line),
            };
            if final_read {
                while let Ok(line) = lines.recv_timeout(Duration::from_millis(200)) {
                    take(line);
                }
            } else {
                while let Ok(line) = lines.try_recv() {
                    take(line);
                }
            }
        }
    };
    loop {
        drain(false);
        if !alive()
            || output.is_some_and(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 65536))
            || Instant::now() >= deadline
            || payload.is_some_and(|path| {
                std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_BUNDLE_BYTES)
            })
        {
            return Err("local Git preparation interrupted; retry the same request".into());
        }
        if let Some(path) = &working_dir {
            headroom(path, 0)?;
        }
        if let Some(path) = payload {
            headroom(path.parent().ok_or("bundle directory unavailable")?, 0)?;
        }
        if let Some(status) = child
            .0
            .try_wait()
            .map_err(|_| "local Git process unavailable")?
        {
            drain(true);
            return if status.success() {
                Ok(())
            } else {
                Err(git_failure(&diagnostics.borrow()))
            };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How many recent git diagnostic lines a failed run keeps, so a chatty failure stays bounded.
const MAX_GIT_DIAGNOSTICS: usize = 20;

/// What git printed on stderr that is not progress: the first `fatal:` line is
/// kept whatever comes after it, and the last few lines are kept in order.
#[derive(Default)]
struct GitDiagnostics {
    fatal: Option<String>,
    recent: std::collections::VecDeque<String>,
}

impl GitDiagnostics {
    fn push(&mut self, line: String) {
        if self.fatal.is_none() && is_fatal_line(&line) {
            self.fatal = Some(line.clone());
        }
        if self.recent.len() == MAX_GIT_DIAGNOSTICS {
            self.recent.pop_front();
        }
        self.recent.push_back(line);
    }
}

/// `fatal:` at the start of a line, or after git's `remote:` prefix.
fn is_fatal_line(line: &str) -> bool {
    let text = line.trim_start();
    let text = text.strip_prefix("remote:").map_or(text, str::trim_start);
    text.starts_with("fatal:")
}

/// Drops the user and password from URLs git echoes, so a credential in a
/// remote address never reaches the error text.
fn redact_userinfo(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("://") {
        let (head, tail) = rest.split_at(at + 3);
        out.push_str(head);
        // A password may hold an unencoded `/`, so the span runs to whitespace or a quote.
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '\'' || c == '"')
            .unwrap_or(tail.len());
        let span = &tail[..end];
        let host = span.rfind('@').map_or(0, |i| i + 1);
        out.push_str(&span[host..]);
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// The reason git gave for a failed run: its first `fatal:` line, else its last
/// non-empty line, with credentials removed. Names no account.
fn git_failure(diagnostics: &GitDiagnostics) -> String {
    let reason = diagnostics
        .fatal
        .clone()
        .or_else(|| {
            diagnostics
                .recent
                .iter()
                .rev()
                .find(|line| !line.trim().is_empty())
                .cloned()
        })
        .map(|line| {
            redact_userinfo(line.trim())
                .chars()
                .take(240)
                .collect::<String>()
        });
    match reason {
        Some(reason) => format!("local Git operation failed: {reason}"),
        None => "local Git operation failed; git gave no reason".into(),
    }
}

#[cfg(test)]
pub(crate) fn build(
    root: &Path,
    key: &str,
    intent_hash: &str,
    source: BuildSource,
    alive: impl Fn() -> bool + Send + Sync + 'static,
    probe: impl Fn(&str) -> Result<RepositoryKnowledge, String>,
) -> Result<(PathBuf, GitBundle), String> {
    build_reporting(root, key, intent_hash, source, alive, probe, &|_| {})
}

/// [`build`], reporting `Fetching` (the seed fetch, with Git's object counts)
/// and `Bundling` (with the object count once Git reports it).
pub(crate) fn build_reporting(
    root: &Path,
    key: &str,
    intent_hash: &str,
    source: BuildSource,
    alive: impl Fn() -> bool + Send + Sync + 'static,
    probe: impl Fn(&str) -> Result<RepositoryKnowledge, String>,
    progress: &dyn Fn(PrepareProgress),
) -> Result<(PathBuf, GitBundle), String> {
    let BuildSource {
        url,
        repository_key,
        base_ref,
        seed,
    } = source;
    let (url, base_ref) = (url.as_str(), base_ref.as_str());
    let deadline = Instant::now() + Duration::from_secs(30 * 60);
    let dir = root.join(format!("bundle-{key}"));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|_| "private bundle directory unavailable")?;
    let _lock = lock_operation(&dir.join("operation.lock"), &alive, deadline)?;
    let state_path = dir.join("intent.json");
    let mut state = if state_path.exists() {
        read_state(&state_path)?
    } else {
        let state = BundleState {
            intent_hash: intent_hash.into(),
            url: url.into(),
            base_ref: base_ref.into(),
            commit: None,
            bundle: None,
            settled: false,
        };
        save_state(&dir, &state)?;
        state
    };
    if state.intent_hash != intent_hash || state.url != url || state.base_ref != base_ref {
        return Err("request already owns another local bundle intent".into());
    }
    let path = dir.join("objects.bundle");
    let pending = dir.join("objects.pending.bundle");
    if let Some(bundle) = state.bundle {
        if state.settled || bundle.size == 0 {
            return Ok((path, bundle));
        }
        let payload = if path.exists() { &path } else { &pending };
        if inspect(payload)? != (bundle.size, bundle.digest.clone()) {
            return Err("original local bundle changed".into());
        }
        if payload == &pending {
            std::fs::rename(&pending, &path).map_err(|_| "bundle publication unavailable")?;
            File::open(&dir)
                .and_then(|f| f.sync_all())
                .map_err(|_| "bundle publication not durable")?;
        }
        return Ok((path, bundle));
    }
    if path.exists() {
        return Err("unrecognized bundle destination; no overwrite".into());
    }
    // This partial payload is named only by this reserved, not-yet-consumed intent.
    if pending.exists() {
        std::fs::remove_file(&pending).map_err(|_| "partial bundle requires reconciliation")?;
    }
    let cache_root = root.join(format!(
        "repository-{:x}",
        Sha256::digest(repository_key.as_bytes())
    ));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&cache_root)
        .map_err(|_| "repository cache directory unavailable")?;
    let _cache_lock = lock_operation(&cache_root.join("operation.lock"), &alive, deadline)?;
    headroom(&cache_root, 0)?;
    let cache = cache_root.join("repository.git");
    let repository = if cache.exists() {
        git2::Repository::open_bare(&cache)
    } else {
        git2::Repository::init_bare(&cache)
    }
    .map_err(|_| "managed Git cache unavailable")?;
    let source_ref = if base_ref.starts_with("refs/") {
        base_ref.into()
    } else {
        format!("refs/heads/{base_ref}")
    };
    if !git2::Reference::is_valid_name(&source_ref) {
        return Err("baseRef must name a branch or tag".into());
    }
    // Only the transfer's own counts: Git reports several stages, each
    // counting from zero, and only these measure objects arriving.
    let fetched = |reading: GitProgress| {
        if FETCH_STAGES.contains(&reading.stage.as_str()) {
            progress(PrepareProgress::Fetching {
                objects: Some(reading.done),
                total: reading.total,
            })
        }
    };
    if state.commit.is_none() {
        progress(PrepareProgress::Fetching {
            objects: None,
            total: None,
        });
        if let Some(seed) = seed {
            let local = git2::Repository::open(&seed)
                .map_err(|_| "recorded local repository unavailable")?;
            let origin = local
                .find_remote("origin")
                .map_err(|_| "recorded repository lacks origin")?;
            if Result::ok(origin.url())
                .and_then(crate::context_repo::canonical_remote_key)
                .as_deref()
                != Some(repository_key.as_str())
            {
                return Err("recorded local repository identity changed".into());
            }
            // Only committed objects from the explicitly selected ref; never index/dirty files.
            // Seed all recorded committed heads first so a missing requested ref negotiates
            // with already-owned objects rather than cloning the provider's history again.
            let mut seed_command = Command::new("git");
            seed_command
                .current_dir(&cache)
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "fetch",
                    "--progress",
                    "--no-tags",
                    "--no-write-fetch-head",
                    "--",
                ])
                .arg(&seed)
                .args([
                    "+refs/heads/*:refs/codesense/local/heads/*",
                    "+refs/remotes/origin/*:refs/codesense/local/origin/*",
                ]);
            if let Err(all_refs) = run_with_progress(seed_command, &alive, deadline, None, &fetched)
            {
                if !all_refs.starts_with("local Git operation failed") {
                    return Err(all_refs);
                }
                // One broken object under another ref must not block the base ref.
                // Best effort: if the base ref is also missing, the guidance below applies.
                let mut base = Command::new("git");
                base.current_dir(&cache)
                    .args([
                        "-c",
                        "core.hooksPath=/dev/null",
                        "fetch",
                        "--progress",
                        "--no-tags",
                        "--no-write-fetch-head",
                        "--",
                    ])
                    .arg(&seed)
                    .arg(format!("+{source_ref}:refs/codesense/local/base"));
                if let Err(base_error) = run_with_progress(base, &alive, deadline, None, &fetched) {
                    // Neither fetch worked and the base ref is not local: git's own reason
                    // says more than the generic guidance below.
                    if local
                        .revparse_single(&source_ref)
                        .and_then(|o| o.peel_to_commit())
                        .is_err()
                    {
                        return Err(base_error);
                    }
                }
            }
            if let Ok(object) = local
                .revparse_single(&source_ref)
                .and_then(|o| o.peel_to_commit())
            {
                let pinned = object.id().to_string();
                let mut command = Command::new("git");
                command
                    .current_dir(&cache)
                    .args([
                        "-c",
                        "core.hooksPath=/dev/null",
                        "fetch",
                        "--progress",
                        "--no-tags",
                        "--no-write-fetch-head",
                        "--",
                    ])
                    .arg(&seed)
                    .arg(format!("+{pinned}:refs/heads/codesense-prepared"));
                run_with_progress(command, &alive, deadline, None, &fetched)?;
                state.commit = Some(pinned);
                save_state(&dir, &state)?;
            };
        }
    }
    if state.commit.is_none() {
        return Err(format!("Requested ref '{base_ref}' is unavailable locally. Prepare it in the attached repository using your agent or Git CLI, then retry."));
    }
    let commit = state.commit.clone().ok_or("pinned commit unavailable")?;
    let oid = git2::Oid::from_str(&commit).map_err(|_| "pinned commit invalid")?;
    repository
        .find_commit(oid)
        .map_err(|_| "pinned commit unavailable")?;
    repository
        .reference(
            "refs/heads/codesense-prepared",
            oid,
            true,
            "prepare owned bundle",
        )
        .map_err(|_| "cannot name pinned bundle ref")?;
    repository
        .reference(
            &format!("refs/codesense/requests/{key}"),
            oid,
            true,
            "retain immutable workspace intent",
        )
        .map_err(|_| "cannot retain pinned request commit")?;
    if !alive() {
        return Err("local Git preparation authority retired".into());
    }
    progress(PrepareProgress::Bundling { objects: None });
    let knowledge = probe(&commit)?;
    if !alive() {
        return Err("local Git preparation authority retired".into());
    }
    if knowledge.has_commit {
        let bundle = GitBundle {
            size: 0,
            digest: format!("{:x}", Sha256::digest([])),
            commit,
        };
        state.bundle = Some(bundle.clone());
        save_state(&dir, &state)?;
        return Ok((path, bundle));
    }
    let prerequisite = knowledge
        .imported_commits
        .iter()
        .filter_map(|commit| git2::Oid::from_str(commit).ok())
        .find(|prior| {
            repository.find_commit(*prior).is_ok()
                && repository.graph_descendant_of(oid, *prior).unwrap_or(false)
        });
    let bundle_command = |with_progress: bool| {
        let mut command = Command::new("git");
        command
            .current_dir(&cache)
            .args(["-c", "core.hooksPath=/dev/null", "bundle", "create"]);
        if with_progress {
            command.arg("--progress");
        }
        command.arg(&pending).arg("refs/heads/codesense-prepared");
        if let Some(prerequisite) = prerequisite {
            command.arg(format!("^{prerequisite}"));
        }
        command
    };
    run_or_without_progress(
        &bundle_command,
        &alive,
        deadline,
        Some(&pending),
        &|reading| {
            if reading.stage == BUNDLE_STAGE {
                progress(PrepareProgress::Bundling {
                    objects: Some(reading.total.unwrap_or(reading.done)),
                })
            }
        },
    )?;
    let (size, digest) = inspect(&pending)?;
    let bundle = GitBundle {
        size,
        digest,
        commit,
    };
    state.bundle = Some(bundle.clone());
    save_state(&dir, &state)?;
    std::fs::rename(&pending, &path).map_err(|_| "bundle publication unavailable")?;
    File::open(&dir)
        .and_then(|f| f.sync_all())
        .map_err(|_| "bundle publication not durable")?;
    Ok((path, bundle))
}

/// Drop everything the desktop kept for bundle `key`: its directory (state,
/// bytes, lock) and the cache ref that pinned its commit.
pub(crate) fn discard(root: &Path, key: &str, repository_key: &str) -> Result<(), String> {
    let dir = root.join(format!("bundle-{key}"));
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) if meta.is_dir() => {
            std::fs::remove_dir_all(&dir).map_err(|_| "local bundle cleanup pending")?
        }
        Ok(_) => return Err("local bundle directory is unsafe".into()),
        Err(_) => {}
    }
    let cache = root
        .join(format!(
            "repository-{:x}",
            Sha256::digest(repository_key.as_bytes())
        ))
        .join("repository.git");
    if cache.exists() {
        let repository =
            git2::Repository::open_bare(&cache).map_err(|_| "managed Git cache unavailable")?;
        let found = repository.find_reference(&format!("refs/codesense/requests/{key}"));
        if let Ok(mut reference) = found {
            reference
                .delete()
                .map_err(|_| "local bundle ref cleanup pending")?;
        }
    }
    Ok(())
}

pub(crate) fn settle(path: &Path, expected: &GitBundle) -> Result<(), String> {
    let dir = path.parent().ok_or("bundle directory unavailable")?;
    let _lock = lock_operation(
        &dir.join("operation.lock"),
        &|| true,
        Instant::now() + Duration::from_secs(5),
    )?;
    let mut state = read_state(&dir.join("intent.json"))?;
    if state.bundle.as_ref() != Some(expected) {
        return Err("settled bundle identity changed".into());
    }
    state.settled = true;
    save_state(dir, &state)?;
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => {
            std::fs::remove_file(path).map_err(|_| "settled local bundle cleanup pending")?;
            File::open(dir)
                .and_then(|file| file.sync_all())
                .map_err(|_| "settled bundle cleanup not durable")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        _ => return Err("settled bundle staging is unsafe".into()),
    }
    Ok(())
}

pub(crate) fn inspect(path: &Path) -> Result<(u64, String), String> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "original bundle unavailable")?;
    let meta = file.metadata().map_err(|_| "original bundle unavailable")?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_BUNDLE_BYTES {
        return Err("Git bundle must be a regular file no larger than 64 GiB".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| "cannot read original bundle")?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| "bundle is not durable")?;
    Ok((meta.len(), format!("{:x}", hash.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn move_bundle_carries_named_unpushed_branch_tip_to_a_mock_machine() {
        let dir = tempfile::tempdir().unwrap();
        let seed = dir.path().join("source");
        let repo = git2::Repository::init(&seed).unwrap();
        repo.remote("origin", "https://github.com/example/work.git")
            .unwrap();
        let signature = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
        let tree_id = repo.treebuilder(None).unwrap().write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let base = repo
            .commit(
                Some("refs/heads/main"),
                &signature,
                &signature,
                "base",
                &tree,
                &[],
            )
            .unwrap();
        let parent = repo.find_commit(base).unwrap();
        let tip = repo
            .commit(
                Some("refs/heads/story"),
                &signature,
                &signature,
                "unpushed",
                &tree,
                &[&parent],
            )
            .unwrap();
        let (path, bundle) = build_reporting(
            dir.path(),
            "move",
            "move",
            BuildSource {
                url: "https://github.com/example/work.git".into(),
                repository_key: "github.com/example/work".into(),
                base_ref: "refs/heads/story".into(),
                seed: Some(seed),
            },
            || true,
            |_| Ok(RepositoryKnowledge::default()),
            &|_| {},
        )
        .unwrap();
        assert_eq!(bundle.commit, tip.to_string());
        let machine = dir.path().join("machine");
        git2::Repository::init(&machine).unwrap();
        for args in [
            vec![
                "fetch",
                path.to_str().unwrap(),
                "refs/heads/codesense-prepared",
            ],
            vec!["checkout", "-b", "story", &bundle.commit],
        ] {
            let output = Command::new("git")
                .current_dir(&machine)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(
            git2::Repository::open(machine)
                .unwrap()
                .head()
                .unwrap()
                .target(),
            Some(tip)
        );
        assert_eq!(
            repo.find_reference("refs/heads/main").unwrap().target(),
            Some(base)
        );
    }

    #[test]
    fn missing_local_repository_ref_requires_external_preparation() {
        let temp = tempfile::tempdir().unwrap();
        let error = super::build(
            temp.path(),
            "missing",
            "intent",
            BuildSource {
                url: "https://github.com/acme/repo.git".into(),
                repository_key: "github.com/acme/repo".into(),
                base_ref: "main".into(),
                seed: None,
            },
            || true,
            |_| panic!("missing local objects must not contact the remote machine"),
        )
        .unwrap_err();
        assert!(error.contains("Prepare it in the attached repository"));
    }

    #[test]
    fn verified_local_objects_seed_shared_cache_and_incremental_or_empty_transfer() {
        let temp = tempfile::tempdir().unwrap();
        let seed = temp.path().join("source");
        let repo = git2::Repository::init(&seed).unwrap();
        let url = "https://github.com/example/work.git";
        repo.remote("origin", url).unwrap();
        let repository_key = crate::context_repo::canonical_remote_key(url).unwrap();
        let mut random = 17u32;
        let data: Vec<u8> = (0..256 * 1024)
            .map(|_| {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                random as u8
            })
            .collect();
        let blob = repo.blob(&data).unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        builder.insert("large.bin", blob, 0o100644).unwrap();
        let tree_id = builder.write().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
        let first = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
            .unwrap();
        repo.set_head("refs/heads/main").unwrap();
        std::fs::write(seed.join("dirty.txt"), "uncommitted desktop work").unwrap();
        let source = || BuildSource {
            url: url.into(),
            repository_key: repository_key.clone(),
            base_ref: "main".into(),
            seed: Some(seed.clone()),
        };
        let (_, initial) = super::build(
            temp.path(),
            "first",
            "first",
            source(),
            || true,
            |_| Ok(RepositoryKnowledge::default()),
        )
        .unwrap();
        assert_eq!(initial.commit, first.to_string());
        let parent = repo.find_commit(first).unwrap();
        let second = repo
            .commit(
                Some("refs/heads/main"),
                &sig,
                &sig,
                "second",
                &tree,
                &[&parent],
            )
            .unwrap();
        let (path, delta) = super::build(
            temp.path(),
            "second",
            "second",
            source(),
            || true,
            |_| {
                Ok(RepositoryKnowledge {
                    has_commit: false,
                    imported_commits: vec![first.to_string()],
                })
            },
        )
        .unwrap();
        assert_eq!(delta.commit, second.to_string());
        assert!(
            delta.size < initial.size / 4,
            "incremental={} full={}",
            delta.size,
            initial.size
        );
        let header = std::fs::read(&path).unwrap();
        assert!(header
            .windows(41)
            .any(|w| w == format!("-{first}").as_bytes()));
        let (empty_path, empty) = super::build(
            temp.path(),
            "third",
            "third",
            source(),
            || true,
            |_| {
                Ok(RepositoryKnowledge {
                    has_commit: true,
                    imported_commits: vec![],
                })
            },
        )
        .unwrap();
        assert_eq!(empty.size, 0);
        assert!(!empty_path.exists());
        assert_eq!(repo.head().unwrap().target(), Some(second));
        assert_eq!(
            std::fs::read_to_string(seed.join("dirty.txt")).unwrap(),
            "uncommitted desktop work"
        );
        let caches = std::fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("repository-"))
            .count();
        assert_eq!(caches, 1);
        settle(&path, &delta).unwrap();
        assert!(!path.exists(), "completed desktop staging reclaimed");
        let (_, reconciled) = super::build(
            temp.path(),
            "second",
            "second",
            source(),
            || true,
            |_| panic!("settled operation must use its durable metadata"),
        )
        .unwrap();
        assert_eq!(reconciled, delta);
        repo.remote_set_url("origin", "https://github.com/other/repository.git")
            .unwrap();
        assert!(super::build(
            temp.path(),
            "wrong",
            "wrong",
            source(),
            || true,
            |_| Ok(RepositoryKnowledge::default()),
        )
        .is_err());
    }
    #[test]
    fn supervised_git_wait_has_a_real_deadline_before_any_output() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30 & wait"]);
        let started = Instant::now();
        assert!(run(
            command,
            &|| true,
            started + Duration::from_millis(100),
            None
        )
        .is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn xs9_parse_git_progress_reads_counts_from_every_line_shape() {
        let reading = |line: &str| parse_git_progress(line);
        assert_eq!(
            reading("Receiving objects:  45% (450/1000), 1.20 MiB | 2.00 MiB/s"),
            Some(GitProgress {
                stage: "Receiving objects".into(),
                done: 450,
                total: Some(1000)
            })
        );
        assert_eq!(
            reading("remote: Counting objects: 100% (5/5), done."),
            Some(GitProgress {
                stage: "Counting objects".into(),
                done: 5,
                total: Some(5)
            })
        );
        assert_eq!(
            reading("Enumerating objects: 12, done."),
            Some(GitProgress {
                stage: "Enumerating objects".into(),
                done: 12,
                total: None
            })
        );
        assert_eq!(
            reading("Resolving deltas: 100% (3/3), done.").map(|r| r.stage),
            Some("Resolving deltas".to_string())
        );
        for noise in [
            "",
            "From /tmp/seed",
            " * [new branch]      main -> refs/codesense/local/heads/main",
            "Receiving objects: lots",
            "warning: objects: not a count (x/y)",
        ] {
            assert_eq!(reading(noise), None, "{noise:?}");
        }
        assert!(FETCH_STAGES.contains(&"Receiving objects"));
        assert_eq!(BUNDLE_STAGE, "Writing objects");
    }

    /// A Git that rejects `--progress` still gets its bundle.
    #[test]
    fn xs9_a_failing_progress_run_falls_back_to_a_plain_run() {
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("plain");
        let script = temp.path().join("git-like");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\ncase \"$*\" in *--progress*) exit 129;; esac\ntouch '{}'\n",
                marker.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let command = |progress: bool| {
            let mut command = Command::new(&script);
            if progress {
                command.arg("--progress");
            }
            command
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        assert!(run_with_progress(command(true), &|| true, deadline, None, &|_| {}).is_err());
        run_or_without_progress(&command, &|| true, deadline, None, &|_| {}).unwrap();
        assert!(marker.exists());
    }
}

#[cfg(test)]
mod git_failure_reason_tests {
    use super::*;

    fn failing(script: &str) -> String {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        let deadline = Instant::now() + Duration::from_secs(20);
        run_inner(command, &|| true, deadline, None, None, None, None).unwrap_err()
    }

    #[test]
    fn a_failed_git_run_reports_its_fatal_line() {
        let err = failing("echo 'fatal: bad tree object abc123' >&2; exit 128");
        assert!(err.contains("bad tree object abc123"), "{err}");
        assert!(!err.contains("account"), "{err}");
    }

    #[test]
    fn a_failure_without_output_says_git_gave_no_reason() {
        let err = failing("exit 1");
        assert_eq!(err, "local Git operation failed; git gave no reason");
    }
}

#[cfg(test)]
mod git_diagnostic_tests {
    use super::*;

    fn failing(script: &str) -> String {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        let deadline = Instant::now() + Duration::from_secs(20);
        run_inner(command, &|| true, deadline, None, None, None, None).unwrap_err()
    }

    #[test]
    fn the_fatal_line_wins_over_later_output() {
        let err =
            failing("echo 'fatal: bad tree object abc123' >&2; echo 'cleanup noise' >&2; exit 128");
        assert!(err.contains("bad tree object abc123"), "{err}");
    }

    #[test]
    fn a_fatal_line_after_many_noise_lines_is_kept() {
        let err = failing(
            "i=0; while [ $i -lt 40 ]; do echo \"* [new branch] b$i\" >&2; i=$((i+1)); done; echo 'fatal: real reason' >&2; exit 128",
        );
        assert!(err.contains("real reason"), "{err}");
    }

    #[test]
    fn a_remote_prefixed_fatal_line_is_the_reason() {
        let err = failing("echo 'remote: fatal: bad tree object aacf29' >&2; exit 128");
        assert!(err.contains("bad tree object aacf29"), "{err}");
    }

    #[test]
    fn credentials_in_a_git_url_are_not_reported() {
        let err = failing("echo \"fatal: unable to access 'https://user:s3cret@example.com/repo.git/'\" >&2; exit 128");
        assert!(!err.contains("s3cret") && !err.contains("user:"), "{err}");
        assert!(err.contains("https://example.com/repo.git/"), "{err}");
    }
}

#[cfg(test)]
mod seed_fallback_tests {
    use super::*;

    #[test]
    fn a_broken_non_base_ref_does_not_block_the_base_ref() {
        let temp = tempfile::tempdir().unwrap();
        let seed = temp.path().join("source");
        let repo = git2::Repository::init(&seed).unwrap();
        let url = "https://github.com/example/work.git";
        repo.remote("origin", url).unwrap();
        let repository_key = crate::context_repo::canonical_remote_key(url).unwrap();
        let sig = git2::Signature::now("Fixture", "fixture@example.invalid").unwrap();
        let mut builder = repo.treebuilder(None).unwrap();
        builder
            .insert("a.txt", repo.blob(b"base").unwrap(), 0o100644)
            .unwrap();
        let tree = repo.find_tree(builder.write().unwrap()).unwrap();
        let base = repo
            .commit(Some("refs/heads/main"), &sig, &sig, "base", &tree, &[])
            .unwrap();
        // A second branch whose tree object is then removed from the seed.
        let mut other = repo.treebuilder(None).unwrap();
        other
            .insert("b.txt", repo.blob(b"other").unwrap(), 0o100644)
            .unwrap();
        let other_tree = repo.find_tree(other.write().unwrap()).unwrap();
        repo.commit(
            Some("refs/heads/broken"),
            &sig,
            &sig,
            "broken",
            &other_tree,
            &[],
        )
        .unwrap();
        let object = other_tree.id().to_string();
        std::fs::remove_file(
            seed.join(".git/objects")
                .join(&object[..2])
                .join(&object[2..]),
        )
        .unwrap();
        let source = BuildSource {
            url: url.into(),
            repository_key,
            base_ref: "main".into(),
            seed: Some(seed),
        };
        let (_, bundle) = super::build(
            temp.path(),
            "broken-ref",
            "broken-ref",
            source,
            || true,
            |_| Ok(RepositoryKnowledge::default()),
        )
        .unwrap();
        assert_eq!(bundle.commit, base.to_string());
    }
}

#[cfg(test)]
mod redaction_tests {
    use super::redact_userinfo;

    #[test]
    fn userinfo_is_removed_even_with_a_slash_in_the_password() {
        let out =
            redact_userinfo("fatal: unable to access 'https://user:pa/ss@example.com/repo.git/'");
        assert_eq!(
            out,
            "fatal: unable to access 'https://example.com/repo.git/'"
        );
    }

    #[test]
    fn a_url_without_credentials_is_unchanged() {
        let text = "fatal: unable to access 'https://example.com/repo.git/'";
        assert_eq!(redact_userinfo(text), text);
    }
}
