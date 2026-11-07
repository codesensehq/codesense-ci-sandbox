// SPDX-License-Identifier: MIT
//! Shared std-mutex lock helper (RF-9).
//!
//! Collapses the 13 copy-pasted `.lock().map_err(|e| format!("lock error: {e}"))?`
//! idioms in `commands::watch` into one extension-trait method. The mapped
//! string lives here exactly once. Behavior is byte-identical to the inline
//! form: the returned [`MutexGuard`] borrows the mutex directly (no clone, no
//! `Arc` bump), so guard scope — and any poison propagation — is unchanged.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Lock a [`std::sync::Mutex`], mapping a poison error to the canonical
/// `"lock error: {e}"` string.
///
/// The label-free method keeps the call site terse (`self.state.lock_or_err()?`);
/// all 13 rerouted sites share the same message, so no label argument is needed.
pub trait LockOrErr<T> {
    /// Acquire the lock, returning the guard or a `"lock error: {e}"` string.
    fn lock_or_err(&self) -> Result<MutexGuard<'_, T>, String>;
}

impl<T> LockOrErr<T> for Mutex<T> {
    #[inline]
    fn lock_or_err(&self) -> Result<MutexGuard<'_, T>, String> {
        self.lock().map_err(|e| format!("lock error: {e}"))
    }
}

/// Wait on a blocking persistence lock and name it while a prolonged wait is ongoing.
/// Callers must already be off asynchronous workers when the holder can do I/O.
pub(crate) fn lock_named<'a, T>(
    mutex: &'a Mutex<T>,
    name: &'static str,
) -> std::sync::LockResult<MutexGuard<'a, T>> {
    lock_named_observed(mutex, std::time::Duration::from_secs(5), move || {
        log::warn!(target: "lock_wait", "waiting more than 5 s for {name} lock");
    })
}

/// Try the lock, otherwise park on the mutex itself (no polling) while a single
/// process-wide watchdog fires `warn` once if the wait outlasts `warning_after`.
fn lock_named_observed<T>(
    mutex: &Mutex<T>,
    warning_after: std::time::Duration,
    warn: impl FnOnce() + Send + 'static,
) -> std::sync::LockResult<MutexGuard<'_, T>> {
    match mutex.try_lock() {
        Ok(guard) => return Ok(guard),
        Err(std::sync::TryLockError::Poisoned(error)) => return Err(error),
        Err(std::sync::TryLockError::WouldBlock) => {}
    }
    let done = Arc::new(AtomicBool::new(false));
    watchdog::register(
        std::time::Instant::now() + warning_after,
        done.clone(),
        Box::new(warn),
    );
    let result = mutex.lock();
    done.store(true, Ordering::Release);
    result
}

/// One lazily started thread holding a deadline min-heap of pending lock-wait
/// warnings. Entries whose wait already finished are skipped when due and
/// pruned on registration once the heap grows.
mod watchdog {
    use super::{Arc, AtomicBool, Ordering};
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    use std::sync::{Condvar, Mutex, Once};
    use std::time::Instant;

    const PRUNE_ABOVE: usize = 64;

    struct Entry {
        deadline: Instant,
        done: Arc<AtomicBool>,
        warn: Box<dyn FnOnce() + Send>,
    }
    impl PartialEq for Entry {
        fn eq(&self, other: &Self) -> bool {
            self.deadline == other.deadline
        }
    }
    impl Eq for Entry {}
    impl PartialOrd for Entry {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for Entry {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            self.deadline.cmp(&other.deadline)
        }
    }

    struct Shared {
        heap: Mutex<BinaryHeap<Reverse<Entry>>>,
        wake: Condvar,
    }

    static SHARED: Shared = Shared {
        heap: Mutex::new(BinaryHeap::new()),
        wake: Condvar::new(),
    };
    static START: Once = Once::new();
    #[cfg(test)]
    pub(super) static STARTS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    pub(super) fn register(
        deadline: Instant,
        done: Arc<AtomicBool>,
        warn: Box<dyn FnOnce() + Send>,
    ) {
        START.call_once(|| {
            #[cfg(test)]
            STARTS.fetch_add(1, Ordering::SeqCst);
            std::thread::Builder::spawn(
                std::thread::Builder::name(
                    std::thread::Builder::new(),
                    "lock-wait-watchdog".into(),
                ),
                run,
            )
            .expect("spawn lock-wait watchdog");
        });
        let mut heap = SHARED.heap.lock().unwrap_or_else(|e| e.into_inner());
        if heap.len() >= PRUNE_ABOVE {
            BinaryHeap::retain(&mut heap, |Reverse(e)| !e.done.load(Ordering::Acquire));
        }
        let earlier = heap.peek().is_none_or(|Reverse(e)| deadline < e.deadline);
        heap.push(Reverse(Entry {
            deadline,
            done,
            warn,
        }));
        if earlier {
            SHARED.wake.notify_one();
        }
    }

    #[cfg(test)]
    pub(super) fn pending() -> usize {
        SHARED.heap.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    fn run() {
        let mut heap = SHARED.heap.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let now = Instant::now();
            match heap.peek().map(|Reverse(e)| e.deadline) {
                None => {
                    heap = SHARED.wake.wait(heap).unwrap_or_else(|e| e.into_inner());
                }
                Some(deadline) if deadline > now => {
                    heap = SHARED
                        .wake
                        .wait_timeout(heap, deadline - now)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                Some(_) => {
                    let Reverse(entry) = heap.pop().expect("peeked");
                    if !entry.done.load(Ordering::Acquire) {
                        // Run outside the heap lock so a slow logger can't block registration.
                        drop(heap);
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(entry.warn));
                        heap = SHARED.heap.lock().unwrap_or_else(|e| e.into_inner());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The timing/CPU tests below measure the whole process, so they run one at a time.
    fn serial() -> MutexGuard<'static, ()> {
        static GATE: Mutex<()> = Mutex::new(());
        GATE.lock().unwrap_or_else(|e| e.into_inner())
    }
    #[test]
    fn cp3_warns_once_while_named_lock_is_still_held() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        let lock = Arc::new(Mutex::new(()));
        let holder = lock.lock().unwrap();
        let waiting = lock.clone();
        let (warned, warning) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _guard = lock_named_observed(&waiting, Duration::from_millis(20), move || {
                warned.send("app-state I/O").unwrap();
            })
            .unwrap();
            done.send(()).unwrap();
        });
        let observed = warning.recv_timeout(Duration::from_secs(5));
        let acquired_while_held = completed.try_recv().is_ok();
        drop(holder);
        worker.join().unwrap();
        assert_eq!(observed.unwrap(), "app-state I/O");
        assert!(!acquired_while_held);
        assert!(warning.try_recv().is_err());
    }

    #[test]
    fn no_warning_when_lock_acquired_before_deadline() {
        use std::sync::{mpsc, Arc};
        use std::time::Duration;
        let lock = Arc::new(Mutex::new(()));
        let holder = lock.lock().unwrap();
        let waiting = lock.clone();
        let (warned, warning) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _g = lock_named_observed(&waiting, Duration::from_millis(300), move || {
                warned.send(()).unwrap();
            })
            .unwrap();
        });
        std::thread::sleep(Duration::from_millis(30));
        drop(holder);
        worker.join().unwrap();
        // `warned` was moved into the closure; it is dropped unfired, so recv errors
        // with Disconnected only once the watchdog has discarded the entry.
        assert!(matches!(
            warning.recv_timeout(Duration::from_millis(600)),
            Err(mpsc::RecvTimeoutError::Disconnected) | Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }

    #[test]
    fn poison_propagates_as_err() {
        use std::sync::Arc;
        let m = Arc::new(Mutex::new(0));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison");
        })
        .join();
        // Fast path (try_lock sees Poisoned).
        assert!(lock_named_observed(&m, std::time::Duration::from_secs(5), || {}).is_err());
        // Slow path: poisoned while a waiter is parked on lock().
        let m = Arc::new(Mutex::new(0));
        let holder = m.clone();
        let (held, held_rx) = std::sync::mpsc::channel();
        let (go, go_rx) = std::sync::mpsc::channel::<()>();
        let t = std::thread::spawn(move || {
            let _g = holder.lock().unwrap();
            held.send(()).unwrap();
            go_rx.recv().unwrap();
            panic!("poison while held");
        });
        held_rx.recv().unwrap();
        let waiter = m.clone();
        let w = std::thread::spawn(move || {
            lock_named_observed(&waiter, std::time::Duration::from_secs(5), || {}).is_err()
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        go.send(()).unwrap();
        let _ = t.join();
        assert!(w.join().unwrap());
    }

    #[test]
    fn wake_latency_is_within_5ms() {
        let _serial = serial();
        use std::sync::{mpsc, Arc};
        use std::time::{Duration, Instant};
        let mut samples = Vec::new();
        for _ in 0..20 {
            let lock = Arc::new(Mutex::new(()));
            let holder = lock.lock().unwrap();
            let waiting = lock.clone();
            let (tx, rx) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let _g = lock_named_observed(&waiting, Duration::from_secs(5), || {}).unwrap();
                tx.send(Instant::now()).unwrap();
            });
            std::thread::sleep(Duration::from_millis(20));
            let released = Instant::now();
            drop(holder);
            let acquired = rx.recv().unwrap();
            worker.join().unwrap();
            samples.push(acquired.saturating_duration_since(released));
        }
        samples.sort();
        assert!(
            samples[10] <= Duration::from_millis(5),
            "median wake {:?}",
            samples[10]
        );
    }

    /// (CPU time, voluntary context switches) of this process.
    #[cfg(unix)]
    fn usage() -> (std::time::Duration, i64) {
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) }, 0);
        let tv =
            |t: libc::timeval| std::time::Duration::new(t.tv_sec as u64, (t.tv_usec as u32) * 1000);
        (tv(ru.ru_utime) + tv(ru.ru_stime), ru.ru_nvcsw as i64)
    }

    /// 32 waiters on a 500 ms hold must park, not poll. `getrusage(RUSAGE_SELF)` is
    /// process wide, so the measurement runs alone in a re-executed copy of this
    /// test binary (the `nofile` isolated-process pattern); the parent only asserts
    /// the child exited 0 AND ran exactly one test ("1 passed"). A sleep-poll loop wakes each waiter every 10 ms
    /// (~1,600 voluntary context switches and several ms of CPU in the child);
    /// parked waiters cost ~0 of both.
    #[cfg(unix)]
    #[test]
    fn blocked_waiters_do_not_poll() {
        const CHILD: &str = "CODESENSE_LOCK_POLL_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let _serial = serial();
            // libtest exits 0 when `--exact` matches nothing, so the parent also
            // requires "1 passed": a renamed test can't make this vacuous.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "shared::lock_ext::tests::blocked_waiters_do_not_poll",
                    "--test-threads=1",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && str::contains(&stdout, "1 passed"),
                "isolated measurement failed: {}\nstdout:\n{stdout}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        use std::sync::{Arc, Barrier};
        use std::time::Duration;
        let lock = Arc::new(Mutex::new(()));
        let holder = lock.lock().unwrap();
        let barrier = Arc::new(Barrier::new(33));
        let waiters: Vec<_> = (0..32)
            .map(|_| {
                let l = lock.clone();
                let b = barrier.clone();
                std::thread::spawn(move || {
                    b.wait();
                    drop(lock_named_observed(&l, Duration::from_secs(5), || {}).unwrap());
                })
            })
            .collect();
        barrier.wait();
        std::thread::sleep(Duration::from_millis(100)); // let all park
        let (cpu_before, switches_before) = usage();
        std::thread::sleep(Duration::from_millis(500));
        let (cpu_after, switches_after) = usage();
        drop(holder);
        for w in waiters {
            w.join().unwrap();
        }
        let spent = cpu_after - cpu_before;
        let switches = switches_after - switches_before;
        assert!(
            spent < Duration::from_millis(25),
            "cpu during hold: {spent:?}"
        );
        assert!(
            switches < 100,
            "voluntary context switches during hold: {switches}"
        );
    }

    #[test]
    fn watchdog_starts_once_and_prunes_done_entries() {
        let _serial = serial();
        use std::sync::Arc;
        use std::time::Duration;
        let lock = Arc::new(Mutex::new(()));
        for _ in 0..1000 {
            let holder = lock.lock().unwrap();
            let waiting = lock.clone();
            let t = std::thread::spawn(move || {
                drop(lock_named_observed(&waiting, Duration::from_secs(60), || {}).unwrap());
            });
            // Give the waiter a moment to register, then release.
            std::thread::sleep(Duration::from_micros(200));
            drop(holder);
            t.join().unwrap();
        }
        assert_eq!(super::watchdog::STARTS.load(Ordering::SeqCst), 1);
        // Other tests may add a few concurrently; the bound is the prune threshold plus slack.
        assert!(
            super::watchdog::pending() < 200,
            "pending {}",
            super::watchdog::pending()
        );
    }

    #[test]
    fn lock_ext_has_no_sleep_poll() {
        let src = include_str!("lock_ext.rs");
        let prod = src.split("#[cfg(test)]\nmod tests").next().unwrap();
        assert!(!prod.contains("thread::sleep"));
    }

    #[test]
    fn lock_or_err_ok_returns_guard() {
        let m = Mutex::new(41);
        {
            let mut g = m.lock_or_err().unwrap();
            *g += 1;
        }
        assert_eq!(*m.lock_or_err().unwrap(), 42);
    }

    #[test]
    fn lock_or_err_maps_poison_to_canonical_prefix() {
        let m = std::sync::Arc::new(Mutex::new(0));
        let m2 = m.clone();
        // Poison the mutex by panicking while the guard is held.
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("poison");
        })
        .join();
        let err = m.lock_or_err().unwrap_err();
        assert!(
            err.starts_with("lock error: "),
            "expected canonical prefix, got: {err}"
        );
    }
}
