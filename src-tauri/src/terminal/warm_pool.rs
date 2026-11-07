// SPDX-License-Identifier: MIT
//! PT-8: warm terminal pool.
//!
//! A pool of N pre-spawned, fully-booted `zsh` shells sitting at their first
//! prompt in `$HOME`. When the user opens a new terminal, the
//! warm spawn (`terminal_open_tab`) pops a `Ready` member, renames it to the
//! frontend's session id, resizes it, and `cd`s it to the target — so the
//! terminal opens at ~0ms perceived latency instead of paying the shell's full
//! cold-start cost (D6, AC-3).
//!
//! This module owns the pool *registry* and lifecycle (fill, readiness, TTL
//! recycle, dead-member self-heal, drain) plus internal telemetry. The actual
//! shells are normal `codesense-pty-core` registry sessions flagged `is_pool_member`;
//! the pool just tracks which session ids are members and their warm/ready
//! state.
//!
//! ## Lock ordering (D9)
//! The pool registry mutex is the **innermost** lock. Nothing here re-enters
//! `PtyState`'s `sessions`/`shared`/`stream_index` locks while holding it — the
//! registry lock is taken, the decision is made, the lock is released, and only
//! then are PTY-side operations (spawn/kill) performed. The `Osc133Bus`
//! readiness listener (which fires synchronously from a member's batcher
//! side-tap) only ever takes this innermost lock.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Pool members idle longer than this are recycled (killed + respawned) — stale
/// env / shell process state shouldn't be handed to a user (AC-8).
const TTL_MS: u64 = 60 * 60 * 1000; // 1 hour

/// How often the TTL reaper sweeps for stale members. Exposed so the setup site
/// (which owns the app handle needed to re-enter the managed pool each sweep) can
/// drive the reaper without this module reaching for Tauri.
pub(crate) const TTL_SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Cap on retained refill-latency samples (bounds memory + keeps the p50/p95
/// sort cheap over a long-running session). Keeps the most recent samples.
const REFILL_SAMPLES: usize = 256;

/// Default geometry for a freshly-spawned member. The viewport's real size is
/// applied at handoff via the session's resize (AC-13), so this is just the boot
/// size — kept modest so the boot prompt wraps sanely if ever shown.
const MEMBER_COLS: u16 = 80;
const MEMBER_ROWS: u16 = 24;

/// Namespace prefix for pool-member session ids — a warm member boots with this
/// placeholder id and only rebinds to the frontend's real `shell-<uuid>` at
/// handoff (SW-EW-22). Anything still carrying it is a pool member, never a real
/// tab. Exposed so callers that must reject the placeholder (e.g. the
/// terminal-status endpoint's liveness guard, SLW-FX-17) share one definition.
pub const WARM_POOL_PREFIX: &str = "__warm_pool_";

/// Unique suffix for pool-member session ids: this process's pid, then a
/// counter. Namespaced so a member id can never collide with a frontend-minted
/// terminal id, and carrying the pid so it never collides with another app's
/// member on the session server every app on the Mac shares.
fn next_member_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "{WARM_POOL_PREFIX}{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// SLW-FX-17: true when `session_id` is a warm-pool placeholder id (never a real
/// terminal tab). Load-bearing for the status endpoint: a warm member is a *live*
/// PTY session, so a plain liveness check would accept its placeholder id — this
/// is the defense-in-depth catch that rejects it regardless of liveness.
pub fn is_warm_pool_id(session_id: &str) -> bool {
    session_id.starts_with(WARM_POOL_PREFIX)
}

/// A monotonic millisecond clock, injectable so TTL recycling is unit-testable
/// without real time (AC-8).
type ClockFn = std::sync::Arc<dyn Fn() -> u64 + Send + Sync>;

fn real_clock() -> ClockFn {
    let base = Instant::now();
    std::sync::Arc::new(move || base.elapsed().as_millis() as u64)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MemberState {
    /// Spawned, booting; not eligible for handoff until its first prompt-start.
    Warming,
    /// Booted (first OSC 133 prompt-start fired) — eligible for handoff.
    Ready,
}

struct Member {
    state: MemberState,
    /// Clock ms when the member began warming (for refill-latency telemetry).
    warming_at_ms: u64,
    /// Clock ms when the member became `Ready` (for TTL idle age).
    ready_at_ms: u64,
    /// The Settings shell override this member was spawned with (`None` =
    /// detected shell). A member whose shell no longer matches the pool's is
    /// stale and is never handed out.
    shell: Option<String>,
    /// Whether its spawn has reported back ([`WarmPoolState::on_member_spawned`]).
    spawned: bool,
}

#[derive(Default)]
struct Telemetry {
    /// Handoffs served from the pool (warm).
    warm_hits: u64,
    /// Handoffs that fell through to a cold direct spawn.
    direct: u64,
    /// Total members spawned since launch.
    warm_spawns: u64,
    /// Warm→Ready latencies (ms), for p50/p95.
    refill_ms: Vec<u64>,
}

#[derive(Default)]
struct PoolInner {
    /// Target member count. 0 = pool disabled (AC-7).
    target: usize,
    /// Spawn params, set by `reconfigure` from frontend settings (D8).
    preset: Option<String>,
    shell_integration: Option<bool>,
    /// The Settings shell override new members spawn with (`None` = detect),
    /// set by [`WarmPoolState::set_shell`].
    shell: Option<String>,
    members: HashMap<String, Member>,
    telemetry: Telemetry,
    /// Members in a row that exited before their first prompt.
    early_exits: u32,
    /// Clock ms until which refills are skipped, after [`EARLY_EXIT_LIMIT`] early exits.
    paused_until_ms: u64,
    /// Members that exited before their spawn reported back: the exit already
    /// reclaimed them, so the spawn's report kills nothing more.
    exited_unspawned: HashSet<String>,
}

/// Members in a row that may exit on boot (within [`BOOT_WINDOW_MS`], before their
/// first prompt) before the pool stops refilling: a shell that dies on boot (a broken
/// rc file, a missing shell) would otherwise be respawned without end.
const EARLY_EXIT_LIMIT: u32 = 3;

/// How soon after its spawn a member's exit counts as dying on boot. A member waiting
/// longer (a host with no shell integration never reports a prompt) that ends, as at a
/// terminal-server restart, did not die on boot.
const BOOT_WINDOW_MS: u64 = 60 * 1000;

/// How long refills stay paused after [`EARLY_EXIT_LIMIT`] early exits. The TTL sweep
/// tries again at its first run past it, so the pool stays short for 5 to 10 minutes.
const EARLY_EXIT_PAUSE_MS: u64 = 5 * 60 * 1000;

/// Internal telemetry snapshot (AC-14) — surfaced via `tracing`/getters only,
/// never in user-facing Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TelemetrySnapshot {
    pub warm_hits: u64,
    pub direct: u64,
    pub warm_spawns: u64,
    pub refill_p50_ms: u64,
    pub refill_p95_ms: u64,
}

/// The per-refill parameters a warm member boots with. The pool owns these
/// decisions (geometry, preset, integration); the host adapter turns them into a
/// real PTY spawn. A plain data struct with no Tauri coupling, so `warm_pool`
/// stays off the command layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberSpawnParams {
    pub home: String,
    pub cols: u16,
    pub rows: u16,
    pub preset: Option<String>,
    pub shell_integration: Option<bool>,
}

/// The PTY-side operations the warm pool needs, expressed with no Tauri coupling.
/// The pool decides *which* members to spawn/kill under its innermost registry
/// lock (D9); the host performs the spawn/kill off-lock and owns the async
/// orchestration + pool re-entry, which require the app handle. OC-10's adapter
/// (`commands::pty_host::AppWarmPoolHost`) wraps the `AppHandle`; tests use a recording
/// fake. This is the seam that keeps `warm_pool` Tauri-free.
pub trait WarmPoolHost: Send + Sync {
    /// Spawn a member off the request path, then reconcile it into the pool via
    /// [`WarmPoolState::on_member_spawned`] (success) or
    /// [`WarmPoolState::remove_member`] (failure).
    fn spawn_member(&self, id: String, params: MemberSpawnParams);
    /// Kill and reclaim a member session synchronously (drain / TTL recycle).
    fn kill_member(&self, id: &str);
    /// A member's PTY exited: reclaim its dead session and refill the pool.
    fn reclaim_exited_and_refill(&self, id: String);
}

/// Managed Tauri state for the warm pool. Created empty at `setup`; filled when
/// the frontend calls `reconfigure_warm_pool` after settings hydrate (D8/AC-1).
pub struct WarmPoolState {
    inner: Mutex<PoolInner>,
    clock: ClockFn,
    /// Set once at `setup`. The port through which the pool spawns/kills shells;
    /// before it is set (e.g. in unit tests) those operations are skipped.
    host: OnceLock<Arc<dyn WarmPoolHost>>,
    /// Lock-free fast-path gate for the OSC 133 listener: `mark_ready` fires for
    /// every prompt-start of *every* terminal, so when the pool is disabled
    /// (size 0 or integration off) we skip taking the registry lock entirely.
    /// Set by `reconfigure` from `pool_fill_enabled`. (L2)
    enabled: std::sync::atomic::AtomicBool,
}

impl Default for WarmPoolState {
    fn default() -> Self {
        Self {
            inner: Mutex::new(PoolInner::default()),
            clock: real_clock(),
            host: OnceLock::new(),
            enabled: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl WarmPoolState {
    /// Store the spawn/kill host and fill to target. Called once at `setup`. The
    /// TTL reaper is started by the setup site (it owns the app handle needed to
    /// re-enter the managed pool state each sweep — this module stays Tauri-free).
    pub fn init(&self, host: Arc<dyn WarmPoolHost>) {
        let _ = self.host.set(host);
        // Close the launch race: if the frontend called `reconfigure` before the
        // host was available (so its `refill` no-op'd), the target is already
        // stored — fill it now. No-op when nothing has been configured.
        self.refill();
    }

    /// Apply new target size + spawn params, draining stale members and
    /// refilling to the new target. Idempotent re-invocation is how the
    /// frontend launches the pool (AC-1) and reacts to setting changes — size 0
    /// drains and schedules nothing (AC-7), a preset/integration change drains
    /// the now-stale members and refills with the new params (AC-15).
    pub fn reconfigure(
        &self,
        size: usize,
        preset: Option<String>,
        shell_integration: Option<bool>,
    ) {
        self.enabled.store(
            pool_fill_enabled(size, shell_integration),
            std::sync::atomic::Ordering::Relaxed,
        );
        {
            let mut inner = self.lock();
            inner.target = size;
            inner.preset = preset;
            inner.shell_integration = shell_integration;
            // New settings get a fresh start.
            inner.early_exits = 0;
            inner.paused_until_ms = 0;
        }
        // Drain the now-stale members (sync, cheap), then refill to target.
        self.drain();
        self.refill();
    }

    /// Pop a `Ready` member's session id for handoff, or `None` if the pool has
    /// no ready member (cold launch / refill lag / disabled). The caller renames
    /// the live session and then calls [`Self::refill`] to restore the target.
    ///
    /// SW-LN-B31: a member (Ready or still warming) spawned with a shell other
    /// than the current one is stale: it is retired, never handed out, and the
    /// pool refills with the current shell. Callers run this off the UI thread
    /// (a warm spawn wraps it in `await_blocking`), so the kills are too.
    pub fn acquire_ready(&self) -> Option<String> {
        let (id, stale) = {
            let mut inner = self.lock();
            let want = inner.shell.clone();
            let stale: Vec<String> = inner
                .members
                .iter()
                .filter(|(_, m)| m.shell != want)
                .map(|(id, _)| id.clone())
                .collect();
            for sid in &stale {
                inner.members.remove(sid);
            }
            let id = inner
                .members
                .iter()
                .find(|(_, m)| m.state == MemberState::Ready)
                .map(|(id, _)| id.clone());
            if let Some(ref id) = id {
                inner.members.remove(id);
            }
            (id, stale)
        };
        if !stale.is_empty() {
            if let Some(host) = self.host.get() {
                for sid in &stale {
                    host.kill_member(sid);
                }
            }
            self.refill();
        }
        id
    }

    /// SW-LN-B31: the Settings shell override changed. Lock-local and cheap, so
    /// the command sets it in the same step as the override itself: from then on
    /// members spawned with the old shell are stale and [`Self::acquire_ready`]
    /// never hands one out. The frontend's follow-up `reconfigure` drains and
    /// refills the pool eagerly (one churn, not two).
    pub fn set_shell(&self, shell: Option<String>) {
        self.lock().shell = shell;
    }

    /// The shell override a pool member was reserved with, for its spawn.
    /// `None` when `id` is not a pool member.
    pub fn member_shell(&self, id: &str) -> Option<Option<String>> {
        self.lock().members.get(id).map(|m| m.shell.clone())
    }

    /// Flip a `Warming` member to `Ready` on its first prompt-start. Called from
    /// the `Osc133Bus` listener for *every* prompt-start; a no-op when the id is
    /// not a warming pool member (normal terminals, already-ready members).
    pub fn mark_ready(&self, session_id: &str) {
        // L2: lock-free skip when the pool is disabled — the common case for a
        // prompt-start, which fires for every terminal whether pooled or not.
        if !self.enabled.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let now = (self.clock)();
        let mut inner = self.lock();
        if let Some(m) = inner.members.get_mut(session_id) {
            if m.state == MemberState::Warming {
                m.state = MemberState::Ready;
                m.ready_at_ms = now;
                let latency = now.saturating_sub(m.warming_at_ms);
                inner.early_exits = 0;
                inner.telemetry.refill_ms.push(latency);
                let len = inner.telemetry.refill_ms.len();
                if len > REFILL_SAMPLES {
                    inner.telemetry.refill_ms.drain(..len - REFILL_SAMPLES);
                }
            }
        }
    }

    /// A pool member's PTY exited (e.g. external `kill -9`). Remove it and
    /// refill (AC-12). Called from the batcher's exit seam — cheap + lock-local;
    /// the actual session reclamation + refill run on a background task. A member
    /// that exits before its first prompt counts toward [`EARLY_EXIT_LIMIT`]; at the
    /// limit, refills pause for [`EARLY_EXIT_PAUSE_MS`].
    pub fn notify_member_exited(&self, session_id: &str) {
        let now = (self.clock)();
        {
            let mut inner = self.lock();
            let Some(member) = inner.members.remove(session_id) else {
                return;
            };
            if !member.spawned {
                inner.exited_unspawned.insert(session_id.to_string());
            }
            if member.state == MemberState::Warming
                && now.saturating_sub(member.warming_at_ms) < BOOT_WINDOW_MS
            {
                inner.early_exits += 1;
                if inner.early_exits >= EARLY_EXIT_LIMIT {
                    inner.paused_until_ms = now + EARLY_EXIT_PAUSE_MS;
                    log::warn!(
                        target: "warm_pool",
                        "{} warm shells in a row exited before their first prompt; refills paused for {}s",
                        inner.early_exits,
                        EARLY_EXIT_PAUSE_MS / 1000
                    );
                }
            }
        }
        // The shell already exited; hand reclamation + refill to the host (it owns
        // the async orchestration + the app handle needed to re-enter the pool).
        if let Some(host) = self.host.get() {
            host.reclaim_exited_and_refill(session_id.to_string());
        }
    }

    /// Kill every pool member synchronously. Used by `reconfigure` (before a
    /// refill). No-op without a host. At app exit the registry's complement kill
    /// reaps members instead (they never persist).
    pub fn drain(&self) {
        let ids: Vec<String> = {
            let mut inner = self.lock();
            inner.members.drain().map(|(id, _)| id).collect()
        };
        if let Some(host) = self.host.get() {
            for id in &ids {
                host.kill_member(id);
            }
        }
    }

    /// SW-LN-35: the prompt preset the frontend last configured (the user's
    /// Settings choice), for spawns that name none. `None` before the first
    /// `reconfigure`.
    pub fn configured_preset(&self) -> Option<String> {
        self.lock().preset.clone()
    }

    /// Spawn members up to the target (async, off the request path — AC-1/AC-5).
    /// Slots are reserved under the lock so concurrent refills never overshoot.
    pub fn refill(&self) {
        let Some(host) = self.host.get() else {
            return;
        };
        let now = (self.clock)();
        let (ids, preset, shell_integration) = {
            let mut inner = self.lock();
            // Paused after early exits: the next sweep past the pause tries again (and
            // one more early exit pauses it again).
            if !pool_fill_enabled(inner.target, inner.shell_integration)
                || now < inner.paused_until_ms
            {
                return;
            }
            let mut ids = Vec::new();
            let shell = inner.shell.clone();
            while inner.members.len() < inner.target {
                let id = next_member_id();
                inner.members.insert(
                    id.clone(),
                    Member {
                        state: MemberState::Warming,
                        warming_at_ms: now,
                        ready_at_ms: 0,
                        shell: shell.clone(),
                        spawned: false,
                    },
                );
                ids.push(id);
            }
            (ids, inner.preset.clone(), inner.shell_integration)
        };
        if ids.is_empty() {
            return;
        }
        // Reserve the slots under the lock (above), then hand each spawn to the
        // host off-lock. The host owns the blocking-off-worker spawn and the
        // success/failure reconciliation (`on_member_spawned`/`remove_member`) —
        // the pool just decides how many and with what params.
        let home = home_dir_string();
        for id in ids {
            host.spawn_member(
                id,
                MemberSpawnParams {
                    home: home.clone(),
                    cols: MEMBER_COLS,
                    rows: MEMBER_ROWS,
                    preset: preset.clone(),
                    shell_integration,
                },
            );
        }
        // AC-14: internal metrics, tracing only (no user-facing Settings).
        log::debug!(target: "warm_pool", "telemetry {:?}", self.telemetry());
    }

    /// Host re-entry after a member spawn **succeeds**: count the warm spawn iff
    /// the reserved slot still exists (a concurrent `drain`/`reconfigure` may have
    /// removed it — L3). Returns whether the host leaves the PTY be: the member
    /// joined, or it already exited and its exit reclaimed it. `false` tells the host
    /// to reclaim the now-live PTY so it can't leak as an orphan invisible to
    /// acquire/TTL/drain.
    pub fn on_member_spawned(&self, id: &str) -> bool {
        let mut inner = self.lock();
        if inner.exited_unspawned.remove(id) {
            return true;
        }
        match inner.members.get_mut(id) {
            Some(member) => {
                member.spawned = true;
                inner.telemetry.warm_spawns += 1;
                true
            }
            None => false,
        }
    }

    /// Host re-entry after a member spawn **fails**: drop the reserved slot.
    pub fn remove_member(&self, id: &str) {
        let mut inner = self.lock();
        inner.members.remove(id);
        inner.exited_unspawned.remove(id);
    }

    /// Ids of `Ready` members idle longer than `ttl_ms` (pure selection — the
    /// substance of AC-8, unit-testable via the injectable clock).
    pub fn collect_stale(&self, ttl_ms: u64) -> Vec<String> {
        let now = (self.clock)();
        let inner = self.lock();
        inner
            .members
            .iter()
            .filter(|(_, m)| {
                m.state == MemberState::Ready && now.saturating_sub(m.ready_at_ms) > ttl_ms
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Kill + respawn members past their TTL (AC-8). Driven by the reaper thread,
    /// which also tops the pool up (after a pause for early exits).
    pub fn recycle_stale(&self) {
        let stale = self.collect_stale(TTL_MS);
        if stale.is_empty() {
            self.refill();
            return;
        }
        {
            let mut inner = self.lock();
            for id in &stale {
                inner.members.remove(id);
            }
        }
        if let Some(host) = self.host.get() {
            for id in &stale {
                log::debug!(target: "warm_pool", "recycling stale member {id}");
                host.kill_member(id);
            }
        }
        self.refill();
    }

    /// AC-14: record a warm handoff (served from the pool).
    pub fn record_warm_hit(&self) {
        self.lock().telemetry.warm_hits += 1;
    }

    /// AC-14: record a cold/direct handoff (pool empty or disabled).
    pub fn record_direct(&self) {
        self.lock().telemetry.direct += 1;
    }

    /// AC-14: snapshot of internal counters (logs / dev tools only).
    pub fn telemetry(&self) -> TelemetrySnapshot {
        let inner = self.lock();
        let mut lat = inner.telemetry.refill_ms.clone();
        lat.sort_unstable();
        let pct = |p: usize| -> u64 {
            if lat.is_empty() {
                0
            } else {
                let idx = ((lat.len() - 1) * p) / 100;
                lat[idx]
            }
        };
        TelemetrySnapshot {
            warm_hits: inner.telemetry.warm_hits,
            direct: inner.telemetry.direct,
            warm_spawns: inner.telemetry.warm_spawns,
            refill_p50_ms: pct(50),
            refill_p95_ms: pct(95),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PoolInner> {
        // A poisoned pool lock is unrecoverable bookkeeping corruption; recover
        // the guard rather than panicking a batcher / command thread.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    fn with_clock(clock: ClockFn) -> Self {
        Self {
            inner: Mutex::new(PoolInner::default()),
            clock,
            host: OnceLock::new(),
            // Tests exercise the registry directly; enable the fast-path gate so
            // `mark_ready` runs (production sets this via `reconfigure`).
            enabled: std::sync::atomic::AtomicBool::new(true),
        }
    }

    #[cfg(test)]
    fn insert_member_for_test(&self, id: &str, state: MemberState, warming_at_ms: u64) {
        self.lock().members.insert(
            id.to_string(),
            Member {
                state,
                warming_at_ms,
                ready_at_ms: warming_at_ms,
                shell: None,
                spawned: true,
            },
        );
    }

    #[cfg(test)]
    fn set_target_for_test(&self, target: usize) {
        self.lock().target = target;
    }

    #[cfg(test)]
    fn set_host_for_test(&self, host: Arc<dyn WarmPoolHost>) {
        let _ = self.host.set(host);
    }

    #[cfg(test)]
    fn member_count(&self) -> usize {
        self.lock().members.len()
    }

    #[cfg(test)]
    fn contains_member(&self, id: &str) -> bool {
        self.lock().members.contains_key(id)
    }
}

/// Whether the pool should hold members for this config. Disabled at size 0, or
/// when shell integration is off — without OSC 133 a member never emits a
/// prompt-start, so it could never be marked `Ready` and would only churn
/// through the TTL reaper. Re-enabling integration fires `reconfigure` again.
fn pool_fill_enabled(target: usize, shell_integration: Option<bool>) -> bool {
    target > 0 && shell_integration != Some(false)
}

fn home_dir_string() -> String {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;

    fn test_clock() -> (Arc<AtomicU64>, ClockFn) {
        let now = Arc::new(AtomicU64::new(0));
        let n2 = now.clone();
        (now, Arc::new(move || n2.load(Ordering::Relaxed)))
    }

    /// A `WarmPoolHost` that records what the pool routes to it — proves the pool
    /// drives spawn/kill/reclaim through the port with no Tauri in scope.
    #[derive(Default)]
    struct RecordingHost {
        spawned: Mutex<Vec<(String, MemberSpawnParams)>>,
        killed: Mutex<Vec<String>>,
        reclaimed: Mutex<Vec<String>>,
    }

    impl WarmPoolHost for RecordingHost {
        fn spawn_member(&self, id: String, params: MemberSpawnParams) {
            // Record the params too — a mis-wired `MemberSpawnParams` field is the
            // one thing the extraction could silently break, so tests assert them.
            self.spawned.lock().unwrap().push((id, params));
        }
        fn kill_member(&self, id: &str) {
            self.killed.lock().unwrap().push(id.to_string());
        }
        fn reclaim_exited_and_refill(&self, id: String) {
            self.reclaimed.lock().unwrap().push(id);
        }
    }

    #[test]
    fn mark_ready_flips_warming_to_ready_once() {
        let (clock_val, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        pool.insert_member_for_test("m1", MemberState::Warming, 0);

        // First prompt-start → Ready, and records a refill latency.
        clock_val.store(120, Ordering::Relaxed);
        pool.mark_ready("m1");
        // Only Ready members are acquirable.
        assert_eq!(pool.acquire_ready(), Some("m1".to_string()));

        let snap = pool.telemetry();
        assert_eq!(snap.refill_p50_ms, 120);
    }

    #[test]
    fn warming_member_is_not_acquirable() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        pool.insert_member_for_test("m1", MemberState::Warming, 0);
        assert_eq!(pool.acquire_ready(), None);
    }

    #[test]
    fn mark_ready_is_noop_for_unknown_session() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        pool.mark_ready("not-a-member"); // normal terminal id — ignored
        assert_eq!(pool.member_count(), 0);
    }

    #[test]
    fn collect_stale_selects_only_aged_ready_members() {
        let (clock_val, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        pool.insert_member_for_test("ready-old", MemberState::Ready, 0);
        pool.insert_member_for_test("warming", MemberState::Warming, 0);

        // Not yet past TTL.
        clock_val.store(TTL_MS, Ordering::Relaxed);
        assert!(pool.collect_stale(TTL_MS).is_empty());

        // Fast-forward just past TTL → the ready member is stale; warming isn't.
        clock_val.store(TTL_MS + 1, Ordering::Relaxed);
        let stale = pool.collect_stale(TTL_MS);
        assert_eq!(stale, vec!["ready-old".to_string()]);
    }

    #[test]
    fn shells_that_die_on_boot_pause_refills_instead_of_respawning_without_end() {
        let (clock_val, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        let host = Arc::new(RecordingHost::default());
        pool.set_host_for_test(host.clone());
        pool.set_target_for_test(1);
        let spawned = || host.spawned.lock().unwrap().len();
        let last = || host.spawned.lock().unwrap().last().unwrap().0.clone();
        // Each member exits before its first prompt; the host refills after each.
        pool.refill();
        for _ in 0..EARLY_EXIT_LIMIT {
            pool.notify_member_exited(&last());
            pool.refill();
        }
        // The limit's exit paused refills: no new member.
        assert_eq!(spawned(), EARLY_EXIT_LIMIT as usize);
        assert_eq!(pool.member_count(), 0);
        clock_val.store(EARLY_EXIT_PAUSE_MS - 1, Ordering::Relaxed);
        pool.recycle_stale();
        assert_eq!(spawned(), EARLY_EXIT_LIMIT as usize);
        // The sweep past the pause tries once more; one more early exit pauses again.
        clock_val.store(EARLY_EXIT_PAUSE_MS, Ordering::Relaxed);
        pool.recycle_stale();
        assert_eq!(spawned(), EARLY_EXIT_LIMIT as usize + 1);
        pool.notify_member_exited(&last());
        pool.refill();
        assert_eq!(spawned(), EARLY_EXIT_LIMIT as usize + 1);
        // A shell that reaches its prompt resets the count; a later external kill of a
        // Ready member refills at once.
        clock_val.store(3 * EARLY_EXIT_PAUSE_MS, Ordering::Relaxed);
        pool.recycle_stale();
        pool.mark_ready(&last());
        pool.notify_member_exited(&last());
        pool.refill();
        assert_eq!(spawned(), EARLY_EXIT_LIMIT as usize + 3);
        assert_eq!(pool.member_count(), 1);
    }

    #[test]
    fn a_member_ending_after_its_boot_window_does_not_count_as_dying_on_boot() {
        let (clock_val, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        let host = Arc::new(RecordingHost::default());
        pool.set_host_for_test(host.clone());
        pool.set_target_for_test(1);
        // Members that never report a prompt, ended long after their spawn (as at a
        // terminal-server restart), keep the pool refilling.
        for round in 1..=EARLY_EXIT_LIMIT as u64 + 1 {
            pool.refill();
            clock_val.fetch_add(BOOT_WINDOW_MS, Ordering::Relaxed);
            let last = host.spawned.lock().unwrap().last().unwrap().0.clone();
            pool.notify_member_exited(&last);
            assert_eq!(host.spawned.lock().unwrap().len() as u64, round);
        }
        pool.refill();
        assert_eq!(pool.member_count(), 1);
    }

    #[test]
    fn a_member_that_exited_before_its_spawn_reported_is_not_killed_again() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        let host = Arc::new(RecordingHost::default());
        pool.set_host_for_test(host.clone());
        pool.set_target_for_test(1);
        pool.refill();
        let id = host.spawned.lock().unwrap()[0].0.clone();
        // Its exit (reclaimed by the host) beats the spawn's report.
        pool.notify_member_exited(&id);
        assert_eq!(&*host.reclaimed.lock().unwrap(), &[id.clone()]);
        assert!(
            pool.on_member_spawned(&id),
            "already reclaimed: no second kill"
        );
        assert_eq!(pool.telemetry().warm_spawns, 0);
        // A member dropped by a drain still gets its live PTY reclaimed.
        pool.refill();
        let next = host.spawned.lock().unwrap()[1].0.clone();
        pool.drain();
        assert!(!pool.on_member_spawned(&next));
    }

    #[test]
    fn the_sweep_tops_up_a_short_pool_and_a_reconfigure_lifts_a_pause() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        let host = Arc::new(RecordingHost::default());
        pool.set_host_for_test(host.clone());
        pool.set_target_for_test(1);
        pool.refill();
        // A failed spawn leaves the pool short; the sweep fills it.
        let first = host.spawned.lock().unwrap()[0].0.clone();
        pool.remove_member(&first);
        pool.recycle_stale();
        assert_eq!(host.spawned.lock().unwrap().len(), 2);
        // Paused by early exits, a settings change starts afresh.
        for _ in 0..EARLY_EXIT_LIMIT {
            let last = host.spawned.lock().unwrap().last().unwrap().0.clone();
            pool.notify_member_exited(&last);
            pool.refill();
        }
        assert_eq!(pool.member_count(), 0);
        pool.reconfigure(1, None, None);
        assert_eq!(pool.member_count(), 1);
    }

    #[test]
    fn recycle_stale_routes_kills_through_host_and_refills() {
        let (clock_val, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        let host = Arc::new(RecordingHost::default());
        pool.set_host_for_test(host.clone());
        // Target 1 with default (None) integration ⇒ fill-enabled; one aged Ready
        // member is stale once the clock passes TTL.
        pool.set_target_for_test(1);
        pool.insert_member_for_test("stale-1", MemberState::Ready, 0);
        clock_val.store(TTL_MS + 1, Ordering::Relaxed);

        pool.recycle_stale();

        // The stale member is killed through the port and dropped from the
        // registry; the follow-up refill re-reserves a replacement through the port.
        assert_eq!(&*host.killed.lock().unwrap(), &["stale-1".to_string()]);
        assert!(!pool.contains_member("stale-1"));
        assert_eq!(host.spawned.lock().unwrap().len(), 1);
        assert_eq!(pool.member_count(), 1);
    }

    #[test]
    fn refill_threads_configured_spawn_params_to_the_host() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        let host = Arc::new(RecordingHost::default());
        pool.set_host_for_test(host.clone());
        // Configure real spawn params; `reconfigure` drains (empty) then refills 1.
        pool.reconfigure(1, Some("agnoster".to_string()), Some(true));

        let spawned = host.spawned.lock().unwrap();
        assert_eq!(spawned.len(), 1);
        let (_id, params) = &spawned[0];
        // Every field the pool owns must reach the host untouched — the guard
        // against a silently mis-wired `MemberSpawnParams`.
        assert_eq!(
            params,
            &MemberSpawnParams {
                home: home_dir_string(),
                cols: MEMBER_COLS,
                rows: MEMBER_ROWS,
                preset: Some("agnoster".to_string()),
                shell_integration: Some(true),
            }
        );
    }

    /// SW-LN-B31: changing the shell retires every member spawned with the old
    /// shell and refills with the new one, so the next terminal is the new shell.
    #[test]
    fn set_shell_replaces_members_spawned_with_the_old_shell() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        let host = Arc::new(RecordingHost::default());
        pool.set_host_for_test(host.clone());
        pool.reconfigure(2, None, Some(true));
        let old: Vec<String> = host
            .spawned
            .lock()
            .unwrap()
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        for id in &old {
            pool.mark_ready(id);
            assert_eq!(pool.member_shell(id), Some(None));
        }

        pool.set_shell(Some("/bin/bash".to_string()));

        // The very next terminal never gets an old-shell member.
        assert_eq!(pool.acquire_ready(), None);
        let mut killed = host.killed.lock().unwrap().clone();
        killed.sort();
        let mut expected = old.clone();
        expected.sort();
        assert_eq!(killed, expected, "old-shell members are retired");
        let spawned = host.spawned.lock().unwrap();
        assert_eq!(spawned.len(), 4, "the pool refills to target");
        for (id, _) in &spawned[2..] {
            assert_eq!(
                pool.member_shell(id),
                Some(Some("/bin/bash".to_string())),
                "refilled members spawn with the new shell"
            );
        }
        drop(spawned);
        for id in &old {
            assert!(!pool.contains_member(id));
        }
    }

    #[test]
    fn set_shell_then_reconfigure_churns_the_pool_once() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        let host = Arc::new(RecordingHost::default());
        pool.set_host_for_test(host.clone());
        pool.reconfigure(2, None, Some(true));
        // What a Settings change does: the command, then the frontend's reconfigure.
        pool.set_shell(Some("/bin/bash".to_string()));
        pool.reconfigure(2, None, Some(true));
        assert_eq!(host.killed.lock().unwrap().len(), 2);
        assert_eq!(host.spawned.lock().unwrap().len(), 4);
    }

    #[test]
    fn a_member_with_the_current_shell_is_handed_out() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        let host = Arc::new(RecordingHost::default());
        pool.set_host_for_test(host.clone());
        pool.set_shell(Some("/bin/bash".to_string()));
        pool.reconfigure(1, None, Some(true));
        let id = host.spawned.lock().unwrap()[0].0.clone();
        pool.mark_ready(&id);
        assert_eq!(pool.acquire_ready(), Some(id));
        assert!(host.killed.lock().unwrap().is_empty());
    }

    /// A Ready member whose shell no longer matches (it raced the change) is
    /// never handed out: it is killed and the pool refills.
    #[test]
    fn acquire_ready_retires_a_member_with_a_stale_shell() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        let host = Arc::new(RecordingHost::default());
        pool.set_host_for_test(host.clone());
        pool.lock().shell = Some("/bin/bash".to_string());
        pool.set_target_for_test(1);
        pool.insert_member_for_test("zsh-member", MemberState::Ready, 0);

        assert_eq!(pool.acquire_ready(), None);
        assert_eq!(*host.killed.lock().unwrap(), vec!["zsh-member".to_string()]);
        let spawned = host.spawned.lock().unwrap();
        assert_eq!(spawned.len(), 1);
        assert_eq!(
            pool.member_shell(&spawned[0].0),
            Some(Some("/bin/bash".to_string()))
        );
    }

    #[test]
    fn remove_member_drops_a_reserved_slot() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        pool.insert_member_for_test("m1", MemberState::Warming, 0);
        assert_eq!(pool.member_count(), 1);
        pool.remove_member("m1");
        assert_eq!(pool.member_count(), 0);
        // Removing an unknown id is a harmless no-op.
        pool.remove_member("ghost");
        assert_eq!(pool.member_count(), 0);
    }

    #[test]
    fn telemetry_counts_hits_and_directs() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        pool.record_warm_hit();
        pool.record_warm_hit();
        pool.record_direct();
        let snap = pool.telemetry();
        assert_eq!(snap.warm_hits, 2);
        assert_eq!(snap.direct, 1);
    }

    #[test]
    fn refill_without_host_is_a_noop() {
        // No host set (unit context) → refill must not panic or reserve slots.
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        pool.set_target_for_test(3);
        pool.refill();
        assert_eq!(pool.member_count(), 0);
    }

    #[test]
    fn refill_routes_reserved_slots_through_the_host() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        pool.set_target_for_test(2);
        let host = Arc::new(RecordingHost::default());
        // `init` stores the host and fills to target — the reserved slots route
        // to the port; the pool tracks them under its lock.
        pool.init(host.clone());
        assert_eq!(host.spawned.lock().unwrap().len(), 2);
        assert_eq!(pool.member_count(), 2);
    }

    #[test]
    fn drain_routes_kills_through_the_host() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        pool.set_target_for_test(0); // init's refill is a no-op at target 0
        let host = Arc::new(RecordingHost::default());
        pool.init(host.clone());
        pool.insert_member_for_test("m1", MemberState::Ready, 0);
        pool.drain();
        assert_eq!(&*host.killed.lock().unwrap(), &["m1".to_string()]);
        assert_eq!(pool.member_count(), 0);
    }

    #[test]
    fn member_exit_routes_reclaim_through_the_host() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        pool.set_target_for_test(0);
        let host = Arc::new(RecordingHost::default());
        pool.init(host.clone());
        pool.insert_member_for_test("m1", MemberState::Ready, 0);
        pool.notify_member_exited("m1");
        assert_eq!(&*host.reclaimed.lock().unwrap(), &["m1".to_string()]);
        // A non-member exit routes nothing.
        pool.notify_member_exited("not-a-member");
        assert_eq!(host.reclaimed.lock().unwrap().len(), 1);
    }

    #[test]
    fn on_member_spawned_counts_only_when_slot_survives() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        pool.insert_member_for_test("m1", MemberState::Warming, 0);
        // Slot present → counts as a warm spawn and reports joined.
        assert!(pool.on_member_spawned("m1"));
        assert_eq!(pool.telemetry().warm_spawns, 1);
        // Slot already drained → not counted, reports not-joined (host reclaims).
        assert!(!pool.on_member_spawned("gone"));
        assert_eq!(pool.telemetry().warm_spawns, 1);
    }

    #[test]
    fn mark_ready_skips_when_pool_disabled() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        // No host, so drain/refill no-op; reconfigure(0) just flips the
        // fast-path gate off.
        pool.reconfigure(0, None, None);
        pool.insert_member_for_test("m1", MemberState::Warming, 0);
        pool.mark_ready("m1"); // gated off → no-op
        assert_eq!(pool.acquire_ready(), None); // still Warming
    }

    #[test]
    fn pool_fill_enabled_gates_on_size_and_integration() {
        assert!(pool_fill_enabled(2, Some(true)));
        assert!(pool_fill_enabled(2, None)); // default-on
        assert!(!pool_fill_enabled(0, Some(true))); // disabled
        assert!(!pool_fill_enabled(2, Some(false))); // no OSC → never Ready
    }

    #[test]
    fn refill_ms_samples_are_capped() {
        let (_v, clock) = test_clock();
        let pool = WarmPoolState::with_clock(clock);
        for i in 0..(REFILL_SAMPLES + 50) {
            let id = format!("m{i}");
            pool.insert_member_for_test(&id, MemberState::Warming, 0);
            pool.mark_ready(&id);
        }
        // Bounded retention; percentile getters stay cheap.
        assert!(pool.lock().telemetry.refill_ms.len() <= REFILL_SAMPLES);
    }
}
