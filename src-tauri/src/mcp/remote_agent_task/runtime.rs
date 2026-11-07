// SPDX-License-Identifier: MIT
use super::{
    claude,
    harness::{self, Environment, McpConnection, Validate},
};
use crate::agents::adapter::cli_of_provider;
use crate::install::agent_account_store::AgentProvider;
use codesense_pty_core::agent_cli::descriptor;
#[cfg(doc)]
use codesense_pty_core::agent_cli::remote_executor;
use std::{
    path::Path,
    sync::{atomic::AtomicBool, Arc},
};
pub(super) enum Process {
    Codex(harness::Harness),
    Claude(claude::Harness),
}
/// Refuses a local runtime outside its qualified release before the task leaves any
/// trace: no receipt and no remote executor. Whatever the runtime, the machine executes
/// with [`remote_executor`], whose own release its session server checks the same way
/// before starting it.
pub(super) async fn verify(provider: AgentProvider) -> Result<Qualified, String> {
    verify_on(provider, std::env::var_os("PATH").unwrap_or_default()).await?;
    Ok(Qualified(provider))
}
async fn verify_on(provider: AgentProvider, path: std::ffi::OsString) -> Result<(), String> {
    let cli = cli_of_provider(provider);
    let mut command = tokio::process::Command::new(&descriptor(cli).binary);
    command.env_clear().env("PATH", path);
    if let Some(home) = std::env::var_os("HOME") {
        command.env("HOME", home);
    }
    harness::verify_version(command, cli).await
}
/// A local runtime whose release [`verify`] admitted; a task's runtime spawns only as one.
pub(super) struct Qualified(AgentProvider);
impl Process {
    pub async fn spawn(
        Qualified(provider): Qualified,
        dir: &Path,
        home: Option<&Path>,
        mcp: &McpConnection,
        prompt: &str,
        valid: Validate,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        match provider {
            AgentProvider::Codex => harness::Harness::spawn(dir, home, valid, cancel)
                .await
                .map(Self::Codex),
            AgentProvider::Claude => claude::Harness::spawn(
                dir,
                claude::Launch {
                    home,
                    mcp,
                    prompt,
                    mode: claude::AuthMode::Native,
                },
                valid,
                cancel,
            )
            .await
            .map(Self::Claude),
        }
    }
    pub async fn run(
        &mut self,
        environment: Environment,
        mcp: &McpConnection,
        prompt: &str,
        event: impl FnMut(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        match self {
            Self::Codex(process) => process.run(environment, mcp, prompt, event).await,
            Self::Claude(process) => process.run(event).await,
        }
    }
    pub async fn stop(&mut self) -> bool {
        match self {
            Self::Codex(process) => process.stop().await,
            Self::Claude(process) => process.stop().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::verify_on;
    use crate::install::agent_account_store::AgentProvider;
    use codesense_pty_core::agent_cli::{descriptor, AgentCli};
    use std::os::unix::fs::PermissionsExt;

    /// A PATH whose `codex` and `claude` answer `--version` with these releases.
    fn path(codex: &str, claude: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, version) in [("codex", codex), ("claude", claude)] {
            let file = dir.path().join(name);
            std::fs::write(&file, format!("#!/bin/sh\necho '{version}'\n")).unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    }

    #[tokio::test]
    async fn each_provider_is_held_to_its_own_release() {
        let current = path("codex-cli 0.162.1", "2.1.296 (Claude Code)");
        for provider in [AgentProvider::Codex, AgentProvider::Claude] {
            verify_on(provider, current.path().into()).await.unwrap();
        }
        // Each refusal names its own CLI's release, so a swapped arm fails.
        let old = path("codex-cli 0.158.0", "2.1.140 (Claude Code)");
        for (provider, agent) in [
            (AgentProvider::Codex, AgentCli::Codex),
            (AgentProvider::Claude, AgentCli::Claude),
        ] {
            let refused = verify_on(provider, old.path().into()).await.unwrap_err();
            assert!(
                refused.contains(descriptor(agent).release.supported),
                "{refused}"
            );
        }
        let mixed = path("codex-cli 0.158.0", "2.1.296 (Claude Code)");
        verify_on(AgentProvider::Claude, mixed.path().into())
            .await
            .unwrap();
        assert!(verify_on(AgentProvider::Codex, mixed.path().into())
            .await
            .is_err());
    }
}
