// SPDX-License-Identifier: MIT
//! Opt-in native CLI proofs. Each uses the caller's existing provider home unchanged or
//! a fresh empty one; no hook trust bypass, auth copying or installed-app replacement.
use super::*;

#[test]
#[ignore = "requires explicitly selected installed Codex and freshly built bridge"]
fn installed_codex_uses_canonical_relay_and_original_home() {
    let _home_env = crate::routines::test_support::TempHome::new();
    use crate::shared::proc_capture::{spawn_capture, CaptureRequest};
    let binary = PathBuf::from(
        std::env::var_os("CODESENSE_SCOPE_TEST_CODEX").expect("explicit Codex binary"),
    );
    let bridge = PathBuf::from(
        std::env::var_os("CODESENSE_SCOPE_TEST_BRIDGE").expect("fresh bridge binary"),
    );
    let home = PathBuf::from(
        std::env::var_os("CODESENSE_SCOPE_TEST_HOME").expect("original provider home"),
    );
    assert!(binary.is_file() && bridge.is_file() && home.is_dir());
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("workspace");
    std::fs::create_dir_all(cwd.join(".codex")).unwrap();
    let marker = root.path().join("called");
    let script = root.path().join("server.py");
    std::fs::write(&script,r#"import sys,json,os
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r:continue
 if r['method']=='initialize':
  result={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'scope-fixture','version':'1'},'instructions':'Use scope_probe only for this harmless fixture.'}
 elif r['method']=='tools/list':
  result={'tools':[{'name':name,'description':'Read the harmless fixture scope marker.','inputSchema':{'type':'object','properties':{}},'annotations':{'readOnlyHint':True,'destructiveHint':False,'openWorldHint':False}} for name in ['scope_probe','denied_probe']]}
 elif r['method']=='tools/call':
  assert r['params']['name']=='scope_probe'
  with open(os.environ['FIXTURE_MARKER'],'a') as f:f.write('canonical-called\n')
  result={'content':[{'type':'text','text':'CANONICAL_SCOPE_CONFIRMED'}]}
 else:result={}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#).unwrap();
    let canonical = serde_json::json!({"mcp_servers":{"scope_fixture":{"command":"/usr/bin/python3","args":[script],"env":{"FIXTURE_MARKER":marker},"enabled_tools":["scope_probe"]}}});
    let config = crate::shared::config_format::ConfigFormat::Toml
        .serialize(&canonical)
        .unwrap();
    std::fs::write(cwd.join(".codex/config.toml"), config).unwrap();
    let watched = ["hooks.json", "config.toml"];
    let before: Vec<_> = watched
        .iter()
        .map(|name| std::fs::read(home.join(name)).ok())
        .collect();
    let session = format!("native-scope-{}", uuid::Uuid::new_v4());
    struct Cleanup(String);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            remove_scope_config(&self.0);
        }
    }
    let _cleanup = Cleanup(session.clone());
    let launch = LaunchCli {
        runtime: None,
        binary: &binary,
        args: &[],
        bridge: Some(&bridge),
        version: None,
        attribution_hook_trust: true,
    };
    let scope =
        codesense_pty_core::agent_cli::descriptor(codesense_pty_core::agent_cli::AgentCli::Codex)
            .mcp_scope
            .as_ref()
            .unwrap();
    let (outcome, _) = project(
        &session,
        &cwd,
        &["scope_fixture".into()],
        None,
        None,
        scope,
        scope.inventory.as_ref().unwrap(),
        Some(&home),
        &launch,
        &str::to_string,
    )
    .unwrap();
    let mut args = vec![
        "--sandbox".into(),
        "read-only".into(),
        "exec".into(),
        "--skip-git-repo-check".into(),
    ];
    args = compose_args(args, outcome.extra_args);
    args.push("Call the exposed MCP scope_probe tool exactly once. It is the canonical fixture tool (canonical prefix mcp__scope_fixture__). Do not use shell or filesystem tools. Reply with its result.".into());
    let _lease = retain_detached_scope(&session).unwrap();
    bind_fixture_relay_root(
        &session,
        codesense_pty_core::activity::peer::inspect(std::process::id()).unwrap(),
    );
    let output = spawn_capture(CaptureRequest {
        program: binary,
        args,
        cwd: Some(cwd),
        env: outcome.extra_env,
        env_remove: Vec::new(),
        stdin: None,
        timeout: Duration::from_secs(90),
        process_group: true,
        pid_sink: None,
        output_sink: None,
        last_drain_ms: None,
        max_output_bytes: None,
        cancel: None,
    })
    .unwrap();
    // Never print the native account's output/configuration: only controlled facts.
    assert!(!output.timed_out, "native Codex fixture timed out");
    assert!(
        output.status.is_some_and(|status| status.success()),
        "native Codex fixture failed (output intentionally not logged)"
    );
    assert_eq!(
        std::fs::read_to_string(marker).unwrap(),
        "canonical-called\n"
    );
    let after: Vec<_> = watched
        .iter()
        .map(|name| std::fs::read(home.join(name)).ok())
        .collect();
    assert!(before == after, "native provider hook/config files changed");
    println!("native Codex canonical relay call PASS; original home/config/hooks retained; no hook trust bypass; user hook execution is a separate oracle");
}

/// Starts a thread on the installed Codex app server (no model turn) with the
/// launch's real server entries: a team server that never answers must not hold the
/// thread up, and a bridge that cannot start must refuse it, naming the bridge.
#[test]
#[ignore = "requires explicitly selected installed Codex"]
fn installed_codex_waits_only_for_the_bridge() {
    use std::io::{BufRead, BufReader, Write};
    let binary = PathBuf::from(
        std::env::var_os("CODESENSE_SCOPE_TEST_CODEX").expect("explicit Codex binary"),
    );
    let root = tempfile::tempdir().unwrap();
    let (home, cwd) = (root.path().join("home"), root.path().join("workspace"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&cwd).unwrap();
    // Stands in for the bridge: answers as the bridge (marking `bridge-up`), never answers
    // as a team server (marking `team-up`), and exits at once as the bridge once `broken` exists (Codex
    // passes servers no env). Every copy exits when Codex closes its stdin.
    let bridge = root.path().join("bridge.py");
    std::fs::write(&bridge, r#"#!/usr/bin/python3
import sys,json,os
here=os.path.dirname(sys.argv[0])
if sys.argv[-1]!='codesense':open(os.path.join(here,'team-up'),'w').close();sys.stdin.read();sys.exit(0)
if os.path.exists(os.path.join(here,'broken')):sys.exit(1)
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r:continue
 if r['method']=='initialize':open(os.path.join(here,'bridge-up'),'w').close()
 result={'protocolVersion':'2024-11-05','capabilities':{'tools':{}},'serverInfo':{'name':'bridge-fixture','version':'1'}} if r['method']=='initialize' else {'tools':[]} if r['method']=='tools/list' else {}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#).unwrap();
    std::fs::set_permissions(&bridge, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let scope =
        codesense_pty_core::agent_cli::descriptor(codesense_pty_core::agent_cli::AgentCli::Codex)
            .mcp_scope
            .as_ref()
            .unwrap();
    assert_eq!(
        scope.bridge_id, "codesense",
        "the fixture answers as `codesense`"
    );
    let inventory = scope.inventory.as_ref().unwrap();
    let inherited = McpListing::new();
    let aliases = aliases(
        ["codesense".into(), "cloudflare".into()].into_iter(),
        &inherited,
    )
    .unwrap();
    let entries = override_entries(
        inventory,
        &inherited,
        &aliases,
        &scope.bridge_id,
        None,
        &bridge,
        Path::new("/fixture/descriptor"),
    )
    .unwrap();
    let start_thread = |broken: bool| {
        let mut command = std::process::Command::new(&binary);
        command
            .args(inventory.override_argv(&scope.server_map_key, &entries, str::to_owned))
            .arg("app-server")
            .current_dir(&cwd)
            .env("CODEX_HOME", &home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let marker = root.path().join("broken");
        if broken {
            std::fs::write(&marker, "").unwrap();
        } else {
            let _ = std::fs::remove_file(&marker);
        }
        for started in ["bridge-up", "team-up"] {
            let _ = std::fs::remove_file(root.path().join(started));
        }
        let mut child = command.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        for request in [
            serde_json::json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"scope-test","version":"0"}}}),
            serde_json::json!({"method":"initialized"}),
            serde_json::json!({"id":2,"method":"thread/start","params":{"cwd":cwd}}),
        ] {
            writeln!(stdin, "{request}").unwrap();
        }
        let started = std::time::Instant::now();
        let (sender, receiver) = std::sync::mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(reply) = serde_json::from_str::<Value>(&line) {
                    if reply["id"] == 2 && reply.get("method").is_none() {
                        let _ = sender.send(reply);
                        return;
                    }
                }
            }
        });
        let reply = receiver.recv_timeout(Duration::from_secs(60));
        let elapsed = started.elapsed();
        // The team server may be started after the reply; give it a moment to show up.
        let team_up = root.path().join("team-up");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !team_up.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
        (reply.expect("Codex never answered thread/start"), elapsed)
    };
    let (reply, elapsed) = start_thread(false);
    assert!(reply.get("result").is_some(), "thread refused: {reply}");
    assert!(
        root.path().join("bridge-up").exists(),
        "the bridge never started"
    );
    assert!(
        root.path().join("team-up").exists(),
        "the team server was never started"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "thread waited {elapsed:?} for a team server that never answers"
    );
    let (reply, elapsed) = start_thread(true);
    assert!(
        elapsed < Duration::from_secs(10),
        "refusal took {elapsed:?}"
    );
    let error = reply["error"]["message"].as_str().unwrap_or_default();
    assert!(
        error.contains("required MCP servers failed to initialize")
            && error.contains(&aliases["codesense"])
            && !error.contains(&aliases["cloudflare"]),
        "{reply}"
    );
}
