// SPDX-License-Identifier: MIT
import { beforeEach, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import {
  createNote,
  deleteNote,
  listNotes,
  notesRestore,
  searchNotes,
  updateNoteMeta,
  writeNote,
  type NoteHit,
  type NoteRow,
} from "./commands";

const scope = { githubUserId: 42, orgSlug: "acme/ctx" };
const note = { id: "abc-1", title: "Plan", pinned: false, lastOpenedAt: 0, content: "both edits" };
const written = {
  org: "acme/ctx",
  note,
  changed: true,
  commit: null,
  merged: true,
  overlaps: [{ ours: "mine", theirs: "theirs", at: 1 }],
};

beforeEach(() => invoke.mockReset());

it("keeps the editor body contract when a shared write returns a merged result", async () => {
  invoke.mockResolvedValue(written);
  expect(await writeNote(scope, note.id, "mine", "base")).toBe("both edits");
  expect(invoke).toHaveBeenCalledWith("write_note", {
    githubUserId: 42,
    orgSlug: "acme/ctx",
    id: note.id,
    content: "mine",
    base: "base",
  });
});

it("returns the full note after create and restore", async () => {
  invoke.mockResolvedValue(written);
  expect(await createNote(scope, "Plan")).toEqual(note);
  expect(await notesRestore(scope, note.id, "abcd", { undo: true })).toEqual(note);
  expect(invoke).toHaveBeenLastCalledWith("notes_restore", {
    githubUserId: 42,
    orgSlug: "acme/ctx",
    id: note.id,
    commit: "abcd",
    undo: true,
  });
});

it("returns metadata after a pin or local recency update", async () => {
  invoke.mockResolvedValue(written);
  expect(await updateNoteMeta(scope, note.id, { pinned: true, lastOpenedAt: 21 })).toEqual(note);
});

it("refreshes the surviving rows after delete", async () => {
  const rows = [
    { id: "abc-2", title: "Prompts", pinned: true, lastOpenedAt: 0, preview: "Prompt" },
  ];
  invoke.mockResolvedValueOnce(written).mockResolvedValueOnce(rows);
  expect(await deleteNote(scope, note.id)).toEqual(rows);
  expect(invoke.mock.calls.map(([name]) => name)).toEqual(["delete_note", "list_notes"]);
});

it("creates a note with its body in one call", async () => {
  invoke.mockResolvedValue(written);
  expect(await createNote(scope, "Plan", { content: "both edits", pinned: true })).toEqual(note);
  expect(invoke).toHaveBeenCalledTimes(1);
  expect(invoke).toHaveBeenCalledWith("create_note", {
    githubUserId: 42,
    orgSlug: "acme/ctx",
    title: "Plan",
    content: "both edits",
    pinned: true,
  });
});

it("passes list rows and search hits through in the backend's shape", async () => {
  const rows: NoteRow[] = [
    {
      id: "abc-1",
      title: "Plan",
      pinned: false,
      lastOpenedAt: 3,
      updatedAt: 4,
      preview: "Ship it",
    },
  ];
  const hits: NoteHit[] = [
    { id: "abc-1", title: "Plan", matched: "title", snippet: "Plan" },
    { id: "abc-2", title: "Todo", matched: "text", snippet: null },
  ];
  invoke.mockResolvedValueOnce(rows).mockResolvedValueOnce(hits);
  expect(await listNotes(scope)).toEqual(rows);
  expect(await searchNotes(scope, "plan")).toEqual(hits);
  expect(invoke).toHaveBeenLastCalledWith("search_notes", {
    githubUserId: 42,
    orgSlug: "acme/ctx",
    query: "plan",
  });
});
