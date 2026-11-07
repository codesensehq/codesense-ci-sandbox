// SPDX-License-Identifier: MIT
import { projectedSession } from "./agent-list-projection";
import { hasTurnAfterCompletion } from "@/shared/lib/agent-state";
import type { ManagedSessionState } from "@/shared/lib/managed-session";
import { isTerminalManagedState } from "@/shared/lib/managed-session";
import type {
  NeedsYouKind,
  TerminalAiStatus,
  TerminalWireStatus,
} from "@/shared/lib/terminal-ai-status";
import type { AgentDots, useAgentStateStore } from "@/shared/stores/agent-state-store";

export interface DotSources {
  aiStatusBySession: Readonly<Record<string, TerminalWireStatus>>;
}

export type PendingAsks = Readonly<Record<string, { sessionId: string }>>;

/** Shared wire/record precedence, independent of React and mark projections. */
export function selectSessionAiStatus(
  s: DotSources,
  dots: AgentDots,
  sessionId: string,
): TerminalAiStatus | undefined {
  const projected = dots[sessionId];
  const stored = s.aiStatusBySession?.[sessionId];
  if (stored === "needs-permission" && (projected === "running" || projected === "thinking"))
    return stored;
  return projected ?? stored;
}

export interface NeedsYouState extends DotSources {
  agentDots: AgentDots;
  pendingAsks: PendingAsks;
  outcomes?: Readonly<Record<string, ManagedSessionState>>;
  crashes?: Pick<ReturnType<typeof useAgentStateStore.getState>, "bySession"> &
    Partial<
      Pick<
        ReturnType<typeof useAgentStateStore.getState>,
        "activityBySession" | "completionSeqBySession"
      >
    >;
}

/** Historical outcomes remain stored but no longer own current turn details. */
export function selectEffectiveManagedOutcome(
  state: Pick<NeedsYouState, "crashes" | "outcomes">,
  sessionId: string,
): ManagedSessionState | undefined {
  return hasTurnAfterCompletion(
    state.crashes?.bySession[sessionId],
    state.crashes?.completionSeqBySession?.[sessionId],
  )
    ? undefined
    : state.outcomes?.[sessionId];
}

/** A blocking question, crash, declared outcome, or the current permission/block. */
export function selectNeedsYouKind(state: NeedsYouState, sessionId: string): NeedsYouKind | null {
  const owner = projectedSession(sessionId);
  if (owner && "needsYouKind" in owner) return owner.needsYouKind ?? null;
  if (Object.values(state.pendingAsks).some((a) => a.sessionId === sessionId)) return "question";
  const outcome =
    projectedSession(sessionId)?.effectiveStatus === "dropped"
      ? "needs-attention"
      : selectEffectiveManagedOutcome(state, sessionId);
  const snapshot = state.crashes?.activityBySession?.[sessionId]?.snapshot;
  if (
    // Done retains host ask IDs for cleanup, but retires their answer controls.
    // Only positively ordered new-turn evidence releases its display outcome.
    (outcome === undefined || !isTerminalManagedState(outcome)) &&
    snapshot?.observed &&
    snapshot.exit == null &&
    snapshot.state === "needs-permission" &&
    (snapshot.reason === "question" || snapshot.asks.length > 0)
  )
    return "question";
  const status = selectSessionAiStatus(state, state.agentDots, sessionId);
  if (status === "crashed") {
    const projected = projectedSession(sessionId);
    return projected?.crashId &&
      projected.effectiveStatus !== "crashed" &&
      projected.effectiveStatus !== "failed"
      ? null
      : "crashed";
  }
  if (outcome === "failed") return "failed";
  if (outcome === "needs-attention") return "attention";
  if (status === "error") return "failed";
  if (status === "needs-permission") {
    if (state.crashes?.bySession[sessionId]?.state === "blocked") return "blocked";
    return "permission";
  }
  return null;
}
