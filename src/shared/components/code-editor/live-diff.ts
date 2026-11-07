// SPDX-License-Identifier: MIT
import type { Extension } from "@codemirror/state";
import type { DecorationSet, EditorView } from "@codemirror/view";

import type { CmModule } from "./cm-loader";

/**
 * SLW-DV-5 — the live-follow primitives shared by the review merge view: the
 * minimal edit that turns one document into another (so a live update patches in
 * place instead of remounting), a transient flash decoration for the just-changed
 * lines, and a scroll helper that drives the WD-7 OUTER scroller (the split merge
 * view disables its inner scrollers, so `EditorView.scrollIntoView` can't move it).
 */

/** The single replaced span `minimalChange` produces — module-internal shape. */
interface DocChange {
  from: number;
  to: number;
  insert: string;
}

/**
 * The single replaced span that transforms `current` into `next` — a common
 * prefix/suffix trim. Returns `null` when the strings are identical (no dispatch
 * needed). Not a full multi-hunk diff: one tight range is enough to (a) keep the
 * CodeMirror update minimal for the typical append/edit an agent makes and (b)
 * hand the flash exactly the inserted range. The inserted span in the NEXT doc is
 * `[from, from + insert.length)`.
 */
export function minimalChange(current: string, next: string): DocChange | null {
  if (current === next) return null;
  const max = Math.min(current.length, next.length);
  let start = 0;
  while (start < max && current.charCodeAt(start) === next.charCodeAt(start)) start++;
  let endCurrent = current.length;
  let endNext = next.length;
  while (
    endCurrent > start &&
    endNext > start &&
    current.charCodeAt(endCurrent - 1) === next.charCodeAt(endNext - 1)
  ) {
    endCurrent--;
    endNext--;
  }
  return { from: start, to: endCurrent, insert: next.slice(start, endNext) };
}

export interface FlashController {
  /** Add to the after-side editor's extensions at construction. */
  extension: Extension;
  /** Pulse every line overlapping `[from, to)` in `view`, auto-clearing after `durationMs`. */
  flash: (view: EditorView, from: number, to: number, durationMs: number) => void;
  /** Cancel any pending clear timers — call on view teardown so a destroyed
   *  view is not pinned in a timer closure until its fade would have ended. */
  dispose: () => void;
}

/**
 * A self-clearing flash-on-change decoration (AC-3): a StateField holds a
 * DecorationSet of line highlights, added by an effect over the changed range and
 * dropped by a timer-fired clear effect so highlights never accumulate. The fade
 * itself is a CSS animation (green → transparent) carried by a `baseTheme`, so no
 * external stylesheet is needed.
 */
export function createFlashController(cm: CmModule): FlashController {
  const addFlash = cm.StateEffect.define<{ from: number; to: number }>();
  const clearFlash = cm.StateEffect.define<null>();
  const flashLine = cm.Decoration.line({ attributes: { class: "cm-live-flash-line" } });

  const field = cm.StateField.define<DecorationSet>({
    create: () => cm.Decoration.none,
    update(deco, tr) {
      deco = deco.map(tr.changes);
      for (const effect of tr.effects) {
        if (effect.is(addFlash)) {
          const docLen = tr.state.doc.length;
          const first = tr.state.doc.lineAt(Math.min(effect.value.from, docLen));
          const last = tr.state.doc.lineAt(Math.min(effect.value.to, docLen));
          const marks = [];
          for (let n = first.number; n <= last.number; n++) {
            marks.push(flashLine.range(tr.state.doc.line(n).from));
          }
          if (marks.length) deco = deco.update({ add: marks, sort: true });
        } else if (effect.is(clearFlash)) {
          deco = cm.Decoration.none;
        }
      }
      return deco;
    },
    provide: (f) => cm.EditorView.decorations.from(f),
  });

  const theme = cm.EditorView.baseTheme({
    ".cm-live-flash-line": {
      animation: "cm-live-flash-fade 700ms ease-out",
    },
    "@keyframes cm-live-flash-fade": {
      from: { backgroundColor: "var(--cm-live-flash, rgba(34, 197, 94, 0.35))" },
      to: { backgroundColor: "transparent" },
    },
  });

  const pending = new Set<number>();
  const flash = (view: EditorView, from: number, to: number, durationMs: number): void => {
    view.dispatch({ effects: addFlash.of({ from, to }) });
    const handle = window.setTimeout(() => {
      pending.delete(handle);
      // The view may have been destroyed (file switch) before the timer fires.
      try {
        view.dispatch({ effects: clearFlash.of(null) });
      } catch {
        /* view destroyed — nothing to clear */
      }
    }, durationMs);
    pending.add(handle);
  };

  const dispose = (): void => {
    for (const handle of pending) window.clearTimeout(handle);
    pending.clear();
  };

  return { extension: [field, theme], flash, dispose };
}

/**
 * The nearest scrollable ancestor of `el` (the WD-7 `overflowY:auto` merge-view
 * frame). Returns `null` if none is found before the document root.
 */
export function findScrollParent(el: HTMLElement | null): HTMLElement | null {
  let node = el?.parentElement ?? null;
  while (node) {
    const overflowY = getComputedStyle(node).overflowY;
    if (overflowY === "auto" || overflowY === "scroll") return node;
    node = node.parentElement;
  }
  return null;
}

/** The top of `pos`'s line in the scroller's own scroll coordinate system. */
function lineTopInScroller(view: EditorView, scroller: HTMLElement, pos: number): number {
  const block = view.lineBlockAt(Math.max(0, Math.min(pos, view.state.doc.length)));
  const contentTop =
    view.contentDOM.getBoundingClientRect().top -
    scroller.getBoundingClientRect().top +
    scroller.scrollTop;
  return contentTop + block.top;
}

/**
 * Scroll `scroller` so document position `pos` is vertically centered — used by
 * follow (`tail -f`) and the "jump to latest" pill. Uses DOM coordinates rather
 * than `EditorView.scrollIntoView` because a split MergeView defers its vertical
 * scroll to this outer element (WD-7), so the editor's own scroll effect is inert.
 * Best-effort: any measurement gap is a no-op.
 */
export function scrollPosToCenter(view: EditorView, scroller: HTMLElement, pos: number): void {
  const block = view.lineBlockAt(Math.max(0, Math.min(pos, view.state.doc.length)));
  const target =
    lineTopInScroller(view, scroller, pos) - scroller.clientHeight / 2 + block.height / 2;
  scroller.scrollTop = Math.max(0, target);
}

/** The document position + exact sub-line offset of the line at the viewport top,
 *  captured BEFORE a live patch so `restoreTopAnchor` can hold it pixel-stable
 *  across an above-the-fold edit (AC-2). `null` if the top can't be measured. */
export function captureTopAnchor(
  view: EditorView,
  scroller: HTMLElement,
): { pos: number; viewportY: number } | null {
  const rect = scroller.getBoundingClientRect();
  const pos = view.posAtCoords({ x: rect.left + 4, y: rect.top + 4 });
  if (pos == null) return null;
  // How far the anchored line's top sits relative to the viewport top (usually a
  // small negative — the line straddles the fold). Preserving this exact offset
  // is what makes the restore pixel-stable rather than snapping to a line edge.
  return { pos, viewportY: lineTopInScroller(view, scroller, pos) - scroller.scrollTop };
}

/** Restore the captured anchor so its line holds the same viewport offset. */
export function restoreTopAnchor(
  view: EditorView,
  scroller: HTMLElement,
  pos: number,
  viewportY: number,
): void {
  scroller.scrollTop = Math.max(0, lineTopInScroller(view, scroller, pos) - viewportY);
}
