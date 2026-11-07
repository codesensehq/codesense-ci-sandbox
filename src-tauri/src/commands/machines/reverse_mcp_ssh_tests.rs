// SPDX-License-Identifier: MIT
#[derive(Clone)]
struct ReverseContextFixture {
    context: crate::commands::route_to_context::ActiveContext,
    suppression: crate::content_engine::dispatcher::SuppressionHandle,
    gate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}
impl rmcp::ServerHandler for ReverseContextFixture {
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
        (self.gate)().map_err(|e| rmcp::ErrorData::invalid_request(e, None))?;
        let tools = crate::mcp::handler::CodesenseHandler::served_tool_router()
            .list_all()
            .into_iter()
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
        (self.gate)().map_err(|e| rmcp::ErrorData::invalid_request(e, None))?;
        let args = serde_json::Value::Object(request.arguments.unwrap_or_default());
        let bad = |e: serde_json::Error| rmcp::ErrorData::invalid_params(e.to_string(), None);
        let value = match request.name.as_ref() {
            "sense_sync_adopt" => {
                crate::mcp::context_tools::adopt_for(
                    self.context.clone(),
                    serde_json::from_value(args).map_err(bad)?,
                    &crate::context_repo::caller_scope::CallerScope::Human,
                    || Ok(self.suppression.clone()),
                )
                .await?
            }
            "sense_sync_read_layer_file" => {
                crate::mcp::context_tools::read(
                    self.context.clone(),
                    serde_json::from_value(args).map_err(bad)?,
                )
                .await?
            }
            _ => {
                return Err(rmcp::ErrorData::invalid_request(
                    "fixture does not invoke this permitted production family",
                    None,
                ))
            }
        };
        Ok(rmcp::model::CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(value.to_string()),
        ]))
    }
}

#[test]
#[ignore = "owned real SSH: installed bridge, actual multiplexer/rmcp and production bound-context tool core"]
fn reverse_mcp_bridge_context_over_real_ssh() {
    use crate::commands::host_connection::RemoteTransportOwner;
    use crate::terminal::session_server::{Ledger, SessionServerClient};
    use codesense_pty_core::domain::{PtySessionId, TerminalDomain};
    use codesense_pty_core::spawn::SpawnIntent;
    use codesense_session_server::testing::{registration, RecordingSink};
    use rmcp::ServiceExt;
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let store = FakeStore::default();
    let mut environment = env(root.path());
    environment.registry = Registry::new(environment.codesense_home.clone(), Principal::from(41));
    let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(41);
    environment.private_authority = Some(authority);
    let params:crate::mcp::machine_tools::DraftOperationParams=serde_json::from_value(serde_json::json!({"input":{"mode":"private-account","label":"Reverse MCP fixture","host":"127.0.0.1","port":var("CODESENSE_REMOTE_HOST_PORT").parse::<u16>().unwrap(),"username":"dev","signIn":{"method":"keyFile","keyPath":var("CODESENSE_REMOTE_HOST_KEY")}}})).unwrap();
    let mut draft: MachineDraftInput = params.input.into();
    draft.trusted_host_key_fp = test_connection(&environment, &store, None, &draft)
        .unwrap()
        .steps[0]
        .fingerprint
        .clone();
    let saved = save(&environment, &store, None, &draft, &Scope::Personal, false).unwrap();
    private_setup::enroll(&environment, &store, Some(&saved.machine_id), &draft).unwrap();
    install_server(&environment, &store, Some(&saved.machine_id), &draft).unwrap();
    save(
        &environment,
        &store,
        Some(&saved.machine_id),
        &draft,
        &Scope::Personal,
        true,
    )
    .unwrap();
    assert!(private_setup::authorize_remote_mcp_control(&environment, &saved.machine_id).is_err());
    private_setup::set_remote_mcp_control(&environment, &saved.machine_id, true).unwrap();
    let environment = Arc::new(environment);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let transport=open_pinned_saved(&environment,&store,&saved.machine_id).unwrap();
        let admitted=RemoteTransportOwner::from_pinned(transport).into_pending().admit(std::time::Duration::from_secs(5),tokio_util::sync::CancellationToken::new()).await.unwrap();
        let managed=SessionServerClient::from_remote_admitted(admitted,Arc::new(RecordingSink::default()),Arc::<Ledger>::default(),None).await.unwrap();
        let namespace=managed.domain_client().namespace().clone();
        let manager=crate::commands::host_connection_manager::test_adopt_real_private_client(&saved.machine_id,managed).await;
        let target=manager.target_for_namespace(&namespace).unwrap();
        let backend=manager.capture_admitted_backend(&target).unwrap();
        let client=backend.client();
        let session=PtySessionId::new(format!("reverse-mcp-{}",uuid::Uuid::new_v4()));
        let context=crate::commands::route_to_context::ActiveContext{codesense_home:environment.codesense_home.clone(),user_id:41,org_slug:"acme/context".into(),team_name:Some("bound".into())};
        let dispatcher=crate::content_engine::dispatcher::Dispatcher::new(1,100,1000,500);
        let gate={let environment=environment.clone();let machine=saved.machine_id.clone();let manager=manager.clone();let backend=backend.clone();Arc::new(move ||{private_setup::authorize_remote_mcp_control(&environment,&machine)?;manager.revalidate_backend(&backend).map_err(|e|format!("{e:?}"))})};
        let handler=ReverseContextFixture{context:context.clone(),suppression:dispatcher.suppression_handle(),gate};
        let serve=Arc::new(move |stream:tokio::io::DuplexStream|{let handler=handler.clone();tauri::async_runtime::spawn(async move {let service=handler.serve(stream).await.unwrap();let _=service.waiting().await;});});
        client.prepare_reverse_mcp(&session,format!("{}{}",uuid::Uuid::new_v4().simple(),uuid::Uuid::new_v4().simple()),serve).unwrap();
        client.spawn(SpawnIntent{agent_env:Vec::new(),remote_launch:Default::default(),login_path:None,command:Some(r#"python3 -c 'import json,os,subprocess; c=json.load(open(os.environ["CODESENSE_MCP_CONFIG"]))["mcpServers"]["codesense"]; raise SystemExit(subprocess.call([c["command"]]+c["args"],close_fds=True,env={"PATH":"/usr/bin:/bin"}))'"#.into()),argv:None,shell:Some("/bin/sh".into()),cwd:"/home/dev".into(),cols:240,rows:40,shell_integration:false,starship_preset:None,api_port:None,env_remove:vec![],env:vec![]},registration(session.as_str(),None,false)).unwrap();
        client.claim_writable_typed(&session).unwrap();
        let send=|value:serde_json::Value|client.write(&session,format!("{value}\n").as_bytes()).unwrap();
        let response=|id:u64|{
            let deadline=std::time::Instant::now()+std::time::Duration::from_secs(12);
            loop {
                let output=codesense_pty_core::domain::visible_text(&*client, &session, u32::MAX).unwrap().text;
                if let Some(value)=output.lines().filter_map(|line|serde_json::from_str::<serde_json::Value>(line.trim()).ok()).find(|value|value["id"]==id&&(value.get("result").is_some()||value.get("error").is_some())) {break value;}
                assert!(std::time::Instant::now()<deadline,"MCP response {id} missing: {output}");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        send(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"owned-remote","version":"1"}}}));
        assert!(response(1).get("result").is_some());
        send(serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        send(serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
        let listed=response(2);let tools=listed["result"]["tools"].as_array().unwrap_or_else(||panic!("tools/list: {listed}"));
        assert!(tools.iter().any(|tool|tool["name"]=="connect_machine"));
        // #1919: a remote agent lists every tool a local agent has (this
        // assertion that the remote-control grant was hidden predates it).
        assert!(tools.iter().any(|tool|tool["name"]=="set_machine_remote_mcp_control"));
        let content="Agent-chosen remote context — UTF8 ✓";
        let logical=format!("notes/remote-{}.md",uuid::Uuid::new_v4());
        let slug=serde_json::json!({"owner":"acme","repo":"api"});
        // The real context core receives captured 'bound'; a changing UI choice
        // cannot participate in this adapter's destination selection.
        let viewer=crate::mcp::state::ActiveTeamState::default();
        viewer.update(crate::mcp::state::ActiveTeam{org_slug:"acme/context".into(),team_name:"another-team".into()});
        send(serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"sense_sync_adopt","arguments":{"codeRepoPath":"/home/dev","codeRepoSlug":slug,"logicalName":logical,"targetLayer":"TeamWorkspace","content":content}}}));
        let adopted=response(3);assert!(adopted.get("error").is_none(),"{adopted}");
        send(serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"sense_sync_read_layer_file","arguments":{"codeRepoSlug":slug,"logicalName":logical,"layerId":"TeamWorkspace"}}}));
        let read=response(4);let body:serde_json::Value=serde_json::from_str(read["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["contentUtf8"],content,"{read}");
        let org_root=crate::context_repo::path_to_org_root(&context.codesense_home,&Principal::from(41),"acme/context").unwrap();
        // SW-XS-8: `TeamWorkspace` is the retired install tier; an old caller
        // naming it lands in the team's content (the load-time mapping).
        let bound=crate::context_repo::path_to_team_content_dir(&org_root,"bound").join(&logical);
        assert!(!crate::context_repo::path_to_team_content_dir(&org_root,"another-team").join(&logical).exists());
        assert!(!crate::context_repo::path_to_team_install_dir(&org_root,"bound").join(&logical).exists());
        assert_eq!(std::fs::read_to_string(&bound).unwrap(),content);
        private_setup::set_remote_mcp_control(&environment,&saved.machine_id,false).unwrap();
        send(serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"sense_sync_adopt","arguments":{"codeRepoPath":"/home/dev","codeRepoSlug":slug,"logicalName":logical,"targetLayer":"TeamWorkspace","content":"MUST_NOT_OVERWRITE"}}}));
        assert!(response(5).get("error").is_some());
        assert_eq!(std::fs::read_to_string(&bound).unwrap(),content);
        client.kill(&session).unwrap();manager.disconnect(&saved.machine_id).await.unwrap();
        println!("REVERSE_MCP_BRIDGE_CONTEXT_PASS enabled_open_adopt_read_revoke=true content={content}");
    });
}
