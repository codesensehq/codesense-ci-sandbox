// SPDX-License-Identifier: MIT
import { useCallback, useState, type FocusEvent, type KeyboardEvent } from "react";

/**
 * SW-GS-15 — the shared roving-tabindex + arrow-key logic for a keyboard tree,
 * extracted from `install-workbench/WorkbenchTree` so both that view and the
 * git-sync changed-files nav navigate identically from ONE implementation.
 *
 * The hook is deliberately view-agnostic: it owns the active-row index + the key
 * handling and returns the handlers, but it delegates the two things that differ
 * per consumer — how a row gains DOM focus (`focusRow`: WorkbenchTree scrolls its
 * virtualizer then rAF-retries; the non-virtualized sync nav focuses synchronously)
 * and how a row expands/collapses (`toggle`: a store write vs. a local Set edit).
 * Rows are described structurally (`depth`/`expandable`/`expanded`) so the hook
 * needs no knowledge of either view's row model.
 *
 * Enter/Space activation is intentionally NOT handled here — each row's native
 * `<button>` handles it, so the consumer wires activation on the element itself.
 */
export interface RovingTreeRow {
  /** Nesting depth — drives ArrowLeft "ascend to the nearest shallower row". */
  depth: number;
  /** Whether this row can expand/collapse (a directory / a story-with-docs / the
   *  Documentation bucket). A leaf/own-file row is `false`. */
  expandable: boolean;
  /** Whether an expandable row is currently open. */
  expanded: boolean;
}

export interface UseRovingTreeNavOptions {
  /** The flat, visible rows in DOM order. Its indices MUST match the `data-index`
   *  the consumer stamps on each rendered row (that's how `onFocus` syncs). */
  rows: RovingTreeRow[];
  /** Move DOM focus to the row at `index`. The consumer owns any scroll/mount
   *  concerns (virtualized vs. plain, sticky-header scroll-margin, etc.). */
  focusRow: (index: number) => void;
  /** Expand/collapse the row at `index`. */
  toggle: (index: number) => void;
}

export interface RovingTreeNav {
  /** The active row (a single tab stop follows it). Clamped to the current rows. */
  activeIndex: number;
  onKeyDown: (e: KeyboardEvent<HTMLElement>) => void;
  onFocus: (e: FocusEvent<HTMLElement>) => void;
  /** `0` for the active row, `-1` for the rest — spread onto each row's control. */
  tabIndexFor: (index: number) => 0 | -1;
}

export function useRovingTreeNav({
  rows,
  focusRow,
  toggle,
}: UseRovingTreeNavOptions): RovingTreeNav {
  const [activeIndex, setActiveIndex] = useState(0);
  // Clamp at render so a shrunk visible set (filter / collapse) keeps a valid tab
  // stop without a corrective effect.
  const activeRowIndex = rows.length === 0 ? -1 : Math.min(activeIndex, rows.length - 1);

  const moveActive = useCallback(
    (index: number) => {
      const clamped = Math.max(0, Math.min(rows.length - 1, index));
      setActiveIndex(clamped);
      focusRow(clamped);
    },
    [rows.length, focusRow],
  );

  const onKeyDown = useCallback(
    (e: KeyboardEvent<HTMLElement>) => {
      const row = rows[activeRowIndex];
      switch (e.key) {
        case "ArrowDown":
          e.preventDefault();
          moveActive(activeRowIndex + 1);
          break;
        case "ArrowUp":
          e.preventDefault();
          moveActive(activeRowIndex - 1);
          break;
        case "Home":
          e.preventDefault();
          moveActive(0);
          break;
        case "End":
          e.preventDefault();
          moveActive(rows.length - 1);
          break;
        case "ArrowRight":
          if (row?.expandable) {
            e.preventDefault();
            if (!row.expanded) toggle(activeRowIndex);
            else moveActive(activeRowIndex + 1); // already open → step into first child
          }
          break;
        case "ArrowLeft":
          if (!row) break;
          e.preventDefault();
          if (row.expandable && row.expanded) {
            toggle(activeRowIndex); // collapse in place
          } else {
            // Jump to the nearest shallower ancestor row.
            for (let i = activeRowIndex - 1; i >= 0; i--) {
              if (rows[i].depth < row.depth) {
                moveActive(i);
                break;
              }
            }
          }
          break;
      }
    },
    [rows, activeRowIndex, moveActive, toggle],
  );

  // Keep the roving tab stop in sync with whatever the user actually focuses — a
  // mouse click or a Tab into a row, not just arrow moves.
  const onFocus = useCallback((e: FocusEvent<HTMLElement>) => {
    const indexAttr = (e.target as HTMLElement).closest("[data-index]")?.getAttribute("data-index");
    if (indexAttr != null) {
      const idx = Number(indexAttr);
      if (!Number.isNaN(idx)) setActiveIndex(idx);
    }
  }, []);

  const tabIndexFor = useCallback(
    (index: number): 0 | -1 => (index === activeRowIndex ? 0 : -1),
    [activeRowIndex],
  );

  return { activeIndex: activeRowIndex, onKeyDown, onFocus, tabIndexFor };
}
