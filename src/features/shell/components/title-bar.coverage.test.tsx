// SPDX-License-Identifier: MIT
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { screen } from "@testing-library/react";
import { renderWithProviders as render } from "../../../../tests/support/helpers/render-with-providers";
import userEvent from "@testing-library/user-event";

import { TitleBar } from "./title-bar";
import { useViewerStore } from "@/shared/stores/viewer-store";

// ---------- Mocks ----------

// SW-UX: the Machines menu row opens the virtual tab via this free function.
const { mockOpenMachines } = vi.hoisted(() => ({ mockOpenMachines: vi.fn() }));
vi.mock("../machines-virtual-tab", () => ({
  openMachines: mockOpenMachines,
}));

// SW-OC-B7: the Settings menu row opens the Settings tab via this free function.
const { mockOpenAppSettings } = vi.hoisted(() => ({ mockOpenAppSettings: vi.fn() }));
vi.mock("@/features/settings/lib/open-settings", () => ({
  openAppSettings: mockOpenAppSettings,
}));

// T.4 AC-14: TitleBar reads the team's tabs via `useActiveTeamKey`. Mock the
// hook to return the same string the test seeds those tabs under so the
// existing fixtures (keyed by "/r") keep matching without an org-store
// dance.
vi.mock("@/shared/lib/team-key", () => ({
  useActiveTeamKey: () => "/r",
  getActiveTeamKey: () => "/r",
  buildTeamKey: (orgSlug: string, teamName: string | null) => `${orgSlug}:${teamName ?? "_none"}`,
  parseTeamKey: (key: string) => {
    const idx = key.indexOf(":");
    if (idx === -1) return { orgSlug: key, teamName: null };
    const orgSlug = key.slice(0, idx);
    const tail = key.slice(idx + 1);
    return { orgSlug, teamName: tail === "_none" ? null : tail };
  },
  // T.2: viewer-store transitively imports `buildRootWorkspaceId` and
  // `firstRootIdForTeam` from team-key (post-rebase onto main).
  buildRootWorkspaceId: (slug: string) => `root:${slug}`,
  firstRootIdForTeam: () => null,
  // SW-AU-UI-1: TitleBar mounts WorkspaceModeControls, which gates on
  // `teamKeyHasTeam`. The "/r" sentinel above parses to a null team (no colon),
  // so the control self-renders nothing — this suite asserts the surrounding
  // chrome, not the mode controls (covered in workspace-mode-controls.test.tsx).
  teamKeyHasTeam: (key: string) => {
    // This suite uses /r as its opened-team fixture throughout.
    if (key === "/r") return true;
    const idx = key.indexOf(":");
    if (idx === -1) return false;
    return key.slice(idx + 1) !== "_none";
  },
  activeWorkspaceIdForTeam: (
    teamKey: string,
    activeWorkspaceByTeam: Record<string, string>,
    workspacesByTeam: Record<string, Array<{ id: string; kind: string }>>,
  ) =>
    activeWorkspaceByTeam?.[teamKey] ??
    (workspacesByTeam?.[teamKey] ?? []).find((w) => w.kind === "root")?.id ??
    null,
  NO_TEAM_SENTINEL: "_no_team:_none",
}));

// T.6: title-bar reads `repoPath` from `useActiveWorkspaceContext` — the
// synthetic workspace key. The real hook derives it via
// `buildSyntheticRepoPathFromPrompt(teamKey + sourceRepoSlug)`, which
// requires a parseable teamKey shape (`org:team`). The test fixture
// mocks `useActiveTeamKey` to return "/r" (which doesn't parse), so
// we mock the hook directly to return the same "/r" sentinel that
// existing fixtures + assertions are keyed on. Reads viewer-store
// directly via vi.hoisted so the workspace presence + activeWorkspace
// resolve at call time (matches the real hook's reactivity surface
// closely enough for the slot-render assertions in this file).
const { viewerStoreRef } = vi.hoisted(() => ({
  viewerStoreRef: { store: null as null | { getState: () => unknown } },
}));
vi.mock("@/shared/hooks/use-active-workspace-context", async () => {
  const viewer = await import("@/shared/stores/viewer-store");
  viewerStoreRef.store = viewer.useViewerStore;
  function snapshot() {
    const state = viewer.useViewerStore.getState() as unknown as {
      workspaces: Record<string, Array<{ id: string; sourceRepoSlug?: string | null }>>;
      activeWorkspace: Record<string, string>;
    };
    const list = state.workspaces?.["/r"];
    const activeId = state.activeWorkspace?.["/r"];
    const workspace = Array.isArray(list) ? (list.find((w) => w.id === activeId) ?? null) : null;
    const sourceRepoSlug = workspace?.sourceRepoSlug ?? null;
    return {
      teamKey: "/r",
      workspace,
      sourceRepoSlug,
      // SLW-FX-11: mirror production `deriveRepoPath` — the workspace-derived
      // synthetic `repoPath` is null for a slug-less workspace (a root
      // workspace with no source repo, or an epic integration workspace). Only
      // a workspace carrying a `sourceRepoSlug` yields a non-null contextRepoPath.
      repoPath: sourceRepoSlug ? "/r" : null,
    };
  }
  return {
    useActiveWorkspaceContext: () => snapshot(),
    getActiveWorkspaceContext: () => snapshot(),
  };
});

// SW-EW-24: the viewer-tab gate now keys off `useTeamPipelineRepoPath`
// (own repoPath, or the team's first repo for an epic shell) instead of the
// identity `workspaceRepoPath`. The team-key fixture here uses the
// non-parseable "/r" sentinel, so — mirroring the `useActiveWorkspaceContext`
// mock above — stub the hook to a controllable value and assert TitleBar's own
// gate logic. The hook's own resolution is covered in
// use-team-pipeline-repo-path.test.ts.
let mockTabRepoPath = "/r";
vi.mock("@/shared/hooks/use-team-pipeline-repo-path", () => ({
  useTeamPipelineRepoPath: () => mockTabRepoPath,
}));

const mockStartDragging = vi.fn();
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({ startDragging: mockStartDragging }),
}));

// Stub the viewer-tabs slot so this suite focuses on TitleBar's own slot
// logic rather than tab internals (covered in viewer-tabs coverage tests).
// SW-EW-34: the title bar no longer hosts terminal tabs (ConsoleTabs is
// retired) — the only tab slot is ViewerTabs, in every layout mode.
vi.mock("@/features/viewer/components/viewer-tabs", () => ({
  ViewerTabs: ({ repoPath }: { repoPath: string }) => (
    <div data-testid="stub-viewer-tabs" data-repo-path={repoPath} />
  ),
}));

// ---------- Helpers ----------

// T.6: title-bar gates its middle-slot doc tabs on
// `useActiveWorkspaceContext().workspace !== null`. The team-key mock above
// forces `useActiveTeamKey()` to "/r", so tests can seed a root workspace
// under that key and the hook will pick it up.
function seedActiveWorkspace() {
  useViewerStore.setState({
    openedTeams: ["/r"],
    workspaces: {
      "/r": [{ id: "w1", kind: "root", cwd: "/r", sourceRepoPath: "/r", status: "ready" }],
    },
    activeWorkspace: { "/r": "w1" },
  });
}

// ---------- Tests ----------

describe("TitleBar", () => {
  it("SW-AGR-19: shows the out-of-date terminal servers notice for the active team and opens Machines", async () => {
    const { useMachinesStore } = await import("@/features/machines/stores/machines-store");
    const { mockTerminalServer } = await import("@/app/mock/handlers/machines");
    useViewerStore.setState({ dismissedTerminalServers: [] });
    useMachinesStore.setState({
      byTeam: {
        "/r": [
          {
            machineId: "box",
            kind: "ssh",
            label: "box",
            scope: { kind: "personal" },
            declaration: null,
            binding: null,
            canPublish: true,
            status: { state: "connected", workspaces: 1 },
            attention: 0,
            capabilities: { agentCallbacks: true },
            terminalServer: mockTerminalServer(false, {
              serverVersion: "0.0.21",
              upToDate: false,
              relation: "older",
            }),
          },
        ],
      },
    });
    render(<TitleBar />);
    const notice = screen.getByTestId("titlebar-terminal-servers-out-of-date");
    expect(notice).toHaveAccessibleName("1 terminal server out of date — open Machines");
    await userEvent.click(notice);
    try {
      await vi.waitFor(() => expect(mockOpenMachines).toHaveBeenCalled());
    } finally {
      useMachinesStore.setState({ byTeam: {} });
    }
  });

  beforeEach(() => {
    mockTabRepoPath = "/r";
  });

  describe("render structure", () => {
    it("renders a drag-region container with the titlebar-drag class and h-10 height", () => {
      const { container } = render(<TitleBar />);
      const bar = container.querySelector("[data-testid='title-bar']");
      expect(bar).not.toBeNull();
      expect(bar).toHaveClass("titlebar-drag", "flex-shrink-0");
    });

    // Traffic lights → identity menu → middle lane → optional CLI controls.
    it("holds only the identity menu outside the middle lane", () => {
      render(<TitleBar />);
      const middle = screen.getByTestId("title-bar-middle");
      const buttons = screen
        .getAllByRole("button")
        .filter((b) => !middle.contains(b))
        .map((b) => b.dataset.testid);
      expect(buttons).toEqual(["identity-menu-trigger"]);
      const trigger = screen.getByTestId("identity-menu-trigger");
      expect(
        trigger.compareDocumentPosition(middle) & Node.DOCUMENT_POSITION_FOLLOWING,
      ).toBeTruthy();
      expect(screen.queryByTestId("inbox-nav-button")).not.toBeInTheDocument();
    });

    it("the retired destination buttons are gone from the bar", () => {
      render(<TitleBar />);
      for (const name of [
        "Private",
        "Routines",
        "Agents",
        "Open Agent Accounts",
        "Open Machines",
        "Open settings",
        "Open Connections",
      ]) {
        expect(screen.queryByRole("button", { name })).toBeNull();
      }
      expect(screen.queryByRole("button", { name: /^Account(:|$)/ })).toBeNull();
    });
  });

  // SW-OC-B7 → SW-OC-28: Settings and Machines stay one click away — visible rows in the
  // identity menu, each with the same opener as before. Agent accounts have no row yet
  // (SV-ENG-78).
  describe("destinations in the identity menu", () => {
    it("Settings opens via openAppSettings", async () => {
      const user = userEvent.setup();
      mockOpenAppSettings.mockClear();
      render(<TitleBar />);
      await user.click(screen.getByTestId("identity-menu-trigger"));
      await user.click(await screen.findByRole("menuitem", { name: /^Settings/ }));
      expect(mockOpenAppSettings).toHaveBeenCalledWith("general");
    });

    it("Machines opens the unified Machines surface", async () => {
      const user = userEvent.setup();
      mockOpenMachines.mockClear();
      render(<TitleBar />);
      await user.click(screen.getByTestId("identity-menu-trigger"));
      await user.click(await screen.findByRole("menuitem", { name: /^Machines/ }));
      expect(mockOpenMachines).toHaveBeenCalled();
    });
  });

  describe("SW-CP-30 (AC-6): Home icon removed from the top chrome", () => {
    it("does NOT render a Home button in the title bar", () => {
      // The top chrome keeps only traffic-light clearance + doc tabs.
      render(<TitleBar />);
      expect(screen.queryByTestId("title-bar-home")).not.toBeInTheDocument();
    });
  });

  describe("SW-FX-89 (AC-2): global sync-wave indicator removed from the top chrome", () => {
    it("does NOT render the sync-wave indicator — sync now lives on the workspace pills", () => {
      // The variable-width indicator label previously sat in the right-controls
      // cluster and could shrink the flex-1 doc-tab region on activity. It is gone;
      // an explicit operation surfaces on its workspace pill's operation chip instead,
      // so no variable-width sync chrome remains in this row to reflow the tabs.
      render(<TitleBar />);
      expect(screen.queryByTestId("sync-wave-indicator")).not.toBeInTheDocument();
    });
  });

  describe("traffic-light drag guard (F-21)", () => {
    beforeEach(() => {
      mockStartDragging.mockClear();
    });

    it("does NOT call startDragging when mousedown lands in the traffic-light zone (clientX < 78)", () => {
      const { container } = render(<TitleBar />);
      const row = container.querySelector("[data-testid='title-bar'] > div") as HTMLElement;
      row.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, clientX: 40, button: 0 }));
      expect(mockStartDragging).not.toHaveBeenCalled();
    });

    it("calls startDragging when mousedown lands outside the traffic-light zone (clientX >= 78)", () => {
      const { container } = render(<TitleBar />);
      const row = container.querySelector("[data-testid='title-bar'] > div") as HTMLElement;
      row.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, clientX: 200, button: 0 }));
      expect(mockStartDragging).toHaveBeenCalled();
    });
  });

  describe("middle slot (tabs)", () => {
    // Snapshot + restore viewer-store state around each case so we don't
    // leak into neighboring tests in this suite (the store is real here;
    // only the tab components themselves are stubbed).
    let viewerSnapshot: ReturnType<typeof useViewerStore.getState>;

    beforeEach(() => {
      viewerSnapshot = useViewerStore.getState();
    });
    afterEach(() => {
      useViewerStore.setState(viewerSnapshot);
    });

    it("renders no doc tabs when no workspace is active", () => {
      // T.6: gate flipped from `viewMode === viewer` to
      // `useActiveWorkspaceContext().workspace !== null`. With no
      // workspace seeded under team key "/r", the tab slot stays closed.
      useViewerStore.setState({ workspaces: {}, activeWorkspace: {} });
      render(<TitleBar />);
      expect(screen.queryByTestId("stub-viewer-tabs")).toBeNull();
    });

    it("renders ViewerTabs for a repo-less team (no workspace) when doc tabs are open", () => {
      // Regression (no-tabs-for-context-files): a team with no attached repos has
      // `workspace === null` (synthetic root:_no_repos), but the user can still open
      // context-repo files, which create doc tabs. The strip must mount so those tabs
      // are visible — the prior `workspace !== null` gate hid a NON-empty strip here.
      useViewerStore.setState({
        workspaces: {},
        activeWorkspace: {},
        openTabs: { "/r": [{ path: "a.md", name: "a.md", preview: true }] },
      });
      render(<TitleBar />);
      expect(screen.getByTestId("stub-viewer-tabs")).toBeInTheDocument();
    });

    it("renders ViewerTabs when active workspace + layoutMode='shared'", () => {
      seedActiveWorkspace();
      useViewerStore.setState({ layoutMode: "shared" });
      render(<TitleBar />);
      const tabs = screen.getByTestId("stub-viewer-tabs");
      expect(tabs).toHaveAttribute("data-repo-path", "/r");
    });

    it("renders ViewerTabs when layoutMode defaults to 'shared'", () => {
      seedActiveWorkspace();
      useViewerStore.setState({ layoutMode: null });
      render(<TitleBar />);
      expect(screen.getByTestId("stub-viewer-tabs")).toBeInTheDocument();
    });

    it("renders ViewerTabs when layoutMode='viewer' (non-console)", () => {
      seedActiveWorkspace();
      useViewerStore.setState({ layoutMode: "viewer" });
      render(<TitleBar />);
      expect(screen.getByTestId("stub-viewer-tabs")).toBeInTheDocument();
    });

    // SW-EW-34 v2: console (full-screen CLI) is its own immersive terminal mode —
    // ALL markdown-viewer chrome is hidden (no doc tabs), and the title-bar middle
    // hosts the workspace-switcher SLOT instead (the single WorkspaceTray is
    // portaled in by the terminal view).
    it("hides ViewerTabs and shows the workspace-switcher slot when layoutMode='console'", () => {
      seedActiveWorkspace();
      useViewerStore.setState({ layoutMode: "console" });
      render(<TitleBar />);
      expect(screen.queryByTestId("stub-viewer-tabs")).toBeNull();
      expect(screen.getByTestId("console-switcher-slot")).toBeInTheDocument();
      // No terminal-tab affordance in the title bar either.
      expect(screen.queryByRole("button", { name: "New terminal" })).toBeNull();
    });

    // SW-EW-34 v2 AC-3: in console the CLI controls + the always-visible
    // full-screen EXIT toggle render in the title-bar right cluster (the
    // structural defense against the no-visible-exit trap).
    it("renders the CLI controls + full-screen EXIT toggle in the right cluster in console", () => {
      seedActiveWorkspace();
      useViewerStore.setState({
        layoutMode: "console",
        terminalVisible: true,
      });
      render(<TitleBar />);
      expect(screen.getByTestId("cli-bottom-chrome")).toBeInTheDocument();
      expect(screen.getByTestId("cli-chrome-console-toggle")).toBeInTheDocument();
    });

    it("does NOT render the CLI controls in the title bar in shared mode", () => {
      seedActiveWorkspace();
      useViewerStore.setState({
        layoutMode: "shared",
        terminalVisible: true,
      });
      render(<TitleBar />);
      expect(screen.queryByTestId("cli-bottom-chrome")).toBeNull();
    });

    // SW-EW-24 AC-2: an epic integration workspace has NO identity repoPath
    // (no sourceRepoSlug), but the tab gate now keys off the team pipeline
    // repoPath, so the team-keyed viewer tabs render with the fallback path.
    it("renders ViewerTabs for an epic integration workspace via the team pipeline repoPath", () => {
      useViewerStore.setState({
        workspaces: {
          "/r": [{ id: "epic1", kind: "epic", cwd: "/r", epicDirPath: "SW-EW", status: "ready" }],
        },
        activeWorkspace: { "/r": "epic1" },
        layoutMode: "shared",
      });
      mockTabRepoPath = "workspace:acme:default:acme/codesense";
      render(<TitleBar />);
      expect(screen.getByTestId("stub-viewer-tabs")).toHaveAttribute(
        "data-repo-path",
        "workspace:acme:default:acme/codesense",
      );
    });

    // SW-EW-24 AC-6 (preserved through SLW-FX-11): a repo-less team with
    // NOTHING to show still renders no strip (no empty `flex-1` container).
    // SLW-FX-11 re-expresses the contract: hidden requires no resolved repo
    // (`tabRepoPath===""`) AND no workspace-derived fallback
    // (`contextRepoPath===null`, i.e. a slug-less workspace) AND no open tabs
    // (`hasTabs===false`). The seeded root workspace carries no
    // `sourceRepoSlug`, so contextRepoPath is null; `openTabs` is empty.
    it("hides the viewer tab bar for a repo-less team with no open tabs (SW-EW-24 AC-6)", () => {
      seedActiveWorkspace();
      useViewerStore.setState({ layoutMode: "shared", openTabs: {} });
      mockTabRepoPath = "";
      render(<TitleBar />);
      expect(screen.queryByTestId("stub-viewer-tabs")).toBeNull();
    });

    // SLW-FX-11 (SW-EW-24 AC-6, byte-for-byte): a SLUG-BEARING workspace (so
    // contextRepoPath is truthy) with ZERO open tabs and tabRepoPath not yet
    // resolved must NOT mount an empty strip pre-discovery. The mount rides
    // `tabRepoPath || hasTabs`, NEVER the contextRepoPath prop fallback — this
    // guards against re-adding contextRepoPath to the mount condition (which
    // would mount an invisible empty strip here).
    it("does not mount an empty strip for a slug workspace with no tabs before discovery (SW-EW-24 AC-6)", () => {
      useViewerStore.setState({
        workspaces: {
          "/r": [
            {
              id: "w1",
              kind: "root",
              cwd: "/r",
              sourceRepoPath: "/r",
              sourceRepoSlug: "acme/codesense",
              status: "ready",
            },
          ],
        },
        activeWorkspace: { "/r": "w1" },
        layoutMode: "shared",
        openTabs: {}, // hasTabs=false
      });
      mockTabRepoPath = ""; // orgs not yet discovered → tabRepoPath falsy
      render(<TitleBar />);
      expect(screen.queryByTestId("stub-viewer-tabs")).toBeNull();
    });

    // SLW-FX-11: pins the `tabRepoPath` term of the mount gate independently
    // of `hasTabs`. A resolved repo mounts the strip even with ZERO open tabs
    // (the warm steady state — an empty tab strip is normal once a repo
    // resolves). Pairs with the AC-6 cases (which pin `hasTabs=false` hides) so
    // BOTH gate terms are covered: a degenerate `hasTabs`-only gate fails here.
    it("mounts ViewerTabs via tabRepoPath alone when there are no open tabs (gate term coverage)", () => {
      seedActiveWorkspace();
      useViewerStore.setState({ layoutMode: "shared", openTabs: {} }); // hasTabs=false
      mockTabRepoPath = "workspace:acme:default:acme/codesense";
      render(<TitleBar />);
      expect(screen.getByTestId("stub-viewer-tabs")).toHaveAttribute(
        "data-repo-path",
        "workspace:acme:default:acme/codesense",
      );
    });

    // SLW-FX-11 AC-3 (the core cold-open fix): with org discovery still in
    // flight (`tabRepoPath===""`) but a slug-bearing workspace focused and
    // persisted tabs present, the strip mounts on first paint — fed the
    // workspace-derived fallback repoPath — instead of blanking until
    // `setOrgs` lands.
    it("mounts ViewerTabs on cold open via the workspace fallback when tabRepoPath is empty (AC-3)", () => {
      useViewerStore.setState({
        workspaces: {
          "/r": [
            {
              id: "w1",
              kind: "root",
              cwd: "/r",
              sourceRepoPath: "/r",
              sourceRepoSlug: "acme/codesense",
              status: "ready",
            },
          ],
        },
        activeWorkspace: { "/r": "w1" },
        layoutMode: "shared",
        openTabs: { "/r": [{ name: "README.md", path: "/r/README.md" }] },
      });
      mockTabRepoPath = ""; // orgs not yet discovered
      render(<TitleBar />);
      // Mounts, fed the contextRepoPath fallback ("/r"), not blank.
      expect(screen.getByTestId("stub-viewer-tabs")).toHaveAttribute("data-repo-path", "/r");
    });

    // SLW-FX-11 AC-3b (wholistic — covers epic workspaces): an epic
    // integration workspace has NO `sourceRepoSlug`, so contextRepoPath is
    // ALSO null. The `hasTabs` term (not contextRepoPath) is what lights the
    // strip on cold open. Mounts with an empty repoPath — the tabs render from
    // `openTabs[teamKey]`, and the empty repoPath degrades actions safely
    // until `tabRepoPath` resolves.
    it("mounts ViewerTabs on cold open for an epic workspace via hasTabs (AC-3b)", () => {
      useViewerStore.setState({
        workspaces: {
          "/r": [{ id: "epic1", kind: "epic", cwd: "/r", epicDirPath: "SW-EW", status: "ready" }],
        },
        activeWorkspace: { "/r": "epic1" },
        layoutMode: "shared",
        openTabs: { "/r": [{ name: "epic.md", path: "/r/epic.md" }] },
      });
      mockTabRepoPath = ""; // orgs not yet discovered
      render(<TitleBar />);
      const tabs = screen.getByTestId("stub-viewer-tabs");
      expect(tabs).toBeInTheDocument();
      // No slug + no tabRepoPath → fallback resolves to "".
      expect(tabs).toHaveAttribute("data-repo-path", "");
    });

    // SLW-FX-11 AC-4 (SW-FX-37 flash fix preserved): once orgs load,
    // `tabRepoPath` is truthy and WINS in the prop over the fallback, and it's
    // stable across a workspace switch to a workspace with a different
    // `sourceRepoSlug` (the team-stable invariant SW-FX-37 established).
    it("uses tabRepoPath (not the fallback) once orgs load, stable across a workspace switch (AC-4)", () => {
      useViewerStore.setState({
        workspaces: {
          "/r": [
            {
              id: "w1",
              kind: "root",
              cwd: "/r",
              sourceRepoPath: "/r",
              sourceRepoSlug: "acme/codesense",
              status: "ready",
            },
            {
              id: "w2",
              kind: "root",
              cwd: "/r2",
              sourceRepoPath: "/r2",
              sourceRepoSlug: "acme/other",
              status: "ready",
            },
          ],
        },
        activeWorkspace: { "/r": "w1" },
        layoutMode: "shared",
        openTabs: { "/r": [{ name: "README.md", path: "/r/README.md" }] },
      });
      mockTabRepoPath = "workspace:acme:default:acme/codesense";
      const { rerender } = render(<TitleBar />);
      // tabRepoPath wins over contextRepoPath ("/r").
      expect(screen.getByTestId("stub-viewer-tabs")).toHaveAttribute(
        "data-repo-path",
        "workspace:acme:default:acme/codesense",
      );
      // Switch to the other workspace (different sourceRepoSlug); tabRepoPath is
      // team-stable, so the resolved prop is identical (no re-derive / flash).
      useViewerStore.setState({ activeWorkspace: { "/r": "w2" } });
      rerender(<TitleBar />);
      expect(screen.getByTestId("stub-viewer-tabs")).toHaveAttribute(
        "data-repo-path",
        "workspace:acme:default:acme/codesense",
      );
    });
  });
});
