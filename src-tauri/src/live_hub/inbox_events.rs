// SPDX-License-Identifier: MIT
use crate::commands::dashboard_launch::LaunchTeam;
use serde::{Deserialize, Serialize};

pub const INBOX_EVENT: &str = "codesense:inbox-domain";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InboxChange {
    Stored,
    Read,
    Archived,
    Withdrawn,
    RoleClaim,
    NoticeConfirmed,
    Reacted,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxEvent {
    pub kind: InboxChange,
    pub team: LaunchTeam,
    pub message_ids: Vec<i64>,
}
impl InboxEvent {
    pub fn new(
        kind: InboxChange,
        team: &LaunchTeam,
        message_ids: impl IntoIterator<Item = i64>,
    ) -> Self {
        Self {
            kind,
            team: team.clone(),
            message_ids: message_ids.into_iter().collect(),
        }
    }
}
