// SPDX-License-Identifier: MIT
//! Task-owned native WebSocket executors behind admitted bounded byte channels.
use super::*;
use crate::host::Conn;
use codesense_host_proto::{executor as wire, Frame, Kind};
use codesense_pty_core::agent_cli::{remote_executor, VERSION_OUTPUT_MAX};
use codesense_pty_core::remote_executor::{ExecutorStarted, StartExecutor};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
struct Running {
    owner: u64,
    control: u32,
    endpoint: SocketAddr,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Process>>,
    token_file: std::path::PathBuf,
    streams: Arc<Mutex<HashMap<u32, Stream>>>,
}
struct Stream {
    socket: TcpStream,
    input: mpsc::SyncSender<Vec<u8>>,
}
struct Process {
    child: Option<std::process::Child>,
    confirmed: bool,
}
#[derive(Default)]
pub(crate) struct Executors {
    entries: Mutex<Registry>,
}
#[derive(Default)]
struct Registry {
    live: HashMap<String, Running>,
}
impl Executors {
    pub(crate) fn start(
        &self,
        root: &Path,
        owner: u64,
        conn: Conn,
        request: StartExecutor,
        alive: impl Fn() -> bool,
    ) -> Result<ExecutorStarted, String> {
        uuid::Uuid::parse_str(&request.task_id).map_err(|_| "invalid executor task")?;
        if request.channel < 2 {
            return Err("invalid executor channel".into());
        }
        let (_, _, receipt) = resolve_completed_target(root, owner, &request.target, &alive)?;
        let cwd = receipt.intent.worktree_path;
        let directory = open_directory(&cwd, false)?;
        owner_directory(&directory)?;
        let home = std::env::var_os("HOME").ok_or("executor account home unavailable")?;
        let executor = remote_executor();
        if let Some(check) = executor.machine_check {
            check.verify()?;
        }
        let preferred = std::path::PathBuf::from(&home)
            .join(".local/bin")
            .join(&executor.binary);
        let binary = if preferred.is_file() {
            preferred
        } else {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|p| p.join(&executor.binary))
                .find(|p| p.is_absolute() && p.is_file())
                .ok_or_else(|| {
                    format!(
                        "capabilityUnavailable: install {} on the remote machine",
                        executor.release.supported
                    )
                })?
        };
        verify_version(&binary)?;
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "executor registry unavailable")?;
        if entries.live.len() >= wire::MAX_EXECUTORS
            || entries.live.contains_key(&request.task_id)
            || entries
                .live
                .values()
                .any(|e| e.owner == conn.id && e.control == request.channel)
        {
            return Err("executor already reserved or capacity reached".into());
        }
        reserve(root, &request.task_id)?;
        if !alive() {
            return Err("executor authority retired".into());
        }
        let token_file = root.join(format!("executor-{}.token", request.task_id));
        let auth_token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&token_file)
            .map_err(|_| "executor capability unavailable")?;
        file.write_all(auth_token.as_bytes())
            .map_err(|_| "executor capability unavailable")?;
        // The private root is a daemon-only /proc/self/fd path. Pass the pinned
        // token descriptor, not that pathname, across exec into the native server.
        let native_token_path = format!("/proc/self/fd/{}", file.as_raw_fd());
        let mut command = Command::new(binary);
        command
            .args([
                "exec-server",
                "--listen",
                "ws://127.0.0.1:0",
                "--ws-auth",
                "capability-token",
                "--ws-token-file",
            ])
            .arg(native_token_path)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0);
        // SAFETY: the descriptor pins the validated worktree; fchdir is async-signal-safe.
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fchdir(directory.as_raw_fd()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().map_err(|_| "executor launch unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("executor readiness unavailable")?;
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = BufReader::new(stdout)
                .take(257)
                .read_until(b'\n', &mut bytes);
            let _ = ready_tx.send(result.map(|_| bytes));
        });
        let child = Arc::new(Mutex::new(Process {
            child: Some(child),
            confirmed: false,
        }));
        let endpoint = ready_rx
            .recv_timeout(Duration::from_secs(5))
            .ok()
            .and_then(Result::ok)
            .filter(|v| v.len() <= 256)
            .and_then(|v| String::from_utf8(v).ok())
            .and_then(|v| {
                v.trim()
                    .strip_prefix("ws://")
                    .and_then(|v| v.parse::<SocketAddr>().ok())
            })
            .filter(|v| v.ip() == std::net::Ipv4Addr::LOCALHOST && v.port() != 0);
        let Some(endpoint) = endpoint else {
            let _ = terminate(&child);
            let _ = std::fs::remove_file(token_file);
            return Err("executor readiness unavailable".into());
        };
        let stop = Arc::new(AtomicBool::new(false));
        let streams = Arc::new(Mutex::new(HashMap::<u32, Stream>::new()));
        entries.live.insert(
            request.task_id.clone(),
            Running {
                owner: conn.id,
                control: request.channel,
                endpoint,
                stop: stop.clone(),
                child: child.clone(),
                token_file,
                streams: streams.clone(),
            },
        );
        let channel = request.channel;
        std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) && !leader_exited(&child) {
                std::thread::sleep(Duration::from_millis(100));
            }
            stop.store(true, Ordering::Release);
            close_streams(&streams);
            let _ = terminate(&child);
            conn.send(Frame {
                kind: Kind::EXECUTOR_CLOSE,
                channel,
                payload: vec![],
            });
        });
        Ok(ExecutorStarted {
            task_id: request.task_id,
            channel: request.channel,
            cwd,
            auth_token,
        })
    }
    pub(crate) fn open(&self, conn: Conn, task_id: &str, channel: u32) -> Result<(), String> {
        let entries = self
            .entries
            .lock()
            .map_err(|_| "executor registry unavailable")?;
        let entry = entries
            .live
            .get(task_id)
            .filter(|e| e.owner == conn.id && !e.stop.load(Ordering::Acquire))
            .ok_or("executor authority unavailable")?;
        if channel < 2
            || entries.live.values().any(|e| {
                e.owner == conn.id
                    && (e.control == channel
                        || e.streams
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .contains_key(&channel))
            })
        {
            return Err("executor channel unavailable".into());
        }
        let mut streams = entry
            .streams
            .lock()
            .map_err(|_| "executor streams unavailable")?;
        if streams.len() >= wire::MAX_CONNECTIONS {
            return Err("executor connection limit reached".into());
        }
        let socket = TcpStream::connect_timeout(&entry.endpoint, Duration::from_secs(2))
            .map_err(|_| "executor connection unavailable")?;
        socket
            .set_read_timeout(Some(Duration::from_millis(200)))
            .map_err(|_| "executor socket unavailable")?;
        socket
            .set_write_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| "executor socket unavailable")?;
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(wire::QUEUE_CHUNKS);
        let mut reader = socket
            .try_clone()
            .map_err(|_| "executor socket unavailable")?;
        let mut writer = socket
            .try_clone()
            .map_err(|_| "executor socket unavailable")?;
        streams.insert(
            channel,
            Stream {
                socket,
                input: sender,
            },
        );
        let write_stop = entry.stop.clone();
        let peer_stop = Arc::new(AtomicBool::new(false));
        let peer_read = peer_stop.clone();
        let bound = entry.streams.clone();
        std::thread::spawn(move || {
            while !write_stop.load(Ordering::Acquire) && !peer_stop.load(Ordering::Acquire) {
                match receiver.recv_timeout(Duration::from_millis(100)) {
                    Ok(bytes) => {
                        if writer.write_all(&bytes).is_err() {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => break,
                }
            }
            let _ = writer.shutdown(Shutdown::Both);
        });
        let task_stop = entry.stop.clone();
        std::thread::spawn(move || {
            let mut bytes = vec![0; wire::CHUNK_BYTES];
            while !task_stop.load(Ordering::Acquire) {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) => {
                        if !conn.send(Frame {
                            kind: Kind::EXECUTOR_DATA,
                            channel,
                            payload: bytes[..n].into(),
                        }) {
                            break;
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        continue
                    }
                    Err(_) => break,
                }
            }
            peer_read.store(true, Ordering::Release);
            bound
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&channel);
            conn.send(Frame {
                kind: Kind::EXECUTOR_CLOSE,
                channel,
                payload: vec![],
            });
        });
        Ok(())
    }
    pub(crate) fn receive(&self, conn: &Conn, frame: Frame) {
        let entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        for entry in entries.live.values().filter(|e| e.owner == conn.id) {
            let mut streams = entry.streams.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(stream) = streams.get(&frame.channel) {
                if frame.kind != Kind::EXECUTOR_DATA
                    || frame.payload.len() > wire::CHUNK_BYTES
                    || stream.input.try_send(frame.payload).is_err()
                {
                    if let Some(stream) = streams.remove(&frame.channel) {
                        let _ = stream.socket.shutdown(Shutdown::Both);
                    }
                }
                return;
            }
        }
    }
    pub(crate) fn stop(&self, root: Option<&Path>, owner: u64, id: &str) -> Result<(), String> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "executor registry unavailable")?;
        uuid::Uuid::parse_str(id).map_err(|_| "invalid executor task")?;
        if let Some(entry) = entries.live.get(id) {
            if entry.owner != owner {
                return Err("executor owner mismatch".into());
            }
            entry.stop.store(true, Ordering::Release);
            close_streams(&entry.streams);
            let result = terminate(&entry.child);
            let _ = std::fs::remove_file(&entry.token_file);
            entries.live.remove(id);
            return result;
        } else if let Some(root) = root {
            reserve(root, id)?
        }
        Ok(())
    }
    pub(crate) fn disconnect(&self, owner: u64) {
        let ids = self
            .entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .live
            .iter()
            .filter(|(_, e)| e.owner == owner)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            let _ = self.stop(None, owner, &id);
        }
    }
}
fn close_streams(streams: &Mutex<HashMap<u32, Stream>>) {
    for (_, stream) in streams.lock().unwrap_or_else(|e| e.into_inner()).drain() {
        let _ = stream.socket.shutdown(Shutdown::Both);
    }
}
// Observe without reaping, so the owned PGID cannot be recycled before cleanup.
fn leader_exited(child: &Mutex<Process>) -> bool {
    let guard = child.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .child
        .as_ref()
        .is_none_or(|child| exited_without_reaping(child.id()))
}
fn exited_without_reaping(pid: u32) -> bool {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            pid,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    result != 0 || unsafe { info.si_pid() } != 0
}
fn terminate(process: &Mutex<Process>) -> Result<(), String> {
    let mut process = process.lock().map_err(|_| "executor process unavailable")?;
    if let Some(child) = process.child.as_mut() {
        // The official runtime handles SIGTERM and retires native command groups.
        // A forced kill cannot provide that cleanup receipt.
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while !exited_without_reaping(child.id()) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let graceful = exited_without_reaping(child.id());
        let killed = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
        if killed != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            return Err("executor group stop unknown".into());
        }
        let status = child.wait().map_err(|_| "executor stop unknown")?;
        process.confirmed = graceful && status.success();
        process.child.take();
    }
    if process.confirmed {
        Ok(())
    } else {
        Err("executor forced stop; native child cleanup outcome unknown".into())
    }
}

/// Refuses a machine Codex outside its qualified release, naming the release it needs,
/// before the executor reserves or starts anything.
fn verify_version(binary: &Path) -> Result<(), String> {
    let release = &remote_executor().release;
    let unavailable =
        |what: &str| format!("{}; executor version {what}", release.remote_task_refusal());
    let mut child = Command::new(binary)
        .arg("--version")
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|_| unavailable("unavailable"))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| unavailable("unavailable"))?;
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = output
            .take(VERSION_OUTPUT_MAX as u64 + 1)
            .read_to_end(&mut bytes);
        let _ = tx.send(result.map(|_| bytes));
    });
    let bytes = rx.recv_timeout(Duration::from_secs(3));
    // No try_wait before kill: preserve ownership of the process group until retirement.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.wait();
    let bytes = bytes
        .map_err(|_| unavailable("timed out"))?
        .map_err(|_| unavailable("unavailable"))?;
    if !release.output_qualified(&bytes) {
        return Err(release.remote_task_refusal());
    }
    Ok(())
}

fn reserve(root: &Path, id: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(format!("executor-{id}.reserved")))
    {
        Ok(file) => super::durable::sync(&file).map_err(|_| "executor reservation unavailable")?,
        Err(_) => return Err("executor task previously submitted; outcome unknown".into()),
    }
    File::open(root)
        .and_then(|root| super::durable::sync(&root))
        .map_err(|_| "executor reservation directory unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancel_before_start_survives_registry_restart() {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let executors = Executors::default();
        executors.stop(Some(root.path()), 1, &id).unwrap();
        drop(executors);
        assert!(reserve(root.path(), &id).is_err());
        assert!(Executors::default()
            .stop(Some(root.path()), 1, &id)
            .is_err());
    }
    #[test]
    fn reservation_never_reopens_after_submission() {
        let root = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        reserve(root.path(), &id).unwrap();
        assert!(reserve(root.path(), &id).is_err());
    }
    #[test]
    fn version_check_admits_the_qualified_range_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let codex = |name: &str, answer: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{answer}'\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        verify_version(&codex("current", "codex-cli 0.162.1")).unwrap();
        let refused = verify_version(&codex("pinned", "codex-cli 0.158.0")).unwrap_err();
        let needed = remote_executor().release.supported;
        assert!(refused.contains(needed), "{refused}");
        let long = format!("codex-cli 0.162.1{}", " ".repeat(VERSION_OUTPUT_MAX));
        assert!(verify_version(&codex("long", &long)).is_err());
    }
}
