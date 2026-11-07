// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, within } from "@testing-library/react";
import { getSchema } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import type { Editor } from "@tiptap/react";
import { NoteOutline } from "./note-outline";
import { enumerateHeadings } from "../lib/note-headings";
import { makeHeadingEl, makeScrollContainer } from "../lib/note-headings.test-utils";

const schema = getSchema([StarterKit]);

function doc(headings: Array<[number, string]>) {
  return schema.nodeFromJSON({
    type: "doc",
    content: headings.map(([level, text]) => ({
      type: "heading",
      attrs: { level },
      content: [{ type: "text", text }],
    })),
  });
}

/** Mock editor: a real doc (so `enumerateHeadings` runs), a `view.nodeDOM` that
 *  hands back a stable <h*> element per position (with a LIVE viewport top read
 *  from `tops` so scroll-spy tests can move headings after render), a `view.dom`
 *  scroll host, and `on`/`off` for the "update" event the scroll-spy subscribes to. */
function makeEditor(pmDoc: ReturnType<typeof doc>) {
  const byPos = new Map<number, HTMLElement>();
  const tops = new Map<number, number>(); // pos → viewport top (default 0)
  const handlers: Record<string, Array<() => void>> = {};
  const nodeDOM = vi.fn((pos: number) => {
    let el = byPos.get(pos);
    if (!el) {
      el = makeHeadingEl(() => tops.get(pos) ?? 0);
      byPos.set(pos, el);
    }
    return el;
  });
  const dom = makeScrollContainer(0);
  return {
    state: { doc: pmDoc },
    view: { nodeDOM, dom },
    on: vi.fn((event: string, cb: () => void) => {
      (handlers[event] ??= []).push(cb);
    }),
    off: vi.fn(),
    _nodeDOM: nodeDOM,
    _byPos: byPos,
    _tops: tops,
    _dom: dom,
  };
}

const DOC = doc([
  [1, "Title"],
  [2, "Repro steps"],
  [3, "Sub detail"],
  [4, "Deep four"],
]);
const HEADINGS = enumerateHeadings(DOC);

let editor: ReturnType<typeof makeEditor>;
beforeEach(() => {
  editor = makeEditor(DOC);
});

describe("NoteOutline", () => {
  it("renders a role=tree with a treeitem per heading, aria-level = heading level", () => {
    render(<NoteOutline editor={editor as unknown as Editor} headings={HEADINGS} active />);
    const tree = screen.getByRole("tree", { name: "Outline" });
    const items = within(tree).getAllByRole("treeitem");
    expect(items).toHaveLength(4);
    // H4 is included (schema is not restricted to H1–H3) with its true level.
    expect(items.map((el) => el.getAttribute("aria-level"))).toEqual(["1", "2", "3", "4"]);
  });

  it("shows the quiet empty state when there are no headings", () => {
    render(<NoteOutline editor={editor as unknown as Editor} headings={[]} active />);
    expect(screen.getByText("No headings yet.")).toBeInTheDocument();
    expect(screen.queryByRole("tree")).not.toBeInTheDocument();
  });

  it("click scrolls ONLY the editor container to that heading (by recomputed PM pos) and marks it aria-current", () => {
    render(<NoteOutline editor={editor as unknown as Editor} headings={HEADINGS} active />);
    // The H2 sits 300px below the container top; the jump should land it at the top.
    const h2 = HEADINGS[1];
    editor._tops.set(h2.pos, 300);
    const row = screen.getByText("Repro steps");
    fireEvent.click(row);

    // Jumped to the H2's live pos via nodeDOM (scroll-by-position).
    expect(editor._nodeDOM).toHaveBeenCalledWith(h2.pos);
    // Only the note's own scroll port moved (300 - 0); the ancestor-walking
    // scrollIntoView — which shoved the whole app shell (SW-SN-15) — is never used.
    expect(editor._dom.scrollTop).toBe(300);
    expect(editor._byPos.get(h2.pos)!.scrollIntoView).not.toHaveBeenCalled();
    // The clicked row is the active (you-are-here) row.
    const treeitem = row.closest('[role="treeitem"]')!;
    expect(treeitem).toHaveAttribute("aria-current", "true");
  });

  it("applies the landing-flash class to the editor heading on jump (AC-7)", () => {
    render(<NoteOutline editor={editor as unknown as Editor} headings={HEADINGS} active />);
    fireEvent.click(screen.getByText("Title"));
    const h1 = editor._byPos.get(HEADINGS[0].pos)!;
    expect(h1.classList.contains("scratch-heading-flash")).toBe(true);
  });

  it("Enter on a focused row jumps (keyboard-activatable)", () => {
    render(<NoteOutline editor={editor as unknown as Editor} headings={HEADINGS} active />);
    const row = screen.getByText("Sub detail").closest('[role="treeitem"]')!;
    fireEvent.keyDown(row, { key: "Enter" });
    expect(editor._nodeDOM).toHaveBeenCalledWith(HEADINGS[2].pos);
  });

  it("SW-FX-97: ArrowDown focuses the next row with { preventScroll: true } (never scroll-yanks the app shell)", () => {
    render(<NoteOutline editor={editor as unknown as Editor} headings={HEADINGS} active />);
    const tree = screen.getByRole("tree", { name: "Outline" });
    const focusSpy = vi.spyOn(HTMLElement.prototype, "focus");
    // Roving focus starts on the first row; ArrowDown moves it to the next.
    fireEvent.keyDown(tree, { key: "ArrowDown" });
    // Focus must pass preventScroll so the browser doesn't scroll ancestors (up to
    // the programmatically-scrollable app root) to reveal the row — a bare focus()
    // there is the SW-SN-15-class shell displacement this closes. The moved-to row
    // is the ONLY focus() the nav performs.
    expect(focusSpy).toHaveBeenCalledTimes(1);
    expect(focusSpy).toHaveBeenCalledWith({ preventScroll: true });
    // The focused node is an outline row (the nav's only focus() call).
    expect(focusSpy.mock.instances[0] as HTMLElement).toHaveAttribute("data-heading-slug");
    focusSpy.mockRestore();
  });

  it("marks the current section with aria-current (styled by the outline's CSS), never color alone", () => {
    render(<NoteOutline editor={editor as unknown as Editor} headings={HEADINGS} active />);
    const active = screen.getByText("Title").closest('[role="treeitem"]') as HTMLElement;
    const resting = screen.getByText("Deep four").closest('[role="treeitem"]') as HTMLElement;
    fireEvent.click(screen.getByText("Title"));
    expect(active).toHaveAttribute("aria-current", "true");
    expect(resting).not.toHaveAttribute("aria-current");
    // Both rows share the outline row class; the current one is set apart by
    // weight and wash in `.nt-toc-row[aria-current="true"]`.
    expect(active).toHaveClass("nt-toc-row");
    expect(resting).toHaveClass("nt-toc-row");
  });

  it("scroll-spy marks the last heading above the viewport top as aria-current, and follows scroll (AC-7)", () => {
    // Run scheduled frames synchronously so the scroll handler resolves in-test.
    const raf = vi
      .spyOn(window, "requestAnimationFrame")
      .mockImplementation((cb: FrameRequestCallback) => {
        cb(0);
        return 0;
      });
    // Container top = 0. "Title" and "Repro steps" sit above the viewport top;
    // "Repro steps" is the last one above → it is the active section.
    editor._tops.set(HEADINGS[0].pos, -100);
    editor._tops.set(HEADINGS[1].pos, -10);
    editor._tops.set(HEADINGS[2].pos, 50);
    editor._tops.set(HEADINGS[3].pos, 200);
    render(<NoteOutline editor={editor as unknown as Editor} headings={HEADINGS} active />);
    fireEvent.scroll(editor._dom);
    expect(screen.getByText("Repro steps").closest('[role="treeitem"]')).toHaveAttribute(
      "aria-current",
      "true",
    );

    // Scroll on: "Sub detail" crosses the top → the active row follows it.
    editor._tops.set(HEADINGS[2].pos, -5);
    fireEvent.scroll(editor._dom);
    expect(screen.getByText("Sub detail").closest('[role="treeitem"]')).toHaveAttribute(
      "aria-current",
      "true",
    );
    expect(screen.getByText("Repro steps").closest('[role="treeitem"]')).not.toHaveAttribute(
      "aria-current",
    );
    raf.mockRestore();
  });
});
