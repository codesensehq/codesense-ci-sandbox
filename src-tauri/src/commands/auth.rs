// SPDX-License-Identifier: MIT
//! Read-only historical identity metadata. Service authentication has been retired.
//! Credentials are neither refreshed nor mutated by this compatibility reader.
use serde::Deserialize;
use std::collections::HashMap;
pub mod private_authority;

#[derive(Debug, Clone, Deserialize)]
pub struct UserInfo {
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub user_id: Option<u64>,
}
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProviderState {
    #[serde(default)]
    pub user_info: Option<UserInfo>,
}
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProviderConnections {
    #[serde(default)]
    pub active_account: Option<String>,
    #[serde(default)]
    pub accounts: HashMap<String, ProviderState>,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(from = "AuthFileWire")]
pub struct AuthFile {
    pub connections: HashMap<String, ProviderConnections>,
}
#[derive(Deserialize)]
struct AuthFileWire {
    #[serde(default)]
    connections: HashMap<String, serde_json::Value>,
    #[serde(default)]
    github: Option<serde_json::Value>,
}
impl From<AuthFileWire> for AuthFile {
    fn from(wire: AuthFileWire) -> Self {
        let mut connections = HashMap::new();
        let value = wire.connections.get("github").cloned().or(wire.github);
        if let Some(value) = value {
            let active_account = value
                .get("active_account")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or_else(|| {
                    value
                        .get("active_account_id")
                        .and_then(|v| v.as_u64())
                        .map(|id| id.to_string())
                });
            let accounts = value
                .get("accounts")
                .and_then(|v| v.as_object())
                .into_iter()
                .flatten()
                .filter_map(|(key, v)| {
                    serde_json::from_value(v.clone())
                        .ok()
                        .map(|account| (key.clone(), account))
                })
                .collect();
            connections.insert(
                "github".into(),
                ProviderConnections {
                    active_account,
                    accounts,
                },
            );
        }
        Self { connections }
    }
}
