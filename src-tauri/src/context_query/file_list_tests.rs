// SPDX-License-Identifier: MIT
use std::sync::Arc;

use super::file_list::{FileListMemo, MAX_MEMO_ROOTS, NOT_INDEXED_PREFIX};
use super::index::{IndexRoot, Snapshot};
use super::model::{Kind, Layer};
use super::record::ContextRecord;

fn record(path: &str) -> Arc<ContextRecord> {
    Arc::new(ContextRecord {
        headings: Vec::new(),
        path: path.to_string(),
        rel_path: String::new(),
        file_name: path.rsplit('/').next().unwrap().to_string(),
        org: None,
        team: None,
        layer: Layer::Team,
        kind: Kind::Doc,
        mtime_ms: None,
        title: None,
        fields: Default::default(),
        is_pipeline_doc: false,
        metadata_truncated: false,
        metadata_valid: true,
        summary: None,
    })
}

fn snapshot(roots: &[&str], paths: &[&str], generation: u64) -> Arc<Snapshot> {
    let mut records: Vec<_> = paths.iter().map(|p| record(p)).collect();
    records.sort_by(|a, b| a.path.cmp(&b.path));
    let roots = roots
        .iter()
        .map(|d| IndexRoot {
            dir: (*d).into(),
            org: None,
            team: None,
            layer: Layer::Team,
        })
        .collect();
    Arc::new(Snapshot::build(records, roots, generation, true))
}

fn names(files: &[crate::commands::file_index::ContextFile]) -> Vec<&str> {
    files.iter().map(|f| f.relative_path.as_str()).collect()
}

#[test]
fn matches_the_old_walker_apart_from_the_documented_exclusions() {
    let snap = snapshot(
        &["/c"],
        &[
            "/c/top.md",
            "/c/sub/nested.md",
            "/c/sub/skip.txt",
            "/c/page.htm",
            "/c/notes.markdown",
            "/c/.hidden.md",
            "/c/.dir/inner.md",
            "/c/design.excalidraw.png",
            "/c/plain.png",
        ],
        1,
    );
    let files = FileListMemo::new().list(&snap, "/c").unwrap();
    assert_eq!(
        names(&files),
        vec![
            "notes.markdown",
            "page.htm",
            "plain.png",
            "sub/nested.md",
            "top.md"
        ]
    );
    assert_eq!(files[3].full_path, "/c/sub/nested.md");
    assert_eq!(files[3].name, "nested.md");
}

// Q3: secret paths are not listed (the old walker listed `prod.env`).
#[test]
fn secret_paths_are_not_listed() {
    let snap = snapshot(
        &["/c"],
        &[
            "/c/prod.env",
            "/c/keys/server.pem",
            "/c/ok.md",
            "/c/notes/readme.md",
        ],
        1,
    );
    let files = FileListMemo::new().list(&snap, "/c").unwrap();
    assert_eq!(names(&files), vec!["notes/readme.md", "ok.md"]);
}

// Q3: a root the index does not hold is refused with a recognisable error.
#[test]
fn a_non_indexed_root_is_refused() {
    let snap = snapshot(&["/c"], &["/c/a.md"], 1);
    let err = FileListMemo::new().list(&snap, "/elsewhere").unwrap_err();
    assert_eq!(err, format!("{NOT_INDEXED_PREFIX} /elsewhere"));
    // A sibling that merely shares a name prefix is not under the root.
    assert!(FileListMemo::new().list(&snap, "/c-other").is_err());
}

#[test]
fn a_folder_under_an_indexed_root_lists_relative_to_itself() {
    let snap = snapshot(&["/c"], &["/c/a.md", "/c/sub/b.md", "/c/sub/deep/c.md"], 1);
    let files = FileListMemo::new().list(&snap, "/c/sub/").unwrap();
    assert_eq!(names(&files), vec!["b.md", "deep/c.md"]);
}

#[test]
fn calls_at_one_generation_filter_once_and_share_an_arc() {
    let snap = snapshot(&["/c"], &["/c/a.md"], 7);
    let memo = FileListMemo::new();
    let a = memo.list(&snap, "/c").unwrap();
    let b = memo.list(&snap, "/c/").unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    let newer = snapshot(&["/c"], &["/c/a.md", "/c/b.md"], 8);
    let c = memo.list(&newer, "/c").unwrap();
    assert!(!Arc::ptr_eq(&a, &c));
    assert_eq!(c.len(), 2);
    assert_eq!(memo.len(), 1, "only the newest generation is kept");
}

#[test]
fn the_memo_is_capped_at_64_roots() {
    let dirs: Vec<String> = (0..MAX_MEMO_ROOTS + 10).map(|i| format!("/r{i}")).collect();
    let roots: Vec<&str> = dirs.iter().map(String::as_str).collect();
    let snap = snapshot(&roots, &[], 1);
    let memo = FileListMemo::new();
    for d in &dirs {
        memo.list(&snap, d).unwrap();
    }
    assert_eq!(memo.len(), MAX_MEMO_ROOTS);
}
