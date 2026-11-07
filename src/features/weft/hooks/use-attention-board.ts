// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";

import { listen } from "@tauri-apps/api/event";

import {
  listAttentionBoard,
  wvListPendingApprovals,
  type AttentionBoardDto,
  type AttentionRowDto,
  type WvApprovalRow,
} from "@/tauri/commands";
import { WEFT_ATTENTION_CHANGED_EVENT } from "@/tauri/events";
import { createListenerGroup } from "@/shared/lib/tauri-listeners";
import { logger } from "@/shared/lib/logger";
import { useBackendActiveTeamStore } from "@/shared/lib/backend-active-team";
import type { LiveListDegraded } from "@/features/shell/right-pane/live-list";
import { useWvEndorsementEvents } from "./use-wv-endorsement-events";

/**
 * SW-WV-F1 — the Weft Board's data hook. Reads the five-section attention board and
 * keeps it live via the `wv:attention-changed` push event (NO poll interval — a write
 * nudges a background re-fetch, mirroring `useAutomationChanged`). Gen-guarded so a stale
 * in-flight response can never overwrite a newer one and no post-unmount `setState` fires
 * (mirrors `useAutomations`).
 *
 * SW-AV-27: the controller lease/badge was removed with the control plane — the hook no
 * longer reads a lease. A failure flips the `degraded` banner so `LiveList` renders its
 * Retry path (`onRetry` → `reload`). The Rust command runs off the UI thread, so this
 * never blocks render.
 */
export interface UseAttentionBoard {
  /** `null` = not yet loaded (distinct from an all-empty board). */
  board: AttentionBoardDto | null;
  /** The last fetch error message, or `null`. */
  error: string | null;
  /** Degraded live-channel state for the `LiveList` banner, or `null` when healthy. */
  degraded: LiveListDegraded | null;
  /** Re-read the board. Gen-guarded; safe to pass to `LiveList.onRetry`. */
  reload: () => void;
}

/** A failed FIRST read (e.g. the backend has no active team yet on a fresh profile) is
 *  retried once on its own after this delay, still showing "Loading…"; only a failed
 *  retry shows the error + Retry. */
export const FIRST_READ_RETRY_MS = 700;
/** How long a first read may go unanswered before it is retried (and, if the retry also
 *  hangs, shown as failed). A late answer is still used. */
export const FIRST_READ_TIMEOUT_MS = 1000;
/** After that, a failed read keeps retrying on its own with these delays (the last one
 *  repeats) until the backend answers — a failure never becomes the resting state. This
 *  is the only repeating read, and only while reads fail: a healthy board never polls. */
export const RETRY_BACKOFF_MS = [1000, 2000, 4000, 8000] as const;

interface BoardSnapshot {
  board: AttentionBoardDto | null;
  error: string | null;
  degraded: LiveListDegraded | null;
}

const EMPTY_SNAPSHOT: BoardSnapshot = { board: null, error: null, degraded: null };

/**
 * The one board reader behind every `useAttentionBoard()`. Several surfaces show the
 * board at once (the panel, its header, the needs-you badge and notifications…); they
 * share this single read, its retry timers and its log lines, so a failure episode is
 * one read per attempt and one warn line, not one per mounted surface. It starts with
 * the first surface and stops (dropping its state) when the last one goes.
 */
const shared = {
  snapshot: EMPTY_SNAPSHOT,
  listeners: new Set<() => void>(),
  users: 0,
  /** Bumped each time the last reader lets go, so a read from an earlier run is dropped. */
  episode: 0,
  gen: 0,
  loaded: false,
  /** Consecutive failed reads; 0 once a read lands. */
  failures: 0,
  /** The last failure reason logged, so each new reason is logged once, not every retry. */
  loggedReason: null as string | null,
  timers: new Set<ReturnType<typeof setTimeout>>(),
  stop: null as (() => void) | null,
};

function publish(patch: Partial<BoardSnapshot>): void {
  shared.snapshot = { ...shared.snapshot, ...patch };
  for (const l of shared.listeners) l();
}

function later(ms: number, fn: () => void): ReturnType<typeof setTimeout> {
  const t = setTimeout(() => {
    shared.timers.delete(t);
    fn();
  }, ms);
  shared.timers.add(t);
  return t;
}

function clearTimers(): void {
  for (const t of shared.timers) clearTimeout(t);
  shared.timers.clear();
}

/** Re-read the board. Gen-guarded; a manual Retry or push read supersedes a pending
 *  automatic retry. */
function reloadBoard(): void {
  // Nothing reads the board (a stale Retry or push after the last reader went): don't
  // start reads or retries that no release would ever stop.
  if (shared.users === 0) return;
  const episode = shared.episode;
  const attempt = () => {
    if (episode !== shared.episode) return;
    const gen = ++shared.gen;
    let failed = false;
    let timeout: ReturnType<typeof setTimeout> | null = null;
    const fail = (message: string) => {
      if (failed || gen !== shared.gen) return;
      failed = true;
      const n = ++shared.failures;
      // Say why — one warn line per failure episode, and a quieter line when the reason
      // changes during it — so a miss can be diagnosed without drowning the log.
      if (message !== shared.loggedReason) {
        shared.loggedReason = message;
        const line = `Activity read failed (attempt ${n}): ${message}`;
        if (n === 1) logger.warn("activity", line);
        else logger.info("activity", line);
      }
      // Before anything has loaded, the first failure retries by itself and the panel
      // keeps saying "Loading…"; later failures show the error + Retry, and keep retrying.
      if (!shared.loaded && n === 1) {
        later(FIRST_READ_RETRY_MS, attempt);
        return;
      }
      publish({ error: message, degraded: { status: "failed" } });
      const step = Math.min(n - (shared.loaded ? 1 : 2), RETRY_BACKOFF_MS.length - 1);
      later(RETRY_BACKOFF_MS[Math.max(0, step)], attempt);
    };
    if (!shared.loaded) {
      timeout = later(FIRST_READ_TIMEOUT_MS, () => fail("Activity took too long to load"));
    }
    void listAttentionBoard().then(
      (nextBoard) => {
        if (timeout) {
          clearTimeout(timeout);
          shared.timers.delete(timeout);
        }
        // A late first answer is still good data, even after its timeout fired — unless
        // every reader has gone meanwhile (a new run starts over on its own).
        if (episode !== shared.episode) return;
        if (gen !== shared.gen && !(failed && !shared.loaded)) return;
        if (shared.failures > 0) {
          logger.info("activity", `Activity read recovered after ${shared.failures} failure(s)`);
        }
        shared.loaded = true;
        shared.failures = 0;
        shared.loggedReason = null;
        // The answer landed: drop any retry still scheduled.
        clearTimers();
        publish({ board: nextBoard, error: null, degraded: null });
      },
      (e) => {
        if (timeout) {
          clearTimeout(timeout);
          shared.timers.delete(timeout);
        }
        fail(e instanceof Error ? e.message : String(e));
      },
    );
  };
  clearTimers();
  attempt();
}

function startBoard(): () => void {
  // Warm on start, subscribe to the push nudge — NO poll interval (AC-6).
  reloadBoard();
  const group = createListenerGroup();
  group.track(listen(WEFT_ATTENTION_CHANGED_EVENT, () => reloadBoard()));
  // The board is read for the backend's active team: whenever the backend takes a team
  // (on a fresh profile that is after the first read, which failed with "no active team
  // yet"), re-read at once rather than wait for the next retry.
  const unsubscribe = useBackendActiveTeamStore.subscribe((next, prev) => {
    if (next.epoch !== prev.epoch) reloadBoard();
  });
  return () => {
    group.dispose();
    unsubscribe();
  };
}

/** Hold the shared reader open for one surface; the returned release lets it go. */
function retainBoard(): () => void {
  shared.users += 1;
  if (shared.users === 1) shared.stop = startBoard();
  return () => {
    shared.users -= 1;
    if (shared.users > 0) return;
    // The last surface went: stop reading and forget, as a fresh mount starts over.
    shared.episode += 1;
    shared.gen += 1;
    clearTimers();
    shared.stop?.();
    shared.stop = null;
    shared.loaded = false;
    shared.failures = 0;
    shared.loggedReason = null;
    publish(EMPTY_SNAPSHOT);
  };
}

function subscribeBoard(listener: () => void): () => void {
  shared.listeners.add(listener);
  return () => {
    shared.listeners.delete(listener);
  };
}

const getBoardSnapshot = () => shared.snapshot;

export function useAttentionBoard(): UseAttentionBoard {
  const snapshot = useSyncExternalStore(subscribeBoard, getBoardSnapshot);
  useEffect(() => retainBoard(), []);
  return { ...snapshot, reload: reloadBoard };
}

/**
 * SW-WV-F1 (ARCHITECTURE §4.3 SHARED-SURFACE) — the single YOU-OWE merge site
 * (owner: F1). SW-WV-F5 wires its approval fetcher into THIS hook (Task 7b): the
 * `task:<id>` attention rows and the `approval:<id>` endorsement rows are merged here so
 * the two sub-lists can never fork into an undifferentiated list. F5 also subscribes to
 * the endorsement lifecycle events (Task 6b) so a ratify/revoke re-fetches the approvals
 * without a poll.
 */
export type WeftYouOweRow =
  | { kind: "attention"; row: AttentionRowDto }
  | { kind: "approval"; row: WvApprovalRow };

export interface UseWeftYouOwe {
  rows: WeftYouOweRow[];
  reload: () => void;
}

/** SW-WV-F1/F5: the stable YOU-OWE key for a merged row. Attention rows key `task:<id>`;
 *  approval rows key `approval:<id>` — the two prefixes guarantee no collision. */
export function weftYouOweKey(row: WeftYouOweRow): string {
  return row.kind === "approval" ? `approval:${row.row.id}` : `task:${row.row.id}`;
}

export function useWeftYouOwe(): UseWeftYouOwe {
  const { board, reload } = useAttentionBoard();
  const [approvals, setApprovals] = useState<WvApprovalRow[]>([]);
  const genRef = useRef(0);

  const reloadApprovals = useCallback(() => {
    const gen = ++genRef.current;
    void wvListPendingApprovals().then(
      (rows) => {
        if (gen !== genRef.current) return;
        setApprovals(rows);
      },
      () => {
        // A failed approvals read leaves the attention rows intact — the endorsement
        // sub-list simply doesn't populate; no crash, no wiped board.
      },
    );
  }, []);

  useEffect(() => {
    reloadApprovals();
    // Like the board, approvals are read for the backend's active team: re-read when it
    // takes one, so a read that failed before then doesn't leave them missing.
    const unsubscribe = useBackendActiveTeamStore.subscribe((next, prev) => {
      if (next.epoch !== prev.epoch) reloadApprovals();
    });
    return () => {
      genRef.current += 1;
      unsubscribe();
    };
  }, [reloadApprovals]);

  // Task 6b: keep the approval sub-list live off the E1 assertion events — a ratify /
  // revoke (or a new post) re-reads the pending approvals without a poll.
  useWvEndorsementEvents({
    onPosted: () => reloadApprovals(),
    onRatified: () => reloadApprovals(),
    onRevoked: () => reloadApprovals(),
  });

  const combined = useCallback((): WeftYouOweRow[] => {
    const attentionRows: WeftYouOweRow[] = (board?.youOwe ?? []).map((row) => ({
      kind: "attention" as const,
      row,
    }));
    const approvalRows: WeftYouOweRow[] = approvals.map((row) => ({
      kind: "approval" as const,
      row,
    }));
    return [...attentionRows, ...approvalRows];
  }, [board?.youOwe, approvals]);

  return {
    rows: combined(),
    reload: () => {
      reload();
      reloadApprovals();
    },
  };
}
