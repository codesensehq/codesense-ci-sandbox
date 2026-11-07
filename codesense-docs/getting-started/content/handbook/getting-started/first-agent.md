# Launch your first agent

_Go from a story to its own worktree with Claude Code, Codex or OpenCode running in it._

Every story gets its own worktree: a separate checkout of its repo, on its
own branch. You start an agent on the story, and CodeSense makes the worktree,
copies in the context the agent's definition declares and runs Claude Code,
Codex or OpenCode there.

This page uses Ledgerline's story `LL-214` ("Refund endpoint idempotency
keys"), which changes `ledgerline/api`.

## Before you start

- The story's repo is attached. See [Attach your repos](attach-repos.md).
- The story file has `type: story` (or `bug` or `spike`) and a
  `source_repo`. The first lines of `LL-214` look like this:

  ```yaml
  ---
  type: story
  story_id: LL-214
  title: Refund endpoint idempotency keys
  status: todo
  source_repo: ledgerline/api
  depends_on: [LL-209]
  ---
  ```

- `claude`, `codex` or `opencode` is installed, signed in and on your `PATH`.

## 1. Launch an agent on the story

In the left nav, right-click the story. Under **Launch agent**, choose an
agent: your team's saved agents, your own, and the starters **Claude**,
**Codex** and **OpenCode**, which run the CLI with its defaults and your own login.

CodeSense creates the story's worktree and branch the first time, and reuses
them after that. Then it copies in the context the agent's definition
declares, and starts the agent in a terminal there with the prompt "Read the
story at … and complete it, satisfying every acceptance criterion."

- **Branch.** The story's `branch` field if it has one; otherwise the repo's
  branch prefix plus the story id. With the default prefix `feat/`,
  `LL-214` gets `feat/LL-214`.
- **Folder.** By default `../{repo}-wt/{slug}` next to the repo's clone,
  where `{slug}` is the branch with `/` turned into `-`. For a clone at
  `~/code/api`, that's `~/code/api-wt/feat-LL-214`.

You can change both per repo. See [Worktrees](../concepts/worktrees.md).

## 2. Check the context is there

The starter agents get the CLI's usual files from your team's context: for
Claude Code, `CLAUDE.md` and `.claude/` skills, commands, hooks and agents;
for Codex, `AGENTS.md` and `.agents/skills`. A saved agent gets what its
definition lists.

```sh
ls CLAUDE.md .claude/skills
git status --short    # clean: context paths are excluded from git
```

## 3. Or open a terminal and type the command

Right-click the story and choose **Open CLI** instead to get the worktree with
a plain terminal. If your team has machines, it asks where: **This Mac** or
one of them. Then start the agent as you would anywhere else:

```sh
claude
# or
codex
```

An agent you type yourself gets CodeSense's MCP bridge, registered in the
worktree's `.mcp.json` and `.codex/config.toml` and kept out of git, which
gives it CodeSense tools such as validating a story or pulling an epic's stories
into the worktree. It doesn't get the team's context copied in at start: ask
it to call `hydrate_worktree`, which with no arguments brings what a starter
agent would get. See [MCP tools for agents](../concepts/mcp-tools.md).

A good first prompt names the story:

```text
Implement story LL-214. Read the story and its depends_on stories first,
and use the add-endpoint skill.
```

## 4. Read the status mark

> **Beta**
>
>   A terminal tab running Claude Code, Codex or OpenCode shows a small mark that tracks
>   it, so you don't have to watch every terminal. It works whether CodeSense
>   launched the agent or you typed it, on This Mac or (Claude Code and Codex only) a remote machine. Each
>   state has its own shape as well as its colour.
>
>   | Mark | Meaning |
>   |---|---|
>   | Three breathing dots, blue | Thinking |
>   | Turning arc, blue | Running a tool |
>   | Dot with a lock and a pulsing halo, amber | Needs approval |
>   | Dot with a "?" and a pulsing halo, amber | Needs answer |
>   | Triangle with a "!", red | Error |
>   | Filled square, red | Crashed |
>   | Hollow ring, grey | Ready: the turn finished |
>   | Hollow square, grey | Ended |
>

> **Your settings stay yours**
>
>   For a `claude` you type yourself, CodeSense writes its status hooks into
>   `.claude/settings.local.json` in the worktree, never into your
>   `.claude/settings.json`, and excludes it from git. An agent CodeSense
>   launches gets them in a settings file of its own, outside the worktree. See
>   [Keep your own Claude Code hooks](../guides/keep-your-hooks.md).

## What's next

- Launch several stories at once and watch them side by side:
  [Launch agents and watch them](../guides/launch-agents.md).
- Bring your own scripts and CLIs:
  [Run your own tools and scripts in a worktree](../guides/your-own-tools.md).
- A worktree stays until you remove it, even after the story is merged. See
  [Worktrees](../concepts/worktrees.md).

The full guide ships in the app's docs: **Agents**.
