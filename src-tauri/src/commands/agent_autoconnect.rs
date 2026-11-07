// SPDX-License-Identifier: MIT
//! What a supported agent CLI started by hand in a local CodeSense tab needs to reach
//! CodeSense: the tab exports it, and the shipped shell-init functions add it to the
//! command line (see `resources/shell-integration`).
//!
//! Nothing in it depends on the tab: the bridge finds the app on its socket. So one copy
//! of the file serves every tab, and nothing has to be removed when a tab closes.

use std::path::{Path, PathBuf};

/// The MCP configuration a hand-started Claude adds with `--mcp-config`. Its status and
/// inbox hooks come with the tab's terminal host, which observes the CLI typed there.
pub(crate) const MCP_CONFIG_ENV: &str = "CODESENSE_MCP_CONFIG";
/// The `-c` override that gives a hand-started Codex the CodeSense server.
pub(crate) const CODEX_MCP_ENV: &str = "CODESENSE_CODEX_MCP";
/// The per-user OpenCode fragment a hand-started CLI merges from this tab.

const MCP_CONFIG_FILE: &str = "claude-mcp.json";

/// The variables a local interactive tab exports, after writing the files they name
/// under `~/.codesense/shell/agents`. Empty when the bridge is not installed or the
/// files cannot be written: the tab then opens as before.
pub(crate) fn tab_env() -> Vec<(String, String)> {
    let (Some(home), Some(bridge)) = (
        super::paths::codesense_home_dir(),
        super::deps::bridge_symlink_path(),
    ) else {
        return Vec::new();
    };
    if !bridge.exists() {
        return Vec::new();
    }
    match write_agent_files(&home.join("shell").join("agents"), &bridge) {
        Ok(env) => env,
        Err(error) => {
            log::warn!(target: "pty", "agent auto-connect files unavailable: {error}");
            Vec::new()
        }
    }
}

/// Write the MCP configuration for `bridge` into `dir` (only when it changed) and return
/// the variables naming it and the Codex override.
pub(crate) fn write_agent_files(
    dir: &Path,
    bridge: &Path,
) -> Result<Vec<(String, String)>, String> {
    let bridge = bridge.to_str().ok_or("bridge path is not UTF-8")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let config = serde_json::json!({
        "mcpServers": {
            (codesense_host_proto::reverse_mcp::CODESENSE_SERVER): { "command": bridge }
        }
    });
    let config_path = write_json(dir, MCP_CONFIG_FILE, &config)?;
    let shell_config = codesense_pty_core::agent_cli::AgentCli::ALL
        .into_iter()
        .find_map(|cli| (crate::agents::adapter::desktop(cli).shell_mcp_config)(bridge));
    let codex = format!(
        "mcp_servers.{}={{command={},env_vars=[{}]}}",
        codesense_host_proto::reverse_mcp::CODESENSE_SERVER,
        serde_json::to_string(bridge).map_err(|e| e.to_string())?,
        serde_json::to_string(super::mcp_scope::TAB_SESSION_ENV).map_err(|e| e.to_string())?
    );
    let mut env = vec![
        (MCP_CONFIG_ENV.into(), path_string(config_path)?),
        (CODEX_MCP_ENV.into(), codex),
    ];
    if let Some((env_var, file_name, content)) = shell_config {
        let path = write_json(dir, file_name, &content)?;
        env.push((env_var.into(), path_string(path)?));
    }
    Ok(env)
}

fn write_json(dir: &Path, name: &str, value: &serde_json::Value) -> Result<PathBuf, String> {
    let path = dir.join(name);
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    if std::fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
        // Write beside and rename, so a tab starting an agent never reads half a file.
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let partial = dir.join(format!(".{name}.{}.{unique}", std::process::id()));
        std::fs::write(&partial, &bytes)
            .and_then(|()| std::fs::rename(&partial, &path))
            .map_err(|e| {
                let _ = std::fs::remove_file(&partial);
                format!("{}: {e}", path.display())
            })?;
    }
    Ok(path)
}

fn path_string(path: PathBuf) -> Result<String, String> {
    path.into_os_string()
        .into_string()
        .map_err(|_| "agent file path is not UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tab_gets_the_bridge_and_the_codex_override() {
        let home = crate::routines::test_support::TempHome::new();
        let dir = tempfile::tempdir().unwrap();
        let agents = dir.path().join("shell/agents");
        let bridge = dir.path().join("bin/codesense bridge");
        let env: std::collections::HashMap<_, _> = write_agent_files(&agents, &bridge)
            .unwrap()
            .into_iter()
            .collect();

        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&env[MCP_CONFIG_ENV]).unwrap()).unwrap();
        assert_eq!(
            config,
            serde_json::json!({"mcpServers": {"codesense": {"command": bridge.to_str().unwrap()}}})
        );

        // No settings: a typed Claude's status and inbox hooks are its host's.
        assert_eq!(env.len(), 3);
        let shell_env = env
            .keys()
            .find(|name| *name != MCP_CONFIG_ENV && *name != CODEX_MCP_ENV)
            .unwrap();
        let opencode: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&env[shell_env]).unwrap()).unwrap();
        assert_eq!(opencode["mcp"]["codesense"]["type"], "local");
        assert_eq!(
            opencode["mcp"]["codesense"]["command"][0],
            bridge.to_str().unwrap()
        );
        assert!(opencode["mcp"]["codesense"].get("environment").is_none());
        // A typed OpenCode gets CodeSense's activity plugin, a directory plugin with its
        // server.js entrypoint, through the same config.
        let plugin_dir = home.root().join(".codesense/opencode/codesense");
        assert_eq!(
            opencode["plugins"],
            serde_json::json!([String::from(
                url::Url::from_directory_path(&plugin_dir).unwrap()
            )])
        );
        assert!(plugin_dir.join("server.js").is_file());
        assert!(opencode.get("plugin").is_none());

        // One TOML value, the bridge path quoted (it may hold spaces).
        let codex = &env[CODEX_MCP_ENV];
        let (key, value) = codex.split_once('=').unwrap();
        assert_eq!(key, "mcp_servers.codesense");
        let parsed: toml::Table = format!("v={value}").parse().unwrap();
        assert_eq!(
            parsed["v"]["command"].as_str(),
            Some(bridge.to_str().unwrap())
        );
    }

    #[test]
    fn files_are_rewritten_only_when_they_change() {
        let dir = tempfile::tempdir().unwrap();
        let bridge = dir.path().join("bridge");
        let env = write_agent_files(dir.path(), &bridge).unwrap();
        let config = PathBuf::from(&env[0].1);
        let first = std::fs::metadata(&config).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_agent_files(dir.path(), &bridge).unwrap();
        assert_eq!(
            std::fs::metadata(&config).unwrap().modified().unwrap(),
            first
        );
        // A stale copy (another bridge path) is replaced.
        write_agent_files(dir.path(), &dir.path().join("other")).unwrap();
        assert!(std::fs::read_to_string(&config).unwrap().contains("other"));
        // Nothing but the one file is left behind.
        let mut names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n != "bridge" && n != "other")
            .collect();
        names.sort();
        assert_eq!(names, [MCP_CONFIG_FILE, "opencode-mcp.json"]);
    }
}
