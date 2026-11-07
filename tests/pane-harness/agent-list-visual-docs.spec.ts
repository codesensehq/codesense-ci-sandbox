// SPDX-License-Identifier: MIT
import { test, expect } from "./harness";
import { AGENT_LIST_PANE, agentListCaptureFixtures } from "./agent-list-fixtures";

test("capture the Agents dock for its docs figure", async ({
  browserName,
  mountPane,
}, testInfo) => {
  test.skip(browserName !== "chromium", "Docs screenshot uses Chromium");
  const fixtures = agentListCaptureFixtures();
  fixtures.width = 236;
  fixtures.height = 700;
  fixtures.sessions![1].subagents = 2;
  fixtures.sessions![2].pendingAsk = "Which branch should I use?";
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const vars = await pane.page.evaluate(async () => {
    const { themeTokens } = await import("/src/themes/index.ts");
    return themeTokens("codesense", "dark");
  });
  await pane.setTheme({ id: "codesense", scheme: "dark", vars });
  await expect(pane.frame.locator('[data-agent-id="s1"] .al-state')).toContainText("Working");
  await expect(pane.frame.locator('[data-agent-id="s2"] .al-ask')).toContainText("Waiting on you");
  await pane.frame.evaluate(() => document.fonts.ready);
  await pane.page.mouse.move(0, 0);
  await pane.page
    .locator("iframe")
    .first()
    .screenshot({
      path: testInfo.outputPath("agent-list.png"),
      animations: "disabled",
    });
});
