// SPDX-License-Identifier: MIT
//! The Tauri-free terminal domain contract: the identities, lifecycle
//! axes and ports ([`types`]), the registry seam ([`registry`]) and the session
//! runtime's coordination protocols ([`runtime`]).

pub(crate) mod registry;
pub(crate) mod runtime;
pub mod types;

pub(crate) use registry::{AttachOutcome, SessionView, TerminalRegistry};
pub use runtime::{is_blank_agent_input, SubscribeResult};
pub(crate) use runtime::{MonotonicClock, PtySessionRuntime};
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::RecordingSink;

    // ---- Fixtures: the ports are implementable with zero Tauri ----------

    struct FakeClock {
        millis: u64,
    }
    impl ClockPort for FakeClock {
        fn now_millis(&self) -> u64 {
            self.millis
        }
    }

    #[derive(Default)]
    struct MemPersistence {
        blobs: std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>,
    }
    impl SessionPersistencePort for MemPersistence {
        fn persist(&self, session: &PtySessionId, snapshot: &[u8]) -> Result<(), PortUnavailable> {
            self.blobs
                .lock()
                .unwrap()
                .insert(session.as_str().to_string(), snapshot.to_vec());
            Ok(())
        }
        fn load(&self, session: &PtySessionId) -> Result<Option<Vec<u8>>, PortUnavailable> {
            Ok(self.blobs.lock().unwrap().get(session.as_str()).cloned())
        }
    }

    struct AllAlive;
    impl ProcessInspector for AllAlive {
        fn is_alive(&self, _pid: u32) -> bool {
            true
        }
    }

    #[test]
    fn ports_and_domain_are_implementable_without_tauri() {
        let clock = FakeClock { millis: 42 };
        assert_eq!(clock.now_millis(), 42);

        let sink = std::sync::Arc::new(RecordingSink::default());
        sink.emit(TerminalDomainEvent::Output {
            session: PtySessionId::new("pty-1"),
            data: "hi".to_string(),
            seq: 7,
        })
        .unwrap();
        assert_eq!(sink.events.lock().unwrap().len(), 1);

        // The real host, built with no `AppHandle` and no command DTO, is the
        // domain: a missing session is an error carrying the registry's own
        // text, reads idle, and a kill of it is a no-op.
        let host = crate::TerminalHost::new(
            crate::layout::ServerIdentity::ephemeral("test"),
            sink,
            crate::fixtures::fixture_runtime(),
        );
        let domain: &dyn TerminalDomain = &host;
        let gone = PtySessionId::new("gone");
        assert_eq!(
            domain.write(&gone, b"x"),
            Err(PortUnavailable("write_pty: session not found: gone".into()))
        );
        assert_eq!(
            domain
                .subscribe(&gone, &ViewportId::new("vp-1"))
                .map(|_| ()),
            Err(PortUnavailable(
                "subscribe_terminal: session not found: gone".into()
            ))
        );
        assert_eq!(domain.has_session(&gone), Ok(false));
        assert_eq!(domain.session_cwd(&gone), Ok(None));
        assert_eq!(
            domain.unsubscribe(crate::term_state::StreamId(u64::MAX)),
            Ok(false)
        );
        assert!(domain.is_idle(&gone).unwrap());
        domain.kill(&gone).unwrap();
        let snapshot = domain
            .layout_snapshot(&ViewerId::new("viewer"), &ClientBootId::new("boot"))
            .unwrap();
        assert!(snapshot.workspaces.is_empty());
    }

    #[test]
    fn every_runtime_port_is_composable_and_tauri_free() {
        // Exercises the ports the domain-trait test does not — persistence, process
        // inspection, clock — through the composed bundle the host
        // wires, proving each is implementable with zero Tauri.
        let ports = TerminalRuntimePorts {
            events: std::sync::Arc::new(RecordingSink::default()),
            persistence: std::sync::Arc::new(MemPersistence::default()),
            clock: std::sync::Arc::new(FakeClock { millis: 5 }),
            process: std::sync::Arc::new(AllAlive),
        };

        let session = PtySessionId::new("pty-1");
        ports.persistence.persist(&session, b"snapshot").unwrap();
        assert_eq!(
            ports.persistence.load(&session).unwrap().as_deref(),
            Some(&b"snapshot"[..])
        );
        assert!(ports
            .persistence
            .load(&PtySessionId::new("absent"))
            .unwrap()
            .is_none());
        assert!(ports.process.is_alive(123));
        assert_eq!(ports.clock.now_millis(), 5);
    }

    // ---- Identities are distinct + wire-transparent ---------------------

    #[test]
    fn newtype_identities_are_serde_transparent() {
        let session = PtySessionId::new("pty-9");
        assert_eq!(session.as_str(), "pty-9");
        assert_eq!(session.to_string(), "pty-9");
        // #[serde(transparent)] ⇒ the wire form is the bare string (compat).
        assert_eq!(serde_json::to_string(&session).unwrap(), "\"pty-9\"");
        let back: PtySessionId = serde_json::from_str("\"pty-9\"").unwrap();
        assert_eq!(back, session);
    }

    // ---- Lifecycle axes: transitions + independence ---------------------

    #[test]
    fn pty_transitions_enforce_the_table() {
        assert!(PtyLifecyclePhase::Spawn
            .validate(PtyLifecyclePhase::Ready)
            .is_ok());
        let err = PtyLifecyclePhase::Spawn
            .validate(PtyLifecyclePhase::Active)
            .unwrap_err();
        assert_eq!(err.axis, "pty");
        assert_eq!(err.from, "spawn");
        assert_eq!(err.to, "active");
        // terminated is absorbing.
        assert!(PtyLifecyclePhase::Terminated
            .validate(PtyLifecyclePhase::Ready)
            .is_err());
    }

    #[test]
    fn viewport_transitions_enforce_the_table_and_teardown_from_anywhere() {
        assert!(ViewportLifecyclePhase::Attach
            .validate(ViewportLifecyclePhase::Ready)
            .is_ok());
        assert!(ViewportLifecyclePhase::Hidden
            .validate(ViewportLifecyclePhase::Restored)
            .is_ok());
        for from in [
            ViewportLifecyclePhase::Attach,
            ViewportLifecyclePhase::Ready,
            ViewportLifecyclePhase::Active,
            ViewportLifecyclePhase::Hidden,
            ViewportLifecyclePhase::Detached,
            ViewportLifecyclePhase::Restored,
        ] {
            assert!(from.validate(ViewportLifecyclePhase::Terminated).is_ok());
        }
    }
}
