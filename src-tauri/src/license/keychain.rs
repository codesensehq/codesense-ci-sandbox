// SPDX-License-Identifier: MIT
//! The Keychain accounts an older beta build kept its license in (SV-PBF-278). Nothing reads or
//! writes them any more: they are named here so the one-time cleanup (`static_channel.rs`) can
//! delete each one.

pub const INSTALL_TOKEN: &str = "install_token";
pub const INSTALL_ID: &str = "install_id";
pub const LAST_JWS: &str = "last_jws";
pub const LAST_SERVER_TIME: &str = "last_server_time";
pub const LOCK_REASON: &str = "lock_reason";
pub const PENDING_REVOKE: &str = "pending_revoke";
pub const DEVICE_TIME: &str = "device_time";

/// An older deactivate that could not remove the install id left this marker.
pub const DEACTIVATE_INCOMPLETE: &str = "deactivate_incomplete";

#[cfg(all(test, cs_channel = "beta"))]
pub const SERVICE: &str = "ai.codesense.beta";
#[cfg(all(test, cs_channel = "beta-staging"))]
pub const SERVICE: &str = "ai.codesense.beta-staging";

/// The six records of the license, plus the device mark. `DEACTIVATE_INCOMPLETE` is the eighth
/// account and is named apart.
pub(super) const ALL_RECORDS: [&str; 7] = [
    INSTALL_TOKEN,
    INSTALL_ID,
    LAST_JWS,
    LAST_SERVER_TIME,
    LOCK_REASON,
    PENDING_REVOKE,
    DEVICE_TIME,
];
