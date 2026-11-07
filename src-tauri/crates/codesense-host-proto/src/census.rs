// SPDX-License-Identifier: MIT
//! The census: how many sessions a host is running. A host sends it right after
//! its `Hello` on every connection, so a client whose protocol major differs
//! from the host's can still say "restart terminal server (ends N sessions)".
//!
//! FROZEN. Its frame is `[u32 len = 9][u8 kind = 0x02][u32 channel = 0][u32
//! live_sessions]`, big-endian, and never changes in any protocol version;
//! `census_frame_bytes_are_frozen` pins it.

use crate::frame::{Frame, Kind, CONTROL_CHANNEL};

/// A host's live-session count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Census {
    pub live_sessions: u32,
}

impl Census {
    /// The census frame.
    pub fn frame(self) -> Frame {
        Frame {
            kind: Kind::CENSUS,
            channel: CONTROL_CHANNEL,
            payload: self.live_sessions.to_be_bytes().to_vec(),
        }
    }

    /// Read a census frame.
    pub fn from_frame(frame: &Frame) -> Result<Self, String> {
        let count: [u8; 4] = frame
            .expect(Kind::CENSUS)?
            .payload
            .as_slice()
            .try_into()
            .map_err(|_| format!("census payload is {} bytes, not 4", frame.payload.len()))?;
        Ok(Self {
            live_sessions: u32::from_be_bytes(count),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The census layout must never change: a newer client relies on it to
    /// count an older host's sessions. If this fails, the change is wrong.
    #[test]
    fn census_frame_bytes_are_frozen() {
        let frame = Census {
            live_sessions: 0x0102_0304,
        }
        .frame();
        assert_eq!(
            frame.encode(),
            [0, 0, 0, 9, 0x02, 0, 0, 0, 0, 0x01, 0x02, 0x03, 0x04]
        );
        assert_eq!(
            Census { live_sessions: 3 }.frame().encode(),
            [0, 0, 0, 9, 0x02, 0, 0, 0, 0, 0, 0, 0, 3]
        );
    }

    #[test]
    fn a_census_round_trips_and_nothing_else_reads_as_one() {
        let census = Census { live_sessions: 7 };
        assert_eq!(Census::from_frame(&census.frame()), Ok(census));
        let mut long = census.frame();
        long.payload.push(0);
        assert!(Census::from_frame(&long).is_err());
        assert!(Census::from_frame(&Frame::output(2, 0, b"")).is_err());
    }
}
