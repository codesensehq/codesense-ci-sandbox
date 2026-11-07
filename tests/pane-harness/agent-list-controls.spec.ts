// SPDX-License-Identifier: MIT
import { test, expect } from "./harness";
import { AGENT_LIST_PANE, agentListFixtures } from "./agent-list-fixtures";

const PANE = process.env.AGP9_CONTROL_PANE || AGENT_LIST_PANE;

for (const arrange of ["teams", "manager"])
  test(`pin has a stable hit area and toggles shared pins under live deltas in ${arrange}`, async ({
    mountPane,
  }) => {
    const fixtures = agentListFixtures(8, arrange);
    fixtures.width = 300;
    const pane = await mountPane(PANE, fixtures);
    const row = pane.frame.locator('[data-agent-id="s0"]');
    await row.locator(".al-main").focus();
    const pin = row.getByRole("button", { name: "Pin Agent 0", exact: true });
    await expect(pin).toBeVisible();
    const bounds = await pin.boundingBox();
    expect(bounds!.width).toBeGreaterThanOrEqual(20);
    expect(bounds!.height).toBeGreaterThanOrEqual(20);
    await pin.evaluate((el) => ((window as any).pinNode = el));
    await pane.page.mouse.move(bounds!.x + bounds!.width / 2, bounds!.y + bounds!.height / 2);
    await pane.page.mouse.down();
    for (let i = 0; i < 5; i++) {
      await pane.upsert("sessions", {
        ...fixtures.sessions![0],
        heartbeatAtMs: Date.now(),
        effectiveStatus: i % 2 ? "thinking" : "tool-running",
        progress: { note: `Delta ${i}` },
      });
      await pane.page.waitForTimeout(100);
    }
    expect(await pin.evaluate((el) => el === (window as any).pinNode)).toBe(true);
    await pane.page.mouse.up();
    const unpin = row.locator(".al-pin");
    expect(await unpin.evaluate((el) => el === (window as any).pinNode)).toBe(true);
    await expect(unpin).toHaveAttribute("aria-pressed", "true");
    await expect
      .poll(async () => (await pane.verbs()).filter((v) => v.verb === "setUiState").at(-1)?.args)
      .toMatchObject({
        topic: "ui:dock",
        patch: {
          pins: {
            "fixture:dev": [
              {
                kind: "session",
                teamKey: "fixture:dev",
                handle: "agent-0",
                sessionId: "s0",
                name: "Agent 0",
              },
            ],
          },
        },
      });
    await unpin.focus();
    await unpin.press("Space");
    await expect(row.getByRole("button", { name: "Pin Agent 0", exact: true })).toHaveAttribute(
      "aria-pressed",
      "false",
    );
    await expect
      .poll(async () => (await pane.verbs()).filter((v) => v.verb === "setUiState").at(-1)?.args)
      .toMatchObject({ patch: { pins: {} } });
  });

test("filter fits its frame and contains wheel scrolling at both boundaries", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(80);
  fixtures.height = 220;
  const state = fixtures.topics!.agentList.find((r) => r.rowType === "state")!;
  (state.pane as any).openTeams = Array.from({ length: 30 }, (_, i) => `fixture:team-${i}`);
  const pane = await mountPane(PANE, fixtures);
  await pane.frame.locator("#list").evaluate((el) => (el.scrollTop = 200));
  await pane.frame.locator("#al-filter").click();
  const menu = pane.frame.locator("#al-menu");
  const rect = await menu.evaluate((el) => {
    const r = el.getBoundingClientRect();
    return { top: r.top, bottom: r.bottom, height: innerHeight };
  });
  expect(rect.top).toBeGreaterThanOrEqual(4);
  expect(rect.bottom).toBeLessThanOrEqual(rect.height - 4);
  const before = await pane.frame.evaluate(() => ({
    list: document.querySelector("#list")!.scrollTop,
    page: (document.scrollingElement || document.documentElement).scrollTop,
  }));
  const bounds = await menu.boundingBox();
  await pane.page.mouse.move(bounds!.x + 30, bounds!.y + 30);
  await pane.page.mouse.wheel(0, 250);
  await expect.poll(() => menu.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
  for (const [edge, delta] of [
    ["bottom", 600],
    ["top", -600],
  ] as const) {
    await menu.evaluate((el, edge) => (el.scrollTop = edge === "top" ? 0 : el.scrollHeight), edge);
    await pane.page.mouse.wheel(0, delta);
    await pane.page.waitForTimeout(150);
    expect(
      await pane.frame.evaluate(() => ({
        list: document.querySelector("#list")!.scrollTop,
        page: (document.scrollingElement || document.documentElement).scrollTop,
      })),
    ).toEqual(before);
  }
});

test("clicking elsewhere in the app blurs the pane and closes its filter", async ({
  mountPane,
}) => {
  const pane = await mountPane(PANE, agentListFixtures(8));
  await pane.frame.locator("#al-filter").click();
  await expect(pane.frame.locator("#al-menu")).toBeVisible();
  await pane.page.locator("#opener").click();
  await expect(pane.frame.locator("#al-menu")).toHaveCount(0);
  await expect(pane.frame.locator("#al-filter")).toHaveAttribute("aria-expanded", "false");
});

test("Escape closes the filter and returns focus to its trigger", async ({ mountPane }) => {
  const pane = await mountPane(PANE, agentListFixtures(8));
  await pane.frame.locator("#al-filter").click();
  await pane.page.keyboard.press("Escape");
  await expect(pane.frame.locator("#al-menu")).toHaveCount(0);
  await expect(pane.frame.locator("#al-filter")).toBeFocused();
});

test("hiding the pane closes its filter before it is shown again", async ({ mountPane }) => {
  const pane = await mountPane(PANE, agentListFixtures(8));
  await pane.frame.locator("#al-filter").click();
  await pane.show(false);
  await expect(pane.frame.locator("#al-menu")).toHaveCount(0);
  await pane.show(true);
  await expect(pane.frame.locator("#al-filter")).toHaveAttribute("aria-expanded", "false");
});

test("pinned glyph unpins at narrow width without navigating the row", async ({ mountPane }) => {
  const fixtures = agentListFixtures(8);
  fixtures.width = 236;
  const state = fixtures.topics!.agentList.find((r) => r.rowType === "state")!;
  (state.pane as any).pinnedRefs = ["r0"];
  fixtures.topics!["ui:dock"][0].pins = {
    "fixture:dev": [
      {
        kind: "session",
        teamKey: "fixture:dev",
        handle: "agent-0",
        sessionId: "s0",
        name: "Agent 0",
      },
    ],
  };
  fixtures.sessions![0].pinned = true;
  const pane = await mountPane(PANE, fixtures);
  const copy = pane.frame.locator('[data-row-id="pinned:r0"]');
  const unpin = copy.getByRole("button", { name: "Unpin Agent 0", exact: true });
  await expect(unpin).toBeVisible();
  await expect(copy.locator(".al-pin")).toBeHidden();
  await unpin.click();
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "setUiState").at(-1)?.args)
    .toMatchObject({ patch: { pins: {} } });
  expect((await pane.verbs()).filter((v) => v.verb === "gotoAgent")).toEqual([]);
});

test("offline sessions never count as Working or needs-you and use Other", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(1, "status");
  Object.assign(fixtures.sessions![0], {
    state: "offline",
    effectiveStatus: "running",
    subagents: 2,
  });
  const pane = await mountPane(PANE, fixtures);
  await expect(pane.frame.locator('[data-agent-id="s0"] .al-state')).toHaveText("Offline");
  await expect(pane.frame.locator('#al-tally [data-status="working"]')).toHaveCount(0);
  await expect(pane.frame.getByText("Other", { exact: true })).toBeVisible();
  await pane.upsert("sessions", {
    ...fixtures.sessions![0],
    state: "offline",
    effectiveStatus: "paused",
    pendingAsk: "Approve the task?",
  });
  await expect(pane.frame.locator('#al-tally [data-status="needs"]')).toHaveCount(0);
  await expect(pane.frame.locator(".al-ask")).toHaveCount(0);
});

test("scroll-mounted pins keep Unpin over the glyph without a live delta", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(70, "teams");
  const refs = fixtures.topics!.agentList.filter((row) => row.sessionRef);
  const state = fixtures.topics!.agentList.find((row) => row.rowType === "state")!;
  (state.pane as { pinnedRefs: string[] }).pinnedRefs = refs.map((row) => row.id);
  fixtures.topics!["ui:dock"][0].pins = {
    "fixture:dev": fixtures.sessions!.map((session) => ({
      kind: "session",
      teamKey: "fixture:dev",
      handle: session.handle,
      sessionId: session.sessionId,
      name: (session.rowNames as { primary: string }).primary,
    })),
  };
  const pane = await mountPane(PANE, fixtures);
  await pane.frame.getByRole("button", { name: /\+65 more pinned/ }).click();
  const row = pane.frame.locator('[data-row-id="pinned:r40"]');
  await expect(row).toHaveCount(0);
  await pane.frame.locator("#list").evaluate((el) => (el.scrollTop = 1100));
  await expect(row).toBeVisible();
  await expect
    .poll(() =>
      row.evaluate((el) => {
        const button = el.querySelector(".al-unpin")!.getBoundingClientRect();
        const slot = el.querySelector(".al-pin-slot")!.getBoundingClientRect();
        return Math.max(
          Math.abs(button.x + button.width / 2 - slot.x - slot.width / 2),
          Math.abs(button.y + button.height / 2 - slot.y - slot.height / 2),
        );
      }),
    )
    .toBeLessThan(1);
  const unpin = row.locator(".al-unpin");
  await expect(unpin).toHaveAttribute("aria-label", "Unpin Agent 40");
  const bounds = (await unpin.boundingBox())!;
  expect(bounds.width).toBeGreaterThanOrEqual(20);
  expect(bounds.height).toBeGreaterThanOrEqual(20);
  await pane.page.mouse.click(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "setUiState").at(-1)?.args)
    .toMatchObject({
      patch: {
        pins: {
          "fixture:dev": expect.not.arrayContaining([
            expect.objectContaining({ sessionId: "s40" }),
          ]),
        },
      },
    });
  expect((await pane.verbs()).filter((v) => v.verb === "gotoAgent")).toEqual([]);
});

test("positive Manager subagent counts keep the row avatar and action targets", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(4, "manager");
  fixtures.height = 900;
  const pane = await mountPane(PANE, fixtures);
  const row = pane.frame.locator('[data-agent-id="s1"]');
  await expect(row.locator(".al-l3")).toHaveText("Working · 2 subagents");
  await row.evaluate((el) => {
    (window as any).managerNodes = [
      el,
      ...el.querySelectorAll(
        ".al-face img, img.al-face, .al-main, .al-quick, .al-messages, .al-pin",
      ),
    ];
  });
  for (const count of [3, 1]) {
    await pane.upsert("sessions", { ...fixtures.sessions![1], subagents: count });
    await expect(row.locator(".al-l3")).toHaveText(
      `Working · ${count} ${count === 1 ? "subagent" : "subagents"}`,
    );
    expect(
      await row.evaluate((el) => {
        const nodes = [
          el,
          ...el.querySelectorAll(
            ".al-face img, img.al-face, .al-main, .al-quick, .al-messages, .al-pin",
          ),
        ];
        return (
          nodes.length === (window as any).managerNodes.length &&
          nodes.every((node, i) => node === (window as any).managerNodes[i])
        );
      }),
    ).toBe(true);
  }
});

test("Run history visibly retains outcome kind and age metadata", async ({ mountPane }) => {
  const fixtures = agentListFixtures(4, "teams");
  fixtures.runs = [
    {
      outcome: {
        run_id: "inbox-sweep",
        story_id: "routine:fixture/dev/inbox-sweep@1",
        ended_at: new Date().toISOString(),
        artifact_ref: "output",
        summary: "Inbox cleared",
      },
      validated: "completed",
    },
  ] as never;
  const pane = await mountPane(PANE, fixtures);
  await pane.frame.locator("#al-history").click();
  const metadata = pane.frame.locator(".al-run .al-l2").first();
  await expect(metadata).toContainText("Ended · Routine ·");
  await expect(metadata).toBeVisible();
});

test("waiting lines and tooltips preserve literal identifiers and fenced commands", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(3, "teams");
  const question = "Use agent_list.ts or agent-list.ts?";
  fixtures.sessions![2].pendingAsk = question;
  const pane = await mountPane(PANE, fixtures);
  const row = pane.frame.locator('[data-agent-id="s2"]');
  for (const [input, literal] of [
    [question, question],
    ["Run this:\n```sh\npnpm test agent_list.ts\n```", "Run this: pnpm test agent_list.ts"],
    ["Use `agent_list.ts` and **keep** _literal_ text", "Use agent_list.ts and keep literal text"],
  ]) {
    await pane.upsert("sessions", { ...fixtures.sessions![2], pendingAsk: input });
    await expect(row.locator(".al-ask")).toContainText(literal);
    await expect(row.locator(".al-ask")).toHaveAttribute("title", expect.stringContaining(literal));
    await expect(row.locator(".al-main")).toHaveAttribute(
      "title",
      expect.stringContaining(literal),
    );
  }
});

test("a Ready radio press survives a count delta without replacing menu controls", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(4, "teams");
  const pane = await mountPane(PANE, fixtures);
  await pane.frame.locator("#al-filter").click();
  const menu = pane.frame.locator("#al-menu");
  const ready = menu.locator('[data-choice="show:idle"]');
  await menu.evaluate((el) => {
    (window as any).menuNodes = [el, ...el.querySelectorAll("button, input")];
  });
  const bounds = (await ready.boundingBox())!;
  await pane.page.mouse.move(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
  await pane.page.mouse.down();
  await pane.upsert("sessions", { ...fixtures.sessions![0], effectiveStatus: "idle" });
  await expect(ready.locator(".al-fn")).toHaveText("1");
  const sameNodes = await menu.evaluate((el) => {
    const nodes = [el, ...el.querySelectorAll("button, input")];
    return (
      nodes.length === (window as any).menuNodes.length &&
      nodes.every((node, i) => node === (window as any).menuNodes[i])
    );
  });
  await pane.page.mouse.up();
  await expect(ready).toHaveAttribute("aria-checked", "true");
  expect(sameNodes).toBe(true);
});

for (const trigger of ["pointer", "focus"] as const) {
  test(`tooltip ages refresh from the latest row on ${trigger} without deltas`, async ({
    mountPane,
  }) => {
    const fixtures = agentListFixtures(1, "teams");
    const now = Date.now();
    Object.assign(fixtures.sessions![0], {
      effectiveStatus: "stalled",
      startedAtMs: now - 2 * 60_000,
      activityAtMs: now - 60_000,
    });
    const pane = await mountPane(PANE, fixtures);
    const main = pane.frame.locator('[data-agent-id="s0"] .al-main');
    await expect(main).toHaveAttribute("title", /Developer · opus · 2m\nLast activity 1m ago/);
    const original = await main.elementHandle();
    await pane.upsert("sessions", {
      ...fixtures.sessions![0],
      startedAtMs: now - 3 * 60_000,
      activityAtMs: now - 2 * 60_000,
    });
    await expect(main).toHaveAttribute("title", /Developer · opus · 3m\nLast activity 2m ago/);
    expect(await main.evaluate((el, original) => el === original, original)).toBe(true);
    await pane.frame.evaluate(
      (advancedNow) => {
        Date.now = () => advancedNow;
      },
      now + 10 * 60_000,
    );
    if (trigger === "pointer") await main.hover();
    else await main.focus();
    await expect(main).toHaveAttribute("title", /Developer · opus · 13m\nLast activity 12m ago/);
    expect(await main.evaluate((el, original) => el === original, original)).toBe(true);
  });
}
