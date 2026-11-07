// SPDX-License-Identifier: MIT
//! Home-preserving projection for a CLI that lists its own MCP servers (its
//! descriptor's [`McpInventory`]): the selected servers reach it through the captured
//! relay as one config override, and every server it already has is switched off. The
//! inventory is only a disable-list; canonical server configurations are enforced by
//! the captured relay.
use super::*;
use crate::shared::config_format::ConfigFormat;
use codesense_pty_core::agent_cli::{McpInventory, McpListing, McpTransportKind};
use std::collections::{BTreeMap, BTreeSet};

#[allow(clippy::too_many_arguments)]
pub(super) fn project(
    session: &str,
    cwd: &Path,
    allow: &[String],
    bridge_tools: Option<&[String]>,
    canonical_base: Option<&Value>,
    scope: &McpScope,
    inventory: &McpInventory,
    home: Option<&Path>,
    launch: &LaunchCli<'_>,
    quote: &impl Fn(&str) -> String,
) -> Result<(ScopeOutcome, Option<ScopeCreation>), String> {
    let ScopeInjection::EnvDir {
        env_var,
        default_home,
        ..
    } = &scope.injection
    else {
        return Err("this runtime keeps no home of its own".into());
    };
    let original_home = home
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os(env_var).map(PathBuf::from))
        .unwrap_or_else(|| expand_home(default_home));
    let canonical_path = cwd.join(&scope.config_filename);
    let bytes = bounded_config(&canonical_path)?;
    let bridge = launch
        .bridge
        .map(Path::to_path_buf)
        .or_else(super::super::deps::bridge_symlink_path)
        .filter(|path| path.is_file())
        .ok_or("CodeSense MCP bridge is unavailable; repair the CodeSense CLI installation")?;
    let mut canonical =
        canonical_with_bridge(bytes.as_deref(), canonical_base, scope, &bridge, session)?;
    if let Some(tools) = bridge_tools {
        canonical[&scope.server_map_key][&scope.bridge_id]["enabled_tools"] =
            serde_json::json!(tools);
    }
    let effective = effective_mcp_servers(&canonical, allow, scope);
    let grant = crate::mcp::local_mcp_relay::config::parse_local_scope(
        &effective,
        &scope.server_map_key,
        cwd,
    )
    .map_err(|error| format!("scoped MCP configuration is unsupported by this CLI: {error}"))?;
    let inherited = registered(inventory, env_var, launch, cwd, &original_home)?;
    // Check native input stability as well as canonical input. Native profile and
    // -c selectors are carried into BOTH inventories. This is a launch-time guard,
    // not an OS sandbox against concurrent user configuration writers.
    let aliases = aliases(grant.keys().cloned(), &inherited)?;
    let (path, creation) = write_scope_config_bytes(session, scope, b"", write_0600_exclusive)?;
    let prepared = (|| {
        enable_relay(&creation)?;
        let saved = SavedGrant {
            grant,
            aliases: aliases.clone(),
        };
        write_0600_exclusive(
            &path.with_file_name("local-relay-grant.json"),
            &serde_json::to_vec(&saved).map_err(|_| "cannot encode captured MCP grant")?,
        )?;
        let bound = creation.clone();
        let validate = std::sync::Arc::new(move || validate_creation(&bound));
        publish_relay(
            path.parent().ok_or("scope directory missing")?,
            saved,
            validate,
        )?;
        let descriptor_path = path.with_file_name("local-relay.json");
        let entries = override_entries(
            inventory,
            &inherited,
            &aliases,
            &scope.bridge_id,
            bridge_tools,
            &bridge,
            &descriptor_path,
        )?;
        if bounded_config(&canonical_path)? != bytes
            || registered(inventory, env_var, launch, cwd, &original_home)? != inherited
        {
            return Err("the CLI's configuration changed during launch; retry launch".into());
        }
        Ok(ScopeOutcome {
            execution: None,
            extra_args: inventory.override_argv(&scope.server_map_key, &entries, quote),
            extra_env: vec![(
                env_var.clone(),
                original_home.to_string_lossy().into_owned(),
            )],
            environment_binding: None,
            remove_args: None,
            server_aliases: aliases,
        })
    })();
    match prepared {
        Ok(outcome) => Ok((outcome, Some(creation))),
        Err(error) => {
            remove_scope_config(session);
            Err(error)
        }
    }
}

fn canonical_with_bridge(
    bytes: Option<&[u8]>,
    canonical_base: Option<&Value>,
    scope: &McpScope,
    bridge: &Path,
    session: &str,
) -> Result<Value, String> {
    // SW-AN-52: start from the content-layer base (when the launch spine
    // resolved one) and deep-merge the cwd config on top. Cwd is nearer than
    // any content layer, so a worktree-local server override wins.
    let mut canonical = canonical_base
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(bytes) = bytes {
        let cwd: Value = ConfigFormat::from(scope.format)
            .parse(bytes)
            .map_err(|_| "cannot parse canonical Codex MCP scope")?;
        crate::shared::json_io::deep_merge(&mut canonical, &cwd);
    }
    with_bridge_server(&mut canonical, scope, Some(bridge), None);
    // The relay starts the bridge from CodeSense, not from the tab, so the tab session
    // the bridge reads (`$CODESENSE_TAB_SESSION_ID`) is set on its entry explicitly.
    if let Some(entry) = canonical
        .get_mut(&scope.server_map_key)
        .and_then(|servers| servers.get_mut(&scope.bridge_id))
        .and_then(Value::as_object_mut)
    {
        entry.insert(
            "env".into(),
            serde_json::json!({ super::TAB_SESSION_ENV: session }),
        );
    }
    Ok(canonical)
}

fn bounded_config(path: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err("canonical Codex configuration must be an accessible regular file".into())
        }
    };
    if !file
        .metadata()
        .map_err(|_| "canonical Codex metadata unavailable")?
        .is_file()
    {
        return Err("canonical Codex configuration must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read canonical Codex configuration")?;
    if bytes.len() > 1024 * 1024 {
        return Err("canonical Codex configuration exceeds limit".into());
    }
    Ok(Some(bytes))
}

fn aliases(
    ids: impl Iterator<Item = String>,
    inherited: &McpListing,
) -> Result<BTreeMap<String, String>, String> {
    let mut result = BTreeMap::new();
    for id in ids {
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(
                "selected MCP server identity cannot be represented safely in Codex".into(),
            );
        }
        // Stable per canonical name, never provider credentials. Avoid native
        // table merging with any user-defined registration, even selected IDs.
        let alias = format!(
            "codesense_scope_{}",
            &blake3::hash(id.as_bytes()).to_hex()[..16]
        );
        if inherited.contains_key(&alias) || result.values().any(|prior| prior == &alias) {
            return Err("reserved CodeSense MCP alias already exists; remove that registration before launching".into());
        }
        result.insert(id, alias);
    }
    Ok(result)
}

/// The override's server entries: every registered server switched off, and each alias
/// relayed to its selected server. Only the CodeSense bridge (`bridge_id`) is required:
/// the CLI refuses to start without a required server, so any other server that is slow
/// or unreachable connects in the background, or never, instead of stopping the launch.
fn override_entries(
    inventory: &McpInventory,
    inherited: &McpListing,
    aliases: &BTreeMap<String, String>,
    bridge_id: &str,
    bridge_tools: Option<&[String]>,
    bridge: &Path,
    descriptor: &Path,
) -> Result<Vec<String>, String> {
    let json = |value: &str| {
        serde_json::to_string(value)
            .map_err(|_| "cannot encode the scoped MCP configuration".to_string())
    };
    let mut entries = inventory.disabled(inherited)?;
    for (id, alias) in aliases {
        let policy = if id == bridge_id {
            let tools = bridge_tools
                .map(|tools| {
                    serde_json::to_string(tools).map(|tools| format!(",enabled_tools={tools}"))
                })
                .transpose()
                .map_err(|_| "cannot encode bridge tool policy")?
                .unwrap_or_default();
            format!(",default_tools_approval_mode=\"approve\"{tools}")
        } else {
            String::new()
        };
        entries.push(format!("{}={{command={},args=[\"--local-scope\",{},\"--server-id\",{}],enabled=true,required={}{}}}",
            json(alias)?, json(bridge.to_str().ok_or("bridge path is not UTF-8")?)?,
            json(descriptor.to_str().ok_or("descriptor path is not UTF-8")?)?, json(id)?,
            id == bridge_id, policy));
    }
    Ok(entries)
}

/// The servers the CLI `launch` runs already has registered, under its own home `home`
/// (exported as `env_var`).
fn registered(
    inventory: &McpInventory,
    env_var: &str,
    launch: &LaunchCli<'_>,
    cwd: &Path,
    home: &Path,
) -> Result<McpListing, String> {
    // Only configuration selectors affect inventory. Prompts/subcommands and
    // unsafe permission flags must not turn this read into an agent invocation.
    let selectors = inventory.selector_args(launch.args)?;
    let binary = launch.binary.to_path_buf();
    let cwd = cwd.to_path_buf();
    let home = home.to_path_buf();
    let (argv, env_var) = (inventory.argv.clone(), env_var.to_owned());
    let bytes = tauri::async_runtime::block_on(async move {
        use tokio::io::AsyncReadExt;
        let mut command = tokio::process::Command::new(binary);
        command
            .args(selectors)
            .args(argv)
            .current_dir(cwd)
            .env(env_var, home)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|_| "cannot inspect selected Codex MCP inventory")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("Codex inventory output unavailable")?;
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let mut bytes = Vec::new();
            stdout
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| "Codex inventory read failed")?;
            if bytes.len() > 1024 * 1024 {
                return Err("Codex inventory exceeds limit");
            }
            let status = child
                .wait()
                .await
                .map_err(|_| "Codex inventory did not exit")?;
            if !status.success() {
                return Err("selected Codex does not support scoped MCP inventory; update Codex");
            }
            Ok(bytes)
        })
        .await;
        match result {
            Ok(Ok(bytes)) => Ok(bytes),
            other => {
                let _ = child.kill().await;
                Err(match other {
                    Ok(Err(error)) => error,
                    _ => "Codex inventory timed out",
                })
            }
        }
    })?;
    McpInventory::names(&bytes).map_err(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projection_disables_all_inherited_and_exposes_only_secret_free_aliases() {
        let inherited = BTreeMap::from([
            ("codesense".into(), McpTransportKind::Stdio),
            ("custom.with.dot".into(), McpTransportKind::Http),
            ("unused".into(), McpTransportKind::Stdio),
        ]);
        let aliases = aliases(
            ["codesense".into(), "custom.with.dot".into()].into_iter(),
            &inherited,
        )
        .unwrap();
        let scope = codesense_pty_core::agent_cli::descriptor(
            codesense_pty_core::agent_cli::AgentCli::Codex,
        )
        .mcp_scope
        .as_ref()
        .unwrap();
        let inventory = scope.inventory.as_ref().unwrap();
        let entries = override_entries(
            inventory,
            &inherited,
            &aliases,
            &scope.bridge_id,
            None,
            Path::new("/fixture/bridge"),
            Path::new("/fixture/descriptor"),
        )
        .unwrap();
        let text = inventory
            .override_argv(&scope.server_map_key, &entries, str::to_owned)
            .pop()
            .unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        let servers = parsed["mcp_servers"].as_table().unwrap();
        for id in inherited.keys() {
            assert_eq!(servers[id]["enabled"].as_bool(), Some(false));
        }
        for (id, alias) in &aliases {
            let row = servers[alias].as_table().unwrap();
            assert_eq!(row["command"].as_str(), Some("/fixture/bridge"));
            assert!(!row.contains_key("env"));
            assert!(!row.contains_key("http_headers"));
            // Only the bridge may stop a launch; any other server starts in the background.
            assert_eq!(
                row["required"].as_bool(),
                Some(id == &scope.bridge_id),
                "{id} required"
            );
        }
        assert!(super::aliases(
            ["codesense".into()].into_iter(),
            &BTreeMap::from([(aliases["codesense"].clone(), McpTransportKind::Http)])
        )
        .is_err());
    }
    /// The seam Codex's merged-config validation sits behind: a registered project server
    /// reaches the launch override switched off with a placeholder command, never its own args or env.
    #[test]
    fn a_registered_server_is_switched_off_with_a_placeholder_in_the_launch_override() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        std::fs::write(
            &fixture.binary,
            "#!/bin/sh\ncat <<'EOF'\n[{\"name\":\"projsrv\",\"transport\":{\"type\":\"stdio\",\"command\":\"/bin/echo\",\"args\":[\"--api-key\",\"secret\"],\"env\":{\"K\":\"secret\"}}}]\nEOF\n",
        )
        .unwrap();
        std::fs::set_permissions(&fixture.binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let scope = codesense_pty_core::agent_cli::descriptor(
            codesense_pty_core::agent_cli::AgentCli::Codex,
        )
        .mcp_scope
        .as_ref()
        .unwrap();
        let inventory = scope.inventory.as_ref().unwrap();
        let home = tempfile::tempdir().unwrap();
        let inherited = registered(
            inventory,
            "CODEX_HOME",
            &fixture.launch(&[]),
            home.path(),
            home.path(),
        )
        .unwrap();
        let entries = override_entries(
            inventory,
            &inherited,
            &BTreeMap::new(),
            &scope.bridge_id,
            None,
            Path::new("/fixture/bridge"),
            Path::new("/fixture/descriptor"),
        )
        .unwrap();
        let text = inventory
            .override_argv(&scope.server_map_key, &entries, str::to_owned)
            .pop()
            .unwrap();
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        let row = parsed["mcp_servers"]["projsrv"].as_table().unwrap();
        assert_eq!(row["enabled"].as_bool(), Some(false));
        assert_eq!(row["command"].as_str(), Some("/usr/bin/false"));
        assert!(
            !row.contains_key("args") && !row.contains_key("env") && !text.contains("secret"),
            "{text}"
        );
    }
    #[test]
    fn unsupported_native_fields_cannot_be_dropped_from_canonical_scope() {
        let config = serde_json::json!({"mcp_servers":{"codesense":{"command":"bridge","oauth_scopes":["secret"]}}});
        assert!(crate::mcp::local_mcp_relay::config::parse_local_scope(
            &config,
            "mcp_servers",
            Path::new("/tmp")
        )
        .is_err());
    }
}

/// Only test callers may replace installed executables. The production constructor
/// always uses the exact selected CLI and the installed CodeSense bridge.
#[cfg(test)]
pub(crate) struct Fixture {
    _root: tempfile::TempDir,
    binary: PathBuf,
}
#[cfg(test)]
impl Fixture {
    pub(crate) fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("codex");
        std::fs::write(&binary, "#!/bin/sh\ncase \"$*\" in *--version*) echo 'codex-cli 0.162.1';; *) printf '[]\\n';; esac\n").unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            _root: root,
            binary,
        }
    }
    pub(crate) fn launch<'a>(&'a self, args: &'a [String]) -> LaunchCli<'a> {
        LaunchCli {
            runtime: None,
            binary: &self.binary,
            args,
            bridge: Some(&self.binary),
            version: None,
            attribution_hook_trust: true,
        }
    }
    pub(crate) fn launch_with_bridge<'a>(
        &'a self,
        args: &'a [String],
        bridge: &'a Path,
    ) -> LaunchCli<'a> {
        LaunchCli {
            bridge: Some(bridge),
            ..self.launch(args)
        }
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use codesense_pty_core::agent_cli::{descriptor, AgentCli};
    #[test]
    fn original_home_and_user_hooks_are_preserved_across_repeated_launches() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("original-home");
        let cwd = root.path().join("worktree");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(cwd.join(".codex")).unwrap();
        for (name, bytes) in [
            ("auth.json", "provider-auth"),
            ("hooks.json", "user-hooks"),
            ("config.toml", "user-trust"),
        ] {
            std::fs::write(home.join(name), bytes).unwrap();
        }
        std::fs::write(
            cwd.join(".codex/config.toml"),
            "[mcp_servers.codesense]\ncommand='/canonical/bridge'\n",
        )
        .unwrap();
        let scope = descriptor(AgentCli::Codex).mcp_scope.as_ref().unwrap();
        let fixture = Fixture::new();
        for _ in 0..2 {
            let session = format!("scope-fixture-{}", uuid::Uuid::new_v4());
            let (outcome, creation) = project(
                &session,
                &cwd,
                &[],
                None,
                None,
                scope,
                scope.inventory.as_ref().unwrap(),
                Some(&home),
                &fixture.launch(&[]),
                &str::to_string,
            )
            .unwrap();
            assert_eq!(
                outcome.extra_env,
                [("CODEX_HOME".into(), home.to_string_lossy().into_owned())]
            );
            assert!(outcome.extra_args.join(" ").contains("--local-scope"));
            assert!(!outcome.extra_args.join(" ").contains("provider-auth"));
            let descriptor = mcp_scope_session_dir(&session).join("local-relay.json");
            assert!(codesense_host_proto::scoped_mcp::read_descriptor(&descriptor).is_ok());
            assert!(!mcp_scope_session_dir(&session).join("auth.json").exists());
            validate_creation(&creation.unwrap()).unwrap();
            remove_scope_config(&session);
            assert!(!descriptor.exists());
        }
        std::fs::remove_file(cwd.join(".codex/config.toml")).unwrap();
        let session = format!("scope-empty-{}", uuid::Uuid::new_v4());
        let (outcome, _) = project(
            &session,
            &cwd,
            &[],
            None,
            None,
            scope,
            scope.inventory.as_ref().unwrap(),
            Some(&home),
            &fixture.launch(&[]),
            &str::to_string,
        )
        .unwrap();
        assert!(outcome.server_aliases.contains_key("codesense"));
        assert!(
            !cwd.join(".codex/config.toml").exists(),
            "launch must not create workspace config"
        );
        remove_scope_config(&session);
        for (name, bytes) in [
            ("auth.json", "provider-auth"),
            ("hooks.json", "user-hooks"),
            ("config.toml", "user-trust"),
        ] {
            assert_eq!(std::fs::read_to_string(home.join(name)).unwrap(), bytes);
        }
    }
    #[test]
    fn absent_or_stale_canonical_bridge_uses_current_private_copy() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("missing.toml");
        assert_eq!(bounded_config(&path).unwrap(), None);
        let scope = descriptor(AgentCli::Codex).mcp_scope.as_ref().unwrap();
        let bridge = Path::new("/current/bridge");
        let empty = canonical_with_bridge(None, None, scope, bridge, "tab-7").unwrap();
        assert_eq!(
            empty["mcp_servers"]["codesense"]["command"],
            "/current/bridge"
        );
        // The relay starts the bridge outside the tab: it carries the tab session.
        assert_eq!(
            empty["mcp_servers"]["codesense"]["env"],
            serde_json::json!({ "CODESENSE_TAB_SESSION_ID": "tab-7" })
        );
        let original = b"[mcp_servers.codesense]\ncommand='/stale/bridge'\n[mcp_servers.other]\ncommand='/other/server'\n";
        std::fs::write(&path, original).unwrap();
        let bytes = bounded_config(&path).unwrap();
        let copy = canonical_with_bridge(bytes.as_deref(), None, scope, bridge, "tab-7").unwrap();
        assert_eq!(
            copy["mcp_servers"]["codesense"]["command"],
            "/current/bridge"
        );
        assert_eq!(copy["mcp_servers"]["other"]["command"], "/other/server");
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    /// SW-AN-52 R1 reviewer nit #3: an end-to-end `inventory::project` test with
    /// `Some(canonical_base)` strengthens the regression floor against a
    /// future refactor that un-threads `canonical_base` through the relay
    /// descriptor + aliases scaffolding (not just `canonical_with_bridge`).
    /// Asserts the base-defined server round-trips to `ScopeOutcome.server_aliases`.
    #[test]
    fn project_end_to_end_threads_canonical_base_through_relay_and_aliases() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("original-home");
        let cwd = root.path().join("worktree");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(cwd.join(".codex")).unwrap();
        for (name, bytes) in [
            ("auth.json", "provider-auth"),
            ("hooks.json", "user-hooks"),
            ("config.toml", "user-trust"),
        ] {
            std::fs::write(home.join(name), bytes).unwrap();
        }
        // No cwd config: the base is the sole non-bridge source of servers.
        let scope = descriptor(AgentCli::Codex).mcp_scope.as_ref().unwrap();
        let fixture = Fixture::new();
        let base = serde_json::json!({
            "mcp_servers": {
                "slack": { "command": "slack-mcp" }
            }
        });
        let session = format!("scope-base-{}", uuid::Uuid::new_v4());
        let (outcome, creation) = project(
            &session,
            &cwd,
            &["slack".into()],
            None,
            Some(&base),
            scope,
            scope.inventory.as_ref().unwrap(),
            Some(&home),
            &fixture.launch(&[]),
            &str::to_string,
        )
        .unwrap();
        assert!(
            outcome.server_aliases.contains_key("slack"),
            "a server from the content-layer base reaches the relay descriptor + aliases, not just canonical_with_bridge: {:?}",
            outcome.server_aliases,
        );
        assert!(
            outcome.server_aliases.contains_key("codesense"),
            "bridge still projected",
        );
        // The launch waits only for the bridge: a team server that is slow or never
        // answers is optional, so it cannot stop the agent from starting.
        let text = outcome.extra_args.last().unwrap();
        let parsed: toml::Value = toml::from_str(text).unwrap();
        let servers = parsed["mcp_servers"].as_table().unwrap();
        let required = |id: &str| servers[&outcome.server_aliases[id]]["required"].as_bool();
        assert_eq!(required("codesense"), Some(true));
        assert_eq!(required("slack"), Some(false));
        validate_creation(&creation.unwrap()).unwrap();
        remove_scope_config(&session);
    }

    /// Without the bridge there is no CodeSense in the agent: the launch is refused
    /// with a reason rather than starting an agent that cannot reach CodeSense.
    #[test]
    fn a_missing_bridge_refuses_the_launch() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("worktree");
        std::fs::create_dir_all(&cwd).unwrap();
        let scope = descriptor(AgentCli::Codex).mcp_scope.as_ref().unwrap();
        let fixture = Fixture::new();
        let missing = root.path().join("no-bridge");
        let launch = LaunchCli {
            bridge: Some(&missing),
            ..fixture.launch(&[])
        };
        let error = project(
            &format!("scope-no-bridge-{}", uuid::Uuid::new_v4()),
            &cwd,
            &["slack".into()],
            None,
            None,
            scope,
            scope.inventory.as_ref().unwrap(),
            Some(root.path()),
            &launch,
            &str::to_string,
        )
        .unwrap_err();
        assert!(
            error.contains("CodeSense MCP bridge is unavailable"),
            "{error}"
        );
    }

    /// SW-AN-52 AC-3 (Codex route): a server defined only in the
    /// content-layer base reaches the agent, matching the Claude-route fix.
    /// Pass 2 reviewer Blocker 1 regression: without this fixture a future
    /// edit could silently un-thread `canonical_base` through Codex.
    #[test]
    fn content_layer_base_without_cwd_still_offers_the_server_to_codex_launch() {
        let scope = descriptor(AgentCli::Codex).mcp_scope.as_ref().unwrap();
        let bridge = Path::new("/current/bridge");
        let base = serde_json::json!({
            "mcp_servers": {
                "slack": { "command": "slack-mcp" }
            }
        });
        let canonical = canonical_with_bridge(None, Some(&base), scope, bridge, "tab-9").unwrap();
        assert_eq!(
            canonical["mcp_servers"]["slack"]["command"], "slack-mcp",
            "the content-layer base's server survives into the Codex canonical",
        );
        assert_eq!(
            canonical["mcp_servers"]["codesense"]["command"], "/current/bridge",
            "bridge is still force-included on top of the base",
        );
    }

    /// SW-AN-52 fixture #5 (Codex): cwd `.mcp.json` deep-merges on top of the
    /// content-layer base. A worktree-local override wins over the team-shared
    /// config.
    #[test]
    fn cwd_canonical_overrides_the_content_layer_base_on_codex_route() {
        let scope = descriptor(AgentCli::Codex).mcp_scope.as_ref().unwrap();
        let bridge = Path::new("/current/bridge");
        let base = serde_json::json!({
            "mcp_servers": {
                "slack": { "command": "slack-prod" }
            }
        });
        let cwd_bytes = b"[mcp_servers.slack]\ncommand='slack-staging'\n";
        let merged =
            canonical_with_bridge(Some(cwd_bytes), Some(&base), scope, bridge, "tab-9").unwrap();
        assert_eq!(
            merged["mcp_servers"]["slack"]["command"], "slack-staging",
            "cwd nearer than the content layer — cwd overrides base",
        );
    }

    /// SW-AN-52 AC-4 (Codex): cwd-only (no base) still works — pre-SW-AN-52
    /// behavior preserved for workspaces outside any content layer.
    #[test]
    fn cwd_only_codex_canonical_without_a_base_matches_pre_sw_an_52() {
        let scope = descriptor(AgentCli::Codex).mcp_scope.as_ref().unwrap();
        let bridge = Path::new("/current/bridge");
        let cwd_bytes = b"[mcp_servers.slack]\ncommand='cwd-slack'\n";
        let canonical =
            canonical_with_bridge(Some(cwd_bytes), None, scope, bridge, "tab-9").unwrap();
        assert_eq!(
            canonical["mcp_servers"]["slack"]["command"], "cwd-slack",
            "cwd-only Codex launches still carry cwd servers",
        );
    }

    #[test]
    fn canonical_fifo_is_refused_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("fifo");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(bounded_config(&path).unwrap_err().contains("regular file"));
    }
}

#[cfg(test)]
#[path = "codex_native_test.rs"]
mod native_test;

#[derive(serde::Serialize, serde::Deserialize)]
struct SavedGrant {
    grant: crate::mcp::local_mcp_relay::config::Grant,
    aliases: BTreeMap<String, String>,
}

/// Re-arm only grants captured for sessions the terminal hosts still report alive.
pub(crate) fn restore_live_relays(app: &tauri::AppHandle) {
    let sessions = restorable_relay_sessions();
    let names: Vec<&str> = sessions.iter().map(String::as_str).collect();
    let super::super::pty_backend::Liveness::Known(live) =
        super::super::pty_backend::alive_session_ids(app, &names)
    else {
        return;
    };
    for session in live {
        let dir = mcp_scope_session_dir(&session);
        let app = app.clone();
        let name = session.clone();
        let host = app.clone();
        let alive = std::sync::Arc::new(move || {
            validate_restored_liveness(
                super::super::pty_backend::alive_session_ids(&host, &[&name]),
                &name,
            )
        });
        super::bind_relay_launch(&app, &session);
        if let Err(error) = restore_relay(&dir, alive) {
            log::warn!(target: "mcp_scope", "cannot restore scoped MCP for {session}: {error}");
        }
    }
}

fn restorable_relay_sessions() -> BTreeSet<String> {
    scope_roots()
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|session| {
            mcp_scope_session_dir(session)
                .join("local-relay-grant.json")
                .is_file()
        })
        .collect()
}

#[cfg(test)]
mod instance_restore_tests {
    use super::*;

    #[test]
    fn instance_relay_restore_finds_owned_and_legacy_sessions_once() {
        let mut home = crate::routines::test_support::TempHome::new();
        let tmp = home.isolate_tmpdir();
        let root = home.codesense_home().join("mcp-scope");
        let legacy = tmp.join("codesense-mcp-scope");
        for dir in [
            root.join("new"),
            legacy.join("legacy"),
            root.join("both"),
            legacy.join("both"),
        ] {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("local-relay-grant.json"), b"{}").unwrap();
        }
        assert_eq!(
            restorable_relay_sessions(),
            BTreeSet::from(["new".into(), "legacy".into(), "both".into()])
        );
        assert_eq!(mcp_scope_session_dir("new"), root.join("new"));
        assert_eq!(mcp_scope_session_dir("legacy"), legacy.join("legacy"));
        assert_eq!(mcp_scope_session_dir("both"), root.join("both"));
    }
}

fn validate_restored_liveness(
    liveness: super::super::pty_backend::Liveness<std::collections::HashSet<String>>,
    session: &str,
) -> Result<(), String> {
    if liveness.known_gone(session) {
        Err("scoped session is no longer live".into())
    } else {
        Ok(())
    }
}

fn restore_relay(
    dir: &Path,
    alive: std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
) -> Result<(), String> {
    let descriptor_path = dir.join("local-relay.json");
    let old = codesense_host_proto::scoped_mcp::read_descriptor(&descriptor_path)?;
    // A second app must not replace a relay that still owns the live grant.
    if std::os::unix::net::UnixStream::connect(&old.socket).is_ok() {
        return Ok(());
    }
    let identity = scope_identity(dir)?;
    let captured = dir.join("local-relay-grant.json");
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::symlink_metadata(&captured).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.mode() & 0o077 != 0 {
        return Err("captured MCP grant must be private".into());
    }
    let saved: SavedGrant =
        serde_json::from_slice(&std::fs::read(captured).map_err(|e| e.to_string())?)
            .map_err(|_| "cannot read captured MCP grant")?;
    let directory = dir.to_path_buf();
    let validate = std::sync::Arc::new(move || {
        alive()?;
        if scope_identity(&directory)? != identity {
            return Err("scoped launch directory changed".into());
        }
        Ok(())
    });
    publish_relay(dir, saved, validate)
}

fn publish_relay(
    dir: &Path,
    saved: SavedGrant,
    validate: std::sync::Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
) -> Result<(), String> {
    let descriptor =
        crate::mcp::local_mcp_relay::local_grant::start(dir, saved.grant, saved.aliases, validate)?;
    codesense_host_proto::scoped_mcp::write_descriptor(&dir.join("local-relay.json"), &descriptor)
}

#[cfg(test)]
mod restart_tests {
    use super::*;
    use codesense_host_proto::scoped_mcp::{self as wire, Descriptor, Hello};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn a_live_captured_grant_rearms_after_restart_and_survives_unknown_liveness() {
        let root = tempfile::Builder::new()
            .prefix("csr-")
            .tempdir_in("/tmp")
            .unwrap();
        let grant = crate::mcp::local_mcp_relay::config::parse_local_scope(
            &serde_json::json!({"mcp_servers":{"codesense":{"command":"/bin/cat","enabled_tools":["safe"]}}}),
            "mcp_servers", root.path(),
        ).unwrap();
        write_0600_exclusive(
            &root.path().join("local-relay-grant.json"),
            &serde_json::to_vec(&SavedGrant {
                grant,
                aliases: BTreeMap::from([("codesense".into(), "captured-alias".into())]),
            })
            .unwrap(),
        )
        .unwrap();
        let path = root.path().join("local-relay.json");
        wire::write_descriptor(
            &path,
            &Descriptor {
                version: wire::VERSION,
                socket: root.path().join("retired"),
                capability: "a".repeat(64),
            },
        )
        .unwrap();
        use super::super::super::pty_backend::Liveness;
        let live = std::sync::Arc::new(std::sync::Mutex::new(Liveness::Unknown));
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let validate = {
            let live = live.clone();
            let reads = reads.clone();
            std::sync::Arc::new(move || {
                let result =
                    validate_restored_liveness(live.lock().unwrap().clone(), "live-session");
                reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                result
            })
        };
        restore_relay(root.path(), validate).unwrap();
        let new = wire::read_descriptor(&path).unwrap();
        assert_ne!(new.capability, "a".repeat(64));
        // The restored listener must survive an Unknown validation tick, then accept
        // the same grant once the host answers live again.
        let before = reads.load(std::sync::atomic::Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while reads.load(std::sync::atomic::Ordering::SeqCst) == before {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        *live.lock().unwrap() = Liveness::Known(["live-session".into()].into());
        super::bind_fixture_relay_root(
            root.path().file_name().unwrap().to_str().unwrap(),
            codesense_pty_core::activity::peer::inspect(std::process::id()).unwrap(),
        );
        let mut stream = tokio::net::UnixStream::connect(&new.socket).await.unwrap();
        let hello = serde_json::to_vec(&Hello {
            version: wire::VERSION,
            capability: new.capability.clone(),
            server: "codesense".into(),
        })
        .unwrap();
        stream.write_u32(hello.len() as u32).await.unwrap();
        stream.write_all(&hello).await.unwrap();
        assert_eq!(stream.read_u8().await.unwrap(), 1);
        // App restart re-arms the captured grant, including its tool restriction.
        stream.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"notes_list\",\"arguments\":{}}}\n").await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            stream.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let refused: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(refused["error"]["code"], -32602);
        *live.lock().unwrap() = Liveness::Known(Default::default());
    }
}
