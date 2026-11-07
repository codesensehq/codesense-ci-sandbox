// SPDX-License-Identifier: MIT
import { expect, test, type Locator, type Page } from "@playwright/test";

/**
 * Real-browser guards for what pops up from a terminal tile's chrome: nothing
 * that opens from a control may cover that control (or its neighbours), so it
 * can always be closed the way it was opened, and the caption's name shows no
 * hover card at all.
 */

const BUG = "BUG-142";
const TAB = "sess-login-lead";

async function boot(page: Page) {
  await page.setViewportSize({ width: 1600, height: 950 });
  await page.goto("/?agents=team");
  await page.getByRole("button", { name: "Full screen" }).click();
  await page.getByTestId(`tile-zoom-${BUG}`).click();
  await expect(page.getByTestId(`terminal-grid-caption-hud-${TAB}`)).toBeVisible();
}

async function rect(l: Locator) {
  const b = await l.boundingBox();
  if (!b) throw new Error("no box");
  return b;
}

function overlaps(
  a: { x: number; y: number; width: number; height: number },
  b: { x: number; y: number; width: number; height: number },
) {
  return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
}

test.describe("tile chrome popovers", () => {
  test.beforeEach(async ({ page }) => boot(page));

  test("the story card opens below the caption, never over its toggle or the caption icons", async ({
    page,
  }) => {
    const toggle = page.getByTestId(`terminal-grid-caption-hud-${TAB}`);
    await toggle.click();
    const card = page.getByTestId(`story-hud-${BUG}`);
    await expect(card).toBeVisible();
    const c = await rect(card);
    const caption = await rect(page.getByTestId(`terminal-grid-caption-${TAB}`));
    expect(overlaps(c, await rect(toggle))).toBe(false);
    expect(c.y).toBeGreaterThanOrEqual(caption.y + caption.height);
    for (const icon of await page
      .getByTestId(`terminal-grid-caption-icons-${TAB}`)
      .locator("button")
      .all()) {
      expect(overlaps(c, await rect(icon))).toBe(false);
    }
    // The same toggle closes it.
    await toggle.click();
    await expect(card).toBeHidden();
  });

  test("the expanded story stays under the caption; its toggle still closes it", async ({
    page,
  }) => {
    const toggle = page.getByTestId(`terminal-grid-caption-hud-${TAB}`);
    await toggle.click();
    await page.getByTestId(`story-hud-expand-${BUG}`).click();
    const panel = page.getByTestId(`story-hud-expanded-${BUG}`);
    await expect(panel).toBeVisible();
    expect(overlaps(await rect(panel), await rect(toggle))).toBe(false);
    await toggle.click();
    await expect(panel).toBeHidden();
  });

  test("Esc closes the story card from inside it, focus back on its toggle", async ({ page }) => {
    // Out of zoom: while a tile is zoomed, its first Esc restores the zoom.
    await page.getByTestId(`tile-zoom-${BUG}`).click();
    const toggle = page.getByTestId(`terminal-grid-caption-hud-${TAB}`);
    await toggle.click();
    await page.getByTestId(`story-hud-expand-${BUG}`).focus();
    await page.keyboard.press("Escape");
    await expect(page.getByTestId(`story-hud-${BUG}`)).toBeHidden();
    await expect(toggle).toBeFocused();
  });

  test("hovering a caption's name shows no hover card", async ({ page }) => {
    await page.getByTestId(`terminal-grid-caption-focus-${TAB}`).hover();
    await page.waitForTimeout(900);
    await expect(page.getByRole("tooltip")).toHaveCount(0);
  });

  test("a caption icon's tooltip sits below the icon, never over it", async ({ page }) => {
    const icon = page.getByTestId(`terminal-grid-caption-inbox-${TAB}`);
    await icon.hover();
    const tip = page.getByRole("tooltip");
    await expect(tip).toBeVisible();
    const t = await rect(tip);
    const i = await rect(icon);
    expect(overlaps(t, i)).toBe(false);
    expect(t.y).toBeGreaterThanOrEqual(i.y + i.height - 1);
  });
});
