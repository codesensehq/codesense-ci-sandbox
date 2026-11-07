// SPDX-License-Identifier: MIT
//! Deliberate memory-safety faults that let the AddressSanitizer gate prove it
//! can turn red (falsifiability self-check).
//!
//! This module is compiled only under the `sanitizer-canary` cargo feature, and
//! every fault lives inside a `#[test]` function — so it exists in *no* normal or
//! shipped build, and in no non-test build even when the feature is enabled. The
//! gate script (`scripts/run-rust-sanitizer-gate.sh --calibrate`) turns the
//! feature on, runs only these tests under ASan, and asserts the run exits
//! nonzero with a symbolized report. A green calibration would mean the detector
//! is not actually wired up, so the gate treats it as a failure.

/// Deliberate heap use-after-free: free a boxed allocation, then read through the
/// dangling pointer. Under AddressSanitizer this aborts with a symbolized
/// `heap-use-after-free` report; without instrumentation it is undefined
/// behaviour and must never run outside the calibration path.
#[test]
fn asan_canary_heap_use_after_free() {
    let dangling: *const u8 = {
        let boxed = Box::new(0xA5_u8);
        Box::into_raw(boxed) as *const u8
    };

    // SAFETY: intentionally unsound. Free the allocation, then read the freed
    // memory so AddressSanitizer reports a heap-use-after-free. `read_volatile`
    // plus `black_box` stop the optimizer from eliding the access.
    unsafe {
        drop(Box::from_raw(dangling as *mut u8));
        let observed = std::ptr::read_volatile(dangling);
        std::hint::black_box(observed);
    }
}
