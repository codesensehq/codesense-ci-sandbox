// SPDX-License-Identifier: MIT
//! Pinned, verified installation into CodeSense's own bin directory. No global setup.
use super::{
    detect::{self, Detection},
    managed::Managed,
    version::Version,
    AttributionStatus,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    future::Future,
    io::{self, Write},
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    path::{Path, PathBuf},
    pin::Pin as FuturePin,
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(crate) struct Pin {
    pub expected_version: &'static str,
    pub asset: &'static str,
    pub platform: &'static str,
    pub resource: &'static str,
    pub sha256: String,
}
// The builder and runtime read the same release manifest. Before changing its hash,
// run `codesign -dv` and `spctl -a -t install` on the new asset.
pub(crate) static PIN: std::sync::LazyLock<Pin> = std::sync::LazyLock::new(|| {
    Pin::parse(include_str!("../../git-ai.pin")).expect("invalid bundled git-ai pin")
});
impl Pin {
    fn parse(text: &'static str) -> Result<Self, &'static str> {
        let mut values = HashMap::new();
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| !str::is_empty(l) && !l.starts_with('#'))
        {
            let (key, value) = line.split_once('=').ok_or("expected key=value")?;
            if str::is_empty(value) || values.insert(key, value).is_some() {
                return Err("empty or duplicate pin value");
            }
        }
        let get = |key| values.get(key).copied().ok_or("missing pin value");
        let expected_version = get("version")?;
        if Version::parse(expected_version).is_none() {
            return Err("invalid pin version");
        }
        let sha256 = get("sha256")?;
        if sha256.len() != 64
            || !Iterator::all(&mut str::bytes(sha256), |c| {
                c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
            })
        {
            return Err("invalid pin sha256");
        }
        if get("tag")? != format!("v{expected_version}") || get("repo")? != "alfypops/git-ai" {
            return Err("invalid pin release");
        }
        Ok(Self {
            expected_version,
            asset: get("asset")?,
            platform: get("platform")?,
            resource: "resources/git-ai",
            sha256: sha256.into(),
        })
    }
}
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const BACKOFF: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct Failure {
    message: String,
    sticky: bool,
}
impl Failure {
    fn retryable(e: impl std::fmt::Display) -> Self {
        Self {
            message: e.to_string(),
            sticky: false,
        }
    }
    fn mismatch(message: &str) -> Self {
        Self {
            message: message.into(),
            sticky: true,
        }
    }
}
type FetchFuture<'a> = FuturePin<Box<dyn Future<Output = Result<(), Failure>> + Send + 'a>>;
trait Fetch: Send + Sync {
    fn fetch<'a>(&'a self, sink: &'a mut (dyn Write + Send)) -> FetchFuture<'a>;
}
/// The app already carries the signed asset; runtime installation never uses the network.
struct Bundled {
    path: Option<PathBuf>,
}
impl Fetch for Bundled {
    fn fetch<'a>(&'a self, sink: &'a mut (dyn Write + Send)) -> FetchFuture<'a> {
        Box::pin(async move {
            let path = self
                .path
                .as_ref()
                .ok_or_else(|| Failure::mismatch("bundle_missing: bundled git-ai is missing"))?;
            let mut source = File::open(path).map_err(|error| {
                if std::io::Error::kind(&error) == io::ErrorKind::NotFound {
                    Failure::mismatch("bundle_missing: bundled git-ai is missing")
                } else {
                    Failure::retryable(error)
                }
            })?;
            io::copy(&mut source, sink).map_err(Failure::retryable)?;
            Ok(())
        })
    }
}

/// Persisted rollback facts, also returned by the attribution doctor.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviousBinary {
    pub version: String,
    pub replacement: String,
    pub path: PathBuf,
}
#[derive(Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Notice {
    #[default]
    Pending,
    Shown,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Previous {
    #[serde(flatten)]
    binary: PreviousBinary,
    sha256: String,
    #[serde(default)]
    notice: Notice,
}
fn previous(home: &Path) -> Option<Previous> {
    Result::ok(serde_json::from_slice(&Result::ok(std::fs::read(
        home.join("bin/.git-ai-previous"),
    ))?))
}
pub(crate) fn previous_binary(home: &Path) -> Option<PreviousBinary> {
    previous(home).map(|previous| previous.binary)
}
/// Who is reading the one-time replacement notice: `describe_setup` acknowledges it; a passive
/// read (the dashboards' `setup` query) peeks and leaves the receipt untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReplacementNotice {
    Acknowledge,
    Peek,
}
fn notice_text(previous: &Previous) -> String {
    format!(
        "replaced git-ai {} → {} (previous kept at {})",
        previous.binary.version,
        previous.binary.replacement,
        Path::display(&previous.binary.path)
    )
}
/// A pending notice's text without writing or locking.
fn peek_replacement_notice(home: &Path) -> Option<String> {
    let previous = previous(home)?;
    (previous.notice == Notice::Pending).then(|| notice_text(&previous))
}
/// Acknowledge the receipt under the same cross-process lock as replacement.
/// Re-read after locking: another diagnostic reader may have announced it first.
fn take_replacement_notice(home: &Path) -> Option<String> {
    if previous(home)?.notice == Notice::Shown {
        return None;
    }
    let _lock = Result::ok(InstallLock::acquire(&home.join("bin")))?;
    let mut previous = previous(home)?;
    if previous.notice == Notice::Shown {
        return None;
    }
    previous.notice = Notice::Shown;
    let receipt = home.join("bin/.git-ai-previous");
    Result::ok(std::fs::write(
        &receipt,
        Result::ok(serde_json::to_vec(&previous))?,
    ))?;
    Result::ok(File::open(&receipt).and_then(|file| file.sync_all()))?;
    Some(notice_text(&previous))
}
pub(crate) const USER_PIN_RECOVERY: &str = r#"user_pinned: the restored binary is respected; reinstall the bundled build with save_source_repo_attachment {"repoPath":"<absolute checkout path>","attribution":"on"}"#;
fn persisted_failure(home: &Path) -> Option<String> {
    Result::ok(std::fs::read_to_string(
        home.join("bin/.git-ai-install-failure"),
    ))
}
fn failed_status(home: &Path) -> Option<AttributionStatus> {
    persisted_failure(home).map(|message| AttributionStatus::InstallFailed { message })
}
fn user_pinned(home: &Path) -> bool {
    Path::exists(&home.join("bin/user-restored"))
}
fn hash_file(path: &Path) -> io::Result<String> {
    let mut hash = Sha256::new();
    io::copy(&mut File::open(path)?, &mut hash)?;
    Ok(crate::shared::hash::digest_hex(&hash.finalize()))
}

/// flock is held by the descriptor until it closes, including on an error or unwind.
struct InstallLock(File);
impl InstallLock {
    fn acquire(dir: &Path) -> Result<Self, Failure> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".git-ai.lock"))
            .map_err(Failure::retryable)?;
        loop {
            // SAFETY: valid descriptor, no pointers; released automatically on close.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                return Ok(Self(file));
            }
            let error = std::io::Error::last_os_error();
            if std::io::Error::kind(&error) != std::io::ErrorKind::Interrupted {
                return Err(Failure::retryable(error));
            }
        }
    }
}
impl Drop for InstallLock {
    fn drop(&mut self) {
        // SAFETY: descriptor remains open throughout drop.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
struct VerifiedWriter<'a> {
    file: &'a mut File,
    hash: Sha256,
    count: u64,
}
impl Write for VerifiedWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > MAX_BYTES.saturating_sub(self.count) {
            return Err(std::io::Error::other("git-ai asset exceeds 64 MB"));
        }
        let size = self.file.write(bytes)?;
        self.hash.update(&bytes[..size]);
        self.count += size as u64;
        Ok(size)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}
#[cfg(test)]
async fn install_once(
    home: &Path,
    path: Option<&OsStr>,
    fetch: &dyn Fetch,
    pin: &Pin,
) -> Result<(), Failure> {
    install_with_intent(home, path, fetch, pin, false).await
}
async fn install_with_intent(
    home: &Path,
    path: Option<&OsStr>,
    fetch: &dyn Fetch,
    pin: &Pin,
    explicit: bool,
) -> Result<(), Failure> {
    let dir = home.join("bin");
    std::fs::create_dir_all(&dir).map_err(Failure::retryable)?;
    let _lock = InstallLock::acquire(&dir)?;
    let result = install_locked(home, path, fetch, pin, explicit).await;
    let failure_path = dir.join(".git-ai-install-failure");
    match &result {
        Ok(_) => {
            let _ = std::fs::remove_file(failure_path);
        }
        Err(error) => {
            if let Err(write_error) = std::fs::write(failure_path, &error.message) {
                log::warn!("could not persist git-ai install failure: {write_error}");
            }
        }
    }
    result
}
async fn install_locked(
    home: &Path,
    path: Option<&OsStr>,
    fetch: &dyn Fetch,
    pin: &Pin,
    explicit: bool,
) -> Result<(), Failure> {
    let dir = home.join("bin");
    for entry in std::fs::read_dir(&dir).map_err(Failure::retryable)? {
        let entry = entry.map_err(Failure::retryable)?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".git-ai.tmp")
            && entry.file_type().map_err(Failure::retryable)?.is_file()
        {
            std::fs::remove_file(std::fs::DirEntry::path(&entry)).map_err(Failure::retryable)?;
        }
    }
    // Re-detect under the cross-process lock: the other app may have just installed it.
    let managed = Managed::for_codesense_home(home);
    let env = managed.daemon_env();
    let detected = detect::detect(home, path, &env);
    let own = dir.join("git-ai");
    if let Some(prev) = previous(home) {
        if hash_file(&own).is_ok_and(|sha| sha == prev.sha256) {
            std::fs::write(dir.join("user-restored"), &prev.sha256).map_err(Failure::retryable)?;
        }
    }
    if explicit && user_pinned(home) {
        // Only an explicit override of restored bytes releases rollback metadata.
        // Ordinary off/on checks must keep the receipt for a later manual revert.
        std::fs::remove_file(dir.join("user-restored")).map_err(Failure::retryable)?;
        match std::fs::remove_file(dir.join(".git-ai-previous")) {
            Ok(()) => {}
            Err(error) if std::io::Error::kind(&error) == io::ErrorKind::NotFound => {}
            Err(error) => return Err(Failure::retryable(error)),
        }
    }
    if !needs_install_for_pin(home, &detected, pin) {
        return Ok(());
    }
    let mut temp = tempfile::Builder::new()
        .prefix(".git-ai.tmp")
        .tempfile_in(&dir)
        .map_err(Failure::retryable)?;
    let mut sink = VerifiedWriter {
        file: temp.as_file_mut(),
        hash: Sha256::new(),
        count: 0,
    };
    fetch.fetch(&mut sink).await?;
    let digest = crate::shared::hash::digest_hex(&sink.hash.finalize());
    if digest != pin.sha256 {
        return Err(Failure::mismatch(
            "sha_mismatch: bundled git-ai changed (sha mismatch); retry on demand or restart CodeSense",
        ));
    }
    // chmod uses the open descriptor; nothing is executable until verification succeeds.
    temp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o755))
        .map_err(Failure::retryable)?;
    temp.as_file().sync_all().map_err(Failure::retryable)?;
    // Native executables must not still be open for writing at exec (ETXTBSY).
    // TempPath retains cleanup ownership after closing the writable descriptor.
    let temp = temp.into_temp_path();
    match detect::detect_binary(temp.to_path_buf(), &env, detect::VERSION_TIMEOUT) {
        Detection::Found { raw, .. } if raw == pin.expected_version => {}
        Detection::Found { .. } => {
            return Err(Failure::mismatch(
                "version_mismatch: bundled git-ai changed (version mismatch); retry on demand or restart CodeSense",
            ))
        }
        Detection::Broken { message, .. } => {
            return Err(Failure::retryable(format!(
                "git-ai version probe: {message}"
            )))
        }
        Detection::NotInstalled => {
            return Err(Failure::retryable(
                "git-ai version probe: staged binary not found",
            ))
        }
    }
    let directory = File::open(&dir).map_err(Failure::retryable)?;
    directory.sync_all().map_err(Failure::retryable)?;
    // The verify-to-rename gap is writable only by this user (accepted in D2).
    if managed.daemon_reachable() {
        let root = home.join("git-ai");
        std::fs::create_dir_all(&root).map_err(Failure::retryable)?;
        let stamp = root.join("daemon-binary-version");
        if !Path::exists(&stamp) {
            let old = match &detected {
                Detection::Found { raw, .. } => raw.clone(),
                _ => "unknown".into(),
            };
            std::fs::write(stamp, old).map_err(Failure::retryable)?;
        }
    }
    if let Detection::Found { raw, .. } = &detected {
        // Version text is used in a filename; keep the basename within our bin directory.
        let name: String = raw
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || ".-_".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let backup = dir.join(format!("git-ai.prev-{name}"));
        std::fs::copy(&own, &backup).map_err(Failure::retryable)?;
        File::open(&backup)
            .and_then(|f| f.sync_all())
            .map_err(Failure::retryable)?;
        let prev = Previous {
            binary: PreviousBinary {
                version: raw.clone(),
                replacement: pin.expected_version.into(),
                path: backup.clone(),
            },
            sha256: hash_file(&backup).map_err(Failure::retryable)?,
            notice: Notice::Pending,
        };
        std::fs::write(
            dir.join(".git-ai-previous"),
            serde_json::to_vec(&prev).map_err(Failure::retryable)?,
        )
        .map_err(Failure::retryable)?;
        for entry in std::fs::read_dir(&dir).map_err(Failure::retryable)? {
            let entry = entry.map_err(Failure::retryable)?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("git-ai.prev-")
                && std::fs::DirEntry::path(&entry) != backup
            {
                std::fs::remove_file(std::fs::DirEntry::path(&entry))
                    .map_err(Failure::retryable)?;
            }
        }
    }
    std::fs::rename(&temp, dir.join("git-ai")).map_err(Failure::retryable)?;
    directory.sync_all().map_err(Failure::retryable)?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Trigger {
    Startup,
    Launch,
    OnDemand,
}
#[derive(Default)]
struct State {
    running: bool,
    on_demand_pending: bool,
    failed: Option<(Failure, Instant)>,
}
struct Service {
    home: PathBuf,
    path: Option<OsString>,
    fetch: Arc<dyn Fetch>,
    pin: Pin,
    supported: bool,
    state: Mutex<State>,
    done: Condvar,
}
impl Service {
    fn new(
        home: PathBuf,
        path: Option<OsString>,
        fetch: Arc<dyn Fetch>,
        pin: Pin,
        supported: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            home,
            path,
            fetch,
            pin,
            supported,
            state: Mutex::new(State::default()),
            done: Condvar::new(),
        })
    }
    fn status_locked(&self, state: &State) -> Option<AttributionStatus> {
        if !self.supported {
            return Some(AttributionStatus::Unsupported {
                reason: "platform".into(),
            });
        }
        if state.running {
            Some(AttributionStatus::Installing)
        } else {
            state
                .failed
                .as_ref()
                .map(|(e, _)| AttributionStatus::InstallFailed {
                    message: e.message.clone(),
                })
                .or_else(|| failed_status(&self.home))
        }
    }
    fn request(self: &Arc<Self>, trigger: Trigger) -> Option<AttributionStatus> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !self.supported {
            return self.status_locked(&state);
        }
        if state.running {
            state.on_demand_pending |= trigger == Trigger::OnDemand;
            return self.status_locked(&state);
        }
        if trigger != Trigger::OnDemand
            && state
                .failed
                .as_ref()
                .is_some_and(|(e, at)| e.sticky || at.elapsed() < BACKOFF)
        {
            return self.status_locked(&state);
        }
        state.running = true;
        state.failed = None;
        let service = self.clone();
        let spawned = std::thread::Builder::new()
            .name("git-ai-install".into())
            .spawn(move || {
                let mut explicit = trigger == Trigger::OnDemand;
                loop {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .map_err(Failure::retryable)?
                            .block_on(install_with_intent(
                                &service.home,
                                service.path.as_deref(),
                                service.fetch.as_ref(),
                                &service.pin,
                                explicit,
                            ))
                    }))
                    .unwrap_or_else(|_| Err(Failure::retryable("git-ai installation interrupted")));
                    let mut state = service.state.lock().unwrap_or_else(|e| e.into_inner());
                    match result {
                        Ok(()) => state.failed = None,
                        Err(e) => state.failed = Some((e, Instant::now())),
                    }
                    if std::mem::take(&mut state.on_demand_pending) {
                        // Keep the same flight alive so waiters see the explicit result,
                        // even when the previous pass has just detected a manual restore.
                        explicit = true;
                        continue;
                    }
                    // A finished install wakes the checkpoint watcher so its watches register
                    // without another user event; this only queues work (no IO or policy reads).
                    super::watcher::refresh(&service.home);
                    state.running = false;
                    service.done.notify_all();
                    break;
                }
            });
        if let Err(e) = spawned {
            state.running = false;
            state.failed = Some((Failure::retryable(e), Instant::now()));
        }
        self.status_locked(&state)
    }
    fn wait(&self, timeout: Duration) -> Option<AttributionStatus> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, _) = self
            .done
            .wait_timeout_while(state, timeout, |s| s.running)
            .unwrap_or_else(|e| e.into_inner());
        self.status_locked(&state)
    }
}
fn services() -> &'static Mutex<HashMap<PathBuf, Arc<Service>>> {
    static SERVICES: OnceLock<Mutex<HashMap<PathBuf, Arc<Service>>>> = OnceLock::new();
    SERVICES.get_or_init(Mutex::default)
}
fn service(home: &Path, path: Option<&OsStr>) -> Option<Arc<Service>> {
    let _ = path;
    services()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(home)
        .cloned()
}
fn register_bundle(
    home: &Path,
    path: Option<&OsStr>,
    bundled_path: Option<PathBuf>,
    pin: Pin,
    supported: bool,
) -> Arc<Service> {
    services()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(home.into())
        .or_insert_with(|| {
            Service::new(
                home.into(),
                path.map(OsStr::to_os_string),
                Arc::new(Bundled { path: bundled_path }),
                pin,
                supported,
            )
        })
        .clone()
}
pub(crate) fn initialize(app: &tauri::AppHandle, home: &Path, path: Option<&OsStr>) {
    register_bundle(
        home,
        path,
        crate::commands::paths::resolve_bundled_resource(app, PIN.resource),
        PIN.clone(),
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
    );
}
/// Background triggers skip an existing flight. Explicit attribution-on waits up to 30s.
/// Call the explicit form from spawn_blocking, since MCP itself is asynchronous.
pub(crate) fn ensure_installed(
    home: &Path,
    path: Option<&OsStr>,
    trigger: Trigger,
) -> Option<AttributionStatus> {
    let service = service(home, path)?;
    let status = service.request(trigger);
    if trigger == Trigger::OnDemand {
        service.wait(Duration::from_secs(30))
    } else {
        status
    }
}
pub(crate) fn status(home: &Path) -> Option<AttributionStatus> {
    if let Some(service) = service(home, None) {
        let state = service.state.lock().unwrap_or_else(|e| e.into_inner());
        service.status_locked(&state)
    } else {
        failed_status(home)
    }
}
pub(crate) fn warning(home: &Path, notice: ReplacementNotice) -> Option<String> {
    match notice {
        ReplacementNotice::Acknowledge => take_replacement_notice(home),
        ReplacementNotice::Peek => peek_replacement_notice(home),
    }
    .or_else(|| {
        service(home, None)
            .and_then(|service| {
                let state = service.state.lock().unwrap_or_else(|e| e.into_inner());
                state
                    .failed
                    .as_ref()
                    .map(|(error, _)| error.message.clone())
            })
            .or_else(|| persisted_failure(home))
    })
}
#[cfg(test)]
#[path = "install_tests.rs"]
pub(crate) mod tests;

fn current_pin(home: &Path) -> Pin {
    service(home, None)
        .map(|s| s.pin.clone())
        .unwrap_or_else(|| PIN.clone())
}
pub(crate) fn expected_version(home: &Path) -> &'static str {
    current_pin(home).expected_version
}
pub(crate) fn needs_install(home: &Path, detection: &Detection) -> bool {
    let pin = current_pin(home);
    needs_install_for_pin(home, detection, &pin)
}
fn needs_install_for_pin(home: &Path, detection: &Detection, pin: &Pin) -> bool {
    let own = home.join("bin/git-ai");
    if user_pinned(home) {
        return false;
    }
    if std::fs::symlink_metadata(&own).is_ok_and(|m| m.file_type().is_symlink()) {
        return false;
    }
    match detection {
        Detection::Found {
            path,
            version: Some(v),
            raw,
        } => {
            path != &own
                || (raw != pin.expected_version
                    && *v <= Version::parse(pin.expected_version).expect("pinned release version"))
        }
        _ => true,
    }
}
/// Only an exact supported pin may capture, including when an old copy remains after a failure.
pub(crate) fn capture_status(
    home: &Path,
    raw: &str,
    version: Option<Version>,
) -> Option<AttributionStatus> {
    if user_pinned(home) {
        return Some(AttributionStatus::Unsupported {
            reason: "user_pinned".into(),
        });
    }
    let pin = current_pin(home);
    if version
        .is_some_and(|v| v > Version::parse(pin.expected_version).expect("pinned release version"))
    {
        return Some(AttributionStatus::Unsupported {
            reason: "newer".into(),
        });
    }
    (raw != pin.expected_version || !version.is_some_and(Version::in_range)).then(|| {
        status(home).unwrap_or_else(|| AttributionStatus::UnsupportedVersion { found: raw.into() })
    })
}

pub(crate) fn platform_status(home: &Path) -> Option<AttributionStatus> {
    #[cfg(test)]
    {
        status(home).filter(
            |s| matches!(s, AttributionStatus::Unsupported { reason } if reason == "platform"),
        )
    }
    #[cfg(not(test))]
    {
        let _ = home;
        (!cfg!(all(target_os = "macos", target_arch = "aarch64"))).then(|| {
            AttributionStatus::Unsupported {
                reason: "platform".into(),
            }
        })
    }
}
