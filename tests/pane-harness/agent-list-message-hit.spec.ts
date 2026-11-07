// SPDX-License-Identifier: MIT
import { test, expect } from "./harness";
import { AGENT_LIST_PANE, agentListFixtures } from "./agent-list-fixtures";

const PANE = process.env.AGP9_BADGE_PANE || AGENT_LIST_PANE;
for (const arrange of ["teams", "manager"])
  for (const width of [236, 268])
    test(`badge centre opens Messages without pinning in ${arrange} at ${width}px`, async ({
      mountPane,
    }) => {
      const fixtures = agentListFixtures(8, arrange);
      fixtures.width = width;
      fixtures.inbox![0].needsYouBySender = { "agent-0": 2 };
      const pane = await mountPane(PANE, fixtures);
      const row = pane.frame.locator('[data-agent-id="s0"]');
      const badge = row.getByRole("button", {
        name: "2 messages need you: open messages",
        exact: true,
      });
      await expect(badge).toBeVisible();
      const before = (await badge.boundingBox())!;
      const centre = { x: before.x + before.width / 2, y: before.y + before.height / 2 };
      await pane.page.mouse.move(centre.x, centre.y);
      await pane.upsert("sessions", {
        ...fixtures.sessions![0],
        effectiveStatus: "tool-running",
        progress: { note: "Still working" },
      });
      expect(await badge.boundingBox()).toEqual(before);
      await expect(badge).toBeVisible();
      // A real mouse click exercises hit testing after hover activates the toolbar.
      await pane.page.mouse.click(centre.x, centre.y);
      await expect(pane.page.getByTestId("agent-inbox-chat")).toBeVisible();
      expect(
        (await pane.verbs()).filter((v) => v.verb === "setUiState" && (v.args as any).patch?.pins),
      ).toEqual([]);
    });
