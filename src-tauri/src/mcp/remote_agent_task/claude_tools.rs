// SPDX-License-Identifier: MIT
//! Exact-workspace operations supplement the same bound CodeSense handler.
use super::claude_executor::Client;
use crate::mcp::{handler::CodesenseHandler, remote_authority::RemoteAuthority};
use rmcp::{model::*, service::RequestContext, ErrorData, RoleServer, ServerHandler};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
#[derive(Clone)]
pub(super) struct Handler {
    pub base: CodesenseHandler,
    pub authority: RemoteAuthority,
    pub client: Arc<Client>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Exec {
    command: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
    path: String,
    text: String,
}
impl ServerHandler for Handler {
    fn get_info(&self) -> ServerInfo {
        self.base.get_info()
    }
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut result = self.base.list_tools(request, context).await?;
        result.tools.extend(tools()?);
        Ok(result)
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if !matches!(
            request.name.as_ref(),
            "remote_exec" | "remote_read" | "remote_write"
        ) {
            return self.base.call_tool(request, context).await;
        }
        let handler = self.clone();
        crate::mcp::dispatch::call(request, move |request| async move {
            handler.authority.validate().await?;
            let result = invoke(
                &handler.client,
                request.name.as_ref(),
                request.arguments.unwrap_or_default(),
            )
            .await?;
            handler.authority.validate().await?;
            Ok(CallToolResult::success(vec![ContentBlock::text(
                result.to_string(),
            )]))
        })
        .await
    }
}

pub(super) fn tools() -> Result<Vec<Tool>, ErrorData> {
    let mut result = ListToolsResult {
        tools: Vec::new(),
        meta: None,
        next_cursor: None,
    };
    for (name, description, properties, required) in [
        (
            "remote_exec",
            "Run a bounded command in this managed remote workspace",
            json!({"command":{"type":"string"}}),
            json!(["command"]),
        ),
        (
            "remote_read",
            "Read a UTF-8 file in this managed remote workspace (256KiB maximum)",
            json!({"path":{"type":"string"}}),
            json!(["path"]),
        ),
        (
            "remote_write",
            "Write a UTF-8 file in this managed remote workspace (256KiB maximum)",
            json!({"path":{"type":"string"},"text":{"type":"string"}}),
            json!(["path", "text"]),
        ),
    ] {
        result.tools.push(serde_json::from_value(json!({"name":name,"description":description,"inputSchema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).map_err(|_|ErrorData::internal_error("task schema unavailable",None))?);
    }
    Ok(result.tools)
}
pub(super) async fn invoke(
    client: &Client,
    name: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Value, ErrorData> {
    let args = serde_json::Value::Object(arguments);
    let bad =
        |_: serde_json::Error| ErrorData::invalid_params("invalid remote task arguments", None);
    let result = match name {
        "remote_exec" => {
            let args: Exec = serde_json::from_value(args).map_err(bad)?;
            client.exec(&args.command).await
        }
        "remote_read" => {
            let args: Read = serde_json::from_value(args).map_err(bad)?;
            client
                .read(&args.path)
                .await
                .map(|text| json!({"text":text}))
        }
        "remote_write" => {
            let args: Write = serde_json::from_value(args).map_err(bad)?;
            client
                .write(&args.path, &args.text)
                .await
                .map(|()| json!({"written":true}))
        }
        _ => unreachable!(),
    }
    .map_err(|error| ErrorData::invalid_request(error, None))?;
    Ok(result)
}
