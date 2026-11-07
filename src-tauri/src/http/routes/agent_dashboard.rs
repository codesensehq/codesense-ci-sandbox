// SPDX-License-Identifier: MIT
use axum::{
    http::{header, HeaderValue, StatusCode},
    response::Response,
    routing::get,
    Router,
};

const AGENT_USAGE_HTML: &[u8] = include_bytes!("../../../assets/dashboards/agent-usage/index.html");
const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; connect-src 'none'; frame-ancestors http://tauri.localhost https://tauri.localhost tauri://localhost";

async fn serve_agent_usage() -> Result<Response, StatusCode> {
    let mut response = Response::new(AGENT_USAGE_HTML.into());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

pub fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    // The opaque id is resolved by this exact route. There is no filesystem
    // parameter to traverse and no absolute renderer path to validate.
    Router::new().route(
        "/api/v1/bundled-dashboards/agent-usage",
        get(serve_agent_usage),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn serves_only_the_sealed_dashboard_with_csp() {
        let response = router::<()>()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/bundled-dashboards/agent-usage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response
            .headers()
            .contains_key(header::CONTENT_SECURITY_POLICY));

        let missing = router::<()>()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/bundled-dashboards/../../etc/passwd")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    }
}
