// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import type { NoteMeta } from "@/tauri/commands";
import { buildScratchNotePickerItems } from "./scratch-note-picker-items";

function note(over: Partial<NoteMeta> & { id: string }): NoteMeta {
  return { title: over.id, pinned: false, lastOpenedAt: 0, ...over };
}

describe("buildScratchNotePickerItems (SW-SN-3)", () => {
  it("orders pinned-first then recency (concrete: [pinned, newer, older])", () => {
    const notes = [
      note({ id: "a", pinned: false, lastOpenedAt: 100 }),
      note({ id: "b", pinned: true, title: "Bee" }),
      note({ id: "c", pinned: false, lastOpenedAt: 300 }),
    ];
    // pinned "b" first, then unpinned by lastOpenedAt desc: c (300) before a (100).
    expect(buildScratchNotePickerItems(notes).map((i) => i.id)).toEqual(["b", "c", "a"]);
  });

  it("falls back to 'Untitled' for a blank/whitespace title (stays searchable)", () => {
    const items = buildScratchNotePickerItems([
      note({ id: "blank", title: "   " }),
      note({ id: "empty", title: "" }),
    ]);
    expect(items.map((i) => i.label)).toEqual(["Untitled", "Untitled"]);
  });

  it("floats a pinned note above a more-recently-opened unpinned note (empty-state AC-2)", () => {
    const notes = [
      note({ id: "recent", pinned: false, lastOpenedAt: 999 }),
      note({ id: "pinned", pinned: true, title: "Pinned" }),
    ];
    expect(buildScratchNotePickerItems(notes).map((i) => i.id)).toEqual(["pinned", "recent"]);
  });

  it("carries the title in `label` (the fuzzy-search target) and the note id in `id`", () => {
    const items = buildScratchNotePickerItems([note({ id: "n1", title: "Migration questions" })]);
    expect(items[0]).toEqual({ id: "n1", label: "Migration questions", sublabel: "" });
  });

  it("does NOT duplicate the title into `sublabel` (dialog searches max(label, sublabel))", () => {
    const items = buildScratchNotePickerItems([note({ id: "n1", title: "Prompts" })]);
    expect(items[0].sublabel).toBe("");
  });

  it("uses the note id verbatim as the item id (no encode/decode wrapper)", () => {
    const items = buildScratchNotePickerItems([note({ id: "abc-123-def", title: "x" })]);
    expect(items[0].id).toBe("abc-123-def");
  });

  it("returns an empty list for no notes", () => {
    expect(buildScratchNotePickerItems([])).toEqual([]);
  });
});
