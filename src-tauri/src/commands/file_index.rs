// SPDX-License-Identifier: MIT
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextFile {
    pub name: String,
    pub relative_path: String,
    pub full_path: String,
}

/// Extensions accepted by the viewer (case-insensitive check below).
/// Keep in sync with VIEWER_EXTENSIONS in src/shared/lib/viewer-file-type.ts.
/// SW-SC-7: image extensions are viewable (URL-served `<img>`).
/// SW-VE-2: the code/config/data set — a file the viewer renders read-only is
/// also ⌘P quick-openable (the "keep in sync" contract holds). This is the
/// explicit named set only; the FE catch-all (unknown text → `code`) is a
/// render concern, not a quick-open-index concern.
const VIEWABLE_EXTENSIONS: &[&str] = &[
    "md",
    "markdown",
    "html",
    "htm",
    "excalidraw",
    "png",
    "jpg",
    "jpeg",
    "gif",
    "webp",
    "svg",
    // SW-VE-2: code / config / data
    "yaml",
    "yml",
    "toml",
    "json",
    "xml",
    "ini",
    "env",
    "csv",
    "sh",
    "py",
    "rs",
    "ts",
    "js",
];

pub(crate) fn is_viewable_extension(ext: &str) -> bool {
    VIEWABLE_EXTENSIONS
        .iter()
        .any(|v| v.eq_ignore_ascii_case(ext))
}

/// The rendered BINARY formats (canvas/image) — never edited as text. Their
/// overwrites are *redraws* (the SW-SC-1 ~1/sec canvas autosave + its
/// `.excalidraw.png` sidecar), which the FE must NOT nudge on (SW-FX-26). This is
/// the exclusion set the SW-VE-4 nudge inverts: everything that is NOT one of
/// these is text a human can hold open in the context editor (VE-2's viewer
/// catch-all types every non-binary file — incl. unknown/extensionless ones — as
/// editable `code`), so its overwrite MUST surface live. Superset of the binary
/// members of `VIEWABLE_EXTENSIONS`.
const BINARY_REDRAW_EXTENSIONS: &[&str] =
    &["excalidraw", "png", "jpg", "jpeg", "gif", "webp", "svg"];

/// SW-VE-4: whether `ext` is a rendered binary format whose overwrite is a redraw
/// the FE must stay silent on. The complement — every other extension, plus
/// extensionless files — is treated as editable text that must fire the
/// concurrent-write nudge (see `should_emit_fe_nudge`).
pub fn is_binary_redraw_extension(ext: &str) -> bool {
    BINARY_REDRAW_EXTENSIONS
        .iter()
        .any(|b| b.eq_ignore_ascii_case(ext))
}

/// The shared per-process list memo behind [`list_context_files`].
static FILE_LIST_MEMO: crate::context_query::file_list::FileListMemo =
    crate::context_query::file_list::FileListMemo::new();

/// How long a call waits for the index's first publish before it fails.
const FIRST_PUBLISH_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// SV-PBF-267: the viewable files under `root_path`, served from the context
/// index snapshot (no filesystem walk), memoised per (generation, root).
#[tauri::command]
pub async fn list_context_files<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    root_path: String,
) -> Result<Vec<ContextFile>, String> {
    use tauri::Manager;
    let service = app
        .try_state::<crate::context_query::ContextIndexService>()
        .ok_or("the context index is not running")?
        .inner()
        .clone();
    crate::commands::await_blocking("list_context_files", move || {
        list_from_index(&service, &root_path)
    })
    .await
}

fn list_from_index(
    service: &crate::context_query::ContextIndexService,
    root_path: &str,
) -> Result<Vec<ContextFile>, String> {
    let snapshot = service
        .wait_ready(FIRST_PUBLISH_WAIT)
        .ok_or_else(|| "the context index is still starting; try again shortly".to_string())?;
    let files = FILE_LIST_MEMO.list(&snapshot, root_path)?;
    if files.len() > 50_000 {
        log::warn!(target: "files", "list_context_files: {} lists {} files", root_path, files.len());
    }
    Ok(files.as_ref().clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_query::index::IndexRoot;
    use crate::context_query::model::Layer;
    use crate::context_query::{file_list, ContextIndexService};
    use std::fs;

    fn service_over(dir: &std::path::Path) -> ContextIndexService {
        let service = ContextIndexService::new();
        let root = IndexRoot {
            dir: dir.to_path_buf(),
            org: None,
            team: None,
            layer: Layer::AllOrgs,
        };
        let roots: crate::context_query::service::RootsFn = std::sync::Arc::new(move || {
            Some(crate::context_query::service::Footprint {
                owner: 1,
                roots: vec![root.clone()],
                summary_headings: Vec::new(),
            })
        });
        service.start(roots, std::sync::Arc::new(|_| {}));
        service
    }

    #[test]
    fn lists_viewable_files_from_the_index_without_walking() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("readme.md"), "# Readme").unwrap();
        fs::write(dir.path().join("report.html"), "<h1>R</h1>").unwrap();
        fs::write(dir.path().join("skip.txt"), "not viewable").unwrap();
        let service = service_over(dir.path());
        let root = dir.path().to_string_lossy().into_owned();
        let names: Vec<_> = list_from_index(&service, &root)
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(names, vec!["readme.md", "report.html"]);
    }

    #[test]
    fn a_non_indexed_root_is_refused_with_a_not_indexed_error() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("x.md"), "x").unwrap();
        let service = service_over(dir.path());
        let err = list_from_index(&service, &other.path().to_string_lossy()).unwrap_err();
        assert!(err.starts_with(file_list::NOT_INDEXED_PREFIX), "{err}");
    }
}
