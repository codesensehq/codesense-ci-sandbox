// SPDX-License-Identifier: MIT
import type { StatusMarkKind } from "./status-mark";

/** Recovery qualifies the last observation; it does not replace its state. */
export function activityQualifier(
  activity: { freshness: string; snapshot: { observed: boolean } | null } | undefined,
  mark?: StatusMarkKind,
): string | undefined {
  if (!activity || mark === "offline" || mark === "ended" || mark === "crashed") return;
  const stale = activity.snapshot?.observed ? "; last observation may be stale" : "";
  if (activity.freshness === "recovering") return `Restoring agent activity${stale}`;
  if (activity.freshness === "unavailable") return `Agent activity unavailable${stale}`;
  if (activity.freshness === "upgrade-required") return `Agent activity requires an update${stale}`;
}

export function withActivityQualifiers(
  words: string,
  qualifiers: Iterable<string | undefined>,
  separator = " · ",
): string {
  return [words, ...new Set([...qualifiers].filter((value) => value !== undefined))].join(
    separator,
  );
}
