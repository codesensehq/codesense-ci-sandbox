// SPDX-License-Identifier: MIT
/**
 * SW-WC-3 — relative time formatter for the chat pane.
 *
 * Chat conventions: "just now" / "Nm ago" / "Nh ago" / "HH:MM" for same-day
 * older / "MMM D" for older. The mocks tag each message with a `title` that
 * carries the full absolute timestamp so hovering clarifies the fuzzy label.
 *
 * Pure — safe to import from tests. Deterministic given `now`.
 */
export function formatMessageTime(createdAtMs: number, now: number = Date.now()): string {
  const diff = Math.max(0, now - createdAtMs);
  const seconds = Math.floor(diff / 1000);
  if (seconds < 45) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 12) return `${hours}h ago`;
  const date = new Date(createdAtMs);
  const nowDate = new Date(now);
  const sameDay =
    date.getFullYear() === nowDate.getFullYear() &&
    date.getMonth() === nowDate.getMonth() &&
    date.getDate() === nowDate.getDate();
  if (sameDay) {
    const hh = String(date.getHours()).padStart(2, "0");
    const mm = String(date.getMinutes()).padStart(2, "0");
    return `${hh}:${mm}`;
  }
  const months = [
    "Jan",
    "Feb",
    "Mar",
    "Apr",
    "May",
    "Jun",
    "Jul",
    "Aug",
    "Sep",
    "Oct",
    "Nov",
    "Dec",
  ];
  return `${months[date.getMonth()]} ${date.getDate()}`;
}
