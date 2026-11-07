// SPDX-License-Identifier: MIT
/**
 * Seam test: minimize/restore, Hide/reopen, End and Restart on the terminal
 * manager. The real panel tree, `ShellTerminalPanel` + `TerminalSlot`, close
 * verbs, terminal manager and a real `@xterm/xterm` display per session; only
 * the Tauri boundary is faked.
 *
 * Minimize and Hide park a display and never dispose it; End disposes it once;
 * Restart rebuilds it once, keeping the old output above the restart seam.
 */
import { act, cleanup, render } from "@/test-utils";
import { DndContext } from "@dnd-kit/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  bus: null as unknown as import("@/shared/terminal/__test-utils__/fake-terminal-server").FakeEventBus,
  server:
    null as unknown as import("@/shared/terminal/__test-utils__/fake-terminal-server").FakeTerminalServer,
  created: 0,
  disposed: 0,
}));

vi.mock("@xterm/xterm", async (importOriginal) => {
  const xterm = await importOriginal<typeof import("@xterm/xterm")>();
  class CountedTerminal extends xterm.Terminal {
    constructor(...args: ConstructorParameters<typeof xterm.Terminal>) {
      super(...args);
      h.created++;
    }
    override dispose(): void {
      h.disposed++;
      super.dispose();
    }
  }
  return { ...xterm, Terminal: CountedTerminal };
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
}));
vi.mock("@/features/viewer/components/source-editor-inset", () => ({
  WorktreeEditorSurface: () => null,
}));
vi.mock("@/features/dashboard/components/markdown-viewer", () => ({ MarkdownViewer: () => null }));
vi.mock("@/shared/lib/pipeline-graph/use-story-details", () => ({
  useStoryDetails: vi.fn(() => ({ data: { title: null, storyText: "" }, loading: false })),
}));

import { fireEvent } from "@testing-library/react";
import {
  FakeEventBus,
  FakeTerminalServer,
} from "@/shared/terminal/__test-utils__/fake-terminal-server";
import { headlessFrom } from "@/shared/terminal/__test-utils__/headless-from";
import { chunked, paneSteps, type PaneStep } from "@/shared/terminal/__test-utils__/pane-feed";
import { loadPtylog, type PtylogFixture } from "@/shared/terminal/__fixtures__/ptylog";
import type { ReferenceBuffer } from "@/shared/terminal/buffer-dump";
import { terminalManager } from "@/shared/terminal/terminal-manager";
import { rememberTerminalSize } from "@/shared/lib/terminal-open";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";
import { useTerminalDisplayWarm } from "../hooks/use-terminal-display-warm";
import { WorkspaceTerminalPanels } from "../components/workspace-terminal-panels";
import { endEndedTab, endTabs, hideTab, hideWorkspace } from "./terminal-close";

installTerminalLayoutHarness();

const SEPARATOR = "─── session restarted ───";
// What the host's emulator worker writes between the old normal buffer and
// the new process's output on a respawn.
const seam = `\r\n\x1b[90m${SEPARATOR}\x1b[0m\r\n`;

const TEAM = "acme:default";
const A = "ws-a";
const B = "ws-b";
const FIXTURE: Record<string, PtylogFixture> = {
  codex: "codex-tui",
  claude: "claude-ink",
  shell: "append-only",
  other: "append-only",
};
const SESSIONS = Object.keys(FIXTURE);
const CHUNKS = 4;

const s = () => useViewerStore.getState();
const flush = () => act(async () => new Promise((r) => setTimeout(r, 20)));
const wait = (ms: number) => act(async () => new Promise((r) => setTimeout(r, ms)));

let feeds: Record<string, PaneStep[][]>;

function feed(i: number, sessions = SESSIONS) {
  act(() => {
    for (const session of sessions) {
      for (const step of feeds[session][i] ?? []) {
        if (step.kind === "output") h.server.produce(session, step.data);
        else terminalManager.requestResize(session, step.cols, step.rows);
      }
    }
  });
}

function seed() {
  useViewerStore.setState((st) => ({
    settings: { ...st.settings, terminalScrollback: 1000 },
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: A },
    terminalVisible: true,
    aiStatusBySession: {},
    softClosedWorkspaces: {},
    workspaces: {
      [TEAM]: [
        { id: A, kind: "story", cwd: "/a", status: "ready" },
        { id: B, kind: "story", cwd: "/b", status: "ready" },
      ],
    },
  }));
  seedTerminalTabs(TEAM, A, ["codex", "claude", "shell"], { active: "codex" });
  seedTerminalTabs(TEAM, B, ["other"], { active: "other" });
}

function DisplayWarm() {
  useTerminalDisplayWarm();
  return null;
}

function renderApp() {
  return render(
    <DndContext>
      <DisplayWarm />
      <div data-xterm-host data-repo-path="/repo">
        <WorkspaceTerminalPanels teamKey={TEAM} repoPath="/repo" />
      </div>
    </DndContext>,
  );
}

const instanceOf = async (session: string) => (await terminalManager.debugDump(session)).instanceId;

async function bufferOf(session: string): Promise<ReferenceBuffer> {
  const {
    instanceId: _i,
    created: _c,
    rebuilds: _r,
    restoredFromSnapshot: _s,
    appliedSeq: _a,
    appliedResizeGen: _g,
    ...buffer
  } = await terminalManager.debugDump(session);
  return buffer;
}

/** Every line of the session's buffer, trailing blanks dropped. */
async function linesOf(session: string): Promise<string[]> {
  await terminalManager.debugDump(session);
  const buffer = terminalManager.terminal(session)!.buffer.active;
  const lines: string[] = [];
  for (let i = 0; i < buffer.length; i++) {
    lines.push((buffer.getLine(i)?.translateToString(true) ?? "").trimEnd());
  }
  while (lines.length && lines[lines.length - 1] === "") lines.pop();
  return lines;
}

beforeEach(() => {
  h.bus = new FakeEventBus();
  h.server = new FakeTerminalServer(h.bus);
  for (const session of SESSIONS) h.server.open(session);
  h.created = 0;
  h.disposed = 0;
  rememberTerminalSize(100, 30);
  feeds = Object.fromEntries(
    SESSIONS.map((session) => [session, chunked(paneSteps(loadPtylog(FIXTURE[session])), CHUNKS)]),
  );
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  seed();
});

afterEach(() => {
  cleanup();
  for (const session of SESSIONS) terminalManager.dispose(session);
  vi.restoreAllMocks();
});

/** Park `sessions` with `park`, stream the rest of their recordings, bring them
 *  back with `unpark`; the displays must be the same ones, current and exact. */
async function expectParked(
  sessions: string[],
  park: () => void,
  unpark: () => void,
  hiddenMs: number,
) {
  const view = renderApp();
  await flush();
  const instances = Object.fromEntries(
    await Promise.all(SESSIONS.map(async (id) => [id, await instanceOf(id)])),
  );
  feed(0);
  await flush();
  act(park);
  await flush();
  for (let i = 1; i < CHUNKS; i++) feed(i);
  await wait(hiddenMs);
  act(unpark);
  await flush();

  expect(h.created).toBe(SESSIONS.length);
  expect(h.disposed).toBe(0);
  for (const id of SESSIONS) expect(await instanceOf(id)).toBe(instances[id]);
  for (const id of sessions) expect(await bufferOf(id)).toEqual(await headlessFrom(FIXTURE[id]));
  return view;
}

describe("minimize and Hide park the display", () => {
  it.each([
    ["immediately", 0],
    ["after 2 s", 2000],
  ])(
    "minimize_restore_parks (%s)",
    async (_label, hiddenMs) => {
      await expectParked(
        ["codex", "claude", "shell"],
        () => hideWorkspace(TEAM, A),
        () => s().reopenSoftClosedWorkspace(TEAM, A),
        hiddenMs,
      );
      expect(document.activeElement).toBe(terminalManager.terminal("codex")!.textarea);
    },
    20_000,
  );

  it("a scrolled-up view keeps its line across minimize/restore", async () => {
    renderApp();
    await flush();
    act(() => s().setActiveTerminalTab(TEAM, "shell", A));
    for (let i = 0; i < CHUNKS; i++) feed(i, ["shell"]);
    await flush();
    const shell = terminalManager.terminal("shell")!;
    act(() => shell.scrollLines(-3));
    await wait(300);
    const viewportY = shell.buffer.active.viewportY;
    act(() => hideWorkspace(TEAM, A));
    await flush();
    act(() => s().reopenSoftClosedWorkspace(TEAM, A));
    await flush();
    expect(shell.buffer.active.viewportY).toBe(viewportY);
  }, 20_000);

  it("hide_parks_never_disposes: a hidden tab and a hidden tile come back on their displays", async () => {
    await expectParked(
      ["codex", "other"],
      () => {
        hideTab(TEAM, A, "codex");
        hideWorkspace(TEAM, B);
      },
      () => {
        s().restoreHiddenTerminalTab(TEAM, A, "codex");
        s().reopenSoftClosedWorkspace(TEAM, B);
      },
      5000,
    );
  }, 20_000);
});

describe("End disposes the display once", () => {
  it("End on a running tab", async () => {
    renderApp();
    await flush();
    await act(() => endTabs(TEAM, A, { tabId: "shell" }));
    await flush();
    expect(h.disposed).toBe(1);
    expect(terminalManager.terminal("shell")).toBeNull();
    expect(h.created).toBe(SESSIONS.length);
  }, 20_000);

  it("closing a tab whose process exited", async () => {
    renderApp();
    await flush();
    act(() => terminalHost().exit("claude", 0));
    await flush();
    act(() => endEndedTab(TEAM, A, "claude"));
    await flush();
    expect(h.disposed).toBe(1);
    expect(terminalManager.terminal("claude")).toBeNull();
    expect(h.created).toBe(SESSIONS.length);
  }, 20_000);
});

describe("Restart rebuilds once", () => {
  it("respawn_rebuilds_once and respawn_keeps_scrollback_below_separator", async () => {
    // The host's terminal keeps the ended output and marks the seam.
    const respawnTab = terminalHost().respawnTab.bind(terminalHost());
    vi.spyOn(terminalHost(), "respawnTab").mockImplementation((...args) => {
      const answer = respawnTab(...args);
      h.server.produce("shell", seam + "new$ ");
      return answer;
    });
    const view = renderApp();
    await flush();
    act(() => s().setActiveTerminalTab(TEAM, "shell", A));
    act(() => h.server.produce("shell", "old one\r\nold two\r\nold$ exit"));
    await flush();
    const before = await terminalManager.debugDump("shell");
    act(() => terminalHost().exit("shell", 0));
    await flush();

    const { writePty } = await import("@/tauri/commands");
    vi.mocked(writePty).mockClear();
    const restart = view.getByTestId("terminal-ended-restart");
    act(() => restart.focus());
    fireEvent.click(restart);
    await flush();
    await flush();

    const after = await terminalManager.debugDump("shell");
    expect(after.instanceId).not.toBe(before.instanceId);
    expect(after.rebuilds).toBe(before.rebuilds + 1);
    expect(h.created).toBe(SESSIONS.length + 1);
    expect(h.disposed).toBe(1);
    expect(view.queryByTestId("terminal-ended-bar")).toBeNull();

    const lines = await linesOf("shell");
    expect(lines).toEqual(["old one", "old two", "old$ exit", SEPARATOR, "new$"]);
    expect(document.activeElement).toBe(terminalManager.terminal("shell")!.textarea);
    // The click that pressed Restart typed nothing into the new shell.
    expect(vi.mocked(writePty).mock.calls.filter(([id]) => id === "shell")).toEqual([]);
  }, 20_000);
});

describe("teeth", () => {
  it("a Hide that disposes the display is caught", async () => {
    const hide = s().hideTerminalTab;
    useViewerStore.setState({
      hideTerminalTab: (teamKey, tabId, workspaceId) => {
        terminalManager.dispose(tabId);
        return hide(teamKey, tabId, workspaceId);
      },
    });
    try {
      await expect(
        expectParked(
          ["codex"],
          () => hideTab(TEAM, A, "codex"),
          () => s().restoreHiddenTerminalTab(TEAM, A, "codex"),
          0,
        ),
      ).rejects.toThrow();
    } finally {
      useViewerStore.setState({ hideTerminalTab: hide });
    }
  }, 20_000);
});
