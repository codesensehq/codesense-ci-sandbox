// SPDX-License-Identifier: MIT
use codesense_host_proto::agent_cli::NativeLaunch;
use codesense_pty_core::domain::{PtySessionId, SubmitOutcome, TerminalText, ViewportId};
use codesense_pty_core::emulator::{TerminalSnapshot, TextMode};
use codesense_pty_core::spawn::SpawnIntent;
use codesense_pty_core::{is_warm_safe_cwd, SessionMeta, SessionRegistration};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};

use tauri::Manager;

use crate::commands::async_helpers::await_blocking;
use crate::commands::pty_backend::{
    hosting_in_process, managed_host_environment, register_in_process, resolve_handoff_backend,
    resolve_in_process, resolve_live_session_backend, resolve_session_backend,
    resolve_spawn_backend, spawn_on_session_server, SpawnPlacement,
};
use crate::commands::pty_layout::app_viewer;
use crate::terminal::warm_pool::WarmPoolState;

pub(crate) fn transfer_fallback_scope<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    creation: &super::mcp_scope::ScopeCreation,
    old: &super::pty_host::LocalEffectClaim,
    next: &super::pty_host::LocalEffectClaim,
) -> Result<(), String> {
    super::mcp_scope::transfer_scope_creation(creation, old, next).inspect_err(|_| {
        app.state::<crate::terminal::session_server::SessionServerState>()
            .admission
            .effects
            .never_started(next);
    })
}

/// Whether `data` is only reports a terminal sends by itself, never a keystroke: focus
/// in/out (`ESC [I` / `ESC [O`), a cursor-position, device-attributes or mode reply
/// (`ESC [ … R` / `ESC [ … c` / `ESC [ … $ y`), an OSC reply (`ESC ] … BEL` or
/// `ESC ] … ESC \`), or a DCS reply such as a settings report (`ESC P … ESC \`).
fn is_terminal_report(data: &str) -> bool {
    let mut rest = data.as_bytes();
    if rest.is_empty() {
        return true;
    }
    while !rest.is_empty() {
        match rest {
            [0x1b, b'[', tail @ ..] => {
                let params = tail
                    .iter()
                    .take_while(|b| b.is_ascii_digit() || matches!(b, b';' | b'?' | b'>' | b'$'))
                    .count();
                match tail.get(params) {
                    Some(b'I' | b'O') if params == 0 => {}
                    Some(b'R' | b'c') if params > 0 => {}
                    Some(b'y') if params > 0 && tail[params - 1] == b'$' => {}
                    _ => return false,
                }
                rest = &tail[params + 1..];
            }
            [0x1b, b']', tail @ ..] => {
                let Some(end) = tail.iter().position(|&b| b == 0x07 || b == 0x1b) else {
                    return false;
                };
                let skip = if tail[end] == 0x1b {
                    if tail.get(end + 1) != Some(&b'\\') {
                        return false;
                    }
                    end + 2
                } else {
                    end + 1
                };
                rest = &tail[skip..];
            }
            [0x1b, b'P', tail @ ..] => {
                let Some(end) = tail.windows(2).position(|pair| pair == [0x1b, b'\\']) else {
                    return false;
                };
                rest = &tail[end + 2..];
            }
            _ => return false,
        }
    }
    true
}

/// The app's one path for typed terminal input (`write_pty`, and SW-DP-23's
/// owner control from a dashboard): `pty_backend::with_session_reference` →
/// `TerminalDomain::write`, through the app's own session-server connection
/// (which holds the writable claim), then the typed-input bookkeeping: a
/// keystroke a person typed (not a report the terminal sends by itself) keeps a
/// `maxRuntime` session running while they use it
/// (`DashboardLaunchState::note_human_input`). The caller has authorized
/// `reference` already. The bytes are never logged.
pub(crate) async fn write_session_input(
    app: tauri::AppHandle,
    reference: crate::commands::machines::RuntimeSessionReference,
    data: String,
    remote: bool,
    human: HumanInput,
) -> Result<(), String> {
    let typed = counts_as_human(remote, human, &data);
    let session = reference.session().to_string();
    let handle = app.clone();
    await_blocking("write_pty", move || {
        crate::commands::pty_backend::with_session_reference(
            &app,
            &reference,
            remote,
            |host, session| host.write(session, data.as_bytes()).map_err(String::from),
        )
    })
    .await?;
    if typed {
        record_human_input(&handle, &session, super::dashboard_launch::now_epoch_ms());
    }
    Ok(())
}

fn record_human_input<R: tauri::Runtime>(app: &tauri::AppHandle<R>, session: &str, now_ms: u64) {
    if let Some(state) = app.try_state::<super::dashboard_launch::DashboardLaunchState>() {
        if state.note_human_input(session, now_ms) {
            super::dashboard_launch::emit_session_state_changed(app);
        }
    }
}

/// `text` with every bracketed-paste marker removed, split ones too (removing one can join
/// another), in one linear pass: each marker found at the end of what is kept so far is dropped.
/// A marker starts with ESC and none of its tails does, so this equals removing until none is left.
pub(crate) fn strip_paste_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        out.push(ch);
        if ch == '~' && (out.ends_with("\x1b[200~") || out.ends_with("\x1b[201~")) {
            out.truncate(out.len() - 6);
        }
    }
    out
}

/// A paste as the bytes to type (SW-DP-23 `paste`): bracketed-paste markers in
/// it are removed (`strip_paste_markers`, so a marker split around another goes too)
/// so it cannot end the paste early, and text with a line break goes as one
/// bracketed paste (the `submit_line` rule for multi-line text), so its line
/// breaks do not submit it piecemeal. A single line goes as typed.
pub(crate) fn bracketed_paste(text: &str) -> String {
    const START: &str = "\x1b[200~";
    const END: &str = "\x1b[201~";
    let text = strip_paste_markers(text);
    if text.contains(['\r', '\n']) {
        format!("{START}{text}{END}")
    } else {
        text
    }
}

/// Whether a write is a person's input, for `note_human_input`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HumanInput {
    /// A pane's write (`write_pty`): reports the terminal sends by itself
    /// (focus, cursor-position replies) are told apart from keystrokes.
    Detect,
    /// The owner's input from a dashboard (SW-DP-23): every `keys` or `paste`
    /// frame is something they typed (the toolkit already keeps its terminal's
    /// own reports out), even one that looks like a report (Shift+F3 is
    /// `ESC [1;2R`).
    Typed,
}

/// Whether `data` keeps a `maxRuntime` session running (`note_human_input`).
pub(crate) fn counts_as_human(remote: bool, human: HumanInput, data: &str) -> bool {
    !remote
        && match human {
            HumanInput::Typed => true,
            HumanInput::Detect => !is_terminal_report(data),
        }
}

/// The app's one resize path (`resize_pty` as the `app` viewer, SW-DP-23 as
/// the `dashboard` viewer): `with_session_reference` → `TerminalDomain::resize`.
/// The PTY follows the most recently active viewer that asked for a size.
pub(crate) async fn resize_session_as(
    app: tauri::AppHandle,
    reference: crate::commands::machines::RuntimeSessionReference,
    viewer: codesense_pty_core::domain::ViewerId,
    cols: u16,
    rows: u16,
    remote: bool,
) -> Result<(), String> {
    await_blocking("resize_pty", move || {
        crate::commands::pty_backend::with_session_reference(
            &app,
            &reference,
            remote,
            |host, session| {
                host.resize(&viewer, session, cols, rows)
                    .map_err(String::from)
            },
        )
    })
    .await
}

/// Every non-keyboard terminal operation is admitted here before its submit future is polled.
/// Native keyboard writes remain on write_session_input.
pub(crate) async fn submit_automatic_input<T>(
    app: tauri::AppHandle,
    session: crate::commands::machines::RuntimeSessionReference,
    submit: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    automatic_input_checked(
        crate::session_usage::spend_caps::refuse_session_input(&app, session.session()),
        submit,
    )
    .await
}

async fn automatic_input_checked<T>(
    permission: impl std::future::Future<Output = Result<(), String>>,
    submit: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    permission.await?;
    submit.await
}

pub(crate) async fn write_automatic_input(
    app: tauri::AppHandle,
    session: crate::commands::machines::RuntimeSessionReference,
    bytes: Vec<u8>,
) -> Result<(), String> {
    submit_automatic_input(
        app.clone(),
        session.clone(),
        await_blocking("automatic_input", move || {
            crate::commands::pty_backend::with_session_reference(
                &app,
                &session,
                false,
                |host, id| host.write(id, &bytes).map_err(String::from),
            )
        }),
    )
    .await
}

/// Scoped IPC references never enter the legacy resolver after admission expires.
pub mod admitted_commands {
    use super::*;
    use crate::commands::machines::RuntimeSessionReference;
    use crate::commands::pty_backend::{subscribe_reference_authorized, with_session_reference};

    #[tauri::command]
    pub async fn write_pty(
        app: tauri::AppHandle,
        session_id: RuntimeSessionReference,
        data: String,
    ) -> Result<(), String> {
        let remote = super::super::pty_layout::authorize_human_terminal(&app, &session_id).await?;
        super::write_session_input(app, session_id, data, remote, super::HumanInput::Detect).await
    }

    #[tauri::command]
    pub async fn submit_pty_line(
        app: tauri::AppHandle,
        session_id: RuntimeSessionReference,
        line: String,
        quiet_ms: u64,
        confirm_ms: u64,
        timeout_ms: u64,
        max_nudges: u32,
    ) -> Result<SubmitOutcome, String> {
        submit_automatic_input(
            app.clone(),
            session_id.clone(),
            await_blocking("submit_pty_line", move || {
                with_session_reference(&app, &session_id, false, |host, session| {
                    host.submit_line(session, &line, quiet_ms, confirm_ms, timeout_ms, max_nudges)
                        .map_err(String::from)
                })
            }),
        )
        .await
    }

    #[tauri::command]
    pub async fn resize_pty(
        app: tauri::AppHandle,
        session_id: RuntimeSessionReference,
        cols: u16,
        rows: u16,
    ) -> Result<(), String> {
        let remote = super::super::pty_layout::authorize_human_terminal(&app, &session_id).await?;
        super::resize_session_as(app, session_id, app_viewer(), cols, rows, remote).await
    }

    #[tauri::command]
    pub async fn subscribe_terminal(
        app: tauri::AppHandle,
        pty_id: RuntimeSessionReference,
        stream: Option<u64>,
    ) -> Result<SubscribeResponse, String> {
        let remote = super::super::pty_layout::authorize_human_terminal(&app, &pty_id).await?;
        await_blocking("subscribe_terminal", move || {
            let result = subscribe_reference_authorized(
                &app,
                &pty_id,
                &ViewportId::new("webview"),
                stream,
                remote,
            )?;
            Ok(SubscribeResponse {
                private_authority_epoch: if remote {
                    crate::commands::pty_backend::resolve_session_reference(&app, &pty_id, true)?
                        .client()
                        .and_then(|client| client.private_lease())
                        .map(|lease| lease.epoch())
                } else {
                    None
                },
                snapshot: result.snapshot,
                stream_id: result.stream_id.0,
                story_id: result.story_id,
                workspace_id: result.workspace_id,
                epic_id: result.epic_id,
            })
        })
        .await
    }

    #[tauri::command]
    pub async fn read_terminal_text(
        app: tauri::AppHandle,
        pty_id: RuntimeSessionReference,
        lines: u32,
        mode: TextMode,
    ) -> Result<TerminalText, String> {
        await_blocking("read_terminal_text", move || {
            with_session_reference(&app, &pty_id, false, |host, session| {
                host.read_terminal_text(session, lines, mode)
                    .map_err(String::from)
            })
        })
        .await
    }

    #[tauri::command]
    pub async fn session_is_idle(
        app: tauri::AppHandle,
        pty_id: RuntimeSessionReference,
    ) -> Result<bool, String> {
        await_blocking("session_is_idle", move || {
            with_session_reference(&app, &pty_id, false, |host, session| {
                host.is_idle(session).map_err(String::from)
            })
        })
        .await
    }

    #[tauri::command]
    pub async fn session_has_live_agent(
        app: tauri::AppHandle,
        pty_id: RuntimeSessionReference,
        agent_name: String,
    ) -> Result<bool, String> {
        await_blocking("session_has_live_agent", move || {
            with_session_reference(&app, &pty_id, true, |host, session| {
                named_agent_alive(host, session, &agent_name)
            })
        })
        .await
    }

    /// SW-TP (CAP-11 / CAP-13): the working directory `session` last reported.
    /// Lazy-on-demand resolver for the per-tab toolbar's cwd slot — called when
    /// a caption mounts or a cwd-dependent action fires, never polled in a
    /// background loop. `None` when the host doesn't know the cwd yet (e.g.
    /// a brand-new session that hasn't emitted OSC 7 and whose host hasn't
    /// probed its working directory).
    #[tauri::command]
    pub async fn session_cwd_cmd(
        app: tauri::AppHandle,
        session_id: String,
    ) -> Result<Option<String>, String> {
        let session_id_for_await = session_id.clone();
        await_blocking("session_cwd_cmd", move || {
            Ok(
                crate::commands::pty_backend::session_cwd(&app, &session_id_for_await)
                    .map(|p| p.to_string_lossy().into_owned()),
            )
        })
        .await
    }

    /// The terminal caption's prompt segment for `pty_id` — its cwd, the host
    /// account's home and the Git status of the checkout the cwd is in — read
    /// on the host that runs the session (a remote tab's Machine included).
    /// Polled by the caption while visible; each host caches per path.
    #[tauri::command]
    pub async fn session_prompt_cmd(
        app: tauri::AppHandle,
        pty_id: RuntimeSessionReference,
    ) -> Result<codesense_pty_core::git_prompt::SessionPrompt, String> {
        await_blocking("session_prompt_cmd", move || {
            crate::commands::pty_backend::session_prompt(&app, &pty_id)
        })
        .await
    }

    #[tauri::command]
    pub async fn sessions_running_processes(
        app: tauri::AppHandle,
        pty_ids: Vec<RuntimeSessionReference>,
    ) -> Result<Vec<codesense_pty_core::process::RunningJobs>, String> {
        await_blocking("sessions_running_processes", move || {
            let mut pids = Vec::with_capacity(pty_ids.len());
            for reference in &pty_ids {
                pids.push(with_session_reference(
                    &app,
                    reference,
                    false,
                    |host, session| {
                        Ok(host
                            .live_child_pids(std::slice::from_ref(session))?
                            .first()
                            .copied()
                            .flatten())
                    },
                )?);
            }
            Ok(codesense_pty_core::process::running_jobs(&pids))
        })
        .await
    }
}

/// Terminal dimensions of a spawn, as the frontend's `{ cols, rows }`.
#[derive(Debug, Deserialize)]
pub struct SpawnPtySize {
    pub cols: u16,
    pub rows: u16,
}

/// The inputs of one PTY spawn as the frontend sends them: a layout tab's cold
/// spawn source.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnPtyArgs {
    pub command: String,
    /// SW-WA-F10: argv to spawn as PID-1 of the PTY — bypasses the shell
    /// wrapper. When set, [`Self::command`] / [`Self::interactive`] no longer
    /// pick the session's PID-1. Set by the managed launch so a saved-agent
    /// session runs the agent CLI directly, no interactive shell in front of
    /// it (SW-WA-F9 root cause: `.zshrc` init races the launch-settle gate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_argv: Option<Vec<String>>,
    pub cwd: String,
    pub size: SpawnPtySize,
    pub interactive: Option<bool>,
    pub starship_preset: Option<String>,
    pub story_id: Option<String>,
    pub workspace_id: Option<String>,
    pub epic_id: Option<String>,
    /// The display's scrollback lines; the emulator terminal keeps as many.
    pub scrollback: Option<u32>,
    pub shell_integration: Option<bool>,
    pub env_remove: Option<Vec<String>>,
    pub account: Option<crate::commands::agent_accounts::ResolveAgentAccountInput>,
    pub environment_binding: Option<String>,
    /// SW-WA-F10: the native-launch provider the spawn runs — `Claude` /
    /// `Codex` for the managed-launch argv-direct path, so the host reserves
    /// an activity invocation and binds [`SOCKET_ENV`] / [`TOKEN_ENV`] onto
    /// the child before exec. `None` is the default (no native binding).
    ///
    /// [`SOCKET_ENV`]: codesense_pty_core::activity::SOCKET_ENV
    /// [`TOKEN_ENV`]: codesense_pty_core::activity::TOKEN_ENV
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_launch: Option<NativeLaunch>,
}

/// Spawn `session_id` from `args` in `host`: resolve the account intent into
/// its one-use launch binding, then spawn and register the session.
pub(crate) async fn spawn_from_args(
    app: &tauri::AppHandle,
    session_id: String,
    args: SpawnPtyArgs,
    host: SpawnPlacement,
) -> Result<(), String> {
    let SpawnPtyArgs {
        command,
        command_argv,
        cwd,
        size,
        interactive,
        starship_preset,
        story_id,
        workspace_id,
        epic_id,
        scrollback,
        shell_integration,
        env_remove,
        account,
        environment_binding,
        native_launch,
    } = args;
    let account_binding = if let Some(input) = account {
        let (resolved, binding) =
            crate::commands::agent_accounts::resolve_for_launch(app, input).await?;
        if !resolved.spawn_eligible {
            return Err(crate::agents::launch::account_refusal(resolved.reason));
        }
        Some(binding)
    } else {
        None
    };
    // RF-15: fold the three correlation keys into one value the moment they
    // leave the flat wire signature.
    let meta = SessionMeta {
        story_id,
        workspace_id,
        epic_id,
    };
    spawn_pty_inner(
        app,
        session_id,
        command,
        command_argv,
        cwd,
        size,
        interactive,
        starship_preset,
        meta,
        scrollback,
        shell_integration,
        env_remove,
        account_binding,
        environment_binding,
        native_launch,
        false,
        host,
    )
    .await
}

/// SW-LN-35: the prompt preset when neither the spawn nor Settings names one —
/// the user's own prompt (Starship presets are opt-in in Settings).
const DEFAULT_PROMPT_PRESET: &str = "plain";

/// SW-LN-35: the spawn's own preset, else the configured Settings preset, else
/// [`DEFAULT_PROMPT_PRESET`].
fn effective_prompt_preset(requested: Option<String>, configured: Option<String>) -> String {
    requested
        .or(configured)
        .unwrap_or_else(|| DEFAULT_PROMPT_PRESET.to_string())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_remote_spawn_intent(
    command: String,
    command_argv: Option<Vec<String>>,
    cwd: String,
    cols: u16,
    rows: u16,
    interactive: Option<bool>,
    shell_integration: Option<bool>,
    env_remove: Option<Vec<String>>,
) -> SpawnIntent {
    SpawnIntent {
        remote_launch: Default::default(),
        // SW-WA-F10: argv-direct overrides the shell command — the host runs
        // argv[0] as PID-1 of the PTY, no shell wrapper.
        command: command_argv
            .is_none()
            .then(|| (interactive != Some(true)).then_some(command))
            .flatten(),
        argv: command_argv,
        shell: None,
        cwd,
        cols,
        rows,
        shell_integration: shell_integration.unwrap_or(true),
        starship_preset: None,
        api_port: None,
        env_remove: env_remove.unwrap_or_default(),
        env: Vec::new(),
        agent_env: Vec::new(),
        login_path: None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_local_spawn_intent(
    command: String,
    command_argv: Option<Vec<String>>,
    cwd: String,
    cols: u16,
    rows: u16,
    interactive: Option<bool>,
    shell_integration: Option<bool>,
    env_remove: Option<Vec<String>>,
    shell: Option<String>,
    preset: String,
    api_port: Option<u16>,
) -> SpawnIntent {
    SpawnIntent {
        remote_launch: Default::default(),
        // SW-WA-F10: argv-direct overrides the shell command — the host runs
        // argv[0] as PID-1 of the PTY, no shell wrapper.
        command: command_argv
            .is_none()
            .then(|| (interactive != Some(true)).then_some(command))
            .flatten(),
        argv: command_argv,
        shell,
        cwd,
        cols,
        rows,
        shell_integration: shell_integration.unwrap_or(true),
        starship_preset: Some(preset),
        api_port,
        env_remove: env_remove.unwrap_or_default(),
        env: Vec::new(),
        agent_env: Vec::new(),
        login_path: None,
    }
}

/// Fold a spawn's consumed one-use bindings into its intent: the account's and the
/// launch's variables over everything the host sets, a saved agent's own under it.
/// Returns the MCP scope the launch created, for the caller to bind.
pub(crate) fn apply_bindings(
    intent: &mut SpawnIntent,
    account_env: Vec<(String, String)>,
    account_env_remove: Vec<String>,
    launch: crate::commands::agent_accounts::LaunchEnvironment,
) -> Option<crate::commands::mcp_scope::ScopeCreation> {
    intent.env.extend(account_env);
    intent.env_remove.extend(account_env_remove);
    intent.env.extend(launch.values);
    intent.agent_env = launch.agent_env;
    launch.scope
}

/// Resolve a producer's emitted request through the real local intent and binding consumers.
#[cfg(test)]
pub(crate) fn consume_emitted_spawn_for_test<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    args: SpawnPtyArgs,
    session: &str,
    environment: &codesense_pty_core::spawn::HostEnvironment,
) -> Result<(codesense_pty_core::runner::PtySpec, Vec<(String, String)>), String> {
    let mut intent = build_local_spawn_intent(
        args.command,
        args.command_argv,
        args.cwd,
        args.size.cols,
        args.size.rows,
        args.interactive,
        args.shell_integration,
        args.env_remove,
        None,
        DEFAULT_PROMPT_PRESET.into(),
        None,
    );
    intent.remote_launch = args.native_launch;
    let launch = crate::commands::agent_accounts::consume_environment_binding(
        app,
        args.environment_binding.as_deref(),
    )?;
    let values = launch.values.clone();
    apply_bindings(&mut intent, Vec::new(), Vec::new(), launch);
    Ok((intent.resolve(session, environment), values))
}

/// A local shell tab with CodeSense's shell integration exports what a `claude` or
/// `codex` typed in it needs to start connected to CodeSense (`tab_env`); the
/// integration's functions add it. A command or a plain shell gets nothing.
fn with_tab_agents(
    mut intent: SpawnIntent,
    tab_env: impl FnOnce() -> Vec<(String, String)>,
) -> SpawnIntent {
    if intent.osc_enabled() {
        intent.env.extend(tab_env());
    }
    intent
}

/// A local agent launched without a shell (argv-direct) builds its PATH on the
/// user's login-shell PATH, so its hooks and MCP servers find what the user's
/// terminal finds. `login_path` is only asked for such a launch; it may block
/// on the bounded login-shell probe. When it has no value the host's own PATH
/// stays, with one warning. A shell tab's login shell sets its own PATH.
pub(crate) fn with_login_path(
    mut intent: SpawnIntent,
    login_path: impl FnOnce() -> Option<String>,
) -> SpawnIntent {
    if intent.argv.is_some() {
        intent.login_path = login_path();
        if intent.login_path.is_none() {
            warn!(target: "pty", "agent launch: login-shell PATH unavailable, using the app's PATH");
        }
    }
    intent
}

/// SW-WA-F10: reserve a native-launch activity invocation on the in-process
/// host BEFORE the spawn, returning the [`Writer`] the caller binds onto the
/// child's env ([`SOCKET_ENV`] / [`TOKEN_ENV`]). `None` when the in-process
/// host or its activity service is not managed yet, or when the owner refuses
/// the reservation; the spawn still runs — the agent just has no activity
/// reporting. Mirrors the sidecar's `arm_prepare` / `spawn_and_register` path.
///
/// [`Writer`]: codesense_pty_core::activity::Writer
/// [`SOCKET_ENV`]: codesense_pty_core::activity::SOCKET_ENV
/// [`TOKEN_ENV`]: codesense_pty_core::activity::TOKEN_ENV
fn native_prep_in_process(
    app: &tauri::AppHandle,
    session_id: &str,
    provider: codesense_pty_core::agent_cli::AgentCli,
) -> Option<codesense_pty_core::activity::Writer> {
    let host = crate::commands::pty_backend::in_process_host_handle(app)?;
    let service = app.try_state::<codesense_pty_core::activity::ActivityService>()?;
    let socket = service.path().to_string_lossy().into_owned();
    host.activity()
        .prepare_native(session_id, provider, &socket)
        .ok()
}

fn prepare_send_bindings<T>(
    mut revalidate: impl FnMut() -> Result<(), String>,
    validate: impl FnOnce() -> Result<(), String>,
    consume_once: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    revalidate()?;
    validate()?;
    let consumed = consume_once()?;
    // This is the exact-client send-boundary check. A failure here deliberately
    // consumes the one-use capability once but stages no provenance and sends
    // no bytes; retry must mint a fresh binding against the new generation.
    revalidate()?;
    Ok(consumed)
}

/// PT-8: the spawn engine shared by a layout tab's command spawn, the warm-pool
/// fill, and the cold fallback of a warm spawn: state the spawn as a
/// [`SpawnIntent`] and consume the one-use account binding into it (the
/// app-side half), then hand it to `host`, which resolves it against its own
/// environment, spawns and registers the session. `is_pool_member` marks an
/// unattached warm-pool shell.
///
/// RF-15 folded the correlation triplet into [`SessionMeta`]; the remaining args
/// (`command`/`cwd`/`size`/`interactive`/`starship_preset`/replay+shell flags/
/// `env_remove`/`is_pool_member`) are distinct spawn inputs with no further
/// natural grouping, so the `#[allow]` stays over refactoring a stable engine
/// signature just for the lint (project-context "structural lints" convention).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn spawn_pty_inner(
    app: &tauri::AppHandle,
    session_id: String,
    command: String,
    command_argv: Option<Vec<String>>,
    cwd: String,
    size: SpawnPtySize,
    interactive: Option<bool>,
    starship_preset: Option<String>,
    meta: SessionMeta,
    scrollback: Option<u32>,
    shell_integration: Option<bool>,
    // SW-GS-2a (AC-7): inherited env vars stripped from the child. Default None → unaffected.
    env_remove: Option<Vec<String>>,
    account_binding: Option<crate::commands::agent_accounts::LaunchBinding>,
    environment_binding: Option<String>,
    // SW-WA-F10: the native-launch provider for the spawn, so the host
    // reserves an activity invocation and binds SOCKET_ENV / TOKEN_ENV onto
    // the child before exec. `None` is the default (no native binding).
    native_launch: Option<NativeLaunch>,
    is_pool_member: bool,
    host: SpawnPlacement,
) -> Result<(), String> {
    let SpawnPtySize { cols, rows } = size;
    let may_retry_in_process = matches!(&host, SpawnPlacement::ResolveLocal);
    info!(target: "pty", "spawn_pty: session_id={}, command={}, cwd={}", session_id, command, cwd);
    debug!(target: "pty", "spawn_pty: opening PTY with cols={}, rows={}", cols, rows);
    // What to run; the host resolves the shell, its environment and the
    // shell-integration resources where the child runs. PT-5: shell
    // integration (OSC 7/133 + inline prompt) defaults on; only an interactive
    // shell gets the injected init.
    // SW-WL-6-A (H2): connecting the host and re-reading the account binding
    // may block, so they run on a blocking worker and opening a terminal never
    // freezes the UI thread.
    let (
        intent,
        provenance,
        backend,
        environment,
        mut effect_claim,
        scope_creation,
        _workspace_admission,
    ) = {
        let app = app.clone();
        let session_id = session_id.clone();
        await_blocking("spawn_pty", move || {
            // Where the session is hosted; connecting (or launching) the sidecar
            // may block. It is fallible (an incompatible sidecar refuses the
            // spawn), so it runs before the one-use bindings are consumed. A
            // layout tab's host was resolved before its reservation: it is
            // pinned.
            let backend = match host {
                SpawnPlacement::ResolveLocal => resolve_spawn_backend(
                    &app,
                    crate::commands::machines::LOCAL_MACHINE_ID,
                    &session_id,
                )
                .inspect_err(|_| {
                    if !is_pool_member {
                        crate::commands::pty_backend::refused_open(&app);
                    }
                })?,
                SpawnPlacement::Exact(backend) => *backend,
            };
            backend.revalidate(&app)?;
            let remote = backend.is_remote();
            // Architecture: local admission stays pending through sidecar or
            // in-process registration, using the same fence as Mac cleanup.
            let workspace_admission = if remote {
                None
            } else {
                Some(codesense_pty_core::remote_workspace::removal::admit_spawn(
                    &cwd,
                )?)
            };
            let mut intent = if remote {
                build_remote_spawn_intent(
                    command,
                    command_argv,
                    cwd,
                    cols,
                    rows,
                    interactive,
                    shell_integration,
                    env_remove,
                )
            } else {
                let shell = {
                    let member_shell = is_pool_member
                        .then(|| {
                            app.try_state::<WarmPoolState>()
                                .and_then(|pool| pool.member_shell(&session_id))
                        })
                        .flatten();
                    crate::commands::user_shell::spawn_shell(
                        member_shell,
                        crate::commands::user_shell::current_override,
                    )
                };
                let preset = effective_prompt_preset(
                    starship_preset,
                    app.try_state::<WarmPoolState>()
                        .and_then(|pool| pool.configured_preset()),
                );
                let api_port = app
                    .try_state::<crate::http::HttpApiPort>()
                    .map(|port| port.0);
                with_login_path(
                    with_tab_agents(
                        build_local_spawn_intent(
                            command,
                            command_argv,
                            cwd,
                            cols,
                            rows,
                            interactive,
                            shell_integration,
                            env_remove,
                            shell,
                            preset,
                            api_port,
                        ),
                        crate::commands::agent_autoconnect::tab_env,
                    ),
                    crate::commands::deps::cached_login_shell_path,
                )
            };
            // SW-WA-F10: route a native launch through the SpawnIntent so the
            // host (sidecar or in-process) reserves an activity invocation
            // and binds SOCKET_ENV / TOKEN_ENV onto the child at spawn time.
            // Mirrors the routine-arm pathway (`session-server/host.rs`
            // arm_prepare), extended here to the generic spawn seam the MCP
            // managed launch uses so a saved-agent session gets the same
            // native-prep without a shell wrapper.
            intent.remote_launch = native_launch;
            let environment = if crate::commands::pty_backend::ResolvedTerminalBackend::client(
                &backend,
            )
            .is_none()
            {
                Some(managed_host_environment(&app)?)
            } else {
                None
            };

            // Resolve and consume the one-use account capability only after every
            // fallible setup step has completed and immediately before process
            // spawn. A failure while preparing the PTY therefore does not burn a
            // valid token; detach/generation/auth state is still revalidated at
            // this boundary.
            let (consumed_account, consumed_environment) = prepare_send_bindings(
                || backend.revalidate(&app),
                || {
                    crate::commands::agent_accounts::validate_environment_binding(
                        &app,
                        environment_binding.as_deref(),
                    )
                },
                || {
                    Ok((
                        crate::commands::agent_accounts::consume_resolved_launch_binding(
                            account_binding,
                        )?,
                        crate::commands::agent_accounts::consume_environment_binding(
                            &app,
                            environment_binding.as_deref(),
                        )?,
                    ))
                },
            )?;
            if !remote && consumed_environment.requires_spend_holds {
                if let Some(client) =
                    crate::commands::pty_backend::ResolvedTerminalBackend::client(&backend)
                {
                    client.require_spend_holds()?;
                }
            }
            let consumed_scope = apply_bindings(
                &mut intent,
                consumed_account.environment,
                consumed_account.env_remove,
                consumed_environment,
            );
            // A terminal server that would drop the agent's own variables refuses the launch
            // before anything starts.
            if let Some(client) =
                crate::commands::pty_backend::ResolvedTerminalBackend::client(&backend)
            {
                client.require_agent_env(!intent.agent_env.is_empty())?;
            }
            let claim = if remote {
                None
            } else {
                Some(crate::commands::pty_backend::reserve_spawn_effect_claim(
                    &app,
                    &backend,
                    &session_id,
                    is_pool_member,
                )?)
            };
            if let Some(creation) = &consumed_scope {
                let owner = claim
                    .as_ref()
                    .ok_or("local scope cannot bind to a remote runtime")?;
                if let Err(error) = crate::commands::mcp_scope::bind_scope_creation(creation, owner)
                {
                    app.state::<crate::terminal::session_server::SessionServerState>()
                        .admission
                        .effects
                        .never_started(owner);
                    return Err(error);
                }
                crate::commands::mcp_scope::bind_relay_launch(&app, &session_id);
            }
            Ok((
                intent,
                consumed_account.provenance,
                backend,
                environment,
                claim,
                consumed_scope,
                workspace_admission,
            ))
        })
        .await?
    };

    // RF-perf-17: the STABLE worktree anchor for reverse-sync liveness is the
    // spawn dir, so a later OSC-7 `cd` can't drop this worktree from the
    // live-CLI set.
    let registration = SessionRegistration {
        session_id: session_id.clone(),
        meta,
        home_cwd: intent.spawn_dir(),
        osc_enabled: intent.osc_enabled(),
        is_pool_member,
        scrollback: scrollback.unwrap_or(codesense_pty_core::emulator::DEFAULT_SCROLLBACK),
    };

    // Sidecar mode: host the session in the session server. A failure that
    // leaves it known not to run there falls open to the in-process host below
    // — unless the host is pinned, where the session lands there or nowhere.
    if crate::commands::pty_backend::ResolvedTerminalBackend::client(&backend).is_some() {
        let outcome = spawn_on_session_server(
            app,
            backend.clone(),
            intent.clone(),
            registration.clone(),
            provenance.clone(),
            effect_claim.clone(),
        )
        .await;
        if let (Err(failure), Some(claim)) = (&outcome, &effect_claim) {
            app.state::<crate::terminal::session_server::SessionServerState>()
                .admission
                .effects
                .spawn_failed(claim, failure.disposition);
        }
        match outcome {
            Ok(()) => return Ok(()),
            Err(failure) if may_fall_open_after_spawn(&backend, &failure, may_retry_in_process) => {
                warn!(target: "pty", "session server spawn failed, hosting in-process: {}", failure.message);
                let fallback =
                    reserve_in_process_effect_claim(app, &session_id, is_pool_member).await?;
                if let (Some(creation), Some(old)) = (&scope_creation, &effect_claim) {
                    let (creation, old, next, app) =
                        (creation.clone(), old.clone(), fallback.clone(), app.clone());
                    await_blocking("spawn_fallback_scope", move || {
                        transfer_fallback_scope(&app, &creation, &old, &next)
                    })
                    .await?;
                }
                effect_claim = Some(fallback);
            }
            Err(failure) => {
                if failure.may_host_elsewhere {
                    if let Some(claim) = &effect_claim {
                        crate::commands::mcp_scope::remove_owned_scope(claim);
                    }
                }
                return Err(failure.message);
            }
        }
    }

    if backend.is_remote() {
        return Err("remote terminal backend became unavailable".into());
    }
    hosting_in_process(app, &session_id);
    let effect_claim = match effect_claim {
        Some(claim) => claim,
        None => reserve_in_process_effect_claim(app, &session_id, is_pool_member).await?,
    };
    let native_provider = intent.remote_launch.and_then(NativeLaunch::cli);
    let created = async {
        let mut spec = resolve_in_process(
            match environment {
                Some(environment) => environment,
                None => managed_host_environment(app)?,
            },
            &session_id,
            intent,
        )
        .await?;
        // Every shell names its host, so a CLI typed in it reports here.
        if let Some((name, root)) = app
            .try_state::<codesense_pty_core::activity::ActivityService>()
            .and_then(|service| service.shell_env())
        {
            spec.command.env(name, root);
        }
        // SW-WA-F10: reserve the native-launch activity invocation BEFORE
        // spawn and bind SOCKET_ENV / TOKEN_ENV onto the child, so an
        // argv-direct managed launch gets its activity reporting from
        // PID-1 of the PTY — no helper indirection, no shell wait-for-prompt.
        // Mirrors the sidecar's native-prep path (session-server/host.rs
        // `arm_prepare` + `spawn_and_register`).
        let native_writer = if let Some(provider) = native_provider {
            native_prep_in_process(app, &session_id, provider).inspect(|writer| {
                spec.command
                    .env(codesense_pty_core::activity::SOCKET_ENV, &writer.socket);
                spec.command
                    .env(codesense_pty_core::activity::TOKEN_ENV, &writer.token);
            })
        } else {
            None
        };

        // The interactive-PTY spawn locus is a fail-open-to-local seam: the child is
        // born through the resolved `PtyRunner`. With no runner selected this resolves
        // to the built-in `LocalPtyRunner`. A future remote driver is one
        // `resolve_pty_runner` arm away.
        let runner = crate::commands::pty_runner::resolve_pty_runner(None)
            .ok_or_else(|| "no PTY runner resolved".to_string())?;
        // The distinct per-failure `error!(target: "pty", ...)` lines fire inside the
        // runner's spawn, so this only maps the error surface back to
        // `Result<_, String>` — no double-log.
        match runner.spawn(spec).await.map_err(String::from) {
            Ok(pty) => Ok(pty),
            Err(error) => {
                // SW-WA-F10: a spawn that never ran releases the pre-reserved
                // activity invocation so a retry mints a fresh one.
                if let (Some(writer), Some(host)) = (
                    native_writer,
                    crate::commands::pty_backend::in_process_host_handle(app),
                ) {
                    host.activity()
                        .remove(&session_id, &writer.identity.session_incarnation);
                }
                Err(error)
            }
        }
    }
    .await;
    let pty = match created {
        Ok(pty) => pty,
        Err(error) => {
            let app = app.clone();
            await_blocking("spawn_pty_rollback", move || {
                crate::commands::mcp_scope::remove_owned_scope(&effect_claim);
                app.state::<crate::terminal::session_server::SessionServerState>()
                    .admission
                    .effects
                    .never_started(&effect_claim);
                Ok(())
            })
            .await?;
            return Err(error);
        }
    };

    // Stage the consumed launch provenance, then register the session. Both run
    // on a blocking worker: registration can tear down a displaced or capped
    // session. A failure drops the staged provenance again, so a failed spawn
    // never leaves it orphaned.
    let app = app.clone();
    await_blocking("spawn_pty", move || {
        if let Err(error) = app
            .state::<crate::commands::agent_accounts::AgentAccountRuntimeState>()
            .stage_owned_provenance(&session_id, provenance, Some(effect_claim.clone()))
        {
            pty.discard();
            crate::commands::mcp_scope::remove_owned_scope(&effect_claim);
            app.state::<crate::terminal::session_server::SessionServerState>()
                .admission
                .effects
                .never_started(&effect_claim);
            return Err(error);
        }
        let registered = register_in_process(&app, pty, registration);
        if registered.is_err() {
            app.state::<crate::commands::agent_accounts::AgentAccountRuntimeState>()
                .take_owned_provenance(&effect_claim);
            crate::commands::mcp_scope::remove_owned_scope(&effect_claim);
            app.state::<crate::terminal::session_server::SessionServerState>()
                .admission
                .effects
                .retire_claim(&effect_claim);
        }
        registered
    })
    .await
}

async fn reserve_in_process_effect_claim(
    app: &tauri::AppHandle,
    session: &str,
    pool: bool,
) -> Result<super::pty_host::LocalEffectClaim, String> {
    let app = app.clone();
    let session = session.to_string();
    await_blocking("spawn_pty_claim", move || {
        let backend = super::pty_backend::managed_in_process_backend(&app)?;
        super::pty_backend::reserve_spawn_effect_claim(&app, &backend, &session, pool)
    })
    .await
}

pub(crate) fn may_fall_open_after_spawn(
    backend: &crate::commands::pty_backend::ResolvedTerminalBackend,
    failure: &crate::terminal::session_server::SpawnFailure,
    may_retry_in_process: bool,
) -> bool {
    failure.may_host_elsewhere && may_retry_in_process && backend.allows_local_fail_open()
}

/// SW-GS-23: reliably submit a single line to an agent CLI's PTY.
///
/// Writing into a raw-mode TUI *mid-render* can be batched as a paste, so the
/// trailing Enter is held as input-box content and the line sits unsent ("typed
/// but not sent"). There is no agent-agnostic "at-prompt" signal to gate on
/// (`aiStatus` is Claude-hook-driven and deliberately unwired at `org_root`), so
/// this command watches the session's OWN output byte counter (`seq`) — pure
/// byte observation, no hooks, works headless:
///
///  1. **Settle** — wait until output has been quiet for `quiet_ms` (the agent is
///     idle at its prompt, done rendering), bounded by the overall `timeout_ms`.
///  2. **Write** `line`, then — after its echo settles (at least a short gap) —
///     a carriage return as its own keystroke, so the TUI never reads the Enter
///     as part of a pasted burst. A multi-line `line` goes as a bracketed paste.
///  3. **Confirm / nudge** — the TUI *redraws its input box* on our write (a
///     bounded output echo), but a real submit *resumes* output (box clears, the
///     turn begins). So after a brief echo window, watch for further output: if it
///     grows ⇒ submitted (return); if it stays quiet ⇒ the line is held ⇒ send one
///     discrete carriage return and retry, up to `max_nudges` / `timeout_ms`. A
///     bare CR at an empty/submitted prompt is a harmless no-op, so an unnecessary
///     nudge never harms — we bias toward nudging over missing a held line.
///
/// Never blocks past `timeout_ms`; returns `Ok(SubmitOutcome { submitted })` —
/// `submitted:true` on a confirmed resume, `false` on exhausted nudges / timeout
/// (SW-GS-30). A missing session at the first write is a hard `Err` (SW-EW-39).
pub async fn submit_pty_line(
    app: tauri::AppHandle,
    session_id: String,
    line: String,
    quiet_ms: u64,
    confirm_ms: u64,
    timeout_ms: u64,
    max_nudges: u32,
) -> Result<SubmitOutcome, String> {
    admitted_commands::submit_pty_line(
        app,
        crate::commands::machines::RuntimeSessionReference::Legacy(session_id),
        line,
        quiet_ms,
        confirm_ms,
        timeout_ms,
        max_nudges,
    )
    .await
}

/// SW-SR-5 (AC-4): the ONE generic, concurrent, per-target-error-tolerant fan-out over a set
/// of session ids — the shared skeleton behind `send_to_sessions` (via [`submit_pty_line`]) and
/// `run_shell_in_sessions`. Each target's write runs through `f`, which itself already offloads
/// to the blocking pool (the submit helpers wrap `await_blocking`), so this needs no `spawn_blocking` of its own — `join_all` drives
/// them concurrently. Concurrency is load-bearing: `submit_pty_line` can block up to its
/// `timeout_ms` per target, and a sequential fan-out would serialize those timeouts. A dead
/// tab yields its `Err` in the result vec (paired with its session id) and NEVER aborts the
/// batch (AC-5) — the submit helpers already return `Err` on an unknown session
/// (SW-EW-39), so the tolerance comes for free.
pub async fn fan_out_sessions<F, Fut, T>(
    session_ids: Vec<String>,
    f: F,
) -> Vec<(String, Result<T, String>)>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let f = &f;
    futures_util::future::join_all(session_ids.into_iter().map(|sid| async move {
        let result = f(sid.clone()).await;
        (sid, result)
    }))
    .await
}

/// How typed input reaches a tab: a shell takes `line` + Enter in one write; an
/// agent TUI takes the text, then Enter as its own keystroke (confirmed/nudged).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputRoute {
    Shell,
    Agent,
}

impl InputRoute {
    /// The route for a tab: an agent when it was launched as one or an agent TUI
    /// runs in its foreground now (e.g. `claude` started by hand in a shell).
    pub(crate) fn for_tab(
        launched_as_agent: bool,
        agent_in_foreground: impl FnOnce() -> bool,
    ) -> Self {
        if launched_as_agent || agent_in_foreground() {
            Self::Agent
        } else {
            Self::Shell
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Agent => "agent",
        }
    }
}

/// `run_in_terminal_tab` on a local tab: a shell command runs through the shell
/// protocol, while a prompt for an agent TUI in the foreground goes through the
/// agent protocol so it is submitted rather than left in the input box.
pub async fn submit_tab_pty_line(
    app: tauri::AppHandle,
    session_id: String,
    line: String,
) -> Result<(InputRoute, SubmitOutcome), String> {
    use crate::commands::mcp_workspace::{
        SUBMIT_CONFIRM_MS, SUBMIT_MAX_NUDGES, SUBMIT_QUIET_MS, SUBMIT_TIMEOUT_MS,
    };
    submit_automatic_input(
        app.clone(),
        crate::commands::machines::RuntimeSessionReference::Legacy(session_id.clone()),
        await_blocking("submit_tab_pty_line", move || {
            let host = resolve_session_backend(&app, &session_id)?;
            let session = PtySessionId::new(session_id);
            let route = InputRoute::for_tab(false, || agent_in_foreground(host.as_ref(), &session));
            let outcome = match route {
                InputRoute::Agent => host.submit_line(
                    &session,
                    &line,
                    SUBMIT_QUIET_MS,
                    SUBMIT_CONFIRM_MS,
                    SUBMIT_TIMEOUT_MS,
                    SUBMIT_MAX_NUDGES,
                )?,
                InputRoute::Shell => {
                    host.submit_shell_line(&session, &line, SUBMIT_QUIET_MS, SUBMIT_TIMEOUT_MS)?
                }
            };
            Ok((route, outcome))
        }),
    )
    .await
}

/// Write one key's bytes (e.g. a lone Enter) to a local tab, nothing else.
pub async fn write_pty_key(
    app: tauri::AppHandle,
    session_id: String,
    bytes: &'static [u8],
) -> Result<(), String> {
    write_automatic_input(
        app,
        crate::commands::machines::RuntimeSessionReference::Legacy(session_id),
        bytes.to_vec(),
    )
    .await
}

/// PT-8: kill + reclaim a session by id, so the warm pool, the layout's closes,
/// the dashboard and the MCP verbs close sessions through the ONE
/// registry teardown. A deliberate close: the host releases the dashboard slot
/// and audits `Kill`, drops the scoped MCP config, and clears launch provenance.
/// A missing id is a no-op.
pub fn kill_session(app: &tauri::AppHandle, session_id: &str) {
    if let Err(error) = try_kill_session(app, session_id) {
        warn!(target: "pty", "kill {session_id}: {error}");
    }
}

/// The same teardown with an observable failure for callers that must retry.
pub(crate) fn try_kill_session(app: &tauri::AppHandle, session_id: &str) -> Result<(), String> {
    resolve_live_session_backend(app, session_id)
        .and_then(|host| Ok(host.kill(&PtySessionId::new(session_id))?))
        .map_err(|error| error.to_string())?;
    crate::agents::launch::forget_launch_env(session_id);
    Ok(())
}

/// Response from `subscribe_terminal`: the host emulator's terminal to build a
/// display from, the stream id (the reserved one, or a fresh one), and the
/// session's correlation metadata.
#[derive(Debug, Serialize)]
pub struct SubscribeResponse {
    #[serde(
        rename = "privateAuthorityEpoch",
        skip_serializing_if = "Option::is_none"
    )]
    private_authority_epoch: Option<String>,
    /// Written into a display of `cols × rows`, `vt` reproduces the terminal as
    /// of `seq` / `resize_gen`; the stream carries what follows.
    snapshot: TerminalSnapshot,
    /// Live output and resizes arrive on `pty-output-stream-{stream_id}`.
    stream_id: u64,
    story_id: Option<String>,
    workspace_id: Option<String>,
    epic_id: Option<String>,
}

/// Reserve a viewport stream id ahead of `subscribe_terminal`, so the frontend
/// listens on `pty-output-stream-{id}` before it subscribes and misses nothing
/// in between. Allocated app-side, with no host round trip; released by
/// `unsubscribe_terminal` if never bound.
#[tauri::command]
pub fn reserve_terminal_stream(app: tauri::AppHandle) -> Result<u64, String> {
    app.try_state::<crate::commands::pty_host::StreamIds>()
        .map(|streams| streams.reserve())
        .ok_or_else(|| "PTY streams unavailable".to_string())
}

/// Detach a viewport. The PTY and its terminal keep running; only this
/// subscriber's live-stream registration is removed, in the host its stream
/// lives in. Idempotent — a no-op if the stream is already gone (e.g. the PTY
/// was killed first).
#[tauri::command]
pub async fn unsubscribe_terminal(app: tauri::AppHandle, stream_id: u64) -> Result<(), String> {
    // A sidecar-hosted stream is detached over its socket: off the UI thread.
    await_blocking("unsubscribe_terminal", move || {
        crate::commands::pty_backend::unsubscribe(&app, stream_id)
    })
    .await
}

/// This instance's terminal server as last found incompatible — new terminals
/// are refused until it is restarted — or `None`.
#[tauri::command]
pub async fn terminal_server_status(
    app: tauri::AppHandle,
) -> Result<Option<crate::commands::pty_backend::IncompatibleTerminalServer>, String> {
    // Off the main thread: a restart in progress holds the connection state.
    await_blocking("terminal_server_status", move || {
        Ok(crate::commands::pty_backend::incompatible_terminal_server(
            &app,
        ))
    })
    .await
}

/// Read-only capability upgrade review. Offline/unknown is an error, never
/// permission to stop a process. Null means no upgrade is required.
#[tauri::command]
pub async fn terminal_activity_upgrade_status(
    app: tauri::AppHandle,
) -> Result<Option<crate::terminal::session_server::LocalActivityUpgrade>, String> {
    await_blocking("terminal_activity_upgrade_status", move || {
        let state = app.state::<crate::terminal::session_server::SessionServerState>();
        state.activity_upgrade_status()
    })
    .await
}

#[tauri::command]
pub async fn update_terminal_activity_server(
    app: tauri::AppHandle,
    expected: crate::terminal::session_server::LocalActivityUpgrade,
) -> Result<(), crate::commands::pty_backend::RestartError> {
    await_blocking("update_terminal_activity_server", move || {
        Ok(crate::commands::pty_backend::update_terminal_activity_server(&app, &expected))
    })
    .await
    .unwrap_or_else(|message| {
        Err(crate::commands::pty_backend::RestartError {
            old_stopped: false,
            message,
        })
    })
}

/// End this instance's running terminal server — ending its sessions — and
/// launch this build's in its place; how many sessions it ended, when the old
/// server said. Off the UI thread: the old server gets a grace period to exit
/// and the new one a startup wait.
#[tauri::command]
pub async fn restart_terminal_server(
    app: tauri::AppHandle,
) -> Result<Option<u32>, crate::commands::pty_backend::RestartError> {
    await_blocking("restart_terminal_server", move || {
        Ok(crate::commands::pty_backend::restart_terminal_server(&app))
    })
    .await
    .unwrap_or_else(|message| {
        Err(crate::commands::pty_backend::RestartError {
            old_stopped: false,
            message,
        })
    })
}

/// SV-7: This Mac's terminal server for this instance — whether it runs, what
/// it holds, which apps are connected, the sessions no app holds — read
/// without ever launching one.
#[tauri::command]
pub async fn terminal_server_overview(
    app: tauri::AppHandle,
) -> Result<crate::commands::pty_backend::TerminalServerOverview, String> {
    await_blocking("terminal_server_overview", move || {
        Ok(crate::commands::pty_backend::terminal_server_overview(&app))
    })
    .await
}

/// SV-7: stop This Mac's terminal server after the person confirmed ending
/// exactly `expected`'s sessions (as `terminal_server_overview` read them).
#[tauri::command]
pub async fn stop_terminal_server(
    app: tauri::AppHandle,
    expected: crate::terminal::session_server::LocalActivityUpgrade,
) -> Result<
    crate::commands::pty_backend::StoppedTerminalServer,
    crate::commands::pty_backend::RestartError,
> {
    await_blocking("stop_terminal_server", move || {
        Ok(crate::commands::pty_backend::stop_terminal_server(
            &app, &expected,
        ))
    })
    .await
    .unwrap_or_else(|message| {
        Err(crate::commands::pty_backend::RestartError::still_running(
            message,
        ))
    })
}

/// SV-7: start This Mac's terminal server when none runs.
#[tauri::command]
pub async fn start_terminal_server(
    app: tauri::AppHandle,
) -> Result<crate::commands::pty_backend::TerminalServerOverview, String> {
    await_blocking("start_terminal_server", move || {
        crate::commands::pty_backend::start_terminal_server(&app)
    })
    .await
}

/// SV-7: restart This Mac's running terminal server — any build — after the
/// person confirmed ending exactly `expected`'s sessions: it stops and this
/// build's starts in its place. How many running sessions it ended.
#[tauri::command]
pub async fn replace_terminal_server(
    app: tauri::AppHandle,
    expected: crate::terminal::session_server::LocalActivityUpgrade,
) -> Result<u32, crate::commands::pty_backend::RestartError> {
    await_blocking("replace_terminal_server", move || {
        Ok(
            crate::commands::pty_backend::replace_local_terminal_server(&app, &expected)
                .map(|()| expected.session_ids.len() as u32),
        )
    })
    .await
    .unwrap_or_else(|message| {
        Err(crate::commands::pty_backend::RestartError::still_running(
            message,
        ))
    })
}

/// SV-7: end a session no connected app holds.
#[tauri::command]
pub async fn end_unowned_terminal_session(
    app: tauri::AppHandle,
    session: String,
) -> Result<(), String> {
    await_blocking("end_unowned_terminal_session", move || {
        crate::commands::pty_backend::end_unowned_terminal_session(&app, &session)
    })
    .await
}

// SW-WV-A3: `pub(crate)` so the weft `wv_worker_deprovision` Tauri boundary can wire
// it as the injected `is_idle` liveness probe (the core method is probe-agnostic).
pub(crate) fn session_is_idle_inner(
    app: &tauri::AppHandle,
    pty_id: String,
) -> Result<bool, String> {
    Ok(resolve_session_backend(app, &pty_id)?.is_idle(&PtySessionId::new(pty_id))?)
}

/// SW-GS-2a (AC-12): true agent-process liveness for the git-sync owned tab —
/// the signal SW-GS-1's connection chip reflects. Unlike [`admitted_commands::session_is_idle`]
/// (process-TREE idle, which reads idle for BOTH "claude live at its prompt" AND
/// "dropped to a bare shell"), this asks specifically whether the named agent
/// process is alive in the session's subtree: `claude` present → connected;
/// no `claude` child → disconnected. `agent_name` is a parameter (the resolved
/// agent basename) so story/epic agent sessions reuse it verbatim. A
/// missing/exited session reports not-live (disconnected).
///
/// SW-WA-F10: for an argv-direct managed launch the agent is PID-1 of the PTY,
/// so the "dropped to a bare shell" state is unreachable — the two signals
/// collapse to "session alive". The mixed-mode case (user typed `claude` by
/// hand into a plain shell tab) still produces the shell-under-agent arrangement
/// this signal distinguishes.
pub async fn session_has_live_agent(
    app: tauri::AppHandle,
    pty_id: String,
    agent_name: String,
) -> Result<bool, String> {
    // Host process census and socket reads run outside the UI thread.
    await_blocking("session_has_live_agent", move || {
        session_has_live_agent_inner(&app, pty_id, agent_name)
    })
    .await
}

fn session_has_live_agent_inner(
    app: &tauri::AppHandle,
    pty_id: String,
    agent_name: String,
) -> Result<bool, String> {
    crate::commands::pty_backend::with_session_reference(
        app,
        &super::machines::RuntimeSessionReference::Legacy(pty_id),
        false,
        |host, session| named_agent_alive(host, session, &agent_name),
    )
}

/// Single and batched reads share the owning host's process namespace.
pub(crate) fn named_agent_alive(
    host: &dyn codesense_pty_core::domain::TerminalDomain,
    session: &PtySessionId,
    name: &str,
) -> Result<bool, String> {
    host.agents_alive(&[(session.clone(), name.to_owned())])?
        .first()
        .copied()
        .ok_or_else(|| "owner process telemetry unavailable".into())
}

/// Whether each agent CLI, by its process name, runs in `session`: what a host is asked.
fn agent_queries(session: &PtySessionId) -> Vec<(PtySessionId, String)> {
    codesense_pty_core::agent_cli::AgentCli::ALL
        .into_iter()
        .map(|cli| {
            let binary = &codesense_pty_core::agent_cli::descriptor(cli).binary;
            (session.clone(), binary.clone())
        })
        .collect()
}

/// Whether an agent TUI (Claude or Codex) is the session's foreground job, as its
/// owning host sees it — also for an agent a person started by hand in a shell.
/// A host that cannot tell (an older terminal server) answers `false`.
pub(crate) fn agent_in_foreground(
    host: &dyn codesense_pty_core::domain::TerminalDomain,
    session: &PtySessionId,
) -> bool {
    let queries = agent_queries(session);
    host.agents_alive(&queries)
        .is_ok_and(|alive| alive.iter().any(|running| *running))
}

/// SW-AGR-2: whether the session still runs its agent, as a wake must judge
/// it. `Some(true)` when an agent CLI runs in it, `Some(false)` only when a
/// probe that worked found none and (`local_jobs`, this Mac's session) the
/// shell runs no job of its own (a stopped `claude`, Ctrl-Z, is a job: the
/// agent is still there, as the frontend's `isIdleShell` reads it). `None`
/// when it cannot be told: the probe failed or timed out, the session has no
/// process id, the host predates the process probe, or (`local_jobs` off) no
/// agent was found but the shell's jobs cannot be read from here.
pub(crate) fn agent_present(
    host: &dyn codesense_pty_core::domain::TerminalDomain,
    session: &PtySessionId,
    local_jobs: bool,
) -> Option<bool> {
    let pid = host
        .live_child_pids(std::slice::from_ref(session))
        .ok()?
        .first()
        .copied()
        .flatten()?;
    let queries = agent_queries(session);
    let alive = host.agents_alive(&queries).ok()?;
    if alive.iter().any(|running| *running) {
        return Some(true);
    }
    if !local_jobs {
        return None;
    }
    let jobs = codesense_pty_core::process::running_jobs_known(&[Some(pid)])
        .pop()
        .flatten()?;
    Some(jobs.foreground.is_some() || !jobs.background.is_empty())
}

/// SW-FX-52 (AC-3): one session's liveness query for the batched probe. The FE
/// sends `{ptyId, agentName}` per registered monitor; Tauri's camelCase-aware
/// deserializer maps them onto these snake_case fields.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLivenessQuery {
    pub pty_id: String,
    #[serde(default)]
    pub target: Option<super::machines::RuntimeSessionReference>,
    pub agent_name: String,
    /// Also return the session's output tail (for idle-output attention).
    #[serde(default)]
    pub tail: bool,
}

/// How many trailing screen lines a tail query returns — enough for the last
/// line.
const OUTPUT_TAIL_LINES: u32 = 5;

/// A session's output sequence and trailing output, for a query that asked for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionOutputTail {
    pub seq: u64,
    pub tail: String,
}

/// SW-FX-52 (AC-3): one session's liveness result, echoing `pty_id` so the FE
/// can re-key results back onto its registry regardless of ordering.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLivenessResult {
    pub pty_id: String,
    /// None means unavailable, never evidence that an agent died.
    pub alive: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<SessionOutputTail>,
}

/// SW-FX-52 (AC-3): batched agent liveness — the same signal as
/// [`session_has_live_agent`], but for MANY sessions in ONE call sharing ONE
/// process-table snapshot. The single consolidated FE poller
/// (`useAgentLivenessMonitor`) calls this once per tick with every registered
/// session, collapsing N per-session pollers (N full `sysinfo` scans/tick) into
/// one enumeration. A missing/exited session reports not-live (disconnected),
/// mirroring the single-call contract.
#[tauri::command]
pub async fn sessions_have_live_agent(
    app: tauri::AppHandle,
    queries: Vec<SessionLivenessQuery>,
) -> Result<Vec<SessionLivenessResult>, String> {
    // SW-FX-52 (AC-3): mirror `session_has_live_agent` — offload the whole body
    // (one `sysinfo` scan reading `KERN_PROCARGS2`, slow on macOS) to a blocking
    // worker so the batched poll never touches the UI thread.
    await_blocking("sessions_have_live_agent", move || {
        sessions_have_live_agent_inner(&app, queries)
    })
    .await
}

fn sessions_have_live_agent_inner(
    app: &tauri::AppHandle,
    queries: Vec<SessionLivenessQuery>,
) -> Result<Vec<SessionLivenessResult>, String> {
    use super::machines::RuntimeSessionReference;
    use crate::commands::pty_backend::{resolve_session_reference, ResolvedTerminalBackend};
    if queries.len() > 1024 {
        return Err("agent probe exceeds bounds".into());
    }
    let mut groups: Vec<(ResolvedTerminalBackend, Vec<usize>)> = Vec::new();
    for (i, query) in queries.iter().enumerate() {
        let reference = query
            .target
            .clone()
            .unwrap_or_else(|| RuntimeSessionReference::Legacy(query.pty_id.clone()));
        if reference.session() != query.pty_id || query.agent_name.len() > 256 {
            continue;
        }
        let Ok(backend) = resolve_session_reference(app, &reference, true) else {
            continue;
        };
        if let Some((_, indices)) = groups
            .iter_mut()
            .find(|(host, _)| host.admission_target() == backend.admission_target())
        {
            indices.push(i);
        } else {
            groups.push((backend, vec![i]));
        }
    }
    let mut alive = vec![None; queries.len()];
    let mut output = vec![None; queries.len()];
    for (backend, indices) in groups {
        if backend.revalidate(app).is_err() {
            continue;
        }
        let domain = backend.domain();
        let probes: Vec<_> = indices
            .iter()
            .map(|&i| {
                (
                    PtySessionId::new(&queries[i].pty_id),
                    queries[i].agent_name.clone(),
                )
            })
            .collect();
        let Ok(results) = domain.agents_alive(&probes) else {
            continue;
        };
        if results.len() != indices.len() {
            continue;
        }
        let tails: Vec<_> = indices
            .iter()
            .map(|&i| {
                // Legacy local idle-output fallback stays local. Remote activity
                // transport does not collect terminal/prompt content.
                (queries[i].tail && !backend.is_remote())
                    .then(|| {
                        codesense_pty_core::domain::visible_text(
                            &*domain,
                            &PtySessionId::new(&queries[i].pty_id),
                            OUTPUT_TAIL_LINES,
                        )
                        .ok()
                    })
                    .flatten()
            })
            .collect();
        if backend.revalidate(app).is_err() {
            continue;
        }
        for ((i, result), tail) in indices.into_iter().zip(results).zip(tails) {
            alive[i] = Some(result);
            output[i] = tail.map(|text| (text.seq, text.text));
        }
    }
    let tails = queries
        .iter()
        .enumerate()
        .filter(|(_, q)| q.tail)
        .map(|(i, _)| output[i].take())
        .collect();
    if let Some(hub) = app.try_state::<crate::live_hub::LiveHub>() {
        let probes: Vec<_> = queries
            .iter()
            .zip(&alive)
            .filter(|(query, _)| {
                codesense_pty_core::agent_cli::known_label(&query.agent_name).is_some()
            })
            .map(|(query, alive)| (query.pty_id.clone(), query.agent_name.clone(), *alive))
            .collect();
        let missing = hub.note_process_probe(&probes);
        if let Some(launch) =
            app.try_state::<crate::commands::dashboard_launch::DashboardLaunchState>()
        {
            // Reuse the admitted probe's session-level decision: another CLI's
            // negative result cannot end a CLI that is still present.
            for session in missing {
                if launch
                    .turns()
                    .record_exit(&session, None, false, false, false)
                {
                    hub.invalidate_sessions();
                }
            }
        }
    }
    Ok(assemble_liveness_results(queries, alive, tails))
}

/// Pair each query with its liveness and — only for a `tail` query — the next tail from
/// `tails` (which holds one entry per tail query, in query order).
fn assemble_liveness_results(
    queries: Vec<SessionLivenessQuery>,
    alive: Vec<Option<bool>>,
    tails: Vec<Option<(u64, String)>>,
) -> Vec<SessionLivenessResult> {
    let mut tails = tails.into_iter();
    queries
        .into_iter()
        .zip(alive)
        .map(|(q, alive)| SessionLivenessResult {
            alive,
            output: if q.tail {
                tails
                    .next()
                    .flatten()
                    .map(|(seq, tail)| SessionOutputTail { seq, tail })
            } else {
                None
            },
            pty_id: q.pty_id,
        })
        .collect()
}

/// The inputs of one new shell terminal as the frontend sends them: a layout
/// tab's warm spawn source.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachWarmArgs {
    pub target_cwd: String,
    pub size: SpawnPtySize,
    pub story_id: Option<String>,
    pub workspace_id: Option<String>,
    pub epic_id: Option<String>,
    /// The display's scrollback lines, for a cold spawn. A warm-pool member
    /// keeps the scrollback it booted with.
    pub scrollback: Option<u32>,
    pub starship_preset: Option<String>,
    pub shell_integration: Option<bool>,
}

/// Open `session_id` as a shell at `args.target_cwd` in `host`, from a
/// warm-pool member when one is ready there (`Ok(true)`) or by a cold spawn
/// (`Ok(false)`).
pub(crate) async fn attach_warm_from_args(
    app: &tauri::AppHandle,
    session_id: String,
    args: AttachWarmArgs,
    host: SpawnPlacement,
) -> Result<bool, String> {
    let AttachWarmArgs {
        target_cwd,
        size,
        story_id,
        workspace_id,
        epic_id,
        scrollback,
        starship_preset,
        shell_integration,
    } = args;
    // RF-15: group the correlation triplet once.
    let meta = SessionMeta {
        story_id,
        workspace_id,
        epic_id,
    };

    let warm_app = app.clone();
    let warm_session = session_id.clone();
    let warm_cwd = target_cwd.clone();
    let warm_meta = meta.clone();
    let (cols, rows) = (size.cols, size.rows);
    let warm_host = host.clone();
    let cold_host = host.clone();
    attach_warm_from_args_core(
        &host,
        move || async move {
            // SW-WL-6-A (H3): the warm handoff stats the target and touches the
            // PTY writer — run it on a blocking worker so opening a terminal
            // never blocks the UI thread.
            let app = warm_app;
            let session_id = warm_session;
            let target_cwd = warm_cwd;
            let meta = warm_meta;
            let spawn_host = warm_host;
            await_blocking("attach_warm", move || {
            // Only serve from the pool when the target is warm-safe: a warm member
            // must `cd` into it under the gate, so an unsafe path would leak the
            // echo or stall first paint (see `is_warm_safe_cwd`). Check before
            // `acquire_ready` so an unsafe cwd never pops (and wastes) a Ready
            // member. (SW-EW-22 review)
            if !is_warm_safe_cwd(&target_cwd) {
                return Ok(false);
            }
            let pool = app.state::<WarmPoolState>();
            let Some(member_id) = pool.acquire_ready() else {
                return Ok(false);
            };
            let exact = match &spawn_host {
                SpawnPlacement::Exact(backend) => backend,
                SpawnPlacement::ResolveLocal => {
                    kill_session(&app, &member_id);
                    pool.refill();
                    return Ok(false);
                }
            };
            let Some(member_host) =
                resolve_handoff_backend(&app, &member_id, &session_id, exact)
            else {
                // The member and the target id live in different hosts: the
                // member is retired and the terminal opens cold.
                kill_session(&app, &member_id);
                pool.refill();
                return Ok(false);
            };
            let (source, target) = super::pty_backend::prepare_popped_warm(
                &app, &member_host, &member_id,
                || super::pty_backend::reserve_spawn_effect_claim(&app, exact, &session_id, false),
                || pool.refill(),
            )?;
            let state = app.state::<crate::terminal::session_server::SessionServerState>();
            let effects = &state.admission.effects;
            let outcome = member_host.handoff_warm_member(
                &PtySessionId::new(member_id.as_str()),
                &PtySessionId::new(session_id.as_str()),
                &target_cwd,
                cols,
                rows,
                meta,
            );
            effects.finish_warm_handoff(&source, &target, &outcome);
            match outcome {
                Ok(()) => {
                    pool.record_warm_hit();
                    pool.refill(); // restore target asynchronously (AC-5)
                    Ok(true)
                }
                Err(e) => {
                    // A lost reply can hide a completed re-key. Neither the
                    // target nor source has a proven never-started outcome.
                    warn!(target: "warm_pool", "warm handoff failed for {member_id}: {e}; retaining drain guard");
                    pool.refill();
                    Err(format!("warm handoff outcome requires drain before retry: {e}"))
                }
            }
            })
            .await
        },
        move || async move {
            // Cold/direct path: a fresh shell at the target cwd (no cd/gate).
            app.state::<WarmPoolState>().record_direct();
            spawn_pty_inner(
                app,
                session_id,
                String::new(),
                // SW-WA-F10: cold-fallback shell has no argv-direct spawn
                // and no native-launch binding.
                None,
                target_cwd,
                size,
                Some(true),
                starship_preset,
                meta,
                scrollback,
                shell_integration,
                None,
                None,
                None,
                None,
                false,
                cold_host,
            )
            .await
        },
    )
    .await
}

/// App-independent production core. Exact remote placement makes the local
/// managed-operation closure uncallable and invokes the exact cold send once.
pub(crate) async fn attach_warm_from_args_core<Local, LocalFuture, Cold, ColdFuture>(
    host: &SpawnPlacement,
    local_warm: Local,
    cold_send: Cold,
) -> Result<bool, String>
where
    Local: FnOnce() -> LocalFuture,
    LocalFuture: std::future::Future<Output = Result<bool, String>>,
    Cold: FnOnce() -> ColdFuture,
    ColdFuture: std::future::Future<Output = Result<(), String>>,
{
    let served_warm = if host.remote_backend().is_some() {
        false
    } else {
        local_warm().await?
    };
    if served_warm {
        return Ok(true);
    }
    cold_send().await?;
    Ok(false)
}

/// PT-8: set the warm-pool target size + spawn params (drains stale members and
/// refills with the new params). Called by the frontend at launch once settings
/// hydrate (AC-1) and on any change to pool size / preset / shell integration
/// (AC-15). Size is clamped to 0–5 (0 disables — AC-7).
#[tauri::command]
pub fn reconfigure_warm_pool(
    app: tauri::AppHandle,
    size: u32,
    starship_preset: Option<String>,
    shell_integration: Option<bool>,
) -> Result<(), String> {
    let size = (size as usize).min(5);
    app.state::<WarmPoolState>()
        .reconfigure(size, starship_preset, shell_integration);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The webview's `spawn_pty` names the native launch of every registered CLI, and the
    /// command decodes each into its spawn: renderer and dashboard direct exec go through it.
    #[test]
    fn spawn_pty_decodes_the_native_launch_of_every_registered_cli() {
        use codesense_pty_core::agent_cli::AgentCli;
        for cli in AgentCli::every_cli() {
            let args: SpawnPtyArgs = serde_json::from_value(serde_json::json!({
                "command": "", "cwd": "/work", "size": {"cols": 80, "rows": 24},
                "nativeLaunch": cli.as_str(),
            }))
            .unwrap_or_else(|error| panic!("{cli}: {error}"));
            assert_eq!(args.native_launch, Some(NativeLaunch::Cli(cli)));
        }
    }

    #[test]
    fn extending_a_deadline_emits_a_session_change_only_when_it_moves() {
        use super::super::dashboard_launch::{
            DashboardLaunchState, LaunchRecord, SESSION_STATE_CHANGED,
        };
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use tauri::Listener;
        let app = tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        app.manage(DashboardLaunchState::new(2));
        let state = app.state::<DashboardLaunchState>();
        state
            .begin_with_provenance(
                "worker",
                None,
                LaunchRecord {
                    max_runtime_ms: Some(60_000),
                    ..Default::default()
                },
            )
            .unwrap();
        state.bind("worker", "worker-session", None).unwrap();
        let deadline = state.deadline_snapshot()[0].deadline_ms;
        let changes = Arc::new(AtomicUsize::new(0));
        let seen = changes.clone();
        app.listen(SESSION_STATE_CHANGED, move |_| {
            seen.fetch_add(1, Ordering::Relaxed);
        });
        record_human_input(app.handle(), "worker-session", deadline);
        assert!(state.deadline_snapshot()[0].deadline_ms > deadline);
        assert_eq!(changes.load(Ordering::Relaxed), 1);
        record_human_input(app.handle(), "worker-session", deadline);
        record_human_input(app.handle(), "unknown", deadline);
        assert_eq!(changes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn liveness_results_pair_tails_with_their_own_tail_queries() {
        let q = |id: &str, tail: bool| SessionLivenessQuery {
            pty_id: id.into(),
            agent_name: "aider".into(),
            target: None,
            tail,
        };
        let results = assemble_liveness_results(
            vec![
                q("a", false),
                q("b", true),
                q("c", false),
                q("gone", true),
                q("d", true),
            ],
            vec![Some(true), Some(true), Some(false), Some(false), Some(true)],
            vec![Some((5, "b>".into())), None, Some((9, "d?".into()))],
        );
        let json = serde_json::to_value(&results).unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                { "ptyId": "a", "alive": true },
                { "ptyId": "b", "alive": true, "output": { "seq": 5, "tail": "b>" } },
                { "ptyId": "c", "alive": false },
                { "ptyId": "gone", "alive": false },
                { "ptyId": "d", "alive": true, "output": { "seq": 9, "tail": "d?" } },
            ])
        );
    }

    /// One runtime: the PTY runtime lives in `codesense-pty-core`. None of the
    /// moved runtime symbols may be defined here (outside this test module), the
    /// old session-store / domain / runner-vocabulary homes are gone.
    #[test]
    fn the_pty_runtime_is_defined_only_in_pty_core() {
        let src = include_str!("pty.rs");
        let non_test = crate::test_support::source_guard::production_text(src);
        let non_test = non_test.as_str();
        for moved in [
            "struct TerminalSession",
            "struct SpawnedChildGuard",
            "struct PtyState",
            "impl TerminalRegistry",
            "fn run_pty_batcher",
            "fn emit_valid_utf8",
            "PTY_BATCH_WINDOW",
            "PTY_IDLE_TIMEOUT",
            "PTY_MAX_BATCH_BYTES",
            "fn insert_session_reaping_displaced",
            "fn spawn_session_background_thread",
            "fn cleanup_failed_background_start_in",
            "fn teardown_session",
            "fn reap_session_in_background",
            "fn reap_child_in_background",
            "PTY_DEAD_SESSION_CAP",
            "fn dead_sessions_to_evict",
            "fn enforce_dead_session_cap",
            "fn prune_subscriber_index",
            "fn gated_slice",
            "fn fan_out_to_subscribers",
            "fn child_is_alive",
            "fn terminate_child",
            "fn session_is_persisting",
            "fn is_persisting",
            "fn collect_persisting_ids",
            "fn kill_non_persisting",
            "fn session_cwd(",
            "fn unsubscribe_terminal_in",
            "fn dispatch_osc133",
            "fn handoff_warm_member",
            "fn spawn_suppress_timeout_guard",
            "fn warm_handoff_command",
            "fn is_warm_safe_cwd",
            "fn shell_quote",
            "WARM_SUPPRESS_TIMEOUT",
            "struct AppWarmPoolHost",
            "struct SessionMeta",
            "fn make_isolated_session",
            "fn leak_gate_iteration",
            "fn replay_snapshot",
            "fn alive_session_ids",
            "fn build_spawn_command",
            "fn resolve_spawn_resources",
            "struct ResolvedSpawnResources",
            "fn resolve_source_zdotdir",
            "fn inherited_env_to_warn",
            "fn resolve_spawn_cwd",
        ] {
            assert!(
                !non_test.contains(moved),
                "`{moved}` must live in codesense-pty-core, not commands/pty.rs"
            );
        }

        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        for gone in [
            "src/terminal/session_store.rs",
            "src/terminal/osc_parser.rs",
            "src/terminal/domain",
        ] {
            assert!(!manifest.join(gone).exists(), "{gone} must be deleted");
        }
        // The runner vocabulary lives in pty-core; the deleted runner surfaces
        // (built from parts so a repo-wide grep for them stays empty) are gone.
        let runner = include_str!("pty_runner.rs");
        let deleted = [
            ["block_on", "_spawn"].concat(),
            ["PtyRunner", "Descriptor"].concat(),
            ["into", "_parts"].concat(),
        ];
        for moved in [
            "struct PtySpec",
            "struct PtySession",
            "struct LocalPtyRunner",
            "enum PtyRunnerError",
        ]
        .into_iter()
        .chain(deleted.iter().map(String::as_str))
        {
            assert!(
                !runner.contains(moved),
                "`{moved}` must not be defined in commands/pty_runner.rs"
            );
        }
        for gone in &deleted {
            assert!(
                !non_test.contains(gone.as_str()),
                "`{gone}` must be deleted"
            );
        }
    }

    // ── SW-SR-5 (AC-4/AC-5): the shared fan-out skeleton ────────────────────

    #[tokio::test]
    async fn fan_out_sessions_runs_every_target_and_tolerates_per_target_errors() {
        // AC-5: a failing target yields its Err paired with its id and NEVER aborts the batch;
        // every target produces exactly one result, in input order (join_all).
        let ids = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let results = fan_out_sessions(ids, |sid| async move {
            if sid == "b" {
                Err(format!("dead: {sid}"))
            } else {
                Ok::<String, String>(format!("ok: {sid}"))
            }
        })
        .await;
        assert_eq!(results.len(), 3, "one result per target, none dropped");
        assert_eq!(results[0], ("a".to_string(), Ok("ok: a".to_string())));
        assert_eq!(
            results[1],
            ("b".to_string(), Err("dead: b".to_string())),
            "the dead tab's error is captured, not propagated"
        );
        assert_eq!(results[2], ("c".to_string(), Ok("ok: c".to_string())));
    }

    #[tokio::test]
    async fn fan_out_sessions_on_empty_targets_is_a_noop() {
        let results = fan_out_sessions(
            Vec::new(),
            |_sid: String| async move { Ok::<(), String>(()) },
        )
        .await;
        assert!(results.is_empty());
    }
}

/// SW-LN-35: the app's half of running the user's own shell — the prompt
/// preset it asks for, and the integration scripts it ships. The shell itself is
/// resolved and exercised host-side (`codesense_pty_core::spawn`).
#[cfg(test)]
mod shell_tests {
    use super::*;
    use codesense_pty_core::shell::ShellKind;
    use std::path::Path;

    #[test]
    fn only_an_integrated_local_shell_tab_exports_the_agent_connection() {
        let local = |interactive: Option<bool>, integration: Option<bool>| {
            build_local_spawn_intent(
                "claude".into(),
                None,
                "/work".into(),
                80,
                24,
                interactive,
                integration,
                None,
                None,
                "plain".into(),
                None,
            )
        };
        let tab = || vec![("CODESENSE_MCP_CONFIG".to_string(), "/cfg".to_string())];
        assert_eq!(
            with_tab_agents(local(Some(true), None), tab).env,
            tab(),
            "an interactive tab with shell integration"
        );
        for (interactive, integration) in
            [(None, None), (Some(false), None), (Some(true), Some(false))]
        {
            assert!(
                with_tab_agents(local(interactive, integration), || panic!("not consulted"))
                    .env
                    .is_empty(),
                "{interactive:?} {integration:?}"
            );
        }
    }

    /// A saved agent's env goes from its definition, through the launch it resolves to and
    /// the one-use binding its interactive spawn consumes, onto the PTY child, under every
    /// variable CodeSense sets: the session identity, the API port, the account and the
    /// MCP scope's variables, and the runtime's guarded-variable strip.
    #[test]
    fn a_saved_agents_env_reaches_its_pty_spawn_under_codesense_variables() {
        use crate::agents::resolve::tests::{alice, draft, save, team_content};
        use crate::commands::agent_accounts::{
            consume_environment_binding, mint_launch_environment, AgentAccountRuntimeState,
            LaunchEnvironment,
        };
        use codesense_pty_core::agent_cli::AgentCli;
        use codesense_pty_core::spawn::HostEnvironment;

        let root = tempfile::tempdir().unwrap();
        team_content(root.path(), &alice());
        let mut definition = draft("isolated", AgentCli::Claude);
        definition.env = [
            ("CLAUDE_CODE_DISABLE_AUTO_MEMORY", "1"),
            ("CODESENSE_TAB_SESSION_ID", "spoofed"),
            ("CODESENSE_API_PORT", "1"),
            ("ACCOUNT_VAR", "agent"),
            ("CODESENSE_MCP_CONFIG", "/agent.json"),
            ("ANTHROPIC_API_KEY", "agent-key"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let key = save(root.path(), &alice(), definition);
        let launch =
            crate::agents::launch::prepare(root.path(), &alice(), &key, Default::default())
                .unwrap();

        let app = tauri::test::mock_builder()
            .manage(AgentAccountRuntimeState::default())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app");
        let scope_env = vec![(
            "CODESENSE_MCP_CONFIG".to_string(),
            "/scoped.json".to_string(),
        )];
        let token = mint_launch_environment(
            app.handle(),
            LaunchEnvironment::for_agent(&launch.launch, scope_env, None, false),
        );
        let consumed = consume_environment_binding(app.handle(), token.as_deref()).unwrap();
        let mut intent = build_local_spawn_intent(
            String::new(),
            Some(vec!["/opt/claude".to_string()]),
            String::new(),
            120,
            40,
            Some(false),
            None,
            Some(launch.launch.env_suppress.clone()),
            None,
            "plain".to_string(),
            Some(8737),
        );
        let scope = apply_bindings(
            &mut intent,
            vec![("ACCOUNT_VAR".to_string(), "account".to_string())],
            vec!["CLAUDE_CODE_OAUTH_TOKEN".to_string()],
            consumed,
        );
        assert!(scope.is_none());
        assert!(intent
            .env_remove
            .contains(&"CLAUDE_CODE_OAUTH_TOKEN".to_string()));
        assert!(intent.env_remove.contains(&"ANTHROPIC_API_KEY".to_string()));
        let host = HostEnvironment::with_vars(None, None, [("PATH".into(), "/usr/bin".into())]);
        let spec = intent.resolve("tab-isolated", &host);
        let env = |name: &str| {
            spec.command
                .get_env(name)
                .map(|value| value.to_string_lossy().into_owned())
        };
        assert_eq!(env("CLAUDE_CODE_DISABLE_AUTO_MEMORY").as_deref(), Some("1"));
        assert_eq!(
            env("CODESENSE_TAB_SESSION_ID").as_deref(),
            Some("tab-isolated")
        );
        assert_eq!(env("CODESENSE_API_PORT").as_deref(), Some("8737"));
        assert_eq!(env("ACCOUNT_VAR").as_deref(), Some("account"));
        assert_eq!(env("CODESENSE_MCP_CONFIG").as_deref(), Some("/scoped.json"));
        assert_eq!(env("ANTHROPIC_API_KEY"), None);
    }

    /// A managed account's scrub also drops the credential a saved agent carries in its own env;
    /// a system-default launch keeps it.
    #[test]
    fn a_managed_account_scrub_drops_a_saved_agents_token_and_a_default_keeps_it() {
        use codesense_pty_core::spawn::HostEnvironment;
        let resolve = |scrub: Vec<String>| {
            let mut intent = build_local_spawn_intent(
                String::new(),
                Some(vec!["/opt/claude".to_string()]),
                String::new(),
                120,
                40,
                Some(false),
                None,
                Some(vec!["ANTHROPIC_API_KEY".to_string()]),
                None,
                "plain".to_string(),
                Some(8737),
            );
            apply_bindings(
                &mut intent,
                Vec::new(),
                scrub,
                crate::commands::agent_accounts::LaunchEnvironment {
                    agent_env: vec![(
                        "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                        "agent-token".to_string(),
                    )],
                    ..Default::default()
                },
            );
            let host = HostEnvironment::with_vars(None, None, [("PATH".into(), "/usr/bin".into())]);
            intent
                .resolve("tab-scrub", &host)
                .command
                .get_env("CLAUDE_CODE_OAUTH_TOKEN")
                .map(|value| value.to_string_lossy().into_owned())
        };
        assert_eq!(resolve(vec!["CLAUDE_CODE_OAUTH_TOKEN".to_string()]), None);
        assert_eq!(resolve(Vec::new()).as_deref(), Some("agent-token"));
    }

    #[test]
    fn remote_intent_is_host_resolved_without_consulting_desktop_inputs() {
        let intent = build_remote_spawn_intent(
            String::new(),
            None,
            "/remote/worktree".into(),
            120,
            40,
            Some(true),
            Some(true),
            Some(vec!["OPENAI_API_KEY".into()]),
        );
        assert_eq!(intent.cwd, "/remote/worktree");
        assert_eq!(intent.shell, None);
        assert_eq!(intent.starship_preset, None);
        assert_eq!(intent.api_port, None);
        assert_eq!(intent.env_remove, ["OPENAI_API_KEY"]);
        assert!(
            intent.env.is_empty(),
            "only consumed authorized bindings append later"
        );
        let wire = serde_json::to_string(&intent).unwrap();
        for desktop in [
            "/Applications/",
            "/Users/dev/",
            "shell-integration",
            "starship-",
        ] {
            assert!(
                !wire.contains(desktop),
                "desktop provenance leaked: {desktop}"
            );
        }
    }

    #[test]
    fn remote_routes_are_guarded_before_pool_and_legacy_fallback_is_deleted() {
        let pty = include_str!("pty.rs");
        let warm = pty
            .split("pub(crate) async fn attach_warm_from_args")
            .nth(1)
            .unwrap();
        assert!(
            warm.find("attach_warm_from_args_core(").unwrap()
                < warm.find("acquire_ready()").unwrap(),
            "remote warm requests branch before touching the local pool"
        );
        let backend = include_str!("pty_backend.rs");
        assert!(!pty.contains(&["Spawn", "Host"].concat()));
        assert!(!backend.contains(&["pub enum Spawn", "Host"].concat()));
        assert!(!backend.contains("fn resolve_spawn_host("));
        assert!(!backend.contains("falls_open("));
        assert!(!backend.contains("admits("));
        let refill = include_str!("pty_host.rs");
        assert!(refill.contains("SpawnPlacement::ResolveLocal"));
        let viewer = include_str!("../../../src/shared/stores/viewer-store.ts");
        assert!(viewer.contains("machineId: LOCAL_MACHINE_ID"));
        assert!(!viewer.contains("machineId: s.activeMachine"));
        assert!(viewer.contains("terminalRespawnTab(\n      LOCAL_MACHINE_ID,"));
    }

    #[test]
    fn binding_sequence_preserves_before_check_and_consumes_once_at_send_boundary() {
        let consumed = std::cell::Cell::new(0);
        let failure = prepare_send_bindings(
            || Err("stale before binding".into()),
            || Ok(()),
            || {
                consumed.set(consumed.get() + 1);
                Ok(())
            },
        );
        assert_eq!(failure.unwrap_err(), "stale before binding");
        assert_eq!(consumed.get(), 0, "check failure preserves the binding");

        let checks = std::cell::Cell::new(0);
        let failure = prepare_send_bindings(
            || {
                checks.set(checks.get() + 1);
                if checks.get() == 1 {
                    Ok(())
                } else {
                    Err("stale at send".into())
                }
            },
            || Ok(()),
            || {
                consumed.set(consumed.get() + 1);
                Ok(())
            },
        );
        assert_eq!(failure.unwrap_err(), "stale at send");
        assert_eq!(
            consumed.get(),
            1,
            "send-boundary failure consumes exactly once"
        );
    }

    #[test]
    fn spawns_without_a_preset_use_settings_then_the_users_prompt() {
        // An agent launch names no preset: the Settings choice wins...
        assert_eq!(
            effective_prompt_preset(None, Some("compact".into())),
            "compact"
        );
        // ...before Settings reached Rust (and on new installs), the user's own prompt.
        assert_eq!(effective_prompt_preset(None, None), "plain");
        assert_eq!(
            effective_prompt_preset(Some("full".into()), Some("compact".into())),
            "full"
        );
    }

    /// Tauri signs every `externalBin` with the app's one entitlements file, so
    /// the emulator worker, the only binary that needs JIT, ships outside it
    /// (signed by `scripts/build-emulator.sh` with its own entitlements) and
    /// the app declares none.
    #[test]
    fn only_the_emulator_worker_is_entitled_to_jit() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        let bundle = &conf["bundle"];
        assert!(
            bundle["macOS"]["entitlements"].is_null(),
            "the app carries no entitlements"
        );
        assert!(!bundle["externalBin"]
            .as_array()
            .unwrap()
            .iter()
            .any(|bin| bin
                .as_str()
                .is_some_and(|bin| bin.contains("codesense-emulator"))));
        assert_eq!(
            bundle["macOS"]["files"]["MacOS/codesense-emulator"],
            "binaries/codesense-emulator"
        );
        let entitlements = include_str!("../../emulator.entitlements.plist");
        assert!(entitlements.contains("<key>com.apple.security.cs.allow-jit</key>"));
        assert_eq!(
            entitlements.matches("<key>").count(),
            1,
            "JIT and nothing else"
        );
        let resources = bundle["resources"].as_object().unwrap();
        for (source, target) in [
            (
                "resources/emulator/NOTICE.md",
                "resources/emulator/NOTICE.md",
            ),
            (
                "binaries/codesense-emulator.cjs",
                "resources/emulator/codesense-emulator.cjs",
            ),
        ] {
            assert_eq!(resources.get(source).and_then(|v| v.as_str()), Some(target));
        }
    }

    #[test]
    fn every_shell_init_script_is_bundled() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        // Source path → bundled path.
        let resources = conf["bundle"]["resources"].as_object().unwrap();
        for kind in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
            let script = format!(
                "resources/shell-integration/codesense-shell-init.{}",
                kind.init_script_ext()
            );
            assert_eq!(
                resources.get(&script).and_then(|v| v.as_str()),
                Some(script.as_str()),
                "{script} not bundled at its own path"
            );
            assert!(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(&script)
                    .is_file(),
                "{script} missing"
            );
        }
    }

    /// `tauri build --config` merges an overlay as a JSON merge patch: objects
    /// merge key by key, arrays are replaced whole. The release overlay adds
    /// the Linux host agents, each beside its recorded sha256, and must keep
    /// every base resource — which holds only while `bundle.resources` is a
    /// map in both files.
    #[test]
    fn the_release_overlay_adds_the_host_agents_and_keeps_every_resource() {
        fn merge_patch(target: &mut serde_json::Value, patch: &serde_json::Value) {
            match (target, patch) {
                (serde_json::Value::Object(target), serde_json::Value::Object(patch)) => {
                    for (key, value) in patch {
                        if value.is_null() {
                            target.remove(key);
                        } else {
                            merge_patch(
                                target.entry(key.clone()).or_insert(serde_json::Value::Null),
                                value,
                            );
                        }
                    }
                }
                (target, patch) => *target = patch.clone(),
            }
        }
        let base: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        let overlay: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.host-agents.conf.json")).unwrap();
        let mut merged = base.clone();
        merge_patch(&mut merged, &overlay);

        let resources = merged["bundle"]["resources"].as_object().unwrap();
        for (source, target) in base["bundle"]["resources"].as_object().unwrap() {
            assert_eq!(resources.get(source), Some(target), "{source} kept");
        }
        let bundled = |file: &str| {
            assert_eq!(
                resources
                    .get(&format!("binaries/{file}"))
                    .and_then(|v| v.as_str()),
                Some(format!("host-agent/{file}").as_str()),
                "{file} bundled beside its artifact"
            );
        };
        for triple in ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"] {
            for artifact in [
                format!("codesense-session-server-{triple}"),
                format!("codesense-emulator-{triple}"),
                format!("codesense-bridge-{triple}"),
            ] {
                bundled(&artifact);
                bundled(&format!("{artifact}.sha256"));
            }
        }
        bundled(codesense_session_server::install::EMULATOR_NOTICES);
    }
}

#[cfg(test)]
mod terminal_report_tests {
    use super::bracketed_paste;
    use super::is_terminal_report;

    #[test]
    fn reports_the_terminal_sends_by_itself_are_not_keystrokes() {
        assert!(is_terminal_report("\x1b[I"));
        assert!(is_terminal_report("\x1b[O"));
        assert!(is_terminal_report("\x1b[12;40R"));
        assert!(is_terminal_report("\x1b[?1;2c"));
        assert!(is_terminal_report("\x1b]11;rgb:0000/0000/0000\x07"));
        assert!(is_terminal_report("\x1b]10;rgb:ffff/ffff/ffff\x1b\\\x1b[I"));
        assert!(is_terminal_report("\x1b[?2004;1$y"));
        assert!(is_terminal_report("\x1bP1$r0;1m\x1b\\"));
    }

    #[test]
    fn keystrokes_are_not_reports() {
        assert!(!is_terminal_report("a"));
        assert!(!is_terminal_report("\r"));
        assert!(!is_terminal_report("\x1b[A"));
        assert!(!is_terminal_report("\x1b"));
        assert!(!is_terminal_report("\x1b[Ihello"));
    }

    /// SW-DP-23 review 19: markers are stripped in one linear pass, with the same
    /// result as removing them until none is left (nested split markers included).
    #[test]
    fn paste_markers_are_stripped_in_one_pass_like_removing_until_none_is_left() {
        fn naive(text: &str) -> String {
            let mut text = text.to_string();
            loop {
                let next = text.replace("\x1b[200~", "").replace("\x1b[201~", "");
                if next == text {
                    return text;
                }
                text = next;
            }
        }
        let k = 3000;
        let nested = format!("{}\x1b[201~{}", "\x1b[20".repeat(k), "0~".repeat(k));
        assert_eq!(crate::commands::pty::strip_paste_markers(&nested), "");
        let parts = [
            "\x1b",
            "[",
            "2",
            "0",
            "1",
            "~",
            "a",
            "\x1b[200~",
            "\x1b[201~",
            "\n",
            "é",
        ];
        let mut seed: u64 = 7;
        for i in 0..2000 {
            let mut text = String::new();
            for _ in 0..(i % 24) + 1 {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                text.push_str(parts[(seed >> 33) as usize % parts.len()]);
            }
            assert_eq!(
                crate::commands::pty::strip_paste_markers(&text),
                naive(&text),
                "{text:?}"
            );
        }
    }

    /// A multi-line paste arrives bracketed; markers inside it
    /// (even split ones) cannot end it early; one line goes as typed.
    #[test]
    fn bracketed_paste_only_wraps_line_breaks_without_appending_enter() {
        assert_eq!(bracketed_paste("ls -la"), "ls -la");
        assert_eq!(
            bracketed_paste("one\ntwo\nthree"),
            "\x1b[200~one\ntwo\nthree\x1b[201~"
        );
        assert_eq!(bracketed_paste("a\x1b[201~b\x1b[200~c"), "abc");
        assert_eq!(bracketed_paste("x\x1b[20\x1b[201~0~y"), "xy");
        assert_eq!(bracketed_paste("a\rb"), "\x1b[200~a\rb\x1b[201~");
    }

    /// SW-DP-23: `write_pty` and the dashboard's owner input share one write
    /// path (no second PTY write primitive), and so do the two resizes.
    #[test]
    fn write_pty_and_dashboard_input_share_one_write_path() {
        let pty = include_str!("pty.rs");
        let agents = include_str!("../dashboard_publish/agents.rs");
        // The function's whole body, from its signature to its matching brace.
        let body = |src: &'static str, start: &str| -> &'static str {
            let at = src.find(start).expect("function present");
            let open = at + src[at..].find('{').expect("a body");
            let mut depth = 0usize;
            for (i, c) in src[open..].char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            return &src[at..open + i + 1];
                        }
                    }
                    _ => {}
                }
            }
            panic!("unbalanced body of {start}");
        };
        let write_pty = body(pty, "pub async fn write_pty(");
        assert!(write_pty.contains(
            "super::write_session_input(app, session_id, data, remote, super::HumanInput::Detect)"
        ));
        assert!(!write_pty.contains("host.write("));
        let resize_pty = body(pty, "pub async fn resize_pty(");
        assert!(resize_pty.contains("super::resize_session_as("));
        assert!(agents.contains("crate::commands::pty::write_session_input("));
        assert!(agents.contains("crate::commands::pty::HumanInput::Typed"));
        assert!(agents.contains("crate::commands::pty::resize_session_as("));
        // A keystroke the owner typed counts as human input, as on the Mac.
        assert!(!is_terminal_report("ls\r"));
    }

    /// SW-DP-23 review 3: the dashboard's owner input always counts as human
    /// input, even bytes that look like a terminal report (Shift+F3, focus);
    /// a pane's write still tells reports apart; a remote write never counts.
    #[test]
    fn dashboard_input_counts_as_human_even_when_it_looks_like_a_report() {
        use super::{counts_as_human, HumanInput};
        for report in ["\x1b[1;2R", "\x1b[I", "\x1b[O"] {
            assert!(is_terminal_report(report), "{report:?}");
            assert!(counts_as_human(false, HumanInput::Typed, report));
            assert!(!counts_as_human(false, HumanInput::Detect, report));
        }
        assert!(counts_as_human(false, HumanInput::Detect, "ls\r"));
        assert!(!counts_as_human(true, HumanInput::Typed, "ls\r"));
    }
}

#[cfg(test)]
mod spend_telemetry_seams {
    use super::*;
    #[test]
    fn engine_usage_export_wins_in_real_pty_intents_for_both_clis() {
        use codesense_pty_core::{
            agent_cli::{descriptor, AgentCli},
            spawn::HostEnvironment,
        };
        for cli in [AgentCli::Claude, AgentCli::Codex] {
            let saved = vec![
                (
                    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT".into(),
                    "https://other.example/logs".into(),
                ),
                ("OTEL_SDK_DISABLED".into(), "true".into()),
                ("CLAUDE_CODE_ENABLE_TELEMETRY".into(), "0".into()),
            ];
            let mut args = vec![descriptor(cli).binary.clone()];
            let mut env = saved.clone();
            crate::session_usage::telemetry::configure_export(
                descriptor(cli).telemetry.as_ref(),
                Some(8737),
                "usage-cap",
                &mut env,
                &mut args,
            );
            let mut intent = build_local_spawn_intent(
                String::new(),
                Some(args),
                String::new(),
                80,
                24,
                Some(false),
                None,
                None,
                None,
                "plain".into(),
                Some(8737),
            );
            apply_bindings(
                &mut intent,
                Vec::new(),
                Vec::new(),
                crate::commands::agent_accounts::LaunchEnvironment {
                    values: env,
                    agent_env: saved,
                    ..Default::default()
                },
            );
            let spec = intent.resolve(
                "usage-cap",
                &HostEnvironment::with_vars(None, None, [("PATH".into(), "/usr/bin".into())]),
            );
            let get = |name| {
                spec.command
                    .get_env(name)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            };
            assert_eq!(
                get("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"),
                format!(
                    "{}/v1/logs",
                    crate::http::routes::telemetry::endpoint_for(8737, "usage-cap")
                )
            );
            assert_eq!(get("OTEL_SDK_DISABLED"), "false");
            if cli == AgentCli::Claude {
                assert_eq!(get("CLAUDE_CODE_ENABLE_TELEMETRY"), "1");
            } else {
                assert!(format!("{:?}", spec.command.get_argv()).contains("otel.exporter"));
            }
        }
    }
}

#[cfg(test)]
mod automatic_spend_input_seams {
    use super::*;
    #[test]
    fn a_granted_page_prompt_is_refused_by_the_shared_submit_boundary_but_native_typing_works() {
        use crate::dashboard_stream::frame::{FrameGrant, FramePolicy, FrameStore};
        use crate::verbs::{Decision, Dispatcher, Policy};
        use codesense_pty_core::{
            domain::TerminalDomain,
            layout::ServerIdentity,
            runner::{LocalPtyRunner, PtyRunner},
            TerminalHost,
        };
        use codesense_session_server::testing::{registration, sh_spec, wait_until, RecordingSink};
        use std::sync::Arc;
        let frames = FrameStore::default();
        let mut frame = frames.mint(
            9321,
            FrameGrant {
                frame_id: String::new(),
                principal: "human".into(),
                dashboard: "board".into(),
                slot: "right-pane".into(),
                team: None,
                scope: "open".into(),
            },
            "/ctx".into(),
            "/ctx/board.html".into(),
            "/ctx".into(),
            FramePolicy::default(),
            None,
        );
        frame.effective_grant = vec!["sessions:send".into()];
        assert!(matches!(
            Dispatcher::default().dispatch_frame(
                &frame,
                "sendPrompt",
                serde_json::json!({"handle":"worker", "text":"continue"}),
                &Policy::default(),
                0
            ),
            Decision::Admit { .. }
        ));
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let sink = Arc::new(RecordingSink::default());
        let host = TerminalHost::new(
            ServerIdentity::ephemeral("budget-page"),
            sink.clone(),
            runtime.handle().clone(),
        );
        let pty = runtime
            .block_on(LocalPtyRunner.spawn(sh_spec(
                "budget-page",
                r#"stty -echo; printf READY; read line; printf 'GOT=%s' "$line""#,
            )))
            .unwrap();
        host.register(pty, registration("budget-page", None, false))
            .unwrap();
        assert!(wait_until(|| sink.output("budget-page").contains("READY")));
        let session = PtySessionId::new("budget-page");
        let (_dir, mut usage) = crate::session_usage::tests::store();
        usage
            .attribute("budget-page", "org", None, "worker", "worker", None)
            .unwrap();
        let mut row = crate::session_usage::tests::event(
            "spent",
            "budget-page",
            "claude-opus-5-5",
            chrono::Utc::now().timestamp_millis(),
            Default::default(),
        );
        row.reported_cost_usd = Some(1.0);
        usage.ingest(&[row]).unwrap();
        let status = usage
            .cap_status(
                "agent",
                "worker",
                crate::session_usage::spend_caps::Budget {
                    per_day: Some(1.0),
                    per_week: None,
                },
                chrono::Local::now().date_naive(),
                false,
            )
            .unwrap();
        let value = crate::session_usage::spend_caps::Evaluation {
            agents: [("worker".into(), status)].into(),
            ..Default::default()
        };
        crate::session_usage::spend_caps::push_session_holds(
            &Ok(value),
            [("budget-page".into(), Some("worker@1".into()))],
            &[],
        )
        .unwrap();
        let denied = runtime
            .block_on(automatic_input_checked(
                async { crate::session_usage::spend_caps::refuse_held_session("budget-page") },
                async {
                    host.submit_line(&session, "continue", 0, 0, 0, 0)
                        .map_err(String::from)
                },
            ))
            .unwrap_err();
        assert!(denied.contains("cap $1.0000"));
        assert!(!sink.output("budget-page").contains("GOT="));
        host.write(&session, b"native-human\n").unwrap();
        assert!(wait_until(|| sink
            .output("budget-page")
            .contains("GOT=native-human")));
    }
}
