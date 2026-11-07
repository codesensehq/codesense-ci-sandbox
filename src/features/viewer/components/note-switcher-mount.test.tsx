// SPDX-License-Identifier: MIT
import type { NotesScope } from "@/shared/lib/notes-scope";
import { beforeEach, describe, expect, it, vi, type Mock } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import type { NoteHit, NoteMeta } from "@/tauri/commands";
import { searchNotes } from "@/tauri/commands";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { NoteSwitcherMount } from "./note-switcher-mount";

/** Logged out, no active org: the machine-local notes scope. */
const LOCAL_SCOPE = { githubUserId: 0, orgSlug: null };

// SW-SN-11: mock only the backend body scan; keep the rest of the command module.
vi.mock("@/tauri/commands", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/tauri/commands")>();
  return { ...actual, searchNotes: vi.fn() };
});
const searchNotesMock = vi.mocked(searchNotes);

function note(over: Partial<NoteMeta> & { id: string }): NoteMeta {
  return { title: over.id, pinned: false, lastOpenedAt: 0, ...over };
}

function hit(id: string, snippet: string | null, matched: NoteHit["matched"] = "text"): NoteHit {
  return { id, title: id, matched, snippet };
}

describe("NoteSwitcherMount (SW-SN-3)", () => {
  let revealSpy: Mock<() => void>;
  let setActiveSpy: Mock<(id: string) => Promise<void>>;

  beforeEach(() => {
    revealSpy = vi.fn<() => void>();
    setActiveSpy = vi.fn<(id: string) => Promise<void>>().mockResolvedValue(undefined);
    // Inject spies for the two store actions the mount dispatches; seed notes.
    useViewerStore.setState({
      notes: [
        note({ id: "a", title: "Alpha", lastOpenedAt: 100 }),
        note({ id: "b", title: "Beta", pinned: true }),
      ],
      revealPanelById: revealSpy,
      setActiveNote: setActiveSpy,
    });
    useUiStore.setState({ noteSwitcherOpen: false });
    searchNotesMock.mockReset();
    searchNotesMock.mockResolvedValue([]);
  });

  it("does not render the dialog when the view is inactive (gated off)", () => {
    useUiStore.setState({ noteSwitcherOpen: true });
    render(<NoteSwitcherMount isActiveView={false} />);
    expect(screen.queryByPlaceholderText("Search notes…")).toBeNull();
  });

  it("renders notes in partitionNotes order (pinned first) when open + active", () => {
    useUiStore.setState({ noteSwitcherOpen: true });
    render(<NoteSwitcherMount isActiveView={true} />);
    const rows = screen.getAllByRole("option");
    expect(rows[0]).toHaveTextContent("Beta"); // pinned
    expect(rows[1]).toHaveTextContent("Alpha"); // recent
  });

  it("on select: reveals the notes surface, activates the note, and closes", async () => {
    useUiStore.setState({ noteSwitcherOpen: true });
    render(<NoteSwitcherMount isActiveView={true} />);
    await userEvent.click(screen.getByText("Alpha"));
    expect(revealSpy).toHaveBeenCalledOnce();
    expect(setActiveSpy).toHaveBeenCalledWith("a");
    expect(useUiStore.getState().noteSwitcherOpen).toBe(false);
  });

  it("typing fuzzy-filters by title (AC-1)", async () => {
    useUiStore.setState({ noteSwitcherOpen: true });
    render(<NoteSwitcherMount isActiveView={true} />);
    await userEvent.type(screen.getByPlaceholderText("Search notes…"), "alph");
    expect(screen.getByText("Alpha")).toBeInTheDocument();
    expect(screen.queryByText("Beta")).toBeNull();
  });

  it("Enter activates the highlighted note (AC-3 keyboard path)", async () => {
    useUiStore.setState({ noteSwitcherOpen: true });
    render(<NoteSwitcherMount isActiveView={true} />);
    // Type to narrow to a single row, then Enter.
    await userEvent.type(screen.getByPlaceholderText("Search notes…"), "beta{Enter}");
    expect(setActiveSpy).toHaveBeenCalledWith("b");
    expect(useUiStore.getState().noteSwitcherOpen).toBe(false);
  });

  it("Escape closes without activating a note or revealing the pane (AC-3)", async () => {
    useUiStore.setState({ noteSwitcherOpen: true });
    render(<NoteSwitcherMount isActiveView={true} />);
    await userEvent.type(screen.getByPlaceholderText("Search notes…"), "{Escape}");
    expect(setActiveSpy).not.toHaveBeenCalled();
    expect(revealSpy).not.toHaveBeenCalled();
    expect(useUiStore.getState().noteSwitcherOpen).toBe(false);
  });

  describe("body/content search (SW-SN-11)", () => {
    it("does not scan on an empty query — opening stays instant (AC-4)", () => {
      useUiStore.setState({ noteSwitcherOpen: true });
      render(<NoteSwitcherMount isActiveView={true} />);
      expect(searchNotesMock).not.toHaveBeenCalled();
    });

    it("ranks title matches above body matches and shows the matched line as a snippet (AC-2/AC-3)", async () => {
      // `a` matches by title; `c` matches only in its body.
      useViewerStore.setState({
        notes: [
          note({ id: "a", title: "curl notes", lastOpenedAt: 100 }),
          note({ id: "c", title: "Gamma", lastOpenedAt: 50 }),
        ],
      });
      // Backend returns a hit for BOTH — the title-matcher must dedup `a` away.
      searchNotesMock.mockResolvedValue([
        hit("a", "curl -X POST", "both"),
        hit("c", "then curl the api"),
      ]);
      useUiStore.setState({ noteSwitcherOpen: true });
      render(<NoteSwitcherMount isActiveView={true} />);

      await userEvent.type(screen.getByPlaceholderText("Search notes…"), "curl");
      await waitFor(() => expect(searchNotesMock).toHaveBeenCalledWith(LOCAL_SCOPE, "curl"));

      // The body snippet row for `c` appears...
      await screen.findByText("then curl the api");
      const rows = screen.getAllByRole("option");
      // Exactly two rows: `a` once (as a title match), `c` once (as a body match).
      expect(rows).toHaveLength(2);
      expect(rows[0]).toHaveTextContent("curl notes"); // tier 0 (title) first
      expect(rows[1]).toHaveTextContent("Gamma"); // tier 1 (body) after
      expect(rows[1]).toHaveTextContent("then curl the api"); // matched-line snippet
    });

    it("shows a Searching… affordance instead of flashing 'No notes found' while a scan is in flight (AC-4 UX)", async () => {
      useViewerStore.setState({ notes: [note({ id: "c", title: "Gamma" })] });
      // A scan we control — stays pending until we resolve it.
      let resolveScan!: (hits: NoteHit[]) => void;
      searchNotesMock.mockImplementation(
        () => new Promise<NoteHit[]>((res) => (resolveScan = res)),
      );
      useUiStore.setState({ noteSwitcherOpen: true });
      render(<NoteSwitcherMount isActiveView={true} />);

      // "zzz" matches no title → pre-fix this flashed "No notes found".
      await userEvent.type(screen.getByPlaceholderText("Search notes…"), "zzz");
      expect(await screen.findByText("Searching…")).toBeInTheDocument();
      expect(screen.queryByText("No notes found")).toBeNull();

      // Wait for the debounced scan to actually fire before resolving it.
      await waitFor(() => expect(searchNotesMock).toHaveBeenCalledWith(LOCAL_SCOPE, "zzz"));
      // Resolving the scan replaces the affordance with the body row.
      resolveScan([hit("c", "the zzz line")]);
      expect(await screen.findByText("the zzz line")).toBeInTheDocument();
      expect(screen.queryByText("Searching…")).toBeNull();
    });

    it("ignores a superseded scan — stale hits don't render and don't clear the searching flag", async () => {
      useViewerStore.setState({ notes: [note({ id: "c", title: "Gamma" })] });
      // Each scan returns a promise we resolve by hand, keyed by query.
      const scans = new Map<string, (hits: NoteHit[]) => void>();
      searchNotesMock.mockImplementation(
        (_scope: NotesScope, q: string) => new Promise<NoteHit[]>((res) => scans.set(q, res)),
      );
      useUiStore.setState({ noteSwitcherOpen: true });
      render(<NoteSwitcherMount isActiveView={true} />);
      const input = screen.getByPlaceholderText("Search notes…");

      // Scan for "a" fires and is left pending.
      await userEvent.type(input, "a");
      await waitFor(() => expect(scans.has("a")).toBe(true));

      // Advance the query to "ab" (a newer scan supersedes "a").
      await userEvent.type(input, "b");

      // Resolve the SUPERSEDED "a" scan with hits — must be ignored (and it
      // unblocks the chained "ab" scan). Snippet contains "ab" so it would
      // survive the dialog's own fuzzy filter if wrongly applied.
      scans.get("a")!([hit("c", "stale grAB row")]);
      await waitFor(() => expect(scans.has("ab")).toBe(true));

      // Stale hits not shown; still searching (the guard didn't clear it).
      expect(screen.queryByText("stale grAB row")).toBeNull();
      expect(screen.getByText("Searching…")).toBeInTheDocument();

      // The current "ab" scan applies normally.
      scans.get("ab")!([hit("c", "fresh grAB row")]);
      expect(await screen.findByText("fresh grAB row")).toBeInTheDocument();
      expect(screen.queryByText("Searching…")).toBeNull();
    });

    it("a failed scan clears the loading state and falls back to titles (no stuck 'Searching…')", async () => {
      useViewerStore.setState({ notes: [note({ id: "c", title: "Gamma" })] });
      searchNotesMock.mockRejectedValue(new Error("index corrupt"));
      useUiStore.setState({ noteSwitcherOpen: true });
      render(<NoteSwitcherMount isActiveView={true} />);

      // "zzz" matches no title → without error handling this would hang on
      // "Searching…" forever (the scan rejects, so no result would be stamped).
      await userEvent.type(screen.getByPlaceholderText("Search notes…"), "zzz");
      await waitFor(() => expect(searchNotesMock).toHaveBeenCalledWith(LOCAL_SCOPE, "zzz"));

      // The rejection is caught and stamped as an empty result → flag clears.
      expect(await screen.findByText("No notes found")).toBeInTheDocument();
      expect(screen.queryByText("Searching…")).toBeNull();
    });

    it("selecting a body-match row routes through setActiveNote(id) (AC-5)", async () => {
      useViewerStore.setState({
        notes: [note({ id: "c", title: "Gamma", lastOpenedAt: 50 })],
      });
      searchNotesMock.mockResolvedValue([hit("c", "then curl the api")]);
      useUiStore.setState({ noteSwitcherOpen: true });
      render(<NoteSwitcherMount isActiveView={true} />);

      await userEvent.type(screen.getByPlaceholderText("Search notes…"), "curl");
      const bodyRow = await screen.findByText("then curl the api");
      await userEvent.click(bodyRow);

      expect(revealSpy).toHaveBeenCalledOnce();
      expect(setActiveSpy).toHaveBeenCalledWith("c");
      expect(useUiStore.getState().noteSwitcherOpen).toBe(false);
    });
  });

  describe("NoteHit fields", () => {
    async function search(query: string, hits: NoteHit[]) {
      useViewerStore.setState({
        notes: [
          note({ id: "a", title: "curl notes", lastOpenedAt: 100 }),
          note({ id: "c", title: "Gamma", lastOpenedAt: 50 }),
        ],
      });
      searchNotesMock.mockResolvedValue(hits);
      useUiStore.setState({ noteSwitcherOpen: true });
      render(<NoteSwitcherMount isActiveView={true} />);
      await userEvent.type(screen.getByPlaceholderText("Search notes…"), query);
      await waitFor(() => expect(searchNotesMock).toHaveBeenCalledWith(LOCAL_SCOPE, query));
    }

    it("a title-only hit adds no body row", async () => {
      // The backend's title differs from the in-memory one (renamed since): the
      // title tier alone decides title matches, so no body row appears.
      await search("zzq", [hit("c", "zzq renamed", "title")]);
      expect(await screen.findByText("No notes found")).toBeInTheDocument();
      expect(screen.queryByText("Gamma")).toBeNull();
    });

    it("a both hit on a fuzzy-matching title appears once, as a title row", async () => {
      await search("curl", [hit("a", "curl -X POST", "both"), hit("c", "curl it", "text")]);
      await screen.findByText("curl it");
      const rows = screen.getAllByRole("option");
      expect(rows).toHaveLength(2);
      expect(rows[0]).toHaveTextContent("curl notes");
      expect(rows[0]).not.toHaveTextContent("curl -X POST");
    });

    it("a text hit with a null snippet settles without crashing", async () => {
      await search("zzq", [hit("c", null)]);
      // An empty sublabel and a non-matching title leave nothing for the dialog to show.
      expect(await screen.findByText("No notes found")).toBeInTheDocument();
      expect(screen.queryByText("Searching…")).toBeNull();
    });
  });
});
