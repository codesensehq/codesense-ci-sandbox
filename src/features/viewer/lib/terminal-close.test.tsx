// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, fireEvent, render, renderHook, screen, waitFor } from "@/test-utils";
import userEvent from "@testing-library/user-event";

import {
  closeActiveTerminalTab,
  closeTab,
  closeWorkspace,
  endTabs,
  endWorkspace,
  hideTab,
  hideWorkspace,
  settleEndConfirm,
  useEndConfirmStore,
} from "./terminal-close";
import { EndConfirmDialog } from "../components/end-confirm-dialog";
import { TerminalTileChrome } from "../components/terminal-tile-chrome";
import { useTileTabActions } from "../components/workspace-terminal-panels";
import { useAgentDrill } from "../hooks/use-agent-drill";
import { WorkspaceTray } from "../components/workspace-tray";
import {
  selectActiveTerminalTab,
  selectHiddenTerminalTabs,
  selectTerminalTabIndex,
  selectWorkspaceTabs,
  useViewerStore,
  type TerminalTab,
  type Workspace,
} from "@/shared/stores/viewer-store";
import { useOrgStore } from "@/shared/stores/org-store";
import type { AgentSessionRow } from "@/tauri/commands";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
} from "@/test/terminal-layout-harness";

/**
 * The test terminal host holds the sessions (a tab is its session; a Hidden tab
 * is a Background entry) — everything above it (the store, the close verbs, the
 * dialog, the tile chrome) is real. The process probe is faked at the IPC seam:
 *
 * - `running`: what each live shell runs in the foreground (absent = at its prompt).
 * - `background`: each live shell's background jobs.
 * - `killed`: the sessions the host ended, in order.
 */
const backend = vi.hoisted(() => ({
  running: new Map<string, string>(),
  background: new Map<string, string[]>(),
  probeFails: false,
  killed: [] as string[],
}));

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  sessionsRunningProcesses: vi.fn(async (ids: string[]) => {
    if (backend.probeFails) throw new Error("ipc down");
    return ids.map((id) =>
      alive().has(id)
        ? {
            foreground: backend.running.get(id) ?? null,
            background: backend.background.get(id) ?? [],
          }
        : { foreground: null, background: [] },
    );
  }),
  dashboardForegroundSession: vi.fn(async () => {}),
}));
vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

const TEAM = "acme:default";
const WS = "ws-story";
const OTHER = "ws-other";

installTerminalLayoutHarness();

const story = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });
const tab = (id: string): TerminalTab => ({ id, name: id, sessionId: id });
const sid = (id: string) => id;

/** The sessions whose process runs in the host. */
function alive(): Set<string> {
  const out = new Set<string>();
  for (const layout of Object.values(terminalHost().snapshot().workspaces)) {
    for (const entry of layout.tabs) if (entry.state.kind === "live") out.add(entry.session);
  }
  return out;
}

/** Every session that ever had a PTY in this test (replacement tabs never do). */
let spawned = new Set<string>();

type Seed = string[] | { bg: string[] } | null;

function seed(tabs: Record<string, Seed>) {
  // `null` = at its prompt; `[name]` = `name` runs in the foreground;
  // `{ bg }` = at its prompt with background jobs.
  const all = Object.keys(tabs).map(tab);
  backend.running = new Map();
  backend.background = new Map();
  for (const [id, run] of Object.entries(tabs)) {
    if (Array.isArray(run)) backend.running.set(sid(id), run.join(", "));
    else if (run) backend.background.set(sid(id), run.bg);
  }
  backend.probeFails = false;
  backend.killed = [];
  spawned = new Set([...all.map((t) => t.sessionId), sid("o1")]);
  useViewerStore.setState({
    workspaces: { [TEAM]: [story(WS), story(OTHER)] },
    activeWorkspace: { [TEAM]: WS },
    softClosedWorkspaces: { [TEAM]: {} },
    foregroundLayout: {},
  });
  seedTerminalTabs(
    TEAM,
    WS,
    all.map((t) => ({ session: t.id, name: t.name })),
    { active: all[0]?.id },
  );
  seedTerminalTabs(TEAM, OTHER, [{ session: "o1", name: "o1" }]);
  // The host's own record of what it ended.
  const host = terminalHost();
  const close = host.closeTab.bind(host);
  vi.spyOn(host, "closeTab").mockImplementation((session, mode) => {
    const outcome = close(session, mode);
    if (outcome === "killed") backend.killed.push(session);
    return outcome;
  });
  useEndConfirmStore.setState({ request: null, resolve: null });
}

const tabsIn = (ws: string) =>
  selectWorkspaceTabs(useViewerStore.getState(), TEAM, ws).map((t) => t.id);

const hiddenIn = (ws: string) =>
  selectHiddenTerminalTabs(useViewerStore.getState(), TEAM, ws).map((t) => t.id);

/** Visible tiles ∪ the tray (hidden tiles + each chip's hidden tabs). */
function reachable(): Set<string> {
  const s = useViewerStore.getState();
  const out = new Set<string>();
  const tiles = [
    ...(s.workspaces[TEAM] ?? []).map((w) => w.id),
    ...Object.keys(s.softClosedWorkspaces[TEAM] ?? {}),
  ];
  for (const w of tiles) {
    for (const t of selectWorkspaceTabs(s, TEAM, w)) out.add(t.sessionId);
    for (const t of selectHiddenTerminalTabs(s, TEAM, w)) out.add(t.sessionId);
  }
  return out;
}

/**
 * The invariant after ANY close: the running sessions are exactly the ones on a
 * tile or in a hidden list — nothing running is orphaned, and nothing listed is
 * dead (fresh replacement tabs, which never had a PTY, aside).
 */
function expectRunningEqualsReachable() {
  const listed = new Set([...reachable()].filter((id) => spawned.has(id)));
  const running = [...alive()].filter((id) => spawned.has(id));
  expect(running.sort()).toEqual([...listed].sort());
}

const flush = () => act(async () => new Promise((r) => setTimeout(r, 0)));

/** Answer the open End confirmation (after it has opened). */
async function answer(confirmed: boolean) {
  await waitFor(() => expect(useEndConfirmStore.getState().request).not.toBeNull());
  settleEndConfirm(confirmed);
}

beforeEach(() => vi.clearAllMocks());
afterEach(() => useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null }));

describe("the close decision table — running/idle × verb × single/others/all", () => {
  it("× on a RUNNING tab Hides it into the tray: listed under its chip, still running", async () => {
    seed({ t1: ["claude"], t2: null });
    await closeTab(TEAM, WS, "t1");
    expect(tabsIn(WS)).toEqual(["t2"]);
    expect(hiddenIn(WS)).toEqual(["t1"]);
    expect(backend.killed).toEqual([]);
    expect(useEndConfirmStore.getState().request).toBeNull(); // no dialog for a Hide
    expectRunningEqualsReachable();
  });

  it("⌘W on the active RUNNING tab Hides it; on an active IDLE tab Ends it", async () => {
    seed({ t1: ["claude"], t2: null });
    await closeActiveTerminalTab(TEAM); // t1 is active + running → Hide
    expect(tabsIn(WS)).toEqual(["t2"]);
    expect(backend.killed).toEqual([]);
    expectRunningEqualsReachable();
    await closeActiveTerminalTab(TEAM); // t2 is now active + idle → End
    expect(backend.killed).toEqual([sid("t2")]);
    expectRunningEqualsReachable();
  });

  it("× on an IDLE tab Ends it directly — no dialog", async () => {
    seed({ t1: null, t2: ["npm"] });
    await closeTab(TEAM, WS, "t1");
    expect(tabsIn(WS)).toEqual(["t2"]);
    expect(backend.killed).toEqual([sid("t1")]);
    expect(hiddenIn(WS)).toEqual([]);
    expect(useEndConfirmStore.getState().request).toBeNull();
    expectRunningEqualsReachable();
  });

  it("× at a prompt with BACKGROUND jobs Ends — but asks first, naming them", async () => {
    seed({ t1: { bg: ["npm"] }, t2: null });
    const closing = closeTab(TEAM, WS, "t1");
    await waitFor(() => expect(useEndConfirmStore.getState().request).not.toBeNull());
    expect(useEndConfirmStore.getState().request!.running).toEqual(["t1 — npm (background)"]);
    settleEndConfirm(false);
    await closing;
    expect(backend.killed).toEqual([]);
    expect(tabsIn(WS)).toEqual(["t1", "t2"]);
    expectRunningEqualsReachable();
  });

  it("tile × with only background jobs asks before Ending", async () => {
    seed({ t1: { bg: ["vite"] }, t2: null });
    const closing = closeWorkspace(TEAM, WS);
    await answer(true);
    await closing;
    expect(backend.killed.sort()).toEqual([sid("t1"), sid("t2")]);
    expectRunningEqualsReachable();
  });

  it("× when the probe fails treats the tab as running → Hide, never a kill", async () => {
    seed({ t1: null, t2: null });
    backend.probeFails = true;
    await closeTab(TEAM, WS, "t1");
    expect(backend.killed).toEqual([]);
    expect(hiddenIn(WS)).toEqual(["t1"]);
    expectRunningEqualsReachable();
  });

  it("there is no hide limit — the 7th hidden shell still hides", async () => {
    seed({ a: null, b: null, c: null, d: null, e: null, f: null, g: null, h: null });
    for (const id of ["a", "b", "c", "d", "e", "f", "g"]) hideTab(TEAM, WS, id);
    await flush();
    expect(hiddenIn(WS)).toEqual(["a", "b", "c", "d", "e", "f", "g"]);
    expect(tabsIn(WS)).toEqual(["h"]);
    expect(backend.killed).toEqual([]);
    expectRunningEqualsReachable();
  });

  it("hiding a tile's ONLY tab hides the tile — no replacement shell is spawned", async () => {
    seed({ t1: ["claude"] });
    await closeTab(TEAM, WS, "t1");
    expect(useViewerStore.getState().workspaces[TEAM]?.some((w) => w.id === WS)).toBe(false);
    expect(useViewerStore.getState().softClosedWorkspaces[TEAM]?.[WS]).toBeDefined();
    expect(tabsIn(WS)).toEqual(["t1"]);
    expect(backend.killed).toEqual([]);
    expectRunningEqualsReachable();
  });

  it("explicit Hide hides even an idle shell (it keeps running in the tray)", async () => {
    seed({ t1: null, t2: null });
    hideTab(TEAM, WS, "t1");
    await flush();
    expect(hiddenIn(WS)).toEqual(["t1"]);
    expect(backend.killed).toEqual([]);
    expectRunningEqualsReachable();
  });

  it("a hidden tab whose process ended stays in the tray, marked, until the user acts", async () => {
    seed({ t1: ["claude"], t2: null });
    hideTab(TEAM, WS, "t1");
    await flush();
    terminalHost().exit(sid("t1"), 0);
    expect(hiddenIn(WS)).toEqual(["t1"]);
    expect(
      selectHiddenTerminalTabs(useViewerStore.getState(), TEAM, WS).find((t) => t.id === "t1")
        ?.exit,
    ).toEqual({ code: 0, signal: null });
  });

  it("End on a tile kills its hidden tabs too — nothing orphaned", async () => {
    seed({ t1: ["claude"], t2: null });
    hideTab(TEAM, WS, "t1");
    await flush();
    const ending = endWorkspace(TEAM, WS);
    await answer(true);
    await ending;
    expect(backend.killed.sort()).toEqual([sid("t1"), sid("t2")]);
    expect(hiddenIn(WS)).toEqual([]);
    expectRunningEqualsReachable();
  });

  it("Hide on the tile keeps its hidden tabs reachable (they ride along into the tray)", async () => {
    seed({ t1: ["claude"], t2: null });
    hideTab(TEAM, WS, "t1");
    await flush();
    hideWorkspace(TEAM, WS);
    expect(backend.killed).toEqual([]);
    expectRunningEqualsReachable();
    useViewerStore.getState().reopenSoftClosedWorkspace(TEAM, WS);
    expect(tabsIn(WS).sort()).toEqual(["t1", "t2"]);
    expectRunningEqualsReachable();
  });

  it("End on a RUNNING tab asks first, naming the process — Cancel keeps it", async () => {
    seed({ t1: ["claude"], t2: null });
    const ending = endTabs(TEAM, WS, { tabId: "t1" });
    await answer(false);
    await ending;
    expect(backend.killed).toEqual([]);
    expect(tabsIn(WS)).toEqual(["t1", "t2"]);
    expectRunningEqualsReachable();
  });

  it("End on a RUNNING tab — the dialog names it, and End kills it", async () => {
    seed({ t1: ["claude"], t2: null });
    const ending = endTabs(TEAM, WS, { tabId: "t1" });
    await waitFor(() => expect(useEndConfirmStore.getState().request).not.toBeNull());
    const req = useEndConfirmStore.getState().request!;
    expect(req.title).toBe("End “t1”? It is still running.");
    expect(req.running).toEqual(["t1 — claude"]);
    expect(req.confirmLabel).toBe("End");
    settleEndConfirm(true);
    await ending;
    expect(backend.killed).toEqual([sid("t1")]);
    expect(tabsIn(WS)).toEqual(["t2"]);
    expectRunningEqualsReachable();
  });

  it("End on an IDLE tab kills it with no dialog", async () => {
    seed({ t1: null, t2: null });
    await endTabs(TEAM, WS, { tabId: "t1" });
    expect(backend.killed).toEqual([sid("t1")]);
    expect(useEndConfirmStore.getState().request).toBeNull();
  });

  it("End Others with anything running asks with the count; confirming spares the kept tab", async () => {
    seed({ t1: null, t2: ["claude"], t3: { bg: ["npm", "node"] }, t4: null });
    const ending = endTabs(TEAM, WS, { except: "t1" });
    await waitFor(() => expect(useEndConfirmStore.getState().request).not.toBeNull());
    const req = useEndConfirmStore.getState().request!;
    expect(req.title).toBe("End the other open terminals? 2 of 3 terminals are still running.");
    expect(req.running).toEqual(["t2 — claude", "t3 — npm (background), node (background)"]);
    expect(req.confirmLabel).toBe("End 3 terminals");
    settleEndConfirm(true);
    await ending;
    expect(backend.killed.sort()).toEqual([sid("t2"), sid("t3"), sid("t4")]);
    expect(tabsIn(WS)).toEqual(["t1"]);
    expectRunningEqualsReachable();
  });

  it("End All with anything running asks; Cancel ends nothing", async () => {
    seed({ t1: ["claude"], t2: null });
    const ending = endTabs(TEAM, WS, "all");
    await answer(false);
    await ending;
    expect(backend.killed).toEqual([]);
    expect(tabsIn(WS)).toEqual(["t1", "t2"]);
    expectRunningEqualsReachable();
  });

  it("End All with nothing running ends everything with no dialog", async () => {
    seed({ t1: null, t2: null });
    await endTabs(TEAM, WS, "all");
    expect(backend.killed.sort()).toEqual([sid("t1"), sid("t2")]);
    expect(useEndConfirmStore.getState().request).toBeNull();
    expectRunningEqualsReachable();
  });

  it("tile × with anything running Hides the tile into the tray (all its terminals live)", async () => {
    seed({ t1: ["claude"], t2: null });
    await closeWorkspace(TEAM, WS);
    expect(useViewerStore.getState().workspaces[TEAM]?.some((w) => w.id === WS)).toBe(false);
    expect(useViewerStore.getState().softClosedWorkspaces[TEAM]?.[WS]).toBeDefined();
    expect(tabsIn(WS)).toEqual(["t1", "t2"]);
    expect(backend.killed).toEqual([]);
    expectRunningEqualsReachable();
  });

  it("tile × with everything idle Ends the tile (no tray shadow)", async () => {
    seed({ t1: null, t2: null });
    await closeWorkspace(TEAM, WS);
    expect(useViewerStore.getState().softClosedWorkspaces[TEAM]?.[WS]).toBeUndefined();
    expect(backend.killed.sort()).toEqual([sid("t1"), sid("t2")]);
    expectRunningEqualsReachable();
  });

  it("End on a running tile asks; confirming kills its terminals", async () => {
    seed({ t1: ["vim"], t2: null });
    const ending = endWorkspace(TEAM, WS);
    await waitFor(() => expect(useEndConfirmStore.getState().request).not.toBeNull());
    expect(useEndConfirmStore.getState().request!.running).toEqual(["t1 — vim"]);
    settleEndConfirm(true);
    await ending;
    expect(backend.killed.sort()).toEqual([sid("t1"), sid("t2")]);
    expect(useViewerStore.getState().workspaces[TEAM]?.some((w) => w.id === WS)).toBe(false);
    expectRunningEqualsReachable();
  });

  it("End on a tile hidden in the tray asks too, and kills from the tray", async () => {
    seed({ t1: ["claude"] });
    hideWorkspace(TEAM, WS);
    const ending = endWorkspace(TEAM, WS);
    await answer(true);
    await ending;
    expect(backend.killed).toEqual([sid("t1")]);
    expect(useViewerStore.getState().softClosedWorkspaces[TEAM]?.[WS]).toBeUndefined();
    expectRunningEqualsReachable();
  });
});

describe("a visible way back — hidden running sessions restore intact", () => {
  it("a hidden tile restores from the tray onto the SAME live sessions", async () => {
    seed({ t1: ["claude"], t2: ["npm"] });
    hideWorkspace(TEAM, WS);
    expectRunningEqualsReachable();
    useViewerStore.getState().reopenSoftClosedWorkspace(TEAM, WS);
    expect(selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS)).toMatchObject([
      tab("t1"),
      tab("t2"),
    ]);
    expect(backend.killed).toEqual([]);
    expectRunningEqualsReachable();
  });

  it("a hidden tab is listed under its tray chip; Restore puts it back onto the SAME session", async () => {
    const user = userEvent.setup();
    seed({ t1: ["claude"], t2: null });
    render(
      <WorkspaceTray teamKey={TEAM} tree={null} isLightTheme={false} onNewWorkspace={vi.fn()} />,
    );
    await closeTab(TEAM, WS, "t1");
    // The chip shows a count badge; it never fades (only Needs you and Working move).
    const badge = await screen.findByTestId(`tray-chip-hidden-${WS}`);
    expect(badge).toHaveTextContent("1");
    expect(badge.className).not.toContain("animate-pulse");
    expect(badge).toHaveAccessibleName("1 hidden tab — restore");
    // One click opens the list; one more restores.
    await user.click(badge);
    await user.click(screen.getByText("Restore “t1”"));
    await waitFor(() => expect(tabsIn(WS)).toEqual(["t2", "t1"]));
    expect(selectWorkspaceTabs(useViewerStore.getState(), TEAM, WS)[1]).toMatchObject(tab("t1"));
    expect(selectActiveTerminalTab(useViewerStore.getState(), TEAM, WS)).toBe("t1");
    expect(hiddenIn(WS)).toEqual([]);
    expect(screen.queryByTestId(`tray-chip-hidden-${WS}`)).toBeNull();
    // Same PTY, never killed or respawned — the session (and its output) is intact.
    expect(backend.killed).toEqual([]);
    expectRunningEqualsReachable();
  });

  it("a hidden tab can be Ended from its tray chip — asking first when it runs", async () => {
    const user = userEvent.setup();
    seed({ t1: ["claude"], t2: null, t3: null });
    render(
      <>
        <WorkspaceTray teamKey={TEAM} tree={null} isLightTheme={false} onNewWorkspace={vi.fn()} />
        <EndConfirmDialog />
      </>,
    );
    await closeTab(TEAM, WS, "t1"); // running → hidden
    hideTab(TEAM, WS, "t2"); // idle, hidden explicitly
    await flush();
    // Running: End asks; Cancel keeps it hidden and running.
    await waitFor(() =>
      expect(screen.getByTestId(`tray-chip-hidden-${WS}`)).toHaveTextContent("2"),
    );
    await user.click(screen.getByTestId(`tray-chip-hidden-${WS}`));
    await user.click(screen.getByText("End “t1”"));
    await screen.findByTestId("end-confirm-dialog");
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByTestId("end-confirm-dialog")).toBeNull());
    expect(hiddenIn(WS)).toEqual(["t1", "t2"]);
    expect(backend.killed).toEqual([]);
    // …End kills it and drops it from the tray.
    await user.click(screen.getByTestId(`tray-chip-hidden-${WS}`));
    await user.click(screen.getByText("End “t1”"));
    await user.click(await screen.findByRole("button", { name: "End" }));
    await waitFor(() => expect(hiddenIn(WS)).toEqual(["t2"]));
    expect(backend.killed).toEqual([sid("t1")]);
    // Idle: no dialog.
    await user.click(screen.getByTestId(`tray-chip-hidden-${WS}`));
    await user.click(screen.getByText("End “t2”"));
    await waitFor(() => expect(hiddenIn(WS)).toEqual([]));
    expect(backend.killed).toEqual([sid("t1"), sid("t2")]);
    expect(screen.queryByTestId("end-confirm-dialog")).toBeNull();
    expectRunningEqualsReachable();
  });

  it("End All ends the tile's OPEN tabs only — hidden tabs keep running, and the dialog says so", async () => {
    seed({ t1: ["claude"], t2: ["vim"], t3: null });
    hideTab(TEAM, WS, "t1");
    await flush();
    const ending = endTabs(TEAM, WS, "all");
    await waitFor(() => expect(useEndConfirmStore.getState().request).not.toBeNull());
    expect(useEndConfirmStore.getState().request!.note).toBe(
      "1 hidden tab keeps running in the tray.",
    );
    settleEndConfirm(true);
    await ending;
    expect(backend.killed.sort()).toEqual([sid("t2"), sid("t3")]);
    expect(hiddenIn(WS)).toEqual(["t1"]);
    expect(alive().has(sid("t1"))).toBe(true);
    expectRunningEqualsReachable();
  });

  it("a hidden AGENT tab brought forward from the Agents panel restores in place (no duplicate)", async () => {
    useOrgStore.setState({ activeOrgSlug: "acme", activeTeamName: "default" });
    seed({ t1: ["claude"], t2: null });
    await closeTab(TEAM, WS, "t1");
    const row = {
      handle: sid("t1"),
      sessionId: sid("t1"),
      kind: "registered",
      alive: true,
      backgrounded: false,
      startedWith: null,
      startedAtMs: 0,
      launcher: null,
    } as unknown as AgentSessionRow;
    const { result } = renderHook(() => useAgentDrill(TEAM));
    act(() => result.current.onBringToForeground(row));
    await waitFor(() => expect(hiddenIn(WS)).toEqual([]));
    const all = Object.values(selectTerminalTabIndex(useViewerStore.getState())[TEAM] ?? {}).flat();
    expect(all.filter((t) => t.sessionId === sid("t1"))).toMatchObject([tab("t1")]);
    expect(backend.killed).toEqual([]);
    expectRunningEqualsReachable();
  });
});

describe("the End confirmation dialog — in-app, keyboard-first", () => {
  it("opens naming what runs; Enter is the safe default (Cancel), Esc cancels, End kills", async () => {
    const user = userEvent.setup();
    seed({ t1: ["claude"], t2: null });
    render(<EndConfirmDialog />);

    // Enter → Cancel (focused by default): nothing ends.
    let ending = endTabs(TEAM, WS, "all");
    const dialog = await screen.findByTestId("end-confirm-dialog");
    expect(dialog).toHaveTextContent("End all open terminals? 1 of 2 terminals is still running.");
    expect(screen.getByTestId("end-confirm-running")).toHaveTextContent("t1 — claude");
    // The running list is part of the dialog's accessible description (announced).
    const described = document.getElementById(dialog.getAttribute("aria-describedby")!);
    expect(described).toHaveTextContent("t1 — claude");
    await waitFor(() => expect(document.activeElement).toHaveTextContent("Cancel"));
    await user.keyboard("{Enter}");
    await ending;
    expect(backend.killed).toEqual([]);
    await waitFor(() => expect(screen.queryByTestId("end-confirm-dialog")).toBeNull());

    // Esc cancels.
    ending = endTabs(TEAM, WS, "all");
    await screen.findByTestId("end-confirm-dialog");
    await user.keyboard("{Escape}");
    await ending;
    expect(backend.killed).toEqual([]);

    // Only the explicit End kills.
    ending = endTabs(TEAM, WS, "all");
    await screen.findByTestId("end-confirm-dialog");
    await user.click(screen.getByRole("button", { name: "End 2 terminals" }));
    await ending;
    expect(backend.killed.sort()).toEqual([sid("t1"), sid("t2")]);
    expectRunningEqualsReachable();
  });
});

describe("through the real tile chrome — middle-click and the tab menu", () => {
  function Tile() {
    const tabs = useViewerStore((s) => selectWorkspaceTabs(s, TEAM, WS));
    const actions = useTileTabActions(TEAM, WS, () => {});
    return (
      <>
        <TerminalTileChrome
          workspace={story(WS)}
          isFocused
          tabs={tabs}
          activeTabId={tabs[0]?.id}
          onFocusTile={() => {}}
          onHideTile={() => hideWorkspace(TEAM, WS)}
          onCloseTile={() => void closeWorkspace(TEAM, WS)}
          {...actions}
        />
        <EndConfirmDialog />
      </>
    );
  }
  const tabButton = (name: string) => screen.getByText(name).closest("button") as HTMLElement;

  it("middle-click on a running tab Hides it; on an idle tab Ends it", async () => {
    seed({ t1: ["claude"], t2: null, t3: null });
    render(<Tile />);
    fireEvent.pointerDown(tabButton("t1"), { button: 1 });
    await waitFor(() => expect(tabsIn(WS)).toEqual(["t2", "t3"]));
    expect(hiddenIn(WS)).toEqual(["t1"]);
    fireEvent.pointerDown(tabButton("t2"), { button: 1 });
    await waitFor(() => expect(tabsIn(WS)).toEqual(["t3"]));
    expect(backend.killed).toEqual([sid("t2")]);
    expectRunningEqualsReachable();
  });

  it("the tab × names its verb (Hide when running, End when idle) after hover", async () => {
    seed({ t1: ["claude"], t2: null });
    render(<Tile />);
    // The × is the mouse's close: its tooltip names the verb (and the key).
    const closeOf = (t: string) =>
      tabButton(t).parentElement!.querySelector<HTMLElement>('button[aria-hidden="true"]')!;
    const tip = async (t: string) => {
      await userEvent.unhover(document.body);
      await userEvent.hover(closeOf(t));
      return (await screen.findByRole("tooltip")).textContent;
    };
    fireEvent.pointerEnter(closeOf("t2"));
    await flush();
    await waitFor(async () => expect(await tip("t2")).toMatch(/^End /));
    fireEvent.pointerEnter(closeOf("t1"));
    await flush();
    expect(await tip("t1")).toMatch(/^Hide · keeps running/);
  });

  it("tab menu End All with a running tab confirms in-app before anything dies", async () => {
    const user = userEvent.setup();
    seed({ t1: ["claude"], t2: null });
    render(<Tile />);
    fireEvent.contextMenu(tabButton("t2"));
    await user.click(screen.getByText("End All"));
    await screen.findByTestId("end-confirm-dialog");
    expect(backend.killed).toEqual([]);
    await user.click(screen.getByRole("button", { name: "End 2 terminals" }));
    await waitFor(() => expect(backend.killed.sort()).toEqual([sid("t1"), sid("t2")]));
    expectRunningEqualsReachable();
  });

  it("tab menu Hide sends a tab to the tray without a kill", async () => {
    const user = userEvent.setup();
    seed({ t1: null, t2: null });
    render(<Tile />);
    fireEvent.contextMenu(tabButton("t1"));
    await user.click(screen.getByText("Hide"));
    await waitFor(() => expect(hiddenIn(WS)).toEqual(["t1"]));
    expect(backend.killed).toEqual([]);
    expectRunningEqualsReachable();
  });
});

describe("the folded ⋮ close verbs on a narrow tile", () => {
  // A tile too narrow for inline controls folds Hide and × into ⋮.
  class NarrowResizeObserver {
    constructor(private cb: ResizeObserverCallback) {}
    observe(el: Element) {
      const width = el.getAttribute("data-testid")?.startsWith("tile-chrome-") ? 120 : 0;
      this.cb(
        [{ contentRect: { width } } as ResizeObserverEntry],
        this as unknown as ResizeObserver,
      );
    }
    unobserve() {}
    disconnect() {}
  }
  beforeEach(() => vi.stubGlobal("ResizeObserver", NarrowResizeObserver));
  afterEach(() => vi.unstubAllGlobals());

  function NarrowTile() {
    const tabs = useViewerStore((s) => selectWorkspaceTabs(s, TEAM, WS));
    const actions = useTileTabActions(TEAM, WS, () => {});
    return (
      <>
        <TerminalTileChrome
          workspace={story(WS)}
          isFocused
          tabs={tabs}
          activeTabId={tabs[0]?.id}
          onFocusTile={() => {}}
          onToggleZoom={() => {}}
          onHideTile={() => hideWorkspace(TEAM, WS)}
          onCloseTile={() => void closeWorkspace(TEAM, WS)}
          {...actions}
        />
        <EndConfirmDialog />
      </>
    );
  }
  const liveTile = () => useViewerStore.getState().workspaces[TEAM]?.some((w) => w.id === WS);
  const shadowOf = () => useViewerStore.getState().softClosedWorkspaces[TEAM]?.[WS];

  async function openMenu(user: ReturnType<typeof userEvent.setup>) {
    expect(screen.queryByTestId(`tile-close-${WS}`)).toBeNull(); // folded, not inline
    await user.click(screen.getByTestId(`tile-view-options-${WS}`));
  }

  it("running: the folded × reads Hide and Hides the tile into the tray", async () => {
    const user = userEvent.setup();
    seed({ t1: ["claude"], t2: null });
    render(<NarrowTile />);
    await openMenu(user);
    const close = await screen.findByTestId(`tile-menu-close-${WS}`);
    await waitFor(() => expect(close).toHaveTextContent("Hide · keeps running"));
    await user.click(close);
    await waitFor(() => expect(liveTile()).toBe(false));
    expect(shadowOf()).toBeDefined();
    expect(tabsIn(WS)).toEqual(["t1", "t2"]);
    expect(backend.killed).toEqual([]);
    expectRunningEqualsReachable();
  });

  it("idle: opening ⋮ probes, the folded × reads End, and Ends with no dialog", async () => {
    const user = userEvent.setup();
    seed({ t1: null, t2: null });
    render(<NarrowTile />);
    await openMenu(user);
    const close = await screen.findByTestId(`tile-menu-close-${WS}`);
    await waitFor(() => expect(close).toHaveTextContent(/^End$/));
    await user.click(close);
    await waitFor(() => expect(backend.killed.sort()).toEqual([sid("t1"), sid("t2")]));
    expect(shadowOf()).toBeUndefined();
    expect(screen.queryByTestId("end-confirm-dialog")).toBeNull();
    expectRunningEqualsReachable();
  });

  it("background jobs: the folded × Ends only after the in-app confirmation", async () => {
    const user = userEvent.setup();
    seed({ t1: { bg: ["vite"] }, t2: null });
    render(<NarrowTile />);
    await openMenu(user);
    await user.click(await screen.findByTestId(`tile-menu-close-${WS}`));
    await screen.findByTestId("end-confirm-dialog");
    expect(backend.killed).toEqual([]);
    await user.click(screen.getByRole("button", { name: "End 2 terminals" }));
    await waitFor(() => expect(backend.killed.sort()).toEqual([sid("t1"), sid("t2")]));
    expectRunningEqualsReachable();
  });

  it("the folded Hide entry always Hides — even an idle tile — never Ends", async () => {
    const user = userEvent.setup();
    seed({ t1: null, t2: null });
    render(<NarrowTile />);
    await openMenu(user);
    const hide = await screen.findByTestId(`tile-menu-hide-${WS}`);
    expect(hide).toHaveTextContent("Hide · keeps running");
    await user.click(hide);
    await waitFor(() => expect(liveTile()).toBe(false));
    expect(shadowOf()).toBeDefined();
    expect(backend.killed).toEqual([]);
    expectRunningEqualsReachable();
  });
});
