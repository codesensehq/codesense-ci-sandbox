# Agents and runtimes

_Every launch names an agent: the built-in Claude Code, Codex and OpenCode runtimes, their builtin/* starters, launch modes and MCP scope._

CodeSense doesn't ship its own coding agent. It launches the agent CLI you already use, in the story's worktree, with your team's context and CodeSense's MCP tools already in place. Three agents are built in: **Claude Code**, **OpenAI Codex** and **OpenCode**. OpenCode runs on this Mac only; remote machines run Claude Code and Codex.

## Built-in runtimes

CodeSense drives each CLI through a built-in adapter: which binary to run, which versions it needs, how to wire the CodeSense MCP bridge into its config, and how it signals that it needs you. Adapters are part of the app, not files you edit. You customise a launch through an agent's `agent.yaml`: its runtime, args (a `--model` flag included), environment variables (`env`), MCP servers, permissions and startup files. Which login runs it is your own binding on each Mac, not part of the agent.

| | Claude Code | Codex | OpenCode |
|---|---|---|---|
| Binary | `claude` (2.0 or newer) | `codex` (0.140 or newer) | `opencode` (2.1.4 or a newer 2.x release) |
| MCP bridge written to | `.mcp.json` | `.codex/config.toml` | No file: passed at launch in `OPENCODE_CONFIG_CONTENT` |
| Starter agents | `builtin/claude`, `builtin/claude-unattended` | `builtin/codex`, `builtin/codex-unattended` | `builtin/opencode`, `builtin/opencode-unattended` |
| Stripped from the environment | `ANTHROPIC_API_KEY` | `OPENAI_API_KEY` | `OPENCODE_API_KEY` and the provider API keys |
| Runs on | This Mac and remote machines | This Mac and remote machines | This Mac |

API-key variables are removed from the agent's environment so it uses the account you signed in with in the CLI. Each CLI still does its own sign-in. When you add an account, CodeSense keeps that account's CLI login in a private folder it creates, and never reads or sends the credentials.

CodeSense finds `claude`, `codex` and `opencode` on your login shell's `PATH`, even when you start the app from Finder. An agent that isn't installed (or is below the minimum version) is simply unavailable. Git is the only hard requirement.

The config file the bridge goes into is only written inside worktrees and clones CodeSense manages, and it's added to git's exclude list so it never shows up in your commits.

## Every launch names an agent

An **agent** is what runs: its runtime (Claude Code, Codex or OpenCode), args, MCP servers, permissions and startup files. Every way of starting a CLI session names one: the story **Launch agent** menu, **Launch** on an agent in the **Agents** tab, a dashboard's `openWorkspace` (`agent`), the `open_workspace` MCP tool, routines and Weft workers. A launch that names none runs **`builtin/claude`**; **`builtin/codex`** and **`builtin/opencode`** are the other starters. Each has an unattended twin, **`builtin/claude-unattended`** (Claude Code with permission checks skipped), **`builtin/codex-unattended`** (Codex with approval `never` and the `danger-full-access` sandbox) and **`builtin/opencode-unattended`** (OpenCode with every permission allowed), which runs without asking for permission. A background launch that names no agent (a headless `open_workspace`, a dashboard's `openWorkspace` with `headed: false`) runs `builtin/claude-unattended`, and a Weft worker runs the unattended twin of its provider, so a session nobody is watching never waits on a permission prompt. It runs on the account you bound to the ordinary starter unless you bind the twin to its own. An agent on a remote machine always opens as a tab there, so it keeps `builtin/claude`. A named agent always runs with its own permissions. All starters are read-only; **Use template** makes an editable copy of one. Create your own agents in the **Agents** tab.

An agent runs in one of two **execution modes**:

- **Interactive** (the default, "in a terminal tab"): the agent runs in a terminal tab in the story's workspace, on this Mac or on a machine.
- **Headless**: there's no terminal tab. The run shows in the agents list beside the terminal, where its row's **Stop** stops it. Headless runs on this Mac only.

> **Agents that skip permissions**
>
> `builtin/claude`, `builtin/codex` and `builtin/opencode` use the CLI's own permission settings. The unattended starters, and any agent whose args carry a bypass flag (such as `--dangerously-skip-permissions`), act without asking. CodeSense warns about a bypass flag on your own agents and adds one only through the unattended starters.

## MCP scope

An agent sees only the MCP servers its definition names, plus `codesense`, which is always on. An agent that names none, the starters included, gets `codesense` alone. Pick the servers in the **Agents** tab.

At launch, CodeSense writes a temporary, machine-local config that contains only those servers plus the CodeSense bridge, and deletes it when the session ends. Your shared `.mcp.json` is never changed.

## Raw terminal snapshots

An agent's **Your account (only you)** area in the **Agents** tab has **Retain raw terminal snapshot for completed runs (only you)**. It's yours alone, kept on this Mac, and applies only to that agent's interactive launches.

## Which login runs it

An agent never carries an account. Each agent's **Your account (only you)** picks which of your own Claude or Codex CLI logins on this Mac runs it, or **Use my default**. It's stored on your Mac, so teammates launch the same team agent with their own logins. An unattended starter with no choice of its own uses the login you picked for its ordinary twin.

## Live status

> **Beta**
>
> Each agent session shows a status dot: **running**, **done**, **error** or **needs permission**. Claude Code and Codex report through hooks CodeSense adds, and OpenCode through a plugin CodeSense adds, whether CodeSense launched them or you typed them. See [Launch agents from the board and watch them](../guides/launch-agents.md).

## Switching agents

Your context and stories are files, not agent state. So you can move a story from one agent to another, say from Claude Code to Codex or OpenCode, or back, without redoing any of them. See [Switch a story between Claude Code and Codex](../guides/switch-agents.md).

## Other agents

Whichever agent you run in a worktree terminal, your team's context is already there as plain files. It reads them the way it reads any file. One-click launch and the MCP bridge wiring are built in for the three agents above, and so is the status dot (in Beta).

The full guide ships in the app's docs: **Agents → agent.yaml**.

## Roles and role icons

Give each agent a role and an icon for it. A role is any name the agents agree on; send work to it and whichever registered agent holds it picks the job up from that role’s queue.

A role holder registers once and claims the work sent to it. Roles ship with CodeSense; no role name is special.

```text title="register_role"
{
  "team": { "org": "acme/acme-context", "team": "web" },
  "role": "reviewer"
}
```

Then `claim_role_messages` hands it the next job, and `ack_role_message` marks it done. If a holder exits halfway, the next one to register adopts the unfinished work. Pick an icon for each role; in our own setup the roles are chess pieces, so you can tell the reviewer from the builder at a glance. [Role queues](inbox.md#roles-mail-that-outlives-a-session) covers the life of a delivery.

## Faces and personas

Upload your own faces and share them with the team. A persona is just words in your agent’s own files, so it can talk in character while it does serious work.

Every agent has a face, shown in its tab, its row in the Agents list and its messages. Add your own with `avatar_add` and share it with the team with `avatar_share`; `avatar_list` shows what you have.

> **Tip from the designer**
>
> Write the persona in the agent’s own files, next to its rules: a line in its `CLAUDE.md` is enough. It talks in character in the Inbox, with GIFs and reactions, and still does serious work.

## Several agents, one worktree

Agents sharing a worktree each get their own context side by side. CodeSense has no special agent: a manager is an agent you design, with its own prompt, skills and routines.

Agents drive the app through MCP: they open workspaces, start and read other agents’ terminals, and message each other. So a manager is a recipe, not a feature: an agent that holds the `manager` role, plus a routine that wakes it when mail arrives.

```yaml title="web/config/agents/manager/routines/manager-wake/routine.yml"
id: manager-wake
name: Wake the manager
mode: wake
task: |
  You have unread mail. Call read_messages, then claim_role_messages
  for the manager role, and carry on.
where:
  kind: team_workspace
  machine: local
triggers:
  - event:
      kind: inbox_idle
policy:
  max_runtime: 5m
revision: 1
```

Give the manager the skills it needs to split a story, send the parts to the builder and reviewer roles, and report back to you. [Routines and triggers](routines.md) has the other ways to wake it.
