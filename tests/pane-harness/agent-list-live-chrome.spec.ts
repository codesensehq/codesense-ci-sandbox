// SPDX-License-Identifier: MIT
import { test, expect } from "./harness";
import { AGENT_LIST_PANE, agentListFixtures } from "./agent-list-fixtures";

test("tally filters keep keyboard focus through activation and live updates", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(5);
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const waiting = pane.frame.locator(".al-waiting");
  await waiting.focus();
  await pane.upsert("sessions", {
    ...fixtures.sessions![4],
    progress: { note: "Working on the pane" },
  });
  await expect(waiting).toBeFocused();
  await waiting.press("Enter");
  await expect(waiting).toHaveAttribute("aria-pressed", "true");
  await expect(waiting).toBeFocused();
});

test("all retained Manager workspace chips follow the selected workspace", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(20);
  fixtures.height = 1600;
  fixtures.topics!["ui:dock"][0].selected = "s0";
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const oldChip = pane.frame.locator('[data-agent-id="s3"] .al-wchip');
  const newChip = pane.frame.locator('[data-agent-id="s13"] .al-wchip');
  await expect(oldChip).toHaveAttribute("data-current", "");
  await expect(newChip).not.toHaveAttribute("data-current");
  await pane.writeUi({ selected: "s10" });
  await expect(oldChip).not.toHaveAttribute("data-current");
  await expect(newChip).toHaveAttribute("data-current", "");
});

for (const arrange of ["teams", "manager"]) {
  test(`held role chips stay inside their row in ${arrange}`, async ({ mountPane }) => {
    const fixtures = agentListFixtures(5, arrange);
    fixtures.sessions![0].roleHoldings = [{ name: "builder", label: "Builder" }];
    const pane = await mountPane(AGENT_LIST_PANE, fixtures);
    await expect(pane.frame.locator('[data-agent-id="s0"]')).toBeVisible();
    const spills = await pane.frame.locator(".al-role:visible").evaluateAll(
      (chips) =>
        chips.filter((chip) => {
          const row = chip.closest(".al-item")!.getBoundingClientRect(),
            bounds = chip.getBoundingClientRect();
          return bounds.bottom > row.bottom || bounds.top < row.top;
        }).length,
    );
    expect(spills).toBe(0);
    if (arrange === "manager") await expect(pane.frame.locator(".al-role")).toBeVisible();
  });
}

test("status sections and tallies keep active parents working and crashed separate", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(3, "status");
  Object.assign(fixtures.sessions![0], { effectiveStatus: "idle", subagents: 2 });
  Object.assign(fixtures.sessions![1], { effectiveStatus: "crashed", subagents: 0 });
  fixtures
    .topics!.agentList.filter((r) => r.sessionRef)
    .forEach((r, i) => (r.statusGroup = [2, 0, 0][i]));
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  await expect(pane.frame.locator(".al-section")).toHaveText(["Needs you", "Working", "Other"]);
  const grouped = await pane.frame.locator(".al-item").evaluateAll((items) => {
    let section = "";
    const result: Record<string, string> = {};
    for (const item of items) {
      section = item.querySelector(".al-section")?.textContent || section;
      const row = item.querySelector<HTMLElement>(".al-row");
      if (row) result[row.dataset.agentId!] = section;
    }
    return result;
  });
  expect(grouped).toEqual({ s0: "Working", s1: "Other", s2: "Needs you" });
  await expect(pane.frame.locator('#al-tally button[data-status="idle"]')).toHaveCount(0);
});

test("a live update between tally pointer press and release preserves its click", async ({
  mountPane,
}) => {
  const fixtures = agentListFixtures(5);
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const waiting = pane.frame.locator(".al-waiting");
  const bounds = (await waiting.boundingBox())!;
  await pane.page.mouse.move(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
  await pane.page.mouse.down();
  await waiting.evaluate((el) => ((window as any).pressedTally = el));
  await pane.upsert("sessions", {
    ...fixtures.sessions![4],
    progress: { note: "A live heartbeat" },
  });
  await expect.poll(() => waiting.evaluate((el) => (window as any).pressedTally === el)).toBe(true);
  await pane.page.mouse.up();
  await expect(waiting).toHaveAttribute("aria-pressed", "true");
});

test("Manager pins keep their compact height when they hold roles", async ({ mountPane }) => {
  const fixtures = agentListFixtures(3, "manager");
  fixtures.sessions![0].roleHoldings = [{ name: "builder", label: "Builder" }];
  fixtures.topics!.agentList.find((r) => r.rowType === "state")!.pane = { pinnedRefs: ["r0"] };
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const pin = pane.frame.locator('[data-row-id="pinned:r0"]');
  await expect(pin).toBeVisible();
  expect((await pin.boundingBox())!.height).toBe(28);
  await expect(pin.locator(".al-holds")).toBeHidden();
  await expect(pin.locator(".al-main")).toHaveAttribute("aria-label", /Holds: builder/);
});
