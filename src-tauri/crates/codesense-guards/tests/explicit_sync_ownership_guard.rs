// SPDX-License-Identifier: MIT
//! Durable guards that directory ownership (X4) stays gone.
//!
//! X4: no code reads or writes a designation, no UI offers
//! ownership, and `designations.yml` keeps only team nav pins. The checks here
//! are source-driven, like `explicit_sync_reverse_guard.rs`:
//! - the Tauri invoke registry lacks the ownership commands and keeps the pins;
//! - the frontend bindings invoke none of the removed commands;
//! - the managed-surface write guard (K4) is still consulted;
//! - an allow-list scan over
//!   `src-tauri/src` and the frontend `src` for the ownership vocabulary
//!   ([`NEEDLES`]). Test code is exempt: `#[cfg(test)]` items, `*_tests.rs` /
//!   `tests.rs` / `tests/` files, and `*.test.ts(x)`. Every other hit must be in
//!   `explicit_sync_ownership_allowlist.txt` with its reason (pins or
//!   migration) and its exact hit count, so a new reader in an allow-listed
//!   file still fails; a stale entry fails too.
//!
//! The MCP registry check lives beside the served router
//! (`mcp::handler` `xs5_the_surface_scope_writer_is_not_served`), and the
//! loader-type check beside the loader (`install::team_pins`).
//!
//! Std-only; point `CODESENSE_GUARD_ROOT` at another checkout's `src-tauri` to
//! run it against that tree.

use codesense_guards::guard_support;
use codesense_guards::guard_support::{app_root as root, read};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The command paths registered in `generate_handler![…]`, one per entry
/// (`commands::worktree::set_team_nav_pin`), comments and attributes stripped.
/// Handles a last entry with no trailing comma.
fn tauri_registry() -> Vec<String> {
    let lib = read("src/lib.rs");
    let start = lib
        .find("generate_handler![")
        .expect("generate_handler! not found")
        + "generate_handler![".len();
    let block = &lib[start..start + lib[start..].find("])").expect("registry end")];
    block
        .lines()
        .map(|line| line.split("//").next().unwrap_or_default())
        .flat_map(|line| line.split(','))
        .map(|entry| {
            let mut entry = entry.trim();
            while entry.starts_with("#[") {
                entry = entry[entry.find(']').map_or(entry.len(), |i| i + 1)..].trim();
            }
            entry.to_string()
        })
        .filter(|entry| !entry.is_empty())
        .collect()
}

fn registered(registry: &[String], command: &str) -> bool {
    registry
        .iter()
        .any(|entry| entry.rsplit("::").next() == Some(command))
}

const REMOVED_COMMANDS: [&str; 3] = [
    "get_surface_scope",
    "get_dir_file_ownership",
    "set_surface_designation",
];

/// The ownership read/write commands are not in the Tauri invoke registry; the
/// team nav pin commands (K8) are.
#[test]
fn ownership_commands_are_unregistered_and_pins_stay() {
    let registry = tauri_registry();
    assert!(
        registered(&registry, "check_branch_exists"),
        "the registry parse found no commands — the guard would pass vacuously"
    );
    for command in REMOVED_COMMANDS {
        assert!(
            !registered(&registry, command),
            "`{command}` is still a registered Tauri command"
        );
    }
    for command in ["set_team_nav_pin", "get_team_nav_pins"] {
        assert!(
            registered(&registry, command),
            "`{command}` (team nav pins, K8) must stay registered"
        );
    }
}

/// The frontend bindings invoke none of the removed commands.
#[test]
fn frontend_bindings_invoke_no_ownership_command() {
    let commands_ts = read("../src/tauri/commands.ts");
    for command in REMOVED_COMMANDS {
        assert!(
            !commands_ts.contains(&format!("\"{command}\"")),
            "src/tauri/commands.ts still invokes `{command}`"
        );
    }
}

/// The managed-surface write guard (K4) is write safety, not ownership: it
/// survives the removal. (The forward materialize that also consulted
/// it is gone with the legacy engine; the wave's store write still does.)
#[test]
fn the_managed_surface_write_guard_stays() {
    let index = read("src/install/worktree_index.rs");
    assert!(
        index.contains("pub(crate) fn is_codesense_managed_target("),
        "the managed-surface guard is gone"
    );
    let wave = read("src/content_engine/sync/wave.rs");
    assert!(
        wave.contains("is_codesense_managed_target("),
        "the wave's store write no longer consults is_codesense_managed_target"
    );
}

// --- The allow-list scan -----------------------------------------------------

const ALLOWLIST: &str = "tests/explicit_sync_ownership_allowlist.txt";
const REASONS: [&str; 2] = ["pins", "migration"];

/// The ownership vocabulary. `(needle, case_insensitive, whole_word)`: a
/// whole-word needle must not be glued to an identifier character on either
/// side (so `ScopeConfig` doesn't match `projectMcpScopeConfig`).
const NEEDLES: &[(&str, bool, bool)] = &[
    ("designation", true, false),
    ("surface_scope", true, false),
    ("surfacescope", true, false),
    ("file_ownership", true, false),
    ("fileownership", true, false),
    ("ScopeConfig", false, true),
    ("inherit-only", true, false),
    ("is_install_designated", false, false),
    ("reverse_syncable", false, false),
    (".scope_context", false, false),
    (".scope_install", false, false),
    ("epic_scope", false, false),
];

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn line_hits(line: &str) -> bool {
    let lower = line.to_lowercase();
    NEEDLES.iter().any(|&(needle, insensitive, whole_word)| {
        let (hay, needle) = if insensitive {
            (lower.as_str(), needle.to_lowercase())
        } else {
            (line, needle.to_string())
        };
        hay.match_indices(needle.as_str()).any(|(i, m)| {
            !whole_word
                || (!hay[..i].chars().next_back().is_some_and(is_ident)
                    && !hay[i + m.len()..].chars().next().is_some_and(is_ident))
        })
    })
}

/// Per line: is it inside a `#[cfg(test)]` item? The item after the attribute
/// runs to its matching close brace (or a `;` before any brace). Braces inside
/// string, raw-string and char literals and comments are skipped.
fn rust_test_lines(src: &str) -> Vec<bool> {
    let lines: Vec<&str> = src.lines().collect();
    let mut in_test = vec![false; lines.len()];
    let mut offsets = Vec::with_capacity(lines.len());
    let mut acc = 0usize;
    for l in &lines {
        offsets.push(acc);
        acc += l.len() + 1;
    }
    let line_of = |byte: usize| offsets.partition_point(|&o| o <= byte) - 1;
    let bytes = src.as_bytes();
    let mut search = 0usize;
    while let Some(found) = src[search..].find("#[cfg(test)]") {
        let attr = search + found;
        let mut i = attr + "#[cfg(test)]".len();
        let mut depth = 0usize;
        let mut end = None;
        while i < bytes.len() {
            match bytes[i] {
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                b'r' if bytes.get(i + 1) == Some(&b'#') || bytes.get(i + 1) == Some(&b'"') => {
                    let mut j = i + 1;
                    let mut hashes = 0;
                    while bytes.get(j) == Some(&b'#') {
                        hashes += 1;
                        j += 1;
                    }
                    if bytes.get(j) == Some(&b'"') && (i == 0 || !is_ident(bytes[i - 1] as char)) {
                        let close = format!("\"{}", "#".repeat(hashes));
                        i = j + 1 + src[j + 1..].find(&close).map_or(0, |k| k + close.len());
                        continue;
                    }
                }
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                }
                b'\'' if bytes.get(i + 2) == Some(&b'\'') || bytes.get(i + 1) == Some(&b'\\') => {
                    i += if bytes.get(i + 1) == Some(&b'\\') {
                        4
                    } else {
                        3
                    };
                    continue;
                }
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                b';' if depth == 0 => {
                    end = Some(i);
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        let end = end.unwrap_or(bytes.len() - 1);
        for flag in &mut in_test[line_of(attr)..=line_of(end)] {
            *flag = true;
        }
        search = end + 1;
    }
    in_test
}

fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name.ends_with("_tests.rs")
        || name == "tests.rs"
        || rel.contains("/tests/")
        || name.contains(".test.")
}

fn sources(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "node_modules") {
                continue;
            }
            sources(&path, exts, out);
        } else if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| exts.contains(&e))
        {
            out.push(path);
        }
    }
}

/// `path → non-test hit count` for every file whose production code says the
/// ownership vocabulary.
fn hits() -> BTreeMap<String, usize> {
    let mut roots: Vec<(PathBuf, String, &[&str])> = guard_support::source_roots()
        .into_iter()
        .map(|base| {
            let label = format!("src-tauri/{}", base.strip_prefix(root()).unwrap().display());
            (base, label, &["rs"][..])
        })
        .collect();
    roots.push((root().join("../src"), "src".into(), &["ts", "tsx"]));
    let mut out = BTreeMap::new();
    for (base, label, exts) in roots {
        let mut files = Vec::new();
        sources(&base, exts, &mut files);
        for file in files {
            let rel = file
                .strip_prefix(&base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let key = format!("{label}/{rel}");
            if is_test_file(&key) {
                continue;
            }
            let text = fs::read_to_string(&file).unwrap_or_default();
            let test_lines = if rel.ends_with(".rs") {
                rust_test_lines(&text)
            } else {
                Vec::new()
            };
            let count = text
                .lines()
                .enumerate()
                .filter(|(n, line)| {
                    !test_lines.get(*n).copied().unwrap_or(false) && line_hits(line)
                })
                .count();
            if count > 0 {
                out.insert(key, count);
            }
        }
    }
    out
}

/// `path → (reason, pinned hit count)` from the allow-list file. Lines are
/// `<path> <reason> <count> # note`.
fn allowlist() -> BTreeMap<String, (String, usize)> {
    let mut out = BTreeMap::new();
    for (n, raw) in read(ALLOWLIST).lines().enumerate() {
        let line = raw.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let path = parts.next().unwrap().to_string();
        let reason = parts
            .next()
            .unwrap_or_else(|| panic!("{ALLOWLIST}:{}: `{path}` has no reason", n + 1));
        assert!(
            REASONS.contains(&reason),
            "{ALLOWLIST}:{}: reason `{reason}` is not one of {REASONS:?}",
            n + 1
        );
        let count: usize = parts
            .next()
            .and_then(|c| c.parse().ok())
            .unwrap_or_else(|| panic!("{ALLOWLIST}:{}: `{path}` has no hit count", n + 1));
        out.insert(path, (reason.to_string(), count));
    }
    out
}

/// Production code says the ownership vocabulary only in the
/// allow-listed pins / migration files, exactly as often as pinned, and the
/// list carries no stale entry.
#[test]
fn designation_mentions_are_only_pins_migration_and_tests() {
    let hits = hits();
    let allowed = allowlist();
    let mut problems = Vec::new();
    for (path, count) in &hits {
        match allowed.get(path) {
            None => problems.push(format!("{path}: {count} lines, not allow-listed")),
            Some((_, pinned)) if pinned != count => {
                problems.push(format!("{path}: {count} lines, allow-list pins {pinned}"))
            }
            Some(_) => {}
        }
    }
    for path in allowed.keys().filter(|p| !hits.contains_key(*p)) {
        problems.push(format!(
            "{path}: allow-listed but no longer mentions it; drop it"
        ));
    }
    assert!(
        problems.is_empty(),
        "directory ownership is gone (X4); production code outside the pins / migration \
         allow-list ({ALLOWLIST}) still uses its vocabulary:\n{}",
        problems.join("\n")
    );
}

/// The scanner's own behavior: test regions are exempt, word boundaries hold,
/// and a planted production hit is found.
#[test]
fn scanner_self_test() {
    let src = "fn prod() { let _ = ScopeConfig::default(); }\n#[cfg(test)]\nmod tests {\n    fn t() { let s = \"}\"; ScopeConfig::default(); }\n}\nfn after() { projectMcpScopeConfig(); }\n";
    let test_lines = rust_test_lines(src);
    let flagged: Vec<usize> = src
        .lines()
        .enumerate()
        .filter(|(n, l)| !test_lines[*n] && line_hits(l))
        .map(|(n, _)| n)
        .collect();
    assert_eq!(flagged, vec![0], "only the production line is flagged");
    assert!(line_hits("x.scope_context.is_some()"));
    assert!(line_hits("// marks it inherit-only"));
    assert!(!line_hits("projectMcpScopeConfig(sessionId)"));
}
