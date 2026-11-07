---
name: context-sync
description: Move context between this workspace and team content on request. Pull team files into the workspace, or publish the workspace's edits back after a preview. Use when the user says "pull the latest skills", "bring in this story's epic", "publish my edits to the team", "what would publishing change", or "add this file to team context". Example from the CodeSense docs — when you copy it into your context repo's `.claude/skills/`, delete this sentence and adapt the rest. The CodeSense MCP tools work without it.
---

# Context sync

CodeSense never syncs on its own. A workspace gets context when it is created and when an
agent starts in it; anything more happens only when you call one of these tools. This skill
just picks the right one and always previews before it writes team content.

`hydrate_worktree`, `hydrate_story`, `preview_reverse_sync_worktree`,
`reverse_sync_worktree` and `list_hydrated` take a `target`. On This Mac:
`target: {"kind": "local", "codeRepoPath": "$PWD", "sessionId": "$CODESENSE_TAB_SESSION_ID"}`.
On a machine: `{"kind": "remote", "machineId": "…", "workspaceHandle": "…"}`.
Routing takes paths instead (This Mac only), and `context_operation_status` and
`cancel_context_operation` take an `operationId`; both are below.

## Pull team context in

- Named files or folders: `hydrate_worktree` with `paths` (exact files) or `sets` (folders
  such as `".claude/skills"`). It adds what is missing and refreshes what nobody edited.
  A file you edited is kept and reported, never overwritten.
- A story's epic: `hydrate_story` with `storyId`.
- Only pass `prune: true` when the user asks to remove copies the workspace no longer
  wants. It acts only under the `paths`/`sets` of that call (with neither, the runtime
  defaults: `CLAUDE.md`, `.claude/{commands,hooks,agents,skills}`, `AGENTS.md`,
  `.agents/skills`) and never removes an edited file.
- Only pass `overwriteModified: true` when the user asks to discard their edits to those
  files, and always with `paths` naming exactly those files.

## Publish edits back

1. Call `preview_reverse_sync_worktree` with the same `target`, `paths` and `sets` you
   will publish. Show the user the entries, conflicts and `withheldRemovals`.
2. Ask before publishing. On a yes, call `reverse_sync_worktree` with the same inputs.
3. A conflict is never written: the team file changed since it was copied. To resolve one:
   1. Save the workspace's version of the file somewhere outside the workspace.
   2. Call `hydrate_worktree` with `paths: [that file]` and `overwriteModified: true`. This
      takes the team's version and makes it the new base.
   3. Re-apply the saved edit on top of it.
   4. Preview again, then publish.

Team content is never deleted unless you pass `includeDeletes: true`. Pass it only when the
user asked to remove those team files and the preview showed them.

## Add one new file to team context

This Mac only. `route_file_to_context` with `codeRepoPath` (`$PWD`), `codeRepoSlug`
(`owner/repo`) and `logicalName` (the repo-relative path). It copies the file into team
content and hides it from git. `unroute_file_from_context`, with the same three, undoes it.

## Long operations

`hydrate_worktree`, `hydrate_story` and `reverse_sync_worktree` accept an `operationId` (a
UUID): calling again with the same one returns the kept result, so a retry is safe.
`context_operation_status` and `cancel_context_operation` take that `operationId`; a
cancel stops the operation without leaving a partial file. `list_hydrated` shows what
CodeSense copied into this workspace.
