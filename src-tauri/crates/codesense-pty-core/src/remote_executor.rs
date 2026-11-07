// SPDX-License-Identifier: MIT
//! A dedicated admitted executor. These bytes are NOT the MCP protocol.
use crate::remote_files::FileTarget;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartExecutor {
    pub task_id: String,
    pub target: FileTarget,
    pub channel: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutorStarted {
    pub task_id: String,
    pub channel: u32,
    pub cwd: String,
    pub auth_token: String,
}
