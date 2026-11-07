// SPDX-License-Identifier: MIT
//! The local receiver for agent CLIs' OpenTelemetry export. A launched Claude or Codex session is
//! pointed here by its launch settings (see the adapter's `telemetry`), and each usage record it
//! sends is stored by the usage store. Metrics repeat the same usage as aggregates, so they are
//! acknowledged and not stored.

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, Path},
    http::StatusCode,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};

/// The largest export one request may carry. A busy session's batch of usage records is far
/// smaller, and this bounds what one request can make the app hold.
const MAX_EXPORT_BYTES: usize = 8 * 1024 * 1024;

/// The base URL a CLI's OTLP exporter is given. Its signal paths hang off it.
pub const ENDPOINT_PATH: &str = "/api/v1/telemetry";

/// The base URL a launch's exporter is given: the local API port, then the launch's own token. The
/// token is what ties the launch's usage to who ran it.
pub fn endpoint_for(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}{ENDPOINT_PATH}/{token}")
}

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    build(None)
}

/// The receiver, storing into the usage store at `store` rather than the CodeSense home's. Tests use
/// it to check the route against a temporary store.
#[cfg(test)]
pub fn router_at_store<S: Clone + Send + Sync + 'static>(store: PathBuf) -> Router<S> {
    build(Some(store))
}

fn build<S: Clone + Send + Sync + 'static>(store: Option<PathBuf>) -> Router<S> {
    let store = Arc::new(store);
    let logs_store = store.clone();
    Router::new()
        .route(
            &format!("{ENDPOINT_PATH}/{{token}}/v1/logs"),
            post(move |Path(token): Path<String>, Json(export): Json<Value>| {
                let store = logs_store.clone();
                async move { logs(store, token, export).await }
            }),
        )
        .route(&format!("{ENDPOINT_PATH}/{{token}}/v1/metrics"), post(metrics))
        .layer(DefaultBodyLimit::max(MAX_EXPORT_BYTES))
}

/// Stores one export. A failed store answers 503, so the CLI's exporter retries rather than losing
/// the usage.
async fn logs(
    store: Arc<Option<PathBuf>>,
    token: String,
    export: Value,
) -> Result<Json<Value>, StatusCode> {
    let stored = tokio::task::spawn_blocking(move || match store.as_deref() {
        Some(path) => crate::session_usage::ingest_telemetry_for_at(path, &token, &export),
        None => crate::session_usage::ingest_telemetry_for(&token, &export),
    })
    .await;
    match stored {
        Ok(Ok(_)) => Ok(Json(json!({}))),
        Ok(Err(e)) => {
            log::warn!(target: "telemetry", "usage export not stored: {e}");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
        Err(e) => {
            log::warn!(target: "telemetry", "usage export task failed: {e}");
            Err(StatusCode::SERVICE_UNAVAILABLE)
        }
    }
}

async fn metrics(Path(_token): Path<String>) -> Json<Value> {
    Json(json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    const CLAUDE_EXPORT: &str = r#"{"resourceLogs":[{"scopeLogs":[{"logRecords":[{"attributes":[
        {"key":"event.name","value":{"stringValue":"api_request"}},
        {"key":"request_id","value":{"stringValue":"req_route"}},
        {"key":"session.id","value":{"stringValue":"sess-route"}},
        {"key":"event.timestamp","value":{"stringValue":"2026-10-16T18:52:24.455Z"}},
        {"key":"model","value":{"stringValue":"claude-haiku-5-5"}},
        {"key":"input_tokens","value":{"intValue":3}}
    ]}]}]}]}"#;

    fn post_to(app: Router, path: &str, body: &str) -> axum::http::Response<Body> {
        let request = Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(app.oneshot(request))
            .unwrap()
    }

    #[test]
    fn an_export_posted_to_a_launch_address_is_stored_for_that_launch() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("usage.sqlite");
        let res = post_to(router_at_store(db.clone()), "/api/v1/telemetry/launch-1/v1/logs", CLAUDE_EXPORT);
        assert_eq!(res.status(), StatusCode::OK);
        let r = crate::session_usage::UsageStore::open(&db)
            .unwrap()
            .report(&crate::session_usage::Filter::default())
            .unwrap();
        assert_eq!(r.sessions.len(), 1);
        assert_eq!(r.sessions[0].session_id, "sess-route");
    }

    #[test]
    fn a_token_registered_at_launch_attributes_the_exported_session_to_that_launch() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("usage.sqlite");
        crate::session_usage::UsageStore::open(&db)
            .unwrap()
            .register_launch("launch-9", "acme/team-context", Some("riverside"), "builtin/claude", "h", None)
            .unwrap();
        let res = post_to(router_at_store(db.clone()), "/api/v1/telemetry/launch-9/v1/logs", CLAUDE_EXPORT);
        assert_eq!(res.status(), StatusCode::OK);
        let r = crate::session_usage::UsageStore::open(&db)
            .unwrap()
            .report(&crate::session_usage::Filter::default())
            .unwrap();
        assert_eq!(r.sessions[0].org.as_deref(), Some("acme/team-context"));
        assert_eq!(r.sessions[0].team.as_deref(), Some("riverside"));
        assert_eq!(r.sessions[0].agent.as_deref(), Some("builtin/claude"));
    }

    /// A valid export whose one record carries `padding_bytes` of filler in an extra attribute, so the
    /// body is that large.
    fn export_of_about(padding_bytes: usize) -> String {
        let padding = "x".repeat(padding_bytes);
        let attr = format!(r#"{{"key":"padding","value":{{"stringValue":"{padding}"}}}}"#);
        CLAUDE_EXPORT.replacen("]}]}]}", &format!(",{attr}]}}]}}]}}"), 1)
    }

    #[test]
    fn an_export_just_under_the_body_limit_is_stored() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("usage.sqlite");
        let body = export_of_about(MAX_EXPORT_BYTES - 64 * 1024);
        assert!(body.len() < MAX_EXPORT_BYTES);
        let res = post_to(router_at_store(db.clone()), "/api/v1/telemetry/launch-1/v1/logs", &body);
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[test]
    fn an_export_over_the_body_limit_is_refused_rather_than_dropped_silently() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("usage.sqlite");
        let body = export_of_about(MAX_EXPORT_BYTES + 64 * 1024);
        let res = post_to(router_at_store(db.clone()), "/api/v1/telemetry/launch-1/v1/logs", &body);
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn metrics_are_acknowledged_and_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("usage.sqlite");
        let res = post_to(router_at_store(db.clone()), "/api/v1/telemetry/launch-1/v1/metrics", CLAUDE_EXPORT);
        assert_eq!(res.status(), StatusCode::OK);
        let r = crate::session_usage::UsageStore::open(&db)
            .unwrap()
            .report(&crate::session_usage::Filter::default())
            .unwrap();
        assert!(r.sessions.is_empty());
    }

    #[test]
    fn a_store_that_cannot_be_opened_answers_503_so_the_exporter_retries() {
        // A directory where the store file should be: the store cannot open, so the usage is not stored.
        let dir = tempfile::tempdir().unwrap();
        let res = post_to(router_at_store(dir.path().to_path_buf()), "/api/v1/telemetry/launch-1/v1/logs", CLAUDE_EXPORT);
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
