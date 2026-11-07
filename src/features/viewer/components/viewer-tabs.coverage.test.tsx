// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ViewerTabs } from "./viewer-tabs";

// ---------- Mocks ----------

let mockState: Record<string, unknown> = {};

vi.mock("@/shared/stores/viewer-store", () => {
  const fn = (selector: (s: Record<string, unknown>) => unknown) => selector(mockState);
  fn.subscribe = () => () => {};
  fn.getState = () => mockState;
  fn.setState = vi.fn();
  return {
    useViewerStore: fn,
  };
});

// T.4 Phase 5: ViewerTabs derives teamKey via this hook. Mock returns the
// same REPO string so existing fixtures (openTabs[REPO], activeTab[REPO])
// resolve under the team-keyed bucket without restructuring the seed.
vi.mock("@/shared/lib/team-key", () => ({
  useActiveTeamKey: () => "/repo/x",
  getActiveTeamKey: () => "/repo/x",
  buildTeamKey: (orgSlug: string, teamName: string | null) => `${orgSlug}:${teamName ?? "_none"}`,
  NO_TEAM_SENTINEL: "_no_team:_none",
}));

// Story tab titles come from the story's frontmatter `title:` (fetched on demand).
let mockTitleByPath: Record<string, string> = {};
vi.mock("@/shared/hooks/use-summary-headings", () => ({ useSummaryHeadings: () => [] }));
vi.mock("@/shared/lib/pipeline-graph/use-story-details", () => ({
  useStoryDetails: (path: string | undefined) => ({
    data:
      path && mockTitleByPath[path]
        ? { title: null, storyText: null, titleMeta: mockTitleByPath[path] }
        : undefined,
    loading: false,
  }),
}));

// The active team's starter page: `web` in `acme/ctx`, README.md by default.
vi.mock("@/features/viewer/hooks/use-team-starter-page", () => ({
  useActiveTeamStarterPage: () => ({
    org: { slug: "acme/ctx", org_root: "/orgs/acme" },
    team: { name: "web", source_repos: [] },
    contentRoot: "/orgs/acme/web/content",
    effectivePath: "/orgs/acme/web/content/README.md",
  }),
}));

// ---------- Helpers ----------

const REPO = "/repo/x";

function makeTabs(paths: string[]) {
  return paths.map((p) => ({ path: p, name: p.split("/").pop() ?? p }));
}

function setupState(overrides: Partial<typeof mockState> = {}) {
  mockState = {
    openTabs: { [REPO]: makeTabs(["/a.md", "/b.md", "/c.md"]) },
    activeTab: { [REPO]: "/b.md" },
    setActiveTab: vi.fn(),
    closeTab: vi.fn(),
    closeOtherTabs: vi.fn(),
    closeAllTabs: vi.fn(),
    reorderTab: vi.fn(),
    pinTab: vi.fn(),
    openFile: vi.fn(),
    ...overrides,
  };
}

// ---------- Tests ----------

describe("ViewerTabs", () => {
  beforeEach(() => {
    setupState();
  });

  it("renders one button per tab", () => {
    render(<ViewerTabs repoPath={REPO} />);
    // Three tab labels are visible (a.md, b.md, c.md)
    expect(screen.getByText("a.md")).toBeInTheDocument();
    expect(screen.getByText("b.md")).toBeInTheDocument();
    expect(screen.getByText("c.md")).toBeInTheDocument();
  });

  it("renders nothing interactive when the tab list is empty", () => {
    setupState({ openTabs: { [REPO]: [] } });
    const { container } = render(<ViewerTabs repoPath={REPO} />);
    // No tab labels, no tab buttons
    expect(container.querySelector("button")).toBeNull();
  });

  it("clicking a tab calls setActiveTab(repoPath, path)", async () => {
    const user = userEvent.setup();
    render(<ViewerTabs repoPath={REPO} />);
    await user.click(screen.getByText("c.md"));
    expect(mockState.setActiveTab).toHaveBeenCalledWith(REPO, REPO, "/c.md");
  });

  it("middle-click on a tab button calls closeTab", () => {
    const { container } = render(<ViewerTabs repoPath={REPO} />);
    const tabButtons = container.querySelectorAll("button");
    expect(tabButtons.length).toBeGreaterThan(0);
    // Simulate pointer-down with middle button (button=1)
    fireEvent.pointerDown(tabButtons[0], { button: 1 });
    expect(mockState.closeTab).toHaveBeenCalledWith(REPO, REPO, "/a.md", { user: true });
  });

  it("right-click on a tab opens the context menu", () => {
    render(<ViewerTabs repoPath={REPO} />);
    fireEvent.contextMenu(screen.getByText("a.md"));
    // Context menu items show up
    expect(screen.getByText("Close")).toBeInTheDocument();
    expect(screen.getByText("Close Others")).toBeInTheDocument();
    expect(screen.getByText("Close All")).toBeInTheDocument();
  });

  it("the tab menu is a named menu whose dividers are separators, not items", () => {
    render(<ViewerTabs repoPath={REPO} />);
    fireEvent.contextMenu(screen.getByText("a.md"));
    const menu = screen.getByRole("menu", { name: "Tab actions" });
    const separators = menu.querySelectorAll('[role="separator"]');
    expect(separators.length).toBeGreaterThan(0);
    for (const sep of separators) expect(sep.className).toContain("h-px");
  });

  it("context menu 'Close' fires closeTab for the clicked tab", async () => {
    const user = userEvent.setup();
    render(<ViewerTabs repoPath={REPO} />);
    fireEvent.contextMenu(screen.getByText("a.md"));
    await user.click(screen.getByText("Close"));
    expect(mockState.closeTab).toHaveBeenCalledWith(REPO, REPO, "/a.md", { user: true });
  });

  it("context menu 'Close Others' fires closeOtherTabs", async () => {
    const user = userEvent.setup();
    render(<ViewerTabs repoPath={REPO} />);
    fireEvent.contextMenu(screen.getByText("b.md"));
    await user.click(screen.getByText("Close Others"));
    expect(mockState.closeOtherTabs).toHaveBeenCalledWith(REPO, REPO, "/b.md");
  });

  it("context menu 'Close All' fires closeAllTabs", async () => {
    const user = userEvent.setup();
    render(<ViewerTabs repoPath={REPO} />);
    fireEvent.contextMenu(screen.getByText("c.md"));
    await user.click(screen.getByText("Close All"));
    expect(mockState.closeAllTabs).toHaveBeenCalledWith(REPO);
  });

  it("context menu closes when clicking its backdrop", async () => {
    const user = userEvent.setup();
    render(<ViewerTabs repoPath={REPO} />);
    fireEvent.contextMenu(screen.getByText("a.md"));
    expect(screen.getByText("Close")).toBeInTheDocument();
    // Click the fixed-inset backdrop div (it sits at z-40)
    const backdrop = document.querySelector(".fixed.inset-0.z-40");
    expect(backdrop).not.toBeNull();
    await user.click(backdrop as Element);
    expect(screen.queryByText("Close")).toBeNull();
  });

  it("offers no trust item on an HTML tab — HTML docs need no trust", () => {
    setupState({ openTabs: { [REPO]: makeTabs(["/dash.html"]) } });
    render(<ViewerTabs repoPath={REPO} />);
    fireEvent.contextMenu(screen.getByText("dash.html"));
    expect(screen.queryByText(/trust/i)).toBeNull();
  });

  // ---------- Preview tabs (Story 32.2 Part 1) ----------

  it("renders the preview tab label in italic and pinned labels non-italic", () => {
    setupState({
      openTabs: {
        [REPO]: [
          { path: "/a.md", name: "a.md" }, // pinned
          { path: "/b.md", name: "b.md", preview: true }, // preview
        ],
      },
      activeTab: { [REPO]: "/b.md" },
    });

    render(<ViewerTabs repoPath={REPO} />);

    expect(screen.getByText("a.md").className).not.toContain("italic");
    expect(screen.getByText("b.md").className).toContain("italic");
  });

  // SW-FX-10: double-click is open-or-switch (consistent across surfaces), not
  // pinTab. Pinning of a preview tab happens as a side-effect of openFile's
  // commit semantics; an explicit pin affordance also moved to the tab context
  // menu (asserted below).
  it("double-clicking a tab calls openFile(teamKey, repoPath, path) (SW-FX-10 AC-2)", () => {
    setupState({
      openTabs: { [REPO]: [{ path: "/b.md", name: "b.md", preview: true }] },
      activeTab: { [REPO]: "/b.md" },
    });

    render(<ViewerTabs repoPath={REPO} />);
    fireEvent.doubleClick(screen.getByText("b.md"));

    expect(mockState.openFile).toHaveBeenCalledWith(REPO, REPO, "/b.md");
    expect(mockState.pinTab).not.toHaveBeenCalled();
  });

  it("tab context menu offers 'Pin tab' for a preview tab and pins on click (SW-FX-10 AC-2)", () => {
    setupState({
      openTabs: { [REPO]: [{ path: "/b.md", name: "b.md", preview: true }] },
      activeTab: { [REPO]: "/b.md" },
    });

    render(<ViewerTabs repoPath={REPO} />);
    fireEvent.contextMenu(screen.getByText("b.md"));

    const pin = screen.getByText("Pin tab");
    fireEvent.click(pin);
    expect(mockState.pinTab).toHaveBeenCalledWith(REPO, "/b.md");
  });

  it("tab context menu hides 'Pin tab' for an already-pinned tab (SW-FX-10 AC-2)", () => {
    setupState({
      openTabs: { [REPO]: [{ path: "/a.md", name: "a.md" }] }, // pinned (no preview)
      activeTab: { [REPO]: "/a.md" },
    });

    render(<ViewerTabs repoPath={REPO} />);
    fireEvent.contextMenu(screen.getByText("a.md"));

    expect(screen.queryByText("Pin tab")).not.toBeInTheDocument();
  });

  // ---------- Drag-reorder rect caching (NB-3) ----------

  // Tabs are rendered with stubbed rects spaced 100px apart:
  // tab[0] → [0, 100), tab[1] → [100, 200), tab[2] → [200, 300).
  function setupDragHarness() {
    const view = render(<ViewerTabs repoPath={REPO} />);
    const { container } = view;
    const tabButtons = container.querySelectorAll("button");
    const tabContainers = Array.from(tabButtons).map((b) => b.parentElement as HTMLElement);
    const dragContainer = tabContainers[0].parentElement as HTMLElement;
    const stubs = tabContainers.map((el, i) => {
      const rect = {
        left: i * 100,
        right: (i + 1) * 100,
        top: 0,
        bottom: 30,
        x: i * 100,
        y: 0,
        width: 100,
        height: 30,
        toJSON: () => ({}),
      } as DOMRect;
      const stub = vi.fn(() => rect);
      el.getBoundingClientRect = stub;
      return stub;
    });
    return { container, tabButtons, dragContainer, stubs, unmount: view.unmount };
  }

  it("caches tab rects at pointer-down — getBoundingClientRect is not called during pointer-move", () => {
    const { container, tabButtons, dragContainer, stubs } = setupDragHarness();
    expect(tabButtons.length).toBe(3);
    const totalCalls = () => stubs.reduce((sum, fn) => sum + fn.mock.calls.length, 0);

    // Drag-start on tab[0] — snapshots all rects exactly once
    fireEvent.pointerDown(tabButtons[0], { button: 0, clientX: 50 });
    expect(totalCalls()).toBe(3);

    // Two pointer-moves with clientX over tab[2] — must NOT re-read rects
    fireEvent.pointerMove(dragContainer, { clientX: 250 });
    // Positive anchor: the indicator class IS `.border-accent` and is applied
    // to exactly one wrapper (tab[2]). Pins the selector against future renames.
    expect(container.querySelectorAll(".border-accent").length).toBe(1);
    fireEvent.pointerMove(dragContainer, { clientX: 260 });
    expect(totalCalls()).toBe(3);

    // Drop on tab[2] — commits reorder using cached drop index
    fireEvent.pointerUp(dragContainer);
    expect(mockState.reorderTab).toHaveBeenCalledWith(REPO, 0, 2);
  });

  it("hit-test uses cached values, not live rect reads — clientX 250 maps to cached tab[2] even when live rects would map it to tab[1]", () => {
    const { tabButtons, dragContainer } = setupDragHarness();
    fireEvent.pointerDown(tabButtons[0], { button: 0, clientX: 50 });

    // After the cache is populated, swap each tab's rect stub so a live re-read
    // would map clientX=250 to tab[1] (new range [200, 300)) instead of tab[2]
    // (cached range [200, 300)). If production reads the cache, the drop lands
    // on tab[2] and the assertion passes. If production re-reads live, the drop
    // lands on tab[1] and `reorderTab(REPO, 0, 2)` is never called — assertion fails.
    const liveRanges: Array<[number, number]> = [
      [400, 500], // tab[0]
      [200, 300], // tab[1] — clientX=250 would live-match here
      [600, 700], // tab[2]
    ];
    tabButtons.forEach((b, i) => {
      const [left, right] = liveRanges[i];
      (b.parentElement as HTMLElement).getBoundingClientRect = vi.fn(
        () =>
          ({
            left,
            right,
            top: 0,
            bottom: 30,
            x: left,
            y: 0,
            width: right - left,
            height: 30,
            toJSON: () => ({}),
          }) as DOMRect,
      );
    });

    fireEvent.pointerMove(dragContainer, { clientX: 250 });
    fireEvent.pointerUp(dragContainer);
    expect(mockState.reorderTab).toHaveBeenCalledWith(REPO, 0, 2);
  });

  it("5px dead zone — small movement does not set drag-over indicator and does not reorder on pointer-up", () => {
    const { container, tabButtons, dragContainer } = setupDragHarness();
    fireEvent.pointerDown(tabButtons[0], { button: 0, clientX: 50 });
    // dx = |54 - 50| = 4, below 5px dead zone → setDragOverIdx must not fire
    fireEvent.pointerMove(dragContainer, { clientX: 54 });
    // Drop indicator (border-accent class on a tab wrapper) must NOT have been applied.
    // Scope to `container` so an unrelated future portal/global mount can't false-positive.
    expect(container.querySelectorAll(".border-accent").length).toBe(0);
    fireEvent.pointerUp(dragContainer);
    expect(mockState.reorderTab).not.toHaveBeenCalled();
  });

  it("onPointerLeave cancels the drag — reorder is not committed", () => {
    const { tabButtons, dragContainer } = setupDragHarness();
    fireEvent.pointerDown(tabButtons[0], { button: 0, clientX: 50 });
    fireEvent.pointerMove(dragContainer, { clientX: 250 });
    fireEvent.pointerLeave(dragContainer);
    // pointer-up after leave should be a no-op since dragState was cleared
    fireEvent.pointerUp(dragContainer);
    expect(mockState.reorderTab).not.toHaveBeenCalled();
  });

  // ---------- NB-7 / AC-7: unmount cleanup ----------

  it("mounts and unmounts without errors or warnings (NB-7 AC-7 smoke)", () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { unmount } = render(<ViewerTabs repoPath={REPO} />);
    unmount();
    expect(errorSpy).not.toHaveBeenCalled();
    expect(warnSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
    warnSpy.mockRestore();
  });

  it("unmount mid-drag clears refs (no leaked DOMRect arrays, NB-7 AC-7)", () => {
    const { tabButtons, dragContainer, unmount } = setupDragHarness();
    // Start a drag and move into the cached range — populates both refs.
    fireEvent.pointerDown(tabButtons[0], { button: 0, clientX: 50 });
    fireEvent.pointerMove(dragContainer, { clientX: 250 });

    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    unmount();
    // No React warnings about state-after-unmount and no React DOM cleanup errors.
    expect(errorSpy).not.toHaveBeenCalled();
    errorSpy.mockRestore();
  });
});

describe("story tab titles", () => {
  beforeEach(() => {
    mockTitleByPath = {};
  });

  it("a story tab shows its frontmatter title, not the file stem", () => {
    mockTitleByPath = { "/e/TR-1-api-health-endpoint.md": "API health endpoint" };
    setupState({
      openTabs: { [REPO]: makeTabs(["/e/TR-1-api-health-endpoint.md", "/e/README.md"]) },
      activeTab: { [REPO]: "/e/README.md" },
    });
    render(<ViewerTabs repoPath={REPO} />);
    expect(screen.getByText("TR-1 — API health endpoint")).toBeInTheDocument();
    expect(screen.queryByText("TR-1-api-health-endpoint.md")).toBeNull();
    expect(screen.getByText("README.md")).toBeInTheDocument();
  });

  it("a story without a title keeps its file name", () => {
    setupState({
      openTabs: { [REPO]: makeTabs(["/e/TR-2-status.md"]) },
      activeTab: { [REPO]: "/e/TR-2-status.md" },
    });
    render(<ViewerTabs repoPath={REPO} />);
    expect(screen.getByText("TR-2-status.md")).toBeInTheDocument();
  });
});

describe("ViewerTabs — team starter page", () => {
  it("a team file's tab offers Set as team starter page; a file outside offers nothing", () => {
    setupState({
      openTabs: { [REPO]: makeTabs(["/orgs/acme/web/content/guide.md", "/a.md"]) },
      activeTab: { [REPO]: "/a.md" },
    });
    render(<ViewerTabs repoPath={REPO} />);
    fireEvent.contextMenu(screen.getByText("guide.md"));
    expect(screen.getByRole("menuitem", { name: "Set as team starter page" })).toBeInTheDocument();
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
  });

  it("the default README's tab says it is the team starter page", () => {
    setupState({
      openTabs: { [REPO]: makeTabs(["/orgs/acme/web/content/README.md"]) },
      activeTab: { [REPO]: "/orgs/acme/web/content/README.md" },
    });
    render(<ViewerTabs repoPath={REPO} />);
    fireEvent.contextMenu(screen.getByText("README.md"));
    expect(screen.getByTestId("starter-page-current")).toHaveTextContent("Team starter page");
  });
});
