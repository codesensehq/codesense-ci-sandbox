---
name: routine
description: Set up and manage the user's CodeSense routines and session timers. A routine is a saved, standing instruction an Agent runs on a schedule or an event ("make a routine for Barb to triage issues every weekday"); a timer is a temporary routine for the session you are in ("add a routine for your session to check the build in 20 minutes", "remind yourself"). Use when the user says "set this up as a routine", "do this every morning", "remind yourself", "check back on X", "what routines do I have", "pause X", "make it a draft", or asks to change, run or delete one. Advisory — the routine_* and *_wakeup tools work without it.
---

# Routines and timers

CodeSense has two ways to make an agent do something later. Pick by **who it is for**
and **how long it should last**:

| The user says                                                      | Make a…                    | Tool                                    |
| ------------------------------------------------------------------ | -------------------------- | --------------------------------------- |
| "add a routine for your session to check the build in 20 minutes"  | **timer** on your session  | `schedule_wakeup`                       |
| "remind yourself at 3pm to post the summary"                       | **timer**                  | `schedule_wakeup`                       |
| "check the deploy every 10 minutes until it's green"               | **timer** (repeating)      | `schedule_wakeup` with `every`          |
| "make a routine for Barb to triage new issues every weekday at 9"  | **routine** on Barb        | `routine_create` with `agent: "barb"`   |
| "every morning, start a fresh agent that writes the standup"       | **routine**, start a new   | `routine_create` with `mode: "fresh"`   |
| "whenever Barb has unread mail and is idle, wake her up"           | **routine**, `inbox_idle`  | a `routine.yml` with an event trigger   |

- A **timer** belongs to one running session: yours. It types its prompt into *this*
  conversation, so you keep your context. It never starts anything, never reaches another
  session, is never written to git, and **ends when the session ends** (exit, close, crash).
- A **routine** belongs to a saved **Agent**. It is a file in the team's context repo,
  `config/agents/<agent>/routines/<id>/routine.yml`, that outlives every session, shows
  under that agent in the **Agents** tab, and is shared with the team by a git push.

When in doubt about "your session" vs "Barb": if the user names an agent, or says
"every weekday" with no end, it is a routine. If they mean "you, right now, later", it is
a timer.

## Timers: "add a routine for your session"

One call to `schedule_wakeup`:

```json
{
  "sessionId": "$CODESENSE_TAB_SESSION_ID",
  "in": "20m",
  "name": "Check the build",
  "prompt": "Check the CI run for PR #212. If it is green, merge it; if red, read the failing job and fix it."
}
```

- `sessionId` is your own `$CODESENSE_TAB_SESSION_ID`. Another session's id is refused:
  a timer can only wake the session that set it.
- When: exactly one of `in` (`90s`, `20m`, `2h`, `1h30m`, `1d`) or `at` (RFC 3339 **with
  its offset**, in the future; `date +%z` prints yours). Add `every` (at least `1m`) to
  repeat, then at most one of `until` (RFC 3339) or `count` (at least 1). Add `minGap`
  (`30m`, `2h`; 1m to `7d`) when each fire starts heavy work: a fire inside the gap
  folds into the last one instead of waking you again.
- Write `prompt` for yourself later: it arrives as a new message, starting with
  `[timer: <name> | fire <n>]`, once you are idle. It is never typed while you are working
  or waiting on a permission prompt; fires due while one still waits collapse into it.
- `list_wakeups {sessionId}` shows your timers; `cancel_wakeup {sessionId, timerId}`
  stops one. Tell the user the `nextAt` you got back.
- Works in any interactive CodeSense session, saved agent or not, on this Mac or on a
  connected machine. Not for headless runs (`claude -p`); Codex sessions are not supported
  yet. Setting, listing and cancelling need the CodeSense desktop connected; timers already
  set on a remote machine keep firing while the desktop is away. On a remote machine, if the
  desktop disconnects your CodeSense tools stop working for the rest of the session (a fix is
  planned); your timers keep firing, but you can't list or cancel them until a restart.
- Limits: 10 active timers per session, 200 per machine. There is no lifetime cap: a
  session that runs for a week can take hundreds of small wakes. A fixed safety backstop
  (120 wakes an hour per session, every source together) refuses the rest as
  `rate_limited`, posts one inbox note naming the top source, and resumes on its own.

People set timers too: **Remind this agent…** in a terminal tab's menu. A tab with timers
shows a clock chip in its caption; its popover lists them with Cancel and **Keep this…**,
which turns a timer into a routine on the tab's agent (saving the session as an agent
first if it has none).

## Routines: "make a routine for Barb"

Every routine runs a saved **Agent** from `agent_list` (its runtime, MCP servers, startup
paths and permissions), and lives under it. Pick one, or make one first with
`agent_create`. One call to `routine_create`:

```json
{
  "name": "Morning triage",
  "agent": "barb",
  "mode": "wake",
  "when": "weekdays 09:00",
  "task": "Triage issues opened in acme/web since yesterday. Label each by area, post a 3-line summary to #web-triage with the Slack MCP tools, then call routine_done with that summary."
}
```

### `mode`: wake Barb, or start a new Barb

| `mode`  | In the app       | What a fire does                                                                                                                                                                                  |
| ------- | ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `wake`  | **wake**         | Types the task into Barb's **running** session when it is idle, so Barb keeps its context. Never starts a second Barb. If no Barb is running, starts one with the task as its first prompt. |
| `fresh` | **start a new**  | Starts a **new** Barb session every time, even if one is already running, with the task as its whole prompt. `overlap` decides what happens while the last run is still going.                |

Choose **wake** for an agent people keep open and talk to (a manager, a triage desk, a
long-lived builder). Choose **start a new** for independent jobs that want a clean slate
(a nightly report, a digest, a sweep), and for headless agents.

Pass `mode` explicitly. Left out, `routine_create` writes the agent's default: `wake` for a
visible routine on a saved agent that launches in the foreground, else `fresh`. The reply's
`effective_mode` says what a fire will do.

Wake has rules:

- It needs a **saved** agent. A built-in starter (`builtin/claude`, `builtin/codex`) is
  `fresh` only, so a tab someone opened by hand is never typed into.
- An agent that runs without permission checks (bypass or unattended) is woken only if its
  definition sets `allowWake: true`.
- A headless run is never woken. Codex idle detection is not qualified yet, so wakes into
  Codex sessions may come back `undelivered (unobservable)`.
- The wake text is the `task` with only `{{date}}` and `{{timestamp}}` filled in.
  `{{last_run_summary}}`, `{{last_run_at}}` and `{{trigger.*}}` fill in only for
  `fresh`; in a wake they stay literal, so don't rely on them there.

### `machine`: where Barb is looked for

`machine` is `local` (the default, this Mac), a machine id from `list_machines`, or `any`
connected machine. A wake on `any` goes into Barb's session wherever one runs, preferring an
idle one, and starts a new Barb on this Mac when none does. A remote wake is delivered by
the machine itself, so it lands even if the desktop drops while it waits. An `each_mac`
routine must use `machine: local` (or leave it out), except that a `fresh` one may use
`any` (each Mac then starts its own); a machine id is refused in either mode, since every
teammate's Mac would wake or start the same agent there.

A Barb on a machine that is offline still counts as running: the wake is `undelivered
(machine_offline)` and no second Barb starts while that machine stays offline.

### What a run says happened

While Barb is busy, a wake run is **queued** ("Waiting for Barb to be idle"). That is not
an end state: it waits up to the routine's max wait (`policy.max_wait`, 30 minutes by
default). A wake run ends as one of:

- **delivered**: typed into the session (the tab shows "Woken by ‹routine›" and a marker in
  the scrollback);
- **coalesced**: a fire that folded into one already queued;
- **undelivered (reason)**: nothing was typed, for example `busy_timeout`, `machine_offline`,
  `machine_unknown`, `session_ended`, `unobservable`, `needs_preparation`, `archived`,
  `limit_reached` (over the routine's `max_wakes_per_day`; it resumes on its own),
  `max_wakes` (the routine made its `max_wakes` and paused itself), `rate_limited` (the
  session's safety backstop; one inbox note per session per hour);
- **skipped (min_gap)**: inside the routine's `min_gap` with `overlap: skip`;
- **unknown**: it may have been delivered. CodeSense never re-sends an `unknown` delivery,
  and posts it once to the human inbox. Say "May have been delivered. Check the terminal
  before re-sending." Never re-send it yourself without looking.

A `fresh` run keeps its usual statuses (succeeded, failed, ended, timed out, needs you).

### The rest of `routine_create`

- `when`: a preset or a 5-field cron, in this Mac's zone unless `tz` (`Europe/London`):

  | The user says             | `when`                                   |
  | ------------------------- | ---------------------------------------- |
  | every 15 minutes          | `every 15m` (5m, 10m, 15m, 20m, 30m)     |
  | every 2 hours             | `every 2h` (1h, 2h, 3h, 4h, 6h, 8h, 12h) |
  | every hour                | `hourly`                                 |
  | every day at 9            | `daily 09:00`                            |
  | weekday mornings at 8:30  | `weekdays 08:30`                         |
  | Fridays at 5pm            | `weekly fri 17:00`                       |
  | the 1st of the month at 9 | `0 9 1 * *`                              |

  Nothing fires more often than every 5 minutes; for "every minute" use a timer. Omit `when`
  for a manual-only routine.
- `agent`: the agent's `id` in the routine's own space (`barb` is the team's agent for a
  team routine), or its full key from `agent_list`. Required unless you pass a `sessionId`
  launched as a saved agent, which then defaults to you. A team routine needs one of its
  own team's agents (or a starter, fresh only).
- `scope`: `team` (default, the active team) or `personal` (Only me, with `org`).
- `runsOn` (team only): `one_mac` (default; one shared job for the team) or `each_mac` (a
  recipe each teammate turns on for their own Mac with `routine_set_enabled`).
- `task` may use `{{date}}`, `{{timestamp}}`, `{{last_run_at}}`, `{{last_run_summary}}` and
  declared `variables` (`{"channel": {"type": "text", "default": "#web"}}` → `{{channel}}`),
  with the wake limit above.
- `dryRun: true` checks everything and writes nothing. Do it when unsure.

### Wake limits: you choose them for the work

Every limit is optional, set on the routine by whoever writes it (usually you). They count
wakes typed into a live session and are enforced by the terminal server that runs it, so
they hold on a connected machine with the desktop closed. What they do not cover:

- They apply to `mode: wake` only. A `fresh` routine starts a new agent each fire and makes
  no wakes; one with limits saves with a `wake_limits_ignored` warning (use `overlap`).
- A wake that finds no running agent and starts one (launch if absent) is not counted.
- Each Mac and machine keeps its own count: a `machine: any` routine has a budget per
  machine, and an `each_mac` recipe one per Mac.
- A machine whose terminal server is too old to enforce them is sent no limited wake: the
  run is `undelivered (unavailable)` with the reason as its detail, and the inbox is told
  once to update that terminal server. A machine-armed routine with limits is not armed
  there until it is (it fires nowhere meanwhile: the inbox says so once a day, and
  `routine_update` / `routine_arm_on` reply with a `wake_limits_unenforced` warning).

- `maxWakes`: wakes in all. Reached, the routine pauses itself and posts one inbox note;
  resuming it starts a new count.
- `maxWakesPerDay`: wakes in any rolling 24 hours. Fires over it end
  `undelivered (limit_reached)` and it resumes on its own.
- `minGap`: the least time between two wakes of this routine, into any session (`30m`, `2h`, `1d`, at
  most `7d`). A fire inside it is coalesced into the last one, or skipped with
  `overlap: skip`.

Light work needs none: a quick queue check can wake every 15 minutes for a week.

```json
{ "name": "Queue check", "agent": "barb", "mode": "wake", "when": "every 15m",
  "task": "Check the review queue. If it is empty, say so in one line and stop." }
```

Heavy work should not rewake or pile up:

```json
{ "name": "Regression sweep", "agent": "barb", "mode": "wake", "when": "hourly",
  "minGap": "4h", "maxWakesPerDay": 3, "overlap": "skip",
  "task": "Run the full regression suite and file what fails." }
```

`routine_update` takes the same three; `null` removes one. In the app they sit under
**Limits** in the routine's form, folded until opened. Separately, every session has a
fixed safety backstop (120 wakes an hour from all sources) that only catches loops.

Then tell the user what the reply says: `schedule_in_words`, the first of `next_fires`, the
`agent` and the `effective_mode`. Read `warnings`: the common one is a task starting with
`/name` the agent doesn't start with (fix with `agent_update`, adding the command or skill
to its `startup`).

### Event triggers

Set `events` in `routine_create` or `routine_update`, using the same event fields as
`routine.yml`, separate from `when`. Omitted or null events preserves; `[]` clears
inbox_idle, agent_done and role_pending; a nonempty list replaces those kinds only,
keeping schedules and metadata triggers.

```yaml
# config/agents/manager/routines/manager-wake/routine.yml
id: manager-wake
name: Wake the manager
owner: codesense
mode: wake
task: |
  You have unread mail. Call read_messages with your session id, then
  claim_role_messages for the manager role, and carry on with /manage-team.
where:
  kind: team_workspace
policy:
  max_runtime: 5m
triggers:
  - event:
      kind: inbox_idle   # one of the manager's sessions sits idle with unread mail
runs_on: each_mac
revision: 1
```

- `inbox_idle` fires when a session sits idle at its prompt with unread mail. With
  `mode: wake` only **the routine's own agent's** sessions fire it, and the one with the
  mail is woken; another agent's mail never wakes it. With `mode: fresh` any session the
  routine can reach (launched in its team) fires it, and `{{trigger.sessions}}` lists
  them. There is no middleman: do not write a routine whose agent calls `send_to_session`
  to wake another; give that agent its own wake routine. After 3 wakes that leave the
  same mail unread, the runner stops repeating that mail and records the reason in
  routine run history. Use `routine_runs` then `routine_run` to inspect it. Make it
  `runs_on: each_mac`: sessions are per Mac.
- `agent_done` (optionally `from_agent: reviewer`) fires when an agent finishes. A routine
  with it shows under the agent it runs, with an "after reviewer" note.
- In a `fresh` run, `{{trigger.sessions}}`, `{{trigger.count}}`, `{{trigger.session_id}}`,
  `{{trigger.status}}`, `{{trigger.summary}}` and `{{trigger.agent}}` say what fired it.

### Metadata triggers

Use `metadataTriggers` in `routine_create` or `routine_update`, not prose in `when`:

```json
{"id":"review-story","name":"Review changed stories","agent":"default","mode":"fresh","task":"Review the changed story and report the result.","maxRuntime":"5m","enabled":false,"dryRun":true,"metadataTriggers":[{"selection":{"where":{"kinds":["story"]}},"target":{"key":"status","op":"eq","value":"review"},"previous":{"key":"status","op":"eq","value":"in-progress"}}]}
```

Require `status: ok`, inspect `data.warnings`, and save by changing `dryRun` to false.
Omit `previous` for any entry into the target value. First discovery and restarts baseline
existing matches; they do not replay them. Metadata routines cannot use `each_mac`.
On update, omit `metadataTriggers` to preserve it; `[]` removes all metadata conditions.
The equivalent YAML is `triggers: [{event: {kind: metadata_change, metadata: {selection, target, previous}}}]`.
See the [complete cookbook YAML](../../../arc-g-routines/trigger-cookbook.html#metadata_change).

### Create and preview event triggers

Use `events: [{kind:"inbox_idle"}]` for unread mail at an idle prompt, or
`events: [{kind:"agent_done",from_agent:"reviewer"}]` for reviewer completion.
Only agent_done accepts `from_agent`; omit it for all agents in reach. The agent
reference needs valid syntax, not an installed agent.

Use `events: [{kind:"role_pending"}]` when a dispatcher needs every queue in reach.
Use `events: [{kind:"role_pending",role:"reviewer"}]` for reviewer mail only.
Only role_pending accepts `role`; it must be nonblank and needs no registry entry.
Preview creation with `dryRun:true`, inspect `data.triggers`, then save disabled
and read the definition with `routine_get` before enabling. Updates have no dry run.
Refusals identify `events[i].field` with a hint and save nothing. These three event
kinds support `each_mac`; metadata triggers still require `one_mac`.

The [cookbook](../../../arc-g-routines/trigger-cookbook.html#events) has equivalent
MCP and complete YAML examples. For a definition installed by hand, preserve placement,
open the app editor, make a harmless edit (such as the name), then choose **Save changes**
to validate it. Use `routine_arm_on {id, machine:"this_mac"}`
before enabling a `one_mac` routine. The schema browser's local shape check does not
establish event semantics. Role notification never claims mail: inspect `role_state`,
register and claim the queue, accept with `accept_role_message` before work, then
`ack_role_message` with the receipt after work. `send_role_message` sends role work;
`send_message` addresses a session or human.

### Command routines and chaining

For deterministic work use `action: command` and `command: [python3, scripts/check.py]`.
Each item is one argv element; no implicit shell, pipes, redirects or expansion. Commands
have no agent, task or mode, run on this Mac, and live under
`config/agents/_command/routines/<id>/routine.yml`. Program names use PATH. Keep scripts in the team content so teammates can run them;
program paths used as argv[0] must stay within that root. The working directory follows
`where.kind`: team content for `team_workspace`, or the managed clone for `repo:owner/repo`.
Set `maxRuntime` to bound execution.

Exit 0 succeeds; nonzero fails; cancelled and timed_out are separate outcomes. Read the
exit code and bounded stdout/stderr tails through `routine_runs` then `routine_run`.
The process gets `CODESENSE_ROUTINE_ID` and `CODESENSE_RUN_ID`; stdout becomes result_text.
A successor uses `after: {routine: check, on: succeeded}` in MCP or YAML. Create the
predecessor first, in the same space; cycles are refused. `on: finished` also continues
on unsuccessful terminal outcomes. The successor task or argv can use
`{{trigger.result_text}}`, `{{trigger.run_id}}`, `{{trigger.routine}}`, `{{trigger.phase}}`
and `{{trigger.detail}}`. `result_text` is what the agent produced; `detail` is CodeSense's
explanation of why the run ended (wake, machine and admission runs still use `result_text`).
See the [copyable metadata → command → agent chain](../../../arc-g-routines/command-routines.html#chain).
Choose an agent routine when the task needs judgment; use the
[agent design guide](../../../arc-e-agents/agent-design.html) for responsibilities.

## When a routine woke you, or started you

- **Woken** (`mode: wake`): the message starts with `[routine: <name> | run <id>]`. Do the
  task, then call `routine_done {sessionId, runId, outcome, summary}` to finish just that
  run. Don't call `signal_done` for it: that ends your whole session's task. If you call
  neither, the run is recorded `delivered`.
- **Started** (`mode: fresh`): the task is your whole prompt. Call `signal_done` with a
  `summary` when the routine wants a result (it becomes the next run's
  `{{last_run_summary}}`), or with status `needs-attention` and a question.
- Either way `routine_context {sessionId}` returns the routine, its pinned revision, the
  values your task was filled with and the last result.

The task is the whole contract: CodeSense adds nothing to it. Routines never deliver
results anywhere on their own: if the user wants a result posted, sent or written, the
task says so and names the tool.

## "What routines do I have", pause, change, run, delete

- `routine_list` (filter with `agent`) covers your private space and every synced team.
  Summarize each by name, agent, mode, `schedule_in_words`, enabled, `next_run_at` and
  how `last_run` went. `agent_get` lists one agent's routines. `routine_get` gives one in
  full with its last 5 runs; `routine_runs` has more history.
- Pause / resume: `routine_set_enabled`. It affects only this Mac's switch and is never a
  git change; a routine armed on another Mac is paused there.
- Change: `routine_update` with the `baseRevision` you last read and only the fields that
  change (`mode`, `machine`, `when`, `task`, …). `conflict` means re-read and reapply.
- Run once now: `routine_run_now`. Delete: `routine_delete` archives it; history stays.
  Archiving an agent archives its routines.
- Drafts: `routine_create` with `enabled: false`.

Anyone who can see a routine can change it. Edits are local edits to the context repo,
credited in its history; sharing them is a git push, which is yours to make when the user
wants it. Creating or changing a routine notifies no one.

People do all of this in the **Agents** tab: open an agent, and its **Routines** section
lists its routines as sentences ("Every weekday 9:00 · wake · Barb · on This Mac") with
**+ Add routine**. **Agents | Schedule** switches to a timeline of upcoming fires and recent
runs across agents, grouped by agent and machine. There is no separate Routines tab.

## Private dashboard results

For an agent-backed dashboard, `routine_create` accepts an inline JSON Schema in
`resultSchema` (`null` on update clears it). Bind the routine id to the page's
`routineResult`, `routineRefresh`, `routineStatus` and `routineCancel` (each takes `{key}`).
Cached reads never launch; make Refresh an explicit user action, and use
`onRoutineResult` to reread when a run finishes. The task must say: "Read `routine_context`
for the result schema, collect the data, publish `{sessionId, payload}` with
`routine_publish_result`, then call `signal_done` success." A candidate result is promoted
only on success; failure keeps the last good one. Never put credentials in a result.
Limits: schema 32 KiB, payload 1 MiB.

Coming later: routines armed on a machine's own server, so they fire while the desktop is
closed.
