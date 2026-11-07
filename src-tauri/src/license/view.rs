// SPDX-License-Identifier: MIT
//! The payload of `license_state` and of the `license://state` event: the channel and the update
//! fields. There is no license state any more (SV-PBF-278), so nothing here gates the app.

use serde::Serialize;

use super::updates;

pub const EVENT: &str = "license://state";

#[cfg(cs_channel = "beta")]
pub const CHANNEL: &str = "beta";
#[cfg(cs_channel = "beta-staging")]
pub const CHANNEL: &str = "beta-staging";

/// Carried on every state payload: the build the beta updater has staged for the next quit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UpdateStatus {
    pub staged_version: String,
    /// The release notes from the update manifest that offered it.
    pub notes: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct View {
    pub channel: &'static str,
    pub update: Option<UpdateStatus>,
    /// Set when the next build has to be installed by hand (SW-PB-13).
    pub update_attention: Option<updates::Attention>,
    /// This build gets no updates: it was built without its channel's updater config.
    pub updates_off: bool,
    /// SW-PB-23: the staged build can be installed now and the app relaunched (Restart to
    /// update). The click checks the gate again.
    pub restart_ready: bool,
}
