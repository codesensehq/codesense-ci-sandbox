// SPDX-License-Identifier: MIT
//! Persistent acknowledgements of an individual invocation's exit mark.
use codesense_pty_core::activity::Snapshot;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::Manager;

pub(crate) fn crash_id(snapshot: &Snapshot) -> Option<String> {
    if snapshot.exit.is_none()
        || !matches!(
            snapshot.state,
            codesense_pty_core::activity::State::Crashed
                | codesense_pty_core::activity::State::Errored
        )
    {
        return None;
    }
    if snapshot.reason.as_deref() == Some(codesense_pty_core::activity::ENGINE_STOPPED) {
        return Result::ok(serde_json::to_string(&(
            &snapshot.identity,
            codesense_pty_core::activity::ENGINE_STOPPED,
            snapshot.turn_generation,
        )));
    }
    serde_json::to_string(&(&snapshot.identity, &snapshot.exit, snapshot.turn_generation)).ok()
}

pub(crate) fn legacy_crash_id(
    session: &str,
    observation: crate::agent_state::TurnObservation,
) -> Option<String> {
    observation.exit.filter(|exit| exit.abnormal).map(|exit| {
        serde_json::json!(["legacy", session, observation.incarnation, exit.sequence]).to_string()
    })
}

#[derive(Default)]
pub(crate) struct AgentMarks(Mutex<Option<BTreeMap<String, String>>>);
impl AgentMarks {
    pub(crate) fn load(&self, path: &Path) -> Result<(), String> {
        let mut cache = self.0.lock().unwrap();
        if cache.is_none() {
            *cache = Some(match std::fs::read(path) {
                Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(e) => return Err(e.to_string()),
            });
        }
        Ok(())
    }
    pub(crate) fn dismissed(&self, session: &str, crash: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|marks| marks.get(session).is_some_and(|id| id == crash))
    }
    fn edit(
        &self,
        path: &Path,
        edit: impl FnOnce(&mut BTreeMap<String, String>),
    ) -> Result<(), String> {
        self.load(path)?;
        let mut cache = self.0.lock().unwrap();
        let mut next = cache.clone().unwrap_or_default();
        edit(&mut next);
        if cache.as_ref() == Some(&next) {
            return Ok(());
        }
        crate::content_engine::atomic_io::write_tmp_fsync_rename(
            path,
            &serde_json::to_vec(&next).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        *cache = Some(next);
        Ok(())
    }
    pub(crate) fn retain(&self, path: &Path, sessions: &BTreeSet<String>) -> Result<(), String> {
        self.edit(path, |marks| {
            marks.retain(|session, _| sessions.contains(session))
        })
    }
}
pub(crate) fn path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(crate::commands::system::app_state_path(app)?.with_file_name("agent-mark-dismissals.json"))
}

pub(crate) fn admit(row: Option<&Value>, view_key: &str) -> Result<(), String> {
    let row = row.ok_or("unavailable: session is retired")?;
    let scope = row["team"]["org"]
        .as_str()
        .zip(row["team"]["team"].as_str())
        .map(|(org, team)| super::view_key::team_key(org, team));
    if !super::view_key::admits(view_key, scope.as_deref()) {
        return Err("denied: session outside grant".into());
    }
    Ok(())
}

// Architecture: native, MCP and page bindings share this exact crash-keyed write.
// Policy: anyone who can read the session can dismiss it, without confirmation.
fn dismiss_in(
    hub: &super::LiveHub,
    marks: &AgentMarks,
    path: &Path,
    session_id: &str,
    crash_id: &str,
    view_key: &str,
    request_id: Option<String>,
) -> Result<Value, String> {
    let mut model = hub.model.lock().unwrap();
    let rows = model.projections.snapshot("sessions", "all")?.snapshot;
    let row = rows
        .iter()
        .find(|row| row["id"] == session_id || row["sessionId"] == session_id);
    admit(row, view_key)?;
    let row = row.unwrap();
    if row["crashId"].as_str() != Some(crash_id) && !marks.dismissed(session_id, crash_id) {
        return Err("conflict: mark changed".into());
    }
    marks.edit(path, |marks| {
        marks.insert(session_id.into(), crash_id.into());
    })?;
    model.status_request_id = request_id.clone();
    drop(model);
    hub.invalidate(super::REBUILD);
    Ok(
        serde_json::json!({"sessionId":session_id,"crashId":crash_id,"dismissed":true,"requestId":request_id}),
    )
}

pub(crate) async fn dismiss(
    app: tauri::AppHandle,
    session_id: String,
    crash_id: String,
    view_key: String,
    request_id: Option<String>,
) -> Result<Value, String> {
    crate::commands::await_blocking("dismiss_agent_mark", move || {
        dismiss_in(
            &app.state::<super::LiveHub>(),
            &app.state::<AgentMarks>(),
            &path(&app)?,
            &session_id,
            &crash_id,
            &view_key,
            request_id,
        )
    })
    .await
}

#[tauri::command]
pub async fn dismiss_agent_mark(
    app: tauri::AppHandle,
    session_id: String,
    crash_id: String,
    origin: Option<String>,
    token: Option<String>,
    request_id: Option<String>,
) -> Result<Value, String> {
    let key = match (origin, token) {
        (None, None) => "all".into(),
        (Some(origin), Some(token)) => {
            let frames = app.state::<std::sync::Arc<crate::dashboard_stream::frame::FrameStore>>();
            let grant = frames
                .resolve(&origin, &token)
                .ok_or("denied: frame identity")?;
            super::view_key::resolve(&grant).ok_or("denied: team scope requires a team")?
        }
        _ => return Err("denied: incomplete frame identity".into()),
    };
    dismiss(app, session_id, crash_id, key, request_id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persists_across_restart_prunes_retirement_and_is_specific_to_the_crash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marks.json");
        let marks = AgentMarks::default();
        marks
            .edit(&path, |marks| {
                marks.insert("s".into(), "first-exit".into());
            })
            .unwrap();
        let restarted = AgentMarks::default();
        restarted.load(&path).unwrap();
        assert!(restarted.dismissed("s", "first-exit"));
        assert!(!restarted.dismissed("s", "next-exit"));
        restarted.retain(&path, &BTreeSet::new()).unwrap();
        let retired = AgentMarks::default();
        retired.load(&path).unwrap();
        assert!(!retired.dismissed("s", "first-exit"));
    }
    #[test]
    fn legacy_cli_exit_uses_the_same_durable_mark_owner() {
        use crate::agent_state::{AgentState, SessionTurnState};
        use crate::live_hub::effective_status::*;
        use serde_json::json;
        let turns = SessionTurnState::default();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marks.json");
        turns.record("shell", AgentState::Thinking);
        turns.record_exit("shell", Some(1), false, false, false);
        assert!(
            legacy_crash_id("shell", turns.observation("shell").unwrap()).is_none(),
            "an MCP-only state does not establish a foreground CLI"
        );
        turns.record_hook("s", "UserPromptSubmit");
        turns.record_exit(
            "s",
            Some(i64::from(128 + libc::SIGTSTP)),
            false,
            false,
            false,
        );
        assert!(
            turns.observation("s").unwrap().exit.is_none(),
            "suspension is not an exit"
        );
        turns.record_exit("s", None, false, false, false);
        let observation = turns.observation("s").unwrap();
        let id = legacy_crash_id("s", observation).unwrap();
        turns.record_exit("s", Some(137), false, false, false);
        assert_eq!(
            legacy_crash_id("s", turns.observation("s").unwrap()).as_ref(),
            Some(&id)
        );
        let marks = AgentMarks::default();
        marks
            .edit(&path, |marks| {
                marks.insert("s".into(), id.clone());
            })
            .unwrap();
        let restarted = AgentMarks::default();
        restarted.load(&path).unwrap();
        let legacy = LegacySignals {
            state: Some("crashed".into()),
            observed: true,
            ended: true,
            ..Default::default()
        };
        let display = |dismissed| {
            effective_status(
                &json!({"sessionId":"s","alive":true}),
                None,
                Some(&select_live_signals(
                    None,
                    &legacy,
                    Some("crashed"),
                    None,
                    None,
                    false,
                    dismissed,
                    None,
                    None,
                    None,
                    None,
                    false,
                    0,
                )),
            )
        };
        assert_eq!(display(false), AgentStatus::Crashed);
        assert_eq!(display(restarted.dismissed("s", &id)), AgentStatus::Done);
        turns.record_hook("s", "UserPromptSubmit");
        turns.record_exit("s", Some(1), false, false, false);
        let next = legacy_crash_id("s", turns.observation("s").unwrap()).unwrap();
        assert_ne!(id, next);
        assert!(!restarted.dismissed("s", &next));
        turns.record_hook("s", "UserPromptSubmit");
        turns.record_exit("s", None, false, false, false);
        turns.record_exit("s", Some(0), false, false, false);
        assert!(legacy_crash_id("s", turns.observation("s").unwrap()).is_none());
    }
    #[test]
    fn legacy_probe_exit_requires_every_cli_to_be_absent_and_preserves_declared_failure() {
        use crate::agent_state::{AgentState, SessionTurnState};
        let hub = crate::live_hub::LiveHub::default();
        let turns = SessionTurnState::default();
        turns.record_hook("s", "UserPromptSubmit");
        let probes = vec![
            ("s".into(), "Claude".into(), Some(true)),
            ("s".into(), "Codex".into(), Some(false)),
        ];
        assert!(hub.note_process_probe(&probes).is_empty());
        assert_eq!(turns.last("s"), Some(AgentState::Thinking));
        let missing = hub.note_process_probe(&[("s".into(), "Claude".into(), Some(false))]);
        for session in missing {
            turns.record_exit(&session, None, false, false, false);
        }
        assert!(legacy_crash_id("s", turns.observation("s").unwrap()).is_some());
        turns.record_hook("failure", "StopFailure");
        turns.record_exit("failure", Some(0), false, false, false);
        assert_eq!(turns.last("failure"), Some(AgentState::Errored));
    }
    #[test]
    fn engine_end_reason_and_crash_mark_survive_projection_and_late_exit_status() {
        use crate::live_hub::{
            agent_list::sessions, effective_status::*, projection::FieldProviders,
        };
        use serde_json::json;
        let mut snapshot: Snapshot = serde_json::from_value(json!({
            "session":"s","identity":{"hostBootId":"b","sessionIncarnation":"p","invocationId":"i"},
            "revision":4,"turnGeneration":1,"provider":"codex","state":"crashed","observed":false,"limited":false,
            "reason":"engine_stopped","exit":{"code":null,"signal":null,"requested":false},"asks":[]
        })).unwrap();
        let id = crash_id(&snapshot).unwrap();
        snapshot.exit.as_mut().unwrap().signal = Some("SIGKILL".into());
        snapshot.revision += 1;
        assert_eq!(
            crash_id(&snapshot).unwrap(),
            id,
            "late status is the same failure mark"
        );
        let reason = codesense_pty_core::activity::end_reason(&snapshot).unwrap();
        let signals = select_live_signals(
            Some(&snapshot),
            &LegacySignals {
                state: Some("crashed".into()),
                ended: true,
                ..Default::default()
            },
            Some("crashed"),
            None,
            None,
            false,
            false,
            None,
            None,
            None,
            None,
            false,
            0,
        );
        let mut providers = FieldProviders::new(["id", "sessionId", "alive", "status"]);
        register(&mut providers);
        let projected = sessions::project(
            &[json!({"handle":"worker","sessionId":"s","alive":false,"status":"crashed"})],
            &[
                json!({"handle":"worker","sessionId":"s","endReason":reason,"crashId":id,
                "_statusSignals":signals}),
            ],
            &mut [],
            &providers,
        );
        assert_eq!(projected[0].1["endReason"], reason);
        assert_eq!(projected[0].1["effectiveStatus"], "crashed");
        let mut tool_rows = vec![json!({"session":"s","handle":"worker"})];
        join_list_rows(&mut tool_rows, &[projected[0].1.clone()]);
        assert_eq!(tool_rows[0]["endReason"], reason);
        assert_eq!(tool_rows[0]["crashId"], id);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marks.json");
        let marks = AgentMarks::default();
        marks
            .edit(&path, |marks| {
                marks.insert("s".into(), id.clone());
            })
            .unwrap();
        let reopened = AgentMarks::default();
        reopened.load(&path).unwrap();
        assert!(reopened.dismissed("s", &crash_id(&snapshot).unwrap()));
    }

    #[tokio::test]
    async fn dismissed_exit_reprojects_once_and_stays_dismissed_after_restart() {
        use crate::live_hub::{
            agent_list::sessions, effective_status::*, projection::FieldProviders,
        };
        use serde_json::json;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("marks.json");
        let snapshot: Snapshot = serde_json::from_value(json!({
            "session":"s","identity":{"hostBootId":"b","sessionIncarnation":"p","invocationId":"i"},
            "revision":4,"turnGeneration":1,"provider":"claude","state":"crashed","observed":true,"limited":false,
            "exit":{"code":1,"signal":null,"requested":false},"asks":[]
        })).unwrap();
        let mark = crash_id(&snapshot).unwrap();
        let render = |snapshot: &Snapshot, marks: &AgentMarks| {
            let id = crash_id(snapshot).unwrap();
            let legacy = LegacySignals {
                state: Some("crashed".into()),
                ended: true,
                observed: true,
                ..Default::default()
            };
            let signals = select_live_signals(
                Some(snapshot),
                &legacy,
                Some("crashed"),
                None,
                None,
                false,
                marks.dismissed("s", &id),
                None,
                None,
                None,
                None,
                false,
                0,
            );
            let mut providers = FieldProviders::new(["id", "sessionId", "alive", "status"]);
            register(&mut providers);
            sessions::project(
                &[json!({"handle":"worker","sessionId":"s","alive":true,"status":"running"})],
                &[json!({"handle":"worker","sessionId":"s","_statusSignals":signals,"crashId":id})],
                &mut [],
                &providers,
            )
        };
        let marks = AgentMarks::default();
        marks.load(&path).unwrap();
        let hub = super::super::LiveHub::default();
        let first = render(&snapshot, &marks);
        assert_eq!(first[0].1["effectiveStatus"], "crashed");
        let mut tool_rows = vec![json!({"session":"s","handle":"worker"})];
        join_list_rows(&mut tool_rows, &[first[0].1.clone()]);
        assert_eq!(tool_rows[0]["crashId"], first[0].1["crashId"]);
        assert_eq!(tool_rows[0]["effectiveStatus"], "crashed");
        assert!(first[0].1.get("_statusSignals").is_none());
        hub.model
            .lock()
            .unwrap()
            .projections
            .replace("sessions", first, None)
            .unwrap();
        hub.snapshot("all", "sessions").unwrap();
        let reply =
            dismiss_in(&hub, &marks, &path, "s", &mark, "all", Some("write".into())).unwrap();
        assert_eq!(reply["dismissed"], true);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), hub.next_batch())
                .await
                .unwrap(),
            super::super::REBUILD
        );
        let mut model = hub.model.lock().unwrap();
        let request_id = model.status_request_id.take();
        let registry = &mut model.projections;
        let dismissed = render(&snapshot, &marks);
        assert_eq!(dismissed[0].1["effectiveStatus"], "done");
        assert_eq!(
            registry.replace("sessions", dismissed, request_id).unwrap()[0]
                .upserts
                .len(),
            1
        );
        let restarted = AgentMarks::default();
        restarted.load(&path).unwrap();
        assert!(registry
            .replace("sessions", render(&snapshot, &restarted), None)
            .unwrap()
            .is_empty());
        let mut next = snapshot;
        next.identity.invocation_id = "next-agent".into();
        assert_eq!(render(&next, &restarted)[0].1["effectiveStatus"], "crashed");
    }
    #[test]
    fn grant_is_session_visibility_without_ownership_or_confirmation() {
        let row = serde_json::json!({"id":"s","team":{"org":"o","team":"a"}});
        assert!(admit(Some(&row), "all").is_ok());
        assert!(admit(Some(&row), "team:o/a").is_ok());
        assert!(admit(Some(&row), "team:o/b").is_err());
        assert!(admit(None, "all").is_err());
    }
}
