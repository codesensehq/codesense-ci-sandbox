// SPDX-License-Identifier: MIT
//! The `routines:changed` app event: emitted after every admission, every
//! terminal transition and every change to a routine's definition or local
//! state, so any view of routines knows to re-read.
//!
//! The routine core is `AppHandle`-free, so the app installs the emitter once at
//! startup (the `claim_loop::wake` idiom). Before that, and in tests, a change
//! notifies nobody.

use std::sync::OnceLock;

use serde::Serialize;

/// The app event name.
pub const ROUTINES_CHANGED: &str = "routines:changed";

/// The event payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoutineChanged {
    pub routine_key: String,
    /// The run that changed; `None` when the routine itself did.
    pub run_id: Option<String>,
}

type Sink = Box<dyn Fn(&RoutineChanged) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Install the process's emitter. Only the first install takes effect.
pub fn install(sink: impl Fn(&RoutineChanged) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

/// Install the app emitter for `routines:changed`.
pub fn install_app_emitter(app: tauri::AppHandle) {
    use tauri::Emitter;
    install(move |change| {
        if let Err(e) = app.emit(ROUTINES_CHANGED, change) {
            log::debug!(target: "routines", "routines:changed emit failed: {e}");
        }
    });
}

/// Tell any listener that a run of `routine_key` changed.
pub fn changed(routine_key: &str, run_id: &str) {
    emit(routine_key, Some(run_id));
}

/// Tell any listener that `routine_key`'s definition or local state changed.
pub fn routine_changed(routine_key: &str) {
    emit(routine_key, None);
}

fn emit(routine_key: &str, run_id: Option<&str>) {
    if let Some(sink) = SINK.get() {
        sink(&RoutineChanged {
            routine_key: routine_key.to_string(),
            run_id: run_id.map(str::to_string),
        });
    }
}
