// SPDX-License-Identifier: MIT
//! The frame codec. Every message on a host connection, in both directions, is
//! one frame:
//!
//! ```text
//! [u32 len][u8 kind][u32 channel][payload]
//! ```
//!
//! Integers are big-endian. `len` counts every byte after itself (the kind, the
//! channel and the payload). This header is frozen: a client of any version can
//! delimit the frames of a host of any version.

use std::io::{Read, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// The control channel: [`Kind::HELLO`], [`Kind::CENSUS`] and
/// [`Kind::CONTROL`] frames travel here.
pub const CONTROL_CHANNEL: u32 = 0;

/// The first channel a side assigns to a byte stream: a host to an output
/// stream, a client to a session's input. Channel 1 is reserved for the
/// layout push.
pub const FIRST_STREAM_CHANNEL: u32 = 2;

/// Bytes of the header after `len`: the kind and the channel.
const HEADER_AFTER_LEN: usize = 1 + 4;

/// Largest frame either side accepts (`len`), so a peer cannot make the other
/// buffer without bound.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// What a frame's payload is. Unknown kinds are skipped by a reader, so a
/// newer minor version may add kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Kind(pub u8);

impl Kind {
    /// A JSON [`crate::Hello`]: the first frame each way. Frozen.
    pub const HELLO: Kind = Kind(0x01);
    /// The host's [`crate::Census`], right after its `Hello`. Frozen.
    pub const CENSUS: Kind = Kind(0x02);
    /// A JSON [`crate::Control`] message: a call, a reply or a push.
    pub const CONTROL: Kind = Kind(0x03);
    /// One batch of an output stream: `[u64 seq][raw bytes]`.
    pub const OUTPUT: Kind = Kind(0x04);
    /// One write to a session's input, client to host: `[u64 rid][raw
    /// bytes]`. Answered by the control reply carrying `rid`.
    pub const INPUT: Kind = Kind(0x05);
    pub const MCP_OPEN: Kind = Kind(0x06);
    pub const MCP_DATA: Kind = Kind(0x07);
    pub const MCP_CLOSE: Kind = Kind(0x08);
    pub const EXECUTOR_DATA: Kind = Kind(0x09);
    pub const EXECUTOR_CLOSE: Kind = Kind(0x0a);
}

/// One frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: Kind,
    pub channel: u32,
    pub payload: Vec<u8>,
}

impl Frame {
    /// A control-channel frame of `kind` carrying `value` as JSON.
    pub fn json<T: Serialize>(kind: Kind, value: &T) -> Result<Self, String> {
        Ok(Self {
            kind,
            channel: CONTROL_CHANNEL,
            payload: serde_json::to_vec(value).map_err(|e| format!("encode: {e}"))?,
        })
    }

    /// A [`Kind::CONTROL`] frame carrying `message`.
    pub fn control<T: Serialize>(message: &T) -> Result<Self, String> {
        Self::json(Kind::CONTROL, message)
    }

    /// One batch of the output stream on `channel`: its sequence number and
    /// its bytes, verbatim.
    pub fn output(channel: u32, seq: u64, data: &[u8]) -> Self {
        Self::numbered(Kind::OUTPUT, channel, seq, data)
    }

    /// One write to the session input on `channel`: the call id its reply
    /// answers, and its bytes, verbatim.
    pub fn input(channel: u32, rid: u64, data: &[u8]) -> Self {
        Self::numbered(Kind::INPUT, channel, rid, data)
    }

    /// A byte-stream frame: `[u64 number][raw bytes]`.
    fn numbered(kind: Kind, channel: u32, number: u64, data: &[u8]) -> Self {
        let mut payload = Vec::with_capacity(8 + data.len());
        payload.extend_from_slice(&number.to_be_bytes());
        payload.extend_from_slice(data);
        Self {
            kind,
            channel,
            payload,
        }
    }

    /// This frame, when it is of `kind`.
    pub fn expect(&self, kind: Kind) -> Result<&Self, String> {
        if self.kind == kind {
            Ok(self)
        } else {
            Err(format!(
                "expected a frame of kind {:#04x}, got {:#04x}",
                kind.0, self.kind.0
            ))
        }
    }

    /// The JSON payload as a `T`.
    pub fn decode_json<T: DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_slice(&self.payload).map_err(|e| format!("decode: {e}"))
    }

    /// An [`Kind::OUTPUT`] frame's sequence number and bytes.
    pub fn output_parts(&self) -> Result<(u64, &[u8]), String> {
        self.numbered_parts("output frame shorter than its sequence number")
    }

    /// An [`Kind::INPUT`] frame's call id and bytes.
    pub fn input_parts(&self) -> Result<(u64, &[u8]), String> {
        self.numbered_parts("input frame shorter than its call id")
    }

    fn numbered_parts(&self, short: &str) -> Result<(u64, &[u8]), String> {
        let (number, data) = self
            .payload
            .split_first_chunk::<8>()
            .ok_or_else(|| short.to_string())?;
        Ok((u64::from_be_bytes(*number), data))
    }

    /// The frame's wire bytes. Only [`write_frame`] checks the size limit.
    pub fn encode(&self) -> Vec<u8> {
        let len = HEADER_AFTER_LEN + self.payload.len();
        let mut bytes = Vec::with_capacity(4 + len);
        bytes.extend_from_slice(&(len as u32).to_be_bytes());
        bytes.push(self.kind.0);
        bytes.extend_from_slice(&self.channel.to_be_bytes());
        bytes.extend_from_slice(&self.payload);
        bytes
    }
}

/// Write `frame` with a single write, so frames from writers that share a
/// stream under a lock never interleave. A frame over [`MAX_FRAME_BYTES`],
/// which its reader would refuse, is not sent.
pub fn write_frame(writer: &mut impl Write, frame: &Frame) -> std::io::Result<()> {
    let len = HEADER_AFTER_LEN + frame.payload.len();
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("frame length {len} exceeds {MAX_FRAME_BYTES}"),
        ));
    }
    writer.write_all(&frame.encode())?;
    writer.flush()
}

/// Read the next frame. `Ok(None)` at a clean end of stream (between frames);
/// an I/O error, a stream cut mid-frame, or a frame over [`MAX_FRAME_BYTES`]
/// is an `Err`.
pub fn read_frame(reader: &mut impl Read) -> Result<Option<Frame>, String> {
    let mut len = [0u8; 4];
    let mut filled = 0;
    while filled < len.len() {
        match reader.read(&mut len[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => return Err("stream ended inside a frame header".to_string()),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let len = u32::from_be_bytes(len) as usize;
    if len < HEADER_AFTER_LEN {
        return Err(format!("frame length {len} is shorter than its header"));
    }
    if len > MAX_FRAME_BYTES {
        return Err(format!("frame length {len} exceeds {MAX_FRAME_BYTES}"));
    }
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .map_err(|e| format!("stream ended inside a frame: {e}"))?;
    let payload = body.split_off(HEADER_AFTER_LEN);
    Ok(Some(Frame {
        kind: Kind(body[0]),
        channel: u32::from_be_bytes([body[1], body[2], body[3], body[4]]),
        payload,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(bytes: &[u8]) -> Vec<Result<Option<Frame>, String>> {
        let mut reader = std::io::Cursor::new(bytes.to_vec());
        let mut out = Vec::new();
        loop {
            let next = read_frame(&mut reader);
            let done = !matches!(next, Ok(Some(_)));
            out.push(next);
            if done {
                return out;
            }
        }
    }

    /// Frames of every kind round-trip back to back, including an empty
    /// payload and output bytes that are not UTF-8 or contain newlines.
    #[test]
    fn frames_round_trip_back_to_back() {
        let frames = [
            Frame::control(&serde_json::json!({"type": "call", "rid": 1})).unwrap(),
            Frame::output(7, 42, b"\xff\x00line\nnext\r\n"),
            Frame {
                kind: Kind(0x7f),
                channel: 9,
                payload: Vec::new(),
            },
        ];
        let wire: Vec<u8> = frames.iter().flat_map(Frame::encode).collect();
        let read = read_all(&wire);
        assert_eq!(read.len(), frames.len() + 1);
        for (got, want) in read.iter().zip(&frames) {
            assert_eq!(got.as_ref().unwrap().as_ref(), Some(want));
        }
        assert_eq!(read.last(), Some(&Ok(None)));
    }

    /// The header layout: big-endian length of everything after it, the kind
    /// byte, the big-endian channel, then the payload.
    #[test]
    fn the_header_is_len_kind_channel() {
        let frame = Frame::output(0x0102_0304, 5, b"hi");
        assert_eq!(
            frame.encode(),
            [
                0, 0, 0, 15,   // len = 1 + 4 + 8 + 2
                0x04, // kind
                1, 2, 3, 4, // channel
                0, 0, 0, 0, 0, 0, 0, 5, // seq
                b'h', b'i',
            ]
        );
        assert_eq!(frame.output_parts(), Ok((5, &b"hi"[..])));
    }

    #[test]
    fn a_truncated_or_oversized_frame_is_an_error() {
        let whole = Frame::output(3, 1, b"abc").encode();
        for cut in [2, 4, 7, whole.len() - 1] {
            assert!(read_all(&whole[..cut])[0].is_err(), "cut at {cut}");
        }
        let short = [0, 0, 0, 4, 0x03, 0, 0, 0];
        assert!(read_all(&short)[0].is_err());
        let huge = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes();
        assert!(read_all(&huge)[0].is_err());
    }

    /// Input rides the same byte-stream layout as output, under its own kind,
    /// with the call id in place of the sequence number.
    #[test]
    fn an_input_frame_carries_its_call_id_and_raw_bytes() {
        let frame = Frame::input(3, 9, b"\x1b[A\r\xff");
        assert_eq!(frame.kind, Kind::INPUT);
        assert_eq!(
            frame.encode()[..13],
            [0, 0, 0, 18, 0x05, 0, 0, 0, 3, 0, 0, 0, 0]
        );
        assert_eq!(frame.input_parts(), Ok((9, &b"\x1b[A\r\xff"[..])));
        let short = Frame {
            kind: Kind::INPUT,
            channel: 3,
            payload: vec![1],
        };
        assert!(short.input_parts().is_err());
    }

    #[test]
    fn an_output_frame_without_a_sequence_number_is_refused() {
        let frame = Frame {
            kind: Kind::OUTPUT,
            channel: 2,
            payload: vec![1, 2, 3],
        };
        assert!(frame.output_parts().is_err());
    }

    /// A frame is written whole; one its reader would refuse is not written.
    #[test]
    fn write_frame_writes_only_frames_a_reader_accepts() {
        let frame = Frame::output(2, 9, b"x");
        let mut sink = Vec::new();
        write_frame(&mut sink, &frame).unwrap();
        assert_eq!(sink, frame.encode());

        let at_limit = Frame {
            kind: Kind::CONTROL,
            channel: 0,
            payload: vec![b' '; MAX_FRAME_BYTES - HEADER_AFTER_LEN],
        };
        let mut sink = Vec::new();
        write_frame(&mut sink, &at_limit).unwrap();
        assert_eq!(
            read_all(&sink)[0].as_ref().unwrap().as_ref(),
            Some(&at_limit)
        );

        let over = Frame {
            payload: vec![b' '; MAX_FRAME_BYTES - HEADER_AFTER_LEN + 1],
            ..at_limit
        };
        let mut sink = Vec::new();
        assert!(write_frame(&mut sink, &over).is_err());
        assert!(sink.is_empty(), "nothing of a refused frame is written");
    }

    #[test]
    fn expect_names_the_kind_it_wanted() {
        let frame = Frame::output(2, 0, b"");
        assert!(frame.expect(Kind::OUTPUT).is_ok());
        let error = frame.expect(Kind::HELLO).unwrap_err();
        assert!(error.contains("0x01") && error.contains("0x04"), "{error}");
    }
}
