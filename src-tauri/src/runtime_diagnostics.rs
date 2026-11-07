// SPDX-License-Identifier: MIT
//! Architecture: an OS thread observes atomic liveness without the runtime/logger locks.
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};

// Policy: capture after 30 s, sample for 5 s, retain one file capped at 4 MiB.
const STALE_MS: u64 = 30_000;
const CAPTURE_BYTES: u64 = 4 * 1024 * 1024;
const PROBE_INTERVAL: Duration = Duration::from_secs(5);

static EPOCH: OnceLock<Instant> = OnceLock::new();
static RUNTIME: AtomicU64 = AtomicU64::new(0);
static LOGGER: AtomicU64 = AtomicU64::new(0);
static RUNTIME_STALLED: AtomicBool = AtomicBool::new(false);

pub(crate) fn runtime_stalled() -> bool {
    RUNTIME_STALLED.load(Ordering::Acquire)
}

fn publish_runtime_health(now: u64, runtime: u64, stalled: &AtomicBool) {
    // Architecture: only the off-runtime observer decides staleness, including recovery.
    stalled.store(now.saturating_sub(runtime) >= STALE_MS, Ordering::Release);
}

fn now_ms() -> u64 {
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

pub(crate) fn logger_beat() {
    LOGGER.fetch_max(now_ms(), Ordering::Relaxed);
}

#[derive(Default)]
struct Incident {
    captured: bool,
}

impl Incident {
    fn observe(&mut self, now: u64, runtime: u64, logger: u64) -> bool {
        let stale =
            now.saturating_sub(runtime) >= STALE_MS || now.saturating_sub(logger) >= STALE_MS;
        if !stale {
            self.captured = false;
            return false;
        }
        if self.captured {
            return false;
        }
        self.captured = true;
        true
    }
}

fn watch_runtime(
    stop: &AtomicBool,
    stalled: &AtomicBool,
    poll: Duration,
    mut heartbeats: impl FnMut() -> (u64, u64, u64),
    mut on_stall: impl FnMut(u64, u64, u64),
) {
    let mut incident = Incident::default();
    while !stop.load(Ordering::Relaxed) {
        let (now, runtime, logger) = heartbeats();
        publish_runtime_health(now, runtime, stalled);
        if incident.observe(now, runtime, logger) {
            on_stall(now, runtime, logger);
        }
        std::thread::sleep(poll);
    }
}

struct Stop(Arc<AtomicBool>);
impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn marker(path: &Path, message: &str) {
    // Architecture: a separate append descriptor, without log macros or logger locks.
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(
            file,
            "[runtime-self-capture] pid={} uptime_ms={} {message}",
            std::process::id(),
            now_ms()
        );
    }
}

#[cfg(target_os = "macos")]
fn capture(dir: &Path, log: &Path) {
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
    };
    let output = dir.join("runtime-stall.sample.txt");
    if let Err(e) = File::create(&output) {
        marker(log, &format!("cannot create sample: {e}"));
        return;
    }
    // Architecture: this pid is obtained here, never selected from a process name.
    let mut command = Command::new("/usr/bin/sample");
    command
        .args([
            std::process::id().to_string(),
            "5".into(),
            "-file".into(),
            output.to_string_lossy().into_owned(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Architecture: enforce the output cap in the child before it can write.
    unsafe {
        command.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: CAPTURE_BYTES,
                rlim_max: CAPTURE_BYTES,
            };
            if libc::setrlimit(libc::RLIMIT_FSIZE, &limit) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    match command.spawn() {
        Err(e) => marker(log, &format!("sample spawn failed: {e}")),
        Ok(mut child) => {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                // Architecture: keep observing recovery while this same OS thread samples.
                publish_runtime_health(now_ms(), RUNTIME.load(Ordering::Relaxed), &RUNTIME_STALLED);
                match child.try_wait() {
                    Ok(Some(status)) => {
                        marker(
                            log,
                            &format!("sample finished: {status}; path={}", output.display()),
                        );
                        break;
                    }
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(100))
                    }
                    result => {
                        let _ = child.kill();
                        let _ = child.wait();
                        marker(
                            log,
                            &format!(
                                "sample stopped at capture deadline or wait error: {result:?}"
                            ),
                        );
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn capture(_dir: &Path, log: &Path) {
    marker(log, "native sample is available on macOS only");
}

pub(crate) fn start(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let dir = crate::feedback::log_dir(app)?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let log: PathBuf = dir.join(format!("{}.log", app.package_info().name));
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let initial = now_ms();
    RUNTIME.store(initial, Ordering::Relaxed);
    LOGGER.store(initial, Ordering::Relaxed);
    std::thread::Builder::new().name("runtime-self-capture".into()).spawn(move || {
        watch_runtime(
            &thread_stop,
            &RUNTIME_STALLED,
            Duration::from_secs(1),
            || (now_ms(), RUNTIME.load(Ordering::Relaxed), LOGGER.load(Ordering::Relaxed)),
            |now, runtime, logger| {
                marker(&log, &format!("heartbeat stale: runtime_age_ms={} logger_age_ms={}; capturing own process", now.saturating_sub(runtime), now.saturating_sub(logger)));
                capture(&dir, &log);
            },
        );
    }).map_err(|e| e.to_string())?;
    let probe_inflight = Arc::new(AtomicBool::new(false));
    let runtime_stop = stop.clone();
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(PROBE_INTERVAL);
        loop {
            tick.tick().await;
            if runtime_stop.load(Ordering::Relaxed) {
                break;
            }
            RUNTIME.fetch_max(now_ms(), Ordering::Relaxed);
            // Architecture: flush also proves logger progress during otherwise idle periods.
            // Only one blocking probe may be outstanding; it cannot pin a Tokio worker.
            if !probe_inflight.swap(true, Ordering::Relaxed) {
                let inflight = probe_inflight.clone();
                tokio::task::spawn_blocking(move || {
                    log::logger().flush();
                    logger_beat();
                    inflight.store(false, Ordering::Relaxed);
                });
            }
        }
    });
    app.manage(Stop(stop));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cp3_os_observer_trips_and_captures_with_all_tokio_workers_parked_on_std_mutex() {
        use std::sync::{mpsc, Mutex};
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("capture.log");
        let lock = Arc::new(Mutex::new(()));
        let (held, ready) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let holder_lock = lock.clone();
        let holder = std::thread::spawn(move || {
            let _guard = holder_lock.lock().unwrap();
            held.send(()).unwrap();
            resume.recv().unwrap();
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .unwrap();
        let start = Instant::now();
        // Accelerate only the test clock. The production 30 s threshold is unchanged.
        let clock = move || start.elapsed().as_millis() as u64 * 1000;
        let heartbeat = Arc::new(AtomicU64::new(clock()));
        let beat = heartbeat.clone();
        let (live, started) = mpsc::channel();
        runtime.spawn(async move {
            live.send(()).unwrap();
            loop {
                beat.store(clock(), Ordering::Relaxed);
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        let (parking, parked) = mpsc::channel();
        for _ in 0..2 {
            let lock = lock.clone();
            let parking = parking.clone();
            runtime.spawn(async move {
                parking.send(()).unwrap();
                let _guard = lock.lock().unwrap();
            });
        }
        for _ in 0..2 {
            parked.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let health = Arc::new(AtomicBool::new(false));
        let observed_health = health.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let observer_stop = stop.clone();
        let observer_beat = heartbeat.clone();
        let (captured, capture) = mpsc::channel();
        let observer = std::thread::spawn(move || {
            watch_runtime(
                &observer_stop,
                &observed_health,
                Duration::from_millis(1),
                || {
                    let now = clock();
                    // A healthy logger must not mask a starved runtime.
                    (now, observer_beat.load(Ordering::Relaxed), now)
                },
                |_, _, _| {
                    marker(
                        &log,
                        "heartbeat stale: capturing own process (test capture sink)",
                    );
                    captured
                        .send(std::fs::read_to_string(&log).unwrap())
                        .unwrap();
                },
            );
        });
        let evidence = capture.recv_timeout(Duration::from_secs(5));
        let tripped = health.load(Ordering::Acquire);
        release.send(()).unwrap();
        holder.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while health.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let recovered = !health.load(Ordering::Acquire);
        stop.store(true, Ordering::Relaxed);
        observer.join().unwrap();
        runtime.shutdown_timeout(Duration::from_secs(1));
        assert!(
            tripped,
            "OS thread must mark the runtime unhealthy without a Tokio poll"
        );
        assert!(evidence.unwrap().contains("capturing own process"));
        assert!(
            recovered,
            "health must clear when the parked workers resume"
        );
    }

    #[test]
    fn os_thread_sets_and_clears_runtime_health_without_tokio() {
        let stalled = Arc::new(AtomicBool::new(false));
        let observer_flag = stalled.clone();
        let (updates, snapshots) = std::sync::mpsc::channel();
        let (observed, ready) = std::sync::mpsc::channel();
        let observer = std::thread::spawn(move || {
            for (now, runtime) in snapshots {
                publish_runtime_health(now, runtime, &observer_flag);
                observed.send(()).unwrap();
            }
        });
        for (now, runtime, expected) in [
            (29_999, 0, false),
            (30_000, 0, true),
            (30_001, 30_001, false),
        ] {
            updates.send((now, runtime)).unwrap();
            ready.recv().unwrap();
            assert_eq!(stalled.load(Ordering::Acquire), expected);
        }
        drop(updates);
        observer.join().unwrap();
    }

    #[test]
    fn either_heartbeat_triggers_at_thirty_seconds() {
        for (runtime, logger) in [(0, 29_999), (29_999, 0)] {
            let mut incident = Incident::default();
            assert!(!incident.observe(29_999, runtime, logger));
            assert!(incident.observe(30_000, runtime, logger));
        }
    }

    #[test]
    fn one_capture_per_incident_rearms_only_after_both_recover() {
        let mut incident = Incident::default();
        assert!(incident.observe(30_000, 0, 0));
        assert!(!incident.observe(60_000, 0, 60_000));
        assert!(!incident.observe(90_000, 90_000, 0));
        assert!(!incident.observe(90_001, 90_001, 90_001));
        assert!(incident.observe(120_001, 90_001, 120_001));
        assert!(!incident.observe(150_001, 90_001, 120_001));
    }
}
