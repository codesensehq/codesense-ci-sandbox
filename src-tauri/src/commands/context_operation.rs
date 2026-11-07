// SPDX-License-Identifier: MIT
//! SW-XS-10: Stop, from the workspace's operation chip, for a running
//! explicit context operation (`hydrate_worktree`, `hydrate_story`,
//! `reverse_sync_worktree`). The same cancel the `cancel_context_operation`
//! MCP tool asks for: a hydrate stops with no partial file left behind, a
//! publish stops before it writes team content, and the operation's own
//! terminal progress event says `cancelled`.

/// Ask the running operation `operation_id` to stop. `true` when it was
/// running here (its `cancelled` event follows); `false` when it had already
/// ended or is not this app's.
#[tauri::command]
pub fn cancel_context_operation(operation_id: String) -> Result<bool, String> {
    let id = uuid::Uuid::parse_str(&operation_id)
        .map_err(|_| "operationId must be a UUID".to_string())?
        .to_string();
    Ok(crate::mcp::context_operations::explicit::cancel(&id))
}

/// Replace `paths` in the workspace `target` with the team's version
/// (`hydrate_worktree {paths, overwriteModified:true}`): the one recovery for
/// files kept because they were modified. Files the repo tracks are never
/// written. Returns the operation's result (written / kept / failed), and its
/// progress arrives on the operation chip like any explicit operation.
#[tauri::command]
pub(crate) async fn discard_context_edits(
    app: tauri::AppHandle,
    target: crate::mcp::context_operations::WorkspaceTarget,
    paths: Vec<String>,
    operation_id: Option<String>,
) -> Result<serde_json::Value, String> {
    // The engine checks each path is one exact file (never a folder, never
    // trimmed) against the workspace's managed record before writing.
    let op = crate::mcp::context_operations::explicit::Op::Discard {
        paths,
        expect: Default::default(),
    };
    crate::mcp::context_operations::explicit::run(app, target, None, op, operation_id, None, None)
        .await
        .map_err(|e| e.message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xs10_cancel_refuses_an_id_that_is_not_a_uuid() {
        assert_eq!(
            cancel_context_operation("op-1".into()),
            Err("operationId must be a UUID".into())
        );
    }

    #[test]
    fn xs10_cancel_of_an_operation_not_running_here_is_false() {
        assert_eq!(
            cancel_context_operation("9b2f8f0e-6a52-4c39-9d43-3c2a0c9f2a11".into()),
            Ok(false)
        );
    }
}
