// SPDX-License-Identifier: MIT
import { defineConfig, devices } from "@playwright/test";

/**
 * Layout specs against the frontend mock backend (`pnpm dev:mock`, see
 * src/app/mock/README.md) — real browser geometry, no Rust. Starts its own
 * mock vite on `PORT` (default 5198, never the shared :1420) unless one is
 * already up there; or point `BASE_URL` at a server you started yourself.
 *
 *   npm run test:e2e:mock              # self-hosted on :5198
 *   PORT=5311 npm run test:e2e:mock    # another free port
 *
 * A server already on the port must belong to this worktree (checked via the dev-only
 * `/__cs-worktree` marker, SV-PBF-163); a foreign or unverifiable one fails the run. Stop it,
 * change `PORT`, or start your own server and set `BASE_URL` to it.
 */
const PORT = Number(process.env.PORT ?? 5198);
const BASE_URL = process.env.BASE_URL ?? `http://localhost:${PORT}`;

export default defineConfig({
  globalSetup: "./scripts/playwright-guard-mock.ts",
  testDir: "./tests/e2e-mock",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: BASE_URL,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    actionTimeout: 15_000,
    navigationTimeout: 60_000,
  },
  timeout: 90_000,
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    {
      name: "webkit",
      testMatch: /(?:agent-list-qa-live|pane-save)\.spec\.ts/,
      use: { ...devices["Desktop Safari"] },
    },
  ],
  webServer: process.env.BASE_URL
    ? undefined
    : {
        command: `pnpm run dev:mock --port ${PORT} --strictPort`,
        url: BASE_URL,
        reuseExistingServer: !process.env.CI,
        timeout: 120_000,
      },
});
