// SPDX-License-Identifier: MIT
//! SW-AGR-14: keep the app out of App Nap while remote machines are connected.
//!
//! A hidden window makes the app App-Nap-eligible (see the close handler in
//! `lib.rs`), and App Nap can delay its timers by minutes. A machine reaps a
//! desktop link after 180 s without heartbeats (SW-AGR-11), so delayed
//! heartbeats could drop a healthy link. While at least one remote machine is
//! connected the app holds an `NSProcessInfo` activity assertion that rules out
//! App Nap but still lets the system sleep; it releases the assertion when none
//! are connected. Elsewhere than macOS the port is a no-op.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use super::host_connection_manager::{HostConnectionManager, HostConnectionState};
use super::machines::LOCAL_MACHINE_ID;

/// Begins and ends the one process activity assertion.
pub(crate) trait ActivityPort: Send + Sync {
    fn begin(&self);
    fn end(&self);
}

#[derive(Default)]
struct GuardState {
    connected: BTreeSet<String>,
    held: bool,
}

/// Holds the assertion while any remote machine is connected.
pub(crate) struct AppNapGuard<P: ActivityPort> {
    port: P,
    state: Mutex<GuardState>,
}

impl<P: ActivityPort> AppNapGuard<P> {
    pub(crate) fn new(port: P) -> Self {
        Self {
            port,
            state: Mutex::new(GuardState::default()),
        }
    }

    /// Record a machine's connection state and begin or end the assertion.
    pub(crate) fn observe(&self, machine_id: &str, state: &HostConnectionState) {
        if machine_id == LOCAL_MACHINE_ID {
            return;
        }
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(state, HostConnectionState::Connected { .. }) {
            guard.connected.insert(machine_id.to_string());
        } else {
            guard.connected.remove(machine_id);
        }
        // Ports run under the lock so begin and end never reorder.
        match (guard.connected.is_empty(), guard.held) {
            (false, false) => {
                self.port.begin();
                guard.held = true;
            }
            (true, true) => {
                self.port.end();
                guard.held = false;
            }
            _ => {}
        }
    }
}

/// Subscribe a guard to the connection manager's events.
pub(crate) fn install(manager: &HostConnectionManager) {
    let guard = Arc::new(AppNapGuard::new(platform_port()));
    manager.subscribe(Arc::new(move |event| {
        guard.observe(&event.machine_id, &event.state);
    }));
}

#[cfg(target_os = "macos")]
fn platform_port() -> mac::ProcessActivity {
    mac::ProcessActivity::default()
}

#[cfg(not(target_os = "macos"))]
fn platform_port() -> NoopActivity {
    NoopActivity
}

#[cfg(not(target_os = "macos"))]
struct NoopActivity;

#[cfg(not(target_os = "macos"))]
impl ActivityPort for NoopActivity {
    fn begin(&self) {}
    fn end(&self) {}
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::Mutex;

    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

    use super::ActivityPort;

    /// The token `beginActivityWithOptions:reason:` returns.
    struct Activity(Retained<ProtocolObject<dyn NSObjectProtocol>>);

    // SAFETY: the token is an opaque object Foundation only reads back in
    // `endActivity:`; `NSProcessInfo` is thread-safe, and the token is only
    // touched under the mutex below.
    unsafe impl Send for Activity {}

    #[derive(Default)]
    pub(super) struct ProcessActivity {
        activity: Mutex<Option<Activity>>,
    }

    impl ActivityPort for ProcessActivity {
        fn begin(&self) {
            let mut activity = self.activity.lock().unwrap_or_else(|e| e.into_inner());
            if activity.is_some() {
                return;
            }
            let reason = NSString::from_str("Keeping remote machine connections alive");
            // Rules out App Nap without keeping the system awake.
            let token = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
                NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
                &reason,
            );
            *activity = Some(Activity(token));
        }

        fn end(&self) {
            let taken = self
                .activity
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(Activity(token)) = taken {
                // SAFETY: `token` came from `beginActivityWithOptions:reason:`
                // and is ended exactly once.
                unsafe { NSProcessInfo::processInfo().endActivity(&token) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default, Clone)]
    struct RecordingPort(Arc<Mutex<Vec<&'static str>>>);

    impl RecordingPort {
        fn calls(&self) -> Vec<&'static str> {
            self.0.lock().unwrap().clone()
        }
    }

    impl ActivityPort for RecordingPort {
        fn begin(&self) {
            self.0.lock().unwrap().push("begin");
        }
        fn end(&self) {
            self.0.lock().unwrap().push("end");
        }
    }

    fn connected() -> HostConnectionState {
        HostConnectionState::Connected {
            server_id: "server".into(),
        }
    }

    fn guard() -> (AppNapGuard<RecordingPort>, RecordingPort) {
        let port = RecordingPort::default();
        (AppNapGuard::new(port.clone()), port)
    }

    #[test]
    fn begins_on_first_connect_and_ends_on_last_disconnect() {
        let (guard, port) = guard();
        guard.observe("m1", &connected());
        assert_eq!(port.calls(), ["begin"]);
        guard.observe("m2", &connected());
        guard.observe("m1", &HostConnectionState::Disconnected);
        assert_eq!(port.calls(), ["begin"], "m2 is still connected");
        guard.observe("m2", &HostConnectionState::Disconnected);
        assert_eq!(port.calls(), ["begin", "end"]);
    }

    #[test]
    fn repeated_events_are_idempotent() {
        let (guard, port) = guard();
        guard.observe("m1", &HostConnectionState::Disconnected);
        assert!(port.calls().is_empty(), "nothing to release");
        guard.observe("m1", &connected());
        guard.observe("m1", &connected());
        assert_eq!(port.calls(), ["begin"]);
        guard.observe("m1", &HostConnectionState::Disconnected);
        guard.observe("m1", &HostConnectionState::Disconnected);
        assert_eq!(port.calls(), ["begin", "end"]);
        guard.observe("m1", &connected());
        assert_eq!(port.calls(), ["begin", "end", "begin"]);
    }

    #[test]
    fn any_state_other_than_connected_counts_as_not_connected() {
        let (guard, port) = guard();
        guard.observe("m1", &connected());
        guard.observe(
            "m1",
            &HostConnectionState::Unreachable {
                last_error: "timed out".into(),
            },
        );
        assert_eq!(port.calls(), ["begin", "end"]);
        guard.observe("m1", &HostConnectionState::Connecting);
        guard.observe("m1", &HostConnectionState::Handshaking);
        assert_eq!(port.calls(), ["begin", "end"]);
    }

    #[test]
    fn the_local_machine_never_holds_the_assertion() {
        let (guard, port) = guard();
        guard.observe(LOCAL_MACHINE_ID, &connected());
        assert!(port.calls().is_empty());
    }

    #[test]
    fn the_platform_port_tolerates_repeated_begin_and_end() {
        // The production port is safe to drive in a test: on macOS it takes
        // and releases a real assertion, elsewhere it does nothing.
        let port = platform_port();
        port.begin();
        port.begin();
        port.end();
        port.end();
    }
}
