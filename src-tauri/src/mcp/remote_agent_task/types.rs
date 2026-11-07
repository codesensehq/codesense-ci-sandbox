// SPDX-License-Identifier: MIT
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct StartParams {
    pub request_id: String,
    pub workspace_handle: String,
    pub provider: String,
    pub account_id: Option<String>,
    pub account_generation: Option<u64>,
    pub prompt: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReadParams {
    pub task_handle: String,
    pub cursor: Option<String>,
}
#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CancelParams {
    pub task_handle: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Outcome {
    pub task_handle: String,
    pub status: String,
    pub cursor: String,
    pub text: String,
    pub message: Option<String>,
}
