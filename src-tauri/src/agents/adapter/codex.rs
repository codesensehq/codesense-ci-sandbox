// SPDX-License-Identifier: MIT
//! What the app alone needs to drive the `codex` CLI.

use super::{strings, Attribution, DesktopCli, PermissionBlock, RunnerRecipe, Starter, Unattended};
use crate::agents::model::{
    check_permission, flag_value, known_permission_keys, set_permission_block, Agent,
    CodexPermissions, Permissions, ValidationError,
};
use crate::install::agent_account_store::AccountRequirement;
use crate::session_usage::event::{Tokens, UsageEvent};
use crate::session_usage::telemetry::{at_ms, Attrs};
use codesense_pty_core::agent_cli::AgentCli;

pub(super) fn desktop() -> DesktopCli {
    DesktopCli {
        model_endpoint: None,
        account_requirement: AccountRequirement::Optional,
        supports_wake_delivery: true,
        supports_remote_launch: true,
        preview: false,
        attribution: Attribution::ArgvHooks,
        runner: RunnerRecipe {
            default_model: None,
            analyze_args: strings(&[
                "exec",
                "--dangerously-bypass-approvals-and-sandbox",
                "--skip-git-repo-check",
            ]),
            skill_args: strings(&[
                "exec",
                "--dangerously-bypass-approvals-and-sandbox",
                "--skip-git-repo-check",
            ]),
        },
        starter: Starter {
            name: "Codex",
            description: "Codex with its defaults and your own login. Starts with the team's AGENTS.md when it has one.",
            unattended_name: "Codex (unattended)",
            unattended_description: "Codex that runs without asking for permission (approval never, no sandbox), for unattended background work.",
        },
        // No approval is ever asked for (`never`: a refused command comes back to the model
        // instead of waiting) and no sandbox confines it (`danger-full-access`: network and
        // writes outside the workspace, e.g. a linked worktree's `.git`, all work): the
        // effect of the bypass flag, as permission keys.
        unattended: Unattended {
            bypass_flag: false,
            permissions: Permissions {
                codex: Some(CodexPermissions {
                    approval_policy: Some("never".into()),
                    sandbox_mode: Some("danger-full-access".into()),
                    ..CodexPermissions::default()
                }),
                ..Permissions::default()
            },
        },
        escalates,
        launch_settings: crate::agents::permissions::codex,
        teardown_home: None,
        home_config: None,
        seed_home: None,
        managed_launch_env: || Ok(Vec::new()),
        inline_config: |_| Ok(Default::default()),
        shell_mcp_config: |_| None,
        pretrust: crate::agents::workspace_trust::pretrust_codex_launch,
        permissions: PermissionBlock {
            validate: validate_permissions,
            set: |permissions, block| set_permission_block(&mut permissions.codex, block),
        },
        launch_notes: |_| Vec::new(),
        permission_display: None,
        enforces_bridge_tool_allowlist: true,
        // Its launch takes no settings file: a headless launch's approval policy rides its
        // permission keys.
        headless_settings: || serde_json::json!({}),
        native_permission_rules: None,
        // Its usage is polled (`Usage::Poll`).
        status_line: None,
        usage_record,
        managed_settings: &[],
        session_log_usage: None,
    }
}

fn validate_permissions(permissions: &Permissions) -> Result<(), ValidationError> {
    let Some(p) = permissions.codex.as_ref() else {
        return Ok(());
    };
    known_permission_keys(AgentCli::Codex, &p.extra)?;
    p.approval_policy
        .as_deref()
        .map(check_permission)
        .transpose()?;
    p.sandbox_mode
        .as_deref()
        .map(check_permission)
        .transpose()?;
    Ok(())
}

/// No sandbox and no approval ever asked for, from its flags or the agent's own keys.
fn escalates(agent: &Agent) -> bool {
    let args = agent.args.as_slice();
    let own = agent.permissions.as_ref().and_then(|p| p.codex.as_ref());
    let sandbox = flag_value(args, &["--sandbox", "-s"])
        .or_else(|| own.and_then(|p| p.sandbox_mode.as_deref()));
    let approval = flag_value(args, &["--ask-for-approval", "-a"])
        .or_else(|| own.and_then(|p| p.approval_policy.as_deref()));
    sandbox == Some("danger-full-access") && approval == Some("never")
}

/// Codex's `response.completed` event. Its input count includes the cached part, which is split out
/// here so each token is counted once. The event has no response id, so its key is the
/// conversation, the time, and the counts.
fn usage_record(a: &Attrs) -> Option<UsageEvent> {
    if a.get("event.name")? != "codex.sse_event"
        || a.get("event.kind") != Some("response.completed")
    {
        return None;
    }
    let session_id = a.id("conversation.id")?.to_string();
    let at = at_ms(a)?;
    let input_total = a.count("input_token_count");
    let cached = a.count("cached_token_count");
    let tokens = Tokens {
        input: input_total.saturating_sub(cached),
        output: a.count("output_token_count"),
        cache_read: cached,
        cache_write: a.count("cache_write_token_count"),
        cache_write_1h: 0,
    };
    Some(UsageEvent {
        cli: AgentCli::Codex,
        event_id: format!("{session_id}@{at}:{input_total}:{}", tokens.output),
        session_id,
        at_ms: at,
        cwd: None,
        model: a.get("model")?.to_string(),
        tokens,
        reported_cost_usd: None,
    })
}
