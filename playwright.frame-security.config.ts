// SPDX-License-Identifier: MIT
import { defineConfig, devices } from "@playwright/test";
export default defineConfig({
  testDir: "./tests/frame-security",
  workers: 1,
  timeout: 20_000,
  // Security specs must run, not skip: the guard reporter fails a run that skipped any test.
  reporter: [["list"], ["./tests/frame-security/support/fixture-guard.mjs"]],
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    { name: "webkit", use: { ...devices["Desktop Safari"] } },
  ],
});
