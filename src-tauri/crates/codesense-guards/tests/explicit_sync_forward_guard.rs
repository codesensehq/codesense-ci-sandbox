// SPDX-License-Identifier: MIT
//! Guard that no automatic forward-sync trigger survives.
//!
//! Every forward trigger the removal map lists (F1, F2, F4–F14) is checked at
//! the stimulus that still exists — the watcher handler, the team-activation
//! command, the attach command, the PTY sink, the story-move hook, the settings
//! and gesture commands — by reading that function's source and asserting it
//! reaches no wave enqueue or projection write. Each check fails on a tree
//! where these functions still enqueue a wave.
//!
//! Plus durable checks:
//! - an enum-variant test that the removed `WaveReason`s are gone;
//! - a registry test that the removed Tauri commands are unregistered;
//! - a symbol-anchored scan of `src/` for the removed symbols.
//!
//! Deliberately std-only and source-driven: it needs no app runtime, and it can
//! run via `cargo test -p codesense-guards` against any checkout by pointing
//! `CODESENSE_GUARD_ROOT` at that checkout's `src-tauri`. The transitive
//! call-graph guard (`explicit_sync_unattended_writer_guard.rs`) generalizes
//! these per-function checks.

use codesense_guards::guard_support;
use codesense_guards::guard_support::{app_root as root, read};
use std::fs;
use std::path::{Path, PathBuf};

/// Strip `//` line comments (outside string literals) so prose that merely
/// mentions a removed symbol never trips a check.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let mut in_str = false;
        let mut prev = '\0';
        let mut cut = line.len();
        let bytes: Vec<char> = line.chars().collect();
        let mut i = 0;
        let mut byte_idx = 0;
        while i < bytes.len() {
            let c = bytes[i];
            if c == '"' && prev != '\\' {
                in_str = !in_str;
            }
            if !in_str && c == '/' && i + 1 < bytes.len() && bytes[i + 1] == '/' {
                cut = byte_idx;
                break;
            }
            prev = c;
            byte_idx += c.len_utf8();
            i += 1;
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

/// The body of the first `fn <name>` (`fn <name>(` or `fn <name><`) in `src`,
/// comments stripped. Panics if the function is missing.
fn fn_body(src: &str, name: &str) -> String {
    fn_body_opt(src, name).unwrap_or_else(|| panic!("fn {name} not found"))
}

fn fn_body_opt(src: &str, name: &str) -> Option<String> {
    let src = strip_comments(src);
    let start = [format!("fn {name}("), format!("fn {name}<")]
        .iter()
        .filter_map(|needle| src.find(needle.as_str()))
        .min()?;
    braced_item(&src, start)
}

/// The item starting at byte `start` of the comment-stripped `src`, through the
/// `}` that closes its first `{` block.
fn braced_item(src: &str, start: usize) -> Option<String> {
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
                    return Some(src[start..open + offset + 1].to_string());
                }
            }
        }
        prev = c;
    }
    None
}

/// The wave entry points and projection writers no surviving stimulus may reach.
const FORWARD_WRITERS: &[&str] = &[
    "enqueue_content_wave_from_app",
    "enqueue_content_layer_event",
    "enqueue_content_projection",
    "enqueue_attach_reconcile",
    "enqueue_derive_wave",
    "enqueue_and_await_wave",
    "derive_and_sync_worktree",
    "derive_record",
    "rederive_worktrees_for_story",
    "build_worktree_derive_wave",
    "try_enqueue_team_switch_wave",
    "propagate_structural_dests",
    "materialize_projections_for_team",
    "materialize_manifest_projections",
    "Wave::new(",
    ".enqueue(",
    ".enqueue_with_trigger(",
];

fn assert_no_forward_writer(file: &str, func: &str, trigger: &str) {
    let body = fn_body(&read(file), func);
    for writer in FORWARD_WRITERS {
        assert!(
            !body.contains(writer),
            "{trigger}: `{func}` in {file} still reaches `{writer}` — a forward sync trigger survived"
        );
    }
}

fn assert_fn_absent(file: &str, func: &str, trigger: &str) {
    assert!(
        fn_body_opt(&read(file), func).is_none(),
        "{trigger}: `fn {func}` still exists in {file}"
    );
}

// ---- One check per forward trigger -------------------------------------------

#[test]
fn f1_content_watcher_enqueues_no_projection() {
    assert_no_forward_writer(
        "src/commands/watch.rs",
        "build_context_handler",
        "F1 content watcher",
    );
    assert_no_forward_writer(
        "src/commands/watch.rs",
        "dispatch_context_event",
        "F1 content watcher",
    );
    assert_fn_absent(
        "src/commands/watch.rs",
        "enqueue_content_projection",
        "F1 content watcher",
    );
}

#[test]
fn f2_team_config_watcher_is_gone() {
    assert_fn_absent(
        "src/commands/watch.rs",
        "build_team_config_handler",
        "F2 team-config watcher",
    );
    assert!(
        !strip_comments(&read("src/commands/watch.rs")).contains("DirRole::TeamConfig"),
        "F2: the team config/ watch role is still registered"
    );
}

#[test]
fn f3_f4_team_activation_and_switch_enqueue_no_wave() {
    assert_no_forward_writer(
        "src/commands/content_engine.rs",
        "set_active_team_for_wave",
        "F3/F4 team activation",
    );
    assert_fn_absent(
        "src/commands/install.rs",
        "switch_team_install",
        "F4 team switch",
    );
}

#[test]
fn f5_startup_wave_is_gone() {
    assert_fn_absent(
        "src/commands/content_engine.rs",
        "trigger_startup_wave",
        "F5 startup wave",
    );
}

#[test]
fn f6_workspace_open_enqueues_no_reconcile() {
    assert_no_forward_writer(
        "src/commands/watch.rs",
        "lazy_attach_workspace",
        "F6 open/focus attach",
    );
    assert_fn_absent(
        "src/content_engine/sync/wave.rs",
        "enqueue_attach_reconcile_from_app",
        "F6 open/focus attach",
    );
}

#[test]
fn f7_pty_attach_enqueues_no_reconcile() {
    // The attach / catch-up stimulus is `TerminalDomainEvent::WorktreeLive`: a
    // session going live in a worktree. The PTY sink must not answer it with an
    // `Auto` attach reconcile wave: every place pty_host.rs receives it
    // must stay inert.
    const FILE: &str = "src/commands/pty_host.rs";
    let src = strip_comments(&read(FILE));
    let no_writer = |body: &str, at: &str| {
        for writer in FORWARD_WRITERS.iter().chain(&["SyncInitiator::Auto"]) {
            assert!(
                !body.contains(writer),
                "F7: {at} in {FILE} still reaches `{writer}` — a PTY attach enqueues a reconcile"
            );
        }
    };

    // Local sink: the `TerminalEventSink` impl hands every local event to
    // `emit_local_received`, which is where a local WorktreeLive is answered.
    let sink_impl = src
        .find("TerminalEventSink for AppTerminalSink")
        .and_then(|i| braced_item(&src, i))
        .expect("F7: `impl TerminalEventSink for AppTerminalSink` not found");
    // The legacy fan-out it falls through to, and the remote (namespaced) sink.
    let received = fn_body(&src, "emit_local_received");
    let legacy = fn_body(&src, "emit_local_legacy");
    let remote = fn_body(&src, "emit_remote");
    assert!(
        sink_impl.contains("emit_local_received("),
        "F7: the AppTerminalSink `TerminalEventSink::emit` no longer delivers through `emit_local_received`; re-anchor the guard"
    );
    no_writer(&sink_impl, "the AppTerminalSink `TerminalEventSink::emit`");
    for (body, at) in [
        (&received, "`emit_local_received`"),
        (&legacy, "`emit_local_legacy`"),
        (&remote, "`emit_remote`"),
    ] {
        assert!(
            body.contains("TerminalDomainEvent::WorktreeLive"),
            "F7: {at} no longer handles WorktreeLive — the guard would pass vacuously; re-anchor it"
        );
        no_writer(body, at);
    }

    // And nothing else in the production half of pty_host.rs (attach,
    // subscribe, re-point, resume) reaches a wave either.
    let production = src
        .find("#[cfg(test)]\nmod tests")
        .map_or(src.as_str(), |i| &src[..i]);
    assert!(
        production.contains("fn attach_reserved("),
        "F7: the stream-attach path moved out of {FILE}; re-anchor the guard"
    );
    no_writer(production, "the production source");
}

#[test]
fn f8_f9_forward_cadence_and_overrides_are_gone() {
    for func in ["set_forward_sync_default", "set_forward_sync_overrides"] {
        assert_fn_absent("src/commands/watch.rs", func, "F8/F9 forward cadence");
    }
    // The whole cadence module (forward and reverse) and the reverse
    // overrides are gone, so the F12 override catch-up has no seat either.
    assert!(
        guard_support::absent("content_engine/sync/cadence.rs"),
        "F8/F9: sync/cadence.rs survives"
    );
    assert!(
        !strip_comments(&read("src/lib.rs")).contains("forward_cadence"),
        "F9: the focus/battery handlers still feed a forward cadence"
    );
    assert_fn_absent(
        "src/commands/watch.rs",
        "set_reverse_sync_overrides",
        "F12 override catch-up",
    );
}

#[test]
fn f10_story_move_writes_no_worktree() {
    assert_no_forward_writer(
        "src/commands/content_engine.rs",
        "handle_story_move_event",
        "F10 story move",
    );
    assert_fn_absent(
        "src/commands/content_engine.rs",
        "rederive_worktrees_for_story",
        "F10 story move",
    );
    assert_no_forward_writer(
        "src/commands/content_engine.rs",
        "relink_worktrees_for_story",
        "F10 story move",
    );
}

#[test]
fn f11_view_override_change_enqueues_no_wave() {
    // View overrides are retired end to end: the F11 stimulus itself is
    // gone, so no override change can reach a forward writer.
    for func in [
        "add_view_override",
        "remove_view_override",
        "load_view_overrides",
    ] {
        assert_fn_absent("src/commands/install.rs", func, "F11 view override");
    }
    assert_fn_absent(
        "src/commands/install.rs",
        "sync_staleness_after_override",
        "F11 view override",
    );
}

#[test]
fn f12_promote_enqueues_no_wave() {
    assert_no_forward_writer(
        "src/commands/content_engine.rs",
        "promote_content_entry",
        "F12 promote side effect",
    );
}

#[test]
fn f13_structural_gesture_does_not_fan_out() {
    assert_no_forward_writer(
        "src/commands/files.rs",
        "run_structural_gesture",
        "F13 structural fan-out",
    );
}

#[test]
fn f14_hydration_mode_is_gone() {
    assert!(
        guard_support::absent("content_engine/sync/hydration.rs"),
        "F14: sync/hydration.rs (hydrationMode) still exists"
    );
    assert!(
        !strip_comments(&read("src/mcp/state.rs")).contains("hydration::set_mode"),
        "F14: the viewer-state push still mirrors hydrationMode"
    );
}

#[test]
fn retry_worktree_sync_chain_and_branch_install_wave_are_gone() {
    let ce = "src/commands/content_engine.rs";
    for func in [
        "retry_worktree_sync",
        "derive_and_sync_worktree",
        "derive_record",
        "build_worktree_derive_wave",
        "actual_team_to_tear_down",
        "build_branch_install_wave",
    ] {
        assert_fn_absent(ce, func, "SPEC §12.4 removal");
    }
    assert_no_forward_writer(ce, "ensure_branch_workspace", "SPEC §12.4 removal");
}

/// Other surviving stimuli: workspace creation/registration and the launch /
/// in-session reconciles must not reach a forward writer either.
#[test]
fn workspace_creation_and_reconciles_enqueue_no_forward_wave() {
    let ce = "src/commands/content_engine.rs";
    for func in [
        "ensure_epic_workspace",
        "ensure_epic_workspace_inner",
        "establish_worktree_story_link",
        "establish_worktree_story_link_blocking",
        "register_story_workspace",
        "reconcile_worktrees_on_launch",
        "reconcile_stuck_worktrees",
        "reconcile_worktrees_with",
        "spawn_worktree_reconcile",
        "reconcile_epic_worktrees",
        "reconcile_epic_worktrees_core",
    ] {
        assert_no_forward_writer(ce, func, "workspace creation / reconcile");
    }
}

/// The app's setup loops and the window focus handler (`lib.rs`) reach no
/// forward writer. (The reverse sweeps they still drive are checked by
/// `explicit_sync_reverse_guard.rs`.)
#[test]
fn lib_setup_loops_and_focus_handler_enqueue_no_forward_wave() {
    let lib = strip_comments(&read("src/lib.rs"));
    for writer in FORWARD_WRITERS {
        assert!(
            !lib.contains(writer),
            "lib.rs (setup loops / focus handler) still reaches `{writer}`"
        );
    }
    let focus = lib
        .find("on_window_event(")
        .map(|i| &lib[i..])
        .expect("window event handler not found");
    let focus = &focus[..focus
        .find("WindowEvent::CloseRequested")
        .unwrap_or(focus.len().min(4000))];
    assert!(
        focus.contains("WindowEvent::Focused"),
        "the focus-handler slice does not contain the focus arm — vacuous"
    );
    for symbol in [
        "forward_cadence",
        "enqueue_attach_reconcile",
        "forward_sync",
    ] {
        assert!(
            !focus.contains(symbol),
            "the focus handler still drives `{symbol}`"
        );
    }
}

// ---- Durable checks -----------------------------------------------------------

/// The removed wave reasons are not variants of `WaveReason`.
#[test]
fn removed_wave_reasons_are_not_enum_variants() {
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
    for removed in [
        "Startup",
        "TeamSwitch",
        "ContentLayerEvent",
        "WorktreeDerive",
        "EpicInheritance",
    ] {
        assert!(
            !variants.contains(&removed),
            "WaveReason::{removed} is still a variant ({variants:?})"
        );
    }
    // The Agent-initiated reconcile is gone with the legacy verbs.
    assert!(
        !variants.contains(&"AttachReconcile"),
        "WaveReason::AttachReconcile is still a variant"
    );
}

/// The removed commands are not in the Tauri invoke registry.
#[test]
fn removed_commands_are_unregistered() {
    let lib = strip_comments(&read("src/lib.rs"));
    let start = lib
        .find("generate_handler![")
        .expect("generate_handler! not found");
    let registry = &lib[start..start + lib[start..].find("])").expect("registry end")];
    for command in [
        "switch_team_install",
        "trigger_startup_wave",
        "retry_worktree_sync",
        "set_forward_sync_default",
        "set_forward_sync_overrides",
        "get_out_of_date_surfaces",
    ] {
        assert!(
            !registry.contains(&format!("::{command},")),
            "`{command}` is still a registered Tauri command"
        );
    }
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

/// The symbol-anchored scan of `src/` — no code (comments excluded)
/// names a removed wave reason, the hydration mode, or a forward override.
/// `SyncCadence` is checked too: the reverse direction now names its own type.
#[test]
fn symbol_anchored_scan_finds_no_removed_symbol() {
    let mut files = Vec::new();
    for base in guard_support::source_roots() {
        rust_sources(&base, &mut files);
    }
    let needles = [
        "WaveReason::Startup",
        "WaveReason::TeamSwitch",
        "WaveReason::ContentLayerEvent",
        "WaveReason::WorktreeDerive",
        "WaveReason::EpicInheritance",
        "SyncCadence",
        "hydration_mode",
        "forward_sync_override",
    ];
    let mut hits = Vec::new();
    for file in files {
        let src = strip_comments(&fs::read_to_string(&file).unwrap());
        for (n, line) in src.lines().enumerate() {
            for needle in needles {
                if line.contains(needle) {
                    hits.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "removed symbols remain:\n{}",
        hits.join("\n")
    );
}

fn ts_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "node_modules") {
                continue;
            }
            ts_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "ts" || e == "tsx") {
            out.push(path);
        }
    }
}

/// Strip `//` line comments and `/* … */` / JSDoc lines (lines whose trimmed
/// start is `*`, `/*` or `/**`) from TypeScript, so history notes don't count.
fn strip_ts_comments(src: &str) -> String {
    strip_comments(src)
        .lines()
        .filter(|l| {
            let t = l.trim_start();
            !(t.starts_with('*') || t.starts_with("/*"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The frontend (`src/`) names no forward-only command, event, hook or
/// setting. The viewer-store migration that DROPS the old settings (and its
/// tests) is the only allowed mention.
#[test]
fn frontend_names_no_forward_only_symbol() {
    let fe = root().join("../src");
    if !fe.is_dir() {
        // `CODESENSE_GUARD_ROOT` pointed at a bare `src-tauri` export.
        return;
    }
    let mut files = Vec::new();
    ts_sources(&fe, &mut files);
    let allowed = |p: &Path| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        name.starts_with("viewer-schema-migrations") || name.ends_with(".migration.test.ts")
    };
    let needles = [
        "setForwardSyncDefault",
        "setForwardSyncOverrides",
        "forwardSyncDefault",
        "forwardSyncOverrides",
        "useForwardSync",
        "hydrationMode",
        "resolveHydrationMode",
        "autoPullEnabled",
        "triggerStartupWave",
        "switchTeamInstall",
        "retryWorktreeSync",
        "getOutOfDateSurfaces",
        "sync-surface-out-of-date",
        "manifest-changed",
        "useManifestWatchSync",
        "\"ContentLayerEvent\"",
        "\"WorktreeDerive\"",
        "\"EpicInheritance\"",
        "\"TeamSwitch\"",
    ];
    let mut hits = Vec::new();
    for file in files.into_iter().filter(|f| !allowed(f)) {
        let src = strip_ts_comments(&fs::read_to_string(&file).unwrap());
        for (n, line) in src.lines().enumerate() {
            for needle in needles {
                if line.contains(needle) {
                    hits.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "forward-only symbols remain in the frontend:\n{}",
        hits.join("\n")
    );
}
