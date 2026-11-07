// SPDX-License-Identifier: MIT
/**
 * SW-CN-13 (AC-3): the SE-4 freshness stamp. Render a poll's epoch-ms
 * `polledAtMs` as a relative "updated Ns ago" label against `nowMs`.
 *
 * Pure, so a SINGLE shared `useNow` ticker can drive every connection row without
 * a per-row `setInterval` (no render thrash). A stalled stream reads visibly stale
 * because the number keeps climbing while the health state stays `Error`/`Empty`.
 * The backend only ever emits the epoch-ms; all relative formatting is FE-side.
 */
export function formatUpdatedAgo(polledAtMs: number, nowMs: number): string {
  return `updated ${formatAgo(polledAtMs, nowMs)}`;
}

/**
 * Bare relative-time core shared by every "N ago" label: `"just now"` under 5s,
 * then `"30s ago"` / `"2m ago"` / `"3h ago"` / `"5d ago"`. Pure, so a single shared
 * `useNow` ticker can drive many rows without per-row intervals. Extracted from
 * [`formatUpdatedAgo`] (which just prefixes `"updated "`) so the RF-perf-24 worktree
 * "Synced … · <ago>" sub-line reads the exact same cadence without a second formatter.
 */
export function formatAgo(sinceMs: number, nowMs: number): string {
  const duration = formatDuration(sinceMs, nowMs);
  return duration === "just now" ? duration : `${duration} ago`;
}

/**
 * The future-facing sibling of [`formatAgo`]: a countdown to `futureMs` in the SAME
 * `s`/`m`/`h`/`d` vocabulary — `"in 45s"` / `"in 2m"`, or `"due now"` once the target
 * is within 5s or already past. Pure, so the shared `useNow` ticker drives it with no
 * per-row timer (SW-AU-85D's "next check" label).
 */
export function formatIn(futureMs: number, nowMs: number): string {
  const secs = Math.round((futureMs - nowMs) / 1000);
  if (secs <= 5) return "due now";
  // `formatDuration(nowMs, futureMs)` yields the remaining `futureMs - nowMs`.
  return `in ${formatDuration(nowMs, futureMs)}`;
}

function formatDuration(sinceMs: number, nowMs: number): string {
  const secs = Math.max(0, Math.round((nowMs - sinceMs) / 1000));
  if (secs < 5) return "just now";
  if (secs < 60) return `${secs}s`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  return `${days}d`;
}

/**
 * SW-OR-19: the ONE "last output Ns ago" liveness label, shared by the fleet-glance
 * Agents-panel row and OR-13's open "View output" panel so the two heartbeats read the
 * exact same cadence and can never contradict for the same run. Prefixes [`formatAgo`]
 * with `"last output "` — pure, so the shared `useNowClock` ticker drives it with no
 * per-row timer.
 */
export function formatLastOutput(lastOutputMs: number, nowMs: number): string {
  return `last output ${formatAgo(lastOutputMs, nowMs)}`;
}
