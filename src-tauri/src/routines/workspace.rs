// SPDX-License-Identifier: MIT
//! Disposable agent working files, separate from the routine's content target.

use std::path::{Path, PathBuf};

use crate::agents::{launch::AgentLaunch, prepare, AgentKey};
use crate::domain::Principal;

use super::model::RoutineKey;

pub(crate) fn team_root(home: &Path, principal: &Principal, key: &RoutineKey) -> PathBuf {
    crate::context_repo::path_to_worker_workspace(home, principal, &key.org, &key.team, "routines")
}

pub(crate) fn agent_dir(routine_dir: &Path, agent_key: &AgentKey) -> PathBuf {
    let agent_key = agent_key.to_string();
    routine_dir.join(blake3::hash(agent_key.as_bytes()).to_hex().to_string())
}

/// Clear only this routine and saved agent's scratch files. Keep the directory itself so a
/// completed run's visible tab still has a valid cwd. Neither clearing nor
/// preparation follows symlinks an agent left inside the directory.
pub(crate) fn prepare(
    home: &Path,
    principal: &Principal,
    key: &RoutineKey,
    target: &Path,
    agent: &AgentLaunch,
) -> Result<PathBuf, String> {
    let dir = if key.is_personal() {
        target.to_path_buf()
    } else {
        team_root(home, principal, key).join(&key.id)
    };
    // Reassignment must not clear a still-running predecessor's working files.
    let dir = agent_dir(&dir, &agent.manifest.agent.key);
    let root = dir.parent().ok_or("routine workspace has no parent")?;
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let canonical_home = home.canonicalize().map_err(|e| e.to_string())?;
    let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
    let relative = root.strip_prefix(home).map_err(|e| e.to_string())?;
    if canonical_root != canonical_home.join(relative) {
        return Err("routine workspace parent changed location; nothing was cleared".into());
    }
    match std::fs::symlink_metadata(&dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink()
                || !meta.is_dir()
                || dir.canonicalize().map_err(|e| e.to_string())?.parent()
                    != Some(canonical_root.as_path())
            {
                return Err("routine workspace changed location; nothing was cleared".into());
            }
            for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                    std::fs::remove_dir_all(entry.path())
                } else {
                    std::fs::remove_file(entry.path())
                }
                .map_err(|e| e.to_string())?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut want = prepare::WantSet::for_agent(
        home,
        principal,
        &agent.manifest.agent.key,
        &agent.startup_context.target,
        true,
        Some(&agent.manifest),
    )?;
    want.add_workspace_files(home, principal, &agent.generated)?;
    let report = prepare::prepare(prepare::PrepareRequest {
        home,
        principal,
        destination: prepare::Destination::Local { root: &dir },
        want: &want,
        origin: "step2:routine".into(),
        workspace: dir.display().to_string(),
        agent: agent.name.clone(),
        legacy: &|| Ok(Vec::new()),
        prune: false,
        progress: &|_| {},
        validate: &|| Ok(()),
        explicit: Default::default(),
    })?;
    if let Some(error) = report.failure() {
        return Err(error);
    }
    crate::commands::managed_worktree_config::write(&dir, home);
    Ok(dir)
}
