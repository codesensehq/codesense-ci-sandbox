// SPDX-License-Identifier: MIT
import type { NoteMeta } from "@/tauri/commands";

/** SW-SN-2: the rail's two-section ordering, extracted so the SW-SN-3 switcher
 *  sorts the same list identically without re-deriving it (DRY seam).
 *
 *  - PINNED first, sorted by title. v1 has no manual pin order, so title is the
 *    stable fallback (see DESIGN.md `pinned section` assumption); SW-SN-4 may
 *    introduce manual ordering later.
 *  - RECENT = the rest, most-recently-opened first (`lastOpenedAt` desc) — the
 *    same recency rule as the store's `mostRecentNote`.
 *
 *  Pure and non-mutating (copies before sorting). */
export function partitionNotes(notes: NoteMeta[]): { pinned: NoteMeta[]; recent: NoteMeta[] } {
  const pinned = notes
    .filter((n) => n.pinned)
    .slice()
    .sort((a, b) => a.title.localeCompare(b.title, undefined, { sensitivity: "base" }));
  const recent = notes
    .filter((n) => !n.pinned)
    .slice()
    .sort((a, b) => b.lastOpenedAt - a.lastOpenedAt);
  return { pinned, recent };
}

/** SW-SN-2: title-only, case-insensitive substring narrow for the in-rail
 *  filter. Body/content search is the SW-SN-11 `⌘O` switcher's job, not the
 *  rail's (EXPERIENCE.md Component Patterns). A blank query returns everything. */
export function filterByTitle(notes: NoteMeta[], query: string): NoteMeta[] {
  const q = query.trim().toLowerCase();
  if (!q) return notes;
  return notes.filter((n) => n.title.toLowerCase().includes(q));
}
