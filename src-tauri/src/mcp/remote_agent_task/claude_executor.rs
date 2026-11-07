// SPDX-License-Identifier: MIT
//! Typed executor operations for Claude's finite remote MCP tools. No app-server forwarding.
use super::harness::{check, Validate};
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::{
    tungstenite::{client::IntoClientRequest, Message},
    MaybeTlsStream, WebSocketStream,
};
type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
pub(super) struct Client {
    wire: Mutex<(Socket, u64)>,
    cwd: String,
    valid: Validate,
}
impl Client {
    pub async fn connect(
        endpoint: &crate::mcp::remote_executor::ExecutorEndpoint,
        valid: Validate,
    ) -> Result<Arc<Self>, String> {
        check(&valid).await?;
        // The executor is a local `ws://` listener. A `wss://` URL is refused
        // here: tokio-tungstenite's default TLS config cannot pick a rustls
        // provider in this tree (both are on), and the dashboard live link is the
        // only `wss` caller, with its own explicit config.
        if !endpoint.ws_url().starts_with("ws://") {
            return Err("executor URL unavailable".into());
        }
        let mut request = endpoint
            .ws_url()
            .into_client_request()
            .map_err(|_| "executor URL unavailable")?;
        request.headers_mut().insert(
            "Authorization",
            format!("Bearer {}", endpoint.auth_token())
                .parse()
                .map_err(|_| "executor capability unavailable")?,
        );
        let (wire, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|_| "executor attach unavailable")?;
        let client = Arc::new(Self {
            wire: Mutex::new((wire, 0)),
            cwd: endpoint.remote_cwd().into(),
            valid,
        });
        client
            .call(
                "initialize",
                json!({"clientName":"codesense-claude-task","resumeSessionId":null}),
            )
            .await?;
        client
            .wire
            .lock()
            .await
            .0
            .send(Message::Text(
                json!({"method":"initialized","params":{}})
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(|_| "executor initialization outcome unknown")?;
        Ok(client)
    }
    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        check(&self.valid).await?;
        let mut wire = self.wire.lock().await;
        wire.1 += 1;
        let id = wire.1;
        wire.0
            .send(Message::Text(
                json!({"id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .await
            .map_err(|_| "executor request outcome unknown")?;
        let response = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while let Some(message) = wire.0.next().await {
                let message = message.map_err(|_| "executor response outcome unknown")?;
                if let Message::Text(text) = message {
                    if text.len() > 1024 * 1024 {
                        return Err("executor response exceeds bound".to_string());
                    }
                    let value: Value =
                        serde_json::from_str(&text).map_err(|_| "executor response invalid")?;
                    if value.get("id").and_then(Value::as_u64) == Some(id) {
                        if value.get("error").is_some() {
                            return Err("remote operation refused".into());
                        }
                        return value
                            .get("result")
                            .cloned()
                            .ok_or_else(|| "remote result unavailable".into());
                    }
                }
            }
            Err("executor ended; operation outcome unknown".into())
        })
        .await
        .map_err(|_| "executor request outcome unknown")??;
        check(&self.valid).await?;
        Ok(response)
    }
    fn path(&self, path: &str) -> Result<String, String> {
        let path = std::path::Path::new(path);
        if path.is_absolute()
            || path
                .components()
                .any(|p| !matches!(p, std::path::Component::Normal(_)))
        {
            return Err("path must be relative to the managed workspace".into());
        }
        url::Url::from_file_path(std::path::Path::new(&self.cwd).join(path))
            .map(|u| u.to_string())
            .map_err(|_| "remote path unavailable".into())
    }
    pub async fn read(&self, path: &str) -> Result<String, String> {
        let value = self
            .call(
                "fs/readFile",
                json!({"path":self.path(path)?,"followSymlinks":false,"sandbox":null}),
            )
            .await?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value["dataBase64"].as_str().ok_or("remote bytes missing")?)
            .map_err(|_| "invalid remote bytes")?;
        if bytes.len() > 256 * 1024 {
            return Err("file exceeds 256KiB task tool bound".into());
        }
        String::from_utf8(bytes).map_err(|_| "file is not UTF-8".into())
    }
    pub async fn write(&self, path: &str, text: &str) -> Result<(), String> {
        if text.len() > 256 * 1024 {
            return Err("file exceeds 256KiB task tool bound".into());
        }
        self.call("fs/writeFile",json!({"path":self.path(path)?,"followSymlinks":false,"sandbox":null,"dataBase64":base64::engine::general_purpose::STANDARD.encode(text.as_bytes())})).await?;
        Ok(())
    }
    pub async fn exec(&self, command: &str) -> Result<Value, String> {
        if command.is_empty() || command.len() > 65536 {
            return Err("command must contain 1–65536 bytes".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let cwd = url::Url::from_file_path(&self.cwd)
            .map_err(|_| "remote cwd unavailable")?
            .to_string();
        self.call("process/start",json!({"processId":id,"argv":["/bin/sh","-c",command],"cwd":cwd,"env":{},"envPolicy":{"inherit":"none","ignoreDefaultExcludes":false,"exclude":[],"set":{},"includeOnly":[]},"tty":false,"pipeStdin":false})).await?;
        let result=tokio::time::timeout(std::time::Duration::from_secs(60),async {
            let mut cursor=Value::Null;let mut stdout=Vec::new();let mut stderr=Vec::new();
            loop {
                let value=self.call("process/read",json!({"processId":id,"afterSeq":cursor,"maxBytes":65536,"waitMs":1000})).await?;
                for chunk in value["chunks"].as_array().ok_or("remote output malformed")? {
                    let bytes=base64::engine::general_purpose::STANDARD.decode(chunk["chunk"].as_str().ok_or("remote output missing")?).map_err(|_|"remote output malformed")?;
                    if stdout.len()+stderr.len()+bytes.len()>256*1024 {return Err("command output exceeds 256KiB".to_string());}
                    if chunk["stream"]=="stderr" {stderr.extend(bytes);}else{stdout.extend(bytes);}
                }
                cursor=value["nextSeq"].as_u64().map(|v|json!(v.saturating_sub(1))).unwrap_or(Value::Null);
                if value["exited"]==true&&value["closed"]==true {return Ok(json!({"stdout":String::from_utf8_lossy(&stdout),"stderr":String::from_utf8_lossy(&stderr),"exitCode":value["exitCode"]}));}
            }
        }).await.map_err(|_|"remote command timeout; stop requested".to_string()).and_then(|v|v);
        if result.is_err() {
            let _ = self
                .call("process/terminate", json!({"processId":id}))
                .await;
        }
        result
    }
}
