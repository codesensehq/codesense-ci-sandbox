// SPDX-License-Identifier: MIT
// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if codesense_pty_core::activity::run_helper() {
        return;
    }
    codesense_pty_core::nofile::raise(10_240);
    codesense_pty_core::nofile::serve_child_limit();
    // AC2's monitor runs here and in the session server only. The bridge is
    // deliberately not watched: its stderr is the MCP client's error stream,
    // which a warning would pollute, and it holds only its stdio and one
    // socket, so it cannot plausibly approach its limit.
    codesense_pty_core::nofile::watch("codesense", |line| {
        log::warn!(target: "nofile", "{line}")
    });
    codesense_lib::run()
}
