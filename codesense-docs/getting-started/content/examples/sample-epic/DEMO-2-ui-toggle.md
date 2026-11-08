---
title: "DAG node kill button + confirmation modal"
type: story
story_id: DEMO-2
status: review
priority: p2
sub_epic: per-agent-kill-switch
parent_epic: DEMO
stream: frontend
depends_on: [DEMO-1]
source_repo: acme/sample-source-repo
sprint: FAKE-S1
points: 2
assigned_to: reviewer
---

## Purpose

Add a kill button on every running agent node in `agent-dag.html`. Click → modal
confirms "Kill agent `{id}`? Any uncommitted work will be lost." → confirm invokes
the MCP wrapper from DEMO-1.

Reduced-motion-safe: the button is a plain icon, no spinner, no attention animation.
Keyboard accessible: Tab lands on it, Space/Enter triggers, Esc dismisses the modal.

## Acceptance Criteria

- AC-1 Running agent nodes get a kill button; stopped nodes do not.
- AC-2 Click opens a confirm modal with the agent id and the warning copy above.
- AC-3 Confirm invokes `kill_agent` via `window.codesense.invoke` and shows a loading
  state until the host returns.
- AC-4 Cancel or Esc closes the modal, leaves the agent running.
- AC-5 Accessible: focus returns to the kill button on cancel; focus moves to the
  confirm button on open.

## Depends on

- `DEMO-1` — needs the host primitive and the MCP wrapper.
