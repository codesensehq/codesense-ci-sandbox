// SPDX-License-Identifier: MIT
import { projectSessionStatus, resetSessionProjection } from "@/test/session-projection-fixture";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@/test-utils";
import userEvent from "@testing-library/user-event";
import { TerminalTileChrome, partitionTabs } from "./terminal-tile-chrome";
import { useViewerStore } from "@/shared/stores/viewer-store";
import type { TerminalTab, Workspace } from "@/shared/stores/viewer-store";
import type { MachineSummary } from "@/tauri/commands";
import { useMachinesStore } from "@/features/machines/stores/machines-store";
import { teamPieceHue } from "@/features/viewer/lib/agent-list-view";

const ws: Workspace = {
  id: "epic-x-story-1",
  kind: "story",
  cwd: "/wt/story-1",
  status: "ready",
  storyId: "SW-EW-3",
  storyFilePath: "/ctx/story-1.md",
};

function tab(id: string): TerminalTab {
  return { id, name: id, sessionId: id };
}

function renderChrome(overrides: Partial<React.ComponentProps<typeof TerminalTileChrome>> = {}) {
  const noop = vi.fn();
  return render(
    <TerminalTileChrome
      workspace={ws}
      isFocused={false}
      tabs={[tab("t1")]}
      activeTabId="t1"
      onSelectTab={noop}
      onCloseTab={noop}
      onHideTab={noop}
      onEndTab={noop}
      onEndOtherTabs={noop}
      onEndAllTabs={noop}
      onNewShell={noop}
      onReorderTab={noop}
      onRenameTab={noop}
      onHideTile={noop}
      onFocusTile={noop}
      {...overrides}
    />,
  );
}

describe("TerminalTileChrome — identity", () => {
  it("renders the STORY-ID label", () => {
    renderChrome();
    expect(screen.getByText("SW-EW-3")).toBeInTheDocument();
  });

  it("marks the store-selected tile (data-focused)", () => {
    renderChrome({ isFocused: true });
    expect(screen.getByTestId(`tile-chrome-${ws.id}`)).toHaveAttribute("data-focused");
  });

  it("does not mark an unfocused tile", () => {
    renderChrome({ isFocused: false });
    expect(screen.getByTestId(`tile-chrome-${ws.id}`)).not.toHaveAttribute("data-focused");
  });

  // SW-EW-34 v2 (D8): on a lone console tile the top switcher owns identity, so
  // the `[status] WORKSPACE-ID` block is dropped — but the tab bar stays.
  it("hideIdentity drops the id/status block but keeps the tabs", () => {
    renderChrome({ hideIdentity: true });
    expect(screen.queryByTestId(`tile-label-${ws.id}`)).toBeNull();
    expect(screen.queryByText("SW-EW-3")).toBeNull();
    expect(screen.queryByTestId(`tile-story-status-${ws.id}`)).toBeNull();
    // tabs + the new-shell affordance still render
    expect(screen.getByRole("button", { name: "New terminal" })).toBeInTheDocument();
  });

  it("renders the identity block by default (hideIdentity unset)", () => {
    renderChrome();
    expect(screen.getByTestId(`tile-label-${ws.id}`)).toBeInTheDocument();
  });
});

describe("TerminalTileChrome — the Expanded · all tabs toggle (SW-EW-46 D2)", () => {
  const toggleId = `tile-grid-toggle-${ws.id}`;

  it("hides the toggle when the tile is not expanded (canGrid false)", () => {
    renderChrome({ tabs: [tab("t1"), tab("t2")], onShowAllTabs: vi.fn(), onShowOneTab: vi.fn() });
    expect(screen.queryByTestId(toggleId)).toBeNull();
  });

  it("hides the toggle on a single-terminal tile that isn't gridded", () => {
    renderChrome({
      canGrid: true,
      tabs: [tab("t1")],
      onShowAllTabs: vi.fn(),
      onShowOneTab: vi.fn(),
    });
    expect(screen.queryByTestId(toggleId)).toBeNull();
  });

  it("shows the unpressed 'Expanded · all tabs' toggle when expanded with ≥2 tabs and routes the click", () => {
    const onShowAllTabs = vi.fn();
    renderChrome({
      canGrid: true,
      tabs: [tab("t1"), tab("t2")],
      onShowAllTabs,
      onShowOneTab: vi.fn(),
    });
    const btn = screen.getByTestId(toggleId);
    expect(btn).toHaveAttribute("aria-label", "Expanded · all tabs");
    expect(btn).toHaveAttribute("aria-pressed", "false");
    fireEvent.click(btn);
    expect(onShowAllTabs).toHaveBeenCalledTimes(1);
  });

  it("stays available, pressed, even when down to a single cell (no dead-end)", () => {
    const onShowOneTab = vi.fn();
    renderChrome({
      canGrid: true,
      gridActive: true,
      tabs: [tab("t1")],
      panedTabIds: ["t1"],
      onShowAllTabs: vi.fn(),
      onShowOneTab,
    });
    const btn = screen.getByTestId(toggleId);
    expect(btn).toHaveAttribute("aria-label", "Expanded · all tabs");
    expect(btn).toHaveAttribute("aria-pressed", "true");
    fireEvent.click(btn);
    expect(onShowOneTab).toHaveBeenCalledTimes(1);
  });
});

describe("TerminalTileChrome — view-options overflow menu (OC-14)", () => {
  it("renders the view-options kebab on a story tile (all three toggles qualify)", () => {
    renderChrome();
    expect(screen.getByTestId(`tile-view-options-${ws.id}`)).toBeInTheDocument();
  });

  // SW-VE-12: every folder-rooted kind now carries the Source diff + Code
  // editor view toggles — so the kebab is never empty on a `generic`/`root`
  // tile. (The prior "hides on no view toggles" assertion no longer applies —
  // no kind returns an empty option set.)
  it("SW-VE-12: renders the kebab on a `generic` tile — source-diff + code-editor qualify", async () => {
    const user = userEvent.setup();
    const generic: Workspace = { id: "g", kind: "generic", cwd: "/r", status: "ready" };
    renderChrome({ workspace: generic });
    expect(screen.getByTestId("tile-view-options-g")).toBeInTheDocument();
    await user.click(screen.getByTestId("tile-view-options-g"));
    expect(await screen.findByTestId("source-diff-toggle-g")).toBeInTheDocument();
    expect(screen.getByTestId("code-editor-toggle-g")).toBeInTheDocument();
    // No story-HUD on a `generic` kind — that one stays kind-gated.
    expect(screen.queryByTestId("story-hud-toggle-g")).not.toBeInTheDocument();
  });

  it("a kind without story-HUD still gets ⋮ via Split, and Split splits the active tab", async () => {
    const user = userEvent.setup();
    const onSplitTab = vi.fn();
    const root: Workspace = { id: "r", kind: "root", cwd: "/r", status: "ready" };
    renderChrome({ workspace: root, tabs: [tab("t1"), tab("t2")], activeTabId: "t2", onSplitTab });
    const kebab = screen.getByTestId("tile-view-options-r");
    // SW-VE-12: `root` now also carries source-diff + code-editor toggles, so
    // the aria-label reads as "Tile options" (view options + split, not just split).
    expect(kebab.getAttribute("aria-label")).toBe("Tile options");
    await user.click(kebab);
    const split = await screen.findByTestId("tile-menu-split-r");
    expect(split).toHaveTextContent("Split");
    await user.click(split);
    expect(onSplitTab).toHaveBeenCalledWith("t2");
  });

  it("a double-click on the kebab does NOT zoom the tile (only the chrome bar does)", () => {
    const onToggleZoom = vi.fn();
    renderChrome({ onToggleZoom });
    // The kebab swallows its own double-click — opening the menu never zooms.
    fireEvent.doubleClick(screen.getByTestId(`tile-view-options-${ws.id}`));
    expect(onToggleZoom).not.toHaveBeenCalled();
    // Sanity: the chrome bar itself DOES zoom on double-click.
    fireEvent.doubleClick(screen.getByTestId(`tile-chrome-${ws.id}`));
    expect(onToggleZoom).toHaveBeenCalledTimes(1);
  });

  it("opens to the three view toggles on a story tile", async () => {
    const user = userEvent.setup();
    renderChrome();
    await user.click(screen.getByTestId(`tile-view-options-${ws.id}`));
    expect(await screen.findByTestId(`story-hud-toggle-${ws.id}`)).toBeInTheDocument();
    expect(screen.getByTestId(`source-diff-toggle-${ws.id}`)).toBeInTheDocument();
    expect(screen.getByTestId(`code-editor-toggle-${ws.id}`)).toBeInTheDocument();
  });

  it("SW-VE-12: an epic tile offers the epic summary + code-editor (source-diff stays off)", async () => {
    const user = userEvent.setup();
    const epic: Workspace = { id: "e", kind: "epic", cwd: "/r", status: "ready" };
    renderChrome({ workspace: epic });
    await user.click(screen.getByTestId("tile-view-options-e"));
    const hud = await screen.findByTestId("story-hud-toggle-e");
    // The epic HUD renders the epic README, so the item reads "Epic summary".
    expect(hud).toHaveTextContent("Epic summary");
    // SW-VE-12: epic gains code-editor via `sourceCapable`. Source diff still
    // gates on `diffCapable` (false for epic) so the overlay never opens on
    // a cwd whose enclosing-repo diff would mislead.
    expect(screen.getByTestId("code-editor-toggle-e")).toBeInTheDocument();
    expect(screen.queryByTestId("source-diff-toggle-e")).not.toBeInTheDocument();
  });

  // Table-driven so a copy-paste swap (wrong toggle action on a descriptor) fails:
  // each item must flip ONLY its own store slice, and exactly once (a double-fire
  // would land back on `before` and fail the `!before` assertion). Story-HUD stays
  // keyed by workspaceId; the per-tab primitives key on the active tab's sessionId.
  it.each([
    { testId: "story-hud-toggle", slice: "storyHudShown" as const, keyBy: "ws" as const },
    {
      testId: "source-diff-toggle",
      slice: "sourceDiffShownBySession" as const,
      keyBy: "session" as const,
    },
    {
      testId: "code-editor-toggle",
      slice: "worktreeEditorOpenBySession" as const,
      keyBy: "session" as const,
    },
  ])(
    "toggling $testId flips only its own store slice ($slice)",
    async ({ testId, slice, keyBy }) => {
      const user = userEvent.setup();
      const id = `oc14-${testId}`;
      const storyWs: Workspace = { ...ws, id };
      const key = keyBy === "ws" ? id : "t1";
      const before = useViewerStore.getState()[slice][key] ?? false;
      renderChrome({ workspace: storyWs });
      await user.click(screen.getByTestId(`tile-view-options-${id}`));
      await user.click(await screen.findByTestId(`${testId}-${id}`));
      expect(useViewerStore.getState()[slice][key]).toBe(!before);
    },
  );

  it("closes the menu on select and still fires the toggle", async () => {
    const user = userEvent.setup();
    // A dedicated id keeps this test's toggle mutation out of the shared `ws`.
    const id = "oc14-close-on-select";
    const storyWs: Workspace = { ...ws, id };
    const before = useViewerStore.getState().storyHudShown[id] ?? false;
    renderChrome({ workspace: storyWs });
    await user.click(screen.getByTestId(`tile-view-options-${id}`));
    await user.click(await screen.findByTestId(`story-hud-toggle-${id}`));
    // A stray double-fire would land back on `before`, so this guards the toggle
    // firing on the closing select.
    expect(useViewerStore.getState().storyHudShown[id]).toBe(!before);
    // Selecting closes the menu — the selected item and its siblings all leave.
    await waitFor(() => {
      expect(screen.queryByTestId(`story-hud-toggle-${id}`)).not.toBeInTheDocument();
      expect(screen.queryByTestId(`source-diff-toggle-${id}`)).not.toBeInTheDocument();
    });
  });

  // SLW-DV-3: "Code editor" is a view toggle (checkbox) like Source diff.
  it("offers a 'Code editor' toggle with a keystroke hint on a story tile", async () => {
    const user = userEvent.setup();
    renderChrome();
    await user.click(screen.getByTestId(`tile-view-options-${ws.id}`));
    const item = await screen.findByTestId(`code-editor-toggle-${ws.id}`);
    expect(item).toHaveTextContent("Code editor");
    expect(item.querySelector('[data-slot="dropdown-menu-shortcut"]')).not.toBeNull();
  });

  it("SW-VE-12: the code-editor toggle renders on an epic tile via `sourceCapable`", async () => {
    const user = userEvent.setup();
    const epic: Workspace = { id: "e2", kind: "epic", cwd: "/r", status: "ready" };
    renderChrome({ workspace: epic });
    await user.click(screen.getByTestId("tile-view-options-e2"));
    await screen.findByTestId("story-hud-toggle-e2");
    expect(screen.getByTestId("code-editor-toggle-e2")).toBeInTheDocument();
    // The source-diff toggle stays off on epic (`diffCapable` is false).
    expect(screen.queryByTestId("source-diff-toggle-e2")).not.toBeInTheDocument();
  });

  it("marks the open overlay's item checked; only one is checked (mutual exclusion)", async () => {
    const user = userEvent.setup();
    const id = "mx-menu";
    const storyWs: Workspace = { ...ws, id };
    useViewerStore.setState({
      storyHudShown: {},
      storyHudExpanded: {},
      sourceDiffShownBySession: { t1: true },
      worktreeEditorOpenBySession: {},
    });
    renderChrome({ workspace: storyWs });

    // The open overlay (diff) reads as checked; every other row reads unchecked.
    await user.click(screen.getByTestId(`tile-view-options-${id}`));
    expect(await screen.findByTestId(`source-diff-toggle-${id}`)).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(screen.getByTestId(`story-hud-toggle-${id}`)).toHaveAttribute("aria-checked", "false");
    expect(screen.getByTestId(`code-editor-toggle-${id}`)).toHaveAttribute("aria-checked", "false");

    // Opening the code editor closes the diff → the check moves (only one checked).
    await user.click(screen.getByTestId(`code-editor-toggle-${id}`));
    await user.click(screen.getByTestId(`tile-view-options-${id}`)); // reopen the menu
    expect(await screen.findByTestId(`code-editor-toggle-${id}`)).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(screen.getByTestId(`source-diff-toggle-${id}`)).toHaveAttribute("aria-checked", "false");
    expect(screen.getByTestId(`story-hud-toggle-${id}`)).toHaveAttribute("aria-checked", "false");
    expect(useViewerStore.getState().sourceDiffShownBySession.t1 ?? false).toBe(false);
  });

  it("a tile of another team than the active one toggles its source diff in that team", async () => {
    const user = userEvent.setup();
    const id = "other-team-tile";
    const toggleSourceDiffShown = vi.fn();
    const real = useViewerStore.getState().toggleSourceDiffShown;
    useViewerStore.setState({ toggleSourceDiffShown });
    try {
      renderChrome({ workspace: { ...ws, id }, teamKey: "acme:other" });
      await user.click(screen.getByTestId(`tile-view-options-${id}`));
      await user.click(await screen.findByTestId(`source-diff-toggle-${id}`));
      expect(toggleSourceDiffShown).toHaveBeenCalledWith({
        sessionId: "t1",
        workspaceId: id,
        teamKey: "acme:other",
      });
    } finally {
      useViewerStore.setState({ toggleSourceDiffShown: real });
    }
  });

  it("a tile of another team than the active one wears that team's hue, by its place there", () => {
    const id = "other-team-tile";
    const real = useViewerStore.getState().workspaces;
    // The active team has no such workspace; in its own team it is the second.
    useViewerStore.setState({
      workspaces: {
        "acme:default": [],
        "acme:other": [
          { ...ws, id: "first" },
          { ...ws, id },
        ],
      },
    });
    try {
      const { container } = renderChrome({ workspace: { ...ws, id }, teamKey: "acme:other" });
      const rings = [...container.querySelectorAll<HTMLElement>("*")]
        .map((el) => el.style.getPropertyValue("--ag-tab-ring"))
        .filter(Boolean);
      expect(rings).toContain(`oklch(78% 0.06 ${teamPieceHue(1)})`);
    } finally {
      useViewerStore.setState({ workspaces: real });
    }
  });

  it("opening the source diff from the menu records the pre-menu focus as its return target", async () => {
    const { takeSourceDiffReturnFocus } = await import("./source-diff-return-focus");
    const user = userEvent.setup();
    const id = "rf-menu";
    useViewerStore.setState({ sourceDiffShownBySession: {}, worktreeEditorOpenBySession: {} });
    renderChrome({ workspace: { ...ws, id } });
    const terminal = document.createElement("textarea");
    document.body.appendChild(terminal);
    try {
      terminal.focus();
      await user.click(screen.getByTestId(`tile-view-options-${id}`));
      await user.click(await screen.findByTestId(`source-diff-toggle-${id}`));
      expect(useViewerStore.getState().sourceDiffShownBySession.t1).toBe(true);
      expect(takeSourceDiffReturnFocus("t1")).toBe(terminal);
    } finally {
      terminal.remove();
    }
  });

  it("a menu opened from the keyboard returns focus to its trigger (never body)", async () => {
    const { takeSourceDiffReturnFocus } = await import("./source-diff-return-focus");
    const user = userEvent.setup();
    const id = "rf-kbd";
    useViewerStore.setState({ sourceDiffShownBySession: {}, worktreeEditorOpenBySession: {} });
    renderChrome({ workspace: { ...ws, id } });
    const trigger = screen.getByTestId(`tile-view-options-${id}`);
    trigger.focus();
    await user.keyboard("{Enter}");
    await user.click(await screen.findByTestId(`source-diff-toggle-${id}`));
    expect(useViewerStore.getState().sourceDiffShownBySession.t1).toBe(true);
    expect(takeSourceDiffReturnFocus("t1")).toBe(trigger);
  });

  it("another menu's trigger that had focus is a valid return target, and the menu keeps its label", async () => {
    const { takeSourceDiffReturnFocus } = await import("./source-diff-return-focus");
    const user = userEvent.setup();
    const id = "rf-other";
    useViewerStore.setState({ sourceDiffShownBySession: {}, worktreeEditorOpenBySession: {} });
    renderChrome({ workspace: { ...ws, id } });
    const other = document.createElement("button");
    other.setAttribute("aria-haspopup", "menu");
    document.body.appendChild(other);
    try {
      other.focus();
      await user.click(screen.getByTestId(`tile-view-options-${id}`));
      const menu = await screen.findByTestId(`tile-view-options-menu-${id}`);
      const labelledBy = menu.getAttribute("aria-labelledby");
      expect(labelledBy && document.getElementById(labelledBy)).toBe(
        screen.getByTestId(`tile-view-options-${id}`),
      );
      await user.click(screen.getByTestId(`source-diff-toggle-${id}`));
      expect(takeSourceDiffReturnFocus("t1")).toBe(other);
    } finally {
      other.remove();
    }
  });

  it("toggling 'Code editor' opens then closes the editor across menu visits", async () => {
    const user = userEvent.setup();
    const id = "dv3-toggle";
    const storyWs: Workspace = { ...ws, id };
    // Session memory: a remembered file is preserved across the toggle (the editor
    // resumes it on open) — the menu toggle only flips visibility.
    useViewerStore.setState({
      worktreeEditorOpenBySession: {},
      worktreeEditorFile: { [id]: "src/mod.ts" },
    });
    renderChrome({ workspace: storyWs });
    await user.click(screen.getByTestId(`tile-view-options-${id}`));
    await user.click(await screen.findByTestId(`code-editor-toggle-${id}`));
    expect(useViewerStore.getState().worktreeEditorOpenBySession.t1).toBe(true);
    expect(useViewerStore.getState().worktreeEditorFile[id]).toBe("src/mod.ts");
    // Selecting closed the menu; reopen it to flip the toggle back off.
    await user.click(screen.getByTestId(`tile-view-options-${id}`));
    await user.click(await screen.findByTestId(`code-editor-toggle-${id}`));
    expect(useViewerStore.getState().worktreeEditorOpenBySession.t1).toBe(false);
    // Closing preserves the remembered file so a later reopen resumes it.
    expect(useViewerStore.getState().worktreeEditorFile[id]).toBe("src/mod.ts");
  });
});

describe("TerminalTileChrome — story-status dropdown (SW-EW-18)", () => {
  // the workspace-level AGENT roll-up dot is gone from the identity — a
  // foreground tile's per-tab dots already report agent status.
  it("does NOT render an agent roll-up status dot in the identity", () => {
    renderChrome();
    expect(screen.queryByTestId(`tile-status-${ws.id}`)).not.toBeInTheDocument();
  });

  // a story tile with a storyStatus shows a story-status control.
  it("renders a story-status control for a story tile with a storyStatus", () => {
    const storyWs: Workspace = { ...ws, storyStatus: "in-progress" };
    renderChrome({ workspace: storyWs });
    expect(screen.getByTestId(`tile-story-status-${ws.id}`)).toBeInTheDocument();
  });

  it("omits the control for a story tile with no storyStatus", () => {
    renderChrome(); // default ws has no storyStatus
    expect(screen.queryByTestId(`tile-story-status-${ws.id}`)).not.toBeInTheDocument();
  });

  it("omits the control for a non-story (integration) tile", () => {
    const intg: Workspace = { id: "i", kind: "epic", cwd: "/r", status: "ready" };
    renderChrome({ workspace: intg });
    expect(screen.queryByTestId("tile-story-status-i")).not.toBeInTheDocument();
  });

  it("selecting a status fires onStoryStatusChange", () => {
    const onStoryStatusChange = vi.fn();
    const storyWs: Workspace = { ...ws, storyStatus: "in-progress" };
    renderChrome({ workspace: storyWs, onStoryStatusChange });
    // open the popover (the stage rail is wrapped in the trigger button)
    fireEvent.click(screen.getByTestId(`tile-story-status-${ws.id}`).querySelector("button")!);
    // pick a different status from the menu
    fireEvent.click(screen.getByText("Done"));
    expect(onStoryStatusChange).toHaveBeenCalledWith("done");
  });
});

describe("TerminalTileChrome — overflow dropdown", () => {
  it("shows no overflow control at/under the visible threshold", () => {
    renderChrome({ tabs: [tab("t1"), tab("t2"), tab("t3"), tab("t4")], activeTabId: "t1" });
    expect(screen.queryByTestId(`tile-overflow-${ws.id}`)).not.toBeInTheDocument();
  });

  it("collapses surplus tabs into a ⌄ +N dropdown past the threshold", () => {
    const tabs = ["t1", "t2", "t3", "t4", "t5", "t6"].map((id) => tab(id));
    const onSelectTab = vi.fn();
    renderChrome({ tabs, activeTabId: "t1", onSelectTab });
    const overflow = screen.getByTestId(`tile-overflow-${ws.id}`);
    expect(overflow).toHaveTextContent("+2"); // 6 tabs, 4 visible → 2 overflow
    fireEvent.click(overflow);
    const menu = screen.getByTestId(`tile-overflow-menu-${ws.id}`);
    expect(menu).toBeInTheDocument();
    // selecting an overflow tab routes through onSelectTab (focus-then-act)
    fireEvent.click(screen.getByText("t6"));
    expect(onSelectTab).toHaveBeenCalledWith("t6");
  });
});

describe("partitionTabs — active tab is never hidden in overflow (review fix)", () => {
  const mk = (n: number) => Array.from({ length: n }, (_, i) => tab(`t${i}`));

  it("returns all tabs when under the cap", () => {
    const { visible, overflow } = partitionTabs(mk(3), "t0", 4);
    expect(visible).toHaveLength(3);
    expect(overflow).toHaveLength(0);
  });

  it("keeps the first window when the active tab is already visible", () => {
    const { visible, overflow } = partitionTabs(mk(6), "t1", 4);
    expect(visible.map((t) => t.id)).toEqual(["t0", "t1", "t2", "t3"]);
    expect(overflow.map((t) => t.id)).toEqual(["t4", "t5"]);
  });

  it("pins the active tab into the visible window when it would overflow", () => {
    const { visible, overflow } = partitionTabs(mk(6), "t5", 4);
    expect(visible.map((t) => t.id)).toContain("t5"); // active always visible
    expect(overflow.map((t) => t.id)).not.toContain("t5");
    expect(visible).toHaveLength(4);
  });
});

describe("TerminalTileChrome — focused title highlight (SW-EW-12)", () => {
  it("brightens + bolds the story-id label on the focused tile", () => {
    renderChrome({ isFocused: true });
    expect(screen.getByTestId(`tile-label-${ws.id}`).className).toContain("font-semibold");
  });

  it("does not bold the label on an unfocused tile", () => {
    renderChrome({ isFocused: false });
    expect(screen.getByTestId(`tile-label-${ws.id}`).className).not.toContain("font-semibold");
  });
});

describe("TerminalTileChrome — double-click opens story + recenters pipeline (SW-EW-12)", () => {
  it("double-clicking the story-id label calls onOpenStoryDoc with its storyFilePath", () => {
    const onOpenStoryDoc = vi.fn();
    renderChrome({ onOpenStoryDoc });
    fireEvent.doubleClick(screen.getByTestId(`tile-label-${ws.id}`));
    expect(onOpenStoryDoc).toHaveBeenCalledWith("/ctx/story-1.md");
  });

  it("a single click does NOT open the doc (only focus, handled elsewhere)", () => {
    const onOpenStoryDoc = vi.fn();
    renderChrome({ onOpenStoryDoc });
    fireEvent.click(screen.getByTestId(`tile-label-${ws.id}`), { detail: 1 });
    expect(onOpenStoryDoc).not.toHaveBeenCalled();
  });

  it("does nothing for a tile with no doc path (e.g. a doc-less integration)", () => {
    const onOpenStoryDoc = vi.fn();
    const intg: Workspace = { id: "i", kind: "epic", cwd: "/r", status: "ready" };
    renderChrome({ workspace: intg, onOpenStoryDoc });
    fireEvent.doubleClick(screen.getByTestId("tile-label-i"));
    expect(onOpenStoryDoc).not.toHaveBeenCalled();
  });

  it("double-clicking an epic tile opens its epic doc, like a story tile (SW-EW-35)", () => {
    const onOpenStoryDoc = vi.fn();
    const epic: Workspace = {
      id: "i",
      kind: "epic",
      cwd: "/r/epic",
      status: "ready",
      epicDirPath: "/r/epic",
      epicDocPath: "/r/epic/epic.md",
    };
    renderChrome({ workspace: epic, onOpenStoryDoc });
    fireEvent.doubleClick(screen.getByTestId("tile-label-i"));
    expect(onOpenStoryDoc).toHaveBeenCalledWith("/r/epic/epic.md");
  });

  it("double-clicking the label does NOT also toggle zoom (stops bubbling to the chrome)", () => {
    // Regression: SW-EW-4 put onDoubleClick={onToggleZoom} on the chrome bar; the
    // label's dblclick must not bubble up to it, or opening a doc also zooms.
    const onToggleZoom = vi.fn();
    renderChrome({ onOpenStoryDoc: vi.fn(), onToggleZoom });
    fireEvent.doubleClick(screen.getByTestId(`tile-label-${ws.id}`));
    expect(onToggleZoom).not.toHaveBeenCalled();
    // Sanity: double-clicking the chrome bar itself DOES toggle zoom.
    fireEvent.doubleClick(screen.getByTestId(`tile-chrome-${ws.id}`));
    expect(onToggleZoom).toHaveBeenCalledTimes(1);
  });

  // SW-EW-28 Task 2: the double-click open gesture selects the id text as a side
  // effect (bad UX). `select-none` suppresses the highlight without touching the
  // gesture — the open-doc dblclick above must keep working.
  it("the story-id label is not text-selectable (select-none)", () => {
    renderChrome({ onOpenStoryDoc: vi.fn() });
    expect(screen.getByTestId(`tile-label-${ws.id}`).className).toContain("select-none");
  });
});

describe("TerminalTileChrome — controls", () => {
  it("interacting with the chrome focuses the tile (active-terminal invariant)", () => {
    const onFocusTile = vi.fn();
    renderChrome({ onFocusTile });
    fireEvent.mouseDown(screen.getByTestId(`tile-chrome-${ws.id}`));
    expect(onFocusTile).toHaveBeenCalled();
  });
});

describe("TerminalTileChrome — the two close verbs (Hide / End)", () => {
  it("– reads Hide and is a single-purpose control (no right-click gesture)", async () => {
    const user = userEvent.setup();
    renderChrome({});
    const hide = screen.getByTestId(`tile-hide-${ws.id}`);
    await user.hover(hide);
    expect(await screen.findByRole("tooltip")).toHaveTextContent("Hide · keeps running");
    fireEvent.contextMenu(hide);
    expect(screen.queryByText("End")).not.toBeInTheDocument();
  });

  it("× renders only when onCloseTile is wired, and names the verb it takes", async () => {
    const user = userEvent.setup();
    renderChrome({ onCloseTile: undefined });
    expect(screen.queryByTestId(`tile-close-${ws.id}`)).not.toBeInTheDocument();
    renderChrome({ onCloseTile: vi.fn() });
    const close = screen.getByTestId(`tile-close-${ws.id}`);
    // Unprobed (or unprobeable) reads as Hide — the verb that never loses anything.
    expect(close).toHaveAttribute("aria-label", expect.stringMatching(/^Hide /));
    await user.hover(close);
    expect(await screen.findByRole("tooltip")).toHaveTextContent("Hide · keeps running");
  });

  it("clicking × fires onCloseTile, clicking – fires onHideTile", () => {
    const onHideTile = vi.fn();
    const onCloseTile = vi.fn();
    renderChrome({ onHideTile, onCloseTile });
    fireEvent.click(screen.getByTestId(`tile-close-${ws.id}`));
    expect(onCloseTile).toHaveBeenCalledTimes(1);
    expect(onHideTile).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId(`tile-hide-${ws.id}`));
    expect(onHideTile).toHaveBeenCalledTimes(1);
  });

  it("no Close / Minimize / Exit / kill wording remains on the tile", () => {
    renderChrome({ onCloseTile: vi.fn() });
    expect(screen.queryByLabelText(/Minimize|Close/)).not.toBeInTheDocument();
    expect(screen.queryByText(/Exit/)).not.toBeInTheDocument();
    expect(screen.queryByText(/kill/i)).not.toBeInTheDocument();
  });
});

describe("TerminalTileChrome — SW-EW-4 zoom toggle", () => {
  it("renders no zoom toggle when onToggleZoom is absent (1-up)", () => {
    renderChrome();
    expect(screen.queryByTestId(`tile-zoom-${ws.id}`)).not.toBeInTheDocument();
  });

  it("renders an Expand toggle that fires onToggleZoom", () => {
    const onToggleZoom = vi.fn();
    renderChrome({ onToggleZoom });
    const btn = screen.getByTestId(`tile-zoom-${ws.id}`);
    expect(btn).toHaveAttribute("aria-label", expect.stringMatching(/Expand/));
    fireEvent.click(btn);
    expect(onToggleZoom).toHaveBeenCalled();
  });

  it("flips to Back to workspaces + shows the +N hidden hint while zoomed", () => {
    renderChrome({ onToggleZoom: vi.fn(), isZoomed: true, hiddenTileCount: 3 });
    expect(screen.getByTestId(`tile-zoom-${ws.id}`)).toHaveAttribute(
      "aria-label",
      expect.stringMatching(/Back to workspaces/),
    );
    expect(screen.getByTestId(`tile-hidden-hint-${ws.id}`)).toHaveTextContent("+3 hidden");
  });

  it("badges the +N hidden hint with the hidden tiles' most urgent mark (zoom case)", () => {
    projectSessionStatus("hidden-a", "unknown");
    projectSessionStatus("hidden-b", "paused");
    renderChrome({
      onToggleZoom: vi.fn(),
      isZoomed: true,
      hiddenTileCount: 2,
      hiddenAttentionScope: { sessionIds: ["hidden-a", "hidden-b"] },
    });
    const badge = screen.getByTestId(`tile-hidden-attention-${ws.id}`);
    expect(badge).toHaveAttribute("data-mark", "permission");
    expect(badge).toHaveAttribute("aria-label", "Hidden tiles: 1 needs approval");
  });

  it("does not show the +N hidden hint when not zoomed", () => {
    renderChrome({ onToggleZoom: vi.fn(), isZoomed: false, hiddenTileCount: 3 });
    expect(screen.queryByTestId(`tile-hidden-hint-${ws.id}`)).not.toBeInTheDocument();
  });

  it("double-clicking the chrome bar toggles zoom", () => {
    const onToggleZoom = vi.fn();
    renderChrome({ onToggleZoom });
    fireEvent.doubleClick(screen.getByTestId(`tile-chrome-${ws.id}`));
    expect(onToggleZoom).toHaveBeenCalled();
  });

  it("shows a fuller tab strip when zoomed (raises the overflow budget, )", () => {
    const tabs = ["t1", "t2", "t3", "t4", "t5", "t6"].map((id) => tab(id));
    // Un-zoomed: 6 tabs > 4 budget → overflow control present.
    const { rerender } = renderChrome({ tabs, activeTabId: "t1", onToggleZoom: vi.fn() });
    expect(screen.getByTestId(`tile-overflow-${ws.id}`)).toBeInTheDocument();
    // Zoomed: 6 tabs <= 8 budget → no overflow (the fuller strip).
    rerender(
      <TerminalTileChrome
        workspace={ws}
        isFocused={false}
        tabs={tabs}
        activeTabId="t1"
        onSelectTab={vi.fn()}
        onCloseTab={vi.fn()}
        onHideTab={vi.fn()}
        onEndTab={vi.fn()}
        onEndOtherTabs={vi.fn()}
        onEndAllTabs={vi.fn()}
        onNewShell={vi.fn()}
        onReorderTab={vi.fn()}
        onRenameTab={vi.fn()}
        onHideTile={vi.fn()}
        onFocusTile={vi.fn()}
        onToggleZoom={vi.fn()}
        isZoomed
      />,
    );
    expect(screen.queryByTestId(`tile-overflow-${ws.id}`)).not.toBeInTheDocument();
  });
});

describe("TerminalTileChrome — first-class epic chrome (SW-EW-25)", () => {
  // An integration workspace's id is the raw epic doc path — the bug this story
  // fixes is the chrome rendering THAT instead of a clean label.
  const epicWs: Workspace = {
    id: "integration:/Users/x/ctx/_bmad-output/.../epic-workspaces/SW-EW-20-foo.md",
    kind: "epic",
    cwd: "/r",
    status: "ready",
    epicDirPath: "epic-workspaces",
    epicDocPath: "/Users/x/ctx/_bmad-output/.../epic-workspaces/SW-EW-20-foo.md",
  };

  // clean label = prettified epic dir leaf, NOT the absolute path id.
  it("renders the prettified epic leaf as the label, not the raw path id", () => {
    renderChrome({ workspace: epicWs });
    expect(screen.getByTestId(`tile-label-${epicWs.id}`)).toHaveTextContent("Epic Workspaces");
    expect(screen.queryByText(epicWs.id)).not.toBeInTheDocument();
  });

  // SW-EW-35: the `Epic` badge was removed (the leading dir icon marks an epic
  // tile), so neither kind renders it.
  it("renders no `Epic` badge on either tile kind (SW-EW-35)", () => {
    renderChrome({ workspace: epicWs });
    expect(screen.queryByTestId(`tile-epic-badge-${epicWs.id}`)).not.toBeInTheDocument();
    renderChrome();
    expect(screen.queryByTestId(`tile-epic-badge-${ws.id}`)).not.toBeInTheDocument();
  });

  // SW-EW-35: the leading identity slot for an epic is its DIR icon, NOT a
  // rolled-up lifecycle-status dot (which read as a stray blue dot). Agent
  // activity shows on the per-tab notification dots, like a story tile.
  it("renders the epic dir icon in the leading slot, not a status dot", () => {
    renderChrome({ workspace: epicWs });
    const icon = screen.getByTestId(`tile-epic-icon-${epicWs.id}`);
    expect(icon.querySelector("svg")).not.toBeNull(); // the FolderKanban glyph
    expect(icon.querySelector("button")).toBeNull(); // identity, not an interactive control
    // No epic lifecycle-status chip and no story-status control on an epic tile.
    expect(screen.queryByTestId(`tile-epic-status-${epicWs.id}`)).not.toBeInTheDocument();
    expect(screen.queryByTestId(`tile-story-status-${epicWs.id}`)).not.toBeInTheDocument();
  });

  it("does NOT render the epic dir icon on a story tile", () => {
    renderChrome();
    expect(screen.queryByTestId(`tile-epic-icon-${ws.id}`)).not.toBeInTheDocument();
  });
});

describe("TerminalTileChrome — controls survive a long label (SW-EW-25 )", () => {
  // The raw-path title that triggered this story overflowed and pushed the
  // zoom/minimize controls off the tile. Guard: with an extreme label the
  // controls still render, and the layout primitives that prevent the overflow
  // (label `truncate` + `min-w-0`, shrinkable identity block) are in place.
  const longWs: Workspace = {
    ...ws,
    storyId: `ridiculously-long-label-${"x".repeat(200)}`,
  };

  it("keeps the zoom + Hide controls present despite a very long label", () => {
    renderChrome({ workspace: longWs, onToggleZoom: vi.fn(), onCloseTile: vi.fn() });
    expect(screen.getByTestId(`tile-zoom-${longWs.id}`)).toBeInTheDocument();
    expect(screen.getByTestId(`tile-hide-${longWs.id}`)).toBeInTheDocument();
  });

  it("the label truncates in the middle (min-w-0 root, ellipsizing head, fixed tail)", () => {
    renderChrome({ workspace: longWs });
    const label = screen.getByTestId(`tile-label-${longWs.id}`);
    expect(label.className).toContain("min-w-0");
    const [head, tail] = Array.from(label.children);
    expect(head.className).toContain("text-ellipsis");
    expect(tail.className).toContain("flex-shrink-0");
    expect(head.textContent! + tail.textContent!).toBe(longWs.storyId);
    expect(tail.textContent).toBe("xxxxxx");
  });

  it("the identity block is allowed to shrink (no flex-shrink-0)", () => {
    renderChrome({ workspace: longWs });
    const identityBlock = screen.getByTestId(`tile-label-${longWs.id}`).parentElement!;
    expect(identityBlock.className).toContain("min-w-0");
    expect(identityBlock.className).not.toContain("flex-shrink-0");
  });
});

describe("TerminalTileChrome — Rename from the tab context menu", () => {
  function openMenuAndRename() {
    fireEvent.contextMenu(screen.getByText("t1"), { clientX: 10, clientY: 10 });
    fireEvent.click(screen.getByRole("menuitem", { name: "Rename" }));
  }
  const editor = () => document.querySelector<HTMLInputElement>("input");

  it("opens the inline editor focused, and Enter commits the typed name", async () => {
    const onRenameTab = vi.fn();
    renderChrome({ onRenameTab });
    openMenuAndRename();
    const input = editor();
    expect(input).not.toBeNull();
    expect(input!.value).toBe("t1");
    await waitFor(() => expect(document.activeElement).toBe(input));

    fireEvent.change(input!, { target: { value: "build" } });
    fireEvent.keyDown(input!, { key: "Enter" });
    expect(onRenameTab).toHaveBeenCalledTimes(1);
    expect(onRenameTab).toHaveBeenCalledWith("t1", "build");
    expect(editor()).toBeNull();
  });

  it("Esc cancels without renaming, and a second Rename re-opens the editor", async () => {
    const onRenameTab = vi.fn();
    renderChrome({ onRenameTab });
    openMenuAndRename();
    const first = editor()!;
    fireEvent.change(first, { target: { value: "discard me" } });
    fireEvent.keyDown(first, { key: "Escape" });
    expect(editor()).toBeNull();
    expect(onRenameTab).not.toHaveBeenCalled();

    openMenuAndRename();
    const second = editor();
    expect(second).not.toBeNull();
    expect(second!.value).toBe("t1");
    await waitFor(() => expect(document.activeElement).toBe(second));
  });
});

describe("TerminalTileChrome — the tab menu from the keyboard", () => {
  const pill = () => screen.getByText("t1").closest("button") as HTMLButtonElement;

  async function openWithShiftF10(overrides: Parameters<typeof renderChrome>[0] = {}) {
    const user = userEvent.setup();
    renderChrome(overrides);
    pill().focus();
    await user.keyboard("{Shift>}{F10}{/Shift}");
    return user;
  }
  /** An item's name: its text without the drawn shortcut (hidden from AT). */
  const nameOf = (el: Element | null) => {
    const copy = el?.cloneNode(true) as HTMLElement | undefined;
    copy?.querySelectorAll("[aria-hidden]").forEach((hint) => hint.remove());
    return copy?.textContent;
  };
  const focusedName = () => nameOf(document.activeElement);

  it("Shift+F10 on a focused pill opens the menu with its first item focused", async () => {
    await openWithShiftF10();
    expect(screen.getByRole("menu")).toBeInTheDocument();
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Rename" }));
  });

  it("the pill announces its menu, and reads as expanded only while it is open", async () => {
    const user = userEvent.setup();
    renderChrome({ tabs: [tab("t1"), tab("t2")] });
    const p1 = () => screen.getByText("t1").closest("button") as HTMLButtonElement;
    const p2 = () => screen.getByText("t2").closest("button") as HTMLButtonElement;
    expect(p1()).toHaveAttribute("aria-haspopup", "menu");
    expect(p1()).toHaveAttribute("aria-expanded", "false");
    p1().focus();
    await user.keyboard("{Shift>}{F10}{/Shift}");
    expect(screen.getByRole("menu", { name: "Tab actions" })).toBeInTheDocument();
    expect(p1()).toHaveAttribute("aria-expanded", "true");
    expect(p2()).toHaveAttribute("aria-expanded", "false");
    await user.keyboard("{Escape}");
    expect(p1()).toHaveAttribute("aria-expanded", "false");
  });

  it("the arrows walk every item, with wrap and Home/End", async () => {
    const user = await openWithShiftF10();
    const names = screen.getAllByRole("menuitem").map(nameOf);
    expect(names).toEqual(["Rename", "New terminal", "Hide", "End", "End Others", "End All"]);
    for (const name of names.slice(1)) {
      await user.keyboard("{ArrowDown}");
      expect(focusedName()).toBe(name);
    }
    await user.keyboard("{ArrowDown}");
    expect(focusedName()).toBe("Rename");
    await user.keyboard("{ArrowUp}");
    expect(focusedName()).toBe("End All");
    await user.keyboard("{Home}");
    expect(focusedName()).toBe("Rename");
    await user.keyboard("{End}");
    expect(focusedName()).toBe("End All");
  });

  it("Esc closes the menu and puts focus back on the tab pill", async () => {
    const user = await openWithShiftF10();
    await user.keyboard("{ArrowDown}{Escape}");
    expect(screen.queryByRole("menu")).toBeNull();
    expect(document.activeElement).toBe(pill());
  });

  it("Enter on Rename opens the inline editor, focused — the whole rename without a mouse", async () => {
    const onRenameTab = vi.fn();
    const user = await openWithShiftF10({ onRenameTab });
    await user.keyboard("{Enter}");
    expect(screen.queryByRole("menu")).toBeNull();
    const input = document.querySelector<HTMLInputElement>("input")!;
    await waitFor(() => expect(document.activeElement).toBe(input));
    await user.keyboard("{Control>}a{/Control}build{Enter}");
    expect(onRenameTab).toHaveBeenCalledWith("t1", "build");
  });

  it("Space on End ends the invoking tab, once", async () => {
    const onEndTab = vi.fn();
    const user = await openWithShiftF10({ onEndTab });
    await user.keyboard("{ArrowDown}{ArrowDown}{ArrowDown}[Space]");
    expect(onEndTab).toHaveBeenCalledTimes(1);
    expect(onEndTab).toHaveBeenCalledWith("t1");
    expect(screen.queryByRole("menu")).toBeNull();
  });
});

describe("TerminalTileChrome — the machine a remote workspace lives on", () => {
  const box = {
    machineId: "box-1",
    kind: "ssh",
    label: "codesense-test-1 (private)",
    scope: { kind: "personal" },
    declaration: null,
    binding: null,
    canPublish: true,
    status: { state: "connected", workspaces: null },
    attention: 0,
    capabilities: { agentCallbacks: true },
  } as const;

  it("shows the machine's mark with its name in words, not 'on <machine>' text", () => {
    useMachinesStore.setState({ byTeam: { t: [box as unknown as MachineSummary] } });
    renderChrome({
      workspace: { ...ws, location: { kind: "machine", machineId: "box-1", requestId: "r" } },
    });
    const mark = screen.getByTestId(`tile-machine-${ws.id}`);
    expect(mark.getAttribute("role")).toBe("img");
    expect(mark.getAttribute("aria-label")).toBe("on codesense-test-1 (private)");
    expect(mark.querySelector("svg")).not.toBeNull();
    expect(screen.queryByText(/on codesense-test-1/)).toBeNull();
    useMachinesStore.setState({ byTeam: {} });
  });

  it("shows nothing for a workspace on This Mac", () => {
    renderChrome();
    expect(screen.queryByTestId(`tile-machine-${ws.id}`)).toBeNull();
  });
});

beforeEach(() => resetSessionProjection());
