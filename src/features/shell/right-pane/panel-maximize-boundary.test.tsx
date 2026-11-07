// SPDX-License-Identifier: MIT
import { beforeEach, describe, expect, it, vi } from "vitest";

import { fireEvent, render, screen } from "@/test-utils";

// SW-WG-9: the docked-header maximize gate + the overlay foreground gate both compare
// the view's own team to the active team — pin the active team so `isFullscreen` is
// reachable in the test (mirrors builtin-panels.test.tsx).
const activeTeamKeyRef = vi.hoisted(() => ({ current: "acme:default" }));
vi.mock("@/shared/lib/team-key", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/shared/lib/team-key")>();
  return {
    ...actual,
    useActiveTeamKey: () => activeTeamKeyRef.current,
    getActiveTeamKey: () => activeTeamKeyRef.current,
  };
});

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
}));

import { Columns3 } from "lucide-react";

import { promptIdentityFromRepoPath, useViewerStore } from "@/shared/stores/viewer-store";

import { registerPanel, getPanel } from "./panel-registry";

import { PanelMaximizeBoundary, PanelMaximizeButton } from "./panel-maximize-boundary";

const REPO = "workspace:acme:default:owner/repo";
const TEAM = promptIdentityFromRepoPath(REPO)!.teamKey;
/** Neutral panel ids — the boundary is panel-agnostic. */
const PANEL = "panel-a";
const SIBLING = "panel-b";

// A minimal right-pane layout with `activePanelId` as the (expanded) active panel —
// the boundary gates its overlay on this so a hidden/inactive panel never holds a live
// overlay + Esc listener.
function layoutWithActive(activePanelId: string) {
  return {
    sections: [
      { id: "s1", group: { panelIds: [PANEL], activePanelId }, collapsed: false, size: 1 },
    ],
  };
}

beforeEach(() => {
  activeTeamKeyRef.current = TEAM;
  registerPanel({
    id: PANEL,
    title: "Panel A",
    icon: Columns3,
    render: () => null,
    maximizable: true,
  });
  useViewerStore.setState({
    maximizedPanel: {},
    rightPaneLayout: layoutWithActive(PANEL),
  });
});

describe("PanelMaximizeButton (SW-WG-9)", () => {
  it("toggles this panel's maximize for the view's own team", () => {
    render(<PanelMaximizeButton panelId={PANEL} repoPath={REPO} />);
    fireEvent.click(screen.getByLabelText("Maximize panel"));
    expect(useViewerStore.getState().maximizedPanel[TEAM]).toBe(PANEL);
    // Now reflects restore state; a second click exits.
    fireEvent.click(screen.getByLabelText("Restore panel"));
    expect(useViewerStore.getState().maximizedPanel[TEAM]).toBeUndefined();
  });

  it("renders nothing when the view has no team key (legacy real-path repo)", () => {
    const { container } = render(
      <PanelMaximizeButton panelId={PANEL} repoPath="/plain/local/path" />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});

describe("PanelMaximizeBoundary (SW-WG-9)", () => {
  it("does not show an overlay for a renderer without maximize capability", () => {
    registerPanel({ ...getPanel(PANEL)!, maximizable: false });
    useViewerStore.setState({ maximizedPanel: { [TEAM]: PANEL } });
    render(
      <PanelMaximizeBoundary panelId={PANEL} repoPath={REPO} title="Panel A">
        <div>panel body</div>
      </PanelMaximizeBoundary>,
    );
    expect(screen.getByTestId("panel-a-fullscreen-shell")).toHaveAttribute(
      "data-fullscreen",
      "false",
    );
  });

  it("is docked by default, maximizes when the slice targets this panel, and restores via ✕", () => {
    render(
      <>
        <PanelMaximizeButton panelId={PANEL} repoPath={REPO} />
        <PanelMaximizeBoundary panelId={PANEL} repoPath={REPO} title="Panel A" icon={Columns3}>
          <div>panel body</div>
        </PanelMaximizeBoundary>
      </>,
    );
    const shell = screen.getByTestId("panel-a-fullscreen-shell");
    expect(shell.dataset.fullscreen).toBe("false");

    // Enter maximize via the generic host button → the boundary reacts.
    fireEvent.click(screen.getByLabelText("Maximize panel"));
    expect(shell.dataset.fullscreen).toBe("true");

    // The generic ✕ restore lives INSIDE the overlay (the host button is covered).
    fireEvent.click(screen.getByLabelText("Exit full screen"));
    expect(shell.dataset.fullscreen).toBe("false");
    expect(useViewerStore.getState().maximizedPanel[TEAM]).toBeUndefined();
  });

  it("suppresses the overlay + Esc listener when the panel is NOT the active tab, keeping state", () => {
    // The panel is maximized IN STATE, but a sibling tab is the active panel of its
    // section — so the panel's body is hidden. The overlay + document Esc listener must
    // NOT be live (else Esc would exit an invisible maximize), yet the state is kept.
    useViewerStore.setState({
      maximizedPanel: { [TEAM]: PANEL },
      rightPaneLayout: layoutWithActive(SIBLING),
    });
    render(
      <PanelMaximizeBoundary panelId={PANEL} repoPath={REPO} title="Panel A" icon={Columns3}>
        <div>panel body</div>
      </PanelMaximizeBoundary>,
    );
    const shell = screen.getByTestId("panel-a-fullscreen-shell");
    expect(shell.dataset.fullscreen).toBe("false");
    expect(screen.queryByLabelText("Exit full screen")).toBeNull();
    // State preserved — switching back to the panel (active) re-shows it maximized.
    expect(useViewerStore.getState().maximizedPanel[TEAM]).toBe(PANEL);
  });

  it("truncates the overlay title so a long (widget-authored) title can't push the ✕ off-screen", () => {
    // SW-WG-14: widgets are the first boundary consumer with arbitrary user-authored
    // titles. The title span must shrink/truncate so the exit control stays reachable.
    useViewerStore.setState({ maximizedPanel: { [TEAM]: PANEL } });
    render(
      <PanelMaximizeBoundary panelId={PANEL} repoPath={REPO} title="Panel A" icon={Columns3}>
        <div>panel body</div>
      </PanelMaximizeBoundary>,
    );
    const titleSpan = screen.getByText("Panel A");
    expect(titleSpan.className).toContain("truncate");
    expect(titleSpan.className).toContain("min-w-0");
  });

  // SW-WG-12 AC-7: the truncating title must still expose its full name to a screen
  // reader and a mouse hover, so it carries both a native tooltip and an accessible name.
  it("gives the overlay title an accessible name + native tooltip (AC-7)", () => {
    useViewerStore.setState({ maximizedPanel: { [TEAM]: PANEL } });
    render(
      <PanelMaximizeBoundary
        panelId={PANEL}
        repoPath={REPO}
        title="A very long widget-authored title"
        icon={Columns3}
      >
        <div>body</div>
      </PanelMaximizeBoundary>,
    );
    const titleSpan = screen.getByLabelText("A very long widget-authored title");
    expect(titleSpan).toHaveAttribute("title", "A very long widget-authored title");
  });

  // SW-WG-12 AC-6: the overlay carries an optional badge slot (a widget's severity count),
  // rendered next to the title — symmetric with headerActions, benefiting any consumer.
  it("renders the optional badge slot next to the title (AC-6)", () => {
    useViewerStore.setState({ maximizedPanel: { [TEAM]: PANEL } });
    render(
      <PanelMaximizeBoundary
        panelId={PANEL}
        repoPath={REPO}
        title="Panel A"
        icon={Columns3}
        badge={<span data-testid="sev-badge">3</span>}
        headerActions={<button type="button">Refresh</button>}
      >
        <div>body</div>
      </PanelMaximizeBoundary>,
    );
    expect(screen.getByTestId("sev-badge")).toBeInTheDocument();
    // AC-7: header actions sit before the ✕ (tab into the action, not out of the overlay).
    expect(screen.getByRole("button", { name: "Refresh" })).toBeInTheDocument();
    expect(screen.getByLabelText("Exit full screen")).toBeInTheDocument();
  });

  it("suppresses the overlay when the panel's section is COLLAPSED", () => {
    useViewerStore.setState({
      maximizedPanel: { [TEAM]: PANEL },
      rightPaneLayout: {
        sections: [
          {
            id: "s1",
            group: { panelIds: [PANEL], activePanelId: PANEL },
            collapsed: true,
            size: 1,
          },
        ],
      },
    });
    render(
      <PanelMaximizeBoundary panelId={PANEL} repoPath={REPO} title="Panel A" icon={Columns3}>
        <div>panel body</div>
      </PanelMaximizeBoundary>,
    );
    expect(screen.getByTestId("panel-a-fullscreen-shell").dataset.fullscreen).toBe("false");
  });
});
