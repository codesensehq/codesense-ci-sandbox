// SPDX-License-Identifier: MIT
import { defineConfig, devices } from "@playwright/test";
export default defineConfig({
  testDir: "./tests/pane-harness",
  workers: 1,
  timeout: 20_000,
  reporter: "list",
  projects: [
    { name: "chromium", use: { ...devices["Desktop Chrome"] } },
    { name: "webkit", use: { ...devices["Desktop Safari"] } },
  ],
});
