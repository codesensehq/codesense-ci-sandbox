// SPDX-License-Identifier: MIT
import { beforeEach, describe, it, expect, vi } from "vitest";
import { getSchema } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import type { Node as ProseMirrorNode } from "@tiptap/pm/model";
import {
  clearHeadingFlash,
  enumerateHeadings,
  findActiveHeadingIndex,
  headingSignature,
  scrollToHeadingBySlug,
  scrollToHeadingPos,
  slugify,
  type HeadingJumpEditor,
  type HeadingScrollEditor,
} from "./note-headings";
import { makeHeadingEl, makeScrollContainer } from "./note-headings.test-utils";

// A real ProseMirror schema from the pad's own extension (StarterKit) — the
// honest source of the `heading` node name + `attrs.level` the walk reads.
const schema = getSchema([StarterKit]);

/** Build a real PM doc from a compact spec so `enumerateHeadings` walks genuine
 *  heading nodes (not a hand-rolled fake). */
function docFrom(
  blocks: Array<{ type: "heading" | "paragraph"; level?: number; text: string }>,
): ProseMirrorNode {
  return schema.nodeFromJSON({
    type: "doc",
    content: blocks.map((b) =>
      b.type === "heading"
        ? {
            type: "heading",
            attrs: { level: b.level ?? 1 },
            content: b.text ? [{ type: "text", text: b.text }] : [],
          }
        : { type: "paragraph", content: b.text ? [{ type: "text", text: b.text }] : [] },
    ),
  });
}

describe("slugify", () => {
  it("lowercases, dashes non-alphanumerics, trims edges", () => {
    expect(slugify("Repro Steps")).toBe("repro-steps");
    expect(slugify("  What we ruled out!  ")).toBe("what-we-ruled-out");
    expect(slugify("TLS handshake (timing)")).toBe("tls-handshake-timing");
  });
});

describe("enumerateHeadings", () => {
  it("collects H1/H2/H3 in document order with correct level/text", () => {
    const headings = enumerateHeadings(
      docFrom([
        { type: "heading", level: 1, text: "Title" },
        { type: "paragraph", text: "body" },
        { type: "heading", level: 2, text: "Section" },
        { type: "heading", level: 3, text: "Sub" },
      ]),
    );
    expect(headings.map((h) => [h.level, h.text])).toEqual([
      [1, "Title"],
      [2, "Section"],
      [3, "Sub"],
    ]);
    // pos is the boundary before each node — strictly increasing, first at 0.
    expect(headings[0].pos).toBe(0);
    expect(headings[1].pos).toBeGreaterThan(headings[0].pos);
  });

  it("includes H4–H6 (StarterKit schema is not restricted to H1–H3)", () => {
    const headings = enumerateHeadings(
      docFrom([
        { type: "heading", level: 4, text: "Deep four" },
        { type: "heading", level: 5, text: "Deeper five" },
        { type: "heading", level: 6, text: "Deepest six" },
      ]),
    );
    expect(headings.map((h) => h.level)).toEqual([4, 5, 6]);
  });

  it("skips empty-text headings (an in-progress '## ' line)", () => {
    const headings = enumerateHeadings(
      docFrom([
        { type: "heading", level: 1, text: "Real" },
        { type: "heading", level: 2, text: "" },
      ]),
    );
    expect(headings.map((h) => h.text)).toEqual(["Real"]);
  });

  it("de-duplicates repeated heading text via an occurrence ordinal on the slug", () => {
    const headings = enumerateHeadings(
      docFrom([
        { type: "heading", level: 2, text: "Notes" },
        { type: "heading", level: 2, text: "Notes" },
        { type: "heading", level: 2, text: "Notes" },
      ]),
    );
    expect(headings.map((h) => h.slug)).toEqual(["notes", "notes-2", "notes-3"]);
  });
});

describe("headingSignature", () => {
  it("is stable across body-only edits and changes when a heading changes", () => {
    const a = enumerateHeadings(
      docFrom([
        { type: "heading", level: 1, text: "One" },
        { type: "paragraph", text: "first body" },
      ]),
    );
    const bodyEdited = enumerateHeadings(
      docFrom([
        { type: "heading", level: 1, text: "One" },
        { type: "paragraph", text: "different body text entirely" },
      ]),
    );
    const headingEdited = enumerateHeadings(
      docFrom([{ type: "heading", level: 1, text: "One renamed" }]),
    );
    expect(headingSignature(bodyEdited)).toBe(headingSignature(a));
    expect(headingSignature(headingEdited)).not.toBe(headingSignature(a));
  });
});

describe("scrollToHeadingPos", () => {
  /** A scroll editor: a container with a known viewport `top` + a settable
   *  `scrollTop`, and a heading element at a known `top`. `nodeDOM` returns the
   *  heading for any pos so we can assert the exact pos it was queried with. */
  function scrollEditor(containerTop: number, containerScrollTop: number, headingTop: number) {
    const container = makeScrollContainer(containerTop, containerScrollTop);
    const heading = makeHeadingEl(headingTop);
    const nodeDOM = vi.fn(() => heading);
    const editor: HeadingScrollEditor = { view: { dom: container, nodeDOM } };
    return { editor, container, heading, scrollIntoView: heading.scrollIntoView, nodeDOM };
  }

  it("scrolls ONLY the editor container to bring the heading to its top — never the ancestor-walking scrollIntoView", () => {
    // Heading sits 400px below the container top; container already scrolled 40.
    const { editor, container, scrollIntoView, nodeDOM } = scrollEditor(100, 40, 500);
    scrollToHeadingPos(editor, 12);
    expect(nodeDOM).toHaveBeenCalledWith(12); // exact pos, not pos+1
    // scrollTop advances by exactly headingTop - containerTop (500 - 100).
    expect(container.scrollTop).toBe(440);
    // The whole-window-displacement bug: scrollIntoView must NEVER be called.
    expect(scrollIntoView).not.toHaveBeenCalled();
  });

  it("is a safe no-op when the heading node has no laid-out element", () => {
    const container = makeScrollContainer(0, 25);
    const editor: HeadingScrollEditor = { view: { dom: container, nodeDOM: () => null } };
    expect(() => scrollToHeadingPos(editor, 12)).not.toThrow();
    expect(container.scrollTop).toBe(25); // unchanged
  });
});

describe("scrollToHeadingBySlug (SW-SN-6)", () => {
  // The flash tracker is module-level (shared across all jump surfaces) — reset
  // between cases so leftover state can't cross-contaminate assertions.
  beforeEach(() => clearHeadingFlash());

  /** A HeadingJumpEditor over a real doc; `nodeDOM` returns the element for each
   *  heading's pos so we can assert the scroll + flash target. The `dom` is the
   *  scroll container (top pinned at 0 by default) `scrollToHeadingPos` moves. */
  function jumpEditor(
    doc: ProseMirrorNode,
    nodeFor: (pos: number) => HTMLElement | null,
  ): HeadingJumpEditor {
    const container = makeScrollContainer(0);
    return {
      state: { doc },
      view: { dom: container, nodeDOM: (pos: number) => nodeFor(pos) },
    };
  }

  it("recomputes the target by slug from the live doc and scrolls its container to it", () => {
    const doc = docFrom([
      { type: "heading", level: 1, text: "Intro" },
      { type: "heading", level: 2, text: "TLS handshake timing" },
    ]);
    const target = enumerateHeadings(doc).find((h) => h.slug === "tls-handshake-timing")!;
    const el = makeHeadingEl(250, "h2");
    const editor = jumpEditor(doc, (pos) => (pos === target.pos ? el : null));

    const result = scrollToHeadingBySlug(editor, "tls-handshake-timing");

    expect(result?.slug).toBe("tls-handshake-timing");
    // Container scrolled to the heading (top 250, container top 0); no ancestor walk.
    expect((editor.view.dom as HTMLElement).scrollTop).toBe(250);
    expect(el.scrollIntoView).not.toHaveBeenCalled();
    // Landing flash lands on the target heading.
    expect(el.classList.contains("scratch-heading-flash")).toBe(true);
  });

  it("returns null and does not scroll when the slug no longer resolves", () => {
    const doc = docFrom([{ type: "heading", level: 1, text: "Only" }]);
    const editor = jumpEditor(doc, () => {
      throw new Error("nodeDOM should not be called for an unresolved slug");
    });
    expect(scrollToHeadingBySlug(editor, "deleted-heading")).toBeNull();
  });

  it("moves the flash off the previous heading on the next jump (one highlight at a time)", () => {
    const doc = docFrom([
      { type: "heading", level: 1, text: "One" },
      { type: "heading", level: 1, text: "Two" },
    ]);
    const [one, two] = enumerateHeadings(doc);
    const elOne = document.createElement("h1");
    const elTwo = document.createElement("h1");
    const editor = jumpEditor(doc, (pos) =>
      pos === one.pos ? elOne : pos === two.pos ? elTwo : null,
    );

    scrollToHeadingBySlug(editor, one.slug);
    expect(elOne.classList.contains("scratch-heading-flash")).toBe(true);
    scrollToHeadingBySlug(editor, two.slug);
    // The prior heading's persistent (reduced-motion) highlight is cleared —
    // including across surfaces, since the tracker is shared, not per-caller.
    expect(elOne.classList.contains("scratch-heading-flash")).toBe(false);
    expect(elTwo.classList.contains("scratch-heading-flash")).toBe(true);
  });

  it("clearHeadingFlash removes the current landing highlight (unmount cleanup)", () => {
    const doc = docFrom([{ type: "heading", level: 1, text: "One" }]);
    const [one] = enumerateHeadings(doc);
    const el = document.createElement("h1");
    const editor = jumpEditor(doc, (pos) => (pos === one.pos ? el : null));

    scrollToHeadingBySlug(editor, one.slug);
    expect(el.classList.contains("scratch-heading-flash")).toBe(true);
    clearHeadingFlash();
    expect(el.classList.contains("scratch-heading-flash")).toBe(false);
  });
});

describe("findActiveHeadingIndex (SW-SN-6 — shared scroll-spy query)", () => {
  /** A spy editor: a container at `containerTop` + heading DOM elements whose
   *  tops come from `tops` (keyed by pos). Missing pos → not-laid-out (null). */
  function spyEditor(doc: ProseMirrorNode, tops: Record<number, number>, containerTop = 100) {
    const container = document.createElement("div");
    container.getBoundingClientRect = () => ({ top: containerTop }) as DOMRect;
    const els = new Map<number, HTMLElement>();
    for (const h of enumerateHeadings(doc)) {
      if (!(h.pos in tops)) continue; // omit → nodeDOM returns null (not laid out)
      const el = document.createElement(`h${h.level}`);
      el.getBoundingClientRect = () => ({ top: tops[h.pos] }) as DOMRect;
      els.set(h.pos, el);
    }
    return {
      view: { dom: container, nodeDOM: (pos: number) => els.get(pos) ?? null },
    };
  }

  const doc = docFrom([
    { type: "heading", level: 1, text: "A" },
    { type: "heading", level: 2, text: "B" },
    { type: "heading", level: 2, text: "C" },
  ]);
  const [a, b, c] = enumerateHeadings(doc);

  it("returns the last heading at/above the viewport top (+offset)", () => {
    // A and B are above the threshold (100+4); C is below.
    const editor = spyEditor(doc, { [a.pos]: 50, [b.pos]: 90, [c.pos]: 400 });
    expect(findActiveHeadingIndex(editor, [a, b, c], 4)).toBe(1); // B
  });

  it("returns -1 when every heading is below the viewport top (scrolled above the first)", () => {
    const editor = spyEditor(doc, { [a.pos]: 300, [b.pos]: 400, [c.pos]: 500 });
    expect(findActiveHeadingIndex(editor, [a, b, c], 4)).toBe(-1);
  });

  it("skips headings that aren't laid out yet (no DOM) and keeps scanning", () => {
    // B has no DOM (omitted); A and C are above the top → C is the answer.
    const editor = spyEditor(doc, { [a.pos]: 20, [c.pos]: 30 });
    expect(findActiveHeadingIndex(editor, [a, b, c], 4)).toBe(2); // C
  });
});
