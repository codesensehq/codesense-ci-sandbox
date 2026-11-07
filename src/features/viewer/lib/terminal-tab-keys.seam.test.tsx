// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach, onTestFinished } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@/test-utils";
import userEvent from "@testing-library/user-event";
import { useEffect, useRef } from "react";

import { TerminalTileChrome } from "../components/terminal-tile-chrome";
import { TerminalFindBar, type TerminalSearch } from "../components/terminal-find-bar";
import { TileNeedsYouSlot } from "../components/tile-needs-you-slot";
import { useTileTabActions } from "../components/workspace-terminal-panels";
import { useWorkspaceCycleIntercept } from "../hooks/use-workspace-cycle-intercept";
import { recordClosedTab, resetClosedTabHistoryForTests } from "./closed-tab-history";
import {
  newTerminalTab,
  resetExpandStepForTests,
  resetWhatNeedsYouCycleForTests,
  TERMINAL_SESSION_ID_ATTR,
} from "./terminal-tab-keys";
import { useMarkActiveSessionSeen } from "../hooks/use-mark-active-session-seen";
import {
  getSessionNeedsYou,
  getNeedsYouState,
  selectSessionsNeedingYou,
} from "./cli-workspace/store/terminal-tab-selectors";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { useShortcutHandler } from "@/shared/hooks/use-shortcut-handler";
import { markSessionSeen, useAgentStateStore } from "@/shared/stores/agent-state-store";
import { useWeftJumpStore } from "@/features/weft/stores/weft-jump-store";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  resetManagedSessionStoreForTests,
  useManagedSessionStore,
} from "@/shared/stores/use-managed-session-store";
import {
  selectExpandedWorkspaceId,
  selectActiveTerminalTab,
  selectHiddenTerminalTabs,
  selectWorkspaceTabs,
  shouldRenderTabGrid,
  useViewerStore,
  workspacePanedTabIds,
  type Workspace,
} from "@/shared/stores/viewer-store";
import { USER_HIDDEN_TAG } from "@/shared/lib/terminal-layout-projection";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
  type SeedTab,
} from "@/test/terminal-layout-harness";
import { resetTestTerminalHost } from "@/test/terminal-layout-source-mock";

/**
 * The terminal tab keys, end to end through the real pieces: a keydown in the
 * terminal area goes through the real capture-phase intercept and the real
 * shortcut registry into the real viewer store and its layout verbs, the test
 * terminal host answers them, and the real tile chrome, tab bar, question banner
 * and find bar render the result. Only the process probe (IPC) and xterm itself
 * are faked: each tile's "terminal" is a textarea under `.xterm`, with a
 * keydown listener standing in for the PTY — no claimed key may reach it.
 */

const backend = vi.hoisted(() => ({ running: new Map<string, string>() }));
const pending = vi.hoisted(() => ({ session: null as string | null }));
vi.mock("./attention-alerts-controller", async (importOriginal) => ({
  ...(await importOriginal<typeof import("./attention-alerts-controller")>()),
  // The session the Jump pill offers (the controller itself is tested on its own).
  getPendingAttentionTarget: () => pending.session,
}));

vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  sessionsRunningProcesses: vi.fn(async (ids: string[]) =>
    ids.map((id) => ({ foreground: backend.running.get(id) ?? null, background: [] })),
  ),
  dashboardForegroundSession: vi.fn(async () => {}),
}));
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }) }));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

installTerminalLayoutHarness();

const TEAM = "acme:default";
const REPO = "/repo";
const WS = "ws-story";
const OTHER = "ws-other";
const TRAY = "ws-tray";
const THIRD = "ws-third";
const FOURTH = "ws-fourth";

const story = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });
const named = (session: string): SeedTab => ({ session, name: session });
const hiddenTab = (session: string): SeedTab => ({
  session,
  name: session,
  placement: "background",
  tags: { [USER_HIDDEN_TAG]: "tab" },
});

/** Keys the fake PTY received — must stay empty for every claimed chord. */
let ptyKeys: string[] = [];

/** A fake search addon per session, recording every call the find bar makes. */
function fakeSearch() {
  let listener: ((e: { resultIndex: number; resultCount: number }) => void) | null = null;
  const search = {
    findNext: vi.fn(() => true),
    findPrevious: vi.fn(() => true),
    clearDecorations: vi.fn(),
    onDidChangeResults: (l: (e: { resultIndex: number; resultCount: number }) => void) => {
      listener = l;
      return { dispose: () => (listener = null) };
    },
    emit: (resultIndex: number, resultCount: number) => listener?.({ resultIndex, resultCount }),
  };
  return search satisfies TerminalSearch;
}
let searches: Record<string, ReturnType<typeof fakeSearch>> = {};
const cleared: string[] = [];
/** Sessions whose program is on the alternate screen (vim, htop, an agent TUI). */
const altScreen = new Set<string>();

/** One tile: the real chrome + tab bar + question banner, and the active tab's
 *  fake terminal + real find bar. */
function Tile({ wsId }: { wsId: string }) {
  const tabs = useViewerStore((s) => selectWorkspaceTabs(s, TEAM, wsId));
  const activeTabId = useViewerStore((s) => selectActiveTerminalTab(s, TEAM, wsId));
  const focusTile = () => useViewerStore.getState().switchWorkspace(TEAM, wsId);
  const actions = useTileTabActions(TEAM, wsId, focusTile);
  const active = tabs.find((t) => t.id === activeTabId) ?? tabs[0];
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    const el = textareaRef.current;
    const onKey = (e: KeyboardEvent) => ptyKeys.push(e.key);
    el?.addEventListener("keydown", onKey);
    return () => el?.removeEventListener("keydown", onKey);
  }, [active?.sessionId]);
  if (!active) return null;
  const sessionId = active.sessionId;
  searches[sessionId] ??= fakeSearch();
  return (
    <div data-pane-id={wsId}>
      <TerminalTileChrome
        workspace={story(wsId)}
        isFocused
        tabs={tabs}
        activeTabId={active.id}
        onFocusTile={focusTile}
        {...actions}
      />
      <TileNeedsYouSlot
        workspaceId={wsId}
        tabs={tabs}
        activeTabId={active.id}
        onShowTab={actions.onSelectTab}
      />
      <div {...{ [TERMINAL_SESSION_ID_ATTR]: sessionId }} className="relative">
        <div className="xterm">
          <textarea ref={textareaRef} aria-label={`terminal ${active.id}`} />
        </div>
        <TerminalFindBar
          sessionId={sessionId}
          getSearch={() => searches[sessionId]}
          onClear={() => {
            if (altScreen.has(sessionId)) return false;
            cleared.push(sessionId);
            return true;
          }}
          onClose={() => textareaRef.current?.focus()}
        />
      </div>
    </div>
  );
}

function TerminalArea() {
  useWorkspaceCycleIntercept(REPO);
  useShortcutHandler();
  const live = useViewerStore((s) => s.workspaces[TEAM] ?? []);
  return (
    <>
      <input aria-label="outside the terminal" />
      <div data-xterm-host data-repo-path={REPO}>
        {live.map((w) => (
          <Tile key={w.id} wsId={w.id} />
        ))}
      </div>
    </>
  );
}

/** WS holds t1..t3 (t1 active); `other` adds OTHER with o1. Starts from a fresh
 *  host, so a test can re-seed over the default. */
function seed(opts: { tabs?: string[]; other?: boolean } = {}) {
  const tabs = opts.tabs ?? ["t1", "t2", "t3"];
  const host = resetTestTerminalHost((change) =>
    useViewerStore.getState().applyLayout({ kind: "change", target: host.target(), change }),
  );
  useViewerStore.setState({
    terminalLayout: { hydrated: false, admission: {}, members: {}, byKey: {} },
  });
  useViewerStore.getState().applyLayout({ kind: "snapshot", replies: [terminalHost().reply()] });
  useViewerStore.setState({
    workspaces: { [TEAM]: opts.other ? [story(WS), story(OTHER)] : [story(WS)] },
    activeWorkspace: { [TEAM]: WS },
    softClosedWorkspaces: { [TEAM]: {} },
    foregroundLayout: {},
    cliExpanded: false,
    aiStatusBySession: {},
    trayOrderByTeam: {},
  });
  seedTerminalTabs(TEAM, WS, tabs.map(named), { active: tabs[0] });
  if (opts.other) seedTerminalTabs(TEAM, OTHER, [named("o1")], { active: "o1" });
}

/** A tile minimized in the tray, running `tabs`. */
function seedTrayTile(tabs: SeedTab[]) {
  useViewerStore.setState((st) => ({
    softClosedWorkspaces: {
      ...st.softClosedWorkspaces,
      [TEAM]: { ...st.softClosedWorkspaces[TEAM], [TRAY]: { workspace: story(TRAY) } },
    },
  }));
  seedTerminalTabs(TEAM, TRAY, tabs);
}

const s = () => useViewerStore.getState();
const tabsIn = (ws: string) => selectWorkspaceTabs(s(), TEAM, ws).map((t) => t.id);
const hiddenIn = (ws: string) => selectHiddenTerminalTabs(s(), TEAM, ws).map((t) => t.id);
const activeIn = (ws: string) => selectActiveTerminalTab(s(), TEAM, ws);
const nameOf = (ws: string, id: string) =>
  selectWorkspaceTabs(s(), TEAM, ws).find((t) => t.id === id)?.name;
const terminal = (tabId: string) => screen.getByLabelText(`terminal ${tabId}`);
const settle = () => act(async () => new Promise((r) => setTimeout(r, 0)));

/** Press a chord where keyboard focus is. */
function press(init: KeyboardEventInit) {
  fireEvent.keyDown(document.activeElement ?? document.body, init);
}

const originalPlatform = navigator.platform;
function setPlatform(value: string) {
  Object.defineProperty(navigator, "platform", { value, configurable: true, writable: true });
}

beforeEach(() => {
  vi.clearAllMocks();
  // The Mac rules are the strict ones (⌘ only; ⌃ chords belong to the shell).
  setPlatform("MacIntel");
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  backend.running = new Map();
  ptyKeys = [];
  searches = {};
  cleared.length = 0;
  altScreen.clear();
  resetClosedTabHistoryForTests();
  resetWhatNeedsYouCycleForTests();
  resetExpandStepForTests();
  resetManagedSessionStoreForTests();
  useAgentStateStore.setState({ bySession: {}, exitBySession: {}, stateSince: {}, seenAt: {} });
  useWeftJumpStore.setState({ jumpToNextNeedsYou: null });
  pending.session = null;
  seed();
});

afterEach(async () => {
  // Let a focus the coordinator scheduled for the next frame land in THIS test.
  await act(async () => new Promise((r) => setTimeout(r, 50)));
  setPlatform(originalPlatform);
  vi.restoreAllMocks(); // spies on store verbs never outlive their test
  expect(ptyKeys).toEqual([]);
});

describe("Control chords stay the shell's on macOS", () => {
  it.each([
    ["⌃T (fzf, transpose, Claude Code's task toggle)", { key: "t", ctrlKey: true }],
    ["⌃F (forward-char, page down)", { key: "f", ctrlKey: true }],
    ["⌃⇧K", { key: "K", ctrlKey: true, shiftKey: true }],
    ["⌃⌥1", { key: "1", code: "Digit1", ctrlKey: true, altKey: true }],
    ["⌃⌥G", { key: "g", code: "KeyG", ctrlKey: true, altKey: true }],
    ["⌃D (EOF)", { key: "d", ctrlKey: true }],
  ])("%s reaches the terminal and triggers nothing", async (_, init) => {
    render(<TerminalArea />);
    terminal("t1").focus();
    press(init);
    await settle();
    expect(ptyKeys).toEqual([init.key]);
    ptyKeys = [];
    expect(tabsIn(WS)).toEqual(["t1", "t2", "t3"]);
    expect(activeIn(WS)).toBe("t1");
    expect(terminalHost().opened).toEqual([]);
    expect(screen.queryByTestId("terminal-find-bar")).toBeNull();
    expect(cleared).toEqual([]);
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false);
  });

  it("the same keys with ⌘ act", async () => {
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "f", metaKey: true });
    expect(screen.getByTestId("terminal-find-bar")).toBeInTheDocument();
    terminal("t1").focus();
    press({ key: "K", metaKey: true, shiftKey: true });
    expect(cleared).toEqual(["t1"]);
    press({ key: "t", metaKey: true });
    await settle();
    expect(tabsIn(WS)).toHaveLength(4);
  });
});

describe("⌘T — a quick tab on this Mac", () => {
  it("opens a tab in the focused workspace without the chooser", async () => {
    seed({ other: true });
    const choose = vi.fn();
    window.addEventListener("codesense:new-terminal", choose, { once: true });
    onTestFinished(() => window.removeEventListener("codesense:new-terminal", choose));
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "t", metaKey: true });
    await settle();
    expect(choose).not.toHaveBeenCalled();
    expect(terminalHost().opened).toHaveLength(1);
    expect(tabsIn(WS)).toHaveLength(4);
    expect(tabsIn(OTHER)).toEqual(["o1"]);
  });
});

describe("the + button", () => {
  it("click opens a quick tab; ⌥-click and right-click open the machine chooser", async () => {
    const choose = vi.fn();
    window.addEventListener("codesense:new-terminal", choose);
    onTestFinished(() => window.removeEventListener("codesense:new-terminal", choose));
    render(<TerminalArea />);
    const plus = screen.getByTestId("terminal-tab-new");
    fireEvent.click(plus, { altKey: true });
    fireEvent.contextMenu(plus);
    await settle();
    expect(choose).toHaveBeenCalledTimes(2);
    expect(terminalHost().opened).toHaveLength(0);
    fireEvent.click(plus);
    await settle();
    expect(terminalHost().opened).toHaveLength(1);
    expect(choose).toHaveBeenCalledTimes(2);
  });
});

describe("⇧⌘N — New terminal on… a chosen machine", () => {
  it("requests the chooser without spawning before selection", async () => {
    seed({ other: true });
    const choose = vi.fn();
    window.addEventListener("codesense:new-terminal", choose, { once: true });
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "N", metaKey: true, shiftKey: true });
    await settle();
    expect(choose).toHaveBeenCalledOnce();
    expect(choose.mock.calls[0][0].detail).toEqual({ teamKey: TEAM, workspaceId: WS });
    expect(terminalHost().opened).toHaveLength(0);
    expect(tabsIn(WS)).toEqual(["t1", "t2", "t3"]);
    expect(tabsIn(OTHER)).toEqual(["o1"]);
  });
});

describe("⌘D — Split: a new terminal beside the focused one", () => {
  it("from the terminal, opens one beside the focused tab, side by side, active", async () => {
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "d", metaKey: true });
    await settle();
    expect(terminalHost().opened).toHaveLength(1);
    const opened = terminalHost().opened[0].session;
    expect(tabsIn(WS)).toEqual(["t1", opened, "t2", "t3"]);
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
    expect(workspacePanedTabIds(s(), TEAM, WS)).toEqual(["t1", opened]);
    expect(activeIn(WS)).toBe(opened);
  });

  it("in a split, expands the focused tile first so the two can show side by side", async () => {
    seed({ other: true });
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "d", metaKey: true });
    await settle();
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
    expect(tabsIn(OTHER)).toEqual(["o1"]);
  });

  it("a text field outside the terminal keeps its own ⌘D", async () => {
    render(<TerminalArea />);
    screen.getByLabelText("outside the terminal").focus();
    press({ key: "d", metaKey: true });
    await settle();
    expect(terminalHost().opened).toEqual([]);
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false);
  });
});

describe("a held open never steals focus later", () => {
  it("local selection: if the user has left the terminal by the time the host opens the tab, focus stays where they went", async () => {
    useViewerStore.setState({ terminalVisible: true });
    let release: () => void = () => {};
    const real = useViewerStore.getState().addTerminalTab;
    // A store action swapped in state (every later state object copies it), put
    // back at the end.
    useViewerStore.setState({
      addTerminalTab: (...args) =>
        new Promise<void>((resolve) => {
          release = () => void real(...args).then(resolve);
        }),
    });
    onTestFinished(() => useViewerStore.setState({ addTerminalTab: real }));
    render(<TerminalArea />);
    terminal("t1").focus();
    act(() => {
      newTerminalTab(TEAM);
    });
    const outside = screen.getByLabelText("outside the terminal");
    outside.focus();
    await act(async () => release());
    await act(async () => new Promise((r) => setTimeout(r, 50)));
    expect(tabsIn(WS)).toHaveLength(4);
    expect(document.activeElement).toBe(outside);
  });
});

describe("a new tab while side by side", () => {
  it("joins the grid as a cell and takes focus, so it is on screen at once", async () => {
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "M", code: "KeyM", metaKey: true, shiftKey: true });
    await settle();
    expect(workspacePanedTabIds(s(), TEAM, WS)).toEqual(["t1", "t2", "t3"]);
    act(() => {
      newTerminalTab(TEAM, WS);
    });
    await act(async () => new Promise((r) => setTimeout(r, 50)));
    const opened = terminalHost().opened[0].session;
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(true);
    expect(workspacePanedTabIds(s(), TEAM, WS)).toEqual(["t1", "t2", "t3", opened]);
    expect(activeIn(WS)).toBe(opened);
  });

  it("in tabs, opens an ordinary active tab and leaves the grid off", async () => {
    render(<TerminalArea />);
    act(() => {
      newTerminalTab(TEAM, WS);
    });
    await act(async () => new Promise((r) => setTimeout(r, 50)));
    const opened = terminalHost().opened[0].session;
    expect(activeIn(WS)).toBe(opened);
    expect(shouldRenderTabGrid(s(), TEAM, WS)).toBe(false);
  });
});

describe("⌘W — stays the App-level close (Hide / End)", () => {
  it("is not claimed by the terminal intercept, so the App's close decision tree still gets it", () => {
    render(<TerminalArea />);
    const appLevel = vi.fn();
    document.addEventListener("keydown", appLevel);
    terminal("t1").focus();
    press({ key: "w", metaKey: true });
    document.removeEventListener("keydown", appLevel);
    expect(appLevel).toHaveBeenCalledTimes(1);
    expect(tabsIn(WS)).toEqual(["t1", "t2", "t3"]);
    // Unclaimed, it reaches the terminal like any key (xterm ignores ⌘ chords).
    expect(ptyKeys).toEqual(["w"]);
    ptyKeys = [];
  });
});

describe("F2 — rename inline", () => {
  it("in the terminal itself, F2 goes to the program running there — no rename", () => {
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "F2" });
    expect(ptyKeys).toEqual(["F2"]);
    ptyKeys = [];
    expect(screen.queryByDisplayValue("t1")).toBeNull();
  });

  it("on a focused pill it renames that tab through the host: Enter commits and the cursor goes to the terminal", async () => {
    render(<TerminalArea />);
    screen.getByText("t1").closest("button")!.focus();
    press({ key: "F2" });
    const input = await screen.findByDisplayValue("t1");
    await waitFor(() => expect(document.activeElement).toBe(input));
    fireEvent.change(input, { target: { value: "build" } });
    fireEvent.keyDown(input, { key: "Enter" });
    await settle();
    expect(nameOf(WS, "t1")).toBe("build");
    expect(document.activeElement).toBe(terminal("t1"));
  });

  it("Esc cancels — the name is unchanged — and returns to the terminal", async () => {
    render(<TerminalArea />);
    screen.getByText("t1").closest("button")!.focus();
    press({ key: "F2" });
    const input = await screen.findByDisplayValue("t1");
    fireEvent.change(input, { target: { value: "discard" } });
    fireEvent.keyDown(input, { key: "Escape" });
    fireEvent.blur(input);
    await settle();
    expect(nameOf(WS, "t1")).toBe("t1");
    expect(screen.queryByDisplayValue("discard")).toBeNull();
    expect(document.activeElement).toBe(terminal("t1"));
  });

  it("renames the pill's tab even when another tab is active", async () => {
    render(<TerminalArea />);
    screen.getByText("t3").closest("button")!.focus();
    press({ key: "F2" });
    expect(await screen.findByDisplayValue("t3")).toBeInTheDocument();
  });

  it("does nothing outside the terminal area", () => {
    render(<TerminalArea />);
    screen.getByLabelText("outside the terminal").focus();
    press({ key: "F2" });
    expect(screen.queryByDisplayValue("t1")).toBeNull();
  });
});

describe("⌘F — find in the terminal", () => {
  it("targets the shown terminal while another mounted pane has a find handle", async () => {
    searches.t2 = fakeSearch();
    render(
      <>
        <TerminalArea />
        <div style={{ display: "none" }} inert aria-hidden>
          <TerminalFindBar
            sessionId="t2"
            getSearch={() => searches.t2}
            onClear={() => true}
            onClose={() => {}}
          />
        </div>
      </>,
    );
    terminal("t1").focus();
    press({ key: "f", metaKey: true });
    const input = await screen.findByRole("textbox", { name: "Find in terminal" });
    fireEvent.change(input, { target: { value: "only shown" } });
    expect(searches.t1.findNext).toHaveBeenCalledWith("only shown", expect.anything());
    expect(searches.t2.findNext).not.toHaveBeenCalled();
  });
  it("opens the find bar; typing searches; Enter / ⇧Enter step; Aa; the count; Esc closes and refocuses", async () => {
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "f", metaKey: true });
    const input = await screen.findByRole("textbox", { name: "Find in terminal" });
    await waitFor(() => expect(document.activeElement).toBe(input));
    const search = searches["t1"];

    fireEvent.change(input, { target: { value: "error" } });
    expect(search.findNext).toHaveBeenLastCalledWith(
      "error",
      expect.objectContaining({ incremental: true, caseSensitive: false }),
    );
    act(() => search.emit(1, 5));
    expect(screen.getByTestId("terminal-find-count")).toHaveTextContent("2 of 5");

    fireEvent.keyDown(input, { key: "Enter" });
    expect(search.findNext).toHaveBeenLastCalledWith(
      "error",
      expect.objectContaining({ incremental: false }),
    );
    fireEvent.keyDown(input, { key: "Enter", shiftKey: true });
    expect(search.findPrevious).toHaveBeenCalledWith("error", expect.anything());

    fireEvent.click(screen.getByRole("button", { name: "Match case" }));
    expect(search.findNext).toHaveBeenLastCalledWith(
      "error",
      expect.objectContaining({ caseSensitive: true }),
    );
    act(() => search.emit(-1, 0));
    expect(screen.getByTestId("terminal-find-count")).toHaveTextContent("No results");

    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.queryByTestId("terminal-find-bar")).toBeNull();
    expect(search.clearDecorations).toHaveBeenCalled();
    expect(document.activeElement).toBe(terminal("t1"));
  });

  it("⌘F outside the terminal area leaves the viewer's own find alone", () => {
    render(<TerminalArea />);
    screen.getByLabelText("outside the terminal").focus();
    press({ key: "f", metaKey: true });
    expect(screen.queryByTestId("terminal-find-bar")).toBeNull();
  });

  it("⌘⇧K clears the focused terminal", () => {
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "K", metaKey: true, shiftKey: true });
    expect(cleared).toEqual(["t1"]);
  });

  it("⌘⇧K leaves a full-screen program's screen alone", () => {
    altScreen.add("t1");
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "K", metaKey: true, shiftKey: true });
    expect(cleared).toEqual([]);
  });
});

describe("⌘⌥1..9 — jump to tab N", () => {
  it("jumps by the physical digit even though Option rewrites the character", async () => {
    render(<TerminalArea />);
    terminal("t1").focus();
    // macOS reports ⌥2 as "™".
    press({ key: "™", code: "Digit2", metaKey: true, altKey: true });
    await settle();
    expect(activeIn(WS)).toBe("t2");
  });

  it("⌘⌥9 is always the last tab", async () => {
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "ª", code: "Digit9", metaKey: true, altKey: true });
    await settle();
    expect(activeIn(WS)).toBe("t3");
  });

  it("⌘3 (no Option) stays the workspace jump, not a tab jump", async () => {
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "3", code: "Digit3", metaKey: true });
    await settle();
    expect(activeIn(WS)).toBe("t1");
  });
});

describe("⌘⇧T — reopen the last closed tab", () => {
  it("brings back a Hidden (running) tab on its live session", async () => {
    backend.running.set("t2", "claude");
    render(<TerminalArea />);
    screen.getByText("t2").closest("button")!.focus();
    await userEvent.keyboard("{Delete}"); // t2 running → Hide
    await waitFor(() => expect(hiddenIn(WS)).toEqual(["t2"]));
    // A more recent close in another team must wait there, even when workspace
    // ids repeat across teams. The keyboard operates on the active team.
    const backgroundTeam = "acme:background";
    seedTerminalTabs(backgroundTeam, WS, [named("background-tab")]);
    recordClosedTab({
      kind: "hidden",
      teamKey: backgroundTeam,
      workspaceId: WS,
      tab: selectWorkspaceTabs(s(), backgroundTeam, WS)[0],
    });
    terminal(activeIn(WS)!).focus();
    press({ key: "T", metaKey: true, shiftKey: true });
    await waitFor(() => expect(activeIn(WS)).toBe("t2"));
    expect(tabsIn(WS)).toContain("t2");
    expect(hiddenIn(WS)).toEqual([]);
    expect(terminalHost().contains("t2")).toBe(true);
    expect(terminalHost().opened).toEqual([]); // the same session, not a new one
  });

  it("reopens an Ended tab as a fresh shell under the same name in the same workspace", async () => {
    render(<TerminalArea />);
    screen.getByText("t3").closest("button")!.focus();
    await userEvent.keyboard("{Delete}"); // t3 idle → End
    await waitFor(() => expect(terminalHost().contains("t3")).toBe(false));
    terminal("t1").focus();
    press({ key: "T", metaKey: true, shiftKey: true });
    await waitFor(() => expect(terminalHost().opened).toHaveLength(1));
    const fresh = terminalHost().opened[0];
    expect(fresh.session).not.toBe("t3");
    expect(fresh.name).toBe("t3");
    expect(tabsIn(WS)).toEqual(["t1", "t2", fresh.session]);
    expect(activeIn(WS)).toBe(fresh.session);
  });

  it("an Ended AGENT tab reopens as a plainly named shell, not under the agent's name", async () => {
    seed({ tabs: ["t1", "claude"] });
    useViewerStore.setState({ aiStatusBySession: { claude: "done" } });
    render(<TerminalArea />);
    screen.getByText("claude").closest("button")!.focus();
    await userEvent.keyboard("{Delete}"); // an idle tab: End
    await waitFor(() => expect(terminalHost().contains("claude")).toBe(false));
    terminal("t1").focus();
    press({ key: "T", metaKey: true, shiftKey: true });
    await waitFor(() => expect(terminalHost().opened).toHaveLength(1));
    const fresh = terminalHost().opened[0].session;
    expect(nameOf(WS, fresh)).toMatch(/^Terminal \d+$/);
  });

  it("ending a tile's last tab then ⌘⇧T reuses the fresh shell that replaced it — no second one", async () => {
    seed({ tabs: ["t1"] });
    render(<TerminalArea />);
    screen.getByText("t1").closest("button")!.focus();
    await userEvent.keyboard("{Delete}"); // the only, idle tab: End
    await waitFor(() => expect(terminalHost().opened).toHaveLength(1)); // the replacement
    const replacement = terminalHost().opened[0].session;
    (document.querySelector(".xterm textarea") as HTMLElement).focus();
    press({ key: "T", metaKey: true, shiftKey: true });
    await waitFor(() => expect(nameOf(WS, replacement)).toBe("t1"));
    expect(terminalHost().opened).toHaveLength(1);
    expect(tabsIn(WS)).toEqual([replacement]);
  });

  it("with nothing closed, does nothing", async () => {
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "T", metaKey: true, shiftKey: true });
    await settle();
    expect(tabsIn(WS)).toEqual(["t1", "t2", "t3"]);
    expect(terminalHost().opened).toEqual([]);
  });
});

describe("⌘⇧M — expand in and out", () => {
  it("from the terminal: in to the tile, in to all its tabs, and back out, never reaching the PTY", async () => {
    seed({ other: true });
    render(<TerminalArea />);
    const step = async () => {
      terminal(activeIn(WS)!).focus();
      press({ key: "M", code: "KeyM", metaKey: true, shiftKey: true });
      await settle();
      const zoomed = selectExpandedWorkspaceId(s(), TEAM) === WS;
      return shouldRenderTabGrid(s(), TEAM, WS) ? "all tabs" : zoomed ? "expanded" : "workspaces";
    };
    expect([await step(), await step(), await step(), await step()]).toEqual([
      "expanded",
      "all tabs",
      "expanded",
      "workspaces",
    ]);
  });
});

const ask = (correlationId: string, sessionId: string) => ({
  correlationId,
  sessionId,
  question: "Ship it?",
  options: ["yes", "no"],
});
/** When each session's dot began — the needs-you list's order (oldest first). */
const since = (order: Record<string, number>) =>
  useAgentStateStore.setState((st) => ({ stateSince: { ...st.stateSince, ...order } }));
const banner = (ws: string) =>
  document.querySelector<HTMLElement>(`[data-pane-id="${ws}"] [data-tile-question]`);
const pressJump = () => press({ key: "J", metaKey: true, shiftKey: true });

describe("⌘⇧J — the one jump to what needs you", () => {
  it("cycles the shared needs-you list across open, Hidden and tray tabs, restoring each; a question lands in its banner", async () => {
    seed({ other: true });
    seedTerminalTabs(TEAM, OTHER, [hiddenTab("o2")]); // Hidden under OTHER's chip
    seedTrayTile([named("y1")]); // a whole tile minimized in the tray
    useViewerStore.setState({ aiStatusBySession: { t3: "needs-permission", y1: "error" } });
    useManagedSessionStore.setState({ pendingAsks: { "ask-user-1": ask("ask-user-1", "o2") } });
    since({ t3: 1, o2: 2, y1: 3 });
    useViewerStore.setState({
      contextHistory: { back: [], forward: [{ teamKey: TEAM, workspaceId: TRAY }] },
    });
    render(<TerminalArea />);
    terminal("t1").focus();

    pressJump(); // t3: needs-permission, same tile → its terminal
    await waitFor(() => expect(document.activeElement).toBe(terminal("t3")));
    expect(s().activeWorkspace[TEAM]).toBe(WS);

    pressJump(); // o2: a question, Hidden — restored onto OTHER, focus in its banner
    await waitFor(() => expect(banner(OTHER)?.contains(document.activeElement)).toBe(true));
    expect(s().activeWorkspace[TEAM]).toBe(OTHER);
    expect(activeIn(OTHER)).toBe("o2");
    expect(hiddenIn(OTHER)).toEqual([]);
    expect(s().contextHistory).toEqual({ back: [{ teamKey: TEAM, workspaceId: WS }], forward: [] });

    pressJump(); // y1: failed, its tile in the tray — the TILE comes back, y1 on it
    await waitFor(() => expect(document.activeElement).toBe(terminal("y1")));
    expect(s().activeWorkspace[TEAM]).toBe(TRAY);
    expect(tabsIn(TRAY)).toEqual(["y1"]);

    pressJump(); // wraps to t3
    await waitFor(() => expect(activeIn(WS)).toBe("t3"));
    expect(s().activeWorkspace[TEAM]).toBe(WS);
    // Nothing was opened or grafted anywhere: every landing is its own session.
    expect(terminalHost().opened).toEqual([]);
  });

  it("a minimized tile's tab: the tile is RESTORED from the tray, its session never adopted elsewhere", async () => {
    seedTrayTile([named("y1"), named("y2")]);
    useViewerStore.setState({ aiStatusBySession: { y2: "needs-permission" } });
    const adopt = vi.spyOn(useViewerStore.getState(), "adoptTerminalSession");
    render(<TerminalArea />);
    terminal("t1").focus();
    pressJump();
    await waitFor(() => expect(document.activeElement).toBe(terminal("y2")));
    expect(s().workspaces[TEAM].map((w) => w.id)).toContain(TRAY);
    expect(s().softClosedWorkspaces[TEAM]?.[TRAY]).toBeUndefined();
    expect(tabsIn(TRAY)).toEqual(["y1", "y2"]);
    expect(tabsIn(WS)).toEqual(["t1", "t2", "t3"]);
    expect(adopt).not.toHaveBeenCalled();
  });

  it("goes to the focused tile's waiting question first; pressed again from the banner, moves on", async () => {
    seed({ other: true });
    useViewerStore.setState({ aiStatusBySession: { o1: "needs-permission" } });
    useManagedSessionStore.setState({ pendingAsks: { "ask-user-1": ask("ask-user-1", "t1") } });
    since({ t1: 1, o1: 2 });
    render(<TerminalArea />);
    terminal("t1").focus();

    pressJump();
    await waitFor(() => expect(banner(WS)?.contains(document.activeElement)).toBe(true));
    expect(s().activeWorkspace[TEAM]).toBe(WS);

    pressJump();
    await waitFor(() => expect(document.activeElement).toBe(terminal("o1")));
    expect(s().activeWorkspace[TEAM]).toBe(OTHER);
  });

  it("jumps to a seen unresolved block until real activity clears it", async () => {
    useAgentStateStore
      .getState()
      .recordSignal("t2", { tier: 1, signal: { kind: "signal-progress", phase: "blocked" } });
    render(<TerminalArea />);
    act(() => {
      since({ t2: 1 });
      markSessionSeen("t2");
    });
    expect(
      selectSessionsNeedingYou(getNeedsYouState(), TEAM).find((row) => row.sessionId === "t2"),
    ).toMatchObject({ kind: "blocked", seen: true });
    terminal("t1").focus();
    pressJump();
    await waitFor(() => expect(activeIn(WS)).toBe("t2"));
    act(() =>
      useAgentStateStore.getState().recordSignal("t2", {
        tier: 1,
        signal: { kind: "claude-hook", event: "UserPromptSubmit" },
      }),
    );
    expect(getSessionNeedsYou("t2")).toBe(false);
  });

  it("a crash or failure the user has already seen is not cycled to", async () => {
    useViewerStore.setState({ aiStatusBySession: { t2: "error", t3: "error" } });
    useAgentStateStore.setState({ stateSince: { t2: 1, t3: 2 }, seenAt: { t2: 5 } });
    render(<TerminalArea />);
    terminal("t1").focus();
    pressJump();
    await waitFor(() => expect(activeIn(WS)).toBe("t3"));
  });

  it("moves FORWARD past a crash it just landed on (which the landing marks seen): Q1 → C2 → Q3", async () => {
    seed({ other: true });
    useViewerStore.setState((st) => ({
      workspaces: { [TEAM]: [...st.workspaces[TEAM], story(THIRD), story(FOURTH)] },
      terminalVisible: true,
      aiStatusBySession: { x1: "error" },
    }));
    seedTerminalTabs(TEAM, THIRD, [named("x1")]);
    seedTerminalTabs(TEAM, FOURTH, [named("z1")]);
    useManagedSessionStore.setState({
      pendingAsks: { "ask-user-1": ask("ask-user-1", "o1"), "ask-user-2": ask("ask-user-2", "z1") },
    });
    function MarksSeen() {
      useMarkActiveSessionSeen();
      return null;
    }
    render(
      <>
        <MarksSeen />
        <TerminalArea />
      </>,
    );
    act(() => since({ o1: 1, x1: 2, z1: 3 }));
    terminal("t1").focus();

    pressJump(); // Q1
    await waitFor(() => expect(banner(OTHER)?.contains(document.activeElement)).toBe(true));
    pressJump(); // C2 — landing on it marks it seen: still stuck, but no longer news
    await waitFor(() => expect(document.activeElement).toBe(terminal("x1")));
    await waitFor(() => expect(useAgentStateStore.getState().seenAt.x1).toBeDefined());
    expect(getSessionNeedsYou("x1")).toBe(true);
    pressJump(); // Q3, not back to Q1
    await waitFor(() => expect(banner(FOURTH)?.contains(document.activeElement)).toBe(true));
    expect(s().activeWorkspace[TEAM]).toBe(FOURTH);
  });

  it("the focused tile's question over a collapsed terminal pane: the pane opens before focus goes in", async () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      terminalVisible: false,
    });
    useManagedSessionStore.setState({ pendingAsks: { "ask-user-1": ask("ask-user-1", "t1") } });
    render(<TerminalArea />);
    terminal("t1").focus();
    pressJump();
    await waitFor(() => expect(banner(WS)?.contains(document.activeElement)).toBe(true));
    expect(s().layoutMode).not.toBe("viewer");
  });

  it("starts with the session the Jump pill is offering, ahead of the list's order", async () => {
    useViewerStore.setState({
      aiStatusBySession: { t2: "needs-permission", t3: "needs-permission" },
    });
    since({ t2: 1, t3: 2 });
    pending.session = "t3";
    render(<TerminalArea />);
    terminal("t1").focus();
    pressJump();
    await waitFor(() => expect(activeIn(WS)).toBe("t3"));
  });

  it("ignores an offered session that no longer needs you", async () => {
    useViewerStore.setState({ aiStatusBySession: { t2: "needs-permission" } });
    pending.session = "t3";
    render(<TerminalArea />);
    terminal("t1").focus();
    pressJump();
    await waitFor(() => expect(activeIn(WS)).toBe("t2"));
  });

  it("does nothing when nothing needs you", async () => {
    render(<TerminalArea />);
    terminal("t1").focus();
    pressJump();
    await settle();
    expect(activeIn(WS)).toBe("t1");
  });

  it("the ⌘⌥J variant is gone — the chord does nothing", async () => {
    useViewerStore.setState({ aiStatusBySession: { t3: "needs-permission" } });
    render(<TerminalArea />);
    terminal("t1").focus();
    press({ key: "∆", code: "KeyJ", metaKey: true, altKey: true });
    await settle();
    expect(ptyKeys).toEqual(["∆"]);
    ptyKeys = [];
    expect(activeIn(WS)).toBe("t1");
  });

  it("Weft's ⌘⌥⇧J runs the same jump when the Activity board isn't mounted", async () => {
    useViewerStore.setState({ aiStatusBySession: { t3: "needs-permission" } });
    render(<TerminalArea />);
    act(() => SHORTCUT_ACTIONS.find((a) => a.id === "weft.jump-to-next-needs-you")!.run());
    await waitFor(() => expect(activeIn(WS)).toBe("t3"));
  });

  it("Weft's ⌘⌥⇧J uses the board's own jump when the board is mounted", async () => {
    const boardJump = vi.fn();
    useWeftJumpStore.setState({ jumpToNextNeedsYou: boardJump });
    useViewerStore.setState({ aiStatusBySession: { t3: "needs-permission" } });
    render(<TerminalArea />);
    act(() => SHORTCUT_ACTIONS.find((a) => a.id === "weft.jump-to-next-needs-you")!.run());
    await waitFor(() => expect(boardJump).toHaveBeenCalledTimes(1));
    expect(activeIn(WS)).toBe("t1");
  });
});

describe("a tab closes from the keyboard with Delete; the × is the mouse's", () => {
  it("Delete on a focused tab takes its close verb; the × is out of the tab order", async () => {
    render(<TerminalArea />);
    const t1 = screen.getByRole("tab", { name: /^t1\b/ });
    t1.focus();
    await userEvent.tab();
    expect(document.activeElement).not.toHaveAccessibleName(/^(Hide|End) t1$/);
    t1.focus();
    await userEvent.keyboard("{Delete}");
    await waitFor(() => expect(tabsIn(WS)).toEqual(["t2", "t3"]));
  });
});
