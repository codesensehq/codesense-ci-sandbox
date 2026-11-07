// SPDX-License-Identifier: MIT
import { test, expect, type Page } from "@playwright/test";

import { getTauriMockInitScript } from "../support/fixtures/tauri-ipc-mock";

/**
 * SLW-DV-8 (AC-2) scope breadcrumb — the REAL-LAYOUT coverage the jsdom unit
 * suites can't reach: the hook resolves the viewport-top scope via
 * `posAtCoords` + `syntaxTree`, both of which need real geometry (jsdom returns
 * 0 / throws). Drives the real `<CodeDiffReview>` via the shared
 * `tests/e2e/harness/live-diff.html` and asserts the breadcrumb appears without
 * scrolling (DW-DV8-2 layout retry) and tracks the enclosing scope on scroll.
 * Closes DW-DV8-1.
 */

const HARNESS_URL = "/tests/e2e/harness/live-diff.html";

/** A tall TS class with two clearly-separated methods. `base` shifts every body
 *  line's value so before/after differ on every line → nothing collapses → the
 *  after editor renders the full source and the frame genuinely scrolls. */
const tsClass = (base: number): string =>
  [
    "class Alpha {",
    "  methodOne() {",
    ...Array.from({ length: 30 }, (_, i) => `    const a${i} = ${base + i};`),
    "  }",
    "  methodTwo() {",
    ...Array.from({ length: 30 }, (_, i) => `    const b${i} = ${base + i};`),
    '    return "bottom";',
    "  }",
    "}",
    "",
  ].join("\n");

interface HarnessProps {
  before: string;
  after: string;
  path: string;
  mode: "split" | "inline";
  autoFollow: boolean;
}

type HarnessWindow = Window & {
  __diffHarness: { set: (partial: Partial<HarnessProps>) => void; ready: boolean };
};

async function gotoHarness(page: Page): Promise<void> {
  await page.addInitScript(getTauriMockInitScript({}));
  await page.goto(HARNESS_URL);
  await page.waitForFunction(
    () => (window as unknown as HarnessWindow).__diffHarness?.ready === true,
  );
}

async function setDiff(page: Page, partial: Partial<HarnessProps>): Promise<void> {
  await page.evaluate((p) => (window as unknown as HarnessWindow).__diffHarness.set(p), partial);
}

test.describe("@E2E SLW-DV-8 scope breadcrumb (real layout)", () => {
  test("resolves the enclosing scope at the viewport top (no live edit)", async ({ page }) => {
    await gotoHarness(page);
    await setDiff(page, {
      before: tsClass(1000),
      after: tsClass(0),
      path: "harness.ts",
      mode: "split",
    });
    await page.waitForSelector(".cm-content");
    const crumb = page.getByTestId("code-diff-breadcrumb");
    const scroller = page.getByTestId("code-diff-review");

    // Park a few lines into methodOne (the file top is the class HEADER line,
    // whose enclosing scope is the file, not a method). The bar resolves the
    // viewport-top scope purely from a scroll — no user gesture / live edit.
    await scroller.evaluate((el) => {
      el.scrollTop = 140;
    });
    await expect(crumb).toBeVisible();
    await expect.poll(() => crumb.textContent(), { timeout: 3000 }).toContain("methodOne");
    await expect(crumb).toContainText("class Alpha");
  });

  test("tracks the enclosing method as the viewport scrolls", async ({ page }) => {
    await gotoHarness(page);
    await setDiff(page, {
      before: tsClass(1000),
      after: tsClass(0),
      path: "harness.ts",
      mode: "split",
    });
    await page.waitForSelector(".cm-content");
    const crumb = page.getByTestId("code-diff-breadcrumb");
    const scroller = page.getByTestId("code-diff-review");

    // Scroll deep enough that methodTwo's body sits at the viewport top.
    await scroller.evaluate((el) => {
      el.scrollTop = 760;
    });

    await expect.poll(() => crumb.textContent(), { timeout: 3000 }).toContain("methodTwo");
    // The class remains the outer crumb in the chain.
    await expect(crumb).toContainText("class Alpha");
  });

  test("shows no breadcrumb for a flat-tree language (rust)", async ({ page }) => {
    await gotoHarness(page);
    await setDiff(page, {
      before: "fn main() {\n    let x = 1;\n}\n",
      after: "fn main() {\n    let x = 2;\n}\n",
      path: "harness.rs",
      mode: "split",
    });
    await page.waitForSelector(".cm-content");

    // rust is a StreamLanguage (flat tree) → the bar is hidden, not faked.
    await expect(page.getByTestId("code-diff-breadcrumb")).toHaveCount(0);
  });
});
