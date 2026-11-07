// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render } from "@testing-library/react";

import { createPortal } from "react-dom";

import { TooltipProvider } from "@/components/ui/tooltip";
import { installMockBackend } from "@/app/mock/install";
import { MOCK_ORG_SLUG } from "@/app/mock/fixtures";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";
import { liveInstanceCount } from "@/shared/lib/fe-instance-registry";
import { SHORTCUT_ACTIONS } from "@/shared/lib/shortcuts/registry";
import { useOrgStore } from "@/shared/stores/org-store";
import { registerVirtualTab } from "@/shared/lib/virtual-viewer-registry";
import { useUiStore } from "@/shared/stores/ui-store";
import { useViewerStore, type Workspace } from "@/shared/stores/viewer-store";

// The team views stand in as plain boxes: this file is about what the workbench itself owns
// (the CLI, the split divider, the terminal section, the tray and the drag context).
vi.mock("@/features/viewer/views/team-workspace-view", () => ({
  TeamWorkspaceView: ({ teamKey }: { teamKey: string }) => <div data-team-view={teamKey} />,
}));
const cliCrash = vi.hoisted(() => ({ active: false }));
// The tiles say which team they were given.
vi.mock("@/features/viewer/components/workspace-terminal-panels", () => ({
  WorkspaceTerminalPanels: ({ teamKey, teamHidden }: { teamKey: string; teamHidden?: boolean }) => {
    if (cliCrash.active) throw new Error("CLI tile crashed");
    // A warm team's grid stays mounted, hidden: only the shown team's is "cli-panels".
    return (
      <div data-testid={teamHidden ? "cli-panels-hidden" : "cli-panels"} data-team={teamKey} />
    );
  },
  WorkspaceInitOverlay: () => null,
}));

// The workbench's DndContext as drawn: a test drives a drag through its own handlers.
const dnd = vi.hoisted(() => ({
  onDragStart: null as ((event: { active: { id: string } }) => void) | null,
  onDragEnd: null as
    | ((event: { active: { id: string }; over: { id: string } | null }) => void)
    | null,
}));
vi.mock("@dnd-kit/core", async (importOriginal) => {
  const core = await importOriginal<typeof import("@dnd-kit/core")>();
  return {
    ...core,
    DndContext: (props: React.ComponentProps<typeof core.DndContext>) => {
      if (props.collisionDetection?.name === "tileZonePriorityCollision") {
        dnd.onDragStart = props.onDragStart as typeof dnd.onDragStart;
        dnd.onDragEnd = props.onDragEnd as typeof dnd.onDragEnd;
      }
      return <core.DndContext {...props} />;
    },
  };
});

import { publishTeamTree, useTeamSurfaceStore } from "@/features/viewer/lib/team-surface-store";
import { useTerminalSectionRegistry } from "@/features/viewer/lib/terminal-section-registry";
import { tileZoneId } from "@/features/viewer/lib/terminal-drag";
import { Workbench } from "./workbench";
import {
  PanelMaximizeBoundary,
  PanelMaximizeButton,
} from "@/features/shell/right-pane/panel-maximize-boundary";
import { registerBuiltinPanels } from "@/features/shell/right-pane/builtin-panels";
import { revealSlot } from "@/features/shell/slots/reveal-slot";
import { INBOX_PANEL_ID, SCRATCH_PANEL_ID } from "@/features/shell/right-pane/right-pane-layout";

const A = `${MOCK_ORG_SLUG}:default`;
const B = `${MOCK_ORG_SLUG}:other`;
const story = (id: string, storyFilePath?: string): Workspace => ({
  id,
  kind: "story",
  cwd: `/${id}`,
  status: "ready",
  ...(storyFilePath ? { storyFilePath } : {}),
});

function seed(over: Partial<ReturnType<typeof useViewerStore.getState>> = {}) {
  useViewerStore.setState({
    openedTeams: [A, B],
    workspaces: { [A]: [story("a-1"), story("a-2")], [B]: [story("b-1"), story("b-2")] },
    activeWorkspace: { [A]: "a-1", [B]: "b-1" },
    softClosedWorkspaces: {},
    trayMode: {},
    selectedEpicDirName: {},
    trayOrderByTeam: {},
    rightPaneOpen: true,
    rightPaneRatio: 0.3,
    maximizedPanel: {},
    rightPaneLayout: {
      sections: [
        {
          id: "inbox",
          group: { panelIds: [INBOX_PANEL_ID], activePanelId: INBOX_PANEL_ID },
          collapsed: false,
          size: 1,
        },
      ],
    },
    layoutMode: "shared",
    terminalVisible: true,
    workspaceTraySeenCoachmark: true,
    ...over,
  });
}

async function mount() {
  const view = render(
    <TooltipProvider>
      <Workbench />
    </TooltipProvider>,
  );
  await act(async () => {});
  return view;
}

const switchTo = async (team: string) => {
  act(() => useOrgStore.setState({ activeTeamName: team }));
  await act(async () => {});
};
const q = (selector: string) => document.querySelector<HTMLElement>(selector);
const all = (selector: string) => document.querySelectorAll<HTMLElement>(selector);
const chips = () =>
  Array.from(all("[data-workspace-bar] [data-tray-chip]")).map((c) =>
    c.getAttribute("data-tray-chip"),
  );

// Happy DOM drops calc(var(...)) flex values; capture what React assigns instead.
// A style is a Proxy that forwards `style.flex = v` to setProperty on the unwrapped
// declaration, so a probe write finds which unwrapped declaration is the column's.
function captureRightColumnFlex() {
  const writes = vi.spyOn(CSSStyleDeclaration.prototype, "setProperty");
  return () => {
    const style = q("[data-testid='right-pane-column']")?.style;
    if (!style) return undefined;
    style.setProperty("--flex-probe", "1");
    const column = writes.mock.contexts.at(-1);
    style.removeProperty("--flex-probe");
    return writes.mock.calls
      .filter((call, i) => writes.mock.contexts[i] === column && call[0] === "flex")
      .at(-1)?.[1];
  };
}

describe("Workbench: one CLI, one divider, one tray, one drag context", () => {
  beforeEach(() => {
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
    installMockBackend();
    registerBuiltinPanels();
    useOrgStore.setState({ activeOrgSlug: MOCK_ORG_SLUG, activeTeamName: "default" });
    useTeamSurfaceStore.setState({ trees: {} });
  });

  afterEach(() => {
    cliCrash.active = false;
    cleanup();
    vi.restoreAllMocks();
    document.body.innerHTML = "";
    useUiStore.setState({ consoleSwitcherSlot: null });
    useViewerStore.setState({
      openedTeams: [],
      layoutMode: null,
      terminalVisible: false,
      trayMode: {},
      workspaces: {},
      activeWorkspace: {},
    });
    useOrgStore.setState({ activeOrgSlug: null, activeTeamName: null });
    delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  });

  it("mounts each CLI part once, outside the team views, and none of them remounts on a switch", async () => {
    seed();
    await mount();
    const parts = () => ({
      host: all("[data-xterm-host]"),
      section: all("[data-testid='terminal-section']"),
      divider: all("[data-testid='terminal-drag-handle']"),
      tray: all("[data-workspace-bar]"),
    });
    const before = parts();
    for (const list of Object.values(before)) expect(list).toHaveLength(1);
    for (const list of Object.values(before))
      expect(list[0].closest("[data-team-view]")).toBeNull();
    expect(liveInstanceCount("cli")).toBe(1);
    expect(q("[data-testid='cli-panels']")).toHaveAttribute("data-team", A);
    expect(chips()).toEqual(["a-1", "a-2"]);

    await switchTo("other");
    expect(q("[data-testid='cli-panels']")).toHaveAttribute("data-team", B);
    expect(chips()).toEqual(["b-1", "b-2"]);
    await switchTo("default");
    const after = parts();
    for (const key of Object.keys(before) as (keyof typeof before)[]) {
      expect(after[key]).toHaveLength(1);
      expect(after[key][0]).toBe(before[key][0]);
    }
    expect(liveInstanceCount("cli")).toBe(1);
  });

  it("settles an unsaved first-run layout through the Workbench only once", async () => {
    seed({ layoutMode: null, terminalVisible: false });
    await mount();
    expect(useViewerStore.getState().layoutMode).toBe("shared");
    expect(useViewerStore.getState().terminalVisible).toBe(true);
    act(() => useViewerStore.getState().setLayoutMode("console"));
    await switchTo("other");
    expect(useViewerStore.getState().layoutMode).toBe("console");
  });

  it("settles a first-run team without tiles to the viewer", async () => {
    seed({ layoutMode: null, terminalVisible: true, workspaces: {}, activeWorkspace: {} });
    await mount();
    expect(useViewerStore.getState().layoutMode).toBe("viewer");
    expect(useViewerStore.getState().terminalVisible).toBe(false);
  });

  it("an unopened active team keeps the empty-state ladder reachable and the CLI on that team", async () => {
    seed({ openedTeams: [B], layoutMode: "console", terminalVisible: true });
    await mount();
    expect(q("[data-testid='workbench-viewer-region']")!.style.display).not.toBe("none");
    expect(q("[data-testid='cli-panels']")!.getAttribute("data-team")).toBe(A);
    expect(q("[data-workspace-bar]")).not.toBeNull();
  });

  it("keeps no-team Settings tabs visible when the settings-open flag is set", async () => {
    seed({ openedTeams: [], layoutMode: "console" });
    useOrgStore.setState({ activeTeamName: null });
    registerVirtualTab("virtual://sv3-settings-probe", "Settings", () => (
      <div data-testid="settings-probe" />
    ));
    useUiStore.setState({
      settingsPanelOpen: true,
      noTeamTabs: ["virtual://sv3-settings-probe"],
      noTeamActiveTab: "virtual://sv3-settings-probe",
    });
    await mount();
    const host = q("[data-testid='no-team-tab-host']")!;
    expect(host).not.toBeNull();
    expect(q("[data-testid='settings-probe']")).not.toBeNull();
    expect(host.parentElement!.style.display).not.toBe("none");
    expect(q("[data-testid='workbench-viewer-region']")!.style.display).not.toBe("none");
  });

  it("a CLI render failure leaves the shell and team view available and retries locally", async () => {
    seed({ layoutMode: "console" });
    vi.spyOn(console, "error").mockImplementation(() => {});
    cliCrash.active = true;
    await mount();
    expect(q("[data-testid='workbench']")).not.toBeNull();
    expect(q(`[data-team-view='${A}']`)).not.toBeNull();
    expect(q("[data-crashed-team='global-cli']")).not.toBeNull();
    cliCrash.active = false;
    const retry = q("[data-crashed-team='global-cli'] button")!;
    act(() => retry.click());
    expect(q("[data-testid='cli-panels']")).not.toBeNull();
    expect(q("[data-crashed-team='global-cli']")).toBeNull();
  });

  it("registers the one terminal section, with the split divider right above it", async () => {
    seed();
    const { unmount } = await mount();
    const section = useTerminalSectionRegistry.getState().section;
    expect(section).toBe(q("[data-testid='terminal-section']"));
    expect(section!.querySelector("[data-xterm-host]")).not.toBeNull();
    const divider = section!.parentElement!.querySelector(
      ":scope > [data-testid='terminal-drag-handle']",
    );
    expect(divider).not.toBeNull();
    // Above it in the same column (its screen-reader label may sit between them).
    expect(
      divider!.compareDocumentPosition(section!) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    await switchTo("other");
    expect(useTerminalSectionRegistry.getState().section).toBe(section);
    unmount();
    expect(useTerminalSectionRegistry.getState().section).toBeUndefined();
  });

  it("in the pane layout the tray spans the column under the CLI; collapsed, it stays on screen", async () => {
    seed();
    await mount();
    const bar = q("[data-workspace-bar]")!;
    const host = q("[data-xterm-host]")!;
    // A sibling of the terminal band, the column's last child, never inside the CLI.
    expect(host.contains(bar)).toBe(false);
    expect(bar.parentElement!.lastElementChild).toBe(bar);
    expect(bar.compareDocumentPosition(host) & Node.DOCUMENT_POSITION_PRECEDING).toBeTruthy();

    act(() => useViewerStore.getState().setLayoutMode("viewer"));
    expect(q("[data-workspace-bar]")).toBe(bar);
    expect(bar.querySelector("[data-agents-toggle]")).not.toBeNull();
    expect(q("[data-testid='terminal-section']")!.style.flex).toBe("0 0 0px");
    // The divider stays as the click-to-open target.
    expect(q("[data-testid='terminal-drag-handle']")).not.toBeNull();
  });

  it("full screen hides the viewer, fills the column with the CLI and portals the tray's top into the title bar", async () => {
    seed();
    const slot = document.body.appendChild(document.createElement("div"));
    useUiStore.setState({ consoleSwitcherSlot: slot });
    await mount();
    const host = q("[data-xterm-host]");
    act(() => useViewerStore.getState().setLayoutMode("console"));
    expect(q("[data-testid='workbench-viewer-region']")!.style.display).toBe("none");
    expect(q("[data-testid='terminal-section']")!.style.flex).toMatch(/^1/);
    expect(q("[data-testid='terminal-drag-handle']")).toBeNull();
    expect(slot.querySelector("[data-tray-chip='a-1']")).not.toBeNull();
    // The same CLI, only laid out again.
    expect(q("[data-xterm-host]")).toBe(host);
    await switchTo("other");
    expect(slot.querySelector("[data-tray-chip='b-1']")).not.toBeNull();
    expect(slot.querySelector("[data-tray-chip='a-1']")).toBeNull();
  });

  it.each([
    ["shared", "maximize"],
    ["console", "maximize"],
    ["shared", "overlay"],
    ["console", "overlay"],
  ] as const)(
    "%s releases the right-pane width for Inbox %s and restores it on Esc, close and reopen",
    async (layoutMode, presentation) => {
      seed({ layoutMode });
      const columnFlex = captureRightColumnFlex();
      await mount();
      const repoPath = `workspace:${MOCK_ORG_SLUG}:default:owner/repo`;
      render(
        createPortal(
          <TooltipProvider>
            <PanelMaximizeButton panelId={INBOX_PANEL_ID} repoPath={repoPath} />
            <PanelMaximizeBoundary panelId={INBOX_PANEL_ID} repoPath={repoPath} title="Inbox">
              <div>Inbox body</div>
            </PanelMaximizeBoundary>
          </TooltipProvider>,
          q(`[data-right-pane-host="${A}"]`)!,
        ),
      );
      const column = q("[data-testid='right-pane-column']")!;
      const section = q("[data-testid='terminal-section']")!;
      const cliColumn = section.parentElement!;
      const dockedFlex = columnFlex();
      expect(dockedFlex).toContain("0.3");
      expect(column.style.display).not.toBe("none");
      expect(useViewerStore.getState().rightPaneRatio).toBe(0.3);
      expect(cliColumn).toHaveClass("flex-1", "min-w-0");
      const shell = q(`[data-testid='${INBOX_PANEL_ID}-fullscreen-shell']`)!;
      const assertClosed = () => {
        expect(useViewerStore.getState().rightPaneOpen).toBe(false);
        expect(column.style.display).toBe("none");
        expect(column.style.flex).toBe("0 0 0px");
        expect(section.parentElement).toBe(cliColumn);
        expect(cliColumn).toHaveClass("flex-1");
      };
      const assertReopened = () => {
        expect(column.style.display).not.toBe("none");
        expect(useViewerStore.getState().rightPaneRatio).toBe(0.3);
      };

      if (presentation === "overlay") {
        act(() => revealSlot("right-pane", { surface: "inbox", presentation: "overlay" }));
      } else {
        fireEvent.click(document.querySelector('[aria-label="Maximize panel"]')!);
      }
      expect(shell).toHaveAttribute("data-fullscreen", "true");
      expect(columnFlex()).toBe("0 0 0px");
      expect(shell).toBeVisible();
      fireEvent.click(q("[data-testid='scratch-note-width-divider']")!);
      assertClosed();
      fireEvent.click(q("[data-testid='scratch-note-width-divider']")!);
      assertReopened();
      expect(shell).toHaveAttribute("data-fullscreen", "true");

      fireEvent.keyDown(document, { key: "Escape" });
      expect(shell).toHaveAttribute("data-fullscreen", "false");
      expect(useViewerStore.getState().maximizedPanel[A]).toBeUndefined();
      expect(columnFlex()).toBe(dockedFlex);
      fireEvent.click(q("[data-testid='scratch-note-width-divider']")!);
      assertClosed();
      fireEvent.click(q("[data-testid='scratch-note-width-divider']")!);
      assertReopened();
      expect(columnFlex()).toBe(dockedFlex);
      expect(q("[data-testid='workbench-viewer-region']")!.style.display).toBe(
        layoutMode === "console" ? "none" : "",
      );
      expect(q("[data-testid='terminal-drag-handle']") !== null).toBe(layoutMode === "shared");
    },
  );

  it("keeps classic Notes docked when the pane full-screen command sets maximize state", async () => {
    seed({
      rightPaneLayout: {
        sections: [
          {
            id: "notes",
            group: { panelIds: [SCRATCH_PANEL_ID], activePanelId: SCRATCH_PANEL_ID },
            collapsed: false,
            size: 1,
          },
        ],
      },
    });
    const columnFlex = captureRightColumnFlex();
    await mount();
    const dockedFlex = columnFlex();
    expect(dockedFlex).toContain("0.3");
    const toggle = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.toggle-pane-full-screen")!;
    act(() => toggle.run());
    expect(useViewerStore.getState().maximizedPanel[A]).toBe(SCRATCH_PANEL_ID);
    expect(columnFlex()).toBe(dockedFlex);
    expect(q("[data-testid='right-pane-column']")!.style.display).not.toBe("none");
    fireEvent.keyDown(document, { key: "Escape" });
    expect(columnFlex()).toBe(dockedFlex);
    act(() => toggle.run());
    expect(useViewerStore.getState().maximizedPanel[A]).toBeUndefined();
    expect(columnFlex()).toBe(dockedFlex);
  });

  it("keeps a fallback tab docked and follows the shown team's maximize state", async () => {
    const absent = "pane:other:team/dashboard";
    seed({
      maximizedPanel: { [A]: INBOX_PANEL_ID },
      rightPaneLayout: {
        sections: [
          {
            id: "inbox",
            group: { panelIds: [absent, INBOX_PANEL_ID], activePanelId: absent },
            collapsed: false,
            size: 1,
          },
        ],
      },
    });
    const columnFlex = captureRightColumnFlex();
    await mount();
    expect(columnFlex()).toContain("0.3");
    act(() =>
      useViewerStore.getState().setRightPaneLayout({
        sections: [
          {
            id: "inbox",
            group: { panelIds: [INBOX_PANEL_ID], activePanelId: INBOX_PANEL_ID },
            collapsed: false,
            size: 1,
          },
        ],
      }),
    );
    expect(columnFlex()).toBe("0 0 0px");
    await switchTo("other");
    expect(columnFlex()).toContain("0.3");
    await switchTo("default");
    expect(columnFlex()).toBe("0 0 0px");
  });

  it("keeps the split when Notes still has docked content beside the maximized Inbox", async () => {
    seed();
    useViewerStore.setState((s) => ({
      rightPaneLayout: {
        sections: [
          ...s.rightPaneLayout.sections,
          {
            id: "notes",
            group: { panelIds: [SCRATCH_PANEL_ID], activePanelId: SCRATCH_PANEL_ID },
            collapsed: false,
            size: 1,
          },
        ],
      },
    }));
    const columnFlex = captureRightColumnFlex();
    await mount();
    act(() => useViewerStore.getState().setMaximized(A, INBOX_PANEL_ID));
    const column = q("[data-testid='right-pane-column']")!;
    expect(columnFlex()).toContain("0.3");
    expect(column.style.display).not.toBe("none");

    act(() =>
      useViewerStore.setState((s) => ({
        rightPaneLayout: {
          sections: s.rightPaneLayout.sections.map((section) =>
            section.id === "notes" ? { ...section, collapsed: true } : section,
          ),
        },
      })),
    );
    expect(columnFlex()).toContain("0.3");
    act(() =>
      useViewerStore.setState((s) => ({
        rightPaneLayout: { sections: s.rightPaneLayout.sections.filter((s) => s.id !== "notes") },
      })),
    );
    expect(columnFlex()).toBe("0 0 0px");
    act(() => useViewerStore.getState().setMaximized(A, null));
    expect(columnFlex()).toContain("0.3");

    act(() => useViewerStore.getState().toggleRightPane());
    expect(columnFlex()).toBe("0 0 0px");
    expect(column.style.display).toBe("none");
  });

  it("Cmd+1..9 jumps to the pill at that place in the active team's tray", async () => {
    seed();
    await mount();
    await switchTo("other");
    expect(useViewerStore.getState().trayOrderByTeam[B]).toEqual(["b-1", "b-2"]);
    const jump2 = SHORTCUT_ACTIONS.find((a) => a.id === "viewer.jump-workspace-2")!;
    act(() => jump2.run());
    expect(useViewerStore.getState().activeWorkspace[B]).toBe("b-2");
    expect(useViewerStore.getState().activeWorkspace[A]).toBe("a-1");
  });

  it("the tray shows each team in its own mode: flat for one, grouped from its published tree for the other", async () => {
    const sref = (filePath: string) => ({
      fileName: filePath.split("/").pop()!,
      filePath,
      dependsOn: null,
      status: null,
      documentType: null,
      supportingDocs: null,
      sourceRepo: null,
      team: null,
    });
    const epic = (dirName: string, paths: string[]): DerivedPipelineTree => ({
      dirPath: dirName,
      dirName,
      absoluteDirPath: "",
      stories: paths.map(sref),
      subEpics: [],
      orderError: null,
      mainDoc: null,
    });
    const tree: DerivedPipelineTree = {
      dirPath: "",
      dirName: "",
      absoluteDirPath: "",
      stories: [],
      subEpics: [epic("alpha", ["/r/alpha/B-1.md"]), epic("beta", ["/r/beta/B-2.md"])],
      orderError: null,
      mainDoc: null,
    };
    seed({
      workspaces: {
        [A]: [story("a-1"), story("a-2")],
        [B]: [story("b-1", "/r/alpha/B-1.md"), story("b-2", "/r/beta/B-2.md")],
      },
      trayMode: { [B]: "grouped" },
    });
    publishTeamTree(B, tree);
    await mount();
    expect(q("[data-testid='workspace-epic-rail']")).toBeNull();
    await switchTo("other");
    const rails = Array.from(all("[data-epic-rail-pill]")).map((el) =>
      el.getAttribute("data-epic-rail-pill"),
    );
    expect(rails).toEqual(expect.arrayContaining(["alpha", "beta"]));
    await switchTo("default");
    expect(q("[data-testid='workspace-epic-rail']")).toBeNull();
    expect(chips()).toEqual(["a-1", "a-2"]);
  });

  it("a team switch mid-drag cancels the drag, and its release drops nothing", async () => {
    seed({
      workspaces: { [A]: [story("a-1")], [B]: [story("b-1")] },
      softClosedWorkspaces: { [A]: { "a-2": { workspace: story("a-2") } } },
    });
    await mount();
    const resize = vi.fn();
    window.addEventListener("resize", resize);
    act(() => dnd.onDragStart?.({ active: { id: "tray:a-2" } }));

    await switchTo("other");
    // Cancelling the old team's sensor never broadcasts a fake window resize.
    expect(resize).not.toHaveBeenCalled();
    act(() =>
      dnd.onDragEnd?.({ active: { id: "tray:a-2" }, over: { id: tileZoneId("a-1", "right") } }),
    );
    const s = useViewerStore.getState();
    expect(s.softClosedWorkspaces[A]?.["a-2"]).toBeDefined();
    expect(s.workspaces[A]?.map((w) => w.id)).toEqual(["a-1"]);
    expect(s.workspaces[B]?.map((w) => w.id)).toEqual(["b-1"]);
    window.removeEventListener("resize", resize);
  });

  it("with no active team the CLI stays mounted and the tray and divider draw nothing", async () => {
    seed();
    await mount();
    const host = q("[data-xterm-host]");
    act(() => useOrgStore.setState({ activeTeamName: null }));
    expect(q("[data-xterm-host]")).toBe(host);
    expect(q("[data-testid='cli-panels']")).toBeNull();
    expect(q("[data-workspace-bar]")).toBeNull();
    expect(q("[data-testid='terminal-drag-handle']")).toBeNull();
    expect(liveInstanceCount("cli")).toBe(1);
  });
});
