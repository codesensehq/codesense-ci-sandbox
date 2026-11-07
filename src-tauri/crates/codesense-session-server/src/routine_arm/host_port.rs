// SPDX-License-Identifier: MIT
//! The arm engine's port onto this host: its layout (the agent's tabs), its
//! activity owner, its wake queue and its spawn path.

use std::path::{Component, Path, PathBuf};
use std::sync::Weak;

use codesense_pty_core::domain::PtySessionId;
use codesense_pty_core::routine_arm::{ArmLaunch, ArmTarget};
use codesense_pty_core::session_wake::{DeliverRequest, Originator, WakeOutcome};

use super::{ArmCandidate, ArmPort, LaunchFailure};
use crate::host::Host;

pub(crate) struct HostArmPort {
    host: Weak<Host>,
    /// The instance root: the empty MCP projections of the agents this host
    /// starts on its own live under it.
    root: PathBuf,
}

impl HostArmPort {
    pub(crate) fn new(host: Weak<Host>, root: PathBuf) -> Self {
        Self { host, root }
    }
}

/// Whether every path a desktop's preparation wrote is still in `cwd`.
pub(crate) fn prepared_in(cwd: &Path, launch: &ArmLaunch) -> Result<(), String> {
    if !cwd.is_dir() {
        return Err(format!(
            "the workspace {} is no longer on this machine",
            cwd.display()
        ));
    }
    for relative in &launch.prepared_paths {
        let path = Path::new(relative);
        if path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(format!("prepared path {relative:?} leaves the workspace"));
        }
        if std::fs::symlink_metadata(cwd.join(path)).is_err() {
            return Err(format!(
                "{relative} is missing from the workspace: the agent must be prepared again"
            ));
        }
    }
    Ok(())
}

impl ArmPort for HostArmPort {
    fn candidates(&self, target: &ArmTarget) -> Vec<ArmCandidate> {
        self.host
            .upgrade()
            .map(|host| host.arm_candidates(target))
            .unwrap_or_default()
    }

    fn session_live(&self, session: &PtySessionId) -> bool {
        self.host
            .upgrade()
            .is_some_and(|host| host.wake_session_live(session))
    }

    fn prepared(&self, target: &ArmTarget, launch: &ArmLaunch) -> Result<(), String> {
        prepared_in(Path::new(&target.cwd), launch)
    }

    fn launch(
        &self,
        target: &ArmTarget,
        launch: &ArmLaunch,
        session: &PtySessionId,
        prompt: &str,
        if_absent: bool,
        desktop: &str,
    ) -> Result<(), LaunchFailure> {
        let host = self
            .host
            .upgrade()
            .ok_or_else(|| LaunchFailure::NotStarted("the terminal server is stopping".into()))?;
        #[cfg(unix)]
        {
            host.arm_launch(
                target,
                launch,
                session,
                prompt,
                &self.root.join("routine-mcp"),
                if_absent,
                desktop,
            )
        }
        #[cfg(not(unix))]
        {
            let _ = (host, target, launch, session, prompt, if_absent, desktop);
            Err(LaunchFailure::NotStarted(
                "this machine cannot start agents on its own".into(),
            ))
        }
    }

    fn kill(&self, session: &PtySessionId) {
        if let Some(host) = self.host.upgrade() {
            if let Err(error) = host.kill_with_supervisors(session) {
                log::warn!("routine session {session} not stopped: {error}");
            }
        }
    }

    fn deliver(&self, request: DeliverRequest) -> Result<WakeOutcome, String> {
        self.host
            .upgrade()
            .and_then(|host| host.wake())
            .ok_or_else(|| "session wakes are not running on this terminal server".to_string())?
            .deliver(request)
    }

    fn delivery(
        &self,
        originator: &Originator,
        session: &PtySessionId,
        key: &str,
    ) -> Option<WakeOutcome> {
        self.host
            .upgrade()?
            .wake()?
            .outcome_of(originator, session, key)
    }

    fn cancel(&self, originator: &Originator, session: &PtySessionId, key: &str) {
        if let Some(wake) = self.host.upgrade().and_then(|host| host.wake()) {
            wake.cancel_delivery(originator, session, key);
        }
    }
}
