# Skills are plain files

_A skill is a file in your team context. Copy an example, edit it, and the next agent you launch gets it._

A **skill** is just a file: a folder under `.claude/skills/` with a
`SKILL.md` inside. There is nothing to install. Put it in your team's
`content/` folder, and an agent whose `context` includes it gets it as a
file in its worktree when it launches, like any other context file.

## Write one

From Ledgerline's `billing` team:

```md
---
name: write-migration
description: Write a forward-only Postgres migration with a backfill plan.
---
Create `api/migrations/<timestamp>_<name>.sql`. Never drop a column in the same
release that stops writing it. Include a backfill query and an index plan.
```

Saved as `billing/content/.claude/skills/write-migration/SKILL.md`, it shows
up as `.claude/skills/write-migration/SKILL.md` in the worktree of the next
Claude Code agent you launch for `billing`: the Claude Code default `context`
includes `.claude/skills`. List it in the agent's `startup` too if the agent
can't work without it: a missing `startup` entry blocks the launch. A Codex agent's default reads `.agents/skills`
instead, so add `.claude/skills` to its `context` or keep a copy there.
An agent that's already running picks it up only when it calls
`hydrate_worktree`; nothing is pushed into a running worktree on its own.

Child teams inherit it when your team shares it in `config/sharing.yml`. Inheritance works per file: a child team that writes
its own `.claude/skills/write-migration/SKILL.md` replaces that one file, and
every other skill in the folder still comes from the parent (see
[Teams and inheritance](teams-and-inheritance.md)).
Step by step: [Write a skill once for every repo](../guides/skill-once.md).

A skill you write and one an agent writes for you are the same thing: plain
content your team edits, reviews and shares through git like everything else.

## Start from an example

The CodeSense docs repo comes bundled with every install, and its
`getting-started/content/examples/` folder holds copyable examples:

- `skills/routine/`, `skills/agent/`, `skills/manage-team/` and
  `skills/codesense-setup/`: skills that help an agent author routines,
  agents, teams and your setup.
- `skills/context-sync/`: brings team context into a worktree, or sends
  edits back after a preview.
- `skills/setup-machine/` and `skills/add-machine-user/`: set up a remote
  machine.
- `dashboards/`: ready-made HTML pages, such as a sprint analysis, a story
  board and a kickoff console.

Copy the folder you want into your team's `content/.claude/skills/` (or a
page into a pane, below) and edit it. The examples are inert where they
sit: nothing loads them until you copy them.

> **Nothing depends on a skill**
>
>   CodeSense's MCP tool descriptions carry everything an agent needs to
>   use them, so no workflow breaks because a skill is missing.

## Where the other pieces live

- **Agents** carry their own args, MCP servers, permissions and startup in
  their `agent.yaml`. See [Agents and runtimes](agents-and-profiles.md).
- **Routines** run scheduled or event-driven agent work; they replace the
  old automations.
- **Panes** are HTML files at `<space>/panes/<id>.html`, next to `content/`
  and `config/`. They show in the right pane, hot-reload as you edit them,
  and reach teammates through git like `config/`. Agents write and open them with the
  [pane tools](mcp-tools.md#pages-panes-and-canvases).
- **Claude Code, Codex and OpenCode** are built into the app; CodeSense knows how to
  launch each CLI. OpenCode runs on your Mac only.

The full guide ships in the app's docs: **Agents → Skills**.
