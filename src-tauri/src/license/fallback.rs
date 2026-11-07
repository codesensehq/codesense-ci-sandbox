// SPDX-License-Identifier: MIT
//! The names of the fallback files an older beta build kept beside the Keychain, in the app data
//! directory (SV-PBF-278). Nothing writes them any more: the one-time cleanup removes them
//! (`static_channel.rs`).

/// The legacy shared file, retained unchanged for the other channel.
pub const FALLBACK_FILE: &str = "license-fallback.json";

/// A backup belongs to the same channel as its Keychain service.
pub fn fallback_file(service: &str) -> String {
    format!("license-fallback-{service}.json")
}
