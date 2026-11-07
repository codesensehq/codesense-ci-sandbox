---
title: "Host kill ladder + MCP wrapper"
type: story
story_id: DEMO-1
status: done
sub_epic: per-agent-kill-switch
parent_epic: DEMO
stream: fullstack
depends_on: []
source_repo: acme/sample-source-repo
sprint: FAKE-S1
points: 3
author: "Seeded demo. The foundation every other DEMO story depends on."
---

## Purpose

Add the host-side primitive that kills a running agent process group and the internal
MCP wrapper that exposes it to callers.

Graceful first: SIGTERM, wait 3 s, SIGKILL. The grace period is configurable per
agent via `agent.yaml` (`graceful_shutdown_ms`). An absent setting falls back to the
host default (3 000 ms).

## Acceptance Criteria

- AC-1 `host::kill_agent(session_id, grace_ms)` sends SIGTERM to the process group, waits up to
  `grace_ms`, then sends SIGKILL. Returns the exit status actually observed.
- AC-2 Called with an already-dead session: no-op; returns `AlreadyExited`.
- AC-3 Internal MCP tool `kill_agent` wraps the host call. Not yet in the public
  surface (DEMO-3 handles that).
- AC-4 Integration test spawns a sleep loop and asserts the kill completes within
  grace_ms + 500 ms slack.

## Depends on

Nothing. This is the foundation story.
