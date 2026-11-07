// SPDX-License-Identifier: MIT
//! SW-SB-13 — `GET /api/v1/sprints`: the read-only sprint-registry HTTP surface.
//!
//! A sandboxed analysis dashboard asks "what are this team's sprints, with their
//! stamped velocity?" and gets back the team's [`SprintRegistry`] (active sprint,
//! ordered `SprintDef`s incl. `deliveredPoints`/`carriedPoints`, warnings). The
//! durable closed-sprint velocity (SW-SB-11) is unrecoverable from live story
//! data after close, so the dashboard reads it here rather than recomputing.
//!
//! This route is a thin sibling of the other read routes: it reuses the SAME token
//! gate (`gate_read_token`) and content-root resolution (`resolve_read_content_root`)
//! — so it fails closed identically. Open frames retain cross-team managed reads;
//! restricted frames and legacy scoped tokens clamp to their own root. Only the loader differs:
//! `load_sprint_registry_from`, offloaded onto the blocking pool via
//! `await_blocking`. No writes.

use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Json, Router};

use crate::commands::sprint_registry::{SprintRegistry, SprintStories};
use crate::context_repo::team_config_dir_beside_content;
use crate::http::routes::read_gate::{gate_read_token, parse_raw_query, resolve_read_content_root};
use crate::http::AppState;

/// `GET /api/v1/sprints` — see the module doc. Read-only, team-scoped by the
/// read capability. Not registered with OpenAPI: `SprintRegistry`/`SprintDef`
/// don't derive `utoipa::ToSchema` (unlike `DocQueryResult`), and the route needs
/// no swagger surface.
pub async fn sprints(
    State(state): State<AppState>,
    headers: HeaderMap,
    RawQuery(raw_query): RawQuery,
) -> Response {
    // TOKEN GATE (AC-11): fail closed before the query string is parsed. Taking
    // `RawQuery` (not `Query<_>`) keeps this first. Shared with the other read routes.
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

    // Offload the read onto the Tokio blocking pool (mirrors query's scan) so it
    // never runs inline on the Axum executor. `load_sprint_registry_from` encodes
    // tolerance in its return shape (empty registry + warnings) and never errors —
    // the `Err` arm is defensive only. SW-SB-24: the ledger lives in the team's
    // `config/` sibling of `content_root`, not under `content/`.
    let result: Result<SprintRegistry, String> =
        crate::commands::await_blocking("load_sprint_registry_route", move || {
            let config_dir = team_config_dir_beside_content(std::path::Path::new(&content_root));
            Ok(
                crate::commands::sprint_registry::load_sprint_registry_from_for(
                    &config_dir,
                    &scope,
                ),
            )
        })
        .await;

    match result {
        Ok(registry) => (StatusCode::OK, Json(registry)).into_response(),
        Err(e) => {
            log::warn!(target: "http", "sprints: load failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "load-failed", "detail": e })),
            )
                .into_response()
        }
    }
}

/// `GET /api/v1/sprints/{id}/stories` (SW-SB-24 AC-6) — the sprint's def plus its
/// **derived** member stories (the stories tagged `sprint: <id>`). Same token gate
/// and content-root resolution as `sprints`; the membership scan is offloaded onto
/// the blocking pool. Reuses the shared `sprint_member_stories` producer. An
/// unknown id yields `sprint: null` with empty `stories` (200, never a 404/500).
///
/// `Path<String>` for `:id` cannot reject a present segment, so the token gate
/// still runs first in the body — the fail-closed ordering is preserved.
pub async fn sprint_stories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let scoped_grant = match gate_read_token(&state, &headers) {
        Ok(grant) => grant,
        Err(resp) => return resp,
    };

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

    // SR-18: warm the membership scan off the app-managed frontmatter cache.
    // The O(1) snapshot is taken here on the async plane and moved into the
    // blocking closure, so drilling a sprint's members no longer cold-parses the
    // whole content tree. `AppState` carries the same `AppHandle` a Tauri command
    // would, so this reaches the identical managed cache.
    let cache = crate::commands::pipeline_scanner::frontmatter_cache_snapshot(&state.app_handle);
    let result: Result<SprintStories, String> =
        crate::commands::await_blocking("sprint_stories_route", move || {
            // The read token grants a content_root; the ledger is its `config/`
            // sibling (the route has no separate org_root/team to build from).
            let content_dir = std::path::Path::new(&content_root);
            Ok(crate::commands::sprint_registry::sprint_member_stories_for(
                &team_config_dir_beside_content(content_dir),
                content_dir,
                &id,
                cache.as_deref(),
                &scope,
            ))
        })
        .await;

    match result {
        Ok(payload) => (StatusCode::OK, Json(payload)).into_response(),
        Err(e) => {
            log::warn!(target: "http", "sprint_stories: load failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "load-failed", "detail": e })),
            )
                .into_response()
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/sprints", get(sprints))
        .route("/api/v1/sprints/{id}/stories", get(sprint_stories))
}

#[cfg(test)]
mod tests {
    // Read-only guard, mirroring `query_router_is_get_only` / `proxy_router_is_get_only`:
    // driving the handler via `oneshot` needs a real `AppHandle` (see proxy.rs), so the
    // GET-only binding is asserted structurally over the router source. A future write
    // binding here is a deliberate change that must trip this test.
    #[test]
    fn sprints_router_is_get_only() {
        let src = include_str!("sprints.rs");
        let start = src.find("pub fn router()").expect("router() fn present");
        let end = start
            + src[start..]
                .find("\n}")
                .expect("router() fn has a closing brace");
        let body = &src[start..end];
        assert!(
            body.contains("get(sprints)"),
            "sprints router must bind GET"
        );
        assert!(
            body.contains("get(sprint_stories)"),
            "sprint-stories route must bind GET (AC-6)"
        );
        for method in ["post(", "put(", "delete(", "patch(", "any("] {
            assert!(
                !body.contains(method),
                "sprints router must not bind {method}"
            );
        }
    }

    // SR-18 seam: the runtime callers reach the warm cache through their
    // `AppHandle` → managed `SharedFrontmatterCache` → `frontmatter_cache_snapshot`.
    // The full handler can't be driven hermetically (its auth gate +
    // `resolve_read_content_root` require real state under the user's
    // `~/.codesense/users/`, and `AppState` is `Wry`-locked so a mock runtime
    // can't populate it), so this pins the exact wiring the handler depends on
    // with a mock `AppHandle`: a registered cache is snapshotted and threaded into
    // `sprint_member_stories`, and an unregistered one degrades to the cold path —
    // the regression a silent revert to `None` would cause.
    use crate::commands::sprint_registry::sprint_member_stories;
    use crate::install::frontmatter_cache::FrontmatterCache;
    use std::path::Path;
    use tauri::Manager;

    fn write_member(content_dir: &Path, name: &str, sprint: Option<&str>) {
        let sprint_line = sprint.map(|s| format!("sprint: {s}\n")).unwrap_or_default();
        let body = format!(
            "---\ntype: story\nstatus: todo\nsource_repo: alderco/codesense\n{sprint_line}---\n\n# {name}\n"
        );
        std::fs::write(content_dir.join(name), body).unwrap();
    }

    fn mock_app() -> tauri::App<tauri::test::MockRuntime> {
        tauri::test::mock_builder()
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .expect("mock app builds")
    }

    #[test]
    fn app_state_handle_reaches_the_warm_cache_and_threads_it_into_the_producer() {
        let tmp = tempfile::tempdir().unwrap();
        let content_dir = tmp.path().join("content");
        // No ledger is written — membership is derived from the content scan, so the
        // producer's `sprint` field is `None` on both sides and the comparison
        // isolates the warm-vs-cold scan (the wiring under test).
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&content_dir).unwrap();
        write_member(&content_dir, "s1.md", Some("2026-08-A"));
        write_member(&content_dir, "s2.md", Some("2026-08-A"));
        write_member(&content_dir, "s3.md", Some("2026-08-B"));
        write_member(&content_dir, "s4.md", None);

        // Warm a cache over the fixtures and register it exactly as production does.
        let mut cache = FrontmatterCache::new();
        for name in ["s1.md", "s2.md", "s3.md", "s4.md"] {
            let path = content_dir.join(name);
            let entry = FrontmatterCache::prepare_refresh(&path, "default");
            cache.commit_refresh(&path, entry);
        }
        assert_eq!(cache.entries.len(), 4, "fixtures must be index-eligible");

        let app = mock_app();
        app.handle().manage(FrontmatterCache::shared(cache));

        // The handler's exact snapshot call — reaches the managed cache off the
        // app handle (`AppState` is a trivial `{ app_handle }` forwarder over this).
        let snap = crate::commands::pipeline_scanner::frontmatter_cache_snapshot(app.handle())
            .expect("registered cache must be reachable via the app handle");
        assert_eq!(snap.entries.len(), 4, "snapshot carries the warm entries");

        // Threading that snapshot into the producer (as the handler does) yields the
        // same membership as the cold path — the warm arm is wired end to end.
        let warm = sprint_member_stories(&config_dir, &content_dir, "2026-08-A", Some(&snap));
        let cold = sprint_member_stories(&config_dir, &content_dir, "2026-08-A", None);
        assert_eq!(warm, cold, "warm handler path must equal the cold path");
        let mut names: Vec<&str> = warm.stories.iter().map(|s| s.file_name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["s1.md", "s2.md"]);
    }

    #[test]
    fn unregistered_cache_degrades_the_snapshot_to_none() {
        // Early boot / headless: no `SharedFrontmatterCache` managed. The handler
        // must degrade to the cold scan (snapshot is `None`), never panic — and it
        // must keep degrading on every repeat call, not just the first. The warn is
        // gated to fire once per process, but the None fallback is unconditional.
        let app = mock_app();
        for _ in 0..3 {
            let snap = crate::commands::pipeline_scanner::frontmatter_cache_snapshot(app.handle());
            assert!(
                snap.is_none(),
                "an unregistered cache must snapshot to None (cold fallback) on every call"
            );
        }
    }
}
