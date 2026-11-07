// SPDX-License-Identifier: MIT
import { beforeEach, vi } from "vitest";

// Legacy store unit cases supply a boot snapshot. The streaming seam uses the real hub instead.
vi.mock("@/features/dashboard/lib/prefs-client", async () => {
  const { create } = await import("zustand");
  return {
    usePrefsStore: create(() => ({
      principal: null,
      global: {},
      owned: {},
      globalReady: false,
      ownedReady: false,
    })),
    readPrefsState: async () => {
      const { loadAppState } = await import("@/tauri/commands");
      return JSON.parse((await loadAppState()) || "{}");
    },
  };
});

beforeEach(async () => {
  const { resetViewerPrefsPersistence } = await import("@/shared/stores/viewer-store");
  resetViewerPrefsPersistence();
});
