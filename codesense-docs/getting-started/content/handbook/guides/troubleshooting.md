# Troubleshooting

_Fixes for common snags: a skipped story, a missing clone and more._

## A story doesn't show up or can't launch

Check the story's frontmatter for these three things.

**It needs `source_repo`.** A story with no `source_repo` can't launch until it names its repo. Add the repo it belongs to:

```yaml
---
type: story
story_id: LL-231
title: Cart summary tax breakdown
status: review
source_repo: ledgerline/web
---
```

Without it, the story is left out of the story index: it can't be opened as a workspace, and other stories can't find it by id.

**It needs an explicit `type`.** Use `type: story`, `type: bug` or `type: spike`. Any other value makes the file a plain doc. Always write the type out.

**Quote values that contain `: `.** A value such as `title: Checkout: tax lines` breaks the YAML, and the story silently drops out. Write `title: "Checkout: tax lines"`.

For an HTML story, the same fields go in meta tags: `cs:story-id`, `cs:source-repo`, `cs:depends-on` and so on. See Story and epic fields.

> **Let your agent check it**
>
> Ask an agent launched from CodeSense to run `validate_story` on the file. It reports `MISSING_SOURCE_REPO`, `UNKNOWN_SOURCE_REPO`, `INVALID_TYPE`, `INVALID_STATUS`, `DUPLICATE_STORY_ID`, `UNRESOLVABLE_DEPENDS_ON` and `UNPARSEABLE_FRONTMATTER`.

> **Coming: configurable work-item types**
>
> Work-item types beyond story, bug and spike, set per team.

## A page action says `denied: write budget`

A runaway guard stopped the page after a large batch or a burst of writes in one render. Reload the page to reset it. If an agent wrote the page, ask it to batch fewer writes per click.

## "No local clone attached"

This appears when a story's repo is listed for your team, but CodeSense doesn't know where its clone lives on this machine. Choose **Attach local clone**, then **Browse for clone directory** to pick a checkout you already have. CodeSense doesn't clone for you: if you don't have one yet, clone it with git first. Worktrees, terminals and editor actions for that repo work once it's attached. See [Attach your repos](../getting-started/attach-repos.md).

## A CLI I typed myself doesn't see the team's context

An agent you launch from CodeSense gets your team's context (`CLAUDE.md`, `.claude/`, skills) copied into its worktree before it starts. A `claude` or `codex` you type yourself in a terminal tab gets only what the worktree already holds. Launch the agent from CodeSense instead, or have an agent call `hydrate_worktree` to copy the context in.

> **Coming: a context-landing indicator**
>
> A terminal that shows when context is still landing, or holds the first prompt until it has.

## An agent says "CodeSense is not running"

The agent's MCP bridge couldn't reach the app. Open CodeSense. The bridge reconnects on its own for about 30 seconds after the app restarts. After that, restart the agent session.

## Claude Code or Codex isn't offered

CodeSense looks for `claude` and `codex` and needs `claude` 2.0 or newer, or `codex` 0.140 or newer. Install or update the CLI; CodeSense notices new installs. Git 2.44 or newer is the only thing CodeSense requires to start.

## My shell config isn't loaded in the terminal

CodeSense terminals start your login shell (`$SHELL`) when it is zsh, bash or fish, and fall back to zsh for any other shell. To pick one, set **Terminal shell** in Settings. See [Terminals](../concepts/terminals.md).

The full guide ships in the app's docs: **Reference → Troubleshooting**.
