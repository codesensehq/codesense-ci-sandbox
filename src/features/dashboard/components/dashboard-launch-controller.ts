// SPDX-License-Identifier: MIT
/**
 * SW-DX-7 — the REAL-dependency wiring of the dashboard execution loop, plus the
 * hook that registers it while a dashboard is mounted.
 *
 * `dashboard-launch.ts` holds the pure, DI-tested orchestration; this module
 * binds that orchestration to the live seams (Rust commands, the
 * agent-session-launcher, the viewer store, the preview + status manager) and
 * exposes a `DashboardLaunchController` the static bridge handlers forward to.
 * Kept separate so the orchestration stays store/Tauri-free and unit-testable.
 *
 * Registration is lifecycle-scoped: `useRegisterDashboardLaunchController` sets
 * the controller on mount and clears it on unmount, so the bridge verbs
 * fail-closed (`unavailable`) whenever no dashboard is showing.
 */

import { useEffect, useMemo } from "react";
import {
  dashboardLaunchBegin,
  dashboardLaunchBind,
  dashboardLaunchAbort,
  dashboardResolveSession,
  dashboardProbeSessionsAlive,
  dashboardListSessions,
  dashboardKillByHandle,
  resolveStoryPath,
  resolveEpicPath,
  readWorkspaceFile,
  terminalCloseTab,
  ensureBranchWorkspace,
  ensureEpicWorkspace,
  establishWorktreeStoryLink,
  agentLaunchPrepare,
  sessionHasLiveAgent,
  sessionsRunningProcesses,
  AGENT_PROVIDERS,
  type AgentLaunchWarning,
  type RunningJobs,
  type LaunchBindScope,
  type PreparedAgentLaunch,
} from "@/tauri/commands";
import { findOpenStoryWorkspace } from "@/shared/lib/workspace-liveness";
import { READ_WORKSPACE_EXTS } from "./html-bridge";
import {
  connectAgentSession,
  makeRealLaunchDeps,
  waitForAgentLive,
  agentBasename,
  GUARDED_ENV_STRIP,
  type LaunchDeps,
  type PreparedShellLaunch,
} from "@/shared/lib/agent-session-launcher";
import { resolveAgentCliPath } from "@/shared/lib/default-agent";
import { injectAgentLine } from "@/shared/lib/agent-session-injection";
import { awaitWorktreeEstablished } from "@/shared/lib/worktree/pending-establish";
import { awaitContextForLaunch } from "@/features/viewer/lib/await-worktree-context";
import {
  expectAgentLaunch,
  forgetAgentLaunch,
  genericWorkspaceId,
  normalizeWorkspaceDir,
  selectRunningTerminalTabIndex,
  useViewerStore,
} from "@/shared/stores/viewer-store";
import { readShowInputs, resolveAgentShow, runShowPlan } from "@/features/viewer/lib/show-agent";
import { showNewTab, whereTheUserIs } from "@/features/viewer/lib/terminal-tab-keys";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import { isTerminalManagedState } from "@/shared/lib/managed-session";
import { useAgentChatStore } from "@/shared/stores/use-agent-chat-store";
import { useAgentStateStore } from "@/shared/stores/agent-state-store";
import { storyIdFromPath } from "@/shared/lib/worktree/resolve";
import { isRemoteWorkspace, remoteWorkspaceDriver } from "@/shared/lib/workspace-location";
import { placeStoryWorkspace } from "@/features/dashboard/lib/place-story-workspace";
import { agentLaunchNames, trackAgentLaunch } from "@/features/viewer/lib/agent-launch-retry";
import { useAgentsStore } from "@/features/agents/stores/agents-store";
import { getActiveTeamKey, parseTeamKey, teamKeyHasTeam } from "@/shared/lib/team-key";
import {
  openEpicWorkspaceFor,
  resolveEpicCommandContext,
  type EpicOpenTarget,
} from "@/features/viewer/lib/epic-workspace-opener";
import { syntheticRepoKey } from "@/shared/lib/build-synthetic-repo";
import { resolveRealRepoPath } from "@/shared/lib/resolve-source-repo-path";
import { publishReviewOpenPhase } from "./review-open-progress";
import {
  openStoryWorkspace,
  openBranchWorkspace,
  openDirWorkspace,
  launchAgentSession,
  launchAgentKey,
  branchTargetHandle,
  withReviewPhaseTimeout,
  dirHandle,
  runPrompt as runStoryPrompt,
  sendToStorySession,
  closeStorySession,
  getDashboardLaunchController,
  setDashboardLaunchController,
  type DashboardLaunchController,
  type DashboardLaunchDeps,
  type BranchLaunchDeps,
  type BranchOpenTarget,
  type BranchResolution,
  type DirLaunchDeps,
  type AgentSessionDeps,
  type AgentLaunchRequest,
  type ConnectedSession,
  type RunPromptParams,
  type SendToSessionParams,
  type OpenWorkspaceArg,
  type WorkspaceStatus,
} from "./dashboard-launch";

/** The role tag on the owned story-agent tab. */
export const STORY_DEV_ROLE = "story-dev";

// A dashboard can replay the same message while React/state catches up. Keep prompt
// delivery single-flight and sticky per live session so retries focus the run without injecting
// the review command twice. Failed delivery is removed and remains retryable.
const deliveredReviewPrompts = new Map<
  string,
  { promise: Promise<void>; status: "pending" | "unknown" | "succeeded" }
>();
const REVIEW_PROMPT_TIMEOUT_MS = 15_000;
const activeReviewOperations = new Map<string, { canceled: boolean }>();

function trimPromptDeliveryTombstones(): void {
  const terminal = [...deliveredReviewPrompts].filter(([, entry]) => entry.status === "succeeded");
  while (terminal.length > 256) {
    const oldest = terminal.shift();
    if (oldest) deliveredReviewPrompts.delete(oldest[0]);
  }
}

export function cancelWorkspaceOpen(operationId: string): void {
  const operation = activeReviewOperations.get(operationId);
  if (operation) operation.canceled = true;
}

export async function injectReviewPromptSingleFlight(
  sessionId: string,
  prompt: string,
  inject: (sessionId: string, line: string) => Promise<void>,
): Promise<void> {
  const key = `${sessionId}\0${prompt}`;
  const existing = deliveredReviewPrompts.get(key);
  if (existing?.status === "unknown") {
    throw new Error(
      "unavailable: review prompt delivery outcome is unknown — Stop this run and start a new review before retrying",
    );
  }
  const unresolvedCount = [...deliveredReviewPrompts.values()].filter(
    (entry) => entry.status !== "succeeded",
  ).length;
  if (!existing && unresolvedCount >= 256) {
    throw new Error("busy: too many review prompt deliveries have an unknown outcome");
  }
  const injection = existing?.promise ?? inject(sessionId, prompt);
  if (!existing) {
    const entry: {
      promise: Promise<void>;
      status: "pending" | "unknown" | "succeeded";
    } = {
      promise: injection,
      status: "pending",
    };
    deliveredReviewPrompts.set(key, entry);
    void injection.then(
      () => {
        const current = deliveredReviewPrompts.get(key);
        if (current === entry) current.status = "succeeded";
        // Retain successful delivery for sticky idempotency. Bound only settled
        // entries; never evict an unknown pending write and permit a duplicate.
        trimPromptDeliveryTombstones();
      },
      () => deliveredReviewPrompts.delete(key),
    );
  }
  let timeoutId: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<void>((_resolve, reject) => {
    timeoutId = setTimeout(() => {
      const current = deliveredReviewPrompts.get(key);
      if (current?.status === "pending") {
        current.status = "unknown";
        trimPromptDeliveryTombstones();
      }
      reject(new Error("timeout: review prompt delivery did not complete"));
    }, REVIEW_PROMPT_TIMEOUT_MS);
  });
  const delivery = Promise.race([injection, timeout]).finally(() => {
    if (timeoutId) clearTimeout(timeoutId);
  });
  return delivery;
}

/**
 * SW-GW-6: the `(teamKey, role)` dedup role for a story session. The MAIN session
 * (no `session`) is `story-dev:<workspaceKey>` — BYTE-IDENTICAL to the pre-GW-6
 * role, so re-opening a story with no label never re-keys (AC-3). A NAMED session
 * suffixes the role with `#<session>` — this is load-bearing: without it,
 * `connectAgentSession`'s liveness-blind `(teamKey, role)` dedup would resolve a
 * named open onto the MAIN session's tab and never spawn a second agent. Pure, so
 * the invariant is unit-asserted without a store/PTY.
 */
export function storySessionRole(workspaceKey: string, session?: string): string {
  return session
    ? `${STORY_DEV_ROLE}:${workspaceKey}#${session}`
    : `${STORY_DEV_ROLE}:${workspaceKey}`;
}

/** SW-DX-18: the role tag on a branch-review agent tab (keyed per branch handle). */
export const BRANCH_REVIEW_ROLE = "branch-review";

/** SW-GW-7: the role tag on a HEADLESS epic agent session (keyed per epic handle). */
export const EPIC_AGENT_ROLE = "epic-agent";

/** SW-GW-7: the role tag on a HEADLESS dir agent session (keyed per dir handle). */
export const DIR_AGENT_ROLE = "dir-agent";

// ── Shared launch-deps session seams ─────────────────────────────────────────
// The story (SW-DX-7), branch (SW-DX-18), and epic/dir-headless (SW-GW-7) deps
// builders drive the SAME launcher/bind/focus wiring — identical modulo the role
// prefix + workspace key. The handle-keyed session seams are wired ONCE in
// `sharedManagedSessionSeams` (below) and spread into all three builders, mirroring
// the `launchAgentSession` spine split on the pure side (`dashboard-launch.ts`).

/** Stamp the handle→session binding AND arm status projection + liveness backstop.
 *  SW-GW-8: `launcher` (the dashboard `docPath`) is recorded as the session's
 *  provenance so the Background-agents panel row shows which surface opened it. */
function bindAndArm(
  handle: string,
  sessionId: string,
  launcher?: string,
  scope?: LaunchBindScope,
): Promise<void> {
  return dashboardLaunchBind(handle, sessionId, launcher, scope);
}

/** Focus the launched terminal: make it the active tab and put the terminal on
 *  screen WITHOUT changing the user's layout mode (SW-LN-B38) — a collapsed pane
 *  expands to the bottom pane; `shared`/`console` stay as they are. */
function focusLaunchedTab(teamKey: string, sessionId: string, workspaceId: string): void {
  const store = useViewerStore.getState();
  store.revealTerminalInPlace(teamKey);
  store.setActiveTerminalTab(teamKey, sessionId, workspaceId);
}

/** Open (or focus) `teamKey`'s generic workspace tile for `cwd` without a shell of its own,
 *  so an agent launched there has a visible home; returns its workspace id. Only a viewer
 *  workspace record renders as a tile: a tab opened under a bare launch handle runs out of
 *  sight. */
function openAgentTile(teamKey: string, cwd: string): { workspaceId: string; created: boolean } {
  const dir = normalizeWorkspaceDir(cwd);
  const workspaceId = genericWorkspaceId(dir);
  const store = useViewerStore.getState();
  const created =
    !(store.workspaces[teamKey]?.some((w) => w.id === workspaceId) ?? false) &&
    !store.softClosedWorkspaces[teamKey]?.[workspaceId];
  store.openWorkspace({
    teamKey,
    workspaceId,
    kind: "generic",
    cwd: dir,
    restoreSoftClosed: true,
    focus: true,
    spawn: false,
  });
  return { workspaceId, created };
}

/** Launch into `teamKey`'s tile for `cwd` when `headed`, else under `workspaceKey` out of
 *  sight. A launch that fails closes the tile it opened, so no empty tile is left behind. */
async function connectInTile<T>(
  headed: boolean,
  teamKey: string,
  cwd: string,
  workspaceKey: string,
  launch: (workspaceKey: string) => Promise<T>,
): Promise<T> {
  if (!headed) return launch(workspaceKey);
  const tile = openAgentTile(teamKey, cwd);
  try {
    return await launch(tile.workspaceId);
  } catch (error) {
    if (tile.created) useViewerStore.getState().closeWorkspace(teamKey, tile.workspaceId);
    throw error;
  }
}

/** Put a headed launch's agent session on screen through the one Show: focus it where a tile
 *  shows it, put a Hidden one back on its tile, else move it from its out-of-sight launch
 *  workspace into the tile for its directory. A session no tab holds yet (its tab is still on
 *  its way) only reveals the terminal column of `teamKey`, the launch's team. */
function revealAgentSession(teamKey: string, sessionId: string): void {
  const row = {
    handle: sessionId,
    sessionId,
    alive: true,
    kind: "dashboard-launched",
    provenance: undefined,
    story: null,
    workspace: null,
  };
  // A launch that just returned is running, whatever an older roster read says.
  const inputs = { ...readShowInputs(row, teamKey), rosterAlive: undefined };
  const plan = resolveAgentShow(row, inputs);
  if (plan.kind === "adopt" && !plan.dir) {
    useViewerStore.getState().revealTerminalInPlace(teamKey);
    return;
  }
  void runShowPlan(plan, row, { name: "The agent", teamKey, stayInTeam: true });
}

// ── SW-GW-5 shared liveness/reap seams (story + branch deps both consume) ─────

/** Is a bound session in a TERMINAL managed state (done / needs-attention)? The
 *  single home of the parked predicate — used by `sendToSession`'s refusal AND
 *  the SW-GW-5 idempotent-reuse gate, so the two can't drift. */
function isBoundSessionParked(sessionId: string): boolean {
  const state = useManagedSessionStore.getState().states[sessionId];
  return state !== undefined && isTerminalManagedState(state);
}

/** Real child-process liveness of an ARBITRARY bound session — the SAME
 *  `try_wait` probe SW-GW-3's status read uses (`dashboardProbeSessionsAlive`),
 *  reused here so the two never diverge. A probe error reads DEAD (heal), never
 *  a false "alive" (which would hand back a corpse). */
function probeBoundSessionAlive(sessionId: string): Promise<boolean> {
  return dashboardProbeSessionsAlive([sessionId])
    .then((alive) => alive.includes(sessionId))
    .catch(() => false);
}

/** How long after its launch an agent may still be on its way up (its CLI not yet in the
 *  shell's process tree), so its session is not judged a bare shell meanwhile. */
const AGENT_START_GRACE_MS = 30_000;

/** Sessions this app just started an agent in, with when. Entries expire after
 *  {@link AGENT_START_GRACE_MS}, so a start that never came up never vouches for its session
 *  forever. */
const recentAgentStarts = new Map<string, number>();

function noteAgentStarted(sessionId: string, now = Date.now()): void {
  recentAgentStarts.set(sessionId, now);
}

/**
 * Whether an agent CLI still runs in a session's shell, so start-or-show may hand it back. A
 * shell whose agent quit is a bare shell: reusing it would "show" an agent that is not
 * there. True while the session's agent is just starting, or when any known agent CLI is in
 * its process tree; false only when every probe says none runs. A probe that cannot say
 * reads as running, so an unreadable session is kept rather than launched twice.
 */
export async function sessionRunsAgent(
  sessionId: string,
  deps: Pick<AdoptDeps, "agentRunning">,
  now = Date.now(),
): Promise<boolean> {
  for (const [sid, at] of recentAgentStarts) {
    if (now - at >= AGENT_START_GRACE_MS) recentAgentStarts.delete(sid);
  }
  if (recentAgentStarts.has(sessionId)) return true;
  try {
    const running = await Promise.all(
      AGENT_PROVIDERS.map((runtime) => deps.agentRunning(sessionId, runtime)),
    );
    return running.some(Boolean);
  } catch {
    return true;
  }
}

/** Whether a session is a shell its agent quit: no agent CLI runs in it and it runs no job
 *  of its own. A person's job in it (or a session nothing can be read of) is never called
 *  idle, so it is never closed out from under them. */
async function isIdleShell(
  sessionId: string,
  deps: Pick<AdoptDeps, "agentRunning" | "runningJobs">,
): Promise<boolean> {
  if (await sessionRunsAgent(sessionId, deps)) return false;
  try {
    const jobs = await deps.runningJobs(sessionId);
    return jobs.foreground == null && jobs.background.length === 0;
  } catch {
    return false;
  }
}

/** A bound session start-or-show may hand back: its PTY is alive, and it is not a shell its
 *  agent quit ({@link isIdleShell}). */
async function probeBoundAgentAlive(sessionId: string): Promise<boolean> {
  if (!(await probeBoundSessionAlive(sessionId))) return false;
  // A headless run has no shell to quit back to: its registry presence is its liveness.
  if (sessionId.startsWith("headless-")) return true;
  return !(await isIdleShell(sessionId, realAdoptDeps));
}

/** Evict a superseded session so a respawn's `(teamKey, role)` lookup misses it
 *  and spawns fresh (see `dashboard-launch.ts`). Session-keyed (the heal holds
 *  only a sessionId): its host closes it, keeping a still-busy one running out
 *  of sight until the heal ends it after the rebind — hidden from this viewer
 *  meanwhile, so the lookup never adopts it back. */
async function evictStaleSessionTab(sessionId: string): Promise<void> {
  await useViewerStore.getState().removeTerminalTabBySession(sessionId);
  useViewerStore.getState().applyLayout({ kind: "dismissed", session: sessionId });
}

/** SW-XS-10: how a launch of `agentKey` into a workspace is recorded for its
 *  setup line's Retry, and how a refusal reaches that line. */
function launchTracking(teamKey: string, workspaceId: string, agentKey: string) {
  const name = useAgentsStore.getState().rows?.find((r) => r.key === agentKey)?.name;
  const names = agentLaunchNames(agentKey, name);
  return {
    teamKey,
    workspaceId,
    names,
    lineWaits: () =>
      useViewerStore.getState().workspaces[teamKey]?.find((w) => w.id === workspaceId)?.setupState
        ?.kind === "awaitingAgent",
    report: (all: readonly string[], reason: string, code?: string) =>
      useViewerStore.getState().failAgentRun(teamKey, workspaceId, all, reason, code),
  };
}

/**
 * Launch (or focus the existing) agent session at `cwd`. `role` must be UNIQUE
 * per workspace — `connectAgentSession` dedups by `(teamKey, role)`, so a second
 * launch never resolves onto the first's session. The backend prepares the named agent
 * for the session once its id is minted; nothing is written into the worktree.
 */
function connectAgent(p: {
  teamKey: string;
  cwd: string;
  role: string;
  workspaceKey: string;
  // SW-GW-6: optional tab title (names the owned tab).
  title?: string;
  agent: AgentLaunchRequest;
}): Promise<ConnectedSession> {
  return connectNamedAgent(p, p.agent);
}

/** The shell launch a prepared agent runs: exactly the backend's args, scope, env strip
 *  and account (its binding as the explicit override, else the runtime's Active account). */
export function shellLaunchFromPrepared(prepared: PreparedAgentLaunch): PreparedShellLaunch {
  const { agentPath, launch, account, scope } = prepared;
  const accountRuntime = AGENT_PROVIDERS.find((provider) => provider === account.runtime);
  return {
    agentPath,
    cwd: prepared.cwd ?? undefined,
    args: launch.args,
    mcpAllow: launch.mcp_allow ?? [],
    bridgeTools: launch.bridge_tools == null ? null : (launch.effective_bridge_tools ?? []),
    extraArgs: scope.extraArgs,
    envRemove: launch.env_suppress ?? GUARDED_ENV_STRIP,
    ...(!accountRuntime
      ? {}
      : {
          account: {
            runtime: accountRuntime,
            overrideAccountId: account.overrideAccountId ?? undefined,
            overrideAccountGeneration: account.overrideAccountGeneration ?? undefined,
          },
        }),
    environmentBinding: scope.environmentBinding ?? undefined,
    execution: scope.execution,
    ...(launch.env ? { env: launch.env } : {}),
    title: prepared.name.trim() || undefined,
  };
}

/** How an adopted role tab is checked and, when its agent quit, retired. */
export interface AdoptDeps {
  /** Whether the agent CLI `agentName` runs in the session (a process-tree probe). Rejects
   *  when the terminal host can't say. */
  agentRunning: (sessionId: string, agentName: string) => Promise<boolean>;
  /** The session's jobs besides its shell: its foreground job and background jobs. */
  runningJobs: (sessionId: string) => Promise<RunningJobs>;
  /** Close a stale tab out of this viewer, so the role lookup misses it: its host ends an
   *  idle shell and keeps a busy one running out of sight. */
  evict: (sessionId: string) => Promise<void>;
}

const realAdoptDeps: AdoptDeps = {
  agentRunning: (sessionId, agentName) => sessionHasLiveAgent(sessionId, agentName),
  runningJobs: async (sessionId) => {
    const [jobs] = await sessionsRunningProcesses([sessionId]);
    if (!jobs) throw new Error("no process read for the session");
    return jobs;
  },
  evict: evictStaleSessionTab,
};

/**
 * Launch a named agent into the workspace terminal: once the session id is minted, the
 * backend prepares the agent for that session (`agent_launch_prepare` — its scope and
 * permission files written for it), and the shell spawns with exactly what came back. An
 * adopted existing session prepares nothing and records no agent.
 */
export async function connectNamedAgent(
  p: { teamKey: string; cwd: string; role: string; workspaceKey: string; title?: string },
  agent: AgentLaunchRequest,
  launchDeps: LaunchDeps = makeRealLaunchDeps(async () => {}),
  prepare: typeof agentLaunchPrepare = agentLaunchPrepare,
  adopt: AdoptDeps = realAdoptDeps,
): Promise<ConnectedSession> {
  let prepared: PreparedAgentLaunch | undefined;
  // A role tab whose agent quit back to an idle shell is not the agent: never hand it back
  // as one. It is closed out of the viewer and a fresh agent starts in its place; a tab the
  // eviction leaves in place is adopted as it is.
  const deps: LaunchDeps = {
    ...launchDeps,
    findExisting: async (teamKey, role) => {
      const hit = await launchDeps.findExisting(teamKey, role);
      if (!hit || !(await isIdleShell(hit.sessionId, adopt))) return hit;
      await adopt.evict(hit.sessionId);
      return launchDeps.findExisting(teamKey, role);
    },
  };
  let result: Awaited<ReturnType<typeof connectAgentSession>>;
  try {
    result = await connectAgentSession(
      {
        teamKey: p.teamKey,
        orgRoot: p.cwd,
        agentPath: "",
        role: p.role,
        workspaceKey: p.workspaceKey,
        title: p.title,
        prepare: async (sessionId) => {
          prepared = await prepare({
            agentKey: agent.agent,
            newInstance: agent.newInstance,
            frameId: agent.frameId,
            pagePath: agent.pagePath,
            team: p.teamKey,
            cwd: p.cwd,
            sessionId,
            // A background session nobody watches gets the agent's headless permissions, so
            // it never waits on a prompt; a visible one the person answers.
            mode: agent.headless ? "headless" : "interactive",
            prompt: agent.prompt,
            accountId: agent.account?.id,
            accountGeneration: agent.account?.generation,
          }).catch((error: unknown) => {
            throw typedPrepareError(error);
          });
          return shellLaunchFromPrepared(prepared);
        },
      },
      deps,
    );
  } catch (error) {
    // SW-AGR-2: a routine (or another Start) is launching this agent in this dir
    // right now: show that session instead of starting a second one.
    const starting = alreadyStarting(error);
    if (starting === undefined) throw error;
    if (starting) return { sessionId: starting, adopted: true };
    throw new Error(
      "unavailable: this agent is already starting here; it shows as soon as it is up",
    );
  }
  if (!prepared) return { sessionId: result.sessionId, adopted: true };
  noteAgentStarted(result.sessionId);
  return {
    sessionId: result.sessionId,
    agentRevision: prepared.agent,
    warnings: prepared.warnings,
    promptInArgv: prepared.promptInArgv,
    defaultPrompt: agent.prompt ? undefined : (prepared.defaultPrompt ?? undefined),
  };
}

/** The backend's typed refusal of a Start that races a launch of the same agent in the
 *  same dir (`already-starting: <session>`): the session to show, `""` while it is still
 *  unbound, or `undefined` for any other error. */
export function alreadyStarting(error: unknown): string | undefined {
  const message = error instanceof Error ? error.message : String(error);
  const match = /^already-starting:\s*(\S*)/.exec(message);
  return match ? match[1] : undefined;
}

/** A failed agent preparation as the typed rejection a caller can branch on: an agent that
 *  cannot be found or used for this launch is `invalid:`; an account that cannot run it is
 *  `unavailable:`. An error that already carries a typed prefix passes through. */
export function typedPrepareError(error: unknown): Error {
  const message = error instanceof Error ? error.message : String(error);
  if (/^[a-z][a-z-]*:/.test(message)) return error instanceof Error ? error : new Error(message);
  if (message.startsWith("agent account is unavailable")) {
    return new Error(`unavailable: ${message}`);
  }
  return new Error(`invalid: ${message}`);
}

/** Submit `text` into a live agent session with the agent-aware submit, keeping whether it
 *  landed. A session whose tab is gone rejects with the typed dead-session reason. */
async function sendLineToSession(sessionId: string, text: string): Promise<{ submitted: boolean }> {
  let outcome;
  try {
    outcome = await injectAgentLine(sessionId, text, undefined, {
      gate: "none",
      submit: true,
    });
  } catch (e) {
    // `injectAgentLine` throws only when the session's tab is gone; a deeper
    // `submit_pty_line` rejection also means the turn couldn't be delivered.
    // Surface the typed dead-session reject (AC-8) but PRESERVE the underlying
    // cause so the audit/log keeps fidelity instead of a flattened message.
    const cause = e instanceof Error ? e.message : String(e);
    throw new Error(`unavailable: session not live${cause ? ` (${cause})` : ""}`);
  }
  return { submitted: outcome.ok };
}

/** The built-in agent CLI path, asked of the backend at each launch that needs it (its
 *  liveness fallback) — never memoized across launches, so a moved or reinstalled CLI is
 *  picked up without restarting the controller. */
const askAgentPathPerLaunch = (): Promise<string | null> => resolveAgentCliPath();

/** Best-effort wait for the launched agent PROCESS to come up (liveness probe): the agent
 *  CLI the session's launch recorded, else the built-in Claude CLI. */
async function waitAgentLive(
  getAgentPath: () => Promise<string | null>,
  sessionId: string,
): Promise<boolean> {
  const launched = useAgentStateStore.getState().launchBySession[sessionId]?.agent;
  if (launched) return waitForAgentLive(sessionId, launched);
  const agentPath = await getAgentPath();
  if (!agentPath) return false;
  return waitForAgentLive(sessionId, agentBasename(agentPath));
}

/**
 * The kind-AGNOSTIC session seams every launch spine shares — the handle-keyed Rust
 * registry commands (`begin`/`abort`/`bind`/`resolveSession`), the reap choke
 * (`killSession`), and the SW-GW-5 liveness/reap probes
 * (`isSessionParked`/`probeAlive`/`evictStaleTab`) + the process liveness wait
 * (`waitLive`). Every seam is byte-identical across the story (SW-DX-7), branch
 * (SW-DX-18), and epic/dir-headless (SW-GW-7) builders — they differ ONLY in the
 * per-kind `resolve`/`ensureCwd`/`connect`/`focus`. Wired ONCE here so the three
 * builders can't drift; each spreads this and adds its own kind-specific seams.
 *
 * - `begin`/`abort` (AC-8): reserve / free a cap slot, keyed by the opaque handle.
 * - `killSession` (SW-DX-17b AC-8): end an orphaned/superseded session through its
 *   host (`terminal_close_tab(Kill)` → the one `kill_session` choke: frees any cap
 *   slot + audits).
 * - `bind` (AC-5): stamp `handle → sessionId` AND arm status projection + liveness.
 * - `isSessionParked`/`probeAlive`/`evictStaleTab` (SW-GW-5): the guaranteed-live
 *   idempotent-re-open gate + stale-tab eviction.
 */
function sharedManagedSessionSeams(
  getAgentPath: () => Promise<string | null>,
  launcher: string,
): Pick<
  AgentSessionDeps,
  | "begin"
  | "abort"
  | "killSession"
  | "resolveSession"
  | "isSessionParked"
  | "probeAlive"
  | "evictStaleTab"
  | "waitLive"
> & { bind: DashboardLaunchDeps["bind"] } {
  return {
    begin: (h) => dashboardLaunchBegin(h),
    abort: (h) => dashboardLaunchAbort(h),
    killSession: (sessionId) => terminalCloseTab(sessionId, "kill").then(() => undefined),
    resolveSession: (h) => dashboardResolveSession(h).catch(() => null),
    isSessionParked: isBoundSessionParked,
    probeAlive: probeBoundAgentAlive,
    evictStaleTab: evictStaleSessionTab,
    // SW-GW-8: every session launched through the dashboard controller records
    // its host `docPath` as provenance (the panel's "which surface opened this").
    bind: (h, sessionId, scope) => bindAndArm(h, sessionId, launcher, scope),
    waitLive: (sessionId) => waitAgentLive(getAgentPath, sessionId),
  };
}

/** New terminal uses the shared managed spine while keeping the chosen workspace.
 * The directory launcher creates a generic tile, so it cannot serve this entry unchanged. */
export function launchAgentInWorkspace(input: {
  teamKey: string;
  workspaceKey: string;
  agent: string;
  /** Where in the workspace it starts (a tab's own folder); the workspace's folder otherwise. */
  cwd?: string;
}) {
  const { teamKey, workspaceKey, agent } = input;
  const workspace = useViewerStore
    .getState()
    .workspaces[teamKey]?.find((row) => row.id === workspaceKey);
  if (!workspace) return Promise.reject(new Error("The workspace is no longer open"));
  const cwd = input.cwd ?? workspace.cwd;
  const stillHere = whereTheUserIs(teamKey);
  const role = `terminal:${crypto.randomUUID()}`;
  const handle = dirHandle(cwd, role);
  return launchAgentSession(handle, true, {
    ...sharedManagedSessionSeams(askAgentPathPerLaunch, "new-terminal"),
    bind: (h, sessionId, agentRevision) =>
      bindAndArm(h, sessionId, "new-terminal", { teamKey, agentRevision }),
    ensureCwd: async () => cwd,
    connect: (path) =>
      connectNamedAgent({ teamKey, cwd: path, workspaceKey, role }, { agent, newInstance: true }),
    sendLine: sendLineToSession,
    focus: (sessionId) => {
      focusLaunchedTab(teamKey, sessionId, workspaceKey);
      // Use the same grid placement and focus boundary as ordinary new tabs.
      void showNewTab(
        teamKey,
        workspaceKey,
        sessionId,
        () =>
          getActiveTeamKey() === teamKey &&
          useViewerStore.getState().activeWorkspace[teamKey] === workspaceKey &&
          stillHere(),
      ).catch(() => {});
    },
    emptyCwdError: "The workspace has no working directory",
  });
}

/**
 * SW-GW-7: build the kind-agnostic {@link AgentSessionDeps} for the shared
 * {@link launchAgentSession} spine — the shared session seams plus the per-kind
 * `ensureCwd` (worktree/epic-dir/plain-dir), `connect`/`focus` (keyed by `teamKey`
 * + `handle` + `role`), and the empty-cwd reject. Used by the epic-headless +
 * dir-headless launchers; story/branch keep their own richer deps (preview/chat
 * seams the spine doesn't need) but share the SAME session seams via the helper.
 */
function makeManagedAgentSessionDeps(opts: {
  teamKey: string;
  handle: string;
  role: string;
  ensureCwd: () => Promise<string>;
  emptyCwdError: string;
  getAgentPath: () => Promise<string | null>;
  /** SW-GW-8: the launching dashboard's `docPath` (session provenance). */
  launcher: string;
  /** The agent the session runs. */
  agent: AgentLaunchRequest;
  /** Open the session's tab in a visible tile for its cwd; otherwise it runs out of sight. */
  headed?: boolean;
}): AgentSessionDeps {
  const { teamKey, handle, role, ensureCwd, emptyCwdError, getAgentPath, launcher, agent, headed } =
    opts;
  return {
    ...sharedManagedSessionSeams(getAgentPath, launcher),
    // The workspace's team is where the agent runs.
    bind: (h, sessionId, agentRevision) =>
      bindAndArm(h, sessionId, launcher, { teamKey, agentRevision }),
    ensureCwd,
    // Role UNIQUE per workspace (`${role}:${handle}`), like the story/branch spines.
    connect: (cwd) =>
      connectInTile(headed === true, teamKey, cwd, handle, (workspaceKey) =>
        connectAgent({
          teamKey,
          cwd,
          role: `${role}:${handle}`,
          workspaceKey,
          agent: headed === true ? agent : { ...agent, headless: true },
        }),
      ),
    sendLine: sendLineToSession,
    // The spine focuses only a headed open; a re-open of a session still out of sight
    // brings it into its tile.
    focus: (sessionId) => revealAgentSession(teamKey, sessionId),
    emptyCwdError,
  };
}

/**
 * SW-GW-7: the injected background-session launchers for the epic + dir HEADLESS
 * arms of `dispatchOpenWorkspace`. Kept as an injected seam (like the other launch
 * deps) so the dispatch stays pure/unit-testable — the real ones (asking the backend
 * for the agent CLI path per launch) are built in `makeDashboardLaunchController`.
 */
export interface HeadlessSessionLaunchers {
  /** Background a first-class agent session in the resolved epic workspace dir —
   *  NOT the visible N-tile assembly. `ensureEpicWorkspace` materializes the dir;
   *  the agent gets its MCP bridge per session. `agent` is the agent it runs. */
  openEpicHeadless: (target: EpicOpenTarget, agent: string) => Promise<{ sessionId: string }>;
  /** Background a first-class agent session in an existing `path` (an agent tab,
   *  NOT the SW-GW-1 shell tile). The dir is NOT written to (materialize-safety
   *  invariant): the agent gets its MCP bridge per session. `opts.agent` is the agent it
   *  runs; `opts.prompt` is seeded once the agent is live; `opts.headed` opens its tab in
   *  the active team's generic workspace tile for `path` and focuses it (default:
   *  background, no tile). */
  openDirHeadless: (
    path: string,
    opts: { agent: string; prompt?: string; headed?: boolean; session?: string },
  ) => Promise<{ sessionId: string; promptDelivered?: boolean; warnings?: AgentLaunchWarning[] }>;
}

/** SW-GW-7: the live epic/dir headless launchers (mirrors
 *  `makeRealDashboardLaunchDeps`/`…BranchLaunchDeps`). */
export function makeRealHeadlessLaunchers(
  docPath: string,
  frameId?: string,
): HeadlessSessionLaunchers {
  const getAgentPath = askAgentPathPerLaunch;
  return {
    openEpicHeadless: (target, agent) => {
      const ctx = resolveEpicCommandContext({ orgSlug: target.orgSlug, team: target.team });
      if (!ctx) {
        throw new Error(`unavailable: unknown or unresolved team '${target.teamKey}'`);
      }
      const base = { ...ctx, epicFilePath: target.epicFilePath };
      // Handle = the epic's canonical file path — the SAME key `getEpicHandle()` /
      // `sendToSession` address the epic by, so a headless epic agent is reachable.
      return launchAgentSession(
        target.epicFilePath,
        false,
        makeManagedAgentSessionDeps({
          teamKey: target.teamKey,
          handle: target.epicFilePath,
          role: EPIC_AGENT_ROLE,
          // Materialize the epic dir ONLY; skip the heavy per-repo
          // `assembleEpicWorktrees` + the visible tile — that's the headless win (AC-8).
          ensureCwd: () => ensureEpicWorkspace(base).then((h) => h.dirPath),
          emptyCwdError: `unavailable: epic workspace did not open for "${target.epicFilePath}"`,
          getAgentPath,
          launcher: docPath,
          agent: { agent, frameId, pagePath: docPath },
        }),
      );
    },
    openDirHeadless: (path, opts) => {
      const teamKey = getActiveTeamKey();
      // `getActiveTeamKey()` returns the TRUTHY `NO_TEAM_SENTINEL` (`_no_team:_none`)
      // when no org is active, so `if (!teamKey)` alone silently binds the launch
      // into the sentinel bucket and the resulting row never surfaces in the active
      // team's roster — the launch looks like a no-op. Reject both
      // no-team forms via `teamKeyHasTeam` so the caller's toast surfaces instead.
      if (!teamKey || !teamKeyHasTeam(teamKey)) {
        throw new Error("unavailable: no active team for a headless dir open");
      }
      const trimmed = typeof path === "string" ? path.trim() : "";
      // Reject an empty path UP FRONT — before minting a handle / reserving a cap
      // slot (code-review r1: an empty `dirHandle("")` would otherwise `begin("")`
      // then self-abort via `emptyCwdError`, needlessly churning the "" cap slot).
      if (!trimmed) {
        throw new Error("unavailable: dir target needs a non-empty { path }");
      }
      const handle = dirHandle(trimmed, opts.session);
      const prompt = opts.prompt?.trim() ? opts.prompt : undefined;
      const managed = makeManagedAgentSessionDeps({
        teamKey,
        handle,
        role: DIR_AGENT_ROLE,
        // The dir already exists — no materialize, no write into it (materialize-
        // safety invariant).
        ensureCwd: async () => trimmed,
        emptyCwdError: "unavailable: dir target needs a non-empty { path }",
        getAgentPath,
        launcher: docPath,
        agent: { agent: opts.agent, prompt, frameId, pagePath: docPath },
        headed: opts.headed === true,
      });
      // Every launch brings its MCP access and status hooks in its own per-session files,
      // so nothing is written into the dir (it may be a git-synced context folder).
      return launchAgentSession(handle, opts.headed === true, managed, {
        initialPrompt: prompt,
      });
    },
  };
}

/**
 * Build the live deps for a launch scoped to `docPath` (the dashboard doc, for
 * the preview). `teamKey` is resolved from the active team; the story path comes
 * trusted-side from the Rust frontmatter-cache resolver (AC-11).
 */
export function makeRealDashboardLaunchDeps(
  docPath: string,
  frameId?: string,
): DashboardLaunchDeps {
  const getAgentPath = askAgentPathPerLaunch;

  return {
    // SW-GW-7: the handle-keyed session seams (begin/abort/bind/reap/probe/waitLive)
    // shared with the branch + epic/dir-headless spines — see the helper.
    // SW-GW-8: `docPath` is the session's launcher provenance.
    ...sharedManagedSessionSeams(getAgentPath, docPath),
    resolveStory: async (storyId) => {
      const activeTeamKey = getActiveTeamKey();
      if (!activeTeamKey) return null;
      // AC-11: the path is resolved from the managed frontmatter cache — never a
      // caller-supplied path. Pair the story's team with the active org slug.
      try {
        const { storyFilePath, team } = await resolveStoryPath(storyId, frameId, docPath);
        const orgSlug = activeTeamKey.split(":")[0];
        return { storyFilePath, teamKey: `${orgSlug}:${team}` };
      } catch {
        return null;
      }
    },
    resolveAccountProvenance: async (handle) => {
      const row = (await dashboardListSessions()).find((candidate) => candidate.handle === handle);
      return row?.accountProvenance ?? null;
    },
    ensureWorktree: (teamKey, storyFilePath) => {
      // SW-XS-10: an agent launch follows this workspace's Step 1, so its line
      // waits for that agent's preparation instead of going quiet.
      const workspaceId = storyIdFromPath(storyFilePath);
      expectAgentLaunch(teamKey, workspaceId);
      return useViewerStore
        .getState()
        .ensureWorktree(teamKey, storyFilePath)
        .catch((e: unknown) => {
          // No launch follows a create that failed: never leave it expected.
          forgetAgentLaunch(teamKey, workspaceId);
          throw e;
        });
    },
    remoteWorkspace: (teamKey, storyFilePath) => {
      const workspaceId = storyIdFromPath(storyFilePath);
      const ws = useViewerStore
        .getState()
        .workspaces[teamKey]?.find((w) => w.id === workspaceId && isRemoteWorkspace(w));
      return isRemoteWorkspace(ws) ? { workspaceId, location: ws.location } : null;
    },
    placeWorkspace: (teamKey, storyFilePath, machineId) => {
      const workspaceId = storyIdFromPath(storyFilePath);
      expectAgentLaunch(teamKey, workspaceId);
      return placeStoryWorkspace(teamKey, storyFilePath, machineId).catch((e: unknown) => {
        forgetAgentLaunch(teamKey, workspaceId);
        throw e;
      });
    },
    launchRemote: (p) => {
      const driver = remoteWorkspaceDriver();
      if (!driver) return Promise.reject(new Error("unavailable: machines are unavailable"));
      // SW-XS-10: a failed Step 2's Retry launches this agent here again.
      return trackAgentLaunch(launchTracking(p.teamKey, p.workspaceId, p.agentKey), () =>
        driver.launchAgent({ ...p, frameId, pagePath: docPath }),
      );
    },
    awaitEstablished: (cwd) => awaitWorktreeEstablished(cwd),
    awaitContext: (cwd) => awaitContextForLaunch(cwd),
    // Role keyed by the story (see `connectAgent` — unique per workspace).
    // SW-GW-6: `workspaceKey` stays the BARE storyId so all of a story's tabs
    // (main + named) share ONE workspace — focus/layout and
    // the SW-GW-5 eviction keep working; only role/sessionId/name differ per tab.
    // The load-bearing detail: a NAMED session (`session` present) MUST carry a
    // `#<session>`-suffixed role or `connectAgentSession`'s liveness-blind
    // `(teamKey, role)` dedup would return the MAIN session's tab and never spawn.
    connect: ({ teamKey, cwd, storyFilePath, session, title, agent }) => {
      const workspaceKey = storyIdFromPath(storyFilePath);
      const launch = () =>
        connectAgent({
          teamKey,
          cwd,
          role: storySessionRole(workspaceKey, session),
          workspaceKey,
          title,
          agent: { ...agent, frameId, pagePath: docPath },
        });
      // SW-XS-10: a failed Step 2's Retry launches this agent here again.
      return trackAgentLaunch(launchTracking(teamKey, workspaceKey, agent.agent), launch);
    },
    focus: (teamKey, sessionId, storyFilePath) =>
      focusLaunchedTab(teamKey, sessionId, storyIdFromPath(storyFilePath)),
    // Fire into the live agent (gate "none") with the reliable agent-aware
    // submit (the shared re-orient path).
    injectLine: async (sessionId, line) => {
      await injectAgentLine(sessionId, line, undefined, { gate: "none", submit: true });
    },
    // ── SW-DX-10 send-half seams ──────────────────────────────────────────────
    // Reuse the SAME agent-aware submit primitive as `runPrompt`, but KEEP its
    // outcome: `submit:true` surfaces whether the line actually landed (AC-7).
    // `injectAgentLine` throws only when the session's tab is gone → map that to
    // the typed dead-session reject (AC-8); a written-but-held line returns
    // `{ok:false}` → `submitted:false` (surfaced, not dropped).
    sendLine: sendLineToSession,
    // AC-8: refuse a session already in a terminal managed state (done /
    // needs-attention) — a chat turn into it would be lost.
    isSessionParked: isBoundSessionParked,
    // AC-1: echo the sent turn into the shared transcript store (the `"user"`
    // role the store already models) so the in-app chat surface shows it at once.
    echoUserTurn: (sessionId, text) => {
      useAgentChatStore.getState().appendMessage(sessionId, "user", text);
    },
    // SW-GW-5: the guaranteed-live idempotent-re-open seams — REUSED by
    // SW-GW-10's close (probe liveness + evict tab share the heal's shape).
    probeAlive: probeBoundAgentAlive,
    evictStaleTab: evictStaleSessionTab,
    // SW-GW-10 close-half seam: the one authoritative kill choke.
    closeByHandle: (target) => dashboardKillByHandle(target),
  };
}

/**
 * SW-DX-18: the live deps for a `branch` open, scoped to `docPath`. Reuses the
 * handle-agnostic Rust registry commands + the shared agent-session-launcher (as
 * the story path does); only the resolve/checkout differ. `resolveBranch` reads
 * the active team's install context (the SAME `resolveEpicCommandContext`
 * snapshot the epic path uses) and the repo's local clone from the org store.
 */
export function makeRealBranchLaunchDeps(docPath: string, frameId?: string): BranchLaunchDeps {
  const getAgentPath = askAgentPathPerLaunch;

  return {
    // SW-GW-7: the SAME handle-keyed session seams the story + epic/dir-headless
    // spines use (begin/abort/bind/reap/probe/waitLive) — see the helper.
    // SW-GW-8: `docPath` is the session's launcher provenance.
    ...sharedManagedSessionSeams(getAgentPath, docPath),
    resolveBranch: async (repo) => {
      const activeTeamKey = getActiveTeamKey();
      if (!activeTeamKey) return null;
      const { orgSlug, teamName } = parseTeamKey(activeTeamKey);
      const team = teamName ?? "_none";
      // Install context (codesenseHome + githubUserId) — the SAME snapshot read
      // the epic path uses; `null` when org/user/team isn't fully resolved.
      const ctx = resolveEpicCommandContext({ orgSlug, team });
      if (!ctx) return null;
      // The `repo` slug → its local clone path, via the org store's synthetic key.
      const slash = repo.indexOf("/");
      if (slash <= 0 || slash === repo.length - 1) return null;
      const slug = { owner: repo.slice(0, slash), repo: repo.slice(slash + 1) };
      const repoPath = resolveRealRepoPath(syntheticRepoKey(orgSlug, team, slug));
      if (!repoPath) return null;
      return {
        repoPath,
        teamKey: activeTeamKey,
        codesenseHome: ctx.codesenseHome,
        githubUserId: ctx.githubUserId,
        orgSlug: ctx.orgSlug,
        team: ctx.team,
      } satisfies BranchResolution;
    },
    ensureBranchWorktree: (res, target) =>
      ensureBranchWorkspace({
        codesenseHome: res.codesenseHome,
        githubUserId: res.githubUserId,
        orgSlug: res.orgSlug,
        team: res.team,
        repoPath: res.repoPath,
        repoSlug: target.baseRepo ?? target.repo,
        branch: target.branch,
        reviewHeadRepo: target.repo,
        pullNumber: target.pullNumber,
        checkout: target.checkout,
      }).then((h) => h.dirPath),
    reconcileStory: (storyId, cwd, sourceRepo) =>
      reconcileManagedStory(storyId, cwd, sourceRepo, {
        resolve: resolveStoryPath,
        establish: establishWorktreeStoryLink,
      }),
    // Role keyed by the branch handle (unique per workspace — see `connectAgent`).
    connect: ({ teamKey, cwd, workspaceKey, headed, agent }) =>
      connectInTile(headed, teamKey, cwd, workspaceKey, (tileKey) =>
        connectAgent({
          teamKey,
          cwd,
          role: `${BRANCH_REVIEW_ROLE}:${workspaceKey}`,
          workspaceKey: tileKey,
          agent: { ...agent, frameId, pagePath: docPath },
        }),
      ),
    sendLine: sendLineToSession,
    focus: (teamKey, sessionId) => revealAgentSession(teamKey, sessionId),
  };
}

export async function reconcileManagedStory(
  storyId: string,
  cwd: string,
  sourceRepo: string,
  deps: {
    resolve: (storyId: string) => Promise<{ storyFilePath: string }>;
    establish: (
      cwd: string,
      storyFilePath: string,
      storyId: string,
      sourceRepo: string,
    ) => Promise<unknown>;
  },
): Promise<boolean> {
  let storyFilePath: string;
  try {
    ({ storyFilePath } = await withReviewPhaseTimeout(
      deps.resolve(storyId),
      "story reconciliation lookup",
    ));
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    // SW-CR-9 (R2): both unresolvable-story refusals (unknown id OR a candidate
    // on disk whose frontmatter won't index) are "nothing to reconcile" from
    // this call site's perspective — skip rather than throw. Match on the
    // first line so a hint on line 2 (SW-CR-9) doesn't change the branch.
    const firstLine = message.split(/\r?\n/, 1)[0] ?? message;
    if (
      firstLine.startsWith("unavailable: no managed story with id '") ||
      firstLine.startsWith("unavailable: story file candidate at ")
    )
      return false;
    throw error;
  }
  await deps.establish(cwd, storyFilePath, storyId, sourceRepo);
  return true;
}

/**
 * SW-DX-11 §5: resolve the `epicDocPath` handle of the epic workspace that
 * CONTAINS `docPath` (a dashboard sits next to its epic's README in the same
 * dir), or `null` when the doc isn't inside an epic. Trusted-side (reads the
 * viewer store, not doc input), so a doc gets its epic handle without hard-coding
 * a machine-specific absolute path — then `sendToSession(target: <this>)` lands
 * in the epic's running agent via the SAME generalized `resolve()` as a story.
 */
/**
 * SW-GW-3: resolve a story's live workspace state by `storyId`, trusted-side.
 * Open-state comes from the viewer-store open list (`findOpenStoryWorkspace` —
 * launch-source-agnostic, so a manually-opened story is found too, the origin
 * bug); liveness is the REAL arbitrary-session `try_wait` probe (never "a tab
 * exists"). Not docPath-scoped: a board legitimately observes many story rows,
 * and the answer only ever reveals whether the USER's own workspace is open —
 * caller JS supplies only a storyId and can't enumerate anything else.
 */
export async function resolveWorkspaceStatusForStory(storyId: string): Promise<WorkspaceStatus> {
  const state = useViewerStore.getState();
  // Prefer the ACTIVE org's open workspace on a cross-org same-id collision, so
  // this agrees with the org-scoped launch side (resolveStoryPath).
  // Hidden tabs count: a story agent hidden to the tray is still running.
  const found = findOpenStoryWorkspace(
    state.workspaces,
    selectRunningTerminalTabIndex(state),
    storyId,
    getActiveTeamKey(),
  );
  if (!found) return { exists: false, live: false, sessionId: null };
  const { sessionId } = found;
  if (!sessionId) return { exists: true, live: false, sessionId: null };
  // Real liveness — arbitrary session (may be a manual open, never in the
  // registry), so probe PtyState directly, not the registry-filtered list.
  const alive = await dashboardProbeSessionsAlive([sessionId]);
  return { exists: true, live: alive.includes(sessionId), sessionId };
}

export function resolveEpicHandleForDoc(docPath: string): string | null {
  const docDir = dirOfPath(docPath);
  const { workspaces } = useViewerStore.getState();
  for (const teamWs of Object.values(workspaces)) {
    for (const ws of teamWs) {
      if (ws.kind === "epic" && ws.epicDocPath && dirOfPath(ws.epicDocPath) === docDir) {
        return ws.epicDocPath;
      }
    }
  }
  return null;
}

// The parent-directory of a path; `""` for a slash-less input (NOT `slice(0,-1)`,
// which would drop the last char and could false-match — review r1). The single
// dir-of helper for this module (epic-handle match + the doc's own dir anchor).
const dirOfPath = (p: string) => {
  const i = p.lastIndexOf("/");
  return i < 0 ? "" : p.slice(0, i);
};

// SW-DX-15: the epic mainDoc precedence — an EXACT mirror of the scanner's
// `MAIN_DOC_DEFAULTS` (pipeline_scanner.rs:703), case-insensitive first-match:
// README before index, then .md → .html → .htm. Selecting by this order (not by
// directory sort) is what makes a dashboard-open converge on the SAME
// `epicFilePath` — hence the same `integration:${epicFilePath}` id — as the nav,
// so a dir with both `readme.md` and `index.md` never double-opens.
const MAIN_DOC_PRIORITY = [
  "readme.md",
  "index.md",
  "readme.html",
  "index.html",
  "readme.htm",
  "index.htm",
] as const;
const MAIN_DOC_SET = new Set<string>(MAIN_DOC_PRIORITY);

/**
 * SW-DX-15: is `name` one of the default epic mainDoc names (README/index ×
 * pipeline ext md/html/htm)? Mirrors the name set Rust `is_epic_main_doc_name`
 * (frontmatter_editor.rs:135) / `detect_format` gate on. The extension check is
 * load-bearing: the doc-scoped dir list returns entries UNFILTERED (the ext
 * whitelist only gates file reads), so `readme.txt` / `index.json` must not be
 * mistaken for the epic doc.
 */
export function isEpicMainDocName(name: string): boolean {
  return MAIN_DOC_SET.has(name.toLowerCase());
}

/**
 * SW-DX-15: resolve the board's OWN containing epic into an `EpicOpenTarget`.
 * The `epicFilePath` MUST be the epic's canonical mainDoc (not the board's own
 * `docPath`) so a dashboard-open converges on the SAME integration id as the nav
 * (no double-open). Resolved with NO new Tauri command: list the board's own dir
 * via the existing doc-scoped `readWorkspaceFile` and pick the README/index by
 * the scanner's `MAIN_DOC_PRIORITY` order. Returns `null` when there's no active
 * team or the doc isn't in an epic dir.
 *
 * Known limitation: a per-repo CONFIGURED custom mainDoc name (e.g. `Epic.md`,
 * which the scanner honors before the defaults) is not replicated here — that
 * config isn't available controller-side. Such an epic falls through to `null`
 * and the board honest-degrades; default-named epics (the norm) converge exactly.
 */
export async function resolveOwnEpicTarget(
  docPath: string,
  frameId?: string,
): Promise<EpicOpenTarget | null> {
  const activeTeamKey = getActiveTeamKey();
  if (!activeTeamKey) return null;
  const epicDir = dirOfPath(docPath);
  if (!epicDir) return null;
  const epicDirName = epicDir.replace(/\/+$/, "").split("/").pop() || epicDir;
  let entries;
  try {
    entries = (await readWorkspaceFile(docPath, "./", [...READ_WORKSPACE_EXTS], frameId)).entries;
  } catch {
    return null;
  }
  // Match the scanner's ordered precedence, not directory sort order.
  const byLower = new Map<string, string>();
  for (const e of entries ?? []) {
    if (e.isDir) continue;
    const k = e.name.toLowerCase();
    if (!byLower.has(k)) byLower.set(k, e.name); // stable: first occurrence wins
  }
  let mainDocName: string | undefined;
  for (const cand of MAIN_DOC_PRIORITY) {
    const hit = byLower.get(cand);
    if (hit) {
      mainDocName = hit;
      break;
    }
  }
  if (!mainDocName) return null;
  const { orgSlug, teamName } = parseTeamKey(activeTeamKey);
  return {
    teamKey: activeTeamKey,
    orgSlug,
    team: teamName ?? "_none",
    epicDirName,
    epicFilePath: `${epicDir}/${mainDocName}`,
  };
}

/**
 * SW-DX-16: resolve an EXPLICIT `{team, epic}` into an `EpicOpenTarget` — the
 * foreign-epic sibling of `resolveOwnEpicTarget`. Unlike the own-epic path (which
 * lists the board's OWN dir), a foreign epic is invisible to the doc-scoped
 * `readWorkspaceFile` wall, so the mainDoc is resolved TRUSTED-SIDE via
 * `resolveEpicPath` (canonicalize + confine + precedence). The command context
 * comes from the FE's org store (`resolveEpicCommandContext`), never doc input,
 * so a doc can only name a team/org the USER has. Typed rejects
 * (`unavailable:`/`traversal:`/`unmanaged:`) propagate to the board unchanged.
 */
export async function resolveExplicitEpicTarget(
  teamKey: string,
  epic: string,
  frameId?: string,
  page?: string,
): Promise<EpicOpenTarget> {
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  const team = teamName ?? "_none";
  const ctx = resolveEpicCommandContext({ orgSlug, team });
  if (!ctx) {
    throw new Error(`unavailable: unknown or unresolved team '${teamKey}'`);
  }
  const res = await resolveEpicPath(
    ctx.codesenseHome,
    ctx.githubUserId,
    orgSlug,
    team,
    epic,
    frameId,
    page,
  );
  return {
    teamKey,
    orgSlug,
    team,
    epicDirName: res.epicDirName,
    epicFilePath: res.epicFilePath,
  };
}

/**
 * SW-DX-15: dispatch the generic `openWorkspace` primitive by `target.kind`.
 * `story` → the shipped `openStoryWorkspace` (returns `{sessionId}`);
 * `epic` → the shared `openEpicWorkspaceFor` (returns `{}` — the board re-probes
 * `getEpicHandle()`). SW-DX-16: an `epic` target with `{team, epic}` opens that
 * EXPLICIT epic (trusted-side resolve); without them, the board's OWN epic (DX-15).
 *
 * SW-GW-7: `headed` (default `true`) is the orthogonal background option, now valid
 * for EVERY kind. `headed:false` routes to a background agent SESSION — for story/branch the
 * same session spine as headed (focus suppressed); for epic/dir a dedicated headless
 * launcher (`headlessLaunchers`) that backgrounds ONE MCP-connected agent instead of
 * the visible epic tile / dir shell (which the headed arms still open unchanged, AC-5).
 * The heavy tile/assembly work is skipped on the headless path — no UI flash (AC-8).
 */
export async function dispatchOpenWorkspace(
  arg: OpenWorkspaceArg,
  docPath: string,
  deps: DashboardLaunchDeps,
  branchDeps: BranchLaunchDeps,
  dirDeps: DirLaunchDeps,
  headlessLaunchers: HeadlessSessionLaunchers,
): Promise<{
  sessionId?: string;
  promptDelivered?: boolean;
  reconcileStatus?: "reconciled" | "skipped";
  warnings?: AgentLaunchWarning[];
}> {
  const { target, headed, initialPrompt, sessionMode, reviewPrompt, account } = arg;
  if (target.kind === "story") {
    if ("storyPath" in target)
      throw new Error("openWorkspace: story paths must use the native workspace opener");
    // SW-GW-6: thread the optional named-session label and tab title. Both absent → the
    // story's single main session, unchanged.
    // SW-GW-9: story is the only agent-backed session-returner at call time, so it is
    // also the only target that carries `initialPrompt` through to the seed (the bridge
    // rejects it for the other kinds). The returned `promptDelivered` flows straight up.
    // SW-DX-17e: `sessionMode` is likewise story-only (the bridge rejects `new` for the
    // other kinds); `openStoryWorkspace` mints an anonymous fork when it is `new`.
    return openStoryWorkspace(target.storyId, deps, {
      headed,
      session: target.session,
      title: target.title,
      initialPrompt,
      sessionMode,
      account,
      agent: target.agent,
      machineId: target.machineId,
    });
  }
  if (target.kind === "branch") {
    // SW-GW-7: `headed:false` backgrounds the branch-review agent via the SAME
    // session spine (focus suppressed) — the branch spine is already session-based.
    const branchTarget: BranchOpenTarget = {
      repo: target.repo,
      baseRepo: target.baseRepo,
      pullNumber: target.pullNumber,
      branch: target.branch,
      linkedStoryId: target.linkedStoryId,
      checkout: target.checkout,
    };
    const operation = arg.operationId ? { canceled: false } : undefined;
    if (arg.operationId) {
      if (activeReviewOperations.has(arg.operationId)) {
        throw new Error("busy: duplicate review operation");
      }
      activeReviewOperations.set(arg.operationId, operation as { canceled: boolean });
    }
    const throwIfCanceled = () => {
      if (operation?.canceled) throw new Error("canceled: review open canceled before launch");
    };
    let reused = false;
    try {
      // The backend prepares the agent for the session it spawns.
      const opened = await openBranchWorkspace(branchTarget, branchDeps, {
        headed,
        agent: {
          agent: launchAgentKey(target.agent, headed !== false),
          prompt: reviewPrompt,
          account: account ? { id: account.id, generation: account.generation } : undefined,
        },
        isCanceled: operation ? () => operation.canceled : undefined,
        onPhase: (phase) => publishReviewOpenPhase(arg.operationId, phase),
        onReuse: () => {
          reused = true;
          publishReviewOpenPhase(arg.operationId, "joining");
        },
        onOwnershipAcquired: () => {
          reused = false;
        },
      });
      throwIfCanceled();
      if (reviewPrompt) {
        await injectReviewPromptSingleFlight(opened.sessionId, reviewPrompt, deps.injectLine);
        // Prompt delivery is the commit point for a reused session: we do not
        // own that pre-existing run and must not report Canceled while leaving
        // a successfully delivered review command executing in it.
        if (!reused) throwIfCanceled();
        publishReviewOpenPhase(arg.operationId, "reviewing");
      }
      return {
        ...opened,
        ...(reviewPrompt ? { reconcileStatus: opened.reconcileStatus ?? "skipped" } : {}),
        promptDelivered: Boolean(reviewPrompt),
      };
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      const readinessFailed =
        message.includes("agent readiness") ||
        message.includes("review agent did not become ready");
      if ((operation?.canceled || readinessFailed) && !reused) {
        const handle = branchTargetHandle(target);
        const sessionId = await deps.resolveSession(handle).catch(() => null);
        if (sessionId) await deps.evictStaleTab(sessionId);
        await deps.closeByHandle(handle).catch(() => {});
      }
      if (operation?.canceled) {
        throw new Error("canceled: review open canceled during prompt delivery");
      }
      throw error;
    } finally {
      if (arg.operationId) activeReviewOperations.delete(arg.operationId);
    }
  }
  if (target.kind === "dir") {
    // SW-GW-7: a HEADLESS dir open backgrounds an agent session in the dir (an agent
    // tab, not the shell) — a DIFFERENT path than the headed synchronous shell tile.
    // A named agent always launches an agent session in the dir: headed, as a focused tab
    // in the dir's generic workspace tile; headless, in the background. A headless open
    // with no agent named runs the unattended built-in Claude agent.
    if (target.agent?.trim() || headed === false) {
      return headlessLaunchers.openDirHeadless(target.path, {
        agent: launchAgentKey(target.agent, headed !== false),
        prompt: initialPrompt,
        headed: headed !== false,
        session: target.session,
      });
    }
    // SYNCHRONOUS open (SW-GW-1): the tile paints before this resolves (AC-8).
    return openDirWorkspace({ path: target.path }, dirDeps);
  }
  // `epic` — an EXPLICIT `{team, epic}` (SW-DX-16) or the board's OWN epic (DX-15).
  // Resolve the target FIRST (both the headed tile and the headless session need it).
  let epicTarget: EpicOpenTarget | null;
  if (target.team !== undefined && target.epic !== undefined) {
    epicTarget = await resolveExplicitEpicTarget(target.team, target.epic, arg.frameId, docPath);
  } else {
    epicTarget = await resolveOwnEpicTarget(docPath, arg.frameId);
    if (!epicTarget) {
      throw new Error("unavailable: this dashboard is not inside an epic workspace dir");
    }
  }
  // SW-GW-7: a HEADLESS epic open backgrounds ONE agent session in the epic workspace
  // dir (the motivating tech-debt-scout case) — NOT the visible N-tile assembly. It
  // returns a `{sessionId}` (first-class + adoptable), unlike the headed tile's `{}`.
  if (headed === false) {
    return headlessLaunchers.openEpicHeadless(epicTarget, launchAgentKey(target.agent, false));
  }
  await openEpicWorkspaceFor(epicTarget);
  return {};
}

/** Build the controller bound to a dashboard doc. */
export function makeDashboardLaunchController(docPath: string): DashboardLaunchController {
  const deps = makeRealDashboardLaunchDeps(docPath);
  const branchDeps = makeRealBranchLaunchDeps(docPath);
  // SW-GW-2: the `dir` open dispatches to SW-GW-1's generic-workspace wrapper in
  // the ACTIVE team's bucket (a dir has no epic/story to derive a team from —
  // mirrors `open-generic-workspace.ts`'s `getActiveTeamKey()` human entry).
  const dirDeps: DirLaunchDeps = {
    openGenericWorkspace: (path) =>
      useViewerStore.getState().openOrFocusGenericWorkspace(getActiveTeamKey(), path),
  };
  // SW-GW-7: the epic/dir HEADLESS launchers (background an MCP-connected agent
  // session instead of the visible tile/shell) — built once per controller, like
  // `deps`/`branchDeps` above.
  const headlessLaunchers = makeRealHeadlessLaunchers(docPath);
  return {
    openWorkspace: (arg) => {
      if (arg.target.kind === "story" && "storyPath" in arg.target) {
        // The path form is the native workspace opener (a foreground tab); it has no
        // background session contract, so refuse before any reveal/select/tab side effect.
        if (arg.headed === false) {
          return Promise.reject(
            new Error(
              "unsupported: a story path opens the visible workspace; headed:false needs a story id",
            ),
          );
        }
        const teamKey = getActiveTeamKey();
        if (!teamKey) throw new Error("unavailable: no active team for story path");
        return useViewerStore
          .getState()
          .openOrFocusStoryWorkspace(teamKey, arg.target.storyPath, undefined, undefined, true)
          .then(() => ({}));
      }
      return dispatchOpenWorkspace(
        arg,
        docPath,
        arg.frameId ? makeRealDashboardLaunchDeps(docPath, arg.frameId) : deps,
        arg.frameId ? makeRealBranchLaunchDeps(docPath, arg.frameId) : branchDeps,
        dirDeps,
        arg.frameId ? makeRealHeadlessLaunchers(docPath, arg.frameId) : headlessLaunchers,
      );
    },
    // SW-GW-3: story-keyed liveness read (open-state from the viewer store, not the
    // launch registry; real liveness probe). Not docPath-scoped — see the resolver.
    getWorkspaceStatus: (storyId: string) => resolveWorkspaceStatusForStory(storyId),
    runPrompt: (params: RunPromptParams) => runStoryPrompt(params, deps),
    sendToSession: (params: SendToSessionParams) => sendToStorySession(params, deps),
    // SW-GW-10: close a session by handle — pure orchestration owns the
    // resolve→evict→kill order; the deps are handle-agnostic.
    closeWorkspace: (target: string) => closeStorySession(target, deps),
    // SW-DX-11: the doc's own path is THIS controller's scope, so `readWorkspace`
    // is a thin trusted call — the doc never supplies the docPath or the ext
    // whitelist. Rust enforces confinement/traversal/managed/size/binary.
    readWorkspace: (path: string, frameId?: string) =>
      readWorkspaceFile(docPath, path, [...READ_WORKSPACE_EXTS], frameId),
    // SW-DX-11 §5: the containing epic's handle, resolved trusted-side.
    getEpicHandle: () => resolveEpicHandleForDoc(docPath),
    // SW-DX-14: the dashboard's own absolute directory, resolved trusted-side —
    // the anchor a board uses to build an absolute sibling-story path for
    // `runSkill` without needing an open epic workspace. Reuses `dirOfPath` but
    // keeps the `null`-for-slashless contract (a doc with no dir has no anchor).
    getDocDir: () => (docPath.includes("/") ? dirOfPath(docPath) : null),
  };
}

/** The controllers of every mounted live doc, oldest first. */
const mountedControllers: DashboardLaunchController[] = [];

/**
 * The launch controller of a live dashboard doc (`docPath`), registered as the
 * app-wide one while it is mounted; `null` for a baseline render (`live` false),
 * which gets no launch surface. A doc hands the returned controller to its own
 * bridge calls, so each doc acts for itself while several are live. Unmounting
 * hands the app-wide slot back to the newest doc still mounted (none → `null`,
 * so the verbs fail closed), never clearing a slot another doc holds.
 */
export function useRegisterDashboardLaunchController(
  docPath: string | null,
  live: boolean,
): DashboardLaunchController | null {
  const controller = useMemo(
    () => (docPath && live ? makeDashboardLaunchController(docPath) : null),
    [docPath, live],
  );
  useEffect(() => {
    if (!controller) return;
    mountedControllers.push(controller);
    setDashboardLaunchController(controller);
    return () => {
      const at = mountedControllers.indexOf(controller);
      if (at >= 0) mountedControllers.splice(at, 1);
      if (getDashboardLaunchController() === controller) {
        setDashboardLaunchController(mountedControllers.at(-1) ?? null);
      }
    };
  }, [controller]);
  return controller;
}
