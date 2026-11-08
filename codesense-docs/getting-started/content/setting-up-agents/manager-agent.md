# Recipe: Build a manager agent

A manager is an agent that takes incoming work, decides what to do with it, launches workers
and tells you what needs you. CodeSense has no built-in manager or controller. This recipe
builds one from the same tools every agent has, so you can see each piece and change it.

Nothing here is special to CodeSense. To CodeSense the manager is an ordinary saved Agent that
holds a role called `manager`. Rename it, run two of them, or give each epic its own: it is
your design.

## How it works

| What the manager does              | The mechanism                                                                                                                                                                                                                 |
| ---------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Has an address work can be sent to | A **role queue**. Anyone sends work with `send_role_message {role: "manager"}`. It waits in the queue until a manager takes it, even if no manager is running.                                                                |
| Is the only manager                | `register_role {role: "manager", sessionId, exclusive: true}`. A second session gets `{status: "alreadyHeld", instance}`, naming the holding session.                                                                                                |
| Stays the manager                  | CodeSense keeps a registration alive while its session runs and releases it when the session ends. The agent never heartbeats.                                                                                                |
| Takes work safely                  | `claim_role_messages`, `accept_role_message`, `ack_role_message`. A manager that dies after accepting leaves the work as `acceptedUnacked`, and the next manager adopts it.                                                   |
| Knows who it is                    | `who_am_i {session_id}` gives its handle and name.                                                                                                                                                                             |
| Launches workers                   | `open_workspace` (on this Mac or with `machine_id` on a machine), passing `launched_by`. Its workers nest under it in the agents list.                                                                                          |
| Tracks workers                     | `list_sessions {launched_by}` shows each worker's state, progress and unread mail.                                                                                                                                             |
| Gets the answers                   | `replyTo: "role:manager"` on a message. Whoever replies, the human included, answers into the manager queue, not to the session that asked. It works after that session has exited. The Inbox shows "Replies go to: manager". |
| Tells you things                   | `send_message {to: "human"}`, as markdown or as a live HTML dashboard: `kind: "needs-you"` for a decision, `kind: "update"` for a digest. Both kinds appear in conversations and count as unread human mail; kind marks whether they wait on you for agent views and dashboards. A digest with `replyTo: "role:manager"` from the manager is an update either way. |
| Wakes up when mail arrives         | The **Wake the manager** routine (`inbox_idle` trigger, `mode: wake`) types into the manager's own idle session. No second agent does the waking.                                                                            |
| Comes back after a crash           | The **Keep a manager on the queue** routine (`mode: wake`, `machine: any`) wakes the manager every hour to check its queue. If no manager is running anywhere, it starts one.                                             |

## The files

| Copy this                                                                                         | To                                                   |
| ------------------------------------------------------------------------------------------------- | ---------------------------------------------------- |
| [`examples/skills/manage-team/SKILL.md`](../examples/skills/manage-team/SKILL.md)                 | `<team>/content/.claude/skills/manage-team/SKILL.md` |
| [`examples/agents/manager/agent.yaml`](../examples/agents/manager/agent.yaml)                     | `<team>/config/agents/manager/agent.yaml`            |
| [`examples/agents/manager/routines/manager-wake/routine.yml`](../examples/agents/manager/routines/manager-wake/routine.yml)     | `<team>/config/agents/manager/routines/manager-wake/routine.yml`    |
| [`examples/agents/manager/routines/manager-ensure/routine.yml`](../examples/agents/manager/routines/manager-ensure/routine.yml) | `<team>/config/agents/manager/routines/manager-ensure/routine.yml`  |

The agent file:

```yaml
id: manager
name: Manager
description: Watches the manager role queue, triages incoming work, launches and tracks workers, and reports to you.
runtime: claude
launch: foreground
startup:
  - CLAUDE.md
  - .claude/skills/manage-team/
default_prompt: /manage-team
revision: 1
```

The routines live under the agent (`config/agents/manager/routines/`), so they name no
`agent:` of their own: the folder says whose they are. Both use `mode: wake`, which delivers
the routine's task into the running manager session when it is idle instead of starting a
second manager, and starts one only when none is running.

`launch: foreground` gives the manager a visible tab, so you can watch it and talk to it, and
makes it wakeable. Put your team's rules in its `CLAUDE.md`, such as what it may approve on its own. Without them,
the skill asks you before any merge, approval or irreversible step.

## Set it up

1. Copy the four files into your team.
2. Open your team in CodeSense. **Manager** shows in the Agents tab, with both routines in its
   **Routines** section (and in **Agents | Schedule**).
3. Turn the routines on for your Mac. **Keep a manager on the queue** runs once for the team:
   pick **Run on this Mac**. **Wake the manager** runs on each Mac whose user turns it on, since
   sessions are per Mac: pick **Run on my Mac**. An agent can do the same with
   `routine_update` and `routine_set_enabled`.
4. Push the files with git so your teammates get them.

Or ask an agent:

> Set up the manager agent from the CodeSense docs, in my team.

It copies the skill and both routine files, creates the agent with `agent_preview` and
`agent_create`, and turns the routines on. It asks before it changes anything.

## Use it

- Launch **Manager**. It shows in the agents list like any agent, and its workers show under
  it.
- Pin it (the pin on its row, or **Pin** in its row menu) to keep it at the top of your agents
  list. Pins are yours alone. While no Manager runs, its pinned row says "Not running" with a
  **Start** button that opens the launch sheet with Manager selected.
- Send it work from anywhere: you, a dashboard or another agent, with
  `send_role_message {role: "manager", text}`.
- Its row says "Holds: manager" while it holds the role. If it hangs while holding the role,
  choose **Release role** in its row menu, or stop it. Its role is also released when its
  session ends, and the next manager takes over the queue, including the work it had accepted.

## Make it yours

- **Two managers sharing the load.** Drop `exclusive: true` from the skill. Each instance
  claims its own deliveries from the same queue.
- **A manager per epic.** Copy the agent as `manager-billing` (any id), change `role` in the
  skill and the routines to match, and send that epic's work to its role.
- **A remote manager.** Launch it on a machine with `open_workspace {machine_id}`. The ensure
  routine's `machine: any` already finds and wakes it there; set `machine` to that machine's
  id to also start it there when none runs. It has the same tools as a local one. While that
  machine is offline its manager still counts as running: the ensure fire is
  `undelivered (machine_offline)` and no second manager starts while that machine stays
  offline.
- **Someone else gets the answers.** Change `replyTo` in the skill to another role or a
  session handle.
- **No routines.** Both are optional. Without them, nudge an idle manager yourself and start it
  again after a crash: the queue keeps all the work meanwhile.
- **A one-off check-in.** For "look at the queue again in 30 minutes", the manager doesn't need
  a routine: it sets a timer on its own session with `schedule_wakeup`, which ends with the
  session.
