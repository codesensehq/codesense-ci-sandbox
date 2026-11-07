// SPDX-License-Identifier: MIT
import { render, screen, fireEvent } from "@testing-library/react";
import { Workflow } from "lucide-react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useViewerStore } from "@/shared/stores/viewer-store";

import { usePanelRegistry } from "./panel-registry";
import { RightPaneHostProvider, type RightPaneHostValue } from "./right-pane-host-context";
import {
  SCRATCH_PANEL_ID,
  INBOX_PANEL_ID,
  defaultRightPaneLayout,
  type RightPaneLayout,
} from "./right-pane-layout";
import { RightPaneStack } from "./right-pane-stack";

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

const HOST: RightPaneHostValue = { repoPath: "/repo", onClose: vi.fn() };

function stub(id: string, title: string) {
  return {
    id,
    title,
    icon: Workflow,
    render: () => <div data-testid={`body-${id}`}>{title} body</div>,
  };
}

function stubWithActions(id: string, title: string, onAction: () => void) {
  return {
    ...stub(id, title),
    headerActions: () => (
      <button data-testid={`ha-${id}`} onClick={onAction}>
        Act
      </button>
    ),
  };
}

function renderStack(layout: RightPaneLayout) {
  useViewerStore.setState({ rightPaneLayout: layout });
  return render(
    <RightPaneHostProvider value={HOST}>
      <RightPaneStack />
    </RightPaneHostProvider>,
  );
}

// Capture the store's real setRightPaneLayout once, before any test stubs it.
// The resize test replaces it with a vi.fn() via setState, which
// vi.restoreAllMocks() does NOT revert (it's a state write, not a spy) — so
// without this restore every later test would inherit a no-op setter.
const realSetRightPaneLayout = useViewerStore.getState().setRightPaneLayout;

beforeEach(() => {
  usePanelRegistry.setState({ panels: {} });
});

afterEach(() => {
  vi.restoreAllMocks();
  useViewerStore.setState({ setRightPaneLayout: realSetRightPaneLayout });
});

describe("RightPaneStack", () => {
  it("AC-4: the default layout renders the Inbox over scratch (no regression)", () => {
    usePanelRegistry.setState({
      panels: {
        [INBOX_PANEL_ID]: stub(INBOX_PANEL_ID, "Inbox"),
        [SCRATCH_PANEL_ID]: stub(SCRATCH_PANEL_ID, "Scratch"),
      },
    });
    renderStack(defaultRightPaneLayout());

    // the Inbox is the active body of the main section
    expect(screen.getByTestId(`body-${INBOX_PANEL_ID}`)).toBeInTheDocument();
    // scratch is its own section (title bar shows the single panel title)
    expect(screen.getByText("Scratch")).toBeInTheDocument();
    // the inter-section resize divider exists between the two expanded sections
    expect(screen.getByTestId("rps-section-divider")).toBeInTheDocument();
  });

  it("AC-3 + AC-3b: switches the active tab and keeps the inactive panel MOUNTED", () => {
    usePanelRegistry.setState({ panels: { p1: stub("p1", "P1"), p2: stub("p2", "P2") } });
    renderStack({
      sections: [
        {
          id: "s1",
          group: { panelIds: ["p1", "p2"], activePanelId: "p1" },
          collapsed: false,
          size: 1,
        },
      ],
    });

    // Only the active panel is mounted initially (lazy mount).
    expect(screen.getByTestId("body-p1")).toBeInTheDocument();
    expect(screen.queryByTestId("body-p2")).toBeNull();

    // Switch to P2 → it mounts and becomes visible; P1 stays MOUNTED (hidden).
    fireEvent.click(screen.getByRole("button", { name: /P2/ }));
    expect(screen.getByTestId("body-p2")).toBeInTheDocument();
    const p1Wrapper = screen.getByTestId("body-p1").parentElement!;
    expect(p1Wrapper.style.display).toBe("none"); // hidden, not unmounted
    const p2Wrapper = screen.getByTestId("body-p2").parentElement!;
    expect(p2Wrapper.style.display).toBe("flex");

    // Switch back → both remain in the DOM (no remount, state preserved).
    fireEvent.click(screen.getByRole("button", { name: /P1/ }));
    expect(screen.getByTestId("body-p1")).toBeInTheDocument();
    expect(screen.getByTestId("body-p2")).toBeInTheDocument();
  });

  it("AC-2: collapses a section (panel kept mounted, hidden) and disables collapse on the last expanded section", () => {
    usePanelRegistry.setState({ panels: { p1: stub("p1", "P1"), p2: stub("p2", "P2") } });
    renderStack({
      sections: [
        { id: "s1", group: { panelIds: ["p1"], activePanelId: "p1" }, collapsed: false, size: 0.5 },
        { id: "s2", group: { panelIds: ["p2"], activePanelId: "p2" }, collapsed: false, size: 0.5 },
      ],
    });

    // both bodies mounted (both expanded)
    expect(screen.getByTestId("body-p1")).toBeInTheDocument();
    expect(screen.getByTestId("body-p2")).toBeInTheDocument();

    // collapse section 1 via its chevron (both sections expanded → two buttons)
    fireEvent.click(screen.getAllByRole("button", { name: "Collapse section" })[0]);

    // body still in the DOM (mounted), but its container is hidden
    expect(screen.getByTestId("body-p1")).toBeInTheDocument();
    const bodyContainer = screen.getByTestId("body-p1").parentElement!.parentElement!;
    expect(bodyContainer.style.display).toBe("none");

    // floor: the remaining expanded section's collapse control is disabled
    const expandS1 = screen.getByRole("button", { name: "Expand section" });
    expect(expandS1).not.toBeDisabled();
    const lastCollapse = screen.getByRole("button", { name: "Collapse section" });
    expect(lastCollapse).toBeDisabled();
  });

  it("AC-3: resize divider commits the section size once on release, never during pointermove", () => {
    const setLayout = vi.fn();
    useViewerStore.setState({ setRightPaneLayout: setLayout });
    Element.prototype.setPointerCapture = vi.fn();
    Element.prototype.releasePointerCapture = vi.fn();
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      height: 1000,
      width: 500,
      top: 0,
      left: 0,
      right: 500,
      bottom: 1000,
      x: 0,
      y: 0,
      toJSON: () => "",
    });
    usePanelRegistry.setState({ panels: { p1: stub("p1", "P1"), p2: stub("p2", "P2") } });
    renderStack({
      sections: [
        { id: "s1", group: { panelIds: ["p1"], activePanelId: "p1" }, collapsed: false, size: 0.5 },
        { id: "s2", group: { panelIds: ["p2"], activePanelId: "p2" }, collapsed: false, size: 0.5 },
      ],
    });

    const divider = screen.getByTestId("rps-section-divider");
    fireEvent.pointerDown(divider, { pointerId: 1, clientY: 1000 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientY: 900 });
    fireEvent.pointerMove(divider, { pointerId: 1, clientY: 800 });
    expect(setLayout).not.toHaveBeenCalled(); // visual-follow only during drag
    fireEvent.pointerUp(divider, { pointerId: 1, clientY: 800 });
    expect(setLayout).toHaveBeenCalledTimes(1); // single commit on release
  });

  it("an expanded section uses fill mode (basis 0%) while a collapsed one is auto-height", () => {
    usePanelRegistry.setState({ panels: { p1: stub("p1", "P1"), p2: stub("p2", "P2") } });
    renderStack({
      sections: [
        {
          id: "s1",
          group: { panelIds: ["p1"], activePanelId: "p1" },
          collapsed: false,
          size: 0.55,
        },
        { id: "s2", group: { panelIds: ["p2"], activePanelId: "p2" }, collapsed: true, size: 0.45 },
      ],
    });
    // Expanded section → flex-basis 0% so its (normalized) grow fills the pane.
    // (The grow value itself is a CSS var that happy-dom can't represent.)
    const expandedSection =
      screen.getByTestId("body-p1").parentElement!.parentElement!.parentElement!;
    expect(expandedSection.style.flexBasis).toBe("0%");
    // Collapsed section → auto-height (title bar only), grow 0.
    const collapsedSection = screen.getByText("P2").closest("div")!.parentElement!.parentElement!;
    expect(collapsedSection.style.flexBasis).toBe("auto");
    expect(collapsedSection.style.flexGrow).toBe("0");
  });

  it("falls back to equal-share fill when all expanded sizes are degenerate (0)", () => {
    usePanelRegistry.setState({ panels: { p1: stub("p1", "P1"), p2: stub("p2", "P2") } });
    renderStack({
      sections: [
        { id: "s1", group: { panelIds: ["p1"], activePanelId: "p1" }, collapsed: false, size: 0 },
        { id: "s2", group: { panelIds: ["p2"], activePanelId: "p2" }, collapsed: false, size: 0 },
      ],
    });
    // Both render in fill mode (basis 0%) — no NaN/zero-grow gap.
    const s1 = screen.getByTestId("body-p1").parentElement!.parentElement!.parentElement!;
    const s2 = screen.getByTestId("body-p2").parentElement!.parentElement!.parentElement!;
    expect(s1.style.flexBasis).toBe("0%");
    expect(s2.style.flexBasis).toBe("0%");
  });

  it("renders a guiding empty-state (not a blank pane) when all panels are closed", () => {
    renderStack({ sections: [] });
    expect(screen.getByText("No panels open")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /Close pane/ })).toBeInTheDocument();
  });

  it("the empty state points at View → Panels — there is no widget gallery to open", () => {
    renderStack({ sections: [] });
    expect(screen.getByText(/View → Panels/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /widget/i })).toBeNull();
  });

  it("AC-2: renders headerActions for the active panel of an expanded section; none for a panel without", () => {
    const onAction = vi.fn();
    usePanelRegistry.setState({
      panels: { p1: stubWithActions("p1", "P1", onAction), p2: stub("p2", "P2") },
    });
    renderStack({
      sections: [
        { id: "s1", group: { panelIds: ["p1"], activePanelId: "p1" }, collapsed: false, size: 0.5 },
        { id: "s2", group: { panelIds: ["p2"], activePanelId: "p2" }, collapsed: false, size: 0.5 },
      ],
    });
    // p1 has headerActions → rendered; p2 has none → nothing extra.
    expect(screen.getByTestId("ha-p1")).toBeInTheDocument();
    expect(screen.queryByTestId("ha-p2")).toBeNull();
  });

  it("AC-2: clicking a header action runs it but does NOT toggle the section collapse", () => {
    const onAction = vi.fn();
    usePanelRegistry.setState({
      panels: { p1: stubWithActions("p1", "P1", onAction), p2: stub("p2", "P2") },
    });
    renderStack({
      sections: [
        { id: "s1", group: { panelIds: ["p1"], activePanelId: "p1" }, collapsed: false, size: 0.5 },
        { id: "s2", group: { panelIds: ["p2"], activePanelId: "p2" }, collapsed: false, size: 0.5 },
      ],
    });
    fireEvent.click(screen.getByTestId("ha-p1"));
    expect(onAction).toHaveBeenCalledTimes(1);
    // Section stays expanded — its body container is still visible (not collapsed).
    const bodyContainer = screen.getByTestId("body-p1").parentElement!.parentElement!;
    expect(bodyContainer.style.display).toBe("flex");
  });

  it("AC-2: in a multi-panel group each tab's headerActions live in ITS OWN body toolbar; the inactive tab's is hidden, not in the chrome", () => {
    const onA = vi.fn();
    const onB = vi.fn();
    usePanelRegistry.setState({
      panels: { p1: stubWithActions("p1", "P1", onA), p2: stubWithActions("p2", "P2", onB) },
    });
    renderStack({
      sections: [
        {
          id: "s1",
          group: { panelIds: ["p1", "p2"], activePanelId: "p1" },
          collapsed: false,
          size: 1,
        },
      ],
    });
    // Each panel's toolbar mounts in its OWN reparented body (never the shared chrome). p1 is active
    // from the start → its toolbar is mounted + visible; p2 hasn't been shown yet, so it's lazily
    // unmounted (bodies mount only once made visible). The per-panel reparented container (whose
    // `display` toggles with active/inactive) is the body node's DIRECT parent.
    const containerOf = (id: string) => screen.getByTestId(`body-${id}`).parentElement!;
    expect(screen.getByTestId("ha-p1")).toBeInTheDocument();
    expect(containerOf("p1").style.display).toBe("flex");
    expect(screen.queryByTestId("ha-p2")).toBeNull();
    // Switch the active tab → p2's body + toolbar mount and show; p1's body stays mounted but hides.
    fireEvent.click(screen.getByRole("button", { name: /P2/ }));
    expect(screen.getByTestId("ha-p2")).toBeInTheDocument();
    expect(containerOf("p2").style.display).toBe("flex");
    expect(containerOf("p1").style.display).toBe("none");
  });

  it("AC-2: header actions are not rendered while the section is collapsed", () => {
    const onAction = vi.fn();
    usePanelRegistry.setState({
      panels: { p1: stubWithActions("p1", "P1", onAction), p2: stub("p2", "P2") },
    });
    renderStack({
      sections: [
        { id: "s1", group: { panelIds: ["p1"], activePanelId: "p1" }, collapsed: true, size: 0.5 },
        { id: "s2", group: { panelIds: ["p2"], activePanelId: "p2" }, collapsed: false, size: 0.5 },
      ],
    });
    expect(screen.queryByTestId("ha-p1")).toBeNull();
  });

  it("an all-unregistered section is DROPPED (no dead body, no raw-id title)", () => {
    usePanelRegistry.setState({ panels: { p1: stub("p1", "P1") } });
    renderStack({
      sections: [
        {
          id: "s1",
          group: { panelIds: ["ghost"], activePanelId: "ghost" },
          collapsed: false,
          size: 1,
        },
      ],
    });
    // The render-time filter drops a section whose whole group is
    // unregistered — no dead body AND no raw-id title bar (the old fallback).
    expect(screen.queryByTestId("body-ghost")).toBeNull();
    expect(screen.queryByText("ghost")).toBeNull();
    // It was the only section → the guiding empty state shows instead of a dead pane.
    expect(screen.getByText("No panels open")).toBeInTheDocument();
  });

  it("an unregistered TAB is hidden while its registered sibling still renders", () => {
    usePanelRegistry.setState({ panels: { p1: stub("p1", "P1") } });
    renderStack({
      sections: [
        {
          id: "s1",
          // stored active is the absent pane → display falls back to p1.
          group: {
            panelIds: ["pane:acme:core/absent", "p1"],
            activePanelId: "pane:acme:core/absent",
          },
          collapsed: false,
          size: 1,
        },
      ],
    });
    expect(screen.getByTestId("body-p1")).toBeInTheDocument();
    expect(screen.queryByText("pane:acme:core/absent")).toBeNull();
  });
});
