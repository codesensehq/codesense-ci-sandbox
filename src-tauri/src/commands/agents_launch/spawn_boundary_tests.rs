// SPDX-License-Identifier: MIT
#![cfg(test)]
//! Exercise emitted production requests and one-use bindings, never a fabricated SpawnIntent.
use super::*;
use crate::agents::adapter::ManagedHomeFixture;
use crate::commands::mcp_workspace::managed_spawn::ProjectedManagedSpawn;
use crate::commands::{agent_accounts, mcp_workspace, pty};
use codesense_pty_core::spawn::HostEnvironment;
use std::sync::{Arc, Mutex};

fn host_environment() -> HostEnvironment {
    HostEnvironment::with_vars(
        None,
        None,
        [
            ("HOME".into(), "/unused-personal-cli-home".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HERMES_HOME".into(), "/ambient-hermes".into()),
            ("OPENAI_API_KEY".into(), "ambient-secret".into()),
            ("ANTHROPIC_API_KEY".into(), "ambient-secret".into()),
            ("HERMES_ACCEPT_HOOKS".into(), "ambient".into()),
            ("HERMES_SAFE_MODE".into(), "1".into()),
            ("HERMES_IGNORE_USER_CONFIG".into(), "1".into()),
            ("HERMES_IGNORE_RULES".into(), "1".into()),
            ("HERMES_TUI_GATEWAY_URL".into(), "http://outside-owner.invalid".into()),
            ("HERMES_MANAGED_DIR".into(), "/ambient-policy".into()),
        ],
    )
}

fn managed_request<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    program: &Path,
    launch: &crate::agents::launch::AgentLaunch,
    session: &str,
    cwd: &Path,
    prompt: Option<&str>,
) -> pty::SpawnPtyArgs {
    let cli = mcp_scope::LaunchCli::new(program, &launch.launch.args);
    let mut projected = ProjectedManagedSpawn::project_managed_scope(
        launch,
        session,
        &cwd.to_string_lossy(),
        &cli,
        &launch.launch.args,
    )
    .unwrap();
    assert!(projected.take_settings().is_none());
    let plan =
        mcp_workspace::plan_initial_prompt(launch.runtime, projected.take_arguments(), prompt);
    projected.bind(app, launch).unwrap().into_args(
        program,
        &cwd.to_string_lossy(),
        "parity-workspace",
        &plan.launch_args,
    )
}

fn inspect_request<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    site: &str,
    session: &str,
    request: pty::SpawnPtyArgs,
) -> serde_json::Value {
    let token = request
        .environment_binding
        .clone()
        .expect("producer must mint its real environment binding");
    let (spec, values) =
        pty::consume_emitted_spawn_for_test(app, request, session, &host_environment()).unwrap();
    assert!(
        agent_accounts::consume_environment_binding(app, Some(&token)).is_err(),
        "{site}: actual binding is one-use"
    );
    let config = super::hermes_tests::consume(site, session, spec, &values);
    mcp_scope::remove_scope_config(session);
    crate::agents::launch::forget_launch_env(session);
    config
}

fn inspect_execution<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    site: &str,
    session: &str,
    program: &Path,
    cwd: &Path,
    execution: Option<mcp_scope::ScopedExecution>,
    suppress: Vec<String>,
    token: Option<String>,
) -> serde_json::Value {
    let execution = execution.expect("the real producer must request direct execution");
    assert_eq!(
        execution.argv.first().map(String::as_str),
        program.to_str(),
        "{site}"
    );
    assert_eq!(execution.native_launch, AgentCli::Hermes, "{site}");
    let args = serde_json::from_value(json!({
        "command":"", "cwd":cwd, "size":{"cols":80,"rows":24},
        "interactive":true, "envRemove":suppress,
    }))
    .unwrap();
    // This is the same fresh-projection consumer used by terminal_respawn_tab,
    // followed by the same production spawn decoder. No fabricated SpawnIntent.
    let spawn = crate::commands::pty_layout::apply_respawn_projection(
        crate::commands::pty_layout::TabSpawn::Cold(Box::new(args)),
        token.clone(),
        Some(execution),
    )
    .unwrap();
    let crate::commands::pty_layout::TabSpawn::Cold(request) = spawn else {
        panic!("direct spawn")
    };
    let (spec, values) =
        pty::consume_emitted_spawn_for_test(app, *request, session, &host_environment()).unwrap();
    assert!(
        agent_accounts::consume_environment_binding(app, token.as_deref()).is_err(),
        "{site}: actual binding is one-use"
    );
    super::hermes_tests::consume(site, session, spec, &values)
}

#[test]
fn hermes_outside_gateway_environment_cannot_replace_the_managed_owner() {
    let fixture = ManagedHomeFixture::new();
    let program = fixture.managed_program();
    let launch = super::hermes_tests::prepared(&fixture);
    let cwd = worktree(fixture.root());
    let app = tauri::test::mock_builder()
        .manage(agent_accounts::AgentAccountRuntimeState::default())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let id = session("outside-gateway-boundary");
    let request = managed_request(app.handle(), &program, &launch, &id, &cwd, None);
    inspect_request(app.handle(), "outside gateway owner", &id, request);
}

pub(super) async fn run_parity() {
    let fixture = ManagedHomeFixture::new();
    let program = fixture.managed_program();
    let launch = super::hermes_tests::prepared(&fixture);
    let cwd = worktree(fixture.root());
    let app = tauri::test::mock_builder()
        .manage(agent_accounts::AgentAccountRuntimeState::default())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let id = session("renderer-boundary");
    let (mut prepared, creation, settings) =
        apply_for_session(launch.clone(), &program, &id, &cwd, None, true).unwrap();
    assert!(settings.is_none());
    bind_prepared_launch_environment(app.handle(), &mut prepared, creation, false);
    let expected = inspect_execution(
        app.handle(),
        "renderer/dashboard prepare binding",
        &id,
        &program,
        &cwd,
        prepared.scope.execution.take(),
        prepared.launch.env_suppress.clone(),
        prepared.scope.environment_binding.take(),
    );
    mcp_scope::remove_scope_config(&id);

    let respawn_scope = || crate::commands::pty_layout::RespawnScope {
        cwd: cwd.to_string_lossy().into_owned(),
        mcp_allow: launch.launch.mcp_allow.clone(),
        bridge_tools: launch.launch.bridge_tools.clone(),
        account: None,
        launch_args: Some(prepared.launch.args.clone()),
        binary: Some("hermes".into()),
        agent: true,
        default_args: false,
        agent_env: Default::default(),
    };
    let mut respawn = respawn_scope();
    let producer = respawn.take_launch();
    let cli = mcp_scope::LaunchCli::new(&program, &launch.launch.args);
    // An executable that cannot be found is refused, never typed into a shell, and nothing
    // is projected for the refusal.
    let refused = respawn_scope().take_launch().project_agent_scope(
        AgentCli::Hermes,
        &id,
        &cwd,
        respawn.mcp_allow.as_deref().unwrap_or_default(),
        launch.launch.mcp_scope.as_ref(),
        None,
        None,
        (&launch.startup_context.home, &alice()),
        &prepared.launch.args,
    );
    assert!(
        refused.is_err(),
        "an unresolved engine CLI is not relaunched"
    );
    assert!(!mcp_scope::mcp_scope_session_dir(&id).exists());
    let (mut outcome, creation) = producer
        .project_agent_scope(
            AgentCli::Hermes,
            &id,
            &cwd,
            respawn.mcp_allow.as_deref().unwrap_or_default(),
            launch.launch.mcp_scope.as_ref(),
            None,
            Some(&cli),
            (&launch.startup_context.home, &alice()),
            &prepared.launch.args,
        )
        .unwrap();
    crate::commands::mcp_authoring::bind_projected_scope_environment(
        app.handle(),
        &mut outcome,
        creation,
        Vec::new(),
    );
    let cached = inspect_execution(
        app.handle(),
        "cached RespawnScope::take_launch",
        &id,
        &program,
        &cwd,
        outcome.execution.take(),
        launch.launch.env_suppress.clone(),
        outcome.environment_binding,
    );
    assert_eq!(cached, expected, "cached producer");
    mcp_scope::remove_scope_config(&id);
    crate::agents::launch::forget_launch_env(&id);

    let observed = Arc::new(Mutex::new(Vec::new()));
    let events = observed.clone();
    let bound_app = app.handle().clone();
    let bound_program = program.clone();
    let story_root = cwd.clone();
    let probe = Arc::new(
        move |open: &crate::commands::workspace_open::Launch,
              session: &str|
              -> Result<(), String> {
            let root = match &open.target {
                crate::commands::workspace_open::LaunchTarget::Dir { cwd, .. } => cwd,
                crate::commands::workspace_open::LaunchTarget::Story { story_id, .. } => {
                    assert_eq!(story_id, "SW-9");
                    &story_root
                }
            };
            let request = managed_request(
                &bound_app,
                &bound_program,
                &open.agent,
                session,
                root,
                open.initial_prompt.as_deref(),
            );
            let config = inspect_request(
                &bound_app,
                "workspace/UI real launch callback",
                session,
                request,
            );
            events.lock().unwrap().push(config);
            Ok(())
        },
    );
    let routed =
        crate::commands::workspace_open::tests::agent_opens::registered_producers(&launch, probe)
            .await;
    let configs = observed.lock().unwrap();
    assert_eq!(configs.len(), routed.len());
    for ((site, _), config) in routed.iter().zip(configs.iter()) {
        assert_eq!(config, &expected, "{site}");
    }
    drop(configs);

    let id = session("role-binding");
    let request = managed_request(
        app.handle(),
        &program,
        &launch,
        &id,
        &cwd,
        Some("role task"),
    );
    let role = inspect_request(
        app.handle(),
        "role spawn_and_bind production binding",
        &id,
        request,
    );
    assert_eq!(role, expected);
}
