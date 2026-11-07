// SPDX-License-Identifier: MIT
//! Registry conformance tests retain real argument paths and explicit classifications.
use serde_json::{Map, Value};

#[derive(Debug, PartialEq, Eq)]
enum Class {
    Identity,
    ConnectionOnlyIdentity,
    Target,
    NoIdentity,
}

// Explicitly reviewed inventories. Never derive the no-identity set by subtracting
// today's registry: that would silently approve tomorrow's new/renamed tool.
const TARGET_TOOLS: &[&str] = &[
    "list_sessions",
    "session_usage",
    "session_read",
    "inbox_list",
    "list_agent_updates",
    "dismiss_agent_mark",
    "context_repo_status",
    "context_repo_history",
    "report_context_sync",
    "context_operation_status",
    "get_sync_status",
    "list_hydrated",
    "hydrate_worktree",
    "hydrate_story",
    "reverse_sync_worktree",
    "preview_reverse_sync_worktree",
    "routine_run",
    "read_run",
    "read_run_evidence",
    "read_run_work_product",
    "wv_upsert_attention",
];
// These tools operate on WorkspaceTarget::Local; its session names the workspace
// subject at target.sessionId, not the caller. No other nested path is exempt.
const WORKSPACE_TARGET_TOOLS: &[&str] = &[
    "context_operation_status",
    "get_sync_status",
    "list_hydrated",
    "hydrate_story",
    "hydrate_worktree",
    "reverse_sync_worktree",
    "preview_reverse_sync_worktree",
];
const NO_IDENTITY_TOOLS: &[&str] = &[
    "why_line",
    "refresh_history",
    "add_source_repo",
    "agent_get",
    "agent_list",
    "apply_canvas",
    "archive_team",
    "attach_context_repository",
    "attach_source_repo_path",
    "avatar_add",
    "avatar_list",
    "avatar_share",
    "cancel_context_operation",
    "cancel_remote_agent_task",
    "capture_machine_passphrase",
    "close_terminal_tab",
    "close_workspace",
    "codesense_call_tool",
    "codesense_get_tool_schema",
    "codesense_overview",
    "codesense_search_tools",
    "command_list",
    "command_run",
    "commit_registry_edit_local",
    "connect_machine",
    "copy_between_contexts",
    "create_content_file",
    "create_dashboard",
    "create_local_organization",
    "create_team",
    "delete_content_file",
    "delete_team",
    "describe_setup",
    "detach_context_repository",
    "disconnect_machine",
    "end_unowned_terminal_session",
    "enroll_private_machine_account",
    "forget_missing_workspace",
    "get_active_agent_account",
    "get_human_status",
    "get_live_data_contract",
    "get_publish_set",
    "get_story_format",
    "get_viewer_state",
    "health_ping",
    "install_terminal_server",
    "list_agent_accounts",
    "list_context_attachments",
    "list_context_content",
    "list_machines",
    "list_move_intents",
    "list_orgs_teams",
    "list_run_events",
    "list_runs",
    "list_source_repos",
    "list_ssh_sources",
    "list_worktrees",
    "model_rates",
    "move_between_contexts",
    "move_content_file",
    "move_workspace",
    "notes_history",
    "notes_list",
    "notes_read",
    "notes_read_at",
    "notes_search",
    "open_terminal_tab",
    "pane_list",
    "pane_open",
    "pane_write",
    "prepare_machine_terminal_server_replacement",
    "prepare_remote_workspace",
    "publish_dashboard",
    "query_context",
    "read_content_file",
    "read_epic_repos",
    "read_remote_agent_task",
    "read_rendered_document",
    "read_terminal_tab",
    "read_worktree_config",
    "refresh_frontmatter_cache",
    "remote_workspace_fetch",
    "remove_from_context",
    "remove_machine",
    "remove_source_repo",
    "rename_team",
    "rename_terminal_tab",
    "rename_workspace",
    "render_canvas",
    "reorder_workspace",
    "resolve_agent_account",
    "resolve_machine_identity",
    "restart_machine_terminal_server",
    "restart_terminal_server",
    "retry_machine",
    "role_save",
    "role_state",
    "roles_list",
    "route_file_to_context",
    "routine_get",
    "routine_list",
    "routine_result",
    "routine_runs",
    "run_in_terminal_tab",
    "save_machine",
    "save_source_repo_attachment",
    "sense_sync_adopt",
    "sense_sync_list_layers",
    "sense_sync_move_layer",
    "sense_sync_read_layer_file",
    "sense_sync_read_lockfile",
    "set_active_agent_account",
    "set_active_team",
    "set_epic_repos",
    "set_machine_auto_connect",
    "set_machine_hidden",
    "set_machine_remote_mcp_control",
    "set_model_rate",
    "set_private_items_use",
    "set_team_parent",
    "set_team_starter_page",
    "set_viewer_state",
    "share_dashboard",
    "start_remote_agent_task",
    "start_terminal_server",
    "stop_terminal_server",
    "story_changes",
    "story_totals",
    "terminal_server_status",
    "test_machine_connection",
    "unpublish_dashboard",
    "unroute_file_from_context",
    "unshare_machine",
    "update_codesense_docs",
    "update_workspace_base",
    "validate_live_data_doc",
    "validate_story",
    "workspace_group_add",
    "workspace_group_create",
    "workspace_group_delete",
    "workspace_group_list",
    "workspace_group_remove",
    "workspace_group_rename",
    "workspace_group_set_members",
    "write_worktree_config",
    "wv_audit_prune",
    "wv_audit_query",
    "wv_classify_action",
    "wv_dispatch_state",
    "wv_enqueue",
    "wv_handoff_state",
    "wv_list_overdue_harvests",
    "wv_pause_t2_windows",
    "wv_queue_list",
    "wv_read_posture",
    "wv_read_recent",
    "wv_report_cycle",
    "wv_resume_t2_windows",
    "wv_retire_decision",
    "wv_scope_state",
    "wv_set_posture",
    "wv_settle_approval",
    "wv_spawn_worker",
    "wv_upsert_watermark",
    "wv_verify_done",
    "wv_wait",
    "wv_watch_list",
    "wv_write_dispatch",
];

// These callers carry identity only in the connection. Enumerate their data
// arguments too: a new/renamed caller field must trigger a fresh classification,
// even when its spelling is unfamiliar to the identity-field scanner.
const CONNECTION_ONLY_IDENTITY_TOOLS: &[(&str, &[&str])] = &[
    ("mark_unread", &["messageIds", "org", "team"]),
    ("inbox_post", &["title", "context", "backlink", "need"]),
    ("send_to_session", &["handle", "text"]),
];

fn classify(name: &str) -> Option<Class> {
    if CONNECTION_ONLY_IDENTITY_TOOLS
        .iter()
        .any(|(tool, _)| *tool == name)
    {
        Some(Class::ConnectionOnlyIdentity)
    } else if super::remote_authority::is_identity_tool(name) {
        Some(Class::Identity)
    } else if TARGET_TOOLS.contains(&name) {
        Some(Class::Target)
    } else if NO_IDENTITY_TOOLS.contains(&name) {
        Some(Class::NoIdentity)
    } else {
        None
    }
}

#[derive(Debug)]
struct Field {
    path: Vec<String>,
    repository_owner: bool,
}

fn identity_fields(schema: &Value, include_data: bool) -> Vec<Field> {
    fn walk(
        root: &Value,
        schema: &Value,
        path: &[String],
        refs: &mut Vec<String>,
        fields: &mut Vec<Field>,
        include_data: bool,
    ) {
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            if !refs.iter().any(|r| r == reference) {
                if let Some(target) = reference.strip_prefix('#').and_then(|p| root.pointer(p)) {
                    refs.push(reference.into());
                    walk(root, target, path, refs, fields, include_data);
                    refs.pop();
                }
            }
        }
        if let Some(props) = schema.get("properties").and_then(Value::as_object) {
            for (key, value) in props {
                let mut child = path.to_vec();
                child.push(key.clone());
                if include_data
                    || [
                        "sessionId",
                        "session_id",
                        "sender",
                        "launchedBy",
                        "launched_by",
                        "instanceId",
                        "instance_id",
                        "surfacedSession",
                        "surfaced_session",
                        "checkSession",
                        "check_session",
                        "author",
                        "actor",
                        "createdBy",
                        "created_by",
                        "caller",
                        "callerSessionId",
                        "caller_session_id",
                        "owner",
                        "workerId",
                        "worker_id",
                        "runId",
                        "run_id",
                        "delegateRunId",
                        "delegate_run_id",
                    ]
                    .contains(&key.as_str())
                {
                    fields.push(Field {
                        path: child.clone(),
                        repository_owner: key == "owner" && props.contains_key("repo"),
                    });
                }
                walk(root, value, &child, refs, fields, include_data);
            }
        }
        if let Some(items) = schema.get("items") {
            let mut child = path.to_vec();
            child.push("0".into());
            walk(root, items, &child, refs, fields, include_data);
        }
        for union in ["allOf", "anyOf", "oneOf"] {
            if let Some(branches) = schema.get(union).and_then(Value::as_array) {
                for branch in branches {
                    walk(root, branch, path, refs, fields, include_data);
                }
            }
        }
    }
    let mut fields = Vec::new();
    walk(
        schema,
        schema,
        &[],
        &mut Vec::new(),
        &mut fields,
        include_data,
    );
    fields
}

fn pointer(path: &[String]) -> String {
    path.iter()
        .map(|key| format!("/{}", key.replace('~', "~0").replace('/', "~1")))
        .collect()
}

fn probe_at(path: &[String], value: Value) -> Value {
    match path.split_first() {
        None => value,
        Some((key, rest)) if key == "0" => Value::Array(vec![probe_at(rest, value)]),
        Some((key, rest)) => {
            let mut object = Map::new();
            object.insert(key.clone(), probe_at(rest, value));
            Value::Object(object)
        }
    }
}

fn target_field(name: &str, field: &Field) -> bool {
    if field.repository_owner {
        return true;
    }
    if WORKSPACE_TARGET_TOOLS.contains(&name)
        && field.path.len() == 2
        && field.path[0] == "target"
        && ["sessionId", "session_id"].contains(&field.path[1].as_str())
    {
        return true;
    }
    if field.path.len() != 1 {
        return false;
    }
    let key = field.path[0].as_str();
    (TARGET_TOOLS.contains(&name) && ["sessionId", "session_id"].contains(&key))
        || (name == "list_sessions" && ["launchedBy", "launched_by"].contains(&key))
        || (name == "wv_upsert_attention" && key == "owner")
        || ([
            "routine_run",
            "routine_cancel",
            "routine_context",
            "routine_done",
            "routine_publish_result",
            "read_run",
            "read_run_evidence",
            "read_run_work_product",
        ]
        .contains(&name)
            && ["runId", "run_id"].contains(&key))
}

pub(super) fn check_tool(name: &str, schema: &Value) -> Result<(), String> {
    let class = classify(name).ok_or_else(|| format!("unclassified served tool {name}"))?;
    let fields = identity_fields(schema, class == Class::ConnectionOnlyIdentity);
    if class == Class::ConnectionOnlyIdentity {
        let (_, data_keys) = CONNECTION_ONLY_IDENTITY_TOOLS
            .iter()
            .find(|(tool, _)| *tool == name)
            .expect("explicit connection-only classification");
        for field in &fields {
            if field.path.len() != 1 || !data_keys.contains(&field.path[0].as_str()) {
                return Err(format!(
                    "{name}{}: undeclared argument on connection-only identity tool",
                    pointer(&field.path)
                ));
            }
        }
        return Ok(());
    }
    if class == Class::Identity {
        // Every field-based binder declares the receiver's root key. A rename
        // cannot silently turn an identity tool into zero probes.
        let keys: &[&str] = if name == "send_role_message" {
            &["sender"]
        } else if name == "wv_write_decision" {
            &["owner"]
        } else if ["open_workspace", "open_remote_tab"].contains(&name) {
            &["launchedBy", "launched_by"]
        } else if let Some((_, keys)) = super::remote_authority::CLAIMED_SESSION_TOOLS
            .iter()
            .find(|(tool, _)| *tool == name)
        {
            keys
        } else if super::remote_authority::ROLE_INSTANCE_TOOLS.contains(&name) {
            &["instanceId", "instance_id"]
        } else {
            &["sessionId", "session_id"]
        };
        if !fields
            .iter()
            .any(|f| f.path.len() == 1 && keys.contains(&f.path[0].as_str()))
        {
            return Err(format!("{name}: missing declared caller field {keys:?}"));
        }
        for (tool, run_keys) in [
            ("wv_claim_queue", &["delegateRunId", "delegate_run_id"][..]),
            ("wv_post_assertion", &["runId", "run_id"][..]),
        ] {
            if name == tool
                && !fields
                    .iter()
                    .any(|f| f.path.len() == 1 && run_keys.contains(&f.path[0].as_str()))
            {
                return Err(format!("{name}: missing declared run correlation field"));
            }
        }
    }
    for field in fields {
        if target_field(name, &field) {
            continue;
        }
        let location = pointer(&field.path);
        if class != Class::Identity {
            return Err(format!(
                "{name}{location}: caller-shaped field needs an identity/target classification"
            ));
        }
        let run_claim = field.path.len() == 1
            && matches!(
                (name, field.path[0].as_str()),
                ("wv_claim_queue", "delegateRunId" | "delegate_run_id")
                    | ("wv_post_assertion", "runId" | "run_id")
            );
        if run_claim {
            if super::local_identity::owned_run(Some("foreign"), Some("mine")).is_ok() {
                return Err(format!("{name}{location}: foreign run accepted"));
            }
            continue;
        }
        let mut probe = probe_at(&field.path, Value::from("foreign-caller"));
        let object = probe.as_object_mut().expect("tool argument root");
        let refused =
            super::remote_authority::bind_identity(name, object, Some(("handle", "mine"))).is_err();
        if !refused && probe.pointer(&location) == Some(&Value::from("foreign-caller")) {
            return Err(format!(
                "{name}{location}: receiver still consumes the supplied caller"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn why_line_has_no_caller_or_target_session_identity() {
        assert!(!TARGET_TOOLS.contains(&"why_line"));
        assert!(NO_IDENTITY_TOOLS.contains(&"why_line"));
        let schema = serde_json::to_value(rmcp::schemars::schema_for!(crate::attribution::why_line::Params)).unwrap();
        check_tool("why_line", &schema).unwrap();
    }

    use serde_json::json;

    #[test]
    fn routine_cancel_classifies_its_run_as_a_target_and_its_session_as_the_caller() {
        let schema = json!({"properties":{
            "runId":{"type":"string"}, "sessionId":{"type":"string"}
        }});
        assert!(check_tool("routine_cancel", &schema).is_ok());
    }

    #[test]
    fn declared_workspace_target_paths_are_not_caller_identity() {
        let schema =
            json!({"properties":{"target":{"properties":{"sessionId":{"type":"string"}}}}});
        assert!(check_tool("context_operation_status", &schema).is_ok());
        let wrong_path =
            json!({"properties":{"context":{"properties":{"sessionId":{"type":"string"}}}}});
        assert!(check_tool("context_operation_status", &wrong_path).is_err());
    }

    #[test]
    fn nested_identity_is_probed_at_the_receiver_path() {
        let schema = json!({"type":"object", "properties": {
            "sessionId":{"type":"string"},
            "context":{"type":"object","properties":{"sessionId":{"type":"string"}}}
        }});
        let error = check_tool("send_message", &schema).unwrap_err();
        assert!(error.contains("/context/sessionId"), "{error}");
    }

    #[test]
    fn renamed_role_sender_and_added_fieldless_arguments_fail() {
        assert!(check_tool(
            "send_role_message",
            &json!({"properties":{"sender":{"type":"string"}}})
        )
        .is_ok());
        assert!(check_tool(
            "send_role_message",
            &json!({"properties":{"from":{"type":"string"}}})
        )
        .is_err());
        for tool in ["mark_unread", "inbox_post", "send_to_session"] {
            // Even an unfamiliar name must not hide a new caller argument.
            assert!(
                check_tool(tool, &json!({"properties":{"from":{"type":"string"}}})).is_err(),
                "{tool}"
            );
            assert!(
                check_tool(tool, &json!({"properties":{"sender":{"type":"string"}}})).is_err(),
                "{tool}"
            );
        }
    }

    #[test]
    fn an_unclassified_tool_and_a_renamed_field_fail() {
        assert!(
            check_tool("new_unclassified_tool", &json!({"type":"object"}))
                .unwrap_err()
                .contains("unclassified")
        );
        assert!(check_tool(
            "send_message",
            &json!({"properties":{"renamedCaller":{"type":"string"}}})
        )
        .is_err());
    }
}
