# Launch agents and watch them

_Start agents on ready stories from the left nav and follow them in tiles and the agents list._

The left nav shows what's ready. This guide takes a ready story from the nav, opens its workspace (a worktree in the story's repo, with your team's context already in place), starts an agent there, and then keeps an eye on several agents at once.

The example is Ledgerline's sprint: LL-233 (refund button, in `web`) and LL-209 (payment intents, in `api`).

## 1. Pick a ready story

In the left nav, a not-started story whose dependencies are all done is marked ready. Hover it to check ("Ready to start · all 2 dependencies done"). Click it to open the story file and read it. See [Dependencies and readiness](../concepts/dependency-graph.md).

To open a story's workspace and start an agent in one action, right-click it and, under **Launch agent**, choose your agent. The next two steps do the same by hand.

## 2. Open the story's workspace

Use any of these:

- **Left nav:** right-click the story and choose **Open CLI**. If your team has machines, pick **This Mac** or a machine.
- **⌘K** (Open Workspace): search for the workspace by name.

CodeSense then:

1. Looks up the story's `source_repo` and finds your clone of that repo. If you haven't attached one, it asks you to (**No local clone attached** › **Attach local clone**).
2. The first time, creates a git worktree for the story beside your clone (`../<repo>-wt/<story>`), on the story's `branch_name`, or else `feat/` plus its story id. Later opens reattach to it. It stays until you remove it.
3. Copies your team's context into the worktree as files: by default `CLAUDE.md` and `.claude/` skills, commands, hooks and agents for Claude Code, or `AGENTS.md` and `.agents/skills` for Codex, plus the MCP config for CodeSense's tools. None of it is committed: the paths go in the repo's `.git/info/exclude`.
4. Opens the workspace with a terminal tab in the worktree.

## 3. Start your agent

The terminal runs your login shell (zsh, bash or fish), already in the worktree. Start your agent there:

```sh
claude
```

Claude Code starts with the team's context and CodeSense's MCP tools in place. On a brand-new worktree the tile shows **context landing…** while the files are written. A command you type then waits for them ("your command runs when it's ready"), for up to 10 seconds. You can run any other CLI the same way, including Codex. See [Switch a story between Claude Code and Codex](switch-agents.md).

Repeat for LL-209. Its workspace opens in your `api` clone.

> **Beta**
>
> A dashboard page can also open a story's workspace with its agent, using the page SDK's `openWorkspace` action. See [Dashboards and pages](../concepts/dashboards.md).

## 4. Watch them side by side

Each workspace has its own terminal tabs. **New terminal** adds another one. To see several at once:

- **⌘⇧L** (Cycle Terminal Layout) switches tiles between **Columns**, **Rows** and **Grid**.
- **⌘1** to **⌘9** jump between workspaces, and **⌘0** goes back to the root workspace.
- Each tile's **View options** menu opens **Story summary** (⌘⇧H), **Source diff** (⌘⇧D) and **Code editor** (⌘⇧C) over the terminal. ⌘I opens the agent's inbox.

> **Beta**
>
> A Claude Code, Codex or OpenCode session shows a **status dot** on its tab and in the workspace tray, locally or on a remote machine (Claude Code and Codex only). The dot is driven by the hooks or plugin CodeSense adds to each CLI, so you can see which agent needs you without watching every terminal. Each state has its own shape as well as its colour.
>
> | Dot | Meaning |
> |---|---|
> | Three breathing dots | Thinking |
> | A turning arc | Running a tool |
> | A dot with a lock and a pulsing halo | Needs approval: it's waiting on you |
> | A hollow ring | Ready: it finished its turn |
> | A triangle with "!" | Error |
> | A filled square | Crashed |
>

## 5. Follow what an agent is changing

Open a tile's **View options** menu and choose **Source diff** (or press ⌘⇧D). It floats over the terminal with a live diff of the worktree: the whole branch or only uncommitted changes. You can view it side by side or inline, and a structural mode compares the code's syntax instead of lines. **Follow edits** is on by default, so the view jumps to the newest change as the agent writes it. The diff is for reading: nothing you do in it is sent to the agent. It works on a machine's worktree too.

## 6. The agents list

The team's running agents are listed in the **Agents** list on the left of the terminal area. An agent that launched others has them nested under it. Click a row to go to its terminal; right-click it for **Open inbox**, **Open story**, **Stop…** and more. The stop button in the list's header stops every background agent running unattended; terminals you opened stay open.

When an agent finishes, it calls `signal_done` to record the result. Moving the story on is a plain status edit, by you or the agent. See [Statuses and phases](../concepts/statuses-and-phases.md).

> **Note**
>
> Other agent CLIs you run in a workspace terminal work normally, but their tabs don't show the status dot; only Claude Code, Codex and OpenCode report one.

The full guide ships in the app's docs: **Pipelines → Phase 8 · Build (parallel)**.
