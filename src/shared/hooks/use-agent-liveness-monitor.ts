// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { sessionTarget } from "@/shared/lib/terminal-layout-projection";

import {
  type AgentSignalInfo,
  listAgentSignals,
  type SessionLivenessQuery,
  type SessionLivenessResult,
  type RuntimeSessionTarget,
  sessionsHaveLiveAgent,
} from "@/tauri/commands";
import { agentProcessName, deathDot, endedByExit, isTurnOpen } from "@/shared/lib/agent-state";
import {
  admitsAgentUpdate,
  agentInputSeq,
  useAgentStateStore,
} from "@/shared/stores/agent-state-store";
import { KNOWN_AGENT_CLI_NAMES } from "@/shared/lib/agent-clis";
import {
  selectMinimizedTerminalTabIndex,
  selectRunningTerminalTabIndex,
  type TerminalWireStatus,
  type ViewerState,
  useViewerStore,
} from "@/shared/stores/viewer-store";

/**
 * SLW-FX-17: the "reliability floor" merge for the per-tab AI-status dot, driven
 * by the process-liveness probe. Pure so the push (hook) vs pull (poll) policy is
 * unit-testable in isolation:
 *  - process GONE → clear the dot (consistent with the `SessionEnd`/`pty-exit`
 *    clear paths); self-heals a dot left stuck by a mislocated warm-pool post.
 *  - process PRESENT → floor to `running` ONLY when the dot is otherwise empty;
 *    never override a precise hook-set state (`running`/`done`/`error`/
 *    `needs-permission`). The hook stays the precise fast-path; the poll only
 *    backfills where the hook never fired (e.g. the deliberately-unwired
 *    `org_root`) or was dropped by the endpoint liveness guard.
 *
 * Known coarseness: the probe cannot tell "running a turn" from "done, at prompt"
 * (the agent process is alive in both), so a floored dot reads `running` for the
 * whole time the agent is open, then clears on exit — strictly better than the
 * no-dot / stuck-dot it replaces. Where hooks fire, the session's agent-state
 * record (`agent-state-store.ts`) carries the turn itself: `thinking` / `working`
 * while it is open, `done` once `Stop` closes it.
 */
export function floorDotStatus(
  current: TerminalWireStatus | undefined,
  alive: boolean,
): TerminalWireStatus | undefined {
  if (!alive) return undefined;
  return current ?? "running";
}

/** How often to probe that a CONNECTED agent's process is still alive. The user
 *  can exit the agent in the CLI (it drops to a bare `zsh` — the PTY survives, so
 *  there's no `pty-exit`, and the notify hook isn't wired for git-sync), so we
 *  poll the process probe and report it gone. */
export const LIVENESS_POLL_MS = 4_000;

/**
 * SW-FX-52 (AC-3): one live registration in the shared liveness registry. Each
 * active monitor instance contributes one of these; the ONE consolidated poller
 * batches all of them into a single `sessionsHaveLiveAgent` call per tick.
 */
interface Registration {
  sessionId: string;
  agentBasename: string;
  /** Chat-liveness: fire ONCE when the agent process is found gone, then
   *  auto-unregister. Absent for a dot-floor-only registration. */
  onDead?: () => void;
  /** SLW-FX-17: dot-floor — reconcile `aiStatusBySession[sessionId]` from the
   *  probe every tick (persistent; never auto-unregisters). */
  reconcileDot?: boolean;
}

/** SLW-FX-17: apply the floor-merge to the store for one session. Guarded so a
 *  stable value is a no-op (setSessionAiStatus is itself idempotent). A gone
 *  process keeps the dot its agent record ended on (`deathDot`) — a crash is
 *  never cleared by the floor. */
function reconcileDotFloor(sessionId: string, alive: boolean): void {
  const store = useViewerStore.getState();
  const current = store.aiStatusBySession[sessionId];
  const next = alive ? floorDotStatus(current, alive) : sessionDeathDot(sessionId);
  if (next !== current) store.setSessionAiStatus(sessionId, next);
}

/** The dot `sessionId` keeps now that its agent process is gone. */
function sessionDeathDot(sessionId: string): TerminalWireStatus | undefined {
  return deathDot(useAgentStateStore.getState().bySession[sessionId]);
}

/**
 * The heartbeat of an open agent turn. Claude fires no hook between
 * `UserPromptSubmit` and `Stop` when no tool runs, so the probe result is the
 * turn's Tier-2 input: while the agent is alive the record keeps its declared
 * `thinking`/`working`, and a vanished process ends the turn. The probe sees only
 * alive/gone, so a gone process is a code-less exit.
 *
 * Every session with an open turn is probed, not just the registered ones, so a
 * Claude in a plain CLI tab (no dot-floor registration) gets the heartbeat too;
 * it drops out of the probe once its turn closes.
 */
function openTurnProbes(): { sessionId: string; agentName: string }[] {
  const out: { sessionId: string; agentName: string }[] = [];
  for (const [sessionId, rec] of Object.entries(useAgentStateStore.getState().bySession)) {
    const agentName = agentProcessName(rec);
    if (agentName !== null && isTurnOpen(rec)) out.push({ sessionId, agentName });
  }
  return out;
}

function hasOpenTurn(): boolean {
  return Object.values(useAgentStateStore.getState().bySession).some(isTurnOpen);
}

/** Start the poller whenever an agent turn opens. Returns the unsubscribe. */
export function watchOpenTurns(): () => void {
  if (hasOpenTurn()) ensurePoller();
  return useAgentStateStore.subscribe((s, prev) => {
    if (s.bySession !== prev.bySession && hasOpenTurn()) ensurePoller();
  });
}

// SW-FX-52 (AC-3): module-level registry + ONE shared interval. Previously each
// monitor mounted its own `setInterval` + per-session IPC probe → N sessions =
// N intervals = N full process-table scans/tick. Now every active monitor
// REGISTERS here; one interval (lazily created on first register, torn down when
// the registry empties) polls one batch per owning host.
const registry = new Map<symbol, Registration>();
let pollHandle: ReturnType<typeof setInterval> | null = null;
const hostsInFlight = new Set<string>();
let pollGeneration = 0;
const probeKey = (sessionId: string, agentName: string) => `${sessionId}\u0000${agentName}`;

/** Undefined is legacy local; null is held but unavailable/ambiguous. */
function probeTarget(sessionId: string): RuntimeSessionTarget | null | undefined {
  const layout = useViewerStore.getState().terminalLayout;
  const target = sessionTarget(layout, sessionId);
  if (target) return target;
  const held = Object.values(layout.members).some((member) =>
    Object.values(member.workspaces).some((workspace) =>
      workspace.tabs.some((tab) => tab.session === sessionId),
    ),
  );
  return held ? null : undefined;
}

function tick(): void {
  // Snapshot the registry so registrations added/removed mid-await don't skew
  // this pass (and a session unregistered after its onDead can't be re-notified).
  const entries = Array.from(registry.entries());
  const openTurns = openTurnProbes();
  const tabs = Array.from(tabAgents.entries());
  if (entries.length === 0 && openTurns.length === 0 && tabs.length === 0) {
    stopPoller();
    return;
  }

  // Deduplicate queries, then batch independently by admitted host. A slow
  // remote read cannot postpone another host's reconciliation or next poll.
  const probes = [
    ...entries.map(([, r]) => ({ sessionId: r.sessionId, agentName: r.agentBasename })),
    ...openTurns,
  ];
  const queries: SessionLivenessQuery[] = Array.from(
    new Map(probes.map((p) => [probeKey(p.sessionId, p.agentName), p])).values(),
  ).map((p) => ({ ptyId: p.sessionId, agentName: p.agentName }));
  // Terminal-tab floor: probe every known agent in every tab (deduped against the
  // registrations above); an idle-output agent's probe also asks for the output tail.
  const queried = new Map(queries.map((q) => [probeKey(q.ptyId, q.agentName), q]));
  for (const [sessionId, agents] of tabs) {
    for (const agent of agents) {
      const key = probeKey(sessionId, agent.basename);
      const tail = agent.attention === "idle-output";
      const existing = queried.get(key);
      if (existing) {
        if (tail) existing.tail = true;
        continue;
      }
      const q: SessionLivenessQuery = { ptyId: sessionId, agentName: agent.basename };
      if (tail) q.tail = true;
      queried.set(key, q);
      queries.push(q);
    }
  }
  const groups = new Map<string, SessionLivenessQuery[]>();
  for (const query of queries) {
    const target = probeTarget(query.ptyId);
    // Held sessions require exact admission; never fall back to local IDs.
    if (target === null) continue;
    if (target) query.target = target;
    const key = target ? JSON.stringify({ ...target, session: undefined }) : "legacy-local";
    const group = groups.get(key) ?? [];
    group.push(query);
    groups.set(key, group);
  }
  const generation = pollGeneration;
  for (const [host, queries] of groups) {
    if (hostsInFlight.has(host)) continue;
    hostsInFlight.add(host);
    const sentAt = agentInputSeq();
    void Promise.resolve()
      .then(() => sessionsHaveLiveAgent(queries))
      .then((results) => {
        if (generation !== pollGeneration) return;
        reconcileProbeResults(queries, results, entries, openTurns, tabs, sentAt);
      })
      .catch(() => {
        // Unavailable telemetry is not process-exit evidence.
      })
      .finally(() => {
        if (generation === pollGeneration) hostsInFlight.delete(host);
      });
  }
}

function reconcileProbeResults(
  queries: SessionLivenessQuery[],
  results: SessionLivenessResult[],
  entries: [symbol, Registration][],
  openTurns: ReturnType<typeof openTurnProbes>,
  tabs: [string, AgentSignalInfo[]][],
  sentAt: number,
): void {
  // Results come back in query order; key each by the (session, agent) it
  // answered, so two agent names probed on one session don't collide.
  const aliveByKey = new Map<string, boolean>();
  const outputByKey = new Map<string, { seq: number; tail: string }>();
  results.forEach((r, i) => {
    const q = queries[i];
    const target = q ? probeTarget(q.ptyId) : null;
    if (
      q &&
      r.ptyId === q.ptyId &&
      admitsAgentUpdate(q.ptyId, sentAt) &&
      r.alive !== null &&
      target !== null &&
      JSON.stringify(q.target) === JSON.stringify(target)
    ) {
      aliveByKey.set(probeKey(q.ptyId, q.agentName), r.alive);
      if (r.output) outputByKey.set(probeKey(q.ptyId, q.agentName), r.output);
    }
  });

  // Tier-2 input for every probed session's agent record (a no-op for a session
  // with none, or one whose PTY already exited). An open turn's own probe (the
  // record's agent name) wins over a registration's. Folded before the dot is
  // reconciled below, so a gone process's dot reads the record's verdict; a
  // session no floor reconciles gets its crash onto the dot here.
  const aliveBySession = new Map<string, boolean>();
  for (const p of [
    ...entries.map(([, r]) => ({ sessionId: r.sessionId, agentName: r.agentBasename })),
    ...openTurns,
  ]) {
    const alive = aliveByKey.get(probeKey(p.sessionId, p.agentName));
    if (alive !== undefined) aliveBySession.set(p.sessionId, alive);
  }
  // A settled agent (its turn closed) gets no probe of its own, but its tab's
  // floor probes the tab for it: when that finds it gone, the agent ended there
  // too, so the record never outlives the process it names — a later command in
  // that shell is not taken for the agent's exit.
  for (const [sessionId] of tabs) {
    if (aliveBySession.has(sessionId)) continue;
    const rec = useAgentStateStore.getState().bySession[sessionId];
    const agent = rec ? agentProcessName(rec) : null;
    if (agent !== null && aliveByKey.get(probeKey(sessionId, agent)) === false) {
      aliveBySession.set(sessionId, false);
    }
  }
  for (const [sessionId, alive] of aliveBySession) {
    const before = useAgentStateStore.getState().bySession[sessionId];
    useAgentStateStore
      .getState()
      .recordProcess(sessionId, alive ? { kind: "alive" } : { kind: "exited", code: null }, sentAt);
    const after = useAgentStateStore.getState().bySession[sessionId];
    // The agent's life just ended here: the stored status takes the dot it
    // ended on (a crash holds; a code-less vanish clears), as a pty-exit does.
    if (after && after !== before && endedByExit(after) && !(before && endedByExit(before))) {
      useViewerStore.getState().setSessionAiStatus(sessionId, sessionDeathDot(sessionId));
    }
  }

  for (const [token, r] of entries) {
    if (!registry.has(token) || !admitsAgentUpdate(r.sessionId, sentAt)) continue;
    const alive = aliveByKey.get(probeKey(r.sessionId, r.agentBasename));
    // SLW-FX-17 dot-floor: reconcile the per-tab dot from the probe. Unknown /
    // missing pty in the results (e.g. added mid-await) is treated as alive — do
    // NOT clear on an unknown result. Persistent: never auto-unregisters here.
    if (r.reconcileDot && alive !== undefined) {
      reconcileDotFloor(r.sessionId, alive);
    }
    // Chat-liveness onDead: only fire for a session the backend explicitly
    // reported dead. Unknown / missing pty is treated as alive (never a false
    // disconnect). Fire once, then auto-unregister so it runs at most once.
    if (r.onDead && alive === false) {
      // The registration may have been torn down (unmount) mid-await — never call
      // onDead after unregister.
      if (!registry.has(token)) continue;
      registry.delete(token);
      r.onDead();
    }
  }

  const now = Date.now();
  for (const [sessionId, agents] of tabs) {
    // A tab removed mid-await is no longer watched.
    if (!tabAgents.has(sessionId) || !admitsAgentUpdate(sessionId, sentAt)) continue;
    const results = agents.map((agent) => ({
      agent,
      alive: aliveByKey.get(probeKey(sessionId, agent.basename)),
    }));
    // Unknown results (no answer for this tab) never clear the dot.
    if (results.every((r) => r.alive === undefined)) continue;
    const alive = results.filter((r) => r.alive === true).map((r) => r.agent);
    const live = alive.find((a) => a.attention === "idle-output") ?? alive[0];
    if (live) {
      tabsWithAgent.add(sessionId);
      useAgentStateStore.getState().setProcessAgent(sessionId, live.basename, sentAt);
      reconcileDotFloor(sessionId, true);
    } else if (results.every((r) => r.alive === false) && tabsWithAgent.delete(sessionId)) {
      useAgentStateStore.getState().setProcessAgent(sessionId, null, sentAt);
      // Clear only after this floor saw a listed agent leave: an agent it cannot see
      // (unlisted, or not a descendant of the tab's shell) keeps its own dot. The
      // agent left, so its record (any, hook-established or not) ends here too.
      useAgentStateStore
        .getState()
        .recordProcess(sessionId, { kind: "exited", code: null }, sentAt);
      reconcileDotFloor(sessionId, false);
      idleRaised.delete(sessionId);
    }
    const output = live && outputByKey.get(probeKey(sessionId, live.basename));
    if (!live || live.attention !== "idle-output") {
      idleTracks.delete(sessionId);
      continue;
    }
    // The screen text could not be read this tick (e.g. the emulator worker is
    // restarting): keep the track as it is rather than restarting the quiet
    // stretch or forgetting a raised dot.
    if (!output) continue;
    const step = stepIdleAttention(idleTracks.get(sessionId), output, now, live.quietSecs * 1000);
    if (step.change === "needs-you" && !applyIdleAttention(sessionId, step.change)) {
      // The dot was showing something else; retry on a later tick of this quiet stretch.
      idleTracks.set(sessionId, { ...step.track, flagged: false });
      continue;
    }
    idleTracks.set(sessionId, step.track);
    if (step.change === "resumed") applyIdleAttention(sessionId, step.change);
  }

  if (registry.size === 0 && tabAgents.size === 0 && !hasOpenTurn()) stopPoller();
}

function ensurePoller(): void {
  if (pollHandle !== null) return;
  pollHandle = setInterval(tick, LIVENESS_POLL_MS);
}

/** Test-only: drop every registration and stop the shared poller, so one test's
 *  interval (and its fake-timer handle) never leaks into the next. */
export function resetLivenessPollerForTests(): void {
  registry.clear();
  tabAgents.clear();
  idleTracks.clear();
  tabsWithAgent.clear();
  idleRaised.clear();
  stopPoller();
  hostsInFlight.clear();
  pollGeneration++;
}

function stopPoller(): void {
  if (pollHandle === null) return;
  clearInterval(pollHandle);
  pollHandle = null;
}

/** Register one entry into the shared registry, start the poller, and return the
 *  teardown that unregisters it and stops the poller once the registry empties.
 *  Shared by both registration hooks so the token/ensure/stop bookkeeping lives
 *  in exactly one place. */
function addRegistration(reg: Registration): () => void {
  const token = Symbol("liveness-registration");
  registry.set(token, reg);
  ensurePoller();
  return () => {
    registry.delete(token);
    if (registry.size === 0 && tabAgents.size === 0 && !hasOpenTurn()) stopPoller();
  };
}

// ---------------------------------------------------------------------------
// Terminal-tab dot floor + idle-output attention
// ---------------------------------------------------------------------------

/** Every terminal tab's session → the installed agents that may be running in it. */
const tabAgents = new Map<string, AgentSignalInfo[]>();
/** Idle-output bookkeeping per tab session (only while an idle-output agent is alive). */
const idleTracks = new Map<string, IdleTrack>();
/** Tab sessions where the floor has seen a listed agent alive. */
const tabsWithAgent = new Set<string>();
/** Tab sessions whose needs-you dot the idle-output detector raised. */
const idleRaised = new Set<string>();

/** One tab session's output-quiet bookkeeping. */
export interface IdleTrack {
  /** The output sequence last observed. */
  seq: number;
  /** When that sequence was first observed (ms). */
  since: number;
  /** This detector raised needs-you for the current quiet stretch. */
  flagged: boolean;
}

/** The last non-blank line of a session's screen text (the emulator's plain
 *  text, one screen line per `\n`). */
export function lastOutputLine(text: string): string {
  const lines = text.split("\n");
  for (let i = lines.length - 1; i >= 0; i--) {
    const line = (lines[i] ?? "").trimEnd();
    if (line.trim() !== "") return line;
  }
  return "";
}

/** A line that reads like the agent is asking something: a question, a `key:` /
 *  `>` style input prompt, or a yes/no choice. */
export function isPromptShaped(line: string): boolean {
  const l = line.trimEnd();
  if (l === "") return false;
  return /[?:>›❯»]$/.test(l) || /(\((y\/n|yes\/no)\)|\[(y\/n)\])$/i.test(l);
}

/**
 * Fold one output observation into a tab's idle track. Output that moved starts a new
 * stretch (and, if needs-you was raised for the previous one, reports `"resumed"`);
 * output quiet for `quietMs` on a prompt-shaped last line reports `"needs-you"` once.
 */
export function stepIdleAttention(
  prev: IdleTrack | undefined,
  output: { seq: number; tail: string },
  now: number,
  quietMs: number,
): { track: IdleTrack; change: "needs-you" | "resumed" | null } {
  if (!prev || prev.seq !== output.seq) {
    return {
      track: { seq: output.seq, since: now, flagged: false },
      change: prev?.flagged ? "resumed" : null,
    };
  }
  if (!prev.flagged && now - prev.since >= quietMs && isPromptShaped(lastOutputLine(output.tail))) {
    return { track: { ...prev, flagged: true }, change: "needs-you" };
  }
  return { track: prev, change: null };
}

/** Apply an idle-output transition to the dot; returns whether it wrote. Needs-you only
 *  replaces an empty or floored `running` dot; resuming only clears the needs-you this
 *  detector raised and that is still showing. */
function applyIdleAttention(sessionId: string, change: "needs-you" | "resumed"): boolean {
  const store = useViewerStore.getState();
  const current = store.aiStatusBySession[sessionId];
  if (change === "needs-you") {
    if (current !== undefined && current !== "running") return false;
    store.setSessionAiStatus(sessionId, "needs-permission");
    idleRaised.add(sessionId);
    return true;
  }
  const raised = idleRaised.delete(sessionId);
  if (!raised || current !== "needs-permission") return false;
  store.setSessionAiStatus(sessionId, "running");
  return true;
}

/** Every terminal tab session id — shown, Hidden (so a hidden tab's status
 *  keeps feeding its tray chip), and minimized workspaces. */
export function collectTabSessionIds(state: ViewerState): string[] {
  const ids = new Set<string>();
  for (const index of [
    selectRunningTerminalTabIndex(state),
    selectMinimizedTerminalTabIndex(state),
  ]) {
    for (const byWs of Object.values(index)) {
      for (const tabs of Object.values(byWs)) {
        for (const tab of tabs) ids.add(tab.sessionId);
      }
    }
  }
  return [...ids];
}

/** Replace the watched tab set: each session is probed for every agent in `agents`. */
function setTabAgents(sessionIds: string[], agents: AgentSignalInfo[]): void {
  const keep = new Set(agents.length > 0 ? sessionIds : []);
  for (const id of tabAgents.keys()) {
    if (!keep.has(id)) {
      tabAgents.delete(id);
      idleTracks.delete(id);
      if (tabsWithAgent.delete(id)) useAgentStateStore.getState().setProcessAgent(id, null);
      idleRaised.delete(id);
    }
  }
  for (const id of keep) tabAgents.set(id, agents);
  if (tabAgents.size > 0) ensurePoller();
  else if (registry.size === 0 && !hasOpenTurn()) stopPoller();
}

/** A known CLI with no built-in adapter has no attention channel; its quiet window is never read. */
const DEFAULT_QUIET_SECS = 10;

/**
 * What the tab floor probes each tab for: every built-in agent adapter (with its attention
 * channel), then every other known agent CLI ({@link KNOWN_AGENT_CLI_NAMES}) for liveness
 * alone, so a `gemini` or `aider` someone typed into a tab is found as well.
 */
export function tabProbeAgents(installed: readonly AgentSignalInfo[]): AgentSignalInfo[] {
  const out = [...installed];
  const have = new Set(installed.map((a) => a.basename));
  for (const cli of KNOWN_AGENT_CLI_NAMES) {
    if (have.has(cli)) continue;
    out.push({ basename: cli, attention: null, quietSecs: DEFAULT_QUIET_SECS });
  }
  return out;
}

/**
 * The per-tab status dot for every terminal tab, not only agent-module surfaces: each
 * tab is probed for every installed agent (a story/worktree tab where the user ran
 * `codex` gets the dot as well), and an agent whose adapter declares `idle-output`
 * attention gets needs-you from the terminal going quiet on a prompt. The agent CLI it
 * finds running in a tab is recorded (`setProcessAgent`), which is what makes the tab an
 * agent when no hook ever said so: a reattached session, a hookless CLI. Returns the
 * teardown.
 */
export function watchTerminalTabAgents(loadAgents?: () => Promise<AgentSignalInfo[]>): () => void {
  let disposed = false;
  let unsubscribe: (() => void) | null = null;
  let loading: Promise<AgentSignalInfo[]>;
  try {
    loading = (loadAgents ?? listAgentSignals)();
  } catch (err) {
    loading = Promise.reject(err);
  }
  loading
    // No installed-agent list still leaves the known CLIs to look for.
    .catch((): AgentSignalInfo[] => [])
    .then((installed) => {
      const agents = tabProbeAgents(installed);
      if (disposed) return;
      const sync = () => setTabAgents(collectTabSessionIds(useViewerStore.getState()), agents);
      sync();
      // The tab indexes' inputs: comparing them (not the memoized indexes of
      // both states) rebuilds nothing on an unrelated tick.
      unsubscribe = useViewerStore.subscribe((s, prev) => {
        if (
          s.terminalLayout.byKey !== prev.terminalLayout.byKey ||
          s.workspaces !== prev.workspaces ||
          s.softClosedWorkspaces !== prev.softClosedWorkspaces
        ) {
          sync();
        }
      });
    })
    .catch(() => {
      // The floor could not start; agent-module surfaces keep their own floor.
    });
  return () => {
    disposed = true;
    unsubscribe?.();
    setTabAgents([], []);
  };
}

/**
 * SW-GS-2d — while an agent session is verified-live, poll that its process is
 * still alive and call `onDead` ONCE when it disappears. Otherwise the chat keeps
 * thinking it's connected after the user exits the agent in the CLI.
 *
 * Agent-agnostic: the caller supplies `agentBasename` (no vendor literal here). A
 * probe error is treated as still-alive (never a false-positive disconnect).
 *
 * SW-FX-52 (AC-3): each active monitor no longer owns its own interval — it
 * REGISTERS into the shared module-level registry, and ONE consolidated poller
 * batches every registered session into a single `sessionsHaveLiveAgent` call
 * per tick (one process-table snapshot for all N sessions). Teardown on unmount
 * removes the registration and guarantees `onDead` is never called afterward.
 */
export function useAgentLivenessMonitor(opts: {
  /** Monitor only while the session is verified-live. */
  active: boolean;
  sessionId: string | null;
  agentBasename: string;
  /** Invoked once when the agent process is found gone. Memoize it (it's a dep). */
  onDead: () => void;
}): void {
  const { active, sessionId, agentBasename, onDead } = opts;
  useEffect(() => {
    if (!active || sessionId === null) return;
    return addRegistration({ sessionId, agentBasename, onDead });
  }, [active, sessionId, agentBasename, onDead]);
}

/**
 * SLW-FX-17 — reliability floor for the per-tab AI-status dot. While a
 * codesense-launched agent session is bound, reconcile `aiStatusBySession` from
 * the SAME batched process probe (`floorDotStatus`): floor to `running` when the
 * dot is empty and the agent is alive, clear when it is gone. This makes the dot
 * correct even where the notify hook never fires (e.g. the deliberately-unwired
 * `org_root`) or was dropped by the endpoint liveness guard (the warm-pool race).
 *
 * Distinct from `useAgentLivenessMonitor`: it has NO `onDead` (no chat-disconnect
 * side effect) and is PERSISTENT (does not auto-unregister on death — it keeps
 * clearing/refloring across reconnects). It shares the ONE module-level registry
 * + poller, so it adds no second interval and no extra IPC (queries dedupe by
 * (session, agent name)). Activate whenever a session is bound — independent of chat "live"
 * state — so the dot is floored before/around the chat verifying liveness.
 */
export function useAgentDotFloor(opts: {
  /** Floor the dot whenever a session is bound (not only when chat is "live"). */
  active: boolean;
  sessionId: string | null;
  agentBasename: string;
}): void {
  const { active, sessionId, agentBasename } = opts;
  useEffect(() => {
    if (!active || sessionId === null) return;
    return addRegistration({ sessionId, agentBasename, reconcileDot: true });
  }, [active, sessionId, agentBasename]);
}
