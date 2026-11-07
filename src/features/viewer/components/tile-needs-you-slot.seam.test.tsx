// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, act, cleanup, fireEvent, within, waitFor } from "@/test-utils";
import userEvent from "@testing-library/user-event";
import { DndContext } from "@dnd-kit/core";

// Seam: the real `mcp:ask-user` / `pty-exit` listeners (useManagedSessionBridge),
// the real managed-session store, the real tile frame (WorkspaceTerminalPanels),
// the real ManagedQuestionPanel and the real status selectors the tab dot and the
// tray chip read. Only the Tauri boundary (event bus + resolve command) and the
// xterm leaf are stubbed.

const bus = vi.hoisted(() => ({
  handlers: {} as Record<string, Set<(e: { payload: unknown }) => void>>,
  terminalKeys: [] as string[],
}));

vi.mock("@tauri-apps/api/event", () => ({
  // Like Tauri, every listener of an event hears it: `pty-exit` has more than one
  // subscriber once the tiles mount.
  listen: vi.fn((name: string, cb: (e: { payload: unknown }) => void) => {
    const set = (bus.handlers[name] ??= new Set());
    set.add(cb);
    return Promise.resolve(() => void set.delete(cb));
  }),
  emit: vi.fn(() => Promise.resolve()),
}));

vi.mock("./shell-terminal-panel", () => ({
  ShellTerminalPanel: ({ sessionId, visible }: { sessionId: string; visible: boolean }) => (
    <div className="xterm" style={{ display: visible ? "block" : "none" }}>
      <textarea
        data-sid={sessionId}
        aria-label={`terminal ${sessionId}`}
        onKeyDown={(e) => bus.terminalKeys.push(e.key)}
      />
    </div>
  ),
}));

vi.mock("@/tauri/commands", () => ({
  // Each team draws its agents with its own faces.
  avatarPacksListForTeam: vi.fn().mockResolvedValue({ packs: [], warnings: [] }),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  // The workspace's terminal chooser loads the team's machines.
  listMachines: vi.fn().mockResolvedValue([]),
  resolveAskUser: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("./source-editor-inset", () => ({ WorktreeEditorSurface: () => null }));
vi.mock("@/shared/lib/default-agent", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/shared/lib/default-agent")>()),
  resolveAgentCliPath: () => Promise.resolve(null),
}));
vi.mock("@/features/dashboard/components/markdown-viewer", () => ({ MarkdownViewer: () => null }));
vi.mock("@/shared/lib/pipeline-graph/use-story-details", () => ({
  useStoryDetails: vi.fn(() => ({ data: { title: null, storyText: "" }, loading: false })),
}));

import {
  selectActiveTerminalTab,
  useViewerStore,
  type TerminalAiStatus,
} from "@/shared/stores/viewer-store";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { useOrgStore } from "@/shared/stores/org-store";
import {
  useAgentStateStore,
  resetPrunedAgentSessionsForTests,
} from "@/shared/stores/agent-state-store";
import {
  useManagedSessionStore,
  resetManagedSessionStoreForTests,
} from "@/shared/stores/use-managed-session-store";
import { answerAskUser, useManagedSessionBridge } from "@/shared/hooks/use-managed-session-bridge";
import { ManagedQuestionPanel } from "@/shared/components/managed-question-panel";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { markOfTerminalStatus } from "@/shared/lib/status-mark";
import * as commands from "@/tauri/commands";
import { WorkspaceTerminalPanels } from "./workspace-terminal-panels";
import { WITHDRAWN_NOTE_MS, pickAskingTab } from "./tile-needs-you-slot";
import { focusTileQuestion } from "../lib/tile-question-focus";

const resolveAskUser = commands.resolveAskUser as ReturnType<typeof vi.fn>;

const TEAM = "acme:default";
const A = "e-1-a";
const B = "e-1-b";
const C = "e-1-c";

installTerminalLayoutHarness();

function seedStore() {
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: A },
    cliExpanded: false,
    terminalVisible: true,
    aiStatusBySession: {},
    workspaces: {
      [TEAM]: [
        { id: A, kind: "story", cwd: "/a", status: "ready" },
        { id: B, kind: "story", cwd: "/b", status: "ready" },
        { id: C, kind: "story", cwd: "/c", status: "ready" },
      ],
    },
  });
  // Each tab is its session in the host (the tab id is the session id).
  seedTerminalTabs(
    TEAM,
    A,
    [
      { session: "sid-a", name: "claude" },
      { session: "sid-a2", name: "shell" },
    ],
    { active: "sid-a" },
  );
  seedTerminalTabs(TEAM, B, [{ session: "sid-b", name: "b-agent" }]);
  seedTerminalTabs(TEAM, C, [{ session: "sid-c", name: "c-agent" }]);
}

const activeTab = (workspaceId: string) =>
  selectActiveTerminalTab(useViewerStore.getState(), TEAM, workspaceId);

function Bridge() {
  useManagedSessionBridge();
  return null;
}

function renderApp(extra?: React.ReactNode) {
  return render(
    <div data-xterm-host>
      <DndContext>
        <Bridge />
        <WorkspaceTerminalPanels teamKey={TEAM} repoPath="/repo" />
        {extra}
      </DndContext>
    </div>,
  );
}

function fire(name: string, payload: unknown) {
  const set = bus.handlers[name];
  if (!set?.size) throw new Error(`no handler registered for ${name}`);
  for (const cb of [...set]) cb({ payload });
}

let askSeq = 0;
function askUser(sessionId: string, extra: Record<string, unknown> = {}): string {
  const correlationId = `ask-user-${++askSeq}`;
  act(() => {
    fire("mcp:ask-user", { correlationId, sessionId, question: "Ship it?", ...extra });
    projectSessionStatus(sessionId, "awaiting-answer");
  });
  return correlationId;
}

/** Rust ended the ask without an answer (the client cancelled / it timed out). */
function askClosed(sessionId: string, correlationId: string, reason: string) {
  act(() => {
    fire("mcp:ask-user-closed", { sessionId, correlationId, reason });
  });
}

const pressChord = () =>
  act(() => SHORTCUT_ACTIONS.find((a) => a.id === "viewer.focus-tile-question")!.run());

const tile = (id: string) => document.querySelector<HTMLElement>(`[data-pane-id="${id}"]`)!;
const banner = (id: string) =>
  tile(id).querySelector<HTMLElement>(`[data-testid="tile-question-${id}"]`);
const terminal = (sid: string) => document.querySelector<HTMLElement>(`[data-sid="${sid}"]`)!;
const tabDot = (tabId: string) =>
  document.querySelector<HTMLElement>(`[data-testid="tab-${tabId}-ai-status"]`);
const dotClass = (status: TerminalAiStatus) => `cs-mark-${markOfTerminalStatus(status)}`;
const firstOption = (id: string) => within(banner(id)!).getAllByTestId("ask-option")[0]!;

beforeEach(() => {
  resetSessionProjection();
  resetPrunedAgentSessionsForTests();
  useAgentStateStore.setState({ bySession: {}, activityBySession: {}, exitBySession: {} });
  askSeq = 0;
  bus.handlers = {};
  bus.terminalKeys = [];
  resolveAskUser.mockClear();
  resetManagedSessionStoreForTests();
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  seedStore();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("in-tile question banner — seam", () => {
  it("an ask_user on a tab's session renders in THAT tile only, between chrome and terminal", () => {
    renderApp();
    expect(banner(A)).toBeNull();

    askUser("sid-a", { options: ["yes", "no"] });

    const b = banner(A)!;
    expect(within(b).getByText("Ship it?")).toBeTruthy();
    expect(within(b).getByText(/claude is waiting for your answer/)).toBeTruthy();
    expect(within(b).getByTestId("tile-question-chord").textContent).toMatch(/to answer/);
    expect(banner(B)).toBeNull();
    expect(banner(C)).toBeNull();
    const body = tile(A).querySelector(`[data-testid="workspace-panels-${A}"]`)!;
    expect(body.contains(b)).toBe(false);
    expect(b.nextElementSibling).toBe(body);
    expect(useManagedSessionStore.getState().states["sid-a"]).toBe("awaiting-answer");
  });

  it("a pending question marks the tab Needs answer, like its agents-list row", () => {
    renderApp();
    act(() => {
      projectSessionStatus("sid-a", "thinking");
    });
    expect(tabDot("sid-a")?.className).toContain(dotClass("thinking"));
    askUser("sid-a", { options: ["yes"] });
    expect(banner(A)).not.toBeNull();
    // a pending `ask_user` projects to the specific `question` mark —
    // the `?` glyph — not the generic `permission` lock.
    expect(tabDot("sid-a")?.className).toContain("cs-mark-question");
    expect(tabDot("sid-a")?.getAttribute("aria-label")).toMatch(/^Needs answer/);
  });

  it("typing into the terminal while a question arrives never answers it or moves focus", async () => {
    const user = userEvent.setup();
    renderApp();
    terminal("sid-a").focus();

    askUser("sid-a", { options: ["yes", "no"] });
    await user.keyboard(" 1{Enter}2 ");
    askUser("sid-a", { question: "Why?" }); // a free-text one queued behind it
    await user.keyboard("abc{Enter}");

    expect(document.activeElement).toBe(terminal("sid-a"));
    expect(resolveAskUser).not.toHaveBeenCalled();
    expect(banner(A)).not.toBeNull();
    expect(Object.keys(useManagedSessionStore.getState().pendingAsks)).toHaveLength(2);
  });

  it("the chord moves focus in, a number key answers, and focus returns to the terminal", () => {
    renderApp();
    terminal("sid-a").focus();
    const id = askUser("sid-a", { options: ["yes", "no"] });

    pressChord();
    expect(document.activeElement).toBe(firstOption(A));
    expect(within(banner(A)!).queryByTestId("tile-question-chord")).toBeNull();
    fireEvent.keyDown(document.activeElement!, { key: "2" });

    expect(resolveAskUser).toHaveBeenCalledWith(id, "accept", "no");
    expect(banner(A)).toBeNull();
    expect(document.activeElement).toBe(terminal("sid-a"));
  });

  it("arrows move between choices and Enter on a choice answers it", () => {
    renderApp();
    const id = askUser("sid-a", { options: ["yes", "no", "later"] });
    pressChord();
    const options = within(banner(A)!).getAllByTestId("ask-option");

    fireEvent.keyDown(document.activeElement!, { key: "ArrowRight" });
    expect(document.activeElement).toBe(options[1]);
    fireEvent.keyDown(document.activeElement!, { key: "ArrowLeft" });
    fireEvent.keyDown(document.activeElement!, { key: "ArrowLeft" });
    expect(document.activeElement).toBe(options[2]);

    // A focused <button> activates on Enter natively; the banner must not eat it.
    fireEvent.keyDown(document.activeElement!, { key: "Enter" });
    fireEvent.click(document.activeElement!);
    expect(resolveAskUser).toHaveBeenCalledWith(id, "accept", "later");
    expect(document.activeElement).toBe(terminal("sid-a"));
  });

  it("Esc goes back to the terminal WITHOUT answering; the question stays pending", () => {
    renderApp();
    askUser("sid-a", { options: ["yes", "no"] });
    pressChord();
    expect(banner(A)!.contains(document.activeElement)).toBe(true);

    fireEvent.keyDown(document.activeElement!, { key: "Escape" });

    expect(document.activeElement).toBe(terminal("sid-a"));
    expect(resolveAskUser).not.toHaveBeenCalled();
    expect(banner(A)).not.toBeNull();
  });

  it("a free-text answer is sent with Enter (Shift+Enter does not send)", () => {
    renderApp();
    const id = askUser("sid-a");
    pressChord();
    const field = within(banner(A)!).getByLabelText("Your answer");
    expect(document.activeElement).toBe(field);

    fireEvent.change(field, { target: { value: "use the staging db" } });
    fireEvent.keyDown(field, { key: "Enter", shiftKey: true });
    expect(resolveAskUser).not.toHaveBeenCalled();
    fireEvent.keyDown(field, { key: "Enter" });

    expect(resolveAskUser).toHaveBeenCalledWith(id, "accept", "use the staging db");
    expect(banner(A)).toBeNull();
    expect(document.activeElement).toBe(terminal("sid-a"));
  });

  it("the chord prefers the focused tile's question over an older one elsewhere", () => {
    renderApp();
    terminal("sid-a").focus();
    askUser("sid-b", { options: ["b"] });
    askUser("sid-a", { options: ["a"] });

    pressChord();
    expect(document.activeElement).toBe(firstOption(A));
  });

  it("with no question in the focused tile, the chord cycles the shared needs-you list (oldest dot first, else tile order)", async () => {
    renderApp();
    terminal("sid-a").focus();
    askUser("sid-c", { options: ["c"] });
    askUser("sid-b", { options: ["b"] });

    // Neither dot has a tracked start, so the list keeps tile order: A → B → C.
    pressChord();
    await waitFor(() => expect(document.activeElement).toBe(firstOption(B)));
    pressChord(); // from B's banner → C
    await waitFor(() => expect(document.activeElement).toBe(firstOption(C)));
  });

  it("the chord skips banners in zoom-collapsed tiles (even older ones) for a visible one", () => {
    renderApp();
    act(() => useViewerStore.getState().expandWorkspace(TEAM, A));
    askUser("sid-b", { options: ["b"] }); // oldest, but collapsed by A's zoom
    askUser("sid-c", { options: ["c"] }); // collapsed too
    askUser("sid-a", { options: ["a"] }); // the only one on screen
    expect(tile(B).closest('[data-zoom="collapsed"]')).not.toBeNull();
    expect(tile(C).closest('[data-zoom="collapsed"]')).not.toBeNull();

    pressChord();
    expect(document.activeElement).toBe(firstOption(A));
  });

  it("when the only question is in a zoom-collapsed tile, the chord brings that tile on screen", async () => {
    renderApp();
    act(() => useViewerStore.getState().expandWorkspace(TEAM, A));
    askUser("sid-b", { options: ["b"] });
    expect(tile(B).closest('[data-zoom="collapsed"]')).not.toBeNull();

    pressChord();

    await waitFor(() => expect(document.activeElement).toBe(firstOption(B)));
    expect(tile(B).closest('[data-zoom="collapsed"]')).toBeNull();
  });

  it("a question withdrawn while you type your answer: the rest of your typing never reaches the terminal", async () => {
    const user = userEvent.setup();
    renderApp();
    const id = askUser("sid-a");
    pressChord();
    const field = within(banner(A)!).getByLabelText("Your answer");
    await user.type(field, "use the stag");

    askClosed("sid-a", id, "cancelled");
    await user.keyboard("ing db{Enter}");

    expect(bus.terminalKeys).toEqual([]);
    expect(document.activeElement).not.toBe(terminal("sid-a"));
    expect(tile(A).contains(document.activeElement)).toBe(true);
    expect(resolveAskUser).not.toHaveBeenCalled();
    expect(document.querySelector('[data-testid="tile-question-withdrawn"]')?.textContent).toMatch(
      /Question withdrawn/,
    );
  });

  it("withdrawn with another question queued: focus stays on the strip and Enter answers nothing", async () => {
    const user = userEvent.setup();
    renderApp();
    const first = askUser("sid-a");
    askUser("sid-a", { options: ["yes", "no"] });
    pressChord();
    await user.type(within(banner(A)!).getByLabelText("Your answer"), "abc");

    askClosed("sid-a", first, "timeout");
    await user.keyboard("d1{Enter}2");

    expect(document.activeElement).toBe(banner(A));
    expect(bus.terminalKeys).toEqual([]);
    expect(resolveAskUser).not.toHaveBeenCalled();
    expect(within(banner(A)!).getByTestId("tile-question-withdrawn")).toBeTruthy();
  });

  it("answered in the Agents panel while you are in the strip: focus stays in the tile", () => {
    renderApp(
      <div data-testid="agents-panel-card">
        <ManagedQuestionPanel sessionId="sid-a" />
      </div>,
    );
    const id = askUser("sid-a", { options: ["yes"] });
    pressChord();
    act(() => {
      void answerAskUser(id, { action: "accept", content: "yes" });
    });
    expect(banner(A)).toBeNull();
    expect(document.activeElement).toBe(tile(A));
  });

  it("the withdrawn note goes away on its own", () => {
    vi.useFakeTimers();
    try {
      renderApp();
      const id = askUser("sid-a", { options: ["yes"] });
      pressChord();
      askClosed("sid-a", id, "cancelled");
      expect(document.querySelector('[data-testid="tile-question-withdrawn"]')).not.toBeNull();
      act(() => vi.advanceTimersByTime(WITHDRAWN_NOTE_MS + 1));
      expect(document.querySelector('[data-testid="tile-question-withdrawn"]')).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("+N more keeps focus in the banner, on the next tab's question", () => {
    renderApp();
    askUser("sid-a", { options: ["yes"] });
    askUser("sid-a2", { options: ["other", "x"] });
    pressChord();
    expect(document.activeElement).toBe(firstOption(A));

    fireEvent.click(within(banner(A)!).getByTestId("tile-question-more"));

    expect(banner(A)!.getAttribute("data-session-id")).toBe("sid-a2");
    expect(document.activeElement).toBe(firstOption(A));
    expect(within(banner(A)!).queryByTestId("tile-question-withdrawn")).toBeNull();
  });

  it("the chord's hidden-tile path resolves only once focus really landed", async () => {
    renderApp();
    act(() => useViewerStore.getState().expandWorkspace(TEAM, A));
    askUser("sid-b", { options: ["b"] });

    // A reveal that doesn't bring the tile on screen → not reported as success.
    await expect(focusTileQuestion(A, () => {})).resolves.toBe(false);
    expect(banner(B)!.contains(document.activeElement)).toBe(false);

    let ok: boolean | undefined;
    await act(async () => {
      ok = await focusTileQuestion(A, (ws) => useViewerStore.getState().switchWorkspace(TEAM, ws));
    });
    expect(ok).toBe(true);
    expect(document.activeElement).toBe(firstOption(B));
  });

  it("answering in the Agents panel's card clears the in-tile banner (one pending question)", () => {
    renderApp(
      <div data-testid="agents-panel-card">
        <ManagedQuestionPanel sessionId="sid-a" />
      </div>,
    );
    askUser("sid-a", { options: ["yes", "no"] });
    expect(banner(A)).not.toBeNull();

    const panel = document.querySelector<HTMLElement>('[data-testid="agents-panel-card"]')!;
    fireEvent.click(within(panel).getAllByTestId("ask-option")[0]!);

    expect(banner(A)).toBeNull();
    expect(panel.querySelector('[data-testid="managed-question-panel"]')).toBeNull();
  });

  it.each(["cancelled", "timeout", "superseded"])(
    "an ask closed unanswered (%s) clears question UI while the managed session remains active",
    (reason) => {
      renderApp(
        <div data-testid="agents-panel-card">
          <ManagedQuestionPanel sessionId="sid-a" />
        </div>,
      );
      act(() => {
        projectSessionStatus("sid-a", "paused");
      }); // The current approval is independent of the pending question.
      const id = askUser("sid-a", { options: ["yes"] });
      pressChord();
      expect(banner(A)).not.toBeNull();

      askClosed("sid-a", id, reason);
      act(() => {
        projectSessionStatus("sid-a", "paused");
      });

      expect(banner(A)).toBeNull();
      expect(document.querySelector('[data-testid="managed-question-panel"]')).toBeNull();
      expect(useManagedSessionStore.getState().pendingAsks[id]).toBeUndefined();
      // Withdrawn under the user: focus parks on the tile, never the terminal input.
      expect(document.activeElement).toBe(tile(A));
      expect(document.querySelector('[data-testid="tile-question-withdrawn"]')).not.toBeNull();
      // No question overlay remains; current approval and activity still show.
      expect(tabDot("sid-a")?.className).toContain(dotClass("needs-permission"));
      act(() => {
        projectSessionStatus("sid-a", "unknown");
      });
      // A managed running outcome is not proof of observed activity. Closing
      // an ask without a hook or progress signal keeps the activity unknown.
      expect(tabDot("sid-a")).toHaveAttribute("data-mark", "unknown");
    },
  );

  it("after an answer the projected status stays unknown without activity evidence", () => {
    renderApp();
    act(() => {
      projectSessionStatus("sid-a", "paused");
    });
    const id = askUser("sid-a", { options: ["yes"] });
    pressChord();
    fireEvent.keyDown(document.activeElement!, { key: "1" });
    expect(resolveAskUser).toHaveBeenCalledWith(id, "accept", "yes");
    act(() => {
      projectSessionStatus("sid-a", "paused");
    });
    expect(banner(A)).toBeNull();
    expect(tabDot("sid-a")?.className).toContain(dotClass("needs-permission"));
    act(() => {
      projectSessionStatus("sid-a", "unknown");
    });
    expect(tabDot("sid-a")).toHaveAttribute("data-mark", "unknown");
  });

  it("answering a question that was already closed is harmless", async () => {
    renderApp();
    const id = askUser("sid-a", { options: ["yes"] });
    const other = askUser("sid-b", { options: ["b"] });
    askClosed("sid-a", id, "timeout");
    expect(banner(A)).toBeNull();

    await expect(answerAskUser(id, { action: "accept", content: "yes" })).resolves.toBeUndefined();
    expect(useManagedSessionStore.getState().pendingAsks[other]).toBeDefined();
    expect(banner(B)).not.toBeNull();
  });

  it("a question from a background tab shows in the tile with a Show tab action", () => {
    renderApp();
    askUser("sid-a2", { options: ["yes"] });

    const b = banner(A)!;
    expect(b.getAttribute("data-session-id")).toBe("sid-a2");
    fireEvent.click(within(b).getByTestId("tile-question-show-tab"));
    expect(activeTab(A)).toBe("sid-a2");
  });

  it("shows ONE count, and +N more jumps to the next waiting tab", () => {
    renderApp();
    askUser("sid-a", { options: ["yes"] });
    askUser("sid-a", { options: ["again"] });
    askUser("sid-a2", { options: ["other"] });

    const b = banner(A)!;
    expect(b.textContent?.match(/\+\d+ more/g)).toEqual(["+2 more"]);
    fireEvent.click(within(b).getByTestId("tile-question-more"));
    expect(activeTab(A)).toBe("sid-a2");
    expect(banner(A)!.getAttribute("data-session-id")).toBe("sid-a2");
  });

  it("clears when the agent's session dies", () => {
    renderApp();
    askUser("sid-a", { options: ["yes"] });
    expect(banner(A)).not.toBeNull();

    act(() => fire("pty-exit", { session_id: "sid-a" }));

    expect(banner(A)).toBeNull();
  });
});

describe("pickAskingTab", () => {
  const tabs = [
    { id: "t1", name: "one", sessionId: "s1" },
    { id: "t2", name: "two", sessionId: "s2" },
    { id: "t3", name: "three", sessionId: "s3" },
  ];
  const ask = (correlationId: string, sessionId: string) => ({
    correlationId,
    sessionId,
    question: "q",
  });

  it("prefers the front tab, else the longest-waiting (numeric id order)", () => {
    const pending = {
      "ask-user-10": ask("ask-user-10", "s2"),
      "ask-user-9": ask("ask-user-9", "s3"),
    };
    expect(pickAskingTab(tabs, "t1", pending)).toEqual({
      tab: tabs[2],
      moreCount: 1,
      nextTab: tabs[1],
    });
    expect(pickAskingTab(tabs, "t2", pending)).toEqual({
      tab: tabs[1],
      moreCount: 1,
      nextTab: tabs[2],
    });
    expect(pickAskingTab(tabs, "t1", {})).toBeNull();
    expect(pickAskingTab(tabs, "t1", { x: ask("x", "elsewhere") })).toBeNull();
  });
});
