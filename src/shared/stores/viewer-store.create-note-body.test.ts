// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useViewerStore } from "./viewer-store";
import * as commands from "@/tauri/commands";

// SW-SN-10: `createNote(title, initialBody)` seeds the body in the one `create_note`
// call and reflects it in-memory — never re-reading via setActiveNote (which no-ops
// on the already-active new id).
vi.mock("@/tauri/commands", () => ({
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "x", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi.fn(async (_scope: unknown, title: string, opts?: { content?: string }) => ({
    id: "seed-1",
    title,
    pinned: false,
    lastOpenedAt: 0,
    content: opts?.content ?? "",
  })),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "seed-1", title: "T", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
}));

const createNoteMock = commands.createNote as ReturnType<typeof vi.fn>;
const writeNoteMock = commands.writeNote as ReturnType<typeof vi.fn>;

describe("viewer-store createNote initialBody (SW-SN-10)", () => {
  beforeEach(() => {
    createNoteMock.mockClear();
    writeNoteMock.mockClear();
    useViewerStore.setState({ notes: [], activeNoteId: null, scratchNoteContent: "" });
  });

  it("seeds the body in one create call and reflects it in-memory + active", async () => {
    const body = `[X](cs:story:SW-1)\n\n`;
    const id = await useViewerStore.getState().createNote("Note about X", body);
    expect(id).toBe("seed-1");
    expect(createNoteMock).toHaveBeenCalledOnce();
    expect(createNoteMock).toHaveBeenCalledWith(
      { githubUserId: 0, orgSlug: null },
      "Note about X",
      {
        content: body,
      },
    );
    expect(writeNoteMock).not.toHaveBeenCalled();
    expect(useViewerStore.getState().activeNoteId).toBe("seed-1");
    expect(useViewerStore.getState().scratchNoteContent).toBe(body);
  });

  it("without initialBody, sends no content", async () => {
    await useViewerStore.getState().createNote("Plain");
    expect(createNoteMock).toHaveBeenCalledWith({ githubUserId: 0, orgSlug: null }, "Plain", {});
    expect(createNoteMock.mock.calls[0][2]).not.toHaveProperty("content");
    expect(writeNoteMock).not.toHaveBeenCalled();
  });
});
