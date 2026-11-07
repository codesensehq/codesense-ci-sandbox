// SPDX-License-Identifier: MIT
//! The client side of `InboxWaitV1`: whether a host hands its agents inbox notices (the
//! notices and waiter states themselves travel through
//! [`TerminalDomain`](codesense_pty_core::domain::TerminalDomain);
//! `crate::commands::host_inbox` is the caller).

use super::*;

impl SessionServerClient {
    /// Whether this host hands its agents inbox notices.
    pub(crate) fn supports_inbox_wait(&self) -> bool {
        self.hello.supports(&Capability::InboxWaitV1)
    }

    pub(crate) fn local_mcp_sessions(&self, sessions: Vec<PtySessionId>) {
        if !self.hello.supports(&Capability::LocalMcpReadinessV1) {
            return;
        }
        if let Err(error) = self.post(Request::LocalMcpSessions { sessions }) {
            log::warn!(target: "inbox", "cannot publish local MCP readiness: {error}");
            // Never leave a stale ready snapshot after a failed lifecycle update.
            // Reconnection publishes the full current snapshot through the same path.
            self.writer.close();
        }
    }

    /// Fails unless this host hands its agents inbox notices.
    pub(super) fn require_inbox_wait(
        &self,
    ) -> Result<(), codesense_pty_core::domain::PortUnavailable> {
        if self.supports_inbox_wait() {
            return Ok(());
        }
        Err(codesense_pty_core::domain::PortUnavailable(
            "inbox notices require a terminal server update".into(),
        ))
    }
}
