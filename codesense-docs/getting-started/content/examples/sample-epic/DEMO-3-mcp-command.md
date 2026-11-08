---
title: "Public MCP tool: kill_agent"
type: story
story_id: DEMO-3
status: in-progress
priority: p1
sub_epic: per-agent-kill-switch
parent_epic: DEMO
stream: backend
depends_on: [DEMO-1]
source_repo: acme/sample-source-repo
sprint: FAKE-S1
points: 2
assigned_to: builder
---

## Purpose

Promote the internal kill wrapper from DEMO-1 to the public MCP surface so a
routine, a dashboard, or a human-driven agent in a terminal can kill another agent.

The public tool reuses the DEMO-1 wrapper; this story is schema + registration +
docs. Error vocabulary stays small: `AlreadyExited`, `NotFound`, `PermissionDenied`
(a non-admin agent cannot kill an admin agent).

## Acceptance Criteria

- AC-1 `kill_agent(session_id: string, grace_ms?: u32)` registered in the MCP handler
  with a stable schema. `grace_ms` defaults to the agent's `graceful_shutdown_ms`.
- AC-2 Error cases return the three listed codes with human-readable detail.
- AC-3 MCP reference generator picks up the new tool automatically (no manual edit).
- AC-4 A fixture scopes test proves the tool is callable from both the UI bridge and
  a running agent's own MCP session.

## Depends on

- `DEMO-1` — the wrapper this story promotes.
