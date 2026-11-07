// SPDX-License-Identifier: MIT
pub(crate) mod activity;
pub(crate) mod agent_dot;
pub(crate) mod agent_tools;
pub(crate) mod avatar_tools;
mod canvas_scene;

/// A field that takes any JSON value. Its schema is the empty object `{}`: schemars
/// would otherwise emit the boolean schema `true` for a bare `serde_json::Value`, and
/// the MCP TypeScript SDK 1.23+ (OpenCode) rejects a `tools/list` containing one.
/// Use `#[schemars(with = "AnyJson")]` (or `Option<AnyJson>` / `Vec<AnyJson>`) on
/// such fields; `served_tool_schemas_have_no_boolean_property_schemas` enforces it.
pub(crate) struct AnyJson;

impl rmcp::schemars::JsonSchema for AnyJson {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AnyJson".into()
    }

    fn json_schema(_: &mut rmcp::schemars::SchemaGenerator) -> rmcp::schemars::Schema {
        rmcp::schemars::json_schema!({})
    }
}
pub(crate) mod feedback_tool;
pub(crate) mod live_data_authoring;
pub(crate) mod local_setup;
pub(crate) mod machine_tools;
pub(crate) mod notes_tools;
pub(crate) mod overview;
pub(crate) mod pane_tools;
pub(crate) mod records_tools;
pub(crate) mod remote_terminal;
pub(crate) mod schema_inline;
pub(crate) mod routine_tools;
pub(crate) mod run_tools;
pub(crate) mod task_tools;
pub(crate) mod setup_tools;
pub(crate) mod team_file_edits;
pub(crate) mod team_tools;
pub(crate) mod terminal_server_tools;
mod tool_envelope;
mod dispatch;
// Crate-visible so the signal-done bridge Tauri command
// (`handler::wv_signal_done_bridge`) can be named in lib.rs `generate_handler!`.
pub(crate) mod handler;
pub(crate) mod identity_tools;
pub(crate) mod session_list;
mod socket_server;
pub mod state;
pub(crate) mod story_authoring;
pub(crate) mod terminal_tabs;
pub(crate) mod workspace_chat_tools;
pub(crate) mod worktree_tools;

pub(crate) use socket_server::serve_stream;
pub use socket_server::start_mcp_server;
pub use socket_server::{mcp_socket_bound_here, mcp_socket_path};

pub(crate) mod outbox_once;
pub(crate) mod remote_authority;

pub(crate) mod context_query_tool;
pub(crate) mod context_tools;

pub(crate) mod command_tools;
pub(crate) mod move_workspace;
pub(crate) mod remote_destination;
#[cfg(unix)]
pub(crate) mod remote_executor;
pub(crate) mod remote_story_workspace;
pub(crate) mod remote_workspace;
pub(crate) mod remote_workspace_target;
pub(crate) mod reorder_workspace;

pub(crate) mod remote_workspace_bundle;

pub(crate) mod remote_workspace_fetch;

pub(crate) mod workspace_base;

pub(crate) mod local_identity;
pub(crate) mod local_mcp_relay;

#[cfg(unix)]
pub(crate) mod remote_agent_task;
#[cfg(not(unix))]
#[path = "remote_agent_task_unavailable.rs"]
pub(crate) mod remote_agent_task;

pub(crate) mod context_operations;
pub(crate) mod flat_params;

pub(crate) mod nav_lens;

mod request_id;

pub(crate) mod local_tools;

pub(crate) mod session_status;

pub(crate) mod workspace_groups;

pub(crate) mod caller_tools;
#[cfg(test)]
mod connection_tests;

#[cfg(test)]
mod identity_guard;
mod reference;
