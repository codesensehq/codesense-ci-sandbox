// SPDX-License-Identifier: MIT
use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlLinks {
    pub take: String,
    pub end: String,
    pub expires_hours: u8,
}
fn links_for(slug: &str, host: &str) -> Result<ControlLinks, String> {
    super::management::validate_slug(slug)?;
    Ok(ControlLinks {
        take: format!("https://{host}/account/control/{slug}"),
        end: format!("https://{host}/account"),
        expires_hours: 12,
    })
}
#[tauri::command]
pub fn dashboard_control_links(slug: String) -> Result<ControlLinks, String> {
    links_for(&slug, env!("CODESENSE_PUBLIC_HOST"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn control_stays_in_the_browser_on_the_build_account_host() {
        let links = links_for("weekly-metrics", "staging.example.invalid").unwrap();
        assert_eq!(
            links.take,
            "https://staging.example.invalid/account/control/weekly-metrics"
        );
        assert_eq!(links.end, "https://staging.example.invalid/account");
        assert_eq!(links.expires_hours, 12);
        let wire = serde_json::to_string(&links).unwrap();
        assert!(!wire.contains("csp_"));
        assert!(!wire.contains("active"));
    }
}
