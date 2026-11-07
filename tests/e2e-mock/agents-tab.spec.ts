// SPDX-License-Identifier: MIT
import { mkdirSync } from "node:fs";

import { expect, test, type Page } from "@playwright/test";

/**
 * The redesigned Agents tab (SV-AGP-13) in a real browser, against the frontend mock backend
 * (`?scenario=onboarded&agents=team`, whose Triage has two live sessions and a spend cap): the list, an agent's Overview, Running, Edit with the Model field,
 * the launch window, New agent, Schedule and Roles, in light and dark. Each stop saves a
 * screenshot to `test-results/agp13/` (or `AGP13_SHOTS`) for the design review.
 *
 * `CI=1 PORT=<free port> pnpm test:e2e:mock tests/e2e-mock/agents-tab.spec.ts`
 */

const SHOTS = process.env.AGP13_SHOTS ?? "test-results/agp13";

async function openAgentsTab(page: Page, scheme: "light" | "dark") {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.addInitScript((t) => localStorage.setItem("theme", t), scheme);
  await page.goto("/?scenario=onboarded&agents=team");
  // The menu can swallow the first click while the app is still booting: retry the opening.
  await expect(async () => {
    await page.getByTestId("identity-menu-trigger").click();
    await expect(page.getByTestId("identity-menu-agents")).toBeVisible({ timeout: 2_000 });
  }).toPass({ timeout: 30_000 });
  await page.getByTestId("identity-menu-agents").click();
  await expect(page.getByTestId("agents-tab")).toBeVisible();
  // The team world also fills the bottom agents pane; hide it so the tab has the window.
  await page.waitForFunction(() => !!window.__stores?.useViewerStore);
  await page.evaluate(() => window.__stores!.useViewerStore.setState({ terminalVisible: false }));
  await expect(page.getByTestId("agent-row").first()).toBeVisible();
}

async function shot(page: Page, scheme: string, name: string) {
  mkdirSync(SHOTS, { recursive: true });
  await page.screenshot({ path: `${SHOTS}/${scheme}-${name}.png` });
}

for (const scheme of ["light", "dark"] as const) {
  test(`the Agents tab, ${scheme}`, async ({ page }) => {
    await openAgentsTab(page, scheme);

    // The list: Mine first, then each team; every row reads runtime, model and account.
    const list = page.getByTestId("agent-list");
    await expect(list).toBeVisible();
    const triage = page.locator('[data-testid="agent-row"][data-agent-key$="/triage"]');
    await expect(triage.getByTestId("agent-row-line")).toContainText(/Claude/);
    await shot(page, scheme, "1-list");

    // Overview: the profile header, its four facts and the fact sheet, spend meter included.
    await triage.getByRole("button", { name: /^Open / }).click();
    await expect(page.getByTestId("agent-overview")).toBeVisible();
    await expect(page.getByTestId("agent-strip-runs-on")).toBeVisible();
    await expect(page.getByTestId("agent-fact-tools")).toBeVisible();
    await expect(
      page.getByTestId("agent-fact-spend").getByRole("meter", { name: "Spend today" }),
    ).toBeVisible();
    // No account select at the bottom, and no banners between the facts and the routines.
    await expect(page.getByTestId("agent-account-select")).toHaveCount(0);
    await expect(page.getByTestId("agent-overview").getByRole("alert")).toHaveCount(0);
    await shot(page, scheme, "2-overview");

    // The strip's Change opens the account picker over the cell.
    await page.getByTestId("agent-strip-account-action").click();
    await expect(page.getByTestId("agent-account-menu")).toBeVisible();
    await page.waitForTimeout(250);
    await shot(page, scheme, "2b-account-picker");
    await page.keyboard.press("Escape");

    // Running: this agent's live sessions, with the tab itself selected.
    await page.getByRole("tab", { name: /^Running/ }).click();
    await expect(page.getByRole("tab", { name: /^Running/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    await expect(page.getByRole("tab", { name: "Overview" })).toHaveAttribute(
      "aria-selected",
      "false",
    );
    await expect(page.getByTestId("agent-running-view")).toBeVisible();
    await expect(page.getByTestId("agent-instance").first()).toBeVisible();
    await page.waitForTimeout(150);
    await shot(page, scheme, "3-running");

    // The running count is a link: it stays visible and clickable while the row is hovered.
    await triage.hover();
    await expect(triage.getByTestId("agent-running-action")).toBeVisible();
    await expect(triage.getByTestId("agent-running")).toBeHidden();
    await expect(triage.getByTestId("agent-row-launch")).toBeVisible();
    await page.mouse.move(700, 600);

    // Edit: sections, the Model field and the save bar's count.
    await page.getByRole("tab", { name: "Edit" }).click();
    const editor = page.getByTestId("agent-editor");
    await expect(editor).toBeVisible();
    const model = editor.getByLabel("Model");
    // Extra args takes text one key at a time: the space in "--effort high" survives typing.
    const args = editor.getByLabel("Args");
    await args.pressSequentially("--effort high");
    await expect(args).toHaveValue("--effort high");
    await model.fill("opus-test");
    await expect(page.getByTestId("agent-savebar-msg")).toContainText("1 unsaved change");
    // The save bar and the section nav stay in view while the form scrolls.
    const scroller = page.getByTestId("agent-detail-scroll");
    await scroller.evaluate((el) => el.scrollTo(0, 300));
    const viewport = page.viewportSize()!;
    const bar = await page.getByTestId("agent-savebar").boundingBox();
    expect(bar!.y + bar!.height).toBeLessThanOrEqual(viewport.height);
    expect(bar!.y).toBeGreaterThan(0);
    const nav = await page.getByTestId("agent-editor-nav").boundingBox();
    expect(nav!.y).toBeGreaterThanOrEqual(0);
    expect(nav!.y).toBeLessThan(viewport.height / 2);
    await scroller.evaluate((el) => el.scrollTo(0, 0));
    await shot(page, scheme, "4-edit-model");
    await page.getByRole("button", { name: "Discard" }).click();
    await expect(page.getByTestId("agent-savebar-msg")).not.toContainText("unsaved");

    // The launch window: one place, with Branch or worktree and the account that pays.
    await page.getByRole("tab", { name: "Overview" }).click();
    await page.getByTestId("agent-detail-launch").click();
    const sheet = page.getByTestId("agent-launch-sheet");
    await expect(sheet).toBeVisible();
    await expect(sheet.getByTestId("launch-account")).toBeVisible();
    // The header says who is being launched: the agent's face and "Launch <name>".
    const launchHeader = sheet.getByTestId("launch-header");
    await expect(launchHeader.getByTestId("launch-header-face")).toBeVisible();
    await expect(launchHeader.getByRole("heading")).toContainText("Launch ");
    await launchHeader.screenshot({ path: `${SHOTS}/${scheme}-5a-launch-header.png` });
    await sheet.getByTestId("launch-where-branch").click();
    await expect(sheet.getByTestId("launch-target-kind-newWorktree")).toBeVisible();
    await shot(page, scheme, "5-launch-branch");
    await page.keyboard.press("Escape");
    await expect(sheet).toBeHidden();

    // New agent: who can use it, then a template or a blank agent.
    await page.getByRole("button", { name: "New agent" }).click();
    const dialog = page.getByTestId("new-agent-dialog");
    await expect(dialog).toBeVisible();
    await expect(dialog.getByTestId("new-agent-blank")).toBeVisible();
    await shot(page, scheme, "6-new-agent");
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();

    // Schedule.
    await page.getByTestId("agents-view-schedule").click();
    await expect(page.getByTestId("schedule-view")).toBeVisible();
    await shot(page, scheme, "7-schedule");

    // Roles.
    await page.getByTestId("agents-view-roles").click();
    await expect(page.getByTestId("roles-view")).toBeVisible();
    await expect(page.getByTestId("role-editor")).toBeVisible();
    await shot(page, scheme, "8-roles");
  });
}
