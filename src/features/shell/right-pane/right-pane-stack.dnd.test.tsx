// SPDX-License-Identifier: MIT
import { act, render, screen } from "@testing-library/react";
import { Workflow } from "lucide-react";
import { useEffect } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Replace DndContext with a passthrough that captures the real `onDragEnd` so we
// can drive a synthetic drag-end (`{ active, over }`) without pointer geometry —
// the in-repo precedent (workspace-terminal-panels.test) trusts the pure
// resolver and drives the store action; this additionally exercises the wiring.
let capturedOnDragEnd: ((e: unknown) => void) | undefined;
vi.mock("@dnd-kit/core", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@dnd-kit/core")>();
  return {
    ...actual,
    DndContext: ({
      children,
      onDragEnd,
    }: {
      children: React.ReactNode;
      onDragEnd?: (e: unknown) => void;
    }) => {
      capturedOnDragEnd = onDragEnd;
      return <>{children}</>;
    },
  };
});

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  saveScratchNote: vi.fn().mockResolvedValue(undefined),
  loadScratchNote: vi.fn().mockResolvedValue(""),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

import { useViewerStore } from "@/shared/stores/viewer-store";

import { usePanelRegistry } from "./panel-registry";
import { gapDropId, sectionDragId, tabDragId } from "./right-pane-dnd";
import { RightPaneHostProvider, type RightPaneHostValue } from "./right-pane-host-context";
import {
  MAIN_SECTION_ID,
  SCRATCH_PANEL_ID,
  SCRATCH_SECTION_ID,
  INBOX_PANEL_ID,
  defaultRightPaneLayout,
  type RightPaneLayout,
} from "./right-pane-layout";
import { RightPaneStack } from "./right-pane-stack";

const HOST: RightPaneHostValue = { repoPath: "/repo", onClose: vi.fn() };

/** Neutral panel ids — the drag wiring is panel-agnostic. */
const PANEL_A = "panel-a";
const PANEL_B = "panel-b";

/** The default layout with a multi-tab main group: [ main(A*, B, Inbox), scratch ]. */
function tabbedLayout(): RightPaneLayout {
  const layout = defaultRightPaneLayout();
  layout.sections[0].group = {
    panelIds: [PANEL_A, PANEL_B, INBOX_PANEL_ID],
    activePanelId: PANEL_A,
  };
  return layout;
}

const mounts = { count: {} as Record<string, number> };

function stub(id: string, title: string) {
  const Body = () => {
    useEffect(() => {
      mounts.count[id] = (mounts.count[id] ?? 0) + 1;
    }, []);
    return <div data-testid={`body-${id}`}>{title} body</div>;
  };
  return { id, title, icon: Workflow, render: () => <Body /> };
}

function renderStack(layout: RightPaneLayout) {
  useViewerStore.setState({ rightPaneLayout: layout });
  return render(
    <RightPaneHostProvider value={HOST}>
      <RightPaneStack />
    </RightPaneHostProvider>,
  );
}

const ids = () => useViewerStore.getState().rightPaneLayout.sections.map((s) => s.id);
const fireDragEnd = (active: string, over: string | null) =>
  act(() => capturedOnDragEnd?.({ active: { id: active }, over: over ? { id: over } : null }));

// Captured before any test can swap it for a spy (AC-4), restored each test.
const realSetLayout = useViewerStore.getState().setRightPaneLayout;

beforeEach(() => {
  capturedOnDragEnd = undefined;
  mounts.count = {};
  useViewerStore.setState({ setRightPaneLayout: realSetLayout });
  usePanelRegistry.setState({
    panels: {
      [PANEL_A]: stub(PANEL_A, "Panel A"),
      [PANEL_B]: stub(PANEL_B, "Panel B"),
      [SCRATCH_PANEL_ID]: stub(SCRATCH_PANEL_ID, "Scratch"),
    },
  });
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("RightPaneStack drag wiring (onDragEnd → model op → setRightPaneLayout)", () => {
  it("AC-1: dropping a section onto another reorders the stack (persisted layout)", () => {
    renderStack(tabbedLayout());
    expect(ids()).toEqual([MAIN_SECTION_ID, SCRATCH_SECTION_ID]);
    fireDragEnd(sectionDragId(MAIN_SECTION_ID), sectionDragId(SCRATCH_SECTION_ID));
    expect(ids()).toEqual([SCRATCH_SECTION_ID, MAIN_SECTION_ID]);
  });

  it("reorders a tab within its group (drop B onto A → B|A), panel stays mounted", () => {
    const { container } = renderStack(tabbedLayout()); // main = [A, B, Inbox]
    // B mounts once it's been the active panel; switch then capture. It is
    // inactive initially, so assert via the layout + the active A body node.
    const bodyA = screen.getByTestId(`body-${PANEL_A}`);

    fireDragEnd(tabDragId(PANEL_B), tabDragId(PANEL_A));

    const main = useViewerStore
      .getState()
      .rightPaneLayout.sections.find((s) => s.id === MAIN_SECTION_ID);
    expect(main?.group.panelIds).toEqual([PANEL_B, PANEL_A, INBOX_PANEL_ID]);
    // A (the active, mounted one) was not remounted by the tab reorder.
    expect(container.querySelector(`[data-testid="body-${PANEL_A}"]`)).toBe(bodyA);
    expect(mounts.count[PANEL_A]).toBe(1);
  });

  it("AC-2: dropping a tab onto another section merges it as the active tab", () => {
    renderStack(tabbedLayout());
    fireDragEnd(tabDragId(SCRATCH_PANEL_ID), sectionDragId(MAIN_SECTION_ID));
    const layout = useViewerStore.getState().rightPaneLayout;
    const main = layout.sections.find((s) => s.id === MAIN_SECTION_ID);
    expect(main?.group.panelIds).toContain(SCRATCH_PANEL_ID);
    expect(main?.group.activePanelId).toBe(SCRATCH_PANEL_ID);
    // scratch's old (now-empty) section dropped
    expect(layout.sections.some((s) => s.id === SCRATCH_SECTION_ID)).toBe(false);
  });

  it("AC-3: dropping a tab from a multi-panel group into a gap ungroups it", () => {
    renderStack(tabbedLayout()); // main = [A, B, Inbox]
    fireDragEnd(tabDragId(PANEL_B), gapDropId(2));
    const layout = useViewerStore.getState().rightPaneLayout;
    const solo = layout.sections.find(
      (s) => s.group.panelIds.length === 1 && s.group.panelIds[0] === PANEL_B,
    );
    expect(solo).toBeDefined();
    const main = layout.sections.find((s) => s.id === MAIN_SECTION_ID);
    expect(main?.group.panelIds).toEqual([PANEL_A, INBOX_PANEL_ID]);
  });

  it("AC-4: an invalid drop (no target) leaves the layout unchanged", () => {
    const setSpy = vi.fn();
    renderStack(tabbedLayout());
    useViewerStore.setState({ setRightPaneLayout: setSpy });
    fireDragEnd(sectionDragId(MAIN_SECTION_ID), null);
    expect(setSpy).not.toHaveBeenCalled();
    expect(ids()).toEqual([MAIN_SECTION_ID, SCRATCH_SECTION_ID]);
  });

  it("AC-5: a reordered panel stays MOUNTED (same DOM node, no remount)", () => {
    const { container } = renderStack(tabbedLayout());
    const scratchBody = screen.getByTestId(`body-${SCRATCH_PANEL_ID}`);
    expect(mounts.count[SCRATCH_PANEL_ID]).toBe(1);

    // Reorder scratch above main.
    fireDragEnd(sectionDragId(SCRATCH_SECTION_ID), sectionDragId(MAIN_SECTION_ID));
    expect(ids()).toEqual([SCRATCH_SECTION_ID, MAIN_SECTION_ID]);

    // Same DOM node, never re-mounted (react-flow/editor state would survive).
    expect(container.querySelector(`[data-testid="body-${SCRATCH_PANEL_ID}"]`)).toBe(scratchBody);
    expect(mounts.count[SCRATCH_PANEL_ID]).toBe(1);
  });

  it("AC-5: a GROUPED panel stays MOUNTED across the cross-section move", () => {
    const { container } = renderStack(tabbedLayout());
    const scratchBody = screen.getByTestId(`body-${SCRATCH_PANEL_ID}`);
    expect(mounts.count[SCRATCH_PANEL_ID]).toBe(1);

    // Group scratch into the main section — a cross-section move.
    fireDragEnd(tabDragId(SCRATCH_PANEL_ID), sectionDragId(MAIN_SECTION_ID));
    const main = useViewerStore
      .getState()
      .rightPaneLayout.sections.find((s) => s.id === MAIN_SECTION_ID);
    expect(main?.group.panelIds).toContain(SCRATCH_PANEL_ID);

    // The reparenting host kept the SAME DOM node mounted (no react-flow/Tiptap
    // reset) even though it moved to a different section's subtree.
    expect(container.querySelector(`[data-testid="body-${SCRATCH_PANEL_ID}"]`)).toBe(scratchBody);
    expect(mounts.count[SCRATCH_PANEL_ID]).toBe(1);
  });

  it("AC-5: an UNGROUPED panel stays MOUNTED across the split into a new section", () => {
    // Start with B as the active tab so its body is mounted up front.
    const layout = tabbedLayout();
    layout.sections[0].group.activePanelId = PANEL_B;
    const { container } = renderStack(layout);
    const bodyB = screen.getByTestId(`body-${PANEL_B}`);
    expect(mounts.count[PANEL_B]).toBe(1);

    // Ungroup B into its own new section — a cross-section move.
    fireDragEnd(tabDragId(PANEL_B), gapDropId(2));
    const solo = useViewerStore
      .getState()
      .rightPaneLayout.sections.find(
        (s) => s.group.panelIds.length === 1 && s.group.panelIds[0] === PANEL_B,
      );
    expect(solo).toBeDefined();

    // Same DOM node, never remounted.
    expect(container.querySelector(`[data-testid="body-${PANEL_B}"]`)).toBe(bodyB);
    expect(mounts.count[PANEL_B]).toBe(1);
  });

  it("a reordered custom PANE stays MOUNTED (its iframe state survives)", () => {
    // A pane body's live state (its iframe document) must survive a reorder — assert
    // it for an actual pane panel, not just a built-in stub, on the same shared
    // reparenting path.
    const paneId = "pane:acme:core/sales";
    usePanelRegistry.setState({
      panels: { ...usePanelRegistry.getState().panels, [paneId]: stub(paneId, "Sales") },
    });
    const layout: RightPaneLayout = {
      sections: [
        {
          id: MAIN_SECTION_ID,
          group: { panelIds: [PANEL_A], activePanelId: PANEL_A },
          collapsed: false,
          size: 0.5,
        },
        {
          id: "pane-sec",
          group: { panelIds: [paneId], activePanelId: paneId },
          collapsed: false,
          size: 0.5,
        },
      ],
    };
    const { container } = renderStack(layout);
    const paneBody = screen.getByTestId(`body-${paneId}`);
    expect(mounts.count[paneId]).toBe(1);

    // Reorder the pane's section above the main section.
    fireDragEnd(sectionDragId("pane-sec"), sectionDragId(MAIN_SECTION_ID));
    expect(ids()).toEqual(["pane-sec", MAIN_SECTION_ID]);

    // Same DOM node — the pane's body was reparented, never remounted, so its
    // document state is preserved across the move.
    expect(container.querySelector(`[data-testid="body-${paneId}"]`)).toBe(paneBody);
    expect(mounts.count[paneId]).toBe(1);
  });
});
