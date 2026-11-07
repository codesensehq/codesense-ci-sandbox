// SPDX-License-Identifier: MIT
#![cfg(test)]

use super::*;
use codesense_session_server::testing::{RecordingSink, TestServer};

struct PartialWriter(usize);
impl Write for PartialWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        if self.0 == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        }
        let written = self.0.min(buffer.len());
        self.0 -= written;
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl TransportWriter for PartialWriter {
    fn close(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn partial_input_bytes_are_observed_without_claiming_a_completed_frame() {
    let observed = Arc::new(Rm4fObservation::default());
    observed.enabled.store(true, Ordering::Release);
    let alive = Arc::new(AtomicBool::new(true));
    let mailbox = WriteMailbox::start(
        Box::new(Rm4fWriter {
            inner: PartialWriter(18),
            observation: observed.clone(),
            input_position: None,
        }),
        Arc::new(|| {}),
        alive.clone(),
    )
    .unwrap();
    mailbox.try_send(&Frame::input(2, 7, b"abc")).unwrap();
    assert!(mailbox.completion().wait_timeout(Duration::from_secs(2)));
    let snapshot = lock(&observed.snapshot);
    assert_eq!(snapshot.bytes_written, 18);
    assert_eq!(snapshot.input_payload_bytes, 1);
    assert_eq!(snapshot.completed_frames, 0);
    assert!(snapshot.requests.is_empty());
    assert!(!alive.load(Ordering::SeqCst));
}

fn client(server: &TestServer) -> Arc<SessionServerClient> {
    SessionServerClient::connect(
        server.root(),
        Arc::new(RecordingSink::default()),
        Arc::default(),
        None,
    )
    .unwrap()
}

fn request() -> OpenTab {
    OpenTab {
        viewer: ViewerId::new("rm4f-observer"),
        workspace: WorkspaceKey::new("rm4f"),
        session: PtySessionId::new("controlled"),
        descriptor: Some(WorkspaceDescriptor {
            display_name: "observer".into(),
            kind: "root".into(),
            ..Default::default()
        }),
        name: None,
        role: None,
        tags: Default::default(),
        placement: Placement::Tab,
        index: None,
        activate: true,
    }
}

#[test]
fn observer_is_opt_in_per_client_and_preserves_typed_layout_refusal() {
    let server = TestServer::start();
    let other_server = TestServer::start();
    let a = client(&server);
    let b = client(&other_server);
    a.heartbeat(Duration::from_secs(2)).unwrap();
    assert_eq!(a.rm4f_snapshot(), Rm4fSnapshot::default());
    a.rm4f_observe();
    b.rm4f_observe();
    let before_b = b.rm4f_snapshot();
    assert_eq!(
        a.open_tab(request(), Box::new(|| Err("named rejection".into()))),
        Err(LayoutError::Spawn("named rejection".into()))
    );
    let snapshot = a.rm4f_snapshot();
    let refused = snapshot
        .replies
        .iter()
        .find(|reply| !reply.success)
        .unwrap();
    assert!(refused.transport_success);
    assert_eq!(refused.error.as_deref(), Some("named rejection"));
    assert!(matches!(&refused.reply, Some(Reply::LayoutRefused { .. })));
    assert!(snapshot.bytes_read > 0 && snapshot.bytes_written > 0);
    assert_eq!(
        snapshot.input_payload_bytes, 0,
        "CONTROL is not INPUT payload"
    );
    assert_eq!(b.rm4f_snapshot(), before_b);
    // The same-session retry reaches its spawn callback only after actual
    // ReserveTab success; it completes normal error cleanup again.
    assert_eq!(
        a.open_tab(request(), Box::new(|| Err("retry reached spawn".into()))),
        Err(LayoutError::Spawn("retry reached spawn".into()))
    );
}

#[test]
fn skipped_error_dispatch_retains_reservation_and_records_retry_refusal() {
    let server = TestServer::start();
    let a = client(&server);
    a.rm4f_observe();
    a.rm4f_skip_abort(true);
    assert_eq!(
        a.open_tab(request(), Box::new(|| Err("named rejection".into()))),
        Err(LayoutError::Spawn("named rejection".into()))
    );
    a.rm4f_skip_abort(false);
    assert!(matches!(
        a.open_tab(
            request(),
            Box::new(|| panic!("held reservation reached spawn"))
        ),
        Err(LayoutError::AlreadyPlaced(_))
    ));
    assert!(a.is_alive());
    let snapshot = a.rm4f_snapshot();
    assert!(!snapshot
        .requests
        .iter()
        .any(|request| request.kind == "finish_tab_error"));
    let refused = snapshot
        .replies
        .iter()
        .find(|reply| !reply.success)
        .unwrap();
    assert!(refused.transport_success);
    assert!(matches!(
        &refused.reply,
        Some(Reply::LayoutRefused {
            error: LayoutError::AlreadyPlaced(_)
        })
    ));
}
