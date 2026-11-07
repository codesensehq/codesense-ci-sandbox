// SPDX-License-Identifier: MIT
import { mkdirSync } from "node:fs";
import { expect, test, type Page } from "@playwright/test";

/**
 * Review captures of moving workspaces, in the frontend mock backend's `?agents=teams` world: a
 * workspace card dragged in the Agents pane and a pill dragged along the tray, mid-drag and after
 * the drop, in both CLI layouts (the CLI pane beside the viewer, and the full-screen CLI). Both
 * places share one order, so each drop is checked in the other place too. The `?nav=demo` world
 * (stories under three epics) shows a workspace moved across its epic's edge: the pane's top card,
 * alone in its tray group, dragged below the next one, and the tray's rules where the epic changes.
 */

const OUT_DIR = process.env.WORKSPACE_REORDER_OUT ?? "test-results/workspace-reorder-review";
test.use({ deviceScaleFactor: 2 });

async function boot(page: Page, layout: "shared" | "console", url = "/?agents=teams") {
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto(url);
  await page.waitForFunction(() => Boolean(window.__stores?.useViewerStore));
  await page.getByTestId("agents-workspace-group").first().waitFor();
  await page.evaluate((layout) => {
    const store = window.__stores!.useViewerStore;
    store.setState({ workspaceTraySeenCoachmark: true });
    store.getState().setLayoutMode(layout);
  }, layout);
  // The cards settle once the viewer's workspaces reach the mock backend's projection; measure
  // them only after two reads, a beat apart, agree.
  const keys = () =>
    page.evaluate(() =>
      [...document.querySelectorAll<HTMLElement>("[data-testid=agents-workspace-group]")]
        .map((g) => g.dataset.groupKey)
        .join(),
    );
  await expect
    .poll(async () => {
      const before = await keys();
      await page.waitForTimeout(700);
      return before === (await keys());
    })
    .toBe(true);
}

const cardOrder = (page: Page, team: string) =>
  page.evaluate(
    (team) =>
      [...document.querySelectorAll<HTMLElement>("[data-testid=agents-workspace-group]")]
        .filter((g) => g.dataset.groupKey?.startsWith(`${team}|`))
        .map((g) => g.dataset.workspaceId),
    team,
  );
const chipOrder = (page: Page) =>
  page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>("[data-tray-chip]")].map((c) => c.dataset.trayChip),
  );

const ABACUS = "abacus/labs:riverside";
const ACME = "acme/widgets:default";

for (const layout of ["shared", "console"] as const) {
  test(`workspace reorder captures — ${layout} CLI layout`, async ({ page }) => {
    mkdirSync(OUT_DIR, { recursive: true });
    await boot(page, layout);

    const paneBox = (await page.locator(".ag-scroll").boundingBox())!;
    const PANE = {
      x: 0,
      y: paneBox.y - 90,
      width: 272,
      height: Math.min(760, 900 - paneBox.y + 90),
    };

    // 1. A card that can move shows its grip on hover.
    const groups = page.getByTestId("agents-workspace-group");
    const sv3 = page.locator(`[data-group-key="${ABACUS}|SV-3"] .ag-ws-head-row`);
    const contentHead = page.locator(`[data-group-key^="${ABACUS}|generic:"] .ag-ws-head-row`);
    await contentHead.hover();
    await page.waitForTimeout(250);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-pane-grip-hover.png`, clip: PANE });

    // 2. Drag the content card above SV-3: mid-drag, with the preview and the line.
    const before = await cardOrder(page, ABACUS);
    const from = (await contentHead.boundingBox())!;
    await page.mouse.move(from.x + 60, from.y + from.height / 2);
    await page.mouse.down();
    await page.mouse.move(from.x + 60, from.y + from.height / 2 - 12, { steps: 4 });
    // The card folds while it moves, so the target is measured again once it has.
    await page.waitForTimeout(250);
    const aim = (await sv3.boundingBox())!;
    await page.mouse.move(from.x + 64, aim.y + 2, { steps: 12 });
    await expect(page.getByTestId("agents-drop-line")).toBeVisible();
    await page.waitForTimeout(200);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-pane-mid-drag.png`, clip: PANE });
    await page.screenshot({ path: `${OUT_DIR}/${layout}-pane-mid-drag-full.png` });
    await page.mouse.up();
    await page.waitForTimeout(120);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-pane-settling.png`, clip: PANE });
    await page.waitForTimeout(700);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-pane-dropped.png`, clip: PANE });
    const after = await cardOrder(page, ABACUS);
    expect(after).toEqual([before[1], before[0]]);
    await expect(groups.first()).toBeVisible();

    // A card held over another team: refused (dimmed preview, no line); Escape puts it back.
    const sv3Box = (await sv3.boundingBox())!;
    const acmeHead = (await page
      .locator(`[data-group-key="${ACME}|BUG-142"] .ag-ws-head-row`)
      .boundingBox())!;
    await page.mouse.move(sv3Box.x + 60, sv3Box.y + sv3Box.height / 2);
    await page.mouse.down();
    await page.mouse.move(sv3Box.x + 60, sv3Box.y + 30, { steps: 4 });
    await page.mouse.move(sv3Box.x + 64, acmeHead.y + 40, { steps: 10 });
    await expect(page.getByTestId("agents-drag-preview")).toHaveAttribute("data-refused");
    await expect(page.getByTestId("agents-drop-line")).toHaveCount(0);
    await page.waitForTimeout(150);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-pane-refused.png`, clip: PANE });
    await page.keyboard.press("Escape");
    await page.mouse.up();
    await expect(page.getByTestId("agents-drag-preview")).toHaveCount(0);
    expect(await cardOrder(page, ABACUS)).toEqual(after);

    // 3. Acme's tray: drag the "content" pill to the front, along the tray.
    const chips = await chipOrder(page);
    const last = chips.findIndex((id) => id?.endsWith("/content"));
    const pill = page.locator(`[data-tray-chip="${chips[last]}"]`);
    const first = page.locator(`[data-tray-chip="${chips[0]}"]`);
    const pb = (await pill.boundingBox())!;
    const fb = (await first.boundingBox())!;
    await page.mouse.move(pb.x + pb.width / 2, pb.y + pb.height / 2);
    await page.mouse.down();
    await page.mouse.move(pb.x + pb.width / 2 - 12, pb.y + pb.height / 2, { steps: 4 });
    await page.mouse.move(fb.x + 8, fb.y + fb.height / 2, { steps: 14 });
    await expect(page.getByTestId("workspace-tray-insertion")).toBeVisible();
    await page.waitForTimeout(200);
    const trayClip =
      layout === "shared"
        ? { x: 0, y: 820, width: 1000, height: 80 }
        : { x: 0, y: 0, width: 1000, height: 80 };
    await page.screenshot({ path: `${OUT_DIR}/${layout}-tray-mid-drag.png`, clip: trayClip });
    await page.screenshot({ path: `${OUT_DIR}/${layout}-tray-mid-drag-full.png` });
    await page.mouse.up();
    await page.waitForTimeout(500);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-tray-dropped.png`, clip: trayClip });
    const chipsAfter = await chipOrder(page);
    expect(chipsAfter[0]).toBe(chips[last]);
    // The pane follows the tray.
    const acmeCards = await cardOrder(page, ACME);
    expect(acmeCards.indexOf(chips[last]!)).toBe(0);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-after-both.png` });
  });
}

/** The strip's pills in order, with "|" for each epic rule. */
const stripSequence = (page: Page) =>
  page.evaluate(() => {
    const strip = document.querySelector("[data-tray-chip]")!.parentElement!;
    return [...strip.children]
      .map((c) =>
        c.hasAttribute("data-tray-divider") ? "|" : ((c as HTMLElement).dataset.trayChip ?? null),
      )
      .filter((x): x is string => x !== null);
  });

/** A rule wherever the epic changes and nowhere else (never two in a row, never at an end). */
async function expectRulesAtEpicChanges(page: Page, team: string) {
  const seq = await stripSequence(page);
  const rails = await page.evaluate(
    (team) => window.__stores!.useViewerStore.getState().railKeyByWorkspaceId[team] ?? {},
    team,
  );
  const epic = (id: string) => (id.startsWith("generic:") ? "" : (rails[id] ?? ""));
  const pills = seq.filter((x) => x !== "|");
  const expected: string[] = [];
  pills.forEach((id, i) => {
    if (i > 0 && epic(pills[i - 1]!) !== epic(id)) expected.push("|");
    expected.push(id);
  });
  expect(seq).toEqual(expected);
}

for (const layout of ["shared", "console"] as const) {
  test(`moving a workspace across its epic's edge — ${layout} CLI layout`, async ({ page }) => {
    mkdirSync(OUT_DIR, { recursive: true });
    await boot(page, layout, "/?nav=demo");
    const paneBox = (await page.locator(".ag-scroll").boundingBox())!;
    const PANE = {
      x: 0,
      y: paneBox.y - 90,
      width: 272,
      height: Math.min(760, 900 - paneBox.y + 90),
    };
    const trayClip =
      layout === "shared"
        ? { x: 0, y: 820, width: 1440, height: 80 }
        : { x: 0, y: 0, width: 1440, height: 80 };
    await page.screenshot({ path: `${OUT_DIR}/${layout}-epics-before.png` });

    // 1. The pane's top card dragged below the first card of another epic.
    await page.waitForTimeout(1200);
    const before = (await cardOrder(page, ACME)).filter((id) => id !== "");
    const top = before[0]!;
    const rails = await page.evaluate(
      (team) => window.__stores!.useViewerStore.getState().railKeyByWorkspaceId[team] ?? {},
      ACME,
    );
    const past = before.findIndex((id) => rails[id] !== rails[top]);
    expect(past).toBeGreaterThan(0);
    const headOf = (id: string) => page.locator(`[data-group-key="${ACME}|${id}"] .ag-ws-head-row`);
    const from = (await headOf(top).boundingBox())!;
    await page.mouse.move(from.x + 60, from.y + from.height / 2);
    await page.mouse.down();
    await page.mouse.move(from.x + 60, from.y + from.height / 2 + 12, { steps: 4 });
    // Folded while it moves: aim just inside the top of the card after that one.
    await page.waitForTimeout(250);
    // The pane is short: a target near its bottom edge would make the list scroll away under the
    // pointer, so wheel it to the middle first (the drag follows the scroll).
    const toMiddle = await page.locator(".ag-scroll").evaluate(
      (scroller, selector) => {
        const head = document.querySelector(selector)!.getBoundingClientRect();
        const pane = scroller.getBoundingClientRect();
        return head.top - (pane.top + pane.height / 2);
      },
      `[data-group-key="${ACME}|${before[past + 1]}"] .ag-ws-head-row`,
    );
    await page.mouse.wheel(0, toMiddle);
    await page.waitForTimeout(150);
    const aim = (await headOf(before[past + 1] ?? "").boundingBox())!;
    await page.mouse.move(from.x + 64, aim.y + 2, { steps: 12 });
    await expect(page.getByTestId("agents-drop-line")).toBeVisible();
    await page.waitForTimeout(200);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-epics-pane-top-mid-drag.png`, clip: PANE });
    await page.mouse.up();
    await page.waitForTimeout(800);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-epics-pane-top-dropped.png`, clip: PANE });
    const moved = [...before.slice(1, past + 1), top, ...before.slice(past + 1)];
    expect((await cardOrder(page, ACME)).filter((id) => id !== "")).toEqual(moved);
    // The tray follows: the pill sits past the other epic's pill, set off by its own rules.
    const chips = await chipOrder(page);
    expect(chips.indexOf(top)).toBeGreaterThan(chips.indexOf(before[past]!));
    await expectRulesAtEpicChanges(page, ACME);
    await page.screenshot({
      path: `${OUT_DIR}/${layout}-epics-tray-after-pane.png`,
      clip: trayClip,
    });

    // 2. The tray's first pill dragged past the next epic's first pill.
    const firstId = chips[0]!;
    await page.locator(`[data-tray-chip="${firstId}"]`).scrollIntoViewIfNeeded();
    await page.waitForTimeout(150);
    const nextEpic = chips.findIndex((id) => rails[id] !== rails[firstId]);
    const [left, right] = [chips[nextEpic]!, chips[nextEpic + 1]!];
    const pill = page.locator(`[data-tray-chip="${firstId}"]`);
    const pb = (await pill.boundingBox())!;
    await page.mouse.move(pb.x + pb.width / 2, pb.y + pb.height / 2);
    await page.mouse.down();
    await page.mouse.move(pb.x + pb.width / 2 + 12, pb.y + pb.height / 2, { steps: 4 });
    // The strip may scroll as the drag starts: read the target where it is now.
    await page.waitForTimeout(250);
    const rb = (await page.locator(`[data-tray-chip="${right}"]`).boundingBox())!;
    await page.mouse.move(rb.x + 6, rb.y + rb.height / 2, { steps: 16 });
    await expect(page.getByTestId("workspace-tray-insertion")).toBeVisible();
    await page.waitForTimeout(200);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-epics-tray-mid-drag.png`, clip: trayClip });
    await page.mouse.up();
    await page.waitForTimeout(500);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-epics-tray-dropped.png`, clip: trayClip });
    const after = await chipOrder(page);
    expect(after.indexOf(firstId)).toBe(after.indexOf(left) + 1);
    expect(after.indexOf(firstId)).toBe(after.indexOf(right) - 1);
    await expectRulesAtEpicChanges(page, ACME);
    // Cmd+1..9 numbers the pills as shown.
    const order = await page.evaluate(
      (team) => window.__stores!.useViewerStore.getState().trayOrderByTeam[team],
      ACME,
    );
    expect(order).toEqual(after);

    // 3. Grouped by epic: the selected epic's pills keep the person's order.
    const inbox = after.filter((id) => rails[id] === "26-agent-inbox");
    await page.evaluate(
      ([team, focus]) => {
        const store = window.__stores!.useViewerStore;
        store.setState((st) => ({
          trayMode: { ...st.trayMode, [team]: "grouped" },
          activeWorkspace: { ...st.activeWorkspace, [team]: focus },
        }));
      },
      [ACME, inbox[0]!] as const,
    );
    await page.waitForTimeout(400);
    expect(await chipOrder(page)).toEqual(inbox);
    await page.screenshot({ path: `${OUT_DIR}/${layout}-epics-grouped.png` });
    await page.screenshot({
      path: `${OUT_DIR}/${layout}-epics-grouped-tray.png`,
      clip: trayClip,
    });
  });
}
