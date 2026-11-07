// SPDX-License-Identifier: MIT
//! Fast denial for a native hook already selected by its tool-name matcher.
use std::time::{Duration, Instant};

fn decision(input: &[u8]) -> bool {
    match serde_json::from_slice::<serde_json::Value>(input) {
        Ok(value) => match value
            .get("hook_event_name")
            .and_then(|event| event.as_str())
        {
            Some("pre_tool_call") | None => true,
            Some(_) => false,
        },
        Err(_) => true,
    }
}

/// A selected denial invocation fails closed on missing input. Readiness is bounded so
/// an accidentally inherited/open stdin cannot park a launch's permission hook.
// The adapter's CLI token is context, not a runtime parse or a permission decision:
// the native matcher already selected this denial invocation.
pub(super) fn run(_cli: Option<&std::ffi::OsStr>) -> i32 {
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut input = Vec::new();
    let mut complete = false;
    while Instant::now() < deadline && input.len() <= 1024 * 1024 {
        let mut poll = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        if unsafe { libc::poll(&mut poll, 1, remaining) } <= 0 {
            break;
        }
        let mut buffer = [0u8; 8192];
        let count = unsafe { libc::read(0, buffer.as_mut_ptr().cast(), buffer.len()) };
        if count == 0 {
            complete = true;
            break;
        }
        if count < 0 {
            break;
        }
        input.extend_from_slice(&buffer[..count as usize]);
    }
    if !complete || decision(&input) {
        println!(
            "{}",
            serde_json::json!({"decision": "block", "reason": "Tool excluded by this agent's permissions", "message": "Tool excluded by this agent's permissions"})
        );
        2
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hook_block_process_returns_json_exit_two_and_never_waits_for_stdin() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        const CHILD: &str = "CODESENSE_TEST_HOOK_BLOCK_CHILD";
        if std::env::var_os(CHILD).is_some() {
            std::process::exit(run(Some(std::ffi::OsStr::new("hermes"))));
        }
        for (input, expected) in [
            (b"{".as_slice(), 2),
            (br#"{"hook_event_name":"pre_tool_call"}"#, 2),
            (
                br#"{"hook_event_name":"pre_tool_call","tool_name":"memory"}"#,
                2,
            ),
            (
                br#"{"hook_event_name":"pre_tool_call","tool_name":"skill_manage"}"#,
                2,
            ),
            (
                br#"{"hook_event_name":"pre_tool_call","tool_name":"cronjob_manage"}"#,
                2,
            ),
            (br#"{"hook_event_name":"on_session_end"}"#, 0),
        ] {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "activity::hook_block::tests::hook_block_process_returns_json_exit_two_and_never_waits_for_stdin", "--nocapture"])
                .env(CHILD, "1").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
            child.stdin.take().unwrap().write_all(input).unwrap();
            let output = child.wait_with_output().unwrap();
            assert_eq!(output.status.code(), Some(expected));
            if expected == 2 {
                let text = String::from_utf8(output.stdout).unwrap();
                assert!(text
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .any(|value| value["decision"] == "block"));
            }
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "activity::hook_block::tests::hook_block_process_returns_json_exit_two_and_never_waits_for_stdin", "--nocapture"])
            .env(CHILD, "1").stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
        let open_stdin = child.stdin.take().unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(2));
        drop(open_stdin);
    }

    #[test]
    fn hook_block_selected_denial_fails_closed_and_unrelated_events_stay_open() {
        for input in [
            b"".as_slice(),
            b"{",
            b"{}",
            br#"{"hook_event_name":"pre_tool_call","tool_name":"mcp__codesense__excluded_read"}"#,
            br#"{"hook_event_name":"pre_tool_call","tool_name":"mcp__codesense__excluded_write"}"#,
        ] {
            assert!(decision(input));
        }
        assert!(!decision(br#"{"hook_event_name":"post_tool_call"}"#));
    }
}
