// SPDX-License-Identifier: MIT
import { inboxFixtureRow } from "../../src/app/mock/inbox-fixtures";
import { expect, test } from "./harness";

test("Inbox keeps the configured fixture team in its directory through the real bridge", async ({
  mountPane,
  page,
}) => {
  const pane = await mountPane("src/features/shell/right-pane/__fixtures__/inbox-pane.html", {
    inbox: [inboxFixtureRow(1, "Fixture mail")],
  });
  const context = await pane.frame.evaluate(() => (window as any).codesense.context());
  expect(context.inboxTeams).toEqual([{ org: "fixture", team: "dev" }]);
  await expect(pane.frame.locator(".ib-row .prev")).toContainText("Fixture mail");
  expect(
    await page.evaluate(() =>
      (window as any).__TAURI_INTERNALS__.invoke("check_path_exists", {
        path: "/fixture/dev/content",
      }),
    ),
  ).toBe(true);
  expect(
    await page.evaluate(() =>
      (window as any).__TAURI_INTERNALS__.invoke("check_path_exists", {
        path: "/fixture/not-synced/content",
      }),
    ),
  ).toBe(false);
});
