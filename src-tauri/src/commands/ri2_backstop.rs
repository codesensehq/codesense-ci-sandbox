// SPDX-License-Identifier: MIT
//! SLW-RI-2 — CI backstop for the blocking-offload guardrail.
//!
//! The `BlockingCtx` *type* witness (see [`super::async_helpers`]) makes it a
//! **compile error** to name the heavy blocking fns or acquire `SWITCH_LOCK`
//! from an un-offloaded async context (SW-XS-8 deleted the projection engine
//! fns this once named). But the type system cannot police the *escape
//! hatch*: `with_blocking` mints a witness inline and proves nothing by itself.
//! A careless new command could write `with_blocking(|ctx| heavy(ctx, …))`
//! **on a Tauri command / MCP / HTTP worker** and reintroduce the exact
//! UI-freeze SLW-RI-2 closes.
//!
//! This test is that backstop. It scans **all of `src/`** and fails if a
//! `with_blocking(` mint appears without a `// RI-2-ALLOW: <reason>` marker.
//! `with_blocking` is `pub` and callable from *any* module, and the type
//! system cannot police it — so the scan must be crate-wide, not limited to a
//! hand-listed set of dispatch dirs (round-2 review: enumerating "modules that
//! spawn onto `tauri::async_runtime`" — commands, mcp, http, menu, runtime,
//! terminal, … — is itself fragile; a new dispatch module would silently
//! escape). Scanning everything and requiring a marker at every mint is the
//! only future-proof surface.
//!
//! **File-level opt-out.** The content-engine sync modules (`wave.rs`,
//! `story_linker.rs`) mint `with_blocking` legitimately and often — always
//! inside a `spawn_blocking` / `thread::scope` closure or a test — because by
//! RI-1's plane isolation they run on the *engine* runtime, never the
//! command-dispatch workers. Rather than tag ~19 individual off-plane mints,
//! such a module declares a single `RI-2-ALLOW-FILE: <reason>` marker near its
//! top and the scanner skips it wholesale. This is a coarser, explicitly
//! reviewed grant scoped to the engine plane; command/mcp/http/etc. sites still
//! require a per-call marker.
//!
//! ## Design (round-1 hardening)
//!
//! An earlier version tried to *skip* `#[cfg(test)]` regions by brace-depth
//! tracking. Adversarial review found that unsound: `{`/`}` inside string/char
//! literals and comments miscount the depth (the skip region never closes → the
//! scanner silently no-ops over trailing production code), and a braceless
//! `#[cfg(test)]` item armed a spurious skip. A guardrail that can pass
//! *vacuously* is worse than none. So we DROPPED structural parsing entirely:
//! **every** token hit — test or production — must carry a marker (or live in a
//! file-opted-out engine module). A required one-line marker on a test is not a
//! false positive; it is an explicit, greppable acknowledgment.
//!
//! Known limits (the type witness is the primary guard; this is
//! belt-and-suspenders): a textual scan cannot catch a *deliberately* obfuscated
//! mint (aliased import `use … as wb; wb(…)`, the token split across newlines),
//! a mint hidden inside a string literal or trailing comment on a code line
//! (latent false-*positive*, not a miss), or a re-export of `with_blocking`
//! wrapped by another crate. The realistic threat — an *accidental* in-tree
//! reintroduction — is what this catches.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// The scan root: **all** crate source. `with_blocking` is `pub` and
    /// callable from any module, so nothing short of the whole tree is a
    /// complete surface for the escape hatch (round-2 review).
    fn scan_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    /// A module opts its whole file out of the per-call marker requirement by
    /// declaring this near its top — reserved for engine-plane modules whose
    /// every `with_blocking` is inside a `spawn_blocking`/`thread::scope`/test
    /// and therefore never on the command-dispatch workers.
    const FILE_OPT_OUT: &str = "RI-2-ALLOW-FILE";

    /// The one token that may only appear off the command plane: `with_blocking(`,
    /// the mint escape hatch.
    ///
    /// Why *only* this token — and why it is necessary-AND-sufficient on this
    /// surface: the heavy fns (`run_story_projection_plan`,
    /// `materialize_projections_for_team`) and `SWITCH_LOCK` are **type-gated** —
    /// each requires a `&BlockingCtx`, and the ONLY source of one in a
    /// command/mcp/http module is `with_blocking` (these modules have no `ctx`
    /// params; the witness cannot be forged). So a heavy fn is *unreachable* here
    /// without a `with_blocking` first, and policing the mint polices every path
    /// to the gated work — including the macro-expanded case (a macro that names
    /// a heavy fn still must obtain a witness, i.e. still expands a
    /// `with_blocking`). Scanning the heavy-fn names too would only double-flag
    /// the call *inside* a already-marked `with_blocking(|ctx| heavy(ctx))`
    /// closure, adding noise without adding coverage.
    const OFFLOAD_TOKENS: &[&str] = &["with_blocking("];

    const ALLOW_MARKER: &str = "RI-2-ALLOW";

    /// One token occurrence found by the scanner.
    struct Hit {
        file: PathBuf,
        line_no: usize,
        token: &'static str,
        allowed: bool,
    }

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        if !dir.exists() {
            return;
        }
        for entry in fs::read_dir(dir).expect("read scan dir") {
            let entry = entry.expect("dir entry");
            // Skip symlinks: no cycle/escape risk, and `file_type()` does not
            // follow the link (unlike `path.is_dir()`).
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

    fn is_comment(trimmed: &str) -> bool {
        trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*')
    }

    /// A `use`/`pub use` line names the symbols for import/re-export — not a
    /// call. Excluded so the re-export in `commands/mod.rs` isn't flagged.
    fn is_use(trimmed: &str) -> bool {
        trimmed.starts_with("use ") || trimmed.starts_with("pub use ")
    }

    /// Whitespace-insensitive token match: `with_blocking (` and `with_blocking(`
    /// both hit; `with_blocking<T>(` (the fn *definition*) and
    /// `{with_blocking, …}` (an import) do not. Defeats the "space before the
    /// paren" evasion without matching declarations.
    fn line_hits(line: &str) -> Vec<&'static &'static str> {
        let squished: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        OFFLOAD_TOKENS
            .iter()
            .filter(|t| squished.contains(**t))
            .collect()
    }

    /// Return the code portion of a line, dropping a trailing `//` comment — but
    /// only a `//` that is NOT inside a string literal, so `"https://…"; …` on a
    /// code line neither hides a real mint after it (round-3: the naive
    /// `split("//")` false-*negative*) nor lets a token mentioned in a trailing
    /// comment false-*positive* (round-2). Escaped quotes are treated crudely —
    /// good enough for this token scan.
    fn strip_line_comment(line: &str) -> &str {
        let bytes = line.as_bytes();
        let mut in_str = false;
        let mut i = 0;
        while i + 1 < bytes.len() {
            match bytes[i] {
                b'"' => in_str = !in_str,
                b'/' if !in_str && bytes[i + 1] == b'/' => return &line[..i],
                _ => {}
            }
            i += 1;
        }
        line
    }

    /// Allowed iff a `RI-2-ALLOW` marker sits on the hit line itself or anywhere
    /// in the contiguous run of comment/blank lines directly above it. Tolerates
    /// a blank line between the marker comment and the call (round-1 review: the
    /// old "contiguous `//` only" walk broke on a single blank line).
    fn has_allow_marker(lines: &[&str], i: usize) -> bool {
        if lines[i].contains(ALLOW_MARKER) {
            return true;
        }
        let mut j = i;
        while j > 0 {
            j -= 1;
            let t = lines[j].trim_start();
            if t.is_empty() || is_comment(t) {
                if lines[j].contains(ALLOW_MARKER) {
                    return true;
                }
            } else {
                break;
            }
        }
        false
    }

    /// Scan the whole dispatch surface, returning every token hit. Excludes this
    /// backstop file (it names the tokens as string literals).
    fn scan_surface() -> Vec<Hit> {
        let mut files = Vec::new();
        rust_files(&scan_root(), &mut files);
        let mut hits = Vec::new();
        for file in files {
            // This backstop names the token as a string literal; and the
            // engine-plane modules opt out wholesale via `FILE_OPT_OUT`.
            if file.file_name().and_then(|n| n.to_str()) == Some("ri2_backstop.rs") {
                continue;
            }
            let src = fs::read_to_string(&file).expect("read source");
            // Header-anchored opt-out: the marker must sit in the module header,
            // not anywhere in the body — so a future doc/string that merely
            // *mentions* the token can't silently disable the whole scan
            // (round-3 review).
            if src.lines().take(20).any(|l| l.contains(FILE_OPT_OUT)) {
                continue;
            }
            let lines: Vec<&str> = src.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let trimmed = line.trim_start();
                if is_comment(trimmed) || is_use(trimmed) {
                    continue;
                }
                for token in line_hits(strip_line_comment(line)) {
                    hits.push(Hit {
                        file: file.clone(),
                        line_no: i + 1,
                        token,
                        allowed: has_allow_marker(&lines, i),
                    });
                }
            }
        }
        hits
    }

    #[test]
    fn no_ungated_with_blocking_mint_in_crate() {
        let violations: Vec<String> = scan_surface()
            .into_iter()
            .filter(|h| !h.allowed)
            .map(|h| {
                format!(
                    "{}:{}: `{}` on the async dispatch surface without a `// {}: <reason>` marker",
                    h.file.display(),
                    h.line_no,
                    h.token,
                    ALLOW_MARKER,
                )
            })
            .collect();

        assert!(
            violations.is_empty(),
            "SLW-RI-2 backstop: heavy blocking-offload work reached the async dispatch \
             plane without an off-thread proof. Either move it inside a `spawn_blocking` \
             / engine blocking closure, or (if it is a justified brief acquire or a test) \
             annotate the site with a `// RI-2-ALLOW: <reason>` marker.\n{}",
            violations.join("\n"),
        );
    }

    /// Guards the guard: prove the scanner actually EXAMINES production code and
    /// its marker logic works — a silently-vacuous scan (wrong path, a skip bug)
    /// would make the main test pass for the wrong reason. Anchors on a real
    /// production mint (the establish chain's `with_blocking` in
    /// `commands/content_engine.rs`; SW-XS-3 removed `switch_team_install`, the
    /// former anchor): it must be detected AS a hit AND recognized as allow-marked.
    #[test]
    fn backstop_positively_detects_the_known_production_mint() {
        let hits = scan_surface();
        assert!(
            hits.len() >= 3,
            "expected the scanner to see the known offload sites, found {} hits — \
             the scan may be silently skipping the surface",
            hits.len()
        );
        let mint = hits.iter().find(|h| {
            h.file.file_name().and_then(|n| n.to_str()) == Some("content_engine.rs")
                && h.token == "with_blocking("
        });
        let mint = mint.expect(
            "scanner must detect the establish chain's production `with_blocking(` — \
             if this is None the scan reaches no production code (vacuous pass)",
        );
        assert!(
            mint.allowed,
            "the known production mint carries a RI-2-ALLOW marker — the marker logic \
             must recognize it (else the main test would false-positive)",
        );
    }

    /// SLW-RI-6 (AC-1): the worktree-OPEN command chain must offload through the
    /// blocking gateway too — RI-2 fenced the engine (`wave.run`), RI-6 extends
    /// the same discipline to `establish_worktree_story_link`, whose synchronous
    /// establish/derive body now runs inside `await_blocking` and mints its
    /// witness via `with_blocking`. Assert the scanner sees that mint in
    /// `content_engine.rs` AND that it is allow-marked, so a refactor that drops
    /// the offload (running the establish chain inline on a
    /// command worker — the exact N=10–20 open freeze) trips this backstop.
    #[test]
    fn backstop_covers_the_worktree_open_offload() {
        let hits = scan_surface();
        let establish_mint = hits.iter().find(|h| {
            h.file.file_name().and_then(|n| n.to_str()) == Some("content_engine.rs")
                && h.token == "with_blocking("
        });
        let establish_mint = establish_mint.expect(
            "scanner must detect the SLW-RI-6 open-chain `with_blocking(` in \
             commands/content_engine.rs — if None, the establish offload was removed and \
             the worktree-open chain runs inline on the command-dispatch plane (AC-1 regression)",
        );
        assert!(
            establish_mint.allowed,
            "the open-chain offload mint must carry a RI-2-ALLOW marker",
        );
    }

    /// Proves the guard has TEETH — the core round-1 review concern was a guard
    /// that could pass vacuously. Exercises the detection primitives directly on
    /// synthetic input so the "bites an unmarked mint / clears a marked one"
    /// contract is verified independent of the live tree.
    #[test]
    fn backstop_bites_unmarked_and_clears_marked() {
        // Unmarked call → detected as a hit, NOT allowed (a violation).
        let unmarked = ["fn f() {", "    with_blocking(|ctx| heavy(ctx));", "}"];
        assert!(
            !line_hits(unmarked[1]).is_empty(),
            "must detect the mint token"
        );
        assert!(
            !has_allow_marker(&unmarked, 1),
            "an unmarked mint must NOT be allowed — the guard would be toothless"
        );

        // Marker one line above, with a blank line between → allowed (round-1:
        // the old contiguous-`//` walk broke on the blank).
        let marked = [
            "    // RI-2-ALLOW: justified",
            "",
            "    with_blocking(|ctx| heavy(ctx));",
        ];
        assert!(
            has_allow_marker(&marked, 2),
            "a marker in the comment block above (blank line tolerated) must allow"
        );

        // Whitespace-before-paren evasion is still caught.
        assert!(
            !line_hits("    with_blocking (|ctx| x)").is_empty(),
            "`with_blocking (` (space before paren) must still be detected"
        );

        // The fn definition and the re-export must NOT be flagged.
        assert!(
            line_hits("pub fn with_blocking<T>(f: F) -> T {").is_empty(),
            "the `with_blocking<T>(` definition must not hit"
        );
        assert!(
            is_use("pub use async_helpers::{with_blocking, BlockingCtx};"),
            "the re-export line is a `use` and is excluded"
        );

        // Trailing-comment strip is quote-aware: a token merely MENTIONED in a
        // trailing comment is dropped (no false positive)...
        assert!(
            line_hits(strip_line_comment(
                "let n = 5; // note about with_blocking("
            ))
            .is_empty(),
            "a token in a trailing comment must not be flagged"
        );
        // ...but a `//` INSIDE a string does not hide a real mint after it.
        assert!(
            !line_hits(strip_line_comment(
                "let u = \"https://x\"; with_blocking(|c| y)"
            ))
            .is_empty(),
            "a mint after a string containing `//` must still be detected"
        );
    }

    /// SLW-FX-24 (AC-5): the two worktree-WATCH commands must offload their
    /// canonicalize + recursive kernel-watch registration onto the blocking pool
    /// via `await_blocking`, never run it inline on the async command-dispatch
    /// worker (the "freeze while the worktree sets up"). This mirrors
    /// `backstop_covers_the_worktree_open_offload`'s *intent* — but keys on the
    /// `await_blocking(<label>)` marker, NOT the `with_blocking(` scanner:
    /// `codesense_fs_core::watch::attach` is a bare `notify` watcher setup, not a type-gated heavy
    /// engine fn, so it mints no `BlockingCtx` witness and `OFFLOAD_TOKENS` never
    /// sees it. Adding `await_blocking(` to `OFFLOAD_TOKENS` is the wrong fix — 100+
    /// legitimate `await_blocking` call sites would false-positive and the guard's
    /// meaning would invert (`await_blocking` is the *safe* primitive). So this is
    /// a standalone, file-scoped structural assertion on the two commands' labels.
    #[test]
    fn backstop_covers_the_worktree_watch_offload() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/worktree_watch.rs");
        let src = fs::read_to_string(&path).expect("read worktree_watch.rs");
        // Scope to PRODUCTION code only — everything before the `#[cfg(test)]`
        // module. The sibling unit test legitimately calls
        // `await_blocking("lazy_attach_worktree_watch", …)`, so scanning the whole
        // file would let that test string satisfy the attach needle even if the
        // production command were re-inlined — a false-negative for exactly the
        // regression this guard exists to catch (review-caught).
        let production = src.split("#[cfg(test)]").next().unwrap_or(&src);
        // Drop full-line comments, then trailing comments via the module's
        // quote-aware `strip_line_comment` (so a `//`-mention can't satisfy it),
        // then squish whitespace so a future rustfmt arg-reflow that wraps
        // `await_blocking(` onto its own line still matches — mirroring `line_hits`'
        // whitespace insensitivity rather than a brittle raw `contains`.
        let squished: String = production
            .lines()
            .map(|l| l.trim_start())
            .filter(|t| !(t.starts_with("//") || t.starts_with("/*") || t.starts_with('*')))
            .map(strip_line_comment)
            .flat_map(str::chars)
            .filter(|c| !c.is_whitespace())
            .collect();
        for label in ["lazy_attach_worktree_watch", "detach_worktree_watch"] {
            let needle = format!("await_blocking(\"{label}\"");
            assert!(
                squished.contains(&needle),
                "SLW-FX-24 guardrail: `{label}` must offload its watch work via \
                 `{needle}, …)` in its PRODUCTION command body so canonicalize + the \
                 recursive kernel watch never run inline on the async command-dispatch \
                 worker (the worktree-setup UI freeze). If this fails, the production \
                 offload was removed or the label changed.",
            );
        }
    }

    /// SLW-FX-25 (AC-5): `lazy_attach_impl`'s slow path must NOT hold the global
    /// `state.watcher` guard across `register_directory_handlers` — that lock scope
    /// serialized N concurrent worktree creates on one `std::sync::Mutex` and froze
    /// the UI. It must instead route through `register_revalidating_watcher_lifecycle`,
    /// which registers against a watcher *clone* with the guard DROPPED **and**
    /// re-validates by `Arc` identity (`Arc::ptr_eq`) that the slot still holds that
    /// watcher before trusting the result. Both halves are load-bearing: dropping the
    /// guard without re-validation reintroduces the `start_watching` swap race (silent
    /// dispatch loss) — a bare "lock not held across register" check would PASS that
    /// unsafe variant, so this guard also witnesses the re-validate, scoped to the
    /// HELPER BODY (a file-global check would be satisfied vacuously by unrelated
    /// re-validate usages elsewhere — review r1 finding). Structural sibling to
    /// `backstop_covers_the_worktree_watch_offload`; the behavioral proof (guard
    /// dropped in phase-B, held in the phase-C fallback, re-register on real Arc swap)
    /// lives in watch.rs's `slwfx25_*` unit tests. A future refactor that re-inlines a
    /// `state.watcher.lock()` + `register_directory_handlers(` into the slow path
    /// (restoring the freeze), or drops the `Arc::ptr_eq` re-validate from the helper
    /// (restoring the race), trips this test.
    #[test]
    fn backstop_covers_the_lazy_attach_watcher_lock_scope() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/watch.rs");
        let src = fs::read_to_string(&path).expect("read watch.rs");
        // Scope to PRODUCTION code (before `#[cfg(test)]`) — the `slwfx25_*` unit
        // tests legitimately name the helper, so scanning the whole file would let a
        // test mention satisfy the needle even if the production slow path were
        // re-inlined (the same false-negative the sibling guard was hardened against).
        let production = src.split("#[cfg(test)]").next().unwrap_or(&src);
        let squished: String = production
            .lines()
            .map(|l| l.trim_start())
            .filter(|t| !(t.starts_with("//") || t.starts_with("/*") || t.starts_with('*')))
            .map(strip_line_comment)
            .flat_map(str::chars)
            .filter(|c| !c.is_whitespace())
            .collect();

        // (a) The swap-safe helper exists.
        assert!(
            squished.contains("fnregister_revalidating_watcher_lifecycle"),
            "SLW-FX-25 guardrail: the swap-safe helper `register_revalidating_watcher_lifecycle` \
             must exist — it registers against a watcher clone with `state.watcher` DROPPED so \
             concurrent worktree creates don't serialize on the global mutex (the batch-create freeze).",
        );

        // (b) The helper RE-VALIDATES by Arc identity — scoped to the helper body
        // (between its `fn` and the following `fnlazy_attach_impl`) so a stray
        // `Arc::ptr_eq` elsewhere in the file can't satisfy this vacuously.
        let helper_body = squished
            .split_once("fnregister_revalidating_watcher_lifecycle")
            .and_then(|(_, rest)| rest.split_once("fnlazy_attach_impl"))
            .map(|(body, _)| body)
            .unwrap_or("");
        assert!(
            helper_body.contains("Arc::ptr_eq"),
            "SLW-FX-25 guardrail: `register_revalidating_watcher_lifecycle` must RE-VALIDATE by Arc \
             identity (`Arc::ptr_eq`) that the slot still holds the watcher it registered against. \
             A `watcher_lifecycle_seq`-only check is UNSOUND — `start_watching` bumps the seq before \
             it acquires the lock and swaps the slot, so a snapshot can see the bumped seq with the \
             old watcher. Dropping this re-validate reintroduces the swap race (silent dispatch loss).",
        );

        // (c) The slow path ROUTES THROUGH the helper rather than re-inlining a
        // locked `register_directory_handlers` call (which would restore the freeze).
        // Scoped to the `lazy_attach_impl` body (between its `fn` and the following
        // `fnlazy_detach_workspace`) so a stray call from some *other* function can't
        // satisfy this while the slow path silently reverts to an inline lock.
        let lazy_attach_body = squished
            .split_once("fnlazy_attach_impl")
            .and_then(|(_, rest)| rest.split_once("fnlazy_detach_workspace"))
            .map(|(body, _)| body)
            .unwrap_or("");
        assert!(
            lazy_attach_body.contains("register_revalidating_watcher_lifecycle(&state,"),
            "SLW-FX-25 guardrail: `lazy_attach_impl`'s slow path must call \
             `register_revalidating_watcher_lifecycle(&state, …)` rather than holding \
             `state.watcher` across `register_directory_handlers` directly (which restores the \
             concurrent-create UI freeze).",
        );
    }
}
