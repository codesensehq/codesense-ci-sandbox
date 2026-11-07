// SPDX-License-Identifier: MIT
import { revealSlot } from "@/features/shell/slots/reveal-slot";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  SCRATCH_PANEL_ID,
  INBOX_PANEL_ID,
  MAIN_SECTION_ID,
  SCRATCH_SECTION_ID,
  defaultRightPaneLayout,
  collapseSection,
  mergePanelIntoSection,
  removePanel,
  setActivePanel,
  isPanelVisible,
  findPanelLocation,
} from "@/features/shell/right-pane/right-pane-layout";

import { useViewerStore } from "./viewer-store";

vi.mock("@/tauri/commands", async (original) => ({
  ...(await original<typeof import("@/tauri/commands")>()),
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

describe("revealSlot Notes surface", () => {
  beforeEach(() => {
    useViewerStore.setState({ rightPaneOpen: false, rightPaneLayout: defaultRightPaneLayout() });
    vi.clearAllMocks();
  });

  it("opens the right pane and makes the scratch panel visible (from the default layout)", () => {
    revealSlot("right-pane", { surface: "notes" });
    const s = useViewerStore.getState();
    expect(s.rightPaneOpen).toBe(true);
    expect(isPanelVisible(s.rightPaneLayout, SCRATCH_PANEL_ID)).toBe(true);
  });

  it("opens the pane even when it started closed", () => {
    useViewerStore.setState({ rightPaneOpen: false });
    revealSlot("right-pane", { surface: "notes" });
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
  });

  it("activates the scratch tab when another panel is active in the scratch group", () => {
    // Merge scratch into the main Inbox group, then make the Inbox the
    // active tab — so scratch is a *background* tab.
    let layout = mergePanelIntoSection(defaultRightPaneLayout(), SCRATCH_PANEL_ID, MAIN_SECTION_ID);
    layout = setActivePanel(layout, MAIN_SECTION_ID, INBOX_PANEL_ID);
    useViewerStore.setState({ rightPaneLayout: layout, rightPaneOpen: true });

    revealSlot("right-pane", { surface: "notes" });

    const next = useViewerStore.getState().rightPaneLayout;
    const loc = findPanelLocation(next, SCRATCH_PANEL_ID)!;
    expect(next.sections[loc.sectionIndex].group.activePanelId).toBe(SCRATCH_PANEL_ID);
    expect(isPanelVisible(next, SCRATCH_PANEL_ID)).toBe(true);
  });

  it("re-adds the scratch panel if it is somehow missing from the layout", () => {
    // Defensive branch: strip scratch out entirely, then reveal must restore it.
    const layout = removePanel(defaultRightPaneLayout(), SCRATCH_PANEL_ID);
    expect(findPanelLocation(layout, SCRATCH_PANEL_ID)).toBeNull();
    useViewerStore.setState({ rightPaneLayout: layout, rightPaneOpen: false });

    revealSlot("right-pane", { surface: "notes" });

    const next = useViewerStore.getState();
    expect(next.rightPaneOpen).toBe(true);
    expect(isPanelVisible(next.rightPaneLayout, SCRATCH_PANEL_ID)).toBe(true);
  });

  it("expands the scratch section when it was collapsed", () => {
    const layout = collapseSection(defaultRightPaneLayout(), SCRATCH_SECTION_ID);
    // sanity: it really is collapsed to start
    expect(isPanelVisible(layout, SCRATCH_PANEL_ID)).toBe(false);
    useViewerStore.setState({ rightPaneLayout: layout, rightPaneOpen: true });

    revealSlot("right-pane", { surface: "notes" });

    expect(isPanelVisible(useViewerStore.getState().rightPaneLayout, SCRATCH_PANEL_ID)).toBe(true);
  });
});
