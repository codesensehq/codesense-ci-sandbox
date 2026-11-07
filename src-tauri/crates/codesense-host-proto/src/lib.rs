// SPDX-License-Identifier: MIT
//! The one wire protocol a CodeSense host speaks to its clients, local or
//! remote: the session-server sidecar over its Unix socket today, a remote host
//! agent over an SSH stdio relay later. Both sides use this crate, so the codec
//! exists exactly once.
//!
//! - [`frame`]: the length-prefixed multiplexed frame
//!   `[u32 len][u8 kind][u32 channel][payload]`. Channel 0 carries control
//!   (JSON [`Control`] calls, replies and pushes); each output stream, and each
//!   session's input, gets a channel of its own carrying raw bytes.
//! - [`hello`]: the [`Hello`] each side sends as its first frame, the protocol
//!   version skew rule (minor = compatible, major = incompatible), and the
//!   capabilities a host advertises.
//! - [`census`]: the frozen live-session count a host sends right after its
//!   `Hello`, readable by a client of any version.
//!
//! What never changes, so a client of any version can always read a host of
//! any version far enough to report "restart terminal server (ends N
//! sessions)": the frame header, the `HELLO` and `CENSUS` kinds, `Hello`'s
//! `proto_major` field, and the census payload.

pub mod activity;
pub mod agent_cli;
pub mod census;
pub mod control;
pub mod frame;
pub mod hello;
pub mod inbox_wait;
pub mod machine_stats;
pub mod nofile;

pub use census::Census;
pub use control::{Control, Envelope};
pub use frame::{read_frame, write_frame, Frame, Kind, FIRST_STREAM_CHANNEL};
pub use hello::{
    greet, read_hello, read_host_greeting, BuildStamp, Capability, EmulatorHello, FrozenHostHello,
    GreetError, Hello, HostGreeting, PeerHello, Skew, SkewReason, PROTO_MAJOR, PROTO_MINOR,
    XTERM_VERSION,
};

#[cfg(unix)]
pub mod broker;
pub mod executor;
pub mod reverse_mcp;

pub mod scoped_mcp;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
