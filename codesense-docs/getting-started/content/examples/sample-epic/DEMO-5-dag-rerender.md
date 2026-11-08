---
title: "DAG transitions through 'stopping' without a reload"
type: story
story_id: DEMO-5
status: blocked
priority: p2
blocked_reason: "Waiting on a design decision: animate the stopping state, or just swap the colour?"
sub_epic: per-agent-kill-switch
parent_epic: DEMO
stream: frontend
depends_on: [DEMO-1]
source_repo: acme/sample-source-repo
sprint: FAKE-S1
points: 3
assigned_to: builder
---

## Purpose

When an agent is killed, the DAG node transitions through a `stopping` state (the kill
button becomes a spinner) and settles on `stopped`. No page reload: the dashboard
subscribes to session state via `window.codesense.subscribeContext` and re-renders on
each state change.

## Acceptance Criteria

- AC-1 On confirm, the node immediately shows `stopping` (kill button → spinner).
- AC-2 When the host returns `AlreadyExited`, the node settles on `stopped` within
  one subscribeContext tick.
- AC-3 Edge list is updated: edges from the killed node become dashed to indicate
  the downstream work is now ownerless.
- AC-4 Prefers-reduced-motion disables the spinner (replaced with a plain text label).

## Depends on

- `DEMO-1` — needs the state transitions emitted by the kill primitive.

## Blocker

The design review hasn't settled whether the stopping transition is
colour-only (grey → red) or includes a slight pulse. Treating as blocked until the
designer rules; code is scaffolded in a branch waiting for the directive.
