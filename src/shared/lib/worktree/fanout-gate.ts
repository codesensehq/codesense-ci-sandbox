// SPDX-License-Identifier: MIT
// A FIFO-fair async concurrency gate: at most `cap` tasks run at once; the rest
// queue in arrival order and are woken oldest-first as slots free.
//
// Extracted from creation-fanout (SLW-RI-6) so the worktree-CREATE burst gate
// and the RF-perf-14 background-SETUP gate share ONE proven implementation, each
// with its own independent cap + queue state. The single-task path is unchanged:
// one task takes the first slot immediately (no queueing, no extra tick).

export interface FanoutGate {
  /**
   * Run `fn` under a slot: block until fewer than `cap` tasks are in flight,
   * then run it, releasing the slot (and waking the next waiter) when it settles
   * — success OR failure, so an aborted/failed task never leaks a slot.
   */
  withSlot<T>(fn: () => Promise<T>): Promise<T>;
  /**
   * Raise/lower the ceiling. Raising wakes exactly the new headroom of OLDEST
   * waiters in FIFO order (never a thundering herd; never over-admits — the
   * (headroom+1)th waiter re-confirms saturation). Never drops below 1.
   */
  setCap(cap: number): void;
  /** Tasks currently holding a slot (tests / observability). */
  activeCount(): number;
  /** Reset to the default cap + empty queue (tests). */
  resetForTest(): void;
}

export function createFanoutGate(defaultCap: number): FanoutGate {
  const baseCap = Math.max(1, Math.floor(defaultCap));
  let cap = baseCap;
  let active = 0;
  const waiters: Array<() => void> = [];

  return {
    async withSlot<T>(fn: () => Promise<T>): Promise<T> {
      while (active >= cap) {
        await new Promise<void>((resolve) => waiters.push(resolve));
      }
      active++;
      try {
        return await fn();
      } finally {
        active--;
        // Hand the freed slot to the oldest waiter (FIFO fairness).
        const next = waiters.shift();
        if (next) next();
      }
    },

    setCap(next: number): void {
      cap = Math.max(1, Math.floor(next));
      // Wake only as many OLDEST waiters as the raised cap now admits. Each woken
      // waiter increments `active` later (its own microtask), so we can't observe
      // the fill synchronously — bound the wakes by the headroom snapshot.
      let headroom = cap - active;
      while (headroom > 0 && waiters.length > 0) {
        waiters.shift()?.();
        headroom--;
      }
    },

    activeCount(): number {
      return active;
    },

    resetForTest(): void {
      cap = baseCap;
      active = 0;
      waiters.splice(0);
    },
  };
}
