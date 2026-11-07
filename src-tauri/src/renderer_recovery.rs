// SPDX-License-Identifier: MIT
//! Architecture: posted reloads are checked and surviving renderers require fresh ownership proof.
use crate::renderer_watchdog::{ReloadCause, RendererLiveness};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use tauri::Manager;

// Policy: check the first reload after 5 s; allow recovery 30 s, dispatch at most 5 s.
const CHECK_DELAY: Duration = Duration::from_secs(5);
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(5);
static DISPATCHING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Recovered,
    Gone,
    Refuse,
    Terminate,
    TimedOut,
}

fn decide(
    elapsed: Duration,
    recovered: bool,
    alive: bool,
    in_snapshot: bool,
    owned: bool,
) -> Action {
    if recovered {
        Action::Recovered
    } else if elapsed >= RECOVERY_TIMEOUT {
        Action::TimedOut
    } else if !alive {
        Action::Gone
    } else if !in_snapshot || !owned {
        Action::Refuse
    } else {
        Action::Terminate
    }
}

async fn blocking<T: Send + 'static>(
    budget: Duration,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::time::timeout(
        budget,
        crate::commands::await_blocking("renderer recovery", work),
    )
    .await
    .map_err(|_| "renderer recovery operation timed out".to_string())?
}

async fn post_reload(app: tauri::AppHandle, budget: Duration) -> Result<u64, String> {
    // Architecture: a stuck dispatcher can occupy only one blocking thread, never a runtime worker.
    if DISPATCHING.swap(true, Ordering::AcqRel) {
        return Err("previous webview reload dispatch is still outstanding".into());
    }
    struct Release;
    impl Drop for Release {
        fn drop(&mut self) {
            DISPATCHING.store(false, Ordering::Release);
        }
    }
    let release = Release;
    let until = Instant::now() + budget;
    blocking(budget, move || {
        let _release = release;
        let win = app
            .get_webview_window("main")
            .ok_or("no 'main' webview window to reload")?;
        if Instant::now() >= until {
            return Err("reload dispatch expired before posting".into());
        }
        // Architecture: only a heartbeat received after posting proves recovery.
        let posted_at = crate::runtime::now_ms();
        win.reload().map_err(|e| e.to_string())?;
        Ok(posted_at)
    })
    .await
}

fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        pid > 1
            && (unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn terminate_survivors(
    app: &tauri::AppHandle,
    original: &[u32],
    started: Instant,
    ack: u64,
) -> Result<usize, String> {
    let (_, current) = crate::memory_metrics::renderer_webcontent_snapshot();
    let own_pid = std::process::id();
    let mut killed = 0;
    for &pid in original {
        let recovered = app.state::<RendererLiveness>().last_ack_ms() > ack;
        // Architecture: membership in BOTH snapshots plus a direct app responsibility check.
        // Revalidate immediately before the signal; a name alone never grants ownership.
        let owned =
            pid > 1 && pid != own_pid && crate::memory_metrics::responsible(pid) == Some(own_pid);
        match decide(
            started.elapsed(),
            recovered,
            alive(pid),
            current.contains(&pid),
            owned,
        ) {
            Action::Recovered => return Ok(killed),
            Action::TimedOut => return Err("renderer recovery expired before termination".into()),
            Action::Gone => {
                log::info!(target: "webview-watchdog", "old renderer pid={pid} exited after initial reload")
            }
            Action::Refuse => {
                log::warn!(target: "webview-watchdog", "refusing renderer termination pid={pid}: current snapshot/ownership proof missing")
            }
            Action::Terminate => {
                #[cfg(unix)]
                if unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(libc::ESRCH) {
                        log::info!(target: "webview-watchdog", "renderer pid={pid} exited before termination");
                        continue;
                    }
                    return Err(format!("terminate renderer pid={pid}: {error}"));
                }
                killed += 1;
                log::warn!(target: "webview-watchdog", "terminated surviving renderer pid={pid}; verified snapshot and responsible app pid={own_pid}");
            }
        }
    }
    Ok(killed)
}

async fn recover(app: tauri::AppHandle, original: Vec<u32>, ack: u64, started: Instant) {
    let result = tokio::time::timeout(RECOVERY_TIMEOUT.saturating_sub(started.elapsed()), async {
        tokio::time::sleep(CHECK_DELAY).await;
        if app.state::<RendererLiveness>().last_ack_ms() > ack { return Ok::<_, String>(()); }
        let check_app = app.clone();
        let killed = blocking(RECOVERY_TIMEOUT.saturating_sub(started.elapsed()), move || {
            terminate_survivors(&check_app, &original, started, ack)
        }).await?;
        if killed > 0 {
            post_reload(app.clone(), DISPATCH_TIMEOUT.min(RECOVERY_TIMEOUT.saturating_sub(started.elapsed()))).await?;
            log::info!(target: "webview-watchdog", "replacement reload posted after terminating {killed} renderer(s); awaiting heartbeat");
        }
        while app.state::<RendererLiveness>().last_ack_ms() <= ack {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }).await;
    match result {
        Ok(Ok(())) => {
            log::info!(target: "webview-watchdog", "renderer recovery complete: heartbeat resumed")
        }
        Ok(Err(error)) => {
            log::error!(target: "webview-watchdog", "renderer recovery failed: {error}")
        }
        Err(_) => {
            log::error!(target: "webview-watchdog", "renderer recovery timed out after 30 s; no heartbeat received")
        }
    }
}

pub(crate) async fn reload(app: tauri::AppHandle, cause: ReloadCause) -> bool {
    let original = if cause == ReloadCause::ProcessDeath || crate::memory_metrics::memory_enabled()
    {
        match blocking(DISPATCH_TIMEOUT, || {
            Ok(crate::memory_metrics::renderer_webcontent_snapshot())
        })
        .await
        {
            Ok((rss, pids)) => {
                // Architecture: recovery and the reload forensic line share one off-thread snapshot.
                if crate::memory_metrics::memory_enabled() {
                    log::debug!(target: "memory", "reload cause={} pre_pids={:?} pre_rss={}", cause.as_str(), pids, rss);
                }
                pids
            }
            Err(error) => {
                log::error!(target: "webview-watchdog", "pre-reload renderer snapshot failed: {error}");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    match post_reload(app.clone(), DISPATCH_TIMEOUT).await {
        Err(error) => {
            log::error!(target: "webview-watchdog", "webview reload failed: {error}");
            false
        }
        Ok(ack) => {
            log::info!(target: "webview-watchdog", "initial reload posted; old renderer pids={original:?}");
            if cause == ReloadCause::ProcessDeath {
                tauri::async_runtime::spawn(recover(app, original, ack, Instant::now()));
            }
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn blocked_gateway_times_out_without_pinning_the_runtime() {
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, held) = std::sync::mpsc::channel();
        let (finished, done) = tokio::sync::oneshot::channel();
        let operation = blocking(Duration::from_millis(50), move || {
            let _ = entered.send(());
            // Architecture: release even on assertion failure so the test cannot leak a worker.
            let _ = held.recv_timeout(Duration::from_secs(2));
            let _ = finished.send(());
            Ok(())
        });
        let sibling = async {
            ready.await.unwrap();
            tokio::task::yield_now().await;
            true
        };
        let (result, progressed) = tokio::join!(operation, sibling);
        assert!(progressed);
        assert!(result.unwrap_err().contains("timed out"));
        release.send(()).unwrap();
        done.await.unwrap();
    }

    #[test]
    fn only_a_live_proven_renderer_can_be_terminated() {
        assert_eq!(
            decide(Duration::ZERO, false, true, true, true),
            Action::Terminate
        );
        assert_eq!(
            decide(Duration::ZERO, false, false, true, true),
            Action::Gone
        );
        assert_eq!(
            decide(Duration::ZERO, false, true, false, true),
            Action::Refuse
        );
        assert_eq!(
            decide(Duration::ZERO, false, true, true, false),
            Action::Refuse
        );
    }

    #[test]
    fn recovered_or_expired_attempts_never_terminate() {
        assert_eq!(
            decide(Duration::ZERO, true, true, true, true),
            Action::Recovered
        );
        assert_eq!(
            decide(Duration::from_secs(30), false, true, true, true),
            Action::TimedOut
        );
    }
}
