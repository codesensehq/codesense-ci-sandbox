---
name: agent-design
description: Design a small CodeSense agent team with the user. Use when asked to "set up my agents", "design a team", "what agents should I have", or improve an existing team. Discover, propose, preview, ask before saving, prove and hand over.
---

# Agent design

Use CodeSense MCP tools. You do not need CodeSense source. The playbook is
[Agent design](../../../arc-e-agents/agent-design.html). Cite the page and section
used at each step. Read the running tool's input schema before calling it. If docs
and the running app differ, report the mismatch and follow the tool contract.

## 1. Orient

Call `codesense_overview`. Read its docs location and version information when
present; do not invent a version if absent. Read the guide above. To find the
installed guide, use `query_context` with
`where: {orgs: ["codesense/docs"]}` and `text: "agent design"`. Read the matching
page using its returned path, with `read_rendered_document` or an available file
reader. Resolve a mismatch between docs and the app before changing the setup.

Call `describe_setup`, `agent_list` and `routine_list`. Read reusable definitions
with `agent_get`. Find CodeSense tools with `codesense_search_tools`. Report errors
in the current setup before editing that part of it.

## 2. Interview

Follow the guide's **Discover the work and data** section. Ask at most three
questions at once. Stop when you can write a useful design:

- What repeats, how often, and what result counts as good?
- What needs a human? What may happen unattended? What are the hard limits?
- Which tools, repos and machines are involved?
- Where does data live, who owns it, and how sensitive is it? Does this task need
  read or write access? Which MCP servers and tools are actually connected?
- What is the time and money budget? What should the user see and how do they pause it?

Minimize data and access per seat. Prefer selected fields, counts and ids over raw
personal records. Never request secrets in chat or copy them into design files.

## 3. Propose

Follow **Shape the smallest team** and **Choose models and budgets**. Prefer fixed
scripts, checks and command routines before model calls. Offer one or two patterns
with trade-offs. Start with one seat when it can do the job.

Show a table: seat, agent, model choice, role, routine, pane, tool access or grants,
risk tier and why. Add cost assumptions, autonomy limits, stop conditions and proof
cases. Compare model quality, speed and cost on the task; use `model_rates` and
`session_usage` when available. Missing prices are unknown, not zero. Choose by
task type, not by a blanket Claude or Codex ranking.

Show a **dry-run plan**: exact previews, proposed saved resources, expected outputs,
what would launch or cost money, and what will stay disabled. Read-only previews
may run now. Get the user's explicit approval of the design and plan before saving
anything or launching a proof. Existing approval applies only to its stated scope.

## 4. Record

Follow **Write the design down**. After approval, write a short design note in
content the user owns. Record goals, data sensitivity, decisions and their reasons,
seats, tool boundaries, budget, risk tiers, handoffs, proof cases and who approved.
Keep a list of created ids so the user can change or remove the setup.

The optional `team-design.yaml` and human-readable `DESIGN.md` are a structured
record of intent. Use the plain note unless the user already has that format. Refer to agents and routines by id. Do not duplicate
fields from their real `agent.yaml` and `routine.yml`. Never save data or secrets
in a design. Keep personal and finance notes in the private layer.

## 5. Apply

Follow **Build in small steps**, in this order:

1. Agents: prefer reuse or a copy. Call `agent_preview`, check manifest errors,
   warnings, context and provided skills. Use `agent_create` with `dryRun: true`
   before saving. Do not proceed with unresolved errors. Keep returned ids.
2. Roles: newer apps offer `role_save`; check the running tool list. If absent,
   copy the starter's matching role YAML. Use the selected team key and role
   config. A default handler does not launch itself. A worker still registers with `register_role`.
3. Routines: use `routine_create` with `dryRun: true`, then save with
   `enabled: false`. Bind the actual saved agent. Newer apps support command
   actions, argv commands, metadata triggers and `after` through MCP. Check the
   running schema; if advanced fields are absent, use an approved schedule-only
   agent routine that checks state. Event
   triggers such as role-pending currently need YAML. Read the
   [solo-founder walkthrough](../../agents/solo-founder/README.md) before copying
   its wake routine. Never invent an unsupported event parameter. If
   `open_workspace` lacks its queue-selection field, omit that field and keep
   explicit `register_role` in the saved prompt.
4. Panes: only if needed, use `pane_write` with the agreed capabilities and grants.
5. Inheritance: only if needed and approved, use `set_team_parent`. Only content
   follows the parent chain. Parent config does not become child config.
   A child receives only the content paths its ancestors declare in `config/sharing.yml`;
   each team in between must share them onward.
   Preview from the child team.

Name every resource clearly. Add only startup/context paths that exist for this
user. Never assume a workflow skill or external MCP server is installed. Use the
starter's self-contained prompts when they fit. Saving is a local context-repo
edit; sharing through git is a separate action. Ask before enabling each routine.

## 6. Prove

Follow **Prove it works**. A creation dry run validates setup and writes nothing.
`routine_run_now` performs real work; it has no dry-run flag.

After approval, run one bounded end-to-end normal case and one stop case per seat.
Send labelled test work with `send_role_message`. Verify registration, claim,
acceptance, acknowledgement and the final result. Check the handoff in a fresh
session, with the selected context. Confirm that missing approval stops action.

Inspect `routine_runs` and `routine_run`. For story runs, use `list_runs` and
`read_run_evidence` when evidence exists. Open an optional pane with `pane_open` and
check the visible result. Show the user the evidence and any unverified behavior.
A preview is not proof of a live handoff. On failure, pause, simplify and retry the
bounded case; ask the user when the agreed retry budget is exhausted.

## 7. Hand over

Give the user the design location, saved ids, what runs when, expected spend and
what needs approval. Show how to pause with `routine_set_enabled`, and how to read
or change definitions with `agent_get` and `agent_update` using the current revision.
Explain how to remove this setup without touching other work. Report what was
proved, what remains disabled and what still needs a real run.

## 8. Revisit

Follow **Iterate from evidence** and **Learning and context**. Offer a manual weekly
or monthly review, or an optional review routine within the user's budget. Ask
before scheduling it. Read structured usage and outcome signals, compare with the
goals, and propose one change at a time. Replay the agreed cases before applying.

A fuller loop adds a fresh-context evaluator and a steward that proposes changes
for approval. Its monthly memory prune keeps, merges, summarizes or archives notes
within a per-agent context budget. It does not auto-apply policy. Use small notes
with provenance. Quarantine untrusted-source facts until a human promotes them.
Keep personal and finance memory private. This skill does not install evaluator or
steward skills.

## Guardrails

- CodeSense provides mechanisms. The user and agents choose policy. Do not present
  a pattern, model choice or workflow as an engine requirement.
- Save and launch only within the approved design. Confirm new schedules, paid
  work and any wider access. A preview does not authorize creation.
- Safety tiers: R0 reads and analyses. R1 makes bounded, reversible private drafts.
  R2 outward actions need explicit human approval of content, recipients and
  limits. R3 money movement, trading and deletion need explicit approval of the
  exact action and hard limits. Keep R2/R3 actions out of unattended seats.
- Tiers and prompts are not runtime enforcement. Restrict tools and native CLI
  permissions. Do not give one agent or an unbounded message chain both untrusted
  input, sensitive data and outward tools. Use validated ids and bounded fields.
- Keep the design in content the user owns. Only content inherits today; do not
  promise config inheritance or treat a parent relationship as a privacy boundary.
- Claude agents call `who_am_i` with no arguments. Their MCP connection supplies
  `sessionId` for `who_am_i`, `signal_done` and `routine_publish_result` when
  omitted. Headless permissions can deny shell variable expansion, so a Claude
  seat's bootstrap must not depend on `echo`.
- Codex agents pass your own sessionId to `send_message`/`signal_done`.
  The wire key is `session_id` for `send_message` and `reply`, and `sessionId` for
  `signal_done`. For every `send_role_message`, set `sender` to your own handle
  from `who_am_i.handle`. First run
  `echo $CODESENSE_TAB_SESSION_ID` in your shell. If empty, match your own tab name
  and workspace in `list_sessions` and take `launchTelemetry.sessionId`. If there
  is no unique match, ask instead of guessing. Pass that id to `who_am_i` and
  keep its returned handle for role-message attribution and replies. Inspect the
  running schema. Never send as an implied human or another
  session.
- To stop future starts, call `routine_set_enabled` with `enabled: false` for each
  routine. To stop running sessions, take each intended `handle` from
  `list_sessions`, then call `close_terminal_tab` with that handle. It ends the
  CLI and keeps the worktree files. These are separate actions; do not promise
  a single kill switch. Close only sessions the user intended to stop.
- Keep scope and release decisions with the user. Pause on unclear approval.
  Never claim proof from configuration, a dry run or missing evidence.
