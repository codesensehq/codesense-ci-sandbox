// SPDX-License-Identifier: MIT
use super::*;
use std::sync::{Mutex, OnceLock};
use tauri::State;
#[derive(Default)]
pub struct VerbState(pub Mutex<Dispatcher>);
fn now() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
}
pub(crate) async fn read_policy<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    principal: &str,
) -> Result<Policy, String> {
    let raw = crate::commands::system::load_app_state(app, Some(principal.into())).await?;
    let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    serde_json::from_value(
        v.get("dashboardPolicy")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    )
    .map_err(|e| e.to_string())
}
#[tauri::command]
pub async fn verb_dispatch(
    app: tauri::AppHandle,
    state: State<'_, VerbState>,
    caller: Caller,
    expected_principal: String,
    verb: String,
    args: serde_json::Value,
    flushing: Option<bool>,
) -> Result<Decision, String> {
    // A frame's actor can be a stored message sender. Policy belongs to the
    // renderer's immutable local profile; load_app_state fences profile changes.
    let policy = read_policy(app.clone(), &expected_principal).await?;
    use tauri::Manager;
    let frame = app
        .state::<std::sync::Arc<crate::dashboard_stream::frame::FrameStore>>()
        .by_id(&caller.frame_id);
    let mut dispatcher = state.0.lock().map_err(|e| e.to_string())?;
    dispatcher.set_flushing(&caller.frame_id, flushing.unwrap_or(false));
    let decision =
        dispatch_registered_frame(&mut dispatcher, frame.as_ref(), &verb, args, &policy, now());
    dispatcher.set_flushing(&caller.frame_id, false);
    Ok(decision)
}

fn dispatch_registered_frame(
    dispatcher: &mut Dispatcher,
    frame: Option<&crate::dashboard_stream::frame::FrameRecord>,
    verb: &str,
    args: serde_json::Value,
    policy: &Policy,
    at: u64,
) -> Decision {
    match frame {
        Some(frame) => dispatcher.dispatch_frame(frame, verb, args, policy, at),
        None => Decision::Deny {
            reason: "denied: frame released".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dashboard_stream::frame::{FrameGrant, FramePolicy, FrameStore};
    #[test]
    fn releasing_a_frame_between_resolution_and_admission_never_grants_full_access() {
        let frames = FrameStore::default();
        let frame = frames.mint(
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
            FramePolicy::default(),
            None,
        );
        let caller = Caller::from(&frame);
        frames.release(&frame.frame_id);
        assert_eq!(
            dispatch_registered_frame(
                &mut Dispatcher::default(),
                frames.by_id(&caller.frame_id).as_ref(),
                "sessions",
                serde_json::json!({}),
                &Policy::default(),
                0
            ),
            Decision::Deny {
                reason: "denied: frame released".into()
            }
        );
    }
}

#[tauri::command]
pub fn verb_answer(
    state: State<'_, VerbState>,
    confirm_id: String,
    allow: bool,
) -> Result<Call, String> {
    state
        .0
        .lock()
        .map_err(|e| e.to_string())?
        .answer(&confirm_id, allow)
}
#[tauri::command]
pub fn verb_settle(
    state: State<'_, VerbState>,
    call_id: String,
    landed: bool,
    reason: String,
) -> Result<(), String> {
    Dispatcher::settle(
        &mut *state.0.lock().map_err(|e| e.to_string())?,
        &call_id,
        landed,
        &reason,
    )
}
