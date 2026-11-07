// SPDX-License-Identifier: MIT
//! The native hook format Claude and Codex share: a hook's input is one JSON object that
//! names its event (`hook_event_name`), and most events mean the same in both.

use serde_json::Value;

use super::AgentCli;
use crate::activity::{Event, Report, Scope, REASONS};

/// The command the activity observer's hooks run: `helper` reporting for `cli`.
pub(super) fn observer_command(cli: AgentCli, helper: &str) -> String {
    format!("{} --activity-hook {cli}", crate::shell_quote(helper))
}

/// A native hook's input, its content still in it.
pub(super) struct HookInput(Value);

impl HookInput {
    pub(super) fn parse(input: &[u8]) -> Option<Self> {
        let raw: Value = serde_json::from_slice(input).ok()?;
        raw.get("hook_event_name")?.as_str()?;
        Some(Self(raw))
    }

    pub(super) fn name(&self) -> &str {
        self.field("hook_event_name").unwrap_or_default()
    }

    pub(super) fn field(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(Value::as_str)
    }

    pub(super) fn value(&self, key: &str) -> Option<&Value> {
        self.0.get(key)
    }

    /// The event its hook means in every CLI of this format, with the reason it carries.
    pub(super) fn shared_event(&self) -> Option<(Event, Option<String>)> {
        let event = match self.name() {
            "SessionStart" => Event::Started,
            "UserPromptSubmit" => Event::Submitted,
            "PreToolUse" => Event::ToolStarted,
            "PostToolUseFailure"
                if self.0.get("is_interrupt").and_then(Value::as_bool) == Some(true) =>
            {
                Event::Interrupted
            }
            "PostToolUse" | "PostToolUseFailure" => Event::ToolFinished,
            "PermissionRequest" => Event::Waiting,
            "Stop" => Event::Stopped,
            "StopFailure" => {
                let kind = self.field("error").or_else(|| self.field("error_type"));
                let reason = kind
                    .filter(|kind| REASONS.contains(kind))
                    .unwrap_or("unknown");
                return Some((Event::Failed, Some(reason.to_owned())));
            }
            "Interrupt" => Event::Interrupted,
            "SessionEnd" => Event::Ended,
            "SubagentStart" => Event::SubagentStarted,
            "SubagentStop" => Event::SubagentFinished,
            _ => return None,
        };
        Some((event, None))
    }

    /// The report `cli` makes with this hook as `event`, its content dropped. Its scope:
    /// a hook that names a subagent (`agent_id`) is the child's; any other is the main
    /// turn's when `main`, and unknown otherwise.
    pub(super) fn report(
        &self,
        cli: AgentCli,
        (event, reason): (Event, Option<String>),
        main: bool,
    ) -> Option<Report> {
        let opaque = |key: &str| self.field(key).map(str::to_owned);
        // An id a report can't carry still makes the hook the child's, unnamed.
        let agent = opaque("agent_id").filter(|id| crate::activity::opaque(id));
        let child = names_subagent(self.0.get("agent_id"));
        let scope = match (child, main) {
            (true, _) => Scope::Child,
            (false, true) => Scope::Main,
            (false, false) => Scope::Unknown,
        };
        let report = Report {
            provider: cli,
            event,
            scope,
            report_id: uuid::Uuid::new_v4().to_string(),
            provider_session: opaque("session_id"),
            turn: opaque("turn_id"),
            tool: opaque("tool_use_id"),
            tool_name: opaque("tool_name"),
            reason,
            agent,
            subagents: None,
        };
        report.valid().then_some(report)
    }
}

/// Appends `group(event)` to each of `events`' hook groups in a settings object, unless it
/// is there already: a user's own groups stay, and appending again changes nothing.
pub(super) fn append_settings_hooks(
    settings: &mut Value,
    events: &[&str],
    group: impl Fn(&str) -> Value,
) -> Result<(), &'static str> {
    let hooks = settings
        .as_object_mut()
        .ok_or("launch settings must be an object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("launch hooks must be an object")?;
    for &event in events {
        let owned = group(event);
        let groups = hooks
            .entry(event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or("launch hook groups must be arrays")?;
        if !groups.contains(&owned) {
            groups.push(owned);
        }
    }
    Ok(())
}

/// Whether a hook whose input holds `agent_id` is a subagent's: any value but `null`.
fn names_subagent(agent_id: Option<&Value>) -> bool {
    agent_id.is_some_and(|id| !id.is_null())
}

/// What an inbox check reads of its hook's input: the head of the prompt it carries (its
/// first `max_prompt` bytes, cut on a character boundary: where a wake key goes), and
/// whether it is known to be the agent's own hook, one that names no subagent (as its
/// report is scoped), and the CLI session it names (`session_id`). The input streams by,
/// read to its end so the CLI's write never blocks: what a check does not read (a tool
/// call's big result) is passed over, held nowhere. An input that is not one object, or
/// names a field twice, is not known to be the agent's.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CheckInput {
    /// Opaque provider receipts named by a native consumption-confirmation hook.
    pub receipts: Vec<String>,
    pub prompt: Option<String>,
    pub main: bool,
    pub session: Option<String>,
}

/// The fields of a hook's input a check reads.
#[derive(serde::Deserialize)]
struct CheckFields {
    #[serde(default)]
    agent_id: Option<Value>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    session_id: Option<Value>,
}

/// Reads `input` as [`CheckInput`] says.
pub fn read_check(input: &mut (impl std::io::Read + ?Sized), max_prompt: usize) -> CheckInput {
    use std::io::{self, BufRead};
    let mut reader = io::BufReader::new(input);
    // One object: the fields of anything else (an array) are no hook's.
    let object = loop {
        match reader.fill_buf() {
            Ok([first, ..]) if first.is_ascii_whitespace() => reader.consume(1),
            Ok([first, ..]) => break *first == b'{',
            Ok([]) => break false,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break false,
        }
    };
    let read = object
        .then(|| {
            let mut fields = serde_json::Deserializer::from_reader(&mut reader);
            serde::Deserialize::deserialize(&mut fields)
                .and_then(|read: CheckFields| fields.end().map(|()| read))
                .ok()
        })
        .flatten();
    let _ = io::copy(&mut reader, &mut io::sink());
    let Some(read) = read else {
        return CheckInput::default();
    };
    CheckInput {
        receipts: Vec::new(),
        main: !names_subagent(read.agent_id.as_ref()),
        session: match read.session_id {
            Some(Value::String(session)) => Some(session),
            _ => None,
        },
        prompt: read.prompt.map(|mut prompt| {
            let mut end = prompt.len().min(max_prompt);
            while !prompt.is_char_boundary(end) {
                end -= 1;
            }
            prompt.truncate(end);
            prompt
        }),
    }
}
