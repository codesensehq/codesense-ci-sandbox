// SPDX-License-Identifier: MIT
//! SV-8: this instance's terminal server in the macOS menu bar.
//!
//! An opt-in item (`settings.terminalServerMenuBar`, pushed by the frontend
//! through [`set_terminal_server_menu_bar`]) that shows whether the terminal
//! server runs, how many agents it runs, and offers Open CodeSense, Restart,
//! Stop or Start, and Quit. It lives in the app process: it stays while the
//! app is closed to the background and goes when the app quits.
//!
//! What the item shows is a pure function of SV-7's overview
//! ([`menu_model`]); Restart and Stop go through SV-7's MCP tool bodies, so
//! they bind to the reviewed `expected` (and the other instances connected)
//! exactly as an agent's do. A menu click cannot open an inline panel, so the
//! confirmation is a native dialog naming what ends, read when the item was
//! clicked; the server refuses, and nothing ends, if it changed before the
//! person confirmed.
//!
//! It refreshes when the menu-bar item is clicked, when the
//! server starts, stops or loses its connection ([`terminal_server_changed`]),
//! and otherwise every [`REFRESH`] — no faster than the Settings section.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};

use crate::commands::pty_backend::{TerminalServerOverview, TerminalServerRun};

/// The item's id among the app's tray icons.
pub const TRAY_ID: &str = "terminal-server";

/// The prefix of the item's menu ids: the app menu's handler ignores them.
pub const MENU_ID_PREFIX: &str = "terminal-server-tray:";

/// How often the item re-reads the server while nothing else asked it to:
/// the Settings section's pace (SV-7), never faster.
const REFRESH: Duration = Duration::from_secs(5);

/// Which icon the item shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayIcon {
    Running,
    Stopped,
    /// Needs attention: from another CodeSense version, or not answering.
    Attention,
}

/// The menu's Stop or Start item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopOrStart {
    pub action: MenuAction,
    pub label: &'static str,
    pub enabled: bool,
}

/// What a menu item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    Open,
    Restart,
    Stop,
    Start,
    Quit,
}

impl MenuAction {
    fn id(self) -> String {
        let name = match self {
            Self::Open => "open",
            Self::Restart => "restart",
            Self::Stop => "stop",
            Self::Start => "start",
            Self::Quit => "quit",
        };
        format!("{MENU_ID_PREFIX}{name}")
    }

    fn from_id(id: &str) -> Option<Self> {
        Some(match id.strip_prefix(MENU_ID_PREFIX)? {
            "open" => Self::Open,
            "restart" => Self::Restart,
            "stop" => Self::Stop,
            "start" => Self::Start,
            "quit" => Self::Quit,
            _ => return None,
        })
    }
}

/// What the item shows, from an overview: its icon, the count beside it
/// (running agents, when there are any), its status line, and which items
/// are enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMenuModel {
    pub icon: TrayIcon,
    pub count: Option<u32>,
    pub status: String,
    pub restart_enabled: bool,
    pub stop_or_start: StopOrStart,
}

fn plural(n: u32, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// The item for `overview` (SV-7's read of the terminal server).
pub fn menu_model(overview: &TerminalServerOverview) -> TrayMenuModel {
    let stop = |enabled| StopOrStart {
        action: MenuAction::Stop,
        label: "Stop Terminal Server…",
        enabled,
    };
    match overview.state {
        TerminalServerRun::Running => {
            let mut status = "Terminal server running".to_string();
            if let Some(sessions) = overview.sessions {
                status.push_str(&format!(" · {}", plural(sessions, "session")));
            }
            if let Some(agents) = overview.running_agents.filter(|agents| *agents > 0) {
                status.push_str(&format!(
                    " · {agents} agent{} running",
                    if agents == 1 { "" } else { "s" }
                ));
            }
            let bound = overview.expected.is_some();
            TrayMenuModel {
                icon: TrayIcon::Running,
                count: overview.running_agents.filter(|agents| *agents > 0),
                status,
                restart_enabled: bound,
                stop_or_start: stop(bound),
            }
        }
        TerminalServerRun::Stopped => TrayMenuModel {
            icon: TrayIcon::Stopped,
            count: None,
            status: "Terminal server not running".into(),
            restart_enabled: false,
            stop_or_start: StopOrStart {
                action: MenuAction::Start,
                label: "Start Terminal Server",
                enabled: true,
            },
        },
        TerminalServerRun::Incompatible => TrayMenuModel {
            icon: TrayIcon::Attention,
            count: None,
            status: "Terminal server from another CodeSense version: restart it".into(),
            restart_enabled: true,
            stop_or_start: stop(false),
        },
        TerminalServerRun::Unreachable => TrayMenuModel {
            icon: TrayIcon::Attention,
            count: None,
            status: "Terminal server running, but not answering".into(),
            restart_enabled: false,
            stop_or_start: stop(false),
        },
        TerminalServerRun::NotApplicable => TrayMenuModel {
            icon: TrayIcon::Stopped,
            count: None,
            status: "CodeSense hosts its terminals itself".into(),
            restart_enabled: false,
            stop_or_start: stop(false),
        },
    }
}

impl TrayMenuModel {
    /// Before the first read answers.
    fn reading() -> Self {
        Self {
            icon: TrayIcon::Stopped,
            count: None,
            status: "Reading the terminal server…".into(),
            restart_enabled: false,
            stop_or_start: StopOrStart {
                action: MenuAction::Stop,
                label: "Stop Terminal Server…",
                enabled: false,
            },
        }
    }
}

/// Where the item shows: the menu bar, or a test's record of it.
pub(crate) trait TrayHost {
    fn shown(&self) -> bool;
    fn show(&self, model: &TrayMenuModel) -> Result<(), String>;
    fn update(&self, model: &TrayMenuModel) -> Result<(), String>;
    fn remove(&self);
}

/// Turn the item on (shown once, reading) or off (removed at once); whether
/// it shows now.
pub(crate) fn toggle(host: &dyn TrayHost, enabled: bool) -> Result<bool, String> {
    match (enabled, host.shown()) {
        (true, false) => host.show(&TrayMenuModel::reading())?,
        (false, true) => host.remove(),
        _ => {}
    }
    Ok(host.shown())
}

/// The setting's mirror: whether the item should show.
static ENABLED: AtomicBool = AtomicBool::new(false);
/// Bumped as the item turns on, so a refresher of an earlier turn stops.
static TURN: AtomicU64 = AtomicU64::new(0);
/// The app the item belongs to, once it was turned on.
static APP: std::sync::OnceLock<AppHandle> = std::sync::OnceLock::new();
static REFRESHING: AtomicBool = AtomicBool::new(false);
const STOP_OR_START_ID: &str = "terminal-server-tray:stop-or-start";

#[derive(Default)]
struct NativeMenuState(Mutex<Option<(tauri::menu::Menu<tauri::Wry>, TrayMenuModel)>>);

/// FE→Rust push of `settings.terminalServerMenuBar`: the item shows at once,
/// or goes at once. macOS only (elsewhere a no-op).
#[tauri::command]
pub fn set_terminal_server_menu_bar(app: AppHandle, enabled: bool) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }
    if APP.set(app.clone()).is_ok() {
        app.manage(NativeMenuState::default());
        // Tauri's tray builder registers a global listener. Register once for
        // the app, so toggling the item never duplicates destructive actions.
        app.on_menu_event(|app, event| on_menu_event(app, event.id().as_ref()));
    }
    let shown = toggle(&AppTray { app: app.clone() }, enabled)?;
    let was = ENABLED.swap(enabled, Ordering::SeqCst);
    if enabled && shown && !was {
        let turn = TURN.fetch_add(1, Ordering::SeqCst) + 1;
        let refreshing = app.clone();
        std::thread::Builder::new()
            .name("terminal-server-tray".into())
            .spawn(move || {
                while ENABLED.load(Ordering::SeqCst) && TURN.load(Ordering::SeqCst) == turn {
                    refresh_now(&refreshing);
                    std::thread::sleep(REFRESH);
                }
            })
            .map_err(|error| {
                ENABLED.store(false, Ordering::SeqCst);
                AppTray { app: app.clone() }.remove();
                format!("start the menu-bar refresh: {error}")
            })?;
    }
    if was != enabled {
        log::info!(target: "terminal_server_tray", "menu-bar item {}", if enabled { "on" } else { "off" });
    }
    Ok(())
}

/// The server started, stopped, or lost or regained its connection: the
/// item (when it shows) re-reads it now, off the caller's thread.
pub fn terminal_server_changed<R: Runtime>(_app: &AppHandle<R>) {
    if !ENABLED.load(Ordering::SeqCst) {
        return;
    }
    if let Some(app) = APP.get() {
        refresh_soon(app);
    }
}

fn refresh_soon(app: &AppHandle) {
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("terminal-server-tray-refresh".into())
        .spawn(move || refresh_now(&app));
}

/// Read the server (never starting one) and show what it says.
fn refresh_now(app: &AppHandle) {
    if !ENABLED.load(Ordering::SeqCst) || REFRESHING.swap(true, Ordering::SeqCst) {
        return;
    }
    struct Reading;
    impl Drop for Reading {
        fn drop(&mut self) {
            REFRESHING.store(false, Ordering::SeqCst);
        }
    }
    let _reading = Reading;
    let turn = TURN.load(Ordering::SeqCst);
    let overview = crate::commands::pty_backend::terminal_server_overview(app);
    if !ENABLED.load(Ordering::SeqCst) || TURN.load(Ordering::SeqCst) != turn {
        return;
    }
    if let Err(error) = (AppTray { app: app.clone() }).update(&menu_model(&overview)) {
        log::debug!(target: "terminal_server_tray", "menu-bar update failed: {error}");
    }
}

/// The item in the macOS menu bar.
struct AppTray {
    app: AppHandle,
}

const ICON_RUNNING: &[u8] = include_bytes!("../../icons/tray/terminal-server-running.png");
const ICON_STOPPED: &[u8] = include_bytes!("../../icons/tray/terminal-server-stopped.png");
const ICON_ATTENTION: &[u8] = include_bytes!("../../icons/tray/terminal-server-attention.png");

fn icon_image(icon: TrayIcon) -> Result<tauri::image::Image<'static>, String> {
    let bytes = match icon {
        TrayIcon::Running => ICON_RUNNING,
        TrayIcon::Stopped => ICON_STOPPED,
        TrayIcon::Attention => ICON_ATTENTION,
    };
    tauri::image::Image::from_bytes(bytes).map_err(|error| error.to_string())
}

impl AppTray {
    fn menu(&self, model: &TrayMenuModel) -> Result<tauri::menu::Menu<tauri::Wry>, String> {
        use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
        let app = &self.app;
        let item = |id: String, label: &str, enabled: bool| {
            MenuItem::with_id(app, id, label, enabled, None::<&str>)
        };
        let status = item(format!("{MENU_ID_PREFIX}status"), &model.status, false)
            .map_err(|e| e.to_string())?;
        let open =
            item(MenuAction::Open.id(), "Open CodeSense", true).map_err(|e| e.to_string())?;
        let restart = item(
            MenuAction::Restart.id(),
            "Restart Terminal Server…",
            model.restart_enabled,
        )
        .map_err(|e| e.to_string())?;
        let stop_or_start = item(
            STOP_OR_START_ID.into(),
            model.stop_or_start.label,
            model.stop_or_start.enabled,
        )
        .map_err(|e| e.to_string())?;
        let quit =
            item(MenuAction::Quit.id(), "Quit CodeSense", true).map_err(|e| e.to_string())?;
        let separator = || PredefinedMenuItem::separator(app).map_err(|e| e.to_string());
        let (one, two, three) = (separator()?, separator()?, separator()?);
        Menu::with_items(
            app,
            &[
                &status,
                &one,
                &open,
                &two,
                &restart,
                &stop_or_start,
                &three,
                &quit,
            ],
        )
        .map_err(|e| e.to_string())
    }
}

impl TrayHost for AppTray {
    fn shown(&self) -> bool {
        self.app.tray_by_id(TRAY_ID).is_some()
    }

    fn show(&self, model: &TrayMenuModel) -> Result<(), String> {
        let menu = self.menu(model)?;
        tauri::tray::TrayIconBuilder::with_id(TRAY_ID)
            .icon(icon_image(model.icon)?)
            .icon_as_template(true)
            .tooltip("CodeSense terminal server")
            .menu(&menu)
            .show_menu_on_left_click(true)
            .on_tray_icon_event(|tray, event| {
                // Clicked (the menu opens): read it again.
                if matches!(event, tauri::tray::TrayIconEvent::Click { .. }) {
                    refresh_soon(tray.app_handle());
                }
            })
            .build(&self.app)
            .map_err(|error| error.to_string())?;
        *self.app.state::<NativeMenuState>().0.lock().unwrap() = Some((menu, model.clone()));
        Ok(())
    }

    fn update(&self, model: &TrayMenuModel) -> Result<(), String> {
        let app = self.app.clone();
        let model = model.clone();
        let turn = TURN.load(Ordering::SeqCst);
        // Mutate the existing NSMenu items on the main thread. Replacing the
        // entire menu would leave an already-open menu showing stale items.
        self.app.run_on_main_thread(move || {
            if !ENABLED.load(Ordering::SeqCst) || TURN.load(Ordering::SeqCst) != turn {
                return;
            }
            let state = app.state::<NativeMenuState>();
            let mut state = state.0.lock().unwrap();
            let Some((menu, previous)) = state.as_mut() else { return; };
            if *previous == model { return; }
            let update = || -> Result<(), String> {
                for (id, text, enabled) in [
                    (format!("{MENU_ID_PREFIX}status"), model.status.as_str(), false),
                    (MenuAction::Restart.id(), "Restart Terminal Server…", model.restart_enabled),
                    (STOP_OR_START_ID.into(), model.stop_or_start.label, model.stop_or_start.enabled),
                ] {
                    if let Some(item) = menu.get(&id).and_then(|item| item.as_menuitem().cloned()) {
                        item.set_text(text).map_err(|e| e.to_string())?;
                        item.set_enabled(enabled).map_err(|e| e.to_string())?;
                    }
                }
                if let Some(tray) = app.tray_by_id(TRAY_ID) {
                    if previous.icon != model.icon {
                        tray.set_icon(Some(icon_image(model.icon)?)).map_err(|e| e.to_string())?;
                        tray.set_icon_as_template(true).map_err(|e| e.to_string())?;
                    }
                    tray.set_title(model.count.map(|count| count.to_string())).map_err(|e| e.to_string())?;
                    tray.set_tooltip(Some(&model.status)).map_err(|e| e.to_string())?;
                }
                Ok(())
            };
            match update() {
                Ok(()) => *previous = model,
                Err(error) => log::debug!(target: "terminal_server_tray", "menu-bar update failed: {error}"),
            }
        }).map_err(|error| error.to_string())
    }

    fn remove(&self) {
        let _ = self.app.remove_tray_by_id(TRAY_ID);
        *self.app.state::<NativeMenuState>().0.lock().unwrap() = None;
    }
}

/// A click on one of the item's menu entries. The work runs off the main
/// thread: reads, confirmations and stops block.
fn on_menu_event(app: &AppHandle, id: &str) {
    let action = if id == STOP_OR_START_ID {
        app.state::<NativeMenuState>()
            .0
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, model)| model.stop_or_start.action)
    } else {
        MenuAction::from_id(id)
    };
    let Some(action) = action else {
        return;
    };
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("terminal-server-tray-action".into())
        .spawn(move || run_action(&app, action));
}

fn run_action(app: &AppHandle, action: MenuAction) {
    match action {
        MenuAction::Open => open_codesense(app),
        MenuAction::Quit => app.exit(0),
        MenuAction::Start => {
            if let Err(error) = crate::commands::pty_backend::start_terminal_server(app) {
                tell(app, &format!("Couldn't start the terminal server: {error}"));
            }
        }
        MenuAction::Stop | MenuAction::Restart => confirm_and_end(app, action),
    }
    refresh_now(app);
}

/// Bring CodeSense to the front, as a Dock click does.
fn open_codesense(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.show();
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// What the confirmation of `action` says, from the read it binds to.
pub(crate) fn confirmation_text(action: MenuAction, overview: &TerminalServerOverview) -> String {
    let verb = if action == MenuAction::Stop {
        "Stop"
    } else {
        "Restart"
    };
    let ends = crate::mcp::terminal_server_tools::ends_line(overview);
    let mut text = format!("{verb} the terminal server? It {ends}.");
    if action == MenuAction::Stop {
        text.push_str(" Nothing starts it again until a terminal is opened or you start it.");
    }
    text
}

/// Read the server, confirm what ends in a native dialog, and stop or
/// restart it bound to that read: the server refuses, and nothing ends, if
/// it or the instances connected to it changed before the person confirmed.
fn confirm_and_end(app: &AppHandle, action: MenuAction) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    let overview = crate::commands::pty_backend::terminal_server_overview(app);
    let incompatible = overview.state == TerminalServerRun::Incompatible;
    let expected = overview.expected.clone();
    if expected.is_none() && !(action == MenuAction::Restart && incompatible) {
        tell(
            app,
            "The terminal server can't be reviewed right now; nothing was ended.",
        );
        return;
    }
    let verb = if action == MenuAction::Stop {
        "Stop"
    } else {
        "Restart"
    };
    let confirmed = app
        .dialog()
        .message(confirmation_text(action, &overview))
        .title(format!("{verb} terminal server"))
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            verb.to_string(),
            "Cancel".to_string(),
        ))
        .blocking_show();
    if !confirmed {
        return;
    }
    let params = end_params(&overview);
    let sink = crate::commands::pty_backend::app_sink(app);
    let ended = if action == MenuAction::Stop {
        crate::mcp::terminal_server_tools::stop(
            app,
            params,
            sink,
            crate::terminal::session_server::terminate_sidecar,
        )
    } else {
        crate::mcp::terminal_server_tools::restart(
            app,
            params,
            sink,
            crate::terminal::session_server::terminate_sidecar,
            |root| crate::commands::pty_backend::launch_sidecar(app, root),
            || crate::commands::pty_backend::restart_terminal_server(app),
        )
    };
    if let Err(error) = ended {
        tell(
            app,
            &format!(
                "Couldn't {} the terminal server: {error}",
                verb.to_lowercase()
            ),
        );
    }
}

/// Bind the action to the same overview used for the confirmation text.
fn end_params(
    overview: &TerminalServerOverview,
) -> crate::mcp::terminal_server_tools::EndTerminalServerParams {
    crate::mcp::terminal_server_tools::EndTerminalServerParams {
        confirm_end_sessions: true,
        expected: overview.expected.clone().map(|expected| {
            crate::mcp::terminal_server_tools::TerminalServerExpected {
                server_id: expected.server_id,
                epoch: expected.epoch,
                pid: expected.pid,
                session_ids: expected.session_ids,
                other_instances: overview.other_instances,
            }
        }),
        reviewed_incompatible: (overview.state == TerminalServerRun::Incompatible).then(|| {
            crate::mcp::terminal_server_tools::ReviewedIncompatible::from_overview(overview)
        }),
    }
}

/// Tell the person something went wrong, in a native dialog.
fn tell(app: &AppHandle, message: &str) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
    app.dialog()
        .message(message)
        .title("Terminal server")
        .kind(MessageDialogKind::Error)
        .blocking_show();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    fn overview(state: TerminalServerRun) -> TerminalServerOverview {
        TerminalServerOverview::bare(state, None)
    }

    fn running(sessions: u32, agents: u32) -> TerminalServerOverview {
        let mut overview = overview(TerminalServerRun::Running);
        overview.sessions = Some(sessions);
        overview.running_agents = Some(agents);
        overview.ends = Some(sessions);
        overview.other_instances = Some(0);
        overview.expected = Some(crate::terminal::session_server::LocalActivityUpgrade {
            server_id: "s".into(),
            epoch: "e".into(),
            pid: 7,
            session_ids: (0..sessions).map(|n| format!("s{n}")).collect(),
        });
        overview
    }

    /// A running server: its status line, the agent count beside the icon,
    /// and Restart and Stop enabled (bound to what was read).
    #[test]
    fn sv8_a_running_server_shows_its_sessions_and_agents() {
        let model = menu_model(&running(5, 2));
        assert_eq!(model.icon, TrayIcon::Running);
        assert_eq!(model.count, Some(2));
        assert_eq!(
            model.status,
            "Terminal server running · 5 sessions · 2 agents running"
        );
        assert!(model.restart_enabled);
        assert_eq!(model.stop_or_start.action, MenuAction::Stop);
        assert!(model.stop_or_start.enabled);

        let quiet = menu_model(&running(1, 0));
        assert_eq!(quiet.count, None, "no count with no agent running");
        assert_eq!(quiet.status, "Terminal server running · 1 session");

        let mut unbound = running(3, 0);
        unbound.expected = None;
        let model = menu_model(&unbound);
        assert!(!model.restart_enabled && !model.stop_or_start.enabled);
    }

    #[test]
    fn sv8_unknown_counts_are_not_reported_as_zero() {
        let model = menu_model(&overview(TerminalServerRun::Running));
        assert_eq!(model.status, "Terminal server running");
        assert_eq!(model.count, None);
        assert!(!model.restart_enabled && !model.stop_or_start.enabled);
        assert_eq!(
            menu_model(&running(1, 1)).status,
            "Terminal server running · 1 session · 1 agent running"
        );
    }

    #[test]
    fn sv8_template_icons_are_distinct_and_have_transparency() {
        let icons = [TrayIcon::Running, TrayIcon::Stopped, TrayIcon::Attention]
            .map(|icon| icon_image(icon).unwrap());
        for icon in &icons {
            assert!(icon.width() > 0 && icon.height() > 0);
            assert!(icon.rgba().chunks_exact(4).any(|pixel| pixel[3] == 0));
            assert!(icon.rgba().chunks_exact(4).any(|pixel| pixel[3] > 0));
        }
        assert_ne!(icons[0].rgba(), icons[1].rgba());
        assert_ne!(icons[0].rgba(), icons[2].rgba());
        assert_ne!(icons[1].rgba(), icons[2].rgba());
    }

    /// A stopped server offers Start; one that needs attention offers what
    /// can be done, and its icon says so.
    #[test]
    fn sv8_stopped_and_attention_states() {
        let stopped = menu_model(&overview(TerminalServerRun::Stopped));
        assert_eq!(stopped.icon, TrayIcon::Stopped);
        assert_eq!(stopped.status, "Terminal server not running");
        assert!(!stopped.restart_enabled);
        assert_eq!(stopped.stop_or_start.action, MenuAction::Start);
        assert_eq!(stopped.stop_or_start.label, "Start Terminal Server");
        assert!(stopped.stop_or_start.enabled);

        let incompatible = menu_model(&overview(TerminalServerRun::Incompatible));
        assert_eq!(incompatible.icon, TrayIcon::Attention);
        assert!(incompatible.restart_enabled);
        assert!(!incompatible.stop_or_start.enabled);

        let unreachable = menu_model(&overview(TerminalServerRun::Unreachable));
        assert_eq!(unreachable.icon, TrayIcon::Attention);
        assert!(!unreachable.restart_enabled && !unreachable.stop_or_start.enabled);

        let in_process = menu_model(&overview(TerminalServerRun::NotApplicable));
        assert!(!in_process.restart_enabled && !in_process.stop_or_start.enabled);
    }

    /// The confirmation names what ends, as SV-7's does.
    #[test]
    fn sv8_the_confirmation_names_what_ends() {
        let mut read = running(4, 2);
        read.other_instances = Some(1);
        assert_eq!(
            confirmation_text(MenuAction::Restart, &read),
            "Restart the terminal server? It ends 4 running sessions, 2 running agents among them; 1 other CodeSense instance is connected, whose tabs end too."
        );
        assert!(confirmation_text(MenuAction::Stop, &running(1, 0))
            .starts_with("Stop the terminal server? It ends 1 running session."));
    }

    #[test]
    fn incompatible_confirmation_passes_the_reviewed_identity() {
        let mut read = overview(TerminalServerRun::Incompatible);
        read.pid = Some(42);
        read.ends = Some(2);
        read.other_instances = Some(1);
        let params = end_params(&read);
        assert!(params.confirm_end_sessions);
        assert!(params.expected.is_none());
        let reviewed = params.reviewed_incompatible.unwrap();
        assert_eq!(reviewed.pid, Some(42));
        assert_eq!(reviewed.sessions, Some(2));
        assert_eq!(reviewed.other_instances, Some(1));
        read.ends = None;
        assert_eq!(
            end_params(&read).reviewed_incompatible.unwrap().sessions,
            None
        );
        assert!(end_params(&running(2, 0)).reviewed_incompatible.is_none());
    }

    /// Menu ids round-trip, and only the item's own are taken.
    #[test]
    fn sv8_menu_ids_are_the_items_own() {
        for action in [
            MenuAction::Open,
            MenuAction::Restart,
            MenuAction::Stop,
            MenuAction::Start,
            MenuAction::Quit,
        ] {
            assert_eq!(MenuAction::from_id(&action.id()), Some(action));
        }
        assert_eq!(MenuAction::from_id("open"), None);
        assert_eq!(MenuAction::from_id("terminal-server-tray:status"), None);
    }

    #[derive(Default)]
    struct Recorded {
        shown: Cell<bool>,
        shows: Cell<u32>,
        removes: Cell<u32>,
        last: RefCell<Option<TrayMenuModel>>,
    }

    impl TrayHost for Recorded {
        fn shown(&self) -> bool {
            self.shown.get()
        }
        fn show(&self, model: &TrayMenuModel) -> Result<(), String> {
            self.shown.set(true);
            self.shows.set(self.shows.get() + 1);
            *self.last.borrow_mut() = Some(model.clone());
            Ok(())
        }
        fn update(&self, model: &TrayMenuModel) -> Result<(), String> {
            *self.last.borrow_mut() = Some(model.clone());
            Ok(())
        }
        fn remove(&self) {
            self.shown.set(false);
            self.removes.set(self.removes.get() + 1);
        }
    }

    /// Turning the setting on adds the item once (reading until the first
    /// read answers); turning it off removes it at once.
    #[test]
    fn sv8_the_setting_adds_and_removes_the_item() {
        let host = Recorded::default();
        assert!(
            !toggle(&host, false).unwrap(),
            "off by default: nothing shows"
        );
        assert_eq!(host.shows.get(), 0);
        assert!(toggle(&host, true).unwrap());
        assert!(toggle(&host, true).unwrap());
        assert_eq!(host.shows.get(), 1, "shown once");
        assert_eq!(
            host.last.borrow().as_ref().unwrap().status,
            "Reading the terminal server…"
        );
        assert!(!toggle(&host, false).unwrap());
        assert_eq!(host.removes.get(), 1, "removed at once");
        assert!(!toggle(&host, false).unwrap());
        assert_eq!(host.removes.get(), 1);
    }
}
