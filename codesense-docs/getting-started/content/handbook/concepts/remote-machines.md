# Remote machines

_Run agents on a Linux machine over SSH: sessions that keep running after you quit, story workspaces on the machine, moving work there and back._

A **machine** is somewhere a CodeSense terminal can run. Your Mac is one. A Linux box you reach over SSH can be another: a bigger build server, or one that stays on when your laptop is shut. A terminal tab on it looks and works like a local one, and the agents in it use the same MCP tools, the same Inbox and your team's context.

The whole feature is in beta.

## Add a machine

> **Beta**
>
> Open **Machines** (**⌘⌥C**, or **Go → Machines**) and add one, or run **Add machine…** from the command palette (**⌘⇧P**). CodeSense connects with ordinary OpenSSH and your own SSH setup: your SSH agent or a key file. It never asks for a private key, a password or a token. The first connection test shows the machine's host key fingerprint; once you confirm it, CodeSense pins it and refuses a machine whose key changes.
>
> You can keep a machine to yourself or share it with a team. Sharing shares where the machine is, never how to log in: each teammate signs in with their own account on it.
>
> The bundled `setup-machine` skill in the CodeSense docs walks a fresh machine through the rest. It checks what's installed, asks before any `sudo`, and installs only the agent CLIs you use. You sign in to Claude Code or Codex yourself, on the machine.

## The terminal server

> **Beta**
>
> CodeSense installs a small **terminal server** in your account on the machine. It owns the machine's terminals, not your SSH connection, so they keep running when your Mac sleeps, loses Wi-Fi, or you quit CodeSense. When you reconnect, CodeSense reattaches and the agent you left is still there, with the scrollback it wrote meanwhile. Closing a tab is the one thing that ends its session.

## Story workspaces on a machine

> **Beta**
>
> `open_workspace` with a `story_id` and a `machine_id` opens the story's workspace on that machine. The first open copies your local clone there, so the machine needs no GitHub credentials, and makes the story's worktree. Add `target: "story"` and an `agent` to start an agent in it in the same call. The call returns once the workspace is open there, or says why it couldn't.
>
> A story's workspace lives in one place at a time. To move it:
>
> - **`move_workspace`** moves a workspace to another machine, or back to This Mac with `targetMachine: "local"`. It refuses a worktree with uncommitted changes unless you pass `discardUncommitted: true`. A move runs in three recorded steps, so a move cut short by a crash is finished or rolled back the next time the app starts.
> - **`remote_workspace_fetch`** brings the commits from a workspace on a machine into your local clone, under `refs/remotes/codesense-machine-<machineId>/<branch>`. It creates the local branch or fast-forwards it when that's safe, and never merges, rebases or force-updates. Uncommitted work stays on the machine. Push and open pull requests from your local clone with your own git tools.

## Context on a machine

> **Beta**
>
> Context works the same way it does locally (see [Agent context](agent-context.md)): `hydrate_worktree` with a remote target copies team files into the workspace on the machine, and never overwrites a file you edited there. `preview_reverse_sync_worktree` shows what `reverse_sync_worktree` would write to the team before it writes anything. A team file that changed since it was copied comes back as a conflict and is never written.

## Agents on the machine using CodeSense

> **Beta**
>
> Agents on a machine can call CodeSense's MCP tools only if you turn on **Allow agents on this machine to use CodeSense** for that machine. It's off by default, it's your setting alone, and an agent on the machine can't turn it on. Turning it on doesn't share your credentials. Moving a workspace onto a machine needs it on.

## Usage and routines

> **Beta**
>
> Token use and spend from sessions on your machines are pulled back to your Mac, so `session_usage` and dashboards count them with your local sessions. A connected machine is pulled every minute; one that was offline catches up when it next connects. See [Usage and ROI](../guides/usage-and-roi.md).
>
> A routine can wake an agent that runs on a machine (`machine` set to its id, or `any`). With `routine_arm_on`, the machine's terminal server fires the routine on its own clock, with every Mac closed. See [Routines](routines.md).

> **Note**
>
> Linux is the verified remote. Another Mac over SSH may work but hasn't been checked.

The full guide ships in the app's docs: **Terminals & machines → Remote (private account)**.

## Agents that keep going

Terminals on a private machine survive the app closing, and a routine armed on the machine fires on the machine’s clock.

```text title="routine.yml (excerpt)"
where:
  kind: team_workspace
  machine: build-box      # this machine's id; or local, or any
triggers:
  - schedule:
      cron: "0 6 * * *"
```

A remote wake is delivered by that machine, so it lands with your laptop shut. [Routines](routines.md#where-a-routine-fires) has the details.

## Move a live workspace

> **Beta**
>
> Move a live workspace from your Mac to a server. A stopped move picks up where it left off.
>
> Ask for it in the app or with `move_workspace`. The workspace comes back up on the new machine with its worktree and context; if the move stops partway, run it again and it resumes where it stopped.

## Your keys stay home

Your git keys stay on your Mac. CodeSense seeds each clone over SSH, and commits come back through CodeSense.

| What | On the box | Stays on your Mac |
|---|---|---|
| Git | A clone seeded from your Mac | Your GitHub or git host credentials |
| Context | The files each agent was given | Your context repo and its history |
| Agents | The CLIs, signed in with your own logins there | Nothing extra |

Remote agents pull more context through MCP and preview changes before they send them back. Their commits come back to your Mac with `remote_workspace_fetch`, and you push them yourself.

Cost from remote sessions already counts like local ones. Line attribution covers Claude Code and Codex agents on your Mac.

## Update every server

Update every terminal server at once.

When a terminal server is out of date, the Machines tab says so. `update_terminal_servers` updates them all. That ends every terminal and agent on them; the tabs that were open come back as fresh shells, and an Agent with `resumeAfterRestart` relaunches.
