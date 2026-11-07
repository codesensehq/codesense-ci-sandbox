// SPDX-License-Identifier: MIT
//! Durable guards for the routines non-negotiables.
//!
//! The behaviour is proven by the seam tests (`routines::wake_seams`,
//! `routines::wake_seams::invariants`, `delivery::wakeup_tests`) and the
//! real-SSH and real-app suites. These checks pin the *shape* that behaviour
//! rests on, so a refactor cannot quietly route around it:
//!
//! 1. **Wake = wake the running instance, launch only if absent, never
//!    duplicate (R1).** Only `Pick::Absent` reaches a launch in `wake::fire`;
//!    dispatch launches for a wake only on `Fired::LaunchHere`; every launch
//!    primitive in the fire paths is in `routines_launch_sites_allowlist.txt`
//!    with its exact count (a new fire path that launches, such as the
//!    machine-armed one, fails until it is listed with its R1 argument).
//! 2. **Fresh = a new instance.** A fresh run never goes through `wake::fire`.
//! 3. **Timers die with their session and never launch (R6).** The session
//!    server ends a timer whose session is gone before it looks at whether it
//!    is due; nothing on the timer path launches anything.
//! 4. **Identical local/remote (R2).** One delivery path: the desktop
//!    never writes a wake into a PTY itself; timers do not branch on saved vs
//!    unnamed.
//! 5. **Unknown is never re-sent (R5).** Only a still-queued, reason-less run
//!    is redispatched; an `unknown` outcome is posted, never redispatched; the
//!    UI offers Retry only for `undelivered`.
//! 6. **Needs-you is never typed into.** The session server's idle rule
//!    requires no open ask, and only an idle head may go.
//! 7. **Routines live under agents.** `virtual://routines` only
//!    redirects to Agents → Schedule; nothing opens it as a tab.
//! 8. **The mode word is "start a new"** (a design decision).
//! 9. **Wake limits belong to routines and hold on the host.** No
//!    per-session lifetime cap; the session server checks the limits before
//!    it queues anything; every routine delivery (desktop-fired and
//!    machine-armed) carries the routine's limits.
//!
//! Std-only and source-driven, like the `explicit_sync_*` guards: point
//! `CODESENSE_GUARD_ROOT` at another checkout's `src-tauri` to run it against
//! that tree (that is how each check is shown failing on a broken variant).

use codesense_guards::guard_support;
use codesense_guards::guard_support::{app_root as root, read};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// The repo root (the frontend lives in `<repo>/src`).
fn repo() -> PathBuf {
    root()
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}

fn read_at(base: &Path, rel: &str) -> String {
    let path = base.join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Strip `/* … */` block comments and `//` line comments (outside string
/// literals), so prose never trips a check. Works for Rust and TypeScript.
fn strip_comments(src: &str) -> String {
    let mut src = src.to_string();
    while let Some(open) = src.find("/*") {
        // `"/*"` inside a string literal is left alone (rare; good enough).
        let line_start = src[..open].rfind('\n').map_or(0, |i| i + 1);
        if src[line_start..open].matches('"').count() % 2 == 1 {
            break;
        }
        let close = src[open..].find("*/").map_or(src.len(), |i| open + i + 2);
        let newlines = src[open..close].matches('\n').count();
        src.replace_range(open..close, &"\n".repeat(newlines));
    }
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let mut in_str: Option<char> = None;
        let mut prev = '\0';
        let mut cut = line.len();
        for (i, c) in line.char_indices() {
            match in_str {
                Some(q) if c == q && prev != '\\' => in_str = None,
                None if c == '"' || c == '`' => in_str = Some(c),
                None if c == '/' && line[i + 1..].starts_with('/') => {
                    cut = i;
                    break;
                }
                _ => {}
            }
            prev = c;
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

/// The item starting at byte `start` of `src`, through the `}` that closes its
/// first `{` block.
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

/// The body of `fn <name>` in `src` (comments stripped). Panics if missing.
fn fn_body(src: &str, name: &str) -> String {
    let src = strip_comments(src);
    let start = [format!("fn {name}("), format!("fn {name}<")]
        .iter()
        .filter_map(|needle| src.find(needle.as_str()))
        .min()
        .unwrap_or_else(|| panic!("fn {name} not found"));
    braced_item(&src, start).unwrap_or_else(|| panic!("fn {name} has no body"))
}

/// `src` with every `#[cfg(test)]` item removed (comments stripped first).
fn without_test_items(src: &str) -> String {
    let mut src = strip_comments(src);
    while let Some(at) = src.find("#[cfg(test)]") {
        let rest = &src[at..];
        // `#[cfg(test)] mod x;` (a file) or an item with a body.
        let semi = rest.find(';');
        let brace = rest.find('{');
        let end = match (semi, brace) {
            (Some(s), Some(b)) if s < b => at + s + 1,
            (_, Some(_)) => at + braced_item(&src, at).map_or(rest.len(), |item| item.len()),
            (Some(s), None) => at + s + 1,
            (None, None) => src.len(),
        };
        src.replace_range(at..end, "");
    }
    src
}

/// Whether `rel` is test code: never production, never guarded.
fn is_test_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == "tests.rs"
        || name == "test_support.rs"
        || name.ends_with("_tests.rs")
        || name.ends_with("_seams.rs")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || rel.contains("/wake_seams/")
        || rel.contains("/tests/")
        || rel.starts_with("src/test/")
}

/// Every production `.rs` file under `dir` (relative to `base`), sorted.
fn rust_files(base: &Path, dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = if base == root() && (dir == "src" || dir.starts_with("src/")) {
        guard_support::source_roots()
            .into_iter()
            .map(|source| source.join(dir.strip_prefix("src/").unwrap_or("")))
            .collect()
    } else {
        vec![base.join(dir)]
    };
    while let Some(at) = stack.pop() {
        let Ok(entries) = fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if !is_test_file(&rel) {
                    out.push(rel);
                }
            }
        }
    }
    out.sort();
    out
}

/// The name of the `fn` enclosing byte `at` of `src`.
fn enclosing_fn(src: &str, at: usize) -> String {
    let before = &src[..at];
    let Some(fn_at) = before.rfind("fn ") else {
        return "<module>".into();
    };
    before[fn_at + 3..]
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

// ───────────────────────────── 1. wake never duplicates ─────────────────────────────

const WAKE: &str = "src/routines/wake.rs";
const DISPATCH: &str = "src/routines/dispatch.rs";

/// In `wake::fire`, every arm of the resolution other than `Pick::Absent`
/// (a live session, an offline one, one starting, one quitting) is handled
/// before any launch is even named; `launch_absent` is reached once, from the
/// `Pick::Absent` arms.
#[test]
fn wake_fire_launches_only_when_nothing_is_live() {
    let body = fn_body(&read(WAKE), "fire");
    let resolve = body
        .find("match pick(")
        .expect("wake::fire resolves through `pick`");
    let arms = &body[resolve..];
    let absent = arms
        .find("Pick::Absent")
        .expect("wake::fire handles Pick::Absent");
    let live_arms = &arms[..absent];
    for live in [
        "Pick::Deliver",
        "Pick::Offline",
        "Pick::Starting",
        "Pick::Quitting",
    ] {
        assert!(
            live_arms.contains(live),
            "wake::fire must handle {live} before Pick::Absent (R1)"
        );
    }
    for launch in [
        "launch_absent",
        "LaunchHere",
        ".launch(",
        "remote_tab",
        "open_remote",
    ] {
        assert!(
            !live_arms.contains(launch),
            "wake::fire reaches `{launch}` from a live arm: a wake must never launch over a live instance (R1)"
        );
        assert!(
            !body[..resolve].contains(launch),
            "wake::fire reaches `{launch}` before resolving its candidates (R1)"
        );
    }
    assert_eq!(
        arms.matches("launch_absent(").count(),
        1,
        "wake::fire launches from exactly one place, the Pick::Absent arm"
    );
    // A launch already in flight (or just made) is waited for, never launched over.
    let absent_arms = &arms[absent..];
    assert!(
        absent_arms.contains("flights().begin(") && absent_arms.contains("Begin::Wait"),
        "a wake that finds nothing live must claim the single flight and wait on one in flight (§9.8)"
    );
    // An unreadable machine in the target may run the agent unseen: defer.
    assert!(
        arms.find("unseen(")
            .is_some_and(|u| u < arms.rfind("launch_absent(").unwrap()),
        "an unreadable machine in the target must defer before any launch (R1)"
    );
}

/// Only `launch_absent` hands a launch back to this Mac, and only for a
/// local (or `any`) target; `fire` never builds one itself.
#[test]
fn only_launch_absent_hands_a_launch_back() {
    let src = without_test_items(&read(WAKE));
    let mut sites = Vec::new();
    for (at, _) in src.match_indices("Fired::LaunchHere") {
        sites.push(enclosing_fn(&src, at));
    }
    assert_eq!(
        sites,
        ["launch_absent"],
        "Fired::LaunchHere is built only by launch_absent: {sites:?}"
    );
    let body = fn_body(&read(WAKE), "launch_absent");
    let local = body.find("Fired::LaunchHere").unwrap();
    assert!(
        body[..local].contains("TargetMachine::Machine("),
        "launch_absent hands a launch back only when the target is not another machine"
    );
}

/// Dispatch launches for a wake only when `wake::fire` said `LaunchHere`,
/// and a fresh run never goes through `wake::fire` (fresh = a new instance).
#[test]
fn dispatch_launches_a_wake_only_when_fire_says_so_and_fresh_never_wakes() {
    let src = read(DISPATCH);
    let wake = fn_body(&src, "wake");
    // The gate: `wake::fire(..) != Fired::LaunchHere { return; }`, before the
    // first call of `launch(` itself (never `relaunch(` or `launch_x(`).
    let gate = wake
        .find("!= Fired::LaunchHere")
        .expect("dispatch::wake gates on `!= Fired::LaunchHere`");
    let after_gate = &wake[gate..];
    let block = after_gate[..after_gate.find('}').expect("the gate's block")].to_string();
    assert!(
        block.contains("return"),
        "dispatch::wake must return when wake::fire did not say LaunchHere (R1)"
    );
    let calls = |text: &str| -> Vec<usize> {
        text.match_indices("launch(")
            .map(|(at, _)| at)
            .filter(|&at| {
                at == 0
                    || !text[..at]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
            .collect()
    };
    let first_launch = calls(&wake)
        .into_iter()
        .next()
        .expect("dispatch::wake can launch");
    assert!(
        gate < first_launch,
        "dispatch::wake calls launch( before its LaunchHere gate (R1)"
    );
    let execute = fn_body(&src, "execute");
    let fresh = execute
        .find("Prepared::Fresh(")
        .expect("dispatch::execute dispatches fresh runs");
    let fresh_arm_end = execute[fresh..]
        .find("Prepared::Wake(")
        .map_or(execute.len(), |i| fresh + i);
    assert!(
        !execute[fresh..fresh_arm_end].contains("wake"),
        "a fresh run must start a new instance, never go through a wake"
    );
    // `prepare` delegates the plan decision to `prepare_run`; the mode check lives there.
    let prepare = fn_body(&src, "prepare");
    assert!(
        prepare.contains("prepare_run("),
        "dispatch::prepare must delegate to prepare_run, where the wake decision is checked"
    );
    let prepare_run = fn_body(&src, "prepare_run");
    let decision = prepare_run
        .find("let wake =")
        .expect("dispatch::prepare_run decides `let wake = ..`");
    let decision_end = decision + prepare_run[decision..].find(';').expect("the decision's end");
    let decision_text = &prepare_run[decision..decision_end];
    assert!(
        decision_text.contains("effective_mode(") && decision_text.contains("== RoutineMode::Wake"),
        "only a routine whose effective mode is wake becomes a wake plan"
    );
    assert!(
        decision_text.contains("wake_prompt.is_none()"),
        "a wake's own relaunch (wake_prompt set) must not decide wake again"
    );
    let branch = prepare_run
        .find("if wake {")
        .expect("dispatch::prepare_run branches on `if wake`");
    let after_branch = &prepare_run[branch..];
    let branch_block = &after_branch[..after_branch.find('}').expect("the wake branch's block")];
    assert!(
        decision_end < branch && branch_block.contains("wake_plan("),
        "the wake plan is built only under the effective-mode decision"
    );
    let src_before_tests = src.split("#[cfg(test)]").next().unwrap();
    assert_eq!(
        src_before_tests.matches("Prepared::Wake(Box::new(").count(),
        1,
        "Prepared::Wake is built in one place"
    );
    assert!(
        fn_body(&src, "wake_plan").contains("Prepared::Wake(Box::new("),
        "wake_plan is the one builder of a wake plan"
    );
    // `fn wake_plan(` plus the one call, which sits in the `if wake` branch.
    assert_eq!(
        without_test_items(&src).matches("wake_plan(").count(),
        2,
        "wake_plan is called once, from the `if wake` branch of prepare_run"
    );
    assert!(
        branch_block.contains("wake_plan("),
        "the one wake_plan call sits inside the `if wake` branch"
    );
}

/// The launch primitives on every fire path, by `(file, fn, primitive)`, must
/// match `routines_launch_sites_allowlist.txt` exactly.
#[test]
fn every_launch_site_on_a_fire_path_is_allow_listed() {
    const PRIMITIVES: &[&str] = &[
        ".launch(",
        ".remote_tab(",
        "open_remote(",
        "launch_remote(",
        "launch_absent(",
        "SpawnIntent",
        "open_tab(",
        "Request::Spawn",
        // The app's launch entry points, named so a fire path that reaches them
        // directly (not through `Launcher`) is caught too.
        "launch_managed_dir_session(",
        "launch_headless_session(",
        "remote_shell_tab_bound(",
        "arm_launch(",
    ];
    let base = root();
    let mut files = rust_files(&base, "src/routines");
    files.extend(rust_files(&base, "src/delivery"));
    // The session server's wake/timer engine, and whatever arms routines on a
    // machine (its fire path lives under a `routine*` module).
    files.extend(
        rust_files(&base, "crates/codesense-session-server/src")
            .into_iter()
            .filter(|f| f.contains("session_wake") || f.contains("routine")),
    );
    let mut found: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for file in &files {
        let src = without_test_items(&read_at(&base, file));
        for primitive in PRIMITIVES {
            for (at, _) in src.match_indices(primitive) {
                // A definition is not a call.
                if src[..at].ends_with("fn ") || src[..at].ends_with("fn") {
                    continue;
                }
                *found
                    .entry((file.clone(), enclosing_fn(&src, at), primitive.to_string()))
                    .or_default() += 1;
            }
        }
    }
    let listed = allowlist();
    let mut problems = Vec::new();
    for (key, count) in &found {
        match listed.get(key) {
            None => problems.push(format!(
                "{} fn {} calls `{}` x{count}: a new launch on a fire path. Prove it keeps R1 (wake never duplicates) and add it to routines_launch_sites_allowlist.txt",
                key.0, key.1, key.2
            )),
            Some(n) if n != count => problems.push(format!(
                "{} fn {} calls `{}` x{count}, allow-listed x{n}",
                key.0, key.1, key.2
            )),
            Some(_) => {}
        }
    }
    for key in listed.keys() {
        if !found.contains_key(key) {
            problems.push(format!(
                "stale allow-list entry: {} fn {} `{}`",
                key.0, key.1, key.2
            ));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

fn allowlist() -> BTreeMap<(String, String, String), usize> {
    let text = read("tests/routines_launch_sites_allowlist.txt");
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap().trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(fields.len(), 4, "allow-list line: {line}");
        out.insert(
            (
                fields[0].to_string(),
                fields[1].to_string(),
                fields[2].to_string(),
            ),
            fields[3].parse().unwrap(),
        );
    }
    out
}

// ───────────────────────────── 3. timers ─────────────────────────────

const SESSION_WAKE: &str = "crates/codesense-session-server/src/session_wake/mod.rs";

/// A timer whose session is gone (exited, closed, another agent invocation)
/// ends `session_ended` before its due time is even read: it never fires
/// into a dead or replaced session.
#[test]
fn timers_end_with_their_session_before_they_can_fire() {
    let body = fn_body(&read(SESSION_WAKE), "fire_timers");
    let ended = body
        .find("UndeliveredReason::SessionEnded")
        .expect("fire_timers ends a timer whose session ended");
    let due = body.find("let due").expect("fire_timers reads due times");
    assert!(
        ended < due,
        "a timer whose session ended must be ended before its due time is read"
    );
    let check = &body[..ended];
    assert!(
        check.contains("!live") && check.contains("exit.is_some()"),
        "a timer ends when its session is gone or its agent exited"
    );
    assert!(
        body[ended..due].contains("continue"),
        "an ended timer is dropped, never kept"
    );
}

/// Timers only ever deliver into their own session (R6): nothing on the
/// timer path (desktop `delivery`, the session server's engine) names a
/// launch primitive. (The allow-list check above lists none for them.)
#[test]
fn the_timer_path_never_launches() {
    let base = root();
    let mut files = rust_files(&base, "src/delivery");
    files.extend(
        rust_files(&base, "crates/codesense-session-server/src")
            .into_iter()
            .filter(|f| f.contains("session_wake")),
    );
    assert!(!files.is_empty());
    for file in files {
        let src = without_test_items(&read_at(&base, &file));
        for primitive in [
            "Launcher",
            "launch_request_id",
            "remote_shell_tab",
            "SpawnIntent",
            "open_tab(",
            "Request::Spawn",
        ] {
            assert!(
                !src.contains(primitive),
                "{file} names `{primitive}`: a timer or delivery must never launch anything (R6)"
            );
        }
    }
}

// ───────────────────────────── 4. one path, local and remote ─────────────────────────────

/// The desktop never types a wake or a timer into a PTY itself: the
/// routine and delivery code never reach the desktop's PTY write commands.
/// Local and remote sessions take the same session-server path.
#[test]
fn the_desktop_never_types_a_wake_itself() {
    let base = root();
    let mut files = rust_files(&base, "src/routines");
    files.extend(rust_files(&base, "src/delivery"));
    for file in files {
        let src = without_test_items(&read_at(&base, &file));
        for writer in ["write_pty", "write_pty_key", "queue_write(", "pty_write"] {
            assert!(
                !src.contains(writer),
                "{file} reaches `{writer}`: a wake or timer must go through the session server, never a desktop PTY write (§9.6)"
            );
        }
    }
    // Remote and local are told apart only to find the host, never to pick a
    // different way to deliver.
    let delivery = without_test_items(&read("src/delivery/mod.rs"));
    assert!(
        delivery.contains("fn deliver(") && delivery.contains("resolve("),
        "delivery resolves the session's host and delivers through it"
    );
}

/// A routine finds its machine's workspace from the durable
/// preparation receipts, never from the viewer (the frontend's state is not
/// there for a closed workspace, or right after start). No routine path reads
/// the viewer state, and the app's remote launch and machine arm both choose
/// through the receipts and launch with the durable mark.
#[test]
fn a_routine_never_reads_the_viewer_for_its_workspace() {
    let base = root();
    for file in rust_files(&base, "src/routines") {
        let src = without_test_items(&read_at(&base, &file));
        for viewer in ["McpViewerState", "viewer_state", "\"workspaces\""] {
            assert!(
                !src.contains(viewer),
                "{file} reads `{viewer}`: a routine's workspace comes from the receipts (SW-AGR-12)"
            );
        }
    }
    for (file, function) in [
        ("src/routines/wake_app.rs", "remote_tab"),
        ("src/routines/machine_arm_app.rs", "place"),
    ] {
        let body = fn_body(&without_test_items(&read(file)), function);
        assert!(
            body.contains("durable_place("),
            "{file} {function} chooses the workspace from the receipts"
        );
    }
    let wake = fn_body(
        &without_test_items(&read("src/routines/wake_app.rs")),
        "remote_tab",
    );
    assert!(
        wake.contains("durable_workspace: true"),
        "a routine's remote launch carries the durable mark"
    );
}

/// Only routines launch durably. Outside
/// `src/routines` nothing writes the durable mark (any `durable_workspace:`
/// or `.durable_workspace =`), homes an armed tab, or resolves a workspace
/// without the viewer, but the allow-listed sites below.
#[test]
fn only_routines_launch_durably() {
    const ALLOWED: &[(&str, &str, &str)] = &[
        // The receipt gate of a launch whose record a routine marked durable.
        (
            "src/commands/pty_layout/remote_shell.rs",
            "resolve_prepared",
            "resolve_durable(",
        ),
        // Not a launch — the person's Publish/Discard from the
        // unsaved-team-edits list names a machine workspace that may not be
        // open in the viewer; its durable receipt admits it (owner, team,
        // repo, machine, server and the machine's confirmation all still hold).
        (
            "src/mcp/context_operations/explicit.rs",
            "bind_app",
            "resolve_durable(",
        ),
        // A routine's durable launch that a terminal server
        // update ended is relaunched by the person's confirmed update as it
        // was launched — the mark from the app-only `remoteDurable` tag, on a
        // saved Agent's tab in its prepared workspace, as `recover` reads it.
        // The receipt gate still checks it exactly.
        (
            "src/commands/machines/resume.rs",
            "resume_request",
            "durable_workspace:",
        ),
        // Recovery from the layout: the mark from the app-only
        // `remoteDurable` tag (the renderer cannot write it), on a saved
        // Agent's tab in its prepared workspace.
        (
            "src/commands/pty_layout/remote_shell.rs",
            "recover",
            "durable_workspace:",
        ),
    ];
    let base = root();
    let mut offenders = Vec::new();
    for file in rust_files(&base, "src") {
        if file.starts_with("src/routines/") {
            continue;
        }
        let src = without_test_items(&read_at(&base, &file));
        for pattern in [
            "durable_workspace:",
            ".durable_workspace =",
            "arm_tab_home(",
            "resolve_durable(",
        ] {
            for (at, _) in src.match_indices(pattern) {
                let defines = src[..at].ends_with("fn ")
                    || (pattern == "durable_workspace:" && src[..at].ends_with("pub "));
                let function = enclosing_fn(&src, at);
                if defines || ALLOWED.contains(&(file.as_str(), function.as_str(), pattern)) {
                    continue;
                }
                offenders.push(format!("{file} {function}: `{pattern}`"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "a durable launch is a routine's own (SW-AGR-12): {offenders:?}"
    );
}

/// Timers work for any interactive session, saved or unnamed: the
/// timer path never branches on a saved agent or a starter.
#[test]
fn timers_never_branch_on_saved_or_unnamed() {
    let src = without_test_items(&read("src/delivery/wakeup.rs"));
    for branch in ["builtin/", "is_starter", "AgentKey", "agent_key"] {
        assert!(
            !src.contains(branch),
            "delivery/wakeup.rs branches on `{branch}`: timers must work the same for saved and unnamed sessions (R2)"
        );
    }
}

// ───────────────────────────── 5. unknown is never re-sent ─────────────────────────────

/// Only a wake that is still queued and has no outcome is dispatched again;
/// an `unknown` outcome is recorded and posted, never redispatched.
#[test]
fn an_unknown_wake_is_never_sent_again() {
    let src = read(WAKE);
    let deferred = fn_body(&src, "deferred");
    assert!(
        deferred.contains("phase = 'queued'") && deferred.contains("reason IS NULL"),
        "only queued, reason-less wakes are redispatched after a restart"
    );
    let record = fn_body(&src, "record_status");
    let ended = record
        .find("UndeliveredReason::SessionEnded")
        .expect("record_status reopens a wake whose agent quit");
    let unknown = record
        .find("WakeRunStatus::Unknown")
        .expect("record_status handles unknown");
    let redispatches: Vec<usize> = record
        .match_indices("redispatch(")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        redispatches.len(),
        1,
        "record_status redispatches from one place"
    );
    assert!(
        ended < redispatches[0] && redispatches[0] < unknown,
        "the only redispatch in record_status is the session-ended reopen, never an unknown"
    );
    let reopen = fn_body(&src, "reopen");
    assert!(
        reopen.contains("delivered_at IS NULL") && reopen.contains("reason IS NULL"),
        "a reopen never touches a wake that may have been typed"
    );
    let ui = strip_comments(&read_at(&repo(), "src/features/routines/lib/run-status.ts"));
    let retry = ui
        .find("export function canRetry")
        .expect("run-status.ts exports canRetry");
    let retry = &ui[retry..ui[retry..].find("\n}").map_or(ui.len(), |e| retry + e)];
    assert!(
        retry.contains("run.phase !== \"undelivered\") return false"),
        "Retry is offered only for undelivered runs, never for unknown (R5)"
    );
}

// ───────────────────────────── 6. needs-you ─────────────────────────────

/// The session server types only into a head whose agent is idle with no
/// open ask (a permission prompt, `ask_user`, a needs-attention done).
#[test]
fn needs_you_is_never_typed_into() {
    let src = read(SESSION_WAKE);
    let verdict = fn_body(&src, "verdict");
    let idle = verdict.find("let idle").expect("verdict computes idle");
    let rule = &verdict[idle..idle + verdict[idle..].find(';').unwrap()];
    assert!(
        rule.contains("Activity::Idle | Activity::Done") && rule.contains("asks.is_empty()"),
        "idle means Idle/Done with no open ask: {rule}"
    );
    assert!(
        !rule.contains("NeedsPermission")
            && !rule.contains("Thinking")
            && !rule.contains("Working"),
        "a waiting permission prompt or a busy agent is never idle: {rule}"
    );
    let go = verdict.find("Verdict::Go").expect("verdict can go");
    assert!(
        verdict[..go].contains("!idle"),
        "verdict must refuse a head that is not idle before it may go"
    );
    let gather = fn_body(&src, "gather");
    assert!(
        gather.contains("Snapshot::takes_input"),
        "the process probe and input checks run only for idle sessions with no ask"
    );
    let takes_input = fn_body(
        &read("crates/codesense-host-proto/src/activity.rs"),
        "takes_input",
    );
    assert!(
        takes_input.contains("at_prompt()") && takes_input.contains("asks.is_empty()"),
        "a session takes input only at its prompt with no open ask"
    );
}

// ───────────────────────────── 7, 8. the UI ─────────────────────────────

/// There is no top-level Routines tab: `virtual://routines` resolves
/// only to the redirect, every entry point opens Agents → Schedule, and no
/// code opens the old tab.
#[test]
fn routines_live_under_agents_with_no_top_level_tab() {
    let repo = repo();
    let tab = strip_comments(&read_at(
        &repo,
        "src/features/routines/routines-virtual-tab.ts",
    ));
    assert!(
        tab.contains("registerVirtualTab(ROUTINES_VIRTUAL_PATH, \"Schedule\", RoutinesRedirect)"),
        "virtual://routines must resolve only to the redirect into Agents → Schedule"
    );
    let open = &tab[tab
        .find("export function openRoutines")
        .expect("openRoutines")..];
    let open = &open[..open.find("\n}").unwrap()];
    assert!(
        open.contains("switchView(\"schedule\")") && open.contains("openAgentsTab(opts)"),
        "openRoutines must land on Agents → Schedule"
    );
    let redirect = strip_comments(&read_at(
        &repo,
        "src/features/routines/components/routines-redirect.tsx",
    ));
    assert!(
        redirect.contains("openRoutines(") && !redirect.contains("RoutinesTab"),
        "the redirect opens Agents → Schedule and renders no Routines tab"
    );
    // Nothing else names the old path or renders the old tab.
    let mut offenders = Vec::new();
    for file in ts_files(&repo.join("src")) {
        let rel = file
            .strip_prefix(&repo)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if is_test_file(&rel) || rel.contains("/e2e") {
            continue;
        }
        let src = strip_comments(&fs::read_to_string(&file).unwrap());
        if src.contains("\"virtual://routines") && !rel.ends_with("routines-virtual-tab.ts") {
            offenders.push(format!("{rel}: names virtual://routines"));
        }
        if src.contains("<RoutinesTab") {
            offenders.push(format!("{rel}: renders <RoutinesTab>"));
        }
        if src.contains("openVirtualTab(ROUTINES_VIRTUAL_PATH")
            || src.contains("openTab(ROUTINES_VIRTUAL_PATH")
        {
            offenders.push(format!("{rel}: opens the Routines tab"));
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}

/// The fresh mode reads "start a new" everywhere a person sees it.
#[test]
fn the_fresh_mode_reads_start_a_new() {
    let repo = repo();
    let form = strip_comments(&read_at(
        &repo,
        "src/features/routines/components/routine-sentence-form.tsx",
    ));
    assert!(
        form.contains("<option value=\"fresh\">start a new</option>"),
        "the sentence form's fresh option reads \"start a new\""
    );
    let words = strip_comments(&read_at(&repo, "src/features/agents/lib/agent-routines.ts"));
    assert!(
        words.contains("mode === \"wake\" ? \"wake\" : \"start a new\""),
        "a read-only sentence says \"start a new\""
    );
    let mut offenders = Vec::new();
    for file in ts_files(&repo.join("src")) {
        let rel = file
            .strip_prefix(&repo)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if is_test_file(&rel) {
            continue;
        }
        let src = strip_comments(&fs::read_to_string(&file).unwrap()).to_lowercase();
        for retired in ["start a fresh", ">fresh<", "\"fresh\" : \"wake\""] {
            if src.contains(retired) {
                offenders.push(format!("{rel}: `{retired}`"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "the mode word is \"start a new\": {offenders:?}"
    );
}

fn ts_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(entries) = fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "ts" || e == "tsx") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

// 9. Limits belong to routines, are enforced where the session
// runs (so they hold remote and with the desktop closed), and there is no
// lifetime cap a long-lived session could run into.
#[test]
fn wake_limits_are_enforced_on_the_session_server_and_ride_on_every_routine_delivery() {
    let wire = strip_comments(&read("crates/codesense-pty-core/src/session_wake.rs"));
    assert!(
        !wire.contains("SESSION_DELIVERY_CAP"),
        "the per-session lifetime cap is gone (SW-AGR-13)"
    );
    let server = read("crates/codesense-session-server/src/session_wake/mod.rs");
    assert!(!strip_comments(&server).contains("SESSION_DELIVERY_CAP"));
    let deliver = fn_body(&server, "deliver");
    let checked = deliver
        .find("limit_verdict")
        .expect("deliver checks the limits");
    let queued = deliver.find("self.enqueue").expect("deliver queues");
    assert!(
        checked < queued,
        "limits are decided before anything is queued"
    );
    assert!(
        fn_body(&server, "fire_timers").contains("backstop"),
        "timer fires count against the backstop too (every source)"
    );
    let arm = read("crates/codesense-session-server/src/routine_arm/mod.rs");
    assert!(
        fn_body(&arm, "wake_into").contains("limits: arm.limits"),
        "a machine-armed wake carries its routine's limits"
    );
    let wake = read("src/routines/wake.rs");
    assert!(
        strip_comments(&wake).contains("limits: plan.limits"),
        "a desktop-fired wake carries its routine's limits"
    );
    assert!(
        fn_body(&read("src/routines/dispatch.rs"), "wake_plan").contains("wake_limits_of("),
        "a desktop wake plan takes its routine's limits"
    );
    assert!(
        fn_body(&read("src/routines/machine_arm.rs"), "desired_arm").contains("wake_limits_of("),
        "a machine arm takes its routine's limits"
    );
    assert!(
        fn_body(&read("src/routines/machine_arm.rs"), "sync_machine")
            .contains("enforces_wake_limits()"),
        "a limited routine is never armed where its limits would be dropped"
    );
    let delivery = read("src/delivery/mod.rs");
    assert!(
        fn_body(&delivery, "deliver").contains("limits: delivery.limits"),
        "the delivery core forwards the limits to the session server"
    );
}
