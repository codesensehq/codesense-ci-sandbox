// SPDX-License-Identifier: MIT
import { useEffect, useMemo } from "react";

import type { TerminalTab } from "@/shared/stores/viewer-store";
import type { SessionTimerRow } from "@/tauri/commands";

import { useSessionEnd } from "../components/session-status-mark";
import { timerTargetKey, type TimerTarget } from "../lib/session-timers";
import {
  useSessionTimersStore,
  visibleTimers,
  watchSessionTimers,
  type SessionTimersEntry,
} from "../stores/session-timers-store";

/** How long after a fire is due the tab re-reads its timers: the terminal
 *  server evaluates schedules at 15-second ticks. */
const AFTER_DUE_MS = 16_000;
/** setTimeout's ceiling. */
const MAX_DELAY_MS = 2 ** 31 - 1;

/** The timer target of a tab: its session, on its machine. */
export function tabTimerTarget(tab: Pick<TerminalTab, "sessionId" | "origin">): TimerTarget {
  const machineId = tab.origin?.machineId;
  return machineId ? { sessionId: tab.sessionId, machineId } : { sessionId: tab.sessionId };
}

export interface TabTimers {
  target: TimerTarget;
  key: string;
  entry: SessionTimersEntry | undefined;
  /** The timers shown: without those whose cancel waits on its Undo. */
  rows: SessionTimerRow[];
  /** The tab's session ended: it has no timers any more. */
  ended: boolean;
  /** A remote tab whose machine is unreachable: its timers keep firing there. */
  offline: boolean;
}

/** A tab's timers as the store holds them (read-only: the loader keeps them current). */
export function useTabTimers(tab: TerminalTab): TabTimers {
  const target = useMemo(
    () => tabTimerTarget(tab),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [tab.sessionId, tab.origin?.machineId],
  );
  const key = timerTargetKey(target);
  const ended = useSessionEnd(tab.sessionId, tab.exit) !== null || !!tab.exit;
  const offline = tab.origin?.offlineSince !== undefined;
  const entry = useSessionTimersStore((s) => s.bySession[key]);
  const cancelling = useSessionTimersStore((s) => s.cancelling[key]);
  const rows = useMemo(
    () => (ended ? [] : (visibleTimers(entry?.rows, cancelling) ?? [])),
    [ended, entry?.rows, cancelling],
  );
  return { target, key, entry: ended ? undefined : entry, rows, ended, offline };
}

/**
 * Keep one agent tab's timers current (SW-AGR-6), whether or not its caption
 * is on screen: read once, again when its machine reconnects and when a fire
 * comes due; dropped when its session ends, since its timers end with it.
 * Mounted per agent tab by `SessionTimersLoader`.
 */
export function useKeepTabTimers(tab: TerminalTab): void {
  const { target, entry, ended, offline } = useTabTimers(tab);

  useEffect(() => watchSessionTimers(), []);

  useEffect(() => {
    const store = useSessionTimersStore.getState();
    if (ended) store.forget(target);
    else if (!offline) void store.refresh(target);
  }, [target, ended, offline]);

  // A due fire: re-read once the server has had its tick (a one-shot is gone).
  // A fire already due (waiting for the agent to be idle) arms nothing: the
  // delivery's own event re-reads.
  const rows = entry?.rows;
  useEffect(() => {
    if (ended || offline || !rows?.length) return;
    const now = Date.now();
    const next = Math.min(...rows.map((r) => Date.parse(r.nextAt)).filter((t) => t > now));
    if (!Number.isFinite(next)) return;
    const delay = Math.min(MAX_DELAY_MS, next - now + AFTER_DUE_MS);
    const id = setTimeout(
      () => void useSessionTimersStore.getState().refresh(target, { force: true }),
      delay,
    );
    return () => clearTimeout(id);
  }, [rows, ended, offline, target]);
}
