// SPDX-License-Identifier: MIT
import { expect, test } from "./harness";
import { AGENT_LIST_PANE, agentListFixtures } from "./agent-list-fixtures";
import type { Page } from "@playwright/test";

async function admitLaunchTeams(page: Page, orgs: string[], teams: string[]) {
  await page.evaluate(
    async ({ orgs, teams }) => {
      const modulePath = "/src/shared/stores/org-store.ts";
      const { useOrgStore } = await import(modulePath);
      useOrgStore.setState({
        orgs: orgs.map((slug) => ({ slug, teams: teams.map((name) => ({ name })) })),
      });
    },
    { orgs, teams },
  );
}

for (const gesture of ["Enter", "Space", "q", "Shift+Q", "Alt-click", "m", "Shift+M", "Messages"]) {
  test(`${gesture} opens the host popover and closes to the row`, async ({ mountPane }) => {
    const fixtures = agentListFixtures(3);
    const pane = await mountPane(AGENT_LIST_PANE, fixtures);
    const row = pane.frame.locator('[data-agent-id="s0"] .al-main');
    await row.focus();
    if (gesture === "Alt-click") await row.click({ modifiers: ["Alt"] });
    else if (gesture === "Messages") {
      await row.hover();
      const messages = pane.frame.getByRole("button", { name: "Messages for Agent 0" });
      await messages.focus();
      await pane.upsert("sessions", {
        ...fixtures.sessions![0],
        progress: { phase: "dev", note: "Updated while focused" },
      });
      await expect(pane.frame.locator('[data-agent-id="s0"] .al-main')).toHaveAttribute(
        "title",
        /Updated while focused/,
      );
      await expect(messages).toBeFocused();
      await messages.click();
    } else await row.press(gesture);
    const inbox = gesture === "m" || gesture === "Shift+M" || gesture === "Messages";
    const popup = pane.page.getByTestId(inbox ? "agent-inbox-chat" : "agent-quick-send");
    await expect(popup).toBeVisible();
    expect(await pane.verbs()).toContainEqual({
      verb: "showPopover",
      args: expect.objectContaining({
        kind: inbox ? "agent-inbox-chat" : "agent-quick-send",
        sessionId: "s0",
      }),
    });
    await pane.page.keyboard.press("Escape");
    await expect(popup).toBeHidden();
    await expect(row).toBeFocused();
  });
}

test("offline rows retain native gestures", async ({ mountPane }) => {
  const fixtures = agentListFixtures(1);
  fixtures.sessions![0].state = "offline";
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  await pane.setTeam("fixture", "dev");
  const row = pane.frame.locator('[data-agent-id="s0"] .al-main');
  await row.press("Enter");
  await row.press("Space");
  await row.press("q");
  await row.click({ modifiers: ["Alt"] });
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "gotoAgent"))
    .toHaveLength(2);
  expect((await pane.verbs()).filter((v) => v.verb === "showPopover")).toEqual([]);
  await row.press("m");
  await expect(pane.page.getByTestId("agent-inbox-chat")).toBeVisible();
});

test("team and workspace Launch use the existing launcher", async ({ mountPane }) => {
  const fixtures = agentListFixtures(3, "teams");
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  await admitLaunchTeams(pane.page, ["fixture"], ["dev"]);
  const state = fixtures.topics!.agentList.find((row) => row.rowType === "state")!;
  const teamLaunch = pane.frame.getByRole("button", { name: "Launch in dev", exact: true });
  await teamLaunch.focus();
  await pane.upsert("agentList", {
    ...state,
    teamNames: { "fixture:dev": { org: "Updated org", team: "dev" } },
  });
  await expect(pane.frame.locator(".al-team-org")).toHaveText("Updated org");
  await expect(teamLaunch).toBeFocused();
  const workspaceLaunch = pane.frame.getByRole("button", { name: "Launch in Work 0", exact: true });
  await workspaceLaunch.focus();
  await pane.upsert("sessions", {
    ...fixtures.sessions![0],
    rowNames: { primary: "Renamed lead", full: "Renamed lead" },
  });
  await expect(pane.frame.locator('[data-agent-id="s0"] .al-name')).toHaveText("Renamed lead");
  await expect(workspaceLaunch).toBeFocused();
  await pane.setTeam("fixture", "dev");
  await pane.frame.getByRole("button", { name: "Launch agent", exact: true }).click();
  await pane.frame.getByRole("button", { name: "Launch in dev", exact: true }).click();
  await pane.frame.getByRole("button", { name: "Launch in Work 0", exact: true }).click();
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "openLauncher"))
    .toEqual([
      { verb: "openLauncher", args: { org: "fixture", team: "dev", task: "" } },
      { verb: "openLauncher", args: { org: "fixture", team: "dev", task: "" } },
      { verb: "openLauncher", args: { org: "fixture", team: "dev", task: "" } },
    ]);
});

test("Launch keeps row targets and lets the header use the active team", async ({ mountPane }) => {
  const fixtures = agentListFixtures(1, "teams");
  fixtures.slotId = "dock";
  fixtures.sessions![0].team = { org: "beta", team: "ops" };
  const rows = fixtures.topics!.agentList;
  rows[0].teamKey = "beta:ops";
  const state = rows.find((row) => row.rowType === "state")!;
  Object.assign(state, {
    homeTeam: "acme:ops",
    teamOrder: ["acme:ops", "beta:ops"],
    teamNames: {
      "acme:ops": { org: "Acme", team: "ops" },
      "beta:ops": { org: "Beta", team: "ops" },
    },
  });
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  await admitLaunchTeams(pane.page, ["fixture", "acme", "beta"], ["dev", "ops"]);
  await pane.setTeam("acme", "ops");
  await pane.frame.getByRole("button", { name: "Launch in ops", exact: true }).click();
  await pane.frame.getByRole("button", { name: "Launch in Work 0", exact: true }).click();
  const sheetRequest = () =>
    pane.page.evaluate(async () => {
      const modulePath = "/src/features/agents/lib/launch-sheet.ts";
      const { useLaunchSheetStore } = await import(modulePath);
      return useLaunchSheetStore.getState().request;
    });
  await expect.poll(sheetRequest).toEqual({ teamKey: "beta:ops", task: "" });
  const closeSheet = () =>
    pane.page.evaluate(async () => {
      const modulePath = "/src/features/agents/lib/launch-sheet.ts";
      const { useLaunchSheetStore } = await import(modulePath);
      useLaunchSheetStore.getState().close();
    });
  await closeSheet();
  await pane.setTeam("beta", "ops");
  expect(await pane.frame.evaluate(() => (window as any).codesense.context())).toMatchObject({
    org: "fixture",
    team: "dev",
    activeTeam: { org: "beta", team: "ops" },
  });
  await pane.frame.getByRole("button", { name: "Launch agent", exact: true }).click();
  await expect
    .poll(async () => (await pane.verbs()).filter((v) => v.verb === "openLauncher"))
    .toEqual([
      { verb: "openLauncher", args: { org: "beta", team: "ops", task: "" } },
      { verb: "openLauncher", args: { org: "beta", team: "ops", task: "" } },
      { verb: "openLauncher", args: { org: "beta", team: "ops", task: "" } },
    ]);
  await expect.poll(sheetRequest).toEqual({ teamKey: "beta:ops", task: "" });
  await closeSheet();

  await pane.setTeam("", "");
  await pane.frame.getByRole("button", { name: "Launch agent", exact: true }).click();
  await expect(pane.frame.locator("#al-move-notice")).toHaveText("unavailable: no team selected");
  expect(await sheetRequest()).toBeNull();
  expect((await pane.verbs()).filter((v) => v.verb === "openLauncher")).toHaveLength(3);

  await admitLaunchTeams(pane.page, ["fixture", "acme"], ["dev", "ops"]);
  await pane.setTeam("beta", "ops");
  await pane.frame.getByRole("button", { name: "Launch agent", exact: true }).click();
  await expect(pane.frame.locator("#al-move-notice")).toHaveText(
    "openLauncher: unknown or unadmitted team beta:ops",
  );
  expect(await sheetRequest()).toBeNull();
});

test("two skeleton cards remain until the first snapshot", async ({ page, mountPane }) => {
  await page.route("**/pane/**", async (route) => {
    const response = await route.fetch();
    const body = (await response.text()).replace(
      "const cs = window.codesense,",
      `
      const snapshotGate = new Promise(resolve => window.releaseSnapshot = resolve);
      const subscribe = window.codesense.subscribe;
      window.codesense.subscribe = async (...args) => { await snapshotGate; return subscribe(...args); };
      const cs = window.codesense,`,
    );
    await route.fulfill({ response, body });
  });
  const pane = await mountPane(AGENT_LIST_PANE, agentListFixtures(3));
  const loading = pane.frame.getByRole("status", { name: "Loading agents" });
  await expect(loading).toBeVisible();
  await expect(loading.locator(".al-skeleton-card")).toHaveCount(2);
  await pane.frame.evaluate(() => (window as any).releaseSnapshot());
  await expect(pane.frame.locator('[data-agent-id="s0"]')).toBeVisible();
  await expect(loading).toHaveCount(0);
});

test("236px Workspace keeps its title, real faces and needs tally at the approved density", async ({
  mountPane,
}, testInfo) => {
  const fixtures = agentListFixtures(8, "teams");
  fixtures.width = 236;
  fixtures.height = 700;
  fixtures.sessions![2].pendingAsk = null;
  fixtures.sessions![0].avatar = "builtin-funny:sysadmin-goblin";
  fixtures.sessions![1].avatar = "builtin-funny:coffee-bot";
  fixtures.sessions![1].rowNames = fixtures.sessions![0].rowNames;
  const pane = await mountPane(AGENT_LIST_PANE, fixtures);
  const title = pane.frame.locator(".al-ws-name").first();
  await expect(title).toHaveText("Work 0");
  expect(await title.evaluate((el) => el.scrollWidth <= el.clientWidth)).toBe(true);
  await expect(pane.frame.locator('[data-agent-id="s0"] .al-face img')).toBeVisible();
  await expect(pane.frame.locator('[data-agent-id="s1"] .al-face img')).toHaveAttribute(
    "src",
    "/kit/v1/avatars/builtin-funny/coffee-bot.svg",
  );
  await expect(pane.frame.locator(".al-ws-tail .al-hot").first()).toHaveText("?1");
  const row = (id: string) => pane.frame.locator(`[data-agent-id="${id}"] .al-main`);
  expect((await row("s1").boundingBox())!.y - (await row("s0").boundingBox())!.y).toBe(28);
  expect((await row("s2").boundingBox())!.height).toBe(45);
  const assertNoGap = async () => {
    const ask = (await row("s2").boundingBox())!;
    expect((await row("s3").boundingBox())!.y - ask.y).toBe(ask.height);
  };
  await assertNoGap();
  await pane.page.screenshot({ path: testInfo.outputPath("narrow.png") });
  await pane.upsert("sessions", {
    ...fixtures.sessions![2],
    pendingAsk:
      "Please choose the branch and explain which changes should go into the next release.",
  });
  await expect(pane.frame.locator('[data-agent-id="s2"] .al-ask')).toContainText("Please choose");
  await assertNoGap();
});
