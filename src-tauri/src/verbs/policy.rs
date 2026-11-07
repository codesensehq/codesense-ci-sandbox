// SPDX-License-Identifier: MIT
use super::{Caller, VerbSpec};
use crate::dashboard_stream::frame::FrameCallerKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Preset {
    #[default]
    Open,
    TeamSafe,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Confirm {
    #[default]
    Never,
    Prompt,
    PromptOnce,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Switches {
    pub private_install: bool,
    pub confirm_message_destructive: bool,
    pub confirm_team_send_as_you: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Policy {
    pub preset: Preset,
    pub switches: Switches,
    pub per_dashboard: BTreeMap<String, DashboardOverride>,
    pub human_only: HumanOnly,
}

/// The person's "Humans only" switches for dashboard commands: per dashboard (by
/// key) and per command (by namespaced id).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HumanOnly {
    pub dashboards: BTreeMap<String, bool>,
    pub commands: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DashboardOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant: Option<Vec<String>>,
    #[serde(flatten)]
    pub confirmations: BTreeMap<String, Confirm>,
}
impl std::ops::Deref for DashboardOverride {
    type Target = BTreeMap<String, Confirm>;
    fn deref(&self) -> &Self::Target {
        &self.confirmations
    }
}
impl std::ops::DerefMut for DashboardOverride {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.confirmations
    }
}
pub fn send_as_you(verb: &str) -> bool {
    matches!(
        verb,
        "sendAsHuman"
            | "replyAsHuman"
            | "sendPrompt"
            | "sendToSession"
            | "sendToWorkspaceTab"
            | "runPrompt"
    )
}
pub fn destructive(spec: &VerbSpec) -> bool {
    spec.class == super::Class::Manage
        || spec.class == super::Class::Send
        || spec.name == "records.delete"
}
impl Policy {
    pub fn effective_grant(
        &self,
        dashboard: &str,
        slot: &str,
        scope: &str,
        declared: &[String],
        principal_scopes: &[String],
    ) -> Vec<String> {
        let override_grant = self
            .per_dashboard
            .get(dashboard)
            .and_then(|p| p.grant.as_ref());
        super::capabilities::ALL
            .iter()
            .filter(|cap| {
                principal_scopes.iter().any(|p| p == **cap)
                    && (self.preset == Preset::Open
                        || super::capabilities::records_allows(slot, scope, cap))
                    && match override_grant {
                        Some(grant) => grant.iter().any(|p| p == **cap),
                        None if self.preset == Preset::Open => true,
                        None => {
                            declared.iter().any(|p| p == **cap)
                                && super::capabilities::slot_allows(slot, cap)
                                && super::capabilities::scope_allows(scope, cap)
                        }
                    }
            })
            .map(|s| (*s).to_owned())
            .collect()
    }
    /// Policy: whether agents are kept from a dashboard command. Team-safe locks a
    /// private dashboard's commands to humans; a dashboard's switch covers all its
    /// commands; otherwise the command's own switch wins over the manifest's
    /// suggestion (`declared`). Open by default.
    pub fn human_only(
        &self,
        dashboard: &str,
        dashboard_scope: &str,
        command: &str,
        declared: bool,
    ) -> bool {
        (self.preset == Preset::TeamSafe && dashboard_scope == "private")
            || self.human_only.dashboards.get(dashboard) == Some(&true)
            || self
                .human_only
                .commands
                .get(command)
                .copied()
                .unwrap_or(declared)
    }
    pub fn effective_switches(&self) -> Switches {
        Switches {
            private_install: self.preset == Preset::TeamSafe || self.switches.private_install,
            confirm_message_destructive: self.preset == Preset::TeamSafe
                || self.switches.confirm_message_destructive,
            confirm_team_send_as_you: self.preset == Preset::TeamSafe
                || self.switches.confirm_team_send_as_you,
        }
    }
    pub fn denied(&self, caller: &Caller, spec: &VerbSpec) -> bool {
        self.preset == Preset::TeamSafe
            && caller.kind == FrameCallerKind::Message
            && destructive(spec)
    }
    pub fn confirm(&self, caller: &Caller, spec: &VerbSpec) -> Confirm {
        if spec.name == "loadMedia" {
            return Confirm::Prompt;
        }
        if !spec.confirmable {
            return Confirm::Never;
        }
        let switches = self.effective_switches();
        let configured = self
            .per_dashboard
            .get(&caller.dashboard)
            .and_then(|v| v.get(&spec.name))
            .copied();
        let value = configured.unwrap_or_else(|| {
            if (caller.kind == FrameCallerKind::Message
                && switches.confirm_message_destructive
                && destructive(spec))
                || (caller.kind == FrameCallerKind::Team
                    && switches.confirm_team_send_as_you
                    && send_as_you(&spec.name))
                || (self.preset == Preset::TeamSafe
                    && caller.kind == FrameCallerKind::Team
                    && (destructive(spec) || spec.name == "openWorkspace"))
            {
                Confirm::Prompt
            } else {
                Confirm::Never
            }
        });
        if value == Confirm::PromptOnce
            && (!matches!(
                caller.kind,
                FrameCallerKind::Private | FrameCallerKind::Builtin
            ) || send_as_you(&spec.name))
        {
            Confirm::Prompt
        } else {
            value
        }
    }
}
