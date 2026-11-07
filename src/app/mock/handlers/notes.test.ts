// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import {
  createNote,
  deleteNote,
  listNotes,
  notesDirPath,
  notesHistory,
  notesReadAt,
  notesRestore,
  readNote,
  reapEmptyScratchNotes,
  searchNotes,
  updateNoteMeta,
  writeNote,
} from "@/tauri/commands";
import { MockEventBus } from "../mock-events";
import { createInvokeRouter } from "../mock-router";
import { notesHandlers } from "./notes";

const scope = { githubUserId: 42, orgSlug: "acme/ctx" };

function useHandlers(search = "") {
  invoke.mockReset();
  invoke.mockImplementation(createInvokeRouter(notesHandlers(new MockEventBus(), search)));
}

async function lastResult() {
  return invoke.mock.results.at(-1)!.value;
}

describe("Notes wrappers against the mock handlers", () => {
  beforeEach(() => useHandlers());

  it("creates a full note through the shared write envelope", async () => {
    const note = await createNote(scope, "Plan");
    expect(note).toMatchObject({ title: "Plan", pinned: false, content: "" });
    expect(await lastResult()).toMatchObject({
      org: scope.orgSlug,
      note,
      changed: true,
      commit: null,
      merged: false,
      overlaps: [],
    });
    expect(await readNote(scope, note.id)).toEqual(note);
  });

  it("unwraps edited, unchanged and stale writes to editor body strings", async () => {
    const note = await createNote(scope, "Plan");
    expect(await writeNote(scope, note.id, "Buy milk", "")).toBe("Buy milk");
    expect(await lastResult()).toMatchObject({ note: { content: "Buy milk" }, changed: true });
    expect(await writeNote(scope, note.id, "Buy milk", "Buy milk")).toBe("Buy milk");
    expect(await lastResult()).toMatchObject({ changed: false });
    expect(await writeNote(scope, note.id, "", "")).toBe("Buy milk");
    expect(await lastResult()).toMatchObject({
      note: { content: "Buy milk" },
      changed: false,
      merged: true,
      overlaps: [],
    });
  });

  it("returns full notes after pin, rename and recency updates", async () => {
    const note = await createNote(scope, "Plan");
    await writeNote(scope, note.id, "Body");
    const before = await readNote(scope, note.id);
    expect(await updateNoteMeta(scope, note.id, { pinned: true })).toMatchObject({
      id: note.id,
      pinned: true,
      content: "Body",
    });
    expect(await updateNoteMeta(scope, note.id, { title: "Renamed" })).toMatchObject({
      id: note.id,
      title: "Renamed",
      pinned: true,
      content: "Body",
      updatedAt: before.updatedAt,
      updatedBy: before.updatedBy,
    });
    expect(await updateNoteMeta(scope, note.id, { lastOpenedAt: Date.now() })).toMatchObject({
      id: note.id,
      content: "Body",
    });
    expect(await lastResult()).toMatchObject({ changed: false });
  });

  it("merges independent stale edits and keeps both sides of overlapping edits", async () => {
    const note = await createNote(scope, "Plan");
    const base = "first\nmiddle\nlast\n";
    await writeNote(scope, note.id, base);
    await writeNote(scope, note.id, "their first\nmiddle\nlast\n");
    expect(await writeNote(scope, note.id, "first\nmiddle\nour last\n", base)).toBe(
      "their first\nmiddle\nour last\n",
    );
    expect(await lastResult()).toMatchObject({ merged: true, overlaps: [] });
    const merged = await writeNote(scope, note.id, "another first\nmiddle\nlast\n", base);
    expect(merged).toContain("their first");
    expect(merged).toContain("another first");
    expect(await lastResult()).toMatchObject({ merged: true, overlaps: [expect.any(Object)] });
  });

  it.each([false, true])("returns a full note from restore with undo=%s", async (undo) => {
    useHandlers("?notes=demo");
    const revisions = await notesHistory(scope, "prompts");
    const target = revisions[1];
    const expected = await notesReadAt(scope, "prompts", revisions[undo ? 2 : 1].commit);
    const restored = await notesRestore(scope, "prompts", target.commit, { undo });
    expect(restored).toMatchObject({ id: "prompts", content: expected.content });
    expect(await lastResult()).toMatchObject({
      note: restored,
      changed: true,
      commit: expect.any(String),
      merged: false,
      overlaps: [],
    });
    expect(await readNote(scope, "prompts")).toEqual(restored);
  });

  it("returns a delete envelope and refreshes the immediately reseeded survivor", async () => {
    const [note] = await listNotes(scope);
    const rows = await deleteNote(scope, note.id);
    const deleteCall = invoke.mock.calls.findIndex(([command]) => command === "delete_note");
    expect(await invoke.mock.results[deleteCall].value).toMatchObject({
      note: { id: note.id, content: "" },
      changed: true,
      commit: null,
      merged: false,
      overlaps: [],
    });
    expect(rows).toHaveLength(1);
    expect(await readNote(scope, rows[0].id)).toMatchObject({ title: "Note", content: "" });
    expect(invoke.mock.calls.slice(-2).map(([command]) => command)).toEqual([
      "list_notes",
      "read_note",
    ]);
  });

  it("keeps list, search, reap, directory and history read contracts", async () => {
    const note = await createNote(scope, "Plan");
    await writeNote(scope, note.id, "Buy milk");
    expect(await listNotes(scope)).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ id: note.id, title: "Plan", preview: "Buy milk", refs: [] }),
      ]),
    );
    expect(await searchNotes(scope, " MILK ")).toEqual([
      { id: note.id, title: "Plan", matched: "text", snippet: "Buy milk" },
    ]);
    const rows = await reapEmptyScratchNotes(scope, note.id, [note.id]);
    expect(rows.map((r) => r.id).sort()).toEqual((await listNotes(scope)).map((r) => r.id).sort());
    expect(await notesDirPath(scope)).toBe("/mock/users/1/scratch/notes");
    const revisions = await notesHistory(scope, note.id);
    expect(revisions).toHaveLength(1);
    expect(await notesReadAt(scope, note.id, revisions[0].commit)).toEqual({
      id: note.id,
      commit: revisions[0].commit,
      title: "Plan",
      content: "Buy milk",
    });
  });

  it("creates a note with its body and pinned flag in one call", async () => {
    const note = await createNote(scope, "Seeded", {
      content: "[X](cs:story:SW-1)\n",
      pinned: true,
    });
    expect(note).toMatchObject({ title: "Seeded", pinned: true, content: "[X](cs:story:SW-1)\n" });
    expect(await readNote(scope, note.id)).toEqual(note);
    const revisions = await notesHistory(scope, note.id);
    expect(revisions).toEqual([
      expect.objectContaining({
        actor: { kind: "human", name: "You" },
        change: "create",
        added: 1,
      }),
    ]);
    expect(await notesReadAt(scope, note.id, revisions[0].commit)).toMatchObject({
      content: "[X](cs:story:SW-1)\n",
    });
  });

  it("lists rows with the backend's preview, pinned first then most recent", async () => {
    const older = await createNote(scope, "Older", { content: "\n  first line  \nsecond" });
    const pinned = await createNote(scope, "Pinned", { pinned: true });
    await updateNoteMeta(scope, older.id, { lastOpenedAt: Date.now() + 60_000 });
    const rows = await listNotes(scope);
    expect(rows.map((r) => r.id).slice(0, 2)).toEqual([pinned.id, older.id]);
    expect(rows[1].preview).toBe("first line · second");
    expect(rows[0].preview).toBe("");
  });

  it("searches titles and bodies, title matches first", async () => {
    const body = await createNote(scope, "Errands", { content: "call the plumber" });
    const both = await createNote(scope, "Plumber", { content: "Plumber quote" });
    const title = await createNote(scope, "plumber invoices");
    const hits = await searchNotes(scope, "plumber");
    expect(hits.slice(0, 2)).toEqual(
      expect.arrayContaining([
        { id: both.id, title: "Plumber", matched: "both", snippet: "Plumber quote" },
        { id: title.id, title: "plumber invoices", matched: "title", snippet: "plumber invoices" },
      ]),
    );
    expect(hits[2]).toEqual({
      id: body.id,
      title: "Errands",
      matched: "text",
      snippet: "call the plumber",
    });
    expect(await searchNotes(scope, "   ")).toEqual([]);
  });
});
