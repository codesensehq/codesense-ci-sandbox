// SPDX-License-Identifier: MIT
//! The client side of `RoutineArmV1` (SW-AGR-8): arm and disarm a routine on
//! a machine's terminal server, list what it holds, and read the runs it
//! fired while this app was away. A server that predates the capability is
//! refused by name, and nothing is sent to it.

use codesense_pty_core::routine_arm::{
    ArmAnswer, ArmCompleteAnswer, ArmFireAnswer, ArmList, ArmRunLog, RoutineArm,
};

use super::*;

/// SW-AGR-11: the first minor whose server takes the desktop identity on
/// `RoutineArmBindSecret`.
pub(crate) const DESKTOP_IDENTITY_PROTO_MINOR: u16 = 14;

impl SessionServerClient {
    pub(crate) fn supports_routine_arm(&self) -> bool {
        self.hello.supports(&Capability::RoutineArmV1)
    }

    fn require_routine_arm(&self) -> Result<(), String> {
        if self.supports_routine_arm() {
            return Ok(());
        }
        Err(if self.hello.proto_minor >= 12 {
            format!(
                "Routines cannot be armed on {}: its terminal server's routine scheduler is not running.",
                self.hello.server_name
            )
        } else {
            format!(
                "Update the terminal server on {} to run routines there without this Mac.",
                self.hello.server_name
            )
        })
    }

    pub(crate) fn routine_arm(&self, arm: RoutineArm) -> Result<ArmAnswer, String> {
        self.require_routine_arm()?;
        self.require_agent_env(
            arm.launch
                .as_ref()
                .is_some_and(|launch| !launch.agent_startup.env.is_empty()),
        )?;
        match self
            .call(Request::RoutineArm { arm: Box::new(arm) }, REPLY_TIMEOUT)
            .map_err(String::from)?
        {
            Reply::RoutineArmAnswer { answer } => Ok(*answer),
            other => Err(format!("unexpected routine arm reply {other:?}")),
        }
    }

    pub(crate) fn routine_disarm(&self, key: &str, epoch: u64) -> Result<ArmAnswer, String> {
        self.require_routine_arm()?;
        match self
            .call(
                Request::RoutineDisarm {
                    key: key.to_string(),
                    epoch,
                },
                REPLY_TIMEOUT,
            )
            .map_err(String::from)?
        {
            Reply::RoutineArmAnswer { answer } => Ok(*answer),
            other => Err(format!("unexpected routine disarm reply {other:?}")),
        }
    }

    pub(crate) fn routine_arms(&self) -> Result<ArmList, String> {
        self.require_routine_arm()?;
        match self
            .call(Request::RoutineArms, REPLY_TIMEOUT)
            .map_err(String::from)?
        {
            Reply::RoutineArms { list } => Ok(*list),
            other => Err(format!("unexpected routine arm list reply {other:?}")),
        }
    }

    pub(crate) fn routine_arm_runs(&self, since: u64) -> Result<ArmRunLog, String> {
        self.require_routine_arm()?;
        match self
            .call(Request::RoutineArmRuns { since }, REPLY_TIMEOUT)
            .map_err(String::from)?
        {
            Reply::RoutineArmRuns { log } => Ok(*log),
            other => Err(format!("unexpected routine run log reply {other:?}")),
        }
    }

    /// Fire an armed routine now, on the machine (`routine_run_now`).
    pub(crate) fn routine_arm_fire_now(
        &self,
        key: &str,
        epoch: u64,
        task: Option<String>,
    ) -> Result<ArmFireAnswer, String> {
        self.require_routine_arm()?;
        match self
            .call(
                Request::RoutineArmFireNow {
                    key: key.to_string(),
                    epoch,
                    task,
                },
                // A fire may start the agent: as long as a launch takes.
                std::time::Duration::from_secs(150),
            )
            .map_err(String::from)?
        {
            Reply::RoutineArmFired { answer } => Ok(*answer),
            other => Err(format!("unexpected routine fire reply {other:?}")),
        }
    }

    /// Finish a run the machine fired into `session` (an agent's
    /// `routine_done` or `signal_done` this app relayed).
    pub(crate) fn routine_arm_complete(
        &self,
        session: &str,
        run_id: Option<&str>,
        outcome: &str,
        summary: &str,
    ) -> Result<ArmCompleteAnswer, String> {
        self.require_routine_arm()?;
        match self
            .call(
                Request::RoutineArmComplete {
                    session: session.to_string(),
                    run_id: run_id.map(str::to_string),
                    outcome: outcome.to_string(),
                    summary: summary.to_string(),
                },
                REPLY_TIMEOUT,
            )
            .map_err(String::from)?
        {
            Reply::RoutineArmCompleted { answer } => Ok(*answer),
            other => Err(format!("unexpected routine complete reply {other:?}")),
        }
    }

    /// Accept, on this connection, the CodeSense MCP of agents the machine
    /// starts for routines, keyed by `secret` and served by `binder` on
    /// `runtime` (the caller's: this tree stays Tauri-free).
    pub(crate) fn accept_routine_bindings(
        &self,
        secret: String,
        binder: super::RoutineBinder,
        runtime: tokio::runtime::Handle,
    ) {
        self.reverse_streams
            .accept_routine_bindings(secret, binder, runtime);
    }

    pub(crate) fn accepts_routine_bindings(&self) -> bool {
        self.reverse_streams.accepts_routine_bindings()
    }

    /// Give the machine the secret this app accepts the CodeSense MCP of the
    /// agents it starts with, and accept them (SW-AGR-8: a routine fired
    /// while this app is connected gets its full tool set).
    ///
    /// SW-AGR-11: `desktop` is this app's identity (per install and person):
    /// the agents it relayed are handed back to it when it reconnects.
    pub(crate) fn routine_arm_bind_secret(
        &self,
        secret: &str,
        desktop: Option<&str>,
    ) -> Result<(), String> {
        self.require_routine_arm()?;
        // An older server has no use for it (it hands agents back by viewer).
        let desktop = desktop.filter(|_| self.hello.proto_minor >= DESKTOP_IDENTITY_PROTO_MINOR);
        match self
            .call(
                Request::RoutineArmBindSecret {
                    secret: secret.to_string(),
                    desktop: desktop.map(str::to_string),
                },
                REPLY_TIMEOUT,
            )
            .map_err(String::from)?
        {
            Reply::Done => {
                *super::lock(&self.reverse_streams.bound_desktop) = desktop.map(str::to_string);
                Ok(())
            }
            other => Err(format!("unexpected routine binding reply {other:?}")),
        }
    }
}

impl crate::routines::machine_arm::ArmHost for SessionServerClient {
    fn arms(&self) -> Result<ArmList, String> {
        self.routine_arms()
    }
    fn arm(&self, arm: RoutineArm) -> Result<ArmAnswer, String> {
        self.routine_arm(arm)
    }
    fn disarm(&self, key: &str, epoch: u64) -> Result<ArmAnswer, String> {
        self.routine_disarm(key, epoch)
    }
    fn runs(&self, since: u64) -> Result<ArmRunLog, String> {
        self.routine_arm_runs(since)
    }
    fn enforces_wake_limits(&self) -> bool {
        SessionServerClient::enforces_wake_limits(self)
    }
    fn desktop_identity(&self) -> Option<String> {
        super::lock(&self.reverse_streams.bound_desktop).clone()
    }
}
