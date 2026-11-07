// SPDX-License-Identifier: MIT
//! Records of MCP-bridge disconnects and reconnects, and of connection errors
//! the session server would otherwise discard. Each record is one line appended
//! to `<root>/logs/mcp-bridge-disconnect.log`, which rotates to `.1` once it
//! reaches [`MAX_BYTES`]. The host's own `pty-server.log` is cut down in place
//! by [`cap_in_place`] before each launch.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Serializes the rotate and append of every record in this process, so one
/// connection's rename cannot land between another's size check and its write.
static DISCONNECT_LOG_LOCK: Mutex<()> = Mutex::new(());

/// The disconnect log's file name under the instance's `logs` directory.
pub const FILE_NAME: &str = "mcp-bridge-disconnect.log";
/// The host's stderr log, under the same `logs` directory.
pub const SERVER_LOG_NAME: &str = "pty-server.log";
/// Size at which the disconnect log rotates, and at which `pty-server.log` is cut down.
pub const MAX_BYTES: u64 = 1024 * 1024;
/// What `pty-server.log` keeps of its tail when it is cut down.
pub const KEEP_TAIL_BYTES: u64 = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Disconnect,
    Reconnect,
    AcceptRefused,
    WriteFailed,
}

/// Where the disconnect log lives under an instance root.
pub fn log_path(root: &Path) -> PathBuf {
    root.join("logs").join(FILE_NAME)
}

/// The `session=` field of a record: the viewer's id once it is known, else the
/// connection's id, which restarts at 1 with each process.
pub fn session_label(viewer: Option<&str>, connection: u64) -> String {
    viewer.map_or_else(|| format!("conn-{connection}"), str::to_string)
}

/// Append `event` for `session` to the disconnect log under `root`. A reason
/// of `None` is written as `unknown`. A failure to write is logged, not dropped.
pub fn record(root: &Path, session: &str, event: Event, reason: Option<&str>) {
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    if let Err(err) = append(&log_path(root), session, event, reason, &timestamp) {
        log::warn!("could not record {event:?} for session {session}: {err}");
    }
}

/// Append one record to the log file at `path`, using `timestamp` as given.
fn append(
    path: &Path,
    session: &str,
    event: Event,
    reason: Option<&str>,
    timestamp: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _guard = DISCONNECT_LOG_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    rotate_if_full(path)?;
    // A reason is one line: a newline inside it would split the record.
    let reason = reason.map_or_else(|| "unknown".to_string(), |r| r.replace(['\r', '\n'], " "));
    let name = match event {
        Event::Disconnect => "disconnect",
        Event::Reconnect => "reconnect",
        Event::AcceptRefused => "accept_refused",
        Event::WriteFailed => "write_failed",
    };
    let line = format!("{timestamp} session={session} event={name} reason={reason}\n");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(line.as_bytes())
}

/// Move a full log aside to `<FILE_NAME>.1`, replacing any earlier one.
fn rotate_if_full(path: &Path) -> std::io::Result<()> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() >= MAX_BYTES => {
            std::fs::rename(path, path.with_file_name(format!("{FILE_NAME}.1")))
        }
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Cut `path` down to its last [`KEEP_TAIL_BYTES`] once it reaches
/// [`MAX_BYTES`]. Done in place, so the file keeps its name and any writer
/// appending to it keeps working. A smaller file is left alone.
///
/// The tail is written at offset 0 before the file is cut, so a failure never
/// leaves it empty. A failure or crash in between can leave the new tail, a torn
/// line at the seam and older bytes after it. The file stays at or above the cap,
/// so the next launch caps it again.
pub fn cap_in_place(path: &Path) -> std::io::Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let len = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if len < MAX_BYTES {
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    file.seek(SeekFrom::Start(len - KEEP_TAIL_BYTES))?;
    let mut tail = Vec::with_capacity(KEEP_TAIL_BYTES as usize);
    file.read_to_end(&mut tail)?;
    // Start at the first whole line of the tail.
    if let Some(newline) = tail.iter().position(|&byte| byte == b'\n') {
        tail.drain(..=newline);
    }
    // Write from the start before cutting the length, so a failed write leaves
    // the file holding data rather than empty.
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&tail)?;
    file.set_len(tail.len() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_log() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = log_path(dir.path());
        (dir, path)
    }

    #[test]
    fn a_disconnect_writes_one_line_with_its_session_and_event() {
        let (_dir, path) = temp_log();
        append(
            &path,
            "sess-7",
            Event::Disconnect,
            Some("socket closed"),
            "2026-10-17T00:00:00Z",
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "2026-10-17T00:00:00Z session=sess-7 event=disconnect reason=socket closed\n"
        );
    }

    #[test]
    fn a_missing_reason_is_written_as_unknown() {
        let (_dir, path) = temp_log();
        append(&path, "sess-7", Event::AcceptRefused, None, "t").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text, "t session=sess-7 event=accept_refused reason=unknown\n");
    }

    #[test]
    fn the_disconnect_log_rotates_exactly_at_one_mib() {
        let (_dir, path) = temp_log();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let rotated = path.with_file_name(format!("{FILE_NAME}.1"));

        std::fs::write(&path, vec![b'x'; (MAX_BYTES - 1) as usize]).unwrap();
        append(&path, "s", Event::Disconnect, None, "t").unwrap();
        assert!(!rotated.exists(), "a file one byte under 1 MiB does not rotate");

        std::fs::write(&path, vec![b'x'; MAX_BYTES as usize]).unwrap();
        append(&path, "s2", Event::Reconnect, None, "t").unwrap();
        assert_eq!(std::fs::metadata(&rotated).unwrap().len(), MAX_BYTES);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "t session=s2 event=reconnect reason=unknown\n",
            "the new file starts with only the new record"
        );
    }

    #[test]
    fn pty_server_log_is_cut_to_its_tail_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SERVER_LOG_NAME);
        let mut body = Vec::new();
        while body.len() <= MAX_BYTES as usize {
            body.extend_from_slice(b"0123456789abcdef\n");
        }
        body.extend_from_slice(b"last line\n");
        std::fs::write(&path, &body).unwrap();

        cap_in_place(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.len() as u64 <= KEEP_TAIL_BYTES, "kept {} bytes", text.len());
        assert!(text.ends_with("last line\n"));
        assert!(
            text.lines().all(|line| line == "0123456789abcdef" || line == "last line"),
            "the cut lands on a line boundary"
        );

        std::fs::write(&path, b"small\n").unwrap();
        cap_in_place(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"small\n");
    }

    #[test]
    fn the_session_field_is_the_viewer_when_known_else_the_connection() {
        assert_eq!(session_label(Some("viewer-a"), 7), "viewer-a");
        assert_eq!(session_label(None, 7), "conn-7");
    }

    #[test]
    fn record_appends_each_event_with_its_session_and_reason() {
        let dir = tempfile::tempdir().unwrap();
        record(
            dir.path(),
            &session_label(Some("viewer-a"), 1),
            Event::Disconnect,
            Some("socket closed\nnext"),
        );
        record(dir.path(), &session_label(None, 2), Event::Reconnect, None);
        record(dir.path(), "conn-3", Event::AcceptRefused, Some("peer not admitted"));
        record(dir.path(), "conn-4", Event::WriteFailed, Some("broken pipe"));

        let text = std::fs::read_to_string(log_path(dir.path())).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "one line per record: {text:?}");
        let suffixes = [
            " session=viewer-a event=disconnect reason=socket closed next",
            " session=conn-2 event=reconnect reason=unknown",
            " session=conn-3 event=accept_refused reason=peer not admitted",
            " session=conn-4 event=write_failed reason=broken pipe",
        ];
        for (line, suffix) in lines.iter().zip(suffixes) {
            assert!(line.ends_with(suffix), "{line:?} does not end with {suffix:?}");
        }
    }

    #[test]
    fn concurrent_appends_rotate_without_failing_or_tearing_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = log_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let reason = "x".repeat(4096);
        // About 6.5 MiB in all, so the log rotates several times under contention.
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let (path, reason) = (&path, reason.as_str());
                scope.spawn(move || {
                    for _ in 0..200 {
                        append(path, "s", Event::WriteFailed, Some(reason), "t").expect("append");
                    }
                });
            }
        });

        let expected = format!("t session=s event=write_failed reason={reason}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.is_empty());
        assert!(text.lines().all(|line| line == expected), "every line is one whole record");
    }
}
