// SPDX-License-Identifier: MIT
import type { NotesScope } from "@/shared/lib/notes-scope";
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn(),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Note", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi.fn().mockResolvedValue({
    id: "n2",
    title: "Untitled",
    pinned: false,
    lastOpenedAt: 0,
    content: "",
  }),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Note", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
  // SW-SN-13 AC-4: default no-op (returns undefined → loadNotes keeps its
  // listNotes result). Reap-specific tests override this per case.
  reapEmptyScratchNotes: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

import { useViewerStore, flushViewerForProfileSwitch } from "./viewer-store";

/** Logged out, no active org: the machine-local notes scope. */
const LOCAL_SCOPE = { githubUserId: 0, orgSlug: null };

beforeEach(() => {
  // Reset store to defaults. F-6: scratch-note fields are scalars now.
  useViewerStore.setState({
    layoutMode: null,
    lastTerminalMode: "shared",
    terminalVisible: false,
    navCollapsed: {},
    tabDiff: {},
    openTabs: {},
    activeTab: {},
    scratchNoteContent: "",
    notes: [],
    activeNoteId: null,
    scratchNoteLoadStatus: "pending",
    emptyScratchReapDone: false,
    rightPaneOpen: false,
    rightPaneRatio: 0.33,
  });
  vi.clearAllMocks();
});

describe("layoutMode", () => {
  it("setLayoutMode sets mode and syncs terminalVisible", () => {
    useViewerStore.getState().setLayoutMode("console");
    expect(useViewerStore.getState().layoutMode).toBe("console");
    expect(useViewerStore.getState().terminalVisible).toBe(true);

    useViewerStore.getState().setLayoutMode("viewer");
    expect(useViewerStore.getState().layoutMode).toBe("viewer");
    expect(useViewerStore.getState().terminalVisible).toBe(false);

    useViewerStore.getState().setLayoutMode("shared");
    expect(useViewerStore.getState().layoutMode).toBe("shared");
    expect(useViewerStore.getState().terminalVisible).toBe(true);
  });

  it("setLayoutMode stores lastTerminalMode for console and shared", () => {
    useViewerStore.getState().setLayoutMode("console");
    expect(useViewerStore.getState().lastTerminalMode).toBe("console");

    useViewerStore.getState().setLayoutMode("viewer");
    // lastTerminalMode should NOT be updated for viewer
    expect(useViewerStore.getState().lastTerminalMode).toBe("console");

    useViewerStore.getState().setLayoutMode("shared");
    expect(useViewerStore.getState().lastTerminalMode).toBe("shared");
  });

  it("toggleConsoleShared toggles between console and shared", () => {
    useViewerStore.getState().setLayoutMode("shared");
    useViewerStore.getState().toggleConsoleShared();
    expect(useViewerStore.getState().layoutMode).toBe("console");

    useViewerStore.getState().toggleConsoleShared();
    expect(useViewerStore.getState().layoutMode).toBe("shared");
  });

  it("toggleConsoleShared is no-op in viewer mode", () => {
    useViewerStore.getState().setLayoutMode("viewer");
    useViewerStore.getState().toggleConsoleShared();
    expect(useViewerStore.getState().layoutMode).toBe("viewer");
  });

  it("defaults to shared when layoutMode not set", () => {
    const mode = useViewerStore.getState().layoutMode ?? "shared";
    expect(mode).toBe("shared");
  });
});

// Full-screen CLI is left only by a user action: a close that empties the CLI keeps
// it, and the CLI chrome keeps the full-screen toggle on the empty state.
describe("a close never leaves full-screen CLI", () => {
  const TEAM = "acme:default";
  const WS = "epic-x-story-a";

  beforeEach(() => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [{ id: WS, kind: "story", cwd: "/a", status: "ready" }] },
      activeWorkspace: { [TEAM]: WS },
      layoutMode: "console",
      lastTerminalMode: "console",
    });
  });

  it("keeps console and its reopen mode when the last workspace closes: the CLI shows its empty state", () => {
    useViewerStore.getState().closeWorkspace(TEAM, WS);
    expect(useViewerStore.getState()).toMatchObject({
      layoutMode: "console",
      lastTerminalMode: "console",
    });
  });

  it("does not touch layoutMode when the last workspace closes in shared mode", () => {
    useViewerStore.setState({
      layoutMode: "shared",
      lastTerminalMode: "shared",
    });
    useViewerStore.getState().closeWorkspace(TEAM, WS);
    expect(useViewerStore.getState().layoutMode).toBe("shared");
  });
});

describe("navCollapsed (SW-FX-8)", () => {
  it("setNavCollapsed sets per-team collapsed state", () => {
    useViewerStore.getState().setNavCollapsed("acme:default", true);
    expect(useViewerStore.getState().navCollapsed["acme:default"]).toBe(true);

    useViewerStore.getState().setNavCollapsed("acme:default", false);
    expect(useViewerStore.getState().navCollapsed["acme:default"]).toBe(false);
  });

  it("is independent per team", () => {
    useViewerStore.getState().setNavCollapsed("acme:a", true);
    useViewerStore.getState().setNavCollapsed("acme:b", false);
    expect(useViewerStore.getState().navCollapsed["acme:a"]).toBe(true);
    expect(useViewerStore.getState().navCollapsed["acme:b"]).toBe(false);
  });

  it("is undefined (treated as expanded) when unset", () => {
    expect(useViewerStore.getState().navCollapsed["never:set"]).toBeUndefined();
  });
});

describe("tabDiff: a tab on its Changes", () => {
  const TEAM = "acme:default";
  const REPO = "workspace:acme:default:acme/widgets";

  it("openChangeTab opens an ordinary tab, on its Changes", () => {
    useViewerStore.getState().openChangeTab(TEAM, REPO, "/ctx/AID-4.md", false);
    const s = useViewerStore.getState();
    expect(s.openTabs[TEAM]).toEqual([
      expect.objectContaining({ path: "/ctx/AID-4.md", preview: true }),
    ]);
    expect(s.activeTab[TEAM]).toBe("/ctx/AID-4.md");
    expect(s.tabDiff[TEAM]).toEqual({ "/ctx/AID-4.md": true });
  });

  it("a pinned open is a pinned tab, still on its Changes", () => {
    useViewerStore.getState().openChangeTab(TEAM, REPO, "/ctx/AID-4.md", true);
    expect(useViewerStore.getState().openTabs[TEAM]?.[0]?.preview).toBeFalsy();
    expect(useViewerStore.getState().tabDiff[TEAM]?.["/ctx/AID-4.md"]).toBe(true);
  });

  it("setTabDiff flips one tab between File and Changes", () => {
    useViewerStore.getState().openChangeTab(TEAM, REPO, "/ctx/a.md", true);
    useViewerStore.getState().setTabDiff(TEAM, "/ctx/a.md", false);
    expect(useViewerStore.getState().tabDiff[TEAM]).toEqual({});
    useViewerStore.getState().setTabDiff(TEAM, "/ctx/a.md", true);
    expect(useViewerStore.getState().tabDiff[TEAM]).toEqual({ "/ctx/a.md": true });
  });

  it("closing the tab forgets its side, so reopening it shows the file", () => {
    useViewerStore.getState().openChangeTab(TEAM, REPO, "/ctx/a.md", true);
    useViewerStore.getState().openFile(TEAM, REPO, "/ctx/b.md");
    useViewerStore.getState().closeTab(TEAM, REPO, "/ctx/a.md");
    expect(useViewerStore.getState().tabDiff[TEAM]?.["/ctx/a.md"]).toBeUndefined();
  });
});

describe("scratchNote (F-6 global model)", () => {
  it("toggleRightPane toggles open state globally", () => {
    expect(useViewerStore.getState().rightPaneOpen).toBe(false);
    useViewerStore.getState().toggleRightPane();
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
    useViewerStore.getState().toggleRightPane();
    expect(useViewerStore.getState().rightPaneOpen).toBe(false);
  });

  it("setScratchNoteContent updates the single global content", () => {
    useViewerStore.getState().setScratchNoteContent("# Hello");
    expect(useViewerStore.getState().scratchNoteContent).toBe("# Hello");
  });

  it("setRightPaneRatio updates global ratio", () => {
    useViewerStore.getState().setRightPaneRatio(0.4);
    expect(useViewerStore.getState().rightPaneRatio).toBe(0.4);
  });

  it("loadNotes opens the most-recently-opened note", async () => {
    const { listNotes: listCmd, readNote: readCmd } = await import("@/tauri/commands");
    vi.mocked(listCmd).mockResolvedValue([
      { id: "a", title: "A", pinned: false, lastOpenedAt: 100, preview: "" },
      { id: "b", title: "B", pinned: false, lastOpenedAt: 500, preview: "" },
    ]);
    vi.mocked(readCmd).mockResolvedValue({
      id: "b",
      title: "B",
      pinned: false,
      lastOpenedAt: 500,
      content: "# Loaded B",
    });

    await useViewerStore.getState().loadNotes();
    expect(useViewerStore.getState().activeNoteId).toBe("b");
    expect(useViewerStore.getState().scratchNoteContent).toBe("# Loaded B");
    expect(readCmd).toHaveBeenCalledWith(LOCAL_SCOPE, "b");
    expect(useViewerStore.getState().notes).toHaveLength(2);
  });

  it("loadNotes handles errors gracefully — sets error status", async () => {
    const { listNotes: listCmd } = await import("@/tauri/commands");
    vi.mocked(listCmd).mockRejectedValue(new Error("disk error"));

    await useViewerStore.getState().loadNotes();
    expect(useViewerStore.getState().scratchNoteLoadStatus).toBe("error");
  });

  it("setScratchNoteContent saves to the active note id (debounced writeNote)", async () => {
    const { writeNote: writeCmd } = await import("@/tauri/commands");
    vi.mocked(writeCmd).mockClear();
    useViewerStore.setState({ activeNoteId: "note-x" });

    vi.useFakeTimers();
    useViewerStore.getState().setScratchNoteContent("# edit");
    expect(useViewerStore.getState().scratchNoteContent).toBe("# edit");
    await vi.advanceTimersByTimeAsync(1100);
    vi.useRealTimers();
    expect(writeCmd).toHaveBeenCalledWith(
      LOCAL_SCOPE,
      "note-x",
      "# edit",
      undefined,
      expect.stringMatching(/^note-save:/),
    );
  });

  it("SLW-PT-19: an edit made after an org switch but before the reload saves under the note's OWN org", async () => {
    const {
      listNotes: listCmd,
      readNote: readCmd,
      writeNote: writeCmd,
    } = await import("@/tauri/commands");
    const { useOrgStore } = await import("@/shared/stores/org-store");
    vi.mocked(listCmd).mockResolvedValue([
      { id: "a-note", title: "A", pinned: false, lastOpenedAt: 1, preview: "" },
    ]);
    vi.mocked(readCmd).mockResolvedValue({
      id: "a-note",
      title: "A",
      pinned: false,
      lastOpenedAt: 1,
      content: "org A body",
    });
    useOrgStore.setState({ activeOrgSlug: "acme/a" });
    await useViewerStore.getState().loadNotes();
    vi.mocked(writeCmd).mockClear();

    // The org switches; the panel's reload has not landed yet when the user types.
    useOrgStore.setState({ activeOrgSlug: "acme/b" });
    vi.useFakeTimers();
    useViewerStore.getState().setScratchNoteContent("typed after the switch");
    await vi.advanceTimersByTimeAsync(1100);
    vi.useRealTimers();
    // It also names the body the edit started from (what the load read).
    expect(writeCmd).toHaveBeenCalledWith(
      { githubUserId: 0, orgSlug: "acme/a" },
      "a-note",
      "typed after the switch",
      "org A body",
      expect.stringMatching(/^note-save:/),
    );
    expect(writeCmd).not.toHaveBeenCalledWith(
      expect.objectContaining({ orgSlug: "acme/b" }),
      expect.anything(),
      expect.anything(),
      expect.anything(),
      expect.anything(),
    );
    useOrgStore.setState({ activeOrgSlug: null });
    useViewerStore.setState({ notesLoadedScope: null });
  });

  it("setActiveNote loads the target body and bumps lastOpenedAt", async () => {
    const {
      listNotes: listCmd,
      readNote: readCmd,
      updateNoteMeta: metaCmd,
    } = await import("@/tauri/commands");
    vi.mocked(listCmd).mockResolvedValue([
      { id: "a", title: "A", pinned: false, lastOpenedAt: 100, preview: "" },
      { id: "b", title: "B", pinned: false, lastOpenedAt: 50, preview: "" },
    ]);
    vi.mocked(readCmd).mockImplementation(async (_scope: NotesScope, id: string) => ({
      id,
      title: id.toUpperCase(),
      pinned: false,
      lastOpenedAt: 0,
      content: `body ${id}`,
    }));
    vi.mocked(metaCmd).mockClear();
    await useViewerStore.getState().loadNotes(); // active = "a" (most recent)

    await useViewerStore.getState().setActiveNote("b");
    expect(useViewerStore.getState().activeNoteId).toBe("b");
    expect(useViewerStore.getState().scratchNoteContent).toBe("body b");
    expect(metaCmd).toHaveBeenCalledWith(
      LOCAL_SCOPE,
      "b",
      expect.objectContaining({ lastOpenedAt: expect.any(Number) }),
    );
    const b = useViewerStore.getState().notes.find((n) => n.id === "b");
    expect(b?.lastOpenedAt).toBeGreaterThan(50);
  });

  it("createNote activates the new note and adds it to the list", async () => {
    const { createNote: createCmd } = await import("@/tauri/commands");
    vi.mocked(createCmd).mockResolvedValue({
      id: "new1",
      title: "Untitled",
      pinned: false,
      lastOpenedAt: 999,
      content: "",
    });
    const before = useViewerStore.getState().notes.length;
    const id = await useViewerStore.getState().createNote();
    expect(id).toBe("new1");
    expect(useViewerStore.getState().activeNoteId).toBe("new1");
    expect(useViewerStore.getState().notes.some((n) => n.id === "new1")).toBe(true);
    expect(useViewerStore.getState().notes.length).toBe(before + 1);
  });

  // SW-SN-12 AC-1/AC-2: flushNotes lands the pending note-body edit NOW, before
  // a teardown (quit/blur/unmount) could drop it. Without advancing the 1s
  // debounce, the write must already have fired after flushNotes resolves.
  it("flushNotes flushes the pending note save immediately (SW-SN-12)", async () => {
    const { writeNote: writeCmd } = await import("@/tauri/commands");
    vi.mocked(writeCmd).mockClear();
    useViewerStore.setState({ activeNoteId: "note-q" });

    useViewerStore.getState().setScratchNoteContent("pending edit");
    // Still inside the debounce window — nothing written yet.
    expect(writeCmd).not.toHaveBeenCalled();

    await useViewerStore.getState().flushNotes();
    expect(writeCmd).toHaveBeenCalledWith(
      LOCAL_SCOPE,
      "note-q",
      "pending edit",
      undefined,
      expect.stringMatching(/^note-save:/),
    );
  });

  it("profile switch flush preserves a failed note write and retries its captured body", async () => {
    const { writeNote: writeCmd } = await import("@/tauri/commands");
    useViewerStore.setState({ activeNoteId: "note-switch", stateLoaded: false });
    vi.mocked(writeCmd).mockRejectedValueOnce(new Error("disk full"));
    useViewerStore.getState().setScratchNoteContent("unsaved body");
    await expect(flushViewerForProfileSwitch()).rejects.toThrow("disk full");
    await expect(flushViewerForProfileSwitch()).resolves.toBeUndefined();
    expect(writeCmd).toHaveBeenLastCalledWith(
      LOCAL_SCOPE,
      "note-switch",
      "unsaved body",
      undefined,
      expect.stringMatching(/^note-save:/),
    );
  });

  it("flushNotes is a no-op when no edit is pending (SW-SN-12)", async () => {
    const { writeNote: writeCmd } = await import("@/tauri/commands");
    vi.mocked(writeCmd).mockClear();
    await expect(useViewerStore.getState().flushNotes()).resolves.toBeUndefined();
    expect(writeCmd).not.toHaveBeenCalled();
  });

  // SW-SN-12 AC-3: a keystroke with no active note must not be dropped — the
  // store lazily creates a note and routes the buffered content into it.
  it("setScratchNoteContent with no active note lazily creates a note (SW-SN-12 AC-3)", async () => {
    const { createNote: createCmd, writeNote: writeCmd } = await import("@/tauri/commands");
    vi.mocked(createCmd).mockResolvedValue({
      id: "n2",
      title: "Note",
      pinned: false,
      lastOpenedAt: 0,
      content: "",
    });
    vi.mocked(writeCmd).mockClear();
    useViewerStore.setState({ activeNoteId: null, notes: [] });

    useViewerStore.getState().setScratchNoteContent("buffered");
    // Content is reflected optimistically even before the note exists.
    expect(useViewerStore.getState().scratchNoteContent).toBe("buffered");

    // Lazy create resolves and activates the new note.
    await vi.waitFor(() => expect(useViewerStore.getState().activeNoteId).toBe("n2"));
    await Promise.resolve(); // let the create's .then schedule the debounced save

    // The buffered content is now durable against the new note's id.
    await useViewerStore.getState().flushNotes();
    expect(writeCmd).toHaveBeenCalledWith(
      LOCAL_SCOPE,
      "n2",
      "buffered",
      "",
      expect.stringMatching(/^note-save:/),
    );
  });

  // SW-SN-12 AC-3 (P1): if a switch supersedes the lazy-create mid-flight, the
  // buffered content must still land — written straight to the note created for
  // it (never dropped, never left as an empty orphan) — and the switch target
  // stays active.
  it("persists buffered content to the created note when a switch supersedes it (SW-SN-12 P1)", async () => {
    const {
      createNote: createCmd,
      readNote: readCmd,
      writeNote: writeCmd,
      listNotes: listCmd,
    } = await import("@/tauri/commands");
    vi.mocked(writeCmd).mockClear();
    vi.mocked(listCmd).mockResolvedValue([
      { id: "other", title: "Other", pinned: false, lastOpenedAt: 10, preview: "" },
    ]);
    vi.mocked(readCmd).mockResolvedValue({
      id: "other",
      title: "Other",
      pinned: false,
      lastOpenedAt: 10,
      content: "other body",
    });
    vi.mocked(createCmd).mockResolvedValue({
      id: "n7",
      title: "Note",
      pinned: false,
      lastOpenedAt: 0,
      content: "",
    });
    useViewerStore.setState({ activeNoteId: null, notes: [] });

    // Type with no active note (starts a lazy-create), then immediately switch —
    // the switch bumps the generation so the create can't steal focus.
    useViewerStore.getState().setScratchNoteContent("rescued");
    await useViewerStore.getState().setActiveNote("other");
    await vi.waitFor(() => expect(createCmd).toHaveBeenCalled());

    // Buffered content written directly to the created note; "other" stays active.
    await vi.waitFor(() =>
      expect(writeCmd).toHaveBeenCalledWith(
        LOCAL_SCOPE,
        "n7",
        "rescued",
        undefined,
        expect.stringMatching(/^note-save:/),
      ),
    );
    expect(useViewerStore.getState().activeNoteId).toBe("other");
  });

  // SW-SN-12 (round-2 P3): a teardown flush that lands WHILE a lazy-create is
  // still in flight must await it — the buffered content isn't in the debounce
  // until the create resolves, so flushNotes drains the create chain first.
  it("flushNotes awaits an in-flight lazy-create so a teardown mid-create still persists (SW-SN-12 P3)", async () => {
    const { createNote: createCmd, writeNote: writeCmd } = await import("@/tauri/commands");
    vi.mocked(writeCmd).mockClear();
    // Hold the create pending so we can flush before it resolves.
    let resolveCreate: (v: unknown) => void = () => {};
    vi.mocked(createCmd).mockImplementationOnce(
      () => new Promise((r) => (resolveCreate = r as (v: unknown) => void)) as never,
    );
    useViewerStore.setState({ activeNoteId: null, notes: [] });

    useViewerStore.getState().setScratchNoteContent("midflight");
    // Teardown fires while the create is still pending.
    const flushP = useViewerStore.getState().flushNotes();
    // Wait until createNote actually reaches the (held) command — only then is
    // `resolveCreate` wired up — before releasing it. flushNotes must have
    // awaited the whole chain.
    await vi.waitFor(() => expect(createCmd).toHaveBeenCalled());
    resolveCreate({ id: "nz", title: "Note", pinned: false, lastOpenedAt: 0, content: "" });
    await flushP;
    expect(writeCmd).toHaveBeenCalledWith(
      LOCAL_SCOPE,
      "nz",
      "midflight",
      "",
      expect.stringMatching(/^note-save:/),
    );
  });

  // SW-SN-12 AC-3 (P1): a failed lazy-create must not drop the buffered edit —
  // the in-memory content is retained and a later successful keystroke persists.
  it("retains the buffered edit when the lazy-create fails, then persists on retry (SW-SN-12 P1)", async () => {
    const { createNote: createCmd, writeNote: writeCmd } = await import("@/tauri/commands");
    vi.mocked(writeCmd).mockClear();
    useViewerStore.setState({ activeNoteId: null, notes: [] });

    // First keystroke: create fails → the edit is still on screen (not dropped).
    vi.mocked(createCmd).mockRejectedValueOnce(new Error("disk down"));
    useViewerStore.getState().setScratchNoteContent("first");
    await vi.waitFor(() => expect(createCmd).toHaveBeenCalledTimes(1));
    await new Promise((r) => setTimeout(r, 0)); // let the rejection settle
    expect(useViewerStore.getState().scratchNoteContent).toBe("first");

    // Second keystroke: create succeeds → the latest buffered content persists.
    vi.mocked(createCmd).mockResolvedValue({
      id: "n9",
      title: "Note",
      pinned: false,
      lastOpenedAt: 0,
      content: "",
    });
    useViewerStore.getState().setScratchNoteContent("second");
    await vi.waitFor(() => expect(useViewerStore.getState().activeNoteId).toBe("n9"));
    await Promise.resolve();
    await useViewerStore.getState().flushNotes();
    expect(writeCmd).toHaveBeenCalledWith(
      LOCAL_SCOPE,
      "n9",
      "second",
      "",
      expect.stringMatching(/^note-save:/),
    );
  });

  it("loadNotes preserves the active note across a re-sync (does not force-switch)", async () => {
    const { listNotes: listCmd, readNote: readCmd } = await import("@/tauri/commands");
    // "a" is active but "b" is more recent — a re-sync must keep "a".
    useViewerStore.setState({ activeNoteId: "a" });
    vi.mocked(listCmd).mockResolvedValue([
      { id: "a", title: "A", pinned: false, lastOpenedAt: 100, preview: "" },
      { id: "b", title: "B", pinned: false, lastOpenedAt: 900, preview: "" },
    ]);
    vi.mocked(readCmd).mockImplementation(async (_scope: NotesScope, id: string) => ({
      id,
      title: id.toUpperCase(),
      pinned: false,
      lastOpenedAt: 0,
      content: `body ${id}`,
    }));

    await useViewerStore.getState().loadNotes();
    expect(useViewerStore.getState().activeNoteId).toBe("a");
    expect(useViewerStore.getState().scratchNoteContent).toBe("body a");
  });

  it("a create supersedes an in-flight setActiveNote (no clobber-back)", async () => {
    const {
      listNotes: listCmd,
      readNote: readCmd,
      createNote: createCmd,
    } = await import("@/tauri/commands");
    vi.mocked(listCmd).mockResolvedValue([
      { id: "a", title: "A", pinned: false, lastOpenedAt: 100, preview: "" },
      { id: "b", title: "B", pinned: false, lastOpenedAt: 50, preview: "" },
    ]);
    // readNote("b") hangs until released; other ids resolve immediately.
    let releaseB = () => {};
    vi.mocked(readCmd).mockImplementation((_scope: NotesScope, id: string) => {
      if (id === "b") {
        return new Promise((res) => {
          releaseB = () =>
            res({ id: "b", title: "B", pinned: false, lastOpenedAt: 0, content: "body b" });
        });
      }
      return Promise.resolve({
        id,
        title: id,
        pinned: false,
        lastOpenedAt: 0,
        content: `body ${id}`,
      });
    });
    vi.mocked(createCmd).mockResolvedValue({
      id: "new1",
      title: "Untitled",
      pinned: false,
      lastOpenedAt: 999,
      content: "",
    });

    await useViewerStore.getState().loadNotes(); // active = "a"
    const switchP = useViewerStore.getState().setActiveNote("b"); // parks on readNote("b")
    const id = await useViewerStore.getState().createNote(); // supersedes the switch
    expect(id).toBe("new1");
    expect(useViewerStore.getState().activeNoteId).toBe("new1");

    releaseB(); // stale switch resolves — must NOT clobber back to "b"
    await switchP;
    expect(useViewerStore.getState().activeNoteId).toBe("new1");
  });

  it("a slow loadNotes refresh defers to an explicit switch (never reverts the selection)", async () => {
    const { listNotes: listCmd, readNote: readCmd } = await import("@/tauri/commands");
    useViewerStore.setState({
      activeNoteId: "a",
      notes: [
        { id: "a", title: "A", pinned: false, lastOpenedAt: 100 },
        { id: "b", title: "B", pinned: false, lastOpenedAt: 50 },
      ],
    });
    // The refresh's listNotes hangs until released — simulating a slow refresh
    // that a user switch races.
    let releaseList: (v: unknown) => void = () => {};
    vi.mocked(listCmd).mockImplementation(
      () => new Promise((res) => (releaseList = res as (v: unknown) => void)),
    );
    vi.mocked(readCmd).mockImplementation(async (_scope: NotesScope, id: string) => ({
      id,
      title: id,
      pinned: false,
      lastOpenedAt: 0,
      content: `body ${id}`,
    }));

    const refreshP = useViewerStore.getState().loadNotes();
    // Let the refresh get past its flush and capture the generation.
    await Promise.resolve();
    await Promise.resolve();
    // User explicitly switches — bumps the generation and completes.
    await useViewerStore.getState().setActiveNote("b");
    expect(useViewerStore.getState().activeNoteId).toBe("b");

    // Release the slow refresh; it must DEFER (not revert active to "a").
    releaseList([
      { id: "a", title: "A", pinned: false, lastOpenedAt: 100 },
      { id: "b", title: "B", pinned: false, lastOpenedAt: 50 },
    ]);
    await refreshP;
    expect(useViewerStore.getState().activeNoteId).toBe("b");
  });

  // ===== SW-SN-13: stop minting empty duplicate "Scratch" notes =====

  // AC-1: a null-active-note transaction that serializes to empty/whitespace must
  // NOT create a note — that empty spawn is the whole bug.
  it("setScratchNoteContent with no active note and EMPTY content creates NO note (SW-SN-13 AC-1)", async () => {
    const { createNote: createCmd } = await import("@/tauri/commands");
    vi.mocked(createCmd).mockClear();
    useViewerStore.setState({ activeNoteId: null, notes: [] });

    useViewerStore.getState().setScratchNoteContent("   \n\t  "); // whitespace-only
    // Content is still reflected optimistically, but nothing is created.
    expect(useViewerStore.getState().scratchNoteContent).toBe("   \n\t  ");
    await new Promise((r) => setTimeout(r, 0));
    expect(createCmd).not.toHaveBeenCalled();
  });

  // AC-2: SW-SN-12's "never drop a real keystroke" survives — a NON-empty null-id
  // write still lands. (AC-1 narrows the trigger; it does not remove the fallback.)
  it("setScratchNoteContent with no active note and NON-empty content still lands (SW-SN-13 AC-2)", async () => {
    const { createNote: createCmd, writeNote: writeCmd } = await import("@/tauri/commands");
    vi.mocked(createCmd).mockResolvedValue({
      id: "real",
      title: "Note",
      pinned: false,
      lastOpenedAt: 0,
      content: "",
    });
    vi.mocked(writeCmd).mockClear();
    useViewerStore.setState({ activeNoteId: null, notes: [] });

    useViewerStore.getState().setScratchNoteContent("meaningful text");
    await vi.waitFor(() => expect(useViewerStore.getState().activeNoteId).toBe("real"));
    await Promise.resolve();
    await useViewerStore.getState().flushNotes();
    expect(writeCmd).toHaveBeenCalledWith(
      LOCAL_SCOPE,
      "real",
      "meaningful text",
      "",
      expect.stringMatching(/^note-save:/),
    );
  });

  // AC-3 (suspenders): loadNotes must never settle a null active id under a
  // "loaded" status — it deterministically creates one canonical note instead.
  it("loadNotes never leaves a null active note under 'loaded' — creates a canonical note (SW-SN-13 AC-3)", async () => {
    const { listNotes: listCmd, createNote: createCmd } = await import("@/tauri/commands");
    vi.mocked(listCmd).mockResolvedValue([]); // degenerate empty (migration normally prevents)
    vi.mocked(createCmd).mockResolvedValue({
      id: "seed",
      title: "Note",
      pinned: false,
      lastOpenedAt: 0,
      content: "",
    });
    useViewerStore.setState({ activeNoteId: null, notes: [], scratchNoteLoadStatus: "pending" });

    await useViewerStore.getState().loadNotes();

    expect(createCmd).toHaveBeenCalledWith(LOCAL_SCOPE, "Note", {});
    expect(useViewerStore.getState().activeNoteId).toBe("seed");
    expect(useViewerStore.getState().scratchNoteLoadStatus).toBe("loaded");
  });

  // AC-4: the empty-duplicate reap runs ONCE per session on load, uses the
  // reaper's survivor list, and does NOT re-run on a subsequent (hot-reload) load.
  it("loadNotes reaps empty duplicates once per session and uses the survivor list (SW-SN-13 AC-4)", async () => {
    const {
      listNotes: listCmd,
      readNote: readCmd,
      reapEmptyScratchNotes: reapCmd,
    } = await import("@/tauri/commands");
    vi.mocked(listCmd).mockResolvedValue([
      { id: "keep", title: "Note", pinned: false, lastOpenedAt: 5, preview: "" },
      { id: "dupe", title: "Note", pinned: false, lastOpenedAt: 3, preview: "" },
    ]);
    // The reaper drops the empty dupe and returns only the survivor.
    vi.mocked(reapCmd).mockResolvedValue([
      { id: "keep", title: "Note", pinned: false, lastOpenedAt: 5 },
    ]);
    vi.mocked(readCmd).mockResolvedValue({
      id: "keep",
      title: "Note",
      pinned: false,
      lastOpenedAt: 5,
      content: "body",
    });
    useViewerStore.setState({
      activeNoteId: null,
      notes: [],
      scratchNoteLoadStatus: "pending",
      emptyScratchReapDone: false,
    });

    await useViewerStore.getState().loadNotes();
    expect(reapCmd).toHaveBeenCalledTimes(1);
    // Called with the active id + the BOOT SNAPSHOT of note ids (the reap-eligible
    // set), so a concurrently-created note is never reaped.
    expect(reapCmd).toHaveBeenCalledWith(LOCAL_SCOPE, undefined, ["keep", "dupe"]);
    expect(useViewerStore.getState().notes.map((n) => n.id)).toEqual(["keep"]);
    expect(useViewerStore.getState().activeNoteId).toBe("keep");

    // A second load (e.g. SN-7 hot-reload) must NOT reap again — it would police a
    // note the user just emptied.
    await useViewerStore.getState().loadNotes();
    expect(reapCmd).toHaveBeenCalledTimes(1);

    // Reset the reaper override so it can't leak into later suites (default is a
    // no-op: undefined → loadNotes keeps its listNotes result).
    vi.mocked(reapCmd).mockResolvedValue(undefined as never);
  });

  // AC-4 (review r1): a transient reap failure must NOT burn the one-shot — the
  // flag resets so the next load retries, and the failure never breaks the load.
  it("loadNotes retries the reap on a later load if it failed transiently (SW-SN-13 r1)", async () => {
    const {
      listNotes: listCmd,
      readNote: readCmd,
      reapEmptyScratchNotes: reapCmd,
    } = await import("@/tauri/commands");
    vi.mocked(listCmd).mockResolvedValue([
      { id: "keep", title: "Note", pinned: false, lastOpenedAt: 5, preview: "" },
    ]);
    vi.mocked(readCmd).mockResolvedValue({
      id: "keep",
      title: "Note",
      pinned: false,
      lastOpenedAt: 5,
      content: "body",
    });

    // First load: reap throws → caught, load still succeeds, flag reset.
    vi.mocked(reapCmd).mockRejectedValueOnce(new Error("ipc stalled"));
    useViewerStore.setState({ activeNoteId: null, notes: [], emptyScratchReapDone: false });
    await useViewerStore.getState().loadNotes();
    expect(useViewerStore.getState().activeNoteId).toBe("keep"); // load not broken
    expect(useViewerStore.getState().emptyScratchReapDone).toBe(false); // one-shot NOT burned

    // Second load: reap is retried (not skipped).
    vi.mocked(reapCmd).mockResolvedValue([
      { id: "keep", title: "Note", pinned: false, lastOpenedAt: 5 },
    ]);
    await useViewerStore.getState().loadNotes();
    expect(reapCmd).toHaveBeenCalledTimes(2);
    expect(useViewerStore.getState().emptyScratchReapDone).toBe(true);

    vi.mocked(reapCmd).mockResolvedValue(undefined as never);
  });
});
