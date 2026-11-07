// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useSyncExternalStore, type RefObject } from "react";
import { listen } from "@tauri-apps/api/event";

import * as commands from "@/tauri/commands";
import type { RuntimeSessionReference, SessionPrompt } from "@/tauri/commands";
import { getViewerState } from "@/features/viewer/lib/cli-workspace/store/viewer-access";
import { heldRemotely, sessionTarget } from "@/shared/lib/terminal-layout-projection";
import { logger } from "@/shared/lib/logger";

/**
 * The terminal caption's prompt segment for one session — its cwd, the home of
 * the account it runs as, and the Git status of the checkout it is in — read
 * on the host that runs it (`session_prompt_cmd`; a remote tab asks its
 * Machine, by its admitted target).
 *
 * Kept cheap, the way a shell prompt is:
 * - read when a caption mounts, and again when the session's shell reaches a
 *   prompt or finishes a command (OSC 133 `prompt-start` / `command-end`);
 * - otherwise at most every {@link PROMPT_POLL_MS} while the caption is on
 *   screen (a hidden workspace's caption, or a hidden window, never polls);
 * - one read in flight per session, shared by every caption showing it, and
 *   each host caches `git status` per path (`git_prompt.rs`).
 */
export type { GitPromptStatus, SessionPrompt } from "@/tauri/commands";

export const PROMPT_POLL_MS = 5000;
/** OSC 133 marks arrive in bursts (prompt-start right after command-end). */
const MARK_DEBOUNCE_MS = 150;

type Listener = () => void;

/** How long a session's cached prompt outlives its last caption, so a caption
 *  that remounts (a tab or workspace switch) doesn't flash back to its name. */
const EVICT_AFTER_MS = 30_000;

const prompts = new Map<string, SessionPrompt>();
const inFlight = new Map<string, Promise<void>>();
/** Sessions whose shell moved on while a read was in flight: that read may
 *  predate the change (a `cd`), so one more read follows it. */
const rerun = new Set<string>();
const lastRead = new Map<string, number>();
/** Sessions whose shell moved on (an OSC 133 mark) while no caption showing
 *  them was on screen: the first caption to come back on screen reads. */
const staleOffScreen = new Set<string>();
const listeners = new Map<string, Set<Listener>>();
const markTimers = new Map<string, ReturnType<typeof setTimeout>>();
/** Per session, whether each mounted caption is on screen right now. */
const visibility = new Map<string, Set<() => boolean>>();
const evictTimers = new Map<string, ReturnType<typeof setTimeout>>();
let unlistenMarks: (() => void) | null = null;
let markSubscribers = 0;
let markListenPending = false;
/** Bumped by {@link resetSessionPrompts}: a read begun before it writes nothing. */
let epoch = 0;

/** A monotonic clock: a wall-clock jump back must not stall polling. */
const now = (): number => (typeof performance !== "undefined" ? performance.now() : Date.now());

function notify(sessionId: string): void {
  listeners.get(sessionId)?.forEach((l) => l());
}

/** The session's host reference: its admitted target when a remote Machine
 *  holds it, its id for This Mac, `null` while its Machine is unreachable. */
function referenceFor(sessionId: string): RuntimeSessionReference | null {
  const layout = getViewerState().terminalLayout;
  if (!layout || !heldRemotely(layout, sessionId)) return sessionId;
  return sessionTarget(layout, sessionId) ?? null;
}

/** The invoke wrapper, resolved defensively: a vitest module mock throws on a
 *  key it omits, and many trees render the caption under such a mock. */
function promptCommand(): ((ref: RuntimeSessionReference) => Promise<SessionPrompt>) | null {
  try {
    const fn = (commands as { sessionPrompt?: unknown }).sessionPrompt;
    return typeof fn === "function"
      ? (fn as (ref: RuntimeSessionReference) => Promise<SessionPrompt>)
      : null;
  } catch {
    return null;
  }
}

function samePrompt(a: SessionPrompt | undefined, b: SessionPrompt): boolean {
  return !!a && JSON.stringify(a) === JSON.stringify(b);
}

/**
 * Read `sessionId`'s prompt now, joining a read already in flight. `fresh`
 * (the shell just reached a prompt or finished a command) means a read
 * already running may predate what changed, so another follows it.
 */
export function refreshSessionPrompt(
  sessionId: string,
  options: { fresh?: boolean } = {},
): Promise<void> {
  const running = inFlight.get(sessionId);
  if (running) {
    if (options.fresh) rerun.add(sessionId);
    return running;
  }
  const fn = promptCommand();
  const ref = referenceFor(sessionId);
  if (!fn || ref === null) return Promise.resolve();
  lastRead.set(sessionId, now());
  staleOffScreen.delete(sessionId);
  const started = epoch;
  const read: Promise<void> = fn(ref)
    .then((prompt) => {
      if (started !== epoch) return;
      // Evicted while the read was out (no caption, no pending eviction): an
      // entry written now would never be cleared.
      if (!listeners.has(sessionId) && !evictTimers.has(sessionId)) return;
      if (!prompt || samePrompt(prompts.get(sessionId), prompt)) return;
      prompts.set(sessionId, prompt);
      notify(sessionId);
    })
    .catch((err) => {
      logger.debug("useSessionPrompt", `read failed for ${sessionId} — ${err}`);
    })
    .finally(() => {
      // Only this read's own entry: a reset may have let a newer one start.
      if (inFlight.get(sessionId) === read) inFlight.delete(sessionId);
      if (started === epoch && rerun.delete(sessionId) && listeners.has(sessionId)) {
        // Hidden while the read was out: read when a caption comes back on screen, as a
        // mark does, rather than running git on the host for nothing shown.
        if (anyOnScreen(sessionId)) void refreshSessionPrompt(sessionId);
        else {
          lastRead.delete(sessionId);
          staleOffScreen.add(sessionId);
        }
      }
    });
  inFlight.set(sessionId, read);
  return read;
}

function onMark(sessionId: string): void {
  if (!listeners.has(sessionId)) return;
  clearTimeout(markTimers.get(sessionId));
  markTimers.delete(sessionId);
  // No caption showing it on screen: read when one comes back on screen (or
  // at the next on-screen poll tick) instead of reading now.
  if (!anyOnScreen(sessionId)) {
    lastRead.delete(sessionId);
    staleOffScreen.add(sessionId);
    return;
  }
  markTimers.set(
    sessionId,
    setTimeout(() => {
      markTimers.delete(sessionId);
      // Hidden during the debounce: wait for it to come back instead.
      if (!anyOnScreen(sessionId)) {
        lastRead.delete(sessionId);
        staleOffScreen.add(sessionId);
        return;
      }
      void refreshSessionPrompt(sessionId, { fresh: true });
    }, MARK_DEBOUNCE_MS),
  );
}

/** Whether any mounted caption of `sessionId` is on screen (true when none
 *  reports, so a caption without a visibility check still reads). */
function anyOnScreen(sessionId: string): boolean {
  const checks = visibility.get(sessionId);
  return !checks || checks.size === 0 || [...checks].some((visible) => visible());
}

function subscribeMarks(): () => void {
  const subscribedIn = epoch;
  markSubscribers += 1;
  // Any subscriber (re)starts the listener when none is up: a failed `listen`
  // is retried by the next caption, not only after every caption unmounts.
  if (!unlistenMarks && !markListenPending) {
    markListenPending = true;
    try {
      void Promise.resolve(
        listen<{ session_id: string; kind: string }>("pty-osc133", (event) => {
          const { session_id, kind } = event.payload ?? {};
          if (session_id && (kind === "prompt-start" || kind === "command-end")) onMark(session_id);
        }),
      )
        .then((un) => {
          // A reset while `listen` was pending: that listener is orphaned.
          if (subscribedIn !== epoch || markSubscribers === 0 || unlistenMarks) un?.();
          else unlistenMarks = un ?? null;
        })
        .catch(() => undefined)
        .finally(() => {
          if (subscribedIn === epoch) markListenPending = false;
        });
    } catch {
      // No Tauri event bridge (tests, storybook): polling still refreshes.
      markListenPending = false;
    }
  }
  return () => {
    // A reset since this subscribed already dropped its count.
    if (subscribedIn !== epoch) return;
    markSubscribers -= 1;
    if (markSubscribers === 0) {
      unlistenMarks?.();
      unlistenMarks = null;
    }
  };
}

function subscribe(sessionId: string, listener: Listener): () => void {
  let set = listeners.get(sessionId);
  if (!set) listeners.set(sessionId, (set = new Set()));
  set.add(listener);
  clearTimeout(evictTimers.get(sessionId));
  evictTimers.delete(sessionId);
  return () => {
    set.delete(listener);
    // A reset since this subscribed replaced the session's set: leave it.
    if (set.size === 0 && listeners.get(sessionId) === set) {
      listeners.delete(sessionId);
      clearTimeout(markTimers.get(sessionId));
      markTimers.delete(sessionId);
      rerun.delete(sessionId);
      // A closed session's prompt is not kept for the life of the app.
      evictTimers.set(
        sessionId,
        setTimeout(() => {
          evictTimers.delete(sessionId);
          if (listeners.has(sessionId)) return;
          prompts.delete(sessionId);
          lastRead.delete(sessionId);
          staleOffScreen.delete(sessionId);
        }, EVICT_AFTER_MS),
      );
    }
  };
}

/** Whether `el` is laid out on screen (not inside a `display:none` subtree)
 *  and the window is showing. */
function onScreen(el: HTMLElement | null | undefined): boolean {
  if (typeof document !== "undefined" && document.visibilityState === "hidden") return false;
  if (!el) return true;
  return el.getClientRects().length > 0;
}

/** Test seam: forget every cached prompt. */
export function resetSessionPrompts(): void {
  epoch += 1;
  prompts.clear();
  inFlight.clear();
  rerun.clear();
  lastRead.clear();
  staleOffScreen.clear();
  markTimers.forEach((t) => clearTimeout(t));
  markTimers.clear();
  evictTimers.forEach((t) => clearTimeout(t));
  evictTimers.clear();
  listeners.clear();
  visibility.clear();
  unlistenMarks?.();
  unlistenMarks = null;
  markSubscribers = 0;
  markListenPending = false;
}

/**
 * The prompt segment for `sessionId`, or `null` until the host has answered.
 * `visibleRef` is the caption element: polling pauses while it is not laid
 * out.
 */
export function useSessionPrompt(
  sessionId: string | undefined,
  visibleRef?: RefObject<HTMLElement | null>,
): SessionPrompt | null {
  const subscribeSession = useCallback(
    (cb: Listener) => (sessionId ? subscribe(sessionId, cb) : () => undefined),
    [sessionId],
  );
  const prompt = useSyncExternalStore(
    subscribeSession,
    () => (sessionId ? (prompts.get(sessionId) ?? null) : null),
    () => null,
  );

  useEffect(() => {
    if (!sessionId) return;
    const unmarks = subscribeMarks();
    const isVisible = () => onScreen(visibleRef?.current);
    let checks = visibility.get(sessionId);
    if (!checks) visibility.set(sessionId, (checks = new Set()));
    checks.add(isVisible);
    void refreshSessionPrompt(sessionId);
    const timer = setInterval(() => {
      if (!isVisible()) return;
      const last = lastRead.get(sessionId);
      if (last !== undefined && now() - last < PROMPT_POLL_MS - 50) return;
      void refreshSessionPrompt(sessionId);
    }, PROMPT_POLL_MS);
    const onVisible = () => {
      // Coming back to the window refreshes only the captions on screen.
      if (document.visibilityState === "visible" && onScreen(visibleRef?.current)) {
        void refreshSessionPrompt(sessionId, { fresh: true });
      }
    };
    document.addEventListener("visibilitychange", onVisible);
    // A hidden workspace shown again (its caption laid out once more) reads
    // at once when the shell moved on while it was hidden — not at the next
    // poll tick, up to PROMPT_POLL_MS later.
    const el = visibleRef?.current;
    const resize =
      el && typeof ResizeObserver !== "undefined"
        ? new ResizeObserver(() => {
            if (staleOffScreen.has(sessionId) && isVisible()) {
              void refreshSessionPrompt(sessionId, { fresh: true });
            }
          })
        : null;
    if (el) resize?.observe(el);
    return () => {
      clearInterval(timer);
      resize?.disconnect();
      document.removeEventListener("visibilitychange", onVisible);
      unmarks();
      const set = visibility.get(sessionId);
      set?.delete(isVisible);
      if (set?.size === 0 && visibility.get(sessionId) === set) visibility.delete(sessionId);
    };
  }, [sessionId, visibleRef]);

  return prompt;
}
