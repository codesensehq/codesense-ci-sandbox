// SPDX-License-Identifier: MIT
import { inboxFixtureRow } from "../../src/app/mock/inbox-fixtures";
import { expect, test, type PaneFixtures } from "./harness";

const INBOX = "src/features/shell/right-pane/__fixtures__/inbox-pane.html";
const session = {
  id: "mia",
  sessionId: "mia",
  handle: "mia",
  displayName: "Mia",
  rowNames: { primary: "Mia", secondary: null, full: "Mia" },
  state: "idle",
  alive: true,
  team: { org: "fixture", team: "dev" },
};
const fixtures: PaneFixtures = {
  width: 860,
  height: 680,
  sessions: [session],
  inbox: [inboxFixtureRow(1, "Ready for your review", { from: "mia", fromName: "Mia" })],
};

test("Inbox opens a thread with a single click", async ({ mountPane }) => {
  const pane = await mountPane(INBOX, fixtures);
  await pane.frame.locator(".ib-row .prev").click();
  await expect(pane.frame.locator("#thread-name")).toHaveText("Mia");
  await expect(pane.frame.locator("#log .ib-msg")).toContainText("Ready for your review");
});

test("Inbox keeps a single click through a sessions snapshot between mouse down and up", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane(INBOX, fixtures);
  await pane.focus();
  const row = pane.frame.locator(".ib-row .prev");
  await expect(row).toBeVisible();
  // Observe the real subscription so mouseup waits for the intervening snapshot to arrive.
  await pane.frame.evaluate(async () => {
    const w = window as any;
    await w.codesense.subscribe("sessions", {
      onChange: (view: any) => {
        w.clickSnapshotSeen = view.snapshot.some((s: any) => s.state === "busy");
      },
    });
  });
  const box = (await row.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await pane.upsert("sessions", { ...session, state: "busy" });
  await expect.poll(() => pane.frame.evaluate(() => (window as any).clickSnapshotSeen)).toBe(true);
  await page.mouse.up();
  await expect(pane.frame.locator("#thread-name")).toHaveText("Mia");
  await expect(pane.frame.locator("#log .ib-msg")).toContainText("Ready for your review");
});

test("Inbox opens on the first click while the host has focus", async ({ mountPane, page }) => {
  const pane = await mountPane(INBOX, fixtures);
  await expect(pane.frame.locator(".ib-row")).toBeVisible();
  await page.locator("#opener").focus();
  await expect(page.locator("#opener")).toBeFocused();
  await pane.frame.locator(".ib-row .prev").click();
  await expect(pane.frame.locator("#thread-name")).toHaveText("Mia");
  await expect(pane.frame.locator("#log .ib-msg")).toContainText("Ready for your review");
});
