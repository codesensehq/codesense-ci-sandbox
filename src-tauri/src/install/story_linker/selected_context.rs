// SPDX-License-Identifier: MIT
//! The selected-context request shapes and the retained-intent codec.
//!
//! SW-XS-8: the selected executor is gone — `hydrate_worktree` /
//! `reverse_sync_worktree` with `kind:selected` remap onto the one
//! preparation engine (`agents::prepare`). What stays: the request shapes the
//! MCP tools still accept, and the types the sync-state store's migrations
//! parse when they meet an intent an older build retained.
use super::*;
use crate::content_engine::destination::{
    DestinationEntry, DestinationReference, ExpectedDestination,
};
use base64::Engine;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod batch;
pub(crate) use batch::validate_retained as validate_batch_intent;
pub(crate) mod progress;
mod startup;
use startup::CapturedSource;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HydrateItem {
    #[serde(rename = "ref")]
    pub reference: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum UpdateOperation {
    #[serde(rename = "update")]
    Update,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum HashKind {
    #[serde(rename = "hash")]
    Hash,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpectedHash {
    pub kind: HashKind,
    pub hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishItem {
    pub op: UpdateOperation,
    pub path: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub expected_source: ExpectedHash,
    pub expected_workspace: ExpectedHash,
}
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreviewItem {
    pub op: UpdateOperation,
    pub path: String,
    #[serde(rename = "ref")]
    pub reference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Applied,
    Unchanged,
    Conflict,
    Denied,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ItemResult {
    pub index: usize,
    #[serde(rename = "ref")]
    pub reference: String,
    pub path: String,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationResult {
    pub request_id: Uuid,
    pub status: String,
    pub items: Vec<ItemResult>,
    pub files_read: u64,
    pub files_written: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum EffectState {
    Prepared,
    Started,
    Applied,
    Terminal,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Snapshot {
    entry: DestinationEntry,
    /// Source ownership is resolved once; replay verifies containment but never
    /// substitutes newer content or a different authoring home.
    source_root: PathBuf,
    #[serde(default)]
    captured_source: Option<CapturedSource>,
    #[serde(default)]
    dependencies: Vec<String>,
    bytes: String,
    expected: ExpectedDestination,
    source_expected: Option<[u8; 32]>,
    mode: u32,
    state: EffectState,
    result: ItemResult,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    generation: u64,
    publish: bool,
    snapshots: Vec<Snapshot>,
    #[serde(default)]
    transfer: Option<batch::TransferDescriptor>,
    result: OperationResult,
}

fn error(message: impl std::fmt::Display) -> AppError {
    install_err("selected context", message)
}
