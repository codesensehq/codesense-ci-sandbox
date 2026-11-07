// SPDX-License-Identifier: MIT
/**
 * Seam test: a Finder file dropped where an Agents pane popup covers a CLI tile goes to the
 * popup, never to the tile under it. The quick view's live terminal takes the drop for its own
 * session; the inbox chat is not a terminal, so a drop on it pastes nowhere. Neither lights the
 * covered tile.
 *
 * The real panel tree, `ShellTerminalPanel`, `TerminalSlot`, terminal manager, quick view and
 * `@xterm/xterm`; only the Tauri boundary (a terminal server, the webview-global drag-drop
 * event), the inbox reads, and layout (jsdom lays nothing out) are faked.
 */
import { act, cleanup, render } from "@/test-utils";
import { DndContext } from "@dnd-kit/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  bus: null as unknown as import("@/shared/terminal/__test-utils__/fake-terminal-server").FakeEventBus,
  server:
    null as unknown as import("@/shared/terminal/__test-utils__/fake-terminal-server").FakeTerminalServer,
  /** Every paste, with the terminal it went into. */
  pasted: [] as Array<{ term: unknown; data: string }>,
  dragDrop: [] as Array<(event: { payload: unknown }) => void>,
}));

vi.mock("@xterm/xterm", async (importOriginal) => {
  const xterm = await importOriginal<typeof import("@xterm/xterm")>();
  class RecordingTerminal extends xterm.Terminal {
    override paste(data: string): void {
      h.pasted.push({ term: this, data });
      super.paste(data);
    }
  }
  return { ...xterm, Terminal: RecordingTerminal };
});
vi.mock("@xterm/addon-webgl", () => ({
  WebglAddon: class {
    activate() {}
    dispose() {}
    onContextLoss() {
      return { dispose() {} };
    }
  },
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: (handler: (event: { payload: unknown }) => void) => {
      h.dragDrop.push(handler);
      return Promise.resolve(() => {
        h.dragDrop = h.dragDrop.filter((x) => x !== handler);
      });
    },
  }),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, handler: (event: { payload: unknown }) => void) =>
    h.bus.listen(name, handler),
  emit: vi.fn(),
}));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  sessionsRunningProcesses: vi.fn((ids: string[]) =>
    Promise.resolve(ids.map(() => ({ foreground: null, background: [] }))),
  ),
  reserveTerminalStream: () => h.server.reserve(),
  subscribeTerminal: (session: string, stream?: number) => h.server.subscribe(session, stream),
  unsubscribeTerminal: (stream: number) => h.server.unsubscribe(stream),
  writePty: vi.fn().mockResolvedValue(undefined),
  resizePty: vi.fn((session: string, cols: number, rows: number) =>
    h.server.resize(session, cols, rows),
  ),
  markInboxMessageRead: vi.fn(() => Promise.resolve()),
}));
vi.mock("@/features/inbox/hooks/use-session-inbox", () => ({
  useSessionInbox: (handle: string) => ({
    data: { handle, roles: [], messages: [] },
    source: "live",
    error: null,
    reload: vi.fn(),
  }),
}));
vi.mock("@/features/inbox/hooks/use-inbox-sessions", () => ({ useInboxSessions: () => [] }));
vi.mock("@/shared/lib/get-current-user", () => ({ useCurrentUser: () => null }));
vi.mock("./source-editor-inset", () => ({ WorktreeEditorSurface: () => null }));
vi.mock("@/features/dashboard/components/markdown-viewer", () => ({ MarkdownViewer: () => null }));
vi.mock("@/shared/lib/pipeline-graph/use-story-details", () => ({
  useStoryDetails: vi.fn(() => ({ data: { title: null, storyText: "" }, loading: false })),
}));

import {
  FakeEventBus,
  FakeTerminalServer,
} from "@/shared/terminal/__test-utils__/fake-terminal-server";
import { terminalManager } from "@/shared/terminal/terminal-manager";
import { rememberTerminalSize } from "@/shared/lib/terminal-open";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { TERMINAL_SESSION_ID_ATTR } from "../lib/terminal-tab-keys";
import { WorkspaceTerminalPanels } from "./workspace-terminal-panels";
import { GlobalCli } from "./global-cli";
import { QuickSendPopover } from "./agents-quick-send";
import { InboxChatPopover } from "./agents-inbox-chat";

installTerminalLayoutHarness();

const TEAM = "acme:default";
const WS = "ws-1";
/** The tab the CLI shows: its tile is under the popup. */
const SHOWN = "shown";
/** Another tab of the workspace, not shown: the quick view borrows its terminal. */
const OTHER = "other";
const DROPPED = "/Users/me/Screen Shot (1).png";
const ESCAPED = "/Users/me/Screen\\ Shot\\ \\(1\\).png";
/** Every box covers the whole webview (see the rect stub), so a drop here is inside each. */
const AT = { x: 50, y: 50 };

const flush = () => act(async () => new Promise((r) => setTimeout(r, 20)));

/** The element `document.elementFromPoint` reports as drawn on top at the drop point. */
let topmost: () => Element | null = () => null;

const tileOf = (session: string) =>
  document.querySelector<HTMLElement>(`[${TERMINAL_SESSION_ID_ATTR}="${session}"]`);

function seed() {
  useViewerStore.setState({
    foregroundLayout: {},
    openedTeams: [TEAM],
    activeWorkspace: { [TEAM]: WS },
    terminalVisible: true,
    aiStatusBySession: {},
    workspaces: { [TEAM]: [{ id: WS, kind: "story", cwd: "/repo", status: "ready" }] },
  });
  seedTerminalTabs(TEAM, WS, [SHOWN, OTHER], { active: SHOWN });
}

function drag(type: "over" | "drop") {
  act(() => {
    for (const handler of [...h.dragDrop]) {
      handler({ payload: { type, paths: [DROPPED], position: AT } });
    }
  });
}

/** The CLI, with the row button a popup anchors to, and `popup` drawn over it. */
async function renderWith(popup: (anchor: HTMLElement) => React.ReactNode) {
  const anchor = document.createElement("button");
  document.body.appendChild(anchor);
  render(
    <DndContext>
      <div data-xterm-host data-repo-path="/repo">
        <WorkspaceTerminalPanels teamKey={TEAM} shown repoPath="/repo" />
      </div>
      {popup(anchor)}
    </DndContext>,
  );
  await flush();
}

beforeEach(() => {
  h.bus = new FakeEventBus();
  h.server = new FakeTerminalServer(h.bus);
  h.server.open(SHOWN);
  h.server.open(OTHER);
  h.pasted = [];
  h.dragDrop = [];
  rememberTerminalSize(100, 30);
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    left: 0,
    top: 0,
    right: 800,
    bottom: 600,
    width: 800,
    height: 600,
    x: 0,
    y: 0,
    toJSON: () => ({}),
  } as DOMRect);
  topmost = () => null;
  document.elementFromPoint = () => topmost();
  seed();
});

afterEach(() => {
  cleanup();
  terminalManager.dispose(SHOWN);
  terminalManager.dispose(OTHER);
  vi.restoreAllMocks();
  document.body.innerHTML = "";
});

describe("a file dropped where a popup covers a CLI tile", () => {
  it("with nothing over the tile, the tile takes it (the baseline)", async () => {
    await renderWith(() => null);
    topmost = () => tileOf(SHOWN);
    drag("over");
    expect(tileOf(SHOWN)).toHaveClass("terminal-drag-over");
    drag("drop");
    expect(h.pasted).toEqual([{ term: terminalManager.terminal(SHOWN), data: ESCAPED }]);
  });

  it("over a covering element, the tile neither pastes nor lights", async () => {
    await renderWith(() => <div data-testid="cover" />);
    topmost = () => document.querySelector("[data-testid='cover']");
    drag("over");
    expect(tileOf(SHOWN)).not.toHaveClass("terminal-drag-over");
    drag("drop");
    expect(h.pasted).toEqual([]);
  });

  it("on the quick view's terminal, pastes into that agent's terminal and lights the popup", async () => {
    await renderWith((anchor) => (
      <QuickSendPopover
        name="Worker"
        statusWord="Working"
        workspace={null}
        sessionId={OTHER}
        anchor={anchor}
        onClose={() => {}}
        onShowInCli={() => {}}
      />
    ));
    const body = document.querySelector<HTMLElement>(".ag-quick-send-terminal");
    expect(body).not.toBeNull();
    topmost = () => body!.querySelector(".xterm") ?? body;
    drag("over");
    expect(body).toHaveClass("terminal-drag-over");
    expect(tileOf(SHOWN)).not.toHaveClass("terminal-drag-over");
    drag("drop");
    expect(body).not.toHaveClass("terminal-drag-over");
    expect(terminalManager.terminal(OTHER)).not.toBeNull();
    expect(h.pasted).toEqual([{ term: terminalManager.terminal(OTHER), data: ESCAPED }]);
  });

  it("on the inbox chat, pastes nowhere and lights nothing", async () => {
    await renderWith((anchor) => (
      <InboxChatPopover
        name="Worker"
        statusWord="Working"
        workspace={null}
        handle="worker"
        teamKey={TEAM}
        offlineMachine={null}
        anchor={anchor}
        onClose={() => {}}
        onOpenInCli={null}
      />
    ));
    const chat = document.querySelector<HTMLElement>("[role='dialog']");
    expect(chat).not.toBeNull();
    topmost = () => chat;
    drag("over");
    expect(document.querySelector(".terminal-drag-over")).toBeNull();
    drag("drop");
    expect(h.pasted).toEqual([]);
  });
});

describe("a file dropped on the one CLI across a team switch", () => {
  const OTHER_TEAM = "acme:other";
  /** The other team's shown tab. */
  const B_SHOWN = "b-shown";

  beforeEach(() => {
    h.server.open(B_SHOWN);
    useViewerStore.setState((st) => ({
      openedTeams: [TEAM, OTHER_TEAM],
      activeWorkspace: { ...st.activeWorkspace, [OTHER_TEAM]: "ws-b" },
      workspaces: {
        ...st.workspaces,
        [OTHER_TEAM]: [{ id: "ws-b", kind: "story", cwd: "/b", status: "ready" }],
      },
    }));
    seedTerminalTabs(OTHER_TEAM, "ws-b", [B_SHOWN], { active: B_SHOWN });
  });
  afterEach(() => terminalManager.dispose(B_SHOWN));

  function ActiveTeamCli() {
    const team = useOrgStore((o) => o.activeTeamName);
    return <GlobalCli teamKey={team ? `acme:${team}` : null} />;
  }

  async function renderCli(popup: (anchor: HTMLElement) => React.ReactNode = () => null) {
    const anchor = document.body.appendChild(document.createElement("button"));
    render(
      <DndContext>
        <ActiveTeamCli />
        {popup(anchor)}
      </DndContext>,
    );
    await flush();
  }

  it("after a switch, only the new team's tile under the pointer takes the drop", async () => {
    await renderCli();
    expect(tileOf(SHOWN)).not.toBeNull();
    act(() => useOrgStore.setState({ activeTeamName: "other" }));
    await flush();
    // The old team is warm: its tile stays mounted, hidden, and the pointer is never over it.
    expect(tileOf(SHOWN)!.closest<HTMLElement>("[data-cli-team]")!.style.display).toBe("none");
    topmost = () => tileOf(B_SHOWN);
    drag("over");
    expect(tileOf(B_SHOWN)).toHaveClass("terminal-drag-over");
    drag("drop");
    expect(h.pasted).toEqual([{ term: terminalManager.terminal(B_SHOWN), data: ESCAPED }]);
  });

  it("the quick view borrows the old team's off-screen terminal and takes a drop for it", async () => {
    await renderCli((anchor) => (
      <QuickSendPopover
        name="Worker"
        statusWord="Working"
        workspace={null}
        sessionId={SHOWN}
        anchor={anchor}
        onClose={() => {}}
        onShowInCli={() => {}}
      />
    ));
    act(() => useOrgStore.setState({ activeTeamName: "other" }));
    await flush();
    // The tile that held it is gone, so the quick view now shows the display.
    const body = document.querySelector<HTMLElement>(".ag-quick-send-terminal");
    expect(body?.contains(terminalManager.terminal(SHOWN)!.element!)).toBe(true);
    topmost = () => body!.querySelector(".xterm") ?? body;
    drag("drop");
    expect(h.pasted).toEqual([{ term: terminalManager.terminal(SHOWN), data: ESCAPED }]);
  });
});
