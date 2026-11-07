// SPDX-License-Identifier: MIT
//! One verified gateway teardown for host engine loss and desktop home removal.
use crate::activity::peer;
use std::os::fd::AsRawFd;
#[cfg(test)]
use std::os::unix::process::CommandExt;
use std::{
    fs, io,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

fn owns_lock(pid: u32, home: &Path) -> bool {
    let path = home.join("gateway.lock");
    let Ok(bytes) = fs::read(&path) else {
        return false;
    };
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    if record.get("pid").unwrap_or(&record).as_u64() != Some(pid as u64) {
        return false;
    }
    let Ok(lock) = fs::OpenOptions::new().read(true).write(true).open(&path) else {
        return false;
    };
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return false;
    }
    if io::Error::last_os_error().raw_os_error() != Some(libc::EWOULDBLOCK) {
        return false;
    }
    // Darwin lsof does not expose flock's owner. Require all three facts: the live
    // gateway's lock record, a held kernel lock and that process's open file.
    let Ok(output) = Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid.to_string(), "-Fpn"])
        .arg(&path)
        .output()
    else {
        return false;
    };
    let Ok(canonical) = fs::canonicalize(path) else {
        return false;
    };
    output.status.success()
        && String::from_utf8_lossy(&output.stdout).lines().any(|line| {
            line.strip_prefix('n')
                .is_some_and(|name| Path::new(name) == canonical)
        })
}

fn verified_gateway(home: &Path) -> Option<peer::PeerProcess> {
    let bytes = Result::ok(fs::read(home.join("gateway.pid")))?;
    let record: serde_json::Value = Result::ok(serde_json::from_slice(&bytes))?;
    let pid = record
        .get("pid")
        .unwrap_or(&record)
        .as_u64()
        .and_then(|n| Result::ok(u32::try_from(n)))?;
    let identity = peer::inspect(pid)?;
    let created = Result::ok(Result::ok(fs::metadata(home))?.created())?;
    if !identity.started_after(created) {
        return None;
    }
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let id = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[id]),
        true,
        ProcessRefreshKind::new().with_cmd(UpdateKind::Always),
    );
    let process = system.process(id)?;
    let args: Vec<_> = process.cmd().iter().map(|s| s.to_string_lossy()).collect();
    if !args.iter().any(|arg| arg == "hermes_cli.main")
        || !args.windows(2).any(|args| args == ["gateway", "run"])
        || unsafe { libc::getpgid(pid as i32) } != pid as i32
        || !owns_lock(pid, home)
        || !peer::runs(identity)
    {
        return None;
    }
    Some(identity)
}

pub(crate) fn teardown_home(home: &Path, wait_for_replacement: bool) {
    // A detached recovery may still be importing/bootstraping when the CLI tree ends.
    // Only engine-loss cleanup waits for its verified lock/pid publication. Normal
    // desktop teardown remains one-shot. Discovery and stop share the original budget.
    let deadline = Instant::now() + Duration::from_secs(3);
    let process = loop {
        if let Some(process) = verified_gateway(home) {
            break process;
        }
        if !wait_for_replacement || Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // The gateway owns this process group; never signal the caller's group or a recycled PID.
    unsafe {
        libc::killpg(process.pid() as i32, libc::SIGTERM);
    }
    while peer::runs(process) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if peer::runs(process) {
        unsafe {
            libc::killpg(process.pid() as i32, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod gateway_tests {
    use super::*;
    use std::io::BufRead;
    struct Gateway(std::process::Child);
    impl Gateway {
        fn spawn(home: &Path, owns_lock: bool) -> Self {
            let code = r#"import fcntl,json,os,pathlib,signal,sys,time
h=pathlib.Path(sys.argv[1]); f=open(h/'gateway.lock','w+')
json.dump({'pid':os.getpid()},f); f.flush()
if sys.argv[2]=='lock': fcntl.flock(f,fcntl.LOCK_EX)
(h/'gateway.pid').write_text(json.dumps({'pid':os.getpid()}))
def stop(*args):
 (h/'stopped').write_text('checkpointed'); sys.exit(0)
signal.signal(signal.SIGTERM,stop)
print('ready',flush=True)
while True: time.sleep(.05)
"#;
            let mut child = Command::new("python3")
                .args(["-c", code])
                .arg(home)
                .arg(if owns_lock { "lock" } else { "open" })
                .args(["hermes_cli.main", "gateway", "run"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .unwrap();
            let mut ready = String::new();
            std::io::BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut ready)
                .unwrap();
            assert_eq!(ready.trim(), "ready");
            Self(child)
        }
    }
    impl Drop for Gateway {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    #[test]
    fn teardown_stops_only_a_home_born_gateway_holding_its_own_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let mut gateway = Gateway::spawn(tmp.path(), true);
        assert!(
            verified_gateway(tmp.path()).is_some(),
            "real process, lock and creation verification"
        );
        teardown_home(tmp.path(), false);
        assert!(
            tmp.path().join("stopped").exists(),
            "TERM allows final checkpoint before returning"
        );
        assert!(gateway.0.wait().unwrap().success());
        teardown_home(tmp.path(), false);
        let mut unheld = Gateway::spawn(tmp.path(), false);
        teardown_home(tmp.path(), false);
        assert!(
            unheld.0.try_wait().unwrap().is_none(),
            "a matching command with an unheld lock is not signaled"
        );
        let _ = unheld.0.kill();
        unheld.0.wait().unwrap();
    }

    #[test]
    fn engine_loss_teardown_waits_for_verified_replacement_publication() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_owned();
        let (ready, started) = std::sync::mpsc::channel();
        let replacement = std::thread::spawn(move || {
            // Recovery can detach before its bootstrap publishes the lock and PID.
            std::thread::sleep(Duration::from_millis(50));
            let mut gateway = Gateway::spawn(&home, true);
            ready.send(()).unwrap();
            assert!(gateway.0.wait().unwrap().success());
        });
        teardown_home(tmp.path(), true);
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let stopped = tmp.path().join("stopped").exists();
        // Keep the negative control clean too: join only after stopping the test's
        // verified child, even when the missing discovery wait made the assertion red.
        if !stopped {
            teardown_home(tmp.path(), false);
        }
        replacement.join().unwrap();
        assert!(stopped, "replacement was swept after verification");
    }
}

#[cfg(test)]
use std::process::Stdio;
