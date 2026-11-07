// SPDX-License-Identifier: MIT
//! Synthetic model routing proof, never an account-authentication claim.
use super::harness::{Environment, Harness, McpConnection};
use axum::{routing::post, Json, Router};
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};

pub(crate) async fn prove_harness(
    endpoint: &crate::mcp::remote_executor::ExecutorEndpoint,
    root: &Path,
) -> Result<(), String> {
    let real_account = std::env::var("CODESENSE_REMOTE_AUTH_ACCOUNT_PROOF").as_deref() == Ok("1");
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = Arc::new(AtomicBool::new(false));
    let observed_copy = observed.clone();
    let mcp_calls = calls.clone();
    let context = crate::commands::route_to_context::ActiveContext {
        codesense_home: root.join("context-home"),
        user_id: 41,
        org_slug: "fixture/context".into(),
        team_name: Some("bound".into()),
    };
    let mcp = super::mcp_endpoint::Endpoint::serve(move || ContextFixture {
        context: context.clone(),
        calls: mcp_calls.clone(),
    })
    .await?;
    let url = mcp.url.clone();
    let n = Arc::new(AtomicUsize::new(0));
    let desktop_cwd = root.join("local-harness").to_string_lossy().to_string();
    let model=Router::new().route("/v1/responses",post(move |Json(body):Json<Value>|{
        let n=n.clone();let observed=observed_copy.clone();let desktop_cwd=desktop_cwd.clone();
        async move {
            let step=n.load(Ordering::SeqCst);
            if let Some(input)=body["input"].as_array() {for item in input {
                if matches!(item["type"].as_str(),Some("function_call_output"|"custom_tool_call_output")) {eprintln!("SYNTHETIC TOOL RESULT {}",item.to_string().chars().take(2000).collect::<String>());}
            }}
            let tools=advertised_tools(&body);
            eprintln!("SYNTHETIC TOOL NAMES {}",tools.iter().filter_map(|t|t["name"].as_str().or_else(||t["type"].as_str())).collect::<Vec<_>>().join(","));
            if body.to_string().contains("owned-remote-task-proof") {observed.store(true,Ordering::SeqCst);}
            let item=if step==0 {json!({"type":"function_call","id":"fc1","call_id":"call1","name":"exec_command","arguments":json!({"cmd":"test -z \"$CODESENSE_AUTH1_LOCAL_SENTINEL\" && test -z \"$AUTH1_LOCAL_MCP_CREDENTIAL\" && pwd && printf owned-remote-task-proof > agent-proof.txt && cat agent-proof.txt","max_output_tokens":1000}).to_string()})}
            else if step==1 {json!({"type":"function_call","id":"fc_escape","call_id":"call_escape","name":"exec_command","arguments":json!({"cmd":"printf bad > desktop-escape.txt","workdir":desktop_cwd,"max_output_tokens":1000}).to_string()})}
            else if step==2 {json!({"type":"custom_tool_call","id":"patch1","call_id":"patch1","name":"apply_patch","input":"*** Begin Patch\n*** Add File: native-proof.txt\n+owned native patch\n*** End Patch"})}
            else if step==3 {
                assert!(tools.iter().any(|t|t["type"]=="tool_search"),"native MCP discovery must be advertised");
                json!({"type":"tool_search_call","id":"search1","call_id":"search1","execution":"client","arguments":{"query":"codesense_remote_task sense_sync_list_layers fixture_local local_ping denied_tool","limit":16}})
            } else if step==4 {
                let namespace=tools.iter().find(|v|v["name"]=="mcp__codesense_remote_task").expect("task MCP namespace must be advertised");
                assert_eq!(namespace["type"],"namespace");
                assert!(namespace["tools"].as_array().is_some_and(|tools|tools.iter().any(|tool|tool["name"]=="sense_sync_list_layers")));
                json!({"type":"function_call","id":"fc2","call_id":"call2","name":"sense_sync_list_layers","namespace":"mcp__codesense_remote_task","arguments":json!({"codeRepoSlug":{"owner":"fixture","repo":"workspace"}}).to_string()})
            } else if step==5 {
                let namespace=tools.iter().find(|v|v["name"]=="mcp__fixture_local").expect("selected local MCP namespace must be advertised");
                let exposed=namespace["tools"].as_array().expect("selected MCP tools");
                assert!(exposed.iter().any(|t|t["name"]=="local_ping"));
                assert!(!exposed.iter().any(|t|t["name"]=="denied_tool"));
                json!({"type":"function_call","id":"fc_local","call_id":"call_local","name":"local_ping","namespace":"mcp__fixture_local","arguments":"{}"})
            } else if step==6 {json!({"type":"function_call","id":"fc_child","call_id":"call_child","name":"exec_command","arguments":json!({"cmd":"echo $$ > executor-child.pid; sleep 300","yield_time_ms":1000,"max_output_tokens":1000}).to_string()})
            } else {json!({"id":"msg1","type":"message","role":"assistant","content":[{"type":"output_text","text":"owned fixture completed","annotations":[]}]})};
            n.fetch_add(1,Ordering::SeqCst);
            let response=json!({"id":format!("resp{step}"),"object":"response","status":"completed","output":[item.clone()],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}});
            let data=format!("data: {}\n\ndata: {}\n\n",json!({"type":"response.output_item.done","output_index":0,"item":item}),json!({"type":"response.completed","response":response}));
            ([("content-type","text/event-stream")],data)
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let port = listener.local_addr().unwrap().port();
    let model_task = tokio::spawn(async move {
        let _ = axum::serve(listener, model).await;
    });
    let home = root.join("synthetic-codex-home");
    let cwd = root.join("local-harness");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&cwd).map_err(|e| e.to_string())?;
    std::fs::write(home.join("config.toml"),format!("model_provider='owned_fixture'\nmodel='gpt-5.5'\n[model_providers.owned_fixture]\nname='Owned fixture'\nbase_url='http://127.0.0.1:{port}/v1'\nwire_api='responses'\nrequires_openai_auth=false\n")).map_err(|e|e.to_string())?;
    use std::io::Write;
    writeln!(std::fs::OpenOptions::new().append(true).open(home.join("config.toml")).unwrap(),"\n[mcp_servers.unallowed]\ncommand='/bin/sh'\nargs=['-c','touch {}/unallowed-local-server']",cwd.display()).unwrap();
    // A task's start checks the local release before it spawns; so does the proof.
    super::runtime::verify(crate::install::agent_account_store::AgentProvider::Codex).await?;
    let mut harness = Harness::spawn(
        &cwd,
        if real_account { None } else { Some(&home) },
        Arc::new(|| Ok(())),
        Arc::new(AtomicBool::new(false)),
    )
    .await?;
    harness.synthetic_fixture = !real_account;
    harness.fixture_diagnostics = true;
    let mut connection = McpConnection::new(url, mcp.token.clone());
    if !real_account {
        connection.projection = super::profile_fixture::prepare(
            root,
            &cwd,
            crate::install::agent_account_store::AgentProvider::Codex,
        )?;
    }
    let mut output = String::new();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(if real_account {180}else{60}),
        harness.run(
            Environment {
                id: endpoint.environment_id().into(),
                url: endpoint.ws_url().into(),
                token: endpoint.auth_token().into(),
                cwd: endpoint.remote_cwd().into(),
            },
            &connection,
            if real_account {"Use native exec_command in the selected remote workspace to write the exact text owned-remote-task-proof into agent-proof.txt and read it back. Then call codesense_remote_task sense_sync_list_layers once. Do not use other tools. Finish with the exact text owned fixture completed."} else {"Run owned remote fixture"},
            |text| {
                output.push_str(text);
                Ok(())
            },
        ),
    )
    .await
    .map_err(|_| "fixture timed out".to_string())
    .and_then(|v| v);
    let stopped = harness.stop().await;
    drop(mcp);
    model_task.abort();
    eprintln!(
        "OWNED FIXTURE FINAL {}",
        output.chars().take(2000).collect::<String>()
    );
    result?;
    if !stopped {
        return Err("local harness cleanup outcome unknown".into());
    }
    if !real_account {
        super::profile_fixture::verify(root).await?;
    }
    if (!real_account && !observed.load(Ordering::SeqCst))
        || calls.load(Ordering::SeqCst) != 1
        || cwd.join("agent-proof.txt").exists()
        || cwd.join("desktop-escape.txt").exists()
        || cwd.join("native-proof.txt").exists()
        || cwd.join("unallowed-local-server").exists()
        || !output.contains("owned fixture completed")
    {
        return Err(format!(
            "routing fixture incomplete: remote={}, MCP calls={}, output={}",
            observed.load(Ordering::SeqCst),
            calls.load(Ordering::SeqCst),
            output.len()
        ));
    }
    Ok(())
}

#[derive(Clone)]
pub(super) struct ContextFixture {
    pub(super) context: crate::commands::route_to_context::ActiveContext,
    pub(super) calls: Arc<AtomicUsize>,
}
impl rmcp::ServerHandler for ContextFixture {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::InitializeResult::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
    }
    async fn list_tools(
        &self,
        _: Option<rmcp::model::PaginatedRequestParams>,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        let tools = crate::mcp::handler::CodesenseHandler::served_tool_router()
            .list_all()
            .into_iter()
            .filter(|t| t.name == "sense_sync_list_layers")
            .collect();
        Ok(rmcp::model::ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        })
    }
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        if request.name != "sense_sync_list_layers" {
            return Err(rmcp::ErrorData::invalid_request(
                "outside fixture scope",
                None,
            ));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        let layers = crate::commands::content_engine::list_layers(
            crate::commands::content_engine::ListLayersParams {
                codesense_home: self.context.codesense_home.to_string_lossy().into(),
                user_id: self.context.user_id,
                org_slug: self.context.org_slug.clone(),
                team_name: self.context.team_name.clone(),
                code_repo_slug: Some(
                    crate::domain::SourceRepoSlug::new("fixture", "workspace").unwrap(),
                ),
                writable_only: false,
            },
        )
        .await
        .map_err(|e| rmcp::ErrorData::internal_error(e.to_string(), None))?;
        Ok(rmcp::model::CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(serde_json::to_string(&layers).unwrap()),
        ]))
    }
}

/// Native discovery can add declarations incrementally alongside static tools.
fn advertised_tools(body: &Value) -> Vec<&Value> {
    body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            body["input"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|item| {
                    matches!(
                        item["type"].as_str(),
                        Some("additional_tools" | "tool_search_output")
                    )
                })
                .flat_map(|item| item["tools"].as_array().into_iter().flatten()),
        )
        .collect()
}
#[test]
fn fixture_collects_static_and_all_discovered_tools() {
    let body = json!({"tools":[{"name":"static"}],"input":[{"type":"additional_tools","tools":[{"name":"first"}]},{"type":"additional_tools","tools":[{"name":"second"}]},{"type":"tool_search_output","tools":[{"name":"discovered"}]}]});
    assert_eq!(
        advertised_tools(&body)
            .iter()
            .map(|v| v["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["static", "first", "second", "discovered"]
    );
}
