// SPDX-License-Identifier: MIT
//! Official Claude CLI with a synthetic model; actual admitted SSH tool execution.
use super::{
    claude, claude_executor, claude_tools, fixture::ContextFixture, harness::McpConnection,
};
use axum::{routing::post, Json, Router};
use rmcp::{model::*, service::RequestContext, ErrorData, RoleServer, ServerHandler};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

#[derive(Clone)]
struct Fixture {
    base: ContextFixture,
    client: Arc<claude_executor::Client>,
    calls: Arc<AtomicUsize>,
}
impl ServerHandler for Fixture {
    fn get_info(&self) -> ServerInfo {
        self.base.get_info()
    }
    async fn list_tools(
        &self,
        p: Option<PaginatedRequestParams>,
        c: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut result = self.base.list_tools(p, c).await?;
        result.tools.extend(claude_tools::tools()?);
        Ok(result)
    }
    async fn call_tool(
        &self,
        p: CallToolRequestParams,
        c: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if p.name == "sense_sync_list_layers" {
            return self.base.call_tool(p, c).await;
        }
        if !matches!(
            p.name.as_ref(),
            "remote_exec" | "remote_read" | "remote_write"
        ) {
            return Err(ErrorData::invalid_request("outside fixture scope", None));
        }
        let result =
            claude_tools::invoke(&self.client, &p.name, p.arguments.unwrap_or_default()).await?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(CallToolResult::success(vec![ContentBlock::text(
            result.to_string(),
        )]))
    }
}

pub(crate) async fn prove_claude_harness(
    endpoint: &crate::mcp::remote_executor::ExecutorEndpoint,
    root: &Path,
) -> Result<(), String> {
    let calls = Arc::new(AtomicUsize::new(0));
    let context_calls = Arc::new(AtomicUsize::new(0));
    let valid: super::harness::Validate = Arc::new(|| Ok(()));
    let client = claude_executor::Client::connect(endpoint, valid.clone()).await?;
    let fixture = Fixture {
        client,
        calls: calls.clone(),
        base: ContextFixture {
            context: crate::commands::route_to_context::ActiveContext {
                codesense_home: root.join("claude-context"),
                user_id: 41,
                org_slug: "fixture/context".into(),
                team_name: Some("bound".into()),
            },
            calls: context_calls.clone(),
        },
    };
    let mcp = super::mcp_endpoint::Endpoint::serve(move || fixture.clone()).await?;
    let steps = Arc::new(AtomicUsize::new(0));
    let observations = Arc::new(AtomicUsize::new(0));
    let seen = observations.clone();
    let model=Router::new()
        .route("/v1/messages/count_tokens",post(||async {Json(json!({"input_tokens":1}))}))
        .route("/v1/messages",post(move|Json(body):Json<Value>| {
            let steps=steps.clone();let seen=seen.clone();
            async move {
                let step=steps.load(Ordering::SeqCst);
                for message in body["messages"].as_array().into_iter().flatten() {
                    for block in message["content"].as_array().into_iter().flatten() {
                        if block["type"]=="tool_result" && block.to_string().contains("owned-remote-task-proof") {seen.fetch_add(1,Ordering::SeqCst);}
                    }
                }
                let tools=body["tools"].as_array().expect("Claude tools advertised");
                assert!(tools.iter().all(|t|t["name"].as_str().is_some_and(|n|n.starts_with("mcp__codesense_remote_task__") || n=="mcp__fixture_local__local_ping")),"unexpected local tools: {tools:?}");
                let operation=match step {
                    0=>Some(("remote_write",json!({"path":"agent-proof.txt","text":"owned-remote-task-proof"}))),
                    1=>Some(("remote_read",json!({"path":"agent-proof.txt"}))),
                    2=>Some(("remote_exec",json!({"command":"test -z \"$ANTHROPIC_API_KEY\" && test -z \"$RM_USER_SETTINGS_SENTINEL\" && test -z \"$AUTH1_LOCAL_MCP_CREDENTIAL\" && cat agent-proof.txt && printf 'owned claude command' > claude-command-proof.txt"}))),
                    3=>Some(("sense_sync_list_layers",json!({"codeRepoSlug":{"owner":"fixture","repo":"workspace"}}))),
                    4=>Some(("local_ping",json!({}))),
                    _=>None,
                };
                let mut events=vec![json!({"type":"message_start","message":{"id":format!("msg_{step}"),"type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":0}}})];
                let reason=if let Some((name,input))=operation {
                    let name=if name=="local_ping" {"mcp__fixture_local__local_ping".to_string()} else {format!("mcp__codesense_remote_task__{name}")};
                    assert!(tools.iter().any(|t|t["name"]==name),"missing {name}");
                    events.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":format!("tool_{step}"),"name":name,"input":{}}}));
                    events.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":input.to_string()}}));
                    "tool_use"
                } else {
                    events.push(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}));
                    events.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"owned claude fixture completed"}}));
                    "end_turn"
                };
                events.extend([json!({"type":"content_block_stop","index":0}),json!({"type":"message_delta","delta":{"stop_reason":reason,"stop_sequence":null},"usage":{"output_tokens":3}}),json!({"type":"message_stop"})]);
                let data=events.iter().map(|v|format!("event: {}\ndata: {v}\n\n",v["type"].as_str().unwrap())).collect::<String>();
                steps.fetch_add(1,Ordering::SeqCst);
                ([("content-type","text/event-stream")],data)
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let base_url = format!(
        "http://{}",
        listener.local_addr().map_err(|e| e.to_string())?
    );
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, model).await;
    });
    let home = root.join("synthetic-claude-home");
    let cwd = root.join("claude-local-harness");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&cwd).map_err(|e| e.to_string())?;
    let marker = root.join("claude-hook-ran");
    std::fs::write(home.join("settings.json"),json!({"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":format!("touch {}",marker.display())}]}]},"env":{"RM_USER_SETTINGS_SENTINEL":"forbidden"}}).to_string()).map_err(|e|e.to_string())?;
    let mut connection = McpConnection::new(mcp.url.clone(), mcp.token.clone());
    connection.projection = super::profile_fixture::prepare(
        root,
        &cwd,
        crate::install::agent_account_store::AgentProvider::Claude,
    )?;
    super::runtime::verify(crate::install::agent_account_store::AgentProvider::Claude).await?;
    let mut harness = claude::Harness::spawn(
        &cwd,
        claude::Launch {
            home: Some(&home),
            mcp: &connection,
            prompt: "Run owned remote fixture",
            mode: claude::AuthMode::Synthetic { base_url },
        },
        valid,
        Arc::new(AtomicBool::new(false)),
    )
    .await?;
    let mut output = String::new();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        harness.run(|text| {
            output.push_str(text);
            Ok(())
        }),
    )
    .await
    .map_err(|_| "Claude fixture timed out".to_string())
    .and_then(|v| v);
    let stopped = harness.stop().await;
    task.abort();
    drop(mcp);
    result?;
    if !stopped {
        return Err("local Claude cleanup outcome unknown".into());
    }
    super::profile_fixture::verify(root).await?;
    if calls.load(Ordering::SeqCst) != 3
        || context_calls.load(Ordering::SeqCst) != 1
        || observations.load(Ordering::SeqCst) == 0
        || !output.contains("owned claude fixture completed")
        || marker.exists()
        || cwd.join("agent-proof.txt").exists()
        || cwd.join("claude-command-proof.txt").exists()
    {
        return Err(format!(
            "Claude fixture incomplete: remote calls={}, context calls={}, observed={}, output={}",
            calls.load(Ordering::SeqCst),
            context_calls.load(Ordering::SeqCst),
            observations.load(Ordering::SeqCst),
            output.len()
        ));
    }
    Ok(())
}
