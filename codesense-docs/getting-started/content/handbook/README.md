# CodeSense docs

_How CodeSense works: context for every agent, plans with proof, and dashboards you author._

CodeSense is a macOS app for teams that build software with coding agents: your team's context, plans and dashboards as plain files in a git repo, put to work in every repo and worktree your agents touch. These docs describe what the app does today.

## Browse by topic

- **Context:** [The context repo](concepts/context-repo.md), [Teams and inheritance](concepts/teams-and-inheritance.md), [Context in a worktree](concepts/agent-context.md).
- **Work:** [The planning model](concepts/planning-model.md), [Agents and profiles](concepts/agents-and-profiles.md), [Remote machines](concepts/remote-machines.md).
- **Coordinate:** [The Inbox](concepts/inbox.md), [Routines](concepts/routines.md).
- **Build on the engine:** [Panes and slots](concepts/panes-and-slots.md), [Dashboards and pages](concepts/dashboards.md), [Dashboard uses](guides/dashboard-uses.md).
- **Team and trust:** [Attribution and cost](concepts/attribution-and-cost.md), [Sharing through git](concepts/sharing.md), [Security and privacy](concepts/security-and-privacy.md).
- **Reference:** MCP tools, Live-data API and [dashboard kit](../generated/page-sdk.md), [Story fields](../generated/story-fields.md), pipeline.yml, config files, context queries, [Key terms](getting-started/key-terms.md).

## Start here

- [Install CodeSense](getting-started/install.md): get the app running
  and its dependencies checked.
- [Your first hour](getting-started/first-hour.md): from a fresh install
  to agents working on stories in your own repos.
- [Create or connect a context repo](getting-started/workspace.md): start
  your team's context, or connect the one it already shares.
- [Key terms](getting-started/key-terms.md): org, team, context,
  worktree, epic, story and the rest, in one page.

## The three things CodeSense does

**Context that follows every agent.** Write your team's `CLAUDE.md`,
`AGENTS.md`, skills and docs once, in your team context. When an agent
launches in a worktree, CodeSense copies in the context its definition asks
for as real files, hidden from git, and the agent can ask for more with a
tool call. Reverse sync, in Beta, lets an agent bring edits made in a
worktree back.
Read [The context repo](concepts/context-repo.md),
[Teams and inheritance](concepts/teams-and-inheritance.md) and
[Context in a worktree and reverse sync](concepts/agent-context.md).

**Plans agents can execute.** Epics are folders and stories are markdown or
HTML files with a few metadata fields. CodeSense tracks their status and
dependencies, and gives each story its own worktree and branch. Status is a
plain field: you or your agents move it, and nothing gates the move.
Read [The planning model](concepts/planning-model.md) and
[Worktrees](concepts/worktrees.md).

**Dashboards you author.** HTML pages in your context repo become live
dashboards over your stories, your agents' sessions and their routine results.
Read [Dashboards and pages](concepts/dashboards.md).

## Popular tasks

- [Write a skill once for every repo](guides/skill-once.md)
- [Override a parent team's skill](guides/override-parent-skill.md)
- [Keep your own Claude Code hooks](guides/keep-your-hooks.md)
- [Run your own tools and scripts in a worktree](guides/your-own-tools.md)
- [Write your first story](guides/first-story.md)

## The sample project

Examples across these docs come from **Ledgerline**, a made-up billing
product with two repos, `ledgerline/web` and `ledgerline/api`, and two teams:
`platform`, and `billing`, which inherits from `platform`.

The full training manual ships in the app's docs, on every install, and your agents can read it too.

## Walkthroughs

Each published one is recorded by an agent driving the real app, with a demo team, demo repos, AI narration and captions, and each opens the guide it belongs to.

1. [Install and first run](getting-started/install.md): 1 min
2. [Your first context repo](concepts/context-repo.md): 1 min
3. [Two axes: team layers and per-repo context](concepts/teams-and-inheritance.md): 1 min
4. [What lands in a worktree](concepts/agent-context.md): 1 min
5. [Stories, epics and many repos](concepts/planning-model.md): 1 min
6. [Agents are files your team shares](concepts/agents-and-profiles.md): 1 min
7. [Remote machines without your git keys](concepts/remote-machines.md): 6–8 min
8. [Inbox, Needs you and role queues](concepts/inbox.md): 5–6 min
9. [Work that starts itself](concepts/routines.md): 1 min
10. [Build a team pane](concepts/panes-and-slots.md): 1 min
11. [Dashboards on the engine](concepts/dashboards.md): 1 min
12. [Every line traced, every story costed](concepts/attribution-and-cost.md): 5–7 min
13. [A coworker on day one](concepts/sharing.md): 4–5 min
14. [What leaves your Mac](concepts/security-and-privacy.md): 5–6 min

## In the app, too

These docs ship inside CodeSense. Every install places them on your Mac as a docs context repo, so you can read them in the app, offline, and your agents read them through MCP. Ask an agent how routines work, or which tool opens a workspace, and it answers from the same pages, with an example call for every tool.

> **Tip from the builder**
>
> Ask me “how do I arm a routine on build-01?” and I’ll answer from the docs I already have, with the call.
