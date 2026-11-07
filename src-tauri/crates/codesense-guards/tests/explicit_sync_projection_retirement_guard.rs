// SPDX-License-Identifier: MIT
//! Guard that the team projection model's staleness and view-override
//! machinery is gone, durably:
//! - a registry test that the removed Tauri commands are unregistered;
//! - a symbol-anchored scan of `src/` (Rust) and `../src` (frontend) for the
//!   removed symbols, files and menu items.
//!
//! The left nav's install-tier composite overlay
//! (`resolve_composite_for_repo`) is gone too; the pivot reads the agent's want
//! set (`agent_context_preview`) instead.
//!
//! Std-only and source-driven, like the other `explicit_sync_*` guards: it can be
//! compiled on its own against any checkout via `CODESENSE_GUARD_ROOT`.

use codesense_guards::guard_support;
use codesense_guards::guard_support::{app_root as root, read};
use std::fs;
use std::path::{Path, PathBuf};

/// Strip `//` line comments (outside string literals), so prose that mentions a
/// removed symbol never trips a check.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let mut in_str = false;
        let mut prev = '\0';
        let mut cut = line.len();
        let mut byte_idx = 0;
        let chars: Vec<char> = line.chars().collect();
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

fn sources(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
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

/// Lines of `files` (comments stripped) that contain any of `needles`, skipping
/// files whose path contains one of `allow`.
fn hits(files: &[PathBuf], needles: &[&str], allow: &[&str], ts: bool) -> Vec<String> {
    let mut out = Vec::new();
    for file in files {
        let shown = file.display().to_string();
        if allow.iter().any(|a| shown.contains(a)) {
            continue;
        }
        let raw = fs::read_to_string(file).unwrap();
        let src = strip_comments(&raw);
        for (n, line) in src.lines().enumerate() {
            let t = line.trim_start();
            if ts && (t.starts_with('*') || t.starts_with("/*")) {
                continue;
            }
            for needle in needles {
                if line.contains(needle) {
                    out.push(format!("{shown}:{}: {}", n + 1, line.trim()));
                }
            }
        }
    }
    out
}

/// The view-override commands are not in the Tauri invoke registry.
#[test]
fn view_override_commands_are_unregistered() {
    let lib = strip_comments(&read("src/lib.rs"));
    let start = lib
        .find("generate_handler![")
        .expect("generate_handler! not found");
    let registry = &lib[start..start + lib[start..].find("])").expect("registry end")];
    for command in [
        "add_view_override",
        "remove_view_override",
        "load_view_overrides",
    ] {
        assert!(
            !registry.contains(&format!("::{command},")),
            "`{command}` is still a registered Tauri command"
        );
    }
}

/// No Rust code names the staleness filter, the view-override store or a
/// staleness threshold.
#[test]
fn rust_names_no_staleness_or_view_override_symbol() {
    let mut files = Vec::new();
    for base in guard_support::source_roots() {
        sources(&base, &["rs"], &mut files);
    }
    let needles = [
        "StalenessConfig",
        "stale_dropped_paths",
        "view-overrides.json",
        "ViewOverrides",
        "load_view_overrides",
        "staleness_threshold",
        "view_staleness_months",
        "compute_cutoff_unix",
        "is_epic_stale",
    ];
    let allow: [&str; 0] = [];
    let found = hits(&files, &needles, &allow, false);
    assert!(
        found.is_empty(),
        "staleness / view-override code remains:\n{}",
        found.join("\n")
    );
    assert!(
        guard_support::absent("domain/staleness.rs"),
        "domain/staleness.rs still exists"
    );
    assert!(
        guard_support::absent("install/staleness_config.rs"),
        "install/staleness_config.rs still exists"
    );
}

/// The frontend has no staleness hook, no view-override command and no
/// "Include in view" / "Remove from view" nav items.
#[test]
fn frontend_names_no_staleness_or_view_override_symbol() {
    let fe = root().join("../src");
    if !fe.is_dir() {
        return; // compiled against a bare src-tauri checkout
    }
    assert!(
        !fe.join("shared/hooks/use-staleness-overrides.ts").exists(),
        "use-staleness-overrides.ts still exists"
    );
    let mut files = Vec::new();
    sources(&fe, &["ts", "tsx"], &mut files);
    let needles = [
        "useStalenessOverrides",
        "add_view_override",
        "remove_view_override",
        "load_view_overrides",
        "Include in view",
        "Remove from view",
        "viewStalenessMonths",
        "stalenessThreshold",
    ];
    // The guard test itself and the schema migration that drops old keys.
    let allow = ["projection-retirement.guard", "viewer-schema-migrations"];
    let found = hits(&files, &needles, &allow, true);
    assert!(
        found.is_empty(),
        "frontend staleness / view-override code remains:\n{}",
        found.join("\n")
    );
}

/// The install-tier composite overlay command is gone — unregistered,
/// undefined in Rust, and never invoked or adapted by the frontend.
#[test]
fn composite_overlay_command_is_gone() {
    let lib = strip_comments(&read("src/lib.rs"));
    assert!(
        !lib.contains("resolve_composite_for_repo"),
        "`resolve_composite_for_repo` is still registered"
    );
    let mut files = Vec::new();
    for base in guard_support::source_roots() {
        sources(&base, &["rs"], &mut files);
    }
    let needles = [
        "fn resolve_composite_for_repo",
        "ResolveCompositeForRepoParams",
        "struct CompositeEntry",
    ];
    let found = hits(&files, &needles, &[], false);
    assert!(
        found.is_empty(),
        "composite overlay code remains:\n{}",
        found.join("\n")
    );
    let fe = root().join("../src");
    if !fe.is_dir() {
        return; // compiled against a bare src-tauri checkout
    }
    let mut files = Vec::new();
    sources(&fe, &["ts", "tsx"], &mut files);
    let needles = [
        "resolve_composite_for_repo",
        "resolveCompositeForRepo",
        "compositeToRepoScan",
        "useInstalledNavTree",
    ];
    let allow = ["projection-retirement.guard"];
    let found = hits(&files, &needles, &allow, true);
    assert!(
        found.is_empty(),
        "frontend composite overlay code remains:\n{}",
        found.join("\n")
    );
}

/// Every `.rs` file under `src/`, raw (comments included).
fn rust_sources() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for base in guard_support::source_roots() {
        sources(&base, &["rs"], &mut files);
    }
    files
}

/// `rg "AttachReconcile|enqueue_reconcile_verb|propagate_structural" src`
/// finds nothing — not in code, not in a comment.
#[test]
fn xs8_no_attach_reconcile_or_structural_fanout_anywhere() {
    let needles = [
        "AttachReconcile",
        "enqueue_reconcile_verb",
        "propagate_structural",
    ];
    let mut found = Vec::new();
    for file in rust_sources() {
        let text = fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            if needles.iter().any(|needle| line.contains(needle)) {
                found.push(format!("{}:{}: {}", file.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(found.is_empty(), "AC6:\n{}", found.join("\n"));
}

/// The legacy remote projection engine is
/// gone — the writer, its claim path, its MCP adapter and the remote legacy
/// lease — and nothing calls `remote_hydration::run` any more. (The local
/// legacy lease, `legacy_local` / `require_legacy_context`, is checked with the
/// local projection engine below.)
#[test]
fn xs8_legacy_remote_projection_engine_is_deleted() {
    for gone in [
        "src/mcp/remote_hydration.rs",
        "src/install/story_linker/remote_lifecycle.rs",
    ] {
        assert!(
            guard_support::absent(gone.strip_prefix("src/").unwrap()),
            "{gone} still exists"
        );
    }
    let files = rust_sources();
    let needles = [
        "materialize_remote_projections",
        "remote_hydration::",
        "remote_lifecycle::",
        "context_lifecycle::legacy(",
        "hydrate_story_selected",
        "enqueue_attach_reconcile",
        "remote_projection_passes",
    ];
    let found = hits(&files, &needles, &[], false);
    assert!(
        found.is_empty(),
        "legacy remote projection code remains:\n{}",
        found.join("\n")
    );
}

/// The explicit MCP tools run on the one preparation
/// engine (`context_operations::explicit`), `cancel_context_operation` is a
/// tool, and no tool reaches the old selected executor or a wave.
#[test]
fn xs8_explicit_tools_run_on_the_preparation_engine() {
    let handler = strip_comments(&read("src/mcp/handler.rs"));
    for tool in [
        "fn hydrate_worktree",
        "fn hydrate_story",
        "fn reverse_sync_worktree",
        "fn preview_reverse_sync_worktree",
        "fn list_hydrated",
    ] {
        let start = handler
            .find(tool)
            .unwrap_or_else(|| panic!("{tool} missing"));
        let body = &handler[start..start + handler[start..].find("\n    }\n").unwrap()];
        assert!(
            body.contains("context_operations::explicit::run")
                || body.contains("run(\n                self.app_handle.clone()"),
            "{tool} does not run on the engine"
        );
        for banned in ["selected_context::", "wave::", "story_linker::"] {
            assert!(!body.contains(banned), "{tool} reaches {banned}");
        }
    }
    assert!(
        handler.contains("async fn cancel_context_operation("),
        "cancel_context_operation is not a tool"
    );
    let explicit = strip_comments(&read("src/mcp/context_operations/explicit.rs"));
    for engine in [
        "step3::hydrate(",
        "publish::publish(",
        "publish::preview_for(",
    ] {
        assert!(
            explicit.contains(engine),
            "the explicit ops do not call {engine}"
        );
    }
}

/// The local legacy projection engine is
/// gone — whole-root synthesis (with its hard-coded six-month done-epic drop),
/// the manifest projection resolution and materializer, the claim latches, the
/// projection destination, and the local legacy lease — so nothing reads
/// `projections.yml` to deliver content.
#[test]
fn xs8b_legacy_local_projection_engine_is_deleted() {
    let files = rust_sources();
    let needles = [
        "resolve_manifest_projections",
        "synthesize_whole_root",
        "WholeRoot",
        "materialize_manifest_projections",
        "materialize_projection(",
        "ProjectionDestination",
        "legacy_local(",
        "require_legacy_context",
        "LegacyLease",
        "claim_now(",
        "run_worktree_projection_blocking",
        "apply_workspace_files(",
        // The done-epic drop, the reverse classifier, the pair write and the
        // interim merged-file writer.
        "LEGACY_DONE_EPIC_MONTHS",
        "legacy_done_epic_drop",
        "fn classify_reverse",
        "fn materialize_pair",
        "WORKSPACE_FILE_RECORDS_DIR",
        "preview_merge_marked_bytes",
    ];
    let found = hits(&files, &needles, &[], false);
    assert!(
        found.is_empty(),
        "legacy local projection code remains:\n{}",
        found.join("\n")
    );
    // The interim merged-file record is folded into the managed
    // record; nothing writes it any more: its dir is named only by the one
    // reader (and that reader's tests).
    let generated = hits(&files, &["report_workspace_files("], &[], false);
    assert!(generated.is_empty(), "{}", generated.join("\n"));
    let interim = hits(
        &files,
        &["generated-workspace-files"],
        &["agents/prepare/record.rs", "agents/product_bootstrap.rs"],
        false,
    );
    assert!(
        interim.is_empty(),
        "an interim writer remains:\n{}",
        interim.join("\n")
    );
}

/// The retired install tiers are deleted from
/// `LayerId` together with the install-tier resolver that walked them; old
/// records naming them load through a mapping (serde aliases + `WireLayerId`),
/// never through a live variant.
#[test]
fn xs8c_install_tier_layers_and_resolver_are_deleted() {
    assert!(
        guard_support::absent("content_engine/resolver.rs"),
        "the install-tier resolver still exists"
    );
    let files = rust_sources();
    let found = hits(
        &files,
        &[
            "LayerId::TeamRepo",
            "LayerId::TeamWorkspace",
            "resolve_all(",
            "content_engine::resolve(",
            "scratch_overlay_roots",
        ],
        &[],
        false,
    );
    // `WireLayerId::TeamRepo` is the load-time mapping itself, not a live tier.
    let found: Vec<&String> = found
        .iter()
        .filter(|hit| {
            let line = hit
                .replace("WireLayerId::TeamRepo", "")
                .replace("WireLayerId::TeamWorkspace", "");
            line.contains("LayerId::Team")
                || line.contains("resolve")
                || line.contains("scratch_overlay")
        })
        .collect();
    assert!(
        found.is_empty(),
        "retired install tiers remain:\n{found:#?}"
    );
    let layers = strip_comments(&read("src/content_engine/layers.rs"));
    let start = layers.find("pub enum LayerId {").expect("LayerId");
    let body = &layers[start..start + layers[start..].find('}').unwrap()];
    let variants: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| l.chars().next().is_some_and(char::is_uppercase))
        .collect();
    assert_eq!(variants, vec!["TeamProjection = 2,"], "{body}");
    let deser = layers
        .split("impl<'de> Deserialize<'de> for LayerId")
        .nth(1)
        .expect("LayerId's own Deserialize");
    assert!(
        deser.contains(r#""TeamProjection" | "TeamWorkspace" => Ok(LayerId::TeamProjection)"#),
        "an old TeamWorkspace must still load as team content"
    );
    assert!(
        deser.contains(r#""TeamRepo" => Err("#),
        "TeamRepo must be refused, never landing team-wide"
    );
}
