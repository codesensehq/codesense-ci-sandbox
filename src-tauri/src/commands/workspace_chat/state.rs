// SPDX-License-Identifier: MIT
//! Positions persisted beside a workspace's shared log. Human viewing has a separate
//! field, so it cannot acknowledge any session's unread history.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatPosition {
    pub read: i64,
    pub told: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ChatPositions {
    #[serde(rename = "sessions")]
    pub cursors: HashMap<String, SessionChatPosition>,
    pub human: i64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatUnread {
    pub count: u32,
    pub through_id: i64,
    pub read_id: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionChatPosition {
    #[serde(default)]
    pub session_id: String,
    #[serde(flatten)]
    pub position: ChatPosition,
}
