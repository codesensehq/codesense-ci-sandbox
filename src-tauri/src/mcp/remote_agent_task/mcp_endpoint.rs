// SPDX-License-Identifier: MIT
//! Task-only loopback MCP endpoint; provider credentials never enter this capability.
use super::super::{handler::CodesenseHandler, remote_authority::RemoteAuthority};
use axum::{
    extract::Request,
    middleware::{self, Next},
    response::IntoResponse,
    Router,
};
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) struct Endpoint {
    pub url: String,
    pub token: String,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Endpoint {
    pub async fn open(app: tauri::AppHandle, authority: RemoteAuthority) -> Result<Self, String> {
        authority
            .validate()
            .await
            .map_err(|_| "task MCP authority unavailable")?;
        Self::serve(move || CodesenseHandler::new_remote(app.clone(), authority.clone())).await
    }
    pub(super) async fn serve<H: rmcp::ServerHandler + 'static>(
        factory: impl Fn() -> H + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "task MCP listener unavailable")?;
        let address = listener
            .local_addr()
            .map_err(|_| "task MCP listener unavailable")?;
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let expected = format!("Bearer {token}");
        let shutdown = CancellationToken::new();
        let service = StreamableHttpService::new(
            move || Ok(factory()),
            Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default()
                .with_stateful_mode(false)
                .with_json_response(true)
                .with_cancellation_token(shutdown.clone()),
        );
        let router = Router::new()
            .nest_service("/mcp", service)
            .layer(middleware::from_fn(move |request: Request, next: Next| {
                let expected = expected.clone();
                async move {
                    if request.headers().contains_key("origin")
                        || request
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            != Some(expected.as_str())
                    {
                        return axum::http::StatusCode::UNAUTHORIZED.into_response();
                    }
                    let (parts, body) = request.into_parts();
                    let bytes = match axum::body::to_bytes(body, 1024 * 1024).await {
                        Ok(bytes) => bytes,
                        Err(_) => return axum::http::StatusCode::PAYLOAD_TOO_LARGE.into_response(),
                    };
                    next.run(Request::from_parts(parts, axum::body::Body::from(bytes)))
                        .await
                }
            }));
        let stop = shutdown.clone();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(stop.cancelled_owned())
                .await;
        });
        Ok(Self {
            url: format!("http://{address}/mcp"),
            token,
            shutdown,
            task,
        })
    }
}
impl Drop for Endpoint {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.task.abort();
    }
}
