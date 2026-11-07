// SPDX-License-Identifier: MIT
pub(crate) mod media;
/// Rust mirror of the canonical agent-state literals (TS home: `agent-state.ts`).
pub mod agent_state;
/// Agents: named, team-shared worker definitions (runtime, args, MCP servers,
/// startup manifest, native CLI permissions) stored as git-synced revisioned
/// YAML, plus the frozen startup-manifest types the context lifecycle consumes.
pub mod agents;
mod attribution;
mod avatars;
mod commands;
mod queue_roles;
pub mod records;
mod session_conversation;
mod session_usage;
pub mod verbs;

/// Compiler-only façade for SW-RM-4's negative ownership proofs. The runtime
/// crate does not export these internal transport stages.
///
/// A caller cannot forge an admission proof around a Hello read from another
/// stream because the proof's field is private:
///
/// ```compile_fail
/// use codesense_host_proto::Hello;
/// use codesense_lib::rm4_transport_typeproof::RemoteAdmissionProof;
///
/// fn forge(hello: Hello) -> RemoteAdmissionProof {
///     RemoteAdmissionProof { hello }
/// }
/// ```
///
/// Nor can it construct a pending remote stream from independently selected
/// parts; all of its fields remain private:
///
/// ```compile_fail
/// use codesense_lib::rm4_transport_typeproof::PendingRemoteTransport;
///
/// fn forge() -> PendingRemoteTransport {
///     PendingRemoteTransport { ..todo!() }
/// }
/// ```
#[cfg(doc)]
pub mod rm4_transport_typeproof {
    pub use crate::commands::host_connection::{PendingRemoteTransport, RemoteAdmissionProof};
}
/// SW-PB-21: the build channel and public host, injected into every webview.
mod app_channel;
mod console_log;
pub mod content_engine;
mod context_query;
mod context_repo;
/// The machine-local coordination store (`{home}/orchestration/coordinator.sqlite3`):
/// run claims and leases, run events, deliveries, and the routine runtime tables.
mod coordinator;
/// SW-PB-22a: dashboard publishing, app side. Beta releases enable this feature.
#[cfg(feature = "dashboard-publish")]
pub mod dashboard_publish;
/// SW-DX-3: the agent session event bus (`tokio::broadcast` hub) and the scoped
/// read-token store the read-only HTTP routes accept.
pub mod dashboard_stream;
mod delivery;
/// SW-DX-2: the scanned-pipeline-tree query, an adapter over `context_query`
/// whose output pins the frontend `DocSummary` shape (cross-language golden).
pub mod doc_query;
pub mod domain;
mod exit_coordinator;
/// Redaction for text that leaves the app, and the app log's directory.
// `redact`'s callers are behind the `dashboard-publish` feature.
#[cfg_attr(not(feature = "dashboard-publish"), allow(dead_code))]
mod feedback;
#[cfg(any(feature = "e2e-webdriver", test))]
mod harness_guard;
mod http;
mod install;
#[cfg(feature = "dhat-heap")]
pub mod leak_support;
/// The beta channels' update channel (SV-PBF-278: no license gate; static manifest).
/// Beta channels only; internal builds have none of it.
#[cfg(not(cs_channel = "internal"))]
mod license;
pub mod live_hub;
mod mcp;
mod memory_metrics;
mod menu;
mod models;
mod operations;
mod panes;
#[cfg(feature = "bench-support")]
pub mod perf_support;
mod principal_lifecycle;
mod renderer_recovery;
mod renderer_watchdog;
mod runtime_diagnostics;
/// SW-GW-4: the semantic **role** contract — the one finite thing the live
/// progress `phase` and the durable, team-configurable story `status` agree on.
/// A fixed `phase` maps to a `role` here; SW-SC-1 extends this module with the
/// data-driven `status → role` map so custom team states never break control flow.
pub mod roles;
/// Routines: git-synced standing-instruction definitions with machine-local
/// runtime state, armed only by their owner's app on one chosen host. The
/// `service` module is the single API the MCP tools and the UI wrap.
pub mod routines;
mod runtime;
#[cfg(feature = "sanitizer-canary")]
pub mod sanitizer_canary;
mod shared;
mod terminal;
/// SW-WV-B1: the `weft::` ledger substrate — the sole module that reads and
/// writes the `wv_*` tables in a single per-team `weft.sqlite3`, through the
/// sanctioned write path (one writer per record, live-max/AUTOINCREMENT id
/// minting, atomic audit trail). See [`weft`] for the load-bearing rules.
mod weft;

use log::LevelFilter;
use std::sync::{Arc, RwLock};
use tauri::{Emitter, Listener, Manager};
use tauri_plugin_log::{RotationStrategy, Target, TargetKind};

/// SF-3: Format a panic record into a multi-line string for stderr + log capture.
///
/// Extracted as a pure helper so the production hook (in `install_panic_hook`)
/// and the test capturing hook (in `panic_hook_tests`) share the exact same
/// output — avoids drift between what production emits and what the test asserts.
///
/// The backtrace section is prefixed with the literal label `"backtrace:"` so
/// the test can pin against a string this formatter owns, not against
/// `std::backtrace::Backtrace`'s Display impl (which has shifted across Rust
/// versions).
fn format_panic_record(
    thread_name: Option<&str>,
    payload: &str,
    location: Option<&std::panic::Location<'_>>,
    backtrace: &std::backtrace::Backtrace,
) -> String {
    let thread = thread_name.unwrap_or("<unnamed>");
    let loc = match location {
        Some(l) => format!("{}:{}:{}", l.file(), l.line(), l.column()),
        None => "<unknown location>".to_string(),
    };
    format!("panic on thread '{thread}' at {loc}: {payload}\nbacktrace:\n{backtrace}")
}

/// Where Rust log lines go. `Webview` mirrors them into the webview console via
/// `attachConsole`. Under `e2e-webdriver` the WDIO plugin forwards every webview
/// console line back INTO the Rust log, so keeping the webview sink there makes each
/// line re-enter the log it came from, nesting without bound (multi-GB logs, slowed
/// IPC). The harness build therefore omits it; release builds are unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogSink {
    LogDir,
    Stdout,
    Webview,
}

fn log_sinks() -> Vec<LogSink> {
    let mut sinks = vec![LogSink::LogDir, LogSink::Stdout];
    if !cfg!(feature = "e2e-webdriver") {
        sinks.push(LogSink::Webview);
    }
    sinks
}

fn log_targets() -> Vec<Target> {
    log_sinks()
        .into_iter()
        .map(|sink| {
            Target::new(match sink {
                LogSink::LogDir => TargetKind::LogDir { file_name: None },
                LogSink::Stdout => TargetKind::Dispatch(console_log::stdout_dispatch()),
                LogSink::Webview => TargetKind::Webview,
            })
        })
        .collect()
}

/// SF-3: Install a process-global panic hook that routes panic payloads +
/// backtraces to both stderr (dev terminal) and the rotated log file
/// (`~/Library/Logs/ai.codesense/CodeSense.log` via `tauri_plugin_log`'s
/// `LogDir` target).
///
/// Guarded by `Once` so re-entry under cargo-test setups is safe. Default
/// panic strategy is `unwind` (unchanged by this story); the hook fires for
/// both strategies.
///
/// Pre-builder caveat: until `tauri::Builder::build()` finishes wiring the
/// log plugin, `log::error!` is a no-op (no global logger installed yet).
/// In that window, stderr is the only capture. Post-build, both fire.
///
/// The hook must never panic itself: a panic inside a panic hook aborts the
/// process. Its stderr write therefore ignores failures (stderr is often a pipe
/// whose reader has already exited), and the console log target does the same.
fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            // Note: std aborts on any panic raised while the hook runs, even
            // inside this catch_unwind, so every step below must be panic-free
            // on its own (see the console_log closed-console tests).
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let thread = std::thread::current();
                let thread_name = thread.name();
                // payload() returns Box<dyn Any>; downcast to &str or String.
                let payload: &str = info
                    .payload()
                    .downcast_ref::<&'static str>()
                    .copied()
                    .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
                    .unwrap_or("<non-string panic payload>");
                let backtrace = std::backtrace::Backtrace::force_capture();
                let record = format_panic_record(thread_name, payload, info.location(), &backtrace);
                console_log::write_stderr_line(&record);
                log::error!(target: "panic", "{record}");
            }));
        }));
    });
}

/// Prevent macOS's global "Prefer tabs: Always" setting from implicitly
/// grouping CodeSense windows. This changes only automatic grouping; explicit
/// user tab actions remain available through AppKit.
#[cfg(any(target_os = "macos", test))]
const ALLOW_AUTOMATIC_WINDOW_TABBING: bool = false;

#[cfg(target_os = "macos")]
fn disable_automatic_window_tabbing() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSWindow;

    let Some(mtm) = MainThreadMarker::new() else {
        // This runs before the logging plugin exists, so stderr is the only
        // reliable diagnostic. Native setup must never block app launch.
        console_log::write_stderr_line(
            "unable to disable automatic macOS window tabbing: run() is not on the main thread",
        );
        return;
    };
    NSWindow::setAllowsAutomaticWindowTabbing(ALLOW_AUTOMATIC_WINDOW_TABBING, mtm);
}

/// Compile-time contract for unsupported platforms: native tabbing setup is a
/// no-op and introduces no AppKit symbols into their builds.
#[cfg(not(target_os = "macos"))]
fn disable_automatic_window_tabbing() {}

#[cfg(test)]
mod sync_no_auto_reverse_tests;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod automatic_window_tabbing_tests {
    use super::ALLOW_AUTOMATIC_WINDOW_TABBING;

    #[test]
    fn native_policy_disables_automatic_grouping() {
        const { assert!(!ALLOW_AUTOMATIC_WINDOW_TABBING) };
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn unsupported_platform_setup_is_a_noop() {
        super::disable_automatic_window_tabbing();
    }
}

/// The window focus handler (`WindowEvent::Focused`). SW-XS-4: a focus change
/// starts no sync work — the focus-regain reverse sweeps and the adaptive reverse
/// cadence they fed are gone (X1: no unattended writers). Kept as the named
/// stimulus seam the no-automatic-reverse tests (and SW-XS-12's guard) drive.
pub(crate) fn on_window_focus_changed<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    _focused: bool,
) {
}

/// The OSC-133 `CommandEnd` handler for a finished terminal command. SW-XS-4: a
/// finished command adopts nothing — the per-command reverse claim (and the cwd
/// read it needed) is gone (X1). Kept as the named stimulus seam the
/// no-automatic-reverse tests (and SW-XS-12's guard) drive.
pub(crate) fn on_osc133_command_end<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    _session_id: &str,
) {
}

/// The timers app setup starts for content sync. SW-XS-4: none — the reverse
/// drain loop and the entry / new-file backstop polls are gone, so no sync work
/// runs on a clock (X1). Kept as the named seam the no-automatic-reverse tests
/// drive under a paused clock (and SW-XS-12's guard roots on).
pub(crate) fn spawn_sync_timers<R: tauri::Runtime>(_app: &tauri::AppHandle<R>) {}

/// SW-GT-25: periodic audit-log compaction poll. On a slow fixed cadence, run one
/// [`should_compact`](commands::sprint_run::compaction::should_compact)-gated pass on
/// a `spawn_blocking` worker so the fold + rewrite of the run store never touch the
/// reactor tick or the webview thread (AC-4). The size-stat gate keeps each tick
/// near-free while the store is small (a fold only runs once it is past the size
/// threshold); a completed pass emits its reclaim metrics for dashboards (AC-7).
/// Deliberately its own slow fixed cadence — store
/// growth is slow and there is no latency SLA. Overlapping passes are harmless: the
/// shared store-mutation lock serializes them and each pass re-gates.
fn spawn_compaction_poll(handle: tauri::AppHandle) {
    const COMPACTION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(COMPACTION_POLL_INTERVAL).await;
            let handle = handle.clone();
            tauri::async_runtime::spawn_blocking(move || {
                commands::sprint_run::compaction::maybe_compact_default(|topic, payload| {
                    let _ = handle.emit(topic, payload);
                });
            });
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
fn connection_registry_dependencies_ready<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> bool {
    commands::pty_backend::in_process_host_ready(app)
        && app
            .try_state::<commands::local_profile::LocalProfileState>()
            .is_some()
        && app
            .try_state::<commands::guest_session::GuestSessionState>()
            .is_some()
        && app
            .try_state::<commands::host_connection_manager::HostConnectionManager>()
            .is_some()
        && app.try_state::<commands::pty_host::StreamIds>().is_some()
        && app
            .try_state::<codesense_pty_core::spawn::HostEnvironment>()
            .is_some()
        && app
            .try_state::<terminal::session_server::SessionServerState>()
            .is_some()
}

fn start_connection_registry_after_dependencies<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    start: impl FnOnce(),
) -> Result<(), String> {
    if !connection_registry_dependencies_ready(app) {
        return Err("connection registry dependencies were not managed before startup".into());
    }
    start();
    Ok(())
}

pub fn run() {
    // A WebDriver build started outside the test harness (e.g. relaunched by
    // macOS after a crash) must not touch the real profile or default port.
    #[cfg(feature = "e2e-webdriver")]
    harness_guard::exit_unless_harness_launch();

    // SF-3 AC-1: install the panic hook as the first statement of run() so
    // any downstream panic — including in plugin init or Tauri's builder —
    // is captured to stderr + the rotated log file.
    install_panic_hook();
    disable_automatic_window_tabbing();

    let log_level = std::env::var("CODESENSE_LOG")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) {
            LevelFilter::Debug
        } else {
            LevelFilter::Info
        });

    let builder = tauri::Builder::default();

    // macOS-only menu bar. Item clicks emit `menu:action` events with
    // the item id as payload — the frontend dispatches via the shared
    // action registry.
    #[cfg(target_os = "macos")]
    let builder = builder.menu(menu::build_menu).on_menu_event(menu::on_event);

    // SW-XS-4: focus changes start no sync work (see `on_window_focus_changed`).
    let builder = builder.on_window_event(|window, event| {
        if matches!(event, tauri::WindowEvent::Destroyed) {
            live_hub::drop_window(window.app_handle(), window.label());
        }
        if let tauri::WindowEvent::Focused(focused) = event {
            on_window_focus_changed(window.app_handle(), *focused);
        }

        // SW-AU-34: close-to-background. Closing the window keeps the process — and
        // thus the MCP host, the sync engine, and `ActiveTeamState` — alive, so a
        // running agent can keep driving codesense with no window on screen.
        // macOS-only: the model is dock-resident (Windows/Linux keep today's
        // quit-on-last-close until a real tray ships). Cmd-Q / menu-Quit fire
        // `RunEvent::ExitRequested`, NOT this event, so hiding on close can never
        // swallow a real quit — no "quitting" guard is needed. This arm is
        // deliberately thin: one lock-free atomic read plus native calls, with no
        // disk I/O, no IPC round-trip, and no blocking dialog, so window close never
        // stalls the UI thread. A true windowless daemon (GUI never launched) stays
        // out of scope — this only keeps an already-launched app alive past close.
        // Only the primary "main" window backgrounds the app; any secondary window
        // (none today) should close normally — this keeps the close arm consistent
        // with the "main"-targeted restore in the `Reopen` handler.
        #[cfg(target_os = "macos")]
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            if window.label() == "main" {
                use commands::background_mode::{close_action, quit_on_close, CloseAction};
                let app = window.app_handle();
                match close_action(quit_on_close()) {
                    CloseAction::Background => {
                        let _ = window.hide();
                        // NSApp `hide:` — drop off-screen and become App-Nap-eligible
                        // (the MCP socket stays responsive via I/O wakeups regardless).
                        let _ = app.hide();
                        api.prevent_close();
                        // One-time explainer: emit; the FE renders it once as a toast.
                        let _ =
                            app.emit(commands::background_mode::BACKGROUND_MODE_ENTERED_EVENT, ());
                    }
                    CloseAction::Quit => {
                        // On macOS closing the last window does not quit by default, so
                        // exit explicitly → `ExitRequested` → `app_exit_cleanup` teardown.
                        app.exit(0);
                    }
                }
            }
        }
    });

    let builder = builder
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log_level)
                // SW-PB-13: the updater plugin logs the whole update reply at debug, and that
                // reply holds the signed download URL. Capping its target at info keeps those
                // lines out of the log file (and so out of feedback logs) even in a debug build.
                // What it logs at error names only the update route and an HTTP or parse error;
                // the route carries no token, and the bearer header is never logged.
                .level_for("tauri_plugin_updater", LevelFilter::Info)
                // SW-DP-23: tungstenite traces every message it receives, and the dashboard link's
                // `i` frames carry what the owner types. Capped so typed bytes never reach the log.
                .level_for("tungstenite", LevelFilter::Info)
                .level_for("tokio_tungstenite", LevelFilter::Info)
                .rotation_strategy(RotationStrategy::KeepOne)
                .max_file_size(5_000_000)
                .format(|out, message, record| {
                    let level = record.level();
                    let target = record.target();
                    let timestamp = chrono::Local::now().format("%H:%M:%S");
                    let reset = "\x1b[0m";
                    // Level color for timestamp + level prefix
                    let level_color = match level {
                        log::Level::Error => "\x1b[41;97;1m", // white on red bg, bold — unmissable
                        log::Level::Warn  => "\x1b[1;33m",    // bold yellow
                        log::Level::Info  => "\x1b[32m",       // green
                        log::Level::Debug => "\x1b[36m",       // cyan
                        log::Level::Trace => "\x1b[2m",        // dim
                    };
                    // Per-target color for subsystem tag — stable hash to ANSI color
                    let target_color = match target {
                        "auth"    => "\x1b[35m",    // magenta
                        "github"  => "\x1b[34m",    // blue
                        "jira"    => "\x1b[94m",    // bright blue
                        "git"     => "\x1b[96m",    // bright cyan
                        "update"  => "\x1b[93m",    // bright yellow
                        "session" => "\x1b[95m",    // bright magenta
                        "pty"     => "\x1b[33m",    // yellow
                        "watch"   => "\x1b[92m",    // bright green
                        "system"  => "\x1b[37m",    // white
                        "deps"    => "\x1b[91m",    // bright red
                        "files"   => "\x1b[34m",    // blue
                        "config"  => "\x1b[36m",    // cyan
                        "install" => "\x1b[32m",    // green
                        "meta"    => "\x1b[94m",    // bright blue
                        "verify"  => "\x1b[92m",    // bright green
                        "tools"   => "\x1b[93m",    // bright yellow
                        "http"    => "\x1b[96m",    // bright cyan
                        "memory"  => "\x1b[38;5;208m", // orange — RF-perf-5 gated memory samples
                        _         => "\x1b[37m",    // white fallback
                    };
                    out.finish(format_args!(
                        "{level_color}{timestamp} [{level:<5}]{reset} {target_color}[{target}]{reset} {message}"
                    ));
                    // Architecture: heartbeat only after all logger sinks finish.
                    runtime_diagnostics::logger_beat();
                })
                .targets(log_targets())
                .build(),
        );

    // The production logger must initialize first: the WDIO execute plugin
    // detects and reuses an installed logger, while the inverse order makes the
    // production log plugin reject a second global logger during app startup.
    #[cfg(feature = "e2e-webdriver")]
    let builder = builder
        .plugin(tauri_plugin_wdio::init())
        .plugin(tauri_plugin_wdio_webdriver::init());

    let builder = builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init());
    // The updater plugin, in every channel. Internal builds drive it through the GitHub
    // commands (`commands/update.rs`, internal-only). Beta builds drive it only from
    // `license/updates.rs`, against the channel's static manifest (SV-PBF-278). The webview
    // gets no updater permission in either.
    let builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
    builder
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_notification::init())
        .manage(verbs::commands::VerbState::default())
        .manage(panes::commands::CommandRegistry::default())
        .manage(commands::watch::WatcherState::default())
        .manage(commands::host_connection_manager::HostConnectionManager::default())
        .manage(terminal::Osc133Bus::default())
        // SW-WV-A4: the OSC-133 broadcast relay `wv_wait` takes per-call receivers
        // from (armed against the bus in `.setup()` below), and the silence-watchdog
        // registry the `signal_done` watchdog-cancel hook cancels through.
        .manage(weft::WeftOsc133Relay::default())
        .manage(weft::WeftWatchdogRegistry::default())
        .manage(terminal::warm_pool::WarmPoolState::default())
        .manage(commands::claude_session::SessionWatcherState::default())
        .manage(commands::update::UpdateState::default())
        .manage(commands::worktree::WorktreeRunRegistry::new())
        .manage(codesense_fs_core::watch::WorktreeWatchState::default())
        // DESIGN-remote-source.md §3.3 — polling-task registry for remote
        // worktree watchers, one entry per `source_remote_watch_start` token.
        .manage(commands::remote_source::RemoteWatchState::default())
        // SW-SD-2: agent-runner tunables (model/argv-template/timeout). Tunables
        // only — the binary is resolved per-call via `resolve_agent_for_launch`.
        .manage(commands::agent_runner::AgentRunnerTunables::default())
        // SW-DX-11: the dedicated `run_skill` concurrency cap — separate budget
        // from the openWorkspace cap and `analyze`'s single-flight.
        .manage(commands::agent_runner::RunSkillCap::default())
        // SW-DX-3: the live-status-stream hub (broadcast channel + per-root
        // snapshot cache + scoped SSE token store). Managed so the file-watch
        // handler, the MCP agent verbs, and the SSE route all reach the one hub.
        .manage(dashboard_stream::DashboardStreamHub::new())
        .manage(std::sync::Arc::new(dashboard_stream::frame::FrameStore::default()))
        // SW-DX-7: the dashboard execution-loop registry (SpawnCap + storyId↔sessionId).
        .manage(commands::dashboard_launch::DashboardLaunchState::default())
        // SW-WA-F11: per-session launch telemetry — in-flight registration for the
        // output pipeline's first-byte hook, plus the latest finalized record each
        // `list_sessions` row surfaces.
        .manage(commands::launch_telemetry::LaunchTelemetryTracker::new())
        // SW-WC-1: the per-workspace archive-flag registry — one nullable
        // `archived_at_ms` field keyed by viewer workspace id, persisted to
        // `~/.codesense/workspace-registry.json` (atomic write; load-or-default).
        // `install()` runs below in `.setup()` once `codesense_home_dir()` is known.
        .manage(commands::workspace_registry::WorkspaceRegistryState::default())
        // One shared workspace log and its persisted reader positions.
        // Installation waits for the CodeSense home directory below.
        .manage(commands::workspace_chat::WorkspaceChatStore::default())
        .manage(commands::agent_usage::AgentUsageState::default())
        .manage(commands::agent_accounts::AgentAccountRuntimeState::default())
        // SW-FX-64: renderer-liveness state for the webview watchdog. Written by
        // the `renderer_heartbeat` command (FE ack), read by the watchdog task.
        .manage(renderer_watchdog::RendererLiveness::default())
        .manage(mcp::state::AskUserRegistry::new())
        .setup(move |app| {
            live_hub::start(app.handle());
            log::info!(target: "system", "CodeSense v{} starting (log_level={:?}, platform={})",
                env!("CARGO_PKG_VERSION"), log_level, std::env::consts::OS);

            // SW-PB-22f-3: keep published dashboards in step with their folders.
            #[cfg(feature = "dashboard-publish")]
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let Ok(data_dir) = handle.path().app_data_dir() else {
                        return;
                    };
                    let mut driver = crate::dashboard_publish::driver::Driver::open(&data_dir).with_app(handle.clone());
                    let mut every = tokio::time::interval(std::time::Duration::from_secs(5));
                    // After a long push, wait the full interval rather than catching up.
                    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        every.tick().await;
                        driver.tick(std::time::Instant::now()).await;
                    }
                });
                // SW-DP-4c: the live link to the dashboard host, held while a
                // published dashboard has its live block on.
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let Ok(data_dir) = handle.path().app_data_dir() else {
                        return;
                    };
                    crate::dashboard_publish::live::run(&data_dir, Some(handle)).await;
                });
            }

            // The in-process terminal host: the PTY session registry and the
            // terminal layout it owns. Every runtime side effect flows back
            // through the one app sink; reaps run on the app's blocking pool.
            let local_sink = Arc::new(commands::pty_host::AppTerminalSink::new(
                app.handle().clone(), commands::pty_host::StreamHost::InProcess,
            ));
            let in_process_host = Arc::new(codesense_pty_core::TerminalHost::new(
                commands::pty_host::local_server_identity(),
                local_sink.clone(),
                tauri::async_runtime::handle().inner().clone(),
            ));
            // This host and its observational listener share the app lifetime.
            // A sidecar has its own independently retained activity owner.
            if let Some(home) = commands::paths::codesense_home_dir() {
                // SW-WC-1: load the archive-flag registry off disk before any FE
                // read lands. A missing / corrupt file is logged and treated as
                // empty so the app always boots.
                app.state::<commands::workspace_registry::WorkspaceRegistryState>()
                    .install(&home);
                // SW-WC-2: install the chat engine's on-disk root (log + index).
                // Same "load-or-default" discipline: a missing or corrupt index
                // boots empty; a tail scan reconciles against the log on open.
                app.state::<commands::workspace_chat::WorkspaceChatStore>()
                    .install(&home);
                use std::os::unix::fs::DirBuilderExt;
                let root = home.join("in-process-activity");
                let created = std::fs::DirBuilder::new().mode(0o700).create(&root);
                if created.is_ok() || created.as_ref().is_err_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists) {
                    match codesense_pty_core::activity::ActivityService::start(&root, in_process_host.activity()) {
                        Ok(service) => { app.manage(service); }
                        Err(error) => log::warn!("in-process activity reporting unavailable: {error}"),
                    }
                }
            }
            app.manage(in_process_host);
            commands::pty_backend::start_in_process_emulator(app.handle());
            // The viewport streams of every host, under client-unique ids.
            app.manage(commands::pty_host::StreamIds::default());
            // What the in-process host resolves spawns against; the sidecar is
            // handed the same resources when it is launched.
            app.manage(commands::pty_backend::in_process_environment(app.handle()));
            // The connection to the session-server sidecar. Resolving the
            // backend once, off the setup thread, connects to (or launches) the
            // sidecar so the first terminal does not wait for it and the
            // sessions a previous run left there are reachable. An incompatible
            // sidecar is logged there and reported again by the first terminal
            // it refuses.
            let local_sessions = terminal::session_server::SessionServerState::at(
                commands::paths::codesense_home_dir(),
            );
            let identity = commands::pty_backend::in_process_identity(app.handle())
                .map_err(std::io::Error::other)?;
            let namespace = terminal::session_server::mint_event_namespace(
                identity.server.id.as_str(), &identity.epoch,
            ).map_err(|error| std::io::Error::other(error.to_string()))?;
            local_sessions.admission.register_in_process(&namespace)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            local_sink.bind_namespace(namespace);
            let sidecar_handle = app.handle().clone();
            local_sessions.admission.set_sink_factory(Arc::new(move |namespace| {
                let sink = commands::pty_host::AppTerminalSink::new(
                    sidecar_handle.clone(), commands::pty_host::StreamHost::LocalSidecar(namespace.clone()),
                );
                sink.bind_namespace(namespace.clone());
                Arc::new(sink)
            }));
            let admission_handle = app.handle().clone();
            local_sessions.admission.subscribe(Arc::new(move |row| {
                if let Err(error) = admission_handle.emit("terminal-layout-admission-changed", row) {
                    log::warn!(target: "pty", "emit local runtime admission: {error}");
                }
            }));
            app.manage(local_sessions);
            commands::pty_backend::reconnect_on_loss(app.handle());
            {
                let handle = app.handle().clone();
                let resolved = std::thread::Builder::new()
                    .name("pty-server-connect".into())
                    .spawn(move || {
                        let _ = commands::pty_backend::resolve_terminal_backend(
                            &handle,
                            commands::machines::LOCAL_MACHINE_ID,
                        );
                        // Per-launch agent settings files left by sessions that ended while
                        // the app was closed, or by a crash. Only once the terminal hosts
                        // can say which sessions still run: a session the session server
                        // kept alive keeps its file. Best-effort.
                        let alive = |sessions: &[&str]| {
                            match commands::pty_backend::alive_session_ids(&handle, sessions) {
                                commands::pty_backend::Liveness::Known(live) => Some(live),
                                commands::pty_backend::Liveness::Unknown => None,
                            }
                        };
                        crate::agents::launch::sweep_stale_launch_settings(alive);
                        // The per-session config dirs (MCP config, launch settings, a scoped
                        // Codex home) of sessions that are gone, by the same liveness.
                        crate::commands::mcp_scope::sweep_stale_configs(alive);
                        session_conversation::start_retention_sweep();
                    });
                if let Err(e) = resolved {
                    log::warn!(target: "pty", "session server connect thread failed: {e}");
                }
            }

            // The harness build keeps its window above other windows: WKWebView
            // marks an occluded page hidden and stops rendering, which makes
            // every terminal capture blank or frozen.
            #[cfg(feature = "e2e-webdriver")]
            if let Some(window) = app.get_webview_window("main") {
                if let Err(e) = window.set_always_on_top(true) {
                    log::warn!(target: "system", "e2e: could not pin window on top: {e}");
                }
            }

            // Idempotent relocation of v3 lockfiles out of the context-repo
            // clone (`orgs/{slug}/_codesense/installs/`) into the
            // machine-local `users/{uid}/installs/{slug}/` root. Must run
            // BEFORE the first wave so consumers see the new layout. Moves
            // files (preserving materialization baselines); markerless and
            // self-disabling once the legacy dir is drained. Failure is
            // logged but never aborts startup.
            match crate::commands::paths::codesense_home_dir() {
                Some(home) => {
                    if let Err(e) =
                        content_engine::lockfile::relocate_install_state_out_of_clone(&home)
                    {
                        log::error!(
                            target: "content_engine::lockfile",
                            "install-state relocation failed: {} — legacy lockfiles left in place, will retry next boot",
                            e
                        );
                    }
                }
                None => log::warn!(
                    target: "content_engine::lockfile",
                    "could not resolve codesense home dir; install-state relocation skipped (HOME / USERPROFILE unset?)"
                ),
            }

            // SW-IS-3c: delete the retired per-file JSON lockfiles now that the store
            // is the authoritative substrate. The deleter self-gates on the
            // backfill-complete marker, so the JSON is removed only once the store has
            // been seeded from it (never before — that would lose install state).
            // A first-boot-after-upgrade delete of the whole JSON tree is heavy and has
            // NO downstream `.setup()` consumer (nothing reads the deleted JSON
            // post-3c), so run it OFF the setup critical path on the blocking pool —
            // startup stays snappy (no stall, no terminal spinner) — mirroring the
            // SLW-IN-6 orphan prune above. Idempotent + best-effort: logged, never
            // aborts startup, retried next boot. (`relocate_install_state_out_of_clone`
            // above stays inline: it is cheap same-volume dir moves, self-disabling
            // once drained, and must precede the backfill that seeds the store.)
            match crate::commands::paths::codesense_home_dir() {
                Some(home) => {
                    tauri::async_runtime::spawn_blocking(move || {
                        if let Err(e) =
                            content_engine::lockfile::prune_install_lockfiles_at_boot(&home)
                        {
                            log::error!(
                                target: "content_engine::lockfile",
                                "install lockfile boot deletion failed: {} — JSON left in place, will retry next boot",
                                e
                            );
                        }
                    });
                }
                None => log::warn!(
                    target: "content_engine::lockfile",
                    "could not resolve codesense home dir; JSON lockfile deletion skipped (HOME / USERPROFILE unset?)"
                ),
            }

            let handle = app.handle().clone();
            // SW-CN-4: build with empty in-memory state (non-blocking, no
            // keychain I/O) so `.setup()` never blocks first paint, then load +
            // migrate the keychain blob on a background thread (AC-8, mirrors
            // SW-OB-12). Status reads (`get_auth_status`/`list_connections`)
            // `await` the load rather than observe the empty state, so the UI
            // stays on the splash until the real status resolves — never a
            // logged-out flash.
            let profile_home = commands::paths::codesense_home_dir().ok_or("Local profile home unavailable")?;
            app.manage(commands::local_profile::LocalProfileState::load(&profile_home)?);
            commands::local_profile::LocalProfileState::start_import(handle.clone());
            app.manage(commands::guest_session::GuestSessionState::load(commands::paths::codesense_home_dir().as_deref()));
            // Local HTML query/routine capabilities remain independent of service access.
            // Registry observation may immediately schedule auto-connect, so
            // every terminal/session and ambient-profile dependency must already be
            // managed before the observer task starts.
            {
                let handle = app.handle().clone();
                let registry_handle = handle.clone();
                start_connection_registry_after_dependencies(&handle, || {
                    let event_handle = registry_handle.clone();
                    app.state::<commands::host_connection_manager::HostConnectionManager>()
                        .subscribe(Arc::new(move |event| {
                            commands::machines::forward_manager_event(&event_handle, &event);
                        }));
                    // SW-AGR-14: no App Nap while machines are connected.
                    commands::app_nap::install(
                        &app.state::<commands::host_connection_manager::HostConnectionManager>(),
                    );
                    // Pulls each connected machine's agent usage into the usage store.
                    commands::remote_usage_pull::install(app.handle().clone());
                    // SW-AGR-9: the delivery core, and its catch-up on every
                    // (re)connect of a terminal server, local or remote.
                    delivery::app::install(&registry_handle);
                    routines::wake_app::install(&registry_handle);
                    // SW-AGR-8: routines armed on machines.
                    routines::machine_arm_app::install(&registry_handle);
                    // InboxWaitV1: every tab's inbox notices go through its host.
                    commands::host_inbox::install(&registry_handle);
                    session_usage::spend_caps::install(registry_handle.clone());
                    // SV-PBF-287: the story totals index's worker.
                    session_usage::story_totals::install(registry_handle.clone());
                    let spawn_handle = registry_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) =
                            commands::machines::sync_connection_manager(&spawn_handle).await
                        {
                            log::warn!(target: "machines", "start connection registry: {error}");
                        }
                    });
                })?;
            }
            app.manage(http::ProxyCapabilityState::new_random());
            app.manage(commands::deps::DependencyManager::resolve());
            commands::deps::start_dep_watching(app.handle().clone());
            // SW-OB-12: if the cheap startup fast-path missed the bundled agent,
            // resolve it via a login-shell PATH probe on a background thread so
            // `.setup()` (and first paint) never blocks. Emits `dep-status-changed`
            // on completion; the frontend gate stays on the splash while resolving.
            commands::deps::spawn_agent_background_resolve(app.handle().clone());
            app.manage(mcp::state::McpViewerState::default());
            app.manage(mcp::state::ActiveTeamState::default());
            // SW-CN-26: the per-team connector resolution cache, keyed by the
            // same `(org, team)` identity ActiveTeamState carries. Populated off
            // the runtime and evicted on active-team switch / materialize / local
            // write, so the async proxy hot path is a lock-read only.
            // SW-CN-UX-23: the durable per-team baseline the promotion needs-credential
            // diff reads its "before" set from (outside the resolve cache above, which is
            // evicted on every wave — see the type doc).
            app.manage(mcp::state::CanvasRenderRegistry::new());
            // SW-FX-131: the viewer's verdict on an `open_workspace` machine open.
            app.manage(std::sync::Arc::new(
                commands::workspace_open::MachineOpenRegistry::default(),
            ));
            app.manage(mcp::state::RenderedDocumentRegistry::new());
            // SLW-RI-3: published as `RwLock<Arc<_>>` (a snapshot pointer) so
            // readers clone the inner `Arc` and run heavy work off-lock; writers
            // RCU. Type alias: `SharedFrontmatterCache`.
            let shared_frontmatter: install::frontmatter_cache::SharedFrontmatterCache =
                Arc::new(RwLock::new(Arc::new(
                    install::frontmatter_cache::FrontmatterCache::new(),
                )));
            install::frontmatter_cache::FrontmatterCache::register_global(&shared_frontmatter);
            app.manage(shared_frontmatter);
            // SW-OC-26: the footprint-wide context index (every synced org's
            // context files, the private space, all-orgs content). Built off the UI
            // thread by its own worker; queries read an immutable snapshot.
            app.manage(context_query::ContextIndexService::new());
            context_query::start_for_app(app.handle());

            // SW-AU-51: cross-org story resolver. Additive to the active-org
            // frontmatter cache above — the active org delegates to it; other
            // synced orgs are lazily built + held here so fire paths resolve a
            // story in ANY synced org (see `org_story_resolver`). Managed as an
            // `Arc` so async fire paths can offload the first-touch cold build onto
            // the blocking pool via `await_blocking` (AC-7 — never a runtime worker).
            app.manage(std::sync::Arc::new(
                install::org_story_resolver::OrgStoryResolver::new(),
            ));

            // SS-4c: WaveCoordinator for async fan-out + per-target keyed wave lock.
            // Registered inside setup() (not the pre-setup .manage() chain)
            // because the wave engine's WaveDeps::from_app reads WatcherState
            // (registered pre-setup at line 165) and depends on AuthManager-
            // backed state. Single source-of-truth for cross-repo wave
            // serialization (per-target keyed locks; SLW-RI-3 retired the old
            // global SWITCH_LOCK).
            app.manage(Arc::new(
                content_engine::sync::WaveCoordinator::default(),
            ));

            // SLW-RI-1: build + inject the dedicated content/sync ENGINE runtime
            // so wave execution runs on its own bounded worker pool, structurally
            // isolated from Tauri's command-dispatch runtime — a heavy wave can
            // no longer starve terminal commands (the SLW-FX-7 freeze class).
            // The coordinator gets a Handle the same way it gets the live filter
            // above (the D-17 seam), so `WaveCoordinator::new()`/`default()`
            // stays runtime-free for tests. Ownership of the Runtime lives in an
            // `EngineRuntimeGuard` in managed state (NOT on the coordinator): a
            // wave task captures the coordinator, and a Handle doesn't keep the
            // runtime alive, so a finishing wave can never drop the runtime from
            // its own worker thread (the load-bearing invariant — see
            // `EngineRuntimeGuard`). The guard's `Drop` does a bounded
            // `shutdown_background` IF managed state is torn down; on a normal
            // macOS/Windows quit tao `process::exit`s and destructors don't run,
            // which is itself trivially bounded. A failed build is fatal at
            // startup — the engine can't run without it.
            {
                let engine = content_engine::sync::build_engine_runtime()
                    .expect("build cs-engine engine runtime");
                app.state::<Arc<content_engine::sync::WaveCoordinator>>()
                    .set_engine_handle(engine.handle().clone());
                app.manage(content_engine::sync::EngineRuntimeGuard::new(engine));
            }

            if let Some(home) = commands::paths::codesense_home_dir() {
                attribution::install::initialize(app.handle(), &home, std::env::var_os("PATH").as_deref());
                attribution::install::ensure_installed(
                    &home,
                    std::env::var_os("PATH").as_deref(),
                    attribution::install::Trigger::Startup,
                );
                attribution::watcher::start(app.handle(), home);
            }

            // Start MCP server on Unix domain socket
            mcp::start_mcp_server(app.handle().clone());

            // Ensure `~/.codesense/bin/codesense-bridge` points at the bundled
            // bridge, so the `.mcp.json` CodeSense writes for an agent names a
            // stable path. Failure is logged but does not abort startup.
            if let Err(e) = commands::deps::ensure_bridge_symlink(app.handle()) {
                log::warn!(
                    target: "deps",
                    "ensure_bridge_symlink failed: {} — agent MCP configs fall back to the bundled bridge path",
                    e
                );
            }

            // Start HTTP API server on localhost
            http::start_http_server(app.handle().clone());

            // SS-FU-17 AC-5: process-space liveness heartbeat. Crash-honest
            // (staleness-based) machine-readable "is codesense running?"
            // signal under ~/.codesense/runtime/ — never a user repo.
            runtime::start_heartbeat();

            // SW-FX-64 Layer 1: renderer-liveness watchdog. Distinct from the
            // process heartbeat above — an OOM/wedged WKWebView renderer leaves
            // this Rust process healthy while the window goes white. The watchdog
            // reads the FE ack (`renderer_heartbeat`) and reloads the webview if
            // the renderer stops acking, so a content-process death self-recovers
            // instead of blanking until manual restart.
            renderer_watchdog::start_renderer_watchdog(app.handle().clone());
            if let Err(error) = runtime_diagnostics::start(app.handle()) {
                log::warn!(target: "system", "runtime self-capture unavailable: {error}");
            }

            // PT-8: store the app handle + start the warm-pool TTL reaper, and
            // wire the OSC 133 prompt-start listener so a warming pool member
            // flips to Ready on its first prompt (AC-2). The pool starts empty;
            // the frontend fills it via `reconfigure_warm_pool` once settings
            // hydrate (D8/AC-1), so nothing is spawned here.
            {
                let handle = app.handle().clone();
                app.state::<terminal::warm_pool::WarmPoolState>()
                    .init(std::sync::Arc::new(
                        crate::commands::pty_host::AppWarmPoolHost::new(handle.clone()),
                    ));
                // PT-8 TTL reaper: recycle stale members on an interval. Owns the
                // app handle so it can re-enter the managed pool state each sweep —
                // the pool itself is Tauri-free (OC-10), so this loop lives here.
                {
                    let handle = handle.clone();
                    let reaper = std::thread::Builder::new()
                        .name("warm-pool-ttl".into())
                        .spawn(move || loop {
                            std::thread::sleep(terminal::warm_pool::TTL_SWEEP_INTERVAL);
                            if let Some(pool) =
                                handle.try_state::<terminal::warm_pool::WarmPoolState>()
                            {
                                pool.recycle_stale();
                            }
                        });
                    if let Err(e) = reaper {
                        // Non-fatal, but observable: without the reaper, stale pool
                        // members are never recycled for the process lifetime.
                        log::error!(target: "warm_pool", "TTL reaper failed to spawn: {e}");
                    }
                }
                // SW-WS-1: boot reaper for `move_workspace` intents. Scans
                // `~/.codesense/users/{principal}/move-workspace-intents-v1/`
                // and advances each open intent to a terminal state (completed
                // OR rolled back). Non-blocking boot — spawned as a background
                // Tokio task after the app handle + MCP viewer state are
                // initialized. Per-intent isolation inside the reaper so one
                // failure doesn't abort the pass.
                {
                    let handle = handle.clone();
                    tauri::async_runtime::spawn(async move {
                        crate::mcp::move_workspace::reap_move_workspace_intents(handle).await;
                    });
                }
                // SW-WV-A4: arm the ONE permanent Osc133 → broadcast relay subscriber
                // so per-call `wv_wait` receivers can observe `CommandEnd` markers
                // without each registering a leaked bus listener. Synchronous, ns-budget
                // fan-out into the managed relay's broadcast sender (no I/O in-callback).
                {
                    let relay_tx = app.state::<weft::WeftOsc133Relay>().sender();
                    app.state::<terminal::Osc133Bus>()
                        .subscribe(Box::new(move |ev| {
                            let _ = relay_tx.send(ev.clone());
                        }));
                }
                app.state::<terminal::Osc133Bus>()
                    .subscribe(Box::new(move |ev| {
                        match ev.kind {
                            // PT-8: flip a warming pool member to Ready on its
                            // first prompt.
                            terminal::Osc133Kind::PromptStart => {
                                if let Some(pool) =
                                    handle.try_state::<terminal::warm_pool::WarmPoolState>()
                                {
                                    pool.mark_ready(&ev.session_id);
                                }
                            }
                            // SW-XS-4: a finished command adopts nothing (see
                            // `on_osc133_command_end`).
                            terminal::Osc133Kind::CommandEnd => {
                                on_osc133_command_end(&handle, &ev.session_id);
                            }
                            terminal::Osc133Kind::CommandStart => {}
                        }
                    }));
            }

            // Every agent write, from the tab or an agent's MCP tools, tells the app.
            agents::events::install_app_emitter(app.handle().clone());
            // Routines run on their own scheduler thread and their own agent-done intake. The
            // intake offers each finished session to routines first, then to its registered
            // consumers; they register before it starts so none misses an early signal.
            routines::scheduler::spawn_scheduler(app.handle().clone());
            agents::session_events::spawn_session_status_producer(app.handle().clone());

            // Start the claim-driven dispatcher — the single consumer that drains the durable
            // runnable queue routine fires enqueue, executing each run exactly once, and re-drives
            // work recovered from a prior crash/quit on startup. Its own async task, all store I/O
            // off-thread (no jank).
            commands::sprint_run::claim_loop::spawn_claim_loop(app.handle().clone());

            // SW-XS-4: no sync work runs on a timer (see `spawn_sync_timers`).
            spawn_sync_timers(app.handle());

            // SW-GT-25: the audit-log compaction poll — bound the machine-local
            // run/verdict store over a long-lived pipeline. Each slow tick cheaply
            // size-gates before any fold, then compacts on a blocking worker only
            // when the store is genuinely large and dirty; a failed pass is logged
            // and skipped, never fatal (the store stays fully usable).
            spawn_compaction_poll(app.handle().clone());

            // Keep each running session's role registrations alive and release those
            // of sessions that ended.
            app.manage(commands::role_inbox::RoleRenewalStores::default());
            commands::role_inbox::spawn_role_renewer(app.handle().clone());

            // SW-FX-launcher-persistence: wire the launch-registry file for the
            // signed-in principal, so `launched_by` survives sleep / wake / app
            // restart. Keyed by local profile only (not the active team) so an
            // "all teams" launcher map is captured for the user. Runs as a
            // background task so the setup block never blocks on the auth
            // resolver; if the user is signed out, nothing is installed and
            // the registry stays in its pre-fix behavior (empty on each start).
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let (codesense_home, user_id) =
                        match commands::route_to_context::resolve_ambient(&handle).await {
                            Ok(result) => result,
                            Err(error) => {
                                log::info!(
                                    target: "agents",
                                    "launch registry: skipping persistence install ({error})",
                                );
                                return;
                            }
                        };
                    let principal = crate::domain::Principal::from(user_id);
                    let path = commands::dashboard_launch::persistence_registry_path(
                        &codesense_home,
                        &principal,
                    );
                    handle
                        .state::<commands::dashboard_launch::DashboardLaunchState>()
                        .install_persistence(path);
                });
            }

            #[cfg(not(cs_channel = "internal"))]
            license::install(app.handle()).map_err(std::io::Error::other)?;

            log::info!(target: "system", "startup complete — all managers initialized");
            Ok(())
        })
        .append_invoke_initialization_script(app_channel::init_script())
        .invoke_handler(shared::invoke_stats::counted(tauri::generate_handler![
            records::commands::records_call,
            records::commands::records_prepare_view,
            records::commands::records_prepare_native_view,
            verbs::commands::verb_dispatch,
            verbs::commands::verb_answer,
            verbs::commands::verb_settle,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::control::dashboard_control_links,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::management::dashboard_detail,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::management::dashboard_move_link,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::management::dashboard_publish,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::management::dashboard_unpublish,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::management::dashboard_share,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::confirm::dashboard_live_requests,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::confirm::dashboard_live_confirm,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::confirm::dashboard_live_decline,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::confirm::dashboard_live_turn_off,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::management::dashboard_viewer_sign_out,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::inventory::dashboard_inventory,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::keys::dashboard_publisher_key_status,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::keys::dashboard_publisher_key_save,
            #[cfg(feature = "dashboard-publish")]
            dashboard_publish::keys::dashboard_publisher_key_remove,
            #[cfg(not(cs_channel = "internal"))]
            license::commands::license_state,
            #[cfg(not(cs_channel = "internal"))]
            license::commands::license_check_now,
            #[cfg(not(cs_channel = "internal"))]
            license::commands::license_restart_to_update,
            #[cfg(not(cs_channel = "internal"))]
            license::commands::license_cancel_restart,
            #[cfg(not(cs_channel = "internal"))]
            license::commands::license_headless_runs,
            commands::install::init_watched_cache,
            commands::content_engine::set_active_team_for_wave,
            commands::content_engine::ensure_epic_workspace,
            commands::content_engine::ensure_branch_workspace,
            commands::content_engine::assemble_epic_worktrees,
            commands::content_engine::reconcile_epic_worktrees,
            commands::content_engine::establish_worktree_story_link,
            commands::content_engine::reconcile_worktrees_on_launch,
            commands::content_engine::reconcile_stuck_worktrees,
            commands::system::save_app_state,
            commands::agent_list_preferences::agent_list_identity,
            commands::agent_list_preferences::agent_list_refresh,
            commands::agent_list_preferences::agent_list_set_identity,
            commands::system::load_app_state,
            // SW-WV-B3: sibling-ledger Tauri commands (roster is Tauri-only; settle +
            // watermark are also MCP verbs).
            commands::weft_ledger::wv_upsert_roster,
            // QH weft-e2e ONLY: seed a pending assertion + YOU-OWE attention row for the
            // real-app endorsement-gate proof, and the dispatch-record + dispatch-status
            // roster row for the RT1 done-hook proof (both refused unless CODESENSE_E2E=1).
            commands::weft_ledger::wv_e2e_seed_endorsement,
            commands::weft_ledger::wv_e2e_seed_dispatch,
            // SW-WA-E7: the three Stage-2 "it fails on purpose" red-path drivers — a
            // worker smuggling an above-tier verb is refused at the tier authority, an
            // unverified dispatch cannot fold, and an evidence-free/skill-install gate
            // criterion is refused at the write boundary. Each drives its real seam and
            // returns the typed refusal (refused unless CODESENSE_E2E=1).
            commands::weft_ledger::wv_e2e_drive_fold,
            // SW-WA-F4: drive the open→run→read round-trip through the REAL F1/F2/F3
            // MCP verb methods (open_terminal_tab → run_in_terminal_tab →
            // read_terminal_tab) plus the forged-target red path, so the capstone's
            // Act 3 operator round-trip is provable through the shipped verbs in the
            // real-app weft-e2e harness. Split open+run (one shot) from read (polled,
            // because the ls output reaches the emulator async). Refused unless
            // CODESENSE_E2E=1.
            mcp::handler::wv_e2e_terminal_open_run,
            mcp::handler::wv_e2e_terminal_read,
            commands::weft_ledger::wv_deprovision_roster,
            commands::weft_ledger::wv_list_roster,
            commands::weft_ledger::wv_settle_approval,
            commands::weft_ledger::wv_upsert_watermark,
            // SW-WV-A3: worker-lifecycle transition Tauri commands.
            commands::weft_ledger::wv_worker_handoff,
            commands::weft_ledger::wv_worker_deprovision,
            // SW-WV-B5: scope-layer Tauri commands (scopes + watches).
            commands::weft_scopes::cmd_wv_scope_get,
            commands::weft_scopes::cmd_wv_scope_create,
            commands::weft_scopes::cmd_wv_scope_update_artifact_ptr,
            commands::weft_scopes::cmd_wv_scope_close,
            commands::weft_scopes::cmd_wv_watch_upsert,
            commands::weft_scopes::cmd_wv_watch_close,
            commands::weft_scopes::cmd_wv_watch_mark_quiet,
            commands::weft_scopes::cmd_wv_watch_list,
            // SW-WV-C6: decision-harvest-norm Tauri commands (controller-tier).
            commands::weft_ledger::wv_set_harvest_artifact,
            commands::weft_ledger::wv_approve_harvest,
            commands::weft_ledger::wv_mark_harvest_not_required,
            commands::weft_ledger::wv_list_overdue_harvests,
            // SW-WV-E1: endorsement-gate Tauri commands — recipient-ack + owner
            // ratify/revoke. Tauri-only (human UI boundary); NEVER MCP verbs.
            commands::weft_ledger::wv_record_recipient_ack,
            commands::weft_ledger::wv_ratify,
            commands::weft_ledger::wv_revoke,
            // SW-WV-F4: read pending assertions for the review-at-altitude header.
            commands::weft_ledger::wv_read_pending_assertions,
            // SW-WV-F5: the YOU-OWE endorsement surface — pending T2/T3 approvals read
            // + the surfaced-item guard arm (FE half of the E3 window).
            commands::weft_ledger::wv_list_pending_approvals,
            commands::weft_ledger::wv_mark_approval_surfaced,
            // SW-WV-F1: the Weft Board read command (attention board).
            commands::weft_ledger::wv_list_attention_board,
            // SW-WV-F2: the dispatch preflight (queue-row / capacity → one verdict).
            weft::preflight::preflight_wv_dispatch,
            // SW-WV-F7: the FE dispatch bridge for the weft-board dispatch surface (a
            // thin door over the C1 dispatch core; the M1 first-click seam).
            weft::dispatch_bridge::wv_dispatch_worker,
            // SW-WV-RT1: the signal-done bridge — a Tauri door onto the same
            // `signal_done` hook registry the MCP verb dispatches, so a worker
            // completion (roster advance + done-record + handoff + stamp) is reachable
            // from the UI and the QH harness, not only the MCP protocol path.
            weft::dispatch_bridge::wv_signal_done_bridge,
            // SW-WS-1: FE bridge for `move_workspace` and `list_move_intents`.
            // The workspace-header dialog invokes `move_workspace_bridge`;
            // the viewer-store's init effect calls `list_move_intents_bridge`
            // (pull-on-init so a restart that happens mid-move rehydrates
            // the moving-banner without waiting for the reaper's emit).
            mcp::move_workspace::move_workspace_bridge,
            mcp::move_workspace::list_move_intents_bridge,
            // SW-WV-F3: the fleet-view roster commands — keyed lookup and operator
            // force-close (observe-half; the write reuses A4's single-writer CAS).
            weft::roster_query::wv_roster_for_session,
            weft::roster_query::wv_worker_force_close,
            commands::panes::list_panes,
            commands::panes::list_builtin_panes,
            commands::panes::write_dashboard_profile,
            commands::scratch::list_notes,
            commands::scratch::read_note,
            commands::scratch::search_notes,
            commands::scratch::create_note,
            commands::scratch::write_note,
            commands::scratch::update_note_meta,
            commands::scratch::delete_note,
            commands::scratch::notes_history,
            commands::scratch::notes_read_at,
            commands::scratch::notes_restore,
            commands::scratch::reap_empty_scratch_notes,
            commands::scratch::notes_dir_path,
            commands::system::ensure_directory,
            commands::pty::admitted_commands::write_pty,
            commands::pty::admitted_commands::submit_pty_line,
            commands::pty::admitted_commands::resize_pty,
            commands::pty::admitted_commands::subscribe_terminal,
            commands::pty::admitted_commands::read_terminal_text,
            commands::routines::routine_list,
            commands::context_query::query_context,
            commands::routines::routine_get,
            commands::routines::routine_set_enabled,
            commands::routines::routine_run_now,
            commands::routines::routine_runs,
            commands::routines::routine_runs_since,
            commands::routines::routine_stop_run,
            commands::routines::routine_result,
            commands::routines::routine_cancel_result,
            commands::routines::routine_archive,
            commands::routines::routine_copy_to_mine,
            commands::routines::routine_arm_here,
            commands::routines::routine_arm_on,
            commands::routines::routine_handover_release,
            commands::routines::routine_refresh_on_machine,
            commands::routines::routine_run_detail,
            commands::routines::routine_needs_you_count,
            commands::routines::routine_scheduler_status,
            commands::routines::routine_acknowledge_missed,
            commands::routines::routine_save,
            commands::routines::routine_preview_schedule,
            commands::routines::routine_preview_command,
            commands::routines::routine_restore_revision,
            commands::routines::routine_open_authoring_agent,
            commands::dashboard_launch::dashboard_launch_begin,
            commands::dashboard_launch::dashboard_launch_bind,
            commands::dashboard_launch::dashboard_launch_abort,
            commands::dashboard_launch::dashboard_register_session,
            commands::dashboard_launch::dashboard_resolve_session,
            commands::dashboard_launch::dashboard_list_sessions,
            commands::agent_inbox::list_inbox_messages,
            commands::agent_inbox::reply_as_human,
            commands::agent_inbox::react_as_human,
            commands::agent_inbox::inbox_reply_recipient,
            commands::agent_inbox::mark_inbox_message_read,
            commands::agent_inbox::mark_inbox_messages_read,
            commands::agent_inbox::mark_inbox_messages_unread,
            commands::agent_inbox::mark_inbox_messages_archived,
            commands::agent_inbox::withdraw_inbox_message,
            commands::agent_inbox::list_agent_updates,
            commands::agent_inbox::get_human_status,
            commands::agent_inbox::set_human_status,
            commands::agent_inbox::inbox_attachment_status,
            commands::agent_inbox::load_inbox_media,
            commands::agent_inbox::trigger_hydrate_from_inbox,
            live_hub::live_hub_subscribe,
            live_hub::agent_definitions::agent_definitions_prepare,
            live_hub::configuration::configuration_prepare,
            live_hub::dismiss_agent_mark,
            live_hub::live_hub_page,
            live_hub::live_hub_window,
            live_hub::live_hub_view_key,
            live_hub::live_hub_unsubscribe,
            commands::agent_read_model::agent_read_sessions,
            commands::agent_read_model::agent_read_agent,
            commands::agent_read_model::agent_dismiss_context_suggestion,
            commands::agent_read_model::agent_read_inbox,
            commands::agent_read_model::agent_read_roles,
            commands::agent_read_model::send_inbox_as_human,
            commands::agent_read_model::list_session_inbox,
            commands::agent_read_model::list_inbox_rollup,
            commands::data_read_model::routine_read_list,
            commands::data_read_model::routine_read_get,
            commands::data_read_model::routine_read_runs,
            commands::data_read_model::routine_read_run,
            commands::data_read_model::run_read_list,
            commands::data_read_model::run_read,
            commands::data_read_model::run_read_evidence,
            commands::data_read_model::run_read_work_product,
            commands::data_read_model::session_read_usage,
            commands::data_read_model::session_read,
            commands::data_read_model::why_line,
            commands::data_read_model::open_diff_read,
            commands::data_read_model::setup,
            commands::data_read_model::story_changes,
            commands::data_read_model::story_totals,
            commands::data_read_model::session_set_model_rate,
            commands::data_read_model::spend_cap_set,
            commands::data_read_model::spend_cap_accept,
            commands::data_read_model::spend_rate_accept,
            commands::data_read_model::session_read_model_rates,
            commands::role_inbox::register_role_cmd,
            commands::role_inbox::claim_role_messages_cmd,
            commands::role_inbox::accept_role_message_cmd,
            commands::role_inbox::ack_role_message_cmd,
            commands::role_inbox::release_role_cmd,
            commands::role_inbox::send_role_message_cmd,
            commands::dashboard_launch::dashboard_probe_sessions_alive,
            commands::dashboard_launch::dashboard_kill_by_handle,
            commands::dashboard_launch::dashboard_foreground_session,
            commands::dashboard_launch::dashboard_set_background_limit,
            commands::dashboard_launch::resolve_story_path,
            commands::dashboard_launch::resolve_epic_path,
            commands::pty::reserve_terminal_stream,
            commands::pty::unsubscribe_terminal,
            commands::pty::terminal_server_status,
            commands::pty::terminal_activity_upgrade_status,
            commands::pty::update_terminal_activity_server,
            commands::pty::restart_terminal_server,
            commands::pty::terminal_server_overview,
            commands::pty::stop_terminal_server,
            commands::pty::start_terminal_server,
            commands::pty::replace_terminal_server,
            commands::pty::end_unowned_terminal_session,
            commands::host_connection_manager::prepare_machine_terminal_server_replacement,
            commands::host_connection_manager::restart_machine_terminal_server,
            commands::pty::admitted_commands::session_is_idle,
            commands::pty::admitted_commands::session_has_live_agent,
            commands::pty::sessions_have_live_agent,
            commands::pty::admitted_commands::sessions_running_processes,
            commands::pty::admitted_commands::session_cwd_cmd,
            commands::pty::admitted_commands::session_prompt_cmd,
            commands::pty::reconfigure_warm_pool,
            commands::user_shell::terminal_shell_info,
            commands::user_shell::set_terminal_shell_override,
            // SW-RT-7b: the host-owned terminal layout and its verbs.
            commands::pty_layout::terminal_layout_snapshot,
            commands::pty_layout::terminal_activity_snapshot,
            commands::pty_layout::terminal_prepare_shell_activity,
            commands::pty_layout::terminal_launch_activity_line,
            commands::pty_layout::terminal_layout_admission,
            commands::pty_layout::terminal_open_tab,
            commands::pty_layout::terminal_remote_shell_tab,
            commands::pty_layout::terminal_close_tab,
            commands::pty_layout::terminal_respawn_tab,
            commands::pty_layout::terminal_place_tab,
            commands::pty_layout::terminal_rename_tab,
            commands::pty_layout::terminal_move_tab,
            commands::pty_layout::terminal_set_active,
            commands::pty_layout::terminal_set_grid,
            commands::pty_layout::terminal_set_view_hint,
            commands::pty_layout::terminal_set_tags,
            commands::pty_layout::terminal_set_descriptor,
            commands::pty_layout::terminal_close_workspace,
            // SLW-WL-1: FE→Rust push for the "auto-remove merged worktrees" setting
            // that gates the merge→teardown poll (spawned above).
            commands::tools::detect_tools,
            commands::tools::open_terminal,
            commands::tools::open_editor,
            commands::files::check_path_exists,
            commands::files::read_file,
            commands::file_surface::file_surface_call,
            live_hub::files::file_surface_prepare_view,
            live_hub::stories::nav_model_publish,
            commands::files::read_workspace_file,
            commands::files::write_content_file,
            commands::files::write_worktree_file,
            commands::worktree_editor::worktree_file_head,
            commands::worktree_editor::worktree_list_files,
            commands::files::write_binary_content_file,
            commands::files::delete_content_file,
            commands::files::move_content_file,
            commands::files::list_directory,
            commands::files::resolve_team_landing_page,
            commands::files::create_file,
            commands::files::ingest_file_into_context,
            commands::files::rename_path,
            commands::files::delete_path,
            // SW-SD-2: account/CLI-agnostic agent-runner `analyze` bridge verb.
            commands::agent_runner::analyze,
            commands::agent_runner::run_skill,
            // SW-AN-7: resolve a dashboard-selected launch profile (args + escalation).
            commands::agents::list_agent_signals,
            commands::watch::start_watching,
            commands::watch::stop_watching,
            commands::watch::add_watch,
            commands::watch::remove_watch,
            commands::watch::lazy_attach_workspace,
            commands::watch::lazy_detach_workspace,
            commands::team_file_edits::team_file_edits_status,
            commands::team_file_edits::forget_missing_workspace,
            commands::team_file_edits::publish_context_edits,
            commands::team_file_edits::discard_team_file_edits,
            commands::team_file_edits::worktree_unpublished_context,
            commands::context_operation::cancel_context_operation,
            commands::context_operation::discard_context_edits,
            commands::worktree_watch::lazy_attach_worktree_watch,
            commands::worktree_watch::detach_worktree_watch,
            commands::git::get_context_repo_path,
            commands::git::check_context_repo_remote,
            commands::git::reveal_context_repo,
            commands::git::reveal_path,
            // SW-GS-2: git-sync publish-set picker transport (F4).
            commands::git_sync::get_publish_set,
            // Cheap name-only changed-path count for the closed-pane sync badge.
            commands::git_sync::changed_path_count,
            // SW-GS-6: read-only out-of-sync probe for the "N incoming" badge.
            commands::git_sync::incoming_status,
            // SW-GS-3: per-file before/after content for the redline viewer.
            commands::git_sync::get_file_redline,
            // SLW-WD-1: source worktree change-set (merge-base baseline) + redline.
            commands::git_sync::source_worktree_changeset,
            commands::git_sync::source_file_redline,
            // SLW-WD-8: source worktree commit graph (all refs) + per-commit
            // change-set + per-commit redline — the history data for WD-9.
            commands::git_sync::source_commit_graph,
            commands::git_sync::source_commit_changeset,
            commands::git_sync::source_commit_file_redline,
            // SLW-WD-4: app-side bridge to WD-3's route/un-route verbs (the "→ context"
            // affordance in the source-diff overlay). Shares one orchestration with the
            // MCP `#[tool]`s.
            commands::route_to_context::route_file_to_context,
            commands::route_to_context::unroute_file_from_context,
            // The context repo's status button, its cards and the History view.
            commands::context_repo_git::context_repo_status,
            commands::context_repo_git::resolve_context_conflict,
            commands::context_repo_git::hand_context_conflict_to_agent,
            commands::context_repo_git::run_context_sync_action,
            commands::context_repo_git::context_repo_history,
            // SW-GS-5: authenticated fast-forward pull (the Pull button).
            commands::claude_session::watch_for_claude_session,
            commands::claude_session::cancel_session_watcher,
            commands::file_index::list_context_files,
            // Auth + GitHub commands
            commands::local_profile::local_profile_status,
            commands::local_profile::select_local_profile,
            commands::local_profile::revoke_local_machine_access,
            commands::local_profile::set_local_identity,
            commands::machines::list_machines,
            commands::machines::reinstall::machine_terminal_server_update_mode,
            commands::machines::reinstall::reinstall_machine_terminal_server,
            commands::machines::restore::machine_pending_tabs,
            commands::machines::restore::reopen_machine_pending_tabs,
            commands::machines::local_update::local_terminal_server_update,
            commands::machines::local_update::update_local_terminal_server,
            commands::machines::strays::list_stray_terminal_servers,
            commands::machines::strays::stop_stray_terminal_server,
            commands::machines::update_all::start_terminal_servers_update,
            commands::machines::update_all::terminal_servers_update_status,
            commands::machines::update_all::review_terminal_server_update,
            commands::machines::list_unreadable_machine_files,
            commands::machines::open_unreadable_machine_file,
            commands::machines::remove_unreadable_machine_file,
            commands::machines::list_ssh_sources,
            commands::machines::capture_machine_passphrase,
            commands::machines::test_machine_connection,
            commands::machines::install_terminal_server,
            commands::machines::enroll_private_machine_account,
            commands::machines::save_machine,
            commands::machines::identity::resolve_machine_identity,
            commands::machines::set_machine_hidden,
            commands::machines::set_machine_auto_connect,
            commands::machines::set_machine_remote_mcp_control,
            commands::machines::unshare_machine,
            commands::machines::remove_machine,
            commands::machines::legacy_retire::retire_legacy_terminal_server,
            commands::host_connection_manager::connect_machine,
            commands::host_connection_manager::disconnect_machine,
            commands::host_connection_manager::retry_machine,
            // SW-GS-19: in-UI PR review — live list + one-click approve&merge.
            // Update commands
            #[cfg(cs_channel = "internal")]
            commands::update::check_for_update,
            #[cfg(cs_channel = "internal")]
            commands::update::download_and_install_update,
            // Dependency manager
            commands::deps::get_dependency_status,
            commands::deps::recheck_dependencies,
            commands::deps::resolve_agent_cli,
            commands::deps::install_command_line_tools,
            // Pipeline scanner (story 30.7) + status update (story 34.5)
            commands::pipeline_scanner::scan_pipeline_tree,
            commands::pipeline_scanner::find_nearest_valid_scope,
            commands::pipeline_scanner::get_story_details,
            commands::pipeline_scanner::update_pipeline_status,
            commands::pipeline_scanner::read_epic_repos,
            commands::pipeline_scanner::set_epic_repos,
            // SLW-MD-1: in-file frontmatter / cs:* metadata editor
            commands::frontmatter_editor::get_metadata_model,
            // SW-DX-1: team tag registry read seam
            commands::frontmatter_editor::get_tag_schema,
            commands::frontmatter_editor::list_source_repos_for_team,
            commands::frontmatter_editor::apply_metadata_edit,
            commands::frontmatter_editor::update_frontmatter,
            commands::pane_bridge::bridge_doc_context,
            commands::pane_bridge::bridge_resolve_open_target,
            commands::guest_session::set_guest_session,
            commands::guest_session::get_guest_session,
            commands::story_create::preview_new_story,
            commands::story_create::create_story,
            commands::story_create::validate_story_file,
            commands::story_create::story_repo_choices,
            // SW-OR-12: agent-result output surface — durable outcome header + persisted transcript
            commands::sprint_run::run_store::read_run_outcome,
            commands::sprint_run::run_store::read_run_work_product,
            commands::sprint_run::run_store::list_run_outcomes,
            commands::sprint_run::run_evidence::read_run_evidence,
            commands::sprint_run::run_artifact::read_run_artifact,
            commands::sprint_run::run_artifact::read_raw_terminal_snapshot,
            commands::sprint_run::run_artifact::has_raw_terminal_snapshot,
            // SW-OR-16: adapter-declared structured transcript — parsed turns for the rich panel mode
            commands::sprint_run::run_transcript::read_run_transcript,
            // SW-SB-2: per-team sprint definition registry (read side)
            commands::sprint_registry::load_sprint_registry,
            // SW-CP-24: merge-mark set/clear seam (the CP-27 toggle)
            commands::merge_marks::set_content_merge_mark,
            commands::merge_marks::clear_content_merge_mark,
            // SW-CP-27: merge-mark read seam (toggle renders tree badge + Mark/Unmark label)
            commands::merge_marks::list_content_merge_marks,
            // SW-CP-28: team-level projection-exclude read/write seam (Settings section)
            commands::projection_excludes::get_projection_excludes,
            commands::projection_excludes::set_projection_excludes,
            // SW-GT-1: per-team status taxonomy (read side; SE-6 foundation)
            commands::status_taxonomy::load_status_taxonomy,
            // SW-GT-6: team config writes
            commands::config_editor::save_team_config,
            commands::config_editor::can_declare_team_field,
            commands::config_editor::declare_team_field,
            // Worktree discovery + validation (story 29.2)
            commands::worktree::probe_worktree_config,
            commands::worktree::get_worktree_config,
            commands::worktree::create_worktree,
            commands::worktree::fetch_and_create_worktree,
            commands::worktree::local_default_branch,
            commands::worktree::story_default_start_point,
            commands::worktree::switch_worktree_branch,
            commands::worktree::remove_worktree,
            commands::worktree::check_worktree_dirty,
            commands::worktree_liveness::worktree_live_session_count,
            commands::worktree::list_worktrees,
            commands::worktree::list_managed_worktrees,
            // SW-AU-34: FE push of the quit-on-close preference into the process-global
            // the window-close handler reads (background-on-close is the default).
            commands::background_mode::set_quit_on_close,
            commands::terminal_server_tray::set_terminal_server_menu_bar,
            commands::worktree::run_worktree_setup,
            commands::worktree_provenance::evaluate_setup_provenance,
            commands::worktree::cancel_worktree_run,
            commands::worktree::check_branch_exists,
            // SLW-NC-7: team-shared nav pins (`install::team_pins`).
            commands::worktree::set_team_nav_pin,
            commands::worktree::get_team_nav_pins,
            // Registry loader
            commands::registry::load_org_cmd,
            // SW-CR-8: the install-tree fold migration's validate finding and
            // one-click heal action.
            commands::install_tree_heal::check_install_leftovers_cmd,
            commands::install_tree_heal::heal_install_tree_cmd,
            commands::layout_resolver::legacy_repository_statuses,
            commands::layout_resolver::legacy_repository_local_state,
            commands::layout_resolver::cancel_legacy_repository_job,
            // Org management (C.2)
            commands::org::discover_local_orgs,
            commands::local_organization::create_local_organization,
            commands::context_attachments::attach_context_repository,
            commands::context_attachments::list_context_attachments,
            commands::context_attachments::detach_context_repository,
            commands::context_attachments::ensure_codesense_docs,
            commands::context_attachments::ensure_sample_repo,
            // SLW-PT-21: the Scratch space tab
            commands::scratch_space_view::list_scratch_space,
            commands::private_nav::resolve_private_nav_source,
            commands::private_nav::open_panels_menu,
            commands::scratch_space_view::create_scratch_file,
            commands::scratch_use::get_scratch_team_use,
            commands::scratch_use::set_scratch_team_use,
            commands::scratch_use::list_scratch_overlay_files,
            commands::scratch_use::set_scratch_file_use_team,
            commands::agent_accounts::list_agent_accounts,
            commands::agent_accounts::list_detached_agent_accounts,
            commands::agent_accounts::create_agent_account,
            commands::agent_accounts::rename_agent_account,
            commands::agent_accounts::detach_agent_account,
            commands::agent_accounts::reattach_agent_account,
            commands::agent_accounts::start_agent_account_login,
            commands::agent_accounts::cancel_agent_account_login,
            commands::agent_accounts::list_agent_account_login_states,
            commands::agent_accounts::acknowledge_agent_account_lifecycle_event,
            commands::agent_accounts::get_active_agent_account,
            commands::agent_accounts::set_active_agent_account,
            commands::agent_accounts::preview_agent_account_launch,
            commands::interactive_gate::grant_trusted_capability,
            commands::wake::wake_history,
            commands::wake::remind_session,
            commands::wake::session_timers,
            commands::wake::cancel_session_timer,
            commands::agent_prompt::send_prompt_to_agent,
            commands::agent_usage::read_agent_usage,
            commands::agent_usage::refresh_agent_usage,
            commands::agent_usage::set_agent_usage_visibility,
            // SW-AN-42: MCP scope authoring — read installed servers + project the
            // interactive-launch scoped config
            commands::mcp_authoring::project_mcp_scope_config,
            mcp::remote_workspace_target::list_prepared_remote_workspaces,
            mcp::remote_story_workspace::prepare_remote_story_workspace,
            mcp::workspace_base::update_workspace_base,
            // The frontend's interactive launch of a named agent
            commands::agents_launch::agent_launch_prepare,
            // The Agents tab
            commands::agents::agents_list,
            commands::avatars::avatar_packs_list_for_team,
            commands::roles::roles_list,
            commands::roles::role_save,
            commands::avatars::avatar_default_pack_set,
            commands::avatars::avatar_pack_share_with_team,
            commands::agents::agent_detail,
            commands::agents::agent_archive,
            commands::agents::agent_restore_routines,
            commands::agents::agent_unarchive,
            commands::agents::agent_delete,
            commands::agents::agent_copy_to_mine,
            commands::agents::agent_binding_set,
            commands::agents::agent_binding_clear,
            commands::agents::agent_save,
            commands::agents::agent_preview,
            commands::agents::agent_context_preview,
            commands::agents::agent_context_update,
            commands::agents::agent_restore_revision,
            commands::agents::agent_startup_catalog,
            commands::agents::agent_open_authoring_agent,
            commands::workspace_open::agent_launch_open,
            // Logging
            commands::system::get_log_file_path,
            commands::system::get_log_debug_enabled,
            // HTTP API
            commands::system::get_http_api_port,
            commands::frames::mint_frame,
            commands::frames::release_frame,
            commands::frames::rebind_frame_team,
            commands::frames::resolve_frame_grant,
            commands::dashboard_commands::commands_register,
            commands::dashboard_commands::commands_sync,
            commands::dashboard_commands::commands_list,
            commands::dashboard_commands::commands_run,
            // MCP state
            mcp::state::push_viewer_state,
            commands::system::workspace_group_edit,
            // SW-SC-14 canvas render bridge reply
            mcp::state::resolve_canvas_render,
            mcp::state::resolve_rendered_document,
            mcp::state::admit_rendered_document,
            // SW-FX-131: the viewer's reply for an `open_workspace` machine open.
            commands::workspace_open::resolve_open_workspace_machine,
            // SW-AW-4 ask_user answer bridge reply
            mcp::state::resolve_ask_user,
            // Source repo attachment (W.2)
            commands::attachments::resolve_source_repo_attachment,
            commands::attachments::save_source_repo_attachment,
            commands::attachments::list_attachments_by_remote,
            commands::attachments::delete_source_repo_attachment,
            commands::attachments::remove_source_repo_from_team,
            commands::attachments::add_source_repo_to_team,
            commands::attachments::attach_source_repo_clone,
            commands::attachments::list_managed_clone_health,
            commands::reconcile_workspace::reconcile_workspace,
            commands::reconcile_workspace::preview_workspace_reconcile,
            commands::git::read_source_repo_origin_url,
            commands::git::resolve_tab_source_repo,
            // DESIGN-remote-source.md — the remote source + editor commands.
            // Resolve, read, write, watch and diff (changeset + redline) all
            // go over the `WorkspaceFile` wire protocol to the machine's Git.
            commands::remote_source::resolve_tab_source_repo_remote,
            commands::remote_source::source_remote_file_read,
            commands::remote_source::source_remote_file_write,
            commands::remote_source::source_remote_list_directory,
            commands::remote_source::source_remote_file_inspect,
            commands::remote_source::source_remote_file_head,
            commands::remote_source::source_remote_list_files,
            commands::remote_source::source_remote_changeset,
            commands::remote_source::source_remote_file_redline,
            commands::remote_source::source_remote_watch_start,
            commands::remote_source::source_remote_watch_stop,
            commands::remote_source::source_remote_tracked_count,
            commands::registry_local::commit_registry_edit_local,
            commands::registry_local::bootstrap_workspace_content_cmd,
            // Workspace config (R.10)
            commands::workspace_config::load_workspace_config,
            commands::workspace_config::save_workspace_config,
            // Path resolution (PTS-3)
            commands::paths::resolve_team_content_dir,
            // macOS menu rebuild (C.16)
            menu::rebuild_menu,
            // PDF export (R.4) — macOS only
            #[cfg(target_os = "macos")]
            commands::export_pdf::export_pdf_from_url,
            #[cfg(target_os = "macos")]
            commands::export_pdf::export_pdf_from_html,
            #[cfg(target_os = "macos")]
            commands::export_pdf::write_binary_file,
            // Content engine boundary surface (SS-13), read by the
            // frontend (merge preview) and the context MCP tools.
            commands::content_engine::list_layers,
            commands::content_engine::list_layer_contents,
            commands::content_engine::read_layer_file,
            commands::content_engine::list_merge_preview_repos,
            commands::content_engine::preview_merge_marked_file,
            commands::content_engine::read_lockfile_for_repo,
            commands::content_engine::move_file_between_layers,
            commands::move_between_contexts::move_content_between_contexts,
            commands::remove_from_context::remove_content_from_context,
            commands::content_engine::promote_content_entry,
            commands::content_engine::sense_sync_write_to_layer,
            commands::content_engine::sense_sync_scan_cwd,
            commands::content_engine::read_canvas_library,
            commands::content_engine::write_canvas_library,
            // SW-FX-64: FE liveness ack for the webview renderer watchdog.
            renderer_watchdog::renderer_heartbeat,
            // SW-WC-1: per-workspace archive flag (one nullable `archivedAtMs`
            // field in `~/.codesense/workspace-registry.json`; SW-WC-2's chat
            // compose reads it to lock the input while active).
            commands::workspace_registry::archive_workspace,
            commands::workspace_registry::unarchive_workspace,
            commands::workspace_registry::get_workspace_archive_state,
            commands::workspace_registry::list_workspace_archive_states,
            // SW-WC-2: workspace chat engine (dashboard-facing Tauri surface).
            commands::workspace_chat::workspace_chat_send,
            commands::workspace_chat::workspace_chat_read,
            commands::workspace_chat::workspace_chat_mark_viewed,
            commands::workspace_chat::workspace_chat_human_rooms,
            commands::workspace_chat::workspace_chat_list_workspaces,
            commands::workspace_chat::workspace_chat_archive,
        ]))
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            // Exit-time PTY teardown: kill the non-persisting complement
            // (warm-pool members + orphans) so nothing outlives the app
            // (AC-10/AC-11). Cross-quit disk persistence was retired (SW-EW-46),
            // so there is nothing to serialize here — just the clean kill.
            tauri::RunEvent::ExitRequested { api, .. } => {
                // SW-PB-23: from here an armed restart is neither dropped nor cancelled, so a
                // slow drain still ends in the relaunch.
                #[cfg(not(cs_channel = "internal"))]
                {
                    if let Some(updates) =
                        app.try_state::<std::sync::Arc<license::updates::Updates>>()
                    {
                        updates.mark_exiting();
                    }
                }
                let connections = app.state::<commands::host_connection_manager::HostConnectionManager>();
                match exit_coordinator::request(&connections) {
                    exit_coordinator::ExitDisposition::StartDrain => {
                        api.prevent_exit();
                        app.state::<terminal::session_server::SessionServerState>().admission.shutdown();
                        let handle = app.clone();
                        tauri::async_runtime::spawn(async move {
                            let (ack, answer) = tokio::sync::oneshot::channel();
                            let ack = std::sync::Mutex::new(Some(ack));
                            let listener = handle.listen("app:quit-flush-ack", move |_| {
                                if let Some(ack) = ack.lock().unwrap().take() { let _ = ack.send(()); }
                            });
                            let _ = handle.emit("app:quit-flush", ());
                            exit_coordinator::wait_for_flush(answer).await;
                            handle.unlisten(listener);
                            let connections = handle.state::<
                                commands::host_connection_manager::HostConnectionManager,
                            >();
                            let registry = handle.clone();
                            exit_coordinator::drain_then(
                                &connections,
                                || backstop_kill(&handle),
                                move || {
                                    if let Some(launch) = registry
                                        .try_state::<commands::dashboard_launch::DashboardLaunchState>()
                                    {
                                        launch.flush_and_wait(std::time::Duration::from_secs(3));
                                    }
                                },
                                || handle.exit(0),
                            )
                            .await;
                        });
                    }
                    exit_coordinator::ExitDisposition::KeepWaiting => {
                        api.prevent_exit();
                    }
                    exit_coordinator::ExitDisposition::Allow => {
                        // SW-PB-13 AC-4: a staged beta build installs as the app quits.
                        // SW-PB-23: after Restart to update or the required-update deadline,
                        // the app then relaunches onto it.
                        #[cfg(not(cs_channel = "internal"))]
                        {
                            if install_staged_update(app) {
                                relaunch_after_update(app);
                            }
                        }
                    }
                }
            }
            tauri::RunEvent::Exit => {
                graceful_exit_cleanup();
            }
            // SW-AU-34 (AC-2): dock-icon click after close-to-background. `Reopen`
            // fires on macOS dock activation; showing + focusing the window also
            // un-hides the app the close handler put to sleep. The primary restore
            // affordance — no system tray needed.
            #[cfg(target_os = "macos")]
            tauri::RunEvent::Reopen { .. } => {
                // `app.show()` (NSApp `unhide:`) is load-bearing: the close arm hid the
                // whole app via `app.hide()`, and showing a window alone does NOT clear
                // that app-level hidden flag — and tao's `applicationShouldHandleReopen`
                // returns false while no window is visible, suppressing AppKit's own
                // default unhide. Without this the dock click leaves the app stuck
                // hidden (AC-2 restore fails).
                let _ = app.show();
                match app.get_webview_window("main") {
                    Some(window) => {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                    None => log::warn!(
                        target: "system",
                        "background-on-close: no 'main' window to restore on dock reopen"
                    ),
                }
            }
            _ => {}
        });
}

/// SW-PB-13 AC-4: installs the build the beta updater staged, if it is newer than this one,
/// then lets the exit continue. The new version runs at the next launch. With nothing staged
/// (or after a kill), nothing happens here, and the next launch's check finds the update again.
/// Returns true only when Restart to update or the required-update deadline (SW-PB-23) armed
/// this exit: then the caller relaunches. A plain quit never relaunches.
#[cfg(not(cs_channel = "internal"))]
fn install_staged_update(app: &tauri::AppHandle) -> bool {
    let Some(updates) = app.try_state::<std::sync::Arc<license::updates::Updates>>() else {
        return false;
    };
    // The install takes a few seconds; hide the windows first so the app doesn't look frozen.
    if updates.has_staged() {
        for window in app.webview_windows().values() {
            let _ = window.hide();
        }
    }
    let plan = updates.finish_exit();
    match plan.installed {
        Some(Ok(version)) if plan.relaunch => {
            log::info!(target: "license", "update {version} installed; relaunching onto it")
        }
        Some(Ok(version)) => {
            log::info!(target: "license", "update {version} installed; it runs at the next launch")
        }
        Some(Err(e)) => log::warn!(target: "license", "the staged update was not installed: {e}"),
        None => {}
    }
    plan.relaunch
}

/// SW-PB-23: the relaunch after Restart to update or the deadline. It runs only from the exit
/// the coordinator allowed, so the remote connections have drained, the non-persisting sessions
/// are reaped and the terminal servers keep their sessions for the new build to reattach. The
/// exit cleanup runs first, because a restart from the main thread skips `RunEvent::Exit`.
#[cfg(not(cs_channel = "internal"))]
fn relaunch_after_update(app: &tauri::AppHandle) {
    graceful_exit_cleanup();
    app.restart();
}

/// The cleanup of a graceful exit: at `RunEvent::Exit`, and before the relaunch after an update.
fn graceful_exit_cleanup() {
    attribution::watcher::shutdown();
    // SS-FU-17 AC-3: exit cleans ONLY codesense's own runtime artifacts — never materialized
    // files, exclude blocks, or lockfiles. See [`app_exit_cleanup`].
    let mcp_socket = mcp::mcp_socket_path();
    app_exit_cleanup(
        &mcp_socket,
        mcp::mcp_socket_bound_here(&mcp_socket),
        &http::http_port_file_path(),
        std::env::var("CODESENSE_DISABLE_HTTP").is_err(),
    );
    // SS-FU-17 AC-5: drop the liveness heartbeat on graceful exit (the staleness window covers
    // the crash path).
    runtime::clear_heartbeat();
}

/// Exit-time PTY teardown (AC-10/AC-11): complement-kill every session NOT in the
/// persisting set (warm-pool members + orphans), so nothing outlives the app.
/// Cross-quit disk persistence was retired (SW-EW-46), so there is no serialize —
/// the persisting set is only the keep-set that the kill spares. Runs at
/// `ExitRequested`; best-effort, and never blocks on a child's death.
///
/// In sidecar mode the sidecar's registry runs the same complement kill, in one
/// call bounded to a few seconds: its persisting sessions keep running there,
/// output still buffered, after the app is gone. Every CodeSense app on the Mac
/// shares that sidecar, so the kill takes only the sessions this app opened;
/// the last app to quit also takes every unhanded warm-pool member, a crashed
/// app's idle shells included, but never another app's tab.
fn backstop_kill(app: &tauri::AppHandle) {
    // Complement-kill: every session not in the persisting set, incl. warm-pool
    // members (best-effort; does not block on completion). The ONE exit-time
    // PTY reaper, run by each host that holds sessions; in the shared sidecar,
    // scoped to this app's sessions (plus idle pool shells for the last app).
    commands::pty_backend::reap_non_persisting(app);

    // SW-OR-10: PTY-less headless runs live outside `PtyState`, so the sweep above can't
    // reach them — `killpg` each live headless child's group directly so a detached agent
    // never orphans past app close.
    if let Some(launch) = app.try_state::<commands::dashboard_launch::DashboardLaunchState>() {
        for pgid in launch.headless_child_pgids() {
            commands::dashboard_launch::kill_process_group(pgid);
        }
    }
    log::info!(
        target: "terminal::persistence",
        "exit teardown: killed the non-persisting session complement"
    );
}

/// SS-FU-17 AC-3: app-exit cleanup. Removes **only** codesense's own
/// runtime artifacts — the MCP socket and the HTTP port file — and
/// deregisters from the HTTP server registry. It MUST NOT remove
/// materialized files, strip `.git/info/exclude` blocks, or delete
/// lockfiles.
///
/// Persistence across the app lifecycle is the contract: materialized
/// content survives quit/crash, and running-vs-not legibility comes from
/// the status indicator + graceful degradation, never from mutating the
/// user's repos (see the story's "Why not clean-on-close"). **Do not add
/// install teardown here** — detach is the only uninstall verb.
///
/// `http_enabled` is threaded in (rather than read from the environment
/// inside) so the guard test can exercise the file-cleanup paths without
/// touching the real HTTP registry or mutating process-global env.
///
/// `mcp_socket` is removed only when `mcp_socket_ours`: an instance that found
/// another's socket live never bound it, and removing it would cut every agent
/// off from the instance still running.
fn app_exit_cleanup(
    mcp_socket: &std::path::Path,
    mcp_socket_ours: bool,
    http_port_file: &std::path::Path,
    http_enabled: bool,
) {
    if mcp_socket_ours && mcp_socket.exists() {
        let _ = std::fs::remove_file(mcp_socket);
        log::info!(target: "mcp", "Cleaned up MCP socket: {:?}", mcp_socket);
    }
    if http_enabled {
        if let Err(e) = http::registry::deregister() {
            log::warn!(target: "http", "Failed to deregister from server registry: {}", e);
        } else {
            log::info!(target: "http", "Deregistered from server registry");
        }
    }
    // Deprecated: unconditional cleanup is safe (no-op if absent). Remove in v0.4.0.
    if http_port_file.exists() {
        let _ = std::fs::remove_file(http_port_file);
        log::info!(target: "http", "Cleaned up HTTP port file: {:?}", http_port_file);
    }
}

#[cfg(test)]
mod connection_startup_order_tests {
    use super::start_connection_registry_after_dependencies;
    use codesense_pty_core::domain::{PortUnavailable, TerminalDomainEvent, TerminalEventSink};
    use std::sync::Arc;
    use tauri::Manager;

    struct Sink;

    impl TerminalEventSink for Sink {
        fn emit(&self, _event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
            Ok(())
        }
    }

    #[test]
    fn registry_start_boundary_requires_terminal_and_ambient_profile_dependencies() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let starts = std::sync::atomic::AtomicUsize::new(0);
        assert!(
            start_connection_registry_after_dependencies(app.handle(), || {
                starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
            .is_err()
        );
        app.manage(Arc::new(codesense_pty_core::TerminalHost::new(
            crate::commands::pty_host::local_server_identity(),
            Arc::new(Sink),
            runtime.handle().clone(),
        )));
        app.manage(crate::commands::pty_host::StreamIds::default());
        app.manage(codesense_pty_core::spawn::HostEnvironment::of_process(
            None, None,
        ));
        assert!(
            start_connection_registry_after_dependencies(app.handle(), || {
                starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
            .is_err()
        );
        app.manage(crate::terminal::session_server::SessionServerState::default());
        assert!(
            start_connection_registry_after_dependencies(app.handle(), || {
                starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
            .is_err()
        );
        app.manage(crate::commands::host_connection_manager::HostConnectionManager::default());
        assert!(
            start_connection_registry_after_dependencies(app.handle(), || {
                starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
            .is_err()
        );
        let profile_home = tempfile::tempdir().unwrap();
        app.manage(crate::commands::local_profile::LocalProfileState::for_test(
            profile_home.path(),
            41,
            crate::commands::local_profile::MachineOwner::LegacyGithub { id: 41 },
        ));
        assert!(
            start_connection_registry_after_dependencies(app.handle(), || {
                starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })
            .is_err()
        );
        app.manage(crate::commands::guest_session::GuestSessionState::load(
            None,
        ));
        start_connection_registry_after_dependencies(app.handle(), || {
            starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
        .unwrap();
        assert_eq!(starts.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod log_target_tests {
    use super::{log_sinks, log_targets, LogSink};

    /// Release builds keep the webview console mirror; the WebDriver harness build
    /// drops it so forwarded console lines cannot loop back into the log.
    #[test]
    fn webview_sink_only_outside_the_webdriver_harness() {
        let sinks = log_sinks();
        assert_eq!(
            sinks.contains(&LogSink::Webview),
            !cfg!(feature = "e2e-webdriver")
        );
        assert!(sinks.contains(&LogSink::LogDir));
        assert!(sinks.contains(&LogSink::Stdout));
        assert_eq!(log_targets().len(), sinks.len());
    }
}

#[cfg(test)]
mod exit_guard_tests {
    use super::app_exit_cleanup;
    use std::fs;
    use tempfile::TempDir;

    /// SS-FU-17 AC-3 guard: the app-exit path removes its own runtime
    /// artifacts but leaves a materialized install — files, exclude block,
    /// lockfile — completely intact. If a future change adds clean-on-close
    /// teardown to the exit path, this test fails.
    #[test]
    fn app_exit_cleanup_never_touches_materialized_install() {
        let tmp = TempDir::new().unwrap();

        // A materialized install fixture the exit path must never touch.
        let code_repo = tmp.path().join("repo");
        fs::create_dir_all(code_repo.join(".git/info")).unwrap();
        let materialized = code_repo.join("skill.md");
        fs::write(&materialized, b"materialized bytes").unwrap();
        let exclude = code_repo.join(".git/info/exclude");
        fs::write(
            &exclude,
            b"# >>> codesense managed\nskill.md\n# <<< codesense managed\n",
        )
        .unwrap();
        let lockfile = tmp.path().join("install.lock.json");
        fs::write(&lockfile, b"{\"schema_version\":3}").unwrap();

        // Runtime artifacts the exit path IS allowed to remove.
        let socket = tmp.path().join("mcp.sock");
        fs::write(&socket, b"").unwrap();
        let port_file = tmp.path().join("http.port");
        fs::write(&port_file, b"54321").unwrap();

        // http_enabled=false → skip the real HTTP registry deregister.
        app_exit_cleanup(&socket, true, &port_file, false);

        // Runtime artifacts cleaned…
        assert!(!socket.exists(), "MCP socket should be cleaned at exit");
        assert!(
            !port_file.exists(),
            "HTTP port file should be cleaned at exit"
        );

        // …but the install is untouched (the AC-3 invariant).
        assert!(
            materialized.exists(),
            "exit must NOT remove materialized files"
        );
        assert!(exclude.exists(), "exit must NOT strip the exclude block");
        assert_eq!(
            fs::read(&exclude).unwrap(),
            b"# >>> codesense managed\nskill.md\n# <<< codesense managed\n",
            "exclude block bytes must be unchanged"
        );
        assert!(lockfile.exists(), "exit must NOT delete lockfiles");
    }

    /// An instance that found another instance's MCP socket live leaves it in
    /// place at exit, so the running instance's agents keep their MCP.
    #[test]
    fn app_exit_cleanup_keeps_another_instances_mcp_socket() {
        let tmp = TempDir::new().unwrap();
        let socket = tmp.path().join("mcp.sock");
        fs::write(&socket, b"").unwrap();
        let port_file = tmp.path().join("http.port");

        app_exit_cleanup(&socket, false, &port_file, false);

        assert!(
            socket.exists(),
            "another instance's MCP socket must survive"
        );
    }
}

#[cfg(test)]
mod panic_hook_tests {
    use super::format_panic_record;
    use std::sync::{Arc, Mutex};

    /// Panic hooks are process-global; the Rust test runner is multi-threaded
    /// by default. Tests that install/restore the hook must serialize against
    /// each other (and against other tests that might install hooks).
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// The boxed panic hook shape (`std::panic::set_hook` / `take_hook`).
    type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static>;

    /// RAII guard that restores the prior panic hook on drop. Safe across
    /// early-return / panic-in-test paths.
    struct HookRestoreGuard {
        prev: Option<PanicHook>,
    }

    impl HookRestoreGuard {
        fn install_capturing(buffer: Arc<Mutex<Vec<String>>>) -> Self {
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let thread = std::thread::current();
                let thread_name = thread.name();
                let payload: &str = info
                    .payload()
                    .downcast_ref::<&'static str>()
                    .copied()
                    .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
                    .unwrap_or("<non-string panic payload>");
                let backtrace = std::backtrace::Backtrace::force_capture();
                let record = format_panic_record(thread_name, payload, info.location(), &backtrace);
                buffer.lock().unwrap().push(record);
            }));
            Self { prev: Some(prev) }
        }
    }

    impl Drop for HookRestoreGuard {
        fn drop(&mut self) {
            if let Some(prev) = self.prev.take() {
                std::panic::set_hook(prev);
            }
        }
    }

    #[test]
    fn panic_hook_captures_record() {
        // Serialize panic-hook tests; even if a sibling test panics, the
        // lock's poisoning is tolerable — we just want exclusion.
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let buffer: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let _restore = HookRestoreGuard::install_capturing(Arc::clone(&buffer));

        // Spawn a named thread that panics with a known marker. Under default
        // `unwind`, the panic kills the thread; join() returns Err(payload).
        let handle = std::thread::Builder::new()
            .name("sf3-test-thread".into())
            .spawn(|| panic!("test-panic-marker-xyz"))
            .expect("spawn test thread");
        let join_result = handle.join();
        assert!(
            join_result.is_err(),
            "panicked thread should propagate as JoinError under unwind"
        );

        let records = buffer.lock().unwrap();
        // SF-3 review patch: filter by marker before asserting count. The
        // panic hook is process-global; even with TEST_LOCK serializing the
        // hook install/restore, any other thread panic during the window
        // (test-runtime worker, background task) would also land in the
        // buffer. The marker pin ("test-panic-marker-xyz") makes the count
        // robust against incidental traffic.
        let matching: Vec<&String> = records
            .iter()
            .filter(|r| r.contains("test-panic-marker-xyz"))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "exactly one marker-matching panic record should have been captured (buffer total: {})",
            records.len()
        );
        let record = matching[0];
        assert!(
            record.contains("sf3-test-thread"),
            "record must contain the thread name; got: {record}"
        );
        assert!(
            record.contains("lib.rs"),
            "record must contain the source-file location; got: {record}"
        );
        assert!(
            record.contains("backtrace:"),
            "record must contain the 'backtrace:' label owned by format_panic_record; got: {record}"
        );
    }

    #[test]
    fn format_panic_record_handles_unknown_location() {
        let bt = std::backtrace::Backtrace::disabled();
        let record = format_panic_record(Some("worker-7"), "kaboom", None, &bt);
        assert!(record.contains("'worker-7'"));
        assert!(record.contains("kaboom"));
        assert!(record.contains("<unknown location>"));
        assert!(record.contains("backtrace:"));
    }

    #[test]
    fn format_panic_record_handles_unnamed_thread() {
        let bt = std::backtrace::Backtrace::disabled();
        let record = format_panic_record(None, "oops", None, &bt);
        assert!(record.contains("<unnamed>"));
        assert!(record.contains("oops"));
    }
}

#[cfg(all(test, cs_channel = "internal"))]
mod license_channel_tests {
    /// SW-PB-9 AC-1: an internal build carries no beta build values, so the license module
    /// has nothing to read them from.
    #[test]
    fn internal_build_has_no_license_build_values() {
        assert_eq!(option_env!("CODESENSE_RELEASED_AT"), None);
        assert_eq!(option_env!("CODESENSE_LICENSE_PUBLIC_KEYS"), None);
    }

    /// SV-PBF-278 case row 11: the internal build is unchanged. It keeps the GitHub update
    /// commands and its own feedback refusal, and starts no beta update channel.
    #[test]
    fn row_11_the_internal_build_keeps_its_github_update_path() {
        // These name the GitHub commands, so this compiles only while the internal build has them.
        let _ = crate::commands::update::check_for_update;
        let _ = crate::commands::update::download_and_install_update;
        let refused = crate::mcp::feedback_tool::availability().unwrap_err();
        assert!(refused.starts_with("Beta feedback is only in CodeSense beta builds."));
        let lib = include_str!("lib.rs");
        let install = lib
            .find("license::install(app.handle())")
            .expect("the beta setup");
        assert!(
            lib[..install]
                .lines()
                .rev()
                .take(2)
                .any(|l| l.trim() == "#[cfg(not(cs_channel = \"internal\"))]"),
            "the beta update channel starts only outside the internal build"
        );
    }
}

#[cfg(test)]
mod docs_schemas;
