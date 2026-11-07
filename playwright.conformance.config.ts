// SPDX-License-Identifier: MIT
import { defineConfig, devices } from "@playwright/test";
// Conformance runs in real WebKit, the engine the app's frames use.
export default defineConfig({
  testDir: "./tests/conformance",
  workers: 1,
  timeout: 30_000,
  reporter: "list",
  outputDir: process.env.PLAYWRIGHT_OUTPUT_DIR ?? "test-results",
  projects: [{ name: "webkit", use: { ...devices["Desktop Safari"] } }],
});
