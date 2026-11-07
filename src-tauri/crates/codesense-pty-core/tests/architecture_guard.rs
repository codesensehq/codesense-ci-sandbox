// SPDX-License-Identifier: MIT
//! The Rust half of the terminal architecture guard (the frontend half is
//! `scripts/terminal-arch-guard.ts`; both run from
//! `scripts/check-terminal-arch.mjs`, called by `npm run lint`):
//!
//! - (a) no symbol of the deleted byte replay and delta recovery comes back
//!   in any `.rs` file under `src-tauri`, code or comment;
//! - (e) no `Cargo.toml` takes a terminal emulator other than xterm.js (the
//!   host's emulator worker) as a direct dependency: one parser.
//!
//! `TERMINAL_ARCH_GUARD_ROOT` scans another tree instead of `src-tauri`.

use std::path::{Path, PathBuf};

const DELETED_SYMBOLS: [&str; 6] = [
    "ReplayBuffer",
    "replay_tail",
    "snapshot_delta",
    "SnapshotDelta",
    "resnapshot",
    "needs_cold_settle",
];
const OTHER_EMULATORS: [&str; 4] = ["alacritty_terminal", "vt100", "wezterm-term", "vte"];
const FIXTURES: &str = "architecture_guard_fixtures";

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !name.starts_with("target") && name != "node_modules" && name != FIXTURES {
                files(&path, out);
            }
        } else {
            out.push(path);
        }
    }
}

/// Every violation under `root`, as `(clause, "file:line: text")`.
fn scan(root: &Path) -> Vec<(char, String)> {
    let mut all = Vec::new();
    files(root, &mut all);
    let this_file = Path::new(file!()).file_name().unwrap();
    let mut violations = Vec::new();
    for path in all {
        let name = path.file_name().unwrap_or_default();
        let is_rust = path.extension().is_some_and(|ext| ext == "rs") && name != this_file;
        let is_manifest = name == "Cargo.toml";
        if !is_rust && !is_manifest {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            let at = |clause: char| {
                (
                    clause,
                    format!("{}:{}: {}", path.display(), index + 1, line.trim()),
                )
            };
            if is_rust && DELETED_SYMBOLS.iter().any(|symbol| line.contains(symbol)) {
                violations.push(at('a'));
            }
            if is_manifest {
                let key = line.split('=').next().unwrap_or_default().trim();
                if OTHER_EMULATORS.contains(&key) {
                    violations.push(at('e'));
                }
            }
        }
    }
    violations
}

fn fixture(clause: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(FIXTURES)
        .join(clause)
}

#[test]
fn the_tree_is_clean() {
    let root = std::env::var_os("TERMINAL_ARCH_GUARD_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let violations = scan(&root);
    assert!(
        violations.is_empty(),
        "terminal architecture guard:\n{}",
        violations
            .iter()
            .map(|(clause, at)| format!("({clause}) {at}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn clause_a_fires_on_its_fixture() {
    assert!(scan(&fixture("a")).iter().any(|(clause, _)| *clause == 'a'));
}

#[test]
fn clause_e_fires_on_its_fixture() {
    assert!(scan(&fixture("e")).iter().any(|(clause, _)| *clause == 'e'));
}
