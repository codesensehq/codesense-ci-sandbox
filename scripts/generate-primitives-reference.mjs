#!/usr/bin/env node
// SPDX-License-Identifier: MIT
// scripts/generate-primitives-reference.mjs (SW-ODC-5)
//
// Reads the shared source models for the primitives an HTML dashboard can reach:
//
//   src-tauri/src/verbs/registry.generated.json         — verbs, schemas and classes
//   dashboard_kit/codesense.d.ts                       — exact SDK members and signatures
//   src/features/dashboard/components/doc-theme.ts    — TOKEN_SOURCES (the token
//                                                        name + its fallback chain)
//   dashboard_kit/tokens.css + app palettes            — kit tokens and light/dark defaults
//   src-tauri/src/http/routes/viewer.rs                — every `codesense:*`
//                                                        postMessage event the host
//                                                        pushes or the shim emits
//
// Emits a single committed JSON doc at
//
//   codesense-docs/getting-started/content/generated/primitives.json
//
// with six keyed sections consumed by `primitives-explorer.html`:
//
//   {
//     "schema": 1,
//     "generatedAt": "deterministic",
//     "counts": { "invoke": N, "kit": N, "classes": N, "tokens": N, "events": N, "contexts": N },
//     "invoke":   [ { name, category, write, audited, readOnlyDocsSafe, description, args, returns, sourceRef, starter? } ],
//     "kit":      [ { name, category, signature, wraps, description, example, sourceRef, starter? } ],
//     "classes":  [ { name, category, description, appliedBy, tokensUsed, example, sourceRef } ],
//     "tokens":   [ { name, role, swatchType, fallbackChain, sourceRef } ],
//     "events":   [ { name, direction, payload, timing, description, example, sourceRef } ],
//     "contexts": [ { name, category, route, sandbox, bridgeFull, snapshotAvailable, detection, description, features, sourceRef } ]
//   }
//
// AC-8 (SW-ODC-5): every `invoke` entry carries `readOnlyDocsSafe: boolean`.
// `true` iff the verb is in `READ_VERBS` or `READ_ACTION_VERBS` (the audit-exempt
// reads + the audited reads). `false` for every WRITE class (METADATA / SPAWN /
// SEND / MANAGEMENT). Consumed by SW-ODC-4 and SW-ODC-24 dashboards to gate the
// "Try it" button when the user is pinned to the read-only docs org.
//
// Modes:
//   write (default) — regenerate `generated/primitives.json` from the shared sources.
//   --check          — exit non-zero if the committed JSON differs from a fresh
//                      render. Error message names (i) the drifted entity (invoke
//                      verb / kit method / cs-* class / token / event), (ii) the
//                      file to edit, (iii) `pnpm gen:docs`.

import ts from "typescript";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { listInputs } from "./lib/generator-inputs.mjs";
import { buildPageSdk, publicSdkMembers } from "./lib/page-sdk.mjs";
import { sourceTree, tokenModel } from "./lib/pane-references.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");

// ---------------------------------------------------------------------------
// Hand-curated descriptions and examples
//
// API membership, schemas, signatures and audit facts come from the shared
// registry/SDK models. These tables add explorer copy without owning API facts.
// ---------------------------------------------------------------------------

/** Starter primitives — pinned to the top of their tab (brief §signature-moves). */
const STARTERS = new Set(["queryContext", "subscribeContext", "openWorkspace"]);

/**
 * @type {Record<string, {
 *   description: string,
 *   examples?: Array<{args?: unknown, result?: unknown}>,
 * }>}
 */
const INVOKE_META = {
  "commands.register": {
    description: "Replace this dashboard's active command set with ids declared in its manifest.",
    examples: [{ args: { ids: ["refresh"] }, result: { active: ["refresh"] } }],
  },
  "commands.list": {
    description:
      "List dashboard commands with their bindings, scope, active state and human-only status.",
    examples: [{ args: {}, result: [] }],
  },
  "commands.run": {
    description: "Request dispatch of an id from commands.list, with optional command arguments.",
    examples: [{ args: { id: "dashboard/refresh" }, result: { requested: true } }],
  },
  showMenu: {
    description:
      "Open a host-rendered menu outside the frame. Built-ins use host labels, availability and confirmations. A second menu dismisses the first.",
  },
  showPopover: {
    description:
      "Open a host agent popover outside the frame: agent-quick-send (the quick view) or agent-inbox-chat (the agent's inbox chat). Unknown kinds reject; dismissal returns focus to anchorId.",
  },
  focusOut: {
    description:
      "Return focus to the previous host control, or the active terminal if it has gone. Available only to a rendered frame.",
  },
  focusTerminal: {
    description:
      "Focus the active workspace terminal for this frame's team. Available only to a rendered frame.",
  },
  subscribe: {
    description:
      "Subscribe to a host projection. Returns {topic, viewKey, rev, snapshot}; codesense:delta carries {topic, viewKey, rev, upserts, removes, requestId?}. Hidden frames pause and resync on show.",
  },
  page: {
    description: "Load older rows from an inbox projection cursor; at most 200 rows per page.",
  },
  unsubscribe: {
    description: "Stop watching a projection topic in this frame.",
  },
  setUiState: {
    description:
      "Persist a shared ui:<slotId> patch and echo its requestId in a delta. Uses host app-state storage, separate from page-private prefs.",
  },
  // ── READ verbs ─────────────────────────────────────────────────────────
  getDependencyStatus: {
    description:
      "Zero-arg health check — proves the bridge seam end-to-end. Returns `{ok: boolean}` without touching anything sensitive.",
    examples: [{ args: {}, result: { ok: true } }],
  },
  queryContext: {
    description:
      "Context metadata search. The workhorse read for every dashboard that needs to know what docs / orgs / teams / tiered artifacts exist. Pairs with `subscribeContext` for push updates.",
    examples: [
      {
        args: { text: "DEMO" },
        result: {
          rows: [{ path: "stories/DEMO-5.md", title: "primitives-explorer" }],
          scopeOrgs: ["codesense"],
          truncated: false,
        },
      },
    ],
  },
  context: {
    description:
      "The dashboard's own location — resolved from the host-supplied doc path, never from a doc-supplied one. Returns `{org, team, layer, contentRoot}` or `null` for a page outside every content root.",
    examples: [
      {
        args: {},
        result: {
          org: "codesense",
          team: "default",
          layer: "team",
          contentRoot: "/…/content",
        },
      },
    ],
  },
  sessions: {
    description:
      "List live and recent sessions. Pairs with `subscribe('sessions', {onChange})` for versioned payload deltas.",
    examples: [
      {
        args: {},
        result: [{ handle: "DEMO-5", sessionId: "sess-7b2", status: "live" }],
      },
    ],
  },
  agents: {
    description:
      "Every launchable agent visible to this team. Each row carries scope / team / `runningNow` so a dashboard can render honest affordances.",
    examples: [
      {
        args: {},
        result: [{ key: "amelia", name: "Amelia", runtime: "codex", runningNow: 2 }],
      },
    ],
  },
  agent: {
    description: "A single agent by key, with every running instance.",
    examples: [{ args: { key: "amelia" }, result: { key: "amelia", runningNow: 1 } }],
  },
  inbox: {
    description:
      "Team-scoped inbox query with exact full-history conversation summaries. Filter by agent, session, role, scope or conversation. Page older mail with conversation, before and limit:50; pairs with `onInboxChanged(cb)` for push.",
    examples: [
      {
        args: { role: "builder", limit: 5 },
        result: {
          messages: [{ messageId: 204, conversation: "#builder", text: "Ready" }],
          conversations: [],
          humanUnread: 0,
          hasMore: false,
        },
      },
    ],
  },
  inboxRollup: {
    description:
      'Every synced team’s newest mail, exact full-history conversation summaries and merged role queues. Page each team with inbox({org,team,conversation,before,limit:50}). Messages and updates carry team: {org, team}. Requires a private pane marked <meta name="codesense:surface" content="human-inbox">; the tag on a team pane grants nothing. Re-read on onInboxChanged for mail, onAgentsChanged for role changes, and window message codesense:agent-activity-changed for notes, across every team.',
    examples: [{ args: {}, result: { messages: [], roles: [], updates: [] } }],
  },
  roles: {
    description: "Agent role queues (`role_name` → `queue_size`, oldest `last_touched`, …).",
    examples: [{ args: {}, result: [{ role_name: "builder", queue_size: 3 }] }],
  },
  updates: {
    description: "Agent activity feed. Pairs with `codesense:agent-activity-changed`.",
    examples: [{ args: {}, result: { updates: [] } }],
  },
  humanStatus: {
    description:
      "The user's own presence — `online` / `away` / `busy` / `offline` + optional note.",
    examples: [{ args: {}, result: { status: "online" } }],
  },
  watchInbox: {
    description:
      'Handshake — resolves `{subscribed: "codesense:inbox-changed"}`. The host already pushes the event; a doc opts in by listening.',
    examples: [{ args: {}, result: { subscribed: "codesense:inbox-changed" } }],
  },
  watchAgentInbox: {
    description:
      "Scoped inbox watch. Resolves `{subscribed, agentId?}`. The host pushes `codesense:inbox-changed`.",
    examples: [
      {
        args: { agentId: "amelia" },
        result: { subscribed: "codesense:inbox-changed", agentId: "amelia" },
      },
    ],
  },
  watchAgentActivity: {
    description: 'Handshake — resolves `{subscribed: "codesense:agent-activity-changed"}`.',
    examples: [{ args: {}, result: { subscribed: "codesense:agent-activity-changed" } }],
  },
  routines: {
    description: "Every routine this user sees. Pairs with `onRoutinesChanged(cb)` for push.",
    examples: [{ args: {}, result: [{ id: "acme/web/rt-nightly", name: "nightly sweep" }] }],
  },
  routine: {
    description: "A single routine by id.",
    examples: [
      {
        args: { key: "acme/web/rt-nightly" },
        result: { id: "acme/web/rt-nightly", name: "nightly" },
      },
    ],
  },
  routineRuns: {
    description:
      "Paginated run history for one routine (newest first). `before` is the previous page's `nextBefore`.",
    examples: [
      {
        args: { key: "acme/web/rt-nightly", limit: 5 },
        result: { rows: [{ runId: "run-42", outcome: "ok" }], nextBefore: null },
      },
    ],
  },
  routineRun: {
    description: "One routine run — the run plus its routine.",
    examples: [{ args: { runId: "run-42" }, result: { runId: "run-42", outcome: "ok" } }],
  },
  runs: {
    description:
      "Latest outcome for every agent run visible to this team. Use `run({runId})` for the full transcript + artifact (capped at 512 KiB).",
    examples: [{ args: {}, result: [{ runId: "run-42", outcome: "ok", sessionId: "sess-7b2" }] }],
  },
  run: {
    description:
      "A single agent run: `{outcome, transcript, artifact, artifactTruncated}`. Artifact capped at `maxArtifactBytes` or 512 KiB.",
    examples: [
      {
        args: { runId: "run-42" },
        result: { outcome: "ok", artifactTruncated: false },
      },
    ],
  },
  runEvidence: {
    description:
      "Run evidence / testing data — `{header, run, perNodeResults, authority}` or `null`. `storyId` defaults to the run's own story.",
    examples: [{ args: { runId: "run-42" }, result: null }],
  },
  runWorkProduct: {
    description: "Branch produced by an agent run: `{branch}` or `null`.",
    examples: [
      { args: { runId: "run-42" }, result: { branch: "feat/DEMO-5-primitives-explorer" } },
    ],
  },
  worktrees: {
    description:
      "Managed worktrees on this Mac — one row per managed checkout. Pairs with `onWorktreesChanged(cb)`.",
    examples: [{ args: {}, result: [{ id: "wt-demo-1", path: "/Users/…/worktrees/demo-1" }] }],
  },
  canPasteToTerminal: {
    description:
      'Whether `sendPrompt({handle: "active", text, submit: false})` has a terminal to paste into: the selected terminal tab of the person\'s focused workspace. Pairs with `onPasteTargetChanged(cb)`.',
    examples: [{ args: {}, result: true }],
  },
  machines: {
    description:
      "Machines this team can see (local + remote private-account). Pairs with `onMachinesChanged(cb)`.",
    examples: [{ args: {}, result: [{ id: "mb-01", name: "local-mbp" }] }],
  },
  sessionUsage: {
    description:
      "Per-session token usage and spend for the sessions CodeSense launched, on this Mac and the remote machines it connects to, with per-day totals. Each session's `machineId` is the machine it ran on, as `machines()` names it (`local` for this Mac). Takes optional `sinceMs` and `untilMs` (local days: `sinceMs`'s day included, `untilMs`'s day excluded), `sessionId`, `cwd`, `story` (one story id) and `limit`. `costUsd` is `null` only when a call used tokens on a model with no rate. Every call the CLI reports counts, including Codex's start-up cache warm-up, which Codex's own total leaves out.",
    examples: [{ args: {}, result: { sessions: [], days: [], totals: {} } }],
  },
  modelRates: {
    description:
      "Every stored rate (USD per million tokens), by model prefix and then by day: the seeded list prices (from `1970-01-01`) and the ones `setModelRate` set. A model's rate on a day is the latest one from on or before that day, under its longest matching prefix. `cacheReadPerMtok: null` is 0.1x input; `long: null` is one price at every prompt length.",
    examples: [
      {
        args: {},
        result: [
          {
            modelPrefix: "gpt-6-sol",
            effectiveFrom: "1970-01-01",
            inputPerMtok: 2,
            outputPerMtok: 10,
            cacheReadPerMtok: null,
            long: {
              fromTokens: 272000,
              inputPerMtok: 4,
              outputPerMtok: 15,
              cacheReadPerMtok: null,
            },
            source: "seed: standard list price from the vendor's pricing page",
          },
          {
            modelPrefix: "gpt-6-sol",
            effectiveFrom: "2026-10-17",
            inputPerMtok: 2,
            outputPerMtok: 10,
            cacheReadPerMtok: null,
            long: null,
            source: "set by the user",
          },
        ],
      },
    ],
  },
  setModelRate: {
    description:
      "Sets a model's rate (USD per million tokens) from an effective day, optionally with its own cache-read price and the higher prices of a long prompt. A rate is the whole price from its day on: a price it leaves out falls back to its default (cache reads at 0.1x input, one price at every prompt length), even where the earlier rate named one. Reprices the rate-table costs it covers; a cost the CLI reported is not changed. Rejects an invalid prefix or rate, or an unknown key, with an `invalid:` error.",
    examples: [],
  },
  getEpicHandle: {
    description:
      "The containing epic's `epicDocPath` for this dashboard, or `null`. Lets a dashboard address its epic's running agent via `sendToSession(handle)` without a machine-specific absolute path.",
    examples: [{ args: {}, result: "epics/DEMO.md" }],
  },
  getDocDir: {
    description:
      "The absolute directory of this dashboard doc, or `null`. Lets a board build an absolute path to a sibling story for `runSkill` without an open epic workspace.",
    examples: [{ args: {}, result: "/Users/dev/…/content" }],
  },
  validateYaml: {
    description:
      "Validate a YAML / frontmatter / JSON snippet against a known schema. It powers the schema-browser.html live validator. The handler always answers `unavailable:` today; the page validates client-side. `kind` is one of the schema slugs in `generated/schemas.json` (e.g. `agent`, `routine`, `org`, `teams`, `worktree`, `bundle`, `story-frontmatter`, `epic-frontmatter`).",
    examples: [
      {
        args: { kind: "agent", text: "runtime: typescript\n" },
        result: "Error: unavailable: validateYaml has no app validator; validate in the page",
      },
    ],
  },
  prefs: {
    description:
      "This dashboard's own settings object ({} at first). Kept in the user's app state per dashboard instance (a pane's own file, a centre dashboard's path, an inbox message), so it survives restarts; a team copy and a private copy each have their own.",
    examples: [{ args: null, result: { details: "compact", hideDone: true } }],
  },

  // ── METADATA (write) ────────────────────────────────────────────────────
  setPrefs: {
    description:
      "Merge keys into this dashboard's own settings (a key set to null is removed) and resolve the result. Plain JSON only (`invalid:` otherwise); 16 KB per dashboard (`too-large:` past it), 512 KB across all dashboards (oldest dropped). Its own budget: 30 calls per 10 s (`denied: prefs budget`); not audited.",
    examples: [{ args: { details: "full" }, result: { details: "full", hideDone: true } }],
  },
  setStatus: {
    description:
      "Flip a story or epic's `status` frontmatter. Thin wrapper over `setField` with managed-target confinement and batch support.",
    examples: [
      {
        args: { path: "stories/DEMO-5.md", status: "in-progress" },
        result: { ok: true },
      },
    ],
  },
  setField: {
    description:
      "Write frontmatter keys atomically, byte-preserving the rest of the doc. Managed-target confined; `null` removes a field. Supports batch via a `fields` object.",
    examples: [
      {
        args: { path: "stories/x.md", field: "status", value: "in-progress" },
        result: { ok: true },
      },
    ],
  },
  routeToContext: {
    description:
      "Route a code file to context. Rejects (deny-by-default) if the source is not routable. Affirmative outcomes resolve with a `RouteOutcomeDto`.",
    examples: [
      {
        args: {
          codeRepoPath: "src/app.ts",
          codeRepoSlug: "codesense",
          logicalName: "app",
        },
        result: { outcome: "routed" },
      },
    ],
  },
  draft: {
    description:
      "Create a new managed doc at tier `level` derived from `from`. `from`'s directory is the managed-target wall; a nonce in the filename prevents collisions.",
    examples: [
      {
        args: { level: "story", from: "epics/DEMO.md" },
        result: { path: "epics/story-draft-k2a7b3.md" },
      },
    ],
  },
  archive: {
    description:
      "Archive and read human-directed messages in one transaction; returns newly archived rows. Agent mail, already archived and unknown ids are unchanged. Store failures reject so pages can roll back optimistic state. On a human-inbox surface, pass both {org, team} to act in that synced team; omit both to keep the pinned page team. Partial pairs and explicit targets on other surfaces reject with the verb named.",
    examples: [{ args: { messageIds: [204, 205] }, result: { archived: 2 } }],
  },
  unarchive: {
    description:
      "Undo `archive`: unarchive human-directed messages in one transaction; returns the rows that were archived. Ids in `restoreUnread` (the ones unread before archiving) become unread again; the rest keep their read state. Agent mail, unarchived and unknown ids are unchanged. Store failures reject so pages can roll back optimistic state. On a human-inbox surface, pass both {org, team} to act in that synced team; omit both to keep the pinned page team. Partial pairs and explicit targets on other surfaces reject with the verb named.",
    examples: [
      { args: { messageIds: [204, 205], restoreUnread: [205] }, result: { unarchived: 2 } },
    ],
  },
  openMessage: {
    description:
      "Open or focus live HTML mail from an agent to the human in this page's team, using the message's team bridge. Rejects markdown/plain, agent-addressed, human-authored and unknown messages. Preserves layout and marks nothing read. On a human-inbox surface, pass both {org, team} to act in that synced team; omit both to keep the pinned page team. Partial pairs and explicit targets on other surfaces reject with the verb named.",
    examples: [{ args: { messageId: 204 }, result: { opened: true, messageId: 204 } }],
  },
  embedMessage: {
    description:
      "Embed human-directed HTML mail inline in a right-pane dashboard. Agent mail returns a host-minted isolated child frame URL, frameId and initial height; human-authored mail returns sanitized HTML. Supply messageId and optionally both {org, team} for a synced team. Cross-team access requires inbox:read:all-teams. Marks nothing read. Release an individual child with {release: true, frameId}; the pane receives no child token.",
    examples: [{ args: { release: true, frameId: "message-frame" }, result: { released: true } }],
  },
  markRead: {
    description:
      "Mark unread human-directed inbox messages read in one transaction; returns the number changed, with duplicate ids counted once. On a human-inbox surface, pass both {org, team} to act in that synced team; omit both to keep the pinned page team. Partial pairs and explicit targets on other surfaces reject with the verb named.",
    examples: [{ args: { messageIds: [204] }, result: { marked: 1 } }],
  },
  markUnread: {
    description:
      "Policy: clear read state on human-directed mail in one scoped batch using inbox:write. Preserve archive and delivery state; no new prompt. Architecture: the inbox holds an explicitly unread conversation until Open, Reply or Jump to latest.",
    examples: [{ args: { messageIds: [204] }, result: { marked: 1 } }],
  },
  setHumanStatus: {
    description: "Set your own presence. `note` is optional.",
    examples: [{ args: { status: "away", note: "lunch" }, result: { ok: true } }],
  },

  // ── SPAWN (write) ───────────────────────────────────────────────────────
  openWorkspace: {
    description:
      "Open or fork a story / epic / branch / dir workspace. Target validation lives in the Rust `OpenWorkspaceTarget` enum. Returns a session handle you can address with `sendToSession`.",
    examples: [
      { args: { storyId: "DEMO-5" }, result: { sessionId: "sess-7b2", handle: "DEMO-5" } },
    ],
  },
  openFile: {
    description:
      "Open a context file in the viewer. Rust resolves the target through the current team's layered path lookup. Pass `newTab: true` for a new centre tab.",
    examples: [
      {
        args: { path: "stories/DEMO-5.md" },
        result: {
          path: "stories/DEMO-5.md",
          org: "codesense",
          team: "default",
        },
      },
    ],
  },
  runPrompt: {
    description:
      "Inject a free-form prompt into an already-live story session. `target` is a story handle (bare `storyId` or `storyId#label`).",
    examples: [{ args: { target: "DEMO-5", prompt: "hey" }, result: { submitted: true } }],
  },
  openTab: {
    description:
      "Spawn an extra tab in an already-open generic (dir) workspace. Only operates on `generic` workspaces.",
    examples: [{ args: { workspaceId: "wt-generic-1" }, result: { sessionId: "sess-tab-1" } }],
  },
  routineRefresh: {
    description:
      "Re-run a routine right now with optional overriding variables. Subscribe via `onRoutineResult` for completion.",
    examples: [
      {
        args: { key: "acme/web/rt-nightly" },
        result: { runId: "run-99", startedAt: "2026-10-05T14:00:00Z" },
      },
    ],
  },
  gotoAgent: {
    description:
      "Show an agent by handle or definition id through the native Show path. Set preserveFocus:true to reveal it without taking keyboard focus from the calling frame.",
    examples: [{ args: { agentId: "amelia" }, result: { located: true } }],
  },
  moveWorkspace: {
    description:
      "Move a workspace card in its team's shared order, as native drag does. Supply exactly one of before or after. Unknown ids and unchanged placements return moved:false. MCP twin: reorder_workspace; move_workspace moves between machines.",
    examples: [
      { args: { teamKey: "acme:core", workspaceId: "b", before: "a" }, result: { moved: true } },
    ],
  },
  openLauncher: {
    description:
      "Open the native launch sheet with a task filled in. Supply org and team for an admitted team; team-only uses the active org. Unknown or unadmitted teams reject. The person confirms in the sheet. Uses sessions:read under the open policy without an additional prompt.",
  },
  openRoutineDraft: {
    description:
      "Open a filled private routine draft under its agent. Without an agent, open Agents to choose one. The person saves from the editor. Uses sessions:read under the open policy without an additional prompt.",
  },
  launchUnder: {
    description:
      "Open the native launch sheet under a running agent, prefilled with its placement team, manager handle and story. Optional task fills the initial task. The person still clicks Launch. Unknown handles reject with unavailable: no such agent.",
    examples: [{ args: { handle: "lead", task: "Review the change" }, result: { opened: true } }],
  },
  openAgentInbox: {
    description:
      "Reveal a running agent and open its native inbox rail. A handle without a running agent session rejects with unavailable: agent is not running.",
    examples: [{ args: { handle: "lead" }, result: { opened: true } }],
  },
  dismiss_agent_mark: {
    description:
      "Dismiss a session's Crashed or Error mark for that one exit, as the terminal's Dismiss does, so every surface stops showing it. crashId is the opaque crashId on the session's projected row (sessions topic); a later abnormal exit gets a new id and shows again, and the dismissal survives restarts. A crashId that no longer matches rejects with conflict: mark changed, unless it was already dismissed, so a retry succeeds; a session outside this frame's team rejects with denied. Available only to a rendered frame.",
    examples: [
      {
        args: { sessionId: "sid-lead", crashId: '["legacy","sid-lead","pty-1",2]' },
        result: {
          sessionId: "sid-lead",
          crashId: '["legacy","sid-lead","pty-1",2]',
          dismissed: true,
          requestId: null,
        },
      },
    ],
  },
  runSkill: {
    description:
      "Run a registered CLI skill with tool use ENABLED + multi-turn. Bounded by a dedicated SpawnCap (`busy` on saturation).",
    examples: [{ args: { skill: "bmad-build" }, result: "…skill output…" }],
  },

  // ── SEND (write) ────────────────────────────────────────────────────────
  sendPrompt: {
    description:
      "Type into any running agent by handle, locally or remotely. Submits by default; submit:false pastes without Enter. Needs no launch controller. An unknown delivery may have arrived: never retry automatically. `handle: \"active\"` with `submit: false` pastes into the person's active terminal (text is written as typed; a newline in it acts as Enter) and returns `status` `typed`, `no-workspace` or `no-terminal`; ask `canPasteToTerminal()` first. Agents use send_to_session; this is the person's typed send from a page.",
    examples: [
      {
        args: { handle: "active", text: "src/main.ts", submit: false },
        result: { status: "typed", submitted: false, machine: null, code: null, message: null },
      },
    ],
  },
  sendToSession: {
    description:
      "Chat into a story's own running agent (rate-guarded). Resolves `{submitted}` once the user turn echoes.",
    examples: [{ args: { target: "DEMO-5", text: "ping" }, result: { submitted: true } }],
  },
  sendToWorkspaceTab: {
    description: "Drive a generic workspace tab — a shell `command` or an agent `prompt`.",
    examples: [
      {
        args: { sessionId: "sess-tab-1", mode: "command", text: "ls" },
        result: { submitted: true },
      },
    ],
  },
  postMessage: {
    description:
      "Send a message to an inbox or an agent role. `onClick` is the per-message action the Inbox UI honors (goto-agent / open-file / show-message).",
    examples: [
      {
        args: { to: "builder", text: "DEMO-5 ready" },
        result: { status: "delivered", messageId: "m-204" },
      },
    ],
  },
  sendAsHuman: {
    description:
      "Send an inbox message as the user (not an agent). `to` is a session handle or an agent role. Optional `inReplyTo` threads under an existing message in this team and sends only to `to`; an unknown reply id resolves status invalid. On a human-inbox surface, pass both {org, team} to act in that synced team; omit both to keep the pinned page team. Partial pairs and explicit targets on other surfaces reject with the verb named.",
    examples: [
      {
        args: { to: "amelia", text: "hey" },
        result: { messageId: 205 },
      },
    ],
  },
  replyAsHuman: {
    description:
      "Reply to an inbox message as the user. On a human-inbox surface, pass both {org, team} to act in that synced team; omit both to keep the pinned page team. Partial pairs and explicit targets on other surfaces reject with the verb named.",
    examples: [
      {
        args: { messageId: 204, text: "merged" },
        result: { messageId: 206 },
      },
    ],
  },

  // ── READ_ACTION (audited reads) ─────────────────────────────────────────
  readWorkspace: {
    description:
      "Read a doc or list a dir (trailing `/`). Confined to the doc's own subtree on a managed surface. Audited (`read:true`); never rate-guarded.",
    examples: [
      {
        args: { path: "stories/DEMO-5.md" },
        result: { content: "…file bytes…" },
      },
    ],
  },
  getWorkspaceStatus: {
    description:
      "Check if a story has a workspace. Lets a board render honest affordances (`exists:false → Launch`, `live:true → running`) WITHOUT having opened the story.",
    examples: [
      {
        args: { storyId: "DEMO-5" },
        result: { exists: true, live: false },
      },
    ],
  },
  routineResult: {
    description:
      "Current result snapshot for one routine (last successful run's output). `key` is the routine's `routines()` row `id`, its full `<org>/<team>/<id>` key. For paginated history use `routineRuns`.",
    examples: [
      {
        args: { key: "acme/web/rt-nightly" },
        result: { ok: true, finishedAt: "2026-10-05T04:00:00Z" },
      },
    ],
  },
  routineStatus: {
    description:
      "Current execution status for one routine. Alias of `routineResult` with status-slanted framing.",
    examples: [{ args: { key: "acme/web/rt-nightly" }, result: { ok: true } }],
  },

  // ── MANAGEMENT (write-ish — kills what you opened) ──────────────────────
  closeWorkspace: {
    description:
      "Kill the handle-bound session (freeing the concurrency cap + writing one Kill audit line) AND evict its tab. Idempotent — an unbound handle is a typed no-op. `removeWorktree: true` escalates to the destructive close — reaps the managed worktree after the session closes; `discardUnsaved: true` forces that reap over uncommitted work (listed in `discardedUnsavedWork`). Remote workspaces close every tab but do not reap the worktree on the remote machine.",
    examples: [{ args: { target: "DEMO-5" }, result: { closed: true } }],
  },
  closeTerminalTab: {
    description:
      "Close ONE terminal tab opened via `openTab`/`openWorkspace` or `open_terminal_tab`/`open_remote_tab`. Idempotent — an already-closed handle is a typed no-op; a session trying to close its OWN tab mid-call is refused with `selfClose`. Does NOT touch the worktree even when closing the last tab (closeWorkspace is the destructive verb — one per concept).",
    examples: [{ args: { handle: "s1" }, result: { closed: true } }],
  },
  stopSession: {
    description:
      "Stop any live session by its roster handle, as the Agents panel's Stop does. Gone handle = no-op (so a doc can stop defensively).",
    examples: [{ args: { handle: "DEMO-5" }, result: { stopped: true } }],
  },
  routineCancel: {
    description: "Cancel a running routine.",
    examples: [{ args: { key: "acme/web/rt-nightly" }, result: { cancelled: true } }],
  },

  // ── Analyze (one-shot CLI) ──────────────────────────────────────────────
  analyze: {
    description:
      "One-shot agent CLI (no tools; `--max-turns 1`) over `{prompt, data}`. Typed rejection prefixes: `busy:`, `timeout:`, `unavailable:`.",
    examples: [
      {
        args: { prompt: "summarize", data: { foo: 1 } },
        result: "…analysis…",
      },
    ], // analyze is in READ_VERBS but is a spawn-ish call; audit-exempt
  },
};

// ---------------------------------------------------------------------------
// Kit SDK — the shortcuts the viewer shim binds onto `window.codesense`.
// Driven by the inventory + the `LINK_INTERCEPTOR_JS` / `DATA_READS_JS` method
// list in `viewer.rs`. The parser below checks that every method named here
// still appears in the shim source (so a removed method surfaces loudly).
// ---------------------------------------------------------------------------
const KIT = [
  {
    name: "codesense.showMenu",
    category: "ui",
    wraps: "invoke('showMenu', args)",
    description:
      "Open a host-rendered menu outside the frame. Built-ins keep host labels, availability and confirmations. Unknown built-in ids reject.",
    example:
      "const result = await codesense.showMenu({anchorRect: button.getBoundingClientRect(), anchorId: button.id, row, items});",
  },
  {
    name: "codesense.showPopover",
    category: "ui",
    wraps: "invoke('showPopover', args)",
    description:
      "Open the native quick-send popup or the agent's inbox chat. Other kinds reject; dismissal returns focus to the opening anchor.",
    example:
      "await codesense.showPopover({kind:'agent-quick-send', sessionId, anchorRect: button.getBoundingClientRect(), anchorId: button.id});",
  },
  {
    name: "codesense.focusOut",
    category: "ui",
    wraps: "invoke('focusOut', args)",
    description:
      "Return focus to the previous host control, or the active terminal if it has gone.",
    example: "await codesense.focusOut({direction:'backward'});",
  },
  {
    name: "codesense.focusTerminal",
    category: "ui",
    wraps: "invoke('focusTerminal', {})",
    description: "Focus the active workspace terminal for the frame team.",
    example: "await codesense.focusTerminal();",
  },
  {
    name: "codesense.textMenuOpen",
    category: "ui",
    wraps: "shim-owned automatic text-menu state",
    description:
      "Architecture: true from handing a text target to the host until its menu settles. Blur handlers defer edits only while this is true; ordinary focus leaving the frame still commits. Lifecycle flush saves pending edits before teardown.",
    example: "input.onblur = () => { if (!codesense.textMenuOpen) saveDraft(); };",
  },
  {
    name: "codesense.onFocusReturn",
    category: "ui",
    wraps: "codesense:focus-return listener",
    description:
      "Replace default anchor focus after host UI closes. With no handler, the shim focuses anchorId or its first focusable control.",
    example: "const off = codesense.onFocusReturn(({anchorId}) => restoreFocus(anchorId));",
  },
  {
    name: "codesense.invoke",
    category: "core",
    wraps: "the postMessage RPC seam itself",
    description:
      "The core bridge RPC. Every allowlist verb is reachable via `codesense.invoke(name, args)`; every other kit method is a thin wrapper over this.",
    example:
      'const result = await codesense.invoke("queryContext", { text: "DEMO" });\nconsole.log(result.rows.length);',
  },
  {
    name: "codesense.act",
    category: "core",
    wraps: "alias for invoke()",
    description:
      "Semantic alias for `invoke(cmd, args)` — spells `codesense.act(...)` out loud when the call is a user-initiated action (write-class verb) rather than a plain read.",
    example: 'await codesense.act("setField", { path: "x.md", field: "status", value: "done" });',
  },
  {
    name: "codesense.subscribeContext",
    category: "core",
    wraps: "invoke('queryContext') + codesense:context-changed listener",
    description:
      "Subscribe to context metadata. Re-runs the query automatically whenever `codesense:context-changed` fires. Returns a function; call it to unsubscribe.",
    example:
      'const stop = codesense.subscribeContext(\n  { text: "DEMO" },\n  (rows, err) => {\n    if (err) return console.error(err);\n    render(rows);\n  }\n);\n// later: stop();',
  },
  {
    name: "codesense.queryContext",
    category: "data-read",
    wraps: "invoke('queryContext', query || {})",
    description:
      "One-shot context metadata query. Thin wrapper over `invoke`; prefer `subscribeContext` if you want push updates.",
    example: 'const { rows } = await codesense.queryContext({ where: { orgs: ["acme"] } });',
  },
  {
    name: "codesense.context",
    category: "data-read",
    wraps: "invoke('context')",
    description: "The dashboard's own location — resolved from the host-supplied doc path.",
    example: "const where = await codesense.context();",
  },
  {
    name: "codesense.openFile",
    category: "nav",
    wraps: "invoke('openFile', args)",
    description:
      "Open a context file in the viewer. Accepts a string path or an options object; pass `newTab: true` for a new centre tab.",
    example:
      'codesense.openFile("stories/DEMO-5.md");\ncodesense.openFile({ path: "architecture.md", newTab: true });',
  },
  {
    name: "codesense.setField",
    category: "nav",
    wraps: "invoke('setField', args)",
    description:
      "Write frontmatter keys atomically. `null` removes a field. Shortcut for the full bridge call.",
    example: 'await codesense.setField({ path: "x.md", field: "status", value: "done" });',
  },
  {
    name: "codesense.setBadge",
    category: "ui",
    wraps: "codesense:badge direct postMessage (no RPC)",
    description:
      "Architecture: publish a badge count; pass `null` to clear. The closed right-pane edge keeps that count. Default paused panes retain their last known count and refresh when shown; resolved badge: live panes can update it while hidden. Policy: the built-in Inbox opts into live badges; other panes default off. One-way message; no promise.",
    example:
      'codesense.setBadge({ count: 3, severity: "warning" });\n// later:\ncodesense.setBadge(null);',
  },
  {
    name: "codesense.snapshot",
    category: "ui",
    wraps: "codesense:snapshot postMessage with ack",
    description:
      "Archive the current DOM as HTML (+ optional sidecar state). The host serializes the frame, strips scripts, and stores it. Resolves when the host acks.",
    example: 'await codesense.snapshot({ selected: "queryContext" });',
  },
  {
    name: "codesense.ready",
    description:
      "Tell the host the page has finished starting. Slot dashboards call ready() or subscribe within 3 seconds; the shim's automatic handshake does not count as page liveness.",
    example: "renderDashboard();\ncodesense.ready();",
  },
  {
    name: "codesense.theme",
    category: "theme",
    wraps: "poll of the last codesense:theme push",
    description:
      "Return the current theme snapshot (one-shot). Prefer `onTheme(cb)` for live updates.",
    example:
      'const theme = await codesense.theme();\nconsole.log(theme?.scheme); // "dark" | "light"',
  },
  {
    name: "codesense.onTheme",
    category: "theme",
    wraps: "codesense:theme postMessage listener",
    description:
      "Listen for theme changes. Called immediately with the current theme once ready, then again whenever the viewer theme switches.",
    example:
      'codesense.onTheme((theme) => {\n  if (!theme) return;\n  document.body.classList.toggle(\n    "dark-mode",\n    theme.scheme === "dark"\n  );\n});',
  },
  {
    name: "codesense.onIntent",
    category: "data-read",
    wraps: "codesense:intent postMessage listener",
    description:
      "Receive an opaque intent object supplied to pane_open (at most 4096 UTF-8 bytes). Returns an unsubscribe. The shim buffers the latest undelivered intent until a subscriber registers, then consumes it once.",
    example:
      "const off = codesense.onIntent((intent) => {\n  if (intent.conversation) openConversation(intent.conversation);\n});",
  },
  {
    name: "codesense.onVisibility",
    category: "theme",
    wraps: "codesense:visibility postMessage listener",
    description: "Called on each visibility change (and immediately with the current state).",
    example: "codesense.onVisibility((visible) => {\n  if (!visible) pauseAnimations();\n});",
  },
  {
    name: "codesense.prefs",
    category: "data-read",
    wraps: "invoke('prefs')",
    description:
      "This dashboard's own settings ({} at first), kept in the user's app state so they survive restarts (a frame's localStorage does not). One entry per dashboard instance: a team copy and a private copy each have their own.",
    example:
      '// Feature-detect: an older CodeSense has no prefs (fall back to localStorage).\nconst { details = "off" } = codesense.prefs ? await codesense.prefs() : {};',
  },
  {
    name: "codesense.setPrefs",
    category: "ui",
    wraps: "invoke('setPrefs', patch)",
    description:
      "Merge keys into this dashboard's own settings (null removes a key); resolves the result. Plain JSON values only; 16 KB per dashboard, 512 KB across all. Its own budget (30 per 10 s, `denied: prefs budget`), not audited.",
    example: 'await codesense.setPrefs?.({ details: "compact" });',
  },
  {
    name: "codesense.getNavSelection",
    category: "nav",
    wraps: "codesense:slot-context selection",
    description:
      "What the left nav has selected in the page's own team: `{org, team, file, drill}`. `file` is the active tab (`path`, `relPath`, `kind`, `id`, `title`, `epic`); `drill` is the folder the nav is drilled into (`path`, `relPath`, `epic`), `null` at the root. A path outside the team's content root reads as `null`. Read-only; to select a file in the nav, call `openFile`.",
    example:
      '// Feature-detect: an older CodeSense has no nav selection.\nconst sel = (await codesense.getNavSelection?.()) ?? null;\nif (sel?.file?.kind === "story") highlight(sel.file.id);',
  },
  {
    name: "codesense.onNavSelectionChange",
    category: "nav",
    wraps: "codesense:slot-context selection listener",
    description:
      "Follow the left nav: called with the current selection once known, then once per settled change (bursts coalesced, repeats dropped). The host sends selection after ready and on settled changes. Returns an unsubscribe.",
    example:
      "// Feature-detect: an older CodeSense has no nav selection.\nconst off = codesense.onNavSelectionChange?.((sel) => {\n  focusStory(sel?.file?.id ?? null);\n  scopeToEpic(sel?.drill?.epic?.id ?? null);\n});",
  },
  {
    name: "codesense.sessions",
    category: "data-read",
    wraps: "invoke('sessions')",
    description: "List live and recent sessions. Shortcut for `invoke('sessions')`.",
    example: "const live = await codesense.sessions();",
  },
  {
    name: "codesense.onSessions",
    category: "data-read",
    wraps: "subscribe('sessions') + codesense:delta",
    description:
      "Receive the current session rows, then apply subscribed deltas by stable id. Hidden frames resync when shown.",
    example: "codesense.onSessions((rows) => render(rows));",
  },
  {
    name: "codesense.agents",
    category: "data-read",
    wraps: "invoke('agents')",
    description: "Every launchable agent visible to this team.",
    example: "const all = await codesense.agents();",
  },
  {
    name: "codesense.agent",
    category: "data-read",
    wraps: "invoke('agent', args)",
    description: "A single agent by key.",
    example: 'const one = await codesense.agent({ key: "amelia" });',
  },
  {
    name: "codesense.inbox",
    category: "data-read",
    wraps: "invoke('inbox', args || {})",
    description: "Team-scoped inbox query. Pairs with `onInboxChanged(cb)` for push.",
    example: 'const builderQueue = await codesense.inbox({ role: "builder" });',
  },
  {
    name: "codesense.roles",
    category: "data-read",
    wraps: "invoke('roles', args || {})",
    description: "Agent role queues.",
    example: "const queues = await codesense.roles();",
  },
  {
    name: "codesense.updates",
    category: "data-read",
    wraps: "invoke('updates', args || {})",
    description: "Agent activity feed.",
    example: "const { updates } = await codesense.updates();",
  },
  {
    name: "codesense.humanStatus",
    category: "data-read",
    wraps: "invoke('humanStatus')",
    description: "The user's own presence.",
    example: "const me = await codesense.humanStatus();",
  },
  {
    name: "codesense.onAgentsChanged",
    category: "data-read",
    wraps: "codesense:agents-changed listener",
    description:
      "Nudge callback — fires with no payload whenever the host pushes `codesense:agents-changed`. Combine with `codesense.agents()` to re-read.",
    example: "codesense.onAgentsChanged(() => codesense.agents().then(render));",
  },
  {
    name: "codesense.onInboxChanged",
    category: "data-read",
    wraps: "codesense:inbox-changed listener",
    description:
      "Nudge callback — fires with no payload whenever the host pushes `codesense:inbox-changed`.",
    example:
      'codesense.onInboxChanged(async () => {\n  const items = await codesense.inbox({ scope: "team" });\n  renderInbox(items);\n});',
  },
  {
    name: "codesense.routines",
    category: "data-read",
    wraps: "invoke('routines')",
    description: "Every routine this user sees.",
    example: "const all = await codesense.routines();",
  },
  {
    name: "codesense.routine",
    category: "data-read",
    wraps: "invoke('routine', args)",
    description: "One routine by id.",
    example: 'const r = await codesense.routine({ key: "acme/web/rt-nightly" });',
  },
  {
    name: "codesense.routineRuns",
    category: "data-read",
    wraps: "invoke('routineRuns', args)",
    description: "Paginated run history for one routine.",
    example: 'const page = await codesense.routineRuns({ key: "acme/web/rt-nightly", limit: 20 });',
  },
  {
    name: "codesense.routineRun",
    category: "data-read",
    wraps: "invoke('routineRun', args)",
    description: "One routine run with its routine.",
    example: 'const run = await codesense.routineRun({ runId: "run-42" });',
  },
  {
    name: "codesense.routineResult",
    category: "data-read",
    wraps: "invoke('routineResult', args)",
    description:
      "Current result snapshot (last successful run). Pairs with `onRoutineResult(cb)` for push.",
    example: 'const latest = await codesense.routineResult({ key: "acme/web/rt-nightly" });',
  },
  {
    name: "codesense.routineRefresh",
    category: "nav",
    wraps: "invoke('routineRefresh', args)",
    description: "Re-run a routine right now with optional overriding variables.",
    example: 'await codesense.routineRefresh({ key: "acme/web/rt-nightly" });',
  },
  {
    name: "codesense.routineStatus",
    category: "data-read",
    wraps: "invoke('routineStatus', args)",
    description: "Current execution status for one routine.",
    example: 'const s = await codesense.routineStatus({ key: "acme/web/rt-nightly" });',
  },
  {
    name: "codesense.routineCancel",
    category: "nav",
    wraps: "invoke('routineCancel', args)",
    description: "Cancel a running routine.",
    example: 'await codesense.routineCancel({ key: "acme/web/rt-nightly" });',
  },
  {
    name: "codesense.onRoutineResult",
    category: "data-read",
    wraps: "codesense:routines-changed listener",
    description: "Nudge callback — fires whenever the host pushes `codesense:routines-changed`.",
    example: "codesense.onRoutineResult(() => codesense.routineResult({ key }).then(render));",
  },
  {
    name: "codesense.onRoutinesChanged",
    category: "data-read",
    wraps: "codesense:routines-changed listener",
    description:
      "Nudge callback — fires whenever the routine LIST changes (added / removed / enabled).",
    example: "codesense.onRoutinesChanged(() => codesense.routines().then(render));",
  },
  {
    name: "codesense.runs",
    category: "data-read",
    wraps: "invoke('runs', args || {})",
    description: "Latest outcome of every agent run.",
    example: "const recent = await codesense.runs({ limit: 10 });",
  },
  {
    name: "codesense.run",
    category: "data-read",
    wraps: "invoke('run', args)",
    description: "One agent run with transcript + artifact (artifact capped at 512 KiB).",
    example: 'const full = await codesense.run({ runId: "run-42" });',
  },
  {
    name: "codesense.runEvidence",
    category: "data-read",
    wraps: "invoke('runEvidence', args)",
    description: "Run evidence + testing data; null if the run didn't produce evidence.",
    example: 'const ev = await codesense.runEvidence({ runId: "run-42" });',
  },
  {
    name: "codesense.runWorkProduct",
    category: "data-read",
    wraps: "invoke('runWorkProduct', args)",
    description: "The branch an agent run produced.",
    example: 'const wp = await codesense.runWorkProduct({ runId: "run-42" });',
  },
  {
    name: "codesense.worktrees",
    category: "data-read",
    wraps: "invoke('worktrees')",
    description: "Managed worktrees on this Mac.",
    example: "const live = await codesense.worktrees();",
  },
  {
    name: "codesense.machines",
    category: "data-read",
    wraps: "invoke('machines')",
    description: "Machines this team can see (local + remote private-account).",
    example: "const net = await codesense.machines();",
  },
  {
    name: "codesense.sessionUsage",
    category: "data-read",
    wraps: "invoke('sessionUsage', args || {})",
    description:
      "Token use and spend per agent session CodeSense launched, on this Mac and the machines it connects to.",
    example: "const { sessions, totals } = await codesense.sessionUsage({ limit: 20 });",
  },
  {
    name: "codesense.modelRates",
    category: "data-read",
    wraps: "invoke('modelRates')",
    description:
      "Every stored rate that prices usage: the seeded list prices and the ones setModelRate set.",
    example: "const rates = await codesense.modelRates();",
  },
  {
    name: "codesense.canPasteToTerminal",
    category: "data-read",
    wraps: "invoke('canPasteToTerminal')",
    description:
      'Whether `sendPrompt({handle: "active", submit: false})` has a terminal to paste into.',
    example: "const canPaste = await codesense.canPasteToTerminal();",
  },
  {
    name: "codesense.onMachinesChanged",
    category: "data-read",
    wraps: "codesense:machines-changed listener",
    description: "Nudge callback for machine list changes.",
    example: "codesense.onMachinesChanged(() => codesense.machines().then(render));",
  },
  {
    name: "codesense.onWorktreesChanged",
    category: "data-read",
    wraps: "codesense:worktrees-changed listener",
    description: "Nudge callback for managed worktree changes.",
    example: "codesense.onWorktreesChanged(() => codesense.worktrees().then(render));",
  },
  {
    name: "codesense.onPasteTargetChanged",
    category: "data-read",
    wraps: "codesense:paste-target-changed listener",
    description: "Nudge callback for when `canPasteToTerminal()` changes.",
    example: "codesense.onPasteTargetChanged(() => codesense.canPasteToTerminal().then(render));",
  },
  {
    name: "codesense.readWorkspace",
    category: "data-read",
    wraps: "invoke('readWorkspace', args)",
    description:
      "Read a doc or list a dir — confined to the doc's own subtree on a managed surface.",
    example: 'const text = await codesense.readWorkspace({ path: "notes.md" });',
  },
  {
    name: "codesense.sprints",
    category: "proxy",
    wraps: "fetch(__codesenseProxy.base + '/api/v1/sprints')",
    description:
      "Authenticated proxy fetch — not a bridge RPC. Rejects with `unavailable` if no read capability has been minted.",
    example: "const s = await codesense.sprints();",
  },
  {
    name: "codesense.taxonomy",
    category: "proxy",
    wraps: "fetch(__codesenseProxy.base + '/api/v1/taxonomy')",
    description: "Authenticated proxy fetch — taxonomy registry snapshot.",
    example: "const t = await codesense.taxonomy();",
  },
];

// ---------------------------------------------------------------------------
// `cs-*` classes — the shipped semantic classes a dashboard can rely on.
// Hand-curated (same pattern as `generate-recipes-manifest`'s META table) with
// a parser check that each class still appears in `viewer-themes.css` (the one
// that will fail loudest on removal) or in the viewer source where injected.
// ---------------------------------------------------------------------------
const CLASSES = [
  {
    name: "cs-page",
    category: "layout",
    description:
      "Applied to `<body>` on every dashboard shell. Carries the base background / foreground / font and anchors every other `cs-*` class.",
    appliedBy: "`<body>` in every dashboard shell",
    tokensUsed: ["--cs-bg", "--cs-fg", "--cs-font-sans"],
    example: '<body class="cs-page" data-page="primitives-explorer">\n  …\n</body>',
    sourceRef: "codesense-docs/_shared/cs-docs.css",
  },
  {
    name: "cs-topnav",
    category: "layout",
    description:
      "The dashboard shell's top navigation bar. Themed border and background follow `--cs-border` + `--cs-surface`.",
    appliedBy: "`<nav>` inside `.cs-page`",
    tokensUsed: ["--cs-surface", "--cs-border"],
    example: '<nav class="cs-topnav">…</nav>',
    sourceRef: "codesense-docs/_shared/cs-docs.css",
  },
  {
    name: "cs-main",
    category: "layout",
    description:
      "Main content region of a dashboard shell. Grid-row sized to fill between `.cs-topnav` and `.cs-footer`.",
    appliedBy: "`<main>` inside `.cs-page`",
    tokensUsed: ["--cs-bg"],
    example: '<main class="cs-main">…</main>',
    sourceRef: "codesense-docs/_shared/cs-docs.css",
  },
  {
    name: "cs-footer",
    category: "layout",
    description: "Keyboard-legend / provenance band at the bottom of a dashboard shell.",
    appliedBy: "`<footer>` inside `.cs-page`",
    tokensUsed: ["--cs-inset", "--cs-border", "--cs-muted-fg"],
    example: '<footer class="cs-footer">…</footer>',
    sourceRef: "codesense-docs/_shared/cs-docs.css",
  },
  {
    name: "cs-sidebar-item",
    category: "sidebar",
    description:
      "A single list item inside a sidebar. Themed from `--cs-sidebar-text` + `--cs-sidebar-border` with a `:hover` surface swap.",
    appliedBy: "list item in any sidebar",
    tokensUsed: ["--cs-sidebar-text", "--cs-sidebar-border", "--cs-sidebar-hover"],
    example: '<li class="cs-sidebar-item">Agents</li>',
    sourceRef: "src/styles/viewer-themes.css",
  },
  {
    name: "cs-sidebar-item-selected",
    category: "sidebar",
    description:
      "Active state for a sidebar item. Separate from `cs-sidebar-item-active` which marks the currently open target.",
    appliedBy: "selected list item in any sidebar",
    tokensUsed: ["--cs-sidebar-selected", "--cs-sidebar-selected-text"],
    example: '<li class="cs-sidebar-item cs-sidebar-item-selected">Agents</li>',
    sourceRef: "src/styles/viewer-themes.css",
  },
  {
    name: "cs-sidebar-item-active",
    category: "sidebar",
    description:
      "Currently active sidebar item — the one the user is looking at. Pairs with `cs-sidebar-item-selected` for the (potentially multi-select) selection state.",
    appliedBy: "the currently active sidebar item",
    tokensUsed: ["--cs-sidebar-selected", "--cs-sidebar-selected-text"],
    example: '<li class="cs-sidebar-item cs-sidebar-item-active">Agents</li>',
    sourceRef: "src/styles/viewer-themes.css",
  },
  {
    name: "cs-sidebar-ready",
    category: "sidebar",
    description:
      "Ready-indicator dot. Renders a left-edge accent pulse via `::before` using `--cs-sidebar-ready-accent`.",
    appliedBy: "sidebar item whose target is live",
    tokensUsed: ["--cs-sidebar-ready-accent"],
    example: '<li class="cs-sidebar-item cs-sidebar-ready">Builder</li>',
    sourceRef: "src/styles/viewer-themes.css",
  },
  {
    name: "cs-workspace-card",
    category: "cards",
    description:
      "Card surface for workspace tiles, routine tiles, dashboard tiles. Raised one step from the panel background.",
    appliedBy: "a `<div>` representing one workspace / routine / tile",
    tokensUsed: ["--cs-workspace-card-bg", "--cs-border"],
    example: '<div class="cs-workspace-card">…</div>',
    sourceRef: "src/styles/viewer-themes.css",
  },
  {
    name: "cs-workspace-card-selected",
    category: "cards",
    description: "Selected state for a workspace tile. Pair with `cs-workspace-card`.",
    appliedBy: "selected workspace tile",
    tokensUsed: ["--cs-workspace-card-bg", "--cs-accent"],
    example: '<div class="cs-workspace-card cs-workspace-card-selected">…</div>',
    sourceRef: "src/styles/viewer-themes.css",
  },
  {
    name: "cs-tab-item",
    category: "tabs",
    description:
      "A single tab inside a tab strip. Themed inactive state via `--cs-tab-text` + `--cs-tab-bg`.",
    appliedBy: "tab button in any tab strip",
    tokensUsed: ["--cs-tab-bg", "--cs-tab-text", "--cs-tab-border"],
    example: '<button class="cs-tab-item">Agents</button>',
    sourceRef: "src/styles/viewer-themes.css",
  },
  {
    name: "cs-tab-item-active",
    category: "tabs",
    description: "Active state for a tab item. Pair with `cs-tab-item`.",
    appliedBy: "the currently open tab",
    tokensUsed: ["--cs-tab-active-bg", "--cs-tab-active-text"],
    example: '<button class="cs-tab-item cs-tab-item-active">Agents</button>',
    sourceRef: "src/styles/viewer-themes.css",
  },
  {
    name: "cs-focus-pane",
    category: "focus",
    description:
      'Focus indicator applied to the currently focused pane. Honors `data-focus-indicator="on"|"dim"`.',
    appliedBy: "pane root element",
    tokensUsed: ["--cs-accent"],
    example: '<section class="cs-focus-pane" data-focus-indicator="on">…</section>',
    sourceRef: "src/features/viewer",
  },
  {
    name: "cs-focus-line",
    category: "focus",
    description: "Focus indicator rendered as an inline line under the focused pane's border.",
    appliedBy: "pane element with `data-focus-indicator`",
    tokensUsed: ["--cs-accent"],
    example: '<div class="cs-focus-line" data-focus-indicator="dim">…</div>',
    sourceRef: "src/features/viewer",
  },
  {
    name: "cs-note-link",
    category: "prose",
    description:
      "Hyperlink inside scratch-note surfaces. Prose link color with a `:focus-visible` outline.",
    appliedBy: "`<a>` inside `.cs-redline-prose` or scratch notes",
    tokensUsed: ["--cs-link", "--cs-accent"],
    example: '<a class="cs-note-link" href="…">story</a>',
    sourceRef: "src/features/viewer",
  },
  {
    name: "cs-terminal-display",
    category: "terminal",
    description:
      "Container wrapping the xterm.js display. Carries terminal foreground / background that match the viewer theme.",
    appliedBy: "the xterm wrapper `<div>`",
    tokensUsed: ["--cs-bg", "--cs-fg"],
    example: '<div class="cs-terminal-display">…</div>',
    sourceRef: "src/features/viewer",
  },
  {
    name: "cs-redline-add",
    category: "prose",
    description: "Added-span inside a redline diff — green on the current theme.",
    appliedBy: "`<ins>` inside `.cs-redline-prose`",
    tokensUsed: ["--cs-success"],
    example: '<ins class="cs-redline-add">new line</ins>',
    sourceRef: "src/features/viewer",
  },
  {
    name: "cs-redline-del",
    category: "prose",
    description: "Removed-span inside a redline diff — red on the current theme.",
    appliedBy: "`<del>` inside `.cs-redline-prose`",
    tokensUsed: ["--cs-danger"],
    example: '<del class="cs-redline-del">old line</del>',
    sourceRef: "src/features/viewer",
  },
];

// ---------------------------------------------------------------------------
// Pane contexts — three live + one baseline. Hand-curated (shape is stable
// per inventory §6); the parser checks the four sandbox strings still appear
// in html-viewer.tsx so a change there surfaces loudly.
// ---------------------------------------------------------------------------
const CONTEXTS = [
  {
    name: "centre-tab",
    category: "centre",
    route: "/ + base64(teamContentPath) + file path",
    sandbox:
      "allow-scripts allow-same-origin allow-forms allow-popups allow-modals allow-downloads",
    bridgeFull: true,
    snapshotAvailable: true,
    detection: "props.message === null && props.pane === null",
    description:
      "Standard centre-tab dashboard. Full bridge, Snapshot button band, Cmd+W closes the tab. Theme applied only if the doc opts in with `window.__codesenseThemeAware`.",
    features: {
      Bridge: "full",
      "Snapshot button": true,
      "Cmd+W closes tab": true,
      "color-scheme injection": "only if `__codesenseThemeAware`",
      "Context anchor": "file's own path",
    },
    sourceRef: "src/features/dashboard/components/html-viewer.tsx",
  },
  {
    name: "right-pane",
    category: "pane",
    route: "/pane + port + contentRoot + panesDir + fileName",
    sandbox:
      "allow-scripts allow-same-origin allow-forms allow-popups allow-modals allow-downloads",
    bridgeFull: true,
    snapshotAvailable: false,
    detection: "props.pane !== null",
    description:
      "Docked right-pane dashboard. Same full bridge as a centre tab; no Snapshot band; Cmd+W is ignored; `color-scheme` always injected because the pane sits on the right surface.",
    features: {
      Bridge: "full",
      "Snapshot button": false,
      "Cmd+W closes tab": false,
      "color-scheme injection": "always",
      "Context anchor": "pane's panesDir + fileName",
    },
    sourceRef: "src/features/dashboard/components/html-viewer.tsx",
  },
  {
    name: "inbox-message",
    category: "message",
    route: "/message + port + org + team + messageId",
    sandbox:
      "allow-scripts allow-same-origin allow-forms allow-popups allow-modals allow-downloads",
    bridgeFull: true,
    snapshotAvailable: false,
    detection: "props.message !== null",
    description:
      "Rich inline message inside the inbox. Same full bridge; height auto-sizes via `codesense:content-size`; `color-scheme` always injected.",
    features: {
      Bridge: "full",
      "Snapshot button": false,
      "Cmd+W closes tab": false,
      "color-scheme injection": "always",
      "Resizable frame": "sized by content via ResizeObserver",
      "Context anchor": "message's team contentRoot",
    },
    sourceRef: "src/features/dashboard/components/html-viewer.tsx",
  },
  {
    name: "baseline",
    category: "baseline",
    route: "/baseline + port + orgRoot + relPath",
    sandbox: "allow-scripts (opaque origin, read-only)",
    bridgeFull: false,
    snapshotAvailable: false,
    detection: "props.baselineSource !== null",
    description:
      "Historical / diff view in an opaque-origin iframe. Bridge is disabled — all `codesense.invoke` calls reject with a structured error. Theme is not applied; browser default chrome.",
    features: {
      Bridge: "disabled (rejects all)",
      "Snapshot button": false,
      "Cmd+W closes tab": "N/A",
      "color-scheme injection": "never",
      "Context anchor": "none — read-only blob",
    },
    sourceRef: "src/features/dashboard/components/html-viewer.tsx",
  },
];

// ---------------------------------------------------------------------------
// Hand-curated event meta — the parser below enumerates every `codesense:xxx`
// string referenced in viewer.rs + html-viewer.tsx + html-bridge.ts. Every
// event that lands with no metadata entry still ships (with safe defaults +
// a warning) so the CI gate catches drift and a developer fills in the copy.
// ---------------------------------------------------------------------------
const EVENT_META = {
  "codesense:before-reload": {
    direction: "parent-to-doc",
    payload: "{id}",
    timing: "When a shown pane freezes for a source save or Reload",
    description:
      "Ask the registered onBeforeReload hook for JSON, bounded to 64 KB and 100 ms. Internal transport; snapshot capture never gates the swap.",
    example: "codesense.onBeforeReload(() => ({scroll: document.scrollingElement.scrollTop}));",
  },
  "codesense:before-reload-result": {
    direction: "doc-to-parent",
    payload: "{id, state, warning?}",
    timing: "After optional capture, or immediately when no hook is registered",
    description:
      "Return JSON or null for this frame instance and request. Oversize, late and invalid results are dropped and logged in the frame and host.",
    example: "// Answered automatically by the shim.",
  },
  "codesense:focus-suppressed": {
    direction: "doc-to-parent",
    payload: "{}",
    timing: "When an off-screen candidate attempts to focus a control",
    description:
      "Remember and blur candidate autofocus, then ask the host to restore the previous focus owner. The shim replays it only when presented with permission.",
    example: "// Internal presentation transport; no page handler is required.",
  },
  "codesense:freeze": {
    direction: "parent-to-doc",
    payload: "{frozen: boolean}",
    timing: "At pane-save freeze and on recovery from a failed attempt",
    description:
      "Block pointer and keyboard input through the shim while retaining the active control. The working frame stays shown; a failed replacement unfreezes it.",
    example: "// Managed by the host and shim.",
  },
  "codesense:presentation": {
    direction: "parent-to-doc",
    payload: "{shown: boolean, focusAllowed: boolean}",
    timing: "At the frame handshake and the atomic replacement swap",
    description:
      "Gate candidate focus until shown. Replay pending autofocus only while the slot owns focus; never steal it from another app control.",
    example: "// Internal presentation transport.",
  },
  "codesense:reload-state": {
    direction: "parent-to-doc",
    payload: "{state: JSON | null}",
    timing: "At the handshake, once optional capture settles",
    description:
      "Resolve reloadState exactly once to the previous document snapshot or null. Missing state resolves promptly without waiting for the capture deadline.",
    example: "const saved = await codesense.reloadState();",
  },
  "codesense:reload-warning": {
    direction: "parent-to-doc",
    payload: "{id, warning}",
    timing: "When an optional beforeReload snapshot is dropped",
    description:
      "Write one console.warn for the capture request. The host also logs the document identity and reason; the replacement continues.",
    example: "// Use the warning to fix an oversized or slow onBeforeReload hook.",
  },
  "codesense:restore-focus": {
    direction: "parent-to-doc",
    payload: "{}",
    timing: "When candidate focus is suppressed or a failed reload returns to the working pane",
    description:
      "Restore the control remembered by the old shim at freeze, with preventScroll. The shown embedding stays focusable while its input remains blocked.",
    example: "// Internal presentation transport; no page handler is required.",
  },

  "codesense:child-rect": {
    direction: "parent-to-doc",
    payload: "{id, origin}",
    timing: "Before a hosted child's menu opens",
    description:
      "Architecture: ask the parent dashboard to measure the matching child iframe across the origin boundary.",
    example: "// Internal host presentation transport.",
  },
  "codesense:child-rect-result": {
    direction: "doc-to-parent",
    payload: "{id, rect?}",
    timing: "In response to child-rect",
    description:
      "Architecture: the parent's child iframe rectangle in parent-frame coordinates; absent when the child is gone.",
    example: "// Answered by the parent shim.",
  },
  "codesense:child-focus": {
    direction: "parent-to-doc",
    payload: "{origin}",
    timing: "After a hosted child's menu closes",
    description:
      "Architecture: focus the nested iframe in its parent before returning focus to the child's anchor.",
    example: "// Internal host presentation transport.",
  },
  "codesense:text-menu-closed": {
    direction: "parent-to-doc",
    payload: "{textId}",
    timing: "After the automatic text menu settles, including dismissal or failure",
    description:
      "Architecture: clear the matching shim-owned textMenuOpen state. A superseded menu cannot clear a newer text menu's state.",
    example: "// Internal host presentation transport.",
  },
  "codesense:text-edit": {
    direction: "parent-to-doc",
    payload: "{textId, action, text?}",
    timing: "After the person activates a text-menu item",
    description:
      "Architecture: restore the exact clicked field and selection, then apply cut, copy, paste or selectAll through its input path. Only the host's Paste activation reads and sends clipboard text.",
    example: "// Applied by the shim; pages do not send paste requests.",
  },
  "codesense:menu-action-ack": {
    direction: "doc-to-parent",
    payload: "{id}",
    timing: "After synchronous page menu callbacks finish",
    description:
      "Architecture: confirm that Mark as unread established its page hold before the host persists the unread state.",
    example: "// Applied by the shim, not page code.",
  },
  "codesense:menu-actions": {
    direction: "doc-to-parent",
    payload: "{enabled}",
    timing: "When onMenuAction listeners register or unsubscribe",
    description:
      "Architecture: advertise page action availability so host menus omit Open, Reply and Open queue when no page handler exists.",
    example: "codesense.onMenuAction(({itemId, row}) => { /* navigate in the page */ });",
  },
  "codesense:menu-action": {
    direction: "parent-to-doc",
    payload: "{itemId, row}",
    timing: "After a host menu activation (before the persisted write for mark-unread)",
    description:
      "Architecture: deliver page-local menu actions to onMenuAction. Hosted message actions reach the inbox parent, preserving its navigation and unread hold.",
    example:
      "codesense.onMenuAction(({itemId, row}) => { if (itemId === 'reply') replyTo(row); });",
  },
  "codesense:contextmenu": {
    direction: "doc-to-parent",
    payload: "{target: row|text|frame, row?, text?, textId, anchorRect, anchorId?}",
    timing: "After a context-click and its one-tick page-owned menu window",
    description:
      "Architecture: fields and selected text get the shared text menu; story, epic, file, agent, workspace, group, conversation, message, attachment and role-queue tags use their native builders. Unknown rows and empty space get the dashboard menu. A page may handle the click or call showMenu within one tick. The browser menu is always suppressed.",
    example: "// Add data-codesense-row to an agent row; no event listener is needed.",
  },
  "codesense:flush": {
    direction: "parent-to-doc",
    payload: "{id}",
    timing: "Before replacement reads or retirement (300 ms), or quit (one shared 500 ms deadline)",
    description:
      "Run every onFlush callback and wait for its promise before acknowledging. A pre-reload flush keeps the shown document alive. Retirement tears it down at the deadline even when a callback never settles.",
    example: "codesense.onFlush(() => savePendingChanges());",
  },
  "codesense:flush-ack": {
    direction: "doc-to-parent",
    payload: "{id}",
    timing: "When all registered flush callbacks settle",
    description:
      "The shim acknowledges the matching host flush automatically, including when a callback rejects or none is registered.",
    example: "// Answered automatically by the shim.",
  },
  "codesense:rebind-pending": {
    direction: "parent-to-doc",
    payload: "{}",
    timing: "Before a dock frame moves to a new team",
    description:
      "The shim holds new SDK calls until the matching codesense:rebind and acknowledges with codesense:rebind-ack, so calls sent before it reach the host first.",
    example: "// Handled by the shim; calls made meanwhile are queued and replayed.",
  },
  "codesense:rebind-ack": {
    direction: "doc-to-parent",
    payload: "{}",
    timing: "On codesense:rebind-pending",
    description: "Every call sent before the pause has been posted; the host may now re-mint.",
    example: "// Answered automatically by the shim.",
  },
  "codesense:rebind": {
    direction: "parent-to-doc",
    payload: "{token}",
    timing: "After a team switch re-mints the dock frame's grant",
    description:
      "The shim adopts the new token, replays the held calls with it, and the host then sends the new team in slot context and resyncs every subscribed topic. The old token is already refused.",
    example: "codesense.onSlotContext((ctx) => render(ctx.team));",
  },
  "codesense:focus-in": {
    direction: "parent-to-doc",
    payload: "{}",
    timing: "When focus enters a frame",
    description: "Focus the first available control unless an onFocusIn handler owns focus.",
    example: 'codesense.onFocusIn(() => document.getElementById("search").focus());',
  },
  "codesense:focus-return": {
    direction: "parent-to-doc",
    payload: "{anchorId?}",
    timing: "After a host menu, popover or confirmation closes",
    description:
      "Focus the opening anchor, or the first available control if it has gone. An onFocusReturn handler receives {anchorId} and replaces default focus.",
    example: "codesense.onFocusReturn(({anchorId}) => restoreFocus(anchorId));",
  },
  "codesense:command": {
    direction: "parent-to-doc",
    payload: "{ id, args?, dispatchId }",
    timing:
      "After the page's ready(), once per dispatchId, when a chord or the palette runs one of its commands",
    description:
      "Deliver a declared command to a slot dashboard. The host reveals the slot first and queues the command until the page is ready; one still queued when the pane fails to load is dropped with a toast.",
    example: "codesense.onCommand(({ id }) => { if (id === 'archive') archiveSelected(); });",
  },
  "codesense:frame-focus": {
    direction: "doc-to-parent",
    payload: "{}",
    timing: "When the child window gains focus",
    description:
      "Internal focus notification: the host records frame ownership and delivers focus-in for ordinary entry. A return from host UI preserves the focus-return anchor.",
    example: "// Posted automatically by the shim.",
  },
  "codesense:key-escape": {
    direction: "doc-to-parent",
    payload: "{}",
    timing: "After an unhandled, unmodified Esc outside IME composition",
    description:
      "Run a command bound to Esc, else exit an overlay, or focus the terminal from a docked right-pane, dock or file-nav slot. Centre-frame Esc has no other host action.",
    example: "// Call preventDefault() to keep Esc in the page.",
  },
  "codesense:hello": {
    direction: "parent-to-doc",
    payload: "{}",
    timing: "When a tab is shown before the page's ready was received",
    description:
      "Asks a page that has not yet been answered to post codesense:ready again. A page the host has already answered ignores it, and the host ignores a repeated ready for a handshake already done.",
    example: "// Answered automatically by the shim.",
  },
  "codesense:ping": {
    direction: "parent-to-doc",
    payload: "{}",
    timing: "Every 5 seconds while visible",
    description:
      "The shim answers automatically. Three consecutive missed answers use the existing crashed fallback strip.",
    example: "// Answered automatically by the shim.",
  },
  "codesense:pong": {
    direction: "doc-to-parent",
    payload: "{}",
    timing: "Immediately after a ping",
    description:
      "The heartbeat answer resets the host missed-pong count. Hidden frames are not pinged.",
    example: "// Posted automatically by the shim.",
  },
  "codesense:slot-context": {
    direction: "parent-to-doc",
    payload:
      "{context: {slotId, presentation, width, narrow, team, focusedSession?, currentFile?, selection?, theme}}",
    timing: "Before theme on ready, and whenever slot context changes",
    description:
      "The host context includes nav selection; onNavSelectionChange is an alias over this selection. Narrow is true below 300 pixels.",
    example: "codesense.onSlotContext(context => render(context));",
  },
  "codesense:teardown": {
    direction: "parent-to-doc",
    payload: "{}",
    timing: "After flush acknowledgment or deadline",
    description:
      "The host retires the frame. onTeardown callbacks release page-owned resources; saving belongs in onFlush.",
    example: "codesense.onTeardown(() => unsubscribe());",
  },
  "codesense:delta": {
    direction: "parent-to-doc",
    payload: "{ topic, viewKey, rev, upserts: rows[], removes: ids[], requestId? }",
    timing: "After a subscribed view changes, at most once per 50 ms",
    description:
      "Apply stable-id row changes to the subscribed view. A revision gap resyncs the snapshot.",
    example: "codesense.subscribe('sessions', {onChange: view => render(view.snapshot)});",
  },
  "codesense:stale": {
    direction: "parent-to-doc",
    payload: "{ topic?, viewKey? }",
    timing: "Once when a hidden frame falls behind",
    description: "Mark the hidden view stale without fetching; becoming visible resyncs it.",
    example: "// Handled automatically by subscribe().",
  },
  "codesense:resync": {
    direction: "parent-to-doc",
    payload: "{ topic, viewKey }",
    timing: "When a frame exceeds its pending delta budget",
    description: "Discard pending changes and subscribe again from the last revision.",
    example: "// Handled automatically by subscribe().",
  },
  "codesense:live": {
    direction: "doc-to-parent",
    payload: "{type: 'codesense:live', token}",
    timing: "When the page calls codesense.ready()",
    description:
      "The page has finished starting. A slot uses this explicit signal, or its first admitted subscribe, to stop its fallback deadline. The automatic codesense:ready event is only a transport handshake.",
    example: "codesense.ready();",
    sourceRef: "viewer.rs",
  },
  "codesense:ready": {
    direction: "doc-to-parent",
    payload: "N/A",
    timing: "After shim IIFE runs",
    description:
      "Announce doc loaded; the host answers with slot-context, theme, visibility and pending intent, in that order.",
    example:
      '// Posted automatically by the shim.\nwindow.addEventListener("message", (e) => {\n  if (e.data?.type === "codesense:ready") init();\n});',
  },
  "codesense:theme": {
    direction: "parent-to-doc",
    payload: "{ theme: { scheme, tokens }, applyScheme? }",
    timing: "After doc ready, on theme change",
    description:
      "Apply the token palette and optionally the CSS `color-scheme`. Fires on initial render and whenever the viewer theme switches.",
    example:
      'codesense.onTheme((t) => {\n  if (!t) return;\n  console.log(t.scheme, t.tokens["--cs-bg"]);\n});',
  },
  "codesense:theme-applied": {
    direction: "doc-to-parent",
    payload: "N/A",
    timing: "One animation frame after the shim applies a codesense:theme",
    description:
      "Posted automatically. The host keeps the frame transparent until it arrives, so the page never shows unthemed; a frame that never acks is revealed after a short fallback.",
    example:
      "// Posted automatically by the shim.\n// A page need not handle it; use codesense.onTheme to react to the palette.",
  },
  "codesense:intent": {
    direction: "parent-to-doc",
    payload: "{ intent: Record<string, JSON> } (at most 4096 UTF-8 bytes)",
    timing: "Immediately when ready, or after codesense:ready for a new or reloaded document",
    description:
      "Deliver the latest pending pane_open intent. One pending intent per panel; newer replaces older. The host clears delivered intents so later reloads do not replay them. The shim buffers the last undelivered intent for a late onIntent subscriber and consumes it once. The page interprets the object.",
    example:
      "codesense.onIntent((intent) => {\n  if (intent.messageId) openMessage(intent.messageId);\n});",
  },
  "codesense:visibility": {
    direction: "parent-to-doc",
    payload: "{ visible: boolean }",
    timing: "After doc ready, on visibility change",
    description:
      "Tell the doc whether it's currently on-screen (useful for render hints or pausing animations).",
    example: "codesense.onVisibility((visible) => {\n  if (!visible) pauseAnimations();\n});",
  },
  "codesense:restore-scroll": {
    direction: "parent-to-doc",
    payload: "{ y: number }",
    timing: "After iframe load",
    description: "Restore the saved scroll offset from the last visit.",
    example:
      "// Handled automatically by the shim.\n// Pair with codesense:scroll (doc → parent) to persist offsets.",
  },
  "codesense:context-changed": {
    direction: "parent-to-doc",
    payload: "{ orgs: string[], allOrgs, truncated }",
    timing: "On context index change",
    description:
      "Nudge for `subscribeContext` listeners — the context metadata index changed; re-query.",
    example:
      'codesense.subscribeContext({ text: "DEMO" }, (rows) => {\n  render(rows); // called on initial + on each nudge\n});',
  },
  "codesense:routines-changed": {
    direction: "parent-to-doc",
    payload: "N/A",
    timing: "On routine list change",
    description:
      "Nudge for routine-aware dashboards — call `codesense.routines()` or `codesense.routineResult(args)` to re-read.",
    example: "codesense.onRoutineResult(() => codesense.routineResult({ key }).then(render));",
  },
  "codesense:agents-changed": {
    direction: "parent-to-doc",
    payload: "N/A",
    timing: "On agent state / roster change",
    description: "Nudge for `onAgentsChanged` / `onSessions` listeners.",
    example: "codesense.onAgentsChanged(() => codesense.agents().then(render));",
  },
  "codesense:inbox-changed": {
    direction: "parent-to-doc",
    payload: "N/A",
    timing: "On inbox update",
    description: "Nudge for `watchInbox` / `onInboxChanged` listeners.",
    example: "codesense.onInboxChanged(() => codesense.inbox({}).then(render));",
  },
  "codesense:agent-activity-changed": {
    direction: "parent-to-doc",
    payload: "N/A",
    timing: "On MCP agent message / progress",
    description: "Nudge for `updates()` re-read — the activity feed changed.",
    example:
      'window.addEventListener("message", (e) => {\n  if (e.data?.type === "codesense:agent-activity-changed")\n    codesense.updates().then(render);\n});',
  },
  "codesense:machines-changed": {
    direction: "parent-to-doc",
    payload: "N/A",
    timing: "On machine list change",
    description: "Nudge for `machines()` listeners.",
    example: "codesense.onMachinesChanged(() => codesense.machines().then(render));",
  },
  "codesense:worktrees-changed": {
    direction: "parent-to-doc",
    payload: "N/A",
    timing: "On managed worktree change",
    description: "Nudge for `worktrees()` listeners.",
    example: "codesense.onWorktreesChanged(() => codesense.worktrees().then(render));",
  },
  "codesense:paste-target-changed": {
    direction: "parent-to-doc",
    payload: "N/A",
    timing: "When the person's active terminal appears, goes or the workspace changes",
    description: "Nudge for `canPasteToTerminal()` listeners.",
    example: "codesense.onPasteTargetChanged(() => codesense.canPasteToTerminal().then(render));",
  },
  "codesense:snapshot-request": {
    direction: "parent-to-doc",
    payload: "{ id, format?, expectedUrl }",
    timing: "On snapshot button / doc capture",
    description:
      "Host asks the doc to serialize its DOM (or innerText). The shim replies with `codesense:snapshot-result`.",
    example:
      '// Handled by the shim; opt in to pre-serialize work:\nwindow.addEventListener("message", (e) => {\n  if (e.data?.type !== "codesense:snapshot-request") return;\n  // prepare the DOM before serialization\n});',
  },
  "codesense:invoke": {
    direction: "doc-to-parent",
    payload: "{ id, command, args }",
    timing: "On codesense.invoke() call",
    description:
      "The bridge RPC request. You never emit this directly — call `codesense.invoke(cmd, args)` and the shim posts it for you.",
    example:
      '// Equivalent — but DO NOT emit directly:\nwindow.parent.postMessage(\n  { type: "codesense:invoke", id, command: "queryContext", args: {} },\n  "*",\n);\n// Just:\ncodesense.invoke("queryContext", {});',
  },
  "codesense:navigate": {
    direction: "doc-to-parent",
    payload: "{ href, metaKey?, ctrlKey? }",
    timing: "On click of `.md` / `.html` / `.htm` link",
    description:
      "Intercepted link click — the host routes to the file (same tab, new tab with Meta).",
    example: "// Emitted automatically by the LINK_INTERCEPTOR_JS.\n// No doc-side action needed.",
  },
  "codesense:external-link": {
    direction: "doc-to-parent",
    payload: "{ href }",
    timing: "On click of http(s) / mailto link",
    description: "Open the URL in the OS handler (never inside the iframe).",
    example: "// Emitted automatically by the shim.",
  },
  "codesense:scroll": {
    direction: "doc-to-parent",
    payload: "{ y: number }",
    timing: "On scroll (throttled rAF)",
    description:
      "Save the current scroll position so the parent can restore it across an iframe reload.",
    example: "// Emitted automatically by the shim.",
  },
  "codesense:close-tab": {
    direction: "doc-to-parent",
    payload: "N/A",
    timing: "On Cmd+W (⌘W, Ctrl+W)",
    description: "Forward the close-tab chord — the parent closes this centre tab.",
    example: "// Emitted automatically by the shim.",
  },
  "codesense:shortcut": {
    direction: "doc-to-parent",
    payload: "{ key, code, metaKey, ctrlKey, shiftKey, altKey, editorTarget, textFieldTarget }",
    timing:
      "On an unhandled Command, Control or Option chord, or a function key (excluding composition)",
    description:
      "Forward a key description for the host shortcut registry (built-ins and slot dashboards' commands) and user bindings; editorTarget and textFieldTarget preserve editor-owned navigation and split keys; generic chords retain browser defaults.",
    example: "// Emitted automatically by the shim.",
  },
  "codesense:snapshot": {
    direction: "doc-to-parent",
    payload: "{ id, html }",
    timing: "On `codesense.snapshot(state)` call",
    description: "Archive HTML + optional state sidecar.",
    example: 'await codesense.snapshot({ selected: "queryContext" });',
  },
  "codesense:snapshot-result": {
    direction: "reply",
    payload: "{ id, ok, html?, content?, capturedAt?, truncated?, error? }",
    timing: "Response to `codesense:snapshot-request` / `codesense:snapshot`",
    description: "Correlated reply carrying the frozen DOM or an error.",
    example: "// Correlated reply — handled inside the shim.",
  },
  "codesense:invoke-result": {
    direction: "reply",
    payload: "{ id, ok, data?, error? }",
    timing: "Response to `codesense:invoke`",
    description: "Correlated reply carrying the invoke result or a structured error.",
    example: "// Correlated reply — resolves the shim's invoke() Promise.",
  },
  "codesense:badge": {
    direction: "doc-to-parent",
    payload: "{ badge: { count, severity? } | null }",
    timing: "On `codesense.setBadge()` call",
    description: "Update the chrome badge for this tab.",
    example: 'codesense.setBadge({ count: 3, severity: "warning" });',
  },
  "codesense:content-size": {
    direction: "doc-to-parent",
    payload: "{ height: number }",
    timing: "On DOM resize (ResizeObserver)",
    description: "Inline message height — the parent sizes the frame to this.",
    example: "// Emitted automatically for inbox messages.",
  },
  "codesense:doc-error": {
    direction: "doc-to-parent",
    payload: '{ kind: "error" | "rejection", message, source?, line? }',
    timing: "On `window.onerror` / `unhandledrejection`",
    description: "Report a script failure — a pane host shows it in place.",
    example: "// Emitted automatically by the shim.",
  },
};

// ---------------------------------------------------------------------------
// Source parsers — lean. Each one extracts the authoritative NAME SET from
// the file; the META tables fill in the human-readable fields. A verb /
// method / event / class / token the META table doesn't know about still
// surfaces (with safe defaults + a warning), so a developer notices and
// backfills the copy.
// ---------------------------------------------------------------------------

/** Pull every `'codesense:xxx'` string referenced in viewer.rs. */
function parseEventNames(src) {
  const names = new Set();
  const re = /'codesense:([a-z][a-z0-9-]*)'/g;
  let m;
  // Shim code compiled only into the e2e-webdriver build is a test hook, never a published event.
  src = src.replace(
    /#\[cfg\(feature = "e2e-webdriver"\)\]\s*const \w+: &str = r#"[\s\S]*?"#;/g,
    "",
  );
  while ((m = re.exec(src)) !== null) names.add(`codesense:${m[1]}`);
  return names;
}

// ---------------------------------------------------------------------------
// Classify + describe the collected tokens / verbs / events.
// ---------------------------------------------------------------------------

function tokenRole(name) {
  switch (name) {
    case "--cs-bg":
      return { role: "Document background", swatchType: "fill" };
    case "--cs-fg":
      return { role: "Document foreground text", swatchType: "fill" };
    case "--cs-muted-fg":
      return { role: "Muted / secondary text", swatchType: "fill" };
    case "--cs-border":
      return { role: "Border colour", swatchType: "border" };
    case "--cs-surface":
      return { role: "Card / raised surface background", swatchType: "fill" };
    case "--cs-accent":
      return { role: "Accent / primary action", swatchType: "fill" };
    case "--cs-accent-fg":
      return { role: "Foreground text on accent", swatchType: "fill" };
    case "--cs-link":
      return { role: "Hyperlink colour", swatchType: "text" };
    case "--cs-danger":
      return { role: "Error / destructive colour", swatchType: "fill" };
    case "--cs-success":
      return { role: "Success / added colour", swatchType: "fill" };
    case "--cs-warning":
      return { role: "Warning / modified colour", swatchType: "fill" };
    case "--cs-font-sans":
      return { role: "Sans-serif font family", swatchType: "font" };
    case "--cs-font-mono":
      return { role: "Monospace font family", swatchType: "font" };
    case "--cs-color-scheme":
      return {
        role: '"dark" or "light" — derived from --cs-bg luminance',
        swatchType: "text",
      };
    default:
      return { role: `${name} token`, swatchType: "fill" };
  }
}

function verbCategory(verb, lanes) {
  if (lanes.READ_VERBS.includes(verb)) return "READ";
  if (lanes.METADATA_ACTION_VERBS.includes(verb)) return "METADATA";
  if (lanes.SPAWN_ACTION_VERBS.includes(verb)) return "SPAWN";
  if (lanes.SEND_ACTION_VERBS.includes(verb)) return "SEND";
  if (lanes.READ_ACTION_VERBS.includes(verb)) return "READ_ACTION";
  if (lanes.MANAGEMENT_ACTION_VERBS.includes(verb)) return "MANAGEMENT";
  return "OTHER";
}

// ---------------------------------------------------------------------------
// Build the manifest.
// ---------------------------------------------------------------------------

/**
 * Pure build — reads the source models, returns `{ doc, warnings }`. Export it
 * for the test harness; `main()` writes / compares the committed file.
 */
export function buildManifest(root = ROOT) {
  const sdk = buildPageSdk(sourceTree(root), ts);
  const path = (file) => join(root, file);
  const viewerSrc =
    readFileSync(path("src-tauri/src/http/routes/viewer.rs"), "utf8") +
    readFileSync(path("src-tauri/src/http/routes/live_hub.js"), "utf8");

  const warnings = [];

  // ---- INVOKE ----
  const names = (predicate) => sdk.verbs.filter(predicate).map((v) => v.name);
  const lanes = {
    READ_VERBS: names((v) => !v.action),
    METADATA_ACTION_VERBS: names((v) => v.class === "metadata" || v.class === "prefs"),
    SPAWN_ACTION_VERBS: names((v) => v.class === "spawn"),
    SEND_ACTION_VERBS: names((v) => v.class === "send"),
    READ_ACTION_VERBS: names((v) => v.action && v.class === "read"),
    MANAGEMENT_ACTION_VERBS: names((v) => v.class === "manage"),
  };
  const invoke = sdk.verbs.map((verb) => {
    const name = verb.name;
    const meta = INVOKE_META[name];
    const category = verbCategory(name, lanes);
    const readOnlyDocsSafe =
      lanes.READ_VERBS.includes(name) || lanes.READ_ACTION_VERBS.includes(name);
    const write = ["METADATA", "SPAWN", "SEND", "MANAGEMENT"].includes(category);
    return {
      name,
      category,
      write,
      readOnlyDocsSafe,
      audited: verb.audited,
      starter: STARTERS.has(name) || undefined,
      description: meta?.description ?? `${name}: ${verb.class}; requires ${verb.grant}.`,
      args: verb.argsSchema,
      returns: verb.resultSchema,
      examples: meta?.examples ?? [],
      sourceRef: "src-tauri/src/verbs/registry.generated.json",
    };
  });

  // ---- KIT ----
  const memberGroups = new Map();
  for (const member of publicSdkMembers(sdk)) {
    const name = `codesense.${member.name}`;
    const group = memberGroups.get(name) ?? [];
    group.push(member);
    memberGroups.set(name, group);
  }
  const kit = [...memberGroups].map(([name, members]) => {
    const meta = KIT.find((k) => k.name === name);
    return {
      name,
      category:
        meta?.category ??
        (members.some((m) => m.owner === "VerbMethods")
          ? members.some((m) => m.action)
            ? "action"
            : "data-read"
          : "sdk"),
      signature: [...new Set(members.map((m) => m.signature))].join("\n"),
      wraps:
        meta?.wraps ??
        (members.some((m) => m.owner === "VerbMethods")
          ? `invoke('${members[0].name}', args)`
          : "declared SDK surface"),
      description:
        members
          .map((m) => m.description)
          .filter(Boolean)
          .join(" ") ||
        meta?.description ||
        `SDK member ${members[0].name}. See codesense.d.ts for named types.`,
      example: meta?.example ?? `// See the exact declaration for ${name}.`,
      sourceRef: "src-tauri/src/http/routes/dashboard_kit/codesense.d.ts",
      ...(STARTERS.has(members[0].name) ||
      ["codesense.invoke", "codesense.subscribeContext"].includes(name)
        ? { starter: true }
        : {}),
    };
  });

  // ---- CLASSES ----
  const classes = CLASSES.slice();

  // ---- TOKENS ----
  const tokens = tokenModel(sourceTree(root)).tokens.map((t) => ({
    name: t.name,
    ...tokenRole(t.name),
    fallbackChain: t.sources,
    sourceRef: "tokens.css + doc-theme.ts",
    light: t.light,
    dark: t.dark,
  }));

  // ---- EVENTS ----
  const eventNames = parseEventNames(viewerSrc);
  const events = Array.from(eventNames)
    .sort()
    .map((name) => {
      const meta = EVENT_META[name];
      if (!meta) {
        warnings.push(
          `generate-primitives-reference: no EVENT_META entry for '${name}'. ` +
            `Add one in scripts/generate-primitives-reference.mjs (EVENT_META table).`,
        );
      }
      return {
        name,
        direction: meta?.direction ?? "parent-to-doc",
        payload: meta?.payload ?? "unknown — add to EVENT_META",
        timing: meta?.timing ?? "unknown",
        description: meta?.description ?? `(TODO: describe ${name})`,
        example: meta?.example ?? "// no example yet",
        sourceRef: "viewer.rs",
      };
    });

  // ---- CONTEXTS ----
  // Verify the four sandbox strings still appear in html-viewer.tsx so a
  // change in capabilities surfaces loudly. The file lives outside the four
  // source-of-truth parsers above; skip the check silently when it's not
  // readable (e.g. a trimmed test harness).
  const htmlViewerPath = join(
    root,
    "src",
    "features",
    "dashboard",
    "components",
    "html-viewer.tsx",
  );
  if (existsSync(htmlViewerPath)) {
    const htmlViewerSrc = readFileSync(htmlViewerPath, "utf8");
    for (const ctx of CONTEXTS) {
      // The baseline sandbox string isn't quoted literally; skip it.
      if (ctx.name === "baseline") continue;
      // The pane/message/centre sandbox differs only by surrounding form; a
      // substring check is intentional — this is drift detection, not parsing.
      const token = "allow-scripts allow-same-origin allow-forms";
      if (!htmlViewerSrc.includes(token)) {
        warnings.push(
          `generate-primitives-reference: expected sandbox token not found in html-viewer.tsx; ` +
            `pane contexts may have drifted. Review CONTEXTS table.`,
        );
        break;
      }
    }
  }

  const doc = {
    schema: 1,
    generatedAt: "deterministic",
    counts: {
      invoke: invoke.length,
      kit: kit.length,
      classes: classes.length,
      tokens: tokens.length,
      events: events.length,
      contexts: CONTEXTS.length,
    },
    invoke,
    kit,
    classes,
    tokens,
    events,
    contexts: CONTEXTS,
  };

  return { doc, warnings };
}

function serialize(doc) {
  return `${JSON.stringify(doc, null, 2)}\n`;
}

/**
 * Walk two generated manifests and name the first entity that differs. The
 * `--check` error message points at THIS entity (plus the source file to
 * edit) so a developer doesn't have to diff by hand.
 */
function firstDrift(got, want) {
  const sections = ["invoke", "kit", "classes", "tokens", "events", "contexts"];
  for (const section of sections) {
    const a = Array.isArray(got?.[section]) ? got[section] : [];
    const b = Array.isArray(want?.[section]) ? want[section] : [];
    const have = new Map(a.map((r) => [r.name, r]));
    for (const r of b) {
      const prev = have.get(r.name);
      if (!prev) return { entity: `${section}: ${r.name} (new)`, section };
      if (JSON.stringify(prev) !== JSON.stringify(r)) {
        return { entity: `${section}: ${r.name}`, section };
      }
    }
    for (const prev of a) {
      if (!b.some((r) => r.name === prev.name)) {
        return { entity: `${section}: ${prev.name} (removed)`, section };
      }
    }
  }
  return { entity: "primitives.json", section: null };
}

function sourceFileFor(section) {
  switch (section) {
    case "invoke":
      return "src-tauri/src/verbs/registry.generated.json + scripts/generate-primitives-reference.mjs INVOKE_META";
    case "kit":
      return "src-tauri/src/http/routes/dashboard_kit/codesense.d.ts + scripts/generate-primitives-reference.mjs KIT";
    case "classes":
      return "src/styles/viewer-themes.css + scripts/generate-primitives-reference.mjs CLASSES";
    case "tokens":
      return "src/features/dashboard/components/doc-theme.ts (TOKEN_SOURCES)";
    case "events":
      return "src-tauri/src/http/routes/viewer.rs + scripts/generate-primitives-reference.mjs EVENT_META";
    case "contexts":
      return "src/features/dashboard/components/html-viewer.tsx + scripts/generate-primitives-reference.mjs CONTEXTS";
    default:
      return "codesense-docs/getting-started/content/generated/primitives.json";
  }
}

export function buildInputs(root = ROOT) {
  const inputs = new Set();
  const tree = sourceTree(root);
  const app = {
    file(path) {
      inputs.add(join(root, path));
      return tree.file(path);
    },
  };
  buildPageSdk(app, ts);
  tokenModel(app);
  return listInputs(import.meta.url, [
    ...inputs,
    join(root, "src-tauri/src/http/routes/viewer.rs"),
    join(root, "src-tauri/src/http/routes/live_hub.js"),
    join(root, "src/features/dashboard/components/html-viewer.tsx"),
  ]);
}

function main() {
  if (process.argv.includes("--list-inputs")) {
    for (const input of buildInputs()) console.log(input);
    return;
  }
  const check = process.argv.includes("--check");
  const rootArg = process.argv.indexOf("--root");
  const root = rootArg < 0 ? ROOT : resolve(process.argv[rootArg + 1]);
  const output = join(root, "codesense-docs/getting-started/content/generated/primitives.json");
  const { doc, warnings } = buildManifest(root);
  for (const w of warnings) console.warn(w);
  const fresh = serialize(doc);
  if (check) {
    const existing = existsSync(output) ? readFileSync(output, "utf8") : "";
    if (existing !== fresh) {
      let culprit = "primitives.json";
      let section = null;
      try {
        const got = existing ? JSON.parse(existing) : {};
        const drift = firstDrift(got, doc);
        culprit = drift.entity;
        section = drift.section;
      } catch {
        /* corrupt JSON — the generic message still gets the dev there. */
      }
      console.error(
        [
          "generate-primitives-reference: committed primitives.json is stale.",
          `  entity:    ${culprit}`,
          `  file:      ${sourceFileFor(section)}`,
          `  generator: scripts/generate-primitives-reference.mjs`,
          `  fix:       pnpm gen:docs`,
        ].join("\n"),
      );
      process.exit(1);
    }
    console.log("generate-primitives-reference: ok");
    return;
  }
  mkdirSync(dirname(output), { recursive: true });
  if (!existsSync(output) || readFileSync(output, "utf8") !== fresh) writeFileSync(output, fresh);
  console.log(`generate-primitives-reference: wrote ${output}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
