// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import {
  createGithubRepo,
  recloneManagedSourceRepo,
  updateManagedClone,
  retryLegacyRepositoryJob,
} from "./commands";
it("legacy mutation wrappers reject locally without dispatching IPC", async () => {
  for (const operation of [
    () => createGithubRepo("owner", "repo", true),
    () => recloneManagedSourceRepo(42, "/repo"),
    () => updateManagedClone(42, "/repo"),
    () => retryLegacyRepositoryJob("legacy"),
  ]) {
    await expect(operation()).rejects.toThrow(/retired/);
  }
  expect(invoke).not.toHaveBeenCalled();
});
