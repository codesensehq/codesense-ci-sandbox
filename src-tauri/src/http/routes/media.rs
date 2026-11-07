// SPDX-License-Identifier: MIT
use crate::{commands::agent_inbox::AppInboxHost, http::AppState};
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Extension, Router,
};

pub fn router() -> Router<AppState> {
    Router::new().route("/media/v1/{sha256}", get(serve))
}

async fn serve(
    State(state): State<AppState>,
    frame: Option<Extension<crate::dashboard_stream::frame::FrameRecord>>,
    Path(sha): Path<String>,
) -> Response {
    // The frame boundary has already admitted this request for its frame: a
    // same-origin asset request (such as this `<img>`) by its frame host, since
    // WebKit sends no SameSite cookie from a cross-site embed; anything else only
    // with the frame's token. Grant and visibility below bind to that frame.
    let Some(Extension(frame)) = frame else {
        return StatusCode::FORBIDDEN.into_response();
    };
    if !inbox_granted(&frame) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let host = AppInboxHost::port(state.app_handle.clone());
    let teams =
        if crate::verbs::check_frame_capability(&frame, "inboxRollup", &serde_json::json!({}))
            .is_ok()
        {
            host.synced_teams().await.unwrap_or_default()
        } else {
            frame
                .grant
                .team
                .iter()
                .map(|t| crate::commands::dashboard_launch::LaunchTeam {
                    org: t.org.clone(),
                    team: t.team.clone(),
                })
                .collect()
        };
    let mut allowed = false;
    for team in teams {
        if let Ok(view) = crate::commands::agent_inbox::list_messages_unbounded(
            &*host,
            &team,
            crate::commands::agent_inbox::InboxScope::All,
            None,
            &[],
        )
        .await
        {
            if view
                .messages
                .iter()
                .flat_map(|m| &m.attachments)
                .any(|a| a.state == "cached" && a.sha256.as_deref() == Some(&sha))
            {
                allowed = true;
                break;
            }
        }
    }
    if !allowed {
        return StatusCode::FORBIDDEN.into_response();
    }
    let app = state.app_handle;
    match crate::commands::await_blocking("media.serve", move || crate::media::cached(&app, &sha))
        .await
    {
        Ok(bytes) => image_response(bytes),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn inbox_granted(frame: &crate::dashboard_stream::frame::FrameRecord) -> bool {
    crate::verbs::check_frame_capability(frame, "inbox", &serde_json::json!({})).is_ok()
}
fn image_response(bytes: Vec<u8>) -> Response {
    match crate::media::inspect(&bytes) {
        Ok((mime, _, _)) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable",
                ),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard_stream::frame::{FrameCallerKind, FrameGrant, FramePolicy, FrameStore};
    #[tokio::test]
    async fn media_bytes_require_inbox_read_and_serve_verified_type_and_cache_headers() {
        let frames = FrameStore::default();
        let grant = FrameGrant {
            frame_id: String::new(),
            principal: "human".into(),
            dashboard: "inbox".into(),
            slot: "right-pane".into(),
            team: None,
            scope: "open".into(),
        };
        let mut frame = frames.mint_with_grant(
            9321,
            grant,
            "/ctx".into(),
            "builtin:inbox".into(),
            "/ctx".into(),
            FramePolicy::default(),
            None,
            vec!["context:read".into()],
            FrameCallerKind::Builtin,
        );
        assert!(!inbox_granted(&frame));
        frame.effective_grant = vec!["inbox:read:team".into()];
        assert!(inbox_granted(&frame));
        let response = image_response(b"GIF89a\x02\x00\x03\x00".to_vec());
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/gif");
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert!(response.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("immutable"));
        assert_eq!(
            image_response(b"<svg/>".to_vec()).status(),
            StatusCode::NOT_FOUND
        );
    }
}
