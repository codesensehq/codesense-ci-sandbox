// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useViewerStore } from "./viewer-store";

// The backend side: each note's body on disk, and the trash a delete moves a note
// into. Restoring a deleted note brings back the body it had on disk when deleted.
const disk = vi.hoisted(() => ({
  bodies: new Map<string, string>(),
  trash: new Map<string, string>(),
}));

const meta = (id: string, lastOpenedAt: number) => ({
  id,
  title: id,
  pinned: false,
  lastOpenedAt,
});

vi.mock("@/tauri/commands", () => ({
  listNotes: vi.fn(async () => [...disk.bodies.keys()].map((id, i) => meta(id, i + 1))),
  readNote: vi.fn(async (_scope: unknown, id: string) => ({
    ...meta(id, 1),
    content: disk.bodies.get(id) ?? "",
  })),
  writeNote: vi.fn(async (_scope: unknown, id: string, content: string) => {
    disk.bodies.set(id, content);
    return content;
  }),
  deleteNote: vi.fn(async (_scope: unknown, id: string) => {
    disk.trash.set(id, disk.bodies.get(id) ?? "");
    disk.bodies.delete(id);
    return [...disk.bodies.keys()].map((n, i) => meta(n, i + 1));
  }),
  reapEmptyScratchNotes: vi.fn(async () => null),
  createNote: vi.fn(),
  updateNoteMeta: vi.fn(async (_scope: unknown, id: string) => meta(id, 1)),
  saveAppState: vi.fn(async () => undefined),
  loadAppState: vi.fn(async () => "{}"),
  pushViewerState: vi.fn(async () => undefined),
}));

describe("deleting a note right after typing in it", () => {
  beforeEach(() => {
    disk.bodies = new Map([
      ["older", "other note\n"],
      ["plan", "line one\n"],
    ]);
    disk.trash = new Map();
    useViewerStore.setState({
      notes: [],
      activeNoteId: null,
      scratchNoteContent: "",
      notesLoadedScope: null,
      emptyScratchReapDone: true,
    });
  });

  it("restores every keystroke typed before the delete", async () => {
    await useViewerStore.getState().loadNotes();
    expect(useViewerStore.getState().activeNoteId).toBe("plan");

    vi.useFakeTimers();
    try {
      // The person types; the last keystroke's save is still waiting on its debounce.
      useViewerStore.getState().setScratchNoteContent("line one\nm");
      useViewerStore.getState().setScratchNoteContent("line one\nmi");
      useViewerStore.getState().setScratchNoteContent("line one\nmine");
      await vi.advanceTimersByTimeAsync(500);
      // They delete the note before a second has passed.
      const deleting = useViewerStore.getState().deleteNote("plan");
      await vi.runAllTimersAsync();
      await deleting;
    } finally {
      vi.useRealTimers();
    }

    expect(disk.trash.get("plan")).toBe("line one\nmine");
    expect(disk.bodies.has("plan")).toBe(false);
    expect(useViewerStore.getState().activeNoteId).toBe("older");
    expect(useViewerStore.getState().scratchNoteContent).toBe("other note\n");
  });
});
