// SPDX-License-Identifier: MIT
import { useRef } from "react";
import { NavFilterField } from "@/features/dashboard/components/status-nav/nav-filter-field";
import { DEFAULT_NAV_LENS, countLenses } from "@/shared/lib/nav-lens";
import { act, render, screen, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  SCRATCH_PANEL_ID,
  addPanelAsTopSection,
  defaultRightPaneLayout,
  findPanelLocation,
} from "@/features/shell/right-pane/right-pane-layout";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { useMcpActions } from "./use-mcp-actions";

// Capture each event's handler so a test can emit a payload the way the Rust
// backend would. `capturedHandler` is the "mcp:set-viewer-state" one.
type Handler = (event: { payload: Record<string, unknown> }) => void;
const handlers = new Map<string, Handler>();
let capturedHandler: Handler | null = null;
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((event: string, handler: Handler) => {
    handlers.set(event, handler);
    if (event === "mcp:set-viewer-state") capturedHandler = handler;
    return Promise.resolve(() => {});
  }),
}));
vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  saveScratchNote: vi.fn().mockResolvedValue(undefined),
  loadScratchNote: vi.fn().mockResolvedValue(""),
  checkPathExists: vi.fn().mockResolvedValue(true),
  detectTools: vi
    .fn()
    .mockResolvedValue({ terminals: [], editors: [], default_terminal: "", default_editor: "" }),
}));

beforeEach(() => {
  capturedHandler = null;
  handlers.clear();
  useViewerStore.setState({ rightPaneLayout: defaultRightPaneLayout() });
});

/** The right-pane History panel's id, retired when History moved into the left nav. */
const RETIRED_HISTORY_PANEL_ID = "git-sync-history";

describe("useMcpActions — the MCP layout write reconciles to the built-in panels", () => {
  it("prunes the retired History panel an MCP write puts into the rightPaneLayout", () => {
    renderHook(() => useMcpActions());
    expect(capturedHandler).toBeTypeOf("function");

    // An MCP client writes a well-formed layout (it passes coerce) that still names the
    // right-pane History panel, which no longer exists.
    const leaked = addPanelAsTopSection(
      defaultRightPaneLayout(),
      RETIRED_HISTORY_PANEL_ID,
      () => "sec-history",
    );
    capturedHandler!({ payload: { rightPaneLayout: leaked } });

    const layout = useViewerStore.getState().rightPaneLayout;
    expect(findPanelLocation(layout, RETIRED_HISTORY_PANEL_ID)).toBeNull();
    // A built-in panel (Notes) written the same way survives.
    expect(findPanelLocation(layout, SCRATCH_PANEL_ID)).not.toBeNull();
  });

  it("still accepts a valid viewer-only MCP layout unchanged", () => {
    renderHook(() => useMcpActions());
    const custom = addPanelAsTopSection(
      defaultRightPaneLayout(),
      SCRATCH_PANEL_ID,
      () => "sec-top",
    );
    capturedHandler!({ payload: { rightPaneLayout: custom } });
    expect(useViewerStore.getState().rightPaneLayout).toEqual(custom);
  });
});

describe("useMcpActions — an agent's cliChrome write is honoured as asked (the agent decides)", () => {
  it("may leave full screen or fold the CLI, merged field by field, and saves", () => {
    useViewerStore.setState({
      layoutMode: "console",
      terminalVisible: true,
      lastTerminalMode: "console",
      terminalSplitRatio: 0.4,
      cliExpanded: true,
    });
    const persist = vi.spyOn(useViewerStore.getState(), "persistState");
    renderHook(() => useMcpActions());

    capturedHandler!({ payload: { cliChrome: { layoutMode: "shared" } } });
    expect(useViewerStore.getState()).toMatchObject({
      layoutMode: "shared",
      terminalVisible: true,
      lastTerminalMode: "shared",
      // The fields the write left out stand.
      terminalSplitRatio: 0.4,
      cliExpanded: true,
    });
    expect(persist).toHaveBeenCalled();

    capturedHandler!({ payload: { cliChrome: { visible: false, expanded: false } } });
    expect(useViewerStore.getState()).toMatchObject({
      layoutMode: "viewer",
      terminalVisible: false,
      cliExpanded: false,
    });
    persist.mockRestore();
  });

  it("ignores the old per-team chrome keys and correctively persists even when every key was dropped", () => {
    const persist = vi.spyOn(useViewerStore.getState(), "persistState");
    useViewerStore.setState({ layoutMode: "shared", terminalVisible: true });
    renderHook(() => useMcpActions());
    capturedHandler!({
      payload: {
        layoutMode: { "acme:default": "viewer" },
        terminalVisible: { "acme:default": false },
      },
    });
    expect(useViewerStore.getState()).toMatchObject({
      layoutMode: "shared",
      terminalVisible: true,
    });
    expect(persist).toHaveBeenCalled();
    persist.mockRestore();
  });
});

describe("useMcpActions — a workspace close_workspace retired leaves the renderer too", () => {
  it("drops the closed story's row so the next save cannot write it back", () => {
    const team = "acme:default";
    const row = (id: string, storyId: string) =>
      ({ id, kind: "story", storyId, name: storyId }) as unknown as ReturnType<
        typeof useViewerStore.getState
      >["workspaces"][string][number];
    useViewerStore.setState({
      workspaces: { [team]: [row("sw-1-slug", "SW-1"), row("sw-2-slug", "SW-2")] },
    });
    renderHook(() => useMcpActions());
    const retired = handlers.get("mcp:workspace-retired");
    expect(retired).toBeTypeOf("function");

    retired!({ payload: { teamKey: team, form: "SW-1", workspaceId: "sw-1-slug" } });

    const ids = useViewerStore.getState().workspaces[team]?.map((w) => w.id);
    expect(ids).toEqual(["sw-2-slug"]);
  });
});

it("applies MCP nav source through the shared lens setter without replacing other teams or lens fields", () => {
  useViewerStore.setState({ navLens: {}, navCollapsed: { other: true } });
  useViewerStore.getState().setNavLens("acme/ctx:dev", { lens: "review" });
  renderHook(() => useMcpActions());
  capturedHandler!({
    payload: {
      navLens: { "acme/ctx:dev": { source: "private", view: "changes", lens: "done" } },
      navCollapsed: { "acme/ctx:dev": false },
    },
  });
  expect(useViewerStore.getState().navLens["acme/ctx:dev"]).toMatchObject({
    source: { kind: "private" },
    view: "changes",
    lens: "review",
  });
  expect(useViewerStore.getState().navCollapsed).toEqual({ other: true, "acme/ctx:dev": false });
  capturedHandler!({ payload: { navLens: { "acme/ctx:dev": { source: "clone" } } } });
  expect(useViewerStore.getState().navLens["acme/ctx:dev"].source).toEqual({ kind: "clone" });
});

it("an MCP nav source reveals Changes and its removable chip, ignoring other lens fields", () => {
  const team = "acme:backend";
  useViewerStore.setState({ navLens: {}, navCollapsed: { [team]: true } });
  function Nav() {
    useMcpActions();
    const view = useViewerStore((s) => s.navLens[team]) ?? DEFAULT_NAV_LENS;
    const inputRef = useRef<HTMLInputElement>(null);
    return (
      <NavFilterField
        query=""
        onQueryChange={() => {}}
        inputRef={inputRef}
        view={view}
        onViewChange={(patch) => useViewerStore.getState().setNavLens(team, patch)}
        counts={countLenses([])}
        changesCount={1}
        sources={[{ id: "cw_sprint", name: "Sprint 1", count: 1 }]}
      />
    );
  }
  render(<Nav />);
  act(() =>
    capturedHandler!({
      payload: {
        navLens: {
          [team]: { source: "cw_sprint", view: "changes", lens: "blocked", arrange: "stage" },
        },
        navCollapsed: { [team]: false },
      },
    }),
  );
  expect(useViewerStore.getState().navCollapsed[team]).toBe(false);
  expect(useViewerStore.getState().navLens[team]).toMatchObject({
    source: { kind: "worktree", id: "cw_sprint" },
    view: "changes",
    lens: "everything",
    arrange: "folders",
  });
  expect(screen.getByTestId("left-nav-source-chip")).toHaveTextContent("Sprint 1");
  act(() => screen.getByRole("button", { name: "Clear Sprint 1" }).click());
  expect(useViewerStore.getState().navLens[team].source).toEqual({ kind: "clone" });
  expect(screen.queryByTestId("left-nav-source-chip")).not.toBeInTheDocument();
});

describe("MCP group snapshots", () => {
  it("replaces the group list, rebuilds tiles immediately, and ignores older deliveries", () => {
    const teamKey = "groups:event";
    const workspace = { id: "a", kind: "story" as const, cwd: "/a", status: "ready" as const };
    useViewerStore.setState({
      workspaces: { [teamKey]: [workspace] },
      softClosedWorkspaces: {},
      customGroupsByTeam: {},
      railKeyByWorkspaceId: {},
      trayMode: { [teamKey]: "grouped" },
      activeWorkspace: { [teamKey]: "a" },
      selectedEpicDirName: { [teamKey]: "" },
    });
    renderHook(() => useMcpActions());
    const emit = handlers.get("mcp:workspace-groups-changed")!;
    act(() =>
      emit({
        payload: {
          teamKey,
          revision: 500,
          groups: [{ id: "g", name: "Release checks", workspaceIds: ["a"] }],
        },
      }),
    );
    expect(useViewerStore.getState().railKeyByWorkspaceId[teamKey].a).toBe(" group:g");
    expect(useViewerStore.getState().selectedEpicDirName[teamKey]).toBe(" group:g");
    act(() => emit({ payload: { teamKey, revision: 499, groups: [] } }));
    expect(useViewerStore.getState().customGroupsByTeam[teamKey][0].name).toBe("Release checks");
    act(() => emit({ payload: { teamKey, revision: 501, groups: [] } }));
    expect(useViewerStore.getState().railKeyByWorkspaceId[teamKey].a).toBe("");
  });
});

it("an MCP app-theme change preserves the person's terminal and other settings", () => {
  const before = useViewerStore.getState().settings;
  useViewerStore.setState({ settings: { ...before, terminalTheme: "nord", contentWidth: "wide" } });
  renderHook(() => useMcpActions());
  act(() => capturedHandler!({ payload: { settings: { appTheme: "catppuccin" } } }));
  expect(useViewerStore.getState().settings).toMatchObject({
    appTheme: "catppuccin",
    terminalTheme: "nord",
    contentWidth: "wide",
  });
  useViewerStore.setState({ settings: before });
});
