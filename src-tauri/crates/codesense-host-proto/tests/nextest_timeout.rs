// SPDX-License-Identifier: MIT

/// Opt-in proof that the CI profile terminates a hung test and names it in JUnit.
/// Enable the planted-hang feature, set CODESENSE_TEST_PLANTED_HANG=1,
/// and filter nextest to planted_hang_timeout_proof.
#[test]
fn planted_hang_timeout_proof() {
    if std::env::var("CODESENSE_TEST_PLANTED_HANG").as_deref() != Ok("1") {
        return;
    }
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
