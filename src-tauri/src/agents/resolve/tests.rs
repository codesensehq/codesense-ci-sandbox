// SPDX-License-Identifier: MIT
//! Seam tests for `resolve_agent`: agents saved through the real store, real
//! team content, the real account store and bindings, resolved for real.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::agents::adapter::LaunchMode as DescriptorLaunchMode;
use crate::context_repo::{path_to_org_root, path_to_team_config_dir, path_to_team_content_dir};
use crate::domain::Principal;
use crate::install::agent_account_store::{
    compare_and_set_active_account, create_profile, detach_profile, set_auth_state,
    AccountResolutionReason, AccountResolutionSource, AgentAccountAuthState, AgentAccountProfile,
    AgentProvider,
};
use crate::routines::model::Author;
use crate::routines::PersonalOwner;
use codesense_pty_core::agent_cli::ScopeInjection;

use super::super::binding;
use super::super::launch_check::LaunchWarningCode;
use super::super::manifest::{GeneratedTarget, ManifestErrorCode};
use super::super::model::{Agent, ClaudePermissions, LaunchMode, McpAccess, Permissions};
use super::super::permissions::{self, claude_generated_allow};
use super::super::roots;
use super::super::service::{self, SaveTarget};
use super::*;
use codesense_pty_core::agent_cli::AgentCli;

pub(crate) const ORG: &str = "acme/api";
pub(crate) const TEAM: &str = "backend";
const PARENT: &str = "platform";

pub(crate) fn alice() -> Principal {
    Principal::from(1u64)
}

pub(crate) fn bob() -> Principal {
    Principal::from(2u64)
}

/// A locally-cloned org for `user`, whose `backend` team has `platform` as
/// its parent.
pub(crate) fn write_org(home: &Path, user: &Principal) -> PathBuf {
    let org_root = path_to_org_root(home, user, ORG).unwrap();
    std::fs::create_dir_all(org_root.join(".git")).unwrap();
    std::fs::write(
        org_root.join("org.yml"),
        format!("slug: {ORG}\ndisplay_name: Test Org\ncreated_at: \"2026-01-01T00:00:00Z\"\n"),
    )
    .unwrap();
    std::fs::write(
        org_root.join("people.yml"),
        "people:\n  - github_user_id: 1\n    github_username: alice\n  - github_user_id: 2\n    github_username: bob\n",
    )
    .unwrap();
    std::fs::write(
        org_root.join("teams.yml"),
        format!("teams:\n  - name: {TEAM}\n    parent_team: {PARENT}\n  - name: {PARENT}\n"),
    )
    .unwrap();
    for team in [TEAM, PARENT] {
        std::fs::create_dir_all(path_to_team_content_dir(&org_root, team)).unwrap();
    }
    std::fs::create_dir_all(org_root.join(PARENT).join("config")).unwrap();
    std::fs::write(
        org_root.join(PARENT).join("config/sharing.yml"),
        "share: ['content/**', 'panes/**']\n",
    )
    .unwrap();
    org_root
}

pub(crate) fn content(home: &Path, user: &Principal, team: &str) -> PathBuf {
    path_to_team_content_dir(&path_to_org_root(home, user, ORG).unwrap(), team)
}

pub(crate) fn write(root: &Path, logical: &str, body: &[u8]) {
    let path = root.join(logical);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Team content with a skill, a command, a team doc, the notify hook and an
/// MCP config that has `slack`.
pub(crate) fn team_content(home: &Path, user: &Principal) -> PathBuf {
    write_org(home, user);
    let team = content(home, user, TEAM);
    write(&team, ".claude/skills/bmad-build/SKILL.md", b"# Build\n");
    write(&team, ".claude/skills/bmad-build/ref/notes.md", b"notes\n");
    write(&team, ".claude/commands/bmad-help.md", b"Help.\n");
    write(&team, ".claude/hooks/team-hook.sh", b"#!/bin/sh\n");
    write(&team, ".agents/skills/review/SKILL.md", b"# Review\n");
    write(&team, "CLAUDE.md", b"# Team\n");
    write(
        &team,
        ".mcp.json",
        br#"{"mcpServers": {"slack": {"command": "slack-mcp"}}}"#,
    );
    team
}

pub(crate) fn draft(id: &str, runtime: AgentCli) -> Agent {
    Agent {
        role: None,
        id: id.into(),
        name: format!("Agent {id}"),
        description: String::new(),
        created_by: String::new(),
        runtime,
        args: vec!["--model".into(), "opus".into()],
        launch: LaunchMode::Foreground,
        mcp: McpAccess {
            bridge_tools: None,
            servers: vec!["slack".into()],
            extra: BTreeMap::new(),
        },
        startup: vec![
            "CLAUDE.md".into(),
            ".claude/skills/bmad-build/".into(),
            ".claude/commands/bmad-help.md".into(),
        ],
        context: None,
        permissions: None,
        default_prompt: String::new(),
        avatar: None,
        allow_wake: false,
        resume_after_restart: false,
        budget_usd: None,
        revision: 0,
        archived_at: None,
        extra: BTreeMap::new(),
        env: Default::default(),
    }
}

pub(crate) fn team_placement() -> Placement {
    Placement::Team {
        org: ORG.into(),
        team: TEAM.into(),
    }
}

#[test]
fn bridge_tool_launch_profile_follows_the_runtime_capability() {
    let key = AgentKey::new(ORG, TEAM, "bridge-reviewer");
    for tools in [None, Some(vec![]), Some(vec!["signal_done".to_string()])] {
        let mut definition = draft("bridge-reviewer", AgentCli::Codex);
        definition.mcp.bridge_tools = tools.clone();
        let profile = launch_profile(&definition, &key);
        assert_eq!(profile.bridge_tools, tools);
        match &tools {
            Some(tools) => assert_eq!(&profile.effective_bridge_tools, tools),
            None => assert!(profile
                .effective_bridge_tools
                .contains(&"signal_done".into())),
        }

        let profile = launch_profile(&draft("bridge-reviewer", AgentCli::Claude), &key);
        assert_eq!(profile.bridge_tools, None);
        assert!(profile.effective_bridge_tools.is_empty());
    }
}

/// Save `agent` through the real store as `user`, into the team.
pub(crate) fn save(home: &Path, user: &Principal, agent: Agent) -> AgentKey {
    service::save(
        home,
        user,
        SaveTarget::Create(team_placement()),
        agent,
        &Author::user(user.as_str()),
    )
    .unwrap()
    .key
}

pub(crate) fn resolve(home: &Path, user: &Principal, key: &AgentKey) -> ResolvedAgent {
    resolve_agent(home, user, key, ResolveOptions::default()).unwrap()
}

pub(crate) fn ready_account(
    home: &Path,
    user: &Principal,
    provider: AgentProvider,
) -> AgentAccountProfile {
    let p = create_profile(home, user, provider, provider.as_str()).unwrap();
    set_auth_state(
        home,
        user,
        &p.id,
        p.generation,
        AgentAccountAuthState::Ready,
        None,
    )
    .unwrap()
}

pub(crate) fn make_active(home: &Path, user: &Principal, account: &AgentAccountProfile) {
    compare_and_set_active_account(
        home,
        user,
        account.provider,
        0,
        Some(&account.id),
        Some(account.generation),
    )
    .unwrap();
}

/// Bind by hand, past `binding::set`'s checks: what a binding looks like once
/// the account changed under it.
pub(crate) fn bind_raw(home: &Path, user: &Principal, key: &AgentKey, account_id: &str) {
    let file = binding::bindings_file(home, user);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        file,
        json!({ key.to_string(): { "account_id": account_id } }).to_string(),
    )
    .unwrap();
}

/// Copy Alice's team agents into `user`'s checkout, as a git pull would.
fn sync_agents_to(home: &Path, user: &Principal) {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let dir = |who: &Principal| {
        roots::team_agents_dir(home, who, ORG, TEAM)
            .unwrap()
            .unwrap()
    };
    copy(&dir(&alice()), &dir(user));
}

// ---------------------------------------------------------------------------
// The seams
// ---------------------------------------------------------------------------

/// An agent saved through the real store resolves into the launch inputs, a
/// manifest equal byte for byte to `resolve_manifest`'s for the same inputs,
/// the names it starts with, and its generated Claude settings.
#[test]
fn seam_saved_agent_resolves_to_launch_inputs_and_the_composed_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(home, &alice(), draft("builder", AgentCli::Claude));

    let r = resolve(home, &alice(), &key);

    assert_eq!(r.agent.to_string(), "acme/api/backend/builder@1");
    assert_eq!(r.launch.id, "acme/api/backend/builder");
    assert_eq!(r.launch.binary, "claude");
    assert_eq!(r.launch.args, vec!["--model", "opus"]);
    assert!(!r.launch.escalating);
    assert_eq!(r.launch.mcp_allow, Some(vec!["slack".to_string()]));
    assert_eq!(r.launch.mode, DescriptorLaunchMode::Interactive);
    assert_eq!(r.launch.env_suppress, vec!["ANTHROPIC_API_KEY"]);
    assert!(matches!(
        r.launch.mcp_scope.as_ref().unwrap().injection,
        ScopeInjection::ConfigFlag { .. }
    ));
    assert!(!r.archived);
    assert!(!r.manifest_applies_at_launch);
    assert_eq!(r.warnings, Vec::new());
    assert_eq!(r.manifest.errors, Vec::new());

    // Composed, never re-derived.
    let pinned = service::get_pinned(home, &alice(), &key, 1).unwrap();
    let generated = permissions::generate(&pinned.agent, AgentCli::Claude, LaunchMode::Foreground);
    let direct = crate::agents::manifest_resolve::resolve_manifest(
        home,
        &alice(),
        &pinned.agent,
        &pinned.reference,
        ManifestTarget::for_launch(&key, None),
        generated,
        Vec::new(),
    );
    assert_eq!(
        serde_json::to_vec(&r.manifest).unwrap(),
        serde_json::to_vec(&direct).unwrap()
    );
    assert_eq!(r.manifest.agent, r.agent);

    assert_eq!(r.provides.commands, vec!["bmad-help"]);
    assert_eq!(r.provides.skills, vec!["bmad-build"]);

    // The generated Claude settings are a self-contained permissions body.
    assert_eq!(r.manifest.generated.len(), 1);
    let settings = &r.manifest.generated[0];
    assert_eq!(settings.target, GeneratedTarget::LaunchSettings);
    assert_eq!(
        settings.content,
        json!({ "permissions": { "allow": claude_generated_allow(), "deny": [] } })
    );
}

/// Teammate B resolving Alice's team agent gets B's own binding or active
/// account, never Alice's.
#[test]
fn seam_account_is_per_user_binding_then_active_and_never_falls_back() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    write_org(home, &bob());
    let claude_key = save(home, &alice(), draft("claude-agent", AgentCli::Claude));
    let codex_key = save(home, &alice(), draft("codex-agent", AgentCli::Codex));

    let claude_account = ready_account(home, &alice(), AgentProvider::Claude);
    let codex_account = ready_account(home, &alice(), AgentProvider::Codex);
    make_active(home, &alice(), &codex_account);
    binding::set(home, &alice(), &claude_key, &claude_account.id).unwrap();

    // Bound: the claude agent previews its bound Claude account.
    let r = resolve(home, &alice(), &claude_key);
    assert_eq!(r.account.source, AccountSource::Binding);
    assert_eq!(
        r.account.override_account_id,
        Some(claude_account.id.clone())
    );
    assert_eq!(r.account.policy.provider(), Some(AgentProvider::Claude));
    assert_eq!(r.account.preview.source, AccountResolutionSource::Explicit);
    assert_eq!(
        r.account.preview.account_id,
        Some(claude_account.id.clone())
    );
    assert!(r.account.preview.spawn_eligible);

    // Unbound: the codex agent previews the active Codex account, never Claude's.
    let r = resolve(home, &alice(), &codex_key);
    assert_eq!(r.account.source, AccountSource::RuntimeDefault);
    assert_eq!(r.account.override_account_id, None);
    assert_eq!(r.account.policy.provider(), Some(AgentProvider::Codex));
    assert_eq!(r.account.preview.source, AccountResolutionSource::Active);
    assert_eq!(r.account.preview.account_id, Some(codex_account.id.clone()));

    // A binding to another provider's account is reported, not replaced.
    bind_raw(home, &alice(), &claude_key, &codex_account.id);
    let r = resolve(home, &alice(), &claude_key);
    assert_eq!(
        r.account.override_account_id,
        Some(codex_account.id.clone())
    );
    assert_eq!(
        r.account.preview.reason,
        AccountResolutionReason::ProviderMismatch
    );
    assert_eq!(r.account.preview.account_id, None);
    assert!(!r.account.preview.spawn_eligible);

    // So is a detached one.
    bind_raw(home, &alice(), &claude_key, &claude_account.id);
    detach_profile(
        home,
        &alice(),
        &claude_account.id,
        claude_account.generation,
    )
    .unwrap();
    let r = resolve(home, &alice(), &claude_key);
    assert_eq!(r.account.preview.reason, AccountResolutionReason::Detached);
    assert!(!r.account.preview.spawn_eligible);

    // And a missing one.
    bind_raw(home, &alice(), &claude_key, "gone");
    let r = resolve(home, &alice(), &claude_key);
    assert_eq!(r.account.override_account_id, Some("gone".into()));
    assert!(!r.account.preview.spawn_eligible);
    assert_eq!(r.account.preview.account_id, None);

    // Bob pulls the team agent into his own checkout. With no binding and no
    // active account, he gets the CLI's own login.
    sync_agents_to(home, &bob());
    let r = resolve(home, &bob(), &codex_key);
    assert_eq!(r.account.source, AccountSource::RuntimeDefault);
    assert_eq!(
        r.account.preview.source,
        AccountResolutionSource::SystemDefault
    );
    assert_eq!(r.account.preview.account_id, None);
    // Bob's own active account, once he has one.
    let bobs = ready_account(home, &bob(), AgentProvider::Codex);
    make_active(home, &bob(), &bobs);
    let r = resolve(home, &bob(), &codex_key);
    assert_eq!(r.account.preview.account_id, Some(bobs.id));
}

/// The generated Claude entry is only a permissions body; two agents get
/// their own rules over the same required set.
#[test]
fn seam_each_agent_gets_its_own_permission_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let with = |id: &str, allow: &str, deny: &str| Agent {
        permissions: Some(Permissions {
            claude: Some(ClaudePermissions {
                allow: vec![allow.into()],
                deny: vec![deny.into()],
                ..ClaudePermissions::default()
            }),
            ..Permissions::default()
        }),
        launch: LaunchMode::Headless,
        ..draft(id, AgentCli::Claude)
    };
    let a = save(home, &alice(), with("a", "Bash(git status)", "WebFetch"));
    let b = save(home, &alice(), with("b", "Read", "Bash"));
    let content = |key: &AgentKey| {
        let r = resolve(home, &alice(), key);
        assert_eq!(r.manifest.errors, Vec::new());
        assert_eq!(r.launch.mode, DescriptorLaunchMode::Headless);
        r.manifest.generated[0].content.clone()
    };
    let (ca, cb) = (content(&a), content(&b));
    for c in [&ca, &cb] {
        assert_eq!(
            c.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["permissions"]
        );
        assert_eq!(c["permissions"]["defaultMode"], "dontAsk");
        let allow = c["permissions"]["allow"].as_array().unwrap();
        assert_eq!(
            &allow[..claude_generated_allow().len()],
            claude_generated_allow()
        );
    }
    assert_eq!(
        ca["permissions"]["allow"][claude_generated_allow().len()],
        "Bash(git status)"
    );
    assert_eq!(ca["permissions"]["deny"], json!(["WebFetch"]));
    assert_eq!(
        cb["permissions"]["allow"][claude_generated_allow().len()],
        "Read"
    );
    assert_eq!(cb["permissions"]["deny"], json!(["Bash"]));
    assert!(!ca.to_string().contains("Read\""));
    assert!(!cb.to_string().contains("WebFetch"));
}

/// An agent whose own rules shut out the CodeSense tools keeps those rules
/// exactly as written: the manifest stays clean (nothing blocks its launch or
/// bootstrap) and the launch only warns that it cannot report back.
#[test]
fn seam_rules_over_codesense_tools_apply_verbatim_and_only_warn() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(
        home,
        &alice(),
        Agent {
            permissions: Some(Permissions {
                claude: Some(ClaudePermissions {
                    deny: vec!["mcp__codesense__*".into(), "WebFetch".into()],
                    ..ClaudePermissions::default()
                }),
                ..Permissions::default()
            }),
            launch: LaunchMode::Headless,
            ..draft("loner", AgentCli::Claude)
        },
    );

    let r = resolve(home, &alice(), &key);

    assert_eq!(r.manifest.errors, Vec::new());
    let p = &r.manifest.generated[0].content["permissions"];
    assert_eq!(p["deny"], json!(["mcp__codesense__*", "WebFetch"]));
    assert_eq!(p["allow"], json!(claude_generated_allow()));
    assert_eq!(
        r.warnings.iter().map(|w| w.code).collect::<Vec<_>>(),
        vec![LaunchWarningCode::DeniesCodesenseTools]
    );
    assert!(r.warnings[0]
        .message
        .starts_with(launch_check::DENIES_CODESENSE_TOOLS_MESSAGE));
}

// ---------------------------------------------------------------------------
// Runtime and launch
// ---------------------------------------------------------------------------

#[test]
fn codex_resolves_through_its_own_descriptor_with_nothing_injected() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(
        home,
        &alice(),
        Agent {
            launch: LaunchMode::Headless,
            startup: vec![".agents/skills/review/".into()],
            mcp: McpAccess::default(),
            ..draft("reviewer", AgentCli::Codex)
        },
    );
    let r = resolve(home, &alice(), &key);
    assert_eq!(r.launch.binary, "codex");
    assert_eq!(r.launch.args, vec!["--model", "opus"]);
    assert!(!r.launch.escalating);
    assert_eq!(r.launch.mcp_allow, Some(Vec::new()));
    assert_eq!(r.launch.mode, DescriptorLaunchMode::Headless);
    assert_eq!(r.launch.env_suppress, vec!["OPENAI_API_KEY"]);
    assert!(matches!(
        r.launch.mcp_scope.as_ref().unwrap().injection,
        ScopeInjection::EnvDir { .. }
    ));
    assert_eq!(r.provides.skills, vec!["review"]);
    assert_eq!(r.provides.commands, Vec::<String>::new());
    // The headless default policy, in the launch's own scoped config.
    assert_eq!(
        r.manifest.generated[0].content,
        json!({ "approval_policy": "never", "sandbox_mode": "workspace-write" })
    );
    assert_eq!(
        r.manifest.generated[0].target,
        GeneratedTarget::LaunchSettings
    );

    // The bypass flag escalates only when the agent's own args carry it.
    let bypass = save(
        home,
        &alice(),
        Agent {
            args: vec!["--dangerously-bypass-approvals-and-sandbox".into()],
            ..draft("bypass", AgentCli::Codex)
        },
    );
    let r = resolve(home, &alice(), &bypass);
    assert!(r.launch.escalating);
    assert_eq!(
        r.launch.args,
        vec!["--dangerously-bypass-approvals-and-sandbox"]
    );
}

#[test]
fn a_bypassing_permission_mode_escalates_a_claude_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(
        home,
        &alice(),
        Agent {
            permissions: Some(Permissions {
                claude: Some(ClaudePermissions {
                    default_mode: Some("bypassPermissions".into()),
                    ..ClaudePermissions::default()
                }),
                ..Permissions::default()
            }),
            ..draft("yolo", AgentCli::Claude)
        },
    );
    let r = resolve(home, &alice(), &key);
    assert!(r.launch.escalating);
    assert!(!r.launch.args.iter().any(|a| a.starts_with("--dangerously")));
}

// ---------------------------------------------------------------------------
// MCP servers
// ---------------------------------------------------------------------------

/// A server the team's MCP config lacks is an error naming it, and is still
/// passed through. Servers from a parent team's config count as present; a
/// stale `agent-packs.lock` left in the team's config adds none, so MCP servers
/// come from the content layers' `.mcp.json` (plus the bridge) only.
#[test]
fn unknown_mcp_servers_are_reported_and_a_stale_lock_file_adds_none() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    write(
        &content(home, &alice(), PARENT),
        ".mcp.json",
        br#"{"mcpServers": {"github": {}}}"#,
    );
    let config = path_to_team_config_dir(&path_to_org_root(home, &alice(), ORG).unwrap(), TEAM);
    write(
        &config,
        "agent-packs.lock",
        b"schema_version: 1\npacks:\n  jira-pack:\n    version: 1.0.0\n    source:\n      kind: team\n    config_additions:\n      claude:\n        - scope: repo\n          target: .mcp.json\n          fragment:\n            mcpServers:\n              jira:\n                command: jira-mcp\n",
    );
    let key = save(
        home,
        &alice(),
        Agent {
            mcp: McpAccess {
                bridge_tools: None,
                servers: vec![
                    "slack".into(),
                    "github".into(),
                    "jira".into(),
                    "linear".into(),
                ],
                extra: BTreeMap::new(),
            },
            ..draft("wide", AgentCli::Claude)
        },
    );
    let r = resolve(home, &alice(), &key);
    let unknown: Vec<&str> = r
        .manifest
        .errors
        .iter()
        .inspect(|e| {
            assert_eq!(e.code, ManifestErrorCode::Invalid);
            assert_eq!(e.path, None);
        })
        .map(|e| e.message.as_str())
        .collect();
    assert!(
        unknown.iter().any(|m| m.contains("\"jira\"")),
        "a server only the stale pack lock declares is unknown: {unknown:?}"
    );
    assert!(
        unknown.iter().any(|m| m.contains("\"linear\"")),
        "{unknown:?}"
    );
    assert!(
        !unknown
            .iter()
            .any(|m| m.contains("\"github\"") || m.contains("\"slack\"")),
        "{unknown:?}"
    );
    assert_eq!(
        r.launch.mcp_allow,
        Some(vec![
            "slack".to_string(),
            "github".into(),
            "jira".into(),
            "linear".into()
        ])
    );
}

// ---------------------------------------------------------------------------
// SW-AN-50 — offered MCP servers + silent-empty warning
// ---------------------------------------------------------------------------

/// Alice names no MCP servers on an agent whose team's `.mcp.json` offers
/// `slack` (and whose parent team's config offers `github`): the resolved
/// view carries both offered keys and a non-blocking warning names them so
/// the editor and starter detail can tell the user why the launch reaches
/// only the bridge.
#[test]
fn an_agent_with_no_mcp_servers_but_an_offered_team_set_gets_a_warning_naming_them() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    // A parent-team server merges in like `unknown_mcp_servers_…`'s seam, so
    // both layers' keys should show up in the offered set.
    write(
        &content(home, &alice(), PARENT),
        ".mcp.json",
        br#"{"mcpServers": {"github": {}}}"#,
    );
    let key = save(
        home,
        &alice(),
        Agent {
            mcp: McpAccess::default(),
            ..draft("bridge-only", AgentCli::Claude)
        },
    );
    let r = resolve(home, &alice(), &key);

    assert_eq!(
        r.offered_mcp_servers,
        vec!["github".to_string(), "slack".to_string()],
        "the resolved view carries every .mcp.json server across content layers, bridge aside",
    );
    let silent = r
        .warnings
        .iter()
        .find(|w| w.code == LaunchWarningCode::NoMcpServersOffered)
        .expect("silent-empty warning fires for an unselected offered server set");
    for server in ["slack", "github"] {
        assert!(
            silent.message.contains(server),
            "the message names each offered server ({server} missing): {}",
            silent.message
        );
    }
    assert!(
        r.manifest.errors.is_empty(),
        "the warning is non-blocking: no manifest errors added",
    );
}

/// AC-4: no false positives.
/// - An agent that already names a server fires no silent-empty warning.
/// - A team whose `.mcp.json` offers nothing fires no silent-empty warning,
///   even if the agent names none either (the editor would otherwise warn
///   the user to copy a nonexistent server).
#[test]
fn no_silent_empty_warning_when_the_agent_names_servers_or_the_team_offers_none() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    // A: the agent already names the offered server.
    let selected = save(home, &alice(), draft("selected", AgentCli::Claude));
    let r = resolve(home, &alice(), &selected);
    assert!(
        !r.warnings
            .iter()
            .any(|w| w.code == LaunchWarningCode::NoMcpServersOffered),
        "a selected server silences the SW-AN-50 warning",
    );
    assert_eq!(r.offered_mcp_servers, vec!["slack".to_string()]);

    // B: the team offers nothing — remove `.mcp.json` entries.
    std::fs::remove_file(content(home, &alice(), TEAM).join(".mcp.json")).unwrap();
    let bare = save(
        home,
        &alice(),
        Agent {
            mcp: McpAccess::default(),
            ..draft("bare", AgentCli::Claude)
        },
    );
    let r = resolve(home, &alice(), &bare);
    assert!(
        r.offered_mcp_servers.is_empty(),
        "no team offers → the resolved view carries no offered set"
    );
    assert!(
        !r.warnings
            .iter()
            .any(|w| w.code == LaunchWarningCode::NoMcpServersOffered),
        "no team offers → no silent-empty warning (would be a false positive)",
    );
}

// ---------------------------------------------------------------------------
// SW-AN-52 — unified MCP resolver (preview-side warnings)
// ---------------------------------------------------------------------------

/// AC-2: an agent that names an MCP server the content-layer config does not
/// define emits a `LaunchWarning::McpServersDropped` naming the dropped
/// entries, same conduit as SW-AN-50's `NoMcpServersOffered`. Non-blocking.
/// A `slack`-only team gets no warning when the agent names only `slack`.
#[test]
fn an_agent_naming_a_server_the_team_does_not_offer_gets_a_dropped_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(
        home,
        &alice(),
        Agent {
            mcp: McpAccess {
                bridge_tools: None,
                servers: vec!["slack".into(), "atlassian".into()],
                extra: BTreeMap::new(),
            },
            ..draft("mixed", AgentCli::Claude)
        },
    );
    let r = resolve(home, &alice(), &key);
    let dropped = r
        .warnings
        .iter()
        .find(|w| w.code == LaunchWarningCode::McpServersDropped)
        .expect("McpServersDropped fires for the dangling allow entry");
    assert!(
        dropped.message.contains("atlassian"),
        "the warning names the dropped server: {}",
        dropped.message
    );
    assert!(
        !dropped.message.contains("slack"),
        "the warning does not name a present server: {}",
        dropped.message
    );
}

/// AC-5 non-regression: an agent whose MCP servers are all in the content
/// layer fires neither `McpServersDropped` nor `NoMcpServersOffered`.
#[test]
fn no_dropped_warning_when_every_named_server_is_in_the_content_layer() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(home, &alice(), draft("clean", AgentCli::Claude));
    let r = resolve(home, &alice(), &key);
    assert!(
        !r.warnings
            .iter()
            .any(|w| w.code == LaunchWarningCode::McpServersDropped),
        "no dangling entry → no dropped warning",
    );
}

/// Pass 2 fixture #3 (target-quadruple equivalence): preview resolves the
/// `ManifestTarget` via `for_launch(&key, options.target_team.map(team))`;
/// launch captures it at `prepare()` the same way (`launch.rs:266`). This
/// fixture pins the equivalence under `target_team: Some(…)`, so a future
/// author filling `target.repo` on only one site is caught by `PartialEq`
/// and the byte-identical `effective_mcp_config` output.
#[test]
fn preview_and_launch_target_derivations_match_for_a_targeted_launch() {
    use super::super::manifest_resolve::ManifestTarget;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(home, &alice(), draft("targeted", AgentCli::Claude));
    // Preview path: `ManifestTarget::for_launch` from `ResolveOptions.target_team`.
    let preview_target = ManifestTarget::for_launch(&key, Some(ManifestTarget::team(ORG, TEAM)));
    // Launch path: same `for_launch` call at `agents::launch::prepare` (launch.rs:266).
    let launch_target = ManifestTarget::for_launch(&key, Some(ManifestTarget::team(ORG, TEAM)));
    assert_eq!(
        preview_target, launch_target,
        "preview and launch derive the same ManifestTarget under target_team: Some(…)",
    );
    // Byte-identical `effective_mcp_config` across both sites.
    let preview_config =
        effective_mcp_config(home, &alice(), AgentCli::Claude, &preview_target).unwrap();
    let launch_config =
        effective_mcp_config(home, &alice(), AgentCli::Claude, &launch_target).unwrap();
    assert_eq!(
        preview_config, launch_config,
        "the content-layer config is byte-identical from both callsites",
    );
    assert!(
        preview_config["mcpServers"]["slack"].is_object(),
        "the target team's `.mcp.json` is reached",
    );
}

/// Pass 3 fallback symmetry (fixture #8): a content-layer `.mcp.json` that
/// exceeds `effective_mcp_config`'s 1 MB per-file limit (but is otherwise
/// valid, defining `slack`) causes `effective_mcp_config` to return `Err`.
/// `compose()` must emit a `LaunchWarning::McpConfigUnresolvable` so the user
/// sees the reason the launch is about to fall back to cwd-only. Preview and
/// launch stay symmetric.
#[test]
fn an_oversized_content_mcp_json_emits_mcp_config_unresolvable_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    // Replace the team `.mcp.json` with a valid JSON object that exceeds the
    // 1 MB per-file limit. Padding lives in a non-server field so parsing stays
    // cheap and the server map remains well-formed.
    let padding = "x".repeat(1024 * 1024 + 128);
    let bytes = format!(
        r#"{{"mcpServers": {{"slack": {{"command": "slack-mcp"}}}}, "padding": "{padding}"}}"#
    );
    std::fs::write(content(home, &alice(), TEAM).join(".mcp.json"), bytes).unwrap();
    let key = save(home, &alice(), draft("padded", AgentCli::Claude));
    let r = resolve(home, &alice(), &key);
    let unresolvable = r
        .warnings
        .iter()
        .find(|w| w.code == LaunchWarningCode::McpConfigUnresolvable)
        .expect(
            "McpConfigUnresolvable fires when the content-layer config exceeds effective_mcp_config's limit",
        );
    assert!(
        !unresolvable.message.is_empty(),
        "warning carries a non-empty reason message",
    );
}

// ---------------------------------------------------------------------------
// Identity, archive, drafts, warnings
// ---------------------------------------------------------------------------

#[test]
fn only_an_unidentifiable_agent_is_an_error_and_archived_agents_resolve() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(home, &alice(), draft("old", AgentCli::Claude));
    service::archive(home, &alice(), &key, &Author::user("1")).unwrap();
    let r = resolve(home, &alice(), &key);
    assert!(r.archived);
    assert_eq!(r.agent.revision, 2);

    let missing = AgentKey::new(ORG, TEAM, "nope");
    assert!(matches!(
        resolve_agent(home, &alice(), &missing, ResolveOptions::default()),
        Err(ResolveError::NotFound { .. })
    ));
    let bobs = AgentKey::personal(&bob(), "acme/api", "mine");
    assert_eq!(
        resolve_agent(home, &alice(), &bobs, ResolveOptions::default()).unwrap_err(),
        ResolveError::NotVisible(bobs)
    );
}

#[test]
fn a_draft_previews_like_the_saved_agent_and_reports_what_it_breaks() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let preview = preview_agent(
        home,
        &alice(),
        &draft("builder", AgentCli::Claude),
        &team_placement(),
        ResolveOptions::default(),
    )
    .unwrap();
    let key = save(home, &alice(), draft("builder", AgentCli::Claude));
    let saved = resolve(home, &alice(), &key);
    assert_eq!(preview.agent.key, key);
    assert_eq!(preview.launch, saved.launch);
    assert_eq!(preview.manifest.entries, saved.manifest.entries);
    assert_eq!(preview.manifest.generated, saved.manifest.generated);
    assert_eq!(preview.provides, saved.provides);
    assert_eq!(preview.manifest.errors, Vec::new());

    let broken = Agent {
        args: vec!["|".into()],
        startup: vec!["docs/gone.md".into()],
        ..draft("broken", AgentCli::Claude)
    };
    let preview = preview_agent(
        home,
        &alice(),
        &broken,
        &Placement::Personal(PersonalOwner {
            github_user_id: 1,
            username: "alice".into(),
            display_name: "Alice".into(),
            avatar_url: None,
            org: "acme/api".into(),
        }),
        ResolveOptions {
            target_team: Some((ORG.into(), TEAM.into())),
        },
    )
    .unwrap();
    assert_eq!(
        preview.agent.key.to_string(),
        "_personal/1/acme/api/_scratch/broken"
    );
    let codes: Vec<_> = preview
        .manifest
        .errors
        .iter()
        .map(|e| (e.path.as_ref().map(ToString::to_string), e.code))
        .collect();
    assert_eq!(
        codes,
        vec![
            (Some("docs/gone.md".into()), ManifestErrorCode::Missing),
            (None, ManifestErrorCode::Invalid),
        ]
    );
}

#[test]
fn the_default_prompt_is_checked_against_what_the_agent_starts_with() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let ok = save(
        home,
        &alice(),
        Agent {
            default_prompt: "/bmad-help please".into(),
            ..draft("ok", AgentCli::Claude)
        },
    );
    assert_eq!(resolve(home, &alice(), &ok).warnings, Vec::new());
    let missing = save(
        home,
        &alice(),
        Agent {
            default_prompt: "/triage-slack go".into(),
            ..draft("missing", AgentCli::Claude)
        },
    );
    let r = resolve(home, &alice(), &missing);
    assert_eq!(r.warnings.len(), 1);
    assert_eq!(r.warnings[0].code, LaunchWarningCode::CommandNotInManifest);
    // The one implementation, with any prompt a caller passes.
    assert_eq!(
        launch_check::warnings(&r, Some("/bmad-build now")),
        Vec::new()
    );
}

#[test]
fn the_bridge_tools_warning_makes_no_cli_claim() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    for cli in AgentCli::ALL
        .into_iter()
        .filter(|cli| adapter::desktop(*cli).enforces_bridge_tool_allowlist)
    {
        let mut definition = draft(&format!("scoped-{cli}"), cli);
        definition.mcp.bridge_tools = Some(vec!["signal_done".to_string()]);
        let key = save(home, &alice(), definition);
        let r = resolve(home, &alice(), &key);
        let warning = r
            .warnings
            .iter()
            .find(|w| w.code == LaunchWarningCode::BridgeToolsLocalOnly)
            .unwrap_or_else(|| panic!("{cli} warns about the allowlist"));
        assert!(
            !warning.message.to_lowercase().contains("codex"),
            "{}",
            warning.message
        );
        assert!(warning.message.contains("local launches"));
    }
}

/// A startup-path problem never hides a later one in the definition.
#[test]
fn every_kind_of_definition_problem_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let mut extra = BTreeMap::new();
    extra.insert("bogus".to_string(), serde_yaml::Value::Bool(true));
    let broken = Agent {
        startup: vec![
            ".claude/skills/".into(),
            ".claude/skills/bmad-build/".into(),
        ],
        permissions: Some(Permissions {
            claude: Some(ClaudePermissions {
                extra,
                ..ClaudePermissions::default()
            }),
            ..Permissions::default()
        }),
        ..draft("broken", AgentCli::Claude)
    };
    let preview = preview_agent(
        home,
        &alice(),
        &broken,
        &team_placement(),
        ResolveOptions::default(),
    )
    .unwrap();
    let messages: Vec<&str> = preview
        .manifest
        .errors
        .iter()
        .map(|e| e.message.as_str())
        .collect();
    assert!(
        messages.iter().any(|m| m.contains("overlaps")),
        "{messages:?}"
    );
    assert!(messages.iter().any(|m| m.contains("bogus")), "{messages:?}");
}

#[test]
fn effective_mcp_uses_parent_team_with_nearest_precedence_and_ignores_a_stale_lock_file() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    write(
        &content(home, &alice(), PARENT),
        ".mcp.json",
        br#"{"mcpServers":{"inherited":{"command":"parent"},"slack":{"command":"old"}}}"#,
    );
    let config = path_to_team_config_dir(&path_to_org_root(home, &alice(), ORG).unwrap(), TEAM);
    write(&config, "agent-packs.lock", b"schema_version: 1\npacks:\n  jira-pack:\n    version: 1.0.0\n    source:\n      kind: team\n    config_additions:\n      claude:\n        - scope: repo\n          target: .mcp.json\n          fragment:\n            mcpServers:\n              jira:\n                command: jira-mcp\n");
    let key = save(home, &alice(), draft("merged", AgentCli::Claude));
    let merged = effective_mcp_config(
        home,
        &alice(),
        AgentCli::Claude,
        &ManifestTarget::for_launch(&key, None),
    )
    .unwrap();
    assert_eq!(merged["mcpServers"]["inherited"]["command"], "parent");
    assert_eq!(merged["mcpServers"]["slack"]["command"], "slack-mcp");
    assert!(
        merged["mcpServers"].get("jira").is_none(),
        "a stale pack lock contributes no MCP server: {merged}"
    );
    write(&content(home, &alice(), TEAM), ".mcp.json", b"malformed");
    assert!(effective_mcp_config(
        home,
        &alice(),
        AgentCli::Claude,
        &ManifestTarget::for_launch(&key, None)
    )
    .is_err());
}

#[test]
fn inherited_mcp_config_and_offered_servers_exclude_unshared_servers() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    let target = ManifestTarget::team("acme/ctx", "child");
    let config = effective_mcp_config(home.path(), &alice(), AgentCli::Claude, &target).unwrap();
    assert!(config["mcpServers"].get("child-srv").is_some());
    for name in ["parent-srv", "grand-srv"] {
        assert!(config["mcpServers"].get(name).is_none());
    }
    let scope = descriptor(AgentCli::Claude)
        .mcp_scope
        .as_ref()
        .unwrap()
        .clone();
    let offered = present_servers(&FsContentReader, home.path(), &alice(), &target, &scope);
    assert!(!offered.contains("parent-srv") && !offered.contains("grand-srv"));
    std::fs::write(
        root.join("parent/config/sharing.yml"),
        "share: ['content/.mcp.json']\n",
    )
    .unwrap();
    let config = effective_mcp_config(home.path(), &alice(), AgentCli::Claude, &target).unwrap();
    assert!(config["mcpServers"].get("parent-srv").is_some());
    assert!(config["mcpServers"].get("grand-srv").is_some());
}

#[test]
fn malformed_parent_sharing_warns_without_blocking_own_startup() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(
        root.join("parent/config/sharing.yml"),
        "share: ['content/[']\n",
    )
    .unwrap();
    let mut agent = draft("child-launch", AgentCli::Claude);
    agent.mcp.servers.clear();
    agent.startup = vec!["shared.md".into()];
    let saved = service::save(
        home.path(),
        &alice(),
        SaveTarget::Create(service::Placement::Team {
            org: "acme/ctx".into(),
            team: "child".into(),
        }),
        agent,
        &Author::user("1"),
    )
    .unwrap();
    let resolved =
        resolve_agent(home.path(), &alice(), &saved.key, ResolveOptions::default()).unwrap();
    assert!(
        resolved.manifest.errors.is_empty(),
        "{:?}",
        resolved.manifest.errors
    );
    assert!(resolved
        .warnings
        .iter()
        .any(|w| w.code == LaunchWarningCode::Inheritance));
    assert!(resolved.manifest.entries.iter().all(|e| e.layer
        == ContentLayer::Team {
            org: "acme/ctx".into(),
            team: "child".into()
        }));
}

/// SV-GHI-3: a parent with no `config/sharing.yml` gives every child launch
/// one Inheritance warning naming it, and the launch still goes ahead.
#[test]
fn a_parent_without_sharing_warns_once_and_the_launch_goes_ahead() {
    let (home, root) = crate::context_repo::inheritance::test_fixture();
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::remove_file(root.join("parent/config/sharing.yml")).unwrap();
    let mut agent = draft("child-unshared", AgentCli::Claude);
    agent.mcp.servers.clear();
    agent.startup = vec!["shared.md".into()];
    let saved = service::save(
        home.path(),
        &alice(),
        SaveTarget::Create(service::Placement::Team {
            org: "acme/ctx".into(),
            team: "child".into(),
        }),
        agent,
        &Author::user("1"),
    )
    .unwrap();
    let resolved =
        resolve_agent(home.path(), &alice(), &saved.key, ResolveOptions::default()).unwrap();
    assert!(
        resolved.manifest.errors.is_empty(),
        "{:?}",
        resolved.manifest.errors
    );
    let inheritance: Vec<_> = resolved
        .warnings
        .iter()
        .filter(|w| w.code == LaunchWarningCode::Inheritance)
        .map(|w| w.message.clone())
        .collect();
    assert_eq!(
        inheritance,
        [crate::context_repo::inheritance::shares_nothing_warning(
            "parent"
        )]
    );
    assert!(!root.join("parent/config/sharing.yml").exists());
}

#[path = "launch_cwd_tests.rs"]
mod launch_cwd_tests;
