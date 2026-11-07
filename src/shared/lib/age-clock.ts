// SPDX-License-Identifier: MIT
import { msUntilAgeChanges } from "./terminal-dot-words";

/**
 * One shared clock for every age a status dot shows. Each subscriber names
 * the moment its state began; the clock keeps a single timer, set for the
 * soonest moment any subscriber's phrased age (`formatAge`) changes — each
 * second while some age is under a minute, else each minute, else each hour —
 * and then calls every subscriber, which re-reads its phrase (a host whose
 * phrase did not change does not re-render).
 */

const subscribers = new Map<() => void, number>();
let timer: ReturnType<typeof setTimeout> | null = null;

function reschedule(): void {
  if (timer !== null) clearTimeout(timer);
  timer = null;
  if (subscribers.size === 0) return;
  const now = Date.now();
  let wait = Number.POSITIVE_INFINITY;
  for (const since of subscribers.values()) wait = Math.min(wait, msUntilAgeChanges(now - since));
  timer = setTimeout(tick, wait);
}

function tick(): void {
  timer = null;
  for (const listener of [...subscribers.keys()]) listener();
  reschedule();
}

/** Call `listener` whenever an age counted from `since` may read differently.
 *  Returns the unsubscribe. */
export function subscribeToAge(since: number, listener: () => void): () => void {
  subscribers.set(listener, since);
  reschedule();
  return () => {
    subscribers.delete(listener);
    reschedule();
  };
}
