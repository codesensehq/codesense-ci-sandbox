// SPDX-License-Identifier: MIT
//! Private task-owned WebSocket adapter for an admitted native executor.
use super::remote_workspace_target::ResolvedWorkspace;
use crate::terminal::session_server::SessionServerClient;
use codesense_host_proto::executor as wire;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{
    handshake::server::{ErrorResponse, Request, Response},
    protocol::WebSocketConfig,
    Message,
};
use uuid::Uuid;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseOutcome {
    Confirmed,
    Unknown,
}
pub(crate) struct ExecutorEndpoint {
    environment_id: String,
    url: String,
    token: String,
    cwd: String,
    stop: watch::Sender<bool>,
    closed: watch::Receiver<Option<CloseOutcome>>,
}
impl ExecutorEndpoint {
    pub(crate) fn environment_id(&self) -> &str {
        &self.environment_id
    }
    pub(crate) fn ws_url(&self) -> &str {
        &self.url
    }
    pub(crate) fn auth_token(&self) -> &str {
        &self.token
    }
    pub(crate) fn remote_cwd(&self) -> &str {
        &self.cwd
    }
    pub(crate) async fn closed(&self) {
        let mut closed = self.closed.clone();
        while closed.borrow().is_none() {
            if closed.changed().await.is_err() {
                break;
            }
        }
    }
    pub(crate) async fn close(&self) -> CloseOutcome {
        let _ = self.stop.send(true);
        let mut closed = self.closed.clone();
        let wait = async {
            loop {
                if let Some(outcome) = *closed.borrow() {
                    return outcome;
                }
                if closed.changed().await.is_err() {
                    return CloseOutcome::Unknown;
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(7), wait)
            .await
            .unwrap_or(CloseOutcome::Unknown)
    }
}
impl Drop for ExecutorEndpoint {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}
type Validate = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;
async fn validate(valid: Validate) -> Result<(), String> {
    tokio::task::spawn_blocking(move || valid())
        .await
        .map_err(|_| "executor authority check unavailable")?
}
pub(crate) async fn open(
    resolved: ResolvedWorkspace,
    task_id: Uuid,
    valid: Validate,
) -> Result<ExecutorEndpoint, String> {
    let workspace_valid = resolved.validate.clone();
    let supplied = valid;
    let valid: Validate = Arc::new(move || {
        workspace_valid()?;
        supplied()
    });
    validate(valid.clone()).await?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(|_| "executor endpoint unavailable")?;
    let address = listener
        .local_addr()
        .map_err(|_| "executor endpoint unavailable")?;
    let client = resolved.client;
    let target = resolved.target;
    let id = task_id.to_string();
    let start_client = client.clone();
    let start_id = id.clone();
    let (started, mut lifetime) =
        tokio::task::spawn_blocking(move || start_client.start_executor(start_id, target))
            .await
            .map_err(|_| "executor start outcome unknown")??;
    let channel = started.channel;
    let remote_token = started.auth_token;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let (stop, mut stopped) = watch::channel(false);
    let (closed_tx, closed) = watch::channel(None);
    let endpoint = ExecutorEndpoint {
        environment_id: format!("codesense-{task_id}"),
        url: format!("ws://{address}"),
        token: token.clone(),
        cwd: started.cwd,
        stop,
        closed,
    };
    tokio::spawn(async move {
        let mut peers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
             accepted=listener.accept()=>{let Ok((socket,_))=accepted else{break};if peers.len()>=wire::MAX_CONNECTIONS{drop(socket);continue}
              let client=client.clone();let id=id.clone();let token=token.clone();let remote_token=remote_token.clone();let valid=valid.clone();
              peers.spawn(async move{let result=serve_peer(socket,client,id,token,remote_token,valid).await;#[cfg(test)] if let Err(error)=&result{eprintln!("executor peer ended: {error}");}result});
             },
             _=stopped.changed()=>break,
             _=lifetime.recv()=>break,
             _=tokio::time::sleep(std::time::Duration::from_millis(200))=>{if validate(valid.clone()).await.is_err(){break}},
             Some(_)=peers.join_next(),if !peers.is_empty()=>{}
            }
        }
        drop(listener);
        peers.abort_all();
        while peers.join_next().await.is_some() {}
        let outcome = tokio::task::spawn_blocking(move || client.stop_executor(id, channel)).await;
        let _ = closed_tx.send(Some(if matches!(outcome, Ok(Ok(()))) {
            CloseOutcome::Confirmed
        } else {
            CloseOutcome::Unknown
        }));
    });
    Ok(endpoint)
}
async fn serve_peer(
    socket: tokio::net::TcpStream,
    client: Arc<SessionServerClient>,
    id: String,
    token: String,
    remote_token: String,
    valid: Validate,
) -> Result<(), String> {
    let expected = format!("Bearer {token}");
    #[allow(
        clippy::result_large_err,
        reason = "WebSocket handshake callback requires the concrete HTTP error response"
    )]
    let callback = move |request: &Request,
                         response: Response|
          -> Result<Response, ErrorResponse> {
        if request
            .headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            == Some(expected.as_str())
            && request.headers().get("origin").is_none()
        {
            Ok(response)
        } else {
            let mut error = ErrorResponse::new(Some("executor capability rejected".into()));
            *error.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED;
            Err(error)
        }
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(wire::LINE_BYTES))
        .max_frame_size(Some(wire::LINE_BYTES));
    let local = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio_tungstenite::accept_hdr_async_with_config(socket, callback, Some(config)),
    )
    .await
    .map_err(|_| "executor handshake timed out")?
    .map_err(|_| "executor handshake rejected")?;
    validate(valid.clone()).await?;
    let open_client = client.clone();
    let (channel, incoming) =
        tokio::task::spawn_blocking(move || open_client.open_executor_channel(id))
            .await
            .map_err(|_| "executor connection unavailable")??;
    let stream = stream(client, channel, incoming);
    let mut request = "ws://127.0.0.1/"
        .into_client_request()
        .map_err(|_| "executor handshake unavailable")?;
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {remote_token}")
            .parse()
            .map_err(|_| "executor capability unavailable")?,
    );
    let (remote, _) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio_tungstenite::client_async_with_config(request, stream, Some(config)),
    )
    .await
    .map_err(|_| "executor native handshake timed out")?
    .map_err(|_| "executor native handshake rejected")?;
    relay(local, remote, valid).await
}
fn stream(
    client: Arc<SessionServerClient>,
    channel: u32,
    mut incoming: mpsc::Receiver<Vec<u8>>,
) -> DuplexStream {
    let (service, transport) = tokio::io::duplex(wire::CHUNK_BYTES);
    let (mut reader, mut writer) = tokio::io::split(transport);
    tokio::spawn(async move {
        let inbound = async {
            while let Some(bytes) = incoming.recv().await {
                writer.write_all(&bytes).await.map_err(|_| ())?;
            }
            Ok::<(), ()>(())
        };
        let outbound = async {
            let mut bytes = vec![0; wire::CHUNK_BYTES];
            loop {
                let n = reader.read(&mut bytes).await.map_err(|_| ())?;
                if n == 0 {
                    return Ok::<(), ()>(());
                }
                client.executor_send(channel, &bytes[..n]).map_err(|_| ())?;
            }
        };
        tokio::select! {_=inbound=>{},_=outbound=>{}}
        client.close_executor_channel(channel);
    });
    service
}
async fn relay(
    local: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    remote: tokio_tungstenite::WebSocketStream<DuplexStream>,
    valid: Validate,
) -> Result<(), String> {
    let (mut local_sink, mut local_source) = local.split();
    let (mut remote_sink, mut remote_source) = remote.split();
    loop {
        tokio::select! {
         message=local_source.next()=>{let Some(message)=message else{return Ok(())};match message.map_err(|_|"executor peer unavailable")?{
          Message::Text(text)=>{let value:Value=serde_json::from_str(&text).map_err(|_|"invalid executor request")?;
           if value.get("method").and_then(Value::as_str)==Some("environmentConfig/read"){if let Some(id)=value.get("id"){let reply=serde_json::json!({"id":id,"error":{"code":-32601,"message":"remote configuration is not an authentication or policy authority"}});local_sink.send(Message::text(reply.to_string())).await.map_err(|_|"executor peer unavailable")?;}continue}
           validate(valid.clone()).await?;remote_sink.send(Message::Text(text)).await.map_err(|_|"executor native connection unavailable")?;
          },Message::Close(_)=>{let _=remote_sink.send(Message::Close(None)).await;return Ok(())},Message::Ping(v)=>local_sink.send(Message::Pong(v)).await.map_err(|_|"executor peer unavailable")?,Message::Pong(_)=>{},_=>return Err("unsupported executor frame".into())}
         },
         message=remote_source.next()=>{let Some(message)=message else{return Ok(())};match message.map_err(|_|"executor native connection unavailable")?{
          Message::Text(text)=>{let mut value:Value=serde_json::from_str(&text).map_err(|_|"invalid executor response")?;suppress_remote_config(&mut value);validate(valid.clone()).await?;local_sink.send(Message::text(value.to_string())).await.map_err(|_|"executor peer unavailable")?;},Message::Close(_)=>return Ok(()),Message::Ping(v)=>remote_sink.send(Message::Pong(v)).await.map_err(|_|"executor native connection unavailable")?,Message::Pong(_)=>{},_=>return Err("unsupported native executor frame".into())}
         }
        }
    }
}
fn suppress_remote_config(value: &mut Value) {
    for pointer in [
        "/result/environmentInfo/capabilities",
        "/result/capabilities",
    ] {
        if let Some(capabilities) = value.pointer_mut(pointer).and_then(Value::as_object_mut) {
            capabilities.insert("environmentConfigRead".into(), Value::Bool(false));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_capability_is_removed_from_both_handshake_and_info() {
        for mut response in [
            serde_json::json!({"result":{"environmentInfo":{"capabilities":{"environmentConfigRead":true}}}}),
            serde_json::json!({"result":{"capabilities":{"environmentConfigRead":true}}}),
        ] {
            suppress_remote_config(&mut response);
            assert!(!response.to_string().contains("true"));
        }
    }
}
