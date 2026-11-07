// SPDX-License-Identifier: MIT
import { memo, useCallback, useEffect, useRef, useState } from "react";
import type { Editor } from "@tiptap/react";
import {
  enumerateHeadings,
  findActiveHeadingIndex,
  noteScrollPort,
  scrollToHeadingBySlug,
  type Heading,
} from "../lib/note-headings";

export interface NoteOutlineProps {
  editor: Editor | null;
  /** The live heading list from `useNoteHeadings` (the panel owns the source). */
  headings: Heading[];
  /** True while the outline is on screen. Gates the scroll-spy listener. */
  active: boolean;
  /** The active note id. Only used to re-seed the scroll-spy position cache on a
   *  note switch: two notes can share a heading signature (so the `headings` prop
   *  keeps the same identity) yet have different `pos` offsets, and the note-load
   *  fires no `update`. Keying the spy effect on this refreshes positions then. */
  noteKey?: string | null;
  /** Optional notify on jump (SW-SN-6 may reuse the row for its own dispatch). */
  onJump?: (heading: Heading) => void;
}

// The active (scroll-spy) heading is "current" once its top crosses just below
// the editor viewport top — a small tolerance so a heading resting at the top
// counts as the section you're in.
const SPY_OFFSET_PX = 4;
const INDENT_BASE_PX = 8;
const INDENT_STEP_PX = 12;

/**
 * The note's outline ("On this note"): a popover from the chrome row when the pane
 * is narrow, a column beside the sheet when it is wide.
 * A flat `role="tree"` (indented by heading level; NO per-node folding — folding
 * is out of scope) with click-to-scroll, a scroll-spy `aria-current` active row,
 * and a landing flash. Reads the live ProseMirror doc via the shared
 * `note-headings` lib; the single mounted editor (SN-2 invariant) makes it live.
 */
export const NoteOutline = memo(function NoteOutline({
  editor,
  headings,
  active,
  noteKey,
  onJump,
}: NoteOutlineProps) {
  // The scroll-spy / just-clicked "you are here" heading (by slug).
  const [activeSlug, setActiveSlug] = useState<string | null>(null);
  // Roving-tabindex focus target (mirrors NotesRail's listbox roving).
  const [focusedSlug, setFocusedSlug] = useState<string | null>(null);
  const treeRef = useRef<HTMLDivElement>(null);
  // A short window after a click-jump during which scroll-spy is suppressed, so
  // the scroll event the jump itself fires can't immediately steal the highlight
  // off the clicked row (e.g. a heading near the doc end that can't reach the top
  // lands mid-viewport, where scroll-spy would otherwise pick an earlier heading).
  const suppressSpyUntilRef = useRef(0);

  // Exactly one row is a tab stop: the focused row if still present, else the
  // active row, else the first — so the tree is never a keyboard dead-end.
  const rovingSlug =
    (focusedSlug && headings.some((h) => h.slug === focusedSlug) && focusedSlug) ||
    (activeSlug && headings.some((h) => h.slug === activeSlug) && activeSlug) ||
    headings[0]?.slug ||
    null;

  const jump = useCallback(
    (heading: Heading) => {
      if (!editor) return;
      // The click is the user's explicit "this is my section" — hold it against
      // the scroll-spy for a beat (covers the jump's own scroll settling).
      suppressSpyUntilRef.current = Date.now() + 300;
      // Shared jump helper (SW-SN-6): recomputes the target by slug from the LIVE
      // doc (R7 — positions go stale after edits), scrolls (off-by-one-correct),
      // and runs the AC-7 landing flash lifecycle on the editor heading. Returns
      // null when the heading was removed/renamed between render and click — no-op
      // rather than scrolling to a stale pos (the row disappears on next rebuild).
      const target = scrollToHeadingBySlug(editor, heading.slug);
      if (!target) return;
      setActiveSlug(target.slug);
      onJump?.(target);
    },
    [editor, onJump],
  );

  const moveFocus = useCallback(
    (delta: number) => {
      if (headings.length === 0) return;
      const i = headings.findIndex((h) => h.slug === rovingSlug);
      const next = headings[Math.min(headings.length - 1, Math.max(0, (i < 0 ? 0 : i) + delta))];
      setFocusedSlug(next.slug);
      // SW-FX-97: `preventScroll` — a bare `.focus()` lets the browser scroll every
      // scrollable ancestor (up to the programmatically-scrollable app root) to
      // reveal the row, which yanks the whole shell. We only move the roving-tabindex
      // focus here; the note scroll is owned by `jump`. Mirrors the sibling
      // `notes-rail.tsx` row focus, which already guards this.
      treeRef.current
        ?.querySelector<HTMLElement>(`[data-heading-slug="${next.slug}"]`)
        ?.focus({ preventScroll: true });
    },
    [headings, rovingSlug],
  );

  // Scroll-spy: the active row is the LAST heading whose editor DOM top is at or
  // above the viewport top. Passive + rAF-throttled. Positions are enumerated
  // from the LIVE doc and CACHED (the `headings` prop is signature-gated, so its
  // `pos` values go stale after body-only edits); the cache is refreshed only on
  // `update` — a scroll frame just re-measures rects, it does not re-walk the doc.
  // A resize also moves heading tops without a scroll, so observe it too.
  useEffect(() => {
    if (!editor || !active) return;
    const container = noteScrollPort(editor);
    let raf = 0;
    let positions = enumerateHeadings(editor.state.doc);
    const compute = () => {
      raf = 0;
      // Shared spy query (SW-SN-6): the last heading at/above the viewport top.
      // The outline keeps the FIRST heading active even when scrolled above it
      // (idx -1 → positions[0]), unlike the sticky overlay which hides there.
      const idx = findActiveHeadingIndex(editor, positions, SPY_OFFSET_PX);
      const current: string | null = positions[idx]?.slug ?? positions[0]?.slug ?? null;
      // Only adopt a slug that a rendered row actually has (the live doc can be a
      // frame ahead of the signature-gated `headings` prop); else keep the prior.
      setActiveSlug((prev) =>
        current !== null && !headings.some((h) => h.slug === current) ? prev : current,
      );
    };
    const schedule = () => {
      if (Date.now() < suppressSpyUntilRef.current) return; // hold a just-clicked row
      if (!raf) raf = requestAnimationFrame(compute);
    };
    const onDocUpdate = () => {
      positions = enumerateHeadings(editor.state.doc);
      schedule();
    };
    compute();
    container.addEventListener("scroll", schedule, { passive: true });
    editor.on("update", onDocUpdate);
    const ro = typeof ResizeObserver !== "undefined" ? new ResizeObserver(schedule) : null;
    ro?.observe(container);
    return () => {
      container.removeEventListener("scroll", schedule);
      editor.off("update", onDocUpdate);
      ro?.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
    // `noteKey` re-seeds `positions` on a note switch even when the heading
    // signature is unchanged (identical structure, different body/offsets).
  }, [editor, active, headings, noteKey]);

  if (headings.length === 0) {
    return <div className="nt-toc-empty">No headings yet.</div>;
  }

  // Indent from the note's shallowest heading, so its top level lines up with the label.
  const topLevel = Math.min(...headings.map((h) => h.level));

  return (
    <div
      ref={treeRef}
      role="tree"
      aria-label="Outline"
      onKeyDown={(e) => {
        if (e.key === "ArrowDown") {
          e.preventDefault();
          moveFocus(1);
        } else if (e.key === "ArrowUp") {
          e.preventDefault();
          moveFocus(-1);
        }
      }}
    >
      {headings.map((h) => {
        const isActive = h.slug === activeSlug;
        return (
          <div
            key={h.slug}
            role="treeitem"
            aria-level={h.level}
            aria-current={isActive ? "true" : undefined}
            data-heading-slug={h.slug}
            tabIndex={h.slug === rovingSlug ? 0 : -1}
            onClick={() => jump(h)}
            onFocus={() => setFocusedSlug(h.slug)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                jump(h);
              }
            }}
            className="nt-toc-row"
            style={{ paddingLeft: INDENT_BASE_PX + (h.level - topLevel) * INDENT_STEP_PX }}
          >
            {h.text}
          </div>
        );
      })}
    </div>
  );
});
