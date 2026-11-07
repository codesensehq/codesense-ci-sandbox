// SPDX-License-Identifier: MIT
pub mod dashboard_kit;
pub mod health;
pub mod media;
pub mod read_gate;
pub mod sprints;
pub mod taxonomy;
pub mod telemetry;
pub mod terminal;
pub mod viewer;

use axum::Router;
use tauri::Manager;

use crate::http::AppState;

/// Builds the combined API router. Each route module contributes its own
/// sub-router via `.merge()` — adding a new endpoint means adding a file
/// and one `.merge()` call here.
pub fn api_router(state: AppState) -> Router {
    let router = Router::new()
        .merge(agent_dashboard::router())
        .merge(dashboard_kit::router())
        .merge(health::router())
        .merge(media::router())
        .merge(sprints::router())
        .merge(taxonomy::router())
        .merge(telemetry::router())
        .merge(terminal::router())
        .merge(viewer::router(state.app_origins.clone()));

    // OpenAPI spec (JSON, no UI) — dev builds only.
    #[cfg(debug_assertions)]
    let router = {
        use utoipa::OpenApi;

        #[derive(OpenApi)]
        #[openapi(
            info(
                title = "CodeSense HTTP API",
                description = "Local API for external integrations — Claude Code hooks, editor extensions, CLI tools.",
                version = env!("CARGO_PKG_VERSION")
            ),
            paths(
                health::health,
                terminal::terminal_status,
                viewer::serve_file,
                viewer::serve_baseline_file,
                viewer::serve_pane,
                viewer::serve_message,
            ),
            components(schemas(
                health::HealthResponse,
                terminal::HookPayload,
                terminal::StatusResponse,
            ))
        )]
        struct ApiDoc;

        router.route(
            "/api-docs/openapi.json",
            axum::routing::get(|| async { axum::Json(ApiDoc::openapi()) }),
        )
    };

    let boundary = crate::http::frame_boundary::FrameBoundary {
        app_origins: state.app_origins.clone(),
        frames: state
            .app_handle
            .state::<std::sync::Arc<crate::dashboard_stream::frame::FrameStore>>()
            .inner()
            .clone(),
        port: state.app_handle.state::<crate::http::HttpApiPort>().0,
    };
    crate::http::frame_boundary::with_boundary(router, boundary).with_state(state)
}
pub mod agent_dashboard;
