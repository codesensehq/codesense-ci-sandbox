// SPDX-License-Identifier: MIT
import { memo, useEffect, useState } from "react";
import type { Editor } from "@tiptap/react";
import {
  enumerateHeadings,
  findActiveHeadingIndex,
  noteScrollPort,
  scrollToHeadingBySlug,
  type Heading,
} from "../lib/note-headings";

export interface StickyNoteHeadersProps {
  editor: Editor | null;
  /** The live heading list from `useNoteHeadings` (the panel owns the source —
   *  never re-enumerated for the render; the spy re-walks positions internally). */
  headings: Heading[];
  /** The active note id. Re-seeds the cached spy positions on a note switch even
   *  when two notes share a heading signature (so `headings` keeps the same
   *  identity) but differ in body offsets — the note-load fires no `update`, so
   *  without this the spy would read the previous note's stale `pos` values.
   *  (Same guard SN-5's `NoteOutline` uses.) */
  noteKey?: string | null;
}

// A heading is the "current section" once its top crosses just below the editor
// viewport top (small tolerance so a heading resting at the top counts). Mirrors
// the outline scroll-spy's SPY_OFFSET_PX.
const SPY_OFFSET_PX = 4;
// Cap the pinned stack so it orients without obstructing (AC-5). Keep the
// DEEPEST levels (closest to where the reader is) when the nesting is deeper.
const MAX_STICKY_DEPTH = 3;
const INDENT_BASE_PX = 18;
const INDENT_STEP_PX = 8;

/** The active heading plus its ancestor chain (each strictly-shallower level),
 *  trimmed to the deepest `MAX_STICKY_DEPTH` — the VS Code sticky-scroll stack. */
function chainForActive(positions: Heading[], activeIdx: number): Heading[] {
  const active = positions[activeIdx];
  const stack: Heading[] = [active];
  let level = active.level;
  for (let i = activeIdx - 1; i >= 0; i--) {
    if (positions[i].level < level) {
      stack.unshift(positions[i]);
      level = positions[i].level;
    }
  }
  return stack.slice(-MAX_STICKY_DEPTH);
}

function sameSlugs(a: Heading[], b: Heading[]): boolean {
  return a.length === b.length && a.every((h, i) => h.slug === b[i].slug);
}

/**
 * SW-SN-6: sticky-scroll headers for the scratch editor. As the user scrolls, the
 * current section's heading (and its ancestor chain) pins to the top of the
 * editor viewport for continuous orientation — the VS Code "Sticky Scroll"
 * pattern. Net-new (no UX spec beyond AC-3/AC-5): built cheap + unobtrusive.
 *
 * A `pointer-events`-scoped overlay (never CSS `position:sticky` on the heading
 * nodes — that fights `contenteditable` selection and stacks heavily), driven by
 * the same scroll-spy math as SN-5's outline: the active heading is the last one
 * whose editor DOM top is ≤ the viewport top. Passive + rAF-throttled; heading
 * positions are cached off the `update` signal, not re-walked per scroll frame.
 * `aria-hidden` — the real headings live in the editor; announcing the sticky
 * duplicate would spam a screen reader (visual orientation only).
 */
export const StickyNoteHeaders = memo(function StickyNoteHeaders({
  editor,
  headings,
  noteKey,
}: StickyNoteHeadersProps) {
  const [chain, setChain] = useState<Heading[]>([]);

  useEffect(() => {
    // Empty/no-editor is handled by the render guard below (return null) — no
    // setState here, so the effect never fights React's set-state-in-effect rule.
    if (!editor || headings.length === 0) return;
    const container = noteScrollPort(editor);
    let raf = 0;
    // Cached heading positions — refreshed on `update`, not per scroll frame.
    let positions = enumerateHeadings(editor.state.doc);
    const compute = () => {
      raf = 0;
      // Shared spy query (SW-SN-6): the last heading at/above the viewport top, or
      // -1 when scrolled above the first — the sticky overlay HIDES there (unlike
      // the outline, which keeps the first heading active).
      const activeIdx = findActiveHeadingIndex(editor, positions, SPY_OFFSET_PX);
      const next = activeIdx < 0 ? [] : chainForActive(positions, activeIdx);
      // Only adopt slugs the signature-gated `headings` prop actually has (the
      // live doc can be a frame ahead of the prop) — else keep it empty.
      const adopt = next.every((h) => headings.some((x) => x.slug === h.slug)) ? next : [];
      setChain((prev) => (sameSlugs(prev, adopt) ? prev : adopt));
    };
    const schedule = () => {
      if (!raf) raf = requestAnimationFrame(compute);
    };
    const onDocUpdate = () => {
      positions = enumerateHeadings(editor.state.doc);
      schedule();
    };
    compute();
    container.addEventListener("scroll", schedule, { passive: true });
    editor.on("update", onDocUpdate);
    // A resize moves heading tops without a scroll (e.g. pane width change).
    const ro = typeof ResizeObserver !== "undefined" ? new ResizeObserver(schedule) : null;
    ro?.observe(container);
    return () => {
      container.removeEventListener("scroll", schedule);
      editor.off("update", onDocUpdate);
      ro?.disconnect();
      if (raf) cancelAnimationFrame(raf);
    };
    // `noteKey` re-seeds `positions` on a note switch even when the heading
    // signature (and thus `headings` identity) is unchanged — two notes can share
    // structure but differ in body offsets, and the note-load fires no `update`.
  }, [editor, headings, noteKey]);

  // Render null when there's no current section, no headings, or no editor. (The
  // empty-headings case leaves any prior `chain` in state untouched — harmless,
  // since we render nothing; the effect re-seeds it when headings return.)
  if (!editor || headings.length === 0 || chain.length === 0) return null;

  return (
    // aria-hidden: the editor's own headings are the real content; the sticky
    // stack is a redundant VISUAL orientation cue. Rows are tabIndex -1 (not a
    // keyboard trap in the hidden subtree) — keyboard nav uses ⌘⇧O / the outline.
    <div aria-hidden data-testid="sticky-note-headers" className="sticky-note-headers nt-sticky">
      {chain.map((hd, i) => {
        const isCurrent = i === chain.length - 1;
        return (
          <button
            key={hd.slug}
            type="button"
            tabIndex={-1}
            title={hd.text}
            data-current={isCurrent}
            onClick={() => editor && scrollToHeadingBySlug(editor, hd.slug)}
            className="nt-sticky-row"
            style={{ paddingLeft: INDENT_BASE_PX + (hd.level - 1) * INDENT_STEP_PX }}
          >
            <span className="truncate">{hd.text}</span>
          </button>
        );
      })}
    </div>
  );
});
