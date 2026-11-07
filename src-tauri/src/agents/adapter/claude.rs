// SPDX-License-Identifier: MIT
//! What the app alone needs to drive the `claude` CLI.

use std::collections::BTreeSet;

use super::{
    strings, Attribution, DesktopCli, PermissionBlock, RunnerRecipe, Starter, StatusLineInstall,
    Unattended,
};
use crate::agents::model::{
    check_permission, flag_value, known_permission_keys, set_permission_block, Agent,
    ClaudePermissions, Permissions, ValidationError,
};
use crate::install::agent_account_store::AccountRequirement;
use crate::session_usage::event::{Tokens, UsageEvent};
use crate::session_usage::telemetry::{at_ms, Attrs};
use codesense_pty_core::agent_cli::AgentCli;

/// Where Claude Code reads managed settings, in the order they are checked.
pub(crate) const MANAGED_SETTINGS_PATHS: [&str; 2] = [
    "/Library/Application Support/ClaudeCode/managed-settings.json",
    "/etc/claude-code/managed-settings.json",
];

pub(super) fn desktop() -> DesktopCli {
    DesktopCli {
        model_endpoint: None,
        account_requirement: AccountRequirement::Optional,
        supports_wake_delivery: true,
        supports_remote_launch: true,
        preview: false,
        attribution: Attribution::SettingsHooks,
        runner: RunnerRecipe {
            default_model: Some("claude-opus-4-6".into()),
            analyze_args: strings(&[
                "-p",
                "--output-format",
                "text",
                "--model",
                "{model}",
                "--max-turns",
                "1",
            ]),
            skill_args: strings(&[
                "-p",
                "--output-format",
                "text",
                "--model",
                "{model}",
                "--max-turns",
                "20",
                "--permission-mode",
                "acceptEdits",
            ]),
        },
        starter: Starter {
            name: "Claude",
            description: "Claude Code with its defaults and your own login. Starts with the team's CLAUDE.md when it has one.",
            unattended_name: "Claude (unattended)",
            unattended_description: "Claude Code that runs without asking for permission (permission checks skipped), for unattended background work.",
        },
        // The flag, and `defaultMode` set to match, so the per-launch settings file never
        // says otherwise.
        unattended: Unattended {
            bypass_flag: true,
            permissions: Permissions {
                claude: Some(ClaudePermissions {
                    default_mode: Some("bypassPermissions".into()),
                    ..ClaudePermissions::default()
                }),
                ..Permissions::default()
            },
        },
        escalates,
        launch_settings: crate::agents::permissions::claude,
        teardown_home: None,
        home_config: None,
        seed_home: None,
        managed_launch_env: || Ok(Vec::new()),
        inline_config: |_| Ok(Default::default()),
        shell_mcp_config: |_| None,
        pretrust: crate::agents::workspace_trust::pretrust_claude_launch,
        permissions: PermissionBlock {
            validate: validate_permissions,
            set: |permissions, block| set_permission_block(&mut permissions.claude, block),
        },
        launch_notes: |_| Vec::new(),
        permission_display: None,
        enforces_bridge_tool_allowlist: false,
        headless_settings,
        native_permission_rules: None,
        status_line: Some(StatusLineInstall {
            install: crate::commands::agent_accounts::install_claude_status_line,
            restore: crate::commands::agent_accounts::restore_owned_claude_status_line,
        }),
        usage_record,
        managed_settings: &MANAGED_SETTINGS_PATHS,
        session_log_usage: Some(|path: &std::path::Path| crate::session_usage::session_log_usage::read(AgentCli::Claude, path)),
    }
}

/// Its native `permissions` keys, each rule listed once.
fn validate_permissions(permissions: &Permissions) -> Result<(), ValidationError> {
    let Some(p) = permissions.claude.as_ref() else {
        return Ok(());
    };
    known_permission_keys(AgentCli::Claude, &p.extra)?;
    for list in [&p.allow, &p.ask, &p.deny, &p.additional_directories] {
        let mut seen = BTreeSet::new();
        for rule in list {
            check_permission(rule)?;
            if !seen.insert(rule.as_str()) {
                return Err(ValidationError::DuplicatePermission(rule.clone()));
            }
        }
    }
    p.default_mode
        .as_deref()
        .map(check_permission)
        .transpose()?;
    p.disable_bypass_permissions_mode
        .as_deref()
        .map(check_permission)
        .transpose()?;
    Ok(())
}

/// `dontAsk`: a tool outside the run's allow set is denied rather than asked about.
fn headless_settings() -> serde_json::Value {
    serde_json::json!({
        "permissions": { "defaultMode": crate::agents::permissions::CLAUDE_HEADLESS_DEFAULT_MODE }
    })
}

/// A permission mode that bypasses approval, from `--permission-mode` or the agent's
/// own `defaultMode`.
fn escalates(agent: &Agent) -> bool {
    let mode = flag_value(&agent.args, &["--permission-mode"]).or_else(|| {
        agent
            .permissions
            .as_ref()
            .and_then(|p| p.claude.as_ref())
            .and_then(|p| p.default_mode.as_deref())
    });
    mode == Some("bypassPermissions")
}

/// Claude Code's `api_request` event: one per model call, with its own request id.
fn usage_record(a: &Attrs) -> Option<UsageEvent> {
    if a.get("event.name")? != "api_request" {
        return None;
    }
    let tokens_total = a
        .count("input_tokens")
        .saturating_add(a.count("output_tokens"))
        .saturating_add(a.count("cache_read_tokens"))
        .saturating_add(a.count("cache_creation_tokens"));
    Some(UsageEvent {
        cli: AgentCli::Claude,
        event_id: a.id("request_id")?.to_string(),
        session_id: a.id("session.id")?.to_string(),
        at_ms: at_ms(a)?,
        cwd: None,
        model: a.get("model")?.to_string(),
        tokens: Tokens {
            input: a.count("input_tokens"),
            output: a.count("output_tokens"),
            cache_read: a.count("cache_read_tokens"),
            cache_write: a.count("cache_creation_tokens"),
            // Claude splits its cache writes by TTL; the one-hour part bills at 2x input.
            cache_write_1h: a
                .count("cache_creation_1h_tokens")
                .min(a.count("cache_creation_tokens")),
        },
        // A cost of 0 with tokens is a model with no price on file, not a free call.
        reported_cost_usd: a
            .real("cost_usd")
            .filter(|c| {
                c.is_finite() && *c >= 0.0 && *c <= crate::session_usage::MAX_REPORTED_COST_USD
            })
            .filter(|c| *c > 0.0 || tokens_total == 0),
    })
}
