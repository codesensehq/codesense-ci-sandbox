// SPDX-License-Identifier: MIT
//! SW-GT-4 — `GET /api/v1/taxonomy`: the read-only status-taxonomy HTTP surface.
//!
//! A sandboxed dashboard asks "what are this team's status states, in their
//! configured order, with label/color/icon/role?" and gets back the team's
//! [`StatusTaxonomy`] (ordered `StatusState`s + soft-drift `warnings`). The
//! dashboards render one lane / dropdown option per state and key their control
//! flow off `role` — retiring the divergent hardcoded status arrays that used to
//! drift three ways across the HTML dashboards and the `sync-story` skill.
//!
//! This route is a thin sibling of [`super::sprints`]: it reuses the SAME token
//! gate (`gate_read_token`) and content-root resolution (`resolve_read_content_root`)
//! — so it fails closed identically. Open frames retain cross-team managed reads;
//! restricted frames and legacy scoped tokens clamp to their own root. Only the loader differs:
//! `load_status_taxonomy_from`, offloaded onto the blocking pool via
//! `await_blocking`. No writes.

use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Json, Router};

use crate::commands::status_taxonomy::StatusTaxonomy;
use crate::http::routes::read_gate::{gate_read_token, parse_raw_query, resolve_read_content_root};
use crate::http::AppState;

/// `GET /api/v1/taxonomy` — see the module doc. Read-only, team-scoped by the
/// read capability. Not registered with OpenAPI: `StatusTaxonomy`/`StatusState`
/// don't derive `utoipa::ToSchema` (like `SprintRegistry`), and the route needs
/// no swagger surface.
pub async fn taxonomy(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
) -> Response {
    // TOKEN GATE: fail closed before the query string is parsed. Taking
    // `RawQuery` (not `Query<_>`) keeps this first. Shared with `/api/v1/sprints`.
    let scoped_grant = match gate_read_token(&state, &headers) {
        Ok(grant) => grant,
        Err(resp) => return resp,
    };

    // Parse the optional managed root after authentication. The effective
    // grant decides whether an explicit cross-team root is allowed.
    let scope = match super::read_gate::caller_scope(&state, &headers, scoped_grant.as_ref()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let params = parse_raw_query(raw_query.as_deref());
    let content_root =
        match resolve_read_content_root(scoped_grant.as_ref(), params.content_root.as_deref()) {
            Ok(root) => root,
            Err(resp) => return resp,
        };

    // Offload the taxonomy load onto the Tokio blocking pool (mirrors sprints'
    // registry read) so it never runs inline on the Axum executor.
    // `load_status_taxonomy_from` encodes tolerance in its return shape (a
    // default-seeded taxonomy + warnings) and never errors — the `Err` arm is
    // defensive only. It reads the team's status-taxonomy config under `content_root`.
    let result: Result<StatusTaxonomy, String> =
        crate::commands::await_blocking("load_status_taxonomy_route", move || {
            Ok(
                crate::commands::status_taxonomy::load_status_taxonomy_from_for(
                    std::path::Path::new(&content_root),
                    &scope,
                ),
            )
        })
        .await;

    match result {
        Ok(taxonomy) => (StatusCode::OK, Json(taxonomy)).into_response(),
        Err(e) => {
            log::warn!(target: "http", "taxonomy: load failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "load-failed", "detail": e })),
            )
                .into_response()
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/taxonomy", get(taxonomy))
}

#[cfg(test)]
mod tests {
    // Read-only guard, mirroring `sprints_router_is_get_only`: driving the handler
    // via `oneshot` needs a real `AppHandle` (see proxy.rs), so the GET-only binding
    // is asserted structurally over the router source. A future write binding here is
    // a deliberate change that must trip this test.
    #[test]
    fn taxonomy_router_is_get_only() {
        let src = include_str!("taxonomy.rs");
        let start = src.find("pub fn router()").expect("router() fn present");
        let end = start
            + src[start..]
                .find("\n}")
                .expect("router() fn has a closing brace");
        let body = &src[start..end];
        assert!(
            body.contains("get(taxonomy)"),
            "taxonomy router must bind GET"
        );
        for method in ["post(", "put(", "delete(", "patch(", "any("] {
            assert!(
                !body.contains(method),
                "taxonomy router must not bind {method}"
            );
        }
    }
}
