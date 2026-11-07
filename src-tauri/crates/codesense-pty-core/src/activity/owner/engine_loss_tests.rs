// SPDX-License-Identifier: MIT
use super::*;
use std::time::Duration;

// Exercise the real keeper, ingress peer verification and captured PTY child handle.
// The sink barrier lets the CLI's detached recovery race with loss handling predictably.
#[track_caller]
fn loss_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "engine-loss fixture stalled");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn loss_order(home: &std::path::Path, step: &str) {
    use std::io::Write;
    writeln!(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(home.join("order"))
            .unwrap(),
        "{step}"
    )
    .unwrap();
}

struct LossSink(PathBuf);
impl crate::domain::TerminalEventSink for LossSink {
    fn emit(
        &self,
        event: crate::domain::TerminalDomainEvent,
    ) -> Result<(), crate::domain::PortUnavailable> {
        if let crate::domain::TerminalDomainEvent::Activity { snapshot } = event {
            if snapshot.state == State::Crashed && !snapshot.observed {
                if snapshot.exit.is_none() {
                    assert!(!snapshot.observed);
                    loss_order(&self.0, "readiness-cleared");
                    loss_until(|| self.0.join("replacement-ready").exists());
                } else {
                    assert!(super::super::end_reason(&snapshot)
                        .unwrap()
                        .contains("Relaunch to continue"));
                    loss_order(&self.0, "visible-end-cause");
                }
            }
        }
        Ok(())
    }
}

fn loss_sweep(home: &std::path::Path) {
    let cli: u32 = std::fs::read_to_string(home.join("cli.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        crate::activity::peer::inspect(cli).is_none(),
        "CLI must end before sweep"
    );
    loss_order(home, "cli-ended");
    if !home.join("delay-publication").exists() {
        loss_until(|| home.join("allow-sweep").exists());
    }
    crate::agent_cli::adapter(AgentCli::Hermes).teardown_home(home, true);
    loss_order(home, "replacement-swept");
}

struct LossCleanup<'a>(&'a crate::runner::PtySession, &'a std::path::Path);
impl Drop for LossCleanup<'_> {
    fn drop(&mut self) {
        self.0.kill();
        self.0.child().wait();
        crate::agent_cli::adapter(AgentCli::Hermes).teardown_home(self.1, false);
    }
}

#[test]
fn real_keeper_loss_clears_readiness_ends_tree_sweeps_only_verified_recovery_and_retains_cause() {
    run_keeper_loss(false);
}

#[test]
fn fallback_loss_keeps_other_sessions_ingress_within_budget_and_coalesces_cleanup() {
    run_keeper_loss(true);
}

fn run_keeper_loss(fallback: bool) {
    use crate::agent_cli::orion;
    use crate::runner::{LocalPtyRunner, PtyRunner, PtySpec};
    use std::os::unix::fs::PermissionsExt;
    const ENGINE: &str = r#"
import os,pathlib,socket,sys,time
h=pathlib.Path(sys.argv[1]); s=socket.socket(socket.AF_UNIX); s.bind(str(h/'orion.sock')); s.listen()
(h/'engine.pid').write_text(str(os.getpid()))
while not (h/'stop-engine').exists(): time.sleep(.005)
"#;
    const CLI: &str = r#"
import json,os,pathlib,socket,struct,subprocess,sys,time,uuid
h=pathlib.Path(sys.argv[1]); exe,raw,endpoint,activity,token,lock=sys.argv[2:]
(h/'cli.pid').write_text(str(os.getpid()))
env=dict(os.environ,CODESENSE_TEST_KEEPER_LAUNCH=raw,CODESENSE_TEST_KEEPER_ENDPOINT=endpoint,ORION_HOME=str(h),CODESENSE_ACTIVITY_SOCKET=activity,CODESENSE_ACTIVITY_TOKEN=token)
subprocess.Popen([exe,'--exact','activity::engine::tests::keeper_process_fixture','--nocapture'],env=env,stdout=subprocess.DEVNULL)
while not (h/'engine.pid').exists(): time.sleep(.005)
pid=int((h/'engine.pid').read_text())
def send(**operation):
 s=socket.socket(socket.AF_UNIX); s.connect(activity)
 raw=json.dumps(dict(token=token,deadline_ns=time.clock_gettime_ns(time.CLOCK_MONOTONIC)+250000000,**operation)).encode()
 s.sendall(struct.pack('>I',len(raw))+raw); assert s.recv(1)==b'A'; s.close()
send(engine=dict(home=str(h),pid=pid,endpoint=endpoint))
send(report=dict(provider='codex',event='started',scope='main',reportId=str(uuid.uuid4()),providerSession='orion-main',turn=None,tool=None))
(h/'cli-ready').touch()
while True:
 try: os.kill(pid,0)
 except ProcessLookupError: break
 time.sleep(.005)
replacement='''import fcntl,json,os,pathlib,signal,sys,time
h=pathlib.Path(sys.argv[1])
if (h/'delay-publication').exists():
 (h/'replacement-ready').touch()
 while not (h/'allow-sweep').exists(): time.sleep(.005)
f=open(h/'gateway.lock','w+')
json.dump({'pid':os.getpid()},f); f.flush()
if sys.argv[2]=='lock': fcntl.flock(f,fcntl.LOCK_EX)
(h/'gateway.pid').write_text(json.dumps({'pid':os.getpid()}))
signal.signal(signal.SIGTERM,lambda *args: sys.exit(0))
(h/'replacement-ready').touch()
while True: time.sleep(.05)
'''
subprocess.Popen([sys.executable,'-c',replacement,str(h),lock,'hermes_cli.main','gateway','run'],start_new_session=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
while True: time.sleep(.05)
"#;
    // The early exit races the sweep; the late exit arrives after the final cause.
    for (verified, early_exit) in [(true, true), (false, false)] {
        let home = tempfile::tempdir_in("/tmp").unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        if fallback {
            // Hold the REAL verifier's discovery loop: the detached replacement is
            // running, but cannot publish its lock/PID until the healthy hook returns.
            std::fs::write(home.path().join("delay-publication"), []).unwrap();
        }
        let sink = Arc::new(LossSink(home.path().into()));
        let mut owner = ActivityOwner::with_adapters(orion::adapters);
        owner.sink = Some(sink);
        let owner = Arc::new(owner);
        let _service = crate::activity::ActivityService::start(home.path(), &owner).unwrap();
        let socket = home.path().join("activity.sock");
        let writer = owner
            .prepare_native("loss", orion::ORION, socket.to_str().unwrap())
            .unwrap();
        let endpoint = home.path().join("engine-loss.sock");
        orion::serve(home.path()).teardown_with(loss_sweep);
        let launch = crate::activity::engine::EngineLaunch {
            provider: orion::ORION,
            serves_at: true,
            // Force the actual listener's decay fallback when the keeper cannot notify.
            end_on_loss: !fallback,
            program: "/usr/bin/python3".into(),
            engine: vec![
                "-c".into(),
                ENGINE.into(),
                home.path().to_str().unwrap().into(),
            ],
            standalone: vec!["--serves-at=unix".into()],
            attached: vec![],
            operands: vec![],
        };
        let mut command = portable_pty::CommandBuilder::new("/usr/bin/python3");
        command.args([
            "-c",
            CLI,
            home.path().to_str().unwrap(),
            std::env::current_exe().unwrap().to_str().unwrap(),
            &serde_json::to_string(&launch).unwrap(),
            endpoint.to_str().unwrap(),
            socket.to_str().unwrap(),
            &writer.token,
            if verified { "lock" } else { "open" },
        ]);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let pty = rt
            .block_on(LocalPtyRunner.spawn(PtySpec {
                command,
                size: portable_pty::PtySize::default(),
            }))
            .unwrap();
        let incarnation = owner.register("loss", pty.pid()).unwrap();
        let cleanup = LossCleanup(&pty, home.path());
        owner.attach_child("loss", &incarnation, pty.child());
        loss_until(|| home.path().join("cli-ready").exists());
        assert!(owner.snapshot("loss").unwrap().observed);
        std::fs::write(home.path().join("stop-engine"), []).unwrap();
        loss_until(|| {
            home.path().join("order").exists()
                && std::fs::read_to_string(home.path().join("order"))
                    .unwrap()
                    .contains("cli-ended")
        });
        // A second sweep and repeated loss for this invocation cannot start cleanup twice.
        assert!(owner.end_failed_engine(&writer.token, None).is_err());
        owner.decay(Instant::now());
        let healthy = fallback.then(|| report_while_loss_pending(&owner, &socket));
        let exit = Exit {
            code: None,
            signal: Some("SIGKILL".into()),
            requested: false,
        };
        if early_exit {
            owner.exit("loss", &incarnation, exit.clone());
        }
        assert!(
            super::super::end_reason(&owner.snapshot("loss").unwrap()).is_none(),
            "snapshot reads must not expose the cause before cleanup completes"
        );
        assert!(!std::fs::read_to_string(home.path().join("order"))
            .unwrap()
            .contains("visible-end-cause"));
        std::fs::write(home.path().join("allow-sweep"), []).unwrap();
        loss_until(|| {
            std::fs::read_to_string(home.path().join("order"))
                .unwrap()
                .contains("visible-end-cause")
        });
        if !early_exit {
            owner.exit("loss", &incarnation, exit);
        }
        assert_eq!(
            owner.snapshot("loss").unwrap().reason.as_deref(),
            Some(super::super::ENGINE_STOPPED)
        );
        assert_eq!(owner.snapshot("loss").unwrap().state, State::Crashed);
        assert_eq!(
            std::fs::read_to_string(home.path().join("order"))
                .unwrap()
                .lines()
                .take(4)
                .collect::<Vec<_>>(),
            [
                "readiness-cleared",
                "cli-ended",
                "replacement-swept",
                "visible-end-cause"
            ]
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join("order"))
                .unwrap()
                .lines()
                .filter(|step| *step == "replacement-swept")
                .count(),
            1,
            "a pending invocation has exactly one cleanup worker"
        );
        let pid: u32 = serde_json::from_slice::<serde_json::Value>(
            &std::fs::read(home.path().join("gateway.pid")).unwrap(),
        )
        .unwrap()["pid"]
            .as_u64()
            .unwrap() as u32;
        assert_eq!(
            crate::activity::peer::inspect(pid).is_none(),
            verified,
            "unverified replacement must never be killed"
        );
        if !verified {
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
        drop(cleanup);
        pty.discard();
        if let Some(result) = healthy {
            result.expect("another session's real hook must be accepted within its 250 ms budget");
            assert!(owner.snapshot("healthy").unwrap().observed);
        }
    }
}

// Use the same real socket, kernel peer and wire deadline as an ordinary activity hook.
fn report_while_loss_pending(
    owner: &ActivityOwner,
    socket: &std::path::Path,
) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let writer = owner
        .prepare_native(
            "healthy",
            crate::agent_cli::orion::ORION,
            socket.to_str().unwrap(),
        )
        .unwrap();
    owner.register("healthy", Some(std::process::id())).unwrap();
    let started = Instant::now();
    let mut clock: libc::timespec = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut clock) },
        0
    );
    let deadline_ns = clock.tv_sec as u64 * 1_000_000_000 + clock.tv_nsec as u64 + 250_000_000;
    let bytes = serde_json::to_vec(&serde_json::json!({
        "token":writer.token,"deadline_ns":deadline_ns,
        "report":{"provider":"codex","event":"started","scope":"main",
            "reportId":"healthy-hook","providerSession":"healthy-session",
            "turn":null,"tool":null}
    }))?;
    let mut stream = std::os::unix::net::UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    stream.set_write_timeout(Some(Duration::from_millis(250)))?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    let mut ack = [0];
    stream.read_exact(&mut ack)?;
    assert_eq!(ack, *b"A");
    assert!(started.elapsed() < Duration::from_millis(250));
    Ok(())
}
