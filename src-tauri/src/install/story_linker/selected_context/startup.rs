// SPDX-License-Identifier: MIT
//! The provenance an older build's selected startup kept with each applied
//! file (SW-XS-8: the selected startup receiver is gone; Step 2 runs on
//! `agents::prepare`). Kept so a retained intent still parses.
use super::*;
use crate::agents::manifest::{AgentRevisionRef, ContentLayer};

/// Execution provenance retained with the applied selected baseline. This is
/// not authored Agent configuration: its layer uses the producer's actual type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct CapturedSource {
    layer: ContentLayer,
    root: PathBuf,
    source_locator: PathBuf,
    logical: String,
    destination: String,
    agent: AgentRevisionRef,
    manifest_hash: String,
    mode: u32,
}
