// SPDX-License-Identifier: MIT
//! SW-WV-F6 store-layer tests for the render tripwire (`wv_render_config`).
//!
//! Covers AC-1 (the table migrates cleanly + the 5 default `scope='root'` rows
//! seed idempotently), AC-3 (the default seeds route each input to the correct
//! target when fed through the pure `evaluate_render_target`), AC-4 (the
//! ONE-WRITER grep: no raw `INSERT|UPDATE|DELETE` on `wv_render_config` outside
//! `src/weft/`), and AC-6 (a custom higher-priority rule overrides a default at
//! the same PK). The pure routing logic itself is unit-tested in
//! `weft::render_tripwire`; these tests wire it to the real seeded store.

use std::path::{Path, PathBuf};

use super::*;
use crate::weft::render_tripwire::{evaluate_render_target, RenderTarget};
use crate::weft::store::WeftStore;

fn open_temp() -> (tempfile::TempDir, WeftStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = WeftStore::open(dir.path().join("weft.sqlite3")).unwrap();
    (dir, store)
}

fn count_render_rows(store: &WeftStore) -> i64 {
    let conn = store.conn().unwrap();
    conn.query_row("SELECT COUNT(*) FROM wv_render_config", [], |r| r.get(0))
        .unwrap()
}

// ---------- AC-1: table exists, 5 default rows, idempotent re-open ----------

#[test]
fn render_config_seeds_five_defaults_and_reopen_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("weft.sqlite3");

    let store = WeftStore::open(&path).unwrap();
    assert_eq!(
        count_render_rows(&store),
        5,
        "a fresh store seeds exactly the 5 default render rules"
    );

    // Re-open the SAME path: INSERT OR IGNORE means still 5, no duplicates.
    let reopened = WeftStore::open(&path).unwrap();
    assert_eq!(
        count_render_rows(&reopened),
        5,
        "re-opening an existing store does not duplicate the default rules"
    );
}

#[test]
fn load_render_config_returns_rules_priority_desc() {
    let (_dir, store) = open_temp();
    let rules = store.load_render_config("root").unwrap();
    assert_eq!(rules.len(), 5, "all 5 root-scope rules load");
    // ORDER BY priority DESC — non-increasing.
    for pair in rules.windows(2) {
        assert!(
            pair[0].priority >= pair[1].priority,
            "rules are returned highest-priority-first"
        );
    }
    // An unknown scope loads nothing (no crash).
    assert!(store.load_render_config("nonexistent").unwrap().is_empty());
}

// ---------- AC-3: the seeded defaults route each input correctly ----------

#[test]
fn seeded_defaults_route_each_input() {
    let (_dir, store) = open_temp();
    let rules = store.load_render_config("root").unwrap();

    // research + long → drill_doc
    let long = "r".repeat(1000);
    assert_eq!(
        evaluate_render_target(&long, Some("research"), false, &rules),
        RenderTarget::DrillDoc
    );
    // review → artifact
    assert_eq!(
        evaluate_render_target("x", Some("review"), false, &rules),
        RenderTarget::Artifact
    );
    // has_artifacts=true → artifact regardless of length
    assert_eq!(
        evaluate_render_target(&long, None, true, &rules),
        RenderTarget::Artifact
    );
    // short, no subclass, no artifacts → terminal
    assert_eq!(
        evaluate_render_target("quick note", None, false, &rules),
        RenderTarget::Terminal
    );
}

// ---------- AC-6: a custom higher-priority rule overrides a default ----------

#[test]
fn custom_rule_overrides_default_at_same_pk() {
    let (_dir, store) = open_temp();

    // Insert a custom override at the SAME (scope, kind, threshold) PK as the
    // default message_len=300 rule, retargeting it to `artifact` at priority=100
    // (well above every default). Written directly here in the sanctioned
    // `src/weft/` tree (exempt from the ONE-WRITER grep) via the store's own
    // connection — this is the test proving the schema accepts an INSERT OR
    // REPLACE override without crashing.
    {
        let conn = store.conn().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO wv_render_config \
                 (scope, kind, threshold, target, priority, created_at) \
             VALUES ('root', 'message_len', '300', 'artifact', 100, 0)",
            [],
        )
        .unwrap();
    }

    // Still 5 rows — the override replaced the default at its PK, not appended.
    assert_eq!(count_render_rows(&store), 5);

    let rules = store.load_render_config("root").unwrap();
    // A 250-char message (<= 300) now routes to Artifact because the message_len
    // rule at threshold 300 was retargeted to `artifact` at the highest priority.
    let msg = "m".repeat(250);
    assert_eq!(
        evaluate_render_target(&msg, None, false, &rules),
        RenderTarget::Artifact
    );
}

// ---------- AC-4: ONE-WRITER grep — no raw wv_render_config writes outside weft ----------

fn scan_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if !dir.exists() {
        return;
    }
    for entry in std::fs::read_dir(dir).expect("read scan dir") {
        let entry = entry.expect("dir entry");
        if entry.file_type().map(|t| t.is_symlink()).unwrap_or(true) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// The whole `src/weft/` module is the sanctioned home for raw `wv_*` SQL
/// (ARCHITECTURE §1.2) — its own SQL string literals must not self-trip.
fn is_sanctioned_weft_file(path: &Path) -> bool {
    let s = path.to_string_lossy().replace('\\', "/");
    s.contains("/src/weft/") || s.ends_with("src/weft.rs")
}

#[test]
fn render_tripwire_one_writer() {
    let mut files = Vec::new();
    rust_files(&scan_root(), &mut files);

    let mut violations = Vec::new();
    for file in files {
        if is_sanctioned_weft_file(&file) {
            continue;
        }
        let src = std::fs::read_to_string(&file).expect("read source");
        for (i, line) in src.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
                continue;
            }
            // Whitespace-squished so `INSERT   INTO   wv_render_config` still matches.
            let code: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            let hits = code.contains("INSERTINTOwv_render_config")
                || code.contains("INSERTORREPLACEINTOwv_render_config")
                || code.contains("INSERTORIGNOREINTOwv_render_config")
                || code.contains("UPDATEwv_render_config")
                || code.contains("DELETEFROMwv_render_config");
            if hits {
                violations.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "SW-WV-F6 ONE-WRITER: raw SQL against `wv_render_config` outside \
         `src/weft/`. Route the write through a `weft::` handler:\n{}",
        violations.join("\n")
    );
}
