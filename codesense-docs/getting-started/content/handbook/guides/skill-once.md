# Write a skill once for every repo

_Author a skill in your team context once, and every Claude Code agent you launch in any of the team's repos gets it._

You'll add a skill to your team's context once, and every Claude Code agent
you launch in any repo the team works in gets it (its default context includes
`.claude/skills`; another agent gets it when its `context` includes that
folder). Then you'll improve it from inside a
worktree and have the agent send the change back.

The example adds a `release-notes` skill to Ledgerline's `billing` team,
which works in `ledgerline/web` and `ledgerline/api`.

**You need:** a team with at least one attached repo
([Attach your repos](../getting-started/attach-repos.md)), and a story you
can launch an agent on ([Launch your first agent](../getting-started/first-agent.md)).

## 1. Write the skill in your team context

In CodeSense's file tree for the `billing` team (or in any editor), create
`.claude/skills/release-notes/SKILL.md` in the team's content folder:

```md
---
name: release-notes
description: Draft customer-facing release notes from the done stories in an epic.
---
Read done stories, group by area, write one plain-language line each.
No internal ids in customer copy.
```

On disk that's
`billing/content/.claude/skills/release-notes/SKILL.md` in the context repo.

Saving it changes only the context repo. Nothing is copied into a worktree
yet.

## 2. Launch an agent and find it

Launch a Claude Code agent on a story. Its default `context` includes
`.claude/skills`, so the launch copies the new skill into the story's
worktree with the rest of its context. Then, in that worktree's terminal:

```sh
ls .claude/skills/release-notes
# SKILL.md
git status --short
# nothing: the skill is excluded from git
```

Launch an agent on a `ledgerline/web` story and the same file is there too.
One file in the context repo, for every agent whose context includes it, in either repo.

> **Agents that are already running**
>
>   A running agent doesn't see the new skill until it asks. Tell it to call
>   `hydrate_worktree`, or put that call in a sync skill of your own (the
>   CodeSense docs ship an example to copy, `examples/skills/context-sync/`).
>   For a Codex agent, add `.claude/skills` to its `context`: the Codex
>   default reads `.agents/skills`.

## 3. Improve it from inside a worktree

> **Beta**
>
>
> Say an agent working on `LL-250` finds the skill should also mention
> breaking changes. It edits `.claude/skills/release-notes/SKILL.md` in its
> worktree. To send that edit back to the team context, ask the agent to
> preview it with `preview_reverse_sync_worktree` and then publish it with
> `reverse_sync_worktree`. Nothing is published until an agent makes that call.
>
> The change lands in `billing/content/.claude/skills/release-notes/SKILL.md`.
> Other worktrees get it the next time an agent launches or hydrates there. If
> the team's copy changed in the meantime too, the publish reports a conflict
> instead of overwriting it.
>

## 4. Share it with your team

Your context repo now has the new skill, on your Mac. To get it to
teammates, commit and push it from the context repo folder, or ask your
agent to: CodeSense never commits or pushes for you. Teammates get it on
their next pull. See [Sharing through git](../concepts/sharing.md).

## Things to know

- **The nearest copy of each file wins.** Skills resolve one file at a time
  across the team chain: a parent team's shared `release-notes` reaches your
  agents unless your team has its own copy. Editing an inherited skill from your
  team edits the parent's; **Copy into ‹team›** gives your team its own copy.
  See [Teams and inheritance](../concepts/teams-and-inheritance.md).
- **Any agent's files work the same way.** `AGENTS.md`, `.codex/` files, or
  plain docs in `content/` reach a worktree just like skills do, when they're
  in the launching agent's `context`.
- **Your own clone isn't touched.** CodeSense writes context into worktrees
  it creates and clones it manages, not into a clone you attached from your
  own folder.

The full guide ships in the app's docs: **Agents → Skills**.
