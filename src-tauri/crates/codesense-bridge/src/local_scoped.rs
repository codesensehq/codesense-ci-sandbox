// SPDX-License-Identifier: MIT
//! Reconnect only through the captured launch descriptor; never through unscoped MCP.
use codesense_host_proto::scoped_mcp::{self as wire, Hello};
use std::{ffi::OsString, path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) async fn run(args: &[OsString]) -> Result<(), String> {
    let [flag, descriptor, selector, server] = args else {
        return Err("usage: --local-scope <descriptor> --server-id <canonical-id>".into());
    };
    if flag != "--local-scope" || selector != "--server-id" {
        return Err("invalid local MCP bridge arguments".into());
    }
    let server = server.to_str().ok_or("invalid local MCP server identity")?;
    if server.is_empty() || server.len() > 256 {
        return Err("invalid local MCP server identity".into());
    }
    super::run_local(super::Endpoint::Scoped {
        descriptor: descriptor.into(),
        server: server.into(),
    })
    .await;
    Ok(())
}

pub(super) async fn connect(path: &Path, server: &str) -> Result<tokio::net::UnixStream, String> {
    let descriptor = wire::read_descriptor(path)?;
    let hello = serde_json::to_vec(&Hello {
        version: wire::VERSION,
        capability: descriptor.capability,
        server: server.into(),
    })
    .map_err(|_| "cannot encode local MCP handshake")?;
    if hello.len() > wire::MAX_HANDSHAKE {
        return Err("local MCP handshake exceeds limit".into());
    }
    let stream = tokio::time::timeout(Duration::from_secs(2), async {
        let mut stream = tokio::net::UnixStream::connect(descriptor.socket)
            .await
            .map_err(|_| "local MCP launch is unavailable")?;
        stream
            .write_u32(hello.len() as u32)
            .await
            .map_err(|_| "local MCP handshake closed")?;
        stream
            .write_all(&hello)
            .await
            .map_err(|_| "local MCP handshake closed")?;
        if stream
            .read_u8()
            .await
            .map_err(|_| "local MCP handshake closed")?
            != 1
        {
            return Err("local MCP launch was rejected");
        }
        Ok(stream)
    })
    .await
    .map_err(|_| "local MCP handshake timed out")??;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::{mpsc, oneshot};

    async fn admit(listener: &UnixListener, capability: &str) -> BufReader<UnixStream> {
        let (mut stream, _) = listener.accept().await.unwrap();
        let size = stream.read_u32().await.unwrap();
        let mut bytes = vec![0; size as usize];
        stream.read_exact(&mut bytes).await.unwrap();
        let hello: Hello = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(hello.capability, capability);
        assert_eq!(hello.server, "codesense");
        stream.write_all(&[1]).await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut init = String::new();
        stream.read_line(&mut init).await.unwrap();
        let init: serde_json::Value = serde_json::from_str(&init).unwrap();
        assert_eq!(init["method"], "initialize");
        assert_eq!(init["params"]["_meta"]["codesenseSessionId"], "session");
        stream
            .get_mut()
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n")
            .await
            .unwrap();
        let mut initialized = String::new();
        stream.read_line(&mut initialized).await.unwrap();
        assert!(initialized.contains("notifications/initialized"));
        stream
    }

    #[tokio::test]
    async fn reconnect_uses_the_replaced_scoped_descriptor_and_never_replays_a_call() {
        let root = std::env::temp_dir().join(format!("cbr-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("relay.json");
        let first = root.join("one");
        let second = root.join("two");
        let listener = UnixListener::bind(&first).unwrap();
        let descriptor = wire::Descriptor {
            version: wire::VERSION,
            socket: first,
            capability: "a".repeat(64),
        };
        wire::write_descriptor(&path, &descriptor).unwrap();
        let (input, stdin) = mpsc::channel(16);
        let (stdout, mut output) = mpsc::channel(16);
        let bridge = tokio::spawn(crate::run_channel(
            crate::Endpoint::Scoped {
                descriptor: path.clone(),
                server: "codesense".into(),
            },
            stdin,
            stdout,
            Some("session".into()),
        ));
        input
            .send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.into())
            .await
            .unwrap();
        input
            .send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#.into())
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            let mut stream = admit(&listener, &"a".repeat(64)).await;
            let mut call = String::new();
            stream.read_line(&mut call).await.unwrap();
            assert!(call.contains("send_message"));
            // The call may have run, but its response is lost with the app.
        });
        output.recv().await.unwrap();
        input.send(r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"send_message","arguments":{"requestId":"mail-1"}}}"#.into()).await.unwrap();
        server.await.unwrap();
        let error: serde_json::Value = serde_json::from_str(&output.recv().await.unwrap()).unwrap();
        assert_eq!(error["id"], 2);
        assert!(error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("same requestId"));
        let listener = UnixListener::bind(&second).unwrap();
        wire::write_descriptor(
            &path,
            &wire::Descriptor {
                version: wire::VERSION,
                socket: second,
                capability: "b".repeat(64),
            },
        )
        .unwrap();
        let (ready, reconnected) = oneshot::channel();
        let server = tokio::spawn(async move {
            let mut stream = admit(&listener, &"b".repeat(64)).await;
            ready.send(()).unwrap();
            let mut call = String::new();
            stream.read_line(&mut call).await.unwrap();
            let call: serde_json::Value = serde_json::from_str(&call).unwrap();
            assert_eq!(call["id"], 3, "only the explicit retry is dispatched");
            stream
                .get_mut()
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\n")
                .await
                .unwrap();
        });
        tokio::time::timeout(Duration::from_secs(5), reconnected)
            .await
            .unwrap()
            .unwrap();
        input.send(r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"send_message","arguments":{"requestId":"mail-1"}}}"#.into()).await.unwrap();
        let result: serde_json::Value =
            serde_json::from_str(&output.recv().await.unwrap()).unwrap();
        assert_eq!(result["id"], 3);
        assert!(result.get("result").is_some());
        drop(input);
        bridge.await.unwrap();
        server.await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
