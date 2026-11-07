// SPDX-License-Identifier: MIT
import { test, expect, type Page, type Locator } from "@playwright/test";

import { getTauriMockInitScript } from "../support/fixtures/tauri-ipc-mock";

/**
 * SLW-DV-5 live-follow — the REAL-LAYOUT coverage the jsdom unit suites can't
 * reach (flash decoration rendering, in-place patch, and scroll geometry all
 * need `getBoundingClientRect`/`coordsAtPos`, which jsdom returns as 0). Drives
 * the real `<CodeDiffReview>` in Chromium via `tests/e2e/harness/live-diff.html`
 * and asserts AC-2 (scroll anchor), AC-3 (flash), AC-4 (follow/pause/pill), and
 * AC-6 (patch in place, no remount). Closes DW-DV5-2/3.
 */

const HARNESS_URL = "/tests/e2e/harness/live-diff.html";

/** N lines that all differ from `otherLines(N)` so nothing collapses → the diff
 *  is tall and the frame genuinely scrolls. */
const oldLines = (n: number): string =>
  Array.from({ length: n }, (_, i) => `old line ${i + 1}`).join("\n");
const newLines = (n: number): string =>
  Array.from({ length: n }, (_, i) => `new line ${i + 1}`).join("\n");

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

/** The text of the after-side `.cm-line` occupying the top of the scroller
 *  viewport — found by geometry (not elementFromPoint, which can land on the
 *  line-number gutter) and scoped to the after (b) editor in split mode. */
async function topVisibleLineText(scroller: Locator): Promise<string> {
  return scroller.evaluate((el) => {
    const viewportTop = el.getBoundingClientRect().top;
    const editors = el.querySelectorAll(".cm-editor");
    const editor = editors[editors.length - 1] ?? el; // after (b) side in split
    const lines = Array.from(editor.querySelectorAll<HTMLElement>(".cm-line"));
    // The last line whose top is still at/above the viewport top edge (i.e. the
    // one straddling the top), else the first line below the fold.
    let straddling = "";
    let straddlingTop = -Infinity;
    let firstBelow = "";
    let firstBelowTop = Infinity;
    for (const line of lines) {
      const top = line.getBoundingClientRect().top;
      if (top <= viewportTop + 2 && top > straddlingTop) {
        straddlingTop = top;
        straddling = line.textContent ?? "";
      }
      if (top >= viewportTop - 2 && top < firstBelowTop) {
        firstBelowTop = top;
        firstBelow = line.textContent ?? "";
      }
    }
    return straddling || firstBelow;
  });
}

test.describe("@E2E SLW-DV-5 live-follow (real layout)", () => {
  test("AC-6/AC-3: a live edit patches the after side IN PLACE and flashes the changed line", async ({
    page,
  }) => {
    await gotoHarness(page);
    await setDiff(page, {
      before: oldLines(60),
      after: newLines(60),
      mode: "split",
      autoFollow: true,
    });
    await page.waitForSelector(".cm-content");

    // Tag the after-side (b) content node so we can prove it survives the update.
    await page.evaluate(() => {
      document.querySelectorAll(".cm-content")[1]?.setAttribute("data-h", "1");
    });

    // Live edit: append a line the agent "wrote".
    await setDiff(page, { after: `${newLines(60)}\nZZZ_NEW_TAIL` });

    // Patched in place — the SAME node kept its marker (never torn down + rebuilt).
    await expect(page.locator('.cm-content[data-h="1"]')).toHaveCount(1);
    await expect(page.locator('.cm-content[data-h="1"]')).toContainText("ZZZ_NEW_TAIL");
    // The changed line pulses (transient — Playwright polls fast enough to catch it).
    await expect(page.locator(".cm-live-flash-line")).not.toHaveCount(0);
  });

  test("AC-4: auto-follow scrolls the newest change into view", async ({ page }) => {
    await gotoHarness(page);
    // Open the content as a new file (a fresh view, as the app mounts a diff),
    // not as a live edit of the harness's empty initial file: filling an empty
    // file IS a live edit, and auto-follow rightly chases it to the bottom.
    await setDiff(page, {
      before: oldLines(80),
      after: newLines(80),
      path: "follow.ts",
      autoFollow: true,
    });
    await page.waitForSelector(".cm-content");
    const scroller = page.getByTestId("code-diff-review");

    expect(await scroller.evaluate((el) => el.scrollTop)).toBe(0);

    await setDiff(page, { after: `${newLines(80)}\nZZZ_TAIL` });

    // Follow drove the outer frame down toward the newest change.
    await expect
      .poll(() => scroller.evaluate((el) => el.scrollTop), { timeout: 3000 })
      .toBeGreaterThan(0);
  });

  test("AC-2: an above-the-fold edit keeps the top line stable when NOT following", async ({
    page,
  }) => {
    await gotoHarness(page);
    await setDiff(page, { before: oldLines(80), after: newLines(80), autoFollow: false });
    await page.waitForSelector(".cm-content");
    const scroller = page.getByTestId("code-diff-review");

    // Park the viewport in the middle, then read the line at the top edge.
    await scroller.evaluate((el) => {
      el.scrollTop = 400;
    });
    const topBefore = await topVisibleLineText(scroller);
    expect(topBefore).not.toBe(""); // sanity: we actually sampled a line

    // Insert two lines ABOVE the fold — a naive pixel scroll would jump the view
    // down by two lines; the anchor must hold the same line at the top.
    await setDiff(page, { after: `PREPEND_A\nPREPEND_B\n${newLines(80)}` });

    // The anchored line holds its exact position (not shoved down, not PREPEND_A).
    await expect.poll(() => topVisibleLineText(scroller), { timeout: 3000 }).toBe(topBefore);
  });

  test("AC-4: scrolling up pauses follow (pill appears) and the pill re-engages it", async ({
    page,
  }) => {
    await gotoHarness(page);
    // Open the content as a new file (a fresh view, as the app mounts a diff),
    // not as a live edit of the harness's empty initial file: filling an empty
    // file IS a live edit, and auto-follow rightly chases it to the bottom.
    await setDiff(page, {
      before: oldLines(80),
      after: newLines(80),
      path: "follow.ts",
      autoFollow: true,
    });
    await page.waitForSelector(".cm-content");
    const scroller = page.getByTestId("code-diff-review");

    // Follow to the bottom on a first edit.
    await setDiff(page, { after: `${newLines(80)}\nTAIL1` });
    await expect.poll(() => scroller.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);

    // The user scrolls up — a real wheel gesture over the frame.
    await scroller.hover();
    await page.mouse.wheel(0, -400);

    // Paused → the "jump to latest" pill appears.
    await expect(page.getByTestId("code-diff-jump-to-latest")).toBeVisible();

    // Clicking it re-engages follow (pill clears).
    await page.getByTestId("code-diff-jump-to-latest").click();
    await expect(page.getByTestId("code-diff-jump-to-latest")).toBeHidden();
  });
});
