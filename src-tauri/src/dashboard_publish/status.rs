// SPDX-License-Identifier: MIT
use super::{
    approval::LiveResult, client::opt_unix_seconds, state::LocalState, verbs::VerbOutcome,
};
use serde::{Deserialize, Serialize};

/// One active viewer of a dashboard, as R8 and R9 list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Share {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub sessions: Option<u64>,
    #[serde(default)]
    pub last_seen_at: Option<i64>,
    pub email: String,
    #[serde(default)]
    pub invite: Option<String>,
    #[serde(default)]
    pub sent_at: Option<i64>,
    #[serde(default)]
    pub added_at: Option<i64>,
    /// Opened this dashboard through their CodeSense account at least once.
    #[serde(default)]
    pub joined: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardView {
    #[serde(default)]
    pub session_days: Option<u16>,
    pub slug: String,
    pub title: String,
    pub state: String,
    pub visibility: String,
    pub current_version: Option<u64>,
    #[serde(default, deserialize_with = "opt_unix_seconds")]
    pub pushed_at: Option<String>,
    #[serde(default, deserialize_with = "opt_unix_seconds")]
    pub checked_at: Option<String>,
    #[serde(default)]
    pub share_count: usize,
    #[serde(default)]
    pub shares: Vec<Share>,
    #[serde(default)]
    pub live_queries: Option<serde_json::Value>,
    #[serde(default)]
    pub live_streams: Option<serde_json::Value>,
    #[serde(default)]
    pub live_control: Option<String>,
    #[serde(default)]
    pub live_actions: Option<serde_json::Value>,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationStatus {
    pub slug: String,
    pub manifest_state: Option<String>,
    pub local_state: Option<LocalState>,
    pub visibility: Option<String>,
    pub hosted_copy: Option<String>,
    pub hosted_copy_error: Option<String>,
    pub hosted_files_deleted: Option<super::client::DeletedFiles>,
    pub live: Option<LiveResult>,
    pub notes: Vec<String>,
    pub retry: bool,
    pub viewers: Vec<String>,
    pub shares: Vec<Share>,
    pub not_on_list: Vec<String>,
}
impl From<VerbOutcome> for PublicationStatus {
    fn from(outcome: VerbOutcome) -> Self {
        Self {
            slug: outcome.slug,
            manifest_state: Some(outcome.manifest_state.into()),
            local_state: Some(outcome.local_state),
            visibility: outcome.visibility.map(str::to_owned),
            ..Self::default()
        }
    }
}
impl PublicationStatus {
    pub fn sharing(slug: String, shared: super::share::Shared) -> Self {
        Self {
            slug,
            viewers: shared.viewers,
            shares: shared.shares,
            not_on_list: shared.not_on_list,
            ..Self::default()
        }
    }

    pub fn published(&mut self, result: Result<super::push::Hosted, super::push::DriveError>) {
        use super::push::Hosted;
        let (copy, error) = match result {
            Ok(Hosted::Live) => ("live", None),
            Ok(Hosted::AuthFailed) => (
                "auth_failed",
                Some("The publisher key is missing or was refused. Fix key.".into()),
            ),
            Ok(Hosted::Retrying(e)) => (
                "retrying",
                serde_json::to_value(e)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned)),
            ),
            Err(e) => ("not_sent", Some(crate::feedback::redact(&e.reason()))),
        };
        self.hosted_copy = Some(copy.into());
        self.hosted_copy_error = error;
    }
    pub fn unpublished(
        &mut self,
        result: Result<Option<super::client::DeletedFiles>, super::push::DriveError>,
    ) {
        let (copy, error) = match result {
            Ok(deleted) => {
                self.hosted_files_deleted = deleted;
                ("removed", None)
            }
            Err(e) => ("still_live", Some(crate::feedback::redact(&e.reason()))),
        };
        self.hosted_copy = Some(copy.into());
        self.hosted_copy_error = error;
    }
}

#[derive(Debug, Serialize)]
pub struct Capability {
    pub label: String,
    pub available: bool,
    pub detail: String,
}
impl DashboardView {
    pub fn capabilities(&self) -> Vec<Capability> {
        let published = self.state == "published";
        let mut capabilities = vec![Capability {
            label: "View the dashboard".into(),
            available: published,
            detail: if published {
                "Available"
            } else {
                "This dashboard isn't published"
            }
            .into(),
        }];
        if self.live_queries.is_some()
            || self
                .live_streams
                .as_ref()
                .is_some_and(|s| s.as_array().is_some_and(|a| !a.is_empty()))
        {
            capabilities.push(Capability {
                label: "See live data".into(),
                available: published,
                detail: if published {
                    "Available while this Mac is online"
                } else {
                    "This dashboard isn't published"
                }
                .into(),
            });
        }
        if let Some(actions) = self.live_actions.as_ref().and_then(|a| a.as_object()) {
            for (name, action) in actions {
                if action["who"] == "viewers" {
                    let available = published
                        && super::actions::authorize(&serde_json::json!({"role":"viewer"}), None)
                            .is_ok();
                    capabilities.push(Capability { label: name.clone(), available, detail: if available { "Available" } else { "Declared for viewers, not available yet. CodeSense only lets the owner run actions today." }.into() });
                }
            }
        }
        capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn unpublished_dashboards_offer_no_available_viewer_capabilities() {
        for state in ["draft", "unpublished"] {
            use super::super::client::{
                fake::{Fake, Reply},
                Client,
            };
            let server = Fake::start(move |_| Reply::Json(200, serde_json::json!({"slug":"metrics","title":"Metrics","visibility":"private","state":state,"live_queries":[],"live_streams":["terminal"],"live_control":"owner","live_actions":{"Restart build":{"who":"viewers","verb":"runRoutine"}}}).to_string())).await;
            let dashboard = Client::new(&server.base)
                .unwrap()
                .view(
                    &crate::commands::credential_store::Secret::new("csp_test_token"),
                    "metrics",
                )
                .await
                .unwrap();
            let capabilities = dashboard.capabilities();
            assert_eq!(capabilities.len(), 3);
            assert!(capabilities.iter().all(|cap| !cap.available));
        }
    }
    #[test]
    fn declared_viewer_actions_follow_the_mac_authorizer() {
        let dashboard: DashboardView = serde_json::from_value(serde_json::json!({"slug":"metrics","title":"Metrics","visibility":"private","state":"published","current_version":3,"live_actions":{"Restart build":{"who":"viewers","verb":"runRoutine"}},"shares":[{"email":"viewer@example.com","invite":"drafted"}]})).unwrap();
        assert!(!dashboard.capabilities().last().unwrap().available);
        assert_eq!(dashboard.shares[0].invite.as_deref(), Some("drafted"));
        let mut status = PublicationStatus::default();
        status.unpublished(Err(super::super::push::DriveError::Refused));
        assert_eq!(status.hosted_copy.as_deref(), Some("still_live"));
    }
}

#[cfg(test)]
mod invite_tests {
    use super::Share;

    #[test]
    fn share_invite_states_and_added_time() {
        for invite in ["drafted", "sent", "waiting_limit", "suppressed", "none", "unknown"] {
            let share: Share = serde_json::from_value(serde_json::json!({
                "email": "viewer@example.com", "invite": invite, "added_at": 1760000000
            })).unwrap();
            assert_eq!(share.invite.as_deref(), Some(invite));
            assert_eq!(share.added_at, Some(1760000000));
            assert_eq!(share.joined, None);
        }
    }

    #[test]
    fn share_joined_round_trips() {
        for joined in [true, false] {
            let share: Share = serde_json::from_value(serde_json::json!({
                "email": "viewer@example.com", "invite": "sent", "joined": joined
            }))
            .unwrap();
            assert_eq!(share.joined, Some(joined));
            assert_eq!(serde_json::to_value(&share).unwrap()["joined"], joined);
        }
    }
}
