// SPDX-License-Identifier: MIT
//! Pinned committed Git data from an authoritative managed workspace; no arbitrary downloads.
use crate::{remote_files::FileTarget, remote_workspace::GitBundle};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportIntent {
    pub target: FileTarget,
    pub request_id: String,
    pub expected_head: String,
    /// A commit verified present in the desktop's private repository cache.
    pub prerequisite: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportDescriptor {
    pub request_id: String,
    pub bundle: GitBundle,
    pub released: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase", deny_unknown_fields)]
pub enum ExportOperation {
    Prepare,
    Read { offset: u64 },
    Release { bundle: GitBundle },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportRequest {
    pub intent: ExportIntent,
    pub operation: ExportOperation,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "camelCase", deny_unknown_fields)]
pub enum ExportReply {
    Prepared {
        descriptor: ExportDescriptor,
    },
    Chunk {
        offset: u64,
        #[serde(with = "crate::remote_workspace::chunk_encoding")]
        bytes: Vec<u8>,
    },
    Released {
        descriptor: ExportDescriptor,
    },
}

/// Where a managed workspace's checkout stands right now: the commit `HEAD`
/// names, and the branch it is on (`None` on a detached `HEAD`). Read before a
/// pinned export, whose intent names the exact commit it carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceHead {
    pub commit: String,
    pub branch: Option<String>,
}
