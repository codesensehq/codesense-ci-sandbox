// SPDX-License-Identifier: MIT
//! A Claude session's usage, read from its own session log while this Mac's managed Claude
//! settings send its usage logs past CodeSense. Each call is keyed by the same request id and
//! native session id that Claude's `api_request` telemetry event carries, so a call read here and
//! the same call exported over OTLP are one usage row.
//!
//! Only ids, the timestamp, the working folder, the model and token counts are deserialized: a
//! log line's message text is never bound, stored or logged.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use codesense_pty_core::agent_cli::AgentCli;
use serde::Deserialize;

use super::event::{Tokens, UsageEvent};

/// The fields of one session-log line this reader uses. Serde skips every other field.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    request_id: Option<String>,
    session_id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    cache_creation: Option<CacheCreation>,
}

#[derive(Deserialize)]
struct CacheCreation {
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

/// The usage events, as `cli`'s, in a Claude-format session log at `path` and in its subagents'
/// logs beside it (`<session>/subagents/*.jsonl`), which Claude's telemetry counts under the same
/// session. The adapter whose log this is names `cli`.
pub(crate) fn read(cli: AgentCli, path: &Path) -> Vec<UsageEvent> {
    let mut files = vec![path.to_path_buf()];
    files.extend(subagent_logs(&path.with_extension("").join("subagents")));
    let mut calls = BTreeMap::new();
    let mut lines = 0usize;
    for file in &files {
        for_each_line(file, |line| {
            lines += 1;
            if let Some(event) = event_of(cli, line) {
                // Claude rewrites a call's usage as its blocks arrive: the last line is the total.
                calls.insert(event.event_id.clone(), event);
            }
        });
    }
    let found = calls.len();
    if found == 0 && lines > 0 {
        log::info!(target: "session_usage", "no usage found in {} session log lines across {} files", lines, files.len());
    }
    calls.into_values().collect()
}

/// The usage event of one log line: an assistant message with a request id, a session id, a
/// timestamp, a model and some tokens. Any other line, or one that does not parse, has none.
fn event_of(cli: AgentCli, line: &[u8]) -> Option<UsageEvent> {
    let line: Line = serde_json::from_slice(line).ok()?;
    if line.kind.as_deref() != Some("assistant") {
        return None;
    }
    let message = line.message?;
    let usage = message.usage?;
    let model = present(message.model).filter(|m| m != "<synthetic>")?;
    let tokens = Tokens {
        input: usage.input_tokens,
        output: usage.output_tokens,
        cache_read: usage.cache_read_input_tokens,
        cache_write: usage.cache_creation_input_tokens,
        cache_write_1h: usage
            .cache_creation
            .map_or(0, |c| c.ephemeral_1h_input_tokens)
            .min(usage.cache_creation_input_tokens),
    };
    // A call with no tokens is no usage.
    if tokens.input == 0 && tokens.output == 0 && tokens.cache_read == 0 && tokens.cache_write == 0
    {
        return None;
    }
    Some(UsageEvent {
        cli,
        event_id: present(line.request_id)?,
        session_id: present(line.session_id)?,
        at_ms: codesense_pty_core::agent_cli::output::rfc3339_ms(line.timestamp.as_deref()?)?,
        cwd: present(line.cwd),
        model,
        tokens,
        reported_cost_usd: None,
    })
}

/// `text`, unless it is empty: an empty id is no id, since two would share one event key.
fn present(text: Option<String>) -> Option<String> {
    text.filter(|t| !t.is_empty())
}

/// The regular `.jsonl` files in `dir`, when `dir` is a real directory (not a symlink).
fn subagent_logs(dir: &Path) -> Vec<PathBuf> {
    let real_dir = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    if !dir.parent().is_some_and(real_dir) || !real_dir(dir) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| dir.join(e.file_name()))
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    files
}

/// Each non-empty line of the regular file at `path`, in order; none when it is a symlink or not
/// a file.
fn for_each_line(path: &Path, mut each: impl FnMut(&[u8])) {
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    else {
        return;
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return;
    }
    std::io::BufReader::new(file)
        .split(b'\n')
        .map_while(Result::ok)
        .filter(|line| !line.is_empty())
        .for_each(|line| each(&line));
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// One assistant line of a Claude 2.1.296 session log, as written for one block of a call.
    pub(crate) fn assistant(request: &str, session: &str, model: &str, usage: &str) -> String {
        format!(
            r#"{{"type":"assistant","requestId":"{request}","sessionId":"{session}","timestamp":"2026-11-06T10:00:00.000Z","cwd":"/work/wt","isSidechain":false,"message":{{"id":"msg_{request}","role":"assistant","model":"{model}","content":[{{"type":"text","text":"working on it"}}],"usage":{usage}}}}}"#
        )
    }

    /// Text a person typed, which must never reach an event.
    const SECRET: &str = "the-words-a-person-typed";

    fn log(dir: &Path, lines: &[String]) -> PathBuf {
        let path = dir.join("session.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    #[test]
    fn each_call_is_one_event_keyed_like_its_telemetry_with_its_last_lines_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = log(
            dir.path(),
            &[
                format!(
                    r#"{{"type":"user","sessionId":"s","message":{{"role":"user","content":"use {SECRET}"}}}}"#
                ),
                assistant(
                    "req_1",
                    "s",
                    "claude-sonnet-5-5",
                    r#"{"input_tokens":12,"output_tokens":6,"cache_read_input_tokens":100,"cache_creation_input_tokens":40,"cache_creation":{"ephemeral_5m_input_tokens":10,"ephemeral_1h_input_tokens":30}}"#,
                ),
                assistant(
                    "req_1",
                    "s",
                    "claude-sonnet-5-5",
                    r#"{"input_tokens":12,"output_tokens":172,"cache_read_input_tokens":100,"cache_creation_input_tokens":40,"cache_creation":{"ephemeral_5m_input_tokens":10,"ephemeral_1h_input_tokens":30}}"#,
                ),
                assistant(
                    "req_2",
                    "s",
                    "claude-sonnet-5-5",
                    r#"{"input_tokens":5,"output_tokens":9,"cache_creation_input_tokens":3,"cache_creation":{"ephemeral_1h_input_tokens":99}}"#,
                ),
            ],
        );
        let events = read(AgentCli::Claude, &path);
        assert_eq!(events.len(), 2, "{events:?}");
        let first = &events[0];
        assert_eq!(
            (
                first.cli,
                first.event_id.as_str(),
                first.session_id.as_str()
            ),
            (AgentCli::Claude, "req_1", "s")
        );
        assert_eq!(first.model, "claude-sonnet-5-5");
        assert_eq!(first.cwd.as_deref(), Some("/work/wt"));
        assert_eq!(first.at_ms, 1_793_959_200_000);
        assert_eq!(first.reported_cost_usd, None, "priced from the rate table");
        assert_eq!(
            first.tokens,
            Tokens {
                input: 12,
                output: 172,
                cache_read: 100,
                cache_write: 40,
                cache_write_1h: 30
            },
            "the last line of a call holds its total"
        );
        assert_eq!(
            events[1].tokens.cache_write_1h, 3,
            "the one-hour part never exceeds the cache write"
        );
        assert!(!format!("{events:?}").contains(SECRET), "no text is kept");
    }

    #[test]
    fn lines_that_are_not_a_priced_call_give_no_event() {
        let dir = tempfile::tempdir().unwrap();
        let usage = r#"{"input_tokens":1,"output_tokens":1}"#;
        let path = log(
            dir.path(),
            &[
                assistant("req_synthetic", "s", "<synthetic>", usage),
                assistant("req_zero", "s", "claude-sonnet-5-5", r#"{"input_tokens":0,"output_tokens":0}"#),
                assistant("", "s", "claude-sonnet-5-5", usage),
                assistant("req_no_session", "", "claude-sonnet-5-5", usage),
                assistant("req_no_usage", "s", "claude-sonnet-5-5", "null"),
                r#"{"type":"assistant","requestId":"req_no_time","sessionId":"s","message":{"model":"claude-sonnet-5-5","usage":{"input_tokens":1}}}"#.to_owned(),
                r#"{"type":"result","requestId":"req_result","sessionId":"s","timestamp":"2026-11-06T10:00:00Z","message":{"model":"claude-sonnet-5-5","usage":{"input_tokens":1}}}"#.to_owned(),
                assistant("req_partial", "s", "claude-sonnet-5-5", usage)[..80].to_owned(),
            ],
        );
        assert_eq!(read(AgentCli::Claude, &path), Vec::new());
    }

    #[test]
    fn a_subagents_calls_count_and_a_symlinked_log_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let usage = r#"{"input_tokens":7,"output_tokens":2}"#;
        let path = log(
            dir.path(),
            &[assistant("req_main", "s", "claude-sonnet-5-5", usage)],
        );
        let subagents = path.with_extension("").join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let sidechain = assistant("req_sub", "s", "claude-sonnet-5-5", usage)
            .replace(r#""isSidechain":false"#, r#""isSidechain":true"#);
        std::fs::write(subagents.join("agent-a.jsonl"), sidechain).unwrap();
        let elsewhere = dir.path().join("elsewhere.jsonl");
        std::fs::write(
            &elsewhere,
            assistant("req_linked", "s", "claude-sonnet-5-5", usage),
        )
        .unwrap();
        std::os::unix::fs::symlink(&elsewhere, subagents.join("agent-b.jsonl")).unwrap();
        let ids: Vec<String> = read(AgentCli::Claude, &path)
            .into_iter()
            .map(|e| e.event_id)
            .collect();
        assert_eq!(ids, ["req_main", "req_sub"]);

        let linked = dir.path().join("linked.jsonl");
        std::os::unix::fs::symlink(&path, &linked).unwrap();
        assert_eq!(
            read(AgentCli::Claude, &linked),
            Vec::new(),
            "a symlinked session log is not followed"
        );
    }
}
