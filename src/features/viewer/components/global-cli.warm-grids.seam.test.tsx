// SPDX-License-Identifier: MIT
/**
 * Seam test: the CLI keeps a warm team's terminal grid attached. The real `GlobalCli`, panel tree,
 * `TerminalSlot`, terminal manager and a real `@xterm/xterm` display per session; only the Tauri
 * boundary is faked.
 *
 * A team in the warm set (the active one and the most recent others) keeps its tiles mounted,
 * hidden while another team is active: a switch between warm teams attaches, detaches, rebuilds
 * and re-measures nothing. A team that is suspended gives its tiles back (parked, PTY running).
 */
import { act, cleanup, fireEvent, render } from "@/test-utils";
import {
  DndContext,
  KeyboardSensor,
  useDndContext,
  useDraggable,
  useSensor,
  useSensors,
  type CollisionDetection,
  type DragEndEvent,
} from "@dnd-kit/core";
import { useEffect } from "react";
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
import { rememberTerminalSize } from "@/shared/lib/terminal-open";
import { useOrgStore } from "@/shared/stores/org-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";
import { warmScopes } from "@/shared/lib/warm-scopes";

import { GlobalCli } from "@/features/viewer/components/global-cli";
import { terminalManager } from "@/shared/terminal/terminal-manager";
import { expectFlatAcross } from "@/shared/lib/warm-scopes/__test-utils__/expect-flat-across";

installTerminalLayoutHarness();

const A = "acme:alpha";
const B = "acme:beta";
const X = "acme:gamma";
const Y = "acme:delta";
const C = "zed:other";
const ALL = [A, B, X, Y, C];

const flush = () => act(async () => new Promise((r) => setTimeout(r, 20)));

function seed() {
  // The same workspace id in two teams: the tile identity is its own, never the other team's.
  useViewerStore.setState((st) => ({
    settings: { ...st.settings },
    foregroundLayout: {},
    terminalVisible: true,
    layoutMode: "shared",
    aiStatusBySession: {},
    openedTeams: ALL,
    activeWorkspace: { [A]: "ws-1", [B]: "ws-1", [X]: "ws-x", [Y]: "ws-y", [C]: "ws-c" },
    workspaces: Object.fromEntries(
      ALL.map((tk) => [
        tk,
        [
          {
            id: tk === X ? "ws-x" : tk === Y ? "ws-y" : tk === C ? "ws-c" : "ws-1",
            kind: "story" as const,
            cwd: "/w",
            status: "ready" as const,
          },
        ],
      ]),
    ),
  }));
  seedTerminalTabs(A, "ws-1", ["sess-a"], { active: "sess-a" });
  seedTerminalTabs(B, "ws-1", ["sess-b"], { active: "sess-b" });
  seedTerminalTabs(X, "ws-x", ["sess-x"], { active: "sess-x" });
  seedTerminalTabs(Y, "ws-y", ["sess-y"], { active: "sess-y" });
  seedTerminalTabs(C, "ws-c", ["sess-c"], { active: "sess-c" });
}

/** Resolves a drag onto the enabled terminal grid: collisions by registration, not geometry
 *  (jsdom has none), so what is under test is which registrations dnd-kit holds. */
const enabledGridCollision: CollisionDetection = ({ droppableContainers }) =>
  droppableContainers.filter((c) => c.id === "terminal-grid").map((c) => ({ id: c.id }));

const probe: { registry: ReturnType<typeof useDndContext>["droppableContainers"] | null } = {
  registry: null,
};
function RegistryProbe() {
  const { droppableContainers } = useDndContext();
  useEffect(() => {
    probe.registry = droppableContainers;
  });
  return null;
}
function TrayChip() {
  const { setNodeRef, listeners, attributes } = useDraggable({ id: "tray:ws-1" });
  return <button ref={setNodeRef} data-testid="chip" {...listeners} {...attributes} />;
}

/** The window's shape: the one CLI shows the active team, in the Workbench's one DndContext. */
function Cli({ active, onDragEnd }: { active: string; onDragEnd?: (e: DragEndEvent) => void }) {
  warmScopes.activate(active);
  const sensors = useSensors(useSensor(KeyboardSensor));
  return (
    <DndContext sensors={sensors} collisionDetection={enabledGridCollision} onDragEnd={onDragEnd}>
      <RegistryProbe />
      <TrayChip />
      <GlobalCli teamKey={active} />
    </DndContext>
  );
}

const attaches = () => vi.mocked(terminalManager.attach).mock.calls.length;
const detaches = () => vi.mocked(terminalManager.detach).mock.calls.length;
const atlasClears = () => vi.mocked(terminalManager.clearTextureAtlas).mock.calls.length;
const SESSIONS = ["sess-a", "sess-b", "sess-x", "sess-y", "sess-c"];

beforeEach(() => {
  h.bus = new FakeEventBus();
  h.server = new FakeTerminalServer(h.bus);
  for (const session of SESSIONS) h.server.open(session);
  h.created = 0;
  h.disposed = 0;
  rememberTerminalSize(100, 30);
  useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "alpha" });
  for (const tk of ALL) warmScopes.forget(tk);
  seed();
  vi.spyOn(terminalManager, "attach");
  vi.spyOn(terminalManager, "detach");
  vi.spyOn(terminalManager, "clearTextureAtlas");
});

afterEach(() => {
  cleanup();
  for (const session of SESSIONS) terminalManager.dispose(session);
  for (const tk of ALL) warmScopes.forget(tk);
  vi.restoreAllMocks();
});

const tileCount = () => document.querySelectorAll("[data-pane-id]").length;

describe("the CLI keeps warm teams' grids attached", () => {
  it("a warm team's tiles stay mounted, hidden, while another team is shown", async () => {
    const view = render(<Cli active={A} />);
    await flush();
    view.rerender(<Cli active={B} />);
    await flush();
    expect(tileCount()).toBe(2);
    // Same workspace id in both teams: two tiles, each in its own team's body.
    expect(document.querySelectorAll('[data-pane-id="ws-1"]')).toHaveLength(2);
    expect(terminalManager.holdsVisible("sess-b")).toBe(true);
    expect(terminalManager.holdsVisible("sess-a")).toBe(false);
  });

  it("switching between warm teams attaches, detaches, rebuilds and re-measures nothing", async () => {
    const view = render(<Cli active={A} />);
    await flush();
    view.rerender(<Cli active={B} />);
    await flush();
    const built = h.created;
    const before = { attaches: attaches(), detaches: detaches(), atlasClears: atlasClears() };
    for (let i = 0; i < 10; i++) {
      view.rerender(<Cli active={A} />);
      view.rerender(<Cli active={B} />);
    }
    await flush();
    expect({ attaches: attaches(), detaches: detaches(), atlasClears: atlasClears() }).toEqual(
      before,
    );
    expect(h.created).toBe(built);
    expect(h.disposed).toBe(0);
  });

  it("50 round trips A<->B and across orgs A<->C stay flat", async () => {
    const view = render(<Cli active={A} />);
    await flush();
    // Displays are built asynchronously: prime every team the round trips visit.
    for (const tk of [B, C, A]) {
      view.rerender(<Cli active={tk} />);
      await flush();
    }
    const read = () => ({
      attaches: attaches(),
      detaches: detaches(),
      atlasClears: atlasClears(),
      created: h.created,
      disposed: h.disposed,
      tiles: tileCount(),
    });
    await expectFlatAcross(() => {
      view.rerender(<Cli active={B} />);
      view.rerender(<Cli active={A} />);
    }, read);
    await expectFlatAcross(() => {
      view.rerender(<Cli active={C} />);
      view.rerender(<Cli active={A} />);
    }, read);
  }, 30_000);

  it("a suspended team gives its tiles back but its PTY and display live on", async () => {
    const view = render(<Cli active={A} />);
    await flush();
    const element = terminalManager.terminal("sess-a")?.element;
    const kill = vi.spyOn(terminalHost(), "closeTab");
    for (const tk of [B, X, Y]) {
      view.rerender(<Cli active={tk} />);
      await flush();
    }
    expect(warmScopes.tierOf(A)).toBe("suspended");
    expect(document.querySelectorAll('[data-pane-id="ws-1"]')).toHaveLength(1);
    expect(h.disposed).toBe(0);
    expect(kill).not.toHaveBeenCalled();
    view.rerender(<Cli active={A} />);
    await flush();
    expect(terminalManager.terminal("sess-a")?.element).toBe(element);
    expect(h.disposed).toBe(0);
  });

  it("a team coming back from suspended re-measures without rebuilding the glyph atlas", async () => {
    const view = render(<Cli active={A} />);
    await flush();
    for (const tk of [B, X, Y]) {
      view.rerender(<Cli active={tk} />);
      await flush();
    }
    const clears = atlasClears();
    view.rerender(<Cli active={A} />);
    await flush();
    expect(atlasClears()).toBe(clears);
  });

  it("A->B->A leaves the active team's grid as the enabled drop target (real sensor)", async () => {
    const dropped: Array<string | undefined> = [];
    const onDragEnd = (e: DragEndEvent) => {
      const node = probe.registry?.get(String(e.over?.id))?.node.current;
      dropped.push(node?.closest("[data-cli-team]")?.getAttribute("data-cli-team") ?? undefined);
    };
    const view = render(<Cli active={A} onDragEnd={onDragEnd} />);
    await flush();
    view.rerender(<Cli active={B} onDragEnd={onDragEnd} />);
    await flush();
    view.rerender(<Cli active={A} onDragEnd={onDragEnd} />);
    await flush();
    const live = [...(probe.registry?.getEnabled() ?? [])].filter((c) => c.id === "terminal-grid");
    expect(live).toHaveLength(1);
    const chip = document.querySelector<HTMLElement>('[data-testid="chip"]')!;
    chip.focus();
    fireEvent.keyDown(chip, { code: "Space" });
    await flush();
    fireEvent.keyDown(document, { code: "Space" });
    await flush();
    expect(dropped).toEqual([A]);
  });

  it("a hidden team's wrapper and tile frames never move in the DOM across switches", async () => {
    const view = render(<Cli active={A} />);
    await flush();
    view.rerender(<Cli active={B} />);
    await flush();
    const host = document.querySelector('[data-testid="global-cli"]')!;
    const wrappers = [...host.querySelectorAll(":scope > [data-cli-team]")];
    const frames = [...host.querySelectorAll("[data-pane-id]")];
    const parents = frames.map((f) => f.parentElement);
    const moves: MutationRecord[] = [];
    const observer = new MutationObserver((records) => moves.push(...records));
    // The wrappers' own child lists (their grids) and the host's (the wrappers).
    observer.observe(host, { childList: true });
    for (const w of wrappers) observer.observe(w, { childList: true });
    for (let i = 0; i < 10; i++) {
      view.rerender(<Cli active={A} />);
      view.rerender(<Cli active={B} />);
    }
    await flush();
    observer.disconnect();
    expect([...host.querySelectorAll(":scope > [data-cli-team]")]).toEqual(wrappers);
    expect([...host.querySelectorAll("[data-pane-id]")]).toEqual(frames);
    expect(frames.map((f) => f.parentElement)).toEqual(parents);
    expect(moves).toEqual([]);
  });
});
