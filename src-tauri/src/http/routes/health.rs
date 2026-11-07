// SPDX-License-Identifier: MIT
use axum::{routing::get, Json, Router};
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Serialize, ToSchema)]
pub struct HealthResponse {
    /// Always "ok" when the server is running.
    status: &'static str,
    /// CodeSense app version (semver).
    version: &'static str,
}

/// Health check — confirms the HTTP API server is running.
#[utoipa::path(
    get,
    path = "/api/v1/health",
    tag = "system",
    responses(
        (status = 200, description = "Server is healthy", body = HealthResponse)
    )
)]
pub async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new().route("/api/v1/health", get(health))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn test_health_returns_ok() {
        let app = router();
        let req = Request::builder()
            .uri("/api/v1/health")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert!(!json["version"].as_str().unwrap().is_empty());
    }
}
