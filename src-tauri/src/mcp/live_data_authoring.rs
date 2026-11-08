// SPDX-License-Identifier: MIT
//! The pure core of the MCP live-data authoring surface.
//!
//! Mirrors `story_authoring.rs`: pure builders + a pure lint, with all
//! stateful inputs (connection state, the real proxy constants, the registry
//! provider ids) passed in as **data** — no `AppHandle`, fully unit-testable.
//! The thin async handlers in `handler.rs` compose these with a single auth
//! read.
//!
//! Two capabilities:
//!
//! - [`proxy_contract_payload`] + [`data_source_catalog_payload`] + [`STARTER_HTML`]
//!   power `get_live_data_contract` (the "how" + best-practices + the live
//!   "what" + a copy-me starter).
//! - [`lint_live_data_doc`] powers `validate_live_data_doc` (the dashboard-side
//!   parity of `validate_story`).
//!
//! Non-secret by construction: the [`DataSource`] rows the catalog projects carry
//! only registry metadata, a `connected` bool, and the user's own (non-secret)
//! host — never a token.

use serde_json::{json, Value};

const PROXY_PATH_PREFIX: &str = "/api/v1/proxy/";

/// The copy-me dashboard starter, compiled into the binary so
/// an authoring agent in ANY repo can reach it. It demonstrates every
/// best-practice pattern the contract describes and provably passes
/// [`lint_live_data_doc`].
pub const STARTER_HTML: &str = include_str!("live-data-starter.html");

/// The `window.codesense` verb tokens, shared by BOTH the
/// contract text ([`proxy_contract_payload`]) AND the lint detection anchors
/// ([`lint_live_data_doc`]) so a rename can never drift the "what we teach" from
/// the "what we lint" (mirrors how the proxy header/cap bind to `proxy::*`).
/// `analyze` is reached via `window.codesense.invoke('analyze', …)`; `snapshot`
/// via `window.codesense.snapshot(state?)`.
const ANALYZE_VERB: &str = "analyze";
const SNAPSHOT_VERB: &str = "snapshot";

/// The ACTION-verb tokens the lint detects, bound to
/// the SAME strings the contract documents in [`ACTION_VERB_DOCS`] (a unit test
/// — `action_lint_anchors_are_documented_verbs` — asserts each is present, so a
/// rename can't drift "what we teach" from "what we lint", exactly as
/// `ANALYZE_VERB`/`SNAPSHOT_VERB` do for the read tier). `readWorkspace` is a
/// direct shim method (`codesense.readWorkspace(`); `runSkill`/`sendToSession`
/// are reached via `act('<verb>'`. `getEpicHandle` is a read verb, so it is
/// deliberately NOT linted (only used as the negative anchor for the
/// hard-coded-target advisory).
const READ_WORKSPACE_VERB: &str = "readWorkspace";
const RUN_SKILL_VERB: &str = "runSkill";
const SEND_TO_SESSION_VERB: &str = "sendToSession";
// The tab-management verbs, reached via `act('<verb>'`.
const OPEN_TAB_VERB: &str = "openTab";
const SEND_TO_WORKSPACE_TAB_VERB: &str = "sendToWorkspaceTab";
const OPEN_WORKSPACE_VERB: &str = "openWorkspace";
// `setStatus` (reached via `act('setStatus'`) is addressed by the doc's `path`, never a
// `storyId`.
const SET_STATUS_VERB: &str = "setStatus";

/// The shipped ACTION-tier verb surface the contract
/// documents, as a Rust source-of-truth table. Each `verb` string MUST equal a
/// member of the TS `ACTION_VERBS`
/// (`src/features/dashboard/components/html-bridge.ts`) — enforced by the
/// cross-language tripwire in `html-bridge.test.ts` (which reads THIS file from a
/// vitest test and diffs the two sets, mirroring `commands.contract.test.ts`).
/// Because Rust cannot import a TS const, the tripwire — not a shared literal —
/// is the anti-drift mechanism. Extends the `ANALYZE_VERB`/
/// `SNAPSHOT_VERB` discipline to the whole action tier, so the `action` section
/// is emitted from ONE table and never re-typed inline.
pub(crate) struct ActionVerbDoc {
    pub(crate) verb: &'static str,
    /// API documentation only — every verb runs when called. `metadata` (silent
    /// writes, bounded by a runaway write budget) · `spawn` (concurrency cap, or a runaway guard for cap-free opens) ·
    /// `send` (rate-limited) · `read` (audited) · `manage` (audited, no budget).
    tier: &'static str,
    args: &'static str,
    purpose: &'static str,
    /// A REPRESENTATIVE typed deny prefix for this verb (the full typed-prefix
    /// contract is in the `action.errors` string).
    deny_example: &'static str,
}

/// The SHIPPED action verbs. Unshipped spawn verbs
/// (openContextWorkspace/openBranchWorkspace) stay ABSENT until they
/// ship — the tripwire forces this list to grow with `ACTION_VERBS`.
pub(crate) const ACTION_VERB_DOCS: &[ActionVerbDoc] = &[
    ActionVerbDoc {
        verb: "files.list", tier: "read", args: "{scope?, team?, storyId?, agentId?, dir?}",
        purpose: "List own team, private, story hydration or agent context files; returns {rows}. MCP twin: list_context_content.", deny_example: "File not found",
    },
    ActionVerbDoc {
        verb: "files.read", tier: "read", args: "{path, maxBytes?}",
        purpose: "Read a content file; returns {text?, bytesUrl?, size, truncated, sha256}. MCP twin: read_content_file.", deny_example: "File not found",
    },
    ActionVerbDoc {
        verb: "files.move", tier: "metadata", args: "{from, to}",
        purpose: "Move a content file; returns {fromPath, toPath}. MCP twin: move_content_file.", deny_example: "File not found",
    },
    ActionVerbDoc {
        verb: "files.delete", tier: "metadata", args: "{path}",
        purpose: "Delete a content file; returns {deletedPath}. MCP twin: delete_content_file.", deny_example: "File not found",
    },
    ActionVerbDoc {
        verb: "files.create", tier: "metadata", args: "{path, content?, onCollision?}",
        purpose: "Create a content file with fail or keep-both collision behavior; returns {path}. MCP twin: create_content_file.", deny_example: "File not found",
    },
    ActionVerbDoc {
        verb: "records.create", tier: "metadata", args: "{collection, id?, fields?, body?, requestId?}",
        purpose: "Create one record in a declared collection: generated-id collections such as Notes assign an id when it is omitted, and slug collections require id. Returns the record with its id. Scope and profile are bound to the frame. Writes allow 30 per 10 seconds per frame and collection; flush writes are exempt.", deny_example: "denied: write budget",
    },
    ActionVerbDoc {
        verb: "records.write", tier: "metadata", args: "{collection, id, body? | append? | patch?, base?, requestId?}",
        purpose: "Replace, append to or patch one record's body; pass exactly one of body, append or patch. With base (the body you started from) a concurrent edit is merged: the result has merged: true and overlaps lists the regions both sides changed. Scope and profile are bound to the frame. Writes allow 30 per 10 seconds per frame and collection; flush writes are exempt.", deny_example: "denied: write budget",
    },
    ActionVerbDoc {
        verb: "records.setFields", tier: "metadata", args: "{collection, id, fields, requestId?}",
        purpose: "Set declared fields on one record without touching its body, e.g. Notes {title} or {pinned: true}. Pinned Notes list first, then most recently changed. Scope and profile are bound to the frame. Writes allow 30 per 10 seconds per frame and collection; flush writes are exempt.", deny_example: "denied: write budget",
    },
    ActionVerbDoc {
        verb: "records.delete", tier: "metadata", args: "{collection, id, requestId?}",
        purpose: "Delete one record; confirmable. Agents can't delete notes: Notes allows human deletion only, and a page acts as the human. Private collections keep the history, so records.restore can bring it back. Scope and profile are bound to the frame. Writes allow 30 per 10 seconds per frame and collection; flush writes are exempt.", deny_example: "denied: grant",
    },
    ActionVerbDoc {
        verb: "records.restore", tier: "metadata", args: "{collection, id, commit, undo?, requestId?}",
        purpose: "Restore one private record to the revision commit from records.history; the restore is itself a new revision. Shared collections have no history. Scope and profile are bound to the frame. Writes allow 30 per 10 seconds per frame and collection; flush writes are exempt.", deny_example: "denied: write budget",
    },
    ActionVerbDoc {
        verb: "setUiState",
        tier: "prefs",
        args: "{topic: 'ui:<slotId>', patch: object, requestId?: string}",
        purpose: "Persist host-owned shared slot UI state and publish a versioned delta echoing requestId. Uses the app-state store with its own namespace and the same limits as prefs.",
        deny_example: "denied: prefs budget",
    },
    ActionVerbDoc { verb: "routineResult", tier: "read", args: "{key: string}", purpose: "Read a private cached structured routine result; never launches. Returns snapshot, latest_attempt, revision and schema_compatible.", deny_example: "unavailable:" },
    ActionVerbDoc { verb: "routineStatus", tier: "read", args: "{key: string}", purpose: "Read cached routine result and latest attempt status, without launches.", deny_example: "unavailable:" },
    ActionVerbDoc { verb: "routineRefresh", tier: "spawn", args: "{key: string, variables?: Record<string,string>}", purpose: "Explicitly refresh through existing routine admission, profile, overlap and runtime policy. Call it only from an explicit user action.", deny_example: "busy" },
    ActionVerbDoc { verb: "routineCancel", tier: "manage", args: "{key: string}", purpose: "Cancel this principal's live structured result runs for a routine, retaining last-good data.", deny_example: "unavailable:" },
    ActionVerbDoc {
        verb: "setStatus",
        tier: "metadata",
        args: "{path: string | string[], status: string}",
        purpose: "Set a scanned doc's `status` frontmatter. The batch form (`path[]`) applies best-effort and resolves to a per-item result array.",
        deny_example: "denied: write budget",
    },
    ActionVerbDoc {
        verb: "setField",
        tier: "metadata",
        args: "{path: string, field: string, value: string | number | boolean | Array<string|number|boolean> | null} | {path: string, fields: Record<string, …>}",
        purpose: "Write top-level frontmatter keys of a content file (`.md`, or a mapped `cs:*` meta in `.html`) inside a team or private content root: `value: null` removes the key, numbers/booleans are written as YAML numbers/booleans, lists as block lists. One atomic write that changes only those keys' bytes; `subscribeContext` pages re-query. Field names are plain keys (letters, digits, `_`, `-`); objects and multi-line strings reject `invalid:`, a path outside every content root rejects `refusing to edit metadata:`.",
        deny_example: "denied: write budget",
    },
    ActionVerbDoc {
        verb: "setModelRate",
        tier: "metadata",
        args: "{modelPrefix, effectiveFrom (YYYY-MM-DD, local), inputPerMtok, outputPerMtok, cacheReadPerMtok?, long?: {fromTokens, inputPerMtok, outputPerMtok, cacheReadPerMtok?}} (USD per million tokens)",
        purpose: "Request a model price from a local day. Reductions and prices for unknown models stay pending until a person reviews and accepts the exact request in the affected agent's or routine's Spend cap block in the native app; neither pages nor MCP can approve. Rate increases apply immediately. `cacheReadPerMtok` is the cache-read price where it is not 0.1x input; `long` prices a call whose prompt (input, cache reads and cache writes) is over `fromTokens`. A rate is the whole price from its day on: a price it leaves out falls back to its default (cache reads at 0.1x input, one price at every prompt length), even where the earlier rate named one. Every price is above 0 and at most 100,000; `fromTokens` is a whole number from 1; `null` is the same as leaving a price out; an unknown key is refused with `invalid:`, so a misspelled price is never dropped. Usage already priced from the rate table is corrected to it; a cost the CLI reported is never changed. Returns the number of costs changed immediately; a pending request returns 0 and does not change effective prices. `codesense.modelRates()` shows effective rates, not pending requests. Pending requests and exact native approval are shown in the affected agent's or routine's Spend cap block, through agentList. Page and MCP requests use source 'set by an agent'; native acceptance uses 'set by the user'. The MCP tool `set_model_rate` follows the same rules.",
        deny_example: "invalid:",
    },
    ActionVerbDoc {
        verb: "setPrefs",
        tier: "prefs",
        args: "Record<string, JSON> (a key set to null is removed)",
        purpose: "Merge keys into THIS dashboard's own settings (read them back with `codesense.prefs()`), kept in the user's app state across restarts — a frame's localStorage is not (its origin changes every launch). One entry per dashboard instance, chosen by the app: a pane's own file, a centre dashboard's path, an inbox message. A team copy and a private copy never share; nothing is written to team content. 16 KB per dashboard (`too-large:` past it), 512 KB for all of a user's dashboards (the least recently written are dropped first); values must be plain JSON (`invalid:` for a Map, Set, BigInt, Date, function or NaN). Its own runaway budget, 30 calls per 10 s (`denied: prefs budget`), separate from the metadata write budget, and no audit line: it touches only this dashboard's own entry.",
        deny_example: "denied: prefs budget",
    },
    ActionVerbDoc {
        verb: "routeToContext",
        tier: "metadata",
        args: "{codeRepoPath, codeRepoSlug, logicalName, layer?}",
        purpose: "Route a source-repo file into a CodeSense context layer.",
        deny_example: "denied: route not applied (…)",
    },
    ActionVerbDoc {
        verb: "draft",
        tier: "metadata",
        args: "{level: string, from: string}",
        purpose: "Create + seed a NEW managed content doc derived from `from` at tier `level`.",
        deny_example: "unmanaged (verbatim CodeSense reject)",
    },
    ActionVerbDoc {
        verb: "openWorkspace",
        tier: "spawn",
        args: "{storyId: string} | {target: {kind: 'story'|'epic'|'branch'|'dir', storyId?, session?, title?, agent?, team?, epic?, repo?, branch?, path?}, headed?: boolean, initialPrompt?: string, sessionMode?: 'new'|'focus'}",
        purpose: "Open a workspace — the generic open primitive. Back-compat `{storyId}`, or a `{target}` descriptor: `story` (worktree + agent), `epic` (prepare + assemble + agent, so `getEpicHandle()`/`sendToSession` reach it), `branch` (`{repo, branch}` — check out an arbitrary branch/PR into a managed worktree with the install layer, so `runSkill('code-review')` etc. run on it; adoptable into a CLI tab), or `dir` (`{path}` — open a plain CLI terminal rooted at ANY directory, the generic kind; new tabs inherit the cwd, and the tab-management verbs `openTab`/`sendToWorkspaceTab` drive it). The `epic` target opens the doc's OWN containing epic by default; passing an explicit `{team, epic}` (team = `orgSlug:teamName`, epic = a content-root-relative path) opens ANY managed epic. `headed` defaults `true`; `headed:false` runs the agent for ANY kind (story, epic, branch, or dir) in the BACKGROUND with no visible terminal (observe/adopt/kill from the Background agents panel). A headless epic/dir backgrounds ONE agent session (not the visible tile/shell the headed open surfaces). A `story` target also accepts an optional `session` (a lowercase slug) to open an ADDITIONAL named agent session on the SAME worktree (address it later via `sendToSession('storyId#session', …)`), and an optional `title` for the tab's display name. Every kind that launches an agent takes an optional `agent` — the agent that runs: a key (e.g. `builtin/codex`) or an id found in the team, then your private space, then the starters; omitted = `builtin/claude`. An unknown agent rejects `invalid: <reason>` and spawns nothing. `agent` applies to `story` (always), `branch` (the review agent, headed or background), `dir`, and `epic` ONLY via its HEADLESS agent (`headed:false`) — a HEADED epic open is a bare terminal with no auto-launched agent, so an agent there is rejected. Omitting `session` = the story's single main session, unchanged; fire several `openWorkspace` calls with different `session` labels to run e.g. architecture-review + ux-review + dev in parallel on one story. `initialPrompt` is the spawned agent's starting prompt in the SAME call (spawn → wait-live → submit into the agent, not the shell; a slash command such as `/review` works) — story-only (the only agent-backed session-returner that threads one today); epic, branch, and dir targets reject `initialPrompt`, and an empty/whitespace prompt is treated as absent (byte-identical spawn-only). `sessionMode` picks the launch's session semantics: `'focus'` (the DEFAULT, or omitted) REUSES + focuses the story's existing session — a repeat `openWorkspace` shows the same agent, takes no second cap slot (idempotent); `'new'` FORKS a fresh session even when one is live (a retry / parallel attempt — e.g. a second reviewer), auto-minted as `storyId#attempt-N` and legible in the Background agents panel as \"StoryId · Attempt N\". Each `'new'` fork costs one cap slot and rejects `busy` when saturated (never a silent focus-instead-of-fork). STORY-ONLY (like `initialPrompt`) — epic/branch/dir reject `'new'`; to get a second ADDRESSABLE session, prefer naming it via `session`. A `story` target also takes an optional `machineId`: where the story's workspace is created when it has none yet — a machine id from the team's machines, or `\"local\"` for This Mac. A new workspace is created on that machine (its worktree copied there) and its agent starts there; an existing workspace opens where it lives, and a `machineId` naming somewhere else rejects `unavailable:` (a workspace stays where it was created). Other kinds reject `machineId`.",
        deny_example: "busy",
    },
    ActionVerbDoc {
        verb: "openDiff",
        tier: "spawn",
        args: "{repo: string, rev: string, path: string, storyId?: string, range?: {start: number, end: number, side?: 'after'}}",
        purpose: "await codesense.openDiff({repo,rev,path,storyId?,range?:{start,end,side?:after}}) → opened, commitNotLocal, worktreeRemoved, refused or unavailable. repo is a configured source slug, rev a local full/short commit SHA or WORKTREE (requires storyId); path is repository-relative. Opens a read-only source diff TAB; every tab read revalidates the attachment/worktree. Range is after-side, 1-based inclusive. repo:read required. Missing commits show This commit isn't on this Mac yet and a selectable git fetch origin <sha> command; CodeSense never fetches. Known stories offer Open in workspace using openWorkspace. Context-repo revisions are not supported.",
        deny_example: "denied: spawn budget",
    },
    ActionVerbDoc {
        verb: "openFile",
        tier: "spawn",
        args: "{path: string, team?: string, newTab?: boolean}",
        purpose: "Open a file in the CodeSense viewer. `path` is an absolute path (e.g. a `queryContext()` row's `path`) inside any synced team's content root, or a path relative to the page's own content root; with `team` it is relative to that team's content root in the page's org. A file in another team switches to that team first. Anything outside a known team content root rejects `refused:`, a missing file `not-found:`. `newTab:true` opens it in a tab of its own.",
        deny_example: "refused:",
    },
    ActionVerbDoc {
        verb: "runPrompt",
        tier: "send",
        args: "{target: string, prompt: string}",
        purpose: "Drive a prompt into an ALREADY-LIVE story session (supersedes the retired `fireSkill`). `target` is a story session handle — a bare `storyId` (the main session) or the compound `storyId#<label>` (a named session, e.g. `DEMO-11#architecture-review`) — resolved CodeSense-side, so a raw session id from the doc can never redirect the write. `prompt` is ARBITRARY text (a slash-command like `/bmad-code-review {storyPath}` is just one shape): the single token `{storyPath}` is filled CodeSense-side with the story's managed absolute path, and the line is submitted to the agent as is. It does NOT spawn and does NOT take an agent/command/model: it drives the agent that is already running. A handle with no live session rejects `unavailable:` (never spawns, never redirects) — open it first with `openWorkspace` (which owns spawn + `session` + `agent` + `initialPrompt`).",
        deny_example: "unavailable:",
    },
    ActionVerbDoc {
        verb: "sendToSession",
        tier: "send",
        args: "{target: string, text: string}",
        purpose: "Inject a chat turn into a story's already-running agent session.",
        deny_example: "denied: send budget",
    },
    ActionVerbDoc {
        verb: "sendPrompt",
        tier: "send",
        args: "{handle: string, text: string, submit?: boolean}",
        purpose: "Type into any running agent by its handle, locally or on a remote machine, without a dashboard launch controller. Submits by default; submit:false types without Enter. Returns {status, submitted, machine, code, message}; a rejected command returns status:invalid. An unknown delivery may have arrived: never retry it automatically. handle:\"active\" with submit:false pastes into the person's active terminal (the selected terminal tab of the focused workspace while its process runs, still the target while focus is in a pane): text is written as typed; a newline in it acts as Enter. It returns status typed, no-workspace or no-terminal; message is null when typed, else the reason the host showed. Ask codesense.canPasteToTerminal() first. Agents prompt running agents with send_to_session; this is the person's typed send from a page.",
        deny_example: "denied: send budget",
    },
    ActionVerbDoc {
        verb: "readWorkspace",
        tier: "read",
        args: "{path: string}",
        purpose: "Read a file (or list a dir with a trailing `/`) STRICTLY within the doc's OWN directory subtree on a managed surface. The doc supplies only `path`; the extension whitelist, `..`/symlink confinement, and size/binary guards are all enforced CodeSense-side.",
        deny_example: "traversal:",
    },
    ActionVerbDoc {
        verb: "runSkill",
        tier: "spawn",
        args: "{skill: string, args: string[]}",
        purpose: "DEPRECATED → prefer `runPrompt` (a prompt into a real, observable session) or `openWorkspace({headed:false, initialPrompt})`. Runs a REGISTERED skill on the user's agent CLI (detached, headless) with tool use ENABLED + multi-turn — the write-capable sibling of `analyze`. Bounded by its own concurrency cap.",
        deny_example: "not-allowed:",
    },
    ActionVerbDoc {
        verb: "openTab",
        tier: "spawn",
        args: "{workspaceId: string, agent?: boolean}",
        purpose: "Add a new terminal tab to an already-open GENERIC (dir) workspace and RETURN its addressable `{sessionId}`. The tab spawns at the workspace cwd; `agent:true` also launches the agent CLI into it so it can host `prompt`s. The tab-management half of a dashboard-driven dir workspace — refuses any non-generic / un-opened workspaceId.",
        deny_example: "not-allowed: openTab targets generic (dir) workspaces only",
    },
    ActionVerbDoc {
        verb: "sendToWorkspaceTab",
        tier: "send",
        args: "{sessionId: string, mode: 'command'|'prompt', text: string}",
        purpose: "Deliver text to a specific generic-workspace tab addressed by the `sessionId` returned from `openTab`. `command` submits to the shell; `prompt` injects into a verified-live agent (refuses `no-agent-in-tab` when the tab hosts no live agent — a prompt NEVER lands in a bare shell). Addressable ONLY for a tab of an OPEN generic workspace (a story/branch/epic agent session is never reachable through this verb).",
        deny_example: "no-agent-in-tab",
    },
    ActionVerbDoc {
        verb: "commands.run",
        tier: "read",
        args: "{id: string, args?: any}",
        purpose: "Run a slot dashboard's declared command by its `<dashboard>/<command>` id from `commands.list()`, as the person's chord or the command palette does: the host reveals the dashboard's slot when needed and delivers `codesense:command` once, after that page's `ready()`. The page acts for the person, so a humans-only command runs. Needs `commands:run` in the calling frame's grant (open grants it). The MCP twin `command_run` is the agent's: it is refused for humans-only commands and for dashboards whose grant lacks `commands:run`. Resolves `{requested: true}`; `unavailable:` for an inactive command.",
        deny_example: "denied: grant",
    },
    ActionVerbDoc {
        verb: "getWorkspaceStatus",
        tier: "read",
        args: "{storyId: string}",
        purpose: "The OBSERVE half of a dir/story dashboard: return `{exists, live, sessionId}` for a story's workspace, so a board renders the HONEST affordance — `exists:false` → Launch, `exists:true` → Focus, `live:true` → running — for a story it did NOT itself open. Open-state is the launch-source-agnostic viewer open list (a story worktree opened in the normal CodeSense viewer is observed too — the whole point), and `live` is a REAL child `try_wait` probe (never map membership, never 'a tab exists'). Poll it to stay current as the session starts/ends. A read: AUDITED (read:true, the observed storyId), never budgeted.",
        deny_example: "unavailable:",
    },
    ActionVerbDoc {
        verb: "closeWorkspace",
        tier: "manage",
        args: "{target: string, removeWorktree?: boolean, discardUnsaved?: boolean}",
        purpose: "The CLOSE half of a board-owned session lifecycle, with an optional DESTRUCTIVE escalation. Base behavior: close a session the board opened, addressed by the SAME opaque handle it opened/sends with (a storyId, a `{repo}#{branch}`, or an epicDocPath). Kills the handle-bound session (freeing the concurrency cap + writing one Kill audit line) AND evicts its terminal tab. Resolved CodeSense-side, so a raw session id from the doc can never close another session. Idempotent: a handle with no live binding (already closed / never opened) is a typed no-op, NOT an error — a board can call it defensively. The DEFAULT closes the SESSION, not the worktree — the worktree persists for the next open. The typical loop: `openWorkspace → (runPrompt/sendToSession) → poll getWorkspaceStatus until !live → closeWorkspace`. A still-live target is stopped mid-turn. **DESTRUCTIVE opt-in**: pass `removeWorktree: true` to ALSO reap the managed worktree (git worktree remove + registry prune) — the symmetric close of an `openWorkspace({storyId})`. DIRTY-GUARDED: a worktree with uncommitted tracked/untracked edits or unpushed commits is refused with `{closed: false, reason: \"dirty\", dirty: {uncommittedFiles, unpushedCommits}}` and nothing is removed. ESCALATION: pass `discardUnsaved: true` on TOP of `removeWorktree: true` to force the removal over that dirty state; the response lists what was lost in `discardedUnsavedWork`. CodeSense-managed hydrated files (CLAUDE.md, .claude/skills, …) are always excluded from the dirty check (AC-8). A team-workspace close with `removeWorktree: true` returns `{worktreeRemoved: false, reason: \"noManagedWorktree\"}` — the content root is NEVER removed. A remote-machine workspace returns `{worktreeRemoved: false, reason: \"remoteWorktreeReapDeferred\"}` — removing the worktree directory on a remote machine is not supported. Rate-guarded like every other `codesense.act(...)` surface.",
        deny_example: "unavailable:",
    },
    ActionVerbDoc {
        verb: "closeTerminalTab",
        tier: "manage",
        args: "{handle: string}",
        purpose: "Close ONE terminal tab opened via `openWorkspace`/`openTab` (or minted by `open_terminal_tab`/`open_remote_tab` MCP), the symmetric counterpart of the open verbs. `handle` is the opaque tab handle a `sessions()` row carries (a raw sessionId is refused with `unknownHandle`). Terminates the CLI if one is running, closes the tab, drops it from `sessions()`. Works for local AND remote tabs — the handle resolves to its host transparently. Idempotent: a handle whose session already exited returns `{closed: true, alreadyClosed: true}`; a handle nothing ever had returns `{closed: false, reason: \"unknownHandle\"}`. A session trying to close its OWN tab mid-call is refused with `{closed: false, reason: \"selfClose\"}` — the reply would never reach it. Does NOT remove the worktree even when closing the last tab (that is `closeWorkspace`'s destructive opt-in — one verb per concept). Rate-guarded.",
        deny_example: "unavailable:",
    },
    ActionVerbDoc {
        verb: "stopSession",
        tier: "manage",
        args: "{handle: string}",
        purpose: "Stop ANY live session by the `handle` a `sessions()` row carries, as the Agents panel's Stop does (a dispatched Weft worker is marked needs-attention first; frees its concurrency slot, writes one audit line). Idempotent: a handle with no live session is a no-op, so a page can stop defensively. Resolves `{stopped: true}` once the stop is delivered; `onSessions` then sees the roster change (a Weft worker's durable row stays listed).",
        deny_example: "unavailable:",
    },
    ActionVerbDoc {
        verb: "postMessage",
        tier: "send",
        args: "{to: string, text: string, onClick?: {type: 'goto-agent', agentId} | {type: 'open-file', path} | {type: 'show-message'}}",
        purpose: "Send a message from a dashboard pane into the active team's inbox. `to` is a role name (an agent id) OR the literal `\"human\"`; the message writes an inbox row AND (for a role) a pending wv_role_delivery row in one commit. Optional `onClick` is the per-message action the human-inbox UI honours when the human clicks the row: `goto-agent` focuses the agent's running terminal, `open-file` opens a path in the viewer, `show-message` (or omitted) opens the conversation in the native panel. The idempotency key is minted CodeSense-side so a double-click can't duplicate; rate-guarded under the send class.",
        deny_example: "denied: send budget",
    },
    ActionVerbDoc {
        verb: "sendAsHuman",
        tier: "send",
        args: "{org?: string, team?: string, to: string, text: string, contentType?: 'plain'|'markdown'|'html', onClick?: {type: 'goto-agent', agentId} | {type: 'open-file', path} | {type: 'show-message'}, attachFile?: string, replyTo?: string, summary?: string, inReplyTo?: number}",
        purpose: "Pass both {org, team} to act in a synced team; omit both to keep the page team. Cross-team access requires inbox:read:all-teams in the Rust frame grant, included by the default open policy. Send a message AS THE HUMAN. `to` is a running session (a `sessions()` handle; the mail lands in that session's own inbox and its inbox waiter wakes it) or, failing that, a role (any role name, usually a saved Agent's id) queued in the page's team for whichever instance claims it. `contentType` defaults to markdown; `html` is sanitized in the inbox. `attachFile` is a team-content path shown as an attachment card. `replyTo` is where replies go instead of to the human, like an email Reply-To (only there): a running session's handle, `'human'`, or `'role:<agent id>'` (that role's durable queue, so the answer outlives the session that writes it). `summary` is one plain-text line (≤280 chars) shown as the message's one-line gist in the Inbox and lists; set it for html and long markdown. Resolves `{status:'sent', messageId, deliveryId?}` (`deliveryId` for a role) or `{status:'invalid', message}` (blank text, no recipient, a replyTo session that is not running, or an unknown inReplyTo). `inReplyTo` threads the reply under an existing message in this team, sends only to `to`, and marks the answered message read only if addressed to the human.",
        deny_example: "denied: send budget",
    },
    ActionVerbDoc {
        verb: "replyAsHuman",
        tier: "send",
        args: "{org?: string, team?: string, messageId: number, text: string, contentType?: 'plain'|'markdown'|'html', summary?: string}",
        purpose: "Pass both {org, team} to act in a synced team; omit both to keep the page team. Cross-team access requires inbox:read:all-teams in the Rust frame grant, included by the default open policy. Reply AS THE HUMAN to an `inbox()` message, exactly as the Inbox pane's reply does: the reply goes only to the message's `replyTo` when it has one (for `role:<id>`, queued as a pending delivery for that role), else to its sender, threaded under it, and the original is marked read. Resolves `{status:'sent', messageId}` or `{status:'invalid', message}` (blank text, unknown message).",
        deny_example: "denied: send budget",
    },
    ActionVerbDoc {
        verb: "dismiss_agent_mark",
        tier: "metadata",
        args: "{sessionId: string, crashId: string, requestId?: string}",
        purpose: "Acknowledge the session's current crash mark. Any caller who can read the session may dismiss it, without confirmation. Uses the same persistent Rust write as native and MCP; a new crash has a new identity.",
        deny_example: "denied: session outside grant",
    },
    ActionVerbDoc {
        verb: "loadMedia",
        tier: "metadata",
        args: "{org?: string, team?: string, messageId: number}",
        purpose: "Load an inbox image from the Inbox pane after the person taps and confirms in the host. Message frames are refused. Requires inbox:write; pass both org and team for a synced team, otherwise use the page team. Fetches pending or retryable failed media through the host and emits the normal inbox change. Refuses when the team's config/inbox.yml sets media.remote: off. It cannot change the media setting.",
        deny_example: "media is off in team config",
    },
    ActionVerbDoc {
        verb: "react",
        tier: "metadata",
        args: "{org?: string, team?: string, messageId: number, emoji: string, remove?: boolean}",
        purpose: "Pass both {org, team} to act in a synced team; omit both to keep the page team. Cross-team access requires inbox:read:all-teams in the Rust frame grant, included by the default open policy. React AS THE HUMAN to an `inbox()` message with one emoji, like a tapback: one reaction per (reactor, message, emoji), so reacting again with the same emoji takes it back (`remove: true` takes it back, `remove: false` makes sure it is there, omitted toggles). A reaction is not mail: it is not unread, wakes nobody and is not a reply. It appears in the row's `reactions` list and fires the inbox-changed event. Resolves `{status:'reacted', messageId, emoji, outcome: 'added'|'removed'|'unchanged'}` or `{status:'invalid', message}` (not exactly one emoji, unknown message).",
        deny_example: "denied: write budget",
    },
    ActionVerbDoc {
        verb: "markRead",
        tier: "metadata",
        args: "{org?: string, team?: string, messageIds: number[]}",
        purpose: "Pass both {org, team} to act in a synced team; omit both to keep the page team. Cross-team access requires inbox:read:all-teams in the Rust frame grant, included by the default open policy. Mark messages to the human read, as opening them in the Inbox does (`inbox()` row `unread` turns false; a message to anyone else is left alone). One transaction and at most one inbox change event. Resolves `{marked: n}` with how many unread human rows actually changed, counting duplicate ids once.",
        deny_example: "denied: write budget",
    },
    ActionVerbDoc {
        verb: "markUnread",
        tier: "metadata",
        args: "{org?: string, team?: string, messageIds: number[]}",
        purpose: "Policy: restore unread state for human-directed messages using inbox:write, without changing archive or delivery state. Pass both org and team for a synced team, otherwise use the page team. Cross-team access uses the existing inbox:read:all-teams admission. One transaction, at most one inbox change event; resolves {marked:n}. The MCP twin is mark_unread. Architecture: a pane must hold an explicitly unread conversation against automatic read-on-display until the person opens it or chooses Jump to latest.",
        deny_example: "denied: inbox write scope",
    },
    ActionVerbDoc {
        verb: "embedMessage",
        tier: "spawn",
        args: "{messageId: number, org?: string, team?: string, release?: boolean, frameId?: string}",
        purpose: "Embed an inbox HTML message inline in a right-pane dashboard. The host mints an isolated message frame and returns {url, frameId, height, allow}. Architecture: set iframe.allow to the returned allow value before assigning src, so the host-drawn Copy action can write the clipboard from the child. This is browser delegation, not a new app grant. Human-authored HTML returns {html} through the existing sanitizer. Agent-addressed mail is refused. Pass release:true and the returned frameId when removing an embedded message. The URL contains no token; a live child talks directly to the host with its own identity and stored message team. Explicit team targets use the same inbox grant checks as openMessage.",
        deny_example: "embedMessage: message unavailable",
    },
    ActionVerbDoc {
        verb: "openMessage",
        tier: "spawn",
        args: "{org?: string, team?: string, messageId: number}",
        purpose: "Pass both {org, team} to act in a synced team; omit both to keep the page team. Cross-team access requires inbox:read:all-teams in the Rust frame grant, included by the default open policy. Open or focus an existing live HTML message tab for an inbox({scope:'to-human'}) row in the page team. Only HTML mail from an agent to the human is eligible; markdown/plain, agent-addressed, human-authored and unknown ids reject with an openMessage-prefixed error. Resolves `{opened: true, messageId}`. Preserves the current layout and marks nothing read; call markRead explicitly. The tab uses the message team bridge, action gate and preferences, exactly like native Open in tab. Render markdown/plain and show-message conversations in the page; use embedMessage for inline HTML.",
        deny_example: "openMessage: no message 42 for the human in this team",
    },
    ActionVerbDoc {
        verb: "archive",
        tier: "metadata",
        args: "{org?: string, team?: string, messageIds: number[]}",
        purpose: "Pass both {org, team} to act in a synced team; omit both to keep the page team. Cross-team access requires inbox:read:all-teams in the Rust frame grant, included by the default open policy. Archive and mark read human-directed messages in the page team in one transaction and at most one inbox change event. Resolves `{archived: n}`, counting newly archived rows once; agent mail, already archived and unknown ids are unchanged. Requires a non-empty array of positive safe integer ids. Store failures reject verbatim so a page can roll back optimistic state.",
        deny_example: "denied: write budget",
    },
    ActionVerbDoc {
        verb: "unarchive",
        tier: "metadata",
        args: "{org?: string, team?: string, messageIds: number[], restoreUnread?: number[]}",
        purpose: "Pass both {org, team} to act in a synced team; omit both to keep the page team. Cross-team access requires inbox:read:all-teams in the Rust frame grant, included by the default open policy. Undo `archive`: unarchive human-directed messages in one transaction and at most one inbox change event, with the same grant as archive. Archive also marks messages read, so pass the ids that were unread before archiving as `restoreUnread` to make them unread again; the others keep their read state. Resolves `{unarchived: n}`, counting rows that were archived; agent mail, unarchived and unknown ids are unchanged. Store failures reject verbatim.",
        deny_example: "denied: write budget",
    },
    ActionVerbDoc {
        verb: "setHumanStatus",
        tier: "metadata",
        args: "{status: 'online'|'away'|'busy'|'offline', note?: string}",
        purpose: "Set the human's status in the page's team (what agents read with get_human_status to decide whether to ping): `online`, `away` (short break), `busy` (heads-down; only critical pings), `offline` (out). `note` is a short free-text hint (<=200 chars). Resolves the new `{status, note?, updatedAtMs}`; pages get `codesense:agents-changed`.",
        deny_example: "denied: write budget",
    },
    ActionVerbDoc {
        verb: "moveWorkspace",
        tier: "prefs",
        args: "{teamKey: string, workspaceId: string, before?: string, after?: string}",
        purpose: "Move a workspace card in its team's shared order, exactly as native card drag. Supply exactly one of before or after with another workspace id. Resolves {moved:boolean}; unknown ids and unchanged placements return false. The existing agentList delta carries the resulting order. MCP twin: reorder_workspace (move_workspace moves between machines).",
        deny_example: "denied: grant",
    },
    ActionVerbDoc {
        verb: "openLauncher",
        tier: "spawn",
        args: "{task: string, org?: string, team?: string}",
        purpose: "Opens the native launch sheet with a task filled in. Supply org and team for an admitted team; team-only uses the active org. Unknown or unadmitted teams reject. Presentation only: the person launches from the sheet. Architecture: the registry admits sessions:read. Policy (open): no extra prompt. Agents launch with open_workspace.",
        deny_example: "denied: grant",
    },
    ActionVerbDoc {
        verb: "openRoutineDraft",
        tier: "spawn",
        args: "{name: string, task: string, when?: string, agent?: string}",
        purpose: "Opens a filled private routine draft under its agent. Without an agent, opens Agents to choose one. Presentation only: the person saves from the editor. Architecture: the registry admits sessions:read. Policy (open): no extra prompt. Agents create with routine_create.",
        deny_example: "denied: grant",
    },
    ActionVerbDoc {
        verb: "launchUnder",
        tier: "spawn",
        args: "{handle: string, task?: string}",
        purpose: "Open the native launch sheet under a running agent, prefilled with its placement team, manager handle and story; task optionally fills the initial task. The person still clicks Launch in the sheet. Unknown handle rejects with unavailable: no such agent. Presentation only; agents launch with open_workspace.",
        deny_example: "unavailable: no such agent",
    },
    ActionVerbDoc {
        verb: "openAgentInbox",
        tier: "spawn",
        args: "{handle: string}",
        purpose: "Reveal a running agent and open its native inbox rail, as the row menu's Open inbox does. A handle without a running session rejects with unavailable: agent is not running. Presentation only.",
        deny_example: "unavailable: agent is not running",
    },
    ActionVerbDoc {
        verb: "gotoAgent",
        tier: "spawn",
        args: "{agentId: string, preserveFocus?: boolean}",
        purpose: "Show an agent by handle or bare definition id through the native Show path: select and reveal its tab (restoring or adopting it if needed), switch its team, open saved output, or state why it cannot be shown. With preserveFocus:true the tab is still activated but keyboard focus stays on the calling frame. Without it the terminal takes focus as usual. Resolves {located,outcome}.",
        deny_example: "unavailable:",
    },
];

/// The bridge READ verbs — the Rust mirror of the TS `READ_VERBS`
/// (`src/features/dashboard/components/html-bridge.ts`), diffed by the same
/// cross-language tripwire as [`ACTION_VERB_DOCS`]. Together the two tables are
/// the whole `HTML_BRIDGE_ALLOWLIST`.
pub(crate) const READ_VERBS: &[&str] = &[
    "showMenu",
    "showPopover",
    "focusOut",
    "focusTerminal",
    "records.collections",
    "records.list",
    "records.read",
    "records.search",
    "records.history",
    "records.readAt",
    "subscribe",
    "page",
    "unsubscribe",
    "getDependencyStatus",
    ANALYZE_VERB,
    "getEpicHandle",
    "getDocDir",
    // Find context files by metadata (the one engine; `subscribeContext`
    // on the shim re-runs it on every index change).
    "queryContext",
    // Where the page lives: `{org, team, layer, contentRoot}` (null outside every
    // content root), so a page can scope its own queries.
    "context",
    // The agents read model: the same shapes as the MCP tools list_sessions,
    // agent_get, inbox_list, role_state, list_agent_updates and get_human_status.
    // The host pushes `codesense:agents-changed`, `codesense:inbox-changed` and
    // `codesense:agent-activity-changed` so a page re-reads.
    "sessions",
    "agents",
    "agent",
    "inbox",
    "inboxRollup",
    "roles",
    "updates",
    "humanStatus",
    "watchInbox",
    "watchAgentInbox",
    "watchAgentActivity",
    // Routines, agent runs, worktrees and machines: the same shapes as the MCP tools
    // routine_list, routine_get, routine_runs, routine_run, list_runs, read_run,
    // list_worktrees and list_machines. The host pushes `codesense:routines-changed`,
    // `codesense:machines-changed` and `codesense:worktrees-changed`.
    "routines",
    "routine",
    "routineRuns",
    "routineRun",
    "runs",
    "run",
    // A run's evidence and work product: the same shapes as read_run_evidence and
    // read_run_work_product.
    "runEvidence",
    "runWorkProduct",
    "worktrees",
    "machines",
    // Token use and spend per session, reported by each agent CLI (Claude and Codex by telemetry): the same
    // shape as the MCP tool session_usage, from the same function.
    "sessionUsage",
    "sessionRead",
    "whyLine",
    // Whether line attribution and turn-link capture work on this Mac: the attribution part of
    // describe_setup, from the same function.
    "setup",
    "storyChanges",
    // Many stories' totals in one read, from the story totals index; the same as the
    // MCP tool story_totals.
    "storyTotals",
    // The stored rates that price it: the seeded list prices and the ones setModelRate set. The
    // same as the MCP tool model_rates.
    "modelRates",
    // Whether sendPrompt({handle:"active"}) has a terminal to paste into; the host pushes
    // `codesense:paste-target-changed` when the answer changes.
    "canPasteToTerminal",
    // Validate a YAML / frontmatter / JSON snippet against the shape
    // named by `kind` (one of the slugs in `generated/schemas.json`). Pure
    // parsing, audit-exempt. The Rust validator wiring lands under a later
    // story; the TS handler surfaces `unavailable:` meanwhile so the
    // schema-browser dashboard falls back to its in-page parser cleanly.
    "validateYaml",
    // The calling dashboard's own settings (`setPrefs` is the write).
    "prefs",
    // Which of the calling slot dashboard's declared commands are active now.
    "commands.register",
    // The slot dashboards' commands, as command_list returns them.
    "commands.list",
];

/// The `sdk` section — the page SDK's identity as ONE list the published SDK
/// reference is generated from too: the frozen `window.codesense.version`, the
/// `schemaVersion` every `queryContext()` result carries, and
/// every allowlisted bridge verb with its tier.
fn sdk_section() -> Value {
    let verbs: Vec<Value> = READ_VERBS
        .iter()
        .map(|verb| {
            let spec = crate::verbs::builtin_registry().get(verb).expect("registered SDK verb");
            json!({ "verb": verb, "tier": "exempt", "mcpTwin": spec.mcp_twin, "presentationOnlyReason": spec.presentation_only_reason })
        })
        .chain(
            ACTION_VERB_DOCS
                .iter()
                .map(|v| json!({ "verb": v.verb, "tier": v.tier })),
        )
        .collect();
    json!({
        "version": crate::http::routes::viewer::PAGE_SDK_VERSION,
        "features": crate::http::routes::viewer::PAGE_SDK_FEATURES,
        "has": "codesense.has(name) → true when name is one of these features or a verb this frame's effective grant admits (no feature is a verb name); false until the host answers the shim's ready. Informational only: Rust still checks every call. Under the default open preset every verb reports true. Types for every verb and the SDK surface: /kit/v1/codesense.d.ts.",
        "query_schema_version": crate::context_query::model::CONTEXT_QUERY_SCHEMA_VERSION,
        "verbs": verbs,
        "frame_presentation": {
            "menus": "await codesense.showMenu({anchorRect, anchorId, row, items}) renders in the host. Rectangles are frame-relative. Returns {itemId, row} or {closed:true}; a second request closes the first.",
            "builtins": "builtin:stop, builtin:close-tab and builtin:set-role use host labels, icons, availability and handlers. Stop and Close tab always use the host confirmation. Results include builtin:true after the action settles. Unknown builtin ids reject, including builtin:relaunch.",
            "default_menu": "Architecture: put JSON in data-codesense-row: {kind:'story'|'epic'|'file', path} (team-content-relative; epic path names the folder), {kind:'agent', sessionId} or {kind:'agent', agentId}, {kind:'workspace', workspaceId}, or {kind:'group', groupId}. Nav paths resolve only against the active team's mounted nav tree; workspace/group ids resolve through its native workspace tray. Architecture: inbox tags are {kind:'conversation', key, conversations:[{with,team}]}, {kind:'message', messageId, team?}, {kind:'attachment', path, team?}, {kind:'role-queue', role, key?, team?, conversations?}. Inbox builders resolve stored messages through existing granted reads; attachment paths retain their team. Every menu reuses the native builder and handlers. Architecture: register codesense.onMenuAction(({itemId,row}) => ...) for page-local Open/Reply/Open queue/New message. Without a listener the host omits those items. Hosted HTML messages use their stored message/team and the inbox parent callback; fields and selection still take precedence. Mark as unread notifies the page before its write, holding the conversation until explicit Open/Reply/Jump to latest. Policy: Copy link is excluded until messages have an openable link. Policy: automatic text, nav and dashboard menus need no sessions:read grant; the agent menu stays gated. Unknown/unresolved tags or denied agent menus fall back to the dashboard menu. Architecture: fields or selected text at the pointer get the shared Cut, Copy, Paste, Select All menu before any row tag. The shim always suppresses the browser menu, including baseline and hosted frames. Baselines show text menus only; hosted children present through their parent. A page's preventDefault or showMenu wins in the event task or the one-tick defer. One menu at a time; reload/unmount closes it and focus returns only to a connected frame. Text edits restore the exact field and selection, including fields with no id. Architecture: readonly codesense.textMenuOpen is true only while the host holds the frame's automatic text menu; blur handlers defer only while it is true, and lifecycle onFlush must save pending field edits before teardown. Paste reads the clipboard only after a person activates Paste. Policy: empty space gets builtin:reload-pane, builtin:open-as-tab (slots only), builtin:edit-source (editable sources only); centre tabs say Reload tab.",
            "popovers": "await codesense.showPopover({kind:'agent-quick-send', sessionId, anchorRect, anchorId}) uses the host quick-send popup; kind 'agent-inbox-chat' opens the agent's inbox chat. Both resolve {closed:true}. Unknown kinds reject.",
            "focus": "codesense.focusOut({direction}) returns to the previous host element or terminal; codesense.focusTerminal() focuses the active workspace terminal. Both pass through frame admission and have no MCP twin.",
            "commands": "A slot dashboard (right-pane, dock or file-nav) declares commands in its pane-manifest.yaml entry or a <meta name=\"codesense:commands\"> JSON array: [{id, label, icon?, defaultChord?, scope: global|slot-visible|slot-focused, humanOnly?}]. The host owns their chords (rebindable in Settings, conflicts resolved there), lists them in the command palette, and delivers each run as codesense.onCommand(({id, args, dispatchId}) => …), once per dispatchId and only after the page's ready(); a closed slot is revealed first. codesense.commands.register(ids) sets which declared commands are active now (the last call replaces the set; an undeclared id is refused). codesense.commands.list() and codesense.commands.run(id, args) are the page's twins of the MCP tools command_list and command_run and need commands:run. Agents can't run humanOnly commands; the person can switch humanOnly per command or per dashboard in Settings, and team-safe locks it on for private dashboards.",
            "file_drop": {
                "drag": "{type:'codesense:file-drag', phase:'enter'|'over'|'leave'} — no paths.",
                "drop": "{type:'codesense:file-drop', paths:string[]} — absolute OS paths only on actual drop.",
                "delivery": "Architecture: native file drags reach only the topmost iframe under the native event position, when it is a visible, ready local pane slot (right-pane, dock or file-nav). The host sends leave on exiting that frame, cancellation/drag end, hiding or teardown. Published pages, nested message frames, centre tabs and baseline viewers do not receive these messages. Listen on window and require event.source === parent. Named types: CodesenseSdk.FileDragMessage and CodesenseSdk.FileDropMessage. This is a host push, with no Surface SDK method or bridge verb.",
                "grant": "Policy: reuse the existing files:read grant. The open preset includes it; team-safe panes must declare files:read. A person's explicit drop authorizes disclosure of those paths to that eligible pane only. There is no new capability, filesystem read permission or confinement change.",
                "handling": "The page decides what to do with a drop. The host does not ingest files or change a composer automatically. Drag enter/over/leave carry no paths; only an actual drop discloses paths.",
                "example": "window.addEventListener('message', (event) => {\n  if (event.source !== parent) return;\n  const message = event.data;\n  if (message?.type === 'codesense:file-drag') {\n    document.body.classList.toggle('file-drag', message.phase !== 'leave');\n  } else if (message?.type === 'codesense:file-drop') {\n    document.body.classList.remove('file-drag');\n    showDroppedPaths(message.paths); // Your page's handler decides what happens next.\n  }\n});"
            },
            "focus_return": "When the host UI closes, the host focuses the iframe and sends codesense:focus-return {anchorId}. The shim focuses that id, else its first focusable control. codesense.onFocusReturn(cb) passes {anchorId} and returns an unsubscribe; any registered handler replaces default focus."
        },
        "context_query": {
            "read": "codesense.queryContext(q) → {total, rows:[{id,path,relPath,fileName,org,team,layer,kind,mtime,title,fields,relationships?}], groups?, nextCursor?}",
            "live": "const off = codesense.subscribeContext(q, (result, err) => render(result)) — runs now and again on every context change; call off() to stop.",
            "paging": "Pass nextCursor back as q.cursor with the same query. A walk's pages read one index snapshot, kept 30 s after each page and 120 s after its first page at most (4 snapshots at most); a text query's walk pages only until the index changes. Past that, or after an app restart, the call rejects with a message starting stale_cursor: restart without a cursor (kit.queryAll and kit.subscribeAll do it silently). A cursor from another query rejects with invalid cursor.",
            "scope": "Team content is footprint-wide (every synced org) unless q.where.orgs / q.where.sameOrgAs narrows it. Private-space items only ever come from the page's own org, whatever the query asks; all-orgs files match every org.",
            "own_team": "const {org, team, layer, contentRoot} = await codesense.context() — where this page lives (null outside every content root; team is null in the private space). Scope a query to the page's own team with where: {orgs: [org], teams: [team], layers: ['team']}, or to its own content root with where: {sameRootAs: contentRoot + '/x'}.",
            "epics": "A row's relationships.parentEpic is the parent_epic/epic frontmatter; relationships.folderEpic is the nearest folder holding a README/index (the epic as the left nav shows it). Ask for both with select: [..., 'relationships'].",
            "example_board": {
                "where": { "kinds": ["story"] },
                "relationships": { "parentEpic": "DEMO" },
                "filters": [{ "key": "status", "op": "in", "value": ["todo", "in-progress", "review"] }],
                "sort": ["priority"],
                "groupBy": "status",
                "select": ["status", "priority", "assigned_to"]
            },
            "docs": "Same model as the `query_context` MCP tool — see its description for every operator.",
        },
        "sessions": {
            "read": "codesense.sessions() → the list_sessions rows: [{handle, displayName, state, launchedBy, unread, oldestUnreadAt, subagents?, story?, workspace?, machine?, role?, avatar?, agent?, progress?}]. subagents: how many of its agent's subagents run now (absent at none); state stays the agent's own, so one whose turn ended while they work is idle with subagents: N. progress: its latest signal_progress, {phase: planning|dev|review|pr|blocked|done, step?, total? (step of total), note?, atMs}, present once it reported one. state: running | idle (turn finished) | needs-you (awaiting permission) | exited | offline (its machine is disconnected; it may still run there) | unknown. unread: unread messages to its own handle (null when unreadable); oldestUnreadAt: RFC 3339. machine: 'This Mac' or the machine's label. agent: the saved Agent id it runs. role/avatar: display labels its launcher gave it.",
            "live": "const off = codesense.onSessions((list, err) => render(list)) — runs now and again whenever the roster changes; call off() to stop.",
            "stop": "await codesense.act('stopSession', {handle}) stops any listed session.",
            "paste": "await codesense.canPasteToTerminal() → boolean: true when the person's active terminal (the selected terminal tab of the focused workspace) exists and its process runs. const off = codesense.onPasteTargetChanged(() => …) runs on the payloadless codesense:paste-target-changed nudge when that answer changes; re-read then. await codesense.sendPrompt({handle: 'active', text, submit: false}) writes text there: text is written as typed; a newline in it acts as Enter → {status: 'typed' | 'no-workspace' | 'no-terminal', message (the toast the host showed, null when typed)}. Feature-detect both (typeof codesense.canPasteToTerminal === 'function') so the page still works on an older CodeSense.",
        },
        "agents": "codesense.agents() → [{key, name, description, runtime, scope (personal|team|builtin), team, runningNow}] — every agent you can launch, including builtin/claude and builtin/codex.",
        "live_hub": {
            "subscribe": "await codesense.subscribe(topic, {sinceRev?, window?: [from,to], onChange?}) returns {topic, viewKey, rev, snapshot, total?, cursor?}; inbox snapshots contain at most 200 rows. codesense.page(topic,cursor) loads older rows. onChange(snapshot, delta) receives the updated rows after each codesense:delta {topic, viewKey, rev, baseRev?, upserts, removes, requestId?}. A burst may arrive as one delta with baseRev, the revision immediately before the folded changes. Row id is stable. Revisions are per filtered team/space view; a gap resyncs. Hidden frames receive one stale marker and do no reads; showing resyncs from the last revision. firstPaint uses the same snapshot shape.",
            "stories": "Subscribe to stories with {window:[0,200],onChange}; use page('stories',cursor) for more. This is a team-scoped, grant-derived team: view over the existing native full-team pipeline scan, taxonomy, dependency graph, agent marks and recursive epic counts. Rows are keyed by path; kind is story, epic, doc or main-doc. Stories carry storyId, declared title (without a repeated id), status, taxonomy role and label, ready, blockedBy, dependencyCount, mark, sprint, metadataValid and metadataInvalidReason. Epics carry total, done and a rolled-up mark. ownerPath locates docs and main docs. Null means absent. The existing watcher feeds snapshots plus deltas; never poll. Declare team scope or use the file-nav slot for a team-bound view.",
            "navLens": "ui:file-nav mirrors the native viewer store in both directions: source {kind:'clone'|'private'} or {kind:'worktree',id}, view files|changes|history, lens everything|needs|agents|ready|active|review|blocked|not_started|done, arrange folders|stage, storiesOnly, doneOpen and historyBranches. setUiState writes the native setter; no independent persisted lens exists. files rows additionally expose starterPage, mergeMarked, shadows, overrides, installedSourceRepo, repoGlyph and repoTint from the native owners; installed groups keep their installed:// path. kit.stageRail(role,ready), kit.statusMark(mark) and kit.fileKindIcon(kind) are generated from native renders with matching metrics, colours and motion. showMenu items accept a kit icon name or {kind:'stageRail',role,ready}, {kind:'statusMark',mark}, {kind:'fileKindIcon',fileKind}; count, radioGroup with checked, and segmented:[{id,label,checked}] extend ordinary items. Choice returns the selected child itemId.",
            "files": "Use codesense.files.list({scope:'team'|'private'|'story'|'agent',team?,storyId?,agentId?,dir?}) for {rows:[{id,path,name,kind:'dir'|'file'|'story'|'epic',size?,mtime?}]}. Story uses hydrate_story's default file closure; agentId is the full agent key and uses its context want set. Own content only until the sharing gate is connected; private scope is your private space. Read with files.read({path,maxBytes?}) for {text?,bytesUrl?,size,truncated,sha256}; text is capped at 1.5 MB, with a /viewer URL for larger or binary files. files.create({path,content?,onCollision:'keep-both'|'fail'}) returns {path}; files.move({from,to}) returns {fromPath,toPath}; files.delete({path}) returns {deletedPath}. MCP twins: list_context_content (omit context and use scope), read_content_file, create_content_file, move_content_file, delete_content_file. Subscribe with codesense.subscribe('files',{scope,dir?,storyId?,agentId?,window:[0,200],onChange}); page with codesense.page('files',cursor,options). Views are grant-derived team:<key>|files:<selector>, fed by the existing context-file-change watcher; never poll. Declare slots: file-nav for one context-following sidebar body, with host-owned width/collapse/resize. The native PipelineStatusNav stays default. ui:file-nav holds per-team {expanded,selected?,scope?}; native and pane share the same expansion backing store. Teaching example: arc-i-dashboard-primitives/pane-shapes.html#file-nav.",
            "records": "Use codesense.records.collections/list/read/search/history/readAt/create/write/setFields/delete/restore. Every call takes collection; scope, org, team and profile are bound to the frame. Subscribe with codesense.subscribe('records', {collection, window?: [from,to], onChange}); records views default to 200 rows, contain no bodies, and page through codesense.page('records', cursor, {collection}). Multiple collection subscriptions are independent; unsubscribe with its callback or {collection} as the third argument. The view key is records:<scope>:<org>[/<team>]:<collection>. Writer deltas carry actor, kind and optional requestId; duplicate requests and unchanged index rows produce no delta. Private history commits later update headCommit only. Team and story collections have no automatic history.",
            "notes_example": "A private Notes page: const {rows} = await codesense.records.list({collection:'notes'}) (pinned first, then most recent); const note = await codesense.records.read({collection:'notes', id}); keep note.body as base and save with const r = await codesense.records.write({collection:'notes', id, body, base, requestId}); when r.merged is true show r.body and highlight r.overlaps. Pin with codesense.records.setFields({collection:'notes', id, fields:{pinned:true}}). codesense.records.delete({collection:'notes', id}) is confirmable and human-only (agents can't delete notes); bring a note back with codesense.records.restore({collection:'notes', id, commit}) using a commit from records.history.",
            "ui": "Use ui:<slotId> for shared selection, folds and arrangement: subscribe then codesense.setUiState(topic, patch, requestId). Native and page share the host-owned state; writes persist separately from page-private prefs and echo requestId in the resulting delta.",
            "machineStats": "Native Machines subscription: keyed by machine id (id), snapshot plus versioned deltas through this one hub. Resource fields match machines(): load1, cpuCount, memUsedBytes, memTotalBytes, diskFreeBytes, diskTotalBytes, diskPath, sampledAt and machineStatsSupported. The host watches only with subscribers; hidden/disposed frames and closed windows release subscriptions. Samples every 30 seconds while watched, no per-view polling; runtime state only.",
            "sessions": "codesense.onSessions(cb) is subscribe('sessions')-backed. The Rust projection is shared by native and MCP consumers. Inbox and agentList projections are available through subscribe. onAgentsChanged follows the agentDefinitions projection, including unused saved agents changed by a save, external edit or pull; agents() reads its cached summary rows. Rich agent({key}) remains a fresh canonical read of resolved startup and login state. Compatibility revisions preserve role and human-status notifications. onInboxChanged retains visible-only inbox invalidations. No per-frame session re-reads or hidden polling."
        },
        "agents_and_inbox": {
            "role": "One read model for agents and their inboxes. Every call returns the same shape as its MCP twin (named in brackets), so a dashboard and an agent see the same data. Team-scoped calls act on the page's own team (else the active team). Reads mark nothing read and claim nothing.",
            "agent": "await codesense.agent({key}) [agent_get] → {id, scope, created_by, revision, archived, definition, revisions, resolved, instances: [sessions() rows running it]}. Rejects `not_found: …` for an unknown key.",
            "inbox": "await codesense.inbox({scope?, agentId?, sessionId?, role?, kind?, conversation?, before?, limit?}) [inbox_list] → {messages: [{messageId, conversation, from, fromName, to, toName, text, createdAtMs, inReplyTo, inReplyToTeam, toHuman, unread, primaryFile?, attachments, archivedAtMs?, delivery, onClick?, contentType?, replyTo?, replyToName?, summary?, replyTargetEnded, via?, reactions: [{emoji, count, reactors: [handle|'human']}], kind}], humanUnread, conversations: [{with, team, unread, messageCount, lastAt, lastMessageId, lastPreview}], hasMore}. Newest first, at most 500 rows initially. conversations covers all human conversation history; unread excludes archived/read mail. conversation filters by a summary with key, before is a positive message id (exclusive); ask for limit:50 to page a conversation, including replies whose roots are outside the payload. hasMore stays true until its oldest page is returned. In a cross-team view pass {org,team} per request and merge pages by team and messageId. kind: needs-you (waits on the human) | update (informational) — terminal-typed rows are update; else the sender's kind; else needs-you when it carries a replyTo naming someone other than the human, its sender or a role its sender holds; else update (kind describes mail to the human for agent views and dashboards). Pass kind to list only that kind (applied before limit). Both kinds appear as conversations in the human Inbox; all unread human mail counts in its badge. humanUnread counts both kinds. scope: to-human | to-agents | all (default to-human with no filter, all with one). agentId: a saved Agent's role inbox + its running instances' inboxes + replies; sessionId: one session's sent and received mail; role: one role queue. delivery (role messages): {state: queued|reading|accepted|done|withdrawn, pickedUpAtMs, instance, acceptedAtMs, disposition, note, withdrawnAtMs}. replyTo (when the sender set one): where replies go instead of the sender — a session handle, human, or role:<id>; replyTargetEnded: a reply would land in a session that is no longer running. summary (when the sender set one): a one-line plain-text gist — show it in list rows instead of text. Render text by contentType (markdown default) with textContent or a sanitizer, never raw innerHTML.",
            "inboxRollup": "await codesense.inboxRollup() → {messages, conversations, roles}, the list_inbox_rollup DTO. conversations carries {with, team, unread, messageCount, lastAt, lastMessageId, lastPreview} over all history per team; page older conversation mail using inbox({org,team,conversation:with,before,limit:50}). Every synced team's messages (human, session and role recipients), agent notes and role queues. Messages use {id, conversation, team: {org, team}, from, fromName, to, toName, toRole, text, contentType, createdAtMs, inReplyTo, via, replyTo, summary, unread, delivery, kind, attachments}; unlike inbox(), the id field is id, not messageId. to is a session handle or human, or null for a role queue named by toRole. unread is relative to the recipient; filter to === human for the human inbox. Ids are unique only within team: key by org, team and id. Updates also carry team: {org, team}; roles are merged by role name. Requires inbox:read:all-teams in the Rust frame grant. The default open policy grants it to every dashboard, including team panes; team-safe limits it to declared private and built-in dashboards unless explicitly overridden. Re-read on onInboxChanged for mail, onAgentsChanged for role changes, and window message codesense:agent-activity-changed for agent notes. These events cover every team. A missing capability rejects with 'denied: grant'.",
            "roles": "await codesense.roles({role?, limit?}) [role_state] → {roles: [{role, roleName, registrations: [{instanceId, instanceName, generation, registeredAtMs, lastRenewedAtMs, expiresAtMs, live}], counts: {pending, claimed, accepted, acknowledged}, deliveries: [{deliveryId, messageId, state, from, fromName, excerpt, createdAtMs, firstClaimedAtMs, claimOwner, claimOwnerName, claimOwnerGeneration, claimExpiresAtMs, claimLapsed, receipt}], deliveriesTruncated}]}. Delivery state: pending (nobody holds it) | claimed (an instance is deciding; claimLapsed = its claim expired) | accepted (as work or no_work, not yet acknowledged) | acknowledged (finished). Open deliveries are all listed; `limit` caps acknowledged ones per role (default 20). A registration's `live` turns false at expiresAtMs.",
            "updates": "await codesense.updates({limit?}) [list_agent_updates] → {updates: [{id, sessionId, agentName, role, text, createdAtMs}]} — agents' progress notes, newest first. These notes are available to dashboards and do not appear in the native human Inbox. Read update mail sent to the human with inbox({kind:'update'}).",
            "human_status": "await codesense.humanStatus() [get_human_status] → {status: online|away|busy|offline, note?, updatedAtMs (0 = never set)}.",
            "media_messages": "inbox() and inboxRollup() image attachments carry {kind:'image', sha256?, mime?, width?, height?, bytes?, alt, sourceUrl?, sourceHost?, state:'cached'|'pending'|'blocked'|'failed', reason?, retryable}. Render cached bytes from same-origin /media/v1/<sha256>, never from sourceUrl. Pending or retryable failed rows can call act('loadMedia', {org?,team?,messageId}) after the person taps. Blocked means remote media is off in team config; offer a source link, not a fetch. Respect OS Reduce Motion with a still first frame and explicit play/pause.",
            "html_messages": "Open an agent HTML message to the human with act('openMessage', {messageId}); do not render a live message frame inside the page. Markdown/plain conversations render in the page.",
            "human_writes": "As the human, through act(): act('sendAsHuman', {to, text, contentType?, onClick?, attachFile?, replyTo?, summary?, inReplyTo?}) (to = a session handle, else a role), act('replyAsHuman', {messageId, text, contentType?, summary?}), act('react', {messageId, emoji, remove?}), act('markRead', {messageIds}), act('markUnread', {messageIds}), act('archive', {messageIds}), act('unarchive', {messageIds, restoreUnread?}) (Undo of archive; restoreUnread = the ids that were unread before archiving), act('setHumanStatus', {status, note?}). sendAsHuman, replyAsHuman, react, markRead, markUnread, archive, unarchive and openMessage accept optional {org, team} together, subject to the Rust inbox:read:all-teams grant; use the team of the rollup row. Omit both to keep the pinned team. A partial pair or unsynced target rejects with the verb named. Under open, any dashboard may target another synced team; team-safe checks the frame's grant before any inbox I/O. See the action verbs for results.",
            "live": "const off = codesense.onAgentsChanged(() => reread()) follows versioned agentDefinitions rows when saved definitions or running counts change, plus compatibility revisions for role registrations, queues and the human's status; codesense.onInboxChanged((change?) => …) when a message is stored, read, claimed or reacted to (change.changes, when present: [{team: {org, team}, messageIds}] names the messages the committed update re-read, only your team's when you are team-scoped; re-read just those); window message `codesense:agent-activity-changed` when an update is posted. Each returns an unsubscribe. Re-read with the calls above. Registrations and claims also lapse with time, so a queue view may re-read on a slow timer (e.g. every 30s while visible).",
            "times": "Every *AtMs field is epoch milliseconds.",
            "example": "const off = codesense.onAgentsChanged(render); render(); async function render() { const [list, {roles}, {messages}] = await Promise.all([codesense.sessions(), codesense.roles(), codesense.inbox({scope: 'to-human', limit: 20})]); /* draw with textContent */ }",
            "sample": "content/examples/dashboards/agent-inbox-console.html in the CodeSense docs context repo (getting-started team) shows sessions, role queues and the human inbox live, with reply and mark-read."
        },
        "routines_runs_and_machines": {
            "role": "Everything the Routines tab, run detail, run evidence, Worktrees panel and Machines panel show. Every call returns the same shape as its MCP twin (named in brackets): a dashboard and an agent see the same data. Reads only: nothing runs, nothing is marked. Times inside routine rows are RFC 3339 strings; *AtMs / *At numbers are epoch milliseconds.",
            "routines": "await codesense.routines() [routine_list data] → [{id, scope (personal|team), org, team, name, schedule_in_words, agent, where, runs_on (one_mac|each_mac), enabled (this Mac's switch), next_run_at, last_run: {status, reason?, at}|null, owner, owner_name, created_by, armed_here, needs_you, revision}]. `id` is the key every other routine call takes.",
            "routine": "await codesense.routine({key}) [routine_get data] → {id, scope, owner, owner_name, armed_here, runs_on, enabled, revision, schedule_in_words, routine (the full definition: name, task, trigger, agent, policy, ...), runs: [this Mac's last 5 runs]}. Rejects `validation: …` for an unknown key.",
            "routine_runs": "await codesense.routineRuns({key, limit?, before?}) [routine_runs data] → {runs: [{run_id, revision, source, phase, reason, linked_run_id, created_at, started_at, ended_at, result_text, detail, agent}], next_before}. Newest first; limit default 50, max 200; pass next_before as before for the next page (null on the last). phase: queued | running | succeeded | failed | needs_you (result_text is its question) | ended (exited without signal_done) | timed_out | cancelled | skipped (reason says why) | coalesced (folded into linked_run_id). result_text: what the agent produced (its signal_done summary); detail: why the run ended (a launch refusal, an unfilled task, a signal-only CLI's exit summary), or null; wake, machine and admission runs still explain themselves in result_text.",
            "routine_run": "await codesense.routineRun({runId}) [routine_run data] → one routineRuns() row plus {routine_id, routine_name}.",
            "runs": "await codesense.runs({limit?}) [list_runs] → [{outcome: {story_id, run_id, status (success|failed|needs-attention), summary?, exit_code?, started_at?, ended_at, source (signal|process_exit), automation_id?, trigger_source?, workspace?}, validated (completed|needs-attention|failed: the state to show), has_structured_transcript}] — the latest outcome of every agent run on this Mac, newest first (limit default 50, max 500). A run still going is not listed: sessions() shows it.",
            "run": "await codesense.run({runId, maxArtifactBytes?}) [read_run] → {outcome (a runs() row, or null), transcript: {turns: [{role (assistant|user|result), text?, tool_calls?: [{name, input?, result?, is_error?}], usage?: {input_tokens?, output_tokens?}}]} | null, artifact (the tail of its captured output, secrets redacted, or null), artifactTruncated}. maxArtifactBytes default 65536, max 1048576. Render transcript text and artifact with textContent.",
            "run_evidence": "await codesense.runEvidence({runId, storyId?}) [read_run_evidence] → null (still running, no outcome recorded, or no story) | {header: {runId, storyId, automationId?, trigger: {kind: 'attributed', source} | {kind: 'preEnvelope'} (no trigger recorded), scheduledAt?, firedAt?, startedAt?, endedAt?, status? (success|failed|needs-attention), summary? (secrets redacted), workflowRevisionId?}, run: {runId, parentRunId?, originNodeId?, correlationId, steps: [{stepExecutionId, nodeId, attempts: [{attemptId, attemptOrdinal (1-based), runId (pass to run() for that attempt's transcript and output), result (passed|failed|pending)}]}]}, perNodeResults: [{nodeId, summary?, outputs?: [{key, value (any JSON)}], artifacts?: [{kind (file|pr|entity), reference, label?}], footprint?: [{kind, reference, change (created|modified)}]}], authority (durable|currentStoreFallback; same shape either way)}. storyId defaults to the story the run's outcome names; pass it only for a run that worked several stories. Times are RFC 3339; absent lists are empty. Everything the built-in run-evidence view draws.",
            "story_changes": "await codesense.storyChanges({storyId, only?}) [story_changes] → all linked sessions (sessionRead facts plus basis), tasks with review ids, totals {sessions,costUsd,wallMs,activeMs,idleMs,reviewRounds,reviewTasks,lines}, unavailable, historyAsOf, warnings. reviewRounds counts the story's complete phase:review review-rounds markers, never wave/use observations (null with unavailable.reviewRounds when unreadable); reviewTasks counts review tasks (else marker tasks, warnings.reviewTasks; null with unavailable.reviewTasks); warnings.reviewRounds says when they disagree. totals.lines {ai:{total,byRuntime:{<listed CLI id>:n,other:n+unlisted}},human,unattributed,total} counts the lines added by every landed non-merge commit of the story (an unmerged branch is unavailable.branches), whatever limit or cursor (ai: exact agent author in git-ai notes; human: exact human author; unattributed: no note, unknown or association only, never human); it is null with unavailable.lines when any commit cannot be attributed (no checkout, objects not local, notes changing, incomplete history), never partial. prs[] lists merged PRs found locally, so mergedAt (committer time of mergedSha, epoch ms) is always set. Unknown cost is null with a reason; wall spans sessions, active/idle sum them. Includes branches/worktrees, locally resolved PRs and commits, totals.commits/files, planned/plannedBase, actual file facts (lines:{added,removed}|null and linesReason) and plannedUntouched. File lines use a containing branch range once, or lines changed by this story’s merged PRs against each first parent once; incomparable branches keep file lines null with unavailable.fileLines. PR url/urlReason use only local origin metadata. Commits include author. limit (default100,max500) and cursor page commits; total is known, stale cursors refused. Missing branch/squash/open PR facts carry unavailable reasons, without network; only a trail missing its merge fetches origin once (10 s bound; unavailable.historyFetch). historyAsOf {fetchedAt, originHead}. Published query shares commit sha/date, repo-relative tracked file facts and PR/merged SHAs with link basis, worktree state/basis, session id/models/cost/timing and task ids/reviews; no host or worktree paths, records, accounts, branch names, commit subjects, goals or summaries. Private reads accept include:[\"ranges\"] for current-page introduced added-line ranges with resolver session/native IDs, basis and reasons, plus rangesBasis/rangesReason; default/empty include omits these fields. No-note/merge/unavailable reads preserve durable facts with explicit reasons. The resolver still attributes the full file internally. Published queries reject include and exclude ranges. No conversation. only?: [tasks|sessions|commits|prs|branches|worktrees|actual|reviews] answers just those arrays with totals; tasks, sessions and reviews never read git, and without a git array totals.commits, files and lines are null (unavailable.commits, files and lines say why) and a cursor is stale. reviews = the story's complete review rounds {id,round,phase,scope,skill,subjectKind,subjectRef,subjectRev,high,medium,low,dismissed,converged,reportedAt}; published queries drop id, subjectKind and subjectRef.",
            "story_totals": "await codesense.storyTotals({storyIds}) [story_totals] → {stories: {<id as asked>: {totals, unavailable: {field: index into reasons}, computedAtMs, refreshing?} | {pending: true} | {error}}, reasons, pending, indexing: {done, total}} for 1 to 500 ids in one answer, from the Mac's story totals index; it never walks git. kit.storyTotals(ids) and CS.storyTotals(ids) take any number of ids, ask in parts (500 in the app; hosted, as many as fit the hosted 2 KB args limit, about 120 to 150) and merge one answer, renumbering reasons, asking again for rest, with source cache and the oldest answered_at when any part came from the cache. totals equals storyChanges({storyId}).totals and unavailable its unavailable reasons. Each ready story also has mergedAt (the latest merge time of its merged PRs, epoch ms; null with mergedAtReason), prs (its newest 5 merged PRs as {number, merged}) with prsTotal, and runtimes (sorted ids of the CLIs that reported its sessions' usage), from the same computation. It also has firstActivityAt and lastActivityAt (epoch ms over the story's sessions' usage, commits, merges, tasks and review rounds; null with activityReason when none is recorded) and openSessions (sessions with observed timing and no end that were active in the 15 minutes before computedAtMs, by usage or turn); a row computed before these existed answers them null with activityReason refreshing while it is recomputed, never pending. linesInProgress is the lines on story branches not merged yet, shaped like totals.lines (null with linesInProgressReason); they move into totals.lines on merge (after a squash or rebase merge, lines stay here while the story's branch ref still exists, local or on origin, until it is deleted). While a story's commits are still being counted its lines are the earlier ones with refreshing true, or unavailable (\"counting in the background\") when it had none, never 0. pending: not indexed yet and queued; show Loading and ask again, never 0. refreshing: a commit, merge, session, task or review marker changed the story since computedAtMs and a recompute is queued. indexing counts the scope's indexed stories (done of total). An answer past 240 KB is cut: truncated: true and rest lists the ids to ask again. Published storyTotals pins storyIds like storyChanges.storyId (every id must pass), shares totals whole and only the reasons storyChanges publishes, and answers {error: failed}.",
            "why_line": "await codesense.whyLine({repo, rev?, story?, path, start, end}) [why_line] → committed line attribution from the bound local checkout's history, without fetching. An omitted rev is the story's merged SHA, else its branch head, else origin's default branch, never the checkout's HEAD (revSource says which; historyAsOf {fetchedAt, originHead}); lines are 1-based inclusive, end - start <= 2000. Returns resolvedCommit, notesRef, ranges with exact/associated/unknown basis and live joined sessions. Linked exact agent turn pointers are {sessionId,toolUseId,turn,reason?}, with a zero-based index from stored conversation metadata. Missing mappings use range.unavailable.turn; expired or dropped text keeps its index with a reason. Startup sweeps retained completed archives and live logs for attached clones with attribution enabled; completed archive events also capture while the app is open; exact-range queries reconcile on demand for these commits and parents. Stored mappings survive log deletion. Logs pruned before either capture cannot be recovered; no retention interval is assumed. Published turn and unavailable values use nested allow-lists. Missing local objects return unavailable. Full in-app JSON matches MCP; published queries omit native session ids, records, accounts, host paths and conversation.",
            "setup": "await codesense.setup() [describe_setup] → {attribution}: whether line attribution and turn-link capture work on this Mac, the same object describe_setup returns under attribution. A passive read: it never installs, launches or refreshes. attribution: status {kind: active (git-ai installed, supported and its daemon answers; a repo's own setting can still keep a worktree off, shown on worktrees rows) | not_installed | installing | install_failed with message | unsupported_version with found | unsupported with reason | error with message; off with a reason appears only if a state ever reports it here, per-launch states are on worktrees rows}, binary (path), gitAiVersion, daemonReachable, warnings [text], previousBinary (null | {version, replacement, path}: the last git-ai replacement receipt), supportedVersions, notesRef (sense: notes stay on this Mac), notesPushRepos (repos set to push notes), daemonRepoRoots (how many attached clones CodeSense lists in git-ai's daemon_repo_roots; 0: no list, every repo; null while none resolves and the earlier list is kept), capture {lastSweepAt (epoch ms, null until the first sweep since the app started), trigger (startup|watcher or null), clonesSwept, logsSeen, logsNew, traceRowsStored, sessionMappingRowsStored (counts of the last startup or watcher pass, since app start; zero when none has run), watchErrors (registration failures in the latest watch reconciliation), lastError (the latest unresolved fixed phase message, no paths)}. Published (shared) dashboards declare `live: {queries: [setup]}` and get an allow-list only: status {kind, reason? (present only when the status is off)}, gitAiVersion, daemonReachable, supportedVersions, notesRef, daemonRepoRoots (a count) and the whole capture block; never the binary path, warnings or status message text, nor the previousBinary receipt or notesPushRepos. Always this Mac's values. Example: `const {attribution:a}=await codesense.setup(); rows=[['status',a.status.kind],['git-ai',a.gitAiVersion??'none'],['daemon',a.daemonReachable],['last sweep',a.capture.lastSweepAt??'not yet since start'],['logs new',a.capture.logsNew],['last error',a.capture.lastError??'none']]`.",
            "session_read": "await codesense.sessionRead({sessionId, include?: [conversation] or [turns], cursor?, limit?, turn?, toolUseId?}) [session_read] → joined session record, nativeSessionIds, models/cost (reported|rate_table|mixed|null), tools allowed/used (usedState and usedReason; used is null when absent/unsupported/unavailable, retained on expiry; firstError is the earliest indexed failure, firstRetainedError is the first outside droppedTurns or null, each with turn/toolUseId for a verified lookup), story, run, tasks, timing and unavailable reasons. retries is null (no recorded retry signal). Unknown cost is null with a reason. Conversation include returns the existing page and explicit availability state. Turns include returns the same page as summaries {index, role, tsMs, text (200 chars), toolCalls, toolErrors, model} with no tool input or result text, and the answer holds only session {sessionId, cli, branch, timing}, story and turns (not both includes); the app only, not published. Runtime reads are machine-wide; caller scope only gates story enrichment.",
            "session_usage": "await codesense.sessionUsage({sinceMs?, untilMs?, sessionId?, cwd?, story?, limit?, cursor?}) [session_usage] → {sessions, days, totals, total, nextCursor?}: nextCursor is present only when more sessions follow (pass it back as cursor, with the same other args, to read the next page; total, totals and days count the current matches on every page; during a walk a session that becomes active moves to the first page; a walk is complete when its distinct sessionIds equal total, otherwise walk again from page 1 through continuation, merging by sessionId, up to 2 recovery rewalks; after that report the truthful latest N of M.); per session cli (claude, codex or opencode), tokens {input, output, cacheRead, cacheWrite}, messages, models, firstAtMs, lastAtMs, org, team and agent (null when unattributed), machineId (the machine it ran on, as machines() names it: 'local' for this Mac), story {id, title, status, epic, basis?} (the story the session was spent on: its story worktree, else the story its launch was opened for, else a context worktree named <team>/<epic>-<n> with basis 'associated' (no basis means exact); null when not tied to one; story: 'DEMO-5' keeps only that story's sessions), costUsd (null when a model has no list price on file), unpricedModels; days: per local day totals. Covers this Mac and the remote machines it connects to. Each model call counts once, including Codex's start-up cache warm-up, which Codex's own total leaves out.",
            "model_rates": "await codesense.modelRates() [model_rates] → [{modelPrefix, effectiveFrom (YYYY-MM-DD; 1970-01-01 for a seeded list price), inputPerMtok, outputPerMtok, cacheReadPerMtok (null: 0.1x input), long ({fromTokens, inputPerMtok, outputPerMtok, cacheReadPerMtok (null: 0.1x the long input price)} or null: one price at every prompt length), source ('set by the user' after native acceptance, 'set by an agent' for effective MCP/page requests, or the seeded list price's note)}], every stored rate (USD per million tokens), by model prefix and then by day. A model's rate on a day is the latest one from on or before that day, under its longest matching prefix. act('setModelRate', …) requests one: increases apply immediately, while reductions or prices for unknown usage remain pending and return 0 changed costs. This read shows effective rates only; review pending model/date/exact prices in the affected agent's or routine's Spend cap block. Only that native Accept can apply them; pages and MCP cannot approve.",
            "run_work_product": "await codesense.runWorkProduct({runId}) [read_run_work_product] → {branch} (the git branch the run produced) | null. The repository is the runs() row's outcome.work_product.source_repo ({owner, repo}).",
            "worktrees": "await codesense.worktrees() [list_worktrees] → {rows, reaped}: this Mac's managed worktrees. Each row: kind (story|epicIntegration|orphan), workspaceKey (story id, epic id or clone path to group by), path, branch, dirty, repoPath, liveCliSession; a story row adds storyId, sourceRepo, syncStatus, failureReason, storySourcePath, orgSlug, team, orphaned, lastReverseSyncAt, lastReverseSyncBy, attribution (null before any agent launch there; else {status, gitAiVersion, checkedAt}: what git-ai line-attribution capture was at the LAST launch, checkedAt in epoch ms; status.kind is active (hooks and env were wired, not proof a checkpoint was seen), off with reason disabled_for_repo | notes_would_push | not_managed_worktree | repo_not_attached (the worktree's repo isn't attached on this Mac; attach its clone to track the next launch), installing (relaunch after completion), install_failed with message, not_installed, unsupported_version with found, unsupported with reason (including platform or newer), or error with message); an epicIntegration row adds epicId, owner, repo.",
            "machines": "await codesense.machines() [list_machines with the page's org and team] → [{machineId ('local' = This Mac), kind (local|ssh), label, scope (null | {kind:'personal'} | {kind:'team', orgSlug, team}), declaration, binding, canPublish, status: {state: connected (with workspaces) | idle | connecting | handshaking | reconnecting | disconnected | unreachable | connectionFailed | needsCredential | authFailed | hostKeyChanged | notInstalled | versionMismatch | terminalServerFrozen, ...}, attention (agents there waiting on you), capabilities: {agentCallbacks}, terminalServer? (connected only: {serverVersion?, appVersion, upToDate, relation: same|older|newer|otherBuild, ...})}]. Also optional load1 (1-minute load average), cpuCount (logical cores), memUsedBytes (total minus available), memTotalBytes, diskFreeBytes, diskTotalBytes (all byte counts), diskPath (workspace volume path), sampledAt (server Unix epoch milliseconds), machineStatsSupported (false means unknown: update the terminal server). Resource values are self-reported runtime observations, never synced or reservations. MCP uses a sample under 30 seconds old or one one-shot sample; a read does not subscribe. The native Machines tab watches the machineStats topic while visible, using snapshot plus deltas every 30 seconds. This Mac first, then the team's shared machines, then your private ones.",
            "live": "codesense.onRoutinesChanged(cb) (a routine or one of its runs changed), codesense.onMachinesChanged(cb) (a machine was saved, removed, shared or changed connection state), codesense.onWorktreesChanged(cb) (a managed worktree was added or removed) each run cb on the host's payloadless nudge (window messages codesense:routines-changed, codesense:machines-changed, codesense:worktrees-changed) and return an unsubscribe: re-read then. Session progress arrives with onSessions.",
            "example": "const off = codesense.onRoutinesChanged(render); render(); async function render() { const list = await codesense.routines(); for (const r of list) { const {runs} = await codesense.routineRuns({key: r.id, limit: 5}); /* draw r.name and runs[i].phase with textContent */ } }"
        },
        "theme": {
            "tokens": "The host sets the app's theme on the page root as CSS custom properties, in a right pane, a centre tab and an inbox message alike: --cs-bg (the surface the page sits on), --cs-fg, --cs-muted-fg, --cs-border, --cs-surface (a raised card), --cs-accent, --cs-accent-fg, --cs-link, --cs-danger, --cs-success, --cs-warning, --cs-font-sans, plus --cs-color-scheme (dark|light). Style with var(--cs-fg) etc. and paint the body with var(--cs-bg) (or use the kit's `cs-app` body class). In a pane, and in a centre tab when the page uses --cs-* tokens, the root's `color-scheme` is set to match too. Do NOT use prefers-color-scheme: the surface follows the viewer theme, which can be dark while the app is light.",
            "read": "await codesense.theme() → {scheme: 'dark'|'light', tokens: {'--cs-bg': '#282a36', …}}, resolving once the host has sent it.",
            "live": "const off = codesense.onTheme((theme) => …) — runs now (once known) and on every theme change.",
        },
        "intent": "const off = codesense.onIntent((intent) => …) — receives the opaque JSON object (at most 4096 UTF-8 bytes) supplied to pane_open. Returns an unsubscribe. The host posts {type:'codesense:intent', intent} when ready, or after codesense:ready on a new/reloaded document. One pending intent per panel, newest wins; delivered intents are cleared and never replayed after reload. The shim buffers the last undelivered intent for a late subscriber and consumes it once. The page interprets it; the host does not.",
        "visibility": "const off = codesense.onVisibility((visible) => …) — false while the page is off screen (a collapsed section, another tab, the right pane closed, the app hidden). Pause polling while hidden.",
        "summary": "queryContext({…, select: [\"summary\", …]}) adds row.summary to each row: a story/bug/spike/epic's overview — the text of the first configured summary heading (Settings → Summary headings, default ## Purpose then ## Story; the same matcher as validate_story), as plain text (Markdown links, emphasis, code and bullets removed, HTML tags removed, entities decoded), whitespace collapsed, capped at 500 characters; null when no heading matches within the first 16 KB of the file, or for other files. Feature-detect it: an older CodeSense omits the key (`'summary' in row`). Parsed once when the index reads the file, so selecting it costs no file reads. Rows carry it only when selected.",
        "prefs": "if (codesense.prefs) { const p = await codesense.prefs(); … } → this dashboard's own settings object ({} at first); await codesense.setPrefs({details: 'compact'}) merges keys (null removes one) and resolves the result. Feature-detect both (an older CodeSense has neither: fall back to localStorage). Kept in the user's app state by DashboardRef identity; a private copy inherits installedFrom preferences until its first write, then keeps its own settings. Settings survive restarts; prefer it over localStorage, which a restart clears. 16 KB per dashboard.",
        "nav_selection": {
            "what": "What the left nav has selected — the open file and the folder the nav is drilled into — with the story or epic each one is. Use it to make a dashboard or right pane follow the nav (highlight the selected story, scope to the drilled epic).",
            "read": "await codesense.getNavSelection() → {org, team, file, drill} | null. file: {path (absolute), relPath, kind ('story'|'bug'|'spike'|'epic'|'doc'|… or null when not indexed), id (story id / epic id or null), title, epic (a story's parent epic, else its folder epic; an epic doc's own id)} | null. drill: {path, relPath, epic: {id, title, path, relPath} | null (the epic main doc in that folder or its nearest ancestor)} | null at the nav root.",
            "live": "const off = codesense.onNavSelectionChange((sel) => …) — runs now (once known) and once per settled change (bursts are coalesced; an unchanged selection is not re-sent; a hidden page catches up when shown). Returns an unsubscribe. Selection arrives through the frame slot context. getNavSelection() resolves null rather than wait past 5 s. Event-driven: never poll it. Feature-detect both (typeof codesense.onNavSelectionChange === 'function') so the page still works on an older CodeSense.",
            "scope": "Read-only: the host sends the selection in slot context after ready and on settled changes. The selection is always the page's own team: a right pane gets the team it is shown in, an HTML inbox message its message's team, a centre dashboard the team its file lives in. A path outside that team's content root (another team, a source repo) reads as null, never as its path.",
            "reveal": "To select or reveal a file in the nav, call codesense.openFile({path}) — it opens the file as the active tab and expands its folders. There is no separate select verb.",
            "example": "if (typeof codesense.onNavSelectionChange === 'function') codesense.onNavSelectionChange((sel) => { const id = sel && sel.file && sel.file.id; document.querySelectorAll('[data-story]').forEach((el) => el.classList.toggle('selected', el.dataset.story === id)); }); // feature-detect: older CodeSense has no nav selection",
        },
        "liveness": "Call `codesense.ready()` once your page has rendered; a pane that doesn't, and never subscribes, falls back after 3 s. The shim's automatic codesense:ready is a transport handshake only. An admitted page subscribe counts immediately, even when its snapshot waits for visibility. Denied calls do not count.",
        "pane_chrome": {
            "title": "The pane's `<title>` is its panel title.",
            "icon": "`<meta name=\"codesense:icon\" content=\"git-pull-request\">` — any Lucide icon name (kebab-case or PascalCase) for the panel tab; an unknown name shows a \"?\" glyph.",
            "badge": "codesense.setBadge({count, severity?: 'info'|'warning'|'error'}) shows a count on the panel's title bar, even while the section is collapsed; setBadge(null) or a count of 0 clears it. A reload clears it.",
        },
    })
}

/// The `action` tier section — what any HTML doc can DO (mutate state), the verb
/// catalog (from [`ACTION_VERB_DOCS`]), and the stable typed error-prefix
/// contract. Pure — built from the const table + static text.
fn action_section() -> Value {
    let verbs: serde_json::Map<String, Value> = ACTION_VERB_DOCS
        .iter()
        .map(|v| {
            (
                v.verb.to_string(),
                json!({
                    "tier": v.tier,
                    "args": v.args,
                    "purpose": v.purpose,
                    "deny_example": v.deny_example,
                }),
            )
        })
        .collect();
    json!({
        "role": "Beyond READING data, any HTML doc in your spaces can ACT — mutate CodeSense state through the bridge verbs (flip a story's status, route a file into context, draft a doc, open a workspace, run a skill, chat to a running agent). With the default open policy, every allowlisted verb runs when called without a prompt. Opt-in policy can require a host confirmation.",
        "call": "await window.codesense.act(verb, args) → Promise. `act` is a thin convenience wrapper over the same invoke RPC. Resolves with the verb's result; REJECTS with a typed-prefix Error when the call fails.",
        "verbs": verbs,
        "policy": crate::verbs::contract(),
        "tiers": {
            "prefs": "Dashboard-local preferences: separate 30 calls per 10 s budget, no audit line.",
            "metadata": "Silent frontmatter/file and inbox-state writes (setStatus/setField/routeToContext/draft, markRead/markUnread/archive/unarchive/setHumanStatus). A runaway guard caps a burst of write calls (a batch `setStatus` is one call); past it the call rejects `denied: write budget` until the burst window passes.",
            "spawn": "Opens a workspace / drives an agent (openWorkspace/runPrompt/runSkill/openTab), or opens a file in the viewer (openFile) — bounded by a concurrency cap (`busy` when saturated). Opens that take no cap slot (openFile, openTab, a headed dir/epic openWorkspace) are bounded by a runaway guard instead: past a burst the call rejects `denied: spawn budget` until the window passes.",
            "send": "Injects a chat turn into an already-running agent (sendToSession/sendToWorkspaceTab, and runPrompt), or sends an inbox message (postMessage, sendAsHuman, replyAsHuman) — rate-limited (anti-flood; `denied: send budget`).",
            "read": "Reads a file / lists a dir CONFINED to the doc's own directory subtree (readWorkspace), or observes a workspace — AUDITED (read:true), never budgeted.",
            "manage": "Closes a session the board opened (closeWorkspace) — AUDITED, but NOT budgeted by the metadata write guard and NOT rate-limited like a chat send, so an autonomous cleanup pass can close many finished sessions in a row."
        },
        "errors": "A failed call REJECTS the act() Promise with a STABLE typed prefix — branch on `err.message.startsWith(...)`: `denied: write budget` (metadata runaway guard) · `denied: route not applied (…)` · `denied: send budget` (chat rate) · `denied: prefs budget` (setPrefs burst) · `denied: spawn budget` (cap-free open burst) · `busy` (concurrency cap saturated) · `not-allowed:` (runSkill: a prompt-splitting skill name / control-char arg) · `unavailable:` (backend / workspace context / agent CLI down) · `traversal:` / `unsupported-type:` / `too-large:` / `binary:` / `not-found:` (readWorkspace path/content rejects) · `refused:` / `not-found:` (openFile outside a known team content root, or a missing file) · `invalid:` (setField: a bad key name or value) · a verbatim CodeSense reject for an out-of-bounds (non-managed) path. Same typed-prefix convention as `analyze`.",
        "enforcement": "The runaway guards, the concurrency cap and managed-path confinement run in the CodeSense process, NEVER in your doc's JS. Your act() call is convenience; those guards live behind it.",
        "safe_output": "Treat any value an action returns — and all upstream/model data — as unsafe input: render with textContent / createElement, never innerHTML / outerHTML / document.write.",
        "authoring": {
            "role": "Everything a dashboard AUTHOR needs to build an acting dashboard WITHOUT reading the CodeSense source — the doc scaffold, the epic-addressing primitive, the working-directory rule, and the pre-ship check.",
            "scaffold": "Add `<meta name=\"cs:type\" content=\"dashboard\">` to <head> so the doc is enumerable as a dashboard. Nothing else needs declaring: call any verb directly.",
            "get_doc_dir": {
                "call": "await window.codesense.invoke(\"getDocDir\")",
                "returns": "the ABSOLUTE directory of this dashboard doc; null when no dashboard is mounted.",
                "use": "The anchor for building an absolute path to a SIBLING doc (e.g. a story) for runSkill — available WITHOUT an open epic workspace. `abs = getDocDir() + '/' + name`."
            },
            "get_epic_handle": {
                "call": "await window.codesense.invoke(\"getEpicHandle\")",
                "returns": "the epic's ABSOLUTE doc path (epicDocPath) when this dashboard is hosted INSIDE an open epic workspace; otherwise null.",
                "use": "How a dashboard addresses the epic's already-running agent (the `target` for sendToSession). When null, the doc is not inside an OPEN epic workspace — HIDE the epic-chat controls, never render a dead button. (For sibling absolute paths, prefer getDocDir, which needs no open epic.)"
            },
            "cwd_rule": "runSkill and sendToSession launch the user's agent CLI in a DIFFERENT working directory than the doc — pass ABSOLUTE paths, never doc-relative ones. Build them from getDocDir() (the board's own dir, available without an open epic); getEpicHandle() also yields an absolute anchor but only inside an open epic. readWorkspace is the exception: it is confined to the doc's OWN directory subtree, so it takes './'-relative paths.",
            "non_blocking": "Never await a full readWorkspace sweep before first paint. Paint the shell + skeleton rows synchronously, THEN hydrate rows off the read promises (Promise.all) so the UI is interactive immediately.",
            "validate": "Run validate_live_data_doc on your HTML before shipping: it flags a setStatus call addressed by storyId instead of path (blocking), a sendToSession target not resolved via getEpicHandle (advisory), and unsafe innerHTML (advisory).",
            "examples": "The CodeSense docs context repo (org codesense/docs, bundled with every install and listed in the context repo menu) has working reference dashboards in its getting-started team under content/examples/dashboards/ — kickoff-console.html (query + taxonomy + act('openWorkspace') + act('setStatus', {path, status})), sprint-analysis.html (query + sprints + taxonomy), agent-inbox-console.html (sessions + roles + inbox + act('replyAsHuman') / act('markRead'), live on onAgentsChanged / onInboxChanged), with routine snapshots for external data. Copy one into your team's or Private content to start from it, and validate the finished dashboard before use."
        }
    })
}

/// The `kit` section: the dashboard kit every live doc gets (served by
/// `http::routes::dashboard_kit`) — tokens, `cs-` classes and `window.codesense.kit`,
/// so a page can match the built-in panes without CodeSense's source.
fn kit_section() -> Value {
    json!({
        "role": "Every live page (a dashboard file, a right-pane pane, an html inbox message) gets the dashboard kit with no tag to add: a stylesheet of tokens and opt-in `cs-` classes, and window.codesense.kit. A page that uses neither looks as it did. Use them to look like the built-in panes (Agents tree, Pipeline, Inbox, Routines).",
        "tokens": "CSS custom properties beside the theme colours: --cs-space-1..6 (4, 8, 12, 16, 24, 32px), --cs-radius-sm / --cs-radius / --cs-radius-lg / --cs-radius-full, --cs-text-xs / -sm / -base / -lg / -xl (11–18px), --cs-leading, --cs-font-mono, --cs-hover and --cs-inset (derived surfaces), --cs-shadow; the agent-state palette --cs-state-running, --cs-state-idle, --cs-state-needs-you, --cs-state-exited, --cs-state-offline, --cs-state-unknown; signal_progress phases --cs-phase-planning | -dev | -review | -pr | -blocked | -done; default story statuses --cs-story-planning | -todo | -in-progress | -blocked | -review | -qa | -done (a team's taxonomy() colours win).",
        "classes": "cs-app (on <body>: theme background, text, font, spacing), cs-stack / cs-row / cs-grid / cs-spacer / cs-toolbar (layout), cs-card + cs-card-header + cs-card-body (is-interactive, is-selected), cs-list, cs-table (cs-num cells), cs-badge (is-accent | is-success | is-warning | is-danger, or style --cs-badge-color), cs-count, cs-dot (data-state = a sessions() state), cs-btn (is-primary | is-ghost | is-danger | is-sm | is-icon), cs-input, cs-tabs + cs-tab (aria-selected=\"true\"), cs-empty, cs-notice (is-warning | is-danger), cs-avatar, cs-progress (a <span> child with a width), cs-kv (a <dl>), cs-pre, cs-drawer + cs-drawer-header + cs-drawer-body (a side panel; toggle `hidden`), cs-segmented (a group of buttons, aria-pressed=\"true\" on the chosen one), cs-btn[aria-pressed] (a toggle), cs-live (a live/stale indicator: is-stale, is-loading), cs-skeleton*, cs-h1 / cs-h2 / cs-h3, cs-muted, cs-small, cs-xs, cs-mono, cs-truncate, and cs-dag* (kit.dagView / kit.dagSvg; nodes get is-selected, is-focus, is-upstream, is-downstream, is-dim). Inside cs-app, [hidden] always hides.",
        "helpers": {
            "h": "kit.h(tag, {class, text, style, dataset, onClick, ...attrs}, ...children) → an element; strings become text, never HTML. kit.s(...) is the same for SVG.",
            "icon": "kit.icon(name, {size?, strokeWidth?, class?, title?}) → an inline <svg> Lucide icon (the app's glyphs, ISC licence). kit.iconNames() lists the bundled set (agents, stories, routines, machines, chrome: bot, network, workflow, git-branch, git-pull-request, inbox, calendar-clock, server, terminal, ...); an unknown name draws circle-help.",
            "status": "kit.statusDot(state) and kit.statusBadge(state) for a sessions() state (running | idle | needs-you | exited | offline | unknown), with the app's colours and labels; kit.phaseBadge(phase) for progress.phase; kit.storyBadge(status, taxonomyState?) for a story. kit.agentStates / kit.phases / kit.storyStatuses are the palettes ({label, color, icon}).",
            "avatar": "kit.avatar({face?, name, size?}) → a round avatar: a built-in face ref (a sessions() row's avatar, e.g. 'builtin-funny:coffee-bot') draws that face, anything else the name's initials.",
            "layout_dag": "kit.layoutDag(nodes, edges, {direction: 'TB'|'LR', nodeWidth: 160, nodeHeight: 44, nodeSep: 24, rankSep: 56, group?: (node) => key, maxWidth?}) → {width, height, nodes: [{id, x, y (top-left), cx, cy, width, height, layer, order, data, group?}], edges: [{from, to, points: [{x, y}], reversed, cross?, data}], groups?: [{key, x, y, width, height, count}], reversed, skipped, crossings}. nodes are ids or {id, width?, height?, ...}; edges {from, to} with `from` drawn first (launcher → launched, dependency → dependent). Layered by longest path, ordered to cut crossings; a cycle is drawn by reversing a back edge (listed in `reversed`); self-loops and unknown ids are skipped. With `group` (an epic, a machine) each group is laid out in its own framed cluster and edges between clusters are marked `cross`.",
            "dag_view": "const view = kit.dagView(container, {renderNode: (node) => element, renderGroup?, group?, onSelect?(id), onActivate?(id) (double-click), nodeVersion?(data), ...layoutDag options}); view.update(nodes, edges) on every read. A pannable, zoomable graph (drag or wheel to pan, pinch or ⌘-wheel to zoom, buttons to zoom and fit), packed to the view's shape. A re-read with the same shape re-renders only the nodes whose data changed and keeps pan and zoom; hovering or selecting a node lights everything upstream and downstream. view.select(id), view.center(id), view.fit(), view.layout(). Use this for an agent launch tree or a story dependency graph.",
            "dag_svg": "kit.dagSvg(layout, (node) => element, {edgeClass?}) → a static <div class=\"cs-dag\">: an SVG of curved arrows with each node's element (e.g. a div.cs-dag-card) placed over it. kit.edgePath(points, direction) is the path data alone.",
            "keyed": "kit.keyed(container, items, {key, render, version?}) renders a list by key, reusing an item's element while its version (default JSON) is unchanged, so a live re-read patches rows instead of redrawing them.",
            "skeleton": "kit.skeleton({rows, avatar?}) → shimmering placeholder rows to show until the first read lands.",
            "call": "kit.call(verb, args) → the named helper (codesense[verb]) when this app has it, else invoke(verb, args); a call this app does not know rejects with err.unavailable === true, so show a notice instead of failing. kit.has(verb) checks first.",
            "live": "const stop = kit.live(render, {events: ['sessions-changed', 'agents-changed'], pollMs: 30000}) runs render now, again (coalesced) on each named host event (codesense:<event>), and every pollMs while on screen. kit.on(event, cb) listens for one event.",
            "storyTotals": "kit.storyTotals(ids) -> Promise<{stories, reasons, pending, indexing, source}> for any number of story ids: asks codesense.storyTotals 500 ids at a time and merges one answer (reason indexes renumbered into one reasons list; rest asked again).",
            "queryAll": "kit.queryAll(name,args,opts?) -> Promise<{rows,total,returned,complete,truncated,loading,error,canLoadMore,atCeiling,...}> for sessionUsage, queryContext, storyChanges (commits by sha; sessions metadata retained). opts override shapes: rows key path or answer=>array, id key or row=>id, cursor:{arg,next} with next key path or answer=>cursor|null, total key path or answer=>number. Undefined options keep defaults; cursor arg/next merge individually. Unknown verbs page only with opts.rows; otherwise single-answer behavior. App-only inbox needs opts {rows:'messages',id:'messageId',cursor:{arg:'before',next:a=>a.hasMore?a.messages.at(-1)?.messageId??null:null}}; app-only routineRuns needs opts {rows:'runs',id:'run_id',cursor:{arg:'before',next:a=>a.next_before}}. Captures filters and effective limit (default100); sequential pages, hard maxRows5000, up to 3 silent walks from the head on stale_cursor (only the new walk replaces rows), one other stale restart and <=2 full same-limit recovery walks. Complete only when known numeric total equals distinct loaded rows and no cursor. Unknown/overshoot stay partial; zero can be complete. Latest total/totals/days replace, never sum. Explicit page asks back off5s*n up to6 retries; ordinary errors reject. Usage identity exact(cli,sessionId); machineId metadata. No snapshot guarantee.",
            "subscribeAll": "kit.subscribeAll(name,args,everyS,cb,opts?) -> callable stop with Promise-returning stop.loadMore()/refresh(). opts.manual:true makes each loadMore fetch one next page; default auto-walks. Heads stay live (app context/usage subscriptions, other verbs poll while visible), retain loaded depth; full reconciliation at restEveryS60 or changed total in automatic mode. Identical cached head bodies preserve newer page rows/summary. Only a successful complete fresh traversal replaces membership. Live errors in result.error retain rows/cursor. Stop settles own lifecycle promises, clears own backoff and subscriptions; dispatched transport may finish.",
            "partialList": "kit.partialList({container,render,subscribe:cb=>kit.subscribeAll(...,cb,{manual:true}),onResult?}) -> {el,loadMore(),refresh(),stop()}. Binds the real subscription handle, controls loading/unknown/zero/complete/ceiling/error/Retry, prevents double clicks and handles Promise failures. Existing-layout binding: {status:element,button:element,subscribe,onResult}; no extra row list. Optional query:()=>queryAll(...) supports a one-time list. Legacy loadMore callback remains supported. One handle per resource/scope; stop the old component before scope change or rebinding status/button. Wrap results in your resource store run(resource,scope,fn); its slot owns value/error/token/scope, the handle owns pages. Stop on pagehide/teardown.",
            "patterns": "Fast, light pages, the same names in the app (kit.*, also window.CS) and on a shared page (CS.*): CS.boot({render, queries:{alias:{name,args,maxAgeMs}}, key}) is the one entry for both hosts (the host is the bridge that is present, never script[src]); CS.cachedQuery(name,args,{maxAgeMs,key}).peek() -> {value,ageMs,state: cached|fresh|stale|none} paints the last answer at once and reads once; CS.visibleTotals(container,{rowSelector,idAttr,root}) asks storyTotals only for rows on screen; CS.section(el,{key,load,render,when:'open'|'visible',scope}) reads when a <details> opens, with its own skeleton, empty, unknown, error+Retry and 'Still reading' with Cancel; CS.budget({inFlight:2}) is the page-wide read queue; CS.value(v,{pending,unavailable,error}) never turns a missing figure into 0; CS.keyedRegion(el,scope) repaints in place. Docs: arc-i-dashboard-primitives/fast-light-dashboards.html.",
            "misc": "kit.timeAgo(msOrIso), kit.empty(text, icon?) (say what to do next, e.g. how to start one), kit.notice(text, 'info'|'warning'|'danger', {label, onClick}?) (e.g. an error with Retry)."
        },
        "example_paged_list": "<div id=items></div><script>const sdk=codesense.kit; const view=sdk.partialList({container:document.getElementById('items'),subscribe:cb=>sdk.subscribeAll('queryContext',{},15,cb,{manual:true}),render:row=>{const el=document.createElement('div');el.textContent=row.title||row.fileName||row.id||row.path;return el;}});window.addEventListener('pagehide',()=>view.stop(),{once:true});</script>",
        "example_agent_dag": "<body class=\"cs-app\" style=\"height:100vh;display:flex;flex-direction:column\"><div id=g style=\"flex:1;min-height:0\"></div><script>const kit = codesense.kit; const view = kit.dagView(g, {nodeWidth: 220, nodeHeight: 56, renderNode: n => kit.h('div', {class: 'cs-dag-card'}, kit.h('div', {class: 'cs-row'}, kit.avatar({face: n.data.row.avatar, name: n.data.row.displayName, size: 18}), kit.h('b', {class: 'cs-truncate', text: n.data.row.displayName}), kit.statusDot(n.data.row.state)), kit.h('div', {class: 'cs-xs cs-muted cs-truncate', text: (n.data.row.progress && n.data.row.progress.note) || n.data.row.machine || ''})), onActivate: id => codesense.act('gotoAgent', {agentId: id})}); kit.live(async () => { const rows = await kit.call('sessions'); const handles = new Set(rows.map(r => r.handle)); view.update(rows.map(r => ({id: r.handle, row: r})), rows.filter(r => handles.has(r.launchedBy)).map(r => ({from: r.launchedBy, to: r.handle}))); }, {events: ['sessions-changed', 'agents-changed']});</script></body>",
        "states": "Show every state: kit.skeleton() while loading, kit.empty() that says how to get started when there is nothing, kit.notice(…, 'danger', {label: 'Retry', onClick}) on an error, and a short notice when kit.call rejects with err.unavailable (an app too old for the call). Below ~560px (a narrow right pane) prefer a list or tree to a graph.",
        "references": "Working rebuilds of built-in panes in the CodeSense docs context repo (getting-started team), content/examples/dashboards/: agent-dag.html (the Agents tree as a launch DAG), pipeline-graph.html (story dependency graph), routines-console.html (routines, run history, run detail). Copy one to start."
    })
}

/// The local dashboard authoring contract — the mechanics AND the transferable
/// best-practice patterns. Pure static — no auth read.
pub fn local_contract_payload() -> Value {
    #[allow(unused_mut)]
    let mut payload = local_contract_base();
    // Hosted (published) dashboards ask the owner's Mac declared queries.
    #[cfg(feature = "dashboard-publish")]
    {
        payload["hosted"] = hosted_section(crate::dashboard_publish::catalog::CATALOG);
    }
    payload
}

/// The copy-me helper a hosted page uses to talk to its shell: the
/// toolkit `cs-live.js` itself, the same bytes
/// `create_dashboard` writes. It uses `postMessage` only: `window.codesense`
/// is refused in published pages. The page posts to its own window (the
/// frame's setup script forwards it with the frame token), and replies,
/// presence and the viewer's hello arrive from `parent`.
#[cfg(feature = "dashboard-publish")]
const HOSTED_HELPER: &str = crate::dashboard_publish::create::TOOLKIT_JS;

/// The hosted section of the contract, generated from the query
/// catalog, so a new catalog entry is documented with no other change.
#[cfg(feature = "dashboard-publish")]
pub fn hosted_section(catalog: &[crate::dashboard_publish::catalog::QueryDef]) -> Value {
    let queries: Vec<Value> = catalog
        .iter()
        .map(|q| {
            let optional: Vec<&str> = match q.fields {
                crate::dashboard_publish::catalog::Fields::Named(list) => list.to_vec(),
                crate::dashboard_publish::catalog::Fields::Frontmatter => vec![
                    "any frontmatter key except summary, relationships, body, text, path, content",
                ],
            };
            json!({"name": q.name, "describe": q.describe, "shares": crate::dashboard_publish::catalog::shares_rule(q), "fields": optional})
        })
        .collect();
    json!({
        "what": "A dashboard published to codesense.ai can ask the owner's Mac live, read-only queries while the Mac is connected. The page posts each request to its own window with postMessage(msg, '*') (not parent.postMessage); the frame forwards it to the shell, which relays through codesense.ai to the Mac and back.",
        "declare": "List the queries the page uses in dashboard.yml: `live: {queries: [sessionUsage, runs]}`; `queries: []` shows only whether the owner's Mac is online; `queries: all` allows every query below, now and later. A person confirms a new or wider list once in the CodeSense app; until they do, publish_dashboard publishes the page without live data and returns live: pending_confirmation. Only declared queries are answered; any other gets query_not_declared. Editing the block of a published dashboard takes effect within seconds while CodeSense is open, without publishing again; removing it turns live data off. Secrets (publisher tokens, API keys, Keychain) are never answered.",
        "install": {
            "declare": "In dashboard.yml add install: {name: Family brief, short_name: Brief, icon: images/icon.png, theme_color: {light: '#f7f6f3', dark: '#201e1c'}}. The icon must be a .png path in publish.files. install and chrome take effect on the next push or scheduled check-in; removing install removes home-screen metadata.",
            "name": "name is 1 to 64 characters; short_name is optional and at most 30. Keep short_name to 12 characters or fewer. If name is longer than that, set a short_name. The document title stays the dashboard title.",
            "icon": "Use a square, opaque PNG of at least 180 x 180; 512 x 512 is better for both iOS and Android. iOS rounds corners itself and fills transparency with black. Do not draw rounded corners or a shadow. Keep the mark inside the middle 70%, without words. Size and opacity are advice, not enforced.",
            "theme_color": "Set theme_color to your page background: light and dark are #rrggbb. Defaults are #f5f5f7 light and #161618 dark. The shell owns the safe-area padding; do not add it again in the page.",
            "offline": "Offline use is not supported; installation does not add a service worker or cache the dashboard for offline use."
        },
        "chrome": "chrome: none (default), full or minimal. With none the viewer draws nothing over the page, for the owner and for viewers: no bar, no badge. The page gets the state through CS.viewer(cb) ({role, mac, mac_seen_at, updated_at, stale, display}) and shows its own as-of time, Mac status and offline or stale notice; CS.endOwnerAccess() ends the owner's access (owner only, from a click). A page that has not called CS.viewer within 3 s gets the viewer's own offline and stale notice, so a page that never adopted the SDK is not silent. Ended access, a page that kept leaving the dashboard, and a refused link are always drawn by the viewer; a page cannot block or hide them or the server ending access. A dashboard with no stored presentation becomes none the next time the viewer serves it, with no republish. A stored full or minimal is kept until the next publish: older manifests with install: commonly stored full, so republish those to drop the bar. chrome: full shows the viewer bar (title, Page updated, Mac status, and for the owner End owner access); minimal hides it for viewers (the owner always sees the full header), with notices and the live stale banner still shown; show your own as-of time. A chromeless page can imitate CodeSense UI: never ask a viewer for a password or code in a dashboard. Sign-out and the dashboards a viewer can see live on the share site's /account page, outside the page.",
        "queries": queries,
        "args": "The dashboard sets what a query shares; a viewer's args only narrow it. sessionUsage takes {sinceMs?, untilMs?, sessionId?, limit?, cursor?}, plus story? (one story id: only the sessions spent on it) when the dashboard shares story and cwd? when it shares cwd; a dashboard that lists `live: {fields: {sessionUsage: [story]}}` gets each row's story {id, title, status, epic} or null, never a path, so a page can group usage by story; sessions {launchedBy?}, setup {} (no args; a fixed allow-list of attribution and capture status), runs {limit?}, queryContext {match?: {field: value | [values]}, sort?: [field | -field], limit?, groupBy?, cursor?} on the fields the dashboard shares, always in one org and team: the ones whose context repo folder holds the dashboard folder (the org and team query_context lists for a file there), worked out when publish_dashboard runs. A dashboard outside a team's context repo, or published while CodeSense could not tell its team, gets answer_failed. Unknown or wrong-typed args give query_invalid. Rows are capped at 500 (sessionUsage and runs default to 50); see answer_size for the 256 KB cap.",
        "pins": "Narrow hosted queries with live.args: {storyChanges: {storyId: [DEMO-46]}, whyLine: {repo: [acme/app]}}. Each declared query's known arg takes 1–64 literal strings or integers, sorted and deduped. Matching is exact by JSON type and value (5 differs from \"5\"); every pinned arg must be sent, including optional args such as whyLine.rev. Unpinned args stay open. Pins apply to owners too. Viewers require storyChanges.storyId, storyTotals.storyIds and whyLine.repo pins; share_dashboard add refuses until those pins exist, even with queries: all. Owner-only publish and unpinned owner asks stay allowed. Unknown live keys (including arg instead of args) are refused. Edits take effect when the watcher saves the block, without republishing; until then asks fail closed. Requests carry live_args; the share server refuses before relay or cache and deletes a query's cache when its pins change. Only storyChanges.storyId and storyTotals.storyIds also accept {epic: DEMO} or {team: web} selectors, mixed with literals. storyTotals.storyIds is a list: every id in it must pass the pin. Selectors require a known saved team scope at publish; team must equal that saved team. At ask time the Mac resolves current stories using the context-query engine in that scope (parent_epic/epic or epic folder), and refuses unknown or foreign stories, a moved dashboard scope, or an index that is not ready. A selector satisfies the viewer pin rule. The share server checks literal args and leaves selector args to the Mac. Pin changes prune the cache. While the Mac is offline, the last answer it approved under the current pins can still be served, even for a story that has since left the epic; an answer the Mac refused never is. Online failures never serve a selector cache fallback. Pins cover queries only, not streams. The starter sends the first literal for each pinned arg of its selected query, preserving JSON types. A selector-only arg gets a named placeholder in app.js, with configuration guidance in the rendered error, warnings and nextSteps.",
        "fields": "Share more with `live: {fields: {queryContext: [status, title, priority], runs: [summary], sessionUsage: [story]}}`: for queryContext the frontmatter keys returned (default status, title, type, owner, updated); for the others the optional fields listed under each query. They are on as soon as the block is saved.",
        "page_api": {
            "query": "{cs:'live', v:1, op:'query', id, query, args}",
            "subscribe": "{cs:'live', v:1, op:'subscribe', id, query, args, every_s} (repeats at max(every_s, 15) s; at most 20 per page)",
            "unsubscribe": "{cs:'live', v:1, op:'unsubscribe', id}",
            "result": "{cs:'live', v:1, id, ok:true, source:'live'|'cache', answered_at, server_time, answer} (times are server unix seconds; source 'cache' is the last answer, shown while the Mac is offline)",
            "error": "{cs:'live', v:1, id, ok:false, error} with error arg_not_allowed | query_not_declared | mac_offline | mac_busy | mac_timeout | query_invalid | stale_cursor | answer_failed | answer_too_large | rate_limited | bad_request | access_ended | post_to_own_window | not_owner | needs_gesture | busy | failed (when each happens: guide.live_errors)",
            "presence": "{cs:'live', v:1, op:'presence', mac:'online'|'offline'|'unknown', mac_seen_at}",
        },
        "helper": HOSTED_HELPER,
        "embeds": hosted_embeds(),
        "toolkit": hosted_toolkit(),
        "streams": hosted_streams(crate::dashboard_publish::catalog::STREAMS),
        "actions": {
            "verbs": crate::dashboard_publish::catalog::ACTIONS.iter().map(|a| json!({"name": a.name, "description": a.describe})).collect::<Vec<_>>(),
            "declare": "actions maps a name to {verb, who: owner, match?, max_per_hour?, max_running?}. setFields takes fixed scalar fields (null removes a key) or choices mapping a declared choice to fixed fields. max_running applies to launched agents; routines retain their own admission/overlap policy. launchAgent takes agent, prompt (only {id}, <=500 characters), mode headless or visible-unfocused. runRoutine takes routine and fixed vars (only {id}, <=500 characters each). Agents/routines must exist. Only the owner can run actions; viewers watch activity. who: viewers is parsed for compatibility but does not permit mutation access.",
            "call": "CS.act(name, {id, choice?}) resolves the declared action result or rejects its error code. id is an ASCII alphanumeric first character followed by at most 63 letters, digits, dot, underscore or hyphen; slash and .. are refused. It must resolve uniquely in this dashboard's team and match the manifest. No text, paths, prompts or extra keys. Calls are sent once and never retried; on timeout inspect activity before deciding to try again.",
            "limits": "Defaults 5 successful calls/hour and 2 running agents per action; hard maximum 30/hour and 4 running. Relay cap 10 calls/minute/session, 1024-byte args. Errors: not_declared, not_allowed, invalid_args, not_found, rate_limited, busy, failed, mac_offline, timeout.",
            "frames": "page to relay: act {v:1, op:act, aid, action, args} -> acted {v:1, op:acted, aid, ok, result|error}; relay to Mac: x {v:1,t:x,id,dashboard,action,args,by} -> xa {v:1,t:xa,id,ok,result|error}. The relay stamps by; the page cannot choose it. Subscribe to actions for the last 50 calls and session/routine progress. Activity exposes role and share id, never an email.",
            "starter": "create_dashboard {template: bug-triage, agent: <agent_list id>} writes queryContext bugs, three owner actions, activity and the shared terminal view. Without agent it omits Work it and explains how to add it. control: true optionally enables owner terminal control."
        },
        "guide": hosted_guide(),
    })
}

/// `live.embeds`, the grant that lets an agent's HTML message run live inside a
/// published dashboard, and `run`, the publisher's switch for whom its scripts run.
#[cfg(feature = "dashboard-publish")]
fn hosted_embeds() -> Value {
    json!({
        "what": "Lets the HTML messages an agent sends to the owner run live inside a published dashboard (the Inbox page, say): the page puts each message in a sandboxed frame and forwards the message's codesense queries (queryContext, subscribeContext, inbox) to its own live pipe. The page's publisher grants what a message may read; the message never grants itself anything.",
        "declare": "live: {queries: [queryContext], fields: {queryContext: [status, title, sprint]}, embeds: {queries: [queryContext], fields: {queryContext: [status, title, sprint]}, run: owner}}. Every name in embeds must already be declared by the page itself and every field must be one the page shares: an embed never sees more than the page. A query with no field list in embeds gets its defaults, narrowed to what the page shares. embeds is part of the block's sha, so an edit is saved like any other live edit. An unknown key under embeds is refused.",
        "run": "embeds.run says for whom a message's own scripts run: none (the default: nobody, messages stay static), owner (only the signed-in owner) or everyone (every invited viewer). run needs at least one query or stream in embeds. Viewers and owners alike can only read what embeds grants: nothing in this pipe acts, so a viewer can never send, reply, mark, open or change anything through a message.",
        "risk": "With run: owner or run: everyone, agent-authored JavaScript runs on those people's devices with the same trust as on the Mac. It can read everything embeds grants and, through WebRTC (which a page cannot block), it can reveal the device's IP address to a host of its choosing. With everyone, you vouch for your agents' messages to the people you invited. Leave run: none if that is not what you want; no run is the safe default.",
        "grant_file": "publish writes cs-embeds.json from the saved block ({v, sha, queries, streams, fields, run}); it is never read from the folder and the name cannot be listed in publish.files. The page fetches it with fetch('cs-embeds.json'). A missing or unreadable file means no embeds. Older CodeSense versions ignore the key, so the file is absent and nothing runs.",
        "toolkit": "cs-embed.js (version comment `// cs-embed 1`) is the one source for the page side and the message side. Copy it next to cs-live.js and list it in publish.files. CSEmbed.mayRun(grant, CS.role()) says whether to build the frame at all; CSEmbed.createPool(upstream) bounds the page's asks; CSEmbed.createForwarder({pool, grant, role: CS.role, frame}) is the only path from the frame; CSEmbed.buildSrcdoc(body) wraps the message with a closed CSP and the message-side window.codesense.",
        "page_steps": [
            "fetch('cs-embeds.json'); if it is missing, show the message as a card or static text",
            "await CS.ready(); build the frame only when CSEmbed.mayRun(grant, CS.role()) is true, else show the card",
            "create the frame with sandbox=\"allow-scripts\" only and srcdoc = CSEmbed.buildSrcdoc(body, {head: themeStyle}), where themeStyle defines the --cs-* tokens for light and dark (see theme)",
            "create the pool over CS.query (upstream.query(name, args) returns the answer), create the forwarder with role: CS.role, and window.addEventListener('message', forwarder.handle)",
            "call forwarder.setVisible(false) for a collapsed or off-screen message"
        ],
        "theme": "The host page must supply the --cs-* theme tokens to the message frame, for light and dark, as the in-app Inbox does. A message written on the Mac styles itself with var(--cs-fg), var(--cs-bg) and the rest; a frame without them falls back to the message's own defaults (often light text, written for a dark app) and can render unreadably, for example light text on a white bubble. The tokens: --cs-bg, --cs-fg, --cs-muted-fg, --cs-border, --cs-surface, --cs-accent, --cs-link, --cs-success, --cs-warning, --cs-danger (and --cs-font-sans). Pass them through the head option of buildSrcdoc, with color: var(--cs-fg) on the body, for example CSEmbed.buildSrcdoc(body, {head: '<style>:root{--cs-bg:#fff;--cs-fg:#1d1d1f;--cs-muted-fg:#6e6e73;--cs-border:#d2d2d7;--cs-surface:#f5f5f7;--cs-accent:#0a64d6;--cs-link:#0a64d6;--cs-success:#1a7f37;--cs-warning:#9a6700;--cs-danger:#cf222e}@media (prefers-color-scheme:dark){:root{--cs-bg:#161618;--cs-fg:#e6e6e6;--cs-muted-fg:#9a9aa0;--cs-border:#333338;--cs-surface:#1f1f22;--cs-accent:#7ab7ff;--cs-link:#7ab7ff;--cs-success:#3fb950;--cs-warning:#d29922;--cs-danger:#f85149}}body{margin:0;color:var(--cs-fg)}</style>'}). The head runs under the frame's closed CSP (inline styles are allowed, nothing else loads), so put styles in it, never links.",
        "message_api": "Inside the message: window.codesense.queryContext(q), subscribeContext(q, cb(result, err)), inbox(args), has(name), ready(). openFile rejects with unavailable (it needs the Mac app). Errors carry .code: not_declared, unavailable, rate_limited, invalid_args, mac_offline, no_answer, failed.",
        "limits": "Per message 10 reads a minute and 4 subscriptions: one-shot calls and every recurring poll tick count against the same 10, so a message that over-subscribes hears rate_limited on the ticks it cannot pay for; across the page 30 asks a minute and 2 in flight; identical subscriptions share one upstream poll (at most every 15 s) and each subscribing message pays for its own tick. Envelopes over 16 KB are dropped. Answers are cut to the granted fields before the message sees them.",
        "mac_side": "queryContext accepts the local SDK shape, as narrowing only: where.kinds, filters with eq and in on shared fields, select within the shared fields. The team and layer stay the dashboard's."
    })
}

/// The toolkit section: what `cs-live.js` offers and how a page uses it.
#[cfg(feature = "dashboard-publish")]
fn hosted_toolkit() -> Value {
    use crate::dashboard_publish::create::{
        TOOLKIT, TOOLKIT_VERSION, XTERM_CSS_PATH, XTERM_JS_PATH, XTERM_VERSION,
    };
    json!({
        "what": format!("{TOOLKIT} (the helper above, version comment `// cs-live {TOOLKIT_VERSION}`) wraps the page API in plain functions on one global, CS. It is a convenience: a page that posts the messages itself works the same. create_dashboard {{template: \"workspaces\"}} or {{template: \"mission-control\"}} writes it with xterm.js {XTERM_VERSION} ({XTERM_JS_PATH}, {XTERM_CSS_PATH}) and lists them. Both templates default to view-only; add control: true to write live.control: owner (only the owner may type, after Take control in the shell). Mission control declares queries [inbox, sessions], streams [inbox, terminals, terminal], and fields.inbox [text], with agents ordered by attention, a role line and a team inbox; Workspaces groups sessions by their open workspaces. Leave pages, data and live out for either template. To use it in another dashboard, copy those three files into the folder and list them in publish.files. Load {XTERM_JS_PATH} before {TOOLKIT}."),
        "api": {
            "CS.ready()": "a promise of {role, control} once the viewer says hello; rejects no_shell after 3 s, as in the CodeSense viewer on the Mac, where no live data comes",
            "CS.role()": "viewer or owner (null before the hello)",
            "CS.query(name, args)": "one live answer (a query frame). busy/mac_busy (every Mac slot taken) and rate_limited (every relay slot taken) are asked again after 1, 2, 4, 8 and 8 s before the error is the page's",
            "CS.storyTotals(ids)": "CS.storyTotals(ids) -> Promise<{stories, reasons, pending, indexing, source, answered_at?}> for any number of story ids: asks storyTotals in parts that fit the hosted 2 KB args limit (about 120 to 150 ids), three at a time, asks again for any rest, and merges one answer (reason indexes renumbered into one reasons list; source cache and the oldest answered_at when any part came from the cache). Needs storyTotals declared and pinned like storyChanges.",
            "CS.queryAll(name, args, opts?)": "CS.queryAll(name,args,opts?) -> Promise<{rows,total,returned,complete,truncated,loading,error,canLoadMore,atCeiling,...}> for sessionUsage, queryContext, storyChanges (commits by sha; sessions metadata retained). opts override shapes: rows key path or answer=>array, id key or row=>id, cursor:{arg,next} with next key path or answer=>cursor|null, total key path or answer=>number. Undefined options keep defaults; cursor arg/next merge individually. Unknown verbs page only with opts.rows; otherwise single-answer behavior. Captures filters and effective limit (default100); sequential pages, hard maxRows5000, up to 3 silent walks from the head on stale_cursor (only the new walk replaces rows), one other stale restart and <=2 full same-limit recovery walks. Complete only when known numeric total equals distinct loaded rows and no cursor. Unknown/overshoot stay partial; zero can be complete. Latest total/totals/days replace, never sum. Explicit page asks back off5s*n up to6 retries; ordinary errors reject. Usage identity exact(cli,sessionId); machineId metadata. No snapshot guarantee. Actual outer source/answered_at/server_time metadata is preserved when provided.",
            "CS.subscribeAll(name, args, everyS, cb, opts?)": "CS.subscribeAll(name,args,everyS,cb,opts?) -> callable stop with Promise-returning stop.loadMore()/refresh(). opts.manual:true makes each loadMore fetch one next page; default auto-walks. Heads stay live through the hosted subscription at everyS (minimum15 seconds; at most20 subscriptions), retain loaded depth; full reconciliation at restEveryS60 or changed total in automatic mode. Identical cached head bodies preserve newer page rows/summary. Only a successful complete fresh traversal replaces membership. Live errors in result.error retain rows/cursor. Stop settles own lifecycle promises, clears own backoff and subscriptions; dispatched transport may finish. Server-owned head rate_limited errors are visible until next >=15s tick; explicit page backoff is separate.",
            "CS.partialList(opts)": "CS.partialList({container,render,subscribe:cb=>CS.subscribeAll(...,cb,{manual:true}),onResult?}) -> {el,loadMore(),refresh(),stop()}. Binds the real subscription handle, controls loading/unknown/zero/complete/ceiling/error/Retry, prevents double clicks and handles Promise failures. Existing-layout binding: {status:element,button:element,subscribe,onResult}; no extra row list. Optional query:()=>queryAll(...) supports a one-time list. Legacy loadMore callback remains supported. One handle per resource/scope; stop the old component before scope change or rebinding status/button. Wrap results in your resource store run(resource,scope,fn); its slot owns value/error/token/scope, the handle owns pages. Stop on pagehide/teardown.",
            "CS.act(name, args)": "a promise of the result of one declared owner action; args contain only id and a declared choice. Never retries. Rejection is an error code; a timeout can still complete, so inspect activity before retrying",
            "CS.onActions(cb)": "cb(rows, state, detail) from the actions stream (last 50 calls, with session or routine progress); returns stop()",
            "CS.subscribe(name, args, every_s, cb)": "repeated answers; returns stop()",
            "CS.viewer(cb)": "cb({role: owner|viewer, mac: online|offline|unknown|null, mac_seen_at, updated_at, stale, display}) now and on each change; display is the signed-in viewer's own email, or owner; subscribing also tells the viewer this page shows offline and stale itself (chrome: none). Returns stop()",
            "CS.endOwnerAccess()": "owner only: ends this browser's owner access on the server (the same path as the End owner access button), then the page reloads as a viewer. Call it from a click; rejects not_owner, needs_gesture, busy, failed, timeout",
            "CS.onPresence(cb)": "cb({mac, mac_seen_at}) for the owner's Mac, or cb({wid, watchers, controlling, mine}) for a watched stream; test which shape arrived before reading its fields. wid identifies the page watch. Returns stop()",
            "CS.watch(stream, args, handlers)": "one stream watch: handlers {state(s, detail), value, snapshot(vt, cols, rows), data, resized(cols, rows), presence(watchers, controlling)}; resyncs after a gap; returns {stop()}",
            "CS.passing(reason)": "true for an end or error that passes (closed, failed, busy, rate_limited): such a watch opens again by itself when the page is next shown, and watchTerminal also opens its terminal again after 15 s (then longer, up to 2 min); a page that retries a CS.watch sooner calls the old watch's stop() first, or both would open",
            "CS.fontRange": "{min: 6, max: 18, initial: 13}, the terminal text sizes watchTerminal takes",
            "CS.listAgents(cb)": "cb(rows, state, detail) from the terminals stream (detail: the end or error reason); returns stop()",
            "CS.listWorkspaces(cb)": "cb(workspaces, state, detail) from the workspaces stream; returns stop()",
            "CS.inbox(cb)": "cb(messages, state, detail) from the team inbox stream; returns stop(). Default message fields: messageId, from, fromName, summary, kind, createdAt (epoch ms), unread, inReplyTo, delivery.state when present. Text requires live.fields.inbox: [text]; to, toName, replyTo?, replyToName? and contentType (plain, markdown, html or null) require [meta]; attachments [{kind: image, alt, state, mime?, sha256?, width?, height?, bytes?}] require [media]; reactions [{emoji, count, mine}] (mine: the human reacted; never who reacted) require [reactions]. sender.face on a row requires [faces]: {sha256, mime: image/png, width, height} for a picture (pass its sha256 and the row's messageId to CS.media), {emoji} or {colour} (#rrggbb, the page draws the sender's initials on it); an agent with no face of its own is dealt one the way the in-app agents list deals it; the human has one only if they set it, and a sender whose face this Mac cannot resolve has none (show initials). Only for senders of the rows returned, never a roster. Reading never marks mail read. Query inbox takes only scope (to-human, to-agents, all, or human: only messages to or from the human, filtered on the Mac so no agent-to-agent mail leaves it), agentId, sessionId, role, limit; team is fixed by the dashboard. At most 500 messages, redacted.",
            "CS.media(messageId, sha256)": "a promise of {url, mime, bytes} for one cached image of a message in this team inbox (an attachment with state cached, from live.fields.inbox: [media]; or the picture face of that message's sender, sha256 from the row's sender.face, from live.fields.inbox: [faces]; also list the media query in live.queries). url is a data: URL (base64), usable as an img src in the frame; nothing to revoke. The Mac only reads its cache (never fetches for a viewer); an image is read in chunks of at most 180 KB raw, one at a time, and one image at a time, backing off on rate_limited (60 asks a minute). Rejects not_approved (the media query, or media and faces both missing from live.fields.inbox, is not declared; the relay's query_not_declared), not_found (not this team's message or hash, and for a face not the face of that message's sender: the same answer for every miss), not_cached (pending, blocked, failed or evicted) or too_large (over 2 MB: show a card instead). GIF, WebP, PNG and JPEG only, never SVG.",
            "CS.send(to, text, replyTo?)": "Owner only: invokes ownerMessage through CS.act. Recipient is human, role:<id>, or a running local session handle in the dashboard team. Plain text only, 1 to 2,000 characters, at most 60 sends/hour. replyTo is a positive messageId shown in this team inbox; replies follow its Reply-To within the team. No attachments, HTML format or click actions. Resolves with status and messageId; never auto-retry a send with an unknown outcome. Message bodies are never logged.",
            "CS.react(messageId, emoji, remove?)": "Owner only: invokes ownerReact through CS.act. emoji is one of 😂 ❤️ 👍 😮 🎉 🔥 and messageId a positive id of a message in this team inbox. Without remove it toggles; remove true takes the reaction back, false makes sure it is there. Resolves with {on} (the reaction is on now). Rejects not_allowed (viewer), invalid_args (other emoji), not_found (not this team's message) or rate_limited (120 an hour). Shows in CS.inbox as reactions only with live.fields.inbox: [reactions]; a reaction is not unread mail or a reply.",
            "CS.markRead(messageIds)": "Owner only: invokes ownerMarkRead through CS.act, as opening those messages in the app does. messageIds is 1 to 200 positive integer ids, each a message to the human in this team's inbox; one that is not (another team's, an agent's, a missing one) refuses the whole call with not_found and marks nothing. Resolves with {marked}, how many changed (already-read mail is left alone, so repeating a call resolves {marked: 0}). Rejects not_allowed (viewer), invalid_args, not_found or rate_limited (the relay's 10 acts a minute shared with sends and reactions, and 600 calls an hour on the Mac). The inbox stream pushes the new unread flags and humanUnread to every watcher.",
            "CS.confirmHint()": "a promise that resolves once the hint is sent, with nothing about eligibility. Asks the viewer to show its own Confirm it's you pill when this browser's signed-in account is the dashboard owner's; it never opens the sheet and shows nothing to anyone else, so call it where the page would offer owner powers and design the page to work without the pill. Rejects no_shell outside the hosted viewer",
            "CS.watchTerminal(handle, element, opts)": "an xterm.js terminal in element at the session's own size, scrolling inside the element and following new output until the viewer scrolls up, and opening itself again after a passing end (no page retry needed). Read-only, except for the owner while they control it (see control): then it takes keys, and on a phone (or with opts.keyBar true) shows a key bar with Esc, Tab, a sticky Ctrl, the arrows and a line field whose Enter sends the line and Enter. opts {fontSize} (6 to 18, default 13), {fit} (default true: fits the font to the width, capped at 18px, without resizing the PTY; spare rows show scrollback), {theme} (github-dark by default; also dracula, nord, catppuccin, solarized-dark), {keyBar} (true or false to force the bar); returns {dispose(), onState(cb), onFollow(cb), onResize(cb), onControl(cb), controlling(), onFit(cb), setFontSize(px), fontSize(), setFit(on), fitting(), jumpToLatest(), size()}; setFontSize turns fit off, setFit(true) restores it, onFit calls cb({fontSize, fit}) when either changes",
            "CS.sendInput(handle, text, opts)": "a promise that resolves once the text is sent to that terminal, or queued behind a paste still being sent (a later refusal, or control ending while it waited, comes through onControl with lost: true); opts {paste: true} sends it as a paste. Rejects not_controlling unless the owner has taken control of it with the viewer's own Take control button (a page cannot take control itself), and too_large over 16 KB or when 1 MB or 1600 messages already wait to be sent; input goes in order",
            "CS.resize(handle, cols, rows)": "a promise: asks that session for this size (1-1000 columns, 1-500 rows), under the same rules (not_controlling, too_large when the queue is full); the owner's Mac pane takes its size back when it is used",
            "CS.onControl(cb)": "cb({handle, on, reason}) when control of a terminal turns on or off (reason as in the control message: closed when the connection drops, mac_offline when the Mac goes away; or, from the toolkit itself, hidden when the page goes to the background, ended when the terminal ends or fails, unwatched on dispose), and cb({handle, on, error, lost}) when an input was refused (any input error: rate_limited, too_large, mac_offline, refused, failed, not_controlling, control_off, bad_request; too_large also when a paste or the queue would pass 1 MB; on says whether control is still on, lost that queued input was dropped), and cb({handle, on: false, error, about: 'control'}) when a take was refused, and cb({handle, on, lost: true, interrupted: true}) when the owner's own Ctrl-C, Ctrl-D, Ctrl-Z or Esc dropped what was still waiting (it is sent at once); returns stop()",
        },
        "example_paged_list": "<div id=items></div><script>const sdk=CS; const view=sdk.partialList({container:document.getElementById('items'),subscribe:cb=>sdk.subscribeAll('queryContext',{},15,cb,{manual:true}),render:row=>{const el=document.createElement('div');el.textContent=row.title||row.fileName||row.id||row.path;return el;}});window.addEventListener('pagehide',()=>view.stop(),{once:true});</script>",
        "states": "connecting, live, paused (mac_offline, reconnecting, or hidden while the page is in the background), ended (with the reason), error (with the code). An end or error with a passing reason (CS.passing) opens again when the page is next shown; any other end or error is final",
        "phone": "Watches stop while the page is hidden (visibilitychange) and resume with from_seq when it is shown, so missed output arrives without a redraw when the viewer still holds it, else as a fresh snapshot. Only the watches a page opens stream: open one terminal at a time.",
        "control": "Owner control: only the dashboard's owner, signed in to codesense.ai with a passkey or password and a second factor, on a dashboard whose dashboard.yml says `live: {streams: [terminal, ...], control: owner}`, can type into a terminal. The owner clicks Take control in the viewer's top bar (outside the page; it acts on the page's newest terminal watch), and the viewer shows a bar saying which session they control, with Release. Control ends on Release, a page change, the page going to the background, the owner's Mac going offline, 10 minutes with no typing (a resize does not count), another window or dashboard of the owner taking it, the dashboard no longer allowing control, the session ending or no longer being shared, the connection dropping, and when the owner's access ends (their owner sign-in for this dashboard ending or expiring, or the dashboard unpublished). Invited viewers never type, by any path: the viewer, codesense.ai and the Mac each refuse it. Input is at most 16 KB per message and 64 KB and 100 messages a second; a paste typed into the terminal (up to 1 MB) is sent in pieces of at most 12 KB within those limits, each a paste of its own (cut at a line break where one is near; the Mac brackets each piece that has a line break). What is typed is never logged; who controlled which session, when, and how many messages and bytes are.",
        "reserved": "CS.launch and CS.stop are names kept for later",
    })
}

/// The streams part of the hosted section, generated from
/// `STREAMS` like the queries from `CATALOG`, with the page API.
#[cfg(feature = "dashboard-publish")]
pub fn hosted_streams(streams: &[crate::dashboard_publish::catalog::StreamDef]) -> Value {
    let list: Vec<Value> = streams
        .iter()
        .map(|s| json!({"name": s.name, "kind": s.kind.as_str(), "describe": s.describe, "shares": s.shares}))
        .collect();
    json!({
        "what": "Streams push frames to the page as they happen, instead of asking again. The shell holds one WebSocket to the dashboard's own host per open dashboard; the page uses postMessage only, as for queries. A `value` stream sends the whole value on each change; a `bytes` stream sends a snapshot (one or more parts) and then ordered output. Every invited viewer can watch, up to 50 streaming connections at once across all the dashboards one Mac publishes (one per viewer session or owner sign-in per open dashboard; one more gets rate_limited).",
        "declare": "List the streams the page uses in dashboard.yml: `live: {streams: [name]}` (queries may be left out then). Only declared streams open; any other ends with not_declared. At most 16 names.",
        "available": if list.is_empty() { json!("none in this build yet: a live.streams name is refused until one exists") } else { json!(list) },
        "page_api": {
            "watch": format!("{{cs:'live', v:1, op:'watch', wid, stream, args, from_seq?}} (at most {SHELL_STREAM_MAX_WATCHES} watches per page, more get error rate_limited; wid is the page's own id, a 1-100 character string or a number; args are a plain object of at most 2 KB as JSON (larger gets error too_large); a watch for a wid already watching gets error bad_request and the open watch goes on; from_seq is optional and inclusive (the frame with that seq comes again): the shell resumes each watch itself after a reconnect. seq values are large and grow across re-opens too; use them only to order frames, and start afresh at each snapshot (see limits). Every watch request counts against the per-minute limit, refused or not)"),
            "ack": "{cs:'live', v:1, op:'ack', wid, seq} is accepted and ignored: the shell acknowledges for the page",
            "unwatch": format!("{{cs:'live', v:1, op:'unwatch', wid, pause?}}: pause: true (the page went to the background) keeps the stream open at codesense.ai for up to {LINGER_S} s, so a watch with from_seq on return gets the missed frames from its tail with no new snapshot; later, or without pause, the stream stops"),
            "resync": "{cs:'live', v:1, op:'resync', wid} after a gap: a fresh snapshot (a value stream: its current value) follows. It counts against the per-minute watch limit; past it, error rate_limited ends the watch",
            "hello": "{cs:'live', v:1, op:'hello', role:'viewer'|'owner', control}: sent by the viewer to each page when it is shown; control is true only for the owner of a dashboard whose manifest allows control (taking it still needs the viewer's Take control button)",
            "viewport": "{cs:'live', v:1, op:'viewport', covered}: from the viewer while the owner controls a terminal, how many CSS pixels at the bottom of the page a phone's on-screen keyboard covers (the toolkit's key bar stays above it); a page may ignore it",
            "input": "{cs:'live', v:1, op:'input', wid, kind:'keys'|'paste', data} or {cs:'live', v:1, op:'input', wid, kind:'resize', cols, rows}: owner only, and only for the watch under control. Refused with {op:'error', wid, error, about:'input'} (not_owner, not_controlling, control_off, too_large, rate_limited, mac_offline, refused, failed, bad_request); such an error does not end the watch",
            "control": "{cs:'live', v:1, op:'control', wid, on, reason?}: from the viewer only, when control of this watch turns on or off (reason: released, hidden, unwatched, switched, idle, replaced, reopened, access_ended, control_off, not_declared, ended, closed, mac_offline). A take the viewer sends can be refused with not_owner, control_off, mac_offline, not_ready (the terminal is still opening) or bad_request (not a terminal watch). A page cannot take control: a page that posts op control itself gets {op:'error', about:'control', source:'page', error} with not_owner for an invited viewer, control_off when the dashboard does not allow control, else bad_request",
            "watching": "{cs:'live', v:1, op:'watching', wid}: the stream is open (again, after paused)",
            "value": "{cs:'live', v:1, op:'value', wid, seq, value}",
            "snapshot": "{cs:'live', v:1, op:'snapshot', wid, seq, part, last, vt, cols?, rows?}: cols and rows come as a pair of 1..65535 or not at all; collect the parts, and replace what is shown with them joined once last is true; a part 0 starts a new snapshot and drops any parts collected so far",
            "data": "{cs:'live', v:1, op:'data', wid, seq, data}: output after the snapshot, in seq order",
            "resized": "{cs:'live', v:1, op:'resized', wid, seq, cols, rows}: a terminal's new size (terminal stream)",
            "gap": "{cs:'live', v:1, op:'gap', wid}: this viewer fell behind and frames were dropped; send resync",
            "paused": "{cs:'live', v:1, op:'paused', wid, reason:'mac_offline'|'reconnecting'}: frames resume, after watching, without any action",
            "ended": "{cs:'live', v:1, op:'ended', wid, reason:'exited'|'closed'|'not_found'|'not_declared'|'no_team'|'invalid_args'|'too_large'|'busy'|'failed'|'access_ended'|'replaced'}: the watch is over (no_team: the dashboard folder is in no team's context repo, so the agent streams have no team; closed: also sent by the shell after 8 refused or at-once-closed connections in a row, not counting a server going away or restarting; failed: the stream could not be opened or broke on the Mac, or this Mac never answered the open; access_ended: the viewer shows the request form; replaced: this viewer opened the dashboard in another window, so this window watches nothing more until it is reloaded)",
            "presence": "{cs:'live', v:1, op:'presence', wid, watchers, controlling, mine}: how many watches of this stream are open, one per page watch (a viewer watching it twice counts twice); controlling is true while the owner controls this terminal, and mine is true for the watch in control",
            "error": format!("{{cs:'live', v:1, op:'error', wid, error:'rate_limited'|'bad_request'|'too_large'}}: the watch is gone (except bad_request for a wid already watching, whose open watch goes on). rate_limited refuses this watch when the page has {SHELL_STREAM_MAX_WATCHES} watches already or the viewer made {RELAY_WATCHES_PER_MINUTE} watch and resync requests in the last minute (across reconnects; a refused resync ends its watch); only when the publishing Mac's 50 streaming connections (across its dashboards) are in use, or this viewer reconnects too often, does every open watch get it, with new watches refused for 30 s"),
        },
        "limits": "Each stream sends at most 256 KB a second and one frame per 50 ms; no data or vt part is over 64 KB; a value may be up to 256 KB as JSON and a snapshot up to 1 MB; a larger one ends the watch too_large. A viewer more than 1 MB behind gets gap. Between snapshots, seq only grows for data and value frames: a page can ignore one whose seq it has seen. Always apply a snapshot (one asked for with resync, or sent after a reconnect, may repeat a seq) and start afresh from it: the frames after it may repeat seqs seen before it.",
    })
}

/// The viewer shell's page-API limits (its SHELL_SCRIPT). Where the site's
/// source is present, a test reads the shell's source and holds these to it.
#[cfg(feature = "dashboard-publish")]
const SHELL_LIVE_MAX_IN_FLIGHT: u32 = 4;
#[cfg(feature = "dashboard-publish")]
const SHELL_LIVE_MAX_SUBS: u32 = 20;
#[cfg(feature = "dashboard-publish")]
const SHELL_LIVE_MIN_EVERY_S: u32 = 15;
#[cfg(feature = "dashboard-publish")]
const SHELL_LIVE_MAX_QUEUE: u32 = 64;
/// Watches per page, held to the shell like the limits above.
#[cfg(feature = "dashboard-publish")]
const SHELL_STREAM_MAX_WATCHES: u32 = 8;
/// The relay's LINGER_MS in seconds: how long a paused (or dropped) watch's stream is kept.
#[cfg(feature = "dashboard-publish")]
const LINGER_S: u32 = 45;
/// The relay's WATCHES_PER_MINUTE (per viewer session), for the page docs.
#[cfg(feature = "dashboard-publish")]
const RELAY_WATCHES_PER_MINUTE: u32 = 240;

/// Every error a page-API reply can carry, and when (as built in the shell; a
/// test holds the list to the shell's source).
#[cfg(feature = "dashboard-publish")]
const LIVE_ERRORS: &[(&str, &str)] = &[
    ("arg_not_allowed", "a pinned arg is missing or outside live.args, or a viewer asks storyChanges without a storyId pin, storyTotals without a storyIds pin or whyLine without a repo pin (HTTP 403). Never served from cache"),
    ("query_not_declared", "the query is not in the dashboard's live list (HTTP 403), or the Mac answered not_approved"),
    ("mac_offline", "the owner's Mac is not connected and there is no cached answer (HTTP 503)"),
    ("mac_busy", "every one of the owner's Mac's 8 query slots stayed taken for 3 s and there is no cached answer (HTTP 503). CS.query and CS.queryAll ask again after 1, 2, 4, 8 and 8 s before they reject with it"),
    ("mac_timeout", "the Mac did not answer within 10 s of starting the query and there is no cached answer (HTTP 504)"),
    ("query_invalid", "args over 2 KB or not accepted by the query: an unknown or wrong-typed arg (HTTP 422)"),
    ("stale_cursor", "a queryContext cursor whose index snapshot is gone: a walk's pages read one snapshot, kept 30 s after each page and 120 s at most, and a cursor from an earlier app run is stale too (HTTP 409). Restart paging without a cursor; CS.queryAll and CS.subscribeAll do it silently. Never served from cache"),
    ("answer_failed", "the Mac's answer failed, the relay failed, or the request did not reach codesense.ai, and there is no cached answer (HTTP 502, or anything unexpected)"),
    ("answer_too_large", "the answer is over 256 KB even as one row or one value, and there is no cached answer (HTTP 502). The list queries cut their rows to fit instead, so this is rare: narrow the query (fewer rows or a shorter window) and show \"That answer is too big for a shared dashboard; narrow the query (fewer rows or a shorter window).\""),
    ("rate_limited", "over 60 asks a minute for this viewer session, or 4 already waiting for it at codesense.ai with no cached answer (HTTP 429); in the shell: a 21st subscription on the page, or a request when 64 are already queued"),
    ("bad_request", "a malformed request: v is not 1, id is not a 1-100 character string or a number, op is not query/subscribe/unsubscribe, query is not a name, args is not a plain object, every_s is not a non-negative number, or subscribe reuses the id of an active subscription"),
    ("access_ended", "the viewer's access ended (session expired or the viewer was removed, or the dashboard was unpublished); the page's subscriptions stop and the viewer shows the request form"),
    ("post_to_own_window", "the page posted the request to parent instead of its own window; nothing was asked"),
    ("not_owner", "CS.endOwnerAccess() from a viewer: only the dashboard's owner can end owner access"),
    ("needs_gesture", "CS.endOwnerAccess() without a recent click or tap in the page; call it from a click handler"),
    ("busy", "CS.endOwnerAccess() while an earlier call is still waiting for the server"),
    ("failed", "CS.endOwnerAccess() could not reach the server; owner access is unchanged"),
];

#[cfg(feature = "dashboard-publish")]
fn live_errors() -> Value {
    Value::Object(
        LIVE_ERRORS
            .iter()
            .map(|(code, when)| (code.to_string(), Value::from(*when)))
            .collect(),
    )
}

/// How to build a dashboard that publishes to codesense.ai, the same
/// guidance as the handbook page `guides/agent-builds-dashboard`. Keep the two
/// in step.
#[cfg(feature = "dashboard-publish")]
fn hosted_guide() -> Value {
    json!({
        "start": "create_dashboard {folder, slug, title, pages?, data?, live?, template?, control?} writes a folder that already follows every rule below. Use template mission-control for an agent list and terminal, or workspaces to group by workspace; both accept control: true (default false). Then fill any data files, publish_dashboard, share_dashboard. Publishing is in beta release builds and internal builds with the dashboard-publish feature.",
        "layout": "One folder holds the dashboard: dashboard.yml, index.html (the page viewers open first), any other pages, scripts, stylesheets, images and data files, e.g. index.html, runs.html, app.js, style.css, data/summary.json. Only files listed in dashboard.yml leave the Mac; the rest stay local. Put the folder inside a team's context repo (under that team's content folder): CodeSense shows it there, and live queryContext reads that team's context. Outside a team's context repo CodeSense may not show it and every queryContext answer fails; create_dashboard warns about it.",
        "manifest": {
            "example": "slug: ai-usage\ntitle: AI usage\npublish:\n  state: draft\n  visibility: private\n  files:\n    - index.html\n    - runs.html\n    - app.js\n    - style.css\n    - data/summary.json\nlive:\n  queries: [sessions, runs, storyChanges, whyLine]\n  args:\n    storyChanges: {storyId: [DEMO-46, {epic: DEMO}]}\n    whyLine: {repo: [acme/app]}\n  fields:\n    runs: [summary]\n",
            "slug": "3 to 40 characters of a-z, 0-9 and -; the address is https://<dash host>/d/<slug>. Unique across publishers (slug taken: choose another).",
            "title": "1 to 200 characters after trimming; shown in the viewer's top bar.",
            "publish.state": "draft or published. publish_dashboard sets published, unpublish_dashboard sets draft; do not edit it by hand.",
            "publish.visibility": "private or team. team is reserved and is currently the same as private: only invited viewers get in.",
            "publish.files": "Every file the pages use, as relative paths inside the folder (no leading /, no ./ or .., no backslash, no symlink or hard link, each listed once). At most 50 files, 5 MB each, 25 MB in all. Types: html, css, js, json, csv, txt, png, jpg/jpeg, webp, gif. No SVG and no font files.",
            "publish.entry": "Not a manifest field. The hosted viewer opens index.html, else the first .html in sorted order. Always have an index.html.",
            "live": "Optional. Absent: a static dashboard. queries: [] shows only whether the owner's Mac is online; queries: [names] or all allows live answers (see declare, queries, fields and pins above)."
        },
        "rules": [
            "Load data with fetch(\"data/<name>.json\") relative to the page. In the hosted viewer the page runs in a sandbox with no network: fetch of a listed file is answered by the viewer, fetch of an unlisted file gets a 404, fetch of any web address fails. XMLHttpRequest, WebSocket, EventSource, cookies and localStorage do not work there.",
            "Reference only listed files: <script src>, <link rel=stylesheet href>, <img src> and CSS url() are inlined by the viewer when the file is listed; anything else (a CDN script, a web font, an unlisted image) is dropped. Put libraries in the folder and list them; embed fonts as data: URLs.",
            "No bridge calls. window.codesense, codesense.act and codesense.<verb>( are for pages inside the CodeSense app; publish_dashboard refuses a page that has one and names the file and line. Live data comes from the live block instead.",
            "Links between pages are relative (<a href=\"runs.html\">) and must point at a listed .html page; in-page #anchors work. A link to an unlisted file shows 'That link is not in this dashboard.'",
            "Outside links (http and https) are allowed and open in a new tab; nothing is fetched for them at publish. A link to another dashboard is an outside link to https://<dash host>/d/<slug>: the viewer needs its own invite to that dashboard, and publishing one dashboard never publishes another.",
            "Only a real click follows a link (at most 10 per 5 s). A page that navigates itself by script is rebuilt, and stopped after 3 tries.",
            "Each page load is a fresh document: keep state in the URL fragment or reload it from data files.",
            "Per page the viewer allows 8 data requests at once and 25 MB of data read in all, so read the data once on load instead of polling; the viewer reloads the page itself when a new version arrives."
        ],
        "unchecked_references": "Publish does not scan pages for references to unlisted or outside files, and it does not report linked dashboards that are unpublished. Such a reference just fails to load in the hosted viewer, so check the references yourself.",
        "fresh_data": "A dashboard is as fresh as its listed files. An agent or a routine rewrites a data file (for example a routine that calls session_usage and writes data/usage.json); there is no built-in usage source. While CodeSense is open on the owner's Mac, a change to a listed file is pushed about 60 s after the last write, and every 15 minutes the Mac checks in or pushes. Viewers see 'Page updated N minutes ago' from server time: when the files were last published (a check-in alone does not change it, nor does a live answer; a live page can show its answers' answered_at), and a stale warning when the Mac has not checked in for 30 minutes. The viewer checks for a new version every 60 s and reloads the page. Adding a file means adding it to publish.files.",
        "answer_size": "An answer is at most 256 KB as JSON. The list queries (sessionUsage, sessions, runs, inbox, queryContext) keep the first rows that fit, measured as sent (after redaction), and cut from the end of their own order: sessionUsage newest activity first, runs newest first, inbox newest first, sessions in handle order (not by recency), queryContext the sort you asked for (or its default). A limit of 500 is safe to ask. Every list answers as an object, and when the Mac had to cut rows, by the limit or by size, the object carries truncated: true, returned (the rows sent) and total (the rows before the cut), the same on every list: runs answers {runs: [...]}, sessionUsage {sessions, days, totals, total}, sessions {sessions}, inbox {messages}, queryContext {rows} (its total and truncated are always present; sessionUsage and queryContext pages always carry returned and total too). The totals and days of sessionUsage still cover everything. Test truncated and say so on the page. To read past the cut, page: sessionUsage and queryContext answer nextCursor when rows follow the last one sent (it points after that row even when the cut was by size); pass it back as cursor with the same other args to get the next page, until nextCursor is absent. A cursor from other args is refused (query_invalid). Every page carries total (all matches), returned (the rows on this page) and truncated (this page is not everything); sessionUsage total, totals and days count the current matches on every page. During a walk a session that becomes active moves to the first page; a walk is complete when its distinct sessionIds equal total, otherwise walk again from page 1 through continuation, merging by sessionId, up to 2 recovery rewalks; after that report the truthful latest N of M. queryContext's pages read one index snapshot, kept 30 s after each page and 120 s at most; past that, or after the Mac's app restarted, the cursor answers stale_cursor, so restart without a cursor. Other lists have no cursor: sessionUsage also takes a window (sinceMs, untilMs); runs takes only limit, sessions only launchedBy, and inbox has no time or cursor (narrow it with scope, agentId, sessionId or role); queryContext narrows with match, filters or where. Say on the page when it shows part of the list. Rough bytes per row: sessionUsage 600 to 850 (more with several models), sessions 350, runs 450, inbox 400 plus the text of each message when fields.inbox lists text, queryContext 150 plus about 30 per frontmatter field; the totals and days of sessionUsage and the rest of the answer are counted first. Only a single row or value over the cap fails, as answer_too_large.",
        "live_answers": "A page gets live answers only through live_page_api. Declared live queries are asked of the owner's Mac through codesense.ai while the Mac is connected (CodeSense open, a published dashboard with a live block). A person must confirm a new or wider live block once in the CodeSense app (publish_dashboard returns live: pending_confirmation until they do, and live data stays paused); a narrower or removed block takes effect within seconds. Answers are read-only and redacted, at most 256 KB, 10 s timeout, 60 asks per minute per viewer session. When the Mac is offline the last answer to the same query and args is returned with source 'cache' and its answered_at time, and the viewer shows 'Live data unavailable, showing data from N minutes ago' above the page until a live answer comes; with no cached answer the error is mac_offline. The cache is deleted on unpublish, when a query leaves the list, and for a query whose live.args pins change. Pin refusals never use the cache. Cached and in-flight answers must match the current query pin revision and the viewer permission computed on the Mac. Selector queries use a cache fallback only while the Mac is offline, never for online rate limits, timeout, failure or oversized answers. An authoritative arg_not_allowed or not_approved discards that exact cached answer and prevents pending older answers from restoring it.",
        "live_page_api": format!("Built in the viewer shell. The page posts to its own window, not to parent: postMessage({{cs:'live', v:1, op:'query', id, query, args}}, '*'); op:'subscribe' adds every_s (repeats at max(every_s, {min}) s, at most {subs} per page); op:'unsubscribe' with the id. id is a string of 1 to 100 characters or a number; args is a plain object. At most {inflight} requests per page run at once; up to {queue} more wait, and beyond that the request gets rate_limited. Replies come from parent (check e.source === parent): {{cs:'live', v:1, id, ok:true, source:'live'|'cache', answered_at, server_time, answer}} or {{cs:'live', v:1, id, ok:false, error}} (see live_errors). Presence arrives as {{cs:'live', v:1, op:'presence', mac:'online'|'offline'|'unknown', mac_seen_at}} when the page has loaded and again on each status check (every 60 s); with chrome: full the viewer also shows 'Owner's Mac: online', 'offline since <time>' or 'status unknown' in its top bar. The default, chrome: none, draws nothing: the viewer sends {{cs:'live', v:1, op:'viewer', role:'owner'|'viewer', mac, mac_seen_at, updated_at, stale, display}} on hello and on each change (CS.viewer in the toolkit; display is the signed-in viewer's own email, or owner); posting {{cs:'live', v:1, op:'viewer_sub'}} tells the viewer the page shows offline and stale itself, otherwise the viewer shows its own notice after 3 s. {{cs:'live', v:1, op:'end_owner', id}} (owner only, from a click) ends owner access on the server and the reply is {{cs:'live', v:1, id, ok}}. A new page ends the old page's subscriptions. In the CodeSense viewer on the Mac nothing answers these messages, so give the page a fallback (the create_dashboard starter times out after 15 s and says so).", min = SHELL_LIVE_MIN_EVERY_S, subs = SHELL_LIVE_MAX_SUBS, inflight = SHELL_LIVE_MAX_IN_FLIGHT, queue = SHELL_LIVE_MAX_QUEUE),
        "live_errors": live_errors(),
        "share": "share_dashboard {dashboard, add?: [emails], remove?: [emails]}: add gives access, remove takes it away and ends those viewers' sessions at once, neither just lists the viewers; every call returns {slug, viewers, notOnList}. Invite emails are reviewed before they are sent. To see the dashboard yourself, add the owner's own address.",
        "viewers": "A viewer opens the one-time link from the email (valid 15 minutes, single use) and gets a session for that dashboard only; it ends after viewers.session_days (90 by default) without a visit, and 90 days after sign-in at the latest. They see the title, 'Page updated N minutes ago' (when the files were last published), a stale warning when the Mac stopped checking in, the pages, and, for a dashboard with a live block, 'Owner's Mac: online', 'offline since <time>' or 'status unknown'. Anyone else gets the same not-found page with a form to request a link. Viewers cannot change anything. If the dashboard sets live.embeds.run: everyone, an agent's HTML messages on it run their scripts on the viewer's device; they can read only what live.embeds grants, can reveal the device's IP address and can never act. Without run (the default) viewers see static messages only (see hosted.embeds).",
        "you_can": [
            "build a small multi-page web app: several pages, shared scripts and styles, images, JSON, CSV or text data",
            "use any JavaScript library you put in the folder and list",
            "keep the data fresh by having an agent or routine rewrite the data files while CodeSense is open",
            "link to any web page or to another dashboard (opens in a new tab)",
            "show live, read-only answers from the owner's Mac for declared catalog queries (sessionUsage, sessions, runs, queryContext), with the last answer shown when the Mac is offline",
            "list the team's agents and open workspaces live and watch an agent's terminal as it runs (streams workspaces, terminals, terminal; create_dashboard {template: \"workspaces\"} writes a ready dashboard)",
            "take it down with unpublish_dashboard and publish again later"
        ],
        "you_cannot": [
            "call the CodeSense bridge (window.codesense, act) from a published page",
            "reach the network from a hosted page: no fetch to web addresses, no XMLHttpRequest, no WebSocket, no CDN scripts or web fonts",
            "load or link files that are not listed or that live outside the folder",
            "use SVG or font files, more than 50 files, a file over 5 MB or more than 25 MB in all",
            "write anything as an invited viewer (only the owner can type into terminals)",
            "type into a terminal unless you are the owner and the dashboard allows control (live.control: owner), and only after Take control in the viewer's top bar",
            "send invite emails yourself: they are reviewed before they are sent",
            "set a different entry page: the viewer opens index.html first"
        ]
    })
}

fn local_contract_base() -> Value {
    json!({
        "role": "Author local HTML dashboards using scoped local queries, the agents read model (sdk.agents_and_inbox), routines, agent runs, worktrees and machines (sdk.routines_runs_and_machines), actions and completed routine snapshots. Agents own external credentials and requests.",
        "action": action_section(), "sdk": sdk_section(), "kit": kit_section(),
        "refresh": "Read routineResult/routineStatus on page load. Only an explicit user action may request routineRefresh; page load must not launch an agent.",
        "snapshot": "codesense.snapshot captures a frozen record; never execute saved snapshot state.",
        "analyze": "invoke('analyze') uses the configured agent CLI; handle unavailable, timeout and busy errors, and render model output as text.",
        "inbox_messages": "An agent can send the human a dashboard as a message: send_message with {to:\"human\", contentType:\"html\", text:<the HTML>}. It renders in the Inbox exactly like a dashboard file — scripts, window.codesense (reads, act() through the same action gate) and pushed events — sized to its content, with Open in tab. Always pass {summary}: one plain-text line (≤280 chars) the Inbox and lists show for it, since a line derived from the markup reads badly. Team-scoped calls act in the message's team. Example text: `<p id=o>…</p><script>codesense.sessions().then(s=>{o.textContent=s.length+' sessions running'})</script>`.",
        "retired_services": "Built-in provider connections, connector polling and /api/v1/proxy are unavailable. Publish external data through a routine and read its result.",
        "story_fields": story_fields_section(),
        "see_also": ["validate_live_data_doc", "routine_result", "routine_publish_result"]
    })
}

/// Story fields a dashboard can read: the registered fields `get_story_format`
/// publishes, printed from the story-field registry.
fn story_fields_section() -> Value {
    let fields: Vec<Value> = crate::commands::pipeline_format::contract_fields()
        .map(|f| f.contract_entry())
        .collect();
    json!({
        "what": "Story fields a dashboard can read. Use the Markdown name in codesense.queryContext filters, sort, groupBy and select, and as a row's fields key; HTML stories carry the same field as the cs_meta tag. A team's own fields, declared in content/field-schema.yml or the org's tag_schema, read the same way. Rely only on these and declared fields: validate_story flags any other key with the UNREGISTERED_FIELD advisory.",
        "fields": fields
    })
}

/// A finding severity. `blocking` fails `valid`; `advisory` does not (mirrors
/// `validate_story`).
const BLOCKING: &str = "blocking";
const ADVISORY: &str = "advisory";

fn finding(code: &str, severity: &str, message: &str, hint: &str) -> Value {
    json!({ "code": code, "severity": severity, "message": message, "hint": hint })
}

/// Analysis ceiling for the heuristic lint. A real dashboard's `<script>` is a
/// few tens of KB (the shipped Jira example is ~27 KB); 512 KiB is generous
/// headroom while bounding the one full-file `to_lowercase` allocation on a
/// pathological input.
const LINT_ANALYSIS_CAP: usize = 512 * 1024;

/// Lint a candidate live-data HTML dashboard. Heuristic pattern analysis
/// (declared as such — best-effort, not a
/// JS parser). Applies to a doc that either fetches through the proxy OR uses a
/// `window.codesense` verb (`snapshot`/`analyze`) — a doc with neither is never
/// nagged (mirrors `validate_story`'s plain_doc). A file under panes/ also
/// receives the advisory liveness check; file_path names it in the warning.
///
/// Declarations use the canonical pane validator. Usage and liveness findings
/// remain heuristics over the text, with no JavaScript execution or runtime proof.
///
/// `valid` is true iff there are zero **blocking** findings. It is never
/// `true` with blocking findings present.
pub fn lint_live_data_doc(text: &str, file_path: &str, _provider_ids: &[&str]) -> Value {
    // Bound the single full-text allocation; analyze a capped, char-boundary
    // -safe prefix (heuristic — a real dashboard is well under the cap).
    let analyzed = if text.len() > LINT_ANALYSIS_CAP {
        let mut end = LINT_ANALYSIS_CAP;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    } else {
        text
    };
    let lower = analyzed.to_lowercase();
    let path = std::path::Path::new(file_path);
    let is_pane = (path.components().any(|part| part.as_os_str() == "panes")
        && path
            .extension()
            .is_some_and(|ext| ext == "html" || ext == "htm"))
        || regex::Regex::new(
            r#"(?is)<meta\b[^>]*\bname\s*=\s*["']codesense:(?:slots|capabilities|commands)["']"#,
        )
        .expect("pane declaration tag pattern")
        .is_match(analyzed);
    let is_live = lower.contains("__codesenseproxy")
        || lower.contains(PROXY_PATH_PREFIX)
        || lower.contains("connectorpolldelta")
        || lower.contains("openproviderconnect")
        || lower.contains("routineresult(");

    // Detect the verbs by their CALL-SHAPE built from the shared
    // consts — `invoke('analyze'` / `codesense.snapshot(` — never the bare word
    // "analyze"/"snapshot" (which appears in prose on many dashboards). Both
    // anchors require the opening `(` so a prose/comment mention of the verb
    // name (no call) is not treated as usage.
    let analyze_call = lower.contains(&format!("invoke('{ANALYZE_VERB}'"))
        || lower.contains(&format!("invoke(\"{ANALYZE_VERB}\""));
    let snapshot_call = lower.contains(&format!("codesense.{SNAPSHOT_VERB}("));
    // Detect actual SDK calls in scripts as well as the older proxy/action anchors.
    // This remains a heuristic: neither execution nor runtime grants are checked here.
    let script = regex::Regex::new(r"(?is)<script\b[^>]*>(.*?)</script>")
        .expect("script pattern")
        .captures_iter(analyzed)
        .map(|capture| capture[1].to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let sdk_call = regex::Regex::new(r"(?:window\.)?codesense\s*(?:\?\.|\.)\s*[a-zA-Z][a-zA-Z0-9_]*(?:\.[a-zA-Z][a-zA-Z0-9_]*)?\s*(?:\?\.)?\(")
        .expect("SDK call pattern")
        .is_match(&script)
        || (script.contains("window.codesense")
            && regex::Regex::new(r"\.\s*(?:ready|subscribe|subscribeContext|onSlotContext)\s*\(")
                .expect("SDK alias pattern")
                .is_match(&script));
    let uses_codesense_verb = analyze_call || snapshot_call || sdk_call;

    // The action verbs, detected by CALL-SHAPE from the
    // shared consts (never the bare word, and never the `content="…"` manifest —
    // the anchors require a bridge-call prefix `act(`/`invoke(` or the direct
    // `readWorkspace(` method, none of which appear inside a meta tag). The
    // dashboard SDK (`viewer.rs`) exposes `readWorkspace` as a direct method and
    // reaches `runSkill`/`sendToSession`/`getEpicHandle` through `act`/`invoke`.
    // `getEpicHandle` is a read verb, so it is only the NEGATIVE anchor for the
    // hard-coded-target advisory.
    // Tolerate whitespace after the paren (`act( "runSkill"`) so a call can't slip
    // past the lint on spacing (prettier emits none, but hand-authored code can).
    let call_via_bridge = |verb: &str| {
        let v = verb.to_lowercase();
        for kw in ["act(", "invoke("] {
            let mut from = 0;
            while let Some(rel) = lower[from..].find(kw) {
                let after = from + rel + kw.len();
                let rest = lower[after..].trim_start();
                if rest.starts_with(&format!("'{v}")) || rest.starts_with(&format!("\"{v}")) {
                    return true;
                }
                from = after;
            }
        }
        false
    };
    let readworkspace_call = lower.contains(&format!("{}(", READ_WORKSPACE_VERB.to_lowercase()))
        || call_via_bridge(READ_WORKSPACE_VERB);
    let runskill_call = call_via_bridge(RUN_SKILL_VERB);
    let sendtosession_call = call_via_bridge(SEND_TO_SESSION_VERB);
    // The tab-management verbs (reached via `act('<verb>'`).
    let opentab_call = call_via_bridge(OPEN_TAB_VERB);
    let sendtoworkspacetab_call = call_via_bridge(SEND_TO_WORKSPACE_TAB_VERB);
    let getepichandle_call = lower.contains("getepichandle(") || call_via_bridge("getEpicHandle");
    let openworkspace_call = call_via_bridge(OPEN_WORKSPACE_VERB);
    // Whitespace-collapsed view of the doc so any spacing/newline between arg
    // tokens (Prettier emits `path: x`, hand-authored spacing varies) is tolerated
    // by the arg-shape detection below.
    let collapsed: String = lower.split_whitespace().collect();
    let setstatus_call = call_via_bridge(SET_STATUS_VERB);
    // `setStatus` takes `{path, status}`. A literal args object that names a
    // `storyId` but no `path` is the shipped-sample bug class — the call always rejects.
    let setstatus_storyid_args = setstatus_call && setstatus_args_name_storyid_not_path(&collapsed);
    let uses_routine = [
        "routineResult",
        "routineStatus",
        "routineRefresh",
        "routineCancel",
    ]
    .iter()
    .any(|verb| call_via_bridge(verb));
    let uses_action_verb = uses_routine
        || readworkspace_call
        || setstatus_call
        || runskill_call
        || sendtosession_call
        || openworkspace_call
        || opentab_call
        || sendtoworkspacetab_call;

    // The applicability gate. `snapshot`/`analyze` are proxy
    // -INDEPENDENT, so a doc using them with no proxy fetch must still be linted
    // — otherwise the verb checks below would be dead for exactly the docs they
    // target. A doc with NEITHER proxy NOR a verb is still a plain doc.
    if !is_pane && !is_live && !uses_codesense_verb && !uses_action_verb {
        return json!({
            "detectedKind": "not_live_data",
            "method": "heuristic pattern analysis",
            "checked": {
                "declarations": [],
                "usage": "applicability heuristic",
                "analyzedBytes": analyzed.len(),
                "truncated": analyzed.len() < text.len(),
                "runtime": false,
            },
            "valid": true,
            "findings": [],
            "note": "Not a live-data doc and uses no window.codesense verb (no proxy fetch, no snapshot()/analyze(), no action verb). Nothing to lint — a plain doc is never nagged.",
            "see_also": ["get_live_data_contract"]
        });
    }

    // `live_data_doc` when the proxy handle/fetch is present; `codesense_doc`
    // when the doc only uses a verb (snapshot/analyze) with no live fetch.
    let detected_kind = if is_pane {
        "pane_doc"
    } else if is_live {
        "live_data_doc"
    } else {
        "codesense_doc"
    };
    let does_proxy_fetch = lower.contains(PROXY_PATH_PREFIX);
    let mut findings: Vec<Value> = Vec::new();
    let declaration = crate::panes::DashboardDeclaration::from_html(analyzed);
    if let Err(message) = declaration.validate() {
        findings.push(finding(
            "PANE_DECLARATION", BLOCKING, &message,
            "Correct codesense:slots, codesense:capabilities and codesense:commands using the generated pane declaration contract.",
        ));
    }
    if is_pane
        && !regex::Regex::new(r"\b(?:ready|subscribe)\s*(?:\?\.)?\s*\(")
            .expect("liveness call pattern")
            .is_match(&lower)
    {
        let file = if file_path.is_empty() {
            "Inline pane"
        } else {
            file_path
        };
        findings.push(finding("PANE_NOT_LIVE", ADVISORY,
            &format!("{file}: this pane has no explicit ready() or subscribe() call and falls back after 3 s."),
            "Add `codesense.ready()` after your page renders (or call `codesense.subscribe(...)`)."));
    }

    // BLOCKING — a hardcoded credential must never live in the doc. Covers the
    // registered providers' token shapes (GitHub `ghp_`/`github_pat_`, Atlassian
    // `ATATT3…` API tokens) — not just GitHub, since a BYO-token (Jira) doc is
    // exactly where a pasted raw token would land. The Atlassian anchor is
    // `atatt3` (the token's stable prefix) NOT bare `atatt`, so common camelCase
    // like `formatAttribute`/`dataAttr` isn't a false blocking finding.
    if lower.contains("ghp_") || lower.contains("github_pat_") || lower.contains("atatt3") {
        findings.push(finding(
            "HARDCODED_TOKEN",
            BLOCKING,
            "The doc appears to contain a hardcoded credential (a known provider token prefix).",
            "Remove it. Credentials belong to the configured agent, never the dashboard.",
        ));
    }
    // BLOCKING — the doc must not set an Authorization header itself (the proxy
    // does, server-side). Require BOTH an `authorization` key AND a literal
    // `Bearer `/`Basic ` scheme value, so a prose/comment mention of the word
    // "Authorization" (e.g. "never set an Authorization header") does not
    // hard-fail a correct doc — only an actual embedded credential does.
    let has_auth_key = lower.contains("\"authorization\"") || lower.contains("'authorization'");
    let has_scheme_literal = lower.contains("bearer ") || lower.contains("basic ");
    if has_auth_key && has_scheme_literal {
        findings.push(finding(
            "EMBEDDED_AUTH_HEADER",
            BLOCKING,
            "The doc sets an Authorization header to a literal credential.",
            "Use routine snapshots; keep external credentials in the configured agent.",
        ));
    }
    if does_proxy_fetch
        || lower.contains("connectorpolldelta(")
        || lower.contains("openproviderconnect")
    {
        findings.push(finding("RETIRED_SERVICE_ACCESS", BLOCKING,
            "This dashboard uses retired built-in service access.",
            "Use an agent-owned routine to publish data and read it with routineResult; refresh only on an explicit user action."));
    }
    // ADVISORY — unsafe rendering of unsafe input. Covers BOTH upstream
    // provider data AND model (`analyze`) output (one shared detector, one
    // code, so the two can't drift). Matches actual
    // property access / calls (`.innerHTML`, `document.write(`) via
    // `detects_unsafe_render` — NOT a prose mention of the word (so a doc that
    // documents "never use innerHTML" in a comment isn't falsely flagged).
    if detects_unsafe_render(&lower) {
        findings.push(finding(
            "UNSAFE_INNERHTML",
            ADVISORY,
            "Upstream provider data or analyze model output may be rendered via innerHTML/outerHTML/document.write.",
            "Treat all upstream AND model output as unsafe input — render with textContent / createElement.",
        ));
    }
    // ── Usage checks for the snapshot/analyze verbs (all ADVISORY,
    // all keyed on the CALL-SHAPE detected above, never a prose word). ────────

    // ADVISORY — an `analyze` call with NO error handling at all. A doc that
    // never handles the rejection will blank / raw-error when the agent CLI is
    // absent. Whole-doc (like SINGLE_401_CODE), conservative: the degrade signal
    // is either a `catch` (try/catch or `.catch(`) OR a `startsWith` — the two
    // idioms the contract teaches (`try/catch` and a two-arg
    // `.then(ok, err => err.message.startsWith(prefix))`). A doc that handles
    // errors either way is not nagged.
    //
    // The bare words `timeout`/`busy`/`unavailable` are not degrade evidence:
    // `setTimeout` contains "timeout", so the check would almost never fire.
    // `startswith` covers the valid `.then(ok, err)` form and has no common
    // collision.
    if analyze_call && !(lower.contains("catch") || lower.contains("startswith")) {
        findings.push(finding(
            "ANALYZE_NO_DEGRADE",
            ADVISORY,
            "An analyze() call has no error handling — no catch or startsWith branch for the rejection.",
            "Wrap the call in try/catch (or a two-arg .then) and branch on err.message.startsWith('unavailable'|'timeout'|'busy') — the CLI may be absent, slow, or busy.",
        ));
    }
    // ADVISORY (low-confidence) — a streaming expectation around analyze. The
    // result is a single completed string, not a token stream. Anchored on
    // STRONG, streaming-specific signals only — never the bare word "stream" and
    // never the generic `.stream(` which false-positives on any
    // unrelated stream (e.g. a ReadableStream body reader).
    if analyze_call
        && (lower.contains("text/event-stream")
            || lower.contains("readablestream")
            || lower.contains("ontoken"))
    {
        findings.push(finding(
            "ANALYZE_EXPECTS_STREAMING",
            ADVISORY,
            "The doc appears to expect a token stream around analyze(). analyze() returns a single completed string — it does not stream.",
            "Render an 'Analyzing…' placeholder, then swap in the completed result via textContent. Drop any streaming/token-by-token handling.",
        ));
    }
    // ADVISORY — snapshot state re-hydrated by EXECUTING it. The inert
    // `type=application/json` blob is written by codesense at serialize time, so
    // the only author-controlled inertness risk is reading that state back and
    // running it as code (eval / new Function) instead of JSON.parse + safe DOM.
    if lower.contains("codesense-snapshot-state")
        && (lower.contains("eval(") || lower.contains("new function("))
    {
        findings.push(finding(
            "SNAPSHOT_STATE_EXECUTED",
            ADVISORY,
            "Embedded snapshot state appears to be executed (eval / new Function) instead of parsed as inert data.",
            "Re-hydrate snapshot state with JSON.parse(el.textContent) and render via safe DOM — never eval / new Function / innerHTML.",
        ));
    }

    if setstatus_storyid_args {
        findings.push(finding(
            "SETSTATUS_STORYID_ARG",
            BLOCKING,
            "setStatus is called with a `storyId` and no `path` — setStatus addresses the story DOC by its content path, so this call always rejects.",
            "Pass the doc's path: act('setStatus', {path: doc.path, status}). queryContext() rows carry `path`.",
        ));
    }
    // ADVISORY — a `sendToSession` that never resolves its target via
    // `getEpicHandle()` is likely pointing at a hard-coded absolute path, which
    // won't resolve on another machine or epic.
    if sendtosession_call && !getepichandle_call {
        findings.push(finding(
            "SENDTOSESSION_HARDCODED_TARGET",
            ADVISORY,
            "sendToSession is called but getEpicHandle() is never used — the target is likely a hard-coded path that won't resolve on another machine or epic.",
            "Resolve the target at runtime: sendToSession(getEpicHandle(), …). Hide the control when getEpicHandle() is null.",
        ));
    }

    let valid = !findings.iter().any(|f| f["severity"] == BLOCKING);
    json!({
        "detectedKind": detected_kind,
        "method": "declaration validation and heuristic pattern analysis",
        "checked": {
            "declarations": ["codesense:slots", "codesense:capabilities", "codesense:commands"],
            "usage": "bounded heuristic pattern analysis",
            "analyzedBytes": analyzed.len(),
            "truncated": analyzed.len() < text.len(),
            "runtime": false,
        },
        "valid": valid,
        "findings": findings,
        "see_also": ["get_live_data_contract"]
    })
}

/// Does any literal `act('setStatus', {…})` args object (whitespace-collapsed,
/// lowercased doc) name `storyid:` without a `path` key? Heuristic like the sibling
/// arg-shape detections: an args object passed by variable is not inspected.
fn setstatus_args_name_storyid_not_path(collapsed: &str) -> bool {
    let verb = SET_STATUS_VERB.to_lowercase();
    let mut rest = collapsed;
    while let Some(idx) = rest.find(&verb) {
        let after = &rest[idx + verb.len()..];
        // Only a bridge call `…setstatus",{…}` / `…setstatus',{…}` carries a literal object.
        let Some(obj) = after
            .strip_prefix("\",{")
            .or_else(|| after.strip_prefix("',{"))
            .or_else(|| after.strip_prefix("`,{"))
        else {
            rest = after;
            continue;
        };
        let args = &obj[..obj.find('}').unwrap_or(obj.len())];
        // Top-level keys, quotes stripped: `path`, `path:x`, `"path":x`. A spread may carry
        // `path`, so it is given the benefit of the doubt.
        let keys: Vec<&str> = args
            .split(',')
            .map(|entry| {
                let key = entry.split(':').next().unwrap_or("");
                key.trim_matches(|c| c == '"' || c == '\'' || c == '`')
            })
            .collect();
        let names_path = keys.iter().any(|k| *k == "path" || k.starts_with("..."));
        if keys.contains(&"storyid") && !names_path {
            return true;
        }
        rest = after;
    }
    false
}

/// The single unsafe-render detector, shared by the
/// `UNSAFE_INNERHTML` finding for BOTH upstream provider data and `analyze`
/// model output. Matches the actual sink call-shapes (`.innerHTML`,
/// `.outerHTML`, `document.write(`) — never a prose mention of the word.
fn detects_unsafe_render(lower: &str) -> bool {
    lower.contains(".innerhtml")
        || lower.contains(".outerhtml")
        || lower.contains("document.write(")
}

#[cfg(test)]
mod tests {
    #[test]
    fn golden_pane_and_invalid_declarations_are_checked() {
        let html = include_str!("../../../codesense-docs/getting-started/content/examples/dashboards/live-list-with-command.html");
        let result = lint_live_data_doc(html, "panes/live-list-with-command.html", &[]);
        assert_eq!(result["detectedKind"], "pane_doc");
        assert_eq!(result["valid"], true, "{result}");
        assert_eq!(result["checked"]["runtime"], false);
        for bad in [
            html.replace("right-pane dock", "nonexistent"),
            html.replace("sessions:read commands:run", "made-up:read"),
            html.replace("slot-focused", "not-a-scope"),
            html.replace("CmdOrCtrl+Alt+L", "L"),
        ] {
            let result = lint_live_data_doc(&bad, "panes/golden.html", &[]);
            assert_eq!(result["valid"], false, "{result}");
            assert!(has_code(&result, "PANE_DECLARATION"));
        }
        let sdk_only = lint_live_data_doc("<script>const cs = window.codesense; cs.subscribe('sessions', {}); cs.ready();</script>", "live.html", &[]);
        assert_eq!(sdk_only["detectedKind"], "codesense_doc");
        assert_eq!(
            lint_live_data_doc("<p>A plain document.</p>", "plain.html", &[])["detectedKind"],
            "not_live_data"
        );
    }

    #[test]
    fn emit_local_contract() {
        let payload = super::local_contract_payload();
        let text = serde_json::to_string_pretty(&payload).unwrap() + "\n";
        let path = std::env::var("LIVE_DATA_CONTRACT_OUTPUT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
                    "../codesense-docs/getting-started/content/generated/live-data-contract.json",
                )
            });
        if std::env::var_os("LIVE_DATA_CONTRACT_OUTPUT").is_some() {
            std::fs::write(&path, &text).unwrap();
        }
        let mut exported: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        if !cfg!(feature = "dashboard-publish") {
            exported
                .as_object_mut()
                .expect("contract export is an object")
                .remove("hosted");
        }
        assert_eq!(
            exported, payload,
            "run node scripts/generate-live-data-contract.mjs --export"
        );
    }

    #[test]
    fn pane_without_liveness_warns_with_its_file_and_one_line_fix() {
        let r = lint_live_data_doc(
            "<title>Inbox</title><p>Rendered</p>",
            "panes/inbox.html",
            &[],
        );
        assert_eq!(r["valid"], true);
        let warning = r["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["code"] == "PANE_NOT_LIVE")
            .unwrap();
        assert!(warning["message"]
            .as_str()
            .unwrap()
            .contains("panes/inbox.html"));
        assert_eq!(
            warning["hint"],
            "Add `codesense.ready()` after your page renders (or call `codesense.subscribe(...)`)."
        );
        for call in [
            "codesense.ready()",
            "codesense.subscribe('sessions')",
            "window.codesense?.ready?.()",
        ] {
            let r = lint_live_data_doc(call, "panes/inbox.html", &[]);
            assert!(!has_code(&r, "PANE_NOT_LIVE"));
        }
        let plain = lint_live_data_doc("<p>plain doc</p>", "content/notes.html", &[]);
        assert!(!has_code(&plain, "PANE_NOT_LIVE"));
    }

    use super::*;

    /// The hosted section carries the build guide, consistent with
    /// the code it describes (limits, types, the page API target).
    #[cfg(feature = "dashboard-publish")]
    #[test]
    fn dashboard_publish_hosted_guide_matches_the_code() {
        use crate::dashboard_publish::{manifest, payload};
        let c = local_contract_payload();
        let guide = &c["hosted"]["guide"];
        let text = guide.to_string();
        for key in [
            "start",
            "layout",
            "manifest",
            "rules",
            "unchecked_references",
            "fresh_data",
            "live_answers",
            "live_page_api",
            "share",
            "viewers",
            "you_can",
            "you_cannot",
        ] {
            assert!(!guide[key].is_null(), "guide has no {key}");
        }
        assert!(text.contains("create_dashboard"));
        assert!(text.contains(&format!("At most {} files", manifest::MAX_FILES)));
        assert_eq!(payload::MAX_FILE_BYTES, 5 * 1024 * 1024);
        assert_eq!(payload::MAX_VERSION_BYTES, 25 * 1024 * 1024);
        assert!(text.contains("5 MB each, 25 MB in all"));
        for ext in [
            "html", "css", "js", "json", "csv", "txt", "png", "webp", "gif",
        ] {
            assert!(
                manifest::content_type_for(&format!("a.{ext}")).is_some(),
                "{ext}"
            );
        }
        assert!(manifest::content_type_for("a.svg").is_none() && text.contains("No SVG"));
        // As built in the shell: requests go to the page's own window.
        assert!(text.contains("posts to its own window, not to parent"));
        assert!(text.contains("team's context repo"));
        assert!(text.contains("every call returns {slug, viewers, notOnList}"));
        // The entry page is index.html; there is no entry field.
        assert!(guide["manifest"]["publish.entry"]
            .as_str()
            .unwrap()
            .starts_with("Not a manifest field. The hosted viewer opens index.html"));
        // The example manifest is one the app accepts.
        let dir = tempfile::tempdir().unwrap();
        for f in [
            "index.html",
            "runs.html",
            "app.js",
            "style.css",
            "data/summary.json",
        ] {
            let p = dir.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "{}").unwrap();
        }
        let example = guide["manifest"]["example"].as_str().unwrap();
        let m = manifest::parse(example, dir.path()).unwrap();
        assert_eq!(m.files.len(), 5);
        assert!(m.live.is_some());
    }

    /// Both authoring surfaces document the template and owner opt-in.
    #[cfg(feature = "dashboard-publish")]
    #[test]
    fn dashboard_publish_mission_control_is_documented_in_contract_and_handbook() {
        let c = local_contract_payload();
        let toolkit = &c["hosted"]["toolkit"];
        assert!(toolkit["what"]
            .as_str()
            .unwrap()
            .contains("mission-control"));
        assert!(toolkit["what"].as_str().unwrap().contains("control: true"));
        assert!(toolkit["api"]["CS.onPresence(cb)"]
            .as_str()
            .unwrap()
            .contains("watchers"));
        let guide = c["hosted"]["guide"]["start"].as_str().unwrap();
        assert!(guide.contains("template?") && guide.contains("control?"));
        // The committed snapshot, which the docs build keeps in line with the site MDX.
        const HANDBOOK: &str = include_str!(
            "../../../codesense-docs/getting-started/content/handbook/guides/agent-builds-dashboard.md"
        );
        for phrase in [
            "Live agents",
            "mission-control",
            "control: true",
            "CS.onPresence",
            "CS.launch",
            "CS.stop",
            "Take control",
        ] {
            assert!(HANDBOOK.contains(phrase), "handbook missing {phrase}");
        }
    }

    /// The page-API limits and error codes the guide states
    /// are the ones the viewer shell's script has.
    #[cfg(all(feature = "dashboard-publish", cs_site))]
    #[test]
    fn dashboard_publish_hosted_guide_matches_the_viewer_shell() {
        const SHELL: &str = include_str!("../../../site/worker/routes/dash-shell.tsx");
        let script = &SHELL[SHELL.find("export const SHELL_SCRIPT = `").unwrap()..];
        let script = &script[..script.find("})();`").unwrap()];
        let var = |name: &str| -> u32 {
            let re = regex::Regex::new(&format!(r"var {name} = (\d+);")).unwrap();
            re.captures(script)
                .unwrap_or_else(|| panic!("{name} not in the shell"))[1]
                .parse()
                .unwrap()
        };
        assert_eq!(var("LIVE_MAX_IN_FLIGHT"), SHELL_LIVE_MAX_IN_FLIGHT);
        assert_eq!(var("LIVE_MAX_SUBS"), SHELL_LIVE_MAX_SUBS);
        assert_eq!(var("LIVE_MIN_EVERY_S"), SHELL_LIVE_MIN_EVERY_S);
        assert_eq!(var("LIVE_MAX_QUEUE"), SHELL_LIVE_MAX_QUEUE);
        assert_eq!(var("MAX_IN_FLIGHT"), 8);
        assert_eq!(var("MAX_LINKS"), 10);
        assert_eq!(var("MAX_REBUILDS"), 3);
        assert_eq!(var("STREAM_MAX_WATCHES"), SHELL_STREAM_MAX_WATCHES);

        // Every stream op the shell passes to a page is documented.
        let ops = regex::Regex::new(r"var STREAM_OPS = \[([^\]]*)\]").unwrap();
        let stream_api = hosted_streams(&[])["page_api"].clone();
        let quoted_op = regex::Regex::new(r#""([a-z_]+)""#).unwrap();
        for c in quoted_op.captures_iter(&ops.captures(script).expect("STREAM_OPS")[1]) {
            assert!(
                stream_api[&c[1]].is_string(),
                "stream op {} is not documented",
                &c[1]
            );
        }
        assert!(script.contains("op: \"hello\"") && stream_api["hello"].is_string());

        // Every `ended` reason the relay passes on is documented for pages.
        const RELAY: &str = include_str!("../../../site/worker/live/relay.ts");
        let budget = regex::Regex::new(r"export const WATCHES_PER_MINUTE = (\d+);").unwrap();
        assert_eq!(
            budget.captures(RELAY).expect("WATCHES_PER_MINUTE")[1]
                .parse::<u32>()
                .unwrap(),
            RELAY_WATCHES_PER_MINUTE
        );
        let linger = regex::Regex::new(r"export const LINGER_MS = ([\d_]+);").unwrap();
        assert_eq!(
            linger.captures(RELAY).expect("LINGER_MS")[1]
                .replace('_', "")
                .parse::<u32>()
                .unwrap(),
            LINGER_S * 1000
        );
        let reasons = regex::Regex::new(r"const ENDED_REASONS = new Set\(\[([^\]]*)\]\)").unwrap();
        let documented_ended = hosted_streams(&[])["page_api"]["ended"]
            .as_str()
            .unwrap()
            .to_string();
        let names = regex::Regex::new(r#""([a-z_]+)""#).unwrap();
        for c in names.captures_iter(&reasons.captures(RELAY).expect("ENDED_REASONS")[1]) {
            assert!(
                documented_ended.contains(&format!("'{}'", &c[1])),
                "ended reason {} is not documented",
                &c[1]
            );
        }

        // Every code the shell can put in a live reply: the relay's codes it passes
        // through, and each literal error it makes itself.
        let mut shell_codes = std::collections::BTreeSet::new();
        let list = regex::Regex::new(r"var LIVE_ERRORS = \[([^\]]*)\]").unwrap();
        let quoted = regex::Regex::new(r#""([a-z_]+)""#).unwrap();
        for c in quoted.captures_iter(&list.captures(script).unwrap()[1]) {
            shell_codes.insert(c[1].to_string());
        }
        let literal = regex::Regex::new(r#"error: "([a-z_]+)""#).unwrap();
        for c in literal.captures_iter(script) {
            shell_codes.insert(c[1].to_string());
        }
        let documented: std::collections::BTreeSet<String> = LIVE_ERRORS
            .iter()
            .map(|(code, _)| code.to_string())
            .collect();
        assert_eq!(documented, shell_codes);

        let c = local_contract_payload();
        let guide = &c["hosted"]["guide"];
        let api = guide["live_page_api"].as_str().unwrap();
        assert!(api.contains(&format!(
            "max(every_s, {}) s, at most {} per page",
            var("LIVE_MIN_EVERY_S"),
            var("LIVE_MAX_SUBS")
        )));
        assert!(api.contains(&format!(
            "At most {} requests per page",
            var("LIVE_MAX_IN_FLIGHT")
        )));
        assert!(api.contains(&format!("up to {} more wait", var("LIVE_MAX_QUEUE"))));
        let page_error = c["hosted"]["page_api"]["error"].as_str().unwrap();
        for code in &documented {
            assert!(guide["live_errors"][code].is_string(), "{code}");
            assert!(
                page_error.contains(code.as_str()),
                "page_api.error lacks {code}"
            );
        }
        // The shell's own notices, quoted as the guide quotes them.
        for quote in [
            "Live data unavailable, showing data from",
            "Owner's Mac: status unknown",
            "That link is not in this dashboard.",
        ] {
            assert!(SHELL.contains(quote), "{quote}");
        }
    }

    /// The contract teaches the ACTION tier as a top-level section, and every
    /// verb just runs: no declaration, grant, trust or confirm is described.
    #[test]
    fn contract_teaches_action_tier() {
        let c = local_contract_payload();

        let action = &c["action"];
        assert!(action.is_object(), "top-level `action` section present");
        for key in ["role", "call", "verbs", "tiers", "errors", "enforcement"] {
            assert!(action.get(key).is_some(), "action.{key} present");
        }
        assert!(action.get("grant_handshake").is_none());
        assert!(c.get("trust").is_none(), "no trust section");
        assert!(action["call"]
            .as_str()
            .unwrap()
            .contains("window.codesense.act"));

        let verbs = action["verbs"].as_object().unwrap();
        for v in [
            "setStatus",
            "routeToContext",
            "draft",
            "openWorkspace",
            "runPrompt",
            "sendToSession",
            "readWorkspace",
            "runSkill",
        ] {
            assert!(verbs.contains_key(v), "action.verbs.{v} present");
            assert!(verbs[v]["tier"].is_string(), "{v} carries a tier");
            assert!(verbs[v]["purpose"].is_string(), "{v} carries a purpose");
        }

        // The runaway guards stay documented as typed errors.
        let errors = action["errors"].as_str().unwrap();
        for prefix in [
            "denied: write budget",
            "denied: send budget",
            "busy",
            "unavailable:",
        ] {
            assert!(
                errors.contains(prefix),
                "action.errors documents `{prefix}`"
            );
        }
    }

    /// The whole contract an agent reads tells the truth: no capability manifest,
    /// grant, trust gate, confirm or preview is mentioned anywhere.
    /// An agent can author a dashboard (or an HTML inbox message) over agents and
    /// inboxes from the contract alone: every read-model call, every human write and
    /// the live events are named with an example.
    #[test]
    fn contract_teaches_the_agents_read_model() {
        let c = local_contract_payload();
        let block = serde_json::to_string(&c["sdk"]["agents_and_inbox"]).unwrap();
        for call in [
            "codesense.sessions()",
            "codesense.agent({key})",
            "codesense.inbox(",
            "codesense.roles(",
            "codesense.updates(",
            "codesense.humanStatus()",
            "act('sendAsHuman'",
            "act('replyAsHuman'",
            "act('markRead'",
            "act('archive'",
            "act('setHumanStatus'",
            "onAgentsChanged",
            "onInboxChanged",
            "codesense:agent-activity-changed",
            "agent-inbox-console.html",
            "[inbox_list]",
            "[role_state]",
        ] {
            assert!(block.contains(call), "agents_and_inbox teaches `{call}`");
        }
        for verb in [
            "sendAsHuman",
            "replyAsHuman",
            "markRead",
            "archive",
            "unarchive",
            "setHumanStatus",
        ] {
            assert!(
                c["action"]["verbs"].get(verb).is_some(),
                "{verb} documented"
            );
        }
        for verb in ["agent", "inbox", "roles", "updates", "humanStatus"] {
            assert!(READ_VERBS.contains(&verb), "{verb} is a read verb");
        }
    }

    #[cfg(feature = "dashboard-publish")]
    #[test]
    fn hosted_contract_teaches_query_pins() {
        let contract = local_contract_payload();
        let pins = contract["hosted"]["pins"].as_str().unwrap();
        assert!(pins.contains("storyChanges.storyId") && pins.contains("whyLine.repo"));
        assert!(LIVE_ERRORS
            .iter()
            .any(|(code, _)| *code == "arg_not_allowed"));
    }

    /// A dashboard can split the human's mail into Needs you and Updates from
    /// the contract alone: the row's `kind`, its default, and the `kind` filter.
    #[test]
    fn contract_teaches_the_message_kind() {
        let c = local_contract_payload();
        let inbox = serde_json::to_string(&c["sdk"]["agents_and_inbox"]["inbox"]).unwrap();
        for needle in [
            "kind?",
            "kind}",
            "needs-you",
            "update",
            "replyTo naming someone other than the human, its sender or a role its sender holds",
            "before limit",
        ] {
            assert!(inbox.contains(needle), "inbox teaches `{needle}`");
        }
        let updates = serde_json::to_string(&c["sdk"]["agents_and_inbox"]["updates"]).unwrap();
        assert!(updates.contains("inbox({kind:'update'})"), "{updates}");
    }

    /// The contract teaches the terminal paste: its read, its nudge and sendPrompt's active mode.
    #[test]
    fn contract_teaches_the_terminal_paste() {
        let c = local_contract_payload();
        let paste = serde_json::to_string(&c["sdk"]["sessions"]["paste"]).unwrap();
        for call in [
            "codesense.canPasteToTerminal()",
            "codesense.onPasteTargetChanged(",
            "codesense:paste-target-changed",
            "handle: 'active'",
            "'typed' | 'no-workspace' | 'no-terminal'",
            "text is written as typed; a newline in it acts as Enter",
        ] {
            assert!(paste.contains(call), "sessions.paste teaches `{call}`");
        }
        assert!(READ_VERBS.contains(&"canPasteToTerminal"));
    }

    /// A dashboard over routines, runs, worktrees and machines can be written from the
    /// contract alone: every call names its MCP twin, and the live nudges are named.
    #[test]
    fn contract_teaches_routines_runs_worktrees_and_machines() {
        let c = local_contract_payload();
        let block = serde_json::to_string(&c["sdk"]["routines_runs_and_machines"]).unwrap();
        for call in [
            "codesense.routines()",
            "codesense.routine({key})",
            "codesense.routineRuns({key",
            "codesense.routineRun({runId})",
            "codesense.runs({limit?})",
            "codesense.run({runId",
            "codesense.runEvidence({runId, storyId?})",
            "codesense.runWorkProduct({runId})",
            "codesense.sessionUsage({sinceMs?, untilMs?, sessionId?, cwd?, story?, limit?, cursor?})",
            "[session_usage]",
            "codesense.modelRates()",
            "[model_rates]",
            "[read_run_evidence]",
            "[read_run_work_product]",
            "codesense.worktrees()",
            "codesense.machines()",
            "[routine_list data]",
            "[routine_get data]",
            "[routine_runs data]",
            "[routine_run data]",
            "[list_runs]",
            "[read_run]",
            "[list_worktrees]",
            "list_machines",
            "onRoutinesChanged",
            "onMachinesChanged",
            "onWorktreesChanged",
            "codesense:machines-changed",
            "codesense:worktrees-changed",
        ] {
            assert!(
                block.contains(call),
                "routines_runs_and_machines teaches `{call}`"
            );
        }
        for verb in [
            "routines",
            "routine",
            "routineRuns",
            "routineRun",
            "runs",
            "run",
            "runEvidence",
            "runWorkProduct",
            "worktrees",
            "machines",
            "modelRates",
        ] {
            assert!(READ_VERBS.contains(&verb), "{verb} is a read verb");
        }
        let sessions = serde_json::to_string(&c["sdk"]["sessions"]).unwrap();
        assert!(
            sessions.contains("progress?"),
            "sessions() rows carry progress"
        );
    }

    /// The setup-status example page passes the lint and calls the verb the contract teaches.
    #[test]
    fn setup_example_page_lints_clean() {
        let page = include_str!(
            "../../../codesense-docs/getting-started/content/examples/dashboards/setup-status.html"
        );
        let lint = lint_live_data_doc(page, "", &[]);
        assert_eq!(lint["valid"], true, "{lint}");
        assert!(lint["findings"].as_array().unwrap().is_empty(), "{lint}");
        assert!(page.contains("codesense.setup()"));
    }

    /// `setup` is taught by the contract: its call, its MCP twin, the capture block and the
    /// published allow-list, and the hosted `args` text names it.
    #[test]
    fn contract_teaches_the_setup_query() {
        let c = local_contract_payload();
        let row = serde_json::to_string(&c["sdk"]).unwrap();
        for needle in [
            "codesense.setup()",
            "[describe_setup]",
            "capture {lastSweepAt",
            "lastError (the latest unresolved fixed phase message",
            "live: {queries: [setup]}",
            "never the binary path, warnings or status message text",
        ] {
            assert!(row.contains(needle), "sdk teaches `{needle}`");
        }
        assert!(READ_VERBS.contains(&"setup"));
        // The hosted `args` text lives in `hosted_section`, compiled only with dashboard-publish.
        #[cfg(feature = "dashboard-publish")]
        assert!(serde_json::to_string(&c["hosted"])
            .unwrap()
            .contains("setup {} (no args"));
    }

    /// The kit is documented well enough to match a built-in pane from the contract
    /// alone: its tokens, classes, every helper and a DAG example.
    #[test]
    fn contract_teaches_the_dashboard_kit() {
        let c = local_contract_payload();
        let kit = serde_json::to_string(&c["kit"]).unwrap();
        for needle in [
            "--cs-space-1",
            "--cs-state-needs-you",
            "--cs-font-mono",
            "cs-card",
            "cs-table",
            "cs-badge",
            "cs-btn",
            "cs-tabs",
            "cs-dot",
            "cs-empty",
            "cs-toolbar",
            "kit.icon(",
            "kit.avatar(",
            "kit.statusDot(",
            "kit.layoutDag(",
            "kit.dagSvg(",
            "kit.call(",
            "err.unavailable",
            "agent-dag.html",
            "pipeline-graph.html",
            "routines-console.html",
        ] {
            assert!(kit.contains(needle), "kit section teaches `{needle}`");
        }
        // Every helper the contract names exists on the served kit.
        for helper in ["dagView", "keyed", "skeleton"] {
            assert!(
                kit.contains(&format!("kit.{helper}(")),
                "kit section teaches kit.{helper}"
            );
        }
        for helper in [
            "h",
            "icon",
            "statusDot",
            "statusBadge",
            "phaseBadge",
            "storyBadge",
            "avatar",
            "layoutDag",
            "dagSvg",
            "edgePath",
            "call",
            "has",
            "live",
            "on",
            "timeAgo",
            "empty",
            "notice",
            "iconNames",
            "dagView",
            "keyed",
            "skeleton",
        ] {
            assert!(
                crate::http::routes::dashboard_kit::KIT_JS
                    .contains(&format!("    {helper}: {helper},")),
                "kit.{helper} is served"
            );
        }
        // A centre tab is themed too.
        let theme = serde_json::to_string(&c["sdk"]["theme"]).unwrap();
        assert!(theme.contains("centre tab"));
        assert!(!theme.contains("null outside a right pane"));
    }

    /// An agent can show story overviews and keep a dashboard's own
    /// settings from the contract alone.
    #[test]
    fn contract_teaches_summary_and_prefs() {
        let c = local_contract_payload();
        let summary = serde_json::to_string(&c["sdk"]["summary"]).unwrap();
        for needle in [
            "select",
            "summary",
            "Purpose",
            "Story",
            "500",
            "null",
            "plain text",
            "16 KB",
        ] {
            assert!(summary.contains(needle), "summary teaches `{needle}`");
        }
        let prefs = serde_json::to_string(&c["sdk"]["prefs"]).unwrap();
        for needle in [
            "codesense.prefs()",
            "codesense.setPrefs(",
            "private copy",
            "localStorage",
            "Feature-detect",
        ] {
            assert!(prefs.contains(needle), "prefs teaches `{needle}`");
        }
        assert!(READ_VERBS.contains(&"prefs"));
        let set_prefs = serde_json::to_string(&c["action"]["verbs"]["setPrefs"]).unwrap();
        assert!(set_prefs.contains("denied: prefs budget"), "{set_prefs}");
        assert!(set_prefs.contains("no audit line"));
        let errors = c["action"]["errors"].as_str().unwrap();
        assert!(errors.contains("denied: prefs budget"));
    }

    /// An agent can make a dashboard follow the left nav from the
    /// contract alone — the read, the subscription, the shape, the team scope, and
    /// that revealing a file in the nav is `openFile` (no new verb).
    #[test]
    fn contract_teaches_the_nav_selection() {
        let c = local_contract_payload();
        let block = serde_json::to_string(&c["sdk"]["nav_selection"]).unwrap();
        for needle in [
            "codesense.getNavSelection()",
            "codesense.onNavSelectionChange(",
            "file",
            "drill",
            "epic",
            "relPath",
            "own team",
            "openFile",
        ] {
            assert!(block.contains(needle), "nav_selection teaches `{needle}`");
        }
        assert!(
            !READ_VERBS.contains(&"getNavSelection"),
            "the nav selection is a push, not a bridge verb"
        );
    }

    #[test]
    fn contract_documents_opt_in_policy_with_open_default() {
        let c = local_contract_payload();
        assert_eq!(c["action"]["policy"]["default"], "open");
        assert_eq!(
            c["action"]["policy"]["switches"]["confirmMessageDestructive"]["default"],
            false
        );
        assert_eq!(
            c["action"]["policy"]["switches"]["confirmTeamSendAsYou"]["default"],
            false
        );
        assert_eq!(c["action"]["policy"], crate::verbs::contract());
    }

    #[test]
    fn contract_authoring_block_is_self_sufficient_for_blind_author() {
        let c = local_contract_payload();
        let authoring = &c["action"]["authoring"];
        assert!(authoring.is_object(), "action.authoring present");

        // Scaffold: only the dashboard type meta tag.
        let scaffold = authoring["scaffold"].as_str().unwrap();
        assert!(scaffold.contains("cs:type"), "scaffold names `cs:type`");

        // getEpicHandle — the epic-addressing primitive and its null semantics.
        let geh = &authoring["get_epic_handle"];
        assert!(
            geh["call"].as_str().unwrap().contains("getEpicHandle"),
            "authoring documents getEpicHandle"
        );
        let geh_blob = serde_json::to_string(geh).unwrap().to_lowercase();
        assert!(
            geh_blob.contains("null"),
            "getEpicHandle null case documented"
        );
        assert!(
            geh_blob.contains("epic"),
            "getEpicHandle epic hosting documented"
        );

        // The working-directory / absolute-path rule (the sharpest hidden gotcha).
        let cwd = authoring["cwd_rule"].as_str().unwrap().to_lowercase();
        assert!(cwd.contains("absolute"), "cwd_rule states absolute paths");

        // Non-blocking discipline + pre-ship validate + reference examples.
        assert!(
            authoring.get("non_blocking").is_some(),
            "non_blocking rule present"
        );
        assert!(
            authoring["validate"]
                .as_str()
                .unwrap()
                .contains("validate_live_data_doc"),
            "authoring points at validate_live_data_doc"
        );
        let examples = authoring["examples"].as_str().unwrap();
        assert!(
            examples.contains("content/examples/dashboards/ — kickoff-console.html"),
            "authoring names the docs repo's reference dashboards"
        );
        // The named dashboards are real files in the docs repo source.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../codesense-docs/getting-started/content/examples/dashboards");
        for page in [
            "kickoff-console.html",
            "sprint-analysis.html",
            "agent-inbox-console.html",
        ] {
            assert!(examples.contains(page), "names {page}");
            assert!(dir.join(page).is_file(), "{page} ships in the docs repo");
        }
    }

    /// Rust half of the verb parity check: the documented verb set is emitted FROM the
    /// `ACTION_VERB_DOCS` const table (contract ⟺ table), not re-typed inline —
    /// so the cross-language tripwire (`html-bridge.test.ts`) has a single Rust
    /// source of truth to diff against `ACTION_VERBS`. Mirrors
    /// `contract_and_lint_share_verb_consts` for the read verbs.
    #[test]
    fn contract_action_verbs_come_from_the_const_table() {
        let c = local_contract_payload();
        let verbs = c["action"]["verbs"].as_object().unwrap();
        // Every table row is emitted…
        for row in ACTION_VERB_DOCS {
            assert!(
                verbs.contains_key(row.verb),
                "action.verbs missing const-table verb `{}`",
                row.verb
            );
        }
        // …and nothing else is (the section is a pure projection of the table).
        assert_eq!(
            verbs.len(),
            ACTION_VERB_DOCS.len(),
            "action.verbs must be exactly the const table (no inline extras)"
        );
    }

    #[cfg(feature = "dashboard-publish")]
    #[test]
    fn contract_teaches_embeds_run_and_its_risk() {
        let c = local_contract_payload();
        let e = c["hosted"]["embeds"].to_string();
        for needle in [
            "embeds.run says",
            "embeds: {queries",
            "none (the default",
            "owner (only the signed-in owner)",
            "everyone (every invited viewer)",
            "WebRTC",
            "IP address",
            "you vouch for your agents",
            "cs-embeds.json",
            "never read from the folder",
            "nothing in this pipe acts",
            "a viewer can never send",
            "CSEmbed.mayRun",
            "openFile rejects with unavailable",
            "must supply the --cs-* theme tokens",
            "--cs-muted-fg",
            "prefers-color-scheme:dark",
            "light text on a white bubble",
        ] {
            assert!(e.contains(needle), "hosted.embeds lacks {needle}");
        }
    }

    /// The `sdk` section is the one list both the contract and the published SDK
    /// reference come from: every allowlisted verb exactly once, with its tier,
    /// plus the live SDK and query-schema versions.
    /// PR #2298: the hosted page posts to its own window; replies come from parent.
    #[cfg(feature = "dashboard-publish")]
    #[test]
    fn hosted_helper_posts_to_its_own_window() {
        assert!(!HOSTED_HELPER.contains("parent.postMessage"));
        assert!(HOSTED_HELPER.contains("window.postMessage(msg, \"*\")"));
        assert!(HOSTED_HELPER.contains("e.source !== window.parent"));
        let hosted = hosted_section(crate::dashboard_publish::catalog::CATALOG);
        assert!(!hosted["what"]
            .as_str()
            .unwrap()
            .contains("asks its shell with postMessage"));
    }

    #[cfg(feature = "dashboard-publish")]
    #[test]
    fn hosted_contract_explains_home_screen_presentation() {
        let hosted = hosted_section(crate::dashboard_publish::catalog::CATALOG);
        let install = hosted["install"].to_string();
        for advice in [
            "short_name",
            "12 characters",
            "square",
            "opaque",
            "180",
            "512",
            "theme_color",
            "offline",
        ] {
            assert!(install.contains(advice), "missing {advice}");
        }
        let chrome = hosted["chrome"].as_str().unwrap();
        assert!(chrome.contains("minimal"));
        assert!(chrome.contains("as-of time"));
        assert!(chrome.contains("owner"));
        assert!(chrome.starts_with("chrome: none (default)"));
        assert!(chrome.contains("CS.viewer") && chrome.contains("CS.endOwnerAccess"));
        assert!(chrome.contains("imitate CodeSense UI"));
        for api in ["CS.viewer(cb)", "CS.endOwnerAccess()"] {
            assert!(hosted["toolkit"]["api"][api].is_string(), "missing {api}");
        }
    }

    #[cfg(feature = "dashboard-publish")]
    #[test]
    fn contract_hosted_toolkit_documents_confirm_hint_without_eligibility() {
        let hosted = hosted_section(crate::dashboard_publish::catalog::CATALOG);
        let doc = hosted["toolkit"]["api"]["CS.confirmHint()"]
            .as_str()
            .expect("CS.confirmHint() documented");
        for needle in [
            "nothing about eligibility",
            "never opens the sheet",
            "without the pill",
        ] {
            assert!(doc.contains(needle), "CS.confirmHint() teaches `{needle}`");
        }
    }

    #[test]
    fn contract_sdk_section_lists_every_bridge_verb_and_the_schema_version() {
        let c = local_contract_payload();
        let sdk = &c["sdk"];
        assert_eq!(
            sdk["query_schema_version"],
            json!(crate::context_query::model::CONTEXT_QUERY_SCHEMA_VERSION)
        );
        assert_eq!(
            sdk["version"],
            json!(crate::http::routes::viewer::PAGE_SDK_VERSION)
        );
        assert_eq!(
            sdk["features"],
            json!(crate::http::routes::viewer::PAGE_SDK_FEATURES)
        );
        let rows = sdk["verbs"].as_array().unwrap();
        let listed: Vec<&str> = rows.iter().map(|r| r["verb"].as_str().unwrap()).collect();
        let mut expected: Vec<&str> = READ_VERBS
            .iter()
            .copied()
            .chain(ACTION_VERB_DOCS.iter().map(|d| d.verb))
            .collect();
        assert_eq!(
            listed, expected,
            "sdk.verbs is READ_VERBS then ACTION_VERB_DOCS"
        );
        expected.sort_unstable();
        expected.dedup();
        assert_eq!(expected.len(), listed.len(), "no verb listed twice");
        for row in rows {
            assert!(row.get("grant").is_none(), "no grant column");
            let verb = row["verb"].as_str().unwrap();
            let expected_tier = ACTION_VERB_DOCS
                .iter()
                .find(|d| d.verb == verb)
                .map_or("exempt", |d| d.tier);
            assert_eq!(row["tier"], json!(expected_tier), "tier for `{verb}`");
        }
    }

    #[test]
    fn lint_ignores_non_live_data_docs() {
        let r = lint_live_data_doc(
            "# Just a plain doc\n\nNo proxy here.",
            "x-codesense-proxy-token",
            &["github"],
        );
        assert_eq!(r["detectedKind"], "not_live_data");
        assert_eq!(r["valid"], true);
        assert!(r["findings"].as_array().unwrap().is_empty());
    }

    #[test]
    fn lint_flags_embedded_auth_header_blocking() {
        let html = r#"<script>fetch("/api/v1/proxy/github/user", { headers: { "Authorization": "Bearer x", "X-Codesense-Proxy-Token": p } }); setInterval(f, 1000); if(!proxy){} </script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert_eq!(r["valid"], false);
        assert!(has_code(&r, "EMBEDDED_AUTH_HEADER"));
    }

    #[test]
    fn lint_flags_hardcoded_token_blocking() {
        let html = r#"<script>const t="ghp_abc123"; fetch("/api/v1/proxy/github/user",{headers:{"X-Codesense-Proxy-Token":t}}); setInterval(f,1000); if(!proxy){}</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert_eq!(r["valid"], false);
        assert!(has_code(&r, "HARDCODED_TOKEN"));
    }

    #[test]
    fn lint_flags_atlassian_hardcoded_token_blocking() {
        // A pasted raw Atlassian API token (ATATT…) in a BYO-token doc must fail.
        let html = r#"<script>const t="ATATT3xFfGF0abc"; fetch("/api/v1/proxy/jira/rest/api/3/myself",{headers:{"X-Codesense-Proxy-Token":p}}); setInterval(f,1000); if(!proxy){}</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github", "jira"]);
        assert_eq!(r["valid"], false);
        assert!(has_code(&r, "HARDCODED_TOKEN"));
    }

    #[test]
    fn lint_does_not_flag_camelcase_containing_atatt() {
        // `formatAttribute` / `dataAttr` lowercase to contain "atatt" but are NOT
        // credentials — the `atatt3` anchor must not false-block them.
        let html =
            r#"<script>const formatAttribute = dataAttr; window.codesense.snapshot({});</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(!has_code(&r, "HARDCODED_TOKEN"));
        assert_eq!(r["valid"], true);
    }

    #[test]
    fn lint_does_not_block_on_authorization_prose() {
        // A comment mentioning "Authorization" (no literal credential) must NOT
        // be treated as an embedded auth header (no false blocking).
        let html = r#"<script>/* Authorization is described here without any credential. */ window.codesense.snapshot({});</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(!has_code(&r, "EMBEDDED_AUTH_HEADER"));
        assert_eq!(r["valid"], true);
    }

    #[test]
    fn lint_flags_unsafe_innerhtml_advisory() {
        let html = r#"<script>window.__codesenseProxy; if(!proxy){} el.innerHTML=up; setInterval(f,1000); fetch("/api/v1/proxy/github/user",{headers:{"X-Codesense-Proxy-Token":p}});</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(has_code(&r, "UNSAFE_INNERHTML"));
    }

    // ── the action-verb surface checks ───────────────────────────────────────

    /// Drift guard: every lint anchor MUST be a verb the contract documents, so a
    /// rename can't split "what we lint" from "what we teach".
    #[test]
    fn action_lint_anchors_are_documented_verbs() {
        for verb in [
            READ_WORKSPACE_VERB,
            RUN_SKILL_VERB,
            SEND_TO_SESSION_VERB,
            OPEN_TAB_VERB,
            SEND_TO_WORKSPACE_TAB_VERB,
            OPEN_WORKSPACE_VERB,
            SET_STATUS_VERB,
        ] {
            assert!(
                ACTION_VERB_DOCS.iter().any(|d| d.verb == verb),
                "lint anchor '{verb}' is not in ACTION_VERB_DOCS"
            );
        }
    }

    /// A doc that ONLY uses an action verb (no proxy, no analyze/snapshot) is
    /// still linted — the applicability gate must include the action tier.
    #[test]
    fn lint_covers_action_only_doc() {
        let html = r#"<script>const cs=window.codesense; cs.readWorkspace({path:"./"}).then(r=>{});</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert_eq!(r["detectedKind"], "codesense_doc");
    }

    /// Calling any action verb with no meta tags at all is valid: nothing needs
    /// declaring, so the lint never asks for a manifest or a grant token.
    #[test]
    fn lint_accepts_every_action_verb_without_any_declaration() {
        let html = r#"<script>
const cs = window.codesense;
cs.readWorkspace({path:"./"});
cs.act("runSkill",{skill:"sync-story",args:[id]});
cs.act('openWorkspace',{target:{kind:'epic'},headed:false});
cs.act('openWorkspace',{target:{kind:'branch',repo:'a/b',branch:'x'}});
cs.act('openWorkspace',{target:{kind:'dir',path:'/x'}});
cs.act('openWorkspace',{target:{kind:'story',storyId:'x',agent:'builtin/codex'}});
cs.act('openTab',{workspaceId:'generic:/x'});
cs.act('sendToWorkspaceTab',{sessionId:'s',mode:'command',text:'ls'});
cs.act('setStatus',{path:doc.path,status});
const h = await cs.invoke("getEpicHandle"); if (h) cs.act("sendToSession",{target:h,text:"hi"});
</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert_eq!(r["valid"], true, "{r}");
        assert!(r["findings"].as_array().unwrap().is_empty(), "{r}");
    }

    #[test]
    fn lint_flags_hardcoded_sendtosession_target_advisory() {
        // Target is a hard-coded path (no getEpicHandle()).
        let html = r#"<script>const cs=window.codesense; cs.act('sendToSession',{target:"/Users/x/epic/README.md",text:"/sync-story x"});</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(has_code(&r, "SENDTOSESSION_HARDCODED_TARGET"));
        assert_eq!(r["valid"], true);
    }

    /// The two SHIPPED action dashboards lint clean (zero findings).
    ///
    /// The dashboards are **git-excluded by design** — materialized from the
    /// context repo, never committed to this source repo. So this tripwire
    /// reads them at RUNTIME (relative to
    /// `CARGO_MANIFEST_DIR`) and no-ops for any file that isn't materialized in
    /// the current checkout (CI / a fresh clone). A compile-time `include_str!`
    /// would make the WHOLE Rust lib-test target fail to build wherever those
    /// files are absent. When a dashboard IS present locally the lint still fires:
    /// an edit that breaks the manifest⇔usage contract (or reintroduces an
    /// unsafe sink) fails HERE.
    #[test]
    fn shipped_action_dashboards_lint_clean() {
        let dashboards_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("dashboards");
        for name in ["sync-drift-board", "epic-control-console"] {
            let path = dashboards_dir.join(format!("{name}.html"));
            let Ok(html) = std::fs::read_to_string(&path) else {
                eprintln!(
                    "shipped_action_dashboards_lint_clean: `{}` not materialized in this \
                     checkout (git-excluded dashboards); skipping lint tripwire for `{name}`.",
                    path.display()
                );
                continue;
            };
            let r = lint_live_data_doc(&html, "x-codesense-proxy-token", &["github", "jira"]);
            assert_eq!(
                r["detectedKind"], "codesense_doc",
                "{name} should be recognized as a codesense doc"
            );
            let findings = r["findings"].as_array().unwrap();
            assert!(
                findings.is_empty(),
                "{name} should lint clean, got: {findings:?}"
            );
            assert_eq!(r["valid"], true, "{name} should be valid");
        }
    }

    // ── The applicability gate + verb-misuse codes ──────

    /// A verb-only doc (analyze/snapshot, NO proxy) IS linted — the whole
    /// point of broadening the gate. It reports `codesense_doc`, not
    /// `not_live_data`.
    #[test]
    fn lint_covers_verb_only_doc_with_no_proxy() {
        let html = r#"<script>document.getElementById('out').textContent='Analyzing…'; window.codesense.invoke('analyze',{prompt:'p',data:d}).then(r=>{document.getElementById('out').textContent=r}).catch(e=>{if(e.message.startsWith('unavailable')){}});</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert_eq!(r["detectedKind"], "codesense_doc");
        assert_eq!(r["valid"], true);
        // A verb-only doc must NOT be nagged for missing a proxy refresh loop.
        assert!(!has_code(&r, "NO_REFRESH_LOOP"));
    }

    /// SINGLE_401_CODE is proxy-specific — a verb-only doc (no proxy
    /// fetch) that merely contains one of the 401 literal strings is NOT nagged.
    #[test]
    fn lint_does_not_flag_401_on_verb_only_doc() {
        let html = r#"<script>window.codesense.snapshot({note:"not-authenticated"});</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert_eq!(r["detectedKind"], "codesense_doc");
        assert!(!has_code(&r, "SINGLE_401_CODE"));
    }

    /// A doc with NEITHER proxy NOR a verb is still a plain doc.
    #[test]
    fn lint_ignores_doc_with_neither_proxy_nor_verb() {
        let r = lint_live_data_doc(
            "# A report\n\nWe analyze risk and stream updates to the team.",
            "x-codesense-proxy-token",
            &["github"],
        );
        assert_eq!(r["detectedKind"], "not_live_data");
        assert!(r["findings"].as_array().unwrap().is_empty());
    }

    /// Prose mentions of the words (no invoke/verb call-shape) are NOT
    /// treated as verb usage — no false verb findings.
    #[test]
    fn lint_does_not_treat_prose_as_verb_call() {
        // `is_live` (has proxy) but only PROSE mentions of analyze/stream.
        let html = r#"<script>/* we analyze the data as a stream of events */ window.__codesenseProxy; if(!proxy){} setInterval(f,1000); fetch("/api/v1/proxy/github/user",{headers:{"X-Codesense-Proxy-Token":p}});</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(!has_code(&r, "ANALYZE_NO_DEGRADE"));
        assert!(!has_code(&r, "ANALYZE_EXPECTS_STREAMING"));
    }

    #[test]
    fn lint_flags_analyze_no_degrade_advisory() {
        let html = r#"<script>const r = await window.codesense.invoke('analyze',{prompt:'p',data:d}); el.textContent=r;</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(has_code(&r, "ANALYZE_NO_DEGRADE"));
        assert_eq!(r["valid"], true); // advisory only
    }

    #[test]
    fn lint_analyze_with_degrade_is_clean() {
        let html = r#"<script>try{const r=await window.codesense.invoke('analyze',{prompt:'p',data:d}); el.textContent=r;}catch(e){if(e.message.startsWith('unavailable')){} else if(e.message.startsWith('timeout')){} else if(e.message.startsWith('busy')){}}</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(!has_code(&r, "ANALYZE_NO_DEGRADE"));
    }

    /// `setTimeout` contains the substring "timeout" — it must NOT be
    /// mistaken for degrade handling. An undegraded analyze in a doc that also
    /// uses setTimeout is still flagged.
    #[test]
    fn lint_analyze_no_degrade_not_suppressed_by_settimeout() {
        let html = r#"<script>setTimeout(f,1000); const r = await window.codesense.invoke('analyze',{prompt:'p',data:d}); el.textContent=r;</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(has_code(&r, "ANALYZE_NO_DEGRADE"));
    }

    /// A prose/comment mention of `codesense.snapshot` with NO call
    /// paren is not treated as verb usage (call-shape parity with analyze).
    #[test]
    fn lint_does_not_treat_snapshot_prose_as_verb_call() {
        let html = "# Docs\n\nYou can freeze the page with window.codesense.snapshot any time.";
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert_eq!(r["detectedKind"], "not_live_data");
    }

    /// The two-arg `.then(onOk, onErr)` rejection form (no `catch`
    /// token) IS valid degrade handling — the contract teaches branching on
    /// `err.message.startsWith(prefix)`, so `startswith` counts as a degrade
    /// signal and the doc is NOT flagged ANALYZE_NO_DEGRADE.
    #[test]
    fn lint_analyze_degrade_via_then_two_arg_is_clean() {
        let html = r#"<script>window.codesense.invoke('analyze',{prompt:'p',data:d}).then(r=>{el.textContent=r}, e=>{if(e.message.startsWith('busy')){}});</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(!has_code(&r, "ANALYZE_NO_DEGRADE"));
    }

    #[test]
    fn lint_flags_analyze_expects_streaming_advisory() {
        let html = r#"<script>const s = window.codesense.invoke("analyze",{prompt:'p',data:d}); s.stream(onToken); el.textContent='x'; try{}catch(e){}</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(has_code(&r, "ANALYZE_EXPECTS_STREAMING"));
    }

    #[test]
    fn lint_flags_snapshot_state_executed_advisory() {
        // Reads its own snapshot state back and EXECUTES it (eval) — the real
        // author-controlled inertness risk.
        let html = r#"<script>window.codesense.snapshot({a:1}); const s = JSON.parse(document.getElementById('codesense-snapshot-state').textContent); eval(s.code);</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(has_code(&r, "SNAPSHOT_STATE_EXECUTED"));
    }

    #[test]
    fn lint_snapshot_state_parsed_safely_is_clean() {
        let html = r#"<script>window.codesense.snapshot({a:1}); const s = JSON.parse(document.getElementById('codesense-snapshot-state').textContent); el.textContent = s.a;</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(!has_code(&r, "SNAPSHOT_STATE_EXECUTED"));
    }

    /// The SHARED `UNSAFE_INNERHTML` code also covers analyze output on
    /// a verb-only doc (one detector, no parallel ANALYZE_UNSAFE_RENDER code).
    #[test]
    fn lint_flags_unsafe_render_of_analyze_output() {
        let html = r#"<script>const r = await window.codesense.invoke('analyze',{prompt:'p',data:d}).catch(e=>{if(e.message.startsWith('busy')){}}); el.innerHTML=r;</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        assert!(has_code(&r, "UNSAFE_INNERHTML"));
        // The parallel code was deliberately NOT added (DRY).
        assert!(!has_code(&r, "ANALYZE_UNSAFE_RENDER"));
    }

    /// No finding is about doc trust or grants: no finding CODE or message
    /// mentions trust, grants or a denial gated on either.
    #[test]
    fn lint_never_asserts_trust() {
        // A doc that uses both verbs, with no degrade + unsafe render, produces
        // findings — but none of them is a trust or grant assertion.
        let html = r#"<script>const r = await window.codesense.invoke('analyze',{prompt:'p',data:d}); el.innerHTML=r; window.codesense.snapshot({});</script>"#;
        let r = lint_live_data_doc(html, "x-codesense-proxy-token", &["github"]);
        for f in r["findings"].as_array().unwrap() {
            let code = f["code"].as_str().unwrap();
            assert!(!code.to_lowercase().contains("trust"), "trust code: {code}");
            let text = format!("{} {}", f["message"], f["hint"]).to_lowercase();
            for banned in ["trust", "grant", "cs:capabilities", "will be denied"] {
                assert!(!text.contains(banned), "`{banned}` in finding: {text}");
            }
        }
    }

    #[test]
    fn lint_flags_setstatus_storyid_args() {
        // `setStatus({storyId})` always rejects, because setStatus addresses the
        // story doc by `path`.
        let bad = r#"<script>
            await window.codesense.act("setStatus", { storyId: row.id, status: "review" });
        </script>"#;
        let r = lint_live_data_doc(bad, "x-codesense-proxy-token", &["github"]);
        assert_eq!(r["valid"], false);
        assert!(has_code(&r, "SETSTATUS_STORYID_ARG"));

        // A look-alike key (`filePath`) is not the `path` setStatus needs.
        let lookalike =
            r#"<script>cs.act("setStatus", { filePath: p, storyId: id, status });</script>"#;
        let r = lint_live_data_doc(lookalike, "x-codesense-proxy-token", &["github"]);
        assert!(has_code(&r, "SETSTATUS_STORYID_ARG"));

        for good in [
            r#"<script>cs.act('setStatus', { path: doc.path, status });</script>"#,
            r#"<script>cs.act("setStatus", { path: row.path, status: next, storyId: row.id });</script>"#,
            r#"<script>cs.act("setStatus", { path, status });</script>"#,
            r#"<script>cs.act("setStatus", args);</script>"#,
            r#"<script>cs.act("setStatus", { "path": p, "storyId": id, status });</script>"#,
            r#"<script>cs.act("setStatus", { ...target, storyId: id, status });</script>"#,
        ] {
            let r = lint_live_data_doc(good, "x-codesense-proxy-token", &["github"]);
            assert_eq!(r["valid"], true, "{good}: {:?}", r["findings"]);
            assert!(r["findings"].as_array().unwrap().is_empty(), "{good}");
        }
    }

    fn has_code(result: &Value, code: &str) -> bool {
        result["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == code)
    }
    /// The contract teaches that an html message to the human is a live dashboard.
    #[test]
    fn contract_teaches_html_messages_are_live_dashboards() {
        let guide = local_contract_payload()["inbox_messages"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(guide.contains("send_message"));
        assert!(guide.contains("contentType:\"html\""));
        assert!(guide.contains("codesense.sessions()"));
    }
    #[test]
    fn retained_contract_and_starter_teach_cached_routine_results() {
        let contract = local_contract_payload();
        assert!(contract.get("providers").is_none());
        assert!(contract.get("fetch").is_none());
        let verbs = contract["sdk"]["verbs"].as_array().unwrap();
        assert!(verbs.iter().any(|v| v["verb"] == "routineResult"));
        assert!(!verbs
            .iter()
            .any(|v| v["verb"] == "connectorWrite" || v["verb"] == "openProviderConnect"));
        assert_eq!(lint_live_data_doc(STARTER_HTML, "", &[])["valid"], true);
        let retired = lint_live_data_doc("fetch('/api/v1/proxy/github/repos')", "", &[]);
        assert_eq!(retired["valid"], false);
        assert!(has_code(&retired, "RETIRED_SERVICE_ACCESS"));
    }
}

#[cfg(test)]
mod spend_rate_contract_seams {
    #[test]
    fn rate_contract_describes_pending_native_acceptance_and_request_source() {
        let doc = super::ACTION_VERB_DOCS
            .iter()
            .find(|d| d.verb == "setModelRate")
            .unwrap();
        assert!(doc.purpose.contains("pending"));
        assert!(doc.purpose.contains("native app"));
        assert!(doc.purpose.contains("returns 0"));
        assert!(doc.purpose.contains("set by an agent"));
        assert!(doc.purpose.contains("set by the user"));
        assert!(!doc.purpose.contains("The rate is the user's"));
    }
}
