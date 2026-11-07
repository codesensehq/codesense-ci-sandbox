// SPDX-License-Identifier: MIT
//! What the desktop needs to launch OpenCode.

use super::{strings, Attribution, DesktopCli, PermissionBlock, RunnerRecipe, Starter, Unattended};
use crate::agents::model::{
    check_permission, known_permission_keys, set_permission_block, Permissions, ValidationError,
};
use crate::install::agent_account_store::AccountRequirement;
use crate::session_usage::event::{Tokens, UsageEvent};
use crate::session_usage::telemetry::{at_ms, Attrs};
use codesense_pty_core::agent_cli::AgentCli;
use std::path::Path;

pub(super) fn desktop() -> DesktopCli {
    DesktopCli {
        model_endpoint: None,
        account_requirement: AccountRequirement::None,
        supports_wake_delivery: true,
        supports_remote_launch: false,
        preview: false,
        attribution: Attribution::Unsupported("opencode"),
        runner: RunnerRecipe {
            default_model: None,
            // OpenCode is never the runner: a headless run hands it the prompt on stdin, and
            // an argv template would put a prompt that starts with `-` where a flag goes.
            analyze_args: Vec::new(),
            skill_args: Vec::new(),
        },
        starter: Starter {
            name: "OpenCode",
            description: "OpenCode with your existing login. Starts with the team's AGENTS.md when it has one.",
            unattended_name: "OpenCode (unattended)",
            unattended_description: "OpenCode that runs without asking for permission, for unattended background work.",
        },
        unattended: Unattended {
            bypass_flag: true,
            permissions: Permissions::default(),
        },
        escalates: |agent| {
            agent
                .permissions
                .as_ref()
                .and_then(|p| p.opencode.as_ref())
                .is_some_and(|p| p.permission.get("*").is_some_and(|a| a == "allow"))
        },
        launch_settings: crate::agents::permissions::opencode,
        teardown_home: None,
        home_config: None,
        seed_home: None,
        managed_launch_env,
        inline_config,
        shell_mcp_config,
        pretrust: |_, _, _| Vec::new(),
        permissions: PermissionBlock {
            validate: validate_permissions,
            set: |permissions, block| set_permission_block(&mut permissions.opencode, block),
        },
        launch_notes: |_| Vec::new(),
        permission_display: None,
        enforces_bridge_tool_allowlist: true,
        headless_settings,
        native_permission_rules: Some(crate::agents::adapter::native_permission_rules),
        status_line: None,
        usage_record,
        managed_settings: &[],
        session_log_usage: None,
    }
}

/// The one definition of an unattended OpenCode run's permission map, read by profile launches
/// and by `permissions::opencode` for a headless agent launch. `opencode run` auto-rejects the
/// first `ask` and stops with no answer, so nothing here asks: a tool outside the allows is
/// denied, which hides the tool or ends that call in an error turn, and the run goes on.
fn headless_settings() -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("*".into(), "deny".into());
    for tool in ["read", "glob", "grep", "list", "todowrite"] {
        map.insert(tool.into(), "allow".into());
    }
    map.insert("external_directory".into(), "deny".into());
    for pattern in crate::agents::permissions::bridge_tool_allow_patterns() {
        map.insert(pattern, "allow".into());
    }
    serde_json::Value::Object(map)
}

/// The plugin's `opencode.usage` record: one per finished assistant message. OpenCode's `input`
/// already excludes cache reads, and its `output` excludes reasoning, which bills as output.
fn usage_record(a: &Attrs) -> Option<UsageEvent> {
    if a.get("event.name")? != "opencode.usage" {
        return None;
    }
    let tokens = Tokens {
        input: a.count("input"),
        output: a.count("output").saturating_add(a.count("reasoning")),
        cache_read: a.count("cache_read"),
        cache_write: a.count("cache_write"),
        ..Default::default()
    };
    Some(UsageEvent {
        cli: AgentCli::OpenCode,
        event_id: a.id("message_id")?.to_string(),
        session_id: a.id("session.id")?.to_string(),
        at_ms: at_ms(a)?,
        cwd: None,
        model: a.get("model")?.to_string(),
        tokens,
        // OpenCode prices a model it has no price for at 0, which is unknown, not free. Only a
        // model named as free keeps a real zero.
        reported_cost_usd: a
            .real("cost")
            .filter(|c| {
                c.is_finite() && *c >= 0.0 && *c <= crate::session_usage::MAX_REPORTED_COST_USD
            })
            .filter(|c| *c > 0.0 || a.get("model").is_some_and(|m| m.ends_with("-free"))),
    })
}

const PLUGIN: &str = include_str!("../../../resources/opencode/codesense.js");
/// Where the plugin takes the waiter protocol; it never waits while this is unfilled.
const WAITER_SLOT: &str = "/* @waiter */ null";
/// Where the plugin takes the range its host must be in; it checks nothing while this is unfilled.
const SUPPORTED_SLOT: &str = "/* @supported */ null";
const PLUGIN_DIR: &str = ".codesense/opencode/codesense";
const PLUGIN_FILE: &str = "server.js";
const SHELL_CONFIG_ENV: &str = "CODESENSE_OPENCODE_CONFIG";
pub(crate) const SHELL_CONFIG_FILE: &str = "opencode-mcp.json";

fn shell_mcp_config(bridge: &str) -> Option<(&'static str, &'static str, serde_json::Value)> {
    let scope =
        codesense_pty_core::agent_cli::descriptor(crate::agents::adapter::AgentCli::OpenCode)
            .mcp_scope
            .as_ref()?;
    let mut config = serde_json::json!({
        (scope.server_map_key.clone()): {
            scope.bridge_id.clone(): scope.bridge_entry.render(bridge, None)
        }
    });
    // Without its plugin a typed OpenCode still runs, but its tab shows no activity.
    // A typed OpenCode's version is not known here; the plugin checks its own host.
    match inline_config(true) {
        Ok(entries) => config.as_object_mut().unwrap().extend(entries),
        Err(error) => log::warn!(target: "pty", "OpenCode activity plugin unavailable: {error}"),
    }
    Some((SHELL_CONFIG_ENV, SHELL_CONFIG_FILE, config))
}

/// CodeSense's own plugin as the `file://` entry of a launch's inline `plugin` list. The
/// file lives outside OpenCode's config directories, so no user plugin is touched and an
/// update rewrites it in place. An OpenCode outside the qualified range runs without it:
/// the plugin's events and hooks are contracts of the releases it was qualified on.
fn inline_config(qualified: bool) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    if !qualified {
        return Ok(serde_json::Map::new());
    }
    Ok(serde_json::Map::from_iter([(
        "plugins".to_owned(),
        serde_json::json!([plugin_url()?]),
    )]))
}

fn plugin_url() -> Result<String, String> {
    let home = dirs::home_dir().ok_or("home directory unavailable for the OpenCode plugin")?;
    let dir = home.join(PLUGIN_DIR);
    ensure_private_dir(&dir)?;
    let path = dir.join(PLUGIN_FILE);
    write_plugin(&path, &plugin_source())?;
    url::Url::from_directory_path(&dir)
        .map(String::from)
        .map_err(|()| format!("{} is not an absolute path", path.display()))
}

/// The plugin as installed: the one place its install-time slots are filled, each from the
/// single source of what it carries.
pub(super) fn plugin_source() -> String {
    let mut source = PLUGIN.replacen(SUPPORTED_SLOT, &supported_range(), 1);
    // A plugin without a waiter protocol never waits while its slot is unfilled.
    if let Some(protocol) = waiter_protocol() {
        source = source.replacen(WAITER_SLOT, &protocol, 1);
    }
    source
}

/// The range its host must be in, from the same `Release` the desktop checks versions with,
/// so the plugin and the launch warning cannot disagree about it.
fn supported_range() -> String {
    let release = &codesense_pty_core::agent_cli::descriptor(AgentCli::OpenCode).release;
    let range = serde_json::json!({
        "min": [release.min.0, release.min.1, release.min.2],
        "beforeMajor": release.before_major,
    });
    format!("/* @supported */ {range}")
}

/// What the adapter declares of the waiter (the hook events, which of them replace a waiter,
/// and the lines a waiter prints to renew itself and to confirm), so the protocol has one source.
fn waiter_protocol() -> Option<String> {
    use codesense_pty_core::agent_cli::{
        adapter, InboxDelivery, WAITER_CONFIRMATION_LINE, WAITER_RENEWAL_LINE,
    };
    let InboxDelivery::Waiter {
        events,
        replace_events,
        ..
    } = adapter(AgentCli::OpenCode).inbox()
    else {
        return None;
    };
    Some(
        serde_json::json!({
            "events": events,
            "replace": replace_events,
            "renewal": WAITER_RENEWAL_LINE,
            "confirmation": WAITER_CONFIRMATION_LINE,
        })
        .to_string(),
    )
}

fn managed_launch_env() -> Result<Vec<(String, String)>, String> {
    let home = dirs::home_dir().ok_or("home directory unavailable for OpenCode isolation")?;
    let xdg_home = home.join(PLUGIN_DIR).join("xdg");
    ensure_private_dir(&xdg_home)?;
    // The plugin once lived here and would load a second time beside the inline one.
    let stale = xdg_home.join("opencode/plugins/codesense-xdg.js");
    match std::fs::remove_file(&stale) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not remove {}: {error}", stale.display())),
    }

    let original_xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(vec![
        (
            "XDG_CONFIG_HOME".into(),
            xdg_home.to_string_lossy().into_owned(),
        ),
        ("CODESENSE_ORIGINAL_XDG_CONFIG_HOME".into(), original_xdg),
    ])
}

fn ensure_private_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("{} must be a plain directory", path.display()));
    }
    Ok(())
}

fn write_plugin(path: &Path, source: &str) -> Result<(), String> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match options.open(path) {
        Ok(mut file) => file.write_all(source.as_bytes()).map_err(|error| {
            let _ = std::fs::remove_file(path);
            format!("could not write {}: {error}", path.display())
        }),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(path)
                .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!("{} must be a plain file", path.display()));
            }
            let current = std::fs::read_to_string(path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?;
            if current == source {
                return Ok(());
            }
            let mut options = std::fs::OpenOptions::new();
            options.write(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW);
            }
            let mut file = options
                .open(path)
                .map_err(|error| format!("could not update {}: {error}", path.display()))?;
            file.write_all(source.as_bytes())
                .map_err(|error| format!("could not write {}: {error}", path.display()))
        }
        Err(error) => Err(format!("could not create {}: {error}", path.display())),
    }
}

fn validate_permissions(permissions: &Permissions) -> Result<(), ValidationError> {
    let Some(permission) = permissions.opencode.as_ref() else {
        return Ok(());
    };
    known_permission_keys(
        crate::agents::adapter::AgentCli::OpenCode,
        &permission.extra,
    )?;
    for (pattern, decision) in &permission.permission {
        check_permission(pattern)?;
        if !matches!(decision.as_str(), "allow" | "ask" | "deny") {
            return Err(ValidationError::InvalidPermission(decision.clone()));
        }
    }
    Ok(())
}
