// SPDX-License-Identifier: MIT
import { defineConfig, devices } from "@playwright/test";

/**
 * SV-INP-29 real-frame checks for the embed pipe (cs-embed.js): a sandboxed
 * allow-scripts page frame builds a nested allow-scripts embed frame, the real
 * forwarder talks to it over real postMessage. No app or dev server is needed.
 *
 *   npx playwright test -c playwright.live-embed.config.ts
 */
export default defineConfig({
  testDir: "./tests/live-embed",
  fullyParallel: true,
  retries: 0,
  reporter: [["list"]],
  timeout: 60_000,
  use: { actionTimeout: 15_000 },
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    { name: "webkit", use: { ...devices["Desktop Safari"] } },
  ],
});
