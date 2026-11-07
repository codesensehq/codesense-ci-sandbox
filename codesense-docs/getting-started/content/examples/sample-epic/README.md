---
title: "Epic DEMO — Per-agent kill switch for agent-dag"
type: epic
epic_id: DEMO
status: in-progress
source_repo: acme/sample-source-repo
repos:
  - acme/sample-source-repo
author: "Seeded demo — a tiny fictitious epic that every data dashboard renders against on first launch."
---

## Purpose

The `agent-dag.html` dashboard shows every running agent in a live DAG. A long-running
agent that has gone off the rails is hard to stop from the DAG: you have to open the
Agents panel, find the row, click Stop. This epic adds a kill switch directly on the
node — one click to kill the agent (gracefully, then SIGKILL after a short grace
period) with full confirmation + undo.

This is intentionally a small, UI-centric feature. It touches the frontend (the DAG
node renderer), the MCP surface (a new `kill_agent` tool so routines can reach the
same affordance), the host (SIGTERM → SIGKILL ladder on the agent process group), and
the dashboard re-render path (so the node transitions through "stopping" to "stopped"
without a full refresh).

Five stories, one chain. DEMO-1 lays the foundation (the kill primitive at the host
layer and the MCP wrapper). DEMO-2..5 are the UI, the persistence, and the DAG
rerender — all siblings that depend on DEMO-1 only, so they ship in parallel.

## Repos

- `acme/sample-source-repo` — a tiny React + Rust mini-codebase that stands in for
  CodeSense itself for the purposes of the demo. The committed `.git` ships five
  commits, one per story, each with a meaningful diff.

## Stories

- `DEMO-1-foundation.md` — host kill ladder + MCP wrapper
- `DEMO-2-ui-toggle.md` — DAG node kill button + confirmation modal
- `DEMO-3-mcp-command.md` — expose `kill_agent` through the public MCP surface
- `DEMO-4-persistence.md` — remember a killed agent across relaunches
- `DEMO-5-dag-rerender.md` — transition the node through "stopping" without a reload

## Sprint

See `SPRINT.md`. Status is deliberately spread across the full taxonomy so every
dashboard renders non-empty on first launch.
