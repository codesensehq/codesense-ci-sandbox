// SPDX-License-Identifier: MIT
import { Workflow } from "lucide-react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { useViewerStore } from "@/shared/stores/viewer-store";

import { usePanelRegistry } from "./panel-registry";
import { addPanelToRightPane, removePanelFromRightPane } from "./right-pane-actions";
import { findPanelLocation, type RightPaneLayout } from "./right-pane-layout";

vi.mock("@/tauri/commands", () => ({
  rebuildMenu: vi.fn().mockResolvedValue(undefined),
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

function layoutWith(...panelIds: string[]): RightPaneLayout {
  return {
    sections: panelIds.map((id, i) => ({
      id: `s${i}`,
      group: { panelIds: [id], activePanelId: id },
      collapsed: false,
      size: 1,
    })),
  };
}

beforeEach(() => {
  usePanelRegistry.setState({ panels: {} });
  useViewerStore.setState({
    rightPaneLayout: layoutWith("p1"),
    rightPaneOpen: false,
  });
});

describe("addPanelToRightPane", () => {
  it("adds a registered panel to the layout and opens the pane", () => {
    usePanelRegistry.setState({
      panels: { p2: { id: "p2", title: "Two", icon: Workflow, render: () => null } },
    });
    expect(addPanelToRightPane("p2")).toBe(true);
    const s = useViewerStore.getState();
    expect(findPanelLocation(s.rightPaneLayout, "p2")).not.toBeNull();
    expect(s.rightPaneOpen).toBe(true);
  });

  it("is a no-op for an already-present panel (no duplicate, still present)", () => {
    expect(addPanelToRightPane("p1")).toBe(true);
    const layout = useViewerStore.getState().rightPaneLayout;
    const count = layout.sections.flatMap((sec) => sec.group.panelIds).filter((id) => id === "p1");
    expect(count).toHaveLength(1);
  });

  it("refuses a retired (offerable:false) panel — returns false, not added", () => {
    usePanelRegistry.setState({
      panels: {
        r: { id: "r", title: "Retired", icon: Workflow, render: () => null, offerable: false },
      },
    });
    expect(addPanelToRightPane("r")).toBe(false);
    expect(findPanelLocation(useViewerStore.getState().rightPaneLayout, "r")).toBeNull();
  });
});

describe("removePanelFromRightPane", () => {
  it("removes a panel and closes the pane when the last one goes", () => {
    useViewerStore.setState({ rightPaneOpen: true });
    removePanelFromRightPane("p1");
    const s = useViewerStore.getState();
    expect(s.rightPaneLayout.sections).toHaveLength(0);
    expect(s.rightPaneOpen).toBe(false);
  });

  it("keeps the pane open when other panels remain", () => {
    useViewerStore.setState({ rightPaneLayout: layoutWith("p1", "p2"), rightPaneOpen: true });
    removePanelFromRightPane("p1");
    const s = useViewerStore.getState();
    expect(findPanelLocation(s.rightPaneLayout, "p2")).not.toBeNull();
    expect(s.rightPaneOpen).toBe(true);
  });
});
