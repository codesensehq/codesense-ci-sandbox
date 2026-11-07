// SPDX-License-Identifier: MIT
import { expect, test, type Page } from "@playwright/test";

/**
 * SW-XS-27: the scope row's "N unsaved" counter and its list, in a real browser
 * (`?nav=demo&edits=demo`): it fits the scope row beside the GitHub button, the
 * card opens beside the nav with each workspace's files and plain words, Discard
 * says what it does inline, View shows both versions side by side with the title
 * clear of the close button, and Publish takes a file off the count.
 *
 * `SHOTS=<dir>` also saves the screenshots the PR shows (light and dark).
 */
const SHOTS = process.env.SHOTS;

async function boot(page: Page, theme: "light" | "dark") {
  await page.setViewportSize({ width: 1400, height: 860 });
  await page.emulateMedia({ colorScheme: theme });
  // The app's light/dark axis (next-themes' stored choice), set before it boots.
  await page.addInitScript((t) => window.localStorage.setItem("theme", t), theme);
  await page.goto("/?nav=demo&edits=demo");
  await expect(page.locator("html")).toHaveClass(new RegExp(`\\b${theme}\\b`));
  await expect(page.getByTestId("unsaved-team-edits")).toBeVisible();
  // The viewer (and its left nav) has its own palette: the light one is "confluence".
  if (theme === "light") {
    await page.waitForFunction(() => Boolean(window.__stores?.useViewerStore));
    await page.evaluate(() =>
      window.__stores!.useViewerStore.getState().updateSettings({ viewerTheme: "confluence" }),
    );
  }
}

for (const theme of ["light", "dark"] as const) {
  test(`unsaved team edits: counter, list, discard confirm, view (${theme})`, async ({ page }) => {
    await boot(page, theme);
    const button = page.getByTestId("unsaved-team-edits");
    await expect(button).toHaveText(/5 unsaved/);
    await expect(page.getByTestId("unsaved-conflicts")).toHaveText("1");
    // It sits on the scope row, inside it, and doesn't push the GitHub button out.
    const row = await page.getByTestId("left-nav-scope-row").boundingBox();
    const box = await button.boundingBox();
    expect(
      box && row && box.y >= row.y - 0.5 && box.y + box.height <= row.y + row.height + 0.5,
    ).toBe(true);
    if (SHOTS)
      await page
        .getByTestId("left-nav-scope-row")
        .screenshot({ path: `${SHOTS}/xs27-row-${theme}.png` });

    await button.click();
    const card = page.getByTestId("unsaved-team-edits-card");
    await expect(card).toBeVisible();
    await expect(card).toContainText("Unsaved team edits");
    await expect(card).toContainText("Changed here and in the team since it was copied");
    await expect(card).toContainText("machine offline · last count 2h ago");
    await expect(card).toContainText("goes to your private files");
    await expect(card).not.toContainText(/reverse_sync|hydrate_|0123456789ab/);
    if (SHOTS) await card.screenshot({ path: `${SHOTS}/xs27-card-${theme}.png` });

    // Discard says what it will do, inline; Cancel leaves it.
    const conflictRow = card.locator('[data-testid="team-edit-row"][data-kind="conflict"]');
    await conflictRow.getByRole("button", { name: "Discard" }).click();
    await expect(conflictRow).toContainText(
      "Throw away the changes to CLAUDE.md here and use the team's version?",
    );
    if (SHOTS) await card.screenshot({ path: `${SHOTS}/xs27-discard-${theme}.png` });
    await expect(conflictRow.getByRole("button", { name: "Cancel" })).toBeFocused();
    await page.keyboard.press("Escape");
    await expect(card).toBeVisible();
    await expect(conflictRow).not.toContainText("Throw away the changes");

    // View: both versions side by side, the title clear of the close button.
    await card
      .locator('[data-testid="team-edit-row"][data-kind="conflict"]')
      .getByRole("button", { name: "View" })
      .click();
    const diff = page.getByTestId("team-edit-diff");
    await expect(diff).toBeVisible();
    await expect(diff).toContainText("Both changed since the file was copied");
    await expect(diff.getByTestId("team-edit-diff-sides")).toContainText("Team's version now");
    await expect(diff.getByTestId("team-edit-diff-sides")).toContainText("SW-RM-4");
    const title = await diff.getByRole("heading").boundingBox();
    const close = await diff.getByRole("button", { name: "Close" }).boundingBox();
    expect(title && close && title.x + title.width <= close.x + 0.5).toBe(true);
    await expect(diff.locator(".cm-editor").first()).toBeVisible();
    if (SHOTS) await page.screenshot({ path: `${SHOTS}/xs27-view-${theme}.png` });
    await page.keyboard.press("Escape");
    // The list closed behind the dialog: focus is back on the counter, not lost.
    await expect(page.getByTestId("unsaved-team-edits")).toBeFocused();
  });
}

test("an action takes a file off the count; an offline machine says why", async ({ page }) => {
  await boot(page, "light");
  await page.getByTestId("unsaved-team-edits").click();
  const card = page.getByTestId("unsaved-team-edits-card");
  const remoteSection = card.getByTestId("team-edits-workspace").filter({ hasText: "SW-RM-4" });
  const offline = card.getByTestId("team-edits-workspace").filter({ hasText: "SW-AGR-7" });
  await expect(offline).toContainText("The machine is offline");
  await expect(remoteSection).toContainText("on a machine · checked 12m ago");
  const local = card.getByTestId("team-edits-workspace").filter({ hasText: "SW-BSI-9" });
  // This Mac's workspace acts as the app's own request: no terminal needed.
  await expect(local.getByRole("button", { name: "Publish 3" })).toBeEnabled();
  // A machine workspace's Discard goes through and takes the file off the count.
  const conflictRow = remoteSection.locator('[data-testid="team-edit-row"]');
  await conflictRow.getByRole("button", { name: "Discard" }).click();
  await conflictRow.getByRole("button", { name: "Discard" }).click();
  await expect(page.getByTestId("unsaved-team-edits")).toHaveText(/4 unsaved/);
});

for (const theme of ["light", "dark"] as const) {
  test(`missing machine folder can be forgotten without file actions (${theme})`, async ({
    page,
  }) => {
    await boot(page, theme);
    await page.goto("/?nav=demo&edits=missing");
    const counter = page.getByTestId("unsaved-team-edits");
    await expect(counter).toHaveAttribute(
      "aria-label",
      /5 unsaved team edits.*1 workspace folder gone/,
    );
    await counter.click();
    const card = page.getByTestId("unsaved-team-edits-card");
    const missing = card.getByTestId("team-edits-workspace").filter({ hasText: "SW-AN-55-P5" });
    await expect(missing).toContainText("folder no longer exists on codesense-test-1");
    await expect(missing).toContainText("its 3 unsaved team edits went with the folder");
    await expect(missing.getByRole("button", { name: "Check now" })).toHaveCount(0);
    await expect(missing.getByTestId("team-edit-row")).toHaveCount(0);
    if (SHOTS) await card.screenshot({ path: `${SHOTS}/missing-folder-${theme}.png` });
    await missing.getByRole("button", { name: "Remove from list" }).click();
    await expect(missing).toHaveCount(0);
    await expect(
      page.getByText("Removed SW-AN-55-P5 from the list. Nothing on codesense-test-1 was deleted."),
    ).toBeVisible();
    await expect(card.getByText("SW-BSI-9", { exact: true })).toBeVisible();
  });
}
