// SPDX-License-Identifier: MIT
import { describe, expect, it, vi, beforeEach } from "vitest";
import { act, fireEvent, render, screen, waitFor, within } from "@/test-utils";

import type { NoteRevision } from "@/tauri/commands";

import { ChangeBanner, HistorySheet } from "../components/note-changes";
import { resetNoteHistoryAvailabilityForTests, useNoteHistory } from "./use-note-history";

// The history UI over the REAL `commands.ts` wrappers: only Tauri's `invoke` is
// stubbed, so the command names and argument keys asserted here are what the Rust
// handlers receive (`notes_history`, `notes_read_at` / `notes_restore` with `commit`).

const h = vi.hoisted(() => ({
  invoke: vi.fn(),
  revisions: [] as NoteRevision[],
  listeners: [] as ((event: { payload: unknown }) => void)[],
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: h.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((_name: string, fn: (event: { payload: unknown }) => void) => {
    h.listeners.push(fn);
    return Promise.resolve(() => {
      h.listeners = h.listeners.filter((l) => l !== fn);
    });
  }),
}));
vi.mock("@/features/notes/lib/note-agents", () => ({
  useNoteAgents: () => ({ byHandle: new Map() }),
}));
vi.mock("@/features/agents/lib/agent-names", () => ({
  useAgentRows: () => ({ rows: [], error: null }),
}));
vi.mock("@/shared/stores/viewer-store", () => {
  const state = {
    notesLoadedScope: { githubUserId: 7, orgSlug: "acme" },
    flushNotes: vi.fn(() => Promise.resolve()),
    loadNotes: vi.fn(() => Promise.resolve()),
  };
  const store = () => state;
  store.getState = () => state;
  return { useViewerStore: store };
});

const NOW = Date.UTC(2026, 9, 6, 18, 0);

const REVISIONS: NoteRevision[] = [
  {
    commit: "c3",
    at: NOW - 4 * 60_000,
    actor: { kind: "agent", name: "Notes tidy", handle: "notes-tidy", sessionId: "s1" },
    change: "write",
    summary: "Notes tidy grouped 3 lines under This week",
    added: 4,
    removed: 3,
  },
  {
    commit: "c2",
    at: NOW - 60 * 60_000,
    actor: { kind: "human", name: "You" },
    change: "write",
    summary: "Edited by you",
    added: 2,
    removed: 0,
  },
];

function Harness() {
  const history = useNoteHistory("n1");
  const rev = history.unseenAgentChange;
  return (
    <>
      {rev && (
        <ChangeBanner
          rev={rev}
          reviewing={false}
          now={NOW}
          onReview={() => void history.readAt(history.revisionBefore(rev.commit)!.commit)}
          onUndo={() => void history.undo(rev.commit)}
          onDone={() => history.markSeen(rev.commit)}
        />
      )}
      <HistorySheet
        top={0}
        title="Prompts"
        entries={history.entries}
        now={NOW}
        onReview={() => undefined}
        onRestore={(r) => void history.restore(r.commit)}
        onClose={() => undefined}
      />
    </>
  );
}

beforeEach(() => {
  resetNoteHistoryAvailabilityForTests();
  localStorage.clear();
  h.revisions = REVISIONS;
  h.listeners = [];
  h.invoke.mockReset();
  h.invoke.mockImplementation((cmd: string, args: Record<string, unknown>) => {
    if (cmd === "notes_history") return Promise.resolve(h.revisions);
    if (cmd === "notes_read_at")
      return Promise.resolve({ id: args.id, commit: args.commit, content: "before" });
    if (cmd === "notes_restore") return Promise.resolve({ id: args.id, content: "before" });
    return Promise.reject(new Error(`unexpected ${cmd}`));
  });
});

describe("note history over the IPC seam", () => {
  it("reads the active note's revisions with the loaded scope", async () => {
    render(<Harness />);
    await screen.findByRole("status");
    expect(h.invoke).toHaveBeenCalledWith("notes_history", {
      githubUserId: 7,
      orgSlug: "acme",
      id: "n1",
      limit: undefined,
    });
  });

  it("names the agent once in the banner and the history rows", async () => {
    render(<Harness />);
    const banner = await screen.findByRole("status");
    expect(banner.textContent).toContain("Notes tidy reorganized this");
    // What it did, without its name repeated, rides along as the banner's tooltip and
    // reads in full in the history row.
    expect(within(banner).getByTitle("grouped 3 lines under This week")).toBeTruthy();
    expect(
      screen.getByText("Notes tidy", { selector: ".nt-hrow-text b" }).parentElement?.textContent,
    ).toBe("Notes tidy grouped 3 lines under This week");
    expect(banner.textContent).toContain("7 changes");
    // The person's stock "Edited by you" summary reads "You edited".
    expect(screen.getByText("You", { selector: "b" }).parentElement?.textContent).toBe(
      "You edited",
    );
  });

  it("Undo takes back only the agent's revision, so later typing stays", async () => {
    render(<Harness />);
    await screen.findByRole("status");
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "Undo these changes" }));
    });
    expect(h.invoke).toHaveBeenCalledWith("notes_restore", {
      githubUserId: 7,
      orgSlug: "acme",
      id: "n1",
      commit: "c3",
      undo: true,
    });
  });

  it("re-reads history for a new revision, not for every save while typing", async () => {
    render(<Harness />);
    await screen.findByRole("status");
    const reads = () => h.invoke.mock.calls.filter(([cmd]) => cmd === "notes_history").length;
    const before = reads();
    const emit = (payload: unknown) =>
      act(async () => {
        for (const l of h.listeners) l({ payload });
      });
    await emit({ orgSlug: "acme", id: "n1", actor: { kind: "human", name: "You" }, kind: "write" });
    expect(reads()).toBe(before);
    await emit({
      orgSlug: "acme",
      id: "n1",
      commit: "c4",
      actor: { kind: "human", name: "You" },
      kind: "write",
    });
    expect(reads()).toBe(before + 1);
  });

  it("Review reads the earlier body by commit", async () => {
    render(<Harness />);
    await screen.findByRole("status");
    await act(async () => {
      fireEvent.click(within(screen.getByRole("status")).getByRole("button", { name: "Review" }));
    });
    expect(h.invoke).toHaveBeenCalledWith(
      "notes_read_at",
      expect.objectContaining({ id: "n1", commit: "c2" }),
    );
  });

  it("no banner once the person has looked; none when the person made the latest change", async () => {
    h.revisions = [REVISIONS[1]];
    render(<Harness />);
    await waitFor(() => expect(h.invoke).toHaveBeenCalled());
    await act(async () => {});
    expect(screen.queryByRole("status")).toBeNull();
  });

  it("hides the history UI on an install whose backend has no history commands", async () => {
    h.invoke.mockImplementation(() => Promise.reject(new Error("Command notes_history not found")));
    render(<Harness />);
    await act(async () => {});
    expect(screen.queryByRole("status")).toBeNull();
    expect(screen.getByText("No saved changes yet.")).toBeTruthy();
  });
});
