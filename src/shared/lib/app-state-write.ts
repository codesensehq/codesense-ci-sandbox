// SPDX-License-Identifier: MIT
/** All app-state read/modify/write callers share this queue so disjoint keys survive. */
let writeTail: Promise<void> = Promise.resolve();
export function serializeAppStateWrite(write: () => Promise<void>): Promise<void> {
  const next = writeTail.then(write);
  // Callers retain/report failures; one failed write must not poison later attempts.
  writeTail = next.catch(() => undefined);
  return next;
}
