// SPDX-License-Identifier: MIT
//! Owned selected-profile fixture shared by official Codex and Claude CLI proofs.
use crate::install::agent_account_store::AgentProvider;
use crate::mcp::local_mcp_relay::native::Projection;
use serde_json::json;
use std::path::Path;
pub(super) fn prepare(
    root: &Path,
    cwd: &Path,
    provider: AgentProvider,
) -> Result<Projection, String> {
    let local_server = root.join("local-profile-server.py");
    std::fs::write(&local_server, r#"import sys,json,os,pathlib
root=pathlib.Path(sys.argv[1])
(root/'local-profile.pid').write_text(str(os.getpid()))
for line in sys.stdin:
 req=json.loads(line)
 if 'id' not in req: continue
 method=req.get('method')
 if method=='initialize': result={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'owned-local-profile','version':'1'}}
 elif method=='tools/list': result={'tools':[{'name':name,'description':'Owned fixture tool','inputSchema':{'type':'object','properties':{}}} for name in ['local_ping','denied_tool']]}
 elif method=='tools/call':
  name=req['params']['name']
  if name=='local_ping':
   assert os.environ.get('AUTH1_LOCAL_MCP_CREDENTIAL')=='owned-local-only-sentinel'
   (root/'local-profile-called').write_text('local credential consumed')
  else: (root/'local-profile-denied-called').write_text('forbidden')
  result={'content':[{'type':'text','text':'owned local profile completed'}]}
 else: result={}
 print(json.dumps({'jsonrpc':'2.0','id':req['id'],'result':result}),flush=True)
"#).map_err(|e|e.to_string())?;
    let grant = crate::mcp::local_mcp_relay::config::parse(
        &json!({"mcpServers":{"fixture_local":{"command":"python3","args":[local_server,root],"env":{"AUTH1_LOCAL_MCP_CREDENTIAL":"${FIXTURE_AUTH}"},"enabled_tools":["local_ping","denied_tool"],"disabled_tools":["denied_tool"]}}}),
        "mcpServers",
        "unused",
        Some(&["fixture_local".into()]),
        cwd,
        |key| (key == "FIXTURE_AUTH").then(|| "owned-local-only-sentinel".to_string()),
    )?;
    crate::mcp::local_mcp_relay::native::project(&grant, provider)
}
pub(super) async fn verify(root: &Path) -> Result<(), String> {
    let pid = std::fs::read_to_string(root.join("local-profile.pid"))
        .map_err(|e| e.to_string())?
        .trim()
        .parse::<i32>()
        .map_err(|e| e.to_string())?;
    let retired = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            // The PID was written by our owned child; signal zero only observes it.
            if unsafe { libc::kill(pid, 0) } == -1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .is_ok();
    if !retired
        || std::fs::read_to_string(root.join("local-profile-called"))
            .ok()
            .as_deref()
            != Some("local credential consumed")
        || root.join("local-profile-denied-called").exists()
    {
        return Err("selected local MCP profile call/policy/retirement proof incomplete".into());
    }
    Ok(())
}
