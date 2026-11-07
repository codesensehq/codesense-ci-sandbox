// SPDX-License-Identifier: MIT
//! Hermes shell hooks use the shared event envelope with lifecycle fields in `extra`.
use super::*;
use crate::activity::{Event, Scope};

pub(super) fn normalize(input: &[u8], qualified: bool) -> Option<Report> {
    let hook = hooks::HookInput::parse(input)?;
    let event = match hook.name() {
        "on_session_start" => Event::Started,
        "pre_llm_call" => Event::Submitted,
        "pre_tool_call" => Event::ToolStarted,
        "post_tool_call" | "post_approval_response" => Event::ToolFinished,
        "pre_approval_request" => Event::Waiting,
        "subagent_start" => Event::SubagentStarted,
        "subagent_stop" => Event::SubagentFinished,
        "on_session_end" => Event::Stopped,
        _ => return None,
    };
    let extra = hook.value("extra")?;
    let field = |key: &str| {
        extra
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let mut report = hook.report(AgentCli::Hermes, (event, None), qualified)?;
    // Native turn keys repeat session/task IDs and exceed the shared 128-byte
    // identifier budget. Preserve equality without widening the host contract.
    report.turn = field("turn_id")
        .or_else(|| field("parent_turn_id"))
        .map(|id| {
            if id.len() > 128 {
                format!("hermes-turn-{}", blake3::hash(id.as_bytes()).to_hex())
            } else {
                id
            }
        });
    report.tool = field("tool_call_id");
    let child_event = matches!(event, Event::SubagentStarted | Event::SubagentFinished);
    if child_event {
        // Start supplies child_subagent_id but stop does not: child_session_id is
        // the stable identity present in both real payloads.
        report.agent = field("child_session_id");
        report.scope = Scope::Child;
    } else if extra.get("platform").and_then(serde_json::Value::as_str) == Some("subagent") {
        report.agent = report.provider_session.clone();
        report.scope = Scope::Child;
    }
    if event == Event::Waiting {
        let key =
            serde_json::to_vec(&(&report.provider_session, &report.turn, &report.tool)).ok()?;
        report.report_id = format!("hermes-approval-{}", blake3::hash(&key).to_hex());
    }
    report.valid().then_some(report)
}

/// Read only native identity and receipt fields; message bodies are skipped by serde.
pub(super) fn read_check(input: &mut dyn std::io::Read) -> CheckInput {
    #[derive(serde::Deserialize)]
    struct Message {
        role: Option<String>,
        platform_message_id: Option<String>,
    }
    #[derive(Default, serde::Deserialize)]
    struct Extra {
        platform: Option<String>,
        #[serde(default)]
        conversation_history: Vec<Message>,
    }
    #[derive(serde::Deserialize)]
    struct Input {
        hook_event_name: String,
        session_id: String,
        #[serde(default)]
        extra: Extra,
    }
    let Ok(raw) = serde_json::from_reader::<_, Input>(&mut *input) else {
        let _ = std::io::copy(input, &mut std::io::sink());
        return CheckInput::default();
    };
    let main = raw.extra.platform.as_deref() != Some("subagent");
    let receipts = if main && raw.hook_event_name == "pre_llm_call" {
        raw.extra
            .conversation_history
            .into_iter()
            .rev()
            .filter(|message| message.role.as_deref() == Some("user"))
            .filter_map(|message| message.platform_message_id)
            .filter(|id| crate::activity::opaque(id))
            .take(8)
            .collect()
    } else {
        Vec::new()
    };
    CheckInput {
        prompt: None,
        main,
        session: Some(raw.session_id),
        receipts,
    }
}

/// The final hooks fold owns scoped block resolution, observers and inbox checks.
pub(super) fn fold_hooks(
    settings: &mut serde_json::Value,
    helper: &str,
    commands: impl IntoIterator<Item = (&'static str, String)>,
) -> Result<(), &'static str> {
    let root = settings.as_object_mut().ok_or("invalid Hermes settings")?;
    root.insert("hooks_auto_accept".into(), serde_json::json!(true));
    let hooks = root
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or("invalid Hermes hooks")?;
    // Preserve 108's final resolution of native scoped blocks at the packaged helper.
    let block_argument = format!("--hook-block {}", AgentCli::Hermes.as_str());
    if let Some(entries) = hooks.get_mut("pre_tool_call") {
        for entry in entries.as_array_mut().ok_or("invalid Hermes hook event")? {
            if entry["command"] == block_argument.as_str() {
                entry["command"] =
                    serde_json::json!(format!("{} {block_argument}", crate::shell_quote(helper)));
            }
        }
    }
    for (event, command) in commands {
        let entries = hooks
            .entry(event)
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or("invalid Hermes hook event")?;
        // Upgrade our owned entry once; retain scoped block and unrelated user hooks.
        entries.retain(|entry| entry["command"].as_str() != Some(command.as_str()));
        entries.push(serde_json::json!({"command":command,"timeout":2,"fail_closed":false}));
    }
    Ok(())
}
