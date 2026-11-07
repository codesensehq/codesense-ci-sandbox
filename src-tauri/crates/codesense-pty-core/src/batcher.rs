// SPDX-License-Identifier: MIT
//! The 16 ms PTY output batcher — receives raw byte chunks from the reader
//! thread, coalesces them, and hands coalesced valid-UTF-8 strings to an `emit`
//! callback.

use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Time window for coalescing PTY reads into a single IPC emit.
pub(crate) const PTY_BATCH_WINDOW: Duration = Duration::from_millis(16);

/// Idle timeout — if no data arrives within this duration while a batch is
/// pending, flush immediately. Keeps interactive typing latency low (~2ms
/// Rust-side) while still batching during sustained high-throughput output
/// (where chunks arrive faster than this interval).
pub(crate) const PTY_IDLE_TIMEOUT: Duration = Duration::from_millis(2);

/// Maximum bytes per batched emit — forces early flush even if the window
/// hasn't expired, preventing JSON serialization spikes on pathological output.
pub(crate) const PTY_MAX_BATCH_BYTES: usize = 65_536;

/// Drains `pending` into `emit` as UTF-8 text. Each invalid byte sequence is
/// replaced with U+FFFD and drained, so one bad byte can never wedge the
/// buffer; only an incomplete trailing sequence (at most 3 bytes) is held back
/// for the next call. Returns whether any replacement happened.
fn emit_valid_utf8(pending: &mut Vec<u8>, emit: &mut impl FnMut(&str)) -> bool {
    let mut out = String::new();
    let mut replaced = false;
    let mut start = 0;
    while start < pending.len() {
        match std::str::from_utf8(&pending[start..]) {
            Ok(rest) => {
                out.push_str(rest);
                start = pending.len();
            }
            Err(e) => {
                let valid = e.valid_up_to();
                // Safety of the unwrap: `valid_up_to` bytes are valid UTF-8.
                out.push_str(std::str::from_utf8(&pending[start..start + valid]).unwrap());
                start += valid;
                match e.error_len() {
                    Some(bad) => {
                        out.push(char::REPLACEMENT_CHARACTER);
                        start += bad;
                        replaced = true;
                    }
                    // A truncated sequence at the end may still complete with
                    // the next read.
                    None => break,
                }
            }
        }
    }
    pending.drain(..start);
    if !out.is_empty() {
        emit(&out);
    }
    replaced
}

/// Final flush at end of stream: anything still held is an incomplete sequence
/// that will never complete, so it is emitted lossily rather than dropped.
fn flush_lossy(pending: &mut Vec<u8>, emit: &mut impl FnMut(&str)) -> bool {
    let mut replaced = emit_valid_utf8(pending, emit);
    if !pending.is_empty() {
        emit(&String::from_utf8_lossy(pending));
        pending.clear();
        replaced = true;
    }
    replaced
}

/// Logs the first replacement of a session's stream only, so a binary `cat`
/// cannot flood the log.
fn note_replacement(replaced: bool, logged: &mut bool) {
    if replaced && !*logged {
        *logged = true;
        log::debug!(target: "pty", "replaced invalid UTF-8 in PTY output with U+FFFD");
    }
}

/// Drives the batching loop: receives raw byte chunks from the reader thread,
/// accumulates them, and calls `emit` with coalesced valid UTF-8 strings on
/// timer expiry, idle timeout, or channel close.
pub(crate) fn run_pty_batcher(
    rx: mpsc::Receiver<Option<Vec<u8>>>,
    mut emit: impl FnMut(&str),
    batch_window: Duration,
    idle_timeout: Duration,
    max_batch_bytes: usize,
) {
    let mut pending: Vec<u8> = Vec::new();
    let mut batch_start: Option<Instant> = None;
    let mut replacement_logged = false;

    loop {
        let timeout = match batch_start {
            Some(start) => {
                let elapsed = start.elapsed();
                let remaining = if elapsed >= batch_window {
                    Duration::ZERO
                } else {
                    batch_window - elapsed
                };
                remaining.min(idle_timeout)
            }
            None => Duration::from_secs(86_400),
        };

        match rx.recv_timeout(timeout) {
            Ok(Some(data)) => {
                pending.extend_from_slice(&data);
                if batch_start.is_none() {
                    batch_start = Some(Instant::now());
                }
                let window_expired = batch_start
                    .map(|s| s.elapsed() >= batch_window)
                    .unwrap_or(false);
                if window_expired || pending.len() >= max_batch_bytes {
                    let replaced = emit_valid_utf8(&mut pending, &mut emit);
                    note_replacement(replaced, &mut replacement_logged);
                    batch_start = if pending.is_empty() {
                        None
                    } else {
                        Some(Instant::now())
                    };
                }
            }
            Ok(None) => {
                // PTY closed — final flush
                let replaced = flush_lossy(&mut pending, &mut emit);
                note_replacement(replaced, &mut replacement_logged);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Batch window expired or idle with pending data
                let replaced = emit_valid_utf8(&mut pending, &mut emit);
                note_replacement(replaced, &mut replacement_logged);
                batch_start = None;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Reader thread dropped — final flush
                let replaced = flush_lossy(&mut pending, &mut emit);
                note_replacement(replaced, &mut replacement_logged);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Multiple rapid sends coalesce into a single emit.
    #[test]
    fn batch_coalesces_within_window() {
        let (tx, rx) = mpsc::channel();
        tx.send(Some(b"hello ".to_vec())).unwrap();
        tx.send(Some(b"world".to_vec())).unwrap();
        tx.send(None).unwrap();

        let mut emits = Vec::new();
        run_pty_batcher(
            rx,
            |d| emits.push(d.to_string()),
            Duration::from_millis(100),
            Duration::from_millis(100), // idle = window (coalescing test)
            PTY_MAX_BATCH_BYTES,
        );
        assert_eq!(emits, vec!["hello world"]);
    }

    /// An idle gap (no data for > idle timeout) triggers a flush before more data arrives.
    #[test]
    fn batch_idle_flush_on_gap() {
        let (tx, rx) = mpsc::channel();
        let emits = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let ec = emits.clone();

        let handle = std::thread::spawn(move || {
            let mut local = Vec::new();
            run_pty_batcher(
                rx,
                |d| local.push(d.to_string()),
                Duration::from_millis(500),
                Duration::from_millis(20), // idle timeout triggers flush
                PTY_MAX_BATCH_BYTES,
            );
            *ec.lock().unwrap() = local;
        });

        tx.send(Some(b"A".to_vec())).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        tx.send(Some(b"B".to_vec())).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        tx.send(None).unwrap();

        handle.join().unwrap();
        let emits = emits.lock().unwrap();
        assert!(emits.len() >= 2, "expected >=2 emits, got: {:?}", *emits);
        assert_eq!(emits[0], "A");
    }

    /// Incomplete UTF-8 at the end of a batch is retained until completed.
    #[test]
    fn batch_retains_incomplete_utf8() {
        let (tx, rx) = mpsc::channel();
        // é = [0xC3, 0xA9], split across chunks
        tx.send(Some(vec![b'h', b'i', 0xC3])).unwrap();
        tx.send(Some(vec![0xA9])).unwrap();
        tx.send(None).unwrap();

        let mut emits = Vec::new();
        run_pty_batcher(
            rx,
            |d| emits.push(d.to_string()),
            Duration::from_millis(100),
            Duration::from_millis(100),
            PTY_MAX_BATCH_BYTES,
        );
        assert_eq!(emits.join(""), "hié");
    }

    /// Incomplete UTF-8 is retained across idle flush boundaries.
    #[test]
    fn batch_incomplete_utf8_across_flush() {
        let (tx, rx) = mpsc::channel();
        let emits = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let ec = emits.clone();

        let handle = std::thread::spawn(move || {
            let mut local = Vec::new();
            run_pty_batcher(
                rx,
                |d| local.push(d.to_string()),
                Duration::from_millis(500),
                Duration::from_millis(20),
                PTY_MAX_BATCH_BYTES,
            );
            *ec.lock().unwrap() = local;
        });

        // "hi" + first byte of é
        tx.send(Some(vec![b'h', b'i', 0xC3])).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        // Second byte of é
        tx.send(Some(vec![0xA9])).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        tx.send(None).unwrap();

        handle.join().unwrap();
        let emits = emits.lock().unwrap();
        assert_eq!(emits.join(""), "hié");
        assert!(emits.len() >= 2, "expected >=2 emits, got: {:?}", *emits);
        assert_eq!(emits[0], "hi");
    }

    /// MAX_BATCH_BYTES forces early emit even within a long window.
    #[test]
    fn batch_max_bytes_forces_early_emit() {
        let (tx, rx) = mpsc::channel();
        let chunk = vec![b'x'; 100];
        for _ in 0..5 {
            tx.send(Some(chunk.clone())).unwrap();
        }
        tx.send(None).unwrap();

        let mut emits = Vec::new();
        run_pty_batcher(
            rx,
            |d| emits.push(d.to_string()),
            Duration::from_secs(10),
            Duration::from_secs(10),
            200,
        );

        assert!(
            emits.len() >= 2,
            "expected >=2 emits due to max bytes, got: {:?}",
            emits
        );
        let total: usize = emits.iter().map(|s| s.len()).sum();
        assert_eq!(total, 500);
    }

    fn drain_all(pending: &mut Vec<u8>) -> String {
        let mut out = String::new();
        emit_valid_utf8(pending, &mut |d| out.push_str(d));
        out
    }

    fn run_to_close(chunks: &[&[u8]]) -> Vec<String> {
        let (tx, rx) = mpsc::channel();
        for chunk in chunks {
            tx.send(Some(chunk.to_vec())).unwrap();
        }
        tx.send(None).unwrap();
        let mut emits = Vec::new();
        run_pty_batcher(
            rx,
            |d| emits.push(d.to_string()),
            Duration::from_millis(100),
            Duration::from_millis(100),
            PTY_MAX_BATCH_BYTES,
        );
        emits
    }

    /// A lone invalid byte becomes U+FFFD and the text after it still flows.
    #[test]
    fn invalid_byte_is_replaced() {
        let mut pending = b"ok\xFFafter".to_vec();
        assert_eq!(drain_all(&mut pending), "ok\u{FFFD}after");
        assert!(pending.is_empty(), "nothing is held back: {pending:?}");
    }

    /// A codepoint split across two reads is held, then emitted whole.
    #[test]
    fn split_multibyte_is_emitted_whole() {
        let mut pending = vec![0xC3];
        assert_eq!(drain_all(&mut pending), "");
        assert_eq!(pending, vec![0xC3]);
        pending.push(0xA9);
        assert_eq!(drain_all(&mut pending), "é");
        assert!(pending.is_empty());
    }

    /// Seeded random chunkings of mixed valid multibyte text and injected
    /// invalid bytes decode exactly like a one-shot lossy decode of the whole
    /// input — no dropped bytes, no double replacement at chunk seams, and the
    /// held tail never exceeds one incomplete sequence.
    #[test]
    fn lossy_equivalence_prop_seed_20260930() {
        let mut state: u64 = 20_260_930;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let atoms: [&[u8]; 12] = [
            b"a",
            b"xyz ",
            "é".as_bytes(),
            "€".as_bytes(),
            "𝄞".as_bytes(),
            "日本".as_bytes(),
            b"\x1b[31m",
            b"\xFF",
            b"\xC3",
            b"\xE2\x82",
            b"\xF0\x9D\x84",
            b"\x80",
        ];
        for _ in 0..500 {
            let mut input = Vec::new();
            for _ in 0..(next() % 40) {
                input.extend_from_slice(atoms[(next() % atoms.len() as u64) as usize]);
            }
            let expected = String::from_utf8_lossy(&input).into_owned();

            let mut pending = Vec::new();
            let mut emitted = String::new();
            let mut at = 0;
            while at < input.len() {
                let len = 1 + (next() % 7) as usize;
                let end = (at + len).min(input.len());
                pending.extend_from_slice(&input[at..end]);
                at = end;
                emitted.push_str(&drain_all(&mut pending));
                assert!(pending.len() <= 3, "held tail too long: {pending:?}");
            }
            let tail = String::from_utf8_lossy(&pending).into_owned();
            assert_eq!(format!("{emitted}{tail}"), expected, "input {input:?}");

            let mut flushed = emitted.clone();
            flush_lossy(&mut pending, &mut |d| flushed.push_str(d));
            assert!(pending.is_empty());
            assert_eq!(flushed, expected, "input {input:?}");
            assert_eq!(
                flushed.chars().count(),
                expected.chars().count(),
                "input {input:?}"
            );
        }
    }

    /// A bad byte inside an escape sequence is replaced like any other and
    /// the output after it is delivered.
    #[test]
    fn invalid_byte_inside_csi() {
        let emits = run_to_close(&[b"\x1b[3\xff1m", b"DONE"]);
        let all = emits.join("");
        assert_eq!(all, "\x1b[3\u{FFFD}1mDONE");
        assert!(all.ends_with("DONE"));
    }

    /// An incomplete sequence still held when the stream closes is emitted as
    /// one U+FFFD before the batcher returns, never silently dropped.
    #[test]
    fn held_tail_flushed_on_close() {
        let emits = run_to_close(&[b"ab\xE2\x82"]);
        let all = emits.join("");
        assert_eq!(all, "ab\u{FFFD}");
        assert_eq!(all.matches('\u{FFFD}').count(), 1);
    }
}
