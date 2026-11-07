// SPDX-License-Identifier: MIT
/**
 * Starts `pnpm run dev` on :1420, or reuses one already there. A reused server must belong to
 * this worktree (dev-only `/__cs-worktree` marker, SV-PBF-163); another worktree's fails the
 * run. Stop it, or start your own server and set `BASE_URL` to it.
 */
import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  // SV-PBF-163: refuse to reuse a :1420 dev server started from another worktree.
  globalSetup: "./scripts/playwright-guard-e2e.ts",
  testDir: "./tests/e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 1 : undefined,
  reporter: [
    ["html", { open: "never" }],
    ["list"],
    ...(process.env.CI ? [["junit", { outputFile: "test-results/e2e-results.xml" }] as const] : []),
  ],
  use: {
    baseURL: process.env.BASE_URL || "http://localhost:1420",
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
    actionTimeout: 15_000,
    navigationTimeout: 30_000,
  },
  timeout: 60_000,
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
  webServer: {
    command: "pnpm run dev",
    url: "http://localhost:1420",
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
  },
});
