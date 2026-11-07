// SPDX-License-Identifier: MIT
//! The control channel's messages. A client sends [`Control::Call`]s; the host
//! answers each with the [`Control::Reply`] carrying the same `rid` and sends
//! [`Control::Push`]es unprompted. Each service (terminals today) supplies its
//! own request, reply and push types; the envelope is shared.

use serde::{Deserialize, Serialize};

/// One control message. An `Err` reply carries the host's error text verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Control<Q, R, P> {
    Call { rid: u64, request: Q },
    Reply { rid: u64, result: Result<R, String> },
    Push { push: P },
}

/// A control message's envelope alone: what a peer's message was when this
/// build cannot read the rest of it (a newer minor's request, reply or push),
/// so a call or reply can still be answered on its own `rid`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Envelope {
    Call { rid: u64 },
    Reply { rid: u64 },
    Push {},
}

impl Envelope {
    /// The envelope of `frame`'s control message, if it has one.
    pub fn of(frame: &crate::Frame) -> Option<Self> {
        frame.decode_json().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Frame, Kind};

    type Message = Control<String, u32, bool>;

    #[test]
    fn control_messages_round_trip_in_a_control_frame() {
        for message in [
            Message::Call {
                rid: 4,
                request: "go".into(),
            },
            Message::Reply {
                rid: 4,
                result: Ok(1),
            },
            Message::Reply {
                rid: 5,
                result: Err("write_pty: session not found: s".into()),
            },
            Message::Push { push: true },
        ] {
            let frame = Frame::control(&message).unwrap();
            assert_eq!(frame.kind, Kind::CONTROL);
            assert_eq!(frame.channel, 0);
            assert_eq!(frame.decode_json::<Message>(), Ok(message));
        }
    }

    /// A message this build cannot read still names its kind and `rid`.
    #[test]
    fn an_unreadable_message_keeps_its_envelope() {
        let from_newer =
            |value: serde_json::Value| Envelope::of(&Frame::json(Kind::CONTROL, &value).unwrap());
        assert_eq!(
            from_newer(serde_json::json!({"type": "call", "rid": 7, "request": {"op": "new"}})),
            Some(Envelope::Call { rid: 7 })
        );
        assert_eq!(
            from_newer(serde_json::json!({"type": "reply", "rid": 8, "result": {"Ok": 1}})),
            Some(Envelope::Reply { rid: 8 })
        );
        assert_eq!(
            from_newer(serde_json::json!({"type": "push", "push": {"kind": "new"}})),
            Some(Envelope::Push {})
        );
        assert_eq!(from_newer(serde_json::json!({"rid": 1})), None);
    }
}
