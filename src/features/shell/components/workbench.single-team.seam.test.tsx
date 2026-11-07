// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

import { TooltipProvider } from "@/components/ui/tooltip";
import { installMockBackend } from "@/app/mock/install";
import { MOCK_ORG_SLUG } from "@/app/mock/fixtures";
import { ROW_POPUP_ATTR, TERMINAL_POPUP_ATTR } from "@/shared/lib/focus-utils";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { isTerminalColumnShown } from "@/features/viewer/lib/terminal-column";
import { buildSyntheticRepoPathFromPrompt } from "@/shared/lib/build-synthetic-repo";
import { useOrgStore } from "@/shared/stores/org-store";
import { selectWorkspaceKey, useViewerStore, type Workspace } from "@/shared/stores/viewer-store";
import type { ProjectedWorkspace } from "@/shared/lib/terminal-layout-projection";

// The CLI's tiles are drawn by the panels; here they only say which team they were given, the
// workspace focused in it and that workspace's active tab.
vi.mock("@/features/viewer/components/workspace-terminal-panels", async () => {
  const { useViewerStore: store, selectActiveTerminalTab } =
    await import("@/shared/stores/viewer-store");
  return {
    WorkspaceTerminalPanels: ({
      teamKey,
      repoPath,
      teamHidden,
    }: {
      teamKey: string;
      repoPath: string;
      teamHidden?: boolean;
    }) => {
      const focused = store((s) => s.activeWorkspace[teamKey] ?? "");
      const tab = store((s) => selectActiveTerminalTab(s, teamKey, focused) ?? "");
      return (
        <div
          // A warm team's grid stays mounted, hidden: only the shown team's is "cli-panels".
          data-testid={teamHidden ? "cli-panels-hidden" : "cli-panels"}
          data-team={teamKey}
          data-repo={repoPath}
          data-focused={focused}
          data-tab={tab}
        />
      );
    },
  };
});

// The workbench's DndContext (by its tile-zone collision; the right pane draws one of its own), as
// drawn: a test ends a drag through the workbench's own drag-end handler.
const dnd = vi.hoisted(() => ({
  onDragEnd: null as
    | ((event: { active: { id: string }; over: { id: string } | null }) => void)
    | null,
}));
vi.mock("@dnd-kit/core", async (importOriginal) => {
  const core = await importOriginal<typeof import("@dnd-kit/core")>();
  return {
    ...core,
    DndContext: (props: React.ComponentProps<typeof core.DndContext>) => {
      if (props.collisionDetection?.name === "tileZonePriorityCollision")
        dnd.onDragEnd = props.onDragEnd as typeof dnd.onDragEnd;
      return <core.DndContext {...props} />;
    },
  };
});

import { setTrayReorderIntent } from "@/features/viewer/lib/tray-reorder-drag";
import { tileZoneId } from "@/features/viewer/lib/terminal-drag";
import { Workbench } from "./workbench";

/**
 * The workbench over the mock backend with two open teams. Its one CLI shows the active team, the
 * same team as the nav, the viewer and the pills, and a team switch draws the new team's tiles
 * while each team keeps its own focused workspace and active tabs for its return.
 */
const ACTIVE = `${MOCK_ORG_SLUG}:default`;
const OTHER = `${MOCK_ORG_SLUG}:other`;
const story = (id: string): Workspace => ({ id, kind: "story", cwd: `/${id}`, status: "ready" });

function seed(over: Partial<ReturnType<typeof useViewerStore.getState>> = {}) {
  useViewerStore.setState({
    openedTeams: [ACTIVE, OTHER],
    workspaces: { [ACTIVE]: [story("a-1")], [OTHER]: [story("o-1")] },
    activeWorkspace: { [ACTIVE]: "a-1", [OTHER]: "o-1" },
    softClosedWorkspaces: {},
    layoutMode: "shared",
    terminalVisible: true,
    ...over,
  });
}

/** Sets `workspaceId`'s active tab in the host layout, as a host snapshot would. */
function setActiveTab(teamKey: string, workspaceId: string, tabId: string) {
  const s = useViewerStore.getState();
  const key = selectWorkspaceKey(s, teamKey, workspaceId);
  useViewerStore.setState({
    terminalLayout: {
      ...s.terminalLayout,
      byKey: {
        ...s.terminalLayout.byKey,
        [key]: {
          tabs: [],
          hiddenTabs: [],
          active: tabId,
          grid: { paned: [], preset: "columns", collapsed: false },
        } as unknown as ProjectedWorkspace,
      },
    },
  });
}

/** The workbench with both teams open: the active team's view, the one CLI and the one tray. */
async function mount() {
  const view = render(
    <TooltipProvider>
      <Workbench />
    </TooltipProvider>,
  );
  await act(async () => {});
  return view;
}

describe("Workbench: the one CLI shows the active team", () => {
  const initialLayout = useViewerStore.getState().terminalLayout;
  beforeEach(() => {
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    installMockBackend();
    useOrgStore.setState({ activeOrgSlug: MOCK_ORG_SLUG, activeTeamName: "default" });
  });

  afterEach(() => {
    cleanup();
    document.body.innerHTML = "";
    useViewerStore.setState({
      terminalLayout: initialLayout,
      layoutMode: null,
      terminalVisible: false,
      softClosedWorkspaces: {},
      trayMode: {},
      selectedEpicDirName: {},
      railKeyByWorkspaceId: {},
      epicReposOpen: {},
    });
    useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it("draws the active team's tiles, once", async () => {
    seed();
    await mount();
    await waitFor(() => expect(screen.getAllByTestId("cli-panels")).toHaveLength(1));
    expect(screen.getByTestId("cli-panels")).toHaveAttribute("data-team", ACTIVE);
    expect(screen.getByTestId("cli-panels")).toHaveAttribute("data-focused", "a-1");
  });

  it("the team views draw no CLI part: the CLI, divider, tray and Agents pane are the workbench's, once", async () => {
    seed();
    await mount();
    act(() => useOrgStore.setState({ activeTeamName: "other" }));
    // Both teams' views are mounted now (the one left stays through the switch debounce).
    await waitFor(() => expect(document.querySelectorAll("[data-team-view]")).toHaveLength(2));
    for (const selector of [
      "[data-xterm-host]",
      "[data-workspace-bar]",
      "[data-testid='terminal-drag-handle']",
      "[data-testid='terminal-section']",
    ]) {
      const found = document.querySelectorAll(selector);
      expect(found, selector).toHaveLength(1);
      expect(found[0].closest("[data-team-view]"), selector).toBeNull();
    }
    expect(screen.getAllByTestId("cli-panels")).toHaveLength(1);
  });

  it("a team switch draws the new team's tiles", async () => {
    seed();
    await mount();
    act(() => useOrgStore.setState({ activeTeamName: "other" }));
    await waitFor(() =>
      expect(screen.getByTestId("cli-panels")).toHaveAttribute("data-team", OTHER),
    );
    expect(screen.getAllByTestId("cli-panels")).toHaveLength(1);
    expect(screen.getByTestId("cli-panels")).toHaveAttribute("data-focused", "o-1");
  });

  it("a team keeps its focused workspace and active tabs across a switch away and back", async () => {
    seed({
      workspaces: { [ACTIVE]: [story("a-1"), story("a-2")], [OTHER]: [story("o-1")] },
      activeWorkspace: { [ACTIVE]: "a-2", [OTHER]: "o-1" },
    });
    setActiveTab(ACTIVE, "a-1", "tab-1b");
    setActiveTab(ACTIVE, "a-2", "tab-2c");
    setActiveTab(OTHER, "o-1", "tab-o");
    await mount();
    const panels = () => screen.getByTestId("cli-panels");
    expect(panels()).toHaveAttribute("data-focused", "a-2");
    expect(panels()).toHaveAttribute("data-tab", "tab-2c");

    act(() => useOrgStore.setState({ activeTeamName: "other" }));
    await waitFor(() => expect(panels()).toHaveAttribute("data-team", OTHER));
    expect(panels()).toHaveAttribute("data-tab", "tab-o");
    // Working in the other team leaves the first team's memory alone.
    act(() => useViewerStore.getState().switchWorkspace(OTHER, "o-1"));

    act(() => useOrgStore.setState({ activeTeamName: "default" }));
    await waitFor(() => expect(panels()).toHaveAttribute("data-team", ACTIVE));
    expect(panels()).toHaveAttribute("data-focused", "a-2");
    expect(panels()).toHaveAttribute("data-tab", "tab-2c");
    const s = useViewerStore.getState();
    expect(s.activeWorkspace[ACTIVE]).toBe("a-2");
    const key = selectWorkspaceKey(s, ACTIVE, "a-1");
    expect(s.terminalLayout.byKey[key]?.active).toBe("tab-1b");
  });

  it("an empty CLI counts the team's minimized workspaces", async () => {
    // Grouped, with an epic selected that has no open tile: the CLI is empty.
    seed({
      trayMode: { [ACTIVE]: "grouped" },
      selectedEpicDirName: { [ACTIVE]: "epic-empty" },
      railKeyByWorkspaceId: { [ACTIVE]: { "a-1": "epic-full" } },
      softClosedWorkspaces: {
        [ACTIVE]: { "a-2": { workspace: story("a-2") }, "a-3": { workspace: story("a-3") } },
        [OTHER]: { "o-2": { workspace: story("o-2") } },
      },
    });
    await mount();
    await waitFor(() => expect(screen.getByTestId("terminal-empty-state")).toBeInTheDocument());
    expect(screen.getByTestId("terminal-empty-state")).toHaveTextContent(
      "2 workspaces still running in the background",
    );
  });

  it("with no workspace open, the terminal shortcut hides the empty CLI from the viewer and shows it again (SV-GHI-2)", async () => {
    seed({
      openedTeams: [ACTIVE],
      workspaces: {},
      activeWorkspace: {},
    });
    await mount();
    await waitFor(() => expect(screen.getByTestId("terminal-empty-state")).toBeInTheDocument());
    const viewer = document.querySelector<HTMLElement>("[data-viewer-content]")!;
    act(() => viewer.focus());
    expect(document.activeElement).toBe(viewer);
    const toggle = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-terminal")!;
    act(() => toggle.run());
    expect(isTerminalColumnShown(useViewerStore.getState())).toBe(false);
    act(() => toggle.run());
    expect(isTerminalColumnShown(useViewerStore.getState())).toBe(true);
  });

  it("with no active team the CLI and the tray draw nothing", async () => {
    seed();
    useOrgStore.setState({ activeTeamName: null });
    await mount();
    expect(screen.getByTestId("global-cli")).toBeInTheDocument();
    expect(screen.queryByTestId("cli-panels")).toBeNull();
    expect(screen.queryByTestId("terminal-empty-state")).toBeNull();
    expect(screen.queryByTestId("workspace-tray")).toBeNull();
  });

  it("⌘⇧] and ⌘⇧[ in a CLI terminal cycle the active team's tabs; unshifted they do not", async () => {
    const cycleTerminalTab = vi.fn(() => true);
    const real = useViewerStore.getState().cycleTerminalTab;
    seed({ cycleTerminalTab });
    try {
      await mount();
      const term = document.createElement("div");
      term.className = "xterm";
      term.innerHTML = "<textarea></textarea>";
      screen.getByTestId("global-cli").appendChild(term);
      term.querySelector("textarea")!.focus();
      fireEvent.keyDown(document.activeElement!, { key: "]", metaKey: true });
      fireEvent.keyDown(document.activeElement!, { key: "[", metaKey: true });
      // Unshifted, ⌘] and ⌘[ are the registry's team and workspace forward and back.
      expect(cycleTerminalTab).not.toHaveBeenCalled();
      fireEvent.keyDown(document.activeElement!, {
        key: "}",
        code: "BracketRight",
        metaKey: true,
        shiftKey: true,
      });
      fireEvent.keyDown(document.activeElement!, {
        key: "{",
        code: "BracketLeft",
        metaKey: true,
        shiftKey: true,
      });
      expect(cycleTerminalTab).toHaveBeenNthCalledWith(1, ACTIVE, "next");
      expect(cycleTerminalTab).toHaveBeenNthCalledWith(2, ACTIVE, "prev");
    } finally {
      useViewerStore.setState({ cycleTerminalTab: real });
    }
  });

  it("⌘⇧] in the Agents pane's quick view cycles nothing", async () => {
    const cycleTerminalTab = vi.fn(() => true);
    const real = useViewerStore.getState().cycleTerminalTab;
    seed({ cycleTerminalTab });
    try {
      await mount();
      const popup = document.createElement("div");
      popup.setAttribute(TERMINAL_POPUP_ATTR, "");
      popup.setAttribute(ROW_POPUP_ATTR, "");
      popup.innerHTML = '<div class="xterm"><textarea></textarea></div>';
      document.body.appendChild(popup);
      popup.querySelector("textarea")!.focus();
      fireEvent.keyDown(document.activeElement!, {
        key: "}",
        code: "BracketRight",
        metaKey: true,
        shiftKey: true,
      });
      expect(cycleTerminalTab).not.toHaveBeenCalled();
    } finally {
      useViewerStore.setState({ cycleTerminalTab: real });
    }
  });

  it("its tiles get the repo path of the team's focused workspace", async () => {
    seed({
      workspaces: {
        [ACTIVE]: [{ ...story("a-1"), sourceRepoSlug: "acme/web" }],
        [OTHER]: [{ ...story("o-1"), sourceRepoSlug: "acme/api" }],
      },
    });
    await mount();
    const expected = buildSyntheticRepoPathFromPrompt({
      teamKey: ACTIVE,
      sourceRepoSlug: "acme/web",
    });
    await waitFor(() =>
      expect(screen.getByTestId("cli-panels")).toHaveAttribute("data-repo", expected),
    );
  });

  it("the epic-repos overlay lies over the team's focused epic terminal", async () => {
    const epic: Workspace = {
      id: "epic-1",
      kind: "epic",
      cwd: "/epic-1",
      status: "ready",
      epicDirPath: "epic-one",
      epicDocPath: "/content/epic-one/epic.md",
    };
    seed({
      workspaces: { [ACTIVE]: [epic], [OTHER]: [] },
      activeWorkspace: { [ACTIVE]: "epic-1" },
      epicReposOpen: { [`${ACTIVE}|epic-1`]: true },
    });
    await mount();
    await waitFor(() => expect(screen.getByTestId("epic-repo-overlay")).toBeInTheDocument());
  });

  it("another team's epic workspace reads no epic repos and draws no overlay", async () => {
    const calls: string[] = [];
    type Internals = {
      invoke: (cmd: string, args?: Record<string, unknown>, o?: unknown) => Promise<unknown>;
    };
    const internals = (window as unknown as { __TAURI_INTERNALS__: Internals }).__TAURI_INTERNALS__;
    const invoke = internals.invoke.bind(internals);
    internals.invoke = (cmd, args, o) => {
      calls.push(cmd);
      return invoke(cmd, args, o);
    };
    const epic: Workspace = {
      id: "epic-1",
      kind: "epic",
      cwd: "/epic-1",
      status: "ready",
      epicDirPath: "epic-one",
      epicDocPath: "/content/epic-one/epic.md",
    };
    seed({
      workspaces: { [ACTIVE]: [], [OTHER]: [epic] },
      activeWorkspace: { [OTHER]: "epic-1" },
      epicReposOpen: { [`${OTHER}|epic-1`]: true },
    });
    await mount();
    await act(async () => {});
    expect(screen.queryByTestId("epic-repo-overlay")).toBeNull();
    expect(calls).not.toContain("read_epic_repos");
  });

  it("a pill released on a tile's edge is placed in the team's layout, never another team's", async () => {
    seed({
      softClosedWorkspaces: { [ACTIVE]: { "a-2": { workspace: story("a-2") } } },
    });
    await mount();
    act(() =>
      dnd.onDragEnd?.({ active: { id: "tray:a-2" }, over: { id: tileZoneId("a-1", "right") } }),
    );
    const s = useViewerStore.getState();
    expect(s.workspaces[ACTIVE]?.map((w) => w.id)).toEqual(expect.arrayContaining(["a-1", "a-2"]));
    expect(s.softClosedWorkspaces[ACTIVE]?.["a-2"]).toBeUndefined();
    expect(s.workspaces[OTHER]?.map((w) => w.id)).toEqual(["o-1"]);
  });

  it("a pill released in the tray's band reorders, and splits no tile", async () => {
    const reorderWorkspace = vi.fn(() => true);
    seed({
      workspaces: { [ACTIVE]: [story("a-1"), story("a-2")], [OTHER]: [story("o-1")] },
      reorderWorkspace,
    });
    await mount();
    setTrayReorderIntent({ teamKey: ACTIVE, workspaceId: "a-2", place: { before: "a-1" } });
    act(() =>
      dnd.onDragEnd?.({ active: { id: "tray:a-2" }, over: { id: tileZoneId("a-1", "left") } }),
    );
    expect(reorderWorkspace).toHaveBeenCalledWith(ACTIVE, "a-2", { before: "a-1" }, undefined);
    expect(useViewerStore.getState().workspaces[ACTIVE]?.map((w) => w.id)).toEqual(["a-1", "a-2"]);
  });

  it.each([
    [
      "quick view",
      [TERMINAL_POPUP_ATTR, ROW_POPUP_ATTR],
      '<div class="xterm"><textarea></textarea></div>',
    ],
    ["inbox chat", [ROW_POPUP_ATTR], "<textarea></textarea>"],
  ])(
    "⌘F in the Agents pane's %s opens no find bar over the viewer",
    async (_name, attrs, inner) => {
      seed();
      await mount();
      const popup = document.createElement("div");
      for (const attr of attrs) popup.setAttribute(attr, "");
      popup.innerHTML = inner;
      document.body.appendChild(popup);
      popup.querySelector("textarea")!.focus();
      fireEvent.keyDown(document.activeElement!, { key: "f", code: "KeyF", metaKey: true });
      expect(screen.queryByPlaceholderText("Find in document...")).toBeNull();
      // Outside the popup the same key finds in the viewer.
      (document.activeElement as HTMLElement).blur();
      fireEvent.keyDown(document.body, { key: "f", code: "KeyF", metaKey: true });
      await waitFor(() =>
        expect(screen.getByPlaceholderText("Find in document...")).toBeInTheDocument(),
      );
    },
  );

  it("a chord in the CLI's terminal runs once, with both teams open", async () => {
    seed({
      workspaces: {
        [ACTIVE]: [{ ...story("a-1"), sourceRepoSlug: "acme/web" }],
        [OTHER]: [story("o-1")],
      },
    });
    const action = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.cycle-terminal-tab-next")!;
    const real = action.run;
    const run = vi.fn();
    (action as { run: () => void }).run = run;
    try {
      await mount();
      const inHost = await screen.findByTestId("cli-panels");
      expect(inHost.closest("[data-xterm-host]")).not.toBeNull();
      const key = new KeyboardEvent("keydown", {
        key: "}",
        code: "BracketRight",
        metaKey: true,
        shiftKey: true,
        bubbles: true,
        cancelable: true,
      });
      inHost.dispatchEvent(key);
      expect(run).toHaveBeenCalledTimes(1);
      expect(key.defaultPrevented).toBe(true);
    } finally {
      (action as { run: () => void }).run = real;
    }
  });
});
