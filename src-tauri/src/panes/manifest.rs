// SPDX-License-Identifier: MIT
//! `pane-manifest.yaml` — optional declarative overlay for a `panes/` dir.
//!
//! Pane discovery is file-based (`mod.rs`): any `<id>.html` is a pane, no
//! manifest required. A manifest beside those files, when present, carries
//! metadata the discovery pass cannot infer alone — `showsOn` (which pane
//! contexts the pane wants to appear on), `priority` (stack order within a
//! context), `collapsedByDefault`, and `reactsTo` (purely informational: which
//! `subscribeContext` payload keys the pane watches, so a future primitives
//! explorer can answer "which panes care about this surface?").
//!
//! The scanner applies declarations to discovered HTML siblings. Invalid YAML
//! remains an error so the scanner can retain affected panes as rejected entries.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// The supported manifest schema version.
pub const MANIFEST_VERSION: u32 = 1;

/// The file name a manifest is discovered under, beside the pane HTMLs.
pub const MANIFEST_FILE: &str = "pane-manifest.yaml";

/// One entry in the manifest: an advisory overlay on a discovered pane. The
/// `id` matches the pane's filename stem (`tutor.html` → `tutor`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneManifestEntry {
    #[serde(flatten)]
    pub declaration: super::DashboardDeclaration,
    /// Stable key: matches the pane's filename stem.
    pub id: String,
    /// Pane title (also carried in the HTML's `<title>`; manifest wins when both are set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// HTML file name inside this `panes/` dir, relative to the manifest.
    /// Build-time lint enforces the file exists.
    pub html: String,
    /// Lucide icon name; overrides the HTML declaration when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Pane-context keys where this pane wants to appear. `"*"` = always.
    /// The viewer treats an empty list as "nowhere by default" (the pane is
    /// still placeable manually via View → Panels).
    #[serde(default)]
    pub shows_on: Vec<String>,
    /// Stack ordering within a `showsOn` bucket; lower = nearer the top.
    #[serde(default)]
    pub priority: Option<u32>,
    /// Start folded; the user expands when they want it.
    #[serde(default)]
    pub collapsed_by_default: bool,
    /// Informational: `subscribeContext` payload keys the pane watches.
    #[serde(default)]
    pub reacts_to: Vec<String>,
}

/// The parsed `pane-manifest.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PaneManifest {
    pub version: u32,
    pub panes: Vec<PaneManifestEntry>,
}

/// Load a layer's manifest. Absence returns `None`; malformed YAML remains an
/// error for discovery to surface on each affected pane.
pub fn load(panes_dir: &Path) -> Option<Result<PaneManifest, String>> {
    let path = panes_dir.join(MANIFEST_FILE);
    let bytes = std::fs::read(&path).ok()?;
    Some(parse(&bytes))
}

/// Parse `bytes` as a `pane-manifest.yaml` document.
pub fn parse(bytes: &[u8]) -> Result<PaneManifest, String> {
    let manifest: PaneManifest = serde_yaml::from_slice(bytes).map_err(|e| e.to_string())?;
    if manifest.version != MANIFEST_VERSION {
        return Err(format!("unsupported manifest version {}", manifest.version));
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_shipped_shape() {
        let yaml = r#"
version: 1
panes:
  - id: tutor
    title: "Tutor"
    html: tutor.html
    icon: graduation-cap
    showsOn: ["*"]
    priority: 100
    collapsedByDefault: true
    reactsTo: [surface, workspace, team, selection]
  - id: explain-this-surface
    title: "Explain this surface"
    html: explain-this-surface.html
    icon: scan-text
    showsOn:
      - viewer:markdown
      - viewer:html
    priority: 10
    collapsedByDefault: false
"#;
        let manifest = parse(yaml.as_bytes()).unwrap();
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.panes.len(), 2);
        let tutor = &manifest.panes[0];
        assert_eq!(tutor.id, "tutor");
        assert_eq!(tutor.title.as_deref(), Some("Tutor"));
        assert_eq!(tutor.html, "tutor.html");
        assert_eq!(tutor.icon.as_deref(), Some("graduation-cap"));
        assert_eq!(tutor.shows_on, vec!["*".to_string()]);
        assert_eq!(tutor.priority, Some(100));
        assert!(tutor.collapsed_by_default);
        assert_eq!(tutor.reacts_to.len(), 4);
        let explain = &manifest.panes[1];
        assert_eq!(explain.shows_on, vec!["viewer:markdown", "viewer:html"]);
        assert!(!explain.collapsed_by_default);
    }

    #[test]
    fn missing_optional_fields_default_sanely() {
        let yaml = r#"
version: 1
panes:
  - id: minimal
    html: minimal.html
"#;
        let manifest = parse(yaml.as_bytes()).unwrap();
        let entry = &manifest.panes[0];
        assert!(entry.title.is_none());
        assert!(entry.icon.is_none());
        assert!(entry.shows_on.is_empty());
        assert!(entry.priority.is_none());
        assert!(!entry.collapsed_by_default);
        assert!(entry.reacts_to.is_empty());
    }

    #[test]
    fn load_from_disk_returns_none_without_a_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).is_none());
    }

    #[test]
    fn load_from_disk_parses_when_a_file_is_there() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(MANIFEST_FILE),
            "version: 1\npanes:\n  - id: a\n    html: a.html\n",
        )
        .unwrap();
        let manifest = load(dir.path()).unwrap().unwrap();
        assert_eq!(manifest.panes.len(), 1);
        assert_eq!(manifest.panes[0].id, "a");
    }

    #[test]
    fn load_from_disk_reports_parse_errors_for_a_broken_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MANIFEST_FILE), "not: valid: yaml: shape").unwrap();
        let result = load(dir.path()).unwrap();
        assert!(result.is_err());
    }
}
