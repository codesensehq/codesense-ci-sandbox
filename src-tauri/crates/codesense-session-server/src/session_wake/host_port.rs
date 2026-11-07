// SPDX-License-Identifier: MIT
//! The wake queue's port onto this host: the terminal registry, its activity
//! owner, the writable attachments and the event relay.

use std::sync::Weak;

use codesense_pty_core::activity::Snapshot;
use codesense_pty_core::agent_cli::AgentCli;
use codesense_pty_core::domain::PtySessionId;
use codesense_pty_core::session_wake::{Originator, WakeOutcome};

use super::{Claim, InputStamp, WakePort};
use crate::host::Host;

pub(crate) struct HostWakePort {
    host: Weak<Host>,
}

impl HostWakePort {
    pub(crate) fn new(host: Weak<Host>) -> Self {
        Self { host }
    }
}

impl WakePort for HostWakePort {
    fn session_live(&self, session: &PtySessionId) -> bool {
        self.host
            .upgrade()
            .is_some_and(|host| host.wake_session_live(session))
    }

    fn activity(&self, session: &PtySessionId) -> Option<Snapshot> {
        self.host.upgrade()?.wake_activity(session)
    }

    fn agents_in_foreground(&self, queries: &[(PtySessionId, AgentCli)]) -> Vec<Option<bool>> {
        let Some(host) = self.host.upgrade() else {
            return vec![None; queries.len()];
        };
        let named: Vec<(PtySessionId, String)> = queries
            .iter()
            .map(|(session, provider)| {
                (
                    session.clone(),
                    codesense_pty_core::agent_cli::descriptor(*provider)
                        .binary
                        .clone(),
                )
            })
            .collect();
        host.wake_agents_in_foreground(&named)
    }

    fn shell_idle(&self, session: &PtySessionId) -> Option<bool> {
        self.host.upgrade()?.wake_shell_idle(session)
    }

    fn last_input(&self, session: &PtySessionId) -> Option<InputStamp> {
        self.host.upgrade()?.last_input(session)
    }

    fn bracketed_paste(&self, session: &PtySessionId) -> Option<bool> {
        self.host.upgrade()?.wake_bracketed_paste(session)
    }

    fn output_seq(&self, session: &PtySessionId) -> Option<u64> {
        self.host.upgrade()?.wake_output_seq(session)
    }

    fn claim_available(&self, session: &PtySessionId, originator: &Originator) -> bool {
        self.host.upgrade().is_some_and(|host| {
            host.wake_budget_allowed(session)
                && host.wake_claim_available(session, &originator.desktop)
        })
    }

    fn claim(&self, session: &PtySessionId, originator: &Originator) -> Claim {
        if !self
            .host
            .upgrade()
            .is_some_and(|host| host.wake_budget_allowed(session))
        {
            return Claim::Elsewhere;
        }
        match self
            .host
            .upgrade()
            .and_then(|host| host.wake_claim(session, &originator.desktop))
        {
            Some(held) => Claim::Held(held),
            None => Claim::Elsewhere,
        }
    }

    fn write(&self, session: &PtySessionId, bytes: &[u8]) -> Result<(), String> {
        self.host
            .upgrade()
            .ok_or_else(|| "the terminal server is stopping".to_string())?
            .wake_write(session, bytes)
    }

    fn emit(&self, outcome: &WakeOutcome) {
        if let Some(host) = self.host.upgrade() {
            host.emit_wake(outcome);
        }
    }
}
