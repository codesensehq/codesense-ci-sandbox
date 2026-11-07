// SPDX-License-Identifier: MIT
//! Official local Claude Code, restricted to one task-owned remote MCP endpoint.
use super::harness::{check, McpConnection, Validate};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdout},
};

pub(super) struct Harness {
    child: Child,
    output: BufReader<ChildStdout>,
    valid: Validate,
    cancel: Arc<AtomicBool>,
    _config: ConfigFile,
    tool_prefixes: Vec<String>,
}
struct ConfigFile(PathBuf);
impl Drop for ConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
pub(super) enum AuthMode {
    Native,
    #[cfg(test)]
    Synthetic {
        base_url: String,
    },
}
pub(super) struct Launch<'a> {
    pub home: Option<&'a Path>,
    pub mcp: &'a McpConnection,
    pub prompt: &'a str,
    pub mode: AuthMode,
}
fn command(home: Option<&Path>, mode: &AuthMode) -> tokio::process::Command {
    command_with_env(home, mode, |key| std::env::var_os(key))
}
/// Builds the launch from `env`, the app's own environment outside tests.
fn command_with_env(
    home: Option<&Path>,
    mode: &AuthMode,
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("claude");
    command.env_clear();
    // `USER` names the login keychain item Claude Code keeps its subscription login in; without
    // it a logged-in Claude reads as logged out.
    for key in [
        "HOME",
        "USER",
        "PATH",
        "TMPDIR",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "CLAUDE_CONFIG_DIR",
    ] {
        if let Some(value) = env(key) {
            command.env(key, value);
        }
    }
    if let Some(home) = home {
        command.env("CLAUDE_CONFIG_DIR", home);
    }
    #[cfg(test)]
    if let AuthMode::Synthetic { base_url } = mode {
        command
            .env("ANTHROPIC_API_KEY", "owned-fake-fixture-key")
            .env("ANTHROPIC_BASE_URL", base_url);
    }
    #[cfg(not(test))]
    let _ = mode;
    command
        .env("DISABLE_AUTOUPDATER", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}
impl Harness {
    pub async fn spawn(
        cwd: &Path,
        launch: Launch<'_>,
        valid: Validate,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let Launch {
            home,
            mcp,
            prompt,
            mode,
        } = launch;
        check(&valid).await?;
        // Mandatory enterprise hooks cannot be disabled by these user-level flags.
        for path in crate::agents::adapter::CLAUDE_MANAGED_SETTINGS_PATHS.into_iter().chain([
            "/Library/Application Support/ClaudeCode/managed-mcp.json",
            "/etc/claude-code/managed-mcp.json",
        ]) {
            if Path::new(path).exists() {
                return Err("capabilityUnavailable: managed Claude policy requires a supported isolated runtime".into());
            }
        }
        if matches!(mode, AuthMode::Native) {
            let status = tokio::time::timeout(
                Duration::from_secs(10),
                command(home, &mode)
                    .args(["auth", "status"])
                    .current_dir(cwd)
                    .output(),
            )
            .await
            .map_err(|_| "Claude authentication check timed out")?
            .map_err(|_| "Claude authentication unavailable")?;
            let auth: Value = serde_json::from_slice(&status.stdout)
                .map_err(|_| "Claude authentication status unavailable")?;
            if !status.status.success()
                || auth["loggedIn"] != true
                || auth["authMethod"] != "claude.ai"
                || auth["apiProvider"] != "firstParty"
            {
                return Err(
                "capabilityUnavailable: selected Claude account requires native subscription login"
                    .into(),
            );
            }
        }
        let config = cwd.join(format!("claude-mcp-{}.json", uuid::Uuid::new_v4()));
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let mut servers = mcp.projection.servers.clone();
        servers.insert("codesense_remote_task".into(),json!({"type":"http","url":mcp.url,"headers":{"Authorization":format!("Bearer {}",mcp.token)}}));
        let tool_prefixes = servers.keys().map(|id| format!("mcp__{id}__")).collect();
        let bytes = serde_json::to_vec(&json!({"mcpServers":servers}))
            .map_err(|_| "task MCP configuration unavailable")?;
        let mut allowed = vec!["mcp__codesense_remote_task__*".to_string()];
        allowed.extend(mcp.projection.allowed.iter().cloned());
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&config)
            .map_err(|_| "task MCP configuration unavailable")?;
        let config_guard = ConfigFile(config.clone());
        file.write_all(&bytes)
            .map_err(|_| "task MCP configuration unavailable")?;
        drop(file);
        check(&valid).await?;
        let mut cmd = command(home, &mode);
        cmd.args([
            "-p",
            "--verbose",
            "--output-format",
            "stream-json",
            "--tools",
            "",
            "--strict-mcp-config",
            "--mcp-config",
        ])
        .arg(&config)
        .args([
            "--setting-sources",
            "",
            "--settings",
            "{\"disableAllHooks\":true}",
            "--disable-slash-commands",
            "--no-chrome",
            "--no-session-persistence",
            "--permission-mode",
            "dontAsk",
        ])
        .arg("--allowedTools")
        .args(&allowed);
        if !mcp.projection.denied.is_empty() {
            cmd.arg("--disallowedTools").args(&mcp.projection.denied);
        }
        let spawned = cmd
            .current_dir(cwd)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(_) => {
                let _ = std::fs::remove_file(&config);
                return Err("official Claude unavailable".into());
            }
        };
        let output = BufReader::new(child.stdout.take().ok_or("Claude output unavailable")?);
        let mut result = Self {
            child,
            output,
            valid,
            cancel,
            _config: config_guard,
            tool_prefixes,
        };
        check(&result.valid).await?;
        let mut input = result
            .child
            .stdin
            .take()
            .ok_or("Claude input unavailable")?;
        tokio::time::timeout(Duration::from_secs(5), input.write_all(prompt.as_bytes()))
            .await
            .map_err(|_| "Claude submission outcome unknown")?
            .map_err(|_| "Claude submission outcome unknown")?;
        drop(input);
        Ok(result)
    }
    pub async fn run(
        &mut self,
        mut event: impl FnMut(&str) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut bytes = Vec::new();
        let mut last = String::new();
        loop {
            if self.cancel.load(Ordering::Acquire) {
                return Err("task cancellation requested".into());
            }
            check(&self.valid).await?;
            let available = match tokio::time::timeout(
                Duration::from_millis(250),
                self.output.fill_buf(),
            )
            .await
            {
                Err(_) => continue,
                Ok(Err(_)) => return Err("Claude output unavailable".into()),
                Ok(Ok([])) => return Err("Claude ended before task settlement".into()),
                Ok(Ok(bytes)) => bytes,
            };
            let count = available
                .iter()
                .position(|b| *b == b'\n')
                .map_or(available.len(), |n| n + 1);
            if bytes.len() + count > 1024 * 1024 {
                return Err("Claude output exceeds bound".into());
            }
            let complete = available[count - 1] == b'\n';
            bytes.extend_from_slice(&available[..count]);
            self.output.consume(count);
            if !complete {
                continue;
            }
            let value: Value =
                serde_json::from_slice(&bytes).map_err(|_| "Claude output invalid")?;
            bytes.clear();
            match value["type"].as_str() {
                Some("system") if value["subtype"] == "init" => {
                    let tools = value["tools"]
                        .as_array()
                        .ok_or("Claude tool exposure unavailable")?;
                    if tools.iter().any(|tool| {
                        tool.as_str().is_none_or(|name| {
                            !self
                                .tool_prefixes
                                .iter()
                                .any(|prefix| name.starts_with(prefix))
                        })
                    }) {
                        return Err("Claude exposed tools outside the remote task".into());
                    }
                }
                Some("assistant") => {
                    if let Some(content) =
                        value.pointer("/message/content").and_then(Value::as_array)
                    {
                        let text = content
                            .iter()
                            .filter(|v| v["type"] == "text")
                            .filter_map(|v| v["text"].as_str())
                            .collect::<String>();
                        if !text.is_empty() {
                            check(&self.valid).await?;
                            event(&text)?;
                            last = text;
                        }
                    }
                }
                Some("result") => {
                    if value["subtype"] != "success" || value["is_error"] == true {
                        return Err("Claude task did not complete".into());
                    }
                    if let Some(text) = value["result"].as_str() {
                        if text != last {
                            check(&self.valid).await?;
                            event(text)?;
                        }
                    }
                    return Ok(());
                }
                _ => {}
            }
        }
    }
    pub async fn stop(&mut self) -> bool {
        if tokio::time::timeout(Duration::from_millis(300), self.child.wait())
            .await
            .is_ok_and(|v| v.is_ok_and(|status| status.success()))
        {
            return true;
        }
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGTERM);
            }
        }
        if tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .is_ok_and(|v| v.is_ok_and(|status| status.success()))
        {
            return true;
        }
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        false
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::{command_with_env, AuthMode};

    #[test]
    fn claude_runs_with_the_user_its_login_is_kept_under() {
        // The test supplies the environment, so it holds where `USER` is unset (a bare CI
        // container, `env -i`). The lookup answers every name it is asked.
        let command = command_with_env(None, &AuthMode::Native, |key| {
            Some(format!("fixture-{key}").into())
        });
        let env: Vec<_> = command.as_std().get_envs().collect();
        let user = std::ffi::OsStr::new("fixture-USER");
        assert!(env.contains(&(std::ffi::OsStr::new("USER"), Some(user))));
        // Exactly these are set, so anything else of the app's own environment (a
        // `CODESENSE_*` name included) is not passed on.
        let mut keys: Vec<_> = env.iter().map(|(key, _)| key.to_string_lossy()).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC",
                "CLAUDE_CONFIG_DIR",
                "DISABLE_AUTOUPDATER",
                "HOME",
                "PATH",
                "TMPDIR",
                "USER",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
            ]
        );
    }
}
