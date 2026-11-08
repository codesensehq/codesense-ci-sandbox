// SPDX-License-Identifier: MIT
//! Built-in dashboards: panes the app ships in its binary, listed last for every
//! team so a slot always has one. A team or private pane of the same id wins.
//!
//! The bytes are written to a machine-local `panes/` dir under the user's home
//! (`users/<id>/builtin-panes/panes/`), outside every context repo and private
//! space, so they are served, framed and declared exactly like any other pane.
//! Nothing writes there but this module: `pane_write` targets only team and
//! private spaces, so editing the built-in means forking it into one of them.

use std::path::{Path, PathBuf};

use super::{list_layer, DashboardRef, DashboardScope, PaneEntry, PaneScope};
use crate::context_repo::{path_to_user_home, PANES_DIR};
use crate::domain::Principal;

/// The folder under a user's home that holds the built-in `panes/` dir.
const BUILTIN_PANES_DIR: &str = "builtin-panes";

/// Every built-in pane, one `(id, html)` per line, in the order of `BUILTIN_PANES` in
/// `scripts/lib/builtin-panes.mjs` (a test keeps their ids and sources equal).
#[rustfmt::skip]
pub(super) const BUILTINS: &[(&str, &str)] = &[
    ("inbox", include_str!("../../../src/features/shell/right-pane/__fixtures__/inbox-pane.html")),
    ("story-trail", include_str!("../../assets/dashboards/story-trail/index.html")),
    ("files-tree", include_str!("../../../src/features/shell/slots/builtin/files-tree.html")),
    ("agent-list", include_str!("../../../src/features/shell/slots/__fixtures__/agent-list-pane.html")),
    ("notes", include_str!("../../../src/features/shell/right-pane/__fixtures__/notes-pane.html")),
];

/// The built-in `panes/` dir for `principal`.
pub(crate) fn builtin_panes_dir(home: &Path, principal: &Principal) -> PathBuf {
    path_to_user_home(home, principal)
        .join(BUILTIN_PANES_DIR)
        .join(PANES_DIR)
}

/// Whether `path` is a pane file in a built-in `panes/` dir.
pub(crate) fn is_builtin_pane_path(path: &Path) -> bool {
    path.parent().is_some_and(|dir| {
        dir.file_name().is_some_and(|n| n == PANES_DIR)
            && dir
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == BUILTIN_PANES_DIR)
    })
}

/// Write each built-in whose bytes on disk differ from the binary's.
fn materialize(dir: &Path) -> Result<(), String> {
    for &(id, html) in BUILTINS {
        let path = dir.join(format!("{id}.html"));
        if std::fs::read(&path).ok().as_deref() != Some(html.as_bytes()) {
            super::write_pane(dir, id, html)?;
        }
    }
    Ok(())
}

/// `entry` re-identified as the built-in of `org`: its ref is `built-in`, and its
/// panel id is the private one, so a private fork later takes the same place.
fn as_builtin(mut entry: PaneEntry, org: &str) -> PaneEntry {
    entry.dashboard_ref = DashboardRef {
        scope: DashboardScope::BuiltIn,
        org: org.into(),
        team: None,
        id: entry.id.clone(),
        installed_from: None,
    };
    entry.scope = PaneScope::Private;
    entry.team = None;
    entry.panel_id = super::pane_panel_id(PaneScope::Private, org, None, &entry.id);
    entry
}

/// Every bundled dashboard for `org`; the caller applies visible-list precedence.
/// A built-in that cannot be written is left out rather than failing the listing.
pub(crate) fn builtin_panes(home: &Path, principal: &Principal, org: &str) -> Vec<PaneEntry> {
    let dir = builtin_panes_dir(home, principal);
    if let Err(error) = materialize(&dir) {
        log::warn!("built-in panes unavailable: {error}");
        return Vec::new();
    }
    list_layer(&dir, PaneScope::Private, org, None)
        .into_iter()
        .filter(|entry| BUILTINS.iter().any(|(id, _)| *id == entry.id))
        .map(|entry| as_builtin(entry, org))
        .collect()
}

/// The built-in pane at `path`, as `org` lists it, when `path` is one.
pub(crate) fn builtin_at(path: &Path, org: &str) -> Option<PaneEntry> {
    if !is_builtin_pane_path(path) {
        return None;
    }
    list_layer(path.parent()?, PaneScope::Private, org, None)
        .into_iter()
        .find(|entry| Path::new(&entry.path) == path)
        .map(|entry| as_builtin(entry, org))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn story_trail_materializes_as_a_tab_only_built_in() {
        let home = tempfile::tempdir().unwrap();
        let principal = Principal::from(42u64);
        let panes = builtin_panes(home.path(), &principal, "acme/api");
        let trail = panes.iter().find(|p| p.id == "story-trail").unwrap();
        assert_eq!(trail.dashboard_ref.scope, DashboardScope::BuiltIn);
        assert!(trail.declaration.slots.is_empty());
        assert_eq!(
            trail.declaration.capabilities,
            vec!["sessions:read", "accounts:manage", "context:read", "repo:read", "launch"]
        );
        trail.declaration.validate().unwrap();
        assert_eq!(trail.status, super::super::PaneStatus::Ready);
        let path = Path::new(&trail.path);
        assert!(is_builtin_pane_path(path));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            include_str!("../../assets/dashboards/story-trail/index.html")
        );
        assert_eq!(builtin_at(path, "acme/api").unwrap().id, "story-trail");
    }
    #[test]
    fn shipped_files_tree_lists_for_both_file_slots() {
        let home = tempfile::tempdir().unwrap();
        let panes = builtin_panes(home.path(), &Principal::from(42u64), "acme/api");
        let files = panes.iter().find(|p| p.id == "files-tree").unwrap();
        assert_eq!(files.dashboard_ref.scope, DashboardScope::BuiltIn);
        assert_eq!(files.declaration.slots, vec!["file-nav", "right-pane"]);
        files.declaration.validate().unwrap();
        assert_eq!(files.status, super::super::PaneStatus::Ready);
    }
    #[test]
    fn notes_lists_as_a_right_pane_built_in_for_the_notes_surface() {
        let home = tempfile::tempdir().unwrap();
        let panes = builtin_panes(home.path(), &Principal::from(42u64), "acme/api");
        let notes = panes.iter().find(|p| p.id == "notes").unwrap();
        assert_eq!(notes.dashboard_ref.scope, DashboardScope::BuiltIn);
        assert_eq!(notes.declaration.slots, vec!["right-pane"]);
        assert_eq!(notes.declaration.surface.as_deref(), Some("notes"));
        assert_eq!(
            notes.declaration.capabilities,
            vec![
                "records:read:own",
                "records:write:own",
                "sessions:read",
                "sessions:send",
                "context:read",
                "commands:run"
            ]
        );
        notes.declaration.validate().unwrap();
        assert_eq!(notes.status, super::super::PaneStatus::Ready);
    }
    #[test]
    fn rows_match_the_js_built_in_pane_table() {
        let table = include_str!("../../../scripts/lib/builtin-panes.mjs");
        let field = |row: &str, key: &str| {
            let rest = &row[row.find(&format!("{key}: \"")).unwrap() + key.len() + 3..];
            rest[..rest.find('"').unwrap()].to_owned()
        };
        let js: Vec<(String, String)> = table
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("{ id: \""))
            .map(|row| (field(row, "id"), field(row, "source")))
            .collect();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        assert_eq!(
            BUILTINS.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            js.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            "ids and order"
        );
        for ((id, html), (_, source)) in BUILTINS.iter().zip(&js) {
            let bytes = std::fs::read_to_string(root.join(source)).unwrap();
            assert_eq!(
                *html, bytes,
                "{id}: the Rust include_str! and the JS source differ"
            );
        }
    }
    #[test]
    fn file_nav_declaration_validates_and_lists_as_built_in() {
        let home = tempfile::tempdir().unwrap();
        let dir = builtin_panes_dir(home.path(), &Principal::from(42u64));
        super::super::write_pane(&dir,"file-nav-fixture",r#"<!doctype html><title>Files fixture</title><meta name="codesense:slots" content="file-nav"><meta name="codesense:capabilities" content="files:read">"#).unwrap();
        let entry = builtin_at(&dir.join("file-nav-fixture.html"), "acme/api").unwrap();
        assert_eq!(entry.dashboard_ref.scope, DashboardScope::BuiltIn);
        assert_eq!(entry.declaration.slots, vec!["file-nav"]);
        entry.declaration.validate().unwrap();
        assert_eq!(entry.status, super::super::PaneStatus::Ready);
    }
}
