// SPDX-License-Identifier: MIT
//! Test support shared by this crate's tests and the app's sidecar-client
//! tests: a real sidecar served in-process on a short temporary instance root,
//! plus the polling helpers those tests need on loaded machines.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use codesense_pty_core::domain::{
    PortUnavailable, PtySessionId, TerminalDomain, TerminalDomainEvent, TerminalEventSink,
};
use codesense_pty_core::layout::ServerIdentity;
use codesense_pty_core::runner::{ExitInfo, PtySpec};
use codesense_pty_core::spawn::{HostEnvironment, SpawnIntent};
use codesense_pty_core::{SessionMeta, SessionRegistration};

use crate::host::{lock, Host};
pub use crate::remote_workspace::test_support::RemoteWorkspaceFixture;
use crate::server::{bind, serve};

/// Generous: the suites run on loaded shared machines.
pub const WAIT: Duration = Duration::from_secs(20);

/// A fresh instance root short enough for a socket path (macOS caps socket
/// paths at 104 bytes).
pub fn short_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("rt6")
        .tempdir_in("/tmp")
        .expect("create a temp instance root")
}

/// The environment that ties a launched sidecar binary to this test process:
/// it exits once the test run is gone, even if the run never drops its guards.
pub fn tie_to_this_process() -> (&'static str, String) {
    (
        crate::exit_with::EXIT_WITH_PID_ENV,
        std::process::id().to_string(),
    )
}

/// Kills, on drop, every sidecar process serving `root`. Create it BEFORE
/// launching the sidecar binary, so a test that fails before it learns the
/// sidecar's pid still reaps it.
pub struct ReapSidecarsAt(PathBuf);

impl ReapSidecarsAt {
    pub fn new(root: &Path) -> Self {
        Self(root.to_path_buf())
    }
}

impl Drop for ReapSidecarsAt {
    fn drop(&mut self) {
        for pid in sidecar_pids_rooted_at(&self.0) {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

/// The pids of `codesense-session-server` processes launched with
/// `--root <root>`.
pub fn sidecar_pids_rooted_at(root: &Path) -> Vec<u32> {
    let suffix = format!(" --root {}", root.display());
    let Ok(out) = std::process::Command::new("ps")
        .args(["-Aww", "-o", "pid=,args="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .split('\n')
        .filter_map(|line| {
            let (pid, args) = line.trim().split_once(' ')?;
            let args = args.trim();
            let (program, _) = args.split_once(" --root ")?;
            (program.contains("codesense-session-server") && args.ends_with(&suffix))
                .then(|| pid.parse().ok())?
        })
        .collect()
}

/// Whether `pid` names a live process.
pub fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Poll `cond` until it holds or [`WAIT`] passes; returns its last value.
pub fn wait_until(cond: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

/// The environment every test host resolves spawns against: `/bin/bash` is
/// the user's shell, `PATH` is minimal, and nothing is shipped.
pub fn host_environment() -> HostEnvironment {
    HostEnvironment::with_vars(
        None,
        None,
        [("SHELL", "/bin/bash"), ("PATH", "/usr/bin:/bin")]
            .map(|(key, value)| (key.to_string(), value.to_string())),
    )
}

/// `script`, run by the user's shell of whichever host resolves it — `/bin/bash`
/// on a test host — with an empty home, so no profile of the machine's user
/// runs there.
pub fn sh(script: &str) -> SpawnIntent {
    SpawnIntent {
        remote_launch: Default::default(),
        command: Some(script.to_string()),
        argv: None,
        shell: None,
        cwd: String::new(),
        cols: 80,
        rows: 24,
        shell_integration: false,
        starship_preset: None,
        api_port: None,
        env_remove: Vec::new(),
        env: vec![("HOME".to_string(), "/var/empty".to_string())],
        agent_env: Vec::new(),
        login_path: None,
    }
}

/// [`sh`] resolved for session `id` on a test host, for a host that spawns
/// it itself.
pub fn sh_spec(id: &str, script: &str) -> PtySpec {
    sh(script).resolve(id, &host_environment())
}

/// A registration for `id`, optionally a story session (persisting) or an
/// unattached warm-pool member.
pub fn registration(id: &str, story: Option<&str>, pool: bool) -> SessionRegistration {
    SessionRegistration {
        session_id: id.to_string(),
        meta: SessionMeta {
            story_id: story.map(str::to_string),
            ..SessionMeta::default()
        },
        home_cwd: None,
        osc_enabled: false,
        is_pool_member: pool,
        scrollback: codesense_pty_core::emulator::DEFAULT_SCROLLBACK,
    }
}

/// An event sink that records everything it is handed.
#[derive(Default)]
pub struct RecordingSink(Mutex<Vec<TerminalDomainEvent>>);

impl TerminalEventSink for RecordingSink {
    fn emit(&self, event: TerminalDomainEvent) -> Result<(), PortUnavailable> {
        lock(&self.0).push(event);
        Ok(())
    }
}

impl RecordingSink {
    pub fn events(&self) -> Vec<TerminalDomainEvent> {
        lock(&self.0).clone()
    }

    /// Everything delivered on `session`'s output channel, in order.
    pub fn output(&self, session: &str) -> String {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                TerminalDomainEvent::Output {
                    session: s, data, ..
                } if s.as_str() == session => Some(data),
                _ => None,
            })
            .collect()
    }

    /// `session`'s reported exit, once it has exited.
    pub fn exit(&self, session: &str) -> Option<Option<ExitInfo>> {
        self.events().into_iter().find_map(|event| match event {
            TerminalDomainEvent::Exit {
                session: s, status, ..
            } if s.as_str() == session => Some(status),
            _ => None,
        })
    }
}

/// A real sidecar serving an instance root in-process. Kills the sessions it
/// was told about when dropped.
pub struct TestServer {
    root: PathBuf,
    _owned_root: Option<tempfile::TempDir>,
    host: Arc<Host>,
    sessions: Mutex<Vec<String>>,
    /// A stoppable server's binding (its lock), released by [`Self::stop`].
    stoppable: Mutex<Option<crate::server::Bound>>,
    _runtime: tokio::runtime::Runtime,
}

impl TestServer {
    fn prepare_file_limit() {
        static STARTUP: std::sync::Once = std::sync::Once::new();
        STARTUP.call_once(|| {
            codesense_pty_core::nofile::raise(10_240);
        });
    }

    /// Serve a fresh temporary instance root.
    pub fn start() -> Self {
        Self::prepare_file_limit();
        let owned = short_root();
        let mut server = Self::start_at(owned.path());
        server._owned_root = Some(owned);
        server
    }

    /// Serve the instance rooted at `root`.
    pub fn start_at(root: &Path) -> Self {
        Self::start_at_with_environment(root, host_environment())
    }

    /// Serve with isolated spawn resources, without changing process variables.
    pub(crate) fn start_at_with_environment(root: &Path, environment: HostEnvironment) -> Self {
        Self::prepare_file_limit();
        let bound = bind(root).expect("bind the test sidecar");
        let runtime = tokio::runtime::Runtime::new().expect("test sidecar runtime");
        let host = Host::new(
            bound.instance_id.clone(),
            ServerIdentity::ephemeral("test"),
            environment,
            runtime.handle().clone(),
        );
        host.terminal().attach_echo_emulator();
        let serving = host.clone();
        // The whole binding moves in — its singleton lock too — so the test
        // sidecar holds the instance's lock for as long as it serves, as a
        // real one does.
        let served_root = root.to_path_buf();
        std::thread::spawn(move || {
            let bound = bound;
            serve(&bound.listener, serving, &served_root)
        });
        Self {
            root: root.to_path_buf(),
            _owned_root: None,
            host,
            sessions: Mutex::new(Vec::new()),
            stoppable: Mutex::new(None),
            _runtime: runtime,
        }
    }

    /// Serve the instance rooted at `root` until [`Self::stop`] ends it as a
    /// real server is ended.
    pub fn start_stoppable_at(root: &Path) -> Self {
        Self::prepare_file_limit();
        let bound = bind(root).expect("bind the test sidecar");
        let listener = bound.listener.try_clone().expect("listener");
        let runtime = tokio::runtime::Runtime::new().expect("test sidecar runtime");
        let host = Host::new(
            bound.instance_id.clone(),
            ServerIdentity::ephemeral("test"),
            host_environment(),
            runtime.handle().clone(),
        );
        host.terminal().attach_echo_emulator();
        let serving = host.clone();
        let served_root = root.to_path_buf();
        std::thread::spawn(move || serve(&listener, serving, &served_root));
        Self {
            root: root.to_path_buf(),
            _owned_root: None,
            host,
            sessions: Mutex::new(Vec::new()),
            stoppable: Mutex::new(Some(bound)),
            _runtime: runtime,
        }
    }

    /// Open `session` running `script` as the app process `client` would —
    /// its hello naming that client — and close that app's connection: an
    /// app that opened a session and is gone. The child's pid.
    pub fn spawn_as_gone_app(&self, client: &str, session: &str, script: &str) -> u32 {
        let socket = codesense_pty_core::protocol::socket_path(&self.root);
        let mut stream = std::os::unix::net::UnixStream::connect(socket).expect("connect");
        let mut reader = stream.try_clone().expect("reader");
        let mut hello = codesense_pty_core::protocol::client_hello(
            codesense_pty_core::protocol::instance_id(&self.root),
            "test",
            &codesense_pty_core::domain::ViewerId::new("app"),
        );
        hello.client_id = Some(client.to_string());
        codesense_host_proto::greet(&mut reader, &mut stream, &hello).expect("greeted");
        self.track(session);
        let reply = codesense_pty_core::protocol::call_once(
            &mut reader,
            &mut stream,
            1,
            codesense_pty_core::protocol::Request::Spawn {
                intent: sh(script),
                registration: registration(session, None, false),
            },
        )
        .expect("answered")
        .expect("spawned");
        let codesense_pty_core::protocol::Reply::Spawned { pid: Some(pid) } = reply else {
            panic!("no child pid: {reply:?}")
        };
        pid
    }

    /// End a [`Self::start_stoppable_at`] server as terminating its process
    /// would: every session ends, its socket goes, its clients' connections
    /// close and its lock is released.
    pub fn stop(&self) {
        // A drain waits out its whole grace before it kills: a short one.
        let _ = self
            .host
            .terminal()
            .drain_for_replacement(Instant::now() + Duration::from_millis(200));
        let _ = std::fs::remove_file(codesense_pty_core::protocol::socket_path(&self.root));
        self.host.close_connections();
        lock(&self.stoppable).take();
    }

    /// Serve `SessionWakeV1` too, its store under this root.
    pub fn enable_wake(&self) {
        self.host.start_wake(&self.root).unwrap();
    }

    /// Serve `RoutineArmV1` too (after [`enable_wake`](Self::enable_wake)).
    pub fn enable_arms(&self) {
        self.host
            .start_arms_on(&self.root, Arc::new(crate::session_wake::SystemClock), None)
            .unwrap();
    }

    /// Serve `RoutineArmV1` on `clock` (after [`enable_wake`](Self::enable_wake)).
    #[cfg(test)]
    pub(crate) fn enable_arms_with(&self, clock: Arc<dyn crate::session_wake::Clock>) {
        self.host.start_arms_with(&self.root, clock).unwrap();
    }

    #[cfg(all(target_os = "linux", test))]
    pub(crate) fn enable_reverse_mcp(&self) {
        self.host.start_reverse_mcp(&self.root).unwrap();
    }

    /// The instance root served.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The hosted registry.
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// Kill `session` when the server is dropped.
    pub fn track(&self, session: &str) {
        lock(&self.sessions).push(session.to_string());
    }

    /// The session's terminal as text (its lines joined by `\n`), after all
    /// output delivered so far.
    pub fn text(&self, session: &str) -> String {
        self.host
            .terminal()
            .read_terminal_text(
                &PtySessionId::new(session),
                u32::MAX,
                codesense_pty_core::emulator::TextMode::Normal,
            )
            .map(|text| text.text)
            .unwrap_or_default()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        for id in lock(&self.sessions).iter() {
            let _ = self.host.terminal().kill(&PtySessionId::new(id.as_str()));
        }
    }
}

/// The remote workspace diff, run directly on a worktree directory — so the
/// app's tests can hold the remote answer against its local libgit2 diff on the
/// same repository. `base_commit` stands in for the preparation commit.
pub fn worktree_diff_changes(
    worktree: &Path,
    base_commit: Option<&str>,
    base_ref: Option<&str>,
) -> Result<codesense_pty_core::remote_files::FileReply, String> {
    let root = std::fs::File::open(worktree).map_err(|e| e.to_string())?;
    crate::remote_workspace::diff::changes(&root, base_commit, base_ref, &|| true)
}

/// The remote workspace redline, run directly on a worktree directory.
pub fn worktree_diff_redline(
    worktree: &Path,
    path: &str,
    old_path: Option<&str>,
    base_ref: Option<&str>,
) -> Result<codesense_pty_core::remote_files::FileReply, String> {
    let root = std::fs::File::open(worktree).map_err(|e| e.to_string())?;
    crate::remote_workspace::diff::redline(&root, None, path, old_path, base_ref, &|| true)
}
