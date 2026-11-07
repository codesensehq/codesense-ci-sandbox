// SPDX-License-Identifier: MIT
use rmcp::schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `command_run` input.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CommandRunParams {
    /// A command id from `command_list` (`<dashboard>/<command>`).
    pub id: String,
    /// Passed to the page's `onCommand` callback as `args`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}
