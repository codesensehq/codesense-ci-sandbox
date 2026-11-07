// SPDX-License-Identifier: MIT
//! Pins the routine runtime's boundary: routines, and the shared run plumbing
//! they stand on, must not reach back into the deleted pipeline.d automation
//! runtime (loops, watches, workflows, reactive automations and the Automation
//! Center's backing modules), the retired status gate, or the retired sprint
//! auto-run (`pipeline.yml` transitions). A module named here coming back is a
//! second automation system, or a gate on story status, creeping in beside
//! Routines.

use std::path::{Path, PathBuf};

/// Modules deleted with the pipeline.d automation runtime, the status gate and the
/// sprint auto-run.
const DELETED_MODULES: &[&str] = &[
    "accepted_event_log",
    "action_materializer",
    "action_resolve",
    "automation_audit_gate",
    "automation_authoring",
    "automation_authoring_pack",
    "automation_events",
    "automation_graph",
    "automation_readmodel",
    "automation_tools",
    "authz",
    "child_fanout",
    "default_validator",
    "effect_apply",
    "event_inbox",
    "evidence_stamp",
    "gate_rollup",
    "gate_state",
    "input_mapping",
    "loop_runner",
    "loop_scheduler",
    "orchestration_probe",
    "pack_automation_opt_in_store",
    "pack_entry_move",
    "pack_workflow_source",
    "predicate",
    "reactive_trigger",
    "redaction",
    "retry_admit",
    "run_config",
    "run_focus",
    "run_request",
    "RunRegistry",
    "slot_resolve",
    "sprint_scheduler",
    "status_move",
    "step_dag",
    "subject_provider",
    "transition_config",
    "trigger_simulation",
    "watch_authoring",
    "watch_readmodel",
    "watch_runner",
    "watch_scheduler",
    "workflow_authoring",
    "workflow_build",
    "workflow_executor",
    "workflow_revision",
    "workflow_revision_store",
];

/// The routine runtime and the neutral modules it runs on, relative to `src/`.
/// A directory covers every `.rs` file under it.
const GUARDED: &[&str] = &[
    "routines",
    "agents/session_events.rs",
    "coordinator",
    "commands/sprint_run/causation.rs",
    "commands/sprint_run/claim_loop.rs",
    "commands/sprint_run/loop_guard.rs",
    "commands/sprint_run/run_store.rs",
    "shared/panic_guard.rs",
];

fn rust_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() {
            rust_files(&entry.unwrap().path(), out);
        }
    } else if path.extension().is_some_and(|e| e == "rs") {
        out.push(path.to_path_buf());
    }
}

/// Whether `code` names `module` as a whole word.
fn names(code: &str, module: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    code.match_indices(module).any(|(at, _)| {
        let before = code[..at].chars().next_back();
        let after = code[at + module.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

/// `source` with comments and the contents of string and char literals
/// blanked out, newlines kept so line numbers still match. The scan runs over
/// the whole file, so a string spanning lines or a raw string holding quotes
/// neither hides the code after it nor exposes its text as code.
fn code_of(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut code = String::with_capacity(source.len());
    // Blank `chars[from..to]`, keeping only its newlines.
    let blank = |code: &mut String, from: usize, to: usize| {
        code.extend(chars[from..to].iter().filter(|c| **c == '\n'));
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            let end = chars[i..]
                .iter()
                .position(|c| *c == '\n')
                .map_or(chars.len(), |n| i + n);
            i = end;
        } else if c == '/' && next == Some('*') {
            let (mut depth, mut j) = (1, i + 2);
            while j < chars.len() && depth > 0 {
                match (chars[j], chars.get(j + 1).copied()) {
                    ('/', Some('*')) => (depth, j) = (depth + 1, j + 2),
                    ('*', Some('/')) => (depth, j) = (depth - 1, j + 2),
                    _ => j += 1,
                }
            }
            blank(&mut code, i, j);
            i = j;
        } else if c == '"' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '"' {
                j += if chars[j] == '\\' { 2 } else { 1 };
            }
            code.push('"');
            blank(&mut code, i + 1, j.min(chars.len()));
            code.push('"');
            i = j + 1;
        } else if c == '\'' {
            // A char literal (`'x'`, `'\n'`, `'"'`) is blanked; a lifetime is kept.
            let close = match next {
                Some('\\') => chars[i + 2..]
                    .iter()
                    .position(|c| *c == '\'')
                    .map(|n| i + 2 + n),
                Some(_) if chars.get(i + 2) == Some(&'\'') => Some(i + 2),
                _ => None,
            };
            match close {
                Some(end) => {
                    code.push_str("' '");
                    i = end + 1;
                }
                None => {
                    code.push(c);
                    i += 1;
                }
            }
        } else if is_ident(c) {
            let start = i;
            while i < chars.len() && is_ident(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let hashes = chars[i..].iter().take_while(|c| **c == '#').count();
            let raw =
                matches!(word.as_str(), "r" | "br" | "cr") && chars.get(i + hashes) == Some(&'"');
            if raw {
                let closing: Vec<char> = std::iter::once('"')
                    .chain(std::iter::repeat_n('#', hashes))
                    .collect();
                let body = i + hashes + 1;
                let end = (body..chars.len())
                    .find(|&j| chars[j..].starts_with(&closing))
                    .unwrap_or(chars.len());
                code.push('"');
                blank(&mut code, body, end);
                code.push('"');
                i = (end + closing.len()).min(chars.len());
            } else {
                code.push_str(&word);
            }
        } else {
            code.push(c);
            i += 1;
        }
    }
    code
}

/// Every `file:line: module` where guarded code (comments aside) names a
/// deleted module.
fn violations(src: &Path) -> Vec<String> {
    let mut files = Vec::new();
    for guarded in GUARDED {
        let path = src.join(guarded);
        assert!(
            path.exists(),
            "guarded path {guarded} is gone; update GUARDED"
        );
        rust_files(&path, &mut files);
    }
    let this_file = src.join("routines/boundary_guard.rs");
    let mut found = Vec::new();
    for file in files.into_iter().filter(|f| *f != this_file) {
        let code = code_of(&std::fs::read_to_string(&file).unwrap());
        for (n, line) in code.lines().enumerate() {
            for module in DELETED_MODULES {
                if names(line, module) {
                    let rel = file.strip_prefix(src).unwrap().display();
                    found.push(format!("{rel}:{}: {module}", n + 1));
                }
            }
        }
    }
    found
}

#[test]
fn routines_and_their_run_plumbing_never_reference_the_deleted_automation_runtime() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let found = violations(&src);
    assert!(found.is_empty(), "boundary crossed:\n{}", found.join("\n"));
}

#[test]
fn the_guard_catches_a_path_or_import_but_not_a_longer_identifier() {
    assert!(names(
        "use super::loop_runner::parse_duration;",
        "loop_runner"
    ));
    assert!(names("use super::{dag, watch_runner};", "watch_runner"));
    assert!(names(
        "crate::commands::reactive_trigger::x()",
        "reactive_trigger"
    ));
    assert!(!names(
        "let automation_events_seen = 1;",
        "automation_events"
    ));
    assert!(!names(
        "workflow_revision_store::open()",
        "workflow_revision"
    ));
}

#[test]
fn the_guard_reads_code_past_strings_and_comments_but_not_inside_them() {
    let source = concat!(
        "let a = \"spans \\\n a line\", loop_runner::f();\n",
        "let b = r#\"{\"k\":\"watch_runner\"}\"#; let q = '\"'; reactive_trigger::g();\n",
        "/* trigger_simulation */ fn h<'a>(x: &'a str) {}\n",
        "// workflow_build\n",
    );
    let code = code_of(source);
    let lines: Vec<&str> = code.lines().collect();
    assert_eq!(lines.len(), 5, "newlines are kept: {code:?}");
    assert!(names(lines[1], "loop_runner"), "{code:?}");
    assert!(names(lines[2], "reactive_trigger"), "{code:?}");
    assert!(!names(&code, "watch_runner"), "{code:?}");
    assert!(!names(&code, "trigger_simulation"), "{code:?}");
    assert!(!names(&code, "workflow_build"), "{code:?}");
    assert!(lines[3].contains("<'a>"), "{code:?}");
}
