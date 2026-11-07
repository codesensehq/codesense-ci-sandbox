// SPDX-License-Identifier: MIT
// SV-AGP-9: a status that flips tool-running <-> thinking every 100ms must not flash the row.
import { test, expect } from "./harness";
import { AGENT_LIST_PANE, agentListCaptureFixtures } from "./agent-list-fixtures";
test(`flip builtin: row node stays and the word holds`, async ({ mountPane }) => {
  const fixtures = { ...agentListCaptureFixtures(), width: 236, height: 700 };
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const { frame } = pane;
  await expect(frame.locator('[data-agent-id="s3"]')).toBeVisible();
  await frame.locator('[data-agent-id="s3"] .al-main').hover();
  // Watch the row: node replacements, image replacements, and the visible state word per frame.
  await frame.evaluate(() => {
    const w = window as any;
    w.__flip = { rows: 0, imgs: 0, words: [] as string[] };
    const row = () => document.querySelector('[data-agent-id="s3"]');
    w.__flipRow = row();
    w.__flipImg = row()?.querySelector("img");
    const tick = () => {
      const r = row();
      if (r && r !== w.__flipRow) {
        w.__flip.rows++;
        w.__flipRow = r;
      }
      const img = r?.querySelector("img");
      if (img && img !== w.__flipImg) {
        w.__flip.imgs++;
        w.__flipImg = img;
      }
      const word = (r?.querySelector(".al-state, .al-task")?.textContent || "").trim();
      if (w.__flip.words[w.__flip.words.length - 1] !== word) w.__flip.words.push(word);
      if (!w.__flipStop) requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
  const base = fixtures.sessions!.find((s: any) => s.id === "s3")!;
  for (let i = 0; i < 20; i++) {
    await pane.upsert("sessions", {
      ...base,
      effectiveStatus: i % 2 ? "thinking" : "tool-running",
    });
    await pane.page.waitForTimeout(100);
  }
  await pane.page.waitForTimeout(400);
  const result = await frame.evaluate(() => {
    (window as any).__flipStop = true;
    return (window as any).__flip;
  });
  console.log(
    `FLIP builtin: row nodes replaced=${result.rows} avatar imgs replaced=${result.imgs} word changes=${result.words.length - 1} words=${JSON.stringify(result.words.slice(0, 8))}`,
  );
  expect(result.rows).toBe(0);
  expect(result.imgs).toBe(0);
  expect(result.words.length - 1).toBeLessThanOrEqual(1);
});

test("distinct state words hold before changing and eventually show the final state", async ({
  mountPane,
}) => {
  const fixtures = agentListCaptureFixtures();
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  await expect(pane.frame.locator('[data-agent-id="s3"] .al-state')).toHaveText("Working");
  await pane.frame.evaluate(() => {
    const w = window as any;
    w.wordChanges = [];
    let previous = "";
    const sample = () => {
      const word = document.querySelector('[data-agent-id="s3"] .al-state')?.textContent;
      if (word !== previous) {
        w.wordChanges.push({ word, at: performance.now() });
        previous = word || "";
      }
      if (!w.stopWords) requestAnimationFrame(sample);
    };
    sample();
  });
  for (let i = 0; i < 12; i++) {
    await pane.upsert("sessions", {
      ...fixtures.sessions![3],
      effectiveStatus: i % 2 ? "running" : "idle",
    });
    await pane.page.waitForTimeout(100);
  }
  await pane.upsert("sessions", { ...fixtures.sessions![3], effectiveStatus: "idle" });
  await expect(pane.frame.locator('[data-agent-id="s3"] .al-state')).toHaveText("Ready");
  const changes = await pane.frame.evaluate(() => {
    (window as any).stopWords = true;
    return (window as any).wordChanges as { word: string; at: number }[];
  });
  expect(changes.length).toBeGreaterThan(2);
  for (let i = 2; i < changes.length; i++)
    expect(changes[i].at - changes[i - 1].at).toBeGreaterThanOrEqual(280);
});
