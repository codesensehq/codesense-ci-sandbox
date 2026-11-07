# Examples

Skills, agents, routines and dashboards you can copy into your own team. Nothing here is installed or runs
on its own: CodeSense ships them only as examples.

Copy into your team's or Private `content/.claude/skills/` to use. A dashboard goes
anywhere in your content. To install a slot pane, write its complete HTML into the team's
`panes/` using `pane_write`, then reveal it with `pane_open`.

## Skills

- [`agent-design`](skills/agent-design/SKILL.md): discover work, propose a small team,
  preview, get approval and prove its handoffs.

- [`routine`](skills/routine/SKILL.md): set up and manage routines (an agent's saved,
  scheduled tasks: wake it or start a new one) and timers (temporary routines for one
  session).
- [`agent`](skills/agent/SKILL.md): set up and manage named, reusable agents.
- [`codesense-setup`](skills/codesense-setup/SKILL.md): walk through workspace, teams,
  repos, agents and routines.
- [`setup-machine`](skills/setup-machine/SKILL.md): get a remote machine ready for
  CodeSense over SSH, then test it.
- [`add-machine-user`](skills/add-machine-user/SKILL.md): give a teammate access to a
  machine.
- [`manage-team`](skills/manage-team/SKILL.md): run as the team's manager. Hold the
  `manager` role queue, triage, launch and track workers, report to you. See the
  [manager recipe](../setting-up-agents/manager-agent.md).
- [`context-sync`](skills/context-sync/SKILL.md): pull team context into a workspace, or
  publish its edits back after a preview, through the explicit context tools. See
  [Write your own sync skill](../arc-k-sync/write-your-own-sync-skill.html).
- [`worker-exit-contract`](skills/worker-exit-contract/SKILL.md): how a spawned Weft
  worker ends its task. Weft already gives every worker this text, so you only need a copy
  to read or adapt it.

None of these is required. The CodeSense MCP tools describe everything they take, so an
agent can do all of this without a skill. A skill only adds a shortcut such as `/routine`.

## Agents

- [Solo-founder starter](agents/solo-founder/README.md): planner, builder and reviewer,
  matching roles and one immediate role-queue wake.

Agent files that go with the [Setting up agents](../setting-up-agents/README.md) recipes.
Copy one to your team's `config/agents/<id>/agent.yaml`, with the skills it starts with.

- [`codesense-setup`](agents/codesense-setup/agent.yaml): runs `/codesense-setup`. See the
  [recipe](../setting-up-agents/codesense-setup-agent.md).
- [`machine-setup`](agents/machine-setup/agent.yaml): runs `/setup-machine` and
  `/add-machine-user`. See the [recipe](../setting-up-agents/machine-setup-agent.md).
- [`manager`](agents/manager/agent.yaml): runs `/manage-team`. See the
  [recipe](../setting-up-agents/manager-agent.md).

## Routines

A routine belongs to the agent it runs, so each example routine sits under its agent:
`agents/<agent>/routines/<id>/routine.yml`. Copy the whole agent folder (or just the
`routines/<id>/` folder) to your team's `config/agents/<agent>/`. They go with the
[manager recipe](../setting-up-agents/manager-agent.md).

- [`manager-wake`](agents/manager/routines/manager-wake/routine.yml): `mode: wake`. When one of
  the Manager's sessions sits idle with unread mail (`inbox_idle` trigger), wakes it to read
  and claim it. Other agents' mail never fires it. No second agent, no `send_to_session`.
- [`manager-ensure`](agents/manager/routines/manager-ensure/routine.yml): `mode: wake` on
  `machine: any`. Every hour, wakes the Manager wherever it runs to check its queue,
  and starts a Manager when none is running.

For something only one running session needs ("check the build in 20 minutes"), don't
write a routine: ask the agent to set a timer on its own session with `schedule_wakeup`. See
the [`routine`](skills/routine/SKILL.md) skill.

## Dashboards

- [`live-list-with-command.html`](dashboards/live-list-with-command.html): live sessions,
  a running-only filter chord and native row menus. Follow the
  [five-step pane recipe](../arc-i-dashboard-primitives/custom-pane-live-list.html) to
  save, open, verify and share it.

- [`sprint-analysis.html`](dashboards/sprint-analysis.html): sprint progress over your
  stories.
- [`kickoff-console.html`](dashboards/kickoff-console.html): open a story's workspace and
  move its status.
- [`agent-inbox-console.html`](dashboards/agent-inbox-console.html): who is running, each
  role queue and who holds it, and your inbox, live. Reply, mark read and set your status
  from it.
- [`setup-status.html`](dashboards/setup-status.html): a plain table of whether line attribution and
  turn-link capture work on this Mac, from `codesense.setup()`.
- [`story-board.html`](dashboards/story-board.html): a live board of open stories in an
  epic, one column per status, built on `codesense.subscribeContext`.

Reference dashboards that rebuild built-in panes with only the public bridge and the
dashboard kit (`window.codesense.kit` and the `cs-` classes every live page gets):

- [`agent-dag.html`](dashboards/agent-dag.html): every agent session as a live graph of
  who launched whom, with state, latest progress note, machine and unread mail, like the
  Agents tree.
- [`pipeline-graph.html`](dashboards/pipeline-graph.html): your stories as a dependency
  graph coloured by status, like the Pipeline view. Click a story to open it.
- [`routines-console.html`](dashboards/routines-console.html): every routine with its
  schedule and last run, its run history, and a run detail drawer with the result,
  transcript and captured output, like the Agents tab's Schedule view.

Each works as a right pane, a centre tab or an inbox message, follows the app's light or
dark theme, and shows a short notice on an app too old for a call it uses.

## Built-in panes

Generated, read-only source copies with a provenance comment: [Inbox](panes/inbox.html),
[Notes](panes/notes.html) and [agent list](panes/agent-list.html). Follow
[Customize the built-in panes](../arc-i-dashboard-primitives/customize-built-in-panes.html)
to fork, preview, select and revert them. Nothing in this examples directory installs itself.
