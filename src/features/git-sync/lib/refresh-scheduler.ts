// SPDX-License-Identifier: MIT
import { useCallback, useEffect, useRef, useState } from "react";

/**
 * When a burst of file changes settles, re-read once: a trailing debounce, so a run of writes
 * is one read, capped by a max wait, so writes that never stop still show within it.
 */
export const REFRESH_DEBOUNCE_MS = 400;
export const REFRESH_MAX_WAIT_MS = 2_000;

export interface RefreshScheduler {
  /** Something changed: read again once the burst settles. While inactive, remember it. */
  markDirty: () => void;
  /** Read now (a user action or a new source), still one read at a time. */
  refreshNow: () => void;
  /** Something changed and its burst has already settled: read now while active, else
   *  remember it like {@link markDirty}. */
  changedNow: () => void;
  /** Whether reads may run. Becoming active with a change pending reads once, straight away. */
  setActive: (active: boolean) => void;
  /** The least time between the starts of two reads the scheduler starts on its own (0 = no
   *  floor). `refreshNow` and becoming active are not held back by it. */
  setMinInterval: (ms: number) => void;
  /** When the last read started (0 = never). */
  lastReadAt: () => number;
  dispose: () => void;
}

/**
 * One read at a time over one source. Changes that land while a read is in flight mark it
 * dirty, and the read is followed by exactly one more, debounced like any other change. An
 * inactive scheduler (hidden window, collapsed view) never reads; it keeps the dirty mark for
 * the moment it becomes active.
 */
export function createRefreshScheduler(
  read: () => Promise<unknown>,
  options: { active: boolean; debounceMs?: number; maxWaitMs?: number },
): RefreshScheduler {
  const debounceMs = options.debounceMs ?? REFRESH_DEBOUNCE_MS;
  const maxWaitMs = options.maxWaitMs ?? REFRESH_MAX_WAIT_MS;
  let active = options.active;
  let dirty = false;
  let urgent = false;
  let inFlight = false;
  let disposed = false;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let firstDirtyAt: number | null = null;
  let lastReadAt = 0;
  let minIntervalMs = 0;

  const clearTimer = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
  };

  const start = () => {
    clearTimer();
    dirty = false;
    urgent = false;
    firstDirtyAt = null;
    inFlight = true;
    lastReadAt = Date.now();
    void Promise.resolve()
      .then(read)
      .catch(() => {
        // The reader keeps its last good answer; a failed read is not retried in a loop.
      })
      .finally(() => {
        inFlight = false;
        if (disposed || !dirty || !active) return;
        if (urgent) start();
        else arm();
      });
  };

  const arm = () => {
    if (disposed || !active || inFlight) return;
    const now = Date.now();
    firstDirtyAt ??= now;
    clearTimer();
    const settled = Math.min(debounceMs, firstDirtyAt + maxWaitMs - now);
    const floor = lastReadAt + minIntervalMs - now;
    const wait = Math.max(0, settled, floor);
    timer = setTimeout(() => {
      timer = null;
      if (!disposed && active && dirty && !inFlight) start();
    }, wait);
  };

  return {
    markDirty: () => {
      if (disposed) return;
      dirty = true;
      if (!active) return;
      if (inFlight) {
        firstDirtyAt ??= Date.now();
        return;
      }
      arm();
    },
    refreshNow: () => {
      if (disposed) return;
      dirty = true;
      urgent = true;
      if (!inFlight) start();
    },
    changedNow: () => {
      if (disposed) return;
      dirty = true;
      if (!active) return;
      urgent = true;
      if (!inFlight) start();
    },
    setActive: (next) => {
      if (disposed || next === active) return;
      active = next;
      if (!active) {
        clearTimer();
        firstDirtyAt = null;
        return;
      }
      if (dirty && !inFlight) start();
    },
    setMinInterval: (ms) => {
      if (disposed || ms === minIntervalMs) return;
      minIntervalMs = ms;
      if (dirty && timer !== null) arm();
    },
    lastReadAt: () => lastReadAt,
    dispose: () => {
      disposed = true;
      clearTimer();
    },
  };
}

/** A fallback for changes no event reported: regaining focus re-reads when the last read is
 *  older than this. */
const FOCUS_STALE_MS = 30_000;

export interface ScheduledRead {
  /** True until the current source's first read lands. The last answer stays rendered across
   *  every later read, so a background refresh never shows a spinner. */
  loading: boolean;
  /** Read now, bypassing the debounce. */
  refresh: () => void;
  /** Something changed: read once the burst settles (or when next active). */
  markDirty: () => void;
  /** Something changed and its burst has already settled: read now (or when next active). */
  changedNow: () => void;
}

/**
 * A {@link createRefreshScheduler} per source. A new `sourceKey` (null = no source) reads at
 * once whatever `active` says; every change of `trigger` after that marks it dirty, and every
 * change of `urgentTrigger` (a change already settled upstream) reads straight away. `read`
 * gets an `isCurrent()` that turns false once the source has moved on, so a late answer for
 * an old source is dropped.
 */
export function useScheduledRead(
  sourceKey: string | null,
  read: (isCurrent: () => boolean) => Promise<void>,
  options: {
    active: boolean;
    trigger?: unknown;
    /** Like `trigger`, for changes whose burst has already settled: no debounce. */
    urgentTrigger?: unknown;
    refreshOnFocus?: boolean;
    /** While active, also re-read this often, for changes no event reports. */
    pollMs?: number;
    /** The least time between two reads the scheduler starts on its own. */
    minIntervalMs?: number;
  },
): ScheduledRead {
  const {
    active,
    trigger,
    urgentTrigger,
    refreshOnFocus = false,
    pollMs,
    minIntervalMs = 0,
  } = options;
  const readRef = useRef(read);
  const activeRef = useRef(active);
  const triggerRef = useRef(trigger);
  const urgentTriggerRef = useRef(urgentTrigger);
  // Before the effects below, so each sees this render's values.
  useEffect(() => {
    readRef.current = read;
    activeRef.current = active;
    triggerRef.current = trigger;
    urgentTriggerRef.current = urgentTrigger;
  });
  const seenTrigger = useRef(trigger);
  const seenUrgentTrigger = useRef(urgentTrigger);
  const schedulerRef = useRef<RefreshScheduler | null>(null);
  const [loadedSource, setLoadedSource] = useState<string | null | undefined>(undefined);

  useEffect(() => {
    if (sourceKey === null) return;
    let current = true;
    const isCurrent = () => current;
    const scheduler = createRefreshScheduler(
      () =>
        readRef.current(isCurrent).finally(() => {
          if (current) setLoadedSource(sourceKey);
        }),
      { active: activeRef.current },
    );
    schedulerRef.current = scheduler;
    // The first read already sees everything the trigger has said so far.
    seenTrigger.current = triggerRef.current;
    seenUrgentTrigger.current = urgentTriggerRef.current;
    scheduler.refreshNow();
    return () => {
      current = false;
      scheduler.dispose();
      if (schedulerRef.current === scheduler) schedulerRef.current = null;
    };
  }, [sourceKey]);

  useEffect(() => {
    schedulerRef.current?.setActive(active);
  }, [active, sourceKey]);

  useEffect(() => {
    schedulerRef.current?.setMinInterval(minIntervalMs);
  }, [minIntervalMs, sourceKey]);

  useEffect(() => {
    if (!active || !pollMs || sourceKey === null) return;
    const id = setInterval(() => schedulerRef.current?.markDirty(), pollMs);
    return () => clearInterval(id);
  }, [active, pollMs, sourceKey]);

  useEffect(() => {
    if (Object.is(seenTrigger.current, trigger)) return;
    seenTrigger.current = trigger;
    schedulerRef.current?.markDirty();
  }, [trigger]);

  useEffect(() => {
    if (Object.is(seenUrgentTrigger.current, urgentTrigger)) return;
    seenUrgentTrigger.current = urgentTrigger;
    schedulerRef.current?.changedNow();
  }, [urgentTrigger]);

  useEffect(() => {
    if (!refreshOnFocus) return;
    const onFocus = () => {
      const s = schedulerRef.current;
      if (s && Date.now() - s.lastReadAt() > FOCUS_STALE_MS) s.markDirty();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refreshOnFocus]);

  const refresh = useCallback(() => schedulerRef.current?.refreshNow(), []);
  const markDirty = useCallback(() => schedulerRef.current?.markDirty(), []);
  const changedNow = useCallback(() => schedulerRef.current?.changedNow(), []);
  return {
    loading: sourceKey !== null && loadedSource !== sourceKey,
    refresh,
    markDirty,
    changedNow,
  };
}
