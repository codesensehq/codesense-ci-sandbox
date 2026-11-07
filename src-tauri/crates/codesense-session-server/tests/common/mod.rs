// SPDX-License-Identifier: MIT
//! A relay process driven through its stdio, and the golden check of a host's
//! greeting as it crosses the wire.

use std::collections::VecDeque;
use std::io::Read;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use codesense_host_proto::{
    read_frame, read_hello, write_frame, Census, Frame, Hello, PeerHello, PROTO_MAJOR,
};

/// How long any read from a relay may take.
pub const WAIT: Duration = Duration::from_secs(30);

/// A running relay: write to its stdin, read its stdout with a deadline.
pub struct Relay {
    child: Child,
    input: Option<ChildStdin>,
    pub output: Timed,
}

impl Relay {
    pub fn spawn(mut command: Command) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn the relay");
        let input = child.stdin.take();
        let mut stdout = child.stdout.take().expect("relay stdout");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            output: Timed {
                chunks: rx,
                pending: VecDeque::new(),
            },
        }
    }

    pub fn input(&mut self) -> &mut ChildStdin {
        self.input.as_mut().expect("relay input still open")
    }

    /// The relay's stdout and stdin, to read and write at once.
    pub fn io(&mut self) -> (&mut Timed, &mut ChildStdin) {
        let input = self.input.as_mut().expect("relay input still open");
        (&mut self.output, input)
    }

    /// End the relay's input, as a client that leaves.
    pub fn close_input(&mut self) {
        self.input = None;
    }

    /// Wait for the relay to exit.
    pub fn wait(&mut self) -> ExitStatus {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait for the relay") {
                return status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the relay did not exit"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Kill the relay process, as a dropped connection does.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A relay's stdout as a reader whose reads fail after [`WAIT`].
pub struct Timed {
    chunks: mpsc::Receiver<Vec<u8>>,
    pending: VecDeque<u8>,
}

impl Read for Timed {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pending.is_empty() {
            match self.chunks.recv_timeout(WAIT) {
                Ok(chunk) => self.pending.extend(chunk),
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(0),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(std::io::ErrorKind::TimedOut.into())
                }
            }
        }
        let n = buf.len().min(self.pending.len());
        for (slot, byte) in buf.iter_mut().zip(self.pending.drain(..n)) {
            *slot = byte;
        }
        Ok(n)
    }
}

/// Read a host's greeting — its hello, then its census — as raw bytes, and
/// check them byte for byte against the one codec: the header is the frozen
/// `[u32 len][u8 kind = 0x01][u32 channel = 0]`, the hello frame is exactly
/// what `codesense-host-proto` encodes for the hello it decodes to, and the
/// census is the frozen 13 bytes. Returns the hello and the census count.
pub fn read_greeting_golden(reader: &mut impl Read) -> (Hello, u32) {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len).expect("hello length");
    let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
    reader.read_exact(&mut body).expect("hello body");
    assert_eq!(body[0], 0x01, "the first frame is a HELLO");
    assert_eq!(body[1..5], [0, 0, 0, 0], "on the control channel");
    let wire: Vec<u8> = len.iter().chain(&body).copied().collect();

    let frame = read_frame(&mut wire.as_slice())
        .expect("a frame")
        .expect("not closed");
    assert_eq!(
        frame.encode(),
        wire,
        "the frame re-encodes to the wire bytes"
    );
    let hello = match read_hello(&mut wire.as_slice()).expect("a hello") {
        PeerHello::Compatible(hello) => *hello,
        other => panic!("the host speaks another major: {other:?}"),
    };
    assert_eq!(hello.proto_major, PROTO_MAJOR);
    assert_eq!(
        hello.frame().expect("encode").encode(),
        wire,
        "the hello on the wire is the codec's own encoding"
    );

    let mut census = [0u8; 13];
    reader.read_exact(&mut census).expect("census");
    assert_eq!(
        census[..9],
        [0, 0, 0, 9, 0x02, 0, 0, 0, 0],
        "the frozen census header"
    );
    let count = Census::from_frame(
        &read_frame(&mut census.as_slice())
            .expect("census frame")
            .expect("not closed"),
    )
    .expect("census");
    (hello, count.live_sessions)
}

/// A host's greeting — its hello frame and its census frame — as raw bytes.
pub fn greeting_bytes(reader: &mut impl Read) -> Vec<u8> {
    let mut wire = Vec::new();
    for _ in 0..2 {
        let mut len = [0u8; 4];
        reader.read_exact(&mut len).expect("frame length");
        let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
        reader.read_exact(&mut body).expect("frame body");
        wire.extend(len);
        wire.extend(body);
    }
    wire
}

/// Send `frame` through a relay.
pub fn send(relay: &mut Relay, frame: &Frame) {
    write_frame(relay.input(), frame).expect("write to the relay");
}
