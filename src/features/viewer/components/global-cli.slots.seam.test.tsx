// SPDX-License-Identifier: MIT
/**
 * Seam test: the one global CLI across team switches. The real `GlobalCli`, panel tree,
 * `TerminalSlot`, terminal manager and a real `@xterm/xterm` display per session; only the Tauri
 * boundary is faked (a terminal server that streams recorded output on reserved streams).
 *
 * A team switch changes only the CLI's content: its host and the tiles' grid container stay the
 * same elements and the CLI mounts once. Each terminal keeps its one display (built once, never
 * disposed or rebuilt), and showing a team's tiles again is one attach per shown terminal: the
 * same element moved back into its slot, with its buffer intact.
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
  readEpicRepos: vi.fn().mockResolvedValue([]),
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
import { terminalManager } from "@/shared/terminal/terminal-manager";
import { WEBGL_RELEASE_DEBOUNCE_MS } from "@/shared/terminal/webgl-lease-pool";
import { liveInstanceCount } from "@/shared/lib/fe-instance-registry";
import { buildTeamKey } from "@/shared/lib/team-key";
import { rememberTerminalSize } from "@/shared/lib/terminal-open";
import { useOrgStore } from "@/shared/stores/org-store";
import { selectWorkspaceTabs, useViewerStore } from "@/shared/stores/viewer-store";
import { installTerminalLayoutHarness, seedTerminalTabs } from "@/test/terminal-layout-harness";
import { useTerminalDisplayWarm } from "../hooks/use-terminal-display-warm";
import { switchContext } from "../lib/context-switch";
import {
  focusActiveWorkspaceTerminal,
  focusVisibleTerminal,
  toggleTerminalWithFocus,
} from "../lib/focus-cascade";
import { GlobalCli } from "./global-cli";

installTerminalLayoutHarness();

const A = buildTeamKey("acme", "alpha");
const B = buildTeamKey("acme", "beta");
/** Team A's tile runs a Codex and a shell; team B's tile one more shell. */
const FIXTURE: Record<string, PtylogFixture> = {
  codex: "codex-tui",
  shell: "append-only",
  other: "append-only",
};
const SESSIONS = Object.keys(FIXTURE);

const s = () => useViewerStore.getState();
const flush = () => act(async () => new Promise((r) => setTimeout(r, 20)));
const wait = (ms: number) => act(async () => new Promise((r) => setTimeout(r, ms)));

function feedAll() {
  act(() => {
    for (const session of SESSIONS) {
      const steps: PaneStep[] = chunked(paneSteps(loadPtylog(FIXTURE[session])), 1)[0] ?? [];
      for (const step of steps) {
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
    openedTeams: [A, B],
    activeWorkspace: { [A]: "ws-a", [B]: "ws-b" },
    terminalVisible: true,
    layoutMode: "shared",
    aiStatusBySession: {},
    workspaces: {
      [A]: [{ id: "ws-a", kind: "story", cwd: "/a", status: "ready" }],
      [B]: [{ id: "ws-b", kind: "story", cwd: "/b", status: "ready" }],
    },
  }));
  seedTerminalTabs(A, "ws-a", ["codex", "shell"], { active: "codex" });
  seedTerminalTabs(B, "ws-b", ["other"], { active: "other" });
}

/** The app-level hook that gives every tab its display (App mounts it). */
function DisplayWarm() {
  useTerminalDisplayWarm();
  return null;
}

/** The CLI as the workbench mounts it: once, on the active team. */
function ActiveTeamCli() {
  const slug = useOrgStore((o) => o.activeOrgSlug);
  const team = useOrgStore((o) => o.activeTeamName);
  return <GlobalCli teamKey={slug && team ? buildTeamKey(slug, team) : null} />;
}

function renderCli() {
  return render(
    <DndContext>
      <DisplayWarm />
      <ActiveTeamCli />
    </DndContext>,
  );
}

const switchTo = async (team: string) => {
  act(() => useOrgStore.setState({ activeTeamName: team }));
  await flush();
};
const host = () => document.querySelector<HTMLElement>("[data-testid='global-cli']")!;
const grid = () => host().querySelector<HTMLElement>("[data-drop-active]");
const elementOf = (session: string) => terminalManager.terminal(session)?.element;
const shownInCli = (session: string) => {
  const el = elementOf(session);
  return !!el && host().contains(el) && terminalManager.holdsVisible(session);
};
const sessionIds = () => ({
  a: selectWorkspaceTabs(s(), A, "ws-a").map((t) => t.sessionId),
  b: selectWorkspaceTabs(s(), B, "ws-b").map((t) => t.sessionId),
});

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
const rebuildsOf = async (session: string) => (await terminalManager.debugDump(session)).rebuilds;

beforeEach(() => {
  h.bus = new FakeEventBus();
  h.server = new FakeTerminalServer(h.bus);
  for (const session of SESSIONS) h.server.open(session);
  h.created = 0;
  h.disposed = 0;
  rememberTerminalSize(100, 30);
  useOrgStore.setState({
    // The org and its two teams, so a context switch can change the active team.
    orgs: [
      {
        slug: "acme",
        display_name: "Acme",
        created_at: "",
        remote_url: null,
        config_flags: {},
        people: [],
        org_root: "/acme",
        teams: [
          { name: "alpha", source_repos: [] },
          { name: "beta", source_repos: [] },
        ],
      },
    ],
    activeOrgSlug: "acme",
    activeTeamName: "alpha",
  });
  seed();
});

afterEach(() => {
  cleanup();
  for (const session of SESSIONS) terminalManager.dispose(session);
  vi.restoreAllMocks();
  useOrgStore.setState({ orgs: [], activeOrgSlug: null, activeTeamName: null });
});

describe("the global CLI across team switches", () => {
  it("keeps its host, its grid and its terminals: a first visit attaches once, a return to a warm team nothing", async () => {
    renderCli();
    await flush();
    feedAll();
    await flush();
    expect(h.created).toBe(SESSIONS.length);
    const cliHost = host();
    const cliGrid = grid();
    expect(cliGrid).not.toBeNull();
    expect(liveInstanceCount("cli")).toBe(1);
    const elements = Object.fromEntries(SESSIONS.map((id) => [id, elementOf(id)]));
    const rebuilds = Object.fromEntries(
      await Promise.all(SESSIONS.map(async (id) => [id, await rebuildsOf(id)] as const)),
    );
    const ids = sessionIds();
    expect(shownInCli("codex")).toBe(true);
    const attach = vi.spyOn(terminalManager, "attach");

    await switchTo("beta");
    // B's one shown terminal moved in, once; A's are parked, their PTYs untouched.
    expect(attach.mock.calls.map((c) => c[0])).toEqual(["other"]);
    expect(shownInCli("other")).toBe(true);
    expect(shownInCli("codex")).toBe(false);
    expect(terminalManager.holdsVisible("codex")).toBe(false);
    expect(host()).toBe(cliHost);
    expect(grid()).toBe(cliGrid);

    attach.mockClear();
    await switchTo("alpha");
    // Team alpha is warm: its grid never left, so coming back attaches nothing.
    expect(attach.mock.calls.map((c) => c[0])).toEqual([]);
    expect(terminalManager.holdsVisible("shell")).toBe(false);
    expect(shownInCli("codex")).toBe(true);
    expect(elementOf("codex")).toBe(elements.codex);
    expect(host()).toBe(cliHost);
    expect(grid()).toBe(cliGrid);
    expect(liveInstanceCount("cli")).toBe(1);

    // Nothing was built, disposed or rebuilt, no session changed, and no buffer lost a byte.
    expect(h.created).toBe(SESSIONS.length);
    expect(h.disposed).toBe(0);
    for (const id of SESSIONS) {
      expect(elementOf(id)).toBe(elements[id]);
      expect(await rebuildsOf(id)).toBe(rebuilds[id]);
    }
    expect(sessionIds()).toEqual(ids);
    for (const id of SESSIONS) {
      const steps = chunked(paneSteps(loadPtylog(FIXTURE[id])), 1)[0] ?? [];
      expect(await bufferOf(id)).toEqual(
        await headlessFrom(loadPtylog(FIXTURE[id]).slice(0, steps.length)),
      );
    }
  }, 20_000);

  it("a switch to the team on screen attaches nothing", async () => {
    renderCli();
    await flush();
    const attach = vi.spyOn(terminalManager, "attach");
    await switchTo("alpha");
    act(() => s().switchWorkspace(A, "ws-a"));
    await flush();
    expect(attach).not.toHaveBeenCalled();
  }, 20_000);

  it("the WebGL leases follow the shown tiles: the old team's go after a switch, a hidden CLI holds none", async () => {
    renderCli();
    await flush();
    const leased = (id: string) => terminalManager.webglLease(id)?.leased ?? false;
    // The shown tile's terminal holds a lease.
    expect(leased("codex")).toBe(true);
    await switchTo("beta");
    await wait(WEBGL_RELEASE_DEBOUNCE_MS + 100);
    expect(leased("codex")).toBe(false);
    expect(leased("shell")).toBe(false);
    expect(leased("other")).toBe(true);

    act(() => s().setTerminalVisible(false));
    await wait(WEBGL_RELEASE_DEBOUNCE_MS + 100);
    expect(SESSIONS.filter(leased)).toEqual([]);
  }, 20_000);

  it("another team's terminal is free for the quick view to borrow, and the shown team's is not", async () => {
    renderCli();
    await flush();
    await switchTo("beta");
    // The quick view borrows a display only when no tile shows it.
    expect(terminalManager.holdsVisible("codex")).toBe(false);
    expect(terminalManager.holdsVisible("other")).toBe(true);
    const popup = document.body.appendChild(document.createElement("div"));
    act(() =>
      terminalManager.attach("codex", popup, { focus: false, visible: true, borrow: true }),
    );
    expect(popup.contains(elementOf("codex")!)).toBe(true);
    // Showing A again takes its display back into its tile.
    act(() => terminalManager.detach("codex", popup));
    await switchTo("alpha");
    expect(shownInCli("codex")).toBe(true);
    popup.remove();
  }, 20_000);

  it("focusing the CLI takes its own terminal, not a quick view's drawn before it", async () => {
    const popup = document.body.insertBefore(
      document.createElement("div"),
      document.body.firstChild,
    );
    popup.innerHTML = '<div class="xterm"><textarea data-testid="popup-input"></textarea></div>';
    renderCli();
    await flush();
    act(() => focusVisibleTerminal());
    expect(document.activeElement).not.toBe(popup.querySelector("textarea"));
    expect(host().contains(document.activeElement)).toBe(true);
    popup.remove();
  }, 20_000);

  it("a row click on another team's agent lands the keyboard in that agent's terminal", async () => {
    renderCli();
    await flush();
    act(() => {
      switchContext({ teamKey: B, workspaceId: "ws-b", tabId: "other" }, "agent");
      focusActiveWorkspaceTerminal(B);
    });
    await flush();
    expect(document.activeElement).toBe(terminalManager.terminal("other")!.textarea);
  }, 20_000);

  it("a switch by the team switcher leaves the keyboard where it was", async () => {
    renderCli();
    await flush();
    const notes = document.body.appendChild(document.createElement("textarea"));
    act(() => notes.focus());
    act(() => {
      switchContext({ teamKey: B }, "switcher");
    });
    await flush();
    expect(document.activeElement).toBe(notes);
    notes.remove();
  }, 20_000);

  it("Cmd+\\ reopening the CLI lands the keyboard in the active team's focused workspace", async () => {
    renderCli();
    await flush();
    await switchTo("beta");
    act(() => toggleTerminalWithFocus(B));
    await flush();
    act(() => toggleTerminalWithFocus(B));
    await flush();
    expect(document.activeElement).toBe(terminalManager.terminal("other")!.textarea);
  }, 20_000);

  it("with no active team the host stays and shows nothing", async () => {
    renderCli();
    await flush();
    const cliHost = host();
    act(() => useOrgStore.setState({ activeTeamName: null }));
    await flush();
    expect(host()).toBe(cliHost);
    expect(grid()).toBeNull();
    expect(h.disposed).toBe(0);
  }, 20_000);
});
