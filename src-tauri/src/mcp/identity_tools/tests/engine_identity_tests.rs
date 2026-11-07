// SPDX-License-Identifier: MIT
#![cfg(test)]
use super::*;
use crate::mcp::{
    handler::{initialize_session, WhoAmIParams},
    local_identity,
};
use codesense_pty_core::activity::{peer, ActivityOwner, ActivityService};
use codesense_pty_core::agent_cli::AgentCli;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

// The keeper exits only after initialize/first call. The same live gateway and
// bridge then move outside the still-live CLI's ancestry: the call-time race.
// Every wait is a blocking read on a pipe this test owns: no polling, no sleeping. The hook's
// own deadline and socket bound are generous and test-owned; the host still caps its receive.
//   exit:   test -> keeper, whose exit detaches the gateway and bridge from the CLI
//   hold:   test -> cli and gateway, which stay alive until it closes
//   after:  test -> bridge, which then makes its second call
//   report: cli and bridge -> test, in the order the test sequences them
const CLIENT: &str = r#"
import json,os,socket,struct,subprocess,sys,time
cfg=json.loads(sys.argv[1]); role=sys.argv[2]
def report(**row): os.write(cfg['report'],(json.dumps(row)+'\n').encode())
if role!='bridge':
 child=subprocess.Popen([sys.executable,'-c',sys.argv[3],sys.argv[1],{'cli':'keeper','keeper':'gateway','gateway':'bridge'}[role],sys.argv[3]],pass_fds=cfg['fds'])
 if role=='keeper': os.read(cfg['exit'],1); sys.exit(0)
 if role=='cli': child.wait(); report(keeper='reaped')
 os.read(cfg['hold'],1); sys.exit(0)
s=socket.socket(socket.AF_UNIX); s.settimeout(10); s.connect(cfg['mcp']); f=s.makefile('rwb',buffering=0)
def hook(phase):
 act=json.loads(open(cfg['activity']).read())
 report_=dict(provider='codex',event='started',scope='main',reportId=phase,providerSession='gateway-main',turn=None,tool=None)
 raw=json.dumps(dict(token=act['token'],deadline_ns=time.clock_gettime_ns(time.CLOCK_MONOTONIC)+10000000000,report=report_)).encode()
 with socket.socket(socket.AF_UNIX) as observer:
  observer.settimeout(10); observer.connect(act['socket']); observer.sendall(struct.pack('>I',len(raw))+raw)
  report(phase=phase,hook=observer.recv(1).decode())
def call(i,method,params):
 f.write(json.dumps({'jsonrpc':'2.0','id':i,'method':method,'params':params}).encode()+b'\n')
 return json.loads(f.readline())
call(1,'initialize',{'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'fixture','version':'1'},'_meta':{'codesenseSessionId':'hermes-session'}})
hook('before')
report(phase='before',response=call(2,'tools/call',{'name':'who_am_i','arguments':{}}))
os.read(cfg['after'],1)
hook('after')
report(phase='after',response=call(3,'tools/call',{'name':'who_am_i','arguments':{}}))
"#;

struct Group(tokio::process::Child);
impl Drop for Group {
    fn drop(&mut self) {
        // This test created and retains the live group leader; no PID/group rediscovery.
        if let Some(pid) = self.0.id() {
            unsafe {
                libc::killpg(pid as i32, libc::SIGKILL);
            }
        }
        let _ = self.0.start_kill();
    }
}

/// A pipe whose far end the child processes inherit; our end is a tokio stream.
fn pipe(inherited: &mut Vec<std::os::fd::OwnedFd>) -> (tokio::net::UnixStream, i32) {
    use std::os::fd::{AsRawFd, OwnedFd};
    let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    ours.set_nonblocking(true).unwrap();
    let theirs = OwnedFd::from(theirs);
    // Survives exec; ours stays close-on-exec so only this test holds the other end.
    assert_eq!(
        unsafe { libc::fcntl(theirs.as_raw_fd(), libc::F_SETFD, 0) },
        0
    );
    let fd = theirs.as_raw_fd();
    inherited.push(theirs);
    (tokio::net::UnixStream::from_std(ours).unwrap(), fd)
}

async fn reported(report: &mut BufReader<tokio::net::UnixStream>) -> Value {
    let mut line = String::new();
    report.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn gateway_bridge_who_am_i_checks_live_ancestry_again_after_initialize() {
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        for detach in [false, true] {
            let home = tempfile::tempdir_in("/tmp").unwrap();
            std::fs::set_permissions(home.path(), std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
            let path = home.path().join("mcp.sock");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            // Codex is a listed wire identity for this neutral ingress proof. The
            // trust check is shared; Hermes remains rejected as a fresh public
            // launch until its own independent listing gate passes.
            let owner = Arc::new(ActivityOwner::new());
            let service = ActivityService::start(home.path(), &owner).unwrap();
            let writer = owner.prepare_native("gateway-hook", AgentCli::Codex, service.path().to_str().unwrap()).unwrap();
            let activity = home.path().join("activity.json");
            std::fs::write(&activity, serde_json::to_vec(&json!({"socket":service.path(),"token":writer.token})).unwrap()).unwrap();
            let mut inherited = Vec::new();
            let (exit, exit_fd) = pipe(&mut inherited);
            let (_hold, hold_fd) = pipe(&mut inherited);
            let (mut after, after_fd) = pipe(&mut inherited);
            let (report, report_fd) = pipe(&mut inherited);
            let config = json!({
                "mcp":path, "activity":activity, "exit":exit_fd, "hold":hold_fd,
                "after":after_fd, "report":report_fd, "fds":[exit_fd, hold_fd, after_fd, report_fd],
            });
            let mut command = tokio::process::Command::new("/usr/bin/python3");
            command.args(["-c",CLIENT]).arg(config.to_string()).args(["cli",CLIENT])
                .env_clear().env("HOME",home.path()).env("PATH","/usr/bin:/bin")
                .current_dir(home.path()).process_group(0).kill_on_drop(true);
            let group = Group(command.spawn().unwrap());
            // Only the processes hold the far ends now: our closing is their EOF.
            drop(inherited);
            let mut report = BufReader::new(report);
            let mut exit = exit;
            let root = peer::inspect(group.0.id().unwrap()).unwrap();
            owner.register("gateway-hook", Some(root.pid()));
            let (socket, _) = listener.accept().await.unwrap();
            let caller = peer::of_socket(&socket).unwrap();
            let mut wire = BufReader::new(socket);
            let mut line = String::new();
            wire.read_line(&mut line).await.unwrap();
            let initialize: rmcp::model::ClientJsonRpcMessage = serde_json::from_str(&line).unwrap();
            let rmcp::model::ClientJsonRpcMessage::Request(initialize) = initialize else { panic!("initialize") };
            use rmcp::model::GetMeta;
            let state = RwLock::new(None);
            let captured = local_identity::capture(initialize_session(None, initialize.request.get_meta()),Some(caller),local_identity::Lookup::Root(root)).unwrap();
            local_identity::note_initialize(&state,captured);
            wire.get_mut().write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n").await.unwrap();
            let rows = [row("hermes-worker","hermes-session","Hermes Agent",SessionState::Running)];
            for after_detach in [false,true] {
                if after_detach {
                    if detach {
                        // The keeper exits; its parent, the CLI, reaps it: by then the
                        // gateway and the bridge have moved out of the CLI's ancestry.
                        exit.write_all(b"x").await.unwrap();
                        assert_eq!(reported(&mut report).await, json!({"keeper":"reaped"}));
                        assert!(!peer::descends_from(caller,root),"the bridge has left the CLI's tree");
                    }
                    assert_eq!(peer::inspect(root.pid()),Some(root),"the tab's root stays alive");
                    assert_eq!(peer::inspect(caller.pid()),Some(caller),"the initialized bridge is the same live process");
                    after.write_all(b"x").await.unwrap();
                }
                line.clear(); wire.read_line(&mut line).await.unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                let phase = if after_detach { "after" } else { "before" };
                let hook = reported(&mut report).await;
                assert_eq!(hook["phase"],phase);
                assert_eq!(hook["hook"] == "A", !after_detach || !detach, "hook admission checks call-time ancestry too");
                assert_eq!(request["params"]["name"],"who_am_i");
                assert_eq!(request["params"]["arguments"],json!({}),"no identity argument supplied");
                // These are the production call-time capture, argument binding and row
                // lookup. Only session-list DATA is a fixture; identity uses kernel peers.
                let connection = local_identity::stored_session(&state);
                let binding = local_identity::stored(&state).unwrap();
                let result = local_identity::revalidate(&binding,Some(caller),local_identity::Lookup::Root(root)).and_then(|_| {
                    let mut args = serde_json::Map::new();
                    local_identity::arguments("who_am_i",&mut args,connection.as_deref(),None)?;
                    let params: WhoAmIParams = serde_json::from_value(args.into()).unwrap();
                    let session = local_identity::session(params.session_id.as_deref(),connection.as_deref())?;
                    Ok(serde_json::to_value(who_am_i_in(&rows,session).unwrap()).unwrap())
                });
                let response = match result {
                    Ok(me) => { assert!(!after_detach || !detach); assert_eq!(me["handle"],"hermes-worker"); json!({"jsonrpc":"2.0","id":request["id"],"result":me}) }
                    Err(error) => { assert!(after_detach && detach); assert!(error.message.contains("no verified tab identity")); json!({"jsonrpc":"2.0","id":request["id"],"error":{"message":error.message}}) }
                };
                wire.get_mut().write_all(format!("{response}\n").as_bytes()).await.unwrap();
                // The bridge saw exactly the answer production gave it.
                let seen = reported(&mut report).await;
                assert_eq!((seen["phase"].as_str(), seen["response"]["id"].clone()),(Some(phase),request["id"].clone()));
            }
            drop(group);
        }
    }).await.expect("bounded engine/bridge identity journey");
}
