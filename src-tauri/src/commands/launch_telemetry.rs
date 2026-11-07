// SPDX-License-Identifier: MIT
//! SW-WA-F11 — legible launch telemetry for saved-agent launches.
//!
//! The pre-F11 launch flow (`mcp_workspace::start_agent` + `wait_for_live_agent`)
//! emitted no `tracing::` output and no structured record, so a timed-out launch
//! left no post-hoc evidence of whether the shell dribbled output, how long
//! settle took, or whether the agent process actually came up. This module adds
//! the minimum instrumentation needed to retrospect launch behavior across the
//! release week:
//!
//! * [`LaunchTelemetry`] — one audit record per saved-agent launch attempt.
//! * [`LaunchTelemetryBuilder`] — scratch state threaded through the launch
//!   path; it finalizes into a [`LaunchTelemetry`] at the terminal edge.
//! * [`LaunchTelemetryTracker`] — Tauri-managed per-app state. It keeps the
//!   latest `LaunchTelemetry` per session (so `list_sessions` can surface it)
//!   and stamps `pty_first_byte_ms` from the PTY output pipeline (one hashmap
//!   write on the first delivered batch; idempotent).
//!
//! ### Pre- vs post-F10 semantics
//!
//! Both the pre-F10 shell-mode launch (`start_agent` injects a shell line and
//! waits for it to settle) and the F10 argv-mode launch (binary spawned
//! directly) populate [`LaunchTelemetry`] with the same shape:
//!
//! * `launch_mode`: `"shell"` pre-F10, `"argv"` post-F10. Set at the call site
//!   so no reader has to infer it.
//! * `settle_ms` / `injected_at_ms`: shell-mode only; `None` for argv-mode (the
//!   fields describe the shell dance that no longer runs).
//! * `pty_first_byte_ms`: dominant signal for argv-mode (no settle to amortize
//!   against); still meaningful shell-side.
//! * `agent_live_ms`: how long until `wait_for_live_agent` observed the agent
//!   PID; `None` if the poll budget elapsed with no sighting (`WaitLiveElapsed`).
//!
//! `metrics_version` starts at `2` (reserving `1` for any ad-hoc historical
//! records predating F11). Bump whenever the field set changes. `3` adds the
//! optional `binary_path` / `binary_version` of the agent CLI the launch ran
//! (absent when the launch site did not resolve a binary, e.g. a plain shell).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Current telemetry format version. Readers of `agent-audit.jsonl` match on
/// this before interpreting the `telemetry` object: `1` is reserved for any
/// ad-hoc pre-F11 record, `2` is the pre-binary shape, `3` is the shape this
/// module writes (adds the optional `binaryPath` / `binaryVersion`).
pub const METRICS_VERSION: u32 = 3;

/// How a saved-agent launch delivered its startup line to the PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LaunchMode {
    /// Pre-F10: inject the launch command as a shell line into an interactive
    /// shell, then wait for it to render and settle.
    Shell,
    /// Post-F10: spawn the agent binary as PID 1 of the PTY, no shell dance.
    Argv,
}

/// Terminal state of a saved-agent launch attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchOutcome {
    /// Agent reached the live state (PTY first byte seen, settle completed, PID
    /// observed by `wait_for_live_agent`) within budget.
    Succeeded,
    /// Shell mode: the prompt never settled within `SUBMIT_TIMEOUT_MS`; nothing
    /// was injected.
    SettleTimeout,
    /// `wait_for_live_agent` burned its full budget without ever observing the
    /// agent process. Launch may still be alive (the panel status net covers it)
    /// but the launch flow treats it as a soft failure.
    WaitLiveElapsed,
    /// A pre-launch step (shell spawn, binding, scope prep) returned an error
    /// before the agent ever reached the PTY.
    SpawnFailed,
    /// Launch was explicitly aborted (`abort_dashboard_launch`) before reaching
    /// a live state.
    Canceled,
}

/// One audit record per saved-agent launch attempt. Written to the per-launch
/// `agent-audit.jsonl` line alongside the existing `spawn` / `kill` metadata
/// (`crate::commands::agent_audit::append_with_telemetry`) and surfaced on each
/// `list_sessions` row (`launch_telemetry`).
///
/// Field semantics are versioned by [`metrics_version`](LaunchTelemetry::metrics_version)
/// — see the module docs for the pre-/post-F10 distinction.
///
/// Serialization: the shell-only timing fields (`settle_ms`, `injected_at_ms`)
/// are *explicitly null* on a shell-mode launch that timed out before settling,
/// and *absent* on an argv-mode launch that has no settle step at all. Readers
/// (the retrospect jq) can therefore distinguish "shell-mode timeout" from
/// "argv-mode N/A" without having to join on `outcome`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchTelemetry {
    /// The PTY session id this launch bound to. Correlates with `list_sessions`
    /// rows and the `sessionId` field on sibling audit lines.
    pub session_id: String,
    /// Resolved agent runtime id (`builtin/claude`, `builtin/codex`, or a saved
    /// Agent key for a user-defined agent).
    pub agent: String,
    /// `"shell"` pre-F10, `"argv"` post-F10 — set at the call site.
    pub launch_mode: LaunchMode,
    /// Format version. Readers must branch on this before interpreting any
    /// other field: future-F11 extensions bump this.
    pub metrics_version: u32,
    /// Wall-clock time (epoch milliseconds) at `start_agent` entry — the launch
    /// reference clock.
    pub started_at_ms: i64,
    /// Milliseconds from `started_at_ms` to the first PTY byte observed on the
    /// session output channel. `None` if the launch failed before any byte
    /// arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty_first_byte_ms: Option<u64>,
    /// Milliseconds from `started_at_ms` to the shell settle completing. `None`
    /// on argv-mode launches (serialized as *absent*) and on a shell-mode launch
    /// that timed out before settling (serialized as *explicit null* so the
    /// jq retrospect can distinguish timeout from N/A).
    #[serde(default)]
    pub settle_ms: Option<u64>,
    /// Milliseconds from `started_at_ms` to the shell launch line being
    /// injected (the submit returned Ok). `None` on argv-mode launches
    /// (serialized as *absent*) and on a shell-mode launch that never injected
    /// (serialized as *explicit null* — same reader semantics as `settle_ms`).
    #[serde(default)]
    pub injected_at_ms: Option<u64>,
    /// Milliseconds from `started_at_ms` to `wait_for_live_agent` first
    /// observing the agent PID. `None` if the poll budget elapsed with no
    /// sighting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_live_ms: Option<u64>,
    /// Terminal state of the launch attempt.
    pub outcome: LaunchOutcome,
    /// Typed-error string from the failing exit edge, truncated to
    /// [`ERROR_TEXT_MAX_BYTES`]. `None` on `Succeeded`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_text: Option<String>,
    /// Absolute path of the agent CLI binary the launch spawned. `None` when the
    /// launch site resolved no binary (serialized as *absent*).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    /// The `--version` recorded for that binary when it was resolved (no extra
    /// spawn at launch). `None` when unknown (serialized as *absent*).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_version: Option<String>,
}

impl Serialize for LaunchTelemetry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        // Shell-mode records always emit the two shell-only timings (null when
        // the shell dance never produced them — a settle_timeout line); argv
        // mode omits them entirely because there is no shell dance at all.
        let shell_mode = matches!(self.launch_mode, LaunchMode::Shell);
        // Field count: 7 always-present + up to 5 conditional (pty_first_byte,
        // agent_live, error_text, binary_path, binary_version) + up to 2
        // shell-only.
        let mut n = 7;
        if self.pty_first_byte_ms.is_some() {
            n += 1;
        }
        if self.agent_live_ms.is_some() {
            n += 1;
        }
        if self.error_text.is_some() {
            n += 1;
        }
        if self.binary_path.is_some() {
            n += 1;
        }
        if self.binary_version.is_some() {
            n += 1;
        }
        if shell_mode {
            n += 2;
        }
        let mut s = serializer.serialize_struct("LaunchTelemetry", n)?;
        s.serialize_field("sessionId", &self.session_id)?;
        s.serialize_field("agent", &self.agent)?;
        s.serialize_field("launchMode", &self.launch_mode)?;
        s.serialize_field("metricsVersion", &self.metrics_version)?;
        s.serialize_field("startedAtMs", &self.started_at_ms)?;
        if let Some(v) = self.pty_first_byte_ms {
            s.serialize_field("ptyFirstByteMs", &v)?;
        }
        if shell_mode {
            // Explicit null on shell-mode timeouts so jq's
            // `.launchMode == "shell"` filter sees both successes and
            // never-settled timeouts without joining on `outcome`.
            s.serialize_field("settleMs", &self.settle_ms)?;
            s.serialize_field("injectedAtMs", &self.injected_at_ms)?;
        }
        if let Some(v) = self.agent_live_ms {
            s.serialize_field("agentLiveMs", &v)?;
        }
        s.serialize_field("outcome", &self.outcome)?;
        if let Some(ref e) = self.error_text {
            s.serialize_field("errorText", e)?;
        }
        if let Some(ref p) = self.binary_path {
            s.serialize_field("binaryPath", p)?;
        }
        if let Some(ref v) = self.binary_version {
            s.serialize_field("binaryVersion", v)?;
        }
        s.end()
    }
}

/// Longest `error_text` kept on an audit line. Longer messages are truncated so
/// one pathological error cannot bloat the JSONL file.
pub const ERROR_TEXT_MAX_BYTES: usize = 500;

/// Scratch state threaded through `start_agent` + `wait_for_live_agent`. Each
/// edge (settle, inject, live-agent sighting, exit) stamps its own timing /
/// outcome; the terminal caller finalizes into [`LaunchTelemetry`].
///
/// Non-blocking: every setter is a plain field write (no I/O, no mutex). The
/// audit write that follows [`finish`](Self::finish) is the only side effect
/// in the launch flow, deferred by callers via `tokio::spawn` (AC-5).
///
/// RAII cleanup: constructing with [`start_with_cleanup`](Self::start_with_cleanup)
/// installs an `on_drop` callback that runs if the builder is dropped
/// without ever being finalized ([`finish`](Self::finish) consumes the
/// closure). The launch spine uses it to clean up the tracker's `in_flight`
/// map on panic / `?` early return — see `LaunchTelemetryTracker::forget_in_flight`.
pub struct LaunchTelemetryBuilder {
    session_id: String,
    agent: String,
    launch_mode: LaunchMode,
    started_at_ms: i64,
    /// Reference `Instant` for every `*_ms` delta.
    started_at: Instant,
    pty_first_byte_ms: Option<u64>,
    settle_ms: Option<u64>,
    injected_at_ms: Option<u64>,
    agent_live_ms: Option<u64>,
    binary_path: Option<String>,
    binary_version: Option<String>,
    /// Fallback cleanup for the panic / `?` early-return case: runs on `Drop`
    /// if `finish` was never called. Removed by `finish` so the normal exit
    /// path does not double-clean.
    on_drop: Option<Box<dyn FnOnce() + Send>>,
}

impl std::fmt::Debug for LaunchTelemetryBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `on_drop` is a `FnOnce` with no `Debug` bound; keep the derived view
        // for every other field.
        f.debug_struct("LaunchTelemetryBuilder")
            .field("session_id", &self.session_id)
            .field("agent", &self.agent)
            .field("launch_mode", &self.launch_mode)
            .field("started_at_ms", &self.started_at_ms)
            .field("started_at", &self.started_at)
            .field("pty_first_byte_ms", &self.pty_first_byte_ms)
            .field("settle_ms", &self.settle_ms)
            .field("injected_at_ms", &self.injected_at_ms)
            .field("agent_live_ms", &self.agent_live_ms)
            .field("binary_path", &self.binary_path)
            .field("binary_version", &self.binary_version)
            .field("has_on_drop", &self.on_drop.is_some())
            .finish()
    }
}

impl LaunchTelemetryBuilder {
    /// Open a telemetry record for the given `session_id` + `agent`. Captures
    /// `started_at_ms` as wall-clock epoch milliseconds (for correlation with
    /// sibling audit lines) and an `Instant` for every subsequent delta.
    #[must_use]
    pub fn start(session_id: String, agent: String, launch_mode: LaunchMode) -> Self {
        Self {
            session_id,
            agent,
            launch_mode,
            started_at_ms: now_epoch_millis(),
            started_at: Instant::now(),
            pty_first_byte_ms: None,
            settle_ms: None,
            injected_at_ms: None,
            agent_live_ms: None,
            binary_path: None,
            binary_version: None,
            on_drop: None,
        }
    }

    /// [`start`](Self::start) with an `on_drop` callback that runs if the
    /// builder is dropped without ever being `finish`ed — the panic / `?`
    /// early-return cleanup path. The launch spine passes a closure that
    /// removes the session from the tracker's `in_flight` map so a crashed
    /// launch does not leak a tracker entry.
    #[must_use]
    pub fn start_with_cleanup(
        session_id: String,
        agent: String,
        launch_mode: LaunchMode,
        on_drop: impl FnOnce() + Send + 'static,
    ) -> Self {
        let mut s = Self::start(session_id, agent, launch_mode);
        s.on_drop = Some(Box::new(on_drop));
        s
    }

    /// This builder's PTY session id — used by callers that need to key the
    /// tracker lookup (e.g. the output-pipeline first-byte hook).
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Reference `Instant` for every delta. Public so the tracker can compute
    /// `pty_first_byte_ms` from an output batch's arrival `Instant` without
    /// re-reading wall-clock time.
    #[must_use]
    pub fn started_at(&self) -> Instant {
        self.started_at
    }

    /// Stamp the first-byte observation. Idempotent on `pty_first_byte_ms`
    /// (first observation wins). Observations whose `Instant` arrives AFTER
    /// the launch injected its shell line are DROPPED on purpose: that byte is
    /// the shell echoing our own injection, not independent output, and
    /// crediting it to `pty_first_byte_ms` would make the metric time the echo
    /// instead of the shell's prompt-render (shell mode) or the agent's first
    /// byte (argv mode).
    pub fn observe_first_byte(&mut self, at: Instant) {
        if self.pty_first_byte_ms.is_some() {
            return;
        }
        let ms = delta_ms(self.started_at, at);
        if self.injected_at_ms.is_some_and(|injected| ms >= injected) {
            // The byte arrived at or after the shell's own injection — treat
            // as an echo of the injected command, not independent output.
            return;
        }
        self.pty_first_byte_ms = Some(ms);
    }

    /// Shell-mode launches: stamp the moment the settle→write protocol returned
    /// with the launch line injected (shell went quiet, line written). Not
    /// called on argv-mode launches.
    #[cfg(test)]
    pub fn observe_shell_injected(&mut self, at: Instant) {
        let ms = delta_ms(self.started_at, at);
        // The shell protocol settles first then writes immediately; the two
        // observations collapse to one `Instant` without a sub-millisecond gap.
        // Record both so an argv-vs-shell retrospect can tell `settle_ms` apart
        // from any future split (e.g. an F12 that times the write separately).
        self.settle_ms = Some(ms);
        self.injected_at_ms = Some(ms);
    }

    /// Record the agent CLI binary this launch spawns and the version stored
    /// when it was resolved. Plain field writes — no spawn.
    pub fn set_binary(&mut self, path: &std::path::Path, version: Option<&str>) {
        self.binary_path = Some(path.to_string_lossy().into_owned());
        self.binary_version = version.map(str::to_string);
    }

    /// `wait_for_live_agent` observed the agent PID for the first time.
    pub fn observe_agent_live(&mut self, at: Instant) {
        if self.agent_live_ms.is_none() {
            self.agent_live_ms = Some(delta_ms(self.started_at, at));
        }
    }

    /// Finalize into an immutable record with the given `outcome` + optional
    /// typed-error string. Caller passes the full error text; it is truncated
    /// here to [`ERROR_TEXT_MAX_BYTES`]. Consumes the `on_drop` cleanup
    /// callback so the normal exit path never double-cleans.
    #[must_use]
    pub fn finish(mut self, outcome: LaunchOutcome, error_text: Option<String>) -> LaunchTelemetry {
        // Drop ran-on-finalize contract: the normal path consumes the cleanup
        // closure here so the `Drop` impl's fallback path becomes a no-op.
        self.on_drop.take();
        LaunchTelemetry {
            session_id: std::mem::take(&mut self.session_id),
            agent: std::mem::take(&mut self.agent),
            launch_mode: self.launch_mode,
            metrics_version: METRICS_VERSION,
            started_at_ms: self.started_at_ms,
            pty_first_byte_ms: self.pty_first_byte_ms,
            settle_ms: self.settle_ms,
            injected_at_ms: self.injected_at_ms,
            agent_live_ms: self.agent_live_ms,
            outcome,
            error_text: error_text.map(truncate_error_text),
            binary_path: self.binary_path.take(),
            binary_version: self.binary_version.take(),
        }
    }
}

impl Drop for LaunchTelemetryBuilder {
    fn drop(&mut self) {
        if let Some(cb) = self.on_drop.take() {
            // Panic / `?` early return path — the builder never reached
            // `finish`, so run the installed cleanup (normally
            // `tracker.forget_in_flight`).
            cb();
        }
    }
}

/// Upper bound on `latest`-map growth. The map is a per-session finalized
/// record used only to enrich `list_sessions` rows; once a session is gone,
/// nothing reads its record. Oldest entries beyond the cap are evicted first
/// so the map does not grow unboundedly over a long-running app session.
pub const LATEST_MAP_CAP: usize = 500;

/// App-level state: the latest finished telemetry per session (surfaced on
/// `list_sessions`) and the set of in-flight launches whose PTY output channel
/// should stamp `pty_first_byte_ms` on their first batch.
///
/// Reads: `latest(session_id)` returns the finalized record (or `None` for a
/// session that was launched before F11 or whose launch wrote no telemetry).
///
/// Writes:
///  * `register_in_flight(session_id, started_at)` on launch entry;
///  * `observe_pty_first_byte(session_id, at)` from the output pipeline (one
///    hashmap probe per Output batch; writes once, then the entry is dropped);
///  * `complete(telemetry)` on launch exit (success or failure).
///
/// Perf: a cheap `in_flight_count` atomic lets the PTY output pipeline
/// short-circuit the Mutex when no launch is in flight — [`has_in_flight`] is
/// a single `Relaxed` load, called once per Output batch system-wide.
#[derive(Debug, Default)]
pub struct LaunchTelemetryTracker {
    inner: Mutex<TrackerInner>,
    /// `inner.in_flight.len()` snapshot, kept in sync by `register_in_flight`
    /// / `observe_pty_first_byte` / `forget_in_flight` / `complete`. Readers
    /// use `has_in_flight` to skip the Mutex when zero (the normal case for
    /// every Output batch outside a launch window).
    in_flight_count: AtomicUsize,
}

#[derive(Debug, Default)]
struct TrackerInner {
    /// Sessions whose first PTY byte has not yet been observed. Keyed by the
    /// PTY session id; the value is `started_at` so the first byte's delta is
    /// trivial. Dropped on the first observation or on `complete`/`cancel`.
    in_flight: HashMap<String, Instant>,
    /// First-byte observations the output pipeline recorded before `start_agent`
    /// had a chance to attach them to its builder. Keyed by session id; drained
    /// by the launch flow when it finalizes the telemetry.
    first_bytes: HashMap<String, Instant>,
    /// Most recent finalized telemetry per session, up to [`LATEST_MAP_CAP`].
    /// `latest_order` holds insertion order so the oldest entry is evicted
    /// first when the cap is reached (a lightweight FIFO; the exact LRU
    /// semantics are overkill for a 500-entry read model).
    latest: HashMap<String, LaunchTelemetry>,
    latest_order: std::collections::VecDeque<String>,
}

impl LaunchTelemetryTracker {
    /// Fresh tracker — construct one and manage it as Tauri app state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fast-path for the PTY output pipeline: `true` only when at least one
    /// launch is currently in flight. Single `Relaxed` atomic load; callers
    /// avoid the Mutex on the much more common "nothing is launching" path.
    #[must_use]
    pub fn has_in_flight(&self) -> bool {
        self.in_flight_count.load(Ordering::Relaxed) > 0
    }

    /// Mark `session_id` as an in-flight launch so the output pipeline knows to
    /// stamp its first byte. `started_at` must be the same `Instant` the
    /// builder captured so `pty_first_byte_ms` is measured against the same
    /// reference clock.
    pub fn register_in_flight(&self, session_id: &str, started_at: Instant) {
        let mut inner = self.lock();
        if inner
            .in_flight
            .insert(session_id.to_string(), started_at)
            .is_none()
        {
            self.in_flight_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Called from the PTY output pipeline when a session's first `Output` event
    /// arrives (or on every subsequent one — the writer is idempotent). Returns
    /// `true` on the write that actually stamped the first byte (useful for
    /// callers that want to log it; the normal pipeline ignores the return).
    pub fn observe_pty_first_byte(&self, session_id: &str, at: Instant) -> bool {
        let mut inner = self.lock();
        if inner.in_flight.remove(session_id).is_some() {
            self.in_flight_count.fetch_sub(1, Ordering::Relaxed);
            inner.first_bytes.insert(session_id.to_string(), at);
            true
        } else {
            false
        }
    }

    /// Hand back the first-byte `Instant` the output pipeline stamped for this
    /// session, if any. Called by `start_agent` as it folds the observation
    /// into its builder; drains the entry so a later re-launch of the same id
    /// starts fresh.
    pub fn take_first_byte(&self, session_id: &str) -> Option<Instant> {
        self.lock().first_bytes.remove(session_id)
    }

    /// Clean-up seam for the panic / `?` early-return path (the builder's
    /// `Drop` cleanup — see `LaunchTelemetryBuilder::start_with_cleanup`).
    /// Removes any `in_flight` + `first_bytes` entries for `session_id` so a
    /// crashed launch does not leak a tracker row. Leaves `latest` intact.
    pub fn forget_in_flight(&self, session_id: &str) {
        let mut inner = self.lock();
        if inner.in_flight.remove(session_id).is_some() {
            self.in_flight_count.fetch_sub(1, Ordering::Relaxed);
        }
        inner.first_bytes.remove(session_id);
    }

    /// Record a finalized launch record for the session. Drops any lingering
    /// `in_flight` / `first_bytes` entry (a launch that failed before any byte
    /// never registers one) and stores the record as the session's `latest`,
    /// evicting the oldest when the map is at [`LATEST_MAP_CAP`].
    pub fn complete(&self, telemetry: LaunchTelemetry) {
        let mut inner = self.lock();
        if inner.in_flight.remove(&telemetry.session_id).is_some() {
            self.in_flight_count.fetch_sub(1, Ordering::Relaxed);
        }
        inner.first_bytes.remove(&telemetry.session_id);
        let key = telemetry.session_id.clone();
        if inner.latest.insert(key.clone(), telemetry).is_none() {
            inner.latest_order.push_back(key);
            while inner.latest.len() > LATEST_MAP_CAP {
                if let Some(oldest) = inner.latest_order.pop_front() {
                    inner.latest.remove(&oldest);
                } else {
                    break;
                }
            }
        }
    }

    /// The latest finalized telemetry for `session_id`, or `None`. Kept for
    /// one-off lookups and tests; `list_sessions` enrichment uses the batched
    /// [`latest_many`](Self::latest_many) instead so a long result does not pay
    /// per-row Mutex contention.
    #[cfg(test)]
    pub fn latest(&self, session_id: &str) -> Option<LaunchTelemetry> {
        self.lock().latest.get(session_id).cloned()
    }

    /// Batch read of the latest finalized telemetry for every session id in
    /// `keys`. One Mutex acquisition for every call, so `with_launch_telemetry`
    /// doesn't pay per-row contention when enriching a long `list_sessions`
    /// result.
    #[must_use]
    pub fn latest_many<K: AsRef<str>>(&self, keys: &[K]) -> HashMap<String, LaunchTelemetry> {
        let inner = self.lock();
        keys.iter()
            .filter_map(|k| {
                let k = k.as_ref();
                inner.latest.get(k).map(|t| (k.to_string(), t.clone()))
            })
            .collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, TrackerInner> {
        self.inner.lock().unwrap_or_else(|poisoned| {
            log::warn!(
                target: "launch_telemetry",
                "tracker Mutex was poisoned — recovering inner state",
            );
            poisoned.into_inner()
        })
    }
}

fn delta_ms(from: Instant, to: Instant) -> u64 {
    to.saturating_duration_since(from).as_millis() as u64
}

fn now_epoch_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Truncate `text` on a UTF-8 boundary at [`ERROR_TEXT_MAX_BYTES`]. A longer
/// message gets a trailing `"…"` so a reader can tell the record was cut.
fn truncate_error_text(mut text: String) -> String {
    if text.len() <= ERROR_TEXT_MAX_BYTES {
        return text;
    }
    let mut cut = ERROR_TEXT_MAX_BYTES;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text.truncate(cut);
    text.push('…');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn metrics_version_is_three_and_launch_mode_serializes_lowercase() {
        let t = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
        )
        .finish(LaunchOutcome::Succeeded, None);
        assert_eq!(t.metrics_version, 3);
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["metricsVersion"], 3);
        assert_eq!(json["launchMode"], "shell");
        assert_eq!(json["outcome"], "succeeded");
    }

    #[test]
    fn argv_record_carries_binary_path_and_version_when_set() {
        let mut builder = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Argv,
        );
        builder.set_binary(std::path::Path::new("/tmp/a/claude"), Some("2.1.296"));
        let t = builder.finish(LaunchOutcome::Succeeded, None);
        assert_eq!(t.binary_path.as_deref(), Some("/tmp/a/claude"));
        assert_eq!(t.binary_version.as_deref(), Some("2.1.296"));
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["binaryPath"], "/tmp/a/claude");
        assert_eq!(json["binaryVersion"], "2.1.296");
        assert_eq!(json["metricsVersion"], 3);
        let back: LaunchTelemetry = serde_json::from_value(json).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn record_without_binary_omits_both_keys() {
        let t = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Argv,
        )
        .finish(LaunchOutcome::SpawnFailed, Some("boom".into()));
        let json = serde_json::to_value(&t).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.get("binaryPath").is_none(), "{json}");
        assert!(obj.get("binaryVersion").is_none(), "{json}");

        // A path with an unknown version emits only the path.
        let mut builder = LaunchTelemetryBuilder::start(
            "sess-2".into(),
            "builtin/codex".into(),
            LaunchMode::Argv,
        );
        builder.set_binary(std::path::Path::new("/tmp/b/codex"), None);
        let json = serde_json::to_value(builder.finish(LaunchOutcome::Succeeded, None)).unwrap();
        assert_eq!(json["binaryPath"], "/tmp/b/codex");
        assert!(json.as_object().unwrap().get("binaryVersion").is_none());
    }

    /// Argv-mode records never emit `settleMs` / `injectedAtMs` as fields at
    /// all — even when the builder was told to stamp them. The gate lives in
    /// `impl Serialize for LaunchTelemetry` (keyed on `launch_mode`), not in
    /// the builder; this test exercises the Serialize branch explicitly.
    #[test]
    fn argv_mode_omits_shell_only_timing_fields_even_when_stamped() {
        let mut builder = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Argv,
        );
        // Stamp them anyway so a tautological "builder default is None"
        // interpretation is impossible — the gate must still skip.
        builder.observe_shell_injected(builder.started_at() + Duration::from_millis(50));
        let t = builder.finish(LaunchOutcome::Succeeded, None);
        assert_eq!(t.launch_mode, LaunchMode::Argv);
        // The in-memory record still carries them (the builder stamped them)
        // but the JSON surface strips them for argv-mode.
        assert_eq!(t.settle_ms, Some(50));
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["launchMode"], "argv");
        assert!(
            json.as_object().unwrap().get("settleMs").is_none(),
            "argv-mode JSON must not carry settleMs even when the builder stamped one: {json}"
        );
        assert!(
            json.as_object().unwrap().get("injectedAtMs").is_none(),
            "argv-mode JSON must not carry injectedAtMs even when the builder stamped one: {json}"
        );
    }

    /// Shell-mode record of a settle timeout emits `settleMs` as explicit
    /// `null` (not omitted). Lets the jq retrospect's
    /// `select(.launchMode == "shell")` see both success + timeout rows in
    /// the shell-mode denominator without having to join on `.outcome`.
    #[test]
    fn shell_mode_settle_timeout_emits_explicit_null_not_omitted() {
        let t = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
        )
        .finish(
            LaunchOutcome::SettleTimeout,
            Some("shell never settled".into()),
        );
        let json = serde_json::to_value(&t).unwrap();
        let obj = json.as_object().unwrap();
        assert!(
            obj.contains_key("settleMs"),
            "shell-mode must emit settleMs even on timeout: {json}"
        );
        assert!(
            obj["settleMs"].is_null(),
            "shell-mode timeout → explicit null"
        );
        assert!(obj.contains_key("injectedAtMs"));
        assert!(obj["injectedAtMs"].is_null());
        assert_eq!(obj["outcome"], "settle_timeout");
    }

    /// Shell-mode happy path: both timing fields present and populated.
    #[test]
    fn shell_mode_happy_path_emits_timings_as_numbers() {
        let mut builder = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
        );
        builder.observe_shell_injected(builder.started_at() + Duration::from_millis(42));
        let t = builder.finish(LaunchOutcome::Succeeded, None);
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json["settleMs"], 42);
        assert_eq!(json["injectedAtMs"], 42);
    }

    #[test]
    fn shell_mode_records_settle_and_inject_from_one_observation() {
        let mut builder = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
        );
        let after = builder.started_at() + Duration::from_millis(42);
        builder.observe_shell_injected(after);
        let t = builder.finish(LaunchOutcome::Succeeded, None);
        assert_eq!(t.settle_ms, Some(42));
        assert_eq!(t.injected_at_ms, Some(42));
    }

    #[test]
    fn first_byte_observation_is_idempotent_first_wins() {
        let mut builder = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Argv,
        );
        let first = builder.started_at() + Duration::from_millis(10);
        let later = builder.started_at() + Duration::from_millis(500);
        builder.observe_first_byte(first);
        builder.observe_first_byte(later);
        let t = builder.finish(LaunchOutcome::Succeeded, None);
        assert_eq!(t.pty_first_byte_ms, Some(10));
    }

    /// Shell-mode launches echo the injected command back on the PTY; that
    /// byte is NOT independent output and must not time `pty_first_byte_ms`.
    /// The guard: `observe_first_byte` ignores observations whose `Instant`
    /// is at or after `injected_at`.
    #[test]
    fn first_byte_after_inject_is_dropped_as_a_shell_echo() {
        let mut builder = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
        );
        builder.observe_shell_injected(builder.started_at() + Duration::from_millis(100));
        // Byte arrives AFTER the injection → treated as an echo of the
        // injected command, not the shell's own prompt output.
        builder.observe_first_byte(builder.started_at() + Duration::from_millis(105));
        let t = builder.finish(LaunchOutcome::Succeeded, None);
        assert_eq!(
            t.pty_first_byte_ms, None,
            "post-inject byte must be dropped as a shell echo"
        );
    }

    /// A byte that arrives BEFORE injection is the shell's prompt render — the
    /// signal we actually want to time. Must be kept.
    #[test]
    fn first_byte_before_inject_is_kept_as_the_prompt() {
        let mut builder = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
        );
        // The order the launch flow actually runs: observe_first_byte fires
        // from the output pipeline as the shell prints its prompt, then the
        // settle protocol writes the command → observe_shell_injected.
        builder.observe_first_byte(builder.started_at() + Duration::from_millis(30));
        builder.observe_shell_injected(builder.started_at() + Duration::from_millis(100));
        let t = builder.finish(LaunchOutcome::Succeeded, None);
        assert_eq!(t.pty_first_byte_ms, Some(30));
    }

    #[test]
    fn error_text_truncates_on_a_utf8_boundary_with_an_ellipsis() {
        let long = "x".repeat(ERROR_TEXT_MAX_BYTES + 50);
        let t = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
        )
        .finish(LaunchOutcome::SpawnFailed, Some(long));
        let text = t.error_text.unwrap();
        assert!(text.ends_with('…'), "truncated text ends with ellipsis");
        assert!(text.len() <= ERROR_TEXT_MAX_BYTES + '…'.len_utf8());
    }

    #[test]
    fn error_text_shorter_than_cap_is_untouched() {
        let short = "nope".to_string();
        let t = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
        )
        .finish(LaunchOutcome::SpawnFailed, Some(short.clone()));
        assert_eq!(t.error_text.as_deref(), Some(short.as_str()));
    }

    #[test]
    fn tracker_pipeline_wires_output_hook_to_builder_fold() {
        let tracker = LaunchTelemetryTracker::new();
        let mut builder = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Argv,
        );
        tracker.register_in_flight("sess-1", builder.started_at());

        // Output pipeline hook — stamps once.
        let at = builder.started_at() + Duration::from_millis(7);
        assert!(
            tracker.observe_pty_first_byte("sess-1", at),
            "first byte stamps"
        );
        assert!(
            !tracker.observe_pty_first_byte("sess-1", at + Duration::from_millis(1)),
            "second byte is a no-op (not in-flight any more)"
        );

        // Launch flow folds the first byte into the builder before finalizing.
        if let Some(at) = tracker.take_first_byte("sess-1") {
            builder.observe_first_byte(at);
        }
        let telemetry = builder.finish(LaunchOutcome::Succeeded, None);
        tracker.complete(telemetry.clone());
        assert_eq!(telemetry.pty_first_byte_ms, Some(7));

        // Latest surfaces for `list_sessions`.
        assert_eq!(tracker.latest("sess-1"), Some(telemetry));
        assert_eq!(tracker.latest("sess-missing"), None);
    }

    #[test]
    fn tracker_output_hook_ignores_a_session_that_was_not_registered() {
        let tracker = LaunchTelemetryTracker::new();
        assert!(
            !tracker.observe_pty_first_byte("not-launched", Instant::now()),
            "output on an unregistered session is not stamped"
        );
        assert!(tracker.take_first_byte("not-launched").is_none());
    }

    #[test]
    fn complete_overwrites_the_sessions_latest() {
        let tracker = LaunchTelemetryTracker::new();
        let a = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
        )
        .finish(LaunchOutcome::SettleTimeout, Some("timed out".into()));
        let b = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Argv,
        )
        .finish(LaunchOutcome::Succeeded, None);
        tracker.complete(a);
        tracker.complete(b.clone());
        assert_eq!(tracker.latest("sess-1"), Some(b));
    }

    #[test]
    fn agent_live_observation_is_idempotent_first_wins() {
        let mut builder = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Argv,
        );
        let a = builder.started_at() + Duration::from_millis(120);
        let b = builder.started_at() + Duration::from_millis(300);
        builder.observe_agent_live(a);
        builder.observe_agent_live(b);
        let t = builder.finish(LaunchOutcome::Succeeded, None);
        assert_eq!(t.agent_live_ms, Some(120));
    }

    /// Builders constructed with `start_with_cleanup` run the installed
    /// closure on `Drop` if `finish` was never called — the panic / `?`
    /// early-return cleanup path.
    #[test]
    fn drop_without_finish_runs_the_cleanup_closure() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let fired = Arc::new(AtomicBool::new(false));
        {
            let fired = fired.clone();
            let _builder = LaunchTelemetryBuilder::start_with_cleanup(
                "sess-1".into(),
                "builtin/claude".into(),
                LaunchMode::Shell,
                move || fired.store(true, Ordering::SeqCst),
            );
            // Dropped at the end of the block without calling `finish`.
        }
        assert!(
            fired.load(Ordering::SeqCst),
            "cleanup must run on panic/?-return"
        );
    }

    /// Calling `finish` consumes the cleanup closure so the normal path never
    /// double-cleans. (Verifies the `on_drop.take()` in `finish`.)
    #[test]
    fn finish_consumes_the_cleanup_closure_so_drop_is_a_noop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let fired = Arc::new(AtomicUsize::new(0));
        let fired_cb = fired.clone();
        let builder = LaunchTelemetryBuilder::start_with_cleanup(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
            move || {
                fired_cb.fetch_add(1, Ordering::SeqCst);
            },
        );
        let _ = builder.finish(LaunchOutcome::Succeeded, None);
        // `finish` consumed the builder; cleanup must not have run.
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }

    /// `forget_in_flight` removes a session from `in_flight` + `first_bytes`
    /// without touching `latest` — the Drop-safety cleanup seam.
    #[test]
    fn forget_in_flight_removes_in_flight_but_keeps_latest() {
        let tracker = LaunchTelemetryTracker::new();
        tracker.register_in_flight("sess-1", Instant::now());
        tracker.complete(
            LaunchTelemetryBuilder::start(
                "sess-1".into(),
                "builtin/claude".into(),
                LaunchMode::Shell,
            )
            .finish(LaunchOutcome::Succeeded, None),
        );
        // After complete, latest is set and in_flight is already cleared;
        // register again to prove forget drops it.
        tracker.register_in_flight("sess-1", Instant::now());
        assert!(tracker.has_in_flight());
        tracker.forget_in_flight("sess-1");
        assert!(!tracker.has_in_flight(), "forget drops in_flight");
        // The finalized record is untouched.
        assert!(tracker.latest("sess-1").is_some());
    }

    /// The `in_flight_count` atomic stays in sync across the register / observe
    /// / forget / complete transitions so the pty_host hot path can short-circuit
    /// the Mutex on a `Relaxed` load.
    #[test]
    fn has_in_flight_tracks_registrations_atomically() {
        let tracker = LaunchTelemetryTracker::new();
        assert!(!tracker.has_in_flight(), "empty tracker reads false");

        tracker.register_in_flight("a", Instant::now());
        tracker.register_in_flight("b", Instant::now());
        assert!(tracker.has_in_flight());

        // observe_pty_first_byte removes from in_flight and drops the counter.
        assert!(tracker.observe_pty_first_byte("a", Instant::now()));
        assert!(tracker.has_in_flight(), "one still in flight");

        tracker.forget_in_flight("b");
        assert!(!tracker.has_in_flight(), "empty again");

        // Idempotent register of the same session does NOT double-count.
        tracker.register_in_flight("c", Instant::now());
        tracker.register_in_flight("c", Instant::now());
        assert!(tracker.has_in_flight());
        tracker.forget_in_flight("c");
        assert!(!tracker.has_in_flight());
    }

    /// `latest_many` is one Mutex acquisition for a whole batch — the surface
    /// `list_sessions` enrichment calls so a long result does not pay per-row
    /// contention.
    #[test]
    fn latest_many_returns_the_subset_for_keys_in_one_lock() {
        let tracker = LaunchTelemetryTracker::new();
        for id in ["s-a", "s-b", "s-c"] {
            tracker.complete(
                LaunchTelemetryBuilder::start(
                    id.into(),
                    "builtin/claude".into(),
                    LaunchMode::Shell,
                )
                .finish(LaunchOutcome::Succeeded, None),
            );
        }
        let map = tracker.latest_many(&["s-a", "s-missing", "s-c"]);
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("s-a"));
        assert!(map.contains_key("s-c"));
        assert!(!map.contains_key("s-missing"));
    }

    /// The `latest` map evicts the oldest entry once it hits `LATEST_MAP_CAP`,
    /// so a long-running app session cannot grow it unboundedly.
    #[test]
    fn latest_map_evicts_oldest_at_the_cap() {
        let tracker = LaunchTelemetryTracker::new();
        for i in 0..(LATEST_MAP_CAP + 3) {
            tracker.complete(
                LaunchTelemetryBuilder::start(
                    format!("sess-{i}"),
                    "builtin/claude".into(),
                    LaunchMode::Shell,
                )
                .finish(LaunchOutcome::Succeeded, None),
            );
        }
        // The three earliest entries were evicted; the newest is kept.
        assert!(tracker.latest("sess-0").is_none(), "oldest evicted");
        assert!(tracker.latest("sess-2").is_none());
        assert!(tracker
            .latest(&format!("sess-{}", LATEST_MAP_CAP + 2))
            .is_some());
        // Size equals the cap exactly.
        assert_eq!(tracker.lock().latest.len(), LATEST_MAP_CAP);
    }

    /// Deserializing a record that was written with shell-mode explicit-null
    /// fields round-trips the None semantics correctly.
    #[test]
    fn shell_mode_null_timings_round_trip_through_json() {
        let original = LaunchTelemetryBuilder::start(
            "sess-1".into(),
            "builtin/claude".into(),
            LaunchMode::Shell,
        )
        .finish(LaunchOutcome::SettleTimeout, Some("timed out".into()));
        let encoded = serde_json::to_value(&original).unwrap();
        let decoded: LaunchTelemetry = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.launch_mode, LaunchMode::Shell);
        assert_eq!(decoded.settle_ms, None);
        assert_eq!(decoded.injected_at_ms, None);
        assert_eq!(decoded.outcome, LaunchOutcome::SettleTimeout);
    }
}
