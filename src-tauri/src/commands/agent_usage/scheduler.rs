// SPDX-License-Identifier: MIT
use std::time::Duration;

use codesense_pty_core::agent_cli::Usage;

pub const COALESCE_WINDOW: Duration = Duration::from_millis(750);
pub const PUBLISH_FLOOR: Duration = Duration::from_secs(1);
pub const PERSIST_FLOOR: Duration = Duration::from_secs(60);
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy, Default)]
pub struct RetryBackoff {
    failures: u8,
}

impl RetryBackoff {
    pub fn fail(&mut self) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let exponent = u32::from(self.failures.saturating_sub(1).min(9));
        BACKOFF_MIN
            .saturating_mul(2_u32.saturating_pow(exponent))
            .min(BACKOFF_MAX)
    }
}

/// How often an account whose CLI reports usage through `usage` is asked for it: never
/// for one that reports on its status line, or reports none.
pub fn refresh_interval(usage: Usage, visible: bool) -> Option<Duration> {
    match usage {
        Usage::Poll(poll) => Some(if visible { poll.visible } else { poll.hidden }),
        Usage::StatusLine { .. } | Usage::None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_line_cli_is_never_polled_and_a_polled_one_slows_down_unseen() {
        use codesense_pty_core::agent_cli::{adapter, AgentCli};
        let claude = adapter(AgentCli::Claude).output().usage;
        assert_eq!(refresh_interval(claude, true), None);
        assert_eq!(refresh_interval(claude, false), None);
        let codex = adapter(AgentCli::Codex).output().usage;
        assert_eq!(refresh_interval(codex, true), Some(Duration::from_secs(60)));
        assert_eq!(
            refresh_interval(codex, false),
            Some(Duration::from_secs(300))
        );
    }

    #[test]
    fn retry_backoff_is_exponential_and_bounded() {
        let mut backoff = RetryBackoff::default();
        assert_eq!(backoff.fail(), Duration::from_secs(1));
        assert_eq!(backoff.fail(), Duration::from_secs(2));
        for _ in 0..20 {
            backoff.fail();
        }
        assert_eq!(backoff.fail(), BACKOFF_MAX);
        backoff = RetryBackoff::default();
        assert_eq!(backoff.fail(), BACKOFF_MIN);
    }
}
