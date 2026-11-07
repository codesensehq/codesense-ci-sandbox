// SPDX-License-Identifier: MIT
//! Seam tests for the launch step: agents saved through the real store,
//! prepared for real, and their generated entries written for real.

use std::path::Path;

use serde_json::json;

use crate::agents::adapter::LaunchMode;
use crate::agents::binding;
use crate::agents::launch_check::LaunchWarningCode;
use crate::agents::model::{
    Agent, ClaudePermissions, LaunchMode as AgentMode, McpAccess, Permissions,
};
use crate::agents::permissions::claude_generated_allow;
use crate::agents::resolve::tests::{
    alice, bob, draft, make_active, ready_account, save, team_content, write, write_org, ORG, TEAM,
};
use crate::agents::service::{self, SaveTarget};
use crate::agents::Placement;
use crate::commands::dashboard_launch::DashboardLaunchState;
use crate::install::agent_account_store::AgentProvider;
use crate::routines::model::Author;
use codesense_pty_core::agent_cli::AgentCli;

use super::*;

fn team() -> Option<(String, String)> {
    Some((ORG.to_string(), TEAM.to_string()))
}

fn prep(home: &Path, who: &Principal, key: &AgentKey, options: PrepareOptions<'_>) -> AgentLaunch {
    prepare(home, who, key, options).unwrap()
}

fn claude_with(id: &str, permissions: ClaudePermissions) -> Agent {
    Agent {
        permissions: Some(Permissions {
            claude: Some(permissions),
            ..Permissions::default()
        }),
        ..draft(id, AgentCli::Claude)
    }
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn codex_with_bridge_tools(id: &str, tools: serde_json::Value) -> Agent {
    let mut definition = serde_json::to_value(draft(id, AgentCli::Codex)).unwrap();
    definition["mcp"]["bridge_tools"] = tools;
    serde_json::from_value(definition).unwrap()
}

struct ScopeCleanup(String);

impl Drop for ScopeCleanup {
    fn drop(&mut self) {
        mcp_scope::remove_scope_config(&self.0);
    }
}

#[test]
fn prepare_composes_resolve_and_regenerates_for_the_effective_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(home, &alice(), draft("builder", AgentCli::Claude));
    let resolved = crate::agents::resolve_agent(
        home,
        &alice(),
        &key,
        crate::agents::ResolveOptions::default(),
    )
    .unwrap();

    // The agent as declared: exactly resolve's launch and manifest.
    let declared = prep(home, &alice(), &key, PrepareOptions::default());
    assert_eq!(declared.revision_label(), "acme/api/backend/builder@1");
    assert_eq!(declared.launch.id, key.to_string());
    assert_eq!(declared.launch.mode, LaunchMode::Interactive);
    assert_eq!(declared.manifest_hash, resolved.manifest.manifest_hash);
    assert_eq!(declared.generated, resolved.manifest.generated);
    assert_eq!(declared.manifest_errors, 0);
    assert_eq!(declared.runtime, AgentCli::Claude);

    // A launcher running it headless gets the headless entries and a hash that covers them.
    let headless = prep(
        home,
        &alice(),
        &key,
        PrepareOptions {
            mode_override: Some(LaunchMode::Headless),
            ..PrepareOptions::default()
        },
    );
    assert_eq!(headless.launch.mode, LaunchMode::Headless);
    assert_eq!(
        headless.generated[0].content,
        json!({ "permissions": {
            "allow": claude_generated_allow(), "deny": [], "defaultMode": "dontAsk"
        } })
    );
    assert_ne!(headless.manifest_hash, declared.manifest_hash);

    // A prompt's leading command is checked against what the agent starts with.
    let warned = prep(
        home,
        &alice(),
        &key,
        PrepareOptions {
            prompt: Some("/triage-slack now"),
            ..PrepareOptions::default()
        },
    );
    assert_eq!(warned.warnings.len(), 1);
    assert_eq!(
        warned.warnings[0].code,
        LaunchWarningCode::CommandNotInManifest
    );
    assert!(warned.warnings[0].message.contains("/triage-slack"));

    // Nothing was materialized into the team content.
    assert!(
        !crate::agents::resolve::tests::content(home, &alice(), TEAM)
            .join(".claude/settings.local.json")
            .exists()
    );
}

#[test]
fn prepare_codex_headless_runs_the_agents_own_args_with_the_default_policy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(
        home,
        &alice(),
        Agent {
            launch: AgentMode::Headless,
            default_prompt: "Triage the queue.".into(),
            ..draft("triage", AgentCli::Codex)
        },
    );
    let launch = prep(home, &alice(), &key, PrepareOptions::default());
    assert_eq!(launch.launch.binary, "codex");
    assert_eq!(launch.launch.args, vec!["--model", "opus"]);
    assert!(!launch
        .launch
        .args
        .iter()
        .any(|a| a == "--dangerously-bypass-approvals-and-sandbox"));
    assert_eq!(launch.launch.mcp_allow, Some(vec!["slack".to_string()]));
    assert_eq!(launch.launch.mode, LaunchMode::Headless);
    assert_eq!(
        launch.generated[0].content,
        json!({ "approval_policy": "never", "sandbox_mode": "workspace-write" })
    );
    assert_eq!(launch.default_prompt.as_deref(), Some("Triage the queue."));
    // The unknown `slack` server in the Codex config is a manifest error, recorded only.
    assert_eq!(launch.manifest_errors, 1);
}

#[test]
fn the_account_is_the_runtimes_own_at_the_previewed_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let codex_key = save(home, &alice(), draft("codex-agent", AgentCli::Codex));
    let codex_account = ready_account(home, &alice(), AgentProvider::Codex);
    binding::set(home, &alice(), &codex_key, &codex_account.id).unwrap();

    let launch = prep(home, &alice(), &codex_key, PrepareOptions::default());
    let input = launch.account_input();
    let policy = input.trusted_policy.clone().unwrap();
    assert_eq!(policy.provider(), Some(AgentProvider::Codex));
    assert_eq!(policy.runtime, AgentCli::Codex);
    // The account input names the runtime alone.
    assert_eq!(input.runtime, Some(AgentCli::Codex));
    assert_eq!(input.override_account_id, Some(codex_account.id.clone()));
    assert_eq!(
        input.override_account_generation,
        Some(codex_account.generation)
    );
    assert!(!input.use_system_default);
    launch.refuse_unusable_account().unwrap();

    // With no binding the active account applies, so no override is sent.
    let unbound = save(home, &alice(), draft("codex-unbound", AgentCli::Codex));
    make_active(home, &alice(), &codex_account);
    let input = prep(home, &alice(), &unbound, PrepareOptions::default()).account_input();
    assert_eq!(input.override_account_id, None);
    assert_eq!(input.override_account_generation, None);
}

#[test]
fn a_claude_account_bound_to_a_codex_agent_is_refused_before_spawn() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(home, &alice(), draft("codex-agent", AgentCli::Codex));
    let claude_account = ready_account(home, &alice(), AgentProvider::Claude);
    crate::agents::resolve::tests::bind_raw(home, &alice(), &key, &claude_account.id);

    let launch = prep(home, &alice(), &key, PrepareOptions::default());
    let refusal = launch.refuse_unusable_account().unwrap_err();
    assert_eq!(refusal, "agent account is unavailable: provider-mismatch");
}

#[test]
fn teammates_launch_one_agent_with_their_own_accounts_and_the_same_revision() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    team_content(home, &bob());
    let key = save(home, &alice(), draft("triage", AgentCli::Codex));
    // Bob's checkout has the same team agent, as a git pull brings it.
    let agents_dir = |who: &Principal| {
        crate::agents::roots::team_agents_dir(home, who, ORG, TEAM)
            .unwrap()
            .unwrap()
    };
    crate::routines::test_support::copy_dir(&agents_dir(&alice()), &agents_dir(&bob()));

    let alice_account = ready_account(home, &alice(), AgentProvider::Codex);
    let bob_account = ready_account(home, &bob(), AgentProvider::Codex);
    make_active(home, &alice(), &alice_account);
    binding::set(home, &bob(), &key, &bob_account.id).unwrap();

    let a = prep(home, &alice(), &key, PrepareOptions::default());
    let b = prep(home, &bob(), &key, PrepareOptions::default());
    assert_eq!(a.revision_label(), b.revision_label());
    assert_eq!(a.account_preview.account_id, Some(alice_account.id.clone()));
    assert_eq!(a.account_override, None);
    assert_eq!(b.account_preview.account_id, Some(bob_account.id.clone()));
    assert_eq!(b.account_input().override_account_id, Some(bob_account.id));
}

#[test]
fn a_named_agent_is_a_key_or_an_id_in_the_team_then_the_personal_space() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let team_key = save(home, &alice(), draft("triage", AgentCli::Claude));
    let personal = service::save(
        home,
        &alice(),
        SaveTarget::Create(Placement::Personal(crate::routines::PersonalOwner {
            github_user_id: 1,
            username: "alice".into(),
            display_name: "alice".into(),
            avatar_url: None,
            org: "acme/api".into(),
        })),
        draft("mine", AgentCli::Claude),
        &Author::user(alice().as_str()),
    )
    .unwrap()
    .key;
    let team = Some((ORG, TEAM));

    assert_eq!(
        find_agent(home, &alice(), "triage", team).unwrap(),
        team_key
    );
    assert_eq!(
        find_agent(home, &alice(), &team_key.to_string(), None).unwrap(),
        team_key
    );
    assert_eq!(find_agent(home, &alice(), "mine", team).unwrap(), personal);
    // An unreadable team agent is reported, never replaced by a same-named personal one.
    let mine_in_team = save(home, &alice(), draft("mine", AgentCli::Claude));
    let dir = crate::agents::roots::team_agents_dir(home, &alice(), ORG, TEAM)
        .unwrap()
        .unwrap()
        .join("mine");
    std::fs::write(dir.join("agent.yaml"), "runtime: [not, a, runtime]\n").unwrap();
    let unreadable = find_agent(home, &alice(), "mine", team).unwrap_err();
    assert!(
        unreadable.contains(&mine_in_team.to_string()),
        "{unreadable}"
    );
    let missing = find_agent(home, &alice(), "nope", team).unwrap_err();
    assert!(missing.contains("no agent \"nope\""), "{missing}");
    assert!(find_agent(home, &alice(), "a b", team).is_err());

    // Another user's personal agent is never theirs to launch.
    write_org(home, &bob());
    assert!(prepare_named(
        home,
        &bob(),
        &personal.to_string(),
        PrepareOptions::default()
    )
    .unwrap_err()
    .contains("not visible"));
}

#[test]
fn claude_settings_are_a_private_per_launch_file_outside_the_workspace() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(
        home,
        &alice(),
        claude_with(
            "guarded",
            ClaudePermissions {
                deny: vec!["Bash(git push --force:*)".into()],
                ..ClaudePermissions::default()
            },
        ),
    );
    let launch = prep(
        home,
        &alice(),
        &key,
        PrepareOptions {
            mode_override: Some(LaunchMode::Headless),
            ..PrepareOptions::default()
        },
    );
    let cwd = tmp.path().join("worktree");
    std::fs::create_dir_all(&cwd).unwrap();

    let applied = apply_generated(&launch, "sess-guarded", &cwd, true).unwrap();
    let file = applied.settings.as_ref().unwrap().path().to_path_buf();
    assert!(!file.starts_with(&cwd));
    assert_eq!(
        settings_file_session(&file).as_deref(),
        Some("sess-guarded")
    );
    assert!(file.starts_with(launch_settings_root().unwrap()));
    assert_eq!(mode_of(&file), 0o600);
    // The agent's rules, plus the status hooks the workspace's own settings lack.
    let written = read_json(&file);
    // CodeSense's credential-file denies are added to the agent's own at write time.
    let mut expected = launch.generated[0].content["permissions"].clone();
    expected["deny"].as_array_mut().unwrap().extend(
        codesense_pty_core::notify_hook::deny_rules()
            .iter()
            .map(|rule| json!(rule)),
    );
    assert_eq!(written["permissions"], expected);
    assert!(written["hooks"]["Stop"]
        .to_string()
        .contains("codesense-notify"));
    let deny: Vec<&str> = ["Bash(git push --force:*)"]
        .into_iter()
        .chain(
            codesense_pty_core::notify_hook::deny_rules()
                .iter()
                .map(String::as_str),
        )
        .collect();
    assert_eq!(
        json!({ "permissions": written["permissions"].clone() }),
        json!({ "permissions": {
            "allow": claude_generated_allow(),
            "deny": deny,
            "defaultMode": "dontAsk"
        } })
    );
    assert_eq!(
        applied.argv(|p| format!("'{p}'")),
        vec![
            "--settings".to_string(),
            format!("'{}'", file.to_string_lossy())
        ]
    );
    assert!(!cwd.join(".claude").exists());

    drop(applied);
    assert!(!file.exists());
}

/// A runtime that takes its settings as config overrides gets each generated key as its
/// own override in the launch recipe, and nothing is written for it.
#[test]
fn codex_keys_become_overrides_in_the_launch_recipe() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(
        home,
        &alice(),
        Agent {
            launch: AgentMode::Headless,
            ..draft("triage", AgentCli::Codex)
        },
    );
    let launch = prep(home, &alice(), &key, PrepareOptions::default());
    let cwd = tmp.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();

    let applied = apply_generated(&launch, "sess-codex", &cwd, true).unwrap();
    assert!(applied.settings.is_none());
    assert_eq!(
        applied.argv(str::to_string),
        [
            "-c",
            "approval_policy=\"never\"",
            "-c",
            "sandbox_mode=\"workspace-write\""
        ]
    );
    assert_eq!(std::fs::read_dir(&cwd).unwrap().count(), 0);

    // A key no Codex startup may carry is refused rather than passed on.
    let mut foreign = launch.clone();
    foreign.generated[0].content = serde_json::json!({ "model": "o3" });
    assert_eq!(
        apply_generated(&foreign, "sess-codex", &cwd, true).unwrap_err(),
        "unsupported generated Codex permission key"
    );
}

#[test]
fn unsupported_attribution_is_unwired_on_initial_launch_and_refresh() {
    use crate::attribution::testing::{assert_unwired, logged_calls, Launch};
    use crate::attribution::AttributionStatus;

    let _home = crate::routines::test_support::TempHome::new();
    let fixture = Launch::new(AgentCli::OpenCode);
    let session = format!("unsupported-attribution-{}", uuid::Uuid::new_v4());
    let expected = AttributionStatus::Unsupported {
        reason: "opencode".into(),
    };
    let applied = apply_generated(&fixture.launch, &session, &fixture.cwd, true).unwrap();
    assert!(applied.settings.is_none());
    assert_unwired(&applied.extra_args, None, &applied.extra_env);
    crate::attribution::testing::assert_reflog_marker(&applied.extra_env, Some("opencode"));
    assert_eq!(fixture.status(), Some(expected.clone()));
    assert!(logged_calls(&fixture.log()).is_empty());

    let request = crate::attribution::launch::Request {
        codesense_home: fixture.home(),
        principal: &alice(),
        cwd: &fixture.cwd,
        runtime: AgentCli::OpenCode,
    };
    let (edit, env) = refresh_attribution(&request, &session, &applied.extra_args, true).unwrap();
    assert!(edit.is_none());
    assert_eq!(env, crate::attribution::unsupported::launch_env("opencode"));
    assert_eq!(fixture.status(), Some(expected));
    assert!(logged_calls(&fixture.log()).is_empty());
    forget_launch_env(&session);
}

#[test]
fn opencode_uses_a_private_xdg_config_and_restores_it_for_shell_tools() {
    let home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(home, &alice(), draft("opencode", AgentCli::OpenCode));
    let launch = prep(home, &alice(), &key, PrepareOptions::default());
    let cwd = tmp.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();

    let applied = apply_generated(&launch, "sess-opencode", &cwd, true).unwrap();
    let permission = applied
        .extra_env
        .iter()
        .find(|(name, _)| name == "OPENCODE_CONFIG_CONTENT")
        .map(|(_, value)| serde_json::from_str::<serde_json::Value>(value).unwrap())
        .unwrap();
    assert_eq!(
        permission["permissions"][0],
        serde_json::json!({"action":"*","resource":"*","effect":"ask"})
    );
    assert!(permission.get("permission").is_none());
    let isolated = applied
        .extra_env
        .iter()
        .find(|(name, _)| name == "XDG_CONFIG_HOME")
        .map(|(_, value)| Path::new(value))
        .unwrap();
    assert_eq!(
        isolated,
        home_env.root().join(".codesense/opencode/codesense/xdg")
    );
    // The plugin is delivered through the inline config, never the isolated XDG directory.
    assert!(!isolated.join("opencode/plugins").exists());
    assert_eq!(mode_of(isolated), 0o700);

    // A copy an earlier build left there would load twice: the next launch removes it.
    let stale = isolated.join("opencode/plugins/codesense-xdg.js");
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, "old").unwrap();
    apply_generated(&launch, "sess-opencode", &cwd, true).unwrap();
    assert!(!stale.exists());
}

/// OpenCode's launch settings and its MCP scope share `OPENCODE_CONFIG_CONTENT`. A spawn
/// site that appends the settings env after the scope env would clobber the scope's plugin
/// and MCP config, so `project_and_apply_for_cli` folds them into one entry before any
/// caller sees them.
#[test]
fn opencode_managed_launch_folds_settings_into_the_scope_env_content() {
    let home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(
        home,
        &alice(),
        draft("opencode-managed", AgentCli::OpenCode),
    );
    let launch = prep(home, &alice(), &key, PrepareOptions::default());
    let cwd = tmp.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let session = format!("sess-opencode-managed-{}", uuid::Uuid::new_v4());

    let (scope, _creation, _applied) =
        project_and_apply_for_cli(&launch, &session, &cwd, None, None, str::to_string).unwrap();
    let _cleanup = ScopeCleanup(session);

    let contents: Vec<serde_json::Value> = scope
        .extra_env
        .iter()
        .filter(|(name, _)| name == "OPENCODE_CONFIG_CONTENT")
        .map(|(_, value)| serde_json::from_str(value).unwrap())
        .collect();
    assert_eq!(
        contents.len(),
        1,
        "one merged OPENCODE_CONFIG_CONTENT entry"
    );
    let config = &contents[0];
    // The scope's plugin survives the merge, pointing at the installed directory plugin.
    let plugins = config["plugins"].as_array().expect("plugins is a list");
    assert_eq!(plugins.len(), 1);
    let plugin = plugins[0].as_str().unwrap();
    assert!(plugin.starts_with("file://"), "{plugin}");
    assert!(
        plugin.ends_with("/.codesense/opencode/codesense/"),
        "{plugin}"
    );
    // The agent's permission rules are folded in beside it.
    assert_eq!(
        config["permissions"][0],
        serde_json::json!({"action":"*","resource":"*","effect":"ask"})
    );
    assert_eq!(config["share"], "disabled");
    assert_eq!(config["update"], "disable");
    // The XDG isolation env still rides alongside the merged entry.
    assert!(scope
        .extra_env
        .iter()
        .any(|(name, _)| name == "XDG_CONFIG_HOME"));
}

/// Two different Claude agents launched into one folder through the shared
/// projection step each get their own settings file, held by their own
/// session entry; ending one session removes only its file.
#[test]
fn two_agents_in_one_cwd_get_disjoint_settings_files() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let a_key = save(
        home,
        &alice(),
        claude_with(
            "a",
            ClaudePermissions {
                deny: vec!["Bash(rm:*)".into()],
                ..ClaudePermissions::default()
            },
        ),
    );
    let b_key = save(
        home,
        &alice(),
        claude_with(
            "b",
            ClaudePermissions {
                allow: vec!["Bash(npm test:*)".into()],
                ..ClaudePermissions::default()
            },
        ),
    );
    let cwd = tmp.path().join("shared");
    write(
        &cwd,
        ".mcp.json",
        br#"{"mcpServers": {"codesense": {"command": "bridge"}, "slack": {"command": "slack-mcp"}}}"#,
    );
    let registry = DashboardLaunchState::new(4);

    let mut files = Vec::new();
    for (handle, key) in [("agent-a", &a_key), ("agent-b", &b_key)] {
        let launch = prep(home, &alice(), key, PrepareOptions::default());
        let session = format!("{handle}-{}", uuid::Uuid::new_v4());
        let (scope, _, applied) =
            project_and_apply(&launch, &session, &cwd, None, str::to_string).unwrap();
        assert!(scope
            .extra_args
            .contains(&"--strict-mcp-config".to_string()));
        let argv = applied.argv(str::to_string);
        assert_eq!(argv[0], "--settings");
        let file = std::path::PathBuf::from(&argv[1]);
        assert!(!file.starts_with(&cwd));
        assert_eq!(mode_of(&file), 0o600);
        assert!(registry.begin(handle).unwrap());
        registry.hold_launch_settings(handle, applied.settings.unwrap());
        registry.bind(handle, &session, Some("codesense")).unwrap();
        crate::commands::mcp_scope::remove_scope_config(&session);
        files.push((file, session));
    }

    let (a_file, a_session) = &files[0];
    let (b_file, _) = &files[1];
    assert_ne!(a_file, b_file);
    let rules = |file: &Path| read_json(file)["permissions"].clone();
    let (a, b) = (rules(a_file), rules(b_file));
    // CodeSense's credential-file denies are added to each agent's own.
    let credential_denies = codesense_pty_core::notify_hook::deny_rules();
    let with_credential_denies = |own: &[&str]| -> serde_json::Value {
        json!(own
            .iter()
            .copied()
            .chain(credential_denies.iter().map(String::as_str))
            .collect::<Vec<_>>())
    };
    assert_eq!(a["deny"], with_credential_denies(&["Bash(rm:*)"]));
    assert_eq!(a["allow"], json!(claude_generated_allow()));
    assert_eq!(b["deny"], with_credential_denies(&[]));
    let mut b_allow: Vec<serde_json::Value> =
        claude_generated_allow().iter().map(|r| json!(r)).collect();
    b_allow.push(json!("Bash(npm test:*)"));
    assert_eq!(b["allow"], json!(b_allow));
    assert!(!cwd.join(".claude/settings.local.json").exists());

    // A's session ends: its entry, and so its file, goes; B's stays.
    registry.release_for_session(a_session);
    assert!(!a_file.exists());
    assert!(b_file.exists());
}

/// The sweep removes an aged-out file only when its session is known not to
/// be running: a live session's file stays however old it is, a recent file
/// stays, and a host that cannot answer leaves everything.
#[test]
fn the_startup_sweep_removes_only_aged_out_files_of_sessions_not_running() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let old = SystemTime::now() - Duration::from_secs(3600);
    let names = [
        "ended--a1.json",
        "headless-1b2c--b2.json",
        "live--c3.json",
        "recent--d4.json",
        "unnamed.json",
    ];
    for name in names {
        std::fs::write(dir.join(name), "{}").unwrap();
        if name != "recent--d4.json" {
            std::fs::File::options()
                .write(true)
                .open(dir.join(name))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
    }
    let exists = |name: &str| dir.join(name).exists();

    let mut asked = Vec::new();
    sweep_settings_in(dir, SystemTime::now(), Duration::from_secs(60), |ids| {
        asked = ids.iter().map(|s| s.to_string()).collect();
        None
    });
    asked.sort();
    assert_eq!(asked, vec!["ended", "headless-1b2c", "live"]);
    assert!(
        names.iter().all(|name| exists(name)),
        "unknown liveness keeps all"
    );

    sweep_settings_in(dir, SystemTime::now(), Duration::from_secs(60), |_| {
        Some(HashSet::from(["live".to_string()]))
    });
    assert!(!exists("ended--a1.json"));
    assert!(!exists("headless-1b2c--b2.json"));
    assert!(!exists("unnamed.json"));
    assert!(exists("live--c3.json"), "a running session keeps its file");
    assert!(exists("recent--d4.json"), "a recent file is never reaped");
}

#[test]
fn a_settings_file_is_named_for_its_session_and_never_by_a_path() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(home, &alice(), draft("triage", AgentCli::Claude));
    let launch = prep(home, &alice(), &key, PrepareOptions::default());
    for bad in ["", "..", "a/b", "/abs"] {
        assert!(
            apply_generated(&launch, bad, tmp.path(), true).is_err(),
            "{bad:?}"
        );
    }
}

#[test]
fn the_provenance_names_the_agent_revision_and_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(home, &alice(), draft("triage", AgentCli::Claude));
    let launch = prep(home, &alice(), &key, PrepareOptions::default());

    let fresh = launch.provenance(None, "h");
    assert!(fresh.run_id.starts_with("agent-launch:"));
    assert_eq!(fresh.story_id, "h");
    assert_eq!(fresh.agent.as_deref(), Some("acme/api/backend/triage@1"));
    assert_eq!(fresh.manifest_hash, Some(launch.manifest_hash.clone()));

    let kept = launch.provenance(
        Some(RunProvenance {
            run_id: "r1".into(),
            story_id: "s".into(),
            automation_id: Some("auto".into()),
            ..RunProvenance::default()
        }),
        "h",
    );
    assert_eq!(kept.run_id, "r1");
    assert_eq!(kept.automation_id.as_deref(), Some("auto"));
    assert_eq!(kept.agent.as_deref(), Some("acme/api/backend/triage@1"));
    let wire = serde_json::to_value(&kept).unwrap();
    assert_eq!(wire["agent"], json!("acme/api/backend/triage@1"));
    assert_eq!(wire["manifestHash"], json!(launch.manifest_hash));
}

#[test]
fn a_launch_mode_override_is_used_for_the_entries_but_never_injects_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(
        home,
        &alice(),
        Agent {
            mcp: McpAccess::default(),
            ..draft("plain", AgentCli::Codex)
        },
    );
    let interactive = prep(home, &alice(), &key, PrepareOptions::default());
    assert_eq!(interactive.generated[0].content, json!({}));
    let headless = prep(
        home,
        &alice(),
        &key,
        PrepareOptions {
            account: None,
            target_team: team(),
            mode_override: Some(LaunchMode::Headless),
            prompt: None,
        },
    );
    assert_eq!(headless.launch.args, interactive.launch.args);
    assert_eq!(
        headless.generated[0].content,
        json!({ "approval_policy": "never", "sandbox_mode": "workspace-write" })
    );
}

/// SLW-PT-19 seam: a personal agent filed under org A never launches into org B,
/// whether named by its full key or prepared with B as the target; in A it does.
#[test]
fn a_personal_agent_never_launches_into_another_orgs_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let personal = service::save(
        home,
        &alice(),
        SaveTarget::Create(Placement::Personal(crate::routines::PersonalOwner {
            github_user_id: 1,
            username: "alice".into(),
            display_name: "alice".into(),
            avatar_url: None,
            org: ORG.into(),
        })),
        draft("mine", AgentCli::Claude),
        &Author::user(alice().as_str()),
    )
    .unwrap()
    .key;
    let full = personal.to_string();

    let refused = find_agent(home, &alice(), &full, Some(("other/org", TEAM))).unwrap_err();
    assert!(refused.contains("cannot launch in other/org"), "{refused}");
    assert!(prepare(
        home,
        &alice(),
        &personal,
        PrepareOptions {
            target_team: Some(("other/org".into(), TEAM.into())),
            ..PrepareOptions::default()
        },
    )
    .is_err());
    // A bare id in org B never finds org A's personal agent either.
    assert!(find_agent(home, &alice(), "mine", Some(("other/org", TEAM))).is_err());

    assert_eq!(
        find_agent(home, &alice(), &full, team_ref()).unwrap(),
        personal
    );
    assert!(prepare(
        home,
        &alice(),
        &personal,
        PrepareOptions {
            target_team: team(),
            ..PrepareOptions::default()
        },
    )
    .is_ok());
}

fn team_ref() -> Option<(&'static str, &'static str)> {
    Some((ORG, TEAM))
}

/// SW-XS-14 AC3: a merge-marked `.mcp.json` across two layers reaches the
/// workspace at launch as one generated entry (the layers' deep merge) that
/// survives a mode override. Written into a git-backed workspace, it is added to
/// the CodeSense `info/exclude` block first; a tracked path, a workspace whose
/// index cannot be read (fail closed) and a path under a symlinked directory are
/// never written.
#[test]
fn merge_marked_files_reach_the_workspace_safely() {
    use crate::context_repo::{
        path_to_org_root, path_to_team_config_dir, path_to_team_content_dir,
    };
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let content = team_content(home, &alice()); // the team's `.mcp.json` has `slack`
    write(&content, ".claude/settings.json", br#"{"env": {"A": "1"}}"#);
    let org_root = path_to_org_root(home, &alice(), ORG).unwrap();
    write(
        &path_to_team_content_dir(&org_root, "platform"),
        ".mcp.json",
        br#"{"mcpServers": {"jira": {"command": "jira-mcp"}}}"#,
    );
    for path in [".mcp.json", ".claude/settings.json"] {
        crate::install::merge_marks::set_merge_mark(
            &path_to_team_config_dir(&org_root, TEAM),
            path,
            Default::default(),
        )
        .unwrap();
    }
    let key = save(
        home,
        &alice(),
        Agent {
            startup: vec![".mcp.json".into(), ".claude/settings.json".into()],
            ..draft("merger", AgentCli::Claude)
        },
    );
    let launch = prep(
        home,
        &alice(),
        &key,
        PrepareOptions {
            target_team: team(),
            mode_override: Some(LaunchMode::Headless),
            ..Default::default()
        },
    );
    assert_eq!(launch.manifest_errors, 0, "{:?}", launch.manifest.errors);
    assert_eq!(launch.generated[0].target, GeneratedTarget::LaunchSettings);
    let merged: Vec<&GeneratedEntry> = launch
        .generated
        .iter()
        .filter(|g| g.target == GeneratedTarget::WorkspaceFile)
        .collect();
    assert_eq!(
        merged.len(),
        2,
        "the merged files survive the mode override"
    );
    assert_eq!(
        merged[1].content["mcpServers"],
        json!({"jira": {"command": "jira-mcp"}, "slack": {"command": "slack-mcp"}})
    );

    // SW-XS-8: the merged files ride Step 2 (the one preparation engine).
    let mut want = crate::agents::prepare::WantSet::default();
    want.add_workspace_files(home, &alice(), &launch.generated)
        .unwrap();
    let dests: Vec<&str> = want.files.iter().map(|f| f.dest.as_str()).collect();
    assert_eq!(dests, vec![".claude/settings.json", ".mcp.json"]);
    let run = |root: &Path| {
        let legacy = || Ok(Vec::new());
        crate::agents::prepare::prepare(crate::agents::prepare::PrepareRequest {
            home,
            principal: &alice(),
            destination: crate::agents::prepare::Destination::Local { root },
            want: &want,
            origin: "step2:test".into(),
            workspace: "ws".into(),
            agent: "a".into(),
            legacy: &legacy,
            prune: false,
            progress: &|_| {},
            validate: &|| Ok(()),
            explicit: Default::default(),
        })
    };

    // A git-backed workspace: written, and in this workspace's own block.
    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();
    git2::Repository::init(&repo_dir).unwrap();
    let report = run(&repo_dir).unwrap();
    assert_eq!(report.written, vec![".claude/settings.json", ".mcp.json"]);
    let identity = crate::content_engine::context_lifecycle::local_identity(&repo_dir);
    let block = crate::content_engine::git_exclude::workspace_block(
        &repo_dir,
        &crate::agents::prepare::workspace_id(&identity),
    )
    .unwrap()
    .unwrap();
    assert!(block.contains(&".mcp.json".to_string()), "{block:?}");

    // A tracked path is never written.
    let tracked_dir = tmp.path().join("tracked");
    write(&tracked_dir, ".mcp.json", br#"{"mcpServers": {}}"#);
    let repo = git2::Repository::init(&tracked_dir).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(".mcp.json")).unwrap();
    index.write().unwrap();
    let report = run(&tracked_dir).unwrap();
    assert_eq!(report.skipped_tracked, vec![".mcp.json"]);
    assert_eq!(
        std::fs::read(tracked_dir.join(".mcp.json")).unwrap(),
        br#"{"mcpServers": {}}"#
    );

    // An index that cannot be read fails closed: nothing is written.
    let corrupt_dir = tmp.path().join("corrupt");
    std::fs::create_dir_all(&corrupt_dir).unwrap();
    git2::Repository::init(&corrupt_dir).unwrap();
    std::fs::write(corrupt_dir.join(".git/index"), b"not an index").unwrap();
    assert!(run(&corrupt_dir).is_err());
    assert!(!corrupt_dir.join(".mcp.json").exists());

    // A path under a symlinked directory is never written.
    let plain = tmp.path().join("plain");
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, plain.join(".claude")).unwrap();
    let report = run(&plain).unwrap();
    assert_eq!(report.written, vec![".mcp.json"]);
    assert_eq!(report.skipped_other[0].0, ".claude/settings.json");
    assert!(!elsewhere.join("settings.json").exists());
}

/// A headless Claude launch of an agent whose args are `args`, in a fresh workspace.
/// The caller sets up the team content once per home.
fn launch_with_args(home: &Path, id: &str, args: Vec<String>) -> (AgentLaunch, std::path::PathBuf) {
    let mut agent = claude_with(id, ClaudePermissions::default());
    agent.args = vec!["--model".into(), "opus".into()];
    let key = save(home, &alice(), agent);
    let mut launch = prep(
        home,
        &alice(),
        &key,
        PrepareOptions {
            mode_override: Some(LaunchMode::Headless),
            ..PrepareOptions::default()
        },
    );
    launch.launch.args = args;
    let cwd = home.join(format!("worktree-{id}"));
    std::fs::create_dir_all(&cwd).unwrap();
    (launch, cwd)
}

#[test]
fn an_agents_own_settings_merge_into_the_one_launch_file() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    team_content(tmp.path(), &alice());
    let agent_settings = r#"{"autoMemoryEnabled":false,"permissions":{"allow":["Bash(npm test)"]},"hooks":{"Stop":[{"matcher":"","hooks":[{"type":"command","command":"agent-hook"}]}]}}"#;
    let (launch, cwd) = launch_with_args(
        tmp.path(),
        "merged",
        vec![
            "--model".into(),
            "opus".into(),
            "--settings".into(),
            agent_settings.into(),
        ],
    );
    let applied = apply_generated(&launch, "sess-merged", &cwd, true).unwrap();
    assert!(applied.merged_agent_settings);
    let written = read_json(applied.settings.as_ref().unwrap().path());
    // The agent's own key survives; CodeSense's allows and status hooks are kept.
    assert_eq!(written["autoMemoryEnabled"], json!(false));
    let allow = written["permissions"]["allow"].as_array().unwrap();
    assert!(allow.contains(&json!("Bash(npm test)")));
    for rule in claude_generated_allow() {
        assert!(allow.contains(&json!(rule)), "{rule}");
    }
    let stop = written["hooks"]["Stop"].to_string();
    assert!(stop.contains("agent-hook") && stop.contains("codesense-notify"));
    // Claude keeps only the last `--settings`, so the agent's pair is replaced by CodeSense's.
    let mut args = launch.launch.args.clone();
    applied.drop_agent_settings(&mut args);
    args.extend(applied.argv(str::to_string));
    assert_eq!(args.iter().filter(|arg| *arg == "--settings").count(), 1);
    assert!(args.contains(&"--model".to_string()));
    assert!(!args.iter().any(|arg| arg.contains("autoMemoryEnabled")));
}

#[test]
fn an_agents_settings_file_resolves_against_the_workspace_in_either_flag_form() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    team_content(tmp.path(), &alice());
    for (i, flag) in [
        vec!["--settings".to_string(), "agent-settings.json".to_string()],
        vec!["--settings=agent-settings.json".to_string()],
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("file-settings-{i}");
        let (launch, cwd) = launch_with_args(tmp.path(), &id, flag);
        std::fs::write(
            cwd.join("agent-settings.json"),
            br#"{"autoMemoryEnabled":false}"#,
        )
        .unwrap();
        let applied = apply_generated(&launch, &id, &cwd, true).unwrap();
        assert!(applied.merged_agent_settings, "{id}");
        let written = read_json(applied.settings.as_ref().unwrap().path());
        assert_eq!(written["autoMemoryEnabled"], json!(false), "{id}");
        assert!(written["hooks"]["Stop"]
            .to_string()
            .contains("codesense-notify"));
    }
}

#[test]
fn a_bad_agent_settings_flag_is_refused_before_anything_is_written() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    team_content(tmp.path(), &alice());
    let cases: [(&str, Vec<String>, &str); 4] = [
        ("bare", vec!["--settings".into()], "has no value"),
        (
            "twice",
            vec!["--settings".into(), "{}".into(), "--settings=x.json".into()],
            "one --settings",
        ),
        (
            "malformed",
            vec!["--settings".into(), r#"{"autoMemoryEnabled":"#.into()],
            "not JSON",
        ),
        (
            "not-an-object",
            vec!["--settings".into(), "[1]".into()],
            "cannot be read",
        ),
    ];
    for (id, args, message) in cases {
        let (launch, cwd) = launch_with_args(tmp.path(), id, args);
        let error = apply_generated(&launch, id, &cwd, true).unwrap_err();
        assert!(error.contains(message), "{id}: {error}");
        assert!(!cwd.join(".claude").exists(), "{id}");
    }
}

/// A launch takes one launch-settings entry; a second is refused before any settings
/// file is written.
#[test]
fn a_second_launch_settings_entry_is_refused_before_anything_is_written() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    team_content(tmp.path(), &alice());
    let (mut launch, cwd) = launch_with_args(tmp.path(), "doubled", Vec::new());
    let entry = launch.generated[0].clone();
    launch.generated.push(entry);
    let error = apply_generated(&launch, "doubled", &cwd, true).unwrap_err();
    assert!(error.contains("2 launch settings entries"), "{error}");
    assert!(!cwd.join(".claude").exists());
}

#[test]
fn merging_keeps_the_agents_keys_unions_arrays_and_lets_codesense_scalars_win() {
    let mut merged = json!({
        "autoMemoryEnabled": false,
        "defaultMode": "plan",
        "permissions": {"allow": ["Read"], "defaultMode": "plan"},
        "hooks": {"Stop": [{"hooks": [{"command": "agent"}]}]}
    });
    merge_json(
        &mut merged,
        json!({
            "permissions": {"allow": ["Bash(ls)", "Read"], "defaultMode": "dontAsk"},
            "hooks": {"Stop": [{"matcher": "", "hooks": [{"command": "codesense"}]}]}
        }),
    );
    assert_eq!(merged["autoMemoryEnabled"], json!(false));
    assert_eq!(merged["defaultMode"], json!("plan"));
    assert_eq!(merged["permissions"]["defaultMode"], json!("dontAsk"));
    assert_eq!(merged["permissions"]["allow"], json!(["Read", "Bash(ls)"]));
    assert_eq!(merged["hooks"]["Stop"].as_array().unwrap().len(), 2);
}

#[test]
fn dropping_the_agents_settings_touches_only_its_pair_and_only_when_merged() {
    let merged = AppliedLaunch {
        merged_agent_settings: true,
        settings: Some(LaunchSettingsFile::adopt(PathBuf::from(
            "/nonexistent/settings.json",
        ))),
        ..AppliedLaunch::default()
    };
    let mut spaced: Vec<String> = ["--model", "opus", "--settings", "{}", "--mcp-config", "x"]
        .map(String::from)
        .into();
    merged.drop_agent_settings(&mut spaced);
    assert_eq!(spaced, ["--model", "opus", "--mcp-config", "x"]);
    let mut joined: Vec<String> = ["--settings=a.json", "--model", "opus"]
        .map(String::from)
        .into();
    merged.drop_agent_settings(&mut joined);
    assert_eq!(joined, ["--model", "opus"]);
    let untouched = AppliedLaunch::default();
    let mut kept: Vec<String> = ["--settings", "{}"].map(String::from).into();
    untouched.drop_agent_settings(&mut kept);
    assert_eq!(kept, ["--settings", "{}"]);
    // With CodeSense's own flag already after it, the agent's is still the one dropped.
    let mut both: Vec<String> = [
        "--settings",
        "{}",
        "--model",
        "opus",
        "--settings",
        "/ours.json",
    ]
    .map(String::from)
    .into();
    merged.drop_agent_settings(&mut both);
    assert_eq!(both, ["--model", "opus", "--settings", "/ours.json"]);
}

#[test]
fn identity_saved_agent_launches_project_the_canonical_bridge() {
    for runtime in AgentCli::ALL {
        // A CLI with a managed home launches its fixture install; the others a stub CLI.
        let managed = crate::agents::adapter::desktop(runtime)
            .seed_home
            .is_some()
            .then(crate::agents::adapter::ManagedHomeFixture::new);
        let plain = managed
            .is_none()
            .then(crate::routines::test_support::TempHome::new);
        let home_env = managed
            .as_ref()
            .map_or_else(|| plain.as_ref().unwrap().root(), |fixture| fixture.root())
            .to_path_buf();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        team_content(home, &alice());
        let mut definition = draft("identity-builder", runtime);
        if crate::agents::adapter::desktop(runtime)
            .model_endpoint
            .is_some()
        {
            definition.set_startup_settings(Some(std::collections::BTreeMap::from([("model".into(), json!({"provider":"custom", "base_url":"http://127.0.0.1:11434/v1", "default":"local-model"}))])));
        }
        let key = save(home, &alice(), definition);
        let launch = prep(home, &alice(), &key, PrepareOptions::default());
        let cwd = home.join("worktree");
        std::fs::create_dir_all(&cwd).unwrap();
        let session = format!("identity-launch-{}", uuid::Uuid::new_v4());
        let (_scope, _creation, _applied) = match &managed {
            Some(fixture) => {
                let program = fixture.managed_program();
                let cli = crate::commands::mcp_scope::LaunchCli::new(&program, &launch.launch.args);
                project_and_apply_for_cli(&launch, &session, &cwd, None, Some(&cli), str::to_string)
            }
            None => project_and_apply(&launch, &session, &cwd, None, str::to_string),
        }
        .unwrap();
        let dir = crate::commands::mcp_scope::mcp_scope_session_dir(&session);
        match runtime {
            AgentCli::Claude => assert!(read_json(&dir.join(".mcp.json"))["mcpServers"]
                ["codesense"]["command"]
                .is_string()),
            AgentCli::Codex => {
                let grant = read_json(&dir.join("local-relay-grant.json"));
                assert_eq!(
                    grant["grant"]["codesense"]["endpoint"]["Stdio"]["env"]
                        ["CODESENSE_TAB_SESSION_ID"],
                    session
                );
            }
            AgentCli::OpenCode => {
                let value = _scope
                    .extra_env
                    .iter()
                    .find(|(key, _)| key == "OPENCODE_CONFIG_CONTENT")
                    .unwrap();
                let config: serde_json::Value = serde_json::from_str(&value.1).unwrap();
                assert_eq!(
                    config["mcp"]["codesense"]["environment"]["CODESENSE_TAB_SESSION_ID"],
                    session
                );
                // The activity plugin rides the same inline config as the bridge.
                let plugin = format!(
                    "file://{}",
                    home_env.join(".codesense/opencode/codesense/").display()
                );
                assert_eq!(config["plugins"], serde_json::json!([plugin]));
                assert!(std::fs::read_to_string(
                    Path::new(plugin.trim_start_matches("file://")).join("server.js")
                )
                .unwrap()
                .contains("--activity-hook"));
            }
            AgentCli::Hermes => {
                let config: serde_json::Value =
                    serde_yaml::from_slice(&std::fs::read(dir.join("config.yaml")).unwrap())
                        .unwrap();
                assert_eq!(
                    config["mcp_servers"]["codesense"]["env"]["CODESENSE_TAB_SESSION_ID"],
                    session
                );
            }
        }
        crate::commands::mcp_scope::remove_scope_config(&session);
    }
}

#[test]
fn saved_codex_bridge_policy_reaches_alias_and_captured_relay() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tools = json!(["who_am_i", "read_messages", "reply", "signal_done"]);
    for (approval, sandbox, restriction) in [
        ("never", "read-only", tools.clone()),
        ("never", "workspace-write", tools.clone()),
        ("never", "danger-full-access", serde_json::Value::Null),
        ("on-request", "workspace-write", tools.clone()),
        ("untrusted", "read-only", tools.clone()),
        ("never", "read-only", json!([])),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        team_content(home, &alice());
        let mut definition = codex_with_bridge_tools("bridge-reviewer", restriction.clone());
        definition.mcp.servers = vec!["codegraph".into()];
        definition.permissions = Some(
            serde_json::from_value(json!({"codex": {
                "approval_policy": approval, "sandbox_mode": sandbox
            }}))
            .unwrap(),
        );
        let key = save(home, &alice(), definition);
        let launch = prep(home, &alice(), &key, PrepareOptions::default());
        let cwd = home.join("worktree");
        std::fs::create_dir_all(cwd.join(".codex")).unwrap();
        std::fs::write(
            cwd.join(".codex/config.toml"),
            "[mcp_servers.codegraph]\ncommand = 'codegraph-mcp'\nenabled_tools = ['explore']\n",
        )
        .unwrap();
        let session = format!("bridge-policy-{}", uuid::Uuid::new_v4());
        let _cleanup = ScopeCleanup(session.clone());
        let (scope, _creation, applied) =
            project_and_apply(&launch, &session, &cwd, None, str::to_string).unwrap();
        let config: toml::Value = toml::from_str(scope.extra_args.last().unwrap()).unwrap();
        let native = &config["mcp_servers"][&scope.server_aliases["codesense"]];
        assert_eq!(
            native["default_tools_approval_mode"].as_str(),
            Some("approve")
        );
        let actual = native
            .get("enabled_tools")
            .map(|v| serde_json::to_value(v).unwrap());
        let mut expected = restriction.clone();
        if let Some(names) = expected.as_array_mut() {
            names.sort_by_key(|v| v.as_str().unwrap().to_owned());
        }
        assert_eq!(actual, (!restriction.is_null()).then_some(expected.clone()));
        let grant =
            read_json(&mcp_scope::mcp_scope_session_dir(&session).join("local-relay-grant.json"));
        let captured = &grant["grant"]["codesense"]["enabled_tools"];
        assert_eq!(*captured, expected);
        let other = &config["mcp_servers"][&scope.server_aliases["codegraph"]];
        assert!(other.get("default_tools_approval_mode").is_none());
        assert_eq!(
            grant["grant"]["codegraph"]["enabled_tools"],
            json!(["explore"])
        );
        assert!(applied
            .extra_args
            .contains(&format!("approval_policy=\"{approval}\"")));
        assert!(applied
            .extra_args
            .contains(&format!("sandbox_mode=\"{sandbox}\"")));
    }
}

#[tokio::test]
async fn saved_bridge_allowlist_filters_listing_and_refuses_direct_calls() {
    let _home_env = crate::routines::test_support::TempHome::new();
    use codesense_host_proto::scoped_mcp::{self as wire, Hello};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let mut definition = codex_with_bridge_tools(
        "relay-reviewer",
        json!([
            "who_am_i",
            "read_messages",
            "reply",
            "signal_done",
            "codesense_call_tool"
        ]),
    );
    definition.mcp.servers.clear();
    let key = save(home, &alice(), definition);
    let launch = prep(home, &alice(), &key, PrepareOptions::default());
    let cwd = home.join("workspace");
    std::fs::create_dir_all(&cwd).unwrap();
    let marker = home.join("excluded-called");
    let bridge = home.join("bridge.py");
    std::fs::write(&bridge, format!(r#"#!/usr/bin/python3
import sys,json,pathlib
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 if r['method']=='tools/list':
  result={{'tools':[{{'name':n,'inputSchema':{{'type':'object'}}}} for n in ['who_am_i','notes_list']]}}
 else:
  name=r['params']['name']
  if name=='codesense_call_tool': name=r['params']['arguments']['name'].strip()
  if name=='notes_list': pathlib.Path({marker:?}).touch()
  result={{'content':[{{'type':'text','text':'allowed'}}]}}
 print(json.dumps({{'jsonrpc':'2.0','id':r['id'],'result':result}}),flush=True)
"#, marker=marker.to_str().unwrap())).unwrap();
    std::fs::set_permissions(&bridge, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    let fixture = mcp_scope::inventory::Fixture::new();
    let cli = fixture.launch_with_bridge(&launch.launch.args, &bridge);
    let session = format!("bridge-relay-{}", uuid::Uuid::new_v4());
    let _cleanup = ScopeCleanup(session.clone());
    let (_scope, _creation, _applied) = std::thread::scope(|threads| {
        threads
            .spawn(|| {
                project_and_apply_for_cli(&launch, &session, &cwd, None, Some(&cli), str::to_string)
            })
            .join()
            .unwrap()
            .unwrap()
    });
    mcp_scope::bind_fixture_relay_root(
        &session,
        codesense_pty_core::activity::peer::inspect(std::process::id()).unwrap(),
    );
    let descriptor =
        wire::read_descriptor(&mcp_scope::mcp_scope_session_dir(&session).join("local-relay.json"))
            .unwrap();
    let mut stream = tokio::net::UnixStream::connect(&descriptor.socket)
        .await
        .unwrap();
    let hello = serde_json::to_vec(&Hello {
        version: wire::VERSION,
        capability: descriptor.capability,
        server: "codesense".into(),
    })
    .unwrap();
    stream.write_u32(hello.len() as u32).await.unwrap();
    stream.write_all(&hello).await.unwrap();
    assert_eq!(stream.read_u8().await.unwrap(), 1);
    let mut stream = BufReader::new(stream);
    for (id, method, params) in [
        (1, "tools/list", json!({})),
        (
            2,
            "tools/call",
            json!({"name":"notes_list", "arguments":{}}),
        ),
        (3, "tools/call", json!({"name":"who_am_i", "arguments":{}})),
        (
            4,
            "tools/call",
            json!({"name":"codesense_call_tool", "arguments":{"name":" notes_list "}}),
        ),
        (
            5,
            "tools/call",
            json!({"name":"codesense_call_tool", "arguments":{"name":" who_am_i "}}),
        ),
    ] {
        let request = json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params});
        stream
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
        match id {
            1 => assert_eq!(
                reply["result"]["tools"],
                json!([{"name":"who_am_i", "inputSchema":{"type":"object"}}])
            ),
            2 | 4 => assert_eq!(reply["error"]["code"], -32602),
            _ => assert_eq!(reply["result"]["content"][0]["text"], "allowed"),
        }
    }
    assert!(
        !marker.exists(),
        "excluded call must never reach the bridge"
    );
}

#[test]
fn instance_launch_settings_sweep_isolates_two_homes_with_shared_tmpdir() {
    let mut home = crate::routines::test_support::TempHome::new();
    let tmp = home.isolate_tmpdir();
    let other = home.other_mac("other", 2);
    let cwd = home.root().join("workspace");
    let a = write_settings_file(
        &launch_settings_root().unwrap(),
        &cwd,
        "instance-a",
        &serde_json::json!({}),
        "--settings",
    )
    .unwrap();
    let old = SystemTime::now() - SETTINGS_STALE_AGE - Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(&a.path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    home.act_as(&other);
    assert_eq!(std::env::temp_dir(), tmp);
    let b = write_settings_file(
        &launch_settings_root().unwrap(),
        &cwd,
        "instance-b",
        &serde_json::json!({}),
        "--settings",
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&b.path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    sweep_stale_launch_settings(|_| Some(Default::default()));
    assert!(
        a.path.exists(),
        "home B must not reap home A's live launch settings"
    );
    assert!(
        !b.path.exists(),
        "home B still reaps its own stale launch settings"
    );
    let b_live = write_settings_file(
        &launch_settings_root().unwrap(),
        &cwd,
        "instance-b-live",
        &serde_json::json!({}),
        "--settings",
    )
    .unwrap();
    std::fs::File::options()
        .write(true)
        .open(&b_live.path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    home.act_as(home.root());
    assert!(a.path.starts_with(home.codesense_home()));
    sweep_stale_launch_settings(|_| Some(HashSet::from(["instance-a".into()])));
    assert!(a.path.exists());
    sweep_stale_launch_settings(|_| Some(Default::default()));
    assert!(!a.path.exists());
    assert!(
        b_live.path.exists(),
        "home A must not reap home B's live launch settings"
    );
}

#[test]
fn instance_claude_launch_from_home_keeps_settings_in_private_state() {
    let home = crate::routines::test_support::TempHome::new();
    let state = home.codesense_home();
    team_content(&state, &alice());
    let key = save(
        &state,
        &alice(),
        claude_with("home-cwd", ClaudePermissions::default()),
    );
    let launch = prep(&state, &alice(), &key, PrepareOptions::default());
    for cwd in [home.root(), home.root().parent().unwrap()] {
        let applied = apply_generated(&launch, "home-cwd", cwd, true).unwrap();
        let settings = applied.settings.as_ref().unwrap();
        assert_eq!(
            settings.path.parent().unwrap(),
            state.join("launch-settings")
        );
        assert!(settings.path.is_file());
    }

    let workspace = home.root().join("workspace");
    let inside = workspace.join("generated-settings");
    assert!(
        write_settings_file(&inside, &workspace, "inside", &json!({}), "--settings")
            .unwrap_err()
            .contains("inside the workspace")
    );
    let settings = launch_settings_root().unwrap();
    for cwd in [&state, &settings] {
        assert!(
            write_settings_file(&settings, cwd, "private-cwd", &json!({}), "--settings")
                .unwrap_err()
                .contains("inside the workspace")
        );
    }
    assert!(write_settings_file(
        &settings,
        &state.join("users/1/orgs/acme/api"),
        "managed-content",
        &json!({}),
        "--settings",
    )
    .is_ok());
}

#[test]
fn a_launch_records_its_cli_version_and_warns_only_outside_the_range() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    team_content(home, &alice());
    let key = save(home, &alice(), draft("builder", AgentCli::Claude));
    let codes = |launch: &AgentLaunch| {
        launch
            .warnings
            .iter()
            .filter(|w| w.code == launch_check::LaunchWarningCode::CliVersionUnqualified)
            .count()
    };

    let mut old = prep(home, &alice(), &key, PrepareOptions::default());
    assert_eq!(old.cli_version, None);
    old.note_cli_version(Some("2.1.140"));
    assert_eq!(old.cli_version.as_deref(), Some("2.1.140"));
    assert_eq!(codes(&old), 1);
    let warning = old.warnings.last().unwrap();
    assert!(warning.message.contains("2.1.140"), "{}", warning.message);

    let mut current = prep(home, &alice(), &key, PrepareOptions::default());
    current.note_cli_version(Some("2.1.141"));
    assert_eq!(current.cli_version.as_deref(), Some("2.1.141"));
    assert_eq!(codes(&current), 0);

    let mut unknown = prep(home, &alice(), &key, PrepareOptions::default());
    unknown.note_cli_version(None);
    assert_eq!(unknown.cli_version, None);
    assert_eq!(codes(&unknown), 0);
}
