// SPDX-License-Identifier: MIT
//! Kernel process identities: a PID alone is never an invocation identity.
use std::os::fd::AsRawFd;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Process {
    pub pid: u32,
    pub start: u64,
    pub parent: u32,
}

impl Process {
    /// Whether this is `other`, however either was re-parented since: its pid and start
    /// time.
    pub fn is(self, other: Process) -> bool {
        self.pid == other.pid && self.start == other.start
    }
}

#[cfg(target_os = "linux")]
pub(super) fn inspect(pid: u32) -> Option<Process> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let fields: Vec<_> = stat.rsplit_once(") ")?.1.split_whitespace().collect();
    if matches!(*fields.first()?, "Z" | "X") {
        return None;
    }
    Some(Process {
        pid,
        parent: fields.get(1)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}

#[cfg(target_os = "macos")]
pub(super) fn inspect(pid: u32) -> Option<Process> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    // SAFETY: the kernel writes a correctly sized proc_bsdinfo buffer.
    let read = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (read == size && info.pbi_status != 5).then_some(Process {
        pid,
        parent: info.pbi_ppid,
        start: info
            .pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.pbi_start_tvusec),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn inspect(_: u32) -> Option<Process> {
    None
}

pub(super) fn descends_from(mut peer: Process, root: Process) -> bool {
    for _ in 0..128 {
        if peer.pid == root.pid {
            return peer.is(root);
        }
        if peer.parent <= 1 || peer.parent == peer.pid {
            return false;
        }
        let Some(parent) = inspect(peer.parent) else {
            return false;
        };
        peer = parent;
    }
    false
}

/// Failure to read process metadata is unknown, not proof of death.
pub(super) fn retired(process: Process) -> bool {
    if let Some(current) = inspect(process.pid) {
        return current.start != process.start;
    }
    // SAFETY: signal zero only checks existence; it never signals the process.
    (unsafe { libc::kill(process.pid as i32, 0) }) != 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(target_os = "linux")]
pub(super) fn peer(stream: &impl AsRawFd) -> Option<Process> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&credentials) as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills the bounded credentials buffer.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if result != 0 || credentials.uid != unsafe { libc::geteuid() } {
        return None;
    }
    inspect(credentials.pid as u32)
}

#[cfg(target_os = "macos")]
pub(super) fn peer(stream: &impl AsRawFd) -> Option<Process> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: getpeereid writes the two scalar credentials.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0
        || uid != unsafe { libc::geteuid() }
    {
        return None;
    }
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
    // Darwin LOCAL_PEERPID is a kernel-authenticated pid (sys/un.h).
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            0,
            2,
            (&mut pid as *mut libc::pid_t).cast(),
            &mut len,
        )
    };
    if result != 0 || pid <= 0 {
        return None;
    }
    inspect(pid as u32)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn peer(_: &impl AsRawFd) -> Option<Process> {
    None
}
