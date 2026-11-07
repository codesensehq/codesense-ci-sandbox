// SPDX-License-Identifier: MIT
pub mod capabilities;
pub mod commands;
mod dispatcher;
mod policy;
mod registry;
mod schema;
pub use dispatcher::*;
pub use policy::*;
pub use registry::*;
#[cfg(test)]
mod grant_tests;
#[cfg(test)]
mod tests;

pub fn contract() -> serde_json::Value {
    let registry = builtin_registry();
    serde_json::json!({
     "confirmable": Registry::all(&registry).filter(|s| s.confirmable).map(|s| s.name.clone()).collect::<Vec<_>>(),
     "capabilities": capabilities::ALL,
     "grants": Registry::all(&registry).map(|spec| (&spec.name, &spec.grant)).collect::<std::collections::BTreeMap<_, _>>(),
     "effectiveGrant": "The Rust frame record holds policyGrant intersected with principalScopes. Open grants the full vocabulary by default; team-safe intersects declarations with slot and scope ceilings. Per-dashboard grant overrides replace the policy grant. Missing capabilities reject with denied: grant before confirmation or budgets. Inbox projection calls resolve inbox:read:team or inbox:read:all-teams from the stored frame view. Records calls resolve own or team reach from the collection in the stored frame binding; records subscriptions require its read capability.",
     "presets": ["open", "team-safe"], "default": "open",
     "switches": {"privateInstall": {"default":false, "description":"Copy team dashboards with private reach into the private space. Enabled by team-safe."}, "confirmMessageDestructive": {"default":false, "description":"Confirm destructive and send verbs from HTML messages; team-safe denies them before any dialog."}, "confirmTeamSendAsYou": {"default":false, "description":"Confirm sends as you from team dashboards. Never offers Always."}},
     "document": {"preset":"open", "switches":{"privateInstall":false,"confirmMessageDestructive":false,"confirmTeamSendAsYou":false},"perDashboard":{}},
     "precedence":"Per-dashboard confirmation overrides win over preset and switches. Team-safe message grants are checked before any confirmation policy.",
     "pending": {"admission":{"status":"pending","confirmId":"host-held id"},"confirmed":{"status":"confirmed","result":"handler result"},"cancelled":{"status":"cancelled"}},
     "host":"Confirmations survive frame unmount, suspend every frame, focus Cancel and ignore Confirm for 500 ms. Private and built-in prompt-once callers may choose Always. Team and message callers only allow once. Cancellation consumes the budget.",
     "mcpApps":{"protocolVersion":"2026-01-26","methods":["ui/initialize","tools/call"],"extensions":["codesense/subscribe","codesense/slotContext"],"result":"CallToolResult: content, structuredContent, isError; a pending call resolves after the host answer."}
    })
}
