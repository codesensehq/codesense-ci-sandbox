// SPDX-License-Identifier: MIT
/**
 * The worktree editor's open-tab list as plain data: which files are open, in
 * what order, and which one a close hands focus to. Pure so the ordering rules
 * are tested without rendering the editor.
 */

/** Open tabs past this evict the leftmost CLEAN, inactive tab (new tabs open
 *  beside the active one, so strip order is not open order) — each tab keeps
 *  a live editor, so the set stays bounded. A dirty tab is never evicted (its
 *  edits would be lost). */
export const MAX_EDITOR_TABS = 10;

export interface OpenTabOptions {
  /** The currently active tab; the new tab is inserted right after it. */
  active: string | null;
  /** Whether a tab holds unsaved edits (never evicted). */
  isDirty: (path: string) => boolean;
  max?: number;
}

/** Open `path` (or keep it where it is if already open). */
export function openTab(tabs: readonly string[], path: string, opts: OpenTabOptions): string[] {
  if (tabs.includes(path)) return [...tabs];
  const at = opts.active ? tabs.indexOf(opts.active) : -1;
  const next = [...tabs];
  next.splice(at === -1 ? next.length : at + 1, 0, path);
  const max = opts.max ?? MAX_EDITOR_TABS;
  while (next.length > max) {
    const victim = next.find((t) => t !== path && t !== opts.active && !opts.isDirty(t));
    if (!victim) break; // everything else is dirty or pinned by focus: allow overflow
    next.splice(next.indexOf(victim), 1);
  }
  return next;
}

/** Close `path`; when it was active, focus moves to its right neighbour (or
 *  the left one at the end), matching common editors. */
export function closeTab(
  tabs: readonly string[],
  path: string,
  active: string | null,
): { tabs: string[]; active: string | null } {
  const index = tabs.indexOf(path);
  if (index === -1) return { tabs: [...tabs], active };
  const next = tabs.filter((t) => t !== path);
  if (active !== path) return { tabs: next, active };
  return { tabs: next, active: next[index] ?? next[index - 1] ?? null };
}

/** The tab `delta` steps from `active`, wrapping (⌃Tab / ⌃⇧Tab). */
export function cycleTab(
  tabs: readonly string[],
  active: string | null,
  delta: number,
): string | null {
  if (tabs.length === 0) return null;
  const index = active ? tabs.indexOf(active) : -1;
  const from = index === -1 ? 0 : index;
  return tabs[(((from + delta) % tabs.length) + tabs.length) % tabs.length] ?? null;
}

/** Paths that share a basename get their parent directory appended so two
 *  `index.ts` tabs stay distinguishable. Keyed by full path. */
export function tabLabels(
  tabs: readonly string[],
): Map<string, { name: string; hint: string | null }> {
  const base = (p: string) => p.slice(p.lastIndexOf("/") + 1);
  const counts = new Map<string, number>();
  for (const t of tabs) counts.set(base(t), (counts.get(base(t)) ?? 0) + 1);
  const out = new Map<string, { name: string; hint: string | null }>();
  for (const t of tabs) {
    const name = base(t);
    let hint: string | null = null;
    if ((counts.get(name) ?? 0) > 1) {
      const slash = t.lastIndexOf("/");
      const dir = slash === -1 ? "" : t.slice(0, slash);
      hint = dir.slice(dir.lastIndexOf("/") + 1) || null;
    }
    out.set(t, { name, hint });
  }
  return out;
}

/** How many tabs sit (even partly) outside the strip's visible window, on each
 *  side — the overflow menu's count and the edge fades. `spans` are each tab's
 *  left/right in the strip's scroll coordinates; a pixel of overlap is noise. */
export function tabsOutOfView(
  spans: readonly { left: number; right: number }[],
  scrollLeft: number,
  width: number,
): { before: number; after: number } {
  let before = 0;
  let after = 0;
  for (const s of spans) {
    if (s.left < scrollLeft - 1) before++;
    else if (s.right > scrollLeft + width + 1) after++;
  }
  return { before, after };
}
