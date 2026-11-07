// SPDX-License-Identifier: MIT
//! Fixed, stdin-authorized replacement of one proved Linux terminal server.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_AUTH_BYTES: u64 = 8 * 1024;
const EXIT_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorization {
    pub uid: u32,
    pub pid: u32,
    pub process_start: u64,
    pub executable: PathBuf,
    pub executable_dev: u64,
    pub executable_ino: u64,
    pub root: PathBuf,
    pub manifest_digest: String,
    pub version: String,
    pub server_id: String,
    pub nonce: String,
    pub epoch: String,
}

impl Authorization {
    pub fn read_stdin() -> Result<Self, String> {
        let mut bytes = Vec::new();
        io::stdin()
            .take(MAX_AUTH_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("read replacement authorization: {e}"))?;
        if bytes.len() as u64 > MAX_AUTH_BYTES {
            return Err("replacement authorization is too large".into());
        }
        Self::parse(std::str::from_utf8(&bytes).map_err(|_| "authorization is not UTF-8")?)
    }

    fn parse(input: &str) -> Result<Self, String> {
        let fields = crate::replacement_data::authorization_fields(input)?;
        let take = |key: &str| {
            fields
                .get(key)
                .copied()
                .ok_or_else(|| format!("authorization omitted {key}"))
        };
        if take("schema")? != "codesense-replace-v2" || fields.len() != 13 {
            return Err("unsupported replacement authorization".into());
        }
        let component = |value: &str| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        };
        let version = take("version")?;
        let digest = take("manifest")?;
        if !component(version)
            || digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid installed-version identity".into());
        }
        Ok(Self {
            uid: take("uid")?.parse().map_err(|_| "invalid uid")?,
            pid: take("pid")?.parse().map_err(|_| "invalid pid")?,
            process_start: take("start")?
                .parse()
                .map_err(|_| "invalid process start")?,
            executable: PathBuf::from(take("executable")?),
            executable_dev: take("executable_dev")?
                .parse()
                .map_err(|_| "invalid device")?,
            executable_ino: take("executable_ino")?
                .parse()
                .map_err(|_| "invalid inode")?,
            root: PathBuf::from(take("root")?),
            manifest_digest: digest.to_ascii_lowercase(),
            version: version.into(),
            server_id: take("server_id")?.into(),
            nonce: take("nonce")?.into(),
            epoch: take("epoch")?.into(),
        })
    }
}

struct PidFd(i32);

impl Drop for PidFd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

impl PidFd {
    fn open(pid: u32) -> Result<Self, String> {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as i32;
        if fd < 0 {
            Err(format!("pidfd_open({pid}): {}", io::Error::last_os_error()))
        } else {
            Ok(Self(fd))
        }
    }

    fn wait(&self, timeout: Duration) -> Result<bool, String> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let mut descriptor = libc::pollfd {
                fd: self.0,
                events: libc::POLLIN,
                revents: 0,
            };
            let polled = unsafe { libc::poll(&mut descriptor, 1, 25) };
            if polled < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("poll pidfd: {error}"));
            }
            if polled > 0 {
                if descriptor.revents & libc::POLLNVAL != 0 {
                    return Err("poll pidfd returned POLLNVAL".into());
                }
                if descriptor.revents & libc::POLLIN != 0 {
                    return Ok(true);
                }
                if descriptor.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                    return Err(format!(
                        "poll pidfd returned unexpected events {:#x}",
                        descriptor.revents
                    ));
                }
            }
        }
        Ok(false)
    }
}

pub(crate) fn process_start(pid: u32) -> Result<u64, String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|e| format!("read process stat: {e}"))?;
    let after_name = stat
        .rsplit_once(") ")
        .map(|(_, fields)| fields)
        .ok_or_else(|| "malformed process stat".to_string())?;
    after_name
        .split_whitespace()
        .nth(19)
        .ok_or_else(|| "process stat omitted start time".to_string())?
        .parse()
        .map_err(|_| "invalid process start time".into())
}

fn manifest_digest(executable: &Path) -> Result<String, String> {
    let manifest = executable
        .parent()
        .ok_or_else(|| "installed executable has no parent".to_string())?
        .join(".codesense-manifest");
    let bytes = fs::read(&manifest).map_err(|e| format!("read {manifest:?}: {e}"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn manifest_authenticates_executable(executable: &Path) -> Result<bool, String> {
    let manifest = executable
        .parent()
        .ok_or("installed executable has no parent")?
        .join(".codesense-manifest");
    let manifest = fs::read_to_string(&manifest).map_err(|e| format!("read {manifest:?}: {e}"))?;
    let executable_hash = format!(
        "{:x}",
        Sha256::digest(fs::read(executable).map_err(|e| format!("read executable content: {e}"))?)
    );
    Ok(crate::replacement_data::manifest_matches(
        &manifest,
        &executable_hash,
        executable,
    ))
}

#[cfg(test)]
fn revalidate(auth: &Authorization) -> Result<(), String> {
    let logical = dirs::home_dir()
        .ok_or("no home directory")?
        .join(".codesense");
    revalidate_at(auth, &logical, &auth.root)
}

fn revalidate_at(
    auth: &Authorization,
    logical_root: &Path,
    filesystem_root: &Path,
) -> Result<(), String> {
    if auth.pid == 0 || auth.pid == std::process::id() {
        return Err("invalid replacement target".into());
    }
    if unsafe { libc::geteuid() } != auth.uid {
        return Err("replacement uid changed".into());
    }
    let process_owner = fs::metadata(format!("/proc/{}", auth.pid))
        .map_err(|e| format!("stat process owner: {e}"))?
        .uid();
    if process_owner != auth.uid {
        return Err("replacement process owner changed".into());
    }
    let executable = auth
        .executable
        .canonicalize()
        .map_err(|e| format!("canonical authorized executable: {e}"))?;
    let running = fs::canonicalize(format!("/proc/{}/exe", auth.pid))
        .map_err(|e| format!("canonical running executable: {e}"))?;
    if executable != running || process_start(auth.pid)? != auth.process_start {
        return Err("replacement process identity changed".into());
    }
    let metadata = fs::metadata(&executable).map_err(|e| format!("stat executable: {e}"))?;
    if metadata.dev() != auth.executable_dev || metadata.ino() != auth.executable_ino {
        return Err("replacement executable identity changed".into());
    }
    let root = auth
        .root
        .canonicalize()
        .map_err(|e| format!("canonical root: {e}"))?;
    let expected_root = logical_root
        .canonicalize()
        .map_err(|e| format!("canonical installed root: {e}"))?;
    if root != expected_root
        || executable
            != filesystem_root
                .join("host")
                .join(&auth.version)
                .join(crate::install::BINARY)
                .canonicalize()
                .map_err(|e| format!("canonical expected executable: {e}"))?
        || manifest_digest(&executable)? != auth.manifest_digest
        || !manifest_authenticates_executable(&executable)?
        || fs::read_to_string(filesystem_root.join("pty-server-id"))
            .map(|v| v.trim() != auth.server_id)
            .unwrap_or(true)
    {
        return Err("replacement installation identity changed".into());
    }
    Ok(())
}

fn cooperative_shutdown_at(
    auth: &Authorization,
    filesystem_root: &Path,
) -> Result<codesense_pty_core::protocol::Reply, String> {
    use codesense_host_proto::{
        read_frame, read_host_greeting, write_frame, Capability, HostGreeting, Kind,
    };
    use codesense_pty_core::domain::ViewerId;
    use codesense_pty_core::protocol::{client_hello, Message, Request};
    let deadline = Instant::now() + EXIT_GRACE;
    let stream = connect_cooperative(
        &codesense_pty_core::protocol::socket_path(filesystem_root),
        remaining_budget(deadline, Instant::now())?,
    )?;
    let mut stream = DeadlineStream { stream, deadline };
    let (greeting, _) = read_host_greeting(&mut stream)?;
    let (proto_major, instance_id, server_id, epoch, pid, capable) = match greeting {
        HostGreeting::Supported { hello, .. } => {
            let capable = hello.supports(&Capability::ShutdownForReplacement);
            (
                hello.proto_major,
                hello.instance_id,
                hello.server_id,
                hello.epoch,
                hello.pid,
                capable,
            )
        }
        HostGreeting::Unsupported(hello) => (
            hello.proto_major,
            hello.instance_id,
            hello.server_id,
            hello.epoch,
            hello.pid,
            hello
                .capabilities
                .contains(&Capability::ShutdownForReplacement),
        ),
    };
    if !capable || server_id != auth.server_id || epoch != auth.epoch || pid != auth.pid {
        return Err("cooperative shutdown capability or identity mismatch".into());
    }
    let mut ours = client_hello(
        instance_id,
        env!("CARGO_PKG_VERSION"),
        &ViewerId::new("replacement"),
    );
    ours.proto_major = proto_major;
    write_frame(&mut stream, &ours.frame()?)
        .map_err(|error| format!("write replacement client hello: {error}"))?;
    let request = Request::ShutdownForReplacement {
        nonce: auth.nonce.clone(),
        server_id: auth.server_id.clone(),
        epoch: auth.epoch.clone(),
        process_start: auth.process_start,
    };
    write_frame(
        &mut stream,
        &codesense_host_proto::Frame::control(&Message::Call { rid: 1, request })?,
    )
    .map_err(|error| format!("write replacement shutdown request: {error}"))?;
    loop {
        let frame = read_frame(&mut stream)?.ok_or("server exited before shutdown reply")?;
        if frame.kind != Kind::CONTROL {
            continue;
        }
        if let Message::Reply { rid: 1, result } = frame.decode_json::<Message>()? {
            return result;
        }
    }
}

fn remaining_budget(deadline: Instant, now: Instant) -> Result<Duration, String> {
    let remaining = deadline.saturating_duration_since(now);
    (!remaining.is_zero())
        .then_some(remaining)
        .ok_or_else(|| "cooperative shutdown overall deadline exceeded".into())
}

struct DeadlineStream {
    stream: UnixStream,
    deadline: Instant,
}

impl DeadlineStream {
    fn remaining(&self) -> io::Result<Duration> {
        remaining_budget(self.deadline, Instant::now())
            .map_err(|message| io::Error::new(io::ErrorKind::TimedOut, message))
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(buffer)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.flush()
    }
}

fn await_nonblocking_connect(
    fd: i32,
    timeout: Duration,
    mut poller: impl FnMut(i32) -> io::Result<i16>,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("connect cooperative shutdown timed out".into());
        }
        let millis = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
        let revents =
            poller(millis).map_err(|e| format!("poll cooperative shutdown connect: {e}"))?;
        if revents == 0 {
            return Err("connect cooperative shutdown timed out".into());
        }
        if revents & libc::POLLNVAL != 0 {
            return Err("connect cooperative shutdown returned POLLNVAL".into());
        }
        if revents & (libc::POLLOUT | libc::POLLERR | libc::POLLHUP) == 0 {
            continue;
        }
        let mut socket_error: libc::c_int = 0;
        let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut socket_error as *mut libc::c_int).cast(),
                &mut length,
            )
        } < 0
        {
            return Err(format!(
                "inspect cooperative shutdown connect: {}",
                io::Error::last_os_error()
            ));
        }
        if socket_error != 0 {
            return Err(format!(
                "connect cooperative shutdown: {}",
                io::Error::from_raw_os_error(socket_error)
            ));
        }
        return Ok(());
    }
}

fn connect_cooperative(path: &Path, timeout: Duration) -> Result<UnixStream, String> {
    let bytes = path.as_os_str().as_bytes();
    let max = std::mem::size_of::<libc::sockaddr_un>()
        - std::mem::offset_of!(libc::sockaddr_un, sun_path);
    if bytes.is_empty() || bytes.len() >= max {
        return Err("cooperative shutdown socket path is invalid".into());
    }
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(format!(
            "create cooperative shutdown socket: {}",
            io::Error::last_os_error()
        ));
    }
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    let address_len =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    let connected = unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            address_len,
        )
    };
    if connected < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            unsafe { libc::close(fd) };
            return Err(format!("connect cooperative shutdown: {error}"));
        }
        let result = await_nonblocking_connect(fd, timeout, |millis| {
            let mut descriptor = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let result = unsafe { libc::poll(&mut descriptor, 1, millis) };
            if result < 0 {
                Err(io::Error::last_os_error())
            } else if result == 0 {
                Ok(0)
            } else {
                Ok(descriptor.revents)
            }
        });
        if let Err(error) = result {
            unsafe { libc::close(fd) };
            return Err(error);
        }
    }
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    stream
        .set_nonblocking(false)
        .map_err(|e| format!("restore cooperative shutdown socket blocking mode: {e}"))?;
    Ok(stream)
}

#[cfg(test)]
mod connect_tests {
    use super::*;

    #[test]
    fn rm4d_cooperative_connect_timeout_is_explicit_and_deterministic() {
        let error = await_nonblocking_connect(-1, Duration::from_secs(1), |_| Ok(0)).unwrap_err();
        assert!(error.contains("timed out"), "{error}");
    }

    #[test]
    fn rm4d_cooperative_connect_poll_error_is_propagated() {
        let error = await_nonblocking_connect(-1, Duration::from_secs(1), |_| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
        })
        .unwrap_err();
        assert!(error.contains("injected"), "{error}");
    }

    #[test]
    fn rm4d_irrelevant_frames_share_one_absolute_deadline_budget() {
        let start = Instant::now();
        let deadline = start + Duration::from_secs(3);
        assert_eq!(
            remaining_budget(deadline, start + Duration::from_millis(400)).unwrap(),
            Duration::from_millis(2600)
        );
        assert_eq!(
            remaining_budget(deadline, start + Duration::from_millis(2400)).unwrap(),
            Duration::from_millis(600)
        );
        assert!(remaining_budget(deadline, deadline).is_err());
        assert!(remaining_budget(deadline, deadline + Duration::from_secs(1)).is_err());
    }
}

/// Replace the exact process named by the authorization. The helper never
/// signals it: only its authenticated cooperative protocol may retire it.
pub(crate) fn retire_authorized(
    auth: &Authorization,
    logical_root: &Path,
    filesystem_root: &Path,
) -> Result<(), String> {
    let pidfd = PidFd::open(auth.pid)?;
    revalidate_at(auth, logical_root, filesystem_root)?;
    let receipt_path = filesystem_root.join(format!("replacement-retirement-{}.json", auth.nonce));
    let draining_path = filesystem_root.join(format!("replacement-draining-{}.json", auth.nonce));
    // The daemon may exit after durably publishing its receipt but before its
    // reply reaches this socket. Treat the reply as corroboration; the pidfd
    // exit plus exact durable receipt are the authoritative completion proof.
    let reply = cooperative_shutdown_at(auth, filesystem_root);
    let exited = pidfd.wait(EXIT_GRACE)?;
    let drain_began = fs::read(&draining_path)
        .ok()
        .and_then(|bytes| crate::replacement_evidence::decode(&bytes).ok())
        .is_some_and(|marker| matches!(marker, codesense_pty_core::protocol::Reply::Retired { nonce, server_id, epoch, process_start, sessions: u32::MAX } if nonce == auth.nonce && server_id == auth.server_id && epoch == auth.epoch && process_start == auth.process_start));
    if drain_began {
        eprintln!("CODESENSE_REPLACE_PHASE=old-stopped");
    }
    if !exited {
        return Err(reply
            .err()
            .unwrap_or_else(|| "cooperatively retired terminal server did not exit".into()));
    }
    let persisted: codesense_pty_core::protocol::Reply = crate::replacement_evidence::decode(
        &fs::read(&receipt_path).map_err(|e| format!("read retirement receipt: {e}"))?,
    )
    .map_err(|e| format!("decode retirement receipt: {e}"))?;
    if reply.as_ref().is_ok_and(|reply| reply != &persisted)
        || !matches!(&persisted, codesense_pty_core::protocol::Reply::Retired { nonce, server_id, epoch, process_start, .. } if nonce == &auth.nonce && server_id == &auth.server_id && epoch == &auth.epoch && *process_start == auth.process_start)
    {
        return Err("retirement receipt did not match authorization".into());
    }
    if !drain_began {
        return Err("terminal server exited without a matching drain-began record".into());
    }
    Ok(())
}

pub fn authorized(auth: &Authorization) -> Result<(), String> {
    let logical_root = dirs::home_dir()
        .ok_or("no home directory")?
        .join(".codesense");
    retire_authorized(auth, &logical_root, &auth.root)?;
    let binary = std::env::current_exe().map_err(|e| format!("own executable: {e}"))?;
    let resources = binary
        .parent()
        .map(|dir| dir.join(crate::attach::RESOURCES_DIR))
        .filter(|dir| dir.is_dir());
    let target = crate::attach::Target {
        root: auth.root.clone(),
        resources,
    };
    drop(crate::attach::connect_or_launch(&target, &binary)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rm4d_authorization_is_bounded_and_versioned() {
        assert!(Authorization::parse("schema=codesense-replace-v2\n").is_err());
        assert!(Authorization::parse("schema=codesense-replace-v1\n").is_err());
    }

    #[test]
    fn rm4d_never_targets_itself() {
        let auth = Authorization {
            uid: unsafe { libc::geteuid() },
            pid: std::process::id(),
            process_start: 1,
            executable: PathBuf::from("/nope"),
            executable_dev: 1,
            executable_ino: 1,
            root: PathBuf::from("/nope"),
            manifest_digest: "a".repeat(64),
            version: "v1".into(),
            server_id: "server".into(),
            nonce: "0123456789abcdef".into(),
            epoch: "epoch".into(),
        };
        assert_eq!(revalidate(&auth).unwrap_err(), "invalid replacement target");
    }
}
