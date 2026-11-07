// SPDX-License-Identifier: MIT
import { tmpdir } from "node:os";
import { join } from "node:path";

import { defineConfig, devices } from "@playwright/test";

/**
 * SV-CDE-86 dashboard kit in real browsers: the same page runs against the in-app kit (kit.js over a bridge) and the hosted
 * toolkit (cs-live.js over the share shell's messages), in Chromium and WebKit, on a virtual clock. No app or dev server.
 * Output goes outside the worktree (a vite dev server reloads on any file change).
 *
 *   npx playwright test -c playwright.dashboard-kit.config.ts
 */
export default defineConfig({
  testDir: "./tests/dashboard-kit",
  outputDir: process.env.CS_PW_OUT ?? join(tmpdir(), "dashboard-kit-pw"),
  fullyParallel: true,
  retries: 0,
  reporter: [["list"]],
  timeout: 30_000,
  use: { actionTimeout: 10_000 },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    { name: "webkit", use: { ...devices["Desktop Safari"] } },
  ],
});
