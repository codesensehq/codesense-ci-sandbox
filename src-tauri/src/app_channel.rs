// SPDX-License-Identifier: MIT
//! SW-PB-21: the build channel and its public host. The frontend reads them at
//! startup to decide whether the activation gate and the "Private beta" labels
//! apply. They are injected with the invoke initialization script, so they exist
//! before any app code runs, and the internal build says `internal`.

#[cfg(cs_channel = "internal")]
const CHANNEL: &str = "internal";
#[cfg(cs_channel = "beta")]
const CHANNEL: &str = "beta";
#[cfg(cs_channel = "beta-staging")]
const CHANNEL: &str = "beta-staging";

/// The script every webview runs before the page: sets `window.__CODESENSE_CHANNEL__`
/// and `window.__CODESENSE_PUBLIC_HOST__`.
pub fn init_script() -> String {
    let channel = serde_json::to_string(CHANNEL).expect("channel serializes");
    let host = serde_json::to_string(env!("CODESENSE_PUBLIC_HOST")).expect("host serializes");
    let publishing = cfg!(feature = "dashboard-publish");
    format!("window.__CODESENSE_CHANNEL__={channel};window.__CODESENSE_PUBLIC_HOST__={host};window.__CODESENSE_DASHBOARD_PUBLISH__={publishing};")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_init_script_names_the_build_channel_and_host() {
        let script = init_script();
        assert!(script.contains(&format!("window.__CODESENSE_CHANNEL__=\"{CHANNEL}\"")));
        assert!(script.contains("window.__CODESENSE_PUBLIC_HOST__=\""));
        assert!(script.contains(&format!(
            "window.__CODESENSE_DASHBOARD_PUBLISH__={}",
            cfg!(feature = "dashboard-publish")
        )));
        assert!(matches!(CHANNEL, "internal" | "beta" | "beta-staging"));
    }
}
