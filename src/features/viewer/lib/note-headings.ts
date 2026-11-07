// SPDX-License-Identifier: MIT
import type { Node as ProseMirrorNode } from "@tiptap/pm/model";

/** SW-SN-5: one heading, enumerated from the live ProseMirror doc.
 *  - `level` 1–6 (StarterKit's schema allows H1–H6, not just the H1–H3 that have
 *    keyboard shortcuts — pasted/typed markdown `####`+ produce real H4–H6 nodes).
 *  - `pos` is the PM position (the boundary BEFORE the heading node). It goes
 *    stale after any edit, so navigation RECOMPUTES it at click time; the value
 *    stored here is only a snapshot from the last enumeration.
 *  - `slug` is a stable-within-the-note key/id (slug of the text, de-duplicated by
 *    occurrence ordinal). SW-SN-6 reuses it as its picker-item id. */
export interface Heading {
  level: number;
  text: string;
  pos: number;
  slug: string;
}

/** Minimal editor shape the scroll helpers depend on — mirrors the repo's
 *  `TabHandlerEditor` pattern so tests don't couple to the full Editor class.
 *  The note's scroll port is the sheet around `view.dom` (`.nt-scroll`, which
 *  also holds the title), else `view.dom` itself; both `scrollToHeadingPos` and
 *  `findActiveHeadingIndex` measure against it. */
export interface HeadingScrollEditor {
  view: {
    dom: HTMLElement;
    nodeDOM(pos: number): Node | null;
  };
}

/** `scrollToHeadingBySlug` additionally reads the live doc to recompute the
 *  target's position by its stable slug (positions go stale after edits). */
export interface HeadingJumpEditor extends HeadingScrollEditor {
  state: { doc: ProseMirrorNode };
}

/** Slug of a heading's text — lowercase, non-alphanumerics collapsed to dashes.
 *  Keys need only be stable WITHIN a single note (not URL-global), so a tiny
 *  dependency-free slugify beats pulling in github-slugger. */
export function slugify(text: string): string {
  return text
    .toLowerCase()
    .trim()
    .replace(/[^\p{L}\p{N}]+/gu, "-")
    .replace(/^-+|-+$/g, "");
}

/** Walk the live ProseMirror doc and collect its headings in document order.
 *  Empty-text headings (an in-progress `## ` line) are skipped so they don't
 *  flicker a blank outline row. Duplicate heading texts get an occurrence
 *  ordinal appended to their slug so React keys stay unique. Pure (no editor,
 *  no DOM) — unit-tested against a real `getSchema()`-built doc. */
export function enumerateHeadings(doc: ProseMirrorNode): Heading[] {
  const out: Heading[] = [];
  const seen = new Map<string, number>();
  doc.descendants((node, pos) => {
    if (node.type.name !== "heading") return true;
    const text = node.textContent.trim();
    // Skip empty headings; don't descend into a heading's inline content.
    if (!text) return false;
    const base = slugify(text) || "heading";
    const n = (seen.get(base) ?? 0) + 1;
    seen.set(base, n);
    out.push({
      level: (node.attrs.level as number | undefined) ?? 1,
      text,
      pos,
      slug: n === 1 ? base : `${base}-${n}`,
    });
    return false;
  });
  return out;
}

/** A cheap change-detection signature over the heading list (level + text, in
 *  order). The reactive hook compares this so body-only edits — which leave the
 *  heading list unchanged — trigger no re-render (and no outline re-scroll). */
export function headingSignature(headings: Heading[]): string {
  return headings.map((h) => `${h.level}\u0000${h.text}`).join("\u0001");
}

/** The element the note scrolls in: the sheet (`.nt-scroll`) when the editor sits
 *  in one, else the editor's own DOM. */
export function noteScrollPort(editor: HeadingScrollEditor): HTMLElement {
  return editor.view.dom.closest<HTMLElement>(".nt-scroll") ?? editor.view.dom;
}

/** Scroll the editor so the heading at `pos` sits at the top of its viewport.
 *  Scrolls the note's OWN scroll port ({@link noteScrollPort})
 *  directly — advancing its `scrollTop` by the heading's offset from the port top,
 *  the same rect idiom `findActiveHeadingIndex` measures against. We deliberately
 *  do NOT use `Element.scrollIntoView`: per the CSSOM-view spec it walks every
 *  scrolling ancestor up to the viewport, which yanks the whole app shell (the
 *  root stays programmatically scrollable even though it's `overflow: hidden`).
 *  The write lands instantly (the UA default; the port also pins
 *  `scroll-behavior: auto` in `scratch-note.css` so a future global `smooth`
 *  can't animate it), so there's no goalpost for the scroll-spy suppress window
 *  to chase. Note: it's abandoning `scrollIntoView` — not the CSS — that stops
 *  the ancestor displacement; the pin only keeps the port's own scroll instant.
 *
 *  Off-by-one: `descendants` `pos` is the boundary BEFORE the node, which is
 *  exactly what `view.nodeDOM(pos)` wants (do NOT add 1). A heading with no
 *  laid-out DOM (never at click time — Tiptap doesn't virtualize) is a no-op;
 *  `scrollToHeadingBySlug` already recomputes the position live and null-guards. */
export function scrollToHeadingPos(
  editor: HeadingScrollEditor,
  pos: number,
  container: HTMLElement = noteScrollPort(editor),
): void {
  const dom = editor.view.nodeDOM(pos);
  if (!(dom instanceof HTMLElement)) return;
  container.scrollTop += dom.getBoundingClientRect().top - container.getBoundingClientRect().top;
}

/** The scroll-spy "current section" query, shared by the outline (SW-SN-5) and
 *  the sticky headers (SW-SN-6): the index of the LAST heading whose editor DOM
 *  top is at/above the viewport top (`container.top + offsetPx`), or `-1` when
 *  every heading is still below (scrolled above the first). Headings not yet laid
 *  out (no DOM) are skipped. Callers differ only in what they do with the index —
 *  the outline maps it to an active slug (first-heading fallback), the sticky
 *  overlay builds an ancestor chain (hidden at `-1`). Passive read; the caller
 *  owns the rAF throttle. */
export function findActiveHeadingIndex(
  editor: HeadingScrollEditor,
  positions: Heading[],
  offsetPx: number,
): number {
  const container = noteScrollPort(editor);
  const threshold = container.getBoundingClientRect().top + offsetPx;
  let idx = -1;
  for (let i = 0; i < positions.length; i++) {
    const dom = editor.view.nodeDOM(positions[i].pos);
    if (!(dom instanceof HTMLElement)) continue;
    if (dom.getBoundingClientRect().top <= threshold) idx = i;
    else break;
  }
  return idx;
}

// The editor heading currently carrying the landing-flash class. Module-level
// (not per-caller) so ALL jump surfaces — the outline, the go-to-heading modal
// (via the panel), and the sticky headers — coordinate through one tracker:
// there is only ever a single scratch editor, and only one landing flash should
// exist at a time. A per-surface ref could strand multiple reduced-motion
// PERSISTENT highlights (surface A can't clear surface B's flash).
let flashedHeading: HTMLElement | null = null;

/** SW-SN-6: scroll to the heading identified by its stable `slug`, recomputing
 *  its live position from the current doc (positions go stale after edits, so
 *  match by slug — R3), and apply the SW-SN-5 landing flash on it. Returns the
 *  resolved heading, or `null` if the slug no longer exists (deleted/renamed
 *  between selection and dispatch — the jump then no-ops rather than scrolling to
 *  a stale offset). Shared by the go-to-heading modal's dispatch (via the panel),
 *  the outline row, and the sticky-header overlay, so all three jump identically
 *  and share one landing highlight. */
export function scrollToHeadingBySlug(editor: HeadingJumpEditor, slug: string): Heading | null {
  const target = enumerateHeadings(editor.state.doc).find((h) => h.slug === slug);
  if (!target) return null;
  scrollToHeadingPos(editor, target.pos);
  // Move the flash off the previous target first — even if the new target has no
  // laid-out DOM — so a reduced-motion persistent highlight never strands.
  clearHeadingFlash();
  const dom = editor.view.nodeDOM(target.pos);
  if (dom instanceof HTMLElement) {
    void dom.offsetWidth; // reflow so a repeat jump to the same heading re-flashes
    dom.classList.add("scratch-heading-flash");
    flashedHeading = dom;
  }
  return target;
}

/** Remove the current landing flash (if any). Used by `scrollToHeadingBySlug` to
 *  move the highlight off the previous target before flashing the new one, so
 *  only ONE landing highlight ever exists — the reduced-motion PERSISTENT
 *  highlight therefore stays on the last jump target (AC-7 "must remain") until
 *  the next jump, without accumulating across surfaces. */
export function clearHeadingFlash(): void {
  if (flashedHeading) {
    flashedHeading.classList.remove("scratch-heading-flash");
    flashedHeading = null;
  }
}
