// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { render, screen, act, fireEvent } from "@testing-library/react";
import { getSchema } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import type { Editor } from "@tiptap/react";
import type { Node as ProseMirrorNode } from "@tiptap/pm/model";

import { StickyNoteHeaders } from "./sticky-note-headers";
import { enumerateHeadings } from "../lib/note-headings";
import { makeHeadingEl, makeScrollContainer } from "../lib/note-headings.test-utils";

const schema = getSchema([StarterKit]);

function docFrom(blocks: Array<{ level?: number; text: string; type?: "heading" | "paragraph" }>) {
  return schema.nodeFromJSON({
    type: "doc",
    content: blocks.map((b) =>
      (b.type ?? "heading") === "heading"
        ? {
            type: "heading",
            attrs: { level: b.level ?? 1 },
            content: [{ type: "text", text: b.text }],
          }
        : { type: "paragraph", content: [{ type: "text", text: b.text }] },
    ),
  }) as ProseMirrorNode;
}

/** A fake editor whose `nodeDOM(pos)` returns a stub element with a controllable
 *  viewport top, so we can simulate scroll position without real layout. */
function fakeEditor(doc: ProseMirrorNode, tops: Record<number, number>, containerTop = 100) {
  const container = makeScrollContainer(containerTop);
  const els = new Map<number, HTMLElement>();
  for (const h of enumerateHeadings(doc)) {
    els.set(h.pos, makeHeadingEl(tops[h.pos] ?? 9999, `h${h.level}`));
  }
  const editor = {
    view: { dom: container, nodeDOM: (pos: number) => els.get(pos) ?? null },
    state: { doc },
    on: vi.fn(),
    off: vi.fn(),
  } as unknown as Editor;
  return { editor, els, container };
}

describe("StickyNoteHeaders (SW-SN-6)", () => {
  it("renders nothing when the note has no headings", () => {
    const doc = docFrom([{ type: "paragraph", text: "just body" }]);
    const { editor } = fakeEditor(doc, {});
    render(<StickyNoteHeaders editor={editor} headings={[]} />);
    expect(screen.queryByTestId("sticky-note-headers")).toBeNull();
  });

  it("pins the current section + its ancestor chain (last heading above the viewport top)", () => {
    const doc = docFrom([
      { level: 1, text: "Chapter" },
      { level: 2, text: "Section" },
      { level: 3, text: "Below the fold" },
    ]);
    const [c, s, b] = enumerateHeadings(doc);
    // Chapter + Section scrolled above the viewport top (100+4); the H3 is below.
    const { editor } = fakeEditor(doc, { [c.pos]: 50, [s.pos]: 80, [b.pos]: 500 });
    act(() => {
      render(<StickyNoteHeaders editor={editor} headings={enumerateHeadings(doc)} />);
    });
    const stack = screen.getByTestId("sticky-note-headers");
    // Active section is "Section" (H2); its ancestor "Chapter" (H1) is pinned too.
    expect(stack).toHaveTextContent("Chapter");
    expect(stack).toHaveTextContent("Section");
    expect(stack).not.toHaveTextContent("Below the fold");
    // The overlay is aria-hidden (visual orientation only, not announced).
    expect(stack).toHaveAttribute("aria-hidden");
  });

  it("hides while scrolled above the first heading", () => {
    const doc = docFrom([
      { level: 1, text: "Top" },
      { level: 2, text: "Next" },
    ]);
    const [t, n] = enumerateHeadings(doc);
    // Every heading is below the viewport top → no current section.
    const { editor } = fakeEditor(doc, { [t.pos]: 400, [n.pos]: 500 });
    render(<StickyNoteHeaders editor={editor} headings={enumerateHeadings(doc)} />);
    expect(screen.queryByTestId("sticky-note-headers")).toBeNull();
  });

  it("clicking a pinned row scrolls ONLY the editor container to that heading", () => {
    const doc = docFrom([
      { level: 1, text: "Chapter" },
      { level: 2, text: "Section" },
    ]);
    const [c, s] = enumerateHeadings(doc);
    const { editor, els, container } = fakeEditor(doc, { [c.pos]: 50, [s.pos]: 80 });
    container.scrollTop = 500; // start scrolled so the delta stays positive
    act(() => {
      render(<StickyNoteHeaders editor={editor} headings={enumerateHeadings(doc)} />);
    });
    fireEvent.click(screen.getByText("Chapter"));
    // Container moved by (headingTop 50 - containerTop 100) = -50 → 450. The
    // ancestor-walking scrollIntoView that shoved the whole app shell is never used.
    expect(container.scrollTop).toBe(450);
    expect(els.get(c.pos)!.scrollIntoView).not.toHaveBeenCalled();
    // Landing flash lands on the clicked heading.
    expect(els.get(c.pos)!.classList.contains("scratch-heading-flash")).toBe(true);
  });

  it("re-seeds spy positions on a note switch with a stable editor + headings (noteKey)", () => {
    // Two notes with an IDENTICAL heading signature (same texts) but different
    // scroll/layout — the exact case where `headings` identity is stable and the
    // note-load fires no `update`. Without `noteKey` in the spy deps the effect
    // never re-runs and the overlay pins the previous note's section.
    const doc = docFrom([
      { level: 1, text: "Alpha" },
      { level: 2, text: "Section" },
      { level: 2, text: "Tail" },
    ]);
    const hs = enumerateHeadings(doc);
    const headings = hs; // stable reference across the "switch"
    let tops: Record<number, number> = { [hs[0].pos]: 50, [hs[1].pos]: 80, [hs[2].pos]: 500 };
    const container = document.createElement("div");
    container.getBoundingClientRect = () => ({ top: 100 }) as DOMRect;
    const els = new Map<number, HTMLElement>();
    for (const h of hs) {
      const el = document.createElement(`h${h.level}`);
      el.getBoundingClientRect = () => ({ top: tops[h.pos] ?? 9999 }) as DOMRect;
      els.set(h.pos, el);
    }
    const editor = {
      view: { dom: container, nodeDOM: (pos: number) => els.get(pos) ?? null },
      state: { doc },
      on: vi.fn(),
      off: vi.fn(),
    } as unknown as Editor;

    let view!: ReturnType<typeof render>;
    act(() => {
      view = render(<StickyNoteHeaders editor={editor} headings={headings} noteKey="a" />);
    });
    // Note A: Alpha + Section are above the fold → both pinned.
    expect(screen.getByTestId("sticky-note-headers")).toHaveTextContent("Section");

    // Switch to note B: only Alpha stays above the fold. Same editor + headings
    // refs; only noteKey changes.
    tops = { [hs[0].pos]: 50, [hs[1].pos]: 500, [hs[2].pos]: 600 };
    act(() => {
      view.rerender(<StickyNoteHeaders editor={editor} headings={headings} noteKey="b" />);
    });
    // Re-seeded → active is just Alpha; Section fell below the fold.
    expect(screen.getByTestId("sticky-note-headers")).not.toHaveTextContent("Section");
    expect(screen.getByTestId("sticky-note-headers")).toHaveTextContent("Alpha");
  });

  it("caps the pinned stack depth so it stays unobtrusive", () => {
    const doc = docFrom([
      { level: 1, text: "L1" },
      { level: 2, text: "L2" },
      { level: 3, text: "L3" },
      { level: 4, text: "L4" },
    ]);
    const hs = enumerateHeadings(doc);
    const tops: Record<number, number> = {};
    hs.forEach((h) => (tops[h.pos] = 50)); // all above the fold → deepest is active
    const { editor } = fakeEditor(doc, tops);
    act(() => {
      render(<StickyNoteHeaders editor={editor} headings={hs} />);
    });
    // `hidden: true` — the overlay is aria-hidden, so its rows are outside the
    // accessibility tree (by design); query them explicitly.
    const rows = screen.getAllByRole("button", { hidden: true });
    // MAX_STICKY_DEPTH = 3 → the deepest three of the L1>L2>L3>L4 chain.
    expect(rows).toHaveLength(3);
    expect(rows.map((r) => r.textContent)).toEqual(["L2", "L3", "L4"]);
  });
});
