// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DashboardScope {
    BuiltIn,
    Team,
    Private,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardRef {
    pub scope: DashboardScope,
    pub org: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_from: Option<Box<DashboardRef>>,
}
impl DashboardRef {
    pub fn key(&self) -> String {
        let scope = match self.scope {
            DashboardScope::BuiltIn => "built-in",
            DashboardScope::Team => "team",
            DashboardScope::Private => "private",
        };
        let team = if self.scope == DashboardScope::Team {
            format!(":{}", self.team.as_deref().unwrap_or_default())
        } else {
            String::new()
        };
        format!("dashboard:{scope}:{}{team}/{}", self.org, self.id)
    }
}
