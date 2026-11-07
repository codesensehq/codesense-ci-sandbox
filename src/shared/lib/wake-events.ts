// SPDX-License-Identifier: MIT
/**
 * SW-AGR-9: the wake events the desktop emits (SPEC §9.10, R4), as the
 * frontend reads them. Pinned by the Rust golden fixture
 * `src-tauri/src/fixtures/wake/wake-events.json` (see `wake-events.test.ts`).
 *
 * - `wake-delivered`: the text was typed. Drives the scrollback annotation
 *   (`⏰ ‹marker›`) and the tab's 10-second `Woken by ‹marker›` caption.
 * - `wake-outcome`: every status a delivery reaches — `queued` (the agent
 *   row's `⏰ due`) and the final ones (the run row, the tab's wake history).
 */

export const WAKE_DELIVERED_EVENT = "wake-delivered";
export const WAKE_OUTCOME_EVENT = "wake-outcome";
export const WAKE_SYNC_HEALTH_EVENT = "wake-sync-health";
/** SW-AGR-6: a session's timers changed (set or cancelled, by an agent or the
 *  person), so its tab re-reads them. A fire is told by `wake-outcome`. */
export const SESSION_TIMERS_CHANGED_EVENT = "session-timers-changed";

/** `session-timers-changed`: which session's timers to re-read. */
export interface SessionTimersChanged {
  machineId: string;
  sessionId: string;
}
/** How many deliveries a tab's wake history keeps. */
export const WAKE_HISTORY_CAP = 20;

/** How an outcome reached the desktop. */
export type WakeOutcomeSource = "reply" | "push" | "sync" | "local";

/** Why a wake was not typed (the session server's own spelling). */
export type UndeliveredReason =
  | "busy_timeout"
  | "needs_preparation"
  | "machine_offline"
  | "machine_unknown"
  | "session_ended"
  | "unobservable"
  | "agent_failed"
  | "archived"
  | "server_restarted"
  | "limit_reached"
  | "draft_pending"
  | "unavailable";

/** A delivery's status: the run row, the events and the history all carry it. */
export type WakeRunStatus =
  | { status: "queued"; deadlineMs: number }
  | { status: "delivered"; deliveredAtMs: number }
  | { status: "coalesced"; into: string }
  | { status: "undelivered"; reason: UndeliveredReason }
  /** May have reached the session: never re-sent, never offered a Retry. */
  | { status: "unknown"; detail: string };

/** What woke the session. */
export type WakeOrigin =
  | { kind: "routine"; routineKey: string; runId: string }
  | { kind: "timer"; timerId: string | null }
  | { kind: "human" }
  /** Another desktop's delivery: shown, not recorded as a run here. */
  | { kind: "other" };

export interface WakeDeliveredEvent {
  machineId: string;
  sessionId: string;
  deliveryId: string;
  marker: string;
  caption: string;
  origin: WakeOrigin;
  deliveredAtMs: number;
  source: WakeOutcomeSource;
  /** Re-read after the desktop lost its record: show it, don't caption it anew. */
  replayed: boolean;
}

export type WakeOutcomeEvent = WakeRunStatus & {
  machineId: string;
  sessionId: string;
  /** Empty when the wake never reached a terminal server (an offline machine). */
  deliveryId: string;
  idempotencyKey: string;
  marker: string;
  origin: WakeOrigin;
  /** The run row may offer Retry: never for `unknown`, a timer's fire, an
   * unknown machine, an archived wake or a session at its delivery limit. */
  mayRetry: boolean;
  /** This desktop asked for it, so it records the run. */
  ours: boolean;
  source: WakeOutcomeSource;
  /** Re-read after the desktop lost its record: shown, never recorded. */
  replayed: boolean;
  atMs: number;
};

/** `wake-sync-health`: whether the desktop is caught up with a machine. */
export interface WakeSyncHealth {
  machineId: string;
  ok: boolean;
  failures: number;
  lastError: string | null;
  lastOkMs: number | null;
  saveError: string | null;
}

export type WakeHistoryEntry = WakeRunStatus & {
  deliveryId: string;
  marker: string;
  origin: WakeOrigin;
  atMs: number;
};

/** `agent_get.routines[].queued`: a routine's wake waiting on a busy session. */
export interface QueuedWake {
  runId: string;
  machineId: string;
  sessionId: string;
  marker: string;
  deadlineMs: number | null;
  /** Sent but never answered: show "may be due", not "due". */
  unconfirmed: boolean;
}

/** One routine of an agent in `agent_get` (SW-AGR-1's row plus `queued`). */
export interface AgentRoutineRow {
  id: string;
  name: string;
  /** `fresh` or `wake`. */
  mode: string;
  /** RFC 3339, or null when it has no scheduled fire from this Mac. */
  nextRunAt: string | null;
  /** This Mac's switch. */
  enabled: boolean;
  queued: QueuedWake | null;
}

/** The scrollback annotation line for a delivered wake. */
export const wakeAnnotation = (event: WakeDeliveredEvent): string => `⏰ ${event.marker}`;

/** The run-row copy for a status (SPEC §7). */
export const wakeStatusLabel = (status: WakeRunStatus): string => {
  switch (status.status) {
    case "queued":
      return "Queued until the agent is idle";
    case "delivered":
      return "Delivered";
    case "coalesced":
      return "Folded into the queued wake";
    case "undelivered":
      return `Not delivered (${status.reason.replace(/_/g, " ")})`;
    case "unknown":
      return "May have been delivered. Check the terminal before re-sending.";
  }
};
