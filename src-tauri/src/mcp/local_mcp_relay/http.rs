// SPDX-License-Identifier: MIT
//! SDK session/SSE machinery with byte limits applied BEFORE its JSON/SSE decoder.
use futures_util::{stream::BoxStream, StreamExt};
use reqwest_mcp::{
    header::{HeaderMap, HeaderName, HeaderValue},
    Client, Response,
};
use rmcp::{
    model::{ClientJsonRpcMessage, ClientNotification},
    transport::streamable_http_client::{
        SseError, StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
    },
};
use sse_stream::Sse;
use std::{collections::HashMap, sync::Arc};
type Error = StreamableHttpError<std::io::Error>;
const LIMIT: usize = 1024 * 1024;
fn fail(message: &'static str) -> Error {
    StreamableHttpError::Client(std::io::Error::other(message))
}
// The standing-stream slot shared by one connection's client clones.
const STREAM_IDLE: u8 = 0;
const STREAM_ARMED: u8 = 1;
const STREAM_TAKEN: u8 = 2;
const STREAM_OPENED: u8 = 3;
pub(super) struct BoundedHttp {
    pub client: Client,
    pub valid: super::Validate,
    pub cancel: tokio_util::sync::CancellationToken,
    pub failures: super::transport::Failures,
    /// The one GET this connection may open: the session's standing stream.
    pub stream: Arc<std::sync::atomic::AtomicU8>,
    /// Whether this clone is the one rmcp opens the standing stream with.
    pub owns_stream: bool,
}
// rmcp 2.2 opens the standing GET on the first client clone it makes after the
// `notifications/initialized` POST is accepted; every SSE resume runs on a later clone.
// Only that clone may take the slot, so a resume can never claim it first.
impl Clone for BoundedHttp {
    fn clone(&self) -> Self {
        use std::sync::atomic::Ordering;
        Self {
            client: self.client.clone(),
            valid: self.valid.clone(),
            cancel: self.cancel.clone(),
            failures: self.failures.clone(),
            stream: self.stream.clone(),
            owns_stream: self
                .stream
                .compare_exchange(
                    STREAM_ARMED,
                    STREAM_TAKEN,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok(),
        }
    }
}
impl BoundedHttp {
    async fn send(&self, request: reqwest_mcp::RequestBuilder) -> Result<Response, Error> {
        // After an authority or policy failure nothing else leaves this connection.
        if self.failures.terminal() {
            return Err(fail("MCP authority or policy failure"));
        }
        tokio::select! { biased; _=self.cancel.cancelled()=>return Err(fail("MCP HTTP retired")), result=(self.valid)()=>result.map_err(|_|fail("MCP HTTP authority retired"))? }
        tokio::select! { biased;
            _=self.cancel.cancelled()=>Err(fail("MCP HTTP retired; request outcome unknown")),
            result=tokio::time::timeout(std::time::Duration::from_secs(120),request.send())=>result.map_err(|_|fail("MCP HTTP header deadline exceeded"))?.map_err(|_|fail("MCP HTTP unavailable; request outcome unknown"))
        }
    }
}
fn request_headers(
    session: Option<&str>,
    auth: Option<String>,
    headers: HashMap<HeaderName, HeaderValue>,
) -> Result<HeaderMap, Error> {
    let mut result: HeaderMap = headers.into_iter().collect();
    if let Some(session) = session {
        result.insert(
            "mcp-session-id",
            session.parse().map_err(|_| fail("invalid MCP session"))?,
        );
    }
    if let Some(token) = auth {
        result.insert(
            "authorization",
            format!("Bearer {token}")
                .parse()
                .map_err(|_| fail("invalid MCP authorization"))?,
        );
    }
    Ok(result)
}
fn events(
    response: Response,
    cancel: tokio_util::sync::CancellationToken,
    policy: Arc<std::sync::atomic::AtomicBool>,
) -> BoxStream<'static, Result<Sse, SseError>> {
    // Track framing size only; the maintained sse-stream codec still owns all parsing.
    // CRLF/LF blank lines reset the event budget; arbitrarily long fields/comments
    // are refused before the decoder can buffer them.
    let mut event_bytes = 0usize;
    let mut line_bytes = 0usize;
    let mut failed = false;
    let bytes = response.bytes_stream().map(move |chunk| {
        if failed {
            return Err(std::io::Error::other("MCP stream closed"));
        }
        let chunk = chunk.map_err(|_| std::io::Error::other("MCP HTTP read failed"))?;
        for byte in chunk.iter() {
            event_bytes += 1;
            if event_bytes > LIMIT {
                failed = true;
                policy.store(true, std::sync::atomic::Ordering::Release);
                return Err(std::io::Error::other("MCP event exceeds limit"));
            }
            if *byte == b'\n' {
                if line_bytes == 0 {
                    event_bytes = 0;
                }
                line_bytes = 0;
            } else if *byte != b'\r' {
                line_bytes += 1;
            }
        }
        Ok(chunk)
    });
    sse_stream::SseStream::from_bytes_stream(bytes)
        .take_until(cancel.cancelled_owned())
        .boxed()
}
async fn bounded_json(
    mut response: Response,
    policy: &std::sync::atomic::AtomicBool,
) -> Result<rmcp::model::ServerJsonRpcMessage, Error> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| fail("MCP HTTP read failed"))?
    {
        if body.len() + chunk.len() > LIMIT {
            policy.store(true, std::sync::atomic::Ordering::Release);
            return Err(fail("MCP response exceeds limit"));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| {
        policy.store(true, std::sync::atomic::Ordering::Release);
        fail("invalid MCP response")
    })
}
impl StreamableHttpClient for BoundedHttp {
    type Error = std::io::Error;
    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session: Option<Arc<str>>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, Error> {
        let initialized = matches!(
            &message,
            ClientJsonRpcMessage::Notification(n)
                if matches!(n.notification, ClientNotification::InitializedNotification(_))
        );
        let body = serde_json::to_vec(&message).map_err(|_| fail("invalid MCP request"))?;
        if body.len() > LIMIT {
            return Err(fail("MCP request exceeds limit"));
        }
        let response = self
            .send(
                self.client
                    .post(uri.as_ref())
                    .headers(request_headers(session.as_deref(), auth, headers)?)
                    .header("accept", "application/json, text/event-stream")
                    .header("content-type", "application/json")
                    .body(body),
            )
            .await?;
        let status = response.status();
        if status.as_u16() == 404 && session.is_some() {
            return Err(StreamableHttpError::SessionExpired);
        }
        if status.as_u16() == 401 || status.as_u16() == 403 {
            if let Ok(mut rejected) = self.failures.authentication_required.lock() {
                *rejected = serde_json::to_value(&message)
                    .ok()
                    .and_then(|value| value.get("id").cloned());
            }
            self.failures
                .policy
                .store(true, std::sync::atomic::Ordering::Release);
            return Err(fail("local MCP authentication required"));
        }
        if status.is_success() && initialized {
            let _ = self.stream.compare_exchange(
                STREAM_IDLE,
                STREAM_ARMED,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            );
        }
        if matches!(status.as_u16(), 202 | 204) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if !status.is_success() {
            return Err(fail("MCP HTTP rejected request; request outcome unknown"));
        }
        let session = response
            .headers()
            .get("mcp-session-id")
            .map(|v| {
                v.to_str()
                    .map(str::to_owned)
                    .map_err(|_| fail("invalid MCP session"))
            })
            .transpose()?;
        let content = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if content.starts_with("application/json") {
            Ok(StreamableHttpPostResponse::Json(
                tokio::select! { biased;
                    _=self.cancel.cancelled()=>return Err(fail("MCP HTTP retired")),
                    result=tokio::time::timeout(std::time::Duration::from_secs(120),bounded_json(response, &self.failures.policy))=>result.map_err(|_|fail("MCP HTTP response deadline exceeded"))??
                },
                session,
            ))
        } else if content.starts_with("text/event-stream") {
            Ok(StreamableHttpPostResponse::Sse(
                events(response, self.cancel.clone(), self.failures.policy.clone()),
                session,
            ))
        } else {
            self.failures
                .policy
                .store(true, std::sync::atomic::Ordering::Release);
            Err(fail("unsupported MCP HTTP content type"))
        }
    }
    async fn get_stream(
        &self,
        uri: Arc<str>,
        session: Arc<str>,
        last: Option<String>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, Error> {
        // The relay is one-shot: rmcp resumes a dropped or closed stream with another
        // GET (even under NeverRetry, and whenever the server sends `retry:`), with or
        // without Last-Event-ID. Only the standing-stream open, once, may GET; every
        // resume is refused, and send() refuses everything after a policy failure.
        if !self.owns_stream
            || last.is_some()
            || self
                .stream
                .compare_exchange(
                    STREAM_TAKEN,
                    STREAM_OPENED,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire,
                )
                .is_err()
        {
            return Err(fail("MCP stream reconnect refused"));
        }
        let request = self
            .client
            .get(uri.as_ref())
            .headers(request_headers(Some(&session), auth, headers)?)
            .header("accept", "text/event-stream");
        let response = self.send(request).await?;
        if matches!(response.status().as_u16(), 401 | 403) {
            self.failures
                .policy
                .store(true, std::sync::atomic::Ordering::Release);
        }
        if response.status().as_u16() == 405 {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        if !response.status().is_success() {
            return Err(fail("MCP HTTP stream unavailable"));
        }
        if !response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"))
        {
            self.failures
                .policy
                .store(true, std::sync::atomic::Ordering::Release);
            return Err(fail("unsupported MCP HTTP content type"));
        }
        Ok(events(
            response,
            self.cancel.clone(),
            self.failures.policy.clone(),
        ))
    }
    async fn delete_session(
        &self,
        uri: Arc<str>,
        session: Arc<str>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), Error> {
        let response = self
            .send(self.client.delete(uri.as_ref()).headers(request_headers(
                Some(&session),
                auth,
                headers,
            )?))
            .await?;
        if response.status().is_success() || matches!(response.status().as_u16(), 404 | 405) {
            Ok(())
        } else {
            Err(fail("MCP HTTP close failed"))
        }
    }
}
