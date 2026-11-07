// SPDX-License-Identifier: MIT
//! The one seam every fsync-class call of a workspace effect goes
//! through. It counts the calls and the time they take on the calling thread,
//! so a test can bound how many one Apply makes and the Apply's log line can
//! say where its time went.
//!
//! On macOS `File::sync_all` is `F_FULLFSYNC` (a device cache flush): the
//! count is the cost.
use std::cell::Cell;
use std::fs::File;
use std::time::{Duration, Instant};

// Per thread: (flushes, orders, time spent in both).
thread_local! {
    static SYNCS: Cell<(u64, u64, Duration)> = const { Cell::new((0, 0, Duration::ZERO)) };
}
fn count(flush: bool, spent: Duration) {
    SYNCS.with(|syncs| {
        let (flushes, orders, total) = syncs.get();
        syncs.set(if flush {
            (flushes + 1, orders, total + spent)
        } else {
            (flushes, orders + 1, total + spent)
        });
    });
}

/// Flush `file` (data and metadata; for a directory, its entries) to stable
/// storage, counted.
pub(crate) fn sync(file: &File) -> std::io::Result<()> {
    let started = Instant::now();
    let result = file.sync_all();
    count(true, started.elapsed());
    result
}

/// Order `file`'s writes before every later write (data, or a directory's
/// entries), counted. On macOS this is `F_BARRIERFSYNC`, which does not flush
/// the drive cache: a later [`sync`] (an `F_FULLFSYNC`) makes everything
/// ordered before it durable at once. Elsewhere it is an `fsync`.
pub(crate) fn order(file: &File) -> std::io::Result<()> {
    let started = Instant::now();
    let result = barrier(file);
    count(false, started.elapsed());
    result
}
#[cfg(target_os = "macos")]
fn barrier(file: &File) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: a held descriptor; the command takes no argument.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        // A volume without barriers: a full flush orders too (never a plain
        // fsync, which on macOS neither orders nor flushes).
        Some(libc::ENOTSUP | libc::EINVAL) => file.sync_all(),
        // Anything else (EIO) is the sync's own failure: never masked by a
        // retry that may report success for data already lost.
        _ => Err(error),
    }
}
#[cfg(not(target_os = "macos"))]
fn barrier(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

/// The fsync-class calls this thread made since [`Meter::start`].
#[derive(Clone, Copy)]
pub(crate) struct Meter {
    from: (u64, u64, Duration),
}
/// What a [`Meter`] read: flushes ([`sync`]), orders ([`order`]), and the
/// time both took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Syncs {
    pub(crate) flushes: u64,
    pub(crate) orders: u64,
    pub(crate) spent: Duration,
}
impl Syncs {
    pub(crate) fn total(&self) -> u64 {
        self.flushes + self.orders
    }
}
impl Meter {
    pub(crate) fn start() -> Self {
        Self {
            from: SYNCS.with(Cell::get),
        }
    }
    pub(crate) fn read(&self) -> Syncs {
        let (flushes, orders, spent) = SYNCS.with(Cell::get);
        Syncs {
            flushes: flushes - self.from.0,
            orders: orders - self.from.1,
            spent: spent.saturating_sub(self.from.2),
        }
    }
}
