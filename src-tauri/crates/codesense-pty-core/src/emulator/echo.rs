// SPDX-License-Identifier: MIT
//! A stand-in emulator worker for tests of the code above a host: it answers
//! every request in order, like the real worker, but its "terminal" is only the
//! text fed to it since its `create`. A snapshot's `vt` is that text, so a test
//! can tell which output a snapshot holds without running xterm.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{mpsc, Arc};

use serde_json::{json, Value};

use super::EmulatorLink;

/// Attach an echo worker to `link`; returns its generation.
pub fn attach_echo(link: &Arc<EmulatorLink>) -> u64 {
    let (replies, frames) = mpsc::channel();
    link.attach(
        EchoStdin {
            pending: Vec::new(),
            terminals: HashMap::new(),
            replies,
        },
        frames,
    )
    .expect("attach the echo worker")
}

struct EchoTerminal {
    cols: u64,
    rows: u64,
    scrollback: u64,
    seq: u64,
    resize_gen: u64,
    history_lost: bool,
    text: String,
}

impl EchoTerminal {
    fn lines(&self, count: usize) -> Vec<String> {
        let lines: Vec<String> = self
            .text
            .split('\n')
            .map(|line| line.trim_end_matches('\r').to_string())
            .collect();
        lines[lines.len().saturating_sub(count)..].to_vec()
    }
}

struct EchoStdin {
    pending: Vec<u8>,
    terminals: HashMap<u64, EchoTerminal>,
    replies: mpsc::Sender<Vec<u8>>,
}

impl EchoStdin {
    fn reply(&self, reply: Value) {
        let _ = self.replies.send(reply.to_string().into_bytes());
    }

    fn handle(&mut self, request: Value) {
        let session = request["session"].as_u64().unwrap_or_default();
        let id = request["id"].as_u64();
        let kind = request["type"].as_str().unwrap_or_default().to_string();
        if kind == "create" {
            self.terminals.insert(
                session,
                EchoTerminal {
                    cols: request["cols"].as_u64().unwrap_or_default(),
                    rows: request["rows"].as_u64().unwrap_or_default(),
                    scrollback: request["scrollback"].as_u64().unwrap_or_default(),
                    seq: request["seq_base"].as_u64().unwrap_or_default(),
                    resize_gen: request["resize_gen_base"].as_u64().unwrap_or_default(),
                    history_lost: request["history_lost"].as_bool().unwrap_or_default(),
                    text: String::new(),
                },
            );
            return;
        }
        if kind == "dispose" {
            self.terminals.remove(&session);
            return;
        }
        let Some(terminal) = self.terminals.get_mut(&session) else {
            self.reply(json!({"type": "error", "id": id, "message": "unknown session"}));
            return;
        };
        let reply = match kind.as_str() {
            "feed" => {
                terminal
                    .text
                    .push_str(request["data"].as_str().unwrap_or_default());
                terminal.seq = request["seq_end"].as_u64().unwrap_or_default();
                json!({"type": "applied", "session": session, "seq_end": terminal.seq, "apply_ms": 0.0})
            }
            "resize" => {
                terminal.cols = request["cols"].as_u64().unwrap_or_default();
                terminal.rows = request["rows"].as_u64().unwrap_or_default();
                terminal.resize_gen = request["resize_gen"].as_u64().unwrap_or_default();
                return;
            }
            "respawn" => {
                terminal.seq = request["seq_base"].as_u64().unwrap_or_default();
                terminal.resize_gen = request["resize_gen_base"].as_u64().unwrap_or_default();
                return;
            }
            "snapshot" => json!({
                "type": "snapshot",
                "id": id,
                "session": session,
                "snapshot": {
                    "cols": terminal.cols,
                    "rows": terminal.rows,
                    "seq": terminal.seq,
                    "resize_gen": terminal.resize_gen,
                    "scrollback": terminal.scrollback,
                    "vt": terminal.text,
                },
            }),
            "text" => json!({
                "type": "text",
                "id": id,
                "session": session,
                "seq_end": terminal.seq,
                "resize_gen": terminal.resize_gen,
                "alt_screen": false,
                "lines": terminal.lines(request["lines"].as_u64().unwrap_or_default() as usize),
            }),
            "dump" => json!({
                "type": "dump",
                "id": id,
                "session": session,
                "seq_end": terminal.seq,
                "resize_gen": terminal.resize_gen,
                "history_lost": terminal.history_lost,
                "cols": terminal.cols,
                "rows": terminal.rows,
                "lines": terminal.lines(usize::MAX),
            }),
            other => {
                json!({"type": "error", "id": id, "message": format!("unknown request {other}")})
            }
        };
        self.reply(reply);
    }
}

impl Write for EchoStdin {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buf);
        while self.pending.len() >= 4 {
            let length = u32::from_be_bytes(self.pending[..4].try_into().unwrap()) as usize;
            if self.pending.len() < 4 + length {
                break;
            }
            let frame: Vec<u8> = self.pending.drain(..4 + length).skip(4).collect();
            if let Ok(request) = serde_json::from_slice::<Value>(&frame) {
                self.handle(request);
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
