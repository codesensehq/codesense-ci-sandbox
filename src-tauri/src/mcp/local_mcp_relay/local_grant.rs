// SPDX-License-Identifier: MIT
//! Local launch lifetime and ingress for the existing captured relay policy.
//! A provider home is never copied or changed by this adapter.
use super::{config::Grant, Validate};
use codesense_host_proto::scoped_mcp::{self as wire, Descriptor, Hello};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(crate) fn start(
    directory: &Path,
    mut grant: Grant,
    aliases: std::collections::BTreeMap<String, String>,
    validate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
) -> Result<Descriptor, String> {
    use std::os::unix::fs::PermissionsExt;
    validate()?;
    static LAUNCHES: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    let launch_slot = LAUNCHES
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(64)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| "too many prepared local MCP launches; close an unused managed session")?;
    // macOS sockaddr_un is short; the ordinary per-session config path can
    // exceed it. This RAII-owned 0700 directory contains only the socket.
    let socket_dir = tempfile::Builder::new()
        .prefix("csm-")
        .tempdir_in("/tmp")
        .map_err(|_| "cannot allocate private local MCP socket directory")?;
    let path = socket_dir.path().join("s");
    let listener = std::os::unix::net::UnixListener::bind(&path)
        .map_err(|_| "cannot bind private local MCP relay (launch path may be too long)")?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| "cannot protect local MCP relay")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "cannot configure local MCP relay")?;
    let descriptor = Descriptor {
        version: wire::VERSION,
        socket: path,
        capability: format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ),
    };
    let capability = descriptor.capability.clone();
    let session = directory
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("local MCP scope has no session")?;
    // This is only a hint; the receiver proves identity from the spawned child.
    if let Some(super::config::Server {
        endpoint: super::config::Endpoint::Stdio { env, .. },
        ..
    }) = grant.get_mut("codesense")
    {
        env.insert("CODESENSE_TAB_SESSION_ID".into(), session.into());
        env.remove("CODESENSE_MCP_LAUNCH_TOKEN");
    }
    let identity =
        super::identity::Identity::new(directory, session, capability.clone(), validate.clone());
    crate::commands::mcp_scope::bind_relay_identity(session, &identity);
    tauri::async_runtime::spawn(async move {
        let _socket_dir = socket_dir;
        let _launch_slot = launch_slot;
        let Ok(listener) = tokio::net::UnixListener::from_std(listener) else {
            return;
        };
        let slots = Arc::new(tokio::sync::Semaphore::new(32));
        let grant = Arc::new(grant);
        let aliases = Arc::new(aliases);
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                _ = tick.tick() => if validate().is_err() { break; },
                result = listener.accept() => {
                    let Ok((stream, _)) = result else { break };
                    let Ok(permit) = slots.clone().try_acquire_owned() else { continue };
                    let grant = grant.clone();
                    let capability = capability.clone();
                    let aliases = aliases.clone();
                    let validate = validate.clone();
                    let identity = identity.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let _ = serve_connection(stream, &capability, grant, aliases, validate, identity).await;
                    });
                }
            }
        }
        // Directory cleanup belongs to ScopeCreation; do not unlink a replacement.
    });
    Ok(descriptor)
}

async fn serve_connection(
    mut stream: tokio::net::UnixStream,
    capability: &str,
    grant: Arc<Grant>,
    aliases: Arc<std::collections::BTreeMap<String, String>>,
    validate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
    identity: Arc<super::identity::Identity>,
) -> Result<(), String> {
    let (server, canonical, ingress) = tokio::time::timeout(Duration::from_secs(2), async {
        let length = stream
            .read_u32()
            .await
            .map_err(|_| "relay handshake closed")? as usize;
        if length > wire::MAX_HANDSHAKE {
            return Err("relay handshake exceeds limit");
        }
        let mut bytes = vec![0; length];
        stream
            .read_exact(&mut bytes)
            .await
            .map_err(|_| "relay handshake closed")?;
        let hello: Hello = serde_json::from_slice(&bytes).map_err(|_| "invalid relay handshake")?;
        if hello.version != wire::VERSION || hello.capability != capability {
            return Err("local MCP launch capability rejected");
        }
        validate().map_err(|_| "local MCP launch retired")?;
        let server = grant
            .get(&hello.server)
            .cloned()
            .ok_or("MCP server was not selected")?;
        let ingress = if hello.server == "codesense" {
            let caller = codesense_pty_core::activity::peer::of_socket(&stream)
                .ok_or("MCP relay ingress has no kernel peer")?;
            let proof = identity.clone();
            let verified = tokio::task::spawn_blocking(move || proof.ingress(caller))
                .await
                .map_err(|_| "cannot verify MCP relay ingress")?;
            if !verified {
                return Err("MCP relay ingress has no verified launch identity");
            }
            Some((identity.clone(), caller))
        } else {
            None
        };
        stream
            .write_all(&[1])
            .await
            .map_err(|_| "relay handshake closed")?;
        Ok((server, hello.server, ingress))
    })
    .await
    .map_err(|_| "relay handshake timed out")??;
    let valid: Validate = Arc::new(move || {
        let result = validate();
        Box::pin(async move { result })
    });
    let (relay, mut forwarding) = tokio::io::duplex(64 * 1024);
    let note = aliases.get(&canonical).map(|alias| format!(
        "CodeSense selected MCP server '{canonical}' is exposed in namespace '{alias}'. References to mcp__{canonical}__<tool> mean mcp__{alias}__<tool> in this launch. Use the exposed tools; original tool restrictions still apply."
    ));
    let serve = super::transport::serve_with_namespace(server, relay, valid, note, ingress);
    let copy = tokio::io::copy_bidirectional(&mut stream, &mut forwarding);
    tokio::select! { _ = serve => {}, _ = copy => {} }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncBufReadExt, BufReader};

    #[tokio::test]
    async fn review_r2_a_valid_victim_descriptor_cannot_authorize_an_unrelated_ingress() {
        unproven_ingress_is_refused(true).await;
    }

    #[tokio::test]
    async fn review_r2_a_valid_victim_descriptor_before_root_attachment_is_refused() {
        unproven_ingress_is_refused(false).await;
    }

    async fn unproven_ingress_is_refused(attach_root: bool) {
        use codesense_pty_core::activity::peer;
        let scope = tempfile::Builder::new()
            .prefix("ingress-victim-")
            .tempdir_in("/tmp")
            .unwrap();
        let session = scope.path().file_name().unwrap().to_str().unwrap();
        let marker = scope.path().join("deputy-spawned");
        let mut victim = tokio::process::Command::new("/bin/sleep")
            .arg("60")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        if attach_root {
            crate::commands::mcp_scope::bind_fixture_relay_root(
                session,
                peer::inspect(victim.id().unwrap()).unwrap(),
            );
        }
        let script = "import pathlib,sys;pathlib.Path(sys.argv[1]).write_text('victim identity');sys.stdin.read()";
        let grant = super::super::config::parse_local_scope(&serde_json::json!({"mcp_servers":{"codesense":{"command":"/usr/bin/python3","args":["-c",script,marker]}}}), "mcp_servers", scope.path()).unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let flag = active.clone();
        let descriptor = start(
            scope.path(),
            grant,
            Default::default(),
            Arc::new(move || {
                if flag.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err("retired".into())
                }
            }),
        )
        .unwrap();
        wire::write_descriptor(&scope.path().join("local-relay.json"), &descriptor).unwrap();
        let mut client = tokio::net::UnixStream::connect(&descriptor.socket)
            .await
            .unwrap();
        let hello = serde_json::to_vec(&Hello {
            version: wire::VERSION,
            capability: descriptor.capability,
            server: "codesense".into(),
        })
        .unwrap();
        client.write_u32(hello.len() as u32).await.unwrap();
        client.write_all(&hello).await.unwrap();
        let answer = tokio::time::timeout(Duration::from_secs(3), client.read_u8())
            .await
            .unwrap();
        active.store(false, Ordering::SeqCst);
        drop(client);
        victim.kill().await.unwrap();
        assert!(answer.is_err(), "a valid victim descriptor cannot authorize the unrelated connecting process: {answer:?}");
        assert!(
            !marker.exists(),
            "an unproven ingress must not spawn a session-speaking deputy"
        );
    }

    #[tokio::test]
    async fn identity_captured_endpoint_policy_and_retirement_are_enforced() {
        let root = tempfile::tempdir().unwrap();
        let script = root.path().join("fixture.py");
        std::fs::write(
            &script,
            r#"import sys,json,os
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 if r['method']=='tools/list':
  result={'tools':[{'name':x,'inputSchema':{'type':'object'}} for x in ['safe','denied']]}
 else: result={'content':[{'type':'text','text':os.environ['CANONICAL_SENTINEL']}], '_meta':{'session':os.environ.get('CODESENSE_TAB_SESSION_ID'), 'token':os.environ.get('CODESENSE_MCP_LAUNCH_TOKEN')}}
 print(json.dumps({'jsonrpc':'2.0','id':r['id'],'result':result}),flush=True)
"#,
        )
        .unwrap();
        let config = serde_json::json!({"mcp_servers":{"codesense":{"command":"/usr/bin/python3", "args":[script],
            "env":{"CANONICAL_SENTINEL":"captured-original"},"enabled_tools":["safe"]}}});
        let grant =
            super::super::config::parse_local_scope(&config, "mcp_servers", root.path()).unwrap();
        assert!(grant.contains_key("codesense"));
        let active = Arc::new(AtomicBool::new(true));
        let validate = {
            let active = active.clone();
            Arc::new(move || {
                if active.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err("retired".into())
                }
            })
        };
        let descriptor = start(root.path(), grant, Default::default(), validate).unwrap();
        crate::commands::mcp_scope::bind_fixture_relay_root(
            root.path().file_name().unwrap().to_str().unwrap(),
            codesense_pty_core::activity::peer::inspect(std::process::id()).unwrap(),
        );
        let mut stream = tokio::net::UnixStream::connect(&descriptor.socket)
            .await
            .unwrap();
        let hello = serde_json::to_vec(&Hello {
            version: wire::VERSION,
            capability: descriptor.capability.clone(),
            server: "codesense".into(),
        })
        .unwrap();
        stream.write_u32(hello.len() as u32).await.unwrap();
        stream.write_all(&hello).await.unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), 1);
        let mut stream = BufReader::new(stream);
        for (id, method, params) in [
            (1, "tools/list", serde_json::json!({})),
            (
                2,
                "tools/call",
                serde_json::json!({"name":"denied","arguments":{}}),
            ),
            (
                3,
                "tools/call",
                serde_json::json!({"name":"safe","arguments":{}}),
            ),
        ] {
            let request =
                serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
            stream
                .get_mut()
                .write_all(format!("{request}\n").as_bytes())
                .await
                .unwrap();
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(3), stream.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
            match id {
                1 => assert_eq!(reply["result"]["tools"].as_array().unwrap().len(), 1),
                2 => assert_eq!(reply["error"]["code"], -32602),
                _ => {
                    assert_eq!(reply["result"]["content"][0]["text"], "captured-original");
                    assert!(reply["result"]["_meta"]["token"].is_null());
                    assert_eq!(
                        reply["result"]["_meta"]["session"],
                        root.path().file_name().unwrap().to_str().unwrap()
                    );
                }
            }
        }
        active.store(false, Ordering::SeqCst);
        let mut line = String::new();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), stream.read_line(&mut line))
                .await
                .unwrap()
                .unwrap(),
            0
        );
    }
}
