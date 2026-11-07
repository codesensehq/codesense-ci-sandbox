// SPDX-License-Identifier: MIT
import { AGENT_LIST_PANE } from "./agent-list-fixtures";
import { capturePaneErrors } from "./agent-list-console";
import { test, expect } from "./harness";
import { previewWorld } from "./agent-list-visual-fixtures";
for (const arrange of ["teams", "manager", "status"])
  test(`no runtime errors ${arrange}`, async ({ mountPane, page }, testInfo) => {
    const { errors, messages } = capturePaneErrors(page);
    const pane = await mountPane(
      process.env.AGP9_FIRST_PAINT_PANE || AGENT_LIST_PANE,
      previewWorld({ arrange }),
    );
    await expect(pane.frame.locator(".al-row").first()).toBeVisible();
    await pane.frame.evaluate(() => {
      (window as any).__errs = [];
      window.addEventListener("error", (e) => (window as any).__errs.push(e.message));
    });
    await pane.page.waitForTimeout(2500);
    await pane.frame.locator(".al-row").nth(3).hover();
    const frameErrors = await pane.frame.evaluate(() => (window as any).__errs);
    await testInfo.attach("console", {
      body: JSON.stringify({ errors, messages }),
      contentType: "application/json",
    });
    console.log(`ERRORS ${arrange}:`, JSON.stringify([...errors, ...frameErrors]));
    expect([...errors, ...frameErrors]).toEqual([]);
  });
