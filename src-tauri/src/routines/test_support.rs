// SPDX-License-Identifier: MIT
//! Shared support for routine run seam tests: a temp CodeSense home the whole
//! process resolves to, a stub agent process, a launcher that starts it and
//! registers its session the way the headless spine does, and the real claim
//! loop over the temp coordinator.
//!
//! The app's launch spines and `signal_done` handler are bound to the desktop
//! runtime, so a test cannot build them. Everything around them is real: the
//! launcher registers the stub session in a real `DashboardLaunchState` with
//! the run's provenance, and the signal and exit go through the same outcome
//! funnel (`record_signal_outcome`, `record_captured_outcome`) the handler and
//! the headless completion task call.

use std::collections::HashMap;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::commands::dashboard_launch::{
    headless_run_outcome, DashboardLaunchState, RunOutcomeContext, SignalOutcomeGuard,
};
use crate::commands::pty_backend::Liveness;
use crate::commands::sprint_run::claim_loop::{drain_runnable, ClaimLoopDeps};
use crate::commands::sprint_run::run_store::OutcomeSource;
use crate::context_repo::{path_to_org_root, path_to_team_content_dir};
use crate::coordinator::{
    default_store_path, OrchestrationStore, SqliteOrchestrationStore, WorkerId,
};
use crate::dashboard_stream::SessionStatus;
use crate::domain::Principal;
use crate::shared::proc_capture::CaptureOutput;

use crate::agents::model::{Agent, LaunchMode as AgentLaunchMode};
use crate::agents::AgentKey;
use codesense_pty_core::agent_cli::{AgentCli, TranscriptFormat};

/// What `opencode run --format json` printed for a run that ended on a final text, and for one a
/// provider rejected with an `error` event.
pub const OPENCODE_TEXT_RUN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/crates/codesense-pty-core/tests/fixtures/opencode/text.jsonl"
));
pub const OPENCODE_ERROR_RUN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/crates/codesense-pty-core/tests/fixtures/opencode/error-400.jsonl"
));

use super::dispatch::{LaunchFuture, LaunchRequest, Launcher};
use super::finish::Sessions;

pub const ORG: &str = "acme/api";
pub const TEAM: &str = "backend";
/// The team agent every test org has, which test routines run.
pub const AGENT: &str = "default";

/// A temp directory set as `$HOME` for the test's lifetime, so the app's
/// CodeSense home (and so the run-outcome funnel) resolves inside it. Holds the
/// process env lock: tests that move `$HOME` run one at a time.
pub struct TempHome {
    _env: MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
    previous_tmp: Option<Option<std::ffi::OsString>>,
    previous_xdg: Option<Option<std::ffi::OsString>>,
    dir: tempfile::TempDir,
}

impl TempHome {
    /// A fresh home with Alice's org cloned and `backend` synced.
    pub fn new() -> Self {
        let env = crate::content_engine::test_support::env_guard();
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let home = Self {
            _env: env,
            previous,
            previous_tmp: None,
            previous_xdg: None,
            dir,
        };
        write_org(&home.codesense_home(), 1);
        home
    }

    /// Give instances one disposable shared temp root while holding the env lock.
    pub fn isolate_tmpdir(&mut self) -> PathBuf {
        // Other parallel tests can allocate here while TMPDIR is set. Keep their
        // parent alive after this home's guard ends; the runner or OS retires it.
        let tmp = tempfile::tempdir().unwrap().keep();
        self.previous_tmp
            .get_or_insert_with(|| std::env::var_os("TMPDIR"));
        std::env::set_var("TMPDIR", &tmp);
        tmp
    }

    /// Point `XDG_CONFIG_HOME` at a controlled path under this temp home, so a test
    /// that captures it (OpenCode's isolation env records the ambient value to restore
    /// it for shell tools) is deterministic whatever the runner's ambient value was.
    /// Restored on drop, like `HOME`.
    pub fn isolate_xdg_config_home(&mut self) -> PathBuf {
        self.previous_xdg
            .get_or_insert_with(|| std::env::var_os("XDG_CONFIG_HOME"));
        let dir = self.dir.path().join("xdg-config");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        dir
    }

    /// `$HOME/.codesense`: the home every app path resolves to.
    pub fn codesense_home(&self) -> PathBuf {
        crate::commands::paths::codesense_home_dir().expect("HOME is set")
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// A second Mac beside this one: its own `$HOME` (so its own host id and
    /// coordinator) under this temp dir, with `user_id`'s clone of the org.
    /// Returns that `$HOME`; [`Self::act_as`] makes it the process's.
    pub fn other_mac(&self, name: &str, user_id: u64) -> PathBuf {
        let root = self.dir.path().join(name);
        write_org(&root.join(".codesense"), user_id);
        root
    }

    /// Point `$HOME` at `root` (this Mac's [`Self::root`], or an
    /// [`Self::other_mac`]), so what resolves the app's home, such as the run
    /// outcome funnel, acts as that Mac. This guard still holds the env lock
    /// and restores the original `$HOME` when dropped.
    pub fn act_as(&self, root: &Path) {
        std::env::set_var("HOME", root);
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        if let Some(previous) = self.previous_tmp.take() {
            match previous {
                Some(tmp) => std::env::set_var("TMPDIR", tmp),
                None => std::env::remove_var("TMPDIR"),
            }
        }
        if let Some(previous) = self.previous_xdg.take() {
            match previous {
                Some(xdg) => std::env::set_var("XDG_CONFIG_HOME", xdg),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
        match self.previous.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// The profile and selection state used by the real launch account resolver.
pub fn launch_app(home: &Path) -> tauri::App<tauri::test::MockRuntime> {
    tauri::test::mock_builder()
        .manage(crate::commands::local_profile::LocalProfileState::for_test(
            home,
            1,
            crate::commands::local_profile::MachineOwner::Local {
                id: uuid::Uuid::new_v4().to_string(),
            },
        ))
        .manage(crate::commands::watch::WatcherState::default())
        .manage(crate::mcp::state::ActiveTeamState::default())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("mock launch app")
}

pub fn alice() -> Principal {
    Principal::from(1u64)
}

/// A locally-cloned org for `user_id`, with `backend` synced.
pub fn write_org(home: &Path, user_id: u64) -> PathBuf {
    let org_root = path_to_org_root(home, &Principal::from(user_id), ORG).unwrap();
    crate::context_repo::scratch_space::set_active_org(home, Some(ORG));
    std::fs::create_dir_all(org_root.join(".git")).unwrap();
    std::fs::write(
        org_root.join("org.yml"),
        format!("slug: {ORG}\ndisplay_name: Test Org\ncreated_at: \"2026-01-01T00:00:00Z\"\n"),
    )
    .unwrap();
    std::fs::write(
        org_root.join("people.yml"),
        "people:\n  - github_user_id: 1\n    github_username: alice\n",
    )
    .unwrap();
    std::fs::write(org_root.join("teams.yml"), "teams:\n  - name: backend\n").unwrap();
    std::fs::create_dir_all(path_to_team_content_dir(&org_root, TEAM)).unwrap();
    save_team_agent(
        home,
        &Principal::from(user_id),
        agent(AGENT, AgentCli::Claude),
    );
    org_root
}

/// A plain agent: no args, servers, startup paths or permissions.
pub fn agent(id: &str, runtime: AgentCli) -> Agent {
    Agent {
        role: None,
        id: id.into(),
        name: format!("Agent {id}"),
        description: String::new(),
        created_by: String::new(),
        runtime,
        args: Vec::new(),
        launch: AgentLaunchMode::Foreground,
        mcp: Default::default(),
        startup: Vec::new(),
        context: None,
        permissions: None,
        default_prompt: String::new(),
        avatar: None,
        allow_wake: false,
        resume_after_restart: false,
        budget_usd: None,
        revision: 0,
        archived_at: None,
        extra: Default::default(),
        env: Default::default(),
    }
}

/// Save `agent` into the test team through the real agent store, as `user`.
pub fn save_team_agent(home: &Path, user: &Principal, agent: Agent) -> AgentKey {
    crate::agents::service::save(
        home,
        user,
        crate::agents::SaveTarget::Create(crate::agents::Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        }),
        agent,
        &super::Author::user(user.as_str()),
    )
    .unwrap()
    .key
}

/// Save `agent` into `user`'s personal space through the real agent store.
pub fn save_personal_agent(home: &Path, user: &Principal, agent: Agent) -> AgentKey {
    crate::agents::service::save(
        home,
        user,
        crate::agents::SaveTarget::Create(crate::agents::Placement::Personal(
            super::PersonalOwner {
                github_user_id: user.as_str().parse().unwrap(),
                username: format!("user{user}"),
                display_name: format!("User {user}"),
                avatar_url: None,
                org: "acme/api".into(),
            },
        )),
        agent,
        &super::Author::user(user.as_str()),
    )
    .unwrap()
    .key
}

/// A stand-in agent: a shell script that keeps the prompt it reads on stdin in
/// `$STUB_PROMPT_FILE`, then sleeps for `$STUB_AGENT_SECONDS` (default 30) and
/// exits 0. It never calls back; tests signal and exit for it.
pub fn stub_agent(dir: &Path) -> PathBuf {
    let path = dir.join("stub-agent.sh");
    std::fs::write(
        &path,
        "#!/bin/sh\ncat > \"$STUB_PROMPT_FILE.part\" && mv \"$STUB_PROMPT_FILE.part\" \"$STUB_PROMPT_FILE\"\n\
         sleep \"${STUB_AGENT_SECONDS:-30}\"\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// Copy a directory tree, as a git sync would bring a routine to another
/// clone.
pub fn copy_dir(from: &Path, to: &Path) {
    for entry in walkdir::WalkDir::new(from) {
        let entry = entry.unwrap();
        let dest = to.join(entry.path().strip_prefix(from).unwrap());
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// A process exit status with exit code `code`.
pub fn exit_code(code: i32) -> ExitStatus {
    ExitStatus::from_raw(code << 8)
}

/// A process exit status of a death by `signal`.
pub fn killed_by(signal: i32) -> ExitStatus {
    ExitStatus::from_raw(signal)
}

/// One launch the stub launcher made.
#[derive(Debug, Clone)]
pub struct Launched {
    pub session_id: String,
    pub request: LaunchRequest,
    /// The argv the launch's generated entries added (`--settings <file>`,
    /// the MCP scope's flags), when the launcher writes them.
    pub generated_argv: Vec<String>,
}

/// Starts the stub agent for each launch and registers its session in a real
/// launch registry with the run's provenance, as the headless spine does.
pub struct StubLauncher {
    pub registry: Arc<DashboardLaunchState>,
    agent: PathBuf,
    calls: AtomicUsize,
    launched: Mutex<Vec<Launched>>,
    children: Mutex<HashMap<String, Child>>,
    seq: AtomicU32,
    writes_generated: bool,
    /// What every launch is refused with, as the app's launcher refuses one (a saved
    /// `--safe-mode`, a missing helper) before any process exists.
    refusal: Option<String>,
}

impl StubLauncher {
    pub fn new(agent: PathBuf) -> Self {
        Self {
            registry: Arc::new(DashboardLaunchState::new(8)),
            agent,
            calls: AtomicUsize::new(0),
            launched: Mutex::new(Vec::new()),
            children: Mutex::new(HashMap::new()),
            seq: AtomicU32::new(0),
            writes_generated: false,
            refusal: None,
        }
    }

    /// Also write each launch's MCP scope and generated entries (the per-launch
    /// Claude settings file, Codex's scoped config) with the helper the launch
    /// spines call, holding the settings file on the session's registry entry
    /// as they do, so it goes when the session is released.
    pub fn writing_generated(mut self) -> Self {
        self.writes_generated = true;
        self
    }

    /// Refuse every launch with `refusal`.
    pub fn refusing(mut self, refusal: impl Into<String>) -> Self {
        self.refusal = Some(refusal.into());
        self
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn launched(&self) -> Vec<Launched> {
        self.launched.lock().unwrap().clone()
    }

    pub fn last(&self) -> Launched {
        self.launched().pop().expect("a launch")
    }

    /// Whether the stub process of `session_id` is still running.
    pub fn is_running(&self, session_id: &str) -> bool {
        self.children
            .lock()
            .unwrap()
            .get_mut(session_id)
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }

    /// Kill the stub process of `session_id` and nothing else: no outcome is
    /// written, as when the whole app dies with it.
    pub fn kill_child(&self, session_id: &str) {
        if let Some(mut child) = self.children.lock().unwrap().remove(session_id) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// The agent's `signal_done`, through the handler's outcome path: attest a
    /// live headless session, then persist through `record_signal_outcome`.
    pub async fn signal_done(&self, session_id: &str, status: &str, summary: &str) {
        signal_done_via(&self.registry, session_id, status, summary).await;
    }

    /// The prompt the stub process of `session_id` read on its stdin, byte
    /// for byte.
    pub fn received_prompt(&self, session_id: &str) -> Vec<u8> {
        let path = self.prompt_file(session_id);
        for _ in 0..500 {
            if let Ok(bytes) = std::fs::read(&path) {
                return bytes;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("the stub agent of {session_id} never read a prompt");
    }

    fn prompt_file(&self, session_id: &str) -> PathBuf {
        self.agent.with_file_name(format!("{session_id}.prompt"))
    }

    /// The stub process exits 0 with `stdout` on its output.
    pub async fn exit(&self, session_id: &str) {
        self.exit_with(session_id, exit_code(0), "built it", "")
            .await;
    }

    /// The stub process ends with `status`; its completion goes through the
    /// headless completion path: the capture is classified by the same
    /// classifier, and a `ProcessExit` outcome is written unless the agent
    /// signalled.
    pub async fn exit_with(
        &self,
        session_id: &str,
        status: ExitStatus,
        stdout: &str,
        stderr: &str,
    ) {
        self.exit_structured(session_id, None, status, stdout, stderr)
            .await;
    }

    /// As [`Self::exit_with`], for a run whose agent declares `format` for its stdout.
    pub async fn exit_structured(
        &self,
        session_id: &str,
        format: Option<TranscriptFormat>,
        status: ExitStatus,
        stdout: &str,
        stderr: &str,
    ) {
        self.end_capture(
            session_id,
            format,
            CaptureOutput {
                status: Some(status),
                stdout: stdout.as_bytes().to_vec(),
                stderr: stderr.as_bytes().to_vec(),
                timed_out: false,
                cancelled: false,
            },
        )
        .await;
    }

    /// The headless capture killed the stub process at its own time limit.
    pub async fn capture_timed_out(&self, session_id: &str) {
        self.capture_timed_out_structured(session_id, None, "").await;
    }

    /// As [`Self::capture_timed_out`], for a run that declares `format` and had printed `stdout`.
    pub async fn capture_timed_out_structured(
        &self,
        session_id: &str,
        format: Option<TranscriptFormat>,
        stdout: &str,
    ) {
        self.end_capture(
            session_id,
            format,
            CaptureOutput {
                status: None,
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
                timed_out: true,
                cancelled: false,
            },
        )
        .await;
    }

    /// Someone stopped the stub process's capture outside the routine.
    pub async fn capture_stopped(&self, session_id: &str) {
        self.capture_stopped_structured(session_id, None, "").await;
    }

    /// As [`Self::capture_stopped`], for a run that declares `format` and had printed `stdout`.
    pub async fn capture_stopped_structured(
        &self,
        session_id: &str,
        format: Option<TranscriptFormat>,
        stdout: &str,
    ) {
        self.end_capture(
            session_id,
            format,
            CaptureOutput {
                status: Some(killed_by(15)),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
                timed_out: false,
                cancelled: true,
            },
        )
        .await;
    }

    /// The run identity a launch captured for `session_id`, for a completion that arrives
    /// after the session was released.
    pub fn run_context(&self, session_id: &str) -> Option<RunOutcomeContext> {
        self.registry.run_context_for_session(session_id)
    }

    /// As [`Self::end_capture`], for a completion that holds the `ctx` its launch captured.
    pub async fn exit_captured(
        &self,
        ctx: Option<RunOutcomeContext>,
        session_id: &str,
        format: Option<TranscriptFormat>,
        status: ExitStatus,
        stdout: &str,
    ) {
        self.finish_capture(
            ctx,
            session_id,
            format,
            CaptureOutput {
                status: Some(status),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
                timed_out: false,
                cancelled: false,
            },
        )
        .await;
    }

    async fn end_capture(
        &self,
        session_id: &str,
        format: Option<TranscriptFormat>,
        capture: CaptureOutput,
    ) {
        let ctx = self.run_context(session_id);
        self.finish_capture(ctx, session_id, format, capture).await;
    }

    async fn finish_capture(
        &self,
        ctx: Option<RunOutcomeContext>,
        session_id: &str,
        format: Option<TranscriptFormat>,
        capture: CaptureOutput,
    ) {
        if let Some(mut child) = self.children.lock().unwrap().remove(session_id) {
            let _ = child.kill();
            let _ = child.wait();
        }
        let capture = Ok(capture);
        let (wire, summary) = headless_run_outcome(format, &capture);
        let exit_code = capture.as_ref().ok().and_then(CaptureOutput::exit_code);
        let ctx = ctx.map(|ctx| ctx.with_headless_format(format));
        self.registry.mark_headless_exited(session_id);
        self.registry.release_for_session(session_id);
        crate::commands::mcp_scope::remove_scope_config(session_id);
        if !self.registry.take_signal_attested(session_id) {
            if let (Some(ctx), Some(status)) = (ctx, SessionStatus::from_wire(wire)) {
                DashboardLaunchState::record_captured_outcome(
                    ctx,
                    status,
                    Some(summary),
                    exit_code,
                    OutcomeSource::ProcessExit,
                    "test.headless.outcome",
                    None,
                )
                .await;
            }
        }
    }
}

/// `signal_done` as the MCP handler persists it, against `registry`: the same
/// in-flight guard and the same outcome funnel.
pub async fn signal_done_via(
    registry: &DashboardLaunchState,
    session_id: &str,
    status: &str,
    summary: &str,
) {
    let guard = SignalOutcomeGuard::begin(registry, session_id);
    crate::commands::dashboard_launch::record_signal_outcome(
        Some(registry),
        guard.as_ref().and_then(|guard| guard.context.clone()),
        session_id,
        status,
        Some(summary.to_string()),
        || async { None },
        None,
    )
    .await;
}

impl Launcher for StubLauncher {
    fn command(&self, request: super::command::CommandRequest) -> LaunchFuture<'_> {
        Box::pin(async move {
            let (session, pid, cancel) = super::command::register(&request, &self.registry)?;
            let registry = self.registry.clone();
            let done = session.clone();
            tokio::spawn(async move {
                super::command::capture(request, pid, cancel).await;
                registry.release_for_session(&done);
            });
            Ok(session)
        })
    }

    fn launch(&self, request: LaunchRequest) -> LaunchFuture<'_> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(refusal) = &self.refusal {
                return Err(refusal.clone());
            }
            let handle = request.handle.clone();
            if !self.registry.begin(&handle).map_err(|e| e.to_string())? {
                return Err(format!(
                    "busy: a session is already active for handle '{handle}'"
                ));
            }
            let n = self.seq.fetch_add(1, Ordering::SeqCst);
            let prefix = if request.visible { "shell" } else { "headless" };
            let session_id = format!("{prefix}-stub-{n}");
            let mut generated_argv = Vec::new();
            let mut settings = None;
            if self.writes_generated {
                let (scope, _, applied) = crate::agents::launch::project_and_apply(
                    &request.agent,
                    &session_id,
                    &request.cwd,
                    None,
                    str::to_string,
                )
                .inspect_err(|_| {
                    self.registry.abort(&handle);
                })?;
                generated_argv.extend(scope.extra_args);
                generated_argv.extend(applied.argv(str::to_string));
                settings = applied.settings;
            }
            // The prompt goes in on stdin, as the headless spine sends it.
            let mut child = Command::new(&self.agent)
                .current_dir(&request.cwd)
                .env("STUB_PROMPT_FILE", self.prompt_file(&session_id))
                .stdin(Stdio::piped())
                .spawn()
                .map_err(|e| e.to_string())?;
            // A stub that died before reading is a test's to see in its
            // prompt, not a reason to leak the reserved slot and the child.
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(request.prompt.as_bytes());
            }
            self.registry
                .bind_with_account(&handle, &session_id, Some("routine"), None)?;
            self.registry
                .stamp_provenance(&handle, request.provenance.clone());
            // As the launch spines stamp every agent launch.
            self.registry
                .set_agent_revision_if_absent(&handle, request.agent.revision_label());
            if let Some(settings) = settings {
                self.registry.hold_launch_settings(&handle, settings);
            }
            if !request.visible {
                self.registry.mark_headless(
                    &handle,
                    // The stub is reaped here, not by process group.
                    Arc::new(AtomicU32::new(0)),
                    Default::default(),
                    Default::default(),
                );
            }
            self.children
                .lock()
                .unwrap()
                .insert(session_id.clone(), child);
            self.launched.lock().unwrap().push(Launched {
                session_id: session_id.clone(),
                request,
                generated_argv,
            });
            Ok(session_id)
        })
    }

    fn kill_run(&self, run_id: &str) {
        let targets = self.registry.drain_run(run_id);
        for session_id in targets.to_kill {
            if let Some(mut child) = self.children.lock().unwrap().remove(&session_id) {
                let _ = child.kill();
                let _ = child.wait();
            }
            self.registry.release_for_session(&session_id);
            crate::commands::mcp_scope::remove_scope_config(&session_id);
        }
    }
}

impl Sessions for StubLauncher {
    fn kill_run(&self, run_id: &str, _session_id: Option<&str>) {
        Launcher::kill_run(self, run_id);
    }

    fn liveness(&self, session_id: &str) -> Liveness<bool> {
        Liveness::Known(self.is_running(session_id))
    }
}

/// [`Sessions`] with scripted liveness and a record of kills.
#[derive(Default)]
pub struct FakeSessions {
    pub killed: Mutex<Vec<String>>,
    pub liveness: Mutex<HashMap<String, Liveness<bool>>>,
    /// Sessions on other machines, by `machine/session`: `Unknown` (the
    /// machine cannot be reached) unless set.
    pub remote: Mutex<HashMap<String, Liveness<bool>>>,
    /// Kills sent to other machines: `(run, machine, session)`.
    pub killed_on: Mutex<Vec<(String, String, Option<String>)>>,
}

impl FakeSessions {
    pub fn set(&self, session_id: &str, liveness: Liveness<bool>) {
        self.liveness
            .lock()
            .unwrap()
            .insert(session_id.to_string(), liveness);
    }
}

impl Sessions for FakeSessions {
    fn kill_run(&self, run_id: &str, _session_id: Option<&str>) {
        self.killed.lock().unwrap().push(run_id.to_string());
    }

    fn liveness(&self, session_id: &str) -> Liveness<bool> {
        self.liveness
            .lock()
            .unwrap()
            .get(session_id)
            .cloned()
            .unwrap_or(Liveness::Known(false))
    }

    fn liveness_on(&self, machine: Option<&str>, session_id: &str) -> Liveness<bool> {
        match machine {
            None => self.liveness(session_id),
            Some(machine) => self
                .remote
                .lock()
                .unwrap()
                .get(&format!("{machine}/{session_id}"))
                .cloned()
                .unwrap_or(Liveness::Unknown),
        }
    }

    fn kill_on(&self, run_id: &str, machine: Option<&str>, session_id: Option<&str>) {
        match machine {
            None => self.kill_run(run_id, session_id),
            Some(machine) => self.killed_on.lock().unwrap().push((
                run_id.to_string(),
                machine.to_string(),
                session_id.map(str::to_string),
            )),
        }
    }
}

/// Drain the claim loop once over `home`'s coordinator, dispatching routine
/// pointers through the claim loop's routine branch with `launcher`. Returns
/// how many runs it drove.
pub async fn drain_claim_loop(home: &Path, launcher: Arc<dyn Launcher>) -> usize {
    let store = SqliteOrchestrationStore::open(default_store_path(home)).unwrap();
    let worker = WorkerId::new("routine-test");
    let (claim_store, finish_store, exec_store) = (store.clone(), store.clone(), store);
    let (claim_worker, finish_worker) = (worker.clone(), worker);
    let home = home.to_path_buf();
    let deps = ClaimLoopDeps {
        claim: Arc::new(move || {
            let s = claim_store.clone();
            let w = claim_worker.clone();
            Box::pin(async move {
                s.claim_ready(&w, i64::MAX)
                    .map(|c| c.map(|c| c.run_id))
                    .map_err(|e| e.to_string())
            })
        }),
        execute: Arc::new(move |run_id| {
            let store = exec_store.clone();
            let launcher = Arc::clone(&launcher);
            let home = home.clone();
            Box::pin(async move {
                let json = store
                    .run_request_json(&run_id)
                    .map_err(|e| e.to_string())?
                    .ok_or("no pointer")?;
                super::dispatch::execute_pointer_json(home, &run_id, &json, launcher.as_ref())
                    .await
                    .ok_or_else(|| "not a routine pointer".to_string())?
            })
        }),
        finish: Arc::new(move |run_id| {
            let s = finish_store.clone();
            let w = finish_worker.clone();
            Box::pin(async move { s.finish_run(&run_id, &w).map_err(|e| e.to_string()) })
        }),
        notify: Arc::new(tokio::sync::Notify::new()),
    };
    drain_runnable(&deps).await
}

/// Every `routines:changed` this test process emitted. The sink is installed
/// on first use; nothing else installs one under test.
static CHANGES: Mutex<Vec<super::events::RoutineChanged>> = Mutex::new(Vec::new());

/// Start recording `routines:changed`.
pub fn record_changes() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        super::events::install(|change| {
            CHANGES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(change.clone())
        })
    });
}

/// How many `routines:changed` were emitted for `routine_key` so far. Tests
/// running in parallel share the sink, so each uses its own routine ids.
pub fn changes_of(routine_key: &str) -> usize {
    CHANGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|c| c.routine_key == routine_key)
        .count()
}

#[test]
fn shared_tmpdir_outlives_a_home_for_other_parallel_fixtures() {
    let fixture = {
        let mut home = TempHome::new();
        home.isolate_tmpdir();
        let fixture = tempfile::tempdir().unwrap();
        std::fs::write(fixture.path().join("still-in-use"), b"fixture").unwrap();
        fixture
    };
    assert_eq!(
        std::fs::read(fixture.path().join("still-in-use")).unwrap(),
        b"fixture"
    );
}

/// What `hermes chat -Q --format stream-json` printed (pinned 0.22.5): each recorded run.
macro_rules! hermes_run {
    ($name:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/crates/codesense-pty-core/tests/fixtures/hermes/",
            $name
        ))
    };
}
pub const HERMES_TEXT_RUN: &str = hermes_run!("text.jsonl");
pub const HERMES_DENY_RUN: &str = hermes_run!("deny-continue.jsonl");
pub const HERMES_ERROR_RUN: &str = hermes_run!("error-4xx.jsonl");
pub const HERMES_RECOVERED_RUN: &str = hermes_run!("recover-5xx.jsonl");
pub const HERMES_DEADLINE_RUN: &str = hermes_run!("error-5xx-timeout.jsonl");
