// SPDX-License-Identifier: MIT
//! SW-PB-13: background updates in the beta channels.
//!
//! There is no updater schedule of its own. The update tick (`static_channel.rs`: at launch,
//! daily, on a wake) reads the channel's static manifest, and a newer `version` there hands this
//! module an `Offer`. It then asks the same `/v1/update/{target}/{arch}/latest.json` through the
//! plugin, with no credential, no install id and no query (SV-PBF-278), checks the size with a
//! HEAD of the URL the manifest names, and downloads the tarball from that URL.
//!
//! What a download must pass before it is staged:
//! - the manifest's version is the version the offer named;
//! - the plugin's minisign check against the channel's key, with `requireSignedVersion` on, so
//!   the signature's trusted comment must name that same version (an older signed build served
//!   as a newer one is refused);
//! - the tarball holds exactly one top-level `.app`, whose Info.plist says that version and
//!   declares the `codesense://` scheme only beta builds have;
//! - the app's bundle and its folder can be replaced without an administrator prompt;
//! - a size cap: the HEAD must announce a length under it, and the download stops once the
//!   bytes counted pass it;
//! - the latest manifest read still names that version (`set_gate`). The same holds again at
//!   quit.
//!
//! The verified bytes stay in memory, with the plugin's `Update`, until the app quits;
//! `install_on_exit` installs them then. SW-PB-23: Restart to update arms the exit with
//! `arm_restart` after the same gate; the exit then installs the same way and relaunches. Nothing else relaunches. A failed or refused
//! download stages nothing, and a later check tries again after a backoff. If the app is killed
//! before it quits, nothing is installed, and the next launch's check finds the update again: an
//! `Update` cannot be rebuilt from disk, so nothing is written there.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Runtime, Url};
use tauri_plugin_updater::{Update, UpdaterExt};

/// Locks a mutex, taking the value a panic left behind: the updater never panics on a lock.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Dotted numeric version. Any part that is not a number makes the whole version unparsable.
pub fn parse_version(v: &str) -> Option<Vec<u64>> {
    v.split('.').map(|p| p.parse::<u64>().ok()).collect()
}

/// The update request's timeout.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// The download's timeout. A build is a few hundred MB; the signed URL is good for 10 minutes
/// from issue, and a download that starts inside that window may take longer to finish.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// The largest update the app will download. A build's tarball is well under a third of this.
/// The plugin keeps the whole download in one growing buffer that this code can't size ahead,
/// so while it grows it may briefly need up to twice what it holds.
pub const MAX_UPDATE_BYTES: u64 = 512 * 1024 * 1024;
/// After this many failed attempts in a row, Settings → Beta says to install by hand.
pub const FAILURES_BEFORE_NOTICE: u32 = 3;
/// The first wait after a failure. Each further failure doubles it, up to half the check
/// interval.
pub const FIRST_BACKOFF_SECS: u64 = 60;
/// The URL scheme only beta builds declare (`tauri.beta.conf.json`).
pub const BETA_SCHEME: &str = "codesense";

/// What the channel's manifest offers. It carries no credential: the request is anonymous.
#[derive(Debug)]
pub struct Offer {
    pub version: String,
    /// The release notes from the manifest.
    pub notes: String,
    /// The user asked for this check (Check now), so the backoff after a failure is skipped.
    pub explicit: bool,
}

/// What one update attempt did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The offered version is not newer than the running one, or the manifest has none newer.
    NoUpdate,
    /// This version is already staged.
    AlreadyStaged,
    /// Another attempt is running.
    Busy,
    /// The last attempt failed and the backoff has not passed yet.
    BackingOff,
    /// The app can't be replaced without an administrator, so nothing was downloaded.
    NotWritable,
    /// The download verified, but by then the latest manifest named another version. Nothing
    /// was staged.
    Withdrawn,
    /// The version was downloaded, verified and staged.
    Staged(String),
    /// Nothing was staged. The label names the step and carries no secret.
    Failed(&'static str),
}

/// Why Settings → Beta asks the user to install a new build by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Attention {
    /// The app can't be replaced without an administrator.
    NotWritable,
    /// Several attempts in a row failed.
    Failing,
    /// Installing the offered build at a restart failed (SW-PB-23).
    InstallFailed,
    /// The update records can't be saved, so CodeSense won't restart itself for a required
    /// update. Restart to update still works (SW-PB-23).
    RecordsUnwritable,
}

/// The staged build as the UI shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedInfo {
    pub version: String,
    pub notes: String,
}

/// Asked before a verified build is staged and again before it installs: whether the latest
/// manifest still names this version.
pub type Gate = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// The channel's static manifest on its download host, for example `https://dl.codesense.ai`.
/// The plugin fills in the target and arch; the running version is never sent.
pub fn endpoint(base: &str) -> Result<Url, String> {
    format!(
        "{}/v1/update/{{{{target}}}}/{{{{arch}}}}/latest.json",
        base.trim_end_matches('/')
    )
    .parse()
    .map_err(|e| format!("update endpoint: {e}"))
}

/// Whether `file` is a plain object on the manifest's host: same scheme, host and port, with no
/// user info, query or fragment.
fn is_channel_file(file: &Url, manifest: &Url) -> bool {
    Url::origin(file) == Url::origin(manifest)
        && str::is_empty(Url::username(file))
        && Option::is_none(&Url::password(file))
        && Option::is_none(&Url::query(file))
        && Option::is_none(&Url::fragment(file))
}

/// True only when both versions parse and `candidate` is newer. Anything unparsable is never newer.
pub fn is_newer(candidate: &str, running: &str) -> bool {
    match (parse_version(candidate), parse_version(running)) {
        (Some(c), Some(r)) => {
            let len = c.len().max(r.len());
            for i in 0..len {
                let (x, y) = (
                    c.get(i).copied().unwrap_or(0),
                    r.get(i).copied().unwrap_or(0),
                );
                if x != y {
                    return x > y;
                }
            }
            false
        }
        _ => false,
    }
}

/// Whether the app installs at quit: only a staged build newer than the running one.
pub fn should_install_on_exit(staged: Option<&str>, running: &str) -> bool {
    staged.is_some_and(|v| is_newer(v, running))
}

/// Why Restart to update (or the deadline's restart) can't run now. SW-PB-23.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotReady {
    /// No verified build is staged.
    NothingStaged,
    /// The staged build is not newer than the running one.
    NotNewer,
    /// This Mac lost its access, or the latest check names another build.
    Withdrawn,
    /// The app can't be replaced where it is without an administrator.
    NotWritable,
    /// Installing this build at an earlier restart failed, so it is not tried that way again.
    InstallFailed,
    /// CodeSense is already closing.
    Closing,
    /// A forced restart's attempt couldn't be recorded (its loop guard).
    NotRecorded,
}

impl NotReady {
    /// What the command tells the UI. No secret, no path.
    pub fn message(self) -> &'static str {
        match self {
            NotReady::NothingStaged | NotReady::NotNewer => "No update is ready to install.",
            NotReady::Withdrawn => "This update is no longer offered to this Mac.",
            NotReady::Closing => "CodeSense is already closing.",
            NotReady::NotRecorded => {
                "The update records can't be written, so CodeSense won't restart itself."
            }
            NotReady::NotWritable | NotReady::InstallFailed => {
                "CodeSense can't install this update itself. Install it by hand."
            }
        }
    }
}

/// SW-PB-23: whether the staged build may be installed and the app relaunched now. The same
/// gate as the install at quit: a staged build newer than the running one, still named by the
/// latest manifest, at a location this user can replace, and not one whose install already
/// failed. The bundle checks ran on these exact
/// bytes when they were staged; staged bytes never change.
pub fn restart_readiness(
    staged: Option<&str>,
    running: &str,
    wanted: bool,
    writable: bool,
    failed: bool,
) -> Result<(), NotReady> {
    let Some(staged) = staged else {
        return Err(NotReady::NothingStaged);
    };
    if !is_newer(staged, running) {
        return Err(NotReady::NotNewer);
    }
    if !wanted {
        return Err(NotReady::Withdrawn);
    }
    if failed {
        return Err(NotReady::InstallFailed);
    }
    if !writable {
        return Err(NotReady::NotWritable);
    }
    Ok(())
}

/// What the app does as it exits: whether it installed a build, and whether it relaunches.
#[derive(Debug, PartialEq, Eq)]
pub struct ExitPlan {
    pub installed: Option<Result<String, String>>,
    /// True only when Restart to update or the deadline armed this exit. A plain quit never
    /// relaunches.
    pub relaunch: bool,
}

/// The file that records a build whose install at a restart failed.
const RESTART_FAILED_FILE: &str = "restart-install-failed";
/// The file written just before a relaunch onto a build: the next launch compares it with the
/// running version, so an install that reported success but left the old build, or a failure
/// whose record could not be written, still counts as a failure.
const RESTART_ATTEMPTED_FILE: &str = "restart-attempted";
/// Written when a forced restart is armed: proof the loop guard can be saved. Promoted to
/// `restart-attempted` just before the install, so a crash mid-drain isn't a failed install.
const RESTART_PENDING_FILE: &str = "restart-pending";
/// Written and removed once, to learn whether the folder takes the records.
const PROBE_FILE: &str = ".probe";
/// An armed restart: when, and whether the deadline forced it.
#[derive(Clone, Copy, Debug)]
struct Armed {
    at: Instant,
    forced: bool,
}

/// An armed exit that hasn't happened this long after the click is dropped.
pub const ARMED_FOR: Duration = Duration::from_secs(60);

/// The log line for a failed attempt. Built from the label only, so it never carries a token,
/// the install id or a signed URL.
pub fn failure_line(label: &str) -> String {
    format!("update not staged: {label}; it is tried again at a later check")
}

/// The shortest time after `failures` failed attempts in a row before a scheduled check may try
/// again: one minute, doubling, at most half of `check_interval`, so it always ends before the
/// next daily check is due. It is a minimum wait, not a timer: nothing retries when it ends;
/// the next check after it does (at launch or the daily check). Check now never waits for it.
pub fn backoff(failures: u32, check_interval: Duration) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    let shift = (failures - 1).min(32);
    Duration::from_secs(FIRST_BACKOFF_SECS.saturating_mul(1u64 << shift)).min(check_interval / 2)
}

/// The bundle the updater replaces for an executable: `X.app` for `X.app/Contents/MacOS/x`,
/// otherwise the executable's folder (the plugin's own rule).
pub fn install_target(executable: &Path) -> Option<PathBuf> {
    let dir = executable.parent()?;
    if dir.ends_with("Contents/MacOS") {
        return dir.parent()?.parent().map(Path::to_path_buf);
    }
    Some(dir.to_path_buf())
}

/// Whether a new bundle can be put in `target`'s place without an administrator.
pub fn can_replace(target: &Path) -> bool {
    can_replace_with(target, &std::env::temp_dir())
}

/// The plugin moves the running bundle into a folder under `temp` and the new one out of
/// another, both by rename, and asks for an administrator when a rename is refused. So the
/// bundle must be this user's and writable (a moved folder's `..` changes), its folder must
/// take a new entry, and `temp` must be on the same volume (a rename can't cross volumes).
#[cfg(unix)]
pub fn can_replace_with(target: &Path, temp: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(bundle), Ok(temp)) = (std::fs::symlink_metadata(target), std::fs::metadata(temp))
    else {
        return false;
    };
    let Some(parent) = target.parent() else {
        return false;
    };
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    bundle.is_dir()
        && bundle.uid() == uid
        && bundle.dev() == temp.dev()
        && rustix::fs::access(target, rustix::fs::Access::WRITE_OK).is_ok()
        && tempfile::tempfile_in(parent).is_ok()
}

#[cfg(not(unix))]
pub fn can_replace_with(_target: &Path, _temp: &Path) -> bool {
    false
}

/// What the app inside a verified tarball says about itself, read from
/// `<name>.app/Contents/Info.plist` without unpacking anything.
#[derive(Debug, PartialEq, Eq)]
pub struct BundleFacts {
    pub version: Option<String>,
    pub schemes: Vec<String>,
}

/// Reads the tarball's one top-level `.app`. A tarball with any other top-level entry, more
/// than one, or a path that leaves the archive, is refused: the plugin installs whatever the
/// first entry is.
pub fn bundle_facts(tarball: &[u8]) -> Result<BundleFacts, &'static str> {
    const UNREADABLE: &str = "the update's Info.plist could not be read";
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
    let mut top: Option<std::ffi::OsString> = None;
    let mut facts = None;
    // Symlinks seen so far, by their lower-cased path (APFS ignores case): the folder they sit
    // in, and their target.
    let mut symlinks: std::collections::HashMap<String, (Vec<String>, PathBuf)> =
        std::collections::HashMap::new();
    for entry in archive.entries().map_err(|_| UNREADABLE)? {
        let mut entry = entry.map_err(|_| UNREADABLE)?;
        let path = entry.path().map_err(|_| UNREADABLE)?.into_owned();
        let mut parts = Vec::new();
        for part in path.components() {
            match part {
                Component::CurDir => {}
                Component::Normal(name) => parts.push(name.to_os_string()),
                _ => return Err("the update holds a path outside the app"),
            }
        }
        let Some(first) = parts.first() else {
            continue;
        };
        match &top {
            None => top = Some(first.clone()),
            Some(seen) if seen != first => {
                return Err("the update holds more than one top-level item")
            }
            Some(_) => {}
        }
        let lower = lowered(&parts);
        // Unpacking writes through a folder that is really a symlink, so nothing may sit under one.
        if (1..lower.len()).any(|n| symlinks.contains_key(&lower[..n].join("/"))) {
            return Err("the update holds a path through a link");
        }
        let kind = entry.header().entry_type();
        if kind.is_hard_link() {
            return Err("the update holds a hard link");
        }
        if kind.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|_| UNREADABLE)?
                .ok_or(UNREADABLE)?
                .into_owned();
            symlinks.insert(lower.join("/"), (lower[..lower.len() - 1].to_vec(), target));
        }
        let is_plist = lower.len() == 3 && lower[1] == "contents" && lower[2] == "info.plist";
        if !is_plist {
            continue;
        }
        // A second Info.plist entry would overwrite the one checked here when unpacked.
        if facts.is_some() {
            return Err("the update holds more than one Info.plist");
        }
        let mut bytes = Vec::new();
        entry
            .by_ref()
            .take(1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|_| UNREADABLE)?;
        let value =
            plist::Value::from_reader(std::io::Cursor::new(bytes)).map_err(|_| UNREADABLE)?;
        let dict = value.as_dictionary().ok_or(UNREADABLE)?;
        let version = dict
            .get("CFBundleShortVersionString")
            .and_then(|v| v.as_string())
            .map(str::to_string);
        let schemes = dict
            .get("CFBundleURLTypes")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|t| t.as_dictionary()?.get("CFBundleURLSchemes")?.as_array())
            .flatten()
            .filter_map(|s| s.as_string().map(str::to_string))
            .collect();
        facts = Some(BundleFacts { version, schemes });
    }
    let top = top.ok_or("the update is empty")?;
    // Every symlink, followed through the others (in any order), must land inside the app.
    let top_lower = top.to_string_lossy().to_lowercase();
    for (dir, target) in symlinks.values() {
        let resolved = resolve_link(dir, target, &symlinks, 0);
        if resolved.as_ref().and_then(|at| at.first()) != Some(&top_lower) {
            return Err("the update holds a link that points outside the app");
        }
    }
    if !top.to_string_lossy().ends_with(".app") {
        return Err("the update's top-level item is not an app");
    }
    facts.ok_or("the update holds no app Info.plist")
}

fn lowered(parts: &[std::ffi::OsString]) -> Vec<String> {
    parts
        .iter()
        .map(|p| p.to_string_lossy().to_lowercase())
        .collect()
}

/// Where a symlink in folder `dir` (lower-cased archive components) pointing at `target` ends
/// up, following the archive's other symlinks on the way. None when it leaves the archive,
/// is absolute, or loops.
fn resolve_link(
    dir: &[String],
    target: &Path,
    symlinks: &std::collections::HashMap<String, (Vec<String>, PathBuf)>,
    depth: u32,
) -> Option<Vec<String>> {
    if depth > 32 {
        return None;
    }
    let mut at = dir.to_vec();
    for part in target.components() {
        match part {
            Component::CurDir => {}
            Component::Normal(name) => {
                at.push(name.to_string_lossy().to_lowercase());
                if let Some((link_dir, link_target)) = symlinks.get(&at.join("/")) {
                    at = resolve_link(link_dir, link_target, symlinks, depth + 1)?;
                }
            }
            Component::ParentDir => {
                at.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(at)
}

/// The checks on the app inside a verified tarball before it is staged.
pub fn check_bundle(tarball: &[u8], offered: &str) -> Result<(), &'static str> {
    let facts = bundle_facts(tarball)?;
    if facts.version.as_deref() != Some(offered) {
        return Err("the app in the update is not the offered version");
    }
    if !facts.schemes.iter().any(|s| s == BETA_SCHEME) {
        return Err("the app in the update is not a beta build");
    }
    Ok(())
}

/// A plugin error as a label with no URL, header or body in it.
fn label(error: &tauri_plugin_updater::Error) -> &'static str {
    use tauri_plugin_updater::Error as E;
    match error {
        E::Reqwest(_) | E::Network(_) => "the update host could not be reached",
        E::SignedVersionMismatch { .. } | E::MissingSignedVersion => {
            "the download is not signed for the offered version"
        }
        E::Minisign(_) | E::Base64(_) | E::SignatureUtf8(_) => "the download did not verify",
        E::ReleaseNotFound
        | E::Serialization(_)
        | E::Semver(_)
        | E::UrlParse(_)
        | E::TargetNotFound(_)
        | E::TargetsNotFound(_) => "the update reply was not usable",
        _ => "the updater failed",
    }
}

const TOO_LARGE: &str = "the update is larger than allowed";

struct Staged {
    version: String,
    notes: String,
    update: Update,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct Failures {
    count: u32,
    /// The wait ends once either clock passes its mark: the monotonic one, so setting the
    /// clock back can't stretch it, and the wall clock, so a Mac that slept through it (the
    /// monotonic clock stops in sleep) still downloads at the next daily check.
    retry_at: Option<(Instant, std::time::SystemTime)>,
}

/// Whether an attempt is running, and the latest offer that arrived meanwhile.
#[derive(Default)]
struct Slot {
    busy: bool,
    pending: Option<Offer>,
}

/// Clears the busy flag when an attempt ends early: a panic or a dropped future. The normal
/// end releases the slot itself and disarms the guard, so a run that has claimed the slot
/// since is never cleared.
struct BusyGuard<'a> {
    slot: &'a Mutex<Slot>,
    released: bool,
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let mut slot = lock(self.slot);
        slot.busy = false;
        // A cancelled attempt leaves no offer behind for a later run to pick up stale.
        slot.pending = None;
    }
}

/// The beta updater. Managed by the app; one per process.
pub struct Updates {
    endpoint: Url,
    running: String,
    /// The bundle an install replaces.
    target: Option<PathBuf>,
    /// Set only by tests, which install into a temporary folder.
    executable: Option<PathBuf>,
    /// The longest backoff: the check interval.
    max_backoff: Duration,
    max_bytes: u64,
    clock: Box<dyn Fn() -> Instant + Send + Sync>,
    wall: Box<dyn Fn() -> std::time::SystemTime + Send + Sync>,
    gate: OnceLock<Gate>,
    staged: Mutex<Option<Staged>>,
    failures: Mutex<Failures>,
    not_writable: AtomicBool,
    slot: Mutex<Slot>,
    /// SW-PB-23: set by Restart to update or the deadline once the gate passed; the exit that
    /// follows installs and relaunches.
    restart_armed: Mutex<Option<Armed>>,
    /// The update records (the restart loop guard) can be written. A forced restart needs it.
    records_durable: AtomicBool,
    /// The app is exiting: an armed restart is no longer dropped or cancelled.
    exiting: AtomicBool,
    /// The version the latest offer named.
    last_offered: Mutex<Option<String>>,
    /// Where the update records (the restart loop guard) live:
    /// the app data folder's `updates/`, if anywhere.
    marker_dir: Option<PathBuf>,
    /// The build whose install at a restart failed, from that record.
    restart_failed: Mutex<Option<String>>,
    /// The build the last install at an exit tried.
    last_attempt: Mutex<Option<String>>,
}

impl Updates {
    /// `base` is the updates host's base URL; `running` is this build's version; `max_backoff`
    /// is the check interval.
    pub fn new(base: &str, running: String, max_backoff: Duration) -> Result<Self, String> {
        Ok(Self {
            endpoint: endpoint(base)?,
            running,
            target: std::env::current_exe()
                .ok()
                .and_then(|exe| install_target(&exe)),
            executable: None,
            max_backoff,
            max_bytes: MAX_UPDATE_BYTES,
            clock: Box::new(Instant::now),
            wall: Box::new(std::time::SystemTime::now),
            gate: OnceLock::new(),
            staged: Mutex::new(None),
            failures: Mutex::new(Failures::default()),
            not_writable: AtomicBool::new(false),
            slot: Mutex::new(Slot::default()),
            restart_armed: Mutex::new(None),
            records_durable: AtomicBool::new(false),
            exiting: AtomicBool::new(false),
            last_offered: Mutex::new(None),
            marker_dir: None,
            restart_failed: Mutex::new(None),
            last_attempt: Mutex::new(None),
        })
    }

    /// Keeps the update records in `dir`, and reads them now: it checks that the folder takes a
    /// write, settles a restart attempt left by the last launch, and removes records for a build
    /// that is not newer than the running one (that build, or a later one, is running).
    pub fn with_marker_dir(mut self, dir: PathBuf) -> Self {
        self.marker_dir = Some(dir);
        // The folder must take a write, or there is no durable loop guard.
        let durable = self.write_marker(PROBE_FILE, "") && self.remove_marker(PROBE_FILE);
        self.records_durable.store(durable, Ordering::SeqCst);
        // A relaunch onto a build that left this one running is a failed install, whatever was
        // or wasn't recorded at the exit. The attempt record goes only once the failure is saved.
        if let Some(attempted) = self.read_marker(RESTART_ATTEMPTED_FILE) {
            let newer = is_newer(&attempted, &self.running);
            if newer {
                log::warn!(target: "license", "the restart onto update {attempted} left this build running; it is not tried that way again");
            }
            if !newer || self.record_failure(&attempted) {
                self.remove_marker(RESTART_ATTEMPTED_FILE);
            }
        }
        // A forced restart armed but never installed (the app crashed, or was killed, mid-drain)
        // is not a failed install.
        self.remove_marker(RESTART_PENDING_FILE);
        // Records for a build that is not newer than the running one are stale: that build, or
        // a later one, is running.
        if let Some(version) = self.read_marker(RESTART_FAILED_FILE) {
            if is_newer(&version, &self.running) {
                lock(&self.restart_failed).get_or_insert(version);
            } else {
                self.remove_marker(RESTART_FAILED_FILE);
            }
        }
        self
    }

    /// Installs into `bundle` (a `.app` path) instead of the running app. Tests only.
    #[cfg(test)]
    pub fn installing_into(mut self, bundle: &Path) -> Self {
        self.executable = Some(bundle.join("Contents/MacOS/codesense"));
        self.target = Some(bundle.to_path_buf());
        self
    }

    /// Replaces the monotonic clock. Tests only.
    #[cfg(test)]
    pub fn with_clock(mut self, clock: Box<dyn Fn() -> Instant + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    /// Replaces the wall clock. Tests only.
    #[cfg(test)]
    pub fn with_wall(mut self, wall: Box<dyn Fn() -> std::time::SystemTime + Send + Sync>) -> Self {
        self.wall = wall;
        self
    }

    /// Marks the update records unwritable. Tests only.
    #[cfg(test)]
    pub fn disable_records_for_test(&self) {
        self.records_durable.store(false, Ordering::SeqCst);
    }

    /// Lowers the size cap. Tests only.
    #[cfg(test)]
    pub fn with_max_bytes(mut self, max: u64) -> Self {
        self.max_bytes = max;
        self
    }

    /// Sets what decides whether a verified build may still be staged, and installed at quit.
    /// Only the first call takes effect. Without one, every verified build is wanted.
    pub fn set_gate(&self, gate: Gate) {
        let _ = self.gate.set(gate);
    }

    fn wanted(&self, version: &str) -> bool {
        self.gate.get().is_none_or(|gate| gate(version))
    }

    /// The staged version, if a verified build is waiting for the quit.
    pub fn staged_version(&self) -> Option<String> {
        lock(&self.staged).as_ref().map(|s| s.version.clone())
    }

    /// The staged build and its release notes.
    pub fn staged_info(&self) -> Option<StagedInfo> {
        lock(&self.staged).as_ref().map(|s| StagedInfo {
            version: s.version.clone(),
            notes: s.notes.clone(),
        })
    }

    /// Whether Settings → Beta should say to install the next build by hand.
    pub fn attention(&self) -> Option<Attention> {
        if self.not_writable.load(Ordering::SeqCst) {
            return Some(Attention::NotWritable);
        }
        // SW-PB-23: the build on offer (staged, or not yet staged) is one whose install failed at
        // a restart: install it by hand. A failure for any other build says nothing.
        let offered = self
            .staged_version()
            .or_else(|| lock(&self.last_offered).clone());
        let failed = self.failed_version();
        if failed.is_some() && failed == offered {
            return Some(Attention::InstallFailed);
        }
        let failures = lock(&self.failures);
        if failures.count >= FAILURES_BEFORE_NOTICE {
            return Some(Attention::Failing);
        }
        // Only a real records folder can be unwritable; a staged build makes it worth saying.
        (self.marker_dir.is_some() && !self.records_durable() && self.has_staged())
            .then_some(Attention::RecordsUnwritable)
    }

    /// Drops the staged build unless it is version `keep`. The update tick calls it with the
    /// manifest's version on every read, so a build the channel has rolled back never installs.
    /// Returns whether a build was dropped.
    pub fn clear_unless(&self, keep: Option<&str>) -> bool {
        // A withdrawn offer is no longer on offer: its install-by-hand line goes.
        {
            let mut offered = lock(&self.last_offered);
            if offered.as_deref().is_some_and(|v| Some(v) != keep) {
                *offered = None;
            }
        }
        // An offer waiting behind a running download is dropped the same way.
        {
            let mut slot = lock(&self.slot);
            if slot
                .pending
                .as_ref()
                .is_some_and(|p| Some(p.version.as_str()) != keep)
            {
                slot.pending = None;
            }
        }
        let mut staged = lock(&self.staged);
        if staged
            .as_ref()
            .is_some_and(|s| Some(s.version.as_str()) != keep)
        {
            *staged = None;
            return true;
        }
        false
    }

    fn reset_failures(&self) {
        *lock(&self.failures) = Failures::default();
    }

    /// Asks the manifest, downloads, verifies and stages. Silent: no prompt, no toast.
    /// Failures are logged by label, leave nothing staged and back off. An offer that arrives
    /// while an attempt runs is kept (the latest one wins) and tried as soon as that attempt
    /// ends, by the same call; that call returns the last outcome.
    pub async fn run<R: Runtime>(&self, app: &AppHandle<R>, offer: Offer) -> Outcome {
        {
            let mut slot = lock(&self.slot);
            if slot.busy {
                // The latest offer wins, but a Check now among them keeps skipping the backoff.
                let explicit = offer.explicit || slot.pending.as_ref().is_some_and(|p| p.explicit);
                slot.pending = Some(Offer { explicit, ..offer });
                return Outcome::Busy;
            }
            slot.busy = true;
        }
        let mut guard = BusyGuard {
            slot: &self.slot,
            released: false,
        };
        let mut offer = offer;
        loop {
            let outcome = self.attempt(app, &offer).await;
            let mut slot = lock(&self.slot);
            match slot.pending.take() {
                Some(next) => offer = next,
                None => {
                    slot.busy = false;
                    guard.released = true;
                    return outcome;
                }
            }
        }
    }

    async fn attempt<R: Runtime>(&self, app: &AppHandle<R>, offer: &Offer) -> Outcome {
        if !is_newer(&offer.version, &self.running) {
            self.reset_failures();
            return Outcome::NoUpdate;
        }
        if self.staged_version().as_deref() == Some(offer.version.as_str()) {
            self.reset_failures();
            return Outcome::AlreadyStaged;
        }
        // Nothing is downloaded once a newer manifest has withdrawn the offer.
        if !self.wanted(&offer.version) {
            return Outcome::Withdrawn;
        }
        // Check now is the user's own request: it never waits out the backoff.
        let backing_off = lock(&self.failures)
            .retry_at
            .is_some_and(|(at, wall)| (self.clock)() < at && (self.wall)() < wall);
        if backing_off && !offer.explicit {
            return Outcome::BackingOff;
        }
        // An install the user can't write would raise an administrator prompt at every quit.
        if !self.target.as_deref().is_some_and(can_replace) {
            self.not_writable.store(true, Ordering::SeqCst);
            log::warn!(target: "license", "update not downloaded: the app can't be replaced without an administrator");
            return Outcome::NotWritable;
        }
        self.not_writable.store(false, Ordering::SeqCst);
        match self.fetch(app, offer).await {
            Ok(Some((update, bytes))) => {
                let version = update.version.clone();
                {
                    // Asked under the lock a clear takes, so a newer manifest that lands while
                    // this download ran can't be missed.
                    let mut staged = lock(&self.staged);
                    if !self.wanted(&version) {
                        log::info!(target: "license", "update {version} not staged: the latest manifest no longer names it");
                        return Outcome::Withdrawn;
                    }
                    *staged = Some(Staged {
                        version: version.clone(),
                        notes: offer.notes.clone(),
                        update,
                        bytes,
                    });
                }
                self.reset_failures();
                log::info!(target: "license", "update {version} staged; it installs when CodeSense quits");
                Outcome::Staged(version)
            }
            Ok(None) => {
                self.reset_failures();
                Outcome::NoUpdate
            }
            Err(label) => {
                let mut failures = lock(&self.failures);
                failures.count = failures.count.saturating_add(1);
                let wait = backoff(failures.count, self.max_backoff);
                failures.retry_at = Some(((self.clock)() + wait, (self.wall)() + wait));
                log::warn!(target: "license", "{}", failure_line(label));
                Outcome::Failed(label)
            }
        }
    }

    async fn fetch<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        offer: &Offer,
    ) -> Result<Option<(Update, Vec<u8>)>, &'static str> {
        let mut builder = app
            .updater_builder()
            .endpoints(vec![self.endpoint.clone()])
            .map_err(|_| "the update endpoint was refused")?
            .timeout(CHECK_TIMEOUT)
            // No redirect, and a release build talks only HTTPS. Applies to the manifest
            // request and the file download.
            .configure_client(|client| {
                client
                    .redirect(reqwest_mcp::redirect::Policy::none())
                    .https_only(!cfg!(debug_assertions))
            });
        if let Some(executable) = &self.executable {
            builder = builder.executable_path(executable);
        }
        let updater = builder.build().map_err(|e| label(&e))?;
        let Some(mut update) = updater.check().await.map_err(|e| label(&e))? else {
            return Ok(None);
        };
        // The manifest must still name the build the offer named, nothing older or other.
        if update.version != offer.version {
            return Err("the update manifest named another version than the offer");
        }
        // The manifest is unsigned, so the file it names must be a plain object on the
        // channel's own host: no request goes anywhere else before the signature is checked.
        if !is_channel_file(&update.download_url, &self.endpoint) {
            return Err("the update manifest named a file on another host");
        }
        self.check_size(&update).await?;
        // The file request carries no header of its own either.
        update.headers = Default::default();
        update.timeout = Some(DOWNLOAD_TIMEOUT);
        let received = AtomicU64::new(0);
        let over = tokio::sync::Notify::new();
        // `download` verifies the bytes against the channel's minisign key, and the signed
        // version against the announced one (`requireSignedVersion`), before returning them.
        // Its chunk callback can't stop it, so once the count passes the cap the callback
        // wakes the branch below, which drops the download and with it the connection.
        let download = update.download(
            |chunk, _| {
                let total = received.fetch_add(chunk as u64, Ordering::SeqCst) + chunk as u64;
                if total > self.max_bytes {
                    over.notify_one();
                }
            },
            || {},
        );
        let bytes = tokio::select! {
            biased;
            () = over.notified() => return Err(TOO_LARGE),
            bytes = download => bytes.map_err(|e| label(&e))?,
        };
        // A body that ended in the same poll that passed the cap.
        if received.load(Ordering::SeqCst) > self.max_bytes {
            return Err(TOO_LARGE);
        }
        let version = offer.version.clone();
        let bytes =
            tokio::task::spawn_blocking(move || check_bundle(&bytes, &version).map(|()| bytes))
                .await
                .map_err(|_| "the updater failed")??;
        Ok(Some((update, bytes)))
    }

    /// Refuses an update whose announced length is missing or over the cap, before
    /// downloading it. The download host answers a HEAD with the length.
    async fn check_size(&self, update: &Update) -> Result<(), &'static str> {
        let client = reqwest_mcp::Client::builder()
            .timeout(CHECK_TIMEOUT)
            .redirect(reqwest_mcp::redirect::Policy::none())
            .https_only(!cfg!(debug_assertions))
            .build()
            .map_err(|_| "the updater failed")?;
        let head = client
            .head(update.download_url.clone())
            .send()
            .await
            .map_err(|_| "the update host could not be reached")?;
        if !head.status().is_success() {
            return Err("the update host refused the download");
        }
        let length = head
            .headers()
            .get(reqwest_mcp::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        match length {
            None => Err("the update host gave no size for the download"),
            Some(n) if n > self.max_bytes => Err(TOO_LARGE),
            Some(_) => Ok(()),
        }
    }

    /// Whether a build is staged for the quit.
    pub fn has_staged(&self) -> bool {
        lock(&self.staged).is_some()
    }

    /// At quit: installs the staged build when it is newer than the running one and the
    /// latest manifest still names it, then lets the exit continue. It never relaunches;
    /// `finish_exit` decides that. Returns
    /// the installed version, or None when nothing was installed. A staged build is taken
    /// once, so a second exit request installs nothing.
    pub fn install_on_exit(&self) -> Option<Result<String, String>> {
        let staged = lock(&self.staged).take()?;
        if !should_install_on_exit(Some(&staged.version), &self.running) {
            return None;
        }
        if !self.wanted(&staged.version) {
            log::info!(target: "license", "the staged update {} was not installed: the latest manifest no longer names it", staged.version);
            return None;
        }
        *lock(&self.last_attempt) = Some(staged.version.clone());
        Some(
            staged
                .update
                .install(&staged.bytes)
                .map(|()| staged.version.clone())
                .map_err(|e| format!("install failed: {}", label(&e))),
        )
    }

    /// SW-PB-23: whether Restart to update may run now, and for which build. `check_location`
    /// also checks that the app can be replaced where it is, which writes a temporary file next
    /// to the app: the click and the deadline's restart check it, the state shown does not.
    pub fn restart_ready(&self, check_location: bool) -> Result<String, NotReady> {
        let staged = self.staged_version();
        let wanted = staged.as_deref().is_some_and(|v| self.wanted(v));
        let failed = staged.is_some() && self.failed_version() == staged;
        // Without the location check, a location already found unwritable still counts.
        let writable = if check_location {
            self.target.as_deref().is_some_and(can_replace)
        } else {
            !self.not_writable.load(Ordering::SeqCst)
        };
        restart_readiness(staged.as_deref(), &self.running, wanted, writable, failed)?;
        Ok(staged.unwrap_or_default())
    }

    /// Arms the exit that follows to install the staged build and relaunch, once the full gate
    /// passes. The caller then asks the app to exit through its usual path (the exit coordinator
    /// and the terminal servers' detach). A location that can't be replaced is remembered, so
    /// Settings → Beta shows the install-by-hand line.
    pub fn arm_restart(&self, forced: bool) -> Result<String, NotReady> {
        // Under the arm's lock, so a quit that starts meanwhile can't be armed into a relaunch.
        let mut armed = lock(&self.restart_armed);
        if self.is_exiting() {
            return Err(NotReady::Closing);
        }
        match self.restart_ready(true) {
            Ok(version) => {
                // A forced restart writes its pending record now, its loop guard: without the
                // record it isn't armed at all. A user arm replacing a forced one drops it.
                if forced && !self.write_marker(RESTART_PENDING_FILE, &version) {
                    return Err(NotReady::NotRecorded);
                }
                if !forced && armed.is_some_and(|a| a.forced) {
                    self.remove_marker(RESTART_PENDING_FILE);
                }
                *armed = Some(Armed {
                    at: (self.clock)(),
                    forced,
                });
                Ok(version)
            }
            Err(NotReady::NotWritable) => {
                self.not_writable.store(true, Ordering::SeqCst);
                Err(NotReady::NotWritable)
            }
            Err(e) => Err(e),
        }
    }

    /// Whether an exit was armed to relaunch. An arm older than `ARMED_FOR` (the exit never
    /// came) is dropped, so the deadline watch resumes and a later plain quit doesn't relaunch.
    pub fn restart_armed(&self) -> bool {
        let mut armed = lock(&self.restart_armed);
        if !self.is_exiting() && armed.is_some_and(|a| self.stale(&a)) {
            log::warn!(target: "license", "the restart to update didn't happen; it is no longer armed");
            self.drop_arm(&mut armed);
        }
        armed.is_some()
    }

    /// Drops an armed restart (the button gave up waiting for the exit). Once the app is
    /// exiting, a slow drain must still end in the relaunch, so it is ignored then.
    pub fn disarm_restart(&self) {
        let mut armed = lock(&self.restart_armed);
        if self.is_exiting() {
            return;
        }
        self.drop_arm(&mut armed);
    }

    /// Drops an arm, and the pending record a forced arm wrote: no install follows it.
    fn drop_arm(&self, armed: &mut Option<Armed>) {
        if armed.take().is_some_and(|a| a.forced) && !self.remove_marker(RESTART_PENDING_FILE) {
            log::warn!(target: "license", "could not remove the pending restart record");
        }
    }

    fn stale(&self, armed: &Armed) -> bool {
        (self.clock)().saturating_duration_since(armed.at) >= ARMED_FOR
    }

    /// The app started exiting (the exit coordinator left Running). An arm already stale then
    /// (the exit it asked for never came) is dropped: this quit is a plain one. From here an arm
    /// is neither dropped nor cancelled, so a slow drain still ends in the relaunch.
    pub fn mark_exiting(&self) {
        let mut armed = lock(&self.restart_armed);
        // Only the first exit request judges the arm: the coordinator asks again after its
        // drain, and a second ⌘Q may land mid-drain; neither may drop it then.
        if !self.exiting.swap(true, Ordering::SeqCst) && armed.is_some_and(|a| self.stale(&a)) {
            log::info!(target: "license", "a restart asked for over a minute ago is dropped: this quit doesn't relaunch");
            self.drop_arm(&mut armed);
        }
    }

    /// Whether the app is exiting.
    pub fn is_exiting(&self) -> bool {
        self.exiting.load(Ordering::SeqCst)
    }

    /// Notes the version an offer names, so a build known to fail asks for an install by hand
    /// at once.
    pub fn note_offer(&self, version: &str) {
        *lock(&self.last_offered) = Some(version.to_string());
    }

    /// Whether the update records, the forced restart's loop guard, can be written.
    pub fn records_durable(&self) -> bool {
        self.records_durable.load(Ordering::SeqCst)
    }

    /// At the exit the coordinator allows: installs the staged build as at quit, and relaunches
    /// only when Restart to update or the deadline armed this exit. An armed install that fails
    /// is recorded, so the deadline never restarts this Mac for that build again; the app still
    /// relaunches, on the build it had.
    pub fn finish_exit(&self) -> ExitPlan {
        // Taken at once, with no expiry check: a slow drain still ends in the relaunch.
        let Some(armed) = lock(&self.restart_armed).take() else {
            return ExitPlan {
                installed: self.install_on_exit(),
                relaunch: false,
            };
        };
        // The attempt is recorded before the install, so a relaunch still on this build knows.
        // A forced restart without that record installs nothing (it has no loop guard) and
        // relaunches on this build; the user's own restart goes ahead.
        let attempt = self
            .staged_version()
            .filter(|v| should_install_on_exit(Some(v), &self.running) && self.wanted(v));
        let recorded = attempt
            .as_deref()
            .is_some_and(|v| self.write_marker(RESTART_ATTEMPTED_FILE, v));
        // The pending record is promoted (or, with nothing to install, simply goes).
        if armed.forced {
            self.remove_marker(RESTART_PENDING_FILE);
        }
        // No forced install without its record: nothing to install, or no record written.
        if armed.forced && !recorded {
            log::warn!(target: "license", "update not installed by the forced restart: nothing to install, or no record");
            return ExitPlan {
                installed: None,
                relaunch: true,
            };
        }
        // An armed exit relaunches whether or not it installed: the restart was asked for.
        let installed = self.install_on_exit();
        match (&installed, self.last_install_attempt()) {
            (Some(Err(_)), Some(version)) => {
                self.record_failure(&version);
            }
            (None, _) if recorded => {
                // Withdrawn between the record and the install: nothing to check at relaunch.
                self.remove_marker(RESTART_ATTEMPTED_FILE);
            }
            _ => {}
        }
        ExitPlan {
            installed,
            relaunch: true,
        }
    }

    /// Writes one update record. A record that can't be written turns forced restarts off.
    fn write_marker(&self, name: &str, text: &str) -> bool {
        let written = self.marker_dir.as_ref().is_some_and(|dir| {
            std::fs::create_dir_all(dir).is_ok() && std::fs::write(dir.join(name), text).is_ok()
        });
        if !written {
            log::warn!(target: "license", "could not write the update record {name}; CodeSense won't restart itself for a required update");
        }
        // Any write that lands shows the records can be saved again.
        self.records_durable.store(written, Ordering::SeqCst);
        written
    }

    /// Records `version` as one whose install failed at a restart: in memory first, then on
    /// disk. Returns whether the record was saved.
    fn record_failure(&self, version: &str) -> bool {
        *lock(&self.restart_failed) = Some(version.to_string());
        self.write_marker(RESTART_FAILED_FILE, version)
    }

    /// The build whose install failed at a restart, if any.
    fn failed_version(&self) -> Option<String> {
        lock(&self.restart_failed).clone()
    }

    /// Reads one update record, trimmed; None when missing or empty.
    fn read_marker(&self, name: &str) -> Option<String> {
        let dir = self.marker_dir.as_ref()?;
        std::fs::read_to_string(dir.join(name))
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// Removes one update record. Returns whether it is gone.
    fn remove_marker(&self, name: &str) -> bool {
        self.marker_dir.as_ref().is_some_and(|dir| {
            let path = dir.join(name);
            std::fs::remove_file(&path).is_ok() || !path.exists()
        })
    }

    fn last_install_attempt(&self) -> Option<String> {
        lock(&self.last_attempt).clone()
    }
}

/// Makes a folder read-only, and writable again when dropped, even if the test fails.
/// Tests only.
#[cfg(all(test, unix))]
pub(crate) struct ReadOnly(PathBuf);

#[cfg(all(test, unix))]
impl ReadOnly {
    pub(crate) fn new(dir: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        Self(dir.to_path_buf())
    }
}

#[cfg(all(test, unix))]
impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

#[cfg(all(test, not(cs_channel = "internal")))]
pub(super) mod tests {
    use super::*;
    use crate::test_support::updater::{bad_signatures, shipped_pubkey, ANOTHER_KEY, TEST_PUBKEY};

    use std::sync::atomic::AtomicI64;
    use std::sync::Arc;

    use std::path::Path;

    use tauri::test::{mock_builder, mock_context, noop_assets, MockRuntime};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub(crate) const RUNNING: &str = "0.0.31";
    pub(crate) const NEXT: &str = "0.0.32";
    const DAY: Duration = Duration::from_secs(24 * 60 * 60);
    pub(crate) const SIGNED_PATH: &str = "/dl/beta/0.0.32/CodeSense.app.tar.gz";

    /// A signed fixture: the tarball and its `.sig`, made by `make-fixtures.sh`.
    pub(crate) struct Fixture {
        pub(crate) bytes: &'static [u8],
        pub(crate) sig: &'static str,
    }

    macro_rules! fixture {
        ($name:literal) => {
            Fixture {
                bytes: include_bytes!(concat!("../../tests/fixtures/updater/", $name)),
                sig: include_str!(concat!("../../tests/fixtures/updater/", $name, ".sig")),
            }
        };
    }

    /// 0.0.32, signed for 0.0.32, with the beta scheme.
    pub(crate) fn good() -> Fixture {
        fixture!("update-0.0.32.app.tar.gz")
    }

    /// A test app whose updater plugin holds the test key and requires a signed version, as the
    /// beta channel configs do, running `RUNNING`.
    pub(crate) fn app() -> tauri::App<MockRuntime> {
        app_with_pubkey(TEST_PUBKEY.trim())
    }

    /// The same app, verifying with `pubkey` instead of the test key.
    pub(crate) fn app_with_pubkey(pubkey: &str) -> tauri::App<MockRuntime> {
        let mut context = mock_context(noop_assets());
        context.config_mut().plugins.0.insert(
            "updater".into(),
            serde_json::json!({
                "pubkey": pubkey,
                "endpoints": [],
                "requireSignedVersion": true
            }),
        );
        context.package_info_mut().version = RUNNING.parse().unwrap();
        mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .expect("test app")
    }

    /// The notes the offer carries (from the tick's manifest read). The plugin's own read of the
    /// manifest has its notes too; they are never shown.
    const SIGNED_NOTES: &str = "Signed notes";

    fn offer(version: &str) -> Offer {
        Offer {
            version: version.into(),
            notes: SIGNED_NOTES.into(),
            explicit: false,
        }
    }

    /// A Check now offer: the user asked.
    fn explicit(version: &str) -> Offer {
        Offer {
            explicit: true,
            ..offer(version)
        }
    }

    /// A monotonic clock the test moves by hand, in seconds.
    fn manual_clock() -> (Arc<AtomicI64>, Box<dyn Fn() -> Instant + Send + Sync>) {
        let base = Instant::now();
        let offset = Arc::new(AtomicI64::new(0));
        let tick = offset.clone();
        (
            offset,
            Box::new(move || base + Duration::from_secs(tick.load(Ordering::SeqCst) as u64)),
        )
    }

    fn reply(server: &MockServer, version: &str, sig: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version": version,
            "notes": "Fixture notes",
            "pub_date": "2026-10-17T00:00:00Z",
            "url": format!("{}{SIGNED_PATH}", server.uri()),
            "signature": sig.trim(),
        }))
    }

    /// The channel's static manifest.
    async fn mount_update(server: &MockServer, reply: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path("/v1/update/darwin/aarch64/latest.json"))
            .respond_with(reply)
            .mount(server)
            .await;
    }

    pub(crate) async fn mount_file(server: &MockServer, bytes: Vec<u8>, times: Option<u64>) {
        let len = bytes.len();
        Mock::given(method("HEAD"))
            .and(path(SIGNED_PATH))
            .respond_with(
                ResponseTemplate::new(200).insert_header("content-length", len.to_string()),
            )
            .mount(server)
            .await;
        let mock = Mock::given(method("GET"))
            .and(path(SIGNED_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes));
        let mock = match times {
            Some(n) => mock.up_to_n_times(n),
            None => mock,
        };
        mock.mount(server).await;
    }

    /// An updater whose install target is an app bundle in a writable temporary folder. The
    /// bundle is `Codesense.app`, as builds before the CodeSense name installed it, while the
    /// fixture tarballs hold `CodeSense.app`: an update keeps the installed folder and its name.
    fn updates(server: &MockServer, dir: &tempfile::TempDir) -> Updates {
        let bundle = dir.path().join("Codesense.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        Updates::new(&server.uri(), RUNNING.into(), DAY)
            .unwrap()
            .installing_into(&bundle)
            .with_marker_dir(dir.path().join("updates"))
    }

    async fn serve(server: &MockServer, version: &str, f: &Fixture) {
        mount_update(server, reply(server, version, f.sig)).await;
        mount_file(server, f.bytes.to_vec(), None).await;
    }

    #[test]
    fn the_endpoint_is_the_static_manifest_on_the_download_host() {
        let url = endpoint("https://dl.codesense.ai").unwrap();
        assert_eq!(
            url.as_str(),
            "https://dl.codesense.ai/v1/update/%7B%7Btarget%7D%7D/%7B%7Barch%7D%7D/latest.json"
        );
        assert_eq!(url.query(), None, "the running version is never sent");
        let other = endpoint("https://dl.example.invalid/").unwrap();
        assert!(other
            .as_str()
            .starts_with("https://dl.example.invalid/v1/update/"));
    }

    #[test]
    fn should_install_on_exit_only_for_a_newer_staged_build() {
        assert!(should_install_on_exit(Some("0.0.32"), "0.0.31"), "newer");
        assert!(!should_install_on_exit(Some("0.0.31"), "0.0.31"), "same");
        assert!(!should_install_on_exit(Some("0.0.30"), "0.0.31"), "older");
        assert!(!should_install_on_exit(None, "0.0.31"), "none");
        assert!(
            !should_install_on_exit(Some("0.0.x"), "0.0.31"),
            "unparsable"
        );
        assert!(should_install_on_exit(Some("0.0.100"), "0.0.99"), "numeric");
    }

    #[test]
    fn the_backoff_doubles_from_a_minute_up_to_half_the_check_interval() {
        let secs = |n| backoff(n, DAY).as_secs();
        assert_eq!(secs(0), 0);
        assert_eq!(secs(1), 60);
        assert_eq!(secs(2), 120);
        assert_eq!(secs(5), 960);
        // At most half the interval, so the next daily check is never inside it.
        assert_eq!(secs(40), 43_200);
        assert_eq!(backoff(3, Duration::from_secs(60)).as_secs(), 30);
    }

    #[test]
    fn the_install_target_is_the_app_bundle() {
        assert_eq!(
            install_target(Path::new(
                "/Applications/CodeSense.app/Contents/MacOS/codesense"
            )),
            Some(PathBuf::from("/Applications/CodeSense.app"))
        );
        assert_eq!(
            install_target(Path::new("/tmp/build/codesense")),
            Some(PathBuf::from("/tmp/build"))
        );
    }

    #[test]
    fn the_bundle_facts_come_from_the_apps_info_plist() {
        assert_eq!(
            bundle_facts(good().bytes).unwrap(),
            BundleFacts {
                version: Some(NEXT.into()),
                schemes: vec![BETA_SCHEME.into()],
            }
        );
        assert!(check_bundle(good().bytes, NEXT).is_ok());
        assert!(check_bundle(good().bytes, "0.0.33").is_err());
        assert!(check_bundle(fixture!("update-0.0.32-noscheme.app.tar.gz").bytes, NEXT).is_err());
        assert!(bundle_facts(b"not a tarball").is_err());
    }

    /// An unsigned tarball of `(path, contents)` entries, for the layout checks.
    fn tarball(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn plist_bytes(version: &str) -> Vec<u8> {
        let mut dict = plist::Dictionary::new();
        dict.insert("CFBundleShortVersionString".into(), version.into());
        let mut types = plist::Dictionary::new();
        types.insert(
            "CFBundleURLSchemes".into(),
            plist::Value::Array(vec![BETA_SCHEME.into()]),
        );
        dict.insert(
            "CFBundleURLTypes".into(),
            plist::Value::Array(vec![types.into()]),
        );
        let mut out = Vec::new();
        plist::Value::Dictionary(dict)
            .to_writer_xml(&mut out)
            .unwrap();
        out
    }

    /// The plugin installs whatever the tarball's first entry is, so the tarball must hold the
    /// one app and nothing beside it.
    #[test]
    fn the_update_must_hold_exactly_one_top_level_app() {
        let plist = plist_bytes(NEXT);
        let one = tarball(&[
            ("CodeSense.app/Contents/Info.plist", &plist),
            ("./CodeSense.app/Contents/MacOS/codesense", b"bin"),
        ]);
        assert!(check_bundle(&one, NEXT).is_ok(), "one app, ./ allowed");

        let two_apps = tarball(&[
            ("CodeSense.app/Contents/Info.plist", &plist),
            ("Other.app/Contents/Info.plist", &plist),
        ]);
        assert_eq!(
            check_bundle(&two_apps, NEXT),
            Err("the update holds more than one top-level item")
        );
        let beside = tarball(&[
            ("CodeSense.app/Contents/Info.plist", &plist),
            ("._CodeSense.app", b"appledouble"),
        ]);
        assert_eq!(
            check_bundle(&beside, NEXT),
            Err("the update holds more than one top-level item")
        );
        let not_an_app = tarball(&[("CodeSense/Contents/Info.plist", &plist)]);
        assert_eq!(
            check_bundle(&not_an_app, NEXT),
            Err("the update's top-level item is not an app")
        );
        let mut raw = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        // `append_data` refuses `..`, so write the name into the header by hand.
        header.as_old_mut().name[..18].copy_from_slice(b"../CodeSense.app/x");
        header.set_cksum();
        raw.append(&header, &b"bin"[..]).unwrap();
        let escaping = raw.into_inner().unwrap().finish().unwrap();
        assert_eq!(
            check_bundle(&escaping, NEXT),
            Err("the update holds a path outside the app")
        );

        let twice = tarball(&[
            ("CodeSense.app/Contents/Info.plist", &plist),
            ("CodeSense.app/Contents/Info.plist", &plist_bytes("0.0.99")),
        ]);
        assert_eq!(
            check_bundle(&twice, NEXT),
            Err("the update holds more than one Info.plist")
        );
    }

    /// A tarball with `(path, link target, symlink?)` links next to the app's Info.plist.
    fn with_links(links: &[(&str, &str, bool)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let plist = plist_bytes(NEXT);
        let mut header = tar::Header::new_gnu();
        header.set_size(plist.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "CodeSense.app/Contents/Info.plist", &plist[..])
            .unwrap();
        for (path, target, symlink) in links {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(if *symlink {
                tar::EntryType::Symlink
            } else {
                tar::EntryType::Link
            });
            header.set_size(0);
            header.set_mode(0o755);
            builder.append_link(&mut header, path, target).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// Links inside the bundle (a framework's `Versions/Current`) are fine; one that leaves it is
    /// refused, since unpacking would let a later entry write through it.
    #[test]
    fn a_link_pointing_outside_the_app_is_refused() {
        let inside = with_links(&[
            (
                "CodeSense.app/Contents/Frameworks/X.framework/Versions/Current",
                "A",
                true,
            ),
            (
                "CodeSense.app/Contents/Frameworks/X.framework/X",
                "Versions/Current/X",
                true,
            ),
        ]);
        assert!(check_bundle(&inside, NEXT).is_ok());
        for target in ["/Applications", "../../../..", "../../../../Other.app"] {
            let outside = with_links(&[("CodeSense.app/Contents/Resources/x", target, true)]);
            assert_eq!(
                check_bundle(&outside, NEXT),
                Err("the update holds a link that points outside the app"),
                "{target}"
            );
        }
        // Each link alone stays inside; followed through each other they leave the app.
        let chained = with_links(&[
            ("CodeSense.app/Contents/Resources/s", "../..", true),
            ("CodeSense.app/Contents/Resources/t", "s/../../etc", true),
        ]);
        assert_eq!(
            check_bundle(&chained, NEXT),
            Err("the update holds a link that points outside the app")
        );
        let looping = with_links(&[
            ("CodeSense.app/Contents/Resources/a", "b", true),
            ("CodeSense.app/Contents/Resources/b", "a", true),
        ]);
        assert!(check_bundle(&looping, NEXT).is_err(), "a loop");
        // Hard links are refused outright, even one inside the app.
        let hard = with_links(&[(
            "CodeSense.app/Contents/Resources/copy",
            "CodeSense.app/Contents/Info.plist",
            false,
        )]);
        assert_eq!(
            check_bundle(&hard, NEXT),
            Err("the update holds a hard link")
        );
    }

    /// An entry under an earlier symlink would be written wherever that link points.
    #[test]
    fn an_entry_under_a_symlink_is_refused() {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let plist = plist_bytes(NEXT);
        let mut header = tar::Header::new_gnu();
        header.set_size(plist.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "CodeSense.app/Contents/Info.plist", &plist[..])
            .unwrap();
        let mut link = tar::Header::new_gnu();
        link.set_entry_type(tar::EntryType::Symlink);
        link.set_size(0);
        builder
            .append_link(&mut link, "CodeSense.app/Contents/Lib", "Frameworks")
            .unwrap();
        let mut file = tar::Header::new_gnu();
        file.set_size(3);
        file.set_mode(0o644);
        file.set_cksum();
        // Through the link, in another case (APFS ignores case).
        builder
            .append_data(&mut file, "CodeSense.app/Contents/LIB/x", &b"bin"[..])
            .unwrap();
        let through = builder.into_inner().unwrap().finish().unwrap();
        assert_eq!(
            check_bundle(&through, NEXT),
            Err("the update holds a path through a link")
        );
    }

    /// `contents/info.plist` is the same file as `Contents/Info.plist` on APFS.
    #[test]
    fn an_info_plist_in_another_case_counts_as_a_second_one() {
        let plist = plist_bytes(NEXT);
        let other = plist_bytes("0.0.99");
        let twice = tarball(&[
            ("CodeSense.app/Contents/Info.plist", &plist),
            ("CodeSense.app/contents/info.PLIST", &other),
        ]);
        assert_eq!(
            check_bundle(&twice, NEXT),
            Err("the update holds more than one Info.plist")
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_a_bundle_this_user_can_move_on_the_temp_volume_can_be_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("CodeSense.app");
        std::fs::create_dir(&bundle).unwrap();
        let temp = std::env::temp_dir();
        assert!(can_replace_with(&bundle, &temp));
        assert!(
            !can_replace_with(&dir.path().join("Missing.app"), &temp),
            "no bundle"
        );
        // /dev is its own file system on macOS and Linux: a rename from there can't land.
        assert!(
            !can_replace_with(&bundle, Path::new("/dev")),
            "another volume"
        );
        let guard = ReadOnly::new(&bundle);
        let read_only = can_replace_with(&bundle, &temp);
        drop(guard);
        assert!(!read_only, "a bundle this user can't write");
        // A bundle owned by someone else: /usr/bin is root's on every unix this runs on.
        assert!(
            !can_replace_with(Path::new("/usr/bin"), &temp),
            "not this user's"
        );
    }

    /// AC-1: the static manifest, then the signed download, verified with the configured key.
    #[tokio::test]
    async fn a_newer_build_is_downloaded_verified_and_staged() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);

        let outcome = updates.run(app.handle(), offer(NEXT)).await;

        assert_eq!(outcome, Outcome::Staged(NEXT.into()));
        // The notes come from the offer, not the plugin's own read of the manifest.
        assert_eq!(
            updates.staged_info(),
            Some(StagedInfo {
                version: NEXT.into(),
                notes: SIGNED_NOTES.into()
            })
        );
        {
            let staged = updates.staged.lock().unwrap();
            assert_eq!(staged.as_ref().unwrap().bytes, good().bytes);
        }
        // SV-PBF-278: neither the manifest nor the file request carries a key or an install id,
        // and the manifest request has no query.
        let requests = server.received_requests().await.unwrap();
        let download = requests
            .iter()
            .find(|r| r.url.path() == SIGNED_PATH && r.method.as_str() == "GET")
            .expect("the tarball was fetched");
        let check = requests
            .iter()
            .find(|r| r.url.path().starts_with("/v1/update/"))
            .expect("the manifest was asked");
        for request in [download, check] {
            assert!(request.headers.get("authorization").is_none());
            assert!(request.headers.get("x-install-id").is_none());
        }
        assert_eq!(check.url.query(), None);
    }

    /// SV-REL-32: the unsigned manifest's attacks, with the offer naming the manifest's version as
    /// the tick does. A version not newer than the running one asks nothing; a genuine bundle
    /// whose signed trusted-comment version is not the manifest's is refused and nothing is
    /// staged; a genuine newer bundle signed for its own version is staged.
    #[tokio::test]
    async fn the_public_channel_refuses_downgrade_same_version_and_relabel() {
        let older = || fixture!("update-0.0.30.app.tar.gz");
        let unsigned_version = || fixture!("legacy-unversioned.app.tar.gz");
        const NOT_SIGNED_FOR_IT: &str = "the download is not signed for the offered version";
        // (case, the manifest's version, the bundle it names, the outcome, a request made)
        let rows: [(&str, &str, Fixture, Outcome, bool); 6] = [
            ("downgrade", "0.0.30", older(), Outcome::NoUpdate, false),
            ("same version", RUNNING, good(), Outcome::NoUpdate, false),
            (
                "relabel: an older bundle as newer",
                NEXT,
                older(),
                Outcome::Failed(NOT_SIGNED_FOR_IT),
                true,
            ),
            (
                "relabel: a newer bundle as another newer version",
                "0.0.33",
                good(),
                Outcome::Failed(NOT_SIGNED_FOR_IT),
                true,
            ),
            (
                "relabel: a bundle signed for no version",
                NEXT,
                unsigned_version(),
                Outcome::Failed(NOT_SIGNED_FOR_IT),
                true,
            ),
            (
                "a valid newer bundle",
                NEXT,
                good(),
                Outcome::Staged(NEXT.into()),
                true,
            ),
        ];
        for (case, version, f, expected, asked) in rows {
            let server = MockServer::start().await;
            serve(&server, version, &f).await;
            let (app, dir) = (app(), tempfile::tempdir().unwrap());
            let updates = updates(&server, &dir);

            let outcome = updates.run(app.handle(), offer(version)).await;

            assert_eq!(outcome, expected, "{case}");
            let requests = server.received_requests().await.unwrap();
            assert_eq!(!requests.is_empty(), asked, "{case}: a request");
            if let Outcome::Staged(staged) = &expected {
                assert_eq!(updates.staged_version().as_ref(), Some(staged), "{case}");
                continue;
            }
            assert_eq!(updates.staged_version(), None, "{case}: nothing staged");
            assert!(
                updates.install_on_exit().is_none(),
                "{case}: nothing installs"
            );
        }
    }

    /// The downgrade the review found: an older build's genuine signed tarball, announced as
    /// the newer version, is refused by its signed version.
    #[tokio::test]
    async fn an_older_signed_build_served_as_newer_is_refused() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &fixture!("update-0.0.30.app.tar.gz")).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Failed("the download is not signed for the offered version")
        );
        assert_eq!(updates.staged_version(), None);
    }

    #[tokio::test]
    async fn a_signature_without_a_version_is_refused() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &fixture!("legacy-unversioned.app.tar.gz")).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Failed("the download is not signed for the offered version")
        );
    }

    /// The manifest's version must be the one the offer named.
    #[tokio::test]
    async fn a_route_naming_another_version_than_the_check_is_refused() {
        let server = MockServer::start().await;
        serve(&server, "0.0.33", &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Failed("the update manifest named another version than the offer")
        );
        let requests = server.received_requests().await.unwrap();
        assert!(
            requests.iter().all(|r| r.url.path() != SIGNED_PATH),
            "nothing downloaded"
        );
    }

    /// The unsigned manifest can't send the app to another host: nothing is asked of it.
    #[tokio::test]
    async fn a_file_on_another_host_is_refused_before_any_request_to_it() {
        let (server, elsewhere) = (MockServer::start().await, MockServer::start().await);
        let f = good();
        mount_update(&server, reply(&elsewhere, NEXT, f.sig)).await;
        mount_file(&elsewhere, f.bytes.to_vec(), None).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Failed("the update manifest named a file on another host")
        );
        assert!(elsewhere.received_requests().await.unwrap().is_empty());
        assert_eq!(updates.staged_version(), None);
    }

    #[test]
    fn a_channel_file_is_a_plain_object_on_the_manifest_host() {
        let manifest = endpoint("https://dl.codesense.ai").unwrap();
        let file = |u: &str| is_channel_file(&u.parse().unwrap(), &manifest);
        assert!(file(
            "https://dl.codesense.ai/v1/0.0.32/CodeSense.app.tar.gz"
        ));
        for other in [
            "https://evil.example/v1/0.0.32/CodeSense.app.tar.gz",
            "http://dl.codesense.ai/v1/0.0.32/CodeSense.app.tar.gz",
            "https://dl.codesense.ai:8443/v1/0.0.32/CodeSense.app.tar.gz",
            "https://dl.codesense.ai.evil.example/v1/0.0.32/CodeSense.app.tar.gz",
            "https://user:pw@dl.codesense.ai/v1/0.0.32/CodeSense.app.tar.gz",
            "https://dl.codesense.ai/v1/0.0.32/CodeSense.app.tar.gz?x=1",
            "https://dl.codesense.ai/v1/0.0.32/CodeSense.app.tar.gz#x",
        ] {
            assert!(!file(other), "{other}");
        }
    }

    /// A reply naming the running version, or an older one, stages nothing.
    #[tokio::test]
    async fn a_route_naming_the_running_or_an_older_version_stages_nothing() {
        for (version, f) in [
            (RUNNING, good()),
            ("0.0.30", fixture!("update-0.0.30.app.tar.gz")),
        ] {
            let server = MockServer::start().await;
            serve(&server, version, &f).await;
            let (app, dir) = (app(), tempfile::tempdir().unwrap());
            let updates = updates(&server, &dir);
            let outcome = updates.run(app.handle(), offer(NEXT)).await;
            assert!(
                matches!(outcome, Outcome::NoUpdate | Outcome::Failed(_)),
                "{version}: {outcome:?}"
            );
            assert_eq!(updates.staged_version(), None, "{version}");
        }
    }

    #[tokio::test]
    async fn an_info_plist_naming_another_version_is_refused() {
        let server = MockServer::start().await;
        serve(
            &server,
            NEXT,
            &fixture!("update-0.0.32-plist-0.0.31.app.tar.gz"),
        )
        .await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Failed("the app in the update is not the offered version")
        );
        assert_eq!(updates.staged_version(), None);
    }

    #[tokio::test]
    async fn an_app_without_the_beta_scheme_is_refused() {
        let server = MockServer::start().await;
        serve(
            &server,
            NEXT,
            &fixture!("update-0.0.32-noscheme.app.tar.gz"),
        )
        .await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Failed("the app in the update is not a beta build")
        );
    }

    #[tokio::test]
    async fn an_announced_length_over_the_cap_is_refused_before_downloading() {
        let server = MockServer::start().await;
        mount_update(&server, reply(&server, NEXT, good().sig)).await;
        Mock::given(method("HEAD"))
            .and(path(SIGNED_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-length", (MAX_UPDATE_BYTES + 1).to_string()),
            )
            .mount(&server)
            .await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Failed("the update is larger than allowed")
        );
        let requests = server.received_requests().await.unwrap();
        assert!(requests
            .iter()
            .all(|r| !(r.url.path() == SIGNED_PATH && r.method.as_str() == "GET")));
    }

    #[tokio::test]
    async fn a_download_with_no_announced_length_is_refused_before_downloading() {
        let server = MockServer::start().await;
        mount_update(&server, reply(&server, NEXT, good().sig)).await;
        Mock::given(method("HEAD"))
            .and(path(SIGNED_PATH))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Failed("the update host gave no size for the download")
        );
        let requests = server.received_requests().await.unwrap();
        assert!(requests
            .iter()
            .all(|r| !(r.url.path() == SIGNED_PATH && r.method.as_str() == "GET")));
    }

    /// A server that announces a small length and then sends more is cut off by the count.
    #[tokio::test]
    async fn a_download_longer_than_announced_is_stopped_at_the_cap() {
        let server = MockServer::start().await;
        mount_update(&server, reply(&server, NEXT, good().sig)).await;
        Mock::given(method("HEAD"))
            .and(path(SIGNED_PATH))
            .respond_with(ResponseTemplate::new(200).insert_header("content-length", "10"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(SIGNED_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0u8; 512 * 1024]))
            .mount(&server)
            .await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir).with_max_bytes(1024);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Failed("the update is larger than allowed")
        );
        assert_eq!(updates.staged_version(), None);
    }

    #[tokio::test]
    async fn a_204_stages_nothing_and_downloads_nothing() {
        let server = MockServer::start().await;
        mount_update(&server, ResponseTemplate::new(204)).await;
        mount_file(&server, good().bytes.to_vec(), None).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::NoUpdate
        );
        assert_eq!(updates.staged_version(), None);
        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|r| r.url.path() != SIGNED_PATH));
    }

    fn tampered() -> Vec<u8> {
        let mut bytes = good().bytes.to_vec();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        bytes
    }

    /// AC-3: a tampered download is dropped, logged without secrets, backed off, and retried.
    #[tokio::test]
    async fn a_tampered_download_stages_nothing_backs_off_and_is_retried() {
        let server = MockServer::start().await;
        mount_update(&server, reply(&server, NEXT, good().sig)).await;
        mount_file(&server, tampered(), Some(1)).await;
        Mock::given(method("GET"))
            .and(path(SIGNED_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(good().bytes.to_vec()))
            .mount(&server)
            .await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let (clock, now) = manual_clock();
        let updates = updates(&server, &dir).with_clock(now);

        let first = updates.run(app.handle(), offer(NEXT)).await;
        assert_eq!(first, Outcome::Failed("the download did not verify"));
        assert_eq!(updates.staged_version(), None, "nothing staged");
        let Outcome::Failed(label) = first else {
            unreachable!()
        };
        let line = failure_line(label);
        for secret in ["sig=", "dl_fixture", good().sig.trim()] {
            assert!(
                !line.contains(secret),
                "the log line leaks {secret}: {line}"
            );
        }

        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::BackingOff,
            "a check inside the backoff downloads nothing"
        );
        clock.fetch_add(60, Ordering::SeqCst);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Staged(NEXT.into()),
            "the retry stages it"
        );
    }

    /// A bad signature on genuine bytes is refused like a tampered download: nothing staged,
    /// backed off, and retried. The retry with the fixture's own signature stages it, so the
    /// key that refused is the test key; an app holding the public channel's shipped key
    /// refuses the test-signed build every time.
    #[tokio::test]
    async fn a_bad_signature_stages_nothing_backs_off_and_is_retried() {
        let public_key = shipped_pubkey(include_str!("../../tauri.beta-prod.conf.json"));
        assert_ne!(
            public_key,
            TEST_PUBKEY.trim(),
            "the shipped key is not the test key"
        );
        for bad in bad_signatures(&public_key) {
            let case = bad.case;
            assert_eq!(
                bad.pubkey == TEST_PUBKEY.trim(),
                case != ANOTHER_KEY,
                "{case}: the key"
            );
            let server = MockServer::start().await;
            let manifest = match &bad.sig {
                Some(sig) => reply(&server, NEXT, sig),
                None => ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "version": NEXT,
                    "notes": "Fixture notes",
                    "pub_date": "2026-10-17T00:00:00Z",
                    "url": format!("{}{SIGNED_PATH}", server.uri()),
                })),
            };
            mount_update(&server, manifest).await;
            mount_file(&server, good().bytes.to_vec(), None).await;
            let (app, dir) = (app_with_pubkey(&bad.pubkey), tempfile::tempdir().unwrap());
            let (clock, now) = manual_clock();
            let updates = updates(&server, &dir).with_clock(now);

            // The plugin refuses a manifest with no signature before downloading anything.
            let reason = match bad.sig {
                None => "the update reply was not usable",
                Some(_) => "the download did not verify",
            };
            let first = updates.run(app.handle(), offer(NEXT)).await;
            assert_eq!(first, Outcome::Failed(reason), "{case}");
            assert_eq!(updates.staged_version(), None, "{case}: nothing staged");
            if bad.sig.is_none() {
                let requests = server.received_requests().await.unwrap();
                assert!(
                    requests.iter().all(|r| r.url.path() != SIGNED_PATH),
                    "{case}: nothing downloaded"
                );
            }
            assert_eq!(
                updates.run(app.handle(), offer(NEXT)).await,
                Outcome::BackingOff,
                "{case}: a check inside the backoff downloads nothing"
            );
            clock.fetch_add(60, Ordering::SeqCst);
            server.reset().await;
            serve(&server, NEXT, &good()).await;
            let retry = updates.run(app.handle(), offer(NEXT)).await;
            if case == ANOTHER_KEY {
                assert_eq!(
                    retry,
                    Outcome::Failed("the download did not verify"),
                    "{case}: the retry is refused again"
                );
                assert_eq!(updates.staged_version(), None, "{case}: nothing staged");
                let requests = server.received_requests().await.unwrap();
                assert!(
                    requests.iter().any(|r| r.url.path() == SIGNED_PATH),
                    "{case}: the retry downloaded it"
                );
            } else {
                assert_eq!(
                    retry,
                    Outcome::Staged(NEXT.into()),
                    "{case}: the retry stages it"
                );
            }
        }
    }

    #[tokio::test]
    async fn repeated_failures_ask_for_a_manual_install_and_a_success_clears_that() {
        let server = MockServer::start().await;
        mount_update(&server, ResponseTemplate::new(500)).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let (clock, now) = manual_clock();
        let updates = updates(&server, &dir).with_clock(now);
        for n in 1..=FAILURES_BEFORE_NOTICE {
            assert_eq!(updates.attention(), None, "before failure {n}");
            assert!(matches!(
                updates.run(app.handle(), offer(NEXT)).await,
                Outcome::Failed(_)
            ));
            clock.fetch_add(86_400, Ordering::SeqCst);
        }
        assert_eq!(updates.attention(), Some(Attention::Failing));

        server.reset().await;
        serve(&server, NEXT, &good()).await;
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Staged(NEXT.into())
        );
        assert_eq!(updates.attention(), None);
    }

    /// The Mac slept through the wait: the monotonic clock barely moved, but the wall clock did,
    /// so the next daily check downloads again.
    #[tokio::test]
    async fn a_mac_that_slept_through_the_backoff_retries_at_the_next_check() {
        let server = MockServer::start().await;
        mount_update(&server, ResponseTemplate::new(500)).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let (_mono, now) = manual_clock();
        let start = std::time::SystemTime::now();
        let slept = Arc::new(AtomicI64::new(0));
        let wall = slept.clone();
        let updates = updates(&server, &dir)
            .with_clock(now)
            .with_wall(Box::new(move || {
                start + Duration::from_secs(wall.load(Ordering::SeqCst) as u64)
            }));
        assert!(matches!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Failed(_)
        ));
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::BackingOff
        );
        // Two days asleep: the monotonic clock is where it was; the wall clock moved on.
        slept.store(2 * 86_400, Ordering::SeqCst);
        assert!(
            matches!(
                updates.run(app.handle(), offer(NEXT)).await,
                Outcome::Failed(_)
            ),
            "the daily check after waking tries again"
        );
    }

    /// Check now is the user's own request: it runs straight away, inside the backoff too.
    #[tokio::test]
    async fn check_now_ignores_the_backoff() {
        let server = MockServer::start().await;
        mount_update(&server, ResponseTemplate::new(500)).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let (_clock, now) = manual_clock();
        let updates = updates(&server, &dir).with_clock(now);
        assert!(matches!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Failed(_)
        ));
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::BackingOff
        );
        server.reset().await;
        serve(&server, NEXT, &good()).await;
        assert_eq!(
            updates.run(app.handle(), explicit(NEXT)).await,
            Outcome::Staged(NEXT.into()),
            "Check now inside the backoff"
        );
    }

    /// A check that finds nothing to do ends a run of failures, so an old streak never shows
    /// "install by hand" after the cause has gone.
    #[tokio::test]
    async fn no_update_and_already_staged_reset_the_failure_count() {
        let server = MockServer::start().await;
        mount_update(&server, ResponseTemplate::new(500)).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        for _ in 0..FAILURES_BEFORE_NOTICE {
            assert!(matches!(
                updates.run(app.handle(), explicit(NEXT)).await,
                Outcome::Failed(_)
            ));
        }
        assert_eq!(updates.attention(), Some(Attention::Failing));
        assert_eq!(
            updates.run(app.handle(), offer(RUNNING)).await,
            Outcome::NoUpdate
        );
        assert_eq!(updates.attention(), None, "NoUpdate resets it");

        for _ in 0..FAILURES_BEFORE_NOTICE {
            updates.run(app.handle(), explicit(NEXT)).await;
        }
        server.reset().await;
        serve(&server, NEXT, &good()).await;
        assert_eq!(
            updates.run(app.handle(), explicit(NEXT)).await,
            Outcome::Staged(NEXT.into())
        );
        // A streak recorded while the build stays staged (as after a failed later attempt).
        *updates.failures.lock().unwrap() = Failures {
            count: FAILURES_BEFORE_NOTICE,
            retry_at: None,
        };
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::AlreadyStaged
        );
        assert_eq!(updates.attention(), None, "AlreadyStaged resets it");
    }

    /// A lock, or a check naming another build, that lands while the download runs: the
    /// verified build is not staged, and one staged before it never installs.
    #[tokio::test]
    async fn a_build_the_license_service_no_longer_wants_is_not_staged_or_installed() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        let wanted = Arc::new(AtomicBool::new(false));
        let gate = wanted.clone();
        updates.set_gate(Box::new(move |v| v == NEXT && gate.load(Ordering::SeqCst)));
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Withdrawn
        );
        assert!(!updates.has_staged());

        wanted.store(true, Ordering::SeqCst);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Staged(NEXT.into())
        );
        wanted.store(false, Ordering::SeqCst);
        assert!(updates.install_on_exit().is_none(), "not installed at quit");
        let installed = dir.path().join("Codesense.app/Contents/Info.plist");
        assert!(!installed.exists(), "the running app is untouched");
    }

    /// A counted check naming another version drops the staged build; one naming the staged
    /// version keeps it.
    #[tokio::test]
    async fn clear_unless_keeps_only_the_named_version() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(!updates.clear_unless(Some(NEXT)), "the same version stays");
        assert!(updates.has_staged());
        assert!(updates.clear_unless(Some(RUNNING)), "a rollback drops it");
        assert!(!updates.has_staged());
        assert!(updates.install_on_exit().is_none());
    }

    /// An offer that arrives while a download runs is not lost: it is tried when that one ends.
    #[tokio::test]
    async fn an_offer_that_arrives_while_busy_is_tried_when_the_download_ends() {
        let server = MockServer::start().await;
        mount_update(&server, reply(&server, NEXT, good().sig)).await;
        Mock::given(method("HEAD"))
            .and(path(SIGNED_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-length", good().bytes.len().to_string()),
            )
            .mount(&server)
            .await;
        // The first download is tampered and slow; the one after it is good.
        Mock::given(method("GET"))
            .and(path(SIGNED_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(tampered())
                    .set_delay(Duration::from_millis(800)),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(SIGNED_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(good().bytes.to_vec()))
            .mount(&server)
            .await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        let first = updates.run(app.handle(), offer(NEXT));
        let second = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            updates.run(app.handle(), explicit(NEXT)).await
        };
        let (first, second) = tokio::join!(first, second);
        assert_eq!(second, Outcome::Busy, "kept for later");
        assert_eq!(
            first,
            Outcome::Staged(NEXT.into()),
            "the kept offer ran after the failed download"
        );
        assert!(!updates.slot.lock().unwrap().busy);
    }

    /// A Check now that waits behind a download keeps skipping the backoff, even when a later
    /// scheduled offer replaces it; a clear drops a waiting offer; and a withdrawn offer
    /// downloads nothing.
    #[tokio::test]
    async fn waiting_offers_merge_clear_and_respect_the_gate() {
        let server = MockServer::start().await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        updates.slot.lock().unwrap().busy = true;
        assert_eq!(
            updates.run(app.handle(), explicit(NEXT)).await,
            Outcome::Busy
        );
        assert_eq!(updates.run(app.handle(), offer(NEXT)).await, Outcome::Busy);
        assert!(
            updates
                .slot
                .lock()
                .unwrap()
                .pending
                .as_ref()
                .unwrap()
                .explicit
        );
        updates.clear_unless(Some(NEXT));
        assert!(
            updates.slot.lock().unwrap().pending.is_some(),
            "the same version stays"
        );
        updates.clear_unless(None);
        assert!(
            updates.slot.lock().unwrap().pending.is_none(),
            "a lock drops it"
        );
        updates.slot.lock().unwrap().busy = false;

        updates.set_gate(Box::new(|_| false));
        assert_eq!(
            updates.run(app.handle(), explicit(NEXT)).await,
            Outcome::Withdrawn
        );
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "nothing was asked or downloaded"
        );
    }

    /// The busy flag is cleared even when the attempt's future is dropped mid-download.
    #[tokio::test]
    async fn a_cancelled_attempt_leaves_the_updater_free() {
        let server = MockServer::start().await;
        mount_update(&server, reply(&server, NEXT, good().sig)).await;
        Mock::given(method("HEAD"))
            .and(path(SIGNED_PATH))
            .respond_with(ResponseTemplate::new(200).insert_header("content-length", "10"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(SIGNED_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(good().bytes.to_vec())
                    .set_delay(Duration::from_secs(30)),
            )
            .mount(&server)
            .await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        let attempt = updates.run(app.handle(), offer(NEXT));
        assert!(
            tokio::time::timeout(Duration::from_millis(300), attempt)
                .await
                .is_err(),
            "still downloading"
        );
        let slot = updates.slot.lock().unwrap();
        assert!(
            !slot.busy && slot.pending.is_none(),
            "the slot was released"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_folder_the_user_cant_write_downloads_nothing_and_asks_for_a_manual_install() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let app = app();
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("Applications");
        std::fs::create_dir_all(locked.join("Codesense.app")).unwrap();
        let guard = ReadOnly::new(&locked);
        let updates = Updates::new(&server.uri(), RUNNING.into(), DAY)
            .unwrap()
            .installing_into(&locked.join("Codesense.app"));
        let outcome = updates.run(app.handle(), offer(NEXT)).await;
        drop(guard);
        assert_eq!(outcome, Outcome::NotWritable);
        assert_eq!(updates.attention(), Some(Attention::NotWritable));
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_offer_that_is_not_newer_asks_nothing() {
        let server = MockServer::start().await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        for version in [RUNNING, "0.0.30", "not-a-version"] {
            assert_eq!(
                updates.run(app.handle(), offer(version)).await,
                Outcome::NoUpdate
            );
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_staged_build_is_not_downloaded_again_and_clear_drops_it() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Staged(NEXT.into())
        );
        let asked = server.received_requests().await.unwrap().len();
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::AlreadyStaged
        );
        assert_eq!(server.received_requests().await.unwrap().len(), asked);
        updates.clear_unless(None);
        assert!(!updates.has_staged());
        assert!(updates.install_on_exit().is_none(), "nothing installs");
    }

    /// AC-4, behavioural: the staged build is installed in place of the app at quit, once.
    #[tokio::test]
    async fn the_staged_build_installs_in_place_of_the_app_once() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        // Installed under a name that differs from the update's `CodeSense.app` by more than
        // case, so an installer that wrote to the tarball's name could not pass on APFS.
        let bundle = dir.path().join("Old.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), "the running build").unwrap();
        let updates = Updates::new(&server.uri(), RUNNING.into(), DAY)
            .unwrap()
            .installing_into(&bundle)
            .with_marker_dir(dir.path().join("updates"));
        assert_eq!(
            updates.run(app.handle(), offer(NEXT)).await,
            Outcome::Staged(NEXT.into())
        );
        assert!(updates.has_staged());

        assert_eq!(updates.install_on_exit(), Some(Ok(NEXT.to_string())));

        let installed = std::fs::read(bundle.join("Contents/Info.plist")).unwrap();
        assert_eq!(
            plist::Value::from_reader(std::io::Cursor::new(installed))
                .unwrap()
                .as_dictionary()
                .and_then(|d| d.get("CFBundleShortVersionString"))
                .and_then(|v| v.as_string()),
            Some(NEXT)
        );
        // The installed app keeps its folder and its name, and no second app appears beside it.
        let apps: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.to_lowercase().ends_with(".app"))
            .collect();
        assert_eq!(apps, ["Old.app"]);
        assert!(
            updates.install_on_exit().is_none(),
            "a second quit installs nothing"
        );
    }

    /// SW-PB-23 AC-1: the decision behind Restart to update.
    #[test]
    fn restart_readiness_is_the_install_on_quit_gate_plus_the_location() {
        let ok = restart_readiness(Some(NEXT), RUNNING, true, true, false);
        assert_eq!(ok, Ok(()));
        assert_eq!(
            restart_readiness(None, RUNNING, true, true, false),
            Err(NotReady::NothingStaged)
        );
        for v in [RUNNING, "0.0.30", "0.0.x"] {
            assert_eq!(
                restart_readiness(Some(v), RUNNING, true, true, false),
                Err(NotReady::NotNewer),
                "{v}"
            );
        }
        assert_eq!(
            restart_readiness(Some(NEXT), RUNNING, false, true, false),
            Err(NotReady::Withdrawn),
            "no access, or the latest check names another build"
        );
        assert_eq!(
            restart_readiness(Some(NEXT), RUNNING, true, false, false),
            Err(NotReady::NotWritable)
        );
        assert_eq!(
            restart_readiness(Some(NEXT), RUNNING, true, true, true),
            Err(NotReady::InstallFailed)
        );
    }

    /// A staged build arms the restart; the exit then installs it and relaunches, once.
    #[tokio::test]
    async fn restart_to_update_installs_the_staged_build_then_relaunches() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let bundle = dir.path().join("Codesense.app");
        let updates = updates(&server, &dir);
        assert_eq!(updates.arm_restart(false), Err(NotReady::NothingStaged));
        assert!(!updates.restart_armed(), "nothing staged arms nothing");
        updates.run(app.handle(), offer(NEXT)).await;

        assert_eq!(updates.restart_ready(true), Ok(NEXT.into()));
        assert_eq!(updates.arm_restart(false), Ok(NEXT.into()));
        assert!(updates.restart_armed());

        let plan = updates.finish_exit();
        assert_eq!(plan.installed, Some(Ok(NEXT.to_string())));
        assert!(plan.relaunch);
        let installed = std::fs::read(bundle.join("Contents/Info.plist")).unwrap();
        assert!(String::from_utf8_lossy(&installed).contains(NEXT));
        let again = updates.finish_exit();
        assert_eq!(
            again,
            ExitPlan {
                installed: None,
                relaunch: false
            }
        );
    }

    /// A plain quit installs as before and never relaunches.
    #[tokio::test]
    async fn a_plain_quit_installs_and_never_relaunches() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        updates.run(app.handle(), offer(NEXT)).await;
        let plan = updates.finish_exit();
        assert_eq!(plan.installed, Some(Ok(NEXT.to_string())));
        assert!(!plan.relaunch);
    }

    /// The gate is asked at the click: a withdrawn build arms nothing and installs nothing.
    #[tokio::test]
    async fn a_withdrawn_build_is_not_restarted_onto() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        let wanted = Arc::new(AtomicBool::new(true));
        let gate = wanted.clone();
        updates.set_gate(Box::new(move |_| gate.load(Ordering::SeqCst)));
        updates.run(app.handle(), offer(NEXT)).await;
        wanted.store(false, Ordering::SeqCst);
        assert_eq!(updates.restart_ready(false), Err(NotReady::Withdrawn));
        assert_eq!(updates.arm_restart(false), Err(NotReady::Withdrawn));
        assert!(!updates.restart_armed());
        assert_eq!(
            updates.finish_exit(),
            ExitPlan {
                installed: None,
                relaunch: false
            }
        );
    }

    /// A location this user can't replace at the click: nothing is armed, and Settings → Beta
    /// asks for an install by hand.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unwritable_location_at_the_click_arms_nothing_and_asks_for_a_manual_install() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        updates.run(app.handle(), offer(NEXT)).await;
        assert_eq!(
            updates.restart_ready(false),
            Ok(NEXT.into()),
            "the shown state"
        );
        let read_only = ReadOnly::new(dir.path());
        let armed = updates.arm_restart(false);
        drop(read_only);
        assert_eq!(armed, Err(NotReady::NotWritable));
        assert!(!updates.restart_armed());
        assert_eq!(updates.attention(), Some(Attention::NotWritable));
        assert_eq!(
            updates.restart_ready(false),
            Err(NotReady::NotWritable),
            "the shown state drops the button, and the deadline stops trying"
        );
    }

    /// An armed install that fails still relaunches the old build, and is recorded so the
    /// deadline never restarts this Mac for that build again, across a relaunch too.
    #[tokio::test]
    async fn a_failed_install_at_a_restart_is_recorded_and_not_retried_that_way() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let markers = tempfile::tempdir().unwrap();
        let updates = updates(&server, &dir).with_marker_dir(markers.path().to_path_buf());
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(false).is_ok());
        // The install fails at the exit. (Bytes that don't unpack fail before anything moves; a
        // folder made read-only would make the plugin ask for an administrator instead.)
        updates.staged.lock().unwrap().as_mut().unwrap().bytes = b"not a tarball".to_vec();
        let plan = updates.finish_exit();
        assert!(matches!(plan.installed, Some(Err(_))), "{plan:?}");
        assert!(plan.relaunch, "the app comes back, on the build it had");

        let relaunched = Updates::new(&server.uri(), RUNNING.into(), DAY)
            .unwrap()
            .installing_into(&dir.path().join("Codesense.app"))
            .with_marker_dir(markers.path().to_path_buf());
        relaunched.run(app.handle(), offer(NEXT)).await;
        assert_eq!(relaunched.restart_ready(true), Err(NotReady::InstallFailed));
        assert_eq!(relaunched.attention(), Some(Attention::InstallFailed));

        // Once that build runs, the record is stale and goes.
        let updated = Updates::new(&server.uri(), NEXT.into(), DAY)
            .unwrap()
            .with_marker_dir(markers.path().to_path_buf());
        assert_eq!(updated.attention(), None);
        assert!(!markers.path().join(RESTART_FAILED_FILE).exists());
    }

    /// A relaunch that comes back on the old build is a failed install, even when the exit
    /// recorded no failure (an install that reported success, or a record that wasn't written).
    #[tokio::test]
    async fn a_relaunch_still_on_the_old_build_counts_as_a_failed_install() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let markers = tempfile::tempdir().unwrap();
        let updates = updates(&server, &dir).with_marker_dir(markers.path().to_path_buf());
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(false).is_ok());
        let plan = updates.finish_exit();
        assert_eq!(plan.installed, Some(Ok(NEXT.to_string())));
        assert_eq!(
            std::fs::read_to_string(markers.path().join(RESTART_ATTEMPTED_FILE)).unwrap(),
            NEXT,
            "written before the relaunch"
        );
        assert!(!markers.path().join(RESTART_FAILED_FILE).exists());

        // The relaunch finds itself still on the old build.
        let old = Updates::new(&server.uri(), RUNNING.into(), DAY)
            .unwrap()
            .installing_into(&dir.path().join("Codesense.app"))
            .with_marker_dir(markers.path().to_path_buf());
        assert!(
            !markers.path().join(RESTART_ATTEMPTED_FILE).exists(),
            "read once"
        );
        old.run(app.handle(), offer(NEXT)).await;
        assert_eq!(old.restart_ready(false), Err(NotReady::InstallFailed));
        assert_eq!(old.attention(), Some(Attention::InstallFailed));
    }

    #[test]
    fn a_relaunch_onto_the_new_build_clears_the_attempt() {
        let markers = tempfile::tempdir().unwrap();
        std::fs::write(markers.path().join(RESTART_ATTEMPTED_FILE), NEXT).unwrap();
        let updated = Updates::new("https://updates.codesense.ai", NEXT.into(), DAY)
            .unwrap()
            .with_marker_dir(markers.path().to_path_buf());
        assert!(!markers.path().join(RESTART_ATTEMPTED_FILE).exists());
        assert!(!markers.path().join(RESTART_FAILED_FILE).exists());
        assert_eq!(updated.attention(), None);
    }

    /// An armed exit that installs nothing (the build was withdrawn after the click) still
    /// relaunches, on this build: the restart was asked for.
    #[tokio::test]
    async fn an_armed_exit_with_nothing_installed_relaunches_on_this_build() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        let wanted = Arc::new(AtomicBool::new(true));
        let gate = wanted.clone();
        updates.set_gate(Box::new(move |_| gate.load(Ordering::SeqCst)));
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(false).is_ok());
        wanted.store(false, Ordering::SeqCst);
        assert_eq!(
            updates.finish_exit(),
            ExitPlan {
                installed: None,
                relaunch: true
            }
        );
    }

    /// An arm whose exit never came is dropped after a minute: the deadline watch resumes, and
    /// a later plain quit doesn't relaunch.
    #[tokio::test]
    async fn an_armed_restart_whose_exit_never_came_is_dropped_after_a_minute() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let (clock, now) = manual_clock();
        let updates = updates(&server, &dir).with_clock(now);
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(false).is_ok());
        clock.fetch_add(59, Ordering::SeqCst);
        assert!(updates.restart_armed());
        clock.fetch_add(1, Ordering::SeqCst);
        assert!(!updates.restart_armed(), "dropped");
        assert!(!updates.finish_exit().relaunch, "a plain quit");
    }

    #[tokio::test]
    async fn the_buttons_reset_disarms_at_once() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(false).is_ok());
        updates.disarm_restart();
        assert!(!updates.restart_armed());
    }

    /// With nothing staged after a relaunch onto the old build, the known-failed build asks for
    /// an install by hand rather than looking like a download in progress.
    #[test]
    fn a_known_failed_build_asks_for_a_manual_install_before_it_is_staged_again() {
        let markers = tempfile::tempdir().unwrap();
        std::fs::write(markers.path().join(RESTART_FAILED_FILE), NEXT).unwrap();
        let old = Updates::new("https://updates.codesense.ai", RUNNING.into(), DAY)
            .unwrap()
            .with_marker_dir(markers.path().to_path_buf());
        assert_eq!(old.attention(), None, "nothing on offer yet");
        old.note_offer("0.0.33");
        assert_eq!(old.attention(), None, "another build on offer");
        old.note_offer(NEXT);
        assert_eq!(old.attention(), Some(Attention::InstallFailed));
    }

    /// A slow exit (the coordinator draining for minutes) still ends in the relaunch: once the
    /// app is exiting, neither the arm's expiry nor the button's cancel drops it.
    #[tokio::test]
    async fn a_slow_exit_still_relaunches() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let (clock, now) = manual_clock();
        let updates = updates(&server, &dir).with_clock(now);
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(false).is_ok());
        updates.mark_exiting();
        clock.fetch_add(120, Ordering::SeqCst);
        updates.disarm_restart();
        assert!(updates.restart_armed());
        let plan = updates.finish_exit();
        assert!(plan.relaunch);
        assert_eq!(plan.installed, Some(Ok(NEXT.to_string())));
    }

    /// The attempt is recorded before the install; if it can't be, nothing is installed and
    /// the app relaunches on this build.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_attempt_that_cant_be_recorded_installs_nothing_and_relaunches_on_this_build() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(true).is_ok(), "a forced restart");
        // The folder no longer takes the attempt record the install needs.
        let records = dir.path().join("updates");
        let read_only = ReadOnly::new(&records);
        let plan = updates.finish_exit();
        drop(read_only);
        assert_eq!(
            plan,
            ExitPlan {
                installed: None,
                relaunch: true
            }
        );
        assert!(
            !dir.path()
                .join("Codesense.app/Contents/Info.plist")
                .exists(),
            "the running app is untouched"
        );
        assert_eq!(updates.attention(), Some(Attention::RecordsUnwritable));
    }

    /// The user's own restart isn't blocked by records that can't be saved: it installs, and
    /// the next write that lands shows the records work again.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_users_restart_goes_ahead_without_the_records_and_a_write_restores_them() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        updates.run(app.handle(), offer(NEXT)).await;
        updates.disable_records_for_test();
        assert_eq!(updates.attention(), Some(Attention::RecordsUnwritable));
        assert!(updates.restart_ready(false).is_ok(), "the button stays");
        assert!(updates.arm_restart(false).is_ok());
        let records = dir.path().join("updates");
        let read_only = ReadOnly::new(&records);
        let plan = updates.finish_exit();
        drop(read_only);
        assert_eq!(
            plan.installed,
            Some(Ok(NEXT.to_string())),
            "the user's restart installs"
        );
        assert!(plan.relaunch);
        // A write that lands sets the records right again.
        assert!(updates.write_marker(PROBE_FILE, ""));
        assert!(updates.records_durable());
    }

    /// The coordinator's second exit request after a drain longer than a minute, and a
    /// second ⌘Q mid-drain, don't drop the arm.
    #[tokio::test]
    async fn later_exit_requests_during_a_slow_drain_keep_the_restart() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let (clock, now) = manual_clock();
        let updates = updates(&server, &dir).with_clock(now);
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(false).is_ok());
        updates.mark_exiting();
        clock.fetch_add(30, Ordering::SeqCst);
        updates.mark_exiting(); // a second ⌘Q mid-drain
        clock.fetch_add(90, Ordering::SeqCst);
        updates.mark_exiting(); // the coordinator's exit after the drain
        let plan = updates.finish_exit();
        assert!(plan.relaunch);
        assert_eq!(plan.installed, Some(Ok(NEXT.to_string())));
    }

    /// The pending record a forced arm writes is promoted to the attempt just before the
    /// install; a crash mid-drain leaves only the pending one, which isn't a failure.
    #[tokio::test]
    async fn a_forced_restart_promotes_its_record_and_a_crash_mid_drain_is_no_failure() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let records = dir.path().join("updates");
        let updates = updates(&server, &dir);
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(true).is_ok());
        // A crash mid-drain: the next launch, still on this build.
        let relaunched = Updates::new(&server.uri(), RUNNING.into(), DAY)
            .unwrap()
            .installing_into(&dir.path().join("Codesense.app"))
            .with_marker_dir(records.clone());
        assert!(!records.join(RESTART_PENDING_FILE).exists());
        relaunched.run(app.handle(), offer(NEXT)).await;
        assert_eq!(relaunched.attention(), None, "not a failed install");
        assert!(relaunched.restart_ready(false).is_ok());
        // The exit that does come promotes the record, then installs.
        assert!(relaunched.arm_restart(true).is_ok());
        let plan = relaunched.finish_exit();
        assert_eq!(plan.installed, Some(Ok(NEXT.to_string())));
        assert!(!records.join(RESTART_PENDING_FILE).exists());
        assert_eq!(
            std::fs::read_to_string(records.join(RESTART_ATTEMPTED_FILE)).unwrap(),
            NEXT
        );
    }

    /// A user arm that replaces a forced one drops the forced arm's pending record.
    #[tokio::test]
    async fn a_user_arm_replacing_a_forced_arm_drops_its_pending_record() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let pending = dir.path().join("updates").join(RESTART_PENDING_FILE);
        let updates = updates(&server, &dir);
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(true).is_ok());
        assert!(pending.exists());
        assert!(updates.arm_restart(false).is_ok());
        assert!(!pending.exists());
    }

    /// A forced exit with nothing to install installs nothing, even if a build is staged by
    /// the time the exit runs without a record.
    #[tokio::test]
    async fn a_forced_exit_without_a_record_installs_nothing() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let records = dir.path().join("updates");
        let updates = updates(&server, &dir);
        let wanted = Arc::new(AtomicBool::new(true));
        let gate = wanted.clone();
        updates.set_gate(Box::new(move |_| gate.load(Ordering::SeqCst)));
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(true).is_ok());
        // Not wanted when the exit judges the attempt: no record, so no install.
        wanted.store(false, Ordering::SeqCst);
        let plan = updates.finish_exit();
        assert_eq!(
            plan,
            ExitPlan {
                installed: None,
                relaunch: true
            }
        );
        assert!(!records.join(RESTART_ATTEMPTED_FILE).exists());
        assert!(updates.has_staged(), "nothing was taken for an install");
    }

    /// A forced arm whose build is withdrawn before the exit leaves no record behind.
    #[tokio::test]
    async fn a_forced_arm_with_nothing_installed_leaves_no_record() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let records = dir.path().join("updates");
        let updates = updates(&server, &dir);
        let wanted = Arc::new(AtomicBool::new(true));
        let gate = wanted.clone();
        updates.set_gate(Box::new(move |_| gate.load(Ordering::SeqCst)));
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(true).is_ok());
        wanted.store(false, Ordering::SeqCst);
        let plan = updates.finish_exit();
        assert_eq!(
            plan,
            ExitPlan {
                installed: None,
                relaunch: true
            }
        );
        assert!(!records.join(RESTART_PENDING_FILE).exists());
        assert!(!records.join(RESTART_ATTEMPTED_FILE).exists());
    }

    /// A click during a plain quit's drain doesn't arm a relaunch.
    #[tokio::test]
    async fn a_click_during_a_plain_quit_does_not_arm() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        updates.run(app.handle(), offer(NEXT)).await;
        updates.mark_exiting();
        assert_eq!(updates.arm_restart(false), Err(NotReady::Closing));
        assert!(!updates.finish_exit().relaunch);
    }

    /// A forced restart records its attempt when it is armed, and isn't armed without the
    /// record; a dropped forced arm takes its record with it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_forced_arm_writes_its_pending_record_or_refuses() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        updates.run(app.handle(), offer(NEXT)).await;
        let pending = dir.path().join("updates").join(RESTART_PENDING_FILE);
        assert!(updates.arm_restart(true).is_ok());
        assert!(pending.exists(), "recorded at the arm, as pending");
        updates.disarm_restart();
        assert!(!pending.exists(), "a dropped arm leaves no record");
        let _guard = ReadOnly::new(&dir.path().join("updates"));
        assert_eq!(updates.arm_restart(true), Err(NotReady::NotRecorded));
        assert!(!updates.restart_armed());
    }

    /// An arm older than a minute when the app starts to quit is stale: a plain quit.
    #[tokio::test]
    async fn an_arm_already_stale_when_the_quit_starts_does_not_relaunch() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let (clock, now) = manual_clock();
        let updates = updates(&server, &dir).with_clock(now);
        updates.run(app.handle(), offer(NEXT)).await;
        assert!(updates.arm_restart(false).is_ok());
        clock.fetch_add(61, Ordering::SeqCst);
        updates.mark_exiting();
        assert!(!updates.finish_exit().relaunch);
    }

    /// A withdrawn build that failed at a restart no longer asks for an install by hand.
    #[test]
    fn a_withdrawn_failed_build_drops_the_install_by_hand_line() {
        let markers = tempfile::tempdir().unwrap();
        std::fs::write(markers.path().join(RESTART_FAILED_FILE), NEXT).unwrap();
        let old = Updates::new("https://updates.codesense.ai", RUNNING.into(), DAY)
            .unwrap()
            .with_marker_dir(markers.path().to_path_buf());
        old.note_offer(NEXT);
        assert_eq!(old.attention(), Some(Attention::InstallFailed));
        old.clear_unless(Some(RUNNING));
        assert_eq!(old.attention(), None);
    }

    /// The loop guard: a folder that can't take the update records turns forced restarts off,
    /// and a relaunch on the old build whose failure can't be saved keeps the attempt record.
    #[cfg(unix)]
    #[test]
    fn records_that_cant_be_written_turn_forced_restarts_off() {
        let markers = tempfile::tempdir().unwrap();
        let fine = Updates::new("https://updates.codesense.ai", RUNNING.into(), DAY)
            .unwrap()
            .with_marker_dir(markers.path().to_path_buf());
        assert!(fine.records_durable());
        assert!(
            !Updates::new("https://updates.codesense.ai", RUNNING.into(), DAY)
                .unwrap()
                .records_durable(),
            "no folder, no guard"
        );
        std::fs::write(markers.path().join(RESTART_ATTEMPTED_FILE), NEXT).unwrap();
        let read_only = ReadOnly::new(markers.path());
        let locked = Updates::new("https://updates.codesense.ai", RUNNING.into(), DAY)
            .unwrap()
            .with_marker_dir(markers.path().to_path_buf());
        let wrote = locked.write_marker(PROBE_FILE, "");
        drop(read_only);
        assert!(
            !locked.records_durable(),
            "no forced restart without the guard"
        );
        assert_eq!(
            *locked.restart_failed.lock().unwrap(),
            Some(NEXT.to_string()),
            "in memory first"
        );
        assert!(
            markers.path().join(RESTART_ATTEMPTED_FILE).exists(),
            "the attempt stays until the failure is saved"
        );
        assert!(!wrote, "the folder takes no record");
    }

    /// A failure recorded for one build doesn't mark a different staged build.
    #[tokio::test]
    async fn a_failure_recorded_for_another_build_does_not_mark_the_staged_one() {
        let server = MockServer::start().await;
        serve(&server, NEXT, &good()).await;
        let (app, dir) = (app(), tempfile::tempdir().unwrap());
        let updates = updates(&server, &dir);
        *updates.restart_failed.lock().unwrap() = Some("0.0.33".into());
        updates.run(app.handle(), offer(NEXT)).await;
        assert_eq!(updates.attention(), None);
        assert_eq!(updates.restart_ready(false), Ok(NEXT.into()));
        *updates.restart_failed.lock().unwrap() = Some(NEXT.into());
        assert_eq!(updates.attention(), Some(Attention::InstallFailed));
    }

    #[test]
    fn nothing_staged_installs_nothing_at_quit() {
        let updates = Updates::new("https://updates.codesense.ai", RUNNING.into(), DAY).unwrap();
        assert!(updates.install_on_exit().is_none());
    }
}
