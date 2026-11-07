// SPDX-License-Identifier: MIT
/**
 * SW-DX-7 — the dashboard execution loop's orchestration, trusted-side.
 *
 * `openWorkspace(storyId)` (spawn) and `runPrompt({target, prompt})` (drive) are
 * the dashboard's launch/drive bridge verbs (SW-GW-11 superseded the bounded
 * `fireSkill`). This module turns them into the real "open a story's agent
 * workspace and drive a previewed prompt into it" flow, composed entirely from proven
 * reuse seams — the SW-DX-7 Rust binding/cap commands, the agent-session-launcher
 * (git-sync's precedent), and the guarded injection primitives. No new spawn
 * path, no new agent runtime.
 *
 * The orchestration is written against an injected {@link DashboardLaunchDeps}
 * so it unit-tests without Tauri / a store / a PTY (mirroring
 * `agent-session-launcher`). The real deps are wired by a controller that the
 * live `HtmlViewer` render scope registers (it owns the teamKey + pipeline
 * tree needed to resolve a `storyId` to a managed path) — the static bridge
 * allowlist handlers forward to it.
 */

import type {
  AgentAccountProvenance,
  AgentLaunchWarning,
  BranchCheckoutMode,
  LaunchBindScope,
  WorkspaceReadResult,
} from "@/tauri/commands";
import { DEFAULT_LAUNCH_AGENT, DEFAULT_UNATTENDED_AGENT } from "@/shared/lib/default-agent";
import { reviewWorkspaceHandle } from "@/shared/lib/review-workspace-handle";
import type { MachineLocation } from "@/shared/lib/workspace-location";

/** The sole `runPrompt` prompt token, replaced trusted-side (SW-GW-11 AC-6). */
export const STORY_PATH_TOKEN = "{storyPath}";

/**
 * SW-GW-11: substitute the ONE trusted-side token `{storyPath}` in a caller's
 * prompt with the story's managed absolute path (a `StoryRef`-resolved path,
 * never caller text). This is the ONLY substitution `runPrompt` performs — every
 * other character passes through verbatim, and a prompt with no token is
 * returned unchanged. Keeps path-free authoring (e.g.
 * `/bmad-code-review {storyPath}`) without a hardcoded skill enum.
 */
export function substituteStoryPath(prompt: string, storyPath: string): string {
  return prompt.split(STORY_PATH_TOKEN).join(storyPath);
}

/**
 * SW-GW-11: THE canonical parse of a story-session handle → its two roles. Split
 * on the FIRST `#`: the bare `storyId` (keys `resolveStory` — the story path is
 * per-story) and an optional `label` (the SW-GW-6 named session). Shared by the
 * bridge (which validates the label) and the drive path (which resolves it), so
 * the string the human's handle validates against is BYTE-IDENTICAL to the one
 * `resolveSession` keys off — they can never split differently. A `storyId` can
 * never contain `#` by construction (it's the pre-`#` slice), so a malformed
 * multi-`#` target surfaces its second `#` inside `label`, where the slug
 * validator rejects it.
 */
export function splitStoryHandle(target: string): { storyId: string; label: string | undefined } {
  const hashIdx = target.indexOf("#");
  return hashIdx === -1
    ? { storyId: target, label: undefined }
    : { storyId: target.slice(0, hashIdx), label: target.slice(hashIdx + 1) };
}

/**
 * SW-GW-9 (AC-8): the SINGLE "empty/whitespace ≡ absent" rule for `initialPrompt`.
 * A blank seed is treated as no seed at all (byte-identical spawn-only). Centralized
 * so the untrusted wire boundary (`html-bridge`) and the launch spine can never drift
 * on what "no prompt" means — `if (initialPrompt)` downstream is then always honest.
 */
export function normalizeInitialPrompt(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

/** A named agent to launch into a workspace's terminal: the agent (a key, or an id in the
 *  workspace's team then the caller's personal space) and the prompt the launch brings.
 *  The backend prepares everything it runs with for the session it spawns. */
export interface AgentLaunchRequest {
  /** A deliberate New terminal creates an independent session instead of adopting Start. */
  newInstance?: boolean;
  frameId?: string;
  pagePath?: string;
  agent: string;
  prompt?: string;
  /** A background session nobody watches: the agent is prepared with its headless
   *  permissions, so it never sits waiting on a permission prompt. */
  headless?: boolean;
  /** An account for this launch only, in place of the user's binding (which stays). */
  account?: { id: string; generation: number };
}

/** The session a `connect` spawned (or adopted), and what an agent launch learned when its
 *  backend prepared it. */
export interface ConnectedSession {
  sessionId: string;
  /** The agent revision the session runs (`<key>@<revision>`), recorded at bind. */
  agentRevision?: string;
  /** Warnings about the agent launch; never a refusal. */
  warnings?: AgentLaunchWarning[];
  /** The agent's own first prompt, for a launch that brought none — seeded once live. */
  defaultPrompt?: string;
  promptInArgv?: boolean;
  /** An existing tab was adopted rather than spawned: a failed launch never ends it. */
  adopted?: boolean;
}

/** Where a `storyId` resolves to (trusted-side, via the pipeline scan). */
export interface StoryResolution {
  /** The story's managed absolute file path (confined to a managed target). */
  storyFilePath: string;
  /** The team bucket the story's workspace opens under. */
  teamKey: string;
}

/** Injected seams (real defaults are wired by the controller factory). */
export interface DashboardLaunchDeps {
  /** Resolve `storyId → { managed path, teamKey }` via the pipeline scan, or
   *  `null` when it isn't a resolvable managed story (AC-11 confinement). */
  resolveStory: (storyId: string) => Promise<StoryResolution | null>;
  /** AC-8: reserve a cap slot. Rejects `busy` when saturated. Resolves `true`
   *  when THIS call freshly reserved (so only the owner aborts on failure). */
  begin: (storyId: string) => Promise<boolean>;
  /** AC-8: free a reserved-but-unbound slot on the failure path. */
  abort: (storyId: string) => Promise<void>;
  /** SW-DX-17b (AC-8): directly kill a spawned session by id, routed through the
   *  one `kill_session` choke (frees any cap slot + audits). Used to REAP an
   *  orphaned session when `bind` fails AFTER `connect` already spawned a live
   *  PTY — `abort` alone frees the reserved slot but leaves that PTY running with
   *  no registry handle (invisible + unkillable via the SW-DX-17a panel). Matters
   *  most for a headless open, whose orphan has no terminal a human might notice. */
  killSession: (sessionId: string) => Promise<void>;
  /** Create/attach the story's worktree; resolves the worktree cwd (""=nothing
   *  opened — a handled setup error / concurrent-close ghost). */
  ensureWorktree: (teamKey: string, storyFilePath: string) => Promise<string>;
  /** The story's workspace when it lives on a machine, else `null`. An agent
   *  for it starts on that machine, never in a local worktree. */
  remoteWorkspace?: (
    teamKey: string,
    storyFilePath: string,
  ) => { workspaceId: string; location: MachineLocation } | null;
  /** Start an agent tab in a workspace on its machine, at its worktree:
   *  connecting the machine first, and setting the agent up there before it
   *  starts. Resolves the tab's session; rejects with the machine's reason. */
  /** Make sure the story's workspace lives on `machineId` (`"local"`: This Mac):
   *  create it there when it has none yet (resolving once it is ready), and refuse
   *  when it already lives somewhere else. */
  placeWorkspace?: (teamKey: string, storyFilePath: string, machineId: string) => Promise<void>;
  launchRemote?: (p: {
    teamKey: string;
    workspaceId: string;
    location: MachineLocation;
    agentKey: string;
    name?: string;
    headed?: boolean;
    sessionMode?: "focus" | "new";
    initialPrompt?: string;
  }) => Promise<{ sessionId: string; fresh?: boolean }>;
  /** Wait for the story worktree's in-flight establish (worktree record + agent
   *  settings) to settle before the agent spawns — an agent reads its settings
   *  once, at startup. Optional; never rejects. */
  awaitEstablished?: (cwd: string) => Promise<void>;
  /** Wait for the team context (CLAUDE.md, AGENTS.md, skills) to land in a freshly
   *  created worktree before the agent spawns — establish only enqueues the
   *  materialize wave. Bounded; says so when the agent has to start without it.
   *  Optional; never rejects. */
  awaitContext?: (cwd: string) => Promise<void>;
  /** Launch (or focus the existing) agent session at `cwd` via the
   *  agent-session-launcher; resolves the spawned session id.
   *
   *  SW-GW-6: `session` (a validated slug) marks a NAMED session on the story —
   *  it suffixes the launcher's `(teamKey, role)` dedup key with `#<session>` so a
   *  named tab never aliases the story's MAIN session (the load-bearing hazard).
   *  `title` sets the tab's display name. Both are optional — omitting them is the
   *  story's single main session. `agent` is the agent the session runs: the backend
   *  prepares its launch for the spawned session. */
  connect: (p: {
    teamKey: string;
    cwd: string;
    storyFilePath: string;
    session?: string;
    title?: string;
    agent: AgentLaunchRequest;
  }) => Promise<ConnectedSession>;
  /** AC-12: stamp `storyId → sessionId` trusted-side (and arm status projection).
   *  Account provenance is staged trusted-side at spawn and stamped by the bind
   *  command itself — it is never supplied by the renderer. */
  bind: (storyId: string, sessionId: string, scope?: LaunchBindScope) => Promise<void>;
  /** Best-effort wait for the launched agent PROCESS to come up (liveness
   *  probe, not `aiStatus`) so a subsequent fire lands in the agent, not a shell. */
  waitLive: (sessionId: string) => Promise<boolean>;
  /** AC-14: focus the launched terminal (layout `console` + active tab). */
  focus: (teamKey: string, sessionId: string, storyFilePath: string) => void;
  /** AC-10/AC-12: resolve the story's bound session id, or `null`. */
  resolveSession: (storyId: string) => Promise<string | null>;
  resolveAccountProvenance?: (storyId: string) => Promise<AgentAccountProvenance | null>;
  /** Inject the line into the running agent (agent-aware submit). */
  injectLine: (sessionId: string, line: string) => Promise<void>;
  // ── SW-DX-10 (sendToSession) seams ────────────────────────────────────────
  /** Agent-aware submit of a free-form CHAT turn into the resolved session
   *  (settle → write → confirm → nudge). Resolves `{submitted}` — `false` ⇒ the
   *  line was written but never confirmed to land (held/unresponsive, AC-7),
   *  NOT dropped. Rejects `unavailable: session not live` when the session's PTY
   *  is gone (AC-8). Reuses the shipped `submit_pty_line` primitive — never a
   *  second raw write path. */
  sendLine: (sessionId: string, text: string) => Promise<{ submitted: boolean }>;
  /** AC-8: the resolved session sits in a TERMINAL managed state
   *  (done / needs-attention) — a chat turn into it would be silently lost, so
   *  `sendToSession` rejects rather than writing into a parked agent. */
  isSessionParked: (sessionId: string) => boolean;
  /** AC-1: locally echo the sent turn as a `"user"` bubble in the transcript,
   *  so the chat surface shows the outgoing message at once. */
  echoUserTurn: (sessionId: string, text: string) => void;
  // ── SW-GW-5 (guaranteed-live idempotent re-open) seams ─────────────────────
  /** Real child-process liveness of a bound session — a `try_wait` probe, NOT
   *  registry membership. Used by the idempotent re-open to catch a session
   *  whose PTY has exited but whose managed state hasn't flipped yet. A probe
   *  error reads as dead (heal), never a false "alive". */
  probeAlive: (sessionId: string) => Promise<boolean>;
  /** Take a superseded session out of the viewer — its tab closed, and never
   *  shown or adopted again even while a busy one keeps running until the heal
   *  ends it after the rebind — resolving once done, so a respawn's
   *  `(teamKey, role)` lookup MISSES and spawns fresh instead of re-binding it. */
  evictStaleTab: (sessionId: string) => Promise<void>;
  // ── SW-GW-10 (closeWorkspace) seams ────────────────────────────────────────
  // NB: tab-eviction REUSES the existing `evictStaleTab` seam above (SW-GW-5) — a
  // close is the same "evict + kill" shape as the heal, so no duplicate deps. Only
  // the two genuinely-new atoms are added here.
  /** AC-1/AC-3: route the handle through the ONE authoritative kill choke
   *  (`dashboard_kill_by_handle`): bound → `kill_session` (frees the cap slot +
   *  one Kill audit line); reserved-but-unbound → `abort` (clears the leaked slot);
   *  unknown → typed no-op. Called on EVERY path — incl. the null-session path — so
   *  a reserved-but-unbound slot is still cleared (a `if (sessionId)` guard would
   *  strand it). Idempotency is a property inherited from the Rust choke. */
  closeByHandle: (target: string) => Promise<void>;
}

/** SW-GW-11 `runPrompt` — a prompt into an already-live session.
 *  `target` is a story session handle (bare `storyId`, or the SW-GW-6 compound
 *  `storyId#<label>`), resolved TRUSTED-side — a raw session id never resolves.
 *  `prompt` is arbitrary caller text, sent verbatim with the single `{storyPath}` token filled trusted-side. No `profile`/spawn
 *  fields — command/model/launch live on `openWorkspace` (SW-AN-7); a live agent
 *  is inherited, never re-profiled. */
export interface RunPromptParams {
  target: string;
  prompt: string;
}

/** SW-DX-10 — a user→agent chat turn: the story/session handle the dashboard
 *  already holds, plus the free-form text. `target` is resolved trusted-side to
 *  the story's OWN bound session — a raw session id from the doc never reaches
 *  the PTY (the same anti-forgery rule `runPrompt` applies, AC-3). */
export interface SendToSessionParams {
  target: string;
  text: string;
}

/** Resolve of a chat send. `delivered:false` = the turn was written but the
 *  agent never confirmed it landed (held/unresponsive) — a distinguishable
 *  non-success outcome the caller/UI surfaces, NOT a false "sent" (AC-7). */
export interface SendToSessionResult {
  delivered: boolean;
}

/** SW-GW-10 — resolve of a `closeWorkspace`. `closed:true` = the close was carried
 *  out (or was an idempotent no-op on an already-gone handle, AC-3). */
export interface CloseWorkspaceResult {
  closed: boolean;
}

/**
 * `openWorkspace(storyId)` — resolve the story, reserve a cap slot, open its
 * worktree, launch the agent, bind the session, and
 * (when `headed`) focus the terminal. Idempotent: a second Run for an already-open
 * story focuses the existing session and does not double-spawn (AC-3). Only the
 * caller that FRESHLY reserved the slot aborts on failure (never releases a
 * live launch it doesn't own).
 *
 * SW-DX-17b — the `headed` option (default `true`): a `headed:false` open runs the
 * agent in the BACKGROUND — everything (`begin`→`connect`→`bind`) still runs so the
 * session is first-class in the registry and adoptable via the SW-DX-17a panel, but
 * the focus/console-swap step is suppressed (no terminal takes over the UI).
 */
/**
 * SW-GW-5: a bound session is RE-USABLE by an idempotent re-open iff it is NOT
 * parked (not in a terminal managed state) AND its PTY child is still alive. The
 * cheap SYNC parked-check runs first, so the common "finished run" case never
 * pays the async liveness probe; only a non-parked session is probed. Shared by
 * both launch spines (story + branch).
 */
async function isReusableSession(
  sessionId: string,
  deps: {
    isSessionParked: (id: string) => boolean;
    probeAlive: (id: string) => Promise<boolean>;
  },
): Promise<boolean> {
  if (deps.isSessionParked(sessionId)) return false; // sync store read, no IPC
  return deps.probeAlive(sessionId); // real try_wait probe
}

/**
 * SW-GW-6: humanize a session-label slug into a default tab title
 * (`"arch-review"` → `"Arch Review"`). Pure + total (empty → empty), so the
 * default-title rule (OQ-4) unit-tests without a store. The caller's explicit
 * `title` always wins over this; the user can still rename the tab inline.
 */
export function titleCaseLabel(label: string): string {
  return label
    .split("-")
    .filter((w) => w.length > 0)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

/** SW-DX-17e: the anonymous-fork label stem. A `sessionMode:'new'` launch mints
 *  `attempt-N` (N≥2 — the story's MAIN session is conceptually "attempt 1"), so a
 *  fork reads as "SW-DX-3 · Attempt 2" in the panel via {@link titleCaseLabel}. */
export const FORK_LABEL_PREFIX = "attempt";
/** Defensive upper bound on the fork-label probe. The loop terminates naturally —
 *  it can only spin while consecutive `attempt-N` handles are LIVE, and at most
 *  `cap` sessions can be live (a free label is found within ~cap probes, or `begin`
 *  rejects `busy` first). This guard just converts a pathological non-terminating
 *  case into a loud error instead of a hang. */
const MAX_FORK_PROBES = 256;

/**
 * SW-DX-17e — mint a COLLISION-FREE anonymous fork LABEL for `sessionMode:'new'`,
 * reusing `begin` itself as the atomic collision gate (no new registry state, no
 * persisted counter — resolves OQ-2). Probe `storyId#attempt-N` for ascending N:
 * the FIRST call `begin` answers `true` (freshly reserved) WINS, and its cap slot
 * is HELD for the ensuing launch (the caller passes `preReserved` to
 * {@link launchAgentSession} so the spine does not re-`begin`). A `false` means
 * that fork is already live → try N+1. `begin` REJECTS `busy` when the cap is
 * saturated (AC-4) — propagated untouched, never a silent focus-instead-of-fork.
 * Two concurrent forks can never share a handle: `begin` is atomic, so only one
 * gets `true` for a given N (AC-3). Returns the winning LABEL (the caller rebuilds
 * the `storyId#<label>` handle at its single, uniform construction site — the same
 * template `begin` was probed with, so the reserved key and the launch key match).
 */
export async function reserveForkLabel(
  storyId: string,
  begin: (handle: string) => Promise<boolean>,
): Promise<string> {
  for (let n = 2; n < 2 + MAX_FORK_PROBES; n++) {
    const label = `${FORK_LABEL_PREFIX}-${n}`;
    // `begin` throws `busy` on a saturated cap — let it propagate (AC-4).
    if (await begin(`${storyId}#${label}`)) return label;
    // `false` ⇒ this fork is already live; probe the next N.
  }
  throw new Error(`unavailable: could not mint a fresh session for "${storyId}"`);
}

/**
 * SW-GW-7 — the ONE `headed`-aware agent-session dance, shared by every workspace
 * kind (story · branch · epic-headless · dir-headless). The four spines differ ONLY
 * in how they resolve `(teamKey, cwd, handle, role)`; the reserve → idempotent-
 * re-open → connect → bind → reap → waitLive → (seed) → focus sequence — with SW-GW-5's
 * liveness gate + reservation-coherence ordering and SW-DX-17b's reap-at-bind-fail —
 * is identical, so it lives here ONCE instead of being copy-pasted per kind. The
 * caller (an adapter) pre-resolves its kind-specific context and closes it into the
 * `ensureCwd` / `connect` / `focus` thunks below. SW-GW-9: an optional (already-
 * normalized) `initialPrompt` is seeded into the live agent via `sendLine` after bind.
 */
export interface AgentSessionDeps {
  /** Reserve a cap slot for `handle`; rejects `busy` when saturated. `true` ⇒ THIS
   *  call freshly reserved (only the owner aborts on failure). */
  begin: (handle: string) => Promise<boolean>;
  /** Free a reserved-but-unbound slot on the failure path. */
  abort: (handle: string) => Promise<void>;
  /** Resolve `handle`'s bound session id, or `null` (idempotent re-open). */
  resolveSession: (handle: string) => Promise<string | null>;
  /** Optional idempotent work that must also run when a live session is reused. */
  onReuse?: (sessionId: string) => Promise<void>;
  /** This caller joined a reservation another caller owns, before it was bound. */
  onReservedReuse?: () => void;
  /** A waiting caller atomically acquired the reservation after its prior owner
   *  aborted, so it owns launch cleanup again. */
  onReservedOwnershipAcquired?: () => void;
  /** Optional operation guard checked at every safe boundary before a session is
   * committed. Throwing aborts the reservation and reaps an unbound spawn. */
  assertContinue?: () => void;
  /** The bound session sits in a TERMINAL managed state — not re-usable (SW-GW-5). */
  isSessionParked: (sessionId: string) => boolean;
  /** Real child-process liveness (`try_wait`); a probe error reads DEAD, never a
   *  false "alive" (SW-GW-5). */
  probeAlive: (sessionId: string) => Promise<boolean>;
  /** Evict a superseded session's stale role-tagged tab so a respawn's dedup misses. */
  evictStaleTab: (sessionId: string) => Promise<void>;
  /** Resolve/create the launch cwd (worktree checkout, epic-dir materialize, or a
   *  plain existing dir). `""` ⇒ nothing opened → `emptyCwdError`. The adapter closes
   *  over the resolved kind context. */
  ensureCwd: () => Promise<string>;
  /** Launch (or focus the existing) agent session at `cwd`; the adapter closes over
   *  the kind's teamKey / role / workspaceKey (+ SW-GW-6 session/title/profile). */
  connect: (cwd: string) => Promise<ConnectedSession>;
  /** Stamp `handle → sessionId` trusted-side (+ arm status projection). Account
   *  provenance is staged trusted-side at spawn and stamped by the bind command
   *  itself — it is never supplied by the renderer. `agentRevision` is the named
   *  agent the session runs, when `connect` launched one. */
  bind: (handle: string, sessionId: string, agentRevision?: string) => Promise<void>;
  /** Reap an orphaned/superseded session through the one `kill_session` choke. */
  killSession: (sessionId: string) => Promise<void>;
  /** Best-effort wait for the agent PROCESS to come up (liveness probe). */
  waitLive: (sessionId: string) => Promise<boolean>;
  /** Focus the launched terminal (layout `console` + active tab). Called ONLY when
   *  `headed`; the adapter closes over teamKey + the focus key. */
  focus: (sessionId: string) => void;
  /** SW-GW-9: seed a briefed prompt into the freshly-live agent (story adapter only —
   *  branch/epic/dir pass none). Absent ⇒ no seeding path. */
  sendLine?: (sessionId: string, text: string) => Promise<{ submitted: boolean }>;
  /** The typed reject when `ensureCwd()` resolves empty. */
  emptyCwdError: string;
}

/** Checkout + establish + process launch can each consume a 60s phase budget
 * before bind. A joiner therefore waits for their combined deadline, using
 * capped backoff so it does not hammer the Tauri bridge. */
export const RESERVATION_BIND_TIMEOUT_MS = 180_000;

/**
 * SW-GW-7: run the shared agent-session dance for `handle`. `headed:false` suppresses
 * EVERY focus (both the idempotent re-open re-focus and the post-bind console swap) —
 * the session still fully materializes (connect → bind → register), so it is a
 * first-class, adoptable background agent (SW-DX-17a) with MCP, just with
 * no visible terminal. Idempotent: a re-open of a still-live handle re-focuses (when
 * headed) without double-spawning. Only the FRESH reserver aborts on failure. SW-GW-9:
 * a normalized `opts.initialPrompt` is seeded into the freshly-live agent and the
 * `{promptDelivered}` outcome is reported honestly (undefined when no seed requested).
 */
export async function launchAgentSession(
  handle: string,
  headed: boolean,
  deps: AgentSessionDeps,
  opts?: { initialPrompt?: string; preReserved?: boolean },
): Promise<{ sessionId: string; promptDelivered?: boolean; warnings?: AgentLaunchWarning[] }> {
  // SW-GW-9: the caller (adapter) normalizes; the spine treats a present value as a
  // real seed to deliver, an absent one as "no seeding path".
  const initialPrompt = opts?.initialPrompt;

  // Reserve BEFORE any cwd/spawn work so an over-cap open rejects `busy` cheaply
  // (never spawns). `fresh` = this call owns the permit.
  //
  // SW-DX-17e: an anonymous fork (`sessionMode:'new'`) already reserved its slot
  // while minting a collision-free handle via `reserveForkLabel`'s begin-probe, so
  // it passes `preReserved` — the spine skips its own `begin` and treats THIS call
  // as the fresh owner (it aborts that reserved slot on failure below). A fork's
  // handle is freshly minted + collision-free, so `fresh` is always `true` and the
  // idempotent re-open branch is never taken — it always spawns, never focuses.
  let fresh = opts?.preReserved === true ? true : await deps.begin(handle);

  // SW-DX-17b (AC-8): the session `connect` spawned, captured the moment it returns.
  // Between `connect` and a SUCCESSFUL `bind` the PTY is LIVE but not yet in the
  // registry; if `bind` throws, `abort` frees the slot but leaves this agent orphaned
  // (running, unseen, unkillable). The failure path reaps it. Matters most for a
  // HEADLESS open, whose orphan has no terminal a human might notice.
  let spawnedSessionId: string | undefined;
  // SW-GW-5: a superseded (parked/dead) session to reap AFTER a successful rebind.
  let staleToReap: string | undefined;

  try {
    deps.assertContinue?.();
    // Idempotent re-open: an already-open handle re-focuses its session — but ONLY
    // while it is still LIVE. SW-GW-5: a parked/dead bound session is NOT handed back;
    // it is superseded by a fresh spawn+rebind (evict its tab before connect, reap its
    // PTY after bind), so the open always returns a sendable session.
    if (!fresh) {
      let existing = await deps.resolveSession(handle);
      let announcedJoin = false;
      const joinDeadline = Date.now() + RESERVATION_BIND_TIMEOUT_MS;
      // Never let two callers connect/bind against one reservation. A joiner
      // waits asynchronously for the owner to bind; if the owner aborts, begin()
      // atomically transfers the now-free reservation to this caller.
      for (let attempt = 0; !existing && !fresh; attempt += 1) {
        if (!announcedJoin) {
          deps.onReservedReuse?.();
          announcedJoin = true;
        }
        deps.assertContinue?.();
        if (Date.now() >= joinDeadline) {
          throw new Error("timeout: existing review launch did not bind");
        }
        const backoffMs = Math.min(5_000, 100 * 2 ** Math.min(attempt, 6));
        const remainingMs = joinDeadline - Date.now();
        await new Promise<void>((resolve) => setTimeout(resolve, Math.min(backoffMs, remainingMs)));
        deps.assertContinue?.();
        fresh = await deps.begin(handle);
        if (fresh) {
          deps.onReservedOwnershipAcquired?.();
          break;
        }
        existing = await deps.resolveSession(handle);
      }
      if (existing) {
        if (await isReusableSession(existing, deps)) {
          await deps.onReuse?.(existing);
          deps.assertContinue?.();
          // A headless re-open must not yank focus to the backgrounded session.
          if (headed) deps.focus(existing);
          // SW-GW-9 Gap A (AC-5): an already-live session is handed back WITHOUT
          // re-seeding (re-injecting a briefed prompt would be wrong). But a requested
          // seed must not vanish silently — report promptDelivered:false ("asked, not
          // delivered — session already live") so the caller can send explicitly.
          return initialPrompt
            ? { sessionId: existing, promptDelivered: false }
            : { sessionId: existing };
        }
        staleToReap = existing;
      }
    }

    const cwd = await deps.ensureCwd();
    deps.assertContinue?.();
    if (!cwd) {
      throw new Error(deps.emptyCwdError);
    }

    // SW-GW-5: evict the stale session IMMEDIATELY before connect so the launcher's
    // `(teamKey, role)` lookup misses it and spawns fresh. Deferred to here (not the
    // re-open branch) so a cwd failure leaves the stale session undisturbed
    // — the open self-heals on the next attempt.
    if (staleToReap) await deps.evictStaleTab(staleToReap);

    const connected = await deps.connect(cwd);
    const { sessionId } = connected;
    // An adopted tab was not spawned here, so a failed bind leaves it running.
    spawnedSessionId = connected.adopted ? undefined : sessionId;
    deps.assertContinue?.();
    await deps.bind(handle, sessionId, connected.agentRevision);
    // `bind` COMMITTED the session — first-class + killable via the SW-DX-17a panel,
    // no longer an orphan. Clear the reap handle: a best-effort waitLive/focus failure
    // must NEVER reap a live, bound, registered agent (code-review r1 MAJOR).
    spawnedSessionId = undefined;
    // SW-GW-5: post-rebind the stale id no longer maps in the registry — reap its PTY.
    // FIRE-AND-FORGET (not awaited) so the caller gets the live session WITHOUT waiting
    // on the kill IPC (non-blocking); frees no cap slot, just collapses the dead tab.
    // A hiccup is swallowed — never fails (nor delays) the already-successful open.
    if (staleToReap) {
      void deps.killSession(staleToReap).catch(() => {});
      staleToReap = undefined;
    }
    // Best-effort tail — don't fail the open (nor reap) if the probe/focus/seed
    // hiccups; the status monitor/liveness net (AC-5) flips to needs-attention if the
    // agent never came up. A headed open focuses the terminal; a headless open stays
    // backgrounded + adoptable (no layout swap, no tab steal).
    //
    // SW-GW-9: honest delivery signal — present ONLY when a seed was requested.
    // Optimistically `true`, flipped to `false` on any waitLive/sendLine failure.
    let promptDelivered: boolean | undefined = initialPrompt ? true : undefined;
    try {
      await deps.waitLive(sessionId);
      // Focus FIRST (a synchronous void store setter) so a headed open paints the
      // terminal INSTANTLY; the user then watches the seed land — no perceived delay
      // (SW-GW-9 Gap B). The seed targets sessionId directly, not the focused tab,
      // so focus order can never affect where the line lands.
      if (headed) deps.focus(sessionId);
      // SW-GW-9 (AC-1/AC-7): seed the live agent via the SHIPPED `sendLine` (SW-DX-10)
      // — the one injector, reused verbatim (no new primitive, AC-3). `waitLive` is not
      // gated on `headed`, so this seeds a headless background session too (the RF-18
      // combo). Only the story adapter wires `sendLine` + a normalized `initialPrompt`;
      // branch/epic/dir pass neither, so this is a clean no-op for them.
      //
      // KNOWN RACE (pre-existing, deferred): two CONCURRENT seeded opens of the SAME
      // handle can double-submit here (the non-`fresh` caller in the begin→bind window
      // falls through to connect [launcher dedup → same session] + rebind and runs this
      // tail against the shared session). The shipped code already double-binds/-focuses
      // in this window; the seed only extends it. A local `fresh`-gate is INCORRECT (it
      // would DROP a legitimate seed in the A-fails/B-succeeds interleaving — an AC-5
      // silent drop); the real fix is launcher-level in-flight coordination (SW-GW-5/6),
      // out of scope. Trigger is degenerate; nothing is reaped or corrupted.
      // A named agent launched with no prompt of its own starts with its default one.
      const seed = connected.promptInArgv ? undefined : (initialPrompt ?? connected.defaultPrompt);
      if (seed && deps.sendLine) {
        const { submitted } = await deps.sendLine(sessionId, seed);
        if (initialPrompt) promptDelivered = submitted; // honest: what actually landed
      }
    } catch {
      // swallow — the session is bound and running; the open still succeeds. AC-5:
      // a waitLive timeout / sendLine throw ONLY flips promptDelivered; it never
      // reaches the reap path (gated on spawnedSessionId, cleared at bind above).
      if (initialPrompt && !connected.promptInArgv) promptDelivered = false;
    }
    const warnings = connected.warnings?.length ? { warnings: connected.warnings } : {};
    return initialPrompt ? { sessionId, promptDelivered, ...warnings } : { sessionId, ...warnings };
  } catch (err) {
    // Only the fresh reserver owns the permit — a concurrent/idempotent caller must
    // NOT release a live launch it doesn't own (AC-8). Abort the COMPOUND handle so a
    // named session (SW-GW-6) frees its own permit, not the main session's.
    if (fresh) {
      await deps.abort(handle);
    }
    // SW-DX-17b / SW-GW-5: reap a session WE spawned that never bound — REGARDLESS of
    // permit ownership (a heal runs on a `!fresh` re-open yet still spawns). Evict its
    // tab too so a retry's dedup can't re-bind this dead orphan. Best-effort: a failed
    // reap must not mask the original launch error.
    if (spawnedSessionId) {
      deps.evictStaleTab(spawnedSessionId);
      try {
        await deps.killSession(spawnedSessionId);
      } catch {
        // swallow — surface the real launch failure below
      }
    }
    throw err;
  }
}

/** Create/attach the story worktree, then wait for its establish to settle so the
 *  spawn sees the worktree record and the team's materialized content. */
async function ensureStoryWorktree(
  deps: DashboardLaunchDeps,
  teamKey: string,
  storyFilePath: string,
): Promise<string> {
  const cwd = await deps.ensureWorktree(teamKey, storyFilePath);
  if (cwd && deps.awaitEstablished) await deps.awaitEstablished(cwd);
  if (cwd && deps.awaitContext) await deps.awaitContext(cwd);
  return cwd;
}

export async function openStoryWorkspace(
  storyId: string,
  deps: DashboardLaunchDeps,
  opts?: {
    headed?: boolean;
    session?: string;
    title?: string;
    initialPrompt?: string;
    /** SW-DX-17e: `"new"` forks a fresh anonymous session (auto-minted label) even
     *  when the story's main session is live; `"focus"`/absent (DEFAULT) reuses it.
     *  Ignored when an explicit `session` label is given — a NAMED session already
     *  forks via its own distinct handle/role (SW-GW-6), so it needs no auto-mint. */
    sessionMode?: "new" | "focus";
    /** An account for this one launch, in place of the user's binding for the agent. */
    account?: { userId: number; id: string; generation: number };
    /** The agent to launch (a key, or an id in the story's team then the personal space);
     *  omitted = the built-in Claude agent, or its unattended twin for a background open.
     *  Its runtime, args, permissions and account are the agent's. Always a terminal
     *  session; a background one is prepared with headless permissions. */
    agent?: string;
    /** Where a new workspace is created (see the story target's `machineId`). */
    machineId?: string;
  },
): Promise<{ sessionId: string; promptDelivered?: boolean; warnings?: AgentLaunchWarning[] }> {
  const headed = opts?.headed !== false;
  const agent = launchAgentKey(opts?.agent, headed);
  // SW-GW-9 (AC-8): normalize at the seed-decision owner so a direct caller can never
  // seed blank text — the shared rule keeps the spine and the wire boundary in lockstep.
  const initialPrompt = normalizeInitialPrompt(opts?.initialPrompt);

  // Resolve the story to a managed path FIRST — BEFORE the SW-DX-17e fork-mint probe
  // reserves any cap slot — so a resolve failure can never strand a reserved-but-
  // unbound permit (the probe's `begin` is the earliest slot-taking call on this path).
  const resolved = await deps.resolveStory(storyId);
  if (!resolved) {
    throw new Error(`unavailable: cannot resolve story "${storyId}" to a managed path`);
  }

  // An open that names where the workspace lives creates it there first (or is
  // refused when it already lives elsewhere).
  const machineId = opts?.machineId?.trim();
  if (machineId) {
    if (!deps.placeWorkspace) {
      throw new Error(`unavailable: ${storyId} can't be opened on a machine here`);
    }
    await deps.placeWorkspace(resolved.teamKey, resolved.storyFilePath, machineId);
  }

  // A story whose workspace lives on a machine starts its agent there, set up in
  // the remote worktree first. Never locally.
  const remote = deps.remoteWorkspace?.(resolved.teamKey, resolved.storyFilePath) ?? null;
  if (remote) {
    if (!deps.launchRemote) {
      throw new Error(`unavailable: ${storyId} lives on a machine; open it there`);
    }
    if (opts?.account) {
      throw new Error(
        `unavailable: ${storyId} lives on a machine; its agents use the machine's own sign-in, not an Agent Account`,
      );
    }
    const label = opts?.session?.trim() || undefined;
    const { sessionId, fresh } = await deps.launchRemote({
      teamKey: resolved.teamKey,
      workspaceId: remote.workspaceId,
      location: remote.location,
      headed,
      // A machine's agent always opens as a tab there, never hidden, so no agent named
      // runs the ordinary built-in (its own permission prompts), as a headed open does.
      agentKey: launchAgentKey(opts?.agent),
      ...(label ? { name: titleCaseLabel(label) } : {}),
      ...(opts?.sessionMode === "new" && !label ? { sessionMode: "new" as const } : {}),
      ...(initialPrompt ? { initialPrompt } : {}),
    });
    // A new agent on the machine starts with the prompt; one already running there is
    // focused as it is, its prompt not sent.
    return initialPrompt ? { sessionId, promptDelivered: fresh === true } : { sessionId };
  }

  // SW-GW-6: split the two roles `storyId` plays. `resolveStory` keys off the BARE
  // storyId (the story's file path is per-story, not per-session), while the registry
  // handle = `storyId#<label>` for a NAMED session (main session → bare storyId, so the
  // main path is byte-for-byte unchanged, AC-3). The spine + heal are handle-agnostic,
  // so named sessions inherit them for free.
  const explicitLabel = opts?.session?.trim() || undefined;

  // SW-DX-17e: `sessionMode:'new'` with NO explicit `session` mints an ANONYMOUS fork
  // — a fresh, collision-free `attempt-N` label reserved via `reserveForkLabel`'s
  // begin-probe, so this launch ALWAYS spawns a new session instead of focusing the
  // main one (AC-2). The probe already took the fork's cap slot, so the spine is told
  // `preReserved` (skip its own `begin`). An explicit `session` label is authoritative:
  // a NAMED session already forks by its own distinct handle/role (SW-GW-6), so
  // `sessionMode` is a no-op for it — no double-mint.
  let label = explicitLabel;
  let preReserved = false;
  if (!explicitLabel && opts?.sessionMode === "new") {
    label = await reserveForkLabel(storyId, deps.begin);
    preReserved = true;
  }

  const handle = label ? `${storyId}#${label}` : storyId;
  // A launch that names its account must run with it: focusing a live session that runs
  // with another account would leave the agent on that account while reporting a launch.
  if (opts?.account && opts.sessionMode !== "new" && !explicitLabel) {
    const existingSession = await deps.resolveSession(handle);
    if (existingSession) {
      const existing = await deps.resolveAccountProvenance?.(handle);
      if (existing?.accountId !== opts.account.id) {
        const existingLabel = existing?.label ?? "an account not recorded by this version";
        throw new Error(
          `already-running-with-different-account: existing session used ${existingLabel}; choose “Launch a new session” to use the requested account`,
        );
      }
    }
  }
  // OQ-4 default title: caller's explicit `title` wins; else a title-cased label for a
  // named/fork session, or the storyId for the main session (never "Terminal N").
  const displayTitle = opts?.title ?? (label ? titleCaseLabel(label) : storyId);
  // SW-GW-7: a thin adapter over the shared spine — story-specific resolution
  // (per-story worktree cwd; storyFilePath-keyed focus; SW-GW-6 session/title
  // baked into the connect thunk; SW-GW-9 seed via `sendLine`) closed into the thunks.
  return launchAgentSession(
    handle,
    headed,
    {
      begin: deps.begin,
      abort: deps.abort,
      resolveSession: deps.resolveSession,
      isSessionParked: deps.isSessionParked,
      probeAlive: deps.probeAlive,
      evictStaleTab: deps.evictStaleTab,
      ensureCwd: () => ensureStoryWorktree(deps, resolved.teamKey, resolved.storyFilePath),
      connect: (cwd) =>
        deps.connect({
          teamKey: resolved.teamKey,
          cwd,
          storyFilePath: resolved.storyFilePath,
          session: label,
          title: displayTitle,
          agent: {
            agent,
            prompt: initialPrompt,
            ...(headed ? {} : { headless: true }),
            account: opts?.account
              ? { id: opts.account.id, generation: opts.account.generation }
              : undefined,
          },
        }),
      // Where the agent was launched and which agent revision it runs, so a finished
      // interactive agent reaches the routines it is in reach of.
      bind: (h, sessionId, agentRevision) =>
        deps.bind(h, sessionId, { teamKey: resolved.teamKey, agentRevision }),
      killSession: deps.killSession,
      waitLive: deps.waitLive,
      focus: (sessionId) => deps.focus(resolved.teamKey, sessionId, resolved.storyFilePath),
      sendLine: deps.sendLine,
      emptyCwdError: `unavailable: worktree did not open for "${storyId}"`,
    },
    { initialPrompt, preReserved },
  );
}

/** The agent a launch runs: the one it names (trimmed), else the built-in Claude agent for
 *  a headed launch and its unattended twin (permission checks skipped) for a background one
 *  nobody watches. A named agent always wins, with its own permissions. */
export function launchAgentKey(agent?: string, headed = true): string {
  return agent?.trim() || (headed ? DEFAULT_LAUNCH_AGENT : DEFAULT_UNATTENDED_AGENT);
}

// ── SW-DX-18 (openWorkspace `branch` target) ─────────────────────────────────

/** The `branch` open target: a source-repo slug + the branch/PR ref to check out. */
export interface BranchOpenTarget {
  /** `owner/repo` source-repo slug. */
  repo: string;
  baseRepo?: string;
  pullNumber?: number;
  /** The branch/PR ref (e.g. a teammate's `dev_branch`). */
  branch: string;
  /** Optional, already-grounded story to reconcile after checkout succeeds. */
  linkedStoryId?: string;
  /** Absent → a review of the cached PR head. `existing` opens a branch that already
   *  exists; `new` creates it from the repo's local default branch. */
  checkout?: BranchCheckout;
}

/** How an agent launch checks its branch out (see `BranchOpenTarget.checkout`); a review
 *  is the absent default. */
export type BranchCheckout = Exclude<BranchCheckoutMode, "review">;

/** Where a branch target resolves to (trusted-side), before the checkout. */
export interface BranchResolution {
  /** Local clone of the source repo to check the branch out from. */
  repoPath: string;
  /** The team bucket the branch workspace's terminal opens under. */
  teamKey: string;
  /** Install-layer context sourced into the worktree. */
  codesenseHome: string;
  githubUserId: number;
  orgSlug: string;
  team: string;
}

/**
 * The `branch` open's injected seams — a focused subset of the story spine.
 * `begin`/`abort`/`bind`/`resolveSession`/`waitLive` are
 * handle-agnostic (they pass an opaque handle / cwd to the same Rust registry +
 * launcher the story path uses), so a branch reuses them with the
 * `${repo}::${branch}` handle; only `resolveBranch` + `ensureBranchWorktree` +
 * the workspace-keyed `connect`/`focus` differ from a story.
 */
export interface BranchLaunchDeps {
  /** Resolve `repo (slug) → { local clone path, team, install context }`, or
   *  `null` when the repo has no local clone / no active team (honest degrade).
   *  Repo-scoped — the branch flows separately into `ensureBranchWorktree`. */
  resolveBranch: (repo: string) => Promise<BranchResolution | null>;
  /** AC-5: reserve a cap slot for `handle`; rejects `busy` when saturated.
   *  `true` ⇒ THIS call freshly reserved (only the owner aborts on failure). */
  begin: (handle: string) => Promise<boolean>;
  /** Free a reserved-but-unbound slot on the failure path. */
  abort: (handle: string) => Promise<void>;
  /** SW-DX-17b (AC-8): reap an orphaned session (spawned but never bound) through
   *  the one `kill_session` choke — same concern as the story path: a `bind` that
   *  fails after `connect` spawned a live PTY leaves an agent running with no
   *  registry handle (invisible + unkillable via the SW-DX-17a panel). */
  killSession: (sessionId: string) => Promise<void>;
  /** Resolve `handle`'s bound session id, or `null` (idempotent re-focus). */
  resolveSession: (handle: string) => Promise<string | null>;
  /** Check out the branch into a MANAGED worktree + materialize the install
   *  layer (`ensure_branch_workspace`); resolves the worktree cwd. Rejects
   *  `unavailable:` if the checkout would land outside a managed surface. */
  ensureBranchWorktree: (res: BranchResolution, target: BranchOpenTarget) => Promise<string>;
  /** Reconcile the checked-out branch into a uniquely resolved story. */
  reconcileStory: (storyId: string, cwd: string, sourceRepo: string) => Promise<boolean | void>;
  /** Launch (or focus the existing) agent session at `cwd`; `workspaceKey` keys
   *  the launcher's per-workspace role dedup (the branch handle). `headed` opens the
   *  session's tab in a visible tile for `cwd`; otherwise it runs out of sight. `agent`
   *  is prepared by the backend for the spawned session (see the story `connect`). */
  connect: (p: {
    teamKey: string;
    cwd: string;
    workspaceKey: string;
    headed: boolean;
    agent: AgentLaunchRequest;
  }) => Promise<ConnectedSession>;
  /** Stamp `handle → sessionId` trusted-side (+ arm status projection). */
  bind: (handle: string, sessionId: string, scope?: LaunchBindScope) => Promise<void>;
  /** Best-effort wait for the agent PROCESS to come up (liveness probe). */
  waitLive: (sessionId: string) => Promise<boolean>;
  /** Submit a line into the live agent: a named agent's default prompt, when the launch
   *  brought no review prompt. */
  sendLine?: (sessionId: string, text: string) => Promise<{ submitted: boolean }>;
  /** AC-1: focus the launched terminal (layout `console` + active tab). */
  focus: (teamKey: string, sessionId: string, workspaceKey: string) => void;
  // ── SW-GW-5 (guaranteed-live idempotent re-open) seams ─────────────────────
  /** The bound session sits in a TERMINAL managed state (done / needs-attention)
   *  — not re-usable by an idempotent re-open. (The story deps already carry this
   *  for `sendToSession`; the branch deps add it for the SW-GW-5 heal.) */
  isSessionParked: (sessionId: string) => boolean;
  /** Real child-process liveness of a bound session (`try_wait`, not membership);
   *  a probe error reads as dead (heal), never a false "alive". */
  probeAlive: (sessionId: string) => Promise<boolean>;
  /** Evict the stale role-tagged tab for a superseded session so a respawn's
   *  `(teamKey, role)` dedup misses and spawns fresh (mirrors the story spine). */
  evictStaleTab: (sessionId: string) => Promise<void>;
}

/**
 * The opaque `by_handle` registry key for a branch workspace — the pre-reserved
 * `<repo>::<branch>` shape (SW-DX-11/17a addressing model), non-colliding with a
 * story (bare id), an epic (absolute path), or `team::epic`.
 */
export function branchHandle(repo: string, branch: string): string {
  return `${repo}::${branch}`;
}

/** A fork PR is base-repository scoped: the same head branch can back multiple
 * pull requests, so its immutable base/PR identity must participate in every
 * registry, launcher, focus, and cancellation lookup. */
export function branchTargetHandle(target: BranchOpenTarget): string {
  return reviewWorkspaceHandle(target);
}

export const REVIEW_PHASE_TIMEOUT_MS = 60_000;

function decorateReconciliationOutcome(error: unknown, status: "reconciled" | "skipped"): Error {
  const prefix = status === "reconciled" ? "Reconciled" : "Skipped reconcile";
  const message = error instanceof Error ? error.message : String(error);
  return message.startsWith("canceled:")
    ? new Error(`canceled: ${prefix} · ${message.slice("canceled:".length).trim()}`)
    : new Error(`${prefix} · ${message}`);
}

export async function withReviewPhaseTimeout<T>(promise: Promise<T>, phase: string): Promise<T> {
  let timeoutId: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<never>((_resolve, reject) => {
    timeoutId = setTimeout(
      () => reject(new Error(`timeout: ${phase} did not complete`)),
      REVIEW_PHASE_TIMEOUT_MS,
    );
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (timeoutId) clearTimeout(timeoutId);
  }
}

/**
 * `openWorkspace({target:{kind:'branch', repo, branch}})` — resolve the repo's
 * local clone + install context, reserve a cap slot, check the branch out into a
 * MANAGED worktree with the team install layer, launch the
 * agent, bind the session into the ONE flat `by_handle` registry (so SW-DX-17a
 * enumerates / kills / adopts it), and focus the terminal. Mirrors
 * `openStoryWorkspace`: idempotent re-focus of an already-open branch, and only
 * the FRESH reserver aborts on failure (never releasing a launch it doesn't own).
 */
export async function openBranchWorkspace(
  target: BranchOpenTarget,
  deps: BranchLaunchDeps,
  opts?: {
    headed?: boolean;
    /** The agent the review session runs; omitted = the built-in Claude agent. */
    agent?: AgentLaunchRequest;
    isCanceled?: () => boolean;
    onPhase?: (phase: "opening" | "reconciling" | "skipping-reconcile" | "launching") => void;
    onReuse?: () => void;
    onOwnershipAcquired?: () => void;
  },
): Promise<{
  sessionId: string;
  reconcileStatus?: "reconciled" | "skipped";
  warnings?: AgentLaunchWarning[];
}> {
  const headed = opts?.headed !== false;
  const agent: AgentLaunchRequest = {
    ...(opts?.agent ?? { agent: launchAgentKey(undefined, headed) }),
    ...(headed ? {} : { headless: true }),
  };
  const handle = branchTargetHandle(target);
  const throwIfCanceled = () => {
    if (opts?.isCanceled?.()) throw new Error("canceled: review open canceled before launch");
  };
  throwIfCanceled();
  const res = await withReviewPhaseTimeout(
    deps.resolveBranch(target.baseRepo ?? target.repo),
    "repository resolution",
  );
  if (!res) {
    throw new Error(
      `unavailable: cannot resolve a local clone for repo "${target.repo}" in the active team`,
    );
  }
  throwIfCanceled();
  // SW-GW-7: a thin adapter over the shared spine — branch-specific resolution
  // (checkout cwd, handle-keyed connect/focus) closed into the thunks. `headed:false`
  // (SW-GW-7) backgrounds the branch-review agent exactly as a headless story does.
  let reconcileStatus: "reconciled" | "skipped" | undefined;
  // Once another caller owns the shared reservation, this operation is only a
  // joiner. Keep its UI non-cancelable for the rest of the launch; publishing
  // checkout/reconcile phases again would falsely imply cleanup ownership.
  let joiningExistingLaunch = false;
  const publishOwnedPhase = (
    phase: "opening" | "reconciling" | "skipping-reconcile" | "launching",
  ) => {
    if (!joiningExistingLaunch) opts?.onPhase?.(phase);
  };
  const ensureAndReconcile = async (): Promise<string> => {
    publishOwnedPhase("opening");
    throwIfCanceled();
    const cwd = await withReviewPhaseTimeout(
      deps.ensureBranchWorktree(res, target),
      "review checkout",
    );
    throwIfCanceled();
    if (target.linkedStoryId) {
      publishOwnedPhase("reconciling");
      // This writes the confirmed story link. Do not abandon the mutation on a
      // client timeout and then report failure while it completes in the background.
      const reconciled = await deps.reconcileStory(
        target.linkedStoryId,
        cwd,
        target.baseRepo ?? target.repo,
      );
      reconcileStatus = reconciled === false ? "skipped" : "reconciled";
      throwIfCanceled();
    } else {
      publishOwnedPhase("skipping-reconcile");
    }
    publishOwnedPhase("launching");
    return cwd;
  };
  // Creating a branch never joins a launch already on it: a live one means the branch
  // exists; one still starting may yet fail, so say only that it is starting.
  const refuseJoinWhenCreating = (starting: boolean) => {
    if (target.checkout !== "new") return;
    throw new Error(
      starting
        ? `unavailable: A launch on \`${target.branch}\` is already starting — wait for it, then use Repo + branch to open the branch.`
        : `unavailable: Branch \`${target.branch}\` already exists — use Repo + branch to open it.`,
    );
  };
  let readinessError: Error | undefined;
  let opened: { sessionId: string; promptDelivered?: boolean; warnings?: AgentLaunchWarning[] };
  try {
    opened = await launchAgentSession(handle, headed, {
      // Reservation/bind/cleanup are authoritative in-memory Rust mutations.
      // Await them; racing them client-side can release or bind the wrong retry.
      begin: deps.begin,
      abort: deps.abort,
      resolveSession: (candidate) =>
        withReviewPhaseTimeout(deps.resolveSession(candidate), "session resolution"),
      isSessionParked: deps.isSessionParked,
      probeAlive: (sessionId) =>
        withReviewPhaseTimeout(deps.probeAlive(sessionId), "session liveness probe"),
      evictStaleTab: deps.evictStaleTab,
      onReuse:
        target.linkedStoryId || opts?.isCanceled || target.checkout === "new"
          ? async () => {
              refuseJoinWhenCreating(false);
              joiningExistingLaunch = true;
              opts?.onReuse?.();
              throwIfCanceled();
              if (target.linkedStoryId) await ensureAndReconcile();
              throwIfCanceled();
            }
          : undefined,
      onReservedReuse: () => {
        refuseJoinWhenCreating(true);
        joiningExistingLaunch = true;
        opts?.onReuse?.();
      },
      onReservedOwnershipAcquired: () => {
        joiningExistingLaunch = false;
        opts?.onOwnershipAcquired?.();
      },
      assertContinue: throwIfCanceled,
      ensureCwd: ensureAndReconcile,
      connect: (cwd) => {
        const pending = deps.connect({
          teamKey: res.teamKey,
          cwd,
          workspaceKey: handle,
          headed,
          agent,
        });
        return withReviewPhaseTimeout(pending, "agent launch").catch((error) => {
          // The launcher has no abort signal. If it completes after our bounded
          // wait, reap that late session so timeout recovery cannot orphan a PTY.
          if (error instanceof Error && error.message.startsWith("timeout:")) {
            void pending
              .then(({ sessionId }) => {
                deps.evictStaleTab(sessionId);
                return deps.killSession(sessionId);
              })
              .catch(() => {});
          }
          throw error;
        });
      },
      bind: (h, sessionId, agentRevision) =>
        deps.bind(h, sessionId, { teamKey: res.teamKey, agentRevision }),
      killSession: deps.killSession,
      waitLive: async (sessionId) => {
        try {
          const live = await withReviewPhaseTimeout(deps.waitLive(sessionId), "agent readiness");
          if (!live) throw new Error("unavailable: review agent did not become ready");
          return true;
        } catch (error) {
          const message = error instanceof Error ? error.message : String(error);
          readinessError = new Error(`unavailable: review agent readiness failed (${message})`);
          throw readinessError;
        }
      },
      focus: (sessionId) => deps.focus(res.teamKey, sessionId, handle),
      sendLine: deps.sendLine,
      emptyCwdError: `unavailable: branch worktree did not open for "${handle}"`,
    });
  } catch (error) {
    if (reconcileStatus) {
      throw decorateReconciliationOutcome(error, reconcileStatus);
    }
    throw error;
  }
  if (readinessError) {
    throw reconcileStatus
      ? decorateReconciliationOutcome(readinessError, reconcileStatus)
      : readinessError;
  }
  return { ...opened, reconcileStatus };
}

/**
 * The `dir` open's injected seam — a SINGLE synchronous call into SW-GW-1's
 * generic-workspace wrapper. No registry dance, no cap slot, no worktree checkout:
 * the directory already exists, so the whole open is `openOrFocusGenericWorkspace`
 * (which mints the per-dir id, dedupes re-opens, and paints the tile synchronously).
 */
export interface DirLaunchDeps {
  /** Open (or focus) a generic CLI workspace at `path` — SW-GW-1's synchronous
   *  `openOrFocusGenericWorkspace`. Injected so the launcher stays pure/testable. */
  openGenericWorkspace: (path: string) => void;
}

/**
 * `openWorkspace({target:{kind:'dir', path}})` — SW-GW-2. Open (or focus) a plain
 * CLI workspace rooted at `path`, dispatching to SW-GW-1's generic-workspace
 * wrapper. The DEGENERATE target-kind: the dir already exists, so there is no
 * repo/story resolve, no cap slot, no worktree checkout, and no bound session —
 * dedupe/focus is owned by `openWorkspace`'s find-by-`workspaceId` path. The open
 * is SYNCHRONOUS (the tile paints before this returns — AC-8 non-blocking), and it
 * returns `{}` (no session handle, like the epic open). There is no
 * managed-surface resolve because a generic workspace writes nothing to disk
 * (SW-GW-1).
 */
export function openDirWorkspace(
  target: { path: string },
  deps: DirLaunchDeps,
): { sessionId?: string } {
  const path = typeof target.path === "string" ? target.path.trim() : "";
  if (!path) {
    throw new Error("unavailable: dir target needs a non-empty { path }");
  }
  deps.openGenericWorkspace(path);
  return {};
}

/**
 * SW-GW-7: the `by_handle` registry key for a HEADLESS `dir` agent session — the
 * dir's own absolute path. Non-colliding with a story (bare id), a branch
 * (`repo::branch`), or an epic (a `…/README.md` file path). The headed `dir` open
 * (a plain shell tile) mints no handle; only the headless agent session needs one.
 * A named `session` keys its own `path#session` entry, apart from the dir's own.
 */
export function dirHandle(path: string, session?: string): string {
  const label = session?.trim();
  return label ? `${path}#${label}` : path;
}

/**
 * SW-GW-11 `runPrompt({target, prompt})` — the prompt DRIVE primitive
 * (supersedes the bounded `fireSkill` + its `KNOWN_SKILLS` enum). Resolve the
 * `target`'s story to a managed path (confinement + `{storyPath}`), resolve the
 * addressed session (bare `storyId` main session, or the SW-GW-6 compound
 * `storyId#<label>`) trusted-side, substitute the single `{storyPath}` token,
 * then inject the text into the resolved session — never a caller-supplied id
 * (AC-10/AC-12). A handle with no live session is rejected, never redirected and
 * never spawned (spawning lives on `openWorkspace`, SW-GW-9). The
 * prompt itself is arbitrary; there is no allow-list.
 */
export async function runPrompt(params: RunPromptParams, deps: DashboardLaunchDeps): Promise<void> {
  // Split the handle (the ONE canonical parse, shared with the bridge validator):
  // the bare `storyId` keys `resolveStory` (the story path is per-story, shared by
  // every session), while the full `target` handle keys `resolveSession` (bare id
  // → main session; `storyId#label` → named session).
  const { storyId } = splitStoryHandle(params.target);
  const handle = params.target;

  // Resolve the story to a managed absolute path (confinement + `{storyPath}`).
  const resolved = await deps.resolveStory(storyId);
  if (!resolved) {
    throw new Error(`unavailable: cannot resolve story "${storyId}" to a managed path`);
  }

  // AC-10/AC-12/AC-5: the drive target is the addressed bound session, resolved
  // trusted-side from the COMPOUND handle. No live binding → reject (never fall
  // through to any other id, never spawn — that's `openWorkspace`'s job).
  const sessionId = await deps.resolveSession(handle);
  if (!sessionId) {
    throw new Error(`unavailable: no live session for "${handle}" (open it first)`);
  }

  // AC-6: fill the single `{storyPath}` token and send the line.
  const composed = substituteStoryPath(params.prompt, resolved.storyFilePath);
  await deps.injectLine(sessionId, composed);
}

/**
 * `sendToSession({target, text})` — the user→agent chat SEND half. Resolve the
 * target to the story's OWN bound session **trusted-side** (AC-3/AC-12), refuse
 * a dead/parked session (AC-8), inject the free-form turn via the agent-aware
 * submit primitive (AC-1/AC-7), and **echo the user turn only after a confirmed
 * landing** so a dropped write never leaves a phantom bubble.
 *
 * The receive half is not here: the agent replies via `post_agent_message`,
 * surfaced as SW-DX-3 `onStatus` session-message deltas — DX-10 adds no receive
 * listener, emit, or verb (AC-2/AC-9/AC-10).
 */
export async function sendToStorySession(
  params: SendToSessionParams,
  deps: DashboardLaunchDeps,
): Promise<SendToSessionResult> {
  const text = typeof params.text === "string" ? params.text : "";
  if (text.trim().length === 0) {
    throw new Error("sendToSession: {text} must be a non-empty string");
  }
  if (typeof params.target !== "string" || !params.target) {
    throw new Error("sendToSession: {target} must be a non-empty string");
  }

  // AC-3/AC-12/D6: the send target is the story's OWN bound session, resolved
  // trusted-side from the Rust storyId→sessionId binding. A forged/other id in
  // the doc args can never redirect the write; no live binding → reject (never
  // fall through to a caller-supplied id).
  const sessionId = await deps.resolveSession(params.target);
  if (!sessionId) {
    throw new Error(`unavailable: session not live for "${params.target}" (open it first)`);
  }

  // AC-8: don't chat into a dead/parked agent — a session in a terminal managed
  // state (done / needs-attention) can't act on the turn. Reject clearly rather
  // than writing into a void (a dead PTY is caught by `sendLine` below).
  if (deps.isSessionParked(sessionId)) {
    throw new Error("unavailable: session not live");
  }

  // AC-1/AC-7/D3: inject via the agent-aware submit primitive. A held line is
  // `delivered:false` (surfaced), never silently dropped or reported as sent.
  const { submitted } = await deps.sendLine(sessionId, text);
  if (!submitted) {
    return { delivered: false };
  }

  // Echo the user turn ONLY after a confirmed landing — no phantom "sent" bubble
  // on a dropped write (mirrors the in-app AgentChatSurface discipline).
  deps.echoUserTurn(sessionId, text);
  return { delivered: true };
}

/**
 * SW-GW-10 — `closeWorkspace({target})`: the board-callable CLOSE half of the
 * session lifecycle. Resolve the opaque handle to its bound session **trusted-side**
 * (AC-2 — a doc-supplied raw session id is not a handle and resolves to nothing),
 * evict the tab, then route through the ONE authoritative `dashboard_kill_by_handle`
 * choke (AC-1). An unbound handle is a typed no-op (AC-3) — the choke is still
 * called so Rust's `abort` can clear a reserved-but-unbound leaked slot.
 *
 * Shares the "probe + evict + kill" shape with SW-GW-5's heal, reusing the same
 * `evictStaleTab` seam (the heal fire-and-forgets a SUPERSEDED session; here the
 * board reaps one it opened).
 */
export async function closeStorySession(
  target: string,
  deps: DashboardLaunchDeps,
): Promise<CloseWorkspaceResult> {
  if (typeof target !== "string" || !target) {
    throw new Error("closeWorkspace: {target} must be a non-empty string");
  }

  // AC-2: resolve the handle→session trusted-side. A forged/raw id in the doc args
  // never resolves to another handle's session (null → the no-op path below).
  const sessionId = await deps.resolveSession(target);

  // Evict the tab FIRST (best-effort) — mirrors the SW-GW-5 heal order and
  // keeps the viewer tidy even if the kill races a natural exit. Only when a
  // live binding resolved; the unbound path has no tab.
  if (sessionId) await deps.evictStaleTab(sessionId);

  // ALWAYS route through the authoritative choke — incl. the null-session path, so
  // Rust's `abort` clears a reserved-but-unbound leaked slot. Idempotent (AC-3).
  await deps.closeByHandle(target);
  return { closed: true };
}

// ── Controller registry ─────────────────────────────────────────────────────
//
// The static bridge allowlist handlers (`html-bridge.ts`) can't reach the React
// render scope that owns the teamKey + pipeline tree. So the live `HtmlViewer`
// registers a scope-bound controller here; the handlers forward to it. When no
// dashboard is mounted, the verbs reject (unavailable) rather than guessing.

/**
 * SW-DX-15: the `openWorkspace` primitive's target descriptor — *what* to open.
 * `story` is the shipped case; `epic` opens the board's OWN containing epic.
 * SW-DX-16 widens `epic` with an optional `{team, epic}` — absent → own epic
 * (unchanged), present → an EXPLICIT managed epic (`team` is the full teamKey
 * `orgSlug:teamName`; `epic` is a content-root-relative path resolved + confined
 * trusted-side). `team`/`epic` are both-or-neither. SW-DX-18 adds the `branch`
 * kind (`{repo, branch}` — check out an arbitrary branch/PR into a managed worktree).
 */
export type OpenWorkspaceTarget =
  // SW-GW-6: `session`/`title` are OPTIONAL and STORY-ONLY (put on this variant so
  // TypeScript forbids them on epic/branch/dir with zero runtime guards). `session`
  // (a slug) opens an ADDITIONAL named agent session on the story (handle
  // `storyId#session`); omitted → the main session, unchanged. `title` names the tab.
  // `agent` (an agent's key, or its id in the workspace's team then the personal space;
  // default the built-in Claude agent) is what an open launches: `story` and `branch`
  // always, `dir` always (a `dir` with an agent opens an agent session), and `epic` only
  // headless (a headed epic open is a bare terminal). The backend prepares its runtime,
  // args, permissions and account for the spawned session.
  | {
      kind: "story";
      storyId: string;
      session?: string;
      title?: string;
      agent?: string;
      /** Where the story's workspace is created when it has none yet: a team machine's
       *  id, or `"local"` for This Mac. An existing workspace opens where it lives; a
       *  `machineId` naming somewhere else is refused (a workspace stays where it was
       *  created). Absent: an existing workspace where it lives, else This Mac. */
      machineId?: string;
    }
  /** Trusted file-nav panes open the native story workspace directly by path. */
  | { kind: "story"; storyPath: string }
  | { kind: "epic"; team?: string; epic?: string; agent?: string }
  | {
      kind: "branch";
      repo: string;
      /** Base repository identity, required by review-console admission even when
       * checkout uses the fork/head repository. */
      baseRepo?: string;
      pullNumber?: number;
      branch: string;
      linkedStoryId?: string;
      agent?: string;
      /** Trusted launchers only (the bridge never forwards it): open an existing
       *  branch or create a new one instead of reviewing the cached PR head. */
      checkout?: BranchCheckout;
    }
  // SW-GW-2: the `dir` kind — open a plain CLI workspace at ANY directory (the
  // SW-GW-1 generic kind), dashboard-triggered. The DEGENERATE target: `path`
  // already exists, so there is no repo/story resolve and no session handle.
  // `session` (trusted launchers only) names a distinct agent session at the dir, so it
  // never collides with the dir's plain session or another agent opened there.
  | { kind: "dir"; path: string; agent?: string; session?: string };

/**
 * SW-DX-15: the widened `openWorkspace` arg. `headed` (default `true`) is the
 * orthogonal *whether-to-show-it* axis; `headed:false` (background) is SW-DX-17b.
 */
export interface OpenWorkspaceArg {
  frameId?: string;
  target: OpenWorkspaceTarget;
  headed?: boolean;
  /** SW-GW-9: seed the spawned agent with a first prompt in the SAME call. Story
   *  targets only (the sole agent-backed session-returner at call time); the bridge
   *  rejects it for epic/branch/dir. Empty/whitespace ≡ absent (byte-identical spawn). */
  initialPrompt?: string;
  /** SW-DX-17e: choose the launch's session semantics. `"focus"`/absent (DEFAULT)
   *  reuses+focuses the story's existing session (idempotent — no fork, no second
   *  cap slot). `"new"` forks a FRESH anonymous session (auto-minted `attempt-N`
   *  handle) even when one is live — for a retry / parallel attempt the caller
   *  doesn't need to name. STORY-ONLY: the bridge rejects `"new"` for epic/branch/
   *  dir (their launchers have no distinct-role fork path). */
  sessionMode?: "new" | "focus";
  /** Branch-only prompt submitted after the launched session is live. */
  reviewPrompt?: string;
  /** Trusted-renderer correlation id for pre-session cancellation. */
  operationId?: string;
  /** Trusted-host-only explicit account selection. HTML bridge callers cannot
   * synthesize this; Agent Accounts supplies it after its own picker. */
  account?: { userId: number; id: string; generation: number };
}

/**
 * SW-GW-3: the OBSERVE-half answer for a story's live workspace state. `exists`
 * = a story workspace is OPEN in CodeSense (by any means — a dashboard launch OR
 * a manual viewer open); `live` = its session's child is alive (real `try_wait`,
 * never "a tab exists"); `sessionId` = that session (or null when not started).
 * A board maps `{exists,live}` → the honest affordance: `exists:false` → Launch,
 * `exists:true` → Focus, `live:true` → running.
 */
export interface WorkspaceStatus {
  exists: boolean;
  live: boolean;
  sessionId: string | null;
}

export interface DashboardLaunchController {
  // SW-DX-15: one generic open primitive. `story` returns its `{sessionId}`; the
  // long-lived `epic` open registers cap-free and returns `{}` (the board
  // re-probes `getEpicHandle()` to light up chat).
  openWorkspace: (arg: OpenWorkspaceArg) => Promise<{
    sessionId?: string;
    promptDelivered?: boolean;
    reconcileStatus?: "reconciled" | "skipped";
    /** A named agent launch's warnings (never a refusal); absent when there are none. */
    warnings?: AgentLaunchWarning[];
  }>;
  /** SW-GW-3: the live open/running state of a story's workspace, keyed by
   *  `storyId`, resolved trusted-side from the viewer-store open list (NOT the
   *  launch registry — so a manually-opened story is observed too) + a real
   *  liveness probe. A read: audited, no budget. */
  getWorkspaceStatus: (storyId: string) => Promise<WorkspaceStatus>;
  runPrompt: (params: RunPromptParams) => Promise<void>;
  sendToSession: (params: SendToSessionParams) => Promise<SendToSessionResult>;
  /** SW-GW-10: close (kill + evict) a session the board opened, by opaque handle.
   *  Resolves the handle→session trusted-side; an unbound target is a silent
   *  no-op (AC-3). MANAGEMENT-tier: audited, no write/send budget. */
  closeWorkspace: (target: string) => Promise<CloseWorkspaceResult>;
  /** SW-DX-11: doc-scoped file read/dir-list confined to the doc's own dir
   *  subtree. The controller supplies the doc's own path + the reviewed
   *  extension whitelist; the bridge handler forwards only the caller's `path`. */
  readWorkspace: (path: string, frameId?: string) => Promise<WorkspaceReadResult>;
  /** SW-DX-11 §5: the `epicDocPath` handle of the epic workspace CONTAINING this
   *  dashboard (or `null` when the doc isn't inside an epic). Resolved
   *  trusted-side so a doc can `sendToSession(target: <this>)` into the epic's
   *  running agent WITHOUT hard-coding a machine-specific absolute path. */
  getEpicHandle: () => string | null;
  /** SW-DX-14: the absolute directory of this dashboard doc (or `null`). Lets a
   *  board build an absolute path to a sibling story for `runSkill` WITHOUT an
   *  open epic workspace — the skill's agent CLI runs in a different cwd, so it
   *  needs an absolute path, and the board can't otherwise learn its own dir. */
  getDocDir: () => string | null;
}

let activeController: DashboardLaunchController | null = null;

/** Register (or clear, with `null`) the active dashboard's launch controller.
 *  The live `HtmlViewer` calls this on mount/unmount. */
export function setDashboardLaunchController(controller: DashboardLaunchController | null): void {
  activeController = controller;
}

/** The active controller, or `null` when no dashboard is mounted. */
export function getDashboardLaunchController(): DashboardLaunchController | null {
  return activeController;
}
