// SPDX-License-Identifier: MIT
import { useCallback, useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";

import { logger } from "@/shared/lib/logger";

/**
 * SW-VE-14: per-session revision counter driven by the backend's `pty-osc7`
 * event (OSC 7 cwd mutations, deduped at the PTY batcher so the counter only
 * bumps on real `cd`s). Pair with {@link useSessionCwd} as the composite:
 *
 *     const revision = useSessionCwdRevision(sessionId);
 *     const liveCwd = useSessionCwd(sessionId, revision);
 *
 * On every bump, `useSessionCwd` re-reads the host's cwd for `sessionId`, so
 * the overlay and the worktree editor retarget without a session restart.
 *
 * A single shared Tauri listener fans out to every mounted consumer — sibling
 * hooks on the same session share one subscription and one revision stream.
 */
const revisions = new Map<string, number>();
const listenersBySession = new Map<string, Set<() => void>>();
let subscriberCount = 0;
let unlistenPtyOsc7: (() => void) | null = null;
let listenPending = false;
let epoch = 0;

function bump(sessionId: string): void {
  if (!sessionId) return;
  revisions.set(sessionId, (revisions.get(sessionId) ?? 0) + 1);
  listenersBySession.get(sessionId)?.forEach((cb) => cb());
}

function ensureListener(): void {
  if (unlistenPtyOsc7 || listenPending) return;
  const subscribedIn = epoch;
  listenPending = true;
  try {
    void Promise.resolve(
      listen<{ session_id?: string; cwd?: string }>("pty-osc7", (event) => {
        const sid = event.payload?.session_id;
        if (typeof sid === "string" && sid) bump(sid);
      }),
    )
      .then((un) => {
        // A reset while `listen` was pending, or every subscriber dropped out
        // before the listener attached: orphan it rather than hold the slot.
        if (subscribedIn !== epoch || subscriberCount === 0 || unlistenPtyOsc7) {
          un?.();
          return;
        }
        unlistenPtyOsc7 = un ?? null;
      })
      .catch((err) => logger.warn("useSessionCwdRevision", `listen pty-osc7 failed — ${err}`))
      .finally(() => {
        if (subscribedIn === epoch) listenPending = false;
      });
  } catch {
    // No Tauri bridge (tests, storybook): the hook stays at revision 0.
    listenPending = false;
  }
}

function subscribeSession(sessionId: string, cb: () => void): () => void {
  let set = listenersBySession.get(sessionId);
  if (!set) listenersBySession.set(sessionId, (set = new Set()));
  set.add(cb);
  subscriberCount += 1;
  ensureListener();
  return () => {
    set.delete(cb);
    if (set.size === 0 && listenersBySession.get(sessionId) === set) {
      listenersBySession.delete(sessionId);
    }
    subscriberCount -= 1;
    if (subscriberCount === 0) {
      unlistenPtyOsc7?.();
      unlistenPtyOsc7 = null;
    }
  };
}

/** Test seam: forget every revision and drop the shared Tauri listener. */
export function resetSessionCwdRevisions(): void {
  epoch += 1;
  revisions.clear();
  listenersBySession.clear();
  subscriberCount = 0;
  unlistenPtyOsc7?.();
  unlistenPtyOsc7 = null;
  listenPending = false;
}

/** Test seam: force a revision bump for `sessionId` without going through the
 *  Tauri event bridge. Used by seam tests of the overlay and editor so an
 *  event-driven retarget can be driven without a running backend. */
export function __bumpSessionCwdRevisionForTest(sessionId: string): void {
  bump(sessionId);
}

/**
 * The per-session revision counter — starts at 0, bumps on every real `cd`.
 * Stable identity when `sessionId` is stable: a reader whose cwd never
 * changes never re-renders.
 */
export function useSessionCwdRevision(sessionId: string | undefined): number {
  const subscribe = useCallback(
    (cb: () => void) => (sessionId ? subscribeSession(sessionId, cb) : () => undefined),
    [sessionId],
  );
  return useSyncExternalStore(
    subscribe,
    () => (sessionId ? (revisions.get(sessionId) ?? 0) : 0),
    () => 0,
  );
}
