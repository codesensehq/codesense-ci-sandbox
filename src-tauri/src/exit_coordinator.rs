// SPDX-License-Identifier: MIT
use crate::commands::host_connection_manager::{HostConnectionManager, ShutdownPhase};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitDisposition {
    StartDrain,
    KeepWaiting,
    Allow,
}

pub(crate) fn request(manager: &HostConnectionManager) -> ExitDisposition {
    match manager.shutdown_phase() {
        ShutdownPhase::Running if manager.begin_shutdown() => ExitDisposition::StartDrain,
        ShutdownPhase::Running | ShutdownPhase::Draining => ExitDisposition::KeepWaiting,
        ShutdownPhase::Drained => ExitDisposition::Allow,
    }
}

/// Quit once connections drain: reap the sessions that must not outlive the app, then let
/// queued launch-registry writes reach disk (a blocking wait, so off the async worker),
/// then exit.
pub(crate) async fn drain_then(
    manager: &HostConnectionManager,
    backstop: impl FnOnce(),
    flush_registry: impl FnOnce() + Send + 'static,
    exit: impl FnOnce(),
) {
    manager.drain_connections().await;
    backstop();
    let _ = tokio::task::spawn_blocking(flush_registry).await;
    // Only now may another quit request be allowed: until the registry is flushed the
    // phase stays `Draining`, so a repeat Cmd-Q or window close keeps waiting.
    manager.mark_drained();
    exit();
}

/// A wedged renderer must not hold native quit open.
pub(crate) async fn wait_for_flush(answer: tokio::sync::oneshot::Receiver<()>) {
    let _ = tokio::time::timeout(std::time::Duration::from_millis(500), answer).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[tokio::test]
    async fn renderer_flush_answers_early_or_releases_quit_at_500_ms() {
        let (send, answer) = tokio::sync::oneshot::channel();
        send.send(()).unwrap();
        tokio::time::timeout(Duration::from_millis(50), wait_for_flush(answer))
            .await
            .unwrap();
        let (_send, answer) = tokio::sync::oneshot::channel();
        let started = std::time::Instant::now();
        wait_for_flush(answer).await;
        assert!(started.elapsed() >= Duration::from_millis(500));
        assert!(started.elapsed() < Duration::from_millis(650));
    }

    #[tokio::test]
    async fn exit_is_prevented_until_manager_drain_then_backstop_then_final_exit() {
        let manager = Arc::new(HostConnectionManager::default());
        assert_eq!(request(&manager), ExitDisposition::StartDrain);
        assert_eq!(request(&manager), ExitDisposition::KeepWaiting);
        manager.set_active_operations_for_test(1);
        let order = Arc::new(Mutex::new(Vec::new()));
        let draining = manager.clone();
        let backstop_order = order.clone();
        let exit_order = order.clone();
        let mut task = tokio::spawn(async move {
            drain_then(
                &draining,
                || backstop_order.lock().unwrap().push("backstop"),
                || {},
                || exit_order.lock().unwrap().push("exit"),
            )
            .await;
        });
        assert!(tokio::time::timeout(Duration::ZERO, &mut task)
            .await
            .is_err());
        assert_eq!(request(&manager), ExitDisposition::KeepWaiting);
        assert!(order.lock().unwrap().is_empty());
        manager.set_active_operations_for_test(0);
        task.await.unwrap();
        assert_eq!(&*order.lock().unwrap(), &["backstop", "exit"]);
        assert_eq!(request(&manager), ExitDisposition::Allow);
    }
}
