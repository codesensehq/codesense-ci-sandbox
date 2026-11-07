# Solo founder: planner, builder and reviewer

Start with the [agent-design guide](../../../arc-e-agents/agent-design.html) and
[skill](../../skills/agent-design/SKILL.md). This is a starting design, not an
installed team. The founder approves scope and releases. A notes backlog must
become a managed pipeline story before a builder launches: read `get_story_format`, choose a
configured repo with `list_source_repos`, use `validate_story`, save in team
content and confirm `query_context` finds its story id. Keep one assignment active
until the handoffs are proved.

The trio starts from a design that passed agent previews and creation/schedule dry
runs. The adapted definitions also passed live agent previews and creation dry runs.
Preview them again in your team. These checks did not launch workers or prove
live role delivery. No external server or extra
workflow skill is required by these prompts.

## Files and destinations

| Example | Copy to, relative to the team root |
| --- | --- |
| [solo-founder-planner/agent.yaml](solo-founder-planner/agent.yaml) | `config/agents/solo-founder-planner/agent.yaml` |
| [solo-founder-builder/agent.yaml](solo-founder-builder/agent.yaml) | `config/agents/solo-founder-builder/agent.yaml` |
| [solo-founder-reviewer/agent.yaml](solo-founder-reviewer/agent.yaml) | `config/agents/solo-founder-reviewer/agent.yaml` |
| [roles/solo-founder-planner.yaml](roles/solo-founder-planner.yaml) | `content/roles/solo-founder-planner.yaml` |
| [roles/solo-founder-builder.yaml](roles/solo-founder-builder.yaml) | `content/roles/solo-founder-builder.yaml` |
| [roles/solo-founder-reviewer.yaml](roles/solo-founder-reviewer.yaml) | `content/roles/solo-founder-reviewer.yaml` |
| [planner wake routine](solo-founder-planner/routines/solo-founder-queue-wake/routine.yml) | `config/agents/solo-founder-planner/routines/solo-founder-queue-wake/routine.yml` |

The agent folders follow the same layout as the manager example. Roles live in
content; routines belong under the agent they run. Copying the example directory
under content alone does not install agents or routines.

## Preview before saving

Ask the user to approve names, model choices, tool access, a run budget and one
proof case per seat. Inspect `agent_list` first. Do not overwrite an existing id.

For each YAML definition, form an `agent_preview` payload. Most fields keep their
names. Map `mcp.servers` to `mcpServers` and `default_prompt` to `defaultPrompt`.
Leave out `revision`. Pass the chosen `org`, `team` and `scope` as MCP parameters.
Use the same payload for `agent_create` with `dryRun: true`, then save after approval.

These examples use Codex with native approval and sandbox settings. The reviewer
uses a read-only sandbox; planner and builder use workspace-write. Tool access is
still a separate boundary. A review-only prompt is not an MCP write restriction.
The model is left to the user's CLI default; choose model args after testing the
seat's cases. Explicit `startup: []`, `context: []` and no external MCP servers keep
the coordination example small. Before real project work, add the required project
instructions and skills by their existing paths, preview again and test the result.
An empty context list does not remove files already in a workspace.

For roles, pass each role file's fields as `config` to `role_save`, with
`teamKey: "<org>:<team>"`. Replace `defaultHandler: null` with the full saved agent
key returned by creation. For a manual file copy, make that substitution first.
Newer apps offer `role_save`; check the running tool list. If absent, copy the
matching role YAML to the destinations above. A default handler does not launch
itself. Planner and reviewer register exclusively; the builder role is shared so you can later add a bounded pool.

## One immediate role wake

The planner routine uses the `role_pending` event without a `role` filter, so it
watches every queue in its team. The task inspects `{{trigger.role}}` and
the three queues, then explicitly launches or notifies the relevant worker. It
claims only planner work. Unrelated role mail can cause a no-work wake, so count
that cost and keep the wake limit deliberate.

MCP creation accepts `events: [{kind:"role_pending"}]` for this wildcard behavior.
Use `events: [{kind:"role_pending",role:"reviewer"}]` for a routine that should hear
only reviewer mail. Keep the starter's wildcard: it dispatches across three queues.
Preview with `routine_create {dryRun:true, ...}` and inspect `data.triggers` before
saving disabled. `routine_update` preserves events when omitted/null, clears these
event kinds with `[]`, and replaces them with a nonempty list; schedules and metadata
triggers remain. See the [event cookbook](../../../arc-g-routines/trigger-cookbook.html#events).

Copy the routine under the planner only after approval. `runs_on: each_mac` leaves
it off on each Mac until someone enables it there. Do not add an `enabled` field to
the YAML; enable state is local runtime state. Check `routine_list` and
`routine_get`, then enable on one Mac with `routine_set_enabled` after approval.
A creation dry run writes nothing. `routine_run_now` is a real run.

Role events need a desktop; do not arm this event routine on a remote session
server. The routine has no periodic backstop. The planner and workers must
explicitly launch or notify the next seat. The engine suppresses cycles within a
routine run chain, so another queued message is not a promise of another wake.
When a wake limit or failed handoff stops progress, inspect the queues and resume
manually. For an optional morning planning schedule, ask before adding one.

## Launch fields

Use actual saved ids and an existing managed story id. These are the current
`open_workspace` wire fields; placeholders below are illustrative and must be
replaced. Newer apps support `queue_role`. Check the running schema; if it is
absent, omit it and let the saved prompt call `register_role` explicitly.
A team-workspace planner launch:

```json
{
  "target": "team-workspace",
  "team": "<team>",
  "name": "founder-planner",
  "agent": "<saved planner key>",
  "queue_role": "solo-founder-planner",
  "launched_by": "<your session id>",
  "mode": "headed",
  "max_runtime": "20m"
}
```

The examples omit `initial_prompt` so the saved `default_prompt` runs. A launch
prompt replaces that bootstrap. Send the assignment through the role queue before
launching; the saved prompt registers the seat and claims its message.

A builder or reviewer runs in the assigned story workspace:

```json
{
  "target": "story",
  "story_id": "<existing managed story id>",
  "team": "<team>",
  "name": "founder-builder",
  "agent": "<saved builder key>",
  "queue_role": "solo-founder-builder",
  "launched_by": "<your session id>",
  "machine_id": "local",
  "mode": "headed",
  "max_runtime": "30m"
}
```

Use the reviewer key, name and queue for review. `role` is only a display label;
`queue_role` selects a queue at launch. The prompts still call `register_role` and
use its returned instance and generation for claims and receipts. These seats use
Codex. Call `who_am_i` after reading your id with `echo $CODESENSE_TAB_SESSION_ID`. If empty, match your
own tab name and workspace in `list_sessions` and use `launchTelemetry.sessionId`.
For Codex, pass your id as `session_id` to `send_message` and `reply`, and as
`sessionId` to `signal_done`. Set `sender` on every `send_role_message` to your own
handle from `who_am_i.handle`. Keep that handle from your bootstrap call so role
handoffs and replies retain your identity.

For a Claude adaptation, replace the shell bootstrap with `who_am_i` with no
arguments. Claude's MCP connection supplies `sessionId` for that call,
`signal_done` and `routine_publish_result` when omitted. Do not make a Claude
seat depend on `echo`: headless permissions can deny variable expansion.

## Prove and hand over

1. Preview all three definitions and resolve manifest errors. Save only after approval.
2. Send a labelled test to each role with `send_role_message`. Include a real story
   id for builder and reviewer work. Watch the immediate planner wake and each
   explicit launch or notification. A queued message is not evidence of delivery.
3. Confirm each worker registers, claims, accepts and acknowledges with its receipt.
   Check one visible defect and one green verdict. Stop after two fix rounds and
   ask the founder if the result is still unclear.
4. Check `routine_runs`, `routine_run`, and story evidence with `list_runs` and
   `read_run_evidence` where available. Show the founder what happened and its cost.
5. Keep outward actions disabled pending explicit approval. Show how to pause with
   `routine_set_enabled`. Keep the saved ids and unverified behavior in the design note.
