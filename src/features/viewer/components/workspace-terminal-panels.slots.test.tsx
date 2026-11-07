// SPDX-License-Identifier: MIT
/**
 * Seam test: tab switches, tile clicks, workspace switches, grid ⇄ tabs,
 * maximize/restore and mosaic presets are slot moves. The real panel tree, the
 * real `ShellTerminalPanel` + `TerminalSlot`, the real terminal manager and a
 * real `@xterm/xterm` display per session; only the Tauri boundary is faked (a
 * terminal server that streams recorded output on reserved streams).
 *
 * Every move must keep each session's one display: no `Terminal` built or
 * disposed after the first, the same element, and a buffer equal to a
 * headless xterm fed the same bytes and resizes in stream order.
 */
import { act, cleanup, fireEvent, render } from "@/test-utils";
import { DndContext } from "@dnd-kit/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const h = vi.hoisted(() => ({
  bus: null as unknown as import("@/shared/terminal/__test-utils__/fake-terminal-server").FakeEventBus,
  server:
    null as unknown as import("@/shared/terminal/__test-utils__/fake-terminal-server").FakeTerminalServer,
  created: 0,
  disposed: 0,
  resizePty: [] as string[],
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
  resizePty: vi.fn((session: string, cols: number, rows: number) => {
    h.resizePty.push(session);
    return h.server.resize(session, cols, rows);
  }),
}));
vi.mock("./source-editor-inset", () => ({ WorktreeEditorSurface: () => null }));
vi.mock("@/features/dashboard/components/markdown-viewer", () => ({ MarkdownViewer: () => null }));
vi.mock("@/shared/lib/pipeline-graph/use-story-details", () => ({
  useStoryDetails: vi.fn(() => ({ data: { title: null, storyText: "" }, loading: false })),
}));

import {
  FakeEventBus,
  FakeTerminalServer,
} from "@/shared/terminal/__test-utils__/fake-terminal-server";
import { headlessFrom } from "@/shared/terminal/__test-utils__/headless-from";
import { chunked, paneSteps, type PaneStep } from "@/shared/terminal/__test-utils__/pane-feed";
import { loadPtylog, type PtylogFixture } from "@/shared/terminal/__fixtures__/ptylog";
import type { ReferenceBuffer } from "@/shared/terminal/buffer-dump";
import { terminalManager, type DisplayOpts } from "@/shared/terminal/terminal-manager";
import { rememberTerminalSize } from "@/shared/lib/terminal-open";
import { useOrgStore } from "@/shared/stores/org-store";
import { writePty } from "@/tauri/commands";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";
import { useTerminalDisplayWarm } from "../hooks/use-terminal-display-warm";
import { WorkspaceTerminalPanels } from "./workspace-terminal-panels";

installTerminalLayoutHarness();

const TEAM = "acme:default";
const A = "ws-a";
const B = "ws-b";
/** Tile A runs a Codex and a Claude Code session plus an append-only shell;
 *  tile B one more shell. */
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

let feeds: Record<string, PaneStep[][]>;

/** Deliver chunk `i` of every session's recording, as its PTY would. */
function feed(i: number) {
  act(() => {
    for (const session of SESSIONS) {
      for (const step of feeds[session][i] ?? []) {
        if (step.kind === "output") h.server.produce(session, step.data);
        else terminalManager.requestResize(session, step.cols, step.rows);
      }
    }
  });
}

/** The reference (`headlessFrom`) keeps xterm's default 1 000 lines of history. */
const REFERENCE_SCROLLBACK = 1000;

function seed() {
  useViewerStore.setState((st) => ({
    settings: { ...st.settings, terminalScrollback: REFERENCE_SCROLLBACK },
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: A },
    terminalVisible: true,
    aiStatusBySession: {},
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

/** The app-level hook that gives every tab its display (App mounts it). */
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

const elementOf = (session: string) => terminalManager.terminal(session)?.element;

async function bufferOf(session: string): Promise<ReferenceBuffer> {
  const dump = await terminalManager.debugDump(session);
  const {
    instanceId: _i,
    created: _c,
    rebuilds: _r,
    restoredFromSnapshot: _s,
    appliedSeq: _a,
    appliedResizeGen: _g,
    ...buffer
  } = dump;
  return buffer;
}

/** Every move the story names, each followed by a chunk of output. */
async function runMoves(between: () => Promise<void>) {
  const moves: Array<() => void> = [
    () => s().setActiveTerminalTab(TEAM, "claude", A), // tab switch
    () => s().switchWorkspace(TEAM, B), // tile click
    () => s().switchWorkspace(TEAM, A),
    () => s().gridAllTerminalTabs(TEAM, A), // tabs → grid
    () => s().collapseTabGridToTabs(TEAM, A), // grid → tabs
    () => s().expandWorkspace(TEAM, A), // maximize
    () => s().setCliExpanded(false), // restore
    () => s().setForegroundPreset(TEAM, "rows"), // mosaic preset
    () => s().setActiveTerminalTab(TEAM, "shell", A),
    () => s().setActiveTerminalTab(TEAM, "codex", A),
  ];
  for (const [i, move] of moves.entries()) {
    act(move);
    await flush();
    if (i + 1 < CHUNKS) feed(i + 1);
    await between();
  }
}

beforeEach(async () => {
  h.bus = new FakeEventBus();
  h.server = new FakeTerminalServer(h.bus);
  for (const session of SESSIONS) h.server.open(session);
  h.created = 0;
  h.disposed = 0;
  h.resizePty = [];
  // Every recording starts at 100×30 (tests/fixtures/terminal/README.md).
  rememberTerminalSize(100, 30);
  feeds = Object.fromEntries(
    SESSIONS.map((session) => [session, chunked(paneSteps(loadPtylog(FIXTURE[session])), CHUNKS)]),
  );
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
  seed();
});

afterEach(async () => {
  cleanup();
  for (const session of SESSIONS) terminalManager.dispose(session);
  vi.restoreAllMocks();
});

/** The chunks the teeth feed before showing the faulted tabs. */
const FED = 2;

/** The reference buffer after the first `chunks` chunks of `session`'s recording. */
function referenceAfter(session: string, chunks: number): Promise<ReferenceBuffer> {
  const records = feeds[session].slice(0, chunks).flat().length;
  return headlessFrom(loadPtylog(FIXTURE[session]).slice(0, records));
}

async function expectParity(session: string) {
  expect(await bufferOf(session)).toEqual(await headlessFrom(FIXTURE[session]));
}

describe("every surface change is a slot move", () => {
  it("reattaches the same live terminal slots after renderer eviction", async () => {
    const view = renderApp();
    await flush();
    const elements = Object.fromEntries(SESSIONS.map((id) => [id, elementOf(id)]));
    expect(h.created).toBe(SESSIONS.length);

    act(() => s().closeTeam(TEAM, { preserveTerminalSessions: true }));
    expect(SESSIONS.every((id) => terminalHost().contains(id))).toBe(true);
    view.unmount();

    renderApp();
    await flush();
    expect(h.created).toBe(SESSIONS.length);
    expect(h.disposed).toBe(0);
    for (const id of SESSIONS) expect(elementOf(id)).toBe(elements[id]);
  });

  it.each(["hide", "zoom"])("keeps context-held input when a tile is hidden by %s", async (how) => {
    useViewerStore.setState((state) => ({
      workspaces: {
        ...state.workspaces,
        [TEAM]: state.workspaces[TEAM].map((ws) =>
          ws.id === A ? { ...ws, needsSyncOverlay: true } : ws,
        ),
      },
    }));
    renderApp();
    await flush();
    vi.mocked(writePty).mockClear();
    act(() => terminalManager.terminal("codex")!.input("echo retained\r", true));
    await flush();
    const typed = () =>
      vi
        .mocked(writePty)
        .mock.calls.filter(([session]) => session === "codex")
        .map(([, data]) => data)
        .join("");
    expect(typed()).toBe("echo retained");
    act(() => {
      if (how === "hide") useViewerStore.setState({ terminalVisible: false });
      else s().expandWorkspace(TEAM, B);
    });
    await flush();
    act(() => s().clearSyncOverlay(TEAM, A));
    await flush();
    expect(typed()).toBe("echo retained\r");
  });

  it("retains the neighbour's pane and keeps hidden panes inert across a switch", async () => {
    const view = renderApp();
    await flush();
    const pane = (id: string) =>
      view.container.querySelector<HTMLElement>(`[data-focus-pane="${id}"]`);
    const codex = pane("codex");
    const claude = pane("claude");
    expect(claude).not.toBeNull();
    expect(claude).toHaveAttribute("inert");
    expect(claude).toHaveAttribute("aria-hidden", "true");
    expect(claude?.style.display).toBe("none");
    expect(pane("shell")).toBeNull();
    act(() => s().setActiveTerminalTab(TEAM, "claude", A));
    await flush();
    expect(pane("claude")).toBe(claude);
    expect(pane("claude")).not.toHaveAttribute("inert");
    expect(pane("codex")).toBe(codex);
    expect(pane("codex")).toHaveAttribute("inert");
    expect(pane("shell")).not.toBeNull();
    act(() => s().setActiveTerminalTab(TEAM, "shell", A));
    await flush();
    expect(pane("codex")).toBeNull();
  });

  it("drops a retained terminal's menu on hide without swallowing Escape or reopening", async () => {
    const view = renderApp();
    await flush();
    fireEvent.contextMenu(elementOf("codex")!, { clientX: 12, clientY: 12 });
    expect(view.queryByRole("menu")).not.toBeNull();
    act(() => s().setActiveTerminalTab(TEAM, "claude", A));
    await flush();
    expect(view.queryByRole("menu")).toBeNull();
    const escape = vi.fn();
    document.addEventListener("keydown", escape);
    try {
      fireEvent.keyDown(document, { key: "Escape" });
      expect(escape).toHaveBeenCalledOnce();
    } finally {
      document.removeEventListener("keydown", escape);
    }
    act(() => s().setActiveTerminalTab(TEAM, "codex", A));
    await flush();
    expect(view.queryByRole("menu")).toBeNull();
  });

  it("caps thirty tabs at four panes and keeps only the active pane when the CLI is hidden", async () => {
    const ids = Array.from({ length: 30 }, (_, i) => `warm-${i}`);
    for (const id of ids) h.server.open(id);
    seedTerminalTabs(TEAM, A, ids, { active: ids[15] });
    const view = renderApp();
    try {
      await flush();
      const panes = () =>
        view.container.querySelectorAll(`[data-pane-id="${A}"] [data-focus-pane]`);
      expect(panes()).toHaveLength(3);
      const recent = view.container.querySelector(`[data-focus-pane="${ids[15]}"]`);
      act(() => s().setActiveTerminalTab(TEAM, ids[22], A));
      await flush();
      expect(panes()).toHaveLength(4);
      expect(view.container.querySelector(`[data-focus-pane="${ids[15]}"]`)).toBe(recent);
      act(() => useViewerStore.setState({ terminalVisible: false }));
      await flush();
      expect(panes()).toHaveLength(1);
      expect(panes()[0]).toHaveAttribute("data-focus-pane", ids[22]);
      expect(panes()[0]).toHaveAttribute("inert");
    } finally {
      view.unmount();
      for (const id of ids) terminalManager.dispose(id);
    }
  });

  it("retains the existing grid panes when the CLI is hidden", async () => {
    const view = renderApp();
    act(() => s().expandWorkspace(TEAM, A));
    act(() => s().gridAllTerminalTabs(TEAM, A));
    await flush();
    expect(view.container.querySelector('[data-testid="terminal-grid-cell-codex"]')).not.toBeNull();
    const panes = [...view.container.querySelectorAll("[data-focus-pane]")];
    expect(panes.length).toBeGreaterThan(0);
    act(() => useViewerStore.setState({ terminalVisible: false }));
    await flush();
    expect([...view.container.querySelectorAll("[data-focus-pane]")]).toEqual(panes);
    for (const pane of panes) expect(pane).toHaveAttribute("inert");
  });

  it("retains active panes of tiles hidden by zoom without rebuilding displays", async () => {
    const view = renderApp();
    await flush();
    const before = elementOf("other");
    act(() => s().expandWorkspace(TEAM, A));
    await flush();
    expect(view.container.querySelector('[data-focus-pane="other"]')).toHaveAttribute("inert");
    act(() => s().setCliExpanded(false));
    await flush();
    expect(view.container.querySelector('[data-focus-pane="other"]')).not.toBeNull();
    expect(elementOf("other")).toBe(before);
  });

  it.each([
    ["immediately", async () => {}],
    // Past the old 800 ms tab gate, which disposed a hidden tab's terminal.
    ["after 900 ms hidden", () => act(async () => new Promise((r) => setTimeout(r, 900)))],
  ])(
    "slot_moves_never_construct_or_dispose (%s)",
    async (_label, between) => {
      renderApp();
      await flush();
      // One display per session, hidden tabs included, built once at the start.
      expect(h.created).toBe(SESSIONS.length);
      const elements = Object.fromEntries(SESSIONS.map((id) => [id, elementOf(id)]));
      for (const id of SESSIONS) expect(elements[id]).toBeTruthy();
      feed(0);

      await runMoves(between);
      await flush();

      expect(h.created).toBe(SESSIONS.length);
      expect(h.disposed).toBe(0);
      for (const id of SESSIONS) expect(elementOf(id)).toBe(elements[id]);
      for (const id of SESSIONS) await expectParity(id);
    },
    20_000,
  );

  it("inactive_tab_buffer_equals_reference: output streamed into hidden tabs lands exactly", async () => {
    renderApp();
    await flush();
    feed(0);
    act(() => s().setActiveTerminalTab(TEAM, "shell", A));
    await flush();
    // Codex and Claude are both hidden while the rest of their output streams.
    for (let i = 1; i < CHUNKS; i++) feed(i);
    await act(async () => new Promise((r) => setTimeout(r, 900)));
    const writes = vi.spyOn(terminalManager.terminal("codex")!, "write");
    act(() => s().setActiveTerminalTab(TEAM, "codex", A));
    await flush();
    // Showing it wrote nothing: it was already current.
    expect(writes).not.toHaveBeenCalled();
    for (const id of SESSIONS) await expectParity(id);
  }, 20_000);

  it("clicking between tiles only moves focus: no write, resize or PTY resize", async () => {
    renderApp();
    await flush();
    feed(0);
    await flush();
    const spies = ["codex", "other"].flatMap((id) => {
      const term = terminalManager.terminal(id)!;
      return [vi.spyOn(term, "write"), vi.spyOn(term, "resize"), vi.spyOn(term, "reset")];
    });
    h.resizePty = [];
    for (const target of [B, A, B, A]) {
      act(() => s().switchWorkspace(TEAM, target));
      await flush();
    }
    for (const spy of spies) expect(spy).not.toHaveBeenCalled();
    expect(h.resizePty).toEqual([]);
    expect(h.created).toBe(SESSIONS.length);
  }, 20_000);
});

describe("focus follows the user, never an agent", () => {
  const textareaOf = (session: string) => terminalManager.terminal(session)!.textarea!;

  it("a tab switch from inside the terminal lands the keyboard in the new tab", async () => {
    renderApp();
    await flush();
    act(() => textareaOf("codex").focus());
    expect(document.activeElement).toBe(textareaOf("codex"));
    act(() => s().setActiveTerminalTab(TEAM, "claude", A));
    await flush();
    expect(document.activeElement).toBe(textareaOf("claude"));
  }, 20_000);

  it("a programmatic tab switch leaves focus held by another surface alone", async () => {
    renderApp();
    await flush();
    const notes = document.body.appendChild(document.createElement("textarea"));
    act(() => notes.focus());
    act(() => s().setActiveTerminalTab(TEAM, "claude", A));
    await flush();
    expect(document.activeElement).toBe(notes);
    notes.remove();
  }, 20_000);
});

describe("teeth", () => {
  /** Hide Codex and Claude mid-stream, then show every tab of tile A again. With
   *  `rebuildOnAttach`, every attach throws the display away and restores it
   *  from the host snapshot. */
  async function showAfterHiddenOutput(rebuildOnAttach: boolean) {
    const opts = new Map<
      import("@/shared/terminal/terminal-display-reference").TerminalDisplayReference,
      DisplayOpts
    >();
    const ensure = terminalManager.ensure.bind(terminalManager);
    const attach = terminalManager.attach.bind(terminalManager);
    vi.spyOn(terminalManager, "ensure").mockImplementation((id, o) => {
      opts.set(id, o);
      return ensure(id, o);
    });
    let faulting = false;
    vi.spyOn(terminalManager, "attach").mockImplementation((id, slot, o) => {
      if (faulting) {
        terminalManager.dispose(id);
        void ensure(id, opts.get(id)!);
      }
      attach(id, slot, o);
    });

    renderApp();
    await flush();
    feed(0);
    act(() => s().setActiveTerminalTab(TEAM, "shell", A));
    await flush();
    // Mid-stream: the recordings have resized, and the TUIs have not yet
    // repainted everything at their final size.
    for (let i = 1; i < FED; i++) feed(i);
    await flush();
    faulting = rebuildOnAttach;
    for (const id of ["codex", "claude", "shell"]) {
      act(() => s().setActiveTerminalTab(TEAM, id, A));
      await flush();
    }
  }

  it("the slot move keeps every tab exact at this point of the stream", async () => {
    await showAfterHiddenOutput(false);
    for (const id of ["codex", "claude", "shell"]) {
      expect(await bufferOf(id)).toEqual(await referenceAfter(id, FED));
    }
  }, 20_000);

  it("a restore from the host snapshot on every attach keeps every tab exact too", async () => {
    await showAfterHiddenOutput(true);
    for (const id of ["codex", "claude", "shell"]) {
      await vi.waitFor(() => expect(terminalManager.terminal(id)).not.toBeNull());
      expect(await bufferOf(id)).toEqual(await referenceAfter(id, FED));
    }
  }, 20_000);
});
