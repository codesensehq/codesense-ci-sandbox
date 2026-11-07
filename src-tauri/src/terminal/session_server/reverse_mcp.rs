// SPDX-License-Identifier: MIT
//! Bounded reverse streams on one captured SessionServerClient.
use super::{lock, SessionServerClient};
use codesense_host_proto::{reverse_mcp as wire, Frame, Kind};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{mpsc, oneshot};

type Serve = Arc<dyn Fn(String, DuplexStream) + Send + Sync>;
/// SW-AGR-8: serves the CodeSense MCP of an agent the machine started for a
/// routine while this app was connected, by its session (`None`: refused).
pub(crate) type RoutineBinder = Arc<dyn Fn(&str) -> Option<Serve> + Send + Sync>;
/// The secret the machine keys those sessions' binding nonces with, and how
/// to serve them.
struct RoutineBindings {
    secret: String,
    binder: RoutineBinder,
    runtime: tokio::runtime::Handle,
}
struct Binding {
    nonce: String,
    serve: Serve,
    server_ids: Vec<String>,
    /// The runtime the binding was prepared on; its stream pumps run there.
    runtime: tokio::runtime::Handle,
}
struct Stream {
    session: String,
    sender: mpsc::Sender<Vec<u8>>,
    _cancel: oneshot::Sender<()>,
}
#[derive(Default)]
pub(crate) struct ReverseStreams {
    bindings: Mutex<HashMap<String, Binding>>,
    streams: Mutex<HashMap<u32, Stream>>,
    routines: Mutex<Option<RoutineBindings>>,
    /// SW-AGR-11: the desktop identity this connection was bound with.
    pub(crate) bound_desktop: Mutex<Option<String>>,
}
impl ReverseStreams {
    pub(crate) fn bind(
        &self,
        session: &str,
        nonce: String,
        server_ids: Vec<String>,
        serve: Serve,
        runtime: tokio::runtime::Handle,
    ) -> Result<(), String> {
        let mut bindings = lock(&self.bindings);
        if let Some(existing) = bindings.get(session) {
            return if existing.nonce == nonce && existing.server_ids == server_ids {
                Ok(())
            } else {
                Err("reverse MCP binding is immutable".into())
            };
        }
        bindings.insert(
            session.into(),
            Binding {
                nonce,
                serve,
                server_ids,
                runtime,
            },
        );
        Ok(())
    }
    pub(crate) fn unbind(&self, session: &str) {
        lock(&self.bindings).remove(session);
        lock(&self.streams).retain(|_, stream| stream.session != session);
    }
    pub(crate) fn retire(&self) {
        lock(&self.bindings).clear();
        lock(&self.streams).clear();
        *lock(&self.routines) = None;
        *lock(&self.bound_desktop) = None;
    }

    /// SW-AGR-8: accept the CodeSense MCP of agents the machine starts for
    /// routines, whose binding nonce it keys with `secret` (which this app
    /// sent it): such a session is bound on its first stream, served by
    /// `binder`. Once per connection.
    pub(crate) fn accept_routine_bindings(
        &self,
        secret: String,
        binder: RoutineBinder,
        runtime: tokio::runtime::Handle,
    ) {
        *lock(&self.routines) = Some(RoutineBindings {
            secret,
            binder,
            runtime,
        });
    }

    /// Whether routine bindings are accepted on this connection yet.
    pub(crate) fn accepts_routine_bindings(&self) -> bool {
        lock(&self.routines).is_some()
    }

    /// A binding for `open`, a machine-started routine agent's CodeSense
    /// stream, made now if its nonce is the one keyed by this app's secret.
    fn routine_binding(&self, open: &wire::Open) -> Option<(Serve, tokio::runtime::Handle)> {
        if open.server_id != wire::CODESENSE_SERVER {
            return None;
        }
        let (serve, runtime) = {
            let routines = lock(&self.routines);
            let routines = routines.as_ref()?;
            let expected = codesense_pty_core::routine_arm::routine_binding_nonce(
                &routines.secret,
                &open.session,
            );
            if expected != open.binding_nonce {
                return None;
            }
            ((routines.binder)(&open.session)?, routines.runtime.clone())
        };
        lock(&self.bindings).insert(
            open.session.clone(),
            Binding {
                nonce: open.binding_nonce.clone(),
                serve: serve.clone(),
                server_ids: vec![wire::CODESENSE_SERVER.to_string()],
                runtime: runtime.clone(),
            },
        );
        Some((serve, runtime))
    }
    pub(crate) fn receive(self: &Arc<Self>, client: &Arc<SessionServerClient>, frame: Frame) {
        if client
            .private_lease()
            .is_none_or(|lease| lease.validate().is_err())
        {
            self.retire();
            return;
        }
        match frame.kind {
            Kind::MCP_OPEN => {
                let Ok(open) = frame.decode_json::<wire::Open>() else {
                    self.close(client, frame.channel);
                    return;
                };
                // One lock per statement: the routine binding below takes it
                // again (std mutexes do not re-enter).
                let (bound, unbound) = {
                    let bindings = lock(&self.bindings);
                    let bound = bindings
                        .get(&open.session)
                        .filter(|b| {
                            b.nonce == open.binding_nonce && b.server_ids.contains(&open.server_id)
                        })
                        .map(|b| (b.serve.clone(), b.runtime.clone()));
                    (bound, !bindings.contains_key(&open.session))
                };
                let binding =
                    bound.or_else(|| unbound.then(|| self.routine_binding(&open)).flatten());
                let Some((serve, runtime)) = binding else {
                    self.close(client, frame.channel);
                    return;
                };
                let (sender, mut receiver) = mpsc::channel::<Vec<u8>>(wire::QUEUE_CHUNKS);
                let (cancel, cancelled) = oneshot::channel();
                {
                    let mut streams = lock(&self.streams);
                    if frame.channel == 0
                        || streams.len() >= wire::MAX_STREAMS
                        || streams.contains_key(&frame.channel)
                    {
                        drop(streams);
                        self.close(client, frame.channel);
                        return;
                    }
                    streams.insert(
                        frame.channel,
                        Stream {
                            session: open.session,
                            sender,
                            _cancel: cancel,
                        },
                    );
                }
                let (service, transport) = tokio::io::duplex(wire::CHUNK_BYTES);
                // Downstream preparation runs on its captured runtime, never the host reader.
                runtime.spawn(async move {
                    serve(open.server_id, service);
                });
                let (mut reader, mut writer) = tokio::io::split(transport);
                let weak_client = Arc::downgrade(client);
                let weak = Arc::downgrade(self);
                let channel = frame.channel;
                runtime.spawn(async move {
                    let inbound = async {
                        while let Some(bytes) = receiver.recv().await {
                            writer.write_all(&bytes).await.map_err(|e| e.to_string())?;
                        }
                        Ok::<(), String>(())
                    };
                    let outbound = async {
                        let mut bytes = vec![0; wire::CHUNK_BYTES];
                        loop {
                            let n = reader.read(&mut bytes).await.map_err(|e| e.to_string())?;
                            if n == 0 {
                                return Ok::<(), String>(());
                            }
                            send(
                                &weak_client,
                                Frame {
                                    kind: Kind::MCP_DATA,
                                    channel,
                                    payload: bytes[..n].into(),
                                },
                            )?;
                        }
                    };
                    tokio::select! {_=inbound=>{},_=outbound=>{},_=cancelled=>{}}
                    if let (Some(this), Some(client)) = (weak.upgrade(), weak_client.upgrade()) {
                        this.close(&client, channel);
                    }
                });
            }
            Kind::MCP_DATA => {
                let sender = lock(&self.streams)
                    .get(&frame.channel)
                    .map(|s| s.sender.clone());
                if frame.payload.len() > wire::CHUNK_BYTES
                    || sender.is_none_or(|sender| sender.try_send(frame.payload).is_err())
                {
                    self.close(client, frame.channel);
                }
            }
            Kind::MCP_CLOSE => {
                lock(&self.streams).remove(&frame.channel);
            }
            _ => {}
        }
    }
    fn close(&self, client: &SessionServerClient, channel: u32) {
        lock(&self.streams).remove(&channel);
        let _ = client.writer.try_send(&Frame {
            kind: Kind::MCP_CLOSE,
            channel,
            payload: vec![],
        });
    }
}
fn send(client: &Weak<SessionServerClient>, frame: Frame) -> Result<(), String> {
    let client = client.upgrade().ok_or("remote connection ended")?;
    client
        .writer
        .try_send(&frame)
        .map_err(|_| "remote bridge backpressure or disconnect; outcome unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reverse_server_binding_is_immutable_and_unbind_cancels_only_its_streams() {
        let streams = ReverseStreams::default();
        let serve: Serve = Arc::new(|_, _| {});
        let runtime = tokio::runtime::Handle::current();
        let ids = vec!["codesense".into(), "selected".into()];
        streams
            .bind(
                "session",
                "nonce".into(),
                ids.clone(),
                serve.clone(),
                runtime.clone(),
            )
            .unwrap();
        streams
            .bind(
                "session",
                "nonce".into(),
                ids,
                serve.clone(),
                runtime.clone(),
            )
            .unwrap();
        assert!(streams
            .bind(
                "session",
                "nonce".into(),
                vec!["different".into()],
                serve,
                runtime
            )
            .is_err());
        let (sender, _recv) = mpsc::channel(1);
        let (cancel, cancelled) = oneshot::channel();
        lock(&streams.streams).insert(
            1,
            Stream {
                session: "session".into(),
                sender,
                _cancel: cancel,
            },
        );
        let (sender, _recv2) = mpsc::channel(1);
        let (cancel, mut retained) = oneshot::channel();
        lock(&streams.streams).insert(
            2,
            Stream {
                session: "other".into(),
                sender,
                _cancel: cancel,
            },
        );
        streams.unbind("session");
        assert!(cancelled.await.is_err());
        assert_eq!(
            retained.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        );
        assert_eq!(lock(&streams.streams).len(), 1);
    }

    // SW-AGR-8 (SHOULD 1): an agent the machine started for a routine while
    // this app was connected is bound on its first CodeSense stream, only
    // with the nonce keyed by this app's secret; another server, or another
    // nonce, is refused.
    #[tokio::test]
    async fn a_machine_started_routine_agent_binds_only_with_the_keyed_nonce() {
        let streams = ReverseStreams::default();
        let asked = Arc::new(Mutex::new(Vec::<String>::new()));
        let binder: RoutineBinder = {
            let asked = asked.clone();
            Arc::new(move |session: &str| {
                lock(&asked).push(session.to_string());
                let serve: Serve = Arc::new(|_, _| {});
                Some(serve)
            })
        };
        let open = |nonce: String, server: &str| wire::Open {
            session: "routine-abc".into(),
            binding_nonce: nonce,
            server_id: server.into(),
        };
        let keyed =
            codesense_pty_core::routine_arm::routine_binding_nonce(&"s".repeat(64), "routine-abc");
        assert!(
            streams
                .routine_binding(&open(keyed.clone(), "codesense"))
                .is_none(),
            "not accepted yet"
        );
        streams.accept_routine_bindings("s".repeat(64), binder, tokio::runtime::Handle::current());
        assert!(streams
            .routine_binding(&open("f".repeat(64), "codesense"))
            .is_none());
        assert!(streams
            .routine_binding(&open(keyed.clone(), "github"))
            .is_none());
        assert!(streams
            .routine_binding(&open(keyed.clone(), "codesense"))
            .is_some());
        assert_eq!(*lock(&asked), ["routine-abc"]);
        assert_eq!(lock(&streams.bindings)["routine-abc"].nonce, keyed);
        streams.retire();
        assert!(!streams.accepts_routine_bindings());
    }
}
