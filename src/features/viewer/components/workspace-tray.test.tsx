// SPDX-License-Identifier: MIT
import { openNativeMenu } from "@/features/shell/native-menu-openers";
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, screen, fireEvent, act, cleanup } from "@/test-utils";

vi.mock("@/shared/lib/team-key", async (original) => ({
  ...(await original<typeof import("@/shared/lib/team-key")>()),
  getActiveTeamKey: () => "acme:default",
}));
vi.mock("@/tauri/commands", () => ({
  workspaceGroupEdit: vi.fn(),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  listNotes: vi.fn().mockResolvedValue([]),
  readNote: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0, content: "" }),
  createNote: vi.fn().mockResolvedValue({
    id: "n2",
    title: "Untitled",
    pinned: false,
    lastOpenedAt: 0,
    content: "",
  }),
  writeNote: vi.fn().mockResolvedValue(undefined),
  updateNoteMeta: vi
    .fn()
    .mockResolvedValue({ id: "n1", title: "Scratch", pinned: false, lastOpenedAt: 0 }),
  deleteNote: vi.fn().mockResolvedValue([]),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
  checkWorktreeDirty: vi.fn().mockResolvedValue({ dirty: false }),
  getWorktreeUnpublishedContext: vi.fn().mockResolvedValue({ count: 0, files: [] }),
  removeWorktree: vi.fn().mockResolvedValue(undefined),
  worktreeLiveSessionCount: vi.fn().mockResolvedValue(0),
  dashboardProbeSessionsAlive: vi.fn().mockResolvedValue([]),
}));
vi.mock("@/shared/lib/logger", () => ({
  logger: { info: vi.fn(), error: vi.fn(), warn: vi.fn(), debug: vi.fn() },
  initLogging: vi.fn().mockResolvedValue(undefined),
}));
// Assert the chip's close ROUTING here; the Hide/End decisions themselves are
// seam-tested in `../lib/terminal-close.test.ts`.
const { mockClose, mockHide, mockEnd } = vi.hoisted(() => ({
  mockClose: vi.fn(),
  mockHide: vi.fn(),
  mockEnd: vi.fn(),
}));
vi.mock("../lib/terminal-close", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/terminal-close")>()),
  closeWorkspace: mockClose,
  hideWorkspace: mockHide,
  endWorkspace: mockEnd,
}));

import { useLaunchSheetStore } from "@/features/agents/lib/launch-sheet";
vi.mock("sonner", () => ({ toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }) }));
import * as commands from "@/tauri/commands";
import { toast } from "sonner";
import { WorkspaceTray, selectionBarStyle } from "./workspace-tray";
import { useWorktreeRosterStore } from "../stores/worktree-roster-store";
import type { WorktreeRow } from "@/tauri/commands";
import {
  selectWorkspaceTabs,
  useViewerStore,
  type SoftClosedWorkspace,
  type Workspace,
} from "@/shared/stores/viewer-store";
import { seedTerminalTabs, installTerminalLayoutHarness } from "@/test/terminal-layout-harness";
import type { DerivedPipelineTree } from "@/shared/lib/pipeline-graph/types";

const TEAM = "acme:default";
const A = "e-1-a";
const B = "e-1-b";
const M = "e-1-min";

/** Give each workspace one live terminal in the host. */
const seedTabs = (...ids: string[]) =>
  ids.forEach((id) => seedTerminalTabs(TEAM, id, [`sess-${id}`]));
const story = (id: string): Workspace => ({
  id,
  kind: "story",
  cwd: `/${id}`,
  status: "ready",
});
const shadow = (id: string): SoftClosedWorkspace => ({ workspace: story(id) });

installTerminalLayoutHarness();

const chipIds = () =>
  Array.from(document.querySelectorAll("[data-tray-chip]")).map((el) =>
    el.getAttribute("data-tray-chip"),
  );

beforeEach(() => {
  useViewerStore.setState({
    foregroundLayout: {},
    foregroundLayoutByRail: {},
    workspaceGroupVersionsByTeam: {},
    customGroupsByTeam: {},
    activeWorkspace: { [TEAM]: A },
    workspaces: { [TEAM]: [story(A), story(B)] },
    softClosedWorkspaces: { [TEAM]: { [M]: shadow(M) } },
    mruWorkspaceStack: {},
    trayOrderByTeam: {},
    workspaceTraySeenCoachmark: false,
  });
  seedTabs(A, B, M);
  vi.clearAllMocks();
  vi.mocked(commands.workspaceGroupEdit).mockReset();
});

function renderTray(onNewWorkspace = vi.fn()) {
  render(
    <WorkspaceTray
      teamKey={TEAM}
      tree={null}
      isLightTheme={false}
      onNewWorkspace={onNewWorkspace}
    />,
  );
  return onNewWorkspace;
}

describe("WorkspaceTray (SW-EW-33)", () => {
  it("renders a chip per workspace (open ∪ minimized) in tray order + the launcher", () => {
    renderTray();
    expect(screen.getByTestId("workspace-tray")).toBeInTheDocument();
    expect(chipIds()).toEqual([A, B, M]); // open (id-stable) then minimized
    expect(screen.getByTestId("workspace-tray-launcher")).toBeInTheDocument();
  });

  it("marks the minimized chip and only that one", () => {
    renderTray();
    const min = document.querySelector(`[data-tray-chip="${M}"]`);
    const open = document.querySelector(`[data-tray-chip="${A}"]`);
    expect(min?.getAttribute("data-minimized")).toBe("true");
    expect(min).toHaveClass("cs-workspace-card-minimized");
    expect(min).not.toHaveClass("opacity-60");
    const mark = min!.querySelector(".cs-mark")!;
    expect(mark).not.toBeNull();
    for (let ancestor: Element | null = mark; ancestor; ancestor = ancestor.parentElement) {
      expect(Number(getComputedStyle(ancestor).opacity || 1)).toBe(1);
    }
    expect(open?.getAttribute("data-minimized")).toBe("false");
  });

  it("chip × is the default close (Hide when running, End when idle)", () => {
    renderTray();
    const close = screen.getByTestId(`workspace-tab-${A}-close`);
    expect(close).toHaveAttribute("aria-label", expect.stringMatching(/^Hide workspace /));
    fireEvent.click(close);
    expect(mockClose).toHaveBeenCalledWith(TEAM, A);
  });

  it("chip menu offers Remove worktree… only for a workspace in a managed worktree", async () => {
    const row = {
      kind: "story",
      workspaceKey: "SW-1",
      path: `/${A}`,
      branch: "feat/a",
      dirty: false,
      repoPath: "/clones/acme/api",
      liveCliSession: false,
    } as WorktreeRow;
    // B sits in a worktree made outside CodeSense (an orphan row): never offered.
    const orphan = { ...row, kind: "orphan", path: `/${B}` } as WorktreeRow;
    useWorktreeRosterStore.setState({ rows: [row, orphan] });
    try {
      renderTray();
      fireEvent.contextMenu(
        document.querySelector(`[data-tray-chip="${B}"] button`) as HTMLElement,
      );
      expect(screen.queryByText("Remove worktree…")).not.toBeInTheDocument();
      fireEvent.contextMenu(
        document.querySelector(`[data-tray-chip="${A}"] button`) as HTMLElement,
      );
      fireEvent.click(screen.getByText("Remove worktree…"));
      const dialog = await screen.findByTestId("remove-worktree-dialog");
      expect(dialog).toHaveTextContent(`/${A}`);
      expect(dialog).toHaveTextContent("feat/a");
    } finally {
      useWorktreeRosterStore.setState({ rows: null });
    }
  });

  it("chip menu offers Hide + End on a live chip, Restore + End on a hidden one", () => {
    renderTray();
    fireEvent.contextMenu(document.querySelector(`[data-tray-chip="${A}"] button`) as HTMLElement);
    expect(screen.queryByText(/Minimize|Close|kill/i)).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("Hide"));
    expect(mockHide).toHaveBeenCalledWith(TEAM, A);
    fireEvent.contextMenu(document.querySelector(`[data-tray-chip="${A}"] button`) as HTMLElement);
    fireEvent.click(screen.getByText("End"));
    expect(mockEnd).toHaveBeenCalledWith(TEAM, A);
    // A hidden (minimized) chip: Restore brings it back; End still asks via endWorkspace.
    fireEvent.contextMenu(document.querySelector(`[data-tray-chip="${M}"] button`) as HTMLElement);
    expect(screen.queryByText("Hide")).not.toBeInTheDocument();
    fireEvent.click(screen.getByText("End"));
    expect(mockEnd).toHaveBeenCalledWith(TEAM, M);
    fireEvent.contextMenu(document.querySelector(`[data-tray-chip="${M}"] button`) as HTMLElement);
    fireEvent.click(screen.getByText("Restore"));
    expect(useViewerStore.getState().softClosedWorkspaces[TEAM]?.[M]).toBeUndefined();
    expect(selectWorkspaceTabs(useViewerStore.getState(), TEAM, M).map((t) => t.id)).toEqual([
      `sess-${M}`,
    ]);
  });

  it("never vanishes — zero workspaces still shows the launcher", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [] },
      softClosedWorkspaces: { [TEAM]: {} },
      activeWorkspace: {},
    });
    renderTray();
    expect(chipIds()).toEqual([]);
    expect(screen.getByTestId("workspace-tray-launcher")).toBeInTheDocument();
  });

  it("the launcher opens the new-workspace picker", () => {
    const onNew = renderTray();
    fireEvent.click(screen.getByTestId("workspace-tray-launcher"));
    expect(onNew).toHaveBeenCalledTimes(1);
  });

  it("clicking a minimized chip replaces the focused pane (activate state-rule)", () => {
    renderTray();
    // The pill's primary button carries the workspace label; click the chip's button.
    const chip = document.querySelector(`[data-tray-chip="${M}"]`)!;
    const button = chip.querySelector("button")!;
    fireEvent.click(button);
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(M); // replaced → M active
  });

  it("shows the first-run coachmark and dismisses it (does not reappear)", () => {
    renderTray();
    expect(screen.getByTestId("workspace-tray-coachmark")).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText("Dismiss"));
    expect(useViewerStore.getState().workspaceTraySeenCoachmark).toBe(true);
    expect(screen.queryByTestId("workspace-tray-coachmark")).not.toBeInTheDocument();
  });

  it("interacting with a chip also dismisses the coachmark", () => {
    renderTray();
    expect(screen.getByTestId("workspace-tray-coachmark")).toBeInTheDocument();
    const button = document.querySelector(`[data-tray-chip="${B}"] button`) as HTMLElement;
    fireEvent.click(button);
    expect(useViewerStore.getState().workspaceTraySeenCoachmark).toBe(true);
  });

  it("renders the flat/grouped mode toggle in flat mode too (reachable to switch)", () => {
    renderTray();
    expect(screen.getByTestId("workspace-tray-mode-toggle")).toBeInTheDocument();
    // flat mode → no epic rail
    expect(screen.queryByTestId("workspace-epic-rail")).not.toBeInTheDocument();
  });

  it("the toggle flips trayMode flat → grouped (AC-2)", () => {
    renderTray();
    fireEvent.click(screen.getByTestId("workspace-tray-mode-toggle"));
    expect(useViewerStore.getState().trayMode[TEAM]).toBe("grouped");
  });

  // SW-CW-2: the 2px left bar's "one meaning at a time" precedence. The epic hue is the epic
  // rail pill's only (a workspace pill wears no epic hue; see the tray test below).
  describe("selectionBarStyle precedence", () => {
    it("selected → neutral bar (wins over the epic hue)", () => {
      expect(selectionBarStyle(true, "hsl(175, 22%, 55%)")).toEqual({
        borderLeft: "2px solid var(--cs-sidebar-text, var(--foreground))",
      });
    });
    it("selected root (no epic) → still the neutral bar", () => {
      expect(selectionBarStyle(true, undefined)).toEqual({
        borderLeft: "2px solid var(--cs-sidebar-text, var(--foreground))",
      });
    });
    it("unselected + epic (the epic rail pill) → the desaturated epic spine", () => {
      expect(selectionBarStyle(false, "hsl(175, 22%, 55%)")).toEqual({
        borderLeft: "2px solid hsl(175, 22%, 55%)",
      });
    });
    it("unselected root (no epic) → no bar", () => {
      expect(selectionBarStyle(false, undefined)).toBeUndefined();
    });
  });

  it("SW-CW-2 AC-2/AC-3: the selected chip is structural, not an accent ring", () => {
    renderTray(); // A is the active workspace
    const active = document.querySelector(`[data-tray-chip="${A}"]`)!;
    const inactive = document.querySelector(`[data-tray-chip="${B}"]`)!;
    expect(active.getAttribute("data-active")).toBe("true");
    // Selection reads via structure/weight/fill — no color-only accent ring.
    expect(active.className).toContain("cs-workspace-card-selected");
    expect(active.className).not.toMatch(/\bring-/);
    // The one selected item vs. the rest (AC-4): inactive carries neither.
    expect(inactive.className).not.toContain("cs-workspace-card-selected");
    expect(inactive.className).not.toMatch(/\bring-/);
  });
});

// ───────────────────────── SW-EW-38: grouped tray mode ─────────────────────────

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
function epicNode(dirName: string, storyPaths: string[]): DerivedPipelineTree {
  return {
    dirPath: dirName,
    dirName,
    absoluteDirPath: "",
    stories: storyPaths.map(sref),
    subEpics: [],
    orderError: null,
    mainDoc: null,
  };
}
const groupedTree: DerivedPipelineTree = {
  dirPath: "",
  dirName: "",
  absoluteDirPath: "",
  stories: [],
  // alpha is tree pos 0, beta tree pos 1.
  subEpics: [
    epicNode("alpha", ["/r/alpha/A-1.md", "/r/alpha/A-2.md"]),
    epicNode("beta", ["/r/beta/B-1.md"]),
  ],
  orderError: null,
  mainDoc: null,
};
const gstory = (id: string, filePath: string): Workspace => ({
  id,
  kind: "story",
  cwd: `/${id}`,
  status: "ready",
  storyFilePath: filePath,
});

const A1 = "alpha-A-1";
const A2 = "alpha-A-2";
const B1 = "beta-B-1";

const railKeys = () =>
  Array.from(document.querySelectorAll("[data-epic-rail-pill]")).map((el) =>
    el.getAttribute("data-epic-rail-pill"),
  );

function renderGroupedTray(active = A1) {
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: active },
    workspaces: {
      [TEAM]: [
        gstory(A1, "/r/alpha/A-1.md"),
        gstory(A2, "/r/alpha/A-2.md"),
        gstory(B1, "/r/beta/B-1.md"),
      ],
    },
    softClosedWorkspaces: { [TEAM]: {} },
    mruWorkspaceStack: {},
    trayOrderByTeam: {},
    trayMode: { [TEAM]: "grouped" },
    selectedEpicDirName: {},
    workspaceTraySeenCoachmark: true,
  });
  seedTabs(A1, A2, B1);
  render(
    <WorkspaceTray
      teamKey={TEAM}
      tree={groupedTree}
      isLightTheme={false}
      onNewWorkspace={vi.fn()}
    />,
  );
}

describe("WorkspaceTray — a workspace pill wears no epic hue", () => {
  it("an unselected epic member's chip has no left bar, and its repo glyph no inline colour", () => {
    renderGroupedTray(A1);
    useViewerStore.setState((st) => ({
      workspaces: {
        [TEAM]: st.workspaces[TEAM]!.map((w) =>
          w.id === A2 ? { ...w, sourceRepoSlug: "acme/widgets" } : w,
        ),
      },
    }));
    cleanup();
    render(
      <WorkspaceTray
        teamKey={TEAM}
        tree={groupedTree}
        glyphMap={new Map([["acme/widgets", "WI"]])}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
      />,
    );
    const chip = document.querySelector<HTMLElement>(`[data-tray-chip="${A2}"]`)!;
    expect(chip.getAttribute("data-active")).toBe("false");
    expect(chip.style.borderLeft).toBe("");
    const glyph = screen.getByTestId(`workspace-tab-${A2}-repo-glyph`);
    expect(glyph.style.color).toBe("");
  });
});

describe("WorkspaceTray — picking an epic while the CLI is closed", () => {
  it("an epic that resumes a workspace reopens the panel to it", () => {
    useViewerStore.setState({
      layoutMode: "viewer",
      terminalVisible: false,
      lastTerminalMode: "shared",
    });
    renderGroupedTray(A1);
    fireEvent.click(document.querySelector('[data-epic-rail-pill="beta"]')!);
    const s = useViewerStore.getState();
    expect(s.activeWorkspace[TEAM]).toBe(B1);
    expect(s.layoutMode).toBe("shared");
    expect(s.terminalVisible).toBe(true);
  });
});

describe("WorkspaceTray — grouped mode (SW-EW-38)", () => {
  it("AC-3: renders two rows — children strip over the epic rail", () => {
    renderGroupedTray();
    expect(screen.getByTestId("workspace-epic-rail")).toBeInTheDocument();
    expect(screen.getByTestId("workspace-tray-children")).toBeInTheDocument();
    expect(screen.getByTestId("workspace-tray-launcher")).toBeInTheDocument();
  });

  // SLW-FX-19: the two grouped strips are swapped — story chips ride the TOP
  // strip, the epic-selector rail sits on the BOTTOM (nearest the CLI chrome).
  it("SLW-FX-19/AC-1: story row is on TOP, epic rail on BOTTOM (shared placement)", () => {
    renderGroupedTray();
    const children = screen.getByTestId("workspace-tray-children");
    const rail = screen.getByTestId("workspace-epic-rail");
    // In the flex-col wrapper, DOM order == visual top→bottom. The children
    // strip must precede the rail strip ⇒ children on top, rail on bottom.
    expect(children.compareDocumentPosition(rail) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("SLW-FX-19/AC-2: border weights follow the swap — top=full edge, bottom=soft divider", () => {
    renderGroupedTray();
    const children = screen.getByTestId("workspace-tray-children");
    const rail = screen.getByTestId("workspace-epic-rail");
    // Upper strip (now children) owns the full-strength outer top edge, unsoftened.
    expect(children.classList.contains("border-t")).toBe(true);
    expect(children.classList.contains("border-border")).toBe(true);
    expect(children.classList.contains("border-border/60")).toBe(false);
    // Lower strip (now rail, shared placement) carries the softened inter-strip divider.
    expect(rail.classList.contains("border-border/60")).toBe(true);
  });

  it("AC-4: one pill per epic in pipeline-tree order, each with a child count", () => {
    renderGroupedTray();
    expect(railKeys()).toEqual(["alpha", "beta"]);
    const alpha = document.querySelector('[data-epic-rail-pill="alpha"]')!;
    expect(alpha.getAttribute("aria-label")).toContain("2 workspaces");
  });

  it("AC-5: follow-focus selects the focused workspace's epic on mount", () => {
    renderGroupedTray(B1); // focused on a beta story
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe("beta");
    // row 2 shows beta's child only
    expect(chipIds()).toEqual([B1]);
  });

  it("AC-5/R5: entering grouped while focused on beta selects beta, not the first pill", () => {
    // Start flat, focused on beta, THEN flip to grouped (mode change, not focus change).
    useViewerStore.setState({
      foregroundLayout: {},
      activeWorkspace: { [TEAM]: B1 },
      workspaces: {
        [TEAM]: [
          gstory(A1, "/r/alpha/A-1.md"),
          gstory(A2, "/r/alpha/A-2.md"),
          gstory(B1, "/r/beta/B-1.md"),
        ],
      },
      softClosedWorkspaces: { [TEAM]: {} },
      mruWorkspaceStack: {},
      trayOrderByTeam: {},
      trayMode: { [TEAM]: "flat" },
      selectedEpicDirName: {},
      workspaceTraySeenCoachmark: true,
    });
    render(
      <WorkspaceTray
        teamKey={TEAM}
        tree={groupedTree}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByTestId("workspace-tray-mode-toggle")); // flat → grouped
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe("beta");
  });

  it("AC-6: clicking an epic pill selects it + MRU-resumes the recent member", () => {
    renderGroupedTray(A1);
    // MRU stack favors A-2 within alpha.
    useViewerStore.setState({ mruWorkspaceStack: { [TEAM]: [A2, A1] } });
    fireEvent.click(document.querySelector('[data-epic-rail-pill="beta"]')!);
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe("beta");
    // beta's only member becomes active (state-rule).
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(B1);
  });

  it("AC-6: MRU-resume picks the most-recent member of the clicked epic", () => {
    renderGroupedTray(B1); // focused beta; select beta
    useViewerStore.setState({ mruWorkspaceStack: { [TEAM]: [A2, A1] } }); // A-2 most recent in alpha
    fireEvent.click(document.querySelector('[data-epic-rail-pill="alpha"]')!);
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(A2);
  });

  it("AC-7: row-2 chips are the selected epic's, and the Cmd order == row-2 ids", () => {
    renderGroupedTray(A1); // alpha selected
    expect(chipIds()).toEqual([A1, A2]);
    expect(useViewerStore.getState().trayOrderByTeam[TEAM]).toEqual([A1, A2]);
  });

  it("SW-CW-4: publishes the FULL strip order (all rails) to trayFullOrderByTeam", () => {
    renderGroupedTray(A1); // alpha selected — row-2 is only [A1, A2]
    // trayOrderByTeam is the visible row-2 slice, but the full order spans both rails
    // so closeWorkspace can find the adjacent tab across the epic boundary.
    expect(useViewerStore.getState().trayOrderByTeam[TEAM]).toEqual([A1, A2]);
    expect(useViewerStore.getState().trayFullOrderByTeam[TEAM]).toEqual([A1, A2, B1]);
  });

  it("AC-8: a stale selection with no focused chip reconciles to the first pill (no store/UI drift)", () => {
    // selectedRaw points at an epic with no members AND activeWorkspace is gone
    // (hard-closed): the render falls back to the first pill, and the effect
    // writes that resolved key back so the store can't linger on the stale epic.
    useViewerStore.setState({
      foregroundLayout: {},
      activeWorkspace: { [TEAM]: "ghost-not-in-entries" },
      workspaces: {
        [TEAM]: [gstory(A1, "/r/alpha/A-1.md"), gstory(B1, "/r/beta/B-1.md")],
      },
      softClosedWorkspaces: { [TEAM]: {} },
      mruWorkspaceStack: {},
      trayOrderByTeam: {},
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: "gamma-gone" },
      workspaceTraySeenCoachmark: true,
    });
    render(
      <WorkspaceTray
        teamKey={TEAM}
        tree={groupedTree}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
      />,
    );
    // alpha is the first pill (tree pos 0) → reconciled selection + row-2 chips.
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe("alpha");
    expect(chipIds()).toEqual([A1]);
  });

  it("closes an open chip context menu when the tray mode flips (no orphaned menu)", () => {
    renderGroupedTray(A1);
    const chipBtn = document.querySelector(`[data-tray-chip="${A1}"] button`) as HTMLElement;
    fireEvent.contextMenu(chipBtn);
    expect(screen.getByText("Hide")).toBeInTheDocument();
    // Flip grouped → flat: the chip the menu came from can unmount; menu must close.
    fireEvent.click(screen.getByTestId("workspace-tray-mode-toggle"));
    expect(screen.queryByText("Hide")).not.toBeInTheDocument();
  });

  it("AC-8/R6: an empty grouped tray still shows the launcher + the mode toggle", () => {
    useViewerStore.setState({
      workspaces: { [TEAM]: [] },
      softClosedWorkspaces: { [TEAM]: {} },
      activeWorkspace: {},
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: {},
    });
    render(
      <WorkspaceTray
        teamKey={TEAM}
        tree={groupedTree}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
      />,
    );
    expect(screen.getByTestId("workspace-tray-launcher")).toBeInTheDocument();
    expect(screen.getByTestId("workspace-tray-mode-toggle")).toBeInTheDocument();
    expect(railKeys()).toEqual([]);
  });

  // OC-6 — keyboard epic-group navigation: rail roving focus + a11y (AC3/AC4).
  it("OC-6 AC3: single-select semantics — aria-current on the selected pill, no aria-pressed", () => {
    renderGroupedTray(A1); // alpha selected
    const alpha = document.querySelector('[data-epic-rail-pill="alpha"]') as HTMLButtonElement;
    const beta = document.querySelector('[data-epic-rail-pill="beta"]') as HTMLButtonElement;
    expect(alpha.getAttribute("aria-current")).toBe("true");
    expect(beta.getAttribute("aria-current")).toBeNull();
    // aria-pressed (a toggle-button semantic) is gone.
    expect(alpha.getAttribute("aria-pressed")).toBeNull();
  });

  it("OC-6 AC3: roving tab stop follows the selected pill (0), the rest are -1", () => {
    renderGroupedTray(A1); // alpha selected → alpha is the tab stop
    const alpha = document.querySelector('[data-epic-rail-pill="alpha"]') as HTMLButtonElement;
    const beta = document.querySelector('[data-epic-rail-pill="beta"]') as HTMLButtonElement;
    expect(alpha.tabIndex).toBe(0);
    expect(beta.tabIndex).toBe(-1);
  });

  it("OC-6 AC3/AC7: ArrowRight moves rail focus WITHOUT activating (no terminal thrash)", () => {
    renderGroupedTray(A1); // alpha selected + focused workspace A1
    const alpha = document.querySelector('[data-epic-rail-pill="alpha"]') as HTMLButtonElement;
    const beta = document.querySelector('[data-epic-rail-pill="beta"]') as HTMLButtonElement;
    alpha.focus();
    fireEvent.keyDown(alpha, { key: "ArrowRight" });
    // Focus moved to the next pill…
    expect(document.activeElement).toBe(beta);
    // …but nothing was activated: the focused workspace is unchanged (arrows are
    // roving-only; Enter/Space on the pill's native button is what activates).
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(A1);
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe("alpha");
  });

  it("OC-6 AC3: Home/End jump the rail tab stop to the first/last pill", () => {
    renderGroupedTray(B1); // beta selected
    const alpha = document.querySelector('[data-epic-rail-pill="alpha"]') as HTMLButtonElement;
    const beta = document.querySelector('[data-epic-rail-pill="beta"]') as HTMLButtonElement;
    beta.focus();
    fireEvent.keyDown(beta, { key: "Home" });
    expect(document.activeElement).toBe(alpha);
    fireEvent.keyDown(alpha, { key: "End" });
    expect(document.activeElement).toBe(beta);
  });

  it("OC-6 AC4: a polite live region announces the selected epic", () => {
    renderGroupedTray(A1);
    const live = screen.getByTestId("epic-rail-live");
    expect(live.getAttribute("aria-live")).toBe("polite");
    expect(live.textContent?.toLowerCase()).toContain("selected");
    expect(live.textContent?.length ?? 0).toBeGreaterThan(0);
  });

  it("OC-6 AC3: rail arrows CLAMP at the last pill (no wrap — that's the command's job)", () => {
    renderGroupedTray(B1); // beta selected — it's the last pill
    const beta = document.querySelector('[data-epic-rail-pill="beta"]') as HTMLButtonElement;
    beta.focus();
    fireEvent.keyDown(beta, { key: "ArrowRight" });
    // Stays on beta (clamp), does not wrap to alpha, does not activate.
    expect(document.activeElement).toBe(beta);
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(B1);
  });

  it("OC-6 AC3: the rail keydown self-gate leaves the mode toggle's native behavior intact", () => {
    renderGroupedTray(A1);
    const toggle = screen.getByTestId("workspace-tray-mode-toggle");
    const alpha = document.querySelector('[data-epic-rail-pill="alpha"]') as HTMLButtonElement;
    toggle.focus();
    const ev = new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, cancelable: true });
    toggle.dispatchEvent(ev);
    // The handler self-gates (target isn't a pill): no preventDefault, no rove.
    expect(ev.defaultPrevented).toBe(false);
    expect(document.activeElement).toBe(toggle);
    expect(document.activeElement).not.toBe(alpha);
  });
});

// ───────────── SW-EW-38 × SW-EW-34: full-screen CLI (console) split ─────────────

// Module-scope so BOTH the console describe below AND the one-row describe
// can render the console (title-bar) placement — keep it here, not nested.
function setupConsole(mode: "flat" | "grouped", active = A1) {
  useViewerStore.setState({
    foregroundLayout: {},
    activeWorkspace: { [TEAM]: active },
    workspaces: {
      [TEAM]: [
        gstory(A1, "/r/alpha/A-1.md"),
        gstory(A2, "/r/alpha/A-2.md"),
        gstory(B1, "/r/beta/B-1.md"),
      ],
    },
    softClosedWorkspaces: { [TEAM]: {} },
    mruWorkspaceStack: {},
    trayOrderByTeam: {},
    trayMode: { [TEAM]: mode },
    selectedEpicDirName: {},
    workspaceTraySeenCoachmark: true,
  });
  seedTabs(A1, A2, B1);
  const slot = document.createElement("div");
  slot.setAttribute("data-test-slot", "title-bar");
  document.body.appendChild(slot);
  render(
    <WorkspaceTray
      teamKey={TEAM}
      tree={groupedTree}
      isLightTheme={false}
      onNewWorkspace={vi.fn()}
      topChrome
      trailing={undefined}
      consoleRailSlot={slot}
    />,
  );
  return slot;
}

describe("WorkspaceTray — console (full-screen CLI) placement", () => {
  it("grouped: portals the EPIC RAIL into the title-bar slot, story row stays at the bottom", () => {
    const slot = setupConsole("grouped", B1); // focused beta → beta selected
    // Epic rail (the selector) lives in the title-bar slot.
    expect(slot.querySelectorAll("[data-epic-rail-pill]").length).toBe(2);
    expect(slot.querySelector('[data-epic-rail-pill="alpha"]')).toBeTruthy();
    // The selected epic's story row renders at the bottom (NOT in the slot).
    expect(screen.getByTestId("workspace-tray-children")).toBeInTheDocument();
    expect(slot.querySelector('[data-testid="workspace-tray-children"]')).toBeNull();
    expect(chipIds()).toEqual([B1]); // beta's story
  });

  it("grouped: the mode toggle is reachable in the title-bar slot", () => {
    const slot = setupConsole("grouped");
    expect(slot.querySelector('[data-testid="workspace-tray-mode-toggle"]')).toBeTruthy();
  });

  it("flat: portals the whole switcher into the slot, NO bottom story row", () => {
    const slot = setupConsole("flat");
    expect(slot.querySelector('[data-testid="workspace-tray"]')).toBeTruthy();
    expect(slot.querySelectorAll("[data-tray-chip]").length).toBe(3); // all workspaces, flat
    expect(screen.queryByTestId("workspace-tray-children")).not.toBeInTheDocument();
    expect(screen.queryByTestId("workspace-epic-rail")).not.toBeInTheDocument();
  });

  it("clicking an epic pill in the title bar swaps the bottom story row", () => {
    const slot = setupConsole("grouped", A1); // alpha selected
    expect(chipIds()).toEqual([A1, A2]);
    fireEvent.click(slot.querySelector('[data-epic-rail-pill="beta"]')!);
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe("beta");
    expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(B1);
    // The rendered bottom row actually swaps to beta's story (not just store state).
    expect(chipIds()).toEqual([B1]);
  });

  // SW-FX-94: the store↔tray seam. closeWorkspace preserves console (it never leaves
  // full screen) but does NOT itself move `selectedEpicDirName` off the emptied
  // epic — the SW-EW-38 follow-focus effect in this component does, reacting to
  // the new `activeWorkspace`. This test drives the REAL close through the mounted
  // console tray so both halves are exercised together: console stays full-screen
  // AND the selected rail follows to the surviving epic (so the preserved console
  // renders a rail that actually has a tile, not an empty one).
  it("SW-FX-94: closing the last tile of the selected epic keeps console AND follows the rail to the surviving epic", () => {
    useViewerStore.setState({
      foregroundLayout: {},
      activeWorkspace: { [TEAM]: A1 },
      // alpha holds exactly one tile (A1); beta holds B1 — so closing A1 empties
      // the SELECTED epic while the team still has a foreground tile.
      workspaces: { [TEAM]: [gstory(A1, "/r/alpha/A-1.md"), gstory(B1, "/r/beta/B-1.md")] },
      softClosedWorkspaces: { [TEAM]: {} },
      mruWorkspaceStack: {},
      trayOrderByTeam: {},
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: "alpha" },
      layoutMode: "console",
      lastTerminalMode: "console",
      workspaceTraySeenCoachmark: true,
    });
    seedTabs(A1, B1);
    const slot = document.createElement("div");
    slot.setAttribute("data-test-slot", "title-bar");
    document.body.appendChild(slot);
    render(
      <WorkspaceTray
        teamKey={TEAM}
        tree={groupedTree}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
        topChrome
        consoleRailSlot={slot}
      />,
    );
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe("alpha");

    // Close the last tile of the selected epic; act() flushes the follow-focus effect.
    act(() => {
      useViewerStore.getState().closeWorkspace(TEAM, A1);
    });

    const s = useViewerStore.getState();
    // Store (SW-FX-94): console preserved.
    expect(s.layoutMode).toBe("console");
    expect(s.lastTerminalMode).toBe("console");
    expect(s.activeWorkspace[TEAM]).toBe(B1);
    // Tray follow-focus effect (SW-EW-38): the selected rail moves off the emptied
    // alpha to beta, so the preserved console shows a rail that actually has a tile.
    expect(s.selectedEpicDirName[TEAM]).toBe("beta");
    expect(chipIds()).toEqual([B1]);
  });

  it("renders nothing when the slot is unregistered (backgrounded team)", () => {
    useViewerStore.setState({
      activeWorkspace: { [TEAM]: A1 },
      workspaces: { [TEAM]: [gstory(A1, "/r/alpha/A-1.md")] },
      softClosedWorkspaces: { [TEAM]: {} },
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: {},
      workspaceTraySeenCoachmark: true,
    });
    const { container } = render(
      <WorkspaceTray
        teamKey={TEAM}
        tree={groupedTree}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
        topChrome
        consoleRailSlot={null}
      />,
    );
    expect(container).toBeEmptyDOMElement();
    expect(screen.queryByTestId("workspace-tray-children")).not.toBeInTheDocument();
  });
});

// The bottom bar is one row at any width (compact pills + "+N" handle crowding —
// see workspace-tray.overflow.test). The title-bar switcher uses the same row.
// jsdom computes no layout, so this is asserted via the strip containers' classes.
describe("WorkspaceTray — shared one-row bar in both placements", () => {
  // The inner chip area is the direct parent of a chip / rail pill.
  const innerForChip = (id: string) =>
    document.querySelector(`[data-tray-chip="${id}"]`)!.parentElement!;
  const railInner = () => document.querySelector("[data-epic-rail-pill]")!.parentElement!;

  it("flat shared: the bottom bar stays one row (32px floor, grows with its chips); the chip area scrolls instead of wrapping", () => {
    useViewerStore.setState({ trayMode: { [TEAM]: "flat" } });
    renderTray();
    const strip = screen.getByTestId("workspace-tray");
    expect(strip.className).toContain("min-h-8");
    expect(strip.className).not.toMatch(/(^|\s)h-8(\s|$)/);
    const inner = innerForChip(A);
    expect(inner.className).toContain("overflow-x-auto");
    expect(inner.className).not.toContain("flex-wrap");
    expect(inner.className).not.toContain("max-h-[6.5rem]");
  });

  it("flat shared: the right-pinned mode toggle stays on row 1 (no h-full, self-start)", () => {
    useViewerStore.setState({ trayMode: { [TEAM]: "flat" } });
    renderTray();
    // The pin lives on the toggle's row-1 slot: a fixed 32px row, pinned to the top.
    const slot = screen.getByTestId("workspace-tray-mode-toggle").parentElement!;
    expect(slot.className).not.toMatch(/(^|\s)h-full(\s|$)/);
    expect(slot.className).toContain("self-start");
    expect(slot.className).toContain("h-8");
  });

  it("flat shared: the trailing CLI controls are pinned to row 1 at fixed height", () => {
    useViewerStore.setState({ trayMode: { [TEAM]: "flat" } });
    render(
      <WorkspaceTray
        teamKey={TEAM}
        tree={null}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
        trailing={<div data-testid="cli-trailing" />}
      />,
    );
    const wrapper = screen.getByTestId("cli-trailing").parentElement!;
    expect(wrapper.className).toContain("h-8");
    expect(wrapper.className).toContain("self-start");
  });

  it("flat shared: the leading control sits at the strip's left end, before the pills", () => {
    useViewerStore.setState({ trayMode: { [TEAM]: "flat" } });
    render(
      <WorkspaceTray
        teamKey={TEAM}
        tree={null}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
        leading={<button type="button" data-testid="agents-control" />}
      />,
    );
    const strip = screen.getByTestId("workspace-tray");
    const slot = screen.getByTestId("agents-control").parentElement!;
    expect(strip.firstElementChild).toBe(slot);
    expect(slot.className).toContain("self-start");
  });

  it("grouped shared: the story-chip row stays one row too (scrolls, never wraps)", () => {
    renderGroupedTray(A1);
    const children = screen.getByTestId("workspace-tray-children");
    expect(children.className).toContain("min-h-8");
    expect(children.className).not.toMatch(/(^|\s)h-8(\s|$)/);
    const inner = innerForChip(A1);
    expect(inner.className).toContain("overflow-x-auto");
    expect(inner.className).not.toContain("flex-wrap");
  });

  it("grouped shared: the epic rail never wraps (single-row scroll)", () => {
    renderGroupedTray(A1);
    const rail = screen.getByTestId("workspace-epic-rail");
    expect(rail.className).toContain("items-stretch");
    // A fixed 32px selector row (it never wraps; its controls fit via max-h-full).
    expect(rail.className).toMatch(/(^|\s)h-8(\s|$)/);
    expect(rail.className).not.toContain("min-h-8");
    expect(railInner().className).toContain("overflow-x-auto");
    expect(railInner().className).not.toContain("flex-wrap");
  });

  it("console flat: the portaled switcher uses the same single-row scrolling strip", () => {
    const slot = setupConsole("flat");
    const strip = slot.querySelector('[data-testid="workspace-tray"]') as HTMLElement;
    expect(strip.className).toContain("min-h-8");
    expect(strip.className).toContain("items-stretch");
    const inner = slot.querySelector("[data-tray-chip]")!.parentElement!;
    expect(inner.className).not.toContain("flex-wrap");
    expect(inner.className).not.toContain("max-h-[6.5rem]");
    expect(inner.className).toContain("overflow-x-auto");
  });

  it("console grouped: the inline (bottom) story row is one row like the shared bar", () => {
    setupConsole("grouped", A1);
    const children = screen.getByTestId("workspace-tray-children");
    expect(children.className).toContain("h-8");
    const inner = innerForChip(A1);
    expect(inner.className).toContain("overflow-x-auto");
    expect(inner.className).not.toContain("flex-wrap");
  });

  it("the leading slot (and its divider) disappears when its control renders nothing", () => {
    useViewerStore.setState({ trayMode: { [TEAM]: "flat" } });
    const Nothing = () => null;
    render(
      <WorkspaceTray
        teamKey={TEAM}
        tree={null}
        isLightTheme={false}
        onNewWorkspace={vi.fn()}
        leading={<Nothing />}
      />,
    );
    const slot = screen.getByTestId("workspace-tray").firstElementChild as HTMLElement;
    expect(slot).toBeEmptyDOMElement();
    expect(slot.className).toContain("empty:hidden");
  });
});

describe("a pill click in the tray", () => {
  it("requests a reveal without changing the agents pane's hidden teams after the double-click window", () => {
    vi.useFakeTimers();
    try {
      useViewerStore.getState().setAgentsPaneTeamHidden(TEAM, true);
      renderTray();
      const button = document.querySelector(`[data-tray-chip="${B}"] button`) as HTMLElement;
      fireEvent.click(button);
      act(() => {
        vi.advanceTimersByTime(1000);
      });
      const pane = useViewerStore.getState();
      expect(pane.agentsPaneHiddenTeams).toContain(TEAM);
      expect(pane.agentsWorkspaceReveal).toMatchObject({ teamKey: TEAM, workspaceId: B });
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("the shared tray across team switches", () => {
  const OTHER = "acme:other";
  const draw = (teamKey: string, tree: DerivedPipelineTree | null = null) => (
    <WorkspaceTray
      teamKey={teamKey}
      tree={tree}
      isLightTheme={false}
      onNewWorkspace={vi.fn()}
      onStoryStatusChange={vi.fn()}
    />
  );
  function seedOther() {
    const s = useViewerStore.getState();
    useViewerStore.setState({
      workspaces: { ...s.workspaces, [OTHER]: [story(A), story(B)] },
      activeWorkspace: { ...s.activeWorkspace, [OTHER]: A },
      softClosedWorkspaces: { ...s.softClosedWorkspaces, [OTHER]: {} },
      layoutMode: "shared",
      terminalVisible: true,
    });
    seedTerminalTabs(OTHER, A, ["other-a"]);
    seedTerminalTabs(OTHER, B, ["other-b"]);
  }
  it("dismisses an old team's workspace action menu even when the new team has the same ID", () => {
    seedOther();
    const { rerender } = render(draw(TEAM));
    fireEvent.contextMenu(document.querySelector(`[data-tray-chip="${A}"] button`)!);
    expect(screen.getByText("Hide")).toBeInTheDocument();
    rerender(draw(OTHER));
    expect(screen.queryByText("Hide")).toBeNull();
    expect(mockHide).not.toHaveBeenCalled();
    expect(mockEnd).not.toHaveBeenCalled();
  });
  it("dismisses an old team's status popover on a same-ID switch", () => {
    seedOther();
    act(() =>
      useViewerStore.setState((s) => ({
        workspaces: Object.fromEntries(
          Object.entries(s.workspaces).map(([team, records]) => [
            team,
            records.map((w) => ({
              ...w,
              storyFilePath: `/stories/${w.id}.md`,
              storyStatus: "todo",
            })),
          ]),
        ),
      })),
    );
    const { rerender } = render(draw(TEAM));
    fireEvent.click(
      document.querySelector(`[data-tray-chip="${A}"] button[aria-label^="Change status"]`)!,
    );
    expect(
      document.querySelector(`[data-tray-chip="${A}"] button[aria-label^="Change status"]`),
    ).toHaveAttribute("aria-expanded", "true");
    rerender(draw(OTHER));
    expect(
      document.querySelector(`[data-tray-chip="${A}"] button[aria-label^="Change status"]`),
    ).toHaveAttribute("aria-expanded", "false");
  });
  it("cancels the closed-CLI single-click timer when its team leaves", () => {
    seedOther();
    useViewerStore.setState({ layoutMode: "viewer", terminalVisible: false });
    vi.useFakeTimers();
    try {
      const { rerender } = render(draw(TEAM));
      fireEvent.click(
        document.querySelector(`[data-tray-chip="${B}"] .cs-workspace-pill-selection`)!,
        { detail: 1 },
      );
      rerender(draw(OTHER));
      act(() => vi.advanceTimersByTime(300));
      expect(useViewerStore.getState().layoutMode).toBe("viewer");
      expect(useViewerStore.getState().terminalVisible).toBe(false);
      expect(useViewerStore.getState().activeWorkspace[TEAM]).toBe(A);
    } finally {
      vi.useRealTimers();
    }
  });
  it("resets the rail Tab stop to the new team's selected epic when its position changes", () => {
    const s = useViewerStore.getState();
    useViewerStore.setState({
      workspaces: {
        [TEAM]: [gstory(A1, "/r/alpha/A-1.md"), gstory(B1, "/r/beta/B-1.md")],
        [OTHER]: [gstory(A1, "/r/alpha/A-1.md"), gstory(B1, "/r/beta/B-1.md")],
      },
      activeWorkspace: { [TEAM]: A1, [OTHER]: A1 },
      softClosedWorkspaces: {},
      trayMode: { [TEAM]: "grouped", [OTHER]: "grouped" },
      selectedEpicDirName: { [TEAM]: "alpha", [OTHER]: "alpha" },
      terminalVisible: true,
      layoutMode: "shared",
      terminalLayout: s.terminalLayout,
    });
    const { rerender } = render(draw(TEAM, groupedTree));
    expect(document.querySelector('[data-epic-rail-pill="alpha"]')).toHaveAttribute(
      "tabindex",
      "0",
    );
    rerender(draw(OTHER, { ...groupedTree, subEpics: [...groupedTree.subEpics].reverse() }));
    expect(railKeys()).toEqual(["beta", "alpha"]);
    expect(document.querySelector('[data-epic-rail-pill="alpha"]')).toHaveAttribute(
      "tabindex",
      "0",
    );
    expect(document.querySelector('[data-epic-rail-pill="beta"]')).toHaveAttribute(
      "tabindex",
      "-1",
    );
  });
});

it("Launch agent here on a context pill preselects that worktree in the sheet", () => {
  useViewerStore.setState({
    workspaces: {
      [TEAM]: [
        {
          ...story(A),
          kind: "context",
          contextWorktreeId: "cw_older",
          contextWorktreeName: "Older",
        },
      ],
    },
  });
  renderTray();
  fireEvent.contextMenu(document.querySelector(`[data-tray-chip="${A}"] button`)!);
  fireEvent.click(screen.getByRole("menuitem", { name: "Launch agent here" }));
  expect(useLaunchSheetStore.getState().request).toEqual({
    teamKey: TEAM,
    contextWorktreeId: "cw_older",
  });
  useLaunchSheetStore.getState().close();
});

describe("custom workspace group menus", () => {
  it("creates inline, rejects invalid names, and keeps the full title on the rail", async () => {
    useViewerStore.setState({
      trayMode: { [TEAM]: "grouped" },
      selectedEpicDirName: { [TEAM]: "" },
    });
    renderTray();
    fireEvent.contextMenu(
      document.querySelector<HTMLElement>(`[data-tray-chip="${A}"] button[data-pill-select]`)!,
      { clientX: 20, clientY: 30 },
    );
    fireEvent.click(screen.getByRole("menuitem", { name: "New group…" }));
    expect(screen.getByRole("dialog", { name: "New group" })).toBeInTheDocument();
    const input = screen.getByRole("textbox", { name: "Group name" });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByText("Type a name, or press Esc to cancel.")).toBeInTheDocument();
    const group = { id: "created", name: "Release checks", workspaceIds: [A] };
    vi.mocked(commands.workspaceGroupEdit).mockResolvedValueOnce({
      status: "ok",
      teamKey: TEAM,
      groups: [group],
      group,
      revision: 11,
    });
    fireEvent.change(input, { target: { value: " Release checks " } });
    await act(async () => fireEvent.keyDown(input, { key: "Enter" }));
    expect(commands.workspaceGroupEdit).toHaveBeenCalledWith(TEAM, {
      action: "create",
      name: "Release checks",
      workspaceIds: [A],
      pruneEmpty: true,
    });
    await act(async () => new Promise(requestAnimationFrame));
    const rail = screen.getByRole("button", { name: /Group Release checks, 1 workspace/ });
    expect(rail).toHaveAttribute("title", "Release checks");
    expect(useViewerStore.getState().selectedEpicDirName[TEAM]).toBe(" group:created");
    rail.focus();
    fireEvent.keyDown(rail, { key: "F10", shiftKey: true });
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename…" }));
    expect(screen.getByRole("textbox", { name: "Group name" })).toHaveValue("Release checks");
    fireEvent.keyDown(screen.getByRole("textbox", { name: "Group name" }), { key: "Tab" });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(rail).toHaveFocus();
  });
  it.each(["Escape", "Tab", "outside"])("cancels inline creation with %s", async (key) => {
    renderTray();
    fireEvent.contextMenu(
      document.querySelector<HTMLElement>(`[data-tray-chip="${A}"] button[data-pill-select]`)!,
      { clientX: 20, clientY: 30 },
    );
    fireEvent.click(screen.getByRole("menuitem", { name: "New group…" }));
    const input = screen.getByRole("textbox", { name: "Group name" });
    fireEvent.change(input, { target: { value: "Never created" } });
    if (key === "outside") {
      fireEvent.blur(input);
      fireEvent.click(screen.getByRole("dialog").parentElement!);
    } else fireEvent.keyDown(input, { key });
    expect(commands.workspaceGroupEdit).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });
  it("sends a move operation while a simultaneous MCP add is in flight", async () => {
    const target = { id: "target", name: "Target", workspaceIds: [A] };
    const source = { id: "source", name: "Source", workspaceIds: [B] };
    useViewerStore.setState({
      trayMode: { [TEAM]: "flat" },
      customGroupsByTeam: { [TEAM]: [target, source] },
    });
    let finish!: (result: import("../lib/workspace-groups").GroupResult) => void;
    vi.mocked(commands.workspaceGroupEdit).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    renderTray();
    fireEvent.contextMenu(
      document.querySelector<HTMLElement>(`[data-tray-chip="${B}"] button[data-pill-select]`)!,
      { clientX: 20, clientY: 30 },
    );
    fireEvent.keyDown(screen.getByRole("menuitem", { name: "Move to group" }), {
      key: "ArrowRight",
    });
    await act(async () => {});
    fireEvent.click(screen.getByRole("menuitemradio", { name: "Target, 1 workspace" }));
    expect(commands.workspaceGroupEdit).toHaveBeenCalledWith(TEAM, {
      action: "add",
      groupId: "target",
      workspaceId: B,
      pruneEmpty: true,
    });
    act(() =>
      useViewerStore.getState().applyWorkspaceGroups({
        teamKey: TEAM,
        revision: 50,
        groups: [{ ...target, workspaceIds: [A, M] }, source],
      }),
    );
    await act(async () =>
      finish({
        status: "ok",
        teamKey: TEAM,
        revision: 51,
        groups: [{ ...target, workspaceIds: [A, M, B] }],
      }),
    );
    expect(useViewerStore.getState().customGroupsByTeam[TEAM][0].workspaceIds).toEqual([A, M, B]);
  });
  it("New group on a sole member prunes its old group and Undo restores position and layout", async () => {
    const solo = { id: "solo", name: "Solo", workspaceIds: [A] };
    const empty = { id: "empty", name: "Reserved", workspaceIds: [] };
    const created = { id: "new", name: "New", workspaceIds: [A] };
    const layout = { root: null };
    useViewerStore.setState({
      customGroupsByTeam: { [TEAM]: [solo, empty] },
      foregroundLayoutByRail: { [TEAM]: { " group:solo": layout as never } },
    });
    renderTray();
    fireEvent.contextMenu(
      document.querySelector<HTMLElement>(`[data-tray-chip="${A}"] button[data-pill-select]`)!,
      { clientX: 20, clientY: 30 },
    );
    fireEvent.click(screen.getByRole("menuitem", { name: "New group…" }));
    const input = screen.getByRole("textbox", { name: "Group name" });
    fireEvent.change(input, { target: { value: "New" } });
    vi.mocked(commands.workspaceGroupEdit).mockResolvedValueOnce({
      status: "ok",
      teamKey: TEAM,
      revision: 52,
      groups: [empty, created],
      group: created,
    });
    await act(async () => fireEvent.keyDown(input, { key: "Enter" }));
    expect(useViewerStore.getState().customGroupsByTeam[TEAM]).toEqual([empty, created]);
    expect(useViewerStore.getState().foregroundLayoutByRail[TEAM][" group:solo"]).toBeUndefined();
    const undo = vi.mocked(toast).mock.calls.at(-1)![1]!.action as unknown as {
      onClick: () => void;
    };
    vi.mocked(commands.workspaceGroupEdit).mockResolvedValueOnce({
      status: "ok",
      teamKey: TEAM,
      revision: 53,
      groups: [solo, empty],
    });
    await act(async () => undo.onClick());
    expect(commands.workspaceGroupEdit).toHaveBeenLastCalledWith(TEAM, {
      action: "restore",
      group: solo,
      index: 0,
      pruneGroupId: created.id,
    });
    expect(useViewerStore.getState().customGroupsByTeam[TEAM]).toEqual([solo, empty]);
    expect(useViewerStore.getState().foregroundLayoutByRail[TEAM][" group:solo"]).toEqual(layout);
  });
  it("lists empty destinations and deletes the last UI member with Undo while keeping every workspace", async () => {
    const group = { id: "solo", name: "Solo", workspaceIds: [A] };
    const empty = { id: "empty", name: "Empty", workspaceIds: [] };
    const layout = { root: null };
    const reservedLayout = { root: null };
    useViewerStore.setState({
      customGroupsByTeam: { [TEAM]: [group, empty] },
      foregroundLayoutByRail: {
        [TEAM]: { " group:solo": layout as never, " group:empty": reservedLayout as never },
      },
    });
    renderTray();
    fireEvent.contextMenu(
      document.querySelector<HTMLElement>(`[data-tray-chip="${A}"] button[data-pill-select]`)!,
      { clientX: 20, clientY: 30 },
    );
    fireEvent.keyDown(screen.getByRole("menuitem", { name: "Move to group" }), {
      key: "ArrowRight",
    });
    await act(async () => {});
    expect(screen.getByRole("menuitemradio", { name: "Empty, 0 workspaces" })).toBeInTheDocument();
    expect(screen.getByRole("menuitemradio", { name: "Solo, 1 workspace" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    fireEvent.contextMenu(
      document.querySelector<HTMLElement>(`[data-tray-chip="${A}"] button[data-pill-select]`)!,
      { clientX: 20, clientY: 30 },
    );
    vi.mocked(commands.workspaceGroupEdit).mockResolvedValueOnce({
      status: "ok",
      teamKey: TEAM,
      groups: [empty],
      revision: 12,
    });
    await act(async () =>
      fireEvent.click(screen.getByRole("menuitem", { name: "Remove from group" })),
    );
    expect(useViewerStore.getState().customGroupsByTeam[TEAM]).toEqual([empty]);
    expect(useViewerStore.getState().workspaces[TEAM]).toHaveLength(2);
    expect(useViewerStore.getState().foregroundLayoutByRail[TEAM][" group:solo"]).toBeUndefined();
    expect(toast).toHaveBeenCalledWith(
      expect.stringContaining("had no other workspaces"),
      expect.objectContaining({
        duration: 10000,
        action: expect.objectContaining({ label: "Undo" }),
      }),
    );
    vi.mocked(commands.workspaceGroupEdit).mockResolvedValueOnce({
      status: "ok",
      teamKey: TEAM,
      groups: [group, empty],
      revision: 13,
    });
    const undo = vi.mocked(toast).mock.calls.at(-1)![1]!.action as unknown as {
      onClick: () => void;
    };
    await act(async () => undo.onClick());
    expect(useViewerStore.getState().customGroupsByTeam[TEAM]).toEqual([group, empty]);
    expect(useViewerStore.getState().foregroundLayoutByRail[TEAM][" group:solo"]).toEqual(layout);
    expect(commands.workspaceGroupEdit).toHaveBeenLastCalledWith(TEAM, {
      action: "restore",
      group,
      index: 0,
    });
    expect(useViewerStore.getState().foregroundLayoutByRail[TEAM][" group:empty"]).toEqual(
      reservedLayout,
    );
  });
});

it("dashboard workspace and group heads use the tray menu and keep dashboard focus after edits", async () => {
  const invoker = document.createElement("button");
  document.body.append(invoker);
  renderTray();
  const request = {
    teamKey: TEAM,
    contentRoot: "/ctx",
    anchorRect: { x: 20, y: 30, width: 0, height: 0 },
    owner: {},
    onClose: () => invoker.focus(),
  };
  expect(
    openNativeMenu({ ...request, row: { kind: "workspace", workspaceId: "unknown" } }),
  ).toBeNull();
  expect(openNativeMenu({ ...request, row: { kind: "group", groupId: "unknown" } })).toBeNull();
  act(() => {
    expect(
      openNativeMenu({ ...request, row: { kind: "workspace", workspaceId: A } }),
    ).not.toBeNull();
  });
  fireEvent.click(screen.getByRole("menuitem", { name: "New group…" }));
  const group = { id: "dashboard-group", name: "Dashboard", workspaceIds: [A] };
  vi.mocked(commands.workspaceGroupEdit).mockResolvedValueOnce({
    status: "ok",
    teamKey: TEAM,
    groups: [group],
    group,
    revision: 11,
  });
  const input = screen.getByRole("textbox", { name: "Group name" });
  fireEvent.change(input, { target: { value: group.name } });
  await act(async () => fireEvent.keyDown(input, { key: "Enter" }));
  await act(async () => new Promise(requestAnimationFrame));
  expect(invoker).toHaveFocus();
  act(() => {
    expect(
      openNativeMenu({ ...request, row: { kind: "group", groupId: group.id } }),
    ).not.toBeNull();
  });
  expect(screen.getByRole("menuitem", { name: "Rename…" })).toBeInTheDocument();
  expect(screen.getByRole("menuitem", { name: "Delete group" })).toBeInTheDocument();
  fireEvent.keyDown(document, { key: "Escape" });
  invoker.remove();
});
