// SPDX-License-Identifier: MIT
import { AGENT_LIST_PANE } from "./agent-list-fixtures";
// SV-AGP-9: one needs-you predicate. Collapsed counts equal the rows that show the needs-you treatment,
// and "N waiting" filters to exactly those rows. World: blocked, paused, awaiting-answer, errored, crashed.
import { test, expect } from "./harness";
import { previewWorld } from "./agent-list-visual-fixtures";
const LIVE = AGENT_LIST_PANE;
test("needs-you counts equal needs-you rows", async ({ mountPane }) => {
  const w = previewWorld({ arrange: "teams" });
  w.topics["ui:dock"][0].folds = {};
  w.topics.agentList.find((r: any) => r.rowType === "state").pane.pinnedRefs = [];
  const pane = await mountPane(LIVE, { ...w, height: 2400 });
  const { frame } = pane;
  await expect(frame.locator(".al-row").first()).toBeVisible();
  const expanded = await frame.evaluate(() => {
    const byTeam: Record<string, number> = {},
      byWs: Record<string, number> = {},
      wsShown: Record<string, number> = {};
    let team = "",
      ws = "",
      total = 0;
    for (const item of document.querySelectorAll(".al-item")) {
      if (item.classList.contains("al-team"))
        team = item.querySelector(".al-team-name")!.textContent!;
      const card = item.querySelector(".al-ws-card");
      if (card) {
        ws = card.querySelector(".al-ws-name")!.textContent!;
        const hot = [...card.querySelectorAll(".al-ws-tail .al-hot")].find((h) =>
          h.querySelector(
            '[data-mark="question"], [data-mark="permission"], [data-mark="blocked"]',
          ),
        );
        wsShown[ws] = hot ? Number(hot.textContent!.match(/(\d+)\s*$/)![1]) : 0;
      }
      const row = item.querySelector(".al-row[data-waiting]");
      if (row) {
        byTeam[team] = (byTeam[team] || 0) + 1;
        byWs[ws] = (byWs[ws] || 0) + 1;
        total++;
        if (!row.querySelector(".al-ask")?.textContent?.startsWith("Waiting on you"))
          throw new Error("needs-you row without its ask line: " + row.textContent);
      }
    }
    return {
      byTeam,
      byWs,
      wsShown,
      total,
      chip: Number((document.querySelector(".al-waiting")?.textContent || "0").match(/\d+/)?.[0]),
    };
  });
  console.log("NEEDS expanded", JSON.stringify(expanded));
  expect(expanded.chip).toBe(expanded.total);
  for (const [ws, n] of Object.entries(expanded.byWs)) expect(expanded.wsShown[ws], ws).toBe(n);
  for (const ws of Object.keys(expanded.wsShown))
    expect(expanded.wsShown[ws], ws).toBe(expanded.byWs[ws] || 0);
  // Collapse each team: its heading count equals its expanded needs-you rows.
  for (const [team, n] of Object.entries(expanded.byTeam)) {
    await frame.locator(".al-team-head", { hasText: team }).click();
    const shown = Number(
      (
        (await frame
          .locator(".al-team", { hasText: team })
          .locator(".al-team-need")
          .textContent()) || "0"
      ).match(/(\d+)\s*$/)![1],
    );
    console.log(`NEEDS team ${team}: folded=${shown} rows=${n}`);
    expect(shown).toBe(n);
    await frame.locator(".al-team-head", { hasText: team }).click();
  }
  // "N waiting" filters to exactly the needs-you rows.
  await frame.locator(".al-waiting").click();
  const filtered = await frame.evaluate(() => {
    const rows = [...document.querySelectorAll(".al-row")];
    return {
      rows: rows.length,
      waiting: rows.filter((r) => r.hasAttribute("data-waiting")).length,
    };
  });
  console.log("NEEDS filtered", JSON.stringify(filtered));
  expect(filtered.rows).toBe(expanded.total);
  expect(filtered.waiting).toBe(expanded.total);
});

// Unread needs-you MESSAGES: an amber pill on the row (and a badge on Messages while hovered).
// It opens the agent's inbox chat and never adds to a needs-you count.
test("needs-you messages badge a Ready row without changing counts", async ({ mountPane }) => {
  const counts = async (frame: any) =>
    frame.evaluate(() => ({
      chip: (document.querySelector(".al-waiting")?.textContent || "").replace(/\D/g, ""),
      waitingRows: document.querySelectorAll(".al-row[data-waiting]").length,
      wsHot: [...document.querySelectorAll(".al-ws-tail .al-hot")]
        .map((h) => h.textContent)
        .join(","),
    }));
  const plain = await mountPane(LIVE, { ...previewWorld({ arrange: "teams" }), height: 2400 });
  await expect(plain.frame.locator(".al-row").first()).toBeVisible();
  const before = await counts(plain.frame);
  const w = previewWorld({ arrange: "teams" });
  w.inbox[0].needsYouBySender = { "release-notes": 2 };
  const pane = await mountPane(LIVE, { ...w, height: 2400 });
  const { frame } = pane;
  const row = frame
    .locator(".al-row", { has: frame.locator(".al-name", { hasText: /^Release notes$/ }) })
    .first();
  await expect(row).toBeVisible();
  const pill = row.locator(".al-msgs");
  await expect(pill).toHaveText("2");
  await expect(pill).toHaveAttribute("aria-label", "2 messages need you: open messages");
  await expect(row.locator(".al-state")).toHaveText("Ready");
  expect(await counts(frame)).toEqual(before);
  await frame.evaluate(() => {
    const cs = (window as any).codesense;
    cs.showPopover = (a: any) => {
      (window as any).__pop = a;
      return Promise.resolve();
    };
  });
  // Mouse: Messages and the unread pill keep separate stable hit areas on hover.
  await row.locator(".al-main").hover();
  const messages = row.locator(".al-messages");
  await expect(messages).toHaveAttribute("data-count", "2");
  await messages.click();
  let pop = await frame.evaluate(() => (window as any).__pop);
  console.log("MSGS hover click", JSON.stringify(pop));
  expect(pop.kind).toBe("agent-inbox-chat");
  // Keyboard: the pill itself is a button.
  await frame.evaluate(() => ((window as any).__pop = null));
  await pill.focus();
  await pane.page.keyboard.press("Enter");
  pop = await frame.evaluate(() => (window as any).__pop);
  console.log("MSGS pill key", JSON.stringify(pop));
  expect(pop.kind).toBe("agent-inbox-chat");
  await row.evaluate((el) => ((window as any).__messageRow = el));
  await pane.upsert("inbox", { ...w.inbox[0], needsYouBySender: { "release-notes": 5 } });
  await expect(pill).toHaveText("5");
  expect(await row.evaluate((el) => (window as any).__messageRow === el)).toBe(true);
  expect(await counts(frame)).toEqual(before);
  await pane.upsert("inbox", { ...w.inbox[0], needsYouBySender: {} });
  await expect(row.locator(".al-msgs")).toHaveCount(0);
  await expect(messages).not.toHaveAttribute("data-count");
  expect(await counts(frame)).toEqual(before);
});
