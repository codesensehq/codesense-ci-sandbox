// SPDX-License-Identifier: MIT
import { vi } from "vitest";
import { usePrefsStore } from "@/features/dashboard/lib/prefs-client";

vi.mock("@/features/dashboard/lib/prefs-client", async () => {
  const { create } = await import("zustand");
  return {
    usePrefsStore: create(() => ({
      principal: "42",
      global: {},
      owned: {},
      globalReady: true,
      ownedReady: true,
    })),
    readPrefsState: async () => {
      const { loadAppState } = await import("@/tauri/commands");
      return JSON.parse((await loadAppState()) || "{}");
    },
  };
});

export function publishPolicyFixture(raw: string) {
  usePrefsStore.setState({ owned: JSON.parse(raw), ownedReady: true });
}
