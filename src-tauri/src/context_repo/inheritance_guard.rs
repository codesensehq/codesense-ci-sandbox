// SPDX-License-Identifier: MIT
//! Guard the raw registry walk and the established own-team path boundaries.
use std::path::Path;

/// What the guard scans of `source`: Rust without its test code, other
/// languages whole (the Rust cut would misread their strings), minus comment lines.
fn production(file: &str, source: &str) -> String {
    let code = if file.ends_with(".rs") {
        crate::test_support::source_guard::production_text(source)
    } else {
        source.to_string()
    };
    code.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every guarded source file as (repo-relative path, raw text).
fn guarded_sources() -> Vec<(String, String)> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut sources = Vec::new();
    for root in [repo.join("src-tauri/src"), repo.join("src")] {
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|e| e == "rs" || e == "ts" || e == "tsx")
            {
                continue;
            }
            let file = path
                .strip_prefix(repo)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if crate::test_support::source_guard::is_test_file(path)
                || file.contains("/mock/")
                || file == "src-tauri/src/context_repo/inheritance_guard.rs"
            {
                continue;
            }
            sources.push((file, std::fs::read_to_string(path).unwrap()));
        }
    }
    sources
}

#[test]
fn inherited_reads_cannot_add_raw_chain_or_parent_directory_access() {
    let chain = regex::Regex::new(
        r"\b(?:team_chain|resolve_team_chain(?:_strict)?|ancestor_teams|teamChain)\s*\(",
    )
    .unwrap();
    let direct = direct_pattern();
    assert!(chain.is_match("org.team_chain(\"child\")"));
    assert!(direct.is_match("org_root.join(parent).join(\"content\")"));
    assert!(direct.is_match("layer.content_root(home, principal)"));
    let mut failures = Vec::new();
    for (file, raw) in guarded_sources() {
        let source = production(&file, &raw);
        let registry = file.starts_with("src-tauri/src/domain/")
            || file == "src-tauri/src/context_repo/inheritance.rs";
        let picker = matches!(
            file.as_str(),
            "src/shared/lib/team-chain.ts" | "src/features/org/components/team-parent-picker.tsx"
        );
        if !registry && !picker && chain.is_match(&source) {
            failures.push(format!("{file}: use team_view instead of a raw chain"));
        }
        if registry || picker {
            continue;
        }
        if let Some(why) = direct_violation(&file, &raw, &direct) {
            failures.push(why);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn direct_pattern() -> regex::Regex {
    regex::Regex::new(r#"\.parent_team\b|\bparent_team\b|\bpath_to_team_(?:content|panes)_dir\s*\(|\bteam_content_dir(?:_at)?\s*\(|\.content_root\s*\(|\borg_root\s*\.\s*join\s*\(|\.join\(\s*\"(?:parent|grand)(?:/|\")|\bContentRoot::own\s*\(|\.join\s*\([^;\n]*\)\s*\.join\s*\(\s*"(?:content|panes)"\s*\)"#).unwrap()
}

fn direct_violation(file: &str, source: &str, pattern: &regex::Regex) -> Option<String> {
    let mut source = production(file, source);
    for &(approved_file, expression, multiplicity) in EXACT_BOUNDARIES {
        if file != approved_file {
            continue;
        }
        if source.matches(expression).count() != multiplicity {
            return Some(format!(
                "{file}: approved direct expression changed; use team_view"
            ));
        }
        source = source.replace(expression, "");
    }
    let count = pattern.find_iter(&source).count();
    let allowed = OWN_TEAM_BOUNDARIES
        .iter()
        .find(|(f, _)| *f == file)
        .map_or(0, |(_, n)| *n);
    (count > allowed)
        .then(|| format!("{file}: {count} direct accesses (allowlisted {allowed}); use team_view"))
}

#[test]
fn dynamic_ancestor_root_reconstruction_fails_the_production_guard() {
    let pattern = direct_pattern();
    for root_name in ["root", "checkout", "context_directory"] {
        for read in [
            format!("std::fs::read_dir({root_name}.join(layer.team()).join(\"content\"))"),
            format!("ContentRoot::own({root_name}.join(layer.team()).join(\"content\"))"),
        ] {
            let snippet = format!("let {root_name} = path_to_org_root(home, principal, org)?; for layer in &team_view(&{root_name}, org, child).inherited {{ {read}; }}");
            assert!(
                direct_violation("src-tauri/src/new_consumer.rs", &snippet, &pattern).is_some(),
                "{snippet}"
            );
        }
    }
}

const EXACT_BOUNDARIES: &[(&str, &str, usize)] = &[(
    "src-tauri/src/commands/workspace_open/context.rs",
    r#"root.join(&scope.team).join("content")"#,
    1,
)];

#[test]
fn exact_boundaries_reject_extra_reads_and_same_file_swaps() {
    let pattern = direct_pattern();
    let bypass = r#"root.join(layer.team()).join("content")"#;
    for (file, approved, _) in EXACT_BOUNDARIES {
        assert!(direct_violation(file, approved, &pattern).is_none());
        assert!(direct_violation(file, bypass, &pattern).is_some());
        assert!(direct_violation(file, &format!("{approved};\n{bypass}"), &pattern).is_some());
        assert!(direct_violation(file, &format!("{approved};\n{approved}"), &pattern).is_some());
    }
}

// Existing registry editing, explicit team management, and own-team path
// construction. Counts are the file's production matches, exactly (test code
// is not scanned), so an additional call in an existing file is visible.
const OWN_TEAM_BOUNDARIES: &[(&str, usize)] = &[
    ("src-tauri/src/agents/manifest.rs", 1),
    ("src-tauri/src/agents/manifest_resolve.rs", 3),
    ("src-tauri/src/agents/manifest_resolve/context.rs", 2),
    ("src-tauri/src/agents/prepare/step3.rs", 1),
    ("src-tauri/src/agents/roots.rs", 4),
    ("src-tauri/src/avatars/store.rs", 2),
    ("src-tauri/src/commands/attachments.rs", 2),
    ("src-tauri/src/commands/config_editor.rs", 3),
    ("src-tauri/src/commands/dashboard_launch.rs", 1),
    ("src-tauri/src/commands/frontmatter_editor.rs", 1),
    ("src-tauri/src/commands/paths.rs", 1),
    ("src-tauri/src/commands/pipeline_scanner/inherited.rs", 1),
    ("src-tauri/src/commands/registry_local.rs", 1),
    ("src-tauri/src/commands/roles.rs", 1),
    ("src-tauri/src/commands/route_to_context.rs", 3),
    ("src-tauri/src/commands/scratch_space_view.rs", 1),
    ("src-tauri/src/commands/status_taxonomy.rs", 1),
    ("src-tauri/src/commands/story_create.rs", 1),
    ("src-tauri/src/commands/watch.rs", 4),
    ("src-tauri/src/commands/worktree.rs", 3),
    ("src-tauri/src/content_engine/layers.rs", 1),
    ("src-tauri/src/context_query/service.rs", 1),
    ("src-tauri/src/context_repo/content.rs", 2),
    ("src-tauri/src/context_repo/layout_resolver.rs", 3),
    ("src-tauri/src/context_repo/mutate.rs", 21),
    ("src-tauri/src/context_repo/path.rs", 17),
    ("src-tauri/src/context_repo/scaffold.rs", 2),
    ("src-tauri/src/context_repo/scratch_space.rs", 2),
    ("src-tauri/src/context_repo/team_rename.rs", 5),
    ("src-tauri/src/context_repo/workdir_write.rs", 1),
    ("src-tauri/src/http/routes/viewer.rs", 1),
    ("src-tauri/src/install/attachment_resolver.rs", 1),
    ("src-tauri/src/mcp/avatar_tools.rs", 1),
    ("src-tauri/src/mcp/handler.rs", 1),
    ("src-tauri/src/mcp/local_mcp_relay/mod.rs", 1),
    ("src-tauri/src/mcp/setup_tools.rs", 4),
    ("src-tauri/src/mcp/team_tools.rs", 3),
    ("src-tauri/src/panes/mod.rs", 3),
    ("src-tauri/src/records/binding.rs", 2),
    ("src-tauri/src/routines/agent_seams.rs", 3),
    ("src-tauri/src/routines/agents.rs", 1),
    ("src-tauri/src/routines/run_seams.rs", 1),
    ("src-tauri/src/routines/service.rs", 1),
    ("src-tauri/src/routines/target.rs", 2),
    ("src-tauri/src/routines/test_support.rs", 5),
    // The story's own team's review-rounds markers (SV-CDE-83 I2).
    (
        "src-tauri/src/session_usage/story_changes/review_rounds.rs",
        1,
    ),
    ("src/features/home-base/components/team-identity.tsx", 1),
    ("src/shared/lib/pipeline-graph/types.ts", 1),
    ("src/shared/lib/team-content-path.ts", 1),
    ("src/shared/stores/org-store.ts", 4),
    ("src/shared/types/domain.ts", 2),
    ("src/tauri/commands.ts", 1),
];

/// Allowances count production matches only, exactly: slack would let a new
/// production access hide inside what an old test fixture once used up.
#[test]
fn own_team_allowances_equal_the_production_matches() {
    let pattern = direct_pattern();
    let sources = guarded_sources();
    let mut mismatches = Vec::new();
    for (file, raw) in &sources {
        let count = pattern.find_iter(&production(file, raw)).count();
        let exact = EXACT_BOUNDARIES.iter().any(|(f, ..)| f == file);
        let allowed = OWN_TEAM_BOUNDARIES
            .iter()
            .find(|(f, _)| f == file)
            .map_or(0, |(_, n)| *n);
        let registry = file.starts_with("src-tauri/src/domain/")
            || file == "src-tauri/src/context_repo/inheritance.rs";
        if file.ends_with(".rs") && !exact && !registry && count != allowed {
            mismatches.push(format!("{file}: allowed {allowed}, production {count}"));
        }
    }
    for (file, allowed) in OWN_TEAM_BOUNDARIES {
        if !sources.iter().any(|(f, _)| f == file) {
            mismatches.push(format!("{file}: allowed {allowed}, but it is not scanned"));
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn one_more_production_access_in_an_allowlisted_file_fails() {
    let pattern = direct_pattern();
    let file = "src-tauri/src/commands/attachments.rs";
    let raw = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/attachments.rs"),
    )
    .unwrap();
    assert!(direct_violation(file, &raw, &pattern).is_none());
    let extra = "\nfn added() { let _ = std::fs::read_dir(root.join(team).join(\"content\")); }\n";
    assert!(direct_violation(file, &format!("{raw}{extra}"), &pattern).is_some());
    // A test fixture's access doesn't count toward the allowance either way.
    let fixture = "\n#[cfg(test)]\npub(crate) mod fixtures {\n    fn f() { let _ = root.join(team).join(\"content\"); }\n}\n";
    assert!(direct_violation(file, &format!("{raw}{fixture}"), &pattern).is_none());
}

/// The Rust test cut is for Rust: a TypeScript template literal that mentions
/// a Rust module marker must not hide the interpolated read after it.
#[test]
fn typescript_is_scanned_whole_and_keeps_template_interpolations() {
    let pattern = direct_pattern();
    let source = "export function label(team: Record<string, string>) {\n  return `#[cfg(test)]\nmod fixture { ${team.parent_team} }`;\n}\n";
    for file in ["src/new.ts", "src/new.tsx"] {
        assert!(
            production(file, source).contains("team.parent_team"),
            "{file}"
        );
        assert!(direct_violation(file, source, &pattern).is_some(), "{file}");
    }
}
