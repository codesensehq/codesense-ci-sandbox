// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import type { AskUserRequest } from "@/shared/lib/managed-session";
import {
  useManagedSessionStore,
  selectPendingAsks,
} from "@/shared/stores/use-managed-session-store";

/** The live blocking-ask picture for one session (oldest ask + total pending). */
export interface SessionPendingAsk {
  /** The oldest pending ask for the session (monotonic `ask-user-N`), or `null` when none. */
  ask: AskUserRequest | null;
  /** Total pending asks for the session — drives the "+N more" badge. */
  pendingCount: number;
}

/** Oldest-first order for asks (correlation ids are monotonic `ask-user-N`). */
export function byOldestAsk(a: AskUserRequest, b: AskUserRequest): number {
  return a.correlationId.localeCompare(b.correlationId, undefined, { numeric: true });
}

/**
 * SW-AW-7 — the single source of "which blocking ask is live for this session".
 *
 * Reads the correlation-keyed managed-session store, filters to `sessionId`, orders
 * oldest-first by monotonic `correlationId`, and returns the oldest ask + the count
 * in one memoized pass. Shared by `ManagedQuestionPanel` (the module-owned default
 * card) AND `AgentModuleBody` (the consumer `answerablePrompts` slot) so the
 * filter+sort selection lives in exactly ONE place — the module body never
 * re-derives `selectPendingAsks` (SW-AW-7 AC-3, DRY).
 */
export function useOldestPendingAsk(sessionId: string): SessionPendingAsk {
  const pendingAsks = useManagedSessionStore(selectPendingAsks);
  return useMemo(() => {
    const asks = Object.values(pendingAsks)
      .filter((a) => a.sessionId === sessionId)
      .sort(byOldestAsk);
    return { ask: asks[0] ?? null, pendingCount: asks.length };
  }, [pendingAsks, sessionId]);
}
