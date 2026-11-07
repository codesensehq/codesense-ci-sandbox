// SPDX-License-Identifier: MIT
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

use log::{debug, info, warn};
use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{Emitter, Manager};

use crate::agents::launch_check::{cli_version_warning, LaunchWarning};
use codesense_pty_core::agent_cli::{by_binary, descriptor, AgentCli};

/// Minimum required git version for Code Sense sync operations.
const MIN_GIT_VERSION: (u32, u32) = (2, 44);

// SW-AN-2: the agent binary name and its version floor are no longer hardcoded
// here — they are descriptor data read via `codesense_pty_core::agent_cli::descriptor`
// (`binary` / `min_version`). SW-OB-12's rationale still holds:
// codesense detects and uses the user's installed agent rather than pinning its
// own, and below the floor a detected binary is treated as unavailable.

/// Managed binary directory under the user's home.
fn codesense_bin_dir() -> Option<PathBuf> {
    super::paths::codesense_home_dir().map(|h| h.join("bin"))
}

/// Where a resolved agent binary was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentSource {
    /// The first install on the user's login-shell `$PATH` — what their terminal runs.
    LoginPath,
    /// A fixed package-manager location, used only when the PATH probe failed or
    /// yielded no install meeting the version floor.
    KnownLocation,
}

/// Another install of an agent CLI that was found but not used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadowedInstall {
    pub path: String,
    pub version: Option<String>,
    /// Whether this install is on the login-shell `$PATH` (later than the winner)
    /// or only at a fixed fallback location.
    pub on_path: bool,
}

/// The agent binary a launch spawns: its path and the version recorded when it was
/// resolved (no `--version` spawn at launch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentBinary {
    pub path: PathBuf,
    pub version: Option<String>,
}

/// What a launch finds in the cache for a basename. See [`DependencyManager::launch_binary`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchBinaryLookup {
    /// The cached binary is still the file that was resolved.
    Ready(ResolvedAgentBinary),
    /// A path was cached but it is gone, no longer executable, or changed in place
    /// (canonical target or mtime differ) — re-resolve before launching.
    Stale,
    /// Nothing resolved (not installed, below floor) or still probing — refuse.
    Unresolved,
}

/// What a resolution's first phase leaves for its second: the winner it stored
/// (`None` = nothing usable) and the other installs still to be version-checked
/// (`true` = on the login-shell PATH).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PendingShadowed {
    pub winner: Option<PathBuf>,
    pub candidates: Vec<(PathBuf, bool)>,
}

/// The identity a resolved binary is checked against at launch: its canonical
/// target plus that target's mtime, length and (on unix) inode. The length and
/// inode catch an in-place upgrade that keeps the path and resets the mtime, as
/// npm does (it writes package files with a fixed mtime).
#[derive(Debug, Clone, PartialEq, Eq)]
struct BinaryFingerprint {
    canonical: PathBuf,
    mtime: SystemTime,
    len: u64,
    inode: u64,
    /// Unlike mtime, Unix change time cannot be restored by an installer.
    change_time: (i64, i64),
}

/// How long after a launch found an agent CLI gone (no usable install anywhere)
/// before a later launch looks again. A binary that vanished mid-upgrade is picked
/// up by a later launch, at most one re-probe per cooldown, instead of staying
/// unavailable until a manual Re-check.
const LOST_RETRY_AFTER: Duration = Duration::from_secs(30);

/// One resolved agent binary: its detected path (`None` = unresolved / not
/// installed / below floor) and whether a background login-shell probe is still
/// in flight. SW-AN-44: the SW-OB-12 `resolving` splash-gate semantics now hold
/// **per agent** — one entry per agent basename.
#[derive(Debug, Clone, Default)]
struct AgentEntry {
    path: Option<PathBuf>,
    /// SW-OB-12: true while the background resolution for this basename is in
    /// flight. The frontend keeps the app on the splash — not the guidance screen —
    /// while the active agent's entry has this set.
    resolving: bool,
    /// The resolved binary's `--version` (or, with no path, a below-floor version
    /// found, for "found vX, need Y" guidance).
    version: Option<String>,
    source: Option<AgentSource>,
    /// What the launch-time staleness check compares against.
    fingerprint: Option<BinaryFingerprint>,
    /// Other installs found and not used, filled after the winner is stored.
    shadowed: Vec<ShadowedInstall>,
    /// When a launch-triggered re-resolve found no usable install (the cached binary
    /// vanished). Lets a later launch retry after [`LOST_RETRY_AFTER`]; `None` for
    /// every other resolution.
    lost_at: Option<Instant>,
}

/// Inner state for DependencyManager, protected by RwLock for re-resolution after install.
struct DependencyManagerInner {
    git_path: Option<PathBuf>,
    git_version: Option<String>,
    /// SW-AN-44: resolved agent binaries keyed by descriptor basename (was a single
    /// `claude_path` + `claude_resolving`). Startup seeds+probes the bundled
    /// (default) basename exactly as before; additional basenames are filled by
    /// [`DependencyManager::resolve_agent_with_login_path`]. `git_path` stays separate —
    /// git is host infra, not a launchable agent.
    agents: BTreeMap<String, AgentEntry>,
}

/// Resolved dependency paths for git and claude binaries.
///
/// Git is managed at ~/.codesense/bin/git. Agent CLIs (SW-OB-12) are *detected*
/// from the user's install — the first install on the login-shell `$PATH`, with
/// fixed known locations only when that probe fails — rather than managed/pinned.
/// Uses interior mutability so paths can be updated after (re-)resolution without app restart.
pub struct DependencyManager {
    inner: RwLock<DependencyManagerInner>,
    /// Single-flight for launch-triggered re-resolution of a stale binary, so a burst
    /// of launches right after an uninstall runs one probe.
    stale_resolve: Mutex<()>,
    /// The fixed install locations a lost binary is checked at during its retry cooldown
    /// ([`DependencyManager::lost_binary_is_back`]). Production uses [`known_agent_locations`];
    /// tests inject their own so a CLI installed on the machine cannot change the outcome.
    known_locations: Box<dyn Fn(&str) -> Vec<PathBuf> + Send + Sync>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub available: bool,
    pub path: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentBinaryStatus {
    pub available: bool,
    pub path: Option<String>,
    /// SW-OB-12: detection of this agent's binary is still in flight (background
    /// login-shell probe). While true the frontend shows the splash, not the
    /// missing-dependency guidance — `available:false` here is "not yet known",
    /// not "confirmed missing".
    pub resolving: bool,
    /// The resolved binary's version (or a below-floor version found, when not available).
    pub version: Option<String>,
    /// Where the resolved binary was found; `None` when unresolved.
    pub source: Option<AgentSource>,
    /// Other installs found and not used.
    pub shadowed: Vec<ShadowedInstall>,
    /// Whether `version` is inside the range CodeSense is qualified on; `None` when the
    /// version is unknown or the CLI has no qualified range.
    pub qualified: Option<bool>,
    /// The qualified range, as the user reads it; `None` with no range for this CLI.
    pub supported: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyStatus {
    pub git: GitStatus,
    /// SW-AN-44: per-agent status keyed by descriptor basename (was a single claude-named
    /// `claude` field). With only the built-in Claude descriptor installed, `agents["claude"]`
    /// is byte-identical to the old `.claude`. Serializes as a JSON object.
    pub agents: BTreeMap<String, AgentBinaryStatus>,
}

impl DependencyManager {
    #[cfg(test)]
    pub fn git(&self) -> Option<PathBuf> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .git_path
            .clone()
    }
    /// Resolve managed binary paths. Called once on startup.
    pub fn resolve() -> Self {
        let bin_dir = codesense_bin_dir();

        // Create ~/.codesense/bin/ eagerly if it doesn't exist
        if let Some(ref dir) = bin_dir {
            let _ = std::fs::create_dir_all(dir);
        }

        let (git_path, git_version) = Self::resolve_git();
        // SW-OB-12: the bundled agent is resolved ENTIRELY on a background thread
        // (see `spawn_agent_background_resolve`), so `.setup()` / first paint never
        // blocks on the login-shell `$PATH` probe or a `<agent> --version` spawn.
        // Start in the resolving state; the background resolution clears it and
        // emits `dep-status-changed`.
        info!(
            target: "deps",
            "resolve complete: git={} (version={}), agent=pending (resolving in background)",
            git_path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not found".into()),
            git_version.as_deref().unwrap_or("unknown"),
        );

        // SW-AN-44: seed the bundled (default) basename in the resolving state so
        // the boot splash gates on its background probe exactly as before. Other
        // built-in basenames are filled lazily by `resolve_agent_with_login_path`
        // (the live active-team warm trigger rides SW-AN-45).
        let mut agents = BTreeMap::new();
        agents.insert(
            crate::agents::builtin::preferred_binary().to_owned(),
            AgentEntry {
                resolving: true,
                ..AgentEntry::default()
            },
        );

        DependencyManager {
            inner: RwLock::new(DependencyManagerInner {
                git_path,
                git_version,
                agents,
            }),
            stale_resolve: Mutex::new(()),
            known_locations: Box::new(known_agent_locations),
        }
    }

    /// Re-resolve ALL dependency paths — git plus a FULL resolution (login-shell
    /// `$PATH` first, known locations as the fallback) of every known agent
    /// basename, clearing each agent's `resolving`. Reserved for the explicit
    /// manual re-check (`recheck_dependencies`), which is off the main thread. All
    /// basenames share ONE fresh `$PATH` probe.
    ///
    /// SW-OB-12: the file watcher and post-git-install refresh use
    /// [`re_resolve_git`] instead — an agent is detected/refreshed via the
    /// re-check button and the startup background probe, never by polling PATH
    /// from the watcher (which would spawn a login shell on every bin write).
    pub fn re_resolve(&self) {
        self.re_resolve_git();

        // Re-resolve every known agent basename, always including the
        // bundled/default one even if the map was somehow emptied.
        let mut basenames: Vec<String> = {
            let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
            inner.agents.keys().cloned().collect()
        };
        let default_basename = crate::agents::builtin::preferred_binary().to_owned();
        if !basenames.contains(&default_basename) {
            basenames.push(default_basename);
        }
        let login_path = probe_login_path();
        for basename in &basenames {
            self.resolve_agent_with_login_path(basename, login_path.as_deref(), || {});
        }
    }

    /// SW-OB-12: re-resolve GIT ONLY, leaving the detected agent state intact.
    /// Used by the file watcher (git health at `~/.codesense/bin`) and the
    /// post-git-install refresh — neither of which should trigger the heavy
    /// login-shell agent probe. Cheap (filesystem stat + `git --version`).
    pub fn re_resolve_git(&self) {
        let (git_path, git_version) = Self::resolve_git();

        info!(
            target: "deps",
            "git re-resolve complete: git={} (version={})",
            git_path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not found".into()),
            git_version.as_deref().unwrap_or("unknown"),
        );

        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.git_path = git_path;
        inner.git_version = git_version;
    }

    /// SW-OB-12 / SW-AN-44: resolve `basename` against an already-probed login-shell
    /// `$PATH` (`None` = the probe failed; known locations are then the fallback) and
    /// store the result, clearing that agent's `resolving`. MUST be invoked off the
    /// main thread (spawns child processes). Two phases: store the winner and clear
    /// `resolving`, run `after_winner` (so a caller can notify the frontend without waiting), then
    /// version-check the shadowed installs and store them.
    fn resolve_agent_with_login_path(
        &self,
        basename: &str,
        login_path: Option<&str>,
        after_winner: impl FnOnce(),
    ) -> Option<PathBuf> {
        self.resolve_agent_with_login_path_with(
            basename,
            login_path,
            known_agent_locations(basename),
            parse_agent_version,
            after_winner,
        )
    }

    /// [`Self::resolve_agent_with_login_path`] with the fixed locations and the
    /// version check injected (tests pass stub dirs and a closure, never a real CLI).
    fn resolve_agent_with_login_path_with(
        &self,
        basename: &str,
        login_path: Option<&str>,
        known: Vec<PathBuf>,
        parse_version: impl Fn(&Path) -> Option<String>,
        after_winner: impl FnOnce(),
    ) -> Option<PathBuf> {
        let pending = self.resolve_agent_winner_with(basename, login_path, known, &parse_version);
        after_winner();
        self.fill_shadowed(basename, &pending, &parse_version);
        self.agent(basename)
    }

    /// Phase 1: resolve and store `basename`'s winner (path, version, source,
    /// fingerprint) with `resolving` cleared and no shadowed installs yet. Returns
    /// what [`Self::fill_shadowed`] needs.
    fn resolve_agent_winner_with(
        &self,
        basename: &str,
        login_path: Option<&str>,
        known: Vec<PathBuf>,
        parse_version: impl Fn(&Path) -> Option<String>,
    ) -> PendingShadowed {
        let resolution = Self::agent_resolution(basename, login_path, known, parse_version);
        self.store_winner(basename, resolution)
    }

    /// Resolve `basename` against `login_path` and `known`, gated on its floor,
    /// without storing anything.
    fn agent_resolution(
        basename: &str,
        login_path: Option<&str>,
        known: Vec<PathBuf>,
        parse_version: impl Fn(&Path) -> Option<String>,
    ) -> AgentResolution {
        let floor = Self::floor_for_basename(basename);
        resolve_agent_binary(basename, login_path, known, parse_version, |version| {
            check_version_minimum(version, floor)
        })
    }

    /// Store a resolution's winner for `basename` (phase 1). Returns the winner it
    /// stored and its shadowed candidates.
    fn store_winner(&self, basename: &str, resolution: AgentResolution) -> PendingShadowed {
        self.store_resolution(basename, resolution, None)
    }

    /// Store `resolution` for `basename` (phase 1), with `lost_at` stamped when a
    /// launch found the cached binary gone and nothing usable in its place. Any
    /// resolution that found nothing starts the retry clock: a startup probe can miss
    /// an installed CLI (a slow `--version` on a busy machine), and without a stamp
    /// every launch was refused until the next restart.
    fn store_resolution(
        &self,
        basename: &str,
        resolution: AgentResolution,
        lost_at: Option<Instant>,
    ) -> PendingShadowed {
        let lost_at = match (&resolution.path, lost_at) {
            (None, None) => Some(Instant::now()),
            (_, lost_at) => lost_at,
        };
        info!(
            target: "deps",
            "agent resolve complete: {basename}={} (version={}, source={:?})",
            resolution.path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "not found".into()),
            resolution.version.as_deref().unwrap_or("unknown"),
            resolution.source,
        );
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        inner.agents.insert(
            basename.to_string(),
            AgentEntry {
                path: resolution.path.clone(),
                resolving: false,
                version: resolution.version,
                source: resolution.source,
                fingerprint: resolution.fingerprint,
                shadowed: Vec::new(),
                lost_at,
            },
        );
        PendingShadowed {
            winner: resolution.path,
            candidates: resolution.shadowed_candidates,
        }
    }

    /// Phase 2: version-check each shadowed candidate and store them on
    /// `basename`'s entry, logging one warning when any exist. Runs after the
    /// winner is stored, so it never lengthens the splash or a launch. Skipped when
    /// a newer resolution replaced the winner phase 1 stored, so an old list never
    /// lands on a newer winner.
    fn fill_shadowed(
        &self,
        basename: &str,
        pending: &PendingShadowed,
        parse_version: impl Fn(&Path) -> Option<String>,
    ) {
        if pending.candidates.is_empty() {
            return;
        }
        let shadowed: Vec<ShadowedInstall> = pending
            .candidates
            .iter()
            .map(|(path, on_path)| ShadowedInstall {
                path: path.display().to_string(),
                version: parse_version(path),
                on_path: *on_path,
            })
            .collect();
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = inner.agents.get_mut(basename) else {
            return;
        };
        if entry.path != pending.winner {
            debug!(
                target: "deps",
                "{basename}: winner changed since its shadowed installs were found — not storing them"
            );
            return;
        }
        let winner = entry
            .path
            .as_ref()
            .map(|p| {
                format!(
                    "{} ({})",
                    p.display(),
                    entry.version.as_deref().unwrap_or("unknown version")
                )
            })
            .unwrap_or_else(|| "none".to_string());
        let listed = shadowed
            .iter()
            .map(|s| {
                format!(
                    "{} ({}{})",
                    s.path,
                    s.version.as_deref().unwrap_or("unknown version"),
                    if s.on_path { "" } else { ", not on PATH" }
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        warn!(
            target: "deps",
            "{basename}: using {winner}; other installs found and not used: {listed}"
        );
        entry.shadowed = shadowed;
    }

    /// Whether a binary a launch found gone at `lost` (the miss's `Instant`) is executable
    /// at one of its known install locations, changed since that miss. Ctime, not mtime: a
    /// reinstall can keep an older mtime, but its inode change always moves ctime. A file
    /// unchanged since the miss is still unusable, so it does not re-probe every launch.
    fn lost_binary_is_back(&self, basename: &str, lost: Instant) -> bool {
        let Some(missed_at) = SystemTime::now().checked_sub(lost.elapsed()) else {
            return false;
        };
        (self.known_locations)(basename)
            .iter()
            .any(|path| is_executable_file(path) && changed_since(path, missed_at))
    }

    /// The binary a launch of `basename` should spawn, checked against what was
    /// resolved with stat/canonicalize only (no spawn — safe on async threads).
    pub fn launch_binary(&self, basename: &str) -> LaunchBinaryLookup {
        let entry = {
            let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
            match inner.agents.get(basename) {
                Some(entry) if !entry.resolving => entry.clone(),
                _ => return LaunchBinaryLookup::Unresolved,
            }
        };
        let Some(path) = entry.path else {
            // A binary a launch found gone is looked for again once its cooldown passed.
            // A binary put back at a known install location since its miss is re-resolved
            // at once rather than refused for the rest of the cooldown (stat-only).
            return match entry.lost_at {
                Some(lost)
                    if lost.elapsed() >= LOST_RETRY_AFTER
                        || self.lost_binary_is_back(basename, lost) =>
                {
                    LaunchBinaryLookup::Stale
                }
                _ => LaunchBinaryLookup::Unresolved,
            };
        };
        // A fingerprintable file with none recorded is re-resolved once (which
        // records one); a file that can never be fingerprinted matches its `None`.
        let unchanged = is_executable_file(&path) && binary_fingerprint(&path) == entry.fingerprint;
        if unchanged {
            LaunchBinaryLookup::Ready(ResolvedAgentBinary {
                path,
                version: entry.version,
            })
        } else {
            LaunchBinaryLookup::Stale
        }
    }

    /// Re-resolve a stale `basename` for a launch, once across concurrent launches:
    /// a launch that waited on another's re-resolve re-checks the cache and finds
    /// it fresh. Returns the new lookup plus, when THIS call stored a new winner, the
    /// winner and the shadowed candidates still to be version-checked
    /// ([`Self::fill_shadowed_after_launch`]). Also re-probes an entry a launch
    /// found gone once [`LOST_RETRY_AFTER`] has passed. A re-resolve that finds no
    /// usable install stores the miss stamped `lost_at` and returns `Unresolved` (the
    /// launch is refused); a later launch retries after the cooldown, in case the
    /// binary was mid-upgrade. Blocking (spawns the login-shell probe) — call off the
    /// async runtime.
    pub fn re_resolve_stale(
        &self,
        basename: &str,
    ) -> (LaunchBinaryLookup, Option<PendingShadowed>) {
        self.re_resolve_stale_with(basename, || {
            let login_path = probe_login_path();
            Self::agent_resolution(
                basename,
                login_path.as_deref(),
                known_agent_locations(basename),
                parse_agent_version,
            )
        })
    }

    fn re_resolve_stale_with(
        &self,
        basename: &str,
        resolve: impl FnOnce() -> AgentResolution,
    ) -> (LaunchBinaryLookup, Option<PendingShadowed>) {
        let _flight = self.stale_resolve.lock().unwrap_or_else(|e| e.into_inner());
        match self.launch_binary(basename) {
            LaunchBinaryLookup::Stale => {}
            other => return (other, None),
        }
        info!(target: "deps", "{basename}: cached binary changed or is gone — re-resolving for launch");
        // Stamped before the probe, so a reinstall that lands while the probe runs still
        // counts as changed after this miss on the next launch.
        let missed_at = Instant::now();
        let resolution = resolve();
        if resolution.path.is_none() {
            info!(
                target: "deps",
                "{basename}: no usable install found for launch — retrying after {LOST_RETRY_AFTER:?}"
            );
            let pending = self.store_resolution(basename, resolution, Some(missed_at));
            return (LaunchBinaryLookup::Unresolved, Some(pending));
        }
        let pending = self.store_winner(basename, resolution);
        (self.launch_binary(basename), Some(pending))
    }

    /// Version-check and store the shadowed installs a launch-triggered re-resolve
    /// found ([`Self::re_resolve_stale`]), unless its winner was replaced since.
    /// Blocking.
    pub fn fill_shadowed_after_launch(&self, basename: &str, pending: &PendingShadowed) {
        self.fill_shadowed(basename, pending, parse_agent_version);
    }

    /// Claim the first probe of `basename`: `true` when nothing has probed it yet, and the
    /// claim is recorded (as not found) so a later or concurrent warm skips it. A warm
    /// never probes a claimed basename again; a launch retries a miss once its cooldown
    /// passes, and the explicit re-check ([`Self::re_resolve`]) probes it at once.
    fn claim_first_probe(&self, basename: &str) -> bool {
        let mut inner = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if inner.agents.contains_key(basename) {
            return false;
        }
        inner
            .agents
            .insert(basename.to_string(), AgentEntry::default());
        true
    }

    /// Whether the background resolution for `basename` is still in flight (the
    /// startup resolution of the default basename). An **absent** entry is treated
    /// as *not resolving* — the bounded-resolving safeguard, so a not-yet-seeded
    /// basename never wedges the splash indefinitely.
    pub fn agent_resolving(&self, basename: &str) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .agents
            .get(basename)
            .map(|e| e.resolving)
            .unwrap_or(false)
    }

    /// Get the resolved binary path for a descriptor basename (`None` if not installed /
    /// still probing / below floor). SW-AN-44: the agent-neutral successor to the
    /// former `claude()`; `agent(default_basename)` equals the old `claude()`.
    pub fn agent(&self, basename: &str) -> Option<PathBuf> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .agents
            .get(basename)
            .and_then(|e| e.path.clone())
    }

    /// Get the dependency status for the frontend.
    pub fn status(&self) -> DependencyStatus {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        DependencyStatus {
            git: GitStatus {
                available: inner.git_path.is_some(),
                path: inner.git_path.as_ref().map(|p| p.display().to_string()),
                version: inner.git_version.clone(),
            },
            agents: inner
                .agents
                .iter()
                .map(|(basename, entry)| {
                    (
                        basename.clone(),
                        AgentBinaryStatus {
                            available: entry.path.is_some(),
                            path: entry.path.as_ref().map(|p| p.display().to_string()),
                            resolving: entry.resolving,
                            version: entry.version.clone(),
                            source: entry.source,
                            shadowed: entry.shadowed.clone(),
                            qualified: Self::version_qualified(basename, entry.version.as_deref()),
                            supported: by_binary(basename)
                                .map(|cli| descriptor(cli).release.supported.to_string()),
                        },
                    )
                })
                .collect(),
        }
    }

    /// Whether the CLI `basename` at `version` is inside its qualified range.
    fn version_qualified(basename: &str, version: Option<&str>) -> Option<bool> {
        let cli = by_binary(basename)?;
        Some(descriptor(cli).release.version_qualified(version?))
    }

    /// The launch warning for the cached version of `runtime`'s CLI, when it is outside
    /// the qualified range.
    pub fn version_warning(&self, runtime: AgentCli) -> Option<LaunchWarning> {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let version = inner
            .agents
            .get(descriptor(runtime).binary.as_str())?
            .version
            .as_deref();
        cli_version_warning(runtime, version)
    }

    /// Resolve the user's system `git` — detection, not management. codesense no
    /// longer downloads or pins its own git; it uses whatever git the user has
    /// (macOS/Linux ship one; the onboarding gate guides a user who has none).
    /// Delegates to [`resolve_versioned_binary`]:
    /// known-locations fast-path then a login-shell probe, version-gated to
    /// [`MIN_GIT_VERSION`]. The version is returned even below the floor, so the
    /// gate can show "found vX, need Y" guidance.
    fn resolve_git() -> (Option<PathBuf>, Option<String>) {
        resolve_versioned_binary(
            "git",
            known_git_locations(),
            GIT_PROBE_TIMEOUT,
            parse_git_version,
            check_git_version_minimum,
        )
    }

    /// The version floor for a CLI `basename`: its built-in descriptor's `min_version`.
    /// An unknown basename admits
    /// any parseable version (the `(0, 0)` sentinel — never a bypass; a 2-part
    /// parse is still required upstream in [`check_version_minimum`]).
    fn floor_for_basename(basename: &str) -> (u32, u32) {
        codesense_pty_core::agent_cli::by_binary(basename)
            .map_or((0, 0), |cli| descriptor(cli).min_version)
    }
}

/// Adds the warning for the cached version of the CLI that `resolved` launches, when it is
/// outside the range CodeSense is qualified on. The resolver itself never reads the cache.
pub(crate) fn note_cli_version<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    resolved: &mut crate::mcp::agent_tools::ResolvedItem,
) {
    let (Some(deps), Some(cli)) = (
        app.try_state::<DependencyManager>(),
        by_binary(&resolved.launch.binary),
    ) else {
        return;
    };
    resolved.warnings.extend(deps.version_warning(cli));
}

/// Test constructors for callers outside this module: every install is reported at
/// `version`, so nothing spawns a shell or a CLI.
#[cfg(test)]
impl DependencyManager {
    /// A manager holding only `basename`, resolved from the PATH value `login_path`.
    pub(crate) fn resolved_for_test(basename: &str, login_path: &str, version: &str) -> Self {
        let dm = DependencyManager {
            inner: RwLock::new(DependencyManagerInner {
                git_path: None,
                git_version: None,
                agents: BTreeMap::new(),
            }),
            stale_resolve: Mutex::new(()),
            known_locations: Box::new(no_known_locations),
        };
        let version = version.to_string();
        dm.store_winner(
            basename,
            Self::agent_resolution(basename, Some(login_path), vec![], move |_| {
                Some(version.clone())
            }),
        );
        dm
    }

    /// A manager whose cache holds each `(basename, version)` as an installed CLI, with
    /// nothing resolved or spawned.
    pub(crate) fn cached_for_test(versions: &[(&str, Option<&str>)]) -> Self {
        let agents = versions
            .iter()
            .map(|(basename, version)| {
                (
                    basename.to_string(),
                    AgentEntry {
                        path: Some(PathBuf::from(format!("/bin/{basename}"))),
                        version: version.map(str::to_string),
                        ..AgentEntry::default()
                    },
                )
            })
            .collect();
        DependencyManager {
            inner: RwLock::new(DependencyManagerInner {
                git_path: None,
                git_version: None,
                agents,
            }),
            stale_resolve: Mutex::new(()),
            known_locations: Box::new(no_known_locations),
        }
    }

    /// [`Self::re_resolve_stale`] against the PATH value `login_path` instead of a
    /// login-shell probe.
    pub(crate) fn re_resolve_stale_for_test(
        &self,
        basename: &str,
        login_path: &str,
        version: &str,
    ) -> (LaunchBinaryLookup, Option<PendingShadowed>) {
        let version = version.to_string();
        self.re_resolve_stale_with(basename, || {
            Self::agent_resolution(basename, Some(login_path), vec![], move |_| {
                Some(version.clone())
            })
        })
    }
}

/// Timeout for the login-shell agent probe. Kept tight (gates first paint via
/// the resolving splash) but generous enough for a cold interactive shell init.
const AGENT_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Timeout for the login-shell `git` probe. Same budget as the claude probe.
const GIT_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Known filesystem locations a user's agent binary may live, in fallback order.
/// Consulted only when the login-shell `$PATH` probe fails or yields no install
/// meeting the floor; their hits off the PATH are also reported as shadowed
/// installs. SW-AN-2: the install *directories* are generic package-manager bin
/// dirs (not per-agent knowledge); only the `basename` is descriptor data.
fn known_agent_locations(basename: &str) -> Vec<PathBuf> {
    let mut locs = vec![
        PathBuf::from("/opt/homebrew/bin").join(basename), // Homebrew (Apple silicon)
        PathBuf::from("/usr/local/bin").join(basename),    // Homebrew (Intel) / manual
    ];
    if let Some(home) = dirs::home_dir() {
        locs.push(home.join(".local/bin").join(basename)); // pipx / user-local
        locs.push(home.join(".npm-global/bin").join(basename)); // npm -g custom prefix
        locs.push(home.join(".bun/bin").join(basename)); // bun global
        locs.push(home.join("Library/pnpm").join(basename)); // pnpm global (macOS)
        locs.push(home.join(".local/share/pnpm").join(basename)); // pnpm global (xdg)
    }
    // SW-OB-12: codesense never places its own agent binary — no ~/.codesense/bin fallback.
    locs
}

/// Test default: no fixed install location, so a lost binary retries only after its cooldown.
#[cfg(test)]
fn no_known_locations(_basename: &str) -> Vec<PathBuf> {
    Vec::new()
}

/// Whether `path`'s link or the file it resolves to changed at or after `since` (ctime,
/// which a reinstall or a repoint always moves). Both are checked: a symlink repointed
/// to an older install moves only the link's ctime, while an in-place reinstall behind
/// an unchanged link moves only the target's. Off unix there is no ctime, so the retry
/// cooldown stands.
#[cfg(unix)]
fn changed_since(path: &Path, since: SystemTime) -> bool {
    use std::os::unix::fs::MetadataExt;
    let changed_at = |meta: std::fs::Metadata| {
        let secs = meta.ctime().max(0) as u64;
        std::time::UNIX_EPOCH + Duration::new(secs, meta.ctime_nsec() as u32)
    };
    [std::fs::symlink_metadata(path), std::fs::metadata(path)]
        .into_iter()
        .filter_map(Result::ok)
        .any(|meta| changed_at(meta) > since)
}

#[cfg(not(unix))]
fn changed_since(_path: &Path, _since: SystemTime) -> bool {
    false
}

/// Probe the login shell's `$PATH` once (bounded by [`AGENT_PROBE_TIMEOUT`]). A
/// successful probe also seeds the process-wide [`LOGIN_SHELL_PATH`] cache so
/// worktree setup reuses it. `None` when the probe fails or times out.
fn probe_login_path() -> Option<String> {
    let path = resolve_login_shell_path(AGENT_PROBE_TIMEOUT);
    if let Some(ref value) = path {
        let _ = LOGIN_SHELL_PATH.get_or_init(|| value.clone());
    }
    path
}

/// Whether `path` is a regular file (following symlinks) with an executable bit.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// The canonical target of `path`, or `path` itself when it can't be canonicalized.
fn canonical_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The canonical target with its mtime, length and inode — what a launch compares
/// the cached binary against. `None` when the file is gone.
fn binary_fingerprint(path: &Path) -> Option<BinaryFingerprint> {
    let canonical = std::fs::canonicalize(path).ok()?;
    let meta = std::fs::metadata(&canonical).ok()?;
    let mtime = meta.modified().ok()?;
    #[cfg(unix)]
    let (inode, change_time) = {
        use std::os::unix::fs::MetadataExt;
        (meta.ino(), (meta.ctime(), meta.ctime_nsec()))
    };
    #[cfg(not(unix))]
    let (inode, change_time) = (0, (0, 0));
    Some(BinaryFingerprint {
        canonical,
        mtime,
        len: meta.len(),
        inode,
        change_time,
    })
}

/// Every install of `basename` on `path_env` (a `$PATH` value), in PATH order —
/// `which -a` without a shell. Empty and relative entries are skipped, only
/// executable regular files count, and installs are de-duplicated by canonical
/// path (a symlink and its target are one install; the first spelling is kept).
fn login_path_candidates(path_env: &str, basename: &str) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for dir in path_env.split(':') {
        let dir = Path::new(dir);
        if dir.as_os_str().is_empty() || !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(basename);
        if !is_executable_file(&candidate) {
            continue;
        }
        if seen.insert(canonical_key(&candidate)) {
            candidates.push(candidate);
        }
    }
    candidates
}

/// The outcome of resolving one agent basename.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct AgentResolution {
    path: Option<PathBuf>,
    /// Captured before the version probe, so an upgrade during or after that probe
    /// remains stale instead of pairing a new file with an old version.
    fingerprint: Option<BinaryFingerprint>,
    /// The winner's version, or with no winner the first below-floor version seen.
    version: Option<String>,
    source: Option<AgentSource>,
    /// Every other distinct install: remaining PATH candidates in PATH order
    /// (`true` = on PATH), then known-location hits not on PATH (`false`).
    shadowed_candidates: Vec<(PathBuf, bool)>,
}

/// Pick the agent binary for `basename`: the first install on the login-shell
/// `$PATH` (`login_path`) whose version meets the floor wins; when the probe
/// failed (`None`) or nothing on PATH meets the floor, the first `known` location
/// meeting it. Every other distinct install is reported as shadowed. The version
/// parse and floor check are injected so tests never spawn a real CLI.
fn resolve_agent_binary(
    basename: &str,
    login_path: Option<&str>,
    known: Vec<PathBuf>,
    parse_version: impl Fn(&Path) -> Option<String>,
    meets_floor: impl Fn(&str) -> bool,
) -> AgentResolution {
    let path_candidates = login_path
        .map(|path| login_path_candidates(path, basename))
        .unwrap_or_default();
    let mut below_floor_version: Option<String> = None;
    let mut pick = |candidates: &mut dyn Iterator<Item = &PathBuf>| {
        for path in candidates {
            let fingerprint = binary_fingerprint(path);
            match parse_version(path) {
                Some(version) if meets_floor(&version) => {
                    debug!(target: "deps", "{basename} found at {} version {}", path.display(), version);
                    return Some((path.clone(), version, fingerprint));
                }
                Some(version) => {
                    debug!(target: "deps", "{basename} at {} version {} below floor — skipping", path.display(), version);
                    below_floor_version.get_or_insert(version);
                }
                None => {
                    debug!(target: "deps", "could not determine {basename} version at {}", path.display());
                }
            }
        }
        None
    };
    // The fallback runs only when every PATH candidate was checked and none met the
    // floor, so a known location naming one of them is skipped: no second spawn.
    let checked_on_path: HashSet<PathBuf> = path_candidates
        .iter()
        .map(|path| canonical_key(path))
        .collect();
    let winner = pick(&mut path_candidates.iter())
        .map(|(path, version, fingerprint)| (path, version, fingerprint, AgentSource::LoginPath))
        .or_else(|| {
            pick(&mut known.iter().filter(|path| {
                is_executable_file(path) && !checked_on_path.contains(&canonical_key(path))
            }))
            .map(|(path, version, fingerprint)| {
                (path, version, fingerprint, AgentSource::KnownLocation)
            })
        });

    let mut seen: HashSet<PathBuf> = winner
        .iter()
        .map(|(path, _, _, _)| canonical_key(path))
        .collect();
    let mut shadowed_candidates = Vec::new();
    for path in &path_candidates {
        if seen.insert(canonical_key(path)) {
            shadowed_candidates.push((path.clone(), true));
        }
    }
    for path in known.iter().filter(|path| is_executable_file(path)) {
        if seen.insert(canonical_key(path)) {
            shadowed_candidates.push((path.clone(), false));
        }
    }

    match winner {
        Some((path, version, fingerprint, source)) => AgentResolution {
            path: Some(path),
            fingerprint,
            version: Some(version),
            source: Some(source),
            shadowed_candidates,
        },
        None => AgentResolution {
            path: None,
            fingerprint: None,
            version: below_floor_version,
            source: None,
            shadowed_candidates,
        },
    }
}

/// Known filesystem locations a user's `git` may live, in priority order. The
/// cheap fast-path stats these before spawning a login shell — mirrors
/// [`known_agent_locations`] but for git: the standard package-manager and
/// system bin dirs (Homebrew, `/usr/bin` for Apple CLT / Xcode git, user-local).
fn known_git_locations() -> Vec<PathBuf> {
    let mut locs = vec![
        PathBuf::from("/opt/homebrew/bin/git"), // Homebrew (Apple silicon)
        PathBuf::from("/usr/local/bin/git"),    // Homebrew (Intel) / manual
        PathBuf::from("/usr/bin/git"),          // Apple CLT / Xcode / system git
    ];
    if let Some(home) = dirs::home_dir() {
        locs.push(home.join(".local/bin/git")); // user-local
    }
    locs
}

/// Resolve a versioned binary the way the user's environment would: (a) stat a
/// list of known install locations, spawning `<bin> --version` to gate each hit
/// against `meets_floor`; then (b) a login-shell PATH probe for the miss (picks
/// up Homebrew/asdf/npm-global that a Finder-launched `.app` doesn't inherit).
/// Returns the resolved path (first candidate meeting the floor) plus the parsed
/// version of the resolving — or last-probed — binary; the version is surfaced
/// even when it fails the floor, so callers can show "found vX, need Y" guidance.
///
/// Used by git (SW-OB-14) detection; agent CLIs resolve through
/// [`resolve_agent_binary`] (login-shell `$PATH` first). Spawns child processes
/// (version checks plus an interactive shell), so callers must run it off the
/// main thread.
fn resolve_versioned_binary(
    name: &str,
    known_locations: Vec<PathBuf>,
    probe_timeout: Duration,
    parse_version: impl Fn(&Path) -> Option<String>,
    meets_floor: impl Fn(&str) -> bool,
) -> (Option<PathBuf>, Option<String>) {
    // Retain the first (highest-priority) below-floor version seen on the fast
    // path, so that if nothing meets the floor we can still report "found vX, too
    // old" guidance rather than a bare "not found" — even when the login-shell
    // probe below yields nothing (e.g. the same below-floor binary isn't on the
    // shell's PATH, or the probe times out).
    let mut below_floor_version: Option<String> = None;

    // Fast path: stat known locations, version-check each hit.
    for path in known_locations {
        if !path.exists() {
            continue;
        }
        match parse_version(&path) {
            Some(version) if meets_floor(&version) => {
                debug!(target: "deps", "{name} found at {} version {}", path.display(), version);
                return (Some(path), Some(version));
            }
            Some(version) => {
                debug!(target: "deps", "{name} at {} version {} below floor — skipping", path.display(), version);
                below_floor_version.get_or_insert(version);
                // Keep scanning — another location may hold a newer one.
            }
            None => {
                debug!(target: "deps", "could not determine {name} version at {}", path.display());
            }
        }
    }

    // Miss: resolve via the user's login-shell PATH.
    match resolve_binary_via_login_shell(name, probe_timeout) {
        Some(path) => match parse_version(&path) {
            Some(version) if meets_floor(&version) => {
                debug!(target: "deps", "{name} resolved via login-shell probe: {} version {}", path.display(), version);
                (Some(path), Some(version))
            }
            Some(version) => {
                info!(target: "deps", "{name} at {} version {} below floor — treating as unavailable", path.display(), version);
                (None, Some(version))
            }
            // Probe found nothing parseable — fall back to any below-floor version
            // the fast path turned up, so the gate can still guide the user.
            None => (None, below_floor_version),
        },
        None => {
            debug!(target: "deps", "{name} not found on user PATH");
            (None, below_floor_version)
        }
    }
}

/// Pick the shell for the login-shell PATH probe. Prefers the user's `$SHELL`
/// when it's one that accepts the combined `-lic` (login+interactive+command)
/// flags — `zsh` and `bash` — so a bash-default user's `.bash_profile`/`.bashrc`
/// PATH is honored. Falls back to `zsh` (the macOS default) for any
/// other/unset `$SHELL`.
fn login_probe_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        let path = Path::new(&shell);
        let base_ok = path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|base| matches!(base, "zsh" | "bash"));
        // Require the path to actually exist — a stale `$SHELL` (e.g. Homebrew
        // zsh after uninstall) would otherwise make every spawn fail with no
        // fallback. `exists()` here is fine: this runs on the background thread.
        if base_ok && path.exists() {
            return shell;
        }
    }
    "zsh".to_string()
}

/// SW-OB-12 (Central risk #1): resolve a binary the way the user's shell would.
/// A Finder-launched `.app` inherits a minimal PATH, so `command -v` from our
/// process misses Homebrew/npm-global/asdf. Spawn a **login + interactive** shell
/// (`$SHELL -lic 'command -v <name>'`, see [`login_probe_shell`]) so
/// `.zprofile`/`.zshrc` (or bash equivalents) PATH edits apply — mirroring the
/// login shell a terminal session runs. Bounded by `timeout` with a kill-on-timeout
/// guard (an interactive shell can hang without a tty). Returns the first
/// existing path printed on stdout, or None.
pub fn resolve_binary_via_login_shell(name: &str, timeout: Duration) -> Option<PathBuf> {
    // Only the binary name is interpolated; restrict to a safe charset so this
    // can never inject shell syntax (defense-in-depth — callers pass literals).
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        warn!(target: "deps", "resolve_binary_via_login_shell: refusing unsafe name {:?}", name);
        return None;
    }

    let stdout = run_login_shell_capture(&format!("command -v {name}"), timeout)?;
    // `command -v` prints the resolved path (one per line). Take the first line
    // that names an existing file — interactive shells may prepend prompt noise.
    stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .find(|p| p.is_absolute() && p.exists())
}

/// Shared scaffold for the login-shell probes: spawn `$SHELL -lic <script>`
/// (see [`login_probe_shell`]) so `.zprofile`/`.zshrc` PATH edits apply and
/// return its stdout within `timeout` (see [`capture_stdout_within`]). Callers
/// parse stdout for their own shape (a resolved binary path, the `$PATH`
/// value). `script` MUST be caller-controlled — the charset check for
/// interpolated user input lives in each caller, not here.
fn run_login_shell_capture(script: &str, timeout: Duration) -> Option<String> {
    let mut command = Command::new(login_probe_shell());
    command.args(["-lic", script]);
    capture_stdout_within(command, timeout)
}

/// Run `command` and return its stdout, the whole capture bounded by
/// `timeout`: an interactive shell may hang without a tty, and a process an rc
/// file backgrounds inherits stdout and can hold the pipe open long after the
/// shell exits. stdout drains on its own thread; past the deadline a shell
/// still running is killed and reaped (None), and a shell that exited returns
/// what it printed while the backgrounded holder keeps the detached reader.
fn capture_stdout_within(mut command: Command, timeout: Duration) -> Option<String> {
    use std::io::Read;
    use std::sync::mpsc;

    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| warn!(target: "deps", "login-shell probe spawn failed: {}", e))
        .ok()?;
    let deadline = Instant::now() + timeout;

    let collected = Arc::new(Mutex::new(Vec::new()));
    let (drained_tx, drained_rx) = mpsc::channel::<()>();
    if let Some(mut stdout) = child.stdout.take() {
        let collected = Arc::clone(&collected);
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n @ 1..) = stdout.read(&mut chunk) {
                if let Ok(mut buf) = collected.lock() {
                    buf.extend_from_slice(&chunk[..n]);
                }
            }
            let _ = drained_tx.send(());
        });
    } else {
        drop(drained_tx);
    }

    // Poll until the shell exits. Every exit path reaps the child (kill + wait)
    // so no zombie is leaked.
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    warn!(target: "deps", "login-shell probe timed out after {:?}", timeout);
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                warn!(target: "deps", "login-shell probe wait failed: {}", e);
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }

    // The shell has exited; with no stdout the sender is already dropped.
    let remaining = deadline.saturating_duration_since(Instant::now());
    if let Err(mpsc::RecvTimeoutError::Timeout) = drained_rx.recv_timeout(remaining) {
        warn!(target: "deps", "login-shell probe stdout still held open after {:?}", timeout);
    }
    let bytes = collected.lock().map(|buf| buf.clone()).unwrap_or_default();
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Extract the value bracketed by `begin`/`end` sentinels from probe stdout.
/// Interactive (`-i`) shells can emit prompt/RC noise on stdout, so the value
/// is wrapped in sentinels the payload can't contain. Returns None if either
/// sentinel is absent or the bracketed value is empty. Pure — unit-tested
/// without spawning a shell.
fn extract_sentinel_value(raw: &str, begin: &str, end: &str) -> Option<String> {
    let start = raw.find(begin)? + begin.len();
    let rest = &raw[start..];
    let stop = rest.find(end)?;
    let value = &rest[..stop];
    (!value.is_empty()).then(|| value.to_string())
}

/// Sentinels bracketing the `$PATH` value in [`resolve_login_shell_path`].
/// `$PATH` is a colon-list of directories and can never contain these tokens.
const PATH_SENTINEL_BEGIN: &str = "CS_PATH_BEGIN>>";
const PATH_SENTINEL_END: &str = "<<CS_PATH_END";

/// The `$PATH` probe command. The value is bracketed by [`PATH_SENTINEL_BEGIN`]/
/// [`PATH_SENTINEL_END`] so prompt noise from the interactive (`-i`) shell can't
/// contaminate it. CRUCIAL: the sentinels are emitted from split `printf`
/// fragments (`%sBEGIN>>%s<<%sEND` + `CS_PATH_` args), so the literal markers
/// appear ONLY in the OUTPUT, never in the command TEXT. Otherwise an rc that
/// echoes the command to stdout (a verbose rc, a `preexec`/`precmd` that prints)
/// would let `extract_sentinel_value` match the ECHOED command before the real
/// value — yielding a bogus `%s` PATH, which is strictly worse than the fallback
/// (the `nice`/shell then can't PATH-resolve). Guarded by
/// `login_path_probe_script_hides_sentinels`; the round-trip is proven live by
/// `test_resolve_login_shell_path_returns_system_path`.
fn login_path_probe_script() -> String {
    // `CS_PATH_` + `BEGIN>>` = PATH_SENTINEL_BEGIN; `<<` + `CS_PATH_` + `END` =
    // PATH_SENTINEL_END. Emitted only when printf concatenates them at runtime.
    "printf '%sBEGIN>>%s<<%sEND' 'CS_PATH_' \"$PATH\" 'CS_PATH_'".to_string()
}

/// Resolve the `$PATH` the user's interactive login shell exports. A
/// Finder-launched `.app` inherits the minimal launchd PATH (no nvm, Homebrew,
/// asdf, or npm-global), so subprocesses spawned by the app — e.g. worktree
/// `setup:` steps running `npm ci` — fail with exit 127. Spawning a login +
/// interactive shell and printing `$PATH` (bracketed by sentinels to survive
/// prompt noise) yields the same PATH the user's terminal has. Returns None on
/// probe failure or an empty result — callers then fall back to the process
/// PATH (i.e. no regression versus the pre-fix behavior).
pub fn resolve_login_shell_path(timeout: Duration) -> Option<String> {
    let raw = run_login_shell_capture(&login_path_probe_script(), timeout)?;
    extract_sentinel_value(&raw, PATH_SENTINEL_BEGIN, PATH_SENTINEL_END)
}

/// Timeout for the one-shot login-shell `$PATH` probe. Same budget as the
/// per-binary probe — an interactive shell that sources a slow `.zshrc` gets a
/// few seconds before we fall back to the inherited process PATH.
const LOGIN_PATH_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Caches only a SUCCESSFUL resolution — a `None` is deliberately NOT stored (see
/// [`cached_login_shell_path`]).
static LOGIN_SHELL_PATH: OnceLock<String> = OnceLock::new();

/// Memoized, process-wide accessor for [`resolve_login_shell_path`]. A successful
/// probe is cached and the interactive shell is never spawned again. A FAILED
/// probe (`None`) is deliberately NOT memoized: a transient miss — a momentarily
/// slow `.zshrc`, a busy machine that blows the 5s budget — must NOT strand every
/// later worktree setup on the minimal launchd PATH (re-hitting exit 127) for the
/// rest of the process; the next call re-probes. BLOCKING — call from a blocking
/// context (e.g. `tokio::task::spawn_blocking`) so the poll loop doesn't stall
/// the async runtime.
pub fn cached_login_shell_path() -> Option<String> {
    if let Some(cached) = LOGIN_SHELL_PATH.get() {
        return Some(cached.clone());
    }
    // Probe outside the cache: only a Some result is stored. Under a race, each
    // caller may probe once, but `get_or_init` keeps the first winner.
    let resolved = resolve_login_shell_path(LOGIN_PATH_PROBE_TIMEOUT)?;
    Some(LOGIN_SHELL_PATH.get_or_init(|| resolved).clone())
}

/// Spawn `<path> --version` (bounded by `timeout` with a kill-on-timeout guard)
/// and parse the first digit-leading token from stdout via [`extract_semver_token`]
/// (works for both `git version X.Y.Z` and claude's `X.Y.Z (Claude Code)`).
/// Returns None on spawn/exec failure, non-zero exit, timeout, or unparseable
/// output.
///
/// Bounded because a wedged binary can otherwise hang the caller — and, during
/// the startup agent resolution, hold the splash — forever: a Node CLI cold-start
/// (`claude`: broken shim / slow home / AV scan) or a git shim that blocks on a GUI install
/// prompt (`/usr/bin/git` on a Command-Line-Tools-less mac). Shared by git
/// (SW-OB-14) and claude (SW-OB-12) — the only difference is the timeout budget.
fn parse_binary_version(path: &Path, timeout: Duration) -> Option<String> {
    let raw = probe_binary_version(path, timeout, &[])?;
    extract_semver_token(&raw)
}

fn probe_binary_version(
    path: &Path,
    timeout: Duration,
    environment: &[(String, String)],
) -> Option<String> {
    let mut child = Command::new(path)
        .arg("--version")
        .envs(environment.iter().map(|(name, value)| (name, value)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    warn!(target: "deps", "{} --version timed out after {:?}", path.display(), timeout);
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                warn!(target: "deps", "{} --version wait failed: {}", path.display(), e);
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }

    let output = child.wait_with_output().ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Spawn `git --version` (bounded by [`GIT_PROBE_TIMEOUT`]) and parse the version.
fn parse_git_version(git_path: &Path) -> Option<String> {
    parse_binary_version(git_path, GIT_PROBE_TIMEOUT)
}

/// Spawn `<agent> --version` (bounded by [`AGENT_PROBE_TIMEOUT`]) and parse the version.
fn parse_agent_version(agent_path: &Path) -> Option<String> {
    let probe = agent_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(codesense_pty_core::agent_cli::by_binary)
        .map(|cli| {
            codesense_pty_core::agent_cli::AgentCliAdapter::probe(
                codesense_pty_core::agent_cli::adapter(cli),
            )
        });
    let environment = probe
        .map(|probe| {
            codesense_pty_core::agent_cli::version_cache::resolve_env(probe.version_env, None)
        })
        .unwrap_or_default();
    let raw = probe_binary_version(agent_path, AGENT_PROBE_TIMEOUT, &environment)?;
    let version = extract_semver_token(&raw)?;
    if let Some(probe) = probe {
        // PR C supplies the typed identity after preparing its pinned runtime.
        let _ = codesense_pty_core::agent_cli::version_cache::write(agent_path, &raw, probe, None);
    }
    Some(version)
}

/// Extract the first whitespace-delimited token that starts with a digit from a
/// `--version` output line. Works for both git ("git version X.Y.Z") and claude
/// ("X.Y.Z (Claude Code)"). SW-OB-12: generalized from `extract_version_from_git_output`.
pub fn extract_semver_token(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .map(|token| token.strip_prefix('v').unwrap_or(token))
        .find(|token| token.starts_with(|c: char| c.is_ascii_digit()))
        .map(str::to_owned)
}

/// Check if a version string meets a `(major, minor)` minimum requirement.
fn check_version_minimum(version: &str, min: (u32, u32)) -> bool {
    let parts: Vec<u32> = version.split('.').filter_map(|s| s.parse().ok()).collect();

    if parts.len() < 2 {
        return false;
    }

    (parts[0], parts[1]) >= min
}

/// Check if a git version string meets the minimum requirement.
pub fn check_git_version_minimum(version: &str) -> bool {
    check_version_minimum(version, MIN_GIT_VERSION)
}

// SW-OB-12: codesense no longer manages Claude authentication. The user's own
// installed `claude` handles its own login; we neither check nor drive it.
// (Removed ClaudeAuthResult / check_claude_auth_status / ensure_claude_auth /
// check_claude_auth / sign_out_claude.)

#[tauri::command]
pub fn get_dependency_status(deps: tauri::State<'_, DependencyManager>) -> DependencyStatus {
    deps.status()
}

/// SW-OB-12: manual re-check of dependency detection. The file watcher only
/// covers `~/.codesense/bin` (managed git); a user's PATH-installed `claude`
/// won't trip it, so the guidance screen's "Re-check" button calls this to force
/// a full re-resolution off the main thread, then emits `dep-status-changed` so
/// the existing App.tsx refresh path picks up the result.
#[tauri::command]
pub async fn recheck_dependencies(app: tauri::AppHandle) -> Result<DependencyStatus, String> {
    let handle = app.clone();
    let status = crate::commands::await_blocking("recheck_dependencies", move || {
        let deps = handle.state::<DependencyManager>();
        deps.re_resolve();
        Ok(deps.status())
    })
    .await?;
    let _ = app.emit("dep-status-changed", ());
    Ok(status)
}

/// The path of the agent CLI `basename` a frontend launch should spawn, checked at
/// launch time like every backend launch (a moved or reinstalled binary is
/// re-resolved once). `None` when no usable install exists.
#[tauri::command]
pub async fn resolve_agent_cli(
    app: tauri::AppHandle,
    basename: String,
) -> Result<Option<String>, String> {
    match crate::commands::mcp_workspace::resolve_agent_for_launch(&app, &basename).await {
        Ok(binary) => Ok(Some(binary.path.display().to_string())),
        Err(reason) => {
            debug!(target: "deps", "resolve_agent_cli: no usable {basename}: {reason}");
            Ok(None)
        }
    }
}

/// Outcome of triggering the macOS Command Line Tools install (which provides
/// `git`). `status`: "triggered" (installer launched), "already_installed",
/// "unsupported" (non-macOS), or "error".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CltInstallResult {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Classify `xcode-select --install`'s outcome from its exit + stderr. Pure, so
/// it's unit-testable without spawning. `xcode-select --install` exits 0 after
/// launching the GUI installer; if the tools are already present it exits
/// non-zero with a stderr note containing "already installed".
fn classify_xcode_select(success: bool, stderr: &str) -> CltInstallResult {
    if success {
        CltInstallResult {
            status: "triggered".into(),
            message: None,
        }
    } else if stderr.to_lowercase().contains("already installed") {
        CltInstallResult {
            status: "already_installed".into(),
            message: None,
        }
    } else {
        let trimmed = stderr.trim();
        CltInstallResult {
            status: "error".into(),
            message: Some(if trimmed.is_empty() {
                "xcode-select --install failed".into()
            } else {
                trimmed.to_string()
            }),
        }
    }
}

/// SW-OB-14: trigger Apple's Command Line Tools installer (the official on-demand
/// dev-tools install, which provides `git`) via `xcode-select --install`. Launches
/// the GUI installer and returns immediately — the user completes Apple's dialog,
/// then hits "Re-check". This is a nudge, not a silent install: Apple gates the
/// actual install behind its own dialog + license (an OS boundary). macOS-only;
/// returns "unsupported" elsewhere (codesense ships macOS-only today).
#[tauri::command]
pub async fn install_command_line_tools() -> Result<CltInstallResult, String> {
    crate::commands::await_blocking("install_command_line_tools", move || {
        #[cfg(target_os = "macos")]
        {
            match Command::new("xcode-select").arg("--install").output() {
                Ok(o) => Ok(classify_xcode_select(
                    o.status.success(),
                    &String::from_utf8_lossy(&o.stderr),
                )),
                Err(e) => Ok(CltInstallResult {
                    status: "error".into(),
                    message: Some(format!("failed to run xcode-select: {e}")),
                }),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(CltInstallResult {
                status: "unsupported".into(),
                message: Some("Command Line Tools install is macOS-only".into()),
            })
        }
    })
    .await
}

/// SW-OB-12 / SW-AN-44: resolve the bundled (default) agent basename in the
/// background at startup (login-shell `$PATH` first, known locations as the
/// fallback). No-op if it is no longer resolving. Runs on Tokio's blocking pool so
/// `.setup()` / first paint never blocks. Emits `dep-status-changed` as soon as the
/// winner is stored (so the splash clears without waiting on shadowed-install
/// version checks), and again once the shadowed installs are known.
pub fn spawn_agent_background_resolve(app: tauri::AppHandle) {
    let basename = crate::agents::builtin::preferred_binary().to_owned();
    if !app.state::<DependencyManager>().agent_resolving(&basename) {
        return;
    }
    tauri::async_runtime::spawn_blocking(move || {
        let login_path = probe_login_path();
        app.state::<DependencyManager>()
            .resolve_agent_with_login_path(&basename, login_path.as_deref(), || {
                let _ = app.emit("dep-status-changed", ());
            });
        let _ = app.emit("dep-status-changed", ());
    });
}

/// Warm the deps cache for every built-in CLI's basename beyond the default one probed at
/// startup, probing each once: a CLI found or not found is not probed again on the next team
/// switch (a launch retries a miss after its cooldown; the explicit re-check re-probes it). A
/// launch reads the warmed cache via
/// [`DependencyManager::launch_binary`] and REFUSES a cold basename, so without this Codex could
/// never launch. Rides the idempotent team-activation seam (fires on first mount + every switch);
/// runs on the blocking pool (one shared login-shell `$PATH` probe, then each basename's
/// version checks) and emits
/// `dep-status-changed` so the FE deps gate re-evaluates.
pub fn spawn_builtin_agents_warm<R: tauri::Runtime>(app: tauri::AppHandle<R>) {
    tauri::async_runtime::spawn_blocking(move || {
        let default_basename = crate::agents::builtin::preferred_binary();
        let Some(dm) = app.try_state::<DependencyManager>() else {
            return;
        };
        let mut warmed = 0u32;
        // One `$PATH` probe shared by every basename this warm resolves.
        let mut login_path: Option<Option<String>> = None;
        for descriptor in AgentCli::ALL.map(descriptor) {
            let basename = descriptor.binary.as_str();
            if basename == default_basename || !dm.claim_first_probe(basename) {
                continue;
            }
            let login_path = login_path.get_or_insert_with(probe_login_path);
            // Each winner reaches the frontend before its shadowed installs are checked.
            dm.resolve_agent_with_login_path(basename, login_path.as_deref(), || {
                let _ = app.emit("dep-status-changed", ());
            });
            warmed += 1;
        }
        if warmed > 0 {
            let _ = app.emit("dep-status-changed", ());
        }
    });
}

// ---------------------------------------------------------------------------
// Dependency File Watcher
// ---------------------------------------------------------------------------

/// Holds the dependency file watcher so it stays alive for the app lifetime.
pub struct DepWatcherState {
    _watcher: Mutex<Option<RecommendedWatcher>>,
}

/// Check if a file event is relevant to dependency health.
/// Only reacts to changes on `git`, the agent binary, or `manifest.json`
/// filenames. SW-AN-2: the agent binary name is descriptor data, not a literal.
fn is_dep_relevant_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str());
    matches!(name, Some("git" | "manifest.json"))
        || name == Some(crate::agents::builtin::preferred_binary())
}

/// Start watching dependency files for deletions or modifications.
/// Watches `~/.codesense/bin/` and `~/.codesense/` (NonRecursive).
/// On relevant events: calls `DependencyManager::re_resolve()` then emits `dep-status-changed`.
/// Errors are logged but do not crash the app.
pub fn start_dep_watching(app: tauri::AppHandle) {
    let codesense_dir = match super::paths::codesense_home_dir() {
        Some(d) => d,
        None => {
            warn!(target: "deps", "dep watcher: no home directory, skipping");
            app.manage(DepWatcherState {
                _watcher: Mutex::new(None),
            });
            return;
        }
    };

    let bin_dir = codesense_dir.join("bin");

    // Ensure directories exist before watching
    let _ = std::fs::create_dir_all(&bin_dir);

    let app_handle = app.clone();
    let last_emit: Arc<Mutex<Instant>> =
        Arc::new(Mutex::new(Instant::now() - Duration::from_secs(10)));

    let watcher_result = RecommendedWatcher::new(
        move |res: Result<notify::Event, notify::Error>| {
            let event = match res {
                Ok(e) => e,
                Err(e) => {
                    debug!(target: "deps", "dep watcher error: {}", e);
                    return;
                }
            };

            // Check if any event path is a relevant dep file
            let relevant = event.paths.iter().any(|p| is_dep_relevant_file(p));
            if !relevant {
                return;
            }

            // Debounce: skip if emitted within 500ms
            if let Ok(mut last) = last_emit.lock() {
                let now = Instant::now();
                if now.duration_since(*last) < Duration::from_millis(500) {
                    return;
                }
                *last = now;
            }

            // Re-resolve cached dependency state before notifying frontend.
            // SW-OB-12: git-only — the watcher covers `~/.codesense/bin` (managed
            // git); claude is detected elsewhere (re-check button / startup probe)
            // and must not trigger a login-shell spawn on every bin write.
            let deps = app_handle.state::<DependencyManager>();
            deps.re_resolve_git();

            info!(target: "deps", "dep watcher: file change detected, emitting dep-status-changed");
            let _ = app_handle.emit("dep-status-changed", ());
        },
        Config::default(),
    );

    match watcher_result {
        Ok(mut watcher) => {
            if bin_dir.exists() {
                if let Err(e) = watcher.watch(&bin_dir, RecursiveMode::NonRecursive) {
                    warn!(target: "deps", "dep watcher: failed to watch bin dir: {}", e);
                }
            }
            if codesense_dir.exists() {
                if let Err(e) = watcher.watch(&codesense_dir, RecursiveMode::NonRecursive) {
                    warn!(target: "deps", "dep watcher: failed to watch codesense dir: {}", e);
                }
            }
            info!(target: "deps", "dep watcher: started (watching {:?} and {:?})", bin_dir, codesense_dir);
            app.manage(DepWatcherState {
                _watcher: Mutex::new(Some(watcher)),
            });
        }
        Err(e) => {
            warn!(target: "deps", "dep watcher: failed to create watcher: {}. App continues without dep watching.", e);
            app.manage(DepWatcherState {
                _watcher: Mutex::new(None),
            });
        }
    }
}

// codesense-bridge symlink under ~/.codesense/bin/.
//
// The `.mcp.json` CodeSense writes for an agent (agent launch, managed
// worktree config, MCP scope) names `~/.codesense/bin/codesense-bridge` so
// Claude Code can spawn the bridge sidecar from a stable path; the production
// binary itself lives inside an opaque `<app_bundle>/Contents/MacOS/` path.
//
// A managed symlink under `~/.codesense/bin/` (the same directory that once
// held the managed git symlink, before git was de-managed in favor of the
// user's system git).

/// Compile-time rustc target triple (e.g. `aarch64-apple-darwin`). Required
/// at runtime to reconstruct Tauri's dev-mode sidecar filename
/// (`<name>-<target-triple>`). Set by `src-tauri/build.rs` via
/// `cargo:rustc-env=TARGET_TRIPLE=<triple>`.
const TARGET_TRIPLE: &str = env!("TARGET_TRIPLE");

/// Ensure `~/.codesense/bin/codesense-bridge` is a symlink pointing to the
/// bundled `codesense-bridge` sidecar. Idempotent — re-running with a
/// correctly-pointed symlink is a no-op. Replaces stale/broken symlinks or
/// stray regular files at the link path.
///
/// **Source resolution:**
/// - Production: `<exe_dir>/codesense-bridge` (Tauri bundler renames the
///   `<name>-<target-triple>` source file to just `<name>` inside
///   `<bundle>/Contents/MacOS/` per `externalBin` convention).
/// - Dev: `<exe_dir>/codesense-bridge-<target-triple>` (some Tauri-cli dev
///   flows copy the sidecar next to the dev exe), with fallback to
///   `<src-tauri>/binaries/codesense-bridge-<target-triple>` for unbundled
///   `cargo run` flows.
///
/// Failure is logged at warn level and returned, but the app `.setup()`
/// hook does not abort — the symlink is a nice-to-have, not a hard
/// dependency, and codesense itself remains functional without it.
pub(crate) fn ensure_bridge_symlink(_app: &tauri::AppHandle) -> Result<(), String> {
    let bin_dir = codesense_bin_dir().ok_or_else(|| "no home directory".to_string())?;
    std::fs::create_dir_all(&bin_dir).map_err(|e| format!("mkdir {}: {}", bin_dir.display(), e))?;

    let source = resolve_sidecar_source("codesense-bridge")?;
    let link = bin_dir.join("codesense-bridge");
    ensure_symlink_at(&source, &link)
}

/// Absolute path of the managed bridge symlink
/// (`~/.codesense/bin/codesense-bridge`). This is the single source of truth
/// for the install-location-stable command path written into per-repo agent
/// MCP configs (SW-CP-8): `ensure_bridge_symlink` keeps the symlink pointing
/// at the current bundle across rebuilds/app-updates, so configs referencing
/// this path never need rewriting. Returns `None` when the home dir cannot be
/// resolved (`HOME`/`USERPROFILE` unset).
pub fn bridge_symlink_path() -> Option<PathBuf> {
    codesense_bin_dir().map(|d| d.join("codesense-bridge"))
}

/// Resolve a bundled `externalBin` sidecar (`codesense-bridge`,
/// `codesense-session-server`) by its base name: production path first
/// (alongside the main binary, where the bundler drops the target-triple
/// suffix), then two dev-mode fallbacks. Returns the first existing
/// candidate.
///
/// Review iter-1 (L-4): dropped the unused `_app: &AppHandle` parameter
/// — the resolver works purely from `current_exe()` + the build-time
/// `TARGET_TRIPLE`. Carrying the AppHandle was speculative future-proofing.
///
/// Review iter-1 (M-3): `canonicalize` the exe path before extracting
/// its parent so distro packaging shapes that symlink `/usr/local/bin/codesense
/// → /opt/.../codesense` still resolve to the bundle dir on the prod
/// probe.
pub(crate) fn resolve_sidecar_source(base_name: &str) -> Result<PathBuf, String> {
    let exe_raw = std::env::current_exe().map_err(|e| format!("current_exe: {}", e))?;
    // Both the canonical exe path (resolves symlinks → bundle dir on
    // distro-packaged installs) AND the raw exe path (matters when
    // Tauri ships the sidecar via firmlink-side path the canonical
    // form collapses through).
    //
    // Review iter-2 (M-3): iter-1 only used the canonical form. If
    // canonicalize collapses through a firmlink and Tauri ships the
    // sidecar at the firmlink-side path, the canonical probe misses.
    // Probe BOTH parents per candidate — first hit wins.
    let exe_canonical = std::fs::canonicalize(&exe_raw).ok();
    let exe_dirs: Vec<PathBuf> = {
        let mut v = Vec::with_capacity(2);
        if let Some(p) = exe_raw.parent() {
            v.push(p.to_path_buf());
        }
        if let Some(canon) = exe_canonical.as_ref() {
            if let Some(p) = canon.parent() {
                let pb = p.to_path_buf();
                if !v.contains(&pb) {
                    v.push(pb);
                }
            }
        }
        if v.is_empty() {
            return Err("current_exe has no parent (canonical or raw)".to_string());
        }
        v
    };

    let binary_name = if cfg!(windows) {
        format!("{base_name}.exe")
    } else {
        base_name.to_string()
    };
    let dev_name = format!("{}-{}", binary_name, TARGET_TRIPLE);

    // Per-exe-dir probes: production (alongside exe) then dev-cli
    // (with target-triple suffix).
    for exe_dir in &exe_dirs {
        let prod = exe_dir.join(&binary_name);
        if prod.is_file() {
            return Ok(prod);
        }
        let dev_exe_local = exe_dir.join(&dev_name);
        if dev_exe_local.is_file() {
            return Ok(dev_exe_local);
        }
    }

    // Dev (cargo run): the sidecar build artifact lives at
    // `src-tauri/binaries/<name>-<target-triple>`. From the dev exe path
    // `target/debug/codesense` (or `target-ci/.../codesense`), walk up two
    // to reach `src-tauri/` and then into `binaries/`. Use the
    // canonical form when available (more reliable across firmlinks);
    // raw is the fallback.
    for exe_dir in &exe_dirs {
        if let Some(src_tauri) = exe_dir.parent().and_then(|p| p.parent()) {
            let dev_binaries = src_tauri.join("binaries").join(&dev_name);
            if dev_binaries.is_file() {
                return Ok(dev_binaries);
            }
        }
    }

    Err(format!(
        "{} sidecar not found (probed {} parent dir(s) for `{}` and `{}`, \
         plus src-tauri/binaries/)",
        base_name,
        exe_dirs.len(),
        binary_name,
        dev_name,
    ))
}

/// Idempotent symlink creation at `link` pointing to `source`. Pure helper
/// taking explicit paths so it is unit-testable without an AppHandle.
fn ensure_symlink_at(source: &Path, link: &Path) -> Result<(), String> {
    if !source.is_file() {
        return Err(format!(
            "bridge source does not exist: {}",
            source.display()
        ));
    }

    // Idempotent: if symlink already points exactly to source, no-op.
    // `read_link` returns the literal symlink target (no resolution), so a
    // matching value means no rewrite needed.
    if let Ok(existing) = std::fs::read_link(link) {
        if existing == source {
            return Ok(());
        }
    }

    // Otherwise the link is wrong, broken, missing, or shadowed by a
    // regular file. Remove whatever's there (if anything) and create
    // afresh. `link.exists()` follows symlinks (false for broken ones),
    // so check `is_symlink()` separately to cover the broken-symlink case.
    //
    // Review iter-1 (M-2): if the link path is a real directory (not a
    // symlink to a directory), `remove_file` fails with EISDIR/EPERM
    // and we never self-heal. Branch on `is_dir() && !is_symlink()`.
    //
    // Review iter-2 (M-2): downgraded from `remove_dir_all` (recursive)
    // to `remove_dir` (non-recursive). The recursive form would blast
    // through any user content that ended up at the bridge path —
    // rare but possible (manual `mkdir`, restore-from-backup, plugin
    // install conflict). Non-recursive form refuses non-empty dirs,
    // surfacing a clear error so the user can clean up manually
    // before re-launching codesense.
    if link.is_symlink() {
        std::fs::remove_file(link)
            .map_err(|e| format!("remove existing {}: {}", link.display(), e))?;
    } else if link.is_dir() {
        std::fs::remove_dir(link).map_err(|e| {
            format!(
                "remove unexpected directory at {}: {} — refusing to recursively delete; \
                 remove the directory manually and re-launch codesense",
                link.display(),
                e
            )
        })?;
    } else if link.exists() {
        std::fs::remove_file(link)
            .map_err(|e| format!("remove existing {}: {}", link.display(), e))?;
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, link)
            .map_err(|e| format!("symlink {} -> {}: {}", link.display(), source.display(), e))?;
    }
    #[cfg(not(unix))]
    {
        // Non-Unix fallback: copy the binary in place (symlinks need privilege
        // on Windows).
        std::fs::copy(source, link)
            .map_err(|e| format!("copy {} -> {}: {}", source.display(), link.display(), e))?;
    }

    info!(
        target: "deps",
        "codesense-bridge symlink ensured: {} -> {}",
        link.display(),
        source.display()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::launch_check::LaunchWarningCode;

    #[test]
    fn listed_dependency_probes_and_launches_ignore_previously_warmed_versions() {
        let _home = crate::routines::test_support::TempHome::new();
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("codex");
        let calls = root.path().join("calls");
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\necho probe >> '{}'\nprintf 'codex-cli 0.162.1\\n'\n",
                calls.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let poison = "codex-cli 0.158.0";
        codesense_pty_core::agent_cli::version_cache::seed_for_test(&binary, poison).unwrap();
        assert_eq!(parse_agent_version(&binary).as_deref(), Some("0.162.1"));
        assert_eq!(
            codesense_pty_core::agent_cli::version_cache::peek_for_test(&binary).as_deref(),
            Some(poison)
        );
        let next = codesense_pty_core::activity::prepare_direct(
            AgentCli::Codex,
            binary.as_os_str(),
            &[],
            "/test/activity-helper",
            None,
            None,
        );
        assert!(next.arguments.is_some());
        assert!(next.unqualified_reason.is_none());
        assert_eq!(std::fs::read_to_string(calls).unwrap().lines().count(), 2);
    }

    #[test]
    fn dependency_probe_and_status_lists_only_available_clis() {
        let entries = AgentCli::ALL
            .into_iter()
            .map(|cli| (descriptor(cli).binary.clone(), AgentEntry::default()))
            .collect();
        let status = manager_with(entries).status();
        assert_eq!(status.agents.len(), AgentCli::ALL.len());
        for cli in AgentCli::ALL {
            assert!(status.agents.contains_key(&descriptor(cli).binary), "{cli}");
        }
    }

    #[test]
    fn bridge_symlink_path_resolves_under_managed_bin() {
        // SW-CP-8: the writer's command path must be
        // `{home}/.codesense/bin/codesense-bridge`. Guard env mutation with
        // the crate-wide lock so it can't race other HOME-sensitive tests.
        let _g = crate::content_engine::test_support::env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp.path());

        let resolved = bridge_symlink_path().expect("home is set");
        assert_eq!(
            resolved,
            tmp.path()
                .join(".codesense")
                .join("bin")
                .join("codesense-bridge")
        );

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn test_extract_semver_token() {
        assert_eq!(
            extract_semver_token("git version 2.51.0"),
            Some("2.51.0".to_string())
        );
        assert_eq!(
            extract_semver_token("git version 2.44.0\n"),
            Some("2.44.0".to_string())
        );
        assert_eq!(
            extract_semver_token("git version 2.39.3 (Apple Git-146)"),
            Some("2.39.3".to_string())
        );
        // SW-OB-12: claude --version prints "X.Y.Z (Claude Code)"
        assert_eq!(
            extract_semver_token("2.1.94 (Claude Code)"),
            Some("2.1.94".to_string())
        );
        assert_eq!(extract_semver_token(""), None);
        assert_eq!(extract_semver_token("not git"), None);
    }

    #[test]
    fn test_check_git_version_minimum() {
        // Valid versions >= 2.44
        assert!(check_git_version_minimum("2.44.0"));
        assert!(check_git_version_minimum("2.44.1"));
        assert!(check_git_version_minimum("2.51.0"));
        assert!(check_git_version_minimum("3.0.0"));

        // Too old
        assert!(!check_git_version_minimum("2.43.0"));
        assert!(!check_git_version_minimum("2.43.9"));
        assert!(!check_git_version_minimum("2.39.3"));
        assert!(!check_git_version_minimum("1.9.0"));

        // Edge cases
        assert!(!check_git_version_minimum(""));
        assert!(!check_git_version_minimum("abc"));
        assert!(check_git_version_minimum("2.44")); // no patch version
    }

    #[test]
    fn test_default_agent_version_floor() {
        // SW-OB-12 AC-2 / SW-AN-44: the bundled descriptor's floor is 2.0, applied per
        // basename via `floor_for_basename` (the default basename → default descriptor).
        let basename = crate::agents::builtin::preferred_binary();
        let floor = DependencyManager::floor_for_basename(basename);

        assert!(check_version_minimum("2.0.0", floor));
        assert!(check_version_minimum("2.1.94", floor));
        assert!(check_version_minimum("2.10.0", floor));
        assert!(check_version_minimum("3.0.0", floor));

        // Below floor
        assert!(!check_version_minimum("1.9.0", floor));
        assert!(!check_version_minimum("1.0.0", floor));
        assert!(!check_version_minimum("0.5.0", floor));

        // Edge cases
        assert!(!check_version_minimum("", floor));
        assert!(!check_version_minimum("abc", floor));
        assert!(check_version_minimum("2.0", floor)); // no patch version

        // Codex is held to its own descriptor's floor, not admitted at any version.
        let codex = DependencyManager::floor_for_basename("codex");
        assert_eq!(
            codex,
            codesense_pty_core::agent_cli::descriptor(
                codesense_pty_core::agent_cli::AgentCli::Codex
            )
            .min_version
        );
        assert!(codex > (0, 0));
        assert!(!check_version_minimum("0.1.0", codex));
        assert!(check_version_minimum(
            &extract_semver_token("codex-cli 9.0.0").unwrap(),
            codex
        ));

        // An unknown basename admits any parseable version ((0,0) sentinel), but
        // still requires a 2-part parse — never a bypass.
        let open = DependencyManager::floor_for_basename("zzagent");
        assert!(check_version_minimum("0.1.0", open));
        assert!(!check_version_minimum("abc", open));
    }

    // SW-OB-12: claude is detected from the user's install (login-shell PATH first,
    // known locations as the fallback), not resolved from a managed ~/.codesense/bin.
    // SW-AN-2: the fallback locations are templated from the descriptor's basename.
    #[test]
    fn test_known_agent_locations_includes_expected_for_claude() {
        let locs = known_agent_locations(crate::agents::builtin::preferred_binary());
        assert!(locs.contains(&PathBuf::from("/opt/homebrew/bin/claude")));
        assert!(locs.contains(&PathBuf::from("/usr/local/bin/claude")));
        // A home-relative candidate is present when HOME is set (it is in tests).
        assert!(
            locs.iter().any(|p| p.ends_with(".local/bin/claude")),
            "expected a ~/.local/bin/claude candidate, got {locs:?}"
        );
    }

    // SW-AN-2 AC-3: a second (fixture) agent's binary is discoverable purely from
    // descriptor DATA — the same location-templating serves any basename, no core edit.
    #[test]
    fn test_known_agent_locations_templates_any_basename() {
        let locs = known_agent_locations("zzagent");
        assert!(locs.contains(&PathBuf::from("/opt/homebrew/bin/zzagent")));
        assert!(locs.contains(&PathBuf::from("/usr/local/bin/zzagent")));
        assert!(
            locs.iter().any(|p| p.ends_with(".bun/bin/zzagent")),
            "expected a ~/.bun/bin/zzagent candidate, got {locs:?}"
        );
        // The probe path also accepts a non-claude basename (safe charset).
        assert!(resolve_binary_via_login_shell("zzagent; rm -rf /", AGENT_PROBE_TIMEOUT).is_none());
    }

    #[test]
    fn test_login_shell_probe_rejects_unsafe_name() {
        // Guards against shell injection — anything outside [A-Za-z0-9_-] is refused.
        assert!(resolve_binary_via_login_shell("claude; rm -rf /", AGENT_PROBE_TIMEOUT).is_none());
        assert!(resolve_binary_via_login_shell("", AGENT_PROBE_TIMEOUT).is_none());
        assert!(resolve_binary_via_login_shell("a b", AGENT_PROBE_TIMEOUT).is_none());
    }

    #[test]
    fn test_extract_sentinel_value_strips_prompt_noise() {
        // Interactive shells can prepend/append prompt noise; the sentinels
        // isolate the payload regardless.
        let raw = "\x1b[0m% CS_PATH_BEGIN>>/usr/bin:/bin<<CS_PATH_END\nsome-prompt$ ";
        assert_eq!(
            extract_sentinel_value(raw, PATH_SENTINEL_BEGIN, PATH_SENTINEL_END).as_deref(),
            Some("/usr/bin:/bin")
        );
    }

    #[test]
    fn test_extract_sentinel_value_none_when_absent_or_empty() {
        // Missing sentinels → None.
        assert!(
            extract_sentinel_value("no markers here", PATH_SENTINEL_BEGIN, PATH_SENTINEL_END)
                .is_none()
        );
        // Present but empty value → None (a shell that exports an empty PATH is
        // useless; caller falls back to the process PATH).
        let empty = format!("{PATH_SENTINEL_BEGIN}{PATH_SENTINEL_END}");
        assert!(extract_sentinel_value(&empty, PATH_SENTINEL_BEGIN, PATH_SENTINEL_END).is_none());
    }

    #[test]
    fn login_path_probe_script_hides_sentinels() {
        // Anti-echo (RF-21 review): the emitted COMMAND must not contain the
        // literal sentinels, or an rc that echoes it to stdout would make
        // extract_sentinel_value match the echo (→ bogus `%s` PATH) rather than
        // the real $PATH. The markers must form only in printf's OUTPUT.
        let script = login_path_probe_script();
        assert!(
            !script.contains(PATH_SENTINEL_BEGIN),
            "probe command leaks the begin sentinel: {script}"
        );
        assert!(
            !script.contains(PATH_SENTINEL_END),
            "probe command leaks the end sentinel: {script}"
        );
        // And the fragments must still compose to the sentinels the extractor
        // expects — guards against a fragment edit drifting from the consts.
        assert_eq!(format!("{}{}", "CS_PATH_", "BEGIN>>"), PATH_SENTINEL_BEGIN);
        assert_eq!(
            format!("{}{}{}", "<<", "CS_PATH_", "END"),
            PATH_SENTINEL_END
        );
    }

    #[test]
    fn test_resolve_login_shell_path_returns_system_path() {
        // Live probe: any login shell on macOS/Linux CI exports a PATH that
        // contains the system bin dir. Tolerant of a probe miss (None) so the
        // test can't flake on a container without zsh/bash — but if it DOES
        // resolve, the value must be a real PATH free of sentinel leakage.
        if let Some(path) = resolve_login_shell_path(Duration::from_secs(5)) {
            assert!(
                path.split(':').any(|p| p == "/usr/bin" || p == "/bin"),
                "expected a system bin dir in resolved PATH, got: {path:?}"
            );
            assert!(!path.contains(PATH_SENTINEL_BEGIN) && !path.contains(PATH_SENTINEL_END));
        }
    }

    #[test]
    fn a_backgrounded_child_holding_stdout_cannot_outlast_the_capture_bound() {
        // An rc file that backgrounds a process leaves it holding the probe's
        // stdout after the shell exits; the capture still returns within the
        // bound, with what the shell printed.
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf held; sleep 20 & exit 0"]);
        let started = Instant::now();
        let stdout = capture_stdout_within(command, Duration::from_secs(1));
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(3), "capture took {elapsed:?}");
        assert_eq!(stdout.as_deref(), Some("held"));
    }

    #[test]
    fn a_shell_that_never_exits_is_killed_at_the_capture_bound() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf partial; exec sleep 20"]);
        let started = Instant::now();
        assert_eq!(capture_stdout_within(command, Duration::from_secs(1)), None);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn test_login_shell_probe_missing_binary_returns_none() {
        // A name that resolves to nothing must return None (not hang) within the timeout.
        assert!(resolve_binary_via_login_shell(
            "codesense-definitely-not-a-real-binary-xyz",
            Duration::from_secs(3)
        )
        .is_none());
    }

    // SW-OB-12 (review r1/r2): the probe prefers $SHELL when it's an EXISTING
    // shell that accepts `-lic` (zsh/bash), else falls back to zsh.
    #[test]
    fn test_login_probe_shell_selection() {
        let prev = std::env::var("SHELL").ok();

        // Existing, whitelisted shells (present on macOS) are honored.
        std::env::set_var("SHELL", "/bin/bash");
        assert_eq!(login_probe_shell(), "/bin/bash");
        std::env::set_var("SHELL", "/bin/zsh");
        assert_eq!(login_probe_shell(), "/bin/zsh");

        // Stale $SHELL path (whitelisted basename but doesn't exist) → zsh fallback.
        std::env::set_var("SHELL", "/opt/nonexistent-prefix/bin/zsh");
        assert_eq!(login_probe_shell(), "zsh");

        // Unsupported shell basename (can't be trusted with `-lic`) → zsh fallback.
        std::env::set_var("SHELL", "/usr/bin/fish");
        assert_eq!(login_probe_shell(), "zsh");

        std::env::remove_var("SHELL");
        assert_eq!(login_probe_shell(), "zsh");

        match prev {
            Some(v) => std::env::set_var("SHELL", v),
            None => std::env::remove_var("SHELL"),
        }
    }

    // SW-OB-12 (review r1): re_resolve_git refreshes git without disturbing the
    // detected agent state (no login-shell probe from the watcher).
    #[test]
    fn test_re_resolve_git_preserves_agent() {
        let basename = crate::agents::builtin::preferred_binary();
        let dm = DependencyManager::resolve();
        // Establish a fully-resolved agent baseline (full path incl. probe).
        dm.re_resolve();
        let agent_after_full = dm.agent(basename);
        let resolving_after_full = dm.agent_resolving(basename);

        dm.re_resolve_git();
        assert_eq!(
            dm.agent(basename),
            agent_after_full,
            "re_resolve_git must not change the agent path"
        );
        assert_eq!(
            dm.agent_resolving(basename),
            resolving_after_full,
            "re_resolve_git must not touch the agent resolving flag"
        );
    }

    #[test]
    fn opencode_v2_banner_normalizes_the_v_prefixed_token() {
        assert_eq!(
            extract_semver_token("opencode v2.1.4\n"),
            Some("2.1.4".into())
        );
        assert_eq!(
            extract_semver_token("opencode v3.0.1"),
            Some("3.0.1".into())
        );
    }

    #[test]
    fn status_says_whether_each_cli_version_is_qualified_and_what_range_is() {
        let dm = DependencyManager::cached_for_test(&[
            ("opencode", Some("2.1.4")),
            ("claude", Some("2.0.1")),
            ("codex", None),
            ("unknown-cli", Some("9.9.9")),
        ]);
        let status = dm.status();
        let opencode = &status.agents["opencode"];
        assert_eq!(opencode.qualified, Some(true));
        assert_eq!(
            opencode.supported.as_deref(),
            Some("OpenCode 2.1.4 or a newer 2.x release")
        );
        // Below Claude's qualified minimum.
        assert_eq!(status.agents["claude"].qualified, Some(false));
        // A version that cannot be read, or a CLI without a descriptor, says nothing.
        assert_eq!(status.agents["codex"].qualified, None);
        assert_eq!(status.agents["unknown-cli"].qualified, None);
        assert_eq!(status.agents["unknown-cli"].supported, None);
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["agents"]["opencode"]["qualified"], true);
    }

    #[test]
    fn the_launch_warning_for_a_cached_version_names_it_and_the_range() {
        let dm = DependencyManager::cached_for_test(&[
            ("opencode", Some("3.0.1")),
            ("claude", Some("2.1.150")),
            ("codex", None),
        ]);
        let warning = dm
            .version_warning(AgentCli::OpenCode)
            .expect("out of range warns");
        assert_eq!(warning.code, LaunchWarningCode::CliVersionUnqualified);
        assert!(warning.message.contains("3.0.1"), "{}", warning.message);
        assert!(
            warning
                .message
                .contains("OpenCode 2.1.4 or a newer 2.x release"),
            "{}",
            warning.message
        );
        assert!(dm.version_warning(AgentCli::Claude).is_none());
        assert!(dm.version_warning(AgentCli::Codex).is_none());
    }

    #[test]
    fn test_dependency_status_serialization() {
        let mut agents = BTreeMap::new();
        agents.insert(
            "claude".to_string(),
            AgentEntry {
                path: None,
                resolving: true,
                ..AgentEntry::default()
            },
        );
        let dm = DependencyManager {
            inner: RwLock::new(DependencyManagerInner {
                git_path: None,
                git_version: None,
                agents,
            }),
            stale_resolve: Mutex::new(()),
            known_locations: Box::new(no_known_locations),
        };
        let status = dm.status();
        assert!(!status.git.available);
        let claude = status.agents.get("claude").expect("claude entry present");
        assert!(!claude.available);
        assert!(status.git.path.is_none());
        assert!(claude.path.is_none());
        // SW-OB-12: resolving flag flows through to the serialized status.
        assert!(claude.resolving);

        // Verify it serializes without panic, and the per-agent map is an object
        // keyed by basename (`agents.claude`), not a flat `claude` field.
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"available\":false"));
        assert!(json.contains("\"resolving\":true"));
        assert!(json.contains("\"agents\""));
        assert!(json.contains("\"claude\""));
    }

    /// SW-AN-44 AC-1/AC-2: a synthetic two-basename registry resolves both; `agent()`
    /// returns each path; an unknown basename → `None`; an absent entry is treated
    /// as *not resolving* (the bounded-resolving safeguard). Verifies the mechanism
    /// over a synthetic second basename (no second descriptor shipped — that is SW-AN-45).
    /// A built-in CLI is probed once across team switches, whether or not it was found; a
    /// basename already in the map (the default, or one the startup probe seeded) is never
    /// claimed.
    #[test]
    fn a_cli_is_claimed_for_its_first_probe_only() {
        let mut agents = BTreeMap::new();
        agents.insert("claude".to_string(), AgentEntry::default());
        let dm = DependencyManager {
            inner: RwLock::new(DependencyManagerInner {
                git_path: None,
                git_version: None,
                agents,
            }),
            stale_resolve: Mutex::new(()),
            known_locations: Box::new(no_known_locations),
        };
        assert!(!dm.claim_first_probe("claude"));
        assert!(dm.claim_first_probe("codex"));
        assert!(!dm.claim_first_probe("codex"), "a second switch skips it");
        assert_eq!(dm.agent("codex"), None);
        assert!(
            !dm.agent_resolving("codex"),
            "a claim never gates the splash"
        );
    }

    #[test]
    fn test_agent_map_resolves_per_basename() {
        let mut agents = BTreeMap::new();
        agents.insert(
            "claude".to_string(),
            AgentEntry {
                path: Some(PathBuf::from("/opt/homebrew/bin/claude")),
                resolving: false,
                ..AgentEntry::default()
            },
        );
        agents.insert(
            "codex".to_string(),
            AgentEntry {
                path: Some(PathBuf::from("/usr/local/bin/codex")),
                resolving: true,
                ..AgentEntry::default()
            },
        );
        let dm = DependencyManager {
            inner: RwLock::new(DependencyManagerInner {
                git_path: None,
                git_version: None,
                agents,
            }),
            stale_resolve: Mutex::new(()),
            known_locations: Box::new(no_known_locations),
        };

        assert_eq!(
            dm.agent("claude"),
            Some(PathBuf::from("/opt/homebrew/bin/claude"))
        );
        assert_eq!(
            dm.agent("codex"),
            Some(PathBuf::from("/usr/local/bin/codex"))
        );
        assert_eq!(dm.agent("gemini"), None, "unknown basename → None");

        assert!(!dm.agent_resolving("claude"));
        assert!(dm.agent_resolving("codex"));
        // Bounded-resolving safeguard: an absent basename is never "resolving".
        assert!(!dm.agent_resolving("gemini"));

        // Status projects the whole map, keyed by basename.
        let status = dm.status();
        assert_eq!(status.agents.len(), 2);
        assert!(status.agents["claude"].available);
        assert!(status.agents["codex"].resolving);
    }

    // --- Task 6: Git installation unit tests ---

    // Tests for `ensure_symlink_at` — the path-based
    // helper that backs `ensure_bridge_symlink`. AppHandle-free so each
    // test exercises the idempotency contract directly.

    /// Returns a fresh unique tempdir for one test. Pid + nanosecond
    /// component avoids races between sibling tests sharing the same
    /// process under `cargo test`.
    #[cfg(test)]
    fn fresh_test_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir()
            .join("codesense_deps_test")
            .join(format!("{}_{}_{}", label, std::process::id(), nanos));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `ensure_symlink_at` creates the symlink when the link path does
    /// not exist.
    #[cfg(unix)]
    #[test]
    fn ensure_symlink_at_creates_when_missing() {
        let dir = fresh_test_dir("bridge_symlink_create");
        let source = dir.join("codesense-bridge-source");
        std::fs::write(&source, b"#!/bin/sh\nexit 0\n").unwrap();
        let link = dir.join("codesense-bridge");

        assert!(
            !link.exists() && !link.is_symlink(),
            "precondition: link absent"
        );

        ensure_symlink_at(&source, &link).expect("first-call ensure");

        assert!(link.is_symlink(), "link should be created as a symlink");
        assert_eq!(std::fs::read_link(&link).unwrap(), source);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `ensure_symlink_at` is idempotent when the existing symlink
    /// already points to the right source — should NOT thrash via
    /// remove + recreate (verified by inode preservation).
    #[cfg(unix)]
    #[test]
    fn ensure_symlink_at_is_idempotent_when_correct() {
        use std::os::unix::fs::MetadataExt;

        let dir = fresh_test_dir("bridge_symlink_idempotent");
        let source = dir.join("codesense-bridge-source");
        std::fs::write(&source, b"#!/bin/sh\nexit 0\n").unwrap();
        let link = dir.join("codesense-bridge");
        std::os::unix::fs::symlink(&source, &link).unwrap();

        // Capture identity of the original symlink.
        let original_ino = std::fs::symlink_metadata(&link).unwrap().ino();

        ensure_symlink_at(&source, &link).expect("idempotent ensure");

        // Same symlink, same inode → no remove+recreate.
        let after_ino = std::fs::symlink_metadata(&link).unwrap().ino();
        assert_eq!(
            original_ino, after_ino,
            "idempotent call must not replace the existing symlink"
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), source);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `ensure_symlink_at` replaces a stale symlink (pointing to a
    /// different target).
    #[cfg(unix)]
    #[test]
    fn ensure_symlink_at_replaces_stale_target() {
        let dir = fresh_test_dir("bridge_symlink_stale");
        let old_source = dir.join("old-bridge");
        let new_source = dir.join("new-bridge");
        std::fs::write(&old_source, b"old").unwrap();
        std::fs::write(&new_source, b"new").unwrap();
        let link = dir.join("codesense-bridge");
        std::os::unix::fs::symlink(&old_source, &link).unwrap();

        ensure_symlink_at(&new_source, &link).expect("replace stale");

        assert_eq!(std::fs::read_link(&link).unwrap(), new_source);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `ensure_symlink_at` replaces a regular file sitting at the link
    /// path. Covers the "user manually copied a binary there" recovery
    /// case.
    #[cfg(unix)]
    #[test]
    fn ensure_symlink_at_replaces_regular_file() {
        let dir = fresh_test_dir("bridge_symlink_regfile");
        let source = dir.join("real-bridge");
        std::fs::write(&source, b"real").unwrap();
        let link = dir.join("codesense-bridge");
        // A regular file (not a symlink) at the link path.
        std::fs::write(&link, b"stray content").unwrap();
        assert!(!link.is_symlink());

        ensure_symlink_at(&source, &link).expect("replace regular file");

        assert!(
            link.is_symlink(),
            "regular file must be replaced by a symlink"
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), source);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `ensure_symlink_at` errors when the source path does not exist —
    /// caller must surface this rather than create a broken symlink.
    #[test]
    fn ensure_symlink_at_errors_when_source_missing() {
        let dir = fresh_test_dir("bridge_symlink_no_source");
        let source = dir.join("nonexistent-bridge");
        let link = dir.join("codesense-bridge");

        let err = ensure_symlink_at(&source, &link).expect_err("must error on missing source");
        assert!(
            err.contains("source does not exist"),
            "error should mention missing source, got: {}",
            err
        );
        assert!(
            !link.exists() && !link.is_symlink(),
            "no symlink should be created when source is missing"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 6.2 Test symlink creation and replacement
    #[cfg(unix)]
    #[test]
    fn test_symlink_creation_and_replacement() {
        let dir = std::env::temp_dir()
            .join("codesense_deps_test")
            .join(format!("symlink_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let target = dir.join("real_git");
        std::fs::write(&target, "git binary").unwrap();

        let link = dir.join("git_link");

        // Create initial symlink
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(link.is_symlink());
        assert_eq!(std::fs::read_link(&link).unwrap(), target);

        // Replace symlink with new target
        let new_target = dir.join("new_git");
        std::fs::write(&new_target, "new git binary").unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&new_target, &link).unwrap();
        assert_eq!(std::fs::read_link(&link).unwrap(), new_target);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 6.7b Test DependencyManager re_resolve updates paths
    #[test]
    fn test_dependency_manager_re_resolve() {
        use std::os::unix::fs::PermissionsExt;

        // SW-OB-12: resolve() seeds the default agent as resolving (background
        // probe pending); re_resolve() runs the FULL resolution synchronously
        // (login-shell PATH probe, then known locations as the fallback). So resolve() and re_resolve() may
        // legitimately differ. The invariants that must hold:
        //   - git resolution is unchanged across re_resolve (same logic),
        //   - re_resolve clears the resolving flag,
        //   - re_resolve is idempotent with itself.
        let basename = crate::agents::builtin::preferred_binary();
        let dm = DependencyManager::resolve();
        let git_before = dm.git();

        dm.re_resolve();
        assert_eq!(dm.git(), git_before, "git resolution must be stable");
        assert!(
            !dm.agent_resolving(basename),
            "re_resolve must clear resolving"
        );

        let git_after = dm.git();
        dm.re_resolve();
        assert_eq!(dm.git(), git_after, "re_resolve must be idempotent for git");

        // The agent winner comes from real version checks, which can time out under load,
        // so two calls on this machine may legitimately pick different installs. Idempotence
        // is checked on a fixed candidate: the same inputs must pick the same winner.
        let dir = tempfile::TempDir::new().unwrap();
        let candidate = dir.path().join(basename);
        std::fs::write(&candidate, "").unwrap();
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755)).unwrap();
        let resolve = || {
            dm.resolve_agent_with_login_path_with(
                basename,
                None,
                vec![candidate.clone()],
                |_| Some("99.0.0".into()),
                || {},
            )
        };
        let first = resolve();
        assert_eq!(first, Some(candidate.clone()));
        assert_eq!(
            resolve(),
            first,
            "re_resolve must be idempotent for the agent path"
        );
    }

    // --- Story 4.1: Claude dist migration tests ---

    /// 6.1 DependencyManager::resolve() returns consistent results and re_resolve is safe
    #[test]
    fn test_dependency_manager_resolve_consistency() {
        // resolve() reads the real filesystem — on a dev machine with deps installed,
        // both git and claude should be available if manifest versions match.
        // On CI without deps, both should be None.
        let dm = DependencyManager::resolve();
        let status = dm.status();

        // If git is available, its version should be populated
        if status.git.available {
            assert!(status.git.version.is_some(), "git available but no version");
            assert!(status.git.path.is_some(), "git available but no path");
        }

        // If the bundled agent is available, its path should be populated.
        if let Some(claude) = status.agents.get("claude") {
            if claude.available {
                assert!(claude.path.is_some(), "agent available but no path");
            }
        }
    }

    /// 5.6 No GCS structs remain in source
    #[test]
    fn test_no_gcs_structs() {
        // Split the source at #[cfg(test)] — only check the non-test portion
        let source = include_str!("deps.rs");
        let non_test = source.split("#[cfg(test)]").next().unwrap_or("");
        assert!(
            !non_test.contains("GcsManifest"),
            "GCS manifest struct found in production code"
        );
        assert!(
            !non_test.contains("GcsPlatformInfo"),
            "GCS platform struct found in production code"
        );
        assert!(
            !non_test.contains("GCS_BASE"),
            "GCS base URL found in production code"
        );
    }

    // ===== Dependency file watcher tests =====

    #[test]
    fn test_is_dep_relevant_file_git() {
        assert!(is_dep_relevant_file(Path::new(
            "/home/user/.codesense/bin/git"
        )));
    }

    #[test]
    fn test_is_dep_relevant_file_claude() {
        assert!(is_dep_relevant_file(Path::new(
            "/home/user/.codesense/bin/claude"
        )));
    }

    #[test]
    fn test_is_dep_relevant_file_manifest() {
        assert!(is_dep_relevant_file(Path::new(
            "/home/user/.codesense/manifest.json"
        )));
    }

    #[test]
    fn test_is_dep_relevant_file_rejects_unrelated() {
        assert!(!is_dep_relevant_file(Path::new(
            "/home/user/.codesense/bin/node"
        )));
        assert!(!is_dep_relevant_file(Path::new(
            "/home/user/.codesense/config.json"
        )));
        assert!(!is_dep_relevant_file(Path::new(
            "/home/user/.codesense/bin/"
        )));
        assert!(!is_dep_relevant_file(Path::new("/tmp/random.txt")));
    }

    #[test]
    fn test_is_dep_relevant_file_empty_path() {
        assert!(!is_dep_relevant_file(Path::new("")));
    }

    /// known_git_locations includes the standard system + Homebrew bin paths the
    /// fast-path stats before the login-shell probe.
    #[test]
    fn test_known_git_locations() {
        let locs = known_git_locations();
        assert!(locs.contains(&PathBuf::from("/usr/bin/git")));
        assert!(locs.contains(&PathBuf::from("/opt/homebrew/bin/git")));
        assert!(locs.contains(&PathBuf::from("/usr/local/bin/git")));
    }

    /// resolve_versioned_binary (the shared git+claude detection engine): a
    /// fast-path hit meeting the floor returns (path, version) without probing;
    /// and a below-floor fast-path hit retains its version as the guidance
    /// fallback when the login-shell probe finds nothing (SW-OB-14 review fix).
    #[test]
    fn test_resolve_versioned_binary_fast_path_and_below_floor_retention() {
        let dir = tempfile::TempDir::new().unwrap();
        let bin = dir.path().join("git");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();

        // Fast-path hit: parsed version meets the floor → returned with its path,
        // no login-shell probe needed (closures avoid spawning a real binary).
        let (path, ver) = resolve_versioned_binary(
            "git",
            vec![bin.clone()],
            Duration::from_secs(1),
            |_p| Some("2.50.0".to_string()),
            |_v| true,
        );
        assert_eq!(path.as_deref(), Some(bin.as_path()));
        assert_eq!(ver.as_deref(), Some("2.50.0"));

        // Below-floor fast-path hit + a probe name that resolves to nothing → path
        // is None but the below-floor version survives for "found vX, too old".
        let (path, ver) = resolve_versioned_binary(
            "codesense-nonexistent-binary-xyz",
            vec![bin.clone()],
            Duration::from_secs(1),
            |_p| Some("2.39.3".to_string()),
            |_v| false,
        );
        assert!(path.is_none());
        assert_eq!(ver.as_deref(), Some("2.39.3"));
    }

    /// classify_xcode_select maps `xcode-select --install`'s exit+stderr to the
    /// right status: success → triggered, non-zero "already installed" →
    /// already_installed, other non-zero → error with a message (SW-OB-14).
    #[test]
    fn test_classify_xcode_select() {
        assert_eq!(classify_xcode_select(true, "").status, "triggered");
        assert_eq!(
            classify_xcode_select(
                false,
                "xcode-select: error: command line tools are already installed"
            )
            .status,
            "already_installed"
        );
        let err = classify_xcode_select(false, "some other failure");
        assert_eq!(err.status, "error");
        assert_eq!(err.message.as_deref(), Some("some other failure"));
        // Empty stderr still yields a non-empty error message.
        let empty = classify_xcode_select(false, "   ");
        assert_eq!(empty.status, "error");
        assert!(empty.message.is_some_and(|m| !m.is_empty()));
    }

    // ===== Agent binary resolution: login-shell PATH first =====

    /// An executable stub `name` in `dir` (never run — versions are injected).
    fn exec_stub(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A version closure keyed on the stub's directory name.
    fn versions(map: &[(&str, &str)]) -> impl Fn(&Path) -> Option<String> {
        let map: Vec<(String, String)> = map
            .iter()
            .map(|(dir, v)| (dir.to_string(), v.to_string()))
            .collect();
        move |path: &Path| {
            let dir = path.parent()?.file_name()?.to_str()?.to_string();
            map.iter().find(|(d, _)| *d == dir).map(|(_, v)| v.clone())
        }
    }

    fn path_of(dirs: &[&Path]) -> String {
        dirs.iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(":")
    }

    fn manager_with(agents: BTreeMap<String, AgentEntry>) -> DependencyManager {
        DependencyManager {
            inner: RwLock::new(DependencyManagerInner {
                git_path: None,
                git_version: None,
                agents,
            }),
            stale_resolve: Mutex::new(()),
            known_locations: Box::new(no_known_locations),
        }
    }

    #[test]
    fn login_path_candidates_walk_path_in_order() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b, c) = (
            tmp.path().join("a"),
            tmp.path().join("b"),
            tmp.path().join("c"),
        );
        let in_b = exec_stub(&b, "claude");
        let in_a = exec_stub(&a, "claude");
        // Non-executable in c: skipped.
        std::fs::create_dir_all(&c).unwrap();
        std::fs::write(c.join("claude"), b"x").unwrap();
        // A directory named like the binary: skipped.
        let d = tmp.path().join("d");
        std::fs::create_dir_all(d.join("claude")).unwrap();

        let path = format!(
            "{}::relative/bin:{}:{}:{}",
            b.display(),
            c.display(),
            d.display(),
            a.display()
        );
        assert_eq!(login_path_candidates(&path, "claude"), vec![in_b, in_a]);
        assert!(login_path_candidates("", "claude").is_empty());
        assert!(login_path_candidates(&path, "codex").is_empty());
    }

    #[test]
    fn login_path_candidates_dedupe_a_symlink_and_its_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let real = exec_stub(&tmp.path().join("real"), "claude");
        let links = tmp.path().join("links");
        std::fs::create_dir_all(&links).unwrap();
        std::os::unix::fs::symlink(&real, links.join("claude")).unwrap();

        // The first spelling on PATH is kept.
        let path = path_of(&[&links, &tmp.path().join("real")]);
        assert_eq!(
            login_path_candidates(&path, "claude"),
            vec![links.join("claude")]
        );
        // The same directory listed twice is one install.
        let real_dir = tmp.path().join("real");
        let twice = path_of(&[&real_dir, &real_dir]);
        assert_eq!(login_path_candidates(&twice, "claude"), vec![real]);
    }

    /// PATH order beats the fixed list, and other installs are shadowed, for one
    /// basename + floor.
    fn path_beats_fixed_list(basename: &str, floor: (u32, u32), new: &str, old: &str) {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        let in_a = exec_stub(&a, basename);
        let in_b = exec_stub(&b, basename);
        let off_path = exec_stub(&tmp.path().join("c"), basename);
        let path = path_of(&[&a, &b]);

        let resolution = resolve_agent_binary(
            basename,
            Some(&path),
            // The fixed order disagrees with PATH.
            vec![in_b.clone(), in_a.clone()],
            versions(&[("a", new), ("b", old), ("c", new)]),
            |v| check_version_minimum(v, floor),
        );
        assert_eq!(resolution.path.as_deref(), Some(in_a.as_path()));
        assert_eq!(resolution.version.as_deref(), Some(new));
        assert_eq!(resolution.source, Some(AgentSource::LoginPath));
        assert_eq!(resolution.shadowed_candidates, vec![(in_b.clone(), true)]);

        // A known hit outside PATH is shadowed with on_path = false.
        let resolution = resolve_agent_binary(
            basename,
            Some(&path),
            vec![in_b.clone(), off_path.clone(), in_a.clone()],
            versions(&[("a", new), ("b", old), ("c", new)]),
            |v| check_version_minimum(v, floor),
        );
        assert_eq!(resolution.path.as_deref(), Some(in_a.as_path()));
        assert_eq!(
            resolution.shadowed_candidates,
            vec![(in_b, true), (off_path, false)]
        );
    }

    #[test]
    fn login_path_beats_the_fixed_list_claude() {
        path_beats_fixed_list("claude", (2, 0), "2.1.296", "2.1.141");
    }

    #[test]
    fn login_path_beats_the_fixed_list_codex() {
        path_beats_fixed_list("codex", (0, 140), "0.150.0", "0.141.2");
    }

    #[test]
    fn a_symlink_to_the_winner_is_not_shadowed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let in_a = exec_stub(&a, "claude");
        let links = tmp.path().join("links");
        std::fs::create_dir_all(&links).unwrap();
        std::os::unix::fs::symlink(&in_a, links.join("claude")).unwrap();
        let resolution = resolve_agent_binary(
            "claude",
            Some(&path_of(&[&a])),
            vec![links.join("claude")],
            |_| Some("2.1.296".to_string()),
            |v| check_version_minimum(v, (2, 0)),
        );
        assert_eq!(resolution.path.as_deref(), Some(in_a.as_path()));
        assert!(resolution.shadowed_candidates.is_empty());
    }

    /// The fixed list is the fallback when PATH yields nothing usable, for one
    /// basename + floor.
    fn fixed_list_fallback(basename: &str, floor: (u32, u32), good: &str, low: &str) {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b, k1, k2) = (
            tmp.path().join("a"),
            tmp.path().join("b"),
            tmp.path().join("k1"),
            tmp.path().join("k2"),
        );
        let in_a = exec_stub(&a, basename);
        let in_b = exec_stub(&b, basename);
        let known_low = exec_stub(&k1, basename);
        let known_good = exec_stub(&k2, basename);
        let missing = tmp.path().join("nowhere").join(basename);
        let known = vec![missing, known_low.clone(), known_good.clone()];
        let meets = |v: &str| check_version_minimum(v, floor);

        // Probe failed → first known meeting the floor.
        let r = resolve_agent_binary(
            basename,
            None,
            known.clone(),
            versions(&[("k1", low), ("k2", good)]),
            meets,
        );
        assert_eq!(r.path.as_deref(), Some(known_good.as_path()));
        assert_eq!(r.source, Some(AgentSource::KnownLocation));
        assert_eq!(r.version.as_deref(), Some(good));
        assert_eq!(r.shadowed_candidates, vec![(known_low.clone(), false)]);

        // PATH probed but nothing on it meets the floor → known fallback.
        let r = resolve_agent_binary(
            basename,
            Some(&path_of(&[&a])),
            known.clone(),
            versions(&[("a", low), ("k1", low), ("k2", good)]),
            meets,
        );
        assert_eq!(r.path.as_deref(), Some(known_good.as_path()));
        assert_eq!(r.source, Some(AgentSource::KnownLocation));
        assert_eq!(
            r.shadowed_candidates,
            vec![(in_a.clone(), true), (known_low.clone(), false)]
        );

        // A below-floor PATH winner falls through to the next PATH candidate.
        let r = resolve_agent_binary(
            basename,
            Some(&path_of(&[&a, &b])),
            known.clone(),
            versions(&[("a", low), ("b", good), ("k2", good)]),
            meets,
        );
        assert_eq!(r.path.as_deref(), Some(in_b.as_path()));
        assert_eq!(r.source, Some(AgentSource::LoginPath));

        // Nothing meets the floor anywhere → no path, the first below-floor version kept.
        let r = resolve_agent_binary(
            basename,
            Some(&path_of(&[&a])),
            vec![known_low],
            |_| Some(low.to_string()),
            meets,
        );
        assert!(r.path.is_none());
        assert!(r.source.is_none());
        assert_eq!(r.version.as_deref(), Some(low));
    }

    #[test]
    fn fixed_list_is_the_fallback_claude() {
        fixed_list_fallback("claude", (2, 0), "2.1.296", "1.9.0");
    }

    #[test]
    fn fixed_list_is_the_fallback_codex() {
        fixed_list_fallback("codex", (0, 140), "0.150.0", "0.139.0");
    }

    /// Phase 1 stores the winner and clears `resolving` before the
    /// shadowed installs are version-checked; phase 2 surfaces them on the status.
    #[test]
    fn resolution_stores_the_winner_then_the_shadowed_installs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        let in_a = exec_stub(&a, "claude");
        let in_b = exec_stub(&b, "claude");
        let mut agents = BTreeMap::new();
        agents.insert(
            "claude".to_string(),
            AgentEntry {
                resolving: true,
                ..AgentEntry::default()
            },
        );
        let dm = manager_with(agents);
        let parse = versions(&[("a", "2.1.296"), ("b", "2.1.141")]);
        let resolution =
            resolve_agent_binary("claude", Some(&path_of(&[&a, &b])), vec![], &parse, |v| {
                check_version_minimum(v, (2, 0))
            });
        let pending = dm.store_winner("claude", resolution);
        assert_eq!(pending.winner.as_deref(), Some(in_a.as_path()));

        let status = dm.status();
        let claude = &status.agents["claude"];
        assert!(!claude.resolving, "phase 1 clears resolving");
        assert_eq!(claude.path.as_deref(), Some(in_a.to_str().unwrap()));
        assert_eq!(claude.version.as_deref(), Some("2.1.296"));
        assert_eq!(claude.source, Some(AgentSource::LoginPath));
        assert!(claude.shadowed.is_empty(), "shadowed waits for phase 2");

        dm.fill_shadowed("claude", &pending, &parse);
        let status = dm.status();
        assert_eq!(
            status.agents["claude"].shadowed,
            vec![ShadowedInstall {
                path: in_b.display().to_string(),
                version: Some("2.1.141".to_string()),
                on_path: true,
            }]
        );
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["agents"]["claude"]["source"], "loginPath");
        assert_eq!(json["agents"]["claude"]["shadowed"][0]["onPath"], true);
        assert_eq!(
            json["agents"]["claude"]["shadowed"][0]["version"],
            "2.1.141"
        );
    }

    /// A resolution of `claude` against a PATH of just `dir` (every stub 2.1.296).
    fn stub_resolution(dir: &Path) -> AgentResolution {
        resolve_agent_binary(
            "claude",
            Some(&path_of(&[dir])),
            vec![],
            |_| Some("2.1.296".to_string()),
            |v| check_version_minimum(v, (2, 0)),
        )
    }

    fn resolve_stub_into(dm: &DependencyManager, dir: &Path) -> PendingShadowed {
        dm.store_winner("claude", stub_resolution(dir))
    }

    #[test]
    fn replacement_during_version_probe_is_not_trusted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = exec_stub(tmp.path(), "claude");
        let dm = manager_with(BTreeMap::new());
        let resolution = resolve_agent_binary(
            "claude",
            Some(tmp.path().to_str().unwrap()),
            vec![],
            |_| {
                std::fs::write(&path, "a different binary with a different size").unwrap();
                Some("2.1.296".into())
            },
            |_| true,
        );
        dm.store_winner("claude", resolution);
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Stale);
    }

    #[test]
    fn replacement_before_storage_keeps_the_probed_fingerprint() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = exec_stub(tmp.path(), "claude");
        let resolution = stub_resolution(tmp.path());
        std::fs::write(&path, "replacement after the probe").unwrap();
        let dm = manager_with(BTreeMap::new());
        dm.store_winner("claude", resolution);
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Stale);
    }

    #[test]
    fn launch_binary_is_ready_while_the_file_is_unchanged() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let in_a = exec_stub(&a, "claude");
        let dm = manager_with(BTreeMap::new());
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Unresolved);
        resolve_stub_into(&dm, &a);
        assert_eq!(
            dm.launch_binary("claude"),
            LaunchBinaryLookup::Ready(ResolvedAgentBinary {
                path: in_a,
                version: Some("2.1.296".to_string()),
            })
        );
        // A re-resolve on a ready entry does nothing.
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (lookup, re_resolved) = dm.re_resolve_stale_with("claude", || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            AgentResolution::default()
        });
        assert!(matches!(lookup, LaunchBinaryLookup::Ready(_)));
        assert!(re_resolved.is_none());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn launch_binary_is_unresolved_while_resolving_or_without_a_path() {
        let mut agents = BTreeMap::new();
        agents.insert(
            "claude".to_string(),
            AgentEntry {
                resolving: true,
                path: Some(PathBuf::from("/nonexistent/claude")),
                ..AgentEntry::default()
            },
        );
        agents.insert("codex".to_string(), AgentEntry::default());
        let dm = manager_with(agents);
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Unresolved);
        assert_eq!(dm.launch_binary("codex"), LaunchBinaryLookup::Unresolved);
        assert_eq!(dm.launch_binary("gemini"), LaunchBinaryLookup::Unresolved);
    }

    /// A deleted cached binary is stale and re-resolves once to the next install;
    /// with none left the miss is stored (unavailable, marked lost) and the launch
    /// is refused.
    #[test]
    fn a_deleted_binary_is_stale_and_re_resolves_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        let in_a = exec_stub(&a, "claude");
        let in_b = exec_stub(&b, "claude");
        let dm = manager_with(BTreeMap::new());
        let both = path_of(&[&a, &b]);
        let resolve_both = || {
            resolve_agent_binary(
                "claude",
                Some(&both),
                vec![],
                |_| Some("2.1.296".to_string()),
                |v| check_version_minimum(v, (2, 0)),
            )
        };
        dm.store_winner("claude", resolve_both());
        std::fs::remove_file(&in_a).unwrap();
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Stale);

        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (lookup, re_resolved) = dm.re_resolve_stale_with("claude", || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            resolve_both()
        });
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            re_resolved,
            Some(PendingShadowed {
                winner: Some(in_b.clone()),
                candidates: vec![],
            })
        );
        assert_eq!(
            lookup,
            LaunchBinaryLookup::Ready(ResolvedAgentBinary {
                path: in_b.clone(),
                version: Some("2.1.296".to_string()),
            })
        );

        // Nothing left → the miss is stored and the launch refused.
        std::fs::remove_file(&in_b).unwrap();
        let (lookup, re_resolved) = dm.re_resolve_stale_with("claude", resolve_both);
        assert_eq!(lookup, LaunchBinaryLookup::Unresolved);
        assert_eq!(re_resolved, Some(PendingShadowed::default()));
        assert_eq!(dm.agent("claude"), None);
        assert!(!dm.status().agents["claude"].available);
    }

    /// SW-FX-141: a binary a launch found gone and then found back at a known install
    /// location, still inside the retry cooldown, re-resolves for that launch rather than
    /// being refused with the cooldown's "no CLI" message.
    #[test]
    fn a_lost_binary_back_at_a_known_location_inside_the_cooldown_re_resolves() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let in_a = exec_stub(&a, "claude");
        let mut dm = manager_with(BTreeMap::new());
        resolve_stub_into(&dm, &a);
        std::fs::remove_file(&in_a).unwrap();
        let (lookup, _) = dm.re_resolve_stale_with("claude", || stub_resolution(&a));
        assert_eq!(lookup, LaunchBinaryLookup::Unresolved);

        // Reinstalled at a known location after the miss, still inside the cooldown.
        std::thread::sleep(Duration::from_millis(20));
        let back = exec_stub(&a, "claude");
        dm.known_locations = Box::new(move |_: &str| vec![back.clone()]);
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Stale);
        let (lookup, _) = dm.re_resolve_stale_with("claude", || stub_resolution(&a));
        assert!(matches!(lookup, LaunchBinaryLookup::Ready(_)), "{lookup:?}");
        assert!(dm.agent("claude").is_some());
    }

    /// SW-FX-141: a known-location binary written before the miss (its re-resolve found
    /// nothing usable) keeps the cooldown; it does not re-probe on every launch.
    #[test]
    fn a_known_location_binary_unchanged_since_the_miss_keeps_the_cooldown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let mut dm = manager_with(BTreeMap::new());
        let unusable = exec_stub(&a, "claude");
        std::thread::sleep(Duration::from_millis(20));
        dm.known_locations = Box::new(move |_: &str| vec![unusable.clone()]);
        // The re-resolve found nothing usable: the miss is stamped now, after the file.
        let _ = dm.store_resolution("claude", AgentResolution::default(), Some(Instant::now()));
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Unresolved);
    }

    /// Back-date `basename`'s `lost_at` past the retry cooldown. `false` when the
    /// clock can't be wound back that far (the caller skips the check).
    fn lose_long_ago(dm: &DependencyManager, basename: &str) -> bool {
        let Some(past) = Instant::now().checked_sub(LOST_RETRY_AFTER + Duration::from_secs(1))
        else {
            return false;
        };
        let mut inner = dm.inner.write().unwrap();
        inner.agents.get_mut(basename).unwrap().lost_at = Some(past);
        true
    }

    fn lost_at(dm: &DependencyManager, basename: &str) -> Option<Instant> {
        dm.inner.read().unwrap().agents[basename].lost_at
    }

    /// An install that vanishes mid-upgrade never disables the CLI for good: the
    /// launch in that window stores the miss (unavailable, marked lost) and is
    /// refused; once the cooldown passed a later launch re-resolves to the new binary.
    #[test]
    fn a_launch_while_the_only_binary_is_missing_marks_it_lost_and_retries_after_the_cooldown() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let in_a = exec_stub(&a, "claude");
        let dm = manager_with(BTreeMap::new());
        resolve_stub_into(&dm, &a);
        std::fs::remove_file(&in_a).unwrap();

        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (lookup, re_resolved) = dm.re_resolve_stale_with("claude", || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            stub_resolution(&a)
        });
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(lookup, LaunchBinaryLookup::Unresolved);
        assert_eq!(re_resolved, Some(PendingShadowed::default()));
        let status = dm.status();
        let claude = &status.agents["claude"];
        assert!(!claude.available);
        assert!(claude.path.is_none());
        assert!(!claude.resolving);
        assert!(lost_at(&dm, "claude").is_some());

        // Inside the cooldown a launch is refused without another probe.
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Unresolved);
        let (lookup, re_resolved) = dm.re_resolve_stale_with("claude", || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            stub_resolution(&a)
        });
        assert_eq!(lookup, LaunchBinaryLookup::Unresolved);
        assert!(re_resolved.is_none());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        if !lose_long_ago(&dm, "claude") {
            return;
        }
        // Past the cooldown it is due again.
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Stale);

        // The upgrader wrote the new binary; the due launch re-resolves to it.
        exec_stub(&a, "claude");
        let (lookup, re_resolved) = dm.re_resolve_stale_with("claude", || stub_resolution(&a));
        assert!(re_resolved.is_some());
        assert_eq!(
            lookup,
            LaunchBinaryLookup::Ready(ResolvedAgentBinary {
                path: in_a,
                version: Some("2.1.296".to_string()),
            })
        );
        assert!(
            lost_at(&dm, "claude").is_none(),
            "a found winner clears the mark"
        );
    }

    /// An entry with no miss recorded stays unresolved. (Every stored miss records
    /// one; see the startup-miss test below.)
    #[test]
    fn a_never_found_cli_is_not_retried_by_launches() {
        let mut agents = BTreeMap::new();
        agents.insert("codex".to_string(), AgentEntry::default());
        let dm = manager_with(agents);
        assert_eq!(dm.launch_binary("codex"), LaunchBinaryLookup::Unresolved);
    }

    /// #2237: a startup resolution that found nothing is retried by a later launch
    /// once the cooldown passed, rather than refusing every launch until a restart.
    #[test]
    fn a_startup_miss_is_retried_after_the_cooldown_not_refused_until_restart() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let dm = manager_with(BTreeMap::new());
        // The startup probe found no usable install.
        resolve_stub_into(&dm, &a);
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Unresolved);
        assert!(
            lost_at(&dm, "claude").is_some(),
            "a miss starts the retry clock"
        );
        if !lose_long_ago(&dm, "claude") {
            return;
        }
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Stale);
        // The CLI is there now: the due launch re-resolves to it.
        let in_a = exec_stub(&a, "claude");
        let (lookup, _) = dm.re_resolve_stale_with("claude", || stub_resolution(&a));
        assert_eq!(
            lookup,
            LaunchBinaryLookup::Ready(ResolvedAgentBinary {
                path: in_a,
                version: Some("2.1.296".to_string()),
            })
        );
        assert!(
            lost_at(&dm, "claude").is_none(),
            "a found winner clears the mark"
        );
    }

    /// An in-place upgrade that keeps the path and resets the mtime (npm writes
    /// package files with a fixed mtime) is still seen as changed.
    #[test]
    fn an_in_place_rewrite_with_the_same_mtime_is_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let in_a = exec_stub(&a, "claude");
        let dm = manager_with(BTreeMap::new());
        resolve_stub_into(&dm, &a);
        let old_mtime = std::fs::metadata(&in_a).unwrap().modified().unwrap();

        std::fs::write(&in_a, b"#!/bin/sh\necho upgraded to a longer script\n").unwrap();
        let file = std::fs::File::options().write(true).open(&in_a).unwrap();
        file.set_modified(old_mtime).unwrap();
        drop(file);
        assert_eq!(
            std::fs::metadata(&in_a).unwrap().modified().unwrap(),
            old_mtime
        );
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Stale);
    }

    #[test]
    fn a_same_length_rewrite_with_restored_mtime_is_stale() {
        use std::os::unix::fs::MetadataExt;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = exec_stub(tmp.path(), "claude");
        let dm = manager_with(BTreeMap::new());
        resolve_stub_into(&dm, tmp.path());
        let old = std::fs::metadata(&path).unwrap();
        // Separate metadata timestamps even on filesystems with coarse clocks.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(&path, vec![b'x'; old.len() as usize]).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(old.modified().unwrap()).unwrap();
        let new = std::fs::metadata(&path).unwrap();
        assert_eq!(new.len(), old.len());
        assert_eq!(new.ino(), old.ino());
        assert_eq!(new.modified().unwrap(), old.modified().unwrap());
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Stale);
    }

    /// A binary changed in place (mtime or symlink target) is stale.
    #[test]
    fn a_binary_changed_in_place_is_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let in_a = exec_stub(&a, "claude");
        let dm = manager_with(BTreeMap::new());
        resolve_stub_into(&dm, &a);
        let file = std::fs::File::options().write(true).open(&in_a).unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(120))
            .unwrap();
        drop(file);
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Stale);

        // Symlink retargeted.
        let v1 = exec_stub(&tmp.path().join("v1"), "claude");
        let v2 = exec_stub(&tmp.path().join("v2"), "claude");
        let links = tmp.path().join("links");
        std::fs::create_dir_all(&links).unwrap();
        let link = links.join("claude");
        std::os::unix::fs::symlink(&v1, &link).unwrap();
        resolve_stub_into(&dm, &links);
        assert!(matches!(
            dm.launch_binary("claude"),
            LaunchBinaryLookup::Ready(_)
        ));
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&v2, &link).unwrap();
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Stale);

        // Executable bit removed.
        resolve_stub_into(&dm, &links);
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&v2, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Stale);
    }

    /// A fingerprintable binary cached with no recorded fingerprint is stale, so a
    /// launch re-resolves it once (which records one) rather than trusting it.
    #[test]
    fn a_binary_without_a_fingerprint_is_stale() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let in_a = exec_stub(&a, "claude");
        let mut agents = BTreeMap::new();
        agents.insert(
            "claude".to_string(),
            AgentEntry {
                path: Some(in_a.clone()),
                version: Some("2.1.296".to_string()),
                ..AgentEntry::default()
            },
        );
        let dm = manager_with(agents);
        assert_eq!(dm.launch_binary("claude"), LaunchBinaryLookup::Stale);

        // The one re-resolve records a fingerprint; later launches are ready.
        let (lookup, _) = dm.re_resolve_stale_with("claude", || stub_resolution(&a));
        assert_eq!(
            lookup,
            LaunchBinaryLookup::Ready(ResolvedAgentBinary {
                path: in_a,
                version: Some("2.1.296".to_string()),
            })
        );
    }

    /// Phase 2 never writes an old shadowed list onto a newer winner: a resolution
    /// that replaced the winner phase 1 stored keeps its own (empty) list.
    #[test]
    fn shadowed_installs_of_a_replaced_winner_are_dropped() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b, c) = (
            tmp.path().join("a"),
            tmp.path().join("b"),
            tmp.path().join("c"),
        );
        exec_stub(&a, "claude");
        let in_b = exec_stub(&b, "claude");
        exec_stub(&c, "claude");
        let dm = manager_with(BTreeMap::new());
        let resolve = |path: &str| {
            resolve_agent_binary(
                "claude",
                Some(path),
                vec![],
                |_| Some("2.1.296".to_string()),
                |v| check_version_minimum(v, (2, 0)),
            )
        };
        // Winner A (C shadowed), then a newer resolution stores winner B alone.
        let pending_a = dm.store_winner("claude", resolve(&path_of(&[&a, &c])));
        assert_eq!(pending_a.candidates.len(), 1);
        dm.store_winner("claude", resolve(&path_of(&[&b])));

        dm.fill_shadowed("claude", &pending_a, |_| Some("2.1.141".to_string()));
        let status = dm.status();
        let claude = &status.agents["claude"];
        assert_eq!(claude.path.as_deref(), Some(in_b.to_str().unwrap()));
        assert!(claude.shadowed.is_empty(), "A's list must not land on B");
    }

    /// The composed two-phase resolution releases the splash (winner stored,
    /// `resolving` cleared, nothing shadowed yet) before the shadowed installs are
    /// version-checked, then fills them in.
    #[test]
    fn the_winner_is_published_before_shadowed_installs_are_checked() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b, k) = (
            tmp.path().join("a"),
            tmp.path().join("b"),
            tmp.path().join("k"),
        );
        let in_a = exec_stub(&a, "claude");
        let in_b = exec_stub(&b, "claude");
        let off_path = exec_stub(&k, "claude");
        let mut agents = BTreeMap::new();
        agents.insert(
            "claude".to_string(),
            AgentEntry {
                resolving: true,
                ..AgentEntry::default()
            },
        );
        let dm = manager_with(agents);
        let after_winner_ran = std::cell::Cell::new(false);
        let resolved = dm.resolve_agent_with_login_path_with(
            "claude",
            Some(&path_of(&[&a, &b])),
            vec![off_path.clone()],
            versions(&[("a", "2.1.296"), ("b", "2.1.141"), ("k", "2.0.5")]),
            || {
                let status = dm.status();
                let claude = &status.agents["claude"];
                assert!(!claude.resolving, "the splash is released at the winner");
                assert_eq!(claude.path.as_deref(), Some(in_a.to_str().unwrap()));
                assert!(claude.shadowed.is_empty(), "shadowed is not checked yet");
                after_winner_ran.set(true);
            },
        );
        assert!(after_winner_ran.get());
        assert_eq!(resolved.as_deref(), Some(in_a.as_path()));
        assert_eq!(
            dm.status().agents["claude"].shadowed,
            vec![
                ShadowedInstall {
                    path: in_b.display().to_string(),
                    version: Some("2.1.141".to_string()),
                    on_path: true,
                },
                ShadowedInstall {
                    path: off_path.display().to_string(),
                    version: Some("2.0.5".to_string()),
                    on_path: false,
                },
            ]
        );
    }

    /// A binary both on PATH and in the known list is version-checked once: the
    /// fallback skips what the PATH pass already checked.
    #[test]
    fn a_binary_on_path_and_in_known_is_version_checked_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let in_a = exec_stub(&a, "claude");
        let links = tmp.path().join("links");
        std::fs::create_dir_all(&links).unwrap();
        std::os::unix::fs::symlink(&in_a, links.join("claude")).unwrap();
        let checks = std::cell::Cell::new(0u32);
        let resolution = resolve_agent_binary(
            "claude",
            Some(&path_of(&[&a])),
            // The same file, by its own path and through a symlink.
            vec![in_a.clone(), links.join("claude")],
            |_| {
                checks.set(checks.get() + 1);
                Some("1.9.0".to_string())
            },
            |v| check_version_minimum(v, (2, 0)),
        );
        assert_eq!(checks.get(), 1, "one --version for one binary");
        assert!(resolution.path.is_none());
        assert_eq!(resolution.version.as_deref(), Some("1.9.0"));
        assert!(resolution.shadowed_candidates.contains(&(in_a, true)));
    }

    /// A second PATH dir holding a symlink to the winner is the same install, so it
    /// is never reported as shadowed.
    #[test]
    fn a_path_symlink_to_the_winner_is_not_shadowed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let a = tmp.path().join("a");
        let in_a = exec_stub(&a, "claude");
        let links = tmp.path().join("links");
        std::fs::create_dir_all(&links).unwrap();
        std::os::unix::fs::symlink(&in_a, links.join("claude")).unwrap();
        let resolution = resolve_agent_binary(
            "claude",
            Some(&path_of(&[&a, &links])),
            vec![],
            |_| Some("2.1.296".to_string()),
            |v| check_version_minimum(v, (2, 0)),
        );
        assert_eq!(resolution.path.as_deref(), Some(in_a.as_path()));
        assert_eq!(resolution.source, Some(AgentSource::LoginPath));
        assert!(resolution.shadowed_candidates.is_empty());
    }

    /// Two concurrent launches on a stale entry run one re-resolve.
    #[test]
    fn concurrent_stale_launches_re_resolve_once() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
        let in_a = exec_stub(&a, "claude");
        exec_stub(&b, "claude");
        let dm = manager_with(BTreeMap::new());
        resolve_stub_into(&dm, &a);
        std::fs::remove_file(&in_a).unwrap();

        let calls = std::sync::atomic::AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    barrier.wait();
                    let (lookup, _) = dm.re_resolve_stale_with("claude", || {
                        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        stub_resolution(&b)
                    });
                    assert!(matches!(lookup, LaunchBinaryLookup::Ready(_)));
                });
            }
        });
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    // SW-OB-12: Claude auth status tests removed with the auth machinery —
    // codesense no longer checks or manages Claude login.
}
