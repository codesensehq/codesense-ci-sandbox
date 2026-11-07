// SPDX-License-Identifier: MIT
import { useOrgStore } from "@/shared/stores/org-store";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, act, within } from "@/test-utils";
import { ScratchNotePanel } from "./scratch-note-panel";
import {
  handleScratchNoteTab,
  handleScratchNoteShiftTab,
} from "@/features/notes/editor/note-editor-extensions";
import { sendTextToTerminal } from "@/shared/lib/send-to-terminal";
import { OwningScopeProvider, warmScopes } from "@/shared/lib/warm-scopes";
import { clearHeadingFlash } from "../lib/note-headings";
import { makeHeadingEl, makeScrollContainer } from "../lib/note-headings.test-utils";

// Mock Tiptap editor
const mockChain = {
  focus: vi.fn(() => mockChain),
  toggleHeading: vi.fn(() => mockChain),
  toggleBold: vi.fn(() => mockChain),
  toggleItalic: vi.fn(() => mockChain),
  toggleCode: vi.fn(() => mockChain),
  toggleCodeBlock: vi.fn(() => mockChain),
  setParagraph: vi.fn(() => mockChain),
  setContent: vi.fn(() => mockChain),
  run: vi.fn(),
};

// `on` captures editor event handlers so tests can drive `selectionUpdate`.
const editorListeners: Record<string, (() => void)[]> = {};

const mockEditor = {
  chain: vi.fn(() => mockChain),
  isActive: vi.fn(() => false),
  isFocused: false,
  commands: { setContent: vi.fn() },
  // SW-SN-12 AC-3: the load-status gate calls setEditable().
  setEditable: vi.fn(),
  on: vi.fn((event: string, cb: () => void) => {
    (editorListeners[event] ??= []).push(cb);
  }),
  off: vi.fn(),
  getHTML: () => "",
  storage: { markdown: { getMarkdown: () => "", serializer: { serialize: () => "" } } },
  // SW-SN-5: `doc.descendants` is walked by useNoteHeadings; a no-op walk yields
  // an empty heading list (these tests don't exercise Outline mode).
  // SW-SN-12: `textContent`/`forEach` let `serializeEditorMarkdown` (AC-4 blur
  // commit) run without a real PM doc — the empty-doc fast path returns "".
  state: {
    selection: { from: 1, to: 6 },
    doc: { textBetween: () => "hello", descendants: vi.fn(), textContent: "", forEach: vi.fn() },
  },
  view: { nodeDOM: vi.fn(() => null), dom: document.createElement("div") },
  schema: {},
};

// SW-SN-12: capture the window focus-change handler + unlisten so tests can
// drive app-background blur and assert cleanup. `mock`-prefixed so vitest's
// hoisted mock factory may reference it.
const mockFocus = {
  handler: null as ((e: { payload: boolean }) => void) | null,
  unlisten: vi.fn(),
};

function fireSelectionUpdate() {
  act(() => {
    for (const cb of editorListeners.selectionUpdate ?? []) cb();
  });
}

vi.mock("@/shared/lib/send-to-terminal", () => ({
  sendTextToTerminal: vi.fn(),
  sendTextToSession: vi.fn(() => Promise.resolve()),
}));

// The roster read is the agents list's own seam (covered there); the panel test
// only needs "no agents running".
vi.mock("@/features/viewer/hooks/use-team-agent-identities", () => ({
  useTeamAgentIdentities: () => [],
}));

const mockLaunch = vi.hoisted(() => ({ openLaunchSheet: vi.fn(), openRoutineDraft: vi.fn() }));
vi.mock("@/features/agents/lib/launch-sheet", () => ({
  openLaunchSheet: mockLaunch.openLaunchSheet,
}));
vi.mock("@/features/routines/routines-virtual-tab", () => ({
  openRoutineDraft: mockLaunch.openRoutineDraft,
}));
vi.mock("@/features/agents/agents-virtual-tab", () => ({ openAgents: vi.fn() }));
vi.mock("@/features/agents/lib/agent-names", () => ({
  useAgentRows: () => ({
    rows: [{ key: "me/tidy", name: "Notes tidy", scope: "personal", archived: false }],
    error: null,
  }),
}));

vi.mock("@tiptap/react", () => ({
  useEditor: () => mockEditor,
  EditorContent: ({ editor: _editor }: { editor: unknown }) => <div data-testid="editor-content" />,
  ReactNodeViewRenderer: vi.fn(),
}));

vi.mock("@tiptap/core", () => ({
  Extension: { create: vi.fn(() => ({})) },
  Node: { create: vi.fn(() => ({ extend: vi.fn(() => ({})) })) },
  mergeAttributes: vi.fn(() => ({})),
}));

vi.mock("@tiptap/extension-task-list", () => ({ default: {} }));
vi.mock("@tiptap/extension-task-item", () => ({ default: { configure: vi.fn(() => ({})) } }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));

vi.mock("@tiptap/starter-kit", () => ({
  default: { configure: vi.fn(() => ({})) },
}));

vi.mock("@tiptap/extension-placeholder", () => ({
  default: { configure: vi.fn(() => ({})) },
}));

vi.mock("@tiptap/extension-code-block-lowlight", () => ({
  default: {
    extend: vi.fn(() => ({
      configure: vi.fn(() => ({})),
    })),
  },
}));

vi.mock("tiptap-markdown", () => ({
  Markdown: { configure: vi.fn(() => ({})) },
}));

vi.mock("@tiptap/extension-typography", () => ({
  default: {},
}));

vi.mock("lowlight", () => ({
  all: {},
  createLowlight: vi.fn(() => ({ register: vi.fn() })),
}));

// SW-SN-9: the panel resolves the active workspace + warm pipeline tree for link
// activation. Stub them here so the panel test stays focused (the resolver/
// picker logic is covered by the note-links / cs-link / picker-items lib tests).
vi.mock("@/shared/hooks/use-active-workspace-context", () => ({
  useActiveWorkspaceContext: () => ({
    teamKey: "t",
    repoPath: "/test-repo",
    workspace: null,
    sourceRepoSlug: null,
  }),
}));
vi.mock("@/shared/lib/use-resolved-repo", () => ({
  useResolvedRepo: () => ({ teamContentPath: "/content" }),
}));
vi.mock("@/shared/lib/pipeline-graph/use-pipeline-tree", () => ({
  usePipelineTree: () => ({ tree: null, loading: false, error: null, refetch: vi.fn() }),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    onFocusChanged: (cb: (e: { payload: boolean }) => void) => {
      mockFocus.handler = cb;
      return Promise.resolve(mockFocus.unlisten);
    },
  }),
}));

// F-6 AC-10: store snapshot is scalar-shaped. `selectedRepoPath` is now
// read by `sendTextToTerminal` (was a function argument).
const mockSelectedRepoPath: string | null = "/test-repo";

// SW-SN-2: stable action fns + a mutable state object so tests can drive notes /
// active-note / rail state and assert on the action calls (⌘B, delete, etc.).
const mockActions = {
  setScratchNoteContent: vi.fn(),
  loadNotes: vi.fn(),
  setActiveNote: vi.fn(),
  createNote: vi.fn(() => Promise.resolve("id")),
  renameNote: vi.fn(),
  deleteNote: vi.fn(),
  setNotePinned: vi.fn(),
  // SW-SN-12 AC-1/AC-2: teardown flush.
  flushNotes: vi.fn(() => Promise.resolve()),
  // SW-SN-6: heading read-model publish + jump signal.
  setActiveNoteHeadings: vi.fn(),
  requestHeadingJump: vi.fn(),
  clearHeadingJump: vi.fn(),
};
const mockStoreState: Record<string, unknown> = {
  scratchNoteContent: "",
  notes: [] as unknown[],
  activeNoteId: null as string | null,
  noteSavedAt: null as number | null,
  notesLoadedScope: null,
  // SW-SN-12 AC-3: default to a resolved model so the editor is editable.
  scratchNoteLoadStatus: "loaded",
  // SW-SN-6: heading read-model + jump signal (ephemeral view state).
  activeNoteHeadings: [] as unknown[],
  pendingHeadingJump: null as unknown,
  selectedRepoPath: mockSelectedRepoPath,
  ...mockActions,
};

vi.mock("@/shared/stores/viewer-store", () => {
  const store = (selector?: (s: Record<string, unknown>) => unknown) => {
    if (selector) return selector(mockStoreState);
    return {};
  };
  store.subscribe = () => () => {};
  store.getState = () => ({ terminalTabs: {}, ...mockStoreState });
  return { useViewerStore: store };
});

vi.mock("@/tauri/commands", () => ({
  writePty: vi.fn(() => Promise.resolve()),
  notesDirPath: vi.fn(() => Promise.resolve("/notes")),
  // An install whose backend has no note history: the history UI stays hidden.
  notesHistory: vi.fn(() => Promise.reject(new Error("not mocked: notes_history"))),
  notesReadAt: vi.fn(),
  notesRestore: vi.fn(),
  readNote: vi.fn(() => Promise.resolve({ content: "" })),
  listScratchSpace: vi.fn(() => Promise.resolve({ allOrgs: [] })),
  readFile: vi.fn(),
}));

// SLW-PT-7: drive the resolved GitHub id so we can assert the notes-load effect
// re-fires across an auth transition. Defaults to logged-out (null → uid 0), which
// matches the real hook's behavior in the test env, so other suites are unaffected.
const mockUser = vi.hoisted(() => ({ id: null as number | null }));
vi.mock("@/shared/lib/get-current-user", () => ({
  currentGithubUserId: () => mockUser.id ?? 0,
  useCurrentUser: () =>
    mockUser.id == null
      ? null
      : { github_user_id: mockUser.id, github_username: "u", display_name: "U", avatar_url: null },
}));

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn() }),
}));

vi.mock("./code-block-node-view", () => ({
  CodeBlockNodeView: vi.fn(),
}));

vi.mock("../hooks/use-active-workspace-terminal-tabs", () => ({
  getActiveWorkspaceTabs: vi.fn(() => []),
  getActiveWorkspaceActiveTab: vi.fn(() => undefined),
}));

beforeEach(() => {
  vi.clearAllMocks();
  mockEditor.isActive.mockReturnValue(false);
  mockEditor.isFocused = false;
  for (const key of Object.keys(editorListeners)) delete editorListeners[key];
  // happy-dom has no layout, so stub a real on-screen selection rect for the
  // popover's anchor (the panel suppresses the popover on a zero-size rect).
  vi.spyOn(window, "getSelection").mockReturnValue({
    rangeCount: 1,
    getRangeAt: () => ({
      getBoundingClientRect: () => ({ top: 40, left: 20, width: 30, height: 16 }),
    }),
  } as unknown as Selection);
  // SW-SN-2: reset the mutable store snapshot between tests.
  mockStoreState.notes = [];
  mockStoreState.activeNoteId = null;
  mockStoreState.scratchNoteContent = "";
  // SW-SN-12: reset load-status + captured focus handler.
  mockStoreState.scratchNoteLoadStatus = "loaded";
  mockFocus.handler = null;
  // SW-SN-6: reset heading read-model + jump signal.
  mockStoreState.activeNoteHeadings = [];
  mockStoreState.pendingHeadingJump = null;
});

const NOTE = { id: "n1", title: "My Note", pinned: false, lastOpenedAt: 1 };
const OTHER = { id: "n2", title: "Groceries", pinned: true, lastOpenedAt: 2 };

/** Report a pane width to the panel's ResizeObserver. */
function withPaneWidth(width: number) {
  class RO {
    constructor(private cb: ResizeObserverCallback) {}
    observe() {
      this.cb([{ contentRect: { width } } as ResizeObserverEntry], this as never);
    }
    disconnect() {}
    unobserve() {}
  }
  vi.stubGlobal("ResizeObserver", RO);
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("ScratchNotePanel — one quiet sheet", () => {
  it("has one chrome row (list, Find a note, Agent, note actions) and no old rail or header", () => {
    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    expect(screen.getByRole("button", { name: "Notes list" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Find a note" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Agent/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Note actions" })).toBeInTheDocument();
    // The retired rail and its Notes⇄Outline tabs are gone.
    expect(screen.queryByRole("tab", { name: "Outline" })).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Expand notes rail")).not.toBeInTheDocument();
    expect(screen.queryByText("Scratch Notes")).not.toBeInTheDocument();
    expect(screen.getByTestId("editor-content")).toBeInTheDocument();
  });

  it("the title lives in the sheet, with Private · org in the meta line", () => {
    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    expect(screen.getByRole("textbox", { name: "Note title" })).toHaveValue("My Note");
    expect(screen.getByText("Private")).toBeInTheDocument();
  });

  it("renames the note when its title is edited and Enter pressed", () => {
    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    const title = screen.getByRole("textbox", { name: "Note title" });
    fireEvent.change(title, { target: { value: "Plans" } });
    fireEvent.keyDown(title, { key: "Enter" });
    expect(mockActions.renameNote).toHaveBeenCalledWith("n1", "Plans");
  });

  it("shows 'Saved … ago' once a save lands", () => {
    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    mockStoreState.noteSavedAt = Date.now() - 2 * 60_000;
    render(<ScratchNotePanel />);
    expect(screen.getByText("Saved 2m ago")).toBeInTheDocument();
    mockStoreState.noteSavedAt = null;
  });
});

describe("ScratchNotePanel — layout modes", () => {
  it("narrow: the list is a drawer over the note, toggled by the list button", () => {
    withPaneWidth(360);
    mockStoreState.notes = [NOTE, OTHER];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    expect(screen.queryByRole("navigation", { name: "Notes" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Notes list" }));
    const list = screen.getByRole("navigation", { name: "Notes" });
    expect(within(list).getByText("Pinned")).toBeInTheDocument();
    expect(within(list).getByText("Recent")).toBeInTheDocument();
    // Picking a note opens it and closes the drawer.
    fireEvent.click(within(list).getByText("Groceries"));
    expect(mockActions.setActiveNote).toHaveBeenCalledWith("n2");
    expect(screen.queryByRole("navigation", { name: "Notes" })).not.toBeInTheDocument();
  });

  it("wide: the list is a column beside the note and the chrome drops its list toggle", () => {
    withPaneWidth(1000);
    mockStoreState.notes = [NOTE, OTHER];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    expect(screen.getByRole("navigation", { name: "Notes" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Notes list" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Find a note" })).not.toBeInTheDocument();
  });

  it("⌘B toggles the drawer and stops propagation so the global nav toggle never fires", () => {
    withPaneWidth(360);
    const docHandler = vi.fn();
    document.addEventListener("keydown", docHandler);
    render(<ScratchNotePanel />);
    fireEvent.keyDown(screen.getByTestId("editor-content"), { key: "b", metaKey: true });
    expect(screen.getByRole("navigation", { name: "Notes" })).toBeInTheDocument();
    expect(docHandler).not.toHaveBeenCalled();
    document.removeEventListener("keydown", docHandler);
  });

  it("⌘B while the editor is focused stays Bold (no drawer) but still blocks the global nav", () => {
    mockEditor.isFocused = true;
    const docHandler = vi.fn();
    document.addEventListener("keydown", docHandler);
    render(<ScratchNotePanel />);
    fireEvent.keyDown(screen.getByTestId("editor-content"), { key: "b", metaKey: true });
    expect(screen.queryByRole("navigation", { name: "Notes" })).not.toBeInTheDocument();
    expect(docHandler).not.toHaveBeenCalled();
    document.removeEventListener("keydown", docHandler);
  });

  it("never flips into an outline on its own — the outline is a button with headings", () => {
    mockEditor.state.doc.descendants = ((cb: (node: unknown, pos: number) => void) => {
      for (let i = 0; i < 4; i++) {
        cb({ type: { name: "heading" }, attrs: { level: 2 }, textContent: `H${i}` }, i * 5);
      }
    }) as unknown as typeof mockEditor.state.doc.descendants;
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    expect(screen.queryByRole("tree", { name: "Outline" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "On this note" }));
    expect(screen.getByRole("tree", { name: "Outline" })).toBeInTheDocument();
    mockEditor.state.doc.descendants = vi.fn();
  });
});

describe("ScratchNotePanel — selection menu", () => {
  it("shows Bold/Italic/Code + Send to… on a selection; Send to… opens the picker", () => {
    render(<ScratchNotePanel />);
    fireSelectionUpdate();
    expect(screen.getByLabelText("Bold")).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText("Send to…"));
    expect(screen.getByRole("textbox", { name: "Send to" })).toBeInTheDocument();
    expect(
      screen.getByText("No agents are running. Start one with this line."),
    ).toBeInTheDocument();
    // Paste into the focused terminal sends the selected text.
    fireEvent.click(screen.getByText("Paste into focused terminal"));
    expect(sendTextToTerminal).toHaveBeenCalledWith("hello");
  });

  it("hides the menu when the selection collapses", () => {
    render(<ScratchNotePanel />);
    fireSelectionUpdate();
    expect(screen.getByLabelText("Send to…")).toBeInTheDocument();
    mockEditor.state.selection = { from: 3, to: 3 };
    fireSelectionUpdate();
    expect(screen.queryByLabelText("Send to…")).not.toBeInTheDocument();
    mockEditor.state.selection = { from: 1, to: 6 };
  });

  it("Start a new agent opens the normal launch sheet with the text (and the note as context)", () => {
    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    fireSelectionUpdate();
    fireEvent.click(screen.getByLabelText("Send to…"));
    fireEvent.click(screen.getByText("Start a new agent with this"));
    expect(mockLaunch.openLaunchSheet).toHaveBeenCalledWith({
      teamKey: "t",
      task: expect.stringContaining("hello"),
    });
    expect(mockLaunch.openLaunchSheet.mock.calls[0][0].task).toContain("My Note");
  });
});

describe("ScratchNotePanel — Agent menu", () => {
  function openAgentMenu() {
    fireEvent.click(screen.getByRole("button", { name: /Agent/ }));
  }

  it("lists the agent actions; Changes by agents is hidden without note history", async () => {
    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    await act(async () => {});
    openAgentMenu();
    expect(screen.getByText("Organize with an agent…")).toBeInTheDocument();
    expect(screen.getByText("Send to a running agent…")).toBeInTheDocument();
    expect(screen.getByText("Set up a routine…")).toBeInTheDocument();
    expect(screen.queryByText("Changes by agents")).not.toBeInTheDocument();
  });

  it("Organize launches a normal agent with an editable prompt naming the note", async () => {
    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    await act(async () => {});
    openAgentMenu();
    await act(async () => {
      fireEvent.click(screen.getByText("Organize with an agent…"));
    });
    expect(mockLaunch.openLaunchSheet).toHaveBeenCalledTimes(1);
    const { teamKey, task } = mockLaunch.openLaunchSheet.mock.calls[0][0];
    expect(teamKey).toBe("t");
    expect(task).toContain("“My Note” (/notes/n1.md)");
    expect(task).toMatch(/inbox/i);
  });

  it("names a note in an org for the notes tools, not by its file", async () => {
    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    mockStoreState.notesLoadedScope = { githubUserId: 0, orgSlug: "acme/ctx" };
    try {
      render(<ScratchNotePanel />);
      await act(async () => {});
      openAgentMenu();
      await act(async () => {
        fireEvent.click(screen.getByText("Organize with an agent…"));
      });
      const { task } = mockLaunch.openLaunchSheet.mock.calls[0][0];
      expect(task).toContain("“My Note” (note id n1 in org acme/ctx)");
      expect(task).toContain("notes_write");
      expect(task).not.toContain("/notes/n1.md");
    } finally {
      mockStoreState.notesLoadedScope = null;
    }
  });

  it("Send to a running agent hands over the whole note, named once", async () => {
    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    await act(async () => {});
    openAgentMenu();
    fireEvent.click(screen.getByText("Send to a running agent…"));
    fireEvent.click(screen.getByText("Start a new agent with this"));
    const { task } = mockLaunch.openLaunchSheet.mock.calls[0][0];
    expect(task).toBe("Read my note “My Note” (/notes/n1.md).");
  });

  it("Set up a routine opens the routine editor prefilled for the person only", async () => {
    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    await act(async () => {});
    openAgentMenu();
    fireEvent.click(screen.getByText("Set up a routine…"));
    const sheet = screen.getByRole("dialog", { name: "Set up a routine" });
    await act(async () => {});
    fireEvent.click(within(sheet).getByRole("button", { name: "Create routine…" }));
    expect(mockLaunch.openRoutineDraft).toHaveBeenCalledWith({
      draft: expect.objectContaining({
        agent: "me/tidy",
        when: "weekday 18:00",
        task: expect.stringContaining("My Note"),
      }),
      audience: "personal",
    });
  });
});

describe("ScratchNotePanel — note actions", () => {
  it("… → Delete confirms, and only then calls deleteNote", () => {
    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    fireEvent.click(screen.getByRole("button", { name: "Note actions" }));
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByText(/Delete .My Note.?/)).toBeInTheDocument();
    expect(mockActions.deleteNote).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    expect(mockActions.deleteNote).toHaveBeenCalledWith("n1");
  });

  it("… → New note calls createNote; so does ⌘N in the pane", () => {
    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    fireEvent.click(screen.getByRole("button", { name: "Note actions" }));
    fireEvent.click(screen.getByRole("button", { name: /New note/ }));
    expect(mockActions.createNote).toHaveBeenCalledTimes(1);
    fireEvent.keyDown(screen.getByTestId("editor-content"), { key: "n", metaKey: true });
    expect(mockActions.createNote).toHaveBeenCalledTimes(2);
  });

  it("a blank first note shows the empty state; New note there opens the sheet", () => {
    mockStoreState.notes = [{ ...NOTE, title: "Note" }];
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    expect(screen.getByRole("heading", { name: /Notes for/ })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /New note/ }));
    expect(screen.queryByRole("heading", { name: /Notes for/ })).not.toBeInTheDocument();
    // No second note is minted for the empty first one.
    expect(mockActions.createNote).not.toHaveBeenCalled();
  });

  it("the empty state's New note takes focus again when its warm team is shown again", () => {
    mockStoreState.notes = [{ ...NOTE, title: "Note" }];
    mockStoreState.activeNoteId = "n1";
    act(() => warmScopes.activate("notes:a"));
    render(
      <OwningScopeProvider scope="notes:a">
        <ScratchNotePanel />
      </OwningScopeProvider>,
    );
    const button = screen.getByRole("button", { name: /New note/ });
    expect(button).toHaveFocus();
    button.blur();
    act(() => warmScopes.activate("notes:b"));
    expect(button).not.toHaveFocus();
    act(() => warmScopes.activate("notes:a"));
    expect(button).toHaveFocus();
    for (const k of ["notes:a", "notes:b"]) warmScopes.forget(k);
  });
});

// -- Tab / Shift-Tab handler tests ------------------------------------------

function createTabEditor(activeNodes: string[]) {
  const tabChain = {
    focus: vi.fn((): typeof tabChain => tabChain),
    sinkListItem: vi.fn((): typeof tabChain => tabChain),
    liftListItem: vi.fn((): typeof tabChain => tabChain),
    insertContent: vi.fn((): typeof tabChain => tabChain),
    run: vi.fn(() => true),
  };
  const mockTr = { insertText: vi.fn(() => mockTr) };
  const mockView = { dispatch: vi.fn() };
  return {
    isActive: (name: string) => activeNodes.includes(name),
    chain: () => tabChain,
    state: { selection: { from: 0, to: 0 }, tr: mockTr },
    view: mockView,
    _chain: tabChain,
    _tr: mockTr,
    _view: mockView,
  };
}

describe("handleScratchNoteTab", () => {
  it("defers to built-in ListItem handler when cursor is in a list", () => {
    const editor = createTabEditor(["listItem"]);
    expect(handleScratchNoteTab(editor)).toBe(false);
    expect(editor._chain.focus).not.toHaveBeenCalled();
  });

  it("inserts two spaces when cursor is in a code block", () => {
    const editor = createTabEditor(["codeBlock"]);
    expect(handleScratchNoteTab(editor)).toBe(true);
    expect(editor._tr.insertText).toHaveBeenCalledWith("  ", 0, 0);
    expect(editor._view.dispatch).toHaveBeenCalled();
  });

  it("prefers code block over list item when both are active", () => {
    const editor = createTabEditor(["codeBlock", "listItem"]);
    expect(handleScratchNoteTab(editor)).toBe(true);
    expect(editor._tr.insertText).toHaveBeenCalledWith("  ", 0, 0);
  });

  it("inserts a tab character via ProseMirror transaction in prose", () => {
    const editor = createTabEditor([]);
    expect(handleScratchNoteTab(editor)).toBe(true);
    expect(editor._tr.insertText).toHaveBeenCalledWith("\t", 0, 0);
    expect(editor._view.dispatch).toHaveBeenCalled();
    expect(editor._chain.insertContent).not.toHaveBeenCalled();
  });
});

describe("handleScratchNoteShiftTab", () => {
  it("defers to built-in ListItem handler when cursor is in a list", () => {
    const editor = createTabEditor(["listItem"]);
    expect(handleScratchNoteShiftTab(editor)).toBe(false);
    expect(editor._chain.focus).not.toHaveBeenCalled();
  });

  it("returns true (no-op) when cursor is in a code block", () => {
    const editor = createTabEditor(["codeBlock"]);
    expect(handleScratchNoteShiftTab(editor)).toBe(true);
    expect(editor._chain.focus).not.toHaveBeenCalled();
  });

  it("prefers code block no-op over list lift when both are active", () => {
    const editor = createTabEditor(["codeBlock", "listItem"]);
    expect(handleScratchNoteShiftTab(editor)).toBe(true);
  });

  it("returns true in prose without calling any commands", () => {
    const editor = createTabEditor([]);
    expect(handleScratchNoteShiftTab(editor)).toBe(true);
    expect(editor._chain.focus).not.toHaveBeenCalled();
  });
});

// SW-SN-12: the teardown-durability seams. These are the regression guards
// AC-5 requires — flush-on-unmount + flush-on-blur must actually fire, and the
// cold-boot input gate must hold the editor read-only until the model resolves.
describe("ScratchNotePanel - SW-SN-12 teardown durability", () => {
  it("AC-2: flushes the pending note save on unmount", () => {
    const { unmount } = render(<ScratchNotePanel />);
    expect(mockActions.flushNotes).not.toHaveBeenCalled();
    unmount();
    expect(mockActions.flushNotes).toHaveBeenCalled();
  });

  it("AC-2: flushes on window blur (app backgrounding), not on focus gain", async () => {
    render(<ScratchNotePanel />);
    // The focus listener registers asynchronously (onFocusChanged returns a promise).
    await act(async () => {
      await Promise.resolve();
    });
    expect(mockFocus.handler).toBeTypeOf("function");

    mockActions.flushNotes.mockClear();
    act(() => mockFocus.handler?.({ payload: true })); // regained focus → no flush
    expect(mockActions.flushNotes).not.toHaveBeenCalled();

    act(() => mockFocus.handler?.({ payload: false })); // lost focus → flush
    expect(mockActions.flushNotes).toHaveBeenCalledTimes(1);
  });

  it("AC-2: unlistens the window focus handler on unmount", async () => {
    const { unmount } = render(<ScratchNotePanel />);
    await act(async () => {
      await Promise.resolve();
    });
    unmount();
    expect(mockFocus.unlisten).toHaveBeenCalled();
  });

  it("AC-3: holds the editor read-only until the notes model resolves", () => {
    mockStoreState.scratchNoteLoadStatus = "pending";
    render(<ScratchNotePanel />);
    expect(mockEditor.setEditable).toHaveBeenCalledWith(false);
  });

  it("AC-3: enables the editor once the notes model is loaded AND an active note exists", () => {
    // SW-SN-13: the gate now also requires a non-null active note.
    mockStoreState.scratchNoteLoadStatus = "loaded";
    mockStoreState.activeNoteId = "n1";
    render(<ScratchNotePanel />);
    expect(mockEditor.setEditable).toHaveBeenCalledWith(true);
  });

  // SW-SN-13 AC-3: "loaded" with NO active note must NOT enable the editor — that
  // null-active-editable window is what let a stray transaction mint an empty
  // "Scratch" note. (SW-SN-12's gate enabled on any non-"pending" status.)
  it("SW-SN-13 AC-3: stays read-only when loaded but no active note exists", () => {
    mockStoreState.scratchNoteLoadStatus = "loaded";
    mockStoreState.activeNoteId = null;
    render(<ScratchNotePanel />);
    expect(mockEditor.setEditable).toHaveBeenCalledWith(false);
    expect(mockEditor.setEditable).not.toHaveBeenCalledWith(true);
  });

  // SW-SN-13 AC-3: an "error" load with no active note also stays read-only (only
  // the 3s grace hatch may re-enable it — proven separately below).
  it("SW-SN-13 AC-3: stays read-only on load error with no active note", () => {
    mockStoreState.scratchNoteLoadStatus = "error";
    mockStoreState.activeNoteId = null;
    render(<ScratchNotePanel />);
    expect(mockEditor.setEditable).toHaveBeenCalledWith(false);
    expect(mockEditor.setEditable).not.toHaveBeenCalledWith(true);
  });

  it("AC-3 (P2): enables the editor after a grace period if the model stays pending", () => {
    vi.useFakeTimers();
    try {
      mockStoreState.scratchNoteLoadStatus = "pending";
      render(<ScratchNotePanel />);
      expect(mockEditor.setEditable).toHaveBeenCalledWith(false);
      mockEditor.setEditable.mockClear();
      // A stalled load must not trap the pad read-only forever.
      act(() => {
        vi.advanceTimersByTime(3000);
      });
      expect(mockEditor.setEditable).toHaveBeenCalledWith(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("AC-4: registers an editor blur handler that commits without throwing", () => {
    render(<ScratchNotePanel />);
    // The panel wires a blur listener (alongside the selection-menu dismiss) to
    // commit pending content before a switch.
    expect(editorListeners.blur?.length).toBeGreaterThanOrEqual(1);
    // The empty-doc fast path serializes to "" — firing blur must not throw.
    expect(() =>
      act(() => {
        for (const cb of editorListeners.blur ?? []) cb();
      }),
    ).not.toThrow();
  });
});

describe("ScratchNotePanel - SLW-PT-7 personal-team load keying", () => {
  beforeEach(() => {
    mockActions.loadNotes.mockClear();
    mockUser.id = null; // logged out (uid 0)
  });
  afterEach(() => {
    mockUser.id = null; // don't leak a resolved id into other suites
  });

  it("reloads notes when the user id resolves (boot auth race) and on account switch", () => {
    const { rerender } = render(<ScratchNotePanel />);
    // Initial mount resolves uid 0 (auth not ready) → one load.
    expect(mockActions.loadNotes).toHaveBeenCalledTimes(1);

    // Auth resolves → the id changes 0→42 → notes re-resolve to the personal team.
    mockUser.id = 42;
    rerender(<ScratchNotePanel />);
    expect(mockActions.loadNotes).toHaveBeenCalledTimes(2);

    // A re-render with an unchanged id must NOT re-fire (no reload loop).
    rerender(<ScratchNotePanel />);
    expect(mockActions.loadNotes).toHaveBeenCalledTimes(2);

    // Account switch (42→99) re-resolves again.
    mockUser.id = 99;
    rerender(<ScratchNotePanel />);
    expect(mockActions.loadNotes).toHaveBeenCalledTimes(3);
  });

  it("SLW-PT-19: reloads notes when the active org changes (one set of notes per org)", () => {
    mockUser.id = 42;
    act(() => useOrgStore.setState({ activeOrgSlug: "acme/a" }));
    const { rerender } = render(<ScratchNotePanel />);
    expect(mockActions.loadNotes).toHaveBeenCalledTimes(1);

    act(() => useOrgStore.setState({ activeOrgSlug: "acme/b" }));
    rerender(<ScratchNotePanel />);
    expect(mockActions.loadNotes).toHaveBeenCalledTimes(2);

    rerender(<ScratchNotePanel />);
    expect(mockActions.loadNotes).toHaveBeenCalledTimes(2);
    act(() => useOrgStore.setState({ activeOrgSlug: null }));
  });
});

// SW-SN-15: the go-to-heading modal is the third jump surface — it dispatches
// through the `pendingHeadingJump` store signal rather than a direct click. The
// outline/sticky tests cover their own click seams; this pins the modal's own
// seam so the modal-dispatch path is proven to reach the (container-scoped)
// scroll, not just assumed to via the shared helper.
describe("ScratchNotePanel - SW-SN-15 go-to-heading modal jump seam", () => {
  const NOTE = { id: "n1", title: "My Note", pinned: false, lastOpenedAt: 1 };
  // A doc with one heading — `enumerateHeadings` only walks `doc.descendants`,
  // so this is enough to resolve the "prompts" slug without a real PM doc (tiptap
  // is mocked in this file).
  const headingDoc = {
    descendants: (cb: (node: unknown, pos: number) => void) => {
      cb({ type: { name: "heading" }, textContent: "Prompts", attrs: { level: 2 } }, 3);
    },
  };

  // The panel reads `editor.view.dom` (scroll port) + `nodeDOM`. Swap in a real
  // container + heading with mocked rects, then restore so no state leaks.
  const ed = mockEditor as unknown as {
    state: { doc: unknown };
    view: { dom: HTMLElement; nodeDOM: (pos: number) => Node | null };
  };
  const origDoc = ed.state.doc;
  const origDom = ed.view.dom;
  const origNodeDOM = ed.view.nodeDOM;
  afterEach(() => {
    ed.state.doc = origDoc;
    ed.view.dom = origDom;
    ed.view.nodeDOM = origNodeDOM;
    // `scrollToHeadingBySlug` tracks the flashed heading in a module-level var;
    // clear it so a jump here can't leak a stale detached-element reference into
    // a sibling test (vitest isolates files, but not tests within one).
    clearHeadingFlash();
  });

  // We assert via the landing-flash side effect (applied by `scrollToHeadingBySlug`
  // right after it calls the container-scoped `scrollToHeadingPos`), not the raw
  // `scrollTop`: the panel's own scroll-memory reset and React StrictMode's
  // double-effect both perturb `scrollTop`, whereas the flash cleanly proves the
  // modal dispatch resolved the slug and ran the shared jump. The scroll math
  // itself is pinned in note-headings.test.ts + the outline/sticky seams.
  it("resolves the pending heading and runs the shared jump, then clears the signal", () => {
    const container = makeScrollContainer(0);
    const heading = makeHeadingEl(250, "h2");
    ed.state.doc = headingDoc;
    ed.view.dom = container;
    ed.view.nodeDOM = () => heading;

    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    mockStoreState.pendingHeadingJump = { noteId: "n1", slug: "prompts" };

    render(<ScratchNotePanel />);

    // The modal dispatch reached the shared jump (which scrolls the note's own
    // container) and landed the flash on the resolved heading.
    expect(heading.classList.contains("scratch-heading-flash")).toBe(true);
    // The one-shot jump signal is consumed so it can't re-fire.
    expect(mockActions.clearHeadingJump).toHaveBeenCalled();
  });

  it("ignores a jump whose noteId is for a different (already-switched) note", () => {
    const container = makeScrollContainer(0);
    const heading = makeHeadingEl(250, "h2");
    ed.state.doc = headingDoc;
    ed.view.dom = container;
    ed.view.nodeDOM = () => heading;

    mockStoreState.notes = [NOTE];
    mockStoreState.activeNoteId = "n1";
    // A stale jump targeting a note we've since navigated away from. The doc DOES
    // contain "prompts", so a missing flash proves the noteId guard fired — not a
    // slug-not-found no-op.
    mockStoreState.pendingHeadingJump = { noteId: "n2", slug: "prompts" };

    render(<ScratchNotePanel />);

    // The mismatch guard returns before the jump — no heading is resolved/flashed…
    expect(heading.classList.contains("scratch-heading-flash")).toBe(false);
    // …but the stale one-shot signal is still cleared so it can't linger.
    expect(mockActions.clearHeadingJump).toHaveBeenCalled();
  });
});
