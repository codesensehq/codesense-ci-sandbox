// SPDX-License-Identifier: MIT
import "@/test/prefs-state-fixture";
/**
 * SW-XS-17: CodeSense no longer caps worktrees or reaps idle ones. Opening a
 * workspace never asks for capacity, the hand-off copy is gone, and a saved
 * profile that still carries the retired cap / idle-threshold / pin /
 * merged auto-remove settings
 * loads without them.
 */
import { describe, expect, it, vi } from "vitest";

import * as viewerStore from "./viewer-store";
import { useViewerStore, HANDLED_FAILED_STEPS } from "./viewer-store";

vi.mock("@/tauri/commands", () => ({
  saveAppState: vi.fn().mockResolvedValue(undefined),
  loadAppState: vi.fn().mockResolvedValue("{}"),
  pushViewerState: vi.fn().mockResolvedValue(undefined),
  checkPathExists: vi.fn().mockResolvedValue(true),
}));

const RETIRED_KEYS = [
  "maxWorktrees",
  "idleThresholdMinutes",
  "worktreePins",
  "autoRemoveMergedWorktrees",
] as const;

describe("no worktree cap (SW-XS-17)", () => {
  it("AC-1: there is no capacity hand-off message and no at-capacity failure step", () => {
    expect(viewerStore).not.toHaveProperty("worktreeCapacityHandoffMessage");
    expect(viewerStore).not.toHaveProperty("DEFAULT_MAX_WORKTREES");
    expect(viewerStore).not.toHaveProperty("DEFAULT_IDLE_THRESHOLD_MINUTES");
    expect((HANDLED_FAILED_STEPS as readonly string[]).includes("at-capacity")).toBe(false);
  });

  it("AC-4: a fresh profile's settings carry none of the retired worktree settings", () => {
    const settings = useViewerStore.getState().settings as unknown as Record<string, unknown>;
    for (const key of RETIRED_KEYS) expect(settings).not.toHaveProperty(key);
  });

  it("AC-4: loading a v12 profile drops the cap, idle-threshold, pin and merged auto-remove settings", async () => {
    const { loadAppState } = await import("@/tauri/commands");
    vi.mocked(loadAppState).mockResolvedValue(
      JSON.stringify({
        viewer: {
          schemaVersion: 12,
          openTabs: {},
          activeTab: {},
          terminalVisible: false,
          settings: {
            maxWorktrees: 20,
            idleThresholdMinutes: 30,
            worktreePins: ["/wt/a"],
            autoRemoveMergedWorktrees: false,
            quitOnClose: true,
          },
        },
      }),
    );

    await useViewerStore.getState().loadPersistedState();

    const settings = useViewerStore.getState().settings as unknown as Record<string, unknown>;
    for (const key of RETIRED_KEYS) expect(settings).not.toHaveProperty(key);
    // Unrelated settings survive.
    expect(settings.quitOnClose).toBe(true);
  });
});
