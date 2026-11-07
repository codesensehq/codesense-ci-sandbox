// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";

import { safeUnlisten } from "@/shared/lib/tauri-listeners";
import { resolveAskUser } from "@/tauri/commands";
import {
  type AskUserRequest,
  type AskUserResponse,
  type DoneStatus,
} from "@/shared/lib/managed-session";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";
import {
  scheduleTranscriptPruneOnDeath,
  cancelAllTranscriptPrunes,
} from "@/shared/lib/transcript-prune";
import { logger } from "@/shared/lib/logger";
import { parseActivityCompletion } from "@/shared/lib/activity-completion";
import type { ActivityCompletion } from "@/tauri/commands";

/**
 * SW-AW-4 (B/C/D) — the managed task-session bridge. Two INBOUND agent→UI
 * signals plus the one OUTBOUND answer leg:
 *
 * - `mcp:ask-user`  (blocking question) → record a pending ask (awaiting-answer).
 * - `mcp:ask-user-closed` (the call ended unanswered) → drop that pending ask.
 * - `mcp:agent-done` (signal_done)      → flip the managed state to terminal.
 * - `answerAskUser(...)`                → resolve the Rust oneshot by correlationId.
 *
 * This channel is DISJOINT from the `mcp:agent-message` chat/pong listener: an
 * answer is UI→agent and unblocks a Rust oneshot (not PTY injection, not a chat
 * emit). Mirrors `use-canvas-render-bridge` (request/reply), NOT `use-mcp-actions`
 * (one-way push). Mount once at app root.
 */

export interface AgentDonePayload {
  sessionId: string;
  status: DoneStatus;
  summary?: string | null;
  activityCompletion?: ActivityCompletion;
}

const DONE_STATUSES: readonly DoneStatus[] = ["success", "failed", "needs-attention"];

function isDoneStatus(x: unknown): x is DoneStatus {
  return typeof x === "string" && (DONE_STATUSES as readonly string[]).includes(x);
}

/** Record an inbound blocking question. Ignores malformed payloads. */
export function receiveAskUser(req: AskUserRequest): void {
  if (!req?.correlationId || !req?.sessionId || !req?.question) return;
  logger.debug("mcp", `ask-user received for session ${req.sessionId} (${req.correlationId})`);
  useManagedSessionStore.getState().addPendingAsk(req);
}

/** An ask that ended without the UI answering it (`mcp:ask-user-closed`). */
export interface AskUserClosedPayload {
  sessionId: string;
  correlationId: string;
  reason: "cancelled" | "timeout" | "superseded";
}

/**
 * Take down a question the agent's call no longer waits on (the client
 * cancelled it, it timed out, or its slot was reclaimed), so every surface
 * showing it (the in-tile banner, the Agents-panel card) clears and it can't be
 * answered. Ignores malformed payloads; an unknown id is a no-op.
 */
export function receiveAskUserClosed(payload: AskUserClosedPayload): void {
  if (!payload?.correlationId) return;
  logger.debug("mcp", `ask-user closed (${payload.reason}) for ${payload.correlationId}`);
  useManagedSessionStore.getState().resolvePendingAsk(payload.correlationId);
}

/** Apply an inbound completion signal. Ignores unknown statuses / payloads.
 *  SW-OR-6 (AC-3b / GAP-1): the payload's `summary` was read into the type but
 *  dropped here — now retained in the managed store for the SW-OR-12 output
 *  surface / SW-AU-55 read-model. */
export function receiveAgentDone(payload: AgentDonePayload): void {
  if (!payload?.sessionId || !isDoneStatus(payload.status)) return;
  logger.debug("mcp", `agent-done received for session ${payload.sessionId}: ${payload.status}`);
  useManagedSessionStore.getState().applyDoneStatus(payload.sessionId, payload.status, {
    summary: payload.summary,
    activityCompletion: parseActivityCompletion(payload.activityCompletion),
  });
}

/**
 * Apply an inbound PTY-death signal (`pty-exit`). Drops the session's pending
 * asks + surfaces a mid-task death as `needs-attention` (existing AC-3 leg), then
 * arms the deferred memory prune (RF-perf-15 F-5) so the dead session's transcript
 * and managed state are reclaimed after a grace window (OQ-2).
 */
export function receiveSessionDeath(sessionId: string): void {
  useManagedSessionStore.getState().handleSessionDeath(sessionId);
  scheduleTranscriptPruneOnDeath(sessionId);
}

export type AskUserAnswerDeps = {
  resolve: (
    correlationId: string,
    action: AskUserResponse["action"],
    content: string | null,
  ) => Promise<void>;
};

// Wrap the command in an arrow so `resolveAskUser` is accessed LAZILY (at call
// time), not at module eval — mirrors `use-canvas-render-bridge`'s `resolve`.
// A direct `{ resolve: resolveAskUser }` would touch the export during import,
// which trips consumer test mocks of `@/tauri/commands` that omit it.
const defaultAnswerDeps: AskUserAnswerDeps = {
  resolve: (correlationId, action, content) => resolveAskUser(correlationId, action, content),
};

/**
 * Answer (or dismiss) a pending question. Drops the pending ask FIRST so the UI
 * always moves on (no-wedge — the question can't get stuck on screen), then
 * best-effort resolves the Rust oneshot. A failed resolve is swallowed-and-logged
 * (the Rust verb's timeout/sweep is the backstop) rather than left as an
 * unhandled rejection. Pure + dependency-injected for testability.
 */
export async function answerAskUser(
  correlationId: string,
  response: AskUserResponse,
  deps: AskUserAnswerDeps = defaultAnswerDeps,
): Promise<void> {
  useManagedSessionStore.getState().resolvePendingAsk(correlationId);
  try {
    await deps.resolve(correlationId, response.action, response.content ?? null);
  } catch (e) {
    const message = e instanceof Error ? e.message : String(e);
    logger.error("mcp", `ask_user resolve failed for ${correlationId}: ${message}`);
  }
}

/** Mount the managed-session listeners once at app root (App.tsx). */
export function useManagedSessionBridge(): void {
  useEffect(() => {
    const unAsk = listen<AskUserRequest>("mcp:ask-user", (event) => {
      receiveAskUser(event.payload);
    });
    const unClosed = listen<AskUserClosedPayload>("mcp:ask-user-closed", (event) => {
      receiveAskUserClosed(event.payload);
    });
    const unDone = listen<AgentDonePayload>("mcp:agent-done", (event) => {
      receiveAgentDone(event.payload);
    });
    // AC-3 FE leg — the counterpart to the Rust session-death sweep. When a PTY
    // dies, drop its pending asks (so no dead-session question stays answerable)
    // and surface a mid-task death as `needs-attention`.
    const unExit = listen<{ session_id: string }>("pty-exit", (event) => {
      const sessionId = event.payload?.session_id;
      if (sessionId) receiveSessionDeath(sessionId);
    });
    return () => {
      unAsk.then(safeUnlisten).catch(() => {});
      unClosed.then(safeUnlisten).catch(() => {});
      unDone.then(safeUnlisten).catch(() => {});
      unExit.then(safeUnlisten).catch(() => {});
      // Drop any pending deferred prunes so a timer can't fire after teardown.
      cancelAllTranscriptPrunes();
    };
  }, []);
}
