// SPDX-License-Identifier: MIT
import { mkdirSync } from "node:fs";
import { agentListCaptureFixtures } from "../pane-harness/agent-list-fixtures";
import { expect, test } from "@playwright/test";

/**
 * Review captures of the SV-3 Agents pane (the left agent list) in the frontend
 * mock backend's `?agents=teams` world (two open teams in two orgs). Writes PNGs for
 * design review; asserts only that the pane rendered both teams, with agents and workspace cards.
 */

// Where the PNGs go: AGENTS_REVIEW_OUT, or Playwright's own output folder.
const OUT_DIR = process.env.AGENTS_REVIEW_OUT ?? "test-results/agents-pane-review";

// Sharp captures, so a reviewer can read 1px rules and small type.
test.use({ deviceScaleFactor: 2 });

test("agents pane review captures", async ({ page }) => {
  mkdirSync(OUT_DIR, { recursive: true });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/?agents=teams");

  const workspaceGroups = page.getByTestId("agents-workspace-group");
  await expect(workspaceGroups.first()).toBeVisible();
  const rows = page.locator(".ag-row:not(.ag-ghost)");
  await expect(rows.first()).toBeVisible();
  expect(await rows.count()).toBeGreaterThanOrEqual(3);
  expect(await workspaceGroups.count()).toBeGreaterThanOrEqual(2);
  const teamGroups = page.getByTestId("agents-team-group");
  await expect(teamGroups.nth(1)).toBeVisible();
  expect(await teamGroups.count()).toBeGreaterThanOrEqual(2);

  // 1. As the pane opens.
  await page.mouse.move(0, 0);
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${OUT_DIR}/agents-pane-default.png` });

  // 2. Hover one agent row (a nested row, so the hover actions show).
  const hoverRow = page.locator(".ag-row:not(.ag-ghost)").nth(1);
  await hoverRow.hover();
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${OUT_DIR}/agents-pane-hover-row.png` });
  await page.mouse.move(0, 0);

  // 3. Fold a workspace card by clicking its body (the head row, away from its name).
  // Not the BUG-142 card: a folded card renders none of its rows, and the chat captures below
  // need the BUG-142 lead and repro rows. The card order also varies with load, so pick by id.
  const card = page.locator('[data-testid="agents-workspace-group"][data-workspace-id="BUG-151"]');
  await expect(card).toBeVisible();
  const head = card.locator(".ag-ws-head-row");
  const box = await head.boundingBox();
  if (!box) throw new Error("workspace card head has no box");
  await head.click({ position: { x: 6, y: box.height / 2 } });
  await expect(card).toHaveAttribute("data-collapsed", "true");
  await page.mouse.move(0, 0);
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${OUT_DIR}/agents-pane-folded-workspace.png` });

  // 4. Filter button menu open. The filter icon only shows while the search field is hovered or focused.
  await page.getByTestId("agents-filter-field").hover();
  await page.getByRole("button", { name: "Filter and arrange", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Filter and arrange agents" })).toBeVisible();
  await page.waitForTimeout(300);
  await page.screenshot({ path: `${OUT_DIR}/agents-pane-filter-menu.png` });

  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog", { name: "Filter and arrange agents" })).toBeHidden();

  // 5. The inbox chat: the login lead's conversation with you, from its row's chat icon.
  const leadRow = page.locator('.ag-row[data-agent-handle="BUG-142#lead"]').first();
  await leadRow.hover();
  await leadRow.getByTestId("agent-row-message").click();
  const chat = page.getByTestId("agent-inbox-chat");
  await expect(chat).toBeVisible();
  await expect(chat.getByRole("textbox", { name: "Message" })).toBeFocused();
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${OUT_DIR}/agents-chat-conversation.png` });
  await chat.screenshot({ path: `${OUT_DIR}/agents-chat-conversation-close.png` });
  // Scrolled back to the start: your message and the agent's report.
  await chat.getByTestId("ibx-msgs").evaluate((el) => el.scrollTo(0, 0));
  await page.waitForTimeout(200);
  await chat.screenshot({ path: `${OUT_DIR}/agents-chat-top.png` });

  // 6. A long draft, several lines, in the composer.
  const draftBox = chat.getByRole("textbox", { name: "Message" });
  await draftBox.fill(
    "Bump staging's TTL to match production first, then rerun the idle repro.\nIf it still fails after 15 minutes, page me before you touch the retry logic again.\nAlso: keep the trace on so we can compare against yesterday's run.",
  );
  await page.waitForTimeout(200);
  await chat.screenshot({ path: `${OUT_DIR}/agents-chat-draft.png` });
  await draftBox.fill("");
  await page.keyboard.press("Escape");
  await expect(chat).toBeHidden();

  // 7. An agent with nothing exchanged yet: the empty state.
  const qaRow = page.locator('.ag-row[data-agent-handle="BUG-142#repro"]').first();
  await qaRow.hover();
  await qaRow.getByTestId("agent-row-message").click();
  await expect(chat.getByText("No messages yet")).toBeVisible();
  await page.waitForTimeout(300);
  await chat.screenshot({ path: `${OUT_DIR}/agents-chat-empty.png` });
  await page.keyboard.press("Escape");

  // 8. The quick view, for the family resemblance.
  await leadRow.hover();
  await leadRow.getByTestId("agent-row-cli").click();
  await expect(page.getByTestId("agent-quick-send")).toBeVisible();
  await page.waitForTimeout(400);
  await page.screenshot({ path: `${OUT_DIR}/agents-quick-view.png` });
  await page.keyboard.press("Escape");

  // 9. A message sent from the chat: it lands at the foot of the conversation.
  await leadRow.hover();
  await leadRow.getByTestId("agent-row-message").click();
  const sendBox = page.getByTestId("agent-inbox-chat").getByRole("textbox", { name: "Message" });
  await sendBox.fill("Yes, bump staging's TTL to match production, then rerun the idle repro.");
  await sendBox.press("Enter");
  await expect(sendBox).toHaveValue("");
  await page.waitForTimeout(500);
  await page.screenshot({ path: `${OUT_DIR}/agents-chat-sent.png` });

  // 10. Dragged down to its smallest: the header and composer still fit.
  const grip = page.getByTestId("agent-inbox-chat-grip");
  const g = await grip.boundingBox();
  if (!g) throw new Error("chat grip has no box");
  await page.mouse.move(g.x + g.width / 2, g.y + g.height / 2);
  await page.mouse.down();
  await page.mouse.move(g.x - 400, g.y - 400, { steps: 8 });
  await page.mouse.up();
  await page.waitForTimeout(200);
  await page
    .getByTestId("agent-inbox-chat")
    .screenshot({ path: `${OUT_DIR}/agents-chat-small.png` });
});

test("agents pane review captures: subagents", async ({ page }) => {
  mkdirSync(OUT_DIR, { recursive: true });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/?agents=teams");
  const pane = page.locator(".cs-agents").first();

  // The agents whose subagents run show the mark beside their status word, its words its name.
  const lead = page.locator('.ag-row[data-agent-handle="SV-3#lead"]').first();
  await expect(lead.getByTestId("agent-subagents")).toHaveAttribute("data-count", "3");
  await expect(lead.getByRole("img", { name: "3 subagents running" })).toBeVisible();
  await page.mouse.move(0, 0);
  await page.waitForTimeout(300);
  await pane.screenshot({ path: `${OUT_DIR}/subagents-pane.png` });

  // Hovered: the row's actions take the end of line one, and the mark still shows.
  await lead.hover();
  await page.waitForTimeout(250);
  await lead.screenshot({ path: `${OUT_DIR}/subagents-row-hover.png` });
  await page.mouse.move(0, 0);

  // Arranged by status: the rows keep their marks under Working.
  await page.getByTestId("agents-filter-field").hover();
  await page.getByRole("button", { name: "Filter and arrange", exact: true }).click();
  await page.getByRole("radio", { name: "By status", exact: true }).click();
  await page.keyboard.press("Escape");
  const section = (id: string) => page.locator(`[data-status-section="${id}"]`);
  await expect(page.locator(".ag-status-head").first()).toBeVisible();
  await expect(
    section("needs").locator('.ag-row[data-agent-handle="BUG-142#qa"]').first(),
  ).toBeVisible();
  await expect(
    section("working").locator('.ag-row[data-agent-handle="SV-3#lead"]').first(),
  ).toBeVisible();
  await expect(section("ready").first()).toBeVisible();
  await expect(section("working").locator('.ag-row[data-agent-handle="BUG-142#qa"]')).toHaveCount(
    0,
  );
  await page.mouse.move(0, 0);
  await page.waitForTimeout(300);
  await pane.screenshot({ path: `${OUT_DIR}/subagents-by-status.png` });
  await page.locator('.ag-chip[data-chip="arrange"]').getByRole("button").click();

  // The quick view shares the row's compact subagent mark.
  const leadRow = page.locator('.ag-row[data-agent-handle="SV-3#lead"]').first();
  await leadRow.hover();
  await leadRow.getByTestId("agent-row-cli").click();
  const quick = page.getByTestId("agent-quick-send");
  await expect(quick.locator(".ag-quick-send-meta")).toContainText("Working");
  await expect(quick.getByRole("img", { name: "3 subagents running" })).toBeVisible();
  await page.waitForTimeout(300);
  await quick
    .locator(".ag-quick-send-head")
    .screenshot({ path: `${OUT_DIR}/subagents-quick-view-head.png` });
  await page.keyboard.press("Escape");

  // The inbox chat's header too.
  await leadRow.hover();
  await leadRow.getByTestId("agent-row-message").click();
  const chat = page.getByTestId("agent-inbox-chat");
  await expect(chat.locator(".ag-quick-send-meta")).toContainText("Working · 3 subagents");
  await page.waitForTimeout(300);
  await chat
    .locator(".ag-quick-send-head")
    .screenshot({ path: `${OUT_DIR}/subagents-chat-head.png` });
  await page.keyboard.press("Escape");

  // A light theme: the mark takes the status word's ink there too.
  await page.evaluate(() => {
    window.__stores!.useViewerStore.getState().updateSettings({ viewerTheme: "confluence" });
  });
  await page.mouse.move(0, 0);
  await page.waitForTimeout(500);
  await pane.screenshot({ path: `${OUT_DIR}/subagents-pane-light.png` });
});

test("agents pane review: Status arrangement puts unread mail first within a group", async ({
  page,
}) => {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/?agents=teams");
  await page.getByTestId("agents-filter-field").hover();
  await page.getByRole("button", { name: "Filter and arrange", exact: true }).click();
  await page.getByRole("radio", { name: "By status", exact: true }).click();
  await page.keyboard.press("Escape");
  const working = page.locator('[data-status-section="working"]');
  const order = () =>
    working
      .locator('.ag-row[data-agent-handle^="BUG-142#"]')
      .evaluateAll((rows) => rows.map((row) => row.getAttribute("data-agent-handle")));
  await expect(working.locator('.ag-row[data-agent-handle="BUG-142#dev"]')).toBeVisible();
  const before = await order();
  expect(before.indexOf("BUG-142#dev")).toBeLessThan(before.indexOf("BUG-142#lead"));

  // Mail to the lead goes through the inbox write; the view re-ranks on the change event.
  await page.evaluate(() =>
    (
      window as unknown as {
        __TAURI_INTERNALS__: { invoke: (cmd: string, args: unknown) => Promise<unknown> };
      }
    ).__TAURI_INTERNALS__.invoke("send_inbox_as_human", {
      to: "BUG-142#lead",
      text: "status check",
    }),
  );
  await expect
    .poll(async () => {
      const after = await order();
      return after.indexOf("BUG-142#lead") < after.indexOf("BUG-142#dev");
    })
    .toBe(true);
});

test("agents pane review captures: team funnel", async ({ page }) => {
  mkdirSync(OUT_DIR, { recursive: true });
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/?agents=teams");
  const pane = page.locator(".cs-agents").first();
  const teams = page.getByTestId("agents-team-group");
  await expect(teams.nth(1)).toBeVisible();

  // Hovering a team header swaps its chevron for the funnel.
  const riverside = page.locator('[data-testid="agents-team-group"][data-team-key*="riverside"]');
  await riverside.locator(".ag-team-head").hover();
  await page.waitForTimeout(250);
  await pane.screenshot({ path: `${OUT_DIR}/team-funnel-hover.png` });

  // On: only riverside shows, the funnel stays lit and the find field names the team.
  const funnel = riverside.getByTestId("agents-team-funnel");
  await funnel.click();
  await expect(funnel).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator('.ag-chip[data-chip="team"]')).toHaveText("Team: riverside");
  await expect(teams).toHaveCount(1);
  await page.mouse.move(0, 0);
  await page.waitForTimeout(250);
  await pane.screenshot({ path: `${OUT_DIR}/team-funnel-on.png` });

  // The filter menu agrees: only riverside is checked.
  await page.getByTestId("agents-filter-field").hover();
  await page.getByRole("button", { name: "Filter and arrange", exact: true }).click();
  const menu = page.getByRole("dialog", { name: "Filter and arrange agents" });
  await expect(menu.getByRole("checkbox", { name: /riverside/ })).toBeChecked();
  await expect(menu.getByRole("checkbox", { name: /default/ })).not.toBeChecked();
  await page.waitForTimeout(250);
  await pane.screenshot({ path: `${OUT_DIR}/team-funnel-menu.png` });
  await page.keyboard.press("Escape");

  // Clearing the chip shows the other team again.
  await page.getByRole("button", { name: "Show the other teams" }).click();
  await expect(teams).toHaveCount(2);

  // From the keyboard: Tab from the team header reaches the funnel, Enter turns it on, Enter
  // again turns it off.
  await riverside.locator(".ag-team-head").focus();
  await page.keyboard.press("Tab");
  await expect(funnel).toBeFocused();
  await page.waitForTimeout(250);
  await pane.screenshot({ path: `${OUT_DIR}/team-funnel-focus.png` });
  await page.keyboard.press("Enter");
  await expect(funnel).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.press("Enter");
  await expect(funnel).toHaveAttribute("aria-pressed", "false");
  await expect(teams).toHaveCount(2);
});

for (const scheme of ["light", "dark"] as const) {
  test(`parity native 236px ${scheme}`, async ({ page }) => {
    const out = process.env.AGENTS_PARITY_OUT ?? "test-results/agents-pane-parity";
    mkdirSync(out, { recursive: true });
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.addInitScript((t) => localStorage.setItem("theme", t), scheme);
    await page.goto("/?agents=teams");
    await page.waitForFunction(() => !!window.__stores?.useViewerStore);
    await expect(page.locator(".cs-agents").first()).toBeVisible();
    const fixtures = agentListCaptureFixtures();
    await page.evaluate(
      async ({ fixtures, scheme }) => {
        const { liveHub } = await new Function(
          "return import('/src/features/dashboard/lib/live-hub.ts')",
        )();
        const { useAgentListProjectionStore } = await new Function(
          "return import('/src/features/viewer/lib/agent-list-projection.ts')",
        )();
        const viewer = window.__stores!.useViewerStore;
        const workspaces = {
          "fixture:dev": [
            {
              id: "ws0",
              kind: "story",
              storyId: "Work 0",
              storyTitle: "Work 0",
              storyStatus: "in-progress",
              sourceRepoSlug: "fixture/repo",
              terminalTabs: [],
            },
          ],
        };
        viewer.setState({
          workspacePanelWidth: 236,
          agentsPaneArrange: "teams",
          agentsPaneFolds: {},
          agentsPaneHiddenTeams: [],
          agentsPaneSearch: { query: "", status: "all" },
          agentPins: {},
          workspaces,
        });
        viewer.getState().updateSettings({ viewerTheme: "codesense" });
        document.documentElement.classList.toggle("dark", scheme === "dark");
        const sessions = fixtures.sessions!.map((s) => ({
          ...s,
          displayName: (s.rowNames as { primary: string }).primary,
          launchTeamKey: "fixture:dev",
          startedAtMs: null,
        }));
        useAgentListProjectionStore.setState({
          sessions,
          references: fixtures.topics!.agentList.filter((r) => r.sessionRef),
          teamOrder: ["fixture:dev"],
          partial: false,
        });
        for (const [topic, rows] of Object.entries({
          sessions,
          inbox: fixtures.inbox,
          ...fixtures.topics,
        }))
          liveHub.seed({ topic, viewKey: "all", rev: 10, snapshot: rows });
      },
      { fixtures, scheme },
    );
    const pane = page.locator(".cs-agents").first();
    await expect(pane.locator(".ag-row:not(.ag-ghost)").first()).toBeVisible();
    expect(Math.round((await pane.boundingBox())!.width)).toBe(236);
    await page.mouse.move(1400, 880);
    const capture = async (state: string) => {
      // Playwright hover can scroll a horizontal ancestor to reveal row overflow.
      await pane.evaluate((el) => {
        if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
        for (let parent = el.parentElement; parent; parent = parent.parentElement)
          parent.scrollLeft = 0;
      });
      const bounds = (await pane.boundingBox())!;
      expect(Math.round(bounds.width)).toBe(236);
      expect(bounds.x).toBeGreaterThanOrEqual(0);
      await page.screenshot({
        animations: "disabled",
        path: `${out}/native-${scheme}-${state}.png`,
        clip: { ...bounds, width: 236, height: 700 },
      });
    };
    await capture("default");
    await pane.locator(".ag-row:not(.ag-ghost)").first().hover();
    await capture("hover");
    await pane
      .locator(".ag-ws-head-row")
      .first()
      .click({ position: { x: 6, y: 14 } });
    await expect(pane.getByTestId("agents-workspace-group").first()).toHaveAttribute(
      "data-collapsed",
      "true",
    );
    await page.mouse.move(1400, 880);
    await capture("folded");
  });
}
