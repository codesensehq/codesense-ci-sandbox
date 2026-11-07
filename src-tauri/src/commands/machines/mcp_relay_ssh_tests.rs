// SPDX-License-Identifier: MIT
#[test]
#[ignore = "owned real SSH: selected desktop stdio servers through actual remote bridge and production relay"]
fn selected_desktop_mcp_servers_over_real_ssh() {
    use crate::commands::host_connection::RemoteTransportOwner;
    use crate::terminal::session_server::{Ledger, SessionServerClient};
    use codesense_pty_core::domain::{PtySessionId, TerminalDomain};
    use codesense_pty_core::spawn::SpawnIntent;
    use codesense_session_server::testing::{registration, RecordingSink};
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
        let script_path=root.path().join("desktop-server.py");
        let mutation=root.path().join("mutations.txt");
        let secret=format!("desktop-only-{}",uuid::Uuid::new_v4());
        std::fs::write(&script_path, r#"
import sys,json,os
assert os.environ.get("LOCAL_SENTINEL", "").startswith("desktop-only-")
for line in sys.stdin:
    request=json.loads(line)
    if "id" not in request: continue
    method=request["method"]
    if method=="initialize":
        result={"protocolVersion":"2024-11-05","capabilities":{"tools":{},"resources":{},"prompts":{}},"serverInfo":{"name":"local-fixture","version":"1"}}
    elif method=="tools/list":
        result={"tools":[{"name":"ready","description":"Authenticated locally","inputSchema":{"type":"object"}}]}
    elif method=="prompts/list":result={"prompts":[{"name":"local-prompt"}]}
    elif method=="resources/list":result={"resources":[{"name":"local-resource","uri":"fixture://local/resource"}]}
    elif method=="resources/read":result={"contents":[{"uri":"fixture://local/resource","text":"local-authenticated-result"}]}
    elif method=="tools/call" and request["params"]["name"]=="oversize":
        result={"content":[{"type":"text","text":"x"*(1024*1024+1)}]}
    elif method=="tools/call" and request["params"]["name"]=="mutate":
        with open(os.environ["MUTATION_FILE"],"a") as f:f.write("effect\n")
        os._exit(0)
    else:result={"content":[{"type":"text","text":"local-authenticated-result"}]}
    print(json.dumps({"jsonrpc":"2.0","id":request["id"],"result":result}),flush=True)
"#).unwrap();
        let http=crate::mcp::local_mcp_relay::http_fixture::Fixture::start().await;
        let mut rows=http.configuration()["mcpServers"].as_object().unwrap().clone();
        for n in 0..6 {
            rows.insert(format!("desktop-{n}"),serde_json::json!({"command":"/usr/bin/python3","args":[script_path],"env":{"LOCAL_SENTINEL":"${FIXTURE_SECRET}","MUTATION_FILE":mutation}}));
        }
        let grant=crate::mcp::local_mcp_relay::config::parse(&serde_json::json!({"mcpServers":rows}),"mcpServers","codesense",None,root.path(),|key| match key { "FIXTURE_SECRET"=>Some(secret.clone()), "MCP_HTTP_FIXTURE_TOKEN"=>Some(crate::mcp::local_mcp_relay::http_fixture::SENTINEL.into()), _=>None }).unwrap();
        let selected=grant.keys().cloned().collect();
        let gate:crate::mcp::local_mcp_relay::Validate={
            let environment=environment.clone();let machine=saved.machine_id.clone();let manager=manager.clone();let backend=backend.clone();
            Arc::new(move || {
                let result=private_setup::authorize_remote_mcp_control(&environment,&machine).and_then(|()| manager.revalidate_backend(&backend).map_err(|e|format!("{e:?}")));
                Box::pin(async move {result})
            })
        };
        let opened=Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let serve={let opened=opened.clone();Arc::new(move |id:String, stream:tokio::io::DuplexStream| {
            let Some(server)=grant.get(&id).cloned() else {return};
            opened.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
            let gate=gate.clone();
            tauri::async_runtime::spawn(async move {crate::mcp::local_mcp_relay::serve(server,stream,gate).await;});
        })};
        client.prepare_reverse_mcp_servers(&session,format!("{}{}",uuid::Uuid::new_v4().simple(),uuid::Uuid::new_v4().simple()),selected,serve).unwrap();
        let script=r#"
import json,os,subprocess,sys,select
configuration=json.load(open(os.environ["CODESENSE_MCP_CONFIG"]))
assert set(configuration["mcpServers"])==set(["codesense","desktop-http"]+["desktop-"+str(n) for n in range(6)])
assert "LOCAL_SENTINEL" not in json.dumps(configuration)
assert "desktop-only-" not in json.dumps(configuration)
assert "MCP_HTTP_FIXTURE_TOKEN" not in json.dumps(configuration)
assert "mcp3-desktop-http-fixture-only" not in json.dumps(configuration)
servers=configuration["mcpServers"]
children=[]
def launch(name):
    c=servers[name]
    assert set(c)==set(["command","args"])
    p=subprocess.Popen([c["command"]]+c["args"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,close_fds=True,env={"PATH":"/usr/bin:/bin"})
    children.append(p)
    return p
def send(p,value):p.stdin.write(json.dumps(value)+"\n");p.stdin.flush()
def receive(p):
    assert select.select([p.stdout],[],[],8)[0],"response timeout"
    line=p.stdout.readline()
    return json.loads(line) if line else None
def request(p,id,method,params=None):
    value={"jsonrpc":"2.0","id":id,"method":method}
    if params is not None:value["params"]=params
    send(p,value);return receive(p)
def initialize(p,id,expected="local-fixture"):
    value=request(p,id,"initialize",{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"actual-remote","version":"1"}})
    assert value["result"]["serverInfo"]["name"]==expected
    send(p,{"jsonrpc":"2.0","method":"notifications/initialized"})
try:
    for n in range(6):initialize(launch("desktop-"+str(n)),n+1)
    assert request(children[0],20,"prompts/list")["result"]["prompts"][0]["name"]=="local-prompt"
    assert request(children[0],21,"resources/list")["result"]["resources"][0]["uri"]=="fixture://local/resource"
    assert request(children[0],22,"resources/read",{"uri":"fixture://local/resource"})["result"]["contents"][0]["text"]=="local-authenticated-result"
    c=servers["desktop-0"]
    denied=subprocess.Popen([c["command"],"--remote-socket",os.environ["CODESENSE_MCP_SOCK"],"--server-id","forbidden"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,close_fds=True)
    assert denied.wait(timeout=8)!=0
    assert request(children[0],23,"tools/list")["result"]["tools"][0]["name"]=="ready"
    assert request(children[1],24,"tools/call",{"name":"oversize","arguments":{}}) is None
    assert request(children[2],25,"tools/call",{"name":"mutate","arguments":{}}) is None
    assert request(children[0],26,"tools/list")["result"]["tools"][0]["name"]=="ready"
    http=launch("desktop-http")
    initialize(http,30,"desktop-http")
    assert request(http,31,"prompts/list")["result"]["prompts"][0]["name"]=="fixture-prompt"
    assert request(http,32,"prompts/get",{"name":"fixture-prompt"})["result"]["messages"][0]["content"]["text"]=="authenticated prompt"
    assert request(http,33,"resources/read",{"uri":"fixture://resource"})["result"]["contents"][0]["text"]=="authenticated resource"
    assert request(http,34,"tools/call",{"name":"safe","arguments":{}})["result"]["content"][0]["text"]=="authenticated:true"
    for i,name in enumerate(["expire","lost","oversized"]):
        peer=launch("desktop-http")
        initialize(peer,40+i*2,"desktop-http")
        assert request(peer,41+i*2,"tools/call",{"name":name,"arguments":{}}) is None
    assert request(http,48,"tools/list")["result"]["tools"][0]["name"]=="safe"
    print("MCP_RELAY_READY_REVOKE",flush=True)
    input()
    try:
        value=request(children[0],27,"tools/list")
        assert value is None
    except BrokenPipeError:pass
    print("MCP_RELAY_SSH_PASS",flush=True)
finally:
    for p in children:
        p.terminate()
        try:p.wait(timeout=3)
        except subprocess.TimeoutExpired:p.kill();p.wait()
"#;
        let command=format!("python3 -c '{}'",script.replace('\'',"'\"'\"'"));
        client.spawn(SpawnIntent{agent_env:Vec::new(),remote_launch:Default::default(),login_path:None,command:Some(command),argv:None,shell:Some("/bin/sh".into()),cwd:"/home/dev".into(),cols:240,rows:40,shell_integration:false,starship_preset:None,api_port:None,env_remove:vec![],env:vec![]},registration(session.as_str(),None,false)).unwrap();
        client.claim_writable_typed(&session).unwrap();
        let wait_for=|marker:&str| {
            let deadline=std::time::Instant::now()+std::time::Duration::from_secs(45);
            loop {
                let output=codesense_pty_core::domain::visible_text(&*client,&session,u32::MAX).unwrap().text;
                assert!(!output.contains(&secret),"desktop credential appeared in remote output");
                if output.contains(marker){break;}
                assert!(std::time::Instant::now()<deadline,"missing {marker}: {output}");
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        };
        wait_for("MCP_RELAY_READY_REVOKE");
        assert_eq!(opened.load(std::sync::atomic::Ordering::SeqCst),10,"forbidden selector opened downstream");
        assert_eq!(std::fs::read_to_string(&mutation).unwrap(),"effect\n","ambiguous mutation replayed");
        assert_eq!(http.initializes.load(std::sync::atomic::Ordering::SeqCst),4,"HTTP session silently reinitialized");
        assert_eq!(http.expired_mutations.load(std::sync::atomic::Ordering::SeqCst),1,"expired mutation replayed");
        assert_eq!(http.lost_mutations.load(std::sync::atomic::Ordering::SeqCst),1,"lost mutation replayed");
        private_setup::set_remote_mcp_control(&environment,&saved.machine_id,false).unwrap();
        client.write(&session,b"continue\n").unwrap();
        wait_for("MCP_RELAY_SSH_PASS");
        client.kill(&session).unwrap();
        manager.disconnect(&saved.machine_id).await.unwrap();
        println!("MCP_RELAY_SSH_PASS selected=7 desktop_stdio_http_auth=true prompts_resources=true forbidden_refused=true no_mutation_replay=true revoked=true");
    });
}
