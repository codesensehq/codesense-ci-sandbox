// SPDX-License-Identifier: MIT
//! Ownership boundary for a remote terminal transport.
//!
//! RM-3 hands this module one indivisible pinned SSH transport.  RM-4's later
//! admission slices may inspect it only inside this module; callers cannot
//! recombine byte streams with another machine's identity or process guard.

use crate::commands::auth::private_authority::PrivateAuthorityLease;
use std::io::{BufReader, Write};
use std::process::{ChildStdin, ChildStdout};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use codesense_host_proto::{
    Capability, Census, FrozenHostHello, Hello, HostGreeting, Skew, SkewReason,
};
use codesense_pty_core::protocol::client_hello;

use super::machines::{PinnedSshTransport, PinnedTransportGenerationError, SshProcessGuard};
use crate::terminal::session_server::{WorkerCompletion, WriteMailbox};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

trait BlockingReaper: Send {
    fn close_and_reap(self: Box<Self>) -> Result<(), String>;
}

#[cfg(test)]
pub(crate) struct TestCloseGate {
    entered: std::sync::atomic::AtomicBool,
    released: Mutex<bool>,
    changed: std::sync::Condvar,
}

#[cfg(test)]
impl TestCloseGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: std::sync::atomic::AtomicBool::new(false),
            released: Mutex::new(false),
            changed: std::sync::Condvar::new(),
        })
    }

    pub(crate) fn entered(&self) -> bool {
        self.entered.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn release(&self) {
        *lock(&self.released) = true;
        self.changed.notify_all();
    }
}

#[cfg(test)]
struct GatedRealReaper {
    guard: SshProcessGuard,
    gate: Arc<TestCloseGate>,
}

#[cfg(test)]
impl BlockingReaper for GatedRealReaper {
    fn close_and_reap(self: Box<Self>) -> Result<(), String> {
        self.gate
            .entered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let mut released = lock(&self.gate.released);
        while !*released {
            released = self
                .gate
                .changed
                .wait(released)
                .unwrap_or_else(|error| error.into_inner());
        }
        drop(released);
        self.guard.close_and_reap()
    }
}

trait BlockingWorkerExit: Send {
    fn wait(self: Box<Self>);
}

impl BlockingWorkerExit for WorkerCompletion {
    fn wait(self: Box<Self>) {
        WorkerCompletion::wait(&self);
    }
}

impl BlockingReaper for SshProcessGuard {
    fn close_and_reap(self: Box<Self>) -> Result<(), String> {
        (*self).close_and_reap()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CloseReport {
    pub reap: Result<(), String>,
    pub worker_exited: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CloseWaitTimedOut;

enum CloseState {
    Open(Option<Box<dyn BlockingReaper>>),
    Closing { diagnostic: Option<String> },
    Closed(Arc<CloseReport>),
}

struct LifecycleInner {
    // Construction occurs on the transport attempt's runtime. Synchronous UI/
    // writer callbacks may close from another thread; the app must keep this
    // runtime executing until its owned shutdown drain completes.
    runtime: tokio::runtime::Handle,
    state: Mutex<CloseState>,
    worker: Mutex<Option<Box<dyn BlockingWorkerExit>>>,
    completed: Notify,
}

#[derive(Clone)]
struct PinnedTransportLifecycle(Arc<LifecycleInner>);

/// Capability-limited authority to close and observe one pinned SSH process.
/// It deliberately exposes no stream, identity, or generation material.
#[derive(Clone)]
pub(crate) struct RemoteCloseHandle {
    lifecycle: PinnedTransportLifecycle,
    mailbox: Arc<Mutex<Option<Arc<WriteMailbox>>>>,
}

impl RemoteCloseHandle {
    pub(crate) fn begin_close(&self) {
        let mailbox = lock(&self.mailbox).clone();
        self.lifecycle.begin_close(mailbox);
    }

    pub(crate) async fn close_awaited(&self) -> Arc<CloseReport> {
        self.begin_close();
        loop {
            if let Ok(report) = self.lifecycle.wait_closed(Duration::from_secs(30)).await {
                return report;
            }
        }
    }

    pub(crate) fn register_writer(&self, mailbox: Arc<WriteMailbox>) {
        self.lifecycle.set_worker(mailbox.completion());
        *lock(&self.mailbox) = Some(mailbox);
    }
}

impl PinnedTransportLifecycle {
    fn new(guard: impl BlockingReaper + 'static) -> Self {
        Self(Arc::new(LifecycleInner {
            runtime: tokio::runtime::Handle::current(),
            state: Mutex::new(CloseState::Open(Some(Box::new(guard)))),
            worker: Mutex::new(None),
            completed: Notify::new(),
        }))
    }

    /// Begin normal close exactly once. The blocking task owns the guard
    /// before mailbox producers are dropped, so caller cancellation cannot
    /// strand it or fall back to guard Drop.
    fn set_worker(&self, worker: WorkerCompletion) {
        self.set_worker_exit(worker);
    }

    fn set_worker_exit(&self, worker: impl BlockingWorkerExit + 'static) {
        *lock(&self.0.worker) = Some(Box::new(worker));
    }

    fn begin_close(&self, mailbox: Option<Arc<WriteMailbox>>) {
        let guard = {
            let mut state = lock(&self.0.state);
            match &mut *state {
                CloseState::Open(guard) => {
                    let guard = guard.take().expect("open lifecycle owns its guard");
                    *state = CloseState::Closing { diagnostic: None };
                    guard
                }
                CloseState::Closing { .. } | CloseState::Closed(_) => return,
            }
        };

        // Creating this handle transfers the guard to an independently owned
        // blocking task synchronously, before any producer is dropped.
        let worker = lock(&self.0.worker).take();
        let completion = self.0.runtime.spawn_blocking(move || {
            let reap = guard.close_and_reap();
            if let Some(worker) = worker {
                worker.wait();
            }
            CloseReport {
                reap,
                worker_exited: true,
            }
        });
        if let Some(mailbox) = &mailbox {
            mailbox.close();
        }

        let lifecycle = self.clone();
        self.0.runtime.spawn(async move {
            match completion.await {
                Ok(report) => {
                    *lock(&lifecycle.0.state) = CloseState::Closed(Arc::new(report));
                    lifecycle.0.completed.notify_waiters();
                }
                Err(error) => {
                    let diagnostic = format!("remote close completion task failed: {error}");
                    log::error!(target: "pty", "{diagnostic}");
                    if let CloseState::Closing {
                        diagnostic: current,
                    } = &mut *lock(&lifecycle.0.state)
                    {
                        *current = Some(diagnostic);
                    }
                }
            }
        });
    }

    async fn wait_closed(&self, timeout: Duration) -> Result<Arc<CloseReport>, CloseWaitTimedOut> {
        let wait = async {
            loop {
                let notified = self.0.completed.notified();
                if let CloseState::Closed(report) = &*lock(&self.0.state) {
                    return report.clone();
                }
                notified.await;
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| CloseWaitTimedOut)
    }

    /// Emergency abandonment is runtime-free. RM-3's guard Drop performs its
    /// synchronous kill, bounded poll, and eventual detached wait.
    fn drop_open_guard_now(&self) {
        let guard = {
            let mut state = lock(&self.0.state);
            match &mut *state {
                CloseState::Open(guard) => guard.take(),
                CloseState::Closing { .. } | CloseState::Closed(_) => None,
            }
        };
        drop(guard);
    }

    #[cfg(test)]
    fn phase(&self) -> &'static str {
        match &*lock(&self.0.state) {
            CloseState::Open(_) => "open",
            CloseState::Closing { .. } => "closing",
            CloseState::Closed(_) => "closed",
        }
    }

    #[cfg(test)]
    fn diagnostic(&self) -> Option<String> {
        match &*lock(&self.0.state) {
            CloseState::Closing { diagnostic } => diagnostic.clone(),
            CloseState::Open(_) | CloseState::Closed(_) => None,
        }
    }
}

/// The sole owner of RM-3's pinned remote handoff. Its halves, guard,
/// expected ServerId and opaque generation cannot be supplied independently.
pub struct RemoteTransportOwner {
    private_lease: Option<PrivateAuthorityLease>,
    halves: Mutex<Option<(ChildStdout, ChildStdin)>>,
    expected_server_id: String,
    generation: String,
    lifecycle: Option<PinnedTransportLifecycle>,
}

/// The exact RM-3 stream awaiting greeting admission. 4a.3 consumes this
/// value to perform checks on these bytes; it exposes no independently
/// replaceable half, identity, generation, or guard.
pub struct PendingRemoteTransport {
    private_lease: Option<PrivateAuthorityLease>,
    halves: Option<(ChildStdout, ChildStdin)>,
    expected_server_id: String,
    generation: String,
    lifecycle: PinnedTransportLifecycle,
    close: RemoteCloseHandle,
}

/// Evidence created only by the greeting admission implementation in 4a.3.
pub struct RemoteAdmissionProof {
    hello: Hello,
    census: Census,
}

/// One admitted remote stream. The client transition consumes this whole
/// value, so a caller can never supply a Hello from a different stream.
pub struct AdmittedRemoteTransport {
    pending: PendingRemoteTransport,
    proof: RemoteAdmissionProof,
}

pub(crate) struct RemoteClientParts {
    private_lease: Option<PrivateAuthorityLease>,
    reader: ChildStdout,
    writer: ChildStdin,
    hello: Hello,
    close: RemoteCloseHandle,
}

impl RemoteClientParts {
    /// Sole internal bridge into the one session client. Keeping the fields
    /// private prevents literals, destructuring, and cross-stream pairing.
    pub(crate) fn into_session_bridge(
        self,
    ) -> (
        ChildStdout,
        ChildStdin,
        Hello,
        RemoteCloseHandle,
        Option<PrivateAuthorityLease>,
    ) {
        (
            self.reader,
            self.writer,
            self.hello,
            self.close,
            self.private_lease,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteAdmissionError {
    Deadline,
    Cancelled,
    Protocol(String),
    Incompatible(Box<IncompatibleRemoteHost>),
    EmptyCanonicalInstance,
    ServerIdMismatch { expected: String, actual: String },
    MissingTerminalCapability,
    Worker(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IncompatibleRemoteHost {
    pub(crate) hello: FrozenHostHello,
    pub(crate) census: Census,
}

impl std::fmt::Display for RemoteAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deadline => f.write_str("remote greeting timed out"),
            Self::Cancelled => f.write_str("remote greeting was cancelled"),
            Self::Protocol(detail) | Self::Worker(detail) => f.write_str(detail),
            Self::Incompatible(host) => Skew {
                reason: host.hello.skew_reason().unwrap_or(SkewReason::ProtoMajor {
                    peer: host.hello.proto_major,
                }),
                live_sessions: Some(host.census.live_sessions),
                host_pid: Some(host.hello.pid).filter(|pid| *pid != 0),
            }
            .fmt(f),
            Self::EmptyCanonicalInstance => {
                f.write_str("remote host greeting has an empty canonical instance")
            }
            Self::ServerIdMismatch { expected, actual } => write!(
                f,
                "remote host identity mismatch: expected {expected:?}, got {actual:?}"
            ),
            Self::MissingTerminalCapability => {
                f.write_str("remote host does not advertise terminal capability")
            }
        }
    }
}

impl RemoteTransportOwner {
    pub(crate) fn from_pinned(transport: PinnedSshTransport) -> Self {
        let PinnedSshTransport {
            reader,
            writer,
            child_guard,
            expected_server_id,
            generation,
            private_lease,
        } = transport;
        Self {
            halves: Mutex::new(Some((reader, writer))),
            expected_server_id,
            generation,
            private_lease,
            lifecycle: Some(PinnedTransportLifecycle::new(child_guard)),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_pinned_with_close_gate(
        transport: PinnedSshTransport,
        gate: Arc<TestCloseGate>,
    ) -> Self {
        let PinnedSshTransport {
            reader,
            writer,
            child_guard,
            expected_server_id,
            generation,
            private_lease,
        } = transport;
        Self {
            halves: Mutex::new(Some((reader, writer))),
            expected_server_id,
            generation,
            private_lease,
            lifecycle: Some(PinnedTransportLifecycle::new(GatedRealReaper {
                guard: child_guard,
                gate,
            })),
        }
    }

    pub(crate) fn into_pending(mut self) -> PendingRemoteTransport {
        let lifecycle = self.lifecycle.take().expect("owner has its lifecycle");
        let close = RemoteCloseHandle {
            lifecycle: lifecycle.clone(),
            mailbox: Arc::new(Mutex::new(None)),
        };
        if let Some(lease) = self.private_lease.clone() {
            let retired_close = close.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = lease.retired() => {
                            retired_close.close_awaited().await;
                            return;
                        }
                        closed = retired_close.lifecycle.wait_closed(Duration::from_secs(30)) => {
                            if closed.is_ok() { return; }
                        }
                    }
                }
            });
        }
        PendingRemoteTransport {
            private_lease: self.private_lease.take(),
            halves: lock(&self.halves).take(),
            expected_server_id: std::mem::take(&mut self.expected_server_id),
            generation: std::mem::take(&mut self.generation),
            lifecycle,
            close,
        }
    }

    #[allow(
        dead_code,
        reason = "Retained bounded-close contract; production currently uses close_awaited"
    )]
    pub(crate) async fn close(
        &self,
        timeout: Duration,
    ) -> Result<Arc<CloseReport>, CloseWaitTimedOut> {
        let lifecycle = self.lifecycle.as_ref().expect("owner has its lifecycle");
        lifecycle.begin_close(None);
        // The reaper owns the guard before the pipes are dropped.
        lock(&self.halves).take();
        lifecycle.wait_closed(timeout).await
    }

    pub(crate) async fn close_awaited(&self) -> Arc<CloseReport> {
        let lifecycle = self.lifecycle.as_ref().expect("owner has its lifecycle");
        lifecycle.begin_close(None);
        lock(&self.halves).take();
        loop {
            if let Ok(report) = lifecycle.wait_closed(Duration::from_secs(30)).await {
                return report;
            }
        }
    }
}

impl Drop for RemoteTransportOwner {
    fn drop(&mut self) {
        if let Some(lifecycle) = &self.lifecycle {
            lifecycle.drop_open_guard_now();
        }
    }
}

impl PendingRemoteTransport {
    /// Validate this exact pending handoff without exposing or replacing its generation.
    #[cfg(test)]
    pub(crate) async fn validate_generation_for_test<F>(
        &self,
        validate: impl FnOnce(String) -> F,
    ) -> Result<(), PinnedTransportGenerationError>
    where
        F: std::future::Future<Output = Result<(), PinnedTransportGenerationError>>,
    {
        validate(self.generation.clone()).await
    }

    /// Revalidate RM-3's opaque snapshot after open and immediately before
    /// any greeting or handshake byte is consumed.
    pub(crate) async fn revalidate_before_handshake(
        &self,
        app: tauri::AppHandle,
        machine_id: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        if let Some(lease) = &self.private_lease {
            lease
                .validate()
                .map_err(PinnedTransportGenerationError::Missing)?;
        }
        super::machines::revalidate_pinned_transport_generation(
            app,
            machine_id,
            self.generation.clone(),
        )
        .await?;
        self.private_lease
            .as_ref()
            .map_or(Ok(()), |lease| lease.validate())
            .map_err(PinnedTransportGenerationError::Missing)
    }

    /// Consume an unadmitted handoff through the normal close path. A timed
    /// out waiter does not cancel the independently owned reap.
    #[allow(
        dead_code,
        reason = "Retained bounded-close contract; production currently uses close_awaited"
    )]
    pub(crate) async fn close(
        mut self,
        timeout: Duration,
    ) -> Result<Arc<CloseReport>, CloseWaitTimedOut> {
        self.lifecycle.begin_close(None);
        self.halves.take();
        self.lifecycle.wait_closed(timeout).await
    }

    pub(crate) async fn close_awaited(mut self) -> Arc<CloseReport> {
        self.lifecycle.begin_close(None);
        self.halves.take();
        loop {
            if let Ok(report) = self.lifecycle.wait_closed(Duration::from_secs(30)).await {
                return report;
            }
        }
    }

    /// Perform greeting-first admission on the sole exact stream. The
    /// coordinator is independently owned so cancellation of its caller can
    /// never detach the blocking worker or abandon normal reap.
    pub(crate) async fn admit(
        self,
        timeout: Duration,
        cancelled: CancellationToken,
    ) -> Result<AdmittedRemoteTransport, RemoteAdmissionError> {
        let coordinator = tokio::spawn(async move {
            let close = self.close.clone();
            let mut worker = tokio::task::spawn_blocking(move || self.admit_blocking());
            let outcome = tokio::select! {
                joined = &mut worker => Some(joined),
                _ = tokio::time::sleep(timeout) => None,
                _ = cancelled.cancelled() => {
                    close.begin_close();
                    let joined = worker.await;
                    close.close_awaited().await;
                    return joined
                        .map_err(|error| RemoteAdmissionError::Worker(format!("remote greeting worker failed: {error}")))
                        .and(Err(RemoteAdmissionError::Cancelled));
                }
            };
            match outcome {
                Some(Ok(Ok(admitted))) => Ok(admitted),
                Some(Ok(Err(error))) => {
                    close.close_awaited().await;
                    Err(error)
                }
                Some(Err(error)) => {
                    close.close_awaited().await;
                    Err(RemoteAdmissionError::Worker(format!(
                        "remote greeting worker failed: {error}"
                    )))
                }
                None => {
                    close.begin_close();
                    let joined = worker.await;
                    close.close_awaited().await;
                    let _ = joined.map_err(|error| {
                        RemoteAdmissionError::Worker(format!(
                            "remote greeting worker failed after timeout: {error}"
                        ))
                    })?;
                    Err(RemoteAdmissionError::Deadline)
                }
            }
        });
        coordinator.await.map_err(|error| {
            RemoteAdmissionError::Worker(format!("remote greeting coordinator failed: {error}"))
        })?
    }

    fn admit_blocking(mut self) -> Result<AdmittedRemoteTransport, RemoteAdmissionError> {
        let (reader, mut writer) = self.halves.take().ok_or_else(|| {
            RemoteAdmissionError::Protocol("remote stream was already consumed".into())
        })?;
        let mut reader = BufReader::new(reader);
        let (peer, census) = codesense_host_proto::read_host_greeting(&mut reader)
            .map_err(RemoteAdmissionError::Protocol)?;
        let hello = match peer {
            HostGreeting::Supported { hello } => hello,
            HostGreeting::Unsupported(hello) => {
                if hello.server_id != self.expected_server_id {
                    return Err(RemoteAdmissionError::ServerIdMismatch {
                        expected: self.expected_server_id.clone(),
                        actual: hello.server_id,
                    });
                }
                if hello.epoch.is_empty() {
                    return Err(RemoteAdmissionError::Protocol(
                        "older remote host greeting has no lifetime identity".into(),
                    ));
                }
                if hello.proto_major == 0
                    || hello.agent_version.is_empty()
                    || hello.server_name.is_empty()
                    || hello.instance_id.is_empty()
                    || hello.pid == 0
                {
                    return Err(RemoteAdmissionError::Protocol(
                        "older remote host greeting has incomplete frozen identity".into(),
                    ));
                }
                return Err(RemoteAdmissionError::Incompatible(Box::new(
                    IncompatibleRemoteHost { hello, census },
                )));
            }
        };
        if hello.instance_id.is_empty() {
            return Err(RemoteAdmissionError::EmptyCanonicalInstance);
        }
        // The client greeting is the first byte we own on this stream.  Do
        // not disclose it until the complete host greeting has established
        // both pinned lifetime identity and the required service surface.
        if hello.server_id != self.expected_server_id {
            return Err(RemoteAdmissionError::ServerIdMismatch {
                expected: self.expected_server_id.clone(),
                actual: hello.server_id,
            });
        }
        if !hello.supports(&Capability::Terminals) {
            return Err(RemoteAdmissionError::MissingTerminalCapability);
        }
        // Only the canonical instance parsed from these bytes can construct
        // the echo. There is no caller-controlled production instance input.
        let ours = client_hello(
            hello.instance_id.clone(),
            env!("CARGO_PKG_VERSION"),
            &crate::commands::pty_layout::app_viewer(),
        );
        codesense_host_proto::write_frame(
            &mut writer,
            &ours.frame().map_err(RemoteAdmissionError::Protocol)?,
        )
        .and_then(|()| writer.flush())
        .map_err(|error| RemoteAdmissionError::Protocol(error.to_string()))?;
        let reader = reader.into_inner();
        self.halves = Some((reader, writer));
        Ok(self.into_admitted(RemoteAdmissionProof { hello, census }))
    }

    pub(crate) fn into_admitted(self, proof: RemoteAdmissionProof) -> AdmittedRemoteTransport {
        AdmittedRemoteTransport {
            pending: self,
            proof,
        }
    }
}

impl AdmittedRemoteTransport {
    /// The server identity proven by the greeting on these exact stream halves.
    pub(crate) fn verified_server_id(&self) -> &str {
        &self.proof.hello.server_id
    }

    pub(crate) fn verified_epoch(&self) -> &str {
        &self.proof.hello.epoch
    }

    #[cfg(test)]
    pub(crate) fn verified_pid(&self) -> u32 {
        self.proof.hello.pid
    }

    pub(crate) fn verified_census(&self) -> &Census {
        &self.proof.census
    }

    /// Preserve the opaque registry authority across client construction so
    /// the manager can prove it again at the publication boundary.
    pub(crate) fn publication_generation(&self) -> String {
        self.pending.generation.clone()
    }

    /// Revalidate the same RM-3 handoff after identity admission and
    /// immediately before the manager's adoption/publication critical section.
    pub(crate) async fn revalidate_before_publication(
        &self,
        app: tauri::AppHandle,
        machine_id: String,
    ) -> Result<(), PinnedTransportGenerationError> {
        self.pending
            .revalidate_before_handshake(app, machine_id)
            .await
    }

    #[allow(
        dead_code,
        reason = "Retained bounded-close contract; production currently uses close_awaited"
    )]
    pub(crate) async fn close(
        self,
        timeout: Duration,
    ) -> Result<Arc<CloseReport>, CloseWaitTimedOut> {
        self.pending.close(timeout).await
    }

    pub(crate) async fn close_awaited(self) -> Arc<CloseReport> {
        self.pending.close_awaited().await
    }

    pub(crate) fn into_client_parts(mut self) -> RemoteClientParts {
        let (reader, writer) = self
            .pending
            .halves
            .take()
            .expect("admitted stream has halves");
        RemoteClientParts {
            private_lease: self.pending.private_lease,
            reader,
            writer,
            hello: self.proof.hello,
            close: self.pending.close,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::machines::test_pinned_transport;
    use crate::terminal::session_server::TransportWriter;

    #[tokio::test]
    async fn private_lease_retirement_reaps_only_its_exact_pending_transport() {
        use std::io::Read;
        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("echo-transport");
        std::fs::write(&program, "#!/bin/sh\nexec cat\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (mut transport, old_pid) = test_pinned_transport(&program).unwrap();
        let (lease, switch) = crate::commands::auth::private_authority::test_switching_lease(41);
        transport.private_lease = Some(lease);
        let old = RemoteTransportOwner::from_pinned(transport).into_pending();
        let _foreign = switch(42);
        let fresh = switch(41);
        let (mut replacement, replacement_pid) = test_pinned_transport(&program).unwrap();
        replacement.private_lease = Some(fresh);
        let mut replacement = RemoteTransportOwner::from_pinned(replacement).into_pending();
        old.lifecycle
            .wait_closed(Duration::from_secs(5))
            .await
            .unwrap();
        assert_ne!(unsafe { libc::kill(old_pid, 0) }, 0);
        assert_eq!(unsafe { libc::kill(replacement_pid, 0) }, 0);
        let (reader, writer) = replacement.halves.as_mut().unwrap();
        writer.write_all(b"fresh owner\n").unwrap();
        let mut echoed = [0; 12];
        reader.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"fresh owner\n");
        replacement.close_awaited().await;
    }
    use codesense_host_proto::{
        Capability, Census, Frame, Hello, PeerHello, PROTO_MAJOR, PROTO_MINOR,
    };
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::io::AsRawFd;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Condvar;

    struct FakeReaper {
        calls: Arc<AtomicUsize>,
        release: Option<Arc<(Mutex<bool>, Condvar)>>,
    }

    struct PanicReaper;

    impl BlockingReaper for PanicReaper {
        fn close_and_reap(self: Box<Self>) -> Result<(), String> {
            panic!("injected reaper join failure")
        }
    }

    struct PanicWorkerExit;

    impl BlockingWorkerExit for PanicWorkerExit {
        fn wait(self: Box<Self>) {
            panic!("injected worker join failure")
        }
    }

    impl BlockingReaper for FakeReaper {
        fn close_and_reap(self: Box<Self>) -> Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(release) = self.release {
                let (released, changed) = &*release;
                let mut released = lock(released);
                while !*released {
                    released = changed
                        .wait(released)
                        .unwrap_or_else(|error| error.into_inner());
                }
            }
            Ok(())
        }
    }

    fn signal_release(release: &Arc<(Mutex<bool>, Condvar)>) {
        let (released, changed) = &**release;
        *lock(released) = true;
        changed.notify_all();
    }

    fn greeting_program(server_id: &str, body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let hello = Hello {
            proto_major: PROTO_MAJOR,
            proto_minor: PROTO_MINOR,
            agent_version: "test".into(),
            server_id: server_id.into(),
            server_name: "test".into(),
            epoch: "epoch".into(),
            rev: 1,
            instance_id: "/remote/root".into(),
            capabilities: vec![Capability::Terminals],
            viewer_id: None,
            pid: 7,
            emulator: Some(codesense_host_proto::EmulatorHello::current()),
            build: None,
            client_id: None,
        };
        let mut wire = hello.frame().unwrap().encode();
        wire.extend(Census { live_sessions: 0 }.frame().encode());
        wire_program(wire, body)
    }

    fn wire_program(wire: Vec<u8>, body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let greeting = root.path().join("greeting.bin");
        std::fs::write(&greeting, wire).unwrap();
        let program = root.path().join("fake-ssh");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ncat '{}'\nexec 1>&-\n{body}\n",
                greeting.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        (root, program)
    }

    /// Upper bound for admissions that are expected to settle on their own.
    /// Spawning `/bin/sh` from this heavily threaded test binary takes well
    /// over a second when the whole suite runs in parallel, so a tight budget
    /// turns a protocol verdict into a spurious `Deadline`. A passing test
    /// never waits for it; tests that assert `Deadline` use their own short
    /// budget against a child that never answers.
    const ADMIT_BUDGET: Duration = Duration::from_secs(10);

    async fn admission_error_for_wire(wire: Vec<u8>) -> (RemoteAdmissionError, libc::pid_t) {
        let (_root, program) = wire_program(wire, "cat >/dev/null");
        let (transport, pid) = test_pinned_transport(&program).unwrap();
        let pending = RemoteTransportOwner::from_pinned(transport).into_pending();
        let error = match pending.admit(ADMIT_BUDGET, CancellationToken::new()).await {
            Err(error) => error,
            Ok(_) => panic!("invalid greeting was admitted"),
        };
        (error, pid)
    }

    fn assert_reaped(pid: libc::pid_t) {
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "child is no longer alive"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocked_greeting_deadline_joins_worker_and_truly_reaps() {
        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("blocked-greeting");
        std::fs::write(&program, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (transport, pid) = test_pinned_transport(&program).unwrap();
        let pending = RemoteTransportOwner::from_pinned(transport).into_pending();
        let error = match pending
            .admit(Duration::from_millis(50), CancellationToken::new())
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("blocked greeting was admitted"),
        };
        assert_eq!(error, RemoteAdmissionError::Deadline);
        assert_reaped(pid);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocked_greeting_cancellation_joins_worker_and_truly_reaps() {
        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("cancelled-greeting");
        std::fs::write(&program, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (transport, pid) = test_pinned_transport(&program).unwrap();
        let pending = RemoteTransportOwner::from_pinned(transport).into_pending();
        let token = CancellationToken::new();
        let cancellation = token.clone();
        let admitting =
            tokio::spawn(async move { pending.admit(Duration::from_secs(5), token).await });
        tokio::task::yield_now().await;
        cancellation.cancel();
        let error = match admitting.await.unwrap() {
            Err(error) => error,
            Ok(_) => panic!("cancelled greeting was admitted"),
        };
        assert_eq!(error, RemoteAdmissionError::Cancelled);
        assert_reaped(pid);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocked_echo_write_deadline_joins_worker_and_truly_reaps() {
        let (_root, program) = greeting_program("test-server", "sleep 30");
        let (transport, pid) = test_pinned_transport(&program).unwrap();
        let mut pending = RemoteTransportOwner::from_pinned(transport).into_pending();
        let writer = &mut pending.halves.as_mut().unwrap().1;
        let fd = writer.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let fill = [b'x'; 64 * 1024];
        loop {
            match writer.write(&fill) {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("fill child stdin: {error}"),
            }
        }
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFL, flags) }, 0);
        let error = match pending
            .admit(Duration::from_millis(100), CancellationToken::new())
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("blocked echo was admitted"),
        };
        assert_eq!(error, RemoteAdmissionError::Deadline);
        assert_reaped(pid);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn expected_server_id_mismatch_publishes_no_admitted_value_and_reaps() {
        let (_root, program) = greeting_program("another-server", "cat >/dev/null");
        let (transport, pid) = test_pinned_transport(&program).unwrap();
        let pending = RemoteTransportOwner::from_pinned(transport).into_pending();
        let error = match pending.admit(ADMIT_BUDGET, CancellationToken::new()).await {
            Err(error) => error,
            Ok(_) => panic!("mismatched server id was admitted"),
        };
        assert_eq!(
            error,
            RemoteAdmissionError::ServerIdMismatch {
                expected: "test-server".into(),
                actual: "another-server".into(),
            }
        );
        assert_reaped(pid);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rm4c_ssh_invalid_partial_and_older_greetings_hard_stop_and_reap() {
        let (malformed, pid) = admission_error_for_wire(vec![0, 0, 0, 1, 0xff]).await;
        assert!(matches!(malformed, RemoteAdmissionError::Protocol(_)));
        assert_reaped(pid);

        let hello = Hello {
            proto_major: PROTO_MAJOR,
            proto_minor: PROTO_MINOR,
            agent_version: "test-agent".into(),
            server_id: "test-server".into(),
            server_name: "test-host".into(),
            epoch: "verified-lifetime".into(),
            instance_id: "/remote/root".into(),
            capabilities: vec![Capability::Terminals],
            pid: 42,
            emulator: Some(codesense_host_proto::EmulatorHello::current()),
            ..Hello::default()
        };
        let (eof, pid) = admission_error_for_wire(hello.frame().unwrap().encode()).await;
        assert_eq!(
            eof,
            RemoteAdmissionError::Protocol("closed before its census".into())
        );
        assert_reaped(pid);

        let mut incompatible = hello.clone();
        incompatible.proto_major = PROTO_MAJOR - 2;
        let mut wire = incompatible.frame().unwrap().encode();
        wire.extend(Census { live_sessions: 3 }.frame().encode());
        let (error, pid) = admission_error_for_wire(wire).await;
        assert_eq!(
            error,
            RemoteAdmissionError::Incompatible(Box::new(IncompatibleRemoteHost {
                hello: incompatible.into(),
                census: Census { live_sessions: 3 },
            }))
        );
        assert_reaped(pid);

        let mut no_capability = hello.clone();
        no_capability.capabilities.clear();
        let mut wire = no_capability.frame().unwrap().encode();
        wire.extend(Census { live_sessions: 0 }.frame().encode());
        let (error, pid) = admission_error_for_wire(wire).await;
        assert_eq!(error, RemoteAdmissionError::MissingTerminalCapability);
        assert_reaped(pid);

        let mut empty = hello;
        empty.instance_id.clear();
        let mut wire = empty.frame().unwrap().encode();
        wire.extend(Census { live_sessions: 0 }.frame().encode());
        let (error, pid) = admission_error_for_wire(wire).await;
        assert_eq!(error, RemoteAdmissionError::EmptyCanonicalInstance);
        assert_reaped(pid);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn successful_admission_echoes_only_the_greeted_canonical_instance() {
        let scratch = tempfile::tempdir().unwrap();
        let echoed = scratch.path().join("echoed.bin");
        let body = format!("cat > '{}'", echoed.display());
        let (_root, program) = greeting_program("test-server", &body);
        let (transport, pid) = test_pinned_transport(&program).unwrap();
        let pending = RemoteTransportOwner::from_pinned(transport).into_pending();
        let admitted = match pending.admit(ADMIT_BUDGET, CancellationToken::new()).await {
            Ok(admitted) => admitted,
            Err(error) => panic!("identity was not admitted: {error}"),
        };
        let deadline = std::time::Instant::now() + ADMIT_BUDGET;
        let ours = loop {
            let bytes = std::fs::read(&echoed).unwrap_or_default();
            if let Ok(PeerHello::Compatible(hello)) =
                codesense_host_proto::read_hello(&mut bytes.as_slice())
            {
                break hello;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "canonical echo was not captured"
            );
            tokio::task::yield_now().await;
        };
        assert_eq!(ours.instance_id, "/remote/root");
        admitted.close_awaited().await;
        assert_reaped(pid);
    }

    /// A host one major back is not adapted: admission refuses it as
    /// incompatible, naming it for its restart, and writes nothing to it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rm4c_ssh_previous_major_is_refused_as_incompatible() {
        let scratch = tempfile::tempdir().unwrap();
        let hello = Hello {
            proto_major: PROTO_MAJOR - 1,
            proto_minor: 0,
            agent_version: "old".into(),
            server_id: "test-server".into(),
            server_name: "old host".into(),
            epoch: "v6-epoch".into(),
            instance_id: "/remote/root".into(),
            capabilities: vec![Capability::Terminals],
            pid: 71,
            ..Hello::default()
        };
        let mut wire = hello.frame().unwrap().encode();
        wire.extend(Census { live_sessions: 2 }.frame().encode());
        let greeting = scratch.path().join("greeting-v6.bin");
        std::fs::write(&greeting, wire).unwrap();
        let program = scratch.path().join("historical-v6-host");
        std::fs::write(
            &program,
            format!("#!/bin/sh\ncat '{}'\ncat >/dev/null\n", greeting.display(),),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (transport, pid) = test_pinned_transport(&program).unwrap();
        let refused = RemoteTransportOwner::from_pinned(transport)
            .into_pending()
            .admit(ADMIT_BUDGET, CancellationToken::new())
            .await
            .err()
            .expect("the previous major is not admitted");
        let RemoteAdmissionError::Incompatible(host) = &refused else {
            panic!("unexpected {refused:?}");
        };
        assert_eq!(
            host.hello.skew_reason(),
            Some(SkewReason::ProtoMajor {
                peer: PROTO_MAJOR - 1
            })
        );
        assert_eq!(host.census.live_sessions, 2);
        assert!(refused
            .to_string()
            .starts_with("restart terminal server (ends 2 sessions)"));
        assert_reaped(pid);
    }

    #[derive(Default)]
    struct ReaperLedger {
        normal_close: AtomicUsize,
        emergency_drop: AtomicUsize,
        finished: AtomicUsize,
    }

    struct LedgerReaper {
        ledger: Arc<ReaperLedger>,
        release: Option<Arc<(Mutex<bool>, Condvar)>>,
        normally_closed: bool,
    }

    impl BlockingReaper for LedgerReaper {
        fn close_and_reap(mut self: Box<Self>) -> Result<(), String> {
            self.normally_closed = true;
            self.ledger.normal_close.fetch_add(1, Ordering::SeqCst);
            if let Some(release) = &self.release {
                let (released, changed) = &**release;
                let mut released = lock(released);
                while !*released {
                    released = changed
                        .wait(released)
                        .unwrap_or_else(|error| error.into_inner());
                }
            }
            self.ledger.finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    impl Drop for LedgerReaper {
        fn drop(&mut self) {
            if !self.normally_closed {
                self.ledger.emergency_drop.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    struct LifecycleOwner(PinnedTransportLifecycle);

    impl Drop for LifecycleOwner {
        fn drop(&mut self) {
            self.0.drop_open_guard_now();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_concurrent_closer_observes_one_shared_reap_result() {
        let calls = Arc::new(AtomicUsize::new(0));
        let lifecycle = PinnedTransportLifecycle::new(FakeReaper {
            calls: calls.clone(),
            release: None,
        });
        lifecycle.begin_close(None);
        let (one, two, three) = tokio::join!(
            lifecycle.wait_closed(Duration::from_secs(1)),
            lifecycle.wait_closed(Duration::from_secs(1)),
            lifecycle.wait_closed(Duration::from_secs(1)),
        );
        let one = one.unwrap();
        let two = two.unwrap();
        let three = three.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&one, &two));
        assert!(Arc::ptr_eq(&one, &three));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn caller_timeout_leaves_close_authoritatively_in_progress() {
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let lifecycle = PinnedTransportLifecycle::new(FakeReaper {
            calls: calls.clone(),
            release: Some(release.clone()),
        });
        lifecycle.begin_close(None);
        assert_eq!(
            lifecycle.wait_closed(Duration::from_millis(10)).await,
            Err(CloseWaitTimedOut)
        );
        assert_eq!(lifecycle.phase(), "closing");
        // The reaper runs on the blocking pool, which a loaded parallel run
        // may not schedule within the 10 ms wait above. Release before any
        // panic: an unreleased reaper blocks runtime shutdown forever.
        wait_for_reap_start(&calls, || signal_release(&release)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        signal_release(&release);
        lifecycle.wait_closed(Duration::from_secs(1)).await.unwrap();
        assert_eq!(lifecycle.phase(), "closed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_the_initiating_wait_does_not_cancel_reap() {
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let lifecycle = PinnedTransportLifecycle::new(FakeReaper {
            calls: calls.clone(),
            release: Some(release.clone()),
        });
        lifecycle.begin_close(None);
        let waiting = {
            let lifecycle = lifecycle.clone();
            tokio::spawn(async move { lifecycle.wait_closed(Duration::from_secs(10)).await })
        };
        waiting.abort();
        signal_release(&release);
        lifecycle.wait_closed(Duration::from_secs(1)).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn close_stays_in_progress_until_the_writer_worker_actually_exits() {
        let calls = Arc::new(AtomicUsize::new(0));
        let lifecycle = PinnedTransportLifecycle::new(FakeReaper {
            calls: calls.clone(),
            release: None,
        });
        let worker = WorkerCompletion::pending();
        lifecycle.set_worker(worker.clone());
        lifecycle.begin_close(None);

        assert_eq!(
            lifecycle.wait_closed(Duration::from_millis(10)).await,
            Err(CloseWaitTimedOut)
        );
        assert_eq!(lifecycle.phase(), "closing");
        wait_for_reap_start(&calls, || worker.mark_done()).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        worker.mark_done();
        let report = lifecycle.wait_closed(Duration::from_secs(1)).await.unwrap();
        assert!(report.worker_exited);
        assert_eq!(lifecycle.phase(), "closed");
    }

    /// Waits for the blocking reaper to start, running `unblock` before
    /// failing so a stuck reaper cannot hang runtime shutdown.
    async fn wait_for_reap_start(calls: &AtomicUsize, unblock: impl FnOnce()) {
        let deadline = std::time::Instant::now() + ADMIT_BUDGET;
        while calls.load(Ordering::SeqCst) == 0 {
            if std::time::Instant::now() >= deadline {
                unblock();
                panic!("reaper did not start");
            }
            tokio::task::yield_now().await;
        }
    }

    async fn wait_for_diagnostic(lifecycle: &PinnedTransportLifecycle) -> String {
        // The join error lands only after the panic hook prints its backtrace, which a
        // loaded runner with RUST_BACKTRACE=1 can take over a second to symbolicate.
        let deadline = std::time::Instant::now() + ADMIT_BUDGET;
        loop {
            if let Some(diagnostic) = lifecycle.diagnostic() {
                return diagnostic;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "join failure diagnostic was not recorded"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reaper_join_failure_never_synthesizes_closed() {
        let lifecycle = PinnedTransportLifecycle::new(PanicReaper);
        lifecycle.begin_close(None);
        let diagnostic = wait_for_diagnostic(&lifecycle).await;
        assert!(diagnostic.contains("remote close completion task failed"));
        assert_eq!(lifecycle.phase(), "closing");
        assert_eq!(
            lifecycle.wait_closed(Duration::from_millis(10)).await,
            Err(CloseWaitTimedOut)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn worker_join_failure_never_synthesizes_closed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let lifecycle = PinnedTransportLifecycle::new(FakeReaper {
            calls: calls.clone(),
            release: None,
        });
        lifecycle.set_worker_exit(PanicWorkerExit);
        lifecycle.begin_close(None);
        let diagnostic = wait_for_diagnostic(&lifecycle).await;
        assert!(diagnostic.contains("remote close completion task failed"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(lifecycle.phase(), "closing");
        assert_eq!(
            lifecycle.wait_closed(Duration::from_millis(10)).await,
            Err(CloseWaitTimedOut)
        );
    }

    #[test]
    fn dropping_an_open_owner_runs_the_emergency_guard_drop_immediately() {
        let ledger = Arc::new(ReaperLedger::default());
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let owner = {
            let _entered = runtime.enter();
            LifecycleOwner(PinnedTransportLifecycle::new(LedgerReaper {
                ledger: ledger.clone(),
                release: None,
                normally_closed: false,
            }))
        };
        assert!(tokio::runtime::Handle::try_current().is_err());
        drop(owner);
        assert_eq!(ledger.normal_close.load(Ordering::SeqCst), 0);
        assert_eq!(ledger.emergency_drop.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rm5aa_plain_thread_close_uses_owned_runtime_and_reaps_once() {
        struct CountedRealReaper {
            guard: Option<SshProcessGuard>,
            ledger: Arc<ReaperLedger>,
        }
        impl BlockingReaper for CountedRealReaper {
            fn close_and_reap(mut self: Box<Self>) -> Result<(), String> {
                self.ledger.normal_close.fetch_add(1, Ordering::SeqCst);
                let result = self.guard.take().unwrap().close_and_reap();
                self.ledger.finished.fetch_add(1, Ordering::SeqCst);
                result
            }
        }
        impl Drop for CountedRealReaper {
            fn drop(&mut self) {
                if self.guard.is_some() {
                    self.ledger.emergency_drop.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        let (_root, program) = greeting_program("plain-thread-close", "cat >/dev/null");
        let (transport, pid) = test_pinned_transport(&program).unwrap();
        let PinnedSshTransport {
            child_guard,
            reader,
            writer,
            ..
        } = transport;
        let ledger = Arc::new(ReaperLedger::default());
        let lifecycle = PinnedTransportLifecycle::new(CountedRealReaper {
            guard: Some(child_guard),
            ledger: ledger.clone(),
        });
        let worker = WorkerCompletion::pending();
        lifecycle.set_worker(worker.clone());
        let barrier = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            let close = || {
                assert!(tokio::runtime::Handle::try_current().is_err());
                barrier.wait();
                lifecycle.begin_close(None);
            };
            let first = scope.spawn(close);
            let second = scope.spawn(close);
            barrier.wait();
            first.join().unwrap();
            second.join().unwrap();
        });
        drop((reader, writer));
        let waiting = lifecycle.wait_closed(Duration::from_millis(10)).await;
        let phase_before_worker_exit = lifecycle.phase();
        // Release the test gate before assertions so a failed oracle cannot
        // strand a blocking runtime worker during fixture teardown.
        worker.mark_done();
        let report = lifecycle.wait_closed(Duration::from_secs(1)).await.unwrap();
        assert_eq!(waiting, Err(CloseWaitTimedOut));
        assert_eq!(phase_before_worker_exit, "closing");
        assert_eq!(report.reap, Ok(()));
        assert!(report.worker_exited);
        assert_eq!(ledger.normal_close.load(Ordering::SeqCst), 1);
        assert_eq!(ledger.finished.load(Ordering::SeqCst), 1);
        assert_eq!(ledger.emergency_drop.load(Ordering::SeqCst), 0);
        assert_reaped(pid);
        let duplicate = lifecycle.wait_closed(Duration::from_secs(1)).await.unwrap();
        assert!(Arc::ptr_eq(&report, &duplicate));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_a_closing_owner_leaves_the_independent_reaper_alive() {
        let ledger = Arc::new(ReaperLedger::default());
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let owner = LifecycleOwner(PinnedTransportLifecycle::new(LedgerReaper {
            ledger: ledger.clone(),
            release: Some(release.clone()),
            normally_closed: false,
        }));
        owner.0.begin_close(None);
        drop(owner);
        let started_deadline = std::time::Instant::now() + Duration::from_secs(1);
        while ledger.normal_close.load(Ordering::SeqCst) == 0 {
            if std::time::Instant::now() >= started_deadline {
                signal_release(&release);
                panic!("independent reaper did not start");
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(ledger.normal_close.load(Ordering::SeqCst), 1);
        assert_eq!(ledger.emergency_drop.load(Ordering::SeqCst), 0);
        signal_release(&release);
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while ledger.finished.load(Ordering::SeqCst) == 0 {
            assert!(std::time::Instant::now() < deadline, "reaper was cancelled");
            tokio::task::yield_now().await;
        }
        assert_eq!(ledger.normal_close.load(Ordering::SeqCst), 1);
        assert_eq!(ledger.emergency_drop.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_real_rm3_blocked_child_pipe_is_killed_and_reaped_before_closed() {
        struct ObservedChildStdin {
            inner: ChildStdin,
            entered: Option<std::sync::mpsc::SyncSender<()>>,
            returned: Arc<AtomicBool>,
        }

        impl Write for ObservedChildStdin {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(entered) = self.entered.take() {
                    let _ = entered.send(());
                }
                let result = self.inner.write(bytes);
                self.returned.store(true, Ordering::SeqCst);
                result
            }

            fn flush(&mut self) -> std::io::Result<()> {
                self.inner.flush()
            }
        }

        impl TransportWriter for ObservedChildStdin {
            fn close(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("blocked-ssh");
        std::fs::write(&program, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (transport, pid) = test_pinned_transport(&program).unwrap();
        let mut pending = RemoteTransportOwner::from_pinned(transport).into_pending();
        let (_reader, writer) = pending.halves.take().expect("pending transport has halves");
        let fd = writer.as_raw_fd();
        let original_flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(original_flags >= 0, "read ChildStdin descriptor flags");
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags | libc::O_NONBLOCK) },
            0,
            "set ChildStdin nonblocking while filling its kernel pipe"
        );
        let fill = [b'x'; 64 * 1024];
        loop {
            let written = unsafe { libc::write(fd, fill.as_ptr().cast(), fill.len()) };
            if written >= 0 {
                continue;
            }
            let error = std::io::Error::last_os_error();
            let raw = error.raw_os_error();
            assert!(
                raw == Some(libc::EAGAIN) || raw == Some(libc::EWOULDBLOCK),
                "fill real ChildStdin pipe: {error}"
            );
            break;
        }
        assert_eq!(
            unsafe { libc::fcntl(fd, libc::F_SETFL, original_flags) },
            0,
            "restore blocking ChildStdin before the observed worker write"
        );
        let lifecycle = pending.lifecycle.clone();
        let interrupt_lifecycle = lifecycle.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let write_returned = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let mailbox = WriteMailbox::start(
            Box::new(ObservedChildStdin {
                inner: writer,
                entered: Some(entered_tx),
                returned: write_returned.clone(),
            }),
            Arc::new(move || interrupt_lifecycle.begin_close(None)),
            alive,
        )
        .unwrap();
        lifecycle.set_worker(mailbox.completion());
        mailbox
            .try_send(&Frame::input(1, 1, b"blocked-after-kernel-eagain"))
            .unwrap();
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("writer dequeued the frame and entered the real ChildStdin write");
        assert!(
            !write_returned.load(Ordering::SeqCst),
            "write unexpectedly returned after the kernel proved the pipe full"
        );

        lifecycle.begin_close(Some(mailbox.clone()));
        let report = lifecycle
            .wait_closed(Duration::from_secs(2))
            .await
            .expect("real RM3 child close completes");
        assert_eq!(report.reap, Ok(()));
        assert!(report.worker_exited);
        assert!(write_returned.load(Ordering::SeqCst));
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0, "RM3 child survived close");
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1,
            "RM3 child was killed but not reaped"
        );
    }

    #[test]
    fn remote_transition_signatures_consume_each_opaque_stage_whole() {
        let _owner: fn(PinnedSshTransport) -> RemoteTransportOwner =
            RemoteTransportOwner::from_pinned;
        let _pending: fn(RemoteTransportOwner) -> PendingRemoteTransport =
            RemoteTransportOwner::into_pending;
        let _admitted: fn(PendingRemoteTransport, RemoteAdmissionProof) -> AdmittedRemoteTransport =
            PendingRemoteTransport::into_admitted;
    }
}
