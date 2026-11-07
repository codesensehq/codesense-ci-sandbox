// SPDX-License-Identifier: MIT
//! PT-5: OSC 133 semantic-marker dispatch over the runtime's OSC side-tap.
//! PT-8: the warm terminal pool's membership policy. The PTY runtime itself
//! lives in the `codesense-pty-core` crate; `session_server` is the client of
//! the sidecar that hosts it out of process. This module's tests also guard the
//! Tauri-free runtime crates (`codesense-pty-core`, `codesense-fs-core`,
//! `codesense-host-proto`).

pub mod osc_dispatch;
pub mod session_server;
pub mod warm_pool;

pub use osc_dispatch::{Osc133Bus, Osc133Event, Osc133Kind};

#[cfg(test)]
mod tests {
    /// Strip `//` line comments so prose that *mentions* the forbidden tokens (the
    /// doc comments explaining the no-Tauri rule) is not scanned — only real code.
    fn contains_identifier(code: &str, name: &str) -> bool {
        code.split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|identifier| identifier == name)
    }

    #[test]
    fn boundary_identifier_check_rejects_old_names_without_rejecting_new_types() {
        assert!(contains_identifier(
            "let backend: &TerminalBackend;",
            "TerminalBackend"
        ));
        assert!(contains_identifier(
            "foo::TerminalBackend::Local",
            "TerminalBackend"
        ));
        assert!(!contains_identifier(
            "ResolvedTerminalBackend",
            "TerminalBackend"
        ));
        assert!(!contains_identifier(
            "TerminalBackendResolveError",
            "TerminalBackend"
        ));
        assert!(!contains_identifier(
            "TerminalBackendPolicy",
            "TerminalBackend"
        ));
    }

    fn strip_line_comments(src: &str) -> String {
        src.lines()
            .map(|line| match line.find("//") {
                Some(i) => &line[..i],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn is_tauri_coupled(source: &str) -> bool {
        // Non-test code only, so a test fixture that names Tauri (or this
        // scan's own needles) is not counted; then drop comments.
        let code = strip_line_comments(&crate::test_support::source_guard::production_text(source));
        code.contains("AppHandle") || code.contains("tauri") || code.contains("Tauri")
    }

    fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("source dir is readable") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn terminal_tree_and_runtime_crates_are_tauri_free() {
        // The whole `src-tauri/src/terminal/` tree AND the `codesense-pty-core`,
        // `codesense-fs-core` and `codesense-host-proto` crates are Tauri-free — the symmetric Rust
        // counterpart of the frontend seam gate. The burn-down baseline is empty:
        // no file under any of them may couple to Tauri. The Tauri command
        // adapters live under `commands/` (`pty.rs`, `pty_host.rs`, `files.rs`,
        // `worktree_watch.rs`) and are allowed to use Tauri.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let crates = [
            "codesense-pty-core",
            "codesense-fs-core",
            "codesense-host-proto",
        ]
        .map(|c| manifest.join("crates").join(c));
        let mut files = Vec::new();
        collect_rs(&manifest.join("src/terminal"), &mut files);
        for krate in &crates {
            collect_rs(&krate.join("src"), &mut files);
        }
        assert!(
            files.len() >= 20,
            "expected to scan the terminal tree and the runtime crates"
        );

        let mut coupled: Vec<String> = files
            .iter()
            .filter(|p| is_tauri_coupled(&std::fs::read_to_string(p).expect("readable")))
            .map(|p| p.display().to_string())
            .collect();
        coupled.sort();

        // A new Tauri coupling anywhere fails here with the offending file named —
        // the guard cannot pass vacuously because the file count above proves the
        // scan ran.
        assert!(
            coupled.is_empty(),
            "terminal module tree + runtime crates must be Tauri-free; found coupling in: {coupled:?}"
        );

        // The crates cannot even reach Tauri: no dependency names it.
        for krate in &crates {
            let crate_manifest =
                std::fs::read_to_string(krate.join("Cargo.toml")).expect("crate manifest");
            assert!(
                !crate_manifest.contains("tauri"),
                "{} must not depend on Tauri",
                krate.display()
            );
        }
    }

    #[test]
    fn worktree_file_logic_lives_only_in_fs_core() {
        // One worktree file implementation: listing, reading, the hidden/secret/VCS
        // filters, the root gate, the listing sort and the worktree watch are
        // DEFINED only in `codesense-fs-core`. The app's commands are thin
        // wrappers that inject a gate or an emit sink; no other crate or app
        // module may grow a second copy.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fs_core = manifest.join("crates/codesense-fs-core");
        let mut outside = Vec::new();
        collect_rs(&manifest.join("src"), &mut outside);
        for entry in std::fs::read_dir(manifest.join("crates")).expect("crates dir") {
            let dir = entry.expect("crate entry").path();
            if dir != fs_core && dir.join("src").is_dir() {
                collect_rs(&dir.join("src"), &mut outside);
            }
        }
        let mut inside = Vec::new();
        collect_rs(&fs_core.join("src"), &mut inside);

        // Needles split so this scan does not match itself.
        let definitions = [
            concat!("fn ", "list_dir("),
            concat!("fn ", "read_text("),
            concat!("fn ", "is_hidden_from_listing("),
            concat!("fn ", "is_ignored("),
            concat!("fn ", "is_ignored_subtree("),
            concat!("fn ", "is_secret_path("),
            concat!("fn ", "is_vcs_metadata_dir("),
            concat!("IGNORE_GLOBS", ": &["),
            concat!("SECRET_GLOBS", ": &["),
            concat!("fn ", "normalize_path_components("),
            concat!("fn ", "path_within_any_root("),
            concat!("fn ", "natural_cmp("),
            concat!("EXCLUDED_DIR_NAMES", ": &["),
            concat!("fn ", "has_excluded_segment("),
            concat!("fn ", "coalesce("),
            concat!("fn ", "classify_event("),
            concat!("fn ", "classify_git_event("),
            concat!("fn ", "resolve_git_watch_roots("),
            concat!("fn ", "is_git_ref_change("),
            concat!("fn ", "attach_git_ref_watch<"),
            concat!("fn ", "evict_expired("),
            concat!("fn ", "lock_watches("),
            concat!("WORKTREE_WATCH_SEQ", ": AtomicU64"),
            concat!("fn ", "build_globset("),
            concat!("trait ", "FsGate "),
            concat!("trait ", "FsService"),
            concat!("struct ", "FileEntry "),
            concat!("struct ", "WorktreeFileChange "),
            concat!("struct ", "WorktreeWatchState "),
        ];
        let code_of = |p: &std::path::PathBuf| {
            strip_line_comments(&std::fs::read_to_string(p).expect("readable"))
        };
        let inside_code: Vec<String> = inside.iter().map(code_of).collect();
        let outside_code: Vec<(String, String)> = outside
            .iter()
            .map(|p| (p.display().to_string(), code_of(p)))
            .collect();

        for needle in definitions {
            assert!(
                inside_code.iter().any(|code| code.contains(needle)),
                "fs-core must define `{needle}`"
            );
            let copies: Vec<&str> = outside_code
                .iter()
                .filter(|(_, code)| code.contains(needle))
                .map(|(path, _)| path.as_str())
                .collect();
            assert!(
                copies.is_empty(),
                "`{needle}` is defined outside codesense-fs-core: {copies:?}"
            );
        }

        // The worktree watch command module never touches `notify` itself: every
        // OS watch it registers goes through `codesense_fs_core::watch`.
        let wrapper = code_of(&manifest.join("src/commands/worktree_watch.rs"));
        assert!(
            !wrapper.contains(concat!("notify", "::")),
            "worktree_watch.rs must delegate the watch to codesense-fs-core"
        );
    }

    /// One boundary: no app code reads a terminal host's registry or layout
    /// directly. The session registry, its trait and the session runtime are
    /// pty-core internals, the old per-host enum is gone, and only the boundary
    /// module looks the in-process host up — every other read, the layout verbs
    /// included, goes through `commands::pty_backend`, which answers for both
    /// hosts.
    #[test]
    fn every_terminal_read_goes_through_the_one_boundary() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        collect_rs(&manifest.join("src"), &mut files);
        // Needles split so this scan does not match itself.
        let internals = [
            concat!("Pty", "State"),
            concat!("Terminal", "Registry"),
            concat!("PtySession", "Runtime"),
            concat!("Monotonic", "Clock"),
            concat!("Terminal", "Backend"),
            // A registry read through the host's (crate-private) field.
            concat!(".sessions", "."),
        ];
        // Any generic use of the host type: a state lookup (`state::<…>()`),
        // a `State<'_, …>` command parameter, an `Arc<…>` handle.
        let host_lookup = concat!("TerminalHost", ">");
        // The boundary module, and its test-only child module that builds
        // in-process hosts as fixtures for it.
        let allowed_lookups = [
            "src/commands/pty_backend.rs",
            "src/commands/pty_backend/rm4f_reservation_tests.rs",
        ];

        let mut offenders = Vec::new();
        for path in files
            .iter()
            .filter(|p| !crate::test_support::source_guard::is_test_file(p))
        {
            let source = std::fs::read_to_string(path).expect("readable");
            let code =
                strip_line_comments(&crate::test_support::source_guard::production_text(&source));
            let rel = path
                .strip_prefix(manifest)
                .unwrap_or(path)
                .display()
                .to_string();
            for needle in internals {
                let found = if needle.starts_with('.') {
                    code.contains(needle)
                } else {
                    // Reject the old identifier, not valid names containing it.
                    contains_identifier(&code, needle)
                };
                if found {
                    offenders.push(format!("{rel}: {needle}"));
                }
            }
            if code.contains(host_lookup) && !allowed_lookups.contains(&rel.as_str()) {
                offenders.push(format!("{rel}: reads the in-process host's state"));
            }
        }
        assert!(
            offenders.is_empty(),
            "terminal reads must go through commands::pty_backend: {offenders:?}"
        );
    }

    #[test]
    fn every_host_builds_its_runtime_through_the_one_constructor() {
        // The session registry is crate-private to construct; this pins that no
        // host (the app, the session server, any other crate) grows a parallel
        // construction path around `TerminalHost::new`.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        collect_rs(&manifest.join("src"), &mut files);
        for entry in std::fs::read_dir(manifest.join("crates")).expect("crates dir") {
            let dir = entry.expect("crate entry").path();
            if !dir.ends_with("codesense-pty-core") && dir.join("src").is_dir() {
                collect_rs(&dir.join("src"), &mut files);
            }
        }
        // Needles split so this scan does not match itself.
        let registry_new = concat!("PtyState", "::new(");
        let registry_literal = concat!("PtyState", " {");
        let host_new = concat!("TerminalHost", "::new(");
        // Whole files, tests included: a test fixture is a host too.
        let code_of = |p: &std::path::PathBuf| {
            strip_line_comments(&std::fs::read_to_string(p).expect("readable"))
        };

        let parallel: Vec<String> = files
            .iter()
            .filter(|p| {
                let code = code_of(p);
                code.contains(registry_new) || code.contains(registry_literal)
            })
            .map(|p| p.display().to_string())
            .collect();
        assert!(
            parallel.is_empty(),
            "a host built its session registry outside TerminalHost::new: {parallel:?}"
        );

        let builders: Vec<String> = files
            .iter()
            .filter(|p| code_of(p).contains(host_new))
            .filter_map(|p| p.strip_prefix(manifest).ok())
            .map(|p| p.display().to_string())
            .collect();
        assert!(
            builders.iter().any(|p| p == "src/lib.rs"),
            "the in-process host is built through TerminalHost::new; builders: {builders:?}"
        );
    }
}
