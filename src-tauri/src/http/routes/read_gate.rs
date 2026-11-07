// SPDX-License-Identifier: MIT
//! The token gate and content-root resolution shared by the read-only HTTP
//! surfaces (`/api/v1/sprints`, `/api/v1/taxonomy`).
//!
//! TOKEN GATE: a request must present either the per-session shared secret or a
//! frame capability or legacy scoped read token in the
//! `x-codesense-proxy-token` header. A missing or mismatched token is rejected
//! 401 before any query-string parsing runs, so routes take
//! [`axum::extract::RawQuery`] and call [`gate_read_token`] first.
//!
//! CONTAINMENT: the content root a route reads is canonicalized and required to
//! be an existing directory inside the codesense-managed area
//! (`{codesense_home}/users/`). Open frames retain cross-team reads; restricted
//! frame grants and legacy scoped tokens are clamped to their own root.

use std::path::{Path, PathBuf};

use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use tauri::Manager;

use crate::http::{AppState, ProxyCapabilityState};

const CAP_HEADER: &str = "x-codesense-proxy-token";

/// The query-string params a read route honors (parsed leniently, after the gate).
#[derive(Debug, Default)]
pub struct ReadParams {
    /// Absolute managed content path; honored by open frames and shared-secret callers.
    pub content_root: Option<String>,
}

/// Parse the raw query string leniently; unknown keys are ignored, never a 400.
pub(super) fn parse_raw_query(raw: Option<&str>) -> ReadParams {
    let mut p = ReadParams::default();
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or("").as_bytes()) {
        if key == "content_root" {
            p.content_root = Some(value.into_owned());
        }
    }
    p
}

/// Containment outcome for a caller-supplied `content_root` (FINDING 3).
#[derive(Debug)]
pub(super) enum ContentRootError {
    /// `content_root` was absent or empty.
    Missing,
    /// `content_root` couldn't be canonicalized (doesn't exist) or isn't a dir.
    NotResolvable,
    /// `content_root` canonicalized fine but lands OUTSIDE the managed area.
    OutsideManaged,
}

/// Authenticated read authority. Legacy read tokens keep their single-root
/// clamp; frame records carry the effective open/restricted policy.
#[derive(Debug)]
pub(super) enum ReadGrant {
    Scoped(crate::dashboard_stream::ScopedToken),
    Frame {
        content_root: String,
        allow_other_roots: bool,
    },
}

/// Select before managed-area validation. Open frames may choose another root,
/// defaulting to their own. Restricted frames and legacy scoped tokens retain
/// their root clamp; shared-secret callers provide an explicit root.
pub(super) fn select_requested_root<'a>(
    scoped_grant: Option<&'a ReadGrant>,
    param: Option<&'a str>,
) -> Option<&'a str> {
    match scoped_grant {
        Some(ReadGrant::Scoped(grant)) => Some(grant.content_root.as_str()),
        Some(ReadGrant::Frame {
            content_root,
            allow_other_roots,
        }) => {
            if *allow_other_roots {
                param.or(Some(content_root))
            } else {
                Some(content_root)
            }
        }
        None => param,
    }
}

/// Validate that `content_root` is an existing directory CONTAINED within the
/// codesense-managed content area (`{codesense_home}/users/`). Returns the
/// canonicalized path on success. This is the FINDING 3 gate: a token holder
/// must NOT be able to scan arbitrary filesystem locations (`/etc`, `~/.ssh`).
///
/// Any managed content dir under `users/` is in bounds: a read token reads this Mac's
/// CodeSense content, not one team's (SECURITY.md, "How CodeSense trusts agents").
pub(super) fn validate_content_root(
    content_root: Option<&str>,
    codesense_home: &Path,
) -> Result<PathBuf, ContentRootError> {
    let raw = content_root
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(ContentRootError::Missing)?;

    // Must exist + canonicalize (resolves symlinks + `..`); reject otherwise so a
    // non-existent or symlink-escaping path can't slip past the prefix check.
    let canonical = std::fs::canonicalize(raw).map_err(|_| ContentRootError::NotResolvable)?;
    if !canonical.is_dir() {
        return Err(ContentRootError::NotResolvable);
    }

    // The managed content area: `{codesense_home}/users/`. Canonicalize the
    // anchor too so the `starts_with` compares like-for-like (both symlink-
    // resolved). If the anchor itself can't canonicalize (fresh install, no
    // `users/` yet) there is nothing legitimately in-bounds → reject.
    let users_root = codesense_home.join(crate::context_repo::USERS_DIR);
    let managed =
        std::fs::canonicalize(&users_root).map_err(|_| ContentRootError::OutsideManaged)?;
    if !canonical.starts_with(&managed) {
        return Err(ContentRootError::OutsideManaged);
    }
    Ok(canonical)
}

/// Shared production frame authentication, also exercised by the isolated
/// browser server without constructing a Tauri application.
pub(super) fn frame_read_grant(
    frames: &crate::dashboard_stream::frame::FrameStore,
    port: u16,
    headers: &HeaderMap,
) -> Option<Result<ReadGrant, StatusCode>> {
    let frame = frames.by_host(headers.get(axum::http::header::HOST)?.to_str().ok()?, port)?;
    let origin = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|o| o.to_str().ok())
        .unwrap_or(&frame.origin);
    let token = headers
        .get(CAP_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    Some(
        if frames
            .resolve(origin, token)
            .is_some_and(|g| g.frame_id == frame.frame_id)
        {
            if crate::verbs::check_frame_capability(&frame, "context", &serde_json::json!({}))
                .is_err()
            {
                return Some(Err(StatusCode::FORBIDDEN));
            }
            Ok(ReadGrant::Frame {
                allow_other_roots: frame.grant.scope == "open"
                    && !frame.policy.own_folder_only
                    && !frame.policy.team_safe,
                content_root: frame.content_root,
            })
        } else {
            Err(StatusCode::UNAUTHORIZED)
        },
    )
}

/// TOKEN GATE for the read-only HTTP surfaces. Shared so every read route fails closed identically and
/// the security-critical ordering lives in ONE place. Returns:
/// - `Ok(None)` — a valid SHARED-secret caller (external CLI / hooks / editors);
/// - `Ok(Some(grant))` — a frame policy or legacy scoped read grant. Root
///   selection preserves open-frame access and clamps restricted grants;
/// - `Err(response)` — 401 (missing/invalid token) or 500 (capability state
///   unavailable). The gate reads only headers, so it MUST run before any query
///   string is parsed (see the module `TOKEN GATE` note).
// The `Err` variant is a ready-to-return HTTP `Response` (intentionally large);
// boxing it just to satisfy `result_large_err` would add noise to every call site
// for an internal route helper. Same rationale on `resolve_read_content_root`.
#[allow(clippy::result_large_err)]
pub(super) fn gate_read_token(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<ReadGrant>, Response> {
    let presented = headers
        .get(CAP_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Frame credentials must resolve against the requested origin and host;
    // a valid token from another frame is never a shared-secret fallback.
    if let (Some(frames), Some(port)) = (
        state
            .app_handle
            .try_state::<std::sync::Arc<crate::dashboard_stream::frame::FrameStore>>(),
        state.app_handle.try_state::<crate::http::HttpApiPort>(),
    ) {
        if let Some(result) = frame_read_grant(&frames, port.0, headers) {
            return result.map(Some).map_err(IntoResponse::into_response);
        }
    }
    let shared_ok = match state.app_handle.try_state::<ProxyCapabilityState>() {
        Some(cap) => cap.matches(presented),
        None => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": "read-unavailable" })),
            )
                .into_response())
        }
    };
    // A scoped read token (viewer-minted, TTL-bounded, single-root) is accepted
    // in place of the shared secret — `EventSource`/`fetch` from a sandboxed
    // dashboard can only carry it. Reuse DX-3's `validate` (not a parallel check).
    let scoped_grant = if shared_ok {
        None
    } else {
        state
            .app_handle
            .try_state::<crate::dashboard_stream::DashboardStreamHub>()
            .and_then(|hub| hub.tokens().validate(presented))
    };
    if !shared_ok && scoped_grant.is_none() {
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response());
    }
    Ok(scoped_grant.map(ReadGrant::Scoped))
}

/// Select according to the authenticated read grant, then validate the selected
/// root against the managed content area. Open frame requests honor an explicit
/// root and default to their own; restricted/legacy grants remain clamped.
#[allow(clippy::result_large_err)] // see `gate_read_token` — Err is an HTTP Response.
pub(super) fn resolve_read_content_root(
    scoped_grant: Option<&ReadGrant>,
    caller_root: Option<&str>,
) -> Result<String, Response> {
    let Some(home) = crate::commands::paths::codesense_home_dir() else {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "read-unavailable" })),
        )
            .into_response());
    };
    let requested_root = select_requested_root(scoped_grant, caller_root);
    match validate_content_root(requested_root, &home) {
        Ok(canonical) => {
            if scoped_grant.is_some() {
                Ok(requested_root.unwrap_or_default().to_string())
            } else {
                Ok(canonical.to_string_lossy().into_owned())
            }
        }
        Err(ContentRootError::Missing) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "missing content_root" })),
        )
            .into_response()),
        Err(ContentRootError::NotResolvable) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "invalid content_root" })),
        )
            .into_response()),
        Err(ContentRootError::OutsideManaged) => Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "content_root not in managed area" })),
        )
            .into_response()),
    }
}

/// Architecture: use the authenticated frame host, independently of its selected root.
pub(super) fn caller_scope(
    state: &AppState,
    headers: &HeaderMap,
    grant: Option<&ReadGrant>,
) -> Result<crate::context_repo::caller_scope::CallerScope, Response> {
    let scope = if let Some(frame) =
        crate::http::frame_boundary::request_frame(&state.app_handle, headers)
    {
        crate::context_repo::caller_scope::for_page(&state.app_handle, None, Some(&frame.frame_id))
    } else if let Some(ReadGrant::Scoped(token)) = grant {
        Ok(
            crate::context_repo::caller_scope::for_content(Path::new(&token.content_root))
                .unwrap_or(crate::context_repo::caller_scope::CallerScope::Unknown),
        )
    } else {
        Ok(crate::context_repo::caller_scope::CallerScope::Human)
    };
    scope.map_err(|_| StatusCode::NOT_FOUND.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sdk_sprints_reads_require_context_capability_after_frame_authentication() {
        use crate::dashboard_stream::frame::{
            FrameCallerKind, FrameGrant, FramePolicy, FrameStore,
        };
        let frames = FrameStore::default();
        for (caps, expected) in [
            (vec!["sessions:read".into()], false),
            (vec!["context:read".into()], true),
        ] {
            let frame = frames.mint_with_grant(
                9321,
                FrameGrant {
                    frame_id: String::new(),
                    principal: "human".into(),
                    dashboard: "board".into(),
                    slot: "right-pane".into(),
                    team: None,
                    scope: "private".into(),
                },
                "/ctx".into(),
                "/ctx/board.html".into(),
                "/ctx".into(),
                FramePolicy {
                    team_safe: true,
                    ..Default::default()
                },
                None,
                caps,
                FrameCallerKind::Private,
            );
            let mut headers = HeaderMap::new();
            headers.insert(
                "host",
                frame.origin.trim_start_matches("http://").parse().unwrap(),
            );
            headers.insert("origin", frame.origin.parse().unwrap());
            headers.insert(CAP_HEADER, frame.token.parse().unwrap());
            let result = frame_read_grant(&frames, 9321, &headers).unwrap();
            assert_eq!(result.is_ok(), expected);
            if !expected {
                assert_eq!(result.unwrap_err(), StatusCode::FORBIDDEN);
            }
        }
    }

    #[test]
    fn frame_read_auth_binds_host_origin_token_and_root() {
        use crate::dashboard_stream::frame::{FrameGrant, FramePolicy, FrameStore};
        let frames = FrameStore::default();
        let mint = |root: &str| {
            frames.mint(
                9321,
                FrameGrant {
                    frame_id: String::new(),
                    principal: "human".into(),
                    dashboard: "board".into(),
                    slot: "centre".into(),
                    team: None,
                    scope: "open".into(),
                },
                root.into(),
                "board.html".into(),
                String::new(),
                FramePolicy::default(),
                None,
            )
        };
        let own = mint("/managed/own");
        let other = mint("/managed/other");
        let mut headers = HeaderMap::new();
        headers.insert(
            "host",
            own.origin.trim_start_matches("http://").parse().unwrap(),
        );
        headers.insert("origin", own.origin.parse().unwrap());
        headers.insert(CAP_HEADER, own.token.parse().unwrap());
        let grant = frame_read_grant(&frames, 9321, &headers).unwrap().unwrap();
        assert_eq!(
            select_requested_root(Some(&grant), Some("/managed/other")),
            Some("/managed/other")
        );
        assert_eq!(
            select_requested_root(Some(&grant), None),
            Some("/managed/own")
        );
        let ReadGrant::Frame { content_root, .. } = grant else {
            panic!("frame grant");
        };
        let restricted = ReadGrant::Frame {
            content_root,
            allow_other_roots: false,
        };
        assert_eq!(
            select_requested_root(Some(&restricted), Some("/managed/other")),
            Some("/managed/own")
        );
        headers.insert("origin", other.origin.parse().unwrap());
        assert_eq!(
            frame_read_grant(&frames, 9321, &headers)
                .unwrap()
                .unwrap_err(),
            StatusCode::UNAUTHORIZED
        );
        headers.insert("origin", own.origin.parse().unwrap());
        headers.insert(CAP_HEADER, other.token.parse().unwrap());
        assert_eq!(
            frame_read_grant(&frames, 9321, &headers)
                .unwrap()
                .unwrap_err(),
            StatusCode::UNAUTHORIZED
        );
        frames.release(&own.frame_id);
        assert!(frame_read_grant(&frames, 9321, &headers).is_none());
    }

    // (a) No token → the gate rejects. We test the underlying matcher the
    // handler uses; a full HTTP round-trip needs a Tauri `AppHandle` for
    // `AppState`, which the repo has no mock for (same constraint proxy.rs
    // documents). `matches("")` on any real secret is false → 401 branch.
    #[test]
    fn empty_token_is_rejected_by_the_gate() {
        let cap = ProxyCapabilityState::new_random();
        assert!(!cap.matches(""), "missing token must not match → 401");
        assert!(cap.matches(&cap.0), "the real secret matches → passes gate");
    }

    // SW-DX-4 AC-11: the route accepts EITHER the shared secret OR a per-render
    // scoped read token, and the two are DISJOINT auth paths. A scoped token must
    // never pass the shared-secret gate (else it would skip the containment
    // clamp), and the shared secret must not resolve as a scoped grant. The
    // scoped grant carries the `content_root` the handler clamps the scan to.
    #[test]
    fn scoped_read_token_and_shared_secret_are_disjoint_auth_paths() {
        use crate::dashboard_stream::{ScopedCapabilityStore, ScopedToken};
        let shared = ProxyCapabilityState::new_random();
        let store = ScopedCapabilityStore::default();
        let read_tok = store.mint(ScopedToken {
            content_root: "/managed/root".to_string(),
        });
        // A scoped read token is NOT the shared secret (would bypass containment).
        assert!(
            !shared.matches(&read_tok),
            "a scoped read token must not pass the shared-secret gate"
        );
        // The shared secret is NOT a scoped grant.
        assert!(
            store.validate(&shared.0).is_none(),
            "the shared secret must not resolve as a scoped token"
        );
        // The scoped grant resolves the root the handler clamps the scan to.
        assert_eq!(
            store.validate(&read_tok).unwrap().content_root,
            "/managed/root"
        );
    }

    // SW-DX-4 AC-11: the scan-target clamp. A scoped token's granted root ALWAYS
    // wins over a caller-supplied `content_root` param (a hostile param cannot
    // redirect a scoped scan); the shared-secret path uses the caller's param.
    // BOTH results are containment-validated downstream — this proves the branch.
    #[test]
    fn scoped_grant_root_clamps_over_hostile_param() {
        use crate::dashboard_stream::ScopedToken;
        let grant = ReadGrant::Scoped(ScopedToken {
            content_root: "/managed/root".to_string(),
        });
        // Scoped: grant root wins even when a hostile param tries to redirect it.
        assert_eq!(
            select_requested_root(Some(&grant), Some("/etc")),
            Some("/managed/root")
        );
        assert_eq!(
            select_requested_root(Some(&grant), None),
            Some("/managed/root")
        );
        // Shared secret (no grant): the caller's param is used (then validated).
        assert_eq!(
            select_requested_root(None, Some("/some/root")),
            Some("/some/root")
        );
        assert_eq!(select_requested_root(None, None), None);
    }

    #[test]
    fn content_root_inside_managed_area_is_accepted() {
        // A fake codesense_home with a real `users/.../content` dir under it.
        let home = std::env::temp_dir().join(format!("dx2-home-in-{}", std::process::id()));
        let content = home
            .join(crate::context_repo::USERS_DIR)
            .join("42/orgs/acme/api/default/content/epic-a");
        fs::create_dir_all(&content).unwrap();

        let ok = validate_content_root(Some(&content.display().to_string()), &home)
            .expect("in-managed content_root accepted");
        // Returns the canonicalized dir.
        assert_eq!(ok, std::fs::canonicalize(&content).unwrap());

        // Empty / missing → Missing.
        assert!(matches!(
            validate_content_root(Some(""), &home),
            Err(ContentRootError::Missing)
        ));
        assert!(matches!(
            validate_content_root(None, &home),
            Err(ContentRootError::Missing)
        ));

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn content_root_outside_managed_area_is_rejected() {
        // Managed home has a `users/` dir, but the requested root is a temp dir
        // OUTSIDE it → OutsideManaged (the arbitrary-fs-read guard).
        let home = std::env::temp_dir().join(format!("dx2-home-out-{}", std::process::id()));
        fs::create_dir_all(home.join(crate::context_repo::USERS_DIR)).unwrap();

        let outside = std::env::temp_dir().join(format!("dx2-outside-{}", std::process::id()));
        fs::create_dir_all(&outside).unwrap();

        assert!(matches!(
            validate_content_root(Some(&outside.display().to_string()), &home),
            Err(ContentRootError::OutsideManaged)
        ));
        // A real system dir outside the managed home is likewise rejected.
        assert!(matches!(
            validate_content_root(Some("/etc"), &home),
            Err(ContentRootError::OutsideManaged)
        ));

        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn content_root_that_does_not_exist_is_not_resolvable() {
        let home = std::env::temp_dir().join(format!("dx2-home-nx-{}", std::process::id()));
        fs::create_dir_all(home.join(crate::context_repo::USERS_DIR)).unwrap();
        let missing = home
            .join(crate::context_repo::USERS_DIR)
            .join("does/not/exist");
        assert!(matches!(
            validate_content_root(Some(&missing.display().to_string()), &home),
            Err(ContentRootError::NotResolvable)
        ));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn parses_content_root_and_ignores_the_rest() {
        let p = parse_raw_query(Some("content_root=%2Fa%2Fb&status=todo"));
        assert_eq!(p.content_root.as_deref(), Some("/a/b"));
        assert!(parse_raw_query(None).content_root.is_none());
    }
}
