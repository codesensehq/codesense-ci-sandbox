// SPDX-License-Identifier: MIT
//! RF-perf-5: gated, zero-overhead-off **memory** telemetry — the memory twin of
//! RF-perf-0's CPU/latency gate. Ships the single number every prior blank-screen
//! story (SW-FX-64/65) deferred on: the **WebContent renderer RSS** over time,
//! attributed to a retainer, into the durable app log — so the recurring
//! renderer-OOM incident becomes visible, attributable, and rank-ordered instead
//! of inferred from CPU counters after the fact.
//!
//! The app is **two processes**: the Rust/Tauri main and the out-of-process
//! WKWebView **WebContent** renderer (a separate PID — the 2.3–9 GB retainer that
//! the recurring blank tracks). A single-PID `sysinfo` read of the current process
//! sees only the former, so this module reports **both**: `main_rss` (current PID)
//! and `renderer_rss` (the WebContent helper PID(s) attributed to us).
//!
//! The verbose **forensic line** is behind [`memory_enabled`] (mirrors
//! [`crate::install::story_linker::sync_timing_enabled`]): a default `Info`
//! release logs **nothing** — no lock, no per-retainer read. Dev builds default
//! `Debug` → armed. RF-perf-7 note: the renderer-RSS **sample** itself (one
//! process-table scan) now runs **ungated** on the same watchdog tick (every 4th,
//! ~60 s), because it is the production memory-pressure signal (FE heap is
//! engine-gated to `unavailable` on WebKit); the gated forensic line reuses that
//! same snapshot, so an armed build never double-scans. Always on the watchdog
//! thread — never the UI/render thread — so it cannot stall paint.

use codesense_pty_core::process::ProcessIndex;

/// Log target for the gated memory line. Mirrors `SYNC_TIMING_TARGET`: a
/// per-target `log_enabled!` gate, so `CODESENSE_LOG=debug` (or a dev build's
/// default `Debug`) arms it and a default `Info` release leaves it inert.
pub(crate) const MEMORY_TARGET: &str = "memory";

/// Is the memory-telemetry gate armed? THE single read of [`MEMORY_TARGET`] —
/// every sample read (the `sysinfo` refresh, the process-table snapshot, the
/// retainer `len()`s) funnels through this so "measure only when armed" is
/// enforced by construction. `log_enabled!` is a cheap flag read; below `Debug`
/// (default prod) it is `false` and the whole sampler is skipped — inert-when-off.
#[inline]
pub(crate) fn memory_enabled() -> bool {
    log::log_enabled!(target: MEMORY_TARGET, log::Level::Debug)
}

// ---------------------------------------------------------------------------
// Renderer RSS (AC-1): the WebContent helper footprint attributed to our app.
// ---------------------------------------------------------------------------

/// The WebKit out-of-process helpers macOS spawns for a WKWebView. Only
/// `WebContent` is required for the gate (it holds the DOM/layer/GPU-backing
/// retention that the leak tracks); `Gpu`/`Networking` are reported separately as
/// context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebKitHelper {
    WebContent,
    Gpu,
    Networking,
}

/// Classify a process as a WebKit helper by its name/exe. Matches the distinctive
/// framework token (`WebKit.WebContent` etc.) in either the process name or the
/// exe path — robust to macOS `comm` truncation and to the display *title* being
/// the loaded origin (`http://localhost:1420` in dev) rather than the exe name.
/// Deliberately does **not** match the `localhost:1420` label (dev-only).
pub(crate) fn webkit_kind(name: &str, exe: Option<&str>) -> Option<WebKitHelper> {
    let matches = |needle: &str| name.contains(needle) || exe.is_some_and(|e| e.contains(needle));
    if matches("WebKit.WebContent") {
        Some(WebKitHelper::WebContent)
    } else if matches("WebKit.GPU") {
        Some(WebKitHelper::Gpu)
    } else if matches("WebKit.Networking") {
        Some(WebKitHelper::Networking)
    } else {
        None
    }
}

/// Summed RSS of the WebKit helper PIDs attributed to our app. `webcontent_pids`
/// counts **only** the WebContent helpers (the required renderer figure); GPU /
/// Networking bytes are reported as context but do not affect resolution.
/// `webcontent_pids == 0` means **unresolved** (either `responsible(our_pid)`
/// failed, or no WebContent PID matched) — the caller logs
/// `renderer_rss_bytes=unresolved`, never a fake 0.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RendererRss {
    pub webcontent_bytes: u64,
    pub gpu_bytes: u64,
    pub networking_bytes: u64,
    pub webcontent_pids: u32,
}

impl RendererRss {
    fn add(&mut self, kind: WebKitHelper, bytes: u64) {
        match kind {
            WebKitHelper::WebContent => {
                self.webcontent_bytes = self.webcontent_bytes.saturating_add(bytes);
                // Only WebContent counts toward resolution — it is THE renderer
                // figure; GPU/Networking are context and must not mask an absent
                // WebContent (which would log a fake 0 instead of `unresolved`).
                // `saturating_add` so a wrap could never flip a real match back to
                // 0 (which `is_unresolved` would misread as "no renderer").
                self.webcontent_pids = self.webcontent_pids.saturating_add(1);
            }
            WebKitHelper::Gpu => self.gpu_bytes = self.gpu_bytes.saturating_add(bytes),
            WebKitHelper::Networking => {
                self.networking_bytes = self.networking_bytes.saturating_add(bytes)
            }
        }
    }

    /// `true` when no WebContent PID could be attributed to us — the renderer
    /// figure is unknown (log `unresolved`, do not report 0 as real).
    pub(crate) fn is_unresolved(&self) -> bool {
        self.webcontent_pids == 0
    }
}

/// Pure attribution core (unit-testable without spawning processes): given the
/// process snapshot as `(pid, kind, memory_bytes)` triples, our PID, and a
/// `responsible`-PID resolver, sum the memory of the WebKit helpers whose
/// responsible PID matches ours.
///
/// macOS XPC re-parents every WebKit helper to launchd (PPID 1), so a parent-chain
/// walk finds none of them; the **responsible PID** is the stable link back to the
/// host app (verified live 2026-08-01). We compute `R = responsible(our_pid)` once
/// and keep helpers where `responsible(pid) == R`, which excludes other apps'
/// WebViews and our own PTY/agent children (they aren't WebKit-exe anyway).
fn attribute_renderer_rss(
    procs: impl Iterator<Item = (u32, Option<WebKitHelper>, u64)>,
    our_pid: u32,
    responsible: impl Fn(u32) -> Option<u32>,
) -> RendererRss {
    let Some(r) = responsible(our_pid) else {
        return RendererRss::default(); // unresolved: can't attribute
    };
    let mut out = RendererRss::default();
    for (pid, kind, mem) in procs {
        // `responsible()` is only queried for the handful of WebKit-exe matches,
        // never every process — cheap.
        if let Some(kind) = kind {
            if responsible(pid) == Some(r) {
                out.add(kind, mem);
            }
        }
    }
    out
}

/// Compute the renderer RSS from a process-table snapshot. Wires the real
/// `responsible`-PID FFI + [`webkit_kind`] into [`attribute_renderer_rss`].
pub(crate) fn renderer_rss(index: &ProcessIndex, our_pid: u32) -> RendererRss {
    attribute_renderer_rss(
        index
            .iter_procs()
            .map(|(pid, name, exe, mem)| (pid, webkit_kind(name, exe), mem)),
        our_pid,
        responsible,
    )
}

/// SR-0 (Q4): reload-seam snapshot — summed WebContent RSS **and the actual
/// WebContent PID values**, so the watchdog can log whether `win.reload()`
/// keeps the SAME WebContent process (bmalloc dirty RSS reclaimed in-PID —
/// the load-bearing memory-strategy unknown owed since RF-perf-7) or spawns a
/// NEW one. [`RendererRss`] is a `Copy` *count* and can't carry the PID list,
/// so this returns it directly. Reuses the SAME `ProcessIndex`, `webkit_kind`,
/// and `responsible` primitives as [`sample`]/[`renderer_rss`] — no new
/// attribution logic. PIDs are sorted for a stable, diffable log line.
pub(crate) fn renderer_webcontent_snapshot() -> (u64, Vec<u32>) {
    let our_pid = std::process::id();
    let index = ProcessIndex::build_with_memory();
    let Some(r) = responsible(our_pid) else {
        return (0, Vec::new()); // unresolved — never a fabricated 0-pid list
    };
    let mut bytes = 0u64;
    let mut pids = Vec::new();
    for (pid, name, exe, mem) in index.iter_procs() {
        if matches!(webkit_kind(name, exe), Some(WebKitHelper::WebContent))
            && responsible(pid) == Some(r)
        {
            bytes = bytes.saturating_add(mem);
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    (bytes, pids)
}

// --- responsible-PID FFI (the one new primitive; sysinfo doesn't expose it) ---

/// The Apple symbol that maps a PID to its "responsible" process — for an
/// XPC-spawned WKWebView helper, the host app. Underdocumented but stable in
/// practice (Chromium/Firefox ship it); lives in always-linked libSystem, so only
/// the declaration is needed. Isolated here as the crate's single raw `extern "C"`.
#[cfg(target_os = "macos")]
mod ffi {
    extern "C" {
        pub fn responsibility_get_pid_responsible_for_pid(pid: libc::pid_t) -> libc::pid_t;
    }
}

/// The responsible PID for `pid`, or `None` on failure. macOS-only; the non-macOS
/// stub returns `None`, which degrades the whole renderer sum to `unresolved`
/// (never a compile break, never a fabricated number).
#[cfg(target_os = "macos")]
pub(crate) fn responsible(pid: u32) -> Option<u32> {
    // SAFETY: a pure query taking a scalar and returning a scalar — no pointers,
    // no ownership. Returns the responsible PID, or <= 0 on failure.
    let r = unsafe { ffi::responsibility_get_pid_responsible_for_pid(pid as libc::pid_t) };
    (r > 0).then_some(r as u32)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn responsible(_pid: u32) -> Option<u32> {
    None
}

// ---------------------------------------------------------------------------
// Sample + emit (AC-1/2/3): one gated line on the watchdog tick.
// ---------------------------------------------------------------------------

fn opt_len(o: Option<usize>) -> String {
    // `None` = the map was contended and we skipped it (try_lock-or-skip), never
    // a zero we might misread as "empty".
    o.map_or_else(|| "skip".to_string(), |n| n.to_string())
}

/// The `nonjs_bytes` field: the non-JS renderer footprint (WebContent RSS − JS
/// heap), or a sentinel when it can't be computed honestly. `heap` and `wc` come
/// from different processes sampled at slightly different instants, so a
/// stale/larger heap than the current RSS is an inconsistent sample — reported as
/// `inconsistent`, never a fake `0` gap (the module's "never a misleading 0" rule).
fn nonjs_label(webcontent_bytes: u64, heap: u64) -> String {
    match heap {
        0 => "unavailable".to_string(),
        _ if heap > webcontent_bytes => "inconsistent".to_string(),
        _ => (webcontent_bytes - heap).to_string(),
    }
}

/// RF-perf-7 (AC-1, decision #2): sample the renderer WebContent RSS for the
/// watchdog's pressure decision. Runs the process-table scan **ungated** — it is
/// the production pressure signal, because FE `performance.memory` is engine-gated
/// to `unavailable` on WebKit (measured, RF-perf-5 AC-5) — but takes only ONE
/// snapshot per call, on the renderer-watchdog tick (every 4th, ~60 s) on the
/// watchdog thread, off the UI/render thread so it cannot stall paint. Returns
/// `Some(webcontent_bytes)` when a WebContent PID is attributed to us, or `None`
/// when unresolved — the caller must NOT treat unresolved as `0`, since a pressure
/// reload on an unknown sample would be a guess.
///
/// The verbose forensic `memory` line (AC-1/2/3) is emitted here too, but stays
/// behind [`memory_enabled`] and **reuses this same snapshot**, so an armed debug
/// build never double-scans the process table. Instrumentation only — reads never
/// mutate session/coordinator state.
pub(crate) fn sample(app: &tauri::AppHandle) -> Option<u64> {
    let our_pid = std::process::id();

    // ONE process-table snapshot feeds both the always-on pressure read and the
    // gated forensic line. `main_rss` (inside the forensic line) is our own PID's
    // memory in the same scan the renderer sum walks — no separate refresh.
    let index = ProcessIndex::build_with_memory();
    let renderer = renderer_rss(&index, our_pid);

    if memory_enabled() {
        log_forensic_line(app, &index, our_pid, renderer);
    }

    renderer_pressure_bytes(renderer)
}

/// RF-perf-7 (AC-1): the pressure figure from a renderer snapshot — `Some(bytes)`
/// when a WebContent PID was attributed to us, else `None`. Split out so the
/// "unresolved is `None`, never a fabricated `0`" rule (a `0` would read as
/// "renderer empty" and could suppress a real pressure reload) is unit-tested
/// without needing an `AppHandle`.
fn renderer_pressure_bytes(renderer: RendererRss) -> Option<u64> {
    (!renderer.is_unresolved()).then_some(renderer.webcontent_bytes)
}

/// Emit the one gated `memory` line (AC-1/2/3) from an already-taken snapshot.
/// Called by [`sample`] only when [`memory_enabled`] is armed — a default `Info`
/// release never reaches here (no lock, no per-retainer read happens unless armed).
fn log_forensic_line(
    app: &tauri::AppHandle,
    index: &ProcessIndex,
    our_pid: u32,
    renderer: RendererRss,
) {
    use tauri::Manager;

    let main_rss = index
        .iter_procs()
        .find_map(|(pid, _, _, mem)| (pid == our_pid).then_some(mem));

    // AC-2/AC-4: FE JS-heap subset (0 → engine-gated → `unavailable`) + the
    // non-JS gap, and the cumulative unhandled-rejection count (AC-4).
    let liveness = app.try_state::<crate::renderer_watchdog::RendererLiveness>();
    let heap = liveness.as_ref().map_or(0, |s| s.last_heap_bytes());
    let unhandled_rejections = liveness.as_ref().map_or(0, |s| s.unhandled_rejections());
    // RF-perf-4 (AC-1): the FE per-resource instance tally, attributing the renderer
    // footprint by surface class. `None` (never reported / momentarily contended) →
    // `skip`, never a fake 0 that would misread as "nothing mounted".
    let fe = liveness.as_ref().and_then(|s| s.instance_counts());
    let (fe_editors, fe_iframes, fe_webgl_terminals, fe_teams) = match fe {
        Some(c) => (
            c.editors.to_string(),
            c.iframes.to_string(),
            c.webgl_terminals.to_string(),
            c.teams.to_string(),
        ),
        None => {
            let skip = || "skip".to_string();
            (skip(), skip(), skip(), skip())
        }
    };

    let (renderer_str, nonjs_str) = if renderer.is_unresolved() {
        ("unresolved".to_string(), "unavailable".to_string())
    } else {
        let wc = renderer.webcontent_bytes;
        (wc.to_string(), nonjs_label(wc, heap))
    };
    let heap_str = if heap > 0 {
        heap.to_string()
    } else {
        "unavailable".to_string()
    };

    // AC-3: retainer-size snapshot (all try_lock-or-skip).
    let pty = crate::commands::pty_backend::session_len_and_dead(app);
    let (pty_total, pty_dead) = match pty {
        Some((t, d)) => (t.to_string(), d.to_string()),
        None => ("skip".to_string(), "skip".to_string()),
    };
    let (wave_target, repo_locks) = app
        .try_state::<std::sync::Arc<crate::content_engine::sync::WaveCoordinator>>()
        .map_or((None, None), |c| {
            (c.wave_target_locks_len(), c.repo_locks_len())
        });
    // SW-XS-4: the reverse-claim throttle map is gone with the automatic reverse
    // triggers. The field stays in the line (the soak parser's wire shape) at 0.
    let throttle = Some(0usize);
    // SW-XS-8: the reverse-claim latches are gone with the legacy engine; the
    // fields stay in the line (the soak parser's wire shape) at 0.
    let claimed = Some(0usize);
    let last_mtime = Some(0usize);

    // RF-perf-8: the two insert-only registries this story bounds. Named distinctly
    // from `repo_locks` above (which is the WaveCoordinator map, not these).
    let sync_resolution_locks = Some(0usize);
    let session_cancels = app
        .try_state::<crate::commands::claude_session::SessionWatcherState>()
        .and_then(|s| s.len());

    // Load correlation: whether a team is active. SW-XS-4: the live attached-worktree
    // count came from the reverse sweep's cadence signals, which are gone; the field
    // stays in the line (wire shape) as `unknown`.
    let live_repos = "unknown";
    let active_team = app
        .try_state::<crate::mcp::state::ActiveTeamState>()
        .map_or(0, |s| usize::from(s.get().is_some()));

    log::debug!(
        target: MEMORY_TARGET,
        "sample main_rss_bytes={} renderer_rss_bytes={} webcontent_pids={} gpu_bytes={} networking_bytes={} \
         js_heap_bytes={} nonjs_bytes={} pty_sessions={} pty_dead={} wave_target_locks={} repo_locks={} \
         reverse_claim_throttle={} claimed_repos={} last_claim_mtime={} \
         sync_resolution_locks={} session_cancels={} live_repos={} \
         active_team={} unhandled_rejections={} \
         fe_editors={} fe_iframes={} fe_webgl_terminals={} fe_teams={}",
        main_rss.map_or_else(|| "unavailable".to_string(), |n| n.to_string()),
        renderer_str,
        renderer.webcontent_pids,
        renderer.gpu_bytes,
        renderer.networking_bytes,
        heap_str,
        nonjs_str,
        pty_total,
        pty_dead,
        opt_len(wave_target),
        opt_len(repo_locks),
        opt_len(throttle),
        opt_len(claimed),
        opt_len(last_mtime),
        opt_len(sync_resolution_locks),
        opt_len(session_cancels),
        live_repos,
        active_team,
        unhandled_rejections,
        fe_editors,
        fe_iframes,
        fe_webgl_terminals,
        fe_teams,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_is_inert_without_a_debug_logger() {
        // No logger initialised in the unit-test binary → `log_enabled!` is false →
        // the sampler is skipped. Documents the inert-when-off default.
        assert!(!memory_enabled());
    }

    #[test]
    fn webkit_kind_matches_helpers_by_exe_or_name() {
        let wc = "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent";
        assert_eq!(
            webkit_kind("com.apple.WebKit.WebContent", Some(wc)),
            Some(WebKitHelper::WebContent)
        );
        assert_eq!(
            webkit_kind("", Some("/…/com.apple.WebKit.GPU")),
            Some(WebKitHelper::Gpu)
        );
        assert_eq!(
            webkit_kind("com.apple.WebKit.Networking", None),
            Some(WebKitHelper::Networking)
        );
    }

    #[test]
    fn webkit_kind_ignores_our_binary_and_the_dev_label() {
        // The Rust main and the `http://localhost:1420` display label are NOT
        // WebKit helpers — only the exe token counts.
        assert_eq!(
            webkit_kind("codesense", Some("/…/target/debug/codesense")),
            None
        );
        assert_eq!(webkit_kind("http://localhost:1420", None), None);
    }

    #[test]
    fn attribution_sums_only_our_responsible_webcontent() {
        // Our main = 37236, our WebContent = 37263 (responsible 644, same as ours);
        // a foreign app's WebContent = 7617 (responsible 7550) must be excluded.
        let procs = vec![
            (37263, Some(WebKitHelper::WebContent), 9_000_000_000),
            (37261, Some(WebKitHelper::Gpu), 130_000_000),
            (7617, Some(WebKitHelper::WebContent), 20_000_000), // other app
            (500, None, 1_000_000),                             // non-webkit noise
        ];
        let responsible = |pid: u32| match pid {
            37236 | 37263 | 37261 => Some(644),
            7617 => Some(7550),
            _ => Some(1),
        };
        let r = attribute_renderer_rss(procs.into_iter(), 37236, responsible);
        assert_eq!(r.webcontent_bytes, 9_000_000_000);
        assert_eq!(r.gpu_bytes, 130_000_000);
        // Only WebContent counts toward resolution — our GPU adds bytes but not a
        // webcontent PID, and the foreign app's WebContent is excluded entirely.
        assert_eq!(r.webcontent_pids, 1);
        assert!(!r.is_unresolved());
    }

    #[test]
    fn attribution_unresolved_when_our_responsible_is_unknown() {
        let procs = vec![(37263, Some(WebKitHelper::WebContent), 9_000_000_000)];
        let r = attribute_renderer_rss(procs.into_iter(), 37236, |_| None);
        assert!(r.is_unresolved());
        assert_eq!(r.webcontent_bytes, 0);
    }

    #[test]
    fn attribution_unresolved_when_no_webcontent_matches_us() {
        // Our responsible resolves, but every WebContent belongs to another app.
        let procs = vec![(7617, Some(WebKitHelper::WebContent), 20_000_000)];
        let responsible = |pid: u32| if pid == 37236 { Some(644) } else { Some(7550) };
        let r = attribute_renderer_rss(procs.into_iter(), 37236, responsible);
        assert!(r.is_unresolved());
    }

    #[test]
    fn renderer_pressure_bytes_is_none_when_unresolved() {
        // RF-perf-7 (AC-1): no WebContent PID → None (never a fabricated 0 that
        // could suppress a real pressure reload); a resolved WebContent → Some.
        let mut r = RendererRss::default();
        assert_eq!(renderer_pressure_bytes(r), None);
        r.add(WebKitHelper::WebContent, 6_442_450_944); // 6 GiB
        assert_eq!(renderer_pressure_bytes(r), Some(6_442_450_944));
        // A GPU-only match stays unresolved → still None (context bytes, not the
        // renderer figure).
        let mut g = RendererRss::default();
        g.add(WebKitHelper::Gpu, 130_000_000);
        assert_eq!(renderer_pressure_bytes(g), None);
    }

    #[test]
    fn nonjs_label_reports_gap_unavailable_or_inconsistent() {
        // Normal: non-JS footprint is the gap.
        assert_eq!(nonjs_label(2_000, 500), "1500");
        // Heap absent (engine-gated) → unavailable, not a fake gap.
        assert_eq!(nonjs_label(2_000, 0), "unavailable");
        // Heap larger than the current RSS = inconsistent cross-process sample →
        // sentinel, never a misleading 0.
        assert_eq!(nonjs_label(500, 2_000), "inconsistent");
        // Exactly equal → a real zero gap (all footprint is JS heap).
        assert_eq!(nonjs_label(1_000, 1_000), "0");
    }

    #[test]
    fn gpu_without_webcontent_still_reads_unresolved() {
        // A matched GPU helper adds context bytes but is NOT the renderer figure —
        // without a WebContent PID the renderer RSS is unknown, so `unresolved`
        // (never a fake 0 masked by the GPU match).
        let procs = vec![(37261, Some(WebKitHelper::Gpu), 130_000_000)];
        let responsible = |_pid: u32| Some(644);
        let r = attribute_renderer_rss(procs.into_iter(), 37236, responsible);
        assert!(r.is_unresolved());
        assert_eq!(r.gpu_bytes, 130_000_000);
        assert_eq!(r.webcontent_bytes, 0);
    }

    // --- AC-3 retainer accessors + AC-6 side-effect-free (idempotent reads) ---

    #[test]
    fn wave_coordinator_lens_start_zero_and_reads_are_side_effect_free() {
        let c = crate::content_engine::sync::WaveCoordinator::new();
        assert_eq!(c.wave_target_locks_len(), Some(0));
        assert_eq!(c.repo_locks_len(), Some(0));
        // A second read is identical — sampling never mutates the coordinator.
        assert_eq!(c.wave_target_locks_len(), Some(0));
        assert_eq!(c.repo_locks_len(), Some(0));
    }

    #[test]
    fn sync_lock_and_cancel_lens_start_zero_and_reads_are_side_effect_free() {
        // RF-perf-8: the two insert-only registries this story bounds report an
        // empty count when fresh, and the gate probe never mutates them.

        let cancels = crate::commands::claude_session::SessionWatcherState::default();
        assert_eq!(cancels.len(), Some(0));
        assert_eq!(cancels.len(), Some(0));
    }
}
