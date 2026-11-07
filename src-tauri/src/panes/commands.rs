// SPDX-License-Identifier: MIT
//! Slot dashboards' commands, as the host registry holds them for agents.
//!
//! A dashboard declares its commands in its manifest; at runtime
//! `codesense.commands.register(ids)` names which of them apply now. The last
//! call replaces the whole set, keyed by the dashboard (not the frame), so a
//! remounted frame keeps what its dashboard registered. The frontend registry
//! pushes the slot dashboards' commands with their effective chords, so
//! `command_list` and `command_run` (and their page verbs) answer from here.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{CommandScope, DashboardCommand};
use crate::verbs::Policy;

/// The Tauri event a `command_run` emits; the frontend dispatches it like a chord.
pub const COMMAND_RUN_EVENT: &str = "commands:run";

/// One slot dashboard's command, as the frontend registry pushes it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SlotCommand {
    /// Namespaced `<dashboardKey>/<id>`.
    pub id: String,
    /// The dashboard's key (`DashboardRef::key`).
    pub dashboard: String,
    pub dashboard_title: String,
    /// The dashboard's scope: `team`, `private` or `built-in`.
    pub dashboard_scope: String,
    pub slot: String,
    /// The capabilities the dashboard declares, for its effective grant.
    pub capabilities: Vec<String>,
    /// The id the dashboard declared.
    pub command: String,
    pub label: String,
    pub scope: CommandScope,
    /// The effective chord, empty when it has none.
    pub binding: String,
    /// The manifest's suggestion; the person's policy can change it.
    pub human_only: bool,
}

/// A `command_list` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRow {
    pub id: String,
    pub label: String,
    pub scope: CommandScope,
    pub binding: String,
    pub dashboard: String,
    pub active: bool,
    pub human_only: bool,
}

/// Who asks: a person (chord, palette or page verb) or an agent (MCP).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    Human,
    Agent,
}

/// The payload of [`COMMAND_RUN_EVENT`].
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunCommand {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

/// Each dashboard's registered ids; absent means every declared command is active.
type ActiveSets = HashMap<String, Vec<String>>;

#[derive(Default)]
pub struct CommandRegistry {
    active: Mutex<ActiveSets>,
    commands: Mutex<Vec<SlotCommand>>,
}

fn locked<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, String> {
    mutex.lock().map_err(|e| e.to_string())
}

fn is_active(active: &ActiveSets, command: &SlotCommand) -> bool {
    active
        .get(&command.dashboard)
        .is_none_or(|ids| ids.contains(&command.command))
}

/// Policy: an agent may run a dashboard's commands when that dashboard's effective
/// grant (the one a frame of it gets) holds `commands:run`. Open grants it.
fn agent_may_run(policy: &Policy, command: &SlotCommand, full: &[String]) -> bool {
    policy
        .effective_grant(
            &command.dashboard,
            &command.slot,
            &command.dashboard_scope,
            &command.capabilities,
            full,
        )
        .iter()
        .any(|cap| cap == "commands:run")
}

fn human_only(policy: &Policy, command: &SlotCommand) -> bool {
    policy.human_only(
        &command.dashboard,
        &command.dashboard_scope,
        &command.id,
        command.human_only,
    )
}

impl CommandRegistry {
    /// Replace `dashboard`'s active set with `ids`, in declaration order.
    /// An id the declaration doesn't name rejects the whole call.
    pub fn register(
        &self,
        dashboard: &str,
        declared: &[DashboardCommand],
        ids: &[String],
    ) -> Result<Vec<String>, String> {
        if let Some(id) = ids.iter().find(|id| !declared.iter().any(|c| &c.id == *id)) {
            return Err(format!("undeclared command {id:?}"));
        }
        let active: Vec<String> = declared
            .iter()
            .filter(|c| ids.contains(&c.id))
            .map(|c| c.id.clone())
            .collect();
        locked(&self.active)?.insert(dashboard.to_owned(), active.clone());
        Ok(active)
    }

    /// The slot dashboards' commands now. A dashboard that left its slot starts
    /// over with every command active, as it does in the frontend registry.
    pub fn set_commands(&self, commands: Vec<SlotCommand>) -> Result<(), String> {
        locked(&self.active)?
            .retain(|dashboard, _| commands.iter().any(|c| &c.dashboard == dashboard));
        *locked(&self.commands)? = commands;
        Ok(())
    }

    /// `command_list` / `commands.list`. An agent sees only the commands its policy
    /// lets it run; every row says whether it is humans-only.
    pub fn list(&self, policy: &Policy, actor: Actor) -> Result<Vec<CommandRow>, String> {
        let commands = locked(&self.commands)?;
        let active = locked(&self.active)?;
        let full = crate::verbs::capabilities::full();
        // One grant per dashboard: its commands share it.
        let mut may_run = HashMap::new();
        Ok(commands
            .iter()
            .filter(|c| {
                actor == Actor::Human
                    || *may_run
                        .entry(c.dashboard.as_str())
                        .or_insert_with(|| agent_may_run(policy, c, &full))
            })
            .map(|c| CommandRow {
                id: c.id.clone(),
                label: c.label.clone(),
                scope: c.scope,
                binding: c.binding.clone(),
                dashboard: c.dashboard_title.clone(),
                active: is_active(&active, c),
                human_only: human_only(policy, c),
            })
            .collect())
    }

    /// `command_run` / `commands.run`: check the command can run for `actor`, then
    /// hand it to `emit`, which raises the one dispatch event. A person always may;
    /// an agent needs `commands:run` in the dashboard's grant and a command that
    /// isn't humans-only.
    pub fn run(
        &self,
        policy: &Policy,
        actor: Actor,
        id: &str,
        args: Option<Value>,
        emit: impl FnOnce(&RunCommand) -> Result<(), String>,
    ) -> Result<(), String> {
        let command = locked(&self.commands)?
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .ok_or_else(|| format!("unknown command {id:?}"))?;
        if actor == Actor::Agent {
            if !agent_may_run(policy, &command, &crate::verbs::capabilities::full()) {
                return Err("denied: grant".into());
            }
            if human_only(policy, &command) {
                return Err(format!("denied: {id:?} is for humans only"));
            }
        }
        if !is_active(&*locked(&self.active)?, &command) {
            return Err(format!("unavailable: {id:?} isn't active"));
        }
        emit(&RunCommand {
            id: command.id,
            args,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verbs::{DashboardOverride, Preset};

    fn declared(id: &str) -> DashboardCommand {
        DashboardCommand {
            id: id.into(),
            label: id.into(),
            icon: None,
            default_chord: None,
            scope: CommandScope::Global,
            human_only: false,
        }
    }

    const INBOX: &str = "dashboard:private:acme/inbox";

    fn slot_command(id: &str, human_only: bool) -> SlotCommand {
        SlotCommand {
            id: format!("{INBOX}/{id}"),
            dashboard: INBOX.into(),
            dashboard_title: "Inbox".into(),
            dashboard_scope: "private".into(),
            slot: "right-pane".into(),
            capabilities: vec!["inbox:write".into()],
            command: id.into(),
            label: id.into(),
            scope: CommandScope::Global,
            binding: "CmdOrCtrl+Shift+Y".into(),
            human_only,
        }
    }

    fn registry() -> CommandRegistry {
        let registry = CommandRegistry::default();
        registry
            .set_commands(vec![
                slot_command("archive", false),
                slot_command("delete", true),
            ])
            .unwrap();
        registry
    }

    fn run(
        registry: &CommandRegistry,
        policy: &Policy,
        actor: Actor,
        id: &str,
    ) -> Result<Vec<RunCommand>, String> {
        let mut emitted = Vec::new();
        registry.run(
            policy,
            actor,
            &format!("{INBOX}/{id}"),
            Some(serde_json::json!({"n":1})),
            |e| {
                emitted.push(e.clone());
                Ok(())
            },
        )?;
        Ok(emitted)
    }

    #[test]
    fn register_replaces_the_active_set_and_rejects_undeclared_ids() {
        let registry = CommandRegistry::default();
        let declared = [declared("archive"), declared("next"), declared("reply")];
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            registry
                .register("d", &declared, &ids(&["reply", "archive"]))
                .unwrap(),
            ids(&["archive", "reply"])
        );
        assert_eq!(
            registry.register("d", &declared, &ids(&["next"])).unwrap(),
            ids(&["next"])
        );
        assert_eq!(
            registry
                .register("d", &declared, &ids(&["next", "delete"]))
                .unwrap_err(),
            "undeclared command \"delete\""
        );
        assert_eq!(registry.active.lock().unwrap()["d"], ids(&["next"]));
    }

    #[test]
    fn by_default_an_agent_runs_a_private_dashboards_command_through_one_event() {
        let emitted = run(&registry(), &Policy::default(), Actor::Agent, "archive").unwrap();
        assert_eq!(
            emitted,
            vec![RunCommand {
                id: format!("{INBOX}/archive"),
                args: Some(serde_json::json!({"n":1}))
            }]
        );
        let rows = registry().list(&Policy::default(), Actor::Agent).unwrap();
        assert_eq!(
            serde_json::to_value(&rows[0]).unwrap(),
            serde_json::json!({"id":format!("{INBOX}/archive"),"label":"archive","scope":"global","binding":"CmdOrCtrl+Shift+Y","dashboard":"Inbox","active":true,"humanOnly":false})
        );
    }

    #[test]
    fn a_dashboard_grant_without_commands_run_refuses_the_agent_and_hides_its_commands() {
        let mut policy = Policy::default();
        policy.per_dashboard.insert(
            INBOX.into(),
            DashboardOverride {
                grant: Some(vec!["inbox:write".into()]),
                ..DashboardOverride::default()
            },
        );
        assert_eq!(
            run(&registry(), &policy, Actor::Agent, "archive").unwrap_err(),
            "denied: grant"
        );
        assert!(registry().list(&policy, Actor::Agent).unwrap().is_empty());
        // A person is not an agent: the page's own grant is checked by its dispatcher.
        assert_eq!(
            run(&registry(), &policy, Actor::Human, "archive")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(registry().list(&policy, Actor::Human).unwrap().len(), 2);
    }

    #[test]
    fn humans_only_refuses_the_agent_and_lets_the_person_through() {
        let registry = registry();
        let policy = Policy::default();
        // Declared by the manifest.
        assert_eq!(
            run(&registry, &policy, Actor::Agent, "delete").unwrap_err(),
            format!("denied: \"{INBOX}/delete\" is for humans only")
        );
        assert_eq!(
            run(&registry, &policy, Actor::Human, "delete")
                .unwrap()
                .len(),
            1
        );
        let rows = registry.list(&policy, Actor::Agent).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.human_only).collect::<Vec<_>>(),
            [false, true]
        );

        // The person's switch over the manifest, per command and per dashboard.
        let mut policy = Policy::default();
        policy
            .human_only
            .commands
            .insert(format!("{INBOX}/delete"), false);
        assert_eq!(
            run(&registry, &policy, Actor::Agent, "delete")
                .unwrap()
                .len(),
            1
        );
        policy.human_only.dashboards.insert(INBOX.into(), true);
        assert!(run(&registry, &policy, Actor::Agent, "archive").is_err());
        assert!(run(&registry, &policy, Actor::Agent, "delete").is_err());

        // Team-safe locks a private dashboard's commands to humans, over any switch.
        let mut policy = Policy {
            preset: Preset::TeamSafe,
            ..Policy::default()
        };
        policy
            .human_only
            .commands
            .insert(format!("{INBOX}/delete"), false);
        assert!(registry
            .list(&policy, Actor::Human)
            .unwrap()
            .iter()
            .all(|r| r.human_only));
        assert_eq!(
            run(&registry, &policy, Actor::Human, "delete")
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn inactive_unknown_and_departed_commands() {
        let registry = registry();
        let policy = Policy::default();
        registry
            .register(
                INBOX,
                &[declared("archive"), declared("delete")],
                &["delete".into()],
            )
            .unwrap();
        assert!(run(&registry, &policy, Actor::Human, "archive")
            .unwrap_err()
            .starts_with("unavailable:"));
        assert!(run(&registry, &policy, Actor::Human, "nope")
            .unwrap_err()
            .starts_with("unknown command"));
        // Leaving the slot forgets the active set; coming back, everything is active.
        registry.set_commands(Vec::new()).unwrap();
        assert!(registry.active.lock().unwrap().is_empty());
        registry
            .set_commands(vec![slot_command("archive", false)])
            .unwrap();
        assert!(registry.list(&policy, Actor::Human).unwrap()[0].active);
    }
}
