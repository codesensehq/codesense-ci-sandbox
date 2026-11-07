// SPDX-License-Identifier: MIT
import { test, expect } from "./harness";
import { AGENT_LIST_PANE } from "./agent-list-fixtures";
import { capturePaneErrors } from "./agent-list-console";
import { previewWorld } from "./agent-list-visual-fixtures";

const target = process.env.AGP9_FIRST_PAINT_PANE || AGENT_LIST_PANE;
for (const arrange of ["teams", "manager", "status"]) {
  test(`first-paint roster boots without errors in ${arrange}`, async ({
    page,
    mountPane,
  }, testInfo) => {
    const world = previewWorld({ arrange });
    Object.assign(world.sessions[0], {
      roleHoldings: [{ role: "builder", name: "Builder", label: "Builder" }],
    });
    world.topics.agentList.find((r: any) => r.rowType === "state")!.pane.pinnedRefs.push("r0");
    const { errors, messages } = capturePaneErrors(page);
    await page.route("**/pane/**", async (route) => {
      const response = await route.fetch();
      const snapshot = { topic: "agentList", rev: 1, snapshot: world.topics.agentList };
      const body =
        `<script>window.__codesenseFirstPaint=${JSON.stringify(snapshot)};</script>` +
        (await response.text());
      await route.fulfill({ response, body });
    });
    const pane = await mountPane(target, world);
    await expect(pane.frame.locator(".al-row").first()).toBeVisible();
    await pane.page.waitForTimeout(3500);
    await testInfo.attach("console-errors", {
      body: JSON.stringify({ errors, messages }),
      contentType: "application/json",
    });
    expect(errors).toEqual([]);
    await expect(pane.frame.locator(".al-row").first()).toBeVisible();
  });
}
