// SPDX-License-Identifier: MIT
//! Dashboard publishing, app side (SW-PB-22a, 22e): the manifest, the payload
//! builder and its privacy filter, the local publish record, and the push
//! watcher with its HTTP client (22e). The network calls are in `client.rs` and
//! `push.rs` only; the 22a modules make none.
//!
//! Compiled only with the `dashboard-publish` Cargo feature (DP-D5, DP-D21).
//! Beta release builds enable it; ordinary development builds may leave it off.

pub mod actions;
pub mod agents;
pub mod approval;
pub mod catalog;
pub mod client;
pub mod confirm;
pub mod control;
pub mod create;
pub mod driver;
pub mod inbox;
pub mod keys;
pub mod inventory;
pub mod live;
pub mod manifest;
pub mod management;
pub mod payload;
pub mod push;
pub mod share;
pub mod state;
pub mod status;
pub mod verbs;
pub mod watch;
/// SW-PB-22f: the staging walk, two ignored tests run by hand against staging.
mod staging_walk;

#[cfg(test)]
mod no_network_tests {
    /// AC-9: the module has no network surface. The module's sources must not
    /// name an HTTP client or socket type. This check is a stand-in for the
    /// "HTTP client that panics on any call" mock, because the module has no
    /// client to mock.
    #[test]
    fn module_sources_name_no_http_client_or_socket() {
        let sources = [
            include_str!("create.rs"),
            include_str!("manifest.rs"),
            include_str!("payload.rs"),
            include_str!("state.rs"),
            include_str!("verbs.rs"),
        ];
        for needle in [
            "reqwest",
            "ureq",
            "hyper",
            "TcpStream",
            "std::net",
            "UdpSocket",
        ] {
            for src in sources {
                assert!(!src.contains(needle), "publish module names {needle}");
            }
        }
    }
}
