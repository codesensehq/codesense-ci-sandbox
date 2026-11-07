// SPDX-License-Identifier: MIT
/**
 * Seam test: switching teams parks and unparks terminal displays. The real
 * render gate, panel tree, `ShellTerminalPanel` + `TerminalSlot`, terminal
 * manager and a real `@xterm/xterm` display per session; only the Tauri
 * boundary is faked (a terminal server that streams recorded output).
 *
 * A team that goes out of view (and past the gate's debounce, unmounted) keeps
 * each session's one display: nothing built or disposed, output streamed while
 * hidden lands exactly, and focus and scroll position come back with it.
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
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";
import { useTerminalDisplayWarm } from "@/features/viewer/hooks/use-terminal-display-warm";
import { WorkspaceTerminalPanels } from "@/features/viewer/components/workspace-terminal-panels";
import { useTeamRenderGate } from "./use-team-render-gate";
import { warmScopes } from "@/shared/lib/warm-scopes";

installTerminalLayoutHarness();

const TEAM_A = "acme:alpha";
const TEAM_B = "acme:beta";
const TEAM_X = "acme:gamma";
const TEAM_Y = "acme:delta";
const TEAMS = [TEAM_A, TEAM_B, TEAM_X, TEAM_Y];
const NO_DIRTY: ReadonlySet<string> = new Set();
/** Team A's tile runs a Codex and a Claude Code session plus an append-only
 *  shell; team B one more shell. */
const FIXTURE: Record<string, PtylogFixture> = {
  codex: "codex-tui",
  claude: "claude-ink",
  shell: "append-only",
  other: "append-only",
};
const SESSIONS = Object.keys(FIXTURE);
const CHUNKS = 4;
const REFERENCE_SCROLLBACK = 1000;

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
    settings: { ...st.settings, terminalScrollback: REFERENCE_SCROLLBACK },
    foregroundLayout: {},
    activeWorkspace: { [TEAM_A]: "ws-a", [TEAM_B]: "ws-b" },
    terminalVisible: true,
    aiStatusBySession: {},
    workspaces: {
      [TEAM_A]: [{ id: "ws-a", kind: "story", cwd: "/a", status: "ready" }],
      [TEAM_B]: [{ id: "ws-b", kind: "story", cwd: "/b", status: "ready" }],
    },
  }));
  seedTerminalTabs(TEAM_A, "ws-a", ["codex", "claude", "shell"], {
    active: "codex",
    paned: ["codex", "claude", "shell"],
  });
  seedTerminalTabs(TEAM_B, "ws-b", ["other"], { active: "other" });
}

/** The app's shape: every opened team the gate keeps mounted, only the active
 *  one shown. */
function Teams({ active }: { active: string }) {
  useTerminalDisplayWarm();
  warmScopes.activate(active);
  const mounted = useTeamRenderGate({ openedTeams: TEAMS, dirtyTeamKeys: NO_DIRTY });
  return (
    <DndContext>
      {[TEAM_A, TEAM_B]
        .filter((tk) => mounted.has(tk))
        .map((tk) => (
          <div key={tk} data-team={tk} style={{ display: tk === active ? undefined : "none" }}>
            <div data-xterm-host data-repo-path="/repo">
              <WorkspaceTerminalPanels teamKey={tk} repoPath="/repo" />
            </div>
          </div>
        ))}
    </DndContext>
  );
}

/** Visit two more teams: with team B that is three, so team A leaves the warm set and unmounts. */
async function suspendA(view: { rerender: (ui: React.ReactElement) => void }) {
  view.rerender(<Teams active={TEAM_X} />);
  view.rerender(<Teams active={TEAM_Y} />);
  await flush();
}

const elementOf = (session: string) => terminalManager.terminal(session)?.element;

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

function referenceAfter(session: string, chunks: number): Promise<ReferenceBuffer> {
  const records = feeds[session].slice(0, chunks).flat().length;
  return headlessFrom(loadPtylog(FIXTURE[session]).slice(0, records));
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
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "alpha" });
  seed();
});

afterEach(() => {
  cleanup();
  for (const team of TEAMS) warmScopes.forget(team);
  for (const session of SESSIONS) terminalManager.dispose(session);
  vi.restoreAllMocks();
});

describe("a team switch parks its terminals", () => {
  it.each([
    ["while team A is warm", false],
    ["after the gate unmounted team A", true],
  ])(
    "team_switch_parks_displays (%s)",
    async (_label, suspended) => {
      const view = render(<Teams active={TEAM_A} />);
      await flush();
      expect(h.created).toBe(SESSIONS.length);
      const elements = Object.fromEntries(SESSIONS.map((id) => [id, elementOf(id)]));
      const instances = Object.fromEntries(
        await Promise.all(
          SESSIONS.map(async (id) => [id, (await terminalManager.debugDump(id)).instanceId]),
        ),
      );
      feed(0);
      await flush();

      view.rerender(<Teams active={TEAM_B} />);
      await flush();
      // Team A's TUIs keep drawing while team B is shown.
      for (let i = 1; i < CHUNKS; i++) feed(i);
      if (suspended) await suspendA(view);
      if (suspended) expect(document.querySelector(`[data-team="${TEAM_A}"]`)).toBeNull();

      view.rerender(<Teams active={TEAM_A} />);
      await flush();

      expect(h.created).toBe(SESSIONS.length);
      expect(h.disposed).toBe(0);
      for (const id of SESSIONS) {
        expect(elementOf(id)).toBe(elements[id]);
        expect((await terminalManager.debugDump(id)).instanceId).toBe(instances[id]);
      }
      // hidden_team_buffer_equals_reference; the append-only shell is the control.
      for (const id of SESSIONS)
        expect(await bufferOf(id)).toEqual(await headlessFrom(FIXTURE[id]));
    },
    20_000,
  );

  it("focus and a scrolled-up view come back with the team", async () => {
    const view = render(<Teams active={TEAM_A} />);
    await flush();
    for (let i = 0; i < CHUNKS; i++) feed(i, ["shell"]);
    await flush();
    const shell = terminalManager.terminal("shell")!;
    // Scroll only once every fed chunk is parsed: a write landing after the
    // scroll snaps the view back to the bottom (seen under a loaded suite).
    await terminalManager.debugDump("shell");
    await act(() => new Promise<void>((r) => shell.write("", r)));
    act(() => shell.scrollLines(-3));
    await wait(300);
    const viewportY = shell.buffer.active.viewportY;
    expect(viewportY).toBeLessThan(shell.buffer.active.baseY);
    act(() => shell.textarea!.focus());

    view.rerender(<Teams active={TEAM_B} />);
    await suspendA(view);
    view.rerender(<Teams active={TEAM_A} />);
    await flush();

    expect(shell.buffer.active.viewportY).toBe(viewportY);
    expect(document.activeElement).toBe(shell.textarea);
  }, 20_000);

  it("a session that ends while its team is away shows the ended bar, not a new shell", async () => {
    const view = render(<Teams active={TEAM_A} />);
    await flush();
    act(() => h.server.produce("shell", "last words\r\n"));
    view.rerender(<Teams active={TEAM_B} />);
    await suspendA(view);
    act(() => terminalHost().exit("shell", 3));
    await flush();

    const respawn = vi.spyOn(terminalHost(), "respawnTab");
    view.rerender(<Teams active={TEAM_A} />);
    await flush();

    const bar = view.getByTestId("terminal-ended-bar");
    expect(bar.getAttribute("aria-label")).toMatch(/3/);
    await terminalManager.debugDump("shell");
    const line = terminalManager.terminal("shell")!.buffer.active.getLine(0);
    expect(line?.translateToString(true)).toBe("last words");
    expect(respawn).not.toHaveBeenCalled();
    expect(h.disposed).toBe(0);
  }, 20_000);
});

describe("teeth", () => {
  /** Switch away mid-stream and back. With `rebuildOnAttach`, every attach
   *  throws the display away and restores it from the host snapshot. */
  async function switchAwayAndBack(rebuildOnAttach: boolean) {
    const opts = new Map<Parameters<typeof terminalManager.ensure>[0], DisplayOpts>();
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

    const view = render(<Teams active={TEAM_A} />);
    await flush();
    feed(0);
    view.rerender(<Teams active={TEAM_B} />);
    await suspendA(view);
    feed(1);
    await flush();
    faulting = rebuildOnAttach;
    view.rerender(<Teams active={TEAM_A} />);
    await flush();
    faulting = false;
  }

  it("the park keeps every session exact at this point of the stream", async () => {
    await switchAwayAndBack(false);
    for (const id of ["codex", "claude", "shell"]) {
      expect(await bufferOf(id)).toEqual(await referenceAfter(id, 2));
    }
  }, 20_000);

  it("a restore from the host snapshot on every attach keeps every session exact too", async () => {
    await switchAwayAndBack(true);
    for (const id of ["codex", "claude", "shell"]) {
      await vi.waitFor(() => expect(terminalManager.terminal(id)).not.toBeNull());
      expect(await bufferOf(id)).toEqual(await referenceAfter(id, 2));
    }
  }, 20_000);
});
