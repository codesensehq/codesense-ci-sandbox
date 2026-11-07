import { test as base, expect } from "@playwright/test";

/**
 * Extended test fixtures for CodeSense E2E tests.
 *
 * Usage: import { test, expect } from "../support/fixtures";
 */

// App fixture — waits for CodeSense to be ready
export const test = base.extend<{
  appReady: void;
}>({
  appReady: [
    async ({ page }, use) => {
      // Wait for the app to finish loading (splash screen gone)
      await page.goto("/");
      await page.waitForSelector(
        '[data-testid="app-ready"], [data-testid="login-view"], [data-testid="repo-picker"]',
        {
          timeout: 30_000,
        },
      );
      await use();
    },
    { auto: true },
  ],
});

export { expect };
