// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The persist subscription flushes through the tauri bridge — stub it so setState
// in tests never reaches a real IPC call.
vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
}));

import { useViewerStore } from "@/shared/stores/viewer-store";

import {
  acknowledgeFlush,
  hasLiveFrames,
  registerFrame,
} from "@/features/dashboard/lib/frame-lifecycle";
import { findPanelLocation } from "./right-pane-layout";

const cleanups: Array<() => void> = [];

afterEach(() => {
  cleanups.splice(0).forEach((off) => off());
  vi.useRealTimers();
  vi.restoreAllMocks();
});

// The generic reveal seam the named reveal actions and `pane_open` share.
describe("revealPanelById", () => {
  beforeEach(() => {
    useViewerStore.setState({
      rightPaneLayout: { sections: [] },
      rightPaneOpen: false,
    });
  });

  it("places the panel as an expanded, ACTIVE top section and opens the pane", () => {
    useViewerStore.getState().revealPanelById("pane:acme:core/sales");

    const s = useViewerStore.getState();
    expect(s.rightPaneOpen).toBe(true);
    const loc = findPanelLocation(s.rightPaneLayout, "pane:acme:core/sales");
    expect(loc).not.toBeNull();
    const section = s.rightPaneLayout.sections[loc!.sectionIndex];
    // Expanded (not collapsed) + the active tab of its group — so the section-header
    // chrome (incl. the maximize control) is discoverable on arrival.
    expect(section.collapsed).toBe(false);
    expect(section.group.activePanelId).toBe("pane:acme:core/sales");
  });

  it("is idempotent — revealing the same panel twice yields ONE section", () => {
    useViewerStore.getState().revealPanelById("pane:acme:core/sales");
    useViewerStore.getState().revealPanelById("pane:acme:core/sales");
    const withPane = useViewerStore
      .getState()
      .rightPaneLayout.sections.filter((s) => s.group.panelIds.includes("pane:acme:core/sales"));
    expect(withPane).toHaveLength(1);
  });

  it("retires only right-pane frames whose panelId leaves the layout", async () => {
    vi.useFakeTimers();

    function makeFrame(id: string, slotId: string, panelId: string) {
      const post = vi.fn();
      const hide = vi.fn();
      const frame = { frameId: id, dashboard: id, slotId, panelId, post, hide };
      cleanups.push(registerFrame(frame));
      return { frame, post, hide };
    }

    // Dock and file-nav frames carry panelIds that are never in the right-pane
    // layout; they must survive a right-pane layout change.
    const dock = makeFrame("dock-frame", "dock", "pane:absent:dock");
    const fileNav = makeFrame("file-nav-frame", "file-nav", "pane:absent:file-nav");
    const rightShared = makeFrame("right-shared-frame", "right-pane", "pane:shared:demo");
    const rightOutgoing = makeFrame("right-outgoing-frame", "right-pane", "pane:outgoing:demo");

    useViewerStore.setState({
      rightPaneLayout: {
        sections: [
          {
            id: "shared-section",
            group: { panelIds: ["pane:shared:demo"], activePanelId: "pane:shared:demo" },
            collapsed: false,
            size: 0.5,
          },
          {
            id: "outgoing-section",
            group: { panelIds: ["pane:outgoing:demo"], activePanelId: "pane:outgoing:demo" },
            collapsed: false,
            size: 0.5,
          },
        ],
      },
      rightPaneOpen: true,
    });

    // Reveal adds/preserves existing panels; no frame should retire here.
    useViewerStore.getState().revealPanelById("pane:new:demo");

    expect(dock.hide).not.toHaveBeenCalled();
    expect(fileNav.hide).not.toHaveBeenCalled();
    expect(rightShared.hide).not.toHaveBeenCalled();
    expect(rightOutgoing.hide).not.toHaveBeenCalled();
    expect(dock.post).not.toHaveBeenCalled();
    expect(fileNav.post).not.toHaveBeenCalled();
    expect(rightShared.post).not.toHaveBeenCalled();
    expect(rightOutgoing.post).not.toHaveBeenCalled();

    // Now explicitly remove the outgoing panel from the right-pane layout.
    // The right-pane outgoing frame must retire; the unrelated dock and file-nav
    // frames must stay untouched because they belong to different slots.
    useViewerStore.getState().setRightPaneLayout({
      sections: [
        {
          id: "shared-section",
          group: { panelIds: ["pane:shared:demo"], activePanelId: "pane:shared:demo" },
          collapsed: false,
          size: 0.5,
        },
        {
          id: "new-section",
          group: { panelIds: ["pane:new:demo"], activePanelId: "pane:new:demo" },
          collapsed: false,
          size: 0.5,
        },
      ],
    });

    expect(dock.hide).not.toHaveBeenCalled();
    expect(fileNav.hide).not.toHaveBeenCalled();
    expect(rightShared.hide).not.toHaveBeenCalled();
    expect(rightOutgoing.hide).toHaveBeenCalledOnce();

    expect(dock.post).not.toHaveBeenCalled();
    expect(fileNav.post).not.toHaveBeenCalled();
    expect(rightShared.post).not.toHaveBeenCalled();
    expect(rightOutgoing.post).toHaveBeenCalledExactlyOnceWith(
      expect.objectContaining({ type: "codesense:flush" }),
    );

    // The unrelated frames stay live; the outgoing frame is still flushing.
    expect(hasLiveFrames((f) => f.frameId === dock.frame.frameId)).toBe(true);
    expect(hasLiveFrames((f) => f.frameId === fileNav.frame.frameId)).toBe(true);
    expect(hasLiveFrames((f) => f.frameId === rightShared.frame.frameId)).toBe(true);
    expect(hasLiveFrames((f) => f.frameId === rightOutgoing.frame.frameId)).toBe(true);

    // Acknowledge the flush; the store's layout apply runs after drain.
    acknowledgeFlush(rightOutgoing.frame.frameId, rightOutgoing.post.mock.calls[0][0].id);
    await vi.advanceTimersByTimeAsync(0);

    const layoutAfter = useViewerStore.getState().rightPaneLayout;
    expect(findPanelLocation(layoutAfter, "pane:outgoing:demo")).toBeNull();
    expect(findPanelLocation(layoutAfter, "pane:shared:demo")).not.toBeNull();

    expect(rightOutgoing.post).toHaveBeenLastCalledWith(
      expect.objectContaining({ type: "codesense:teardown" }),
    );
    expect(dock.post).not.toHaveBeenCalled();
    expect(fileNav.post).not.toHaveBeenCalled();
    expect(rightShared.post).not.toHaveBeenCalled();
  });
});
