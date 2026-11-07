// SPDX-License-Identifier: MIT
import { Workflow } from "lucide-react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { getEffectiveShortcut, useShortcutsStore } from "@/shared/stores/shortcuts-store";
import { useViewerStore } from "@/shared/stores/viewer-store";

import { buildNavMenuEntries, buildPanelMenuEntries, handlePanelMenuAction } from "./panel-menu";
import { usePanelRegistry } from "./panel-registry";
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

function layoutWith(panelId: string): RightPaneLayout {
  return {
    sections: [
      {
        id: "s1",
        group: { panelIds: [panelId], activePanelId: panelId },
        collapsed: false,
        size: 1,
      },
    ],
  };
}

beforeEach(() => {
  usePanelRegistry.setState({ panels: {} });
  useViewerStore.setState({
    rightPaneLayout: layoutWith("p1"),
    rightPaneOpen: false,
  });
});

describe("buildPanelMenuEntries", () => {
  it("derives one entry per registered panel, checked = present in the layout", () => {
    usePanelRegistry.setState({
      panels: {
        p1: { id: "p1", title: "Panel One", icon: Workflow, render: () => null },
        p2: { id: "p2", title: "Panel Two", icon: Workflow, render: () => null },
      },
    });
    const entries = buildPanelMenuEntries();
    expect(entries).toEqual([
      { id: "p1", label: "Panel One", checked: true },
      { id: "p2", label: "Panel Two", checked: false },
    ]);
  });
});

describe("buildNavMenuEntries", () => {
  beforeEach(() => {
    useShortcutsStore.setState({ shortcuts: {} });
  });

  it("surfaces exactly the curated axis representatives, in NAV_AXES order", () => {
    expect(buildNavMenuEntries().map((e) => e.id)).toEqual([
      "nav.context-back",
      "nav.context-forward",
      "viewer.cycle-workspace-next",
      "viewer.cycle-workspace-prev",
      "viewer.cycle-terminal-tab-next",
      "viewer.cycle-terminal-tab-prev",
      "viewer.cycle-epic-next",
      "viewer.cycle-epic-prev",
      "viewer.focus-pane-left",
      "viewer.focus-pane-right",
      "viewer.focus-pane-up",
      "viewer.focus-pane-down",
      "viewer.jump-root-workspace",
      "viewer.flip-last-workspace",
      // SW-UX: the management-surface openers (virtual tabs) in the Go menu.
      "nav.open-workspaces",
      "nav.open-teams",
      "nav.open-machines",
      "nav.open-source-repos",
    ]);
  });

  it("omits the nine positional workspace jumps (self-evident Cmd+1..9)", () => {
    const ids = buildNavMenuEntries().map((e) => e.id);
    expect(ids.some((id) => id.startsWith("viewer.jump-workspace-"))).toBe(false);
  });

  it("resolves each entry's live effective accelerator (no empty keystrokes)", () => {
    for (const entry of buildNavMenuEntries()) {
      expect(entry.accelerator).toBe(getEffectiveShortcut(entry.id));
      expect(entry.accelerator.length).toBeGreaterThan(0);
    }
  });

  it("labels come from the registry menu label, not a duplicate", () => {
    const byId = Object.fromEntries(buildNavMenuEntries().map((e) => [e.id, e.label]));
    expect(byId["viewer.cycle-workspace-next"]).toBe("Cycle Story Workspace Next");
    expect(byId["viewer.flip-last-workspace"]).toBe("Flip to Last Workspace");
  });

  it("reflects a rebind through getEffectiveShortcut with no Rust change (AC-4)", () => {
    useShortcutsStore.setState({
      shortcuts: { "viewer.cycle-workspace-next": "CmdOrCtrl+Shift+G" },
    });
    const entry = buildNavMenuEntries().find((e) => e.id === "viewer.cycle-workspace-next");
    expect(entry?.accelerator).toBe("CmdOrCtrl+Shift+G");
  });
});

describe("handlePanelMenuAction", () => {
  it("returns false for a non-panel menu id (falls back to shortcut registry)", () => {
    expect(handlePanelMenuAction("viewer.toggle-terminal")).toBe(false);
  });

  it("unchecking a present panel removes it from the layout", () => {
    expect(handlePanelMenuAction("panel:p1")).toBe(true);
    expect(findPanelLocation(useViewerStore.getState().rightPaneLayout, "p1")).toBeNull();
  });

  it("checking an absent panel adds it as the top section and opens the pane", () => {
    expect(handlePanelMenuAction("panel:agent")).toBe(true);
    const layout = useViewerStore.getState().rightPaneLayout;
    expect(layout.sections[0].group.panelIds).toEqual(["agent"]);
    expect(useViewerStore.getState().rightPaneOpen).toBe(true);
  });
});
