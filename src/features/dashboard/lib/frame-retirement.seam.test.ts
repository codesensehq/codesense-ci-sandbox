// SPDX-License-Identifier: MIT
import { afterEach, expect, it, vi } from "vitest";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { acknowledgeFlush, registerFrame } from "./frame-lifecycle";
import { removePanel, type RightPaneLayout } from "@/features/shell/right-pane/right-pane-layout";
const cleanups: Array<() => void> = [];
afterEach(() => {
  cleanups.splice(0).forEach((off) => off());
  vi.restoreAllMocks();
  vi.useRealTimers();
});
it("panel closes retain the layout until ack, and overlapping closes retire together", async () => {
  vi.useFakeTimers();
  vi.spyOn(useViewerStore.getState(), "persistState").mockImplementation(() => {});
  const layout: RightPaneLayout = {
    sections: ["a", "b"].map((id) => ({
      id,
      size: 0.5,
      collapsed: false,
      group: { panelIds: [id], activePanelId: id },
    })),
  };
  useViewerStore.setState({ rightPaneLayout: layout });
  const a = vi.fn(),
    b = vi.fn();
  for (const [id, post] of [
    ["a", a],
    ["b", b],
  ] as const)
    cleanups.push(
      registerFrame({
        frameId: id,
        panelId: id,
        dashboard: id,
        slotId: "right-pane",
        hide: vi.fn(),
        post,
      }),
    );
  useViewerStore.getState().setRightPaneLayout(removePanel(layout, "a"));
  useViewerStore.getState().setRightPaneLayout(removePanel(layout, "b"));
  expect(useViewerStore.getState().rightPaneLayout).toBe(layout);
  acknowledgeFlush("b", b.mock.calls[0][0].id);
  await vi.advanceTimersByTimeAsync(0);
  expect(useViewerStore.getState().rightPaneLayout).toBe(layout);
  acknowledgeFlush("a", a.mock.calls[0][0].id);
  await vi.advanceTimersByTimeAsync(0);
  expect(useViewerStore.getState().rightPaneLayout.sections).toHaveLength(0);
});
