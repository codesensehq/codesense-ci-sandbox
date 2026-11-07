// SPDX-License-Identifier: MIT
//! Argv command preparation and capture; admission and endings remain shared with agents.
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicU32},
    Arc,
};
use std::time::{Duration, Instant};

use super::model::{RoutineDef, RoutineKey};
use crate::domain::Principal;
use serde::{Deserialize, Serialize};

pub const OUTPUT_BYTES: usize = 4096;

pub fn validate_program(program: &str) -> Result<(), String> {
    if program.trim().is_empty() {
        return Err("Name a program, like python3, or a script in the team's files.".into());
    }
    if program.chars().any(char::is_whitespace) {
        return Err("A program name has no spaces. Put each argument in its own field.".into());
    }
    let path = Path::new(program);
    if path.is_absolute() {
        return Err(format!("{program} is outside the team's files. Use a program name like python3, or a path inside the team's files."));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!(
            "{program} leads outside the team's files. Use a path inside them."
        ));
    }
    Ok(())
}

pub fn program_path(root: &Path, program: &str) -> Result<PathBuf, String> {
    validate_program(program)?;
    if !program.contains('/') {
        return Ok(PathBuf::from(program));
    }
    let path = crate::commands::pane_bridge::join_inside(root, program)?;
    let roots = [crate::context_query::index::IndexRoot {
        dir: root.to_path_buf(),
        org: None,
        team: None,
        layer: crate::context_query::model::Layer::Team,
    }];
    if crate::commands::pane_bridge::root_of(&roots, &path).is_none() {
        return Err(format!(
            "{program} leads outside the team's files. Use a path inside them."
        ));
    }
    if !path.is_file() {
        return Err(format!("{program} isn't in the team's files."));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(&path)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode()
            & 0o111
            == 0
        {
            return Err(format!("{program} can't run by itself: files lose their run permission when the team syncs. Run it with sh, python3 or node."));
        }
    }
    path.canonicalize().map_err(|e| e.to_string())
}

pub fn validate_definition(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    def: &RoutineDef,
) -> Result<(), String> {
    if !def.is_command() {
        return Ok(());
    }
    def.validate_agents_for(key).map_err(|e| e.to_string())?;
    let root = super::service::key_content_dir(home, principal, key).map_err(|e| e.to_string())?;
    program_path(
        &root,
        def.command.first().map(String::as_str).unwrap_or_default(),
    )
    .map(|_| ())
}

pub fn render(argv: &[String], values: &BTreeMap<String, String>) -> Result<Vec<String>, String> {
    argv.iter()
        .map(|arg| {
            let rendered = super::template::render(arg, values);
            if let Some(name) = rendered.unresolved.first() {
                Err(format!("unresolved:{name}"))
            } else {
                Ok(rendered.text)
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct CommandRequest {
    pub home: PathBuf,
    pub routine_key: String,
    pub routine_id: String,
    pub run_id: String,
    pub program: PathBuf,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub max_runtime: Duration,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Evidence {
    #[serde(default)]
    pub argv: Vec<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub stderr_tail: Option<String>,
    pub cwd: Option<String>,
}

pub(super) fn redacted_argv(argv: &[String]) -> Vec<String> {
    argv.iter()
        .map(|arg| crate::shared::secret_scan::redact_secrets(arg))
        .collect()
}

pub fn tail(bytes: &[u8]) -> String {
    let redacted = crate::shared::secret_scan::redact_secrets(&String::from_utf8_lossy(bytes));
    let mut start = redacted.len().saturating_sub(OUTPUT_BYTES);
    while !redacted.is_char_boundary(start) {
        start += 1;
    }
    redacted[start..].to_string()
}

pub async fn capture(request: CommandRequest, pid: Arc<AtomicU32>, cancel: Arc<AtomicBool>) {
    let home = request.home.clone();
    let run_id = request.run_id.clone();
    let result = crate::commands::await_blocking("routine_command", move || {
        use super::finish::{self, Ending};
        use crate::shared::proc_capture::{spawn_capture, CaptureRequest};
        let store =
            super::state::RoutineStateStore::open(&request.home).map_err(|e| e.to_string())?;
        // A stop between preparation and registration must never start the program.
        if !store
            .run(&request.run_id)
            .map_err(|e| e.to_string())?
            .is_some_and(|r| r.phase == super::runs::phase::RUNNING)
        {
            return Ok(false);
        }
        let started = Instant::now();
        let out = spawn_capture(CaptureRequest {
            program: request.program,
            args: request.argv[1..].to_vec(),
            cwd: Some(request.cwd.clone()),
            env: vec![
                ("CODESENSE_ROUTINE_ID".into(), request.routine_id),
                ("CODESENSE_RUN_ID".into(), request.run_id.clone()),
            ],
            env_remove: Vec::new(),
            stdin: None,
            timeout: request.max_runtime,
            process_group: true,
            pid_sink: Some(pid),
            cancel: Some(cancel),
            output_sink: None,
            last_drain_ms: None,
            max_output_bytes: Some(64 * 1024),
        })
        .map_err(|e| e.to_string())?;
        let evidence = Evidence {
            argv: redacted_argv(&request.argv),
            exit_code: out.exit_code(),
            duration_ms: Some(started.elapsed().as_millis() as u64),
            stderr_tail: Some(tail(&out.stderr)),
            cwd: Some(request.cwd.to_string_lossy().into_owned()),
        };
        let stdout = tail(&out.stdout);
        let _ = std::fs::create_dir_all(request.home.join("run-artifacts"));
        crate::commands::sprint_run::run_artifact::write_artifact_to(
            &request.home.join("run-artifacts"),
            &request.run_id,
            stdout.as_bytes(),
            evidence
                .stderr_tail
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        store
            .transaction(|tx| {
                tx.execute(
                    "UPDATE routine_runs SET command_details = ?2, result_text = ?3 WHERE run_id = ?1",
                    rusqlite::params![request.run_id, serde_json::to_string(&evidence).unwrap(), stdout],
                )?;
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        let ending = Ending::Command {
            phase: if out.cancelled {
                "cancelled"
            } else if out.timed_out {
                "timed_out"
            } else if out.exit_code() == Some(0) {
                "succeeded"
            } else {
                "failed"
            },
            code: out.exit_code(),
            output: stdout,
        };
        if let Some(finished) = finish::finish(&store, &request.run_id, ending, finish::now_ms())
            .map_err(|e| e.to_string())?
        {
            finished.announce();
            Ok(true)
        } else {
            super::events::changed(&request.routine_key, &request.run_id);
            Ok(false)
        }
    })
    .await;
    let announced = match result {
        Ok(announced) => announced,
        Err(detail) => {
            if let Ok(store) = super::state::RoutineStateStore::open(&home) {
                if let Ok(Some(finished)) = super::finish::finish(
                    &store,
                    &run_id,
                    super::finish::Ending::Failed {
                        reason: super::runs::reason::LAUNCH_FAILED,
                        detail,
                    },
                    super::finish::now_ms(),
                ) {
                    finished.announce();
                    return;
                }
            }
            false
        }
    };
    if !announced {
        if let Err(e) = super::chain::offer(&crate::coordinator::default_store_path(&home), &run_id)
        {
            log::warn!(target: "routines", "completion trigger failed for {run_id}: {e}");
        }
    }
}

pub fn register(
    request: &CommandRequest,
    registry: &crate::commands::dashboard_launch::DashboardLaunchState,
) -> Result<(String, Arc<AtomicU32>, Arc<AtomicBool>), String> {
    let handle = super::dispatch::handle(&request.routine_key, &request.run_id);
    if !registry.begin(&handle).map_err(|e| e.to_string())? {
        return Err("Command run is already registered".into());
    }
    let session = format!("headless-command-{}", request.run_id);
    registry.bind_with_account(&handle, &session, Some("routine"), None)?;
    registry.stamp_provenance(
        &handle,
        crate::commands::dashboard_launch::RunProvenance {
            run_id: request.run_id.clone(),
            routine_key: Some(request.routine_key.clone()),
            ..Default::default()
        },
    );
    let pid = Arc::new(AtomicU32::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    registry.mark_headless(&handle, pid.clone(), cancel.clone(), Default::default());
    Ok((session, pid, cancel))
}

#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub argv: Vec<String>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

pub fn preview(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    def: &RoutineDef,
    now: i64,
) -> Preview {
    let pointer = super::admit::RoutinePointer {
        run_id: "preview".into(), routine_key: key.to_string(), revision: def.revision,
        principal: principal.to_string(), source: "manual".into(), scheduled_at: None, fired_at: now,
        variables: Default::default(), ended_sessions: Vec::new(),
        trigger_payload: def.after.as_ref().map(|after| {
            let upstream = RoutineKey { id: after.routine.clone(), ..key.clone() };
            let name = super::service::get_current(home, principal, &upstream).map(|r| r.def.name).unwrap_or_else(|_| after.routine.clone());
            serde_json::json!({"run_id":"‹upstream run›", "routine":after.routine, "phase":"succeeded", "result_text":format!("‹result of {name}›"), "detail":""})
        }),
    };
    let values = super::dispatch::template_values(def, &pointer, None);
    let argv: Vec<_> = def
        .command
        .iter()
        .map(|arg| super::template::render(arg, &values).text)
        .collect();
    let mut warnings = Vec::new();
    for arg in &def.command {
        for token in super::template::tokens(arg) {
            let name = token.text();
            if !values.contains_key(&name) {
                warnings.push(if name.starts_with("trigger.") && def.after.is_none() {
                    format!("{{{{{name}}}}} is filled in only when this routine runs after another. Add one under Triggers, or remove it.")
                } else { format!("{{{{{name}}}}} isn't a known fill-in. If a page sends it, keep it. Otherwise the run fails before it starts.") });
            }
        }
    }
    let program = argv.first().map(String::as_str).unwrap_or_default();
    let error = validate_definition(home, principal, key, def).err();
    if error.is_none()
        && !program.contains('/')
        && !std::env::var_os("PATH").is_some_and(|paths| {
            std::env::split_paths(&paths).any(|dir| dir.join(program).is_file())
        })
    {
        warnings.push(format!(
            "{program} isn't on This Mac's PATH. Another Mac may have it."
        ));
    }
    Preview {
        argv,
        error,
        warnings,
    }
}
