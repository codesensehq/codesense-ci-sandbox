// SPDX-License-Identifier: MIT
//! Dev-only byte recorder: with `CODESENSE_PTY_RECORD=<dir>` set, every session
//! writes one `<session>.ptylog` of the exact stream a display receives, in
//! stream order. Compiled only with `debug_assertions`.
//!
//! Line format: see [`crate::ptylog`]. `seq_end` is the session's absolute
//! appended byte count at the end of the batch.
//!
//! Both are written under the per-session lock at the point the stream is
//! ordered, so the file order is the order a display would apply them in.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use log::warn;

use crate::ptylog::{output_line, resize_line};

/// The environment variable naming the directory recordings go to.
pub(crate) const RECORD_DIR_ENV: &str = "CODESENSE_PTY_RECORD";

pub(crate) struct PtyRecorder {
    file: File,
}

impl PtyRecorder {
    /// A recorder for `session_id` when [`RECORD_DIR_ENV`] is set, else `None`.
    /// An unopenable file is logged and recording is skipped for the session.
    pub(crate) fn from_env(session_id: &str) -> Option<Self> {
        let dir = std::env::var_os(RECORD_DIR_ENV).filter(|dir| !dir.is_empty())?;
        match Self::create(Path::new(&dir), session_id) {
            Ok(recorder) => Some(recorder),
            Err(error) => {
                warn!(target: "pty", "pty recorder: cannot record {session_id}: {error}");
                None
            }
        }
    }

    /// A new recording for `session_id` in `dir`. A session id reused by a
    /// respawn gets the next free `<session>.<n>.ptylog`, never an overwrite.
    pub(crate) fn create(dir: &Path, session_id: &str) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let stem = file_stem(session_id);
        for n in 0u32.. {
            let path: PathBuf = if n == 0 {
                dir.join(format!("{stem}.ptylog"))
            } else {
                dir.join(format!("{stem}.{n}.ptylog"))
            };
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => return Ok(Self { file }),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        unreachable!("u32 range exhausted")
    }

    pub(crate) fn output(&mut self, seq_end: u64, data: &[u8]) {
        self.write_line(&output_line(seq_end, data));
    }

    pub(crate) fn resize(&mut self, at_seq: u64, cols: u16, rows: u16) {
        self.write_line(&resize_line(at_seq, cols, rows));
    }

    /// One unbuffered write per record, so a crash never leaves a torn line
    /// behind a later one.
    fn write_line(&mut self, line: &str) {
        if let Err(error) = self.file.write_all(line.as_bytes()) {
            warn!(target: "pty", "pty recorder: write failed: {error}");
        }
    }
}

/// A session id as a file name: anything outside `[A-Za-z0-9._-]` becomes `_`.
fn file_stem(session_id: &str) -> String {
    let stem: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if stem.is_empty() || stem.starts_with('.') {
        format!("session{stem}")
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_become_safe_file_names() {
        assert_eq!(file_stem("pty-1a2b"), "pty-1a2b");
        assert_eq!(file_stem("team:ws/tab 1"), "team_ws_tab_1");
        assert_eq!(file_stem("../x"), "session.._x");
        assert_eq!(file_stem(""), "session");
    }

    #[test]
    fn a_reused_session_id_never_overwrites_its_earlier_recording() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = PtyRecorder::create(dir.path(), "s").unwrap();
        first.output(1, b"a");
        let mut second = PtyRecorder::create(dir.path(), "s").unwrap();
        second.output(1, b"b");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("s.ptylog")).unwrap(),
            output_line(1, b"a")
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("s.1.ptylog")).unwrap(),
            output_line(1, b"b")
        );
    }
}
