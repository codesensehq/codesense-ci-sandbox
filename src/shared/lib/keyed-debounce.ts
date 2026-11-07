// SPDX-License-Identifier: MIT
/**
 * A keyed debounce/throttle — each unique key gets its own timer, so bursts
 * from different identifiers coalesce independently.
 *
 * Two modes (SW-SY-4):
 *
 * - **Trailing** (default, `leading: false`): `schedule(key, payload)` resets
 *   the timer for that key and stores the payload, overwriting any prior queued
 *   payload. After `windowMs` of inactivity the callback fires once with the
 *   most recent payload. This is the classic debounce — good for "settle after
 *   the user stops", bad under a *sustained* burst (the timer resets on every
 *   event and never fires until the burst ends → starved-then-stampede).
 *
 * - **Leading** (`leading: true`): the FIRST `schedule` fires the callback
 *   immediately (leading edge), then opens a `windowMs` window. Events during
 *   the window coalesce into a single trailing payload that flushes when the
 *   window elapses; if the burst is still going the window re-arms, producing a
 *   bounded, regular cadence (≈ one fire per window) instead of a stampede.
 *   When the burst stops, a final trailing flush lands the last state and the
 *   window closes. Use this on hot burst paths that must stay responsive.
 *
 * Both modes suppress an `undefined` payload (never fire the callback with it) —
 * callers that carry no data pass the key itself as the payload.
 *
 * Distinct from `createDebouncedAction` (single shared timer) — use this when
 * bursts from different identifiers must be coalesced independently.
 */
export interface KeyedDebounceOptions {
  /** Fire on the leading edge + coalesced trailing flushes (throttle). Default: trailing-only. */
  leading?: boolean;
}

export interface KeyedDebounce<T> {
  /** Queue `payload` for `key`. Trailing: resets the window. Leading: fires now if idle, else coalesces. */
  schedule: (key: string, payload: T) => void;
  /** Immediately fire any pending payload for `key` and clear its window. No-op if nothing pending. */
  flush: (key: string) => void;
  /** Drop any pending payload + timer for `key` without firing. No-op for an unknown key. */
  cancel: (key: string) => void;
  /** Drop every pending payload + timer without firing. */
  cancelAll: () => void;
}

export function createKeyedDebounce<T>(
  windowMs: number,
  callback: (key: string, lastPayload: T) => void,
  options?: KeyedDebounceOptions,
): KeyedDebounce<T> {
  const leading = options?.leading ?? false;
  const timers = new Map<string, ReturnType<typeof setTimeout>>();
  // Pending (not-yet-fired) payload per key. Trailing: the latest scheduled
  // payload. Leading: the latest payload seen SINCE the last fire (the trailing
  // flush candidate).
  const payloads = new Map<string, T>();

  const fire = (key: string, payload: T): void => {
    // Undefined-payload suppression: a caller that carries no data passes the
    // key as payload; a genuine `undefined` is a no-op, never a callback call.
    if (payload !== undefined) callback(key, payload);
  };

  const clearTimer = (key: string): void => {
    const existing = timers.get(key);
    if (existing != null) clearTimeout(existing);
    timers.delete(key);
  };

  // Leading mode: arm a window that, on expiry, flushes any coalesced trailing
  // payload and re-arms (sustained burst → regular cadence). With nothing
  // pending it closes the window — no stuck/looping timer once the burst ends.
  const armLeadingWindow = (key: string): void => {
    timers.set(
      key,
      setTimeout(() => {
        const pending = payloads.get(key);
        if (pending !== undefined) {
          payloads.delete(key);
          fire(key, pending);
          armLeadingWindow(key);
        } else {
          timers.delete(key);
        }
      }, windowMs),
    );
  };

  const scheduleTrailing = (key: string, payload: T): void => {
    payloads.set(key, payload);
    clearTimer(key);
    timers.set(
      key,
      setTimeout(() => {
        timers.delete(key);
        const p = payloads.get(key);
        payloads.delete(key);
        if (p !== undefined) fire(key, p);
      }, windowMs),
    );
  };

  const scheduleLeading = (key: string, payload: T): void => {
    if (!timers.has(key)) {
      // Leading edge — fire now and open the window.
      fire(key, payload);
      armLeadingWindow(key);
    } else {
      // Inside an open window — coalesce; the newest payload wins the flush.
      payloads.set(key, payload);
    }
  };

  return {
    schedule(key, payload) {
      if (leading) scheduleLeading(key, payload);
      else scheduleTrailing(key, payload);
    },
    flush(key) {
      clearTimer(key);
      const p = payloads.get(key);
      if (p !== undefined) {
        payloads.delete(key);
        fire(key, p);
      }
    },
    cancel(key) {
      clearTimer(key);
      payloads.delete(key);
    },
    cancelAll() {
      for (const t of timers.values()) clearTimeout(t);
      timers.clear();
      payloads.clear();
    },
  };
}
