// SPDX-License-Identifier: MIT
//! Finite framing budget for the official Codex executor's JSON-line transport.
pub const CHUNK_BYTES: usize = 16 * 1024;
pub const LINE_BYTES: usize = 4 * 1024 * 1024;
pub const QUEUE_CHUNKS: usize = 4;
pub const MAX_EXECUTORS: usize = 8;

pub const MAX_CONNECTIONS: usize = 32;
