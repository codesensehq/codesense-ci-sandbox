// SPDX-License-Identifier: MIT
import { listen } from "@tauri-apps/api/event";
import { create } from "zustand";

import { createListenerGroup, type ListenerGroup } from "@/shared/lib/tauri-listeners";
import {
  SESSION_TIMERS_CHANGED_EVENT,
  WAKE_DELIVERED_EVENT,
  WAKE_HISTORY_CAP,
  WAKE_OUTCOME_EVENT,
  type SessionTimersChanged,
  type WakeDeliveredEvent,
  type WakeHistoryEntry,
  type WakeOutcomeEvent,
} from "@/shared/lib/wake-events";
import {
  cancelSessionTimer,
  remindSession,
  sessionTimers,
  wakeHistory,
  type ScheduledSessionTimer,
  type SessionTimerAsk,
  type SessionTimerRow,
} from "@/tauri/commands";

import { terminalManager } from "@/shared/terminal/terminal-manager";

import {
  LOCAL_TIMER_MACHINE,
  authorWords,
  shortTime,
  timerTargetKey,
  type TimerTarget,
} from "../lib/session-timers";
import { annotateDelivery, wakeMarkerLabel } from "../lib/wake-marker";

/** How long a tab's caption says "Woken by ‹name›" after a delivery (§4.14). */
export const WOKEN_CAPTION_MS = 10_000;

/** One tab's timers, as its terminal server last answered. */
export interface SessionTimersEntry {
  /** `null` until the first read lands. */
  rows: SessionTimerRow[] | null;
  /** The last read's refusal, in words (an offline machine, …). */
  error: string | null;
  loading: boolean;
  /** The machine's terminal server is too old for timers. */
  oldServer: boolean;
}

/** The caption's "Woken by" while it shows. */
export interface WokenCaption {
  marker: string;
  deliveryId: string;
  at: number;
  /** The delivery, for the scrollback marker. */
  event: WakeDeliveredEvent;
}

interface SessionTimersState {
  /** By {@link timerTargetKey}. */
  bySession: Record<string, SessionTimersEntry>;
  /** Each tab's last deliveries, newest first (at most 20). */
  historyBySession: Record<string, WakeHistoryEntry[]>;
  /** Tabs whose caption says "Woken by" right now. */
  woken: Record<string, WokenCaption>;
  /** The Wake history disclosure, open or closed, per tab. */
  historyOpen: Record<string, boolean>;
  /** Timers cancelled from the popover, hidden while their Undo toast is up
   *  (the real cancel happens when it closes), by tab key. */
  cancelling: Record<string, string[]>;

  /** Re-read a tab's timers. Unforced, a read already in flight answers it. */
  refresh: (target: TimerTarget, opts?: { force?: boolean }) => Promise<void>;
  /** Hide a timer now; it is cancelled for real by {@link commitCancel}
   *  (when the Undo toast closes) unless {@link undoCancel} comes first. */
  cancel: (target: TimerTarget, timerId: string) => void;
  /** Cancel a hidden timer on its terminal server; rejects with its words. */
  commitCancel: (target: TimerTarget, timerId: string) => Promise<void>;
  /** Show a hidden timer again: nothing was cancelled. */
  undoCancel: (target: TimerTarget, timerId: string) => void;
  /** Set a timer as the person; then re-read the tab's timers. */
  remind: (target: TimerTarget, ask: SessionTimerAsk) => Promise<ScheduledSessionTimer>;
  loadHistory: (target: TimerTarget) => Promise<void>;
  /** The tab's session ended: its timers ended with it. */
  forget: (target: TimerTarget) => void;
  setHistoryOpen: (target: TimerTarget, open: boolean) => void;
}

const UPDATE_REQUIRED = "Update the terminal server";

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const EMPTY: SessionTimersEntry = { rows: null, error: null, loading: false, oldServer: false };

/** Reads by key: a reply lands only if no newer read for that tab was issued. */
const readSeq: Record<string, number> = {};
const historySeq: Record<string, number> = {};
const wokenTimers = new Map<string, ReturnType<typeof setTimeout>>();

function machineArg(target: TimerTarget): string | undefined {
  return target.machineId && target.machineId !== LOCAL_TIMER_MACHINE
    ? target.machineId
    : undefined;
}

export const useSessionTimersStore = create<SessionTimersState>((set, get) => {
  const dropCancelling = (key: string, timerId: string) =>
    set((s) => {
      const left = (s.cancelling[key] ?? []).filter((id) => id !== timerId);
      const cancelling = { ...s.cancelling };
      if (left.length) cancelling[key] = left;
      else delete cancelling[key];
      return { cancelling };
    });
  const patch = (key: string, next: Partial<SessionTimersEntry>) =>
    set((s) => ({
      bySession: { ...s.bySession, [key]: { ...(s.bySession[key] ?? EMPTY), ...next } },
    }));

  return {
    bySession: {},
    historyBySession: {},
    woken: {},
    historyOpen: {},
    cancelling: {},

    refresh: async (target, opts) => {
      const key = timerTargetKey(target);
      if (!opts?.force && get().bySession[key]?.loading) return;
      const seq = (readSeq[key] = (readSeq[key] ?? 0) + 1);
      patch(key, { loading: true });
      try {
        const rows = await sessionTimers(target.sessionId, machineArg(target));
        if (readSeq[key] !== seq) return;
        patch(key, { rows, error: null, loading: false, oldServer: false });
      } catch (e) {
        if (readSeq[key] !== seq) return;
        const error = errorText(e);
        // Keep the last-known rows: an offline machine's timers keep firing there.
        patch(key, { error, loading: false, oldServer: error.startsWith(UPDATE_REQUIRED) });
      }
    },

    cancel: (target, timerId) => {
      const key = timerTargetKey(target);
      set((s) => ({
        cancelling: {
          ...s.cancelling,
          [key]: [...(s.cancelling[key] ?? []).filter((id) => id !== timerId), timerId],
        },
      }));
    },

    commitCancel: async (target, timerId) => {
      const key = timerTargetKey(target);
      // Undone meanwhile: nothing to cancel.
      if (!get().cancelling[key]?.includes(timerId)) return;
      try {
        await cancelSessionTimer(target.sessionId, timerId, machineArg(target));
        // Gone: drop the row before it is shown again.
        const rows = get().bySession[key]?.rows;
        if (rows) patch(key, { rows: rows.filter((r) => r.timerId !== timerId) });
      } finally {
        dropCancelling(key, timerId);
        if (key in get().bySession) void get().refresh(target, { force: true });
      }
    },

    undoCancel: (target, timerId) => dropCancelling(timerTargetKey(target), timerId),

    remind: async (target, ask) => {
      const set = await remindSession(target.sessionId, ask, machineArg(target));
      await get().refresh(target, { force: true });
      return set;
    },

    loadHistory: async (target) => {
      const key = timerTargetKey(target);
      const seq = (historySeq[key] = (historySeq[key] ?? 0) + 1);
      try {
        const entries = await wakeHistory(target.sessionId, machineArg(target));
        if (historySeq[key] !== seq) return;
        const newest = [...entries].sort((a, b) => b.atMs - a.atMs).slice(0, WAKE_HISTORY_CAP);
        set((s) => ({ historyBySession: { ...s.historyBySession, [key]: newest } }));
      } catch {
        // History is supplementary; a failed read keeps what was shown.
      }
    },

    forget: (target) => {
      const key = timerTargetKey(target);
      if (!(key in get().bySession)) return;
      readSeq[key] = (readSeq[key] ?? 0) + 1;
      set((s) => {
        const bySession = { ...s.bySession };
        delete bySession[key];
        const cancelling = { ...s.cancelling };
        delete cancelling[key];
        return { bySession, cancelling };
      });
    },

    setHistoryOpen: (target, open) =>
      set((s) => ({ historyOpen: { ...s.historyOpen, [timerTargetKey(target)]: open } })),
  };
});

function targetOf(event: { machineId: string; sessionId: string }): TimerTarget {
  return { sessionId: event.sessionId, machineId: event.machineId };
}

/** The timer a delivery came from, as its tab lists it: who set it. */
function timerAuthor(key: string, event: WakeDeliveredEvent): string | null {
  if (event.origin.kind !== "timer" || !event.origin.timerId) return null;
  const id = event.origin.timerId;
  const row = useSessionTimersStore.getState().bySession[key]?.rows?.find((r) => r.timerId === id);
  return row ? authorWords(row.createdBy) : null;
}

/** A timer delivery's fire number: the one after the fires the tab had counted
 *  when the event arrived (read before this delivery's refresh). An exact
 *  number only — never the previous fire's — so a recurring timer's marker
 *  cannot land on an older header; undefined when the tab does not list it. */
function timerFires(key: string, event: WakeDeliveredEvent): number[] | undefined {
  if (event.origin.kind !== "timer" || !event.origin.timerId) return undefined;
  const id = event.origin.timerId;
  const row = useSessionTimersStore.getState().bySession[key]?.rows?.find((r) => r.timerId === id);
  return row ? [row.fires + 1] : undefined;
}

/** A delivery: caption it for 10 s and mark the tab's scrollback where it
 *  went in (unless it is a re-read), then re-read the tab's timers (a
 *  one-shot is gone) and its history. The marker goes into the tab's display
 *  whether it is shown or parked behind another tab. */
function onDelivered(event: WakeDeliveredEvent): void {
  const target = targetOf(event);
  const key = timerTargetKey(target);
  const store = useSessionTimersStore.getState();
  if (!event.replayed) {
    annotateDelivery(
      event,
      wakeMarkerLabel(event, shortTime, timerAuthor(key, event)),
      (machine, session) => terminalManager.terminalFor(machine, session),
      { fires: timerFires(key, event) },
    );
    useSessionTimersStore.setState((s) => ({
      woken: {
        ...s.woken,
        [key]: {
          marker: event.marker,
          deliveryId: event.deliveryId,
          at: event.deliveredAtMs,
          event,
        },
      },
    }));
    const prior = wokenTimers.get(key);
    if (prior) clearTimeout(prior);
    wokenTimers.set(
      key,
      setTimeout(() => {
        wokenTimers.delete(key);
        useSessionTimersStore.setState((s) => {
          const woken = { ...s.woken };
          delete woken[key];
          return { woken };
        });
      }, WOKEN_CAPTION_MS),
    );
  }
  // Only tabs whose timers are shown (an agent tab, read by the loader).
  if (key in store.bySession) void store.refresh(target, { force: true });
  void store.loadHistory(target);
}

/** Any final status of a delivery into a tab whose history is shown. */
function onOutcome(event: WakeOutcomeEvent): void {
  const target = targetOf(event);
  const key = timerTargetKey(target);
  const store = useSessionTimersStore.getState();
  if (key in store.historyBySession) void store.loadHistory(target);
  // A timer's fire may have been its last.
  if (event.origin.kind === "timer" && key in store.bySession)
    void store.refresh(target, { force: true });
}

/** Commit every cancel still waiting on its Undo toast: the page is going
 *  away (reload, window close), so the toast will never close by itself. */
export function flushPendingCancels(): void {
  const { cancelling, commitCancel } = useSessionTimersStore.getState();
  for (const [key, ids] of Object.entries(cancelling)) {
    const at = key.indexOf("|");
    const target = { machineId: key.slice(0, at), sessionId: key.slice(at + 1) };
    for (const id of ids) void commitCancel(target, id).catch(() => {});
  }
}

let watchers = 0;
let listeners: ListenerGroup | null = null;

/** Listen for timer changes and deliveries while any tab shows timers. */
export function watchSessionTimers(): () => void {
  watchers += 1;
  if (!listeners) {
    listeners = createListenerGroup();
    window.addEventListener("pagehide", flushPendingCancels);
    listeners.track(
      listen<SessionTimersChanged>(SESSION_TIMERS_CHANGED_EVENT, (e) => {
        const store = useSessionTimersStore.getState();
        const target = targetOf(e.payload);
        if (timerTargetKey(target) in store.bySession) void store.refresh(target, { force: true });
      }),
    );
    listeners.track(
      listen<WakeDeliveredEvent>(WAKE_DELIVERED_EVENT, (e) => onDelivered(e.payload)),
    );
    listeners.track(listen<WakeOutcomeEvent>(WAKE_OUTCOME_EVENT, (e) => onOutcome(e.payload)));
  }
  let stopped = false;
  return () => {
    if (stopped) return;
    stopped = true;
    watchers -= 1;
    if (watchers === 0 && listeners) {
      window.removeEventListener("pagehide", flushPendingCancels);
      listeners.dispose();
      listeners = null;
    }
  };
}

export function resetSessionTimersForTests(): void {
  window.removeEventListener("pagehide", flushPendingCancels);
  listeners?.dispose();
  listeners = null;
  watchers = 0;
  for (const t of wokenTimers.values()) clearTimeout(t);
  wokenTimers.clear();
  for (const k of Object.keys(readSeq)) delete readSeq[k];
  for (const k of Object.keys(historySeq)) delete historySeq[k];
  useSessionTimersStore.setState({
    bySession: {},
    historyBySession: {},
    woken: {},
    historyOpen: {},
  });
}

/** A tab's timers as shown: without those whose cancel waits on its Undo. */
export function visibleTimers(
  rows: SessionTimerRow[] | null | undefined,
  cancelling: string[] | undefined,
): SessionTimerRow[] | null {
  if (!rows) return null;
  return cancelling?.length ? rows.filter((r) => !cancelling.includes(r.timerId)) : rows;
}
