// SPDX-License-Identifier: MIT
import { test, expect } from "./harness";
import { INBOX_PANE, messageViewFixtures } from "./inbox-message-view-fixtures";
import { DEFAULT_FIXTURES, themeCheck } from "../conformance/checks";

test("Inbox follows live themes and catches up after hiding without reloading", async ({
  mountPane,
}) => {
  test.setTimeout(90_000);
  const pane = await mountPane(
    "src/features/shell/right-pane/__fixtures__/inbox-pane.html",
    DEFAULT_FIXTURES,
  );
  await themeCheck.run(pane);
});

test("Inbox conversation contrast passes in every bundled light and dark theme", async ({
  mountPane,
}) => {
  test.setTimeout(90_000);
  const pane = await mountPane(INBOX_PANE, messageViewFixtures(false));
  await pane.frame.locator(".ib-row").click();
  await expect(pane.frame.locator(".rx-chip")).toHaveAttribute("aria-label", "Reacted 👍 by you");
  await themeCheck.run(pane);
});
