// SPDX-License-Identifier: MIT
//! Shared receivers for the connection-bound inbox and completion tools.
use super::handler::{
    ok_json, CodesenseCallToolParams, ReactParams, ReadMessagesParams, ReplyParams,
    SendMessageParams, SignalDoneParams,
};
use crate::commands::agent_inbox::{self, Caller, InboxHost};
use rmcp::{model::CallToolResult, ErrorData};
use std::{future::Future, sync::Arc};

pub(crate) async fn send_message(
    host: &Arc<dyn InboxHost>,
    caller: Caller<'_>,
    params: SendMessageParams,
) -> Result<CallToolResult, ErrorData> {
    let result = agent_inbox::send_message_request(
        host,
        caller,
        &params.to,
        &params.text,
        params.attach_file.as_deref(),
        params.on_click,
        params.content_type,
        params.reply_to.as_deref(),
        params.summary.as_deref(),
        params.kind,
        params.request_id.as_deref(),
        params.media.as_ref(),
    )
    .await
    .map_err(|e| ErrorData::internal_error(e, None))?;
    ok_json(
        serde_json::to_value(result).map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
    )
}

pub(crate) async fn read_messages(
    host: &dyn InboxHost,
    caller: Caller<'_>,
    _params: ReadMessagesParams,
) -> Result<CallToolResult, ErrorData> {
    let messages = agent_inbox::read_messages(host, caller)
        .await
        .map_err(|e| ErrorData::internal_error(e, None))?;
    // Reactions are not mail: they ride alongside the messages and only when there are some.
    let reactions = agent_inbox::take_reaction_notices(host, caller)
        .await
        .map_err(|e| ErrorData::internal_error(e, None))?;
    if reactions.is_empty() {
        return ok_json(serde_json::json!({"messages":messages}));
    }
    ok_json(serde_json::json!({"messages":messages,"reactions":reactions}))
}

pub(crate) async fn react(
    host: &dyn InboxHost,
    caller: Caller<'_>,
    params: ReactParams,
) -> Result<CallToolResult, ErrorData> {
    let result = agent_inbox::react_request(
        host,
        caller,
        params.message_id,
        &params.emoji,
        params.remove,
    )
    .await
    .map_err(|e| ErrorData::internal_error(e, None))?;
    ok_json(
        serde_json::to_value(result).map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
    )
}

pub(crate) async fn reply(
    host: &Arc<dyn InboxHost>,
    caller: Caller<'_>,
    params: ReplyParams,
) -> Result<CallToolResult, ErrorData> {
    let result = agent_inbox::reply_request(
        host,
        caller,
        params.message_id,
        &params.text,
        params.summary.as_deref(),
        params.kind,
        None,
        params.request_id.as_deref(),
        params.media.as_ref(),
    )
    .await
    .map_err(|e| ErrorData::internal_error(e, None))?;
    ok_json(
        serde_json::to_value(result).map_err(|e| ErrorData::internal_error(e.to_string(), None))?,
    )
}

pub(crate) async fn signal_done<F, Fut>(
    connection: Option<String>,
    params: SignalDoneParams,
    finish: F,
) -> Result<CallToolResult, ErrorData>
where
    F: FnOnce(String, SignalDoneParams) -> Fut,
    Fut: Future<Output = Result<CallToolResult, ErrorData>>,
{
    let session =
        super::local_identity::session(params.session_id.as_deref(), connection.as_deref())?
            .to_string();
    if !matches!(
        params.status.trim(),
        "success" | "failed" | "needs-attention"
    ) {
        return Err(ErrorData::invalid_params(
            "status must be one of: success | failed | needs-attention",
            None,
        ));
    }
    finish(session, params).await
}

pub(crate) fn named_tool_request(
    params: CodesenseCallToolParams,
) -> Result<(String, serde_json::Map<String, serde_json::Value>), ErrorData> {
    let name = params.name.trim();
    if name.is_empty() {
        return Err(ErrorData::invalid_params("name must not be empty", None));
    }
    if name == "codesense_call_tool" {
        return Err(ErrorData::invalid_params(
            "codesense_call_tool cannot invoke itself",
            None,
        ));
    }
    Ok((name.to_string(), params.arguments.unwrap_or_default()))
}
