// SPDX-License-Identifier: MIT
import { describe, it, expect } from "vitest";
import type { NoteMeta } from "@/tauri/commands";
import { partitionNotes, filterByTitle } from "./note-ordering";

function note(over: Partial<NoteMeta> & { id: string }): NoteMeta {
  return { title: over.id, pinned: false, lastOpenedAt: 0, ...over };
}

describe("partitionNotes (SW-SN-2)", () => {
  it("splits pinned from unpinned", () => {
    const notes = [note({ id: "a", pinned: false }), note({ id: "b", pinned: true })];
    const { pinned, recent } = partitionNotes(notes);
    expect(pinned.map((n) => n.id)).toEqual(["b"]);
    expect(recent.map((n) => n.id)).toEqual(["a"]);
  });

  it("orders RECENT by lastOpenedAt desc", () => {
    const notes = [
      note({ id: "old", lastOpenedAt: 100 }),
      note({ id: "new", lastOpenedAt: 300 }),
      note({ id: "mid", lastOpenedAt: 200 }),
    ];
    const { recent } = partitionNotes(notes);
    expect(recent.map((n) => n.id)).toEqual(["new", "mid", "old"]);
  });

  it("orders PINNED by title (v1 fallback — no manual pin order)", () => {
    const notes = [
      note({ id: "z", title: "Zebra", pinned: true, lastOpenedAt: 999 }),
      note({ id: "a", title: "Apple", pinned: true, lastOpenedAt: 1 }),
    ];
    const { pinned } = partitionNotes(notes);
    expect(pinned.map((n) => n.title)).toEqual(["Apple", "Zebra"]);
  });

  it("pinned title sort is case-insensitive", () => {
    const notes = [
      note({ id: "b", title: "beta", pinned: true }),
      note({ id: "A", title: "Alpha", pinned: true }),
    ];
    const { pinned } = partitionNotes(notes);
    expect(pinned.map((n) => n.title)).toEqual(["Alpha", "beta"]);
  });

  it("does not mutate the input array", () => {
    const notes = [note({ id: "a", lastOpenedAt: 1 }), note({ id: "b", lastOpenedAt: 2 })];
    const snapshot = notes.map((n) => n.id);
    partitionNotes(notes);
    expect(notes.map((n) => n.id)).toEqual(snapshot);
  });

  it("handles empty and single-note lists", () => {
    expect(partitionNotes([])).toEqual({ pinned: [], recent: [] });
    const one = partitionNotes([note({ id: "solo" })]);
    expect(one.pinned).toEqual([]);
    expect(one.recent.map((n) => n.id)).toEqual(["solo"]);
  });
});

describe("filterByTitle (SW-SN-2)", () => {
  const notes = [
    note({ id: "1", title: "Migration questions" }),
    note({ id: "2", title: "SW-SN-1 notes" }),
    note({ id: "3", title: "Prompts" }),
  ];

  it("narrows by case-insensitive substring", () => {
    expect(filterByTitle(notes, "mig").map((n) => n.id)).toEqual(["1"]);
    expect(filterByTitle(notes, "NOTES").map((n) => n.id)).toEqual(["2"]);
  });

  it("returns all notes for an empty/whitespace query", () => {
    expect(filterByTitle(notes, "").length).toBe(3);
    expect(filterByTitle(notes, "   ").length).toBe(3);
  });

  it("returns none when nothing matches", () => {
    expect(filterByTitle(notes, "zzz")).toEqual([]);
  });
});
