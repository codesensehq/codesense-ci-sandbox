// SPDX-License-Identifier: MIT
//! Shared pure precedence state extracted from the desktop DotLedger.
const LATCH_RELEASING_HOOKS: &[&str] = &["UserPromptSubmit", "SessionEnd"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookAdmission {
    Admit,
    Suppress,
}

/// Per-session bookkeeping. A session has an entry only while one of its fields
/// carries something; each is released on its own trigger, so the ledger does
/// not grow with sessions:
/// - an open ask terminates (answer, dismiss, timeout, session death, abandoned
///   call); a new turn or `SessionEnd` cancels its restore;
/// - a latch is released by a new turn (`UserPromptSubmit`), the agent
///   session ending (`SessionEnd`), the next agent signal (`signal_done`,
///   `signal_progress`, `ask_user`), or PTY exit.
#[derive(Default)]
pub struct DotState {
    pub open_asks: u32,
    /// A newer dot write landed while an ask was open; restoring on resolution
    /// would clobber it.
    pub superseded: bool,
    /// The agent declared a failed finish. Hook writes for
    /// the rest of that turn (its trailing `PostToolUse`, its `Stop`) would
    /// paint over it, so they are suppressed. Released by `UserPromptSubmit`,
    /// `SessionEnd`, the next agent signal, or PTY exit. Self-reported
    /// progress blocks do not latch: the next hook can clear them.
    pub latched: bool,
}

impl DotState {
    pub fn is_empty(&self) -> bool {
        self.open_asks == 0 && !self.latched
    }

    pub fn admit_hook(&mut self, event: &str) -> HookAdmission {
        if LATCH_RELEASING_HOOKS.contains(&event) {
            self.latched = false;
            self.superseded = true;
            return HookAdmission::Admit;
        }
        // A latch keeps every non-releasing hook off the dot.
        if self.latched {
            return HookAdmission::Suppress;
        }
        if event != "PreToolUse" {
            self.superseded = true;
        }
        HookAdmission::Admit
    }

    pub fn declare(&mut self, latch: bool) {
        self.superseded = true;
        self.latched = latch;
    }

    pub fn open_ask(&mut self) {
        self.open_asks += 1;
        self.superseded = false;
        self.latched = false;
    }

    pub fn close_ask(&mut self) -> bool {
        self.open_asks = self.open_asks.saturating_sub(1);
        if self.open_asks > 0 {
            return false;
        }
        let restore = !self.superseded;
        self.superseded = false;
        restore
    }
}
