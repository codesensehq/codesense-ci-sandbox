// SPDX-License-Identifier: MIT
import { create } from "zustand";
import { useAgentStateStore } from "./agent-state-store";
import type { ActivityCompletion } from "@/tauri/commands";

import {
  type AskUserRequest,
  type DoneStatus,
  type ManagedSessionState,
  managedStateForDoneStatus,
} from "@/shared/lib/managed-session";

/**
 * SW-AW-4 (B/D) — the managed task-session state machine.
 *
 * A `sessionId`-keyed SIBLING of `use-agent-chat-store` (NOT an extension of
 * it): the transcript store holds Q&A *text*; this store holds the task-level
 * received lifecycle signals + the correlation-keyed pending asks.
 * Rust supervises launches and publishes their effective status. No git-sync/Claude
 * literals; a story/epic session binds it unchanged (AC-11).
 *
 * A session is "managed" only once a managed signal touches it (launch →
 * `markRunning`, a question → `addPendingAsk`, or `signal_done` →
 * `applyDoneStatus`). Until then `selectManagedState` is `undefined` and the
 * chrome renders only `owner`/`connection` — task-state is a separate axis.
 *
 * Selector discipline: read via `useManagedSessionStore(selectX)`; never
 * destructure the whole store, and never build a fresh array inside a selector.
 */
interface ManagedSessionStoreState {
  /** Task-level state per `sessionId`. Absent = not (yet) a managed task. */
  states: Record<string, ManagedSessionState>;
  /**
   * SW-OR-6 (AC-3b / GAP-1): the short human `summary` a `signal_done` /
   * headless completion carried on `mcp:agent-done`, per `sessionId`. A SIBLING
   * of `states` (not a widening of its value) so `states` / `selectManagedState`
   * stay byte-identical — SW-BA-5's chip derivation and the SW-SR-4 run monitor
   * are untouched. Absent when the run reported no summary. Consumed by the
   * SW-OR-12 output panel + the SW-AU-55 read-model (which render a headless run
   * AFTER its self-removing panel row is gone).
   */
  summaries: Record<string, string>;
  /** Pending blocking asks, keyed by `correlationId`. */
  pendingAsks: Record<string, AskUserRequest>;
  /** Record that a managed launch began. */
  markRunning: (sessionId: string) => void;
  /** An incoming blocking question → `awaiting-answer`. */
  addPendingAsk: (req: AskUserRequest) => void;
  /** The ask was answered/dismissed → drop it; back to `running` if none remain. */
  resolvePendingAsk: (correlationId: string) => void;
  /**
   * `signal_done` arrived → terminal state (top of the completion precedence).
   * SW-OR-6 (AC-3b): an OPTIONAL `summary` is retained alongside — additive, so a
   * call that omits it leaves `summaries` untouched (same reference).
   */
  applyDoneStatus: (
    sessionId: string,
    status: DoneStatus,
    opts?: { summary?: string | null; activityCompletion?: ActivityCompletion },
  ) => void;
  /** Drop answer controls when the session ends; Rust owns the resulting status. */
  handleSessionDeath: (sessionId: string) => void;
  /** Receive the existing task state from the sessions owner; no local supervision. */
  receiveProjectedState: (sessionId: string, state: ManagedSessionState) => void;
  /** Hard-close: drop all managed state + pending asks for a session. */
  clearSession: (sessionId: string) => void;
}

/** Remove every pending ask belonging to `sessionId`. Returns the SAME reference
 *  when the session had no asks, so callers can skip a spurious store write. */
function withoutSessionAsks(
  pendingAsks: Record<string, AskUserRequest>,
  sessionId: string,
): Record<string, AskUserRequest> {
  if (!Object.values(pendingAsks).some((a) => a.sessionId === sessionId)) {
    return pendingAsks;
  }
  const next: Record<string, AskUserRequest> = {};
  for (const [correlationId, ask] of Object.entries(pendingAsks)) {
    if (ask.sessionId !== sessionId) next[correlationId] = ask;
  }
  return next;
}

export const useManagedSessionStore = create<ManagedSessionStoreState>((set) => ({
  states: {},
  summaries: {},
  pendingAsks: {},

  markRunning: (sessionId) =>
    set((s) => {
      // Only arm from unmanaged/running — NEVER clobber a terminal state
      // (`done`/`failed`/`needs-attention`) or an in-flight `awaiting-answer`. This effect
      // re-fires on remount/rebind, so an unguarded set would resurrect a
      // finished session or desync the badge from a still-pending question.
      const cur = s.states[sessionId];
      if (cur !== undefined && cur !== "running") return s;
      return { states: { ...s.states, [sessionId]: "running" } };
    }),

  addPendingAsk: (req) =>
    set((s) => ({
      pendingAsks: { ...s.pendingAsks, [req.correlationId]: req },
      states: { ...s.states, [req.sessionId]: "awaiting-answer" },
    })),

  resolvePendingAsk: (correlationId) =>
    set((s) => {
      const ask = s.pendingAsks[correlationId];
      if (!ask) return s;
      const pendingAsks = { ...s.pendingAsks };
      delete pendingAsks[correlationId];
      // Return the session to RUNNING only if it was waiting on THIS ask and no
      // other ask for the same session is still outstanding — and never override
      // a terminal state (a signal_done that raced the answer wins).
      const stillWaiting = Object.values(pendingAsks).some((a) => a.sessionId === ask.sessionId);
      const states =
        s.states[ask.sessionId] === "awaiting-answer" && !stillWaiting
          ? { ...s.states, [ask.sessionId]: "running" as ManagedSessionState }
          : s.states;
      return { pendingAsks, states };
    }),

  applyDoneStatus: (sessionId, status, opts) =>
    set((s) => {
      const next: ManagedSessionState = managedStateForDoneStatus(status);
      const { accepted, historical, repeated } = useAgentStateStore
        .getState()
        .recordTaskCompletion(sessionId, opts?.activityCompletion);
      if (!accepted) return s;
      return {
        states: { ...s.states, [sessionId]: next },
        // SW-OR-6 (AC-3b): retain the run's summary when one was reported. Additive —
        // a falsy/absent summary leaves the map by the SAME reference, so a run that
        // reported none is byte-identical to today (no spurious selector churn).
        summaries: opts?.summary ? { ...s.summaries, [sessionId]: opts.summary } : s.summaries,
        // A finished session has no open questions — drop any
        // stray pending asks so the panel can't render a phantom question.
        pendingAsks:
          historical || repeated ? s.pendingAsks : withoutSessionAsks(s.pendingAsks, sessionId),
      };
    }),

  receiveProjectedState: (sessionId, state) =>
    set((s) =>
      s.states[sessionId] === state ? s : { states: { ...s.states, [sessionId]: state } },
    ),

  handleSessionDeath: (sessionId) =>
    set((s) => {
      const pendingAsks = withoutSessionAsks(s.pendingAsks, sessionId);
      if (pendingAsks === s.pendingAsks) return s; // nothing to do
      return { states: s.states, pendingAsks };
    }),

  clearSession: (sessionId) =>
    set((s) => {
      const hasSummary = sessionId in s.summaries;
      if (
        !(sessionId in s.states) &&
        !hasSummary &&
        !Object.values(s.pendingAsks).some((a) => a.sessionId === sessionId)
      ) {
        return s;
      }
      const states = { ...s.states };
      delete states[sessionId];
      // SW-OR-6 (AC-3b): a hard-close drops the retained summary too (parity with
      // `states`), so a reused sessionId can't inherit a stale summary.
      let summaries = s.summaries;
      if (hasSummary) {
        summaries = { ...s.summaries };
        delete summaries[sessionId];
      }
      return {
        states,
        summaries,
        pendingAsks: withoutSessionAsks(s.pendingAsks, sessionId),
      };
    }),
}));

/** Task-level state for a session (`undefined` = not a managed task). */
export const selectManagedState =
  (sessionId: string) =>
  (s: ManagedSessionStoreState): ManagedSessionState | undefined =>
    s.states[sessionId];

/** SW-OR-6 (AC-3b): the retained `signal_done` summary for a session (`undefined`
 *  when the run reported none). A scalar selector — same per-session subscription
 *  discipline as {@link selectManagedState} — for the SW-OR-12 output panel header. */
export const selectManagedSummary =
  (sessionId: string) =>
  (s: ManagedSessionStoreState): string | undefined =>
    s.summaries[sessionId];

/** The whole pending-ask map (stable ref — filter per session in a `useMemo`). */
export const selectPendingAsks = (s: ManagedSessionStoreState): Record<string, AskUserRequest> =>
  s.pendingAsks;

/** Test-only reset. */
export function resetManagedSessionStoreForTests(): void {
  useManagedSessionStore.setState({
    states: {},
    summaries: {},
    pendingAsks: {},
  });
}
