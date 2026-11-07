// SPDX-License-Identifier: MIT
import { test, expect } from "./harness";
import { AGENT_LIST_PANE, agentListFixtures } from "./agent-list-fixtures";
const PANE = process.env.AGP9_FIRST_PAINT_PANE || AGENT_LIST_PANE;

test("saved dock choices restore on reload before any agent frame paints", async ({
  page,
  mountPane,
}) => {
  const fixtures = agentListFixtures(8, "teams");
  fixtures.height = 1000;
  const refs = fixtures.topics!.agentList;
  for (const ref of refs.filter((r) => r.sessionRef).slice(4)) ref.teamKey = "fixture:ops";
  const metadata = refs.find((r) => r.rowType === "state")!;
  metadata.teamOrder = ["fixture:dev", "fixture:ops"];
  metadata.teamNames = {
    "fixture:dev": { org: "Fixture", team: "dev" },
    "fixture:ops": { org: "Fixture", team: "ops" },
  };
  fixtures.sessions![3].effectiveStatus = "awaiting-answer";
  fixtures.sessions![3].needsYouKind = "question";
  const pane = await mountPane(PANE, fixtures);
  await pane.frame.locator("#al-arrange").selectOption("manager");
  await pane.frame.locator("#al-search").fill("Agent");
  await pane.frame.locator(".al-waiting").click();
  await pane.writeUi({
    folds: { "#manager-fold:fixture:dev|agent-0": true },
    hiddenTeams: ["fixture:ops"],
    pins: {
      "fixture:dev": [
        {
          kind: "session",
          teamKey: "fixture:dev",
          sessionId: "s3",
          handle: "agent-3",
          name: "Agent 3",
        },
      ],
    },
  });
  await pane.flush();
  const savedState = () =>
    pane.page.evaluate(() => {
      const writes = (window as any).calls.filter(([name]: [string]) => name === "save_app_state");
      return writes.length ? JSON.parse(writes.at(-1)[1].state) : null;
    });
  await expect
    .poll(
      async () =>
        (await savedState())?.liveHubUi?.[JSON.stringify(["ui:dock", "all"])]?.data.hiddenTeams,
    )
    .toEqual(["fixture:ops"]);
  const saved = await savedState();
  const dock = saved.liveHubUi[JSON.stringify(["ui:dock", "all"])].data;
  expect(dock).toMatchObject({
    arrange: "manager",
    search: { query: "Agent", status: "needs" },
    folds: { "#manager-fold:fixture:dev|agent-0": true },
    hiddenTeams: ["fixture:ops"],
    pins: { "fixture:dev": [expect.objectContaining({ sessionId: "s3" })] },
  });
  fixtures.topics!["ui:dock"] = [{ ...dock, id: "ui:dock" }];
  (metadata.pane as Record<string, unknown>).pinnedRefs = ["r3"];
  await page.route("**/pane/**", async (route) => {
    const response = await route.fetch();
    const initial = `<script>window.__codesenseFirstPaint=${JSON.stringify({ topic: "agentList", rev: 1, snapshot: fixtures.topics!.agentList }).replaceAll("<", "\\u003c")};</script>`;
    const source = (initial + (await response.text())).replace(
      "const cs = window.codesense,",
      `
      const subscribe = window.codesense.subscribe;
      window.codesense.subscribe = async (topic, ...args) => {
        if (topic === 'ui:dock') await new Promise(resolve => setTimeout(resolve, 150));
        return subscribe(topic, ...args);
      };
      const cs = window.codesense,`,
    );
    const body = source.replace(
      "</body>",
      `<script>
      window.paintedViews = [];
      function sampleView() {
        const rows = [...document.querySelectorAll('.al-row')];
        if (rows.length) window.paintedViews.push({
          arrange: document.querySelector('#al-arrange').value,
          query: document.querySelector('#al-search').value,
          ids: rows.map(r => r.dataset.agentId),
          pinned: !!document.querySelector('[data-row-id="pinned:r3"]'),
          fold: document.querySelector('[data-agent-id="s0"] .al-fold')?.getAttribute('aria-expanded'),
        });
        requestAnimationFrame(sampleView);
      }
      requestAnimationFrame(sampleView);
    </script></body>`,
    );
    await route.fulfill({ response, body });
  });
  const restored = await mountPane(PANE, fixtures);
  await expect(restored.frame.locator('[data-row-id="pinned:r3"]')).toBeVisible();
  await expect
    .poll(() => restored.frame.evaluate(() => (window as any).paintedViews.length))
    .toBeGreaterThan(2);
  const painted = await restored.frame.evaluate(() => (window as any).paintedViews);
  for (const view of painted) {
    expect(view.arrange).toBe("manager");
    expect(view.query).toBe("Agent");
    expect(view.pinned).toBe(true);
    expect(view.ids.length).toBeGreaterThan(0);
    expect(view.ids).toEqual(["s3", "s0", "s1", "s2", "s3"]);
  }
  await restored.writeUi({ search: { query: "", status: "all" } });
  await expect(restored.frame.locator('[data-agent-id="s0"] .al-fold')).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  await expect(restored.frame.locator('[data-agent-id="s1"]')).toHaveCount(0);
});
