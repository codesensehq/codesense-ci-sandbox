// SPDX-License-Identifier: MIT
//! The worker's framing and messages: a 4-byte big-endian payload length, then
//! a JSON payload. `src-emulator/frame.ts` is the other end and changes with
//! this file.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

/// Largest payload either side accepts; a bigger length means a corrupt stream.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// The id of the `hello` every worker answers first. Replies are matched by id,
/// and `hello` is always the first exchange with a fresh worker, so it needs no
/// counter.
pub const HELLO_ID: u64 = 0;

/// Write one frame carrying `payload`.
pub fn write_frame(out: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .ok()
        .filter(|_| payload.len() <= MAX_FRAME_BYTES)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("frame of {} bytes exceeds {MAX_FRAME_BYTES}", payload.len()),
            )
        })?;
    out.write_all(&length.to_be_bytes())?;
    out.write_all(payload)?;
    out.flush()
}

/// Read one frame's payload. `None` when the stream ends cleanly between
/// frames; an end inside a frame is an error.
pub fn read_frame(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        match input.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {length} exceeds {MAX_FRAME_BYTES}"),
        ));
    }
    let mut payload = vec![0u8; length];
    input.read_exact(&mut payload)?;
    Ok(Some(payload))
}

/// A request to the worker. `session` is the host's key for a session's
/// terminal. `seq` values are byte offsets over the session's whole output and
/// `resize_gen` counts its applied size changes; the host owns both and the
/// worker only echoes them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello {
        id: u64,
    },
    /// A new terminal at `seq_base` / `resize_gen_base`. `history_lost`: the
    /// output before `seq_base` never reached this worker.
    Create {
        session: u64,
        cols: u16,
        rows: u16,
        scrollback: u32,
        seq_base: u64,
        resize_gen_base: u32,
        history_lost: bool,
    },
    /// Output delivered to the session's displays, ending the stream at
    /// `seq_end`.
    Feed {
        session: u64,
        seq_end: u64,
        data: String,
    },
    Resize {
        session: u64,
        cols: u16,
        rows: u16,
        resize_gen: u32,
    },
    Dump {
        id: u64,
        session: u64,
    },
    Text {
        id: u64,
        session: u64,
        lines: u32,
        mode: TextMode,
    },
    Snapshot {
        id: u64,
        session: u64,
    },
    /// A new process in the session's terminal, its output starting at
    /// `seq_base`.
    Respawn {
        session: u64,
        seq_base: u64,
        resize_gen_base: u32,
    },
    Dispose {
        session: u64,
    },
}

/// What a `text` request reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextMode {
    /// The last lines of the normal buffer, soft-wrapped rows joined.
    Normal,
    /// The rows on screen in the active buffer (the alternate screen while
    /// it is active).
    Screen,
}

/// What a worker reports about itself in its `hello` reply.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorkerInfo {
    /// `bun` or `node`.
    pub runtime: String,
    /// The runtime's own version.
    pub version: String,
    /// The `@xterm/headless` version bundled into the worker.
    pub xterm_version: String,
    /// The worker's message-set version.
    pub worker_wire_version: u32,
}

/// A session's terminal as the worker dumps it: the frontend's
/// `referenceBuffer()` shape verbatim, plus the position it describes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct EmulatorDump {
    pub id: u64,
    pub session: u64,
    /// The last parsed byte offset.
    pub seq_end: u64,
    pub resize_gen: u32,
    pub history_lost: bool,
    /// `referenceBuffer()`'s fields.
    #[serde(flatten)]
    pub buffer: serde_json::Map<String, serde_json::Value>,
}

/// A session's terminal as text.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EmulatorText {
    pub id: u64,
    pub session: u64,
    pub seq_end: u64,
    pub resize_gen: u32,
    pub alt_screen: bool,
    pub lines: Vec<String>,
}

/// A session's terminal serialized, at the position the snapshot names.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EmulatorSnapshot {
    pub id: u64,
    pub session: u64,
    pub snapshot: super::TerminalSnapshot,
}

/// A reply from the worker.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    Hello {
        id: u64,
        #[serde(flatten)]
        info: WorkerInfo,
    },
    /// Sent unasked once a `feed` has been parsed.
    Applied {
        session: u64,
        seq_end: u64,
        /// Milliseconds from xterm's `write` to its callback.
        apply_ms: f64,
    },
    Dump(EmulatorDump),
    Text(EmulatorText),
    Snapshot(EmulatorSnapshot),
    Error {
        id: Option<u64>,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_a_clean_end_is_none() {
        let mut stream = Vec::new();
        write_frame(&mut stream, b"one").unwrap();
        write_frame(&mut stream, b"").unwrap();
        assert_eq!(&stream[..4], &[0, 0, 0, 3]);
        let mut input = stream.as_slice();
        assert_eq!(
            read_frame(&mut input).unwrap().as_deref(),
            Some(&b"one"[..])
        );
        assert_eq!(read_frame(&mut input).unwrap().as_deref(), Some(&b""[..]));
        assert!(read_frame(&mut input).unwrap().is_none());
    }

    #[test]
    fn an_end_inside_a_frame_is_an_error() {
        let mut header_only: &[u8] = &[0, 0];
        assert!(read_frame(&mut header_only).is_err());
        let mut short_payload: &[u8] = &[0, 0, 0, 5, b'a'];
        assert!(read_frame(&mut short_payload).is_err());
    }

    #[test]
    fn an_oversized_length_is_refused_before_allocating() {
        let mut input: &[u8] = &[0xff, 0xff, 0xff, 0xff];
        let error = read_frame(&mut input).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn the_worker_hello_reply_parses() {
        let reply: Reply = serde_json::from_str(
            r#"{"id":0,"type":"hello","runtime":"bun","version":"1.4.2","xterm_version":"6.0.0","worker_wire_version":1}"#,
        )
        .unwrap();
        let Reply::Hello { id, info } = reply else {
            panic!("not a hello: {reply:?}");
        };
        assert_eq!(id, HELLO_ID);
        assert_eq!(info.runtime, "bun");
        assert_eq!(info.xterm_version, "6.0.0");
        assert_eq!(info.worker_wire_version, 1);
        assert_eq!(
            serde_json::to_string(&Request::Hello { id: HELLO_ID }).unwrap(),
            r#"{"type":"hello","id":0}"#
        );
    }

    #[test]
    fn session_messages_match_the_worker_protocol() {
        let feed = Request::Feed {
            session: 3,
            seq_end: 10,
            data: "\u{1b}[1mhi".into(),
        };
        assert_eq!(
            serde_json::to_string(&feed).unwrap(),
            r#"{"type":"feed","session":3,"seq_end":10,"data":"\u001b[1mhi"}"#
        );
        let text = Request::Text {
            id: 4,
            session: 3,
            lines: 20,
            mode: TextMode::Screen,
        };
        assert_eq!(
            serde_json::to_string(&text).unwrap(),
            r#"{"type":"text","id":4,"session":3,"lines":20,"mode":"screen"}"#
        );
        let reply: Reply = serde_json::from_str(
            r#"{"type":"dump","id":4,"session":3,"seq_end":10,"resize_gen":1,"history_lost":false,"lines":["hi"],"cols":80}"#,
        )
        .unwrap();
        let Reply::Dump(dump) = reply else {
            panic!("not a dump: {reply:?}");
        };
        assert_eq!((dump.id, dump.seq_end, dump.resize_gen), (4, 10, 1));
        assert_eq!(dump.buffer.len(), 2, "only referenceBuffer() fields remain");
        assert_eq!(dump.buffer["cols"], 80);
    }

    #[test]
    fn snapshot_request_and_reply_match_the_worker_protocol() {
        assert_eq!(
            serde_json::to_string(&Request::Snapshot { id: 5, session: 3 }).unwrap(),
            r#"{"type":"snapshot","id":5,"session":3}"#
        );
        let reply: Reply = serde_json::from_str(
            r#"{"id":5,"type":"snapshot","session":3,"snapshot":{"cols":80,"rows":24,"seq":10,"resize_gen":1,"scrollback":5000,"vt":"hi\u001b[1;3H"}}"#,
        )
        .unwrap();
        let Reply::Snapshot(reply) = reply else {
            panic!("not a snapshot: {reply:?}");
        };
        assert_eq!((reply.id, reply.session), (5, 3));
        assert_eq!(
            reply.snapshot,
            super::super::TerminalSnapshot {
                cols: 80,
                rows: 24,
                seq: 10,
                resize_gen: 1,
                scrollback: 5000,
                vt: "hi\u{1b}[1;3H".into(),
            }
        );
    }
}
