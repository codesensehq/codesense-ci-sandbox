// SPDX-License-Identifier: MIT
//! PT-5: in-process dispatch surface for OSC 133 semantic prompt markers.
//!
//! OSC 133 events have two consumers with different needs:
//!   - **Internal Rust subscribers** (PT-8's warm-pool prompt handoff) need to
//!     react with no frontend round-trip. They register a callback on the
//!     [`Osc133Bus`] (a managed Tauri state).
//!   - **Frontend** consumers (future exit-status badges, jump-to-prompt) get a
//!     thin `pty-osc133` Tauri event.
//!
//! PT-5 ships the bus + the event emit; PT-8 is the first internal subscriber.

use std::sync::Mutex;

use codesense_pty_core::osc::OscEvent;

/// Which OSC 133 marker fired. Serialized to the frontend as a kebab-case tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Osc133Kind {
    PromptStart,
    CommandStart,
    CommandEnd,
}

/// A dispatched OSC 133 marker for one PTY session.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Osc133Event {
    pub session_id: String,
    pub kind: Osc133Kind,
    /// Exit code, present only for `CommandEnd` when the shell reported one.
    pub exit_code: Option<i32>,
}

impl Osc133Event {
    /// Build the dispatch event for an OSC 133 marker. Returns `None` for OSC 7
    /// (`Cwd`) events, which are not prompt markers and are handled separately.
    pub fn from_osc(session_id: &str, event: &OscEvent) -> Option<Self> {
        let (kind, exit_code) = match event {
            OscEvent::PromptStart => (Osc133Kind::PromptStart, None),
            OscEvent::CommandStart => (Osc133Kind::CommandStart, None),
            OscEvent::CommandEnd(code) => (Osc133Kind::CommandEnd, *code),
            OscEvent::Cwd(_) => return None,
        };
        Some(Self {
            session_id: session_id.to_string(),
            kind,
            exit_code,
        })
    }
}

/// A callback invoked synchronously for every published [`Osc133Event`].
pub type Osc133Listener = Box<dyn Fn(&Osc133Event) + Send + Sync>;

/// Fan-out registry for OSC 133 events. Managed as Tauri app state so any
/// backend subsystem can subscribe without an IPC hop (PT-5 AC-10).
#[derive(Default)]
pub struct Osc133Bus {
    listeners: Mutex<Vec<Osc133Listener>>,
}

impl Osc133Bus {
    /// Register an internal subscriber. Called once at startup by consumers
    /// such as PT-8's warm pool and the weft OSC-133 relay.
    pub fn subscribe(&self, listener: Osc133Listener) {
        if let Ok(mut ls) = self.listeners.lock() {
            ls.push(listener);
        }
    }

    /// Notify every registered subscriber. A poisoned lock is skipped rather
    /// than panicking the batcher thread that drives dispatch.
    pub fn publish(&self, event: &Osc133Event) {
        if let Ok(ls) = self.listeners.lock() {
            for listener in ls.iter() {
                listener(event);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn subscribers_receive_published_events_in_order() {
        let bus = Osc133Bus::default();
        let seen: Arc<Mutex<Vec<Osc133Kind>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        bus.subscribe(Box::new(move |ev| sink.lock().unwrap().push(ev.kind)));

        for kind in [
            Osc133Kind::PromptStart,
            Osc133Kind::CommandStart,
            Osc133Kind::CommandEnd,
        ] {
            bus.publish(&Osc133Event {
                session_id: "s1".into(),
                kind,
                exit_code: (kind == Osc133Kind::CommandEnd).then_some(0),
            });
        }

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                Osc133Kind::PromptStart,
                Osc133Kind::CommandStart,
                Osc133Kind::CommandEnd,
            ]
        );
    }

    #[test]
    fn publish_with_no_subscribers_is_a_noop() {
        let bus = Osc133Bus::default();
        bus.publish(&Osc133Event {
            session_id: "s1".into(),
            kind: Osc133Kind::PromptStart,
            exit_code: None,
        });
    }

    /// A real zsh with the shipped integration, run by the in-process host:
    /// the `pty-osc133` payloads its prompt markers become carry the tab's
    /// session id and each foreground command's real status — `1` for a
    /// failure, `137` for a `kill -9` — and nothing for a suspended job, whose
    /// `fg` then reports how it ended.
    #[cfg(unix)]
    #[test]
    fn a_real_shells_command_ends_reach_pty_osc133_with_their_status() {
        use codesense_pty_core::domain::{PtySessionId, TerminalDomain, TerminalDomainEvent};
        use codesense_pty_core::layout::ServerIdentity;
        use codesense_pty_core::runner::{LocalPtyRunner, PtyRunner};
        use codesense_pty_core::spawn::{HostEnvironment, SpawnIntent};
        use codesense_pty_core::TerminalHost;
        use codesense_session_server::testing::{registration, wait_until, RecordingSink};

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let shipped = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources");
        let host_env = HostEnvironment::with_vars(
            Some(tmp.path().join("cs")),
            Some(shipped),
            [
                ("HOME", home.to_str().unwrap().to_string()),
                ("PATH", std::env::var("PATH").unwrap_or_default()),
            ]
            .map(|(k, v)| (k.to_string(), v)),
        );
        let spec = SpawnIntent {
            remote_launch: Default::default(),
            command: None,
            argv: None,
            shell: Some("/bin/zsh".into()),
            cwd: tmp.path().to_string_lossy().into_owned(),
            cols: 120,
            rows: 40,
            shell_integration: true,
            starship_preset: Some("plain".into()),
            api_port: None,
            env_remove: Vec::new(),
            env: Vec::new(),
            agent_env: Vec::new(),
            login_path: None,
        }
        .resolve("tab-7", &host_env);

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let events = Arc::new(RecordingSink::default());
        let host = TerminalHost::new(
            ServerIdentity::ephemeral("test"),
            events.clone(),
            runtime.handle().clone(),
        );
        let pty = runtime.block_on(LocalPtyRunner.spawn(spec)).unwrap();
        let mut reg = registration("tab-7", None, false);
        reg.osc_enabled = true;
        host.register(pty, reg).unwrap();
        let tab = PtySessionId::new("tab-7");

        // What the app sink emits as `pty-osc133` for every prompt marker.
        let payloads = || -> Vec<serde_json::Value> {
            events
                .events()
                .into_iter()
                .filter_map(|event| match event {
                    TerminalDomainEvent::PromptMarker { session, marker } => {
                        Osc133Event::from_osc(session.as_str(), &marker)
                    }
                    _ => None,
                })
                .map(|event| serde_json::to_value(event).unwrap())
                .collect()
        };
        let prompts = || {
            payloads()
                .iter()
                .filter(|p| p["kind"] == "prompt-start")
                .count()
        };
        let output = || events.output("tab-7");
        let type_then_prompt = |bytes: &[u8]| {
            let before = prompts();
            host.write(&tab, bytes).unwrap();
            assert!(
                wait_until(|| prompts() > before),
                "no prompt after {bytes:?}: {:?}",
                output()
            );
        };
        assert!(
            wait_until(|| prompts() >= 1),
            "no first prompt: {:?}",
            output()
        );

        type_then_prompt(b"false\r");
        type_then_prompt(b"sh -c 'kill -9 $$'\r");
        host.write(&tab, b"sleep 30\r").unwrap();
        assert!(wait_until(|| output().contains("sleep 30")));
        std::thread::sleep(std::time::Duration::from_millis(300));
        type_then_prompt(b"\x1a");
        host.write(&tab, b"fg\r").unwrap();
        assert!(wait_until(|| output().matches("fg").count() >= 1));
        std::thread::sleep(std::time::Duration::from_millis(300));
        type_then_prompt(b"\x03");

        let ends: Vec<serde_json::Value> = payloads()
            .into_iter()
            .filter(|p| p["kind"] == "command-end")
            .collect();
        let expect = |code: i32| serde_json::json!({ "session_id": "tab-7", "kind": "command-end", "exit_code": code });
        assert_eq!(
            ends,
            [expect(1), expect(137), expect(130)],
            "{:?}",
            output()
        );
        host.kill(&tab).unwrap();
    }
}
