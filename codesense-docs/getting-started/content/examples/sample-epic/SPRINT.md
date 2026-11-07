---
title: "Sprint FAKE-S1 — Per-agent kill switch"
type: sprint
sprint_id: FAKE-S1
status: active
focus_epics: [DEMO]
goal: "Ship the per-agent kill switch end-to-end."
start: "2026-10-05"
end: "2026-10-19"
source_repo: acme/sample-source-repo
author: "Seeded demo."
---

## Sprint plan

Status distribution is deliberate — one story in each state so the shipped data
dashboards (`sprint-analysis.html`, `story-board.html`, `pipeline-graph.html`) render
non-empty on first launch.

| Story | Status | Points | Notes |
|---|---|---|---|
| `DEMO-1-foundation.md` | done | 3 | Host kill ladder shipped on day 2. |
| `DEMO-2-ui-toggle.md` | review | 2 | PR open, awaiting Winston's review. |
| `DEMO-3-mcp-command.md` | in-progress | 2 | Schema in flight. |
| `DEMO-4-persistence.md` | todo | 2 | Picked up as soon as DEMO-3 ships. |
| `DEMO-5-dag-rerender.md` | blocked | 3 | Awaiting a UX call (see the story). |

Committed: 12 points. Done: 3. Delivered on target so far.

## What's new in this sprint

- Reuses the live `agent-dag` dashboard's subscribe seam — no new context primitives
  needed.
- Adds one new MCP tool (`kill_agent`) and one small schema (the kill hold flag).
- Produces the first "per-node action with host side effects" pattern. The team's
  recipe gallery captures it as a reusable shape.

## Risks

- The DAG rerender under `prefers-reduced-motion` is still open (DEMO-5 blocked).
- No remote-machine kill ladder today: DEMO-1 is local-only. A follow-up story
  captures the parity gap and is deferred to the next sprint.
