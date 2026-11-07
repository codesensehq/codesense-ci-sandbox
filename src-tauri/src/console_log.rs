// SPDX-License-Identifier: MIT
//! Console (stdout/stderr) output that can never take the process down.
//!
//! The app's stdout/stderr are frequently pipes owned by another process (a
//! dev terminal, a test harness). When that reader goes away, every write
//! fails with `EPIPE`. Rust's `eprintln!` panics on a failed write, and fern
//! panics when both the target write and its stderr fallback fail — inside the
//! panic hook that second panic aborts the process. Console output is purely
//! diagnostic (the rotated log file is the durable record), so a failed console
//! write is dropped rather than allowed to crash the app.

use std::io::{self, Write};
use tauri_plugin_log::fern;

/// A writer that forwards to `inner` and reports success even when the
/// underlying write fails, so callers (fern, the panic hook) never see a
/// console error they would escalate into a panic.
struct LossyConsole<W> {
    inner: W,
}

impl<W: Write> Write for LossyConsole<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = self.inner.write_all(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let _ = self.inner.flush();
        Ok(())
    }
}

/// The stdout log target: formatted records go to stdout, and a closed or
/// broken stdout silently drops them.
pub(crate) fn stdout_dispatch() -> fern::Dispatch {
    fern::Dispatch::new().chain(fern::Output::writer(
        Box::new(LossyConsole {
            inner: io::stdout(),
        }),
        "\n",
    ))
}

/// Write one line to stderr, ignoring failure. Use instead of `eprintln!` on
/// any path that must not panic (the panic hook, early startup).
pub(crate) fn write_stderr_line(text: &str) {
    let _ = writeln!(io::stderr().lock(), "{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn lossy_console_reports_success_when_the_reader_is_gone() {
        let mut console = LossyConsole {
            inner: BrokenPipeWriter,
        };
        assert_eq!(console.write(b"line\n").unwrap(), 5);
        console.flush().unwrap();
    }

    #[test]
    fn lossy_console_forwards_bytes_when_the_reader_is_healthy() {
        let mut console = LossyConsole { inner: Vec::new() };
        console.write_all(b"hello").unwrap();
        assert_eq!(console.inner, b"hello");
    }
}

/// Process-level regressions: each parent test re-runs this test binary with
/// exactly one child test selected. The child points its own fd 1 and fd 2 at
/// a pipe with no reader (the state an orphaned harness app is in) and then
/// logs or panics. An abort shows up as the child dying on a signal.
#[cfg(all(test, unix))]
mod closed_console_process_tests {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Output};

    const CHILD_ENV: &str = "CODESENSE_CLOSED_CONSOLE_CHILD";

    fn in_child() -> bool {
        std::env::var_os(CHILD_ENV).is_some()
    }

    fn run_child(test_name: &str) -> Output {
        let exe = std::env::current_exe().expect("test binary path");
        Command::new(exe)
            .args([test_name, "--exact", "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, "1")
            .output()
            .expect("spawn child test")
    }

    fn assert_child_passed(output: &Output) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.signal(),
            None,
            "child was killed by a signal (abort)\nstdout: {stdout}\nstderr: {stderr}"
        );
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "child test failed\nstdout: {stdout}\nstderr: {stderr}"
        );
    }

    /// Points fd 1 and fd 2 at a pipe whose read end is closed, restoring the
    /// originals on drop so the test harness can report the result.
    struct ClosedConsole {
        saved_stdout: libc::c_int,
        saved_stderr: libc::c_int,
    }

    impl ClosedConsole {
        fn install() -> Self {
            let mut fds = [0 as libc::c_int; 2];
            // SAFETY: `fds` is a valid two-element buffer for pipe(2); every
            // other call operates on descriptors this function just obtained
            // or on the process's standard descriptors 1 and 2, which the
            // test harness keeps open.
            unsafe {
                assert_eq!(libc::pipe(fds.as_mut_ptr()), 0, "pipe");
                let [read_end, write_end] = fds;
                libc::close(read_end);
                let saved_stdout = libc::dup(1);
                let saved_stderr = libc::dup(2);
                assert!(saved_stdout >= 0 && saved_stderr >= 0, "dup");
                assert!(
                    libc::dup2(write_end, 1) == 1 && libc::dup2(write_end, 2) == 2,
                    "dup2"
                );
                libc::close(write_end);
                Self {
                    saved_stdout,
                    saved_stderr,
                }
            }
        }
    }

    impl Drop for ClosedConsole {
        fn drop(&mut self) {
            // SAFETY: both descriptors were returned by dup(2) in `install`
            // and are owned exclusively by this guard.
            unsafe {
                libc::dup2(self.saved_stdout, 1);
                libc::dup2(self.saved_stderr, 2);
                libc::close(self.saved_stdout);
                libc::close(self.saved_stderr);
            }
        }
    }

    #[test]
    fn child_logs_with_closed_console() {
        if !in_child() {
            return;
        }
        super::stdout_dispatch()
            .apply()
            .expect("install logger in child process");
        let _closed = ClosedConsole::install();
        for line in 0..200 {
            log::info!("line {line} after the console reader went away");
        }
    }

    #[test]
    fn child_panics_with_closed_console() {
        if !in_child() {
            return;
        }
        super::stdout_dispatch()
            .apply()
            .expect("install logger in child process");
        crate::install_panic_hook();
        let closed = ClosedConsole::install();
        let outcome = std::panic::catch_unwind(|| panic!("panic with the console reader gone"));
        drop(closed);
        assert!(outcome.is_err(), "the panic unwinds normally");
    }

    #[test]
    fn logging_after_console_reader_exits_keeps_running() {
        if in_child() {
            return;
        }
        assert_child_passed(&run_child(
            "console_log::closed_console_process_tests::child_logs_with_closed_console",
        ));
    }

    #[test]
    fn panic_hook_with_console_reader_gone_does_not_abort() {
        if in_child() {
            return;
        }
        assert_child_passed(&run_child(
            "console_log::closed_console_process_tests::child_panics_with_closed_console",
        ));
    }
}
