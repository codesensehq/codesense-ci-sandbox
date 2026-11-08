// SPDX-License-Identifier: MIT
//! One process-wide logger for tests, with captures isolated by thread and target.
//! Captures belong to the emitting OS thread. Synchronous tests may clear/read their
//! own target; async tests must stay on one thread. Spawned-task or worker-thread
//! logs are not included in a caller's capture, and old threads' lines are retained.
use std::sync::{Mutex, OnceLock};
use std::thread::ThreadId;

#[derive(Default)]
struct Capture(Mutex<Vec<(ThreadId, String, String)>>);

impl log::Log for Capture {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }
    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.0.lock().unwrap_or_else(|e| e.into_inner()).push((
                std::thread::current().id(),
                record.target().to_owned(),
                record.args().to_string(),
            ));
        }
    }
    fn flush(&self) {}
}

fn logger() -> &'static Capture {
    static LOGGER: OnceLock<Capture> = OnceLock::new();
    static INSTALLED: OnceLock<()> = OnceLock::new();
    let logger = LOGGER.get_or_init(Capture::default);
    INSTALLED.get_or_init(|| {
        log::set_logger(logger).expect("tests must share the test logger");
        log::set_max_level(log::LevelFilter::Warn);
    });
    logger
}

pub(crate) fn clear(target: &str) {
    let thread = std::thread::current().id();
    logger()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(tid, name, _)| *tid != thread || name != target);
}

pub(crate) fn lines(target: &str) -> Vec<String> {
    let thread = std::thread::current().id();
    logger()
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(tid, name, _)| *tid == thread && name == target)
        .map(|(_, _, line)| line.clone())
        .collect()
}
