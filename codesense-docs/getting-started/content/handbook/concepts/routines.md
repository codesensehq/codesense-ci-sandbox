# Routines

_A routine is a saved agent's standing task: a trigger, a task, and whether it wakes the running agent or starts a new one._

A **routine** pairs a trigger with a task, and belongs to a saved agent. When it fires, CodeSense gives that agent the task. There's no daemon and no polling: the app keeps the clock.

A routine is a small file under the agent it runs, `config/agents/<agent>/routines/<id>/routine.yml`, in your team context, so it's shared like the agent. You rarely write it by hand: an agent calls `routine_create`, or you open an agent in the **Agents** tab and choose **Add routine**. **Agents | Schedule** switches the tab to a list of every routine, its next run and how the last one went.

## When it fires

- **A schedule.** `when` takes a preset (`every 15m`, `hourly`, `daily 09:00`, `weekdays 08:30`, `weekly mon 17:00`) or a 5-field cron, read in the routine's time zone. Nothing fires more often than every 5 minutes.
- **`role_pending`.** A message is queued for a role in the routine's team (see [The Inbox](inbox.md)). In a run that starts a new session, `{{trigger.role}}` names the queue, so a controller agent can decide whether to start someone to work it. Each message fires a routine once.
- **`inbox_idle`.** One of the agent's sessions sits idle at its prompt with unread mail.
- **`agent_done`.** An agent finishes (it calls `signal_done`), optionally only one named agent.
- **By hand.** **Run now** on the routine, a dashboard's **Refresh**, or an agent calling `routine_run_now`. A routine with no trigger runs only this way.

`routine_create` sets the schedule. The event triggers are written in the `triggers:` list of `routine.yml`, and one routine can list several, such as a role queue plus an hourly check as a safety net.

## Wake or start a new

The **mode** says what a fire does:

- **wake** types the task into the agent's running session once it is idle, so the agent keeps everything it already knows. It never starts a second copy. If none is running, it starts one with the task as its first prompt. It types only when the agent is idle, nobody has typed in that tab for 3 seconds, and no prompt is waiting on you. The tab's caption then says which routine woke it.
- **start a new** (`mode: fresh` in the file) starts a new session of the agent on every fire, with the task as its whole prompt. Use it for a nightly report or a digest that wants a clean slate.

A wake into a busy agent waits for it to go idle, 30 minutes by default (`max_wait`), and fires that come meanwhile fold into it. Wake works for a saved agent that launches in a tab. CodeSense can't tell when a Codex session is idle, so a wake into a Codex session may come back undelivered.

## Limits

- **`maxRuntime`**: how long one run may take, `30m` by default, up to `24h`.
- **`overlap`**: what a fire does while a run is still going: `coalesce` (the default), `skip` or `queue`.
- **`missed`**: fires missed while the app was closed or your Mac was asleep are skipped (the default), or `run_once` runs one of them.
- **Wake limits**, for a wake that starts heavy work: `minGap` (the least time between two wakes), `maxWakesPerDay` and `maxWakes`. A light check needs none.
- **25 enabled routines** per Mac at most. It's a guard against runaway agent authoring, not a plan limit.

Every session also has a fixed backstop of 120 wakes an hour from all sources, which only catches loops.

## Results

The task is the agent's whole prompt, and its whole contract: CodeSense adds nothing to it. For a result, the task tells the agent to call `signal_done` with a one-line `summary`, or with status `needs-attention` and the question. That summary shows in the run history (`routine_runs`), and the next run can read it as `{{last_run_summary}}`. A wake that runs inside a session that keeps working calls `routine_done` with the run id instead.

A routine never delivers anything on its own. If something should be posted, filed or sent, the task says so and names the tool, and the agent needs that MCP server in its definition.

## Example: triage on a schedule

Ledgerline's `triage` agent lists its Slack, Jira, GitHub and Datadog MCP servers in its `agent.yaml`. A routine runs it every 30 minutes:

```js
routine_create({
  name: "Triage",
  agent: "triage",
  when: "every 30m",
  mode: "fresh",
  task: "Read what's new since {{last_run_at}}: #ledgerline-support in Slack, the LL project in Jira, new issues in ledgerline/web and ledgerline/api on GitHub, and Datadog monitors that fired. Write each real problem as a story in our format (get_story_format, then validate_story). For anything you can't decide, send me a needs-you message. Then call signal_done with a one-line count.",
})
```

> **Beta**
>
> On its schedule the agent reads each source through its own MCP servers, triages what's new into stories, and can start an agent on an urgent one with `open_workspace`. CodeSense never talks to Slack, Jira, GitHub or Datadog itself; the agent does, with the access you gave it. No full triage run like this has been recorded end to end.

## Where a routine fires

A routine's clock runs in the CodeSense app on your Mac, or on a machine's terminal server when `routine_arm_on` arms it there (schedule triggers only, `one_mac`, no `budget_usd`). A shared job (`runsOn: one_mac`, the default) runs once for the team, from one Mac or machine. A recipe (`each_mac`) runs on Macs only, and is off everywhere until each teammate turns it on for their own Mac with **Run on my Mac**. The one exception: a shared job converted to a recipe stays on for the Mac that ran it. Turning a routine on or off is a setting on your Mac, never a git change.

`machine` says where a wake looks for its agent: `local` (this Mac), a machine id, or `any`.

> **Beta**
>
> `routine_arm_on` with a machine id moves a routine onto a remote machine: that machine's terminal server fires it on its own clock, whether or not any Mac is open. Your Mac prepares the agent there and records the runs. A routine armed on a machine runs on its schedule only, without the event triggers. See [Remote machines](remote-machines.md).

> **Note**
>
> For "check back on this in 20 minutes" inside one conversation, an agent sets a session timer with `schedule_wakeup` instead. It types the prompt into that same session and ends with it.

The full guide ships in the app's docs: **Routines**.

## Your first routine

A routine is a saved agent’s standing task: run by hand, on a schedule, or on an event. It lives under the agent in `config/agents/<id>/routines/<rid>/routine.yml`, and every fire is recorded in its run history.

```yaml title="web/config/agents/digest/routines/daily-digest/routine.yml"
id: daily-digest
name: Daily digest
mode: fresh
task: Summarize yesterday's merged PRs, then call signal_done.
where:
  kind: team_workspace
triggers:
  - schedule:
      cron: "0 9 * * *"
      tz: UTC
policy:
  max_runtime: 30m
revision: 1
```

Or ask an agent; it calls `routine_create` with `"when": "daily 09:00"`, and `dryRun` first if you want to check it. Besides a schedule, a routine can wait for an idle agent with unread mail (`inbox_idle`), mail for a role (`role_pending`) or another agent finishing (`agent_done`).

## Change a field, start the work

Set `status: qa` on a story and a routine wakes the QA agent with the same story and context. Any field, any value: a routine can start an agent, run a command or chain steps.

```text title="routine_create · wake QA when a story enters qa"
{
  "name": "QA on entry",
  "agent": "qa",
  "task": "A story moved to qa. Test it, then signal_done.",
  "metadataTriggers": [{
    "selection": { "where": { "kinds": ["story", "bug", "spike"] } },
    "target": { "key": "status", "op": "eq", "value": "qa" }
  }]
}
```

It fires when a story enters the value, not for stories already there. Add `previous` to fire only on one transition, such as review → qa. A command routine runs a program instead of an agent, and `after` chains it to another routine.

## Watching a run

Routines run interactive sessions of the official CLI, so you can open one and watch it work. Results go where the task says; a routine can also publish a structured result that dashboards read.

`routine_runs` lists a routine’s fires with their outcome, and `routine_result` reads the result it published. For a reminder inside one conversation, an agent sets a session timer with `schedule_wakeup` instead.
