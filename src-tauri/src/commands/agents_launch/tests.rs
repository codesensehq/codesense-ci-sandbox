// SPDX-License-Identifier: MIT
//! Seam tests for the frontend's agent launch: agents saved through the real
//! store, prepared for real, their scope and generated files written for real
//! for the session the renderer spawns, and the bind's revision check over the
//! real revision files.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;
use crate::agents::launch::{prepare, prepare_named};
use crate::agents::launch_check::LaunchWarningCode;
use crate::agents::model::{Agent, ClaudePermissions, CodexPermissions, Permissions};
use crate::agents::permissions::claude_generated_allow;
use crate::agents::resolve::tests::{alice, bob, draft, save, team_content, write, ORG, TEAM};
use crate::commands::dashboard_launch::{
    fe_bind_telemetry, record_bound_launch, staged_bind_revision,
};
use crate::commands::deps::ResolvedAgentBinary;
use crate::commands::mcp_scope;
use codesense_pty_core::agent_cli::AgentCli;

/// An agent's own deny rules, then CodeSense's credential-file rules merged after them.
fn with_credential_denies(own: &[&str]) -> serde_json::Value {
    let rules = codesense_pty_core::notify_hook::deny_rules();
    serde_json::json!(own
        .iter()
        .copied()
        .chain(rules.iter().map(String::as_str))
        .collect::<Vec<_>>())
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

fn interactive(prompt: Option<&str>) -> PrepareOptions<'_> {
    PrepareOptions {
        account: None,
        target_team: Some((ORG.to_string(), TEAM.to_string())),
        mode_override: Some(LaunchMode::Interactive),
        prompt,
    }
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn read_json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// A worktree whose `.mcp.json` carries the bridge and one other server.
fn worktree(root: &Path) -> PathBuf {
    let cwd = root.join("worktree");
    write(
        &cwd,
        ".mcp.json",
        br#"{"mcpServers": {"codesense": {"command": "bridge"}, "slack": {"command": "slack-mcp"}}}"#,
    );
    cwd
}

/// Where these tests' runtime CLI resolved.
const CLI: &str = "/opt/bin/agent";

fn session(tag: &str) -> String {
    format!("{tag}-{}", uuid::Uuid::new_v4())
}

/// The path that follows `--settings` in `args`.
fn settings_arg(args: &[String]) -> PathBuf {
    let at = args.iter().position(|a| a == "--settings").unwrap();
    PathBuf::from(&args[at + 1])
}

#[test]
fn a_claude_agent_spawns_with_preparations_args_and_its_own_settings_file() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    team_content(&home, &alice());
    let key = save(
        &home,
        &alice(),
        claude_with(
            "guarded",
            ClaudePermissions {
                deny: vec!["Bash(git push --force:*)".into()],
                ..ClaudePermissions::default()
            },
        ),
    );
    let cwd = worktree(tmp.path());
    let launch = prepare_named(&home, &alice(), "guarded", interactive(None)).unwrap();
    let declared = prepare(&home, &alice(), &key, interactive(None)).unwrap();
    let id = session("pty");

    let (prepared, _creation, settings) =
        apply_for_session(launch, Path::new(CLI), &id, &cwd, None, true).unwrap();
    let file = settings.as_ref().unwrap().path().to_path_buf();

    // The args are exactly what prepare resolved, plus the flag for this launch's file.
    let mut expected = declared.launch.args.clone();
    expected.extend([
        "--settings".to_string(),
        file.to_string_lossy().into_owned(),
    ]);
    assert_eq!(prepared.launch.args, expected);
    assert_eq!(settings_arg(&prepared.launch.args), file);
    assert_eq!(prepared.launch.id, key.to_string());
    assert_eq!(prepared.launch.binary, "claude");
    assert_eq!(prepared.agent_path, CLI);
    assert_eq!(prepared.agent, "acme/api/backend/guarded@1");
    assert_eq!(prepared.manifest_hash, declared.manifest_hash);

    // The file is private, outside the worktree, and carries the agent's rules.
    assert!(!file.starts_with(&cwd));
    assert_eq!(mode_of(&file), 0o600);
    let rules = read_json(&file)["permissions"].clone();
    assert_eq!(
        rules["deny"],
        with_credential_denies(&["Bash(git push --force:*)"])
    );
    assert_eq!(rules["allow"], json!(claude_generated_allow()));
    // Nothing lands in the worktree's shared settings.
    assert!(!cwd.join(".claude/settings.local.json").exists());

    // The MCP scope is projected for this session too.
    assert!(prepared
        .scope
        .extra_args
        .contains(&"--strict-mcp-config".to_string()));

    // The account the spawn resolves is keyed by the agent's runtime alone.
    assert_eq!(prepared.account.runtime, AgentCli::Claude);
    assert_eq!(declared.account_policy.runtime, AgentCli::Claude);
    assert_eq!(prepared.account.override_account_id, None);

    drop(settings);
    assert!(!file.exists());
    mcp_scope::remove_scope_config(&id);
}

#[test]
fn a_codex_agent_keeps_its_home_and_retains_permission_flags() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    team_content(&home, &alice());
    save(
        &home,
        &alice(),
        Agent {
            permissions: Some(Permissions {
                codex: Some(CodexPermissions {
                    approval_policy: Some("on-request".into()),
                    sandbox_mode: Some("read-only".into()),
                    ..CodexPermissions::default()
                }),
                ..Permissions::default()
            }),
            ..draft("triage", AgentCli::Codex)
        },
    );
    let cwd = worktree(tmp.path());
    write(
        &cwd,
        ".codex/config.toml",
        b"[mcp_servers.codesense]\ncommand = \"bridge\"\n",
    );
    let launch = prepare_named(&home, &alice(), "triage", interactive(None)).unwrap();
    let args = launch.launch.args.clone();
    let id = session("pty-codex");

    let fixture = mcp_scope::inventory::Fixture::new();
    let (prepared, creation, settings) = apply_for_session_with_cli(
        launch,
        Path::new(CLI),
        &id,
        &cwd,
        Some(tmp.path()),
        &fixture.launch(&args),
    )
    .unwrap();
    assert!(settings.is_none());
    assert!(creation.is_some());
    assert!(prepared.launch.args.starts_with(&args));
    assert!(prepared
        .launch
        .args
        .contains(&"approval_policy=\"on-request\"".into()));
    assert!(prepared
        .launch
        .args
        .contains(&"sandbox_mode=\"read-only\"".into()));
    assert_eq!(prepared.launch.binary, "codex");
    assert_eq!(prepared.account.runtime, AgentCli::Codex);
    let codex_home = mcp_scope::mcp_scope_session_dir(&id);
    assert!(prepared
        .scope
        .extra_env
        .iter()
        .any(|(k, v)| k == "CODEX_HOME" && Path::new(v) == tmp.path()));
    assert!(!codex_home.join("auth.json").exists());
    assert!(!cwd.join(".claude").exists());
    mcp_scope::remove_scope_config(&id);
}

/// Two Claude agents opened from the app into one folder each get their own
/// file. Each is staged for its session, moved onto that session's entry at
/// bind, and goes with the session; a session that ends unbound drops its file.
#[test]
fn two_claude_agents_in_one_folder_get_disjoint_files_that_live_with_their_sessions() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    team_content(&home, &alice());
    save(
        &home,
        &alice(),
        claude_with(
            "a",
            ClaudePermissions {
                deny: vec!["Bash(rm:*)".into()],
                ..ClaudePermissions::default()
            },
        ),
    );
    save(
        &home,
        &alice(),
        claude_with(
            "b",
            ClaudePermissions {
                allow: vec!["Bash(npm test:*)".into()],
                ..ClaudePermissions::default()
            },
        ),
    );
    let cwd = worktree(tmp.path());
    let registry = DashboardLaunchState::new(4);

    let mut launched = Vec::new();
    for (handle, agent) in [("story-a", "a"), ("story-b", "b")] {
        let launch = prepare_named(&home, &alice(), agent, interactive(None)).unwrap();
        let id = session(handle);
        let (prepared, _, settings) =
            apply_for_session(launch, Path::new(CLI), &id, &cwd, None, true).unwrap();
        let file = settings_arg(&prepared.launch.args);
        registry.stage_agent_launch(
            &id,
            StagedAgentLaunch {
                queue_role: None,
                agent_revision: prepared.agent.clone(),
                settings,
                capture_raw_terminal: false,
                cwd: None,
                binary: None,
                facts: None,
            },
        );
        assert!(registry.begin(handle).unwrap());
        // The bind records the prepared revision over whatever the renderer names, then
        // moves the staged file onto the session's entry once the session is bound.
        let revision = staged_bind_revision(&registry, &id, Some("acme/api/backend/forged@9"));
        assert_eq!(revision.as_deref(), Some(prepared.agent.as_str()));
        registry.bind(handle, &id, Some("doc.md")).unwrap();
        record_bound_launch(&registry, handle, &id, true, None, None, revision);
        assert!(registry.take_staged_agent_launch(&id).is_none());
        let row = registry
            .snapshot()
            .into_iter()
            .find(|row| row.handle == handle)
            .unwrap();
        assert_eq!(row.agent_revision.as_deref(), Some(prepared.agent.as_str()));
        mcp_scope::remove_scope_config(&id);
        launched.push((file, id));
    }
    let (a_file, a_session) = &launched[0];
    let (b_file, _) = &launched[1];
    assert_ne!(a_file, b_file);
    let (a, b) = (
        read_json(a_file)["permissions"].clone(),
        read_json(b_file)["permissions"].clone(),
    );
    assert_eq!(a["deny"], with_credential_denies(&["Bash(rm:*)"]));
    assert!(!a["allow"]
        .as_array()
        .unwrap()
        .contains(&json!("Bash(npm test:*)")));
    assert_eq!(b["deny"], with_credential_denies(&[]));
    assert!(b["allow"]
        .as_array()
        .unwrap()
        .contains(&json!("Bash(npm test:*)")));
    assert!(!cwd.join(".claude/settings.local.json").exists());

    // A's session ends: its entry, and so its file, goes; B's stays.
    registry.release_for_session(a_session);
    assert!(!a_file.exists());
    assert!(b_file.exists());

    // A launch whose terminal ends before it binds takes its file with it.
    let launch = prepare_named(&home, &alice(), "a", interactive(None)).unwrap();
    let id = session("unbound");
    let (prepared, _, settings) =
        apply_for_session(launch, Path::new(CLI), &id, &cwd, None, true).unwrap();
    let file = settings_arg(&prepared.launch.args);
    registry.stage_agent_launch(
        &id,
        StagedAgentLaunch {
            queue_role: None,
            agent_revision: prepared.agent.clone(),
            settings,
            capture_raw_terminal: false,
            cwd: None,
            binary: None,
            facts: None,
        },
    );
    assert!(file.exists());
    registry.release_for_session(&id);
    assert!(!file.exists());
    mcp_scope::remove_scope_config(&id);
}

#[test]
fn the_prompt_a_launch_brings_is_checked_and_warned_never_refused() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    team_content(&home, &alice());
    save(&home, &alice(), draft("builder", AgentCli::Claude));
    let cwd = worktree(tmp.path());
    let launch = prepare_named(
        &home,
        &alice(),
        "builder",
        interactive(Some("/triage-slack now")),
    )
    .unwrap();
    let id = session("warned");

    let (prepared, _, _settings) =
        apply_for_session(launch, Path::new(CLI), &id, &cwd, None, true).unwrap();
    assert_eq!(prepared.warnings.len(), 1);
    assert_eq!(
        prepared.warnings[0].code,
        LaunchWarningCode::CommandNotInManifest
    );
    assert!(prepared.warnings[0].message.contains("/triage-slack"));
    let wire = serde_json::to_value(&prepared).unwrap();
    assert_eq!(
        wire["warnings"][0]["code"],
        json!("command_not_in_manifest")
    );
    assert_eq!(wire["agent"], json!("acme/api/backend/builder@1"));
    assert_eq!(wire["agentPath"], json!(CLI));
    assert!(wire["scope"]["extraArgs"].is_array());
    assert!(wire["launch"]["args"].is_array());
    mcp_scope::remove_scope_config(&id);
}

/// The bind records a revision only when it exists and resolves for the
/// signed-in account: a forged revision, key or label, or another account's
/// personal agent, is dropped.
#[test]
fn the_bind_records_only_a_real_revision_the_account_can_resolve() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    team_content(&home, &alice());
    let key = save(&home, &alice(), draft("builder", AgentCli::Claude));
    let label = format!("{key}@1");

    assert_eq!(
        verified_agent_revision(&home, &alice(), &label).unwrap(),
        label
    );
    assert_eq!(
        agent_revision_for_bind(&home, &alice(), &label),
        Some(label.clone())
    );
    for forged in [
        format!("{key}@7"),
        "acme/api/backend/nobody@1".to_string(),
        format!("{key}"),
        format!("{key}@one"),
        "not a key@1".to_string(),
    ] {
        assert_eq!(
            agent_revision_for_bind(&home, &alice(), &forged),
            None,
            "{forged}"
        );
    }

    // Alice's personal agent never resolves for Bob.
    let personal = crate::agents::service::save(
        &home,
        &alice(),
        crate::agents::SaveTarget::Create(crate::agents::Placement::Personal(
            crate::routines::PersonalOwner {
                github_user_id: 1,
                username: "alice".into(),
                display_name: "alice".into(),
                avatar_url: None,
                org: "acme/api".into(),
            },
        )),
        draft("mine", AgentCli::Claude),
        &crate::routines::model::Author::user(alice().as_str()),
    )
    .unwrap()
    .key;
    let personal_label = format!("{personal}@1");
    assert!(verified_agent_revision(&home, &alice(), &personal_label).is_ok());
    assert_eq!(
        agent_revision_for_bind(&home, &bob(), &personal_label),
        None
    );
}

/// A Codex agent's sandbox and approval keys ride its launch recipe as config overrides,
/// so a relaunch of that recipe, which projects the session's scope afresh, runs under its
/// declared rules, not the CLI's defaults: nothing in the scope has to be written again.
#[test]
fn a_relaunched_codex_agent_keeps_its_keys_in_its_recipe() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    team_content(&home, &alice());
    save(
        &home,
        &alice(),
        Agent {
            permissions: Some(Permissions {
                codex: Some(CodexPermissions {
                    approval_policy: Some("on-request".into()),
                    sandbox_mode: Some("read-only".into()),
                    ..CodexPermissions::default()
                }),
                ..Permissions::default()
            }),
            ..draft("triage", AgentCli::Codex)
        },
    );
    let cwd = worktree(tmp.path());
    write(
        &cwd,
        ".codex/config.toml",
        b"[mcp_servers.codesense]\ncommand = \"bridge\"\n",
    );
    let launch = prepare_named(&home, &alice(), "triage", interactive(None)).unwrap();
    let id = session("relaunch-codex");
    let fixture = mcp_scope::inventory::Fixture::new();
    let original_args = launch.launch.args.clone();
    let (prepared, _, _) = apply_for_session_with_cli(
        launch,
        Path::new(CLI),
        &id,
        &cwd,
        Some(tmp.path()),
        &fixture.launch(&original_args),
    )
    .unwrap();
    let recipe = prepared.launch.args.clone();
    assert!(recipe.contains(&"sandbox_mode=\"read-only\"".into()));
    assert!(recipe.contains(&"approval_policy=\"on-request\"".into()));
    mcp_scope::remove_scope_config(&id);
    let descriptor = codesense_pty_core::agent_cli::descriptor(AgentCli::Codex);
    let (reprojected, _) = mcp_scope::project_scope_for_cli(
        &id,
        &cwd,
        &[],
        None,
        descriptor.mcp_scope.as_ref(),
        Some(tmp.path()),
        None,
        Some(&fixture.launch(&recipe)),
        str::to_string,
    )
    .unwrap();
    assert!(reprojected
        .extra_env
        .iter()
        .any(|(k, v)| k == "CODEX_HOME" && Path::new(v) == tmp.path()));
    assert!(!mcp_scope::mcp_scope_session_dir(&id)
        .join("auth.json")
        .exists());
    // The saved launch recipe still supplies the agent's explicit permissions.
    assert_eq!(prepared.launch.args, recipe);
    mcp_scope::remove_scope_config(&id);
}

/// The agent CLI binary an agent launch resolved is staged with it, recorded on the bind's
/// telemetry, and gone once the bind takes the staged launch.
#[test]
fn a_staged_binary_reaches_the_bind_telemetry_and_is_taken_with_the_launch() {
    let registry = DashboardLaunchState::new(2);
    let binary = ResolvedAgentBinary {
        path: PathBuf::from("/tmp/a/claude"),
        version: Some("2.1.296".into()),
    };
    registry.stage_agent_launch(
        "s1",
        StagedAgentLaunch {
            queue_role: None,
            agent_revision: "acme/api/backend/triage@1".into(),
            settings: None,
            capture_raw_terminal: false,
            cwd: None,
            binary: Some(binary.clone()),
            facts: None,
        },
    );
    let staged = registry.staged_agent_binary("s1");
    assert_eq!(staged, Some(binary));

    let record = fe_bind_telemetry(
        "s1".into(),
        "acme/api/backend/triage".into(),
        staged.as_ref(),
        None,
    );
    assert_eq!(record.binary_path.as_deref(), Some("/tmp/a/claude"));
    assert_eq!(record.binary_version.as_deref(), Some("2.1.296"));

    // A plain shell bind with nothing staged carries no binary.
    let plain = fe_bind_telemetry("s2".into(), "shell".into(), None, None);
    assert!(plain.binary_path.is_none() && plain.binary_version.is_none());

    assert!(registry.begin("h").unwrap());
    registry.bind("h", "s1", None).unwrap();
    let revision = staged_bind_revision(&registry, "s1", None);
    record_bound_launch(&registry, "h", "s1", true, None, None, revision);
    assert_eq!(registry.staged_agent_binary("s1"), None);
    let rebound = fe_bind_telemetry("s1".into(), "shell".into(), None, Some(record.clone()));
    assert_eq!(rebound, record, "a rebind is not another agent launch");
    let new_session = fe_bind_telemetry("s2".into(), "shell".into(), None, Some(record));
    assert!(
        new_session.binary_path.is_none(),
        "another session cannot inherit provenance"
    );
}

/// A fresh session bound to a reused handle starts clean even when its bind says nothing
/// about where it ran; a re-bind of the same session keeps what it was launched with.
#[test]
fn a_fresh_bind_clears_the_superseded_sessions_stamps_and_a_rebind_keeps_them() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("held.json");
    std::fs::write(&file, "{}").unwrap();
    let registry = DashboardLaunchState::new(2);
    registry.stage_agent_launch(
        "s1",
        StagedAgentLaunch {
            queue_role: None,
            agent_revision: "acme/api/backend/triage@1".into(),
            settings: Some(launch::LaunchSettingsFile::adopt(file.clone())),
            capture_raw_terminal: false,
            cwd: None,
            binary: None,
            facts: None,
        },
    );
    assert!(registry.begin("h").unwrap());
    registry.bind("h", "s1", None).unwrap();
    let revision = staged_bind_revision(&registry, "s1", None);
    record_bound_launch(
        &registry,
        "h",
        "s1",
        true,
        Some("1".into()),
        Some("acme:backend"),
        revision,
    );
    let row = || registry.snapshot().remove(0);
    assert_eq!(
        row().agent_revision.as_deref(),
        Some("acme/api/backend/triage@1")
    );
    assert!(registry.owner_for_session("s1").is_some());

    // The same session re-bound (a focus) with nothing to say keeps every stamp and the file.
    registry.bind("h", "s1", None).unwrap();
    record_bound_launch(&registry, "h", "s1", false, None, None, None);
    assert_eq!(
        row().agent_revision.as_deref(),
        Some("acme/api/backend/triage@1")
    );
    assert!(file.exists());

    // A plain session replaces it on the handle, its bind naming nothing: no stamp survives.
    registry.bind("h", "s2", None).unwrap();
    record_bound_launch(&registry, "h", "s2", true, None, None, None);
    assert_eq!(row().agent_revision, None);
    assert_eq!(registry.profile_id_for_session("s2"), None);
    assert!(registry.owner_for_session("s2").is_none());
    assert!(!file.exists());
}

/// An agent launch the user consented to capture keeps a raw snapshot of its session; a
/// re-bind of it keeps the consent, and a plain session replacing it on the handle does not.
#[test]
fn a_consented_agent_launch_captures_its_raw_terminal_and_a_plain_successor_does_not() {
    let registry = DashboardLaunchState::new(2);
    registry.stage_agent_launch(
        "s1",
        StagedAgentLaunch {
            queue_role: None,
            agent_revision: "acme/api/backend/triage@1".into(),
            settings: None,
            capture_raw_terminal: true,
            cwd: None,
            binary: None,
            facts: None,
        },
    );
    assert!(registry.begin("h").unwrap());
    registry.bind("h", "s1", None).unwrap();
    record_bound_launch(&registry, "h", "s1", true, None, None, None);
    assert!(registry.raw_capture_enabled_for_session("s1"));

    registry.bind("h", "s1", None).unwrap();
    record_bound_launch(&registry, "h", "s1", false, None, None, None);
    assert!(registry.raw_capture_enabled_for_session("s1"));

    registry.bind("h", "s2", None).unwrap();
    record_bound_launch(&registry, "h", "s2", true, None, None, None);
    assert!(!registry.raw_capture_enabled_for_session("s2"));
}

/// A terminal torn down before its bind (its launch line never ran, so the launcher killed
/// it) takes its staged launch with it through the host's teardown.
#[test]
fn a_session_removed_before_its_bind_drops_its_staged_launch() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("staged.json");
    std::fs::write(&file, "{}").unwrap();
    let registry = DashboardLaunchState::new(2);
    registry.stage_agent_launch(
        "unbound",
        StagedAgentLaunch {
            queue_role: None,
            agent_revision: "acme/api/backend/triage@1".into(),
            settings: Some(launch::LaunchSettingsFile::adopt(file.clone())),
            capture_raw_terminal: false,
            cwd: None,
            binary: None,
            facts: None,
        },
    );
    let claim = {
        use crate::commands::machines::{
            RuntimeCounter, RuntimeMemberAddress, RuntimeMemberSlot, RuntimeMemberTarget,
            RuntimeNamespace,
        };
        crate::commands::pty_host::LocalEffectOwners::default()
            .reserve(
                RuntimeMemberTarget {
                    member: RuntimeMemberAddress::local(RuntimeMemberSlot::InProcess),
                    admission_revision: RuntimeCounter::new(1),
                    namespace: RuntimeNamespace {
                        server_id: "server".into(),
                        epoch: "epoch".into(),
                        generation: RuntimeCounter::new(1),
                    },
                },
                "unbound",
            )
            .unwrap()
    };
    assert!(registry.release_owned(&claim).is_none());
    assert!(registry.staged_agent_revision("unbound").is_none());
    assert!(!file.exists());
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = crate::test_support::strip_attribution_env(&mut std::process::Command::new("git"))
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

/// An agent started in the team's content folder of a git-synced context repo
/// writes nothing there: the bridge, its permissions and the status hooks
/// all ride per-session files outside the repo, so teammates never see a change.
#[test]
fn a_launch_into_a_context_content_folder_leaves_the_repo_clean() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    team_content(&home, &alice());
    let cwd = crate::agents::resolve::tests::content(&home, &alice(), TEAM);
    let repo = cwd.parent().unwrap().to_path_buf();
    // A content folder with no project MCP config of its own.
    let _ = std::fs::remove_file(cwd.join(".mcp.json"));
    git(&repo, &["init", "-q"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "seed"]);

    for (reference, account_home) in [("claude", None), ("codex", Some(tmp.path()))] {
        let launch = prepare_named(&home, &alice(), reference, interactive(None)).unwrap();
        let id = session("ctl");
        // A stand-in Codex CLI and bridge, so the launch never depends on this
        // machine's installed CodeSense CLI.
        let args = launch.launch.args.clone();
        let fixture = mcp_scope::inventory::Fixture::new();
        let (prepared, _creation, settings) = apply_for_session_with_cli(
            launch,
            Path::new(CLI),
            &id,
            &cwd,
            account_home,
            &fixture.launch(&args),
        )
        .unwrap();

        assert_eq!(git(&repo, &["status", "--porcelain", "--ignored"]), "");

        if let Some(file) = settings.as_ref().map(|s| s.path().to_path_buf()) {
            // Claude: the scoped MCP config carries the bridge, and the launch's own
            // settings file carries the status hooks the folder would otherwise hold.
            assert!(!file.starts_with(&repo));
            let at = prepared
                .scope
                .extra_args
                .iter()
                .position(|a| a == "--mcp-config")
                .unwrap();
            let scoped = PathBuf::from(&prepared.scope.extra_args[at + 1]);
            assert!(!scoped.starts_with(&repo));
            assert!(read_json(&scoped)["mcpServers"]
                .get(crate::commands::mcp_scope::BRIDGE_SERVER_KEY)
                .is_some());
            let hooks = read_json(&file)["hooks"].clone();
            assert!(
                hooks["Stop"].to_string().contains("codesense-notify"),
                "{hooks}"
            );
        }
        drop(settings);
        mcp_scope::remove_scope_config(&id);
    }
}

#[tokio::test]
async fn product_launch_preparation_gates_scope_and_returns_canonical_spawn_target() {
    let _home_env = crate::routines::test_support::TempHome::new();
    use crate::agents::product_bootstrap::{tests::fixture, BootstrapResources};
    use crate::agents::resolve::tests::{alice, content, write, TEAM};
    let (home, root, launch, _) = fixture();
    let resources = || {
        Some(BootstrapResources {
            sink: std::sync::Arc::new(crate::commands::worktree::NullEventSink),
        })
    };
    let session = format!("product-prelaunch-{}", uuid::Uuid::new_v4());
    write(
        &content(home.path(), &alice(), TEAM),
        "CLAUDE.md",
        b"changed after resolution",
    );
    let failed = prepare_product_session(
        launch.clone(),
        "/fake/claude".into(),
        session.clone(),
        root.path().to_string_lossy().into_owned(),
        None,
        resources(),
        true,
    )
    .await;
    assert!(failed
        .unwrap_err()
        .contains("product startup preparation failed"));
    assert!(!crate::commands::mcp_scope::mcp_scope_session_dir(&session).exists());
    assert!(!root.path().join("CLAUDE.md").exists());
    write(
        &content(home.path(), &alice(), TEAM),
        "CLAUDE.md",
        b"# Team\n",
    );
    let (prepared, _scope, _settings) = prepare_product_session(
        launch,
        "/fake/claude".into(),
        session.clone(),
        root.path().to_string_lossy().into_owned(),
        None,
        resources(),
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        prepared.cwd.as_deref(),
        root.path().canonicalize().unwrap().to_str()
    );
    assert!(prepared.launch.args.iter().any(|arg| arg == "--settings"));
    crate::commands::mcp_scope::remove_scope_config(&session);
}

/// The MCP servers a projected launch reaches: the keys in its CLI-native scope.
fn reached_servers(runtime: AgentCli, scope: &mcp_scope::ScopeOutcome) -> Vec<String> {
    match runtime {
        AgentCli::Claude => {
            let at = scope
                .extra_args
                .iter()
                .position(|a| a == "--mcp-config")
                .expect("a scoped Claude launch passes --mcp-config");
            let config = read_json(Path::new(&scope.extra_args[at + 1]));
            config["mcpServers"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect()
        }
        AgentCli::Codex => scope.server_aliases.keys().cloned().collect(),
        AgentCli::OpenCode => scope
            .extra_env
            .iter()
            .find(|(key, _)| key == "OPENCODE_CONFIG_CONTENT")
            .and_then(|(_, value)| serde_json::from_str::<serde_json::Value>(value).ok())
            .and_then(|config| config["mcp"].as_object().cloned())
            .map(|servers| servers.keys().cloned().collect())
            .unwrap_or_default(),
        AgentCli::Hermes => {
            let home = scope
                .extra_env
                .iter()
                .find(|(key, _)| key == "HERMES_HOME")
                .unwrap();
            let config: serde_json::Value = serde_yaml::from_slice(
                &std::fs::read(Path::new(&home.1).join("config.yaml")).unwrap(),
            )
            .unwrap();
            config["mcp_servers"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect()
        }
    }
}

/// Every agent launch, for both runtimes, interactive and headless, gets the servers its
/// agent selects plus the CodeSense bridge, never a team server it did not select, and
/// Claude gets the per-launch settings file carrying the notify hooks. Covers a team
/// agent that selects `slack` and the built-in starters, unattended twins included.
#[test]
fn every_agent_launch_reaches_its_selected_servers_the_bridge_and_notify() {
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    team_content(&home, &alice());
    let cwd = tmp.path().join("worktree");
    write(
        &cwd,
        ".mcp.json",
        br#"{"mcpServers": {"codesense": {"command": "bridge"}, "slack": {"command": "slack-mcp"}, "jira": {"command": "jira-mcp"}}}"#,
    );
    write(
        &cwd,
        ".codex/config.toml",
        b"[mcp_servers.codesense]\ncommand = \"bridge\"\n[mcp_servers.slack]\ncommand = \"slack-mcp\"\n[mcp_servers.jira]\ncommand = \"jira-mcp\"\n",
    );
    let team_key = |id: &str, runtime| save(&home, &alice(), draft(id, runtime));
    let builtin = crate::agents::builtin::key;
    let unattended = crate::agents::builtin::unattended_key;
    let cases: Vec<(AgentKey, AgentCli, &[&str])> = vec![
        (
            team_key("claude-dev", AgentCli::Claude),
            AgentCli::Claude,
            &["codesense", "slack"],
        ),
        (
            team_key("codex-dev", AgentCli::Codex),
            AgentCli::Codex,
            &["codesense", "slack"],
        ),
        (builtin(AgentCli::Claude), AgentCli::Claude, &["codesense"]),
        (builtin(AgentCli::Codex), AgentCli::Codex, &["codesense"]),
        (
            unattended(AgentCli::Claude),
            AgentCli::Claude,
            &["codesense"],
        ),
        (unattended(AgentCli::Codex), AgentCli::Codex, &["codesense"]),
    ];
    let fixture = mcp_scope::inventory::Fixture::new();
    for (key, runtime, expected) in cases {
        for mode in [LaunchMode::Interactive, LaunchMode::Headless] {
            let label = format!("{key} {mode:?}");
            let launch = prepare(
                &home,
                &alice(),
                &key,
                PrepareOptions {
                    mode_override: Some(mode),
                    ..interactive(None)
                },
            )
            .unwrap();
            assert_eq!(launch.runtime, runtime, "{label}");
            let id = session("reach");
            let args = launch.launch.args.clone();
            let cli = fixture.launch(&args);
            // Interactive and headless launches both provision through this one seam.
            let (scope, _creation, applied) = launch::project_and_apply_for_cli(
                &launch,
                &id,
                &cwd,
                Some(tmp.path()),
                Some(&cli),
                str::to_string,
            )
            .unwrap();
            let mut reached = reached_servers(runtime, &scope);
            reached.sort();
            assert_eq!(reached, expected.to_vec(), "{label}");
            match runtime {
                AgentCli::Claude => {
                    let settings = read_json(applied.settings.as_ref().unwrap().path());
                    for ev in crate::agent_state::status_hooks().map(|hook| hook.event) {
                        assert!(
                            settings["hooks"][ev]
                                .to_string()
                                .contains("codesense-notify"),
                            "{label}: {ev} hook"
                        );
                    }
                }
                AgentCli::Codex => {
                    // Codex reports attention over the bridge it always reaches.
                    assert!(applied.settings.is_none(), "{label}");
                }
                AgentCli::OpenCode => {
                    assert!(applied.settings.is_none(), "{label}");
                    assert!(applied
                        .extra_env
                        .iter()
                        .any(|(key, _)| key == "OPENCODE_CONFIG_CONTENT"));
                }
                AgentCli::Hermes => assert!(applied.settings.is_none(), "{label}"),
            }
            drop(applied);
            mcp_scope::remove_scope_config(&id);
        }
    }
}

/// Every file under `dir`, by path relative to it, with its contents.
fn files_under(dir: &Path) -> serde_json::Map<String, serde_json::Value> {
    let mut files = serde_json::Map::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
                files.insert(rel, serde_json::Value::String(text));
            }
        }
    }
    files
}

/// The full launch of each runtime, interactive and headless, with and without an MCP
/// allowlist, and of every shipped starter, matches the recorded golden: the adapter's
/// recipe, the resolved launch, the argv and env each spawn site runs with, and every file
/// the launch writes. Machine paths and per-launch ids are
/// replaced by stable tokens. Set `UPDATE_ADAPTER_LAUNCH_GOLDEN=1` to re-record.
#[test]
fn builtin_adapter_launches_match_the_recorded_golden() {
    use crate::agents::model::McpAccess;
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;

    // Other tests repoint HOME, which moves the installed bridge path mid-launch.
    // The XDG_CONFIG_HOME the launch records is ambient and machine-specific; pin it
    // under the temp home so the golden is deterministic whether or not the runner
    // sets one.
    let mut managed_fixture = crate::agents::adapter::ManagedHomeFixture::new();
    managed_fixture.isolate_xdg_config_home();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    team_content(&home, &alice());
    let cwd = tmp.path().join("worktree");
    write(
        &cwd,
        ".mcp.json",
        br#"{"mcpServers": {"codesense": {"command": "bridge"}, "slack": {"command": "slack-mcp"}, "jira": {"command": "jira-mcp"}}}"#,
    );
    write(
        &cwd,
        ".codex/config.toml",
        b"[mcp_servers.codesense]\ncommand = \"bridge\"\n[mcp_servers.slack]\ncommand = \"slack-mcp\"\n[mcp_servers.jira]\ncommand = \"jira-mcp\"\n",
    );
    let account_home = tmp.path().join("account");
    write(&account_home, "auth.json", br#"{"token": "fixture"}"#);
    let fixture = mcp_scope::inventory::Fixture::new();
    let fixture_dir = fixture.launch(&[]).binary.parent().unwrap().to_path_buf();

    let managed_program = managed_fixture.managed_program();
    write(
        &cwd,
        "config.yaml",
        b"mcp_servers:\n  slack: {command: slack-mcp}\n  jira: {command: jira-mcp}\n",
    );
    write(
        &crate::agents::resolve::tests::content(&home, &alice(), TEAM),
        "config.yaml",
        b"mcp_servers:\n  slack: {command: slack-mcp}\n  jira: {command: jira-mcp}\n",
    );
    let mut launches = Vec::new();
    for runtime in AgentCli::every_cli() {
        for (tag, servers) in [("allowlist", vec!["slack".to_string()]), ("open", vec![])] {
            let id = format!("{runtime}-{tag}");
            let mut definition = Agent {
                mcp: McpAccess {
                    servers,
                    ..McpAccess::default()
                },
                ..draft(&id, runtime)
            };
            if crate::agents::adapter::desktop(runtime)
                .model_endpoint
                .is_some()
            {
                definition.args.clear();
                definition.startup.clear();
                definition.context = Some(Vec::new());
                definition.set_startup_settings(Some(std::collections::BTreeMap::from([("model".into(), json!({"provider":"custom", "base_url":"http://127.0.0.1:11434/v1", "default":"qwen2.5:3b-instruct"}))])));
            }
            if !AgentCli::ALL.contains(&runtime) {
                definition.revision = 1;
            }
            let registered = (!AgentCli::ALL.contains(&runtime)).then_some(definition.clone());
            let key = if registered.is_some() {
                crate::agents::service::save_typed_for_test(
                    &home,
                    &alice(),
                    &definition,
                    &crate::agents::service::Placement::Team {
                        org: ORG.into(),
                        team: TEAM.into(),
                    },
                )
                .unwrap()
            } else {
                save(&home, &alice(), definition)
            };
            launches.push((id, key, registered));
        }
    }
    // Native override facts are recorded from the same admitted production launch.
    for (id, args) in [
        ("hermes-native-alias", vec!["--yo"]),
        ("hermes-native-narrow", vec!["-t", "web"]),
        ("hermes-native-wide", vec!["--toolset=all"]),
        ("hermes-native-reset", vec!["-t", "all", "--toolsets="]),
        ("hermes-native-model-alias", vec!["--model=first", "--mo=second"]),
        ("hermes-native-model-joined", vec!["--model=first", "-msecond"]),
        ("hermes-native-model-grouped", vec!["--model=first", "-vQmsecond"]),
        ("hermes-native-model-extra", vec!["--mo=new"]),
    ] {
        let mut definition = draft(id, AgentCli::Hermes);
        definition.revision = 1;
        definition.args = args.iter().map(|arg| (*arg).into()).collect();
        definition.startup.clear();
        definition.context = Some(Vec::new());
        definition.mcp.servers.clear();
        definition.mcp.bridge_tools = None;
        definition.set_startup_settings(Some(std::collections::BTreeMap::from([("model".into(), json!({"provider":"custom", "base_url":"http://127.0.0.1:11434/v1", "default":"qwen2.5:3b-instruct"}))])));
        let key = crate::agents::service::save_typed_for_test(&home, &alice(), &definition,
            &crate::agents::service::Placement::Team { org: ORG.into(), team: TEAM.into() }).unwrap();
        launches.push((id.into(), key, Some(definition)));
    }
    // The shipped starters too, unattended twins included: they are what a
    // launch or routine that names no agent runs, with the permission flags they carry.
    launches.extend(
        crate::agents::builtin::all()
            .into_iter()
            .map(|(key, _)| (key.to_string(), key, None)),
    );
    // Use the same starter builders for registered conformance fixtures; discovery still uses ALL.
    for runtime in AgentCli::every_cli().filter(|cli| !AgentCli::ALL.contains(cli)) {
        for definition in [
            crate::agents::builtin::definition(runtime),
            crate::agents::builtin::unattended_definition(runtime),
        ] {
            let id = format!("builtin/{}", definition.id);
            let key = crate::agents::service::save_typed_for_test(
                &home,
                &alice(),
                &definition,
                &crate::agents::service::Placement::Team {
                    org: ORG.into(),
                    team: TEAM.into(),
                },
            )
            .unwrap();
            launches.push((id, key, Some(definition)));
        }
    }
    let mut golden = serde_json::Map::new();
    for (id, key, registered) in launches {
        for mode in [LaunchMode::Interactive, LaunchMode::Headless] {
            // Unlisted starters are recorded from the real typed preview. They cannot
            // resolve through the public builtin lookup while discovery is gated.
            let starter_refusal = registered
                .as_ref()
                .filter(|_| id.starts_with("builtin/"))
                .and_then(|definition| {
                    crate::agents::resolve::preview_agent(
                        &home,
                        &alice(),
                        definition,
                        &crate::agents::service::Placement::Team {
                            org: ORG.into(),
                            team: TEAM.into(),
                        },
                        Default::default(),
                    )
                    .unwrap()
                    .launch_admission
                    .reason
                });
            let launch = if let Some(reason) = starter_refusal {
                Err(reason)
            } else if let Some(definition) = &registered {
                crate::agents::launch::prepare_typed_for_test(
                    &home,
                    &alice(),
                    definition.clone(),
                    &crate::agents::service::Placement::Team {
                        org: ORG.into(),
                        team: TEAM.into(),
                    },
                    PrepareOptions {
                        mode_override: Some(mode),
                        ..interactive(None)
                    },
                )
            } else {
                prepare(
                    &home,
                    &alice(),
                    &key,
                    PrepareOptions {
                        mode_override: Some(mode),
                        ..interactive(None)
                    },
                )
            };
            let launch = match launch {
                Ok(launch) => launch,
                Err(blocked) => {
                    let mut definition = crate::agents::builtin::get(&key).unwrap_or_else(|| {
                        crate::agents::service::read_current(&home, &alice(), &key)
                            .expect("registered conformance fixture")
                    });
                    // A starter's revision is the app version; pin it so a release
                    // bump does not drift the golden.
                    if key.is_builtin() {
                        definition.revision = 1;
                    }
                    let resolved = if definition.runtime == AgentCli::Hermes {
                        definition.launch = match mode {
                            LaunchMode::Interactive => crate::agents::model::LaunchMode::Foreground,
                            LaunchMode::Headless => crate::agents::model::LaunchMode::Headless,
                        };
                        crate::agents::resolve::preview_agent(
                            &home,
                            &alice(),
                            &definition,
                            &crate::agents::service::Placement::Team {
                                org: ORG.into(),
                                team: TEAM.into(),
                            },
                            Default::default(),
                        )
                        .unwrap()
                    } else {
                        crate::agents::resolve::resolve_agent(
                            &home,
                            &alice(),
                            &key,
                            Default::default(),
                        )
                        .unwrap()
                    };
                    assert!(!resolved.launch_admission.allowed, "{id}: {blocked}");
                    golden.insert(format!("{id} {mode:?}"), json!({"definition": definition, "launch": resolved.launch, "generated": resolved.manifest.generated, "launch_admission": resolved.launch_admission, "blocked": blocked}));
                    continue;
                }
            };
            let session_id = session("golden");
            let scope_dir = mcp_scope::mcp_scope_session_dir(&session_id);
            let args = launch.launch.args.clone();
            let cli = if launch.runtime == AgentCli::Hermes {
                mcp_scope::LaunchCli::new(&managed_program, &args)
            } else {
                fixture.launch(&args)
            };
            let (scope, _creation, applied) = launch::project_and_apply_for_cli(
                &launch,
                &session_id,
                &cwd,
                Some(&account_home),
                Some(&cli),
                str::to_string,
            )
            .unwrap();
            let settings_path = applied.settings.as_ref().map(|s| s.path().to_path_buf());
            let settings = settings_path.as_deref().map(read_json);
            let mut launch_args = launch.launch.args.clone();
            launch_args.extend(applied.argv(str::to_string));
            let descriptor = crate::agents::adapter::LaunchDescriptor::of(launch.runtime);
            let spawn = match mode {
                LaunchMode::Interactive => {
                    let mut argv = launch_args.clone();
                    if let Some(structured) = launch.launch.structured_interactive.clone() {
                        crate::commands::mcp_workspace::prepare_interactive_capture(
                            structured,
                            true,
                            &mut argv,
                            || "SESSION_ID".to_string(),
                        )
                        .unwrap();
                    }
                    json!({
                        "argv": mcp_scope::compose_args(argv, scope.extra_args.clone()),
                        "env": scope.extra_env,
                        "env_remove": launch.launch.env_suppress,
                    })
                }
                LaunchMode::Headless => {
                    let mut env = scope.extra_env.clone();
                    let mut args = launch_args.clone();
                    tauri::async_runtime::block_on(
                        crate::session_usage::LaunchOwner::new(
                            Some(ORG.to_string()),
                            Some(TEAM.to_string()),
                            launch.launch.id.clone(),
                            session_id.clone(),
                            Some(cwd.to_string_lossy().into_owned()),
                        )
                        .prepare_export(
                            Ok(tmp.path().join("usage.sqlite")),
                            &session_id,
                            descriptor.telemetry.as_ref(),
                            Some(4321),
                            &mut env,
                            &mut args,
                        ),
                    );
                    let request = crate::commands::dashboard_launch::headless_capture_request(
                        PathBuf::from(CLI),
                        descriptor.clone(),
                        args,
                        mcp_scope::ScopeOutcome {
                            execution: scope.execution.clone(),
                            extra_args: scope.extra_args.clone(),
                            extra_env: Vec::new(),
                            environment_binding: None,
                            remove_args: None,
                            server_aliases: scope.server_aliases.clone(),
                        },
                        "the task".to_string(),
                        cwd.clone(),
                        Arc::new(AtomicU32::new(0)),
                        &session_id,
                        Some(4321),
                        env,
                        Vec::new(),
                        launch.launch.env.clone(),
                        None,
                    );
                    json!({
                        "program": request.program,
                        "argv": request.args,
                        "env": request.env,
                        "env_remove": request.env_remove,
                        "stdin": String::from_utf8_lossy(request.stdin.as_deref().unwrap_or_default()),
                    })
                }
            };
            let profile = &launch.launch;
            let adapter = launch.descriptor();
            let runner = &crate::agents::adapter::desktop(launch.runtime).runner;
            golden.insert(
                format!("{id} {mode:?}"),
                json!({
                    "launch": profile,
                    "structured_interactive": profile.structured_interactive.as_ref().map(|s| {
                        json!([s.session_log_glob, s.session_id_argv, s.format])
                    }),
                    "structured_headless": descriptor.structured_headless.as_ref().map(|s| {
                        json!([s.request_argv, s.format])
                    }),
                    "account_policy": [
                        launch.account_policy.runtime,
                        launch.account_policy.requirement,
                    ],
                    "adapter": {
                        "binary": adapter.binary.as_str(),
                        "min_version": adapter.min_version,
                        "add_dir_flag": adapter.add_dir_flag,
                        "runner": [
                            runner.default_model,
                            runner.analyze_args,
                            runner.skill_args,
                        ],
                    },
                    "spawn": spawn,
                    "server_aliases": scope.server_aliases,
                    "settings_file": settings,
                    "scope_files": files_under(&scope_dir),
                }),
            );
            if launch.runtime == AgentCli::Hermes {
                golden.get_mut(&format!("{id} {mode:?}")).unwrap()["interactive_startup"] = json!({
                    "args": crate::commands::mcp_workspace::plan_initial_prompt(launch.runtime, Vec::new(), None).launch_args,
                    "prompt_flag": adapter.prompt_flag,
                });
                golden.get_mut(&format!("{id} {mode:?}")).unwrap()["generated"] =
                    serde_json::to_value(&launch.generated).unwrap();
                golden.get_mut(&format!("{id} {mode:?}")).unwrap()["launch_admission"] =
                    serde_json::to_value(crate::agents::adapter::launch_admission(
                        launch.runtime,
                        &home,
                        &alice(),
                        &launch.manifest,
                        &launch.launch.args,
                        &crate::agents::content_reader::FsContentReader,
                        launch.default_prompt.as_deref(),
                    ))
                    .unwrap();
                let definition = crate::agents::builtin::get(&key).unwrap_or_else(|| {
                    crate::agents::service::read_current(&home, &alice(), &key).unwrap()
                });
                golden.get_mut(&format!("{id} {mode:?}")).unwrap()["definition"] =
                    serde_json::to_value(definition).unwrap();
            }
            drop(applied);
            mcp_scope::remove_scope_config(&session_id);

            // Per-launch paths and ids, longest first so no token splits another.
            let mut tokens: Vec<(String, &str)> = vec![
                (scope_dir.to_string_lossy().into_owned(), "<SCOPE_DIR>"),
                (session_id.clone(), "<SESSION_ID>"),
            ];
            if let Some(path) = &settings_path {
                tokens.push((path.to_string_lossy().into_owned(), "<SETTINGS_FILE>"));
            }
            tokens.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
            let key = format!("{id} {mode:?}");
            let mut text = serde_json::to_string(&golden[&key]).unwrap();
            for (from, to) in &tokens {
                text = text.replace(from.as_str(), to);
            }
            golden.insert(key, serde_json::from_str(&text).unwrap());
        }
    }

    // A launch of the CLI itself rather than a named agent has no allowlist: the bridge
    // rides on the CLI's own servers through the open template (Claude) or the launch-flag
    // override (Codex), beside the per-launch settings file.
    for runtime in AgentCli::every_cli() {
        let session_id = session("golden-profile");
        let scope_dir = mcp_scope::mcp_scope_session_dir(&session_id);
        let cli = if runtime == AgentCli::Hermes {
            mcp_scope::LaunchCli::new(&managed_program, &[])
        } else {
            fixture.launch(&[])
        };
        let outcome = mcp_scope::project_profile_launch_for_cli(
            &session_id,
            &cwd,
            None,
            runtime,
            Some(&account_home),
            mcp_scope::ProfileLaunch {
                headless: false,
                bridge_override: true,
            },
            Some(&cli),
            str::to_string,
        );
        let (outcome, creation) = match outcome {
            Ok(outcome) => outcome,
            Err(blocked) => {
                assert!(crate::agents::adapter::desktop(runtime)
                    .model_endpoint
                    .is_some());
                golden.insert(
                    format!("{runtime}-profile-open Interactive"),
                    json!({"blocked":blocked}),
                );
                continue;
            }
        };
        let entry = json!({
            "spawn": { "argv": outcome.extra_args, "env": outcome.extra_env },
            "scope_files": files_under(&scope_dir),
        });
        drop(creation);
        mcp_scope::remove_scope_config(&session_id);
        let mut tokens: Vec<(String, &str)> = vec![
            (scope_dir.to_string_lossy().into_owned(), "<SCOPE_DIR>"),
            (session_id.clone(), "<SESSION_ID>"),
        ];
        if let Some(bridge) = crate::commands::deps::bridge_symlink_path() {
            tokens.push((bridge.to_string_lossy().into_owned(), "<BRIDGE>"));
        }
        tokens.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        let mut text = serde_json::to_string(&entry).unwrap();
        for (from, to) in &tokens {
            text = text.replace(from.as_str(), to);
        }
        golden.insert(
            format!("{runtime}-profile-open Interactive"),
            serde_json::from_str(&text).unwrap(),
        );
    }

    // This golden covers adapter wiring; registry contents have their own preview tests.
    for entry in golden.values_mut() {
        if let Some(names) = entry.pointer_mut("/launch/effective_bridge_tools") {
            if names.as_array().is_some_and(|names| !names.is_empty()) {
                *names = json!("<REGISTERED_BRIDGE_TOOLS>");
            }
        }
    }
    for (key, row) in &golden {
        if key.contains("hermes") {
            for source in [
                &row["launch"]["args"],
                &row["spawn"]["argv"],
                &row["interactive_startup"]["args"],
            ] {
                for arg in source
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                {
                    assert!(
                        !["--worktree", "--safe-mode", "--ignore-rules", "--api-key"]
                            .iter()
                            .any(|flag| arg == *flag || arg.starts_with(&format!("{flag}="))),
                        "{key}: {arg}"
                    );
                }
            }
        }
    }
    let text = serde_json::to_string_pretty(&serde_json::Value::Object(golden)).unwrap();
    // The installed bridge lives under the home dir, which tests running alongside this
    // one may point elsewhere.
    let mut text = regex::Regex::new(r#"[^"\\\s]*/\.codesense/bin/codesense-bridge"#)
        .unwrap()
        .replace_all(&text, "<BRIDGE>")
        .into_owned();
    // Codex relay aliases, sockets and capabilities are minted per launch.
    for (pattern, token) in [
        (r"_scope_[0-9a-f]{16}", "_scope_<ALIAS>"),
        (r#"[^"\\]*/csm-[A-Za-z0-9]+"#, "<RELAY_DIR>"),
        (r"[0-9a-f]{64}", "<CAPABILITY>"),
    ] {
        text = regex::Regex::new(pattern)
            .unwrap()
            .replace_all(&text, token)
            .into_owned();
    }
    let mut roots: Vec<(PathBuf, &str)> = vec![
        (fixture_dir, "<CLI_DIR>"),
        (tmp.path().to_path_buf(), "<TMP>"),
        (std::env::temp_dir(), "<TEMP_DIR>"),
    ];
    if let Some(instance_root) = crate::commands::paths::codesense_home_dir() {
        roots.push((instance_root, "<INSTANCE_ROOT>"));
    }
    if let Some(user_home) = dirs::home_dir() {
        roots.push((user_home, "<HOME>"));
    }
    // Launches name a path in its raw form or its canonical one (macOS's default temp dir
    // sits under the `/var` -> `/private/var` symlink; Linux has none), so both redact to the
    // same token and the golden reads the same on every machine.
    let mut machine: Vec<(String, &str)> = Vec::new();
    for (root, token) in roots {
        let canonical = root.canonicalize().ok();
        for form in std::iter::once(root).chain(canonical) {
            let form = form.to_string_lossy().trim_end_matches('/').to_string();
            if !form.is_empty() && !machine.iter().any(|(from, _)| *from == form) {
                machine.push((form, token));
            }
        }
    }
    machine.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
    for (from, to) in &machine {
        text = text.replace(from.as_str(), to);
    }
    text.push('\n');

    let golden_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/commands/agents_launch/adapter_launch_golden.json");
    if std::env::var_os("UPDATE_ADAPTER_LAUNCH_GOLDEN").is_some() {
        std::fs::write(&golden_path, &text).unwrap();
    }
    let recorded = std::fs::read_to_string(&golden_path).expect("the recorded golden exists");
    assert_eq!(
        text, recorded,
        "a built-in adapter launch drifted from the golden"
    );
}

#[tokio::test]
async fn frontend_role_startup_returns_task_and_facts_on_argv_without_later_input() {
    let _home_env = crate::routines::test_support::TempHome::new();
    use crate::commands::role_startup::tests::StartupHost;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    team_content(&home, &alice());
    let mut definition = draft("queue-handler", AgentCli::Claude);
    definition.role = Some("reviewer".into());
    definition.default_prompt = "saved task".into();
    let key = save(&home, &alice(), definition);
    let cwd = worktree(tmp.path());
    let id = session("role-startup");
    let launch = prepare(&home, &alice(), &key, interactive(None)).unwrap();
    let (mut prepared, _creation, _settings) =
        apply_for_session(launch, Path::new(CLI), &id, &cwd, None, true).unwrap();
    let host = StartupHost::new(tmp.path().join("inbox.sqlite3"));
    let now = chrono::Utc::now().timestamp();
    crate::weft::role_inbox::send_role_message_commit(
        &host.store(),
        "human",
        "reviewer",
        "pending work",
        None,
        None,
        None,
        "",
        now,
        None,
        None,
        None,
    )
    .unwrap();
    let team = crate::commands::dashboard_launch::LaunchTeam {
        org: ORG.into(),
        team: TEAM.into(),
    };
    prepare_role_startup(
        &host,
        &mut prepared,
        Some(&team),
        &id,
        Some("/review explicit"),
    )
    .await
    .unwrap();
    assert!(prepared.prompt_in_argv);
    assert!(prepared.default_prompt.is_none());
    assert!(prepared.scope.extra_args.is_empty());
    let args = &prepared.launch.args;
    assert_eq!(args[args.len() - 2], "--");
    assert!(args.last().unwrap().starts_with("/review explicit\n\n"));
    assert!(args.last().unwrap().contains("1 messages are pending"));
    let queues =
        crate::weft::role_state::role_state_read(&host.store(), Some("reviewer"), now, 0).unwrap();
    assert!(queues[0]
        .registrations
        .iter()
        .any(|r| r.session_id.as_deref() == Some(&id) && r.live));
    assert_eq!((queues[0].counts.pending, queues[0].counts.claimed), (1, 0));
}

#[tokio::test]
async fn opencode_role_startup_is_prepared_with_its_native_prompt_flag() {
    let _home_env = crate::routines::test_support::TempHome::new();
    use crate::commands::role_startup::tests::StartupHost;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    team_content(&home, &alice());
    let mut definition = draft("opencode-queue-handler", AgentCli::OpenCode);
    definition.role = Some("reviewer".into());
    definition.default_prompt = "saved task".into();
    let key = save(&home, &alice(), definition);
    let cwd = worktree(tmp.path());
    let id = session("opencode-role-startup");
    let launch = prepare(&home, &alice(), &key, interactive(None)).unwrap();
    let (mut prepared, _creation, _settings) =
        apply_for_session(launch, Path::new(CLI), &id, &cwd, None, true).unwrap();
    let host = StartupHost::new(tmp.path().join("inbox.sqlite3"));
    let now = chrono::Utc::now().timestamp();
    crate::weft::role_inbox::send_role_message_commit(
        &host.store(),
        "human",
        "reviewer",
        "pending role work",
        None,
        None,
        None,
        "",
        now,
        None,
        None,
        None,
    )
    .unwrap();
    let team = crate::commands::dashboard_launch::LaunchTeam {
        org: ORG.into(),
        team: TEAM.into(),
    };
    prepare_role_startup(&host, &mut prepared, Some(&team), &id, None)
        .await
        .unwrap();
    assert!(prepared.prompt_in_argv);
    assert_eq!(prepared.default_prompt, None);
    assert_eq!(
        prepared.launch.args[prepared.launch.args.len() - 2],
        "--prompt"
    );
    assert!(prepared
        .launch
        .args
        .last()
        .unwrap()
        .starts_with("saved task\n\n"));
    assert!(prepared
        .launch
        .args
        .last()
        .unwrap()
        .contains("1 messages are pending"));
}

#[tokio::test]
async fn renderer_default_prompt_reaches_both_role_startup_and_task_capture() {
    use crate::commands::role_startup::tests::StartupHost;
    use crate::session_usage::task_capture::{capture_launch_task, Capture, TaskInputs};
    let _home_env = crate::routines::test_support::TempHome::new();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    team_content(&home, &alice());
    let mut definition = draft("default-task", AgentCli::Claude);
    definition.role = Some("reviewer".into());
    definition.default_prompt =
        "Task-Type: review\nSubject: research\nReview the launch evidence.".into();
    let key = save(&home, &alice(), definition);
    let cwd = worktree(tmp.path());
    let id = session("default-task");
    let launch = prepare(&home, &alice(), &key, interactive(None)).unwrap();
    // The renderer stages capture from the resolved agent before product/role preparation.
    let inputs = TaskInputs::from_launch(&launch, None);
    let (mut prepared, _, _) =
        apply_for_session(launch, Path::new(CLI), &id, &cwd, None, true).unwrap();
    let host = StartupHost::new(tmp.path().join("inbox.sqlite3"));
    let team = crate::commands::dashboard_launch::LaunchTeam {
        org: ORG.into(),
        team: TEAM.into(),
    };
    prepare_role_startup(&host, &mut prepared, Some(&team), &id, None)
        .await
        .unwrap();
    assert!(prepared.default_prompt.is_none());
    assert!(prepared
        .launch
        .args
        .last()
        .unwrap()
        .starts_with(inputs.brief.as_deref().unwrap()));
    let store = crate::session_usage::UsageStore::open(&tmp.path().join("usage.sqlite")).unwrap();
    let record = crate::session_usage::record::SessionRecord::new(&id);
    let task = capture_launch_task(
        &store,
        &Capture {
            session_id: &id,
            record: &record,
            inputs: &inputs,
            routine: None,
            launcher_task: None,
            story_file: None,
            now_ms: 1_000,
        },
    )
    .unwrap();
    assert_eq!(task.goal, "Review the launch evidence.");
    assert_eq!(task.task_type, "review");
    assert_eq!(task.subject.as_deref(), Some("research"));
}

#[test]
fn a_saved_agents_pty_launch_exports_usage_for_its_session_before_bind() {
    let _home_env = crate::routines::test_support::TempHome::new();
    for runtime in [AgentCli::Claude, AgentCli::Codex] {
        for (team_key, org, team) in [
            (None, "", None),
            (Some(format!("{ORG}:_none")), ORG, None),
            (Some(format!("{ORG}:{TEAM}")), ORG, Some(TEAM)),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().join("home");
            team_content(&home, &alice());
            let key = save(&home, &alice(), draft("metered", runtime));
            let launch = prepare(&home, &alice(), &key, interactive(None)).unwrap();
            let cwd = worktree(tmp.path());
            let session = session("metered");
            let fixture = mcp_scope::inventory::Fixture::new();
            let args = launch.launch.args.clone();
            let (mut prepared, _creation, _settings) = apply_for_session_with_cli(
                launch,
                Path::new(CLI),
                &session,
                &cwd,
                Some(tmp.path()),
                &fixture.launch(&args),
            )
            .unwrap();
            prepared
                .launch
                .args
                .extend(["--".to_string(), "startup prompt".to_string()]);
            let path = tmp.path().join("usage.sqlite");
            tauri::async_runtime::block_on(register_prepared_usage(
                &mut prepared,
                &session,
                team_key.as_deref(),
                &cwd,
                Some(58919),
                Ok(path.clone()),
            ));
            crate::session_usage::tests::assert_launch_attribution(
                &path,
                &session,
                runtime,
                &prepared.launch.id,
                &session,
                org,
                team,
            );
            let endpoint = crate::http::routes::telemetry::endpoint_for(58919, &session);
            let expected = codesense_pty_core::agent_cli::descriptor(runtime)
                .telemetry
                .as_ref()
                .unwrap()
                .resolved(&endpoint);
            for pair in expected.0 {
                assert!(prepared.scope.extra_env.contains(&pair));
            }
            for arg in expected.1 {
                let index = prepared
                    .launch
                    .args
                    .iter()
                    .position(|value| value == &arg)
                    .unwrap();
                let boundary = prepared
                    .launch
                    .args
                    .iter()
                    .position(|value| value == "--")
                    .unwrap();
                assert!(
                    index < boundary,
                    "telemetry flags precede the startup prompt"
                );
            }
        }
    }
}

pub(crate) mod attribution_entry {
    //! SV-CDE-7: the renderer's launch entry (`apply_for_session`) switches git-ai capture
    //! on for a managed worktree, and a git-ai that is missing, the wrong version or broken
    //! never fails it.
    use super::*;
    use crate::attribution::testing::{assert_unwired, assert_wired, version_script, Launch};
    use crate::attribution::{AttributionStatus, OffReason};

    pub(crate) fn launched(
        fixture: &Launch,
    ) -> (PreparedAgentLaunch, Option<serde_json::Value>, String) {
        let id = session("attr");
        let inventory = mcp_scope::inventory::Fixture::new();
        let args = fixture.launch.launch.args.clone();
        let cli = inventory.launch(&args);
        let (prepared, _creation, settings) = apply_for_session_with_cli(
            fixture.launch.clone(),
            Path::new(CLI),
            &id,
            &fixture.cwd,
            None,
            &cli,
        )
        .expect("the launch itself succeeds");
        let json = settings.as_ref().map(|s| read_json(s.path()));
        (prepared, json, id)
    }

    /// Feed emitted argv through the renderer's intent builder and the session
    /// server's local preparation entry.
    fn host_argv(fixture: &Launch, argv: Vec<String>, env: Vec<(String, String)>) -> Vec<String> {
        use codesense_pty_core::spawn::HostEnvironment;
        let id = session("attr-host");
        let mut intent = crate::commands::pty::build_local_spawn_intent(
            String::new(),
            Some(argv),
            fixture.cwd.to_string_lossy().into_owned(),
            80,
            24,
            Some(false),
            None,
            None,
            None,
            "plain".into(),
            None,
        );
        intent.env = env;
        let environment = HostEnvironment::with_vars(
            None,
            None,
            [
                ("HOME".into(), fixture.home().to_string_lossy().into_owned()),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ],
        );
        let mut spec = intent.resolve(&id, &environment);
        codesense_session_server::host::prepare_direct_launch_for_test(
            &mut spec,
            AgentCli::Codex,
            "/fixture/activity-helper",
        );
        assert_eq!(
            spec.command
                .get_env(codesense_pty_core::activity::RELEASE_QUALIFIED_ENV),
            Some(std::ffi::OsStr::new("1")),
            "fixture must reach qualified host preparation"
        );
        let launch = codesense_pty_core::activity::launch_of(spec.command.get_argv())
            .expect("qualified Codex host emits an engine launch");
        assert_eq!(hook_overrides(&launch.engine), hook_overrides(&launch.standalone),
            "the actual engine argv and standalone restart recipe carry identical hook definitions and trust");
        std::iter::once(launch.program)
            .chain(launch.standalone)
            .chain(launch.operands)
            .collect()
    }

    fn host_prepared(fixture: &Launch, inventory: &mcp_scope::inventory::Fixture) -> Vec<String> {
        let id = session("attr");
        let args = fixture.launch.launch.args.clone();
        let cli = inventory.launch(&args);
        let (prepared, _creation, _settings) = apply_for_session_with_cli(
            fixture.launch.clone(),
            cli.binary,
            &id,
            &fixture.cwd,
            Some(&fixture.home().join("codex-home")),
            &cli,
        )
        .unwrap();
        let argv = std::iter::once(cli.binary.to_string_lossy().into_owned())
            .chain(mcp_scope::compose_args(
                prepared.launch.args,
                prepared.scope.extra_args,
            ))
            .collect();
        let argv = host_argv(fixture, argv, prepared.scope.extra_env);
        mcp_scope::remove_scope_config(&id);
        crate::agents::launch::forget_launch_env(&id);
        argv
    }

    fn hook_overrides(argv: &[String]) -> toml::Table {
        let mut config = toml::Table::new();
        for pair in argv
            .windows(2)
            .filter(|p| matches!(p[0].as_str(), "-c" | "--config"))
        {
            let Some((key, value)) = pair[1].split_once('=') else {
                continue;
            };
            if let Some(key) = key.strip_prefix("hooks.") {
                let parsed: toml::Table = format!("value={value}").parse().unwrap();
                config.insert(key.to_owned(), parsed["value"].clone());
            }
        }
        config
    }

    /// Check every emitted injected group against production's exact fingerprint;
    /// the user's unrelated hook must never acquire a grant.
    fn assert_hook_trust(argv: &[String], checkpoint_command: Option<&str>) -> Vec<String> {
        use codesense_pty_core::agent_cli::{handler_grant_for_test, ATTRIBUTION_HOOK_MARKER};
        let config = hook_overrides(argv);
        let state = config["state"].as_table().unwrap();
        let mut hashes = Vec::new();
        let mut checkpoints = 0;
        for (event, groups) in &config {
            let Some(groups) = groups.as_array() else {
                continue;
            };
            for (position, group) in groups.iter().enumerate() {
                let handler = &group["hooks"][0];
                let command = handler["command"].as_str().unwrap();
                if command == "user-stop" {
                    assert!(!state.contains_key("/<session-flags>/config.toml:subagent_stop:0:0"));
                    continue;
                }
                let (key, hash) = handler_grant_for_test(event, position, handler).unwrap();
                assert_eq!(
                    state
                        .get(&key)
                        .and_then(|v| v.get("trusted_hash"))
                        .and_then(toml::Value::as_str),
                    Some(hash.as_str()),
                    "missing exact trust for {event} group {position}: {command}"
                );
                if command.contains(ATTRIBUTION_HOOK_MARKER) {
                    checkpoints += 1;
                    assert_eq!(Some(command), checkpoint_command);
                    hashes.push(hash);
                }
            }
        }
        assert_eq!(
            checkpoints,
            if checkpoint_command.is_some() { 3 } else { 0 }
        );
        hashes
    }

    fn checkpoint_command(fixture: &Launch, binary: &Path) -> String {
        crate::attribution::managed::Managed::for_codesense_home(fixture.home())
            .checkpoint_command(binary, "codex")
    }

    #[test]
    fn saved_codex_host_preparation_trusts_exact_checkpoint_commands_over_stale_native_state() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let fixture = Launch::new(AgentCli::Codex);
        let _daemon = fixture.daemon_running();
        let inventory = mcp_scope::inventory::Fixture::new();
        let native = fixture.home().join("codex-home/config.toml");
        let original = "# user's native config\nmodel = 'gpt-5'\n\n[hooks.state.\"/<session-flags>/config.toml:pre_tool_use:0:0\"]\ntrusted_hash = 'sha256:stale'\n\n# unrelated trailing section\n[projects.\"/user/repo\"]\ntrust_level = 'trusted'\n";
        write(
            fixture.home(),
            "codex-home/config.toml",
            original.as_bytes(),
        );
        let argv = host_prepared(&fixture, &inventory);
        assert_hook_trust(
            &argv,
            Some(&checkpoint_command(
                &fixture,
                &fixture.home().join("bin/git-ai"),
            )),
        );
        let hooks = hook_overrides(&argv);
        for event in crate::attribution::launch::CODEX_HOOK_EVENTS {
            assert!(
                hooks[event][0]["hooks"][0]["command"]
                    .as_str()
                    .unwrap()
                    .contains(codesense_pty_core::agent_cli::ATTRIBUTION_HOOK_MARKER),
                "initial {event} checkpoint precedes observers"
            );
        }
        let saved = std::fs::read_to_string(native).unwrap();
        for block in original.split("\n# unrelated trailing section") {
            assert!(
                saved.contains(block),
                "original native bytes changed: {block}"
            );
        }
        assert!(saved.contains(
            "# unrelated trailing section\n[projects.\"/user/repo\"]\ntrust_level = 'trusted'\n"
        ));
    }

    #[test]
    fn saved_codex_restart_refreshes_trust_on_off_on_and_binary_path_change() {
        use crate::attribution::prefs::{AttributionMode, AttributionPrefs};
        let _home_env = crate::routines::test_support::TempHome::new();
        let mut fixture = Launch::new(AgentCli::Codex);
        let _daemon = fixture.daemon_running();
        fixture.launch.launch.args.extend([
            "-c".into(),
            r#"hooks.SubagentStop=[{hooks=[{type="command",command="user-stop"}]}]"#.into(),
        ]);
        let inventory = mcp_scope::inventory::Fixture::new();
        let initial = host_prepared(&fixture, &inventory);
        let original_command = checkpoint_command(&fixture, &fixture.home().join("bin/git-ai"));
        let old_hashes = assert_hook_trust(&initial, Some(&original_command));
        let mut argv = initial;
        for mode in [AttributionMode::Off, AttributionMode::On] {
            fixture.set_prefs(AttributionPrefs {
                mode: Some(mode),
                ..Default::default()
            });
            argv = refreshed_host_argv(&fixture, argv);
            assert!(argv.iter().any(|arg| arg.contains("user-stop")));
            assert_hook_trust(
                &argv,
                (mode == AttributionMode::On).then_some(original_command.as_str()),
            );
            if mode == AttributionMode::On {
                let hooks = hook_overrides(&argv);
                for (event, position) in [("PreToolUse", 1), ("PostToolUse", 2), ("Stop", 1)] {
                    assert_eq!(
                        hooks[event][position]["hooks"][0]["command"].as_str(),
                        Some(original_command.as_str()),
                        "restarted {event} checkpoint follows existing injected groups"
                    );
                }
            }
            if mode == AttributionMode::Off {
                assert!(
                    old_hashes
                        .iter()
                        .all(|hash| !argv.iter().any(|arg| arg.contains(hash))),
                    "removed checkpoint hashes must not survive at observer positions"
                );
            }
        }
        // The binary path changes with the CodeSense home its own copy lives in (SV-CDE-40
        // I1): the same home reached through another path names another git-ai.
        let aliases = tempfile::tempdir().unwrap();
        let alias = aliases.path().join("home");
        std::os::unix::fs::symlink(fixture.home(), &alias).unwrap();
        let _alias_sockets = crate::attribution::testing::SocketDirGuard::for_home(&alias);
        let refreshed = refresh_args_at(&alias, &fixture, &argv).unwrap();
        argv = host_argv(&fixture, refreshed, codex_home_env(&fixture));
        let moved_command = crate::attribution::managed::Managed::for_codesense_home(&alias)
            .checkpoint_command(&alias.join("bin/git-ai"), "codex");
        assert_ne!(moved_command, original_command);
        let moved_hashes = assert_hook_trust(&argv, Some(&moved_command));
        assert_ne!(moved_hashes, old_hashes);
        assert!(old_hashes
            .iter()
            .all(|hash| !argv.iter().any(|arg| arg.contains(hash))));
        // An extra row: with no own copy, a git-ai on PATH is never used for capture, so
        // the refresh wires no checkpoint and drops the previous trust.
        fixture.uninstall();
        let alternate = fixture.home().join("alternate/bin/git-ai");
        crate::attribution::testing::script(
            &alternate,
            &version_script(crate::attribution::install::PIN.expected_version),
        );
        let previous_path = std::env::var_os("PATH");
        let next_path = std::env::join_paths(
            std::iter::once(alternate.parent().unwrap().to_owned()).chain(
                previous_path
                    .as_deref()
                    .into_iter()
                    .flat_map(std::env::split_paths),
            ),
        )
        .unwrap();
        std::env::set_var("PATH", next_path);
        let refreshed = refresh_args(&fixture, &argv);
        match previous_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
        argv = host_argv(&fixture, refreshed.unwrap(), codex_home_env(&fixture));
        assert!(argv.iter().any(|arg| arg.contains("user-stop")));
        assert!(assert_hook_trust(&argv, None).is_empty());
        assert!(moved_hashes
            .iter()
            .all(|hash| !argv.iter().any(|arg| arg.contains(hash))));
    }

    fn codex_home_env(fixture: &Launch) -> Vec<(String, String)> {
        vec![(
            "CODEX_HOME".into(),
            fixture
                .home()
                .join("codex-home")
                .to_string_lossy()
                .into_owned(),
        )]
    }

    fn refresh_args(fixture: &Launch, argv: &[String]) -> Result<Vec<String>, String> {
        refresh_args_at(fixture.home(), fixture, argv)
    }

    /// Refresh `argv` as a launch whose CodeSense home is reached at `home`.
    fn refresh_args_at(
        home: &Path,
        fixture: &Launch,
        argv: &[String],
    ) -> Result<Vec<String>, String> {
        let request = crate::attribution::launch::Request {
            codesense_home: home,
            principal: &alice(),
            cwd: &fixture.cwd,
            runtime: AgentCli::Codex,
        };
        let (edit, _) =
            crate::agents::launch::refresh_attribution(&request, "attr-restart", &argv[1..], true)?;
        let edit = edit.unwrap();
        let mut args = argv.to_vec();
        for pair in edit.remove_args.chunks_exact(2) {
            let at = args
                .windows(2)
                .position(|tokens| tokens == pair)
                .expect("recorded override to remove");
            args.drain(at..at + 2);
        }
        args.extend(edit.extra_args);
        Ok(args)
    }

    fn refreshed_host_argv(fixture: &Launch, argv: Vec<String>) -> Vec<String> {
        host_argv(
            fixture,
            refresh_args(fixture, &argv).unwrap(),
            vec![(
                "CODEX_HOME".into(),
                fixture
                    .home()
                    .join("codex-home")
                    .to_string_lossy()
                    .into_owned(),
            )],
        )
    }

    #[test]
    fn claude_and_codex_launch_with_the_hooks_and_trace_env() {
        let _home_env = crate::routines::test_support::TempHome::new();
        for runtime in [AgentCli::Claude, AgentCli::Codex] {
            let fixture = Launch::new(runtime);
            let _daemon = fixture.daemon_running();
            let (prepared, settings, id) = launched(&fixture);
            assert_wired(
                runtime,
                &prepared.launch.args,
                settings.as_ref(),
                &prepared.scope.extra_env,
            );
            assert_eq!(
                fixture.status(),
                Some(AttributionStatus::Active),
                "{runtime:?}"
            );
            mcp_scope::remove_scope_config(&id);
        }
    }

    #[test]
    fn opencode_launches_unwired_and_says_unsupported() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let fixture = Launch::new(AgentCli::OpenCode);
        let _daemon = fixture.daemon_running();
        let (prepared, settings, id) = launched(&fixture);
        assert_unwired(
            &prepared.launch.args,
            settings.as_ref(),
            &prepared.scope.extra_env,
        );
        crate::attribution::testing::assert_reflog_marker(
            &prepared.scope.extra_env,
            Some("opencode"),
        );
        assert_eq!(
            fixture.status(),
            Some(AttributionStatus::Unsupported {
                reason: "opencode".into()
            })
        );
        mcp_scope::remove_scope_config(&id);
    }

    #[test]
    fn every_failure_still_launches_and_the_record_says_why() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let daemon_fails = format!(
            "{}\ncase \"$1\" in bg) echo 'cannot bind' >&2; exit 1;; esac",
            version_script(crate::attribution::install::PIN.expected_version)
        );
        let cases: [(&str, Option<String>, fn(&AttributionStatus) -> bool); 5] = [
            ("not installed", None, |s| {
                *s == AttributionStatus::NotInstalled
            }),
            ("1.7.4", Some(version_script("1.7.4")), |s| {
                *s == AttributionStatus::UnsupportedVersion {
                    found: "1.7.4".into(),
                }
            }),
            ("1.8.0", Some(version_script("1.8.0")), |s| {
                *s == AttributionStatus::Unsupported {
                    reason: "newer".into(),
                }
            }),
            (
                "daemon",
                Some(daemon_fails),
                |s| matches!(s, AttributionStatus::Error { message } if message.contains("cannot bind")),
            ),
            ("broken", Some("exit 9".into()), |s| {
                matches!(s, AttributionStatus::Error { .. })
            }),
        ];
        for runtime in [AgentCli::Claude, AgentCli::Codex] {
            for (label, binary, expected) in &cases {
                let fixture = Launch::new(runtime);
                match binary {
                    Some(body) => fixture.install(body),
                    None => fixture.uninstall(),
                }
                // Removing the fixture binary must not discover a real git-ai
                // installed on this machine's inherited PATH.
                let previous_path = std::env::var_os("PATH");
                if binary.is_none() {
                    std::env::set_var("PATH", "/usr/bin:/bin");
                }
                let launched = launched(&fixture);
                match previous_path {
                    Some(path) => std::env::set_var("PATH", path),
                    None => std::env::remove_var("PATH"),
                }
                let (prepared, settings, id) = launched;
                assert_unwired(
                    &prepared.launch.args,
                    settings.as_ref(),
                    &prepared.scope.extra_env,
                );
                let status = fixture.status().expect(label);
                assert!(expected(&status), "{runtime:?} {label}: {status:?}");
                mcp_scope::remove_scope_config(&id);
            }
        }
    }

    #[test]
    fn a_codex_agent_with_its_own_tool_hooks_still_launches_and_capture_says_why() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let mut fixture = Launch::new(AgentCli::Codex);
        let _daemon = fixture.daemon_running();
        fixture
            .launch
            .launch
            .args
            .extend(["-c".to_string(), "hooks.Stop=[]".to_string()]);
        let (prepared, settings, id) = launched(&fixture);
        assert!(prepared.launch.args.contains(&"hooks.Stop=[]".to_string()));
        assert_unwired(
            &prepared.launch.args[..prepared.launch.args.len()],
            settings.as_ref(),
            &prepared.scope.extra_env,
        );
        let Some(AttributionStatus::Error { message }) = fixture.status() else {
            panic!("{:?}", fixture.status());
        };
        assert!(message.contains("hooks"), "{message}");
        mcp_scope::remove_scope_config(&id);
    }

    #[test]
    fn a_repo_set_off_launches_unwired() {
        let _home_env = crate::routines::test_support::TempHome::new();
        let fixture = Launch::new(AgentCli::Claude);
        fixture.set_prefs(crate::attribution::prefs::AttributionPrefs {
            mode: Some(crate::attribution::prefs::AttributionMode::Off),
            ..Default::default()
        });
        let (prepared, settings, id) = launched(&fixture);
        assert_unwired(
            &prepared.launch.args,
            settings.as_ref(),
            &prepared.scope.extra_env,
        );
        assert_eq!(
            fixture.status(),
            Some(AttributionStatus::Off {
                reason: OffReason::DisabledForRepo
            })
        );
        mcp_scope::remove_scope_config(&id);
    }
}

#[path = "hermes_tests.rs"]
mod hermes_tests;

#[path = "spawn_boundary_tests.rs"]
mod spawn_boundary_tests;

// Exercise the same preparation claim and settlement as StartFlight, in both
// completion orders. A bypassed New must never settle somebody else's claim.
fn assert_attended_start_order(first_new: bool, second_new: bool) {
    use crate::routines::wake::{flight_key, LaunchFlights, LOCAL};
    for first_finished in [false, true] {
        let flights = LaunchFlights::default();
        let key = flight_key("acme/team/builder", Path::new("/work/project"), LOCAL);
        let claim = |new_instance| StartFlight::claim_key(&flights, key.clone(), new_instance, 100);
        let first = claim(first_new)
            .unwrap()
            .expect("the first launch owns its flight, including New");
        let finish_first = || {
            flights.finish(
                &first,
                Some(("first-session".into(), "first-session".into())),
                100,
            )
        };
        if first_finished {
            finish_first();
        }
        let expected = format!(
            "{ALREADY_STARTING} {}",
            if first_finished { "first-session" } else { "" }
        );
        if second_new {
            // A second explicit instance starts independently, with no ownership
            // of the first request's flight. Its completion cannot replace it.
            assert!(claim(true).unwrap().is_none());
        } else {
            // Ordinary Start waits while pending, then adopts the first session.
            assert_eq!(claim(false).err().unwrap(), expected);
        }
        assert_eq!(claim(false).err().unwrap(), expected);
        if !first_finished {
            finish_first();
        }
        assert_eq!(
            claim(false).err().unwrap(),
            format!("{ALREADY_STARTING} first-session")
        );
    }
}

#[test]
fn new_then_start_shares_the_new_terminal_session() {
    assert_attended_start_order(true, false);
}

#[test]
fn start_then_new_creates_two_sessions_without_replacing_start() {
    assert_attended_start_order(false, true);
}

#[test]
fn new_then_new_creates_two_sessions_without_replacing_the_first() {
    assert_attended_start_order(true, true);
}

#[test]
fn start_then_start_shares_one_session() {
    assert_attended_start_order(false, false);
}

/// SV-GHI-6: a local argv launch carries the user's login-shell PATH to its host,
/// which builds the agent's PATH on it; a shell tab and a remote launch never do.
#[test]
fn a_local_argv_launch_carries_the_login_path_and_a_remote_one_never_does() {
    use crate::commands::pty::{
        build_local_spawn_intent, build_remote_spawn_intent, with_login_path,
    };
    use codesense_pty_core::spawn::HostEnvironment;
    let login = "/opt/homebrew/bin:/usr/bin:/bin";
    let local = |argv: Option<Vec<String>>| {
        build_local_spawn_intent(
            String::new(),
            argv,
            String::new(),
            80,
            24,
            Some(false),
            None,
            None,
            None,
            "plain".into(),
            None,
        )
    };
    let argv = Some(vec![CLI.to_string()]);

    // Seeded cache: the launch carries it and the host builds on it.
    let intent = with_login_path(local(argv.clone()), || Some(login.to_string()));
    assert_eq!(intent.login_path.as_deref(), Some(login));
    let environment =
        HostEnvironment::with_vars(None, None, [("PATH".into(), "/usr/bin:/bin".into())]);
    let spec = intent.resolve(&session("login-path"), &environment);
    assert_eq!(
        spec.command.get_env("PATH"),
        Some(std::ffi::OsStr::new(login))
    );
    assert_eq!(spec.command.get_argv()[0], CLI);

    // A failed probe leaves the host's PATH.
    let intent = with_login_path(local(argv.clone()), || None);
    assert_eq!(intent.login_path, None);

    // A shell tab never asks for it, so it never waits on the probe.
    let intent = with_login_path(local(None), || panic!("a shell tab must not probe"));
    assert_eq!(intent.login_path, None);

    // A remote launch keeps the remote host's own PATH.
    let remote = build_remote_spawn_intent(
        String::new(),
        argv,
        "/remote/worktree".into(),
        80,
        24,
        Some(false),
        None,
        None,
    );
    assert_eq!(remote.login_path, None);
}
