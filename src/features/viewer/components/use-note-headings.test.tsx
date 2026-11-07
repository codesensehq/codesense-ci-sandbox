// SPDX-License-Identifier: MIT
import { describe, it, expect, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { getSchema } from "@tiptap/core";
import StarterKit from "@tiptap/starter-kit";
import type { Editor } from "@tiptap/react";
import { useNoteHeadings } from "./use-note-headings";

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

/** A minimal mutable editor stub: swap `state.doc`, capture the `update` handler. */
function makeEditor(initial: ReturnType<typeof doc>) {
  const handlers: Record<string, Array<() => void>> = {};
  return {
    state: { doc: initial },
    on: vi.fn((event: string, cb: () => void) => {
      (handlers[event] ??= []).push(cb);
    }),
    off: vi.fn((event: string, cb: () => void) => {
      handlers[event] = (handlers[event] ?? []).filter((h) => h !== cb);
    }),
    fireUpdate() {
      (handlers.update ?? []).forEach((h) => h());
    },
  };
}

describe("useNoteHeadings", () => {
  it("enumerates the initial doc's headings", () => {
    const ed = makeEditor(
      doc([
        [1, "Alpha"],
        [2, "Beta"],
      ]),
    );
    const { result } = renderHook(() => useNoteHeadings(ed as unknown as Editor, "note-1"));
    expect(result.current.map((h) => h.text)).toEqual(["Alpha", "Beta"]);
  });

  it("rebuilds when the editor fires 'update' (live typing)", () => {
    const ed = makeEditor(doc([[1, "Alpha"]]));
    const { result } = renderHook(() => useNoteHeadings(ed as unknown as Editor, "note-1"));
    expect(result.current.map((h) => h.text)).toEqual(["Alpha"]);

    ed.state.doc = doc([
      [1, "Alpha"],
      [2, "Added"],
    ]);
    act(() => ed.fireUpdate());
    expect(result.current.map((h) => h.text)).toEqual(["Alpha", "Added"]);
  });

  it("rebuilds on a note switch even though the load fires no 'update' (emitUpdate:false)", () => {
    const ed = makeEditor(doc([[1, "First note"]]));
    const { result, rerender } = renderHook(
      ({ key }) => useNoteHeadings(ed as unknown as Editor, key),
      { initialProps: { key: "note-1" } },
    );
    expect(result.current.map((h) => h.text)).toEqual(["First note"]);

    // Simulate a note switch: the panel's sync effect swaps the doc via
    // setContent(emitUpdate:false) — NO update event — and activeNoteId changes.
    ed.state.doc = doc([
      [1, "Second note"],
      [2, "Its section"],
    ]);
    rerender({ key: "note-2" });
    expect(result.current.map((h) => h.text)).toEqual(["Second note", "Its section"]);
  });

  it("does NOT produce a new array on a body-only edit (signature-gated)", () => {
    const ed = makeEditor(doc([[1, "Stable"]]));
    const { result } = renderHook(() => useNoteHeadings(ed as unknown as Editor, "note-1"));
    const first = result.current;

    // Same headings, different (imagined) body — signature unchanged.
    ed.state.doc = doc([[1, "Stable"]]);
    act(() => ed.fireUpdate());
    expect(result.current).toBe(first); // referential stability → no re-render churn
  });

  it("unsubscribes the update listener on unmount", () => {
    const ed = makeEditor(doc([[1, "Alpha"]]));
    const { unmount } = renderHook(() => useNoteHeadings(ed as unknown as Editor, "note-1"));
    expect(ed.on).toHaveBeenCalledWith("update", expect.any(Function));
    unmount();
    expect(ed.off).toHaveBeenCalledWith("update", expect.any(Function));
  });
});
