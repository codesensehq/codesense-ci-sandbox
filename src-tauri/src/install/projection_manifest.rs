// SPDX-License-Identifier: MIT
//! The legacy per-team projection manifest (`projections.yml`).
//!
//! SW-XS-8: nothing delivers from it any more — the projection engine that
//! lowered it into projections is gone, and an agent's `context` decides what
//! a workspace gets (SPEC §12). It is still READ, once, to turn its entries
//! into a suggestion on each team agent (`agents::context_suggestion`).

use std::path::{Path, PathBuf};

use log::{debug, warn};
use serde::Deserialize;

use crate::context_repo::team_config_dir_beside_content;

/// The manifest's file name in a team's `config/` dir.
pub const PROJECTIONS_MANIFEST_FILENAME: &str = "projections.yml";

/// The manifest's on-disk home: the team's `config/` dir (the sibling of
/// `content/`).
fn manifest_path(content_dir: &Path) -> PathBuf {
    team_config_dir_beside_content(content_dir).join(PROJECTIONS_MANIFEST_FILENAME)
}

// ---------------------------------------------------------------------------
// On-disk schema
// ---------------------------------------------------------------------------

/// One `projections.yml` entry.
#[derive(Debug, Clone, Deserialize)]
pub struct RawProjectionEntry {
    /// Stable per-projection key (`MaterializedEntry::projection_key`).
    pub key: String,
    /// Subtree under `content/` to project. Omitted/`""` = whole tree.
    #[serde(default)]
    pub source_subdir: String,
    /// Destination prefix in the source repo. Omitted → derived from
    /// `source_subdir`'s last component (AC-2).
    #[serde(default)]
    pub dest: Option<String>,
}

/// Top-level `projections.yml` document.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawProjectionsYml {
    #[serde(default)]
    pub projections: Vec<RawProjectionEntry>,
}

// ---------------------------------------------------------------------------
// Load
// ---------------------------------------------------------------------------

/// Read + parse the team manifest (`{config_dir}/projections.yml`, resolved from
/// `content_dir` via [`manifest_path`]). A missing file is the
/// zero-config case → empty spec list (AC-5; whole-root supplies the defaults).
/// A malformed file is logged and treated as empty rather than failing the whole
/// sync — a broken manifest must never block whole-root projection.
pub fn load_specs(content_dir: &Path) -> Vec<RawProjectionEntry> {
    let path = manifest_path(content_dir);
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!(
                target: "projection_manifest",
                "no manifest at {} — zero-config (story projection only)",
                path.display()
            );
            return Vec::new();
        }
        Err(e) => {
            warn!(
                target: "projection_manifest",
                "cannot read {}: {} — treating as zero-config",
                path.display(), e
            );
            return Vec::new();
        }
    };
    match serde_yaml::from_str::<RawProjectionsYml>(&raw) {
        Ok(doc) => doc.projections,
        Err(e) => {
            warn!(
                target: "projection_manifest",
                "malformed {}: {} — ignoring manifest, story projection still applies",
                path.display(), e
            );
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_parse_from_the_config_dir_and_a_missing_or_broken_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let content = dir.path().join("team/content");
        std::fs::create_dir_all(&content).unwrap();
        assert!(load_specs(&content).is_empty());
        let config = team_config_dir_beside_content(&content);
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(
            config.join(PROJECTIONS_MANIFEST_FILENAME),
            "projections:\n  - key: docs\n    source_subdir: docs\n    dest: notes\n",
        )
        .unwrap();
        let specs = load_specs(&content);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].key, "docs");
        assert_eq!(specs[0].dest.as_deref(), Some("notes"));
        std::fs::write(config.join(PROJECTIONS_MANIFEST_FILENAME), "projections: [").unwrap();
        assert!(load_specs(&content).is_empty());
    }
}
