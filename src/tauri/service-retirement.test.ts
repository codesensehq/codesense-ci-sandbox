// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { getAuthenticatedUser, listUserOrgs } from "./commands";
it("rejects legacy service operations locally without invoking unavailable native endpoints", async () => {
  for (const operation of [getAuthenticatedUser, listUserOrgs]) {
    await expect(operation()).rejects.toThrow("retired");
  }
  expect(invoke).not.toHaveBeenCalled();
});
