// SPDX-License-Identifier: MIT
import { expect, test } from "../pane-harness/harness";
import { AGENT_LIST_PANE, agentListFixtures } from "../pane-harness/agent-list-fixtures";

test("the dock pane receives team changes without reminting", async ({ mountPane }) => {
  const pane = await mountPane(AGENT_LIST_PANE, agentListFixtures(5));
  await expect(pane.frame.locator("button").first()).toBeVisible();
  const mints = await pane.page.evaluate(() => (window as any).mints);
  await pane.frame.evaluate(() => {
    (window as any).slotContexts = [];
    window.codesense.onSlotContext((context) => (window as any).slotContexts.push(context));
  });
  await pane.setTeam("fixture", "ops");
  await expect
    .poll(() => pane.frame.evaluate(() => (window as any).slotContexts.at(-1)?.team?.team))
    .toBe("ops");
  expect(await pane.page.evaluate(() => (window as any).mints)).toBe(mints);
});

const PANE = "codesense-docs/getting-started/panes/explain-this-surface.html";
for (const [file, explanation] of [
  ["inbox-flows.html", /inbox-flows\.html/i],
  ["notes.md", /markdown doc/i],
  ["diagram.excalidraw", /Excalidraw canvas/i],
] as const) {
  test(`the docs pane explains the host's selected ${file}`, async ({ mountPane }) => {
    const pane = await mountPane(PANE, {
      activeTab: { "fixture:dev": `/fixture/${file}` },
    });
    await expect(pane.frame.locator(".pane__why")).toContainText(explanation);
  });
}

test("the glossary preserves a typed lookup across maximize and focus context pushes", async ({
  mountPane,
}) => {
  const pane = await mountPane("codesense-docs/getting-started/panes/glossary-lookup.html", {
    activeTab: { "fixture:dev": "/fixture/worktree.html" },
  });
  await pane.frame.waitForLoadState("load");
  await pane.page.waitForTimeout(1_200);
  const term = pane.frame.locator(".defn__term h3");
  const search = pane.frame.locator("#glossary-search");
  await expect(term).toHaveText("worktree");
  // Enter the pane first, letting the host's focus-in choose its initial control.
  await pane.page.locator("iframe").focus();
  await expect(pane.frame.locator(".pane__collapse")).toBeFocused();
  await search.fill("hydration");
  await expect(term).toHaveText("hydration");
  await pane.maximize(true);
  await expect(pane.page.getByTestId("overlay-strip")).toBeVisible();
  // Allow the host's slot-context push and the pane's animation frames to render.
  await pane.page.waitForTimeout(600);
  await expect(term).toHaveText("hydration");
  await pane.maximize(false);
  await search.focus();
  await pane.page.waitForTimeout(600);
  await expect(term).toHaveText("hydration");
  // A different selected document with the same basename still reapplies context.
  await pane.page.evaluate(() => {
    (window as any).viewer.setState({
      activeTab: { "fixture:dev": "/fixture/other/worktree.html" },
    });
  });
  await expect(term).toHaveText("worktree");
  await pane.page.evaluate(() => {
    (window as any).viewer.setState({ activeTab: { "fixture:dev": "/fixture/story.html" } });
  });
  await expect(term).toHaveText("story");
});
