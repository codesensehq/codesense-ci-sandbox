// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useViewerStore } from "./viewer-store";

// The backend side of a save: the body on disk, and a stand-in for its merge (the
// person's lines added after the body they started from go onto whatever is on disk
// now). A save that names no base replaces the body outright.
const disk = vi.hoisted(() => ({ body: "" }));

vi.mock("@/tauri/commands", () => ({
  listNotes: vi.fn(async () => [{ id: "n1", title: "Plan", pinned: false, lastOpenedAt: 1 }]),
  readNote: vi.fn(async () => ({
    id: "n1",
    title: "Plan",
    pinned: false,
    lastOpenedAt: 1,
    content: disk.body,
  })),
  writeNote: vi.fn(async (_scope: unknown, _id: string, content: string, base?: string) => {
    if (base !== undefined && disk.body !== base && content.startsWith(base)) {
      disk.body = disk.body + content.slice(base.length);
    } else {
      disk.body = content;
    }
    return disk.body;
  }),
  reapEmptyScratchNotes: vi.fn(async () => null),
  createNote: vi.fn(),
  updateNoteMeta: vi.fn(async () => ({ id: "n1", title: "Plan", pinned: false, lastOpenedAt: 1 })),
  deleteNote: vi.fn(async () => []),
  saveAppState: vi.fn(async () => undefined),
  loadAppState: vi.fn(async () => "{}"),
  pushViewerState: vi.fn(async () => undefined),
}));

describe("an agent's write to the open note while the person types", () => {
  beforeEach(() => {
    disk.body = "line one\n";
    useViewerStore.setState({
      notes: [],
      activeNoteId: null,
      scratchNoteContent: "",
      notesLoadedScope: null,
      emptyScratchReapDone: true,
    });
  });

  it("keeps the agent's text when the person's pending save lands on reload", async () => {
    await useViewerStore.getState().loadNotes();
    expect(useViewerStore.getState().scratchNoteContent).toBe("line one\n");

    vi.useFakeTimers();
    try {
      // The person types; the save waits for its debounce.
      useViewerStore.getState().setScratchNoteContent("line one\nmine\n");
      // An agent writes the note through notes_write meanwhile.
      disk.body = "agent line\nline one\n";
      // The notes-changed reload flushes the pending save first.
      const reload = useViewerStore.getState().loadNotes();
      await vi.runAllTimersAsync();
      await reload;
    } finally {
      vi.useRealTimers();
    }

    expect(disk.body).toBe("agent line\nline one\nmine\n");
    expect(useViewerStore.getState().scratchNoteContent).toBe("agent line\nline one\nmine\n");
  });

  it("shows the merged body when a save comes back merged and nothing was typed since", async () => {
    await useViewerStore.getState().loadNotes();
    disk.body = "agent line\nline one\n";
    useViewerStore.getState().setScratchNoteContent("line one\nmine\n");
    await useViewerStore.getState().flushNotes();
    expect(disk.body).toBe("agent line\nline one\nmine\n");
    expect(useViewerStore.getState().scratchNoteContent).toBe("agent line\nline one\nmine\n");

    // The next save starts from the merged body, so it replaces it as typed.
    useViewerStore.getState().setScratchNoteContent("agent line\nline one\nmine\nmore\n");
    await useViewerStore.getState().flushNotes();
    expect(disk.body).toBe("agent line\nline one\nmine\nmore\n");
  });
});
