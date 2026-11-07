// SPDX-License-Identifier: MIT
/** The ink for each state word: utilities in `styles/status-tones.css`, built from the app's
 *  status tokens. */
export const INK = {
  needs: "text-needs-ink",
  busy: "text-busy-ink",
  warn: "text-warning-ink",
  failed: "text-failed-ink",
} as const;

/** A faint wash behind a row that waits on you. */
export const NEEDS_WASH = "bg-needs-wash";
