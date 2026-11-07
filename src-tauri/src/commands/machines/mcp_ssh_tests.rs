// SPDX-License-Identifier: MIT
#[test]
#[ignore = "owned SSH fixture; shared production MCP dispatch, injected profile authority"]
fn mcp_machine_shell_over_real_ssh() {
    use crate::commands::host_connection::RemoteTransportOwner;
    use crate::commands::pty_layout::{app_viewer, RemoteShellRecord};
    use crate::mcp::machine_tools::{DraftOperationParams, OpenRemoteTabParams};
    use crate::mcp::remote_terminal::{execute, RemoteOperation};
    use crate::terminal::session_server::{Ledger, SessionServerClient, SessionServerState};
    use codesense_pty_core::domain::{PtySessionId, TerminalDomain};
    use codesense_pty_core::layout::{CloseMode, OpenTab, Placement, WorkspaceDescriptor};
    use codesense_pty_core::spawn::SpawnIntent;
    use codesense_session_server::testing::{
        registration, sh, wait_until, RecordingSink, TestServer,
    };
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let store = FakeStore::default();
    let mut environment = env(root.path());
    environment.registry = Registry::new(environment.codesense_home.clone(), Principal::from(41));
    let (authority, _retire) = crate::commands::auth::private_authority::test_snapshot(41);
    environment.private_authority = Some(authority);
    let params:DraftOperationParams=serde_json::from_value(serde_json::json!({"input":{"mode":"private-account","label":"MCP fixture","host":"127.0.0.1","port":var("CODESENSE_REMOTE_HOST_PORT").parse::<u16>().unwrap(),"username":"dev","signIn":{"method":"keyFile","keyPath":var("CODESENSE_REMOTE_HOST_KEY")}}})).unwrap();
    let mut draft: MachineDraftInput = params.input.into();
    let first = test_connection(&environment, &store, None, &draft).unwrap();
    draft.trusted_host_key_fp = first.steps[0].fingerprint.clone();
    assert!(draft.trusted_host_key_fp.is_some());
    let saved = save(&environment, &store, None, &draft, &Scope::Personal, false).unwrap();
    private_setup::enroll(&environment, &store, Some(&saved.machine_id), &draft).unwrap();
    install_server(&environment, &store, Some(&saved.machine_id), &draft).unwrap();
    let saved = save(
        &environment,
        &store,
        Some(&saved.machine_id),
        &draft,
        &Scope::Personal,
        true,
    )
    .unwrap();

    let local_server = TestServer::start();
    let local_state = SessionServerState::default();
    let local = local_state
        .connect_or_launch(
            local_server.root(),
            Arc::new(RecordingSink::default()),
            |_| Err("must not launch".into()),
        )
        .unwrap();
    let id = "mcp-exact-collision";
    local_server.track(id);
    let local_pid = local
        .spawn(
            sh("printf 'LOCAL_CONTROL\\n';sleep 120"),
            registration(id, None, false),
        )
        .unwrap()
        .unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let transport=open_pinned_saved(&environment,&store,&saved.machine_id).unwrap();
        let admitted=RemoteTransportOwner::from_pinned(transport).into_pending().admit(Duration::from_secs(5),CancellationToken::new()).await.unwrap();
        let managed=SessionServerClient::from_remote_admitted(admitted,Arc::new(RecordingSink::default()),Arc::<Ledger>::default(),None).await.unwrap();
        let namespace=managed.domain_client().namespace().clone();
        let manager=crate::commands::host_connection_manager::test_adopt_real_private_client(&saved.machine_id,managed).await;
        let target=manager.target_for_namespace(&namespace).unwrap();
        let exact=manager.capture_admitted_backend(&target).unwrap();
        let client=exact.client();
        let session=PtySessionId::new(id);
        let request_id=uuid::Uuid::new_v4();
        let request:OpenRemoteTabParams=serde_json::from_value(serde_json::json!({"operation":"create","machineId":saved.machine_id,"teamKey":"org:team","workspaceId":"work","requestId":request_id,"cwd":"/home/dev","kind":"shell","headed":false})).unwrap();
        assert!(request.into_request().is_ok());
        let mut record=RemoteShellRecord {
        workspace_story_id: None, agent: None, phase: Default::default(),handle:"remote-tab-mcp-fixture".into(),owner_id:41,request_id,machine_id:saved.machine_id.clone(),team_key:"org:team".into(),workspace_id:"work".into(),workspace:crate::commands::pty_layout::workspace_key("org:team","work"),cwd:"/home/dev".into(),name:None, mcp_profile_revision: None, mcp_inventory: Vec::new(), hooks: false,launch: Default::default(),permission_mode: Default::default(),target:RuntimeSessionTarget{runtime:target.clone(),session:session.clone()},status:"opened",identity:Default::default()};
        client.open_remote_tab(OpenTab{viewer:app_viewer(),workspace:record.workspace.clone(),session:session.clone(),descriptor:Some(WorkspaceDescriptor{display_name:"MCP fixture".into(),kind:"generic".into(),worktree_path:Some("/home/dev".into()),..Default::default()}),name:None,role:None,tags:[("remoteHandle".into(),record.handle.clone()),("remoteRequestId".into(),request_id.to_string())].into(),placement:Placement::Tab,index:None,activate:false},||client.spawn(SpawnIntent{agent_env:Vec::new(),remote_launch:Default::default(),login_path:None,command:None,argv:None,shell:None,cwd:record.cwd.clone(),cols:100,rows:30,shell_integration:true,starship_preset:None,api_port:None,env_remove:vec![],env:vec![]},registration(id,None,false)).map(|_|())).unwrap();
        let remote_pid=client.live_child_pids(std::slice::from_ref(&session)).unwrap()[0].unwrap();
        let revalidate=||manager.revalidate_backend(&exact).map_err(|e|format!("{e:?}"));
        let run=execute(&client,&record,RemoteOperation::Run{command:"printf 'MCP_%s\\nCWD=%s\\nPID=%s\\n' WORKING \"$PWD\" \"$$\"".into(),target:crate::mcp::remote_terminal::InputTarget::Auto},revalidate);
        assert_eq!(run["status"],"submitted","{run}");
        assert!(wait_until(|| {let read=execute(&client,&record,RemoteOperation::Read{since:None,screen:false},revalidate);read["output"].as_str().is_some_and(|out|out.contains("MCP_WORKING")&&out.contains("CWD=/home/dev"))}));
        let read=execute(&client,&record,RemoteOperation::Read{since:None,screen:false},revalidate);
        let head=read["head"].as_str().unwrap().parse::<u64>().unwrap();
        let screen=execute(&client,&record,RemoteOperation::Read{since:None,screen:true},revalidate);
        assert!(screen["output"].as_str().is_some_and(|out|out.contains("MCP_WORKING")),"{screen}");
        let invalid=execute(&client,&record,RemoteOperation::Read{since:Some(u64::MAX),screen:false},revalidate);
        assert_eq!(invalid["code"],"invalidCursor");
        let no_auth=execute(&client,&record,RemoteOperation::Run{command:"printf SHOULD_NOT_RUN".into(),target:crate::mcp::remote_terminal::InputTarget::Auto},||Err("group revoked".into()));
        assert_eq!(no_auth["submitted"],false);
        let checks=std::cell::Cell::new(0);
        let late_revoked=execute(&client,&record,RemoteOperation::Run{command:"printf MUST_NOT_DISPATCH".into(),target:crate::mcp::remote_terminal::InputTarget::Auto},||{checks.set(checks.get()+1);if checks.get()==2{Err("group revoked before dispatch".into())}else{revalidate()}});
        assert_eq!(late_revoked["submitted"],false);
        let checks=std::cell::Cell::new(0);
        let hidden_read=execute(&client,&record,RemoteOperation::Read{since:None,screen:false},||{checks.set(checks.get()+1);if checks.get()==3{Err("group revoked while reading".into())}else{revalidate()}});
        assert_eq!(hidden_read["status"],"refused");
        assert!(hidden_read.get("output").is_none());
        assert!(!client.read_terminal_text(&session,u32::MAX,codesense_pty_core::emulator::TextMode::Normal).unwrap().text.contains("MUST_NOT_DISPATCH"));

        let empty=execute(&client,&record,RemoteOperation::Read{since:Some(head),screen:false},revalidate);
        assert_eq!(empty["status"],"ok");
        let quiet=execute(&client,&record,RemoteOperation::Run{command:"export MCP_QUIET=1".into(),target:crate::mcp::remote_terminal::InputTarget::Auto},revalidate);
        assert_eq!(quiet["submitted"],true);
        assert_eq!(local.live_child_pids(std::slice::from_ref(&session)).unwrap()[0],Some(local_pid));
        assert!(!local.read_terminal_text(&session,u32::MAX,codesense_pty_core::emulator::TextMode::Normal).unwrap().text.contains("MCP_WORKING"));
        let before=client.live_session_ids().unwrap();
        manager.disconnect(&saved.machine_id).await.unwrap();
        assert!(manager.capture_admitted_backend(&target).is_err());
        let transport=open_pinned_saved(&environment,&store,&saved.machine_id).unwrap();
        let admitted=RemoteTransportOwner::from_pinned(transport).into_pending().admit(Duration::from_secs(5),CancellationToken::new()).await.unwrap();
        let managed=SessionServerClient::from_remote_admitted(admitted,Arc::new(RecordingSink::default()),Arc::<Ledger>::default(),None).await.unwrap();
        let namespace=managed.domain_client().namespace().clone();
        let manager=crate::commands::host_connection_manager::test_adopt_real_private_client(&saved.machine_id,managed).await;
        let target=manager.target_for_namespace(&namespace).unwrap();
        let exact=manager.capture_admitted_backend(&target).unwrap();
        let client=exact.client();
        record.target.runtime=target;
        let after=execute(&client,&record,RemoteOperation::Read{since:None,screen:false},||manager.revalidate_backend(&exact).map_err(|e|format!("{e:?}")));
        assert_eq!(after["status"],"ok","{after}");
        assert_eq!(client.live_child_pids(std::slice::from_ref(&session)).unwrap()[0],Some(remote_pid));
        assert_eq!(client.live_session_ids().unwrap(),before,"read/reconnect must not spawn");
        client.close_tab(&app_viewer(),&session,CloseMode::Kill).unwrap();
        manager.disconnect(&saved.machine_id).await.unwrap();
        println!("MCP_SHARED_DISPATCH_PASS remote_pid={remote_pid} local_pid={local_pid} head={head}");
    });
}
