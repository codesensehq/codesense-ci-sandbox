// SPDX-License-Identifier: MIT
//! Desktop facts for the Hermes preview adapter.

use super::{Attribution, DesktopCli, PermissionBlock, RunnerRecipe, Starter, Unattended};
use crate::agents::manifest::GeneratedTarget;
use crate::agents::model::{Permissions, ValidationError};
use crate::install::agent_account_store::AccountRequirement;
use url::Url as ModelEndpoint;
use codesense_pty_core::agent_cli::hermes::native_arguments;

pub(super) fn desktop() -> DesktopCli {
    DesktopCli {
        model_endpoint: Some(super::ModelEndpointPort { resolve: model_settings, missing_message: missing_model_message, model_arguments }),
        account_requirement: AccountRequirement::None,
        supports_wake_delivery: true,
        supports_remote_launch: false,
        preview: true,
        attribution: Attribution::Unsupported("hermes"),
        runner: RunnerRecipe {
            default_model: None,
            analyze_args: Vec::new(),
            skill_args: Vec::new(),
        },
        starter: Starter {
            name: "Hermes",
            description: "Hermes Agent in a private CodeSense home, with the model you choose. Starts with the team's AGENTS.md when it has one.",
            unattended_name: "Hermes (unattended)",
            unattended_description: "Hermes Agent that runs without asking for permission (approval prompts skipped), for unattended background work.",
        },
        unattended: Unattended {
            bypass_flag: true,
            permissions: Permissions::default(),
        },
        escalates: |agent| native_arguments(&agent.args).yolo || agent.permissions.as_ref()
            .and_then(|p| p.hermes.as_ref()).and_then(|p| p.approval.as_deref()) == Some("yolo"),
        launch_settings: crate::agents::permissions::hermes,
        // SV-ENG-105 PR C: prepare the managed runtime and isolated home.
        teardown_home: Some(runtime::teardown_home),
        home_config: Some(home_config),
        seed_home: Some(runtime::seed_home),
        managed_launch_env: || Ok(Vec::new()),
        inline_config: |_| {
            let mut config = settings(false).as_object().unwrap().clone();
            config.extend(serde_json::json!({
                "memory": {"memory_enabled": false, "user_profile_enabled": false},
                "skills": {"creation_nudge_interval": 0},
                "telemetry": {"shared_metrics": {"enabled": false, "send": false, "offer_version": 2}},
                "auth": {"adopt_external_logins": false},
                "gateway": {"unmanaged_idle_exit_seconds": 60},
                "platforms": {}
            }).as_object().unwrap().clone());
            Ok(config)
        },
        shell_mcp_config: |_| None,
        pretrust: |_, _, _| Vec::new(),
        permissions: PermissionBlock {
            validate: validate_permissions,
            set: |permissions, block| crate::agents::model::set_permission_block(&mut permissions.hermes, block),
        },
        // Pinned 0.22.5 hook-only deferred-call denial and allowed control are proven.
        enforces_bridge_tool_allowlist: true,
        launch_notes,
        permission_display: Some(permission_display),
        headless_settings,
        native_permission_rules: None,
        status_line: None,
        usage_record: |_| None,
        managed_settings: &[],
        session_log_usage: None,
    }
}

// Only pinned, leaf built-in sets; composites and plugins can reintroduce memory/cron/skills.
const MANAGED_TOOLSETS: &[&str] = &[
    "terminal",
    "file",
    "web",
    "search",
    "x_search",
    "vision",
    "video",
    "image_gen",
    "video_gen",
    "computer_use",
    "browser",
    "tts",
    "todo",
    "session_search",
];

fn validate_permissions(permissions: &Permissions) -> Result<(), ValidationError> {
    let Some(own) = &permissions.hermes else {
        return Ok(());
    };
    crate::agents::model::known_permission_keys(super::AgentCli::Hermes, &own.extra)?;
    if let Some(approval) = &own.approval {
        if !matches!(std::string::String::as_str(approval), "ask" | "yolo") {
            return Err(ValidationError::InvalidPermission(approval.clone()));
        }
    }
    for toolset in own.toolsets.iter().flatten() {
        if !MANAGED_TOOLSETS.contains(&std::string::String::as_str(toolset)) {
            return Err(ValidationError::InvalidPermission(toolset.clone()));
        }
    }
    for pattern in &own.deny {
        crate::agents::model::check_permission(pattern)?;
    }
    Ok(())
}

fn headless_settings() -> serde_json::Value {
    settings(true)
}

/// One native base shared by scoped projection and both launch modes. Native args
/// can add toolsets, so the three pinned persistent-tool names stay blocked independently.
pub(crate) fn settings(headless: bool) -> serde_json::Value {
    let mut blocks = vec![block_hook("^(memory|skill_manage|cronjob_manage)$")];
    if headless {
        blocks.push(block_hook("^(terminal|write_file|patch|computer_use)$"));
    }
    serde_json::json!({
        "approvals": {"mode": "manual"},
        "platform_toolsets": {"cli": ["terminal", "file"]},
        "hooks": {"pre_tool_call": blocks}
    })
}

fn launch_notes(args: &[String]) -> Vec<String> {
    let mut notes = vec!["Hermes may contact api.github.com, raw.githubusercontent.com, hermes-agent.nousresearch.com, openrouter.ai and portal.nousresearch.com. Host egress is not restricted.".into()];
    if let Some(toolsets) = native_arguments(args).toolsets {
        let change = if toolsets.composite { "widened" } else { "overridden" };
        notes.push(format!("Toolsets {change} by this agent’s arguments; memory, skills and scheduled jobs stay blocked."));
    }
    notes
}

pub(crate) fn block_hook(matcher: &str) -> serde_json::Value {
    serde_json::json!({"matcher": matcher, "command": format!("--hook-block {}", codesense_pty_core::agent_cli::AgentCli::as_str(super::AgentCli::Hermes)), "fail_closed": true, "timeout": 2})
}

fn permission_display(settings: &serde_json::Value, args: &[String]) -> String {
    let native = native_arguments(args);
    let approval =
        if native.yolo || settings["approvals"]["mode"] == "off" {
            "yolo"
        } else {
            "ask"
        };
    let toolsets = match native.toolsets {
        Some(selection) => {
            let names = <[&str]>::join(&selection.names, ", ");
            let count = std::vec::Vec::len(&selection.names);
            let selected = if selection.composite {
                format!("toolsets {names}")
            } else {
                format!("{} selected toolset{}: {names}", count, if count == 1 { "" } else { "s" })
            };
            format!("{selected} (memory, skill_manage, cronjob_manage blocked)")
        },
        None => format!("{} toolsets", settings["platform_toolsets"]["cli"].as_array().map_or(0, Vec::len)),
    };
    let denies = settings["hooks"]["pre_tool_call"]
        .as_array()
        .map_or(0, Vec::len);
    format!("{approval} approvals · {toolsets} · {denies} tool deny rules")
}

pub(super) mod runtime;

/// Only non-secret startup model selectors are admitted until endpoint credentials land.
fn home_config(
    launch: &crate::agents::launch::AgentLaunch,
    cwd: &std::path::Path,
    config: &mut serde_json::Value,
) -> Result<(), String> {
    for entry in launch
        .manifest
        .generated
        .iter()
        .filter(|entry| entry.target == GeneratedTarget::LaunchSettings)
    {
        if crate::agents::manifest::json_hash(&entry.content) != entry.hash {
            return Err(
                "startup settings changed after resolution; prepare the agent again".into(),
            );
        }
        config
            .as_object_mut()
            .ok_or("invalid managed config")?
            .extend(
                entry
                    .content
                    .as_object()
                    .ok_or("invalid Hermes settings")?
                    .clone(),
            );
    }
    config["model"] = model_settings(
        &launch.startup_context.home,
        &launch.startup_context.principal,
        &launch.manifest,
        &launch.launch.args,
        &crate::agents::content_reader::FsContentReader,
    )?;
    config["terminal"] = serde_json::json!({"cwd": cwd, "backend": "local"});
    Ok(())
}

const MODEL_REQUIRED: &str =
    "Hermes needs a model. Set a local endpoint in Edit → Runs on. API-key providers come later.";

const ADMIN_POLICY: &str = "This Mac has an admin-managed Hermes policy; CodeSense-managed Hermes sessions aren't supported here yet.";

fn unmanaged_host(system: &std::path::Path, ambient: Option<&std::path::Path>) -> Result<(), String> {
    let ambient_policy = ambient.is_some_and(|path| {
        match std::fs::read_dir(path) {
            Ok(mut entries) => std::iter::Iterator::next(&mut entries).is_some(),
            Err(_) => std::path::Path::exists(path),
        }
    });
    if std::path::Path::exists(system) || ambient_policy {
        return Err(ADMIN_POLICY.into());
    }
    Ok(())
}

pub(super) fn ensure_unmanaged_host() -> Result<(), String> {
    let ambient = std::env::var_os("HERMES_MANAGED_DIR");
    unmanaged_host(std::path::Path::new("/etc/hermes"), ambient.as_deref().map(std::path::Path::new))
}

fn missing_model_message() -> String {
    ensure_unmanaged_host().err().unwrap_or_else(|| MODEL_REQUIRED.into())
}

pub(crate) fn merge_model(
    source: &serde_json::Value,
    model: &mut serde_json::Map<String, serde_json::Value>,
) {
    if let Some(name) = source.get("model").and_then(serde_json::Value::as_str) {
        model.insert("default".into(), name.into());
    }
    for key in ["default", "provider", "base_url"] {
        if let Some(value) = source
            .get("model")
            .and_then(|m| m.get(key))
            .or_else(|| source.get(key))
            .filter(|v| v.is_string())
        {
            model.insert(key.into(), value.clone());
        }
    }
}

fn model_arguments(args: &[String]) -> super::ModelArguments {
    let native = native_arguments(args);
    super::ModelArguments {
        input_args: args.to_vec(),
        model: native.model.map(str::to_owned),
        edit: codesense_pty_core::agent_cli::hermes::NativeArguments::model_edit(&native, args).map(|(args, value_index)| {
            let mut clear_args = args.clone();
            drop(std::vec::Vec::drain(&mut clear_args, value_index - 1..=value_index));
            super::ModelArgumentEdit { clear_args, args, value_index }
        }),
    }
}

/// The content root of one manifest layer, resolved the way the manifest itself resolves its layers.
/// The root is unfiltered by inheritance sharing: it is safe only because the caller checks each
/// file's pinned hash, so never reuse it for an unpinned read.
fn manifest_layer_root(
    reader: &dyn crate::agents::content_reader::ContentReader,
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    layer: &crate::agents::manifest::ContentLayer,
) -> Result<crate::context_repo::inheritance::ContentRoot, String> {
    use crate::agents::manifest::ContentLayer;
    let (org, team) = match layer {
        ContentLayer::Team { org, team } => (org.as_str(), team.as_str()),
        ContentLayer::Personal { org, .. } => (org.as_deref().unwrap_or_default(), ""),
    };
    let target = crate::agents::manifest_resolve::ManifestTarget {
        context_root: None,
        org: org.to_owned(),
        team: team.to_owned(),
        repo: None,
    };
    crate::agents::manifest_resolve::content_layers_with(reader, home, principal, &target)
        .into_iter()
        .find_map(|(id, root)| (&id == layer).then_some(root))
        .ok_or_else(|| match layer {
            ContentLayer::Team { org, team } => {
                format!("startup layer {org}/{team} is not available")
            }
            ContentLayer::Personal { .. } => "startup private layer is not available".to_owned(),
        })
}

/// One selector reader for preview, admission and the private home; no credentials are projected.
fn model_settings(
    home: &std::path::Path,
    principal: &crate::domain::Principal,
    manifest: &crate::agents::manifest::StartupManifest,
    args: &[String],
    reader: &dyn crate::agents::content_reader::ContentReader,
) -> Result<serde_json::Value, String> {
    ensure_unmanaged_host()?;
    let native = native_arguments(args);
    codesense_pty_core::agent_cli::hermes::NativeArguments::check_managed(&native).map_err(str::to_owned)?;
    use std::io::Read;
    let mut model = serde_json::Map::new();
    for entry in &manifest.entries {
        for file in entry
            .files
            .iter()
            .filter(|file| file.destination == "config.yaml")
        {
            let layer = file.layer_in(&entry.layer);
            let root = manifest_layer_root(reader, home, principal, layer)?;
            let mut bytes = Vec::new();
            root.open(reader, &file.logical)
                .map_err(|e| e.to_string())?
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if crate::agents::manifest::content_hash(&bytes) != file.hash {
                return Err(
                    "startup config.yaml changed after resolution; prepare the agent again".into(),
                );
            }
            let startup: serde_json::Value = serde_yaml::from_slice(&bytes)
                .map_err(|e| format!("invalid startup config.yaml: {e}"))?;
            merge_model(&startup, &mut model);
        }
    }
    for entry in manifest
        .generated
        .iter()
        .filter(|entry| entry.target == GeneratedTarget::LaunchSettings)
    {
        if crate::agents::manifest::json_hash(&entry.content) != entry.hash {
            return Err(
                "startup settings changed after resolution; prepare the agent again".into(),
            );
        }
        merge_model(&entry.content, &mut model);
    }
    if native.api_key.is_some() { return Err(MODEL_REQUIRED.into()); }
    for (key, value) in [("default", native.model), ("provider", native.provider), ("base_url", native.base_url)] {
        if let Some(value) = value { model.insert(key.into(), value.into()); }
    }
    let valid = (|| {
        if model.get("provider")?.as_str()? != "custom"
            || std::primitive::str::is_empty(model.get("default")?.as_str()?.trim())
        {
            return None;
        }
        let url = std::result::Result::ok(ModelEndpoint::parse(model.get("base_url")?.as_str()?))?;
        let local = match url.host()? {
            url::Host::Domain(name) => name == "localhost",
            url::Host::Ipv4(ip) => ip.is_loopback(),
            url::Host::Ipv6(ip) => ip.is_loopback(),
        };
        (local
            && matches!(url.scheme(), "http" | "https")
            && std::primitive::str::is_empty(url.username())
            && url.password().is_none()
            && ModelEndpoint::query(&url).is_none()
            && url.fragment().is_none())
        .then_some(())
    })()
    .is_some();
    if !valid {
        return Err(MODEL_REQUIRED.into());
    }
    Ok(model.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hermes_admin_policy_presence_refuses_system_or_nonempty_ambient() {
        let fixture = super::super::ManagedHomeFixture::new();
        let system = fixture.root().join("system-policy");
        let ambient = fixture.root().join("ambient-policy");
        assert!(unmanaged_host(&system, Some(&ambient)).is_ok());
        std::fs::create_dir_all(&ambient).unwrap();
        assert!(unmanaged_host(&system, Some(&ambient)).is_ok());
        std::fs::write(ambient.join("config.yaml"), "admin: true").unwrap();
        assert_eq!(unmanaged_host(&system, Some(&ambient)).unwrap_err(), ADMIN_POLICY);
        std::fs::create_dir_all(&system).unwrap();
        assert_eq!(unmanaged_host(&system, None).unwrap_err(), ADMIN_POLICY);
    }

    #[test]
    fn hermes_permissions_validate_set_remove_and_reject_unsafe_toolsets() {
        let adapter = desktop();
        let mut permissions = Permissions::default();
        (adapter.permissions.set)(&mut permissions, serde_json::json!({"approval":"ask", "toolsets":["terminal", "file"], "deny":["^write_file$"]})).unwrap();
        (adapter.permissions.validate)(&permissions).unwrap();
        for block in [
            serde_json::json!({"approval":"off"}),
            serde_json::json!({"unknown":true}),
        ] {
            (adapter.permissions.set)(&mut permissions, block).unwrap();
            assert!((adapter.permissions.validate)(&permissions).is_err());
        }
        for toolset in [
            "memory",
            "skills",
            "skill_manage",
            "cronjob",
            "all",
            "coding",
            "plugin_unknown",
        ] {
            (adapter.permissions.set)(&mut permissions, serde_json::json!({"toolsets":[toolset]}))
                .unwrap();
            assert!(
                (adapter.permissions.validate)(&permissions).is_err(),
                "{toolset}"
            );
        }
        (adapter.permissions.set)(&mut permissions, serde_json::Value::Null).unwrap();
        assert!(permissions.hermes.is_none());
    }

    #[test]
    fn hermes_forbidden_tool_floor_blocks_pinned_names_with_native_toolset_args() {
        let mut agent =
            crate::agents::resolve::tests::draft("native-toolsets", super::super::AgentCli::Hermes);
        for args in [
            vec!["-t", "all"],
            vec!["-t", "all", "--yolo"],
            vec!["--toolsets=all", "--yolo"],
        ] {
            agent.args = args.iter().map(|arg| (*arg).to_owned()).collect();
            assert!((desktop().launch_notes)(&agent.args)
                .iter()
                .any(|note| note.contains("Toolsets widened")));
            for mode in [
                crate::agents::model::LaunchMode::Foreground,
                crate::agents::model::LaunchMode::Headless,
            ] {
                let generated = crate::agents::permissions::generate(&agent, agent.runtime, mode);
                let blocks = generated[0].content["hooks"]["pre_tool_call"]
                    .as_array()
                    .unwrap();
                let floor = "^(memory|skill_manage|cronjob_manage)$";
                assert_eq!(
                    blocks
                        .iter()
                        .filter(|block| block["matcher"] == floor)
                        .count(),
                    1
                );
                let result = std::process::Command::new("python3").args(["-I", "-c",
                    "import re,sys; p=re.compile(sys.argv[1]); assert all(p.fullmatch(n) for n in ['memory','skill_manage','cronjob_manage']); assert all(not p.fullmatch(n) for n in ['read_file','terminal','mcp__codesense__allowed'])",
                    floor]).output().unwrap();
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
            }
        }
        let native = settings(false);
        assert_eq!(
            native["hooks"]["pre_tool_call"][0]["matcher"],
            "^(memory|skill_manage|cronjob_manage)$"
        );
        assert_eq!((desktop().launch_notes)(&[]).len(), 1);
        assert_eq!(
            (desktop().launch_notes)(&["--".into(), "-t".into(), "all".into()]).len(),
            1
        );
    }

    #[test]
    fn hermes_native_facts_use_effective_aliases_and_toolset_selection() {
        let base = settings(false);
        for (args, expected, widened) in [
            (vec!["--yo", "-t", "web"], "yolo approvals · 1 selected toolset: web (memory, skill_manage, cronjob_manage blocked) · 1 tool deny rules", false),
            (vec!["--yol", "--tools=all"], "yolo approvals · toolsets all (memory, skill_manage, cronjob_manage blocked) · 1 tool deny rules", true),
            (vec!["-tall", "--toolset", "web,web,,file"], "ask approvals · 2 selected toolsets: web, file (memory, skill_manage, cronjob_manage blocked) · 1 tool deny rules", false),
        ] {
            let args: Vec<String> = args.iter().map(|arg| (*arg).into()).collect();
            let mut agent = crate::agents::resolve::tests::draft("native-facts", super::super::AgentCli::Hermes);
            agent.args = args.clone();
            assert_eq!(agent.is_escalating(), native_arguments(&args).yolo);
            assert_eq!(permission_display(&base, &args), expected);
            let note = &launch_notes(&args)[1];
            assert!(note.contains(if widened { "Toolsets widened" } else { "Toolsets overridden" }));
            assert!(note.contains("memory, skills and scheduled jobs stay blocked"));
        }
    }

    #[test]
    fn hermes_empty_native_toolset_reset_uses_configured_display_without_override_note() {
        let args = vec!["-t".into(), "all".into(), "--toolsets=".into()];
        assert_eq!(permission_display(&settings(false), &args), "ask approvals · 2 toolsets · 1 tool deny rules");
        assert_eq!(launch_notes(&args).len(), 1);
    }

    #[test]
    fn hermes_model_projection_canonicalizes_aliases_and_clears_without_losing_prefixes() {
        let args = vec!["--model=first".into(), "--mo=second".into(), "-Qmfinal".into()];
        let fact = model_arguments(&args);
        assert_eq!(fact.input_args, args);
        assert_eq!(fact.model.as_deref(), Some("final"));
        let edit = fact.edit.unwrap();
        assert_eq!(edit.args, ["--model", "final", "-Q"]);
        assert_eq!(edit.value_index, 1);
        assert_eq!(edit.clear_args, ["-Q"]);
        assert!(model_arguments(&[]).edit.is_none());
    }

    #[test]
    fn hermes_starters_are_adapter_driven_and_discoverable() {
        use crate::agents::builtin;
        let cli = super::super::AgentCli::Hermes;
        let starter = builtin::definition(cli);
        let unattended = builtin::unattended_definition(cli);
        // Listed, its starters are found as every listed CLI's are.
        assert_eq!(builtin::get(&builtin::key(cli)), Some(starter.clone()));
        assert_eq!(
            builtin::get(&builtin::unattended_key(cli)),
            Some(unattended.clone())
        );
        assert!(!starter.is_escalating());
        assert!(unattended.is_escalating());
        assert!(unattended.args.iter().any(|arg| arg == "--yolo"));
        assert!(desktop().enforces_bridge_tool_allowlist);
        assert!((desktop().inline_config)(true).unwrap()["platforms"]
            .as_object()
            .unwrap()
            .is_empty());
    }
}
