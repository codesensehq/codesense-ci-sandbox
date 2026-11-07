// SPDX-License-Identifier: MIT
//! Session → workspace resolver.
//!
//! The one invariant that makes "sender bound to workspace W" testable: a
//! session's workspace binding is a read against the launch registry, not an
//! independent write. The pure function here takes the few bits of a launch
//! entry it needs (so it is unit-testable without any Tauri state) and the
//! `DashboardLaunchState::workspace_for_session` method is a thin wrapper over
//! it (see `commands/dashboard_launch.rs`).
//!
//! Normalization order (per
//! `_bmad-output/21-workspace-chat/design/engine-architecture.md` §4):
//!   1. `meta.story` → `story:<id>` (every story worker has one).
//!   2. `worktree_dir` → `generic:<path>` (every launch with a managed worktree
//!      has one; the pty-layout `workspace_key` composes this with the team key
//!      for its own purposes, but the chat engine keys by workspace alone).
//!   3. Otherwise `None` — a session with neither story nor worktree is NOT
//!      bound to a workspace, and the admission check refuses it with
//!      `invalid_params("session not bound to a workspace")`.

use std::path::Path;

/// Normalize a (story, worktree_dir) pair into the chat engine's workspace id.
///
/// This matches the viewer-id form every other Tauri surface uses
/// (`OpenTerminalTabParams.workspace_id`): `story:<id>` for a story launch,
/// `generic:<path>` for a plain managed-worktree launch, `None` for a session
/// with neither.
///
/// Normalization rules:
/// - Trim surrounding whitespace and drop empty values.
/// - `story:` wins over `generic:` when both are present (a story launch always
///   has a worktree, but the story id is the stable key).
/// - A `worktree_dir` is rendered via its lossy UTF-8 form; a NUL / newline in
///   a path (never valid on either macOS or Linux filesystems in practice) is
///   rejected rather than silently normalized.
pub fn session_workspace_id(story: Option<&str>, worktree_dir: Option<&Path>) -> Option<String> {
    if let Some(id) = story.and_then(valid_segment) {
        return Some(format!("story:{id}"));
    }
    if let Some(dir) = worktree_dir {
        let rendered = dir.to_string_lossy();
        if let Some(path) = valid_segment(rendered.as_ref()) {
            return Some(format!("generic:{path}"));
        }
    }
    None
}

pub fn remote_workspace_id(story: Option<&str>, workspace: &str) -> Option<String> {
    session_workspace_id(story, None)
        .or_else(|| valid_segment(workspace).map(|id| format!("remote:{id}")))
}

pub fn descriptor_workspace_id(
    descriptor: &codesense_pty_core::layout::WorkspaceDescriptor,
) -> Option<String> {
    match descriptor.kind.as_str() {
        "story" => session_workspace_id(
            descriptor.story_id.as_deref(),
            descriptor.worktree_path.as_deref(),
        ),
        "generic" | "epic" => session_workspace_id(None, descriptor.worktree_path.as_deref()),
        _ => None,
    }
}

/// Accept a trimmed, non-empty, control-char-free segment of a viewer id.
fn valid_segment(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().any(|c| c == '\n' || c == '\r' || c == '\0') {
        return None;
    }
    Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn story_wins_over_worktree() {
        let dir = PathBuf::from("/tmp/wt");
        assert_eq!(
            session_workspace_id(Some("ex-1"), Some(&dir)).as_deref(),
            Some("story:ex-1"),
        );
    }

    #[test]
    fn worktree_dir_becomes_generic_prefix() {
        let dir = PathBuf::from("/tmp/some/worktree");
        assert_eq!(
            session_workspace_id(None, Some(&dir)).as_deref(),
            Some("generic:/tmp/some/worktree"),
        );
    }

    #[test]
    fn neither_story_nor_worktree_resolves_to_none() {
        assert_eq!(session_workspace_id(None, None), None);
        assert_eq!(session_workspace_id(Some("  "), None), None);
        assert_eq!(session_workspace_id(Some(""), None), None);
    }

    #[test]
    fn control_chars_are_rejected() {
        assert_eq!(session_workspace_id(Some("ex\n1"), None), None);
        assert_eq!(session_workspace_id(Some("ex\x001"), None), None);
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(
            session_workspace_id(Some("  ex-1  "), None).as_deref(),
            Some("story:ex-1"),
        );
    }
}
