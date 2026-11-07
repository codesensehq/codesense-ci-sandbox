// SPDX-License-Identifier: MIT
#![cfg(test)]
//! Managed-home integration through the real renderer preparation and activity fold.
use super::*;
use crate::agents::adapter::ManagedHomeFixture;
use crate::agents::launch::prepare_typed_for_test;
use crate::agents::service::Placement;
use codesense_pty_core::agent_cli::descriptor;

pub(super) fn prepared(fixture: &ManagedHomeFixture) -> crate::agents::launch::AgentLaunch {
    prepared_with_args(fixture, vec!["-t".into(), "all".into()]).unwrap()
}

fn prepared_with_args(fixture: &ManagedHomeFixture, args: Vec<String>) -> Result<crate::agents::launch::AgentLaunch, String> {
    let home = fixture.root().join("context");
    team_content(&home, &alice());
    let mut agent = draft("registered-home", AgentCli::Hermes);
    agent.revision = 1;
    // Native toolset overrides stay native; the persistent-tool block is independent.
    agent.args = args;
    agent.permissions = Some(crate::agents::model::Permissions {
        hermes: Some(crate::agents::model::HermesPermissions {
            approval: Some("ask".into()),
            deny: vec!["^write_file$".into()],
            ..Default::default()
        }),
        ..Default::default()
    });
    write(&crate::agents::resolve::tests::content(&home, &alice(), TEAM), "config.yaml",
        b"model: {default: startup-model, provider: custom, base_url: 'http://127.0.0.1:9000/v1', api_key: NEVER_COPY}\nhooks_auto_accept: false\nhooks: {pre_tool_call: []}\nplatform_toolsets: {cli: [all]}\nplatforms: {telegram: {enabled: true}}\n");
    agent.startup = vec!["config.yaml".into()];
    agent.context = Some(Vec::new());
    // This bridge-only fixture has no Hermes external-server catalog. The shared
    // draft's Slack server exists only in the Claude .mcp.json fixture.
    Vec::clear(&mut agent.mcp.servers);
    agent.mcp.bridge_tools = Some(vec!["who_am_i".into(), "list_sessions".into()]);
    prepare_typed_for_test(
        &home,
        &alice(),
        agent,
        &Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        },
        interactive(None),
    )
}

#[test]
fn hermes_saved_arguments_gate_all_elements_and_task_text_stays_literal() {
    let fixture = ManagedHomeFixture::new();
    for args in [
        vec!["--", "chat", "--safe-mode"],
        vec!["chat", "--", "--safe-mode"],
        vec!["chat", "--", "-p", "foo"],
        vec!["--sa"], vec!["--ignore-u"], vec!["--ignore-r"],
        vec!["--profile=other"], vec!["-pother"],
    ] {
        let result = prepared_with_args(&fixture, args.iter().map(|s| (*s).into()).collect());
        assert!(result.is_err(), "saved bypass admitted: {args:?}");
        for headless in [false, true] {
            let raw: Vec<String> = args.iter().map(|s| (*s).into()).collect();
            let emitted = if headless {
                crate::commands::dashboard_launch::headless_capture_request(
                    PathBuf::from("/fixture/hermes"),
                    crate::agents::adapter::LaunchDescriptor::of(AgentCli::Hermes),
                    raw, mcp_scope::ScopeOutcome::default(), "task".into(),
                    fixture.root().to_path_buf(),
                    std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
                    "hermes-bypass-argv", None, Vec::new(), Vec::new(), Default::default(), None,
                ).args
            } else {
                crate::commands::mcp_workspace::plan_initial_prompt(AgentCli::Hermes, raw, None).launch_args
            };
            let mut native = codesense_pty_core::agent_cli::Prepare::new(emitted, json!({}), "/helper");
            assert!(codesense_pty_core::agent_cli::adapter(AgentCli::Hermes).prepare(&mut native).is_err());
        }
    }
    let launch = prepared_with_args(&fixture, vec!["--pro".into(), "custom".into(), "--accept-hooks".into()]).unwrap();
    let task = "explain --safe-mode and --profile without using them";
    let plan = crate::commands::mcp_workspace::plan_initial_prompt(AgentCli::Hermes, launch.launch.args, Some(task));
    assert!(plan.launch_args.iter().any(|arg| arg == &format!("--query={task}")));
    let mut native = codesense_pty_core::agent_cli::Prepare::new(plan.launch_args, json!({}), "/helper");
    codesense_pty_core::agent_cli::adapter(AgentCli::Hermes).prepare(&mut native).unwrap();
}

#[test]
fn hermes_admin_policy_blocks_preview_and_launch_with_exact_message() {
    let fixture = ManagedHomeFixture::new();
    let policy = fixture.root().join("admin-policy");
    std::fs::create_dir_all(&policy).unwrap();
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore {
        fn drop(&mut self) { match &self.0 { Some(value) => std::env::set_var("HERMES_MANAGED_DIR", value), None => std::env::remove_var("HERMES_MANAGED_DIR") } }
    }
    let _restore = Restore(std::env::var_os("HERMES_MANAGED_DIR"));
    std::env::set_var("HERMES_MANAGED_DIR", &policy);
    let launch = prepared(&fixture);
    std::fs::write(policy.join("config.yaml"), "admin: true").unwrap();
    let message = "This Mac has an admin-managed Hermes policy; CodeSense-managed Hermes sessions aren't supported here yet.";
    assert!(prepared_with_args(&fixture, Vec::new()).unwrap_err().contains(message));
    let cwd = worktree(fixture.root());
    let id = session("hermes-admin-policy");
    let error = apply_for_session(launch, &fixture.managed_program(), &id, &cwd, None, false).err().unwrap();
    assert_eq!(error, message);
    assert!(!mcp_scope::mcp_scope_session_dir(&id).exists());
}

#[test]
fn a_listed_hermes_prepares_with_its_containment_warnings() {
    let fixture = ManagedHomeFixture::new();
    assert!(AgentCli::ALL.contains(&AgentCli::Hermes));
    assert_eq!(
        serde_json::from_value::<AgentCli>(json!("hermes")).unwrap(),
        AgentCli::Hermes
    );
    let launch = prepared(&fixture);
    assert_eq!(launch.runtime, AgentCli::Hermes);
    assert_eq!(launch.manifest_errors, 0);
    assert!(launch
        .warnings
        .iter()
        .all(|warning| !warning.message.contains("not enforced")));
    assert!(launch.warnings.iter().any(|warning| warning.message == "Toolsets widened by this agent’s arguments; memory, skills and scheduled jobs stay blocked."));
    for host in [
        "api.github.com",
        "raw.githubusercontent.com",
        "hermes-agent.nousresearch.com",
        "openrouter.ai",
        "portal.nousresearch.com",
    ] {
        assert!(
            launch
                .warnings
                .iter()
                .any(|warning| warning.message.contains(host)),
            "{host}"
        );
    }
}

#[test]
fn hermes_renderer_projection_is_private_qualified_and_does_not_carry_personal_state() {
    let fixture = ManagedHomeFixture::new();
    let program = fixture.managed_program();
    let launch = prepared(&fixture);
    let cwd = worktree(fixture.root());
    let personal = fixture.root().join(".hermes");
    for (name, text) in [
        (
            "config.yaml",
            "personal_marker: PERSONAL_CONFIG\nmodel: {default: PERSONAL_MODEL}\nhooks: {PERSONAL_HOOK: []}\n",
        ),
        (".env", "OPENAI_API_KEY=PERSONAL_SECRET"),
        ("SOUL.md", "PERSONAL_SOUL"),
        ("memories/MEMORY.md", "PERSONAL_MEMORY"),
    ] {
        write(&personal, name, text.as_bytes());
    }
    write(
        &cwd,
        "config.yaml",
        b"model: {default: UNSELECTED_WORKTREE_MODEL}\n",
    );
    write(&cwd, ".hermes.md", b"repo instructions");
    let before = std::fs::read(personal.join("config.yaml")).unwrap();
    let mut results = Vec::new();
    std::thread::scope(|threads| {
        let jobs: Vec<_> = (0..3).map(|_| {
            let launch = launch.clone();
            let cwd = &cwd;
            let program = &program;
            let personal = &personal;
            threads.spawn(move || {
                let id = session("hermes-managed");
                let (prepared, creation, settings) = apply_for_session(launch, program, &id, cwd, Some(personal), true).unwrap();
                assert!(settings.is_none());
                let homes: Vec<_> = prepared.scope.extra_env.iter().filter(|(key, _)| key == "HERMES_HOME").collect();
                assert_eq!(homes.len(), 1);
                let home = PathBuf::from(&homes[0].1);
                assert_eq!(mode_of(&home), 0o700);
                assert_eq!(mode_of(&home.join("config.yaml")), 0o600);
                for key in ["HERMES_TUI_DIR", "npm_config_cache", "UV_CACHE_DIR"] {
                    assert_eq!(prepared.scope.extra_env.iter().filter(|(name, _)| name == key).count(), 1);
                }
                let arguments: Vec<std::ffi::OsString> = prepared.launch.args.iter().map(std::ffi::OsString::from).collect();
                let activity = codesense_pty_core::activity::prepare_direct(AgentCli::Hermes,
                    program.as_os_str(), &arguments, "/bin/true", Some(home.clone()), None);
                assert!(activity.arguments.as_ref().is_some_and(|(_, qualified)| *qualified));
                let config: serde_json::Value = serde_yaml::from_slice(&std::fs::read(home.join("config.yaml")).unwrap()).unwrap();
                assert_eq!(config["model"], json!({"default": "startup-model", "provider": "custom", "base_url": "http://127.0.0.1:9000/v1"}));
                assert_eq!(config["terminal"], json!({"cwd": cwd, "backend": "local"}));
                assert_eq!(config["memory"], json!({"memory_enabled": false, "user_profile_enabled": false}));
                assert_eq!(config["platform_toolsets"]["cli"], json!(["terminal", "file"]));
                assert_eq!(config["mcp_servers"]["codesense"]["tools"]["include"], json!(["list_sessions", "who_am_i"]));
                assert!(config.get("personal_marker").is_none(), "personal config is never carried");
                assert!(config["hooks"].is_object());
                assert_eq!(config["hooks_auto_accept"], true);
                assert_eq!(config["platforms"], json!({}));
                for file in [".env", "SOUL.md", "memories/MEMORY.md", "cron/jobs.json"] { assert!(!home.join(file).exists()); }
                assert!(prepared.warnings.iter().any(|w| w.code == LaunchWarningCode::ShadowingRootDocument && w.message.contains(".hermes.md")));
                assert!(!prepared.launch.args.iter().any(|arg| arg == "--ignore-rules"));
                for key in ["HERMES_HOME", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "HERMES_ACCEPT_HOOKS"] {
                    assert!(prepared.launch.env_suppress.iter().any(|name| name == key));
                }
                (id, home, config, creation)
            })
        }).collect();
        for job in jobs {
            results.push(job.join().unwrap());
        }
    });
    let homes: std::collections::HashSet<_> = results.iter().map(|(_, home, _, _)| home).collect();
    assert_eq!(homes.len(), 3);
    let ids: std::collections::HashSet<_> = results.iter().map(|(id, _, _, _)| id).collect();
    assert_eq!(ids.len(), 3);
    assert_eq!(std::fs::read(personal.join("config.yaml")).unwrap(), before);
    assert_eq!(
        std::fs::read_to_string(program.parent().unwrap().join("prepares"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    for (id, home, _, creation) in results {
        drop(creation);
        mcp_scope::remove_scope_config(&id);
        assert!(!home.exists());
    }
    assert_eq!(descriptor(AgentCli::Hermes).root_doc, "AGENTS.md");
}

#[test]
fn hermes_runtime_preparation_failure_refuses_the_renderer_preview_and_removes_scope() {
    let fixture = ManagedHomeFixture::new();
    let launch = prepared(&fixture);
    let cwd = worktree(fixture.root());
    let id = session("hermes-failed-prepare");
    let result = apply_for_session(
        launch,
        Path::new("/missing/selected-agent"),
        &id,
        &cwd,
        None,
        true,
    );
    assert!(result.is_err());
    assert!(!mcp_scope::mcp_scope_session_dir(&id).exists());
}

#[tokio::test]
async fn hermes_every_spawn_site_produces_one_effective_home() {
    super::spawn_boundary_tests::run_parity().await;
}

pub(super) fn consume(
    site: &str,
    session_id: &str,
    mut spec: codesense_pty_core::runner::PtySpec,
    env: &[(String, String)],
) -> serde_json::Value {
    assert_eq!(
        spec.command.get_env("CODESENSE_TAB_SESSION_ID"),
        Some(std::ffi::OsStr::new(session_id)),
        "{site}"
    );
    let leaked: Vec<_> = ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "HERMES_ACCEPT_HOOKS", "HERMES_SAFE_MODE", "HERMES_IGNORE_USER_CONFIG", "HERMES_IGNORE_RULES", "HERMES_TUI_GATEWAY_URL"]
        .into_iter().filter(|key| spec.command.get_env(key).is_some()).collect();
    assert!(leaked.is_empty(), "{site}: ambient bypass/credential values survived: {leaked:?}");
    assert_eq!(
        env.iter().filter(|(name, _)| name == "HERMES_HOME").count(),
        1,
        "{site}"
    );
    let home = PathBuf::from(
        spec.command
            .get_env("HERMES_HOME")
            .unwrap_or_else(|| panic!("{site}: actual spawn request lost HERMES_HOME")),
    );
    let managed = PathBuf::from(spec.command.get_env("HERMES_MANAGED_DIR").expect("managed policy directory must be pinned"));
    assert_eq!(managed, home.join("managed-policy"), "{site}");
    assert!(std::fs::read_dir(&managed).unwrap().next().is_none());
    let config_path = home.join("config.yaml");
    let mut before: serde_json::Value =
        serde_yaml::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    before["unrelated"] = json!({"keep": true});
    std::fs::write(&config_path, serde_yaml::to_string(&before).unwrap()).unwrap();
    let requested = spec.command.get_argv().to_vec();
    codesense_session_server::host::prepare_direct_launch_for_test(
        &mut spec,
        AgentCli::Hermes,
        "/fixture/activity-helper",
    );
    let once = std::fs::read(&config_path).unwrap();
    // A relaunch prepares the argv its fresh projection asks for, never an engine's wrapper.
    let wrapped = std::mem::replace(spec.command.get_argv_mut(), requested);
    codesense_session_server::host::prepare_direct_launch_for_test(
        &mut spec,
        AgentCli::Hermes,
        "/fixture/activity-helper",
    );
    assert_eq!(
        &wrapped,
        spec.command.get_argv(),
        "{site}: same fresh launch"
    );
    assert_eq!(
        std::fs::read(&config_path).unwrap(),
        once,
        "{site}: idempotent relaunch fold"
    );
    assert_eq!(
        spec.command
            .get_env(codesense_pty_core::activity::RELEASE_QUALIFIED_ENV),
        Some(std::ffi::OsStr::new("1")),
        "{site}"
    );
    let engine = codesense_pty_core::activity::launch_of(spec.command.get_argv())
        .unwrap_or_else(|| {
            panic!(
                "{site}: the real request must exec the CLI through its engine, not {:?}",
                spec.command.get_argv()
            )
        });
    assert_eq!(engine.provider, AgentCli::Hermes, "{site}");
    assert!(engine.end_on_loss && engine.serves_at, "{site}");
    assert_eq!(
        engine.attached, engine.standalone,
        "{site}: no startup argv may disappear"
    );
    // Semantic parsing rejects duplicate YAML keys, including hooks and the bridge.
    let mut config: serde_json::Value =
        serde_yaml::from_slice(&std::fs::read(home.join("config.yaml")).unwrap()).unwrap();
    assert_eq!(
        config["mcp_servers"].as_object().unwrap().len(),
        1,
        "{site}"
    );
    assert!(config["hooks"].is_object(), "{site}");
    assert_eq!(config["unrelated"]["keep"], true);
    assert_eq!(config["approvals"]["mode"], "manual", "{site}");
    let blocks = config["hooks"]["pre_tool_call"].as_array().unwrap();
    assert_eq!(
        blocks
            .iter()
            .filter(|hook| hook["command"]
                .as_str()
                .is_some_and(|command| command.ends_with(" --hook-block hermes")))
            .count(),
        3,
        "{site}"
    );
    assert_eq!(
        blocks
            .iter()
            .filter(|hook| hook["matcher"] == "^(memory|skill_manage|cronjob_manage)$")
            .count(),
        1,
        "{site}: containment floor retained once"
    );
    assert!(
        engine.attached.iter().any(|arg| arg == "-t"),
        "{site}: native override retained"
    );
    assert!(blocks.iter().any(|hook| hook["matcher"] == "^write_file$"));
    assert!(blocks
        .iter()
        .any(|hook| hook["matcher"] == "^mcp__codesense__(?!(who_am_i|list_sessions)$).*"));
    assert_eq!(
        config["memory"],
        json!({"memory_enabled": false, "user_profile_enabled": false})
    );
    assert_eq!(config["skills"]["creation_nudge_interval"], 0);
    assert_eq!(config["platforms"], json!({}));
    assert_eq!(config["telemetry"]["shared_metrics"]["enabled"], false);
    assert!(!home.join("cron/jobs.json").exists());
    assert_eq!(config["model"]["default"], "startup-model", "{site}");
    assert_eq!(
        config["platform_toolsets"]["cli"],
        json!(["terminal", "file"]),
        "{site}"
    );
    assert_eq!(
        config["mcp_servers"]["codesense"]["tools"]["include"],
        json!(["list_sessions", "who_am_i"]),
        "{site}"
    );
    assert_eq!(
        config["mcp_servers"]["codesense"]["env"]["CODESENSE_TAB_SESSION_ID"], session_id,
        "{site}: explicit MCP child identity"
    );
    assert_eq!(
        config["mcp_servers"]["codesense"]["env"]["CODESENSE_MCP_SOCK"],
        json!(crate::mcp::mcp_socket_path()),
        "{site}: explicit instance route"
    );
    config["terminal"]["cwd"] = json!("<CWD>");
    *config["mcp_servers"]["codesense"]["env"]
        .get_mut("CODESENSE_TAB_SESSION_ID")
        .unwrap() = json!("<SESSION>");
    config
}

#[test]
fn hermes_startup_model_is_required_and_pinned_without_secrets() {
    let fixture = ManagedHomeFixture::new();
    let program = fixture.managed_program();
    let launch = prepared(&fixture);
    let cwd = worktree(fixture.root());
    write(
        &cwd,
        "config.yaml",
        b"model: {default: unselected, api_key: never-copy}\n",
    );
    let mut no_startup = launch.clone();
    no_startup.manifest.entries.clear();
    let id = session("optional-model");
    assert_eq!(apply_for_session(no_startup, &program, &id, &cwd, None, true).unwrap_err(), "Hermes needs a model. Set a local endpoint in Edit → Runs on. API-key providers come later.");
    assert!(!mcp_scope::mcp_scope_session_dir(&id).exists());
    write(
        &crate::agents::resolve::tests::content(&launch.startup_context.home, &alice(), TEAM),
        "config.yaml",
        b"model: {default: changed-after-resolution}\n",
    );
    let id = session("stale-startup");
    assert!(apply_for_session(launch, &program, &id, &cwd, None, true)
        .unwrap_err()
        .contains("changed after resolution"));
    assert!(!mcp_scope::mcp_scope_session_dir(&id).exists());
}

/// Export the actual managed bridge entry for the pinned upstream-consumer/SDK seam.
/// An exported fixture home remains under /tmp until the parent finishes the seam.
#[test]
fn hermes_managed_bridge_projects_explicit_socket_and_session_for_filtered_children() {
    let fixture = ManagedHomeFixture::new();
    let program = fixture.managed_program();
    let launch = prepared(&fixture);
    let cwd = worktree(fixture.root());
    let seam_home = tempfile::Builder::new()
        .prefix("SV-ENG-105C2-H-")
        .tempdir_in("/tmp")
        .unwrap();
    // ManagedHomeFixture owns the process environment guard and restores HOME on drop.
    std::env::set_var("HOME", seam_home.path());
    let bridge = crate::commands::deps::bridge_symlink_path().unwrap();
    std::fs::create_dir_all(bridge.parent().unwrap()).unwrap();
    let executable = std::env::var_os("CODESENSE_BRIDGE_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| program.clone());
    std::os::unix::fs::symlink(executable, &bridge).unwrap();
    let id = session("hermes-filtered-child");
    let (_, creation, _) = apply_for_session(launch, &program, &id, &cwd, None, true).unwrap();
    let home = mcp_scope::mcp_scope_session_dir(&id);
    let config: serde_json::Value =
        serde_yaml::from_slice(&std::fs::read(home.join("config.yaml")).unwrap()).unwrap();
    let entry = config["mcp_servers"]["codesense"].clone();
    let socket = crate::mcp::mcp_socket_path();
    let artifact = json!({"entry": entry, "session_id": id, "socket_path": socket});
    drop(creation);
    mcp_scope::remove_scope_config(&id);
    crate::agents::adapter::sweep_managed_runtimes(&seam_home.path().join(".codesense/mcp-scope"));
    if let Some(output) = std::env::var_os("CODESENSE_HERMES_MCP_ENTRY_JSON") {
        assert!(
            std::env::var_os("CODESENSE_BRIDGE_BIN").is_some(),
            "export requires the freshly built bridge"
        );
        std::fs::write(output, serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();
        let _ = seam_home.keep();
    }
    assert_eq!(entry["env"]["CODESENSE_TAB_SESSION_ID"], id);
    assert_eq!(entry["env"]["CODESENSE_MCP_SOCK"], json!(socket));
}

#[test]
fn hermes_model_endpoint_admission_blocks_registered_launch_without_startup_model() {
    let fixture = ManagedHomeFixture::new();
    let home = fixture.root().join("context");
    team_content(&home, &alice());
    let mut agent = draft("no-model", AgentCli::Hermes);
    agent.revision = 1;
    agent.startup.clear();
    agent.context = Some(Vec::new());
    agent.mcp.servers.clear();
    let error = prepare_typed_for_test(
        &home,
        &alice(),
        agent,
        &Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        },
        interactive(None),
    )
    .unwrap_err();
    assert_eq!(error, "Hermes needs a model. Set a local endpoint in Edit → Runs on. API-key providers come later.");
}

#[test]
fn hermes_model_endpoint_admission_projects_saved_inline_overrides_without_keys() {
    let fixture = ManagedHomeFixture::new();
    let program = fixture.managed_program();
    let original = prepared(&fixture);
    let home = original.startup_context.home.clone();
    let mut agent = draft("inline-model", AgentCli::Hermes);
    agent.revision = 1;
    agent.args = vec!["--model=first".into(), "-m".into(), "final".into()];
    agent.startup = vec!["config.yaml".into()];
    agent.context = Some(Vec::new());
    agent.mcp.servers.clear();
    agent.set_startup_settings(Some(std::collections::BTreeMap::from([("model".into(), json!({"provider":"custom", "base_url":"http://127.0.0.1:11434/v1", "default":"configured"}))])));
    let launch = prepare_typed_for_test(
        &home,
        &alice(),
        agent,
        &Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        },
        interactive(None),
    )
    .unwrap();
    let id = session("inline-model");
    let cwd = worktree(fixture.root());
    let (_, creation, _) = apply_for_session(launch, &program, &id, &cwd, None, true).unwrap();
    let config: serde_json::Value = serde_yaml::from_slice(
        &std::fs::read(mcp_scope::mcp_scope_session_dir(&id).join("config.yaml")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        config["model"],
        json!({"provider":"custom", "base_url":"http://127.0.0.1:11434/v1", "default":"final"})
    );
    assert!(!config.to_string().contains("NEVER_COPY"));
    assert!(config["model"].get("api_key").is_none());
    drop(creation);
    mcp_scope::remove_scope_config(&id);
}

#[tokio::test]
async fn hermes_interactive_startup_renderer_without_role_uses_literal_tui_query() {
    let fixture = ManagedHomeFixture::new();
    let program = fixture.managed_program();
    let cwd = worktree(fixture.root());
    let host = crate::commands::role_startup::tests::StartupHost::new(
        fixture.root().join("startup.sqlite3"),
    );
    let saved_launch = prepared(&fixture);
    for prompt in [
        None,
        Some("-literal '$()' `backtick` \"quoted\"\n非ASCII".to_string()),
        Some("x".repeat(60 * 1024)),
        Some("  literal whitespace\n  ".to_string()),
    ] {
        let id = session("native-interactive-query");
        let launch = saved_launch.clone();
        let (mut launch, _creation, _) =
            apply_for_session(launch, &program, &id, &cwd, None, true).unwrap();
        prepare_role_startup(&host, &mut launch, None, &id, prompt.as_deref())
            .await
            .unwrap();
        assert_tui_query(&launch.launch.args, prompt.as_deref());
        assert!(launch.prompt_in_argv);
        assert!(launch.default_prompt.is_none());
        mcp_scope::remove_scope_config(&id);
        crate::agents::launch::forget_launch_env(&id);
    }
}

fn assert_tui_query(args: &[String], prompt: Option<&str>) {
    assert_eq!(args.first().map(String::as_str), Some("chat"));
    assert_eq!(args.iter().filter(|arg| arg.as_str() == "--tui").count(), 1);
    assert!(args.iter().all(
        |arg| !["-q", "-Q", "--quiet", "--oneshot", "--query-file", "--cli"]
            .contains(&arg.as_str())
    ));
    let queries: Vec<_> = args
        .iter()
        .filter_map(|arg| arg.strip_prefix("--query="))
        .collect();
    assert_eq!(queries, prompt.into_iter().collect::<Vec<_>>());
}

#[tokio::test]
async fn hermes_interactive_startup_direct_producer_and_cached_respawn_keep_literal_tui_query() {
    let fixture = ManagedHomeFixture::new();
    let program = fixture.managed_program();
    let cwd = worktree(fixture.root());
    let launch = prepared(&fixture);
    let id = session("native-query-respawn");
    let (_prepared, _creation, _) =
        apply_for_session(launch.clone(), &program, &id, &cwd, None, true).unwrap();
    let host = crate::commands::role_startup::tests::StartupHost::new(
        fixture.root().join("startup.sqlite3"),
    );
    let prompt = "-literal '$()' `backtick` \"quoted\"\n非ASCII";
    let plan = crate::commands::role_startup::start_argv(
        &host,
        None,
        None,
        &id,
        AgentCli::Hermes,
        launch.launch.args.clone(),
        Some(prompt),
        |plan| async { Ok(plan) },
    )
    .await
    .unwrap();
    assert_tui_query(&plan.launch_args, Some(prompt));
    assert!(plan.pty_prompt.is_none());
    assert_ne!(
        descriptor(AgentCli::Hermes).prompt_flag.as_deref(),
        Some("-q")
    );
    assert_ne!(
        plan.launch_args,
        descriptor(AgentCli::Hermes).headless_entry
    );
    mcp_scope::remove_scope_config(&id);
    let mut respawn = crate::commands::pty_layout::RespawnScope {
        cwd: cwd.to_string_lossy().into_owned(),
        mcp_allow: launch.launch.mcp_allow.clone(),
        bridge_tools: launch.launch.bridge_tools.clone(),
        account: None,
        launch_args: Some(plan.launch_args.clone()),
        binary: Some("hermes".into()),
        agent: true,
        default_args: false,
        agent_env: Default::default(),
    };
    let cli = mcp_scope::LaunchCli::new(&program, &launch.launch.args);
    respawn
        .take_launch()
        .project_agent_scope(
            AgentCli::Hermes,
            &id,
            &cwd,
            respawn.mcp_allow.as_deref().unwrap_or_default(),
            launch.launch.mcp_scope.as_ref(),
            None,
            Some(&cli),
            (&launch.startup_context.home, &alice()),
            &plan.launch_args,
        )
        .unwrap();
    assert_tui_query(respawn.launch_args.as_ref().unwrap(), Some(prompt));
    mcp_scope::remove_scope_config(&id);
    crate::agents::launch::forget_launch_env(&id);
}

#[tokio::test]
async fn hermes_interactive_startup_refuses_oversized_task_before_spawn() {
    let fixture = ManagedHomeFixture::new();
    let program = fixture.managed_program();
    let cwd = worktree(fixture.root());
    let launch = prepared(&fixture);
    let id = session("native-query-size");
    let (mut launch, _creation, _) =
        apply_for_session(launch, &program, &id, &cwd, None, true).unwrap();
    let host = crate::commands::role_startup::tests::StartupHost::new(
        fixture.root().join("startup.sqlite3"),
    );
    let oversized = "x".repeat(70 * 1024);
    let result = prepare_role_startup(&host, &mut launch, None, &id, Some(&oversized)).await;
    assert!(
        result.is_err(),
        "a task above the native argv budget must be refused, never typed or truncated"
    );
    let team = crate::commands::dashboard_launch::LaunchTeam {
        org: ORG.into(),
        team: TEAM.into(),
    };
    let near_limit = "x".repeat(codesense_pty_core::agent_cli::MAX_INITIAL_PROMPT_BYTES - 1);
    let composed = crate::commands::role_startup::start_argv(
        &host,
        Some(&team),
        Some("reviewer"),
        &id,
        AgentCli::Hermes,
        Vec::new(),
        Some(&near_limit),
        |_| async { Ok(()) },
    )
    .await;
    assert_eq!(
        composed.err().as_deref(),
        Some("This starting task is too long for this CLI. Shorten it before launching."),
        "role facts must count toward the same final budget"
    );
    mcp_scope::remove_scope_config(&id);
    crate::agents::launch::forget_launch_env(&id);
}

#[test]
fn hermes_interactive_startup_pinned_parser_roundtrips_production_argv() {
    let Ok(python) = std::env::var("CODESENSE_HERMES_PARSER_PYTHON") else {
        return;
    };
    let source = std::env::var("CODESENSE_HERMES_PARSER_SOURCE")
        .expect("pinned parser source accompanies interpreter");
    let home = tempfile::tempdir().unwrap();
    for prompt in [
        "-literal '$()' `backtick` \"quoted\"\n非ASCII".to_string(),
        "é".repeat(30 * 1024),
        "  literal whitespace\n  ".to_string(),
    ] {
        let plan = crate::commands::mcp_workspace::plan_initial_prompt(
            AgentCli::Hermes,
            Vec::new(),
            Some(&prompt),
        );
        let result = std::process::Command::new(&python)
            .args(["-c", "import json,sys; from hermes_cli._parser import build_top_level_parser; p,_,_=build_top_level_parser(); a=p.parse_args(json.loads(sys.argv[1])); assert a.command=='chat' and a.tui and not a.oneshot_exit; print(json.dumps(a.query,ensure_ascii=False))"])
            .arg(serde_json::to_string(&plan.launch_args).unwrap())
            .env_clear().env("HOME", home.path()).env("PYTHONPATH", &source).output().unwrap();
        assert!(
            result.status.success(),
            "pinned parser refused production argv: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<String>(&result.stdout).unwrap(),
            prompt
        );
    }
}

#[test]
fn hermes_interactive_startup_preview_blocks_oversized_saved_task() {
    let fixture = ManagedHomeFixture::new();
    let home = fixture.root().join("context");
    team_content(&home, &alice());
    let mut agent = draft("native-size", AgentCli::Hermes);
    agent.revision = 1;
    agent.args.clear();
    agent.context = Some(Vec::new());
    agent.mcp.servers.clear();
    agent.set_startup_settings(Some(serde_json::from_value(json!({"model": {"provider": "custom", "base_url": "http://127.0.0.1:9000/v1", "default": "local"}})).unwrap()));
    agent.default_prompt = "é".repeat(35 * 1024);
    let preview = crate::agents::resolve::preview_agent(
        &home,
        &alice(),
        &agent,
        &crate::agents::service::Placement::Team {
            org: ORG.into(),
            team: TEAM.into(),
        },
        Default::default(),
    )
    .unwrap();
    let admission = serde_json::to_value(&preview.launch_admission).unwrap();
    assert_eq!(admission["allowed"], false);
    assert_eq!(
        admission["reason"],
        "This starting task is too long for this CLI. Shorten it before launching."
    );
    assert_eq!(admission["initial_prompt_max_bytes"], 64 * 1024);
    assert!(prepare_typed_for_test(
        &home,
        &alice(),
        agent,
        &Placement::Team {
            org: ORG.into(),
            team: TEAM.into()
        },
        interactive(None)
    )
    .is_err());
}

#[tokio::test]
async fn hermes_interactive_startup_does_not_add_tui_to_headless_preparation() {
    let fixture = ManagedHomeFixture::new();
    let program = fixture.managed_program();
    let cwd = worktree(fixture.root());
    let id = session("native-headless-preserved");
    let launch = prepared(&fixture);
    let (mut launch, _creation, _) =
        apply_for_session(launch, &program, &id, &cwd, None, true).unwrap();
    launch.launch.mode = LaunchMode::Headless;
    launch.default_role = Some("reviewer".into());
    let before = launch.launch.args.clone();
    let host = crate::commands::role_startup::tests::StartupHost::new(
        fixture.root().join("startup.sqlite3"),
    );
    prepare_role_startup(&host, &mut launch, None, &id, Some("task"))
        .await
        .unwrap();
    assert_eq!(launch.launch.args, before);
    assert!(!launch.prompt_in_argv);
    mcp_scope::remove_scope_config(&id);
    crate::agents::launch::forget_launch_env(&id);
}

/// Which directly exec'd launches enter interactively: every CLI by its adapter's own fact,
/// a saved or cached `chat --tui` never doubled, and a headless launch left as it was.
#[tokio::test]
async fn a_direct_exec_enters_interactively_once_and_only_for_interactive_engine_launches() {
    let program = Path::new("/fixture/agent");
    let saved = |args: &[&str]| args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>();
    for cli in AgentCli::every_cli() {
        let engine = codesense_pty_core::agent_cli::adapter(cli).end_on_engine_loss();
        let launched = mcp_scope::scoped_execution(cli, program, saved(&["-t", "all"]));
        assert_eq!(
            launched.is_some(),
            engine,
            "{cli}: only an engine CLI is exec'd directly"
        );
    }
    let argv = |args: &[&str]| {
        mcp_scope::scoped_execution(AgentCli::Hermes, program, saved(args))
            .unwrap()
            .argv
    };
    let entered = ["/fixture/agent", "chat", "--tui", "-t", "all"];
    for given in [
        &["-t", "all"][..],
        &["chat", "-t", "all"],
        &["--tui", "-t", "all"],
        &["chat", "--tui", "-t", "all"],
    ] {
        assert_eq!(argv(given), entered, "{given:?}");
    }
    // A cached relaunch hands its recorded arguments back: the same exec again.
    let once = argv(&["-t", "all"]);
    assert_eq!(
        argv(&once.iter().skip(1).map(String::as_str).collect::<Vec<_>>()),
        once
    );
    // A headless launch is captured with its own argv and requests no exec.
    let fixture = ManagedHomeFixture::new();
    let program = fixture.managed_program();
    let cwd = worktree(fixture.root());
    let id = session("headless-entry");
    let app = tauri::test::mock_builder()
        .manage(crate::commands::agent_accounts::AgentAccountRuntimeState::default())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    for (mode, direct) in [
        (LaunchMode::Interactive, true),
        (LaunchMode::Headless, false),
    ] {
        let mut launch = prepared(&fixture);
        launch.launch.mode = mode;
        let (mut prepared, creation, _) =
            apply_for_session(launch, &program, &id, &cwd, None, true).unwrap();
        let args = prepared.launch.args.clone();
        bind_prepared_launch_environment(app.handle(), &mut prepared, creation, false);
        assert_eq!(prepared.scope.execution.is_some(), direct, "{mode:?}");
        assert_eq!(
            prepared.launch.args, args,
            "{mode:?}: the saved argv is not rewritten"
        );
        assert!(
            !prepared.launch.args.iter().any(|arg| arg == "--tui"),
            "{mode:?}"
        );
        mcp_scope::remove_scope_config(&id);
        crate::agents::launch::forget_launch_env(&id);
    }
}

/// The production headless request for `prompt` in a prepared managed home, folded.
fn headless_request(
    fixture: &ManagedHomeFixture,
    launch: crate::agents::launch::AgentLaunch,
    prompt: &str,
    timeout: Option<std::time::Duration>,
) -> (crate::shared::proc_capture::CaptureRequest, PathBuf, Option<ScopeCreation>) {
    let program = fixture.managed_program();
    let cwd = worktree(fixture.root());
    let id = session("hermes-headless");
    let (prepared, creation, _) = apply_for_session(launch.clone(), &program, &id, &cwd, None, true).unwrap();
    let home = PathBuf::from(&prepared.scope.extra_env.iter().find(|(key, _)| key == "HERMES_HOME").unwrap().1);
    let request = crate::commands::dashboard_launch::headless_capture_request(
        program,
        crate::agents::adapter::LaunchDescriptor::of(AgentCli::Hermes),
        prepared.launch.args,
        prepared.scope,
        prompt.into(),
        cwd,
        std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
        &id,
        None,
        Vec::new(),
        Vec::new(),
        launch.launch.env.clone(),
        timeout,
    );
    (request, home, creation)
}

fn folded_hooks(home: &Path) -> serde_json::Value {
    let config: serde_json::Value = serde_yaml::from_slice(&std::fs::read(home.join("config.yaml")).unwrap()).unwrap();
    config["hooks"].clone()
}

#[test]
fn hermes_headless_capture_is_one_literal_query_and_runs_the_final_fold() {
    let fixture = ManagedHomeFixture::new();
    let launch = prepared(&fixture);
    for prompt in ["-starts with a dash", "$(touch /tmp/never-run)", "line one\nline two", "--safe-mode -p other"] {
        let (mut request, home, creation) = headless_request(&fixture, launch.clone(), prompt, None);
        let queries: Vec<_> = request.args.iter().filter(|arg| arg.starts_with("--query=")).collect();
        assert_eq!(queries, [&format!("--query={prompt}")], "one literal query token");
        assert_eq!(request.args.first().map(String::as_str), Some("chat"));
        for flag in ["-Q", "--format", "stream-json"] {
            assert!(request.args.iter().any(|arg| arg == flag), "{flag}");
        }
        assert_eq!(request.stdin, None, "Hermes reads no prompt on stdin");
        crate::commands::dashboard_launch::fold_headless_launch(&mut request, AgentCli::Hermes, Some("/fixture/activity-helper")).unwrap();
        let hooks = folded_hooks(&home);
        let commands: Vec<String> = hooks["pre_tool_call"].as_array().unwrap().iter().map(|entry| entry["command"].as_str().unwrap().to_owned()).collect();
        assert!(commands.iter().all(|command| command != "--hook-block hermes"), "marker resolved: {commands:?}");
        assert!(commands.iter().any(|command| command.contains("/fixture/activity-helper") && command.ends_with(" --hook-block hermes")));
        assert!(hooks["on_session_end"].as_array().is_some_and(|entries| !entries.is_empty()), "observers survive the fold");
        drop(creation);
    }
}

#[test]
fn hermes_headless_fold_refuses_bypasses_and_keeps_the_floor_under_toolset_overrides() {
    let fixture = ManagedHomeFixture::new();
    let launch = prepared(&fixture);
    let (mut request, home, _creation) = headless_request(&fixture, launch.clone(), "task", None);
    // `-t all` is in the saved arguments: the executable floor still blocks persistent tools.
    assert!(request.args.windows(2).any(|pair| pair == ["-t", "all"]));
    crate::commands::dashboard_launch::fold_headless_launch(&mut request, AgentCli::Hermes, Some("/fixture/activity-helper")).unwrap();
    let hooks = folded_hooks(&home);
    assert!(hooks["pre_tool_call"].as_array().unwrap().iter().any(|entry| entry["matcher"].as_str().is_some_and(|m| m.contains("memory|skill_manage|cronjob_manage"))));
    // A bypass that reached the request is refused before anything runs.
    for bypass in ["--safe-mode", "--ignore-user-config", "-pother"] {
        let (mut request, _, _creation) = headless_request(&fixture, launch.clone(), "task", None);
        request.args.insert(1, bypass.into());
        let error = crate::commands::dashboard_launch::fold_headless_launch(&mut request, AgentCli::Hermes, Some("/fixture/activity-helper")).unwrap_err();
        assert!(error.contains("does not support safe-mode"), "{bypass}: {error}");
    }
    // Without the packaged helper the markers cannot be resolved: no run.
    let (mut request, _, _creation) = headless_request(&fixture, launch, "task", None);
    assert!(crate::commands::dashboard_launch::fold_headless_launch(&mut request, AgentCli::Hermes, None).is_err());
}

#[test]
fn hermes_headless_keeps_the_thirty_minute_default_and_a_configured_deadline() {
    let fixture = ManagedHomeFixture::new();
    let launch = prepared(&fixture);
    let (default, _, _a) = headless_request(&fixture, launch.clone(), "task", None);
    assert_eq!(default.timeout, std::time::Duration::from_secs(30 * 60));
    let (short, _, _b) = headless_request(&fixture, launch, "task", Some(std::time::Duration::from_secs(1)));
    assert_eq!(short.timeout, std::time::Duration::from_secs(1));
}
