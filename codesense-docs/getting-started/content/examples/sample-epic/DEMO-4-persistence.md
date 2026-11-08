---
title: "Remember a killed agent across relaunches"
type: story
story_id: DEMO-4
status: todo
priority: p3
sub_epic: per-agent-kill-switch
parent_epic: DEMO
stream: backend
depends_on: [DEMO-1]
wave: 3
source_repo: acme/sample-source-repo
sprint: FAKE-S1
points: 2
assigned_to: planner
---

## Purpose

A killed agent must not be auto-restarted by an `each_mac` routine or an `inbox_idle` trigger
until a human acknowledges the kill. Add a `killed_until_ack` flag to the
agent's session record. The dashboard shows a small banner on the node ("Killed,
click to clear").

## Acceptance Criteria

- AC-1 Killing an agent sets `killed_until_ack = true` on its session record (durable
  across relaunch).
- AC-2 A routine that would otherwise wake the agent skips it while the flag is set
  and logs the skip reason.
- AC-3 Clearing the flag (via the DAG banner or `clear_kill_hold` MCP tool) resumes
  normal wake behavior.
- AC-4 Flag is scoped per machine (a killed-on-local agent is still runnable on a
  remote machine).

## Depends on

- `DEMO-1` — needs the kill primitive to set the flag.
