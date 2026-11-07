// SPDX-License-Identifier: MIT
import { expect, test, type Page } from "@playwright/test";

/**
 * Real-browser layout guards for a terminal tab's body:
 *
 *  - Each tab's agent inbox pane opens INSIDE that tab's own space (its grid
 *    cell, or the tab shown one at a time), starting exactly under that tab's
 *    caption and running to the tile's bottom — never one shared pane at the
 *    tile's edge. Two open cells show two panes, one in each.
 *  - The terminal is SIZED to what is left (tile − tab bar − caption − pane),
 *    never taller than the tile: a terminal box that overflows its tile has its
 *    bottom rows (a TUI's status line) cut off, and a focused terminal scrolls
 *    the tile up until its tab bar slides under the workspace pills.
 *
 * Runs against the frontend mock backend (`?agents=team` world):
 * `npm run test:e2e:mock` starts its own mock vite (see
 * playwright.mock.config.ts; `PORT=<p>` picks the port).
 */

const BUG = "BUG-142";
const AGENT_TAB = "sess-login-lead";
const OTHER_TAB = "sess-login-repro";
const MANAGER_TAB = "sess-manager";
const SHELL_TAB = "demo-shell";
const CONTENT_WS = "generic:/mock/users/4242/orgs/acme/widgets/default/content";

async function boot(page: Page) {
  await page.setViewportSize({ width: 1600, height: 950 });
  await page.goto("/?agents=team");
  await page.getByRole("button", { name: "Full screen" }).click();
  await expect(page.getByTestId(`terminal-grid-caption-inbox-${AGENT_TAB}`)).toBeVisible();
}

async function expand(page: Page, wsId: string) {
  await page.getByTestId(`tile-zoom-${wsId}`).click();
  await expect(page.getByTestId(`tile-zoom-${wsId}`)).toHaveAttribute("aria-label", /^Back to/);
}

async function showAllTabs(page: Page, wsId: string) {
  await page.getByTestId(`tile-grid-toggle-${wsId}`).click();
  await expect(page.getByTestId(`tile-grid-toggle-${wsId}`)).toHaveAttribute(
    "aria-pressed",
    "true",
  );
}

async function openInbox(page: Page, tabId: string) {
  await page.getByTestId(`terminal-grid-caption-inbox-${tabId}`).click();
  const pane = page.locator(paneOf(tabId));
  await expect(pane).toBeVisible();
  return pane;
}

async function box(page: Page, selector: string) {
  const b = await page.locator(selector).first().boundingBox();
  if (!b) throw new Error(`${selector} has no box`);
  return b;
}

const byTestId = (id: string) => `[data-testid="${id}"]`;
/** A tab's body under its caption: its terminal and, when open, its docked inbox. */
const regionOf = (tabId: string) => byTestId(`inbox-split-${tabId}`);
/** The inbox docked in that tab's own body. */
const paneOf = (tabId: string) => `${regionOf(tabId)} [data-testid="inbox-rail"]`;

async function paneGeometry(page: Page, tabId: string, wsId: string) {
  return {
    caption: await box(page, byTestId(`terminal-grid-caption-${tabId}`)),
    pane: await box(page, paneOf(tabId)),
    region: await box(page, regionOf(tabId)),
    tile: await box(page, byTestId(`workspace-panels-${wsId}`)),
  };
}

function expectPaneInItsTab(g: Awaited<ReturnType<typeof paneGeometry>>) {
  // Under its own caption, to the tile's bottom.
  expect(Math.abs(g.pane.y - (g.caption.y + g.caption.height))).toBeLessThanOrEqual(1);
  expect(Math.abs(g.pane.y + g.pane.height - (g.tile.y + g.tile.height))).toBeLessThanOrEqual(1);
  // Inside its own tab's region (so inside its cell), flush right.
  expect(g.pane.x).toBeGreaterThanOrEqual(g.region.x - 1);
  expect(Math.abs(g.pane.x + g.pane.width - (g.region.x + g.region.width))).toBeLessThanOrEqual(1);
}

/** The shown terminal's slot and xterm screen fit inside the tile, and the
 *  tile's tab bar is on screen below the window's top chrome. */
async function expectTerminalFitsTile(page: Page, tabId: string, wsId: string) {
  const geo = await page.evaluate(
    ({ tabId, wsId }) => {
      const rect = (el: Element | null) => el?.getBoundingClientRect() ?? null;
      const region = document.querySelector(`[data-testid="inbox-split-${tabId}"]`);
      const slot = region?.querySelector(".cs-terminal-display") ?? null;
      const frames = [...document.querySelectorAll(`[data-pane-id="${wsId}"]`)].filter(
        (f) => f.getBoundingClientRect().height > 0,
      );
      const scrolled: number[] = [];
      for (let n = frames[0] ?? null; n; n = n.parentElement) {
        if (n.scrollTop !== 0) scrolled.push(n.scrollTop);
      }
      return {
        tile: rect(document.querySelector(`[data-testid="workspace-panels-${wsId}"]`)),
        region: rect(region),
        slot: rect(slot),
        tabBar: rect(frames[0]?.querySelector('[role="tablist"]') ?? null),
        scrolled,
      };
    },
    { tabId, wsId },
  );
  expect(geo.tile && geo.region).toBeTruthy();
  expect(geo.region!.bottom).toBeLessThanOrEqual(geo.tile!.bottom + 1);
  if (geo.slot) expect(geo.slot.bottom).toBeLessThanOrEqual(geo.tile!.bottom + 1);
  if (geo.tabBar) expect(geo.tabBar.top).toBeGreaterThanOrEqual(0);
  expect(geo.scrolled).toEqual([]);
}

test.describe("a tab's agent inbox pane lives in that tab", () => {
  test.beforeEach(async ({ page }) => boot(page));

  test("one tab at a time · agent tab", async ({ page }) => {
    await expand(page, BUG);
    await openInbox(page, AGENT_TAB);
    expectPaneInItsTab(await paneGeometry(page, AGENT_TAB, BUG));
    await expectTerminalFitsTile(page, AGENT_TAB, BUG);
  });

  test("one tab at a time · agent in the team's content workspace", async ({ page }) => {
    await expand(page, CONTENT_WS);
    await showAllTabs(page, CONTENT_WS);
    await openInbox(page, MANAGER_TAB);
    expectPaneInItsTab(await paneGeometry(page, MANAGER_TAB, CONTENT_WS));
    await expectTerminalFitsTile(page, MANAGER_TAB, CONTENT_WS);
  });

  test("a plain shell has no inbox: only a tab with a live agent offers one", async ({ page }) => {
    await expand(page, CONTENT_WS);
    await showAllTabs(page, CONTENT_WS);
    await expect(page.getByTestId(`terminal-grid-caption-inbox-${MANAGER_TAB}`)).toBeVisible();
    await expect(page.getByTestId(`terminal-grid-caption-inbox-${SHELL_TAB}`)).toHaveCount(0);
  });

  test("tab grid · two cells open → two panes, each in its own cell", async ({ page }) => {
    await expand(page, BUG);
    await showAllTabs(page, BUG);
    await openInbox(page, AGENT_TAB);
    await openInbox(page, OTHER_TAB);
    await expect(page.getByTestId("inbox-rail")).toHaveCount(2);
    for (const tab of [AGENT_TAB, OTHER_TAB]) {
      const cell = page.getByTestId(`terminal-grid-cell-${tab}`);
      await expect(cell.getByTestId("inbox-rail")).toHaveCount(1);
      expectPaneInItsTab(await paneGeometry(page, tab, BUG));
    }
  });

  test("narrow tile: the docked pane stays inside its tab, still under the caption", async ({
    page,
  }) => {
    await openInbox(page, AGENT_TAB);
    expectPaneInItsTab(await paneGeometry(page, AGENT_TAB, BUG));
    await expectTerminalFitsTile(page, AGENT_TAB, BUG);
  });
});

test.describe("the shown terminal is sized to its tile", () => {
  test.beforeEach(async ({ page }) => boot(page));

  test("one tab at a time: caption + terminal fit the tile, nothing scrolled", async ({ page }) => {
    await expand(page, BUG);
    await expectTerminalFitsTile(page, AGENT_TAB, BUG);
    // Focusing the terminal (a newly opened tab's autofocus) scrolls nothing.
    await page.locator(`${regionOf(AGENT_TAB)} .xterm textarea`).focus();
    await expectTerminalFitsTile(page, AGENT_TAB, BUG);
  });
});
