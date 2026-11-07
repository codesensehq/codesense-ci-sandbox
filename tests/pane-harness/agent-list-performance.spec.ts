// SPDX-License-Identifier: MIT
import { expect, test } from "./harness";
import { AGENT_LIST_PANE, agentListFixtures } from "./agent-list-fixtures";

test("50 sessions: event-to-paint p95 under 150ms; no hidden fetches", async ({ mountPane }) => {
  const fixtures = agentListFixtures(50);
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const { frame } = pane;
  await expect(frame.locator('[data-agent-id="s3"]')).toBeVisible();
  await frame.evaluate(() => {
    const w = window as any;
    w.latencies = [];
    w.hiddenFetches = 0;
    const fetch = window.fetch;
    window.fetch = (...args) => {
      w.hiddenFetches++;
      return fetch(...args);
    };
    window.addEventListener("message", (event) => {
      if (
        event.source === window.parent &&
        event.data?.type === "codesense:delta" &&
        event.data.topic === "sessions"
      )
        w.arrived = performance.now();
    });
    new MutationObserver(() => {
      const text = document.querySelector('[data-agent-id="s3"] .al-name')?.textContent;
      if (text !== w.lastText && w.arrived) {
        w.lastText = text;
        requestAnimationFrame(() => {
          w.latencies.push(performance.now() - w.arrived);
          w.arrived = 0;
        });
      }
    }).observe(document.getElementById("list")!, {
      subtree: true,
      childList: true,
      characterData: true,
    });
  });
  for (let i = 0; i < 20; i++) {
    await pane.upsert("sessions", {
      ...fixtures.sessions![3],
      rowNames: { primary: `Update ${i}`, full: `Update ${i}`, secondary: "Developer" },
    });
    await expect(frame.locator('[data-agent-id="s3"] .al-name')).toHaveText(`Update ${i}`);
    await expect.poll(() => frame.evaluate(() => (window as any).latencies.length)).toBe(i + 1);
  }
  const measurements = await frame.evaluate(() => (window as any).latencies as number[]);
  expect(measurements.length).toBeGreaterThanOrEqual(19);
  const p95 = measurements.sort((a, b) => a - b)[Math.ceil(measurements.length * 0.95) - 1];
  console.log(
    `[agent-list] 50 sessions, samples=${measurements.length}, event→paint p95=${p95.toFixed(1)}ms`,
  );
  expect(p95).toBeLessThan(150);
  await pane.show(false);
  for (let i = 0; i < 5; i++)
    await pane.upsert("sessions", { ...fixtures.sessions![3], displayName: `Hidden ${i}` });
  expect(await frame.evaluate(() => (window as any).hiddenFetches)).toBe(0);
  await pane.show(true);
});
