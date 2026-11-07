// SPDX-License-Identifier: MIT
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { resolve } from "node:path";

const { spawnSync } = vi.hoisted(() => ({ spawnSync: vi.fn() }));
vi.mock("node:child_process", () => ({ default: { spawnSync }, spawnSync }));
let originalArgv: string[];
beforeEach(() => {
  originalArgv = process.argv;
  vi.resetModules();
  spawnSync.mockReturnValue({ status: 0 });
  vi.stubEnv("CONFORMANCE_DASHBOARDS", undefined);
  vi.spyOn(process, "exit").mockImplementation((code) => {
    throw new Error(`exit ${code}`);
  });
});
afterEach(() => {
  process.argv = originalArgv;
  vi.restoreAllMocks();
  vi.unstubAllEnvs();
  spawnSync.mockClear();
});

it("forwards a grep filter without treating its arguments as dashboard paths", async () => {
  process.argv = ["node", "run-conformance.mjs", "--grep", "files-tree"];
  await expect(import("./run-conformance.mjs")).rejects.toThrow("exit 0");
  expect(spawnSync.mock.calls[0][1]).toEqual([
    "test",
    "-c",
    "playwright.conformance.config.ts",
    "--grep",
    "files-tree",
  ]);
  expect(spawnSync.mock.calls[0][2].env).not.toHaveProperty("CONFORMANCE_DASHBOARDS");
});

it("keeps explicit pane paths and their slot when a grep filter is supplied", async () => {
  process.argv = [
    "node",
    "run-conformance.mjs",
    "pane.html",
    "--slot",
    "file-nav",
    "--grep",
    "resync",
  ];
  await expect(import("./run-conformance.mjs")).rejects.toThrow("exit 0");
  expect(spawnSync.mock.calls[0][1]).toEqual([
    "test",
    "-c",
    "playwright.conformance.config.ts",
    "conformance.spec.ts",
    "--grep",
    "resync",
  ]);
  expect(JSON.parse(spawnSync.mock.calls[0][2].env.CONFORMANCE_DASHBOARDS)).toEqual([
    { path: resolve("pane.html"), slotId: "file-nav" },
  ]);
});
