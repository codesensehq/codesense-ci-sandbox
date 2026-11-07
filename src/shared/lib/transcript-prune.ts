// SPDX-License-Identifier: MIT
import { createKeyedDebounce } from "@/shared/lib/keyed-debounce";
import { useAgentChatStore } from "@/shared/stores/use-agent-chat-store";
import { useManagedSessionStore } from "@/shared/stores/use-managed-session-store";

/**
 * RF-perf-15 (F-5, OQ-2) — deferred prune of a dead agent session's memory.
 *
 * Each session's transcript (`use-agent-chat-store`, up to 500 messages) and
 * managed state (`use-managed-session-store`) is retained for the life of the
 * app once created — `clearTranscript`/`clearSession` had no production callers.
 *
 * We can't delete on the `pty-exit` death edge: a user may still be reading the
 * just-stopped transcript (the Running-Agents panel keeps a dead session visible
 * and bring-to-foreground'able, SW-DX-17d). So the prune is DEFERRED behind a
 * grace window and suppressed entirely while a chat surface is showing the session.
 *
 * Two independent facts drive the decision:
 *  - `deadSessions` — the session's PTY has exited (the only reliable "truly dead"
 *    signal). A live session is never in this set, so it can never be pruned.
 *  - `viewers` — a ref-count of chat surfaces currently showing the session. The
 *    prune is armed ONLY when a dead session has zero viewers, and is (re)armed
 *    when the last viewer leaves. This closes the death-while-viewing window: if
 *    the session dies while its surface is open, we mark it dead but do NOT arm
 *    the timer until the user closes the view — the transcript never vanishes
 *    out from under a reader.
 *
 * Session ids are collision-free by construction (`shell-${crypto.randomUUID()}`,
 * viewer-store.ts) — a re-opened/self-healed workspace spawns a FRESH id, never
 * the dead one. So a pruned id can never belong to a later live session; there is
 * no "resurrection" hazard, and both maps are safe to key on the raw id.
 *
 * Reuses `createKeyedDebounce` (per-session timer + `cancel`) — no new timer
 * primitive. Every op is O(1) map work; nothing here blocks the UI.
 */
const TRANSCRIPT_PRUNE_GRACE_MS = 30_000;

/** Sessions whose PTY has exited — the only sessions eligible for pruning. */
const deadSessions = new Set<string>();
/** Ref-count of chat surfaces currently displaying a session's transcript. */
const viewers = new Map<string, number>();

const isViewed = (sessionId: string): boolean => (viewers.get(sessionId) ?? 0) > 0;

const pruner = createKeyedDebounce<string>(TRANSCRIPT_PRUNE_GRACE_MS, (sessionId) => {
  // Clear the dead-mark first so the set self-heals on every fire (no unbounded
  // growth even if a prune was armed and later superseded).
  deadSessions.delete(sessionId);
  useAgentChatStore.getState().clearTranscript(sessionId);
  useManagedSessionStore.getState().clearSession(sessionId);
});

/**
 * A session's PTY died — mark it dead and arm the deferred prune, UNLESS a chat
 * surface is currently showing it (then the prune arms when the last viewer
 * leaves — see `endViewingSession`). Handles death both before and during viewing.
 */
export function scheduleTranscriptPruneOnDeath(sessionId: string): void {
  deadSessions.add(sessionId);
  if (!isViewed(sessionId)) pruner.schedule(sessionId, sessionId);
}

/** A chat surface began showing `sessionId` — hold any pending prune while viewed. */
export function beginViewingSession(sessionId: string): void {
  viewers.set(sessionId, (viewers.get(sessionId) ?? 0) + 1);
  pruner.cancel(sessionId);
}

/**
 * A chat surface stopped showing `sessionId`. When the LAST viewer leaves, arm the
 * prune iff the session is dead — a no-op for a live session, so tabbing a live
 * agent's chat away never drops its transcript.
 */
export function endViewingSession(sessionId: string): void {
  const current = viewers.get(sessionId) ?? 0;
  // Ignore an unbalanced end (no active viewer) — a floor at 0 keeps the count
  // from going negative and a spurious end from arming a prune.
  if (current === 0) return;
  const remaining = current - 1;
  if (remaining > 0) {
    viewers.set(sessionId, remaining);
    return;
  }
  viewers.delete(sessionId);
  if (deadSessions.has(sessionId)) pruner.schedule(sessionId, sessionId);
}

/**
 * Drop every pending prune + clear the dead-session marks (bridge teardown).
 * Deliberately does NOT touch `viewers` — those are owned by the chat-surface
 * components' own lifecycle (begin/end), not the bridge, so clearing them here
 * could desync a still-mounted surface's ref-count.
 */
export function cancelAllTranscriptPrunes(): void {
  pruner.cancelAll();
  deadSessions.clear();
}

/** The grace window, exposed for tests to advance fake timers precisely. */
export const TRANSCRIPT_PRUNE_GRACE_MS_FOR_TESTS = TRANSCRIPT_PRUNE_GRACE_MS;

/** Test-only: drop every pending prune + clear ALL tracking (incl. viewers). */
export function resetTranscriptPruneForTests(): void {
  cancelAllTranscriptPrunes();
  viewers.clear();
}
