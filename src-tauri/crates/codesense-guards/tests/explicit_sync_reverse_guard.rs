// SPDX-License-Identifier: MIT
//! Durable guards that no automatic REVERSE sync trigger survives.
//!
//! The behavioral half lives in `src/sync_no_auto_reverse_tests.rs` (each
//! surviving stimulus driven against a live worktree with an enqueue counter).
//! This file holds the durable checks, source-driven
//! like `explicit_sync_forward_guard.rs`:
//! - an enum-variant test that the removed reverse `WaveReason`s are gone;
//! - a registry test that the removed Tauri commands are unregistered (and the
//!   upgrade-notice commands are registered);
//! - no `spawn_reverse_*` / `sweep_reverse_*` symbol remains, and app setup
//!   spawns no reverse loop;
//! - a symbol-anchored scan of `src/` for the removed reverse machinery.
//!
//! Std-only; point `CODESENSE_GUARD_ROOT` at another checkout's `src-tauri` to
//! run it against that tree. The transitive call-graph guard
//! (`explicit_sync_unattended_writer_guard.rs`) generalizes these per-function
//! checks.

use codesense_guards::guard_support;
use codesense_guards::guard_support::read;
use std::fs;
use std::path::{Path, PathBuf};

/// Strip `//` line comments (outside string literals) so prose that names a
/// removed symbol never trips a check.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let mut in_str = false;
        let mut prev = '\0';
        let mut cut = line.len();
        let chars: Vec<char> = line.chars().collect();
        let mut byte_idx = 0;
        for (i, &c) in chars.iter().enumerate() {
            if c == '"' && prev != '\\' {
                in_str = !in_str;
            }
            if !in_str && c == '/' && chars.get(i + 1) == Some(&'/') {
                cut = byte_idx;
                break;
            }
            prev = c;
            byte_idx += c.len_utf8();
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

/// The body of the first `fn <name>` (`fn <name>(` or `fn <name><`), comments
/// stripped. `None` when absent.
fn fn_body(src: &str, name: &str) -> Option<String> {
    let src = strip_comments(src);
    let start = [format!("fn {name}("), format!("fn {name}<")]
        .iter()
        .filter_map(|needle| src.find(needle.as_str()))
        .min()?;
    let open = start + src[start..].find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut prev = '\0';
    for (offset, c) in src[open..].char_indices() {
        if c == '"' && prev != '\\' {
            in_str = !in_str;
        }
        if !in_str {
            if c == '{' {
                depth += 1;
            } else if c == '}' {
                depth -= 1;
                if depth == 0 {
                    return Some(src[open + 1..open + offset].to_string());
                }
            }
        }
        prev = c;
    }
    None
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `(file:line: text)` for every non-comment line under `src/` containing any
/// needle, skipping `skip_files` (paths relative to `src/`).
fn scan(needles: &[&str], skip_files: &[&str]) -> Vec<String> {
    let mut files = Vec::new();
    for base in guard_support::source_roots() {
        rust_sources(&base, &mut files);
    }
    let mut hits = Vec::new();
    for file in files {
        let rel = guard_support::source_label(&file);
        if skip_files.contains(&rel.as_str()) {
            continue;
        }
        let src = strip_comments(&fs::read_to_string(&file).unwrap());
        for (n, line) in src.lines().enumerate() {
            for needle in needles {
                if line.contains(needle) {
                    hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                }
            }
        }
    }
    hits
}

/// The removed reverse wave reasons are not variants of `WaveReason`; the two
/// survivors (the migration cutover and the agent-initiated reconcile) remain.
#[test]
fn removed_reverse_wave_reasons_are_not_enum_variants() {
    let wave = strip_comments(&read("src/content_engine/sync/wave.rs"));
    let start = wave
        .find("pub enum WaveReason {")
        .expect("WaveReason enum not found");
    let body = &wave[start..start + wave[start..].find("\n}\n").expect("enum end")];
    let variants: Vec<&str> = body
        .lines()
        .skip(1)
        .filter(|l| l.starts_with("    ") && !l.starts_with("     "))
        .filter_map(|l| {
            l.trim()
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()
        })
        .filter(|v| !v.is_empty())
        .collect();
    for removed in ["ReverseSync", "ReverseDirtyBatch", "ReverseClaim"] {
        assert!(
            !variants.contains(&removed),
            "WaveReason::{removed} is still a variant ({variants:?})"
        );
    }
    // The agent reconcile is gone too.
    assert!(
        !variants.contains(&"AttachReconcile"),
        "WaveReason::AttachReconcile is still a variant"
    );
    assert!(
        variants.contains(&"MigrationCutover"),
        "WaveReason::MigrationCutover must remain"
    );
}

fn tauri_registry() -> String {
    let lib = strip_comments(&read("src/lib.rs"));
    let start = lib
        .find("generate_handler![")
        .expect("generate_handler! not found");
    lib[start..start + lib[start..].find("])").expect("registry end")].to_string()
}

/// The removed reverse settings and conflict commands are not in the Tauri invoke
/// registry, nor is the one-time upgrade notice (a live count replaced it); the
/// count and its Publish remain registered.
#[test]
fn removed_reverse_commands_are_unregistered() {
    let registry = tauri_registry();
    for command in [
        "set_reverse_sync_overrides",
        "set_reverse_sync_default",
        "resolve_conflict",
        "list_conflicts",
        "get_unpublished_context_notice",
        "ack_unpublished_context_notice",
        "preview_reverse_sync",
    ] {
        assert!(
            !registry.contains(&format!("::{command},")),
            "`{command}` is still a registered Tauri command"
        );
    }
    for command in [
        "team_file_edits_status",
        "publish_context_edits",
        "discard_team_file_edits",
    ] {
        assert!(
            registry.contains(&format!("::{command},")),
            "`{command}` must be a registered Tauri command"
        );
    }
    assert!(
        guard_support::absent("commands/conflict.rs"),
        "commands/conflict.rs (the auto-conflict commands) still exists"
    );
}

/// No `spawn_reverse_*` or `sweep_reverse_*` symbol remains anywhere in
/// `src/`.
#[test]
fn no_spawn_or_sweep_reverse_symbols_remain() {
    let hits = scan(&["spawn_reverse_", "sweep_reverse_"], &[]);
    assert!(
        hits.is_empty(),
        "reverse loop/sweep symbols remain:\n{}",
        hits.join("\n")
    );
}

/// App setup spawns no reverse loop. The three stimulus seams the setup
/// wires — the sync timers, the window-focus handler and the OSC-133
/// `CommandEnd` handler — reach no wave, sweep, claim or spawn.
#[test]
fn app_setup_spawns_no_reverse_loop() {
    let lib = read("src/lib.rs");
    for seam in [
        "spawn_sync_timers",
        "on_window_focus_changed",
        "on_osc133_command_end",
    ] {
        let body = fn_body(&lib, seam).unwrap_or_else(|| panic!("fn {seam} not found"));
        for forbidden in [
            "spawn", "sweep", "enqueue", "claim", "reverse", "cadence", "Wave",
        ] {
            assert!(
                !body.contains(forbidden),
                "`{seam}` reaches `{forbidden}` — an automatic reverse trigger survived:\n{body}"
            );
        }
    }
    let setup = strip_comments(&lib);
    assert!(
        setup.contains("spawn_sync_timers(app.handle())"),
        "app setup no longer routes its sync timers through the guarded seam"
    );
    assert!(
        setup.contains("on_window_focus_changed(window.app_handle()"),
        "the focus handler no longer routes through the guarded seam"
    );
    assert!(
        setup.contains("on_osc133_command_end(&handle"),
        "the OSC-133 CommandEnd handler no longer routes through the guarded seam"
    );
}

/// The workspace-open command, team activation and the designation write reach
/// no reverse claim or watch.
#[test]
fn open_activation_and_designation_reach_no_reverse_claim() {
    let watch = read("src/commands/watch.rs");
    for func in ["lazy_attach_workspace", "register_team_activation_watches"] {
        let body = fn_body(&watch, func).unwrap_or_else(|| panic!("fn {func} not found"));
        for forbidden in ["claim", "reverse", "enqueue"] {
            assert!(
                !body.contains(forbidden),
                "`{func}` reaches `{forbidden}`:\n{body}"
            );
        }
    }
    // R8: the directory designation write is gone outright, so marking a
    // directory can no longer kick a reverse claim.
    let worktree = read("src/commands/worktree.rs");
    assert!(
        fn_body(&worktree, "set_surface_designation").is_none(),
        "R8: the directory designation write is back"
    );
}

/// The symbol-anchored scan of `src/` for the removed reverse machinery
/// (comments excluded). The kept primitives (`claim_now`, `execute_reverse_sync`,
/// `classify_reverse`) are deliberately not listed.
#[test]
fn symbol_anchored_scan_finds_no_removed_reverse_symbol() {
    let needles = [
        "WaveReason::ReverseSync",
        "WaveReason::ReverseDirtyBatch",
        "WaveReason::ReverseClaim",
        "push_reverse_dirty",
        "drain_reverse_backlog",
        "enqueue_reverse_",
        "flush_and_drop_lockfile",
        "record_and_emit_conflict",
        "conflict_registry",
        "ConflictRegistry",
        "apply_conflict_resolution",
        "query_projection_dests",
        "ReverseSyncOverride",
        "ReverseCadenceMode",
        "reverse_cadence",
        "register_team_reverse_watch",
        "team_reverse_dirs",
        "DirRole::Reverse",
        "build_reverse_handler",
        "set_reverse_session_filter",
        "\"conflict-detected\"",
        "\"conflict-resolved\"",
    ];
    // The behavioral test module documents the stimuli by name in strings.
    let hits = scan(&needles, &["sync_no_auto_reverse_tests.rs"]);
    assert!(
        hits.is_empty(),
        "removed reverse symbols remain:\n{}",
        hits.join("\n")
    );
    assert!(
        guard_support::absent("content_engine/sync/cadence.rs"),
        "the reverse cadence module still exists"
    );
}

/// End of the `{`-block starting at byte `open` (which must be `{`): string,
/// raw-string and char-literal aware. Returns the index just past the matching `}`.
fn block_end(src: &str, open: usize) -> Option<usize> {
    let b = src.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'r' if src[i..].starts_with("r#\"") || src[i..].starts_with("r\"") => {
                let hashes = src[i + 1..].bytes().take_while(|c| *c == b'#').count();
                let close = format!("\"{}", "#".repeat(hashes));
                let body = i + 2 + hashes;
                i = body + src[body..].find(&close)? + close.len() - 1;
            }
            b'\'' => {
                // char literal ('{', '\'', '\n') vs lifetime ('a)
                if i + 2 < b.len() && b[i + 2] == b'\'' {
                    i += 2;
                } else if i + 3 < b.len() && b[i + 1] == b'\\' && b[i + 3] == b'\'' {
                    i += 3;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `src` with every `#[cfg(test)]`-gated item removed (comments already stripped).
fn strip_cfg_test(src: &str) -> String {
    let mut out = src.to_string();
    while let Some(at) = out.find("#[cfg(test)]") {
        let rest = &out[at..];
        let brace = rest.find('{');
        let semi = rest.find(';');
        let end = match (brace, semi) {
            (Some(b), Some(s)) if s < b => at + s + 1,
            (Some(b), _) => block_end(&out, at + b).unwrap_or(out.len()),
            (None, Some(s)) => at + s + 1,
            (None, None) => out.len(),
        };
        out.replace_range(at..end, "");
    }
    out
}

/// `(file, enclosing fn)` for every non-test call of `symbol` under `src/`.
fn production_callers(symbol: &str) -> Vec<(String, String)> {
    let mut files = Vec::new();
    for base in guard_support::source_roots() {
        rust_sources(&base, &mut files);
    }
    let mut out = Vec::new();
    for file in files {
        let rel = guard_support::source_label(&file);
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with("tests.rs") || name == "test_support.rs" || rel.contains("/tests/") {
            continue;
        }
        let src = strip_cfg_test(&strip_comments(&fs::read_to_string(&file).unwrap()));
        let mut from = 0;
        while let Some(hit) = src[from..].find(symbol) {
            let at = from + hit;
            from = at + symbol.len();
            let before = src[..at].chars().last().unwrap_or(' ');
            let after = &src[at + symbol.len()..];
            let is_ident_char = |c: char| c.is_alphanumeric() || c == '_';
            if is_ident_char(before) || after.starts_with(|c: char| is_ident_char(c)) {
                continue;
            }
            if !(after.starts_with('(') || after.starts_with("::<")) {
                continue; // an import, a doc link or a path, not a call
            }
            if src[..at].ends_with("fn ") {
                continue; // the definition
            }
            let enclosing = src[..at]
                .rfind("fn ")
                .map(|f| {
                    src[f + 3..]
                        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                        .next()
                        .unwrap_or("")
                        .to_string()
                })
                .unwrap_or_default();
            out.push((rel.clone(), enclosing));
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The write primitives that adopt worktree edits into team content are
/// reachable from production code ONLY through the explicit legacy MCP verb
/// (`mcp/handler.rs`) and the wave arm that runs it (`run_repo_projection`).
/// `claim_now` / `execute_reverse_sync` have no production caller.
#[test]
fn reverse_write_primitives_have_only_the_allowed_callers() {
    // The legacy reverse primitives have no production caller left;
    // the one way into team content is the engine's publish, reached only
    // from the explicit operations (`reverse_sync_worktree`).
    let allowed: &[(&str, &[(&str, &str)])] = &[
        ("claim_now", &[]),
        ("execute_reverse_sync", &[]),
        ("reconcile_manifest_projections", &[]),
        (
            "publish::publish",
            &[("mcp/context_operations/explicit.rs", "execute_inner")],
        ),
    ];
    let mut violations = Vec::new();
    for (symbol, ok) in allowed {
        for (file, func) in production_callers(symbol) {
            if !ok.iter().any(|(f, g)| *f == file && *g == func) {
                violations.push(format!("{symbol} called from {file}::{func}"));
            }
        }
    }
    // Non-vacuous: the scan does see the allowed production caller.
    assert_eq!(
        production_callers("publish::publish"),
        vec![(
            "mcp/context_operations/explicit.rs".to_string(),
            "execute_inner".to_string()
        )]
    );
    assert!(
        violations.is_empty(),
        "unallowed callers of reverse write primitives:\n{}",
        violations.join("\n")
    );
}

/// Self-test: the caller scan ignores test-only code and catches a planted call.
#[test]
fn caller_scan_self_test() {
    let planted = "fn sneaky() { claim_now(a); }\n#[cfg(test)]\nmod t { fn x() { claim_now(b); } }";
    let stripped = strip_cfg_test(planted);
    assert!(stripped.contains("fn sneaky"));
    assert_eq!(stripped.matches("claim_now(").count(), 1);
    assert_eq!(block_end("{ '{' \"}\" { } }", 0), Some(15));
}

/// The text of the closure passed to `builder.on_window_event(` and of the
/// `.setup(` closure in `lib.rs` (comments and test items stripped).
fn lib_closure(lib: &str, opener: &str) -> String {
    let at = lib
        .find(opener)
        .unwrap_or_else(|| panic!("{opener} not found"));
    let open = at + lib[at..].find('{').expect("closure body");
    lib[open..block_end(lib, open).expect("closure end")].to_string()
}

/// The whole focus/window handler and the whole setup block — not only the
/// named seams — reach no reverse sweep, claim, enqueue or reverse spawn.
#[test]
fn window_event_handler_and_setup_block_reach_no_reverse_trigger() {
    let lib = strip_cfg_test(&strip_comments(&read("src/lib.rs")));
    let forbidden = [
        "sweep_reverse",
        "enqueue_reverse",
        "push_reverse",
        "spawn_reverse",
        "reverse_cadence",
        "claim_now",
        "execute_reverse_sync",
        "enqueue_attach_reconcile",
        "register_team_reverse_watch",
    ];
    for (name, body) in [
        (
            "on_window_event closure",
            lib_closure(&lib, "builder.on_window_event("),
        ),
        ("setup block", lib_closure(&lib, ".setup(")),
    ] {
        for needle in forbidden {
            assert!(
                !body.contains(needle),
                "{name} reaches `{needle}` — an automatic reverse trigger survived"
            );
        }
    }
}

/// X6: no production code selects the publishing
/// direction (`SyncMode::Reconcile`) any more — publishing is the engine's
/// `publish`, and the only operation that builds a publish is the explicit
/// reverse verb (`reverse_sync_worktree`). So `hydrate_worktree` (local or
/// remote) can never publish worktree edits.
#[test]
fn only_the_reverse_verb_selects_the_publishing_direction() {
    let mut files = Vec::new();
    for base in guard_support::source_roots() {
        rust_sources(&base, &mut files);
    }
    let mut violations = Vec::new();
    for file in files {
        let rel = guard_support::source_label(&file);
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with("tests.rs") || rel.contains("/tests/") {
            continue;
        }
        let src = strip_cfg_test(&strip_comments(&fs::read_to_string(&file).unwrap()));
        for (n, line) in src.lines().enumerate() {
            if line.contains("SyncMode::Reconcile") && !line.contains("== SyncMode::Reconcile") {
                violations.push(format!("{rel}:{}: {}", n + 1, line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "nothing may select the publishing direction:\n{}",
        violations.join("\n")
    );
    // The explicit publish is built only by the reverse verb.
    let handler = strip_comments(&read("src/mcp/handler.rs"));
    let builds: Vec<usize> = handler
        .match_indices("Op::Publish {")
        .map(|(at, _)| at)
        .collect();
    assert!(
        !builds.is_empty(),
        "scan found the reverse verb (non-vacuous)"
    );
    for at in builds {
        let enclosing = handler[..at]
            .rfind("fn ")
            .map(|f| {
                handler[f + 3..]
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .next()
                    .unwrap_or("")
                    .to_string()
            })
            .unwrap_or_default();
        assert_eq!(
            enclosing, "reverse_sync_worktree",
            "a publish built outside the reverse verb"
        );
    }
}
