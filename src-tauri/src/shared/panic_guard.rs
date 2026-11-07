// SPDX-License-Identifier: MIT
//! Keeping a long-lived background thread alive across a panicking pass.

/// Run one scheduler pass inside `catch_unwind`, logging (never propagating) a panic so a
/// poisoned lock in a transitive callee can't kill the thread that runs it. The wake is wrapped
/// in `AssertUnwindSafe` (the `AppHandle` it captures is not `RefUnwindSafe`) — sound because a
/// recovered panic only logs and the next wake reads fresh state, never observing a torn value.
pub fn guarded_wake(label: &str, wake: impl FnOnce()) {
    if let Err(panic) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(wake)) {
        let msg = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        log::warn!("{label}: a wake panicked and was recovered: {msg}");
    }
}
