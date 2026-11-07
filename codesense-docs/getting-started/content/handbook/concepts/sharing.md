# Sharing through git

_The status button, changes, conflicts and history: how your team shares context through git, with agents doing the git._

Your team's context repo is a git repo with a remote, so sharing works the
way git does: you, or an agent you ask, commit and push, teammates pull, and
history records who changed what. CodeSense never commits, pushes, pulls or
merges it. It shows you what's changed, what's incoming and what happened,
and hands the git to an agent when you ask.

This applies to a **shared** team. A local-only team has no remote
to share through. See [Create or join a team](../getting-started/workspace.md).

## See what you've changed

The left nav's scope row has the context repo's status button, `⎇ branch | ↑N ↓M`.
**↑** counts changes GitHub doesn't have yet (edits and unpushed commits),
**↓** counts what's new on GitHub, and a third count appears for conflicts.
With nothing to report it reads **synced**. Hover it for the same thing in a
sentence, including when the clone last heard from GitHub.

Click it to open the **Changes** view: the changed files, in the tree.
Opening a changed markdown file shows a **redline**: prose diffs marked up
the way you'd review a document, with a before/after toggle.

Edits an agent made in a workspace and hasn't sent to the team yet are
counted separately, as **N unsaved** in the same row. Its **Publish** sends a
file into the team's `content/` (not to GitHub), and **Discard** drops the
edit.

## Get your teammates' changes

Nothing is pulled for you. When **↓** shows changes waiting, pull the context
repo with git, or ask an agent to.

If a file changed on both sides, the status button opens a card for it with
**Use mine**, **Use theirs**, **Keep both**, **Hand to ‹agent›** and
**Compare**. Settling a file writes the side you chose and stages it; the
agent doing the sync finishes the merge. A failed sync or an expired GitHub
sign-in shows on the button too, with **Retry**, **Sign in again** or
**Set up**: each asks one of the team's agents to fix it.

Once pulled, the new context is in your team's `content/`. Worktrees get it
the next time an agent launches there, or when a running agent calls
`hydrate_worktree`.
See [Context in a worktree and reverse sync](agent-context.md).

## Look back at history

Choose **History** from the left nav's view menu for a graph of the context
repo's commits and the files each one changed.

## Share your changes

Your context repo is an ordinary git repo, so you share a change with git
from its folder, or ask any agent to do it for you:

```sh
cd ~/code/codesense-context   # wherever you cloned it
git add billing/content/.claude/skills/write-migration
git commit -m "billing: add write-migration skill"
git push
```

An agent that does the git can call `report_context_sync` to say how it
went, so the status button shows it. Teammates then see the change as
incoming, and it reaches their worktrees once they pull.

> **Coming: agent-managed sharing**
>
>   Agents do the git, not you. An agent picks up your change and
>   commits it, or opens it for review, as your team's policy says, and
>   asks you when no policy covers it. Every teammate's CodeSense then pulls
>   the update automatically while it's running.

The full guide ships in the app's docs: **Sync → Agent-driven git**.

## Invite a coworker

A coworker adds the same context repo. CodeSense never commits, pushes or merges for you, so sharing a change with the team is an ordinary git push.

1. **Give them access** to the context repo on your git host, the way you would for any repo.
2. **They install CodeSense** and sign in to their own CLIs with their own accounts.
3. **They add the context repo**, and their Mac syncs it like any clone.

## What they get

They start with your whole agent setup: agents, skills, stories, panes and dashboards, ready on day one.

- **Agents and routines.** Every saved agent, launched with their own login.
- **Skills and rules.** The team’s `CLAUDE.md`, `AGENTS.md` and skills, mirrored into their worktrees.
- **Stories and plans.** The same pipeline, sprints and dependencies.
- **Panes and dashboards.** Every pane in the team’s `panes/` folder, in their app.

Their private layer starts empty and stays theirs. Routines meant to run once for the team keep running where they are; a routine set to run on each Mac waits until they turn it on.

## Child teams

A team can have a parent team, so company rules live once at the top and each team adds its own.

```yaml title="teams.yml"
teams:
- name: platform           # company rules, once
  source_repos: []
- name: web
  parent_team: platform
  source_repos:
  - owner: acme
    repo: checkout
    host: github.com
- name: api
  parent_team: platform
  source_repos:
  - owner: acme
    repo: api
    host: github.com
```

Both teams get platform’s rules. Each adds or overrides its own, and an epic can still span both teams’ repos.

## Sharing outside the team

Publish a dashboard to share.codesense.ai and invite viewers by email; you can see and end every device that opened it. [Dashboards](dashboards.md#author-and-share) shows how.

share.codesense.ai is for invited viewers only: there is no open sign-up.

> **Coming: the engine on your own servers**
>
> The engine on your own servers: the same sharing from your own server, reached through your own private network, so company data stays on company machines.
