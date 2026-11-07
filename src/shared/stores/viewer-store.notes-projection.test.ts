// SPDX-License-Identifier: MIT
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useViewerStore } from "./viewer-store";
import { createNote, listNotes, readNote, writeNote } from "@/tauri/commands";

const disk = vi.hoisted(() => ({ body: "" }));

vi.mock("@/tauri/commands", () => ({
  listNotes: vi.fn(async () => [
    { id: "n1", title: "Plan", pinned: false, lastOpenedAt: 5, updatedAt: 1 },
    { id: "n2", title: "Later", pinned: true, lastOpenedAt: 3, updatedAt: 1 },
  ]),
  readNote: vi.fn(async (_scope: unknown, id: string) => ({
    id,
    title: "Plan",
    pinned: false,
    lastOpenedAt: 5,
    content: disk.body,
  })),
  writeNote: vi.fn(async (_scope: unknown, _id: string, content: string) => {
    disk.body = content;
    return content;
  }),
  reapEmptyScratchNotes: vi.fn(async () => null),
  createNote: vi.fn(),
  updateNoteMeta: vi.fn(),
  deleteNote: vi.fn(async () => []),
  saveAppState: vi.fn(async () => undefined),
  loadAppState: vi.fn(async () => "{}"),
  pushViewerState: vi.fn(async () => undefined),
}));

const agent = { kind: "agent" as const, name: "Helper", handle: "helper#1" };
const row = (id: string, title: string, over: Record<string, unknown> = {}) => ({
  id,
  title,
  fields: { title, pinned: id === "n2" },
  preview: `${title} preview`,
  refs: [],
  updatedAt: 1,
  updatedBy: { kind: "human", name: "You" },
  ...over,
});
const scope = () => useViewerStore.getState().notesLoadedScope!;
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

describe("the native notes list on the records projection", () => {
  beforeEach(async () => {
    disk.body = "first\n";
    useViewerStore.setState({
      notes: [],
      activeNoteId: null,
      scratchNoteContent: "",
      notesLoadedScope: null,
      emptyScratchReapDone: true,
    });
    await useViewerStore.getState().loadNotes();
    vi.mocked(listNotes).mockClear();
    vi.mocked(readNote).mockClear();
  });

  it("an agent's write to another note updates the list from one delta, with no list or body read", async () => {
    const changed = row("n2", "Later", {
      preview: "Call the bank",
      refs: [{ handle: "jimmy", name: "Jimmy" }],
      updatedAt: 2,
      updatedBy: agent,
    });
    useViewerStore.getState().applyNotesProjection(scope(), [row("n1", "Plan"), changed], {
      upserts: [changed],
      kind: "write",
    });
    await settle();
    const notes = useViewerStore.getState().notes;
    expect(notes.find((n) => n.id === "n2")).toMatchObject({
      title: "Later",
      pinned: true,
      lastOpenedAt: 3,
      preview: "Call the bank",
      refs: [{ handle: "jimmy", name: "Jimmy" }],
      updatedBy: agent,
    });
    expect(notes.find((n) => n.id === "n1")?.lastOpenedAt).toBe(5);
    expect(listNotes).not.toHaveBeenCalled();
    expect(readNote).not.toHaveBeenCalled();
  });

  it("the open note adopts an agent's body with one read of that note", async () => {
    disk.body = "agent body\n";
    const changed = row("n1", "Plan", { updatedAt: 2, updatedBy: agent });
    useViewerStore.getState().applyNotesProjection(scope(), [changed, row("n2", "Later")], {
      upserts: [changed],
      kind: "write",
    });
    await vi.waitFor(() =>
      expect(useViewerStore.getState().scratchNoteContent).toBe("agent body\n"),
    );
    expect(readNote).toHaveBeenCalledTimes(1);
    expect(listNotes).not.toHaveBeenCalled();
  });

  it("the person's own save and later commit never re-read the open note", async () => {
    useViewerStore.getState().setScratchNoteContent("first\nmine\n");
    await useViewerStore.getState().flushNotes();
    const requestId = vi.mocked(writeNote).mock.calls.at(-1)![4];
    expect(requestId).toMatch(/^note-save:/);
    const saved = row("n1", "Plan", { updatedAt: 2 });
    const rows = [saved, row("n2", "Later")];
    useViewerStore
      .getState()
      .applyNotesProjection(scope(), rows, { upserts: [saved], kind: "write", requestId });
    useViewerStore
      .getState()
      .applyNotesProjection(scope(), rows, { upserts: [saved], kind: "commit" });
    await settle();
    expect(readNote).not.toHaveBeenCalled();
  });

  it("an agent's body does not replace what the person types while it is read", async () => {
    let release!: () => void;
    vi.mocked(readNote).mockImplementationOnce(async (_scope, id) => {
      await new Promise<void>((resolve) => (release = resolve));
      return { id, title: "Plan", pinned: false, lastOpenedAt: 5, content: "agent body\n" };
    });
    const changed = row("n1", "Plan", { updatedAt: 2, updatedBy: agent });
    useViewerStore
      .getState()
      .applyNotesProjection(scope(), [changed], { upserts: [changed], kind: "write" });
    await vi.waitFor(() => expect(readNote).toHaveBeenCalledTimes(1));
    useViewerStore.setState({ scratchNoteContent: "first\ntyping" });
    release();
    await settle();
    expect(useViewerStore.getState().scratchNoteContent).toBe("first\ntyping");
  });

  it("ignores another org's rows", () => {
    const before = useViewerStore.getState().notes;
    useViewerStore
      .getState()
      .applyNotesProjection({ ...scope(), orgSlug: "other/ctx" }, [row("x", "Other org")]);
    expect(useViewerStore.getState().notes).toBe(before);
  });

  it.each([undefined, "seeded body"])(
    "a created note shows once when its delta lands before the command returns (body %s)",
    async (initialBody) => {
      const created = row("n3", "New", { preview: "from projection" });
      // The projection delivers the new note while create_note is still in flight.
      vi.mocked(createNote).mockImplementationOnce(async () => {
        useViewerStore
          .getState()
          .applyNotesProjection(scope(), [row("n1", "Plan"), row("n2", "Later"), created], {
            upserts: [created],
            kind: "create",
          });
        return { id: "n3", title: "New", pinned: false, lastOpenedAt: 9, content: "" };
      });
      await useViewerStore.getState().createNote("New", initialBody);
      const notes = useViewerStore.getState().notes;
      expect(notes.filter((n) => n.id === "n3")).toHaveLength(1);
      expect(notes).toHaveLength(3);
      expect(notes.find((n) => n.id === "n3")).toMatchObject({ title: "New", lastOpenedAt: 9 });
      expect(useViewerStore.getState().activeNoteId).toBe("n3");
    },
  );

  it("a save to a note kept on this Mac with no org refreshes its list preview", async () => {
    useViewerStore
      .getState()
      .setScratchNoteContent("# Today\n- [ ] Call the bank [@Jimmy](cs:agent:jimmy)\n");
    await useViewerStore.getState().flushNotes();
    expect(useViewerStore.getState().notes.find((n) => n.id === "n1")).toMatchObject({
      preview: "Today · Call the bank",
      refs: [{ handle: "jimmy", name: "Jimmy" }],
    });
    vi.mocked(createNote).mockResolvedValueOnce({
      id: "n4",
      title: "Fresh",
      pinned: false,
      lastOpenedAt: 9,
      content: "First line\nsecond",
    });
    await useViewerStore.getState().createNote("Fresh", "First line\nsecond");
    expect(useViewerStore.getState().notes.find((n) => n.id === "n4")?.preview).toBe(
      "First line · second",
    );
    expect(readNote).not.toHaveBeenCalled();
  });

  it("an org's notes take their previews from the projection, not from saves", async () => {
    const org = { githubUserId: 42, orgSlug: "acme/ctx" };
    useViewerStore.setState({ notesLoadedScope: org });
    useViewerStore.getState().setScratchNoteContent("Typed here\n");
    await useViewerStore.getState().flushNotes();
    expect(useViewerStore.getState().notes.find((n) => n.id === "n1")?.preview).toBeUndefined();
  });
});
