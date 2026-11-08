# Switch a story between Claude Code and Codex

_Move a story from one built-in agent to another without redoing its context or plan._

Nothing about a story belongs to the agent working on it:

- the story file and its place in the plan
- the worktree and branch
- the team context written into that worktree

So switching LL-231 from Claude Code to Codex doesn't mean starting over. You open the same workspace and run the other CLI.

## 1. Open the story's workspace

Right-click LL-231 in the left nav and choose **Open CLI**. If the worktree already exists, CodeSense reattaches to it, and your branch and uncommitted changes are just as the last agent left them.

## 2. Stop the first agent (or don't)

Exit Claude Code in its tab, or leave it running. Both agents can share the worktree. If they do, keep them off the same files.

## 3. Start Codex

Open a new terminal tab in the workspace (⌘T, **New Terminal Tab**) and run:

```sh
codex
```

Codex starts in the story's worktree. It reads `AGENTS.md` if your team keeps one in its context. The Ledgerline `billing` team has both a `CLAUDE.md` and an `AGENTS.md`.

To have CodeSense start Codex for you with its saved settings, choose it under **Launch agent** in the same menu instead.

OpenCode works the same way on this Mac: run `opencode`, or choose it under **Launch agent**. It reads `AGENTS.md` too.

## 4. CodeSense tools in Codex

The story's worktree already has the CodeSense bridge in `.codex/config.toml` (and in `.mcp.json` for Claude Code), kept out of git, so Codex started by hand there can use CodeSense's tools, for example `validate_story`, or `signal_done` to record that it finished. A Codex launched from CodeSense gets the bridge for its session without reading that file.

## What carries over, and what doesn't

| | Claude Code | Codex |
|---|---|---|
| Worktree, branch, story file | yes | yes |
| Team context files in the worktree | `CLAUDE.md`, `.claude/` skills | `AGENTS.md`, plus whatever else your team keeps |
| CodeSense MCP tools | yes | yes |
| Status dot on the tab (Beta) | yes | yes |

A story's status doesn't care which agent worked on it either: it's a field in the story file, and whichever agent finishes writes it. See [Statuses and phases](../concepts/statuses-and-phases.md).

> **Two instruction files**
>
> CodeSense copies your context files as you wrote them. It doesn't turn `CLAUDE.md` into `AGENTS.md`. If your team uses both agents, keep both files, or have one point to the other.
