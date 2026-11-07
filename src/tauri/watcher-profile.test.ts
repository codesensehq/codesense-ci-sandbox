// SPDX-License-Identifier: MIT
import { expect, it, vi } from "vitest";
const { invoke } = vi.hoisted(() => ({ invoke: vi.fn().mockResolvedValue(undefined) }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@/shared/lib/local-profile-session", () => ({ rendererPrincipal: async () => "42" }));
import {
  startWatching,
  stopWatching,
  addWatch,
  removeWatch,
  lazyAttachWorkspace,
  lazyDetachWorkspace,
  type WatchConfig,
} from "./commands";
it("fences every renderer watcher lifecycle operation with its boot principal", async () => {
  const config = { repoPath: "/checkout" } as unknown as WatchConfig;
  await startWatching([config]);
  await stopWatching();
  await addWatch(config);
  await removeWatch(config);
  await lazyAttachWorkspace("workspace", "owner/repo", config);
  await lazyDetachWorkspace("workspace", config);
  expect(invoke.mock.calls.map(([command]) => command)).toEqual([
    "start_watching",
    "stop_watching",
    "add_watch",
    "remove_watch",
    "lazy_attach_workspace",
    "lazy_detach_workspace",
  ]);
  for (const [, args] of invoke.mock.calls) expect(args).toMatchObject({ expectedPrincipal: "42" });
});
