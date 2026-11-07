// SPDX-License-Identifier: MIT
//! The managed producer's owned scope-to-binding-to-spawn handoff.
use crate::agents::launch::{AgentLaunch, LaunchSettingsFile};
use crate::commands::pty::{SpawnPtyArgs, SpawnPtySize};
use crate::commands::{agent_accounts, mcp_scope};
use codesense_host_proto::agent_cli::NativeLaunch;
use std::path::Path;
use tauri::AppHandle;

pub(crate) struct ProjectedManagedSpawn {
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
    creation: Option<mcp_scope::ScopeCreation>,
    settings: Option<LaunchSettingsFile>,
}

impl ProjectedManagedSpawn {
    pub(crate) fn project_managed_scope(
        agent: &AgentLaunch,
        session: &str,
        cwd: &str,
        cli: &mcp_scope::LaunchCli<'_>,
        arguments: &[String],
    ) -> Result<Self, String> {
        let (arguments, environment, creation, settings) =
            super::project_managed_scope(agent, session, cwd, cli, arguments)?;
        Ok(Self {
            arguments,
            environment,
            creation,
            settings,
        })
    }

    pub(crate) fn take_arguments(&mut self) -> Vec<String> {
        std::mem::take(&mut self.arguments)
    }
    pub(crate) fn take_settings(&mut self) -> Option<LaunchSettingsFile> {
        Option::take(&mut self.settings)
    }
    pub(crate) fn environment(&self) -> &[(String, String)] {
        &self.environment
    }
    pub(crate) fn environment_mut(&mut self) -> &mut Vec<(String, String)> {
        &mut self.environment
    }

    pub(crate) fn bind<R: tauri::Runtime>(
        self,
        app: &AppHandle<R>,
        agent: &AgentLaunch,
    ) -> Result<BoundManagedSpawn, String> {
        let binding = agent_accounts::mint_launch_environment(
            app,
            agent_accounts::LaunchEnvironment::for_agent(
                &agent.launch,
                self.environment,
                self.creation,
                agent.requires_spend_holds()?,
            ),
        );
        Ok(BoundManagedSpawn {
            binding,
            account: agent.account_input(),
            env_remove: agent.launch.env_suppress.clone(),
            native_launch: NativeLaunch::Cli(agent.runtime),
        })
    }
}

pub(crate) struct BoundManagedSpawn {
    binding: Option<String>,
    account: agent_accounts::ResolveAgentAccountInput,
    env_remove: Vec<String>,
    native_launch: NativeLaunch,
}

impl BoundManagedSpawn {
    pub(crate) fn into_args(
        self,
        binary: &Path,
        cwd: &str,
        workspace: &str,
        arguments: &[String],
    ) -> SpawnPtyArgs {
        SpawnPtyArgs {
            command: String::new(),
            command_argv: Some(super::compose_agent_argv(binary, arguments)),
            cwd: cwd.into(),
            size: SpawnPtySize {
                cols: super::AGENT_COLS,
                rows: super::AGENT_ROWS,
            },
            interactive: Some(false),
            starship_preset: None,
            story_id: Some(workspace.into()),
            workspace_id: Some(workspace.into()),
            epic_id: None,
            scrollback: None,
            shell_integration: None,
            env_remove: Some(self.env_remove),
            account: Some(self.account),
            environment_binding: self.binding,
            native_launch: Some(self.native_launch),
        }
    }
}
