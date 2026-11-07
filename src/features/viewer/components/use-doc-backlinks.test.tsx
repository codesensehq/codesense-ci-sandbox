// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { renderHook, waitFor } from "@testing-library/react";

import type { Note, NoteMeta } from "@/tauri/commands";
import { listNotes, readNote } from "@/tauri/commands";

import { useDocBacklinks } from "./use-doc-backlinks";
import { formatCsHref } from "../lib/note-links";

vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return { ...actual, listNotes: vi.fn(), readNote: vi.fn() };
});

const STORY = "/ctx/content/epic-a/SW-1.md";
const resolveCtx = {
  nodes: [{ storyId: "SW-1", storyFile: STORY }],
  teamContentPath: "/ctx/content",
};
const listNotesMock = listNotes as Mock;
const readNoteMock = readNote as Mock;

function meta(id: string): NoteMeta {
  return { id, title: id, pinned: false, lastOpenedAt: 0 };
}
function link(id: string): Note {
  return { ...meta(id), content: `[x](${formatCsHref({ kind: "story", target: "SW-1" })})` };
}

describe("useDocBacklinks (SW-SN-10 AC-3)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    listNotesMock.mockResolvedValue([meta("n1")]);
    readNoteMock.mockResolvedValue(link("n1"));
  });

  it("does nothing until enabled (non-blocking on mount)", () => {
    renderHook(() => useDocBacklinks({ enabled: false, activeTabPath: STORY, resolveCtx }));
    expect(listNotesMock).not.toHaveBeenCalled();
  });

  it("scans when enabled and returns the linking notes", async () => {
    const { result } = renderHook(() =>
      useDocBacklinks({ enabled: true, activeTabPath: STORY, resolveCtx }),
    );
    await waitFor(() => expect(result.current.backlinks).toHaveLength(1));
    expect(result.current.backlinks.map((n) => n.id)).toEqual(["n1"]);
    expect(result.current.loading).toBe(false);
  });

  it("re-scans on reopen so an in-place note edit is reflected (no stale cache)", async () => {
    const { result, rerender } = renderHook(
      ({ enabled }) => useDocBacklinks({ enabled, activeTabPath: STORY, resolveCtx }),
      { initialProps: { enabled: true } },
    );
    await waitFor(() => expect(result.current.backlinks).toHaveLength(1));

    // The note's body is edited to REMOVE the link while the chip is closed.
    readNoteMock.mockResolvedValue({ ...meta("n1"), content: "no links anymore" });
    rerender({ enabled: false });
    rerender({ enabled: true });

    // Reopening re-reads bodies and reflects the edit — the note drops off.
    await waitFor(() => expect(result.current.backlinks).toHaveLength(0));
  });

  it("clears results and loading when disabled", async () => {
    const { result, rerender } = renderHook(
      ({ enabled }) => useDocBacklinks({ enabled, activeTabPath: STORY, resolveCtx }),
      { initialProps: { enabled: true } },
    );
    await waitFor(() => expect(result.current.backlinks).toHaveLength(1));
    rerender({ enabled: false });
    expect(result.current).toEqual({ backlinks: [], loading: false });
  });
});
