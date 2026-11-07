// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@/test-utils";

import { endWorkspace, hideTab, hideWorkspace } from "./terminal-close";
import { WorkspaceTray } from "../components/workspace-tray";
import {
  makeRealSendToWorkspaceTabDeps,
  sendToWorkspaceTab,
} from "@/features/dashboard/components/workspace-tab-verbs";
import { collectTabSessionIds } from "@/shared/hooks/use-agent-liveness-monitor";
import {
  selectActiveTerminalTab,
  selectHiddenTerminalTabs,
  selectWorkspaceTabs,
  useViewerStore,
  type Workspace,
} from "@/shared/stores/viewer-store";
import { EMPTY_TERMINAL_LAYOUT, USER_HIDDEN_TAG } from "@/shared/lib/terminal-layout-projection";
import {
  installTerminalLayoutHarness,
  seedTerminalTabs,
  terminalHost,
  type SeedTab,
} from "@/test/terminal-layout-harness";

/**
 * Hidden tabs are still running tabs: every "which sessions exist here" scan must
 * see them, and every tile teardown must carry them along — never strand them.
 * Real store over the test terminal host (a Hidden tab is a Background entry of
 * its workspace's layout).
 */
vi.mock("@/tauri/commands", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));

installTerminalLayoutHarness();

const TEAM = "acme:default";
const OTHER_TEAM = "acme:other";
const ws = (id: string, extra: Partial<Workspace> = {}): Workspace => ({
  id,
  kind: "story",
  cwd: `/${id}`,
  status: "ready",
  ...extra,
});
const settle = () => new Promise((r) => setTimeout(r));

function seed(workspaces: Workspace[], tabs: Record<string, SeedTab[]>, team = TEAM) {
  useViewerStore.setState({
    workspaces: { [team]: workspaces },
    activeWorkspace: { [team]: workspaces[0]?.id ?? "" },
    softClosedWorkspaces: { [team]: {} },
    aiStatusBySession: {},
    foregroundLayout: {},
  });
  for (const [w, list] of Object.entries(tabs)) seedTerminalTabs(team, w, list);
}
const s = () => useViewerStore.getState();
const hiddenIn = (w: string, team = TEAM) =>
  selectHiddenTerminalTabs(s(), team, w).map((t) => t.id);
const killed = (session: string) => !terminalHost().contains(session);

beforeEach(() => {
  resetSessionProjection();
  useViewerStore.setState({ softClosedWorkspaces: {} });
});

describe("every open-tab scan includes Hidden tabs", () => {
  it("a dashboard send reaches a hidden tab where it is (never yanked back onto the tile)", async () => {
    seed([ws("g1", { kind: "generic" })], { g1: ["a", "b"] });
    hideTab(TEAM, "g1", "a");
    await settle();
    const sendCommand = vi.fn(async () => ({ submitted: true }));
    const deps = { ...makeRealSendToWorkspaceTabDeps(), sendCommand };
    await expect(
      sendToWorkspaceTab({ sessionId: "a", mode: "command", text: "ls" }, deps),
    ).resolves.toEqual({ submitted: true });
    expect(sendCommand).toHaveBeenCalledWith("a", "ls");
    expect(hiddenIn("g1")).toEqual(["a"]); // still hidden
  });

  it("a hidden tab stays watched, and its status reaches the tray chip", async () => {
    seed([ws("w1")], { w1: ["a", "b"] });
    hideTab(TEAM, "w1", "a");
    await settle();
    expect(collectTabSessionIds(s())).toContain("a");
    render(
      <WorkspaceTray teamKey={TEAM} tree={null} isLightTheme={false} onNewWorkspace={vi.fn()} />,
    );
    const badge = await screen.findByTestId("tray-chip-hidden-w1");
    expect(badge).toHaveAccessibleName("1 hidden tab — restore");
    // The watcher raises needs-permission for the hidden session → the chip shows it.
    projectSessionStatus("a", "paused");
    await waitFor(() =>
      expect(screen.getByTestId("tray-chip-hidden-w1")).toHaveAccessibleName(
        "1 hidden tab — restore, Needs approval",
      ),
    );
  });

  it("the chip menu lists hidden tabs as a labelled group of its menu", async () => {
    seed([ws("w1")], { w1: ["a", "b"] });
    hideTab(TEAM, "w1", "a");
    await settle();
    render(
      <WorkspaceTray teamKey={TEAM} tree={null} isLightTheme={false} onNewWorkspace={vi.fn()} />,
    );
    await screen.findByTestId("tray-chip-hidden-w1");
    fireEvent.contextMenu(document.querySelector('[data-tray-chip="w1"] button') as HTMLElement);
    const menu = screen.getByRole("menu", { name: "Workspace actions" });
    const group = within(menu).getByRole("group", { name: "Hidden tabs" });
    expect(
      within(group)
        .getAllByRole("menuitem")
        .map((el) => el.textContent),
    ).toEqual([expect.stringMatching(/^Restore “/), expect.stringMatching(/^End “/)]);
    // The tile's own verbs sit outside the group.
    expect(within(group).queryByRole("menuitem", { name: "Hide" })).toBeNull();
    expect(within(menu).getByRole("menuitem", { name: "Hide" })).toBeInTheDocument();
  });
});

describe("tile teardowns never strand hidden tabs", () => {
  it("replacing the focused tile folds its hidden tabs into its tray shadow", async () => {
    seed([ws("w1")], { w1: ["a", "b"] });
    useViewerStore.setState({
      softClosedWorkspaces: { [TEAM]: { w2: { workspace: ws("w2") } } },
    });
    seedTerminalTabs(TEAM, "w2", ["c"]);
    hideTab(TEAM, "w1", "a");
    await settle();
    s().replaceFocusedLeafWorkspace(TEAM, "w2");
    expect(s().workspaces[TEAM].map((w) => w.id)).toEqual(["w2"]);
    expect(s().softClosedWorkspaces[TEAM]?.w1).toBeDefined();
    expect(
      selectWorkspaceTabs(s(), TEAM, "w1")
        .map((t) => t.id)
        .sort(),
    ).toEqual(["a", "b"]);
    expect(hiddenIn("w1")).toEqual([]);
    expect(["a", "b", "c"].some(killed)).toBe(false);
  });

  it("closeTeam kills hidden tabs and sweeps the hidden list", async () => {
    seed([ws("w1")], { w1: ["a", "b"] });
    hideTab(TEAM, "w1", "a");
    await settle();
    s().closeTeam(TEAM);
    await settle();
    expect(killed("a")).toBe(true);
    expect(hiddenIn("w1")).toEqual([]);
  });

  it("a team switch leaves another team's hidden tabs alone — back again, still listed", async () => {
    seed([ws("w1")], { w1: ["a", "b"] });
    hideTab(TEAM, "w1", "a");
    await settle();
    // Working in (and tearing down) another team touches nothing of this one.
    useViewerStore.setState((st) => ({
      workspaces: { ...st.workspaces, [OTHER_TEAM]: [ws("x1")] },
    }));
    seedTerminalTabs(OTHER_TEAM, "x1", ["x"]);
    s().closeTeam(OTHER_TEAM);
    await settle();
    expect(hiddenIn("w1")).toEqual(["a"]);
    expect(killed("a")).toBe(false);
    render(
      <WorkspaceTray teamKey={TEAM} tree={null} isLightTheme={false} onNewWorkspace={vi.fn()} />,
    );
    expect(screen.getByTestId("tray-chip-hidden-w1")).toHaveTextContent("1");
  });

  it("restore after the tile moved into the tray brings the tile back, then the tab", async () => {
    // A hidden tab whose tile has since gone to the tray.
    seed([ws("w2")], { w2: ["c"] });
    useViewerStore.setState({
      softClosedWorkspaces: { [TEAM]: { w1: { workspace: ws("w1") } } },
    });
    seedTerminalTabs(TEAM, "w1", [
      "b",
      { session: "a", placement: "background", tags: { [USER_HIDDEN_TAG]: "tab" } },
    ]);
    expect(hiddenIn("w1")).toEqual(["a"]);
    s().restoreHiddenTerminalTab(TEAM, "w1", "a");
    await settle();
    expect(
      s()
        .workspaces[TEAM].map((w) => w.id)
        .sort(),
    ).toEqual(["w1", "w2"]);
    expect(selectWorkspaceTabs(s(), TEAM, "w1").map((t) => t.id)).toEqual(["b", "a"]);
    expect(selectActiveTerminalTab(s(), TEAM, "w1")).toBe("a");
    expect(hiddenIn("w1")).toEqual([]);
    expect(["a", "b", "c"].some(killed)).toBe(false);
  });
});

describe("a headless launch into the tile's workspace is never one of its Hidden tabs", () => {
  // A sprint / weft / MCP headless agent runs as a Background entry under the
  // story's own workspace key — it belongs to the Agents panel, not the tray.
  const seedHeadless = () => {
    seed([ws("w1"), ws("w2")], { w1: ["a", "b"], w2: ["c"] });
    terminalHost().seed(
      `${TEAM}|w1`,
      { displayName: "w1", kind: "story" },
      {
        session: "agent",
        role: "sprint",
        placement: "background",
      },
    );
    expect(hiddenIn("w1")).toEqual([]);
  };

  it("is not listed or counted on the tray chip", async () => {
    seedHeadless();
    hideTab(TEAM, "w1", "a");
    await settle();
    expect(hiddenIn("w1")).toEqual(["a"]);
    render(
      <WorkspaceTray teamKey={TEAM} tree={null} isLightTheme={false} onNewWorkspace={vi.fn()} />,
    );
    expect(screen.getByTestId("tray-chip-hidden-w1")).toHaveTextContent("1");
  });

  it("Hide tile leaves it in the background (never folded onto the tile)", async () => {
    seedHeadless();
    hideWorkspace(TEAM, "w1");
    await settle();
    expect(selectWorkspaceTabs(s(), TEAM, "w1").map((t) => t.id)).toEqual(["a", "b"]);
    expect(killed("agent")).toBe(false);
  });

  it("End tile and closeTeam leave it running", async () => {
    seedHeadless();
    await endWorkspace(TEAM, "w1", new Map());
    await settle();
    expect(killed("a")).toBe(true);
    expect(killed("agent")).toBe(false);
    s().closeTeam(TEAM);
    await settle();
    expect(killed("c")).toBe(true);
    expect(killed("agent")).toBe(false);
  });
});

describe("two quick hides never empty the strip", () => {
  it("the second hide on a two-tab tile, before the host answers the first, hides the tile", async () => {
    seed([ws("w1"), ws("w2")], { w1: ["a", "b"], w2: ["c"] });
    // Neither hide has reached the host when the second is decided.
    expect(s().hideTerminalTab(TEAM, "a", "w1")).toBe(true);
    expect(s().hideTerminalTab(TEAM, "b", "w1")).toBe(false);
    hideTab(TEAM, "w1", "b");
    await settle();
    await settle();
    // The tile went to the tray with both tabs folded back on it.
    expect(s().softClosedWorkspaces[TEAM]?.w1).toBeDefined();
    expect(
      selectWorkspaceTabs(s(), TEAM, "w1")
        .map((t) => t.id)
        .sort(),
    ).toEqual(["a", "b"]);
    expect(hiddenIn("w1")).toEqual([]);
    expect(["a", "b"].some(killed)).toBe(false);
  });
});

describe("the tray survives a relaunch (SW-RT-9): its terminals are the host's", () => {
  it("a hidden tab and a tray-hidden tile are still in the tray after a relaunch-shaped rehydrate", async () => {
    seed([ws("w1"), ws("w2")], { w1: ["a", "b"], w2: ["c"] });
    hideTab(TEAM, "w1", "a");
    await settle();
    hideWorkspace(TEAM, "w2");
    // Quit: what the viewer persists (its tiles and the tray's hidden tiles)…
    const persisted = {
      workspaces: s().workspaces,
      softClosedWorkspaces: s().softClosedWorkspaces,
    };
    // …relaunch: a fresh viewer restores those records and rebuilds every
    // terminal from its host's snapshot (the host kept them running).
    useViewerStore.setState({
      terminalLayout: EMPTY_TERMINAL_LAYOUT,
      workspaces: {},
      softClosedWorkspaces: {},
    });
    useViewerStore.setState(persisted);
    s().applyLayout({ kind: "snapshot", replies: [terminalHost().reply()] });

    expect(hiddenIn("w1")).toEqual(["a"]);
    expect(s().softClosedWorkspaces[TEAM]?.w2).toBeDefined();
    expect(selectWorkspaceTabs(s(), TEAM, "w2").map((t) => t.id)).toEqual(["c"]);
    render(
      <WorkspaceTray teamKey={TEAM} tree={null} isLightTheme={false} onNewWorkspace={vi.fn()} />,
    );
    expect(screen.getByTestId("tray-chip-hidden-w1")).toHaveTextContent("1");
    expect(document.querySelector('[data-tray-chip="w2"]')).not.toBeNull();
    expect(["a", "b", "c"].some(killed)).toBe(false);
  });
});
